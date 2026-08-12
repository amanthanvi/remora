# Plan 022: Managed DigitalOcean control plane

Status: **TODO**
Tracker: [#32](https://github.com/amanthanvi/remora/issues/32)
Baseline: Plans 015–018 reviewed commits
Depends on: Plans 015–018

## Scope and ownership

Implement the single-owner DigitalOcean deployment: P-256 owner root,
user-verified passkeys, offline recovery, short sessions, one-time Host tickets,
server-rendered private admin, outbound-only Hosts, pause/resume, safe auto-stop,
encrypted volumes, snapshots, restore, health, revocation, and redacted support.

## Contract and acceptance

No work content enters control-plane storage/admin. Recheck current prices before
provisioning. Test challenge/recovery rate limits, origin/CSRF, replay,
revocation, pause drain, volume/identity survival, restore drill, role denial,
and content isolation; document single-node PostgreSQL RPO/RTO.

Rollback infrastructure with retained volumes/snapshots. STOP before destructive
lifecycle action without a drain and required snapshot, or if Host ports become
public.
