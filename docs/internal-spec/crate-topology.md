# Crate Topology

**Status: implemented.**

Ten workspace crates, plus `xtask` (the verification apparatus) and
`harness/phpdoc-oracle` (a PHP-side differential harness).

## Dependency direction

```text
steins-cli
  ├── steins-db ── steins-syntax ── [mago-* : pinned fork]
  ├── steins-infer
  │     ├── steins-domain
  │     ├── steins-contract ── steins-domain, steins-phpdoc
  │     ├── steins-db, steins-syntax
  │     ├── steins-catalog
  │     ├── steins-sidecar
  │     └── steins-phpdoc
  └── steins-edit
        └── steins-db, steins-syntax, steins-infer,
            steins-domain, steins-contract, steins-phpdoc
```

Leaves with **no internal dependencies at all**: `steins-domain`,
`steins-phpdoc`, `steins-catalog`, `steins-sidecar`. That is a deliberate
property — each owns a self-contained body of knowledge and can be tested
without an analyzer.

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

### `steins-db` — the query graph

The salsa database, the `SourceFile` / `Project` inputs, the syntax-level
tracked queries (`parse`, `function_index`), and the whole-project symbol index.

**Defends:** that semantic queries live *outside* this crate — downstream crates
define tracked queries against the `Db` trait, so checking logic never lands in
the engine crate.

### `steins-sidecar` — PHP IPC

The resident PHP process, the JSON-RPC framing, and the embedded single-file
runner.

**Defends:** the zero-FP contract under failure. Every failure mode — spawn
failure, IO error, timeout, malformed response — maps to `Widen`, never to a
value.

### `steins-infer` — the inference engine

The walk, environments, the object store, binding descent, the effect and throw
fixpoints, name resolution, every diagnostic emitter, the diagnostic registry,
inline suppression, and the dam.

**Defends:** the zero-FP bar itself. This is where `Maybe` becomes silence.

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
