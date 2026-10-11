//! End-to-end tests for the native capture -> encode pipeline (D57).
//!
//! A test window is created here, recorded through `start_native_capture`, and the output is
//! checked with the native readers: `clipcheck_mp4` for container facts and remux, and
//! `clipcheck_mfdec` for decoding. No graphics/NVENC support prints `SKIPPED:` unless
//! `CLIPCHECK_REQUIRE_CAPTURE` is set.

#![cfg(windows)]

use std::f64::consts::PI;
use std::ffi::c_void;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::ptr::null;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use clipcheck_encode::pipe::AudioTrackSpec;
use clipcheck_encode::tuning::EncoderTuning;
use clipcheck_engine::native_pipeline::{native_params, start_native_capture, AudioInputs};
use clipcheck_engine::pipeline::PipelineReport;
use clipcheck_mfdec::{OutFormat, VideoDecoder};
use clipcheck_mp4::reader::{read_movie, Movie, Track, TrackKind};
use clipcheck_mp4::remux::{concat_remux, RemuxOptions};

// COLORREF is 0x00BBGGRR.
const BLUE: u32 = 0x00FF_0000;
const RED: u32 = 0x0000_00FF;
const BLUE_RGB: [u8; 3] = [0, 0, 255];
const CANVAS: (u32, u32) = (1280, 720);
const FPS: u32 = 30;
const TOLERANCE: i32 = 24;

const WS_OVERLAPPEDWINDOW: u32 = 0x00CF_0000;
const WS_VISIBLE: u32 = 0x1000_0000;
const SW_HIDE: i32 = 0;
const SW_SHOW: i32 = 5;
const WM_DESTROY: u32 = 0x0002;
const WM_CLOSE: u32 = 0x0010;
const WM_PAINT: u32 = 0x000F;
const WM_APP: u32 = 0x8000;
const WM_REPAINT: u32 = WM_APP + 1;
const CLASS_NAME: &str = "clipcheck_native_pipeline_test_class";

