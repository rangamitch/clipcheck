# Triage batch 01: audio and buffer issues

Judged against commit b6e6525 (HEAD) using `git show HEAD:<path>` and `git grep ... HEAD`.
Line numbers below are HEAD line numbers, which have moved since the issues were filed.
No issue in this batch was closed: all ten are STILL-VALID.

| issue | title | verdict | evidence (HEAD) | closed? | fix size / priority |
|---|---|---|---|---|---|
| #75 | Process loopback timeout frees event and params while activation can still run | STILL-VALID | `crates/audio/src/loopback.rs:119-120` closes the event unconditionally after `activate_with_event` returns, and `:147-151` returns on timeout while the handler (`:63-65`) can still `SetEvent` it; `params` is owned by `open_process_loopback` (`:176-185`) and can dangle. | no | M / P0 |
| #81 | Silent WASAPI packets decode to full-scale negative for 8-bit PCM | STILL-VALID | `crates/audio/src/lib.rs:336-337` feeds `vec![0u8; raw_len]` for silent packets, and `crates/audio/src/format.rs:98` maps `0x00` to `-1.0`; no silent-packet 8-bit test in `crates/audio/tests/format.rs`. | no | S / P0 |
| #84 | Capture errors end the stream silently; thread failures reported as Unsupported | STILL-VALID | `crates/audio/src/lib.rs:266` (`Normalizer::new` Err => return), `:269-271` (`Start` error => return), `:309`, `:331`, `:347` (drain HRESULTs dropped as `None`), `:238` and `:254-256` (thread spawn/exit mapped to `AudioError::Unsupported`); `CaptureHandle` (`:182-185`) has no error slot. | no | M / P0 |
| #88 | decode_to_f32 fallback returns wrong-length buffer for unvalidated formats | STILL-VALID | `crates/audio/src/format.rs:90` is `pub fn decode_to_f32` with no validation, and `:122-123` returns `vec![0.0; raw.len()]` (one f32 per byte) for e.g. `bits_per_sample: 12`. Validation only happens in `Normalizer::new` (`:217-219`). | no | S / P2 |
| #91 | WAVE constants and mix-format parsing duplicated; layout comment overstates check | STILL-VALID | Constants duplicated: `crates/audio/src/loopback.rs:32,34` and `crates/audio/src/format.rs:14-15`. Sub-format extraction still in `loopback.rs:226-232` (already routed into `from_wave_fields` at `:233`). Comment at `loopback.rs:41` claims a layout check, but `:50` only asserts the size. | no | S / P3 |
| #132 | Skipped and unsupported paths report success; find_ffplay duplicates the locator | STILL-VALID | `crates/audio/tests/mic_and_mix.rs:54-56` prints "PASS" on NoDevice without asserting, `:81-83` prints SKIP and passes; `crates/audio/tests/loopback.rs:177-180`, `:188-191`, `:192-195`, `:228-235` return early as pass; `find_ffplay` at `loopback.rs:25-38` ignores `CLIPCHECK_FFMPEG` (`crates/encode/src/locate.rs:22`). | no | S / P2 |
| #136 | Normalizer copies each capture packet through several intermediate Vecs | STILL-VALID | `crates/audio/src/format.rs:230` (decode Vec), `:231` (to_stereo Vec, pure copy for stereo, `:138-140`), `:186` (`input.to_vec()` in passthrough), `:232-236` (flatten collect); silent packets allocate at `crates/audio/src/lib.rs:337`. No scratch buffers exist. | no | M / P1 |
| #142 | Capture start re-reads registry and QueryPerformanceFrequency on every call | STILL-VALID | `crates/audio/src/loopback.rs:303-304` calls `is_supported()` on each `ProcessLoopback::start`, which calls `os_build()` (`:93-94`, a `RegGetValueW` at `:70-90`) each time; `crates/audio/src/lib.rs:173` calls `QueryPerformanceFrequency` on every `now_100ns` (`:167-179`), which runs per packet and per silence chunk. No `OnceLock` cache. | no | S / P1 |
| #144 | Win32_Security windows feature enabled but unused | STILL-VALID | `crates/audio/Cargo.toml:17` enables `"Win32_Security"`; `git grep Security HEAD -- crates/audio` finds no other reference. | no | S / P3 |
| #49 | Buffer public API unused outside tests: purge_all, free_space_ok, gap_100ns, bytes_written | STILL-VALID | `crates/buffer/src/ringfs.rs:129` (purge_all), `:144-150` (free_space_ok, plus non-Windows stub), `:254` (free_bytes_available), `crates/buffer/src/index.rs:182` (gap_100ns), `crates/buffer/src/ringfs.rs:195` (bytes_written). `git grep HEAD` finds callers only in `crates/buffer/src` and `crates/buffer/tests`. Surprise: `crates/engine/src/import.rs:670` has its own `free_bytes_available`, so a disk-space check exists elsewhere. | no | M (decision needed per item) / P3 |

## Counts
- ALREADY-FIXED: 0
- OBSOLETE: 0
- STILL-VALID: 10
- PARTIAL: 0
- UNSURE: 0

## Notes
- #91 is partly addressed already: `read_mix_format` routes through `InputFormat::from_wave_fields` (`loopback.rs:233`). The remaining duplication is the constants and the sub-format extraction.
- #49: the `free_space_ok` question (should the recorder stop when the disk is nearly full) is a possible missing feature, not only dead code. Consider filing it separately.
- #75 and #84 interact: a fix for #84 (error channel) should follow the #75 ownership fix.
