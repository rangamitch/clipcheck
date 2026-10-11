//! Store library scanning (task SIG1).
//!
//! Pure parsers (Steam VDF/ACF, Epic `.item` JSON, GOG registry values, Xbox
//! `MicrosoftGame.config`) operate on strings so they can be unit-tested. The
//! only OS-touching code is [`scan_system`], which reads the real store
//! locations and tolerates every failure (a missing store is simply skipped).
//! Matching is path-based: an exe belongs to an entry when its normalized path
//! lies inside the entry's install directory.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// How often [`LibraryCache`] re-scans the system, in milliseconds.
pub const RESCAN_INTERVAL_MS: u64 = 60_000;

/// Store an installed game belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Store {
    /// Steam (`appid`).
    Steam,
    /// Epic Games Store (`AppName`).
    Epic,
    /// GOG Galaxy (`gameID`).
    Gog,
    /// Xbox / Game Pass (`Identity Name`, or the title folder name).
    Xbox,
}

/// One installed game found by a library scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibEntry {
    /// Store the game was found in.
    pub store: Store,
    /// Store-specific game id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Directory the game is installed into.
    pub install_dir: PathBuf,
}

/// Result of one library scan across all stores.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryScan {
    /// Every installed game found.
    pub entries: Vec<LibEntry>,
}

impl LibraryScan {
    /// Finds the library entry whose install directory contains `exe_path`.
    ///
    /// Comparison is case-insensitive and treats `/` and `\` alike; a `\\?\`
    /// prefix and trailing separators are ignored. The match must end on a
    /// path-component boundary (`C:\games\foo` does not contain
    /// `C:\games\foobar\x.exe`). If several entries contain the exe, the most
    /// specific (longest install directory) wins.
    pub fn find_for_exe(&self, exe_path: &Path) -> Option<&LibEntry> {
        let exe = normalize_path(&exe_path.to_string_lossy());
        let mut best: Option<(&LibEntry, usize)> = None;
        for entry in &self.entries {
            let dir = normalize_path(&entry.install_dir.to_string_lossy());
            if !is_under(&exe, &dir) {
                continue;
            }
            if best.is_none_or(|(_, len)| dir.len() > len) {
                best = Some((entry, dir.len()));
            }
        }
        best.map(|(entry, _)| entry)
    }
}

/// Normalizes a Windows path for comparison: strips `\\?\` (and `\\?\UNC\`),
/// turns `/` into `\`, lowercases, and removes trailing separators.
pub fn normalize_path(path: &str) -> String {
    let unified: String = if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = path.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        path.to_string()
    };
    let mut out = unified.replace('/', r"\").to_lowercase();
    while out.len() > 1 && out.ends_with('\\') {
        out.pop();
    }
    out
}

/// True when `exe` (normalized) lies strictly inside `dir` (normalized).
fn is_under(exe: &str, dir: &str) -> bool {
    if dir.is_empty() || exe.len() <= dir.len() || !exe.starts_with(dir) {
        return false;
    }
    // `dir` is a prefix of `exe`, so `dir.len()` is a char boundary in `exe`.
    exe.as_bytes()[dir.len()] == b'\\'
}

// ---------------------------------------------------------------------------
// Valve KeyValues (VDF / ACF) parsing
// ---------------------------------------------------------------------------

/// A KeyValues value: either a string or a nested block.
#[derive(Debug, Clone, PartialEq)]
enum Kv {
    Str(String),
    Map(Vec<(String, Kv)>),
}

#[derive(Debug, PartialEq)]
enum Tok {
    Str(String),
    Open,
    Close,
}

