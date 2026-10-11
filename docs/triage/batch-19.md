# Triage batch 19 (ui frontend, apps/ui/src), judged at HEAD b6e6525

Read-only triage. Code read with `git show HEAD:<path>` and `git grep ... HEAD`. No issue was closed: none was ALREADY-FIXED or OBSOLETE as a whole.

| issue | title | verdict | evidence | closed? | fix size / priority |
|---|---|---|---|---|---|
| #178 | Name length counts UTF-16 units, daemon counts chars | PARTIAL | Track half is OBSOLETE: track names are derived in Rust (`crates/core/src/config/schema.rs:459-461`), and app names already count code points (`apps/ui/src/lib/settings/audio.ts:100` `Array.from`). Clip half is STILL-VALID: `apps/ui/src/lib/settings/tabs/ReplayTab.svelte:239` `clip.name.length > CLIP_NAME_MAX_CHARS`, while the daemon uses `chars().count()` at `crates/core/src/config/validate.rs:780`. | no | S / P2 |
| #180 | Schema bounds copied into many places; buffer slider reuses clip max | PARTIAL | Bitrate now uses shared constants (`apps/ui/src/lib/settings/pure.ts:379` `BITRATE_MIN_KBPS`, used in `VideoTab.svelte:233-257`). Still STILL-VALID: buffer slider `min="30" max={CLIP_MAX_SECONDS}` at `ReplayTab.svelte:125-126`, clip field `min={1}` at `ReplayTab.svelte:259` vs `CLIP_MIN_SECONDS = 5` (`clips.ts:11`), and hand-typed `max={100000}` at `StorageTab.svelte:87`, `max={4096}` at `ReplayTab.svelte:166`. No `settings/bounds.ts` exists at HEAD. | no | M / P3 |
| #182 | Every settings edit fans out to all rows and re-compares whole config | STILL-VALID | Each row subscribes to the whole store: `apps/ui/src/lib/settings/fields/FieldRow.svelte:32-38` (`$settings.saved`/`draft` deepEqual per row). Whole-config compare on every edit: `apps/ui/src/lib/settings/store.ts:315` `deepEqual(s.saved, draft)`, reached from `setPath` (`store.ts:301-304`). Per-keystroke commit: `fields/TextField.svelte:29` `oninput` calls `onchange`. | no | M / P1 |
| #184 | Live status polls every 5 s while hidden; actions force refresh | STILL-VALID | Poll not paused on hidden window: `apps/ui/src/lib/status.ts:508-510` `setInterval(... refreshStatus(), POLL_MS)` with `POLL_MS = 5000` (`status.ts:499`); only the `now` clock pauses (`status.ts:142-146`). `applySnapshot` always returns a new object (`status.ts:213-215` `return { ...prev, ...}`). Post-action refresh remains at `apps/ui/src/lib/LivePanel.svelte:50` `void refreshStatus()` in `finally`. | no | M / P1 |
| #186 | NumberField mirrors prop with $effect; typed text overwritten | STILL-VALID | `apps/ui/src/lib/settings/fields/NumberField.svelte:36-41` `$effect(() => { text = String(value); })`; `commit()` compares against prop, `NumberField.svelte:44-52` (`if (n !== value) onchange(n)`). | no | S / P0 |
| #187 | Settings tabs repeat heading/message/button CSS; duplicate label and chord lists | PARTIAL | Resolution labels fixed: one `RESOLUTION_CHOICES` in `pure.ts:353`, derived at `VideoTab.svelte:61`. Still STILL-VALID: duplicated `h2`/`h3`/`.message`/`button:hover` rules in `tabs/AudioTab.svelte:483-516`, `ReplayTab.svelte:326-363,483`, `HotkeysTab.svelte:141-146`, `GeneralTab.svelte:89`, `StorageTab.svelte:129`, `VideoTab.svelte:406`; no `settings.css` at HEAD. Session chord list still rebuilt in `ReplayTab.svelte:46-52` from the fields that `HotkeysTab.svelte:31-41` already reads. | no | M / P3 |
| #188 | Hotkey reserved table and defaults hand-copied from Rust, no parity test | STILL-VALID | `apps/ui/src/lib/settings/hotkeys.ts:198` `RESERVED_CHORDS`, `clips.ts:25` `NEW_CLIP_CHORDS`, help text with typed defaults at `tabs/HotkeysTab.svelte:24-27`. No UI parity test exists (grep for "parity" in apps/ui/src is empty) and the daemon does not send defaults. The `DEFAULT_TRACKS` sub-claim is OBSOLETE: no such symbol exists at HEAD. | no | M / P2 |
| #189 | Settings unit tests repeat diffPatch and setPath checks | STILL-VALID | Overlap confirmed: `apps/ui/src/lib/settings/pure.test.ts:46` `describe('diffPatch')`, `pure.test.ts:198` `describe('getPath and setPath')`, and `tabs.diff.test.ts:54` `describe('diffPatch on a fresh copy')`, `tabs.diff.test.ts:218` "diffPatch does not change the saved copy". `pure.test.ts:383-387` still pins `appliesAfterGameEnds` (not checked against the daemon apply-class table; that sub-claim is UNSURE). | no | S / P3 |

## Counts
- ALREADY-FIXED: 0
- OBSOLETE: 0 (sub-claims only: #178 track names, #188 DEFAULT_TRACKS)
- STILL-VALID: 5 (#182, #184, #186, #188, #189)
- PARTIAL: 3 (#178, #180, #187)
- UNSURE: 0 (one sub-claim in #189 unverified)

## Closed
None. No issue met the strict ALREADY-FIXED or OBSOLETE bar.

## Notes
- #178: the emoji/UTF-16 mismatch still affects clip names (`ReplayTab.svelte:239`); the track-name claim is stale.
- #186 is the highest-impact item here (typed text can be lost, and commit can skip a clamped value).
