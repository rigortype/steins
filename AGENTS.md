# Repository instructions

Each route below names the document that holds this repository's non-obvious
rules for one kind of task. Read it when the task matches its trigger.

## Route by task

- **Triage** of issues or PRs: `docs/agents/issue-tracker.md`, and
  `docs/agents/triage-labels.md` for the label strings.
- **Domain vocabulary**, architecture, ADRs, or a rename:
  `docs/agents/domain.md`.
- **Verification** of a change to CI workflows, rustdoc, inference
  compatibility, or command dispatch and exit codes:
  `docs/agents/verification.md`. Several gates here pass without exercising
  what changed.
- **Mining** — running a `mine-*` xtask or editing a `*-mining/*.toml` table:
  `docs/agents/mining.md`.
- **Performance** — before profiling, or before proposing an optimization:
  `docs/agents/profiling.md`. It carries the current baseline and what that
  baseline rules out.
- **Worktrees** — before `git worktree add`, or before removing one:
  `docs/agents/worktrees.md`. It carries what a worktree costs and the clone
  that keeps the cost off the disk. Don't `cargo build --release` in one.
- **Release** — a version bump, changelog seal, or version tag:
  `.claude/skills/steins-release-prep/SKILL.md`. It owns the push approval
  gates for releases.
- **Stacking** dependent PRs: `/gh-stack` for commands,
  `docs/agents/stacked-prs.md` for when to stack and how to adopt or exit.

## Formatting

Rust here is hand-formatted to the policy in `rustfmt.toml`; match the
surrounding code by hand and never run `cargo fmt`. CI has no fmt gate, so a
tree-wide reformat lands unnoticed and buries every later `git blame`.
