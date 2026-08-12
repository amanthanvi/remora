#!/usr/bin/env bash
set -euo pipefail

VERIFY_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$VERIFY_ROOT"

echo "==> Checking tracked diff hygiene"
git diff --check

echo "==> Checking locked Rust dependency resolution"
cargo metadata \
  --manifest-path shared/rust-bridge/Cargo.toml \
  --locked \
  --no-deps \
  --format-version 1 >/dev/null

echo "==> Testing bootstrap, sync, and generated-binding hardening"
make sync-codex-test bootstrap-remora-link-test bindings-hardener-test

echo "==> Testing shared Rust runtime"
make rust-test

if [[ "${REMORA_VERIFY_NATIVE:-0}" == "1" ]]; then
  echo "==> Testing native clients"
  make test-ios
  make test-android
else
  echo "==> Native suites skipped; set REMORA_VERIFY_NATIVE=1 for release validation"
fi
