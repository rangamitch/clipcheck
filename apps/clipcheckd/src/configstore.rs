//! Config file store for the daemon (doc 09, sections 5.2, 5.3 and 5.6).
//!
//! Pure helpers first: [`merge_patch`] applies a JSON patch to the serialized
//! [`Config`], [`parse_config`] turns the merged value back into a typed config
//! and names the failing key when it cannot. [`ConfigStore`] owns the file path
//! and writes the file atomically with one `.bak` backup.
//!
//! Comments in a hand-edited file are not kept on save. The file is rewritten
//! from the typed config (doc 09 section 5.3 asks for comment-preserving edits;
//! that needs `toml_edit`, which is not a dependency of this crate yet).

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use clipcheck_core::config::schema::{read_config_file, Config, ConfigError};
use clipcheck_core::config::validate::{validate, ApplyClass, Diagnostic, Severity};
use serde_json::{Map, Value};

use crate::logging;

/// Name of the config file inside the data directory.
pub const CONFIG_FILE_NAME: &str = "config.toml";

/// Most `config.toml.broken-<unix_ms>` copies kept in the data directory.
pub const MAX_BROKEN_BACKUPS: usize = 5;

/// Top-level table whose keys are free-form game ids and may be created by a patch.
const GAMES_TABLE: &str = "games";

/// A patch that cannot be applied. `path` is the dotted key that caused it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchError {
    /// Dotted key path, empty for the whole document.
    pub path: String,
    /// Human-readable detail.
    pub message: String,
}

impl PatchError {
    fn new(path: &str, message: impl Into<String>) -> Self {
        Self {
            path: path.to_owned(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for PatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.path.is_empty() {
            f.write_str(&self.message)
        } else {
            write!(f, "{}: {}", self.path, self.message)
        }
    }
}

/// Deep-merges `patch` into `base`.
///
/// Rules: objects merge key by key, arrays and scalars replace, `null` sets the
/// key to null (which clears an optional field), and a `null` under `games`
/// removes that game. A key that does not exist in `base` is an error, except
/// under `games`, where a new game table may be added.
pub fn merge_patch(base: &mut Value, patch: &Value) -> Result<(), PatchError> {
    let Some(patch) = patch.as_object() else {
        return Err(PatchError::new("", "patch must be a JSON object"));
    };
    merge_object(base, patch, "", false)
}

/// Merges one object level. `allow_new` is true only for the `games` table.
fn merge_object(
    base: &mut Value,
    patch: &Map<String, Value>,
    path: &str,
    allow_new: bool,
) -> Result<(), PatchError> {
    let Some(base_obj) = base.as_object_mut() else {
        // The base value is null or a scalar (an absent optional table). The patch
        // object becomes the value; `parse_config` checks its fields.
        *base = Value::Object(patch.clone());
        return Ok(());
    };
    for (key, value) in patch {
        let child_path = join_path(path, key);
        if base_obj.contains_key(key) {
            if value.is_null() && allow_new {
                base_obj.remove(key);
                continue;
            }
            let child = base_obj
                .get_mut(key)
                .expect("key present: checked by contains_key above");
            if let (Some(child_patch), true) = (value.as_object(), child.is_object()) {
                // Entries of `games` are fixed-shape tables, so they do not allow new keys.
                let child_allows_new = child_path == GAMES_TABLE;
                merge_object(child, child_patch, &child_path, child_allows_new)?;
            } else {
                *child = value.clone();
            }
        } else if allow_new {
            if !value.is_null() {
                base_obj.insert(key.clone(), value.clone());
            }
        } else {
            return Err(PatchError::new(&child_path, "unknown key"));
        }
    }
    Ok(())
}

fn join_path(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_owned()
    } else {
        format!("{path}.{key}")
    }
}

/// Applies `patch` to `current` and returns the resulting typed config.
///
/// Fails with the key path when the patch names an unknown key or when the merged
/// document no longer deserializes (for example a string where a number belongs).
/// Validation rules are not checked here; run [`validate`] on the result.
///
/// [`validate`]: clipcheck_core::config::validate::validate
pub fn apply_patch(current: &Config, patch: &Value) -> Result<Config, PatchError> {
    let base = to_value(current)?;
    let mut merged = base.clone();
    merge_patch(&mut merged, patch)?;
    parse_config(&base, &merged)
}

/// Turns `merged` into a [`Config`]. `base` is the valid document it was built from.
///
/// On failure the error names the deepest key whose change alone makes the document
/// fail, found by replacing keys of `merged` into `base` one level at a time.
pub fn parse_config(base: &Value, merged: &Value) -> Result<Config, PatchError> {
    match serde_json::from_value::<Config>(merged.clone()) {
        Ok(config) => Ok(config),
        Err(e) => {
            let path = culprit_path(base, merged);
            let shown = if path.is_empty() {
                "config".to_owned()
            } else {
                path.clone()
            };
            Err(PatchError {
                path,
                message: format!("{shown}: {e}"),
            })
        }
    }
}

/// Finds the dotted path of the key whose change makes `merged` fail to load.
fn culprit_path(base: &Value, merged: &Value) -> String {
    let mut path: Vec<String> = Vec::new();
    while let Some(node) = value_at(merged, &path).and_then(Value::as_object) {
        let base_node = value_at(base, &path);
        let mut descended = false;
        for (key, value) in node {
            if base_node.and_then(|b| b.get(key)) == Some(value) {
                continue;
            }
            path.push(key.clone());
            let mut trial = base.clone();
            let fails = set_at(&mut trial, &path, value.clone())
                && serde_json::from_value::<Config>(trial).is_err();
            if fails {
                descended = true;
                break;
            }
            path.pop();
        }
        if !descended {
            break;
        }
    }
    path.join(".")
}

fn value_at<'a>(root: &'a Value, path: &[String]) -> Option<&'a Value> {
    path.iter().try_fold(root, |node, key| node.get(key))
}

