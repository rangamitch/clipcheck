//! Black-box tests for the in-process native encoder (`clipcheck_encode::native`).
//!
//! Output is verified natively: clipcheck-mp4 reads the container and clipcheck-mfdec decodes.
//! Hardware tests need an NVIDIA GPU (NVENC) for the `*_nvenc` targets.
#![cfg(windows)]

use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use clipcheck_encode::native::{native_target, NativeEncoder, NativeParams};
use clipcheck_encode::pipe::AudioTrackSpec;
use clipcheck_hwenc::RateControl;
use clipcheck_mfdec::{OutFormat, VideoDecoder};
use clipcheck_mp4::reader::{Movie, Track, TrackKind};
use tempfile::TempDir;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Texture2D, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_SUBRESOURCE_DATA,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};

// ---------------------------------------------------------------- native oracle helpers
// Container facts come from clipcheck-mp4 (reader), decoding from clipcheck-mfdec.

fn movie(file: &Path) -> Movie {
    clipcheck_mp4::reader::read_movie(file)
        .unwrap_or_else(|e| panic!("read {}: {e:?}", file.display()))
}

fn video_track(m: &Movie) -> &Track {
    m.tracks
        .iter()
        .find(|t| matches!(t.kind, TrackKind::Video { .. }))
        .expect("video track")
}

fn audio_tracks(m: &Movie) -> Vec<&Track> {
    m.tracks
        .iter()
        .filter(|t| matches!(t.kind, TrackKind::Audio { .. }))
        .collect()
}

fn count_video_frames(file: &Path) -> u64 {
    video_track(&movie(file)).samples.len() as u64
}

/// True if the first video sample is a sync sample (keyframe).
fn first_frame_is_key(file: &Path) -> bool {
    video_track(&movie(file))
        .samples
        .first()
        .is_some_and(|s| s.keyframe)
}

/// Full decode through clipcheck-mfdec; returns (ok, error text).
fn decode_clean(file: &Path) -> (bool, String) {
    let mut d = match VideoDecoder::open(file, OutFormat::Bgra) {
        Ok(d) => d,
        Err(e) => return (false, format!("{e:?}")),
    };
    loop {
        match d.next_frame() {
            Ok(Some(_)) => continue,
            Ok(None) => return (true, String::new()),
            Err(e) => return (false, format!("{e:?}")),
        }
    }
}

/// Luma plane (Y bytes) of the first decoded frame.
fn first_frame_luma(file: &Path) -> Vec<u8> {
    let mut d = VideoDecoder::open(file, OutFormat::Nv12).expect("mfdec open");
    let f = d
        .next_frame()
        .expect("mfdec next_frame")
        .expect("first frame");
    let n = f.width as usize * f.height as usize;
    f.data[..n].to_vec()
}

fn format_duration(file: &Path) -> f64 {
    movie(file).duration_secs
}

/// Track length in seconds from its sample durations.
fn track_duration(t: &Track) -> f64 {
    let total: u64 = t.samples.iter().map(|s| u64::from(s.duration)).sum();
    total as f64 / f64::from(t.timescale)
}

fn segment_rows(out_dir: &Path) -> Vec<(String, f64, f64)> {
    let text = fs::read_to_string(out_dir.join("segments.csv")).expect("segments.csv");
    text.lines()
        .filter(|l| l.contains(".mp4"))
        .map(|l| {
            let f: Vec<&str> = l.split(',').collect();
            assert_eq!(f.len(), 3, "csv row: {l}");
            (
                f[0].to_string(),
                f[1].parse().unwrap(),
                f[2].parse().unwrap(),
            )
        })
        .collect()
}

// ---------------------------------------------------------------- input helpers

fn gradient(w: u32, h: u32, t: u32) -> Vec<u8> {
    let mut v = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            v[i] = ((x + t * 4) % 256) as u8;
            v[i + 1] = (y % 256) as u8;
            v[i + 2] = ((x + y + t) % 256) as u8;
            v[i + 3] = 255;
        }
    }
    v
}

