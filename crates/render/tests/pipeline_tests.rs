//! Public-API tests for `clipcheck_render::pipeline` (fake providers and a collecting sink).
//! Synthetic data only. Canvas 320x180 at 30 fps unless a test says otherwise. Pixels are
//! read as BGRA `[b, g, r, a]`, the layout of `Frame`.
//! Never executed by the orchestrator's rules of the test chain; compile check only.

#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::f64::consts::PI;
use std::time::Duration;

use clipcheck_render::dsp::LoudnessMeter;
use clipcheck_render::frame::Frame;
use clipcheck_render::pipeline::{
    measure_loudness, run, AudioProvider, PipelineConfig, PipelineReport, Sink, TitleProvider,
    VideoProvider,
};
use clipcheck_render::{JobControl, Progress, RenderError};
use clipcheck_timeline::export::AudioLayout;
use clipcheck_timeline::model::{
    Canvas, Clip, ClipAudio, ClipKind, MediaKind, MediaRef, Project, Rgba, TextSpec, TrackKind,
    Transition, TransitionKind,
};
use clipcheck_timeline::plan::{derive_plan, RenderPlan};
use clipcheck_timeline::time::{Rate, Tick, TICKS_PER_SECOND};

const W: u32 = 320;
const H: u32 = 180;

// ---------------------------------------------------------------------------------------------
// Fakes
// ---------------------------------------------------------------------------------------------

/// Solid-colour video frames per media id. Records every request.
#[derive(Default)]
struct FakeVideo {
    /// Colour per media id as [r, g, b]. Unknown ids are white.
    colours: HashMap<String, [u8; 3]>,
    /// Media ids that answer `Ok(None)`.
    missing: HashSet<String>,
    /// (media id, requested source time), in call order.
    calls: Vec<(String, Tick)>,
}

impl FakeVideo {
    fn with(mut self, id: &str, rgb: [u8; 3]) -> Self {
        self.colours.insert(id.to_string(), rgb);
        self
    }

    fn missing(mut self, id: &str) -> Self {
        self.missing.insert(id.to_string());
        self
    }
}

impl VideoProvider for FakeVideo {
    fn frame_at(&mut self, media_id: &str, src_time: Tick) -> Result<Option<Frame>, RenderError> {
        self.calls.push((media_id.to_string(), src_time));
        if self.missing.contains(media_id) {
            return Ok(None);
        }
        let [r, g, b] = self
            .colours
            .get(media_id)
            .copied()
            .unwrap_or([255, 255, 255]);
        Ok(Some(Frame::solid(W, H, b, g, r, 255)))
    }
}

/// Constant or sine stereo audio (L = R). Records every read.
struct FakeAudio {
    amp: f64,
    sine_hz: Option<f64>,
    available: bool,
    /// (media id, stream, requested source time, frames requested), in call order.
    calls: Vec<(String, u32, Tick, usize)>,
}

impl FakeAudio {
    fn constant(amp: f64) -> FakeAudio {
        FakeAudio {
            amp,
            sine_hz: None,
            available: true,
            calls: Vec::new(),
        }
    }

    fn sine(amp: f64, hz: f64) -> FakeAudio {
        FakeAudio {
            amp,
            sine_hz: Some(hz),
            available: true,
            calls: Vec::new(),
        }
    }

    /// Answers `Ok(false)` for every read.
    fn unavailable() -> FakeAudio {
        FakeAudio {
            amp: 0.0,
            sine_hz: None,
            available: false,
            calls: Vec::new(),
        }
    }
}

impl AudioProvider for FakeAudio {
    fn read(
        &mut self,
        media_id: &str,
        stream: u32,
        src_time: Tick,
        frames: usize,
        sample_rate: u32,
        out: &mut Vec<f32>,
    ) -> Result<bool, RenderError> {
        self.calls
            .push((media_id.to_string(), stream, src_time, frames));
        if !self.available {
            return Ok(false);
        }
        out.clear();
        let t0 = src_time.as_secs_f64();
        for i in 0..frames {
            let v = match self.sine_hz {
                Some(hz) => {
                    let t = t0 + i as f64 / f64::from(sample_rate);
                    self.amp * (2.0 * PI * hz * t).sin()
                }
                None => self.amp,
            };
            let s = v as f32;
            out.push(s);
            out.push(s);
        }
        Ok(true)
    }
}

/// Title frames: a solid colour [r, g, b], or fully transparent when `rgb` is None.
#[derive(Default)]
struct FakeTitles {
    rgb: Option<[u8; 3]>,
    calls: usize,
}

impl TitleProvider for FakeTitles {
    fn render(&mut self, _text: &TextSpec, w: u32, h: u32) -> Result<Frame, RenderError> {
        self.calls += 1;
        Ok(match self.rgb {
            Some([r, g, b]) => Frame::solid(w, h, b, g, r, 255),
            None => Frame::solid(w, h, 0, 0, 0, 0),
        })
    }
}

/// Collects frames (first and centre pixel, index) and audio per track. Optionally cancels
/// a job once `n` frames have arrived.
#[derive(Default)]
struct CollectSink {
    indices: Vec<u64>,
    first: Vec<[u8; 4]>,
    centre: Vec<[u8; 4]>,
    audio: Vec<Vec<f32>>,
    cancel_after: Option<(usize, JobControl)>,
}

impl CollectSink {
    /// Interleaved stereo samples pushed for `track` (empty when the track never arrived).
    fn track(&self, track: usize) -> &[f32] {
        self.audio.get(track).map_or(&[][..], |v| v.as_slice())
    }

    /// Stereo sample frames pushed for `track`.
    fn track_frames(&self, track: usize) -> usize {
        self.track(track).len() / 2
    }
}

impl Sink for CollectSink {
    fn video(&mut self, frame: &Frame, index: u64) -> Result<(), RenderError> {
        self.indices.push(index);
        self.first.push(frame.pixel(0, 0).unwrap_or([0; 4]));
        self.centre.push(
            frame
                .pixel(frame.width / 2, frame.height / 2)
                .unwrap_or([0; 4]),
        );
        if let Some((n, ctl)) = &self.cancel_after {
            if self.indices.len() == *n {
                ctl.cancel();
            }
        }
        Ok(())
    }

