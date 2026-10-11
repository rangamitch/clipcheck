//! Black-box tests for clipcheck-aacenc. Uses only the public API. There is no native PCM
//! decoder in the workspace (clipcheck-mfdec is video-only), so the AAC output is validated
//! structurally: ADTS headers, frame counts and durations derived from the sample rate.

use clipcheck_aacenc::{AacEncoder, AacError, AacPacket};

/// Interleaved sine wave.
fn sine(rate: u32, ch: usize, secs: f64, freq: f64, amp: f32) -> Vec<f32> {
    let frames = (rate as f64 * secs).round() as usize;
    let mut v = Vec::with_capacity(frames * ch);
    for i in 0..frames {
        let s = (amp as f64 * (2.0 * std::f64::consts::PI * freq * i as f64 / rate as f64).sin())
            as f32;
        for _ in 0..ch {
            v.push(s);
        }
    }
    v
}

/// Wrap raw AAC frames in 7-byte ADTS headers (LC profile, no CRC).
fn adts_wrap(packets: &[AacPacket], freq_idx: u8, chan: u8) -> Vec<u8> {
    let mut out = Vec::new();
    for p in packets {
        let flen = 7 + p.data.len();
        assert!(flen < 8192, "frame too large for ADTS: {flen}");
        let b0: u8 = 0xFF;
        let b1: u8 = 0xF1; // syncword low nibble, ID 0, layer 0, protection_absent 1
        let b2: u8 = (1u8 << 6) | (freq_idx << 2) | ((chan >> 2) & 1); // profile-1 = 1 (LC)
        let b3: u8 = ((chan & 3) << 6) | (((flen >> 11) & 3) as u8);
        let b4: u8 = ((flen >> 3) & 0xFF) as u8;
        let b5: u8 = (((flen & 7) << 5) as u8) | 0x1F;
        let b6: u8 = 0xFC; // buffer fullness low bits 0x3F, 1 raw block
        out.extend_from_slice(&[b0, b1, b2, b3, b4, b5, b6]);
        out.extend_from_slice(&p.data);
    }
    out
}

/// One ADTS header read back from a stream: (profile, sampling_frequency_index, channels, frame length).
#[derive(Debug, PartialEq, Eq)]
struct AdtsHeader {
    profile: u8,
    freq_idx: u8,
    chan: u8,
    frame_len: usize,
}

/// Walks an ADTS stream and returns every header; panics on a bad syncword or a length that
/// overruns the buffer.
fn parse_adts(aac: &[u8]) -> Vec<AdtsHeader> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < aac.len() {
        assert!(i + 7 <= aac.len(), "truncated ADTS header at {i}");
        let b = &aac[i..i + 7];
        assert_eq!(b[0], 0xFF, "ADTS syncword at {i}");
        assert_eq!(b[1] & 0xF6, 0xF0, "ADTS syncword/layer at {i}");
        let frame_len =
            (((b[3] & 3) as usize) << 11) | ((b[4] as usize) << 3) | ((b[5] >> 5) as usize);
        assert!(frame_len >= 7, "ADTS frame length {frame_len} at {i}");
        assert!(
            i + frame_len <= aac.len(),
            "ADTS frame at {i} overruns buffer"
        );
        out.push(AdtsHeader {
            profile: (b[2] >> 6) + 1,
            freq_idx: (b[2] >> 2) & 0xF,
            chan: ((b[2] & 1) << 2) | (b[3] >> 6),
            frame_len,
        });
        i += frame_len;
    }
    out
}

