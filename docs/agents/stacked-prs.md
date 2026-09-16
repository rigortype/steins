# Stacked pull requests

Use this guide when a change is being considered for a dependent PR stack.
`/gh-stack` owns the command syntax; this file records the repository policy.

## Choose a stack by dependency

Stack only when each layer depends on the one below and is intended to land in
that order. The dependency is read from the code: a change that compiles and
reviews independently gets its own PR even if it was authored alongside other
work.

Keep a live stack synchronized with `gh stack sync`; leave it with
`gh stack merge`.

## Adopt and exit safely

Adopt existing PRs with `gh stack link <PR>…`, not `gh stack init <branches>`.
`init` creates branches when local names are absent, while an agent's pushed
worktree commonly leaves exactly that state. Order the stack by the real
parent-child chain; a misplaced base causes every PR above it to double-count
the diff.

GitHub stacks refuse a base-branch change. Before rebasing the top and closing
the rest, run `gh stack unstack`.
