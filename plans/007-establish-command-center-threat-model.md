# Plan 007: Establish the command-center threat model

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. Touch
> only the files listed as in scope. If any STOP condition occurs, stop and
> report; do not improvise. Commit the work in the isolated worktree. When
> dispatched by the Improve advisor, do not update `plans/README.md`; the
> reviewer maintains the index.
>
> **Drift check (run first)**:
> `git diff --stat 58d2e8d812ef8039cd69789a96d5427e93226fa2..HEAD -- README.md CONTEXT.md docs/security/remora-threat-model.md`
> Expected: no output. Any output is a STOP condition.

## Status

- **Priority**: P0
- **Effort**: M
- **Risk**: LOW
- **Depends on**: Plans 001 and 003
- **Category**: security, documentation
- **Planned at**: approved Plan 003 commit
  `58d2e8d812ef8039cd69789a96d5427e93226fa2`, 2026-08-09
- **Tracker**: <https://github.com/amanthanvi/remora/issues/19>
- **Execution status**: DONE at
  `6faa9c3e3667a38c5865021baffca1b416c1eb97`; independent of blocked Plan 002
  and isolated Plan 006.

## Why this matters

The repository has strong, implementation-specific Remora Link and pairing
security records, but no canonical threat model for the accepted command-center
architecture. Browser automation, encrypted device persistence, rich system
awareness, managed Hosts, passkey administration, and signed updates add new
boundaries. A single model is needed before those tracks expand.

The document must be honest about lifecycle state. Existing controls are
`Implemented`; accepted but unshipped controls are `Planned`; intentionally
absent capabilities are `Prohibited`. It must never describe a roadmap control
as current protection.

## Existing evidence

- `docs/research/remora-link-threat-model.md` is the release-blocking model for
  the implemented v2 mobile/Host contract.
- `docs/research/pairing-v2-security.md` records implemented pairing invariants
  and verification.
- `CONTEXT.md` records current runtime ownership and opaque-wake behavior.
- The accepted greenfield rebuild supplies the future command-center security
  requirements; it does not prove their implementation.

## Scope

**In scope**:

- `docs/security/remora-threat-model.md`
- `README.md`
- `CONTEXT.md`

**Out of scope**:

- Source, tests, CI, infrastructure, runtime dependencies, protocols, grants,
  routes, generated bindings, or security implementation.
- Rewriting or deleting either Link-specific research document.
- Publishing secrets, real endpoints, recovery material, exploit recipes, or
  environment-specific offensive detail.

## Git workflow

- Create a fresh isolated worktree from
  `58d2e8d812ef8039cd69789a96d5427e93226fa2`.
- One commit after all validation passes.
- Commit subject: `docs: add command-center threat model`.
- Do not merge, push, or open a pull request.

## Steps

### Step 1: Create the canonical threat model

Create `docs/security/remora-threat-model.md` with these exact top-level
sections:

1. `# Remora command-center threat model`
2. `## Status and use`
3. `## Security objectives`
4. `## Locked assumptions and non-goals`
5. `## System and trust boundaries`
6. `## Protected assets and data classes`
7. `## Attacker model`
8. `## Control lifecycle`
9. `## Threat and control matrix`
10. `## Privacy boundaries`
11. `## Availability and resource exhaustion`
12. `## Release and update integrity`
13. `## Validation and release gates`
14. `## Severity calibration`
15. `## Residual risks`
16. `## Supporting evidence`

Keep the model concrete, defensive, and testable. Use one status vocabulary:

- `Implemented`: directly evidenced in the current repository and the pinned
  Link contract;
- `Planned`: required before the associated new feature can ship;
- `Prohibited`: deliberately unavailable and not emulated.

Do not use `Implemented` for the device SQLite database, rich awareness API,
browser controller/CDP, worktrees/checkpoints, managed DigitalOcean lifecycle,
passkey/recovery administration, signed Link updates, or release promotion.

### Step 2: Cover every accepted threat boundary

The threat/control matrix must separately cover, at minimum:

- public relay and future admin HTTPS entrypoints;
- mobile device compromise and stolen pairing material;
- malicious or compromised Host;
- ChatGPT OAuth redirect, token, account-binding, and credential-custody
  boundaries;
- WebRTC signaling, transcript, microphone, and audio-privacy boundaries;
- direct remote app-server identity, transport, and authorization boundaries;
- SSH server identity, host-key, credential, forwarding, and terminal-stream
  boundaries;
