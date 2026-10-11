# Triage batch 09 (area: encode)

Judged at commit b6e6525 (HEAD). Code read with `git show HEAD:<path>`. Line numbers are HEAD line numbers, which differ from several issue bodies.

| issue | title | verdict | evidence | closed? | fix size / priority |
|---|---|---|---|---|---|
| #95 | Shipped binary searches build machine's source tree for ffmpeg | STILL-VALID | `crates/encode/src/locate.rs:127-130` `from_vendor` still chains `env!("CARGO_MANIFEST_DIR")` as a search start. | no | S / P2 |
| #97 | Newest vendored ffmpeg chosen by reverse string sort (10.x loses to 7.x) | STILL-VALID | `crates/encode/src/locate.rs:148-149` `bins.sort(); bins.reverse();` on path strings, no numeric comparison. | no | S / P2 |
| #100 | Panicked probe thread silently drops encoders; redundant reorder pass | STILL-VALID | `crates/encode/src/probe.rs:148` `h.join().unwrap_or_default()` and the reorder loop at `probe.rs:152-162` are both present. | no | S / P2 |
| #106 | build_args_with_audio truncates ports and maps nonexistent inputs | STILL-VALID | `crates/encode/src/pipe.rs:376` `ports.iter().take(audio.len())`, while `audio_output_args` (`pipe.rs:416-419`) maps `i + 1` for every track; no length check in `validate`/`validate_audio`. | no | S / P2 |
| #113 | Dead branch and test-only public API in pipe.rs / audio_in.rs | STILL-VALID | `pipe.rs:275-278` `child.take().ok_or_else(...)` is unreachable (finish consumes self, child set only in start); `pipe.rs:216` `audio_track_count` and `audio_in.rs:97` `is_connected` have only test callers. | no | S / P3 |
| #120 | locate_honors_env_var mutates env while other tests run in parallel | STILL-VALID | `crates/encode/tests/probe_tests.rs:236,243` `env::set_var(ENV_VAR, ...)` while `real_paths()` (`probe_tests.rs:14-16`) reads the same variable in parallel tests. | no | S / P2 |
| #122 | Probe tests hardcode RTX 5080 and encoder count 11 | STILL-VALID | `crates/encode/tests/probe_tests.rs:72` names "RTX 5080"; literal `11` at lines 59, 167, 179, 191. | no | S / P2 |
| #124 | push_frame error path duplicates with_stderr; stderr_tail wrapper; ring_tail double copy | STILL-VALID | `pipe.rs:251-256` duplicates `with_stderr` (`pipe.rs:480-482`); wrapper `pipe.rs:291-293`; `ring_tail` at `pipe.rs:488` collects into `Vec<u8>` first. | no | S / P3 |
| #127 | Integration tests copy the same fixtures across three files | STILL-VALID | `tool_path`, `nanos`, `TempDir`, `frame` duplicated in `tests/audio_pipe_tests.rs:28-105`, `tests/audio_stress_tests.rs:32-105`, `tests/pipe_tests.rs:21-98`; no `tests/common/` module exists at HEAD. | no | M / P3 |
| #129 | fit_to_canvas allocates and zeroes a full canvas per resized frame | STILL-VALID | `crates/engine/src/pipeline.rs:53-57` allocates, zeroes and fills alpha per call; called per frame at `pipeline.rs:468`. The same-size `src.to_vec()` branch (`pipeline.rs:61-63`) is reached only from tests (`crates/engine/tests/pipeline_tests.rs:131`). Issue line numbers (51-63, 425-426) are stale; the premise holds. | no | M / P1 |

## Counts

- ALREADY-FIXED: 0
- OBSOLETE: 0
- STILL-VALID: 10
- PARTIAL: 0
- UNSURE: 0

## Closed

None. No issue met the strict ALREADY-FIXED or OBSOLETE bar, so no `gh issue close` was run.

## Notes

- #95 and #97 still apply to the rewritten `locate.rs`: the new env, pack store and exe-dir steps are present, but the `CARGO_MANIFEST_DIR` start and the reverse string sort remain.
- #129 issue line numbers are stale (the call site is `pipeline.rs:468`, not 425-426). The defect itself is unchanged.
