# Plan 008: Document command-center performance budgets

> **Executor instructions**: Follow this plan step by step from the reviewed
> Plan 007 commit. Touch only scoped files, honor STOP conditions, validate, and
> commit in a fresh isolated worktree. Do not update `plans/README.md`.
>
> **Drift check (run first)**:
> `( set -euo pipefail; PLAN007_SHA='6faa9c3e3667a38c5865021baffca1b416c1eb97'; git cat-file -e "${PLAN007_SHA}^{commit}"; git diff --stat "${PLAN007_SHA}..HEAD" -- README.md docs/performance/budgets.md )`
> Expected: no output. Any output is a STOP condition.

## Status

- **Priority**: P0
- **Effort**: S
- **Risk**: LOW
- **Depends on**: Plan 007
- **Category**: performance, documentation
- **Planned at**: reviewed Plan 007 commit
  `6faa9c3e3667a38c5865021baffca1b416c1eb97`, 2026-08-09
- **Tracker**: <https://github.com/amanthanvi/remora/issues/20>
- **Execution status**: DONE at
  `3cef97ca4c10cc285e9d1e6bba63d596d7cf2b11` after a reviewer-directed fresh
  retry from the unchanged Plan 007 parent.

## Why this matters

The command-center rebuild accepts bounded pages, projections, browser
artifacts, database work, and relay queries from the start. Those numbers must
be written as testable contracts before implementations diverge. Current code
does not yet implement most target surfaces, so the document must distinguish a
hard target from an already-enforced gate and must not invent measurements.

## Scope

**In scope**:

- `docs/performance/budgets.md`
- `README.md`

**Out of scope**:

- Benchmark code, CI workflows, product source, fixtures, device automation,
  profiler traces, generated reports, dependencies, or measured baseline data.
- Changing any accepted numeric budget.
- Promoting informational device targets to hard gates.

## Git workflow

- Create a fresh isolated worktree from
  `6faa9c3e3667a38c5865021baffca1b416c1eb97`.
- One commit after all validation passes.
- Commit subject: `docs: define performance budgets`.
- Do not merge, push, or open a pull request.

## Steps

### Step 1: Create the canonical performance budget document

Create `docs/performance/budgets.md` with exact sections:

- `# Remora performance budgets`
- `## Status vocabulary`
- `## Target-scale fixture`
- `## Hard deterministic budgets`
- `## Informational device targets`
- `## Measurement protocol`
- `## Baseline promotion`
- `## Evidence and regression triage`
- `## Ownership`

Status vocabulary must distinguish:

- `Hard target, gate planned`: accepted release contract whose mechanical test
  has not landed;
- `Hard gate`: mechanically enforced and linked to evidence;
- `Informational`: collected and reported but not release-blocking;
- `Baseline pending`: no trustworthy current measurement exists.

Do not label any command-center budget `Hard gate` unless the current repository
contains and runs that exact mechanical check through a separately reviewed
change. In this plan, every Step 3 row must be labeled
`Hard target, gate planned`; `Hard gate` is vocabulary for future promotion
only. Do not invent p50/p95, memory, package, or query-count results.

### Step 2: Record the exact target-scale fixture

Document one deterministic logical fixture:

- 10 Hosts;
- 250 Projects;
- 20,000 Thread summaries;
- 5,000 timeline items in the large Thread;
- 20 simultaneous active sessions.

Record that fixture generation, serialization format, platform build, OS/device,
warm/cold cache state, sample count, command/commit, and raw report path must be
captured with every result. Sensitive work content must be synthetic.

### Step 3: Record every hard deterministic budget

Use a table with surface, exact bound, fixture, intended enforcement owner, and
current lifecycle. Include all of these unchanged:

- `Mission Control serialized projection: at most 512 KiB`;
- `Mission Control active rows: at most 20 plus summarized counts`;
- `Sessions page: at most 100 rows and 256 KiB`;
- `Initial timeline page: at most 50 items and 1 MiB`;
- `Older timeline page: at most 50 items`;
- `Search: at most 50 results and 200 decrypted candidates`;
- `Streaming flush: at 8 KiB or 16 ms, whichever comes first`;
- `Hidden Threads: zero hydrated timeline items`;
- `Relay PostgreSQL worker: at most four SQL operations per batch`;
- `Outbox enqueue: exactly one SQLite transaction`;
- `No unbounded public list or string field`;
- `DOM snapshot: at most 1 MiB`;
- `Screenshot: at most 5 MiB`;
- `Console entries: at most 500`;
- `Network entries: at most 500`;
- `Action timeline: at most 1,000`;
- `Recording: five minutes or 100 MiB, whichever comes first`;
- `Browser command deadline: at most 30 seconds unless a smaller command cap applies`;
- `Source file read: at most 1 MiB`;
- `Share draft: at most 25 MiB and ten items`;
- `System-surface rows: at most five`;
- `FeatureAvailability display reason: at most 256 UTF-8 bytes`.

