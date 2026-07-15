# Remora background relay architecture

**Status:** research recommendation, not an implementation authorization

**Date:** 2026-07-15

**Decision horizon:** first hosted beta plus an optional self-hosted data plane

## Conclusion

Build one Rust/Axum relay around PostgreSQL and a transactional push outbox. Ship the same OCI image, schema, HTTP protocol, crypto envelope, and conformance suite in both modes:

- Remora-hosted: managed container(s), managed PostgreSQL, and an isolated APNs/FCM dispatch service.
- Self-hosted: the same container and PostgreSQL in Docker Compose. A stock App Store/Play build uses a narrow Remora push gateway that knows only an opaque push handle and delivery metadata; a custom-signed client can provide its own APNs/FCM credentials and bypass that gateway.

Do **not** make push canonical state. APNs and FCM are lossy, unordered wake or display hints. The relay durably stores end-to-end encrypted semantic events, while the mobile Rust layer decrypts, deduplicates, detects gaps, and reconciles with the authoritative host. A notification can say “attention needed” or wake a fetch; it must never be the only copy of an approval, completion, or state transition.

Start without Redis. PostgreSQL can atomically allocate a per-channel sequence, insert the encrypted event, and enqueue its push intent in one transaction. Add Redis only if production measurements identify a specific database-backed rate-limit or worker-wakeup bottleneck. The local synthetic benchmark in this research sustained 10,728 event transactions/s with no sequence gaps or event/outbox divergence, far above the initial workload model; that is evidence for simplicity, not a production capacity guarantee.

Do not choose Cloudflare Durable Objects for the first implementation. Durable Objects are an excellent hosted execution model, but their storage, queues, and runtime require a second implementation for optional self-hosting. That creates the permanent dual logic this design is intended to avoid.

Live Activities are a later presentation layer, not the relay foundation. On iOS, use complete, low-sensitivity snapshots and normal notification fallback. On Android, start with an ordinary ongoing notification; Android Live Update promotion is capability- and policy-gated because Google explicitly excludes chat messages and ordinary alerts from appropriate Live Update use.

One prerequisite is organizational rather than technical: [the current product boundary](../../CONTEXT.md) explicitly excludes hosted push/proxy infrastructure and Live Activities. Implementation should not begin until that boundary is intentionally changed.

## Decision in one view

```text
Codex host / Remora connector
  semantic event -> HPKE envelope per installation
       |
       | write capability, epoch + sequence + random event ID
       v
Rust/Axum relay ---------------- PostgreSQL
  ingest/fetch/ack                 event + outbox in one transaction
       |                                      |
       |                                      v
       |                             provider dispatch worker
       |                                      |
       |                         opaque handle, class, TTL,
       |                         generic template / small ciphertext
       |                                      v
       |                         Remora push gateway or custom provider
       |                                      |
       |                                  APNs / FCM
       |                                      |
       +--------------------------------------v
                                  iOS / Android adapter
                                    wake or display hint
                                           |
                                           v
                               shared Rust fetch/decrypt/dedupe
                                           |
                                           v
                              authoritative host reconciliation
```

The provider path can be unavailable without losing an event. The client can receive the same push more than once without applying the same event twice. A collapsed or missing push only delays discovery until the next fetch or foreground reconciliation.

## Scope and non-goals

The relay should provide:

- durable, bounded storage of opaque per-installation event envelopes;
- monotonic sequence allocation, idempotent ingest, ordered fetch, acknowledgement, expiry, and revocation;
- APNs and FCM delivery through isolated provider-token infrastructure;
- semantic notification policy for approvals, completion/failure, security/account attention, sync invalidation, and optional activity projection;
- one hosted/self-hosted protocol and executable;
- metadata-minimized observability, quotas, and abuse controls.

It should not:

- store prompts, responses, raw model-token deltas, SSH credentials, ChatGPT OAuth material, or authoritative thread state;
- keep an iOS or Android SSH/WebSocket connection alive in the background;
- execute approvals or other state changes from notification content alone;
- promise exactly-once push delivery, strict APNs/FCM ordering, or an operating-system delivery SLA;
- turn into a generic webhook, arbitrary-notification, or outbound-proxy service;
- make Live Activities or Android Live Updates a prerequisite for correct background behavior.

## Repository fit

The recommendation follows the current Remora architecture rather than adding a parallel native state machine:

- [CONTEXT.md](../../CONTEXT.md) and the repository guidelines place session state, event normalization, reconciliation, and cross-platform policy in shared Rust. Swift and Kotlin own UI, permissions, platform persistence, and native OS APIs.
- iOS already routes lifecycle work through [RemoraApp.swift](../../apps/ios/Sources/Remora/RemoraApp.swift) and [AppRuntimeController.swift](../../apps/ios/Sources/Remora/Models/AppRuntimeController.swift). Its current [Info.plist](../../apps/ios/Sources/Remora/Info.plist) declares only the audio background mode, so remote-notification support would be an explicit future project change.
- Android already performs reconnect and authoritative refresh in [AppLifecycleController.kt](../../apps/android/app/src/main/java/com/remora/android/state/AppLifecycleController.kt). Its current [manifest](../../apps/android/app/src/main/AndroidManifest.xml) has no FCM service or notification permission declaration.
- Both mobile clients already subscribe to typed shared-Rust updates in [AppModel.swift](../../apps/ios/Sources/Remora/Models/AppModel.swift) and [AppModel.kt](../../apps/android/app/src/main/java/com/remora/android/state/AppModel.kt). The underlying typed broadcast originates in [session/connection.rs](../../shared/rust-bridge/codex-mobile-client/src/session/connection.rs).
- The current credential stores are suitable patterns, not keys to reuse. iOS uses Keychain protection in [AlleycatCredentialStore.swift](../../apps/ios/Sources/Remora/Models/AlleycatCredentialStore.swift); Android uses encrypted preferences in [EncryptedPrefs.kt](../../apps/android/app/src/main/java/com/remora/android/state/EncryptedPrefs.kt) and [AlleycatCredentialStore.kt](../../apps/android/app/src/main/java/com/remora/android/state/AlleycatCredentialStore.kt). Relay encryption needs its own per-installation key material and lifecycle.
- The pairing research in [design-pairing-flexible.md](design-pairing-flexible.md) already anticipates an opaque notification ingress and small background projections. The relay should plug into that model: generic envelope/cursor handling remains shared; pair-specific notifications can dispatch into `RemoteHostPairing.ingest` rather than adding Swift/Kotlin wire parsing.

