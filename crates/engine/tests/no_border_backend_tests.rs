//! Tester tests for capture-backend selection (`start_native_capture_with`).
//!
//! Covers: the Wgc backend (unchanged behaviour, no-border counter untouched), the NoBorder
//! backend falling back to window capture for a non-covering window, NoBorder on a topmost
//! window, and an invalid hwnd with NoBorder followed by a healthy Wgc recording.
//!
//! Only test-owned windows are recorded. Without graphics/NVENC support a test prints `SKIPPED:`
//! unless `CLIPCHECK_REQUIRE_CAPTURE` is set.

#![cfg(windows)]

use std::f64::consts::PI;
use std::ffi::c_void;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::ptr::null;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use clipcheck_capture::{no_border_attempts, CaptureBackend};
use clipcheck_encode::pipe::AudioTrackSpec;
use clipcheck_encode::tuning::EncoderTuning;
use clipcheck_engine::native_pipeline::{native_params, start_native_capture_with, AudioInputs};
use clipcheck_engine::pipeline::PipelineReport;
use clipcheck_mfdec::{OutFormat, VideoDecoder};
use clipcheck_mp4::reader::{read_movie, Movie, Track, TrackKind};
use clipcheck_mp4::remux::{concat_remux, RemuxOptions};

const BLUE: u32 = 0x00FF_0000;
const RED: u32 = 0x0000_00FF;
const CANVAS: (u32, u32) = (1280, 720);
const FPS: u32 = 30;

const WS_OVERLAPPEDWINDOW: u32 = 0x00CF_0000;
const WS_VISIBLE: u32 = 0x1000_0000;
const WM_DESTROY: u32 = 0x0002;
const WM_CLOSE: u32 = 0x0010;
const WM_PAINT: u32 = 0x000F;
const WM_APP: u32 = 0x8000;
const WM_REPAINT: u32 = WM_APP + 1;
const CLASS_NAME: &str = "clipcheck_no_border_backend_tester_class";
const HWND_TOPMOST: isize = -1;
const SWP_NOMOVE: u32 = 0x0002;
const SWP_NOSIZE: u32 = 0x0001;
const SWP_SHOWWINDOW: u32 = 0x0040;

/// Serialises the tests in this binary so the process-wide no-border counter deltas are exact.
static SERIAL: Mutex<()> = Mutex::new(());

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
    fn SetWindowPos(hwnd: isize, after: isize, x: i32, y: i32, cx: i32, cy: i32, flags: u32)
        -> i32;
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
        RegisterClassW(&wc);
        let title = wide("clipcheck no-border backend test");
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

