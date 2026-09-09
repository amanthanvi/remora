# 003: Recoverable submitted drafts

Status: implemented. Original priority P1; both native composers changed.

## Change

Home and conversation composers retain a submitted draft independently of the
live editor until the send succeeds. Failure or uncertain delivery leaves it
available through an explicit saved-draft control. Restoring it preserves any
newer editor contents as another saved draft. Nothing resends automatically.

Drafts retain text, native image handles/bytes, files, and supported mentions.
Home launch configuration is captured before async creation. If creation succeeds
but the first turn fails, the created thread identity is retained. iOS saves any
newer home edits before navigation on either success or failure. Android sends
are owned by AppModel, so leaving a composer does not cancel its submission.

## Evidence

- iOS `ComposerRecoveryStoreTests`: attachments/mentions, newer edits,
  out-of-order completions, destination isolation, and both home handoff outcomes
- Android `ComposerDraftRecoveryStoreTest`: delayed failure, out-of-order results,
  failed creation, launch settings, created-thread identity, and navigation lifetime
- Both native build/test gates listed in [the review](../docs/reviews/2026-09-07-upstream-adoption.md)

## Limits

Submitted drafts now survive process restart in protected, excluded-from-backup
iOS storage and an Android Keystore-encrypted no-backup journal. Both commit
before clearing the editor or dispatching. Interrupted submissions become
unconfirmed and require an explicit restore and send. Recovery retains its
durable source until equivalent resubmission or confirmed deletion. Corruption
and failed writes preserve existing data and block the affected operation.

Exceptions are conservatively displayed as unconfirmed, without parsing
wire-error strings. A real provider's delayed-rejection journey
has not been exercised on both installed apps. Current model/permission controls
remain editable when a draft is recovered; recovery is not an exact-payload retry.
