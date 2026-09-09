# Remora Link Source Ownership

This directory is the Remora-owned host and runtime-bridge source. It was
imported from `amanthanvi/remora-link` revision
`42e27678cda63bda440a8f6620344f10baefea4f` on September 9, 2026, preserving
`LICENSE`, `NOTICE.md`, and the original source layout.

Host and mobile protocol changes now belong in the same Remora change. Keep
the host wire specification and golden vectors aligned with the mobile Rust
decoder and lifecycle tests. Edit this tree, not Cargo's cached Git checkout.

The import is ordinary tracked source, not a submodule. The upstream revision
above records provenance; it is not a claim that subsequent local changes are
published in the former host repository.
