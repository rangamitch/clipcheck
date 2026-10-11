//! H.264 parameter sets and the avcC record (worker M1).

use crate::annexb::{split_nals, unescape_rbsp, BitReader};

/// H.264 decoder configuration taken from an access unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvcConfig {
    /// Sequence parameter sets (NAL type 7), NAL header included, escaped as in the stream.
    pub sps: Vec<Vec<u8>>,
    /// Picture parameter sets (NAL type 8), NAL header included.
    pub pps: Vec<Vec<u8>>,
    /// Display width in pixels (after cropping).
    pub width: u32,
    /// Display height in pixels (after cropping).
    pub height: u32,
}

/// Fields of an SPS that the rest of the crate needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SpsInfo {
    chroma_format_idc: u32,
    bit_depth_luma_minus8: u32,
    bit_depth_chroma_minus8: u32,
    width: u32,
    height: u32,
}

/// profile_idc values whose SPS carries the chroma/bit-depth/scaling-list syntax.
const HIGH_PROFILES: [u32; 13] = [100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135];

/// Collects SPS/PPS from an Annex B access unit and parses the display size.
///
/// Identical parameter sets are kept once. None if either set is missing or the first SPS is malformed.
pub fn avc_config(au: &[u8]) -> Option<AvcConfig> {
    let mut sps: Vec<Vec<u8>> = Vec::new();
    let mut pps: Vec<Vec<u8>> = Vec::new();
    for nal in split_nals(au) {
        match nal.first().map(|h| h & 0x1F) {
            Some(7) if !sps.iter().any(|s| s == nal) => sps.push(nal.to_vec()),
            Some(8) if !pps.iter().any(|p| p == nal) => pps.push(nal.to_vec()),
            _ => {}
        }
    }
    if sps.is_empty() || pps.is_empty() {
        return None;
    }
    let info = parse_sps(sps.first()?)?;
    Some(AvcConfig {
        sps,
        pps,
        width: info.width,
        height: info.height,
    })
}

/// AVCDecoderConfigurationRecord payload (the body of an `avcC` box).
///
/// Empty when the config has no SPS. The chroma/bit-depth tail is written for profiles 100, 110, 122
/// and 144; defaults (1, 0, 0) are used if the first SPS cannot be parsed for them.
pub fn avcc_payload(cfg: &AvcConfig) -> Vec<u8> {
    let Some(sps0) = cfg.sps.first() else {
        return Vec::new();
    };
    let byte = |i: usize| sps0.get(i).copied().unwrap_or(0);
    let profile = byte(1);
    let mut out = vec![1, profile, byte(2), byte(3), 0xFF];

    let sps_list: Vec<&Vec<u8>> = cfg.sps.iter().take(31).collect();
    out.push(0xE0 | sps_list.len() as u8);
    for s in &sps_list {
        push_param_set(&mut out, s);
    }

    let pps_list: Vec<&Vec<u8>> = cfg.pps.iter().take(255).collect();
    out.push(pps_list.len() as u8);
    for p in &pps_list {
        push_param_set(&mut out, p);
    }

    if matches!(profile, 100 | 110 | 122 | 144) {
        let (chroma, luma_depth, chroma_depth) = match parse_sps(sps0) {
            Some(info) => (
                info.chroma_format_idc,
                info.bit_depth_luma_minus8,
                info.bit_depth_chroma_minus8,
            ),
            None => (1, 0, 0),
        };
        out.push(0xFC | (chroma & 0x03) as u8);
        out.push(0xF8 | (luma_depth & 0x07) as u8);
        out.push(0xF8 | (chroma_depth & 0x07) as u8);
        out.push(0x00);
    }
    out
}

/// Appends a u16 big-endian length followed by the bytes. Oversized sets are skipped.
fn push_param_set(out: &mut Vec<u8>, bytes: &[u8]) {
    if let Ok(len) = u16::try_from(bytes.len()) {
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(bytes);
    }
}

