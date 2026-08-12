# Plan 021: System surfaces, share, shortcuts, and awareness

Status: **TODO**
Tracker: [#31](https://github.com/amanthanvi/remora/issues/31)
Baseline: Plan 017 reviewed commit
Depends on: Plan 017

## Scope and ownership

Rust owns a five-row expiring SystemSurfaceProjection and opaque routes. Add a
separate authenticated awareness API/grant, iOS Live Activity, Android ongoing
notification/widget parity, privacy settings, share drafts, and allowlisted
shortcuts. The opaque wake contract stays byte-for-byte unchanged.

## Contract and acceptance

Sensitive content/decisions stay in-app. Generic notifications remain default;
rich metadata is explicit. Share caps are 25 MiB/10 items and never auto-send.
Test forbidden fields, priority, expiry, schema/auth, unknown routes, share caps,
and locked-device rendering on both platforms.

Rollback awareness and native extensions independently without touching opaque
wake. STOP on prompt/path/command content, approval actions outside the app, or
route handles that confer authority.
