# Downloads Microsoft's open-source ConPTY host from NuGet and stages its x64 pair in a folder,
# with the license and third-party notices of the microsoft/terminal release those binaries were
# built from, beside them. Runs wherever PowerShell 7 runs; the staged binaries are Windows x64
# files.
# Usage: pwsh dev/fetch-conpty.ps1 -OutDir <dir>     e.g. pwsh dev/fetch-conpty.ps1 -OutDir target/conpty-test
# Output: <dir>/conpty.dll, <dir>/OpenConsole.exe, <dir>/conpty-LICENSE.txt and
#         <dir>/conpty-NOTICE.md, and nothing else.
#
# Why these two files: portable-pty prefers a conpty.dll sitting next to its own executable over
# the system one (`win/psuedocon.rs`: LoadLibraryW on the bare name "conpty.dll", falling back to
# kernel32 when that fails), and that DLL hosts child sessions in OpenConsole.exe from the same
# folder. The system console layer drops the kitty APC and sixel DCS sequences a local program
# writes (measured by .github/workflows/ci.yml's "ConPTY passthrough (bundled)" step, which calls
# this script); the bundled one is what fbterm ships next to fbterm.exe in the installer.
param(
    [Parameter(Mandatory = $true)][string]$OutDir
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if ($PSVersionTable.PSVersion.Major -lt 7) { throw 'dev/fetch-conpty.ps1 needs PowerShell 7: run it with pwsh' }

# The pin, in the one place it lives: the package version and the SHA-256 of its .nupkg, both as
# published at https://www.nuget.org/packages/Microsoft.Windows.Console.ConPTY. Its nuspec declares
# <license type="expression">MIT</license>; dev/package-windows.ps1 calls this script and then checks
# that the built .msi installs each of the four files these pins stage, once, beside fbterm.exe.
$Version = '1.25.260930003'
$Sha256 = '02b07b349af66d801159bdf9e440d4a1ce78bb951f37fc8609731665afdae7ee'

# The microsoft/terminal release these binaries came from, and the SHA-256 of its LICENSE and
# NOTICE.md as served by raw.githubusercontent.com. The package itself names no commit; the match is
# by date, which is how the controller pinned it: this package's files are dated 2026-09-30/10-01
# and v1.25.2733.0 released 2026-10-02, while the next package (1.25.261109001) matches the later
# tag v1.25.3131.0 (2026-11-11). A future version bump must name the tag that matches its own date,
# and re-check both hashes. The notices ship because OpenConsole.exe and conpty.dll incorporate
# third-party code listed in NOTICE.md, and redistributing the binaries carries those obligations.
$Tag = 'v1.25.2733.0'
$LicenseSha256 = '5d177f23ecfeb0ea8e050b6a5a16355e1ae9a0b286436ca8f83ed08b3795be6b'
$NoticeSha256 = 'e7fbaadee6ab20c28b87730a510ee5f5815d8fb4bd88d1d54d282dc2a74c0726'

$Uri = "https://api.nuget.org/v3-flatcontainer/microsoft.windows.console.conpty/$Version/" +
    "microsoft.windows.console.conpty.${Version}.nupkg"
$TagBase = "https://raw.githubusercontent.com/microsoft/terminal/${Tag}"

# The x64 pair, by path inside the package (a .nupkg is a zip), and the names they are written
# under. The package also carries win-x86 and win-arm64 copies; fbterm ships x64 only, which
# dev/package-windows.ps1 insists on. PowerShell variable names are case-insensitive, so nothing here
# may differ from something else by case alone: the loop below reads `$entry`, never a name that
# would alias this map or anything else in the script.
$Files = [ordered]@{
    'runtimes/win-x64/native/conpty.dll'        = 'conpty.dll'
    'build/native/runtimes/x64/OpenConsole.exe' = 'OpenConsole.exe'
}

# The two notice documents, by path in the tagged source tree and the names they are written under.
$Notices = [ordered]@{
    'LICENSE'   = 'conpty-LICENSE.txt'
    'NOTICE.md' = 'conpty-NOTICE.md'
}

$out = (New-Item -ItemType Directory -Force -Path $OutDir).FullName
# The download and the two documents land in temp files with names unique to this run, so two
# concurrent runs cannot collide; they are removed in the finally below whatever happens.
$rand = [System.IO.Path]::GetRandomFileName()
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) "fbterm-conpty-$rand.nupkg"
$tmpLicense = Join-Path ([System.IO.Path]::GetTempPath()) "fbterm-conpty-$rand-license"
$tmpNotice = Join-Path ([System.IO.Path]::GetTempPath()) "fbterm-conpty-$rand-notice"
$zip = $null

