# Triage batch 08 (area: detect)

Judged against commit b6e6525 (HEAD), reading code with `git show HEAD:<path>`.

| issue | title (short) | verdict | evidence (file:line) | closed? | fix size / priority |
|---|---|---|---|---|---|
| #5 | Placeholder modules gpu/modules/library/known_games are empty stubs | OBSOLETE | The four files are implemented (gpu.rs 335 lines, modules.rs 364, library.rs 795, known_games.rs 66 with known_games.json) and used by apps/clipcheckd/src/discovery.rs:19,466,487 and crates/detect/src/enrich.rs:43-47. | yes | S / P3 |
| #6 | Worked example 4 prose contradicts the gate table | STILL-VALID | docs/design/01-game-detection.md:583 says 3 % GPU with no D3D module is blocked by G8, but the G8 row at :550 and the code at crates/detect/src/score.rs:188-194 accept Geometry; the NOTE is still at score.rs:365-366. | no | S / P2 |
| #7 | Every visible window re-opens its process on every poll | STILL-VALID | crates/detect/src/win.rs:141 calls `process_info(pid)` for every window each poll, and process_info (win.rs:209-227) does OpenProcess, image-name query and GetProcessTimes with no cache. enrich.rs:81,184-190 already caches pid to exe path for the module probe, but the enumeration path does not use it. | no | M / P1 |
| #8 | Per-candidate allocations and linear scans in scoring | STILL-VALID | lists.rs:159-168 normalize_exe per constant entry on every call, launchers and anti-cheat not in a set; score.rs:249 `to_ascii_lowercase` runs unconditionally before the `is_running_game` check; score.rs:129 `evaluate` rebuilds Lists. | no | S / P1 |
| #9 | Needless String clones of Pick per candidate and game start | STILL-VALID | runner.rs:146 calls pick_from inside the candidate loop for every better candidate; runner.rs:172,188,197,215,225 clone `chosen`/`self.bound`; debounce.rs:247-248 clones pick fields on start. | no | S / P1 (small absolute cost) |
| #54 | enter_ending maps Liveness::Alive to ProcessExited | STILL-VALID | debounce.rs:354-358 keeps `Liveness::ProcessExited \| Liveness::Alive` arm; both callers (debounce.rs:300 and :348) run only when `alive` is false, so the arm is unreachable but not enforced by the type. | no | S / P3 |
| #57 | evaluate() rebuilds Lists per call and is public | STILL-VALID | score.rs:128-130 `evaluate` calls `Lists::from_config`; crates/detect/examples/dump_candidates.rs:62 calls it per window; lib.rs:18 re-exports it; about 90 calls in tests/api_behavior.rs. | no | S / P2 |
| #61 | win_tests.rs tests with no assertions or overstated names | STILL-VALID | win_tests.rs:119-129 `snapshots_feed_evaluate_without_panic` only calls evaluate; :97-110 `_second` unused; :131-151 is named "nonnegative" on a u64 and returns early when no pid-0 window exists; :18-25 `enumerate_returns_at_least_one_window` has no `#[ignore]` or env gate. | no | S / P2 |
| #64 | api_behavior.rs repeats the same gate cases | STILL-VALID | api_behavior.rs:127-135 vs :454-466 (builtin blocklist), :148-156 vs :507-520 (system shell), :137-146 vs :439-452 (block_extra). The allow-rule variants test a distinct override rule, so the merge should keep one case per rule, not drop them. | no | S / P3 |
| #66 | dump_candidates example is an undocumented debug tool | PARTIAL | Documentation part is OBSOLETE: the file documents its own usage (examples/dump_candidates.rs:1-7) and is referenced in .claude/skills/haiku-swarm/references/lessons.md:34, verification.md:20 and docs/HANDOVER.md:147. Still present: the filter inconsistency at dump_candidates.rs:39 (the visibility filter applies only when no filter is given) and the per-window evaluate at :62. | no | S / P3 |

## Counts

- OBSOLETE: 1 (#5, closed)
- ALREADY-FIXED: 0
- STILL-VALID: 8 (#6, #7, #8, #9, #54, #57, #61, #64)
- PARTIAL: 1 (#66)
- UNSURE: 0

## Closed

- #5 (OBSOLETE, closed with evidence comment)

## Notes

- #7: enrich.rs already has a pid-keyed cache (exe path, pruned by live pids at enrich.rs:138), so the fix can reuse that pattern. The enumeration path in win.rs is the part not covered.
- #64: the pairs are not pure duplicates. The allow-rule tests check an override rule, so merge them rather than delete them.