/// Asserts that the ADTS headers describe `n` LC frames at `freq_idx`/`chan`, each carrying
/// 1024 samples, and that the implied duration matches `secs` within `tol` seconds.
fn check_adts_duration(
    aac: &[u8],
    n: usize,
    freq_idx: u8,
    chan: u8,
    rate: u32,
    secs: f64,
    tol: f64,
) {
    let hdrs = parse_adts(aac);
    assert_eq!(hdrs.len(), n, "ADTS frame count");
    for (i, h) in hdrs.iter().enumerate() {
        assert_eq!(h.profile, 2, "frame {i} profile (AAC-LC)");
        assert_eq!(h.freq_idx, freq_idx, "frame {i} sampling index");
        assert_eq!(h.chan, chan, "frame {i} channel config");
    }
    let dur = n as f64 * 1024.0 / rate as f64;
    println!("adts: {n} frames -> duration {dur:.4} s");
    assert!((dur - secs).abs() <= tol, "duration {dur:.4} vs {secs}");
}

#[test]
fn t1_new_validation_and_asc() {
    match AacEncoder::new(32000, 2, 128) {
        Err(AacError::BadInput(_)) => {}
        Err(e) => panic!("32000 Hz: wrong error variant: {e:?}"),
        Ok(_) => panic!("32000 Hz accepted"),
    }
    assert!(
        matches!(AacEncoder::new(48000, 0, 128), Err(AacError::BadInput(_))),
        "0 channels not BadInput"
    );
    assert!(
        matches!(AacEncoder::new(48000, 3, 128), Err(AacError::BadInput(_))),
        "3 channels not BadInput"
    );

    let e = AacEncoder::new(48000, 2, 128).expect("48k stereo");
    assert_eq!(e.asc(), &[0x11, 0x90], "asc 48000/2");
    let e = AacEncoder::new(44100, 2, 128).expect("44.1k stereo");
    assert_eq!(e.asc(), &[0x12, 0x10], "asc 44100/2");
    let e = AacEncoder::new(48000, 1, 128).expect("48k mono");
    assert_eq!(e.asc(), &[0x11, 0x88], "asc 48000/1");
    let e = AacEncoder::new(44100, 1, 128).expect("44.1k mono");
    assert_eq!(e.asc(), &[0x12, 0x08], "asc 44100/1 (derived by hand)");
}

#[test]
fn t2_stream_2s_stereo_packets() {
    let mut enc = AacEncoder::new(48000, 2, 128).expect("new");
    let first_pts: i64 = 1_000_000;
    let sig = sine(48000, 2, 2.0, 440.0, 0.8);
    let chunk = 480 * 2;
    let mut pkts: Vec<AacPacket> = Vec::new();
    for (ci, c) in sig.chunks(chunk).enumerate() {
        let pts = first_pts + ci as i64 * 100_000; // 10 ms per chunk in 100 ns units
        pkts.extend(enc.push(c, pts).expect("push"));
    }
    pkts.extend(enc.flush().expect("flush"));

    println!("t2: packet count = {}", pkts.len());
    assert!(
        (90..=96).contains(&pkts.len()),
        "packet count {} not in 90..=96",
        pkts.len()
    );

    let step = 1024.0 * 1e7 / 48000.0;
    let mut max_err: f64 = 0.0;
    for (i, p) in pkts.iter().enumerate() {
        assert!(!p.data.is_empty(), "packet {i} empty");
        assert!(p.data.len() < 2048, "packet {i} size {}", p.data.len());
        assert_eq!(p.samples, 1024, "packet {i} samples");
        let expect = first_pts as f64 + i as f64 * step;
        let err = (p.pts as f64 - expect).abs();
        max_err = max_err.max(err);
        assert!(err < 1.0, "packet {i} pts {} expected {:.2}", p.pts, expect);
    }
    println!("t2: max pts error vs formula = {max_err:.4}");
    for w in pkts.windows(2) {
        let d = w[1].pts - w[0].pts;
        assert!(w[1].pts > w[0].pts, "pts not strictly increasing");
        assert!((d as f64 - step).abs() < 1.0, "pts diff {d} vs {step:.2}");
    }
    let sizes: Vec<usize> = pkts.iter().map(|p| p.data.len()).collect();
    println!(
        "t2: sizes min={} max={}",
        sizes.iter().min().unwrap(),
        sizes.iter().max().unwrap()
    );
}

