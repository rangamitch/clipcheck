//! Built-in exe lists (5.16 - 5.18) merged with user configuration.
//!
//! All matching is case-insensitive on the executable file name.
//!
//! Entries may contain the wildcards `*` (any run of characters, including none)
//! and `?` (exactly one character). A plain name matches only itself, so every
//! list written before wildcards existed behaves as before. An entry made only of
//! wildcards (`*`, `??`) is ignored: it would otherwise block or allow every game.
//! See `docs/design/01-game-detection.md`, section "Name patterns".

use std::collections::HashSet;

use crate::{AllowRule, DetectConfig};

/// Built-in blocklist, excluding shell/system entries (see [`BUILTIN_SHELL`]).
pub const BUILTIN_BLOCKLIST: &[&str] = &[
    // Browsers
    "chrome.exe",
    "msedge.exe",
    "firefox.exe",
    "brave.exe",
    "opera.exe",
    "vivaldi.exe",
    "arc.exe",
    "msedgewebview2.exe",
    // Chat / comms
    "discord.exe",
    "slack.exe",
    "teams.exe",
    "ms-teams.exe",
    "zoom.exe",
    "telegram.exe",
    "whatsapp.exe",
    // Media
    "vlc.exe",
    "mpc-hc.exe",
    "mpc-hc64.exe",
    "mpv.exe",
    "spotify.exe",
    "wmplayer.exe",
    "netflix.exe",
    // Recorders / self
    "obs64.exe",
    "obs32.exe",
    "medal.exe",
    "clipcheck.exe",
    "clipcheckd.exe",
    "nvidia share.exe",
    "gamebar.exe",
    "gamebarpresencewriter.exe",
];

/// Built-in shell / system and developer-tool entries (reported as `SystemShell`).
pub const BUILTIN_SHELL: &[&str] = &[
    "explorer.exe",
    "dwm.exe",
    "shellexperiencehost.exe",
    "searchhost.exe",
    "startmenuexperiencehost.exe",
    "applicationframehost.exe",
    "textinputhost.exe",
    "taskmgr.exe",
    "windowsterminal.exe",
    "cmd.exe",
    "powershell.exe",
    "pwsh.exe",
    "notepad.exe",
    "code.exe",
    "devenv.exe",
    "idea64.exe",
    "rider64.exe",
];

/// Built-in launchers: never candidates, used for grouping only (5.17).
pub const BUILTIN_LAUNCHERS: &[&str] = &[
    "steam.exe",
    "steamwebhelper.exe",
    "epicgameslauncher.exe",
    "epicwebhelper.exe",
    "galaxyclient.exe",
    "xboxpcapp.exe",
    "battle.net.exe",
    "riotclientservices.exe",
    "eadesktop.exe",
    "ea app.exe",
    "upc.exe",
    "ubisoftconnect.exe",
    // Rockstar Games Launcher: starts GTA V (GTA5.exe) and is never the game window.
    "playgtav.exe",
];

/// Anti-cheat service executables (5.18). Never candidates by themselves.
pub const ANTICHEAT_SERVICES: &[&str] = &[
    "easyanticheat.exe",
    "easyanticheat_eos.exe",
    "beservice.exe",
    "vgc.exe",
    "vgtray.exe",
    "faceit.exe",
    "fbc.exe",
    "eosoverlayrenderer-win64-shipping.exe",
];

/// Window titles that never identify a game (G6).
pub const GENERIC_TITLES: &[&str] = &["", "Default IME", "MSCTFIME UI", "Program Manager"];

/// Returns the file-name part of an exe reference, trimmed, without allocating.
/// `"C:\\Games\\Foo.EXE "` -> `"Foo.EXE"`.
pub fn exe_basename(exe: &str) -> &str {
    exe.rsplit(['\\', '/']).next().unwrap_or_default().trim()
}

/// Normalizes an exe reference for matching: strips any directory part,
/// trims whitespace and lowercases. `"C:\\Games\\Foo.EXE"` -> `"foo.exe"`.
/// Delegates to [`clipcheck_core::ids::exe_file_name_lower`], the one shared definition.
pub fn normalize_exe(exe: &str) -> String {
    clipcheck_core::ids::exe_file_name_lower(exe)
}

/// Longest exe file name normalized on the stack; longer names allocate.
const STACK_NAME_BYTES: usize = 256;