/// Replaces the value at `path` inside `root`. Returns false when a parent is missing.
fn set_at(root: &mut Value, path: &[String], value: Value) -> bool {
    let Some((last, parents)) = path.split_last() else {
        *root = value;
        return true;
    };
    let mut node = root;
    for key in parents {
        match node.get_mut(key) {
            Some(next) => node = next,
            None => return false,
        }
    }
    match node.as_object_mut() {
        Some(obj) => {
            obj.insert(last.clone(), value);
            true
        }
        None => false,
    }
}

/// Serializes a config to a JSON value.
pub fn to_value(config: &Config) -> Result<Value, PatchError> {
    serde_json::to_value(config)
        .map_err(|e| PatchError::new("", format!("cannot serialize config: {e}")))
}

/// Parses config file text. Syntax and type errors come back as `CFG-P001`.
pub fn parse_text(text: &str) -> Result<Config, Vec<Diagnostic>> {
    Config::from_toml_str(text).map_err(|e| {
        vec![Diagnostic {
            code: "CFG-P001",
            severity: Severity::Error,
            path: String::new(),
            message: format!("config file is not valid TOML: {e}"),
        }]
    })
}

/// Parses config file text like [`parse_text`], and also returns the `CFG-P004`
/// unknown-key warnings. Syntax and type errors still come back as `CFG-P001`.
pub fn parse_text_checked(text: &str) -> Result<(Config, Vec<Diagnostic>), Vec<Diagnostic>> {
    Config::from_toml_str_checked(text).map_err(|e| {
        vec![Diagnostic {
            code: "CFG-P001",
            severity: Severity::Error,
            path: String::new(),
            message: format!("config file is not valid TOML: {e}"),
        }]
    })
}

/// Stable name of an apply class, as sent in IPC replies.
pub fn class_name(class: ApplyClass) -> &'static str {
    match class {
        ApplyClass::Live => "live",
        ApplyClass::NextPart => "next_part",
        ApplyClass::PipelineReinit => "pipeline_reinit",
        ApplyClass::AudioReinit => "audio_reinit",
        ApplyClass::ReplayFlush => "replay_flush",
        ApplyClass::Restart => "restart",
    }
}

