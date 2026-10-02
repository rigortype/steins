---
name: steins-run
description: Orchestrate a Steins run — a theme landed as several PRs, designed with the architect agent, implemented by delegated agents and merged after adversarial review. Use for a redesign, a multi-issue fix, or a request to plan and land work autonomously.
---

# Steins run

A **run** is one theme landed as a sequence of **slices**, each slice one PR.
You are the orchestrator: you decide, audit, push and merge; agents diagnose,
implement and review. The owner's standing delegation covers design decisions
unless the brief says otherwise; record each decision where the owner can
ratify it later (issue, ADR marked PENDING ratification).

| Role | Agent | Does |
| --- | --- | --- |
| Architect | `fable-xhigh-architect` (user-level; if absent, `Plan`) | diagnoses with measurements, designs, consults |
| Implementer | `steins-implementer` | builds one slice in an isolated worktree, commits, never pushes |
| Reviewer | `steins-adversarial-reviewer` | tries to break a PR, witnesses on PHP, returns a verdict |

Every implementer and reviewer reads [`invariants.md`](invariants.md), and the
machine notes in `~/.claude/steins-run-local.md` when that file exists; keep
briefs to what is specific to the task. Agents return a report of at most 30
lines and keep raw output (test runs, A/B diffs, witness runs) as logs in the
scratch directory the brief names; open a log when a decision needs its
detail.

## Steps

1. **Map the frontier.** List the decisions the run needs and which hang off
   which. Settle facts yourself or by dispatching an agent; put decisions to
   the owner, or decide them under the delegation. Done when every decision
   has a recorded answer or a named owner.
2. **Unblock verification first.** If a gate cannot measure what the run will
   change (a corpus that does not finish, a flaky module, a surface no test
   reads), fixing that is slice 0. Done when every slice's acceptance can be
   measured.
3. **Diagnose and design with the architect.** Brief it to test every premise
   against code and measurements, and to deliver: a verified diagnosis, one to
   three options, a recommendation, landable slices (each byte-identical or
   with a classified diff), and the ADR outline. Each slice carries a
   **witness table**: the shapes it must silence and, beside them, the true
   positives it must keep reporting, each a PHP snippet whose outcome the
   architect ran on the local `php`. Done when the slice list is fixed and
   each slice has acceptance criteria and its witness table.
4. **Record before building.** Write the design into GitHub the moment it
   arrives: a parent tracking issue with the slice checklist, an issue per
   slice (new or updated), and the ADR or amendment. Scratch directories can
   vanish between sessions; issues and ADRs cannot. Issue and PR bodies skip
   the git hooks, so pass each through the leak gate before posting.
5. **Implement.** Dispatch `steins-implementer` per slice with
   `isolation: worktree`: the issue number, the design pointers and witness
   table, the measurements you need back, the base branch, and the ADR
   amendment numbers the slice may claim (assign them up front; parallel
   slices collide on the next free number). Run slices in parallel only when
   the files they will touch are disjoint; slices sharing a file run in
   series, each starting from master after the previous one merges. Split a
   slice into a syntax/catalog half and a resolver half when that lets halves
   run in parallel. Forward any structural review finding to every in-flight
   agent it touches at once.
6. **Audit and publish.** Read the diff yourself against `invariants.md`, push
   with a full refspec, open a Draft PR, and turn on Auto-fix for it (one PR per
   session can hold Auto-fix; wait on others with one background
   `gh pr checks <n> --watch`).
7. **Review by tier** (below). Send the reviewer's should-fixes back to the
   implementer that built the slice; keep its context by resuming it.
8. **Escalate on design, not on count.** Consult the architect when a review
   finds a design-level defect (soundness, structure, a wrong premise) or a
   measurement misses its estimate by more than 2×. Otherwise loop fix →
   re-review until the verdict is approve. A kept shape the review finds
   missing from the witness table joins the table in the slice's issue. Anything
   outside the slice becomes an issue linked from the parent, never a widening
   of the PR.
9. **Merge.** CI green on the PR's head and the review approved: append the
   review's outcome to the PR body, `gh pr ready`, `gh pr merge --rebase
   --delete-branch`, tick the parent checklist, and tell in-flight agents to
   rebase. Done when master holds the slice and the issue is closed.

   Steins merges by rebase, so a conflict with master, whoever reports it
   (Auto-fix, CI, GitHub), is answered by a rebase: resume the implementer to
   rebase onto master, check the published tip with `git ls-remote`, and push
   the new head with `+HEAD:refs/heads/<branch>`. An Auto-fix instruction to
   merge `FETCH_HEAD` gets the same rebase.
10. **Close the run.** Comment the outcome table on the parent (slices, what
    moved, what was not measured, follow-up issues), close it, and record what
    the next session needs.

## Review tiers

| Change | Review |
| --- | --- |
| Engine semantics, soundness, catalog rows, persisted formats | `steins-adversarial-reviewer`, PHP witnesses required, re-review after fixes |
| Harness, CLI surface, performance-only, tests | `steins-adversarial-reviewer` with a short brief, one round unless it finds a blocker |
| Docs-only | your own audit |

A byte-identical public-corpus A/B is necessary and never sufficient: false
positives, lost proofs, memory cliffs and regressions in rare shapes surface
only under witnessed probes.

## Measurement

- Public corpora: `check --profile strict --no-cache --format json`, the
  `effect-diff` baseline, and the `effects-envelope`, `throws-envelope` and
  `loop-to-array-map` dry-runs; base binary built from the PR's merge base;
  every difference classified.
- The private half of `cargo xtask fp-gate` runs locally with `--deadline`; a
  PR that could move it says in its body whether it was measured. Private
  runs queue behind the machine's lock (`invariants.md`).
- Every PR body states what was not measured.