/// Parses an SPS NAL unit (header byte included) for the fields listed in [`SpsInfo`].
fn parse_sps(nal: &[u8]) -> Option<SpsInfo> {
    let rbsp = unescape_rbsp(nal.get(1..)?);
    let mut r = BitReader::new(&rbsp);
    let profile_idc = r.read_bits(8)?;
    let _constraint_flags = r.read_bits(8)?;
    let _level_idc = r.read_bits(8)?;
    let _sps_id = r.read_ue()?;

    let (chroma_format_idc, bit_depth_luma_minus8, bit_depth_chroma_minus8) =
        if HIGH_PROFILES.contains(&profile_idc) {
            let chroma = r.read_ue()?;
            if chroma > 3 {
                return None;
            }
            if chroma == 3 {
                let _separate_colour_plane = r.read_bit()?;
            }
            let luma_depth = r.read_ue()?;
            let chroma_depth = r.read_ue()?;
            if luma_depth > 7 || chroma_depth > 7 {
                return None;
            }
            let _qpprime_bypass = r.read_bit()?;
            if r.read_bit()? {
                let lists = if chroma == 3 { 12 } else { 8 };
                for i in 0..lists {
                    if r.read_bit()? {
                        skip_scaling_list(&mut r, if i < 6 { 16 } else { 64 })?;
                    }
                }
            }
            (chroma, luma_depth, chroma_depth)
        } else {
            (1, 0, 0)
        };

    let _log2_max_frame_num_minus4 = r.read_ue()?;
    let poc_type = r.read_ue()?;
    match poc_type {
        0 => {
            let _log2_max_poc_lsb_minus4 = r.read_ue()?;
        }
        1 => {
            let _delta_always_zero = r.read_bit()?;
            let _offset_for_non_ref_pic = r.read_se()?;
            let _offset_for_top_to_bottom = r.read_se()?;
            let cycle_len = r.read_ue()?;
            for _ in 0..cycle_len {
                let _offset_for_ref_frame = r.read_se()?;
            }
        }
        2 => {}
        _ => return None,
    }

    let _max_num_ref_frames = r.read_ue()?;
    let _gaps_allowed = r.read_bit()?;
    let width_mbs_minus1 = u64::from(r.read_ue()?);
    let height_map_units_minus1 = u64::from(r.read_ue()?);
    let frame_mbs_only = r.read_bit()?;
    if !frame_mbs_only {
        let _mb_adaptive_frame_field = r.read_bit()?;
    }
    let _direct_8x8_inference = r.read_bit()?;
    let cropping = r.read_bit()?;
    let (crop_left, crop_right, crop_top, crop_bottom) = if cropping {
        (
            u64::from(r.read_ue()?),
            u64::from(r.read_ue()?),
            u64::from(r.read_ue()?),
            u64::from(r.read_ue()?),
        )
    } else {
        (0, 0, 0, 0)
    };

    let fmo = u64::from(frame_mbs_only);
    let (crop_unit_x, crop_unit_y) = match chroma_format_idc {
        1 => (2u64, 2 * (2 - fmo)),
        2 => (2u64, 2 - fmo),
        _ => (1u64, 2 - fmo),
    };

    let full_width = width_mbs_minus1.checked_add(1)?.checked_mul(16)?;
    let crop_width = crop_unit_x.checked_mul(crop_left.checked_add(crop_right)?)?;
    let width = full_width.checked_sub(crop_width)?;

    let full_height = (2 - fmo)
        .checked_mul(height_map_units_minus1.checked_add(1)?)?
        .checked_mul(16)?;
    let crop_height = crop_unit_y.checked_mul(crop_top.checked_add(crop_bottom)?)?;
    let height = full_height.checked_sub(crop_height)?;

    let width = u32::try_from(width).ok()?;
    let height = u32::try_from(height).ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    Some(SpsInfo {
        chroma_format_idc,
        bit_depth_luma_minus8,
        bit_depth_chroma_minus8,
        width,
        height,
    })
}

/// Consumes one scaling_list() (ISO 14496-10 7.3.2.1.1.1) of `size` entries.
fn skip_scaling_list(r: &mut BitReader<'_>, size: u32) -> Option<()> {
    let mut last_scale: i32 = 8;
    let mut next_scale: i32 = 8;
    for _ in 0..size {
        if next_scale != 0 {
            let delta = r.read_se()?;
            next_scale = last_scale
                .checked_add(delta)?
                .checked_add(256)?
                .rem_euclid(256);
        }
        if next_scale != 0 {
            last_scale = next_scale;
        }
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1280x720 High profile SPS (x264 style), start code stripped.
    const SPS_720P: [u8; 26] = [
        0x67, 0x64, 0x00, 0x1F, 0xAC, 0xD9, 0x40, 0x50, 0x05, 0xBB, 0x01, 0x10, 0x00, 0x00, 0x03,
        0x00, 0x10, 0x00, 0x00, 0x03, 0x03, 0xC0, 0xF1, 0x83, 0x19, 0x60,
    ];
    const PPS: [u8; 4] = [0x68, 0xEB, 0xE3, 0xCB];

    #[test]
    fn parses_720p_sps_and_builds_avcc() {
        let mut au = vec![0, 0, 0, 1];
        au.extend_from_slice(&SPS_720P);
        au.extend_from_slice(&[0, 0, 1]);
        au.extend_from_slice(&PPS);
        let cfg = avc_config(&au).expect("config");
        assert_eq!((cfg.width, cfg.height), (1280, 720));
        assert_eq!(cfg.sps, vec![SPS_720P.to_vec()]);
        assert_eq!(cfg.pps, vec![PPS.to_vec()]);
        let payload = avcc_payload(&cfg);
        assert_eq!(&payload[..4], &[1, 0x64, 0x00, 0x1F]);
        assert_eq!(payload[4], 0xFF);
        assert_eq!(payload[5], 0xE1);
        assert_eq!(&payload[6..8], &[0, 26]);
    }

    #[test]
    fn missing_pps_yields_none() {
        assert!(avc_config(&[0, 0, 1, 0x67, 0x64, 0x00, 0x1F]).is_none());
    }
}
