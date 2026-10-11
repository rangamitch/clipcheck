//! clipcheck-ipc: wire protocol between the clipcheck daemon and its UI.
//!
//! This crate is pure logic: length-prefixed JSON framing ([`frame`]) and the
//! protocol v1 message types ([`proto`]). Named-pipe IO lives elsewhere.

pub mod frame;
pub mod proto;

#[cfg(windows)]
pub mod pipe;

use std::time::Duration;

pub use frame::{read_frame, write_frame, FrameError, MAX_FRAME_BYTES};
#[cfg(windows)]
pub use pipe::{default_pipe_name, PipeClient, PipeConn, PipeServer};

/// Named pipe used by the daemon and the UI (design doc 10, section 4.1).
pub const PIPE_NAME: &str = r"\\.\pipe\clipcheck-ipc";

/// Time allowed for the hello handshake before the connection is dropped.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);

/// Maximum number of simultaneously connected clients.
pub const MAX_CLIENTS: u8 = 8;
