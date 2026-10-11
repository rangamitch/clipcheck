# 02 - Video Capture (`crates/capture`)

Status: draft for review. Owner: capture. Depends on: `core` (ids, errors, config schema), `detect` (target HWND), `encode` (consumes NV12 frames, owns hwframes pool), `session`/`buffer` (consume encoded packets), `ipc` (events and commands).

This doc elaborates `00-brief.md`. Where it is silent, decisions below are binding for this feature. Anything that contradicts the brief is listed under section 12.

---

## 1. Goal & scope

### 1.1 Goal

Produce a constant-frame-rate stream of GPU-resident NV12 frames for exactly one game window (identified by HWND), at a configurable fps and output size, with the cursor optionally included, HDR content converted to SDR, and continuous output while the window is visible, occluded, unfocused, resized, moved between monitors, or temporarily hidden.

### 1.2 In scope

| Area | Included |
|---|---|
| Source | Windows Graphics Capture (WGC) item created from one HWND. Never a monitor or desktop item. |
| GPU | D3D11 device creation and adapter selection, free-threaded frame pool, owned raw texture, device-lost recovery |
| Pacing | Fixed-fps tick generator, duplication of last frame, dropping of surplus source frames |
| Conversion | GPU scale (letterbox into fixed canvas) and BGRA/FP16 to NV12 (BT.709 limited) via pixel shader |
| Cursor | WGC cursor capture on/off |
| HDR | FP16 frame pool on HDR monitors, tone map to SDR in the conversion shader |
| Window changes | Resize, DPI change, monitor move, minimize/restore, hide/show, cloak (other virtual desktop) |
| Multi-GPU | Hybrid laptop adapter selection, cross-adapter policy, encoder-adapter constraint |
| Black frames | Emitted at target fps while the window is hidden; resume on return |
| Focus | Visible-but-unfocused keeps recording |
| Exclusive fullscreen | Detection heuristic, user warning, documented limitation, black fallback |
| v2 | Game-hook design sketch only, section 5.15 (not built) |
| Timing | QPC timestamps on every video frame, shared time base with audio |

### 1.3 Out of scope

| Area | Owner |
|---|---|
| Finding the game window, exe, pid, game detection | `detect` |
| NVENC/AMF/QSV/x264 selection, rate control, encoder hwframes pool creation policy | `encode` (this doc only states the constraints it must satisfy) |
| Audio capture, WASAPI, mixing | `audio` |
| Segment ring buffer, session files, split logic | `buffer`, `session` |
| Desktop / monitor capture, region capture | Not planned. Forbidden by brief. Exception: the opt-in no-border source (5.16) duplicates a monitor but only emits the crop of the game window. |
| Software cursor overlay, cursor zoom, click highlights | Post-v1 |
| Game hook (DLL injection) implementation | v2 (sketch in 5.15 only) |
| Preset-to-setting mapping (Low/Medium/High/Ultra) | `settings` doc; this doc owns the individual keys it maps onto |

---

## 2. User-facing behavior

All behavior below is observable in the output file or in the UI status line.

| Situation | Output | Notes |
|---|---|---|
| Game recording, window visible and focused | Live frames at target fps | Normal path |
| Window visible but another app has focus | Live frames at target fps | Brief requirement. Game may itself throttle rendering when unfocused (game-side behavior); those frames are then duplicated by the pacer. |
| Window covered by other windows (occluded) | Live frames | WGC captures the window's own surface, so other apps never appear. |
| Window minimized | Black frames at target fps after `hidden_grace_ms` | Black frames are full canvas NV12 black (Y=16, UV=128). |
| Window hidden (`ShowWindow(SW_HIDE)`) | Black frames | Same as minimized. |
| Window on another virtual desktop (cloaked) | Black frames | Detected with `DWMWA_CLOAKED`. |
| Window restored / shown | Live frames resume; first live frame is preceded by one IDR keyframe if `keyframe_on_resume = true` | Resume is at the next tick, no catch-up burst. |
| Static window content (paused menu, loading screen) | Duplicated last frame at target fps | Not black. Static is not hidden. |
| Window resized | Content letterboxed or pillarboxed inside the fixed output canvas | Output resolution does not change mid-part. Canvas size is fixed at session start (see 5.5). |
| Window moved to a monitor with different DPI | Same as resize; content size changes in physical pixels | Output canvas unchanged. |
| Window moved to an HDR monitor, or from HDR to SDR | Pool format switches (FP16 or 8-bit); output stays SDR NV12 | Brief: HDR to SDR tone map. Switch takes effect within 250 ms. |
| Cursor option on | Cursor composited by WGC into frames | Only the cursor over the captured window appears. |
| Cursor option off | No cursor in frames | |
| Exclusive fullscreen game | Likely black frames after warning | Section 5.10. UI shows a persistent banner "Exclusive fullscreen: switch the game to borderless windowed". |
| Target window closed | Black frames, capture session ends, session layer notified | Session layer decides whether to restart with a new target. |
| GPU device reset (TDR) | Brief gap (<= 2 s) of black frames, then live | Recovery in 5.8. If recovery fails, reason `DeviceLost` and black frames. |

---

## 3. Architecture & data flow

### 3.1 Threads and flow

```
                       +---------------------------------------------+
  WGC free-threaded    |  Ingest (WGC callback, threadpool thread)   |
  pool (2-3 buffers) ->|  TryGetNextFrame -> CopyResource into       |
  FrameArrived         |  raw_tex (owned, content size) -> Close()   |
                       |  writes LatestFrame { seq, qpc_arrival }    |
                       +------------------+--------------------------+
                                          | atomic seq + raw_tex (GPU order)
                       +------------------v--------------------------+
  Pacer thread         |  Pacer (MMCSS "Games", waitable timer)      |
  (one per session)    |  tick k at t0 + k/fps                       |
                       |  window probe (4 Hz)                        |
                       |  state machine: Live/Hidden/Recovering      |
                       |  Live tick: convert raw -> target NV12      |
                       |  Dup/Black tick: copy last/black -> target  |
                       +------------------+--------------------------+
                                          | GpuFrame { NV12 tex, pts k, qpc }
                       +------------------v--------------------------+
                       |  FrameSink (encode crate)                   |
                       |  acquire() target from hwframes pool        |
                       +---------------------------------------------+
```

- Ingest runs on a WGC-owned threadpool thread. It does one GPU copy and returns the WGC buffer. It never converts.
- The pacer thread is the only thread that calls `Convert`/`Copy` for output frames and the only thread that calls `FrameSink::acquire`.
- Immediate context access is serialized by a `parking_lot::Mutex<GpuContext>` wrapping `ID3D11DeviceContext`. The device is also created with `ID3D11Multithread::SetMultithreadProtected(TRUE)` as a safety net.
- Ordering: because all GPU commands go through one immediate context, a copy submitted by ingest after a convert submitted by the pacer executes after it. This is why one `raw_tex` is sufficient (no ring for raw frames).

### 3.2 Crate layout

```
crates/capture/
  Cargo.toml
  src/
    lib.rs          public API: CaptureSession, CaptureConfig, FrameSink, GpuFrame
    config.rs       CaptureConfig + validation (maps 1:1 to section 6 keys)
    device.rs       adapter selection, D3D11 device, device-lost detection, DXGI output lookup
    ingest.rs       GraphicsCaptureItem, frame pool, FrameArrived, raw_tex, resize Recreate
    pacer.rs        tick loop, timer, state machine, output selection
    convert.rs      D3DCompile, PSO/RTV/SRV setup, constant buffers, pass dispatch
    shaders/
      convert.hlsl  Y pass, UV pass, HDR tone map, letterbox
    probe.rs        window state (IsIconic, IsWindowVisible, DWMWA_CLOAKED, rect), HDR, DPI
    clock.rs        QueryPerformanceCounter helpers, tick math (i64 ticks)
    stats.rs        counters and 1 s rolling stats
    error.rs        CaptureError enum
    fault.rs        (cfg(test)) fault injection hooks for device-lost and window state
  tests/
    golden/         reference NV12 outputs for shader tests
```

### 3.3 Cargo features (windows crate)

The `windows` crate version is pinned at implementation time to the latest 0.6x release.

```toml
[dependencies]
windows = { version = "0.6x", features = [
  "Foundation",
  "Graphics_Capture",
  "Graphics_DirectX",
  "Graphics_DirectX_Direct3D11",
  "Win32_Foundation",
  "Win32_Graphics_Direct3D",
  "Win32_Graphics_Direct3D11",
  "Win32_Graphics_Direct3D_Fxc",
  "Win32_Graphics_Dwm",
  "Win32_Graphics_Dxgi",
  "Win32_Graphics_Dxgi_Common",
  "Win32_Graphics_Gdi",
  "Win32_System_Performance",
  "Win32_System_Threading",
  "Win32_System_WinRT",
  "Win32_System_WinRT_Direct3D11",
  "Win32_System_WinRT_Graphics_Capture",
  "Win32_UI_WindowsAndMessaging",
  "Win32_Media",
] }
parking_lot = "0.12"
crossbeam-channel = "0.5"
thiserror = "1"
tracing = "0.1"
```

Runtime requirement: Windows 10 build 19041 (2004) or newer. On older builds `CaptureSession::start` returns `CaptureError::Unsupported`. Border and cursor options are feature-detected by `ApiInformation::IsPropertyPresent` (see 5.13).

---

## 4. Key types, traits and function signatures

### 4.1 Public API (`lib.rs`)