/// True when any diagnostic is an error.
pub fn has_errors(diagnostics: &[Diagnostic]) -> bool {
    diagnostics.iter().any(|d| d.severity == Severity::Error)
}

/// Writes `text` to `path` through a temporary file, keeping one backup.
///
/// Order: write and flush `path.tmp`; copy the current file to `path.bak`; rename
/// `path.tmp` over `path`. A crash before the rename leaves `path` intact and a stray
/// `.tmp` file, which the loader never reads. Line endings follow the existing file
/// (CRLF or LF); new files use CRLF.
pub fn save_atomic(path: &Path, text: &str) -> io::Result<()> {
    let existing = match fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    let crlf = existing
        .as_deref()
        .is_none_or(|bytes| bytes.windows(2).any(|w| w == b"\r\n"));
    let body = if crlf {
        text.replace("\r\n", "\n").replace('\n', "\r\n")
    } else {
        text.replace("\r\n", "\n")
    };

    let tmp = with_suffix(path, "tmp");
    {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(body.as_bytes())?;
        file.sync_all()?;
    }
    if existing.is_some() {
        fs::copy(path, with_suffix(path, "bak"))?;
    }
    fs::rename(&tmp, path)
}

/// `path` with `.suffix` appended to the full file name (`config.toml` -> `config.toml.tmp`).
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".");
    name.push(suffix);
    PathBuf::from(name)
}

/// True when the file at `path` exists but does not load: unreadable, not valid
/// TOML, or with validation errors. A missing file is not broken.
pub fn file_is_broken(path: &Path) -> bool {
    match read_config_file(path) {
        Ok(text) => match Config::from_toml_str(&text) {
            Ok(config) => has_errors(&validate(&config)),
            Err(_) => true,
        },
        Err(ConfigError::Io(e)) => e.kind() != io::ErrorKind::NotFound,
        Err(_) => true,
    }
}

/// Copies the broken file at `path` to `config.toml.broken-<unix_ms>` and keeps
/// only the newest [`MAX_BROKEN_BACKUPS`] such copies. Returns the new copy's
/// path. An error here means nothing was copied, so the caller must not overwrite `path`.
pub fn backup_broken(path: &Path) -> io::Result<PathBuf> {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let dest = with_suffix(path, &format!("broken-{ms}"));
    fs::copy(path, &dest)?;
    if let Err(e) = prune_broken(path) {
        logging::warn(&format!("cannot prune old broken config copies: {e}"));
    }
    Ok(dest)
}

/// Deletes the oldest `config.toml.broken-<ms>` files beyond [`MAX_BROKEN_BACKUPS`].
fn prune_broken(path: &Path) -> io::Result<()> {
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
        return Ok(());
    };
    let prefix = format!("{file_name}.broken-");
    let mut copies: Vec<(u128, PathBuf)> = fs::read_dir(dir)?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let p = entry.path();
            let ms = p
                .file_name()?
                .to_str()?
                .strip_prefix(&prefix)?
                .parse::<u128>()
                .ok()?;
            Some((ms, p))
        })
        .collect();
    copies.sort();
    let excess = copies.len().saturating_sub(MAX_BROKEN_BACKUPS);
    for (_, old) in copies.into_iter().take(excess) {
        fs::remove_file(old)?;
    }
    Ok(())
}

