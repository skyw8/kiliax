#!/bin/bash
set -euo pipefail

# One-liner install script for kiliax
# Usage: curl -fsSL https://raw.githubusercontent.com/skyw8/kiliax/master/install.sh | bash

REPO="skyw8/kiliax"
BINARY_NAME="kiliax"
INSTALL_DIR="${INSTALL_DIR:-/usr/local/bin}"

log() { echo "$@"; }
warn() { echo "$@" >&2; }
die() { warn "$@"; exit 1; }

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
    local url body
    url="https://api.github.com/repos/$REPO/releases/latest"
    body="$(curl -sS -L \
        -H "Accept: application/vnd.github+json" \
        -H "X-GitHub-Api-Version: 2022-11-28" \
        ${GITHUB_TOKEN:+-H "Authorization: Bearer $GITHUB_TOKEN"} \
        "$url" || true)"
    printf '%s\n' "$body" | grep -m1 '"tag_name":' | sed -nE 's/.*"tag_name"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/p' || true
}

# Download url -> file with useful diagnostics on failure
download_file() {
    local url out curl_err http_code curl_exit
    url="$1"
    out="$2"

    curl_err="$(mktemp)"
    http_code=""
    curl_exit=0

    http_code="$(curl -sS -L -o "$out" -w "%{http_code}" \
        ${KILIAX_INSTALL_DEBUG:+-v} \
        "$url" 2>"$curl_err")" || curl_exit=$?

    if [ "$curl_exit" -ne 0 ]; then
        warn "[!] Download failed (curl exit $curl_exit, HTTP ${http_code:-000})"
        warn "[!] URL: $url"
        sed -n '1,200p' "$curl_err" >&2 || true
        rm -f "$curl_err"
        return 1
    fi

    if [ "${http_code:-000}" != "200" ]; then
        warn "[!] Download failed (HTTP $http_code)"
        warn "[!] URL: $url"
        if [ -s "$out" ]; then
            warn "[!] Response (first 200 bytes):"
            head -c 200 "$out" >&2 || true
            warn ""
        fi
        sed -n '1,200p' "$curl_err" >&2 || true
        rm -f "$curl_err"
        return 1
    fi

    rm -f "$curl_err"
    return 0
}

# Download and install
main() {
    log "[*] Detecting platform..."
    platform=$(detect_platform)
    log "[+] Platform: $platform"

    log "[*] Fetching latest version..."
    version=$(get_latest_version)
    if [ -z "$version" ]; then
        die "[!] Failed to get latest version (hint: try setting GITHUB_TOKEN, or export KILIAX_INSTALL_DEBUG=1)"
    fi
    log "[+] Version: $version"

    # Build download URL
    download_url="https://github.com/$REPO/releases/download/$version/${BINARY_NAME}-${platform}"
    log "[v] Downloading from: $download_url"

    # Create temp directory
    tmp_dir=$(mktemp -d)
    trap 'rm -rf "$tmp_dir"' EXIT

    # Download
    download_file "$download_url" "$tmp_dir/$BINARY_NAME" || die "[!] Download failed"

    # Make executable
    chmod +x "$tmp_dir/$BINARY_NAME"

    # Install
    log "[*] Installing to: $INSTALL_DIR"
    if [ -w "$INSTALL_DIR" ]; then
        mv -f "$tmp_dir/$BINARY_NAME" "$INSTALL_DIR/"
    else
        log "[*] Elevated permissions required..."
        sudo mv -f "$tmp_dir/$BINARY_NAME" "$INSTALL_DIR/"
    fi

    # Verify
    if command -v "$BINARY_NAME" &> /dev/null; then
        log "[+] Installation successful!"
        log ""
        "$BINARY_NAME" --version 2>/dev/null || true
        log ""

        # Create ki alias
        ki_path="$INSTALL_DIR/ki"
        if [ -w "$INSTALL_DIR" ]; then
            ln -sf "$INSTALL_DIR/$BINARY_NAME" "$ki_path"
        else
            sudo ln -sf "$INSTALL_DIR/$BINARY_NAME" "$ki_path"
        fi
        log "[+] Created alias: ki -> $BINARY_NAME"
        log ""
        log "Run 'kiliax --help' or 'ki --help' to get started"
    else
        warn "[!] Installation complete, but $INSTALL_DIR is not in your PATH"
        warn "Add this to your ~/.bashrc or ~/.zshrc:"
        warn "  export PATH=\"$INSTALL_DIR:\$PATH\""
    fi
}

main "$@"