/// Fill colour used by the window procedure; changed from the test thread.
static COLOR: AtomicU32 = AtomicU32::new(BLUE);

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Point {
    x: i32,
    y: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Msg {
    hwnd: isize,
    message: u32,
    wparam: usize,
    lparam: isize,
    time: u32,
    pt: Point,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct PaintStruct {
    hdc: isize,
    erase: i32,
    paint: Rect,
    restore: i32,
    inc_update: i32,
    reserved: [u8; 32],
}

type WndProc = unsafe extern "system" fn(isize, u32, usize, isize) -> isize;

#[repr(C)]
struct WndClassW {
    style: u32,
    lpfn_wnd_proc: WndProc,
    cb_cls_extra: i32,
    cb_wnd_extra: i32,
    h_instance: isize,
    h_icon: isize,
    h_cursor: isize,
    hbr_background: isize,
    lpsz_menu_name: *const u16,
    lpsz_class_name: *const u16,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(name: *const u16) -> isize;
}

#[link(name = "user32")]
unsafe extern "system" {
    fn RegisterClassW(wc: *const WndClassW) -> u16;
    fn CreateWindowExW(
        ex_style: u32,
        class: *const u16,
        title: *const u16,
        style: u32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        parent: isize,
        menu: isize,
        instance: isize,
        param: *const c_void,
    ) -> isize;
    fn DefWindowProcW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize;
    fn ShowWindow(hwnd: isize, cmd: i32) -> i32;
    fn GetMessageW(msg: *mut Msg, hwnd: isize, min: u32, max: u32) -> i32;
    fn TranslateMessage(msg: *const Msg) -> i32;
    fn DispatchMessageW(msg: *const Msg) -> isize;
    fn PostMessageW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> i32;
    fn PostQuitMessage(code: i32);
    fn InvalidateRect(hwnd: isize, rect: *const Rect, erase: i32) -> i32;
    fn BeginPaint(hwnd: isize, ps: *mut PaintStruct) -> isize;
    fn EndPaint(hwnd: isize, ps: *const PaintStruct) -> i32;
    fn GetClientRect(hwnd: isize, rect: *mut Rect) -> i32;
    fn FillRect(hdc: isize, rect: *const Rect, brush: isize) -> i32;
    fn DestroyWindow(hwnd: isize) -> i32;
}

#[link(name = "gdi32")]
unsafe extern "system" {
    fn CreateSolidBrush(color: u32) -> isize;
    fn DeleteObject(obj: isize) -> i32;
}

unsafe extern "system" fn wnd_proc(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize {
    unsafe {
        match msg {
            WM_PAINT => {
                let mut ps = PaintStruct::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                let mut rc = Rect::default();
                GetClientRect(hwnd, &mut rc);
                let brush = CreateSolidBrush(COLOR.load(Ordering::SeqCst));
                FillRect(hdc, &rc, brush);
                DeleteObject(brush);
                EndPaint(hwnd, &ps);
                0
            }
            WM_REPAINT => {
                InvalidateRect(hwnd, null(), 1);
                0
            }
            WM_CLOSE => {
                DestroyWindow(hwnd);
                0
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Runs on the window's own thread: owns the HWND and pumps its messages.
fn window_thread(tx: Sender<isize>) {
    unsafe {
        let inst = GetModuleHandleW(null());
        let class = wide(CLASS_NAME);
        let wc = WndClassW {
            style: 0,
            lpfn_wnd_proc: wnd_proc,
            cb_cls_extra: 0,
            cb_wnd_extra: 0,
            h_instance: inst,
            h_icon: 0,
            h_cursor: 0,
            hbr_background: 0,
            lpsz_menu_name: null(),
            lpsz_class_name: class.as_ptr(),
        };
        // Failure here just means the class already exists in this process.
        RegisterClassW(&wc);
        let title = wide("clipcheck native pipeline test");
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            100,
            100,
            800,
            600,
            0,
            0,
            inst,
            null(),
        );
        let _ = tx.send(hwnd);
        if hwnd == 0 {
            return;
        }
        let mut msg = Msg::default();
        while GetMessageW(&mut msg, 0, 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Owns the window thread. Dropping it posts WM_CLOSE and joins the thread.
struct TestWindow {
    hwnd: isize,
    thread: Option<JoinHandle<()>>,
}

impl TestWindow {
    fn create() -> TestWindow {
        COLOR.store(BLUE, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel::<isize>();
        let thread = thread::spawn(move || window_thread(tx));
        let hwnd = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("window thread did not report an HWND within 5 s");
        assert!(hwnd != 0, "CreateWindowExW returned NULL");
        TestWindow {
            hwnd,
            thread: Some(thread),
        }
    }

    fn show(&self, cmd: i32) {
        unsafe { ShowWindow(self.hwnd, cmd) };
    }

    /// Changes the fill colour and asks the window thread to repaint.
    fn set_color_and_repaint(&self, color: u32) {
        COLOR.store(color, Ordering::SeqCst);
        unsafe { PostMessageW(self.hwnd, WM_REPAINT, 0, 0) };
    }
}

impl Drop for TestWindow {
    fn drop(&mut self) {
        unsafe { PostMessageW(self.hwnd, WM_CLOSE, 0, 0) };
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Sends 10 ms blocks of a 440 Hz stereo sine (480 frames = 960 f32) in real time until `stop`.
fn spawn_feeder(tx: Sender<Vec<f32>>, stop: Arc<AtomicBool>) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut n: u64 = 0;
        let mut next = Instant::now();
        while !stop.load(Ordering::Acquire) {
            let mut block = Vec::with_capacity(960);
            for _ in 0..480 {
                let s = ((2.0 * PI * 440.0 * n as f64) / 48_000.0).sin() as f32 * 0.2;
                block.push(s);
                block.push(s);
                n += 1;
            }
            if tx.send(block).is_err() {
                break;
            }
            next += Duration::from_millis(10);
            let now = Instant::now();
            if next > now {
                thread::sleep(next - now);
            } else {
                next = now;
            }
        }
    })
}

fn skip_or_panic(test: &str, reason: &str) {
    if std::env::var_os("CLIPCHECK_REQUIRE_CAPTURE").is_some() {
        panic!("{test}: prerequisite missing: {reason} (CLIPCHECK_REQUIRE_CAPTURE is set)");
    }
    eprintln!("SKIPPED: {test}: {reason}");
}

/// True when a start error looks like this session lacking D3D/WGC/NVENC support.
fn is_env_error(e: &io::Error) -> bool {
    let s = e.to_string().to_ascii_lowercase();
    [
        "d3d",
        "dxgi",
        "adapter",
        "graphics capture",
        "not supported",
        "unsupported",
        "nvenc",
        "no encoder",
    ]
    .iter()
    .any(|k| s.contains(k))
}

/// Reads the MP4 structure of `file` with the native reader.
fn movie(file: &Path) -> Movie {
    read_movie(file).unwrap_or_else(|e| panic!("{}: mp4 read failed: {e}", file.display()))
}

fn video_tracks(m: &Movie) -> Vec<&Track> {
    m.tracks.iter().filter(|t| t.is_video()).collect()
}

fn audio_tracks(m: &Movie) -> Vec<&Track> {
    m.tracks.iter().filter(|t| t.is_audio()).collect()
}

/// (codec, width, height) of a video track.
fn video_info(t: &Track) -> (String, u32, u32) {
    match &t.kind {
        TrackKind::Video {
            codec,
            width,
            height,
        } => (codec.clone(), *width, *height),
        _ => panic!("not a video track"),
    }
}

/// (codec, sample rate) of an audio track.
fn audio_info(t: &Track) -> (String, u32) {
    match &t.kind {
        TrackKind::Audio {
            codec, sample_rate, ..
        } => (codec.clone(), *sample_rate),
        _ => panic!("not an audio track"),
    }
}

/// Opens `seg` with the Media Foundation decoder, optionally seeking to `seek` seconds first.
fn open_decoder(seg: &Path, seek: Option<f64>) -> VideoDecoder {
    let mut dec = VideoDecoder::open(seg, OutFormat::Bgra)
        .unwrap_or_else(|e| panic!("{}: decoder open failed: {e}", seg.display()));
    if let Some(s) = seek {
        dec.seek(s)
            .unwrap_or_else(|e| panic!("{}: seek to {s} s failed: {e}", seg.display()));
    }
    assert_eq!(
        (dec.width(), dec.height()),
        CANVAS,
        "{}: decoded frame size",
        seg.display()
    );
    dec
}

/// Number of video frames `seg` decodes to. Any decoder error fails the test.
fn decode_count(seg: &Path) -> usize {
    let mut dec = open_decoder(seg, None);
    let mut n = 0usize;
    loop {
        match dec.next_frame() {
            Ok(Some(_)) => n += 1,
            Ok(None) => break,
            Err(e) => panic!("{}: decode failed after {n} frames: {e}", seg.display()),
        }
    }
    n
}

/// Packed RGB24 frames of `seg`, optionally seeked to `seek` seconds and limited to `max_frames`
/// frames (all frames when `None`). Frames are converted from the decoder's BGRA output.
fn decode_rgb(seg: &Path, seek: Option<f64>, max_frames: Option<usize>) -> Vec<u8> {
    let mut dec = open_decoder(seg, seek);
    let mut rgb = Vec::new();
    let mut n = 0usize;
    loop {
        if let Some(m) = max_frames {
            if n >= m {
                break;
            }
        }
        match dec.next_frame() {
            Ok(Some(f)) => {
                for px in f.data.as_chunks::<4>().0 {
                    rgb.extend_from_slice(&[px[2], px[1], px[0]]);
                }
                n += 1;
            }
            Ok(None) => break,
            Err(e) => panic!("{}: decode failed after {n} frames: {e}", seg.display()),
        }
    }
    rgb
}

/// Structural checks on one segment: one H.264 video track at the canvas size whose first sample
/// is a keyframe, one AAC audio track at 48 kHz with packets, and a clean decode of every frame.
fn check_segment(seg: &Path) {
    let name = seg.display();
    let m = movie(seg);
    let video = video_tracks(&m);
    let audio = audio_tracks(&m);
    assert_eq!(video.len(), 1, "{name}: expected 1 video track");
    assert_eq!(audio.len(), 1, "{name}: expected 1 audio track");
    let (vcodec, w, h) = video_info(video[0]);
    assert_eq!(vcodec, "h264", "{name}: codec");
    assert_eq!((w, h), CANVAS, "{name}: size");
    let (acodec, rate) = audio_info(audio[0]);
    assert_eq!(acodec, "aac", "{name}: audio codec");
    assert_eq!(rate, 48_000, "{name}: audio sample rate");
    assert!(!audio[0].samples.is_empty(), "{name}: audio has no packets");
    assert!(
        matches!(video[0].samples.first(), Some(s) if s.keyframe),
        "{name}: first frame is not a keyframe"
    );
    assert!(decode_count(seg) > 0, "{name}: no decodable video frames");
}

fn frame_bytes() -> usize {
    CANVAS.0 as usize * CANVAS.1 as usize * 3
}

/// RGB of the canvas centre in frame `idx` of a raw rgb24 buffer.
fn centre_rgb(buf: &[u8], idx: usize) -> [u8; 3] {
    let stride = CANVAS.0 as usize * 3;
    let base = idx * frame_bytes() + (CANVAS.1 as usize / 2) * stride + (CANVAS.0 as usize / 2) * 3;
    [buf[base], buf[base + 1], buf[base + 2]]
}

fn near_rgb(px: [u8; 3], want: [u8; 3]) -> bool {
    (0..3).all(|i| (px[i] as i32 - want[i] as i32).abs() <= TOLERANCE)
}

fn is_dark(px: [u8; 3]) -> bool {
    px.iter().all(|&c| c < 40)
}

/// Parses `segments.csv` rows (`name,start,end`). Returns the rows and the raw non-empty line count.
fn read_rows(dir: &Path) -> (Vec<(String, f64, f64)>, usize) {
    let text = fs::read_to_string(dir.join("segments.csv")).unwrap_or_default();
    let raw = text.lines().filter(|l| !l.trim().is_empty()).count();
    let rows = text
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.trim().split(',').collect();
            if f.len() != 3 {
                return None;
            }
            Some((f[0].to_string(), f[1].parse().ok()?, f[2].parse().ok()?))
        })
        .collect();
    (rows, raw)
}

struct Recording {
    dir: tempfile::TempDir,
    report: PipelineReport,
    rows: Vec<(String, f64, f64)>,
    raw_rows: usize,
    /// Actual (hide, show) times in seconds from the recording start, when the window was hidden.
    hidden: Option<(f64, f64)>,
}

/// Records a test window for `secs` with a 440 Hz "Mix" audio lane. `toggle` flips the window
/// between blue and red once per second. `hide` is (from, to) seconds for SW_HIDE then SW_SHOW.
/// Returns `None` after a SKIPPED message when the session cannot capture.
fn record(test: &str, secs: f64, toggle: bool, hide: Option<(f64, f64)>) -> Option<Recording> {
    let dir = tempfile::tempdir().expect("tempdir");
    let params = native_params(
        "h264_nvenc",
        FPS,
        CANVAS,
        6000,
        &EncoderTuning::default(),
        dir.path().to_path_buf(),
    )
    .expect("h264_nvenc must map to native params");

    let win = TestWindow::create();
    let feed_stop = Arc::new(AtomicBool::new(false));
    let (feed_tx, feed_rx) = mpsc::channel::<Vec<f32>>();
    let feeder = spawn_feeder(feed_tx, feed_stop.clone());
    let audio: AudioInputs = vec![(
        AudioTrackSpec {
            title: "Mix".into(),
            bitrate_kbps: 160,
        },
        feed_rx,
    )];

    let start = Instant::now();
    let cap = match start_native_capture(win.hwnd, FPS, params, audio) {
        Ok(c) => c,
        Err((e, _)) => {
            feed_stop.store(true, Ordering::Release);
            let _ = feeder.join();
            if is_env_error(&e) {
                skip_or_panic(test, &format!("native capture unavailable: {e}"));
                return None;
            }
            panic!("{test}: start_native_capture failed: {e}");
        }
    };

    let total = Duration::from_secs_f64(secs);
    let mut last_sec = u64::MAX;
    let mut hidden_from: Option<f64> = None;
    let mut hidden_span: Option<(f64, f64)> = None;
    loop {
        let el = start.elapsed();
        if el >= total {
            break;
        }
        if toggle {
            let s = el.as_secs();
            if s != last_sec {
                last_sec = s;
                win.set_color_and_repaint(if s % 2 == 1 { RED } else { BLUE });
            }
        }
        if let Some((from, to)) = hide {
            let now = el.as_secs_f64();
            if hidden_from.is_none() && now >= from {
                win.show(SW_HIDE);
                hidden_from = Some(now);
                eprintln!("{test}: window hidden at {now:.2} s");
            }
            if let Some(h) = hidden_from {
                if hidden_span.is_none() && now >= to {
                    win.show(SW_SHOW);
                    hidden_span = Some((h, now));
                    eprintln!("{test}: window shown again at {now:.2} s");
                }
            }
        }
        thread::sleep(Duration::from_millis(20));
    }

    feed_stop.store(true, Ordering::Release);
    let _ = feeder.join();
    let report = cap
        .stop()
        .unwrap_or_else(|e| panic!("{test}: NativeCapture::stop failed: {e}"));
    drop(win);

    let (rows, raw_rows) = read_rows(dir.path());
    Some(Recording {
        dir,
        report,
        rows,
        raw_rows,
        hidden: hidden_span,
    })
}

#[test]
fn native_params_maps_supported_encoders_only() {
    let tuning = EncoderTuning::default();
    let dir = PathBuf::from("unused");
    for name in ["h264_nvenc", "hevc_nvenc", "libx264"] {
        let p = native_params(name, FPS, CANVAS, 6000, &tuning, dir.clone());
        assert!(p.is_some(), "{name} should map to native params");
    }
    for name in ["av1_nvenc", "libx265"] {
        let p = native_params(name, FPS, CANVAS, 6000, &tuning, dir.clone());
        assert!(p.is_none(), "{name} should not map to native params");
    }
    // Odd canvas sizes round down to even; segments are 2 s and wall-clock aligned.
    let p =
        native_params("h264_nvenc", FPS, (1281, 721), 6000, &tuning, dir).expect("h264_nvenc maps");
    assert_eq!(
        (p.width, p.height),
        (1280, 720),
        "canvas not rounded to even"
    );
    assert_eq!(p.fps, FPS);
    assert!(
        (p.segment_secs - 2.0).abs() < f32::EPSILON,
        "segment_secs {}",
        p.segment_secs
    );
    assert!(
        p.wall_clock,
        "wall_clock should be on for the live recorder"
    );
}

#[test]
fn native_pipeline_rejects_invalid_hwnd_and_returns_audio() {
    let dir = tempfile::tempdir().expect("tempdir");
    let params = native_params(
        "h264_nvenc",
        FPS,
        CANVAS,
        6000,
        &EncoderTuning::default(),
        dir.path().to_path_buf(),
    )
    .expect("h264_nvenc maps");
    let (tx, rx) = mpsc::channel::<Vec<f32>>();
    let audio: AudioInputs = vec![(
        AudioTrackSpec {
            title: "Mix".into(),
            bitrate_kbps: 160,
        },
        rx,
    )];
    match start_native_capture(0x1234, FPS, params, audio) {
        Ok(cap) => {
            let _ = cap.stop();
            panic!("invalid hwnd 0x1234 was accepted");
        }
        Err((e, back)) => {
            eprintln!("invalid hwnd error: {e}");
            assert_eq!(back.len(), 1, "audio lanes not returned on error");
            assert_eq!(back[0].0.title, "Mix", "returned lane has wrong spec");
        }
    }
    drop(tx);
}

#[test]
fn native_pipeline_records_segments_with_audio() {
    const TEST: &str = "native_pipeline_records_segments_with_audio";
    let Some(rec) = record(TEST, 7.0, true, None) else {
        return;
    };
    let r = &rec.report;
    eprintln!(
        "{TEST}: frames_in={} dropped={} audio_dropped={} segments={} csv_rows={} exit_ok={}",
        r.frames_in,
        r.dropped,
        r.audio_dropped,
        r.encode.segments.len(),
        rec.raw_rows,
        r.encode.exit_ok
    );
    eprintln!("{TEST}: encoder stderr tail: {:?}", r.encode.stderr_tail);

    assert!(
        (190..=225).contains(&r.frames_in),
        "frames_in {} outside [190, 225] for 7 s at 30 fps",
        r.frames_in
    );
    assert!(
        r.encode.segments.len() >= 3,
        "expected >= 3 segments, got {}",
        r.encode.segments.len()
    );
    assert!(
        r.encode.exit_ok,
        "encoder exit not ok: {}",
        r.encode.stderr_tail
    );

    // CSV rows: contiguous, 2.0 s each except the last, which may be shorter.
    assert!(
        rec.rows.len() >= 3,
        "segments.csv has {} parsable rows (raw {})",
        rec.rows.len(),
        rec.raw_rows
    );
    assert_eq!(
        rec.rows.len(),
        r.encode.segments.len(),
        "csv rows vs segment files"
    );
    let last = rec.rows.len() - 1;
    for (name, start, end) in &rec.rows {
        eprintln!(
            "{TEST}: {name} start={start:.3} end={end:.3} dur={:.3}",
            end - start
        );
    }
    // Timing problems are collected and asserted after the remaining checks have run.
    let mut problems: Vec<String> = Vec::new();
    for (i, (name, start, end)) in rec.rows.iter().enumerate() {
        let d = end - start;
        if i < last && (d - 2.0).abs() > 0.1 {
            problems.push(format!("{name}: duration {d:.3} not 2.0 +- 0.1"));
        }
        if i == last && !(d > 0.0 && d <= 2.1) {
            problems.push(format!("{name}: last duration {d:.3} out of range"));
        }
        if i > 0 {
            let prev_end = rec.rows[i - 1].2;
            if (start - prev_end).abs() > 0.05 {
                problems.push(format!(
                    "{name}: starts at {start:.3}, previous ended at {prev_end:.3}"
                ));
            }
        }
    }

    for (name, _, _) in &rec.rows {
        check_segment(&rec.dir.path().join(name));
    }

    // Concat the way the app does: flat MP4 with faststart, via the native remuxer.
    let inputs: Vec<PathBuf> = rec
        .rows
        .iter()
        .map(|(n, _, _)| rec.dir.path().join(n))
        .collect();
    let joined = rec.dir.path().join("joined.mp4");
    let rep = concat_remux(&inputs, &joined, &RemuxOptions::default())
        .unwrap_or_else(|e| panic!("{TEST}: concat_remux failed: {e}"));
    eprintln!(
        "{TEST}: remux bytes={} duration={:.3}",
        rep.bytes, rep.duration_secs
    );
    let jm = movie(&joined);
    let v_dur = video_tracks(&jm)
        .first()
        .map(|t| t.duration_secs())
        .expect("joined video track");
    let a_dur = audio_tracks(&jm)
        .first()
        .map(|t| t.duration_secs())
        .expect("joined audio track");
    eprintln!("{TEST}: joined video={v_dur:.3} s audio={a_dur:.3} s");
    assert!(
        (v_dur - 7.0).abs() <= 0.6,
        "joined video duration {v_dur:.3} not 7.0 +- 0.6"
    );
    // Video runs on the wall clock and ends at the last frame the capture delivered (WGC only
    // delivers on change, and stopping the capture takes a few frame intervals), while audio
    // runs to the stop moment in whole 1024-sample AAC frames. Measured over 20 runs, audio
    // exceeds video by 0.22-0.26 s, a stable offset rather than drift. Allowed: one capture
    // frame + one AAC frame + 0.35 s of capture start/stop latency (about 0.41 s), which still
    // catches a real desync (a lane running to a different clock drifts by seconds).
    let av_tolerance = 1.0 / 30.0 + 1024.0 / 48_000.0 + 0.35;
    assert!(
        (a_dur - v_dur).abs() < av_tolerance,
        "A/V duration mismatch: audio {a_dur:.3} vs video {v_dur:.3}"
    );
    assert!(
        problems.is_empty(),
        "segment timing problems:\n  {}",
        problems.join("\n  ")
    );
}

#[test]
fn native_pipeline_frame_shows_window_colour() {
    const TEST: &str = "native_pipeline_frame_shows_window_colour";
    let Some(rec) = record(TEST, 3.0, false, None) else {
        return;
    };
    let seg = rec.dir.path().join(&rec.rows[0].0);
    let buf = decode_rgb(&seg, Some(1.0), Some(1));
    assert_eq!(
        buf.len(),
        frame_bytes(),
        "decoded frame is not {}x{} rgb24",
        CANVAS.0,
        CANVAS.1
    );

    let centre = centre_rgb(&buf, 0);
    eprintln!("{TEST}: centre rgb={centre:?} want {BLUE_RGB:?}");
    assert!(
        !is_dark(centre),
        "centre pixel is black, window content missing"
    );

    // 9x9 grid around the centre: most samples should be the window colour.
    let mut matches = 0usize;
    let mut total = 0usize;
    for dy in -4i64..=4 {
        for dx in -4i64..=4 {
            let x = (CANVAS.0 as i64 / 2 + dx * 8) as usize;
            let y = (CANVAS.1 as i64 / 2 + dy * 8) as usize;
            let base = y * CANVAS.0 as usize * 3 + x * 3;
            let px = [buf[base], buf[base + 1], buf[base + 2]];
            total += 1;
            if near_rgb(px, BLUE_RGB) {
                matches += 1;
            }
        }
    }
    eprintln!("{TEST}: grid matches {matches}/{total} within +-{TOLERANCE} of blue");
    assert!(
        matches * 100 >= total * 70,
        "only {matches}/{total} grid samples match the window colour"
    );
}

#[test]
fn native_pipeline_survives_window_hidden_period() {
    const TEST: &str = "native_pipeline_survives_window_hidden_period";
    let Some(rec) = record(TEST, 7.0, false, Some((2.5, 4.0))) else {
        return;
    };
    let (hide_at, show_at) = rec.hidden.expect("window was never hidden");
    eprintln!(
        "{TEST}: frames_in={} dropped={} segments={} hidden {hide_at:.2}..{show_at:.2}",
        rec.report.frames_in,
        rec.report.dropped,
        rec.report.encode.segments.len()
    );
    assert!(
        rec.report.encode.exit_ok,
        "encoder exit not ok: {}",
        rec.report.encode.stderr_tail
    );
    assert!(
        rec.rows.len() >= 3,
        "expected >= 3 segments, got {}",
        rec.rows.len()
    );

    // Classify each decoded frame by its approximate time: frame i of a segment starting at
    // `start` is at start + i / FPS.
    let (mut pre, mut pre_dark) = (0usize, 0usize);
    let (mut mid, mut mid_dark) = (0usize, 0usize);
    let (mut post, mut post_dark) = (0usize, 0usize);
    for (name, start, _) in &rec.rows {
        let seg = rec.dir.path().join(name);
        check_segment(&seg);
        let buf = decode_rgb(&seg, None, None);
        let n = buf.len() / frame_bytes();
        for i in 0..n {
            let when = start + i as f64 / FPS as f64;
            let dark = is_dark(centre_rgb(&buf, i));
            if when < hide_at - 0.3 {
                pre += 1;
                pre_dark += usize::from(dark);
            } else if when > show_at + 0.3 {
                post += 1;
                post_dark += usize::from(dark);
            } else if when >= hide_at + 0.3 && when <= show_at - 0.3 {
                mid += 1;
                mid_dark += usize::from(dark);
            }
        }
    }
    eprintln!(
        "{TEST}: visible-before dark {pre_dark}/{pre}, hidden dark {mid_dark}/{mid}, visible-after dark {post_dark}/{post}"
    );
    assert!(
        pre > 0 && pre_dark * 10 <= pre,
        "frames before hide should show the window ({pre_dark}/{pre} dark)"
    );
    assert!(
        mid >= 15,
        "too few decoded frames in the hidden period ({mid})"
    );
    assert!(
        mid_dark * 10 >= mid * 8,
        "hidden period should be dark ({mid_dark}/{mid})"
    );
    assert!(
        post > 0 && post_dark * 10 <= post,
        "frames after show should show the window ({post_dark}/{post} dark)"
    );
}