Use binary units (`KiB`, `MiB`) where specified. Do not silently reinterpret a
row/count bound as a byte bound or vice versa.

### Step 4: Record informational device targets

Devices: iPhone 12 and Pixel 6a, with iPad smoke coverage. Record unchanged:

- `Cached Mission Control cold display p50: at most 1.5 seconds`;
- `Cached Mission Control cold display p95: at most 3 seconds`;
- `Warm Mission Control: at most 750 ms`;
- `Local search: p95 at most 150 ms`;
- `Frame time: p95 at most 25 ms`;
- `Janky frames: below 5%`;
- `Home memory: at most 250 MiB`;
- `Active Thread memory: at most 350 MiB`;
- `Package growth: no unexplained increase above 5% from the recorded baseline`.

Every row starts `Baseline pending` and `Informational`. Explain that absence of
a measurement is not a passing result.

### Step 5: Define measurement and promotion rules

Require release-like builds, fixed synthetic fixture, identical device/OS and
power/thermal conditions within a run set, cold/warm definition, raw report
retention, and explicit commit/build identity. Prefer platform-native
instrumentation and existing lazy/image caches; do not prescribe a new runtime
dependency.

Device targets become hard only after three comparable baseline runs establish
low measurement variance and the project explicitly reviews and records the
promotion. Do not invent a variance threshold in this documentation-only plan.
Budget changes require an evidence-backed plan/issue; do not raise limits merely
to make a regression pass.

### Step 6: Link from README and validate

Add the performance budget next to the canonical threat-model link in README's
documentation/security area. State that most command-center gates are planned,
not current measurements, using the exact sentence: `Most command-center
performance gates are planned, not current measurements.`

Every literal checked below with `rg -F` must be contiguous on one physical
line in the output file. In particular, do not wrap either of these literals:

- `three comparable baseline runs`
- `Most command-center performance gates are planned, not current measurements.`

Run:

```sh
(
  set -euo pipefail
  PLAN_BUDGETS='docs/performance/budgets.md'
  test -f "$PLAN_BUDGETS"
  for heading in \
    '# Remora performance budgets' \
    '## Status vocabulary' \
    '## Target-scale fixture' \
    '## Hard deterministic budgets' \
    '## Informational device targets' \
    '## Measurement protocol' \
    '## Baseline promotion' \
    '## Evidence and regression triage' \
    '## Ownership'; do
    rg -n -F "$heading" "$PLAN_BUDGETS"
  done
  for term in \
    'Hard target, gate planned' 'Hard gate' 'Informational' 'Baseline pending' \
    '10 Hosts' '250 Projects' '20,000 Thread summaries' \
    '5,000 timeline items' '20 simultaneous active sessions' \
    'Mission Control serialized projection: at most 512 KiB' \
    'Mission Control active rows: at most 20 plus summarized counts' \
    'Sessions page: at most 100 rows and 256 KiB' \
    'Initial timeline page: at most 50 items and 1 MiB' \
    'Older timeline page: at most 50 items' \
    'Search: at most 50 results and 200 decrypted candidates' \
    'Streaming flush: at 8 KiB or 16 ms, whichever comes first' \
    'Hidden Threads: zero hydrated timeline items' \
    'Relay PostgreSQL worker: at most four SQL operations per batch' \
    'Outbox enqueue: exactly one SQLite transaction' \
    'No unbounded public list or string field' \
    'DOM snapshot: at most 1 MiB' 'Screenshot: at most 5 MiB' \
    'Console entries: at most 500' 'Network entries: at most 500' \
    'Action timeline: at most 1,000' \
    'Recording: five minutes or 100 MiB, whichever comes first' \
    'Browser command deadline: at most 30 seconds unless a smaller command cap applies' \
    'Source file read: at most 1 MiB' \
    'Share draft: at most 25 MiB and ten items' \
    'System-surface rows: at most five' \
    'FeatureAvailability display reason: at most 256 UTF-8 bytes' \
    'iPhone 12' 'Pixel 6a' 'iPad smoke coverage' \
    'Cached Mission Control cold display p50: at most 1.5 seconds' \
    'Cached Mission Control cold display p95: at most 3 seconds' \
    'Warm Mission Control: at most 750 ms' \
    'Local search: p95 at most 150 ms' \
    'Frame time: p95 at most 25 ms' 'Janky frames: below 5%' \
    'Home memory: at most 250 MiB' \
    'Active Thread memory: at most 350 MiB' \
    'Package growth: no unexplained increase above 5% from the recorded baseline' \
    'three comparable baseline runs'; do
    rg -n -F "$term" "$PLAN_BUDGETS"
  done
  rg -n -F 'docs/performance/budgets.md' README.md
  rg -n -F 'Most command-center performance gates are planned, not current measurements.' README.md
  if rg -n -F '<PLAN007_SHA>' README.md "$PLAN_BUDGETS"; then
    echo 'unresolved Plan 007 placeholder' >&2
    exit 1
  else
    PLAN_RG_STATUS=$?
    test "$PLAN_RG_STATUS" -eq 1
  fi
  git diff --check
)
```

