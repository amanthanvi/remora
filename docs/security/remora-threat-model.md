# Remora command-center threat model

## Status and use

Status: release-governing model for the current Remora mobile, shared-runtime,
Remora Link, SSH, relay, and planned command-center boundaries.

This document distinguishes controls evidenced in the current checkout from
requirements for future work. It does not declare the command-center roadmap
shipped. Every control uses one of three lifecycle labels:

- **Implemented**: directly evidenced by this repository or the pinned Remora
  Link contract.
- **Planned**: required before the associated feature may ship.
- **Prohibited**: deliberately unavailable and not emulated by a fallback.

The lifecycle label is part of each requirement. A design description, issue,
or test plan does not promote a control from Planned to Implemented. This model
must be updated when implementation evidence changes. Architecture statements
describe the architectural target unless a current implementation is named.
The Rust store/reconnect/reducer is canonical today, while native `AppModel`
caches, stream/projection merges, duplicate provider inference, and native OAuth
remain transitional. Full thin-shell convergence is Planned.

## Security objectives

- Keep source, prompts, credentials, terminal data, files, transcripts, tool
  results, and approvals confidential across mobile, Host, relay, and provider
  boundaries.
- Bind every privileged operation to the intended owner, device, Host,
  workspace, account, runtime, and current authorization epoch.
- Make durable Rust-owned state authoritative after reconnect, replay, wake,
  duplication, loss, or process restart.
- Prevent delivery infrastructure, UI routes, repository content, providers,
  browser content, or release infrastructure from creating authority.
- Bound public inputs, stored data, queries, streams, and recovery operations so
  resource exhaustion fails closed and remains recoverable.
- Preserve reviewable release provenance and a tested rollback path before
  managed distribution or automated promotion.

## Locked assumptions and non-goals

- Remora has one owner. Multi-tenancy, billing, and organization policy are not
  product requirements.
- The public relay and future admin HTTPS surface are internet reachable. Hosts
  are outbound-only and do not require public inbound ports.
- Source, prompts, credentials, terminal streams, files, tool results, and
  transcripts are sensitive data.
- Work-state relay traffic is end-to-end encrypted. The relay may observe
  delivery metadata and deny service, but must not receive work-state plaintext.
- The existing opaque wake payload remains content-blind and byte-compatible.
  It is only a reconciliation hint, never a source of truth or approval surface.
- Opaque wake does not describe every current system surface. The Android
  active-turn home widget is limited to bounded status and count data; it does
  not render prompts, model labels, context metrics, paths, or tool details.
- Rich awareness is a separate, explicit privacy relaxation. Generic normal
  notifications remain the default.
- Sensitive decisions, including approvals, grants, recovery, and account
  changes, happen in-app only.
- A fully compromised unlocked device or a malicious or compromised Host is a
  residual limit. This model reduces exposure and recovery cost; it cannot make
  an attacker-controlled endpoint trustworthy.
- Arbitrary Host proxying, direct mobile source editing, dangerous Git history
  operations, and provider fallback/emulation are not command-center goals.

## System and trust boundaries