/// The config file on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    /// Store for the file at `path`.
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Path of the config file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads and parses the file. Syntax errors come back as diagnostics.
    pub fn read(&self) -> Result<Config, Vec<Diagnostic>> {
        self.read_checked().map(|(config, _)| config)
    }

    /// Reads and parses the file, also returning the `CFG-P004` unknown-key warnings.
    /// Read and syntax failures come back as diagnostics.
    pub fn read_checked(&self) -> Result<(Config, Vec<Diagnostic>), Vec<Diagnostic>> {
        let text = read_config_file(&self.path).map_err(|e| {
            vec![Diagnostic {
                code: e.diagnostic_code().unwrap_or("CFG-P001"),
                severity: Severity::Error,
                path: String::new(),
                message: format!("cannot read {}: {e}", self.path.display()),
            }]
        })?;
        parse_text_checked(&text)
    }

    /// Reads the file and validates it. This is the one decision shared by startup
    /// and reload: the returned config is `Some` only when the file loads and has no
    /// error diagnostics. Unknown-key warnings come first, then validation output, in
    /// order. Read and syntax failures come back as `CFG-P001` errors with no config.
    pub fn load_checked(&self) -> (Option<Config>, Vec<Diagnostic>) {
        match self.read_checked() {
            Ok((config, mut diagnostics)) => {
                diagnostics.extend(validate(&config));
                if has_errors(&diagnostics) {
                    (None, diagnostics)
                } else {
                    (Some(config), diagnostics)
                }
            }
            Err(diagnostics) => (None, diagnostics),
        }
    }

    /// Saves `config` atomically and keeps one backup of the previous file.
    /// When the file on disk does not load (a hand edit gone wrong, or a failed
    /// startup load), it is first copied to `config.toml.broken-<unix_ms>` and a
    /// warning names the copy. If that copy fails, nothing is written.
    pub fn save(&self, config: &Config) -> Result<(), String> {
        let text = config
            .to_toml_string()
            .map_err(|e| format!("cannot serialize config: {e}"))?;
        if file_is_broken(&self.path) {
            let backup = backup_broken(&self.path).map_err(|e| {
                format!(
                    "cannot back up broken {} before saving: {e}",
                    self.path.display()
                )
            })?;
            logging::warn(&format!(
                "config file was invalid; its contents were kept at {} before saving",
                backup.display()
            ));
        }
        save_atomic(&self.path, &text)
            .map_err(|e| format!("cannot write {}: {e}", self.path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn base() -> Value {
        to_value(&Config::default()).expect("serialize default")
    }

    #[test]
    fn nested_objects_merge_and_siblings_survive() {
        let mut v = base();
        merge_patch(&mut v, &json!({"replay": {"max_seconds": 120}})).expect("merge");
        assert_eq!(v["replay"]["max_seconds"], json!(120));
        assert_eq!(v["replay"]["enabled"], base()["replay"]["enabled"]);
    }

    #[test]
    fn arrays_replace_and_null_clears_option() {
        let mut v = base();
        merge_patch(&mut v, &json!({"detection": {"allow": ["a.exe"]}})).expect("merge");
        assert_eq!(v["detection"]["allow"], json!(["a.exe"]));
        merge_patch(&mut v, &json!({"video": {"bitrate_kbps": null}})).expect("merge");
        assert_eq!(v["video"]["bitrate_kbps"], Value::Null);
    }

    #[test]
    fn unknown_key_is_rejected_with_its_path() {
        let mut v = base();
        let err = merge_patch(&mut v, &json!({"replay": {"nope": 1}})).expect_err("unknown");
        assert_eq!(err.path, "replay.nope");
    }

    #[test]
    fn type_mismatch_fails_to_parse_with_path() {
        let cfg = Config::default();
        let err = apply_patch(&cfg, &json!({"replay": {"max_seconds": "long"}}))
            .expect_err("type mismatch");
        assert_eq!(err.path, "replay.max_seconds");
    }

    #[test]
    fn games_table_accepts_new_entries() {
        let cfg = Config::default();
        let next = apply_patch(
            &cfg,
            &json!({"games": {"cs2.exe": {"display_name": "CS2"}}}),
        )
        .expect("new game");
        assert_eq!(
            next.games
                .get("cs2.exe")
                .and_then(|g| g.display_name.clone()),
            Some("CS2".to_owned())
        );
        let removed = apply_patch(&next, &json!({"games": {"cs2.exe": null}})).expect("remove");
        assert!(removed.games.is_empty());
    }

    #[test]
    fn save_keeps_backup_and_ignores_stray_tmp() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(CONFIG_FILE_NAME);
        fs::write(&path, "config_version = 1\n").expect("seed");
        fs::write(with_suffix(&path, "tmp"), "garbage [[[").expect("stray tmp");
        save_atomic(&path, "config_version = 1\n[replay]\nmax_seconds = 90\n").expect("save");
        let bak = fs::read_to_string(with_suffix(&path, "bak")).expect("bak");
        assert_eq!(bak, "config_version = 1\n");
        let now = Config::from_toml_str(&fs::read_to_string(&path).expect("read")).expect("parse");
        assert_eq!(now.replay.max_seconds, 90);
    }

    fn broken_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .expect("read_dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("config.toml.broken-"))
            .collect();
        names.sort();
        names
    }

    #[test]
    fn broken_detection_covers_syntax_validation_and_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(CONFIG_FILE_NAME);
        assert!(!file_is_broken(&path), "missing file is not broken");
        fs::write(&path, Config::default_toml()).expect("write default");
        assert!(!file_is_broken(&path), "default file loads");
        fs::write(&path, "replay = [[[\n").expect("write syntax error");
        assert!(file_is_broken(&path), "syntax error");
        fs::write(&path, "[games.\"exe:cs2\"]\ndisplay_name = \"CS2\"\n").expect("write invalid");
        assert!(file_is_broken(&path), "validation error");
    }

    #[test]
    fn save_copies_broken_file_before_overwriting() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(CONFIG_FILE_NAME);
        let broken = "replay = [[[ hand edited\n";
        fs::write(&path, broken).expect("seed broken");
        let store = ConfigStore::new(path.clone());
        store.save(&Config::default()).expect("save over broken");
        let copies = broken_names(dir.path());
        assert_eq!(copies.len(), 1, "one backup: {copies:?}");
        let kept = fs::read_to_string(dir.path().join(&copies[0])).expect("read copy");
        assert_eq!(kept, broken);
        assert!(!file_is_broken(&path), "new file loads");
    }

    #[test]
    fn valid_file_is_not_copied_on_save() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(CONFIG_FILE_NAME);
        fs::write(&path, Config::default_toml()).expect("seed");
        ConfigStore::new(path)
            .save(&Config::default())
            .expect("save");
        assert!(broken_names(dir.path()).is_empty());
    }

    #[test]
    fn broken_backups_keep_newest_five() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(CONFIG_FILE_NAME);
        for ms in [100u128, 300, 200, 500, 400, 700, 600] {
            fs::write(with_suffix(&path, &format!("broken-{ms}")), "x").expect("seed copy");
        }
        fs::write(with_suffix(&path, "bak"), "x").expect("unrelated bak");
        prune_broken(&path).expect("prune");
        let kept = broken_names(dir.path());
        let expected: Vec<String> = [300u128, 400, 500, 600, 700]
            .iter()
            .map(|ms| format!("config.toml.broken-{ms}"))
            .collect();
        assert_eq!(kept, expected);
        assert!(dir.path().join("config.toml.bak").exists());
    }

    #[test]
    fn backup_broken_creates_copy_and_respects_limit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(CONFIG_FILE_NAME);
        fs::write(&path, "broken = [[[\n").expect("seed");
        for _ in 0..(MAX_BROKEN_BACKUPS + 2) {
            // Names carry milliseconds, so space the copies out to keep them distinct.
            std::thread::sleep(std::time::Duration::from_millis(3));
            let copy = backup_broken(&path).expect("backup");
            assert!(copy.exists());
        }
        assert_eq!(broken_names(dir.path()).len(), MAX_BROKEN_BACKUPS);
    }

    #[test]
    fn parse_text_checked_reports_unknown_key_as_warning() {
        let (_, diagnostics) = parse_text_checked("bogus = 1\n").expect("parses");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, "CFG-P004");
        assert_eq!(diagnostics[0].severity, Severity::Warning);
    }

    #[test]
    fn parse_text_checked_empty_file_has_no_diagnostics() {
        let (_, diagnostics) = parse_text_checked("").expect("parses");
        assert!(diagnostics.is_empty());
    }
}
