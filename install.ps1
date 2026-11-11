<#
.SYNOPSIS
    Builds and installs the Ferric Rust AI Coding Agent CLI locally on Windows.
#>

$ErrorActionPreference = "Stop"

# Ensure cargo is accessible
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    $cargoBin = "$HOME\.cargo\bin"
    if (Test-Path "$cargoBin\cargo.exe") {
        $env:PATH = "$cargoBin;$env:PATH"
    }
}

Write-Host "=== Building Ferric AI Coding Agent (agent-cli) ===" -ForegroundColor Cyan

# 1. Build release binaries
cargo build -p agent-cli --release
if ($LASTEXITCODE -ne 0) {
    Write-Warning "Release build encountered an issue. Falling back to debug build..."
    cargo build -p agent-cli
    $binaryPath = ".\target\debug\ferric.exe"
} else {
    $binaryPath = ".\target\release\ferric.exe"
}

if (-not (Test-Path $binaryPath)) {
    $binaryPath = ".\target\release\agent.exe"
}

if (-not (Test-Path $binaryPath)) {
    Write-Error "Could not find built executable at $binaryPath"
}

# 2. Determine install directory
$installDir = "$HOME\.local\bin"
if (-not (Test-Path $installDir)) {
    New-Item -ItemType Directory -Path $installDir -Force | Out-Null
}

$destPath = "$installDir\ferric.exe"
$aliasPath = "$installDir\agent.exe"

Copy-Item -Path $binaryPath -Destination $destPath -Force
Copy-Item -Path $binaryPath -Destination $aliasPath -Force

# Remove any old trademarked binaries
if (Test-Path "$installDir\claude.exe") {
    Remove-Item "$installDir\claude.exe" -Force -ErrorAction SilentlyContinue
}
if (Test-Path "$installDir\claude-free.cmd") {
    Remove-Item "$installDir\claude-free.cmd" -Force -ErrorAction SilentlyContinue
}

# Sign with local development certificate if available (prevents Windows Smart App Control blocking)
$cert = Get-ChildItem Cert:\CurrentUser\My -CodeSigningCert -ErrorAction SilentlyContinue | Select-Object -First 1
if ($cert) {
    Set-AuthenticodeSignature -Certificate $cert -FilePath $destPath -ErrorAction SilentlyContinue | Out-Null
    Set-AuthenticodeSignature -Certificate $cert -FilePath $aliasPath -ErrorAction SilentlyContinue | Out-Null
}

Write-Host "`nSuccessfully installed agent executable to:" -ForegroundColor Green
Write-Host "  $destPath" -ForegroundColor Yellow
Write-Host "  $aliasPath" -ForegroundColor Yellow

# 3. Check PATH
$userPath = [Environment]::GetEnvironmentVariable("PATH", "User")
if ($userPath -notlike "*$installDir*") {
    Write-Host "`nNotice: '$installDir' is not in your User PATH." -ForegroundColor Yellow
    Write-Host "Run the following command to add it permanently:" -ForegroundColor Yellow
    Write-Host "  [Environment]::SetEnvironmentVariable('PATH', `"$userPath;$installDir`", 'User')" -ForegroundColor Cyan
}

Write-Host "`nTest installation by running:" -ForegroundColor Green
Write-Host "  & `"$destPath`" --version" -ForegroundColor Cyan