/// Splits KeyValues text into tokens. Handles a UTF-8 BOM, `//` line
/// comments, quoted strings with `\\`, `\"`, `\n`, `\t` escapes, and bare words.
fn tokenize(src: &str) -> Vec<Tok> {
    let src = src.strip_prefix('\u{feff}').unwrap_or(src);
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '{' {
            out.push(Tok::Open);
            i += 1;
        } else if c == '}' {
            out.push(Tok::Close);
            i += 1;
        } else if c == '"' {
            i += 1;
            let mut s = String::new();
            while i < chars.len() && chars[i] != '"' {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    i += 1;
                    s.push(match chars[i] {
                        'n' => '\n',
                        't' => '\t',
                        other => other,
                    });
                } else {
                    s.push(chars[i]);
                }
                i += 1;
            }
            i += 1; // closing quote (or end of input)
            out.push(Tok::Str(s));
        } else {
            let mut s = String::new();
            while i < chars.len()
                && !chars[i].is_whitespace()
                && !matches!(chars[i], '{' | '}' | '"')
            {
                s.push(chars[i]);
                i += 1;
            }
            out.push(Tok::Str(s));
        }
    }
    out
}

/// Parses key/value pairs until a closing brace (consumed) or end of tokens.
fn parse_pairs(toks: &[Tok], pos: &mut usize) -> Vec<(String, Kv)> {
    let mut out = Vec::new();
    while *pos < toks.len() {
        match &toks[*pos] {
            Tok::Close => {
                *pos += 1;
                break;
            }
            Tok::Open => {
                // Stray opening brace without a key: skip it.
                *pos += 1;
            }
            Tok::Str(key) => {
                *pos += 1;
                if *pos >= toks.len() {
                    break;
                }
                match &toks[*pos] {
                    Tok::Str(value) => {
                        *pos += 1;
                        out.push((key.clone(), Kv::Str(value.clone())));
                    }
                    Tok::Open => {
                        *pos += 1;
                        let inner = parse_pairs(toks, pos);
                        out.push((key.clone(), Kv::Map(inner)));
                    }
                    // Key without a value: leave the brace for the caller's loop.
                    Tok::Close => {}
                }
            }
        }
    }
    out
}

/// Parses a whole KeyValues document into its top-level pairs.
fn parse_kv(src: &str) -> Vec<(String, Kv)> {
    let toks = tokenize(src);
    let mut pos = 0;
    let mut out = Vec::new();
    while pos < toks.len() {
        out.extend(parse_pairs(&toks, &mut pos));
    }
    out
}

fn kv_get<'a>(pairs: &'a [(String, Kv)], key: &str) -> Option<&'a Kv> {
    pairs
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v)
}

fn kv_str<'a>(pairs: &'a [(String, Kv)], key: &str) -> Option<&'a str> {
    match kv_get(pairs, key)? {
        Kv::Str(s) => Some(s.as_str()),
        Kv::Map(_) => None,
    }
}

fn kv_map<'a>(pairs: &'a [(String, Kv)], key: &str) -> Option<&'a [(String, Kv)]> {
    match kv_get(pairs, key)? {
        Kv::Map(m) => Some(m.as_slice()),
        Kv::Str(_) => None,
    }
}

/// Parses `libraryfolders.vdf` and returns each library's `steamapps` directory
/// (library root `path` joined with `steamapps`), in file order, de-duplicated.
/// Understands both the current format (`"0" { "path" "..." }`) and the legacy
/// one (`"1" "D:\\SteamLibrary"`).
pub fn parse_library_folders_vdf(src: &str) -> Vec<PathBuf> {
    let top = parse_kv(src);
    let Some(folders) = kv_map(&top, "libraryfolders") else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = Vec::new();
    for (key, value) in folders {
        let numeric = !key.is_empty() && key.chars().all(|c| c.is_ascii_digit());
        let root = match value {
            Kv::Map(inner) => kv_str(inner, "path"),
            Kv::Str(s) if numeric => Some(s.as_str()),
            Kv::Str(_) => None,
        };
        let Some(root) = root.map(str::trim).filter(|r| !r.is_empty()) else {
            continue;
        };
        let steamapps = PathBuf::from(root).join("steamapps");
        let norm = normalize_path(&steamapps.to_string_lossy());
        if !out
            .iter()
            .any(|p| normalize_path(&p.to_string_lossy()) == norm)
        {
            out.push(steamapps);
        }
    }
    out
}