    fn audio(&mut self, track: usize, samples: &[f32]) -> Result<(), RenderError> {
        while self.audio.len() <= track {
            self.audio.push(Vec::new());
        }
        if let Some(buf) = self.audio.get_mut(track) {
            buf.extend_from_slice(samples);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Builders and helpers
// ---------------------------------------------------------------------------------------------

struct Run {
    res: Result<PipelineReport, RenderError>,
    sink: CollectSink,
    video: FakeVideo,
    audio: FakeAudio,
    titles: FakeTitles,
}

fn go(
    plan: &RenderPlan,
    cfg: &PipelineConfig,
    mut video: FakeVideo,
    mut audio: FakeAudio,
    mut titles: FakeTitles,
) -> Run {
    let mut sink = CollectSink::default();
    let ctl = JobControl::new();
    let res = run(
        plan,
        cfg,
        &mut video,
        &mut audio,
        &mut titles,
        &mut sink,
        &ctl,
        &mut |_| {},
    );
    Run {
        res,
        sink,
        video,
        audio,
        titles,
    }
}

fn ok(r: &Run) -> &PipelineReport {
    r.res.as_ref().expect("run should succeed")
}

fn cfg(sample_rate: u32) -> PipelineConfig {
    PipelineConfig {
        out_w: W,
        out_h: H,
        fps: Rate::whole(30),
        sample_rate,
        layout: AudioLayout::Mix,
        audio_tracks: 1,
        limiter: false,
        loudness_gain_db: 0.0,
        video: true,
        audio: true,
    }
}

fn sec(s: i64) -> Tick {
    Tick::from_secs(s)
}

fn project(bg: Rgba) -> Project {
    let mut p = Project::new("pipeline-test");
    p.canvas = Canvas {
        width: W,
        height: H,
        fps: Rate::whole(30),
        background: bg,
        ..Canvas::default()
    };
    p
}

fn black_bg() -> Rgba {
    Rgba {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    }
}

fn add_media(p: &mut Project, id: &str, kind: MediaKind) {
    p.media.push(MediaRef {
        id: id.to_string(),
        kind,
        name: id.to_string(),
        path: format!("synthetic/{id}"),
        duration: sec(10),
        width: W,
        height: H,
        fps: 30.0,
        ..MediaRef::default()
    });
}

fn nth_track(p: &Project, kind: TrackKind, n: usize) -> String {
    p.tracks
        .iter()
        .filter(|t| t.kind == kind)
        .nth(n)
        .map(|t| t.id.clone())
        .unwrap_or_default()
}

fn video_clip(p: &Project, track: usize, id: &str, media: &str, start: Tick, dur: Tick) -> Clip {
    Clip {
        id: id.to_string(),
        track_id: nth_track(p, TrackKind::Video, track),
        kind: ClipKind::Video,
        media_id: Some(media.to_string()),
        start,
        dur,
        ..Clip::default()
    }
}

fn audio_clip(p: &Project, track: usize, id: &str, media: &str, start: Tick, dur: Tick) -> Clip {
    Clip {
        id: id.to_string(),
        track_id: nth_track(p, TrackKind::Audio, track),
        kind: ClipKind::Audio,
        media_id: Some(media.to_string()),
        start,
        dur,
        ..Clip::default()
    }
}

fn with_audio(mut c: Clip, a: ClipAudio) -> Clip {
    c.audio = a;
    c
}

fn plan_of(p: &Project) -> RenderPlan {
    derive_plan(p, None)
}

fn bgra(r: u8, g: u8, b: u8) -> [u8; 4] {
    [b, g, r, 255]
}

fn near(a: [u8; 4], b: [u8; 4], tol: i32) -> bool {
    a.iter()
        .zip(b.iter())
        .all(|(x, y)| (i32::from(*x) - i32::from(*y)).abs() <= tol)
}

fn peak(v: &[f32]) -> f32 {
    v.iter().fold(0.0_f32, |m, s| m.max(s.abs()))
}

fn channel(v: &[f32], ch: usize) -> Vec<f32> {
    v.iter().skip(ch).step_by(2).copied().collect()
}

/// Peak of one channel of one output track.
fn track_peak(r: &Run, track: usize, ch: usize) -> f32 {
    peak(&channel(r.sink.track(track), ch))
}

// ---------------------------------------------------------------------------------------------
// Frame counts and ordering
// ---------------------------------------------------------------------------------------------

#[test]
fn one_second_at_30fps_gives_30_frames() {
    let mut p = project(black_bg());
    add_media(&mut p, "m1", MediaKind::Video);
    let c = video_clip(&p, 0, "c1", "m1", Tick::ZERO, sec(1));
    p.clips.push(c);
    let r = go(
        &plan_of(&p),
        &cfg(48000),
        FakeVideo::default(),
        FakeAudio::unavailable(),
        FakeTitles::default(),
    );
    assert_eq!(ok(&r).frames, 30);
    assert_eq!(r.sink.indices.len(), 30);
}

#[test]
fn frame_indices_arrive_in_order_from_zero() {
    let mut p = project(black_bg());
    add_media(&mut p, "m1", MediaKind::Video);
    let c = video_clip(&p, 0, "c1", "m1", Tick::ZERO, sec(1));
    p.clips.push(c);
    let r = go(
        &plan_of(&p),
        &cfg(48000),
        FakeVideo::default(),
        FakeAudio::unavailable(),
        FakeTitles::default(),
    );
    let expected: Vec<u64> = (0..30).collect();
    assert_eq!(r.sink.indices, expected);
}

#[test]
fn two_and_a_half_seconds_gives_75_frames() {
    let mut p = project(black_bg());
    add_media(&mut p, "m1", MediaKind::Video);
    let c = video_clip(&p, 0, "c1", "m1", Tick::ZERO, Tick::from_ms(2500));
    p.clips.push(c);
    let r = go(
        &plan_of(&p),
        &cfg(48000),
        FakeVideo::default(),
        FakeAudio::unavailable(),
        FakeTitles::default(),
    );
    assert_eq!(ok(&r).frames, 75);
}

#[test]
fn ntsc_rate_floors_frame_count() {
    let mut p = project(black_bg());
    add_media(&mut p, "m1", MediaKind::Video);
    let c = video_clip(&p, 0, "c1", "m1", Tick::ZERO, sec(1));
    p.clips.push(c);
    let mut c_cfg = cfg(48000);
    c_cfg.fps = Rate {
        num: 30000,
        den: 1001,
    };
    let r = go(
        &plan_of(&p),
        &c_cfg,
        FakeVideo::default(),
        FakeAudio::unavailable(),
        FakeTitles::default(),
    );
    // 1 s at 29.97 fps is 29.97 frames, floored to 29.
    assert_eq!(ok(&r).frames, 29);
}

#[test]
fn twenty_four_fps_gives_24_frames() {
    let mut p = project(black_bg());
    add_media(&mut p, "m1", MediaKind::Video);
    let c = video_clip(&p, 0, "c1", "m1", Tick::ZERO, sec(1));
    p.clips.push(c);
    let mut c_cfg = cfg(48000);
    c_cfg.fps = Rate::whole(24);
    let r = go(
        &plan_of(&p),
        &c_cfg,
        FakeVideo::default(),
        FakeAudio::unavailable(),
        FakeTitles::default(),
    );
    assert_eq!(ok(&r).frames, 24);
}

#[test]
fn fractional_duration_floors_frames() {
    let mut p = project(black_bg());
    add_media(&mut p, "m1", MediaKind::Video);
    let c = video_clip(&p, 0, "c1", "m1", Tick::ZERO, Tick::from_ms(1234));
    p.clips.push(c);
    let r = go(
        &plan_of(&p),
        &cfg(48000),
        FakeVideo::default(),
        FakeAudio::unavailable(),
        FakeTitles::default(),
    );
    // 1.234 s * 30 = 37.02
    assert_eq!(ok(&r).frames, 37);
}

#[test]
fn video_false_pushes_no_frames_but_still_audio() {
    let mut p = project(black_bg());
    add_media(&mut p, "a1", MediaKind::Audio);
    let c = audio_clip(&p, 0, "c1", "a1", Tick::ZERO, sec(1));
    p.clips.push(c);
    let mut c_cfg = cfg(48000);
    c_cfg.video = false;
    let r = go(
        &plan_of(&p),
        &c_cfg,
        FakeVideo::default(),
        FakeAudio::constant(0.5),
        FakeTitles::default(),
    );
    assert_eq!(ok(&r).frames, 0);
    assert!(r.sink.indices.is_empty());
    assert_eq!(r.sink.track_frames(0), 48000);
}

#[test]
fn audio_false_pushes_no_audio_but_still_frames() {
    let mut p = project(black_bg());
    add_media(&mut p, "m1", MediaKind::Video);
    add_media(&mut p, "a1", MediaKind::Audio);
    let v = video_clip(&p, 0, "v1", "m1", Tick::ZERO, sec(1));
    let a = audio_clip(&p, 0, "a1c", "a1", Tick::ZERO, sec(1));
    p.clips.push(v);
    p.clips.push(a);
    let mut c_cfg = cfg(48000);
    c_cfg.audio = false;
    let r = go(
        &plan_of(&p),
        &c_cfg,
        FakeVideo::default(),
        FakeAudio::constant(0.5),
        FakeTitles::default(),
    );
    assert_eq!(ok(&r).frames, 30);
    assert!(r.sink.audio.is_empty());
    assert!(r.audio.calls.is_empty());
}

// ---------------------------------------------------------------------------------------------
// Audio sample totals
// ---------------------------------------------------------------------------------------------

/// Plan with one constant audio clip of `dur` on A1, returns the run.
fn audio_only_run(dur: Tick, sr: u32, fps: Rate, limiter: bool) -> Run {
    let mut p = project(black_bg());
    add_media(&mut p, "a1", MediaKind::Audio);
    let c = audio_clip(&p, 0, "c1", "a1", Tick::ZERO, dur);
    p.clips.push(c);
    let mut c_cfg = cfg(sr);
    c_cfg.fps = fps;
    c_cfg.limiter = limiter;
    go(
        &plan_of(&p),
        &c_cfg,
        FakeVideo::default(),
        FakeAudio::constant(0.5),
        FakeTitles::default(),
    )
}

#[test]
fn audio_total_one_second_48k() {
    let r = audio_only_run(sec(1), 48000, Rate::whole(30), false);
    assert_eq!(r.sink.track_frames(0), 48000);
}

#[test]
fn audio_total_one_second_44100() {
    let r = audio_only_run(sec(1), 44100, Rate::whole(30), false);
    assert_eq!(r.sink.track_frames(0), 44100);
}

#[test]
fn audio_total_two_and_a_half_seconds_44100() {
    let r = audio_only_run(Tick::from_ms(2500), 44100, Rate::whole(30), false);
    assert_eq!(r.sink.track_frames(0), 110_250);
}

#[test]
fn audio_total_is_independent_of_ntsc_frame_rate() {
    let r = audio_only_run(
        sec(1),
        48000,
        Rate {
            num: 30000,
            den: 1001,
        },
        false,
    );
    assert_eq!(r.sink.track_frames(0), 48000);
}

#[test]
fn audio_total_is_independent_of_24_fps() {
    let r = audio_only_run(sec(1), 44100, Rate::whole(24), false);
    assert_eq!(r.sink.track_frames(0), 44100);
}

#[test]
fn audio_total_rounds_fractional_samples_44100() {
    // 1.234 s * 44100 = 54419.4, rounds to 54419.
    let r = audio_only_run(Tick::from_ms(1234), 44100, Rate::whole(30), false);
    assert_eq!(r.sink.track_frames(0), 54419);
}

#[test]
fn audio_total_exact_48k_for_fractional_duration() {
    // 1.234 s * 48000 = 59232 exactly.
    let r = audio_only_run(Tick::from_ms(1234), 48000, Rate::whole(30), false);
    assert_eq!(r.sink.track_frames(0), 59232);
}

#[test]
fn audio_total_exact_with_limiter_on() {
    // The limiter's 5 ms look-ahead is compensated: the output length is exactly
    // round(duration * sample_rate), the same as without the limiter.
    let r = audio_only_run(sec(1), 48000, Rate::whole(30), true);
    assert_eq!(r.sink.track_frames(0), 48000);
}

#[test]
fn audio_total_exact_when_video_drives_blocks() {
    // With video on, audio is produced per frame block. Totals must still be exact.
    let mut p = project(black_bg());
    add_media(&mut p, "m1", MediaKind::Video);
    add_media(&mut p, "a1", MediaKind::Audio);
    let v = video_clip(&p, 0, "v1", "m1", Tick::ZERO, Tick::from_ms(1234));
    let a = audio_clip(&p, 0, "a1c", "a1", Tick::ZERO, Tick::from_ms(1234));
    p.clips.push(v);
    p.clips.push(a);
    let r = go(
        &plan_of(&p),
        &cfg(44100),
        FakeVideo::default(),
        FakeAudio::constant(0.5),
        FakeTitles::default(),
    );
    assert_eq!(ok(&r).frames, 37);
    assert_eq!(r.sink.track_frames(0), 54419);
}

// ---------------------------------------------------------------------------------------------
// Video composition
// ---------------------------------------------------------------------------------------------

#[test]
fn video_clip_colour_appears_in_output() {
    let mut p = project(black_bg());
    add_media(&mut p, "m1", MediaKind::Video);
    let c = video_clip(&p, 0, "c1", "m1", Tick::ZERO, sec(1));
    p.clips.push(c);
    let v = FakeVideo::default().with("m1", [200, 50, 100]);
    let r = go(
        &plan_of(&p),
        &cfg(48000),
        v,
        FakeAudio::unavailable(),
        FakeTitles::default(),
    );
    assert!(r
        .sink
        .centre
        .iter()
        .all(|px| near(*px, bgra(200, 50, 100), 1)));
}

#[test]
fn gap_shows_canvas_background() {
    let mut p = project(Rgba {
        r: 10,
        g: 20,
        b: 30,
        a: 255,
    });
    add_media(&mut p, "m1", MediaKind::Video);
    let c = video_clip(&p, 0, "c1", "m1", Tick::from_ms(500), Tick::from_ms(500));
    p.clips.push(c);
    let v = FakeVideo::default().with("m1", [255, 0, 0]);
    let r = go(
        &plan_of(&p),
        &cfg(48000),
        v,
        FakeAudio::unavailable(),
        FakeTitles::default(),
    );
    // Frames 0..15 are before the clip at 0.5 s.
    let bg = bgra(10, 20, 30);
    assert!(near(r.sink.first[0], bg, 1));
    assert!(near(r.sink.centre[14], bg, 1));
    assert!(near(r.sink.centre[29], bgra(255, 0, 0), 1));
}

#[test]
fn stacked_top_layer_covers_bottom() {
    let mut p = project(black_bg());
    add_media(&mut p, "red", MediaKind::Video);
    add_media(&mut p, "blue", MediaKind::Video);
    let bottom = video_clip(&p, 0, "bottom", "red", Tick::ZERO, sec(1));
    let top = video_clip(&p, 1, "top", "blue", Tick::ZERO, sec(1));
    p.clips.push(bottom);
    p.clips.push(top);
    let v = FakeVideo::default()
        .with("red", [255, 0, 0])
        .with("blue", [0, 0, 255]);
    let r = go(
        &plan_of(&p),
        &cfg(48000),
        v,
        FakeAudio::unavailable(),
        FakeTitles::default(),
    );
    assert!(r.sink.centre.iter().all(|px| near(*px, bgra(0, 0, 255), 1)));
}

#[test]
fn opacity_half_blends_over_black() {
    let mut p = project(black_bg());
    add_media(&mut p, "w", MediaKind::Video);
    let mut c = video_clip(&p, 0, "c1", "w", Tick::ZERO, sec(1));
    c.opacity = 0.5;
    p.clips.push(c);
    let v = FakeVideo::default().with("w", [255, 255, 255]);
    let r = go(
        &plan_of(&p),
        &cfg(48000),
        v,
        FakeAudio::unavailable(),
        FakeTitles::default(),
    );
    assert!(r
        .sink
        .centre
        .iter()
        .all(|px| near(*px, [128, 128, 128, 255], 3)));
}

#[test]
fn speed_two_requests_advance_source_twice_as_fast() {
    let mut p = project(black_bg());
    add_media(&mut p, "m1", MediaKind::Video);
    let mut c = video_clip(&p, 0, "c1", "m1", Tick::ZERO, sec(1));
    c.speed = 2.0;
    p.clips.push(c);
    let r = go(
        &plan_of(&p),
        &cfg(48000),
        FakeVideo::default(),
        FakeAudio::unavailable(),
        FakeTitles::default(),
    );
    assert_eq!(r.video.calls.len(), 30);
    let tps = TICKS_PER_SECOND;
    for (n, (id, st)) in r.video.calls.iter().enumerate() {
        assert_eq!(id, "m1");
        let expected = n as i64 * 2 * tps / 30;
        assert!(
            (st.0 - expected).abs() <= 1,
            "frame {n}: got {} want {expected}",
            st.0
        );
    }
}

#[test]
fn reverse_clip_requests_decreasing_source_times() {
    let mut p = project(black_bg());
    add_media(&mut p, "m1", MediaKind::Video);
    let mut c = video_clip(&p, 0, "c1", "m1", Tick::ZERO, sec(1));
    c.reverse = true;
    p.clips.push(c);
    let r = go(
        &plan_of(&p),
        &cfg(48000),
        FakeVideo::default(),
        FakeAudio::unavailable(),
        FakeTitles::default(),
    );
    let times: Vec<i64> = r.video.calls.iter().map(|(_, t)| t.0).collect();
    assert_eq!(times.len(), 30);
    // Source length is 1 s, so the first request is the source end (1 s).
    assert!((times[0] - TICKS_PER_SECOND).abs() <= 1);
    assert!(times.windows(2).all(|w| w[1] < w[0]));
}

#[test]
fn freeze_frame_requests_same_source_time() {
    let mut p = project(black_bg());
    add_media(&mut p, "m1", MediaKind::Video);
    let mut c = video_clip(&p, 0, "c1", "m1", Tick::ZERO, sec(1));
    c.speed = 0.0;
    c.src_in = Tick::from_ms(500);
    p.clips.push(c);
    let r = go(
        &plan_of(&p),
        &cfg(48000),
        FakeVideo::default(),
        FakeAudio::unavailable(),
        FakeTitles::default(),
    );
    assert_eq!(r.video.calls.len(), 30);
    assert!(r.video.calls.iter().all(|(_, t)| *t == Tick::from_ms(500)));
}

#[test]
fn missing_media_gives_black_frames() {
    let mut p = project(Rgba {
        r: 10,
        g: 20,
        b: 30,
        a: 255,
    });
    add_media(&mut p, "m1", MediaKind::Video);
    let c = video_clip(&p, 0, "c1", "m1", Tick::ZERO, sec(1));
    p.clips.push(c);
    let v = FakeVideo::default().missing("m1");
    let r = go(
        &plan_of(&p),
        &cfg(48000),
        v,
        FakeAudio::unavailable(),
        FakeTitles::default(),
    );
    assert_eq!(ok(&r).frames, 30);
    assert!(r.sink.centre.iter().all(|px| near(*px, [0, 0, 0, 255], 0)));
}

#[test]
fn missing_media_warns_exactly_once() {
    let mut p = project(black_bg());
    add_media(&mut p, "m1", MediaKind::Video);
    let c = video_clip(&p, 0, "c1", "m1", Tick::ZERO, sec(1));
    p.clips.push(c);
    let v = FakeVideo::default().missing("m1");
    let r = go(
        &plan_of(&p),
        &cfg(48000),
        v,
        FakeAudio::unavailable(),
        FakeTitles::default(),
    );
    let n = ok(&r).warnings.iter().filter(|w| w.contains("m1")).count();
    assert_eq!(n, 1, "warnings: {:?}", ok(&r).warnings);
}

#[test]
fn missing_media_on_two_layers_still_one_warning() {
    let mut p = project(black_bg());
    add_media(&mut p, "m1", MediaKind::Video);
    let bottom = video_clip(&p, 0, "b", "m1", Tick::ZERO, sec(1));
    let top = video_clip(&p, 1, "t", "m1", Tick::ZERO, sec(1));
    p.clips.push(bottom);
    p.clips.push(top);
    let v = FakeVideo::default().missing("m1");
    let r = go(
        &plan_of(&p),
        &cfg(48000),
        v,
        FakeAudio::unavailable(),
        FakeTitles::default(),
    );
    let n = ok(&r).warnings.iter().filter(|w| w.contains("m1")).count();
    assert_eq!(n, 1);
}

#[test]
fn title_layer_calls_title_provider_and_shows_its_pixels() {
    let mut p = project(black_bg());
    let mut c = Clip {
        id: "t1".to_string(),
        track_id: nth_track(&p, TrackKind::Video, 0),
        kind: ClipKind::Title,
        start: Tick::ZERO,
        dur: sec(1),
        ..Clip::default()
    };
    c.text = Some(TextSpec {
        text: "Hi".to_string(),
        ..TextSpec::default()
    });
    p.clips.push(c);
    let titles = FakeTitles {
        rgb: Some([0, 255, 0]),
        calls: 0,
    };
    let r = go(
        &plan_of(&p),
        &cfg(48000),
        FakeVideo::default(),
        FakeAudio::unavailable(),
        titles,
    );
    assert!(r.titles.calls >= 1);
    assert!(r.sink.centre.iter().all(|px| near(*px, bgra(0, 255, 0), 1)));
}

#[test]
fn transparent_title_shows_background() {
    let mut p = project(Rgba {
        r: 10,
        g: 20,
        b: 30,
        a: 255,
    });
    let mut c = Clip {
        id: "t1".to_string(),
        track_id: nth_track(&p, TrackKind::Video, 0),
        kind: ClipKind::Title,
        start: Tick::ZERO,
        dur: sec(1),
        ..Clip::default()
    };
    c.text = Some(TextSpec {
        text: "Hi".to_string(),
        ..TextSpec::default()
    });
    p.clips.push(c);
    let titles = FakeTitles {
        rgb: None,
        calls: 0,
    };
    let r = go(
        &plan_of(&p),
        &cfg(48000),
        FakeVideo::default(),
        FakeAudio::unavailable(),
        titles,
    );
    assert!(r
        .sink
        .centre
        .iter()
        .all(|px| near(*px, bgra(10, 20, 30), 1)));
}

#[test]
fn solid_layer_fills_the_canvas() {
    let mut p = project(black_bg());
    let c = Clip {
        id: "s1".to_string(),
        track_id: nth_track(&p, TrackKind::Video, 0),
        kind: ClipKind::Solid,
        start: Tick::ZERO,
        dur: sec(1),
        solid: Some(Rgba {
            r: 1,
            g: 2,
            b: 3,
            a: 255,
        }),
        ..Clip::default()
    };
    p.clips.push(c);
    let r = go(
        &plan_of(&p),
        &cfg(48000),
        FakeVideo::default(),
        FakeAudio::unavailable(),
        FakeTitles::default(),
    );
    assert!(r.sink.first.iter().all(|px| near(*px, [3, 2, 1, 255], 1)));
    assert!(r.sink.centre.iter().all(|px| near(*px, [3, 2, 1, 255], 1)));
}

#[test]
fn output_frames_have_output_size_when_canvas_matches() {
    // The sink receives frames at the output size; the centre pixel must exist.
    let mut p = project(black_bg());
    add_media(&mut p, "m1", MediaKind::Video);
    let c = video_clip(&p, 0, "c1", "m1", Tick::ZERO, sec(1));
    p.clips.push(c);
    let r = go(
        &plan_of(&p),
        &cfg(48000),
        FakeVideo::default(),
        FakeAudio::unavailable(),
        FakeTitles::default(),
    );
    assert_eq!(r.sink.centre.len(), 30);
    assert!(r.sink.centre.iter().all(|px| px[3] == 255));
}

// ---------------------------------------------------------------------------------------------
// Audio mixing
// ---------------------------------------------------------------------------------------------

#[test]
fn clip_gain_minus_six_db_halves_amplitude() {
    let mut p = project(black_bg());
    add_media(&mut p, "a1", MediaKind::Audio);
    let a = ClipAudio {
        gain_db: -6.02,
        ..ClipAudio::default()
    };
    let c = with_audio(audio_clip(&p, 0, "c1", "a1", Tick::ZERO, sec(1)), a);
    p.clips.push(c);
    let r = go(
        &plan_of(&p),
        &cfg(48000),
        FakeVideo::default(),
        FakeAudio::constant(1.0),
        FakeTitles::default(),
    );
    let pk = track_peak(&r, 0, 0);
    assert!((f64::from(pk) - 0.5).abs() < 0.01, "peak {pk}");
}

#[test]
fn muted_clip_is_silent() {
    let mut p = project(black_bg());
    add_media(&mut p, "a1", MediaKind::Audio);
    let a = ClipAudio {
        muted: true,
        ..ClipAudio::default()
    };
    let c = with_audio(audio_clip(&p, 0, "c1", "a1", Tick::ZERO, sec(1)), a);
    p.clips.push(c);
    let r = go(
        &plan_of(&p),
        &cfg(48000),
        FakeVideo::default(),
        FakeAudio::constant(1.0),
        FakeTitles::default(),
    );
    assert_eq!(track_peak(&r, 0, 0), 0.0);
    assert_eq!(track_peak(&r, 0, 1), 0.0);
}

#[test]
fn pan_hard_left_silences_right_channel() {
    let mut p = project(black_bg());
    add_media(&mut p, "a1", MediaKind::Audio);
    let a = ClipAudio {
        pan: -1.0,
        ..ClipAudio::default()
    };
    let c = with_audio(audio_clip(&p, 0, "c1", "a1", Tick::ZERO, sec(1)), a);
    p.clips.push(c);
    let r = go(
        &plan_of(&p),
        &cfg(48000),
        FakeVideo::default(),
        FakeAudio::constant(0.5),
        FakeTitles::default(),
    );
    assert!(track_peak(&r, 0, 0) > 0.1);
    assert_eq!(track_peak(&r, 0, 1), 0.0);
}

#[test]
fn track_fader_applies_volume() {
    let mut p = project(black_bg());
    add_media(&mut p, "a1", MediaKind::Audio);
    if let Some(t) = p.tracks.iter_mut().find(|t| t.kind == TrackKind::Audio) {
        t.volume_db = -6.02;
    }
    let c = audio_clip(&p, 0, "c1", "a1", Tick::ZERO, sec(1));
    p.clips.push(c);
    let r = go(
        &plan_of(&p),
        &cfg(48000),
        FakeVideo::default(),
        FakeAudio::constant(1.0),
        FakeTitles::default(),
    );
    let pk = track_peak(&r, 0, 0);
    assert!((f64::from(pk) - 0.5).abs() < 0.01, "peak {pk}");
}

#[test]
fn fade_in_ramps_up_from_zero() {
    let mut p = project(black_bg());
    add_media(&mut p, "a1", MediaKind::Audio);
    let a = ClipAudio {
        fade_in: Tick::from_ms(500),
        ..ClipAudio::default()
    };
    let c = with_audio(audio_clip(&p, 0, "c1", "a1", Tick::ZERO, sec(1)), a);
    p.clips.push(c);
    let r = go(
        &plan_of(&p),
        &cfg(48000),
        FakeVideo::default(),
        FakeAudio::constant(1.0),
        FakeTitles::default(),
    );
    let left = channel(r.sink.track(0), 0);
    assert!(left.first().is_some_and(|s| s.abs() < 0.01));
    // Linear fade: 0.25 s is half of the 0.5 s fade-in, so gain 0.5.
    let at_quarter = left.get(12_000).copied().unwrap_or(-1.0);
    assert!(
        (at_quarter - 0.5).abs() < 0.02,
        "value at 0.25 s: {at_quarter}"
    );
}

#[test]
fn clips_on_different_tracks_sum_without_limiter() {
    let mut p = project(black_bg());
    add_media(&mut p, "a1", MediaKind::Audio);
    add_media(&mut p, "a2", MediaKind::Audio);
    let c1 = audio_clip(&p, 0, "c1", "a1", Tick::ZERO, sec(1));
    let c2 = audio_clip(&p, 1, "c2", "a2", Tick::ZERO, sec(1));
    p.clips.push(c1);
    p.clips.push(c2);
    let r = go(
        &plan_of(&p),
        &cfg(48000),
        FakeVideo::default(),
        FakeAudio::constant(1.0),
        FakeTitles::default(),
    );
    let pk = track_peak(&r, 0, 0);
    assert!((pk - 2.0).abs() < 0.01, "peak {pk}");
}

#[test]
fn limiter_keeps_summed_peak_under_ceiling() {
    let mut p = project(black_bg());
    add_media(&mut p, "a1", MediaKind::Audio);
    add_media(&mut p, "a2", MediaKind::Audio);
    let c1 = audio_clip(&p, 0, "c1", "a1", Tick::ZERO, sec(1));
    let c2 = audio_clip(&p, 1, "c2", "a2", Tick::ZERO, sec(1));
    p.clips.push(c1);
    p.clips.push(c2);
    let mut c_cfg = cfg(48000);
    c_cfg.limiter = true;
    let r = go(
        &plan_of(&p),
        &c_cfg,
        FakeVideo::default(),
        FakeAudio::constant(1.0),
        FakeTitles::default(),
    );
    let pk = track_peak(&r, 0, 0).max(track_peak(&r, 0, 1));
    let db = 20.0 * f64::from(pk).log10();
    assert!(db <= -0.9, "peak {db} dBFS");
}

#[test]
fn separate_layout_routes_each_audio_track_to_its_own_output() {
    let mut p = project(black_bg());
    add_media(&mut p, "a1", MediaKind::Audio);
    add_media(&mut p, "a2", MediaKind::Audio);
    let loud = audio_clip(&p, 0, "c1", "a1", Tick::ZERO, sec(1));
    let quiet_audio = ClipAudio {
        gain_db: -20.0,
        ..ClipAudio::default()
    };
    let quiet = with_audio(
        audio_clip(&p, 1, "c2", "a2", Tick::ZERO, sec(1)),
        quiet_audio,
    );
    p.clips.push(loud);
    p.clips.push(quiet);
    let mut c_cfg = cfg(48000);
    c_cfg.layout = AudioLayout::Separate;
    c_cfg.audio_tracks = 2;
    let r = go(
        &plan_of(&p),
        &c_cfg,
        FakeVideo::default(),
        FakeAudio::constant(1.0),
        FakeTitles::default(),
    );
    assert_eq!(r.sink.audio.len(), 2);
    assert!((track_peak(&r, 0, 0) - 1.0).abs() < 0.01);
    assert!((track_peak(&r, 1, 0) - 0.1).abs() < 0.01);
    assert_eq!(r.sink.track_frames(0), 48000);
    assert_eq!(r.sink.track_frames(1), 48000);
}

#[test]
fn mix_layout_pushes_only_track_zero() {
    let mut p = project(black_bg());
    add_media(&mut p, "a1", MediaKind::Audio);
    add_media(&mut p, "a2", MediaKind::Audio);
    let c1 = audio_clip(&p, 0, "c1", "a1", Tick::ZERO, sec(1));
    let c2 = audio_clip(&p, 1, "c2", "a2", Tick::ZERO, sec(1));
    p.clips.push(c1);
    p.clips.push(c2);
    let mut c_cfg = cfg(48000);
    c_cfg.layout = AudioLayout::Mix;
    c_cfg.audio_tracks = 2;
    let r = go(
        &plan_of(&p),
        &c_cfg,
        FakeVideo::default(),
        FakeAudio::constant(1.0),
        FakeTitles::default(),
    );
    assert_eq!(r.sink.audio.len(), 1);
    // Both sources are summed into the single mix.
    assert!((track_peak(&r, 0, 0) - 2.0).abs() < 0.01);
}

#[test]
fn loudness_gain_six_db_doubles_amplitude() {
    let mut p = project(black_bg());
    add_media(&mut p, "a1", MediaKind::Audio);
    let c = audio_clip(&p, 0, "c1", "a1", Tick::ZERO, sec(1));
    p.clips.push(c);
    let plan = plan_of(&p);
    let base = go(
        &plan,
        &cfg(48000),
        FakeVideo::default(),
        FakeAudio::constant(0.25),
        FakeTitles::default(),
    );
    let mut boosted_cfg = cfg(48000);
    boosted_cfg.loudness_gain_db = 6.02;
    let boosted = go(
        &plan,
        &boosted_cfg,
        FakeVideo::default(),
        FakeAudio::constant(0.25),
        FakeTitles::default(),
    );
    let ratio = f64::from(track_peak(&boosted, 0, 0)) / f64::from(track_peak(&base, 0, 0));
    assert!((ratio - 2.0).abs() < 0.04, "ratio {ratio}");
}

#[test]
fn audio_reads_use_the_clip_source_track_as_stream() {
    let mut p = project(black_bg());
    add_media(&mut p, "a1", MediaKind::Audio);
    let a = ClipAudio {
        source_track: 1,
        ..ClipAudio::default()
    };
    let c = with_audio(audio_clip(&p, 0, "c1", "a1", Tick::ZERO, sec(1)), a);
    p.clips.push(c);
    let r = go(
        &plan_of(&p),
        &cfg(48000),
        FakeVideo::default(),
        FakeAudio::constant(0.5),
        FakeTitles::default(),
    );
    assert!(!r.audio.calls.is_empty());
    assert!(r
        .audio
        .calls
        .iter()
        .all(|(id, stream, _, _)| id == "a1" && *stream == 1));
}

#[test]
fn unavailable_audio_gives_silence_and_one_warning() {
    let mut p = project(black_bg());
    add_media(&mut p, "a1", MediaKind::Audio);
    let c = audio_clip(&p, 0, "c1", "a1", Tick::ZERO, sec(1));
    p.clips.push(c);
    let r = go(
        &plan_of(&p),
        &cfg(48000),
        FakeVideo::default(),
        FakeAudio::unavailable(),
        FakeTitles::default(),
    );
    assert_eq!(track_peak(&r, 0, 0), 0.0);
    let n = ok(&r).warnings.iter().filter(|w| w.contains("a1")).count();
    assert_eq!(n, 1, "warnings: {:?}", ok(&r).warnings);
}

// ---------------------------------------------------------------------------------------------
// Errors, cancel, pause, progress
// ---------------------------------------------------------------------------------------------

fn simple_video_project() -> Project {
    let mut p = project(black_bg());
    add_media(&mut p, "m1", MediaKind::Video);
    let c = video_clip(&p, 0, "c1", "m1", Tick::ZERO, sec(1));
    p.clips.push(c);
    p
}

#[test]
fn cancel_before_run_gives_cancelled_and_no_frames() {
    let p = simple_video_project();
    let plan = plan_of(&p);
    let mut video = FakeVideo::default();
    let mut audio = FakeAudio::unavailable();
    let mut titles = FakeTitles::default();
    let mut sink = CollectSink::default();
    let ctl = JobControl::new();
    ctl.cancel();
    let res = run(
        &plan,
        &cfg(48000),
        &mut video,
        &mut audio,
        &mut titles,
        &mut sink,
        &ctl,
        &mut |_| {},
    );
    assert_eq!(res, Err(RenderError::Cancelled));
    assert!(sink.indices.is_empty());
}

#[test]
fn cancel_from_sink_after_five_frames_stops_promptly() {
    let p = simple_video_project();
    let plan = plan_of(&p);
    let mut video = FakeVideo::default();
    let mut audio = FakeAudio::unavailable();
    let mut titles = FakeTitles::default();
    let ctl = JobControl::new();
    let mut sink = CollectSink {
        cancel_after: Some((5, ctl.clone())),
        ..CollectSink::default()
    };
    let res = run(
        &plan,
        &cfg(48000),
        &mut video,
        &mut audio,
        &mut titles,
        &mut sink,
        &ctl,
        &mut |_| {},
    );
    assert_eq!(res, Err(RenderError::Cancelled));
    assert_eq!(sink.indices.len(), 5);
    assert!(ctl.is_cancelled());
}

#[test]
fn pause_blocks_until_resumed_and_reports_paused() {
    // The only timing-dependent test: a second thread resumes after about 100 ms.
    let p = simple_video_project();
    let plan = plan_of(&p);
    let mut c_cfg = cfg(48000);
    c_cfg.audio = false;
    let ctl = JobControl::new();
    ctl.set_paused(true);
    let resume = ctl.clone();
    let waker = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        resume.set_paused(false);
    });
    let mut video = FakeVideo::default();
    let mut audio = FakeAudio::unavailable();
    let mut titles = FakeTitles::default();
    let mut sink = CollectSink::default();
    let mut seen: Vec<Progress> = Vec::new();
    let res = run(
        &plan,
        &c_cfg,
        &mut video,
        &mut audio,
        &mut titles,
        &mut sink,
        &ctl,
        &mut |pr: Progress| {
            seen.push(pr);
        },
    );
    let _ = waker.join();
    assert!(res.is_ok(), "run failed: {res:?}");
    assert!(seen.iter().any(|pr| pr.paused));
    assert_eq!(sink.indices.len(), 30);
}

#[test]
fn progress_is_reported_and_ends_at_one_hundred() {
    let p = simple_video_project();
    let plan = plan_of(&p);
    let c_cfg = cfg(48000);
    let mut video = FakeVideo::default();
    let mut audio = FakeAudio::unavailable();
    let mut titles = FakeTitles::default();
    let mut sink = CollectSink::default();
    let ctl = JobControl::new();
    let mut seen: Vec<Progress> = Vec::new();
    let res = run(
        &plan,
        &c_cfg,
        &mut video,
        &mut audio,
        &mut titles,
        &mut sink,
        &ctl,
        &mut |pr: Progress| {
            seen.push(pr);
        },
    );
    assert!(res.is_ok());
    let last = seen.last().copied();
    assert!(last.is_some(), "no progress reported");
    let last = last.unwrap_or(Progress {
        out_secs: 0.0,
        total_secs: 0.0,
        percent: 0.0,
        frames: 0,
        paused: false,
    });
    assert!(
        (last.percent - 100.0).abs() < 1e-3,
        "final percent {}",
        last.percent
    );
    assert!((last.total_secs - 1.0).abs() < 1e-6);
    assert!(seen.iter().all(|pr| !pr.paused));
}

#[test]
fn progress_percent_is_monotonic() {
    let p = simple_video_project();
    let plan = plan_of(&p);
    let mut video = FakeVideo::default();
    let mut audio = FakeAudio::unavailable();
    let mut titles = FakeTitles::default();
    let mut sink = CollectSink::default();
    let ctl = JobControl::new();
    let mut seen: Vec<Progress> = Vec::new();
    let res = run(
        &plan,
        &cfg(48000),
        &mut video,
        &mut audio,
        &mut titles,
        &mut sink,
        &ctl,
        &mut |pr: Progress| {
            seen.push(pr);
        },
    );
    assert!(res.is_ok());
    assert!(seen.windows(2).all(|w| w[0].percent <= w[1].percent));
}

#[test]
fn invalid_odd_output_width_is_rejected() {
    let p = simple_video_project();
    let mut c_cfg = cfg(48000);
    c_cfg.out_w = 321;
    let r = go(
        &plan_of(&p),
        &c_cfg,
        FakeVideo::default(),
        FakeAudio::unavailable(),
        FakeTitles::default(),
    );
    assert!(matches!(r.res, Err(RenderError::Invalid(_))));
    assert!(r.sink.indices.is_empty());
}

#[test]
fn invalid_zero_output_height_is_rejected() {
    let p = simple_video_project();
    let mut c_cfg = cfg(48000);
    c_cfg.out_h = 0;
    let r = go(
        &plan_of(&p),
        &c_cfg,
        FakeVideo::default(),
        FakeAudio::unavailable(),
        FakeTitles::default(),
    );
    assert!(matches!(r.res, Err(RenderError::Invalid(_))));
}

#[test]
fn invalid_sample_rate_8000_is_rejected() {
    let p = simple_video_project();
    let r = go(
        &plan_of(&p),
        &cfg(8000),
        FakeVideo::default(),
        FakeAudio::unavailable(),
        FakeTitles::default(),
    );
    assert!(matches!(r.res, Err(RenderError::Invalid(_))));
}

#[test]
fn empty_plan_is_rejected() {
    let plan = RenderPlan {
        canvas: Canvas::default(),
        start: Tick::ZERO,
        end: Tick::ZERO,
        segments: Vec::new(),
    };
    let r = go(
        &plan,
        &cfg(48000),
        FakeVideo::default(),
        FakeAudio::unavailable(),
        FakeTitles::default(),
    );
    assert!(matches!(r.res, Err(RenderError::Invalid(_))));
}

#[test]
fn zero_frame_rate_is_rejected() {
    let p = simple_video_project();
    let mut c_cfg = cfg(48000);
    c_cfg.fps = Rate { num: 0, den: 1 };
    let r = go(
        &plan_of(&p),
        &c_cfg,
        FakeVideo::default(),
        FakeAudio::unavailable(),
        FakeTitles::default(),
    );
    assert!(matches!(r.res, Err(RenderError::Invalid(_))));
}

// ---------------------------------------------------------------------------------------------
// Loudness measurement
// ---------------------------------------------------------------------------------------------

/// Two-second audio-only plan on A1 with the given audio clip gain.
fn two_second_audio_plan() -> RenderPlan {
    let mut p = project(black_bg());
    add_media(&mut p, "a1", MediaKind::Audio);
    let c = audio_clip(&p, 0, "c1", "a1", Tick::ZERO, sec(2));
    p.clips.push(c);
    plan_of(&p)
}

#[test]
fn measure_loudness_of_minus_20_dbfs_sine_is_close_to_expected() {
    let plan = two_second_audio_plan();
    let ctl = JobControl::new();
    let mut audio = FakeAudio::sine(0.1, 1000.0);
    let lufs = measure_loudness(&plan, &cfg(48000), &mut audio, &ctl);
    let value = lufs.expect("no error").expect("not silent");
    // Stereo sine of amplitude A reads -0.691 + 20 log10(A) LUFS; A = 0.1 gives -20.69.
    assert!((value - (-20.69)).abs() < 0.5, "measured {value} LUFS");
}

#[test]
fn measure_loudness_matches_meter_fed_with_run_output() {
    let plan = two_second_audio_plan();
    let mut c_cfg = cfg(48000);
    c_cfg.video = false;
    let r = go(
        &plan,
        &c_cfg,
        FakeVideo::default(),
        FakeAudio::sine(0.1, 1000.0),
        FakeTitles::default(),
    );
    let mut meter = LoudnessMeter::new(48000, 2);
    meter.push(r.sink.track(0));
    let direct = meter.integrated_lufs().expect("audible");

    let ctl = JobControl::new();
    let mut audio = FakeAudio::sine(0.1, 1000.0);
    let measured = measure_loudness(&plan, &cfg(48000), &mut audio, &ctl)
        .expect("no error")
        .expect("audible");
    assert!(
        (measured - direct).abs() < 0.01,
        "measure {measured} vs meter {direct}"
    );
}

#[test]
fn measure_loudness_of_silence_is_none() {
    let plan = two_second_audio_plan();
    let ctl = JobControl::new();
    let mut audio = FakeAudio::constant(0.0);
    let lufs = measure_loudness(&plan, &cfg(48000), &mut audio, &ctl);
    assert_eq!(lufs, Ok(None));
}

#[test]
fn measure_loudness_honours_cancel() {
    let plan = two_second_audio_plan();
    let ctl = JobControl::new();
    ctl.cancel();
    let mut audio = FakeAudio::sine(0.1, 1000.0);
    let lufs = measure_loudness(&plan, &cfg(48000), &mut audio, &ctl);
    assert_eq!(lufs, Err(RenderError::Cancelled));
}

#[test]
fn measure_loudness_rejects_bad_sample_rate() {
    let plan = two_second_audio_plan();
    let ctl = JobControl::new();
    let mut audio = FakeAudio::sine(0.1, 1000.0);
    let lufs = measure_loudness(&plan, &cfg(22050), &mut audio, &ctl);
    assert!(matches!(lufs, Err(RenderError::Invalid(_))));
}

#[test]
fn measure_loudness_rejects_empty_range() {
    let plan = RenderPlan {
        canvas: Canvas::default(),
        start: sec(1),
        end: sec(1),
        segments: Vec::new(),
    };
    let ctl = JobControl::new();
    let mut audio = FakeAudio::sine(0.1, 1000.0);
    let lufs = measure_loudness(&plan, &cfg(48000), &mut audio, &ctl);
    assert!(matches!(lufs, Err(RenderError::Invalid(_))));
}

// ---------------------------------------------------------------------------------------------
// Limiter: delay compensation, exact totals, ceiling (fixed behaviour)
// ---------------------------------------------------------------------------------------------

/// Audio-only run (no video, so blocks of AUDIO_BLOCK samples) with `n` constant clips of `dur`,
/// one per audio track (all on track 0 for Mix, where they are summed). `amp` is each clip's level.
fn audio_stack_run(
    n: usize,
    dur: Tick,
    sr: u32,
    amp: f64,
    layout: AudioLayout,
    limiter: bool,
) -> Run {
    let mut p = project(black_bg());
    for i in 0..n {
        add_media(&mut p, &format!("a{i}"), MediaKind::Audio);
    }
    for i in 0..n {
        let c = audio_clip(&p, i, &format!("c{i}"), &format!("a{i}"), Tick::ZERO, dur);
        p.clips.push(c);
    }
    let mix = matches!(layout, AudioLayout::Mix);
    let mut c_cfg = cfg(sr);
    c_cfg.layout = layout;
    c_cfg.audio_tracks = if mix { 1 } else { n };
    c_cfg.limiter = limiter;
    c_cfg.video = false;
    go(
        &plan_of(&p),
        &c_cfg,
        FakeVideo::default(),
        FakeAudio::constant(amp),
        FakeTitles::default(),
    )
}

#[test]
fn limiter_total_exact_for_three_ms_clip_48k() {
    // 3 ms is shorter than the 5 ms look-ahead: 144 samples exactly.
    let r = audio_stack_run(1, Tick::from_ms(3), 48000, 0.5, AudioLayout::Mix, true);
    assert_eq!(r.sink.track_frames(0), 144);
}

#[test]
fn limiter_total_exact_for_one_ms_clip_44100() {
    // 1 ms at 44.1 kHz is 44.1 samples, rounded to 44.
    let r = audio_stack_run(1, Tick::from_ms(1), 44100, 0.5, AudioLayout::Mix, true);
    assert_eq!(r.sink.track_frames(0), 44);
}

#[test]
fn limiter_total_exact_for_fractional_duration_44100_audio_only() {
    let r = audio_stack_run(1, Tick::from_ms(1234), 44100, 0.5, AudioLayout::Mix, true);
    assert_eq!(r.sink.track_frames(0), 54419);
}

#[test]
fn limiter_total_exact_per_track_in_separate_layout() {
    let r = audio_stack_run(
        2,
        Tick::from_ms(1234),
        44100,
        0.5,
        AudioLayout::Separate,
        true,
    );
    assert_eq!(r.sink.track_frames(0), 54419);
    assert_eq!(r.sink.track_frames(1), 54419);
}

#[test]
fn limiter_total_exact_when_video_drives_blocks() {
    let mut p = project(black_bg());
    add_media(&mut p, "m1", MediaKind::Video);
    add_media(&mut p, "a1", MediaKind::Audio);
    let v = video_clip(&p, 0, "v1", "m1", Tick::ZERO, Tick::from_ms(1234));
    let a = audio_clip(&p, 0, "a1c", "a1", Tick::ZERO, Tick::from_ms(1234));
    p.clips.push(v);
    p.clips.push(a);
    let mut c_cfg = cfg(44100);
    c_cfg.limiter = true;
    let r = go(
        &plan_of(&p),
        &c_cfg,
        FakeVideo::default(),
        FakeAudio::constant(0.5),
        FakeTitles::default(),
    );
    assert_eq!(ok(&r).frames, 37);
    assert_eq!(r.sink.track_frames(0), 54419);
}

#[test]
fn limiter_output_starts_at_timeline_zero_not_after_look_ahead() {
    // A constant 0.1 is far below the ceiling, so the limiter does not change it. The first
    // output sample must be the first input sample, not look-ahead silence.
    let r = audio_stack_run(1, sec(1), 48000, 0.1, AudioLayout::Mix, true);
    let first = r.sink.track(0).first().copied().unwrap_or(0.0);
    assert!((first - 0.1).abs() < 0.01, "first sample {first}");
}

#[test]
fn limiter_peak_stays_under_minus_0_9_dbfs_for_three_hot_tracks() {
    let r = audio_stack_run(3, sec(1), 48000, 1.0, AudioLayout::Mix, true);
    assert_eq!(r.sink.track_frames(0), 48000);
    let pk = track_peak(&r, 0, 0).max(track_peak(&r, 0, 1));
    let db = 20.0 * f64::from(pk).log10();
    assert!(db <= -0.9, "peak {db} dBFS");
}

#[test]
fn limiter_peak_stays_under_minus_0_9_dbfs_on_separate_track() {
    let r = audio_stack_run(2, sec(1), 44100, 1.0, AudioLayout::Separate, true);
    for track in 0..2 {
        let pk = track_peak(&r, track, 0).max(track_peak(&r, track, 1));
        let db = 20.0 * f64::from(pk).log10();
        assert!(db <= -0.9, "track {track} peak {db} dBFS");
    }
}

// ---------------------------------------------------------------------------------------------
// Crossfade: the incoming layer fades in over the content beneath (no blend of the outgoing)
// ---------------------------------------------------------------------------------------------

/// Red clip on V1 under a blue clip on V2 that fades in over `fade` (crossfade, starts at 0 s).
fn crossfade_project(fade: Tick) -> Project {
    let mut p = project(black_bg());
    add_media(&mut p, "red", MediaKind::Video);
    add_media(&mut p, "blue", MediaKind::Video);
    let bottom = video_clip(&p, 0, "bottom", "red", Tick::ZERO, sec(2));
    let mut top = video_clip(&p, 1, "top", "blue", Tick::ZERO, sec(2));
    top.transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: fade,
    });
    p.clips.push(bottom);
    p.clips.push(top);
    p
}

