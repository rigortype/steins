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

PHPStan's strictness is a numeric ladder, with possibly-grade offset
reports additionally gated behind config flags
(`reportPossiblyNonexistentConstantArrayOffset` and its general-array
sibling). Steins gives each diagnostic a **semantic layer** — proof
(runtime breakage, zero-FP) / contract (declared debt) / mechanics
(anti-rot) / debug (requested introspection) — and makes strictness an
opt-in through **named stages** (`default` → `throws-direct` → `contracts`
→ `strict`), per the lenient-default principle (ADR-0050/0053). Each id
carries one `surface_floor` attribute placing it on that cumulative ladder
(ADR-0062 A-G10); the possibly-grade offset ids ship measurement-first —
the triage instrument measures a project before the surface is enabled,
because zero-FP means *calibrated defaults*, never omitted checks. Numeric
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
silences are named. Runtime-first is the release posture, not the last
word: handling **lower-version signature diffs** (library-range checking)
is an intended later direction — deferred, not refused, and deliberately
unprepared-for in the current implementation. What stays refused is
emulating versions the project does not run.

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

The conditional-purity chapter diverges the same way (ADR-0063, pending
ratification): PHPStan's endorsed fix for higher-order purity is a
*declared* contract (`@pure-unless-callable-is-impure`) because modular
analysis cannot see the callback; Steins answers **semantically first** —
a callback-position catalog joins the callee envelope with the visible
callback's envelope through the existing fixpoint — and consults the
declared conditional form only for opaque `callable` parameters. By-ref
out-params get a `mutate.local` effect color that Pure envelopes tolerate,
instead of the per-function `hasSideEffects` flag PHPStan's maintainers
rejected twice as a lie.

## ConstantArrayType vs order-witnessed values + order-declared shapes

PHPStan's `ConstantArrayType` is one class carrying declared key order,
`optionalKeys`, `nextAutoIndexes` and an `isList` flag — and it trusts the
declared order inconsistently: acceptance is order-insensitive, while the
positional projections (`array_keys`, `array_values`, `array_slice`,
`array_reverse`) read declaration order as runtime order, a documented
real-FP class. Steins splits the truth by **provenance** (ADR-0062): the
value lane holds **order-witnessed** concrete arrays (insertion order
observed — order-dependent results are sound there and only there), and
one canonical **shape fact** — fields (key, presence with its own trust
stratum, value slot) + sealed/unsealed tail + denotational `isList`
trinary + non-emptiness + key covers — is the fifth fact form, the single
degenerate home of `array` / `array<K, V>` / `list<T>` / `array{…}`.
Lifting a concrete array into the shape world is where order-witnessed-ness
is honestly lost. Positional projections over shape-only truth take the
sound widening, never declaration order — except where the shape's own
`isList` fact is `Yes`, which is **realizable order**: every admitted value
has keys `0..n-1` in that sequence, so `array_values`/`array_keys`/
`array_reverse` consume it exactly. Order is consumed only where it is a
semantic guarantee (a proven sequence), never a declaration artifact — the
line the FP class above crossed. The #14939 model (`array{…}` a
key *set*, `list{…}` a key *sequence*, `isList` computed over the admitted
value set) runs natively, ahead of PHPStan stable — including `list{…}`
acceptance rejecting permutations and the *rendering*, which follows the
model rather than the oracle: a sealed shape's head keyword is its own
`isList` — `list{…}` for a proven key sequence, `array{…}` otherwise — so
what we print round-trips to the fact we printed. PHPStan stable's
`ConstantArrayType` conflates the two (its `array{A, B}` retains key order
and so means our `list{A, B}`), and the cost of not conflating them is
named: the nsrt headline `match` count falls, with those rows landing on
`subsumed` — "narrower than the assertion", the correct verdict when we
state a sequence and the oracle states a set. Key *layout* is still spelled
the way PHPStan spells it (positional fields for contiguous required keys,
every key printed otherwise, `non-empty-` dropped where a required key
implies it, the empty shape `array{}`), and unsealed shapes are untouched.
Declined
with reasons: an abstract `nextAutoIndexes` (concrete-only, version-aware),
and `ARRAY_COUNT_LIMIT`-style union degradation (replaced by the computed
OneOf descent; the 256 constant survives only as the single-shape
field-width bound).

## Expression-keyed narrowing vs cover facts + arm subtraction

PHPStan's Scope keys narrowings by expression, so it cannot carry the
disjunctive fact `isset($x['a']) || isset($x['b'])` into the right arm of
`$x['a'] ?? $x['b']` — the motivating false positive this work imports and
fixes. Steins records a **KeyCover** on the shape fact itself — an
antichain of key sets in two flavors, `Isset` (non-null member exists) and
`KeyExists` (member exists, possibly null), with genuinely different
discharge strength at `??` (a KeyExists cover discharges only over
non-nullable slots, because present-null falls through at runtime).
Discriminated unions live in the arm lane and are narrowed by
**subtraction**: sealed-powered `isset` discrimination and tag
discrimination (`match`/`===` on a constant-key projection, judged by the
field contract's `admits`), collapsing to a single shape fact when one arm
survives (ADR-0062 A-G3/A-G4/A-G8/A-G11).

## DynamicReturnTypeExtension vs five named seams

PHPStan ships per-call return computation and guard narrowing as
runtime-pluggable extension classes
(`Dynamic*ReturnTypeExtension`, `*TypeSpecifyingExtension`). Steins builds
**no extension mechanism** for this (ADR-0064, pending ratification):
every imported behavior classifies into exactly one of five existing
seams — sidecar folding, symbolic argument-dependent transfer rules,
probe-gated curated return rows, the plugin surface (framework dynamics),
or the guard vocabulary — with the import queue ordered by conformance
rows and corpus frequency, not taste. A sixth open-ended hook would be a
second extension mechanism competing with the plugin contract, so it is
refused.
