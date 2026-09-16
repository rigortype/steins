# Repository instructions

Use progressive disclosure: match the task to one or more routes, then read
only their linked documents. For implementation work, continue through
affected verification and a diff review. User instructions take precedence over
this file and linked skill guidance.

## Route by task

- **Issue or PR triage:** use `gh` for GitHub Issues and PRs; read
  `docs/agents/issue-tracker.md`. Map triage roles to repository labels with
  `docs/agents/triage-labels.md`.
- **Domain terms, architecture, ADRs, or renames:** read
  `docs/agents/domain.md`. It routes `CONTEXT.md` and the relevant ADRs; update
  the glossary before a rename that changes a domain concept.
- **Verification, CI, or CLI compatibility:** read
  `docs/agents/verification.md` when the task touches CI, docs gates, local-only
  corpus/oracle checks, or command dispatch and exit codes.
- **Generated catalog tables:** read `docs/agents/mining.md` before mining or
  regenerating a table.
- **Profiling or performance:** read `docs/agents/profiling.md` before measuring.
- **Release work:** read `.claude/skills/steins-release-prep/SKILL.md` when
  preparing, tagging, or publishing a version. Pushing a branch or tag requires
  the owner's explicit approval.
- **Stacked PRs:** use `/gh-stack` and read `docs/agents/stacked-prs.md` for the
  repository-specific dependency, adoption, sync, and exit rules.

## Repository guardrail

Keep Rust hand-formatted; do not invoke `cargo fmt`. The policy and its
formatting numbers live in `rustfmt.toml`.
