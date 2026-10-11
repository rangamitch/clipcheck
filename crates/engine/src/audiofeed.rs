//! Audio feeding for the recorder (tasks A1b and A3, design D14).
//!
//! [`AudioFeeder::start`] opens one capture per distinct source used by any
//! track. A source that appears in several tracks is captured once and fanned
//! out. One mixer thread runs every 20 ms. It drains the sources into
//! [`SourceRing`](crate::audiomix::SourceRing)s, then builds one block per track
//! from each track's sources, gains and noise gates, and sends the blocks on
//! the per-track receivers.
//!
//! A source that cannot start, or stops while running, is fed silence and a
//! warning is recorded. An application that is not running is silence without
//! a warning. Every track therefore stays continuous, and a missing microphone
//! never fails the recording.

use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use clipcheck_audio::{AudioChunk, OUT_CHANNELS, OUT_RATE};
#[cfg(windows)]
use clipcheck_audio::{LoopbackMode, Microphone, ProcessLoopback, SystemMix};

use crate::audiomix::{db_to_gain, LevelTap, Mixer, NoiseGate, SourceEdge, BLOCK_MS};
use crate::procs::{lock, AppWatcher};

/// Length of one generated silence block, in milliseconds.
pub const SILENCE_BLOCK_MS: u64 = 20;
/// Longest the mixer sleeps before it re-checks the stop flag.
const POLL: Duration = Duration::from_millis(5);
/// If the mixer falls this far behind its schedule, it resumes from now.
const RESYNC: Duration = Duration::from_millis(200);

/// Where a track's audio comes from.
///
/// `OtherApps` and `AllExceptGame` are the same capture in this version. Both
/// take every process except the attached window's tree. Excluding the
/// configured apps is not implemented yet, so `OtherApps` is currently an
/// alias for `AllExceptGame`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AudioSourceKind {
    /// Mix of the default render endpoint (whole-system loopback).
    SystemMix,
    /// Audio of the attached window's process tree only.
    GameProcess,
    /// Audio of everything except the attached window's process tree.
    AllExceptGame,
    /// Everything except the game. Same capture as `AllExceptGame` for now.
    OtherApps,
    /// Every running instance of an executable, by file name, matched
    /// case-insensitively. Each instance tree is captured while it runs.
    App {
        /// Executable file name, for example `discord.exe`.
        exe: String,
    },
    /// A capture endpoint. `device: None` selects the default microphone.
    Microphone {
        /// Endpoint ID, or `None` for the default capture endpoint.
        device: Option<String>,
    },
}

/// One source inside a track's mix.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackSource {
    /// Where this source's audio comes from.
    pub kind: AudioSourceKind,
    /// Gain of this source in this track, in decibels. `0.0` leaves it as is.
    pub gain_db: f32,
    /// Noise gate threshold in decibels. Only used for microphones. `None`
    /// means no gate.
    pub gate_db: Option<f32>,
}

/// One audio track of a recording. Maps to one AAC track in the output. The
/// track is the sum of its sources.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioTrackConfig {
    /// Track title stored in the output (for example `"Game"`).
    pub title: String,
    /// Sources mixed into this track. An empty list gives silence.
    pub sources: Vec<TrackSource>,
    /// AAC bitrate of the track in kilobits per second. Must be non-zero.
    pub bitrate_kbps: u32,
}

/// Converts a gain in decibels to a linear factor (`10^(db / 20)`).
///
/// `-inf` maps to `0.0`. NaN maps to `1.0` (no change).
pub fn db_to_linear(db: f32) -> f32 {
    db_to_gain(db)
}

/// Returns `samples` multiplied by `gain` and clamped to `[-1.0, 1.0]`.
///
/// NaN results become `0.0`.
pub fn apply_gain(samples: &[f32], gain: f32) -> Vec<f32> {
    samples
        .iter()
        .map(|&s| {
            let v = s * gain;
            if v.is_nan() {
                0.0
            } else {
                v.clamp(-1.0, 1.0)
            }
        })
        .collect()
}

/// One block of silence: 20 ms of interleaved stereo at 48 kHz.
pub fn silence_block() -> Vec<f32> {
    let frames = (u64::from(OUT_RATE) * SILENCE_BLOCK_MS / 1000) as usize;
    vec![0.0; frames * OUT_CHANNELS]
}

/// Identity of a capture, used to fan one capture out to every track that
/// names it. Equal keys share one capture.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum SourceKey {
    SystemMix,
    Game,
    ExceptGame,
    App(String),
    Mic(Option<String>),
}

fn source_key(kind: &AudioSourceKind) -> SourceKey {
    match kind {
        AudioSourceKind::SystemMix => SourceKey::SystemMix,
        AudioSourceKind::GameProcess => SourceKey::Game,
        AudioSourceKind::AllExceptGame | AudioSourceKind::OtherApps => SourceKey::ExceptGame,
        AudioSourceKind::App { exe } => SourceKey::App(exe.to_ascii_lowercase()),
        AudioSourceKind::Microphone { device } => SourceKey::Mic(device.clone()),
    }
}

