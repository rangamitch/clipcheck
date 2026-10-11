//! clipcheck-library: SQLite-backed recording library (see docs/design/07-library-and-storage.md).
//!
//! This crate has no dependency on other clipcheck crates. Callers hand it plain values
//! (ULID strings, millisecond timestamps, byte counts) and receive plain structs back.

pub mod db;
pub mod fsops;
pub mod icons;
pub mod query;
pub mod roots;
pub mod schema;

pub use db::{
    AudioTrackRow, Library, NewGame, NewRecording, RecStatus, RecordingKind, RecordingRow,
};
pub use fsops::{
    expand_template, rename_on_disk_and_db, sanitize_filename, unique_path, validate_user_stem,
    FsOpError, NameError, RenameOutcome, TemplateVars,
};
pub use query::{ListQuery, Page, SortKey};
pub use roots::{ExternalFile, ExternalRecording, PathRow, RootInfo};

/// Errors returned by the library.
#[derive(Debug, thiserror::Error)]
pub enum LibError {
    /// An underlying SQLite error.
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
    /// A filesystem error (reserved for callers that wrap file operations).
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// The requested row does not exist.
    #[error("not found")]
    NotFound,
    /// The request is malformed (bad name, bad cursor, incompatible schema, ...).
    #[error("invalid request: {0}")]
    Invalid(String),
}
