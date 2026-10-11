//! Black-box tests for the hardware encoders (Media Foundation async MFTs fed with
//! D3D11 NV12 textures). Uses only the public API of clipcheck-hwenc, the GPU
//! converter from clipcheck-gpuconv, and the native oracle (clipcheck-mp4 muxer/reader and
//! clipcheck-mfdec probe/decode) for the produced bitstreams.
//! Never launches games or touches the user's recordings.

#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::time::Instant;

use clipcheck_gpuconv::d3d::{create_device, GpuConverter};
use clipcheck_hwenc::{
    enumerate, open, Codec, EncError, EncodedPacket, EncoderDescriptor, InputFrame, RateControl,
    Vendor, VideoEncoder, VideoEncoderConfig,
};
use clipcheck_mfdec::{OutFormat, VideoDecoder};
use clipcheck_mp4::reader::TrackKind;
use clipcheck_mp4::remux::{concat_remux, RemuxOptions};
use clipcheck_mp4::segment::{SegmentConfig, SegmentMuxer, VideoPacket};
use clipcheck_mp4::VideoCodec;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE,
    D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};

// ---------------------------------------------------------------------------
// Helpers: device, source frames
// ---------------------------------------------------------------------------

/// A D3D11 device plus a ring of six NV12 textures the encoder reads from.
struct Rig {
    dev: ID3D11Device,
    conv: GpuConverter,
    nv12: Vec<ID3D11Texture2D>,
    w: u32,
    h: u32,
}

fn rig(w: u32, h: u32) -> Option<Rig> {
    let (dev, _ctx) = match create_device(None) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("SKIPPED: no D3D11 device: {e:?}");
            return None;
        }
    };
    let conv = GpuConverter::new(&dev, w, h).expect("GpuConverter::new");
    let nv12 = (0..6)
        .map(|_| conv.create_nv12_texture().expect("create_nv12_texture"))
        .collect();
    Some(Rig {
        dev,
        conv,
        nv12,
        w,
        h,
    })
}

/// BGRA texture with a moving gradient that depends on the frame index.
fn bgra_frame(dev: &ID3D11Device, w: u32, h: u32, i: usize) -> ID3D11Texture2D {
    let mut px = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let o = ((y * w + x) * 4) as usize;
            px[o] = (x as usize + i * 4) as u8; // B
            px[o + 1] = (y as usize + i * 2) as u8; // G
            px[o + 2] = (i * 3) as u8; // R
            px[o + 3] = 255; // A
        }
    }
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let init = D3D11_SUBRESOURCE_DATA {
        pSysMem: px.as_ptr() as *const _,
        SysMemPitch: w * 4,
        SysMemSlicePitch: 0,
    };
    let mut tex: Option<ID3D11Texture2D> = None;
    // SAFETY: desc and init are valid for the call; px outlives it; the out-pointer
    // references a live Option slot.
    unsafe {
        dev.CreateTexture2D(
            &desc as *const _,
            Some(&init as *const _),
            Some(&mut tex as *mut _),
        )
    }
    .expect("CreateTexture2D (BGRA source)");
    tex.expect("no BGRA texture returned")
}

impl Rig {
    /// Renders frame `i` into NV12 ring slot `i % 6`.
    fn fill(&mut self, i: usize) {
        let bgra = bgra_frame(&self.dev, self.w, self.h, i);
        let slot = i % 6;
        self.conv
            .convert(&bgra, &self.nv12[slot])
            .expect("GpuConverter::convert");
    }
}

// ---------------------------------------------------------------------------
// Helpers: encoders, configs
// ---------------------------------------------------------------------------

fn cfg(codec: Codec, w: u32, h: u32, fps: u32, rate: RateControl, gop: u32) -> VideoEncoderConfig {
    VideoEncoderConfig {
        codec,
        width: w,
        height: h,
        fps,
        rate,
        gop_frames: gop,
        quality_vs_speed: 50,
        low_latency: true,
    }
}

