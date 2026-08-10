# Remora performance budgets

These budgets are release contracts for the command-center rebuild. They
define bounded behavior before implementation so mobile, shared Rust, relay,
repository, sharing, and browser surfaces converge on the same limits. A
budget in this document is not evidence that a measurement or enforcement
gate exists.

## Status vocabulary

- **Hard target, gate planned**: accepted release contract whose mechanical
  test has not landed.
- **Hard gate**: mechanically enforced and linked to evidence.
- **Informational**: collected and reported but not release-blocking.
- **Baseline pending**: no trustworthy current measurement exists.

Every hard deterministic budget below is **Hard target, gate planned** on this
repository base. None is a **Hard gate**. Promotion requires the exact
mechanical check and its evidence to land in a separately reviewed change.

## Target-scale fixture

All deterministic gates and device measurements use one fixed, synthetic
logical fixture:

- 10 Hosts;
- 250 Projects;
- 20,000 Thread summaries;
- 5,000 timeline items in the large Thread;
- 20 simultaneous active sessions.

Every result must identify the fixture generator and version, serialization
format, platform build, OS and device, cold or warm cache state, sample count,
command and commit, and raw report path. The fixture must contain synthetic
work content only; production source, prompts, transcripts, credentials, and
other sensitive work content are prohibited.

## Hard deterministic budgets

The exact contract appears in the **Exact bound** column. Row bounds, byte
bounds, item limits, deadlines, and transaction/query limits are independent;
one must not be substituted for another. `KiB` and `MiB` are binary units.

| Surface | Exact bound | Fixture | Intended enforcement owner | Current lifecycle |
| --- | --- | --- | --- | --- |
| Mission Control projection serialization | Mission Control serialized projection: at most 512 KiB | Target-scale fixture; serialize the complete Mission Control projection | Shared Rust projection boundary (`AppStore`) | Hard target, gate planned |
| Mission Control active rows | Mission Control active rows: at most 20 plus summarized counts | Target-scale fixture with all simultaneous active sessions represented | Shared Rust Mission Control projection (`AppStore`) | Hard target, gate planned |
| Sessions page | Sessions page: at most 100 rows and 256 KiB | Target-scale fixture; request the largest valid page | Shared Rust pagination boundary (`AppClient` and `AppStore`) | Hard target, gate planned |
| Initial timeline page | Initial timeline page: at most 50 items and 1 MiB | Large-Thread fixture; request initial hydration | Shared Rust conversation pagination (`AppClient` and `AppStore`) | Hard target, gate planned |
| Older timeline page | Older timeline page: at most 50 items | Large-Thread fixture; request an older page | Shared Rust conversation pagination (`AppClient` and `AppStore`) | Hard target, gate planned |
| Search | Search: at most 50 results and 200 decrypted candidates | Target-scale fixture; exercise the broadest valid synthetic query | Shared Rust local-search boundary | Hard target, gate planned |
| Streaming flush | Streaming flush: at 8 KiB or 16 ms, whichever comes first | Deterministic stream crossing both byte and time thresholds | Shared Rust stream batching (`MobileClient`) | Hard target, gate planned |
| Hidden Threads | Hidden Threads: zero hydrated timeline items | Target-scale fixture with hidden and visible Threads | Shared Rust hydration policy (`AppStore`) | Hard target, gate planned |
| Relay PostgreSQL batch worker | Relay PostgreSQL worker: at most four SQL operations per batch | Target-scale synthetic relay batch | Remora relay PostgreSQL worker | Hard target, gate planned |
| Relay outbox enqueue | Outbox enqueue: exactly one SQLite transaction | One synthetic enqueue operation at the durable outbox boundary | Remora relay outbox persistence | Hard target, gate planned |
| Public boundary schemas | No unbounded public list or string field | Boundary-value records for every public list and string | Public Rust/UniFFI, Remora Link, and relay schema owners | Hard target, gate planned |
| Browser DOM snapshot | DOM snapshot: at most 1 MiB | Synthetic browser page at and beyond the DOM boundary | Planned browser controller | Hard target, gate planned |
| Browser screenshot | Screenshot: at most 5 MiB | Synthetic browser artifact at and beyond the screenshot boundary | Planned browser controller | Hard target, gate planned |
| Browser console capture | Console entries: at most 500 | Synthetic page emitting entries through the boundary | Planned browser controller | Hard target, gate planned |
| Browser network capture | Network entries: at most 500 | Synthetic page emitting entries through the boundary | Planned browser controller | Hard target, gate planned |
| Browser action history | Action timeline: at most 1,000 | Synthetic browser session crossing the history boundary | Planned browser controller | Hard target, gate planned |
| Browser recording | Recording: five minutes or 100 MiB, whichever comes first | Synthetic recording crossing both duration and byte thresholds | Planned browser controller | Hard target, gate planned |
| Browser command execution | Browser command deadline: at most 30 seconds unless a smaller command cap applies | Synthetic commands at the default and smaller command-specific deadlines | Planned browser controller | Hard target, gate planned |
| Repository source read | Source file read: at most 1 MiB | Synthetic source files at and beyond the read boundary | Planned repository service | Hard target, gate planned |
| Share draft | Share draft: at most 25 MiB and ten items | Synthetic draft crossing both total-byte and item limits | Planned share-draft boundary | Hard target, gate planned |
| System surfaces | System-surface rows: at most five | Target-scale fixture projected onto each system surface | Shared Rust system-surface projection (`AppStore`) | Hard target, gate planned |
| Feature availability | FeatureAvailability display reason: at most 256 UTF-8 bytes | Boundary-value typed availability records | Shared Rust public type boundary | Hard target, gate planned |

