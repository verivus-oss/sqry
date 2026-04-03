#!/usr/bin/env pwsh

[CmdletBinding()]
param(
    [string]$Version = "latest",
    [ValidateSet("sqry", "sqry-mcp", "sqry-lsp", "all")]
    [string]$Component = "all",
    [string]$InstallDir = "$env:LOCALAPPDATA\Programs\sqry\bin",
    [string]$Repo = "verivus-oss/sqry",
    [switch]$NoChecksum,
    [switch]$VerifySignatures
)

$ErrorActionPreference = "Stop"

function Get-LatestReleaseTag {
    param([string]$Repository)
    $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repository/releases/latest"
    if (-not $release.tag_name) {
        throw "Failed to resolve latest release tag from GitHub API."
    }
    return $release.tag_name
}

function Get-ExpectedChecksum {
    param(
        [string]$ChecksumFile,
        [string]$AssetName
    )

    foreach ($line in Get-Content -Path $ChecksumFile) {
        if ($line -match "^(?<sha>[a-fA-F0-9]{64})\s+\*?(?<name>.+)$") {
            if ($Matches.name.Trim() -eq $AssetName) {
                return $Matches.sha.ToLowerInvariant()
            }
        }
    }

    throw "Missing checksum entry for '$AssetName' in '$ChecksumFile'."
}

function Add-InstallDirToUserPath {
    param([string]$PathToAdd)

    $current = [Environment]::GetEnvironmentVariable("Path", "User")
    $segments = @()
    if ($current) {
        $segments = $current.Split(';', [System.StringSplitOptions]::RemoveEmptyEntries)
    }
    if ($segments -contains $PathToAdd) {
        return $false
    }

    $newValue = if ([string]::IsNullOrWhiteSpace($current)) {
        $PathToAdd
    } else {
        "$current;$PathToAdd"
    }

    [Environment]::SetEnvironmentVariable("Path", $newValue, "User")
    return $true
}

if ($Version -eq "latest") {
    $Version = Get-LatestReleaseTag -Repository $Repo
}

if ($Version -notmatch '^v\d+\.\d+\.\d+$') {
    throw "Version tag must match v<MAJOR>.<MINOR>.<PATCH>. Got '$Version'."
}

$processorArch = $env:PROCESSOR_ARCHITECTURE
if ($processorArch -and $processorArch -ne "AMD64") {
    Write-Warning "This installer downloads the published Windows x86_64 build. Current architecture: $processorArch."
}

$releaseBase = "https://github.com/$Repo/releases/download/$Version"
$versionNum = $Version -replace '^v', ''
$assetName = "sqry-${versionNum}-windows-x86_64.zip"
$checksumName = "SHA256SUMS.txt"
$tmpRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("sqry-install-" + [guid]::NewGuid().ToString("N"))
$archivePath = Join-Path $tmpRoot $assetName
$checksumPath = Join-Path $tmpRoot $checksumName
$extractDir = Join-Path $tmpRoot "extract"

New-Item -ItemType Directory -Path $tmpRoot | Out-Null
New-Item -ItemType Directory -Path $extractDir | Out-Null

try {
    Write-Host "Downloading $assetName..."
    Invoke-WebRequest -Uri "$releaseBase/$assetName" -OutFile $archivePath

    if (-not $NoChecksum) {
        Write-Host "Downloading $checksumName..."
        Invoke-WebRequest -Uri "$releaseBase/$checksumName" -OutFile $checksumPath
        $expected = Get-ExpectedChecksum -ChecksumFile $checksumPath -AssetName $assetName
        $actual = (Get-FileHash -Algorithm SHA256 -Path $archivePath).Hash.ToLowerInvariant()
        if ($expected -ne $actual) {
            throw "Checksum mismatch for $assetName. Expected $expected, got $actual."
        }
        Write-Host "Checksum verified: $assetName"
    }

    if ($VerifySignatures) {
        $cosign = Get-Command cosign -ErrorAction SilentlyContinue
        if (-not $cosign) {
            throw "cosign is required for -VerifySignatures."
        }
        $bundlePath = "$archivePath.bundle"
        $versionEscaped = [regex]::Escape($Version)
        $identity = "^https://github\.com/$([regex]::Escape($Repo).Replace('/', '\/'))/\.github/workflows/oss-distribute\.yml@refs/tags/$versionEscaped$"
        Write-Host "Downloading $assetName.bundle..."
        Invoke-WebRequest -Uri "$releaseBase/$assetName.bundle" -OutFile $bundlePath
        & $cosign.Source verify-blob --bundle $bundlePath --certificate-identity-regexp $identity --certificate-oidc-issuer "https://token.actions.githubusercontent.com" $archivePath | Out-Null
        Write-Host "Signature verified: $assetName"
    }

    Expand-Archive -Path $archivePath -DestinationPath $extractDir -Force
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null

    $components = if ($Component -eq "all") {
        @("sqry", "sqry-mcp", "sqry-lsp")
    } else {
        @($Component)
    }

    foreach ($name in $components) {
        $source = Join-Path $extractDir "$name.exe"
        if (-not (Test-Path $source)) {
            throw "Archive does not contain '$name.exe'."
        }
        $target = Join-Path $InstallDir "$name.exe"
        Copy-Item -Force -Path $source -Destination $target
        Write-Host "Installed: $target"
    }

    $pathUpdated = Add-InstallDirToUserPath -PathToAdd $InstallDir
    if ($pathUpdated) {
        Write-Host ""
        Write-Host "Added '$InstallDir' to the user PATH."
        Write-Host "Open a new PowerShell window before running sqry commands."
    } else {
        Write-Host ""
        Write-Host "'$InstallDir' is already in the user PATH."
    }

    Write-Host ""
    Write-Host "Installation complete: $Version ($Component)"
} finally {
    Remove-Item -Recurse -Force $tmpRoot -ErrorAction SilentlyContinue
}
