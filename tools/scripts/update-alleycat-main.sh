#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

MODE="all"
case "${1:-}" in
  "")
    ;;
  --all|--shared)
    MODE="${1#--}"
    ;;
  *)
    echo "usage: $(basename "$0") [--all|--shared]" >&2
    exit 1
    ;;
esac

if [ "${REMORA_SKIP_ALLEYCAT_UPDATE:-0}" = "1" ]; then
  echo "==> Skipping Alleycat main refresh (REMORA_SKIP_ALLEYCAT_UPDATE=1)"
  exit 0
fi

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo is required" >&2
  exit 1
fi

ALLEYCAT_MAIN_SHA="$(
  git ls-remote https://github.com/dnakov/alleycat.git refs/heads/main \
    | awk '{ print $1; exit }'
)"
if [ -z "$ALLEYCAT_MAIN_SHA" ]; then
  echo "error: could not resolve dnakov/alleycat main" >&2
  exit 1
fi

update_shared() {
  echo "==> Resolving shared Rust Alleycat deps to dnakov/alleycat main ($ALLEYCAT_MAIN_SHA)..."
  for package in \
    alleycat-bridge-core \
    alleycat-pi-bridge \
    alleycat-claude-bridge \
    alleycat-opencode-bridge
  do
    cargo update \
      --quiet \
      --manifest-path "$REPO_DIR/shared/rust-bridge/Cargo.toml" \
      -p "$package" \
      --precise "$ALLEYCAT_MAIN_SHA"
  done
}

case "$MODE" in
  all|shared)
    update_shared
    ;;
esac
