//! Black-box tests for `annexb`, `avc` and `aac` in clipcheck-mp4.
//!
//! Real-stream oracle tests generate H.264 with `clipcheck-testkit` (native encoder) and are
//! skipped (with an `eprintln!("SKIPPED: ...")`) when native test media is unavailable.

use clipcheck_mp4::annexb::{self, BitReader};
use clipcheck_mp4::avc::{self, AvcConfig};
use clipcheck_mp4::reader::{read_movie, TrackKind};
use clipcheck_mp4::{aac, VideoCodec};
use clipcheck_testkit::MediaSpec;

const H264: VideoCodec = VideoCodec::H264;
const HEVC: VideoCodec = VideoCodec::Hevc;

// ---------------------------------------------------------------- helpers

/// Builds an Annex B stream with 4-byte start codes.
fn annexb_from(nals: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for n in nals {
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(n);
    }
    out
}

/// Deterministic LCG (no rand crate).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    fn byte(&mut self) -> u8 {
        (self.next() & 0xFF) as u8
    }

    /// Bytes biased toward start codes, emulation prevention and NAL headers.
    fn biased_byte(&mut self) -> u8 {
        const ALPHA: [u8; 12] = [0, 0, 0, 1, 3, 0x67, 0x68, 0x65, 0x41, 0x09, 0x46, 0x26];
        if self.next().is_multiple_of(4) {
            self.byte()
        } else {
            ALPHA[(self.next() % ALPHA.len() as u64) as usize]
        }
    }
}

/// Reads one `u16`-length-prefixed parameter set from an `avcC` payload at `*j`.
fn take_param_set(p: &[u8], j: &mut usize) -> Option<Vec<u8>> {
    let l = u16::from_be_bytes([*p.get(*j)?, *p.get(*j + 1)?]) as usize;
    let set = p.get(*j + 2..*j + 2 + l)?.to_vec();
    *j += 2 + l;
    Some(set)
}

/// (lengthSizeMinusOne + 1, SPS list, PPS list) from an `avcC` box.
type AvcParamSets = (usize, Vec<Vec<u8>>, Vec<Vec<u8>>);

/// Finds the `avcC` child of an `avc1` sample entry (full box bytes).
fn avcc_from_sample_entry(entry: &[u8]) -> Option<AvcParamSets> {
    // 8-byte box header + 78 bytes of visual sample entry fields, then child boxes.
    let mut i = 86;
    while i + 8 <= entry.len() {
        let size =
            u32::from_be_bytes([entry[i], entry[i + 1], entry[i + 2], entry[i + 3]]) as usize;
        if size < 8 || i + size > entry.len() {
            return None;
        }
        if &entry[i + 4..i + 8] == b"avcC" {
            let p = &entry[i + 8..i + size];
            if p.len() < 7 {
                return None;
            }
            let length_size = (p[4] & 0x03) as usize + 1;
            let mut j = 6;
            let mut sps = Vec::new();
            for _ in 0..(p[5] & 0x1F) {
                sps.push(take_param_set(p, &mut j)?);
            }
            let npps = *p.get(j)? as usize;
            j += 1;
            let mut pps = Vec::new();
            for _ in 0..npps {
                pps.push(take_param_set(p, &mut j)?);
            }
            return Some((length_size, sps, pps));
        }
        i += size;
    }
    None
}

/// Generates a real H.264 Annex B stream of 5 frames at `width`x`height` with the native
/// encoder. The clip is written as a flat MP4 and converted back: each length-prefixed sample
/// becomes start-code-delimited NALs, and the SPS/PPS from `avcC` are prepended to keyframes.
/// Returns None (after printing SKIPPED) when native test media is unavailable.
fn gen_h264(width: u32, height: u32) -> Option<Vec<u8>> {
    if !clipcheck_testkit::available() {
        eprintln!("SKIPPED: native test media unavailable");
        return None;
    }
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("stream.mp4");
    let spec = MediaSpec::h264_aac(5.0 / 30.0)
        .with_video(width, height, 30)
        .with_audio(Vec::new());
    clipcheck_testkit::write_mp4(&path, &spec).expect("testkit writes H.264 MP4");
    let bytes = std::fs::read(&path).expect("read generated MP4");
    let movie = read_movie(&path).expect("parse generated MP4");
    let track = movie
        .tracks
        .iter()
        .find(|t| matches!(t.kind, TrackKind::Video { .. }))
        .expect("video track");
    let entry = track.sample_entries.first().expect("sample entry");
    let (length_size, sps, pps) = avcc_from_sample_entry(entry).expect("avcC in sample entry");
    assert_eq!(length_size, 4, "testkit writes 4-byte NAL length prefixes");

    let mut out = Vec::new();
    for s in &track.samples {
        let data = &bytes[s.offset as usize..s.offset as usize + s.size as usize];
        if s.keyframe {
            for p in sps.iter().chain(pps.iter()) {
                out.extend_from_slice(&[0, 0, 0, 1]);
                out.extend_from_slice(p);
            }
        }
        let mut i = 0;
        while i + 4 <= data.len() {
            let len = u32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as usize;
            i += 4;
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(data.get(i..i + len).expect("sample NAL in bounds"));
            i += len;
        }
    }
    Some(out)
}

