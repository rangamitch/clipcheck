//! H.265 parameter sets and the hvcC record (worker M2).
//!
//! Every byte of input is untrusted: all parsing is bounds-checked and returns `None`
//! on any malformed or truncated data. The helpers here are private on purpose; the
//! Annex B / RBSP primitives in `annexb` are not used by this module.

const NAL_VPS: u8 = 32;
const NAL_SPS: u8 = 33;
const NAL_PPS: u8 = 34;

/// HEVC decoder configuration taken from an access unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HevcConfig {
    /// Video parameter sets (NAL type 32), NAL header included.
    pub vps: Vec<Vec<u8>>,
    /// Sequence parameter sets (NAL type 33), NAL header included.
    pub sps: Vec<Vec<u8>>,
    /// Picture parameter sets (NAL type 34), NAL header included.
    pub pps: Vec<Vec<u8>>,
    /// Display width in pixels (after the conformance window).
    pub width: u32,
    /// Display height in pixels (after the conformance window).
    pub height: u32,
}

/// Collects VPS/SPS/PPS from an Annex B access unit and parses the display size.
///
/// Identical duplicate NAL units are stored once. Returns `None` if any of the three
/// parameter-set kinds is missing, or if the first SPS is malformed.
pub fn hevc_config(au: &[u8]) -> Option<HevcConfig> {
    let mut vps: Vec<Vec<u8>> = Vec::new();
    let mut sps: Vec<Vec<u8>> = Vec::new();
    let mut pps: Vec<Vec<u8>> = Vec::new();
    for nal in split_nals(au) {
        if nal.len() < 2 {
            continue;
        }
        let list = match nal_type(nal) {
            NAL_VPS => &mut vps,
            NAL_SPS => &mut sps,
            NAL_PPS => &mut pps,
            _ => continue,
        };
        if !list.iter().any(|known| known.as_slice() == nal) {
            list.push(nal.to_vec());
        }
    }
    if vps.is_empty() || sps.is_empty() || pps.is_empty() {
        return None;
    }
    let info = parse_sps(sps.first()?)?;
    Some(HevcConfig {
        vps,
        sps,
        pps,
        width: info.width,
        height: info.height,
    })
}

/// HEVCDecoderConfigurationRecord payload (the body of an `hvcC` box).
///
/// Returns an empty `Vec` if there is no SPS, the first SPS does not parse, or a
/// list is too large for the record's 16-bit counts and lengths.
pub fn hvcc_payload(cfg: &HevcConfig) -> Vec<u8> {
    let Some(first_sps) = cfg.sps.first() else {
        return Vec::new();
    };
    let Some(info) = parse_sps(first_sps) else {
        return Vec::new();
    };
    build_hvcc(cfg, &info).unwrap_or_default()
}

fn build_hvcc(cfg: &HevcConfig, info: &SpsInfo) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    out.push(1); // configurationVersion
    out.push((info.profile_space << 6) | (info.tier << 5) | info.profile_idc);
    out.extend_from_slice(&info.compat.to_be_bytes());
    out.extend_from_slice(&info.constraint);
    out.push(info.level_idc);
    out.extend_from_slice(&[0xF0, 0x00]); // min_spatial_segmentation_idc = 0
    out.push(0xFC); // parallelismType = 0
    out.push(0xFC | (info.chroma_format_idc as u8 & 0x03));
    out.push(0xF8 | (info.bit_depth_luma_minus8 as u8 & 0x07));
    out.push(0xF8 | (info.bit_depth_chroma_minus8 as u8 & 0x07));
    out.extend_from_slice(&[0x00, 0x00]); // avgFrameRate
                                          // constantFrameRate 0, numTemporalLayers, temporalIdNested, lengthSizeMinusOne 3.
    let layers = (info.max_sub_layers_minus1 + 1) & 0x07;
    out.push((layers << 3) | ((info.temporal_id_nesting & 1) << 2) | 3);
    out.push(3); // numOfArrays
    for (nal_kind, list) in [
        (NAL_VPS, &cfg.vps),
        (NAL_SPS, &cfg.sps),
        (NAL_PPS, &cfg.pps),
    ] {
        out.push(0x80 | nal_kind);
        out.extend_from_slice(&u16::try_from(list.len()).ok()?.to_be_bytes());
        for nal in list {
            out.extend_from_slice(&u16::try_from(nal.len()).ok()?.to_be_bytes());
            out.extend_from_slice(nal);
        }
    }
    Some(out)
}

/// NAL unit type from the first header byte (forbidden bit and layer id ignored).
fn nal_type(nal: &[u8]) -> u8 {
    (nal.first().copied().unwrap_or(0) >> 1) & 0x3F
}