## Informational device targets

Measurement devices are iPhone 12 and Pixel 6a, with iPad smoke coverage.
These targets are not release-blocking on this repository base. Every row
starts **Baseline pending** and **Informational** because no trustworthy
measurement has been recorded. Absence of a measurement is not a passing
result.

| Status | Target | Required coverage |
| --- | --- | --- |
| Baseline pending; Informational | Cached Mission Control cold display p50: at most 1.5 seconds | iPhone 12 and Pixel 6a; iPad smoke coverage |
| Baseline pending; Informational | Cached Mission Control cold display p95: at most 3 seconds | iPhone 12 and Pixel 6a; iPad smoke coverage |
| Baseline pending; Informational | Warm Mission Control: at most 750 ms | iPhone 12 and Pixel 6a; iPad smoke coverage |
| Baseline pending; Informational | Local search: p95 at most 150 ms | iPhone 12 and Pixel 6a; iPad smoke coverage |
| Baseline pending; Informational | Frame time: p95 at most 25 ms | iPhone 12 and Pixel 6a; iPad smoke coverage |
| Baseline pending; Informational | Janky frames: below 5% | iPhone 12 and Pixel 6a; iPad smoke coverage |
| Baseline pending; Informational | Home memory: at most 250 MiB | iPhone 12 and Pixel 6a; iPad smoke coverage |
| Baseline pending; Informational | Active Thread memory: at most 350 MiB | iPhone 12 and Pixel 6a; iPad smoke coverage |
| Baseline pending; Informational | Package growth: no unexplained increase above 5% from the recorded baseline | iOS and Android release-like packages; iPad smoke coverage |

## Measurement protocol

- Use release-like builds identified by exact commit and build identity. Record
  build configuration and relevant compiler/package settings; do not compare a
  debug build with a release-like baseline.
- Use the fixed synthetic target-scale fixture. Record fixture generation and
  version plus its serialization format with each run set.
- Keep device model, OS version, power source/state, and thermal conditions
  identical within a run set. Record them rather than assuming equivalence.
- For a cached cold display run, persist the synthetic Mission Control fixture,
  terminate the app, relaunch it, and begin timing before the cached projection
  is materialized in memory. A warm run repeats the display in the same app
  process after the initial render with normal caches retained. Record any
  additional cache preparation or clearing.
- Record the sample count, exact command or procedure, commit, build identity,
  result, and retained raw report path. Summary values without retrievable raw
  reports are not promotion evidence.
- Prefer platform-native instrumentation and the existing lazy-loading and
  image-cache behavior. This document does not prescribe a new runtime
  dependency.

## Baseline promotion

Device targets remain **Baseline pending** and **Informational** until three comparable baseline runs
establish low measurement variance. This document does not invent a variance
threshold: the project must explicitly review and record both the evidence and
the promotion decision. A promotion change must identify the fixture, devices
and OS versions, commands, commits/builds, sample counts, raw report paths,
variance assessment, and chosen enforcement owner.

A deterministic target becomes a **Hard gate** only when the exact mechanical
check lands, runs, and is linked to evidence through a separately reviewed
change. A device target becomes release-blocking only after the same explicit
review records how it will be enforced.

## Evidence and regression triage

Retain raw reports at a stable recorded path or artifact URL and keep a result
summary tied to the exact commit, build, fixture, environment, cache state,
sample count, and command or procedure. Evidence must use synthetic work
content and must not contain secrets or production work data.

When a target regresses:

1. Reproduce it with the same fixture and comparable build, device/OS,
   power/thermal conditions, cache state, and instrumentation.
2. Preserve both raw reports and record whether the difference is product
   behavior, fixture drift, environment drift, or measurement variance.
3. Assign triage to the enforcement owner named in the relevant row and link
   the evidence-backed plan or issue.
4. Fix the implementation or measurement defect and rerun the same protocol.

Budget changes require an evidence-backed plan or issue and explicit review.
Do not raise a limit merely to make a regression pass. A missing measurement
remains **Baseline pending**, an informational miss is reported without
silently becoming a release gate, and a future hard-gate failure blocks the
release governed by that gate.

## Ownership

- Shared Rust runtime owners maintain Mission Control, session/timeline/search,
  streaming, hydration, system-surface, and typed public-boundary budgets.
- Remora relay owners maintain PostgreSQL batch and SQLite outbox budgets.
- Browser-controller owners maintain browser artifact, history, recording, and
  command-deadline budgets when that planned surface lands.
- Repository and sharing owners maintain source-read and share-draft budgets
  when those planned services land.
- iOS and Android platform owners collect comparable device metrics with native
  instrumentation and retain their raw evidence.
- Release reviewers maintain lifecycle labels and evidence links. They may
  promote a row only through an explicit reviewed change that satisfies the
  rules above.
