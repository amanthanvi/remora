# Remora's Rama DNS adapter

This is the published `rama-dns` 0.3.0-alpha.4 source, retained under its
MIT/Apache-2.0 licenses. Remora changes only `Cargo.toml` and `src/hickory.rs`.
The parent bridge workspace selects this directory through `[patch.crates-io]`.
This directory is ordinary tracked source, not a submodule.

## Provenance

- Registry archive: `https://static.crates.io/crates/rama-dns/rama-dns-0.3.0-alpha.4.crate`
- Archive SHA-256: `e340fef2799277e204260b17af01bc23604712092eacd6defe40167f304baed8`
- Upstream repository: `https://github.com/plabayo/rama`
- Published VCS revision: `4733273a10a791762e2b7727850032b0c9b8536d`, directory `rama-dns`.
- Both license files come from that same repository revision.

## Downstream change

Use published Hickory 0.26.2 rather than 0.25.2, removing the affected package
versions for RUSTSEC-2026-0118 and RUSTSEC-2026-0119. No version is relabeled and
no advisory is ignored. The existing Rama family remains pinned to
0.3.0-alpha.4: its stable release changes the proxy's transport and TLS APIs.

The adapter follows the new Hickory runtime, nameserver, and answer APIs while
preserving fully qualified queries, address lists, separate TXT chunks, and
the existing system-configuration/Cloudflare fallback policy. Hickory's new
construction errors propagate through the existing infallible builder's
lookup methods; system construction remains immediately fallible. DNSSEC and
encrypted DNS features remain disabled. The bridge lockfile keeps the resolver,
net, and proto crates aligned at 0.26.2.

## Verification and removal

Run from the repository root:

```sh
make rust-dns-test
cargo check --locked --manifest-path shared/rust-bridge/Cargo.toml -p codex-network-proxy
make dependency-boundaries-check rust-test
```

Cargo cannot test a dependency's dev-dependencies through the bridge workspace;
the adapter's standalone tests seed their local ignored lockfile from the
authoritative bridge lockfile. `make rust-test` also runs this adapter gate.
The production graph is checked separately.

The adapter tests include loopback UDP A/AAAA/TXT answers, NXDOMAIN, closed-port
failure, valid built-in construction, and construction-error propagation. They
do not use public DNS. Keep both native builds green for changes to this source.

Remove this vendor and the workspace patch when the retained Codex proxy moves
to a compatible Rama release using patched Hickory. Compare all nine source
files with the registry archive when refreshing; preserve the license files
and update this provenance record. Do not grow a separate DNS implementation.