/// Checks the avcC tail after the PPS block. High profiles (100/110/122/144) must carry 4:2:0
/// 8-bit chroma/luma fields; other profiles must have no tail.
fn check_profile_tail(cfg: &AvcConfig, payload: &[u8], n: usize) {
    match cfg.sps[0][1] {
        100 | 110 | 122 | 144 => assert_eq!(
            &payload[n..],
            &[0xFD, 0xF8, 0xF8, 0],
            "4:2:0 8-bit tail for profile {}",
            cfg.sps[0][1]
        ),
        p => assert_eq!(payload.len(), n, "no tail for profile {p}"),
    }
}

fn nal_type_h264(nal: &[u8]) -> u8 {
    nal[0] & 0x1F
}

/// Walks a 4-byte length-prefixed buffer and returns the NAL payloads.
fn walk_length_prefixed(buf: &[u8]) -> Option<Vec<&[u8]>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < buf.len() {
        if i + 4 > buf.len() {
            return None;
        }
        let len = u32::from_be_bytes([buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]) as usize;
        i += 4;
        if i + len > buf.len() {
            return None;
        }
        out.push(&buf[i..i + len]);
        i += len;
    }
    Some(out)
}

/// Checks the structural invariants of an avcC payload produced from `cfg`.
/// Returns the offset just past the PPS block.
fn check_avcc_structure(cfg: &AvcConfig, payload: &[u8]) -> usize {
    assert!(payload.len() >= 7, "payload too short: {}", payload.len());
    assert_eq!(payload[0], 1, "configurationVersion");
    assert_eq!(&payload[1..4], &cfg.sps[0][1..4], "profile/compat/level");
    assert_eq!(payload[4], 0xFF, "lengthSizeMinusOne byte");
    assert_eq!(payload[5], 0xE0 | cfg.sps.len() as u8, "numSPS byte");
    let mut i = 6;
    for s in &cfg.sps {
        let l = u16::from_be_bytes([payload[i], payload[i + 1]]) as usize;
        assert_eq!(l, s.len(), "sps length");
        assert_eq!(&payload[i + 2..i + 2 + l], s.as_slice(), "sps bytes");
        i += 2 + l;
    }
    assert_eq!(payload[i] as usize, cfg.pps.len(), "numPPS byte");
    i += 1;
    for p in &cfg.pps {
        let l = u16::from_be_bytes([payload[i], payload[i + 1]]) as usize;
        assert_eq!(l, p.len(), "pps length");
        assert_eq!(&payload[i + 2..i + 2 + l], p.as_slice(), "pps bytes");
        i += 2 + l;
    }
    i
}

// ---------------------------------------------------------------- split_nals

#[test]
fn split_mixed_three_and_four_byte_start_codes() {
    let data = [
        0, 0, 1, 0x67, 1, 2, // 3-byte start code, SPS
        0, 0, 0, 1, 0x68, 3, // 4-byte start code, PPS
        0, 0, 1, 0x65, 4, 5, // 3-byte start code, IDR
    ];
    let nals = annexb::split_nals(&data);
    assert_eq!(
        nals,
        vec![&[0x67u8, 1, 2][..], &[0x68u8, 3][..], &[0x65u8, 4, 5][..]]
    );
}

#[test]
fn split_strips_trailing_zero_bytes() {
    let data = [0, 0, 1, 0x67, 1, 0, 0, 0, 0, 1, 0x68, 2, 0];
    let nals = annexb::split_nals(&data);
    assert_eq!(nals, vec![&[0x67u8, 1][..], &[0x68u8, 2][..]]);
}

