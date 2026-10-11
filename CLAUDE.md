# ClipCheck: instructions for Claude sessions

For ANY multi-file implementation task, use the project skill `haiku-swarm`
(`.claude/skills/haiku-swarm/SKILL.md`; prompts, lessons, verification in its `references/`).

Non-negotiable rules:
1. You are the orchestrator: plan, record decisions (DESIGN.md D-numbers), dispatch, VERIFY, commit. Do not hand-implement large features.
2. Workers are Haiku agents with heavily guided prompts: goal, OWNED files, read-only ranges, exact contracts, edge cases. Context <= 100k tokens, one narrow task each. Sonnet only for hard/meta work or after Haiku fails twice.
3. Testing is a separate chain: implementer only compiles/clippies; a fresh Haiku TESTER (public API only, edits tests only) tests; a fresh FIXER fixes; a RE-TESTER re-runs, until green.
4. Parallel agents only on disjoint files; pre-create stubs, mod lines, Cargo deps/features; one owner per file.
5. After every wave: cargo fmt --all; cargo clippy --workspace --all-targets -- -D warnings; scripts/test-all.sh; apps/ui `npm run build` + `npx vitest run`; src-tauri checks. Root-cause every failure; never trust an agent's report. Commit with the harness's attribution trailer; keep README/DESIGN.md current.
6. Permanent tests never launch games or touch the user's real apps/recordings. Never kill the user's running ClipCheck (scripts/kill-test-strays.ps1 only kills repo builds).
7. Nothing outside the repo (GitHub repos, releases, comments, signing keys) without asking; no secrets in the repo; downloads need approval (file, source, size).
8. Report concisely: done, verified (with numbers), NOT verified, decisions needed.

Env: Windows; no Python (use node/PowerShell/perl); write .ps1 files, not inline PowerShell in bash; no `sleep` loops in git-bash.
