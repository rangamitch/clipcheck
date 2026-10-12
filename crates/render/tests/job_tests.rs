//! Public-API tests for `clipcheck_render::job` (JobControl and Progress), plus one check that
//! the pipeline drives the frame counter. Synthetic data only.

use clipcheck_render::job::{JobControl, Progress};
use clipcheck_render::pipeline::{
    run, AudioProvider, PipelineConfig, Sink, TitleProvider, VideoProvider,
};
use clipcheck_render::{frame::Frame, RenderError};
use clipcheck_timeline::export::AudioLayout;
use clipcheck_timeline::model::{Canvas, Clip, ClipKind, MediaKind, MediaRef, Project, TextSpec};
use clipcheck_timeline::plan::derive_plan;
use clipcheck_timeline::time::{Rate, Tick};

#[test]
fn new_control_is_idle() {
    let ctl = JobControl::new();
    assert!(!ctl.is_cancelled());
    assert!(!ctl.is_paused());
    assert_eq!(ctl.frames(), 0);
}

#[test]
fn default_matches_new() {
    let ctl = JobControl::default();
    assert!(!ctl.is_cancelled());
    assert!(!ctl.is_paused());
    assert_eq!(ctl.frames(), 0);
}

#[test]
fn cancel_sets_flag_and_is_sticky() {
    let ctl = JobControl::new();
    ctl.cancel();
    assert!(ctl.is_cancelled());
    ctl.cancel();
    assert!(ctl.is_cancelled());
}

#[test]
fn cancel_does_not_change_pause() {
    let ctl = JobControl::new();
    ctl.cancel();
    assert!(!ctl.is_paused());
}

#[test]
fn pause_toggles_both_ways() {
    let ctl = JobControl::new();
    ctl.set_paused(true);
    assert!(ctl.is_paused());
    ctl.set_paused(false);
    assert!(!ctl.is_paused());
}

#[test]
fn pause_does_not_change_cancel() {
    let ctl = JobControl::new();
    ctl.set_paused(true);
    assert!(!ctl.is_cancelled());
}

#[test]
fn add_frames_accumulates() {
    let ctl = JobControl::new();
    ctl.add_frames(3);
    ctl.add_frames(4);
    assert_eq!(ctl.frames(), 7);
}

#[test]
fn add_zero_frames_changes_nothing() {
    let ctl = JobControl::new();
    ctl.add_frames(0);
    assert_eq!(ctl.frames(), 0);
}

#[test]
fn clone_shares_cancel_flag() {
    let a = JobControl::new();
    let b = a.clone();
    b.cancel();
    assert!(a.is_cancelled());
}

#[test]
fn clone_shares_pause_flag() {
    let a = JobControl::new();
    let b = a.clone();
    a.set_paused(true);
    assert!(b.is_paused());
    b.set_paused(false);
    assert!(!a.is_paused());
}

#[test]
fn clone_shares_frame_counter() {
    let a = JobControl::new();
    let b = a.clone();
    b.add_frames(5);
    assert_eq!(a.frames(), 5);
    a.add_frames(2);
    assert_eq!(b.frames(), 7);
}

#[test]
fn independent_controls_do_not_share_state() {
    let a = JobControl::new();
    let b = JobControl::new();
    a.cancel();
    a.set_paused(true);
    a.add_frames(9);
    assert!(!b.is_cancelled());
    assert!(!b.is_paused());
    assert_eq!(b.frames(), 0);
}

#[test]
fn cancel_from_another_thread_is_seen() {
    let ctl = JobControl::new();
    let remote = ctl.clone();
    let handle = std::thread::spawn(move || remote.cancel());
    let _ = handle.join();
    assert!(ctl.is_cancelled());
}

#[test]
fn frame_counter_is_exact_across_threads() {
    let ctl = JobControl::new();
    let mut handles = Vec::new();
    for _ in 0..4 {
        let c = ctl.clone();
        handles.push(std::thread::spawn(move || {
            for _ in 0..250 {
                c.add_frames(1);
            }
        }));
    }
    for h in handles {
        let _ = h.join();
    }
    assert_eq!(ctl.frames(), 1000);
}

#[test]
fn debug_and_clone_of_control_work() {
    let ctl = JobControl::new();
    ctl.add_frames(1);
    let text = format!("{ctl:?}");
    assert!(!text.is_empty());
    let copy = ctl.clone();
    assert_eq!(copy.frames(), 1);
}

#[test]
fn progress_fields_round_trip_and_compare() {
    let a = Progress {
        out_secs: 0.5,
        total_secs: 2.0,
        percent: 25.0,
        frames: 15,
        paused: false,
    };
    let b = a;
    assert_eq!(a, b);
    assert!(format!("{a:?}").contains("percent"));
}

