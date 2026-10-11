# Native encoder (FFmpeg substitute): implementation contracts

Status: wave plan for the "ffmpeg substitute" (doc 12 §5.2–§5.3, phase 1 writer + phase 2b). Decision D57.
Goal: frames stay on the GPU from WGC to the hardware encoder, and segments are written by our own fMP4
muxer in process. The output must be a drop-in replacement for today's ffmpeg segment output
(`seg-NNNNNN.mp4` + `segments.csv`) so `tracker`, `buffer` (extraction, still ffmpeg concat in this
phase), sessions and the library keep working unchanged.

Out of scope for these waves: relicensing (doc 12 phase 0), MP4 reader/remux (phase 1 reader half),
`mfdec` (phase 3), FFmpeg removal (phase 4), direct NVENC/AMF/VPL (phase 5).

## New crates

| Crate | Package | `unsafe` | Depends on |
|---|---|---|---|
| `crates/mp4` | `clipcheck-mp4` | `#![forbid(unsafe_code)]` | thiserror |
| `crates/gpuconv` | `clipcheck-gpuconv` | `d3d.rs` only | windows |
| `crates/hwenc` | `clipcheck-hwenc` | `mf_*.rs` only | windows |
| `crates/aacenc` | `clipcheck-aacenc` | `mf.rs` only | windows |

Time unit at every crate boundary: **100 ns ticks (`i64`)**, like `clipcheck_core::media::TICKS_PER_SEC`.

## Decisions taken for v1 of the native path (D57)

1. **No B-frames** on the native path (`CODECAPI_AVEncMPVDefaultBPictureCount = 0`). dts == pts, so the
   muxer needs no edit list and the concat demuxer sees no negative cts. B-frames come later with an `edts`.
2. **One fragment per GOP.** The encoder is asked for an IDR every `segment_secs` (forced key frames
   plus GOP = fps × segment_secs), so in practice one segment = one fragment = one GOP.
