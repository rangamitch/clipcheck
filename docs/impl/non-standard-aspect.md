# Non-16:9 recording (issue: 3440x1440 ultrawide letterboxed; Ultra save error)

Decisions: DESIGN.md D54 (canvas follows the source aspect) and D55 (CFG-V006 gated to
custom + VBR). This file is the contract the workers implement against.

## Problem (confirmed by reading the code)

1. `crates/engine/src/pipeline.rs` `fit_to_canvas` / `CanvasFitter` letterboxes every frame
   into a fixed canvas. The canvas comes from `apps/clipcheckd/src/settings_map.rs`
   `canvas_for_height`, which assumes 16:9 because the capture size is not known there.
   So a 3440x1440 window becomes 2560x1072 with black bars.
2. Ultra sets `bitrate_kbps = 30000` (`apps/ui/src/lib/settings/pure.ts` PRESET_ROWS),
   but `max_bitrate_kbps` stays at 15000. `crates/core/src/config/validate.rs` CFG-V006
   requires `max >= bitrate` for every config, so save is rejected. The rejected field
   (`video.max_bitrate_kbps`) is not rendered for presets, so nothing is highlighted.
   The schema doc (`schema.rs` ~line 1985) says the max applies to "custom + vbr only".

## Contract A: pure canvas math (DONE by orchestrator)

`crates/engine/src/canvas.rs` (module registered in `crates/engine/src/lib.rs`):

- `pub const MAX_CANVAS_WIDTH: u32 = 3840;`
- `pub fn canvas_for_source(src_w: u32, src_h: u32, height: u32) -> (u32, u32)`
  - zero input -> 16:9 for `height`; width = round(height*src_w/src_h), halves up, made even;
    width capped at 3840 with height scaled down to keep the aspect; both even, >= 2.
  - 16:9 gives the old sizes (1080 -> 1920x1080, 480 -> 854x480).

## Contract B: recorder config and attach (worker C, engine)

`crates/engine/src/recorder.rs`:
- `RecorderConfig` gains `pub canvas_height: u32`. `0` means Native: the height is the
  source height capped at 2160. A non-zero value is the preset or custom height.
  The existing `canvas: (u32, u32)` stays as the 16:9 fallback used when the probe fails.
- At `Recorder::attach(hwnd, label)`, before `start_capture` (~line 621), compute the run
  canvas: probe the window size from `hwnd` (new helper, Windows-only, in the engine; use
  `GetWindowRect`), then `canvas_for_source(w, h, height)`. Probe fails or returns zero or
  under 64 px: use `cfg.canvas`. Pass the run canvas to `helpers::start_capture`
  (`helpers.rs` ~77-95) instead of `cfg.canvas`.
- Fixture structs in `crates/engine/tests/` (`recorder_tests.rs`, `issue_193_197_tests.rs`,
  `tests/common/mod.rs`) need the new field; they are owned by worker C.

## Contract C: settings map (worker B, clipcheckd)

`apps/clipcheckd/src/settings_map.rs`:
- `build_recorder_config` sets `canvas_height` from the preset/custom resolution, using
  `0` for `Resolution::Native`. Keep `canvas` = `canvas_for_source(16, 9, height)` (fallback).
- Remove the 16:9-only `canvas_for_height` (or make it call `canvas_for_source(16,9,h)`).
- Update `apps/clipcheckd/src/settings_map/tests.rs` canvas expectations (1920x1080 etc.
  stay the same for 16:9 fallback; add a Native expectation of `0`).

## Contract D: validation (worker A, core)

`crates/core/src/config/validate.rs` ~line 538:
- CFG-V006 only when `preset == Preset::Custom && rate_control == RateControl::Vbr`.
- `crates/core/tests/validate_tests.rs` ~line 278-283 currently pins the ungated behaviour;
  update it to custom + VBR, and add: Ultra with default max validates.

## Contract E: visible save error (worker E, UI)

`apps/ui/src/lib/settings/store.ts` ~line 598-607: when a daemon rejection names a field
that is not rendered, the footer notice must include that error's message (e.g.
"video.max_bitrate_kbps (15000) must be at least video.bitrate_kbps (30000)"), not only
"Not saved. Fix the highlighted fields." Update `store.test.ts` / `store.edge.test.ts`.

## Ownership (disjoint, parallel wave 1)

| Worker | Owns |
|---|---|
| A (core validation) | crates/core/src/config/validate.rs, crates/core/tests/validate_tests.rs |
| B (settings map) | apps/clipcheckd/src/settings_map.rs, apps/clipcheckd/src/settings_map/tests.rs |
| C (engine attach) | crates/engine/src/recorder.rs, crates/engine/src/helpers.rs, new window-size helper file crates/engine/src/window_size.rs, crates/engine/tests/{recorder_tests,issue_193_197_tests}.rs, crates/engine/tests/common/mod.rs |
| E (UI notice) | apps/ui/src/lib/settings/store.ts, apps/ui/src/lib/settings/store.test.ts, apps/ui/src/lib/settings/store.edge.test.ts |

Module registration (`crates/engine/src/lib.rs` `pub mod window_size;`) is pre-created by
the orchestrator before dispatch.

## Risks and open items

- `GetWindowRect` includes the invisible resize border and needs a DPI-aware process. The
  aspect difference is a few pixels; the fitter absorbs it. Check DPI awareness in
  `apps/clipcheckd` manifest (open).
- Session parts concat by stream copy (`sessionfile.rs`): one encoder run per attach, so
  one canvas per part. Safe.
- Canvas width grows with ultrawide sources, so encoding cost rises by about 33 percent
  for 3440x1440 vs 2560x1440 at the same bitrate. Bitrate is not changed (owner decision).
- Native now records the source size (capped 2160 high, 3840 wide), not always 3840x2160.
