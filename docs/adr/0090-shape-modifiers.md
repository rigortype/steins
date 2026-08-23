# Shape modifiers: the presence axis, the seal axis, and the alias prerequisite

**Status: proposed (2026-08-23), PENDING ratification.** Drafted under the
owner's standing delegation, ahead of the slice it governs (#475). No lowering
ships with this ADR, and none should until §7's prerequisite — issue #472 — is
met.
[ADR-0089](0089-derived-type-operators.md) governs the naming rule, the
projection discipline and the `Opaque` floor this ADR inherits without
restating; read it first.

## 1. Context: the shape fact already has these axes

ADR-0062 defines the one canonical abstract array fact as two parts:

> fields (key, **presence**, value) + tail (**sealed**, or unsealed with
> key/value bounds) + denotational `isList` trinary + non-emptiness + key covers

The declaration lane holds the same two parts in `ContractTy::Shape`: `fields:
Vec<CField>` where `CField::optional` is the presence bit, and the pair
`sealed: bool` / `unsealed: Option<(Option<Box<ContractTy>>, Box<ContractTy>)>`
which is the tail.

Every one of those parts is **spellable inline** — `array{a?: int}` for
presence, `array{a: int, ...<string, int>}` for the tail — and **none of them is
addressable as a transform**. There is no way to say "this shape, with every
field optional" or "this shape, but open" without writing the whole shape out
again with the bit flipped.

TypeScript names the presence transform (`Partial` / `Required`) and the
field-set transform (`Pick` / `Omit`). It does not name the seal transform at
all, because excess-property checking and index signatures are separate
mechanisms there rather than one axis of one fact. The owner asked for the
seal pair anyway, and that request is the better-grounded half of this ADR:
the pair is not imported from TypeScript, it is what Steins' own shape fact
has been asking for since ADR-0062 gave the tail a name.

## 2. Decision: six modifiers, one operand rule

| Operator | Axis | Effect on `ContractTy::Shape` |
| --- | --- | --- |
| `partial-of<T>` | presence | every `CField::optional` becomes `true` |
| `required-of<T>` | presence | every `CField::optional` becomes `false` |
| `unsealed-of<T>` | seal | `sealed: false`, tail per arity (§2.1) |
| `sealed-of<T>` | seal | `sealed: true`, `unsealed: None` |
| `pick-of<T, K>` | field set | keep the fields whose key is in `K` |
| `omit-of<T, K>` | field set | drop the fields whose key is in `K` |

`non_empty` and `list` are carried through unchanged by all six. `isList` is not
carried through — §5.

**The operand rule, for all six: the operand must lower to
`ContractTy::Shape`.** Anything else — `MapOf`, `ListOf`, `ArrayAny`,
`IterableOf`, a scalar, a class, an unresolved `@template` — floors to
`ContractTy::Opaque` per ADR-0089 §4. This is one rule rather than six because
the modifiers all address *declared fields* or *a declared tail*, and the other
array forms have neither.

### 2.1 `unsealed-of` arity

The tail is optional in the grammar (`ArrayShape::unsealed`), so the operator
takes it the same way, mirroring `iterable`'s existing 1/2 arity split:

| Spelling | Tail | Equivalent inline shape |
| --- | --- | --- |
| `unsealed-of<T>` | untyped | `array{…, ...}` |
| `unsealed-of<T, V>` | value only | `array{…, ...<V>}` |
| `unsealed-of<T, K, V>` | key and value | `array{…, ...<K, V>}` |

This maps one-to-one onto `UnsealedType { value, key }`, so the operator
introduces no tail form the parser cannot already produce.

### 2.2 `pick-of` / `omit-of` take a key set, not a type

`K` is a literal key or a union of them — `pick-of<T, 'id'|'name'>`. It is
lowered like any other operand and then read as keys: `LitStr` and `LitInt`
members are keys, and **any other member floors the whole operator**. A
non-literal `K` is not a key set, and guessing at one would be the same
manufactured precision ADR-0089 §4 forbids.