#[test]
fn split_ignores_garbage_before_first_start_code() {
    let data = [0xFF, 0x12, 0x34, 0, 0, 1, 0x67, 1];
    assert_eq!(annexb::split_nals(&data), vec![&[0x67u8, 1][..]]);
}

#[test]
fn split_empty_input_and_no_start_code() {
    assert!(annexb::split_nals(&[]).is_empty());
    assert!(annexb::split_nals(&[0x67, 1, 2, 3]).is_empty());
}

#[test]
fn split_only_start_codes_yields_nothing() {
    let data = [0, 0, 1, 0, 0, 1, 0, 0, 0, 1];
    assert!(annexb::split_nals(&data).is_empty());
}

// ---------------------------------------------------------- to_length_prefixed

#[test]
fn length_prefix_h264_values() {
    let data = [0, 0, 1, 0x67, 0xAA, 0xBB, 0, 0, 1, 0x65, 0xCC];
    let out = annexb::to_length_prefixed(&data, H264);
    assert_eq!(
        out,
        vec![0, 0, 0, 3, 0x67, 0xAA, 0xBB, 0, 0, 0, 2, 0x65, 0xCC]
    );
}

#[test]
fn length_prefix_drops_h264_aud() {
    // type 9 = AUD (0x09 and 0x29 both have type 9)
    let data = [
        0, 0, 0, 1, 0x09, 0xF0, 0, 0, 1, 0x29, 0x10, 0, 0, 1, 0x67, 0x01,
    ];
    let out = annexb::to_length_prefixed(&data, H264);
    assert_eq!(out, vec![0, 0, 0, 2, 0x67, 0x01]);
}

#[test]
fn length_prefix_keeps_h264_non_aud_nal_with_similar_header() {
    // 0x46 is type 6 (SEI) for H.264; it must not be dropped.
    let data = [0, 0, 1, 0x46, 0x01, 0x02];
    let out = annexb::to_length_prefixed(&data, H264);
    assert_eq!(out, vec![0, 0, 0, 3, 0x46, 0x01, 0x02]);
}

#[test]
fn length_prefix_drops_hevc_aud_only() {
    // HEVC AUD = type 35 -> header byte 35 << 1 = 0x46.
    // 0x09 is type 4 for HEVC (RASL_N), so it must be kept.
    let data = [
        0, 0, 1, 0x46, 0x01, 0x50, 0, 0, 1, 0x40, 0x01, 0x0C, 0, 0, 1, 0x09, 0x02,
    ];
    let out = annexb::to_length_prefixed(&data, HEVC);
    assert_eq!(
        out,
        vec![0, 0, 0, 3, 0x40, 0x01, 0x0C, 0, 0, 0, 2, 0x09, 0x02]
    );
}

#[test]
fn length_prefix_empty_input() {
    assert!(annexb::to_length_prefixed(&[], H264).is_empty());
}

// ------------------------------------------------------------- unescape_rbsp

#[test]
fn unescape_removes_emulation_prevention() {
    assert_eq!(
        annexb::unescape_rbsp(&[0, 0, 3, 3, 0x11]),
        vec![0, 0, 3, 0x11]
    );
    assert_eq!(annexb::unescape_rbsp(&[0, 0, 3, 0]), vec![0, 0, 0]);
    assert_eq!(annexb::unescape_rbsp(&[0, 0, 3, 1]), vec![0, 0, 1]);
}

#[test]
fn unescape_leaves_plain_bytes_untouched() {
    assert_eq!(annexb::unescape_rbsp(&[1, 2, 3, 0, 1]), vec![1, 2, 3, 0, 1]);
    assert!(annexb::unescape_rbsp(&[]).is_empty());
}

// ---------------------------------------------------------------- is_keyframe

#[test]
fn is_keyframe_h264_idr_true_non_idr_false() {
    assert!(annexb::is_keyframe(&[0, 0, 1, 0x65, 0x88], H264));
    assert!(annexb::is_keyframe(
        &[0, 0, 0, 1, 0x67, 1, 0, 0, 0, 1, 0x68, 2, 0, 0, 1, 0x65, 0x88],
        H264
    ));
    assert!(!annexb::is_keyframe(&[0, 0, 1, 0x41, 0x9A], H264));
    assert!(!annexb::is_keyframe(&[0, 0, 1, 0x67, 1, 2], H264));
    assert!(!annexb::is_keyframe(&[], H264));
}