fn hw_descs(codec: Codec) -> Vec<EncoderDescriptor> {
    enumerate(codec)
        .expect("enumerate")
        .into_iter()
        .filter(|d| d.hardware)
        .collect()
}

fn nvidia_desc(codec: Codec) -> EncoderDescriptor {
    hw_descs(codec)
        .into_iter()
        .find(|d| d.vendor == Vendor::Nvidia)
        .unwrap_or_else(|| panic!("no NVIDIA hardware {codec:?} encoder enumerated"))
}

/// Encodes `frames` frames plus flush. Frame i has pts = i * 1e7 / fps and a forced
/// keyframe when i % gop == 0. Returns all packets and the per-encode() timings in us.
fn run(
    desc: &EncoderDescriptor,
    rig: &mut Rig,
    c: VideoEncoderConfig,
    frames: usize,
) -> Result<(Vec<EncodedPacket>, Vec<f64>), EncError> {
    let mut enc: Box<dyn VideoEncoder> = open(desc, Some(&rig.dev), &c)?;
    let mut out = Vec::new();
    let mut times = Vec::with_capacity(frames);
    for i in 0..frames {
        let pts = i as i64 * 10_000_000 / c.fps as i64;
        let key = (i as u32).is_multiple_of(c.gop_frames);
        rig.fill(i);
        let t = Instant::now();
        let pk = enc.encode(InputFrame::Texture(&rig.nv12[i % 6]), pts, key)?;
        times.push(t.elapsed().as_secs_f64() * 1e6);
        out.extend(pk);
    }
    out.extend(enc.flush()?);
    Ok((out, times))
}

/// NAL unit types in an Annex B buffer (H.264: b & 0x1f; HEVC: (b >> 1) & 0x3f).
fn nal_types(data: &[u8], hevc: bool) -> Vec<u8> {
    let mut v = Vec::new();
    let mut i = 0;
    while i + 3 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            let b = data[i + 3];
            v.push(if hevc { (b >> 1) & 0x3f } else { b & 0x1f });
            i += 3;
        } else {
            i += 1;
        }
    }
    v
}

fn mean(v: &[f64]) -> f64 {
    v.iter().sum::<f64>() / v.len().max(1) as f64
}

// ---------------------------------------------------------------------------
// Helpers: oracle (clipcheck-mp4 muxer/reader, clipcheck-mfdec decode)
// ---------------------------------------------------------------------------

/// Muxes encoded access units into one flat MP4 (fragmented segments, then concatenated), the
/// same path the native recorder uses. Returns the MP4 path under the system temp dir.
fn mux_mp4(
    name: &str,
    packets: &[EncodedPacket],
    codec: Codec,
    w: u32,
    h: u32,
    fps: u32,
) -> PathBuf {
    let root = std::env::temp_dir()
        .join("clipcheck_hwenc_tests")
        .join(name);
    let _ = std::fs::remove_dir_all(&root);
    let segs_dir = root.join("segs");
    let vc = match codec {
        Codec::Hevc => VideoCodec::Hevc,
        _ => VideoCodec::H264,
    };
    let mut muxer = SegmentMuxer::new(SegmentConfig {
        out_dir: segs_dir,
        segment_secs: 2.0,
        fps,
        codec: vc,
        width: w,
        height: h,
        audio: Vec::new(),
    })
    .expect("create segment muxer");
    for p in packets {
        muxer
            .push_video(VideoPacket {
                data: p.data.clone(),
                pts: p.pts,
                dts: p.dts,
                keyframe: p.keyframe,
            })
            .expect("push video packet");
    }
    let segs = muxer.finish().expect("finish segments");
    let out = root.join("stream.mp4");
    concat_remux(&segs, &out, &RemuxOptions::default()).expect("remux segments to mp4");
    out
}

