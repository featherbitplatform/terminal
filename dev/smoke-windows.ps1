# Installs the Windows .msi and checks it, as the release workflow's smoke job does: a silent
# per-machine install, `fbterm --version`, the Start menu shortcut, a silent uninstall that
# keeps the user's config folder, then a reinstall and a final uninstall.
# Run as administrator on a clean machine (a CI runner): it installs into Program Files and
# writes a sample settings file into the current user's real fbterm config folder.
# Usage: pwsh dev/smoke-windows.ps1 <dist-dir> <version> [<msiexec-log-dir>]
param(
    [Parameter(Mandatory = $true)][string]$Dist,
    [Parameter(Mandatory = $true)][string]$Version,
    [string]$Logs = (Join-Path ([System.IO.Path]::GetTempPath()) 'fbterm-msi-logs')
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if (-not $IsWindows) { throw 'smoke-windows.ps1 runs on Windows only' }
$name = "fbterm-$Version-windows-x86_64"
$msi = (Resolve-Path (Join-Path $Dist "$name.msi")).Path

# The checksum file, in sha256sum's format.
$line = (Get-Content (Join-Path $Dist "$name.msi.sha256") -Raw).Trim()
$hash = (Get-FileHash -Algorithm SHA256 $msi).Hash.ToLowerInvariant()
if ($line -cne "$hash  $name.msi") { throw "checksum file says '$line', the .msi hashes to $hash" }

$installDir = Join-Path $env:ProgramFiles 'featherbit terminal'
$exe = Join-Path $installDir 'fbterm.exe'
$shortcut = Join-Path $env:ProgramData 'Microsoft\Windows\Start Menu\Programs\featherbit terminal.lnk'
# fbterm's config folder (fbterm-config's Paths: directories' ProjectDirs "io.featherbit.fbterm").
$configDir = Join-Path $env:APPDATA 'featherbit\fbterm\config'
New-Item -ItemType Directory -Force $Logs | Out-Null

# msiexec, silent, with a verbose log; stops on any result but success.
function Invoke-Msiexec {
    param([string]$Action, [string]$Log)
    $logFile = Join-Path $Logs $Log
    $p = Start-Process -FilePath msiexec.exe -Wait -PassThru `
        -ArgumentList @($Action, "`"$msi`"", '/qn', '/norestart', '/l*v', "`"$logFile`"")
    if ($p.ExitCode -ne 0) {
        Get-Content $logFile -Tail 60
        throw "msiexec $Action failed with exit code $($p.ExitCode) (log: $logFile)"
    }
}

# `fbterm --version`, read from a file (release builds are GUI programs), with a timeout.
function Assert-Version {
    $out = New-TemporaryFile
    try {
        $p = Start-Process -FilePath $exe -ArgumentList '--version' -NoNewWindow -PassThru `
            -RedirectStandardOutput $out.FullName
        $null = $p.Handle # keeps the exit code readable after the process ends
        if (-not $p.WaitForExit(20000)) {
            $p.Kill()
            throw "$exe --version did not exit"
        }
        $got = "$(Get-Content -Raw $out.FullName)".Trim()
        if ($p.ExitCode -ne 0 -or $got -cne "fbterm $Version") {
            throw "$exe --version printed '$got' (exit $($p.ExitCode)), expected 'fbterm $Version'"
        }
        Write-Output $got
    } finally {
        Remove-Item -Force $out.FullName
    }
}

function Assert-Installed {
    foreach ($f in @($exe, (Join-Path $installDir 'LICENSE.txt'), $shortcut)) {
        if (-not (Test-Path $f)) { throw "missing after install: $f" }
    }
    $link = (New-Object -ComObject WScript.Shell).CreateShortcut($shortcut)
    if ($link.TargetPath -ne $exe) { throw "the Start menu shortcut points to '$($link.TargetPath)', not $exe" }
    Assert-Version
}

function Assert-Removed {
    foreach ($f in @($installDir, $shortcut)) {
        if (Test-Path $f) { throw "left behind by uninstall: $f" }
    }
}

if (Test-Path $installDir) { throw "$installDir exists before the install; run on a clean machine" }

# 1. Install and run.
Invoke-Msiexec '/i' 'install.log'
Assert-Installed

# 2. The user's settings, and a stand-in for saved secrets, as fbterm would write them.
New-Item -ItemType Directory -Force $configDir | Out-Null
$settings = Join-Path $configDir 'settings.toml'
$secrets = Join-Path $configDir 'secrets.dpapi'
Set-Content -Path $settings -Value "# smoke test`nfont_size = 15.0`n" -NoNewline
Set-Content -Path $secrets -Value 'smoke test' -NoNewline
$settingsHash = (Get-FileHash $settings).Hash

# 3. Uninstall: the program goes, the config folder stays untouched.
Invoke-Msiexec '/x' 'uninstall.log'
Assert-Removed
foreach ($f in @($settings, $secrets)) {
    if (-not (Test-Path $f)) { throw "uninstall removed $f" }
}
if ((Get-FileHash $settings).Hash -ne $settingsHash) { throw "uninstall changed $settings" }

# 4. Reinstall over the kept config, then remove again.
Invoke-Msiexec '/i' 'reinstall.log'
Assert-Installed
Invoke-Msiexec '/x' 'uninstall-2.log'
Assert-Removed
if (-not (Test-Path $settings)) { throw "the second uninstall removed $settings" }

Write-Output "smoke ok: $name.msi on $((Get-CimInstance Win32_OperatingSystem).Caption)"
