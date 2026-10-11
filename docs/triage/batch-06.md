# Triage batch 06 (clipcheckd, clipedit, core)

Judged at commit b6e6525 (HEAD) using `git show HEAD:<path>`. Closed: none. No gh writes were made.

| issue | title (short) | verdict | evidence (file:line at HEAD) | closed? | fix size / priority |
|---|---|---|---|---|---|
| #190 | Any games-map edit restarts detector | STILL-VALID | `apps/clipcheckd/src/service.rs:819-827`: `old.games != self.config.games` still triggers `restart_detection()`, though the detector only reads `display_name` (`service.rs:1425-1431`). | no | S / P1 |
| #32 | `format_hms_file` duplicates `format_hms` arithmetic | STILL-VALID | `crates/clipedit/src/plan.rs:214-221` and `:224-230` repeat the same h/m/s/clamp expressions. | no | S / P3 |
| #71 | KeyChord serialize panics on unnamed vk | STILL-VALID | `crates/core/src/config/schema.rs:1380` (`ok_or(fmt::Error)?`), `:1386-1388` and `:1416` call `to_string()`; `validate.rs` has no vk-name check (only VK_F4/VK_L at `:72-73`, `:897-898`). Reachable via `KeyChord::alt(vk)` (`schema.rs:1303`) for any vk. | no | S / P2 |
| #79 | Game key formats disagree (validate / docs / runtime) | STILL-VALID | `service.rs:1425-1431` matches only whole key or `exe:` prefix; `validate.rs:1058` and `:1012` accept `cs2.exe@steam:730` but never match it at runtime; `service.rs:1435` still builds `exe:` ids. | no | M / P0 (display name silently ignored) |
| #87 | GameId normalization not enforced | STILL-VALID | `crates/core/src/ids.rs:96-103` lowercases store name but not `store_id`; `ids.rs:84-85` `#[serde(transparent)]` derive accepts any string. Tests in `crates/core/tests/ids.rs` do not cover either. | no | S / P2 |
| #92 | Game override keys not normalized; `.exe` case inconsistent | STILL-VALID | `crates/core/src/config/validate.rs:1058` is case-sensitive on `ends_with(".exe")`; keys are stored as written (no canonicalization on load). | no | S / P2 |
| #99 | Unknown-key policy contradicts docs; CFG-P004 never emitted | STILL-VALID | `schema.rs:80` and `docs/design/09-settings-and-config.md:165,1041` promise CFG-P004; `git grep CFG-P004 HEAD` finds only comment and doc. `schema.rs:896` and `:914` use `deny_unknown_fields`; `crates/core/tests/config_schema.rs:148-165` encode the split. No `serde_ignored` in `crates/core/Cargo.toml`. | no | M / P2 |
| #104 | Stale comments in config module | STILL-VALID | `crates/core/src/config/mod.rs:1` says "owned by tasks B1/B2"; `schema.rs:31` says "Always 1 in v1" while `validate.rs:80` only rejects `> CONFIG_VERSION` (0 accepted); `schema.rs:80` cites unemitted CFG-P004. | no | S / P3 |
| #112 | ids and media modules have no consumers outside core | STILL-VALID (premise partly wrong) | No use of `clipcheck_core::ids`/`media`, `GameId`, `SessionId`, `ClipId`, `VideoParams`, `ticks_to_ms`, etc. outside `crates/core` at HEAD. Correction: engine is not "without importing anything": `crates/engine/src/catalog.rs:386` and `crates/engine/src/recorder/naming.rs:27` use `clipcheck_core::config::schema::sanitize_game_folder`, so the engine dependency must stay. `TICKS_PER_SEC` duplicate is at `crates/buffer/src/index.rs:12`. | no | M / P3 |
| #118 | MAX_FILE_BYTES declared but never enforced | STILL-VALID | `crates/core/src/config/schema.rs:13-14` constant has no references; `apps/clipcheckd/src/configstore.rs:300` and `apps/clipcheckd/src/app.rs:349` call `fs::read_to_string` with no size check. Design doc `09-settings-and-config.md:1279` still lists a "file size guard". | no | S / P2 |

## Verdict counts
- ALREADY-FIXED: 0
- OBSOLETE: 0
- PARTIAL: 0
- UNSURE: 0
- STILL-VALID: 10 (#190, #32, #71, #79, #87, #92, #99, #104, #112, #118)
