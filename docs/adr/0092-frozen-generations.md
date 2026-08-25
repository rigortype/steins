# Frozen generations: cross-run persistence by eager per-package artifacts, not a finer query graph

**Status: proposed (2026-08-25), PENDING ratification.** Drafted under the
owner's standing delegation, ahead of the slices it governs. This ADR
supersedes the *mechanism* half of ADR-0009 — the salsa-style demand-driven
query graph as the road to incrementality and LSP — restates ADR-0028's
revisit trigger, and replaces ADR-0048 §5's prerequisite list. Every
principle those ADRs state survives unchanged: budgets are first-class and
a cutoff names itself (ADR-0009), a failed fold widens and never fabricates
(ADR-0028), replay over retention with its three standing constraints
(ADR-0048 §2–§4). ROADMAP M5 is rewritten to this ADR's shape.

## 1. Context: the graph is a shell, and the evidence moved

ADR-0009 adopted a salsa-style demand-driven engine "from the first
commit", accepting a longer road to a working CLI so that a batch design
would not become a permanent shackle. A month of implementation later,
the honest inventory
(`docs/internal-spec/query-graph.md`) is: two salsa inputs, three tracked
queries at the syntax and index level, and one tracked semantic query
that no production path calls. Everything real — `check_units` in `steins-infer` — is a
hand-ordered batch pass outside the graph, and the reasons it stayed
outside are structural, not scheduling:

- **The fold seam is IPC** (ADR-0028): a fold executes the project's own
  PHP in a sidecar process, which a memoized pure function must not hide.
- **Three verdicts are whole-universe by meaning**, not by accident: the
  effect fixpoint, the throw fixpoint, and the dynamism dam. Their answers
  are properties of the entire analyzed universe; a fine-grained
  dependency graph under them buys invalidation precision below the
  granularity at which their meaning changes.
- **The measured cost is not query-shaped** (`docs/agents/profiling.md`):
  45–57% of worker CPU is re-run pure subtree scans inside CST lowering,
  work a query system would not touch.

Meanwhile there is no cross-run reuse of any kind. Every `steins check`
is a cold universe build (~200s over 90,709 corpus files, peak RSS
1.4 GB — ROADMAP gap 10, "LSP-blocking"). The MCP server re-loads the
project on every `tools/call`, so even the resident process that exists
today gets nothing from being resident. The fold memo's own comment says
"cross-run persistence is M5's problem".

