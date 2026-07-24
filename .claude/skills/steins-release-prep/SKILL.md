---
name: steins-release-prep
description: Prepare a Steins release through a review PR — bump the workspace version, seal the changelog, reconcile the docs, run the full verification protocol, open a release PR for the owner to approve, then merge and tag so GitHub Actions builds the five release binaries and updates the Homebrew tap. Use when the user asks to prepare the next version, cut a release, tag a version, refresh release metadata, or make versioned files consistent before tagging.
metadata:
  internal: true
---

# Steins Release Prep

Follow this workflow to release a new `steins` version. One `vX.Y.Z` tag drives
[`release.yml`](../../../.github/workflows/release.yml), which builds five
platform binaries, creates the GitHub Release from `CHANGELOG.md`, and updates
the Homebrew tap.

**The flow is PR-gated.** You prepare the release on a branch (version bump,
changelog seal, docs reconcile, verification), open a **release PR** so the owner
can review the `CHANGELOG.md` and docs diffs, and only on their Go do you merge
and tag. At a glance:

prepare on a branch → **PR (owner reviews CHANGELOG + docs)** → merge → tag →
Actions builds and publishes → verify the outcome.

## Two standing constraints

**Never push without explicit approval.** The owner's standing directive is that
commits stay local until they say otherwise. This workflow pushes three times —
the branch, the merge, and the tag — and each one needs its own Go. Ask; do not
infer approval from a previous step.

**A release is effectively irreversible.** A GitHub Release can be deleted and a
tag re-cut, but the Homebrew tap commit and anything already downloaded cannot be
recalled, and re-tagging the same version breaks anyone who pinned it. Treat the
tag push as the point of no return.

## There is no crates.io channel — do not try to add one

`cargo publish` **cannot** work here and this is structural, not an oversight:
the parser backend is the rev-pinned Mago fork (ADR-0003/0025), i.e. a `git`
dependency, and crates.io rejects any crate that has one. `publish = false` in
`Cargo.toml` makes that an early, legible error. The three real channels:

1. **GitHub Release binaries** — five targets, each a `.tar.gz` with a `.sha256`
   sidecar: `x86_64`/`aarch64` Linux (glibc), `x86_64` Linux (musl, static), and
   `x86_64`/`aarch64` macOS. Windows is deliberately not shipped (the PHP sidecar
   spawn path is unverified there — the reasoning is in `release.yml`'s header).
2. **Homebrew** — `brew install rigortype/tap/steins`.
3. **`cargo install --git https://github.com/rigortype/steins steins-cli`** — the
   documented fallback for platforms without a prebuilt binary; needs a Rust
   toolchain at the MSRV.

If a release ever *should* reach crates.io, that is a Mago-fork publishing
decision and an ADR, not a change to this skill.

## Before the first release only

- **ROADMAP gates.** M3 ("Checker release 0.1") is blocked by design on gates
  **G2** (public repos) and **G3** (license, ADR-0025) — both decided by the
  owner, not by you. Confirm both are resolved before preparing `v0.1.0`. If they
  are not, stop and say so; everything else in this file is premature.
- **Homebrew tap.** The `homebrew` job pushes `Formula/steins.rb` to
  `rigortype/homebrew-tap` (that repo must exist — it is the same tap lisplens
  uses). It needs the repository secret `HOMEBREW_TAP_TOKEN`, a token with
  `contents:write` on the tap (the built-in `GITHUB_TOKEN` cannot reach another
  repo). **If the secret is unset the job skips cleanly** — logs a warning, exits
  0 — so the binary release still succeeds. Wire it up when you want the tap live.
- No other secret is needed; the binaries and the Release use `GITHUB_TOKEN`.

## Update the versioned files

Decide the next version first. Pre-`1.0`, a release that changes which findings
fire is a **minor** bump, not a patch — a green CI going red is breaking in
effect whatever semver says about `0.x`.

- **`Cargo.toml`** — `version` under `[workspace.package]`. That is the single
  source of truth; all eleven members inherit it via `version.workspace = true`,
  so there is exactly one line to edit. `release.yml`'s `guard` job re-checks it
  against the tag.
- **`Cargo.lock`** — run `cargo build` and commit the result. It updates all
  eleven workspace entries at once. The lock is committed and every CI and
  release build uses `--locked`, so a stale lock fails the release build.
- **`THIRD-PARTY-LICENSES.md`** — regenerate **only if dependencies changed**:
  `cargo about generate about.hbs -o THIRD-PARTY-LICENSES.md`. Unlike the sibling
  repos, `about.toml` here excludes the workspace's own crates, so a version bump
  alone can never make this file stale. Run it anyway and let `git diff` answer —
  CI's drift guard fails the PR if it disagrees.
- **`CHANGELOG.md`** — seal `[Unreleased]` into the new version section (below).

### Seal the `[Unreleased]` entries — the load-bearing step

The highest-value, most-skipped part of a release, and the one no test can check.
`release.yml` extracts this section **verbatim** as the GitHub Release body, so it
is what users actually read. It is also the review surface the release PR exists
for.

1. **If `[Unreleased]` is empty or thin, reconstruct it first.** Entries are meant
   to accumulate as work lands; that discipline slips. Run
   `git log <last-tag>..HEAD --oneline` (for the first release, the whole
   history), and derive the user-facing changes from the commits, their PRs, and
   the ADRs they implement. ADR titles are usually the right altitude for a
   changelog bullet — they name the decision, not the diff.
2. Read the whole block and classify each top-level bullet: release-style (leave)
   or commit-style (rewrite).
3. Rewrite every commit-style bullet — one self-contained sentence per bullet;
   push "why / how / measured numbers" into a child item (`  - …`); delete
   internal-only detail (private refactors, test additions, doc churn) outright.
   Ask of each entry: *would someone running `steins check` on their code notice?*
4. Consolidate several commits into one user-recognisable change; split merge
   artefacts.
5. Re-read the sealed section as a user deciding whether to upgrade.

**What is notable for an analyzer**, concretely — this is the filter that matters:

- a change to **which findings are reported or suppressed**, by id;
- a change to a **profile's surface**, the **exit-code contract** (ADR-0050 §7),
  the **config schema** (ADR-0023), the **CLI**, or the **baseline format**;
- a **new true positive** is a feature; a **removed false positive** is a fix;
- a finding that **starts firing where it did not before** is a breaking change
  for anyone with a green CI, and must say so plainly.

Reference findings by **id** (`call.undefined-function`) — ids are the contract
(ADR-0023); the message wording is not, and quoting it dates the entry.

### Release mechanics

- Add `## [x.y.z] - YYYY-MM-DD` immediately below `## [Unreleased]`, optionally
  opening with a 2–4 sentence prose summary of the release's themes.
- Keep a Changelog headings verbatim: `Added`, `Changed`, `Deprecated`,
  `Removed`, `Fixed`, `Security`. Group like changes; no `####` inside a version
  block.
- **Do not hard-wrap entries.** Each bullet and the summary paragraph is a single
  physical line, however long — the Release body is this text verbatim, and
  wrapping degrades it there.
- Update the bottom-of-file links: point `[Unreleased]` at `compare/vx.y.z...HEAD`
  and add `[x.y.z]: https://github.com/rigortype/steins/releases/tag/vx.y.z`.

## Reconcile the docs

`README.md` is the repository's first impression and the docs are the install
path M3 promises. They drift as features land. Reconcile against the **sealed
changelog** and the **actual binary**, and fold fixes into the release branch so
they are part of the reviewed diff.

- **CLI surface** — [`docs/guide/quickstart.md`](../../../docs/guide/quickstart.md)
  reproduces the usage block verbatim. Check it against what the binary prints
  when run with no arguments (`./target/release/steins`), not against memory. Every
  subcommand and flag it exposes should appear, and none that it does not.
- **Install** — quickstart and
  [`docs/handbook/01-getting-started.md`](../../../docs/handbook/01-getting-started.md)
  currently say only `cargo install --path .`. From the first tagged release
  onward they must also name the real channels: the Release binaries, Homebrew,
  and `cargo install --git`. This is the single most likely doc to be wrong after
  a release — it is the one the release *creates*.
- **Honest gap list** —
  [`docs/type-specification/not-implemented.md`](../../../docs/type-specification/not-implemented.md):
  anything this release implemented must come off it. A stale gap list is worse
  than none, because it is load-bearing for the zero-FP claim's credibility.
- **ROADMAP** — [`docs/ROADMAP.md`](../../../docs/ROADMAP.md): move the milestone
  this release completes, and check its exit criteria actually hold rather than
  assuming.
- **README** — the docs index still resolves; the pitch matches what ships.

If nothing changed, say so and move on — but actually look; doc drift is silent.

## Verify the release

The repo's **standard verification protocol**, plus the packaging checks:

```bash
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --release --locked
cargo xtask fp-gate
cargo xtask phpdoc-oracle --check
cargo deny check licenses
cargo about generate about.hbs -o THIRD-PARTY-LICENSES.md && git diff --exit-code -- THIRD-PARTY-LICENSES.md
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --locked
git diff --check
```

Reading the results:

- **fp-gate** is the zero-false-positive bar (ADR-0013/0026) and it runs in the
  **foreground** for a release — never backgrounded and skimmed. Any finding on
  clean corpus code blocks the release. Run `cargo xtask corpus-sync` first if
  the corpus is absent. Private corpora enter through `corpus.local.toml` and
  stay outside the repo.
- **phpdoc-oracle** is the conformance rerun; it needs `php` + `composer` + the
  harness `vendor/`. It **succeeds without them** by design, so confirm from its
  output that it actually ran rather than skipped.
- **rustdoc is a clean gate** — `-D warnings` means a single broken intra-doc link
  fails, and blocks the release. It reached zero from a ratchet over 18 links and
  is meant to stay there: fix the link, or reword the reference to plain backticks
  when the target is genuinely private. Never reintroduce a cap or an `#[allow]`.
- **`cargo fmt` is not part of this** — the tree is hand-formatted and running it
  would rewrite 110 files. See `rustfmt.toml`.
- **Smoke the binary the way the release will**, from a directory with no project
  and no `steins.toml` — this is the Homebrew formula's test and the release
  workflow's smoke step:

  ```bash
  cd "$(mktemp -d)" && steins doctor --no-php
  ```

## Rehearse the first release with a release candidate

**Read this before cutting `v0.1.0`.** This pipeline has never run end to end —
no tag has ever been pushed in this repo — and parts of it cannot be exercised
locally at all: cross-compilation for `aarch64` Linux and `musl`, the archive and
sidecar naming, and the tap push. The first real tag would be the first test.

A release candidate rehearses all of it for real, at no risk to the version
number. Semver pre-release identifiers are valid in `Cargo.toml`, the tag glob
`v*.*.*` matches them, and `guard` compares the two literally — so an RC needs no
workflow changes, only consistency:

- set `[workspace.package] version = "0.1.0-rc1"` and rebuild so `Cargo.lock` follows;
- add a `## [0.1.0-rc1]` section to `CHANGELOG.md` (it can be one line);
- tag `v0.1.0-rc1` and push it.

Watch the run, then check the assets, install one archive by hand, and confirm the
tap formula. Delete the RC release and tag afterwards if you prefer a clean list.
Then bump to `0.1.0`, seal the real changelog section, and tag for real.

An RC is cheap insurance precisely because a broken *real* release cannot be
re-cut under the same version.

## Prepare on a branch and commit

Work on a release branch off up-to-date `master` — never bump on `master`
directly; the point is that the change lands via the reviewed PR.

```bash
git checkout master && git pull --ff-only && git checkout -b release/vx.y.z
```

One release-prep commit carrying the `Cargo.toml` bump, the `Cargo.lock` bump,
the `CHANGELOG.md` seal, and any docs reconcile edits:

```text
Bump up version to x.y.z
```

Commit unrelated cleanup separately — do not fold it into the version bump.

## Open the release PR — the review gate

**Ask for approval before this push** (the standing directive above). Then:

```bash
git push -u origin release/vx.y.z
```

```bash
gh pr create --base master --title "Release vx.y.z" --body "Release vx.y.z. Publishing is triggered by the vx.y.z tag after this merges. Review focus: the CHANGELOG.md [x.y.z] section — it becomes the GitHub Release body verbatim — and the docs diff. Approve to publish."
```

Then **stop and hand off**. The owner reviews the rendered `CHANGELOG.md` and doc
diffs and gives the Go. Do not merge on your own initiative — this approval is the
irreversible-publish gate. Make sure every CI check is green before asking.

## Merge, then tag to publish

Only after the PR is **approved and its CI is green**, and with explicit approval
for each push. `--rebase` because this repo's history is strictly linear — 160
commits, zero merge commits — so a merge commit would be the anomaly:

```bash
gh pr merge --rebase && git checkout master && git pull --ff-only
```

```bash
git tag vx.y.z && git push origin vx.y.z && gh run watch
```

The tag triggers `release.yml`:

1. **guard** — the tag matches `[workspace.package] version`, and `CHANGELOG.md`
   has a non-empty section for it. Both fail *before* anything is published.
2. **release** — creates the GitHub Release from that changelog section.
3. **binaries** — five targets in parallel (`fail-fast: false`), each uploading
   `steins-vx.y.z-<target>.tar.gz` plus a `.sha256` sidecar, with `LICENSE`,
   `README.md`, and `THIRD-PARTY-LICENSES.md` inside the archive. Natively
   runnable rows smoke-test with `steins doctor --no-php`.
4. **homebrew** — fills `.github/homebrew/steins.rb.tmpl` from the sidecars and
   pushes it to the tap; skips cleanly if `HOMEBREW_TAP_TOKEN` is unset, and
   refuses to push a formula with an unfilled placeholder.

## After publish — verify the outcome

Do not report success from a green workflow alone; check the artifacts:

```bash
gh release view vx.y.z --json assets --jq '.assets[].name'
```

Expect ten assets — five archives and five `.sha256` sidecars. Then confirm a
downloaded binary actually runs, and, if the tap is live, that the formula moved:

```bash
gh api repos/rigortype/homebrew-tap/contents/Formula/steins.rb --jq '.content' | base64 -d | grep -E 'version|sha256'
```

If a target failed while others succeeded, the Release exists with a **partial**
asset set. Fix the target and re-run just that job (`gh run rerun --job <id>`) —
the archive uploads into the existing Release, no re-tag needed.

## If something goes wrong

- **guard failed** — nothing was published. Delete the tag
  (`git push --delete origin vx.y.z`), fix the mismatch, re-tag.
- **A binary target failed** — see above; re-run the job, do not re-tag.
- **The tap push failed but binaries succeeded** — the Release is fine and users
  can install from it. Fix the token and re-run the `homebrew` job alone.
- **A bad release is already out** — do not re-cut the same version. Fix forward
  with `x.y.z+1`; re-tagging a published version breaks every pin.

## Quick checklist

- Gates G2/G3 resolved (first release only); `[Unreleased]` reconstructed if it
  had drifted; every bullet classified and, if commit-style, rewritten — no
  bullet with two sentences, an internal-only detail, or a merge artefact.
- Findings referenced by **id**, not by message text; any newly-firing finding
  flagged as breaking.
- Docs reconciled: quickstart's usage block matches the real binary, the install
  sections name the channels this release creates, `not-implemented.md` no longer
  lists what shipped, ROADMAP milestone moved.
- `[workspace.package] version` and all eleven `Cargo.lock` entries equal `x.y.z`.
- `THIRD-PARTY-LICENSES.md` regenerated if dependencies changed (`git diff` is
  the arbiter); `cargo deny check licenses` clean.
- Full verification protocol green, with **fp-gate run in the foreground** and
  phpdoc-oracle confirmed to have actually run rather than skipped.
- The change landed via a **release PR approved by the owner**; no push happened
  without its own explicit Go; the commit message is `Bump up version to x.y.z`.
- After publish: ten assets on the Release, a downloaded binary runs, and the tap
  formula carries the new version and real sha256s.
