# Releasing ClipCheck

This is the owner's runbook for the installer, the in-app updater and the release pipeline.
The design is in [design/11-installer-and-updates.md](design/11-installer-and-updates.md).
Scripts live in `scripts/release/`; workflows in `.github/workflows/`.

## 1. One-time setup

### 1a. Decide where releases are published (decision 1 in doc 11)

The updater downloads `latest.json` and the installer without credentials. GitHub only serves
release assets of a **public** repository to anonymous clients, so one of these is required:

- **Option A: make this repository public.** Leave `RELEASE_REPO` unset (it defaults to this
  repository). No `RELEASES_TOKEN` is needed.
- **Option B: keep the source private and publish to a public releases-only repository**, for
  example `OWNER/clipcheck-releases`.
  1. Create the empty public repository.
  2. Create a fine-grained personal access token with *Contents: Read and write* on that repository
     only. Store it as the environment secret `RELEASES_TOKEN` (step 1d).
  3. Set the variable `RELEASE_REPO` to `OWNER/clipcheck-releases` (step 1d).

The app's update endpoint is `https://github.com/<RELEASE_REPO>/releases/latest/download/latest.json`.
Use the same value in the app's built-in endpoint when you make that change (step 1c).

### 1b. Generate the updater signing key (once, ever)

```powershell
cd apps\ui; npm ci; cd ..\..
pwsh -File scripts/release/gen-update-keys.ps1
```

The script asks for a password and writes the private key to `%USERPROFILE%\.clipcheck\updater.key`
(outside the repository; the script refuses paths inside it). It prints the public key.

- Back up the private key and its password in a password manager.
- If the private key is lost, installed copies can no longer receive updates signed by a new key.
- Never commit the private key, and never paste it into an issue, a chat or a log.

### 1c. Put the public key into the app

In `apps/ui/src-tauri/tauri.conf.json` (a separate change, not done by the release scripts):

- `plugins.updater.pubkey` = the public key printed by `gen-update-keys.ps1`.
- `plugins.updater.endpoints` = `["https://github.com/<RELEASE_REPO>/releases/latest/download/latest.json"]`.
- Optional: `bundle.createUpdaterArtifacts: true`. The release workflow already passes this flag on the
  command line, so the setting is only needed for local `tauri build` runs.

### 1d. Create the `release` environment, secrets and variable

Repository **Settings → Environments → New environment → `release`**:

- *Deployment branches and tags*: allow only tags matching `v*` (and `main` if you want).
  This keeps the signing secrets away from other branches.
- *Required reviewers* (recommended): yourself. Each release then waits for one click.

Environment secrets (gh CLI logged in to the repository):

```powershell
Get-Content -Raw "$env:USERPROFILE\.clipcheck\updater.key" | gh secret set TAURI_SIGNING_PRIVATE_KEY --env release --repo OWNER/REPO
gh secret set TAURI_SIGNING_PRIVATE_KEY_PASSWORD --env release --repo OWNER/REPO
```

Environment variable (only with Option B):

```powershell
gh variable set RELEASE_REPO --env release --repo OWNER/REPO --body OWNER/clipcheck-releases
```

Environment secret `RELEASES_TOKEN` (only with Option B): the token from step 1a.

### 1e. Optional: Authenticode signing

Without it the installer is unsigned. Updates are still verified (Ed25519, step 1b), so this is
about the Windows warning only. The workflow signs automatically when both secrets exist:

- `WINDOWS_CERTIFICATE`: the `.pfx` file, base64-encoded (`[Convert]::ToBase64String([IO.File]::ReadAllBytes('cert.pfx'))`).
- `WINDOWS_CERTIFICATE_PASSWORD`: the `.pfx` password.

Caveat: current code-signing certificate rules often require a hardware-protected key, which
cannot be exported as a `.pfx`. In that case this path does not work, and the alternative is
Azure Trusted Signing, which the workflow does not implement yet.

## 2. Cutting a release

1. **Bump the version** to the same `X.Y.Z` in all four places:
   root `Cargo.toml` (`[workspace.package] version`), `apps/ui/src-tauri/Cargo.toml`,
   `apps/ui/src-tauri/tauri.conf.json` and `apps/ui/package.json`. Check with
   `node scripts/release/check-versions.mjs --tag vX.Y.Z`.
2. **Write the release notes** (optional): `docs/release-notes/vX.Y.Z.md`. Without this file the notes
   come from the tag annotation, or else from `git log` since the previous tag.
