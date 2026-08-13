# The abstract union layer: a Fact may span bases

**Status:** proposed (2026-08-14), pending ratification. Amends ADR-0035's
four-layer value domain.

## 1. The wall

The finite layers have always carried a mixed-base union *exactly* —
`$c ? 1 : 'x'` is `Fact::OneOf([Int(1), Str("x")])`, whatever the bases. The
abstract layers could not: `join_abstract` returned `None` the moment the bases
differed, and `summarize` returned `None` for a mixed-base overflow. So a value
that started as `1|'x'` and widened past `CAP` became **nothing** rather than
`int|string`, and every rule whose answer spanned two bases declined for want of
a form to say it in — `int|false` returns, `json_decode`'s envelope,
`in_array`/`array_search`'s `int|string|false`, `min`/`max` (ADR-0062
Amendment B's recorded deviation), `switch`/`match` on loose comparison, the
declared-return floor's multi-base rows, and the everyday `$c ? $i : $s`.

Multi-base was not *unsupported* — the **arm lane** carried it, which is why
`int|string $x` has always dumped as `int|string` and narrowed correctly under
`is_int`. The split was deliberate. What this ADR decides is that the *value*
lane should carry it too, because the arm lane cannot reach inside a domain type
(`steins-domain` depends on no other steins crate, so `ShapeFact`'s slots cannot
hold a `ContractTy`) and because every consumer above was paying the same tax in
silence.

## 2. The form

```rust
Fact::Union { arms: Vec<(Base, Option<Refinement>)>, nullable: bool }
```

One arm per `Base`, sorted, **2..=4** of them. PHP has four scalar bases, so this
is a small map rather than an open lattice — which is the point: `join` over the
abstract layers becomes **total** instead of partial. Built only through the
normalising `Fact::union`.

**The array stratum is not an arm.** `ShapeFact` is recursive, and joining it in
would make the scalar layer mutually recursive with the shape algebra for one
spelling. A union with an array in it declines, as it did before.

## 3. Two invariants, and what each one cost to learn

Both were found by the differential vector universe, which is the apparatus
ADR-0059 exists to fund. Neither would have been found by reading the code.

### 3.1 An arm's refinement obeys `Fact::refined`'s own rule
 A contentless
refinement (an empty `StrPreds`, a full `IntRange`) *is* that base's General, so
storing it as `Some` is a second spelling of one fact — and with two spellings
**join is not associative**: `Singleton(1) ⊔ (Singleton('a') ⊔ numeric-string)`
reaches the string arm as `None` while the other bracketing reaches it as
`Some(<empty>)`. 35698 counterexamples over the vector universe.

### 3.2 The mixed-base descent refuses a member with no scalar base
 `Val::base`
is `None` for an array, and the first implementation filtered members per scalar
base — silently dropping the array and building a fact that did **not admit a
value the set contained**. That is a soundness defect, the one thing the
computed widening must never do. A set mixing arrays with scalars drops the fact
whole, exactly as before.

## 4. Consequences, measured

- `join_abstract` concatenates arms and merges per base: total over the abstract
  layers. `join` keeps its `Option` only for the finite/array cases.
- `summarize` descends a mixed-base overflow into a union.
- **The contract lowering follows for free.** `to_fact` builds facts by joining
  member facts, so a declared `int|string` now lowers into the *value* lane as
  well as the arm lane. Both carriers now describe it; ADR-0062 §5's
  one-relation discipline is what keeps them honest, and the acceptance path
  reads the union as a for-all over its arms. The curated-row lowering
  (`contractty_to_fact`) folds any scalar union the same way.
- **The reflected ENVELOPE is deliberately not generalised**, and this is the
  one place the union was tried and backed out. The reflected declaration is
  the *engine's*, which is coarse by construction — `abs` declares `int|float`
  — while ADR-0069's curated floor carries the sharp row for the same name
  (`int<1, max>|0|float`). The envelope rung sits **above** the floor, so an
  envelope that answers in more cases *shadows* the sharper row: 13 nsrt rows
  went from `int<0, max>|float` to `int|float` on exactly that path. A wider
  envelope is not wrong and is not an improvement either, and buying it at the
  cost of the curated rows is a bad trade. Widening it waits on the ladder
  question — whether the floor may refine *within* a union envelope the way
  ADR-0061 §2 has the type rung refine within a scalar one — which is its own
  decision.
- The ternary joins **facts** rather than values when an arm proves none, so
  `$c ? $i : $s` is `int|string` where it was `unknown`.
- `Fact::array_key_cast` answers the rows it had to decline (ADR-0062
  Amendment G): `string` → `int|non-decimal-int-string`, `numeric-string` →
  `int|numeric-string&non-decimal-int-string`.
- Ten sites in the walk that cannot use a union decline explicitly, each with
  its reason — a union names no single type word, no single runtime kind, and
  no single operand kind.

nsrt headline 2444 → **2450**, admissible 2872 → **2878**, transition
one-directional (4 `differ → match`, zero the other way). fp-gate GREEN with **no
baseline movement**; proof layer 0 across 6670 files. Lattice laws hold over the
whole vector universe: associativity, commutativity, and never-loses-a-member.

## 5. Not in this decision

- **The key slot.** `Tail::Unsealed`'s key is a `KeyClass` with three values, so
  `array<int|non-decimal-int-string, T>` still cannot be spelled. `KeyClass`
  appears at 145 sites across 7 crates; it is its own decision (issue #336).
- **A finite member beside an abstract arm.** PHPStan spells `$c ? $i : 'x'` as
  `'x'|int`; this domain summarizes the finite side first and answers
  `int|non-falsy-lowercase-string`. Sound, less sharp, and unchanged by this ADR.
- **The victims list of §1**, each of which becomes its own small slice now that
  the form exists.