`key-of<T>` is the natural companion and composes: `pick-of<T, key-of<T>>` is
`T`, which is the round-trip an implementation should test.

## 3. The soundness asymmetry, and why `sealed-of` is the one to get right

`unsealed-of` **widens**: the result admits everything the operand admitted,
plus arrays carrying extra keys. It can never turn a `Maybe` into a `No`.

The other five **narrow**. Narrowing is not itself a hazard here — a declaration
is an authoritative envelope the author asserts (ADR-0001), and
`required-of<array{a?: int}>` is exactly as much of an assertion as writing
`array{a: int}` by hand. The hazard is narrowing an operand whose declared
fields are not what the author was thinking of, and `sealed-of` is where that
goes wrong loudly:

```
sealed-of<array<string, int>>
```

A `MapOf` carries **no declared fields**. The only reading of "sealed" over it
is a shape with no fields and no tail — `array{}` — which admits the empty array
and nothing else. Every non-empty `array<string, int>` would get a confident
`No` from a type the author wrote to describe them. That is the exact wrong-`No`
hazard `KNOWN_UNENFORCED` exists to prevent and that `key-of` / `value-of`
already solve by flooring (ADR-0062), and it is a false positive rather than
lost precision because the closure-argument variance check raises findings on
`No` (ADR-0071 §1).

§2's operand rule is what forecloses it. The rule costs the other five nothing —
`partial-of<array<string, int>>` flooring to `Opaque` is merely useless, since
there is no presence bit to flip — but for `sealed-of` the same rule is
load-bearing, and an implementation that relaxes it for convenience reintroduces
the bug. **The rule is stated once, for all six, so that it cannot be relaxed
for five of them without noticing the sixth.**

### 3.1 `omit-of` and `pick-of` additionally require a *sealed* operand

Dropping a declared field from an **unsealed** shape does not remove the key:
`omit-of<array{a: int, ...<string>}, 'a'>` deletes the field and the tail
promptly re-admits `'a'` against `string`. The result is a wider type, so
nothing unsound happens — but the operator's meaning is "this key is not there",
and an unsealed tail contradicts it. Rather than ship an operator whose name
lies about half its operands, `pick-of` and `omit-of` floor on an unsealed
operand.

The composition is the remedy and reads correctly:
`omit-of<sealed-of<T>, 'password'>` says both things the author meant, in the
order they meant them.

## 4. Union distribution

ADR-0089 §4 applies unchanged: map over the arms, leave an arm the operator does
not apply to alone, and floor the whole type where the rule declines for one
arm.

For this family that resolves as: a non-array arm passes through
(`partial-of<array{a: int}|null>` is `array{a?: int}|null`, because `null` has
no fields and losing it would narrow), and an **array-flavored arm that is not a
`Shape`** floors the whole type. The second half is §3's rule reaching the union
case, and it is the half that matters: `sealed-of<array{a: int}|array<string,
int>>` must not return `array{a: int}|array{}`.

## 5. `isList` is recomputed, not carried

The `list` flag on `ContractTy::Shape` is a spelling; `isList` is a denotational
fact `ShapeFact::normalize` computes over the admitted value set, optional-key
combinatorics included (ADR-0062). Since the modifiers change exactly the inputs
that computation reads, the fact moves under them, and it should:

- `partial-of<list{int, string}>` makes both fields optional, so `array{1: 'x'}`
  — key 1 without key 0 — enters the admitted set and `isList` falls off `Yes`.
- `unsealed-of<list{int, string}>` with an untyped tail admits string keys, so
  `isList` falls off `Yes` likewise; with an int-keyed tail it need not.

Divergence registry entry 2 then decides the **rendering**: a sealed shape's
head keyword follows its own `isList`, so these results generally spell back as
`array{…}` rather than `list{…}`. That is the registered behavior, not a
regression, and it is the concrete payoff of ADR-0089 §3's projection
discipline — the modifiers get the recomputation for free by producing an
ordinary `Shape` and letting the existing normalizer read it, instead of
carrying an `isList` claim of their own.

