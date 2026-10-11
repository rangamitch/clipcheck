//! clipcheck-audio: WASAPI capture sources.
//!
//! Each source runs on its own capture thread and delivers [`AudioChunk`]s
//! (interleaved stereo `f32`, 48 kHz, QPC-based timestamps in 100 ns units)
//! over an `mpsc` channel. When a source produces no data for 20 ms, a silent
//! chunk of 20 ms is emitted so downstream timelines never stall.

pub mod clip;
pub mod devices;
pub mod format;
pub mod meter;
pub mod sessions;

#[cfg(windows)]
mod loopback;
#[cfg(windows)]
mod mic;

#[cfg(windows)]
pub use loopback::{is_supported, os_build, ProcessLoopback, SystemMix};
#[cfg(windows)]
pub use mic::Microphone;

pub use devices::{list_endpoints, sort_endpoints, Endpoint, EndpointKind};
#[cfg(windows)]
pub use meter::MicMeter;
pub use meter::{peak_db, rms_db, MeterReading};
pub use sessions::{list_sessions, AudioSessionInfo, SeenApp, SeenApps, SessionState};

#[cfg(windows)]
pub use clip::record_mic_clip;
pub use clip::{downmix_mono, encode_wav_mono16, to_i16, MicClip, MIC_TEST_SECONDS};

pub use format::{InputFormat, OUT_CHANNELS, OUT_RATE};

/// One block of normalised audio from a capture source.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioChunk {
    /// Interleaved stereo samples (`L, R, L, R, ...`) at [`OUT_RATE`].
    pub samples: Vec<f32>,
    /// QPC timestamp of the first frame, in 100 ns units.
    pub pts_100ns: i64,
    /// True when the samples are silence (WASAPI silent flag, or synthetic fill).
    pub silent: bool,
}

/// Which process tree a process loopback capture includes or excludes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopbackMode {
    /// Capture only the target process and its children.
    IncludeTree,
    /// Capture everything except the target process and its children.
    ExcludeTree,
}

