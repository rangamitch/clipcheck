# Triage batch 18 (ui frontend, apps/ui/src)

Judged at commit b6e6525 (HEAD), reading `git show HEAD:<path>`.

| issue | title | verdict | evidence | closed? | fix size / priority |
|---|---|---|---|---|---|
| 68 | Misleading comments on addTrack, PRESET_ROWS, InvokeFn | PARTIAL | `addTrack` no longer exists (grep finds none in apps/ui/src); PRESET_ROWS comment at `apps/ui/src/lib/settings/pure.ts:284` is now accurate; but the stale "Command used to exit" comment is still on the generic `InvokeFn` type at `apps/ui/src/lib/exitconfirm.ts:51` | no | S / P3 |
| 69 | Duplicated loops in fieldDiagnostic/diagnosticAt and formatChord/heldModifiersLabel | STILL-VALID | same severity loop at `apps/ui/src/lib/settings/pure.ts:105-117` and `pure.ts:258-266`; same modifier order at `apps/ui/src/lib/settings/hotkeys.ts:112-120` and `hotkeys.ts:123-130` | no | S / P3 |
| 72 | config_changed triggers two get_config calls | STILL-VALID | `apps/ui/src/lib/status.ts:455-456` calls `refreshConfig()` (get_config) and `apps/ui/src/lib/settings/store.ts:536-538` listener calls `checkExternal()` which calls `getConfig()` at `store.ts:469` | no | M / P1 |
| 74 | Every daemon event notifies liveStatus subscribers | STILL-VALID | `apps/ui/src/lib/status.ts:451` always calls `liveStatus.update`; `reduceEvent` returns `prev` for no-op events (`status.ts:307-327`), but `writable` (`status.ts:426`) still notifies on any object | no | S / P1 |
| 76 | Window resize sends isMaximized IPC per resize event | STILL-VALID | `apps/ui/src/lib/window.ts:58-60` calls `win.isMaximized()` inside every `onResized` callback, with no throttle or size check (a `debounce` helper exists at `apps/ui/src/lib/debounce.ts` but is not used here) | no | S / P1 |
| 80 | Gallery keeps every loaded page in the DOM | STILL-VALID | array copy per page at `apps/ui/src/lib/Gallery.svelte:92`, unvirtualized `{#each items as r (r.id)}` at `Gallery.svelte:262`, "Load more" at `Gallery.svelte:306` | no | L / P1 |
| 174 | Video preset help says Medium is default, shipped default is High | PARTIAL | the UI label is gone (no "Medium (default)" string in apps/ui/src at HEAD; presets are cards via `presetCards()` in `apps/ui/src/lib/settings/tabs/VideoTab.svelte:36`), and the schema default is High at `crates/core/src/config/schema.rs:366`; but the design doc still says Medium is default at `docs/design/03-encoding.md:45` and `docs/design/03-encoding.md:579`; no default-equals-default_toml test was added | no | S / P3 |
| 175 | "Applies after game ends" chip wrong for advanced keys and game overrides | STILL-VALID | heuristic unchanged at `apps/ui/src/lib/settings/pure.ts:125-137` (`games.` prefix, advanced.* listed); daemon classes at `crates/core/src/config/validate.rs:153-158` (advanced log_dir/ffmpeg_dir are Restart) and `validate.rs:163-170` (games: only folder is NextPart, others Live/PipelineReinit); used by `apps/ui/src/lib/settings/fields/FieldRow.svelte:38` | no | M / P2 |
| 176 | Allow/block lists keyed by item text, duplicate entry breaks Detection tab | STILL-VALID | `apps/ui/src/lib/settings/fields/ListEditor.svelte:87` `{#each items as item, i (item)}`; `check_detection` at `crates/core/src/config/validate.rs:477-508` checks only allow-vs-block overlap and empty allowlist, no duplicate check within a list | no | S / P2 |
| 177 | Unmute with no remembered gain sets 0 dB | OBSOLETE | `toggleMute`, `gainBeforeMute` and the Unmute/Mute gain label no longer exist in apps/ui/src; `apps/ui/src/lib/settings/tabs/AudioTab.svelte:131-133` is now a plain `setAppGain` slider; the only Mute/Unmute button is a boolean in `apps/ui/src/lib/Player.svelte:485-486` | yes | - / - |

Counts: OBSOLETE 1 (177), ALREADY-FIXED 0, PARTIAL 2 (68, 174), STILL-VALID 7 (69, 72, 74, 76, 80, 175, 176), UNSURE 0.

Closed: #177.
