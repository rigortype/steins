# Spike report: a Lean 4 specification for `steins-domain`

**Verdict: keep it, scoped to the value domain.** The soundness contract
ADR-0035 states in a doc comment and `tests/lattice.rs` samples with five
property tests is now a set of closed theorems over every value; the
differential harness that binds spec to implementation caught two real
divergences on its first two runs; and the whole thing is small enough to check
from `cargo xtask lean-check`. Decision recorded as ADR-0059.

**Do not extend it to `steins-infer`.** See "Where this stops" below.

- Toolchain: Lean 4.30.0, `nix develop` (see `flake.nix`). Lean core only — no
  Mathlib, so `lake build` is offline and takes a few seconds.
- Size: ~1,500 lines of Lean for the 1,089 lines of `crates/steins-domain`.
- Axioms: `propext`, `Classical.choice`, `Quot.sound` — Lean's own three. No
  `sorry`, no `native_decide`, no bespoke axiom. Verify with
  `#print axioms SteinsDomain.Fact.join_sound`.

## What is proved

`SteinsDomain.Soundness` carries the headline:

```lean
theorem join_sound (M : Model) (a b : Fact) (v : Val)
    (h : a.admits M v = true ∨ b.admits M v = true) : denotes M (join M a b) v
```

`denotes` reads `none` as ⊤, which is what the consuming code does — `join_envs`
drops the binding when a join is unrepresentable, i.e. stops claiming anything.

Everything else is in the ADR-0059 table. The two structural lemmas worth calling
out separately, because they are invariants the Rust code *relies on* without
stating:

- `Val.ssorted_ext` — a strictly-increasing list is determined by its member set.
  This is why `OneOf` has one representation per value set, and it is what makes
  `join_comm` true on the finite layers rather than merely plausible.
- `Certainty.allOf_eq_yes_iff` / `_no_iff` — `all_of` is exactly the universal
  quantifier over a non-empty collection. The empty-list `Maybe` is now a named
  theorem (`allOf_nil`) rather than a comment, which is the right status for the
  one place a vacuous `yes` could manufacture a proof-layer finding out of no
  evidence.

## What the harness found

Two divergences, both on the first two runs of
`cargo test -p steins-domain --test lean_vectors`. Neither is a bug in shipped
code — both are in what the *spec assumed about the implementation*, which is
precisely the class of thing a proof rests on silently and the reason the `atom`
and `order` lines exist.

1. **`is_numeric("00")` is true.** The spec's classifier table said `"00"` was
   `NE|NF` (non-empty, non-falsy, not numeric). Settled by asking the engine:
   PHP allows leading zeros, so `"00"` is `NE|NF|NUM`. The Rust
   `php_is_numeric` was right; the spec's assumption was wrong.

2. **String atom ranks must be `str::cmp` order.** The spec models a string value
   as its position in the total order, so an arbitrary labelling silently breaks
   the abstraction's faithfulness. The correct order is
   `"" < " 5 " < "0" < "00" < "5" < "abc"`. Caught by the `order` line, which
   exists for exactly this.

Three further observations, none a defect:

3. **Closedness is a precision property, not a soundness one.** No soundness
   proof uses `Model.predsOf_closed`; `admits` and `join` stay sound on unclosed
   predicate sets. ADR-0035 calls the representation "canonical by construction",
   and it *almost* is — the bare constants `StrPreds::NON_FALSY` and
   `StrPreds::NUMERIC` are unclosed values of the type, so two syntactically
   different `Refined` facts can denote the same set.

4. **One of the eight predicate sets is unreachable.** `{NonFalsy, Numeric}`
   without `NonEmpty` cannot be built through `StrPreds`: `union` closes and
   `intersect` cannot add bits. The vector universe carries the seven reachable
   sets and says so. (This is a stronger statement than the type admits, and a
   mildly encouraging one: the closure invariant is enforced by construction
   everywhere except the single-bit constants.)