#[test]
fn is_keyframe_hevc_irap_range() {
    // type t -> header byte t << 1
    for t in [16u8, 17, 18, 19, 20, 21] {
        assert!(
            annexb::is_keyframe(&[0, 0, 1, t << 1, 0x01], HEVC),
            "type {t}"
        );
    }
    for t in [0u8, 1, 15, 22, 32, 33, 34] {
        assert!(
            !annexb::is_keyframe(&[0, 0, 1, t << 1, 0x01], HEVC),
            "type {t}"
        );
    }
}

// ---------------------------------------------------------------- BitReader

#[test]
fn bitreader_exp_golomb_known_values() {
    let cases: [(&[u8], u32); 4] = [
        (&[0b1000_0000], 0),
        (&[0b0100_0000], 1),
        (&[0b0110_0000], 2),
        (&[0b0010_0000], 3),
    ];
    for (bytes, want) in cases {
        let mut r = BitReader::new(bytes);
        assert_eq!(r.read_ue(), Some(want), "bytes {bytes:02x?}");
    }
}

#[test]
fn bitreader_se_mapping() {
    // 010 (ue 1) 011 (ue 2) 00100 (ue 3) 00101 (ue 4) -> se 1, -1, 2, -2
    let mut r = BitReader::new(&[0b0100_1100, 0b1000_0101, 0b0000_1000]);
    assert_eq!(r.read_se(), Some(1));
    assert_eq!(r.read_se(), Some(-1));
    assert_eq!(r.read_se(), Some(2));
    assert_eq!(r.read_se(), Some(-2));
}

#[test]
fn bitreader_read_bits_msb_first() {
    let mut r = BitReader::new(&[0xAB, 0xCD]);
    assert_eq!(r.read_bits(4), Some(0xA));
    assert_eq!(r.read_bits(4), Some(0xB));
    assert_eq!(r.read_bits(8), Some(0xCD));
}

#[test]
fn bitreader_read_bit_and_past_end() {
    let mut r = BitReader::new(&[0x80]);
    assert_eq!(r.read_bit(), Some(true));
    for _ in 0..7 {
        assert_eq!(r.read_bit(), Some(false));
    }
    assert_eq!(r.read_bit(), None);
    assert_eq!(r.read_bits(1), None);
    assert_eq!(r.read_ue(), None);
    assert_eq!(r.read_se(), None);
    assert_eq!(r.skip_bits(1), None);
}

#[test]
fn bitreader_zero_and_oversize_reads() {
    let mut r = BitReader::new(&[0xFF]);
    assert_eq!(r.read_bits(0), Some(0));
    assert_eq!(r.read_bits(33), None);
    let mut r = BitReader::new(&[0; 8]);
    assert_eq!(r.read_bits(33), None);
}

#[test]
fn bitreader_read_bits_32_and_empty() {
    let mut r = BitReader::new(&[0x12, 0x34, 0x56, 0x78]);
    assert_eq!(r.read_bits(32), Some(0x1234_5678));
    let mut e = BitReader::new(&[]);
    assert_eq!(e.read_bits(8), None);
    assert_eq!(e.read_bit(), None);
}

#[test]
fn bitreader_skip_bits() {
    let mut r = BitReader::new(&[0xF3, 0x0A]);
    assert_eq!(r.skip_bits(4), Some(()));
    assert_eq!(r.read_bits(4), Some(0x3));
    assert_eq!(r.read_bits(8), Some(0x0A));
    assert_eq!(r.skip_bits(1), None);
}

// ---------------------------------------------------------------- avc

#[test]
fn avc_config_none_without_pps() {
    // Plausible SPS only; no PPS -> None.
    let sps: &[u8] = &[0x67, 0x42, 0xC0, 0x1E, 0xDA, 0x01, 0x40, 0x16, 0xE8];
    let au = annexb_from(&[sps]);
    assert!(avc::avc_config(&au).is_none());
}

#[test]
fn avc_config_none_without_sps() {
    let pps: &[u8] = &[0x68, 0xCE, 0x38, 0x80];
    let au = annexb_from(&[pps]);
    assert!(avc::avc_config(&au).is_none());
}

#[test]
fn avc_config_none_for_truncated_sps() {
    // Header byte only: SPS is malformed, so the config must be None.
    let au = annexb_from(&[&[0x67u8][..], &[0x68, 0xCE, 0x38, 0x80][..]]);
    assert!(avc::avc_config(&au).is_none());
}

