# Triage batch 10 (encode, engine), judged at HEAD b6e6525

Verdicts were read from `git show HEAD:<path>`, not the working tree.

| issue | title | verdict | evidence | closed? | fix size / priority |
|---|---|---|---|---|---|
| #130 | encode tests assert nothing, re-implement watchdog, check trivia | STILL-VALID | `crates/encode/tests/audio_stress_tests.rs:439-457` only prints the `finish()` result; hand-rolled `Stage` enum at `:348` duplicates `watchdog` (`:211`); `pipe_tests.rs:377-383` tests a TempDir helper; `pipe_tests.rs:366-367` has the redundant `ffmpeg.exe` assert | no | S / P2 |
| #137 | audio push encodes per sample and allocates per block | STILL-VALID | `crates/encode/src/audio_in.rs:276-282` `samples_to_le_bytes` loops `extend_from_slice` per `f32`; `write_samples` (`:137-145`) allocates a `Vec<u8>` per call; queue is `VecDeque<Vec<u8>>` (`:39`) | no | M / P1 |
| #140 | waits poll with sleeps instead of blocking | STILL-VALID | `audio_in.rs:203-207` and `:241` sleep `ACCEPT_POLL` (5 ms); `pipe.rs:496-513` `wait_with_timeout` polls `try_wait` every `POLL_INTERVAL` (25 ms); `probe.rs:299-313` polls `try_wait` every 20 ms per candidate | no | M / P2 |
| #143 | probe runs test encodes serially within each backend | STILL-VALID | `crates/encode/src/probe.rs:124-141` (`probe_all` backend closure) maps the candidates with a sequential `.map(...)`/`probe_one` call; only backends run in threads (`:118-120`) | no | S / P2 |
| #194 | probe cache keyed only on ffmpeg banner | STILL-VALID | `crates/encode/src/probe.rs:171-181` compares only `cached.build == build`; `ProbeReport` (`:63-68`) has just `build` and `encoders`, with no adapter, driver or fingerprint field | no | M / P0 (wrong encoder chosen after GPU or driver change) |
| #10 | two clips in the same second resolve to the same file | STILL-VALID | `crates/engine/src/recorder.rs:495-499` picks `unique_path` synchronously; `recorder/naming.rs:88-100` only checks `exists()`, and the file is created later on the worker (`recorder.rs:~505`); no reservation exists in `crates/engine/src` | no | S / P0 (silent clip loss) |
| #11 | CREATE_NO_WINDOW, lock and stderr-tail helpers duplicated | STILL-VALID | `CREATE_NO_WINDOW` defined at `engine/src/sessionfile.rs:19`, `engine/src/tracker.rs:22`, `engine/src/catalog.rs:21`, `buffer/src/extract.rs:19`, `encode/src/pipe.rs:31`; `lock` at `engine/src/recorder/helpers.rs:74` and `engine/src/audiofeed.rs:417`; `stderr_tail` at `engine/src/catalog.rs:514`, `buffer/src/extract.rs:275`, `encode/src/pipe.rs:291`, `encode/src/probe.rs:324` | no | M / P3 |
| #12 | sessionfile re-implements concat and temp-then-rename from buffer | STILL-VALID | `engine/src/sessionfile.rs:30` `TMP_SUFFIX`, `:165` `tmp_path`, `:193` `concat_list`, `:206` `run_concat`; `buffer/src/extract.rs:165-166` and `:191-252` do the same job; the two copies still differ (stderr check at `sessionfile.rs:232`) | no | M / P2 |
| #13 | silent fallbacks turn failures into 0 or empty | STILL-VALID | `engine/src/catalog.rs:274` `metadata(..).unwrap_or(0)`; `catalog.rs:410-415` mtime `unwrap_or(0)`; `catalog.rs:225-228` `u64_field` maps bad values to 0; `recorder/naming.rs:93-99` returns `(10000)` without an `exists()` check (overwrite risk); `recorder/naming.rs:39-41` and `:47-50` `map_or(0, ..)` on clock errors | no | M / P2 (the 10,000-suffix overwrite is the one data-loss part) |
| #14 | unreachable and redundant defensive branches | STILL-VALID | `engine/src/pipeline.rs:172-178` `handle.take().ok_or_else("pipeline already stopped")` is unreachable (`stop(self)` runs before `Drop`); `engine/src/recorder.rs:425-441` `if let Some(old) = finished` after a checked `replace`; `engine/src/recorder/helpers.rs:130-131` `let _ = hwnd;` | no | S / P3 |

## Counts

- ALREADY-FIXED: 0
- OBSOLETE: 0
- STILL-VALID: 10
- PARTIAL: 0
- UNSURE: 0

No issues closed. Nothing in this batch met the strict ALREADY-FIXED or OBSOLETE bar.

## Notes

- Working-tree edits to `apps/clipcheckd/src/service.rs`, `crates/encode/src/audio_in.rs`, `crates/encode/src/pipe.rs`, `crates/engine/src/catalog.rs`, `crates/engine/src/recorder.rs`, `crates/engine/src/sessionfile.rs` and `crates/library/src/fsops.rs` were not read. Verdicts reflect HEAD only.
- Line numbers in several issues have drifted: `audiofeed.rs` `lock` is now at `:417` (issue said `:302`), and `pipe.rs` `POLL_INTERVAL` is at `:28` (issue said `:27`). The code they name is unchanged.
- #11 also lists `probe.rs` `stderr_tail` (`:324`) as a fifth variant, which the issue did not mention.
- Priority P0 is given to #194 and #10 because each yields wrong behaviour or lost data with no error shown.
