# Bridge Conformance

Run deterministic checks without starting provider backends:

```sh
cargo test -p remora-bridge-conformance --all-targets
cargo clippy -p remora-bridge-conformance --all-targets -- -D warnings
```

From the repository root, verify the experimental fixtures against a fresh
export from the exact retained source:

```sh
bash services/remora-link/crates/bridge-conformance/scripts/experimental-schemas.sh --check
```

The parity command runs the retained `write_schema_fixtures --experimental`
generator in a temporary directory, compares the four selected files byte for
byte, and removes the temporary export. Use `--write` to regenerate fixtures
after an intentional retained-source update. It honors `CARGO_TARGET_DIR`.

Live tests are ignored by default. Selecting one explicitly requires its
prerequisites and an actual response transcript. Missing prerequisites, empty
captures, and wrong-target captures fail. Aggregate tests preflight every
declared target before starting backends; they cannot pass by skipping targets.
Use a named individual test for bounded target coverage. Live scenarios may use
provider quota and create or modify backend history and configuration.

## Schema Coverage

Response mappings come from the retained Codex `protocol/common.rs`, not a
method-name capitalization convention. Notifications use the published
`ServerNotification.json` envelope, which owns method-to-payload routing.
Unknown methods and missing or invalid schema files fail validation.

`BRIDGE_CONFORMANCE_CODEX_SCHEMA_DIR` points to an exported `json/v2` directory.
Keep its sibling `v1` directory and parent `ServerNotification.json` available.
`BRIDGE_CONFORMANCE_SKIP_UPSTREAM_SCHEMA=1` explicitly disables this layer and
must not be reported as upstream schema conformance. The deterministic schema
proof tests fail when this setting is enabled; a subprocess regression checks
that failure path.

The retained September 9, 2026 published inventory omits experimental schemas
for `mock/experimentalMethod`, `collaborationMode/list`, `thread/turns/list`, and
`thread/backgroundTerminals/clean`. Their explicit mappings use exact generated
fixtures in `tests/fixtures/experimental/` with the default retained directory.
A custom schema override must supply its own experimental schemas; it never
silently falls back to these fixtures. Positive and malformed-payload cases
exercise all four methods in the normal deterministic suite.
`skills/remote/list` and `skills/remote/export` are absent from the retained
upstream protocol and return an explicit unsupported-validation error.

The deterministic tests establish harness behavior and schema validation,
including these four experimental response shapes. They do not establish live
multi-provider behavior or full experimental-method parity.
