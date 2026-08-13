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
The additive v2→v4 layout migrations deliberately preserve the v2 encrypted
record envelope, so queued intents, indexed content, and review notes remain
decryptable. Schema v4 adds only Host search-freshness metadata; searchable
summary bodies and outbox payloads remain individually encrypted. Retention
protects pinned Threads, queued intents, open review notes, and explicitly
protected documents. iOS and Android rebuild only the exact disposable cache
files and retain the secure master key.

Remaining: authoritative Host delivery worker integration and end-to-end
offline compose/reconnect delivery coverage.
