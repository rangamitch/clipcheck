# FFmpeg removal: implementation contracts (D58)

Goal: the installer ships **no** ffmpeg/ffprobe. Every FFmpeg use is replaced by our own code
(doc 12 phases 1, 3, 4) on top of D57. FFmpeg remains allowed **only as a test oracle** (doc 12 §4.4).

Strategy: replace implementations behind the existing public signatures first (unused `ffmpeg`/`ffprobe`
parameters are renamed `_ffmpeg`/`_ffprobe`), so call sites and tests keep compiling. Wave T then deletes
the FFmpeg code paths, parameters, binaries and packaging.

## Wave R: new code

### R1 `crates/mp4/src/reader.rs` (pure, no unsafe)

```rust
pub struct Movie { pub tracks: Vec<Track>, pub duration_secs: f64 }      // duration = max track duration
pub struct Track {
    pub id: u32,
    pub kind: TrackKind,
    pub timescale: u32,
    pub title: Option<String>,        // trak/udta/"name" (UTF-8, NUL-trimmed), else None
    pub sample_entries: Vec<Vec<u8>>, // each stsd entry as a full box (size+type+payload), verbatim
    pub edit_media_time: i64,         // first non-empty edts/elst media_time (track timescale), 0 if none
    pub samples: Vec<SampleRef>,      // decode order
}
pub enum TrackKind {
    Video { codec: String, width: u32, height: u32 },   // codec: "h264" (avc1/avc3), "hevc" (hvc1/hev1), "av1" (av01), else the fourcc
    Audio { codec: String, channels: u16, sample_rate: u32 }, // "aac" (mp4a), "opus" (Opus), else fourcc
    Other { handler: String },
}
pub struct SampleRef {
    pub offset: u64, pub size: u32,
    pub dts: i64,          // track timescale, first sample = 0 (before edit lists)
    pub cts_offset: i32,
    pub duration: u32,
    pub keyframe: bool,
    pub entry: u32,        // 1-based sample description index
}
impl Track { pub fn duration_secs(&self) -> f64; pub fn is_video(&self) -> bool; pub fn is_audio(&self) -> bool }
pub fn read_movie(path: &Path) -> Result<Movie, Mp4Error>;   // seeks; never reads mdat payloads
pub fn read_movie_from<R: Read + Seek>(r: R) -> Result<Movie, Mp4Error>;
```
- Flat MP4/MOV: stts, ctts (v0 unsigned / v1 signed), stss (absent = all sync), stsc, stsz/stz2, stco/co64.
- Fragmented: moov (mvex/trex defaults) + any number of moof/mdat; tfhd (base-data-offset, default-base-is-moof,
  defaults), tfdt, trun (data offset, first-sample-flags, per-sample fields). Sync = !(flags & 0x0001_0000).
  Without tfdt the decode time continues from the previous fragment.
- A truncated trailing box (file still being written, e.g. the open replay segment) ends parsing cleanly:
  samples of complete fragments are returned, nothing past EOF. Samples whose bytes lie past EOF are dropped.
- Untrusted input: checked arithmetic, box sizes validated against the parent, limits (≤ 10 M samples per track,
  ≤ 64 tracks, ≤ 1 MiB per sample entry); errors are `Mp4Error::InvalidData(String)` (new variant), never panics.

### R2 `crates/mp4/src/remux.rs` (pure, no unsafe)

```rust
pub enum TrackSelect { All, Only(Vec<usize>) }   // indices into the first input's track list
pub struct RemuxOptions {
    pub start_secs: f64,           // on the concatenated timeline; 0 = from the start
    pub end_secs: Option<f64>,     // exclusive, on the concatenated timeline
    pub tracks: TrackSelect,
    pub faststart: bool,           // moov before mdat (always true in the app)
}
pub struct RemuxReport { pub bytes: u64, pub duration_secs: f64, pub actual_start_secs: f64 }
pub fn concat_remux(inputs: &[PathBuf], out: &Path, opts: &RemuxOptions) -> Result<RemuxReport, Mp4Error>;
```
- Track layout must match across inputs (same count, same kinds and codecs in order) else `InvalidData`.
- Timeline: input k is placed at the sum of the previous inputs' durations, where an input's duration is its first
  video track's duration (or the longest track if there is no video). Edit-list media_time is subtracted from dts.
- Cut: the reference track (first selected video track, else first selected track) starts at the last keyframe whose
  pts <= start_secs (or its first sample). `actual_start_secs` = that keyframe's time. Other tracks start at their first
  sample with time >= actual_start_secs. Samples with pts >= end_secs are dropped. Output timeline starts at 0.
- Audio continuity across inputs: append each sample at max(running_end, placed_time); if placed_time is more than
  one sample duration after running_end, extend the previous sample's duration to close the gap; if a sample would
  start more than half a sample duration before running_end, drop it.
- Sample descriptions: per output track, the distinct `sample_entries` (byte-equal dedup) in first-seen order; each
  sample references its entry via stsc.
- Output: `ftyp` (isom, 0x200, isom iso2 avc1 mp41) + `moov` (mvhd/tkhd/mdhd durations, hdlr, vmhd/smhd, dinf,
  stbl: stsd(all entries), stts (run-length), ctts v1 only if any offset != 0, stss only for video when not all
  sync, stsc, stsz, stco or co64 when any offset >= 4 GiB; trak/udta/name for titled tracks) + one `mdat`.
  Chunks: consecutive samples of one track, interleaved by time, ≤ 0.5 s per chunk. Box helpers from `fmp4.rs` may
  be made `pub(crate)` and reused.
