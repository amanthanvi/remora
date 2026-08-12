# Plan 024: Final release audit

Status: **TODO**
Tracker: [#34](https://github.com/amanthanvi/remora/issues/34)
Baseline: Plan 023 release candidate
Depends on: Plans 014–023

## Scope and ownership

Enforce deterministic CI budgets, collect iPhone 12/Pixel 6a/iPad reports, run
the threat-model validation and restore drill, update all product/development/
recovery/release docs and ADRs, run repository-local Improve/Ponytail plus
Wayfinder review, and close or explicitly defer every tracker finding.

## Contract and acceptance

Run `./scripts/verify.sh` plus full native/device/Link/relay/control-plane suites.
Require no critical security finding, no unexplained hard-budget regression,
parity QA complete, clean tracked state, restore evidence, and every release
artifact traceable to one signed manifest.

Rollback the release candidate, not validation evidence. STOP release on any
critical finding, missing platform parity, failed restore/rollback, dirty tracked
tree, or unexplained budget/provenance failure.
