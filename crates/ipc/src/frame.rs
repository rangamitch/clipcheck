//! Length-prefixed JSON framing over any `std::io` byte stream.
//!
//! Wire format: a 4-byte unsigned little-endian body length `N`, followed by
//! `N` bytes of UTF-8 JSON. `N` must be at most [`MAX_FRAME_BYTES`].

use std::io::{self, ErrorKind, Read, Write};

use serde::de::DeserializeOwned;
use serde::Serialize;
use thiserror::Error;

/// Maximum frame body size in bytes. Enforced on both read and write.
pub const MAX_FRAME_BYTES: usize = 1 << 20;

/// Errors produced by frame IO.
#[derive(Debug, Error)]
pub enum FrameError {
    /// The body is larger than [`MAX_FRAME_BYTES`]. Detected before allocating.
    #[error("frame of {0} bytes exceeds MAX_FRAME_BYTES")]
    TooLarge(usize),
    /// The stream ended cleanly at a frame boundary.
    #[error("peer closed the stream")]
    Eof,
    /// The stream ended in the middle of a frame header or body.
    #[error("stream ended in the middle of a frame")]
    Truncated,
    /// The body is not valid JSON for the requested type.
    #[error("invalid json body: {0}")]
    Json(#[from] serde_json::Error),
    /// Underlying IO failure.
    #[error("io error: {0}")]
    Io(#[from] io::Error),
}

/// Serializes `value` as JSON and writes it as one frame, then flushes.
///
/// Returns [`FrameError::TooLarge`] without writing anything if the encoded
/// body exceeds [`MAX_FRAME_BYTES`].
pub fn write_frame<W, T>(w: &mut W, value: &T) -> Result<(), FrameError>
where
    W: Write,
    T: Serialize + ?Sized,
{
    // One contiguous buffer, one `write_all`: on a pipe that is one overlapped
    // write instead of a header write plus a body write.
    let frame = encode_frame(value)?;
    w.write_all(&frame)?;
    w.flush()?;
    Ok(())
}

/// Size of the length header in bytes.
const HEADER_BYTES: usize = 4;

/// Serializes `value` into a complete frame: the 4-byte little-endian length
/// followed by the JSON body, in one allocation.
fn encode_frame<T>(value: &T) -> Result<Vec<u8>, FrameError>
where
    T: Serialize + ?Sized,
{
    let mut buf = vec![0u8; HEADER_BYTES];
    serde_json::to_writer(&mut buf, value)?;
    finish_frame(buf)
}

/// Patches the length header of `buf`, whose first [`HEADER_BYTES`] bytes are
/// reserved and whose remainder is the body.
fn finish_frame(mut buf: Vec<u8>) -> Result<Vec<u8>, FrameError> {
    let body_len = buf.len() - HEADER_BYTES;
    if body_len > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge(body_len));
    }
    let len = u32::try_from(body_len).map_err(|_| FrameError::TooLarge(body_len))?;
    buf[..HEADER_BYTES].copy_from_slice(&len.to_le_bytes());
    Ok(buf)
}

/// Reads one frame and deserializes its JSON body as `T`.
///
/// The length is checked against [`MAX_FRAME_BYTES`] before the body buffer is
/// allocated. A stream that ends exactly at a frame boundary yields
/// [`FrameError::Eof`]; one that ends inside a header or body yields
/// [`FrameError::Truncated`].
pub fn read_frame<R, T>(r: &mut R) -> Result<T, FrameError>
where
    R: Read,
    T: DeserializeOwned,
{
    let mut header = [0u8; 4];
    let mut filled = 0usize;
    while filled < header.len() {
        match r.read(&mut header[filled..]) {
            Ok(0) if filled == 0 => return Err(FrameError::Eof),
            Ok(0) => return Err(FrameError::Truncated),
            Ok(n) => filled += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(FrameError::Io(e)),
        }
    }

    let len = u32::from_le_bytes(header) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge(len));
    }

    let mut body = vec![0u8; len];
    r.read_exact(&mut body).map_err(|e| {
        if e.kind() == ErrorKind::UnexpectedEof {
            FrameError::Truncated
        } else {
            FrameError::Io(e)
        }
    })?;
    Ok(serde_json::from_slice(&body)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records every `write` call and the bytes received.
    #[derive(Default)]
    struct CountingWriter {
        calls: usize,
        flushes: usize,
        bytes: Vec<u8>,
    }

    impl Write for CountingWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.calls += 1;
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    /// Reference encoding: header followed by the separately serialized body.
    fn reference_frame(value: &serde_json::Value) -> Vec<u8> {
        let body = serde_json::to_vec(value).expect("serialize");
        let mut out = (body.len() as u32).to_le_bytes().to_vec();
        out.extend_from_slice(&body);
        out
    }

    /// A JSON string whose serialized form is exactly `total` bytes long.
    fn string_of_serialized_len(total: usize) -> serde_json::Value {
        serde_json::Value::String("a".repeat(total - 2))
    }

    fn assert_single_write_matches(value: &serde_json::Value, body_len: usize) {
        let mut w = CountingWriter::default();
        write_frame(&mut w, value).expect("write frame");
        assert_eq!(w.calls, 1, "one write call per frame");
        assert_eq!(w.flushes, 1, "one flush per frame");
        assert_eq!(w.bytes, reference_frame(value));
        assert_eq!(w.bytes.len(), HEADER_BYTES + body_len);
        let mut reader = &w.bytes[..];
        let back: serde_json::Value = read_frame(&mut reader).expect("read back");
        assert_eq!(&back, value);
    }

    #[test]
    fn body_of_one_byte_is_one_write_and_matches_reference() {
        assert_single_write_matches(&serde_json::json!(0), 1);
    }

    #[test]
    fn body_of_64_kib_is_one_write_and_matches_reference() {
        let value = string_of_serialized_len(64 * 1024);
        assert_single_write_matches(&value, 64 * 1024);
    }

    #[test]
    fn body_at_max_is_one_write_and_matches_reference() {
        let value = string_of_serialized_len(MAX_FRAME_BYTES);
        assert_single_write_matches(&value, MAX_FRAME_BYTES);
    }

    #[test]
    fn empty_body_header_is_zero_length() {
        let frame = finish_frame(vec![0u8; HEADER_BYTES]).expect("empty body");
        assert_eq!(frame, vec![0, 0, 0, 0]);
    }

    #[test]
    fn oversized_body_is_rejected_without_writing() {
        let value = string_of_serialized_len(MAX_FRAME_BYTES + 1);
        let mut w = CountingWriter::default();
        let err = write_frame(&mut w, &value).expect_err("too large");
        assert!(matches!(err, FrameError::TooLarge(n) if n == MAX_FRAME_BYTES + 1));
        assert_eq!(w.calls, 0);
        assert_eq!(w.flushes, 0);
    }
}
