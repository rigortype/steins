# Architecture Decision Records

This directory holds the Architecture Decision Records for Steins. Each one
captures a decision that constrains the design, the forcing reason behind it,
and the consequences accepted in exchange. An ADR is the record of *intent*:
where an ADR and the [type specification](../type-specification/README.md)
disagree about what the analyzer does **today**, the specification binds; where
they disagree about what it **should** do, the ADR binds.

## How to read

Four ADRs carry most of the weight — read these first and the rest fall into
place:

- **[ADR-0001](0001-call-site-value-propagation.md)** — call-site value
  propagation, the core analysis model every other decision follows from.
- **[ADR-0002](0002-two-layer-diagnostics-zero-fp.md)** — the zero-FP proof
  layer, named policy profiles, and the refusal of numeric levels.
- **[ADR-0004](0004-php-sidecar-default-on.md)** — the PHP sidecar: ask the
  project's own runtime, degrade to a sound subset when it is absent.
- **[ADR-0035](0035-four-layer-value-domain.md)** — the four-layer value
  domain the whole type system is expressed in.

From there, three clusters:

- **Type semantics** — 0029–0033, 0035–0037, 0042–0045, 0052, 0056.
- **Effects and throws** — 0005–0008, 0018, 0019, 0040, 0055.
- **Apparatus and surfaces** — 0013, 0020–0026, 0049, 0050, 0053, 0054.

Steins ADRs carry no `Status:` field. An ADR is in this directory because it was
accepted; where implementation is still in flight, the ADR says so inline and
[not-implemented.md](../type-specification/not-implemented.md) collects the gap
list in one place.

## Index

