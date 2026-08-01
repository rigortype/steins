---
name: steins-release-prep
description: Release a Steins version end to end — bump the workspace version, seal the changelog, reconcile the docs, run the full verification protocol, then open the release PR, drive CI green, and on the owner's Go merge, tag, and verify the published GitHub Release, binaries and Homebrew tap. Use when the user asks to prepare the next version, cut a release, tag a version, refresh release metadata, or make versioned files consistent before tagging.
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

prepare on a branch → PR → CI green → **owner reviews CHANGELOG + docs** →
merge → tag → Actions builds and publishes the Release → verify the outcome.

Everything from opening the PR to the published Release is this skill's work, not
a hand-off — see [Publish](#publish-pr--ci--merge--tag--release). It runs as one
sequence through **two** approval gates, and stops dead at either.

## Two standing constraints

**Never push without explicit approval — at two gates, and only two.** The
owner's standing directive is that commits stay local until they say otherwise.
This workflow pushes three times (the branch, the merge, the tag) but asks
**twice**, because the second Go is a decision about publishing rather than about
a git command:

- **Gate 1 — open the PR.** Authorises pushing the release branch and creating
  the PR. Nothing is published; this only puts the diff somewhere reviewable.
- **Gate 2 — the owner's Go on the reviewed PR.** Authorises the whole rest of
  the sequence: merge, tag, and everything the tag triggers. Once given, carry it
  through to a verified Release without asking again — re-asking between merge
  and tag strands the repo in a half-published state, which is worse than either
  end of it.

Recorded at `v0.1.1` by owner decision: the skill owns the chain, so a mechanical
step inside an authorised sequence is not a new decision. What still needs a
fresh Go is anything *outside* it — re-tagging, deleting a Release, force-pushing
— and any gate whose preconditions changed after it was given (CI went red, the
diff moved, the version changed). Ask again then; do not infer.

**A release is effectively irreversible.** A GitHub Release can be deleted and a
tag re-cut, but the Homebrew tap commit and anything already downloaded cannot be
recalled, and re-tagging the same version breaks anyone who pinned it. Treat the
tag push as the point of no return.

## There is no crates.io channel — do not try to add one

`cargo publish` **cannot** work here and this is structural, not an oversight:
the parser backend is the rev-pinned Mago fork (ADR-0003/0025), i.e. a `git`
dependency, and crates.io rejects any crate that has one. `publish = false` in
`Cargo.toml` makes that an early, legible error. The four real channels:

1. **GitHub Release binaries** — five targets, each a `.tar.gz` with a `.sha256`
   sidecar: `x86_64`/`aarch64` Linux (glibc), `x86_64` Linux (musl, static), and
   `x86_64`/`aarch64` macOS. Windows is deliberately not shipped (the PHP sidecar
   spawn path is unverified there — the reasoning is in `release.yml`'s header).
2. **Homebrew** — `brew install rigortype/tap/steins`.
3. **Composer** — `composer require --dev typedduck/steins`. Packagist serves a
   PHP shim (`composer/`, ~26KB) that fetches channel 1's archive on first use
   and verifies its `.sha256` sidecar. Nothing here is version-bumped by hand:
   the shim reads the installed version from Composer metadata, and Packagist
   learns the version from the git tag this skill pushes.
4. **`cargo install --git https://github.com/rigortype/steins steins-cli`** — the
   documented fallback for platforms without a prebuilt binary; needs a Rust
   toolchain at the MSRV. Also the only answer for arm64 musl, which has no
   archive at all.

**The Composer channel has a publish-order hazard, and it is the one thing about
it worth remembering.** Packagist sees the new version the moment the tag lands,
but the binaries do not exist until the `binaries` job finishes minutes later —
and that job is `fail-fast: false`, so one target can stay missing much longer.
In that window `composer require` resolves and the first run fails on a missing
asset. The shim's 404 message says so rather than reporting a bare "not found",
but do not announce a release until the assets are actually up.

If a release ever *should* reach crates.io, that is a Mago-fork publishing
decision and an ADR, not a change to this skill.

## Before the first release only

- **ROADMAP gates — both now RESOLVED**, so `v0.1.0` is no longer gate-blocked.
  **G2** (public repos) closed when `rigortype/steins` went public; **G3**
  (license) closed on 2026-07-25 with the relicense to **Apache-2.0**, settled
  before the first external contribution (ADR-0025 amendment). Do not "restore"
  AGPL on the strength of an older document: `docs/notes/20260724-v010-auto-adr-log.md`
  contains a superseded entry deciding the opposite, annotated as reversed. The
  governing record is the ADR-0025 amendment, and `[workspace.package] license`
  is the single source of truth that all eleven crates inherit.
- **Homebrew tap.** The `homebrew` job pushes `Formula/steins.rb` to
  `rigortype/homebrew-tap` (that repo must exist — it is the same tap lisplens
  uses). It needs the repository secret `HOMEBREW_TAP_TOKEN`, a token with
  `contents:write` on the tap (the built-in `GITHUB_TOKEN` cannot reach another
  repo). **If the secret is unset the job skips cleanly** — logs a warning, exits
  0 — so the binary release still succeeds. Wire it up when you want the tap live.
  If the secret is set but *insufficient*, the job fails fast on a preflight
  permission check: the tap is public, so cloning it proves nothing about the
  token. For a **fine-grained PAT** the tap must be in the token's selected
  repositories *and* carry `Contents: read and write`; a classic PAT needs `repo`.
  A trailing newline pasted into the secret also fails. This is what the
  `v0.1.0-rc1` rehearsal caught.
- No other secret is needed; the binaries and the Release use `GITHUB_TOKEN`.
- **`CARGO_REGISTRY_TOKEN` is not used and cannot be** — nothing in this repo runs
  `cargo publish` (see the no-crates.io section below). If it is set, it is inert;
  removing it is one less unused credential on a public repo.

## Update the versioned files

Decide the next version first. Pre-`1.0`, a release that changes which findings
fire is a **minor** bump, not a patch — a green CI going red is breaking in
effect whatever semver says about `0.x`.

**The `0.1.x` preview series overrides that rule, by owner decision (2026-07-26,
at `v0.1.1`).** While Steins is a preview with much still unimplemented, the
series continues through releases that change which findings fire rather than
jumping a minor each time. Do not argue for a minor bump on findings-change
grounds alone during `0.1.x`; ask only if the owner has said the preview phase
is over. What does not relax is the *warning*: with the version number no longer
carrying it, each changelog entry must say plainly that findings can appear or
disappear, and the release summary must repeat it.

- **`Cargo.toml`** — `version` under `[workspace.package]`. That is the single
  source of truth; all eleven members inherit it via `version.workspace = true`,
  so there is exactly one line to edit. `release.yml`'s `guard` job re-checks it
  against the tag.
- **`Cargo.lock`** — run `cargo build` and commit the result. It updates all
  eleven workspace entries at once. The lock is committed and every CI and
  release build uses `--locked`, so a stale lock fails the release build.
- **`THIRD-PARTY-LICENSES.md`** — regenerate with **`cargo xtask licenses`**,
  never with bare `cargo about generate`. Since #45 the xtask post-processes
  cargo-about's output (merging typographic variants, grouping MIT by permission
  text with every copyright notice preserved), so running cargo-about directly
  *reverts* that and corrupts the committed file. `cargo xtask licenses` is also
  exactly what CI's drift guard runs. Unlike the sibling repos, `about.toml` here
  excludes the workspace's own crates, so a version bump alone can never make this
  file stale — but the generator's own output can drift from what is committed
  (it had, at `v0.1.1`), so run it anyway and let `git diff` answer.
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
5. **Classify against the last released version, not against the diff.** This is
   the step the accumulate-as-you-go discipline cannot do for you, because each
   entry was written when its commit landed, without knowing what else the window
   would contain. `Changed` / `Fixed` / `Removed` describe how the **previously
   released** version behaves differently. Something introduced during this same
   unreleased window and then refined during it is, to a user upgrading, simply
   `Added` — the intermediate state never shipped, so nothing changed for them.
   One question per bullet: *did the thing it describes exist in the last
   release?* Check it (`git show vX.Y.Z:path`, `git log vX.Y.Z..HEAD`) rather than
   recalling. If no, fold the bullet into the `Added` entry that introduces the
   feature, describe the **end state**, and delete every "previously X, now Y"
   framing where X never shipped.

   The same trap catches **measured numbers**, and more quietly. A figure written
   mid-window ("goes from 1,702 to 669 lines") is the delta from an intermediate
   state no user ever had. Recompute every before/after number against the last
   tag. At `v0.1.1` the real delta was 1,897 -> 671; the committed entry claimed
   1,702 -> 669, and only re-measuring against `v0.1.0` caught it.

   A bullet can also be **half** wrong: `steins license` and
   `THIRD-PARTY-LICENSES.md` shared one `Changed` entry, but the command was new
   in the window while the file shipped in `v0.1.0` — so it split, the command's
   half moving to `Added` and only the file's half staying a change.
6. Re-read the sealed section as a user deciding whether to upgrade.

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

- **CLI surface** — [`docs/user-manual/01-installation-and-quickstart.md`](../../../docs/user-manual/01-installation-and-quickstart.md)
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
- **`why-effects.md` honesty banner** —
  [`docs/why-effects.md`](../../../docs/why-effects.md) and its Japanese twin
  [`docs/ja/why-effects.md`](../../../docs/ja/why-effects.md) open with a
  `[!NOTE]` banner: the document mixes design goals with shipped behavior, dated
  by owner decision (2026-07-31). Each release, re-audit the document's
  present-tense claims against what actually shipped, refresh the banner's date,
  and keep the two language versions in lockstep — an edit to one without the
  other is drift. When everything the document describes is implemented, remove
  the banner from both files **and delete this bullet plus its checklist line**;
  the audit exists only as long as the banner does.
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
cargo xtask licenses && git diff --exit-code -- THIRD-PARTY-LICENSES.md
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
  It does **not** stay there on its own: eight broken links had accumulated by
  `v0.1.1` because no ordinary CI job runs this. Expect to fix some, and budget
  for it rather than treating a red rustdoc as a surprise.
- **`cargo fmt` is not part of this** — the tree is hand-formatted and running it
  would rewrite 110 files. See `rustfmt.toml`.
- **Smoke the binary the way the release will**, from a directory with no project
  and no `steins.toml` — this is the Homebrew formula's test and the release
  workflow's smoke step:

  ```bash
  cd "$(mktemp -d)" && steins doctor --no-php
  ```
- **Smoke the Composer channel** if anything under `composer/`, `composer.json`
  or `.gitattributes` changed since the last release. Its CI is path-filtered,
  so a release that touched none of them has already been covered — but a
  release that did must not be the first place the shim runs:

  ```bash
  composer/tests/package-contents.sh HEAD
  php composer/tests/target-detection.php && php composer/tests/checksum.php
  composer/tests/smoke.sh "$(gh release list --limit 1 --json tagName --jq '.[0].tagName')"
  ```

  The last one installs the package into a scratch project and runs the binary
  it fetches, against the *previous* release — the version being prepared has no
  binaries yet, which is the whole reason the fixture takes a tag.

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

## Publish: PR → CI → merge → tag → Release

One sequence, owned end to end. Do not stop between steps 4 and 7 — an authorised
publish left half-done is worse than one not started.

**Step 1 — push the branch (Gate 1).** Ask, then:

```bash
git push -u origin release/vx.y.z
```

**Step 2 — open the PR.**

```bash
gh pr create --base master --title "Release vx.y.z" --body "Release vx.y.z. Publishing is triggered by the vx.y.z tag after this merges. Review focus: the CHANGELOG.md [x.y.z] section — it becomes the GitHub Release body verbatim — and the docs diff. Approve to publish."
```

**Step 3 — drive CI to green *before* asking the owner to look.** Their review is
the expensive step; do not spend it on a PR that CI will reject anyway.

```bash
gh pr checks --watch
```

Red is yours to fix, not to report: push the fix to the branch and re-watch. The
local protocol and CI overlap but are not identical — CI also runs the
`THIRD-PARTY-LICENSES.md` drift guard and the relicensing gate, and it runs on
Linux, so a macOS-only local pass can still fail there. If a check is red for a
reason the release cannot fix (an outage, a flake), say which check and why
rather than asking for the Go over it.

**Step 4 — the owner reviews, and gives the Go (Gate 2).** Hand them the PR link
and name what to look at: the rendered `CHANGELOG.md [x.y.z]` section, because it
becomes the Release body verbatim, and the docs diff. State that CI is green and
that their Go covers merge, tag, and publish.

**Step 5 — merge.** `--rebase` because this repo's history is strictly linear —
160 commits, zero merge commits — so a merge commit would be the anomaly:

```bash
gh pr merge --rebase && git checkout master && git pull --ff-only
```

**Step 6 — tag, which publishes.** This is the point of no return:

```bash
git tag vx.y.z && git push origin vx.y.z && gh run watch
```

**Step 7 — verify the outcome** (next section). The sequence is not finished when
the workflow goes green; it is finished when the assets exist and a downloaded
binary runs.

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
the archive uploads into the existing Release, no re-tag needed. Note that
Packagist is already serving the new version at this point, so a partial asset
set is a *broken Composer install* for whichever platform is missing, not merely
an incomplete releases page. Prioritize accordingly.

Then confirm the Composer channel resolves the new version and runs it:

```bash
cd "$(mktemp -d)" && composer require --dev typedduck/steins:x.y.z && ./vendor/bin/steins doctor --no-php
```

If Packagist has not picked up the tag within a few minutes, its GitHub webhook
is the thing to check — the release itself is unaffected.

## If something goes wrong

- **guard failed** — nothing was published. Delete the tag
  (`git push --delete origin vx.y.z`), fix the mismatch, re-tag.
- **A binary target failed** — see above; re-run the job, do not re-tag.
- **The tap push failed but binaries succeeded** — the Release is fine and users
  can install from it. Fix the token and re-run the `homebrew` job alone.
- **A bad release is already out** — do not re-cut the same version. Fix forward
  with `x.y.z+1`; re-tagging a published version breaks every pin.

## Quick checklist

- `[Unreleased]` reconstructed if it
  had drifted; every bullet classified and, if commit-style, rewritten — no
  bullet with two sentences, an internal-only detail, or a merge artefact.
- Every bullet classified **against the last release**: nothing under `Changed` /
  `Fixed` / `Removed` that did not exist in it, no "previously X" where X never
  shipped, and every before/after number re-measured against the tag.
- Findings referenced by **id**, not by message text; any newly-firing finding
  flagged as breaking.
- Docs reconciled: quickstart's usage block matches the real binary, the install
  sections name the channels this release creates, `not-implemented.md` no longer
  lists what shipped, ROADMAP milestone moved.
- `why-effects.md` banner audited (en **and** ja): claims re-checked against what
  shipped, banner date refreshed — or banner and audit removed if everything the
  document describes now exists.
- `[workspace.package] version` and all eleven `Cargo.lock` entries equal `x.y.z`.
- `THIRD-PARTY-LICENSES.md` regenerated if dependencies changed (`git diff` is
  the arbiter); `cargo deny check licenses` clean.
- Full verification protocol green, with **fp-gate run in the foreground** and
  phpdoc-oracle confirmed to have actually run rather than skipped.
- Composer channel smoked if `composer/`, `composer.json` or `.gitattributes`
  changed — its CI is path-filtered and will not have covered a release that
  touched them only in this branch.
- CI driven to green on the PR **before** the owner was asked to review, and any
  red fixed on the branch rather than reported upward.
- The change landed via a **release PR approved by the owner**; Gate 1 and Gate 2
  each had an explicit Go, and nothing outside that sequence was pushed; the
  commit message is `Bump up version to x.y.z`.
- The sequence was carried from the owner's Go through to a **verified** Release
  without stopping in between — never merged-but-untagged, never tagged with the
  outcome unchecked.
- After publish: ten assets on the Release, a downloaded binary runs, the tap
  formula carries the new version and real sha256s, and `composer require --dev
  typedduck/steins:x.y.z` resolves and runs in a scratch directory.