/// One video frame's worth of 48 kHz stereo interleaved f32 sine (amplitude 0.5).
fn sine_block(fps: u32, frame_idx: u64, freq: f64) -> Vec<f32> {
    let n = (48_000 / fps) as u64;
    let mut out = Vec::with_capacity((n * 2) as usize);
    for k in 0..n {
        let sample = frame_idx * n + k;
        let v = (0.5 * (2.0 * std::f64::consts::PI * freq * sample as f64 / 48_000.0).sin()) as f32;
        out.push(v);
        out.push(v);
    }
    out
}

fn audio_specs(titles: &[&str]) -> Vec<AudioTrackSpec> {
    titles
        .iter()
        .map(|t| AudioTrackSpec {
            title: t.to_string(),
            bitrate_kbps: 160,
        })
        .collect()
}

fn params(
    out: &Path,
    width: u32,
    height: u32,
    fps: u32,
    target: &str,
    kbps: u32,
    seg: f32,
) -> NativeParams {
    NativeParams {
        out_dir: out.to_path_buf(),
        width,
        height,
        fps,
        target: native_target(target).unwrap_or_else(|| panic!("native_target({target})")),
        rate: RateControl::Cbr { kbps },
        quality_vs_speed: 50,
        segment_secs: seg,
        wall_clock: false,
    }
}

fn out_dir(root: &TempDir, name: &str) -> PathBuf {
    let d = root.path().join(name);
    fs::create_dir_all(&d).unwrap();
    d
}

fn kind_of<T>(r: io::Result<T>) -> Option<ErrorKind> {
    r.err().map(|e| e.kind())
}

// ---------------------------------------------------------------- tests

#[test]
fn t1_nvenc_h264_two_audio_tracks_segments_and_concat() {
    let root = TempDir::new().unwrap();
    let dir = out_dir(&root, "segs");
    let p = params(&dir, 1280, 720, 30, "h264_nvenc", 6000, 2.0);
    let audio = audio_specs(&["Game", "Mic"]);
    let mut enc = NativeEncoder::start(p, None, &audio).expect("start");
    println!("encoder_name = {}", enc.encoder_name());
    for i in 0..180u32 {
        enc.push_bgra(&gradient(1280, 720, i), 1280, 720)
            .expect("push_bgra");
        enc.push_audio(0, &sine_block(30, i as u64, 440.0))
            .expect("audio 0");
        enc.push_audio(1, &sine_block(30, i as u64, 440.0))
            .expect("audio 1");
    }
    let rep = enc.finish().expect("finish");
    println!(
        "frames_in={} segments={} exit_ok={}",
        rep.frames_in,
        rep.segments.len(),
        rep.exit_ok
    );
    assert_eq!(rep.frames_in, 180);
    assert_eq!(rep.segments.len(), 3, "segments");

    let rows = segment_rows(&dir);
    println!("csv rows: {rows:?}");
    assert_eq!(rows.len(), 3, "csv rows");
    for (i, (name, start, end)) in rows.iter().enumerate() {
        assert_eq!(name, &format!("seg-{i:06}.mp4"));
        let d = end - start;
        assert!((d - 2.0).abs() <= 0.05, "row {i} duration {d}");
        if i > 0 {
            assert!(
                (rows[i - 1].2 - start).abs() < 0.01,
                "row {i} not contiguous"
            );
        }
    }

    let mut total_frames = 0;
    for seg in &rep.segments {
        let m = movie(seg);
        let TrackKind::Video {
            codec,
            width,
            height,
        } = &video_track(&m).kind
        else {
            unreachable!("video_track returns a video track")
        };
        assert_eq!(codec, "h264");
        assert_eq!((*width, *height), (1280, 720));
        let a = audio_tracks(&m);
        assert_eq!(a.len(), 2, "{}: audio tracks", seg.display());
        let mut titles: Vec<String> = a
            .iter()
            .map(|t| t.title.clone().unwrap_or_default())
            .collect();
        titles.sort();
        assert_eq!(
            titles,
            vec!["Game".to_string(), "Mic".to_string()],
            "audio titles"
        );
        for t in &a {
            let TrackKind::Audio {
                codec,
                channels,
                sample_rate,
            } = &t.kind
            else {
                unreachable!("audio_tracks returns audio tracks")
            };
            assert_eq!(codec, "aac");
            assert_eq!(*sample_rate, 48000);
            assert_eq!(*channels, 2);
        }
        let n = count_video_frames(seg);
        total_frames += n;
        println!(
            "{}: frames={n} key_first={}",
            seg.display(),
            first_frame_is_key(seg)
        );
        assert!((58..=62).contains(&n), "{}: {n} frames", seg.display());
        assert!(
            first_frame_is_key(seg),
            "{}: first frame not key",
            seg.display()
        );
        let (ok, err) = decode_clean(seg);
        assert!(
            ok && err.trim().is_empty(),
            "{}: decode stderr: {err}",
            seg.display()
        );
    }
    println!("sum of segment frames = {total_frames}");

    // Concat like the app does (native remux, no external tool).
    let out = root.path().join("joined.mp4");
    clipcheck_mp4::remux::concat_remux(
        &rep.segments,
        &out,
        &clipcheck_mp4::remux::RemuxOptions::default(),
    )
    .expect("concat segments");
    let m = movie(&out);
    let dur = format_duration(&out);
    let frames = count_video_frames(&out);
    let vdur = track_duration(video_track(&m));
    let adur: Vec<f64> = audio_tracks(&m).iter().map(|t| track_duration(t)).collect();
    println!("joined: duration={dur:.3} frames={frames} video_track_dur={vdur:.3} audio_track_dur={adur:?}");
    assert!((dur - 6.0).abs() <= 0.15, "joined duration {dur}");
    assert_eq!(frames, 180, "joined frames");
    for a in &adur {
        assert!((vdur - a).abs() <= 0.15, "audio {a} vs video {vdur}");
    }
}

