#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
MANIFEST="$REPO_DIR/shared/rust-bridge/Cargo.toml"
LOCKFILE="$REPO_DIR/shared/rust-bridge/Cargo.lock"
REVISION="${1:-}"

if [[ ! "$REVISION" =~ ^[0-9a-f]{40}$ ]]; then
  echo "usage: $(basename "$0") <40-character commit>" >&2
  exit 1
fi

VERIFY_DIR="$(mktemp -d)"
git -C "$VERIFY_DIR" init --quiet --bare
if ! git -C "$VERIFY_DIR" fetch --quiet --depth=1 \
  https://github.com/amanthanvi/alleycat.git "$REVISION"; then
  rm -rf "$VERIFY_DIR"
  echo "error: revision is not available from amanthanvi/alleycat: $REVISION" >&2
  exit 1
fi
rm -rf "$VERIFY_DIR"

if ! git -C "$REPO_DIR" diff --quiet -- \
  shared/rust-bridge/Cargo.toml shared/rust-bridge/Cargo.lock || \
  ! git -C "$REPO_DIR" diff --cached --quiet -- \
    shared/rust-bridge/Cargo.toml shared/rust-bridge/Cargo.lock; then
  echo "error: commit or revert local Cargo.toml/Cargo.lock changes before updating the pin" >&2
  exit 1
fi

BACKUP_DIR="$(mktemp -d)"
cp "$MANIFEST" "$BACKUP_DIR/Cargo.toml"
cp "$LOCKFILE" "$BACKUP_DIR/Cargo.lock"
RESTORED=0
restore() {
  if [[ "$RESTORED" -eq 0 ]]; then
    cp "$BACKUP_DIR/Cargo.toml" "$MANIFEST"
    cp "$BACKUP_DIR/Cargo.lock" "$LOCKFILE"
    rm -rf "$BACKUP_DIR"
    RESTORED=1
  fi
}
abort_with_status() {
  local status="$1"
  trap - ERR INT TERM
  restore
  exit "$status"
}
trap 'abort_with_status $?' ERR
trap 'abort_with_status 130' INT
trap 'abort_with_status 143' TERM

perl -0pi -e \
  's#(https://github\.com/amanthanvi/alleycat\.git", rev = ")[0-9a-f]{40}(" \})#${1}'"$REVISION"'${2}#g' \
  "$MANIFEST"

for package in \
  alleycat-bridge-core \
  alleycat-pi-bridge \
  alleycat-claude-bridge \
  alleycat-opencode-bridge
do
  cargo update \
    --quiet \
    --manifest-path "$MANIFEST" \
    -p "$package" \
    --precise "$REVISION"
done

trap - ERR INT TERM
rm -rf "$BACKUP_DIR"
echo "==> Pinned Remora's Alleycat fork to $REVISION"
