# Dependency security remediation

Updated September 9, 2026. The filename retains the original review date.
See [risk gates](2026-09-09-risk-gates.md) for native, provider, relay, and
platform validation beyond dependency remediation.

## Current result

The mobile bridge's authoritative `shared/rust-bridge/Cargo.lock` passes
`cargo audit` with **zero vulnerability entries**. No advisory ignores,
version relabeling, or registry-source edits were used. Three maintenance
notices remain: `derivative`, `fxhash`, and `paste`. A clean advisory scan is
not an exploit test or proof that all third-party code is secure.

| Previous finding | Resolution |
| --- | --- |
| Hickory RUSTSEC-2026-0118/0119 | Published proto/net/resolver 0.26.2 through the retained Rama DNS adapter; DNSSEC disabled |
| MCP RUSTSEC-2026-0189 | Upgraded rmcp 0.15.0 to patched 1.8.0; outer fixture Host validation retained |
| RSA RUSTSEC-2023-0071, active 0.10 prerelease | Disabled russh default features; retained `flate2` and `aws-lc-rs`, removed RSA support |
| RSA RUSTSEC-2023-0071, lock-only 0.9 | Upgraded SQLite-only SQLx 0.8.6 to 0.9.0; optional MySQL graph no longer selects RSA |
| LRU RUSTSEC-2026-0002/0253 | Retained/direct cache upgraded; Remora TUI uses ratatui 0.30.2 and crossterm 0.29 |
| quick-xml RUSTSEC-2026-0194/0195 | Retained parser upgraded to 0.42.0 |

Both bridge and retained Codex Cargo lockfiles contain no `rsa` package.
The bridge dependency-boundary guard checks supported host/iOS/Android graphs
and rejects RSA, active MySQL, old MCP/DNS dependencies, or activation of the
MCP HTTP server in normal mobile builds. The standalone Codex workspace has
additional non-mobile dependencies; the zero-vulnerability claim above applies
to the bridge lockfile, not every package in the larger Codex workspace.

## MCP transport and OAuth

MCP 1.8.0 supplies the patched server Host policy and public constructors
needed by Remora's custom HTTP adapter. Version 1.4.0 introduced non-exhaustive
`AuthRequiredError` without an external constructor, so merely choosing the
first patched version would not compile this adapter.

The migration preserves caller arguments, request metadata, negotiated
protocol-version headers, session IDs, authentication, pagination, and resource
operations. HTTP POST, GET, and DELETE merge the transport's custom headers
before authoritative session/authentication headers. OAuth uses reqwest 0.13
only inside `codex-rmcp-client`; other Codex HTTP consumers remain on 0.12.
The OAuth metadata client reuses the shared custom-CA rustls configuration,
not a duplicate certificate parser. Explicit client IDs still bypass dynamic
registration through the configured authorization manager.

The fixture's `http-test-server` feature remains explicit. Its outer Host
allowlist protects MCP, control, and OAuth metadata routes, requiring exactly
one bind-derived authority and the correct port. Tests cover missing/duplicate
headers, spoofed suffixes, IPv4/IPv6, wildcard binds, and default-port behavior.
The patched MCP transport is not a reason to remove protection from non-MCP
routes. Do not leave a sensitive fixture running persistently.

The 55-test MCP gate covers OAuth/token persistence, transport parsing, stdio
resource reads, process-group cleanup, three Host tests, four HTTP recovery
tests, and HTTP forwarding through a real Codex exec-server. The remote test
uses `CARGO_BIN_EXE_codex` to identify an executable with exec-server support;
this run used the installed Codex executable, not a rebuilt full Codex CLI.

The standalone MCP server was separately rebuilt from current retained source.
Its 11 unit tests and three subprocess integrations passed, including base
instructions and shell/patch approval elicitation. The runtime run exposed a
stale test expectation for version `0.0.0`; it now uses the existing computed
package version, matching the server's unchanged version-reporting behavior.

## RSA and SQLite

RSA support is removed rather than mitigated with an advisory ignore. RSA-only
SSH hosts are intentionally unsupported, as are imported RSA credentials.
Ed25519/ECDSA hosts and credentials remain supported. Existing credentials are
not deleted or rewritten. SSH regression coverage and results are recorded by
the root verification gate.

