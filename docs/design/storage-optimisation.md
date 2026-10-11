# Storage optimisation: where the recording bytes go

Status: measured, partly implemented · Date: 2026-10-10 · Crates touched: `crates/encode` (tuning.rs)

## 1. Question

Recordings are large for their bitrate: a 2:40 session was 276 MB (about 13.8 Mbps), and clips
run about 1.5 to 2.5 MB per second. This note measures the split between video and audio, finds
the cause, and records what was changed and what was left.

## 2. Measured breakdown

Files were read with the vendored `ffprobe` (read-only). Recordings are in
`%USERPROFILE%\Videos\ClipCheck\{Sessions,Clips}`.

| File | Size | Duration | Video | Audio | Video share |
|---|---|---|---|---|---|
| Terraria session part1 (18-12-43) | 289.0 MB | 160.6 s | h264_nvenc, Main, yuv420p, 1920x1080, 60 fps, 13.77 Mbps | 4 AAC-LC stereo tracks: 205, 193, 202, 2 kbps | 95.7% |
| MySingingMonsters session part1 (05-21-54) | 341.4 MB | 193.3 s | h264_nvenc, Main, yuv420p, 1920x1080, 60 fps, 13.76 Mbps | 4 AAC-LC stereo tracks: 198, 188, 192, 3 kbps | ~95.7% |
| MySingingMonsters clip-30s (05-17-01) | 14.7 MB | 8.1 s | 13.87 Mbps | 4 tracks, about 200 kbps each | ~95% |
| Terraria clip-30s (18-09-31) | 25.3 MB | 14.2 s | 13.65 Mbps | 4 tracks, about 195 kbps each | ~95% |

Per-second video bytes (both sessions): mean 1717 KB/s (about 13.7 Mbps), with a range of 146 KB
to 3077 KB in a single second. Keyframes are 7.7% of video bytes on the Terraria session: 79
IDR frames averaging 269 KB, against about 27 KB for P/B frames.

Breakdown of the Terraria session total (14.39 Mbps container bitrate):

- Video: 13.77 Mbps (95.7%)
- Audio: 0.60 Mbps (4.2%), four tracks
- Container and other: about 0.02 Mbps (0.15%)

Clips are stream copies. Their video bitrate matches the sessions they came from, so clip
extraction does not re-encode.

## 3. Root cause

The file size is bitrate multiplied by duration, and the bitrate is set by the rate control, not by
the content. The owner's `[video]` section has `preset = "high"` (1080p, 60 fps, 14000 kbps
target). In `apps/clipcheckd/src/settings_map.rs:145-153`, `rate_mode_of` returns
`RateMode::Cbr` for every non-custom preset. So a High recording spends 14 Mbps whether the
scene is a menu or a fight. The `bitrate_kbps` value in `[video]` is ignored unless the preset is
`custom`.

The measured bitrate (13.77 Mbps against a 14000 kbps target) confirms that the encoder is running
at its CBR target. The design note `docs/design/03-encoding.md` section 5.2 says the presets use
VBR (target and peak) and section 6.3 gives a CQ per preset. The code does not do that, so this is
a gap between the design and the implementation.

Constant bitrate on static content is the largest waste. Synthetic check (20 s, 1080p60,
h264_nvenc, same GOP settings as the app):

| Source | Encode | Size | PSNR (dB) | SSIM |
|---|---|---|---|---|
| Solid colour | CBR 14000k (current) | 34.67 MB | lossless | 1.000 |
| Solid colour | VBR cq30 + AQ | 0.47 MB | lossless | 1.000 |
| mandelbrot zoom | CBR 14000k (current) | 36.17 MB | 28.56 | 0.9428 |
| mandelbrot zoom | CBR 14000k + new AQ/lookahead/b_ref | 36.02 MB | 28.48 | 0.9447 |
| mandelbrot zoom | VBR cq30, maxrate 14000k, AQ, p5 | 34.93 MB | 28.69 | 0.9463 |
| mandelbrot zoom | VBR cq26, maxrate 14000k, AQ, p5 | 36.20 MB | 28.54 | 0.9447 |
| testsrc2 | CBR 14000k (current) | 35.13 MB | 45.22 | 0.9962 |
| testsrc2 | VBR cq28, AQ, p5 | 31.18 MB | 44.45 | 0.9958 |

Reading the table: a flat screen costs 99% less in quality-based VBR. On busy synthetic motion,
quality-based VBR saves little at equal quality. Real gameplay sits between these extremes and has
not been measured. The synthetic results are not evidence of the real saving.

## 4. Changes made

`crates/encode/src/tuning.rs`, NVENC branch of `video_encoder_args` (about lines 144-158), and the
constant `NVENC_LOOKAHEAD_FRAMES` (line 23):

- `-spatial-aq 1` on all NVENC encoders.
- `-temporal-aq 1` on H.264 and HEVC NVENC (not AV1).
- `-rc-lookahead 20` unless the speed is `fastest` (which uses the low-latency tune).
- `-b_ref_mode middle` on H.264 and HEVC NVENC when `b_frames > 0`.