**This is the round-trip an implementation must pin**, because it is the one
place a reader will expect the operator to preserve a spelling and it does not.

## 6. Open: a key the shape does not have

`omit-of<array{a: int}, 'typo'>` drops nothing and returns `array{a: int}`.
TypeScript's `Omit` is famously permissive here for the same reason — `K` is not
constrained to `keyof T` — and the permissive reading is what keeps the operator
usable over a shape that a later edit shrinks.

Whether Steins should *additionally* report it is a real question and is **not
decided here**. It is the same question ADR-0089 §5.1 leaves open for
`non-nullable<null>` yielding `never`: an operator whose arguments provably do
nothing, or provably empty the type, is a docblock defect that the analyzer can
see. Both belong at the contracts floor if either does, under one id, and they
should be answered together — a `phpdoc.*` id for "type operator states
nothing", scoped in its own slice with its own fp-gate evidence. Neither
operator's *lowering* waits on that answer.

## 7. Why this family waits: the operand has to be a name

Applied to an operand written inline, every modifier here is **longer than the
type it projects to**:

```
partial-of<array{a: int, b: string}>   →   array{a?: int, b?: string}
unsealed-of<array{a: int}>             →   array{a: int, ...}
```

That is not a small objection. It is the same objection that refuses `Record`
in ADR-0089 §6.1, turned on this family, and it is only answered when the
operand is a **name** — so that one shape is written once and the variants are
derived from it. There are two candidate names and neither is available today:

**A `@phpstan-type` alias — the one that actually unlocks the family, and it is
absent.** Steins does not resolve type aliases at all. Issue #195 put the tags
in the index as **silence obstacles** alongside `@method` / `@property` /
`@mixin` (ADR-0049 A14) and stopped there: the alias is parsed and shelved,
never expanded, so `partial-of<UserRow>` has nothing to read and floors. Issue
#472 is what would expand it, on `template-type`'s declared-side rewrite
precedent.

That the alias also *obstructs* is a separate defect (issue #471) and not this
family's business: A14 names only `@method` / `@property` / `@mixin`, and its
justification — members live where the index cannot enumerate them — does not
reach a tag that declares no member.

**A `@template T` bound at the call site — closer, but not free either.** Issue
#363 binds a declaration's own `@template` from an argument's carry, which is
the machinery a `@param partial-of<T> $patch` would want. But the binding is
read at exactly one place today — a `@return T` — and the binding rule names
its refusals narrowly and deliberately: top-level `@param` positions only, no
nested spellings, all-or-nothing per name. Evaluating an *operator* over a
call-site-bound `T` in a `@param` position is a new evaluation point, not a
free ride on #363, and widening #363 to reach it is a change to a rule ADR-0032
kept narrow on purpose.

**Decision on sequencing.** This family is designed here and **implemented
after issue #472**, not before. Aliases are the cheaper unlock, they are
declaration-side so they need no call-site plumbing, and they are the form the
value case is actually made of — a codebase that names `UserRow` once instead
of copying a twelve-field shape into every `POST`, `PATCH` and projection
signature. The ADR-0089 roster does not share this dependency and does not
wait on it.

## 8. Consequences

**Accepted.** Six spellings PHPStan reports as `class.notFound`, on the terms
ADR-0089 §7 registers for the whole family. A designed-not-built entry in
[not-implemented.md](../type-specification/not-implemented.md) for as long as
issue #195 is open. One round-trip surprise, pinned by §5's tests rather than
discovered.

**Bounded.** No denotation changes and no variant is added. The six modifiers
read a `Shape` and produce a `Shape`; `ShapeFact::normalize` and the speller do
the rest exactly as they do for a hand-written one.

**Declined.** `Readonly` is refused in ADR-0089 §6.2 rather than treated as a
third axis of this family: PHP arrays are value types, so a readonly shape
denotes the same set as the shape. The seal pair is the axis that request was
reaching for, and it is here.
