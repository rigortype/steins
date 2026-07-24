# Steins Type Specification

## Status

Draft, and **descriptive of the tree as it stands** (verified 2026-07-24, at
the v0.1.0 landing point). This directory is the authoritative account of what
Steins' analysis *means*: the value domain, the acceptance relations, guard
narrowing, the object world, effects, throws, and the diagnostic policy those
feed.

Two things distinguish this corpus from a wish-list:

1. Every claim here is meant to be checkable against the code. Where a document
   describes a rule, it names the ADR that decided it and, where useful, the
   crate that implements it.
2. **Unimplemented machinery is written down as unimplemented**, inline, in the
   section it would belong to — never omitted so the document reads complete.
   A section that describes a designed-but-absent surface says so in its first
   sentence. [not-implemented.md](not-implemented.md) collects those in one
   place for a reader who wants the gap list without the semantics.

Analyzer-internal contracts — crate topology, the syntax-tree contract, the
query graph, the sidecar protocol, config and baseline formats, the transform
engine — live in [`docs/internal-spec/`](../internal-spec/README.md). When this
specification and `docs/internal-spec/` describe the same surface, **this
specification binds** for observable analysis behavior, and `docs/internal-spec/`
binds for the Rust-side contract.

Design rationale, rejected options, and open questions live in
[`docs/adr/`](../adr/). When this specification and an ADR appear to disagree
about what the analyzer *does today*, the specification binds and the ADR is
the record of intent; when they disagree about what it *should* do, the ADR
binds and this document is stale. [`docs/ROADMAP.md`](../ROADMAP.md) sequences
the work and holds the milestone exit criteria.

## Conventions

The keywords MUST, MUST NOT, SHOULD, SHOULD NOT, and MAY are to be interpreted
as described in [RFC 2119](https://www.rfc-editor.org/rfc/rfc2119) and
[RFC 8174](https://www.rfc-editor.org/rfc/rfc8174).

Type expressions are written in PHP or PHPDoc syntax where those can spell them
(`int<1, max>`, `non-empty-string`, `array{a: int}`), and in Steins' internal
notation otherwise. Internal notation is identified the first time it appears in
each document. Rust identifiers (`Fact`, `ContractTy`, `Certainty`) name the
implementing type; they are given so a reader can find the code, not because the
name is a stable API.

Status markers used inline:

- **Implemented** — lands in the binary today and is exercised by tests.
- **Partial** — a described subset lands; the section says which part does not.
- **Designed, not implemented** — an ADR decided it; no code produces it.

## Compatibility hierarchy

Steins is not a PHPStan clone, and the order below is the whole reason a
divergence registry exists.

1. **The PHP runtime** is the first-order norm. A finding must correspond to
   something that provably happens when the code runs, on the PHP the project
   actually runs (ADR-0002, ADR-0004, ADR-0011). Where a static reading and the
   runtime disagree, the runtime wins and the finding is dropped.
2. **PHPStan's denotational core** is the second-order norm for *type-operation
   semantics* — acceptance, unions, shapes, refinement keywords (ADR-0030).
   Where Steins departs deliberately, the departure is a numbered entry in the
   divergence registry, not an accident. See
   [divergence-registry.md](divergence-registry.md).
3. **phpstan/phpdoc-parser** is the normative *grammar* for PHPDoc type
   expressions (ADR-0029), enforced mechanically by a differential oracle
   against the real parser.
4. **Psalm, TypeScript, Rigor** are design references used to find missing
   concepts. They are not compatibility targets.

Where PHPStan's *worst-case* reading would produce a finding that the runtime
does not justify, Steins is silent. This is not a strictness setting; it is the
zero-false-positive bar (ADR-0002, ADR-0013), and no config knob relaxes it in
the other direction.

## Reading order

Foundational definitions first; specific surfaces build on them.

| Document | Scope |
| --- | --- |
| [overview.md](overview.md) | Core principle (call-site value propagation), the proof/contract/mechanics/debug split, what v0.1.0 ships. |
| [certainty.md](certainty.md) | The one trinary (`Yes`/`No`/`Maybe`), Kleene composition, and the silence discipline. |
| [value-domain.md](value-domain.md) | The four-layer value domain: Singleton / OneOf / Refined / General, joins with computed widening, membership. |
| [contract-types.md](contract-types.md) | PHPDoc types lowered to `ContractTy`, the acceptance relation, list/shape semantics, the opaque floor. |
| [phpdoc-grammar.md](phpdoc-grammar.md) | The accepted PHPDoc type grammar, the tag surface, and the differential oracle that binds them. |
| [trust-stratification.md](trust-stratification.md) | Proven beats declared; the `Verified`/`Asserted` strata and the derivation clause. |
| [narrowing.md](narrowing.md) | Guards, branch analysis, the three fact lanes, arm-wise subtraction, and what narrowing does *not* cover. |
| [object-model.md](object-model.md) | The store-based heap, escape sets, readonly immunity, the trinary is-a oracle, enums. |
| [closures.md](closures.md) | Closures as values, invocation shapes, callable-signature variance. |
| [effects.md](effects.md) | Effect labels, the registry, prefix subsumption, envelopes, origin closure, Liskov. |
| [throws.md](throws.md) | Checked/unchecked accounting, escape propagation, damming, `@throws` envelopes and their Liskov rule. |
| [dynamism.md](dynamism.md) | The unanalyzability posture: `eval`, dynamic include, the dam, and what absence proofs require. |
| [diagnostic-policy.md](diagnostic-policy.md) | The id registry, layers, facets, profiles, suppression channels, baseline. |
| [divergence-registry.md](divergence-registry.md) | Deliberate departures from PHPStan semantics, and the conformance-suite standing. |
| [not-implemented.md](not-implemented.md) | The honest gap list: designed surfaces with no code, and known imprecision. |

## Related: analyzer-internal contracts

The Rust-side surfaces that satisfy these semantics — the syntax-tree contract,
the linear trace IR, the salsa query graph, the folding seam, the sidecar
protocol, config/baseline formats, and the transform engine — are specified in
[`docs/internal-spec/`](../internal-spec/README.md).