#[test]
fn crossfade_first_frame_shows_the_content_beneath_only() {
    let p = crossfade_project(sec(1));
    let v = FakeVideo::default()
        .with("red", [255, 0, 0])
        .with("blue", [0, 0, 255]);
    let r = go(
        &plan_of(&p),
        &cfg(48000),
        v,
        FakeAudio::unavailable(),
        FakeTitles::default(),
    );
    assert!(
        near(r.sink.centre[0], bgra(255, 0, 0), 1),
        "frame 0 {:?}",
        r.sink.centre[0]
    );
}

#[test]
fn crossfade_midpoint_blends_incoming_over_the_beneath_layer() {
    // Half way through a 1 s fade the incoming blue is at 50 % over the red beneath it.
    let p = crossfade_project(sec(1));
    let v = FakeVideo::default()
        .with("red", [255, 0, 0])
        .with("blue", [0, 0, 255]);
    let r = go(
        &plan_of(&p),
        &cfg(48000),
        v,
        FakeAudio::unavailable(),
        FakeTitles::default(),
    );
    assert!(
        near(r.sink.centre[15], bgra(128, 0, 128), 3),
        "frame 15 {:?}",
        r.sink.centre[15]
    );
}

#[test]
fn crossfade_after_fade_shows_incoming_only() {
    let p = crossfade_project(sec(1));
    let v = FakeVideo::default()
        .with("red", [255, 0, 0])
        .with("blue", [0, 0, 255]);
    let r = go(
        &plan_of(&p),
        &cfg(48000),
        v,
        FakeAudio::unavailable(),
        FakeTitles::default(),
    );
    // Frame 30 is t = 1 s, where the fade has completed.
    assert!(r.sink.centre[30..]
        .iter()
        .all(|px| near(*px, bgra(0, 0, 255), 1)));
}