- hostile repository contents and provider prompt/tool attacks;
- command/argument injection, path traversal, and symlink escape;
- browser-origin confusion and CDP abuse;
- system-surface privacy and opaque route handles;
- release artifact substitution and protocol skew;
- managed-cloud/control-plane compromise;
- recovery-key theft and passkey revocation;
- denial of service, unbounded payloads, and storage exhaustion.

For each row include: asset/impact, lifecycle status, required control, concrete
verification, and residual limitation. Use bounded descriptions, not exploit
instructions.

Record these locked product assumptions explicitly:

- one owner; no multi-tenancy, billing, or organization policy machinery;
- public relay/admin HTTPS, with Hosts outbound-only;
- source, prompts, credentials, terminal data, and files are sensitive;
- work-state relay traffic stays end-to-end encrypted;
- the existing opaque wake payload remains content-blind and byte-compatible;
- rich awareness is a separate explicit privacy relaxation and generic normal
  notifications remain the default;
- sensitive decisions happen only in-app;
- a fully compromised unlocked device or Host is an explicit residual limit.

### Step 3: Map controls without claiming future implementation

The control-lifecycle section must have separate `Implemented`, `Planned`, and
`Prohibited` tables or lists. It must include:

- current: scoped Link grants, P-256 device proof, epochs, E2E relay state,
  opaque wake hints, platform-backed signing-key and credential storage,
  host-key verification, Rust-owned reconciliation, bounded pairing frames,
  and in-app approval decisions. Do not generalize this to encrypted device
  work-state storage;
- planned: XChaCha20-Poly1305 record encryption, HMAC search postings, passkey
  user verification, offline recovery enrollment/revocation, short sessions,
  one-time websocket tickets, workspace confinement, argv allowlists, browser
  sandbox/origin validation, awareness schema separation, signed manifests,
  side-by-side Link update/rollback, support-bundle preview/redaction, and
  deterministic payload/storage/query budgets;
- prohibited: arbitrary Host proxying, direct mobile source editing, dangerous
  Git history operations, provider fallback/emulation, credentials in URLs,
  sensitive lock-screen actions, and unbounded public lists/strings.

Link to the two existing research documents as narrower supporting evidence,
not replaced or superseded records.

For current OAuth, record Plans 002 and 005's cross-account refresh and bounded
loopback callback controls as required remediation, not `Implemented`. Existing
PKCE/state/account custody does not erase those open availability and
identity-binding gaps. General SQLite/work-record encryption remains `Planned`.

### Step 4: Calibrate severity

Add `## Severity calibration` with repository-grounded Critical, High, Medium,
and Low definitions/examples. Calibrate by required preconditions, scope of
credential/work-content exposure, cross-boundary authority gained, persistence,
and user recovery. Include out-of-scope attacker stories for a fully
compromised unlocked mobile OS and a Host account able to replace Remora Link,
while preserving their residual-risk implications. Avoid exploit procedures.

### Step 5: Correct README and context direction, then link the model

In `README.md`:

- add a concise `Security` section linking
  `docs/security/remora-threat-model.md` and the narrower Link/pairing records;
- state that the threat model distinguishes implemented controls from release
  requirements for planned command-center features;

In both `README.md` and `CONTEXT.md`:

- correct permanent-out-of-scope wording for managed deployment, Live
  Activities, and release automation. Describe those as roadmap work not yet
  implemented in this checkout;
- keep Watch, complications, CarPlay, Fastlane, and store-feedback triage
  excluded;
- retain the current honest statement that the checkout contains native mobile,
  shared runtime, developer tooling, and the self-hostable relay foundation.

Do not market future features as shipped.

### Step 6: Validate and commit

Run from the repository root:

```sh
(
  set -euo pipefail
  PLAN_MODEL='docs/security/remora-threat-model.md'
  test -f "$PLAN_MODEL"
  for heading in \
    '# Remora command-center threat model' \
    '## Status and use' \
    '## Security objectives' \
    '## Locked assumptions and non-goals' \
    '## System and trust boundaries' \
    '## Protected assets and data classes' \
    '## Attacker model' \
    '## Control lifecycle' \
    '## Threat and control matrix' \
    '## Privacy boundaries' \
    '## Availability and resource exhaustion' \
    '## Release and update integrity' \
    '## Validation and release gates' \
    '## Severity calibration' \
    '## Residual risks' \
    '## Supporting evidence'; do
    rg -n -F "$heading" "$PLAN_MODEL"
  done
  for term in \
    'Implemented' 'Planned' 'Prohibited' \
    'public relay' 'future admin HTTPS' \
    'mobile device compromise' 'stolen pairing material' \
    'malicious or compromised Host' 'hostile repository' \
    'ChatGPT OAuth' 'WebRTC' 'direct remote app-server' 'SSH server identity' \
    'provider prompt/tool attacks' \
    'command and argument injection' 'path traversal' 'symlink escape' \
    'browser-origin confusion' 'CDP abuse' 'system-surface privacy' \
    'opaque route handles' 'release artifact substitution' 'protocol skew' \
    'managed-cloud control-plane' 'recovery-key theft' 'passkey revocation' \
    'denial of service' 'unbounded payloads' 'storage exhaustion'; do
    rg -ni -F "$term" "$PLAN_MODEL"
  done
  rg -n -F '../research/remora-link-threat-model.md' "$PLAN_MODEL"
  rg -n -F '../research/pairing-v2-security.md' "$PLAN_MODEL"
  rg -n -F 'docs/security/remora-threat-model.md' README.md
  rg -n -F 'docs/research/remora-link-threat-model.md' README.md
  rg -n -F 'docs/research/pairing-v2-security.md' README.md
  rg -n -F 'command-center roadmap' CONTEXT.md
  git diff --check
)
```

