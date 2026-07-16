# Kumiho Brain installer.
#
# One-liner:
#   irm https://raw.githubusercontent.com/KumihoIO/kumiho-SDKs/main/dashboard/scripts/install.ps1 | iex
#
# Downloads the prebuilt kumiho-brain.exe for Windows x86_64 from the newest
# brain-v* GitHub release, verifies it against the release checksums
# (fail-closed), and installs it. No Rust toolchain required.
#
#   $env:KUMIHO_VERSION = "v0.1.0"       pin a release (accepts v0.1.0 or brain-v0.1.0)
#   $env:KUMIHO_INSTALL_DIR = "C:\bin"   override the destination (default ~\.kumiho\bin)
$ErrorActionPreference = "Stop"
$Repo = "KumihoIO/kumiho-SDKs"
$Version = if ($env:KUMIHO_VERSION) { $env:KUMIHO_VERSION } else { "latest" }
$InstallDir = if ($env:KUMIHO_INSTALL_DIR) { $env:KUMIHO_INSTALL_DIR } else { "$HOME\.kumiho\bin" }

$headers = @{ "User-Agent" = "kumiho-brain-installer" }
# This repo hosts several release families (sdk-v*, memory-v*, go/v*), so the
# releases/latest endpoint can't be used — resolve the newest brain-v* tag
# from the release list (newest first) instead.
if ($Version -eq "latest") {
    $releases = Invoke-RestMethod -Headers $headers -Uri "https://api.github.com/repos/$Repo/releases?per_page=100"
    $tag = ($releases | Where-Object { $_.tag_name -like "brain-v*" } | Select-Object -First 1).tag_name
    if (-not $tag) {
        throw "No brain-v* release found in $Repo"
    }
} elseif ($Version -like "brain-v*") {
    $tag = $Version
} elseif ($Version -like "v*") {
    $tag = "brain-$Version"
} else {
    $tag = "brain-v$Version"
}

# Asset names are deterministic: kumiho-brain-windows-x86_64-vX.Y.Z.zip
$shortVersion = $tag -replace "^brain-", ""
$assetName = "kumiho-brain-windows-x86_64-$shortVersion.zip"
$downloadBase = "https://github.com/$Repo/releases/download/$tag"

$tmp = Join-Path $env:TEMP "kumiho-brain-$tag"
if (Test-Path $tmp) {
    Remove-Item -LiteralPath $tmp -Recurse -Force
}
New-Item -ItemType Directory -Path $tmp | Out-Null

$archive = Join-Path $tmp $assetName
Invoke-WebRequest -Uri "$downloadBase/$assetName" -OutFile $archive

# Verify the download against the release checksums (fail-closed).
$checksums = Join-Path $tmp "checksums.txt"
Invoke-WebRequest -Uri "$downloadBase/checksums.txt" -OutFile $checksums
$expectedLine = Get-Content $checksums | Where-Object { $_ -match [regex]::Escape($assetName) } | Select-Object -First 1
if (-not $expectedLine) {
    throw "No checksum entry for $assetName in checksums.txt; refusing to install unverified binary"
}
$expected = ($expectedLine -split "\s+")[0].ToLowerInvariant()
# Get-FileHash is PowerShell 4.0+; compute SHA-256 via the .NET API so the
# installer also works on Windows PowerShell 3.0 (Windows 8 / Server 2012).
$sha256 = [System.Security.Cryptography.SHA256]::Create()
$fileStream = [System.IO.File]::OpenRead($archive)
try {
    $actual = ([System.BitConverter]::ToString($sha256.ComputeHash($fileStream)) -replace "-", "").ToLowerInvariant()
} finally {
    $fileStream.Close()
    $sha256.Dispose()
}
if ($expected -ne $actual) {
    throw "SHA256 mismatch for $assetName"
}

$extract = Join-Path $tmp "extract"
# Expand-Archive is PowerShell 5.0+; use the .NET ZipFile API (PS 3.0 + .NET 4.5).
Add-Type -AssemblyName System.IO.Compression.FileSystem
[System.IO.Compression.ZipFile]::ExtractToDirectory($archive, $extract)
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
$binary = Get-ChildItem -Path $extract -Recurse -Filter "kumiho-brain.exe" | Select-Object -First 1
if (-not $binary) {
    throw "kumiho-brain.exe not found in release archive"
}
$dest = Join-Path $InstallDir "kumiho-brain.exe"
Copy-Item -LiteralPath $binary.FullName -Destination $dest -Force
Remove-Item -LiteralPath $tmp -Recurse -Force

Write-Host "Installed kumiho-brain $shortVersion to $InstallDir"
Write-Host ""
Write-Host "Run it (serves on 127.0.0.1 and opens your browser):"
Write-Host "  $dest --open"
if (($env:PATH -split ";") -notcontains $InstallDir) {
    Write-Host ""
    Write-Host "Optionally add it to your PATH (persists for your user):"
    Write-Host "  [Environment]::SetEnvironmentVariable('Path', `"$InstallDir;`" + [Environment]::GetEnvironmentVariable('Path', 'User'), 'User')"
}
