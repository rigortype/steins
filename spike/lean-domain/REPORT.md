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
- Size: ~2,300 lines of Lean for the ~1,700 lines of `crates/steins-domain`.
- Axioms: `propext`, `Classical.choice`, `Quot.sound` — Lean's own three. No
  `sorry`, no `native_decide`, no bespoke axiom. This is not a promise but a build
  step: `SteinsDomain/Axioms.lean` pins each headline theorem's axiom set with
  `#guard_msgs`, so a `sorry` left in during a refactor, or a `native_decide`
  (which would add `Lean.ofReduceBool` and move the trust boundary from the
  kernel to the compiler), fails `lake build`.

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

## The array stratum: what is proved and what is checked (ADR-0062 S2)

`Fact` gained a fifth constructor, `shape`, carrying the canonical array fact.
Three things follow, and it is worth being precise about which is which.

**Proved, for every input.** `admits` is the *full* recursive mirror of Rust:
a shape's value slots are structural subterms of the fact, so the definition
recurses with no modelling shortcut, and the theorems that read a decided
verdict (`truthy_yes`, `truthy_no`, `satisfiesStr_yes`, `intIn_no`) now cover
the array stratum too. `Model` gained `arrEntries` with the coherence law
`arrFalsy_iff` — an array is falsy exactly when empty — which is what
`truthy_yes` leans on for an array value, the same way it leans on
`nonFalsy_iff` for a string. `normalize`'s invariants are theorems:
`normalize_fieldsSorted` (fields strictly increasing on the key, hence one
entry per key), `normalize_sealed_no_absent`, `normalize_covers_have_two_keys`.

**Unchanged.** `join_sound`, `join_comm`, `summarize_admits` and
`fromVals_admits` are the *same statements about the same definitions*: the
scalar core is now spelled `joinScalar`/`summarizeScalar`/`fromValsScalar` and
its bodies are untouched, so nothing proved before is weaker now.
`join_eq_joinScalar` records that off the array stratum the shipped join *is*
the proved one.

**Checked, not proved.** The array stratum's own join, lift and computed
descent are exercised exhaustively over the shape vector universe and tallied
in the vector file, on both sides, exactly as associativity is:

```
shapejoinsound 5776 0        γ(a) ∪ γ(b) ⊆ γ(a ⊔ b), shape-level
shapeliftsound 16 0          the lifted shape admits the value it lifted
shapedescentsound 28 0       the descent admits every member it summarized
shapefactjoinsound 2592 0    the same at the `Fact` level, `none` read as ⊤
```

Why it resisted proof rather than being skipped: the join's soundness rests on
the *denotational* correctness of `computeIsList` in both directions — `yes`
must imply every admissible array is a list, and `no` must imply none is, the
second by way of a counting argument over the declared keys — and on
field-wise reasoning through `sortFields` and the sealed-`absent` filter. That
is a real development, not a missing tactic call, and it is the first thing to
do if this spike is continued after associativity.

**Two deliberate modelling widenings**, both forced by the same thing — the
spec models an array as an opaque rank whose entries come from
`Model.arrEntries`, so a value reached *through* an array has no structure for
a termination measure to descend on:

1. `joinSlot` joins value slots through the scalar core, so a slot that is
   itself a shape fact widens to `unknown`, where Rust joins it recursively.
2. `shapeDescent` builds slot facts with `fromValsScalar`, where Rust calls the
   full `from_vals`.

Both make the spec *weaker* (an `unknown` slot admits everything), so no
theorem is put at risk, and neither is reachable from the vector universe —
which is why Rust and Lean agree on all 5,569 data lines.

## The narrowing operators (ADR-0062 S4)

