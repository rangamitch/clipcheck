# 03 — `encode` crate: encoder probing, selection, rate control, GPU path, FFmpeg

Status: draft for review · Date: 2026-10-09 · Crate: `crates/encode/` · Depends on: `core` (shared types), `capture` (D3D11 texture input), `buffer` and `session` (consumers) · Brief: `00-brief.md`

This doc elaborates the "Encoding policy" and "Core data pipeline" sections of the brief. Where it adds a decision the brief does not state, the decision is marked **[new]**. Conflicts with the brief are listed in section 12.

---

## 1. Goal & scope

### Goal
Turn the captured game window (D3D11 BGRA GPU texture) into a single, muxer-agnostic stream of encoded video packets, using the best working encoder on the user's machine, at the quality the user chose. The stream is encoded once and fanned out to the replay buffer and the full-session writer.

### In scope
- Encoder probing at service startup, run in a child process, with results cached on disk.
- Encoder backend selection (Auto order and manual override) for H.264, H.265/HEVC and AV1.
- Container choices for video: fragmented MP4 (replay, and optional session) and MKV (session).
- Quality presets Low / Medium / High / Ultra / Custom, mapped to resolution, fps, bitrate and quality values per encoder.
- Rate control: Quality (CQ/CRF/QP), VBR, CBR.
- Forced keyframes at fixed time boundaries so replay segments and session splits are GOP-aligned.
- GPU zero-copy path: BGRA to NV12 conversion with the D3D11 Video Processor, output straight into a hardware encoder's frame pool.
- Failover when an encoder fails mid-session.
- B-frame and lookahead settings.
- In-house FFmpeg loader (DLL loading), FFmpeg version pinning and licensing rules.
- The `EncodedPacket` type contract shared with `buffer` and `session`.

### Out of scope
- Screen capture, frame pacing, black-frame fallback (`capture`).
- Audio encoding (AAC-LC and Opus are encoded in `audio`, which reuses the loader in section 4.6).
- Muxing policy: segment ring layout, clip extraction, split logic (`buffer`, `session`). This doc only states what the encoder guarantees to them.
- Lossless trim and re-encode trim (`clipedit`).
- Software AV1 (see section 12, Q1).
- HDR and 10-bit output. v1 is 8-bit BT.709 limited range.
- Output resolutions above 2160p, and any output above 120 fps.

---

## 2. User-facing behavior

### 2.1 Settings surface (Settings → Video)
| Control | Behavior |
|---|---|
| Codec | H.264 (default), H.265, AV1. Choosing AV1 with a Software backend is rejected at save time with an inline error. |
| Encoder | Auto (default), NVIDIA NVENC, AMD AMF, Intel QSV, Software (x264 / x265). Lists only encoders the last probe reported as working. Unavailable options stay visible, greyed, with the probe reason as a tooltip. |
| Quality preset | Low, Medium, High (default), Ultra, Custom. The preset summary line shows resolution, fps and bitrate. |
| Custom panel | Visible only for Custom. Resolution, fps, rate control, bitrate / max bitrate, CQ. |
| Container (session) | MKV (default) or MP4 (fragmented). Replay segments are always fragmented MP4 and are not user-selectable. |
| Keyframe interval | Read-only in v1 in the UI; the value is editable in config. Shows the replay segment length. |
| Encoder status | Read-only line: "Using NVENC (H.264, zero-copy) · probed 2026-10-08". Shows "Software fallback" in amber when a failover happened this session. |
| Re-probe encoders | Button. Forces a fresh probe, ignoring cache. Disabled while recording. |

### 2.2 Runtime behavior
- **Startup.** The service starts the probe in the background. Recording is possible immediately if a valid cache exists. If no cache exists, the first recording waits up to 10 s for the probe; after that it starts with the software H.264 encoder and shows a banner.
- **Auto.** The service uses the first working backend in the order NVENC > AMF > QSV > Software for the chosen codec. Auto never picks an encoder that the probe did not confirm.
- **Manual override.** If the forced backend is not available for the chosen codec, recording still starts with Auto and emits `encoder.override_unavailable`. The UI shows a warning. The override is not silently dropped.
- **Mid-session failure.** The service fails over per section 5.3. The user sees a toast ("Switched to AMF after NVENC error, part 3 started") and the status line changes. The recording continues.
- **Settings change while recording.** Encoder-affecting settings (codec, backend, preset, rate control, bitrate, resolution, fps, container) are applied at the next session part start, which is the next split or the next session start. Replay buffer picks up the change on the next segment boundary after a buffer restart. The UI says "Applies to the next part".
- **Output timing.** Output frame rate is the target fps. If the game renders below that rate, the capture layer repeats frames. The encoder does not resample.

---

## 3. Architecture & data flow

### 3.1 Module map (inside `crates/encode/`)
```
encode/
  src/
    lib.rs          public API: probe(), select(), Pipeline, re-exports core::media types
    probe/
      mod.rs        probe orchestration, cache read/write, child-process driver
      cache.rs      cache key, JSON schema, atomic write
      child.rs      `clipcheckd --encoder-probe` entry point (runs in child process)
    select.rs       pure selection: ProbeReport + EncodeRequest -> Selected (no I/O)
    preset.rs       preset table, Custom resolution, bitrate ladder, CQ mapping
    options.rs      per-encoder FFmpeg option builders (pure, golden-tested)
    pipeline.rs     encoder thread, bounded queue, keyframe scheduler, stats
    failover.rs     failover state machine (pure, testable with fake encoders)
    gpu/
      convert.rs    BGRA -> NV12 via D3D11 Video Processor, pooled output
      device.rs     adapter enumeration (DXGI), LUID helpers, device-lost detection
    ffi/            (see 4.6) thin FFmpeg loader + generated type bindings
      mod.rs        Ffmpeg handle, LoadLibrary, version checks, function table
      bindings.rs   bindgen output (types only, checked in)
      manifest.rs   bundled-DLL manifest + SHA-256 verification
  build.rs          bindgen (types only) against vendored headers
  third_party/ffmpeg-7.1/include/   pinned headers
```

### 3.2 Data flow
```
                 +--------------------- encode crate -----------------------------------+
 capture        |                                                                       |
 CapturedFrame  |  bounded queue (4)   Keyframe scheduler    Nv12Converter (GPU)       |
 (BGRA, D3D11)--+--> try_push ------->  decides force_idr -->  VideoProcessorBlt       |
                |                                              BGRA -> NV12 pool slot  |
                |                                                     |                |
                |                                                     v                |
                |                          FfmpegEncoder (h264_nvenc | h264_amf | ...) |
                |                          send_frame (AVFrame D3D11, no copy)           |
                |                                                     |                |
                |                                 receive_packet loop (non-blocking)   |
                +-----------------------------------------------------|----------------+
                                                                      v
                                    EncoderOutput { Packet | ParamsChanged | Discontinuity }
                                                                      |
                                              crossbeam channel (bounded 256 packets)
                                                                      |
                                                  session fan-out (Arc<Bytes> clones)
                                                    |                              |
                                          buffer (replay ring)            session writer
```

### 3.3 Threads
| Thread | Owner | Work | Priority |
|---|---|---|---|
| Capture callback | `capture` | Pushes `CapturedFrame` into the encode queue. Never blocks. | Highest (WGC) |
| Encode thread | `encode::pipeline` | Pops frame, runs converter, submits to encoder, drains packets | `THREAD_PRIORITY_ABOVE_NORMAL` |
| Probe child | `clipcheckd --encoder-probe` | Full probe, writes JSON, exits | `BELOW_NORMAL`, idle I/O |
| Fan-out | `session` | Clones `Bytes` into buffer and session sinks | Normal |

The encode thread is the only thread that calls FFmpeg encode functions for a given pipeline. There is one pipeline per recording (one per game session).

### 3.4 Why the probe runs in a child process [new]
An NVENC or AMF driver crash or hang during probing would take down the resident service, which has to stay under 15 MB RSS and must not hold FFmpeg loaded while idle. A child process isolates the risk, loads FFmpeg only for the probe, and exits. The probe is started as `clipcheckd.exe --encoder-probe --ffmpeg-dir <dir> --out <tmp.json>`. The service reads the JSON when the child exits and renames the temp file into the cache.

