# The Value Domain

**Status: implemented** (`steins-domain`; ADR-0035).

The value domain is what Steins knows about **one value**. It is a four-layer
representation, not a type lattice: the layers name the *shape of the
knowledge*, and widening is a descent between them.

```text
1. Singleton   — exactly one concrete value          (the maximal sieve)
2. OneOf       — a finite value set, 2..=8 members
3. Refined     — base type + refinement (predicate bitset / int interval)
4. General     — the bare base type
```

The implementing type is `Fact`. A value that is not representable in any layer
carries **no fact at all** — γ = everything — which is always the safe side.

## Inhabitants

`Val` is a concrete PHP value: `Int(i64)`, `Float(f64)`, `Str`, `Bool`, `Null`,
and `Array(Vec<(Key, Val)>)` — a fully-known array with PHP-normalized keys in
insertion order. `Key` is `Int(i64)` or `Str(String)`; the trace IR performs PHP
key normalization (numeric strings to ints, `true`→`1`, …) and the domain stores
only the result.

`Base` — the carrier of the abstract layers — is `Int | Float | String | Bool`.
**`Null` is deliberately not a base**: nullability is a *flag* on the abstract
layers (`nullable: bool`), while the null value itself lives in the finite
layers as `Val::Null`.

Equality and ordering on `Val` are **representational, not PHP semantics**:
floats compare by `f64::total_cmp` (so `NAN == NAN` here, and `-0.0 != 0.0`).
The domain needs set semantics; PHP-level `==`/`===` live in the condition
evaluator, not on this type. See [narrowing.md](narrowing.md).

## Refinements

Exactly two refinement kinds exist, one per refinable base.

**Strings — a closed predicate bitset** (`StrPreds`):

| Predicate | Meaning | PHPStan spelling |
| --- | --- | --- |
| `NON_EMPTY` | not `""` | `non-empty-string` |
| `NON_FALSY` | neither `""` nor `"0"` | `non-falsy-string` |
| `NUMERIC` | `is_numeric()` holds | `numeric-string` |

The set is closed under implication **at construction** (`NON_FALSY ⇒
NON_EMPTY`, `NUMERIC ⇒ NON_EMPTY`), so a subset test never misses an entailed
fact. The set is deliberately *closed* as a design choice: adding a predicate is
one constant plus its evaluator, and every interaction stays exhaustively
checkable. Union closes; intersection preserves closure (the implications are
Horn clauses over positive literals).

**Integers — an inclusive interval** (`IntRange`). `positive-int`,
`non-negative-int`, `negative-int`, and PHPDoc `int<lo, hi>` are all spellings of
one interval over PHP's 64-bit ints, with `i64::MIN`/`i64::MAX` as the domain
bounds. Interval algebra (hull, intersection) is total and canonical, so no
normalization pass exists — no non-canonical form can be constructed.
`IntRange::new` returns `None` for `lo > hi`: the domain has no empty interval,
and callers read that as a contradiction.

**Not refined:** floats and bools carry no refinement kind. A float or bool fact
is `Singleton`, `OneOf`, or `General`.

## Canonical forms

Enforced by the constructors, checked by property tests:

- `OneOf` is sorted, deduped, and holds `2..=CAP` members, where `CAP = 8`. One
  member is a `Singleton`; zero is no fact.
- A `Refined` always carries real knowledge — a non-empty predicate set, or a
  non-full interval. A contentless refinement collapses to `General` in the
  constructor, so `Refined` and `General` never denote the same set.

Because canonicity is a constructor invariant rather than a normalization pass,
two facts denoting the same set are `==`. Structural equality is therefore a
usable dedup key, and the branch walk relies on that
(see [narrowing.md](narrowing.md)).

## Join, and computed widening

`join(a, b)` is the least representable fact admitting both denotations.

The soundness contract, property-tested:

```text
γ(a) ∪ γ(b) ⊆ γ(join(a, b))
```

A join may lose precision (widen); it may never lose members. `join` returning
`None` means "not representable" (mixed scalar bases, say) — the caller drops
the fact, which is safe because no fact means γ = everything.

Widening out of the finite layers is **computed, not guessed**. When a value set
overflows `CAP`, the summary it widens to is derived by *evaluating the
predicates on every member*:

```php
// nine distinct non-empty numeric strings overflow OneOf(CAP = 8)
//   → Refined { base: String, refinement: Str(NUMERIC|NON_EMPTY) }
```

Precision loss is therefore measured. This is the ADR-0035 discipline that
separates the domain from an accessory-type representation: the layer descent
carries a real summary rather than dropping to the bare base.

## Queries

Membership is **extensional**: `admits(&Val)` answers whether a concrete value
is in the fact's denotation, by direct evaluation — binary search in `OneOf`,
predicate/interval check in `Refined`, base check in `General`.

The trinary queries (`truthy`, `is_null`, …) return
[`Certainty`](certainty.md). PHP truthiness is applied exactly: `""` and `"0"`
are the only falsy strings (`"0.0"` and `"00"` are truthy), `0`, `0.0`, `[]`,
`null`, and `false` are falsy. On a finite layer the query is decided
member-wise and joined; on an abstract layer it is decided from whether the
denotation can be falsy, truthy, or both — with both giving `Maybe`.

## What the domain does *not* do

- **No union of unlike bases.** `int|string` is not a `Fact`; it is either an
  arm list on the contract side ([contract-types.md](contract-types.md)) or a
  dropped fact. The join returns `None` and the caller widens.
- **No object values.** Objects live in the heap store, keyed by allocation
  identity ([object-model.md](object-model.md)). A variable holding an object
  has no `Fact`.
- **No closure values.** A closure rides in its own binding slot (ADR-0033;
  [closures.md](closures.md)).
- **No partial arrays.** `Val::Array` is a *fully known* array. An array with
  one unknown element is not representable, so the fact is dropped. This is why
  the offset family only fires on fully-proven containers.
- **No generic type arguments.** Carrying `T` through a collection is ADR-0032
  stage 1 — **designed, not implemented**. See
  [not-implemented.md](not-implemented.md).
