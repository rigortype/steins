# Contract Types and Acceptance

**Status: implemented** (`steins-contract`; ADR-0029, ADR-0030, ADR-0038).

A **contract type** is a PHPDoc type expression lowered into a small semantic
form (`ContractTy`) and judged against the value domain. It is the bridge
between the *syntactic* PHPDoc AST and the *extensional* facts of
[value-domain.md](value-domain.md).

## Two acceptance relations, never mixed

Steins runs two distinct acceptance judgments, and confusing them would be a
false-positive vector.

| Relation | Governs | Coercion | Layer |
| --- | --- | --- | --- |
| **Runtime acceptance** | native parameter/return/property types, checked by PHP itself | yes — under the *calling file's* `declare(strict_types=1)` | `proof` |
| **Contract acceptance** | PHPDoc `@param`/`@return`/`@var` types, never checked at runtime | **none** — pure set semantics | `contract` |

Under contract acceptance a numeric string `"5"` does **not** satisfy `int`.
Under runtime acceptance in weak mode it does. The two never share a code path.

This document specifies contract acceptance. Runtime acceptance is PHP's own
rule, applied per the calling file's strict mode against PHP 8.1+ semantics
(ADR-0011).

## Lowering

`lower(&Type) -> ContractTy` is **total**: every parsed PHPDoc AST lowers, with
`ContractTy::Opaque` as the honest floor. Keywords normalize into a small
semantic vocabulary so acceptance is Kleene composition over a handful of leaf
rules instead of a keyword zoo:

- `scalar` → the union of the four bases.
- `positive-int`, `negative-int`, `non-negative-int`, `int<lo, hi>` → `IntIn`.
- The **refined-string grid** (issue #240) → `StrWith`: a core rung ∈ {—,
  `non-empty-`, `non-falsy-`, `numeric-`, `non-falsy-numeric-`} followed by a
  casing rung ∈ {—, `lowercase-`, `uppercase-`, `uncased-`}, suffixed `-string`.
  Twenty cells, one word each — `non-falsy-lowercase-string`,
  `numeric-uppercase-string`, `uncased-string`, … — and the grid is exactly what
  the speller emits, so every cell round-trips through the lowering it came from
  (`every_grid_cell_round_trips`). `non-falsy-numeric-` is its own core rung
  because `numeric-string` does **not** entail `non-falsy-string` (`'0'` and
  `'0.0'` are numeric and falsy). The casing rung is `strtolower($s) === $s` (an
  identity, so an uncased string satisfies both halves — which is what `uncased-`
  names) and is orthogonal to the length/falsy axis.

  **Divergence (ADR-0030).** PHPStan spells these sets as intersections
  (`lowercase-string&uppercase-string`, `non-falsy-string&numeric-string`) and
  parses only the `non-empty-` casing compounds as single words. Steins keeps a
  single identifier per cell: the acceptance relation judges the two spellings
  equal (`lower` maps `A&B` to `Inter`, and an all-`StrWith` intersection folds
  to the same predicate set), while a compound keyword is what the emitted phpdoc
  can lower back through. The cost is that `uncased-string` and the
  `non-falsy-`/`numeric-` casing compounds are not PHPStan vocabulary — PHPStan
  reads them as class names.

  **That cost is accepted deliberately, by owner ruling (2026-08-08), with the
  full grid kept.** It is not a detail of the implementation: `transform` and
  `annotate` write these words into the project's own docblocks, so a project
  running both tools will see PHPStan reject an annotation Steins authored. The
  alternatives — trimming the grid to the `non-empty-` compounds, or lowering
  only the emitted spelling to a PHPStan-parseable cell — were weighed and
  declined in favour of one vocabulary that says what the domain actually holds.
  Steins' vocabulary is Steins' (ADR-0030); the acceptance relation still judges
  PHPStan's intersection spelling equal to the cell, so *reading* PHPStan-shaped
  annotations is unaffected — only what Steins *writes* diverges. An adopter who
  needs PHPStan-parseable output should not run the docblock-writing transforms
  until that is offered as a choice.
- `decimal-int-string`, `non-decimal-int-string`
  → `StrWith`, and NOT part of the grid above (neither is a spelling the speller
  emits; both widen to the cell their closure names).
  The array-key-cast pair is the engine's own `ZEND_HANDLE_NUMERIC_STR` rule —
  `'123'` becomes an `int` key, `'007'`/`'+1'`/`'-0'` and anything past
  `PHP_INT_MAX` stay string keys — so `decimal-int-string` is strictly narrower
  than `numeric-string` and entails it (plus, its alphabet being uncased, both
  casings). It does *not* entail `non-falsy-string`: `'0'` is one.
  `non-decimal-int-string` is the complement **within `string`**, so it is far
  wider than its name (`''`, `'foo'`, `'1.2'`, `'18E+3'` all qualify).
  `StrPreds` is a conjunction over positive literals, so the two are two bits
  rather than one bit and a negation: a *proven value* is decided exactly, while
  an *abstract fact* carrying one bit answers `Maybe` against the other — sound,
  and the honest ceiling of a lattice with no negation.