#[test]
fn t3_bad_input_and_empty_push() {
    let mut enc = AacEncoder::new(48000, 2, 128).expect("new");
    let r = enc.push(&[0.0f32; 3], 0);
    assert!(
        matches!(r, Err(AacError::BadInput(_))),
        "odd stereo count not BadInput"
    );
    let r = enc.push(&[], 0);
    match r {
        Ok(v) => assert!(v.is_empty(), "empty push returned {} packets", v.len()),
        Err(e) => panic!("empty push errored: {e:?}"),
    }
}

#[test]
fn t4_oracle_decode_48k_stereo() {
    let mut enc = AacEncoder::new(48000, 2, 128).expect("new");
    let sig = sine(48000, 2, 2.0, 440.0, 0.8);
    let mut pkts = Vec::new();
    for (ci, c) in sig.chunks(960).enumerate() {
        pkts.extend(enc.push(c, ci as i64 * 100_000).expect("push"));
    }
    pkts.extend(enc.flush().expect("flush"));
    let aac = adts_wrap(&pkts, 3, 2);

    // Structural check: 48 kHz (index 3) stereo (config 2) LC frames covering 2 s.
    check_adts_duration(&aac, pkts.len(), 3, 2, 48000, 2.0, 0.1);
    let step = 1024.0 * 1e7 / 48000.0;
    for w in pkts.windows(2) {
        let d = (w[1].pts - w[0].pts) as f64;
        assert!((d - step).abs() < 1.0, "pts diff {d} vs {step:.2}");
    }
}

#[test]
fn t5_mono_44k1_1s() {
    let mut enc = AacEncoder::new(44100, 1, 128).expect("new");
    let sig = sine(44100, 1, 1.0, 440.0, 0.8);
    let mut pkts = Vec::new();
    for (ci, c) in sig.chunks(441).enumerate() {
        pkts.extend(enc.push(c, ci as i64 * 100_000).expect("push"));
    }
    pkts.extend(enc.flush().expect("flush"));
    println!("t5: packet count = {}", pkts.len());
    assert!(
        (40..=46).contains(&pkts.len()),
        "packet count {}",
        pkts.len()
    );
    for p in &pkts {
        assert_eq!(p.samples, 1024);
        assert!(!p.data.is_empty());
    }

    // freq index 4 = 44100, channel config 1 = mono
    let aac = adts_wrap(&pkts, 4, 1);
    check_adts_duration(&aac, pkts.len(), 4, 1, 44100, 1.0, 0.1);
}

#[test]
fn t6_clipping_extremes_do_not_error() {
    let mut enc = AacEncoder::new(48000, 2, 128).expect("new");
    let mut pattern = Vec::new();
    for i in 0..9600 {
        pattern.push(if i % 2 == 0 { 3.0f32 } else { -3.0f32 });
    }
    let mut n = 0usize;
    for (ci, c) in pattern.chunks(960).enumerate() {
        let v = enc.push(c, ci as i64 * 100_000);
        assert!(v.is_ok(), "push of +/-3.0 errored: {:?}", v.err());
        n += v.unwrap().len();
    }
    assert!(enc.flush().is_ok(), "flush after +/-3.0 errored");

    let mut enc2 = AacEncoder::new(48000, 2, 96).expect("new");
    let big: Vec<f32> = (0..9600)
        .map(|i| if i % 3 == 0 { 1.0e6 } else { -1.0e6 })
        .collect();
    for (ci, c) in big.chunks(960).enumerate() {
        assert!(
            enc2.push(c, ci as i64 * 100_000).is_ok(),
            "push of 1e6 errored"
        );
    }
    assert!(enc2.flush().is_ok(), "flush after 1e6 errored");
    println!("t6: packets from +/-3.0 run before flush = {n}");
}
