# Remora Link host packaging and release strategy

Research date: 2026-07-15
Status: implementation recommendation; no production code changed

## Recommendation

Ship **Remora Link as a standalone Rust daemon**, delivered by a tiny, zero-runtime-dependency npm launcher plus exact-version, platform-specific npm binary packages. The launcher should copy the selected binary into a stable Remora-owned installation directory and delegate persistent supervision to launchd, systemd, or the Windows service manager. It must not keep the daemon inside the transient `npx` cache.

Use npm only as the bootstrap and explicit update channel:

```sh
npx --yes --prefer-online remora-link@latest install
npx --yes remora-link@latest status
npx --yes remora-link@latest pair
npx --yes --prefer-online remora-link@latest upgrade
```

The daemon should discover and launch coding harnesses already installed by the user. It must never install Codex, Claude Code, OpenCode, Pi, Gemini, or another harness as a fallback. Preserve each harness's existing config, auth, and project state.

Fork Alleycat into a Remora-controlled, history-preserving repository and pin one reviewed commit for both the host and Remora's mobile Rust client. Retain `alleycat/1`, `ALLEYCAT_*`, and the v1 wire identifiers only where compatibility requires them. New executable, package, service, config, state, and user-facing names should use Remora Link.

Migrate existing `npx kittylitter` users by **side-by-side re-pairing**. Do not copy Kittylitter's host key or bearer token into Remora Link. Start the new branded host, validate its new pairing, then let the user remove the old pairing and service. This is the clean security and product-identity cutover requested for Remora Link.

Ship public N0 and real self-hosted relay modes first. A Remora-hosted relay is a separate service boundary requiring enrollment, credentials, capacity, privacy, abuse controls, and an operating model; do not imply that setting the current upstream `relay` URL implements it.

## Decision scorecard

Scores are 1–5, higher is better. Weights reflect the requested priorities.

| Option | Startup 20% | Install reliability 25% | Cross-platform release burden 15% | Shared Rust crypto/protocol 25% | Detect/launch installed harnesses 15% | Weighted score |
|---|---:|---:|---:|---:|---:|---:|
| **Standalone Rust daemon + thin npm launcher + platform binary packages** | 5 | 5 | 3 | 5 | 5 | **4.70** |
| Rust daemon embedded as a napi-rs addon | 4 | 4 | 2 | 5 | 4 | 3.95 |
| TypeScript daemon | 3 | 4 | 5 | 1 | 4 | 3.20 |
| Direct Alleycat/Litter fork retaining its postinstall downloader | 5 | 2 | 3 | 4 | 5 | 3.75 |

### Why the standalone Rust daemon wins

- It starts without a resident Node runtime and reuses the Iroh, authentication, framing, reconnection, and harness bridge code already shared with mobile Rust.
- A pure-JavaScript launcher is enough for platform selection and CLI delegation. napi-rs would still require per-platform packages, while adding Node-API lifecycle and process-supervision complexity without a useful in-process API.
- A TypeScript daemon makes packaging superficially simpler, but duplicates the security-critical protocol and state machine that this repository deliberately owns in Rust.
- A direct fork of the current Litter npm wrapper gets native startup and harness support, but inherits an unverified lifecycle downloader, moving upstream dependencies, and the risk of registering a service whose binary lives in an evictable npm cache.

The package layout borrows napi-rs's proven platform-package distribution pattern without embedding the daemon in Node. napi-rs documents both the per-platform package model and the non-transactional nature of multi-package npm publishing ([getting started](https://napi.rs/docs/introduction/getting-started), [release workflow](https://napi.rs/docs/deep-dive/release), [partial-publish recovery](https://napi.rs/docs/cli/pre-publish)).

## Current repository and upstream facts