Four targeted refinement operators (A-G7 — never a general ⊓) mirror
`ShapeFact::promote_present` / `mark_absent` / `set_non_empty` /
`set_is_list` as `Fact.shapePromotePresent` / `shapeMarkAbsent` /
`shapeSetNonEmpty` / `shapeSetIsList`, plus `Fact.stripNullFact` (the
`isset` flavor's null strip) and `GShape.countRange` — the last discharging
S3's Lean debt.

**Proved, for every input.** Invariant preservation, which is what reusing
`normalize` buys: `shapePromotePresent_fieldsSorted`,
`shapeMarkAbsent_fieldsSorted`, `shapeSetNonEmpty_fieldsSorted`,
`shapeSetIsList_fieldsSorted` (each stated with the receiver's own invariant as
the hypothesis, which is what discharges the two passthrough branches),
`shapeMarkAbsent_covers_have_two_keys`, `shapeSetIsList_covers_have_two_keys`,
`shapeSetNonEmpty_nonEmpty`, and `IntRange.new_getD_valid` /
`GShape.countRange_valid` — the count interval is always well-formed, which is
what makes Rust's `unwrap_or(NON_NEGATIVE)` a totality fallback rather than a
reachable branch.

**Checked, not proved.** The narrowing law itself, exhaustively over the shape ×
array vector universe, on both sides:

```
shapenarrowsound 559 0    γ(op(s)) ⊇ { v ∈ γ(s) : v satisfies the guard }
shapeunsetsound  320 0    γ(mark_absent(s, k)) ∋ v \ {k} for every v ∈ γ(s)
shapecountsound   80 0    count_range(s) bounds |v| for every v ∈ γ(s)
```

Why it is checked rather than proved: exactly the obstacle the S2 section names.
Every operator routes through `normalize`, so its result's `is_list` is
`computeIsList` applied to a *changed* field list, and the law needs
`computeIsList` to be denotationally correct in both directions before any of
the four can be discharged. That development is still the first thing to do.
`shapeunsetsound` is the reason `mark_absent` is the one operator that does
**not** carry the receiver's `is_list` and `non_empty` across: under `unset` the
result must admit `v \ {k}`, which the receiver itself need not admit —
`array{a: string}` computes `no`, and removing `a` leaves `[]`, which is a list.
The other three return a denotational *subset* of the receiver, so carrying the
flags is sound there. The Rust unit tests
`narrowing_operators_admit_every_guard_satisfying_member` and
`mark_absent_admits_every_receiver_member_minus_the_key` pin the same two laws
in-crate over their own universe.

## The cover algebra (ADR-0062 S5)

`GShape.recordCover` and `GShape.coverProves` mirror `ShapeFact::record_cover`
and `ShapeFact::cover_proves` — A-G8's recording and A-G11's discharge query.

**Proved, for every input.** The recording constructor establishes nothing of
its own: it routes through `normalize`, so the invariants that adding a cover
could break are the S2 theorems instantiated — `recordCover_fieldsSorted`,
`recordCover_covers_have_two_keys` (with `recordCover_singleton_stores_no_cover`
naming the case A-G8 singles out: a singleton is presence, never a cover) and
`recordCover_sealed_no_absent`. That is the whole design argument for a
normalizing constructor, stated as theorems rather than asserted in a comment.
`coverProves_mem` is the one claim genuinely about the query: it only ever
answers with a cover the shape actually carries, so a discharge cannot invent
the claim it discharges.

**Checked, not proved.** The two laws the `??` right-arm rule rests on,
exhaustively over the shape × array vector universe, on both sides:

```
shapecoversound     488 0    γ(record_cover(s, K, f)) ⊇ { v ∈ γ(s) : v satisfies K@f }
shapedischargesound 218 0    cover_proves(k, [j]) ⇒ k present in every admitted v where j fell through
```

Why checked rather than proved: the same obstacle the S2 and S4 sections name —
`recordCover` routes through `normalize`, so its result's `is_list` is
`computeIsList` over a field list the cover promotion may have changed, and the
containment needs `computeIsList` denotationally correct in both directions
first. The discharge law additionally needs the antichain filter to preserve the
disjunctive claim, which is a statement about `subsumes` being a genuine entailment
rather than a sorting key. Both are real developments, not missing tactic calls.
The Rust unit tests `record_cover_admits_every_member_satisfying_the_disjunction`
and `cover_proves_only_when_the_key_is_really_present` pin the same two laws
in-crate over their own universe.

**One deliberate asymmetry, and it is the point.** `coverProves` returns the
*claim*, not a verdict. An `isset` cover discharges unconditionally; a
`keyExists` cover discharges only when the caller has established that every
refuted member's value slot is non-nullable, because a present-**null** member
satisfies the cover while `??` still falls through it. The domain deliberately
does not know about `??`, so that second condition lives in `steins-infer`
(`coalesce_arm_fact`) and the vector law above states only what the domain
promises.

**One representational note.** `Fact` is a *nested* inductive (its `shape`
payload contains `List (Key × Presence × Option Fact)`), which keeps the Lean
type faithful to Rust's `Vec<(Key, Presence, Option<Box<Fact>>)>` and lets the
ordinary `List` API apply. Lean 4 has no `deriving DecidableEq` handler for
nested inductives, so `Fact.beq` is written out and `BEq Fact` is built from
it; nothing in the spec needs propositional decidability of fact equality. This
is also why `Fact` and `Refinement` are declared in `Shape.lean` rather than
`Fact.lean`: `Fact` and the shape form are one recursive declaration.

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

In CI, `.github/workflows/lean.yml` runs the first two legs behind a path filter
(`leanprover/lean-action`, elan rather than Nix — a nixpkgs cache miss on `lean4`
would build Lean from source). The third leg needs no Lean and runs in the
ordinary `test` job on every PR.

## Module map

| File | Contents |
| --- | --- |
| `SteinsDomain/Certainty.lean` | the trinary judgment; Kleene laws; `allOf` as the quantifier |
| `SteinsDomain/Preds.lean` | `StrPreds` as three named `Bool`s; `containsAll` a partial order, `inter` its meet; the Horn closure claim |
| `SteinsDomain/Range.lean` | `IntRange`; hull/intersection laws by `omega` |
| `SteinsDomain/Val.lean` | values, the `(rank, tie)` total order and its three laws, and `Model` — the classifier parameters with their two coherence laws |
| `SteinsDomain/Canon.lean` | the sorted-deduped finite layer and its canonicity |
| `SteinsDomain/Shape.lean` | the array stratum (ADR-0062): keys, presence, covers, the `Fact` inductive, `normalize` and its invariants, the S5 cover algebra (`recordCover` / `coverProves`) |
| `SteinsDomain/Fact.lean` | the four layers, `admits`, `summarize`, `fromVals`, `join`, the trinary queries, the array stratum's algebra (`lift`, `shapeJoin`, `shapeDescent`), and the S4 narrowing operators |
| `SteinsDomain/Soundness.lean` | `join_sound` and the widening steps it composes |
| `SteinsDomain/Queries.lean` | decided verdicts hold for every admitted value |
| `SteinsDomain/Vectors.lean` | the differential vector file |
| `SteinsDomain/Axioms.lean` | the axiom ratchet — no `sorry`, no `native_decide`, enforced |
