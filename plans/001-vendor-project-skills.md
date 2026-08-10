# Plan 001: Vendor the pinned project skills

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. Touch
> only the files listed as in scope. If any STOP condition occurs, stop and
> report; do not improvise. Commit the work in the isolated worktree. One
> override when dispatched by the Improve advisor: do not update
> `plans/README.md`; the reviewer maintains the index.
>
> **Drift check (run first)**:
> `git diff --stat 4d4f9a9d203a8df89cb465754325cc922950e80a..HEAD -- .gitignore AGENTS.md .agents/skills .agents/vendor-skills`
> Expected: no output. Any output is a STOP condition; do not reinterpret or
> reconcile drift inside this plan.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: MED
- **Depends on**: none
- **Category**: dx
- **Planned at**: commit `4d4f9a9`, 2026-08-09
- **Tracker**: <https://github.com/amanthanvi/remora/issues/13>
- **Execution**: DONE at commit
  `a7994e7e490c827eb515498f4aed61fd20718e53`; included in the current M0 chain.

## Why this matters

Improve currently exists only in the owner's machine-global agent directory;
Ponytail is not installed there. Remora must carry pinned, reviewable copies of
both so future sessions can use the repository's exact instructions. The
repository currently ignores the entire `.agents/` directory, so installing
the files without first narrowing that rule would silently leave them
untracked.

Official Codex documentation says duplicate skill names are not merged and both
can appear in selectors. Therefore byte-identical upstream bundles will live in
the non-scanned `.agents/vendor-skills/` tree. Two tiny project-owned selectors
with unique frontmatter names will live under `.agents/skills/` and explicitly
load those bundles. The deterministic invocations are `$remora-improve` and
`$remora-ponytail`.

This plan vendors only the two core skills. It deliberately excludes hooks,
MCP servers, benchmarks, package scripts, status lines, and global
configuration.

## Current state

- `.gitignore` owns local agent-state exclusions. Its current tail contains:

  ```gitignore
  # Factory/Droid
  .factory/
  .claude/
  shared/third_party/codex/
  .wrangler/
  artifacts/
  .xcodebuildmcp/
  .agents/
  .antigravitycli/
  skills-lock.json
  ```

- `AGENTS.md` is the repository guidance. It currently ends with Commit & Pull
  Request Guidelines and has no project-skills section.
- `.agents/skills/` and `.agents/vendor-skills/` do not exist in the repository.
- `git check-ignore -v .agents/skills/improve/SKILL.md` currently reports the
  `.agents/` rule.
- Commit style is concise and imperative, for example
  `bridge: retry initialize handshake` from `AGENTS.md`.
- Codex CLI 0.146.1 recognizes `.agents/skills/<skill-name>/SKILL.md` as a
  repository-scoped skill root.
- The machine-global root already contains `improve` but not `ponytail`.
  Unique repository selectors avoid the current Improve collision and future
  Ponytail collisions while keeping invocation names consistent.
- Official reference: <https://learn.chatgpt.com/docs/build-skills.md>, section
  “Where Codex loads local skills.”

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Drift | `git diff --stat 4d4f9a9d203a8df89cb465754325cc922950e80a..HEAD -- .gitignore AGENTS.md .agents/skills .agents/vendor-skills` | no output |
| Install Improve | `python /Users/amanthanvi/.codex/skills/.system/skill-installer/scripts/install-skill-from-github.py --repo shadcn/improve --path skills/improve --ref 03369ee6d7cafbfcecc4346539b05b3dc0a603bb --dest .agents/vendor-skills` | reports installation of `improve`, exit 0 |
| Install Ponytail | `python /Users/amanthanvi/.codex/skills/.system/skill-installer/scripts/install-skill-from-github.py --repo dietrichgebert/ponytail --path skills/ponytail --ref 2ed6c52c9d7e5e56942508591085fd45dea277d3 --dest .agents/vendor-skills` | reports installation of `ponytail`, exit 0 |
| Upstream inventory | `find .agents/vendor-skills -type f -print \| sort` | exactly the five upstream files listed below |
| Final inventory | `find .agents/skills .agents/vendor-skills -type f -print \| sort` | exactly eight files: five upstream, two selectors, one provenance file |
| Checksums | `find .agents/vendor-skills/improve -type f -print0 \| sort -z \| xargs -0 shasum -a 256; shasum -a 256 .agents/vendor-skills/ponytail/SKILL.md` | exactly the five upstream checksums below |
| Discovery | `codex debug prompt-input '$remora-improve $remora-ponytail'` plus the two independent checks in Step 6 | both unique project skills and repository paths are reported by a fresh process |
| Scope | `git status --short --untracked-files=all` | changes only in the in-scope paths |

