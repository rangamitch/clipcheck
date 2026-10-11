# Triage batch 03 (capture, clipcheckd)

Judged against HEAD b6e6525 with `git show HEAD:<path>`. No issue was closed in this batch.

| issue | title (short) | verdict | evidence (file:line) | closed? | fix size / priority |
|---|---|---|---|---|---|
| #25 | `is_environment_error` substring match hides real WGC failures | STILL-VALID | `crates/capture/tests/wgc_window.rs:316-335` matches on `context` plus keywords incl. `"device"` and `"winrt"`; early `return` on match at lines 353, 546, 571, 594 | no | S / P2 (test quality: masks regressions as SKIP) |
| #29 | Pacer copies frame up to 3x per tick, allocates black frame per tick | STILL-VALID | `crates/capture/src/lib.rs:230` (`lat.bgra.clone()`), `:234` (`last_live = Some(f.clone())`), `:237` (`last_live.clone()` on duplicate), `:241` (`blackgen::black_bgra` per tick) | no | M (`Frame.bgra` becomes `Arc`, pub API change, check engine callers) / P1 |
| #31 | Shared mutex held during pacer copy and black-frame build | STILL-VALID | `crates/capture/src/lib.rs:220` `let lat = lock(&shared);` held through the match that clones (`:230`) and builds black frames (`:241`); publisher waits at `crates/capture/src/wgc.rs:329` | no | S-M (shrink critical section; best after #29) / P1 |
| #33 | `read_rows` allocates a new frame-sized Vec per frame | STILL-VALID | `crates/capture/src/wgc.rs:373` `let mut out = vec![0u8; row_bytes * h as usize];`; old buffer dropped by `lat.bgra = bgra` at `:333` | no | M (double buffer + swap, coordinate with #29) / P1 |
| #35 | Window state polled with four Win32 calls each tick | STILL-VALID | `crates/capture/src/lib.rs:211` calls `blackgen::is_window_showing`; `crates/capture/src/blackgen.rs:19-34` (IsWindow, IsWindowVisible, IsIconic) plus `:36-49` (DwmGetWindowAttribute) | no | M (measure first, then low-rate poll or event cache) / P1 |
| #164 | wgc_window flow test deep-copies every frame just to check pts order | STILL-VALID | `crates/capture/tests/wgc_window.rs:384, 418, 453, 486` (`timeline.extend(...cloned())`), `:389` (moves `pre`), check at `:489` | no | S (collect `pts_100ns` only) / P2 (test memory and timing) |
| #44 | Event `seq` assigned before subscriber lock, so out-of-order delivery is possible | STILL-VALID | `apps/clipcheckd/src/events.rs:64` `fetch_add` happens before `self.lock()` at `:65`; `send` with that `seq` at `:70-75` | no | S (take seq under the lock, add concurrency test) / P0 (wrong behaviour; low likelihood) |
| #48 | Broken `config.toml` is overwritten with defaults on the next patch | STILL-VALID | `apps/clipcheckd/src/app.rs:340-378` `load_config` returns `Config::default()` on read, parse and validation errors; `apps/clipcheckd/src/service.rs:936-961` `patch_config` applies to `self.config` and calls `self.store.save(&next)`; no load-error state anywhere in `apps/clipcheckd/src` | no | M (store load error, reject patch until reload) / P0 (user data loss) |
| #51 | Timed-out IPC call still runs; `Busy` is retryable so retries double-apply | STILL-VALID | `apps/clipcheckd/src/service.rs:310-314` maps timeout to `Busy`; `:454-456` the queued call still runs and its reply is dropped; no cancel flag in `service.rs`; `apps/clipcheckd/src/server.rs:513` `retryable = e.code == Busy` | no | M (op ids or cancel flag, plus stall test) / P0 (duplicate clips and splits) |
| #55 | Status DTO reports placeholder values (`mic_muted`, `detection_paused`, `buffer_bytes`, `worker_pid`, `worker_restarts_10min`) as real state | STILL-VALID | `apps/clipcheckd/src/service.rs:1308` (`buffer_bytes: 0`), `:1316-1320` (`mic_muted: false`, `detection_paused: false`, `worker_pid: None`, `worker_restarts_10min: 0`); MuteMic is a no-op at `:801-803`; the DTO fields are plain non-Option at `crates/ipc/src/proto.rs:913-925` | no | S (Option or remove fields) / P0 (wrong status shown to clients) |

## Totals
- ALREADY-FIXED: 0
- OBSOLETE: 0
- STILL-VALID: 10
- PARTIAL: 0
- UNSURE: 0

## Notes
- `autostart` and `uptime_s` placeholders in `status_dto` are overwritten in `apps/clipcheckd/src/app.rs:165-172`, so they are not part of the defect (#55 mentions them only as a cleanup).
- #29 and #33 change the same data path (`Frame.bgra` storage). #31 is easiest after #29. Do them together.
