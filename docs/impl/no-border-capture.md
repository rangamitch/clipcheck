# No-border capture (D64)

Opt-in second capture source that records a window **without the Windows capture border**,
for Windows 10 where `IsBorderRequired` does not exist (design 02 section 5.13, 5.16).

It uses DXGI Desktop Duplication of the monitor the game is on, cropped to the game window's
client area. Nothing is injected. Off by default; the code path is not entered unless the user
turns on `video.no_border_capture`.

## Trade-off the user accepts

Desktop Duplication captures the **screen**, not the window. Anything drawn on top of the game
(a Discord toast, another window) appears in the clip. To limit that, the source only starts when
the game window covers (at least 95 % of) its monitor, i.e. borderless/fullscreen. Otherwise the
recorder falls back to normal window capture (border stays) and logs why. The setting's help text
says all of this.

## Settings

`[video] no_border_capture = false` (bool). Global only (no per-game override). Lives in the
**Advanced** block of the Video settings tab. Changing it is `PipelineReinit` (the existing
"any other video key" rule in `validate.rs` covers it, no code change).

## Capture crate API (`crates/capture`)

```rust
/// Which source feeds the pacer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaptureBackend {
    #[default] Wgc,                       // Windows Graphics Capture (today's behaviour)
    NoBorder { require_cover: bool },     // Desktop Duplication crop. Production passes true.
}
WindowCapturer::start_gpu_with(cfg, &device, backend) -> Result<(WindowCapturer, Receiver<GpuFrame>), CaptureError>
// start_gpu(cfg,&device) == start_gpu_with(cfg,&device,CaptureBackend::Wgc). start() (CPU) is WGC only.
pub fn no_border_attempts() -> u64;       // process-wide count of times the NoBorder source was entered
CaptureError::NoBorderUnavailable { reason: String }   // not an "environment" error
```

`lib.rs` (orchestrator-owned, done) dispatches: `Wgc` calls `wgc::spawn` exactly as before and never
touches `dda`/`dda_geom`. `NoBorder` calls `dda::spawn`. The pacer, `Latest`, `TextureRing`,
`GpuFrame` are shared unchanged: the DDA thread publishes into `Shared` like the WGC callback does.

### `dda_geom.rs` (pure, public so tests can reach it)

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect { pub left: i32, pub top: i32, pub right: i32, pub bottom: i32 }
impl Rect {
    pub fn new(left: i32, top: i32, right: i32, bottom: i32) -> Rect;
    pub fn width(&self) -> i64;   // (right-left).max(0), computed in i64
    pub fn height(&self) -> i64;
    pub fn area(&self) -> i64;    // width*height
    pub fn is_empty(&self) -> bool;                 // width()==0 || height()==0
    pub fn intersect(&self, o: &Rect) -> Rect;      // empty Rect {0,0,0,0} when disjoint
    pub fn translate(&self, dx: i32, dy: i32) -> Rect;  // saturating
}
/// true when `window` ∩ `monitor` has area >= 95 % of monitor's area. false if monitor is empty.
pub fn covers_monitor(window: Rect, monitor: Rect) -> bool;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CropBox { pub left: u32, pub top: u32, pub width: u32, pub height: u32 }
/// `client_screen`: window client area in virtual-screen pixels. `output_desktop`: the duplicated
/// output's DXGI DesktopCoordinates (virtual-screen pixels). Returns the crop in OUTPUT-LOCAL pixels
/// (origin = output top-left), clamped to the output. None when the intersection is empty or
/// smaller than MIN_CROP (16) in either dimension.
pub fn crop_box(client_screen: Rect, output_desktop: Rect) -> Option<CropBox>;
pub const MIN_CROP: u32 = 16;
/// DXGI_MODE_ROTATION value: true for UNSPECIFIED (0) and IDENTITY (1) only.
pub fn rotation_supported(rotation: i32) -> bool;
```

### `dda.rs` (Windows only, `pub(crate)`)

```rust
pub(crate) struct DdaThread { /* stop_tx: Sender<()>, join: JoinHandle<()> */ }
impl DdaThread { pub(crate) fn shutdown(self); }   // drop sender, join
pub(crate) fn spawn(hwnd: isize, fps: u32, shared: Shared, errors: Arc<AtomicU64>,
                    device: ID3D11Device, require_cover: bool) -> Result<DdaThread, CaptureError>;