/// Owns a test window thread. Dropping it posts WM_CLOSE and joins the thread.
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

    fn make_topmost(&self) {
        unsafe {
            SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
            )
        };
    }

    fn set_color_and_repaint(&self, color: u32) {
        COLOR.store(color, Ordering::SeqCst);
        unsafe { PostMessageW(self.hwnd, WM_REPAINT, 0, 0) };
    }

    #[allow(dead_code)]
    fn show(&self, cmd: i32) {
        unsafe { ShowWindow(self.hwnd, cmd) };
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

/// Sends 10 ms blocks of a 440 Hz stereo sine in real time until `stop`.
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

fn movie(file: &Path) -> Movie {
    read_movie(file).unwrap_or_else(|e| panic!("{}: mp4 read failed: {e}", file.display()))
}

fn video_tracks(m: &Movie) -> Vec<&Track> {
    m.tracks.iter().filter(|t| t.is_video()).collect()
}

fn audio_tracks(m: &Movie) -> Vec<&Track> {
    m.tracks.iter().filter(|t| t.is_audio()).collect()
}

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

fn audio_info(t: &Track) -> (String, u32) {
    match &t.kind {
        TrackKind::Audio {
            codec, sample_rate, ..
        } => (codec.clone(), *sample_rate),
        _ => panic!("not an audio track"),
    }
}

fn decode_count(seg: &Path) -> usize {
    let mut dec = VideoDecoder::open(seg, OutFormat::Bgra)
        .unwrap_or_else(|e| panic!("{}: decoder open failed: {e}", seg.display()));
    assert_eq!(
        (dec.width(), dec.height()),
        CANVAS,
        "{}: decoded frame size",
        seg.display()
    );
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

/// Same structural checks as the existing native pipeline tests, per segment.
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
}

/// Records `win` for `secs` through `backend` with a 440 Hz "Mix" lane. `toggle` flips the window
/// colour once per second. Returns `None` after SKIPPED when the session cannot capture.
fn record(
    test: &str,
    backend: CaptureBackend,
    win: &TestWindow,
    secs: f64,
    toggle: bool,
) -> Option<Recording> {
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
    let cap = match start_native_capture_with(win.hwnd, FPS, params, audio, backend) {
        Ok(c) => c,
        Err((e, _)) => {
            feed_stop.store(true, Ordering::Release);
            let _ = feeder.join();
            if is_env_error(&e) {
                skip_or_panic(test, &format!("native capture unavailable: {e}"));
                return None;
            }
            panic!("{test}: start_native_capture_with({backend:?}) failed: {e}");
        }
    };

    let total = Duration::from_secs_f64(secs);
    let mut last_sec = u64::MAX;
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
        thread::sleep(Duration::from_millis(20));
    }

    feed_stop.store(true, Ordering::Release);
    let _ = feeder.join();
    let report = cap
        .stop()
        .unwrap_or_else(|e| panic!("{test}: NativeCapture::stop failed: {e}"));

    let (rows, raw_rows) = read_rows(dir.path());
    Some(Recording {
        dir,
        report,
        rows,
        raw_rows,
    })
}

/// Shared output checks: encoder ok, frames captured, segments/CSV consistent, every segment valid,
/// and the concatenated output has matching A/V durations close to the recorded length.
fn verify(test: &str, rec: &Recording, secs: f64) {
    let r = &rec.report;
    eprintln!(
        "{test}: frames_in={} dropped={} audio_dropped={} segments={} csv_rows={} exit_ok={}",
        r.frames_in,
        r.dropped,
        r.audio_dropped,
        r.encode.segments.len(),
        rec.raw_rows,
        r.encode.exit_ok
    );
    assert!(r.frames_in > 0, "{test}: no frames captured");
    assert!(
        r.encode.exit_ok,
        "{test}: encoder exit not ok: {}",
        r.encode.stderr_tail
    );
    assert!(!rec.rows.is_empty(), "{test}: segments.csv has no rows");
    assert_eq!(
        rec.rows.len(),
        r.encode.segments.len(),
        "{test}: csv rows vs segment count"
    );
    for (name, _, _) in &rec.rows {
        check_segment(&rec.dir.path().join(name));
    }
    let inputs: Vec<PathBuf> = rec
        .rows
        .iter()
        .map(|(n, _, _)| rec.dir.path().join(n))
        .collect();
    let joined = rec.dir.path().join("joined.mp4");
    concat_remux(&inputs, &joined, &RemuxOptions::default())
        .unwrap_or_else(|e| panic!("{test}: concat_remux failed: {e}"));
    let jm = movie(&joined);
    let v_dur = video_tracks(&jm)
        .first()
        .map(|t| t.duration_secs())
        .expect("joined video track");
    let a_dur = audio_tracks(&jm)
        .first()
        .map(|t| t.duration_secs())
        .expect("joined audio track");
    eprintln!("{test}: joined video={v_dur:.3} s audio={a_dur:.3} s");
    assert!(
        (v_dur - secs).abs() <= 0.8,
        "{test}: joined video duration {v_dur:.3} not {secs} +- 0.8"
    );
    assert!(
        (a_dur - v_dur).abs() < 0.25,
        "{test}: A/V duration mismatch: audio {a_dur:.3} vs video {v_dur:.3}"
    );
}

