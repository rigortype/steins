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
where it does not, silence. What Steins does instead of solving is
*read*: a `@param Owner<…, T, …>` at the top level asks the argument's
generics carry what sits at `T`'s position, so `@return T` names it —
one positional lookup, no unification, no fixpoint, and all-or-nothing
across every occurrence of a name — one the read cannot perform
(`\Closure():T`, `list<Box<T>>`) contests it rather than being skipped,
so the answer is never narrower than the declaration supports. Always
underneath the body summary, which wins wherever it speaks. The accepted
cost — thin library-author lints, plus the nested and bounded positions
PHPStan solves and Steins leaves silent — is on the registry.

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

The docblock spelling of purity is not purely divergent, though: ADR-0082
reads `@phpstan-impure <labels>` and `@phpstan-pure` as **interop
envelopes** — an unchecked, checkable spelling of the same envelope
concept, at the parameter position PHPStan's own author sketched for the
tag — and `steins transform effects-envelope` writes them back from a
project's own proven effects, so the bridge runs both ways. A bare
`@phpstan-impure` stays unread (⊤ carries no information); the
parameterized and class-level forms enter the declared lane and are
contract-checked against their own declaration, and a label outside
Steins' registry reads the *whole* tag as unspecified rather than a
narrowed bound (owner ruling, 2026-08-12) — a typo can go quietly, the way
one already does under current PHPStan, rather than fail a run the way an
unknown label does under `#[\Steins\Effect]`. See
[phpdoc-effects-interop.md](type-specification/phpdoc-effects-interop.md).

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

## "Always evaluate to true" on an arm condition vs the complement pair

PHPStan reports the last covering arm of an exhaustive `match`/`if` chain
as a condition that "will always evaluate to true". Steins registers no
`condition.*` family and emits nothing there, permanently (ADR-0088 §6).
Where the subject's type is Verified, the redundancy is the *point* of an
exhaustive chain and the diagnostic pushes the author toward deleting
their own safety net; where it is Asserted, the claim rests on a docblock
the runtime never checks, which is the `treatPhpDocTypesAsCertain`
divergence above applied to arm conditions. What Steins reports instead is
the complement PHPStan is silent about: the `default` arm the chain
provably exhausts (`match.dead-default`, and `phpdoc.dead-default` at the
pedantic floor), the case analysis that has stopped being exhaustive
(`phpdoc.never-param-reachable`, read off a `@param never` **sentinel
parameter**), and the `\UnhandledMatchError` an uncovered `default`-less
`match` throws, as an `origin = direct` contribution to `throw.undeclared`.
A dead arm whose body only terminates — the **defensive terminator** — is
never reported by any of them, extending ADR-0019 §2's live-`exit` ruling
to the dead case.

## An unknown *class* vs unknown *vocabulary* in the hyphen space

PHPStan, Psalm and Mago read an unrecognized phpdoc type identifier as a
class reference and resolve it against the file's namespace, so
`@param int-range<0, 255>` in a namespaced file becomes a reference to
`Conformance\Tests\…\int-range` and the tools report an unknown class —
Psalm and Mago additionally **over-reject** the valid calls, because a
nonexistent class accepts nothing. ADR-0091 §2 measures 84 such
over-rejected lines across 48 fixtures of the cross-tool conformance
suite. Steins reserves the whole hyphen space instead (ADR-0091 §3): PHP's
compiler rejects `-` in a class, interface, trait or enum name, so a
hyphenated identifier in a type position **is type vocabulary and never a
class reference** — no namespace resolution, no shadowing, and never a
`ContractTy::Class`. What it lowers to is `Opaque`, which admits every
value as `Maybe`, so the docblock rejects nothing it was written to accept.

What Steins reports there is `phpdoc.unknown-vocabulary` (ADR-0091 §6): a
hyphenated name that survives the `@template` shadow and is not recognized
vocabulary. The claim is deliberately weaker than the one it replaces and
therefore stronger: not "no such class exists", which nothing can prove
from a docblock, but "this spelling denotes nothing" — which the compiler's
own naming rules do prove. The remaining possibilities are a misspelling of
vocabulary and vocabulary from a tool Steins does not model, and neither is
a false claim about the program. PHPStan, notably, is not in the divergence
on the resolution half: it reports `parameter.unresolvableType` and
declines to manufacture a contract, which is the behavior this rule adopts
and generalizes.