```rust
use std::time::Duration;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;

/// Opaque handle for one captured window. Copied into the session; the
/// session re-validates the HWND on every probe tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowTarget {
    pub hwnd: isize,       // HWND stored as isize so the type is Send
    pub pid: u32,
    pub exe_hash: u64,     // for logs, not for matching
}

pub struct CaptureSession { /* private */ }

impl CaptureSession {
    /// Creates device, item, frame pool and starts the pacer thread.
    /// Blocks until the first FrameSink::acquire succeeds or `start_timeout`
    /// elapses. On timeout the session still starts in Hidden/Starting and
    /// emits black frames; it does not fail.
    pub fn start<F>(
        target: WindowTarget,
        cfg: CaptureConfig,
        sink_factory: F,               // called once with the capture device (4.3)
        clock: ClockRef,
    ) -> Result<Self, CaptureError>
    where
        F: FnOnce(Arc<ID3D11Device>) -> Result<Box<dyn FrameSink>, SinkError> + Send + 'static;

    /// Stops the pacer, closes the WGC session, releases the device.
    /// Emits the final black or live frames for the last partial tick
    /// only if the caller requested `flush = true`.
    pub fn stop(self, flush: bool) -> CaptureSummary;

    /// Applies a config change. Fps and output size changes are rejected
    /// (return `CaptureError::RestartRequired`); the caller must split the
    /// session. Cursor, HDR, border, timing and stall parameters apply live.
    pub fn reconfigure(&self, cfg: CaptureConfig) -> Result<(), CaptureError>;

    /// Forces the next emitted frame to be an IDR (used by split and resume).
    pub fn request_keyframe(&self);

    pub fn stats(&self) -> CaptureStats;

    /// QPC tick of tick index 0. Shared with audio and the session writer.
    pub fn base_qpc(&self) -> i64;
    pub fn qpc_freq(&self) -> i64;
}

#[derive(Clone, Copy, Debug)]
pub struct CaptureSummary {
    pub frames_live: u64,
    pub frames_dup: u64,
    pub frames_black: u64,
    pub src_frames_dropped: u64,     // WGC arrivals not consumed by a tick
    pub enc_overrun_drops: u64,      // ticks where acquire() failed
    pub last_pts: i64,
}
```

### 4.2 Config (`config.rs`)

```rust
#[derive(Clone, Debug, PartialEq)]
pub struct CaptureConfig {
    pub fps: u32,                        // 30 | 60 | 120
    pub output: OutputSize,              // Native | Height(u32) with 1440|1080|720
    pub cursor: bool,
    pub hdr_tonemap: HdrMode,            // Auto | Off
    pub sdr_white_nits: u32,             // 80..=400
    pub adapter: AdapterPref,            // Auto | HighPerformance | Monitor
    pub border: BorderMode,              // Auto | On | Off  (Win11 only)
    pub hidden_grace: Duration,          // 0..=5 s
    pub stall_restart: Duration,         // 1..=30 s
    pub resize_debounce: Duration,       // 0..=1 s
    pub pool_buffers: u8,                // 2..=3
    pub keyframe_on_resume: bool,
    pub video_offset_ms: i32,            // -500..=500
    pub exclusive_warn: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputSize { Native, Height(u32) }   // Height: 1440 | 1080 | 720

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HdrMode { Auto, Off }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdapterPref { Auto, HighPerformance, Monitor }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BorderMode { Auto, On, Off }

impl CaptureConfig {
    pub fn validate(&self) -> Result<(), CaptureError>;
}
```

### 4.3 Frame sink (consumed by `encode`)

```rust
use std::sync::Arc;

/// A frame ready for the encoder. The texture is an NV12 texture acquired
/// from the encoder's hwframes pool. Ownership transfers to the sink on push.
pub struct GpuFrame {
    pub tex: ID3D11Texture2D,        // NV12, bound RENDER_TARGET | SHADER_RESOURCE
    pub subresource: u32,            // array slice index in the hwframes pool
    pub index: i64,                  // tick index k, time base 1/fps
    pub pts_qpc: i64,                // nominal QPC of this tick
    pub kind: FrameKind,
    pub width: u32,                  // canvas width (even)
    pub height: u32,                 // canvas height (even)
    pub hdr_source: bool,            // informational (overlay, logs)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameKind { Live, Duplicate, Black }

/// Implemented by the encode crate. Must not block for long: the pacer
/// calls these from the real-time thread.
pub trait FrameSink: Send {
    /// Returns a free NV12 target, or None if the encoder queue is full.
    /// A None is counted as enc_overrun_drops and the tick is skipped.
    fn acquire(&mut self) -> Option<AcquiredTarget>;
    /// Hands a filled target to the encoder.
    fn push(&mut self, frame: GpuFrame) -> Result<(), SinkError>;
    /// The D3D11 device the hwframes pool lives on. Capture must use it.
    fn d3d11_device(&self) -> Arc<ID3D11Device>;
    /// Adapter LUID of that device; used by encode to restrict encoder vendor.
    fn adapter_luid(&self) -> i64;
}

pub struct AcquiredTarget {
    pub tex: ID3D11Texture2D,
    pub subresource: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum SinkError {
    #[error("encoder closed")]
    Closed,
    #[error("device removed")]
    DeviceRemoved,
}
```

Design note: capture creates the D3D11 device first (adapter choice in 5.7), then calls `sink_factory(device)`. `encode` builds its hwframes context on that device inside the factory. This ordering lets capture and the encoder share one device, which avoids a cross-adapter copy per frame. The factory is called again after device-lost recovery (5.8) with the new device.

### 4.4 Clock (`clock.rs`)

```rust
/// All video timing is in QPC ticks (i64). Never use Instant for pts.
#[derive(Clone, Copy, Debug)]
pub struct QpcClock { pub freq: i64 }

pub trait Clock: Send + Sync {
    fn now(&self) -> i64;          // QPC ticks
    fn freq(&self) -> i64;
}
pub type ClockRef = Arc<dyn Clock>;

/// Nominal QPC of tick k: base + round(k * freq / fps), computed in i128.
pub fn tick_qpc(base: i64, freq: i64, fps: u32, k: i64) -> i64;
```

Tests use a `FakeClock` implementing `Clock`.

### 4.5 Window probe (`probe.rs`)

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowState {
    Presentable,      // visible, not iconic, not cloaked, non-zero client rect
    Minimized,        // IsIconic
    Hidden,           // !IsWindowVisible
    Cloaked,          // DWMWA_CLOAKED != 0 (other virtual desktop, or DWM-cloaked)
    Gone,             // IsWindow == false, or client rect empty while visible
}

pub struct ProbeResult {
    pub state: WindowState,
    pub client_px: (u32, u32),
    pub monitor: isize,            // HMONITOR from MonitorFromWindow(MONITOR_DEFAULTTONEAREST)
    pub hdr_active: bool,          // from DXGI output of that monitor
    pub maybe_exclusive: bool,     // heuristic, see 5.10
}

pub trait WindowProbe: Send {
    fn probe(&mut self) -> ProbeResult;
}
```

### 4.6 Ingest (`ingest.rs`)

```rust
pub(crate) struct Ingest {
    item: GraphicsCaptureItem,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    arrived_token: EventRegistrationToken,
    closed_token: EventRegistrationToken,
    latest: Arc<LatestFrame>,
    pool_size: SizeInt32,              // current pool/content size
    pool_format: DirectXPixelFormat,   // B8G8R8A8UIntNormalized | R16G16B16A16Float
}

/// Written by the ingest callback, read by the pacer.
/// `raw_tex` is only touched while holding the GPU context mutex.
pub(crate) struct LatestFrame {
    pub seq: AtomicU64,                // incremented after each copy
    pub qpc_arrival: AtomicI64,        // QPC at FrameArrived
    pub system_rel_qpc: AtomicI64,     // SystemRelativeTime converted to QPC ticks
    pub content_w: AtomicU32,
    pub content_h: AtomicU32,
    pub resize_pending: AtomicBool,
    pub closed: AtomicBool,            // set by GraphicsCaptureItem.Closed
    pub error: AtomicU32,              // last HRESULT from copy/recreate
}

impl Ingest {
    pub(crate) fn start(device: &GpuDevice, target: HWND, cfg: &CaptureConfig,
                        latest: Arc<LatestFrame>) -> Result<Self, CaptureError>;
    pub(crate) fn recreate(&mut self, device: &GpuDevice, size: SizeInt32,
                           format: DirectXPixelFormat) -> Result<(), CaptureError>;
    pub(crate) fn set_cursor(&self, on: bool) -> Result<(), CaptureError>;
    pub(crate) fn set_border(&self, mode: BorderMode) -> Result<(), CaptureError>;
    pub(crate) fn stop(self);
}
```

### 4.7 Device (`device.rs`)

```rust
pub(crate) struct GpuDevice {
    pub d3d: ID3D11Device,
    pub ctx: parking_lot::Mutex<ID3D11DeviceContext>,
    pub winrt: IDirect3DDevice,        // CreateDirect3D11DeviceFromDXGIDevice
    pub luid: i64,
    pub adapter_desc: DXGI_ADAPTER_DESC1,
    pub supports_nv12_rtv: bool,       // CheckFormatSupport(NV12) & RENDER_TARGET
}

pub(crate) fn select_adapter(pref: AdapterPref, hwnd: HWND)
    -> Result<IDXGIAdapter1, CaptureError>;

pub(crate) fn create_device(adapter: &IDXGIAdapter1) -> Result<GpuDevice, CaptureError>;

