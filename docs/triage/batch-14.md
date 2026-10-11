# Triage batch 14: ipc, library, misc, docs

Judged at commit b6e6525 (HEAD) using `git show HEAD:<path>` and `git grep ... HEAD`. Line numbers are HEAD line numbers, which differ from the numbers in the issues.

| issue | title (short) | verdict | evidence (file:line) | closed? | size / priority |
|---|---|---|---|---|---|
| #151 | AudioLevels event declared and consumed, never published | STILL-VALID | `git grep AudioLevels HEAD` finds only `crates/ipc/src/proto.rs:671,799`, the ipc tests and docs. No publisher exists in `crates/engine` or `apps/clipcheckd`. | no | M (implement) or S (remove variant, topic and UI branch) / P2 |
| #179 | PipeConn cannot cancel a pending overlapped read | STILL-VALID | `crates/ipc/src/pipe.rs` has no `CancelIoEx` or `cancel_pending` at HEAD. `pipe.rs:243` maps only `ERROR_BROKEN_PIPE` and `ERROR_HANDLE_EOF`. `apps/ui/src-tauri/src/ipc.rs:9-11` still says the pipe has no cancel. | no | M / P2 |
| #28 | bad_enum reports column index 0 for kind and status | STILL-VALID | `crates/library/src/db.rs:277,279,282` still pass literal `0` to `bad_enum`, which goes into `FromSqlConversionFailure` (`db.rs:266-272`). | no | S / P3 |
| #30 | apply_v1 hard-codes version literals instead of SCHEMA_VERSION | STILL-VALID | `crates/library/src/schema.rs:266` writes `fts_built_version = '1'`, which nothing reads (grep finds only that line). `schema.rs:269` writes `PRAGMA user_version = 1`. Note that migrations are stepwise, so the issue's fix must keep a per-step version and must not write SCHEMA_VERSION there. | no | S / P3 |
| #39 | list() and upsert_game prepare SQL per call | STILL-VALID | `crates/library/src/query.rs:314` uses `self.conn.prepare(&sql)`. `crates/library/src/db.rs:532` uses `tx.prepare` for the fixed game-id query. Neither uses `prepare_cached`. | no | S / P1 |
| #110 | audio_tracks table has no read or write API | STILL-VALID | `crates/library/src/schema.rs:106` creates the table. No library or engine code reads or writes it. `crates/engine/src/catalog.rs:140-151` parses audio tracks, which are not persisted. `crates/buffer/src/extract.rs:83,168` still run ffprobe for titles on each clip. | no | M (persist) or S (drop table in migration) / P2 |
| #147 | upsert_game rewrites FTS for all game recordings on every registration | STILL-VALID | `crates/library/src/db.rs:532-537` runs `fts_refresh(&tx, id, true)` for every recording of the game with no change check. `crates/engine/src/catalog.rs:387` calls `upsert_game` for each newly registered file. | no | S / P1 |
| #149 | failed case-only rename leaves .clipcheck-rename-*.tmp with no recovery | STILL-VALID | `crates/library/src/fsops.rs:466` comment still promises "the reconciler picks it up". `git grep reconcil HEAD -- crates` finds no implementation, only `schema.rs:27` (`last_reconcile_ms`, never used). The leftover `.tmp` path (`fsops.rs:493`) is not a registered media extension. | no | M / P0 (wrong behaviour: recording shown missing, file left under an unknown name) |
| #168 | buffer and capture tests assert too little | STILL-VALID | `crates/buffer/tests/ringfs_tests.rs:294` only checks `.is_ok()` for `free_space_ok(1)`. `ringfs_tests.rs:326-330` still uses `if let Ok` around the scan assertion. `crates/capture/tests/wgc_window.rs:510` accepts `CaptureError::Unsupported`. `wgc_window.rs:521-531` checks InvalidFps with an unreal HWND. | no | S / P2 |
| #185 | Design 10 section 2.5 contradicts design 09 and the close_behavior code | STILL-VALID | `docs/design/10-app-shell-ipc-and-packaging.md:97` still says "Closing does not minimize to tray". `docs/design/09-settings-and-config.md` defines `close_behavior` with default `minimize_to_tray`. `crates/core/src/config/schema.rs:123` defaults to `MinimizeToTray`, and `apps/ui/src-tauri/src/closing.rs:66` implements it. | no | S / P3 |

## Summary

- ALREADY-FIXED: 0
- OBSOLETE: 0
- STILL-VALID: 10
- PARTIAL: 0
- UNSURE: 0

No issues were closed in this batch, because no issue had a resolved premise at HEAD.
