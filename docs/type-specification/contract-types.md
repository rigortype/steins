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
- `numeric-string`, `non-empty-string`, `non-falsy-string` → `StrWith`.
- `class-string`, `literal-string`, `lowercase-string`, … → `StrOpaque`.
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

**Provenance-flavored string types can never answer `Yes`.** `class-string` and
kin lower to `StrOpaque`: a non-string is `No`, a string is `Maybe`. Membership
in these types is a fact about where a value *came from*, not about the value,
and Steins does not do taint tracking (ADR-0038). It reserves value-provenance
labels as the general mechanism, unimplemented.

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
PHPDoc type string (`int|numeric-string|null`, `'GET'|'POST'`). It is the one
shared spelling, consumed by both the `annotate`/dump emitters and the docblock
renderer in `steins-edit` — the latter layering docblock-literal armor
(`*/` and raw-newline widening) on top before delegating.