5. **Three `unreachable!`/`expect` sites are modelled as widening**
   (`summarize`'s empty int/string folds, `abstract_falsy_truthy` on a finite
   layer, `joinFiniteAbstract`'s non-abstract operand). Widening is the sound
   side, so the theorems hold for the panic-free reading — but if any of those
   became reachable, Rust panics where the spec silently widens.

## What is not proved: associativity

`join` associativity is **checked, not proved**: 110,592 triples over the
48-fact vector universe, computed independently in Lean and in Rust, zero
mismatches (the `assoc` line in the fixture, plus
`join_is_associative_over_the_vector_universe`).

Why it resisted: the CAP boundary. In `(a ⊔ b) ⊔ c` the first join may overflow
`CAP` and summarise a *subset* before the second join runs, while
`a ⊔ (b ⊔ c)` may keep `b ∪ c` finite and summarise the *whole* union. Soundness
needs only that `summarize` widens monotonically — which is proved — but
*equality* needs the hull/intersection fold to commute with the layering
decision, across the mixed-base `none` case and the all-null special case in
`joinFiniteAbstract`. Every triple traced by hand agrees, and the exhaustive
check agrees; the proof is a real but unattempted case analysis.

Why it matters rather than being a curiosity: `join_envs`
(`crates/steins-infer/src/lib.rs`) folds multi-branch joins left-to-right, so
non-associativity would make a diagnostic depend on the order the arms happen to
appear in the source.

**This is the first thing to do if the spike is continued.**

## Where this stops

- **`steins-infer` is not a target.** 13,290 lines entangled with PHP's real
  behaviour, Mago's tree, and salsa. More to the point, the live FP source is not
  the algebra: the G1 finding in
  `docs/notes/20260724-adr0049-0052-soundness-audit.md` is four consumers reading
  a lower bound as an exact class. Lean can show the algebra is right; it cannot
  show a right algebra is used rightly.
- **`steins-phpdoc` is not a target.** Its specification is
  phpstan/phpdoc-parser's behaviour, which has no mathematical definition. The
  differential oracle is the correct instrument and already exists.
- **`subtract` / narrowing (ADR-0052) is the next real candidate** if this
  continues. Its rules are order-dependent ("the positive branch deletes the
  final non-member only"), its verification today is twenty example tests, and
  the statement wanted is a containment: `γ(subtract(arms, sub)) ⊇ γ(arms) \
  γ(sub)` — never delete an arm that might still be inhabited. Structurally this
  is the same hazard as the short-circuit rule the macro_peg write-up found a
  counterexample to.

## Running it

```
cd spike/lean-domain
nix develop --command lake build        # the proofs
nix develop --command lake exe vectors  # the vector file, on stdout
```

From the repo root:

```
cargo xtask lean-check                            # proofs compile + fixture is current
cargo xtask lean-check --bless                    # rewrite the fixture after a spec change
cargo test -p steins-domain --test lean_vectors    # Rust vs. the fixture (needs no Lean)
```

## Module map

| File | Contents |
| --- | --- |
| `SteinsDomain/Certainty.lean` | the trinary judgment; Kleene laws; `allOf` as the quantifier |
| `SteinsDomain/Preds.lean` | `StrPreds` as three named `Bool`s; `containsAll` a partial order, `inter` its meet; the Horn closure claim |
| `SteinsDomain/Range.lean` | `IntRange`; hull/intersection laws by `omega` |
| `SteinsDomain/Val.lean` | values, the `(rank, tie)` total order and its three laws, and `Model` — the classifier parameters with their two coherence laws |
| `SteinsDomain/Canon.lean` | the sorted-deduped finite layer and its canonicity |
| `SteinsDomain/Fact.lean` | the four layers, `admits`, `summarize`, `fromVals`, `join`, the trinary queries |
| `SteinsDomain/Soundness.lean` | `join_sound` and the widening steps it composes |
| `SteinsDomain/Queries.lean` | decided verdicts hold for every admitted value |
| `SteinsDomain/Vectors.lean` | the differential vector file |
