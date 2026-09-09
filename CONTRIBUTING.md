# Contributing

## Project status

Remora is an independently maintained greenfield fork. Internal interfaces may
change, and maintainer-directed refactors can replace upstream behavior.
Keep the supported product scope in [CONTEXT.md](CONTEXT.md) and preserve the
license and third-party attribution.

## Before you open a PR

- Discuss non-trivial work with the maintainer first. An existing request is
  enough; it does not need a duplicate issue.
- Explain the problem and keep unrelated changes separate.
- Match existing code style and put shared runtime behavior in Rust.
- Review upstream updates individually against Remora's identity, pairing
  contract, and mobile parity. A bulk merge is not an update policy.

## Things that will not be merged

- Large refactors not requested by a maintainer.
- Cosmetic churn without a maintenance or user-facing benefit.
- New features without prior discussion in an issue.
- PRs that depend on other unmerged PRs.
- Anything that breaks parity between iOS and Android without a clear reason.

## Setup

See [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) for prerequisites and build commands, and [AGENTS.md](AGENTS.md) for repo conventions.
