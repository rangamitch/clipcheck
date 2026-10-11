//! Built-in known-games database (task SIG1).
//!
//! The data lives in `known_games.json` (an array of `{"exe", "name"}`),
//! embedded at compile time. Lookups are case-insensitive on the exe file name.
//!
//! An `exe` may contain the wildcards `*` and `?` (see [`crate::lists::glob_match`]).
//! Use one for titles whose process name changes per build, such as FiveM's
//! `FiveM_b2802_GTAProcess.exe`. A plain name is matched exactly and wins over
//! any pattern. Patterns are tried in file order; the first match gives the name.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::Deserialize;

use crate::lists::{glob_match, is_pattern, normalize_exe};

const KNOWN_GAMES_JSON: &str = include_str!("known_games.json");

/// One known-game entry.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct KnownGame {
    /// Executable file name, e.g. `"Terraria.exe"`, or a wildcard pattern such as
    /// `"FiveM*GTAProcess.exe"`.
    pub exe: String,
    /// Display name, e.g. `"Terraria"`.
    pub name: String,
}

/// Exact names (lowercased) and wildcard patterns (normalized), built once.
struct Index {
    exact: HashMap<String, String>,
    patterns: Vec<(String, String)>,
}

static INDEX: OnceLock<Index> = OnceLock::new();

fn index() -> &'static Index {
    INDEX.get_or_init(|| {
        let mut exact = HashMap::new();
        let mut patterns = Vec::new();
        for g in entries() {
            if is_pattern(&g.exe) {
                patterns.push((normalize_exe(&g.exe), g.name));
            } else {
                exact.insert(g.exe.to_ascii_lowercase(), g.name);
            }
        }
        Index { exact, patterns }
    })
}

/// Parses the embedded JSON and returns every entry, duplicates included.
/// Returns an empty list if the JSON is malformed (a test guards this).
pub fn entries() -> Vec<KnownGame> {
    serde_json::from_str(KNOWN_GAMES_JSON).unwrap_or_default()
}

/// Looks up an exe file name (case-insensitive). Returns the display name.
///
/// Exact names are tried first, then wildcard patterns.
pub fn is_known(exe_name: &str) -> Option<&'static str> {
    let idx = index();
    if let Some(name) = idx.exact.get(&exe_name.trim().to_ascii_lowercase()) {
        return Some(name.as_str());
    }
    let file = normalize_exe(exe_name);
    if file.is_empty() {
        return None;
    }
    idx.patterns
        .iter()
        .find(|(pattern, _)| glob_match(pattern, &file))
        .map(|(_, name)| name.as_str())
}

/// Display name of a game that matched a wildcard pattern entry, e.g. `FiveM_b2802_GTAProcess.exe`
/// gives `"FiveM"`. Returns `None` for exact (plain-name) entries, which keep their own id, and
/// for exes that are not in the database.
pub fn stable_name(exe: &str) -> Option<&'static str> {
    let idx = index();
    let file = normalize_exe(exe);
    if file.is_empty() || idx.exact.contains_key(&file) {
        return None;
    }
    idx.patterns
        .iter()
        .find(|(pattern, _)| glob_match(pattern, &file))
        .map(|(_, name)| name.as_str())
}

/// Stable game id for an exe that matched a wildcard pattern entry: `"exe:"` followed by the
/// lowercase display name with every run of non-alphanumeric characters replaced by `-`.
/// `FiveM_b2802_GTAProcess.exe` gives `Some("exe:fivem")`, and every build of FiveM shares it.
/// Returns `None` for exact entries and unknown exes, so their ids stay `exe:<file name>`.
pub fn canonical_for(exe: &str) -> Option<String> {
    let name = stable_name(exe)?;
    let mut slug = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        return None;
    }
    Some(format!("exe:{slug}"))
}

/// Number of distinct entries in the database: exact exe names plus wildcard patterns.
pub fn count() -> usize {
    let idx = index();
    idx.exact.len() + idx.patterns.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_is_case_insensitive() {
        assert_eq!(is_known("Terraria.exe"), Some("Terraria"));
        assert_eq!(is_known("terraria.EXE"), Some("Terraria"));
        assert_eq!(is_known("not-a-game.exe"), None);
    }

    #[test]
    fn database_is_populated() {
        assert!(count() >= 300, "only {} games", count());
    }
}
