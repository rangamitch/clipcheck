//! Black-box tests for `clipcheck_mp4::hevc`.
//!
//! All inputs are synthetic byte buffers. The earlier real-stream tests were
//! dropped in wave F3 (D62): they needed an HEVC encoder (libx265), which the
//! native test media generator does not provide (H.264 only).

use clipcheck_mp4::hevc::{hevc_config, hvcc_payload, HevcConfig};

fn cfg_clone_shallow() -> HevcConfig {
    HevcConfig {
        vps: vec![vec![0x40, 0x01, 0x0C]],
        sps: vec![vec![0x42, 0x01, 0xFF, 0x00]],
        pps: vec![vec![0x44, 0x01]],
        width: 1,
        height: 1,
    }
}

/// Annex B buffer carrying the VPS, SPS and PPS of `cfg_clone_shallow`.
fn synthetic_annexb() -> Vec<u8> {
    let cfg = cfg_clone_shallow();
    let mut out = Vec::new();
    for nal in cfg.vps.iter().chain(&cfg.sps).chain(&cfg.pps) {
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nal);
    }
    out
}

#[test]
fn truncated_prefixes_never_panic() {
    let s = synthetic_annexb();
    let head = &s[..200.min(s.len())];
    for len in 0..=head.len() {
        let prefix = &head[..len];
        if let Some(cfg) = hevc_config(prefix) {
            let _ = hvcc_payload(&cfg);
        }
    }
}

#[test]
fn random_bytes_never_panic() {
    let mut state: u64 = 0x1234_5678_9ABC_DEF0;
    let mut next = move || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (state >> 33) as u32
    };
    for iter in 0..2000 {
        let len = (next() % 400) as usize;
        let mut buf: Vec<u8> = (0..len).map(|_| next() as u8).collect();
        // Every few iterations, prepend a start code and a NAL header so the parser
        // actually reaches the parameter-set paths.
        if iter % 3 == 0 {
            let mut pre = vec![0u8, 0, 1, ((next() % 64) as u8) << 1, next() as u8];
            pre.append(&mut buf);
            buf = pre;
        }
        if let Some(cfg) = hevc_config(&buf) {
            let _ = hvcc_payload(&cfg);
        }
    }
}

#[test]
fn garbage_sps_payload_is_empty() {
    let cfg = HevcConfig {
        vps: vec![vec![0x40, 0x01, 0x0C]],
        sps: vec![vec![0x42, 0x01, 0xFF, 0x00]],
        pps: vec![vec![0x44, 0x01]],
        width: 1,
        height: 1,
    };
    assert!(hvcc_payload(&cfg).is_empty());

    let empty_sps = HevcConfig {
        sps: vec![],
        ..cfg_clone_shallow()
    };
    assert!(hvcc_payload(&empty_sps).is_empty());

    let empty_nal = HevcConfig {
        sps: vec![vec![]],
        ..cfg_clone_shallow()
    };
    assert!(hvcc_payload(&empty_nal).is_empty());
}

#[test]
fn random_sps_payload_never_panics() {
    let mut state: u64 = 0xDEAD_BEEF_0042_0001;
    let mut next = move || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (state >> 33) as u32
    };
    for _ in 0..500 {
        let len = (next() % 300) as usize;
        let sps: Vec<u8> = (0..len).map(|_| next() as u8).collect();
        let cfg = HevcConfig {
            sps: vec![sps],
            ..cfg_clone_shallow()
        };
        let _ = hvcc_payload(&cfg);
    }
}
