# Triage batch 05 (clipcheckd), judged at commit b6e6525

Read-only triage against `git show HEAD:<path>`. No issue was closed, because every issue still reproduces at HEAD.

| issue | title (short) | verdict | evidence (file:line) | closed? | size / priority |
|---|---|---|---|---|---|
| #102 | Session parts probed twice, library lock held during ffprobe | STILL-VALID | `service.rs:1055` calls `media_duration_ms` after `register`, and `service.rs:1097-1098` holds the library lock while `catalog.rs:371` runs `probe_media` again | no | M / P1 |
| #108 | EventBus clones the full event per subscriber | STILL-VALID | `events.rs:71-73` does `event: event.clone()` per matching subscriber, and `Delivery.event` is an owned `Event` (`events.rs:16-20`) | no | S / P1 |
| #111 | Tray tooltip polls every 2 s but updates at most every 5 s | STILL-VALID | `app.rs:25` `TOOLTIP_POLL = 2 s`, `app.rs:327-329` calls `service.status()` each tick; `tray.rs:43` min interval is 5 s, and `tray.rs:330-336` skips unchanged text only after the poll and IPC round trip | no | S / P1 |
| #116 | Logger has no level filter, unbuffered write under global lock | STILL-VALID | `logging.rs:76-93` `log()` has no level threshold and formats before taking the lock; `logging.rs:125` does an unbuffered `write_all`; `server.rs:110,152,218` emit `debug` lines | no | S / P2 |
| #126 | Unused window title and unreachable `Msg::Stop` arm | STILL-VALID | `service.rs:666` `game_seen(..., _title)` is unused and `service.rs:673` / `:1293` set `title: None`; `service.rs:458` `Msg::Stop => {}` in `handle` is unreachable because `run` breaks at `:438` | no | S / P3 |
| #160 | `SessionPartStarted(N+1)` published before `SessionPartFinished(N)`; error path half-applies split | STILL-VALID | `service.rs:1211-1226` publishes `SessionPartStarted` directly after `session_split()`, while `recorder.rs:585` emits `SessionPartSaved` later through the recorder channel; `service.rs:1216` `?` returns before publish after the swap at `recorder.rs:425-441` | no | M / P0 |
| #161 | RenameRecording / DeleteRecording / RegisterEditedRecording fall to `not_implemented` | STILL-VALID | `server.rs:438` `_ => not_implemented()`, and the explicit library list at `server.rs:431-437` does not include the three variants (`crates/ipc/src/proto.rs:117-129` still defines them); grep finds no handler in `apps/clipcheckd/src`; primitives exist at `crates/library/src/db.rs:457`, `:495` and `fsops.rs:369` | no | M / P2 |
| #163 | Tests use fixed sleeps for negative checks and a loose log predicate | STILL-VALID | `tests/daemon.rs:280` fixed 500 ms sleep; `tests/daemon.rs:297-305` matches any log line containing "warning"; `tests/tray_tests.rs:427` fixed 3 s sleep before the exit-not check | no | S / P2 |
| #165 | Recorder stop, split and detach run synchronously on the service thread | STILL-VALID | `service.rs:1204-1208` `stop_session` calls `session_stop` on the service thread, `service.rs:1211-1226` runs `split_session`, and `service.rs:749-752` runs `recorder.detach()`, which joins workers (`recorder.rs:604`) | no | L / P1 |
| #173 | Dead hotkey service never restarted after `ApplyError::Stopped` | STILL-VALID | `service.rs:910-912` only warns on `ApplyError::Stopped` and keeps `self.hotkeys` as `Some`; it is cleared only in `stop` at `service.rs:603`; `win.rs:222-227` returns `Stopped` | no | S / P0 |

## Counts
- STILL-VALID: 10
- PARTIAL / ALREADY-FIXED / OBSOLETE / UNSURE: 0

## Notes
- The issue line numbers are out of date (the code moved in the later big commit), so the evidence above uses HEAD line numbers.
- #111: the "skip when unchanged" part from the proposal already exists in `tray.rs:330-336`, but the 2 s service poll is still there, so the issue stays valid.
- #161: the variants still exist in `crates/ipc/src/proto.rs`, so they are not obsolete.
- No `gh` writes were made, because none of the issues qualified for closing.