#[test]
fn avcc_payload_empty_without_sps() {
    let cfg = AvcConfig {
        sps: vec![],
        pps: vec![vec![0x68, 0xCE]],
        width: 0,
        height: 0,
    };
    assert!(avc::avcc_payload(&cfg).is_empty());
}

#[test]
fn avcc_payload_baseline_layout_no_tail() {
    let cfg = AvcConfig {
        sps: vec![vec![0x67, 0x42, 0xC0, 0x1E, 0xAA]],
        pps: vec![vec![0x68, 0xCE]],
        width: 640,
        height: 360,
    };
    let got = avc::avcc_payload(&cfg);
    let want = vec![
        1, 0x42, 0xC0, 0x1E, 0xFF, 0xE1, 0x00, 0x05, 0x67, 0x42, 0xC0, 0x1E, 0xAA, 0x01, 0x00,
        0x02, 0x68, 0xCE,
    ];
    assert_eq!(got, want);
}

#[test]
fn avcc_payload_high_profile_tail() {
    let cfg = AvcConfig {
        sps: vec![vec![0x67, 100, 0x00, 0x1F, 0xAC]],
        pps: vec![vec![0x68, 0xEE]],
        width: 16,
        height: 16,
    };
    let got = avc::avcc_payload(&cfg);
    let n = check_avcc_structure(&cfg, &got);
    assert_eq!(
        &got[n..],
        &[0xFD, 0xF8, 0xF8, 0],
        "high-profile tail for 4:2:0 8-bit"
    );
    assert_eq!(got.len(), n + 4);
}

// ---------------------------------------------------------------- aac

#[test]
fn aac_asc_known_values() {
    assert_eq!(aac::aac_lc_asc(48000, 2), Some(vec![0x11, 0x90]));
    assert_eq!(aac::aac_lc_asc(44100, 2), Some(vec![0x12, 0x10]));
    assert_eq!(aac::aac_lc_asc(48000, 1), Some(vec![0x11, 0x88]));
}

#[test]
fn aac_asc_rejects_bad_inputs() {
    assert_eq!(aac::aac_lc_asc(48000, 0), None);
    assert_eq!(aac::aac_lc_asc(48000, 8), None);
    assert_eq!(aac::aac_lc_asc(12345, 2), None);
    assert_eq!(aac::aac_lc_asc(0, 2), None);
}

// ---------------------------------------------------------------- fuzz-ish

#[test]
fn fuzz_random_bytes_never_panic() {
    let mut rng = Lcg(0x1234_5678_9ABC_DEF0);
    for _ in 0..3000 {
        let len = (rng.next() % 96) as usize;
        let v: Vec<u8> = (0..len).map(|_| rng.byte()).collect();
        let _ = annexb::split_nals(&v);
        let _ = annexb::unescape_rbsp(&v);
        let _ = annexb::is_keyframe(&v, H264);
        let _ = annexb::is_keyframe(&v, HEVC);
        let _ = annexb::to_length_prefixed(&v, H264);
        let _ = annexb::to_length_prefixed(&v, HEVC);
        let _ = avc::avc_config(&v);
        let _ = aac::aac_lc_asc(rng.next() as u32, (rng.next() % 16) as u8);
    }
}

#[test]
fn fuzz_biased_start_codes_never_panic() {
    let mut rng = Lcg(0xDEAD_BEEF_0042_0007);
    for _ in 0..3000 {
        let len = (rng.next() % 128) as usize;
        let v: Vec<u8> = (0..len).map(|_| rng.biased_byte()).collect();
        for nal in annexb::split_nals(&v) {
            assert!(!nal.is_empty(), "split_nals must skip empty NALs");
            assert!(!matches!(nal.last(), Some(0)), "trailing zero not stripped");
        }
        let _ = annexb::unescape_rbsp(&v);
        let _ = annexb::is_keyframe(&v, H264);
        let _ = annexb::to_length_prefixed(&v, HEVC);
        let _ = avc::avc_config(&v);
    }
}

#[test]
fn fuzz_truncated_prefixes_never_panic() {
    // Truncate a plausible AU at every length; no truncation may panic.
    let au = annexb_from(&[
        &[0x67, 0x42, 0xC0, 0x1E, 0xDA, 0x01, 0x40, 0x16, 0xE8][..],
        &[0x68, 0xCE, 0x38, 0x80][..],
        &[0x65, 0x88, 0x84, 0x00, 0x33, 0xFF][..],
    ]);
    for end in 0..=au.len() {
        let v = &au[..end];
        let _ = annexb::split_nals(v);
        let _ = annexb::unescape_rbsp(v);
        let _ = annexb::is_keyframe(v, H264);
        let _ = annexb::to_length_prefixed(v, H264);
        let _ = avc::avc_config(v);
    }
}

