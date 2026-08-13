# Plan 016: Encrypted device state

Status: **IN PROGRESS**
Tracker: [#26](https://github.com/amanthanvi/remora/issues/26)
Baseline: Plan 015 reviewed commit
Depends on: Plans 014–015

## Scope and ownership

Rust owns the SQLite cache, record encryption, outbox, delivered organization
projection, search postings, review notes, retention, and recovery. Native code
only supplies a secure-store master key. Dependencies: `rusqlite 0.32.1`
bundled (the newest release compatible with pinned Codex/sqlx's single native
SQLite link) and `chacha20poly1305 0.10.1`.

## Contract and acceptance

Credentials and Host trust never enter SQLite. Bind ciphertext associated data
to schema/type/key/Host. Intent acknowledgement is authoritative and idempotent;
enqueue uses one transaction. Search returns at most 50 results from 200 decrypt
candidates. Test wrong keys, corruption, replay, retention, protected eviction,
offline delivery, and redaction.

Rollback schema and dependency changes together; the greenfield reset may drop
only this new cache. STOP on secret persistence, plaintext work records,
pre-delivery draft sync, or any unbounded scan.

## Progress evidence

Implemented: schema, per-record XChaCha20-Poly1305 with bound associated data,
HMAC exact/prefix postings, bounded search, one-transaction idempotent enqueue,
authoritative acknowledgement removal, protected retention, wrong-key/tamper
failure, monotonic relay-sequenced organization projection, encrypted
review-note CRUD with immutable anchors, 90-day/2 GiB logical search retention,
native device-only key handoff, and Turn-ID-idempotent event-driven
terminal-attention metadata. Sessions summaries are now encrypted and indexed
in batches of at most 500 documents, with two-second coalescing during active
updates and an immediate terminal-state flush. Queries remain bounded to 50
results and 200 decrypted candidates, live Host state wins stale cache state,
and per-Host index freshness is explicit.
The additive v2→v5 layout migrations deliberately preserve the v2 encrypted
record envelope, so queued intents, indexed content, and review notes remain
decryptable. Schema v4 adds Host search-freshness metadata. Schema v5 adds a
20,000-row provider-to-Host Thread binding table whose lookup key is an HMAC
and whose identities are authenticated ciphertext; searchable summary bodies
and outbox payloads remain individually encrypted. Retention
protects pinned Threads, queued intents, open review notes, and explicitly
protected documents. iOS and Android rebuild only the exact disposable cache
files and retain the secure master key.

The pinned Link/Remora contract now supplies the missing at-most-once Host
fence for send-message delivery. Its three authenticated, content-free control
operations expose only opaque intent/Thread IDs and a SHA-256 request
fingerprint; prompts remain in the encrypted provider stream. Shared Rust
validates all thirteen Link v2 operations, rejects cross-phase or cross-intent
receipts, and exports the same typed prepare/begin/complete outcomes to Swift
and Kotlin. A signed `invalid_request` from an older Link becomes explicit
`Unavailable` with bounded Rust-owned guidance; it is never inferred from a
provider name or protocol-version threshold. A current Link uses the distinct
`work_intent_rejected` terminal code for identity/fingerprint/state conflicts,
so those failures cannot be misreported as missing support. A dispatch replay
is explicit `OutcomeUnknown` and never silently reexecutes.

Shared Rust now binds provider Threads to durable Host Thread IDs after an
authoritative start/resume/read, caches only bounded ID mappings, and resolves
them from the Host on a cold delivery pass. Text-only offline intents enqueue
in one encrypted SQLite transaction. The serialized worker pre-hydrates the
provider Thread, checks active-turn state under the existing per-Thread send
lock, crosses the Host dispatch fence only while idle, reuses the canonical
turn-start ambiguity reconciler, completes the Host receipt only after the
authoritative provider acknowledgement, and removes SQLite state only after
Host `Succeeded`. Reconnect and database configuration both schedule bounded
delivery; exponential retry is capped. Post-dispatch uncertainty remains
`OutcomeUnknown` and is never resent automatically.

Both native conversation composers and quick-reply sheets now use one Rust
submission action: connected submissions take the existing live provider path;
disconnected plain-text submissions use the encrypted outbox. The Rust API
receives a typed content classification and rejects images, files, skills, and
plugin context while disconnected, even when a platform request would flatten
a file reference into text. A failed submission restores the complete composer
state. Matching inline banners expose queued counts and terminal
outcome-unknown counts. Recovery offers authoritative refresh and an explicit
destructive confirmation that deletes only the uncertain device copy; later
messages on that Thread remain paused until then. Reconnect drains at most ten
batches of ten intents and both clients perform a short bounded status
reconciliation.

The pinned Improve branch review at
`86f170192d0683121475459df25f0c1fa27c0b1b` identified three closure defects;
all are resolved:

1. Cold launches recover the provider-Thread → Host-Thread binding from the
   encrypted device database by HMAC lookup. The plaintext pair never enters
   SQLite, storage is hard-capped, and no second Host database was added.
2. One coalesced Rust task now queries the earliest deliverable
   `next_attempt_at_ms`, sleeps until that deadline, and wakes early for a new
   enqueue/reconnect trigger. The existing serialized worker remains the only
   delivery authority; native code has no retry timer or service.
3. Explicit AppStore authoritative refresh records a Rust proof bound to the
   current uncertain count. A transaction rejects stale-count discard, and
   both current native presentations keep the destructive control disabled
   until their matching refresh succeeds. Direct resend remains absent.

The offline → queue → cold relaunch → reconnect/lost-response journey is now
covered across executable authority seams: SQLite close/reopen restores the
encrypted binding and Thread-local fence, the retry task advances a stored
deadline without a lifecycle event, Link tests prove an ambiguous dispatch is
never re-executed, provider dispatch requires Host `Execute`, and matching
iOS/Android presentation tests require the fresh count. A physical paired-Host
airplane-mode smoke remains release QA, not an unimplemented safety control.

STOP if any fix stores plaintext provider/Host identity correlation, permits
native retry policy, automatically discards or resends an uncertain intent, or
blocks unrelated Threads behind one Thread's uncertain copy.

Validation at Link `e5cf64ab29ed9798585b4fecb2e31a8785b0a4a3`:
Link format, all-target/all-feature clippy with warnings denied, and the full
locked/frozen workspace suite pass; the focused mobile Link-v2 suite passes
97 tests. Focused outbox persistence, direct-dispatch race, payload, and native
database-configuration tests pass. Rust regressions additionally prove that
live-Host context cannot queue and that outcome-unknown is terminal, blocks
only its own Thread, survives cold relaunch, wakes at its retry deadline, and
is removable only after a matching authoritative-refresh proof. Regenerated
Swift/Kotlin bindings expose the same typed submission, status, discard, bind,
enqueue, and bounded delivery outcomes. Both native clients compile against
that generated boundary; focused iOS simulator and Android unit suites pass.