| Boundary | Security property | Lifecycle |
| --- | --- | --- |
| Mobile UI and platform adapters to shared Rust | Implemented: the Rust store, reconnect, reducer, typed protocol, and authoritative reconciliation. Planned: remove transitional native `AppModel` caches, stream/projection merges, duplicate provider inference, and shared policy from Swift/Kotlin. | Implemented / Planned |
| Mobile to Remora Link Host | Pinned Host identity, scoped grant, fresh P-256 device proof, exact epoch, typed runtime, and bounded pairing frames. | Implemented |
| Client or Host to public relay | Work-state payloads remain end-to-end encrypted; relay metadata grants no authority. | Implemented |
| Owner to future admin HTTPS | Passkey verification, short sessions, one-time websocket tickets, CSRF/origin checks, and auditable changes are release requirements. | Planned |
| Native mobile to ChatGPT OAuth | Native PKCE, state validation, platform token custody, cross-account refresh rejection, refresh-token preservation, and a loopback-only bounded iOS callback listener. | Implemented |
| Mobile to direct remote app-server | Raw direct sockets are fail-closed to secret-free loopback `ws://`/`wss://` endpoints. Non-loopback work must use authenticated Remora Link or host-key-verified SSH. URL credentials, queries, and fragments are rejected. | Implemented |
| Mobile to SSH server | Host-key verification, protected credentials, encrypted transport, and explicit terminal forwarding policy. | Implemented |
| Native WebRTC peer to signaling and transcript state | Platform microphone consent and native media processing are current; signaling-identity and transcript-retention release controls are future requirements. | Implemented / Planned |
| Retired generated-HTML actions and native WebView bridges | Generated-HTML actions, Saved Apps, their WebViews, script bridges, structured-response bridge, dynamic registration, persistence, and navigation routes are removed from Rust, iOS, and Android. | Implemented |
| Command center to repository, Git, tools, providers, and browser | Untrusted content never becomes authority; paths, argv, origins, and provider capabilities are constrained before use. | Planned |
| Build and release systems to installed clients and Hosts | Reviewed source, signed artifacts and manifests, protocol compatibility, promotion evidence, and rollback preserve provenance. | Planned |
| Mobile to system surfaces | Implemented: opaque wake hints trigger reconciliation and the Android widget exposes only bounded status/count data. Planned command-center surfaces require expiring route handles and a separate consented rich-awareness schema. | Implemented / Planned |

Trust is directional. A valid transport does not make remote content safe, a
valid route does not authorize an action, and a signed artifact does not prove
that its behavior satisfies this model.

## Protected assets and data classes

| Class | Examples | Handling requirement |
| --- | --- | --- |
| Authority | Device signing keys, pairing material, credential IDs, authorization epochs, passkeys, recovery keys, sessions, websocket tickets | Platform-backed custody where available; never in URLs, logs, notification text, or support bundles. |
| Work content | Repositories, diffs, prompts, responses, files, terminal streams, commands, tool output, transcripts | End-to-end protection in transit; encrypted device records are Planned and must not be implied by secure credential storage. |
| Identity and account | ChatGPT OAuth tokens, account binding, provider identity, saved server and Host identity | Explicit audience/account binding, least retention, revocation, and in-app changes. |
| Routing and metadata | Relay route IDs, opaque route handles, cursors, timestamps, online status, device and Host labels | Minimize, expire, and never treat as authorization; metadata leakage remains a residual risk. |
| Operational evidence | Audit records, validation reports, update manifests, crash and support bundles | Redact by default, bound retention, preview before export, and preserve provenance. |

## Attacker model

In scope:

- internet and local-network observers, malicious relays, and compromised
  future admin infrastructure;
- mobile device compromise short of complete control, stolen pairing material,
  copied invitations, backups, logs, and notification observations;
- a malicious or compromised Host, harness, provider, repository, tool result,
  future browser-preview page, or dependency;
- replay, reordering, duplication, response loss, stale UI work, protocol skew,
  artifact substitution, revocation races, and interrupted recovery;
- command and argument injection, path traversal, symlink escape, origin
  confusion, and confused-deputy requests across established boundaries;
- denial of service, unbounded payloads, storage exhaustion, expensive queries,
  connection floods, and repeated authentication or recovery attempts.

Explicit attacker-story limits:

- A fully compromised unlocked mobile OS can invoke accessible user authority,
  observe rendered content, and misuse an authenticated session. Non-exportable
  keys and Planned encrypted records reduce portability and at-rest exposure,
  not live endpoint compromise.
- A compromised Host account able to replace Remora Link can observe Host data
  and impersonate that Host. Device revocation, update provenance, and recovery
  controls limit persistence elsewhere but cannot repair the Host in place.

These limits remain residual risks; they are not permission to weaken
cross-boundary validation or recovery.

## Control lifecycle

### Implemented controls