Read the finished document end-to-end. Confirm all numbers match the accepted
plan, no measurement is fabricated, and no unimplemented gate is labeled
current.

Run exact two-file scope and self-contained post-commit gates, using the
substituted Plan 007 SHA for the parent:

```sh
(
  set -euo pipefail
  PLAN_EXPECTED_FILES="$(printf '%s\n' README.md docs/performance/budgets.md | LC_ALL=C sort)"
  PLAN_CHANGED_FILES="$(git status --short --ignore-submodules=all --untracked-files=all | cut -c4- | LC_ALL=C sort)"
  test "$PLAN_CHANGED_FILES" = "$PLAN_EXPECTED_FILES"
)
```

Stage only the two files, inspect the cached diff, commit, then run:

```sh
(
  set -euo pipefail
  PLAN_EXPECTED_FILES="$(printf '%s\n' README.md docs/performance/budgets.md | LC_ALL=C sort)"
  test "$(git log -1 --format=%s)" = 'docs: define performance budgets'
  test "$(git rev-parse HEAD^)" = '6faa9c3e3667a38c5865021baffca1b416c1eb97'
  PLAN_COMMIT_FILES="$(git diff-tree --no-commit-id --name-only -r HEAD | LC_ALL=C sort)"
  test "$PLAN_COMMIT_FILES" = "$PLAN_EXPECTED_FILES"
  test -z "$(git status --short --ignore-submodules=all --untracked-files=all)"
)
```

## Done criteria

- [x] Every accepted deterministic and device budget is documented unchanged.
- [x] Target scale, units, fixtures, owner, and lifecycle are explicit.
- [x] Missing gates and device baselines are not represented as passing.
- [x] Three-run promotion and evidence-backed regression policy are explicit.
- [x] README links the canonical budget document without claiming results.
- [x] Exactly two scoped files are committed from the reviewed Plan 007 parent.
- [x] Isolated worktree is clean.

## STOP conditions

Stop and report without improvising if:

- The parent does not match reviewed Plan 007 commit
  `6faa9c3e3667a38c5865021baffca1b416c1eb97`.
- Drift exists after the substituted parent.
- A required numeric budget conflicts with the accepted rebuild plan.
- Accurate documentation would require inventing a measurement, fixture result,
  CI gate, tool, or variance threshold.
- Any file outside scope, runtime dependency, benchmark implementation, or
  generated artifact is required.
- Any static, scope, or post-commit check fails twice after correcting only an
  environment or command typo.

## Retry record

- First worktree: `/tmp/remora-plan008.xgEwfu/worktree`.
- No commit was created.
- Drift and isolation passed; exactly the two scoped files changed.
- Static run one reached the wrapped `three comparable baseline runs` literal;
  the executor corrected only that physical line.
- Static run two passed the document terms and reached the wrapped exact README
  sentence, then stopped as required.
- The advisor inspected the complete uncommitted document and README diff. No
  semantic, numeric, lifecycle, or scope defect was found.
- This plan revision authorizes one fresh retry from the unchanged exact parent
  and makes the mechanical single-line requirement explicit. The stopped
  worktree must not be reused or committed.
- Fresh retry worktree: `/tmp/remora-plan008-retry.9VX5sR/worktree`; branch:
  `executor/008-performance-budgets-retry`; commit:
  `3cef97ca4c10cc285e9d1e6bba63d596d7cf2b11`.
- The retry passed drift, complete literal/link/heading validation, exact
  two-file scope, cached diff, subject/parent/file-set, cleanliness, and
  `git diff --check` gates. It recorded 22 deterministic rows, all planned, and
  nine device rows, all baseline-pending/informational, with zero hard-gate
  lifecycle rows.
- The advisor independently read both files and rechecked exact parent, subject,
  file set, cleanliness, numeric/lifecycle inventory, and whitespace.
- No merge, push, or pull request was performed.

## Rollback

Revert the single documentation commit. No runtime, data, protocol, or CI state
changes.
