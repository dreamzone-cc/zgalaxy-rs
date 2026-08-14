# =============================================================================
# ZGALAXY-RS — Windows PowerShell One-Line Installer
# =============================================================================
# Run in Administrator PowerShell:
#   irm https://raw.githubusercontent.com/dreamzone-cc/ZGALAXY/main/zgalaxy-rs/install.ps1 | iex

$ErrorActionPreference = "Stop"

Write-Host "[ZGALAXY-RS] Installing ZGALAXY Sovereign Rust Client for Windows..." -ForegroundColor Cyan

# Verify Cargo / Rust
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Host "[ZGALAXY-RS] Rust not detected. Installing Rustup..." -ForegroundColor Yellow
    Invoke-WebRequest -Uri "https://win.rustup.rs/x86_64" -OutFile "rustup-init.exe"
    .\rustup-init.exe -y --profile minimal
    Remove-Item -Force "rustup-init.exe"
    $env:Path += ";$env:USERPROFILE\.cargo\bin"
}

# Build Release Binary
Write-Host "[ZGALAXY-RS] Building high-performance release binary..." -ForegroundColor Green
cargo build --release

$InstallDir = "C:\Program Files\ZGALAXY"
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
Copy-Item -Force "target\release\zgalaxy-rs.exe" "$InstallDir\zgalaxy-rs.exe"

# Add to System Path
$CurrentPath = [Environment]::GetEnvironmentVariable("Path", "Machine")
if ($CurrentPath -notlike "*$InstallDir*") {
    [Environment]::SetEnvironmentVariable("Path", "$CurrentPath;$InstallDir", "Machine")
}

Write-Host "[ZGALAXY-RS] Installation completed successfully! Run 'zgalaxy-rs' to start." -ForegroundColor Green