## Suggested executor toolkit

- Use the installed system `skill-installer` helper for network acquisition.
- Use `apply_patch` for `.gitignore`, `AGENTS.md`, and provenance edits.
- Do not use a package manager or add a runtime dependency.

## Scope

**In scope**:

- `.gitignore`
- `AGENTS.md`
- `.agents/vendor-skills/improve/SKILL.md`
- `.agents/vendor-skills/improve/references/audit-playbook.md`
- `.agents/vendor-skills/improve/references/closing-the-loop.md`
- `.agents/vendor-skills/improve/references/plan-template.md`
- `.agents/vendor-skills/ponytail/SKILL.md`
- `.agents/skills/remora-improve/SKILL.md`
- `.agents/skills/remora-ponytail/SKILL.md`
- `.agents/skills/PROVENANCE.md`

**Out of scope**:

- Any machine-global file under `/Users/amanthanvi/.agents/` or
  `/Users/amanthanvi/.codex/`.
- Ponytail hooks, MCP implementation, benchmarks, package manifests, status
  lines, extra command variants, examples, assets, and plugin manifests.
- `skills-lock.json`.
- Application source, tests, CI, generated bindings, and dependency manifests.
- Any change to the text of the upstream skill files under
  `.agents/vendor-skills/`.

## Git workflow

- Retain the branch supplied by the isolated-worktree dispatcher.
- One commit after all verification passes.
- Commit subject: `agents: vendor project improvement skills`.
- Do not push or open a pull request.

## Steps

### Step 1: Narrow the `.agents` ignore rule

Replace only the `.agents/` line in `.gitignore` with rules that continue to
ignore arbitrary local agent state while allowing the repository skills:

```gitignore
.agents/*
!.agents/skills/
.agents/skills/*
!.agents/skills/remora-improve/
!.agents/skills/remora-improve/**
!.agents/skills/remora-ponytail/
!.agents/skills/remora-ponytail/**
!.agents/skills/PROVENANCE.md
!.agents/vendor-skills/
.agents/vendor-skills/*
!.agents/vendor-skills/improve/
!.agents/vendor-skills/improve/**
!.agents/vendor-skills/ponytail/
!.agents/vendor-skills/ponytail/**
```

Do not relax any other ignore rule.

**Verify**:

```sh
git check-ignore -v .agents/local-state.json
```

Expected: `.agents/*` is reported.

### Step 2: Install the exact upstream skills

Run the two installer commands from the command table in repository root. Do
not use `--method git` unless the helper's normal download path fails; if it
fails for a reason other than a transient download/auth fallback, STOP.

Expected upstream inventory:

```text
.agents/vendor-skills/improve/SKILL.md
.agents/vendor-skills/improve/references/audit-playbook.md
.agents/vendor-skills/improve/references/closing-the-loop.md
.agents/vendor-skills/improve/references/plan-template.md
.agents/vendor-skills/ponytail/SKILL.md
```

Expected upstream SHA-256 values:

```text
1599aa29e9b16424cb767779efac53fa56e3f38be92dff4ac587393a1ad5070a  .agents/vendor-skills/improve/SKILL.md
587c1b927070f3c28139ad95ac34cc21a7da32618cfeb966cb774e58a865d4c8  .agents/vendor-skills/improve/references/audit-playbook.md
5d526bb378e1783bbeb68d7af527776cbe98cde27a1aeccbd13e818e5b73b454  .agents/vendor-skills/improve/references/closing-the-loop.md
7be76dd3c29442288fa7cee4432fc6e95c2fdbf748c9b356344bf647c5f8722e  .agents/vendor-skills/improve/references/plan-template.md
1316a2f3f95741d2300b116fe0c2d81ce4a9568656ed0a62643f54aaf09957f2  .agents/vendor-skills/ponytail/SKILL.md
```

**Verify**: run the inventory and checksum commands from the command table.
Expected: five upstream files, all five checksums match.

### Step 3: Record provenance and license notices

Create `.agents/skills/PROVENANCE.md` containing:

- A heading explaining these are pinned project-local agent instructions.
- A table with skill name, repository URL, source path, exact commit, and MIT
  license.
