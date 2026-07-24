# Changelog

All notable changes to Steins are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

**This file is load-bearing, not decorative.** `.github/workflows/release.yml`
extracts a version's section verbatim as the body of its GitHub Release, so what
is written here is what users read. Two consequences:

- **Do not hard-wrap entries.** Each bullet and each summary paragraph is one
  physical line, however long. Wrapping renders badly on the Release page.
- **Write release notes, not commit messages.** The audience is someone deciding
  whether to upgrade, not someone reading the diff. Internal refactors, test
  additions, and doc churn do not belong here.

What counts as notable for an analyzer, concretely: a change to which findings are
reported or suppressed, to the surface of a profile, to the exit-code contract, to
the config schema, to the CLI, or to the baseline format. A new true positive is a
feature; a removed false positive is a fix; a finding that starts firing where it
did not before is a **breaking** change for anyone with a green CI, and says so.

Diagnostic **ids** are the contract (ADR-0023) — message wording is not. Reference
findings by id (`call.undefined-function`), never by the sentence they print.

## [Unreleased]

Entries accumulate under this heading as work lands; the `steins-release-prep`
skill seals them into a version section at release time, reconstructing from
`git log` if the discipline slipped.

### Changed

- **Steins is now licensed under Apache-2.0, relicensed from AGPL-3.0-only.** This removes the copyleft obligation entirely: you may embed Steins in a proprietary tool, run it as a hosted service, and redistribute it, without any source-disclosure requirement — and Apache-2.0 adds an express patent grant AGPL-3.0 did not give you. Roadmap gate G3, settled before the first external contribution, so it needed no contributor consent (ADR-0025 amendment).
  - The only artifact ever published under the old terms is the `v0.1.0-rc1` pipeline rehearsal, whose archives carry the AGPL text. Nothing from it should be relied on; use `0.1.0` or later.
  - The `steins-attributes` vocabulary package is unaffected and stays MIT — its licence was never tied to the core's.

## [0.1.0-rc1] - 2026-07-25

A **pipeline rehearsal, not a feature release.** Nothing about the analyzer changed for this tag; it exists to exercise the release workflow end to end for the first time — the tag/version guard, the five-target build matrix, the archive and checksum-sidecar naming, and the Homebrew tap push — before a real `0.1.0` makes those paths irreversible. Install it only to test the install itself; `0.1.0` is the first release intended for use.

### Added

- Prebuilt binaries for five targets: `x86_64`/`aarch64` Linux (glibc), `x86_64` Linux (musl, static), and `x86_64`/`aarch64` macOS, each with a `.sha256` sidecar, and each archive carrying `LICENSE` and `THIRD-PARTY-LICENSES.md` alongside the binary.
- A Homebrew formula in `rigortype/homebrew-tap`, so `brew install rigortype/tap/steins` resolves.

### Notes

- There is no crates.io channel and there will not be one while the parser backend is the rev-pinned Mago fork — crates.io rejects crates with git dependencies. Install from a release archive, from Homebrew, or with `cargo install --git https://github.com/rigortype/steins steins-cli`.
- Windows is not shipped: the PHP sidecar's temp-dir spawn path is unverified there, and a binary that mis-spawns would degrade silently to the sound subset.

[Unreleased]: https://github.com/rigortype/steins/compare/v0.1.0-rc1...HEAD
[0.1.0-rc1]: https://github.com/rigortype/steins/releases/tag/v0.1.0-rc1
