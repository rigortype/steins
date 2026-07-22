# The four-layer value domain: refinements as predicate sets, not accessory types

Ratifies the internal type representation. The inference environment's value
domain has four layers, ordered by precision:

```
1. Singleton   — one concrete value: a literal (incl. arrays), an exact-class
                 instance. "A type with one inhabitant" — the maximal sieve.
2. OneOf{…}    — a finite value set (cap ~8): the join of disagreeing branches.
3. Refined     — base type + refinement: for strings a predicate bitset
                 (NonEmpty, NonFalsy, Numeric, …), for ints an interval
                 IntRange(min, max) as the canonical form (positive-int etc.
                 are spellings, not representations).
4. General     — the bare type: string, Foo, array<K,V>, shapes.
```

**Widening is layer descent, and it is computed, not guessed**: a OneOf
overflowing its cap widens to the Refined whose predicates *all members
satisfy* — the predicate summary is evaluated member-by-member, so precision
loss is exactly measured. Descent is total and monotone; termination of any
fixpoint over the domain is structural. Refinements are produced by guard
survival (branch analysis stage 2's negative facts: `$s !== ''` yields
NonEmpty; `$n > 0` yields IntRange(1, max)) and consumed by contract
acceptance — both directions defined extensionally on value sets (ADR-0030
core), with all judgments in the unified Certainty.

## Trade-offs against the two ancestors

**PHPStan accessories** (refinement = IntersectionType of base +
`Accessory*Type` classes, ints separately as `IntegerRangeType`):
- Strengths: uniform "everything is a Type" interface; open extension (a new
  accessory is a new class); battle-tested vocabulary.
- Costs we decline: the N×M pairwise `accepts`/`isSuperTypeOf` dispatch
  matrix across type classes, where each new accessory multiplies
  interaction cases and consistency is maintained by hand (a recurring
  upstream bug source); intersection trees need normalization passes and
  have no canonical form by construction; allocation-heavy object graphs on
  the hot path; heterogeneous treatment of strings (accessories) vs ints
  (ranges).

**Rigor carriers** (`Constant[T]`, `Tuple`, `HashShape`, `Refined`, … — a
set of internal type objects each satisfying a documented contract):
- Strengths: value-first, exactly a bug-finder's priorities; purpose-built
  carriers with explicit erasure; the closest ancestor — our layer 1 is
  Rigor's `Constant`, our layer 3 its `Refined`.
- Costs we decline: an open, duck-typed carrier set suits Ruby's dispatch
  but not Rust — each carrier must hand-implement a wide internal API, and
  nothing forces exhaustive handling of carrier×carrier interactions.

**Why the four-layer form wins for Steins**:
1. **Closed Rust enums**: every layer transition and predicate interaction
   is exhaustively matched — the compiler enforces the N×M completeness
   PHPStan maintains by review, and adding a predicate is one variant plus
   its Certainty evaluators.
2. **Canonical by construction**: predicate bitsets and intervals need no
   normalization pass; meet/join are bit-ops and interval hulls — O(1)
   algebra where accessories need object-graph surgery.
3. **A real widening operator**: layer descent with computed predicate
   summaries gives fixpoints a structural termination argument; neither
   ancestor has a systematic story (thresholds and per-carrier judgment).
4. **Guard-native**: refinements are literally "what survives a guard,"
   which is what a value-precise, branch-pruning analysis produces anyway —
   representation and analysis output coincide.

Accepted costs, recorded honestly: the predicate set is closed (plugin
refinements need a future registry; until then unknown predicates are maybe
→ silence — the safe side); the two-relation split (ADR-0030) means contract
and runtime checkers consume the domain separately; display requires an
explicit mapping to PHPStan vocabulary (`non-empty-string`, `int<1, max>`)
— representation diverges, rendering and extensional semantics hold parity
via the ported reference tests. Registered in the divergence registry.
