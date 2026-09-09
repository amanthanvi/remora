# Background Relay

The host can publish content-free wake events while a phone is detached. This
feature is opt-in and separate from the Iroh transport relay. It requires a
Remora background relay service, plus a mobile client implementing the
proof-bound operations in [the wire contract](remora-link-v2-wire.md).

## Host Configuration

Add this section to `host.toml`, then restart the host daemon. Runtime reload
does not replace relay custody or its background publisher.

```toml
[background_relay]
origin = "https://relay.example"
bootstrap_token_file = "/absolute/private/path/relay-bootstrap-token"
```

The origin must have no user information, non-root path, query, or fragment.
HTTP is rejected unless `allow_loopback_http = true` is explicitly configured
with a literal loopback IP address; this is only for local verification. A
hostname such as `localhost` does not qualify. Requests do not follow redirects
or inherit proxy environment settings. Connections have a three-second timeout,
requests an eight-second timeout, and response bodies a 64 KiB limit.

Bootstrap tokens must be 32-256 non-whitespace characters in a regular
owner-only file. On Unix the final path must not be a symlink. Custody is an
atomic, fsynced mode-0600 journal in the host state directory; its directory is
mode 0700. The journal is capped at 4 MiB and 128 enrollments.

Windows uses a protected DACL with one full-control entry for the current token
user, verifies the file owner and DACL through its open handle, and rejects
reparse points. Pending files receive this descriptor when created, before any
secret bytes are written. Replacement flushes file contents and uses
`MoveFileExW` with replacement and write-through flags; it does not rely on a
Unix-style directory fsync. The state directory is owner-checked and protected
before use. Filesystems that cannot enforce these checks fail closed.

An operator-provided Windows bootstrap file must have the same current-user
owner and protected, non-inherited single-user DACL. An inherited default ACL,
Everyone grant, null DACL, or different owner is rejected, not silently trusted.
Protect an empty bootstrap file before writing its token. Existing token-bearing
files must be moved to secure custody without printing their contents.

The operator supplies and protects bootstrap authority; mobile clients never
receive it. A durable provisioning key is written before requesting an
installation, so retries and host restarts do not create duplicate installations.
Read/manage capability custody is retained only until the mobile client's
authenticated durable commit. Write authority remains host-only. Secrets are
redacted from typed Debug output. Never place the custody journal or token file
in a repository or ordinary diagnostic attachment.

## Publication and Repair

The publisher polls sessions once per second and coalesces unchanged runtime
state. Each event contains AES-GCM ciphertext over a fixed content-free marker,
not transcripts, prompts, tool arguments, paths, or authoritative state. Clients
do not decrypt it. The host persists the complete event identity and body before
HTTP, retries that exact body after ambiguous failure, then persists the returned
cursor before a repair barrier may certify it. An expired event is replaced only
after a definite HTTP 400 rejection; an uncertain response preserves its body.

Publication acquires the same per-credential revocation fence as authenticated
control operations. A committed revocation prevents new publication. Revoked
entries are not automatically recycled or silently reassigned. Origin changes
and malformed or incomplete custody fail closed. Any uncertain journal write
disables relay work until a daemon restart reloads a valid durable journal.

Codex transports remain attached to their backend after phone detach. Both
WebSocket and JSONL frames feed the shared replay ring; only runtime notifications
and requests replay, while responses remain tied to their original attachment
generation. Successful initialize results are retained and only compatible
reattachments may reuse them. Changed capabilities require a fresh session.
Active detached turns survive idle session expiry. Shutdown terminates retained
tasks; session replacement cannot be detached by a stale attachment.
Unanswered Codex requests have a separate bounded table and replay after
compatible initialization even when their original ring frames were evicted.
Answered or server-resolved requests are removed and never replay as approvals.

Relay delivery is only a wake signal. The mobile client must repair every runtime
named by the host and match authenticated before/after barriers before committing
a repaired cursor. Provider acceptance, OS wake delivery, authenticated host
repair, durable local storage, and relay acknowledgement are separate outcomes.

## Verification

From this workspace:

```sh
cargo test -p remora-host -p remora-bridge-core --lib -- --test-threads=1
cargo test -p remora-bridge-core --test session_resilience
REMORA_RELAY_TEST_BINARY=/absolute/path/to/remora-relay \
  cargo test -p remora-host --lib \
  real_loopback_relay_publication_and_authenticated_cursor_barrier \
  -- --ignored --test-threads=1
```

The opt-in integration test owns an ephemeral loopback relay process with local
SQLite and mock providers. It verifies real installation provisioning, durable
commit, detached event publication, fetch, cursor validation, and coalescing.
It does not establish APNs/FCM production credentials or physical-device delivery.

On a Windows host, `cargo test -p remora-host --lib background_relay --
--test-threads=1` also runs native ACL creation/replacement, broad/null DACL
rejection, and the platform-independent enrollment/publication lifecycle tests.
Cross-compiling this target checks Win32 signatures and Rust code, but does not
substitute for executing ACL and replacement tests on Windows.