---

## 4. Key types, traits and function signatures

### 4.1 Shared media types (`crates/core/src/media.rs`) [new placement]
The brief says `core` holds shared types. `EncodedPacket` is the contract between `encode`, `buffer` and `session`, so it lives in `core`. `buffer` and `session` then depend on `core`, not on the encoder.

```rust
use bytes::Bytes;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Codec { H264, Hevc, Av1, Aac, Opus }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StreamId {
    Video,
    /// Audio track number, 1-based, matching the brief's track numbering.
    Audio(u8),
}

/// One encoded access unit (video) or audio frame. Cheap to clone: `data` is refcounted.
#[derive(Clone, Debug)]
pub struct EncodedPacket {
    pub stream: StreamId,
    pub codec: Codec,
    /// Presentation time in 100 ns ticks (WinRT TimeSpan units), on the capture clock.
    pub pts: i64,
    /// Decode time in 100 ns ticks. Equals `pts` when there are no B-frames.
    pub dts: i64,
    /// Duration in 100 ns ticks.
    pub duration: i64,
    /// Random access point. Video: true only for an IDR. Audio: always true.
    pub keyframe: bool,
    /// Video parameter generation (see 4.2). Audio always 0.
    pub generation: u32,
    /// Per-stream sequence number, monotonic. A gap means dropped packets.
    pub seq: u64,
    /// Container-native payload: H.264/H.265 as length-prefixed NAL units (AVCC/HVCC),
    /// AV1 as low-overhead OBU temporal units. Never Annex B.
    pub data: Bytes,
}

/// Parameters a muxer needs to write a sample description (avcC / hvcC / av1C).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoParams {
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    /// avcC, hvcC or av1C record.
    pub extradata: Bytes,
    pub generation: u32,
}
```

### 4.2 Encoder output contract (`encode::pipeline`)
```rust
pub enum EncoderOutput {
    Packet(EncodedPacket),
    /// New parameter sets. Sinks must write a new init segment (fMP4) or a new track header (MKV)
    /// before the next packet with this generation.
    ParamsChanged(VideoParams),
    /// The encoder was replaced, or its codec changed. The first packet of the new
    /// generation is always a keyframe. Sinks must treat this as a split point.
    Discontinuity { new_generation: u32, reason: DiscontinuityReason },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiscontinuityReason { EncoderFailover, CodecFallback, ResolutionChange }
```

### 4.3 Requests, presets, rate control
```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodecChoice { H264, Hevc, Av1 }                       // maps to Codec::{H264,Hevc,Av1}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendPref { Auto, Nvenc, Amf, Qsv, Software }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EncoderId {
    H264Nvenc, HevcNvenc, Av1Nvenc,
    H264Amf,   HevcAmf,   Av1Amf,
    H264Qsv,   HevcQsv,   Av1Qsv,
    X264, X265,                                                   // no software AV1 in v1
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QualityPreset { Low, Medium, High, Ultra, Custom }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RateControlChoice { Quality, Vbr, Cbr }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolutionChoice { Native, P1440, P1080, P720 }     // height cap, see 5.2

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fps { F30 = 30, F60 = 60, F120 = 120 }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RateControl {
    /// Constant quality. CQ 0 (best) to 51 (worst), H.264 basis, offset per codec/encoder.
    Quality { cq: u8 },
    /// Variable bitrate with a target and a peak.
    Vbr { target_kbps: u32, max_kbps: u32 },
    /// Constant bitrate.
    Cbr { kbps: u32 },
}

#[derive(Clone, Debug)]
pub struct CustomSettings {
    pub resolution: ResolutionChoice,
    pub fps: Fps,
    pub rate: RateControlChoice,       // Quality | Vbr | Cbr, bitrate/cq values below
    pub bitrate_kbps: u32,             // 1_000..=100_000
    pub max_bitrate_kbps: u32,         // >= bitrate_kbps, <= 150_000
    pub cq: u8,                        // 0..=51
}

/// Fully resolved request for the encoder. Produced from settings + capture size.
#[derive(Clone, Debug)]
pub struct EncodeRequest {
    pub codec: CodecChoice,
    pub backend: BackendPref,
    pub width: u32,                    // even, already resolved from preset / custom + capture size
    pub height: u32,                   // even
    pub fps: Fps,
    pub rate: RateControl,
    pub keyframe_interval_ms: u32,     // from encode.keyframe_interval_ms
    pub bframes: u8,                   // 0..=4, clamped per encoder by the probe
    pub lookahead: bool,               // hardware only
    pub software_preset: SoftwarePreset,
    pub software_threads: u8,
    pub zero_copy: bool,               // from encode.zero_copy
    pub color: ColorSpec,              // fixed: BT.709, limited range, 8-bit
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoftwarePreset { Veryfast, Faster, Fast, Medium }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColorSpec { pub primaries_bt709: bool, pub limited_range: bool }
```

### 4.4 Probing and selection
```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterInfo {
    pub luid: (u32, i32),              // LUID as stored by DXGI (LowPart, HighPart)
    pub vendor_id: u32,                // 0x10DE NVIDIA, 0x1002 AMD, 0x8086 Intel
    pub device_id: u32,
    pub driver_version: u64,           // DXGI DriverVersion
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncoderCaps {
    pub id: EncoderId,
    pub available: bool,
    /// Why unavailable, or the probe error. Empty when available.
    pub reason: String,
    pub probe_ms: u32,                 // time to first packet
    pub zero_copy_ok: bool,            // D3D11 frames accepted directly
    pub forced_idr_ok: bool,           // forced keyframe produced an IDR
    pub max_bframes: u8,               // 0 when B-frames failed in the probe
    pub max_width: u32,                // verified at 1920 or 3840 (see 5.1)
    pub max_height: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FfmpegBuild {
    pub avcodec_major: u32,            // expect 61 (FFmpeg 7.1)
    pub avutil_major: u32,             // expect 59
    pub version: String,               // e.g. "7.1.1"
    pub manifest_sha256: String,       // digest of the bundled manifest, empty if custom dir
}

#[derive(Clone, Debug)]
pub struct ProbeReport {
    pub schema_version: u32,           // PROBE_SCHEMA = 1
    pub adapter: AdapterInfo,          // the adapter the probe ran on
    pub ffmpeg: FfmpegBuild,
    pub probed_at_unix_ms: i64,
    pub entries: Vec<EncoderCaps>,     // one per EncoderId that was attempted
}

/// Pure function. No I/O. Returns the encoder id to use, or why none is usable.
pub fn select(report: &ProbeReport, req: &EncodeRequest) -> Result<Selection, SelectError>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    pub id: EncoderId,
    pub codec: CodecChoice,            // may differ from request after CodecFallback
    pub zero_copy: bool,               // false when the probe reported no D3D11 acceptance
    pub notices: Vec<SelectNotice>,    // OverrideUnavailable, CodecFallback, CpuPath, ...
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SelectNotice {
    OverrideUnavailable { wanted: BackendPref, used: EncoderId },
    CodecFallback { wanted: CodecChoice, used: CodecChoice },
    CpuPath { id: EncoderId, reason: String },
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SelectError {
    #[error("no working encoder for {0:?}")]
    NoEncoder(CodecChoice),
    #[error("requested resolution {w}x{h} exceeds verified maximum for {id:?}")]
    ResolutionUnsupported { id: EncoderId, w: u32, h: u32 },
}

/// Runs the probe in a child process and reads back the report. Blocking, call from a worker thread.
pub fn probe_blocking(ffmpeg_dir: &Path, adapter: &AdapterInfo, timeout: Duration) -> Result<ProbeReport, ProbeError>;

/// Returns the cached report if its key matches the current machine and FFmpeg build.
pub fn load_cache(path: &Path, key: &CacheKey) -> Option<ProbeReport>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheKey {
    pub luid: (u32, i32),
    pub vendor_id: u32,
    pub device_id: u32,
    pub driver_version: u64,
    pub ffmpeg_version: String,
    pub manifest_sha256: String,
    pub schema_version: u32,
}
```

