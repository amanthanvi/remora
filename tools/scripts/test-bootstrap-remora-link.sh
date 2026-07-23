#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
TEST_ROOT="$(mktemp -d)"
cleanup() {
  rm -rf "$TEST_ROOT"
}
trap cleanup EXIT

SOURCE="$TEST_ROOT/source"
FAKE_BIN="$TEST_ROOT/bin"
INSTALL_ROOT="$TEST_ROOT/install"
CAPTURE="$TEST_ROOT/capture"
mkdir -p "$SOURCE/crates/remora-link/src" "$FAKE_BIN"

git -C "$SOURCE" init --quiet
git -C "$SOURCE" config user.email "bootstrap-test@remora.invalid"
git -C "$SOURCE" config user.name "Remora Bootstrap Test"

cat > "$SOURCE/Cargo.toml" <<'EOF'
[workspace]
members = ["crates/remora-link"]
resolver = "2"
EOF
cat > "$SOURCE/Cargo.lock" <<'EOF'
version = 3
EOF
cat > "$SOURCE/crates/remora-link/Cargo.toml" <<'EOF'
[package]
name = "remora-link"
version = "0.0.0"
edition = "2021"
EOF
printf '%s\n' 'reviewed-source' > "$SOURCE/crates/remora-link/src/marker"
git -C "$SOURCE" add .
git -C "$SOURCE" commit --quiet -m "reviewed fixture"
REVISION="$(git -C "$SOURCE" rev-parse HEAD)"

# Both dirty tracked content and an untracked package file must be absent from
# the exact-commit archive consumed by the bootstrap.
printf '%s\n' 'dirty-source' > "$SOURCE/crates/remora-link/src/marker"
printf '%s\n' 'untracked-source' > "$SOURCE/crates/remora-link/src/untracked"

cat > "$FAKE_BIN/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
package_path=""
while (($#)); do
  if [[ "$1" == "--path" ]]; then
    package_path="$2"
    shift 2
  else
    shift
  fi
done
test -n "$package_path"
cat "$package_path/src/marker" > "$BOOTSTRAP_CAPTURE/marker"
if [[ -e "$package_path/src/untracked" ]]; then
  printf '%s\n' present > "$BOOTSTRAP_CAPTURE/untracked"
fi
EOF
chmod +x "$FAKE_BIN/cargo"
mkdir -p "$CAPTURE"

PATH="$FAKE_BIN:$PATH" \
BOOTSTRAP_CAPTURE="$CAPTURE" \
REMORA_LINK_SOURCE="$SOURCE" \
REMORA_LINK_INSTALL_ROOT="$INSTALL_ROOT" \
  "$SCRIPT_DIR/bootstrap-remora-link.sh" "$REVISION" >/dev/null

test "$(cat "$CAPTURE/marker")" = "reviewed-source"
test ! -e "$CAPTURE/untracked"
echo "Remora Link bootstrap exact-commit test passed"
