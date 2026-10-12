//! The export pipeline driver (pure): walks a render plan frame by frame, asks providers for
//! source frames and audio, composes, and pushes to a sink. The Windows engine supplies real
//! providers and an encoder sink; tests supply fakes. Owner: worker `render-pipe`.
//! Types/traits are the final CONTRACT.

use std::rc::Rc;
use std::time::{Duration, Instant};

use clipcheck_timeline::audiomath::{clip_gain_at, db_to_lin, pan_gains};
use clipcheck_timeline::export::AudioLayout;
use clipcheck_timeline::model::{
    Canvas, ClipKind, FitMode, Rgba, TextSpec, Transform, TransitionKind,
};
use clipcheck_timeline::plan::{AudioSource, PlanSegment, RenderPlan, VideoLayer};
use clipcheck_timeline::time::{Rate, Tick, MAX_TICK, TICKS_PER_SECOND};

use crate::dsp::{self, Limiter, LoudnessMeter};
use crate::error::RenderError;
use crate::frame::Frame;
use crate::job::{JobControl, Progress};
use crate::reference::{self, LayerInput};

/// Audio block size used when there is no video (audio-only runs and loudness measurement).
const AUDIO_BLOCK: i64 = 1024;
/// Minimum time between two progress callbacks (the final report always fires).
const PROGRESS_EVERY: Duration = Duration::from_millis(250);
/// Sleep between checks while paused.
const PAUSE_POLL: Duration = Duration::from_millis(20);

/// Supplies decoded source frames.
pub trait VideoProvider {
    /// BGRA frame of `media_id` at source time `src_time` (the frame whose time is at or before it,
    /// or the last frame). `Ok(None)` = unavailable (missing/corrupt): the pipeline substitutes
    /// black and records a warning.
    fn frame_at(&mut self, media_id: &str, src_time: Tick) -> Result<Option<Frame>, RenderError>;
}

/// Supplies decoded audio.
pub trait AudioProvider {
    /// Reads `frames` stereo frames (interleaved f32, 48 kHz... the provider resamples to
    /// `sample_rate`) of stream `stream` of `media_id` starting at source time `src_time`, forward
    /// in time. Pads with silence past the end. `Ok(false)` = unavailable (silence is used).
    fn read(
        &mut self,
        media_id: &str,
        stream: u32,
        src_time: Tick,
        frames: usize,
        sample_rate: u32,
        out: &mut Vec<f32>,
    ) -> Result<bool, RenderError>;
}

/// Renders title text to a BGRA frame with alpha, canvas sized.
pub trait TitleProvider {
    /// Renders `text` for a canvas of `w` x `h` pixels.
    fn render(&mut self, text: &TextSpec, w: u32, h: u32) -> Result<Frame, RenderError>;
}

/// Receives the finished output.
pub trait Sink {
    /// One composed output frame, `index` counting from 0 at the output frame rate.
    fn video(&mut self, frame: &Frame, index: u64) -> Result<(), RenderError>;
    /// A block of interleaved stereo f32 samples for audio output track `track` (0 for Mix).
    fn audio(&mut self, track: usize, samples: &[f32]) -> Result<(), RenderError>;
}

/// Settings for one pipeline run.
#[derive(Debug, Clone, PartialEq)]
pub struct PipelineConfig {
    /// Output width.
    pub out_w: u32,
    /// Output height.
    pub out_h: u32,
    /// Output frame rate.
    pub fps: Rate,
    /// Audio sample rate (44100 or 48000).
    pub sample_rate: u32,
    /// Audio layout.
    pub layout: AudioLayout,
    /// Number of audio output tracks (1 for Mix, else number of timeline audio tracks used).
    pub audio_tracks: usize,
    /// Apply the peak limiter at -1 dBFS.
    pub limiter: bool,
    /// Loudness gain in dB applied to the mix after measuring (0.0 = none). The caller measures
    /// with a first pass of [`measure_loudness`] when normalisation is requested.
    pub loudness_gain_db: f64,
    /// Whether to produce video frames (false for audio-only).
    pub video: bool,
    /// Whether to produce audio.
    pub audio: bool,
}

/// What a run produced.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PipelineReport {
    /// Video frames pushed.
    pub frames: u64,
    /// Human-readable warnings (missing media, substituted black/silence).
    pub warnings: Vec<String>,
}