3. **Merge to `main`** and wait for CI (`ci.yml`) to pass.
4. **Tag the commit on main** and push the tag:
   ```powershell
   git tag -a vX.Y.Z -m "ClipCheck X.Y.Z: short summary"
   git push origin vX.Y.Z
   ```
   The release workflow refuses tags that are not on `main`, and tags that are not `vX.Y.Z`.
   The same workflow can be started by hand (**Actions → release → Run workflow**) with an existing tag.
5. **Approve the run** if you enabled required reviewers. Watch the run in the Actions tab.
6. **Check the draft** in the releases repository (`RELEASE_REPO`). It should contain:
   `ClipCheck_X.Y.Z_x64-setup.exe`, its `.sig`, `latest.json`, `THIRD-PARTY-NOTICES.txt`, and `SHA256SUMS`.
   Compare the hashes in `SHA256SUMS` with the files you download. Open `latest.json` and check `version`
   and `url`.
7. **Install the draft on a clean Windows account** (or a test folder) before publishing: run the
   installer, start the app, and check the version in Settings.
8. **Publish the draft** (Releases → the draft → Edit → Publish release). Publishing is the step that makes
   `releases/latest/download/latest.json` point at the new version, so existing installs start seeing the update.
   Only published, non-pre-release releases count as "latest".

FFmpeg is not bundled, downloaded or used, not even by tests (D58, D62).

## 3. What the updater reads

1. The app (not the daemon) requests `latest.json` at launch and every 12 hours, unless
   `updates.check = never`. The request has no identifiers.
2. If `version` is newer than the installed version, the app shows the update. Users can skip it.
3. On "Update now" the app downloads `platforms.windows-x86_64.url`, checks the Ed25519 `signature`
   against the public key compiled into the app, and runs the installer silently. A bad signature
   aborts the update and keeps the old version.

`latest.json` fields: `version`, `notes`, `pub_date` (RFC 3339 UTC), `platforms`.

## 4. Rollback

- **Mistake in a draft:** delete the draft release. Nothing has been served yet.
- **Bad version already published:** the updater only moves forward, so installed copies of the bad version
  are not downgraded. Publish a fixed version `X.Y.(Z+1)` as soon as possible. Marking the bad release as a
  pre-release makes `latest` point at the previous release again, so new downloads stop getting the bad
  version. Installed copies see no update until the fix is published.
- **Manual rollback for a user:** the previous installer is kept in
  `%LOCALAPPDATA%\ClipCheck\updates\previous\` (doc 11 §6). Run it, or download the older release
  from the Releases page.
- **Make an older release the "latest" again** (affects new downloads and updates, not already-updated copies):
  `gh release edit vOLD --repo OWNER/REPO --latest`.

## 5. SmartScreen expectations

Unsigned installers trigger "Windows protected your PC" (publisher "Unknown"). The user clicks
**More info → Run anyway**. Warnings fade as the installer builds download reputation, which is slow for
a new publisher. An Authenticode signature does not remove the warning on day one; reputation still has to
build. Update integrity does not depend on Authenticode: it relies on the Ed25519 signature checked by the app.

## 6. What CI checks

| Workflow | Checks |
|---|---|
| `ci.yml` (PRs and `main`) | `cargo fmt --check`, `clippy -D warnings`, headless tests, compile of all test targets and of the Tauri shell, frontend build and Vitest, version consistency, script unit tests, `cargo deny` for both lockfiles |
| `release.yml` (tag `v*`) | tag format and "on main", versions match the tag, Tauri NSIS build, updater signature, notices (written before the installer, so it ships them), optional Authenticode, 30 MiB size budget, `latest.json`, `SHA256SUMS`, draft release |

The desktop, GPU, audio and window suites cannot run on hosted runners. Run them on the dev machine with
`scripts/test-all.sh` before tagging.

## 7. Local checks

```powershell
node scripts/release/check-versions.mjs --tag vX.Y.Z
node --test "scripts/release/*.test.mjs"
node scripts/release/size-budget.mjs --file dist\release\ClipCheck_X.Y.Z_x64-setup.exe --max-mb 30
node scripts/release/write-notices.mjs --strict
```

Each script accepts `--help` (PowerShell scripts: `-Help`).

## 8. Known gaps

- The release build uses the `release` profile from the root `Cargo.toml` (`lto = "thin"`, `strip = true`).
  No `dist` profile exists yet, so no PDB symbols zip is produced.
- `apps/ui/src-tauri/tauri.conf.json` carries the production updater `pubkey` (key id CABC178AAFF59A6A). Its private
  half lives only in `%USERPROFILE%\.clipcheck\updater.key` and the `release` environment secrets.