- Scoped Link grants, fresh P-256 device proof, and authorization epochs bind
  current Remora Link operations to enrolled device authority.
- End-to-end relay state keeps work content opaque to relay infrastructure.
- Opaque wake hints remain content-blind and cause authenticated reconciliation.
- Platform-backed signing-key and credential storage protects the narrow secrets
  placed in Keychain, Secure Enclave where available, Android Keystore, or
  encrypted platform preferences.
- SSH host-key verification fails closed for unknown, changed, or unavailable
  trust state.
- Rust-owned reconciliation restores authoritative runtime state after events,
  replay drift, and wakeups.
- The canonical Rust store/reconnect/reducer is implemented even though native
  caches, streaming merges, projection merges, and provider inference remain.
- Bounded pairing frames reject oversized Remora Link control input.
- Durable send-message intent receipts bind an originating credential, Host
  Thread, and request fingerprint before dispatch. Replays after the dispatch
  fence return an explicit unknown outcome and never authorize another send;
  prompts and message payloads do not cross this control API. Older Hosts
  negotiate explicit unavailability through the authenticated operation rather
  than a provider-name or protocol-version guess; current Host conflicts use a
  distinct terminal code and cannot masquerade as missing support.
- Approval decisions are presented and submitted in-app; wake and lock-screen
  surfaces cannot decide them.
- Native ChatGPT OAuth currently implements PKCE, state validation, and
  platform-backed token custody.
- Raw direct app-server sockets accept only secret-free loopback WebSocket
  endpoints. Non-loopback endpoints fail closed with guidance to use Remora
  Link or SSH; credentials, query strings, and fragments are rejected.

The device SQLite foundation encrypts outbox payloads, search documents, and
review-note bodies per record with a device-only Keychain/Keystore master key;
HMAC exact/prefix postings avoid plaintext search terms. It is a cache, not
authority for live Host/provider state. Durable Host/provider Thread mapping,
the authoritative outbox delivery worker, delivered organization/review
workflows, rich awareness APIs, browser controller/CDP integration,
worktrees/checkpoints, managed DigitalOcean lifecycle, passkey/recovery
administration, signed Link updates, and release promotion are not yet
implemented.

Generated-HTML/WebView surfaces are removed, and the Android widget is
constrained to status/count projection data.

### Planned controls

- Authoritative outbox delivery through durable Host/provider Thread mapping,
  with history reconciliation for the crash window after the dispatch fence.
- Passkey user verification, offline recovery enrollment/revocation, short
  sessions, and one-time websocket tickets.
- Workspace confinement, argv allowlists, canonical path checks, and symlink
  containment before repository, Git, or tool operations.
- Browser sandbox/origin validation before any browser controller or CDP access.
- Awareness schema separation so rich awareness cannot silently expand the
  existing opaque wake contract.
- Signed manifests, side-by-side Link update/rollback, protocol compatibility
  checks, and controlled release promotion.
- Support-bundle preview/redaction before any diagnostic export.
- Deterministic payload/storage/query budgets across public, relay, device,
  provider, transcript, terminal, and administrative surfaces.
- Full thin-shell convergence: remove native canonical-state caches and
  stream/projection reconciliation, centralize provider inference, and move
  shared OAuth policy behind the Rust boundary while retaining native browser
  and secure-storage adapters.
### Prohibited controls and behaviors

- Arbitrary Host proxying or mobile-selected executable paths and arguments.
- Direct mobile source editing and dangerous Git history operations.
- Provider fallback/emulation when a selected provider lacks a required
  capability.
- Credentials in URLs, logs, wake payloads, or route handles.
- Sensitive lock-screen actions, including approval, grant, recovery, account,
  and release decisions.
- Unbounded public lists/strings or implicit unlimited retention.

## Threat and control matrix