Remora still directs remote users to `npx kittylitter`, while the shared Rust workspace consumes four Alleycat crates from a moving `main` branch ([Remora README](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/README.md#L64-L68), [Rust dependencies](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/Cargo.toml#L28-L31)). The current lockfile resolves them to `3c6dfe2c6b060864d8cb0fcae58f73a6ed1ea10f`, but `tools/scripts/update-alleycat-main.sh` resolves upstream `main` and mutates the lockfile during ordinary workflows. This is not a release-grade source pin.

Upstream Alleycat already has the right architectural seam: a thin binary supplies an `App` identity while the shared host implements the protocol and harness bridges ([`App` identity](https://github.com/dnakov/alleycat/blob/3c6dfe2c6b060864d8cb0fcae58f73a6ed1ea10f/crates/alleycat/src/lib.rs#L26-L76)). Litter's Kittylitter service is such a wrapper ([wrapper source](https://github.com/dnakov/litter/blob/abee3ace684204a3cbc4ea1e0e903b9f31518dac/services/kittylitter/src/main.rs)).

The current compatibility contract is:

- protocol v1 and ALPN `alleycat/1`;
- a pair payload with host Iroh `node_id`, a global bearer `token`, optional `relay`, and display-only `host_name`;
- a bounded length-prefixed JSON authorization request before switching to the selected harness wire protocol.

Those details are defined upstream in the [pair/request schema](https://github.com/dnakov/alleycat/blob/3c6dfe2c6b060864d8cb0fcae58f73a6ed1ea10f/crates/alleycat/src/protocol.rs#L3-L15) and [request variants](https://github.com/dnakov/alleycat/blob/3c6dfe2c6b060864d8cb0fcae58f73a6ed1ea10f/crates/alleycat/src/protocol.rs#L144-L180), and mirrored by Remora's parser and dialer ([parser](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/codex-mobile-client/src/alleycat.rs#L434-L463), [dialer](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/codex-mobile-client/src/alleycat.rs#L668-L692)).

The current bearer token is host-global, durable until rotation, and compared as a string for every new stream. It is not bound to the authenticated client Iroh endpoint and has no per-device revocation ([host authorization](https://github.com/dnakov/alleycat/blob/3c6dfe2c6b060864d8cb0fcae58f73a6ed1ea10f/crates/alleycat/src/host.rs#L271-L288)). Packaging must not make that weakness harder to remove; pairing v2 should remain a separate security workstream.

The primary compatibility rule from `CONTEXT.md` is correct: keep the wire identifiers while they are needed, but do not carry Alleycat or Kittylitter naming into new product paths and copy.

## Exact npm package design

### Package names

Proposed public packages:

```text
remora-link                         # JavaScript launcher and CLI entry
@remora/link-darwin-arm64          # native executable only
@remora/link-darwin-x64
@remora/link-linux-arm64-gnu
@remora/link-linux-x64-gnu
@remora/link-win32-x64-msvc
```

Live npm queries on 2026-07-15 returned 404 for `remora-link`, `@remora/link`, and `remora-link-host`. That only means they appeared unclaimed at query time. Reserve the selected package and scope before public documentation or CI references them.

Do not advertise Windows ARM64 until a native artifact is built and tested. The current Kittylitter package maps an ARM Windows selection to an x64 artifact; Remora Link should fail clearly on unsupported targets instead.

### Root `package.json`

Use exact optional dependency versions and no lifecycle scripts:

```json
{
  "name": "remora-link",
  "version": "0.1.0",
  "type": "module",
  "bin": { "remora-link": "bin/remora-link.js" },
  "files": ["bin/remora-link.js", "README.md", "LICENSE"],
  "engines": { "node": ">=22" },
  "optionalDependencies": {
    "@remora/link-darwin-arm64": "0.1.0",
    "@remora/link-darwin-x64": "0.1.0",
    "@remora/link-linux-arm64-gnu": "0.1.0",
    "@remora/link-linux-x64-gnu": "0.1.0",
    "@remora/link-win32-x64-msvc": "0.1.0"
  },
  "publishConfig": { "access": "public", "provenance": true }
}
```

Node 22 and 24 are LTS lines as of the research date, while Node 20 and 18 are end-of-life ([Node release status](https://nodejs.org/en/about/previous-releases)). The launcher should support the full tested Node 22+ user range. The narrower Node `>=22.14` and npm `>=11.5.1` requirements apply to trusted-publishing CI, not to the end-user launcher ([trusted publishing requirements](https://docs.npmjs.com/trusted-publishers/)).

Each platform package should contain only the native executable, license/notices, and metadata:

```json
{
  "name": "@remora/link-darwin-arm64",
  "version": "0.1.0",
  "os": ["darwin"],
  "cpu": ["arm64"],
  "files": ["bin/remora-link", "LICENSE", "THIRD_PARTY_NOTICES"],
  "publishConfig": { "access": "public", "provenance": true }
}
```

Linux packages should also set the appropriate `libc` selector. npm documents `os`, `cpu`, `libc`, `optionalDependencies`, `bin`, and `publishConfig` in the [`package.json` reference](https://docs.npmjs.com/files/package.json/).

### Launcher contract

The launcher should use only Node built-ins:

1. Normalize `process.platform`, `process.arch`, and Linux libc to an exact package name.
2. Resolve the leaf package's `package.json`, join the fixed allowlisted `bin/remora-link` or `bin/remora-link.exe` path, and verify the resolved real path stays inside the package directory. Do not use a package-controlled arbitrary command string.
3. Verify the root and platform package versions are identical.
4. For foreground commands, use `spawn`/`execFile` with an absolute executable, an argv array, `shell: false`, inherited stdio, signal forwarding, and the child's exit code.
5. For `install`/`upgrade`, ask the native CLI to atomically copy itself into the stable Remora-owned binary path and register that path with the OS service manager.
6. If optional dependencies were omitted or the platform is unsupported, print an exact remediation command and exit nonzero. Never download an artifact ad hoc.

Node documents that `shell: true` interprets metacharacters and that unsanitized input must not reach a shell; the launcher does not need a shell at all ([`child_process`](https://nodejs.org/api/child_process.html)).

No `preinstall`, `install`, or `postinstall` script should exist. `npx remora-link status` must not mutate the machine. Persistent installation occurs only after the user explicitly runs `install` or confirms an interactive first-run prompt.

## Bootstrap, service installation, and updates

`npm exec`/`npx` downloads missing packages into the npm cache and may otherwise select a local project dependency. Use an explicit package spec and `--yes`: `npx --yes remora-link@latest ...`. Use `--prefer-online` on canonical `install` and `upgrade` commands so npm refreshes channel metadata; exact versions remain the deterministic support and rollback path. These behaviors are documented by [`npm exec`](https://docs.npmjs.com/cli/v11/commands/npm-exec/).

### Stable binary and service paths

The native `install` command should copy the executable to a versioned staging file in the destination filesystem, verify it, fsync as appropriate, rename atomically, then register the stable path. Proposed identity, subject to final reverse-DNS ownership approval:

| Platform | Config | State/data and installed binary | Logs | Service/control identity |
|---|---|---|---|---|
| macOS | `~/Library/Application Support/com.remora.link/host.toml` | same directory, `bin/remora-link`, `host.key`, enrollment state | `~/Library/Logs/com.remora.link/` | launchd `com.remora.link`; Unix socket under a private runtime dir |
| Linux | `${XDG_CONFIG_HOME:-~/.config}/remora-link/host.toml` | `${XDG_STATE_HOME:-~/.local/state}/remora-link/`; binary under `${XDG_DATA_HOME:-~/.local/share}/remora-link/bin/` | state dir or journald | systemd user unit `remora-link.service`; `$XDG_RUNTIME_DIR/remora-link/control.sock` |
| Windows | `%APPDATA%\Remora\Link\config\host.toml` | `%LOCALAPPDATA%\Remora\Link\data\` and `bin\remora-link.exe` | `%LOCALAPPDATA%\Remora\Link\logs\` | per-user Startup shortcut or Scheduled Task plus `remora-link-control-*` named pipe |

Do not point launchd, systemd, or Windows autostart at `~/.npm/_npx/...`. Cache eviction, `npm cache clean`, a Node version-manager change, or package garbage collection would silently break startup. Official npm documentation places `npx`/`npm exec` downloads in the npm cache, while upstream Alleycat's [macOS](https://github.com/dnakov/alleycat/blob/3c6dfe2c6b060864d8cb0fcae58f73a6ed1ea10f/crates/alleycat/src/service/macos.rs#L11-L22), [Linux](https://github.com/dnakov/alleycat/blob/3c6dfe2c6b060864d8cb0fcae58f73a6ed1ea10f/crates/alleycat/src/service/linux.rs#L10-L27), and [Windows](https://github.com/dnakov/alleycat/blob/3c6dfe2c6b060864d8cb0fcae58f73a6ed1ea10f/crates/alleycat/src/service/windows.rs#L1-L25) installers persist `current_exe()`. Running those installers from an npm cache therefore records an evictable path.

For Windows v0.1, use the non-elevated per-user Startup shortcut pattern already proven upstream, optionally upgraded later to a per-user Scheduled Task. Do not silently request administrator rights for a Windows Service during one-command onboarding.

The native CLI owns service lifecycle, so npm/Node is not required for daemon startup or normal supervision after installation:

```text
install     copy atomically, write service definition, start, health-check
status      version, PID, service state, endpoint ID, relay mode, harness health
pair        emit the new pairing offer/QR
upgrade     stage, stop, swap, start, verify, rollback on failure
uninstall   stop and remove service; preserve data unless --purge is explicit
doctor      paths, permissions, executable resolution, relay connectivity
```

Uninstall and `--purge` must be separate. Persistent user state is removed only with explicit confirmation, using platform-appropriate safe deletion.

### Release channels

Use npm dist-tags as channels:

- `latest`: stable only;
- `next`: beta and release candidates;
- exact SemVer: deterministic rollback and support.

npm describes dist-tags as mutable labels that share the package version namespace ([`npm dist-tag`](https://docs.npmjs.com/cli/v11/commands/npm-dist-tag/)). Do not implement an unattended background self-updater in the first release. `remora-link upgrade` should resolve the requested channel explicitly, report old/new versions, install atomically, restart, health-check, and roll back if the daemon fails readiness.

Concretely, npm selects the upgrade artifact: `npx --yes --prefer-online remora-link@latest upgrade` (or an exact version) invokes the packaged native binary, and that binary stages/copies itself and performs the service health-check/rollback. The already-installed daemon must never query npm or download an artifact.

Because npm cannot atomically publish six packages:

1. Build all targets from one protected tag and one locked source tree.
2. Publish platform packages first under a staging tag such as `candidate`.
3. Install and smoke-test each packed/published platform package.
4. Publish the root package last under `candidate`.
5. Promote the exact version of every package to `next` or `latest` only after the matrix passes.
6. If a partial publish occurs, finish the missing immutable version or publish a new patch; never overwrite a published tarball.

## Supply-chain and release security

The published `kittylitter@0.3.4` wrapper uses a postinstall downloader to fetch and extract GitHub release archives. Although its GitHub release publishes checksums and a manifest, the [published npm downloader](https://unpkg.com/kittylitter@0.3.4/binary-install.js) does not verify those files before extraction. npm provenance covers the published npm tarball, not a different GitHub archive downloaded later by a lifecycle script. Remora Link should remove that network-in-lifecycle path entirely.

Required controls:

- Root package: zero runtime dependencies, no lifecycle scripts, exact optional package versions.
- Platform packages: zero dependencies and scripts; only the executable and notices.
- Commit the JavaScript build lockfile and use `npm ci` for development/release tooling.
- Build Rust with `cargo build --locked --frozen` from the exact reviewed fork commit.
- Use npm trusted publishing from GitHub Actions via OIDC; do not store a long-lived npm token. npm documents automatic provenance for trusted publishing from public repositories ([trusted publishing](https://docs.npmjs.com/trusted-publishers/), [provenance](https://docs.npmjs.com/generating-provenance-statements/)).
- Pin third-party GitHub Actions to full commit SHAs; GitHub exposes a repository policy for requiring this ([Actions policy](https://docs.github.com/en/repositories/managing-your-repositorys-settings-and-features/enabling-features-for-your-repository/managing-github-actions-settings-for-a-repository?apiVersion=2022-11-28)).
- Generate GitHub artifact attestations for every native artifact and retain SHA-256 checksums, SBOMs, source commit, Rust toolchain, and target triple in release metadata ([artifact attestations](https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations/use-artifact-attestations)).
- Run `npm audit signatures` against the release installation and verify provenance in CI.
- Sign/notarize the macOS executable before npm packing. Decide Windows code signing before claiming a supported production Windows service.
- Keep release credentials and relay service credentials entirely out of npm tarballs and the open-source binary.

Trusted-publishing bootstrap is package-specific and should be operationally explicit:

1. Manually publish the first version of each of the six packages with a maintainer account protected by 2FA.
2. Configure the exact GitHub owner, repository, workflow filename, and protected environment as the trusted publisher for every package. npm permits one trusted-publisher configuration per package.
3. Run the workflow on Node 22.14+ with npm 11.5.1+, verify OIDC publication and provenance, then revoke any temporary automation token.
4. Set package access to require 2FA and disallow token publication after OIDC is proven.
5. For the strongest posture, require manual approval on the protected release environment; do not let ordinary branch CI publish.

## Harness discovery and launch

### Supported first release

| Harness | Detection and launch | Existing state that must remain authoritative | Recommendation |
|---|---|---|---|
| Codex CLI | Resolve `codex`; use stdio as the cross-platform default, with Unix-domain sockets only on Unix; verify initialize/readiness | `${CODEX_HOME:-~/.codex}`, project `.codex`, auth/proxy environment | First-class; already supported |
| Claude Code | Resolve `claude`; launch structured stream-JSON mode with session ID/resume | Preserve the active `CLAUDE_CONFIG_DIR` root wholesale, macOS Keychain, cwd | First-class; set permission bypass off by default while retaining stdio approval mediation |
| OpenCode | Resolve `opencode`; launch `serve --hostname=127.0.0.1 --port=0 --no-mdns`, capture the reported port, then health-check | XDG paths, `OPENCODE_CONFIG*`, auth state | First-class after current Basic-auth handling and version-gated port discovery are fixed |
| Pi | Resolve configured path, `pi`, then `pi-coding-agent`; launch `--mode rpc` | User environment and Pi home/config | First-class; bridge already present |
| Gemini CLI | Resolve `gemini`; use `gemini -p <prompt> --output-format stream-json` or non-TTY stdin, plus `--resume` where supported | `${GEMINI_CLI_HOME:-~}/.gemini`, project config, saved chats, cwd `.env` behavior | Later bridge; do not claim persistent approval/thread parity yet |

Codex's official app-server documentation describes the protocol and supported transports ([Codex app server](https://developers.openai.com/codex/app-server)); its config reference documents `CODEX_HOME` ([Codex config](https://developers.openai.com/codex/config-reference)). Claude documents its structured CLI modes and configuration-root override ([CLI reference](https://code.claude.com/docs/en/cli-reference), [environment variables](https://code.claude.com/docs/en/env-vars)). OpenCode documents `serve` and its config ([CLI](https://opencode.ai/docs/cli/), [config](https://opencode.ai/docs/config/)); the exact port-output adapter must be version-gated because it is source behavior rather than a stable machine protocol. Gemini documents headless JSONL, session handling, and automatic `.env` loading from the cwd/parents/home ([headless mode](https://geminicli.com/docs/cli/headless/), [sessions](https://geminicli.com/docs/cli/session-management/), [configuration](https://geminicli.com/docs/get-started/configuration/)). Treat Gemini's cwd as a secret-loading boundary, never return loaded environment values to mobile/logs, and never set `GEMINI_CLI_TRUST_WORKSPACE=true` on the user's behalf.

Amp, Factory Droid, Hermes, Devin, Grok, and Shell appear in upstream Alleycat's current manifest, but Remora does not directly depend on all of those bridges. Report them as experimental or unavailable until compatibility tests exist. Raw shell must default off.

### Deterministic resolution

Use this order:

1. Explicit user-configured executable path.
2. Executable found in the daemon's captured/reconstructed user `PATH`.
3. Tool-specific trusted locations, including the Codex desktop bundle.
4. `not found` with diagnostics.

Never fall back to `npx`, `npm`, `bunx`, Homebrew, WinGet, Chocolatey, Scoop, `cargo install`, `curl | sh`, or another installer.

On Unix, accept an executable regular file or a symlink to one and never inject the working directory into `PATH`. On Windows, respect `PATHEXT`; spawn `.exe` directly and use `%ComSpec% /d /c` only when a known `.cmd`/`.bat` wrapper is unavoidable. Preserve `CLAUDE_CODE_GIT_BASH_PATH` when present.

Capture a bounded user environment snapshot during explicit installation and expose `remora-link doctor --refresh-environment`. A background service does not inherit the user's interactive shell environment. Upstream Alleycat reconstructs a login-shell environment and applies mise/direnv overlays ([launch environment](https://github.com/dnakov/alleycat/blob/3c6dfe2c6b060864d8cb0fcae58f73a6ed1ea10f/crates/bridge-core/src/launch_environment.rs)); Remora Link should not execute arbitrary project-local direnv/mise hooks merely to detect tools. If project environment evaluation is supported, run it only for a user-initiated launch in an explicitly trusted working directory, with timeouts and diagnostics.

Probe candidates with null stdin, capped output, a 2–5 second timeout, and a nonmutating version/capability command. Presence does not prove login state. Show `installed`, `auth unknown`, `login required`, `ready`, and `incompatible` as distinct typed states. Claim `login required` or `ready` only when a documented nonmutating auth probe or the first real protocol handshake establishes it.

### Safe subprocess contract

- Use an absolute executable and argv vector; never interpolate project paths, prompts, or user input into a shell command.
- Validate and set the requested cwd so project configuration and session state work.
- Preserve harness config/home overrides, locale, proxy, SSH agent, temp, and XDG variables. Never log or transmit the complete environment.
- Drain stdout/stderr concurrently with line and total-buffer caps.
- Use protocol readiness, not sleeps: Codex initialize or readiness; OpenCode `/global/health`.
- Bind auxiliary harness servers only to loopback. For OpenCode, explicitly pass `--hostname=127.0.0.1 --port=0 --no-mdns`, capture its actual bound port, and then probe `/global/health`; do not use the current Alleycat bind-port-0/drop-listener/spawn-fixed-port sequence, which has a TOCTOU race. Generate a high-entropy `OPENCODE_SERVER_PASSWORD` and implement current Basic authentication rather than relying on old token assumptions ([network resolution](https://github.com/anomalyco/opencode/blob/4394b324c972c17952a3c890c608b71739b343c3/packages/opencode/src/cli/network.ts), [serve output](https://github.com/anomalyco/opencode/blob/4394b324c972c17952a3c890c608b71739b343c3/packages/opencode/src/cli/cmd/serve.ts), [current Alleycat launcher](https://github.com/dnakov/alleycat/blob/3c6dfe2c6b060864d8cb0fcae58f73a6ed1ea10f/crates/opencode-bridge/src/opencode_proc.rs)).
- Track process ownership and kill only processes Remora Link spawned. Use a process group on Unix and a Job Object with kill-on-close on Windows.
- Shutdown deterministically: close protocol stdin or request graceful shutdown, wait a bounded interval, terminate the owned process group/Job Object, force-kill if necessary, and always wait/reap.
- Default to each harness's normal permission/approval behavior. Do not silently add Claude permission bypass, Gemini auto-approval, or equivalent flags.
- Make shared-backend spawn single-flight and bound retries/backoff.

## Migration from `npx kittylitter`

The required migration is a deliberate re-pair, not secret-bearing identity import.

1. Detect an already-installed Kittylitter executable and service without invoking `npx kittylitter` or downloading anything.
2. After independently confirming an existing service/state directory, the installed `kittylitter status --json` may inventory PID, version, node ID, token fingerprint, relay, config path, uptime, and agents. It does **not** report the executable path or service definition, and offline status can initialize state, so inspect launchd/systemd/Windows startup registration and the running process separately ([status implementation](https://github.com/dnakov/alleycat/blob/3c6dfe2c6b060864d8cb0fcae58f73a6ed1ea10f/crates/alleycat/src/cli/status.rs)).
3. Install/start Remora Link under its new service, path, host-key, and token identity. Leave Kittylitter running temporarily.
4. Show the harnesses Remora Link resolved, their absolute paths and versions, and readiness/login state.
5. Generate a new Remora Link QR/pairing offer.
6. On mobile, stage the new pairing separately. Validate connection, agent listing, and one selected-harness handshake before making it primary.
7. Retain the old pairing as rollback until the user confirms the cutover.
8. Forgetting the old pairing must remove both saved metadata and the secure token on iOS and Android. Current saved-server removal paths do not call the existing credential deletion APIs; this must be fixed as part of migration.
9. Stop/uninstall the old service using the already-installed Kittylitter executable. Preserve its config/state by default; offer a separately confirmed purge later.

Do not copy Kittylitter's `host.key`, token, config directory, service definition, or control socket into Remora Link. Existing harness authentication and projects remain available because Remora Link uses the user's real environment and state directories. In-flight sessions and host replay buffers do not migrate.

New Remora Link pairings must write to Remora-named mobile credential namespaces. Keep time-bounded legacy reads and deletion only so the user can roll back to or forget a Kittylitter pairing; do not import legacy credentials into the new pairing.

If the old service points into an expired npm cache and no executable remains, report the exact service/path and provide an explicit platform-specific cleanup procedure. Do not fetch and execute a fresh Kittylitter package merely to uninstall it.

## Dependency pinning and fork maintenance

### Source ownership and exact pins

Create a real, history-preserving `amanthanvi/remora-link` fork of `dnakov/alleycat` with:

```text
origin    amanthanvi/remora-link
upstream  dnakov/alleycat
```

Use a single reviewed fork commit for the host and Remora mobile code. The clearest repository arrangement is:

```text
shared/third_party/alleycat/        # submodule pinned to one exact commit
shared/rust-bridge/Cargo.toml       # path dependencies into that submodule
```

This matches Remora's existing third-party submodule pattern and makes the exact code under review visible. If a submodule is rejected, use the fork's immutable `rev = "<40-hex-sha>"`, never `branch = "main"`. Cargo explains the complementary roles of manifests and committed lockfiles in its [Cargo.toml/Cargo.lock guide](https://doc.rust-lang.org/cargo/guide/cargo-toml-vs-cargo-lock.html).

Required cutover:

- remove moving `branch = "main"` dependencies;
- remove `alleycat-main` refresh from normal build, test, and binding prerequisites;
- commit `Cargo.lock` and require `--locked --offline` or `--locked --frozen` where appropriate;
- provide an explicit `make update-remora-link REV=<sha>` command that updates the one source pin and lockfile in a reviewable change;
- record the fork commit in `remora-link --version`, npm/GitHub release metadata, and SBOM.

### Upstream maintenance

Run a weekly and manually dispatched upstream-sync workflow:

1. Fetch `upstream/main`.
2. Create a branch and PR; never auto-merge.
3. Record old/new upstream SHAs and the merge base.
4. Classify changes to protocol, auth, relay, harness adapters, config/state, dependencies, service installation, and distribution.
5. Rebase or merge according to the fork policy, keeping Remora product identity in the thin wrapper and generic fixes upstreamable.
6. Run Rust unit tests, protocol fixtures, live host/mobile compatibility, harness adapter tests, relay integration, service lifecycle, and all platform package smoke tests.
7. Merge only after explicit review; release only from a later protected tag.

Keep an auditable patch ledger for product-only changes and upstream PR links. Isolate the Iroh 0.98.1 to 1.x upgrade from the initial packaging/fork cutover because it changes relay defaults and dependencies.

Confirm GPL-3.0 notices, source-offer obligations, Remora's additional distribution terms, and product naming before publishing the host packages.

## Relay configuration

### Current gap

Upstream exposes an optional relay URL, but the host always builds `Endpoint::builder(presets::N0)`. The configured URL is only copied into the pair payload if no home relay is available ([endpoint construction](https://github.com/dnakov/alleycat/blob/3c6dfe2c6b060864d8cb0fcae58f73a6ed1ea10f/crates/alleycat/src/host.rs#L21-L58), [payload relay](https://github.com/dnakov/alleycat/blob/3c6dfe2c6b060864d8cb0fcae58f73a6ed1ea10f/crates/alleycat/src/host.rs#L238-L261)). Advertising a custom URL is not the same as connecting the host endpoint to that relay.

Implement one typed endpoint-construction path and make status report both desired and active relay configuration:

```toml
[relay]
mode = "n0"                 # n0 | custom | disabled | managed
urls = ["https://relay.example.com"]
allow_public_fallback = false
```

Semantics:

- `n0`: current Iroh public preset; acceptable for preview/development bootstrap, but public N0 infrastructure is rate-limited and has no Remora production service guarantee.
- `custom`: construct the endpoint with Iroh's custom relay map. A dedicated configuration fails closed unless `allow_public_fallback` is explicitly true.
- `disabled`: relay-disabled/direct-only operation with clear reachability diagnostics. Do not promise LAN reachability until pairing/discovery supplies usable direct addresses; the current v1 payload does not.
- `managed`: reserved for a Remora-operated enrollment/broker contract; do not alias it to N0.

Iroh 0.98.1's endpoint builder exposes custom relay configuration ([Iroh builder docs](https://docs.rs/iroh/0.98.1/iroh/endpoint/struct.Builder.html)). Current Iroh relay software supports open access, endpoint allow/deny lists, shared-token authentication, and HTTP authorization callouts ([relay README](https://github.com/n0-computer/iroh/blob/57fb5c2805d6c64b12765f8ef6efe43efbd03a2a/iroh-relay/README.md)), but the pinned 0.98.1 client does not expose the newer shared-token client API. Initial 0.98 self-hosting must therefore use open access, endpoint allowlisting, or an HTTP authorization policy. Shared-token relays depend on the isolated Iroh upgrade and secure credential provisioning to **both** host and mobile endpoints; a URL in the v1 QR is insufficient and the QR must not carry the shared secret.

Keep v1's singular `relay` field until both mobile platforms support a richer schema. Multiple configured relay regions may be useful, but do not claim multi-relay pairing support before the host advertisement and iOS/Android dial paths implement it.

### Hosted versus self-hosted

Self-hosted mode is an operator feature:

- operator supplies relay URL and access policy;
- Remora Link configures its actual endpoint with that relay;
- status/doctor reports active home relay, authentication failure, direct path, fallback, and latency;
- integration tests cover host plus iOS and Android through the custom relay.

A Remora-hosted relay is a product/service project with additional decisions:

- per-install enrollment and credential issuance, rotation, revocation, and abuse throttling;
- regional capacity, availability objectives, observability, cost limits, and support;
- metadata privacy, retention, deletion, incident response, and legal terms;
- whether managed mode permits public N0 fallback;
- secure client credential delivery without embedding a shared secret in the package.

Iroh relays cannot read the end-to-end encrypted QUIC payload, but they observe connection metadata and can deny or degrade service. Treat them as untrusted transport infrastructure, not as identity or authorization.

## Implementation sequence

Each phase is independently reviewable and avoids mixing protocol redesign with distribution cutover.

1. **Pin and characterize.** Fork Alleycat, add the exact source pin, remove moving-main refresh, and add v1 protocol/harness fixtures. Do not change the wire.
2. **Create the native Remora Link wrapper.** Supply new product/service/path identity, safe defaults, status/doctor output, stable install path, and OS service lifecycle.
3. **Implement real relay modes.** Centralize endpoint construction, add N0/custom/disabled, and verify custom relay on both mobile platforms. Leave hosted mode unavailable.
4. **Build the npm packages.** Zero-dependency launcher, exact optional platform packages, packed-package tests, trusted publishing, provenance, signing, attestations, and staged dist-tags.
5. **Harden harness launch.** Deterministic resolution, environment refresh, typed readiness/login state, safe argv spawning, permission-safe defaults, and OpenCode auth update.
6. **Ship re-pair migration.** Side-by-side detection, staged mobile pairing, rollback, secure credential deletion on forget, and old-service cleanup without downloading Kittylitter.
7. **Pairing v2.** Replace the reusable global token with short-lived enrollment and per-device revocable authorization in its dedicated security project while preserving a time-bounded v1 compatibility path.
8. **Evaluate managed relay.** Only after the hosted service contract, operating budget, privacy model, and credential broker are approved.

## Acceptance tests

### Package and install

- `npm pack --ignore-scripts` for root and every platform package contains only the allowlisted files.
- Assert root has zero runtime dependencies/scripts; platform packages have zero dependencies/scripts; optional dependency versions equal the root version exactly.
- Install on macOS arm64/x64, Linux arm64/x64 glibc, and Windows x64. Unsupported targets fail before execution with no fallback download.
- `--omit=optional` and a missing platform package produce a precise error.
- A root/platform version mismatch fails closed.
- Paths containing spaces and Unicode work.
- Delete the npm cache after `install`; service restart and `status` still work.
- Force a failed upgraded daemon readiness check; the previous binary and service recover automatically.
- Foreground launcher forwards signals and the child's nonzero exit status. Background service exposes PID, version, health, and restart count.

### Supply chain

- Release uses one protected tag and exact fork commit; `cargo metadata --locked --offline` shows no moving branch dependency.
- Every npm tarball has provenance; every native artifact has an attestation, checksum, SBOM, source SHA, target, and toolchain.
- CI verifies signatures/provenance and full-SHA Actions policy.
- A partial multi-package publish drill demonstrates documented recovery without republishing an immutable version.

### Harnesses

- Resolver tests cover explicit paths, service `PATH`, version managers, spaces/Unicode, symlinks, Windows `PATHEXT`, and Codex desktop candidates.
- Instrumented tests assert that detection never invokes a package manager, installer, or network downloader.
- Each adapter distinguishes absent, installed, auth-unknown, login-required, ready, incompatible, timeout, and crashed without overstating what a version probe proves.
- Project cwd/config/auth remain unchanged; secrets are redacted from logs.
- Dangerous approval bypass and raw shell are off by default.
- Owned processes are reaped; attached user processes are never killed.

### Migration

- Run Kittylitter and Remora Link side by side with distinct keys/services/paths.
- Pair Remora Link, validate agent listing and one harness handshake, then promote it without deleting the old pairing.
- Roll back to the old pairing before confirmation.
- Forget the old pairing and verify both metadata and Keychain/encrypted-preferences token are gone on iOS and Android.
- Uninstall the old service through its existing executable; if it is missing, show a non-destructive manual cleanup path.
- Verify no old host secret/token/config was copied.

### Relay

- N0, custom open/allowlisted, disabled, and explicit fallback modes have deterministic status on Iroh 0.98.1; authenticated shared-token mode is gated on the isolated Iroh upgrade and secure host/mobile provisioning.
- A custom relay test proves the host endpoint actually registers there and both iOS and Android connect through it.
- Dedicated custom mode fails closed when the relay is unavailable unless public fallback is explicitly enabled.
- Direct-path upgrade and relay-only operation preserve the authenticated endpoint identity.

## Open decisions

1. Reserve `remora-link` versus a scoped public package before implementation. Recommendation: unscoped `remora-link` for the user command, scoped platform packages internally.
2. Approve the final reverse-DNS product identifier. Recommendation: use a domain the project controls; treat `com.remora.link` above as a placeholder until confirmed.
3. Confirm the first supported target matrix, especially Linux musl and Windows signing. Recommendation: start with the five packages listed above.
4. Decide whether explicit custom-relay configurations may fall back to N0. Recommendation: fail closed by default.
5. Decide whether a managed relay is in product scope. Recommendation: ship only the provider seam until a separate hosted-service proposal is approved.
6. Define the mobile version/removal criteria for v1 global bearer-token compatibility.
7. Confirm host package licensing and notice/source distribution obligations before public release.

## Bottom line

The implementation should be a native Remora Link product, not a Node service and not a renamed, moving Alleycat checkout. A tiny npm launcher preserves the current one-command onboarding experience while the Rust daemon supplies fast startup, shared protocol/security code, native service operation, and reliable harness control. Exact packages, stable install paths, explicit re-pairing, immutable fork pins, and real relay endpoint configuration turn the current prototype distribution into a supportable release system.