#[test]
fn t2_letterbox_4x3_onto_16x9() {
    let root = TempDir::new().unwrap();
    let dir = out_dir(&root, "lb");
    let p = params(&dir, 1280, 720, 30, "h264_nvenc", 6000, 2.0);
    let mut enc = NativeEncoder::start(p, None, &[]).expect("start");
    let white = vec![255u8; 640 * 480 * 4];
    for _ in 0..30 {
        enc.push_bgra(&white, 640, 480).expect("push");
    }
    let rep = enc.finish().expect("finish");
    let seg0 = &rep.segments[0];
    let gray = first_frame_luma(seg0);
    assert_eq!(gray.len(), 1280 * 720, "gray frame size");
    let bar = gray[360 * 1280 + 10];
    let centre = gray[360 * 1280 + 640];
    println!("letterbox: bar(col10)={bar} centre={centre}");
    assert!(bar < 25, "bar luma {bar}");
    assert!(centre > 220, "centre luma {centre}");
}

#[test]
fn t3_nvenc_hevc_one_segment() {
    let root = TempDir::new().unwrap();
    let dir = out_dir(&root, "hevc");
    let p = params(&dir, 1280, 720, 30, "hevc_nvenc", 6000, 10.0);
    let mut enc = NativeEncoder::start(p, None, &[]).expect("start");
    println!("encoder_name = {}", enc.encoder_name());
    for i in 0..60u32 {
        enc.push_bgra(&gradient(1280, 720, i), 1280, 720)
            .expect("push");
    }
    let rep = enc.finish().expect("finish");
    println!(
        "segments={} frames_in={}",
        rep.segments.len(),
        rep.frames_in
    );
    assert_eq!(rep.segments.len(), 1);
    let seg = &rep.segments[0];
    let m = movie(seg);
    let TrackKind::Video { codec, .. } = &video_track(&m).kind else {
        unreachable!("video_track returns a video track")
    };
    println!("codec = {codec}");
    assert_eq!(codec, "hevc");
    assert_eq!(count_video_frames(seg), 60);
    let (ok, err) = decode_clean(seg);
    assert!(ok && err.trim().is_empty(), "decode: {err}");
}

