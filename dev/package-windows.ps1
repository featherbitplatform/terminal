# Builds fbterm for Windows (x86-64) and packages it as a per-machine .msi with cargo-wix
# from crates/fbterm/wix/main.wxs. It first stages Microsoft's ConPTY host from NuGet, into the
# release folder, by calling dev/fetch-conpty.ps1 (which holds the package pin and checks its
# SHA-256), so the .msi installs conpty.dll, OpenConsole.exe, conpty-LICENSE.txt and
# conpty-NOTICE.md beside fbterm.exe.
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
# Where dev/fetch-conpty.ps1 stages conpty.dll, OpenConsole.exe and their license and notices:
# cargo wix's CargoTargetBinDir is this same release folder, so the files land where the exe is built.
$conptyDir = Join-Path $target 'release'

Invoke-Native cargo @('build', '--locked', '--release', '-p', 'fbterm')

# The binary: version line, and the version information build.rs embeds with the icon.
$line = Get-FbtermVersion $exe
if ($line -cne "fbterm $Version") { throw "fbterm --version printed '$line', expected 'fbterm $Version'" }
$info = (Get-Item $exe).VersionInfo
if ($info.ProductName -cne 'featherbit terminal' -or $info.ProductVersion -cne $Version) {
    throw "fbterm.exe has no or the wrong version information: '$($info.ProductName)' '$($info.ProductVersion)'"
}

# Microsoft's ConPTY host, staged into the release folder so `cargo wix` installs it beside
# fbterm.exe (main.wxs reads that folder as $(var.ConptyDir)). dev/fetch-conpty.ps1 holds the pin
# and checks the .nupkg's SHA-256; portable-pty sideloads a conpty.dll sitting next to its own
# executable, which is what lets a local shell's kitty and sixel images survive at all.
& (Join-Path $PSScriptRoot 'fetch-conpty.ps1') -OutDir $conptyDir

New-Item -ItemType Directory -Force $dist | Out-Null
Remove-Item -Force -ErrorAction SilentlyContinue $msi, "$msi.sha256"

# The .msi holds the binary exactly as cargo built it above. The one -C argument adds main.wxs's
# ConptyDir preprocessor define: cargo-wix forwards each -C value to candle.exe as it is (its
# --compiler-arg option), and the define must reach candle as one argument. The switch is candle's
# lowercase -d, exactly as cargo-wix builds its own defines in src/create.rs (-dCargoTargetBinDir=
# ...); an uppercase -D is not a candle switch and leaves $(var.ConptyDir) undefined.
Invoke-Native cargo @('wix', '--package', 'fbterm', '--no-build', '--nocapture',
    '--install-version', $msiVersion, '--output', $msi,
    '-C', "-dConptyDir=$conptyDir")
if (-not (Test-Path $msi)) { throw "cargo wix did not write $msi" }

# Read the built package's Property table back: name, version, per-machine, upgrade code. The
# upgrade code is checked here because it is the one thing a release must never change.
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

# Read a whole table of the built package back, one PowerShell object per row: Windows Installer's
# SQL subset has no functions and no joins, so filtering and joining happen here in PowerShell and
# every query is a plain SELECT. -Columns names the query's columns in order, because Record hands
# out values only by 1-based position.
function Get-MsiRows {
    param([string]$Path, [string]$Query, [string[]]$Columns)
    $installer = New-Object -ComObject WindowsInstaller.Installer
    $db = $installer.GetType().InvokeMember('OpenDatabase', 'InvokeMethod', $null, $installer, @($Path, 0))
    $view = $db.GetType().InvokeMember('OpenView', 'InvokeMethod', $null, $db, $Query)
    $view.GetType().InvokeMember('Execute', 'InvokeMethod', $null, $view, $null) | Out-Null
    $rows = @()
    while ($true) {
        $record = $view.GetType().InvokeMember('Fetch', 'InvokeMethod', $null, $view, $null)
        if ($null -eq $record) { break }
        $row = [pscustomobject]@{}
        for ($i = 1; $i -le $Columns.Count; $i++) {
            # $i is the Record's 1-based column number; $Columns itself is a 0-based array.
            $row | Add-Member -NotePropertyName $Columns[$i - 1] -NotePropertyValue (
                $record.GetType().InvokeMember('StringData', 'GetProperty', $null, $record, $i))
        }
        $rows += $row
    }
    $view.GetType().InvokeMember('Close', 'InvokeMethod', $null, $view, $null) | Out-Null
    [System.Runtime.InteropServices.Marshal]::FinalReleaseComObject($view) | Out-Null
    [System.Runtime.InteropServices.Marshal]::FinalReleaseComObject($db) | Out-Null
    [System.Runtime.InteropServices.Marshal]::FinalReleaseComObject($installer) | Out-Null
    return $rows
}

# The staged ConPTY files, by the names they must install as (compared case-insensitively: Windows
# names are). fbterm.exe itself is read too, as the reference for "in the application folder".
# Windows Installer's SQL has no functions and no joins, so the two tables are read whole and joined
# here. Filtering goes on the installed name, never the File table's key column: that holds WiX's
# File Ids (conptyDll, exe0, ...), not names. FileName holds short and long name split by a pipe
# when both are stored, and the long part (or the whole value without a pipe) is what msiexec installs.
$expected = @('conpty.dll', 'OpenConsole.exe', 'conpty-LICENSE.txt', 'conpty-NOTICE.md')
$wanted = $expected + 'fbterm.exe'
$fileRows = Get-MsiRows -Path $msi -Query 'SELECT File, Component_, FileName FROM File' `
    -Columns @('File', 'Component_', 'FileName')
$dirs = @{}
$componentRows = Get-MsiRows -Path $msi -Query 'SELECT Component, Directory_ FROM Component' `
    -Columns @('Component', 'Directory_')
foreach ($row in $componentRows) {
    $dirs[$row.Component] = $row.Directory_
}
$found = @()
foreach ($row in $fileRows) {
    $long = $row.FileName
    if ($long.Contains('|')) { $long = $long.Split('|')[-1] }
    # -in compares case-insensitively, like the checks below and like Windows.
    if ($long -in $wanted) {
        $found += [pscustomobject]@{ Name = $long; Directory = $dirs[$row.Component_] }
    }
}
foreach ($fileName in $expected) {
    $same = @($found | Where-Object { $_.Name -ieq $fileName })
    if ($same.Count -ne 1) {
        throw "the .msi installs $fileName $($same.Count) time(s), expected exactly once (check main.wxs's components and the feature's ComponentRefs)"
    }
}
$exeRows = @($found | Where-Object { $_.Name -ieq 'fbterm.exe' })
if ($exeRows.Count -ne 1) { throw "the .msi has $($exeRows.Count) fbterm.exe rows, expected exactly 1" }
foreach ($item in $found) {
    if ($item.Directory -ne $exeRows[0].Directory) {
        throw "$($item.Name) installs into '$($item.Directory)', not the same folder as fbterm.exe ('$($exeRows[0].Directory)')"
    }
    Write-Output "in the .msi: $($item.Name) in $($item.Directory)"
}

# Same format as sha256sum: lower-case hash, two spaces, file name, LF; no BOM.
$hash = (Get-FileHash -Algorithm SHA256 $msi).Hash.ToLowerInvariant()
[System.IO.File]::WriteAllText("$msi.sha256", "$hash  $name.msi`n", [System.Text.UTF8Encoding]::new($false))
Write-Output $msi
Get-Content "$msi.sha256"
