//! Annex B byte-stream helpers and an RBSP bit reader (worker M1).

/// Video codec of an elementary stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodec {
    /// H.264 / AVC.
    H264,
    /// H.265 / HEVC.
    Hevc,
}

/// Position of the next `00 00 01` start code at or after `from`.
fn find_start_code(data: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i < data.len() {
        if data.get(i..i + 3) == Some(&[0, 0, 1][..]) {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Strips trailing zero bytes (they belong to the next 4-byte start code, not to the NAL).
fn trim_trailing_zeros(mut nal: &[u8]) -> &[u8] {
    while let Some((&0, rest)) = nal.split_last() {
        nal = rest;
    }
    nal
}

/// NAL unit payloads (without start codes) of an Annex B byte stream.
///
/// Start codes are `00 00 01` or `00 00 00 01`. Data before the first start code is ignored,
/// empty NAL units are skipped, and an input without any start code yields an empty list.
pub fn split_nals(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let Some(first) = find_start_code(data, 0) else {
        return out;
    };
    let mut start = first + 3;
    loop {
        let next = find_start_code(data, start);
        let end = next.unwrap_or(data.len());
        let nal = trim_trailing_zeros(data.get(start..end).unwrap_or(&[]));
        if !nal.is_empty() {
            out.push(nal);
        }
        match next {
            Some(pos) => start = pos + 3,
            None => break,
        }
    }
    out
}

/// Annex B access unit to an MP4 sample (4-byte big-endian length prefixes, AUDs dropped).
pub fn to_length_prefixed(data: &[u8], codec: VideoCodec) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    for nal in split_nals(data) {
        let Some(&header) = nal.first() else {
            continue;
        };
        let is_aud = match codec {
            VideoCodec::H264 => header & 0x1F == 9,
            VideoCodec::Hevc => (header >> 1) & 0x3F == 35,
        };
        if is_aud {
            continue;
        }
        let Ok(len) = u32::try_from(nal.len()) else {
            continue;
        };
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(nal);
    }
    out
}

/// Removes emulation-prevention bytes (00 00 03 -> 00 00).
///
/// Each `03` that follows two `00` bytes is dropped; the zero counter restarts after a drop.
pub fn unescape_rbsp(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0u32;
    for &b in nal {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        out.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
    out
}

/// True when the access unit contains an IDR/IRAP NAL (H.264 type 5; HEVC types 16..=21).
pub fn is_keyframe(data: &[u8], codec: VideoCodec) -> bool {
    split_nals(data).into_iter().any(|nal| match nal.first() {
        Some(&header) => match codec {
            VideoCodec::H264 => header & 0x1F == 5,
            VideoCodec::Hevc => (16..=21).contains(&((header >> 1) & 0x3F)),
        },
        None => false,
    })
}

/// MSB-first bit reader with exp-Golomb support over an unescaped RBSP.
#[derive(Debug, Clone)]
pub struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    /// Reader at bit 0 of `data`.
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// Reads `n` (0..=32) bits, MSB first. None past the end.
    pub fn read_bits(&mut self, n: u32) -> Option<u32> {
        if n > 32 {
            return None;
        }
        let n = n as usize;
        let end = self.pos.checked_add(n)?;
        if end > self.data.len().saturating_mul(8) {
            return None;
        }
        let mut value: u64 = 0;
        for _ in 0..n {
            let byte = *self.data.get(self.pos / 8)?;
            let bit = (byte >> (7 - self.pos % 8)) & 1;
            value = (value << 1) | u64::from(bit);
            self.pos += 1;
        }
        u32::try_from(value).ok()
    }

    /// Reads one bit.
    pub fn read_bit(&mut self) -> Option<bool> {
        self.read_bits(1).map(|v| v == 1)
    }

    /// Reads an unsigned exp-Golomb value.
    pub fn read_ue(&mut self) -> Option<u32> {
        let mut zeros: u32 = 0;
        while !self.read_bit()? {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        let suffix = self.read_bits(zeros)?;
        let value = (1u64 << zeros) - 1 + u64::from(suffix);
        u32::try_from(value).ok()
    }

    /// Reads a signed exp-Golomb value.
    pub fn read_se(&mut self) -> Option<i32> {
        let k = i64::from(self.read_ue()?);
        let value = if k % 2 == 1 { (k + 1) / 2 } else { -(k / 2) };
        i32::try_from(value).ok()
    }

    /// Skips `n` bits. None past the end.
    pub fn skip_bits(&mut self, n: u32) -> Option<()> {
        let end = self.pos.checked_add(n as usize)?;
        if end > self.data.len().saturating_mul(8) {
            return None;
        }
        self.pos = end;
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_three_and_four_byte_start_codes() {
        let data = [0xFF, 0, 0, 1, 0x67, 0xAA, 0, 0, 0, 1, 0x68, 0xBB, 0, 0];
        let nals = split_nals(&data);
        assert_eq!(nals, vec![&[0x67, 0xAA][..], &[0x68, 0xBB][..]]);
        assert!(split_nals(&[1, 2, 3]).is_empty());
    }

    #[test]
    fn unescapes_emulation_prevention() {
        assert_eq!(unescape_rbsp(&[0, 0, 3, 1]), vec![0, 0, 1]);
        assert_eq!(unescape_rbsp(&[0, 0, 3, 0, 0, 3]), vec![0, 0, 0, 0]);
    }

    #[test]
    fn exp_golomb_values() {
        // bits: 1 | 010 | 011 | 00100 -> ue 0, ue 1, ue 2, ue 3
        let data = [0b1010_0110, 0b0100_0000];
        let mut r = BitReader::new(&data);
        assert_eq!(r.read_ue(), Some(0));
        assert_eq!(r.read_ue(), Some(1));
        assert_eq!(r.read_ue(), Some(2));
        assert_eq!(r.read_ue(), Some(3));
        let mut r = BitReader::new(&[0x00]);
        assert_eq!(r.read_ue(), None);
    }
}