/// Runs the plan over `plan.start..plan.end`. The signature is the public contract, so the
/// argument count is kept. Output frame `n` is for timeline time
/// `plan.start + n / fps`. For each frame: find the segment, collect layers, fetch source frames
/// (one provider call per layer), render Title layers via `titles`, handle transitions
/// (crossfade = the incoming layer fades in over the content beneath it, simplified: the outgoing
/// clip is not blended; dip = [`crate::reference::dip_to_color`]),
/// compose with the reference compositor at the canvas size, scale to `out_w` x `out_h` if
/// different, push to the sink. Audio is produced in blocks of one video frame's worth of samples
/// (accumulated with exact sample counts so total samples == duration * sample_rate): each source
/// is read, speed/reverse applied, multiplied by clip gain/envelope/fades (using
/// `clipcheck_timeline::audiomath`), track fader, pan, summed per output track, limited, and
/// pushed. Checks `ctl` every frame: cancel gives `RenderError::Cancelled`; pause blocks (sleep
/// 20 ms) while reporting progress with `paused = true`. Progress is reported at most every 250 ms
/// and always at the end.
#[allow(clippy::too_many_arguments)]
pub fn run(
    plan: &RenderPlan,
    cfg: &PipelineConfig,
    video: &mut dyn VideoProvider,
    audio: &mut dyn AudioProvider,
    titles: &mut dyn TitleProvider,
    sink: &mut dyn Sink,
    ctl: &JobControl,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<PipelineReport, RenderError> {
    validate_run(plan, cfg)?;
    let dur = plan.end.0.saturating_sub(plan.start.0);
    let total_secs = dur as f64 / TICKS_PER_SECOND as f64;
    let frames = frame_count(dur, cfg.fps);
    let mut total = total_samples(dur, cfg.sample_rate);
    if cfg.video {
        // A range shorter than one frame still gets one frame; its audio must cover that frame.
        total = total.max(sample_index(frames, cfg.sample_rate, cfg.fps));
    }
    let mix = matches!(cfg.layout, AudioLayout::Mix);
    let n_tracks = if mix { 1 } else { cfg.audio_tracks.max(1) };

    let mut video_stage = VideoStage::new(&plan.canvas, cfg);
    let mut audio_stage = AudioStage::new(
        cfg.sample_rate,
        mix,
        n_tracks,
        cfg.audio && cfg.limiter,
        cfg.loudness_gain_db,
        total,
    );
    let mut reporter = Reporter::new(total_secs);
    let mut video_cursor = 0usize;
    let mut pushed: u64 = 0;
    let mut out_secs = 0.0_f64;

    if cfg.video {
        for n in 0..frames {
            wait_while_paused(ctl, &mut reporter, out_secs, pushed, on_progress)?;
            let t_rel = Tick::from_frame(i64::try_from(n).unwrap_or(i64::MAX), cfg.fps);
            let t = plan.start.saturating_add(t_rel);
            let seg = seg_at(&plan.segments, &mut video_cursor, t);
            let frame = video_stage.compose(seg, t, video, titles);
            sink.video(&frame, n)?;
            pushed += 1;
            if cfg.audio {
                let (s0, s1) = frame_samples(n, frames, total, cfg.sample_rate, cfg.fps);
                audio_stage.block(plan, s0, s1, audio, sink)?;
            }
            out_secs = t_rel.as_secs_f64();
            ctl.add_frames(1);
            reporter.emit(out_secs, pushed, false, false, on_progress);
        }
    } else if cfg.audio {
        for b in 0..total_blocks(total) {
            wait_while_paused(ctl, &mut reporter, out_secs, pushed, on_progress)?;
            let (s0, s1) = sample_block(b, total);
            audio_stage.block(plan, s0, s1, audio, sink)?;
            out_secs = Tick::from_sample(s1, cfg.sample_rate).as_secs_f64();
            ctl.add_frames(1);
            reporter.emit(out_secs, pushed, false, false, on_progress);
        }
    }

    if cfg.audio {
        audio_stage.flush(sink)?;
    }
    reporter.emit(total_secs, pushed, false, true, on_progress);

    let mut warnings = Vec::new();
    for w in video_stage.warnings.into_iter().chain(audio_stage.warnings) {
        push_warning(&mut warnings, w);
    }
    Ok(PipelineReport {
        frames: pushed,
        warnings,
    })
}

/// First pass for loudness normalisation: renders only the mixed audio and returns integrated
/// LUFS (None if silent).
pub fn measure_loudness(
    plan: &RenderPlan,
    cfg: &PipelineConfig,
    audio: &mut dyn AudioProvider,
    ctl: &JobControl,
) -> Result<Option<f64>, RenderError> {
    validate_range(plan)?;
    validate_sample_rate(cfg.sample_rate)?;
    let dur = plan.end.0.saturating_sub(plan.start.0);
    let total = total_samples(dur, cfg.sample_rate);
    // Summed into one stereo bus (track 0), no limiter, no loudness gain.
    let mut stage = AudioStage::new(cfg.sample_rate, true, 1, false, 0.0, total);
    let mut meter = LoudnessMeter::new(cfg.sample_rate, 2);
    for b in 0..total_blocks(total) {
        if ctl.is_cancelled() {
            return Err(RenderError::Cancelled);
        }
        let (s0, s1) = sample_block(b, total);
        let samples = usize::try_from(s1.saturating_sub(s0)).unwrap_or(0);
        if samples == 0 {
            continue;
        }
        stage.mix_range(plan, s0, s1, audio);
        if let Some(buf) = stage.tracks.first() {
            meter.push(buf);
        }
    }
    Ok(meter.integrated_lufs())
}

// ---------------------------------------------------------------------------------------------
// Helpers (private).
// ---------------------------------------------------------------------------------------------

/// Checks shared by [`run`] and [`measure_loudness`].
fn validate_run(plan: &RenderPlan, cfg: &PipelineConfig) -> Result<(), RenderError> {
    validate_range(plan)?;
    validate_sample_rate(cfg.sample_rate)?;
    if cfg.out_w == 0 || cfg.out_h == 0 || (cfg.out_w & 1) == 1 || (cfg.out_h & 1) == 1 {
        return Err(RenderError::Invalid(
            "output size must be even and non-zero".to_string(),
        ));
    }
    if cfg.fps.num == 0 || cfg.fps.den == 0 {
        return Err(RenderError::Invalid(
            "frame rate must be positive".to_string(),
        ));
    }
    if plan.canvas.width == 0 || plan.canvas.height == 0 {
        return Err(RenderError::Invalid(
            "canvas size must be non-zero".to_string(),
        ));
    }
    Ok(())
}

fn validate_range(plan: &RenderPlan) -> Result<(), RenderError> {
    if plan.end <= plan.start {
        return Err(RenderError::Invalid("the range is empty".to_string()));
    }
    Ok(())
}

fn validate_sample_rate(sr: u32) -> Result<(), RenderError> {
    if sr == 44_100 || sr == 48_000 {
        Ok(())
    } else {
        Err(RenderError::Invalid(format!(
            "unsupported sample rate {sr}"
        )))
    }
}

/// Adds a warning unless the same text is already recorded (warnings are deduplicated).
fn push_warning(list: &mut Vec<String>, msg: String) {
    if !list.contains(&msg) {
        list.push(msg);
    }
}

fn clamp_i64(v: i128) -> i64 {
    v.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

/// Rounds a tick position computed in floating point into the valid tick range.
fn ticks_to_i64(x: f64) -> i64 {
    if x.is_finite() {
        x.round().clamp(0.0, MAX_TICK as f64) as i64
    } else {
        0
    }
}

/// Look-ahead of [`Limiter::new`] in frames for sample rate `sr` (must match the limiter: 5 ms,
/// at least one frame). Kept here so the delay can be compensated without a limiter accessor.
fn limiter_lookahead(sr: u32) -> usize {
    let sr = if sr == 0 { 48_000.0 } else { f64::from(sr) };
    (sr * 0.005).round().max(1.0) as usize
}

/// Total stereo sample frames per track: round(duration * sample_rate), exact in i128.
fn total_samples(dur: i64, sr: u32) -> i64 {
    let tps = i128::from(TICKS_PER_SECOND);
    let num = 2 * i128::from(dur.max(0)) * i128::from(sr) + tps;
    clamp_i64(num / (2 * tps))
}

/// Sample index of the start of output frame `n`: round(n * sr * den / num).
fn sample_index(n: u64, sr: u32, fps: Rate) -> i64 {
    let num = i128::from(n) * i128::from(sr) * i128::from(fps.den);
    let d = i128::from(fps.num.max(1));
    clamp_i64((2 * num + d) / (2 * d))
}

/// Sample range `[s0, s1)` of the audio block that accompanies output frame `n`. The last
/// frame's block runs to the exact total so the track length is exact.
fn frame_samples(n: u64, frames: u64, total: i64, sr: u32, fps: Rate) -> (i64, i64) {
    let s0 = sample_index(n, sr, fps);
    let s1 = if n + 1 >= frames {
        total
    } else {
        sample_index(n + 1, sr, fps)
    };
    (s0, s1.max(s0))
}

/// Sample range `[s0, s1)` of audio-only block `b` (fixed [`AUDIO_BLOCK`] size).
fn sample_block(b: u64, total: i64) -> (i64, i64) {
    let s0 = i64::try_from(b)
        .unwrap_or(i64::MAX)
        .saturating_mul(AUDIO_BLOCK);
    let s1 = s0.saturating_add(AUDIO_BLOCK).min(total).max(s0);
    (s0, s1)
}

/// Number of output frames for a range of `dur` ticks: the whole frames inside it, but at least
/// one for a non-empty range (a shorter range would otherwise produce no video at all).
fn frame_count(dur: i64, fps: Rate) -> u64 {
    if dur <= 0 {
        return 0;
    }
    u64::try_from(Tick(dur).to_frame(fps)).unwrap_or(0).max(1)
}

/// Smallest sample index `s >= lo` whose time `start + Tick::from_sample(s)` is at or after
/// `boundary`. Samples before it lie before the boundary.
fn sample_reaching(start: Tick, boundary: Tick, sr: u32, lo: i64) -> i64 {
    let reached = |s: i64| start.saturating_add(Tick::from_sample(s, sr)) >= boundary;
    // Start from the fractional estimate, then step to the exact first sample.
    let rel = i128::from(boundary.0) - i128::from(start.0);
    let guess = (rel * i128::from(sr)).div_euclid(i128::from(TICKS_PER_SECOND));
    let mut s = clamp_i64(guess).max(lo);
    while s > lo && reached(s - 1) {
        s -= 1;
    }
    while !reached(s) {
        s += 1;
    }
    s
}

fn total_blocks(total: i64) -> u64 {
    u64::try_from(total.max(0))
        .unwrap_or(0)
        .div_ceil(AUDIO_BLOCK as u64)
}

/// The segment containing `t`, advancing `cursor` (times passed in are non-decreasing).
fn seg_at<'a>(segs: &'a [PlanSegment], cursor: &mut usize, t: Tick) -> Option<&'a PlanSegment> {
    while segs.get(*cursor).is_some_and(|s| s.end <= t) {
        *cursor += 1;
    }
    segs.get(*cursor).filter(|s| s.start <= t)
}