```

`spawn` increments the `no_border_attempts` counter FIRST (a `pub(crate) static NO_BORDER_ATTEMPTS:
AtomicU64` in dda.rs, read by `lib.rs`'s `no_border_attempts()`), then starts thread
`clipcheck-dda`, waits for an init result over a `sync_channel` (same pattern as `wgc::spawn`) and
returns it.

Init (on the dda thread, errors returned through the init channel):
1. `SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)` so window/monitor
   rects are physical pixels. Thread-local; restore nothing (thread is private).
2. Validate hwnd like `wgc::setup` (`IsWindow`, not desktop/shell) -> `InvalidWindow(hwnd)`.
3. `hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST)`; monitor rect via `GetMonitorInfoW`.
4. Window client rect in screen coords: `GetClientRect` + `ClientToScreen` on the top-left.
5. If `require_cover && !covers_monitor(client, monitor)` -> `NoBorderUnavailable{reason:"the game window does not cover its monitor (use borderless fullscreen)"}`.
6. `device` -> `IDXGIDevice` -> `GetAdapter`; loop `EnumOutputs`, pick the output whose
   `DXGI_OUTPUT_DESC.Monitor == hmon`. None -> `NoBorderUnavailable{reason:"the monitor is on a different graphics adapter than the encoder"}`.
7. `!rotation_supported(desc.Rotation.0)` -> `NoBorderUnavailable{reason:"rotated displays are not supported"}`.
8. `output.cast::<IDXGIOutput1>()?.DuplicateOutput(&device)`; map failure to
   `NoBorderUnavailable{reason: format!("Desktop Duplication refused: {e}")}`.
9. Verify `DXGI_OUTDUPL_DESC.ModeDesc.Format == DXGI_FORMAT_B8G8R8A8_UNORM`, else
   `NoBorderUnavailable{reason:"unsupported display format (HDR?)"}`.

Loop (until the stop sender is dropped; check `stop_rx.try_recv()` -> `Disconnected` each pass):
- timeout_ms = (1000 / fps).clamp(4, 100).
- `AcquireNextFrame(timeout_ms, &mut info, &mut resource)`:
  - `DXGI_ERROR_WAIT_TIMEOUT`: continue (pacer repeats the last frame).
  - `DXGI_ERROR_ACCESS_LOST` (mode change, secure desktop, fullscreen switch): drop the duplication,
    `note_error`, retry re-creating it (steps 3, 6-8) every 250 ms until success or stop.
  - other error: `note_error`, sleep 50 ms, continue.
  - Ok: if `info.LastPresentTime == 0` (cursor-only update) -> `ReleaseFrame`, continue.
    Else apply the rate gate `wgc::accept_frame(last, qpc_now, qpc_freq/fps)`; if rejected release and continue.
    Re-read the client rect each frame (window may move/resize); `crop_box` -> None => release, continue.
    `ring.acquire(w,h)` then `CopySubresourceRegion` of the crop box from the desktop texture
    (`resource.cast::<ID3D11Texture2D>()`) into the ring texture (own helper in `dda.rs`; do NOT edit `gpu.rs`),
    then `ReleaseFrame`, then publish under `wgc::lock_counted(&shared,&errors)`: `seq += 1`, `width=w`, `height=h`,
    `texture = Some(SharedTexture(dst))`.
  - Always `ReleaseFrame` after a successful acquire, on every path.
- If the window's monitor changes (`MonitorFromWindow` differs), treat like ACCESS_LOST: rebuild
  for the new monitor; if that fails `note_error` and keep retrying every 250 ms.
- Before the first frame, set `lat.width/height` to the initial crop size (like `wgc::setup`).
- Teardown: drop duplication/ring before the thread exits. No WinRT, no RoInitialize.

Reuse (all `pub(crate)` already): `wgc::{note_error, lock_counted, accept_frame, qpc_now, qpc_frequency}`,
`gpu::{TextureRing, SharedTexture}`, `crate::{lock, CaptureError, Shared}`.
Own error mapper (wgc's `werr` is private): `derr(context) -> impl Fn(windows::core::Error) -> CaptureError`.

## Engine (`crates/engine`)

- `RecorderConfig.no_border_capture: bool` (done by orchestrator).
- `native_pipeline::start_native_capture_with(hwnd, fps, params, audio, backend: CaptureBackend)`;
  `start_native_capture(..)` delegates with `CaptureBackend::Wgc` (signature unchanged: tests use it).
  After the encoder starts, if `backend` is `NoBorder` call `WindowCapturer::start_gpu_with(.., backend)`;
  on `Err(e)` log `eprintln!("clipcheck capture: no-border capture unavailable ({e}); using window capture")`
  and retry with `CaptureBackend::Wgc` on the same device. Only if that also fails is it an error.
  `backend == Wgc` must not log or touch anything new.
- `recorder/helpers.rs::start_capture` passes `CaptureBackend::NoBorder{require_cover:true}` when
  `cfg.no_border_capture` else `Wgc`, through `start_native_capture_with`.

## UI (`apps/ui`)

`store.ts` Video type: `no_border_capture: boolean`. `video.ts` default `false`. In
`VideoTab.svelte`, inside the existing `<details class="advanced">`, after "Show the mouse cursor":
`FieldRow label="Capture Method" path="video.no_border_capture"` with a `Segmented` control ("Standard Capture" = false, "Experimental Capture" = true), same style as the GPU/CPU encoder choice.
Help: "Removes the coloured outline Windows draws around a recorded game. Windows 10 only needs
this; Windows 11 already hides it. It records the screen area under the game instead of the window,
so a notification or window on top of the game will appear in your clips. Only used when the game
is borderless fullscreen; otherwise normal capture is used."

## Acceptance

- Default config / flag off: `no_border_attempts()` stays 0 across a full WGC recording.
- Flag on + covering window: frames match the window pixels; no WGC session created.
- Flag on + non-covering window or DDA failure: recording still works through WGC.
- fmt, clippy -D warnings, test-all.sh, UI build + vitest green.

## Not verified by tests

Absence of the border on a real Windows 10 machine (Windows 11 here already hides it). Needs a
tester on Windows 10.
