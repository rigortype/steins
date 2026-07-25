# Lean 4 as the value domain's specification: proofs over the algebra, differential vectors over the implementation

The zero-false-positive bar (ADR-0002) is not a slogan about care; it is a
containment claim about one function. Every proof-layer finding rests on
`γ(fact)` — the set of runtime values a fact denotes — actually containing every
value the program can produce. ADR-0035 states the claim in a doc comment
(`γ(a) ∪ γ(b) ⊆ γ(join(a, b))`) and `crates/steins-domain/tests/lattice.rs`
samples it with five property tests.

Property tests search for counterexamples. They cannot establish containment,
and the gap between "no counterexample in 256 cases" and "holds for every `i64`,
every string, every predicate set" is exactly the gap the product's central
promise sits in. `steins-domain` is the one place in the tree where closing that
gap is cheap: 1,089 lines, no dependencies, every function pure and total, and no
contact with PHP's runtime, Mago's tree, or the salsa graph.

**Decision: the value domain gets a machine-checked specification in Lean 4,
living beside the Rust implementation rather than generating it, with a
differential vector file binding the two.** The spec is `spike/lean-domain`
(a non-member of the cargo workspace, like `spike/mago-spike`).

## What is proved

Lean core only — no Mathlib, so `lake build` stays offline and fast enough to run
from `cargo xtask lean-check`. Every theorem below is closed; the only axioms
used are Lean's own `propext`, `Classical.choice`, and `Quot.sound`.

| Theorem | What it generalises |
| --- | --- |
| `Fact.join_sound` | `join_never_loses_members` — `γ(a) ∪ γ(b) ⊆ γ(a ⊔ b)` for **every** value, with `none` read as ⊤ |
| `Fact.fromVals_admits` | `from_vals_admits_every_input` |
| `Fact.summarize_admits` | the *computed* widening: the summary an overflowing set descends to admits every member |
| `Fact.join_comm` | `join_is_commutative` |
| `Fact.truthy_yes` / `truthy_no` | `queries_agree_with_witnesses`, for all admitted values rather than the generated witnesses |
| `Fact.satisfiesStr_yes`, `Fact.intIn_no` | ditto, for the string-predicate and interval queries |
| `Val.canon_eq_self`, `Val.ssorted_ext` | the `OneOf` invariant comment: a strictly-increasing list is *determined* by its member set, so the finite layer has one representation |
| `Certainty.allOf_eq_yes_iff` / `_no_iff` | `all_of` **is** the universal quantifier — including the deliberate `maybe` on the empty list, where a vacuous `yes` would manufacture a finding from no evidence |
| `StrPreds.inter_closed` | the Horn-clause claim the Rust `intersect` doc comment makes |
| `IntRange.hull_*`, `inter_*` | `range_hull_and_intersection_laws` |

## Why the spec sits beside the implementation, not above it