pub(crate) fn is_device_removed(hr: windows::core::HRESULT) -> bool;
```

### 4.8 Pacer and convert (`pacer.rs`, `convert.rs`)

```rust
pub(crate) struct Pacer {
    cfg: CaptureConfig,
    base_qpc: i64,
    freq: i64,
    k: i64,                              // next tick index
    state: PipelineState,
    last_live: LastLive,                 // NV12 of last Live tick (capture-owned copy)
    black_nv12: ID3D11Texture2D,         // prefilled once per canvas size
    convert: Converter,
    latest: Arc<LatestFrame>,
    last_seq: u64,
    stats: stats::Counters,
}

pub(crate) enum PipelineState {
    Starting,
    Live,
    Hidden { since_qpc: i64, reason: HiddenReason },
    Recovering { attempt: u8, next_try_qpc: i64 },
    Lost { reason: LostReason },
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum HiddenReason { Minimized, Hidden, Cloaked, Gone, Exclusive }

#[derive(Clone, Copy, Debug)]
pub(crate) enum LostReason { DeviceRemoved, CaptureFailed, TargetGone, RecoveryExhausted }

pub(crate) struct Converter {
    vs: ID3D11VertexShader,
    ps_y: ID3D11PixelShader,
    ps_uv: ID3D11PixelShader,
    cb: ID3D11Buffer,                    // ConvertParams, 64 bytes
    samp: ID3D11SamplerState,            // linear clamp
    scratch: Option<ID3D11Texture2D>,    // intermediate BGRA for >2:1 downscale
}

// Pacer-owned NV12 texture holding the last Live output. Capture-owned
// (not an encoder pool target), so duplicates can be copied from it.
type LastLive = Option<ID3D11Texture2D>;

impl Converter {
    /// Records Y and UV passes from `raw_tex` (or `scratch`) into `dst`.
    /// `dst` is an NV12 texture; RTVs are created per plane per call
    /// (or cached per subresource).
    pub(crate) fn convert(&mut self, ctx: &mut ID3D11DeviceContext, src: &ID3D11ShaderResourceView,
                          dst: &AcquiredTarget, params: &ConvertParams) -> Result<(), CaptureError>;
}

#[repr(C)]
pub(crate) struct ConvertParams {
    pub src_rect: [f32; 4],      // source origin xy, size zw (content px)
    pub dst_rect: [f32; 4],      // letterbox rect in canvas px
    pub canvas: [f32; 4],        // canvas w, canvas h, hdr flag (1/0), sdr_scale
    pub _pad: [f32; 4],
}
```

### 4.9 Errors (`error.rs`)

```rust
#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("unsupported: {0}")]
    Unsupported(&'static str),                   // pre-2004 build, no WGC
    #[error("invalid config: {0}")]
    InvalidConfig(String),
    #[error("window not capturable: {0}")]
    WindowNotCapturable(&'static str),           // HWND invalid, protected, not top-level
    #[error("d3d11 device: {0}")]
    Device(windows::core::Error),
    #[error("device removed (reason {0:#x})")]
    DeviceRemoved(i32),
    #[error("wgc: {0}")]
    Wgc(windows::core::Error),
    #[error("restart required: {0}")]
    RestartRequired(&'static str),
    #[error("shader compile: {0}")]
    Shader(String),
    #[error(transparent)]
    Sink(#[from] SinkError),
}
```

---

## 5. Algorithms and state machines

### 5.1 Session start

```
START(target, cfg, sink_factory, clock)
 1  validate cfg (fps in {30,60,120}, output height in {1440,1080,720} or Native)
 2  probe = WindowProbe(target.hwnd)
    if probe.state == Gone            -> return WindowNotCapturable("gone")
 3  if !is_win_ge(19041)               -> return Unsupported
 4  adapter = select_adapter(cfg.adapter, hwnd)          (5.7)
 5  dev = create_device(adapter)  (BGRA_SUPPORT | VIDEO_SUPPORT, FL 11_0)
 6  sink = sink_factory(dev.d3d)      // encode builds hwframes on dev, checks luid
    if sink.adapter_luid() != dev.luid -> return Device("sink on other adapter")
 7  canvas = if Native: even_round(probe.client_px) else height_to_canvas(H, aspect=probe.client_px)
    NOTE: Native uses the first content size seen; if not yet known, wait up to
          1 s for the first frame; else use client_px.
 8  pool_format = if probe.hdr_active && cfg.hdr_tonemap == Auto then FP16 else BGRA8
 9  ingest = Ingest::start(dev, hwnd, cfg, latest)   // item, pool, session, cursor, border
10  raw_tex = CreateTexture2D(content size, pool_format, DEFAULT, SRV|...)
11  black_nv12 = create NV12 canvas texture; fill Y=16, UV=128 via one clear (see 5.7)
12  base_qpc = clock.now() rounded down to a whole tick index 0
13  spawn pacer thread: MMCSS "Games" (fallback: THREAD_PRIORITY_TIME_CRITICAL), CreateWaitableTimerExW(HIGH_RESOLUTION)
14  return session
```

### 5.2 Ingest callback (`FrameArrived`, threadpool thread)

Must complete in well under 1 ms. No allocation, no logging above debug.

```
ON_FRAME_ARRIVED(pool, _)
 1  frame = pool.TryGetNextFrame()        // None: return (spurious)
 2  qpc_now = QueryPerformanceCounter()
 3  size = frame.ContentSize()
 4  if size != ingest.pool_size:
        latest.resize_pending = true       // pacer will Recreate after debounce
        latest.content_w/h = size          // do not copy this frame; it may be mis-sized
        frame.Close(); return
 5  lock ctx
 6      ctx.CopySubresourceRegion(raw_tex, 0, 0, 0, 0, frame.surface_tex, 0, NULL)
 7  unlock
 8  frame.Close()                          // returns buffer to pool immediately
 9  latest.qpc_arrival = qpc_now
    latest.system_rel_qpc = FromSystemRelativeTime(frame.SystemRelativeTime())   // see 5.9
10  latest.seq += 1  (Release store)
```

Notes:
- `CopySubresourceRegion` is preferred over `CopyResource` when the content box is known to match, because it avoids driver validation for mismatched sizes in debug builds.
- If `frame.Close()` is skipped by an error path, the pool stalls after `pool_buffers` frames. Every path must close. Wrap with a guard struct whose `Drop` calls `Close`.
- Copies from a surface on another adapter (hybrid laptops) are performed by WGC before the frame is delivered to our device (5.7). Ingest sees same-device textures only.

### 5.3 Pacer tick (the real-time loop)

Runs once per tick. `k` is the tick index. Tick times are `T_k = tick_qpc(base, freq, fps, k)`.

```
PACER_LOOP
 0  set MMCSS; create waitable timer
 1  loop:
 2     WAIT_UNTIL(T_k)                        // timer due = T_k - now (100 ns units, negative = relative)
 3     now = QPC()
 4     if now - T_k > period_ticks:           // lagging by > 1 frame
           k = tick_index_at(now)             // floor((now - base)*fps/freq)
           stats.skipped_ticks += (k - old_k)
           T_k = tick_qpc(k)
 5     if k % probe_every == 0 (250 ms):      // probe at 4 Hz
           p = probe.probe(); transition(p)  (5.4)
 6     new = latest.seq != last_seq
       if new and resize_pending and now - resize_seen >= debounce: do_resize()  (5.5)
 7     target = acquire()                     // None -> stats.enc_overrun++; k++; continue
 8     match state:
         Live:
            if new:  convert(raw_tex -> target); last_seq = latest.seq
                     copy target -> last_live (kept as ring of 1; see note)
                     kind = Live; stats.live++; stats.src_dropped += (seq delta - 1)
            else:    if last_live: copy last_live -> target; kind = Duplicate; stats.dup++
                     else: copy black -> target; kind = Black
         Hidden:   copy black_nv12 -> target; kind = Black; stats.black++
         Recovering: copy black_nv12 -> target; kind = Black; stats.black++
         Lost:     copy black_nv12 -> target; kind = Black; stats.black++
         Starting: copy black_nv12 -> target; kind = Black; stats.black++
 9     gpu_flush_marker()  (ctx.Flush(), not Wait)
10     sink.push(GpuFrame { tex: target, index: k, pts_qpc: T_k, kind, ... })
11     k += 1
```

Notes:
- Steps 8 Live "copy target -> last_live": this copy is what allows duplication after the source raw frame changes. Keeping the converted NV12 means duplicates are a pure NV12 copy, not a re-convert. Cost at 1080p: about 3 MB GPU copy per dup tick.
- `probe_every` is 250 ms of ticks: `fps/4` ticks.
- `acquire()` before deciding the kind keeps the target pool balanced even when the frame is black.
- The pacer never blocks on the encoder. Overrun is a drop; the muxer uses true `pts` (k), so the gap appears as a short freeze.
- `period_ticks = freq / fps` (rounded).

### 5.4 Pipeline state machine (window and capture health)

```
                   +-----------+
     start ------>| Starting  |---- first new frame ----+
                   +-----+-----+                        |
                         | probe says Minimized/Hidden  |
                         | /Cloaked/Gone (after grace)  |
                         v                              v
                   +-----------+                  +-----------+
   +-------------->|  Hidden   |<---hidden--------|   Live    |<--+
   |  (black)      +-----+-----+  (after grace)   +-----+-----+   |
   |                     |                          |     |        |
   |  visible + frame    |                 frame    |     | no frame|
   |  arrives            v                 (dup)    |     | 3 s and |
   |               +-----------+                    |     | visible |
   +---------------|   Live    |<-------------------+     v         |
                   +-----------+                  +------------+   |
                                                  | Restarting |---+ ok
                                                  | (session)  |
                                                  +-----+------+
                                                        | error / backoff exhausted
                                                        v
                                                  +-----------+
                                                  |   Lost    |  black; reason;
                                                  | (terminal)|  session layer notified
                                                  +-----------+
```

Transition rules:

| From | To | Condition | Action |
|---|---|---|---|
| Starting | Live | first ingest frame copied | none |
| Starting | Hidden | probe not Presentable for > `hidden_grace` | none |
| Live | Hidden | probe not Presentable continuously for `hidden_grace` | record `since_qpc`, reason |
| Hidden | Live | probe Presentable AND `latest.seq` advanced | if `keyframe_on_resume`: `request_keyframe()` |
| Live | Restarting | visible, no new frame for `stall_restart` (default 3 s) | recreate WGC session (not the pool); count attempt |
| Restarting | Live | new frame within 1 s | none |
| Restarting | Lost | 3 restarts in 30 s without a frame | reason `CaptureFailed` |
| Live/Hidden | Recovering | `DXGI_ERROR_DEVICE_REMOVED` / `ItemClosed` with window still alive | 5.8 |
| Recovering | Live | device rebuilt and first frame copied | keyframe requested |
| Recovering | Lost | 3 attempts (1 s, 2 s, 4 s backoff) fail | reason `DeviceLost` |
| Any | Lost | probe Gone for > 2 s | reason `TargetGone`; sink notified via `Lost` event |

Important: "no frame for 3 s while visible" is ambiguous with a static screen (see 12, Q2). The Restarting transition is a probe, not a black trigger. Black is only produced by Hidden or Lost. A visible but stalled window stays as duplicates until the restart budget is spent, and then goes to Lost.

### 5.5 Resize and DPI change

Output canvas is fixed for the session part. Only the source-to-canvas mapping changes.

```
ON_RESIZE_SEEN (from ingest step 4 or probe client_px change)
  resize_seen = now
  latest.resize_pending = true

PACER (each tick, step 6)
  if resize_pending and now - resize_seen >= resize_debounce (default 150 ms):
     size = latest.content_w/h (or probe.client_px if content not reported)
     lock ctx
        ingest.recreate(dev, size, pool_format)     // pool.Recreate(...)
        raw_tex = recreate at size                  // old raw_tex released after ticks
     unlock
     latest.resize_pending = false
     stats.resizes++; emit CaptureFormatChanged (IPC)

  letterbox params each Live tick:
     scale = min(canvas_w / src_w, canvas_h / src_h)
     dst_w = round_even(src_w * scale); dst_h = round_even(src_h * scale)
     dst_x = (canvas_w - dst_w) / 2 ; dst_y = (canvas_h - dst_h) / 2
```

Behavior during the debounce window: the last NV12 (`last_live`) duplicates. During a continuous drag, the debounce keeps extending until resize stops; if a drag lasts > 2 s the pacer forces a recreate every 500 ms so the picture keeps updating. Canvas remains fixed; there is no encoder reconfigure.

Native mode canvas: the canvas is the even-rounded content size at start. If the window grows later, it is downscaled to fit (letterboxed). If it shrinks, it is letterboxed with black bars. The UI shows "Output locked to WxH; window resized" once per session.

DPI change: the window's physical pixel size changes, which surfaces as a resize (step 4). The app is declared per-monitor DPI aware v2 in its manifest so `GetClientRect` and the probe report physical pixels. No special DPI code path is needed beyond that.

### 5.6 HDR detection and format choice

```
HDR_ACTIVE(hmon):
  for each IDXGIOutput6 o on the adapter that owns hmon (match via DXGI_OUTPUT_DESC.Monitor):
      desc1 = o.GetDesc1()
      return desc1.ColorSpace in {
          DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709     // scRGB linear (Windows HDR)
          // DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020  listed for diagnostics only; see 12 Q3
      }
  return false
```

Re-evaluated on every probe tick (4 Hz). A change in `hdr_active` (or in the monitor) triggers `pool_format` switch:

```
if new_format != pool_format:
   ingest.recreate(dev, current_size, new_format)      // Recreate with new format
   raw_tex = recreate with new format
   converter.set_hdr(new_format == FP16 && cfg.hdr_tonemap == Auto)
```

The NV12 output and the encoder are unaffected by a format switch.

### 5.7 Adapter selection, multi-GPU and cross-adapter policy

Hybrid laptops (Optimus or AMD switchable) have an iGPU that drives the internal display and a dGPU where most games render. Two cases:

| Case | Where the game renders | Where the display is | Auto result |
|---|---|---|---|
| A. Desktop or MUX-enabled laptop | dGPU | dGPU | dGPU, no cross-adapter |
| B. Optimus laptop, internal display | dGPU | iGPU (DWM composes on iGPU, dGPU frames copied) | dGPU; WGC does the cross-adapter copy |
| C. Game forced onto iGPU | iGPU | iGPU | iGPU |

Rules:

```
select_adapter(pref, hwnd):
  Auto:            EnumAdapterByGpuPreference(HIGH_PERFORMANCE) (IDXGIFactory6)
                   -> if none found, fall back to Monitor
  HighPerformance: same as Auto but no fallback to Monitor
  Monitor:         adapter that owns MonitorFromWindow(hwnd)
```

Why Auto prefers the dGPU: NVENC and AMF live on the dGPU. Keeping capture, conversion and encode on one adapter avoids a second cross-adapter copy per frame (about 8 MB per 1080p BGRA frame, 60 times per second).

Cross-adapter cost (case B) is paid inside WGC, not by us. It is measured in spike T1. If WGC cross-adapter cost exceeds 2 % of one core at 1080p60, `Monitor` mode is recommended in the UI for that laptop, and the diagnostic panel shows it.

Encoder constraint (contract with `encode`): the sink's `adapter_luid()` must equal the capture device's LUID. Encode must choose its encoder vendor from that adapter: NVIDIA device -> NVENC, AMD -> AMF, Intel -> QSV, software always allowed. An encoder on a different adapter is rejected, not silently copied.

Adapter change mid-session (for example, power cable plugged, GPU mode switch): DXGI reports device removed or the item is closed. Treated as device-lost (5.8). Adapter is re-selected during recovery, and if it changed, the encoder must be rebuilt, so the session layer splits the file (see section 12, Q5).

### 5.8 Device-lost recovery

```
ON_DEVICE_REMOVED (from copy, convert or Flush; hr = GetDeviceRemovedReason)
  state -> Recovering{attempt=0, next_try = now + 1s}
  latest.closed = true (ingest callbacks become no-ops)
  stop ingest (close session and pool; do not wait on threadpool for more than 100 ms)
  release converter, raw_tex, black_nv12, last_live and device (keep sink object)

RECOVER tick (every tick while Recovering and now >= next_try)
  attempt += 1
  try:
     adapter = select_adapter(cfg.adapter, hwnd)
     dev = create_device(adapter)
     if sink.adapter_luid() != dev.luid -> return Lost(DeviceRemoved)   // see 5.7
     recreate converter, black_nv12
     ingest = Ingest::start(dev, hwnd, ...)
     raw_tex = ...
     state -> Live (on first frame), request_keyframe()
  on error: next_try = now + backoff[attempt] (1 s, 2 s, 4 s); if attempt == 3 -> Lost(RecoveryExhausted)
```

While recovering the pacer emits black frames at full fps; the muxer sees continuous timestamps. Recovery target is <= 2 s typical.

The sink (encode) also sees `SinkError::DeviceRemoved` on its own copy paths and must return the same error so capture can run this path. Encode must not hold a reference to the old device after recovery; it rebuilds its hwframes context when the sink factory is invoked again. This is the "re-open sink" contract (section 12, Q5).

### 5.9 Timestamps (QPC) and A/V sync

Single time base: QueryPerformanceCounter ticks, i64, `freq` from QueryPerformanceFrequency (typically 10 MHz).

| Stream | Stamp | Source |
|---|---|---|
| Video frame k | `pts_qpc = tick_qpc(base, freq, fps, k)` | Pacer tick, nominal. Not arrival time. |
| Video frame k pts (container) | `pts = k`, time base `1/fps` | Muxer, relative to `base_qpc` |
| Video source frame arrival | `latest.qpc_arrival` | Ingest, QPC at callback (diagnostic) |
| WGC content time | `latest.system_rel_qpc` | `SystemRelativeTime()` converted to QPC (diagnostic and A/V calibration) |
| Audio buffer | `u64QPCPosition` from `IAudioCaptureClient::GetBuffer` (100 ns units) | `audio` crate, converted to QPC ticks |

Conversion of `SystemRelativeTime` (a TimeSpan in 100 ns) to QPC ticks: `qpc = system_rel_100ns * freq / 10_000_000`. This assumes WGC's time base is QPC. Spike T1 confirms the assumption by comparing it to `QueryPerformanceCounter` at callback time (expected lag under 50 ms). If it does not match, `system_rel_qpc` is only used as a diagnostic and `qpc_arrival` is used instead (see 12, Q1).

Sync rules:

1. Video pts uses the nominal tick, not arrival. Jitter in callbacks does not change pts.
2. Audio pts is the QPC of the first sample in the packet, minus `base_qpc`, converted to the audio time base.
3. The session writer sets both streams' start time to the earliest of (video `pts_qpc` of tick 0, first audio QPC). Negative offsets are not written; the later stream is shifted.
4. `capture.video_offset_ms` is added to video pts (positive = video later). Default 0. Calibration is user-triggered from the settings UI with a flash test, not automatic.
5. Keyframe at split boundaries uses `k` so the next part starts exactly on tick `k`. (Owned by `session`.)

Drift: the pacer uses the QPC clock only, so there is no drift against audio over the session. Audio clock drift (sample rate of the device vs 48 kHz) is an `audio` concern and is not corrected here.

### 5.10 Exclusive fullscreen detection and limitation

WGC cannot capture a window presented with exclusive fullscreen (DXGI flip-through or legacy exclusive mode bypasses DWM composition). In that case the frame pool produces no frames, or the session fails to start.

Heuristic (`maybe_exclusive`), evaluated when the window has produced no frame for 3 s:

```
maybe_exclusive =
     window style has WS_POPUP and no WS_CAPTION
 AND window rect == monitor rect (full-screen)
 AND window is foreground
 AND no frames for >= 3 s
```

When `maybe_exclusive` is true:

1. Output switches to black with `HiddenReason::Exclusive`.
2. If `exclusive_warn` is true, emit `CaptureWarning { kind: ExclusiveFullscreenSuspected }` over IPC (once per session). The UI shows a banner: "Exclusive fullscreen detected. Set the game to borderless windowed to record it."
3. Capture keeps probing. If frames resume (the game left exclusive mode), the state returns to Live.

Limitation statement for user docs: exclusive fullscreen games cannot be recorded in v1. Borderless windowed (also called windowed fullscreen) is the supported mode. The heuristic can produce false positives on a static borderless game with no frames, in which case the output is black until the game produces a frame. This is accepted for v1.

### 5.11 Conversion shader

Two draw calls per live tick, both full-screen triangles from a vertex shader that uses `SV_VertexID` (no vertex buffer).

```
Pass 1 (Y):   RTV = NV12 plane 0 view, format R8_UNORM, viewport = canvas W x H
Pass 2 (UV):  RTV = NV12 plane 1 view, format R8G8_UNORM, viewport = canvas W/2 x H/2
Input:        SRV on raw_tex (BGRA8 or FP16), or scratch if downscale > 2:1
Sampler:      linear clamp
```

Matrix (BT.709, limited range, 8-bit):

```
Y'  = 0.2126 R' + 0.7152 G' + 0.0722 B'          (R',G',B' in 0..1, gamma-encoded)
Y   = 16  + 219 * Y'
Cb  = (B' - Y') / 1.8556
Cr  = (R' - Y') / 1.5748
U   = 128 + 224 * Cb
V   = 128 + 224 * Cr
```

Black canvas values (limited range): Y = 16, U = V = 128. Filling the `black_nv12` texture once uses `ClearRenderTargetView` on each plane view with those values normalized (16/255, 128/255 and 128/255 for R8G8). Y = 0 (or 0.0) is not black in limited range and must not be used.

Shader sketch (`convert.hlsl`):

```hlsl
cbuffer P : register(b0) {
    float4 srcRect;   // xy origin, zw size (source px)
    float4 dstRect;   // xy origin, zw size (canvas px)
    float4 canvas;    // canvas w, canvas h, hdr (1/0), sdr_scale = 80/sdr_white_nits
    float4 pad;
};
Texture2D<float4> src : register(t0);
SamplerState lin : register(s0);

// Canvas-space pixel centre -> source uv, or outside the letterbox.
bool ToSrcUv(float2 px, out float2 uv) {
    float2 d = (px - dstRect.xy) / dstRect.zw;   // 0..1 inside
    uv = (srcRect.xy + d * srcRect.zw) / src_size; // src_size from GetDimensions
    return all(d >= 0.0 && d < 1.0);
}

float3 ToSdr(float3 c) {
    if (canvas.z < 0.5) return saturate(c);              // 8-bit path: already sRGB
    c = max(c * canvas.w, 0.0);                          // scRGB 1.0 = 80 nits
    c = (c * (2.51 * c + 0.03)) / (c * (2.43 * c + 0.59) + 0.14);   // ACES fit
    return pow(saturate(c), 1.0 / 2.2);                  // approximate sRGB OETF
}

float PS_Y(float4 pos : SV_Position) : SV_Target {
    float2 uv;
    if (!ToSrcUv(pos.xy, uv)) return 16.0 / 255.0;
    float3 rgb = ToSdr(src.SampleLevel(lin, uv, 0).rgb);
    float yp = dot(rgb, float3(0.2126, 0.7152, 0.0722));
    return (16.0 + 219.0 * yp) / 255.0;
}

float2 PS_UV(float4 pos : SV_Position) : SV_Target {
    // pos is a half-res UV texel; sample the 2x2 canvas pixels it covers.
    float2 c = pos.xy * 2.0;
    float3 acc = 0;  int inside = 0;
    [unroll] for (int j = 0; j < 2; ++j) [unroll] for (int i = 0; i < 2; ++i) {
        float2 uv; float2 p = c + float2(i, j) + 0.5;
        if (ToSrcUv(p, uv)) { acc += ToSdr(src.SampleLevel(lin, uv, 0).rgb); inside++; }
    }
    float3 rgb = inside ? acc / inside : 0.0;
    float yp = dot(rgb, float3(0.2126, 0.7152, 0.0722));
    float cb = (rgb.b - yp) / 1.8556, cr = (rgb.r - yp) / 1.5748;
    if (inside == 0) return float2(128.0, 128.0) / 255.0;
    return (float2(128.0, 128.0) + 224.0 * float2(cb, cr)) / 255.0;
}
```

Notes:
- The `src_size` term is passed through the constant buffer in the implementation (not via GetDimensions) so the shader stays simple. It is omitted above for clarity.
- Using `inside` averaging keeps letterbox edges from bleeding black into content.
- Scaling quality: bilinear. For downscale ratios above 2:1 (for example 2160p to 720p), `Converter` first downscales raw to `scratch` (BGRA8 at 2x target) with one bilinear pass. This keeps aliasing acceptable without mip chains.
- RTV on NV12 plane views requires `D3D11_FORMAT_SUPPORT_RENDER_TARGET` for NV12 on the device. `GpuDevice.supports_nv12_rtv` is checked at start. If false, the fallback is `ID3D11VideoProcessor` (fixed-function scale and CSC, no HDR tone-map control), which is a v1.1 item.
- The NV12 destination texture is created by the encoder's hwframes context with `D3D11VA` binding flags that include `D3D11_BIND_RENDER_TARGET` (set through `AVD3D11VAFramesContext.BindFlags`). This is an encode-side requirement stated in section 4.3.
- Shaders are compiled at runtime with `D3DCompile` (ships with Windows, `d3dcompiler_47.dll`), once per process, roughly 10 to 30 ms. Compile errors are fatal at start with `CaptureError::Shader`.

### 5.12 Cursor

- `GraphicsCaptureSession.SetIsCursorCaptureEnabled(cfg.cursor)` at start and on reconfigure.
- WGC composites the cursor into frames only when it is over the captured window. Cursor movement alone does not generate new frames on some GPUs, so the pacer may duplicate frames while the cursor moves over a static scene. The output is still correct; cursor motion is only smooth if the scene is also changing. Documented, not fixed in v1.
- Software cursor overlay (drawing our own cursor from position samples) is post-v1.

### 5.13 Border (Windows 11)

- `GraphicsCaptureSession.SetIsBorderRequired(false)` removes the yellow capture border on Windows 11. Guarded with `ApiInformation::IsPropertyPresent` (implemented as `disable_border` in `crates/capture/src/wgc.rs`, D59). Both outcomes are logged to stderr. The `BorderMode` config option described below is not implemented yet.
- Probe on Windows 11 25H2 (`crates/capture/examples/border_probe.rs`): property present, `SetIsBorderRequired(false)` returns Ok.
- On Windows 10 the border cannot be removed. `BorderMode::Off` would then be a no-op with a logged reason (not implemented; the log line in `disable_border` covers it today).
- The border is a screen overlay. Test T1 verifies that it does not appear in recorded frames. If it does, `Off` is required on all supported builds (see 12, Q4).

### 5.14 Shutdown

```
STOP(flush)
 1  signal pacer to stop; join with 500 ms timeout (on timeout, log and detach)
 2  if flush: emit final tick (Live or Black) to sink
 3  ingest.stop(): session.Close(), pool.Close(), item.Closed handler removed
 4  drop converter, textures, device (in that order)
 5  return CaptureSummary
```

### 5.16 No-border capture (D64, opt-in, Windows 10)

Problem: on Windows 10 WGC always draws the capture border (5.13). `video.no_border_capture` records
via DXGI Desktop Duplication of the game window's monitor, cropped to the window client area.
Full contract in `docs/impl/no-border-capture.md`.

- Gating: off by default; only `CaptureBackend::NoBorder` enters `dda.rs`. The WGC path is unchanged.
- Eligibility: the window must cover >= 95 % of its monitor (`dda_geom::covers_monitor`). Otherwise fall back to WGC.
- Same-adapter rule: Desktop Duplication must run on the adapter that owns the output; if the encoder device is on another GPU, fall back.
- Frames: the dda thread publishes cropped texture copies into the same `Latest`/`TextureRing` the pacer reads, so pacing, black-on-hidden and encoding are unchanged. Cursor-only updates are ignored; ACCESS_LOST rebuilds the duplication.
- Known costs: overlays and notifications on top of the game are recorded; HDR or rotated outputs fall back; exclusive fullscreen is not promised to work.
- Unverified: absence of the border on a Windows 10 machine.

### 5.15 v2 game-hook design sketch (not built in v1)

Status (D64): parked, not scheduled. Desktop Duplication (5.16) covers the Windows 10 border case without injecting into games, and injection carries anti-cheat ban, antivirus and crash risk. Revisit only if Windows 10 testing shows 5.16 fails, or users repeatedly report overlay leakage or windowed/exclusive-fullscreen gaps that borderless mode cannot fix. Any build starts DX11/DX12-only and refuses games with known anti-cheat.

Purpose: record exclusive-fullscreen games that WGC cannot capture. Same approach as OBS "game capture". Designed here so the v1 interfaces do not block it. Nothing in this section is implemented in v1.

```
 ClipCheck service (clipcheckd)              Game process (x64)
 +---------------------------+               +---------------------------------+
 | capture::HookSource        |  named pipe   | clipcheck_hook.dll (injected)    |
 | - control channel (JSON)   |<------------->| - hooks IDXGISwapChain::Present  |
 | - shared texture consumer  |  shared NT    | - Present1, ResizeBuffers        |
 | - keyed mutex / fence wait |<==handle====>| - copies backbuffer to shared tex|
 +---------------------------+               +---------------------------------+
```

Mechanics:

| Step | Mechanism |
|---|---|
| Opt-in | Per-game allowlist entry, set by the user in the UI. Never automatic for every game. |
| Injection | `CreateRemoteThread` + `LoadLibraryW` in a suspended-start or on-demand model. Requires the same integrity level as the game. |
| Hook | Patch the `IDXGISwapChain` vtable for `Present`, `Present1`, `ResizeBuffers` after the game creates its swap chain. Vulkan games: an implicit Vulkan layer (registered per user) hooks `vkQueuePresentKHR`. OpenGL: `wglSwapBuffers`. |
| Frame transport | Hook creates a shared texture with `D3D11_RESOURCE_MISC_SHARED_NTHANDLE`, copies the backbuffer into it with `CopyResource`, duplicates the NT handle into the service process with `DuplicateHandle`. |
| Sync | Keyed mutex (key 0 game, key 1 service) or a shared `ID3D11Fence` (preferred on Win 10 1703+). Service waits with timeout 10 ms; on timeout it duplicates the last frame, same as the pacer. |
| Control | Named pipe with the same length-prefixed JSON style as the main IPC; messages: `hook.hello`, `hook.format {w,h,fmt}`, `hook.frame {seq}`, `hook.bye`. |
| Output | The service feeds the shared texture into the same ingest interface. Pacer, conversion and encode are unchanged, because the source is a `GpuTextureSource` behind a trait. |

Required v1 preparation (already reflected in this doc):

1. The pacer consumes a `FrameSourceTrait` with `latest_texture(&self) -> Option<ID3D11Texture2D>` and `seq()`, not WGC types. WGC is one implementation. (Implement as an internal trait in T3.)
2. Conversion takes any BGRA8 or FP16 texture on the capture device. Shared textures from a hook are opened on the capture device via `OpenSharedResource1`, so the same device constraint applies.
3. Window probe logic is independent of the source. A hook source still uses the probe for Hidden and Gone.

Risks and constraints (why v2, not v1):

| Risk | Notes |
|---|---|
| Anti-cheat | EAC, BattlEye, Vanguard and similar systems block or ban DLL injection and swap-chain hooks. Hook must be disabled for games with known anti-cheat. Allowlist with a warning. |
| Stability | A bad hook can crash the game. Hook code must not allocate in Present and must bail out on any error. |
| Antivirus | Injection and hooking trigger heuristics. Needs code signing and a clear user prompt. |
| Maintenance | Per-API hooks (DX9/10/11/12, Vulkan, GL) multiply the test matrix. v2 starts with DX11 and DX12 only. |
| Legal and policy | Some game EULAs forbid modifying the game process. Users are told this in the UI before enabling a hook. |

Open items for v2 (not in section 12 because they are v2-only): see Q7 and Q8, which affect overlays and resize policy in both paths.

---

---

## 6. Config keys owned by this feature

All keys live under the `[capture]` table in `%APPDATA%\ClipCheck\config.toml`. Hot-reloadable. "Live" means applied without restart; "Split" means applied at the next part boundary (fps and output size cannot change mid-part).

| Key | Type | Default | Range / values | Apply | Notes |
|---|---|---|---|---|---|
| `capture.fps` | u32 | `60` | `30`, `60`, `120` | Split | Target CFR rate. Quality presets set this. |
| `capture.resolution` | enum string | `"native"` | `native`, `1440`, `1080`, `720` | Split | Canvas height. Width follows aspect, rounded to even. |
| `capture.cursor` | bool | `true` | | Live | WGC cursor capture. |
| `capture.hdr_tonemap` | enum string | `"auto"` | `auto`, `off` | Live | `off` forces 8-bit SDR pool. |
| `capture.sdr_white_nits` | u32 | `240` | `80` to `400` | Live | Paper-white for tone map. |
| `capture.adapter` | enum string | `"auto"` | `auto`, `high_performance`, `monitor` | Split | Changing adapter forces encoder rebuild. |
| `capture.border` | enum string | `"auto"` | `auto`, `on`, `off` | Live | Windows 11 only. |
| `capture.hidden_grace_ms` | u32 | `250` | `0` to `5000` | Live | Debounce before black on minimize or hide. Avoids flicker during alt-tab. |
| `capture.stall_restart_ms` | u32 | `3000` | `1000` to `30000` | Live | Visible, no frame: restart WGC session after this. |
| `capture.resize_debounce_ms` | u32 | `150` | `0` to `1000` | Live | Delay before pool Recreate during a resize. |
| `capture.pool_buffers` | u32 | `2` | `2` to `3` | Split | WGC buffer count. 3 adds latency headroom at ~1 frame of VRAM. |
| `capture.keyframe_on_resume` | bool | `true` | | Live | Request IDR when Hidden to Live. |
| `capture.video_offset_ms` | i32 | `0` | `-500` to `500` | Live | Added to video pts. Positive = video later. |
| `capture.exclusive_warn` | bool | `true` | | Live | Show exclusive fullscreen banner. |

Validation: out-of-range values are clamped with a logged warning, not rejected, except `fps` and `resolution` which are rejected (the previous value is kept).

Settings doc note: presets (Low/Medium/High/Ultra) set `capture.fps` and `capture.resolution`. They do not set any other key here.

---

## 7. IPC messages / events

Transport: named pipe `\\.\pipe\clipcheck-ipc`, length-prefixed JSON (per brief). Messages below are the payloads; `ipc` owns the envelope.

### 7.1 Consumed (commands from UI or session layer)

| Command | Payload | Effect |
|---|---|---|
| `capture.start` | `{ "session_id": "...", "hwnd": 123456, "pid": 4321, "config": {...} }` | Starts a session. Reply: `capture.started` or `capture.error`. |
| `capture.stop` | `{ "session_id": "...", "flush": true }` | Stops, returns summary in `capture.stopped`. |
| `capture.reconfigure` | `{ "session_id": "...", "config": {...} }` | Live keys applied. Split keys reply `capture.restart_required`. |
| `capture.request_keyframe` | `{ "session_id": "..." }` | Used by split logic. |

### 7.2 Produced (events to service subscribers)

| Event | Payload | When |
|---|---|---|
| `capture.started` | `{ "session_id", "canvas": [w,h], "fps", "adapter": "NVIDIA GeForce RTX ...", "luid", "hdr_source": bool, "base_qpc", "qpc_freq" }` | After start succeeds |
| `capture.state` | `{ "session_id", "state": "live"|"hidden"|"recovering"|"lost", "reason": "minimized"|"hidden"|"cloaked"|"gone"|"exclusive"|"device_lost"|"capture_failed"|"target_gone"|"recovery_exhausted"|null, "since_ms" }` | On every transition |
| `capture.stats` | `{ "session_id", "fps_out", "live", "dup", "black", "src_dropped", "enc_overrun", "skipped_ticks", "resizes", "last_pts", "cpu_pct_capture", "gpu_ms_p95" }` | Every 1 s while recording; 4 Hz not needed |
| `capture.format_changed` | `{ "session_id", "source": [w,h], "pool_format": "bgra8"|"fp16", "hdr_source": bool }` | Resize, DPI, or HDR switch |
| `capture.warning` | `{ "session_id", "kind": "exclusive_fullscreen_suspected"|"output_locked"|"border_not_supported"|"cross_adapter_cost_high"|"protected_content_suspected", "detail": "..." }` | Once per session per kind |
| `capture.error` | `{ "session_id", "code": "unsupported"|"invalid_config"|"window_not_capturable"|"device"|"wgc"|"shader", "message", "recoverable": bool }` | Start failure or Lost |
| `capture.stopped` | `{ "session_id", "summary": {...} }` | After stop |

Example `capture.state` payload:

```json
{ "type": "capture.state", "session_id": "01J9ZQ...", "state": "hidden",
  "reason": "minimized", "since_ms": 1760000000123 }
```

`capture.stats` is consumed by the UI status line and by the service log. Stats are not persisted per second; the session summary is written to the `recordings` row on stop (session doc).

---

## 8. Edge cases & failure modes

| # | Case | Expected behavior | Mitigation / detection |
|---|---|---|---|
| 1 | Window minimized | Black after `hidden_grace` | `IsIconic` in probe |
| 2 | Window hidden with `SW_HIDE` | Black | `IsWindowVisible == false` |
| 3 | Window on another virtual desktop | Black | `DWMWA_CLOAKED` |
| 4 | Target window destroyed | Black, then Lost(TargetGone) after 2 s | `IsWindow`; WGC `Closed` event |
| 5 | Static content (paused menu, loading screen, no frames from WGC) | Duplicates, not black | Visible-state rule; restart probe only |
| 6 | Game throttles rendering when unfocused | Duplicates at game's reduced rate | Expected. Documented. |
| 7 | Game renders above 120 fps with fps target 60 | Latest-wins, surplus counted as `src_dropped` | Pacer step 6 |
| 8 | Game renders at 30 fps with target 60 | Each new frame is emitted once, the rest are duplicates | Pacer step 8 |
| 9 | Resize during drag (many sizes per second) | Pool Recreate after debounce; at most one Recreate per 150 ms; forced every 500 ms after 2 s | 5.5 |
| 10 | Window grows beyond canvas | Downscale to fit canvas (letterbox) | Fixed canvas |
| 11 | DPI change (monitor move) | Treated as resize | 5.5 |
| 12 | Window moves to HDR monitor (or SDR) | Pool format switches within 250 ms | 5.6 probe |
| 13 | HDR10 PQ surface instead of scRGB | Colors wrong if the path is not PQ-aware | Open question Q3. v1 detects scRGB only; PQ falls back to 8-bit pool with DWM tone map. |
| 14 | Protected content (`SetWindowDisplayAffinity` with `WDA_EXCLUDEFROMCAPTURE` or DRM-protected video overlay) | Black region or full black frames | Detect: frame content all black for 2 s while Live and window not hidden. Show warning `protected_content_suspected`. Not auto-fixed. |
| 15 | Exclusive fullscreen | Black, warning banner | 5.10 |
| 16 | Overlay windows owned by game (in-process overlays such as Steam or Discord) | Appear in capture if drawn into the game's swap chain; separate HWNDs are not captured | Documented; `exclude_overlays` is open (Q7) |
| 17 | Another app's window covers the game | Game pixels only | WGC property |
| 18 | GPU TDR or driver reset | Black, recovery in 5.8 | `DXGI_ERROR_DEVICE_REMOVED` |
| 19 | Laptop switches dGPU on or off (power cable) | Device removed or item closed; recovery re-selects adapter; encoder may need rebuild | 5.7, 5.8, split (Q5) |
| 20 | Cross-adapter copy cost too high (Optimus with display on iGPU) | Higher CPU and GPU use; if over budget, UI suggests `monitor` mode | Stats `cpu_pct_capture` |
| 21 | Frame pool exhaustion (ingest does not Close) | Pool stalls; no new frames | Guard `Drop` closes frame; watchdog: no ingest callback for 3 s while `pool_buffers` frames outstanding logs error |
| 22 | Encoder queue full | Tick skipped, `enc_overrun` counted, PTS gap | 5.3 step 7 |
| 23 | Config `fps` or `resolution` changed mid-session | Rejected, split required | `RestartRequired` |
| 24 | Window is elevated (admin) and ClipCheck is not | WGC works for elevated windows? Assumed to work for capture of the window; verify in T1 | Test matrix includes elevated game |
| 25 | Window is UWP or packaged (Store) app | Same as Win32 for WGC | Test matrix |
| 26 | Window of a 16:9 game shown on 21:9 monitor (letterboxed inside window) | Window content already includes black bars; captured as-is | None needed |
| 27 | Odd canvas dimensions | Rounded down to even; NV12 requires even width and height | `round_even` |
| 28 | Odd source dimensions for chroma | Edge column averages inside-only samples | 5.11 `inside` logic |
| 29 | HDR tone map on SDR-only content | Shader is not used in 8-bit path | `hdr_active` false |
| 30 | Shader compile fails on a driver | Start fails with `Shader`; logs D3DCompile message | Start-time error |
| 31 | NV12 RTV unsupported on device | Start fails with `Unsupported` (video processor fallback is v1.1) | `supports_nv12_rtv` |
| 32 | Clock jump (NTP, sleep/resume) | QPC is monotonic; no effect on pts. Pacer detects lag and skips ticks (5.3 step 4). | Lag rule |
| 33 | System sleep and resume | Device may be removed; recovery path | 5.8 |
| 34 | Secure desktop (UAC prompt, Ctrl-Alt-Del) | Window content stops updating; frames stop; duplicates, then Restarting, then Lost after budget | Expected; session layer may pause |
| 35 | Start with window not yet drawn (first frame delayed) | Output is black until first frame; `start` does not block more than 1 s | 5.1 step 7 |

---

## 9. Performance & resource budget

Reference hardware: Intel i7 laptop with Iris Xe (iGPU), RTX 3060 Laptop (dGPU), 1080p display; and a desktop with RTX 3070. Targets are per-session, while recording a game at the stated settings.

| Metric | Target | Notes |
|---|---|---|
| Capture-related CPU (all threads) at 1080p60 native, dGPU | < 1.0 % of one core | Ingest + pacer + probe. Excludes game and encoder. |
| Capture-related CPU at 1440p60 -> 1080p output | < 1.5 % of one core | Downscale cost is GPU |
| Capture-related CPU, hybrid case B (cross-adapter) | < 2.0 % of one core | Above this, UI warns (`cross_adapter_cost_high`) |
| GPU time per live tick, 1080p output, dGPU | p95 < 0.6 ms | Copy + Y + UV passes |
| GPU time per live tick, 1080p output, iGPU | p95 < 1.5 ms | |
| Duplicate tick cost | < 0.2 ms | NV12 copy only |
| Black tick cost | < 0.1 ms | NV12 copy from prefilled texture |
| Pacer timing jitter (tick actual vs nominal), p99 | < 1.0 ms | Waitable timer HIGH_RESOLUTION |
| Pacer timing jitter, p99.9 | < 4 ms | Under load. Pts unaffected. |
| Capture-to-encoder latency (arrival to push), p95 | < 1 frame (16.7 ms at 60) | Pacer quantizes to tick |
| Idle (no session) | 0 threads, 0 timers | Crate inert until `start` |
| VRAM, SDR 1080p-class content, pool 2 | < 15 MB | 2 WGC buffers + raw + last_live + black |
| VRAM, HDR 1440p content, pool 2 | < 40 MB | FP16 buffers are 2x size |
| System RAM (capture only) | < 2 MB | No CPU frame copies |
| CPU readback | none in steady state | All conversion is GPU. Readback only for tests. |
| Recovery from device lost | <= 2 s typical | 5.8 |
| Resize debounce to new pool | <= 150 ms default | 5.5 |
| Hidden to live resume | next tick (<= 16.7 ms) after first frame | Plus WGC first frame latency (measure in T1) |

Budget rules:
- No heap allocation on the ingest callback and the pacer hot path after startup (preallocated structs; `Vec` capacity reserved).
- No `Mutex` held across GPU waits. `ctx.Flush()` is not `Wait`.
- Counters are `AtomicU64` with relaxed ordering; the 1 s stats snapshot is taken by the stats thread.

---

## 10. Test plan

### 10.1 Unit tests (no GPU)

| Test | What it checks |
|---|---|
| `tick_math` | `tick_qpc` over 1 h at 30/60/120 fps: exactly `fps*3600` ticks; no drift (error < 1 tick) |
| `pacer_dup_drop` | Fake source with arrivals at random rates (0 to 240 Hz) against 60 fps ticks: emitted count per second = 60 exactly; `Live + Dup == 60`; `src_dropped == arrivals - Live` |
| `pacer_lag` | Inject 500 ms stall in the clock: no burst of catch-up frames; `skipped_ticks` equals stalled ticks; pts monotonic |
| `state_machine` | Table-driven: every transition in 5.4 with fake probe and fake clock. Hidden grace (250 ms) not triggered by 100 ms flicker. Resume triggers keyframe request exactly once. |
| `hidden_grace_flicker` | Window toggles Presentable/Hidden every 100 ms for 10 s: state never leaves Live |
| `letterbox_math` | Source sizes 1920x1080, 1280x1024, 3440x1440, 800x600 into 1920x1080 canvas: even dims, centered, never exceeds canvas |
| `round_even` | Canvas and dst rect always even |
| `config_validate` | Out-of-range clamps, fps and resolution rejection |
| `shader_compile` | `D3DCompile` succeeds for both entry points on the CI Windows image |

### 10.2 GPU tests (Windows runner with a GPU, self-hosted)

| Test | What it checks |
|---|---|
| `shader_golden_sdr` | Known BGRA test pattern (SMPTE bars, gradient, checker) converted to NV12, read back, compared to CPU reference: max abs error <= 1 LSB for Y, <= 2 LSB for UV |
| `shader_golden_letterbox` | Letterbox regions are exactly Y=16, UV=128 |
| `shader_golden_hdr` | scRGB ramp 0..4 converted with `sdr_white_nits` 240 and 80: compare to CPU tone-map reference, error <= 2 LSB |
| `black_nv12_fill` | `black_nv12` after clear: Y all 16, UV all 128 |
| `downscale_2to1_plus` | 2160p source to 720p canvas through scratch: no aliasing spike in a checkerboard pattern (measure energy at Nyquist within threshold) |
| `resize_recreate` | Resize window 50 times over 10 s: no stalled pool, `resizes` count matches, output canvas constant |
| `dpi_change` | Move window between 100% and 200% monitors (test rig with two displays): content size change detected, output continuous |
| `hdr_switch` | Move window from HDR to SDR monitor: `format_changed` event within 250 ms; no black frames |

### 10.3 Integration and manual matrix

Each row is a run of 10 minutes, recording a real game or a test app (a simple D3D11 test window with a moving counter for timing checks).

| Scenario | Systems | Pass criteria |
|---|---|---|
| Baseline 1080p60, cursor on | Desktop RTX 3070, Win 11 24H2 | 60 fps output, 0 `enc_overrun`, `src_dropped` as expected |
| Baseline on Win 10 22H2 | Desktop, Win 10 22H2 | Same; border cannot be removed (logged only) |
| Hybrid Optimus, display on iGPU, `auto` | Laptop with iGPU + dGPU | Adapter = dGPU, `cpu_pct_capture` < 2 % |
| Hybrid, `monitor` mode | Same | Adapter = iGPU; encoder = QSV |
| Hybrid, MUX on (if available) | Same | Adapter = dGPU; no cross-adapter |
| AMD dGPU + iGPU | AMD laptop | Adapter = dGPU; encoder = AMF |
| Minimize/restore 200 times | Desktop | Black during min, live within 1 frame after restore; no pool stall |
| Alt-tab every 2 s for 5 min | Desktop, windowed game | Live throughout (visible-unfocused), no black |
| Hide via `SW_HIDE` (test app) | Test app | Black; resume on show |
| Virtual desktop switch | Desktop with 2 virtual desktops | Black while on other desktop; resume |
| Borderless fullscreen game | Game | Live |
| Exclusive fullscreen game | Game | Black, banner shown, documented limitation |
| Driver reset (`Restart-Service` of the display driver via Win+Ctrl+Shift+B) | Desktop | Black, recovery within 2 s, live after |
| Sleep and resume | Laptop | Recovery; no crash; output resumes |
| Soak 4 h at 1440p60 -> 1080p | Desktop | No memory growth (VRAM and RAM flat within 1 %), no pool stall, `enc_overrun` < 0.01 % |
| A/V sync | Test app with flash and click sound, microphone loopback | Measured offset within +/-20 ms (before `video_offset_ms`) |

### 10.4 Instrumentation

- Each session writes a capture log with per-second stats (`capture.stats`).
- A debug build can save every Nth output NV12 frame as PNG (converted back to RGB on CPU) under the scratchpad directory for visual review.
- `tracing` spans on each tick are behind a feature flag `trace-ticks` (off by default; overhead is measurable).

---

## 11. Implementation task breakdown

Each task is about 0.5 to 2 days. Order matters where noted.

| # | Task | Days | Depends on | Done when |
|---|---|---|---|---|
| T1 | Spike: WGC from HWND in a console app. Log frame arrivals, `SystemRelativeTime` vs QPC, cursor on/off, border visibility in frames, static-window frame behavior, elevated and UWP targets, cross-adapter cost on Optimus laptop | 2 | none | Written answers to Q1, Q2, Q4, Q6; numbers recorded in the doc |
| T2 | `device.rs`: adapter selection (3 modes), D3D11 device with BGRA support, LUID, NV12 RTV check | 1 | T1 | Unit-level manual run lists adapters and picks correct one for hybrid test laptop |
| T3 | `ingest.rs`: item, free-threaded pool, FrameArrived, copy to raw_tex, Close guard, resize Recreate | 2 | T2 | Ingest logs fps and sizes on resize test; pool never stalls over 10 min |
| T4 | `convert.rs` and `convert.hlsl` SDR path: Y and UV passes, letterbox, black fill, scratch downscale | 2 | T3 | `shader_golden_sdr`, `shader_golden_letterbox`, `black_nv12_fill` pass |
| T5 | HDR path: FP16 pool, tone map, format switching on monitor change | 1.5 | T4 | `shader_golden_hdr`, `hdr_switch` pass |
| T6 | `pacer.rs`: tick clock, waitable timer, MMCSS, dup/drop logic, Live/Dup/Black output, lag skip | 2 | T4 | `tick_math`, `pacer_dup_drop`, `pacer_lag` pass |
| T7 | `probe.rs` and state machine: minimized, hidden, cloaked, gone, hidden grace, resume keyframe, restart probe | 1.5 | T6 | `state_machine`, `hidden_grace_flicker` pass; manual minimize and alt-tab matrix |
| T8 | Device-lost recovery and sink re-open contract; fault injection in `fault.rs` | 1.5 | T7 | Simulated device removal recovers within 2 s; `Lost` after 3 failed attempts |
| T9 | Encode integration: sink factory, hwframes context on capture device with `BindFlags` RENDER_TARGET, LUID check, encoder vendor restriction. Joint with `encode` owner | 1 | T4, encode crate skeleton | End-to-end file with H.264 from capture device on dGPU and iGPU |
| T10 | Timestamps: QPC base, `system_rel_qpc` diagnostics, `video_offset_ms`, stats thread, IPC events | 1 | T6 | Event sequence in section 7 observed via IPC test client |
| T11 | Protected content and exclusive fullscreen heuristics and warnings | 1 | T7 | Warnings emitted in manual tests |
| T12 | Test harness: golden images, CPU references, soak runner, instrumentation (PNG dump, trace feature) | 2 | T4, T6 | Golden suite runs in CI on the self-hosted Windows runner |
| T13 | Performance pass against section 9 budgets; tune waits, copies, pool size | 1.5 | all | Budgets met on reference hardware; soak 4 h passes |
| T14 | Documentation for user: exclusive fullscreen limitation, borderless guidance, hybrid adapter setting | 0.5 | T11 | Text reviewed; UI banner strings final |

Critical path: T1, T2, T3, T4, T6, T7, T8, T9, T13. Parallel track: T5 (HDR), T10, T11, T12.

---

## 12. Open questions

These do not block starting T1. Each has a planned resolution.

| # | Question | Impact | Resolution plan |
|---|---|---|---|
| Q1 | Is `FrameArrived` `SystemRelativeTime` in QPC base time? | Choice of time source for diagnostics and A/V calibration. Video pts is nominal and unaffected. | T1 compares it to QPC at callback. If not matching, keep `qpc_arrival` only. |
| Q2 | Does WGC deliver periodic frames for a static visible window? | Decides whether "no frame for 3 s" means stall or static content. Affects the Restarting rule. | T1 measures with a static test window. If it does not deliver, the restart rule is a fallback only and static content is always duplicated. |
| Q3 | HDR10 PQ (`G2084`) surfaces: does WGC FP16 pool always deliver scRGB linear, or does it deliver PQ on some drivers? | Colors could be wrong on some HDR setups. | T5 tests on at least two HDR monitors. PQ path may be added in v1.1 if needed. |
| Q4 | Does the Windows 11 border appear in captured frames, and is `IsBorderRequired = false` allowed for an unpackaged Win32 app without a manifest capability? | Whether border mode is a cosmetic or a required setting. | T1 test. If a capability is needed, the app is switched to MSIX packaging or border stays on. |
| Q5 | Encoder rebuild on adapter change (dGPU power-off, GPU mode switch): split or in-place? | Split is simpler and matches brief's split semantics. In-place requires encode re-init with the same muxer timeline. | Proposal: always split on adapter change. Needs session doc sign-off. |
| Q6 | Cross-adapter copy done by WGC on Optimus laptops: CPU cost and whether `monitor` mode is better by default there? | Default adapter choice on hybrid laptops. | T1 measurement, then decide default. Current default is `auto` (dGPU). |
| Q7 | Should in-process overlays (Steam, Discord) be excluded from capture? | Brief says other apps' pixels must not appear. Overlays drawn into game swap chains are game-process pixels, not other apps' windows, but users may not expect them. | Proposal: keep them (they are in the game's surface; excluding needs a hook, which is v2). Confirm with product. |
| Q8 | Native output canvas: fixed at session start, or split on size change? | Affects quality when the user resizes the window during a session. | Proposal: fixed canvas for v1 with a UI note. Split-on-resize as option later. |
| Q9 | Are presets (Low/Medium/High/Ultra) allowed to change `capture.resolution` and `capture.fps` only, or also `pool_buffers`? | Settings doc. | Proposal: presets change only fps and resolution. |
| Q10 | Brief says "fps (30/60/120) selection". Should 120 fps be allowed on a 60 Hz monitor for capture? | Capture is not bound by display refresh; this is about the game's frame rate. | No change needed: capture rate is independent of display. Note in user docs that 120 requires a game that renders at 120. |
| Q11 | Is `SetIsCursorCaptureEnabled` honored on all supported Windows 10 builds? | Cursor option on Win10 2004 builds. | T1 checks on 19041 and 22H2. |
| Q12 | Pool buffer count 2 vs 3 default | Latency vs VRAM. | Default 2. T13 measures stalls at 120 fps. |
| Q13 | Protected-content detection reliability | Black output with no warning if undetected. | Warning heuristic only; no bypass attempts. Accepted limitation. |

---

## Appendix A - Glossary

| Term | Meaning |
|---|---|
| WGC | Windows Graphics Capture (`Windows.Graphics.Capture`) |
| Frame pool | WGC buffers delivered per frame; returned by `Close()` |
| Content size | Size of the captured window surface in pixels; can change at any frame |
| Tick | One output slot at target fps, index `k`, nominal QPC `T_k` |
| Live / Duplicate / Black | Kinds of output frame (`FrameKind`) |
| QPC | QueryPerformanceCounter, monotonic time base shared with audio |
| NV12 | 8-bit 4:2:0 semi-planar YUV (Y plane, interleaved UV plane) |
| Limited range | Y 16..235, UV 16..240. Black is Y=16, UV=128 |