/// Video samples of the first video track as read back by clipcheck-mp4:
/// (sample count, keyframe count, codec string).
fn mp4_video_facts(file: &Path) -> (usize, usize, String) {
    let movie = clipcheck_mp4::reader::read_movie(file).expect("read mp4 with clipcheck-mp4");
    let track = movie
        .tracks
        .iter()
        .find(|t| matches!(t.kind, TrackKind::Video { .. }))
        .expect("video track");
    let codec = match &track.kind {
        TrackKind::Video { codec, .. } => codec.clone(),
        _ => unreachable!(),
    };
    let keys = track.samples.iter().filter(|s| s.keyframe).count();
    (track.samples.len(), keys, codec)
}

/// Decodes every frame with clipcheck-mfdec; returns the frame count. Panics on a decode error.
fn mfdec_frame_count(file: &Path) -> usize {
    let mut d = VideoDecoder::open(file, OutFormat::Bgra).expect("mfdec open");
    let mut n = 0usize;
    while d.next_frame().expect("mfdec next_frame").is_some() {
        n += 1;
    }
    n
}

// ---------------------------------------------------------------------------
// Test 1: NVIDIA H.264 1920x1080 60 fps CBR 8000 kbps, GOP 120, 300 frames
// ---------------------------------------------------------------------------

#[test]
fn t1_nvidia_h264_cbr_1080p60_300_frames() {
    let Some(mut rig) = rig(1920, 1080) else {
        return;
    };
    let desc = nvidia_desc(Codec::H264);
    println!("descriptor: {} (index {})", desc.name, desc.index);
    let c = cfg(
        Codec::H264,
        1920,
        1080,
        60,
        RateControl::Cbr { kbps: 8000 },
        120,
    );
    let (pk, times) = run(&desc, &mut rig, c, 300).expect("NVIDIA H.264 open/encode must succeed");

    println!(
        "T1 total packets (encode+flush): {} (expected 300)",
        pk.len()
    );
    let first_nals = nal_types(&pk[0].data, false);
    println!(
        "T1 first packet NAL types: {first_nals:?} keyframe={}",
        pk[0].keyframe
    );
    assert!(pk[0].keyframe, "first packet must be a keyframe");
    assert!(first_nals.contains(&7), "first packet lacks SPS (7)");
    assert!(first_nals.contains(&8), "first packet lacks PPS (8)");

    let key_pts: Vec<i64> = pk.iter().filter(|p| p.keyframe).map(|p| p.pts).collect();
    println!(
        "T1 keyframe pts (100ns): {key_pts:?} count={}",
        key_pts.len()
    );
    println!(
        "T1 keyframe pts in s: {:?}",
        key_pts.iter().map(|p| *p as f64 / 1e7).collect::<Vec<_>>()
    );
    for want in [0i64, 20_000_000, 40_000_000] {
        assert!(key_pts.contains(&want), "no keyframe at pts {want}");
    }

    let mut mono = true;
    for w in pk.windows(2) {
        if w[1].pts <= w[0].pts {
            mono = false;
        }
    }
    let dts_eq = pk.iter().all(|p| p.dts == p.pts);
    println!("T1 pts strictly increasing: {mono}; dts == pts for all: {dts_eq}");
    assert!(mono, "pts not strictly increasing");
    assert!(dts_eq, "dts != pts (B-frames?)");

    let bytes: usize = pk.iter().map(|p| p.data.len()).sum();
    println!("T1 total bytes: {bytes} (~5 MB expected; accept 3.0-7.0 MB)");
    assert!(
        (3_000_000..=7_000_000).contains(&bytes),
        "byte total out of range"
    );

    println!(
        "T1 timing: mean encode() {:.1} us over {} calls",
        mean(&times),
        times.len()
    );
    assert_eq!(pk.len(), 300, "packet count");
}

// ---------------------------------------------------------------------------
// Test 2: ORACLE decode of the same H.264 stream
// ---------------------------------------------------------------------------

