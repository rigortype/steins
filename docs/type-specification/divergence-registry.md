# The Divergence Registry

**Status: implemented as policy** — every entry below corresponds to shipped
behavior or a recorded refusal. ADR-0030.

## What this is

Steins tracks PHPStan's *denotational core* for type-operation semantics
([overview.md](overview.md), compatibility hierarchy). Where it departs
deliberately, the departure is a numbered entry here rather than a surprise in a
diagnostic. Entries are deliberate and justified; this is not a defect list.

The governing rule, from ADR-0030's amendment:

> Vocabulary and minor judgments track PHPStan's model — familiarity is cheap
> and compounding. But when a decision touches the *nature of the inference* and
> a fundamentally better outcome is in reach, Steins replaces the PHPStan
> approach without hesitation. The registry is what makes that boldness safe.

The precedents for "without hesitation" are call-site propagation over modular
analysis (ADR-0001) and no template solver where propagation reaches (ADR-0032).

## Core semantic entries

**1. Two acceptance relations, not one.** Declared-contract acceptance (PHPDoc)
is pure set semantics with **no coercion**; runtime acceptance is PHP's own rule
under the calling file's strict mode. PHPStan runs one relation with coercion
rules folded in. See [contract-types.md](contract-types.md).

**2. Array-shape / list semantics follow phpstan/phpstan#14939.** `array{}` is
an order-agnostic key *set*, `list{}` a positional key *sequence*, and
`list<T>` requires keys exactly `0..n-1`. Steins implements the RFC's
resolution, which current PHPStan does not.

**3. No benevolent unions.** `BenevolentUnionType` compensates for worst-case
false positives that a proof layer does not emit. The grammar is accepted
(`__benevolent<A|B>` parses, with a provenance flag), the semantics erased — it
is a plain union. Failure-arm labels (`failure.environment`, `failure.input`,
`failure.resource`; ADR-0042) are the replacement mechanism where the real need
is "why does this arm exist".

**4. No narrow-LHS `accepts` strictness.** PHPStan's `accepts` answers a
worst-case question about declared types. Steins keeps a single overlap relation.

**5. Semantic type equality is mutual subsumption only.** No structural
equality, no provenance-sensitive equality. Provenance-flavored types are barred
from the normalizer's arm vocabulary by the type system, so equality cannot be
asked about them ([contract-types.md](contract-types.md)).

**6. No type-combinator layer.** Combination happens in the value lattice; the
type-side normalizer was *extracted from the honesty renderer* rather than built
as a parallel `TypeCombinator`/`TypeUtils` stack (ADR-0030 amendment, discharged
by ADR-0052 slice N1).

## Conformance-suite divergences (intentional silences)

Steins runs `php-typing-conformance`. Standing at the last recorded triage
(2026-07-24): **85/98**, with every remaining non-#14939 failure registered
below as either a standing refusal or an honest deferral, and zero
absent-machinery failures among them at the time of triage. The ceiling is set
by the intentional entries.

**1. Vendor-prefixed tags — standing refusal.** Only `@phpstan-*` / `@psalm-*`
prefixes carry contracts (ADR-0029). `@phan-param` and other tool-specific tags
are erased. PHPStan *does* consume `@phan-param` on the relevant fixture, so this
is a registered divergence from PHPStan's actual behavior; the tool-tag scope is
deliberate and stands.

**2. No declaration-coherence lints — standing refusal (shared with PHPStan).**
"Native `?string` wider than `@param string`" is not reported: the code is
type-safe, and a proof layer speaks on proven value breaks, not declaration
style. Tolerating native-nullable widening is deliberate (the `$x = null`
idiom). At most a future policy profile, never core. PHPStan fails these fixtures
by design too (phpstan/phpstan#7572), so this is a shared refusal.

**3. `static`/`self` return-position acceptance — deferral discharged; the
conditional shapes are now a standing refusal.** Return position landed via the
minimum-bound lemma: every late-bound class `T` satisfies `is_a(T, C) = Yes`, so
an exact returned class with `is_a(V, C) = No` fails *every* possible `T` — an
unconditional runtime `TypeError`, reportable under `type.return-mismatch` with
no worst-case reasoning. What stays out: `new self()` under `: static` in an open
class (breaks only on proper-descendant receivers — PHPStan reports it by
worst-casing) and sibling-subclass returns.

**4. No `resource` type nor resource-value tracking — honest deferral.**
`resource` is not a native type; a `resource $x` hint references a non-existent
class. Rejecting call sites would need `fopen()`-style resource *values* modeled
through `=== false` narrowing. Neither exists yet. This sits in the non-scalar /
object-world value-modeling cluster.

## Not registered — just unimplemented

These are gaps, not divergences, and they are tracked in
[not-implemented.md](not-implemented.md):

- generic type-argument carry (ADR-0032 stage 1);
- callable signatures beyond the closure-variance arm;
- template scope transfer (ADR-0051).

Native **object** acceptance — single classes, unions, enum cases, class
constants, and `A&B` intersections — has landed, along with `instanceof`,
offset-access, and undefined-method finding kinds.