/// Parses an `appmanifest_<appid>.acf`. Returns `(appid, name, installdir)`.
/// Returns `None` when the appid or installdir is missing or empty.
pub fn parse_appmanifest_acf(src: &str) -> Option<(String, String, String)> {
    let top = parse_kv(src);
    let state = kv_map(&top, "AppState")?;
    let appid = kv_str(state, "appid")?.trim();
    let installdir = kv_str(state, "installdir")?.trim();
    if appid.is_empty() || installdir.is_empty() {
        return None;
    }
    let name = kv_str(state, "name")
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or(installdir);
    Some((appid.to_string(), name.to_string(), installdir.to_string()))
}

// ---------------------------------------------------------------------------
// Epic, GOG, Xbox parsers
// ---------------------------------------------------------------------------

fn strip_bom(s: &str) -> &str {
    s.strip_prefix('\u{feff}').unwrap_or(s)
}

/// Parses an Epic Games Store `.item` manifest. Returns
/// `(AppName, DisplayName-or-AppName, InstallLocation)`. Incomplete installs
/// (`bIsIncompleteInstall: true`) and manifests without an install location
/// yield `None`.
pub fn parse_epic_item_json(src: &str) -> Option<(String, String, String)> {
    let v: serde_json::Value = serde_json::from_str(strip_bom(src)).ok()?;
    if v.get("bIsIncompleteInstall")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    let app_name = v.get("AppName")?.as_str()?.trim();
    let location = v.get("InstallLocation")?.as_str()?.trim();
    if app_name.is_empty() || location.is_empty() {
        return None;
    }
    let display = v
        .get("DisplayName")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .unwrap_or(app_name);
    Some((
        app_name.to_string(),
        display.to_string(),
        location.to_string(),
    ))
}

/// Builds a GOG entry from the values under `HKLM\SOFTWARE\WOW6432Node\GOG.com\Games\<id>`
/// (`gameID`, `gameName`, `path`). Requires a game id and a path; the name
/// falls back to the id. A missing `exe` value is fine.
pub fn parse_gog_registry_values(
    game_id: Option<&str>,
    game_name: Option<&str>,
    path: Option<&str>,
) -> Option<LibEntry> {
    let id = game_id?.trim();
    let path = path?.trim();
    if id.is_empty() || path.is_empty() {
        return None;
    }
    let name = game_name
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or(id);
    Some(LibEntry {
        store: Store::Gog,
        id: id.to_string(),
        name: name.to_string(),
        install_dir: PathBuf::from(path),
    })
}

/// Reads the value of attribute `attr` on the first `<tag ...>` element of
/// `xml`. Matches only whole attribute names (preceded by a space).
fn xml_attr(xml: &str, tag: &str, attr: &str) -> Option<String> {
    let start = xml.find(tag)?;
    let rest = &xml[start..];
    let end = rest.find('>').unwrap_or(rest.len());
    let element = &rest[..end];
    let needle = format!(" {attr}=\"");
    let pos = element.find(&needle)?;
    let after = &element[pos + needle.len()..];
    let close = after.find('"')?;
    Some(after[..close].to_string())
}

/// Extracts `(Identity Name, ShellVisuals DefaultDisplayName)` from a
/// `MicrosoftGame.config` document using plain string search (no XML crate).
/// Tolerates a UTF-8 BOM. Returns `None` when there is no non-empty identity name.
pub fn parse_xbox_config(xml: &str) -> Option<(String, Option<String>)> {
    let xml = strip_bom(xml);
    let identity = xml_attr(xml, "<Identity", "Name")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())?;
    let display = xml_attr(xml, "<ShellVisuals", "DefaultDisplayName")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    Some((identity, display))
}

