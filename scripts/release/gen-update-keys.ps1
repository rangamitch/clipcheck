<#
.SYNOPSIS
    One-time setup: generates the Tauri updater signing key pair for ClipCheck.

.DESCRIPTION
    Wraps `npx tauri signer generate -w <path>` (run from apps/ui, so the locally installed
    @tauri-apps/cli is used; run `npm ci` there first). The Tauri CLI asks for a password
    interactively. The private key is written to -KeyPath, which must be OUTSIDE the repository.
    The public key (<KeyPath>.pub) is printed so you can paste it into
    tauri.conf.json -> plugins.updater.pubkey. The private key is never printed.

    Keep the private key and its password in a password manager. If the key is lost, installed
    apps cannot receive updates signed with a new key, so back it up before going on.

.PARAMETER KeyPath
    Where to write the private key. Default: %USERPROFILE%\.clipcheck\updater.key

.PARAMETER Help
    Show this help and exit 0.

.EXAMPLE
    pwsh -File scripts/release/gen-update-keys.ps1
#>
[CmdletBinding()]
param(
    [string]$KeyPath = (Join-Path $env:USERPROFILE '.clipcheck\updater.key'),
    [switch]$Help
)

$ErrorActionPreference = 'Stop'

function Invoke-Main {
    $repoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..'))
    $uiDir = Join-Path $repoRoot 'apps\ui'

    if ($Help) {
        Get-Help $PSCommandPath | Out-Host
        return 0
    }

    $keyFull = [IO.Path]::GetFullPath($KeyPath)
    if ($keyFull.StartsWith($repoRoot.TrimEnd('\') + '\', [StringComparison]::OrdinalIgnoreCase)) {
        throw "refusing to write the private key inside the repository ($keyFull). Choose a path outside $repoRoot."
    }
    if (Test-Path -LiteralPath $keyFull) {
        throw "a key already exists at $keyFull. Not overwriting it. Delete it yourself if you really want a new key (this breaks updates for installed copies)."
    }
    if (-not (Test-Path -LiteralPath (Join-Path $uiDir 'node_modules\.bin'))) {
        throw 'apps/ui/node_modules is missing. Run `npm ci` in apps/ui first.'
    }
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $keyFull) | Out-Null

    Push-Location $uiDir
    try {
        & npx tauri signer generate -w $keyFull | Out-Host
        if ($LASTEXITCODE -ne 0) { throw "tauri signer generate failed (exit $LASTEXITCODE)." }
    }
    finally {
        Pop-Location
    }

    $pubPath = "$keyFull.pub"
    if (-not (Test-Path -LiteralPath $pubPath)) { throw "expected public key at $pubPath but it was not written." }
    $pub = (Get-Content -LiteralPath $pubPath -Raw).Trim()

    Write-Host ''
    Write-Host '=== Step 1: public key (safe to share) ==='
    Write-Host 'Paste this value into apps/ui/src-tauri/tauri.conf.json at plugins.updater.pubkey:'
    Write-Host $pub
    Write-Host ''
    Write-Host '=== Step 2: private key (keep secret, never commit) ==='
    Write-Host "Private key file: $keyFull"
    Write-Host 'Back it up in a password manager. Do not copy it into the repository or into chat.'
    Write-Host ''
    Write-Host '=== Step 3: GitHub secrets for the protected "release" environment ==='
    Write-Host 'Create the environment first (repo Settings > Environments > New environment: release).'
    Write-Host 'Then, from a shell in the repository, with the gh CLI logged in:'
    Write-Host "  Get-Content -Raw '$keyFull' | gh secret set TAURI_SIGNING_PRIVATE_KEY --env release --repo OWNER/REPO"
    Write-Host '  gh secret set TAURI_SIGNING_PRIVATE_KEY_PASSWORD --env release --repo OWNER/REPO'
    Write-Host '(the second command prompts for the password you chose; the value is not echoed)'
    Write-Host ''
    Write-Host 'Optional Authenticode secrets (see docs/release.md): WINDOWS_CERTIFICATE, WINDOWS_CERTIFICATE_PASSWORD.'
    Write-Host 'Do NOT commit the private key or paste it into any file tracked by git.'
    return 0
}

try {
    $code = Invoke-Main
    exit $code
}
catch {
    [Console]::Error.WriteLine("error: $($_.Exception.Message)")
    exit 1
}