Effect on size: none by itself at a fixed CBR bitrate (the mandelbrot pair above is about 0.4%
smaller, with PSNR down 0.08 dB and SSIM up 0.002). These flags make quality-based VBR efficient
and improve quality at a given rate. `cargo check` and `cargo clippy -D warnings` pass for
`clipcheck-encode`. The flags were run through the vendored ffmpeg in the table above; no test
suite was run.

No API or config change. The change applies to every NVENC path, including the existing CBR
and custom modes.

## 4a. Implemented: preset rate control and audio default

Status: implemented, with unit and integration tests. Savings on real gameplay are not measured.

- **Preset rate control.** Non-custom presets no longer use CBR. They use `RateMode::QualityPeak { cq, max_kbps }`
  (new variant in `crates/encode/src/tuning.rs`). The quality value sets the bits, and the codec-scaled preset
  bitrate is the peak cap (`-maxrate`, `-bufsize` = 2 x cap). `custom` keeps its existing mapping.
  The code is in `rate_mode_of` in `apps/clipcheckd/src/settings_map.rs`.
- **CQ table** (H.264 basis, from `docs/design/03-encoding.md` 5.2, with the 6.3 codec offsets):

  | Preset | Low | Medium | High | Ultra |
  |---|---|---|---|---|
  | CQ, H.264 | 32 | 29 | 26 | 23 |
  | Peak cap, H.264 (kbps) | 4000 | 8000 | 14000 | 30000 |

  Offsets: H.265 +2 on NVENC, QSV and AMF; x265 +5; AV1 +3. Clamped to 51.
- **Encoder arguments.** High on `h264_nvenc` was `-rc cbr -b:v 14000k -maxrate 14000k -bufsize 28000k`.
  It is now `-rc vbr -cq 26 -b:v 0 -maxrate 14000k -bufsize 28000k`. x264 and x265 give `-crf cq` with the same
  cap. QSV gives `-global_quality cq` with the cap. AMF gives `-rc cqp` with no cap, since its constant-QP
  mode has no peak.
- **Audio default.** `[audio] bitrate_kbps` default 192 -> 128 (`crates/core/src/config/schema.rs`). A value in
  config still wins. Four tracks save 256 kbps.
- The replay buffer's 2 s keyframe cadence is unchanged.

**Measured** (20 s, 1920x1080 at 60 fps, `h264_nvenc`, vendored ffmpeg 8.1, `-g 120`, p4/hq, B-frames 2,
AQ and lookahead as in the code; sizes in MB):

| Source | Old: CBR 14M | New: CQ 26, cap 14M | Change |
|---|---|---|---|
| Solid colour | 34.67 | 0.48 | -99% |
| testsrc2 | 35.16 | 35.15 | 0% |
| mandelbrot zoom | 36.02 | 36.18 | +0.4% |

On busy content the 14 Mbps cap binds, so there is no saving. Savings come only from low-motion stretches.
The worst case is unchanged, since the peak is the same 14 Mbps as before. Real gameplay falls between the
solid colour and busy rows; its saving depends on the share of static time and has not been measured.

**Not verified:** real game footage; AMF and QSV cap behaviour on hardware (QSV ICQ may ignore `-maxrate`);
HEVC and AV1 quality on hardware (offsets come from the design doc).

## 5. Remaining recommendations

1. **Preset peak cap tuning.** The preset cap is the scaled preset bitrate. A lower cap would bound peaks more tightly, but it also limits quality on busy scenes. Pick it from real footage.
2. **Keyframe cadence.** IDRs are 7.7% of video bytes at a 2 s interval. Raising the interval to
   4 s saves about 4% of video bytes, but doubles the replay segment length. The 2 s segment rule
   is required by the replay buffer, so this is a product decision.
3. **Audio.** Default reduced to 128 kbps (see 4a). Four AAC tracks cost about 0.60 Mbps at 192 kbps. The `Mix` track duplicates the sum of the others. The Discord track measured 2 to 3 kbps (silence).
4. **HEVC.** `hevc_nvenc` is available on the test machine (RTX 5080). The design note expects it
   to be about 35 to 40% smaller at equal quality. Not measured here. The config codec is
   `h264` and the user's choice was left alone.
5. **NVENC preset.** `Speed::Balanced` maps to `p4` (`tuning.rs`, `nvenc_preset`). `p5` or `p6` is
   a small size gain for VBR. It was not changed because the existing golden test asserts `p4`.

## 6. Not verified

- Real game footage was not encoded. The only sources were generated (testsrc2, mandelbrot,
  solid colour), one 20 s clip each.
- HEVC and AV1 were not measured.
- The quality measure is PSNR and SSIM on synthetic content. It does not show whether the
  saving is visible in gameplay.
- The per-second and keyframe figures come from two sessions and one short clip set, not from
  a wider sample.