- `non-null-mixed`, `non-empty-mixed` → `MixedMinus(MixedCut::{Null, Falsy})`,
  and `non-empty-scalar` → `Inter[scalar, MixedMinus(Falsy)]`. The only
  **negative** leaf in the vocabulary: neither spelling is a union of the forms
  above, because the value lattice has no object inhabitant (so "anything but
  null" cannot be enumerated) and no float refinement (so "float minus `0.0`"
  cannot be spelled). The cut is a *value* predicate — `php_is_falsy` is the
  definition — so a proven value is decided exactly; against an abstract fact it
  decides only where the fact's own refinement answers (a `non-falsy-string`, an
  int range missing zero) and is `Maybe` elsewhere. Deliberate divergence:
  PHPStan resolves `non-empty-scalar` to `float|int<min, -1>|int<1, max>|
  non-falsy-string|true` and so stays silent on `0` and `0.0` (its `float`
  member is never narrowed and int-is-accepted-where-float-is-expected lets both
  back in); Steins spells the subtraction and rejects them.
- `class-string`, `interface-string`, `trait-string`, `enum-string` →
  `StrWith(CLASS_STRING)` — a value property, judged like any other string
  refinement (issue #236).
- `literal-string`, `callable-string`, `numeric-int-string` → `StrOpaque`.
- `list<T>`, `non-empty-list<T>` → `ListOf`; `array<K, V>`, `T[]` → `MapOf`;
  `iterable<K, V>` → `IterableOf`.
- `array{…}` / `list{…}` → `Shape`.
- `callable`, `Closure`, `callable(P): R` → `CallableTy(Option<CallableSig>)`
  — `None` for the bare forms, `Some(sig)` carrying the lowered parameter and
  return contracts. A template-bearing signature (`callable(T): T`) drops to
  `CallableTy(None)`, so every carried signature arm is a ground contract.
- `A|B` → `Union`; `A&B` → `Inter`.
- A class or interface name → `Class(fqn)`, normalized (lowercased, leading `\`
  stripped). A generic class reference (`Collection<T>`) lowers to the same
  `Class(fqn)` — the type *arguments* are not a `ContractTy` concern: proven
  argument *values* ride the check-time value carrier and are judged at the
  direct-`new` argument position (ADR-0032 stage 1; see
  [object-model.md](object-model.md)).
- `template-type<Subject, Owner, 'TName'>` → `Opaque`, at *every* arity: it is
  known vocabulary this lane models no relation for yet (issue #360; resolution
  is issue #361). The name is checked before the argument count, so PHPStan's
  error-type arities floor here silently too (a registered divergence). Without
  the entry it would read as a class named `template-type` — a nonexistent-class
  reference, the same wrong-`No` hazard `key-of`/`value-of` solve — which is why
  the floor is `Opaque` and not `Class`.
- Conditionals, offset-access types, const fetches, `$this`/`self`/`static`,
  templates, and anything the parser marks unsupported → `Opaque`. A
  **template name in scope shadows the class universe** for its own
  declaration's docblock types (issue #5): a bare, unqualified name declared
  by `@template` on the declaration or its enclosing class-like lowers
  `Opaque` even when a real class of that name exists. The shadow match is
  deliberately case-insensitive — over-shadowing only ever silences — and a
  `\`-qualified or namespaced reference opts out and still resolves to the
  class.

## The judgment

Two entry points, both returning [`Certainty`](certainty.md):

- `admits_val(ty, &Val)` — is this concrete value in the contract's denotation?
- `admits_fact(ty, &Fact)` — is *every* value the fact denotes admitted?

Everything is Kleene composition: `and` for "all conditions hold", `or` across
union members, and an all-members fold for "every possible value".

Only a definite **`No`** is ever reported. `Maybe` is silence.

The abstract-fact path uses a documented sound under-approximation: a union that
only *jointly* covers a base — `int<min,0>|int<0,max>` against a general `int` —
answers `Maybe`, never a wrong verdict. Steins would rather be silent than
compute a joint-coverage decision it cannot justify.

## Leaf rules worth knowing

These are the places where "obvious" would be wrong.

**`float` accepts `int`.** PHPStan core semantics, and PHP's own widening; an
int value satisfies a `float` contract with `Yes`.

**Float literal types compare by PHP value equality.** Int `5` satisfies the
literal type `5.0` (IEEE `==`), deliberately unlike the domain's set equality
where `5` and `5.0` are distinct values.

**`mixed` admits everything, including null. `never` admits nothing.**

**Provenance-flavored string types can never answer `Yes`.** `literal-string`
and `callable-string` lower to `StrOpaque`: a non-string is `No`, a string is
`Maybe`. Membership in these types is a fact about where a value *came from*,
not about the value, and Steins does not do taint tracking (ADR-0038). It
reserves value-provenance labels as the general mechanism, unimplemented.

**`class-string` is not one of them.** Naming a class-like is a property of the
value, so it lowers to a `CLASS_STRING` refinement instead (issue #236,
ADR-0038's amendment): it **refutes** the strings PHP's identifier grammar rules
out (`''`, `'0'`, `'123'`) and **satisfies** `string`/`non-empty-string`/
`non-falsy-string`. What it still never answers is `Yes` for a concrete
identifier — whether `'App\User'` is in the class table needs the class table,
which `StrPreds::of` has not got — so that stays `Maybe`. `class-string<T>`
drops its bound and is carried as plain `class-string` (issue #10).

**`callable` is `Maybe` for strings and arrays.** A string may name a function,
a two-element array a method; other scalars are `No`. A declared
`callable(P): R` **signature** is not consulted by value/fact acceptance at all
— a runtime string value cannot be judged against a call shape. The signature is
consumed only by the closure-argument variance check
([closures.md](closures.md)).

**`Opaque` is always `Maybe`.** By construction, never by omission.

## Arrays, lists, and shapes

`array{…}` and `list{…}` are specified per PHPStan issue #14939, a deliberate
divergence entry (ADR-0030 — see [divergence-registry.md](divergence-registry.md)):

- **`array{…}` is an order-agnostic key *set*.** Positional fields get keys
  assigned automatically (`array{int, string}` has keys `0`, `1`), but matching
  is by key, not by position.
- **`list{…}` is a positional key *sequence*.** `list<T>` additionally requires
  keys to be exactly `0..n-1`.
- **Sealed shapes reject extra keys.** An unsealed tail (`...<K, V>`) admits
  extras against the tail contract.
- **Optional fields** (`a?: int`) may be absent.

Acceptance of an array value requires the *whole array* to be known — see
`Val::Array` in [value-domain.md](value-domain.md). A partially-known array has
no fact, so it is silent.

## The normalizer

`steins-contract::normalize` is the type-side normalizer (ADR-0052 §4),
**extracted from the honesty renderer's dedup/subsumption logic rather than
built as a fresh combinator layer** — the explicit discharge of ADR-0030's
"no TypeCombinator/TypeUtils layer" refusal.

Types stay syntactic **arm lists**, judged arm-wise through the single
acceptance relation above. The module adds no parallel judgment; `subsumes`
reduces an arm to the denotation query acceptance already answers. Its surface is
final:

| Function | Role |
| --- | --- |
| `subsumes(a, b)` | pairwise arm subsumption, trinary |
| `arm_eq(a, b)` | semantic type equality — **defined only** as mutual subsumption (ADR-0030 registry entry 5) |
| `dedup_arms(arms)` | order-stable dedup + subsumption collapse |
| `summarize_vals(vals)` | proven value set → normal-form arm list |
| `subtract(arms, subtrahend)` | arm-wise negative narrowing (see [narrowing.md](narrowing.md)) |

There is deliberately **no** `union(A, B)` and no generic `remove(T, S)`: joins
stay the value domain's job. Provenance-flavored arms are barred from the
normalizer's vocabulary by the type system — `ContractTy` carries no provenance
slot — so the equality rule cannot be violated by review error.

Every function is **pure** in its arguments: no inference, no cross-scope
coupling, no whole-project ordering dependence (the ADR-0048 constraint that
keeps position queries reachable).

`steins-contract::spell` renders a summarized arm list back to a terminal-safe
PHPDoc type string (`int|numeric-string|null`, `'GET'|'POST'`). It spells the
literal and range arms a lowered PHPDoc envelope carries value-precisely —
int-range (`IntIn` → `int<1, 5>`, `positive-int`), int literal (`LitInt` → `5`),
and float literal (`LitFloat` → `1.5`) — extended for the contract-arm dump
surface (ADR-0052 §9); the docblock renderer is unchanged, since a summarized
value set never produces those buckets (its int members collapse to `Base(Int)`).
It is the one shared spelling, consumed by both the `annotate`/dump emitters and
the docblock renderer in `steins-edit` — the latter layering docblock-literal
armor (`*/` and raw-newline widening) on top before delegating.
