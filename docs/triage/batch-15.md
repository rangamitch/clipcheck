# Triage batch 15 (docs, session, tauri)

Judged at commit b6e6525 (HEAD) using `git show HEAD:<path>` / `git grep ... HEAD`. Line numbers are HEAD line numbers.

| issue | title | verdict | evidence | closed? | fix size / priority |
|---|---|---|---|---|---|
| #198 | DESIGN.md D3 requires a `clipcheckd --worker` process that the code does not implement | STILL-VALID | DESIGN.md:122 still says "Adopt the shell doc ... spawns `clipcheckd --worker`", `git grep -- --worker HEAD -- apps crates` returns nothing, and `apps/clipcheckd/src/main.rs` calls `clipcheckd::run()` with no worker branch (only the in-process service thread exists). | no | S (rewrite D3 to the single-process decision) or L (implement worker) / P2 |
| #21 | Shutdown ignored while Stopping; pending auto-start can open a new part | STILL-VALID | `crates/session/src/lib.rs:226-250`: `stopping()` has no `Event::Shutdown` arm (falls to `_ => {}`), while Recording (165-168) and Splitting (218-221) set `shutdown_pending`. | no | S / P0 (wrong behaviour; latent because the reducer is not wired into the daemon) |
| #23 | `split()` enters Splitting without `EmitStatus` | STILL-VALID | `crates/session/src/lib.rs:365-369` assigns `s.state = State::Splitting` directly; `set_state` (434-437) is the only path that emits `EmitStatus`. | no | S / P2 |
| #26 | `EndReason::Recovered` declared but never produced | STILL-VALID | `crates/session/src/state.rs:91-92` declares the variant; the only other references are `tests/serde_roundtrip.rs:58`. Also `sessionfile::recover` is called only from engine tests, and `apps/clipcheckd/src/service.rs:1069` hard-codes `recovered: false`. | no | S (remove variant or wire it) / P3 |
| #41 | `reduce()` clones the whole SessionState on every event | STILL-VALID | `crates/session/src/lib.rs:21-22` `let mut s = state.clone();`. No production caller exists (no crate outside tests imports `clipcheck_session`), so the cost is latent. | no | S (take by value) / P3 |
| #146 | Reducer sends no Reply for user commands in Idle, Armed, Recording | STILL-VALID | Idle arms (`lib.rs:64-91`) have no UserStart/UserStop/UserSplit; Armed (95-122) replies only to UserStart with no game, UserStop/UserSplit fall through; Recording (126-169) has no UserStart arm; Stopping replies Busy (231-233). Nothing in `apps/` or `crates/engine/src` imports the reducer, so no live hang. | no | M / P2 |
| #82 | Empty `CLIPCHECK_PIPE_NAME` handled differently by spawn path and client | STILL-VALID | `apps/ui/src-tauri/src/daemon.rs:121` `std::env::var(PIPE_NAME_ENV).ok()` keeps an empty value and passes it on (line 136 / `daemon_args`), while `ipc.rs:281` filters `!s.is_empty()`. | no | S (one shared resolver) / P2 |
| #85 | DB path falls back to working directory; missing DB reads as empty gallery | STILL-VALID | `apps/ui/src-tauri/src/commands.rs:260-268` `db_path_from` ends with `PathBuf::from(DB_FILE_NAME)`; `page_at` (~238) and `stats_at` (~247) return empty results when `!db.exists()`. | no | S / P2 |
| #89 | Pipe-name resolution failure silently disables daemon connection | STILL-VALID | `apps/ui/src-tauri/src/ipc.rs:279-283` drops the `default_pipe_name()` error with `.ok()`; `manage` (`ipc.rs:286-289`) silently `return`s on `None`; `DaemonClient::start` (`ipc.rs:220`) returns no error. | no | M / P2 |
| #94 | Reader threads of failed handshakes never reclaimed while daemon is unresponsive | STILL-VALID | `ipc.rs:306-` spawns the reader before Hello/Subscribe; on failure it calls `link.close()`, which (`ipc.rs:173-182`) only wakes waiters and does not close the pipe, and the reader thread holds its own `Arc<Link>` and `reader_conn`. `BACKOFF_MAX` is 2 s (`ipc.rs:44`), so the leak is unbounded. | no | M / P1 |

## Counts

- ALREADY-FIXED: 0
- OBSOLETE: 0
- STILL-VALID: 10
- PARTIAL: 0
- UNSURE: 0

## Closed issues

None. No issue met the ALREADY-FIXED or OBSOLETE bar, so no `gh issue close` was run.

## Notes

- Most session findings (#21, #23, #41, #146) concern a reducer that no production code calls. Their priorities are latent until the reducer is wired in, so the priorities above are for when it is wired.
- Crash recovery is not wired: `sessionfile::recover` has no non-test caller, and session parts are always reported with `recovered: false` (`apps/clipcheckd/src/service.rs:1069`). This is relevant to #26 and to the crash-handling assumptions in #198.
- Line numbers in the issues had moved; all current references above were re-read at HEAD.
