#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
MANIFEST="$REPO_DIR/shared/rust-bridge/Cargo.toml"
LOCKFILE="$REPO_DIR/shared/rust-bridge/Cargo.lock"
REVISION="${1:-}"
REMORA_LINK_GIT_URL="${REMORA_LINK_GIT_URL:-}"
DEFAULT_LOCAL_SOURCE="$REPO_DIR/remora-link-host"
SOURCE="${REMORA_LINK_SOURCE:-}"

if [[ ! "$REVISION" =~ ^[0-9a-f]{40}$ ]]; then
  echo "usage: $(basename "$0") <40-character commit>" >&2
  exit 1
fi

DEPENDENCIES=()
while IFS= read -r dependency; do
  DEPENDENCIES+=("$dependency")
done < <(
  awk '
    /^[[:space:]]*remora[-_[:alnum:]]*[[:space:]]*=.*git[[:space:]]*=/ {
      name = $1
      if (match($0, /package[[:space:]]*=[[:space:]]*"[^"]+"/)) {
        value = substr($0, RSTART, RLENGTH)
        sub(/^[^"]*"/, "", value)
        sub(/"$/, "", value)
        name = value
      }
      print name
    }
  ' "$MANIFEST"
)

if [[ "${#DEPENDENCIES[@]}" -eq 0 ]]; then
  echo "error: no Remora Link Git dependencies found in $MANIFEST" >&2
  exit 1
fi

if [[ -z "$REMORA_LINK_GIT_URL" ]]; then
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

if [[ -z "$REMORA_LINK_GIT_URL" ]]; then
  echo "error: no Remora-owned host Git URL is configured" >&2
  exit 1
fi

if [[ -z "$SOURCE" && -d "$DEFAULT_LOCAL_SOURCE/.git" ]]; then
  SOURCE="$DEFAULT_LOCAL_SOURCE"
fi

if [[ -n "$SOURCE" ]]; then
  if [[ ! -d "$SOURCE/.git" ]]; then
    echo "error: REMORA_LINK_SOURCE is not a Git checkout: $SOURCE" >&2
    exit 1
  fi
  if ! git -C "$SOURCE" cat-file -e "$REVISION^{commit}" 2>/dev/null; then
    echo "error: revision is not available from local Remora Link source: $REVISION" >&2
    exit 1
  fi
else
  VERIFY_DIR="$(mktemp -d)"
  trap 'rm -rf "$VERIFY_DIR"' EXIT
  git -C "$VERIFY_DIR" init --quiet --bare
  if ! git -C "$VERIFY_DIR" fetch --quiet --depth=1 \
    "$REMORA_LINK_GIT_URL" "$REVISION"; then
    echo "error: revision is not available from $REMORA_LINK_GIT_URL: $REVISION" >&2
    exit 1
  fi
  rm -rf "$VERIFY_DIR"
  trap - EXIT
fi

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

REMORA_LINK_GIT_URL="$REMORA_LINK_GIT_URL" REVISION="$REVISION" perl -0pi -e '
  BEGIN { $revision = $ENV{REVISION}; }
  s#(remora[-_\w]*\s*=\s*\{[^}\n]*git\s*=\s*")[^"]+("[^}\n]*rev\s*=\s*")[0-9a-f]{40}(")#$1$ENV{REMORA_LINK_GIT_URL}$2$revision$3#g;
' "$MANIFEST"

for package in "${DEPENDENCIES[@]}"; do
  cargo update \
    --quiet \
    --manifest-path "$MANIFEST" \
    -p "$package" \
    --precise "$REVISION"
done

trap - ERR INT TERM
rm -rf "$BACKUP_DIR"
echo "==> Pinned Remora Link dependencies to $REMORA_LINK_GIT_URL@$REVISION"
