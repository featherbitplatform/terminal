# Downloads Microsoft's open-source ConPTY host from NuGet and stages its x64 pair in a folder,
# with the package's MIT license text beside them.
# Runs wherever PowerShell 7 runs (the two files themselves are Windows x64 binaries).
# Usage: pwsh dev/fetch-conpty.ps1 -OutDir <dir>     e.g. pwsh dev/fetch-conpty.ps1 -OutDir target/conpty
# Output: <dir>/conpty.dll, <dir>/OpenConsole.exe and <dir>/conpty-LICENSE.txt, and nothing else.
#
# Why these two files: portable-pty prefers a conpty.dll sitting next to its own executable over
# the system one (`win/psuedocon.rs`: LoadLibraryW on the bare name "conpty.dll", falling back to
# kernel32 when that fails), and that DLL hosts child sessions in OpenConsole.exe from the same
# folder. The system console layer drops the kitty APC and sixel DCS sequences a local program
# writes (measured by .github/workflows/ci.yml's "ConPTY passthrough (bundled)" step, which calls
# this script); the bundled one is what fbterm ships next to fbterm.exe in Step 2b.
param(
    [Parameter(Mandatory = $true)][string]$OutDir
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if ($PSVersionTable.PSVersion.Major -lt 7) { throw 'dev/fetch-conpty.ps1 needs PowerShell 7: run it with pwsh' }

# The pin, in the one place it lives: the package version and the SHA-256 of its .nupkg, both as
# published at https://www.nuget.org/packages/Microsoft.Windows.Console.ConPTY. Its nuspec declares
# <license type="expression">MIT</license>; Step 2b's packaging re-checks what it installs against
# these same two variables.
$Version = '1.25.260930003'
$Sha256 = '02b07b349af66d801159bdf9e440d4a1ce78bb951f37fc8609731665afdae7ee'
$Uri = "https://api.nuget.org/v3-flatcontainer/microsoft.windows.console.conpty/$Version/" +
    "microsoft.windows.console.conpty.$Version.nupkg"

# The x64 pair, by path inside the package (a .nupkg is a zip), and the names they are written
# under. The package also carries win-x86 and win-arm64 copies; fbterm ships x64 only, which
# dev/package-windows.ps1 insists on.
$Inside = [ordered]@{
    'runtimes/win-x64/native/conpty.dll' = 'conpty.dll'
    'build/native/runtimes/x64/OpenConsole.exe' = 'OpenConsole.exe'
}

# The MIT text as published in microsoft/terminal's LICENSE file (its main branch), with Microsoft's
# copyright line. Written here because the package carries no license file at all: its .nupkg holds
# only the binaries and the nuspec that declares the MIT expression.
$License = @'
Copyright (c) Microsoft Corporation. All rights reserved.

MIT License

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED *AS IS*, WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
'@

$out = (New-Item -ItemType Directory -Force -Path $OutDir).FullName
# Expand-Archive insists on a .zip extension, hence the suffix on the download; it unpacks to a
# staging folder so that only the files named above ever reach -OutDir.
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) "fbterm-conpty-$Version.zip"
$stage = Join-Path ([System.IO.Path]::GetTempPath()) "fbterm-conpty-$Version-stage"

try {
    Write-Host "downloading Microsoft.Windows.Console.ConPTY $Version"
    Invoke-WebRequest -Uri $Uri -OutFile $tmp

    $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $tmp).Hash.ToLowerInvariant()
    if ($actual -ne $Sha256) {
        throw "conpty $Version downloaded with sha256 $actual, expected $Sha256. If the package really did change, update both variables at the top of this script together."
    }

    if (Test-Path -LiteralPath $stage) { Remove-Item -Recurse -Force -LiteralPath $stage }
    Expand-Archive -LiteralPath $tmp -DestinationPath $stage

    foreach ($inside in $Inside.Keys) {
        # Join-Path per segment, so the package's forward slashes become this platform's separator.
        $from = $stage
        foreach ($part in ($inside -split '/')) { $from = Join-Path $from $part }
        if (-not (Test-Path -LiteralPath $from -PathType Leaf)) {
            throw "conpty-$Version.zip does not contain $inside: the package layout changed, or this is not the pinned version"
        }
        $item = Get-Item -LiteralPath $from
        if ($item.Length -lt 1) { throw "$inside inside conpty-$Version.zip is empty" }
        $to = Join-Path $out $Inside[$inside]
        Copy-Item -LiteralPath $from -Destination $to -Force
        Write-Host ("{0}: {1} bytes from {2}" -f $Inside[$inside], $item.Length, $inside)
    }

    $licensePath = Join-Path $out 'conpty-LICENSE.txt'
    Set-Content -LiteralPath $licensePath -Value $License -Encoding utf8NoBOM
    Write-Host "conpty-LICENSE.txt: the package's MIT license, from microsoft/terminal's LICENSE"
    Write-Host "conpty $Version staged in $out"
}
finally {
    Remove-Item -LiteralPath $tmp -Force -ErrorAction SilentlyContinue
    if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue }
}
