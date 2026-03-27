# kiliax installation script for Windows
# Usage: iwr -useb https://raw.githubusercontent.com/skyw8/kiliax/master/install.ps1 | iex

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
    Write-Host "[*] Detecting platform..." -ForegroundColor Cyan
    $platform = Detect-Platform
    Write-Host "[+] Platform: $platform" -ForegroundColor Green

    Write-Host "[*] Fetching latest version..." -ForegroundColor Cyan
    $version = Get-LatestVersion
    Write-Host "[+] Version: $version" -ForegroundColor Green

    $downloadUrl = "https://github.com/$Repo/releases/download/$version/${BinaryName}-${platform}.exe"
    Write-Host "[v] Downloading from: $downloadUrl" -ForegroundColor Cyan

    $tmpDir = New-TemporaryFile | ForEach-Object { $_.DirectoryName }
    $tmpFile = "$tmpDir\${BinaryName}.exe"

    try {
        Invoke-WebRequest -Uri $downloadUrl -OutFile $tmpFile -UseBasicParsing

        Write-Host "[*] Installing to: $InstallDir" -ForegroundColor Cyan
        New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

        $targetPath = "$InstallDir\$BinaryName.exe"
        if (Test-Path $targetPath) {
            # Stop running process if any
            $process = Get-Process -Name $BinaryName -ErrorAction SilentlyContinue
            if ($process) {
                Write-Host "[*] Stopping running $BinaryName..." -ForegroundColor Cyan
                Stop-Process -Name $BinaryName -Force -ErrorAction SilentlyContinue
                Start-Sleep -Milliseconds 500
            }
            Remove-Item -Path $targetPath -Force
        }
        Move-Item -Path $tmpFile -Destination $targetPath -Force

        # Add to PATH if not already present
        $currentPath = [Environment]::GetEnvironmentVariable("Path", "User")
        if ($currentPath -notlike "*$InstallDir*") {
            Write-Host "[*] Adding to user PATH..." -ForegroundColor Cyan
            [Environment]::SetEnvironmentVariable("Path", "$currentPath;$InstallDir", "User")
            Write-Host "[!] Please restart your terminal to use $BinaryName" -ForegroundColor Yellow
        }

        Write-Host "[+] Installation successful!" -ForegroundColor Green


        # Create ki alias
        $kiPath = "$InstallDir\ki.exe"
        Copy-Item "$InstallDir\$BinaryName.exe" $kiPath -Force
        Write-Host "[+] Created alias: ki -> $BinaryName" -ForegroundColor Green
        Write-Host ""
        Write-Host "Run 'kiliax --help' or 'ki --help' to get started" -ForegroundColor Cyan
        Write-Host ""
        & "$InstallDir\$BinaryName.exe" --version 2>$null
        Write-Host ""
    }
    finally {
        Remove-Item -Path $tmpDir\* -Recurse -Force -ErrorAction SilentlyContinue
    }
}

Main