/// Source time of a video layer at timeline time `t`: `src_time` moves by `elapsed * speed`
/// forward, or backward for reverse clips. Freeze (speed 0) always shows `src_time`.
fn source_time(layer: &VideoLayer, seg_start: Tick, t: Tick) -> Tick {
    if !layer.speed.is_finite() || layer.speed <= 0.0 {
        return layer.src_time;
    }
    let elapsed = (t.0 - seg_start.0).max(0) as f64;
    let off = (elapsed * layer.speed).round() as i128;
    let base = i128::from(layer.src_time.0);
    let v = if layer.reverse {
        base - off
    } else {
        base + off
    };
    Tick(clamp_i64(v).clamp(0, MAX_TICK))
}

/// Layer opacity and, for dips below half progress, the dip colour and its strength.
fn layer_opacity(layer: &VideoLayer, t: Tick) -> (f64, Option<(Rgba, f64)>) {
    let base = if layer.opacity.is_finite() {
        layer.opacity.clamp(0.0, 1.0)
    } else {
        1.0
    };
    let Some(tr) = layer.transition else {
        return (base, None);
    };
    let dur = tr.duration.0;
    let elapsed = t.0.saturating_sub(layer.clip_start.0);
    if dur <= 0 || elapsed < 0 || elapsed >= dur {
        return (base, None);
    }
    let p = elapsed as f64 / dur as f64;
    match tr.kind {
        // Simplification: the layer fades in over whatever is beneath it.
        TransitionKind::Crossfade => (base * p, None),
        TransitionKind::DipToBlack => (base * p, dip_for(Rgba::BLACK, p)),
        TransitionKind::DipToWhite => (base * p, dip_for(Rgba::WHITE, p)),
    }
}

