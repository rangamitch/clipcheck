# Contributing to ClipCheck

Thanks for helping. Bug reports and suggestions are welcome in GitHub issues or on the
[ClipCheck Discord](https://discord.gg/jy2NG2EVNw). Security problems: see [SECURITY.md](SECURITY.md).

## License of contributions

ClipCheck is licensed under the [Apache License 2.0](LICENSE). Any contribution you submit is
licensed under Apache-2.0 as well, per section 5 of the license (inbound = outbound). There is no
CLA. Only submit work you have the right to license this way.

## License rules (absolute)

- No GPL, LGPL or AGPL code or binaries, in any form: not copied, not linked, not bundled, not
  downloaded at runtime. This includes FFmpeg/libav, x264, x265 and LAME.
- New dependencies must use a permissive license that `deny.toml` allows (MIT, Apache-2.0, BSD,
  ISC, Zlib, Unicode, ...). MPL-2.0 crates are accepted only unmodified.
- Assets (sounds, images, fonts) must be your own work or carry a license that allows
  redistribution under these terms.

## Tests

- Run `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `bash scripts/test-all.sh`, and in `apps/ui` `npm run build` and `npx vitest run`.
- Tests must never launch games or touch a user's real apps, settings or recordings. Use temp
  directories, unique pipe names and synthetic media.
- Tests must not kill a ClipCheck the user is running (`scripts/kill-test-strays.ps1` only stops
  processes built from the repo).

See [docs/LICENSING.md](docs/LICENSING.md) for the full licensing position.