/// Splits an Annex B byte stream into NAL units (start codes removed).
///
/// Trailing zero bytes (the leading zero of a 4-byte start code, or trailing_zero_8bits)
/// are removed from each NAL unit. Emulation prevention bytes are left in place.
fn split_nals(data: &[u8]) -> Vec<&[u8]> {
    let mut payload_starts = Vec::new();
    let mut i = 0usize;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            payload_starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::with_capacity(payload_starts.len());
    for (k, &begin) in payload_starts.iter().enumerate() {
        let end = match payload_starts.get(k + 1) {
            Some(&next) => next - 3,
            None => data.len(),
        };
        let Some(raw) = data.get(begin..end) else {
            continue;
        };
        let kept = raw.iter().rposition(|&b| b != 0).map_or(0, |p| p + 1);
        if let Some(nal) = raw.get(..kept) {
            out.push(nal);
        }
    }
    out
}

/// Removes emulation prevention bytes: a 0x03 following two zero bytes is dropped.
fn unescape(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut zeros = 0u32;
    for &b in data {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        out.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
    out
}

/// MSB-first bit reader over an RBSP. Every read is bounds-checked.
struct BitReader<'a> {
    data: &'a [u8],
    /// Position in bits from the start of `data`.
    pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn bit(&mut self) -> Option<u32> {
        let byte = *self.data.get(self.pos / 8)?;
        let bit = (byte >> (7 - (self.pos % 8))) & 1;
        self.pos += 1;
        Some(u32::from(bit))
    }

    /// Reads `n` bits (0..=32) as an unsigned value.
    fn bits(&mut self, n: u32) -> Option<u32> {
        if n > 32 {
            return None;
        }
        let mut value = 0u32;
        for _ in 0..n {
            value = (value << 1) | self.bit()?;
        }
        Some(value)
    }

    /// Unsigned Exp-Golomb code ue(v).
    fn ue(&mut self) -> Option<u32> {
        let mut leading = 0u32;
        while self.bit()? == 0 {
            leading += 1;
            if leading > 31 {
                return None;
            }
        }
        let suffix = self.bits(leading)?;
        let value = (1u64 << leading) - 1 + u64::from(suffix);
        u32::try_from(value).ok()
    }

    /// Skips `n` bits, failing if that runs past the end of the data.
    fn skip(&mut self, n: usize) -> Option<()> {
        let end = self.pos.checked_add(n)?;
        if end > self.data.len().checked_mul(8)? {
            return None;
        }
        self.pos = end;
        Some(())
    }
}

/// Fields of the first SPS needed for the hvcC record and the display size.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SpsInfo {
    profile_space: u8,
    tier: u8,
    profile_idc: u8,
    compat: u32,
    /// The 48 general constraint indicator flag bits, copied verbatim.
    constraint: [u8; 6],
    level_idc: u8,
    max_sub_layers_minus1: u8,
    temporal_id_nesting: u8,
    chroma_format_idc: u32,
    bit_depth_luma_minus8: u32,
    bit_depth_chroma_minus8: u32,
    width: u32,
    height: u32,
}

