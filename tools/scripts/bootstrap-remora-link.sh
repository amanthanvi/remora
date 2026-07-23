#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
DEFAULT_LOCAL_SOURCE="$REPO_DIR/remora-link-host"
MANIFEST="$REPO_DIR/shared/rust-bridge/Cargo.toml"
REMORA_LINK_GIT_URL="${REMORA_LINK_GIT_URL:-}"
SOURCE="${REMORA_LINK_SOURCE:-}"
REVISION="${REMORA_LINK_REV:-${1:-}}"
INSTALL_ROOT="${REMORA_LINK_INSTALL_ROOT:-$REPO_DIR/.local/remora-link}"
PACKAGE_DIR="${REMORA_LINK_PACKAGE_DIR:-crates/remora-link}"

if [[ ! "$REVISION" =~ ^[0-9a-f]{40}$ ]]; then
  echo "usage: REMORA_LINK_REV=<40-character commit> $(basename "$0")" >&2
  echo "       $(basename "$0") <40-character commit>" >&2
  exit 1
fi

if [[ -z "$SOURCE" && -d "$DEFAULT_LOCAL_SOURCE/.git" ]]; then
  SOURCE="$DEFAULT_LOCAL_SOURCE"
fi

if [[ -z "$SOURCE" && -z "$REMORA_LINK_GIT_URL" && -f "$MANIFEST" ]]; then
  REMORA_LINK_GIT_URL="$(
    awk '
      /^[[:space:]]*remora[-_[:alnum:]]*[[:space:]]*=.*git[[:space:]]*=/ {
        if (match($0, /git[[:space:]]*=[[:space:]]*"[^"]+"/)) {
          value = substr($0, RSTART, RLENGTH)
          sub(/^[^"]*"/, "", value)
          sub(/"$/, "", value)
          print value
          exit
        }
      }
    ' "$MANIFEST"
  )"
fi

if [[ -z "$SOURCE" && -z "$REMORA_LINK_GIT_URL" ]]; then
  echo "error: no Remora Link source is configured" >&2
  echo "set REMORA_LINK_SOURCE or REMORA_LINK_GIT_URL" >&2
  exit 1
fi

CHECKOUT_DIR=""
cleanup() {
  if [[ -n "$CHECKOUT_DIR" ]]; then
    rm -rf "$CHECKOUT_DIR"
  fi
}
trap cleanup EXIT

if [[ -n "$SOURCE" ]]; then
  if [[ ! -d "$SOURCE/.git" ]]; then
    echo "error: REMORA_LINK_SOURCE is not a Git checkout: $SOURCE" >&2
    exit 1
  fi
  if ! git -C "$SOURCE" cat-file -e "$REVISION^{commit}" 2>/dev/null; then
    echo "error: local Remora Link checkout does not contain $REVISION: $SOURCE" >&2
    exit 1
  fi
  CHECKOUT_DIR="$(mktemp -d)"
  git -C "$SOURCE" archive --format=tar "$REVISION" |
    tar -xf - -C "$CHECKOUT_DIR"
  SOURCE_DIR="$CHECKOUT_DIR"
else
  CHECKOUT_DIR="$(mktemp -d)"
  git clone --quiet --filter=blob:none "$REMORA_LINK_GIT_URL" "$CHECKOUT_DIR"
  git -C "$CHECKOUT_DIR" checkout --quiet --detach "$REVISION"
  SOURCE_DIR="$CHECKOUT_DIR"
fi

PACKAGE_PATH="$SOURCE_DIR/$PACKAGE_DIR"
if [[ ! -f "$PACKAGE_PATH/Cargo.toml" ]]; then
  echo "error: Remora Link Cargo package not found: $PACKAGE_PATH" >&2
  echo "set REMORA_LINK_PACKAGE_DIR to its path within the source checkout" >&2
  exit 1
fi

cargo install \
  --locked \
  --path "$PACKAGE_PATH" \
  --root "$INSTALL_ROOT" \
  --force

echo "==> Installed Remora Link from $REVISION into $INSTALL_ROOT/bin"