### 4.5 Pipeline and encoders
```rust
pub struct PipelineConfig {
    pub request: EncodeRequest,
    pub selection: Selection,
    pub keyframe_origin_100ns: i64,    // part start pts; boundaries are aligned to this origin
    pub failover_budget: FailoverBudget,
}

pub struct FailoverBudget { pub max_per_window: u8, pub window: Duration }  // default 3 per 10 min

/// Input to the encoder. `Gpu` is the zero-copy path; `Cpu` is the fallback path.
pub enum EncoderInput {
    Gpu(PooledNv12),                   // AVFrame with D3D11 format, texture + array slice
    Cpu(Nv12Frame),                    // system-memory NV12, planes packed
}

pub trait VideoEncoder: Send {
    fn id(&self) -> EncoderId;
    fn params(&self) -> &VideoParams;
    /// Submits one frame. Returns Ok(()) when accepted (not necessarily encoded yet).
    fn send(&mut self, input: EncoderInput, pts_100ns: i64, force_idr: bool) -> Result<(), EncodeError>;
    /// Drains all currently available packets into `out`. Never blocks.
    fn receive(&mut self, out: &mut Vec<EncodedPacket>) -> Result<(), EncodeError>;
    /// Signals end of stream and drains everything that remains.
    fn flush(&mut self, out: &mut Vec<EncodedPacket>) -> Result<(), EncodeError>;
}

#[derive(Debug, thiserror::Error)]
pub enum EncodeError {
    /// Recoverable: retry the same encoder (for example NVENC session limit reached by another app).
    #[error("transient encoder error ({code}): {msg}")]
    Transient { code: i32, msg: String },
    /// The D3D11 device was removed or reset (TDR). Requires device recreation.
    #[error("GPU device lost")]
    DeviceLost,
    /// The encoder cannot continue (init failure, invalid option, allocation failure).
    #[error("fatal encoder error ({code}): {msg}")]
    Fatal { code: i32, msg: String },
    #[error("encoder failover chain exhausted")]
    ChainExhausted,
    #[error("invalid configuration: {0}")]
    Config(String),
}

/// Public handle used by `session` and `buffer` wiring.
pub struct Pipeline { /* encode thread, sender, queue, stats */ }

impl Pipeline {
    pub fn start(cfg: PipelineConfig, gpu: Arc<GpuDevice>, ffmpeg: Arc<Ffmpeg>,
                 out: crossbeam_channel::Sender<EncoderOutput>) -> Result<Self, EncodeError>;
    /// Called from the capture callback. Never blocks. Returns Dropped when the queue is full.
    pub fn submit(&self, frame: CapturedFrame) -> SubmitOutcome;
    /// Flushes encoders, emits remaining packets, then joins the encode thread.
    pub fn stop(self) -> Result<PipelineStats, EncodeError>;
    pub fn stats(&self) -> PipelineStats;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmitOutcome { Queued, Dropped }

/// Produced by `capture`. The texture is BGRA8 on `GpuDevice`'s adapter.
pub struct CapturedFrame {
    pub texture: windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    pub system_relative_100ns: i64,    // WGC SystemRelativeTime, 100 ns ticks
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PipelineStats {
    pub frames_in: u64,
    pub frames_encoded: u64,
    pub frames_dropped_queue: u64,
    pub frames_dropped_failover: u64,
    pub failovers: u32,
    pub generation: u32,
    pub active: Option<EncoderId>,    // None until the first encoder is open
    pub zero_copy: bool,
    pub forced_idr: bool,             // false when the active encoder ignores forced keyframes
}
```

### 4.6 FFmpeg loader (`encode::ffi`)
Design choice [new]: a hand-written loader plus bindgen-generated type definitions. We do not link FFmpeg import libraries. All functions are resolved with `GetProcAddress`.

```rust
pub struct Ffmpeg {
    module_avcodec: HMODULE,
    module_avutil: HMODULE,
    pub avcodec: AvcodecFns,           // avcodec_send_frame, avcodec_receive_packet, ...
    pub avutil: AvutilFns,             // av_hwdevice_ctx_create, av_hwframe_*, av_opt_set, ...
    pub build: FfmpegBuild,
}

impl Ffmpeg {
    /// Loads avutil and avcodec from `dir` with LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR.
    /// Checks major versions against the pin, then (bundled dir only) verifies the manifest hashes.
    pub fn load(dir: &Path) -> Result<Arc<Ffmpeg>, FfmpegLoadError>;
}

pub static FFMPEG_PIN: FfmpegPin = FfmpegPin {
    version: "7.1.1",
    avcodec_major: 61,
    avutil_major: 59,
    // Configure flags are recorded in ffmpeg/BUILD.txt and checked by the build script.
    build_flags: &["--enable-gpl", "--enable-shared", "--disable-static", "--disable-everything",
                   "--enable-nvenc", "--enable-amf", "--enable-libvpl",
                   "--enable-libx264", "--enable-libx265",
                   "--enable-decoder=h264,hevc,av1", "--enable-parser=h264,hevc,av1"],
};
```

- Bindings are generated once by `build.rs` with bindgen against the vendored headers. Only types and constants are generated, with no `extern` blocks. Layout tests stay enabled, so a header drift breaks the build, not runtime.
- Calls go through the function table. A missing symbol is a `FfmpegLoadError::MissingSymbol` at load time, not a crash at call time.
- `avformat` is loaded by `buffer` and `session` through the same `Ffmpeg` handle. The loader opens `avformat-61.dll` lazily and only when `Ffmpeg::format()` is called. `encode` never uses it.

### 4.7a Loading rules (DLL search)
1. Load only from the bundled `<exe_dir>\ffmpeg\` or the absolute `encode.ffmpeg_dir`. Never from PATH, the current directory, or the user's Downloads folder.
2. Use `LoadLibraryExW` with `LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR` and `AddDllDirectory(dir)` so that `avcodec` resolves its `avutil` dependency inside that directory only.
3. For the bundled directory, verify each DLL's SHA-256 against `ffmpeg\manifest.json` once, on first load. The manifest is installed with the DLLs and its digest is recorded in `FfmpegBuild.manifest_sha256`.
4. Version gate: `avcodec_version() >> 16 == 61` and `avutil_version() >> 16 == 59`. Any other value is a load error.

### 4.8 Licensing and distribution [new]
| Component | License | How it ships | Obligations |
|---|---|---|---|
| FFmpeg (built with `--enable-gpl`) | GPL-2.0-or-later (GPL build) | Shared DLLs in installer | Provide the exact FFmpeg source for the shipped build, the configure line, and any patches. Put a written source offer and the GPL text in the installer and About dialog. |
| libx264 | GPL-2.0-or-later | Compiled into the FFmpeg DLL | Same as FFmpeg: source and build config. |
| libx265 | GPL-2.0-or-later | Compiled into the FFmpeg DLL | Same as FFmpeg: source and build config. |
| libvpl (QSV dispatcher) | MIT | Compiled into the FFmpeg DLL; the runtime comes from the Intel driver | Include the MIT notice. |
| nv-codec-headers (NVENC) | MIT | Headers only, compiled in | Include the MIT notice. Runtime `nvEncodeAPI64.dll` comes from the NVIDIA driver and is not redistributed. |
| AMF headers | MIT | Headers only, compiled in | Include the MIT notice. Runtime `amfrt64.dll` comes from the AMD driver and is not redistributed. |
| ClipCheck itself | GPL-3.0-or-later | Installer | Compatible with the GPL FFmpeg build. |

Rules:
- The GPL build is the only build shipped. An LGPL-only build cannot include x264 or x265, so there is no LGPL variant in v1.
- ClipCheck's source repository must contain the FFmpeg build script, the configure output, and the pinned source tarball hash. The installer's About page links to that location.
- H.264 and HEVC are covered by patent pools. The FFmpeg build includes these encoders. See section 12, Q11.
- AV1 is royalty-free, but its encoders are only used through hardware in v1, so there is nothing to ship for AV1 software.
- Hardware encoder runtimes are provided by the user's GPU driver under the vendor's licence. ClipCheck does not redistribute them.

### 4.7 GPU helpers (`encode::gpu`)
```rust
pub struct GpuDevice {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub video_device: ID3D11VideoDevice,
    pub video_context: ID3D11VideoContext,
    pub adapter: AdapterInfo,
}

