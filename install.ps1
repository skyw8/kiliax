# kiliax installation script for Windows
# Usage: iwr -useb https://raw.githubusercontent.com/skyw8/kiliax/main/install.ps1 | iex

$ErrorActionPreference = "Stop"

$Repo = "skyw8/kiliax"
$BinaryName = "kiliax"
$InstallDir = $env:INSTALL_DIR
if (-not $InstallDir) {
    $InstallDir = "$env:LOCALAPPDATA\Programs\kiliax"
}

function Detect-Platform {
    $arch = $env:PROCESSOR_ARCHITECTURE
    switch ($arch) {
        "AMD64" { return "windows-x64" }
        "ARM64" { return "windows-arm64" }
        default { throw "Unsupported architecture: $arch" }
    }
}

function Get-LatestVersion {
    $response = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest"
    return $response.tag_name
}

function Main {
    Write-Host "🔍 Detecting platform..." -ForegroundColor Cyan
    $platform = Detect-Platform
    Write-Host "✅ Platform: $platform" -ForegroundColor Green

    # Check current version
    $currentVersion = $null
    $installedPath = "$InstallDir\$BinaryName.exe"
    if (Test-Path $installedPath) {
        try {
            $versionOutput = & $installedPath --version 2>$null
            if ($versionOutput -match 'v?(\d+\.\d+\.\d+)') {
                $currentVersion = "v" + $Matches[1]
                Write-Host "📋 Current version: $currentVersion" -ForegroundColor Gray
            }
        } catch { }
    }

    Write-Host "📦 Fetching latest version..." -ForegroundColor Cyan
    $version = Get-LatestVersion

    # Compare versions
    if ($currentVersion -eq $version -and -not $env:FORCE) {
        Write-Host "✅ Already up to date ($version)" -ForegroundColor Green
        Write-Host "   Set `$env:FORCE=1 to reinstall anyway" -ForegroundColor Gray
        return
    }

    if ($currentVersion) {
        Write-Host "⬆️  Updating: $currentVersion → $version" -ForegroundColor Yellow
    } else {
        Write-Host "✅ Version: $version" -ForegroundColor Green
    }

    $downloadUrl = "https://github.com/$Repo/releases/download/$version/${BinaryName}-${platform}.zip"
    Write-Host "⬇️  Downloading from: $downloadUrl" -ForegroundColor Cyan

    $tmpDir = New-TemporaryFile | ForEach-Object { $_.DirectoryName }
    $zipPath = "$tmpDir\${BinaryName}.zip"

    try {
        Invoke-WebRequest -Uri $downloadUrl -OutFile $zipPath -UseBasicParsing

        Write-Host "📂 Installing to: $InstallDir" -ForegroundColor Cyan
        New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

        Expand-Archive -Path $zipPath -DestinationPath $InstallDir -Force

        # Add to PATH if not already present
        $currentPath = [Environment]::GetEnvironmentVariable("Path", "User")
        if ($currentPath -notlike "*$InstallDir*") {
            Write-Host "🔧 Adding to user PATH..." -ForegroundColor Cyan
            [Environment]::SetEnvironmentVariable("Path", "$currentPath;$InstallDir", "User")
            Write-Host "⚠️  Please restart your terminal to use $BinaryName" -ForegroundColor Yellow
        }

        Write-Host "✅ Installation successful!" -ForegroundColor Green
        Write-Host ""
        & "$InstallDir\$BinaryName.exe" --version 2>$null
        Write-Host ""

        # Create ki alias
        $kiPath = "$InstallDir\ki.exe"
        Copy-Item "$InstallDir\$BinaryName.exe" $kiPath -Force
        Write-Host "✅ Created alias: ki -> $BinaryName" -ForegroundColor Green
        Write-Host ""
        Write-Host "Run 'kiliax --help' or 'ki --help' to get started" -ForegroundColor Cyan
    }
    finally {
        Remove-Item -Path $tmpDir\* -Recurse -Force -ErrorAction SilentlyContinue
    }
}

Main
