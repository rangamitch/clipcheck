# Triage batch 12 (engine area)

Judged against commit b6e6525 (HEAD), read with `git show HEAD:<path>` and `git grep ... HEAD`.
Working-tree edits were not used as evidence.

| issue | title (short) | verdict | evidence (file:line, one sentence) | closed? | fix size / priority |
|---|---|---|---|---|---|
| #156 | Tests use fixed sleeps (about 70 s idle) | STILL-VALID | `crates/engine/tests/recorder_tests.rs:670,692,716,809,864,986` and `pipeline_tests.rs:681,707` still sleep 300 ms to 9 s; `tracker_tests.rs:154` still sleeps `SEG_SLEEP` after each copy. | no | M / P2 |
| #157 | naming: `civil_from_days` unclear, missing date-edge tests, temp dir leak | STILL-VALID | `crates/engine/src/recorder/naming.rs:77` still has `let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;`; the test module (lines 107-170) has no pre-1970, century or 9999 cases, and `:163,169` still use `temp_dir` with `remove_dir_all`. | no | S / P3 |
| #158 | Test files: blanket lint allows, unused static, always-on eprintln | STILL-VALID | `crates/engine/tests/pipeline_tests.rs:1` and `audiofeed_tests.rs:1` still have crate-wide `allow(clippy::chunks_exact_to_as_chunks)`; `pipeline_tests.rs:430` `static COLOR: AtomicU32` is still a static; `recorder_tests.rs:157` still prints every unrelated event; `SERIAL` is still duplicated at `audiofeed_tests.rs:36` and `recorder_tests.rs:18`. | no | S / P3 |
| #159 | `attach_or_skip` skips on `Io`/`Unsupported`, hiding attach regressions | STILL-VALID | `crates/engine/tests/recorder_tests.rs:477-486` and `audiofeed_tests.rs:761-770` still match `Err(e @ (RecorderError::Io(_) \| RecorderError::Unsupported))` and skip; `recorder_tests.rs:492` still falls back to bare `ffmpeg.exe`. | no | M / P2 |
| #162 | `fit_to_canvas` divides per pixel and allocates a discarded canvas | STILL-VALID | `crates/engine/src/pipeline.rs:52-89`: the canvas is allocated and alpha-filled (lines 53-54) before the same-size early return (lines 60-62), and `sx` is recomputed with a 64-bit multiply and divide inside the `dx` loop (line 84). It is called on the hot path at `pipeline.rs:468`; I did not verify the issue's claim that `encode_one` bypasses it. | no | S / P1 |
| #170 | One unreadable segment stops the tracker, and the error is dropped | STILL-VALID | `crates/engine/src/tracker.rs:128` returns early on `probe_duration` error so `next_seq` (line 157) never advances; `tracker.rs:104-105` breaks silently when `out` is non-empty; `tracker.rs:200` discards the `poll` result with `let _ =`; no retry limit or skip exists. | no | M / P0 |
| #171 | Undeletable evicted file makes tracker retry the whole list forever | STILL-VALID | `crates/engine/src/tracker.rs:174-175` uses `?` before `pending_evict.clear()`, so the list is kept and re-sent each poll; `crates/buffer/src/ringfs.rs:87-105` returns one aggregated `Err`; `tracker.rs:108-112` still drops the error when `out` is non-empty. | no | M / P2 |
| #172 | `sessionfile::recover` has no production caller; `staged_duration_hint` is test-only | STILL-VALID | `crates/engine/src/sessionfile.rs:149` `recover` is called only from `crates/engine/tests/sessionfile_tests.rs:438-450`; `sessionfile.rs:93` `staged_duration_hint` is called only from `sessionfile_tests.rs:341-354`; `recover` still scans twice (`sessionfile.rs:154` and inside `SessionAssembler::new`). | no | S-M (needs a scope decision) / P2 |
| #193 | Audio source failures surface only at stop | STILL-VALID | `crates/engine/src/recorder.rs:304` and `:368` are the only `feeder.warnings()` calls in the engine, with the one at `:368` run at stop; `audiofeed.rs:379` pushes the "source stopped" warning mid-session with no live drain. | no | M / P0 |
| #197 | Crashed-session run dirs and staged parts are never removed | STILL-VALID | `crates/engine/src/recorder.rs:279-280` creates `work_dir/<run_id>`, and it is removed only at `:311` (failed start) and `:392` (detach); no startup sweep exists in `apps/clipcheckd/src` or `crates/engine/src`; `Cargo.toml:23` still has `panic = "abort"`. | no | M-L (depends on #172) / P0 |

## Counts

- ALREADY-FIXED: 0
- OBSOLETE: 0
- STILL-VALID: 10
- PARTIAL: 0
- UNSURE: 0

## Closed

None. Nothing qualified as ALREADY-FIXED or OBSOLETE, so no `gh issue close` was run.

## Notes

- #170, #171 and #197 each give a 2-second tracker or crash path, so they are all sound and still open.
- #197 and #172 are linked: salvage (#172) is the only way the crashed session data becomes useful, so the fix order is #172 scope decision, then #197.
- #193 has an upstream half in `crates/audio` (#84) that I did not check.
