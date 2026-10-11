# 12: Relicense to Apache-2.0 and remove FFmpeg

> **Status: ACCEPTED.** The licensing part (§3, §4.1, §4.2) is implemented as DESIGN.md **D61**
> (superseding the GPL part of D45); FFmpeg was removed from the shipped product in **D58** and from
> the whole repository, tests included, in **D62** (§4.4 no longer applies: tests use no FFmpeg at all).
> The draft's "D50" number was taken by another decision. Engineering phases below are not part of D61
> and are tracked separately. This is an engineering analysis, not legal advice.

## 1. Goal

ClipCheck becomes an **Apache-2.0** project with **no GPL/LGPL/AGPL code or binaries anywhere**: not
in the source, not linked, not bundled, not downloaded at runtime. A user, a company or a fork can then
take, ship, embed or sell ClipCheck under one permissive license with an explicit patent grant and no
source-offer obligation.

The model is FilmCraft (`storytold/filmcraft`), which states these rules in CONTRIBUTING.md and treats
them as absolute in AGENTS.md:

| FilmCraft rule | ClipCheck equivalent (this doc) |
|---|---|
| Contributions are licensed under the project license | Apache-2.0 inbound = outbound (§3) |
| No GPL/LGPL/AGPL code (FFmpeg, x264, x265, LAME… named explicitly) | Same list, plus no GPL binaries in the installer or pack store (§4.1) |
| Dependencies permissive only: MIT/Apache-2.0/BSD/ISC/Zlib/Unicode/CC0/BSL-1.0, or MPL-2.0 used unmodified | Same, enforced by `deny.toml` (§4.2) |
| Codecs and formats implemented from public specs, not from other implementations' source | Same for our MP4 muxer/demuxer (§4.3) |
| ffmpeg/ffprobe only as external test oracles and fixture generators; never linked, bundled or shipped | Same (§4.4, §6) |
| Every asset openly licensed, with an attribution sidecar and an `ATTRIBUTION.md` index | Same; the Mixkit sound has to go (§4.5) |
| `unsafe` confined to one platform crate, `// SAFETY:` on every block | Media Foundation code is `unsafe` FFI; confine it the same way (§5.6) |

FilmCraft is dual **MIT OR Apache-2.0**. This proposal uses **Apache-2.0 only**, as requested (open
question Q1).

## 2. Why this is feasible now

1. **One copyright holder.** `git shortlog` shows a single human author (Mitch, 66 commits). AI-assisted
   commits carry a co-author trailer but no separate human copyright holder. The repository is private
   and no public release has been made. No contributor needs to agree, so relicensing is the owner's
   decision alone.
2. **ClipCheck never linked FFmpeg.** It runs `ffmpeg.exe`/`ffprobe.exe` as child processes (D14) and
   builds command lines. Our code was written against FFmpeg's documented CLI, not its source, so none
   of it derives from FFmpeg. D45 already noted that this is aggregation, not a combined work.
3. **Windows already ships licensed encoders.** Media Foundation (MF) provides hardware H.264/HEVC/AV1
   encoder MFTs from NVIDIA, AMD and Intel drivers, and Microsoft's own software H.264 encoder, AAC
   encoder and H.264/HEVC/AV1 decoders. MF is part of the OS, and the `windows` crate (MIT/Apache) is
   already a dependency. The shared brief (00) already names MF as the fallback.

Point 2 means the **license change does not have to wait for the FFmpeg removal** (§8, phase 0). Until
phase 4 the installer would still bundle a GPL FFmpeg as a separate program with its notices, which is
allowed under Apache-2.0. The "no GPL anywhere" goal is reached only at the end of phase 4.

## 3. Licensing changes