3. **Per-segment timestamps start at 0** (ffmpeg's `-reset_timestamps 1`): each segment's video and audio
   `tfdt` start at 0.
4. **H.264 and HEVC** only. AV1 stays on the ffmpeg path.
5. The FFmpeg backend stays, selected by config, until parity is measured. Native is opt-in first.

## crates/mp4

### `annexb.rs` + `avc.rs` + `aac.rs` (worker M1)

```rust
// annexb.rs
/// NAL unit payloads (without start codes) of an Annex B byte stream. Start codes are 00 00 01 or
/// 00 00 00 01. Empty NALs are skipped. Never panics on any input.
pub fn split_nals(data: &[u8]) -> Vec<&[u8]>;
/// Annex B access unit -> MP4 sample: each NAL prefixed with its 4-byte big-endian length.
/// Access unit delimiters (H.264 type 9, HEVC type 35) are dropped; everything else is kept.
pub fn to_length_prefixed(data: &[u8], codec: VideoCodec) -> Vec<u8>;
/// Removes emulation-prevention bytes (00 00 03 -> 00 00).
pub fn unescape_rbsp(nal: &[u8]) -> Vec<u8>;
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum VideoCodec { H264, Hevc }
/// True when the access unit contains an IDR/IRAP NAL (H.264 type 5; HEVC types 16..=21).
pub fn is_keyframe(data: &[u8], codec: VideoCodec) -> bool;
/// MSB-first bit reader with exp-Golomb, over an unescaped RBSP. All reads return Option (None past end).
pub struct BitReader<'a>; // new(&[u8]), read_bits(n<=32)->Option<u32>, read_bit, read_ue, read_se, skip_bits

// avc.rs
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvcConfig { pub sps: Vec<Vec<u8>>, pub pps: Vec<Vec<u8>>, pub width: u32, pub height: u32 }
/// Collects SPS (type 7) and PPS (type 8) from an Annex B access unit and parses the first SPS for the
/// display size (pic_width_in_mbs, pic_height_in_map_units, frame_mbs_only, frame cropping; handles
/// profile_idc 100/110/122/244/44/83/86/118/128/138/139/134/135 chroma/bit-depth/scaling-list syntax).
/// None if either set is missing or the SPS is malformed.
pub fn avc_config(au: &[u8]) -> Option<AvcConfig>;
/// AVCDecoderConfigurationRecord (ISO 14496-15 §5.3.3.1): version 1, profile/compat/level = sps[1..4],
/// 0xFF (lengthSizeMinusOne = 3), 0xE0|numSPS, (u16 len + sps)*, numPPS, (u16 len + pps)*; for profile
/// 100/110/122/144 append 0xFC|chroma_format_idc, 0xF8|(bit_depth_luma-8), 0xF8|(bit_depth_chroma-8), 0x00.
pub fn avcc_payload(cfg: &AvcConfig) -> Vec<u8>;

// aac.rs
/// AudioSpecificConfig for AAC-LC (object type 2): 5 bits type, 4 bits sampling-frequency index
/// (96000..7350 table), 4 bits channel config, 3 zero bits. Unknown rate -> None.
pub fn aac_lc_asc(sample_rate: u32, channels: u8) -> Option<Vec<u8>>;
```

### `hevc.rs` (worker M2)

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HevcConfig { pub vps: Vec<Vec<u8>>, pub sps: Vec<Vec<u8>>, pub pps: Vec<Vec<u8>>, pub width: u32, pub height: u32 }
/// VPS (32), SPS (33), PPS (34) from an Annex B access unit; SPS parsed for pic_width/height_in_luma_samples
/// minus conformance window (scaled by SubWidthC/SubHeightC), profile_tier_level, chroma_format_idc,
/// bit depths. None if any set is missing or malformed.
pub fn hevc_config(au: &[u8]) -> Option<HevcConfig>;
/// HEVCDecoderConfigurationRecord (ISO 14496-15 §8.3.3.1.2): configurationVersion 1, general PTL from the
/// SPS, min_spatial_segmentation 0xF000, parallelismType 0xFC, chroma 0xFC|idc, bit depths 0xF8|(d-8),
/// avgFrameRate 0, constantFrameRate/numTemporalLayers/temporalIdNested/lengthSizeMinusOne=3 byte,
/// then 3 arrays (VPS, SPS, PPS) each: 0x80|type (array_completeness=1), u16 count, (u16 len + nal)*.
pub fn hvcc_payload(cfg: &HevcConfig) -> Vec<u8>;
```

### `fmp4.rs` (worker M3)

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoConfig { Avc(AvcConfig), Hevc(HevcConfig) }
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoTrackSpec { pub config: VideoConfig, pub width: u32, pub height: u32, pub timescale: u32 }
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioTrackSpec { pub sample_rate: u32, pub channels: u16, pub asc: Vec<u8>, pub bitrate: u32, pub title: String }
/// Track ids: video = 1, audio[i] = i + 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitSpec { pub video: VideoTrackSpec, pub audio: Vec<AudioTrackSpec> }
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sample { pub data: Vec<u8>, pub duration: u32, pub cts_offset: i32, pub keyframe: bool }
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackRun { pub track_id: u32, pub base_decode_time: u64, pub samples: Vec<Sample> }
/// ftyp (major "isom", minor 0x200, compatible isom iso2 iso6 avc1|hvc1 mp41) + moov:
/// mvhd (v0, timescale 1000, duration 0, next_track_id = n+1), per track trak(tkhd flags 3 (enabled|in movie),
/// width/height 16.16 for video, volume 0x0100 for audio; mdia(mdhd timescale (audio = sample_rate),
/// language "und", hdlr vide/soun with name "VideoHandler"/"SoundHandler", minf(vmhd flags 1 | smhd,
/// dinf/dref/"url " flags 1, stbl(stsd(avc1+avcC | hvc1+hvcC | mp4a+esds), empty stts, stsc, stsz, stco))),
/// for audio with a non-empty title: trak/udta/"name" box with the UTF-8 title),
/// mvex(trex per track: default_sample_description_index 1, rest 0).
/// esds: ES_Descriptor(tag 3, ES_ID 0, flags 0) > DecoderConfigDescriptor(tag 4, objectTypeIndication 0x40,
/// streamType 0x15, bufferSize 0, max/avg bitrate) > DecoderSpecificInfo(tag 5, asc) ; SLConfig(tag 6, 0x02).
/// avc1/hvc1/mp4a sample entries follow ISO 14496-12 VisualSampleEntry / AudioSampleEntry layout
/// (data_reference_index 1; video 72 dpi, frame_count 1, depth 0x0018, pre_defined -1;
/// audio channelcount, samplesize 16, samplerate << 16).
pub fn write_init(spec: &InitSpec) -> Vec<u8>;
/// moof(mfhd(sequence_number), per run traf(tfhd flags 0x020000 default-base-is-moof + track_id,
/// tfdt v1 base_decode_time, trun v1 flags 0x000F01 (data-offset, duration, size, flags, cts) with
/// per-sample duration/size/flags/cts)) followed by ONE mdat holding every run's sample data in run order.
/// trun data_offset = offset of the run's first byte from the start of moof.
/// Sample flags: keyframe 0x0200_0000, non-key 0x0101_0000.
/// Runs with no samples are omitted. Returns empty Vec when there are no samples at all.
pub fn write_fragment(sequence_number: u32, runs: &[TrackRun]) -> Vec<u8>;
```

### `segment.rs` (worker M4) uses only the M1–M3 APIs above

```rust
#[derive(Debug, Clone)]
pub struct SegmentConfig {
    pub out_dir: PathBuf,          // created if missing
    pub segment_secs: f64,         // > 0
    pub fps: u32,                  // > 0, used for the last video sample's duration
    pub codec: VideoCodec,
    pub width: u32, pub height: u32,
    pub audio: Vec<AudioTrackSpec>, // asc already filled in by the caller
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoPacket { pub data: Vec<u8> /* Annex B */, pub pts: i64, pub dts: i64, pub keyframe: bool } // 100 ns
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioPacket { pub data: Vec<u8> /* raw AAC frame */, pub pts: i64, pub samples: u32 }
pub struct SegmentMuxer;
impl SegmentMuxer {
    pub fn new(cfg: SegmentConfig) -> Result<Self, Mp4Error>;
    /// Packets before the first keyframe are dropped (counted). The first keyframe's AU gives the codec
    /// config (avc_config/hevc_config); if it has none -> Err(MissingParameterSets).
    /// A keyframe with pts >= current segment start + segment_secs (minus half a frame) closes the
    /// current segment and opens the next. Each keyframe also closes the pending fragment.
    pub fn push_video(&mut self, pkt: VideoPacket) -> Result<(), Mp4Error>;
    /// Queued per track; written into the fragment that closes after it. Packets before the first video
    /// keyframe are dropped. Track index out of range -> Err(BadTrack).
    pub fn push_audio(&mut self, track: usize, pkt: AudioPacket) -> Result<(), Mp4Error>;
    /// Flushes the pending fragment, closes the open segment (writing its csv row), returns all segment
    /// paths in order.
    pub fn finish(self) -> Result<Vec<PathBuf>, Mp4Error>;
    pub fn dropped_before_keyframe(&self) -> u64;
}
```

Segment rules:
- File `seg-%06d.mp4` from 0, written as init segment + fragments, flushed on fragment close, closed on
  segment close. Only after a file is closed is its row appended to `segments.csv`:
  `seg-000000.mp4,<start>,<end>\n`, seconds with 6 decimals, start/end on the session timeline (first
  keyframe pts = 0.0). end = next segment's first video pts, or for the last segment last pts + 1/fps.
- The next segment file is created only after the previous one was closed and its row written.
- Video timescale 90 000. Sample duration = next dts − dts (converted, rounded, min 1); last sample of
  the stream = 90 000 / fps. Fragment closed at a keyframe uses the keyframe's dts for its last sample.
- Audio timescale = sample_rate; sample duration = `samples` (1024). Per segment, audio base decode
  time starts at 0 and accumulates. A fragment carries the audio packets with pts < the closing keyframe's
  pts; later ones wait for the next fragment.
- Video base decode time per segment starts at 0 = first keyframe dts of the segment.
- `Mp4Error` (thiserror): `Io(#[from] io::Error)`, `MissingParameterSets`, `BadTrack(usize)`,
  `InvalidConfig(String)`.

## crates/gpuconv

### `cpu.rs` (worker G1, pure)
```rust
/// Destination rect (x, y, w, h) of an `sw x sh` source fitted inside `cw x ch` preserving aspect,
/// centred, all four values even. Zero sizes -> (0,0,0,0).
pub fn letterbox_rect(sw: u32, sh: u32, cw: u32, ch: u32) -> (u32, u32, u32, u32);
/// BGRA (tightly packed) -> NV12 `cw x ch` (cw, ch even; out resized to cw*ch*3/2), bilinear scale into
/// letterbox_rect, black bars (Y=16, U=V=128), BT.709 limited range:
/// Y = 16 + 0.1826R + 0.6142G + 0.0620B ; U = 128 - 0.1006R - 0.3386G + 0.4392B ; V = 128 + 0.4392R - 0.3989G - 0.0403B
/// (rounded, clamped). Chroma from the 2x2 average. Malformed src length -> Err(ConvError::BadInput).
pub fn bgra_to_nv12(src: &[u8], sw: u32, sh: u32, cw: u32, ch: u32, out: &mut Vec<u8>) -> Result<(), ConvError>;
```
### `d3d.rs` (worker G2, FFI)
```rust
/// Hardware D3D11 device with BGRA_SUPPORT | VIDEO_SUPPORT, multithread protected, on `adapter` (None = default).
pub fn create_device(adapter: Option<&IDXGIAdapter>) -> Result<(ID3D11Device, ID3D11DeviceContext), ConvError>;
pub struct GpuConverter;
impl GpuConverter {
    /// Output size cw x ch (even). Uses ID3D11VideoDevice/ID3D11VideoContext; the enumerator/processor is
    /// (re)built lazily for the current input size; input colour space RGB full 0-255, output YCbCr BT.709
    /// studio (limited); background colour black (YCbCr 16/128/128 via D3D11_VIDEO_COLOR YCbCr).
    pub fn new(device: &ID3D11Device, cw: u32, ch: u32) -> Result<Self, ConvError>;
    /// New NV12 texture cw x ch, DEFAULT usage, BIND_RENDER_TARGET (an encoder input / pool texture).
    pub fn create_nv12_texture(&self) -> Result<ID3D11Texture2D, ConvError>;
    /// Blits `src` (B8G8R8A8, any size) into `dst` (from create_nv12_texture), scaled into letterbox_rect.
    pub fn convert(&mut self, src: &ID3D11Texture2D, dst: &ID3D11Texture2D) -> Result<(), ConvError>;
}
```
`ConvError` (thiserror, in `lib.rs`, orchestrator-owned): `BadInput(String)`, `Windows { context: &'static str, message: String }`.

## crates/hwenc

`lib.rs` + `types.rs` are orchestrator-owned (types below). Workers: H1 `mf_enum.rs`, H2 `mf_common.rs` +
`mf_async.rs` (hardware), H3 `mf_sync.rs` (software, after H2).

```rust
pub enum Codec { H264, Hevc }
pub enum Vendor { Nvidia, Amd, Intel, Microsoft, Other }
pub enum RateControl { Cbr { kbps: u32 }, Vbr { kbps: u32, peak_kbps: u32 }, Quality { cq: u8 } }
pub struct VideoEncoderConfig { pub codec: Codec, pub width: u32, pub height: u32, pub fps: u32, pub rate: RateControl,
    pub gop_frames: u32, pub quality_vs_speed: u32 /* 0..=100 */, pub low_latency: bool }
pub struct EncodedPacket { pub data: Vec<u8> /* Annex B */, pub pts: i64, pub dts: i64, pub keyframe: bool }
pub struct EncoderDescriptor { pub name: String, pub vendor: Vendor, pub codec: Codec, pub hardware: bool, pub index: usize }
pub enum InputFrame<'a> { Texture(&'a ID3D11Texture2D), Nv12 { data: &'a [u8], width: u32, height: u32 } }
pub trait VideoEncoder: Send {
    /// Submits one frame at `pts` (100 ns); returns every packet that became ready.
    fn encode(&mut self, frame: InputFrame<'_>, pts: i64, force_keyframe: bool) -> Result<Vec<EncodedPacket>, EncError>;
    /// Drains the encoder (MFT_MESSAGE_COMMAND_DRAIN) and returns the remaining packets.
    fn flush(&mut self) -> Result<Vec<EncodedPacket>, EncError>;
    fn descriptor(&self) -> &EncoderDescriptor;
}
pub fn mf_startup() -> Result<(), EncError>;            // MFStartup once per process, never shut down
pub fn enumerate(codec: Codec) -> Result<Vec<EncoderDescriptor>, EncError>; // hardware first, then software
pub fn open(desc: &EncoderDescriptor, device: Option<&ID3D11Device>, cfg: &VideoEncoderConfig) -> Result<Box<dyn VideoEncoder>, EncError>;
```

## crates/aacenc (worker A1)

```rust
pub struct AacPacket { pub data: Vec<u8> /* raw AAC frame, no ADTS */, pub pts: i64, pub samples: u32 /* 1024 */ }
pub struct AacEncoder;
impl AacEncoder {
    /// Microsoft AAC encoder MFT (CLSID {93AF0C51-2275-45d2-A35B-F2BA21CAED00}), AAC-LC, 48 kHz (also 44.1),
    /// 1 or 2 channels, payload type 0 (raw). bitrate_kbps snaps to the nearest of 96/128/160/192.
    pub fn new(sample_rate: u32, channels: u16, bitrate_kbps: u32) -> Result<Self, AacError>;
    /// AudioSpecificConfig from the output type's MF_MT_USER_DATA (bytes after the 12-byte HEAACWAVEINFO tail).
    pub fn asc(&self) -> &[u8];
    /// Interleaved f32 [-1,1] (clamped) -> s16 PCM input. `pts` is the time of samples[0]; packets get
    /// pts = that of their first input sample (tracked by sample count). Odd length for stereo -> Err.
    pub fn push(&mut self, samples: &[f32], pts: i64) -> Result<Vec<AacPacket>, AacError>;
    pub fn flush(&mut self) -> Result<Vec<AacPacket>, AacError>;
}
```

## Wave plan

| Wave | Workers (parallel, disjoint files) |
|---|---|
| 1 | M1, M2, M3, G1, G2, H1, A1 (H2 after H1's report) |
| 1T | Fresh testers per worker (tests only, public API) → fixers → re-testers |
| 2 | M4 segmenter; H2 async MF; H3 sync MF |
| 2T | testers: segmenter with ffprobe oracle (`CLIPCHECK_FFMPEG`); hwenc synthetic frames, skip absent backends |
| 3 | `encode::native` (NativeEncoder: same report shape as FfmpegEncoder), capture GPU frame mode, engine wiring behind `video.encoder_backend = "native"` |
| 3T | end-to-end synthetic: native segments through tracker + buffer extraction |