- The complete MIT notice from `shadcn/improve` with copyright
  `Copyright (c) 2026 shadcn`.
- The complete MIT notice from `dietrichgebert/ponytail` with copyright
  `Copyright (c) 2026 DietrichGebert`.
- Exact notice boundary markers: `<!-- BEGIN shadcn/improve LICENSE -->` and
  `<!-- END shadcn/improve LICENSE -->`; then
  `<!-- BEGIN dietrichgebert/ponytail LICENSE -->` and
  `<!-- END dietrichgebert/ponytail LICENSE -->`. Put only the corresponding
  notice between each marker pair.
- An update procedure: audit upstream, update commit and checksums in this plan
  or its successor, reinstall the narrow skill path, and review the diff.

Do not claim the repository itself is MIT licensed; the notices apply only to
the vendored skill material.

The Improve notice must be reproduced completely:

```text
MIT License

Copyright (c) 2026 shadcn

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

The Ponytail notice must be reproduced completely:

```text
MIT License

Copyright (c) 2026 DietrichGebert

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

**Verify**:

```sh
rg -q '03369ee6d7cafbfcecc4346539b05b3dc0a603bb' .agents/skills/PROVENANCE.md
rg -q '2ed6c52c9d7e5e56942508591085fd45dea277d3' .agents/skills/PROVENANCE.md
PLAN_IMPROVE_LICENSE="$(sed -n '/^<!-- BEGIN shadcn\/improve LICENSE -->$/,/^<!-- END shadcn\/improve LICENSE -->$/p' .agents/skills/PROVENANCE.md | sed '1d;$d')"
PLAN_PONYTAIL_LICENSE="$(sed -n '/^<!-- BEGIN dietrichgebert\/ponytail LICENSE -->$/,/^<!-- END dietrichgebert\/ponytail LICENSE -->$/p' .agents/skills/PROVENANCE.md | sed '1d;$d')"
test "$(printf '%s\n' "$PLAN_IMPROVE_LICENSE" | shasum -a 256 | cut -d ' ' -f 1)" = 00ff8408e93e2ecc4d90c0edeabdf434100c3aff50acc5d330e17eccd4a3faa5
test "$(printf '%s\n' "$PLAN_PONYTAIL_LICENSE" | shasum -a 256 | cut -d ' ' -f 1)" = fb1bc6909ac3ef82d5c22106e32ef682b0cff66788fa915fb9b53b15c9d2f3ab
```

Expected: every independent check exits 0.

### Step 4: Add deterministic project selectors

Create `.agents/skills/remora-improve/SKILL.md` with exactly this content:

```markdown
---
name: remora-improve
description: Run Remora's pinned Improve advisor workflow for repository audits, implementation plans, isolated execution, and review.
license: MIT
---

# Remora Improve

1. From this selector's directory, read `../../vendor-skills/improve/SKILL.md` completely.
2. Resolve every relative reference against `../../vendor-skills/improve/`, relative to this selector's directory.
3. Follow the vendored skill exactly. This selector adds no behavioral override.
```

Create `.agents/skills/remora-ponytail/SKILL.md` with exactly this content:

```markdown
---
name: remora-ponytail
description: Run Remora's pinned Ponytail anti-slop implementation and review discipline.
license: MIT
---

# Remora Ponytail

1. From this selector's directory, read `../../vendor-skills/ponytail/SKILL.md` completely.
2. Resolve every relative reference against `../../vendor-skills/ponytail/`, relative to this selector's directory.
3. Follow the vendored skill exactly. This selector adds no behavioral override.
```

The wrappers are intentionally project-owned. Do not change either vendored
frontmatter `name`; the vendored files must remain byte-identical.

### Step 5: Document project skill usage

Append this concise section to `AGENTS.md`:

```markdown
## Project Skills

- `$remora-improve` selects the pinned Improve workflow through
  `.agents/skills/remora-improve/`. Its advisor may edit only `plans/`; source
  changes require its isolated executor/review flow.
- `$remora-ponytail` selects the pinned Ponytail discipline through
  `.agents/skills/remora-ponytail/`.
- Do not rely on the machine-global `$improve` or `$ponytail` names for
  repository-deterministic work; duplicate skill names are not merged.
- Keep vendored skill sources pinned and review provenance before updating.
  Do not install their hooks, MCP servers, or global configuration for normal
  repository work.
```

**Verify**:

```sh
rg -q '^## Project Skills$' AGENTS.md
rg -q '\$remora-improve' AGENTS.md
rg -q '\$remora-ponytail' AGENTS.md
rg -q 'isolated executor' AGENTS.md
rg -q 'hooks, MCP servers, or global configuration' AGENTS.md
```

Expected: every independent check exits 0.

### Step 6: Run discovery, inventory, and scope checks

Run:

```sh
PLAN_PROMPT_INPUT="$(codex debug prompt-input '$remora-improve $remora-ponytail' | jq -r '.. | strings')"
PLAN_IMPROVE_ROOT="$(printf '%s\n' "$PLAN_PROMPT_INPUT" | sed -nE 's/^- remora-improve:.*\(file: (r[0-9]+)\/remora-improve\/SKILL[.]md\)$/\1/p')"
PLAN_PONYTAIL_ROOT="$(printf '%s\n' "$PLAN_PROMPT_INPUT" | sed -nE 's/^- remora-ponytail:.*\(file: (r[0-9]+)\/remora-ponytail\/SKILL[.]md\)$/\1/p')"
test -n "$PLAN_IMPROVE_ROOT"
test -n "$PLAN_PONYTAIL_ROOT"
PLAN_SKILLS_ROOT="$(pwd)/.agents/skills"
printf '%s\n' "$PLAN_PROMPT_INPUT" | rg -Fq -- "- \`$PLAN_IMPROVE_ROOT\` = \`$PLAN_SKILLS_ROOT\`"
printf '%s\n' "$PLAN_PROMPT_INPUT" | rg -Fq -- "- \`$PLAN_PONYTAIL_ROOT\` = \`$PLAN_SKILLS_ROOT\`"

for PLAN_TRACKED_FILE in \
    .agents/skills/remora-improve/SKILL.md \
    .agents/skills/remora-ponytail/SKILL.md \
    .agents/skills/PROVENANCE.md \
    .agents/vendor-skills/improve/SKILL.md \
    .agents/vendor-skills/ponytail/SKILL.md; do
    if git check-ignore -q -- "$PLAN_TRACKED_FILE"; then
        exit 1
    fi
done

PLAN_UPSTREAM_FILES="$(find .agents/vendor-skills -type f -print | LC_ALL=C sort)"
PLAN_EXPECTED_UPSTREAM_FILES="$(printf '%s\n' \
    .agents/vendor-skills/improve/SKILL.md \
    .agents/vendor-skills/improve/references/audit-playbook.md \
    .agents/vendor-skills/improve/references/closing-the-loop.md \
    .agents/vendor-skills/improve/references/plan-template.md \
    .agents/vendor-skills/ponytail/SKILL.md | LC_ALL=C sort)"
test "$PLAN_UPSTREAM_FILES" = "$PLAN_EXPECTED_UPSTREAM_FILES"

PLAN_AGENT_FILES="$(find .agents/skills .agents/vendor-skills -type f -print | LC_ALL=C sort)"
PLAN_EXPECTED_AGENT_FILES="$(printf '%s\n' \
    .agents/skills/PROVENANCE.md \
    .agents/skills/remora-improve/SKILL.md \
    .agents/skills/remora-ponytail/SKILL.md \
    .agents/vendor-skills/improve/SKILL.md \
    .agents/vendor-skills/improve/references/audit-playbook.md \
    .agents/vendor-skills/improve/references/closing-the-loop.md \
    .agents/vendor-skills/improve/references/plan-template.md \
    .agents/vendor-skills/ponytail/SKILL.md | LC_ALL=C sort)"
test "$PLAN_AGENT_FILES" = "$PLAN_EXPECTED_AGENT_FILES"

PLAN_CHANGED_FILES="$(git status --short --untracked-files=all | cut -c4- | LC_ALL=C sort)"
PLAN_EXPECTED_CHANGED_FILES="$(printf '%s\n' \
    .agents/skills/PROVENANCE.md \
    .agents/skills/remora-improve/SKILL.md \
    .agents/skills/remora-ponytail/SKILL.md \
    .agents/vendor-skills/improve/SKILL.md \
    .agents/vendor-skills/improve/references/audit-playbook.md \
    .agents/vendor-skills/improve/references/closing-the-loop.md \
    .agents/vendor-skills/improve/references/plan-template.md \
    .agents/vendor-skills/ponytail/SKILL.md \
    .gitignore \
    AGENTS.md | LC_ALL=C sort)"
test "$PLAN_CHANGED_FILES" = "$PLAN_EXPECTED_CHANGED_FILES"
git diff --check
```