| Item | Today | After |
|---|---|---|
| `LICENSE` | GPL-3.0 text | Apache-2.0 text, verbatim from apache.org |
| `NOTICE` (new) | none | `ClipCheck` + `Copyright 2026 ClipCheck contributors` (Apache §4(d)); installed as `licenses\NOTICE.txt` |
| `Cargo.toml` `[workspace.package] license` | `GPL-3.0-or-later` | `Apache-2.0` |
| `apps/ui/src-tauri/Cargo.toml`, `apps/ui/package.json` | `GPL-3.0-or-later` | `Apache-2.0` |
| README License section, `docs/LICENSING.md`, brief 00 "Project license" line | GPL wording | Rewritten |
| `CONTRIBUTING.md` (new) | none | Short, FilmCraft-style: read `AGENTS.md`; contributions are Apache-2.0 (Apache §5 inbound = outbound); no CLA. Optional DCO `Signed-off-by` (Q3) |
| `AGENTS.md` (new) | rules spread over CLAUDE.md and `docs/impl/AGENT_RULES.md` | The absolute rules of §4, and the existing never-kill-user-apps rules. `CLAUDE.md` points to it and defers to it |
| `deny.toml` | GPL-compatible allow-list | Permissive allow-list (§4.2) |
| Per-file headers | none | Not required by Apache-2.0. Recommend none, plus an `SPDX-License-Identifier: Apache-2.0` line in new crates' `lib.rs` (Q4) |
| Installer `licenses\` | LICENSE, FFMPEG-LICENSE, FFMPEG-NOTICE, THIRD-PARTY-NOTICES | LICENSE, NOTICE, THIRD-PARTY-NOTICES |

The old GPL history stays GPL for anyone who already holds a copy; as sole copyright holder the owner
may license the same code under Apache-2.0 from the relicensing commit onward.

## 4. Rules (go into AGENTS.md)

### 4.1 No copyleft code or binaries
Never copy, link, bundle, download at runtime or ship GPL/LGPL/AGPL code or binaries: FFmpeg (any
build), libav\*, x264, x265, LAME, MLT, OBS Studio source, etc. This also covers "LGPL FFmpeg" builds:
the goal is one permissive license, not just GPL avoidance.

### 4.2 Dependencies
Allowed: MIT, Apache-2.0, BSD-2/3-Clause, ISC, Zlib, Unicode-3.0/Unicode-DFS, CC0-1.0, 0BSD, MIT-0,
BSL-1.0, Unlicense, CDLA-Permissive-2.0 (data), and **MPL-2.0 used unmodified** (`cssparser`,
`selectors`, `dtoa-short`, `option-ext` come in through Tauri). Everything else is denied by
`deny.toml`, including all GPL/LGPL/AGPL variants. The `ring` license (`Apache-2.0 AND ISC`) stays
allowed. Adding a dependency needs a line in the PR saying its license.

### 4.3 Clean-room format code
The MP4 muxer/demuxer (§5.2) is written from ISO/IEC 14496-12 (ISOBMFF), 14496-14 (MP4),
14496-15 (AVC/HEVC in ISOBMFF), the AV1-ISOBMFF binding spec and ISO/IEC 14496-3 (AudioSpecificConfig).
While implementing it, do not read FFmpeg's `libavformat` or other copyleft muxers. Reading permissive
code (e.g. the `mp4` crate) is allowed but not required; record the spec editions used in the crate README.

### 4.4 FFmpeg only as a test oracle
`ffprobe`/`ffmpeg` may be used **only** by tests and dev scripts to check our output (stream layout,
timestamps, decodability) or to generate fixtures. Such tests find the tool through `CLIPCHECK_ORACLE_FFMPEG`
or `PATH`, skip with a printed reason when it is absent, and never ship it. CI installs it for the oracle job only.

### 4.5 Assets
Every asset (icon, image, sound, font, logo) is open-licensed or original work under Apache-2.0, has an
`X.attribution` sidecar (FilmCraft's format) and a row in `ATTRIBUTION.md`. A check script fails the gate
on a missing sidecar. Consequence: `assets/sounds/clip-saved.wav` (Mixkit license, no redistribution on
its own) is replaced by a generated tone (a small `scripts/gen-sounds.mjs` producing the WAV), as D45 already
recommended.

## 5. Technical design: replacing FFmpeg

### 5.1 What FFmpeg does today

| Use | Where | Replacement |
|---|---|---|
| Encoder discovery (`-encoders`, test encode) | `crates/encode/src/probe.rs`, `select.rs` | MF: `MFTEnumEx` over `MFT_CATEGORY_VIDEO_ENCODER` with `MFT_ENUM_FLAG_HARDWARE`, then a 1-frame test encode (§5.3) |
| Live encode: BGRA frames → H.264/HEVC/AV1 (NVENC/AMF/QSV/x264/x265), 1-N f32le audio tracks → AAC, segmented fMP4 (`-f segment`, `frag_keyframe+empty_moov`) | `encode/src/pipe.rs`, `tuning.rs`, `audio_in.rs` | `gpuconv` + `hwenc` + `aacenc` + our fMP4 segment writer, in process (§5.2, §5.3) |
| Clip extraction: concat segments, stream copy (`-f concat`, `-map`) | `crates/buffer/src/extract.rs` | Our MP4 demux → remux, no re-encode |
| Media probing (`ffprobe -show_streams` JSON) | `buffer/extract.rs`, `clipedit/src/run.rs`, `engine/src/catalog.rs` | Our MP4 parser (`moov`/`moof` walk) |
| Thumbnails (`-ss … -frames:v 1 -vf scale`) | `engine/src/catalog.rs::make_thumbnail` | MF Source Reader decodes one frame at the nearest keyframe; WIC (Windows Imaging Component) scales and writes JPEG |
| Clip editor fast trim (stream copy) | `clipedit/src/run.rs` | Our remux, cut at keyframes |
| Clip editor precise trim (re-encode) | `clipedit/src/run.rs` | MF Source Reader decode → MF encoder → our muxer |
| Bundled binaries, pack store, notices | `tauri.conf.json` resources, `crates/update/src/pack.rs`, `scripts/release/*ffmpeg*`, `licenses/FFMPEG-*` | Deleted |

### 5.2 New crate `crates/mp4` (pure Rust, no `unsafe`)
- **Writer**: fragmented MP4 (`ftyp`, `moov` with `mvex`/`trex`, then `moof`+`mdat` per fragment), one
  video track (`avc1`/`hvc1`/`av01` with `avcC`/`hvcC`/`av1C` built from the encoder's sequence header)
  and N AAC tracks (`mp4a`/`esds`) with per-track title (`udta`/`name`) and default disposition, matching
  the layout we produce today so existing playback, salvage and library code keeps working.
- **Segmenter**: starts a new `seg-%06d.mp4` at the first keyframe at or after each `segment_secs`
  boundary (today's `-segment_time` + `-segment_time_delta` behaviour) and resets timestamps per segment.
- **Reader**: parses both fragmented and flat MP4 (our segments, our clips, and recordings produced by
  older FFmpeg-based builds) into tracks + samples (offset, size, dts, pts, keyframe flag).
- **Remux**: concat N inputs and cut on keyframes into a flat (faststart) MP4. Used by extraction and fast trim.
- **Probe**: `MediaInfo` (duration, codec, size, fps, audio tracks and titles) from the reader, replacing all three ffprobe callers.
- Every size, offset and count read from a file is untrusted: checked arithmetic, bounded allocations,
  `Result` errors, no panics (FilmCraft §0.4). Mutation-fuzz tests (truncation, bit flips, bad box sizes).

### 5.3 Our own media library, in process (no separate EXE)
Once FFmpeg is gone there is **no licensing reason for a child process**: every line of the encode path
is either our Apache-2.0 code, an OS component (Media Foundation, D3D11) or the GPU driver's own runtime
DLL, which we load at runtime and never ship (like any OS API). The separate `ffmpeg.exe` existed for
FFmpeg's license and as a ready-made program, not because we need it. So the replacement is a small set
of in-house crates that do **only** what ClipCheck needs, linked into `ClipCheck.exe`:

| Crate | Does | Doesn't | `unsafe` |
|---|---|---|---|
| `mp4` | fMP4 segment writer, reader, remux, probe (§5.2) | any other container (MKV, TS, WebM) | forbidden |
| `gpuconv` | On the capture device: scale + letterbox + BGRA→NV12 (BT.709, limited range) in one D3D11 Video Processor blit; a pure-Rust CPU fallback for the software encoder | filters, HDR tone mapping (later) | FFI module only |
| `hwenc` | `VideoEncoder` trait: `encode(&NV12 texture, pts, force_key) -> Vec<Packet>`, `sequence_header()`, `flush()`. Backends below | decoding, software codecs of our own | FFI modules only |
| `aacenc` | AAC-LC per track through the OS AAC encoder MFT (in process), f32→s16 | other audio codecs | FFI module only |
| `mfdec` | One-frame and range decode through the MF Source Reader (thumbnails, precise trim) | playback (stays in WebView2) | FFI module only |

**`hwenc` backends**, all behind the same trait and selected by today's `select.rs` logic:

| Backend | API | Loaded from | Header license | Why |
|---|---|---|---|---|
| `mf` (first) | Media Foundation encoder MFTs (NVIDIA, AMD, Intel hardware; Microsoft software H.264) | Windows | OS API | One code path for every vendor and the software fallback. Ships first. |
| `nvenc` | NVIDIA Video Codec SDK (`nvEncodeAPI`) | `nvEncodeAPI64.dll` from the driver, `LoadLibrary` at runtime | MIT | Full control the MF wrapper does not expose (presets/tuning info, lookahead, AQ, multipass, exact rate control). Most gaming PCs. |
| `amf` | AMD AMF | `amfrt64.dll` from the driver | MIT | Same, for AMD |
| `vpl` | Intel oneVPL | driver runtime via our own minimal dispatcher | MIT | Same, for Intel. Lowest priority: MF's Intel MFT is the same driver code |

The direct backends are optional upgrades, added only where measurements (§7) show MF falls short in
quality or control. We write Rust bindings ourselves for the handful of functions we call (from the MIT
headers, with their notice in `THIRD-PARTY-NOTICES`), not a full SDK binding crate.

Common to all backends:
- Input is an NV12 D3D11 texture on the capture device (`IMFDXGIDeviceManager` for MF,
  `nvEncRegisterResource` for NVENC, `CreateSurfaceFromDX11Native` for AMF), so frames never leave the GPU.
- Rate control and tuning: today's `tuning.rs` settings map to each API's own properties (MF:
  `ICodecAPI` `CODECAPI_AVEncCommonRateControlMode`, `…MeanBitRate`, `…QualityVsSpeed`,
  `CODECAPI_AVEncMPVGOPSize`, `CODECAPI_AVEncVideoForceKeyFrame`). Not every MF vendor MFT honours every
  property; the probe records which ones stick and the UI hides the rest.
- Software fallback: Microsoft's H.264 MFT (OS component, covered by Microsoft's codec licensing),
  fed by `gpuconv`'s CPU path. No software HEVC (paid Store extension) and no software AV1 (already never
  selected, see `select.rs`).
- Timestamps: the wall-clock CFR logic (`FrameClock::Wall`, duplicate/drop to a constant fps) moves into
  our code ahead of the encoder; segment boundaries are forced keyframes we request, so the segmenter
  never has to guess.

What stays out of scope keeps it light: no generic filter graph, no software codecs we wrote, no
protocol layer, no container zoo. Rough size: a few thousand lines of Rust per the table above and
well under 1 MB of extra binary, against ~55 MiB of FFmpeg EXEs and DLLs today.

### 5.4 Crash isolation in process
D46 accepted a single recorder process partly because "the encoder already runs out of process". In
process, a fault inside a GPU driver (access violation, not a Rust panic) takes the recorder down. This
is the same trade every in-process recorder makes (OBS runs NVENC/AMF/QSV in process). Mitigations:
- The encoder runs on its own thread with `catch_unwind`; Rust-side failures become `EncoderFailed` events
  and the session continues on the next backend in the fallback chain (as today on ffmpeg exit).
- `DXGI_ERROR_DEVICE_REMOVED`/`RESET` and encoder error codes are handled as errors: recreate the device
  and encoder, start a new segment.
- Segments are written and closed every `segment_secs`, so a hard crash loses at most the open segment;
  salvage and the stale-run sweep (D46, `sweep.rs`) already handle the rest.
- An optional watchdog: if the encoder thread stops returning packets for N seconds, the recorder
  restarts its capture/encode stack.
- If field crash reports later show driver faults, the same crates can run in a `--encoder` child as a
  fallback mode without design changes (the trait boundary is the same). Not built up front.

### 5.5 Decode (thumbnails, precise trim)
MF Source Reader with hardware decoding (`MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING`, a D3D11
device manager). H.264 decoding is built into Windows. HEVC and AV1 decoding need the free/OEM Store
extensions that most PCs have. When a decoder is missing, the error says which extension to install;
recording is not affected. Player playback stays in WebView2 (`<video>`), which is unchanged.

### 5.6 `unsafe` policy
MF and D3D11 calls are `unsafe` COM FFI, like the existing WGC/WASAPI code. Follow FilmCraft's platform
rule: `unsafe` only in dedicated FFI modules (in `gpuconv`, `hwenc`, `aacenc`, `mfdec`, `capture`, `audio`), a `// SAFETY:` comment
on every block, safe `Result`-returning APIs; `crates/mp4` and the other crates use `#![forbid(unsafe_code)]`.

### 5.7 Compatibility and migration
- **Config**: `EncoderChoice` values stay (auto/backend/codec). `encoder_extra_args` (allowlisted FFmpeg
  flags) has no MF equivalent; it is dropped with a migration that logs and ignores it. `libx264`/`libx265`
  probe-cache entries are invalidated by bumping the cache version.
- **Library**: recordings made by FFmpeg builds are ordinary MP4 and stay playable (WebView2) and
  readable (our reader). The reader is tested against a fixture set produced by the old pipeline.
- **Windows N/KN editions** have no Media Foundation without the Media Feature Pack. Detect at startup
  and show an actionable error. (Today FFmpeg hides this.)
- **Patents**: H.264/HEVC/AAC patents are licensed through the OS and GPU vendors' encoders rather than
  through x264/FFmpeg builds we distribute. Not legal advice; noted for the owner.
- **Size**: the installer drops from ~60 MiB to roughly 10 MiB; the 90 MiB budget (D41) can be cut.
- **Quality**: x264 `veryfast`/`medium` software output is better than Microsoft's software H.264
  encoder at the same bitrate. The software path is a fallback for machines without a hardware encoder,
  which is rare for gaming PCs. Measure it (§7) and state it in the release notes.

## 6. Performance impact

### 6.1 Today's frame path (read from the code, not profiled)
1. WGC delivers a BGRA texture on the GPU.
2. `CopyResource` to a staging texture and a blocking `Map` (`crates/capture/src/wgc.rs:446`): a GPU→CPU
   readback that also stalls until the GPU finishes the copy, every frame.
3. CPU nearest-neighbour scale + letterbox into the output size (`crates/engine/src/pipeline.rs`).
4. Raw BGRA written to `ffmpeg.exe`'s stdin (`encode/src/pipe.rs:277`), copied again by the OS pipe.
5. ffmpeg converts BGRA→yuv420p on the CPU (swscale).
6. ffmpeg uploads the frame back to the GPU for NVENC/AMF/QSV.
7. Audio: f32 PCM over loopback TCP per track; ffmpeg encodes AAC.

Data volume through the CPU at the default 1080p60 (output) from a 1440p game:

| Step | Per frame | Per second (60 fps) |
|---|---|---|
| Readback, 2560×1440 BGRA | 14.7 MB | ~885 MB/s |
| Scaled BGRA through the pipe, 1920×1080 | 8.3 MB | ~498 MB/s (written and read: ~1 GB/s of copies) |
| yuv420p after swscale + upload | 3.1 MB | ~187 MB/s |

These are arithmetic, not measurements. They mean several GB/s of memory traffic and a few CPU cores'
worth of copy/convert work on the same machine that is running the game, plus a per-frame GPU sync.

### 6.2 Proposed frame path
1. WGC texture → one `gpuconv` Video Processor blit (scale + letterbox + BGRA→NV12, bilinear or better)
   into a pooled NV12 texture. GPU only, no sync with the CPU.
2. The NV12 texture goes straight to the hardware encoder on the same device.
3. Only the compressed bitstream (~1 MB/s at 8 Mbps) reaches the CPU, into `mp4`.
4. Audio PCM is handed to `aacenc` in memory; no TCP.

### 6.3 Expected effect (to be confirmed by §7 benchmarks)

| Metric | Today | Expected | Why |
|---|---|---|---|
| CPU copies of video | ~2 GB/s at 1440p→1080p60 | ~0 (bitstream only) | No readback, no pipe, no swscale |
| Recorder CPU while recording, hardware encoder | several cores' worth of copy/convert (to measure) | a small fraction of one core | Only muxing and audio remain on the CPU |
| GPU stall per frame | blocking `Map` | none | No readback |
| Impact on game fps | noticeable at high resolution/fps (to measure) | lower | Fewer CPU cycles and memory bandwidth taken from the game; the encode block (NVENC etc.) is separate silicon either way |
| Scaling quality | nearest-neighbour | bilinear/better | Video Processor filtering |
| Process start per session | `ffmpeg.exe` + ~7 DLLs (tens of MB) | encoder init only (~100 ms typical) | No process spawn |
| Recorder memory | ffmpeg process + pipe buffers | lower | One process; textures pooled on the GPU |
| Installer | ~60 MiB | ~10 MiB | No FFmpeg |
| Software-encoder (no GPU encoder) quality | x264 | lower (Microsoft H.264 MFT) | Accepted trade-off (§5.7); rare on gaming PCs |
| Clip save, extraction, fast trim | ffmpeg process start + concat | in-process remux, no spawn | Expected faster for short clips |
| Thumbnails | ffmpeg spawn + decode | in-process one-frame decode | Faster; could also be taken from the live frame at save time |

The biggest win comes from keeping frames on the GPU, which needs the encoder in the same process (or
cross-process shared textures). That is the main performance argument for §5.3 over a child process.

### 6.4 Risks to performance
- Some vendor MFTs add latency or ignore B-frame/lookahead settings; that is what the direct backends fix.
- Using our D3D11 device for both capture and encode keeps everything on one GPU; on hybrid laptops the
  game may render on the dGPU while WGC returns a texture on another adapter. Today's readback hides
  that; the new path must detect a cross-adapter texture and do one GPU-to-GPU copy (or fall back to
  the CPU path).
- The CPU path stays for the software encoder, so CPU cost there is similar to today minus the pipe.

### 6.5 Relation to issue #200
#200 measured today's cost (i7-7700K / RTX 2070 Super, 1080p60 NVENC: ~19 % CPU, 336 MB, ~43 % GPU 3D)
and planned a fix in two steps. Its Phase 0 quick wins, Phase 2 counters and Phase 3 benchmarks stay
as they are. Its Phase 1 (libav linked in process, row D) is replaced by this design (row E), because
linking the bundled GPL libav into `ClipCheck.exe` would make the shipped binary a GPL combined work.
Rows D and E reach the same estimated floor (~1–3 % CPU, ~80–150 MB, ~2–5 % GPU 3D, no frame bytes
crossing the bus). E also drops ~50 MiB from the installer; it costs our own muxer and less tuning
control on some vendor MFTs (row F, direct vendor SDKs, covers that if needed).

## 7. Verification
- **Unit/fuzz** (`crates/mp4`): box round-trips, sample tables, segment boundaries, hostile inputs.
- **Oracle tests** (skip when no ffprobe/ffmpeg, §4.4): our segments and clips pass `ffprobe` with the
  expected streams, codecs, durations (±1 frame), keyframe at t=0, monotonic timestamps; `ffmpeg -v error -i x -f null -` decodes without errors.
- **Encoder tests**: synthetic frames (no games, CLAUDE.md rule 6) through every `hwenc` backend present on the machine; tests skip backends that are absent.
- **Parity**: the same synthetic 60 s input through the old FFmpeg path and the new path; compare
  file size at equal bitrate, A/V sync drift, segment durations; VMAF/PSNR via the oracle for quality.
- **Benchmarks** (before phase 2, so §6.3 has real numbers): a synthetic D3D11 frame source at
  1080p60, 1440p60 and 2160p60 into (a) today's readback + pipe + ffmpeg path and (b) `gpuconv` + `hwenc`.
  Record recorder CPU %, private memory, GPU 3D/encode engine %, frames dropped, and latency from frame to
  packet. Run them on whichever of NVIDIA/AMD/Intel hardware is available; results go in this doc.
- **Gate**: `cargo deny check` plus a new `scripts/check-assets.mjs` join the wave gate in CLAUDE.md rule 5.

## 8. Phases (each is a separate swarm wave)

| Phase | Content | Ships GPL binaries after it? |
|---|---|---|
| 0 | Relicense: LICENSE, NOTICE, manifests, README, LICENSING.md, CONTRIBUTING.md, AGENTS.md, deny.toml allow-list, asset sidecars + ATTRIBUTION.md, replace the Mixkit sound | Yes (FFmpeg still bundled, as a separate program with its notices) |
| 1 | `mp4`: reader, probe, remux; switch extraction, probe and fast trim off FFmpeg | Yes |
| 2a | Benchmark harness (§7) on today's path | Yes |
| 2b | `gpuconv` + `hwenc` (`mf` backend) + `aacenc` + fMP4 segment writer behind the `Encoder` trait, in process; probe via `MFTEnumEx`; FFmpeg backend kept behind a config switch for side-by-side parity runs | Yes |
| 3 | `mfdec`: thumbnails and precise trim | Yes |
| 4 | Remove FFmpeg: backend, `locate.rs`, pack store in `crates/update`, `tauri.conf.json` resources, `fetch-ffmpeg.mjs`/`package-ffmpeg-pack.ps1`, `licenses/FFMPEG-*`, `vendor/ffmpeg`; update DESIGN D14/D37/D41/D45/D46, docs 03/11, release.md | **No** (goal reached) |
| 5 (optional) | Direct `nvenc`, then `amf`, then `vpl` backends, only where §7 numbers show MF falls short | No |

Phase 0 can be done now. Phases 1 and 3 do not depend on 2b and can run in parallel waves.

## 9. Open questions for the owner
1. **Apache-2.0 only, or MIT OR Apache-2.0** (FilmCraft and most Rust projects)? Dual licensing lets
   people pick MIT, which is GPL-2.0 compatible; Apache-2.0 alone is not. Recommendation: Apache-2.0
   only as requested, unless GPLv2 projects reusing ClipCheck code matters to you.
2. Relicense **now** (phase 0, FFmpeg still bundled until phase 4) or only when FFmpeg is gone?
   Recommendation: now; it is clean and decouples paperwork from engineering.
3. Contributor terms: plain Apache §5 (recommended, like FilmCraft), or require DCO sign-off?
4. SPDX headers: none (recommended) or one line per file?
5. Accept that a GPU driver fault now takes down the recorder (§5.4), in exchange for zero-copy
   encoding and no second process? Recommendation: yes, with the mitigations listed.
6. Accept losing x264/x265 software quality and software HEVC in exchange for no GPL (§5.7)?
7. Direct vendor backends (`nvenc`/`amf`/`vpl`): build only if MF measurements fall short
   (recommended), or plan `nvenc` from the start for full NVIDIA tuning control?
8. Which GPUs can be used for benchmarks and encoder tests? Backends without hardware can only be
   compile-checked.
