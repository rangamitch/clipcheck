# Rules for implementation agents (Haiku)

You are a small, focused worker. Stay inside your assigned files. Keep context small.

1. Read ONLY: this file, the task prompt, and the design-doc sections the task names.
   To read a doc section: `Grep` for the heading with -n, then `Read` with offset/limit (max ~250 lines at a time). Never read a whole design doc.
2. Edit ONLY the files listed as "owned" by your task. Do not touch Cargo.toml of other crates, `lib.rs` of other crates, or docs.
   You may add dependencies to YOUR crate's Cargo.toml only (prefer `workspace = true` deps; if one is missing from the root, tell the orchestrator in your report instead of editing the root).
3. Code style: idiomatic Rust 2021, no `unsafe` unless the task says so, no `unwrap()` outside tests, doc comments on pub items, `#[derive(Debug, Clone, PartialEq)]` where sensible, `serde` derives where the type crosses a boundary.
4. Pure logic first: no Windows API calls unless the task says so.
5. Verify: run `cargo build -p <crate>` and `cargo clippy -p <crate> -- -D warnings` and fix issues.
6. TESTING IS A SEPARATE CHAIN (updated; see .claude/skills/haiku-swarm/SKILL.md): you do NOT write integration tests and you do NOT spawn a tester. Verify only that your code compiles, passes clippy -D warnings (and `npm run build` for UI) and your small inline `#[cfg(test)]` unit tests. The orchestrator then dispatches a DIFFERENT fresh tester agent (public API only, edits tests only) and, if needed, a fixer and re-tester, so no single agent carries both implementation and verification context. End your report with the public API summary and how to run the code so the tester can start cold.
7. Final report (<= 25 lines): files created, public API summary, test results (counts), anything unfinished, and deviations from the design doc. Then stop.
