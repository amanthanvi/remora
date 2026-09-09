#!/usr/bin/env bash
set -euo pipefail

mode="${1:---check}"
if [[ "$#" -gt 1 || ( "$mode" != --check && "$mode" != --write ) ]]; then
    printf 'Usage: bash %s [--check|--write]\n' "$0" >&2
    exit 2
fi

crate_dir="$(cd -- "$(dirname -- "$0")/.." && pwd)"
repo_dir="$(cd -- "$crate_dir/../../../.." && pwd)"
schema_dir="$crate_dir/tests/fixtures/experimental"
generated="$(mktemp -d "${TMPDIR:-/tmp}/remora-conformance-schemas.XXXXXXXX")"
trap 'rm -rf -- "$generated"' EXIT

(
    cd -- "$repo_dir/shared/third_party/codex/codex-rs"
    cargo run --locked --package codex-app-server-protocol --bin write_schema_fixtures -- \
        --experimental --schema-root "$generated"
)

files=(
    CollaborationModeListResponse.json
    MockExperimentalMethodResponse.json
    ThreadBackgroundTerminalsCleanResponse.json
    ThreadTurnsListResponse.json
)
if [[ "$mode" == --write ]]; then
    mkdir -p -- "$schema_dir"
fi
for file in "${files[@]}"; do
    if [[ "$mode" == --write ]]; then
        cp -- "$generated/json/v2/$file" "$schema_dir/$file"
    elif ! cmp -- "$generated/json/v2/$file" "$schema_dir/$file"; then
        printf 'Experimental schema drift: %s; regenerate with --write.\n' "$file" >&2
        exit 1
    fi
done
printf 'Experimental conformance schemas %s: %s files match retained source.\n' "$mode" "${#files[@]}"
