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

Nothing released yet. Steins is pre-`0.1.0`: the version in `Cargo.toml` is
`0.0.0`, no tag exists, and the ROADMAP gates the first tagged release (M3) on
the user-decided license and public-repo gates.

Entries accumulate under this heading as work lands; the `steins-release-prep`
skill seals them into a version section at release time, reconstructing from
`git log` if the discipline slipped.

[Unreleased]: https://github.com/rigortype/steins/commits/master
