[CmdletBinding()]
param(
    [string]$PlayerExe,
    [string]$OutputDirectory,
    [ValidatePattern('^[0-9A-Fa-f]{40}$')]
    [string]$SigningThumbprint,
    [string]$SignToolPath,
    [switch]$RequireSignature
)

$ErrorActionPreference = 'Stop'
if ($RequireSignature -and -not $SigningThumbprint) {
    throw 'A code-signing certificate thumbprint is required for this build.'
}
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
$compilerOptions = @("/O$OutputDirectory", "/DAppVersion=$version")
$stagedPlayer = $null
$stage = $null

function Assert-ReleaseSignature([string]$Path) {
    $signature = Get-AuthenticodeSignature -LiteralPath $Path
    if ($signature.Status -ne 'Valid' -or
        $signature.SignerCertificate.Thumbprint -ne $SigningThumbprint -or
        -not $signature.TimeStamperCertificate) {
        throw "Expected a trusted timestamped publisher signature on $Path."
    }
}

try {
    if ($SigningThumbprint) {
        if (-not $SignToolPath) {
            $SignToolPath = (Get-Command signtool.exe -CommandType Application -ErrorAction SilentlyContinue).Source
        }
        if (-not $SignToolPath) {
            throw 'Pass -SignToolPath or add Windows SDK SignTool to PATH.'
        }
        $SignToolPath = (Resolve-Path -LiteralPath $SignToolPath).Path
        $cert = Get-Item -LiteralPath "Cert:\CurrentUser\My\$SigningThumbprint" -ErrorAction SilentlyContinue
        if (-not $cert -or -not $cert.HasPrivateKey) {
            throw 'The certificate with its signing key must be available in CurrentUser\My.'
        }

        $stage = New-Item -ItemType Directory -Path (Join-Path ([IO.Path]::GetTempPath()) ([guid]::NewGuid().ToString()))
        $stagedPlayer = Join-Path $stage.FullName 'yandex-music-rust.exe'
        Copy-Item -LiteralPath $PlayerExe -Destination $stagedPlayer
        $timestamp = 'http://timestamp.digicert.com'
        & $SignToolPath sign /sha1 $SigningThumbprint /fd SHA256 /tr $timestamp /td SHA256 $stagedPlayer
        if ($LASTEXITCODE -ne 0) { throw "Player signing failed with exit code $LASTEXITCODE." }
        Assert-ReleaseSignature $stagedPlayer
        $PlayerExe = $stagedPlayer
        $innoSignTool = '/Srelease=$q{0}$q sign /sha1 {1} /fd SHA256 /tr {2} /td SHA256 $f' -f $SignToolPath, $SigningThumbprint, $timestamp
        $compilerOptions += @('/DReleaseSigning=1', $innoSignTool)
    }

    & $compiler @compilerOptions "/DPlayerExe=$PlayerExe" (Join-Path $PSScriptRoot 'windows.iss')
    if ($LASTEXITCODE -ne 0) {
        throw "Inno Setup failed with exit code $LASTEXITCODE."
    }
    if ($SigningThumbprint) {
        Assert-ReleaseSignature (Join-Path $OutputDirectory "YandexMusicRust-$version-Setup.exe")
    }
} finally {
    if ($stagedPlayer -and (Test-Path -LiteralPath $stagedPlayer)) {
        Remove-Item -LiteralPath $stagedPlayer
    }
    if ($stage) {
        Remove-Item -LiteralPath $stage.FullName
    }
}