#[test]
fn progress_inequality_on_paused_flag() {
    let a = Progress {
        out_secs: 0.5,
        total_secs: 2.0,
        percent: 25.0,
        frames: 15,
        paused: false,
    };
    let b = Progress { paused: true, ..a };
    assert_ne!(a, b);
}

// ---------------------------------------------------------------------------------------------
// Pipeline drives the frame counter and respects cancel through the shared control.
// ---------------------------------------------------------------------------------------------

struct BlackVideo;

impl VideoProvider for BlackVideo {
    fn frame_at(&mut self, _media_id: &str, _src_time: Tick) -> Result<Option<Frame>, RenderError> {
        Ok(Some(Frame::solid(320, 180, 0, 0, 0, 255)))
    }
}

struct SilentAudio;

impl AudioProvider for SilentAudio {
    fn read(
        &mut self,
        _media_id: &str,
        _stream: u32,
        _src_time: Tick,
        frames: usize,
        _sample_rate: u32,
        out: &mut Vec<f32>,
    ) -> Result<bool, RenderError> {
        out.clear();
        out.resize(frames * 2, 0.0);
        Ok(true)
    }
}

struct NoTitles;

impl TitleProvider for NoTitles {
    fn render(&mut self, _text: &TextSpec, w: u32, h: u32) -> Result<Frame, RenderError> {
        Ok(Frame::solid(w, h, 0, 0, 0, 0))
    }
}

#[derive(Default)]
struct CountSink {
    frames: u64,
    audio_calls: u64,
}

impl Sink for CountSink {
    fn video(&mut self, _frame: &Frame, _index: u64) -> Result<(), RenderError> {
        self.frames += 1;
        Ok(())
    }

    fn audio(&mut self, _track: usize, _samples: &[f32]) -> Result<(), RenderError> {
        self.audio_calls += 1;
        Ok(())
    }
}

fn one_second_project() -> Project {
    let mut p = Project::new("job-test");
    p.canvas = Canvas {
        width: 320,
        height: 180,
        fps: Rate::whole(30),
        ..Canvas::default()
    };
    p.media.push(MediaRef {
        id: "m1".to_string(),
        kind: MediaKind::Video,
        name: "m1".to_string(),
        duration: Tick::from_secs(10),
        width: 320,
        height: 180,
        fps: 30.0,
        ..MediaRef::default()
    });
    let track = p
        .tracks
        .iter()
        .find(|t| t.kind == clipcheck_timeline::model::TrackKind::Video);
    let track_id = track.map(|t| t.id.clone()).unwrap_or_default();
    p.clips.push(Clip {
        id: "c1".to_string(),
        track_id,
        kind: ClipKind::Video,
        media_id: Some("m1".to_string()),
        start: Tick::ZERO,
        dur: Tick::from_secs(1),
        ..Clip::default()
    });
    p
}

fn video_config() -> PipelineConfig {
    PipelineConfig {
        out_w: 320,
        out_h: 180,
        fps: Rate::whole(30),
        sample_rate: 48000,
        layout: AudioLayout::Mix,
        audio_tracks: 1,
        limiter: false,
        loudness_gain_db: 0.0,
        video: true,
        audio: false,
    }
}

#[test]
fn pipeline_adds_one_count_per_video_frame() {
    let plan = derive_plan(&one_second_project(), None);
    let ctl = JobControl::new();
    let mut sink = CountSink::default();
    let res = run(
        &plan,
        &video_config(),
        &mut BlackVideo,
        &mut SilentAudio,
        &mut NoTitles,
        &mut sink,
        &ctl,
        &mut |_| {},
    );
    assert!(res.is_ok());
    assert_eq!(ctl.frames(), 30);
    assert_eq!(sink.frames, 30);
    // audio is disabled in video_config, so the sink never sees audio.
    assert_eq!(sink.audio_calls, 0);
}

#[test]
fn pipeline_cancelled_by_shared_clone_stops_before_frames() {
    let plan = derive_plan(&one_second_project(), None);
    let ctl = JobControl::new();
    let other = ctl.clone();
    other.cancel();
    let mut sink = CountSink::default();
    let res = run(
        &plan,
        &video_config(),
        &mut BlackVideo,
        &mut SilentAudio,
        &mut NoTitles,
        &mut sink,
        &ctl,
        &mut |_| {},
    );
    assert_eq!(res, Err(RenderError::Cancelled));
    assert_eq!(sink.frames, 0);
    assert_eq!(ctl.frames(), 0);
}