pub struct Nv12Converter { /* VideoProcessor, enumerator, cached input/output views per slot */ }

pub struct PooledNv12 {
    frame: AvFrameRef,                 // holds the FFmpeg hwframe reference; pool slot returns on drop
    pub slot: u32,
}

pub struct Nv12Frame { pub width: u32, pub height: u32, pub planes: Box<[u8]> }
```

---

## 5. Algorithms and state machines

### 5.1 Probe (runs in child process)
```
PROBE(ffmpeg_dir, adapter):
  1. Ffmpeg::load(ffmpeg_dir). On error: write report with ffmpeg error, all entries unavailable, exit 0.
  2. Vendor filter:
       NVIDIA adapter (0x10DE) -> [H264Nvenc, HevcNvenc, Av1Nvenc]
       AMD adapter   (0x1002) -> [H264Amf,   HevcAmf,   Av1Amf]
       Intel adapter (0x8086) -> [H264Qsv,   HevcQsv,   Av1Qsv]
       always         -> [X264, X265]
     Hardware candidates whose vendor does not match the probed adapter are not attempted.
  3. For each candidate, with a per-candidate timeout of 3 s (thread + join timeout):
       a. Create D3D11 device on `adapter` (the same LUID the capture uses).
       b. Create an AVHWDeviceContext of type d3d11va wrapping that device; create an AVHWFramesContext
          (format D3D11, sw_format NV12, 1280x720, pool 4).
       c. Open the encoder with the options from options.rs for
          {codec, 1280x720, 60 fps, rc=Quality cq=28, bframes=2, keyint_frames=600}.
       d. Submit 12 frames. Frame 6 has force_idr = true. Use solid colour frames (not black).
       e. Flush. Expect >= 2 packets. Expect at least 1 keyframe at pts 0 and one at frame 6.
       f. Record probe_ms (time to first packet), forced_idr_ok, max_bframes = 2 if reorder observed.
       g. Repeat the open with the D3D11 path turned off (CPU NV12 input). If the D3D11 path failed
          and the CPU path works: available = true, zero_copy_ok = false, reason = "cpu path only".
       h. Verify size: open at 1920x1080 (required), then at 3840x2160 (optional). Set max_width and max_height from the largest size that opened and produced packets.
  4. X264 / X265: open libx264 / libx265 at 1280x720, veryfast, crf 23. Available only if the build
     has the encoder ("not built in" otherwise).
  5. Write ProbeReport (schema v1) to --out. Exit 0.
  Hard limits: the whole probe finishes within 15 s. Overtime candidates are marked unavailable
  with reason "probe timeout" and the process exits anyway.
```

### 5.2 Preset resolution
Input: `QualityPreset`, `CustomSettings` (only for Custom), capture size `(cw, ch)`, `codec`.

```
RESOLVE(preset, custom, cw, ch, codec):
  row = PRESET_TABLE[preset]            # Low/Medium/High/Ultra
  if preset == Custom: row = custom-derived row
  # Resolution: height cap, aspect preserved, never upscale
  cap_h = match row.res { Native -> 2160 ; P1440 -> 1440 ; P1080 -> 1080 ; P720 -> 720 }
  h = min(ch, cap_h); w = round(h * cw / ch)
  if h == ch: w = cw                    # no scaling when the cap is not hit
  w = w & !1; h = h & !1                # NV12 needs even dimensions
  # Fps:
  fps = row.fps
  # Bitrate ladder (H.264 basis), scaled by codec:
  scale = { H264: 1.00, Hevc: 0.65, Av1: 0.50 }[codec]
  target = round(row.target_kbps * scale, to 100 kbps)
  max    = round(row.max_kbps * scale, to 100 kbps)
  rate   = QualityPeak { cq, max = scaled preset bitrate } for presets (cq: H.264 basis below plus the 6.3 offset); custom uses its own rate control.
  cq     = row.cq_h264 + CQ_OFFSET[codec][backend] (section 6.3)
  return (w, h, fps, rate, cq)
```

**Preset table (H.264 basis; default rate control VBR)**

The shipped default is High (schema.rs `Preset::High`).

| Preset | Resolution (height cap) | FPS | Target kbps | Max kbps | Quality CQ / CRF / ICQ / QP for H.264 (see 6.3) |
|---|---|---|---|---|---|
| Low | 720p | 30 | 6 000 | 8 000 | 32 |
| Medium | 1080p | 60 | 12 000 | 15 000 | 29 |
| High (default) | 1440p | 60 | 20 000 | 25 000 | 26 |
| Ultra | Native (cap 2160p) | 60 | 40 000 | 50 000 | 23 |
| Custom | user | user | user | user | user |

Values by codec after scaling (kbps target / max):

| Preset | H.264 | H.265 (x0.65) | AV1 (x0.5) |
|---|---|---|---|
| Low | 6 000 / 8 000 | 3 900 / 5 200 | 3 000 / 4 000 |
| Medium | 12 000 / 15 000 | 7 800 / 9 800 | 6 000 / 7 500 |
| High | 20 000 / 25 000 | 13 000 / 16 300 | 10 000 / 12 500 |
| Ultra | 40 000 / 50 000 | 26 000 / 32 500 | 20 000 / 25 000 |

Resolution examples for a 2560x1440 capture: Native = 2560x1440; 1440p = 2560x1440; 1080p = 1920x1080; 720p = 1280x720. For a 3440x1440 ultrawide capture at 1080p cap: 2586x1080 (aspect preserved, even).

### 5.3 Encoder failover state machine
```
                       submit/receive OK
          +-------------------------------------------+
          |                                           |
          v            Transient                      |
     +---------+ -------------------------> +----------+   retry ok (<= 2 in 5 s)
     | RUNNING |                            | RETRYING | -------------------------+
     +---------+ <------------------------- +----------+                          |
          |  ^                                  |                                  |
          |  |                                  | retries exhausted                |
          |  |                                  | or Fatal / DeviceLost            |
          |  |                                  v                                  |
          |  |                           +------------+  next candidate in chain  |
          |  |                           | FAILOVER   |  (or same encoder on a    |
          |  |                           |            |   new device if DeviceLost)|
          |  |                           +------------+                           |
          |  |                                  |                                  |
          |  |          budget exceeded or chain | exhausted                       |
          |  |                                  v                                  |
          |  |                           +------------+                           |
          |  +---------------------------|  OPEN NEXT |<--------------------------+
          |     open ok: gen += 1,       +------------+
          |     emit Discontinuity;            | open fails -> next candidate
          |     first frame forced IDR         |
          |                                    v
          |                           +------------+
          +-------------------------- |  FAILED    |  emit EncodeError::ChainExhausted,
                                      +------------+  session stops (sinks finalize)
```

Rules:
1. On a transient error, retry the same encoder, same device, with 200 ms backoff. Two retries are allowed within any 5 s window.
2. On `DeviceLost`, recreate the D3D11 device (same adapter LUID), the converter and all pools, then reopen the same encoder. Count this as one failover.
3. On `Fatal` or exhausted retries, move to the next candidate in the chain (section 5.4). The chain is built from the probe, not from the original request.
4. The failover budget is `max_per_window` (default 3) per 10 min. Exceeding it moves to FAILED even if the chain has candidates left, so a flapping encoder cannot loop.
5. Opening the next encoder: the first frame sent is forced IDR. Frames arriving during the swap are dropped and counted in `frames_dropped_failover`. The old encoder is flushed on a best-effort basis and its remaining packets are emitted with the old generation.
6. Each successful swap emits `Discontinuity { new_generation, reason: EncoderFailover }` before the first packet of the new generation. Sinks split at this point.
7. Codec change (AV1 chain exhausted, fallback to H.264): handled as an encoder swap with `reason: CodecFallback`. The chain for the fallback codec is then used. This happens at most once per session.

### 5.4 Chain construction (pure)
```
CHAIN(codec, backend_pref, report):
  if backend_pref != Auto:
      first = [encoder_for(codec, backend_pref)]          # may be None (AV1 + Software)
      rest  = AUTO_ORDER(codec) minus first
  else:
      first = []
      rest  = AUTO_ORDER(codec)

  AUTO_ORDER(codec):
      NVENC, AMF, QSV, SOFTWARE   (SOFTWARE = X264 for H264, X265 for HEVC, none for AV1)

  keep only ids where report.entries[id].available
  keep only ids that cover the requested resolution (max_width/max_height >= w/h)
  if zero_copy requested: put ids with zero_copy_ok first within each vendor group (no removal)
  remove ids on another LUID (cross-adapter encoders cannot take zero-copy frames)

  chain = first_ok + rest_ok
  if chain is empty and codec == Av1: return CHAIN(H264, backend_pref=Auto, report)
                                         with notice CodecFallback
  return chain
