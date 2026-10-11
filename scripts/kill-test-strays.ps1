# Stops recorder/UI processes that were started from THIS repo's build folders.
# It never touches a packaged or installed ClipCheck (for example dist\ClipCheck) that the user
# may be running: the path must start with <repo>\target or <repo>\apps.
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$prefixes = @('target', 'apps') | ForEach-Object { Join-Path $root $_ }
Get-Process clipcheckd, clipcheck-ui -ErrorAction SilentlyContinue |
    Where-Object {
        $p = $_.Path
        $p -and ($prefixes | Where-Object { $p.StartsWith($_ + '\', [StringComparison]::OrdinalIgnoreCase) })
    } |
    Stop-Process -Force -ErrorAction SilentlyContinue