#[test]
fn t4_software_h264_640x360() {
    let root = TempDir::new().unwrap();
    let dir = out_dir(&root, "sw");
    let p = params(&dir, 640, 360, 30, "libx264", 2000, 2.0);
    let mut enc = NativeEncoder::start(p, None, &[]).expect("start");
    let name = enc.encoder_name().to_string();
    println!("encoder_name = {name}");
    for i in 0..60u32 {
        enc.push_bgra(&gradient(640, 360, i), 640, 360)
            .expect("push");
    }
    let rep = enc.finish().expect("finish");
    println!(
        "segments={} frames_in={}",
        rep.segments.len(),
        rep.frames_in
    );
    assert!(name.contains("H264"), "encoder_name {name}");
    assert!(!rep.segments.is_empty());
    let mut total = 0;
    for seg in &rep.segments {
        let n = count_video_frames(seg);
        total += n;
        let (ok, err) = decode_clean(seg);
        assert!(
            ok && err.trim().is_empty(),
            "{}: decode: {err}",
            seg.display()
        );
        println!("{}: frames={n}", seg.display());
    }
    assert_eq!(total, 60, "total frames across segments");
}

#[test]
fn t5_black_then_gradient_first_frame_is_dark() {
    let root = TempDir::new().unwrap();
    let dir = out_dir(&root, "black");
    let p = params(&dir, 1280, 720, 30, "h264_nvenc", 6000, 10.0);
    let mut enc = NativeEncoder::start(p, None, &[]).expect("start");
    for _ in 0..30 {
        enc.push_black().expect("push_black");
    }
    for i in 0..30u32 {
        enc.push_bgra(&gradient(1280, 720, i), 1280, 720)
            .expect("push");
    }
    let rep = enc.finish().expect("finish");
    let gray = first_frame_luma(&rep.segments[0]);
    let dark = gray.iter().filter(|&&y| y < 25).count();
    let frac = dark as f64 / gray.len() as f64;
    println!("first frame: dark fraction = {frac:.4}");
    assert!(frac >= 0.9, "dark fraction {frac}");
}

#[test]
fn t6_push_texture_red_on_device() {
    let root = TempDir::new().unwrap();
    let dir = out_dir(&root, "tex");
    let p = params(&dir, 1280, 720, 30, "h264_nvenc", 6000, 10.0);
    let mut enc = NativeEncoder::start(p, None, &[]).expect("start");

    // Solid red in BGRA: B=0 G=0 R=255 A=255.
    let pixels: Vec<u8> = [0u8, 0, 255, 255].repeat(1280 * 720);
    let desc = D3D11_TEXTURE2D_DESC {
        Width: 1280,
        Height: 720,
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
        pSysMem: pixels.as_ptr() as *const _,
        SysMemPitch: 1280 * 4,
        SysMemSlicePitch: 0,
    };
    let mut tex: Option<ID3D11Texture2D> = None;
    // SAFETY: desc/init are valid for the call; pixels outlives it; out slot is a live Option.
    unsafe {
        enc.device().CreateTexture2D(
            &desc as *const _,
            Some(&init as *const _),
            Some(&mut tex as *mut _),
        )
    }
    .expect("CreateTexture2D");
    let tex = tex.expect("texture");

    for _ in 0..60 {
        enc.push_texture(&tex).expect("push_texture");
    }
    let rep = enc.finish().expect("finish");
    let seg = &rep.segments[0];
    let (ok, err) = decode_clean(seg);
    assert!(ok && err.trim().is_empty(), "decode: {err}");
    assert_eq!(count_video_frames(seg), 60);

    let mut dec = VideoDecoder::open(seg, OutFormat::Bgra).expect("mfdec open");
    let f = dec
        .next_frame()
        .expect("mfdec next_frame")
        .expect("first frame");
    assert_eq!(f.data.len(), 1280 * 720 * 4);
    let i = (360 * 1280 + 640) * 4;
    let (b, g, r) = (f.data[i], f.data[i + 1], f.data[i + 2]);
    println!("centre rgb = ({r},{g},{b})");
    assert!(r > 180 && g < 70 && b < 70, "centre pixel ({r},{g},{b})");
}