// ---------------------------------------------------------------------------
// OS scan (Windows only)
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod sys {
    use std::path::{Path, PathBuf};

    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::{ERROR_NO_MORE_ITEMS, ERROR_SUCCESS, FILETIME};
    use windows::Win32::System::Registry::{
        RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER,
        HKEY_LOCAL_MACHINE, KEY_READ, REG_SZ, REG_VALUE_TYPE,
    };

    use super::{
        normalize_path, parse_appmanifest_acf, parse_epic_item_json, parse_gog_registry_values,
        parse_library_folders_vdf, parse_xbox_config, LibEntry, Store,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Opens a registry key read-only; the handle is closed on drop.
    struct Key(HKEY);

    impl Key {
        fn open(root: HKEY, subkey: &str) -> Option<Key> {
            let sub = wide(subkey);
            let mut handle = HKEY::default();
            // SAFETY: `sub` is a valid NUL-terminated UTF-16 string that outlives
            // the call; `handle` is a valid out-pointer.
            let status =
                unsafe { RegOpenKeyExW(root, PCWSTR(sub.as_ptr()), 0, KEY_READ, &mut handle) };
            (status == ERROR_SUCCESS).then_some(Key(handle))
        }

        /// Reads a `REG_SZ` value as a string.
        fn string(&self, name: &str) -> Option<String> {
            let name = wide(name);
            let mut ty = REG_VALUE_TYPE(0);
            let mut bytes: u32 = 0;
            // SAFETY: querying the required size with a null data buffer.
            let status = unsafe {
                RegQueryValueExW(
                    self.0,
                    PCWSTR(name.as_ptr()),
                    None,
                    Some(&mut ty),
                    None,
                    Some(&mut bytes),
                )
            };
            if status != ERROR_SUCCESS || ty != REG_SZ || bytes == 0 {
                return None;
            }
            let mut buf = vec![0u16; (bytes as usize).div_ceil(2)];
            let mut len = bytes;
            // SAFETY: `buf` has room for `len` bytes, as reported by the size query.
            let status = unsafe {
                RegQueryValueExW(
                    self.0,
                    PCWSTR(name.as_ptr()),
                    None,
                    Some(&mut ty),
                    Some(buf.as_mut_ptr().cast::<u8>()),
                    Some(&mut len),
                )
            };
            if status != ERROR_SUCCESS {
                return None;
            }
            let units = (len as usize / 2).min(buf.len());
            let text = String::from_utf16_lossy(&buf[..units]);
            Some(text.trim_end_matches('\0').to_string())
        }

        /// Lists immediate subkey names.
        fn subkeys(&self) -> Vec<String> {
            let mut out = Vec::new();
            for index in 0..10_000u32 {
                let mut name = vec![0u16; 512];
                let mut len = name.len() as u32;
                // SAFETY: `name` is writable for `len` UTF-16 units; the optional
                // class/time out-parameters are omitted.
                let status = unsafe {
                    RegEnumKeyExW(
                        self.0,
                        index,
                        PWSTR(name.as_mut_ptr()),
                        &mut len,
                        None,
                        PWSTR::null(),
                        None,
                        None::<*mut FILETIME>,
                    )
                };
                if status == ERROR_NO_MORE_ITEMS {
                    break;
                }
                if status != ERROR_SUCCESS {
                    continue;
                }
                out.push(String::from_utf16_lossy(&name[..len as usize]));
            }
            out
        }
    }

    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: the handle was obtained from RegOpenKeyExW and is closed once.
            unsafe {
                let _ = RegCloseKey(self.0);
            }
        }
    }

    fn read_text(path: &Path) -> Option<String> {
        std::fs::read(path)
            .ok()
            .map(|b| String::from_utf8_lossy(&b).into_owned())
    }

    fn same_path(a: &Path, b: &Path) -> bool {
        normalize_path(&a.to_string_lossy()) == normalize_path(&b.to_string_lossy())
    }

    fn steam_root() -> Option<PathBuf> {
        if let Some(key) = Key::open(HKEY_CURRENT_USER, r"Software\Valve\Steam") {
            if let Some(p) = key.string("SteamPath") {
                let path = PathBuf::from(p.replace('/', r"\"));
                if path.is_dir() {
                    return Some(path);
                }
            }
        }
        let pf = std::env::var_os("ProgramFiles(x86)")?;
        let path = PathBuf::from(pf).join("Steam");
        path.is_dir().then_some(path)
    }

    pub(super) fn scan_steam() -> Vec<LibEntry> {
        let Some(root) = steam_root() else {
            return Vec::new();
        };
        let mut libraries = vec![root.join("steamapps")];
        if let Some(vdf) = read_text(&root.join("steamapps").join("libraryfolders.vdf")) {
            for dir in parse_library_folders_vdf(&vdf) {
                if !libraries.iter().any(|known| same_path(known, &dir)) {
                    libraries.push(dir);
                }
            }
        }
        let mut out = Vec::new();
        for steamapps in libraries {
            let Ok(read) = std::fs::read_dir(&steamapps) else {
                continue;
            };
            for item in read.flatten() {
                let file_name = item.file_name().to_string_lossy().into_owned();
                if !(file_name.starts_with("appmanifest_") && file_name.ends_with(".acf")) {
                    continue;
                }
                let Some(text) = read_text(&item.path()) else {
                    continue;
                };
                if let Some((id, name, installdir)) = parse_appmanifest_acf(&text) {
                    out.push(LibEntry {
                        store: Store::Steam,
                        id,
                        name,
                        install_dir: steamapps.join("common").join(installdir),
                    });
                }
            }
        }
        out
    }

    pub(super) fn scan_epic() -> Vec<LibEntry> {
        let Some(program_data) = std::env::var_os("ProgramData") else {
            return Vec::new();
        };
        let dir = PathBuf::from(program_data)
            .join("Epic")
            .join("EpicGamesLauncher")
            .join("Data")
            .join("Manifests");
        let Ok(read) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for item in read.flatten() {
            let path = item.path();
            let is_item = path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("item"));
            if !is_item {
                continue;
            }
            let Some(text) = read_text(&path) else {
                continue;
            };
            if let Some((id, name, location)) = parse_epic_item_json(&text) {
                out.push(LibEntry {
                    store: Store::Epic,
                    id,
                    name,
                    install_dir: PathBuf::from(location),
                });
            }
        }
        out
    }

    pub(super) fn scan_gog() -> Vec<LibEntry> {
        const GAMES: &str = r"SOFTWARE\WOW6432Node\GOG.com\Games";
        let Some(games) = Key::open(HKEY_LOCAL_MACHINE, GAMES) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for sub in games.subkeys() {
            let Some(key) = Key::open(HKEY_LOCAL_MACHINE, &format!(r"{GAMES}\{sub}")) else {
                continue;
            };
            let id = key.string("gameID");
            let name = key.string("gameName");
            let path = key.string("path");
            if let Some(entry) =
                parse_gog_registry_values(id.as_deref(), name.as_deref(), path.as_deref())
            {
                out.push(entry);
            }
        }
        out
    }

    pub(super) fn scan_xbox() -> Vec<LibEntry> {
        let mut out = Vec::new();
        for letter in 'C'..='Z' {
            let games = PathBuf::from(format!(r"{letter}:\XboxGames"));
            let Ok(read) = std::fs::read_dir(&games) else {
                continue;
            };
            for title in read.flatten() {
                let title_path = title.path();
                let content = title_path.join("Content");
                if !content.is_dir() {
                    continue;
                }
                let folder = title.file_name().to_string_lossy().into_owned();
                let config = read_text(&content.join("MicrosoftGame.config"));
                let parsed = config.as_deref().and_then(parse_xbox_config);
                let (id, name) = match parsed {
                    Some((identity, display)) => {
                        let name = display.unwrap_or_else(|| identity.clone());
                        (identity, name)
                    }
                    None => (folder.clone(), folder),
                };
                out.push(LibEntry {
                    store: Store::Xbox,
                    id,
                    name,
                    install_dir: content,
                });
            }
        }
        out
    }
}

