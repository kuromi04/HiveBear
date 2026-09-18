#!/usr/bin/env bash
set -e

# ==============================================================================
# HiveBear Node Setup Script (Linux / Termux / macOS)
# Maintained by @kuromi04
# ==============================================================================

echo "🐻 Installing HiveBear dependencies..."

if [ -n "$TERMUX_VERSION" ] || [ -d "/data/data/com.termux" ]; then
    echo "📱 Android / Termux detected!"
    pkg update -y && pkg upgrade -y
    pkg install -y git rust binutils nodejs build-essential clang openssl
    echo "✅ Dependencies installed for Termux!"
elif [[ "$OSTYPE" == "linux-gnu"* ]]; then
    echo "🐧 Linux detected!"
    if command -v apt-get &> /dev/null; then
        sudo apt update
        sudo apt install -y libwebkit2gtk-4.1-dev build-essential curl wget file libssl-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev nodejs npm
    elif command -v pacman &> /dev/null; then
        sudo pacman -Syu --noconfirm base-devel webkit2gtk-4.1 curl wget openssl gtk3 libayatana-appindicator nodejs npm
    elif command -v dnf &> /dev/null; then
        sudo dnf install -y webkit2gtk4.1-devel openssl-devel curl wget @development-tools nodejs
    fi
    # Install Rust if not installed
    if ! command -v cargo &> /dev/null; then
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
        source "$HOME/.cargo/env"
    fi
    echo "✅ Dependencies installed for Linux!"
elif [[ "$OSTYPE" == "darwin"* ]]; then
    echo "🍏 macOS detected!"
    if ! command -v xcode-select &> /dev/null; then
        xcode-select --install || true
    fi
    if command -v brew &> /dev/null; then
        brew install node rust
    fi
    echo "✅ Dependencies installed for macOS!"
else
    echo "⚠️ Unsupported or unknown OS via bash. If on Windows, run scripts/setup.ps1"
fi