SQLx 0.8.6 resolved an RSA dependency through optional MySQL even with only
SQLite selected. A fresh isolated 0.9.0 resolution eliminated RSA without
manually deleting lock entries. The adopted migration uses `runtime-tokio`,
preserves migrator table/schema metadata, and adapts `QueryBuilder` ownership.

The four `AssertSqlSafe` callsites contain audited static SQL fragments or
generated `(?)` placeholder counts. Thread IDs, filters, goal fields, and other
caller values remain SQL bind parameters. The annotation does not bless
arbitrary input SQL. SQLite's 130 tests cover migration upgrades, logs, goal
accounting, thread filtering, stale metadata protection, and documentation.

SQLx 0.9 requires Rust 1.94. Retained Cargo and Bazel toolchains pin Rust 1.98.1.
The exact local toolchain was installed and matches Remora's stable compiler.
Both retained Cargo and Bazel locks were regenerated.

## Other dependency boundaries

The narrow Rama DNS adapter remains in `shared/third_party/rama-dns`, with
licenses, provenance, and removal criteria in its `REMORA.md`. It retains the
proxy API while using patched Hickory. Its tests include loopback A/AAAA/TXT,
NXDOMAIN, closed ports, FQDN preservation, and construction failures. Remove
it when upstream Codex supports a Rama release with patched Hickory.

The TUI LRU migration uses released ratatui 0.30.2; no cache fork was introduced.
Tests exercise visible rendering and cached layout reuse.

## Remaining maintenance notices

These are maintenance risks, not newly established exploitable vulnerabilities:

- `derivative`: selected by Starlark. Current published Starlark 0.14.2 still
  declares derivative 2.2, so upgrading from 0.13 does not remove it.
- `fxhash`: selected by BM25 and Starlark's map. Current published BM25 2.3.2
  is already selected and still declares fxhash 0.2.1.
- `paste`: used by Starlark and other retained native/runtime dependencies.
  Current published Starlark 0.14.2 still declares paste 1.0.

These released manifests were checked directly on September 9. A version-only
upgrade cannot close the notices. Replacing the execpolicy interpreter,
retrieval scorer, or native runtime is a separate semantic migration requiring
feature-equivalence tests. No speculative major upgrade, replacement package
label, or warning suppression was applied.

## Reproduction and provenance

Run from `shared/rust-bridge`:

```sh
cargo check --locked -p codex-mobile-client --lib
cargo audit --json
```

Run from the retained Codex checkout (`justfile` sets the Rust directory):

```sh
just fmt
just bazel-lock-update
just bazel-lock-check
just write-app-server-schema
```

Run from `shared/third_party/codex/codex-rs`:

```sh
cargo test -p codex-state -p codex-app-server-protocol
cargo test -p codex-mcp-server --all-targets
CARGO_BIN_EXE_codex=/absolute/path/to/codex cargo test \
  -p codex-rmcp-client --features http-test-server --all-targets
```

Local Codex changes are carried by registered patches documented in
`patches/codex/README.md`; no submodule commit or push was made. The dependency
patch distinguishes the internal dynamic-tool JSON schema from the API record
and refreshes generated schema fixtures against the complete Remora patch set.
This fixes a numbered-schema collision exposed by the full protocol tests.
Generated fixtures are not handwritten protocol definitions.

Local evidence includes `/tmp/remora-bridge-audit-after-upgrade.json`,
`/tmp/remora-dependency-final-tests.log`, `/tmp/remora-dependency-clippy.log`,
and `/tmp/remora-bazel-lock-check.log`. The companion risk-gates report records
final native and shared-suite outcomes. Audit and package-maintenance status
must be refreshed when the dependency graph changes.

Standalone runtime evidence is `/tmp/remora-mcp-server-runtime-tests.log`.
Its macOS debug link warns that unwind metadata exceeds the compact-unwind
table's 16 MiB encoding limit; all runtime tests pass. This is not a release
exception-handling performance measurement or a mobile build warning.
