# Triage batch 04 (clipcheckd), judged at commit b6e6525 (HEAD)

Code was read with `git show HEAD:<path>` and `git grep ... HEAD`, not from the working tree.
Line numbers are HEAD line numbers. Several issues cite old line numbers; the cited code was found by name.

| issue | title | verdict | evidence | closed? | fix size / priority |
|---|---|---|---|---|---|
| #60 | Errors in service.rs silently become 0 or empty | STILL-VALID | `service.rs:1116-1123` `media_duration_ms` returns 0 on `Err(_)` with no log; `service.rs:1486-1488` `file_len` `unwrap_or(0)` with no log; `service.rs:1326-1331` `sessions_dir` `unwrap_or_default()`; `service.rs:1061-1062` `None => (0, 0)` still publishes `SessionPartFinished` with the empty `session_id` from `service.rs:1040-1041`. | no | M / P2 |
| #63 | Dead code hidden by allow attributes | STILL-VALID | `settings_map.rs:10-13` still says the module is unused and `settings_map.rs:13` is `#![allow(dead_code)]`, though `service.rs:483,547,856,888` call it; `branding.rs:6-7` `APP_NAME` is unused in all of `apps/`, and `branding.rs:32,54,62,76` keep `#[allow(dead_code)]` on `load_tray_icon`/`destroy_icon`, which `tray.rs:486,673` use. | no | S / P3 |
| #70 | Integration test harness copy-pasted | STILL-VALID | No `apps/clipcheckd/tests/common/` exists; `lock`, `Daemon`, `unique_pipe` are copied in `tests/service_tests.rs:43-64`, `tests/settings_tests.rs:41-72`, `tests/daemon.rs:25-36`, `tests/discovery_tests.rs:46-72` and `tests/tray_tests.rs:117-133`; `wide()` is copied in four files. | no | L / P2 |
| #73 | Tests that cannot run print SKIP and pass | STILL-VALID | `tests/service_tests.rs:942,948` and `:1106,1112` print SKIP and return on window or FFmpeg failure; same in `tests/settings_tests.rs:1203-1209` and `:1295-1301`; `tests/tray_tests.rs:338-345` `skip_without_tray!`; no `#[ignore]` or `CLIPCHECK_REQUIRE_*` gate anywhere in `tests/`. | no | M / P2 |
| #78 | Hotkey registration and failure mapping duplicated | PARTIAL | Fixed: the FailureReason-to-text mapping and per-failure formatting now live once in `service.rs:509-523` `warn_hotkey_failures`, called from `start_hotkeys` (`:482`) and `apply_hotkeys` (`:883`). Still present: bindings build and warnings forwarding, registry build and conflict loop are duplicated at `service.rs:483-497` (start) and `service.rs:888-902` (apply), differing only in the verb "registered"/"changed". | no | S / P3 |
| #83 | Clip length validated in server.rs and service.rs | STILL-VALID | `server.rs:358-364` checks `(1..=MAX_CLIP_SECONDS).contains(&seconds)`; `service.rs:1246-1250` checks `seconds == 0 \|\| seconds > MAX_CLIP_SECONDS` with the same message. | no | S / P3 |
| #86 | Startup config loader duplicates ConfigStore::read | STILL-VALID | `app.rs:340-378` `load_config` (called from `app.rs:227`) does its own `fs::read_to_string`, `Config::from_toml_str` and `validate` loop with a hand-kept `has_error`, while `configstore.rs:212` `parse_text`, `:236` `has_errors` and `:299` `read` provide the same logic for reload. | no | M / P2 |
| #90 | Protocol error classification matches serde text; hello message hardcodes version | STILL-VALID | `server.rs:162` `message.contains("unknown variant")` picks `ErrorCode::UnknownType`; `server.rs:297` hardcodes `"hello must use frame version 1"` while the check at `server.rs:291` uses `PROTOCOL_VERSION` (`crates/ipc/src/proto.rs:9`, currently `1`). | no | S / P2 |
| #93 | Tray menu maps string keys to ids; unknown keys become Exit | STILL-VALID | `tray.rs:72-74` `menu_items()` still returns `(&str, &str)` pairs; `tray.rs:700-706` `show_menu` maps any key other than `"open"` to `MENU_ID_EXIT`. | no | S / P2 |
| #98 | Unreachable defensive branches hide invariants | STILL-VALID | `configstore.rs:82-89` has `contains_key` then `get_mut` with a `let Some(child) = ... else { continue }`; `settings_map.rs:485-487` `if write!(text, "{keys}").is_err()` writes to a `String`; `service.rs:1216-1220` `split_session` uses `unwrap_or_default()` after `require_session()` at `:1212`; `tray.rs:834` `u32::try_from(size_of::<NOTIFYICONDATAW>()).unwrap_or(0)`. | no | S / P3 |

## Totals

- ALREADY-FIXED: 0
- OBSOLETE: 0
- STILL-VALID: 9 (#60, #63, #70, #73, #83, #86, #90, #93, #98)
- PARTIAL: 1 (#78)
- UNSURE: 0

## Closed issues

None. The only resolved part (#78) is partial, and no issue met the strict ALREADY-FIXED or OBSOLETE bar.

## Notes

- #73 is broader than the issue text: `tests/discovery_tests.rs:972-1281` has five more SKIP paths (window, ffplay, render endpoint, capture endpoint x2) that the issue does not list.
- #70 also omits `tests/discovery_tests.rs`, which has its own copy of `lock`, `Daemon`, `unique_pipe` and `wide`.
- #98 line numbers moved: the `split_session` case is at `service.rs:1216-1220`, not `:1155-1159`.
- #78 is the only issue where HEAD code has moved on; the remaining duplication is smaller than the issue describes.