Read the finished document end-to-end and inspect the README diff. Confirm that
every unimplemented feature control is labeled `Planned`, every current claim
has repository evidence, and no sensitive values or exploit-ready environment
details appear.

Run the exact changed-file gate:

```sh
(
  set -euo pipefail
  PLAN_CHANGED_FILES="$(git status --short --ignore-submodules=all --untracked-files=all | cut -c4- | LC_ALL=C sort)"
  PLAN_EXPECTED_CHANGED_FILES="$(printf '%s\n' README.md CONTEXT.md docs/security/remora-threat-model.md | LC_ALL=C sort)"
  test "$PLAN_CHANGED_FILES" = "$PLAN_EXPECTED_CHANGED_FILES"
)
```

Stage only the three files, inspect `git diff --cached`, commit, and verify:

```sh
(
  set -euo pipefail
  PLAN_EXPECTED_COMMIT_FILES="$(printf '%s\n' README.md CONTEXT.md docs/security/remora-threat-model.md | LC_ALL=C sort)"
  test "$(git log -1 --format=%s)" = 'docs: add command-center threat model'
  test "$(git rev-parse HEAD^)" = '58d2e8d812ef8039cd69789a96d5427e93226fa2'
  PLAN_COMMIT_FILES="$(git diff-tree --no-commit-id --name-only -r HEAD | LC_ALL=C sort)"
  test "$PLAN_COMMIT_FILES" = "$PLAN_EXPECTED_COMMIT_FILES"
  test -z "$(git status --short --ignore-submodules=all --untracked-files=all)"
)
```

## Done criteria

- [x] Canonical threat model covers every accepted boundary and threat class.
- [x] Implemented, planned, and prohibited controls are unambiguous.
- [x] Existing Link/pairing records remain intact and linked as evidence.
- [x] README points to the model and no longer calls roadmap work permanently
      out of scope.
- [x] README and CONTEXT agree on roadmap versus excluded surfaces.
- [x] No future feature is described as currently shipped or protective.
- [x] No secret value or exploit-ready environmental detail is published.
- [x] Exactly three scoped documentation files are committed from the approved
      base and the isolated worktree is clean.

## Execution record

- Executor worktree: `/tmp/remora-plan007.Nrpbfc/worktree`.
- Reviewed commit: `6faa9c3e3667a38c5865021baffca1b416c1eb97`.
- The initial documentation commit
  `e34f9d755125a2beb8287eb4ce515646de96988c` was rejected during semantic
  security review and amended before acceptance.
- Exact parent, subject, three-file scope, clean-worktree, heading, terminology,
  link, lifecycle, and `git diff --check` gates passed.
- Independent security re-review approved the current/target split for native
  state, WebView bridges, Android widget disclosure, OAuth, direct remote
  app-server transport, and release-gate semantics.
- Documentation-only change; no build or runtime test was required. No merge,
  push, or pull request was performed.

## STOP conditions

Stop and report without improvising if:

- The drift check reports scoped-file changes after the planned-at commit.
- A current security claim cannot be evidenced in the repository or existing
  Link/pairing records.
- The document requires disclosing a secret, live endpoint, recovery material,
  or exploit-ready environment detail.
- Accurate lifecycle labeling requires a product decision not present in the
  accepted rebuild plan.
- Any file outside the three-file scope, runtime dependency, or generated
  artifact is required.
- Any required static, exact-scope, or post-commit check fails twice after
  correcting only an environment or command typo.

## Rollback

Revert the single executor commit. No runtime, data, protocol, or infrastructure
state changes.