The external evidence moved too. rust-glancer
(<https://rust-glancer.github.io/blog/hello-world/>,
<https://matklad.github.io/2026/08/21/rust-glancer.html>) is a working
Rust LSP built on the opposite premise: index the workspace **eagerly**,
freeze the result, persist it per package on disk, invalidate at the
granularity of a package plus its reverse-dependency closure, and answer
editor queries by re-analyzing only the edited declaration against the
frozen index. It reaches memory and restart targets the lazy
incremental in-memory model does not — and the second post is from the
rust-analyzer lineage ADR-0009 imported. PHPStan's result cache is the
nearest PHP prior art and validates the *need*: persist analysis, pay
only for what changed. Its per-file dependency lists are sound under
PHPStan's modular analysis, where a declared signature is the interface
between files; under call-site value propagation (ADR-0001) a body edit
reaches transitive callers through values, so the unit of reuse has to
close over propagation — which summaries make tractable and file-level
dependency lists do not.

The conclusion this ADR draws: the batch pass is not a shackle to be
decomposed. It is the generation builder, and what was missing is not a
finer graph but **persistence, identity, and a sound invalidation
boundary**.

## 2. Decision: analysis is a frozen generation, persisted per package

A **generation** is one validated, immutable analysis of the universe
under one fingerprinted identity. It is built by the same phased pass
`check_units` runs today, package-parallel where the phases allow;
queries — CLI rendering, MCP tools, and later the LSP — read only
published generations.

**Identity.** The generation fingerprint covers every input that can
change a finding and nothing that only changes its rendering: per-package
source fingerprints, `composer.lock`, the catalog pin, the plugin set,
the engine posture (the boot surface's own identity fields — PHP version,
`PHP_INT_SIZE`, extension set, fold lane — or "engine off"), and the
analyzer's own version. A new Steins is a new universe: artifacts carry a
schema version, a mismatch is a miss, and there is no migration path by
design. Fingerprints are content hashes with explicit field tags,
independent of any `Hash`/serialization byte layout.

**Publication.** A build constructs a private candidate and publishes
atomically on success; a half-written candidate is discarded wholesale at
the next startup, never partially salvaged. Sources are captured once
behind a single boundary, sealed, and re-validated immediately before the
candidate replaces the published generation, so a concurrent edit rejects
the candidate rather than tearing it.

**Per-package artifacts.** Each package persists: its symbol shard, its
declared contracts, its per-declaration summaries (canonical entry state
per ADR-0048 §3, own-effect row, own-throw row, per-file dam facts), its
recorded fold table (§4), and its trace IR in per-file shards behind a
seekable directory, so a binding descent into one vendor function loads
one shard and a metadata query loads none. Loaded shards are owned by the
reading operation and dropped with it; residency (what stays in memory)
is a policy axis independent of what is persisted, with "first-party
resident, vendor offloadable" as the intended default.

**The standing invariant, imported verbatim: a cache miss may change
cost, never meaning.** Its corollary under the zero-FP contract
(ADR-0002): staleness never serves. A generation is valid under its
fingerprint or it is rebuilt; a decode failure of any artifact degrades
to rebuild-from-source, never to a partial answer. Serialization is a
cache, not an interchange format — nothing outside Steins may consume
the artifacts, so their layout is free to change with the schema
version.

## 3. Decision: the unit is the Composer package; global tables are merged per generation, never persisted

The universe partitions into Composer packages — vendor per
`composer.lock`, first-party as its own package (or packages, where the
ADR-0047 partition vocabulary applies). Invalidation is a changed
package plus its reverse-dependency closure: the workspace depends on
all of vendor, vendor's own edges come from `composer.lock`, and a
`composer.lock` change rebuilds exactly the packages it touches. Vendor
almost never changes between edits, so after the first build the vendor
universe is effectively free — which is most of gap 10.

PHP's symbol space forces one deliberate deviation from the package
model: autoloading is not a module system, so a symbol added in one
package can render a name in another package ambiguous, and
`class_alias` edges cross packages freely. Therefore **shards persist
per package, and every global table is recomputed per generation from
the shards**: the merged project index, the ambiguity sets, the literal
`class_alias` folding (already order-independent by construction), the
never-returning veto set, and the dam's universe verdict (per-file dam
facts union monotonically, so the merge is cheap). Reverse-closure
invalidation bounds *which shards rebuild*; the per-generation merge is
what keeps `Resolve::Ambiguous` and userland-shadows-builtin sound
across package boundaries.

This also retires the duplicated symbol table: the shard builder becomes
the one implementation, `steins_db::ProjectIndex` and
`steins_infer::project::Index` both becoming views of merged shards.

## 4. Decision: fold results are generation inputs, recorded through the replay table

ADR-0028's revisit trigger is hereby restated. Fold results do not
become a salsa input layer; they become **recorded rows in the package
artifact**, written through the seam ADR-0066 already built. During a
generation build the process engine answers and the policy records
`(method, params) → result` rows — the replay table's own key shape,
promoted from a browser transport to the persistence protocol. A later
build whose engine identity matches replays the rows through
`TableEngine` semantics and asks the live engine only what the table
cannot answer; one whose identity differs (a different PHP minor, a
different width, a different extension set) asks everything again,
because the rows are keyed under the generation's engine identity and a
mismatched identity is a miss.

Failure semantics are unchanged and load-bearing: an unanswerable
request widens, never fabricates, and a recorded row can never outlive
the fingerprint that scopes it. The differential fixpoint oracle
(`replay_fold.rs`) already pins that replay-from-table means exactly
what ask-the-engine means; it becomes the acceptance pin that
replay-from-disk does too.

## 5. Decision: summaries are the incremental currency; warm ≡ cold is the oracle

The warm path after an edit: reparse the changed files; rebuild the
owning package's shards; recompute the global merges (§3); re-run the
effect and throw fixpoints seeded from persisted summaries, recomputing
only summaries reachable from the changed declarations (both fixpoints
are monotone over the resolved call graph, which is what makes the
seeded re-run equal to a cold one); re-emit diagnostics for the affected
set. No keystroke-level incrementality is promised anywhere in this ADR:
the invalidation floor is the saved file, and interactivity above it is
the replay lane's job (§6).

