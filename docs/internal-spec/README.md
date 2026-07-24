# Steins Internal Specification

## Status

Draft, and **descriptive of the tree as it stands** (verified 2026-07-24, at the
v0.1.0 landing point). This directory specifies the analyzer-internal contracts:
crate boundaries, the syntax-tree contract, the trace IR, the query graph, the
folding seam, the sidecar protocol, config and baseline formats, the transform
engine, and the verification apparatus.

Where a document describes a surface that is designed but has no code, it says
so in its first sentence. Nothing here is written as if it existed.

Type-language *semantics* — the value domain, acceptance relations, narrowing,
effects, throws, diagnostic policy — live in
[`docs/type-specification/`](../type-specification/README.md) and bind whenever a
description here would conflict with observable analysis behavior.

Design rationale, rejected options, and open questions live in
[`docs/adr/`](../adr/). When this specification and an ADR appear to disagree
about what the code does today, the specification binds and the ADR should be
amended (or the code fixed, and then it is a bug report).

## Conventions

The keywords MUST, MUST NOT, SHOULD, SHOULD NOT, and MAY are to be interpreted
as described in [RFC 2119](https://www.rfc-editor.org/rfc/rfc2119) and
[RFC 8174](https://www.rfc-editor.org/rfc/rfc8174).

Rust identifiers (`SourceTree`, `Fact`, `EditPlan`, …) name the implementing
type. **None of them is a stable public API** — see
[public-surface.md](public-surface.md). They are given so a reader can find the
code.

Status markers used inline are the same as in the type specification:
**Implemented**, **Partial**, **Designed, not implemented**.

## Reading order

| Document | Scope |
| --- | --- |
| [crate-topology.md](crate-topology.md) | The ten crates, their dependency direction, and the boundaries each one defends. |
| [syntax-tree-contract.md](syntax-tree-contract.md) | The owned, Mago-free CST contract; spans; the span+splice editing model; the encapsulation rule. |
| [trace-ir.md](trace-ir.md) | The linear/structured trace IR per scope: statements, scopes, poisoning, the ratchet. |
| [query-graph.md](query-graph.md) | The salsa database: inputs, tracked queries, the monolithic project index, and what deliberately runs outside the graph. |
| [inference-engine.md](inference-engine.md) | The walk: environments, the store, binding descent and its budget, the effect/throw fixpoints, entry points. |
| [folding-and-sidecar.md](folding-and-sidecar.md) | The `Folder` seam, the JSON-RPC sidecar protocol, its failure model, and the zero-FP contract. |
| [catalog.md](catalog.md) | The builtin catalog: folding allowlist, effect coloring, label registry, generated class hierarchy, failure arms, invocation shapes. |
| [diagnostic-shape.md](diagnostic-shape.md) | The `Diagnostic` value, the registry totality tests, the JSON wire shape, exit codes. |
| [config.md](config.md) | `steins.toml`: every key the binary actually reads, and the designed keys it does not. |
| [baseline.md](baseline.md) | The `.steins-baseline.jsonl` format, the stable hash, the capture surface, staleness. |
| [transform-engine.md](transform-engine.md) | `EditPlan` transactions, the refusal taxonomy, the completeness oracle, regions, obstacles, the vouch valve. |
| [verification-apparatus.md](verification-apparatus.md) | `xtask` fp-gate, the corpus lock, the phpdoc oracle, the catalog generator, conformance. |
| [plugin-contract.md](plugin-contract.md) | **Designed, not implemented.** The plugin contract's shape and the seam that exists for it. |
| [public-surface.md](public-surface.md) | The stability boundary: what is internal (everything), and what the actual compatibility surface is. |

## Related: type semantics

What the analyzer *means* by the facts these contracts carry is normative in
[`docs/type-specification/`](../type-specification/README.md). The two corpora
are complementary: that directory binds the analysis semantics, this one binds
the Rust-side surfaces that satisfy them.
