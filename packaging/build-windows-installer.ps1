[CmdletBinding()]
param(
    [string]$PlayerExe,
    [string]$OutputDirectory
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
if (-not $PlayerExe) {
    $PlayerExe = Join-Path $repo 'target/release/yandex-music-rust.exe'
}
if (-not $OutputDirectory) {
    $OutputDirectory = Join-Path $repo 'target/installer'
}
$PlayerExe = (Resolve-Path -LiteralPath $PlayerExe).Path

$manifest = Get-Content -LiteralPath (Join-Path $repo 'Cargo.toml') -Raw
if ($manifest -notmatch '(?m)^version\s*=\s*"(?<version>\d+\.\d+\.\d+)"') {
    throw 'Could not read the player version from Cargo.toml.'
}
$version = $Matches.version
$binaryVersion = (Get-Item -LiteralPath $PlayerExe).VersionInfo.ProductVersion
if ($binaryVersion -ne $version) {
    throw "Player version '$binaryVersion' does not match Cargo.toml ($version). Build with cargo build --locked --release first."
}

$compiler = (Get-Command ISCC.exe -CommandType Application -ErrorAction SilentlyContinue).Source
if (-not $compiler) {
    $compiler = foreach ($programFiles in @(${env:ProgramFiles(x86)}, $env:ProgramFiles)) {
        if ($programFiles) {
            $candidate = Join-Path $programFiles 'Inno Setup 6/ISCC.exe'
            if (Test-Path -LiteralPath $candidate -PathType Leaf) {
                $candidate
                break
            }
        }
    }
}
if (-not $compiler) {
    throw 'Install Inno Setup 6.3 or newer to build the Windows installer.'
}

$OutputDirectory = (New-Item -ItemType Directory -Force -Path $OutputDirectory).FullName
& $compiler "/O$OutputDirectory" "/DAppVersion=$version" "/DPlayerExe=$PlayerExe" (Join-Path $PSScriptRoot 'windows.iss')
if ($LASTEXITCODE -ne 0) {
    throw "Inno Setup failed with exit code $LASTEXITCODE."
}