/// Parses an SPS NAL unit (header included) down to the display size.
fn parse_sps(nal: &[u8]) -> Option<SpsInfo> {
    let rbsp = unescape(nal.get(2..)?);
    let mut r = BitReader::new(&rbsp);

    r.bits(4)?; // sps_video_parameter_set_id
    let max_sub_layers_minus1 = r.bits(3)?;
    if max_sub_layers_minus1 > 6 {
        return None;
    }
    let temporal_id_nesting = r.bit()?;

    // profile_tier_level(1, max_sub_layers_minus1)
    let profile_space = r.bits(2)? as u8;
    let tier = r.bit()? as u8;
    let profile_idc = r.bits(5)? as u8;
    let compat = r.bits(32)?;
    let mut constraint = [0u8; 6];
    for byte in constraint.iter_mut() {
        *byte = r.bits(8)? as u8;
    }
    let level_idc = r.bits(8)? as u8;

    let mut sub_layer_flags = Vec::new();
    for _ in 0..max_sub_layers_minus1 {
        let profile_present = r.bit()? == 1;
        let level_present = r.bit()? == 1;
        sub_layer_flags.push((profile_present, level_present));
    }
    if max_sub_layers_minus1 > 0 {
        for _ in max_sub_layers_minus1..8 {
            r.bits(2)?; // reserved_zero_2bits
        }
    }
    for (profile_present, level_present) in sub_layer_flags {
        if profile_present {
            r.skip(88)?;
        }
        if level_present {
            r.skip(8)?;
        }
    }

    r.ue()?; // sps_seq_parameter_set_id
    let chroma_format_idc = r.ue()?;
    if chroma_format_idc > 3 {
        return None;
    }
    let mut separate_colour_plane = false;
    if chroma_format_idc == 3 {
        separate_colour_plane = r.bit()? == 1;
    }
    let pic_width = r.ue()?;
    let pic_height = r.ue()?;
    let (mut left, mut right, mut top, mut bottom) = (0u32, 0u32, 0u32, 0u32);
    if r.bit()? == 1 {
        left = r.ue()?;
        right = r.ue()?;
        top = r.ue()?;
        bottom = r.ue()?;
    }
    let bit_depth_luma_minus8 = r.ue()?;
    let bit_depth_chroma_minus8 = r.ue()?;
    if bit_depth_luma_minus8 > 6 || bit_depth_chroma_minus8 > 6 {
        return None;
    }

    // SubWidthC / SubHeightC (Table 6-1); monochrome and separate planes use 1.
    let (sub_width, sub_height) = match (chroma_format_idc, separate_colour_plane) {
        (1, false) => (2u32, 2u32),
        (2, false) => (2, 1),
        _ => (1, 1),
    };
    let crop_w = sub_width.checked_mul(left.checked_add(right)?)?;
    let crop_h = sub_height.checked_mul(top.checked_add(bottom)?)?;
    let width = pic_width.checked_sub(crop_w)?;
    let height = pic_height.checked_sub(crop_h)?;
    if width == 0 || height == 0 {
        return None;
    }

    Some(SpsInfo {
        profile_space,
        tier,
        profile_idc,
        compat,
        constraint,
        level_idc,
        max_sub_layers_minus1: max_sub_layers_minus1 as u8,
        temporal_id_nesting: temporal_id_nesting as u8,
        chroma_format_idc,
        bit_depth_luma_minus8,
        bit_depth_chroma_minus8,
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal MSB-first bit writer for building synthetic parameter sets.
    struct Bits {
        out: Vec<u8>,
        len: usize,
    }

    impl Bits {
        fn new() -> Self {
            Self {
                out: Vec::new(),
                len: 0,
            }
        }

        fn put(&mut self, value: u32, bits: u32) {
            for i in (0..bits).rev() {
                if self.len.is_multiple_of(8) {
                    self.out.push(0);
                }
                if (value >> i) & 1 == 1 {
                    if let Some(last) = self.out.last_mut() {
                        *last |= 0x80 >> (self.len % 8);
                    }
                }
                self.len += 1;
            }
        }

        fn ue(&mut self, value: u32) {
            let x = u64::from(value) + 1;
            let len = 64 - x.leading_zeros();
            self.put(0, len - 1);
            for i in (0..len).rev() {
                self.put(((x >> i) & 1) as u32, 1);
            }
        }
    }

    /// Inserts emulation prevention bytes.
    fn escape(rbsp: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut zeros = 0;
        for &b in rbsp {
            if zeros >= 2 && b <= 3 {
                out.push(3);
                zeros = 0;
            }
            out.push(b);
            zeros = if b == 0 { zeros + 1 } else { 0 };
        }
        out
    }

    /// Synthetic 1920x1088 SPS (bottom crop of 4 units -> 1920x1080), 4:2:0 8-bit.
    /// `max_sub_layers` > 0 exercises the sub-layer PTL path (level flags only).
    fn sps_nal(max_sub_layers: u32) -> Vec<u8> {
        let mut b = Bits::new();
        b.put(0, 4); // sps_video_parameter_set_id
        b.put(max_sub_layers, 3);
        b.put(1, 1); // temporal_id_nesting
        b.put(0, 2); // general_profile_space
        b.put(0, 1); // general_tier_flag
        b.put(1, 5); // general_profile_idc (Main)
        b.put(0x6000_0000, 32); // compat flags
        for byte in [0x90u32, 0, 0, 0, 0, 0] {
            b.put(byte, 8); // constraint flags
        }
        b.put(93, 8); // general_level_idc
        if max_sub_layers > 0 {
            for _ in 0..max_sub_layers {
                b.put(0, 1); // sub_layer_profile_present
                b.put(1, 1); // sub_layer_level_present
            }
            for _ in max_sub_layers..8 {
                b.put(0, 2);
            }
            for _ in 0..max_sub_layers {
                b.put(90, 8);
            }
        }
        b.ue(0); // sps_seq_parameter_set_id
        b.ue(1); // chroma_format_idc
        b.ue(1920); // pic_width
        b.ue(1088); // pic_height
        b.put(1, 1); // conformance_window_flag
        b.ue(0);
        b.ue(0);
        b.ue(0);
        b.ue(4); // bottom: 4 * SubHeightC(2) = 8 rows
        b.ue(0); // bit_depth_luma_minus8
        b.ue(0); // bit_depth_chroma_minus8
        b.put(1, 1); // rbsp stop bit
        let mut nal = vec![0x42, 0x01];
        nal.extend(escape(&b.out));
        nal
    }

    fn vps_nal() -> Vec<u8> {
        vec![0x40, 0x01, 0x0C, 0x01, 0xFF, 0xFF, 0x01, 0x60, 0x00, 0x00]
    }

    fn pps_nal() -> Vec<u8> {
        vec![0x44, 0x01, 0xC1, 0x72, 0xB4, 0x62, 0x40]
    }

    fn annexb(parts: &[Vec<u8>]) -> Vec<u8> {
        let mut au = Vec::new();
        for p in parts {
            au.extend_from_slice(&[0, 0, 0, 1]);
            au.extend_from_slice(p);
        }
        au
    }

    #[test]
    fn parses_display_size_with_and_without_sub_layers() {
        for max_sub in [0, 1] {
            let au = annexb(&[vps_nal(), sps_nal(max_sub), pps_nal()]);
            let cfg = hevc_config(&au).expect("config");
            assert_eq!((cfg.width, cfg.height), (1920, 1080));
            assert_eq!(cfg.vps.len(), 1);
            assert_eq!(cfg.sps.len(), 1);
            assert_eq!(cfg.pps.len(), 1);
        }
    }

    #[test]
    fn duplicate_parameter_sets_are_stored_once() {
        let au = annexb(&[vps_nal(), sps_nal(0), pps_nal(), sps_nal(0), pps_nal()]);
        let cfg = hevc_config(&au).expect("config");
        assert_eq!(cfg.sps.len(), 1);
        assert_eq!(cfg.pps.len(), 1);
    }

    #[test]
    fn missing_parameter_set_gives_none() {
        assert!(hevc_config(&annexb(&[vps_nal(), sps_nal(0)])).is_none());
        assert!(hevc_config(&[]).is_none());
        assert!(hevc_config(&[1, 2, 3]).is_none());
    }

    #[test]
    fn hvcc_header_fields() {
        let au = annexb(&[vps_nal(), sps_nal(0), pps_nal()]);
        let cfg = hevc_config(&au).expect("config");
        let p = hvcc_payload(&cfg);
        assert_eq!(p[0], 1, "configurationVersion");
        assert_eq!(p[1], 1, "profile_idc");
        assert_eq!(&p[2..6], &0x6000_0000u32.to_be_bytes());
        assert_eq!(p[6], 0x90, "constraint flags copied verbatim");
        assert_eq!(p[12], 93, "general_level_idc");
        assert_eq!(&p[13..15], &[0xF0, 0x00]);
        assert_eq!(p[15], 0xFC);
        assert_eq!(p[16], 0xFD, "chroma_format_idc 1");
        assert_eq!(p[17], 0xF8, "bit depth luma 8");
        assert_eq!(p[18], 0xF8, "bit depth chroma 8");
        assert_eq!(p[21], 0x0F, "1 layer, nested, lengthSizeMinusOne 3");
        assert_eq!(p[22], 3, "numOfArrays");
        assert_eq!(p[23], 0xA0, "VPS array header");
        assert_eq!(&p[24..26], &[0, 1], "one VPS");
    }

    #[test]
    fn hvcc_empty_without_sps() {
        let cfg = HevcConfig {
            vps: vec![],
            sps: vec![],
            pps: vec![],
            width: 0,
            height: 0,
        };
        assert!(hvcc_payload(&cfg).is_empty());
    }

    #[test]
    fn truncated_and_garbage_input_never_panics() {
        let au = annexb(&[vps_nal(), sps_nal(1), pps_nal()]);
        for len in 0..au.len() {
            if let Some(cfg) = hevc_config(&au[..len]) {
                let _ = hvcc_payload(&cfg);
            }
        }
        for seed in 0u32..256 {
            let junk: Vec<u8> = (0..64u32)
                .map(|i| ((i.wrapping_mul(seed + 7) ^ seed) & 0xFF) as u8)
                .collect();
            let mut framed = annexb(&[vec![0x40, 0x01], vec![0x42, 0x01]]);
            framed.extend_from_slice(&junk);
            let _ = hevc_config(&framed);
        }
    }
}