| # | Title |
| --- | --- |
| ADR-0001 | [Call-site value propagation as the core analysis model](0001-call-site-value-propagation.md) |
| ADR-0002 | [Two-layer diagnostics: zero-FP proof layer, named policy profiles, no numeric levels](0002-two-layer-diagnostics-zero-fp.md) |
| ADR-0003 | [Syntax tree contract owned by Steins; Mago's parser evaluated behind it](0003-syntax-tree-ownership.md) |
| ADR-0004 | [PHP sidecar: default-on, runs the project's own PHP, degrades to a sound subset](0004-php-sidecar-default-on.md) |
| ADR-0005 | [Effects: a second inferred dimension, declarations as envelopes, closed origins](0005-effects-second-inferred-dimension.md) |
| ADR-0006 | [Effect envelopes are spelled as native attributes; @throws stays Throwable-only](0006-effect-declaration-syntax.md) |
| ADR-0007 | [Throw-envelope accounting: Error + LogicException unchecked, the rest checked](0007-checked-unchecked-throw-accounting.md) |
| ADR-0008 | [Initial effect labels](0008-initial-effect-lattice.md) |
| ADR-0009 | [Demand-driven incremental engine (salsa-style) from the start](0009-salsa-demand-driven-engine.md) |
| ADR-0010 | [Own codemod engine + fix-its; no Rector integration; agent-first surface](0010-own-codemod-engine-agent-first.md) |
| ADR-0011 | [Scope floor: PHP 8.x only, 8.1 as the working floor](0011-php-floor-8x-only.md) |
| ADR-0012 | [Rigor's plugin architecture imported; framework support deferred, Laravel eventually first-class](0012-plugin-architecture-frameworks-deferred.md) |
| ADR-0013 | [Verification apparatus: FP-gate corpus, counterexample constructibility, differential as instrument](0013-verification-apparatus.md) |
| ADR-0014 | [Builtin catalog: php-src stubs as base, Steins effect layer on top, phpstorm-stubs as PECL supplement](0014-builtin-catalog-sourcing.md) |
| ADR-0015 | [Inference descends into vendor/, per-package budgets, vendor diagnostics off](0015-vendor-propagation-with-budgets.md) |
| ADR-0016 | [Why a new design rather than evolving an existing checker](0016-greenfield-not-upstream.md) |
| ADR-0017 | [Lint and format are not Steins' business: separate-process backends, never linked](0017-no-lint-no-format.md) |
| ADR-0018 | [Hierarchical effect labels: dot-path strings, prefix subsumption, open registry](0018-hierarchical-effect-labels.md) |
| ADR-0019 | [never, exit, and control effects: the type/effect division of labor](0019-never-exit-and-control-effects.md) |
| ADR-0020 | [CLI surface: six commands, two deliberate absences](0020-cli-surface.md) |
| ADR-0021 | [Catalog seeding is demand-driven; the initial FP-gate corpus](0021-catalog-seeding-and-corpus.md) |
| ADR-0022 | [Diagnostic IDs: emitter-decoupled family.rule registry; JSONL baseline](0022-diagnostic-id-registry-and-baseline.md) |
| ADR-0023 | [Config: steins.toml carries intent; suppression splits into three channels](0023-config-toml-and-suppression-channels.md) |
| ADR-0024 | [Sidecar protocol: JSON-RPC over stdio, single-file runner, four core methods](0024-sidecar-protocol.md) |
| ADR-0025 | [License boundaries: MIT vocabulary, AGPL core (relicensing kept open)](0025-license-boundaries-and-packaging.md) |
| ADR-0026 | [Corpus harness as xtask: pinned lock, red-on-any-finding gate](0026-corpus-harness-xtask.md) |
| ADR-0027 | [Propagation is staged through a linear trace IR; unknown lowers to Barrier](0027-linear-trace-ir-staged-propagation.md) |
| ADR-0028 | [Folding runs outside the salsa query graph (for now)](0028-folding-outside-query-graph.md) |
| ADR-0029 | [PHPDoc type grammar: phpstan/phpdoc-parser compatibility, own Rust implementation](0029-phpdoc-grammar-phpstan-compat.md) |
| ADR-0030 | [Type-operation semantics: PHPStan's denotational core + a divergence registry](0030-type-semantics-phpstan-core-divergence-registry.md) |
| ADR-0031 | [Branch-sensitive analysis: structured trace tree, trinary conditions, staged](0031-structured-trace-tree-branch-analysis.md) |
| ADR-0032 | [Generics under call-site propagation: no solver where values flow](0032-generics-under-propagation.md) |
| ADR-0033 | [Closures as values, invocation-shape catalog, and Liskov as a standing rule](0033-closures-as-values-and-liskov.md) |
| ADR-0034 | [Transform engine: EditPlan transactions, code preconditions, dual verification](0034-transform-engine-skeleton.md) |
| ADR-0035 | [The four-layer value domain: refinements as predicate sets, not accessory types](0035-four-layer-value-domain.md) |
| ADR-0036 | [Object state: store-based heap, escape sets, readonly immunity](0036-object-state-heap-model.md) |
| ADR-0037 | [Trust stratification: a proven value never loses to a declared type](0037-trust-stratification-proven-beats-declared.md) |
| ADR-0038 | [No taint analysis; value provenance labels reserved as the general mechanism](0038-no-taint-value-provenance-labels-reserved.md) |
| ADR-0039 | [Plugin contract: composer-distributed declaration suppliers with pattern subscriptions](0039-plugin-contract.md) |
| ADR-0040 | [Throw damming: the one effect that dies, and how it dies](0040-throw-damming-semantics.md) |
| ADR-0041 | [phpdoc→native promotion v1: scope, refusal taxonomy, honesty-repair sibling](0041-phpdoc-native-promotion-v1-scope.md) |
| ADR-0042 | [Failure-cause labels on union arms: the benevolent-union replacement](0042-failure-arm-labels-no-benevolent.md) |
| ADR-0043 | [Object/method world: native object acceptance over a trinary is-a oracle](0043-object-method-world.md) |
| ADR-0044 | [Data-mapper support: dependent shapes, witness refs, mapper returns as runtime truth](0044-data-mapper-packs-dependent-shapes.md) |
| ADR-0045 | [PSR knowledge: vendor recovers the types, the engine supplies the semantics](0045-psr-knowledge-synthetic-envelopes.md) |
| ADR-0046 | [Dynamism posture: eval, dynamic include, unserialize — unanalyzability, not nondeterminism](0046-dynamism-unanalyzability-posture.md) |
| ADR-0047 | [Per-service project partitioning: scoped enumeration obstacles for transforms](0047-project-partitioning.md) |
| ADR-0048 | [Position queries: replay over retention](0048-position-query-architecture.md) |
| ADR-0049 | [Finding breadth: undefined symbols, arity, offset access as absence proofs](0049-finding-breadth-family.md) |
| ADR-0050 | [Diagnostic layers and profiles: registry-carried layers, proof-only default, named surfaces](0050-diagnostic-layers-profiles.md) |
| ADR-0051 | [Template scope transfer: templates as functions, render sites as call sites](0051-template-scope-transfer.md) |
| ADR-0052 | [Narrowing and subtraction: stratified guard facts, arm-wise subtraction, the extracted normalizer](0052-narrowing-and-subtraction.md) |
| ADR-0053 | [The dump surface: a debug lane for requested introspection — dumpType family port, var_dump default-on](0053-dump-surface.md) |
| ADR-0054 | [CI surface: four renderings of one surface, and the doctor posture report](0054-ci-surface.md) |
| ADR-0055 | [Class-level purity defaults, the `Impure` top envelope, and the mutation label family](0055-class-purity-impure-top-mutation-effects.md) |
| ADR-0056 | [Builtin return facts: reflected envelope, curated refinement within it](0056-builtin-return-facts.md) |