fn dip_for(color: Rgba, p: f64) -> Option<(Rgba, f64)> {
    if p < 0.5 {
        Some((color, 1.0 - 2.0 * p))
    } else {
        None
    }
}

/// Wraps the output and waits while paused. Cancel wins over pause.
fn wait_while_paused(
    ctl: &JobControl,
    reporter: &mut Reporter,
    out_secs: f64,
    frames: u64,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<(), RenderError> {
    if ctl.is_cancelled() {
        return Err(RenderError::Cancelled);
    }
    while ctl.is_paused() {
        reporter.emit(out_secs, frames, true, false, on_progress);
        std::thread::sleep(PAUSE_POLL);
        if ctl.is_cancelled() {
            return Err(RenderError::Cancelled);
        }
    }
    Ok(())
}

/// Throttled progress reporting (at most one call per [`PROGRESS_EVERY`] unless forced).
struct Reporter {
    last: Option<Instant>,
    total_secs: f64,
}

impl Reporter {
    fn new(total_secs: f64) -> Reporter {
        Reporter {
            last: None,
            total_secs,
        }
    }

    fn emit(
        &mut self,
        out_secs: f64,
        frames: u64,
        paused: bool,
        force: bool,
        on_progress: &mut dyn FnMut(Progress),
    ) {
        let now = Instant::now();
        let recent = self
            .last
            .is_some_and(|l| now.duration_since(l) < PROGRESS_EVERY);
        if !force && recent {
            return;
        }
        self.last = Some(now);
        let percent = if self.total_secs > 0.0 {
            (out_secs / self.total_secs * 100.0).clamp(0.0, 100.0) as f32
        } else {
            100.0
        };
        on_progress(Progress {
            out_secs,
            total_secs: self.total_secs,
            percent,
            frames,
            paused,
        });
    }
}

/// Video composition state: canvas size, title cache and warnings.
struct VideoStage {
    cw: u32,
    ch: u32,
    ow: u32,
    oh: u32,
    background: Rgba,
    blur: bool,
    /// Last rendered title. The canvas size is fixed for a run, so the spec alone is the key.
    title_cache: Option<(TextSpec, Rc<Frame>)>,
    /// Last solid colour layer, reused while the colour stays the same.
    solid_cache: Option<(Rgba, Rc<Frame>)>,
    /// Shared canvas-sized frames, built on first use and reused for every later frame.
    transparent_frame: Option<Rc<Frame>>,
    black_frame: Option<Rc<Frame>>,
    warnings: Vec<String>,
}

impl VideoStage {
    fn new(canvas: &Canvas, cfg: &PipelineConfig) -> VideoStage {
        VideoStage {
            cw: canvas.width,
            ch: canvas.height,
            ow: cfg.out_w,
            oh: cfg.out_h,
            background: canvas.background,
            blur: canvas.blur_background,
            title_cache: None,
            solid_cache: None,
            transparent_frame: None,
            black_frame: None,
            warnings: Vec::new(),
        }
    }

    fn transparent(&mut self) -> Rc<Frame> {
        let (w, h) = (self.cw, self.ch);
        self.transparent_frame
            .get_or_insert_with(|| Rc::new(Frame::solid(w, h, 0, 0, 0, 0)))
            .clone()
    }

    fn black(&mut self) -> Rc<Frame> {
        let (w, h) = (self.cw, self.ch);
        self.black_frame
            .get_or_insert_with(|| Rc::new(Frame::solid(w, h, 0, 0, 0, 255)))
            .clone()
    }

    /// Composes the output frame at timeline time `t` for segment `seg` (None = gap).
    fn compose(
        &mut self,
        seg: Option<&PlanSegment>,
        t: Tick,
        video: &mut dyn VideoProvider,
        titles: &mut dyn TitleProvider,
    ) -> Frame {
        let (seg_start, layers): (Tick, &[VideoLayer]) = match seg {
            Some(s) => (s.start, s.video.as_slice()),
            None => (t, &[]),
        };
        let mut sources: Vec<(Rc<Frame>, f64)> = Vec::with_capacity(layers.len());
        let mut dip: Option<(Rgba, f64)> = None;
        for layer in layers {
            let frame = self.layer_frame(layer, seg_start, t, video, titles);
            let (opacity, layer_dip) = layer_opacity(layer, t);
            if let Some((color, k)) = layer_dip {
                // Keep the strongest dip when several transitions overlap.
                let keep_old = dip.is_some_and(|(_, old)| old >= k);
                if !keep_old {
                    dip = Some((color, k));
                }
            }
            sources.push((frame, opacity));
        }

        // Blurred backdrop behind fit-mode clips when the canvas asks for it. It is skipped when the
        // first layer would hide it completely.
        let blur_frame = match (self.blur, layers.first(), sources.first()) {
            (true, Some(layer), Some((frame, opacity)))
                if layer.transform.fit == FitMode::Fit
                    && !covers_canvas(frame, layer, *opacity, self.cw, self.ch) =>
            {
                Some(reference::blurred_cover(frame, self.cw, self.ch))
            }
            _ => None,
        };
        let fill = Transform {
            fit: FitMode::Fill,
            ..Transform::default()
        };

        let mut inputs: Vec<LayerInput<'_>> = Vec::with_capacity(layers.len() + 1);
        if let Some(bf) = blur_frame.as_ref() {
            inputs.push(LayerInput {
                frame: bf,
                transform: &fill,
                opacity: 1.0,
            });
        }
        for ((frame, opacity), layer) in sources.iter().zip(layers.iter()) {
            inputs.push(LayerInput {
                frame,
                transform: &layer.transform,
                opacity: *opacity,
            });
        }

        let mut out = reference::compose(self.cw, self.ch, self.background, &inputs);
        if let Some((color, k)) = dip {
            out = reference::dip_to_color(&out, color, k);
        }
        if self.cw != self.ow || self.ch != self.oh {
            out = reference::resize(&out, self.ow, self.oh);
        }
        out
    }

    /// Pixels for one layer at time `t` (before opacity and placement).
    fn layer_frame(
        &mut self,
        layer: &VideoLayer,
        seg_start: Tick,
        t: Tick,
        video: &mut dyn VideoProvider,
        titles: &mut dyn TitleProvider,
    ) -> Rc<Frame> {
        match layer.kind {
            ClipKind::Title => {
                let Some(spec) = layer.text.as_ref() else {
                    return self.transparent();
                };
                if let Some((cached, frame)) = self.title_cache.as_ref() {
                    if cached == spec {
                        return frame.clone();
                    }
                }
                match titles.render(spec, self.cw, self.ch) {
                    Ok(frame) => {
                        let frame = Rc::new(frame);
                        self.title_cache = Some((spec.clone(), frame.clone()));
                        frame
                    }
                    Err(_) => {
                        let msg = format!("title {} could not be rendered, skipped", layer.clip_id);
                        push_warning(&mut self.warnings, msg);
                        self.transparent()
                    }
                }
            }
            ClipKind::Solid => {
                let c = layer.solid.unwrap_or(Rgba::BLACK);
                if let Some((cached, frame)) = self.solid_cache.as_ref() {
                    if *cached == c {
                        return frame.clone();
                    }
                }
                let frame = Rc::new(Frame::solid(self.cw, self.ch, c.b, c.g, c.r, c.a));
                self.solid_cache = Some((c, frame.clone()));
                frame
            }
            ClipKind::Video | ClipKind::Image | ClipKind::Audio => {
                let mid = layer.media_id.as_deref().unwrap_or("");
                let st = source_time(layer, seg_start, t);
                match video.frame_at(mid, st) {
                    Ok(Some(frame)) => Rc::new(frame),
                    _ => {
                        let msg = format!("media {mid} unavailable, black used");
                        push_warning(&mut self.warnings, msg);
                        self.black()
                    }
                }
            }
        }
    }
}

/// Peak limiter state of one output track. The limiter delays its output by its look-ahead, so
/// the first `skip` output frames (the look-ahead silence) are discarded and the flushed tail is
/// trimmed so that exactly `expected` frames reach the sink.
struct LimitedTrack {
    limiter: Limiter,
    /// Limiter output frames still to discard.
    skip: usize,
    /// Frames pushed to the sink so far.
    emitted: usize,
}

/// Audio mixing state: track buffers, limiters, loudness gain and warnings.
struct AudioStage {
    sr: u32,
    /// True when every source goes to track 0 (Mix layout and loudness measurement).
    mix: bool,
    tracks: Vec<Vec<f32>>,
    limiters: Vec<Option<LimitedTrack>>,
    loud_lin: f32,
    /// Stereo frames each track must deliver in total (round(duration * sample_rate)).
    expected: usize,
    /// Segment cursor; block start times are non-decreasing.
    cursor: usize,
    /// Scratch: one source's samples for the current block (reused across blocks and sources).
    read_buf: Vec<f32>,
    /// Scratch for the speed change, which ping-pongs with `read_buf`.
    tmp_buf: Vec<f32>,
    warnings: Vec<String>,
}

impl AudioStage {
    fn new(
        sr: u32,
        mix: bool,
        n_tracks: usize,
        limit: bool,
        loud_db: f64,
        total: i64,
    ) -> AudioStage {
        let limiters = (0..n_tracks)
            .map(|_| {
                if limit {
                    Some(LimitedTrack {
                        limiter: Limiter::new(-1.0, sr, 2),
                        skip: limiter_lookahead(sr),
                        emitted: 0,
                    })
                } else {
                    None
                }
            })
            .collect();
        AudioStage {
            sr,
            mix,
            tracks: vec![Vec::new(); n_tracks],
            limiters,
            loud_lin: db_to_lin(loud_db) as f32,
            expected: usize::try_from(total.max(0)).unwrap_or(0),
            cursor: 0,
            read_buf: Vec::new(),
            tmp_buf: Vec::new(),
            warnings: Vec::new(),
        }
    }

    /// Mixes the block `[s0, s1)` of the plan and pushes it to the sink.
    fn block(
        &mut self,
        plan: &RenderPlan,
        s0: i64,
        s1: i64,
        audio: &mut dyn AudioProvider,
        sink: &mut dyn Sink,
    ) -> Result<(), RenderError> {
        if s1.saturating_sub(s0) <= 0 {
            return Ok(());
        }
        self.mix_range(plan, s0, s1, audio);
        self.finish_block(sink)
    }

    /// Zeroes the tracks and mixes the sample range `[s0, s1)` of the plan into them. The range is
    /// split where the plan segments change, so every part is read and gained from the segment that
    /// really contains it: a cut inside a block must not give the whole block the earlier clip.
    fn mix_range(&mut self, plan: &RenderPlan, s0: i64, s1: i64, audio: &mut dyn AudioProvider) {
        let samples = usize::try_from(s1.saturating_sub(s0)).unwrap_or(0);
        for t in self.tracks.iter_mut() {
            t.clear();
            t.resize(samples * 2, 0.0);
        }
        let mut s = s0;
        while s < s1 {
            let t = plan.start.saturating_add(Tick::from_sample(s, self.sr));
            let seg = seg_at(&plan.segments, &mut self.cursor, t);
            // The part ends where the active segment ends, or where the next one starts in a gap.
            let boundary = match seg {
                Some(seg) => Some(seg.end),
                None => plan.segments.get(self.cursor).map(|next| next.start),
            };
            let end = match boundary {
                Some(b) => sample_reaching(plan.start, b, self.sr, s + 1).min(s1),
                None => s1,
            };
            let off = usize::try_from(s - s0).unwrap_or(0);
            let n = usize::try_from(end - s).unwrap_or(0);
            self.mix_part(seg, t, n, off, audio);
            s = end;
        }
    }

    /// Sums every audible source of `seg` for the `n` samples starting at `block_t`, into the
    /// tracks at sample offset `off`.
    fn mix_part(
        &mut self,
        seg: Option<&PlanSegment>,
        block_t: Tick,
        n: usize,
        off: usize,
        audio: &mut dyn AudioProvider,
    ) {
        let Some(seg) = seg else {
            return;
        };
        let last = self.tracks.len().saturating_sub(1);
        for src in &seg.audio {
            // A muted clip or a track at -120 dB or below contributes exact silence: neither read
            // it from the provider nor mix it.
            if src.audio.muted || db_to_lin(src.track_db) == 0.0 {
                continue;
            }
            let idx = if self.mix {
                0
            } else {
                (src.track_index as usize).min(last)
            };
            self.read_source(src, seg.start, block_t, n, audio);
            apply_clip_gain(&mut self.read_buf, src, block_t, n, self.sr);
            let dst = self
                .tracks
                .get_mut(idx)
                .and_then(|t| t.get_mut(off * 2..(off + n) * 2));
            if let Some(dst) = dst {
                dsp::mix_into(dst, &self.read_buf, 1.0);
            }
        }
    }

    /// Reads one source for a block into `read_buf`: `samples` stereo frames at output speed.
    /// Handles speed (read `ceil(samples * speed)` source frames then time-stretch), reverse and
    /// mono. Missing audio gives silence and a warning.
    fn read_source(
        &mut self,
        src: &AudioSource,
        seg_start: Tick,
        block_t: Tick,
        samples: usize,
        audio: &mut dyn AudioProvider,
    ) {
        let speed = if src.speed.is_finite() && src.speed > 0.0 {
            src.speed
        } else {
            1.0
        };
        let elapsed = (block_t.0 - seg_start.0) as f64 * speed;
        let base = src.src_time.0 as f64;
        let pos0 = if src.reverse {
            base - elapsed
        } else {
            base + elapsed
        };
        let need = (samples as f64 * speed).ceil() as usize;
        // Reverse reads the window that ends at `pos0`, then flips it.
        let window = need as f64 * TICKS_PER_SECOND as f64 / self.sr as f64;
        let start = if src.reverse { pos0 - window } else { pos0 };

        let buf = &mut self.read_buf;
        buf.clear();
        let read_ok = matches!(
            audio.read(
                &src.media_id,
                src.source_track,
                Tick(ticks_to_i64(start)),
                need,
                self.sr,
                buf
            ),
            Ok(true)
        );
        if !read_ok {
            let msg = format!("media {} audio unavailable, silence used", src.media_id);
            push_warning(&mut self.warnings, msg);
            buf.clear();
        }
        buf.resize(need * 2, 0.0);
        if src.reverse {
            dsp::reverse_frames_in_place(buf, 2);
        }
        if (speed - 1.0).abs() > 1e-9 {
            dsp::change_speed_into(buf.as_slice(), 2, speed, &mut self.tmp_buf);
            std::mem::swap(&mut self.read_buf, &mut self.tmp_buf);
        }
        let buf = &mut self.read_buf;
        buf.resize(samples * 2, 0.0);
        if src.audio.mono {
            dsp::stereo_to_mono_in_place(buf);
        }
    }

    /// Applies loudness gain and the limiter, then pushes each track. Limited output drops its
    /// look-ahead silence first, so the stream stays aligned with the source timeline.
    fn finish_block(&mut self, sink: &mut dyn Sink) -> Result<(), RenderError> {
        let loud = self.loud_lin;
        for (i, (buf, lim)) in self
            .tracks
            .iter_mut()
            .zip(self.limiters.iter_mut())
            .enumerate()
        {
            // A gain of exactly 1.0 leaves every sample unchanged, so the pass is skipped.
            if loud != 1.0 {
                for s in buf.iter_mut() {
                    *s *= loud;
                }
            }
            match lim.as_mut() {
                Some(state) => {
                    state.limiter.process(buf);
                    let cut = state.skip.min(buf.len() / 2);
                    state.skip -= cut;
                    let kept: &[f32] = buf.get(cut * 2..).unwrap_or(&[]);
                    state.emitted += kept.len() / 2;
                    if !kept.is_empty() {
                        sink.audio(i, kept)?;
                    }
                }
                None => sink.audio(i, buf.as_slice())?,
            }
        }
        Ok(())
    }

    /// Pushes the limiter tails at the end of the run. The tail is trimmed or zero-padded so that
    /// each limited track delivers exactly `expected` frames in total.
    fn flush(&mut self, sink: &mut dyn Sink) -> Result<(), RenderError> {
        let expected = self.expected;
        for (i, lim) in self.limiters.iter_mut().enumerate() {
            if let Some(state) = lim.as_mut() {
                let tail = state.limiter.flush();
                let cut = state.skip.min(tail.len() / 2);
                state.skip -= cut;
                let mut out: Vec<f32> = tail.get(cut * 2..).unwrap_or(&[]).to_vec();
                let need = expected.saturating_sub(state.emitted);
                out.resize(need * 2, 0.0);
                state.emitted += need;
                if !out.is_empty() {
                    sink.audio(i, &out)?;
                }
            }
        }
        Ok(())
    }
}

/// True when `layer` is opaque decoded video whose destination rectangle covers the whole canvas, so
/// a blurred backdrop behind it would never be visible. Images and titles may carry alpha and are
/// never treated as covering.
fn covers_canvas(frame: &Frame, layer: &VideoLayer, opacity: f64, cw: u32, ch: u32) -> bool {
    if !matches!(layer.kind, ClipKind::Video) || opacity < 1.0 {
        return false;
    }
    let r = reference::dest_rect(frame.width, frame.height, cw, ch, &layer.transform);
    const EPS: f64 = 1e-6;
    r.x <= EPS && r.y <= EPS && r.x + r.w >= f64::from(cw) - EPS && r.y + r.h >= f64::from(ch) - EPS
}

/// Multiplies a block by the clip gain (interpolated between the block ends), the track fader
/// and the constant-power pan.
fn apply_clip_gain(buf: &mut [f32], src: &AudioSource, block_t: Tick, samples: usize, sr: u32) {
    let rel0 = block_t.0.saturating_sub(src.clip_start.0);
    let rel1 = rel0.saturating_add(Tick::from_sample(samples as i64, sr).0);
    let g0 = clip_gain_at(&src.audio, src.clip_dur, Tick(rel0));
    let g1 = clip_gain_at(&src.audio, src.clip_dur, Tick(rel1));
    let track = db_to_lin(src.track_db);
    let denom = samples.max(1) as f64;
    let (pl, pr) = pan_gains(src.audio.pan);
    let (pl, pr) = (pl as f32, pr as f32);
    // One pass: clip gain then pan, in the same float operation order as two separate passes.
    for (i, frame) in buf
        .as_chunks_mut::<2>()
        .0
        .iter_mut()
        .take(samples)
        .enumerate()
    {
        let f = i as f64 / denom;
        let g = ((g0 + (g1 - g0) * f) * track) as f32;
        let [l, r] = frame;
        *l = (*l * g) * pl;
        *r = (*r * g) * pr;
    }
}
