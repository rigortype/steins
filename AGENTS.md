## Agent skills

### Issue tracker

Issues and PRDs live as GitHub Issues on `rigortype/steins`, managed via the `gh` CLI. External PRs are also pulled into the `/triage` queue and treated as feature requests (in-flight PRs from collaborators are excluded). See `docs/agents/issue-tracker.md`.

### Triage labels

Uses the default label vocabulary: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context repo: one `CONTEXT.md` and `docs/adr/` at the repo root. See `docs/agents/domain.md`. The glossary binds: when a rename changes what a concept is called in code, update `CONTEXT.md` first — an entry that lists the new word under _Avoid_ is a decision, not a stale note.

ADRs land under post-hoc ratification: a merged ADR reading `Status: PENDING ratification` is the normal state and never blocks implementation, a release, or a follow-up amendment. The current list is `grep -lri 'pending ratification' docs/adr/`; don't maintain a second copy of it.

### Verification gates

CI runs `test`, `wasm`, `docs`, `licenses` and `fp-gate` (`.github/workflows/ci.yml`). Two things it cannot run for you: the private half of `cargo xtask fp-gate` (the corpus is untracked and machine-local, so a checkout that lacks it silently measures the public packages only) and `cargo xtask nsrt [DIR]` against a local `phpstan-src`. Run both alongside CI rather than as a pre-push gate.

Two gates bite in ways the diff doesn't show: the `docs` job rejects a public doc item that intra-doc-links a private one, and `composer.yml` is path-filtered, so a CLI change that breaks the Composer channel stays green until the next `composer/**` PR — dispatch it manually after touching command dispatch or exit codes.

**Never run `cargo fmt` here.** The tree is hand-formatted, the policy with its numbers is in `rustfmt.toml`, and CI deliberately has no fmt gate — so a routine "tidy" rewrites the tree, nothing catches it, and it buries every later `git blame`.

### Release

Cutting a version is PR-gated and tag-driven: `.claude/skills/steins-release-prep/SKILL.md`. There is no crates.io channel — the rev-pinned Mago fork is a git dependency, so the channels are the GitHub Release binaries, the Homebrew tap, and `cargo install --git`. Pushing a branch or a tag needs the owner's explicit approval each time.

### Stacked pull requests

Use `/gh-stack` autonomously when a change is genuinely stackable: each layer builds on the one below and is meant to land in that order, the bottom merging while the top is still being written. The test is the dependency, and it is read off the code, not the run: work that compiles and reviews on its own gets its own PR (or its own stack) even when it was authored alongside the rest. A stack is kept live with `gh stack sync` and leaves through `gh stack merge`. A GitHub stack refuses a base-branch change, so "rebase the top and close the rest" starts with `gh stack unstack` — the exit #398–#416 took after six days unsynced, which is the case this rule exists to avoid (2026-08-22).

Adopt an existing set of PRs with `gh stack link <PR>…`, never `gh stack init <branches>`: `init` creates branches rather than adopting them when the local names are absent, which is exactly the state after pushing an agent's worktree by full refspec. Order the stack by the real parent-child chain — a mis-ordered stack double-counts the diff of every PR above the break.
