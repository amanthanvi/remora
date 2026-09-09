# 004: Android projection ownership

Status: implemented. Original priority P1; Android-specific scheduling fix.

## Change

`AppModelProjectionOwner` serializes snapshot/cache commits. Blocking UniFFI reads
and saved-server persistence run outside its guard; revision tickets reject stale
reads. Refresh requests coalesce instead of retrying in an unbounded busy loop.

The native navigation owner preserves pending selection across queued activation,
older updates, repeated A/B/A selection, and subscription replacement. Activations
use one conflated queue; hydration cannot redirect a newer navigation intent.
Activation events trigger canonical reads instead of replaying their payloads.
A read started after the latest setter completes releases pending selection even
when activation events were coalesced or lost in the same subscription.
Subscription retirement holds a coroutine mutex through cancellation cleanup,
so a cancelled intermediate starter cannot bypass the retiring collector.

The existing hydration cache now respects authoritative loaded-empty history and
evicts keys absent from a full snapshot's threads and summaries. Rust history
replacement marks history loaded and clears the old pagination cursor.

## Evidence

`AppModelProjectionOwnerTest`, `AppModelNavigationIntentTest`, and
`AppModelThreadSnapshotCacheTest` exercise controlled ordering, stale reads,
activation, subscription churn, cache invalidation, and empty-history behavior.
The Rust rollback regression verifies the authoritative history boundary.

## Limits

Rust remains canonical; these helpers own native observation/navigation only.
iOS already uses main-actor isolation. No streaming batching or frame-time claim
is made. Sustained live streaming while switching/backgrounding remains a manual
validation scenario. Final gates are in [the review](../docs/reviews/2026-09-07-upstream-adoption.md).