#[test]
fn t7_wall_clock_drops_when_ahead_then_paced_encodes() {
    let root = TempDir::new().unwrap();
    let dir = out_dir(&root, "wall");
    let mut p = params(&dir, 640, 360, 30, "h264_nvenc", 3000, 2.0);
    p.wall_clock = true;
    let mut enc = NativeEncoder::start(p, None, &[]).expect("start");

    let frame = gradient(640, 360, 0);
    let t0 = Instant::now();
    for _ in 0..30 {
        enc.push_bgra(&frame, 640, 360).expect("burst push");
    }
    let burst_elapsed = t0.elapsed();
    let d1 = enc.frames_dropped();
    println!("burst: elapsed={burst_elapsed:?} dropped_after_burst={d1}");
    assert!(
        d1 > 0,
        "expected drops when producer runs ahead of real time"
    );

    for _ in 0..30 {
        thread::sleep(Duration::from_millis(1000 / 30));
        enc.push_bgra(&frame, 640, 360).expect("paced push");
    }
    let d2 = enc.frames_dropped() - d1;
    println!("paced: dropped_during_paced={d2}");
    assert!(d2 < 15, "paced phase dropped {d2} of 30");

    let rep = enc.finish().expect("finish");
    println!(
        "frames_in={} segments={} exit_ok={}",
        rep.frames_in,
        rep.segments.len(),
        rep.exit_ok
    );
    assert!(
        rep.exit_ok,
        "finish exit_ok false; stderr tail: {}",
        rep.stderr_tail
    );
}

#[test]
fn t8_error_kinds() {
    let root = TempDir::new().unwrap();
    let dir = out_dir(&root, "err");

    // start() validation
    let p = params(&dir, 641, 720, 30, "h264_nvenc", 6000, 2.0);
    assert_eq!(
        kind_of(NativeEncoder::start(p, None, &[])),
        Some(ErrorKind::InvalidInput),
        "odd width"
    );
    let p = params(&dir, 1280, 720, 0, "h264_nvenc", 6000, 2.0);
    assert_eq!(
        kind_of(NativeEncoder::start(p, None, &[])),
        Some(ErrorKind::InvalidInput),
        "fps 0"
    );
    let p = params(&dir, 1280, 720, 30, "h264_nvenc", 6000, 0.0);
    assert_eq!(
        kind_of(NativeEncoder::start(p, None, &[])),
        Some(ErrorKind::InvalidInput),
        "segment_secs 0"
    );

    // push validation on a valid encoder
    let p = params(&dir, 1280, 720, 30, "h264_nvenc", 6000, 2.0);
    let audio = audio_specs(&["Game"]);
    let mut enc = NativeEncoder::start(p, None, &audio).expect("start");
    assert_eq!(
        kind_of(enc.push_bgra(&[0u8; 10], 1280, 720)),
        Some(ErrorKind::InvalidInput),
        "push_bgra wrong length"
    );
    assert_eq!(
        kind_of(enc.push_audio(0, &[0.0f32; 3])),
        Some(ErrorKind::InvalidInput),
        "push_audio odd length"
    );
    assert_eq!(
        kind_of(enc.push_audio(9, &[0.0f32; 4])),
        Some(ErrorKind::InvalidInput),
        "push_audio track 9"
    );
    let _ = enc.finish();
    println!("error-kind checks passed");
}
