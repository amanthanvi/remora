# Generated Experimental Schemas

These four JSON files are unmodified output from the retained Codex
`codex-app-server-protocol` generator with experimental APIs enabled. Do not
hand-edit them or infer schemas from method names.

From the repository root:

```sh
bash services/remora-link/crates/bridge-conformance/scripts/experimental-schemas.sh --write
bash services/remora-link/crates/bridge-conformance/scripts/experimental-schemas.sh --check
```

The check regenerates from the current retained source using its locked Cargo
dependencies and compares each fixture byte for byte. It neither edits the
retained source nor replaces its published stable schemas. Run it after applying
the retained patch stack and before claiming source/fixture conformance.
