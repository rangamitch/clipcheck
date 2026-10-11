//! Shared media types: codecs, stream ids, encoded packets and video parameters.
//!
//! All timestamps are in 100 ns ticks (WinRT TimeSpan units) on the capture clock.

use serde::{Deserialize, Serialize};

/// Video codec of a stream. Single definition shared with the config schema.
pub use crate::config::schema::Codec;

/// 100 ns ticks per millisecond.
pub const TICKS_PER_MS: i64 = 10_000;
/// 100 ns ticks per second.
pub const TICKS_PER_SEC: i64 = 10_000_000;

/// Which elementary stream a packet belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StreamId {
    /// The video stream.
    Video,
    /// Audio track number, 1-based.
    Audio(u8),
}

/// Parameters a muxer needs to write a sample description (avcC / hvcC / av1C).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoParams {
    /// Codec of the video stream.
    pub codec: Codec,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Frame rate numerator.
    pub fps_num: u32,
    /// Frame rate denominator.
    pub fps_den: u32,
    /// Codec configuration record (avcC, hvcC or av1C). Empty if not yet known.
    pub extradata: Vec<u8>,
}

/// One encoded access unit (video) or audio frame.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncodedPacket {
    /// Stream this packet belongs to.
    pub stream: StreamId,
    /// Presentation time in 100 ns ticks.
    pub pts_100ns: i64,
    /// Decode time in 100 ns ticks. Equals `pts_100ns` when there are no B-frames.
    pub dts_100ns: i64,
    /// Duration in 100 ns ticks.
    pub duration_100ns: i64,
    /// Random access point. Video: true only for an IDR. Audio: normally true.
    pub keyframe: bool,
    /// Video parameter generation this packet was encoded with. Audio uses 0.
    pub generation: u32,
    /// Encoded payload bytes.
    pub data: Vec<u8>,
}

/// Converts milliseconds to 100 ns ticks. Saturates on overflow.
pub fn ms_to_100ns(ms: i64) -> i64 {
    ms.saturating_mul(TICKS_PER_MS)
}

/// Converts seconds to 100 ns ticks, rounded to the nearest tick.
pub fn secs_to_100ns(secs: f64) -> i64 {
    (secs * TICKS_PER_SEC as f64).round() as i64
}

/// Converts 100 ns ticks to whole milliseconds, truncating toward zero.
pub fn ticks_to_ms(ticks: i64) -> i64 {
    ticks / TICKS_PER_MS
}

/// Converts 100 ns ticks to seconds as a float.
pub fn ticks_to_secs_f64(ticks: i64) -> f64 {
    ticks as f64 / TICKS_PER_SEC as f64
}
