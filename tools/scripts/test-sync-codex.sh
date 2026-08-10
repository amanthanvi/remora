#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
SOURCE_SCRIPT="$REPO_DIR/apps/ios/scripts/sync-codex.sh"
TEST_ROOT="$(mktemp -d)"

if [[ -z "$TEST_ROOT" || ! -d "$TEST_ROOT" || "$TEST_ROOT" == "/" ]]; then
  echo "refusing to use invalid test root" >&2
  exit 1
fi

cleanup() {
  if [[ -n "${TEST_ROOT:-}" && -d "$TEST_ROOT" && "$TEST_ROOT" != "/" ]]; then
    rm -rf -- "$TEST_ROOT"
  fi
}
trap cleanup EXIT

RECORDED_COMMIT="1111111111111111111111111111111111111111"
OTHER_COMMIT="2222222222222222222222222222222222222222"

run_case() {
  local case_name="$1"
  local current_commit="$2"
  local initialized="$3"
  local case_root="$TEST_ROOT/$case_name"
  local fake_bin="$case_root/bin"
  local capture="$case_root/git-calls"
  local fixture_script="$case_root/apps/ios/scripts/sync-codex.sh"
  local fixture_submodule="$case_root/shared/third_party/codex"

  mkdir -p \
    "$case_root/apps/ios/scripts" \
    "$fixture_submodule" \
    "$case_root/patches/codex" \
    "$fake_bin"
  cp "$SOURCE_SCRIPT" "$fixture_script"
  chmod +x "$fixture_script"
  : > "$capture"

  while IFS= read -r patch_name; do
    : > "$case_root/patches/codex/$patch_name"
  done < <(
    sed -n '/^PATCH_FILES=(/,/^)/p' "$SOURCE_SCRIPT" |
      sed -nE 's|.*patches/codex/([^"/]+)".*|\1|p'
  )

  if [[ "$initialized" == "yes" ]]; then
    printf '%s\n' "gitdir: fixture" > "$fixture_submodule/.git"
  fi

  cat > "$fake_bin/git" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

{
  for arg in "$@"; do
    printf '<%s>' "$arg"
  done
  printf '\n'
} >> "$SYNC_CODEX_CAPTURE"

if [[ "$#" -eq 7 && "$1" == "-C" && "$2" == "$SYNC_CODEX_REPO_DIR" &&
      "$3" == "submodule" && "$4" == "update" && "$5" == "--init" &&
      "$6" == "--recursive" && "$7" == "shared/third_party/codex" ]]; then
  exit 0
fi

if [[ "$#" -eq 5 && "$1" == "-C" && "$2" == "$SYNC_CODEX_SUBMODULE_DIR" &&
      "$3" == "rev-parse" && "$4" == "--verify" && "$5" == "HEAD" ]]; then
  printf '%s\n' "$SYNC_CODEX_CURRENT_COMMIT"
  exit 0
fi

if [[ "$#" -eq 4 && "$1" == "-C" && "$2" == "$SYNC_CODEX_SUBMODULE_DIR" &&
      "$3" == "rev-parse" && "$4" == "HEAD" ]]; then
  printf '%s\n' "$SYNC_CODEX_CURRENT_COMMIT"
  exit 0
fi

if [[ "$#" -eq 5 && "$1" == "-C" && "$2" == "$SYNC_CODEX_REPO_DIR" &&
      "$3" == "ls-files" && "$4" == "--stage" &&
      "$5" == "shared/third_party/codex" ]]; then
  printf '160000 %s 0\tshared/third_party/codex\n' "$SYNC_CODEX_RECORDED_COMMIT"
  exit 0
fi

if [[ "$#" -eq 6 && "$1" == "-C" && "$2" == "$SYNC_CODEX_SUBMODULE_DIR" &&
      "$3" == "apply" && "$4" == "--reverse" && "$5" == "--check" ]]; then
  exit 0
fi

if [[ "$#" -eq 5 && "$1" == "-C" && "$2" == "$SYNC_CODEX_SUBMODULE_DIR" &&
      "$3" == "rev-parse" && "$4" == "--short" && "$5" == "HEAD" ]]; then
  printf '%.7s\n' "$SYNC_CODEX_CURRENT_COMMIT"
  exit 0
fi

printf 'unexpected git argv:' >&2
printf ' <%s>' "${@:1:8}" >&2
printf '\n' >&2
exit 1
EOF
  chmod +x "$fake_bin/git"

  PATH="$fake_bin:$PATH" \
  SYNC_CODEX_CAPTURE="$capture" \
  SYNC_CODEX_REPO_DIR="$case_root" \
  SYNC_CODEX_SUBMODULE_DIR="$fixture_submodule" \
  SYNC_CODEX_RECORDED_COMMIT="$RECORDED_COMMIT" \
  SYNC_CODEX_CURRENT_COMMIT="$current_commit" \
    "$fixture_script" --preserve-current > "$case_root/output"

  CASE_ROOT="$case_root"
  CASE_SUBMODULE="$fixture_submodule"
  CASE_CAPTURE="$capture"
}

update_call() {
  printf '<-C><%s><submodule><update><--init><--recursive><shared/third_party/codex>' "$1"
}

run_case clean "$RECORDED_COMMIT" no
clean_update_count="$(grep -Fxc -- "$(update_call "$CASE_ROOT")" "$CASE_CAPTURE" || true)"
if [[ "$clean_update_count" -ne 1 ]]; then
  echo "clean case: expected exactly one Codex submodule update" >&2
  exit 1
fi

run_case initialized "$OTHER_COMMIT" yes
initialized_update_count="$(grep -Fxc -- "$(update_call "$CASE_ROOT")" "$CASE_CAPTURE" || true)"
if [[ "$initialized_update_count" -ne 0 ]]; then
  echo "initialized case: expected zero Codex submodule updates" >&2
  exit 1
fi

grep -Fq -- \
  "<-C><$CASE_ROOT><ls-files><--stage><shared/third_party/codex>" \
  "$CASE_CAPTURE" || {
    echo "initialized case: expected recorded gitlink lookup" >&2
    exit 1
  }
grep -Fq -- \
  "<-C><$CASE_SUBMODULE><rev-parse><HEAD>" \
  "$CASE_CAPTURE" || {
    echo "initialized case: expected current commit lookup" >&2
    exit 1
  }

echo "Codex sync bootstrap test passed"
