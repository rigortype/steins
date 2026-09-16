# Issue tracker: GitHub

Issues and PRDs for this repo live as GitHub issues on `rigortype/steins`, managed with the `gh` CLI. Outside a clone of the repository (a scratch directory, say), `gh` cannot infer the repository and fails; pass `--repo rigortype/steins`.

## Pull requests as a triage surface

**PRs as a request surface: yes.** External PRs are treated as feature requests and run through the same labels and states as issues, using the `gh pr` equivalents. For the triage queue, list open PRs with `authorAssociation` and keep only `CONTRIBUTOR`, `FIRST_TIME_CONTRIBUTOR`, or `NONE`; in-flight PRs from `OWNER`, `MEMBER`, or `COLLABORATOR` are not triage items.

GitHub shares one number space across issues and PRs, so a bare `#42` may be either — resolve with `gh pr view 42` and fall back to `gh issue view 42`.

## When a skill says "publish to the issue tracker"

Create a GitHub issue.

## When a skill says "fetch the relevant ticket"

Run `gh issue view <number> --comments`.
