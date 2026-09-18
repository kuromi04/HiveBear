# ==============================================================================
# HiveBear Windows Environment Setup Script
# Maintained by @kuromi04
# ==============================================================================

Write-Host "🐻 HiveBear Windows Setup starting..." -ForegroundColor Cyan

# Check Node.js
if (-not (Get-Command node -ErrorAction SilentlyContinue)) {
    Write-Host "📦 Installing Node.js via winget..." -ForegroundColor Yellow
    winget install OpenJS.NodeJS.LTS --accept-package-agreements --accept-source-agreements
} else {
    Write-Host "✅ Node.js is already installed." -ForegroundColor Green
}

# Check Rust / Cargo
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Host "📦 Installing Rust..." -ForegroundColor Yellow
    winget install Rustlang.Rustup --accept-package-agreements --accept-source-agreements
} else {
    Write-Host "✅ Rust/Cargo is already installed." -ForegroundColor Green
}

# Check Visual Studio C++ Build Tools
Write-Host "💡 Note: Make sure 'Desktop development with C++' is installed in Visual Studio Build Tools." -ForegroundColor Cyan
Write-Host "🎉 Environment setup check completed!" -ForegroundColor Green