/// Errors from starting or running a capture source.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AudioError {
    /// The feature is not available on this OS or configuration.
    #[error("unsupported: {0}")]
    Unsupported(&'static str),
    /// No matching capture or render endpoint exists.
    #[error("no matching audio endpoint")]
    NoDevice,
    /// The device stream format cannot be converted.
    #[error("unsupported audio format: {0}")]
    Format(&'static str),
    /// A Windows API call failed.
    #[error("{context} failed (HRESULT 0x{hresult:08X})")]
    Win {
        /// Name of the failing call.
        context: &'static str,
        /// HRESULT returned by the call.
        hresult: u32,
    },
    /// The capture or meter thread could not be created.
    #[error("could not start capture thread")]
    ThreadSpawn,
    /// The capture thread ended before it reported that setup had finished
    /// (for example, it panicked during setup).
    #[error("capture thread exited during setup")]
    ThreadExited,
    /// A fixed-length capture ended (deadline or stream end) before enough audio arrived.
    #[error("no audio arrived from the capture endpoint in time")]
    NoAudio,
}

/// Output chunk length used for synthetic silence: 20 ms.
pub(crate) const SILENCE_MS: u64 = 20;
/// Frames in one synthetic silence chunk.
pub(crate) const SILENCE_FRAMES: usize = OUT_RATE as usize * SILENCE_MS as usize / 1000;

#[cfg(windows)]
mod engine {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_FAILED};
    use windows::Win32::Media::Audio::{
        IAudioCaptureClient, IAudioClient, AUDCLNT_BUFFERFLAGS_SILENT,
    };
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
    use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
    use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

    use super::format::{InputFormat, Normalizer, OUT_CHANNELS, OUT_RATE};
    use super::{AudioChunk, AudioError, SILENCE_FRAMES, SILENCE_MS};

    /// Buffer duration requested from WASAPI: 200 ms, in 100 ns units.
    pub(crate) const BUFFER_HNS: i64 = 2_000_000;

    /// HRESULT_FROM_WIN32(ERROR_NOT_FOUND): no such endpoint.
    const E_NOT_FOUND: u32 = 0x8007_0490;

    /// Maps a windows-crate error to [`AudioError`], labelling it with `context`.
    pub(crate) fn win_err(context: &'static str) -> impl Fn(windows::core::Error) -> AudioError {
        move |e| {
            let hresult = e.code().0 as u32;
            if hresult == E_NOT_FOUND {
                AudioError::NoDevice
            } else {
                AudioError::Win { context, hresult }
            }
        }
    }

    /// An initialised, not-yet-started WASAPI capture client and its event.
    /// Owned by a single capture thread; never sent across threads.
    pub(crate) struct Session {
        pub(crate) client: IAudioClient,
        pub(crate) capture: IAudioCaptureClient,
        pub(crate) event: HANDLE,
        pub(crate) format: InputFormat,
    }

    impl Drop for Session {
        fn drop(&mut self) {
            // SAFETY: the client was initialised by us; Stop is harmless if it
            // never started, and the event handle was created by us and is closed once.
            unsafe {
                let _ = self.client.Stop();
                let _ = CloseHandle(self.event);
            }
        }
    }

    /// Wraps an initialised client: creates the event, binds it, and fetches
    /// the capture service. Must be called on the thread that owns the session.
    pub(crate) fn finish_session(
        client: IAudioClient,
        format: InputFormat,
    ) -> Result<Session, AudioError> {
        // SAFETY: client is initialised; the event is closed by Session::drop or on the error path.
        unsafe {
            let event = CreateEventW(None, false, false, PCWSTR::null())
                .map_err(win_err("CreateEventW"))?;
            if let Err(e) = client.SetEventHandle(event) {
                let _ = CloseHandle(event);
                return Err(win_err("IAudioClient::SetEventHandle")(e));
            }
            let capture: IAudioCaptureClient = match client.GetService() {
                Ok(c) => c,
                Err(e) => {
                    let _ = CloseHandle(event);
                    return Err(win_err("IAudioClient::GetService")(e));
                }
            };
            Ok(Session {
                client,
                capture,
                event,
                format,
            })
        }
    }

    /// QPC frequency in counts per second. Fixed after boot, so it is queried
    /// once per process and cached; a failed query caches 0 (as the uncached
    /// version would return on every call).
    fn qpc_frequency() -> i64 {
        static FREQ: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
        *FREQ.get_or_init(|| {
            let mut freq = 0i64;
            // SAFETY: the call writes into a valid, live i64 local.
            unsafe {
                let _ = QueryPerformanceFrequency(&mut freq);
            }
            freq
        })
    }

    /// Current QPC time in 100 ns units.
    pub(crate) fn now_100ns() -> i64 {
        let mut count = 0i64;
        // SAFETY: the call writes into a valid, live i64 local.
        unsafe {
            let _ = QueryPerformanceCounter(&mut count);
        }
        let freq = qpc_frequency();
        if freq <= 0 {
            return 0;
        }
        ((count as i128) * 10_000_000 / freq as i128) as i64
    }

    /// Handle to a running capture thread. Stopping joins the thread.
    pub(crate) struct CaptureHandle {
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
        /// Error that ended the capture thread, if it ended because of a failure.
        /// Written by the thread before it drops the chunk sender, so a consumer
        /// that sees the channel disconnect can read it.
        error: Arc<Mutex<Option<AudioError>>>,
    }

    impl CaptureHandle {
        /// Signals the capture thread to finish and waits for it.
        pub(crate) fn stop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }

        /// The error that ended capture, or `None` if capture is still running or was stopped normally.
        pub(crate) fn error(&self) -> Option<AudioError> {
            match self.error.lock() {
                Ok(g) => g.clone(),
                Err(poisoned) => poisoned.into_inner().clone(),
            }
        }
    }

    impl Drop for CaptureHandle {
        fn drop(&mut self) {
            self.stop();
        }
    }

    /// Starts a capture thread. `open` runs on that thread (COM is initialised
    /// there as MTA) and returns the session; setup errors are returned here.
    pub(crate) fn spawn_capture<F>(
        name: &'static str,
        open: F,
    ) -> Result<(CaptureHandle, Receiver<AudioChunk>), AudioError>
    where
        F: FnOnce() -> Result<Session, AudioError> + Send + 'static,
    {
        let (tx, rx) = mpsc::channel::<AudioChunk>();
        let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<(), AudioError>>(1);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let error = Arc::new(Mutex::new(None::<AudioError>));
        let error_thread = Arc::clone(&error);

        let thread = std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                // SAFETY: MTA init on this thread; balanced by CoUninitialize below when it succeeded.
                let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
                let com_owned = com.is_ok();
                let outcome = match open() {
                    Err(e) => {
                        // Setup failure: reported to the spawning caller, not the chunk channel.
                        let _ = ready_tx.send(Err(e));
                        Ok(())
                    }
                    Ok(session) => {
                        let _ = ready_tx.send(Ok(()));
                        // `session` drops at the end of this arm, before COM is uninitialised.
                        run_loop(&session, &stop_thread, &tx)
                    }
                };
                if let Err(e) = outcome {
                    // Store the error before `tx` drops at the end of this closure, so a
                    // consumer that observes the disconnect can read it via `error()`.
                    let mut slot = match error_thread.lock() {
                        Ok(g) => g,
                        Err(p) => p.into_inner(),
                    };
                    *slot = Some(e);
                }
                if com_owned {
                    // SAFETY: matches the successful CoInitializeEx above.
                    unsafe { CoUninitialize() };
                }
            })
            .map_err(|_| AudioError::ThreadSpawn)?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok((
                CaptureHandle {
                    stop,
                    thread: Some(thread),
                    error,
                },
                rx,
            )),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = thread.join();
                Err(AudioError::ThreadExited)
            }
        }
    }

    /// Event-driven capture loop with 20 ms silence fill. Runs until `stop`
    /// is set or the receiver is dropped (both normal ends, returning `Ok`).
    /// A device or conversion failure returns the error, which the caller stores.
    fn run_loop(s: &Session, stop: &AtomicBool, tx: &Sender<AudioChunk>) -> Result<(), AudioError> {
        let mut norm = Normalizer::new(s.format, OUT_RATE)?;
        // SAFETY: the client is initialised and Start is called once, on the owning thread.
        unsafe { s.client.Start() }.map_err(win_err("IAudioClient::Start"))?;
        let silence_wait = Duration::from_millis(SILENCE_MS);
        let mut last_data = Instant::now();
        while !stop.load(Ordering::Relaxed) {
            // SAFETY: the event handle is owned by the session and valid for its lifetime.
            let wait = unsafe { WaitForSingleObject(s.event, SILENCE_MS as u32) };
            if wait == WAIT_FAILED {
                return Err(AudioError::Win {
                    context: "WaitForSingleObject",
                    hresult: windows::core::Error::from_win32().code().0 as u32,
                });
            }
            let got = match drain(s, &mut norm, tx)? {
                Some(g) => g,
                None => return Ok(()), // receiver dropped: normal stop
            };
            if got {
                last_data = Instant::now();
            } else if last_data.elapsed() >= silence_wait {
                let chunk = AudioChunk {
                    samples: vec![0.0; SILENCE_FRAMES * OUT_CHANNELS],
                    pts_100ns: now_100ns(),
                    silent: true,
                };
                if tx.send(chunk).is_err() {
                    return Ok(());
                }
                last_data = Instant::now();
            }
        }
        Ok(())
    }

    /// Drains all pending packets. Returns `Ok(Some(true))` if any frames arrived,
    /// `Ok(Some(false))` if none did, `Ok(None)` if the receiver was dropped, and
    /// `Err` with the failing WASAPI call on device error.
    fn drain(
        s: &Session,
        norm: &mut Normalizer,
        tx: &Sender<AudioChunk>,
    ) -> Result<Option<bool>, AudioError> {
        let frame_bytes = s.format.frame_bytes();
        let mut any = false;
        loop {
            // SAFETY: the capture client belongs to this session on this thread.
            let pending = unsafe { s.capture.GetNextPacketSize() }
                .map_err(win_err("IAudioCaptureClient::GetNextPacketSize"))?;
            if pending == 0 {
                return Ok(Some(any));
            }

            let mut data: *mut u8 = std::ptr::null_mut();
            let mut frames = 0u32;
            let mut flags = 0u32;
            let mut qpc = 0u64;
            // SAFETY: all out-pointers reference valid locals. On success `data` is valid
            // for `frames` frames until ReleaseBuffer is called below.
            unsafe {
                s.capture.GetBuffer(
                    &mut data,
                    &mut frames,
                    &mut flags,
                    None,
                    Some(&mut qpc as *mut u64),
                )
            }
            .map_err(win_err("IAudioCaptureClient::GetBuffer"))?;

            let silent = flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0;
            let raw_len = frames as usize * frame_bytes;
            let samples = if silent || data.is_null() || raw_len == 0 {
                // Silence in the format's own zero level (0x80 for unsigned 8-bit).
                norm.push_silence(frames as usize)
            } else {
                // SAFETY: WASAPI guarantees `frames * block_align` readable bytes at `data`
                // until ReleaseBuffer, and we release only after copying out of it.
                let raw = unsafe { std::slice::from_raw_parts(data, raw_len) };
                norm.push(raw)
            };

            // SAFETY: releases exactly the frames obtained from GetBuffer above.
            let released = unsafe { s.capture.ReleaseBuffer(frames) };
            released.map_err(win_err("IAudioCaptureClient::ReleaseBuffer"))?;

            if frames > 0 {
                any = true;
            }
            if !samples.is_empty() {
                let pts_100ns = if qpc != 0 { qpc as i64 } else { now_100ns() };
                let chunk = AudioChunk {
                    samples,
                    pts_100ns,
                    silent,
                };
                if tx.send(chunk).is_err() {
                    return Ok(None);
                }
            }
        }
    }
}