Expected: all checks exit 0. The first inventory contains exactly five
byte-identical upstream files; the second contains exactly eight agent files;
the final scope contains exactly the ten in-scope repository files.

Stage the ten explicit paths, inspect `git diff --cached --stat` and
`git diff --cached`, then commit the verified work. After the commit, run:

```sh
test "$(git log -1 --format=%s)" = 'agents: vendor project improvement skills'
PLAN_COMMIT_FILES="$(git diff-tree --no-commit-id --name-only -r HEAD | LC_ALL=C sort)"
PLAN_EXPECTED_COMMIT_FILES="$(printf '%s\n' \
    .agents/skills/PROVENANCE.md \
    .agents/skills/remora-improve/SKILL.md \
    .agents/skills/remora-ponytail/SKILL.md \
    .agents/vendor-skills/improve/SKILL.md \
    .agents/vendor-skills/improve/references/audit-playbook.md \
    .agents/vendor-skills/improve/references/closing-the-loop.md \
    .agents/vendor-skills/improve/references/plan-template.md \
    .agents/vendor-skills/ponytail/SKILL.md \
    .gitignore \
    AGENTS.md | LC_ALL=C sort)"
test "$PLAN_COMMIT_FILES" = "$PLAN_EXPECTED_COMMIT_FILES"
test -z "$(git status --short --untracked-files=all)"
```

Expected: all three checks exit 0.

## Test plan

This is a documentation/instruction-only change; application builds are not
required. The regression risks are discovery, accidental upstream mutation,
and unignoring unrelated agent state.

- `git check-ignore` proves arbitrary `.agents` state remains ignored.
- `git check-ignore` exit 1 for the skill files proves they are trackable.
- SHA-256 checks prove the five upstream files are byte-for-byte pinned.
- File-count and `find` checks prove no Ponytail extras were vendored.
- Independent provenance checks prove both commits and both complete notices are
  present.
- A fresh `codex debug prompt-input` process proves both unique selectors are
  discoverable from the repository root.
- Exact pre-commit and post-commit path-set checks prove scope and commit
  cleanliness.

## Done criteria

- [x] Drift check is empty.
- [x] Exactly five byte-identical upstream skill files are installed.
- [x] Exactly two uniquely named project selectors load the vendored skills.
- [x] Exactly one local provenance file is added.
- [x] Both exact upstream commits and complete MIT notices are recorded.
- [x] Arbitrary `.agents` state remains ignored.
- [x] All eight agent files are trackable.
- [x] `AGENTS.md` documents the project-local usage and Improve source-editing
      restriction.
- [x] No out-of-scope file is modified.
- [x] Commit `agents: vendor project improvement skills` exists in the isolated
      worktree.

## STOP conditions

Stop and report without improvising if:

- The drift check reports changes to `.gitignore`, `AGENTS.md`,
  `.agents/skills`, or `.agents/vendor-skills` after the planned-at commit.
- Either upstream commit or source path no longer resolves.
- Any installed upstream checksum differs from the listed value.
- Either project selector cannot resolve its relative vendored `SKILL.md`.
- Fresh-process discovery does not report either selector through the actual
  repository `.agents/skills` root alias.
- Either provenance notice or provenance commit check fails.
- Any exact upstream, agent-file, changed-file, or committed-file inventory
  differs from the listed set.
- The installer leaves persistent files outside `.agents/vendor-skills`.
  Temporary download/extraction files created and cleaned by the installer are
  allowed.
- Tracking the skill files requires unignoring arbitrary `.agents` state.
- More than the five specified upstream files are required for either core
  skill to parse.
- Any application/runtime dependency appears necessary.
- Any required verification fails twice after correcting only a command typo.
  Do not add global configuration, hooks, or extra files to make a check pass.

## Maintenance notes

- Project skill updates are supply-chain changes. Review upstream diffs before
  changing either pinned commit.
- Codex normally detects project-skill changes automatically. This plan uses a
  fresh CLI process for discovery; restart Codex only as a fallback if the
  expected selector entries are absent.
- The Ponytail repository contains many optional integrations; their absence is
  intentional, not an incomplete install.
- Reviewers should scrutinize `.gitignore` first: the change must expose only
  the two selectors, their provenance, and the two pinned vendor bundles—never
  credentials or local orchestration state.