/// Scans the local machine for installed games in Steam, Epic, GOG and Xbox.
///
/// Every step is failure-tolerant: a missing store is skipped, and entries
/// whose install directory does not exist on disk are dropped.
#[cfg(windows)]
pub fn scan_system() -> LibraryScan {
    let mut entries = Vec::new();
    entries.extend(sys::scan_steam());
    entries.extend(sys::scan_epic());
    entries.extend(sys::scan_gog());
    entries.extend(sys::scan_xbox());
    entries.retain(|e| e.install_dir.is_dir());
    LibraryScan { entries }
}

/// Non-Windows builds have no store locations; the scan is always empty.
#[cfg(not(windows))]
pub fn scan_system() -> LibraryScan {
    LibraryScan::default()
}

/// Caches a [`LibraryScan`] and re-runs the scanner at most once per
/// [`RESCAN_INTERVAL_MS`]. The clock is supplied by the caller, so it can be
/// driven in tests.
pub struct LibraryCache {
    scanner: Box<dyn FnMut() -> LibraryScan + Send>,
    last_scan_ms: Option<u64>,
    scan: LibraryScan,
}

impl LibraryCache {
    /// Cache backed by [`scan_system`].
    pub fn new() -> Self {
        Self::with_scanner(scan_system)
    }