```

Note on "Auto selects the first working backend". Auto does not benchmark quality or speed. It takes the vendor order from the brief. A user who wants a different order uses the override.

### 5.5 Keyframe scheduler (forced IDR at fixed boundaries)
```
STATE: origin (part start pts), keyint (= encode.keyframe_interval_ms in 100 ns), next_boundary

START(part):
    origin = first frame pts
    next_boundary = origin + keyint
    ENCODER.g (GOP ceiling) = 2 * keyint in frames            # safety net only
    ENCODER.keyint_min = 1                                    # IDR-driven, not GOP-driven

ON_FRAME(pts):
    force_idr = false
    if pts >= next_boundary:
        force_idr = true
        # resync if we fell behind (stall): align to the grid, do not emit a burst of IDRs
        k = floor((pts - origin) / keyint) + 1
        next_boundary = origin + k * keyint
    send(frame, pts, force_idr)
```

Consequences:
- Every replay segment starts with exactly one IDR, and the segment length equals `keyint` (+/- one frame at the boundary).
- The grid is anchored to the part origin, so a failover keeps the same boundary times because the new encoder's first frame is forced IDR and the grid stays the same.
- A session split waits for the next boundary, so the maximum split latency is `keyint` (2 s by default).
- Encoders without forced-IDR support (`forced_idr_ok = false`): the segment boundary becomes "next natural keyframe" with `g = keyint in frames`. `buffer` is told via the `PipelineStats` flag `forced_idr = false` and logs a warning. Clip accuracy drops to one GOP.

### 5.6 Per-frame flow (encode thread)
```
loop:
  frame = queue.pop_blocking(timeout 100 ms)   # drops a frame if the queue is over 4
  if none: drain encoder (receive) ; continue
  if DeviceLost pending: failover(DeviceLost)
  pts = normalize(frame.system_relative_100ns)   # clamp: pts = max(pts, last_pts + 1)
  if converter == GPU:
       nv12 = converter.convert(frame, target_w, target_h)   # VideoProcessorBlt, no CPU copy
       input = Gpu(nv12)
  else:
       nv12 = converter.convert_and_read(frame)              # staging copy, CPU path
       input = Cpu(nv12)
  force = scheduler.on_frame(pts)
  match encoder.send(input, pts, force):
       Ok -> ;
       Err(Transient) -> retry rules (5.3)
       Err(DeviceLost) -> failover(DeviceLost)
       Err(Fatal) -> failover(Fatal)
  encoder.receive(out)  -> forward packets to channel (bounded 256). If channel full:
       block up to 50 ms, then drop the packet and set sink_backpressure = true (logged, counted)
```

Timestamp normalization: WGC timestamps can go backwards after a pause. The encoder requires strictly increasing pts, so `pts = max(pts, last_pts + 1)`. The 1-tick adjustment is invisible at 100 ns resolution.

### 5.7 Converter (BGRA to NV12, zero copy)
```
INIT (once per device, per target size):
  video_device.CreateVideoProcessorEnumerator(content desc: progressive, input W x H, output w x h)
  video_device.CreateVideoProcessor(enumerator, rate_conversion_index 0)
  VideoProcessorSetStreamColorSpace(input = RGB full range sRGB)
  VideoProcessorSetOutputColorSpace(output = YCbCr BT.709 limited)
  pool = FFmpeg AVHWFramesContext(format D3D11, sw NV12, w x h, initial_pool_size = POOL_SIZE)
         BindFlags = RENDER_TARGET | SHADER_RESOURCE | VIDEO_ENCODER   (verify in spike, task 2)
  for each pool slot: create and cache a VideoProcessorOutputView (NV12 texture array slice)

PER FRAME:
  input_view = cached per WGC texture (keyed by texture pointer; cache size 4)
  slot = av_hwframe_get_buffer(pool)           # no copy
  VideoProcessorBlt(input_view -> output_view[slot]) # scales and converts on GPU
  return PooledNv12 { frame: slot, slot }

CPU PATH (software encoder or zero_copy=false):
  after Blt, CopyResource(output texture -> staging texture, NV12)
  Map(staging, READ), copy rows to Nv12Frame.planes, Unmap
```

POOL_SIZE = encode queue depth (4) + reordering depth + 2 spare, where reordering depth = bframes + (lookahead ? 20 : 0) + 1. Examples: default bframes 2, no lookahead -> 4 + 3 + 2 = 9. With lookahead, 4 + 23 + 2 = 29. Allocation larger than 16 slots is refused and the encoder falls back to `lookahead = false`.

VRAM cost of the pool at 9 slots: 1080p NV12 is about 3.1 MB per slot, so 28 MB. 1440p is about 5.5 MB per slot, so 50 MB. 2160p is about 12.4 MB per slot, so 112 MB. The pool is counted against the encode budget (section 9).

### 5.8 Software path
```
x264: threads = software_threads (0 = auto: clamp(cores - 2, 1, 8))
      options: preset=software_preset, tune=none (not zerolatency), bframes=bframes (<=3 default 3),
               rc-lookahead=20, open-gop=0, keyint=g_ceiling, keyint_min=1, aq-mode=1 default
x265: pools=threads, preset=software_preset mapped (veryfast->veryfast, medium->medium), bframes<=4,
      open-gop=0, rc-lookahead=20
Frame rate degradation: if frames_dropped_queue / frames_in > 5% over 10 s, step fps down
      (120 -> 60 -> 30), emit encoder.degraded. Software path only. Hardware never auto-degrades.
