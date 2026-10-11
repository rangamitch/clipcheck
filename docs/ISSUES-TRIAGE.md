# Issue triage and work queue

Source: 199 open GitHub issues (#1-#199, raised 2026-10-09, before commit c6c786a). Many may already be fixed or
changed by c6c786a, so each wave first checks the issue against the CURRENT code (valid / fixed / partial), then fixes.
Process: 5 issues per wave, one Haiku per issue, disjoint crates, then the tester relay (haiku-swarm), orchestrator gate,
commit, then the issue is commented on (and closed when fixed and verified, or already fixed). Status lives here and on GitHub.

Priority rules: P0 = data loss, wrong behaviour users hit, safety. P1 = performance on hot paths. P2 = latent bugs,
robustness, test quality that hides bugs. P3 = cleanup, duplication, docs. Do-not-fix: pure style unless trivial.

## P0 correctness / data loss / safety (do first)
195 failed finalize deletes staged segments · 196 shutdown recorder errors dropped · 197 crashed-session run dirs never removed ·
172 crash salvage (`recover`) never called · 183 + 77 exit with daemon link down · 181 Shutdown has no mode / refuses not
while recording · 10 same-second clip file collision · 44 event seq out of order · 51 timed-out IPC call runs twice ·
48 patch overwrites hand-broken config · 105 re-register UNIQUE failure · 160 part N+1 published before N finished ·
170 + 171 segment tracker stops / unbounded retry · 103 odd audio sample count swaps channels · 16 zero-filled frame on bad pitch ·
38 + 40 hotkey apply/setup false reports · 173 hotkeys never retried after stop · 161 rename/delete/register not implemented
(needed by player, backlog D) · 85 + 89 + 82 + 94 tauri db/pipe fallbacks · 146 + 21 + 23 session reducer replies/shutdown ·
75 + 81 + 88 audio unsafety · 95 + 97 ffmpeg discovery (installer) · 71 KeyChord panic · 118 config size limit ·
194 probe cache keying · 193 + 84 + 109 silent audio failures · 55 hardcoded status values · 53 purge_all stops at first error.

## P1 performance (hot paths first)
165 recorder stop/split blocks service thread · 116 logger unbuffered · 108 EventBus clones · 190 games edit restarts detector ·
182 + 184 + 125 + 72 + 74 + 76 UI churn · 121 + 36 + 37 IPC copies/syscalls · 131 library stats · 102 + 27 + 115 ffprobe spawns ·
7 + 8 detect poll cost · 17 + 129 + 162 canvas alloc / fit_to_canvas · 29 + 31 + 33 + 35 capture copies · 19 + 136 + 137 audio allocs ·
22 + 24 + 41 + 39 + 147 engine/session/library · 80 gallery virtualization · 117 release profile · 111 + 133 polling waits · 140 + 143 encode waits.

## P2 robustness / test quality
152-156 + 158-159 + 163 + 166-168 + 130 + 127 + 122 + 120 + 107 + 132 + 73 + 61 + 42 + 25 tests that pass silently / fixed sleeps /
copy-pasted harnesses · 13 + 34 + 60 + 65 + 98 silent fallbacks · 3 + 4 + 2 + 54 detect edge cases · 28 + 30 + 149 + 150 library ·
79 + 87 + 92 + 99 + 134 core key/override semantics · 106 + 100 encode · 175-178 + 186 UI correctness.

## P3 cleanup / docs
112 113 119 123 124 126 128 135 138 139 141 142 144 145 148 157 169 179(blocks #94) 180 185 187 188 189 191 192 198 199, and the
remaining small cleanup items. #1 (installer epic) is tracked by HANDOVER 6B/6C. #199 (LICENSE file) needs an owner decision.

## Triage result (commit b6e6525, 19 Haiku triage batches, per-issue evidence in docs/triage/batch-NN.md)
188 issues checked: 175 STILL-VALID, 10 PARTIAL, 3 closed on GitHub with evidence (#5 placeholder detect modules now implemented, #40 hotkey setup error code, #177 unmute gain button removed). Priorities from the triagers: P0=24, P1=39, P2=75, P3=48.

## Waves (one Haiku implementer per crate group, disjoint files, then a fresh tester per fix, then the orchestrator gate and a commit)
| Wave | Issues | Crate / owner | State |
|---|---|---|---|
| 1 | 195, 105, 103, 109, 150, 196, 183, 77 | engine, encode, library, clipcheckd, src-tauri | fixed; testers running; #181 stays open (partial: graceful shutdown still acks while recording; updater path is guarded) |
| 2 (P0) | 75, 81, 84 | audio | planned |
| 2 (P0) | 16 | capture | planned |
| 2 (P0) | 2, 4 | detect | planned |
| 2 (P0) | 79 | core | planned |
| 2 (P0) | 45, 186 | ui | planned |
| 3 (P0) | 44, 51 | clipcheckd server/eventbus | planned |
| 3 (P0) | 48, 173, 55, 160 | clipcheckd service/settings (serial, same files) | planned |
| 3 (P0) | 10, 170 / 193, 197 | engine (two agents, disjoint files) | planned |
| 3 (P0) | 194 encode, 38 hotkeys, 149 library, 21 session, 47 ui | one agent each | planned |
| 4 (P1 perf) | 29+31+33 (capture frame path, together), 35, 115, 102, 108, 111, 116, 165, 190, 129, 162, 17, 136, 137, 142, 7, 8, 9, 36, 37, 121, 125, 131, 72, 74, 76, 80, 182, 184 | by crate | planned |
| 5 (P2/P3) | the rest, plus the test-quality issues | by crate | planned |
Decisions pending for the owner: #199 license choice; #198 whether to implement the --worker process or amend DESIGN.md D3; #172/#197 crash salvage scope (wire recover or delete); #181 graceful-vs-force Shutdown design.
