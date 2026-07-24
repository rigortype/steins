## Agent skills

### Issue tracker

Issues and PRDs live as GitHub Issues on `rigortype/steins`, managed via the `gh` CLI. External PRs are also pulled into the `/triage` queue and treated as feature requests (in-flight PRs from collaborators are excluded). See `docs/agents/issue-tracker.md`.

### Triage labels

Uses the default label vocabulary: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context repo: one `CONTEXT.md` and `docs/adr/` at the repo root. See `docs/agents/domain.md`.

### Release

Cutting a version is PR-gated and tag-driven: `.claude/skills/steins-release-prep/SKILL.md`. There is no crates.io channel — the rev-pinned Mago fork is a git dependency, so the channels are the GitHub Release binaries, the Homebrew tap, and `cargo install --git`. Pushing a branch or a tag needs the owner's explicit approval each time.