/// Calls `f` with the normalized form of `exe` (same result as [`normalize_exe`]).
/// ASCII names up to [`STACK_NAME_BYTES`] are lowercased in a stack buffer, so the
/// per-candidate lookups do not allocate. Other names take the [`normalize_exe`] path,
/// which lowercases all of Unicode, so both paths always agree.
fn with_normalized<R>(exe: &str, f: impl FnOnce(&str) -> R) -> R {
    let name = exe_basename(exe);
    if name.is_ascii() && name.len() <= STACK_NAME_BYTES {
        let mut buf = [0u8; STACK_NAME_BYTES];
        let out = &mut buf[..name.len()];
        out.copy_from_slice(name.as_bytes());
        out.make_ascii_lowercase();
        if let Ok(s) = std::str::from_utf8(out) {
            return f(s);
        }
    }
    f(&normalize_exe(exe))
}

/// Returns true when `title` is one of the generic titles (case-insensitive).
pub fn is_generic_title(title: &str) -> bool {
    let t = title.trim();
    GENERIC_TITLES.iter().any(|g| g.eq_ignore_ascii_case(t))
}

/// True when `entry` (already normalized) contains a `*` or `?` wildcard.
pub fn is_pattern(entry: &str) -> bool {
    entry.contains(['*', '?'])
}

/// Case-insensitive (ASCII) glob match of `name` against `pattern`.
///
/// `*` matches any run of characters, `?` exactly one character. The whole
/// name must match. Directory parts are not stripped here: callers pass file
/// names (see [`normalize_exe`]). A pattern without wildcards is an equality test.
pub fn glob_match(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().map(|c| c.to_ascii_lowercase()).collect();
    let t: Vec<char> = name.chars().map(|c| c.to_ascii_lowercase()).collect();
    // Classic two-pointer match: on a mismatch, retry from the last `*` with one
    // more character absorbed. Linear in practice for exe-name sized inputs.
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut star_text = 0usize;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) && p[pi] != '*' {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            star_text = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            star_text += 1;
            ti = star_text;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// True when the exe (a file name or a path) matches one config entry.
///
/// A plain entry matches the same file name, ignoring case and directories. A wildcard
/// entry matches through [`glob_match`]. An empty or wildcard-only entry matches nothing,
/// the same rule [`NameSet`] applies.
pub fn name_matches(entry: &str, exe: &str) -> bool {
    let pattern = normalize_exe(entry);
    let name = normalize_exe(exe);
    if pattern.is_empty() || name.is_empty() {
        return false;
    }
    if is_pattern(&pattern) {
        pattern.chars().any(|c| c != '*' && c != '?') && glob_match(&pattern, &name)
    } else {
        pattern == name
    }
}

/// A set of normalized exe names: exact names in a hash set, wildcard entries
/// in a short list checked with [`glob_match`].
#[derive(Debug, Clone, Default)]
pub struct NameSet {
    exact: HashSet<String>,
    patterns: Vec<String>,
}

impl NameSet {
    /// Builds a set from raw entries (paths and case are normalized).
    pub fn from_names<'a>(names: impl IntoIterator<Item = &'a str>) -> Self {
        let mut set = Self::default();
        for name in names {
            set.insert(name);
        }
        set
    }

    /// Adds one entry. Empty entries and wildcard-only entries are ignored.
    pub fn insert(&mut self, raw: &str) {
        let norm = normalize_exe(raw);
        if norm.is_empty() {
            return;
        }
        if is_pattern(&norm) {
            if norm.chars().any(|c| c != '*' && c != '?') {
                self.patterns.push(norm);
            }
        } else {
            self.exact.insert(norm);
        }
    }

    /// True when the normalized `name` equals an exact entry or matches a pattern.
    pub fn contains(&self, name: &str) -> bool {
        self.exact.contains(name) || self.patterns.iter().any(|p| glob_match(p, name))
    }
}

/// Merged view of the built-in lists and user configuration.
#[derive(Debug, Clone, Default)]
pub struct Lists {
    blocked: NameSet,
    shell: NameSet,
    allowed: NameSet,
    launchers: NameSet,
    anticheat: NameSet,
}

impl Lists {
    /// Builds the merged lists from the detection configuration.
    pub fn from_config(cfg: &DetectConfig) -> Self {
        Self::new(&cfg.block_extra, &cfg.allow)
    }

