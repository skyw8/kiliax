#!/bin/bash
set -e

# One-liner install script for kiliax-tui
# Usage: curl -fsSL https://raw.githubusercontent.com/skyw8/kiliax/main/install.sh | bash

REPO="skyw8/kiliax"
BINARY_NAME="kiliax"
INSTALL_DIR="${INSTALL_DIR:-/usr/local/bin}"

# Detect platform
detect_platform() {
    local os arch

    os=$(uname -s | tr '[:upper:]' '[:lower:]')
    arch=$(uname -m)

    case "$os" in
        linux)
            case "$arch" in
                x86_64) echo "linux-x64" ;;
                aarch64|arm64) echo "linux-arm64" ;;
                *) echo "Unsupported architecture: $arch"; exit 1 ;;
            esac
            ;;
        darwin)
            case "$arch" in
                x86_64) echo "macos-x64" ;;
                arm64) echo "macos-arm64" ;;
                *) echo "Unsupported architecture: $arch"; exit 1 ;;
            esac
            ;;
        *) echo "Unsupported system: $os"; exit 1 ;;
    esac
}

# Get latest version
get_latest_version() {
    curl -s "https://api.github.com/repos/$REPO/releases/latest" | grep '"tag_name":' | sed -E 's/.*"([^"]+)".*/\1/'
}

# Download and install
main() {
    echo "[*] Detecting platform..."
    platform=$(detect_platform)
    echo "[+] Platform: $platform"

    # Check current version
    current_version=""
    if command -v "$BINARY_NAME" &> /dev/null; then
        current_version=$($BINARY_NAME --version 2>/dev/null | grep -oE 'v?[0-9]+\.[0-9]+\.[0-9]+' | head -1 || echo "")
        if [ -n "$current_version" ]; then
            echo "[i] Current version: $current_version"
        fi
    fi

    echo "[*] Fetching latest version..."
    version=$(get_latest_version)
    if [ -z "$version" ]; then
        echo "[!] Failed to get latest version"
        exit 1
    fi

    # Compare versions
    if [ "$current_version" = "$version" ] && [ -z "$FORCE" ]; then
        echo "[+] Already up to date ($version)"
        echo "    Use FORCE=1 to reinstall anyway"
        exit 0
    fi

    if [ -n "$current_version" ]; then
        echo "[^] Updating: $current_version -> $version"
    else
        echo "[+] Version: $version"
    fi

    # Build download URL
    download_url="https://github.com/$REPO/releases/download/$version/${BINARY_NAME}-${platform}"
    echo "[v] Downloading from: $download_url"

    # Create temp directory
    tmp_dir=$(mktemp -d)
    trap "rm -rf $tmp_dir" EXIT

    # Download
    if ! curl -fsSL "$download_url" -o "$tmp_dir/$BINARY_NAME"; then
        echo "[!] Download failed"
        exit 1
    fi

    # Make executable
    chmod +x "$tmp_dir/$BINARY_NAME"

    # Install
    echo "[*] Installing to: $INSTALL_DIR"
    if [ -w "$INSTALL_DIR" ]; then
        mv "$tmp_dir/$BINARY_NAME" "$INSTALL_DIR/"
    else
        echo "[*] Elevated permissions required..."
        sudo mv "$tmp_dir/$BINARY_NAME" "$INSTALL_DIR/"
    fi

    # Verify
    if command -v "$BINARY_NAME" &> /dev/null; then
        echo "[+] Installation successful!"
        echo ""
        "$BINARY_NAME" --version 2>/dev/null || true
        echo ""

        # Create ki alias
        ki_path="$INSTALL_DIR/ki"
        if [ -w "$INSTALL_DIR" ]; then
            ln -sf "$INSTALL_DIR/$BINARY_NAME" "$ki_path"
        else
            sudo ln -sf "$INSTALL_DIR/$BINARY_NAME" "$ki_path"
        fi
        echo "[+] Created alias: ki -> $BINARY_NAME"
        echo ""
        echo "Run 'kiliax --help' or 'ki --help' to get started"
    else
        echo "[!] Installation complete, but $INSTALL_DIR is not in your PATH"
        echo "Add this to your ~/.bashrc or ~/.zshrc:"
        echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
    fi
}

main "$@"