- Writes `<out>.part` then renames; removes the part file on error. Never modifies inputs.

### R3 `crates/mfdec` (new crate `clipcheck-mfdec`, FFI only in `mf.rs`, `wic.rs`)

```rust
pub struct MediaProbe { pub duration_ms: i64, pub width: u32, pub height: u32, pub fps: f32,
                        pub video_codec: String, pub audio: Vec<ProbeAudio> }
pub struct ProbeAudio { pub codec: String, pub channels: u32, pub sample_rate: u32 }
pub fn probe(path: &Path) -> Result<MediaProbe, DecError>;        // MF source reader, no decoding
pub struct Frame { pub data: Vec<u8>, pub width: u32, pub height: u32, pub pts: i64 }  // 100 ns
pub enum OutFormat { Bgra, Nv12 }
pub struct VideoDecoder;  // MF source reader with MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, first video stream only
impl VideoDecoder {
    pub fn open(path: &Path, format: OutFormat) -> Result<Self, DecError>;
    pub fn seek(&mut self, secs: f64) -> Result<(), DecError>;      // lands on the keyframe at or before
    pub fn next_frame(&mut self) -> Result<Option<Frame>, DecError>; // None at end of stream; tightly packed
    pub fn width(&self) -> u32; pub fn height(&self) -> u32;
}
pub fn thumbnail_jpeg(path: &Path, at_secs: f64, width: u32, out: &Path) -> Result<(), DecError>;
    // decode the first frame with pts >= at_secs (or the last one before EOF), scale to `width` (even height,
    // aspect kept) with WIC, encode JPEG (quality 0.85) with WIC; writes `<out>.part` then renames.
pub enum DecError { BadInput(String), Unsupported(String) /* missing decoder: name the Store extension */, Windows { context, message } }
```

### R4 `crates/encode/src/probe.rs`: native probe

```rust
pub const NATIVE_BUILD: &str = "media-foundation";
pub fn probe_native() -> ProbeReport;   // never fails; same EncoderInfo shape as today
```
For each `CANDIDATES` entry (backend, codec, ffmpeg_name): map to hwenc (Nvenc→Nvidia, Amf→Amd, Qsv→Intel hardware;
Software→Microsoft software, H.264 only; AV1 and software HEVC → works=false "not supported natively"). works = a
hardware descriptor of that vendor opens and encodes 5 frames of 256x144 NV12 on a fresh D3D11 device (software: system
memory) producing ≥ 1 packet; error = the failure text; probe_ms measured. `ffmpeg_name` keeps the same identifiers
(they are the app-wide encoder ids). `build` = NATIVE_BUILD.

## Wave S: switch call sites (signatures unchanged; FFmpeg args ignored)

| Worker | Owned | Change |
|---|---|---|
| S1 | crates/buffer/src/extract.rs | `concat_publish` parses the concat list it is given (`file '…'` + optional `inpoint`) and calls `concat_remux` (start = inpoint, end = duration). `extract_clip*` call `concat_remux` directly. `audio_title_args*` read titles with `read_movie` and keep their output format. No process is spawned. |
| S2 | crates/engine/src/tracker.rs, catalog.rs | tracker fallback duration = `read_movie(..).duration_secs`; `probe_media` = reader for .mp4/.m4v/.mov (fps from video sample durations), `mfdec::probe` for anything else; `make_thumbnail` = `mfdec::thumbnail_jpeg`. |
| S3 | crates/clipedit/src/run.rs | Lossless = `concat_remux([src], dst, start/end)`. Precise = `mfdec::VideoDecoder` (NV12) from `reencode_from_keyframe_ms`, drop frames before `discard_head_ms`, encode with `hwenc` (hardware if available, else software; Vbr ~ source bitrate), mux with `SegmentMuxer` into a temp dir (one segment), copy the source's audio samples (from `read_movie`, AAC only) from the requested start, then `concat_remux` the temp segment to `dst`. Progress from encoded frames; cancel checked per frame. |
| S4 | apps/ui/src-tauri/src/player.rs | probe via `read_movie` (fallback `mfdec::probe`); sidecars via `concat_remux([src], sidecar, Only(track))`; non-AAC tracks get no sidecar (audio_error names the codec). No `FfmpegPaths`. |
| S5 | apps/clipcheckd/src/service.rs (probe parts), discovery.rs | encoder probe = `probe_native()` (cached like today); recording never depends on FFmpeg; `ffmpeg_ready` means "encoders probed". Native encoder is always used. |

## Wave T: removal
FFmpeg encoder backend (`pipe.rs` process parts, `audio_in.rs` sockets), `locate.rs`, the FFmpeg pack store
(`crates/update/src/pack.rs` & manifest parts), `tauri.conf.json` ffmpeg resources, `licenses/FFMPEG-*`,
`scripts/release/fetch-ffmpeg.mjs`/`package-ffmpeg-pack.ps1` and their workflow steps, `video.native_encoder`
(always on; key accepted and ignored), FFmpeg mentions in the UI. Config `codec = "av1"` falls back to HEVC with a
warning until an AV1 path exists.