| Threat boundary | Asset / impact | Lifecycle | Required control | Concrete verification | Residual limitation |
| --- | --- | --- | --- | --- | --- |
| Public relay compromise | Work content and routes; disclosure, tampering, or loss | Implemented | Keep relay work state end-to-end encrypted and make authenticated durable reconciliation authoritative. | Relay ciphertext, payload-shape, replay/drift, and opaque-wake tests. | Relay operators can observe metadata and deny service. |
| Future admin HTTPS compromise | Owner authority and managed Hosts; unauthorized administration | Planned | Require passkey verification, short sessions, one-time websocket tickets, origin checks, and auditable admin changes before admin launch. | Authentication, origin, expiry, replay, revocation, and authorization negative tests. | Admin controls do not repair compromised owner devices or Hosts. |
| Mobile device compromise and stolen pairing material | Device authority and saved work; impersonation or disclosure | Implemented / Planned | Implemented: non-exportable P-256 signing authority, scoped grants, epochs, platform credential storage, and per-record encrypted work cache. Planned: recovery enrollment and broader revocation administration. | Key-provider tests, copied-invitation/replay tests, revoke/forget tests, encrypted-record migration, recovery, and locked-device tests. | A fully compromised unlocked device can invoke available authority and read displayed data. |
| Offline intent replay or ambiguous provider dispatch | Duplicate owner messages, unintended provider work, or silent loss | Implemented / Planned | Implemented: encrypted device intent, credential/Thread/fingerprint-bound Host receipt, durable pre-dispatch fence, monotonic receipt states, and explicit outcome-unknown replay. Planned: durable Host/provider Thread mapping plus authoritative history reconciliation before retry or acknowledgement. | Wrong-fingerprint/credential/Thread, backwards transition, journal/snapshot failure, lost-response, forbidden-content, phase-correlation, and end-to-end reconnect tests. | A crash after the dispatch fence but before provider transmission can require explicit recovery; availability is preferred over a duplicate send. |
| Malicious or compromised Host | Source, terminal, prompts, credentials, runtime authority; exfiltration or false results | Implemented / Planned | Implemented: pinned Link identity, scoped grants/runtime set, and stream closure on epoch change. Planned: workspace confinement and constrained command/provider/browser delegation. | Wrong-Host, narrowed-grant, revoke-stream, workspace escape, and capability-boundary tests. | A Host-account attacker able to replace Link controls that Host's data and behavior. |
| ChatGPT OAuth redirect, token, account-binding, and credential custody failures | Account tokens and identity; account confusion or takeover | Implemented | Native PKCE, state validation, platform token custody, cross-account refresh rejection on both platforms, refresh-token preservation, and loopback-only bounded iOS callbacks. | Wrong-account refresh, state/PKCE, callback interface/port/path, timeout, duplicate-callback, cancellation, refresh omission, and secure-store tests. | A compromised device or provider endpoint can still misuse tokens legitimately available to it. |
| WebRTC signaling, transcript, microphone, and audio privacy | Live audio, transcripts, presence; covert capture or unintended retention | Implemented / Planned | Implemented: platform microphone permission and native media session. Planned: explicit signaling-identity gates, bounded transcript retention, and separation from awareness/export. | Permission-denied, background/end-session, wrong-peer signaling, transcript deletion, log-redaction, and support-bundle tests. | Peers and a compromised endpoint can observe media they legitimately receive; network metadata remains visible. |
| Direct remote app-server identity, transport, and authorization confusion | Sessions, prompts, approvals, account state; wrong-server action | Implemented | Permit raw direct sockets only to secret-free loopback `ws://`/`wss://`; reject URL credentials, queries, fragments, unsupported schemes, and every non-loopback destination. Require Remora Link or host-key-verified SSH for remote Hosts. | Loopback allow tests; non-loopback, scheme, URL-secret, query, and fragment rejection tests; Link and SSH regression suites. | A malicious process on the same device can impersonate a loopback app-server. Remora Link or SSH remains required when Host identity and authorization matter. |
| SSH server identity, host key, credential, forwarding, and terminal stream attacks | SSH credentials and terminal contents; interception or wrong-host execution | Implemented | Verify pinned host keys before authentication, protect credentials, use encrypted SSH, restrict forwarding, and close streams on lifecycle changes. | Unknown/changed/unavailable host-key tests, reconnect tests, credential-store tests, forwarding-policy tests, and terminal cleanup tests. | A trusted SSH account or server can observe commands and terminal data on that server. |
| Retired generated-content WebViews and native bridges | Prompts, navigation, structured requests, app state, and owner intent; generated content formerly exercised native authority | Implemented | Remove generated-HTML tool registration, hydration, WebViews, native bridges, Saved Apps persistence, navigation, and platform routes on both clients. | Cross-repository stale-symbol gate, Rust tests, generated-binding check, native builds, and Android/iOS test suites. | A future browser-preview feature creates a separate Host-side boundary and may not reuse these removed mobile bridges. |
| Hostile repository contents and provider prompt/tool attacks | Owner intent, source, credentials, tool authority; indirect instruction execution | Planned | Treat repository/provider output as untrusted data; enforce typed capability boundaries, explicit approvals, secret redaction, and no provider fallback/emulation. | Adversarial fixture tests for instructions in files/tool output, capability-denial tests, approval tests, and secret-canary scans. | Approved tools may intentionally expose workspace data within their declared capability. |
| Command and argument injection, path traversal, and symlink escape | Filesystem, Git state, Host execution; execution outside intended workspace | Planned | Canonical workspace roots, component-wise path checks, symlink containment, argv allowlists, typed operations, and no shell-string construction. | Metacharacter/argument-boundary, traversal, absolute-path, symlink-race, workspace-root, and denied-operation tests. | Approved commands can still modify data within their allowed workspace and capability. |
| Browser-origin confusion and CDP abuse | Browser sessions, cookies, page data, local services; cross-origin control | Planned | Sandbox browser automation, pin the intended origin/target, separate profiles, minimize CDP methods, and require explicit in-app initiation. | Wrong-origin/target, navigation race, profile isolation, forbidden-method, local-network target, and teardown tests. | Browser controller/CDP access deliberately exposes the selected page to constrained automation. |
| System-surface privacy, Android home-widget disclosure, and opaque route handles | Notification metadata, bounded work status/counts, navigation intent; lock-screen or launcher disclosure/action | Implemented / Planned | Implemented: byte-compatible opaque wake, authenticated reconciliation, and a sanitized Android widget projection. Planned command-center surfaces use expiring non-authoritative route handles and a separate consented rich-awareness schema. Sensitive decisions remain in-app. | Sanitized projection unit tests, device lock-screen/launcher inspection, payload byte-shape, route expiry/staleness, generic-notification-default, and no-action tests. | Launcher observers can infer bounded activity counts/status; push providers still infer timing and delivery metadata. |
| Release artifact substitution and protocol skew | Installed app/Host integrity and pairing compatibility; malicious binary or unsafe upgrade | Planned | Reproducible reviewed inputs, signed manifests, artifact verification, compatibility gates, side-by-side Link update/rollback, and staged promotion. | Signature/provenance, wrong-artifact, downgrade, skew matrix, interrupted-update, rollback, and clean-install tests. | Signing infrastructure compromise can authorize malicious artifacts until detected and revoked. |
| Managed-cloud control-plane compromise | Host lifecycle, relay/admin authority, provider credentials; fleet takeover or destructive operation | Planned | Least-privilege service identities, outbound-only Hosts, separated secrets, approval/audit gates, bounded lifecycle operations, and recovery drills. | Role-denial, credential rotation, audit integrity, tenant-absence assumptions, failed-provision, and disaster-recovery tests. | The single owner remains a concentration of authority; a control-plane outage can deny service. |
| Recovery-key theft and passkey revocation gaps | Account recovery and device enrollment; durable unauthorized access or lockout | Planned | User-verified passkeys, offline recovery enrollment, protected recovery material, explicit revocation, device inventory, and short-lived sessions. | Stolen/reused recovery-key, revoked-passkey, lost-device, offline restore, concurrent recovery, and session-expiry tests. | Loss of all enrolled authenticators and recovery material may be irrecoverable by design. |
| Denial of service, unbounded payloads, and storage exhaustion | Availability, device storage, relay/admin capacity; crash, cost, or unusable state | Implemented / Planned | Implemented: bounded pairing frames. Planned: deterministic payload/storage/query budgets, pagination, timeouts, rate limits, backpressure, eviction, and recoverable compaction. | Boundary/fuzz tests, large-list/query tests, connection and auth rate tests, disk-full tests, compaction/restart tests, and load budgets. | Attackers with network access can still consume bounded capacity or deny upstream service. |