    /// Merges built-in lists with `block_extra` and the allow rules.
    ///
    /// The blocklist always wins: an allow rule for an exe that is blocked
    /// (built-in or user) is ignored by [`Lists::is_allowlisted`].
    pub fn new(block_extra: &[String], allow: &[AllowRule]) -> Self {
        let mut blocked = NameSet::from_names(
            BUILTIN_BLOCKLIST
                .iter()
                .chain(BUILTIN_SHELL.iter())
                .copied(),
        );
        for extra in block_extra {
            blocked.insert(extra);
        }

        Self {
            blocked,
            shell: NameSet::from_names(BUILTIN_SHELL.iter().copied()),
            allowed: NameSet::from_names(allow.iter().map(|r| r.exe.as_str())),
            launchers: NameSet::from_names(BUILTIN_LAUNCHERS.iter().copied()),
            anticheat: NameSet::from_names(ANTICHEAT_SERVICES.iter().copied()),
        }
    }

    /// True when the exe is on the built-in or user blocklist (G1).
    pub fn is_blocked(&self, exe: &str) -> bool {
        with_normalized(exe, |n| self.blocked.contains(n))
    }

    /// True when the exe is a shell/system entry (reported as `SystemShell`).
    pub fn is_system_shell(&self, exe: &str) -> bool {
        with_normalized(exe, |n| self.shell.contains(n))
    }

    /// True when the exe is a built-in launcher (G2).
    pub fn is_launcher(&self, exe: &str) -> bool {
        with_normalized(exe, |n| self.launchers.contains(n))
    }

    /// True when the exe is an anti-cheat service name (5.18).
    pub fn is_anticheat_service(&self, exe: &str) -> bool {
        with_normalized(exe, |n| self.anticheat.contains(n))
    }

    /// True when the exe has an allow rule (exact or wildcard) and is not blocked.
    /// Blocklisted exes are never allowlisted, even via a manual add.
    pub fn is_allowlisted(&self, exe: &str) -> bool {
        with_normalized(exe, |n| {
            self.allowed.contains(n) && !self.blocked.contains(n)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_is_case_insensitive_and_strips_paths() {
        let lists = Lists::new(&[], &[]);
        assert!(lists.is_blocked("CHROME.EXE"));
        assert!(lists.is_blocked("C:\\Program Files\\Google\\chrome.exe"));
        assert!(lists.is_launcher("Steam.exe"));
        assert!(lists.is_system_shell("Explorer.EXE"));
        assert!(!lists.is_blocked("Game.exe"));
    }

    #[test]
    fn builtin_blocklist_covers_self_and_recorders() {
        let lists = Lists::new(&[], &[]);
        for exe in [
            "clipcheck.exe",
            "clipcheckd.exe",
            "obs64.exe",
            "vlc.exe",
            "discord.exe",
        ] {
            assert!(lists.is_blocked(exe), "{exe} should be blocked");
        }
    }

    #[test]
    fn block_extra_is_merged() {
        let lists = Lists::new(&["MyGameLauncherHelper.exe".to_string()], &[]);
        assert!(lists.is_blocked("mygamelauncherhelper.exe"));
    }

    #[test]
    fn blocklist_wins_over_allow_rule() {
        let allow = vec![AllowRule {
            exe: "chrome.exe".into(),
            display_name: None,
        }];
        let lists = Lists::new(&[], &allow);
        assert!(lists.is_blocked("chrome.exe"));
        assert!(!lists.is_allowlisted("chrome.exe"));
    }

    #[test]
    fn allow_rule_is_case_insensitive() {
        let allow = vec![AllowRule {
            exe: "Game.EXE".into(),
            display_name: Some("Game".into()),
        }];
        let lists = Lists::new(&[], &allow);
        assert!(lists.is_allowlisted("game.exe"));
        assert!(!lists.is_allowlisted("other.exe"));
    }

    #[test]
    fn stack_and_heap_normalization_agree() {
        let lists = Lists::new(&[], &[]);
        // A file name longer than the stack buffer takes the allocating fallback.
        // Directory parts are stripped first, so a long directory still matches.
        let long_dir = format!("{}\\CHROME.exe", "d".repeat(400));
        assert!(lists.is_blocked(&long_dir));
        let long_name = format!("{}.exe", "x".repeat(300));
        assert!(!lists.is_blocked(&long_name));
        assert!(!lists.is_allowlisted(&long_name));
        // Surrounding whitespace is trimmed on both paths.
        assert!(lists.is_anticheat_service("  C:\\x\\EasyAntiCheat.exe \t"));
        assert!(lists.is_launcher("steam.exe"));
        assert!(!lists.is_launcher("notsteam.exe"));
        assert!(!lists.is_anticheat_service(""));
        assert!(!lists.is_blocked("   "));
    }

    #[test]
    fn generic_titles_match_case_insensitively() {
        assert!(is_generic_title(""));
        assert!(is_generic_title("program manager"));
        assert!(!is_generic_title("Elden Ring"));
    }
}
