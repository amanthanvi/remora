# Plan 003: Fix clean-worktree Codex submodule bootstrap

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. Touch
> only the files listed as in scope. If any STOP condition occurs, stop and
> report; do not improvise. Commit the work in the isolated worktree. When
> dispatched by the Improve advisor, do not update `plans/README.md`; the
> reviewer maintains the index.
>
> **Drift check (run first)**:
> `git diff --stat a7994e7e490c827eb515498f4aed61fd20718e53..HEAD -- Makefile apps/ios/scripts/sync-codex.sh tools/scripts/test-sync-codex.sh`
> Expected: no output. Any output is a STOP condition.

## Status

- **Priority**: P0
- **Effort**: S
- **Risk**: LOW
- **Depends on**: Plan 001
- **Unblocks**: Plan 002 and every clean-worktree Rust/mobile build
- **Category**: bug, dx, build
- **Tracker**: [#15](https://github.com/amanthanvi/remora/issues/15)
- **Planned at**: commit `a7994e7`, 2026-08-09
- **Execution status**: DONE at executor commit
  `58d2e8d812ef8039cd69789a96d5427e93226fa2`

## Why this matters

The first Plan 002 fast-lane build proved that a clean linked worktree cannot
initialize the Codex submodule through Make. An empty gitlink directory is
inside the superproject, so this check:

```sh
git -C "$SUBMODULE_DIR" rev-parse --verify HEAD
```

walks up to the superproject and succeeds with the superproject commit. The
script then claims to preserve that commit, skips `git submodule update`, and
later Rust commands fail because Codex manifests do not exist.

`sync-ghostty.sh` already carries the correct, simple guard: require a `.git`
file or directory at the submodule root before treating it as initialized.
Apply the same rule to Codex and add a hermetic shell regression so this
failure cannot return.

## Failure evidence

From Plan 002 worktree `/tmp/remora-plan002.E7w4r7`:

- superproject HEAD: `a7994e7e490c827eb515498f4aed61fd20718e53`;
- recorded Codex gitlink: `13595c36e218fcbd13df118eeadf00d4eb0e6d31`;
- empty `shared/third_party/codex/` had no `.git` marker;
- `git -C shared/third_party/codex rev-parse HEAD` returned `a7994e7`;
- `make ios-sim-fast` then failed to read
  `shared/third_party/codex/codex-rs/app-server-protocol/Cargo.toml`.

## Current state

`apps/ios/scripts/sync-codex.sh` begins its sync decision with:

```sh
echo "==> Syncing codex submodule..."
if ! git -C "$SUBMODULE_DIR" rev-parse --verify HEAD >/dev/null 2>&1; then
    git -C "$REPO_DIR" submodule update --init --recursive shared/third_party/codex
elif [ "$SYNC_MODE" = "--recorded-gitlink" ]; then
```

`apps/ios/scripts/sync-ghostty.sh` already protects the equivalent check:

```sh
if [ ! -d "$SUBMODULE_DIR/.git" ] && [ ! -f "$SUBMODULE_DIR/.git" ]; then
    echo "==> ghostty submodule missing; initializing..."
    git -C "$REPO_DIR" submodule update --init --recursive shared/third_party/ghostty
fi
```

No Codex sync regression test exists. The root `make test` target already
aggregates small harness tests before platform suites.

## Scope

**In scope**:

- `apps/ios/scripts/sync-codex.sh`
- `tools/scripts/test-sync-codex.sh`
- `Makefile`

**Out of scope**:

- Changing the recorded Codex gitlink or any Codex patch.
- Editing contents inside `shared/third_party/codex`.
- Changing `--preserve-current` semantics for an initialized checkout.
- Refactoring Ghostty sync or adding a general submodule framework.
- Auth source from blocked Plan 002.
- CI workflow changes, dependencies, generated files, and global Git config.

## Git workflow

- Base the isolated branch on approved Plan 001 commit `a7994e7`.
- One commit after all verification passes.
- Commit subject: `build: fix clean Codex bootstrap`.
- Do not push, merge, or open a pull request.

## Steps

### Step 1: Add the failing hermetic regression

Create executable `tools/scripts/test-sync-codex.sh` using the established
temporary-fixture style from `test-bootstrap-remora-link.sh`:

1. `set -euo pipefail`.
2. Create an exact `mktemp -d` root and clean only that validated root on exit.
3. Recreate the minimum repository layout under the fixture:
   `apps/ios/scripts`, empty `shared/third_party/codex`, and `patches/codex`.
4. Copy the production `sync-codex.sh` into the fixture.
5. Create empty files for every patch basename declared by the production
   script. This satisfies file-presence checks; the fake Git command handles
   patch checks.
6. Put a fake executable `git` first on `PATH`. It must append every argv array
   to a capture file and support only these expected calls:
   - superproject `submodule update --init --recursive
     shared/third_party/codex`: succeed;
   - submodule `rev-parse --verify HEAD` and `rev-parse HEAD`: print the fixed
     recorded fixture commit and succeed;
   - superproject `ls-files --stage shared/third_party/codex`: print one valid
     gitlink row for that same commit;
   - `apply --reverse --check`: succeed;
   - `rev-parse --short HEAD`: print the fixed short commit;
   - any unexpected argv: print a bounded error and fail.
7. Run two isolated fixture cases:
   - **clean directory**: no `.git` marker, fake current commit equal to the
     recorded commit, and exactly one matching submodule-update call;
   - **initialized preserve-current**: create a `.git` file marker, make the
     fake current commit differ from the recorded commit, assert zero
     submodule-update calls, and assert the capture contains the expected
     `ls-files --stage` plus `rev-parse HEAD` calls.
8. Give a specific bounded assertion message for each wrong call count, such
   as `clean case: expected exactly one Codex submodule update`.

Use fixed 40-hex recorded/current fixture commits, such as forty `1` and forty
`2` characters. Never contact the network or mutate the real repository.

Before changing production code, run the new test and require the specific
clean-case assertion to fail:

```sh
set +e
PLAN_RED_OUTPUT="$(./tools/scripts/test-sync-codex.sh 2>&1)"
PLAN_RED_STATUS=$?
set -e
test "$PLAN_RED_STATUS" -ne 0
printf '%s\n' "$PLAN_RED_OUTPUT" | rg -q 'clean case: expected exactly one Codex submodule update'
```

Expected: all wrapper assertions pass, proving the test fails for the intended
missing initialization call rather than a fixture/syntax error. Do not proceed
if the test unexpectedly passes or fails for another reason.

### Step 2: Add the explicit initialization guard

Immediately before the existing `rev-parse --verify HEAD` decision in
`sync-codex.sh`, add the same `.git` file-or-directory guard used by
`sync-ghostty.sh`, with Codex-specific text:

```sh
if [ ! -d "$SUBMODULE_DIR/.git" ] && [ ! -f "$SUBMODULE_DIR/.git" ]; then
    echo "==> codex submodule missing; initializing..."
    git -C "$REPO_DIR" submodule update --init --recursive shared/third_party/codex
fi
```

Keep the existing second `rev-parse` check. It still handles an initialized but
invalid checkout. Keep both sync modes and all patch behavior unchanged.

### Step 3: Wire the regression into the build harness

In `Makefile`:

- add `sync-codex-test` to `.PHONY`;
- add a target that runs `./tools/scripts/test-sync-codex.sh`;
- add it to the root `test` prerequisite list before Rust/platform suites;
- add one concise help entry beside the existing harness-test entries.

Do not change other target recipes or ordering.

### Step 4: Run hermetic and live clean-worktree verification

Run:

```sh
bash -n apps/ios/scripts/sync-codex.sh tools/scripts/test-sync-codex.sh
./tools/scripts/test-sync-codex.sh
make sync-codex-test
git diff --check
```

Expected: all commands pass, and the direct script prints one concise success
line.

Then run the real supported path from this fresh isolated worktree:

```sh
test ! -e shared/third_party/codex/.git
make sync
test -e shared/third_party/codex/.git
test -f shared/third_party/codex/codex-rs/app-server-protocol/Cargo.toml
PLAN_RECORDED_CODEX="$(git ls-files --stage shared/third_party/codex | awk 'NR == 1 { print $2 }')"
test -n "$PLAN_RECORDED_CODEX"
test "$(git -C shared/third_party/codex rev-parse HEAD)" = "$PLAN_RECORDED_CODEX"
```

Expected: `make sync` initializes the recorded gitlink, applies the existing
patch set, and creates the expected manifest. Network access is allowed only
for this recorded submodule fetch. Dirty local patch state inside the submodule
is expected and must not be staged.

Run exact tracked scope checks:

```sh
PLAN_CHANGED_FILES="$(git status --short --ignore-submodules=all --untracked-files=all | cut -c4- | LC_ALL=C sort)"
PLAN_EXPECTED_CHANGED_FILES="$(printf '%s\n' \
  Makefile \
  apps/ios/scripts/sync-codex.sh \
  tools/scripts/test-sync-codex.sh | LC_ALL=C sort)"
test "$PLAN_CHANGED_FILES" = "$PLAN_EXPECTED_CHANGED_FILES"
```

Inspect the full diff, stage only the three explicit files, inspect the staged
diff, and commit. Verify:

```sh
test "$(git log -1 --format=%s)" = 'build: fix clean Codex bootstrap'
PLAN_COMMIT_FILES="$(git diff-tree --no-commit-id --name-only -r HEAD | LC_ALL=C sort)"
PLAN_EXPECTED_COMMIT_FILES="$(printf '%s\n' \
  Makefile \
  apps/ios/scripts/sync-codex.sh \
  tools/scripts/test-sync-codex.sh | LC_ALL=C sort)"
test "$PLAN_COMMIT_FILES" = "$PLAN_EXPECTED_COMMIT_FILES"
test -z "$(git status --short --ignore-submodules=all --untracked-files=all)"
```

## Test plan

- The hermetic fake-Git test deterministically reproduces the empty-directory
  parent-walk trap without a network or real submodule mutation.
- `bash -n` catches shell syntax regressions.
- `make sync-codex-test` proves Make wiring.
- Live `make sync` in a fresh isolated worktree proves the real recorded
  submodule and patch flow succeeds.
- Exact path checks keep auth work and submodule contents out of the commit.

## Done criteria

- [x] Empty Codex gitlink directories issue exactly one initialization call.
- [x] Initialized-checkout `--preserve-current` fixture performs zero update
      calls and resolves both the recorded and distinct current commits.
- [x] Hermetic regression passes directly and through Make.
- [x] Fresh-worktree `make sync` produces the expected Codex manifest.
- [x] The recorded gitlink and patches are unchanged.
- [x] Exactly three scoped files are committed.
- [x] Superproject status is clean when local submodule patch state is ignored.

## Execution record

- **Executor branch**: `executor/003-clean-codex-bootstrap`
- **Executor worktree**: `/tmp/remora-plan003.81jK0p`
- **Commit**: `58d2e8d812ef8039cd69789a96d5427e93226fa2`
  (`build: fix clean Codex bootstrap`)
- **Parent**: approved Plan 001 commit
  `a7994e7e490c827eb515498f4aed61fd20718e53`
- **Changed files**: exactly `Makefile`,
  `apps/ios/scripts/sync-codex.sh`, and
  `tools/scripts/test-sync-codex.sh`.
- **Red proof**: pre-fix hermetic test failed only with
  `clean case: expected exactly one Codex submodule update`.
- **Green proof**: shell syntax, direct hermetic test, Make harness target,
  whitespace, live `make sync`, required manifest, executable bit, exact
  gitlink equality, commit scope, subject, and clean status all passed.
- **Live Codex gitlink**:
  `13595c36e218fcbd13df118eeadf00d4eb0e6d31`.
- **Integration**: local executor commit only; not merged or pushed by the
  Improve advisor.

## STOP conditions

Stop and report without improvising if:

- Scoped files drifted after `a7994e7`.
- The fix requires changing the gitlink, patch contents, or preserve-current
  semantics for an initialized checkout.
- The hermetic test requires network, global Git configuration, or real
  repository mutation.
- Live `make sync` checks out a commit other than the recorded gitlink before
  applying local patches.
- Any required syntax, hermetic, Make, live sync, scope, or post-commit check
  fails twice after correcting only a command/environment typo.

## Maintenance notes

- A directory inside a Git worktree is not an initialized submodule merely
  because `git -C <directory> rev-parse HEAD` succeeds. Require its own `.git`
  marker first.
- Keep the explicit guard local and readable. Two existing sync scripts do not
  justify a generic submodule framework.
- After approval, restack Plan 002 on this commit and rerun the original
  Make-only fast-lane validation; do not bless a direct script workaround.