## Privacy boundaries

The opaque wake contract is Implemented and intentionally narrow: content-blind
bytes identify only enough routing context to trigger authenticated
reconciliation. Any Planned opaque route handles must expire, confer no
authority, and contain no source, prompt, transcript, command, credential,
approval, or account data. Generic normal notifications are the default.

Opaque wake is not the whole current system-surface story. The Android
active-turn home widget uses the integrated sanitized projection and displays
only bounded status/count data. Prompts, model labels, context metrics, paths,
and tool details are forbidden on that surface.

Rich awareness is Planned as a distinct schema, permission, retention, and UI
surface. It may not reuse a protocol change to silently add content to existing
wake payloads. Planned WebRTC privacy gates must keep microphone audio and
transcripts as separate data classes with visible session state, platform
permission, bounded retention, and deletion/export tests. Support bundles
require Planned preview and redaction; credentials and sensitive content are
excluded by default.

## Availability and resource exhaustion

Bounded Remora Link pairing/control frames are Implemented. They are not proof
that all lists, strings, transcripts, terminal output, relay queues, databases,
queries, exports, or public requests are bounded.

Before command-center release, Planned deterministic budgets must specify hard
input limits, pagination, concurrency, timeouts, backpressure, retry ceilings,
storage quotas, retention, compaction, and recovery behavior for each public or
durable surface. Limits must be verified at the boundary, under disk pressure,
after interruption, and after restart. Public endpoints must couple bounded
work with rate control; authentication success must not disable resource limits.

