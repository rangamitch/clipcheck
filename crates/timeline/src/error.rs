//! Errors returned by editing operations. Operations never panic on bad input; they return one
//! of these and leave the project unchanged.

use thiserror::Error;

/// Why an edit was refused.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum EditError {
    /// No clip, track, media, group or marker has this id.
    #[error("not found: {0}")]
    NotFound(String),
    /// A value is out of range or malformed (the message names the field).
    #[error("invalid value: {0}")]
    Invalid(String),
    /// The edit would put two clips on top of each other on one track.
    #[error("clips would overlap on track {0}")]
    Overlap(String),
    /// A locked track refuses edits.
    #[error("track {0} is locked")]
    Locked(String),
    /// The clip kind does not fit the track kind (for example audio on a video track).
    #[error("clip kind does not fit the track: {0}")]
    WrongTrackKind(String),
    /// A limit from section 8.8 would be exceeded (tracks, duration, tick range).
    #[error("limit exceeded: {0}")]
    Limit(String),
    /// The media is still used by clips.
    #[error("media is in use by {0} clip(s)")]
    InUse(usize),
    /// Nothing to undo or redo.
    #[error("nothing to {0}")]
    NothingTo(&'static str),
    /// The operation is not implemented yet.
    #[error("not implemented: {0}")]
    Unsupported(&'static str),
}

/// Result alias for edits.
pub type EditResult<T> = Result<T, EditError>;
