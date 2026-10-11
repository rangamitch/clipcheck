# Triage batch 16 (tauri shell, apps/ui/src-tauri)

Judged against HEAD b6e6525 with `git show HEAD:<path>`. Line numbers are HEAD line numbers; several issues cite lines that have since moved, noted where relevant.

| issue | title | verdict | evidence | closed? | fix size / priority |
|---|---|---|---|---|---|
| #96 | Shutdown request and timeout duplicated (closing.rs vs exit_app) | STILL-VALID | `closing.rs:23` and `ipc.rs:40` both define `SHUTDOWN_ACK_TIMEOUT`; `closing.rs:252` and `ipc.rs:472` both send `{"type":"shutdown"}` with it. | no | S / P2 |
| #101 | Test helpers copy-pasted across three test files | STILL-VALID | `body_type` at `closing_tests.rs:174` and `ipc_client_tests.rs:196`; `unique_pipe` at `closing_tests.rs:33`, `ipc_client_tests.rs:27`, `single_exe_tests.rs:39`; `TEST_LOCK` declared in all three. No `tests/common/mod.rs` exists (only the unrelated `tests/common_player/mod.rs`). Issue line numbers are a few lines stale. | no | M / P3 |
| #107 | Tests use fixed sleeps instead of polling | STILL-VALID | Fixed 300 ms sleeps before the negative asserts at `ipc_client_tests.rs:588` and `closing_tests.rs:538`; 1 s survival sleep at `single_exe_tests.rs:517`; 100 ms polls at `single_exe_tests.rs:530,536`. `wait_until` exists at `ipc_client_tests.rs:43`. | no | M / P2 |
| #114 | scripts/test-all.sh never runs src-tauri tests | STILL-VALID | `scripts/test-all.sh:17` loops only `crates/*/Cargo.toml apps/clipcheckd/Cargo.toml`. Surprise: a `.github/workflows/ci.yml` now exists, but its `ci.yml:78` only runs `cargo check` on src-tauri, and its header (`ci.yml:3-6`) wrongly says src-tauri tests run via test-all.sh. The gap is real and the comment is false. | no | S / P2 |
| #117 | Release profile does not reach the desktop app build | STILL-VALID | Only `[profile.release]` at `Cargo.toml:20`; `apps/ui/src-tauri/Cargo.toml:11` declares its own `[workspace]` with no `[profile]`; no `.cargo/config` at HEAD. `.github/workflows/release.yml` builds the NSIS bundle from `apps/ui/src-tauri`, so the shipped binary gets defaults. | no | S / P2 |
| #121 | IPC read loop clones every pushed frame body | STILL-VALID | `ipc.rs:414` `frame.get("body").cloned()` deep-copies the body on every frame, and `ipc.rs:421-423` mutates a further copy to insert `seq`. | no | S / P1 |
| #125 | UI always subscribes to the 10 Hz Levels topic | STILL-VALID | `ipc.rs:53` puts `Topic::Levels` in `ALL_TOPICS`, and `ipc.rs:333` subscribes to all of it. `daemon.ts:247-248` says `levels` is deliberately left out of `STATUS_TOPICS`, so the Rust default contradicts the frontend. | no | M / P1 |
| #131 | get_library_stats scans every recording; DB reopened per call | STILL-VALID | `commands.rs:211-235` pages all rows with `limit: 200` and sums in Rust; `commands.rs:242` and `251` call `Library::open_readonly` per call. No aggregate method in `crates/library/src` at HEAD (`query.rs:253` is only `list`). | no | M / P1 |
| #133 | closing.rs polls every 20 ms for the daemon client | STILL-VALID | `closing.rs:200-209` loops on `client_of` with `CLIENT_POLL` (20 ms, `closing.rs:27`) for up to `CLIENT_WAIT` (5 s), then returns silently at `closing.rs:205-206`. `lib.rs:67` still registers the client only after the client starts. | no | S / P2 |
| #135 | Dev seed uses try_from(...).unwrap_or(0) and magic numbers | STILL-VALID | `commands.rs:338-354` (the seed is `#[cfg(debug_assertions)]`, `commands.rs:282`, `DEV_SEED_COUNT = 60` at `commands.rs:305`); bare `200` page size at `commands.rs:216`. Issue line numbers are stale by about 5 lines. | no | S / P3 |

## Counts

- ALREADY-FIXED: 0
- OBSOLETE: 0
- STILL-VALID: 10
- PARTIAL: 0
- UNSURE: 0

## Closed issues

None. No issue met the ALREADY-FIXED or OBSOLETE bar, so no `gh issue close` was run.

## Notes

- #114: the issue says the repo has no CI workflow. That is no longer true at HEAD (`.github/workflows/ci.yml`), but CI still does not run the src-tauri tests, so the verdict holds.
- The `ci.yml` header comment (lines 3-6) states that src-tauri integration tests run on the dev machine via `scripts/test-all.sh`. That is false at HEAD, so the doc and the script disagree. Fixing #114 would make the comment accurate.
