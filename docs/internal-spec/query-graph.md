# The Query Graph

**Status: partial, and deliberately final at this size** — the graph exists and
memoizes the syntax level within one run; inference runs outside it, and
cross-run reuse is ADR-0092's generation store rather than a larger graph.
ADR-0009 (as amended), ADR-0028, ADR-0048, ADR-0092.

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
acceptable for a batch CLI. ADR-0009 recorded per-symbol salsa interning as
the plan; ADR-0092 §3 supersedes it — the index shards per *package*, with
every global table merged per generation, and `project_index` already
delegates to that shard builder (`steins_db::shard`, issue #486). The shards
persist per package (#487) and a warm run loads an unchanged package's shard
rather than rebuilding it; the merge itself is recomputed every generation,
because PHP's symbol space makes ambiguity and `class_alias` global facts.

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

## What the warm path is, and where it lives

Everything this section once listed as missing landed under ADR-0092, and none
of it landed *in the graph* — which is the point. Persistence is frozen
generations on disk, not a finer query DAG:

- **Cross-run reuse** is the generation store (`<project>/.steins/gen/`), built
  and read by `steins_infer::generation_check` behind
  `STEINS_EXPERIMENTAL_GENERATIONS=1` (issue #489 slice A). A package whose
  captured source fingerprint matches its artifact loads its lowered trees and
  its shard instead of re-parsing.
- **The per-package payloads** — `symbols`, `contracts`, `trace` — live in
  `steins_db::persist` (#487); the container, identity and store live in
  `steins-gen` (#485).
- **Fold results are recorded generation inputs** through the ADR-0066 table
  seam, keyed under the engine identity (#488).
- **The perf harness** is `cargo xtask perf` (#483), with `--warm` measuring the
  lifecycle and asserting warm ≡ cold in-process (#489).
- **Per-declaration entry-state summaries** were *not* needed and are not
  persisted: a warm run walks every file, so each recomputes its entry state
  locally from the loaded trace. The ADR-0048 §3 constraint stands as a
  constraint; it did not become an artifact.

What salsa still does is exactly what this document's first section describes —
memoize `parse` within one run — and no new tracked semantic query is planned.

## Still open

- **Skipping the walk of unchanged files** (issue #489 slice B). Warm runs
  currently re-walk everything, which measurement says is 60–76% of a warm run
  and the share grows with project size.
- **Per-file walk parallelism** (issue #490, re-scoped to the file loop).
- **The trace codec** (issue #504): artifacts run ~14x the analyzed source.
