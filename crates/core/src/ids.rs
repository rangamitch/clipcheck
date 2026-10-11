//! Identifiers: ULID-based session and clip ids, normalized game ids, and wall-clock helper.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};
use ulid::Ulid;

/// Generates a ULID newtype with `new()`, `Default`, `Display`, `FromStr` and string serde.
macro_rules! ulid_newtype {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(Ulid);

        impl $name {
            /// Creates a new id from the current time and randomness.
            pub fn new() -> Self {
                Self(Ulid::new())
            }

            /// Wraps an existing ULID.
            pub fn from_ulid(ulid: Ulid) -> Self {
                Self(ulid)
            }

            /// Returns the underlying ULID.
            pub fn ulid(&self) -> Ulid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl FromStr for $name {
            type Err = ulid::DecodeError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                s.parse::<Ulid>().map(Self)
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.collect_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let s = String::deserialize(deserializer)?;
                s.parse::<Ulid>()
                    .map(Self)
                    .map_err(serde::de::Error::custom)
            }
        }
    };
}

ulid_newtype!(
    /// Identifier of one recording session.
    SessionId
);

ulid_newtype!(
    /// Identifier of one saved clip.
    ClipId
);

/// Normalized game identifier: `<exe>` or `<exe>@<store>:<store_id>`, all lowercase.
///
/// Examples: `from_exe("Hades.exe")` gives `hades`; `with_store("hades", "steam", "1145360")`
/// gives `hades@steam:1145360`. Every constructor and deserialization normalizes, so ids that
/// differ only in case or surrounding whitespace compare equal.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct GameId(String);

impl GameId {
    /// Builds an id from its text form, `<exe>` or `<exe>@<store>:<store_id>`. The exe part is
    /// normalized with [`normalize_exe`]; the store part is trimmed and lowercased.
    pub fn new(text: &str) -> Self {
        match text.split_once('@') {
            Some((exe, store)) => Self(format!(
                "{}@{}",
                normalize_exe(exe),
                store.trim().to_lowercase()
            )),
            None => Self(normalize_exe(text)),
        }
    }

    /// Builds an id from an executable name or path. Directory parts and a trailing `.exe`
    /// are removed, and the result is lowercased.
    pub fn from_exe(exe_name: &str) -> Self {
        Self(normalize_exe(exe_name))
    }

    /// Builds an id that also carries the store and its store-specific game id. Both the store
    /// name and the store id are trimmed and lowercased.
    pub fn with_store(exe_name: &str, store: &str, store_id: &str) -> Self {
        Self(format!(
            "{}@{}:{}",
            normalize_exe(exe_name),
            store.trim().to_lowercase(),
            store_id.trim().to_lowercase()
        ))
    }

    /// Returns the normalized executable part (text before `@`).
    pub fn exe(&self) -> &str {
        self.0
            .split_once('@')
            .map_or(self.0.as_str(), |(exe, _)| exe)
    }

    /// Returns `(store, store_id)` when the id carries store information.
    pub fn store(&self) -> Option<(&str, &str)> {
        let (_, rest) = self.0.split_once('@')?;
        rest.split_once(':')
    }

    /// Returns the full normalized id string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for GameId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Ok(GameId::new(&text))
    }
}

impl fmt::Display for GameId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The lowercased executable file name, keeping its `.exe` suffix.
///
/// Everything before the last `\` or `/` is dropped, the rest is trimmed and lowercased:
/// `C:\Games\Foo.EXE` gives `foo.exe`. This is the matching key for exe names (detect). The game
/// id key is [`normalize_exe`], which also drops the `.exe`; the two differ only in that suffix.
pub fn exe_file_name_lower(name: &str) -> String {
    let base = match name.rfind(['\\', '/']) {
        Some(index) => &name[index + 1..],
        None => name,
    };
    base.trim().to_lowercase()
}

/// The canonical executable name used for game ids and config keys.
///
/// [`exe_file_name_lower`] followed by removing one trailing `.exe`: `C:\Games\Foo.EXE` gives
/// `foo`. This is the only definition of the exe part of a game id; other code in the workspace
/// should call it rather than copy it.
pub fn normalize_exe(name: &str) -> String {
    let lower = exe_file_name_lower(name);
    match lower.strip_suffix(".exe") {
        Some(stem) => stem.to_owned(),
        None => lower,
    }
}

/// Current Unix time in milliseconds. Returns 0 if the clock is before the epoch.
pub fn unix_ms_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}
