# Licensing position

> This is an engineering analysis written while preparing ClipCheck for distribution. It is **not legal
> advice**. The items under "Owner must confirm" need the owner's decision, and legal review if in doubt.

Decision record: DESIGN.md **D61** (supersedes the GPL part of D45). Related: D49 (release pipeline), D58 (FFmpeg removed).

## 1. ClipCheck's own license

| Item | State |
|---|---|
| License | **Apache-2.0** (SPDX `Apache-2.0`) |
| `LICENSE` (repo root) | Verbatim Apache License 2.0 text (apache.org `LICENSE-2.0.txt`, sha256 `cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30`), appendix unmodified. Committed. |
| `NOTICE` (repo root) | "ClipCheck" / "Copyright 2026 ClipCheck contributors" (Apache-2.0 section 4(d)). Committed. |
| Rust metadata | `[workspace.package] license = "Apache-2.0"` in `Cargo.toml`; every crate uses `license.workspace = true`; `apps/ui/src-tauri/Cargo.toml` declares `"Apache-2.0"` directly. |
| npm metadata | `apps/ui/package.json` `"license": "Apache-2.0"` (package is `private`, never published). |
| Installer | NSIS shows `LICENSE` as the license page (`bundle.licenseFile`) and installs it as `licenses\LICENSE.txt`. |
| Copyright line | `tauri.conf.json` `bundle.copyright` = "Copyright (C) 2026 ClipCheck contributors. Licensed under Apache-2.0." |
| README | "License" section. |
| Per-file headers | Not required by Apache-2.0 and not used. |
| Contributions | Inbound = outbound under Apache-2.0 section 5; no CLA. See `CONTRIBUTING.md`. Security reports: `SECURITY.md`. |

Relicensing note: until D61 the project was GPL-3.0-or-later (D45). The owner is the sole copyright holder,
so the change to Apache-2.0 is the owner's decision. Copies of earlier commits that someone already received
under GPL-3.0-or-later stay available to them under that license; from D61 on the project is Apache-2.0.

Why Apache-2.0: ClipCheck is a permissively licensed application that links only permissive dependencies
(section 3), ships no copyleft binaries (section 4), and wants a license that is simple for users and
contributors. The project rule is that no GPL/LGPL/AGPL code or binaries are allowed anywhere (source,
linked, bundled, or downloaded at runtime).

## 2. What the installer ships (`licenses\` folder)

| File | Source | Content |
|---|---|---|
| `LICENSE.txt` | repo `LICENSE` | ClipCheck's Apache-2.0 license text (also the NSIS license page) |
| `NOTICE.txt` | repo `NOTICE` | ClipCheck attribution notice |
| `THIRD-PARTY-NOTICES.txt` | `scripts/release/write-notices.mjs` | every Rust crate built for the Windows target (both workspaces) and every npm package shipped in the UI bundle, with license id and repository, followed by their license files (deduplicated); generated in CI **before** the installer is built (D49) |

No FFmpeg files are shipped (D58).

WebView2 is not shipped either: the installer downloads Microsoft's bootstrapper. The NSIS runtime is
zlib-licensed. The app icons and logo are the project's own work.

## 3. Dependency licenses (checked offline from `Cargo.lock` and `package-lock.json`)

Rust (Windows target, both workspaces): MIT, Apache-2.0, MIT/Apache dual, BSD-2/3-Clause, ISC, Zlib,
Unicode-3.0, Unlicense-or-MIT, 0BSD/MIT-0 options, MPL-2.0 (`cssparser`, `selectors`, `dtoa-short`,
`option-ext`; file-level copyleft, used unmodified, so compatible with an Apache-2.0 application as long as
their files are not modified), `ring` (`Apache-2.0 AND ISC`) and `webpki-roots` / `webpki-root-certs`
(`CDLA-Permissive-2.0`, a permissive data license). All are compatible with distributing the whole under
Apache-2.0.

npm: the only production dependency is `@tauri-apps/api` (Apache-2.0 OR MIT). `svelte` (MIT) is a dev
dependency whose runtime is compiled into the UI bundle, so the notices list it too
(`BUNDLED_DEV_PACKAGES` in `write-notices.mjs`).

`deny.toml` (`cargo deny check`, run by `ci.yml`) allows only: MIT, Apache-2.0, Apache-2.0 WITH LLVM-exception,
BSD-2-Clause, BSD-3-Clause, ISC, Unicode-3.0, Unicode-DFS-2016, Zlib, MPL-2.0, CC0-1.0, BSL-1.0 and
CDLA-Permissive-2.0. The former copyleft entries were removed for D61. OpenSSL stays disallowed. cargo-deny is
not installed on the dev machine, so the policy was checked by reading `cargo metadata --offline`, not by
running cargo-deny; CI runs it.

## 4. FFmpeg

FFmpeg is not bundled, not downloaded, not linked and not run (D58), and since D62 not even the tests use
it: test media comes from the dev-only `clipcheck-testkit` crate (Media Foundation H.264 + AAC written by
ClipCheck's own MP4 muxer) and outputs are checked with ClipCheck's own MP4 reader and the Media Foundation
decoder. No source offer or FFmpeg license file is needed. The installer still deletes the legacy
`%LOCALAPPDATA%\ClipCheck\ffmpeg` folder that versions before D58 could download.

## 5. The clip-saved sound (`assets/sounds/clip-saved.wav`)

Replaced: the file is now "Tick-Tock Confirm", an original synthesized sound (no samples, no third-party
material), so it has no third-party license and is covered by the project license. The earlier Mixkit "Gaming
lock" file (free license: use inside projects allowed, redistribution on its own not) was removed from the
working tree, but it remains in git HISTORY under the same file name. Before making the repository public the
owner must decide whether to rewrite history to drop it (destructive, needs coordination) or accept the
history copy.

## 6. Owner must confirm

1. The old Mixkit sound in git history (section 5): rewrite history before going public, or accept it.
2. Author name and email in git history: check them before making the repository public.
3. Whether to publish the existing history, or squash it into a fresh repository.
4. `CDLA-Permissive-2.0` (`webpki-roots`) in the allow-list.
5. Seven crates publish no license file, so `THIRD-PARTY-NOTICES.txt` lists only their license id:
   `alloc-stdlib`, `defmt-parser`, `selectors` (MPL-2.0), `tauri-plugin`, `webview2-com`,
   `webview2-com-macros`, `webview2-com-sys`. Decide whether to add the standard license texts for them by
   hand. Every other crate's own license files are reproduced (deduplicated) in the "License texts" section,
   which `write-notices.mjs` generates for the Windows target only.