## Release and update integrity

The current Link release boundary relies on reviewed Remora-owned source pinned
to an exact lockfile revision and golden-vector parity. That is Implemented
evidence for the current contract, not signed update or promotion automation.

Managed distribution requires Planned signed manifests, verified artifact
digests and identities, protocol-skew gates, staged promotion, side-by-side Link
installation, health confirmation, automatic rollback, revocation, and an
auditable record of who promoted what. A failed or interrupted update must leave
one known-good Link executable and must not broaden grants. Credentials in URLs
and unsigned fallback artifacts are Prohibited.

## Validation and release gates

A new roadmap feature cannot ship while any control required for that feature
remains Planned. Existing feature exposure labeled Planned does not mean the
feature is absent. Promotion to Implemented requires integrated code evidence,
deterministic tests, platform validation, and an updated residual-risk
statement; an isolated commit is not integrated evidence.

Minimum security gates:

- exact Remora Link revision review, golden-vector parity, malformed/bounded
  frame tests, grant narrowing, revocation, reconnect, and terminal cleanup;
- Rust tests and binding regeneration plus iOS and Android unit/build gates for
  every shared boundary change;
- OAuth account-binding, refresh-token preservation, and bounded loopback
  regression coverage before relying on the affected account flow;
- negative authorization, wrong-identity, replay, origin, path, argv, symlink,
  provider-capability, and sensitive-system-surface tests as those features land;
- encrypted-record migration/recovery tests before persistent work records are
  treated as protected at rest;
- deterministic payload/storage/query budgets and disk-full/load validation;
- signed artifact, protocol-skew, interrupted-update, rollback, and promotion
  evidence before managed release automation;
