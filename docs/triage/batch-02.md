# Triage batch 02 (buffer, capture) at commit b6e6525

Code read with `git show HEAD:<path>` / `git grep HEAD`. No issue was closed: none reached ALREADY-FIXED or OBSOLETE with proof.

| issue | title (short) | verdict | evidence (file:line, one sentence) | closed? | fix size / priority |
|---|---|---|---|---|---|
| #53 | purge_all stops at first error | STILL-VALID | `crates/buffer/src/ringfs.rs:135` still does `Err(e) => return Err(e)` inside `purge_all` (lines 129-139), while `delete_evicted` (lines 87-106) attempts every entry. | no | S / P2 |
| #58 | audio_title_args hides all ffprobe failures | STILL-VALID | `crates/buffer/src/extract.rs:105-116` returns `Vec::new()` on every failure path, and stderr is discarded at `extract.rs:97`; `extract_clip` (line 168) has no warning channel. | no | M / P2 |
| #115 | ffprobe spawned per clip and per finalize | STILL-VALID | `crates/buffer/src/extract.rs:168` and `crates/engine/src/sessionfile.rs:118` both still call `audio_title_args` (spawn plus JSON parse) with no caching or passed-in titles. | no | M / P1 |
| #119 | push-then-commit leaves unused OpenSegmentPending path | STILL-VALID | `crates/engine/src/tracker.rs:138-149` still pushes `committed: false` then commits under the same lock; `crates/buffer/src/index.rs:138-141` still refuses pushes while a segment is open, and no `push_committed` exists. | no | S / P3 |
| #166 | duplicated ffmpeg helpers, silent remove(0), per-test re-encode | STILL-VALID | `crates/buffer/tests/extract_tests.rs:17-36` and `crates/buffer/tests/audio_title_tests.rs:14-33` duplicate `vendor_bin_dir` with `bins.remove(0)`, there is no `tests/common`, and `extract_tests.rs:105` `fixture_with` re-encodes per call. | no | M / P2 |
| #167 | suspected B-frame defect as plain passing test | PARTIAL | The test at `crates/buffer/tests/extract_tests.rs:405-414` (fn at line 409) still has no `#[ignore]` and still says "suspected defect", but the mechanism it names (output-side `-ss` with `-c copy`) is gone: `crates/buffer/src/extract.rs:43-53` and `:200-203` now trim with a concat-demuxer `inpoint`; whether the test passes was not run. | no | S / P2 |
| #192 | TICKS_PER_SEC defined in buffer and core | STILL-VALID | `crates/core/src/media.rs:10` and `crates/buffer/src/index.rs:12` both define `10_000_000`, and `crates/engine/src/recorder.rs:30` imports the buffer copy although `crates/engine/Cargo.toml:8` depends on core. | no | S / P3 |
| #16 | read_rows publishes zero frame on bad pitch | STILL-VALID | `crates/capture/src/wgc.rs:383` skips the copy when `base` is null or pitch is short but `:391` still returns `Ok(out)` (zeroed), and the caller at `:329-333` bumps `lat.seq` and publishes it. | no | S / P0 |
| #18 | WGC callback errors and poisoned locks silent | STILL-VALID | `crates/capture/src/wgc.rs:285` (`Err(_) => return`), `:287` (`let _ = self.process`), `:214` (`if let Ok(mut slot) = cb_readback.lock()`) and `:133` (`if let Ok` in teardown) all still drop failures, and the capture crate has no logging (grep for tracing/log/eprintln returns nothing). | no | M / P2 |
| #20 | RoInitialize per WGC thread never balanced | STILL-VALID | `crates/capture/src/wgc.rs:97` calls `RoInitialize` per thread and `:115-117` still has the "Deliberately no RoUninitialize" comment with no `RoUninitialize`; note the per-thread COM count is released when the thread exits, so the "grows per start" impact in the issue is probably overstated, though the pattern is exactly as described. | no | S / P3 |

## Counts

- ALREADY-FIXED: 0
- OBSOLETE: 0
- STILL-VALID: 9 (#53, #58, #115, #119, #166, #192, #16, #18, #20)
- PARTIAL: 1 (#167)
- UNSURE: 0

## Closed issues

None.

## Notes for the orchestrator

- #167: the test should be changed or its status fixed. The premise (output-side `-ss`) no longer applies, so a rerun of that test on Windows would settle whether the defect is real. Not run here.
- #20: the claimed COM refcount growth should be checked with the test suggested in the issue before any change.
- #16 is the only P0 in this batch (zero frame emitted as live).
