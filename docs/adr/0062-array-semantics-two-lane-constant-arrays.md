# Array semantics: the ConstantArrayType import, split across order-witnessed values and order-declared shapes

Owner directive (2026-07-29): array semantics follow PHPStan's
`ConstantArrayType` — array shapes, list shapes, sealed/unsealed — as the
specification baseline. ADR-0030 §2 already registers the one correction
Steins applies natively: the [#14939 RFC model](https://github.com/phpstan/phpstan/discussions/14939)
(`array{...}` is an order-agnostic key *set*, `list{...}` a positional key
*sequence*, `isList` a denotational trinary). This ADR fixes what that
means across the four-layer value domain (ADR-0035) and both lanes, names
every deliberate divergence, and slices the work. It leans on the
maintainer-side study notes at
[zonuexe/phpstan-notes](https://github.com/zonuexe/phpstan-notes/tree/master),
cited per file below; issue [#51](https://github.com/rigortype/steins/issues/51)
(the strict offset leg) is the first consumer.

## 1. Where arrays stand today (measured, v0.1.1)

| Surface | State |
| --- | --- |
| Value lane | `Val::Array` is concrete, PHP-normalized keys in **insertion order**; inhabits `Singleton`/`OneOf` only. The abstract layers (`Refined`/`General`) are scalar-only — ADR-0035's layer-4 text names "array<K,V>, shapes" as intended inhabitants, unimplemented. |
| Offset family (S3) | Fires on proven `Singleton`/all-array `OneOf` bases; `Refined`/`General`/shape-declared bases are silent. |
| Folding | Array *arguments* fold when written literally at the call (`count(['x','y'])` → `2`); the same array reached through a binding does not (`$a = ['x','y']; count($a)` → envelope `non-negative-int`). |
| Contract lane | `ContractTy::Shape` + `admits_shape` implement #14939 end to end: order-agnostic `array{}` acceptance, positional `list{}` **rejecting permutations** (ahead of PHPStan stable, which silently accepts them — the #12725 `$b` hole), sealed-by-default, typed unsealed tails, `non-empty-array{}`. Live on the contracts surface for proven values and definite-No abstract facts. |
| Acceptance duplication | The proven-value path judges through `accepts_shape` (PKind), facts through `admits_shape` (ContractTy). Measured divergence: `accepts_shape` does not check the unsealed tail **key** contract — `['a' => 1, 9 => 2]` passes `array{a: int, ...<string, int>}`. |
| Arm lane | `lower_shape` produces `Shape` arms and `seed_contract_arms` has no vocabulary filter, but `spell_arms` returns `None` on any array-bearing arm set, so the dump surface renders **"no declared contract"** for a seeded array param — an absence claim where a contract exists (#51 L1). No consumer reads `Shape` arms for offset knowledge. |

So today's array truth is all-or-nothing: fully concrete or absent. The
missing middle — an abstract array fact with ConstantArrayType's
expressive parts — is the core of this ADR.

## 2. The two lanes are the provenance split

The [#14717/#14940 analysis](https://github.com/zonuexe/phpstan-notes/blob/master/generated-report/20260709-issue14717-array-keys-order.md)
isolates a defect Steins must not import: `ConstantArrayType` cannot
distinguish an array whose insertion order was **observed** (built locally
from literals) from one whose shape was merely **declared** (PHPDoc, with
order-insensitive acceptance). PHPStan's positional projections trust
declared order and produce real false positives
([phpstan/phpstan#14940](https://github.com/phpstan/phpstan/issues/14940)).

Steins has the distinction structurally, and this ADR promotes it to a rule:

- **The value lane is order-witnessed.** `Val::Array` entries exist in true
  insertion order — literals, writes, and call-site propagation (ADR-0001)
  only ever build them by observing the construction. Order-dependent
  results (`array_values`, `array_keys`, `array_slice`, `array_reverse`,
  foreach sequencing, `array_is_list`) are sound **here and only here**.
- **The contract lane is order-declared.** A `Shape` is a key set (or, for
  `list{}`, a key-sequence promise). No transfer function may read field
  declaration order as runtime order. Positional projections over a
  shape-only truth take the sound widening (§4), never `list{k1, k2}` in
  declaration order.

Divergence-registry addition (extends ADR-0030 §2): *declared-order trust
in positional projections is declined*; the two-lane split is the
replacement, and its behavior is evidence for the upstream §B family.

## 3. The abstract array stratum: `Fact::Shape`

One new fact form gives arrays the same four-layer story scalars have.
Components mirror `ConstantArrayType`, re-expressed in fact discipline:

- **fields**: ordered-by-key list of `(VKey, presence, value-arms)`.
  Presence is `Required | Optional` — PHPStan's `optionalKeys`. Field
  *values* are contract arms (they come from docblocks: `Asserted`), but
  **presence carries its own stratum**: an `isset`/`array_key_exists` guard
  that really executed promotes presence to `Verified` while the value
  type stays `Asserted` — exactly the #51 L3 split, and the reason
  `works()`-style code discharges.
- **tail**: `Sealed | Unsealed(key-class, value-arms)` — sealed is the
  PHPStan 2.2 default; an untyped `...` is `Unsealed(array-key, mixed)`.
- **is_list**: a `Certainty`, computed **denotationally** per the
  [RFC audit](https://github.com/zonuexe/phpstan-notes/blob/master/generated-report/20260709-pr3872-array-list-shapes.md)
  (A1/A2): Yes iff every admitted value passes `array_is_list` (`array{}`,
  `array{0: T}`, `array{0?: T}`), No iff none does (a required string or
  gapped int key), Maybe otherwise (`array{0: T, 1: U}` — two realisable
  orders; optional-key combinatorics per
  [phpstan/phpstan#14938](https://github.com/phpstan/phpstan/issues/14938)).
  Never syntactic.
- **non_empty**: bool (`non-empty-array{…}` forms, `count()` guards).

Layer descent becomes total for arrays: `Singleton(Array)` → `OneOf` of
arrays → **computed** `Shape` summary → `ArrayAny`/`MapOf`-grade generality.
The `OneOf` cap is the only size constant: an overflowing array union
descends to the shape whose fields are keys-present-in-all-members
(required, value = member join), keys-present-in-some (optional), tail
unsealed by the undeclared residue. This replaces PHPStan's
`ARRAY_COUNT_LIMIT = 256` degradation — same role, but the summary is
computed member-by-member (ADR-0035's "widening is computed, not guessed"),
not a threshold heuristic.

**No abstract next-auto-index.** ADR-0049 A12 answers the next-int question
for concrete arrays, version-aware. An abstract shape declines the
prediction: append to a shape widens the tail (sound); the fact never
carries `nextAutoIndexes`.

- **covers**: the disjunctive-presence facts (`KeyCover`, #51 L4) live
  *inside* the shape fact — an antichain of key sets with a flavor bit;
  laws in Amendment A-G8.

**Lean obligation (ADR-0059).** `Fact::Shape` enters `steins-domain` only
together with its spec extension: `join_sound` over the field-wise join
(required∧required stays required, else optional; values join arm-wise;
`is_list` joins by Certainty; sealed joins per Amendment A-G5 — the
original "sealed only when both sides seal the same key universe" here was
wrong and is superseded), `summarize_admits` for the OneOf descent, and
agreement between Rust `admits_shape` and the Lean acceptance on the
differential vectors. The form is recursive (field/tail value slots nest),
so the spec becomes an inductive type with a size measure.

## 4. Transfer functions: the sound table

"Concrete" = value lane (order-witnessed); "shape" = the abstract fact.

| Operation | Concrete | Shape |
| --- | --- | --- |
| read `$x[k]` | exact entry; absence → `offset.missing` (proof layer, unchanged) | required field → its value arms; optional → unknown + the #51 strict leg when undischarged; undeclared key under a sealed tail → *declared-absence*, reported on the contract/strict surface only (sealed is `Asserted`-world evidence — never the proof layer) |
| write `$x[k] = v` | entry replace/insert in order (builder semantics) | field update to `Required` with `v`'s fact; an undeclared key unseals the tail (sound); `is_list` recomputed denotationally |
| append `$x[] = v` | A12 (concrete, version-aware) | tail widen; a Yes-list shape stays Yes (append preserves list-ness) |
| `unset($x[k])` | entry removal | `Required` demotes to absent-on-branch, `Optional` stays; `is_list` recomputed (a mid-list unset is No/Maybe by position knowledge) |
| `isset` / `array_key_exists` guards | already narrow concrete bases | presence promotion to `Verified` (#51 L3); `isset` additionally strips null; disjunctions record a KeyCover fact (#51 L4); false branches demote/remove |
| `?? ` | unchanged | right-most arm judged under ¬isset(left arms), consuming KeyCover (#51 L5) |
| `count($x)` | fold | sealed all-required → exact `LitInt` (the one place PHPStan has exact size — mirrored); optionals/unsealed → `IntIn(required-count, max)`; `non_empty` floors at 1 |
| `array_is_list($x)` | fold-grade by inspection | answer = `is_list`; true-branch narrowing sets Yes, false-branch No — the RFC's C1: a pure flag flip, no structural surgery |
| `array_values` / `array_keys` / `array_slice` / `array_reverse` | execute on the witnessed order (sound) | **sound widening only**: value/key unions with list-ness and size bounds from §3 — never declaration-order `list{k1, k2}` (the declined import, §2). `array_slice` additionally reads its own `$offset`/`$preserve_keys` arguments, which are not order (Amendment B) |
| `foreach` | real order | order-independent: key/value unions over fields + tail; first-iteration facts are unions, not the first declared field |
| `array_all` / `array_any` (8.4) | n/a (callback) | v1 imports only the [redesign note's](https://github.com/zonuexe/phpstan-notes/blob/master/array-all-any-type-specifying/02-redesign.md) unconditional legs: `array_all` falsy → `non_empty`, `array_any` truthy → `non_empty`. The empty-array vacuity trap (its probe E: never manufacture `never` — `array_all([], f)` is true) is the same principle `Certainty::all_of` already proves maybe-on-empty for (ADR-0059). Callback descent is deferred. |

Loop-carried list-ness (the
[flow-side note's](https://github.com/zonuexe/phpstan-notes/blob/master/generated-report/20260715-list-loop-semantics.md)
G1–G5 families) is **out of scope**: Steins' concrete unrolls already cover
the small-N constant cases, and the general loop-schema question (a loop as
a map/filter/build over an array) is a separate design with its own ADR
when the corpus demands it. None of PHPStan's syntactic rescue heuristics
(`shouldKeepList`'s four forms &c.) are imported.

## 5. One acceptance relation

The proven-value path (`accepts_shape` over PKind) and the fact path
(`admits_shape` over ContractTy) are two implementations of one relation,
and they have already diverged once (the tail-key gap, §1). The proven
path lowers to `ContractTy` and judges through `steins-contract` — a
single acceptance source, per ADR-0030's no-second-relation discipline.
The fixture: `array{a: int, ...<string, int>}` must reject `['a' => 1,
9 => 2]` on both paths.

## 6. Spelling follows the RFC's D4

`spell_arms` learns the array vocabulary, with round-trip faithfulness as
the rule: a Yes-list spells `list{...}`, a Maybe-list with sequential keys
spells keyless `array{...}` (that spelling round-trips to Maybe), the
non-denotable subtraction residue spells keyed `array{0: …}` — the D4
resolution, run natively here (BC-free proving ground, ADR-0016). This
retires the "no declared contract" rendering on seeded array lanes (#51
L1) and gives `Singleton(Array)` dumps a faithful spelling through the
same one speller.

**Amendment (2026-08-07, issue #172): visibility by dedicated verdict,
not by residence in `differ`.** This section's visibility law — a
D4-native spelling divergence (the oracle asserts `array{…}` where
Steins spells the same denotation `list{…}`) stays visible in the nsrt
harness and is never normalized away — was originally *mechanized* by
keeping such rows in the `differ` bucket, pinned by the harness's D4
exemplar test. Once the acceptance relation learned to prove both
directions for these pairs, that mechanism defeated the law's purpose:
`differ` is the least visible place in the harness, and the class sat
buried among eleven thousand genuine gaps, countable only by hand. The
mechanism is hereby amended: the harness awards such pairs a dedicated
`equal` verdict, granted exclusively when the relation itself proves
both directions (`expected ⊇ got` and `got ⊇ expected`, each
`Certainty::Yes`) while the normalized spellings differ. The law's
*substance* is unchanged and strengthened, not weakened: no
normalization rule may absorb the class (a pair the relation cannot
prove equal both ways stays `differ`, as a relation gap to file), and
the class is now countable in the summary and listed row by row instead
of drowned in the gap inventory. The D4 exemplar pin flips with this
amendment in the same commit.

## 7. Declined imports (divergence registry)

1. Declared-order trust in positional projections (§2) — replaced by the
   provenance split.
2. Abstract `nextAutoIndexes` (§3) — concrete-only via A12.
3. `ARRAY_COUNT_LIMIT = 256` union degradation — replaced by OneOf-cap
   computed descent. (The 256 constant itself returns in Amendment A-G6 as
   the single-shape *field-width* bound; the union-degradation role stays
   declined.)
4. Loop list-ness rescue heuristics (§4) — not imported; loop-schema
   inference is future, separate work.
5. (Standing, ADR-0030) benevolent unions; the accepts/isSuperTypeOf
   asymmetry.

## 8. Slices

Resequenced by Amendment A-G12 (S0–S8, domain-first) — the governing
order lives there; the A-numbers below are retained as content references.

| Slice | Content | Consumer / instrument |
| --- | --- | --- |
| A1 | Acceptance convergence: proven path lowers to ContractTy; tail-key fixture | live FN fixed; conformance |
| A2 | Array spelling (D4) in the one speller | #51 L1; dump surface honest |
| A3 | Shape arms consumed by reads; `count`/`array_is_list` transfers | #51 L2; nsrt |
| A4 | Presence promotion + KeyCover | #51 L3/L4 |
| A5 | Coalesce right-arm discharge | #51 L5 |
| A6 | `Fact::Shape` in steins-domain + Lean spec extension; OneOf-of-arrays descent | ADR-0059 gate; lattice vectors |
| A7 | Fold seam: env-resolved array arguments; positional projections as sound widenings | measured folding gap; nsrt |
| A8 | `array_all`/`array_any` non-empty legs | nsrt |

Every slice runs the full protocol: measurement-mode opening per new id,
corpus verbatim triage, nsrt before/after (arrays are the largest nsrt
mass), conformance, fp-gate EXACT. A3–A5 sequence inside #51's emission
gate: the strict leg emits nothing until guarded code is clean.

## Open questions (pending ratification)

1. **Positional projections: widen vs decline.** §4 chooses the sound
   widening (`array_values(shape)` → bounded list of the value union) over
   returning unknown. Widening is more useful and still sound; decline is
   cheaper. Default: widen.
2. ~~**A6 timing.**~~ **Settled by Amendment A-G12** (owner, 2026-07-29):
   domain-first — the canonical fact lands before the guard slices, because
   the covers live inside it (A-G8) and arms-first would force a temporary
   home plus a migration.
3. **D4 spelling immediately vs behind the strict surface.** Default:
   immediately — the dump surface is debug-grade and BC-free here.

## Amendment A (2026-07-29): grilling resolutions

Owner-settled in a branch-by-branch design interrogation; each item was
confirmed individually. Where an item contradicts the body above, the
amendment governs.

- **A-G1 — One canonical form.** The fact domain does **not** mirror the
  contract lane's `ArrayAny`/`ListOf`/`MapOf`/`Shape` split. All four
  lower into the single shape fact: `array` = no fields + untyped unsealed
  tail; `array<K, V>` = typed tail; `list<T>` = typed tail + isList Yes;
  `array{…}`/`list{…}` as declared. One join, one acceptance, one Lean
  algebra; the contract lane keeps its spellings.
- **A-G2 — Fact placement.** `Fact::Shape { shape, nullable }` is the
  fifth `Fact` variant; `nullable` is the same side-flag `Refined`/
  `General` carry, never a field inside the shape (field-value nullability
  belongs to that value slot's own arms — no double representation). No
  array-`General` variant exists: the degenerate shape fact absorbs
  ADR-0035's layer-4 array text. Mixed-base unions (`array{…}|string`)
  stay un-facted, exactly as for scalars.
- **A-G3 — Discriminated unions live in the arm lane.** A shape∪shape
  union is never dropped to unknown: it persists as multiple `Shape` arms,
  and the fact lane holds a single shape fact only once the union has
  collapsed to one arm. Presence-based subtraction: `isset($x['k'])` true
  eliminates sealed arms lacking `k` (sealed-by-default is what makes the
  idiom sound); false eliminates arms where `k` is Required with a
  non-nullable value. Concrete unions stay `OneOf`.
- **A-G4 — Tagged-union discrimination.** Constant-key projection guards
  (`===`, `match`, `switch`) subtract base arms by the field-value
  `admits` verdict: a No arm is eliminated, so
  `match ($w['type']) { 'x' => $w['x_url'], … }` collapses the union per
  arm and each read spells its declared contract with zero findings on
  every surface (the ADR's acceptance fixture, neutral tag names). v1
  scope: binding base, constant key, literal int/string tags. Symmetry:
  isset-discrimination is powered by *sealed*; tag-discrimination by
  *Required + literal exclusivity*.
- **A-G5 — Join laws (correcting §3).** Field-wise: key on both sides →
  presence join (`Required(s₁)⊔Required(s₂) = Required(min stratum)`; any
  Optional → Optional), values join arm-wise. Key on one side → Optional;
  value joined with the other side's tail value bound when that side is
  unsealed. Tail: `Sealed⊔Sealed = Sealed` — **`Sealed{a}⊔Sealed{b} =
  {a?, b?} + Sealed`**, optionality absorbs the key-set difference (the
  body's "same key universe" condition was wrong); `Sealed⊔Unsealed(K,V) =
  Unsealed(K,V)`; `Unsealed⊔Unsealed` joins key-class and value. isList:
  trinary or. non_empty: and. nullable: or. Concrete⊔abstract lifts the
  `Singleton` (all fields Required/Verified, sealed, isList computed by
  `array_is_list`) and then shape-joins — the lift is where
  order-witnessed-ness is honestly lost.
- **A-G6 — Field-width bound = 256, imported.** Lifting or seeding a
  shape beyond 256 fields (PHPStan's `ARRAY_COUNT_LIMIT`, adopted as-is
  rather than a novel constant) degrades to the tail-only summary
  (unsealed key-class/value join + non_empty + isList). Orthogonal to the
  OneOf cap (8), which governs how many whole arrays stay finite.
- **A-G7 — No general meet in v1.** Narrowing ships as targeted
  refinement operators (presence promotion, arm subtraction, isList flip,
  non_empty set, cover recording); a general ⊓ waits for a real consumer.
- **A-G8 — KeyCover laws.** Covers live inside the shape fact as an
  antichain of key sets, each of size ≥ 2 (a singleton cover normalizes to
  presence promotion; a cover containing a Required key drops as
  redundant). Each cover carries a flavor: **Isset-cover** (from
  `isset(…)||isset(…)`: at least one key present *non-null*) or
  **KeyExists-cover** (from `array_key_exists` disjunctions: at least one
  key *exists*, value possibly null). Join keeps a cover iff both sides
  imply it (via a subset cover or a Required member key). Invalidation:
  `unset($x[k])` kills every cover containing `k` and marks `k` absent; a
  write to `k` promotes `k` to Required/Verified and drops the covers it
  satisfies; a nested write autovivifies the outer key; rebinding clears
  everything; by-ref exposure and by-ref builtins (`sort` &c.) havoc the
  fact (v1); by-value flow copies facts freely (PHP CoW semantics).
  *S2 correction (flavor-aware implication):* a `Required` key discharges a
  **KeyExists**-cover unconditionally, but an **Isset**-cover only when its
  value slot proves non-null — a required key whose value may be null does
  not make `isset` true, and a join keeping a cover one operand does not
  satisfy would reject values that operand admits. The original "a Required
  member key implies the cover" was flavor-blind and unsound on that edge.
- **A-G9 — Reads are never null-poisoned.** An unguarded optional-key
  read yields the declared value arms (`Asserted`), not arms∪null: the
  missing-ness hazard is the strict leg's *finding*, never a type
  pollution (PHPStan's posture, imported; it is what keeps non-strict
  surfaces zero-FP downstream). Stratum law: a read result never exceeds
  its value slot's stratum; presence stratum and value stratum are
  independent. Corollary, normative: **shape-derived facts never feed
  proof-layer findings.**
- **A-G10 — The finding ladder and its wiring.** Three ids:
  `offset.missing` (proof evidence, default surface — unchanged);
  `offset.undeclared` (new: constant-key read outside a sealed shape's
  fields — definite absence *conditional on the docblock*; layer
  contracts, floor contracts); `offset.maybe-missing` (undischarged
  optional-key read; layer contracts, floor strict). The registry gains
  exactly one attribute — `surface_floor ∈ {default, contracts, strict}` —
  and profiles are the cumulative ladder `default ⊂ contracts ⊂ strict`.
  Baselines record their capture surface; `suppress.unmatched` judges only
  ids fireable at the current surface. v1 emission: shape-declared bases
  with constant keys only. General-map/list reads do not fire in v1 but
  are counted in triage (#50) buckets from day one; a future general leg
  is a separate id (PHPStan's two-flag split, imported).
- **A-G11 — Coalesce premise scope.** Premises arise only from pure
  projection arms (binding + constant key, depth 1); each arm carries
  ¬isset of every prior arm; a non-projection arm (any call) contributes
  no premise *and invalidates the accumulated ones* for later arms
  (by-ref/global mutation conservatism); mixed bases discharge nothing.
  Discharge table: Isset-cover + ¬isset(a) ⇒ isset(b), unconditionally;
  a KeyExists-cover discharges only when the prior arms' values are
  non-nullable — with a nullable value, present-null falls through at
  runtime and the right arm may truly be missing (real semantics, not
  imprecision).
- **A-G1a — Implementation note on value slots (orchestrator, pre-S2).**
  In `steins-domain` the canonical form's field/tail value slots are
  recursive `Option<Fact>`, not ContractTy arm lists — the domain crate
  cannot (and must not) name `ContractTy`; the dependency points the other
  way. Declared fidelity the fact domain cannot express (class instances,
  callables) stays in the *aligned* `Shape` arm in the arm lane, which is
  flow-immutable; flow refinement (presence, covers, subtraction) lives in
  the domain form, kept per-binding as a vector aligned index-wise with
  the declared arms. A read takes presence from the flow form and its
  value from the slot fact, else the aligned declared arm, by trust order.
  This refines A-G1's "stratum-carrying arm lists" phrasing, which was
  written before the dependency direction was checked.
- **A-G12 — Domain-first resequencing** (settles Open Question 2). S0
  acceptance convergence (was A1) → S1 spelling (A2) → S2 domain core:
  `Fact::Shape` + lowering + join + lift + computed descent (was A6) → S3
  shape reads (A3) → S4 guards: presence promotion, arm subtraction incl.
  tagged, the invalidation table (A4, expanded) → S5 KeyCover + coalesce
  discharge (A4/A5) → S6 strict-leg emission + `surface_floor` + the
  `strict` profile + baseline surface tags (#51's emission gate) → S7
  positional projections + fold seam (A7; parallel to S3–S6) → S8
  `array_all`/`array_any` legs (A8). Rationale: covers live inside the
  shape fact (A-G8), so arms-first would force a temporary cover home and
  a migration.

## Amendment B (2026-08-02): the S7 seam grows an argument channel — PENDING ratification

S7 shipped with `array_slice` **declined**, and the report gave the
decline one reason: *the seam is single-argument by construction, so the
shape-only answer carries no more than the reflected `array` envelope
already does*. Both halves of that sentence are addressed here, and
neither by weakening a rule.

**The constraint was the decline's whole content, so the constraint
went.** The projection rung now receives the call's own argument list and
may read a sibling argument's fact through the same per-argument reader
the ADR-0064 seam-ii rung next door already owns. Nothing about the rung's
*shape* changed: every arm that does not ask keeps the single-shape
pattern it had, and an arm that asks states the arity it was written
against.

**§2's order boundary is untouched, and that is the load-bearing claim.**
What the grown channel carries is a `$preserve_keys` flag and an offset —
values, not order. No arm may read field declaration order as runtime
order, so the declined import (§7.1) stands exactly as written: a
contract-lane subject never projects positionally, whatever its offset
argument says. The negative fixture is explicit — a declared
`array{a: int, b: string}` sliced at offset 1 takes the widening, because
`['b' => 's', 'a' => 1]` is admitted just as well and upstream's
`array{b: string}` is wrong on it.

**The widening floor supersedes the "no more than the envelope" claim.**
The element union is the counterexample the v1 report missed:
`array_slice(list<Foo>, $n)` is a `list<Foo>` and the envelope says
`array`. The floor is sound for *any* offset and length, and claims four
things, each read from order-independent structure:

- **element bound** — the slice's values are a subset of the subject's, so
  the value union carries across unchanged;
- **key class** — a slice never invents a key class; `$preserve_keys =
  true` keeps each surviving key, `false` renumbers integer keys and
  leaves string keys alone, so an all-int subject yields all-int keys
  either way;
- **list-ness for exactly one combination** — an all-integer-keyed subject
  under an absent-or-false flag is renumbered `0..n-1` and *is* a list; a
  truthy — or merely *unknown* — flag degrades it to `Maybe`, never to
  `No` (the empty array a slice can always return is itself a list);
- **`non_empty` never** — `array_slice([1,2,3], 10)` is `[]`, so the flag
  is dropped unconditionally.

The size bound is deliberately not claimed: expressing "at most the
subject's count" needs a sealed result shape with keys the projection
cannot name, and an unsealed tail is the sound direction.

**The exact rung is the value lane's privilege, and only its.** A subject
whose fact is a witnessed `Val::Array` carries true insertion order, so
with a `Singleton` offset and length and a literal `$preserve_keys` the
projection is *executed* over php-src's own window arithmetic rather than
widened. Failing any of those three premises falls to the widening over
the **lift** of the same entries — which is where order-witnessed-ness is
honestly lost — so a value-lane subject is never worse off than a
declared one.

**`min`/`max` (the same slice, the DR3 rung).** They return **one of their
arguments**, whatever the comparison did, so the union of the argument
facts admits the result unconditionally — no premise about ordering or
type juggling is needed, which is what makes the rule worth having where a
per-type case analysis would not be. Two or more int-ranged arguments
compose intervals instead (`min(a, b) ∈ [min(lo), min(hi)]`, `max`
dually), which is interval arithmetic over declared knowledge — the
`count()`/`strlen` precedent — and never a re-derivation of the
comparison. One argument is the array form and answers from the shape's
value union; `min([])` throwing costs the rule nothing, because a throw is
the absence of a return.

**ADR-0064 Amendment B's arity leg reaches the DR3 rung.** `min`/`max`
declare a bare `mixed`, which pins nothing on its own, so the rung grew
the same second leg S7 already carried: the measured `(2, 1)` signature is
what countersigns the rule, and an engine that cannot state it withholds.
The rung's `mixed` assertion turned from a flat refusal into the same
obligation — name `mixed`, pin the signature. `json_decode` remains
declined for its own reason (a six-base envelope with no single `Fact`),
which no arity leg would change.

**Deviation from the slice design, recorded.** The design routed a
multi-base `min`/`max` union (`min($int, $string)` → `int|string`) into the
**arm lane**. The arm lane has no argument-dependent channel at this seam:
ADR-0069's floor is keyed by *name* alone (`builtin_return_floor(name)`),
and the return-fact rungs carry one `Fact`, not an arm list. Rather than
grow a second channel for one row, such a call **declines** — the honest
floor, and the same outcome `json_decode`'s unspellable envelope already
takes (ADR-0061 §1: a rule that cannot state its own answer says nothing).
Should a consumer want those unions, the argument-dependent arm channel is
its own slice.

**§7's registry is unchanged.** `array_slice` was never a declined
*import*; it was an unimplemented transfer, and the one entry the family
still has — the value side of `in_array`/`array_search` — declines for the
domain reason, not the seam one.