try {
    Write-Host "downloading Microsoft.Windows.Console.ConPTY $Version"
    Invoke-WebRequest -Uri $Uri -OutFile $tmp

    $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $tmp).Hash.ToLowerInvariant()
    if ($actual -ne $Sha256) {
        throw "conpty $Version downloaded with sha256 $actual, expected ${Sha256}. If the package really did change, update both variables at the top of this script together."
    }

    # A .nupkg is a zip: open it and copy out only the two x64 entries, so nothing else from the
    # package ever reaches -OutDir. Entry names are matched case-insensitively because zips may
    # store them either way; the pinned layout uses forward slashes.
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [System.IO.Compression.ZipFile]::OpenRead($tmp)
    foreach ($entry in $Files.Keys) {
        $item = $zip.Entries | Where-Object { $_.FullName -ieq $entry }
        if (-not $item) {
            throw "conpty-${Version}.nupkg does not contain ${entry}: the package layout changed, or this is not the pinned version"
        }
        $to = Join-Path $out $Files[$entry]
        [System.IO.Compression.ZipFileExtensions]::ExtractToFile($item, $to, $true)
        if ((Get-Item -LiteralPath $to).Length -lt 1) { throw "$entry inside conpty-${Version}.nupkg is empty" }
        Write-Host ("{0}: staged from {1}" -f $Files[$entry], $entry)
    }
    $zip.Dispose()
    $zip = $null

    # The license and the notices, byte for byte: download each to a file and hash the file. Never
    # round-trip them through a string, which could change a line ending or a BOM between fetch and
    # write. A mismatched hash names the variable to update alongside the tag.
    $want = [ordered]@{ 'license' = $LicenseSha256; 'notice' = $NoticeSha256 }
    $tmpOf = [ordered]@{ 'license' = $tmpLicense; 'notice' = $tmpNotice }
    foreach ($doc in $Notices.Keys) {
        # The document's kind, for looking up its hash and temp file: LICENSE -> license,
        # NOTICE.md -> notice.
        $kind = $doc.Replace('.md', '').ToLowerInvariant()
        Write-Host "downloading microsoft/terminal $doc from $TagBase"
        Invoke-WebRequest -Uri "$TagBase/$doc" -OutFile $tmpOf[$kind]
        $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $tmpOf[$kind]).Hash.ToLowerInvariant()
        if ($actual -ne $want[$kind]) {
            throw "${doc} at ${Tag} downloaded with sha256 $actual, expected $($want[$kind]). Update the tag and both hashes together, from the release that matches the package's build date."
        }
        $to = Join-Path $out $Notices[$doc]
        Copy-Item -LiteralPath $tmpOf[$kind] -Destination $to -Force
        Write-Host ("{0}: microsoft/terminal {1}, from {2}" -f $Notices[$doc], $doc, $Tag)
    }

    Write-Host "conpty $Version staged in $out"
} finally {
    if ($zip) { $zip.Dispose() }
    Remove-Item -LiteralPath $tmp, $tmpLicense, $tmpNotice -Force -ErrorAction SilentlyContinue
}