The obvious alternative is **extraction**: write the domain in Lean, prove it,
and mechanically translate to Rust — the shape the [macro_peg/Lean 4
write-up](https://zenn.dev/nextbeat/articles/2026-07-macro-peg-lean4-proof) uses
to reach Scala 3. It is rejected here, and the reason is not distaste for
generated code:

- The Rust the domain must be is not the Rust an extractor emits. `Fact` is
  matched exhaustively on a hot path reached from `steins-infer`'s 13,290 lines
  once per trace node; `StrPreds` is a `Copy` bitset with `const fn` operations;
  every type carries `Hash`/`Eq` for salsa. An extractor either learns all of
  that or the generated code is a performance and API regression.
- The extractor becomes a maintained artifact with no other user. Scala's JVM,
  GC, and higher-order idiom make that translation cheap; Rust's ownership and
  representation control make it the expensive half of the project.
- ADR-0003's lesson, applied in reverse: own the artifact that has to be good,
  keep the other side behind a contract. There the contract is the syntax tree
  and Mago is the backend; here the implementation is the artifact and the spec
  is what it answers to.

The other alternative is a **Rust-level verifier** — Kani (bounded model
checking), Creusot, or Verus — which checks the shipped code directly and so has
no correspondence gap at all. That is genuinely the better tool for the question
"is this Rust function correct", and the domain's closed enums, three-bit bitset,
and integer intervals suit it. It is not the tool for the question this ADR
answers, which is "are ADR-0035's *design* claims true": the layering, the
descent, the algebra. Those are statements about a model, and a model is what
Lean is for. **This decision does not close the Kani door**; the two instruments
answer different questions and can coexist.

## The abstraction, and where its cost goes

The spec carries ints and bools concretely. Strings, floats, and arrays are
**ordered atoms** — a `Nat` giving the value's position in the total order —
because comparison and (for strings) the predicate summary are the only two
things the algebra ever does with them.

This is a factoring, not a shortcut, and it is the most useful thing the exercise
produced: **the domain's soundness is independent of PHP's string classifier.**
`join_sound` holds for *any* `predsOf`. IEEE-754 never enters — `total_cmp`,
`NaN`, and `-0.0` affect only the order, which the atom ranks abstract. What the
proofs do assume is two coherence laws, bundled in `SteinsDomain.Model`:

- `predsOf_closed` — `StrPreds::of` returns implication-closed sets. **No
  soundness proof uses it.** Closure is a canonicity/precision property.
- `nonFalsy_iff` — `NON_FALSY` is set exactly when the string is truthy. This one
  is load-bearing: `truthy` on the Refined layer reads its verdict off the
  bitset, so `truthy_yes` is only as good as this coupling.

Both are *classifier* obligations, discharged by execution, not by proof — which
is the ADR-0004 posture applied to a unit semantics. They are checked on concrete
strings by the vector file's `atom` lines, alongside `tests/php_oracle.rs`.

## The differential loop

Three legs, the same shape as the phpdoc oracle (ADR-0029): the spec is the
reference, the Rust is the subject, and disagreement is the signal.

1. **`lake build`** — the proofs compile. A spec that does not build proves
   nothing, so `lean-check` runs this first.
2. **`lake exe vectors`** — the spec prints a deterministic vector file, committed
   as `crates/steins-domain/tests/fixtures/lean-vectors.expected`
   (4,154 lines): the atom tables, the universe in ascending order, and
   `admits` / `truthy` / `isnull` / `satisfiesstr` / `intin` / `join` over a
   48-fact × 22-value universe, plus an exhaustive associativity tally.
   `cargo xtask lean-check` verifies the fixture is still what the spec prints;
   `--bless` rewrites it after a deliberate spec change.
3. **`cargo test -p steins-domain --test lean_vectors`** — the Rust
   implementation walks the same universe in the same order and diffs the
   rendered results line by line.

Both sides *render* rather than parse, so neither needs a parser and every line
carries its own inputs. Leg 3 is an ordinary test and needs no Lean, which is why
leg 1–2 **skip rather than fail** on a machine without a toolchain: the fixture is
committed, so the Rust-side check is always available. The toolchain itself is
pinned by `spike/lean-domain/flake.nix` (`nix develop`, Lean 4.30.0 from nixpkgs;
a separate `.#elan` shell for elan-managed toolchains).

## Scope, honestly

1,089 of 51,586 lines — **2.1%**. The remaining 98% is entangled with PHP's real
behaviour, Mago's tree, and salsa's query graph, and is not a formalisation
target. More pointedly, the live false-positive source is not the algebra:
[the ADR-0049/0052 soundness audit](../notes/20260724-adr0049-0052-soundness-audit.md)'s
G1 is four consumers reading `HeapObj.class` as an exact class when for `$this`
it is a lower bound. **Lean can establish that the algebra is right; it cannot
establish that a correct algebra is used correctly**, and the latter is where the
findings have been coming from.

The indirect benefit is real but should not be oversold: making γ explicit gives
the vocabulary to state a consumer's precondition ("is this fact's class exact or
a lower bound?"), which is precisely the distinction G1 lost.

Two things are deliberately **not** in scope:

- **`join` associativity is not proved.** It is checked exhaustively — 110,592
  triples over the vector universe, independently on both sides, zero mismatches
  — and recorded as an open item in `spike/lean-domain/REPORT.md` with where the
  difficulty lies. It matters because `join_envs` folds multi-branch joins
  left-to-right, so non-associativity would make diagnostics depend on the order
  the arms happen to appear in.
- **`steins-phpdoc` is not a target.** Its specification *is* the reference
  implementation's behaviour, and phpstan/phpdoc-parser has no mathematical
  definition; a Lean model would only prove the Lean model self-consistent.
  Compatibility is measured by the differential oracle, which already exists.

## Accepted costs, recorded honestly

A second language and toolchain enter the tree. They are contained: the spike is
not a workspace member, `lean-check` is not wired into `fp-gate` or the release
gates, and no shipped artifact depends on Lean. The spec is roughly 1,500 lines
of Lean for 1,089 lines of Rust — a 1.4:1 ratio that will not hold if the domain
grows, which is itself an argument for keeping the scope at the domain.

The vector fixture is 207 KB of generated text. It is diffable, and a stale
fixture is a hard error rather than silent drift, but it is not small.

The spec can rot: nothing forces an implementation change to be reflected in
Lean. The ratchet is the vector diff — an algebra change either leaves the
vectors identical, or requires a spec edit, a `--bless`, and proofs that still
compile. That is the mechanism, and it is the whole reason legs 2 and 3 exist
rather than the proofs alone.

Three places where Rust is `unreachable!`/`expect` and a total spec must say
something (`summarize`'s empty folds, `abstract_falsy_truthy` on a finite layer,
`joinFiniteAbstract`'s non-abstract operand) are modelled by **widening**, which
is the sound side. The theorems therefore describe the panic-free reading; if any
of those sites became reachable, Rust would panic where the spec widens.
