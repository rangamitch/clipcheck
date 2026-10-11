# Triage batch 13 (hotkeys, ipc)

Judged at commit b6e6525 (HEAD), reading code with `git show HEAD:<path>`.

| issue | title | verdict | evidence | closed? | size / priority |
|---|---|---|---|---|---|
| #38 | apply() timeout reports Stopped while request still runs | STILL-VALID | `crates/hotkeys/src/win.rs:226-227` maps every `recv_timeout` error to `ApplyError::Stopped` (no Timeout variant at win.rs:84-87); `win.rs:315` ignores the reply send failure, so the late request is still applied. | no | M / P0 (wrong behaviour: caller told "stopped" while registrations change) |
| #40 | setup failures report fabricated Win32 code 0 | ALREADY-FIXED | Startup failures are typed `StartError::Spawn/NoReply/Window` (`crates/hotkeys/src/win.rs:171-190`, enum at 102-109); the `Os` code comes from the real HRESULT (`win.rs:378-398`); daemon prints `hotkeys not started: {e}` (`apps/clipcheckd/src/service.rs:498-503`). No `all_failed(..., 0)` remains. | yes | - / - |
| #42 | win_tests print SKIP and pass when SendInput blocked | STILL-VALID | `crates/hotkeys/tests/win_tests.rs:82-86` and `:155-160` (also `:298-300`, `:354-356`, `:410`) `println!("SKIP")` then `return`, so the test passes. | no | S / P2 (test quality) |
| #43 | MAX_CLIP_HOTKEYS defined twice; public alias unused | STILL-VALID | `crates/hotkeys/src/registry.rs:9` and `:144` (pub alias), `crates/core/src/config/validate.rs:66` (second copy); the alias is only referenced in `crates/hotkeys/tests/registry.rs:248`. | no | S / P3 (cleanup) |
| #46 | needless String allocations in Chord Display, validation, press handler | STILL-VALID | `crates/hotkeys/src/chord.rs:357-372` builds `Vec<&str>` and `join`s; `registry.rs:159` formats `chord_text` eagerly; `win.rs:293` clones `HotkeyAction` on every WM_HOTKEY press, `win.rs:354,357` clone at registration. | no | S / P1 (press path, minor) |
| #169 | registry tests: duplicate assert, loose >=, trivially true check | STILL-VALID | `crates/hotkeys/tests/registry.rs:138` duplicates the loop check at 134-137; `:268` uses `errs.len() >= 3`; `:286` `let _: &dyn std::error::Error = c;`; `:248` constant assertion. | no | S / P2 (test quality) |
| #34 | serde defaults on required v1 fields hide missing data | STILL-VALID | `crates/ipc/src/proto.rs:475-476` (`path` default), `:637-641` (`started_unix_ms`, `parts_saved`), `:937-948` (`game_info`, `buffer_secs`, `settings_pending_restart`, `server_unix_ms` all `#[serde(default)]`), `:1003` (`SessionInfo.parts_saved`). | no | M / P2 (robustness) |
| #36 | write_frame issues two overlapped writes per frame | STILL-VALID | `crates/ipc/src/frame.rs:49-50` separate `write_all` for the 4-byte header and the body. | no | S / P1 (hot path: levels ~10 Hz) |
| #37 | every overlapped pipe read/write creates and closes a kernel event | STILL-VALID | `crates/ipc/src/pipe.rs:278-282` `overlapped()` calls `create_event()` per operation; `pipe.rs:308-312` `CreateEventW` per call; no `ResetEvent` reuse anywhere. | no | L / P1 (hot path perf) |
| #148 | negotiate takes and returns u32 though versions are u16 | STILL-VALID | `crates/ipc/src/proto.rs:1120-1125` signature uses `u32`; `apps/clipcheckd/src/server.rs:302-320` widens inputs and the `u16::try_from` at `:320` is the unreachable branch. | no | S / P3 (cleanup) |

## Counts

- ALREADY-FIXED: 1 (#40, closed)
- OBSOLETE: 0
- PARTIAL: 0
- UNSURE: 0
- STILL-VALID: 9

## Notes

- #38 doc comment at `win.rs:199-200` already states that `Err` means "did not reply in time", so the behaviour is documented but the late-apply divergence is not.
- No GitHub writes other than the single `gh issue close 40`.
