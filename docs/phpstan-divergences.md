# Structural divergences from PHPStan

PHP;STEINS (`steins`) is an imitation, so naturally most of it leans on
PHPStan — but *the Organization's conspiracy* forced a number of internal
structures to depart, unavoidably and substantially.

Each item below reads PHPStan's shape → the forcing reason → Steins' shape,
with the deciding ADR. The binding record is the
[ADR-0030 divergence registry](adr/0030-type-semantics-phpstan-core-divergence-registry.md)
and [type-specification/divergence-registry.md](type-specification/divergence-registry.md);
this document is the narrative companion and loses on conflict.
日本語の詳細版: [docs/ja/phpstan-divergences.md](ja/phpstan-divergences.md).

## Type hierarchy + TypeCombinator vs the four-layer value domain + syntactic arms

PHPStan's types form a rich `Type` class hierarchy normalized by
`TypeCombinator`, with `Type::equals` beside `isSuperTypeOf` and accessory
types composed as intersections. Steins puts the truth on the value side:
runtime-observable value sets live in the four-layer domain
(Singleton / OneOf / Refined / General, ADR-0035), declared types stay
**unnormalized syntactic arm lists** judged arm-wise through one acceptance
relation in a trinary Certainty (ADR-0030). There is no type-combination
algebra — joins belong to the value domain; type equality exists only as
mutual subsumption, with provenance-flavored types barred from the equality
vocabulary at the type-system level (registry entry 5). The normalizer that
does exist was **extracted** from the honesty renderer when narrowing needed
it (`steins_contract::normalize`, ADR-0052 N1) — never built up front.

## Levels 0–9 vs layers + named stages

PHPStan's strictness is a numeric ladder. Steins gives each diagnostic a
**semantic layer** — proof (runtime breakage, zero-FP) / contract (declared
debt) / mechanics (anti-rot) / debug (requested introspection) — and makes
strictness an opt-in through **named stages** (`default` → `throws-direct`
→ `contracts`), per the lenient-default principle (ADR-0050/0053). Numeric
levels are refused: stages have names and definitions, not numbers.

## treatPhpDocTypesAsCertain vs the trust stratum

PHPStan toggles docblock certainty globally. In Steins the trust order is
fixed (ADR-0037): facts carry a **checked stratum bit** — Verified (native
declarations, executed guards) or Asserted (docblock claims) — and every
derivation inherits the minimum stratum (ADR-0052 N2). Proof-layer
diagnostics require all-Verified premises, so a lying `@phpstan-assert`
cannot forge a proof. There is no toggle: configuration selects reporting
surfaces, never inference.

## ignoreErrors regexes vs the id registry + baseline

PHPStan suppresses by message regex; Steins registers ids `(id, layer)`
(ADR-0022) and allows exactly three channels — inline `@steins-ignore`
with rot detection, the JSONL baseline with a capture-surface header and
dormant entries, and scoped policy (ADR-0023). Message wording is not a
contract.

## Version emulation vs ask-the-real-thing

PHPStan emulates PHP versions from signature maps. Steins **asks the PHP
the project actually runs** (ADR-0004/0024): a resident sidecar does
constant folding, environment facts (version, SAPI, extensions), and the
existence oracle (`reflect`); the builtin catalog is never an absence
oracle (ADR-0049 §1). No sidecar means a quieter *sound subset* whose
silences are named.

## Optimistic maybe-reporting vs the zero-FP proof layer

PHPStan reports "probably broken" broadly and compensates (benevolent
unions). Steins' proof layer reports **definite No only** (ADR-0002):
absence claims require complete enumeration — dams, homonyms, conditional
declarations, enums, monkey-patch extensions are all written silence legs
(ADR-0049) — and maybe stays silent. The acceptance test: zero false
positives across 14 held-out real applications, ~237k files
(notes/20260724-adoption-drill-record.md).

## A call-site template solver vs transparent templates

PHPStan unifies template variables at call sites. Steins has no solver
(ADR-0032): where value propagation reaches, templates are transparent;
where it does not, silence. The accepted cost — thin library-author
lints — is on the registry.

## ImpurePoint vs Effect System

PHPStan enumerates a body's impure spots as `ImpurePoint`s to check
`@phpstan-pure`: evidence collection with a flat notion of impurity.
Steins grew this into a second inferred dimension (ADR-0005/0018):
effects are **hierarchical dot-path labels** (`io.filesystem.read`) in an
open registry with prefix subsumption, functions carry **envelopes**
(`#[\Steins\Effect]` / `#[\Steins\Pure]`) as declared upper bounds, and
inference tracks envelope excess and Liskov widening through a
via-provenance fixpoint. Where ImpurePoint gathers evidence of impurity,
the Effect System *types* side effects — forced by this project's end
goal of structurally separating effectful code from testable code.
