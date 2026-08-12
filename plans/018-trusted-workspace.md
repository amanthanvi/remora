# Plan 018: Trusted workspace suite

Status: **TODO**
Tracker: [#28](https://github.com/amanthanvi/remora/issues/28)
Baseline: Plan 017 reviewed commit and exact Link pin
Depends on: Plans 015–017

## Scope and ownership

Link owns Project registration/clone, confined reads, curated Git, isolated
worktrees, automatic checkpoints, linked-child rewind, provisioning-script
trust, review anchors, forge publishing, and cleanup eligibility. Mobile remains
read-only and confirmation-oriented.

## Contract and acceptance

Use argv arrays and canonical roots; reject traversal and symlink escape. Git
omits force/reset/clean/rebase. Checkpoints use a temporary index and preserve
the user tree/index. Test adversarial paths, checkpoint CAS/restart, baseline and
failed-turn checkpoints, trust invalidation, cleanup safety, and forge confirm.

Rollback Link first, then its Remora pin. STOP on shell construction, arbitrary
commands, direct mobile editing, destructive history UI, or uncertain cleanup.