## Hypothesis, experiment, measurement, selection loop

The research used a small hypothesis-driven loop instead of selecting a hosting vendor first.

| Hypothesis | Experiment or evidence | Measurement | Selection |
|---|---|---|---|
| Push can carry canonical events | Compare APNs and FCM guarantees with Remora correctness needs | APNs is best effort and may coalesce; FCM does not guarantee order and can discard queued messages | Reject. Store encrypted events durably; push only invalidates or displays |
| A background socket can replace a relay | Check iOS background push/runtime and Android Doze/background limits | iOS background pushes are throttled and grant about 30 seconds; Android normal delivery is delayed in Doze and foreground services are restricted | Reject. Reconnect/fetch opportunistically |
| One implementation can serve hosted and self-hosted | Map runtime/storage APIs for Axum, PostgreSQL, SQLite, and Durable Objects | Axum + PostgreSQL runs unchanged locally and in managed containers; Durable Objects requires Cloudflare-specific storage/runtime adapters | Select Axum + PostgreSQL |
| Redis is needed for ordering or initial throughput | Prototype transactional sequence/event/outbox inserts in PostgreSQL 17 | 10,728 transactions/s, 0 failures, 0 sequence gaps, 0 event/outbox count delta in a local synthetic run | Defer Redis until measured need |
| Rich encrypted content fits safely in push | Serialize representative APNs envelopes at several ciphertext sizes | 1 KiB ciphertext -> 1,629-byte JSON; 2 KiB -> 2,994 bytes; 3 KiB -> 4,359 bytes and exceeds APNs' 4 KiB limit | Push a hint or at most a <=2 KiB encrypted preview; fetch rich content |
| Live surfaces can be the universal parity layer | Compare ActivityKit and Android Live Update eligibility | ActivityKit is iOS-specific and time-bounded; Android says chat and alerts are inappropriate Live Updates | Use a shared activity projection with conservative platform renderers and standard-notification fallback |
| Provider-side plaintext is necessary | Apply HPKE, capabilities, opaque handles, and generic local templates | Routing can work with stable metadata while content remains host-to-device encrypted | Select opaque envelope plus explicit metadata minimization |

### Disposable PostgreSQL experiment

The benchmark ran PostgreSQL 17.7 in Docker on local Apple Silicon. A `pgbench` custom transaction, with 16 clients and four threads for 15 seconds, did three operations atomically:

1. increment the selected channel's sequence head;
2. insert a 1 KiB ciphertext event with primary key `(channel_id, epoch, seq)` and unique key `(channel_id, event_id)`;
3. insert the corresponding push-outbox row.

It completed 160,918 transactions at 10,727.6 transactions/s and 1.491 ms average latency, with no failures. Ten thousand channels were selected randomly. The event relation used 198 MB and the outbox 20 MB, or approximately 1.42 KiB/event including the measured indexes and outbox row. No channel had a sequence gap, and event/outbox counts matched exactly.

This is a schema sanity check, not a cloud benchmark or SLA. It omits network latency, multi-AZ commit latency, encryption work, push-provider latency, replicas, backups, and realistic contention distributions. It demonstrates that the initial design does not need Redis merely to allocate sequences or wake outbox workers.

## Platform constraints that shape the design

### APNs and iOS

APNs is an HTTP/2 provider service with a 4,096-byte uncompressed payload limit for ordinary remote notifications. Apple describes delivery as best effort, may store only one notification for an app/device while the device is offline, and provides collapse and expiration controls. A successful provider response is therefore not an application-level acknowledgement. See Apple's [APNs request](https://developer.apple.com/documentation/usernotifications/sending-notification-requests-to-apns) and [response handling](https://developer.apple.com/documentation/usernotifications/handling-notification-responses-from-apns) guidance.