/// Something that can be stopped from the owning thread.
trait StopSource: Send {
    fn stop_source(&mut self);
}

#[cfg(windows)]
impl StopSource for ProcessLoopback {
    fn stop_source(&mut self) {
        self.stop();
    }
}

#[cfg(windows)]
impl StopSource for SystemMix {
    fn stop_source(&mut self) {
        self.stop();
    }
}

#[cfg(windows)]
impl StopSource for Microphone {
    fn stop_source(&mut self) {
        self.stop();
    }
}

/// Opens the capture source for `kind`. Returns the stopper and its chunk stream.
/// `App` sources are not opened here. They use an [`AppWatcher`].
#[cfg(windows)]
fn open_source(
    kind: &AudioSourceKind,
    target_pid: u32,
) -> Result<(Box<dyn StopSource>, Receiver<AudioChunk>), String> {
    match kind {
        AudioSourceKind::SystemMix => SystemMix::start()
            .map(|(s, rx)| (Box::new(s) as Box<dyn StopSource>, rx))
            .map_err(|e| e.to_string()),
        AudioSourceKind::GameProcess
        | AudioSourceKind::AllExceptGame
        | AudioSourceKind::OtherApps => {
            if target_pid == 0 {
                return Err("no process id for the attached window".to_string());
            }
            let mode = if *kind == AudioSourceKind::GameProcess {
                LoopbackMode::IncludeTree
            } else {
                LoopbackMode::ExcludeTree
            };
            ProcessLoopback::start(target_pid, mode)
                .map(|(s, rx)| (Box::new(s) as Box<dyn StopSource>, rx))
                .map_err(|e| e.to_string())
        }
        AudioSourceKind::Microphone { device } => Microphone::start(device.clone())
            .map(|(s, rx)| (Box::new(s) as Box<dyn StopSource>, rx))
            .map_err(|e| e.to_string()),
        AudioSourceKind::App { .. } => Err("app sources are not opened directly".to_string()),
    }
}

/// Audio capture is Windows-only. Every track falls back to silence elsewhere.
#[cfg(not(windows))]
fn open_source(
    _kind: &AudioSourceKind,
    _target_pid: u32,
) -> Result<(Box<dyn StopSource>, Receiver<AudioChunk>), String> {
    Err("audio capture is only supported on Windows".to_string())
}

/// Where the mixer gets samples for one distinct source.
enum Input {
    /// A capture stream. `None` once it has stopped, or when it never started.
    Chunks(Option<Receiver<AudioChunk>>),
    /// Every running instance of an application.
    App(AppWatcher),
}

/// Running audio capture for a set of tracks. Created by [`AudioFeeder::start`].
///
/// Dropping it calls [`AudioFeeder::stop`].
pub struct AudioFeeder {
    stop: Arc<AtomicBool>,
    sources: Vec<Box<dyn StopSource>>,
    thread: Option<JoinHandle<()>>,
    warnings: Arc<Mutex<Vec<String>>>,
    /// Per-track level meters, updated by the mixer thread on every block.
    levels: Arc<LevelTap>,
}

impl AudioFeeder {
    /// Opens one capture per distinct source and starts the mixer thread.
    ///
    /// Returns one receiver per track, in the same order as `tracks`. Each
    /// receiver yields blocks of interleaved stereo `f32` at 48 kHz, 20 ms
    /// each. A source that fails to start is fed silence and a warning is
    /// recorded. `target_pid` is the process of the attached window, used by
    /// the `GameProcess` and `AllExceptGame` sources. Only a thread spawn
    /// failure is an error.
    pub fn start(
        tracks: &[AudioTrackConfig],
        target_pid: u32,
    ) -> io::Result<(AudioFeeder, Vec<Receiver<Vec<f32>>>)> {
        let mut feeder = AudioFeeder {
            stop: Arc::new(AtomicBool::new(false)),
            sources: Vec::new(),
            thread: None,
            warnings: Arc::new(Mutex::new(Vec::new())),
            // One slot per track. The mixer sends one block per track, so this
            // matches `outs.len()` below.
            levels: Arc::new(LevelTap::new(tracks.len())),
        };
        let mut index: HashMap<SourceKey, usize> = HashMap::new();
        let mut inputs: Vec<Input> = Vec::new();
        let mut labels: Vec<String> = Vec::new();
        let mut edges: Vec<Vec<SourceEdge>> = Vec::with_capacity(tracks.len());
        let mut outs: Vec<Sender<Vec<f32>>> = Vec::with_capacity(tracks.len());
        let mut receivers = Vec::with_capacity(tracks.len());

        for track in tracks {
            let (tx, rx) = mpsc::channel();
            outs.push(tx);
            receivers.push(rx);
            let mut track_edges = Vec::with_capacity(track.sources.len());
            for src in &track.sources {
                let key = source_key(&src.kind);
                let idx = match index.get(&key) {
                    Some(&i) => i,
                    None => {
                        let label = format!("track \"{}\"", track.title);
                        let input = match &src.kind {
                            AudioSourceKind::App { exe } => {
                                Input::App(AppWatcher::new(exe.clone()))
                            }
                            kind => match open_source(kind, target_pid) {
                                Ok((stopper, chunks)) => {
                                    feeder.sources.push(stopper);
                                    Input::Chunks(Some(chunks))
                                }
                                Err(e) => {
                                    push_warning(
                                        &feeder.warnings,
                                        format!("{label}: {e}; recording silence"),
                                    );
                                    Input::Chunks(None)
                                }
                            },
                        };
                        inputs.push(input);
                        labels.push(label);
                        index.insert(key, inputs.len() - 1);
                        inputs.len() - 1
                    }
                };
                let gate = match (&src.kind, src.gate_db) {
                    (AudioSourceKind::Microphone { .. }, Some(db)) => Some(NoiseGate::new(db)),
                    _ => None,
                };
                track_edges.push(SourceEdge {
                    source: idx,
                    gain: db_to_gain(src.gain_db),
                    gate,
                });
            }
            edges.push(track_edges);
        }

        let mixer = Mixer::new(inputs.len(), edges);
        let stop = feeder.stop.clone();
        let warnings = feeder.warnings.clone();
        let levels = feeder.levels.clone();
        let handle = thread::Builder::new()
            .name("clipcheck-audio-mix".into())
            .spawn(move || run_mixer(mixer, inputs, labels, outs, stop, warnings, levels))?;
        feeder.thread = Some(handle);
        Ok((feeder, receivers))
    }

