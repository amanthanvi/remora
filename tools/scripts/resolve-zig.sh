#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
REQUIRED_VERSION="0.15.2"
CACHE_ROOT="${REMORA_TOOLCHAIN_CACHE_DIR:-$REPO_DIR/.local/toolchains}"

validate_zig() {
    local candidate="$1"
    [ -x "$candidate" ] && [ "$("$candidate" version)" = "$REQUIRED_VERSION" ]
}

if [ -n "${ZIG_BIN:-}" ]; then
    if ! validate_zig "$ZIG_BIN"; then
        echo "error: ZIG_BIN must point to Zig $REQUIRED_VERSION: $ZIG_BIN" >&2
        exit 1
    fi
    printf '%s\n' "$ZIG_BIN"
    exit 0
fi

if command -v zig >/dev/null 2>&1; then
    SYSTEM_ZIG="$(command -v zig)"
    if validate_zig "$SYSTEM_ZIG"; then
        printf '%s\n' "$SYSTEM_ZIG"
        exit 0
    fi
fi

case "$(uname -s)-$(uname -m)" in
    Darwin-arm64)
        PLATFORM="aarch64-macos"
        SHA256="3cc2bab367e185cdfb27501c4b30b1b0653c28d9f73df8dc91488e66ece5fa6b"
        ;;
    Darwin-x86_64)
        PLATFORM="x86_64-macos"
        SHA256="375b6909fc1495d16fc2c7db9538f707456bfc3373b14ee83fdd3e22b3d43f7f"
        ;;
    Linux-aarch64)
        PLATFORM="aarch64-linux"
        SHA256="958ed7d1e00d0ea76590d27666efbf7a932281b3d7ba0c6b01b0ff26498f667f"
        ;;
    Linux-x86_64)
        PLATFORM="x86_64-linux"
        SHA256="02aa270f183da276e5b5920b1dac44a63f1a49e55050ebde3aecc9eb82f93239"
        ;;
    *)
        echo "error: no pinned Zig $REQUIRED_VERSION toolchain is available for $(uname -s)/$(uname -m)" >&2
        exit 1
        ;;
esac

INSTALL_DIR="$CACHE_ROOT/zig-$PLATFORM-$REQUIRED_VERSION"
PINNED_ZIG="$INSTALL_DIR/zig"
if validate_zig "$PINNED_ZIG"; then
    printf '%s\n' "$PINNED_ZIG"
    exit 0
fi

mkdir -p "$CACHE_ROOT"
DOWNLOAD_DIR="$(mktemp -d "$CACHE_ROOT/.zig-download.XXXXXX")"
trap 'rm -rf "$DOWNLOAD_DIR"' EXIT
ARCHIVE="$DOWNLOAD_DIR/zig.tar.xz"
URL="https://ziglang.org/download/$REQUIRED_VERSION/zig-$PLATFORM-$REQUIRED_VERSION.tar.xz"

echo "==> Downloading pinned Zig $REQUIRED_VERSION for $PLATFORM..." >&2
curl --fail --location --silent --show-error "$URL" --output "$ARCHIVE"
if command -v shasum >/dev/null 2>&1; then
    printf '%s  %s\n' "$SHA256" "$ARCHIVE" | shasum -a 256 -c - >/dev/null
else
    printf '%s  %s\n' "$SHA256" "$ARCHIVE" | sha256sum -c - >/dev/null
fi

tar -xJf "$ARCHIVE" -C "$DOWNLOAD_DIR"
EXTRACTED_DIR="$DOWNLOAD_DIR/zig-$PLATFORM-$REQUIRED_VERSION"
if ! validate_zig "$EXTRACTED_DIR/zig"; then
    echo "error: downloaded Zig archive did not contain the expected $REQUIRED_VERSION binary" >&2
    exit 1
fi

if [ ! -d "$INSTALL_DIR" ]; then
    mv "$EXTRACTED_DIR" "$INSTALL_DIR"
fi
if ! validate_zig "$PINNED_ZIG"; then
    echo "error: pinned Zig installation failed validation: $PINNED_ZIG" >&2
    exit 1
fi

printf '%s\n' "$PINNED_ZIG"
