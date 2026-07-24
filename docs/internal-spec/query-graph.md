# The Query Graph

**Status: partial** — the graph exists and memoizes the syntax level; inference
deliberately runs outside it. ADR-0009, ADR-0028, ADR-0048.

## Inputs and queries

```text
#[salsa::input]  SourceFile { path: String, text: String }
#[salsa::input]  Project    { files: Vec<SourceFile> }

#[salsa::tracked] parse(db, file)         -> SourceTree
#[salsa::tracked] function_index(db, file)-> Vec<FunctionDecl>
#[salsa::tracked] project_index(db, proj) -> ProjectIndex
```

`Db` is a trait, so downstream crates define tracked queries against `&dyn Db`
without the engine crate depending on them. That is the seam that keeps checking
logic out of `steins-db`.

Mutating a file's text via `salsa::Setter` creates a new revision and
invalidates only what depended on it. `function_index` is a separate query from
`parse` precisely so a call-site check can depend on the index without
re-triggering on unrelated body edits.

## The project index

`project_index` maps lowercase-normalized FQNs (PHP function, class, and
namespace names are case-insensitive) to declaration sites:

```text
DeclSite { file: SourceFile, index: usize }   // re-derive the decl via parse()

Resolve::Absent               // no such FQN in the project
Resolve::Unique(DeclSite)     // the resolvable case
Resolve::Ambiguous            // two or more definitions
```

`Ambiguous` is **never resolved**. PHP would fatal on a real double-definition,
and Steins cannot know which body runs, so polyfills and conditional
declarations produce silence rather than a guess.

**Granularity, recorded honestly:** `project_index` is one monolithic tracked
query, so *any* file edit invalidates it and everything downstream. That is
acceptable for a batch CLI. The recorded plan is per-symbol salsa interning, so
an edit re-indexes only its own symbols — an LSP prerequisite, not a checker
one (ADR-0009, roadmap M5).

## What runs outside the graph

**The check pass itself.** Folding executes real PHP through the sidecar, which
is impure with respect to the query graph: the same query could return different
values across runs (a changed extension, a changed timezone), which would
corrupt memoization silently.

ADR-0028's decision is to keep folding — and therefore the whole inference walk
— outside the graph rather than lie about purity. The consequence, stated
plainly: **nothing of inference is memoized across runs.** A second `steins
check` does the same work as the first.

The recorded revisit trigger is to fold results into the graph as *recorded
inputs* (so a fold becomes a durable, invalidatable fact rather than an impure
call), which is M5 work.

## Position queries: replay over retention

ADR-0048 decided how position facts will be answered when the LSP lands, and
what that decision binds *today*.

**The decision:** re-walk the enclosing scope from a memoized per-declaration
entry state. Not position-indexed fact tables (memory at 30k-file scale, and
invalidation would need replay anyway); not per-query whole-project re-inference
(minutes-scale).

**What binds today** is deliberately minimal, and current inference work is held
to it:

1. **Scope-walk replayability** — a scope's walk must be reproducible from an
   entry state plus the scope's own trace.
2. **Canonical entry states** — the entry state must be a well-defined value,
   not an accident of traversal order. The contract fact lane is *the* entry
   state contribution (ADR-0052 §9).
3. **No global-ordering dependence** — no fact may depend on the order in which
   the project was walked. This is why the stratum `min` is commutative and
   associative, why `dedup_arms` is order-stable, and why the whole normalizer is
   pure in its arguments.

Everything else about the LSP is M5/M6 work.

## Not implemented

- **Per-declaration entry-state summaries as memoized queries** — the M5 slice.
- **Sharded per-symbol `project_index`.**
- **Fold results as recorded inputs.**
- **Any warm path or cross-run cache.** There is no on-disk cache at all.
- **A perf harness.** Cold/warm baselines are not measured under `xtask`.
