# Sounds

`clip-saved.wav`: "Tick-Tock Confirm", played when a clip is saved. It is an original, synthesized sound
(no samples, no third-party material), so it carries no third-party licence and is covered by the project
licence. The daemon embeds it with `include_bytes!` in `apps/clipcheckd/src/sound.rs`.

History note: earlier commits contained a Mixkit sound under this same file name; see docs/LICENSING.md
section 5 before making the repository public.
