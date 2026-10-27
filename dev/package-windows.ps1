# Builds fbterm for Windows (x86-64) and packages it as a per-machine .msi with cargo-wix
# from crates/fbterm/wix/main.wxs.
# Runs on Windows only. Needs cargo-wix (the version pinned in .github/workflows/release.yml)
# and the WiX Toolset v3 (the WIX environment variable its installer sets, or candle.exe on
# PATH).
# Usage: pwsh dev/package-windows.ps1 <version>     e.g. pwsh dev/package-windows.ps1 0.1.0
# Output: target/dist/fbterm-<version>-windows-x86_64.msi (+ .sha256)
param(
    [Parameter(Mandatory = $true)][string]$Version
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# The installer's fixed upgrade code (crates/fbterm/wix/main.wxs); checked in the built .msi.
$UpgradeCode = '{D2F13F2B-68C8-4ED4-9953-D6E612A1FA8D}'

if (-not $IsWindows) { throw 'package-windows.ps1 runs on Windows only' }
if ($Version -notmatch '^(\d+\.\d+\.\d+)([-+][0-9A-Za-z.-]+)?$') {
    throw "invalid version '$Version' (expected X.Y.Z or X.Y.Z-pre)"
}
# MSI versions are numeric: a pre-release installs as its X.Y.Z, and the release of the same
# X.Y.Z replaces it (AllowSameVersionUpgrades in main.wxs).
$msiVersion = $Matches[1]
if ($env:PROCESSOR_ARCHITECTURE -ne 'AMD64') {
    throw "this machine is $env:PROCESSOR_ARCHITECTURE, not x86-64 (no cross-compiling)"
}
if (-not (Get-Command cargo-wix -ErrorAction SilentlyContinue)) { throw 'package-windows.ps1 needs cargo-wix' }

# Runs a native command and stops on a non-zero exit code.
function Invoke-Native {
    param([string]$File, [string[]]$Arguments)
    & $File @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$File $($Arguments -join ' ') failed with exit code $LASTEXITCODE" }
}

# `fbterm --version`, read from a file: release builds are GUI programs with no console, so
# their output only reaches a redirect. A hard timeout in case the flag were ever ignored and
# a window opened instead.
function Get-FbtermVersion {
    param([string]$Exe)
    $out = New-TemporaryFile
    try {
        $p = Start-Process -FilePath $Exe -ArgumentList '--version' -NoNewWindow -PassThru `
            -RedirectStandardOutput $out.FullName
        $null = $p.Handle # keeps the exit code readable after the process ends
        if (-not $p.WaitForExit(20000)) {
            $p.Kill()
            throw "$Exe --version did not exit"
        }
        if ($p.ExitCode -ne 0) { throw "$Exe --version exited with $($p.ExitCode)" }
        return "$(Get-Content -Raw $out.FullName)".Trim()
    } finally {
        Remove-Item -Force $out.FullName
    }
}

$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Set-Location $root
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root 'target' }
$dist = Join-Path $root 'target/dist'
$name = "fbterm-$Version-windows-x86_64"
$msi = Join-Path $dist "$name.msi"
$exe = Join-Path $target 'release/fbterm.exe'

Invoke-Native cargo @('build', '--locked', '--release', '-p', 'fbterm')

# The binary: version line, and the version information build.rs embeds with the icon.
$line = Get-FbtermVersion $exe
if ($line -cne "fbterm $Version") { throw "fbterm --version printed '$line', expected 'fbterm $Version'" }
$info = (Get-Item $exe).VersionInfo
if ($info.ProductName -cne 'featherbit terminal' -or $info.ProductVersion -cne $Version) {
    throw "fbterm.exe has no or the wrong version information: '$($info.ProductName)' '$($info.ProductVersion)'"
}

New-Item -ItemType Directory -Force $dist | Out-Null
Remove-Item -Force -ErrorAction SilentlyContinue $msi, "$msi.sha256"

# The .msi holds the binary exactly as cargo built it above.
Invoke-Native cargo @('wix', '--package', 'fbterm', '--no-build', '--nocapture',
    '--install-version', $msiVersion, '--output', $msi)
if (-not (Test-Path $msi)) { throw "cargo wix did not write $msi" }

# Read the built package's Property table back: name, version, per-machine, upgrade code.
function Get-MsiProperty {
    param([string]$Path, [string]$Property)
    $installer = New-Object -ComObject WindowsInstaller.Installer
    $db = $installer.GetType().InvokeMember('OpenDatabase', 'InvokeMethod', $null, $installer, @($Path, 0))
    $view = $db.GetType().InvokeMember('OpenView', 'InvokeMethod', $null, $db,
        @("SELECT ``Value`` FROM ``Property`` WHERE ``Property`` = '$Property'"))
    $view.GetType().InvokeMember('Execute', 'InvokeMethod', $null, $view, $null) | Out-Null
    $record = $view.GetType().InvokeMember('Fetch', 'InvokeMethod', $null, $view, $null)
    $value = if ($null -eq $record) { $null } else {
        $record.GetType().InvokeMember('StringData', 'GetProperty', $null, $record, 1)
    }
    $view.GetType().InvokeMember('Close', 'InvokeMethod', $null, $view, $null) | Out-Null
    [System.Runtime.InteropServices.Marshal]::FinalReleaseComObject($view) | Out-Null
    [System.Runtime.InteropServices.Marshal]::FinalReleaseComObject($db) | Out-Null
    [System.Runtime.InteropServices.Marshal]::FinalReleaseComObject($installer) | Out-Null
    return $value
}
$want = @{
    ProductName    = 'featherbit terminal'
    ProductVersion = $msiVersion
    ALLUSERS       = '1'
    UpgradeCode    = $UpgradeCode
}
foreach ($key in $want.Keys) {
    $got = Get-MsiProperty $msi $key
    if ($got -ne $want[$key]) { throw "the .msi has $key '$got', expected '$($want[$key])'" }
}

# Same format as sha256sum: lower-case hash, two spaces, file name, LF; no BOM.
$hash = (Get-FileHash -Algorithm SHA256 $msi).Hash.ToLowerInvariant()
[System.IO.File]::WriteAllText("$msi.sha256", "$hash  $name.msi`n", [System.Text.UTF8Encoding]::new($false))
Write-Output $msi
Get-Content "$msi.sha256"