- log, notification, route, transcript, and support-bundle secret/canary scans;
- interactive in-app verification for approvals, consent, recovery, account
  changes, and other sensitive decisions.

## Severity calibration

Severity is based on exploit preconditions, exposed scope, authority crossed,
persistence, and recovery—not on the component name alone.

| Severity | Repository-grounded calibration and examples |
| --- | --- |
| Critical | Low-precondition compromise crossing a major authority boundary with broad persistent impact and difficult recovery: release artifact substitution reaching normal installs, recovery bypass granting owner authority, or remote admin compromise that can control Hosts without owner verification. |
| High | Compromise of sensitive work or privileged action across a device, Host, account, or workspace, usually requiring a reachable feature or stolen scoped material: OAuth account confusion, wrong-server approval, workspace escape, durable stolen pairing authority, or browser control of an unintended origin. |
| Medium | Bounded disclosure, integrity loss, or repeatable denial requiring meaningful preconditions and having clear recovery: metadata leakage beyond the contract, one-workspace provider overreach, storage exhaustion recoverable by compaction, or protocol skew blocked by reinstall/rollback. |
| Low | Limited defense-in-depth or observability weakness with no demonstrated authority crossing: overly descriptive non-sensitive error text, missing audit detail, or a bounded local availability defect with straightforward recovery. |

A fully compromised unlocked mobile OS and a Host account able to replace
Remora Link are out-of-scope attacker stories for prevention claims. Their
residual implications remain in scope: revoke other devices, protect recovery,
bound session lifetime, preserve release provenance, limit lateral authority,
and document that endpoint contents may already be exposed. This calibration
does not include operational exploit procedures.

## Residual risks

- A fully compromised unlocked device can use non-exportable keys while the
  device remains authorized and can read content available to the app.
- A malicious or compromised Host can observe and alter its repositories,
  terminal streams, runtime output, and installed Link behavior.
- Relay, push, provider, network, cloud, and release services expose metadata
  and can deny service even when they cannot decrypt work-state traffic.
- A trusted provider, SSH server, remote app-server, browser target, or approved
  tool can misuse data intentionally shared within its granted capability.
- Single-owner recovery concentrates authority; losing every authenticator and
  offline recovery path can cause permanent lockout, while theft before
  revocation can enable access.
- Planned signed-release controls, once implemented, will not prevent defects
  in reviewed code, unsafe owner approvals, compromised build identities, or
  endpoint compromise.

## Supporting evidence

- [Remora Link threat model](../research/remora-link-threat-model.md) is the
  narrower release-blocking model for the implemented v2 mobile and pinned Host
  boundary. It is evidence for those controls, not a replacement for this
  command-center model.
- [Remora Link v2 security architecture](../research/pairing-v2-security.md) is
  the narrower implemented pairing contract and release guardrail record. It is
  evidence for pairing lifecycle claims, not a claim that planned
  command-center controls exist.
- Current source evidence includes the Rust-owned store/reconciliation modules,
  Remora Link v2 bounded-frame and lifecycle tests, platform Link key/credential
  adapters, SSH known-host policy and tests, in-app iOS/Android approval UI, and
  opaque background-relay reconciliation types.
- Current transition evidence includes native iOS/Android `AppModel` caches and
  stream/projection merges, duplicated provider-label inference, and native
  ChatGPT OAuth implementations alongside the canonical Rust store/reconnect/
  reducer.
- Generated-HTML actions, mobile WebViews, native bridges, Saved Apps state,
  and their routes are removed across Rust, iOS, and Android. Future Host-side
  browser preview remains a separate Planned boundary.
- The Android `ActiveTurnWidget` uses an integrated status/count-only
  projection with focused disclosure regression tests.
- Raw direct app-server transport is limited to secret-free loopback
  `ws://`/`wss://`. Remote work uses Remora Link or SSH. Focused policy tests
  cover loopback acceptance and all rejected URL/remote classes.
- The accepted command-center roadmap defines Planned requirements. Planning
  text alone is not implementation evidence.
