# Stacked pull requests

Use this guide when a change is being considered for a dependent PR stack.
`/gh-stack` owns the command syntax; this file records the repository policy.

## Choose a stack by dependency

Stack without asking when each layer depends on the one below and is intended
to land in that order, the bottom merging while the top is still being written.
The dependency is read from the code, not from how the work was authored: a
change that compiles and reviews independently gets its own PR even if it was
written alongside the rest.

Keep a live stack synchronized with `gh stack sync` and leave it with
`gh stack merge`. #398–#416 sat unsynced for six days and had to leave by
unstacking instead.

## Adopt and exit safely

Adopt existing PRs with `gh stack link <PR>…`. `gh stack init <branches>`
creates branches instead of adopting them when the local names are absent,
which is exactly the state an agent's worktree leaves after a full-refspec push.
Order the stack by the real parent-child chain; a misplaced base makes every PR
above it double-count the diff.

GitHub stacks refuse a base-branch change, so "rebase the top and close the
rest" starts with `gh stack unstack`.
