# Crate Topology

**Status: implemented.**

Twelve workspace crates, plus `xtask` (the verification apparatus) and
`harness/phpdoc-oracle` (a PHP-side differential harness).

## Dependency direction

Read off the manifests, not drawn by hand — `cargo metadata --no-deps` is the
authority if this drifts again:

```text
steins-cli
  ├── steins-db ── steins-syntax ── steins-domain
  │           └── steins-gen, steins-catalog, steins-phpdoc
  ├── steins-infer
  │     ├── steins-domain
  │     ├── steins-contract ── steins-domain, steins-phpdoc
  │     ├── steins-db, steins-syntax
  │     ├── steins-gen          (native only — the generation store)
  │     ├── steins-catalog
  │     ├── steins-sidecar
  │     └── steins-phpdoc
  ├── steins-edit
  │     └── steins-db, steins-syntax, steins-infer,
  │         steins-domain, steins-contract, steins-phpdoc
  ├── steins-catalog, steins-syntax, steins-phpdoc, steins-sidecar

steins-wasm (a separate artifact, not under the CLI)
  └── steins-db, steins-infer, steins-syntax, steins-catalog
```

Only `steins-syntax` sees the pinned `mago-*` fork (ADR-0003).

Leaves with **no internal dependencies at all**: `steins-domain`,
`steins-phpdoc`, `steins-catalog`, `steins-sidecar`, `steins-gen`. That is a
deliberate property — each owns a self-contained body of knowledge and can be
tested without an analyzer.