#[test]
fn t2_oracle_nvidia_h264_decode() {
    let Some(mut rig) = rig(1920, 1080) else {
        return;
    };
    let desc = nvidia_desc(Codec::H264);
    let c = cfg(
        Codec::H264,
        1920,
        1080,
        60,
        RateControl::Cbr { kbps: 8000 },
        120,
    );
    let (pk, _) = run(&desc, &mut rig, c, 300).expect("NVIDIA H.264 encode");
    let file = mux_mp4("t2_out", &pk, Codec::H264, 1920, 1080, 60);
    let p = clipcheck_mfdec::probe(&file).expect("mfdec probe");
    println!(
        "T2 mfdec probe: {}x{} codec={}",
        p.width, p.height, p.video_codec
    );
    assert_eq!((p.width, p.height), (1920, 1080));
    assert_eq!(p.video_codec, "h264");
    let (n, keys, codec) = mp4_video_facts(&file);
    println!("T2 clipcheck-mp4 reader: samples={n} keyframes={keys} codec={codec}");
    assert_eq!(n, 300, "video sample count");
    assert_eq!(codec, "h264");
    assert!(keys >= 2, "expected keyframes every 120 frames, got {keys}");
    let decoded = mfdec_frame_count(&file);
    println!("T2 mfdec full decode: {decoded} frames");
    assert_eq!(decoded, 300, "decoded frame count");
}

// ---------------------------------------------------------------------------
// Test 3: NVIDIA HEVC, same settings, 120 frames
// ---------------------------------------------------------------------------

#[test]
fn t3_nvidia_hevc_120_frames() {
    let Some(mut rig) = rig(1920, 1080) else {
        return;
    };
    let desc = nvidia_desc(Codec::Hevc);
    println!("HEVC descriptor: {} (index {})", desc.name, desc.index);
    let c = cfg(
        Codec::Hevc,
        1920,
        1080,
        60,
        RateControl::Cbr { kbps: 8000 },
        120,
    );
    let (pk, times) = match run(&desc, &mut rig, c, 120) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("SKIPPED T3 HEVC: NVIDIA HEVC open/encode failed: {e:?}");
            return;
        }
    };
    println!("T3 total packets: {} (expected 120)", pk.len());
    let nals = nal_types(&pk[0].data, true);
    println!("T3 first packet HEVC NAL types: {nals:?}");
    assert!(nals.contains(&32), "first packet lacks VPS (32)");
    assert!(nals.contains(&33), "first packet lacks SPS (33)");
    assert!(nals.contains(&34), "first packet lacks PPS (34)");
    println!("T3 mean encode(): {:.1} us", mean(&times));
    assert_eq!(pk.len(), 120, "packet count");
    let file = mux_mp4("t3_out", &pk, Codec::Hevc, 1920, 1080, 60);
    let (n, _keys, codec) = mp4_video_facts(&file);
    println!("T3 clipcheck-mp4 reader: samples={n} codec={codec}");
    assert_eq!(codec, "hevc");
    assert_eq!(n, 120, "video sample count");
    match VideoDecoder::open(&file, OutFormat::Bgra) {
        Ok(_) => {
            let decoded = mfdec_frame_count(&file);
            println!("T3 mfdec full decode: {decoded} frames");
            assert_eq!(decoded, 120, "HEVC decoded frame count");
        }
        Err(e) => eprintln!("SKIPPED T3 decode: mfdec cannot open HEVC here: {e:?}"),
    }
}

// ---------------------------------------------------------------------------
// Test 4: rate-control modes on NVIDIA H.264, 60 frames each
// ---------------------------------------------------------------------------

#[test]
fn t4_nvidia_h264_rate_modes() {
    let Some(mut rig) = rig(1920, 1080) else {
        return;
    };
    let desc = nvidia_desc(Codec::H264);
    for rate in [
        RateControl::Quality { cq: 23 },
        RateControl::Vbr {
            kbps: 6000,
            peak_kbps: 9000,
        },
    ] {
        let c = cfg(Codec::H264, 1920, 1080, 60, rate, 120);
        let (pk, _) =
            run(&desc, &mut rig, c, 60).unwrap_or_else(|e| panic!("{rate:?} failed: {e:?}"));
        let bytes: usize = pk.iter().map(|p| p.data.len()).sum();
        println!("T4 {rate:?}: packets={} bytes={bytes}", pk.len());
        assert_eq!(pk.len(), 60, "{rate:?} packet count");
    }
}