ADR-0048's three standing constraints — scope-walk replayability, one
canonical entry state per declaration, no global-ordering dependence —
were adopted to keep position queries reachable. They now do double
duty as this ADR's soundness argument: they are exactly the properties
under which a walk re-run from persisted state produces what the cold
walk produced.

The acceptance oracle for every slice of this ADR: **a warm generation's
findings are byte-identical to a cold build of the same tree.** The perf
harness carries it as a differential gate, the same pattern ADR-0066
uses for the replay transport. A slice that cannot meet it does not
land.

## 6. The LSP inherits a dirty-buffer contract, not an invalidation problem

ADR-0048 §1 (replay over retention) is unchanged: position facts are
answered by re-walking the enclosing scope from its entry state — now
read from the published generation. What M6 gains from this ADR is the
contract for unsaved buffers, taken from the same prior art:

- **Request-only re-analysis.** A dirty buffer never mutates the
  generation. The enclosing declaration is re-lowered and re-walked
  against the frozen universe for the duration of one request, and the
  result is discarded. A declaration the buffer adds is visible to its
  own body only; nothing else can discover unsaved code.
- **Header anchoring.** A dirty declaration is matched to its saved
  identity by its declaration header and enclosing headers, and only
  when the anchor is unique on both sides; an ambiguous match is a
  refusal to associate, never a guess.
- **Named staleness.** A cross-file query that would read unsaved state
  it cannot have (references, rename) answers "save required" as a
  first-class result — a refusal with a name, in the ADR-0046 posture,
  rather than an answer that might be wrong.

## 7. Rejected alternatives

- **Completing the salsa decomposition** (M5 as previously written:
  per-declaration entry states as tracked queries, per-symbol
  `project_index` sharding, folds as salsa inputs). Rejected on the
  grounds of §1: the whole-universe verdicts do not decompose into it,
  the fold seam fights it, the measured cost lives elsewhere, and the
  parallel-query machinery would force `Send + Sync` across a walk that
  threads `&mut` state by design. The granularity it buys — sub-file
  invalidation precision — serves keystroke incrementality, which §5
  deliberately does not promise and §6 replaces with request-only
  replay.
- **PHPStan's result-cache shape** (per-file dependency lists driving
  re-analysis). The need is real and this ADR serves it; the shape
  assumes modular analysis. Under ADR-0001 the interface between files
  is not the declared signature but the propagated value, so file-level
  dependency lists either over-invalidate to uselessness or
  under-invalidate to unsoundness. The package closure plus
  per-generation merges is the coarsening that stays sound.
- **Retained position-fact tables** — rejected in ADR-0048 §1;
  unchanged.
- **A resident daemon without disk artifacts.** Keeps the warm path but
  loses free restart, ties the warm state to one process lifetime (the
  MCP server's, today), and caps at memory. The artifacts are what make
  warmth survive the process, which is the agent-workflow case: CI
  steps, MCP calls, and editor sessions arrive as separate processes.

## 8. Consequences

- ROADMAP M5 is rewritten to this ADR's work items and gains the
  warm ≡ cold exit criterion; M6 gains §6. Gap 10's arrow is unchanged.
- ADR-0009, ADR-0028, and ADR-0048 carry amendments pointing here; no
  clause of ADR-0048 §2–§4 moves, which is why existing inference code
  is already held to the properties this design needs.
- `steins-db`'s role shrinks to inputs and the in-run parse memo. No
  new tracked semantic query is planned; whether the crate keeps salsa
  at all becomes an implementation decision this ADR frees but does not
  require.
- The generation builder is package-parallel by construction (one
  sidecar per worker, as the fp-gate already runs; per-package
  diagnostic sinks merged at the end), and parallelism is a memory knob
  as much as a speed knob: peak RSS during a build is bounded by
  workers × the largest package, not by the universe.
- New failure surface, priced deliberately: artifact decode is bounded
  (size limits, schema check) and every failure path is
  rebuild-from-source. The recovery story is deliberately unclever —
  throw the cache away; §2's invariant makes that always correct.
