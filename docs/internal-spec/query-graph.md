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
  `STEINS_EXPERIMENTAL_GENERATIONS=1` (issue #489). A file whose captured
  content fingerprint matches its artifact row loads its lowered tree instead
  of re-parsing (slice A, made per file by issue #512 — the package
  fingerprint survives as the shortcut that says every file is unmoved, and a
  package whose files fall on both sides is *mixed*: it loads what it can,
  parses the rest, and rebuilds its shard from the trees in hand), and a file
  nothing could have changed replays its persisted walk instead of walking
  (slice B, below).
- **The per-package payloads** — `symbols`, `contracts`, `trace` — live in
  `steins_db::persist` (#487); `sources`, `summaries` and `facts` live in
  `steins_infer` beside the orchestrator that reads them, because their
  payloads are that crate's vocabulary; the container, identity and store live
  in `steins-gen` (#485).
- **A tree is decoded only where a walk reaches it** (issue #516). Every
  whole-universe phase used to read something off every file's tree, so a warm
  no-change rebuild decoded the universe whatever the edit was. Those readings
  are all *summaries* of a tree, and the `facts` section persists them per
  file; `FileUnit::tree` is a `LazyTree` that decodes on first use, so what
  costs a decode is a walk — the file's own, plus whatever its binding descent
  and class-chain walks reach. The own rows persist **resolved**, licensed by
  the affected set rather than re-resolved at merge time: a row is a function
  of the file's origins and of how the merged index resolves the names the file
  references, and `F ∉ affected` is precisely the over-approximation of "some
  resolution F makes could have moved" — the same judgement that already
  licenses replaying F's whole diagnostic block, which is the stronger claim.
  Measured on nikic/PHP-Parser, warm no-change: 70 ms → 30 ms, and one tree
  decoded of 341 (the single file whose docblock spells `@throws`).
- **Fold results are recorded generation inputs** through the ADR-0066 table
  seam, keyed under the engine identity (#488).
- **The perf harness** is `cargo xtask perf` (#483), with `--warm` measuring the
  lifecycle and asserting warm ≡ cold in-process (#489).
- **Skipping the walk** of a file nothing could have changed is issue #489
  slice B: the `summaries` section persists per-file walk blocks, and a file
  replays its block unless it is in the affected set — its own bytes moved, its
  name footprint meets the name delta (the names the *changed files* of a
  changed package site in its old or new shard, issue #510), it reaches a
  changed file in the file-level call graph within `MAX_BINDING_DEPTH`, or a
  whole-universe verdict moved. The verifier
  (`STEINS_GENERATIONS_PARANOID=1`, `cargo xtask perf --warm --paranoid`) walks
  everything anyway and grades every would-be skip against its fresh walk.
- **Per-declaration entry-state summaries** were *not* needed and are not
  persisted: a walked file recomputes its entry state locally from the loaded
  trace, and a replayed one recomputes nothing at all. The ADR-0048 §3
  constraint stands as a constraint; it did not become an artifact.

What salsa still does is exactly what this document's first section describes —
memoize `parse` within one run — and no new tracked semantic query is planned.

## Still open

- **Publishing is O(universe)** — and, since issue #516 took the tree decode
  out, it is what an edit costs. A generation is a directory of whole-file
  artifacts, so any edit rewrites every payload of the package holding it, and
  the ordinary first-party shape is one package holding everything. Measured on
  nikic/PHP-Parser, a one-line leaf edit: capture 17 + trees 12 + analyze 5 +
  **persist 78** ms. Unchanged by #516 (it was 76 ms before, re-encoding trees
  rather than copying payloads) and now the largest single phase of a warm
  rebuild by a factor of four.
- **Capture hashes every file every run** (issue #516's second item). 13 ms of
  a 30 ms no-change rebuild on nikic/PHP-Parser — the largest phase *there*,
  but 15% of an edit's cost, so it ranks behind publishing.
- **The reporting passes are gated, not summarized.** `throw_diagnostics` emits
  from a declaration's own docblock, so it is gated per file and costs the tree
  of every file spelling `@throws` (29 of 217 on Seldaek/monolog).
  `effect_diagnostics` is coarser: its Liskov leg reads a class's *ancestors'*
  envelopes, so a project declaring an effect envelope anywhere decodes every
  tree in that pass. Narrowing it needs a persisted class → envelope table.
- **The call graph saturates on common method names** (issue #513). The tree
  load and the name delta are both proportional to the edit now, so what an
  edit costs is decided by the backwards call closure — and a file declaring a
  method name dozens of others also declare pulls them all in. Measured on
  nikic/PHP-Parser: editing a leaf test file walks 2 files of 341, editing one
  that declares `enterNode` walks 337.
- **Per-file walk parallelism** (issue #490, re-scoped to the file loop).
- **The trace codec** (issue #504): artifacts run ~14x the analyzed source.
