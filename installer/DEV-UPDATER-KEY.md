# Development updater key

The public key in `apps/ui/src-tauri/tauri.conf.json` (`plugins.updater.pubkey`) is a
**development key**. Its private half was generated on the developer machine, lives only
in a local scratch folder, and is never committed. It exists so the in-app updater can
be built and tested end to end against a local fake manifest.

Consequences:

- Updates signed with the development key must not be published. Anyone holding the
  private half could sign an update that installed clients accept.
- Before the first public release, the owner must run `scripts/release/gen-update-keys.ps1`,
  store the private key and its password as the `TAURI_SIGNING_PRIVATE_KEY` and
  `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` secrets in the `release` environment, and replace
  `plugins.updater.pubkey` with the new public key.
- `bundle.createUpdaterArtifacts` is not set in the repository. The release workflow
  passes `--config '{"bundle":{"createUpdaterArtifacts":true}}'` to `tauri build`, so
  local builds without `TAURI_SIGNING_PRIVATE_KEY` still succeed.

Sign a test installer with a key file (for local testing only):

    npx tauri signer sign -f <path-to-key> <installer.exe>

This writes `<installer.exe>.sig`. Its content goes into the `signature` field of the
`latest.json` platform entry.