// ---------------------------------------------------------------------------
// Test 5: every other (non-NVIDIA) hardware H.264 descriptor, 1280x720, 60 frames
// Open errors and encode errors are reported, not failed.
// ---------------------------------------------------------------------------

#[test]
fn t5_other_hardware_h264_720p60() {
    let others: Vec<EncoderDescriptor> = hw_descs(Codec::H264)
        .into_iter()
        .filter(|d| d.vendor != Vendor::Nvidia)
        .collect();
    if others.is_empty() {
        println!("T5: no non-NVIDIA hardware H.264 encoders enumerated");
        return;
    }
    let Some(mut rig) = rig(1280, 720) else {
        return;
    };
    for desc in &others {
        let c = cfg(
            Codec::H264,
            1280,
            720,
            60,
            RateControl::Cbr { kbps: 4000 },
            60,
        );
        match run(desc, &mut rig, c, 60) {
            Ok((pk, times)) => println!(
                "T5 {} ({:?}, index {}): packets={} mean_encode_us={:.1}",
                desc.name,
                desc.vendor,
                desc.index,
                pk.len(),
                mean(&times)
            ),
            Err(e) => println!(
                "T5 {} ({:?}, index {}): ERROR {e:?}",
                desc.name, desc.vendor, desc.index
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// Test 6: error handling
// ---------------------------------------------------------------------------

#[test]
fn t6_errors_are_bad_input() {
    let Some(rig) = rig(1920, 1080) else { return };
    let desc = nvidia_desc(Codec::H264);
    let good = cfg(
        Codec::H264,
        1920,
        1080,
        60,
        RateControl::Cbr { kbps: 8000 },
        120,
    );

    // Hardware encoder without a device.
    let e = open(&desc, None, &good).err();
    println!("T6 open(hw, None): {e:?}");
    assert!(
        matches!(e, Some(EncError::BadInput(_))),
        "open without device: {e:?}"
    );

    // Odd width.
    let odd = VideoEncoderConfig {
        width: 1921,
        ..good
    };
    let e = open(&desc, Some(&rig.dev), &odd).err();
    println!("T6 odd width: {e:?}");
    assert!(matches!(e, Some(EncError::BadInput(_))), "odd width: {e:?}");

    // Zero fps and zero GOP (extra checks from the API contract).
    let zero_fps = VideoEncoderConfig { fps: 0, ..good };
    let e = open(&desc, Some(&rig.dev), &zero_fps).err();
    println!("T6 fps 0: {e:?}");
    assert!(matches!(e, Some(EncError::BadInput(_))), "fps 0: {e:?}");
    let zero_gop = VideoEncoderConfig {
        gop_frames: 0,
        ..good
    };
    let e = open(&desc, Some(&rig.dev), &zero_gop).err();
    println!("T6 gop 0: {e:?}");
    assert!(matches!(e, Some(EncError::BadInput(_))), "gop 0: {e:?}");

    // Software-memory frame on a hardware encoder.
    let mut enc = open(&desc, Some(&rig.dev), &good).expect("valid open");
    let buf = vec![0u8; 1920 * 1080 * 3 / 2];
    let e = enc
        .encode(
            InputFrame::Nv12 {
                data: &buf,
                width: 1920,
                height: 1080,
            },
            0,
            true,
        )
        .err();
    println!("T6 encode(Nv12) on hardware: {e:?}");
    assert!(
        matches!(e, Some(EncError::BadInput(_))),
        "Nv12 on hw: {e:?}"
    );
}

// ---------------------------------------------------------------------------
// Test 7 is the timing printed by test 1 (mean encode() microseconds).
// ---------------------------------------------------------------------------