One oddity worth not rediscovering: `steins-db` carries a **dev-dependency on
itself**, which is how its own tests enable the `persist` feature without any
crate shipping it enabled (issue #487).

## What each crate owns, and what it defends

### `steins-domain` — the value domain

The four-layer `Fact` algebra, `Certainty`, `Val`/`Base`/`Key`, `StrPreds`,
`IntRange`, and PHP truthiness/numeric-string predicates.

**Defends:** the soundness of `join` (`γ(a) ∪ γ(b) ⊆ γ(join(a, b))`,
property-tested) and canonical form as a constructor invariant. Nothing outside
this crate constructs a non-canonical fact.

### `steins-phpdoc` — the PHPDoc grammar

Lexer, recursive-descent parser, spanned AST, docblock tag scanner. A hand port
of `phpstan/phpdoc-parser`, including its whitespace-sensitivity and
save-point/backtrack behavior.

**Defends:** grammar compatibility, verified externally by the oracle harness.
The parser never panics on input; unparseable and deliberately-opaque constructs
both mean "no envelope".

### `steins-contract` — acceptance and normalization

Lowers the PHPDoc AST to `ContractTy`, answers `admits_val` / `admits_fact`, and
provides the arm-list normalizer (`subsumes`, `arm_eq`, `dedup_arms`,
`summarize_vals`, `subtract`) plus the shared type spelling.

**Defends:** *one* acceptance relation. The normalizer adds no parallel
judgment; every question reduces to the denotation query. It knows nothing about
the project hierarchy — the is-a oracle enters through an `IsaOracle` trait seam
so the polarity law stays here while hierarchy knowledge stays in `steins-infer`.

### `steins-catalog` — runtime knowledge

The folding allowlist, effect coloring of builtins, the effect label registry,
the generated builtin class hierarchy, builtin exception parents, failure-arm
labels, and invocation shapes.

**Defends:** the pin. Generated tables carry their php-src commit and the PHP
minor they were cross-checked against; consumers demote verdicts on version
skew.

### `steins-syntax` — the syntax-tree contract

The owned, lowered `SourceTree` and every plain-data struct the analyzer sees,
plus the lowering from the pinned Mago fork.

**Defends (hard rule):** the pinned Mago fork is a dependency of *this crate
only*, and **no Mago type appears in its public API**. This is the ADR-0003 seam
that lets a parser backend be swapped without touching an analysis crate.

### `steins-gen` — the frozen-generation substrate

Generation identity (blake3 over tagged, length-prefixed fields), the
payload-agnostic artifact container (named byte ranges behind a seekable
directory), the candidate-then-publish store under `<project>/.steins/`, the
sealed `SourceInventory`, and the Composer partition vocabulary
(`Package`, `PackageKind`, `PackageUniverse`). ADR-0092 §2/§3.

**Defends:** that a cache miss changes cost, never meaning. Every decode
failure is a `Miss` the caller maps to rebuild-from-source; artifacts carry a
schema version with no migration path by design. It knows nothing about what
section bytes *mean* — that belongs to the payload owners, which is why it can
stay a leaf.

### `steins-db` — the query graph, the shards, the payloads

The salsa database, the `SourceFile` / `Project` inputs, the syntax-level
tracked queries (`parse`, `function_index`), the whole-project symbol index,
the Composer partition builder, the per-package symbol shard and the
per-generation merge (ADR-0092 §3), and the artifact payload codecs — what the
`symbols` / `contracts` / `trace` sections mean, plus the read transaction and
residency vocabulary (ADR-0092 §2).

**Defends:** two things. That semantic queries live *outside* this crate —
downstream crates define tracked queries against the `Db` trait, so checking
logic never lands in the engine crate. And that the merge is
**partition-invariant**: any grouping of the same files merges to the same
tables, which is what lets shard boundaries be a persistence decision rather
than a semantic one.

### `steins-sidecar` — PHP IPC

The resident PHP process, the JSON-RPC framing, and the embedded single-file
runner.

**Defends:** the zero-FP contract under failure. Every failure mode — spawn
failure, IO error, timeout, malformed response — maps to `Widen`, never to a
value.

### `steins-infer` — the inference engine

The walk, environments, the object store, binding descent, the effect and throw
fixpoints, name resolution, every diagnostic emitter, the diagnostic registry,
inline suppression, and the dam. Also the generation orchestrator (ADR-0092
§5) and the two artifact sections whose payloads are this crate's own
vocabulary rather than `steins-db`'s — `sources`, the provenance record the
reuse decision reads, and `summaries`, the per-file walk blocks a warm run
replays instead of walking (issue #489). The line is the same one `steins-gen`
draws one level up: a section's bytes belong to whoever knows what they mean,
and `Diagnostic` / the diagnostic registry / `Facet` / `Fix` are not things
`steins-db` knows.

**Defends:** the zero-FP bar itself. This is where `Maybe` becomes silence —
including across a generation: a file may only replay a persisted block when
its own bytes, every name its footprint could resolve, every file it reaches,
and every whole-universe verdict are unmoved, and every unknown resolves to
walking.

### `steins-edit` — the transform engine

`EditPlan` transactions, the diff renderer, the transform vocabulary
(`Refusal`, `CompletenessOracle`, `TransformReport`), the region model, dynamism
obstacles and the vouch valve, and the two shipped transforms.

**Defends:** that a rewrite's preconditions are *proven*, not pattern-matched.
It reaches into `steins-infer` precisely to prove "all call sites flow this
type" — the precondition structurally unavailable to a modular tool.

### `steins-cli` — the binary

Argument parsing, `steins.toml` loading, the profile engine, the baseline
channel, output rendering.

**Defends:** that a profile is *display data*. Nothing in the CLI changes
inference behavior.

### `steins-wasm` — the browser playground's artifact

The C ABI (`sw_check`, `sw_annotate`, and their replay twins), the byte-buffer
in / JSON envelope out protocol, and nothing else. ADR-0065/0066.

**Defends:** that the browser runs the *same* analysis. It constructs no
folder of its own beyond `NoFold` and the replay table, and no `std::process`
— and therefore no generation store — enters its dependency graph.

## Layering rules

1. **No analysis crate sees a Mago type.** Enforced by `steins-syntax`'s public
   API.
2. **`steins-contract` never depends on `steins-infer` or `steins-catalog`.**
   Hierarchy knowledge enters through a trait seam.
3. **`steins-domain` depends on nothing.** The lattice is testable in isolation.
4. **The dependency runs `steins-edit → steins-infer`, never the reverse.** This
   is why the shared type spelling lives in `steins-contract`: the `annotate`
   and dump emitters in `steins-infer` cannot reach the docblock renderer in
   `steins-edit`.
5. **Diagnostic ids are declared in `steins-infer`** and bound to their layers by
   a totality test — see [diagnostic-shape.md](diagnostic-shape.md).
6. **`steins-gen` depends on no steins crate.** Identity, the container and the
   store are payload-agnostic, so the crates that own payloads depend on it and
   never the reverse (ADR-0092 §2).
7. **`steins-infer` reaches the store only on native targets** —
   its `steins-gen` dependency is `cfg(not(target_arch = "wasm32"))`, the same
   discipline the process fold transport follows. Note the rule is *not*
   currently a whole-graph property: `steins-db` depends on `steins-gen`
   unconditionally for the partition vocabulary, so `steins-gen` and `blake3`
   do reach `steins-wasm`. That compiles and CI's wasm job is green; if the
   playground's artifact size ever matters, splitting the partition types from
   the store/fingerprint halves behind a feature is the remedy.