Two consequences of §4.1's ruling ride with it. A user-defined type alias
may be named `foo_bar` and may **not** be named `foo-bar`: phpstan/
phpdoc-parser accepts `@phpstan-type foo-bar = int` and declares an alias
by that name, and Steins refuses the declaration instead, because the
hyphen space is reserved. And the space is reserved rather than frozen — a
plugin registers utility types into it through the existing
`steins-plugin.json` manifest (ADR-0039/0068), the way a PHPStan extension
adds type resolution the core does not ship.

That extension channel makes this id's finding set **plugin-set
dependent**, which is a baseline that moves with *configuration* rather
than with code, and ADR-0022's baseline discipline is told rather than left
to discover it. The allowlist is builtin tables ∪ plugin registrations and
is computed after plugin load; dropping a plugin from a project therefore
introduces findings on every docblock that used its vocabulary, with no
source change anywhere. That is the correct answer — the vocabulary really
did go away — but a baseline captured before the change will show the new
findings as regressions rather than as dormant entries, and the remedy is
to recapture, not to suppress. The registration kind is not on the manifest
yet, so the allowlist is builtin-only today and the coupling bites when it
lands.

## Bare `@assert-if-true` vs the vendor-prefixed assertion family

PHPStan's and Psalm's assertion tags — `@phpstan-assert(-if-true|-if-false)`
and their `@psalm-assert*` counterparts — exist only in vendor-prefixed form;
neither tool defines a bare `@assert-if-true`. Steins' phpdoc scanner follows
the same rule, generalized rather than special-cased: only `@phpstan-*` /
`@psalm-*` prefixes carry a contract (ADR-0029), and the assertion family is
recognized through that uniform prefix strip like every other tag — the same
doctrine already on the registry as the vendor-prefixed-tags standing refusal
(`type-specification/divergence-registry.md` entry 1). ADR-0074 §2, written
for the unrelated `@psalm-trace` tag, states the assertion case as its own
precedent verbatim: "`@phpstan-assert` / `@psalm-assert` exist, bare
`@assert` does not." A bare `@assert-if-true` is therefore not a recognized
tag at all, and narrows nothing.

The 2026-08 conformance rescoring exercised this on
`regressions_string_narrowing_assert_if_true`, whose fixture writes the bare
spelling and expects the narrowing a prefixed tag would carry. Steins reports
none, by the standing rule above — a recorded design decision, not a
capability gap. Issue #266's queued "assert-tag consumption" work covers only
the prefixed spellings (`@phpstan-assert`, `@phpstan-assert-if-true`,
`@phpstan-assert-if-false`) and would not change this fixture's verdict when
it lands.

## An uncovered default-less `match` vs a standalone exhaustiveness finding

For a `match` with no `default`, an exhaustiveness-minded tool can treat the
uncovered residue as a finding in its own right. Steins computes the same
fact — the arms do not exhaust the subject's Verified domain — but does not
surface it that way. ADR-0088 §5 decided this exact surface: an uncovered
default-less `match` throws `\UnhandledMatchError` when the missed value
arrives, so it is a throw like any other and enters the throw accounting as
an `origin = direct` contribution to the existing `throw.undeclared` id
(`Layer::Contract` / `Floor::Contracts`). No new id: a bare `steins check`
stays quiet — the `match` is reachable, not proven, and the crying-wolf
constraint gives it no claim on the default surface — and only a project
that has opted into throw accounting learns that the function can throw
something it does not declare.

The 2026-08 conformance rescoring measured this against
`regressions_backed_enum_value_narrowing`: the fixture's scored half is a
default-less `match` over a two-case backed enum, covering one case, with no
`@throws` declared. Steins reports nothing without the throw-accounting
opt-in, and `throw.undeclared` under it — never a standalone exhaustiveness
finding on the default surface — which is the by-design verdict above, not a
gap. The fixture's other half is unscored: `$s->value === 'H'`-style
case-identity narrowing off a backed enum's value comparison is a real,
separate capability, tracked in its own issue (#540).