#[test]
fn fuzz_bitreader_random_reads_never_panic() {
    let mut rng = Lcg(0xC0FF_EE00_1234_5678);
    for _ in 0..2000 {
        let len = (rng.next() % 12) as usize;
        let v: Vec<u8> = (0..len).map(|_| rng.byte()).collect();
        let mut r = BitReader::new(&v);
        for _ in 0..40 {
            match rng.next() % 5 {
                0 => {
                    let _ = r.read_bits((rng.next() % 40) as u32);
                }
                1 => {
                    let _ = r.read_bit();
                }
                2 => {
                    let _ = r.read_ue();
                }
                3 => {
                    let _ = r.read_se();
                }
                _ => {
                    let _ = r.skip_bits((rng.next() % 40) as u32);
                }
            }
        }
    }
}

// ---------------------------------------------------------------- real streams

/// Shared oracle checks on a real H.264 Annex B stream.
/// Returns the parsed config so callers can add size assertions.
fn oracle_checks(stream: &[u8]) -> AvcConfig {
    let nals = annexb::split_nals(stream);
    assert!(nals.len() >= 3, "expected >= 3 NALs, got {}", nals.len());
    assert!(annexb::is_keyframe(stream, H264), "stream contains an IDR");

    let aud_count = nals.iter().filter(|n| nal_type_h264(n) == 9).count();
    let lp = annexb::to_length_prefixed(stream, H264);
    let walked = walk_length_prefixed(&lp).expect("length-prefixed output must be well formed");
    assert_eq!(
        walked.len(),
        nals.len() - aud_count,
        "length prefixing must keep every non-AUD NAL"
    );
    for n in &walked {
        assert_ne!(nal_type_h264(n), 9, "AUD must be dropped");
    }

    let cfg = avc::avc_config(stream).expect("real stream must yield an AvcConfig");
    assert_eq!(cfg.sps.len(), 1, "one IDR -> one SPS after dedupe");
    assert_eq!(cfg.pps.len(), 1, "one IDR -> one PPS after dedupe");
    assert_eq!(nal_type_h264(&cfg.sps[0]), 7, "SPS header byte");
    assert_eq!(nal_type_h264(&cfg.pps[0]), 8, "PPS header byte");

    let payload = avc::avcc_payload(&cfg);
    check_avcc_structure(&cfg, &payload);
    cfg
}

#[test]
fn real_h264_720p_size_and_structure() {
    let Some(s) = gen_h264(1280, 720) else {
        return;
    };
    let cfg = oracle_checks(&s);
    assert_eq!((cfg.width, cfg.height), (1280, 720));
    let payload = avc::avcc_payload(&cfg);
    let n = check_avcc_structure(&cfg, &payload);
    check_profile_tail(&cfg, &payload, n);
}

#[test]
fn real_h264_1080p_is_cropped_from_1088() {
    let Some(s) = gen_h264(1920, 1080) else {
        return;
    };
    let cfg = oracle_checks(&s);
    assert_eq!(
        (cfg.width, cfg.height),
        (1920, 1080),
        "1088 coded rows must be cropped"
    );
}

#[test]
fn real_h264_360p_baseline_crop_and_no_tail() {
    // The native encoder picks its own profile (not forced to baseline), so the tail rule is
    // profile-driven; a baseline profile must have no tail.
    let Some(s) = gen_h264(640, 360) else {
        return;
    };
    let cfg = oracle_checks(&s);
    assert_eq!((cfg.width, cfg.height), (640, 360));
    let payload = avc::avcc_payload(&cfg);
    let n = check_avcc_structure(&cfg, &payload);
    check_profile_tail(&cfg, &payload, n);
}

#[test]
fn real_h264_without_pps_is_none() {
    let Some(s) = gen_h264(640, 360) else {
        return;
    };
    let kept: Vec<Vec<u8>> = annexb::split_nals(&s)
        .into_iter()
        .filter(|n| nal_type_h264(n) != 8)
        .map(|n| n.to_vec())
        .collect();
    let refs: Vec<&[u8]> = kept.iter().map(|v| v.as_slice()).collect();
    let rebuilt = annexb_from(&refs);
    assert!(avc::avc_config(&rebuilt).is_none());
}