```

### 5.9 Encoder option builders (per encoder, pure)
Each builder returns `Vec<(key, value)>` applied via `av_opt_set` on the codec context, plus the codec-level fields (time_base, framerate, width, height, pix_fmt, gop). Golden tests compare the output for each (encoder, rate control) pair.

| Encoder | Rate control: Vbr {t, m} | Rate control: Quality {cq} | Rate control: Cbr {k} | Other |
|---|---|---|---|---|
| `*_nvenc` | `rc=vbr b=t maxrate=m bufsize=2*t` | `rc=vbr cq=cq b=0` | `rc=cbr b=k bufsize=k` | `preset=p4`, `tune=hq`, `forced-idr=1`, `rc-lookahead=0|20`, `bf=bframes`, `b_ref_mode=middle` (H.264/HEVC), `multipass=disabled`, `spatial-aq=1` |
| `*_amf` | `rc=vbr_peak b=t maxrate=m` | `rc=cqp qp_i=cq qp_p=cq+2` | `rc=cbr b=k` | `quality=quality`, `bf=bframes` clamped to probed max (default 0 if unknown), `forced_idr` via frame `pict_type=I` |
| `*_qsv` | `rc=vbr b=t maxrate=m` | `global_quality=cq` (ICQ) | `rc=cbr b=k` | `preset=medium`, `look_ahead=lookahead`, `bf=bframes`, `low_power=0` |
| `x264` | `b=t maxrate=m bufsize=2*m` | `crf=cq` | `b=k minrate=k maxrate=k bufsize=k nal-hrd=cbr` | see 5.8 |
| `x265` | `b=t maxrate=m bufsize=2*m` | `crf=cq + 5` (see 6.3) | `b=k` with `vbv-maxrate=k vbv-bufsize=k` | `repeat-headers=1`, `aud=0` |

Presets use the VBR column. The `cq` value in the Quality column is used only when the active rate control is Quality. Golden tests pin the exact option string for each row.

All encoders:
- `open_gop = 0` (closed GOP).
- `repeat-headers` / global header set so SPS/PPS or VPS/SPS/PPS are in every IDR (fragments are self-contained). Extradata is also exported in `VideoParams`.
- VUI: BT.709 primaries, transfer, matrix; limited range.

---

## 6. Config keys owned by this feature

All keys are under `[encode]` in `%APPDATA%\ClipCheck\config.toml`. Validation runs on load and on reload. Invalid values are rejected and the previous value is kept, with a logged error.

### 6.1 Keys
| Key | Type | Default | Range / values | Notes |
|---|---|---|---|---|
| `encode.codec` | enum string | `"h264"` | `h264`, `hevc`, `av1` | `av1` requires a hardware backend. |
| `encode.backend` | enum string | `"auto"` | `auto`, `nvenc`, `amf`, `qsv`, `software` | Forced override, section 2.2. |
| `encode.preset` | enum string | `"medium"` | `low`, `medium`, `high`, `ultra`, `custom` | Section 5.2. |
| `encode.custom.resolution` | enum string | `"1080p"` | `native`, `1440p`, `1080p`, `720p` | Used only when `preset = "custom"`. |
| `encode.custom.fps` | u8 | `60` | `30`, `60`, `120` | Used only when `preset = "custom"`. |
| `encode.custom.rate_control` | enum string | `"vbr"` | `quality`, `vbr`, `cbr` | |
| `encode.custom.bitrate_kbps` | u32 | `12000` | 1 000 – 100 000 | Target for VBR and CBR, H.264 basis. |
| `encode.custom.max_bitrate_kbps` | u32 | `15000` | >= bitrate, <= 150 000 | VBR peak only. |
| `encode.custom.cq` | u8 | `23` | 0 – 51 | Quality mode, H.264 basis. |
| `encode.keyframe_interval_ms` | u32 | `2000` | 1 000 – 10 000 | Replay segment length, and split granularity. Owned here, read by `buffer` and `session`. |
| `encode.bframes` | u8 | `2` | 0 – 4 | Clamped per encoder by the probe. |
| `encode.lookahead` | bool | `false` | — | Hardware `rc-lookahead`. Software always uses 20. |
| `encode.software_preset` | enum string | `"veryfast"` | `veryfast`, `faster`, `fast`, `medium` | x264 and x265. |
| `encode.software_threads` | u8 | `0` | 0 – 32 | 0 = auto (`clamp(cores - 2, 1, 8)`). |
| `encode.container` | enum string | `"mkv"` | `mkv`, `mp4` | Session container. `mp4` is always fragmented. Replay is always fragmented MP4. |
| `encode.zero_copy` | bool | `true` | — | `false` forces the CPU path. Debug aid. |
| `encode.failover_budget` | u8 | `3` | 0 – 10 per 10 min | `0` = fail on the first fatal error. |
| `encode.ffmpeg_dir` | string | `""` | empty, or absolute path | Empty = bundled `<exe_dir>\ffmpeg\`. Non-empty skips the manifest check and logs a warning. |

### 6.2 Cross-key validation
| Rule | Action |
|---|---|
| `codec = av1` and `backend = software` | Reject. UI shows error. |
| `custom.max_bitrate_kbps < custom.bitrate_kbps` | Reject. |
| `custom.resolution` above the capture size | Allowed. Capped to native at runtime (no upscaling). |
| `lookahead = true` and `backend = software` | Ignored, logged once. |
| Audio codec = Opus and `container = mp4` | Owned by `audio`. Opus is MKV-only, so the session writer forces AAC-LC and logs. Settings save shows a warning. |
| `keyframe_interval_ms` not a whole number of frames at target fps | Allowed. The scheduler works in time, not frames. |

### 6.3 Quality value mapping (per encoder)
Values are initial and are tuned in task 9 against VMAF. The H.264 basis column comes from the preset table.

| Preset | x264 CRF (H.264 base) | NVENC cq (H.264) | QSV ICQ (H.264) | AMF QP_I (H.264) |
|---|---|---|---|---|
| Low | 32 | 32 | 32 | 32 |
| Medium | 29 | 29 | 29 | 29 |
| High | 26 | 26 | 26 | 26 |
| Ultra | 23 | 23 | 23 | 23 |
| Custom | `custom.cq` | `custom.cq` | `custom.cq` | `custom.cq` |

Codec offsets added to the H.264 basis:

| Codec | x264 / x265 | NVENC | QSV | AMF |
|---|---|---|---|---|
| H.264 | +0 | +0 | +0 | +0 |
| H.265 | x265 +5 | +2 | +2 | +2 |
| AV1 | n/a | +3 | +3 | +3 |

Results are clamped to 0–51. AMF writes `qp_p = qp_i + 2` for H.264 and H.265. The H.264 basis values are the single source of truth in the table in 5.2; this table repeats them for the per-encoder columns.

---

## 7. IPC messages / events

Transport: named pipe `\\.\pipe\clipcheck-ipc`, length-prefixed JSON (per brief). The encode feature adds the following. Envelopes follow the shared `ipc` format `{ "type": "...", "id": "...", "payload": {...} }`.

### 7.1 Commands (UI → service)
| Type | Payload | Response |
|---|---|---|
| `encoder.status.get` | `{}` | `encoder.status` (below) |
| `encoder.reprobe` | `{}` | `encoder.probe.started`, then `encoder.probe.completed` or error. Rejected with `busy` while recording. |

### 7.2 Responses and events (service → UI)
| Type | Payload | When |
|---|---|---|
| `encoder.status` | `{ "active": "NVENC", "codec": "h264", "zero_copy": true, "generation": 1, "failovers": 0, "probe": { "adapter": "NVIDIA GeForce RTX 4070", "driver": "566.36", "probed_at": 1759920000000 }, "entries": [ { "id": "h264_nvenc", "available": true, "reason": "" }, ... ] }` | Response to status.get and on change |
| `encoder.probe.started` | `{ "reason": "startup" \| "manual" }` | Probe child launched |
| `encoder.probe.completed` | `{ "ok": true, "available_count": 5, "duration_ms": 3120, "cache_hit": false }` | Probe finished |
| `encoder.selected` | `{ "id": "h264_nvenc", "codec": "h264", "zero_copy": true, "reason": "auto" \| "override", "width": 1920, "height": 1080, "fps": 60, "rate": "vbr", "target_kbps": 12000, "max_kbps": 15000 }` | Each part start |
| `encoder.override_unavailable` | `{ "wanted": "amf", "used": "h264_nvenc" }` | Override not available at start |
| `encoder.codec_fallback` | `{ "wanted": "av1", "used": "h264" }` | AV1 chain empty |
| `encoder.failover` | `{ "from": "h264_nvenc", "to": "h264_amf", "reason": "fatal" \| "device_lost" \| "transient_exhausted" \| "budget_exceeded", "generation": 2, "dropped_frames": 14 }` | Each swap. `budget_exceeded` is followed by `encoder.error`. |
| `encoder.frames_dropped` | `{ "queue": 0, "failover": 14, "window_s": 10 }` | Sent at most every 10 s when non-zero |
| `encoder.degraded` | `{ "fps_from": 60, "fps_to": 30, "reason": "cpu_bound" }` | Software path step-down |
| `encoder.error` | `{ "fatal": true, "code": "chain_exhausted", "message": "..." }` | Pipeline stops |

Settings reload (`settings.reload`, owned by the shared config code) is consumed by the service. Encode validates the `[encode]` section and updates the next-part config. It does not restart a recording by itself.

---

## 8. Edge cases & failure modes

| # | Case | Behavior |
|---|---|---|
| 1 | NVENC session limit reached (another app holds the sessions) | `Transient` error. Two retries in 5 s, then failover to AMF/QSV/x264. Event `encoder.failover` with `transient_exhausted`. |
| 2 | GPU driver TDR or device removed during recording (`DXGI_ERROR_DEVICE_REMOVED`) | `DeviceLost`. Recreate device, converter and pools, reopen the same encoder. Counted as one failover. Discontinuity sent. |
| 3 | Driver updated between sessions | Cache key changes (driver_version). Re-probe on next start. Recording uses the stale cache only if no probe has finished, and the status line says "re-probing". |
| 4 | Game renders on a different GPU than the one with the encoder (hybrid laptop) | Encoders on other LUIDs are removed from the chain. If only cross-adapter encoders exist, use the software path and emit `CpuPath`. Zero-copy is never attempted across adapters. |
| 5 | Probe child hangs or crashes (driver bug) | Parent kills child at 15 s. Unfinished candidates are marked unavailable with `probe timeout` or `probe crashed`. The cache stores the result, so the bad encoder is not retried until the driver key changes. |
| 6 | Forced IDR ignored by an encoder | Probe sets `forced_idr_ok = false`. Scheduler falls back to natural GOP `g = keyint`. Buffer segments may be up to one GOP longer; warning logged. |
| 7 | Window resized mid-part (capture size changes) | Output size is locked for the part. The converter scales to the locked size and letterboxes (black bars) to keep aspect. A `ResolutionChange` split is requested only when the new size is more than 25% different in pixel count; then the session splits at the next boundary. |
| 8 | Odd or tiny capture size (for example 639x359 or 320x240) | Dimensions are rounded down to even. Minimum output is 320x240; below that the encoder runs at 320x240 with letterboxing. Capture black frames still flow through. |
| 9 | Encoder slower than capture (software on weak CPU) | Queue is bounded at 4. Excess frames are dropped at `submit`, never blocking capture. Software path steps fps down after 5% drops for 10 s. Hardware path only counts drops. |
| 10 | Output packet channel full (disk slow, sink stalled) | Encode thread waits up to 50 ms, then drops the packet, counts it and sets backpressure. Dropping a non-key packet creates a decode error until the next IDR. The session treats 2 dropped packets in 1 s as a write stall and surfaces a warning. Keyframes are never dropped: if a keyframe would be dropped, the thread blocks up to 500 ms. |
| 11 | Frame pool exhausted (lookahead too large for VRAM) | Allocation fails. Encoder reopens with `lookahead = false`. If that also fails, Fatal, then failover. |
| 12 | Non-monotonic capture timestamps after pause or capture restart | `pts = max(pts, last + 1)`. Logged once per part if it happens more than 100 times. |
| 13 | AV1 requested on a GPU without AV1 encode (for example RTX 20 series) | AV1 entries are unavailable in the probe. Auto chain empty, falls back to H.264 with `CodecFallback`. |
| 14 | DLL missing, wrong version, or manifest hash mismatch | `Ffmpeg::load` returns an error. Software and hardware are both unavailable, so recording is blocked and the UI shows "Encoder runtime missing or corrupt, reinstall". Recording never falls back to loading DLLs from PATH or the current directory. |
| 15 | Forced override to an encoder that fails at start | Start-time failure is treated as "unavailable" and the next candidate is used. `encoder.override_unavailable` is emitted. |
| 16 | Failover budget exceeded | Pipeline goes to FAILED. Session finalizes its current part and stops, with `encoder.error`. The user can restart recording; the budget resets on a new session. |
| 17 | Power or sleep resume with the device reset | Treated as DeviceLost (case 2). |
| 18 | Software encoder thread oversubscription with the game | Threads capped at `clamp(cores - 2, 1, 8)`, and encode thread runs at `ABOVE_NORMAL`. Game keeps its cores. |
| 19 | Probe finds an encoder but zero-copy fails | Entry is available with `zero_copy_ok = false`. Selection uses the CPU path and emits `CpuPath`. CPU cost is recorded in section 9. |
| 20 | Settings say `custom.resolution = 1440p` but source is 1280x720 | No upscaling. Output is the source size. |

---

## 9. Performance & resource budget

Targets are for a 1080p60 H.264 medium preset on a mid-range desktop (RTX 3060 / RX 6600 / UHD 770 class). Measured on Windows 11 with the game at 100% GPU load.

| Metric | Hardware path (NVENC / AMF / QSV) | Software path (x264 veryfast) | Notes |
|---|---|---|---|
| Encoder CPU (one core share) | < 1 % | 60–110 % at 1080p60 on 6 cores | Brief target is < 3 % on HW encode, which is met. Software cannot meet it; that is expected. |
| Encode thread wall-clock per frame | < 1 ms | 12–16 ms | Software path is the bottleneck at 60 fps. |
| GPU encode engine | 8–15 % | 0 | NVENC or VCN or QuickSync encode engine. |
| GPU copy / Blt | < 1 % | < 1 % (plus staging copy) | Video Processor Blt of 1080p is cheap. |
| System RAM (encode module) | < 4 MB working set | < 12 MB (x264 buffers) | FFmpeg DLLs are not loaded in the service while idle. |
| VRAM (pool) | 9 slots: 28 MB at 1080p, 50 MB at 1440p, 112 MB at 2160p | 0 | Lookahead 20 raises the pool to 29 slots. |
| Encode latency (capture to packet) | p50 ≤ 25 ms, p99 ≤ 60 ms (with B-frames 2) | p50 ≤ 40 ms | Excludes sink I/O. |
| Probe duration | ≤ 6 s typical, ≤ 15 s hard cap | n/a | Background child, BELOW_NORMAL priority. |
| Probe cache file | < 8 KB | | |
| Idle service impact | 0 (no FFmpeg loaded) | 0 | Brief target: service idle < 15 MB RSS. |
| Failover gap | < 1 s typical | < 1 s | Frames dropped during swap, counted. |

Bitrate-to-file-size check: 12 Mbps (Medium H.264) is about 90 MB per minute. A 1-hour session is about 5.4 GB. H.265 Medium is about 3.5 GB/h, AV1 Medium about 2.7 GB/h. This is informational for the UI estimate.

---

## 10. Test plan

### 10.1 Unit tests (no GPU, no FFmpeg)
- `preset::resolve` table tests: every preset x codec x capture size (1920x1080, 2560x1440, 3440x1440, 1280x720, 639x359). Assert even dims, aspect preserved, no upscaling, bitrate scale.
- `options::*` golden tests: for each encoder x rate control mode, the exact `(key, value)` list. Any change in an option string fails the test.
- `select::select` table tests with synthetic `ProbeReport`s: Auto order, forced override available/unavailable, AV1 with no HW (codec fallback), LUID mismatch removal, resolution above verified maximum, zero-copy false ordering.
- `failover` state machine with a fake `VideoEncoder` scripted to return sequences of Transient/Fatal/DeviceLost. Assertions: retry counts, budget exhaustion, generation increments, Discontinuity before first packet, first frame after swap is IDR, `ChainExhausted` at the end.
- `keyframe scheduler`: simulated timestamps including stalls (gap of 5 s), jitter, and exact boundaries. Assert exactly one IDR per boundary and grid alignment after failover.
- `cache`: key mismatch triggers re-probe; corrupt JSON triggers re-probe; atomic rename on crash-safe write (simulate kill before rename).
- `ffi::manifest`: hash mismatch rejected; missing DLL rejected; custom dir skips hash with warning.
- Pool sizing formula, timestamp normalization.

### 10.2 Integration tests (Windows, GPU present)
Run in a self-hosted matrix. Each run encodes a synthetic 10 s clip (moving gradient + frame counter) through the full pipeline and checks output.

| Hardware (matrix) | Required outcome |
|---|---|
| NVIDIA RTX 30xx or 40xx | NVENC H.264, HEVC, AV1 (AV1 only on 40xx). Zero-copy true. Forced IDR true. |
| NVIDIA GTX 16xx / RTX 20xx | AV1 unavailable with reason. H.264 and HEVC OK. CodecFallback when AV1 requested. |
| AMD RX 6000 / 7000 | AMF H.264, HEVC, AV1 (AV1 on RDNA 3+). B-frames clamped to probed max. |
| Intel UHD 770 (iGPU) | QSV H.264, HEVC; AV1 on Arc / 13th-gen+ iGPU depending on driver. |
| Hybrid laptop (Intel iGPU + NVIDIA dGPU) | Chain filtered to the game's adapter. Cross-adapter case produces CpuPath. |
| VM with no GPU | Probe reports all hardware unavailable; X264 available if built. Recording works on software. |
| Software-only CPU, no x264 build | Recording blocked with the runtime-missing message (case 14), not a crash. |

Functional checks on each output file:
- `ffprobe` (test-only tool, not shipped) reports the expected codec, resolution, fps, and profile.
- Every replay segment (fMP4) starts with an IDR. Segment duration is within 1 frame of `keyframe_interval_ms`.
- Concatenating 5 segments produces a playable file with correct timestamps (no negative DTS, no duplicate PTS).
- MKV session file: `mkvmerge --identify` reports track and duration correctly.

### 10.3 Failover and fault injection
- Inject `DeviceLost` by calling `ResetD3D11Device`-equivalent on the test device every 20 s for 5 minutes. Expect continuous output, one failover per injection, and no more than 1 s of gap each time.
- Inject NVENC session exhaustion by holding sessions with a second process. Expect transient retries then failover to next backend.
- Kill the probe child mid-run. Expect the parent to mark entries unavailable, not hang.

### 10.4 Quality and performance
- Quality target (VMAF, 1080p60 Medium, reference clip set of three): VMAF mean >= 92 for H.264 hardware, >= 93 for x264 veryfast at CRF 25. HEVC and AV1 must reach at least the H.264 score at 65% / 50% of the bitrate.
- Performance: run the budget checks in section 9 on the reference rig for 60 minutes with the game at full GPU load. Fail on any metric above target by more than 20%.
- Soak: 8 hours of recording with 2 failovers. No memory growth above 20 MB over the first hour. No leaked D3D11 texture (check with DXGI debug layer counts at stop).

### 10.5 Manual checks
- Probe UX: first launch with no cache, second launch uses cache, driver update triggers re-probe, manual re-probe button works and is disabled during recording.
- Settings: AV1 + Software rejected; Custom values validated; "applies to next part" banner appears when changed mid-recording.

---

## 11. Implementation task breakdown

Each task is about 0.5–2 days. Order matters; each task has a verifiable exit.

| # | Task | Exit criterion |
|---|---|---|
| 1 | Add `core::media` types (`EncodedPacket`, `VideoParams`, `Codec`, `StreamId`) and the serialization of the IPC event shapes | Compiles in workspace; unit test round-trips a packet; no dependency added to `core` beyond `bytes` and `serde` |
| 2 | FFmpeg pin and build. Build the minimal shared FFmpeg 7.1.1 (GPL, nvenc/amf/libvpl/x264/x265, decoders h264/hevc/av1 only). Produce `ffmpeg/manifest.json` with hashes. Write the `third_party/` license bundle and source-offer text (section 4.8) | Build script produces DLL set; `avcodec-61.dll` loads; manifest hashes verified in a test |
| 3 | `ffi` loader and bindgen types (`build.rs`) | Load, version check, resolve all function pointers; missing symbol test; D3D11 video-device spike: the BindFlags for NV12 pool confirmed on NVIDIA and AMD |
| 4 | GPU device helpers: DXGI enumeration, LUID, device-lost detection, D3D11 device creation shared with capture | Lists adapters with vendor/driver; device-removed simulated and detected |
| 5 | `options.rs` option builders plus golden tests for every encoder and rate control | All golden tests pass; no FFmpeg call yet |
| 6 | `preset.rs`: resolution, bitrate ladder, CQ mapping, Custom resolution and validation rules | Table tests from 10.1 pass |
| 7 | Probe: child-process entry point, candidate probing with timeouts, report JSON, cache key and atomic write, parent driver with kill timeout | Probe on the three vendor machines produces correct entries; cache hit/miss test; hang test kills child |
| 8 | `select.rs` and chain construction (pure) with notices | Selection table tests from 10.1 pass; AV1 fallback path tested |
| 9 | Encoder implementation (`FfmpegEncoder` implementing `VideoEncoder`) for the hardware path with D3D11 frames; `Nv12Converter` with VideoProcessor; quality mapping tuned against VMAF | Hardware encode of a synthetic clip on NVIDIA, AMD, Intel; VMAF targets in 10.4 met |
| 10 | Keyframe scheduler and per-frame flow (5.5, 5.6) with bounded queue, timestamp normalization and packet channel backpressure | Segment alignment test: every boundary is an IDR, duration within 1 frame |
| 11 | Software path (x264 / x265) with CPU readback and fps degradation | Software output passes the functional checks; degradation test with an artificially slow encode thread |
| 12 | Failover state machine and pipeline integration (`failover.rs`, `Pipeline::start/submit/stop`), discontinuity events, budget | Fake-encoder tests from 10.1 pass; fault injection 10.3 passes on the reference rig |
| 13 | IPC events and status, settings validation hook, UI status line data | Events from section 7 appear in the UI status line; "applies to next part" logic tested with a live config reload |
| 14 | Performance pass and soak test (section 9, 10.4) | Budgets met on reference rig; 8 h soak clean |

---

## 12. Open questions

1. **AV1 software encoder (conflict with the brief).** The brief lists "SVT-AV1 optional" under the FFmpeg section, but its codec policy says "AV1 (hardware only, optional)". This doc follows the codec policy: no software AV1 in v1. If SVT-AV1 is wanted, it adds about 2 MB to the installer and needs its own CPU budget. Decision needed.
2. **FFmpeg major version.** This doc pins FFmpeg 7.1.x (libavcodec 61). FFmpeg 8.x has a newer ABI, and it is the better long-term choice, but the minimal build and the NVENC/AMF/libvpl support must be verified on it first. Recommend staying on 7.1 for v1 and tracking 8.x for a v1.x bump.
3. **Installer size.** A minimal GPL build (x264, x265, nvenc, amf, libvpl, h264/hevc/av1 decoders for clipedit) is estimated at 14–20 MB of DLLs. The brief's installer budget is 40 MB. If the app plus FFmpeg exceeds that, the options are: (a) ship x264/x265 as an optional download, (b) drop the HEVC software encoder, or (c) drop decoders from the build and move clipedit re-encode to a download. Preferred: (a).
4. **`ffi` as a crate or module.** The brief calls the FFmpeg wrapper a "thin in-house `ffi` wrapper" but the workspace list has no `ffi` crate. This doc puts it at `encode::ffi` and re-exports it to `buffer`, `session` and `audio`. If three crates depend on `encode` only for FFmpeg, promoting it to an `ffi` crate is cleaner. Decision needed.
5. **Placement of `EncodedPacket`.** This doc places it in `core::media`. The alternative is in `buffer` or `session`, which would make `encode` depend on a consumer crate. Recommend `core`; confirm.
6. **Probe as a subcommand of `clipcheckd`.** The probe runs as `clipcheckd --encoder-probe`. The alternative is a separate `clipcheck-probe.exe`, which adds a binary to the installer but keeps the service binary unchanged. Recommend the subcommand.
7. **Failover produces a new part.** On any encoder swap this doc splits the session file (new generation, new init segment). If parameter sets are identical across backends (rare), the split could be skipped. Not worth the complexity in v1; confirm.
8. **Resolution-change policy.** Case 7 letterboxes within a part and splits only for large size changes. The 25% threshold is a guess. Confirm with product, or change to "always letterbox, never split".
9. **AMF B-frames and QP mapping.** Current AMF B-frame support and the `qp_i` / `qp_p` mapping vary across driver generations. The probe clamps B-frames to the verified value, but the CQ offsets in 6.3 need validation on RDNA 2, 3 and 4 hardware before they are final.
10. **QSV across adapters.** On hybrid laptops the QSV encoder may be the Intel iGPU while the game renders on the NVIDIA dGPU. This doc removes it from the chain (case 4). If the product wants Intel QSV as the default on those laptops for battery reasons, the cross-adapter copy cost must be measured first.
11. **Patent and licensing review.** x264 and x265 are GPL. H.264 and HEVC carry patent pools, and the FFmpeg build includes those encoders. ClipCheck is a free, local-only app, but this should get a legal review before any paid distribution. Hardware encoders rely on the user's GPU driver licences and are not redistributed.
12. **Config location for the probe cache.** The cache file is `%LOCALAPPDATA%\ClipCheck\encoder-probe.json`. It is not in the brief's list of locations. It is under the existing `%LOCALAPPDATA%\ClipCheck\` root, so this is an addition, not a change. Confirm the Settings doc treats it as internal state, not a user setting.
13. **Quality target validation.** VMAF >= 92 for 1080p60 Medium at 12 Mbps is a hypothesis. It should be confirmed on the reference clip set in task 9 before the preset table is frozen.