fn lock() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

#[test]
fn wgc_backend_records_and_leaves_no_border_counter_alone() {
    const TEST: &str = "wgc_backend_records_and_leaves_no_border_counter_alone";
    let _g = lock();
    let win = TestWindow::create();
    let before = no_border_attempts();
    let Some(rec) = record(TEST, CaptureBackend::Wgc, &win, 7.0, true) else {
        return;
    };
    let after = no_border_attempts();
    verify(TEST, &rec, 7.0);
    let fi = rec.report.frames_in;
    assert!(
        (190..=225).contains(&fi),
        "{TEST}: frames_in {fi} outside [190, 225] for 7 s at 30 fps"
    );
    assert!(
        rec.report.encode.segments.len() >= 3,
        "{TEST}: expected >= 3 segments, got {}",
        rec.report.encode.segments.len()
    );
    assert_eq!(
        after, before,
        "{TEST}: Wgc must not enter the no-border source (before {before}, after {after})"
    );
}

#[test]
fn no_border_require_cover_small_window_falls_back_to_wgc() {
    const TEST: &str = "no_border_require_cover_small_window_falls_back_to_wgc";
    let _g = lock();
    let win = TestWindow::create();
    let before = no_border_attempts();
    let Some(rec) = record(
        TEST,
        CaptureBackend::NoBorder {
            require_cover: true,
        },
        &win,
        4.0,
        true,
    ) else {
        return;
    };
    let after = no_border_attempts();
    verify(TEST, &rec, 4.0);
    assert!(
        rec.report.frames_in > 0,
        "{TEST}: frame count is zero after fallback"
    );
    assert_eq!(
        after,
        before + 1,
        "{TEST}: no-border attempt counter should rise by exactly 1 (before {before}, after {after})"
    );
}

#[test]
fn no_border_topmost_window_records() {
    const TEST: &str = "no_border_topmost_window_records";
    let _g = lock();
    let win = TestWindow::create();
    win.make_topmost();
    thread::sleep(Duration::from_millis(300));
    let before = no_border_attempts();
    let Some(rec) = record(
        TEST,
        CaptureBackend::NoBorder {
            require_cover: false,
        },
        &win,
        4.0,
        true,
    ) else {
        return;
    };
    let after = no_border_attempts();
    verify(TEST, &rec, 4.0);
    assert_eq!(
        after,
        before + 1,
        "{TEST}: no-border attempt counter should rise by exactly 1 (before {before}, after {after})"
    );
}

#[test]
fn invalid_hwnd_with_no_border_returns_audio_then_wgc_still_records() {
    const TEST: &str = "invalid_hwnd_with_no_border_returns_audio_then_wgc_still_records";
    let _g = lock();

    // Part 1: invalid hwnd with the no-border backend must fail and return the audio lane.
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
    let before = no_border_attempts();
    match start_native_capture_with(
        0x1234,
        FPS,
        params,
        audio,
        CaptureBackend::NoBorder {
            require_cover: true,
        },
    ) {
        Ok(cap) => {
            let _ = cap.stop();
            panic!("{TEST}: invalid hwnd 0x1234 was accepted with NoBorder");
        }
        Err((e, back)) => {
            eprintln!("{TEST}: invalid hwnd error: {e}");
            assert_eq!(back.len(), 1, "{TEST}: audio lanes not returned on error");
            assert_eq!(
                back[0].0.title, "Mix",
                "{TEST}: returned lane has wrong spec"
            );
        }
    }
    let after = no_border_attempts();
    eprintln!("{TEST}: no-border counter before={before} after={after} (informational)");
    drop(tx);

    // Part 2: a healthy Wgc recording must still work afterwards (no leaked or doubled encoder).
    let win = TestWindow::create();
    let Some(rec) = record(TEST, CaptureBackend::Wgc, &win, 3.0, false) else {
        return;
    };
    verify(TEST, &rec, 3.0);
}