    /// Cache backed by an arbitrary scanner (used by tests).
    pub fn with_scanner<F>(scanner: F) -> Self
    where
        F: FnMut() -> LibraryScan + Send + 'static,
    {
        Self {
            scanner: Box::new(scanner),
            last_scan_ms: None,
            scan: LibraryScan::default(),
        }
    }

    /// Returns the current scan, re-scanning first when the cached one is at
    /// least [`RESCAN_INTERVAL_MS`] old (or the clock went backwards).
    pub fn get(&mut self, now_ms: u64) -> &LibraryScan {
        let stale = match self.last_scan_ms {
            None => true,
            Some(last) => now_ms < last || now_ms - last >= RESCAN_INTERVAL_MS,
        };
        if stale {
            self.scan = (self.scanner)();
            self.last_scan_ms = Some(now_ms);
        }
        &self.scan
    }
}

impl Default for LibraryCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_handles_prefix_slashes_and_case() {
        assert_eq!(normalize_path(r"\\?\F:\Games\Foo\"), r"f:\games\foo");
        assert_eq!(normalize_path("F:/Games/Foo"), r"f:\games\foo");
        assert_eq!(normalize_path(r"\\?\UNC\srv\share"), r"\\srv\share");
    }

    #[test]
    fn find_requires_component_boundary() {
        let scan = LibraryScan {
            entries: vec![LibEntry {
                store: Store::Steam,
                id: "1".into(),
                name: "Foo".into(),
                install_dir: PathBuf::from(r"C:\games\foo"),
            }],
        };
        assert!(scan
            .find_for_exe(Path::new(r"C:\games\foo\bin\x.exe"))
            .is_some());
        assert!(scan
            .find_for_exe(Path::new(r"C:\GAMES\FOO\x.exe"))
            .is_some());
        assert!(scan
            .find_for_exe(Path::new(r"C:\games\foobar\x.exe"))
            .is_none());
        assert!(scan.find_for_exe(Path::new(r"C:\games\foo")).is_none());
    }

    #[test]
    fn appmanifest_minimal() {
        let acf = "\"AppState\"\n{\n\t\"appid\"\t\"440\"\n\t\"name\"\t\"Team Fortress 2\"\n\t\"installdir\"\t\"Team Fortress 2\"\n}\n";
        assert_eq!(
            parse_appmanifest_acf(acf),
            Some((
                "440".into(),
                "Team Fortress 2".into(),
                "Team Fortress 2".into()
            ))
        );
    }

    #[test]
    fn xbox_config_identity() {
        let xml = "\u{feff}<Game ConfigVersion=\"1\"><Identity Name=\"Microsoft.Foo\" Publisher=\"X\"/><ShellVisuals DefaultDisplayName=\"Foo Game\"/></Game>";
        assert_eq!(
            parse_xbox_config(xml),
            Some(("Microsoft.Foo".into(), Some("Foo Game".into())))
        );
    }

    #[test]
    fn cache_rescans_at_most_every_interval() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let mut cache = LibraryCache::with_scanner(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            LibraryScan::default()
        });
        cache.get(0);
        cache.get(RESCAN_INTERVAL_MS - 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        cache.get(RESCAN_INTERVAL_MS);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
