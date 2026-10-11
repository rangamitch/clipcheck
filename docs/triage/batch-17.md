# Triage batch 17 (issues 139, 45, 47, 50, 52, 56, 59, 62, 65, 67)

Judged at commit b6e6525 (HEAD) using `git show HEAD:<path>` and `git grep ... HEAD`. Line numbers are HEAD line numbers.
Closed issues: none. Verdicts were STILL-VALID or PARTIAL, so no `gh issue close` was run.

| issue | title (short) | verdict | evidence (file:line) | closed? | fix size / priority |
|---|---|---|---|---|---|
| #139 | Redundant version strings, empty features, duplicate .gitignore, CREATE_NO_WINDOW | STILL-VALID | Version "0.1.0" in Cargo.toml:7, apps/ui/src-tauri/Cargo.toml:3, tauri.conf.json:4, apps/ui/package.json:4; empty `features = []` at apps/ui/src-tauri/Cargo.toml:33 and :36; `.gitignore` has `dist/` at lines 3 and 10 and `/target` plus `target/` at lines 1 and 8; `CREATE_NO_WINDOW \| DETACHED_PROCESS` at apps/ui/src-tauri/src/daemon.rs:198. | no | S / P3 |
| #45 | suggestNewChord F13 fallback not checked against used chords | STILL-VALID | apps/ui/src/lib/settings/clips.ts:93 returns F13 without a `used.has` check. F13 was added to the candidate list (clips.ts:25), so the fallback is reached only when all candidates are taken, and that is exactly when F13 is also taken. | no | S / P0 (wrong behaviour, rare trigger) |
| #47 | Gallery refresh collapses paginated list to first page | STILL-VALID | Gallery.svelte:124 `refresh()` calls `fetchPage(null)`; Gallery.svelte:94 `items = cursor === null ? page.items : ...` drops loaded pages. The docstring at Gallery.svelte:120-121 still says the list stays on screen. | no | M / P0 (list resets while user browses) |
| #50 | Live chip tooltip "Session ... in" stray word | STILL-VALID | apps/ui/src/lib/status.ts:383 still ends with `} in`. | no | S / P3 |
| #52 | Three duration/clock formatters with different output | STILL-VALID | apps/ui/src/lib/format.ts:27 `formatDuration(ms)`; apps/ui/src/lib/settings/pure.ts:617 `formatDuration(totalSeconds)`; apps/ui/src/lib/status.ts:114 `formatClock(ms)`. Same name, different units. | no | M / P2 |
| #56 | Two Tauri detection helpers with different checks | STILL-VALID | apps/ui/src/lib/daemon.ts:263 `isTauri()` checks `window`; apps/ui/src/lib/window.ts:21 `isTauriRuntime()` checks `globalThis`. Both still used (daemon.ts:274, 284, 342; exitconfirm.ts:4, 99). | no | S / P2 |
| #59 | Unreachable "Coming next" placeholder and dead settings entries | STILL-VALID | All seven tabs in apps/ui/src/lib/settings/tabs.ts:29-35 set `component`, so the premise that all tabs have one holds. The dead code still exists: the `{:else}<ComingNext>` branch at apps/ui/src/lib/Settings.svelte:144-148 with the import at Settings.svelte:3, the optional `component?` at tabs.ts:25, and `advanced.*` entries at apps/ui/src/lib/settings/pure.ts:129-131 with no Advanced tab. | no | S / P3 |
| #62 | Gallery reimplements debounce and duplicates stats fetch | STILL-VALID | Hand-rolled debounce at Gallery.svelte:136-138 and cleanup at :182, though apps/ui/src/lib/debounce.ts:12 `debounce()` exists. Stats fetch is duplicated at Gallery.svelte:124-130 (keeps old stats on error) and :172-179 (sets `stats = null`). | no | S / P2 |
| #65 | Defensive fallbacks for fields the daemon always sends | PARTIAL | Still present: `mapPage` null-coalescing at apps/ui/src/lib/api.ts:100-105; optional wire fields at apps/ui/src/lib/daemon.ts:103-106 although crates/ipc/src/proto.rs:941, 944, 948 declare them required; status.ts:233-237 fallbacks; `p?.connected === true` at daemon.ts:371. Fixed: the `CODEC_SCALE[codec] ?? 1` fallback cited at pure.ts:305 no longer exists at HEAD (grep finds no CODEC_SCALE in apps/ui/src). | no | M / P2 |
| #67 | Unreachable fallbacks, no-op reducer cases, unused field | STILL-VALID | LivePanel.svelte:37 and :74 `gameName ?? 'Recording'` fallback; status.ts:307-321 no-op cases returning `prev` (`clip_saved`, `audio_levels`, `settings_reloaded`, etc.) that duplicate the default; `STATUS_TOPICS` at daemon.ts:248 omits `levels`, so `audio_levels` is unreachable; unused `key` field at settings/hotkeys.ts:35 and constructed at :142. | no | S / P3 |

## Counts

- ALREADY-FIXED: 0
- OBSOLETE: 0
- STILL-VALID: 9 (#139, #45, #47, #50, #52, #56, #59, #62, #67)
- PARTIAL: 1 (#65)
- UNSURE: 0