Background notifications are low priority, not guaranteed, can be throttled, and give the app only a short execution window. Apple advises not sending more than roughly two or three background notifications per hour and notes that a force-quit app does not receive them until relaunched. They are suitable for a coalesced “new data may exist” signal, not one signal per event. See [Pushing background updates to your app](https://developer.apple.com/documentation/usernotifications/pushing-background-updates-to-your-app).

Device tokens are app-device-environment specific and can change. The client should request the current token on every launch and forward changes securely. The provider must delete a token after APNs reports it unregistered; retries for 429/5xx responses need bounded exponential backoff and jitter. See [Registering your app with APNs](https://developer.apple.com/documentation/usernotifications/registering-your-app-with-apns) and [Handling notification responses](https://developer.apple.com/documentation/usernotifications/handling-notification-responses-from-apns).

APNs token authentication puts an ES256 `.p8` signing key in the provider trust boundary. Apple says provider tokens older than one hour are rejected and recommends refreshing them during that window. The signing key must never be distributed in the app or a general self-hosted relay image. See [Establishing a token-based connection to APNs](https://developer.apple.com/documentation/usernotifications/establishing-a-token-based-connection-to-apns).

Apple warns against putting sensitive customer data in a notification payload unless it is encrypted. A Notification Service Extension can decrypt and replace visible alert content, but has a short execution budget and falls back to the original alert on failure. The safe default is a generic localized alert; encrypted previews should be opt-in and retain a generic fallback. See [Generating a remote notification](https://developer.apple.com/documentation/usernotifications/generating-a-remote-notification) and [Modifying content in newly delivered notifications](https://developer.apple.com/documentation/usernotifications/modifying-content-in-newly-delivered-notifications).

ActivityKit supports token-addressed remote updates and push-to-start on supported OS versions. Updates should carry complete current content state, a timestamp, a stale date, and an explicit end event; they should not be incremental model deltas. Apple documents a normal Live Activity as active for up to eight hours, with up to four additional hours on the Lock Screen. High-priority updates consume a system budget, so priority 5 should be the default and priority 10 reserved for rare user-visible transitions. See [Starting and updating Live Activities with ActivityKit push notifications](https://developer.apple.com/documentation/activitykit/starting-and-updating-live-activities-with-activitykit-push-notifications) and [Displaying live data with Live Activities](https://developer.apple.com/documentation/activitykit/displaying-live-data-with-live-activities).

Broadcast channels are not a fit for private Remora sessions: they cannot start a Live Activity and would enlarge the privacy/authorization domain. Use a per-activity token if this feature is approved. Apple's [channel management](https://developer.apple.com/documentation/usernotifications/sending-channel-management-requests-to-apns) documentation describes the broadcast model.

### FCM and Android

FCM normal-priority messages may be delayed during Doze. High priority can wake a device for a limited processing window, but Google expects it to produce a user-visible notification; repeated high-priority messages that do not do so can be deprioritized or proxied based on seven days of per-installation behavior. Longer follow-up work should be scheduled immediately through expedited WorkManager. See [Set and manage Android message priority](https://firebase.google.com/docs/cloud-messaging/android-message-priority).

FCM does not guarantee message ordering. It allows no more than four collapse keys per device, and can discard queued non-collapsible messages after its pending-message limit. That supports four stable semantic collapse classes rather than a collapse key per thread. See [Collapsible message types](https://firebase.google.com/docs/cloud-messaging/customize-messages/collapsible-message-types) and [message lifespan](https://firebase.google.com/docs/cloud-messaging/customize-messages/setting-message-lifespan).

`onMessageReceived` has a short execution window. A data message can carry a small encrypted hint for app-controlled rendering; an OS-rendered notification message cannot be decrypted by application code before display. Google explicitly recommends application-layer end-to-end encryption for sensitive content because FCM transport encryption is not end-to-end. See [Receive messages in an Android app](https://firebase.google.com/docs/cloud-messaging/android/receive-messages) and [Set up end-to-end encryption](https://firebase.google.com/docs/cloud-messaging/encryption).

FCM HTTP v1 uses OAuth 2 service credentials. An accepted message ID is not a device acknowledgement, and the API does not expose a caller-supplied idempotency key. The relay must tolerate ambiguous retries and client-side duplicates. See [Migrate to the HTTP v1 API](https://firebase.google.com/docs/cloud-messaging/send/v1-api) and the [FCM REST message schema](https://firebase.google.com/docs/reference/fcm/rest/v1/projects.messages).

Registration tokens should be timestamped, refreshed, and pruned. Google recommends treating roughly one month of inactivity as a useful stale-token signal; Android registration tokens expire after 270 days of inactivity, and `UNREGISTERED` responses require deletion. See [Manage FCM registration tokens](https://firebase.google.com/docs/cloud-messaging/manage-tokens) and [FCM error codes](https://firebase.google.com/docs/cloud-messaging/error-codes).

Doze, App Standby, and background-service restrictions rule out an always-on Remora foreground service as the generic answer. Use FCM to wake bounded work, WorkManager for continuation, and the existing foreground lifecycle refresh as the correctness path. See Android's [background tasks overview](https://developer.android.com/develop/background-work/background-tasks), [Doze guidance](https://developer.android.com/training/monitoring-device-state/doze-standby), and [foreground-service start restrictions](https://developer.android.com/develop/background-work/services/fgs/restrictions-bg-start).

Android Live Updates are promoted ongoing notifications. They require the promotion permission, suitable styles and channels, and an ongoing, user-initiated, time-sensitive activity with a clear start and end. Google's current guidance lists chat messages and ordinary alerts as inappropriate. A long user-initiated Codex turn might eventually qualify, but Remora should first render the shared activity projection as a normal ongoing notification and promote it only after product-policy and device testing. See [Create a Live Update notification](https://developer.android.com/develop/ui/views/notifications/live-update).

## Architecture comparison

Scores are 1 (poor) through 5 (strong). They are a structured decision aid, not measured precision.

| Criterion | Weight | Rust/Axum + SQLite | Cloudflare Workers + Durable Objects | Managed Axum + PostgreSQL + Redis | Axum + PostgreSQL, no Redis |
|---|---:|---:|---:|---:|---:|
| Ordering and idempotency correctness | 20% | 4 | 5 | 5 | 5 |
| Same hosted/self-hosted implementation | 20% | 5 | 1 | 4 | 5 |
| Opaque routing and token isolation | 15% | 4 | 4 | 5 | 5 |
| Local testability | 10% | 5 | 3 | 4 | 5 |
| Hosted HA and operational maturity | 15% | 2 | 5 | 4 | 4 |
| Low-scale cost | 10% | 5 | 5 | 2 | 3 |
| Geographic placement and latency | 10% | 2 | 5 | 3 | 3 |
| **Weighted result** | **100%** | **3.90** | **3.85** | **4.05** | **4.45** |

### A. Standalone Rust/Axum with SQLite

This is the easiest local and personal self-hosted package: one process, one database file, no external service, and excellent disposable-test ergonomics. SQLite transactions are serializable, WAL permits readers alongside a writer, and upserts can enforce idempotency. See SQLite's [isolation](https://www.sqlite.org/isolation.html), [WAL](https://sqlite.org/wal.html), and [UPSERT](https://sqlite.org/lang_upsert.html) documentation.

The limit is operational, not functional. SQLite allows one writer at a time; WAL assumes processes on one host and is unsuitable for a network filesystem. Multi-replica failover, online maintenance, and regional hosted availability would require new coordination or a database migration. It is reasonable for a developer fixture or a deliberately single-node personal deployment, but choosing it as the canonical storage contract would make the hosted tier diverge.

### B. Cloudflare Workers, Durable Objects, and Queues

Durable Objects provide a globally unique, single-location object with strongly consistent transactional storage and serialized execution, which maps naturally to one object per channel. Cloudflare also supplies global routing and low operational overhead. See [What are Durable Objects?](https://developers.cloudflare.com/durable-objects/concepts/what-are-durable-objects/) and the [Durable Objects rules](https://developers.cloudflare.com/durable-objects/best-practices/rules-of-durable-objects/).

Cloudflare Queues is at-least-once, so the design still needs event IDs, unique operations, and deduplication. A duplicate can be delivered to the consumer. See its [delivery guarantees](https://developers.cloudflare.com/queues/reference/delivery-guarantees/) and [dead-letter queue](https://developers.cloudflare.com/queues/configuration/dead-letter-queues/) guidance.

The decisive drawback is portability. Durable Object storage, alarms, bindings, queue consumers, and deployment are Cloudflare runtime APIs. A self-hosted Axum/PostgreSQL implementation would duplicate the sequence allocator, persistence adapter, outbox behavior, failure recovery, and test harness. Wrangler/Miniflare improves local development but is not the production edge runtime. This option is attractive if Remora deliberately drops self-hosting or accepts a hosted-only control plane; it is not the best answer to the stated one-implementation constraint.

### C. Managed container with PostgreSQL and Redis

Axum, PostgreSQL, and Redis are mature, portable, and locally reproducible. PostgreSQL `INSERT ... ON CONFLICT` can make ingest idempotent, while `FOR UPDATE SKIP LOCKED` supports multiple outbox consumers. See PostgreSQL's [`INSERT`](https://www.postgresql.org/docs/current/sql-insert.html) and [`SELECT`](https://www.postgresql.org/docs/current/sql-select.html) documentation. Redis could provide rate counters, short-lived locks, and worker wakeups.

The problem is that Redis is not needed for the core invariant and can easily create a two-system transaction. If sequence or idempotency state is split into Redis, a crash can advance one store without the other. If Redis is only an optimization, it still adds billing, credentials, backup/failover behavior, and another local service. Keep the interface open to a disposable cache or rate-limit accelerator, but do not require it initially.

### D. Selected: Rust/Axum with PostgreSQL and a transactional outbox

This preserves one code path and one atomic correctness domain. The same migrations and conformance tests run against local Docker, a self-hosted installation, and managed PostgreSQL. PostgreSQL provides unique constraints, row locking, transaction isolation, and `SKIP LOCKED` outbox claiming. Workers can scale independently without moving canonical state to a queue.

Use [`axum`](https://github.com/tokio-rs/axum) for the service and an async PostgreSQL client such as [`sqlx`](https://github.com/launchbadge/sqlx). Package a migration job and relay/worker modes in the same repository and image. The hosted deployment may run multiple process roles, while self-hosting can run one process with the same modules and database schema.

## Selected protocol and storage model

### Identities and capabilities

Use a random, per-installation channel. One host paired with three phones gets three channels and three independent encryption keys. Do not put a user ID, server ID, thread ID, device name, or timestamp in a channel identifier.

Each channel has scoped, revocable capabilities:

- a host **write capability** for event ingest and encrypted snapshot replacement;
- a device **read/ack capability** for ordered fetch and acknowledgement;
- a separate **push handle** representing the device token in the provider gateway.

The relay stores capability hashes, not bearer values. A capability is scoped to one channel and operation set. Pairing transfers the device public encryption key and capabilities over the existing authenticated direct path; the relay never bootstraps trust by itself.

The device generates a relay-specific HPKE keypair, and the host generates a relay-event sender key authenticated by the direct pairing transcript. Do not reuse Remora's iroh identity or any SSH, ChatGPT, or pairing secret. The device must authenticate event origin with HPKE's authenticated mode or a pinned host signature over the envelope; possession of a relay write capability alone is not cryptographic sender authentication. HPKE is standardized in [RFC 9180](https://www.rfc-editor.org/rfc/rfc9180.html), but the RFC explicitly leaves ordering, loss detection, and general replay protection to the application. Use one HPKE context per event and bind the protocol version, channel, recipient and sender key IDs, stream epoch, sequence, event ID, and expiry as associated data.

### Visible and encrypted fields

The relay-visible event header should contain only what storage and delivery require:

| Field | Purpose |
|---|---|
| `protocol_version` | Hard-cutover/version negotiation |
| random `channel_id` | Opaque routing |
| `key_id` | Bounded key rotation |
| random `stream_epoch` | Detect host sequence reset |
| `seq: u64` | Order, gap detection, and acknowledgement |
| random `event_id` | Cross-retry idempotency |
| `expires_at` | Bounded storage and provider TTL |
| `push_class` | One of a small policy-controlled enum |
| `collapse_slot` | One of at most four stable slots |
| padded ciphertext | Host-to-device event or snapshot |

Use a random UUIDv4 or equivalent 128-bit random ID for `event_id`; UUIDv7 would leak an additional creation timestamp that the relay already learns from ingest time. [RFC 9562](https://www.rfc-editor.org/rfc/rfc9562.html) describes the timestamp layout of UUIDv7.

Encrypt the semantic body:

- typed event kind and schema version;
- server/thread/turn identifiers and revision;
- a deep-link target;
- a minimal display-safe projection, if previews are enabled;
- an authoritative-refresh hint or complete activity snapshot;
- key-rotation or reset metadata when applicable.

Pad ciphertext into a small number of buckets, initially 512, 1,024, or 2,048 bytes. Reject larger push previews and store larger encrypted event bodies for fetch. The event-ingest limit can initially be 64 KiB to prevent storage abuse, even though normal semantic events should be far smaller.

### Sequence and idempotency contract

The host owns a monotonic `seq` within a random `stream_epoch` and persists its pending send before transmission.

For each ingest transaction, PostgreSQL should:

1. authenticate and lock the channel head;
2. accept exactly `head + 1`, or recognize an exact retry by `(channel_id, event_id)` and ciphertext hash;
3. reject a reused sequence or event ID with different bytes;
4. insert the encrypted event;
5. insert or update the coalesced provider-outbox intent;
6. advance the head and commit atomically.

A gap returns a conflict containing only the expected sequence. The host retries retained events or starts a new random epoch with an encrypted complete snapshot. PostgreSQL unique constraints on `(channel_id, epoch, seq)` and `(channel_id, event_id)` are the final guardrail; an HTTP `Idempotency-Key` may mirror `event_id` but is not the source of truth.

The device fetches `after_seq`, decrypts in order, ignores a known `event_id` or revision, and acknowledges the highest contiguous sequence. A gap, unknown epoch, expired cursor, or decrypt failure cannot be repaired by guessing. The shared Rust layer requests an encrypted snapshot and/or performs the existing authoritative direct-host reconciliation.

APNs and FCM provider IDs are operational observations only. They never advance the device cursor. An ambiguous provider timeout can be retried because the eventual duplicate carries the same event ID and sequence.

### Transactional outbox

The outbox row is created with the event in the same PostgreSQL transaction. Workers claim ready rows with `FOR UPDATE SKIP LOCKED`, send outside the claim transaction, and record attempt state with a lease. The provider call is necessarily outside the database transaction, so a crash after send but before acknowledgement can produce a duplicate. Client idempotency makes that safe.

Use a dead-letter state after bounded attempts, but retain the event independently. A provider outage must increase outbox age and alerts; it must not reject or delete accepted events. `LISTEN/NOTIFY` may reduce polling latency, but it is only a wake optimization: PostgreSQL delivers notifications after commit and can fold duplicates, so workers must always scan durable outbox state. See PostgreSQL [`NOTIFY`](https://www.postgresql.org/docs/current/sql-notify.html).

### Starting retention defaults

These are proposed beta defaults to validate, not platform requirements:

- delete an event 24 hours after every registered device has acknowledged it;
- hard-delete unacknowledged events after seven days;
- retain the newest encrypted complete snapshot for up to 30 days;
- expire approval notifications at the actual approval deadline, capped at 15 minutes;
- revoke a provider token immediately on APNs/FCM invalid-token responses or logout;
- disable delivery after 30 days without a token refresh or successful app contact, and delete stale mapping records on a separately reviewed schedule no longer than provider requirements justify.

Deletion must cover primary rows, outbox/dead-letter rows, logs, caches, and documented backup expiry. User-visible account/device removal should revoke capabilities immediately even if encrypted backup blocks age out later.

## Notification policy

The server accepts a closed enum, never arbitrary alert text, URLs, sound names, or provider headers.

| Semantic class | APNs | FCM | Initial TTL | Collapse slot | Content |
|---|---|---|---:|---|---|
| Approval/action required | visible alert, priority 10 | high-priority data followed immediately by visible local notification | actual deadline, <=15 min | `attention` | generic template or opt-in encrypted preview |
| Account/security attention | visible alert, priority 10 if urgent | high only when immediately visible | <=1 hour | `attention` | generic template |
| Turn completed/failed | visible if user enabled | normal; high only for a user-selected time-sensitive workflow and visible result | <=24 hours | `completion` | generic or encrypted preview |
| Thread/progress invalidation | background priority 5, coalesced | normal data; expedited work only when justified | <=5 min | `sync` | no plaintext content |
| Activity projection | ActivityKit token, priority 5 by default | ordinary ongoing notification; promotion gated | activity lifetime | `activity` | complete low-sensitivity snapshot |

Four stable slots respect FCM's collapse-key limit. A collapse replaces only the wake/display hint; it never removes the durable events behind that hint.

For Android high priority, the app must post the visible notification within the provided processing window when notification permission and user policy allow it. If it cannot make the event visible, send normal priority instead. For iOS background pushes, coalesce changes and stay well below Apple's throttling guidance.

## Live activity and Android parity

Define one shared Rust `ActivityProjection` with fields such as phase, coarse progress, last-updated time, attention-needed, terminal result, stale-at, and end reason. It must be a complete semantic snapshot, not text streaming or a sequence of token deltas.

On iOS:

- only start an activity after an explicit user action or setting;
- use a per-activity push token and rotate it when ActivityKit reports a change;
- use priority 5 for routine snapshots and priority 10 only for rare transitions;
- always set stale/end behavior and provide a normal notification fallback;
- keep content non-sensitive by default. ActivityKit's required `content-state` travels through APNs. End-to-end encrypted widget rendering is not assumed until a focused prototype proves key access, decode/render behavior, fallback, and battery cost.
- if a Notification Service Extension or widget needs a relay key, place only that relay-specific key in an explicitly shared Keychain access group with after-first-unlock background availability. Failure to unlock or access it must produce the generic fallback, never plaintext storage or reuse of the app's iroh/SSH secrets.

On Android:

- render the same projection as a standard ongoing notification first;
- update it from bounded FCM/WorkManager work, not a permanently running foreground service;
- request Android Live Update promotion only on supported devices and only after confirming the workflow is ongoing, user-initiated, time-sensitive, and acceptable under current platform guidance;
- respect user dismissal/demotion and do not recreate a dismissed promoted update until a new user-initiated activity starts.

This is behavioral parity, not identical chrome. Approval and completion alerts work on both platforms even when neither live surface is available.

## Hosted and self-hosted modes without dual event logic

### Hosted

The hosted service runs the selected relay image against managed PostgreSQL. A provider-dispatch role has access to a token vault and APNs/FCM credentials; ingest and fetch roles do not. Separate service identities and network policy enforce that boundary.

### Self-hosted with the stock mobile app

A stock App Store or Play build cannot safely give arbitrary self-hosted servers Remora's APNs `.p8` key, FCM service credential, bundle/topic authority, or Firebase project authority. Those credentials authorize the developer's entire provider namespace and must remain private.

Use a narrow Remora-operated push gateway:

1. the mobile app registers its APNs/FCM token directly with the gateway and receives a random `push_handle`;
2. pairing gives the handle to the host/self-hosted relay;
3. the self-hosted relay calls the gateway with that handle, a closed push class, collapse slot, TTL, and either no content or a small ciphertext envelope;
4. the gateway maps the handle to the provider token and sends the policy-validated request.

The gateway does not store the event stream, know the self-host relay URL, receive a thread/server identifier, or accept arbitrary text. It will still learn the source IP, handle, timing, size bucket, provider, and push class; that residual metadata must be disclosed. The user can disable gateway delivery and rely on foreground/polling reconciliation.

### Fully independent self-hosting

A custom-signed Remora client can use its owner's bundle/package identity and APNs/FCM project. The same relay image accepts those provider credentials through the isolated dispatch configuration and sends directly. No event protocol, schema, crypto, or mobile reducer fork is required.

The hosted push gateway is a narrow provider adapter used by both hosted and stock self-hosted modes, not a second relay or canonical store. Its API and policy validator should live beside the common provider-dispatch code and share conformance tests.

## Privacy and security posture

### What remains visible

End-to-end encryption hides event content, but not all metadata. Depending on mode, the relay or gateway can observe:

- source and destination IP addresses;
- a stable random channel or push handle;
- ingest and delivery times, frequency, retry pattern, and ciphertext size bucket;
- provider/platform, notification class, TTL, and collapse slot;
- acknowledgement progress and whether an installation is active.

Mitigate this by using random identifiers, per-installation channels, coarse classes, ciphertext padding, coalescing, short retention, content-free logs, and no cross-channel analytics identity by default. Do not claim the service is metadata-blind.

### Token and provider-secret isolation

- Store APNs/FCM device tokens only in a token-vault database or table encrypted with a KMS-managed envelope key. The event database stores only opaque push handles.
- Give only the dispatch service permission to decrypt provider tokens and use APNs/FCM credentials.
- Keep APNs development and production tokens/credentials separate. Bind FCM tokens to the expected Firebase project/sender.
- Redact bearer capabilities, device tokens, ciphertext, provider authorization headers, APNs signing material, and FCM service credentials from logs and traces.
- Restrict dispatch egress to the official APNs and FCM endpoints. The API must not accept a caller-selected destination.
- Rotate provider keys through overlapping workers and revoke compromised keys immediately. Rotate HPKE recipient and host sender keys by key ID, authenticate the new sender key over the existing paired channel, encrypt new events to the current recipient key, and retain old private/pinned keys only until old events are acknowledged or expire.

### Abuse controls

Apply quotas at several independent dimensions: installation, host capability, source IP/prefix, push handle, push class, and provider project. Proposed initial safety limits should be conservative and tuned from beta data:

- bounded channels per paired host/account;
- 64 KiB maximum stored event, <=2 KiB encrypted push preview, and fixed allowed padding buckets;
- strict closed enum for notification class, collapse slot, sound, deep-link shape, and TTL;
- lower rate for `attention`/high-priority messages than for normal ingest;
- APNs silent-push coalescing consistent with Apple's two-to-three-per-hour guidance;
- per-handle circuit breakers, provider `Retry-After`, exponential backoff with jitter, and dead-letter review;
- automatic token deletion on `Unregistered`/`UNREGISTERED` and pruning of inactive mappings;
- no arbitrary external URLs, callback targets, alert text, or provider-specific JSON;
- request-body streaming limits, timeouts, concurrency caps, and storage quotas before expensive crypto/database work.

Apple App Attest and Google Play Integrity can be additional risk signals at stock-client enrollment and sensitive credential rotation. They should not be the sole authorization decision, because legitimate devices may not support or pass them and custom/self-hosted clients need an explicit bypass. Apple documents server challenges, assertions, and counters in [Validating apps that connect to your server](https://developer.apple.com/documentation/devicecheck/validating-apps-that-connect-to-your-server). Google advises treating Play Integrity as one signal in a broader anti-abuse strategy; standard requests support request hashes and replay protection. See the [Play Integrity overview](https://developer.android.com/google/play/integrity/overview) and [standard request guidance](https://developer.android.com/google/play/integrity/standard).

### Operational telemetry without content

Measure:

- ingest accepted, exact duplicate, conflict, gap, rejected size, and auth failure counts;
- sequence allocation latency and database transaction retry count;
- outbox age, attempts, provider response class, `Retry-After`, dead letters, and invalid-token rate;
- push-received-to-fetch and ingest-to-contiguous-ack latency distributions;
- duplicate delivery, cursor gap, reset, decrypt failure, unknown key ID, and authoritative-resync counts;
- active channels, retained encrypted bytes, deletion lag, quota saturation, and per-class send rate;
- generic notification shown, notification permission denied, and activity start/update/end outcome.

Use high-cardinality random identifiers only in short-lived restricted traces when an operator is actively debugging. Normal metrics should aggregate by provider, environment, response class, and software version.

## Failure semantics

| Failure | Required behavior |
|---|---|
| APNs/FCM outage | Accept and retain the encrypted event; age/retry outbox; alert operators; client later fetches |
| Ambiguous provider timeout | Retry with the same event ID/collapse intent; duplicate notification is harmless |
| Push collapsed, delayed, or lost | Device discovers all retained events on next fetch or foreground reconciliation |
| Device offline longer than retention | Relay returns reset-required; shared Rust requests an encrypted snapshot and authoritative host refresh |
| Host loses sequence state | Start a new random stream epoch with an encrypted complete snapshot; never reuse the old epoch |
| Duplicate or reordered fetch response | Apply only the next contiguous sequence; deduplicate event/revision; retain cursor |
| Ciphertext/key mismatch | Do not display guessed content; report generic failure, rotate/re-pair, and reconcile authoritatively |
| Provider token rotates | App registers the new token; gateway atomically swaps mapping; old token is revoked on provider response |
| Capability theft | Per-channel scope and rate limits constrain damage; revoke capability and issue a new channel/key |
| Database failover/serialization abort | Retry the complete ingest transaction; unique constraints preserve idempotency |
| Worker crash after provider send | Lease expires and another worker retries; client deduplication handles the duplicate |
| User disables notifications | Continue encrypted relay fetch on foreground/manual refresh; do not escalate around OS/user policy |

The core correctness SLO is not “every push arrives.” It is:

- no accepted event exists without its durable outbox intent;
- no sequence conflict or gap is silently accepted;
- duplicates do not cause duplicate state transitions;
- missing push never loses canonical state;
- bounded retention and deletion complete as documented.

## Operational cost model

All prices are snapshots as of 2026-07-15 and should be refreshed before a purchase decision.

The workload model assumes six stored semantic events per active installation per day and the benchmark's measured 1.42 KiB/event footprint. It intentionally excludes raw token streaming.

| Active installations | Events/month | Approx. seven-day hot event+outbox storage |
|---:|---:|---:|
| 1,000 | 180,000 | 60 MB |
| 10,000 | 1.8 million | 600 MB |
| 100,000 | 18 million | 6 GB |

Average ingest at 100,000 installations is only about 6.9 events/s; a 100x burst is about 694 events/s. Provider-send volume is at most event volume and should be lower because progress/sync signals are coalesced. The database benchmark provides substantial headroom against this planning workload, but production multi-AZ storage must still be load-tested.

Cost comparison:

- **Cloudflare Durable Objects:** the Workers paid plan has a $5/month minimum with included request, duration, row, and storage allowances; additional Durable Object compute and storage are usage-based. The initial relay likely fits near the minimum, but portability is the larger cost. See [Durable Objects pricing](https://developers.cloudflare.com/durable-objects/platform/pricing/).
- **Managed container + PostgreSQL:** a small app container is inexpensive; a production managed PostgreSQL floor and backups dominate. As a concrete current reference, Fly lists a shared 512 MB machine around $3.32/month in one displayed region, while its smallest Managed Postgres plan starts at $38/month before storage. See [Fly Machines pricing](https://fly.io/docs/about/pricing/) and [Managed Postgres pricing](https://fly.io/docs/mpg/). Railway publishes $20/vCPU-month, $10/GB-month RAM, $0.15/GB-month volume, and $0.05/GB egress, with plan minimums; see [Railway pricing](https://railway.com/pricing) and [usage pricing](https://docs.railway.com/pricing/plans). A production starting range around $40-$100/month is reasonable for one region, managed database/backups, and one or two small service processes, but quotes and HA requirements decide the real number.
- **PostgreSQL + Redis:** add another managed service, credentials, metrics, backup/failover decision, and cross-service traffic. It has no demonstrated initial payoff. Its cost should be evaluated only against a measured bottleneck.
- **Self-hosted:** the same image on existing hardware has little incremental infrastructure spend, but the operator owns availability, database backup/restore, upgrades, TLS, abuse exposure, and incident response. A tiny VM is cheap; operator time is not.
- **Push providers:** FCM is listed as a no-cost Firebase product on the [Firebase pricing page](https://firebase.google.com/pricing). Apple does not publish a per-notification APNs usage line item in the provider documentation reviewed here; confirm current developer-program terms before launch. Both still create relay compute, networking, observability, and support cost.

These estimates exclude engineering, support, incident response, compliance work, cross-region replicas, long-term backups, log ingestion, KMS minimums, and unusually large encrypted snapshots.

## Implementation and validation sequence

### Gate 0: product boundary

- Decide whether hosted push and optional Live Activities are now in Remora's product scope.
- Decide whether stock self-hosted clients may use the metadata-bearing Remora push gateway.
- Update `CONTEXT.md` before production implementation.

### Gate 1: protocol and properties

- Put envelope types, HPKE/AAD rules, sequence/cursor logic, and activity projection in a small shared Rust module.
- Build an in-memory reference relay and property tests for duplicate, loss, reorder, epoch reset, key rotation, expiry, and conflicting idempotency keys.
- Fuzz envelope decoding and size/enum validation. No Swift/Kotlin string parsing.

Exit criterion: generated histories either converge to the same contiguous state or produce an explicit reset; no malformed history is silently accepted.

### Gate 2: local PostgreSQL relay

- Implement Axum ingest/fetch/ack/revoke and transactional outbox against Docker PostgreSQL.
- Run the same conformance suite against in-memory and PostgreSQL implementations.
- Add fault injection around commit, worker lease, send timeout, crash-after-send, database restart, serialization retry, and retention deletion.
- Repeat the benchmark with realistic skew, TLS, encryption, multi-AZ-like latency, and retention cleanup.

Exit criterion: zero event/outbox divergence, zero undetected sequence conflicts, bounded worker recovery, and a documented capacity margin.

### Gate 3: provider and device sandboxes

- Start with fake APNs/FCM adapters that produce success, invalid-token, rate-limit, timeout, and 5xx outcomes.
- Validate APNs sandbox and FCM test devices before production credentials.
- Test iOS foreground/background/force-quit/reboot/low-power states, token rotation, notification permission states, generic fallback, and service-extension timeout.
- Test Android Doze/App Standby, high-priority visible handling, WorkManager continuation, notification permission/channel states, process death, token rotation, and OEM battery restrictions.

Exit criterion: no correctness dependency on push receipt; invalid tokens are removed; provider retries remain bounded; user policy is respected.

### Gate 4: privacy, abuse, and deletion

- Verify database and log inspection cannot recover semantic content.
- Exercise capability theft, replay, channel enumeration, arbitrary-text/URL attempts, oversized bodies, quota exhaustion, and provider amplification.
- Test account/device deletion through primary data, outbox, gateway mapping, logs, caches, and backup-expiry documentation.
- Review App Store privacy disclosures and Android data-safety declarations against observed telemetry. Apple describes the disclosure workflow in [Manage app privacy](https://developer.apple.com/help/app-store-connect/manage-app-information/manage-app-privacy/).

Exit criterion: documented data inventory, enforceable retention, tested revocation, and no generic push/proxy primitive.

### Gate 5: limited beta and self-host parity

- Run one hosted region with conservative quotas and content-free telemetry.
- Publish the same relay image and Compose file for self-hosting.
- Run the identical black-box protocol suite against hosted, Compose, and custom-provider modes.
- Measure p50/p95/p99 ingest-to-provider-acceptance, push-to-ack, duplicate/gap/reset rate, invalid-token rate, database utilization, and cost per active installation.

Exit criterion: provider failures do not lose events, retained storage matches the model, and no permanent hosted/self-hosted behavior branch emerges.

### Gate 6: live surfaces

- Add ActivityKit only after ordinary notification/fetch correctness is stable.
- Prototype encrypted Live Activity content separately; do not make it a launch dependency.
- Evaluate Android Live Update eligibility against the shipping workflow and current policy; retain ordinary ongoing-notification behavior everywhere.

## Revisit triggers

Reconsider the selected architecture if evidence changes one of these assumptions:

- self-hosting is removed, making Durable Objects' portability penalty irrelevant;
- a required global latency SLO cannot be met by regional Axum/PostgreSQL deployments;
- PostgreSQL-backed outbox/rate-limit work consumes a material share of database capacity, justifying Redis or a managed queue;
- single-node self-host simplicity becomes more important than one canonical database contract, justifying a supported SQLite profile;
- notification volume or encrypted snapshot size differs by an order of magnitude from the semantic-event model;
- Apple or Google materially changes background execution, Live Activity/Live Update eligibility, provider credentials, or delivery behavior.

## Primary references

### Apple

- [Sending notification requests to APNs](https://developer.apple.com/documentation/usernotifications/sending-notification-requests-to-apns)
- [Establishing a token-based connection to APNs](https://developer.apple.com/documentation/usernotifications/establishing-a-token-based-connection-to-apns)
- [Handling notification responses from APNs](https://developer.apple.com/documentation/usernotifications/handling-notification-responses-from-apns)
- [Registering your app with APNs](https://developer.apple.com/documentation/usernotifications/registering-your-app-with-apns)
- [Pushing background updates to your app](https://developer.apple.com/documentation/usernotifications/pushing-background-updates-to-your-app)
- [Generating a remote notification](https://developer.apple.com/documentation/usernotifications/generating-a-remote-notification)
- [Starting and updating Live Activities with ActivityKit push notifications](https://developer.apple.com/documentation/activitykit/starting-and-updating-live-activities-with-activitykit-push-notifications)

### Google and Android

- [Set and manage Android message priority](https://firebase.google.com/docs/cloud-messaging/android-message-priority)
- [Collapsible message types](https://firebase.google.com/docs/cloud-messaging/customize-messages/collapsible-message-types)
- [Receive messages in an Android app](https://firebase.google.com/docs/cloud-messaging/android/receive-messages)
- [Set up end-to-end encryption](https://firebase.google.com/docs/cloud-messaging/encryption)
- [Manage registration tokens](https://firebase.google.com/docs/cloud-messaging/manage-tokens)
- [Background tasks overview](https://developer.android.com/develop/background-work/background-tasks)
- [Doze and App Standby](https://developer.android.com/training/monitoring-device-state/doze-standby)
- [Create a Live Update notification](https://developer.android.com/develop/ui/views/notifications/live-update)

### Protocol and implementation

- [RFC 9180: Hybrid Public Key Encryption](https://www.rfc-editor.org/rfc/rfc9180.html)
- [RFC 8030: Generic Event Delivery Using HTTP Push](https://www.rfc-editor.org/rfc/rfc8030.html)
- [RFC 8291: Message Encryption for Web Push](https://www.rfc-editor.org/rfc/rfc8291.html)
- [PostgreSQL transaction isolation](https://www.postgresql.org/docs/current/transaction-iso.html)
- [PostgreSQL `INSERT`](https://www.postgresql.org/docs/current/sql-insert.html)
- [PostgreSQL `SELECT` locking](https://www.postgresql.org/docs/current/sql-select.html)
- [Cloudflare Durable Objects concepts](https://developers.cloudflare.com/durable-objects/concepts/what-are-durable-objects/)
- [Cloudflare Queues delivery guarantees](https://developers.cloudflare.com/queues/reference/delivery-guarantees/)
