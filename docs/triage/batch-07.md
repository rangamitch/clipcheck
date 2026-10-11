# Triage batch 07 (core, detect), judged at HEAD b6e6525

All ten issues are still present at HEAD. No issue was closed.

| issue | title | verdict | evidence | closed? | fix size / priority |
|---|---|---|---|---|---|
| #123 | Two `Codec` enums with conflicting variants (Hevc vs H265) | STILL-VALID | `crates/core/src/config/schema.rs:991-999` defines `Codec { H264, Hevc, Av1 }`, `crates/core/src/media.rs:14-18` defines `Codec { H264, H265, Av1 }`, and `apps/clipcheckd/src/settings_map.rs:85` maps `Codec::Hevc => CodecKind::H265`. | no | M / P3 |
| #128 | Win32 key codes and limits duplicated across core, hotkeys, buffer | STILL-VALID | `MAX_CLIP_HOTKEYS = 8` is defined in `crates/core/src/config/validate.rs:66` and `crates/hotkeys/src/registry.rs:9`; `TICKS_PER_SEC` is defined in `crates/core/src/media.rs:10` and `crates/buffer/src/index.rs:12`; VK constants sit in `validate.rs:72-73` and `schema.rs:1294-1299`. | no | S / P2 |
| #134 | KeyChord key names narrower than hotkeys crate | STILL-VALID | `crates/core/src/config/schema.rs:1317-1327` `vk_from_name` accepts only one A-Z/0-9 char or F1-F24, and `name_from_vk` at `schema.rs:1330-1336` mirrors that set, so `Left`, `Insert`, `Numpad0` and similar cannot be stored in config. | no | S / P2 |
| #138 | `normalize_exe` duplicated four ways, dead `unwrap_or` | STILL-VALID | `crates/core/src/ids.rs:131-135` strips `.exe` and keeps the `rsplit(...).unwrap_or(name)` fallback, while `crates/detect/src/lists.rs:101-104` keeps `.exe` (the doc example `Foo.EXE` -> `foo.exe`); `ExeName::from_str` (`schema.rs:1245`) and `is_valid_game_id` (`validate.rs:1052`) are the other two copies. | no | M / P2 (can cause mismatched game ids across crates) |
| #141 | `serde_json` is a runtime dependency used only by tests | STILL-VALID | `crates/core/Cargo.toml:9` lists `serde_json` under `[dependencies]`; the only uses are in `crates/core/tests/ids.rs`, `tests/media.rs` and `tests/updates_config.rs`, and none in `crates/core/src`. | no | S / P3 |
| #145 | Needless allocations in KeyChord Display, normalize_exe, validate labels | STILL-VALID | `crates/core/src/config/schema.rs:1366-1384` builds a `Vec<String>` for `Display`; `ids.rs:133-134` does `lower.strip_suffix(...).unwrap_or(&lower).to_string()`; `validate.rs:779` and `validate.rs:826-827` format path/label strings eagerly for every entry. | no | S / P3 |
| #191 | Default config header promises ~300 ms hand-edit pickup, no watcher | STILL-VALID | `crates/core/src/config/schema.rs:1436` template says "Hand edits are picked up within ~300 ms", `apps/clipcheckd/src/app.rs:145` says "hot-reload is a later task", and a grep of `apps` and `crates` at HEAD finds no `notify` or `ReadDirectoryChanges` file watcher. | no | S / P2 (docs are wrong for every user) |
| #2 | Empty exe name not rejected, all unreadable processes collide on game id `""` | STILL-VALID | `crates/detect/src/win.rs:207-218` returns `(String::new(), 0)` on `OpenProcess` failure; `crates/detect/src/score.rs:133-150` in `evaluate_with` has no empty-name gate, and `lists.rs:149-170` lookups return false for `""`, so the candidate is scored and can be picked as game `""`. | no | S / P0 (wrong behaviour: unrelated windows can be recorded as one game) |
| #3 | Relaunch title falls back to `""` via `unwrap_or_default` | STILL-VALID | `crates/detect/src/runner.rs:213-214` re-derives the title from `chosen` with `.unwrap_or_default()`, and `crates/detect/src/debounce.rs:102-111` `GameRelaunched` has no `display_name` field, populated at `debounce.rs:378-383` without a name. | no | S / P1 (wrong title shown for relaunch; only when `chosen` is None) |
| #4 | Hysteresis bypassed when bound window fails gates (`bound_score` stays 0) | STILL-VALID | `crates/detect/src/runner.rs:122` initialises `bound_score = 0` and `runner.rs:138` sets it only inside the accepted branch, so when the bound window is rejected the check at `runner.rs:175` (`score >= bound_score + RETARGET_MARGIN`) becomes `score >= 15` and can switch windows. | no | S-M / P0 (wrong behaviour: flapping between windows of one game) |

## Counts

- ALREADY-FIXED: 0
- OBSOLETE: 0
- STILL-VALID: 10
- PARTIAL: 0
- UNSURE: 0

## Notes

- Line numbers in the issues had moved at HEAD (for example `Codec` in `schema.rs` is now at 991 and `normalize_exe` in `ids.rs` at 131). The verdicts above use HEAD line numbers.
- #145 cites `validate.rs:503/625/672/686`; those exact lines no longer hold the same formats at HEAD, but the eager-`format!` pattern is still present, so the issue remains valid.
- #138 and #123 both touch the encode crate's `CodecKind` (`crates/encode/src/probe.rs:37-45`), a fourth codec name not listed in the issues.