    /// Returns the warnings raised since the last call, and clears them.
    ///
    /// Covers sources that fell back to silence at start and sources that
    /// stopped while running.
    pub fn warnings(&self) -> Vec<String> {
        std::mem::take(&mut *lock(&self.warnings))
    }

    /// Shared handle to the warnings list. A live reader can take warnings from
    /// it while the feeder runs. See [`AudioFeeder::warnings`].
    pub(crate) fn warnings_handle(&self) -> Arc<Mutex<Vec<String>>> {
        self.warnings.clone()
    }

    /// Shared handle to the per-track level meters. See [`LevelTap::take`].
    pub fn levels(&self) -> Arc<LevelTap> {
        self.levels.clone()
    }

    /// Stops every capture, then joins the mixer thread. The receivers
    /// disconnect once this returns. Calling it again does nothing.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        for source in self.sources.iter_mut() {
            source.stop_source();
        }
        self.sources.clear();
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for AudioFeeder {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Mixer loop. Paces itself to one tick per 20 ms, drains every input, and sends
/// one block per track. Runs until `stop` is set, or until every track receiver
/// is gone.
fn run_mixer(
    mut mixer: Mixer,
    mut inputs: Vec<Input>,
    labels: Vec<String>,
    outs: Vec<Sender<Vec<f32>>>,
    stop: Arc<AtomicBool>,
    warnings: Arc<Mutex<Vec<String>>>,
    levels: Arc<LevelTap>,
) {
    let block = Duration::from_millis(BLOCK_MS);
    let mut next = Instant::now();
    let mut alive: Vec<bool> = vec![true; outs.len()];
    while !stop.load(Ordering::Acquire) {
        let now = Instant::now();
        if now < next {
            thread::sleep((next - now).min(POLL));
            continue;
        }
        for (i, input) in inputs.iter_mut().enumerate() {
            match input {
                Input::Chunks(slot) => {
                    let Some(rx) = slot.as_ref() else { continue };
                    let mut disconnected = false;
                    loop {
                        match rx.try_recv() {
                            Ok(chunk) => mixer.push(i, &chunk.samples),
                            Err(TryRecvError::Empty) => break,
                            Err(TryRecvError::Disconnected) => {
                                disconnected = true;
                                break;
                            }
                        }
                    }
                    if disconnected {
                        *slot = None;
                        if !stop.load(Ordering::Acquire) {
                            push_warning(
                                &warnings,
                                format!("{}: source stopped; recording silence", labels[i]),
                            );
                        }
                    }
                }
                Input::App(watcher) => {
                    for samples in watcher.pump(now) {
                        mixer.push(i, &samples);
                    }
                }
            }
        }
        let blocks = mixer.tick();
        for (i, ((out, samples), live)) in outs.iter().zip(blocks).zip(alive.iter_mut()).enumerate()
        {
            if *live {
                levels.record(i, &samples);
                if out.send(samples).is_err() {
                    *live = false;
                }
            }
        }
        if alive.iter().all(|a| !a) {
            break;
        }
        next += block;
        let after = Instant::now();
        if after.saturating_duration_since(next) > RESYNC {
            next = after;
        }
    }
    for input in inputs.iter_mut() {
        if let Input::App(watcher) = input {
            watcher.stop();
        }
    }
}

fn push_warning(warnings: &Mutex<Vec<String>>, message: String) {
    lock(warnings).push(message);
}
