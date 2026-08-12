# Plan 019: Provider parity and Cursor

Status: **TODO**
Tracker: [#29](https://github.com/amanthanvi/remora/issues/29)
Baseline: Plan 018 reviewed commit and exact Link pin
Depends on: Plans 015 and 017–018

## Scope and ownership

Map Codex, Claude, Cursor, Grok, and OpenCode into one typed conformance suite.
Cursor uses `cursor-agent acp` through the existing ACP bridge, `cursor_login`,
model probing, and mapped question/plan/todo extensions.

## Contract and acceptance

Every adapter declares exact support/readiness and bounded reasons. Runtime is
fixed per Thread; same-runtime continuation requires a shared continuation
group; cross-runtime work is a previewed linked child. Test lifecycle, message,
interrupt, input, model, reconnect, and declared degradation for each adapter.

Rollback adapters independently behind truthful unavailable states. STOP on
emulation, fallback, mobile provider-specific navigation, or credential return
to mobile.
