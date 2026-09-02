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
| append `$x[] = v` | A12 (concrete, version-aware) | the key `max(int keys) + 1` added when the sequence is *witnessed*, else a tail widen; `is_list` **never** carried (Amendment K corrects this row: append does not preserve list-ness) |
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
  *(Amended by ADR-0078 §6, 2026-08-09: the floor set gains a fourth
  member, `pedantic`, and stays a total order; the rungs remain a
  cumulative chain but the built-in profiles are not one — `pedantic`
  branches off `contracts` as `throws-direct` already branched off
  `default`.)*
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

## Amendment C (2026-08-13): a literal keeps its fact when its elements do not — PENDING ratification

Issue [#327](https://github.com/rigortype/steins/issues/327). §3 built the
abstract array stratum and §8's slice table fed it from exactly one place:
a docblock. The other place an array's structure is known — the source
writing one down — was never wired to it, and the effect was a cliff rather
than a gap. `val_of` needs a proven `Val` per element and answers `None` on
the first one it cannot build, so **one** unproven element dropped the fact
for the **whole** array: its keys, its entry count, its sealing and every
proven sibling. `['p' => 1, 'q' => $s]` knew nothing where the reference
implementation knows `array{p: 1, q: string}`, and `count()` of it was
`int<0, max>` where the count is two whatever `$s` turns out to be.

Nothing about the keys was ever in doubt. `normalize_array` resolves auto
indices, last-wins duplicates and A12's version-dependent next-int rule
without inspecting a single value, so the key sequence is computable
whatever the elements are — and a literal, by being a literal, seals its own
key universe.

### C1. The seeding ladder

An array literal in rvalue position seeds by two rungs, most precise first:

1. **Every element proven** → `Fact::Singleton(Val::Array)`, `Verified`.
   Unchanged, and pinned as unchanged: the abstract rung may not be paid for
   with the concrete one.
2. **Otherwise** → `Fact::Shape` (`ShapeFact::from_witnessed_entries`): each
   normalized key a field at `Presence::Required { witnessed: true }`, its
   value slot that element's own fact and `None` where nothing proved one;
   `Tail::Sealed`; `is_list` denotational over the **witnessed key
   sequence**; `non_empty` from the entry count. Over `SHAPE_WIDTH_LIMIT`
   it degrades to the tail summary exactly as `lift` does, and one unknown
   slot makes that summary's value bound unknown.

`normalize_array` declining (A12: a literal straddling the 8.3 next-int
change on an unpinned minor) declines the **whole** literal at both rungs. A
guessed key set is wrong rather than wide, which is not a trade this domain
makes.

**Stratum** is ADR-0061 §3's derivation clause: `min` over the element facts
that contributed one. An unknown slot contributes nothing — it makes no
claim to be trusted at any stratum. A literal over native elements is
therefore `Verified`, and rightly: the keys were observed in the source, the
sealing is what a literal *means*, and the slots are what the walk proved.
A-G9's corollary keeps its `Asserted` cousins out of the proof layer as
before.

### C2. The order witness

§2 splits the lanes by provenance and §7 declines declaration-order trust in
positional projections, which is phpstan/phpstan#14940's false-positive
class. `ShapeFact` could not *hold* the other side of that split:
`normalize_counted` sorts fields into canonical key order, so a shape seeded
from `['b' => 1, 'a' => $x]` was indistinguishable from one declared
`array{a: …, b: …}` — and the sequence really was known.

`ShapeFact` therefore gains **`order`**, the key sequence as observed, set at
exactly two construction sites (this seeding, and `lift`) and dropped by
every other operation: `normalize_counted` — which every derived shape goes
through — always sets it to `None`, and the single struct-update that
bypasses it (`relax_count_ceiling`) drops it by hand, because the write it
models can append a key the sequence does not mention. Losing it costs
precision and never soundness, which is the direction to fail in.

It is **provenance, not extension**, exactly as `Presence::Required {
witnessed }` already is: `admits` never reads it, two shapes differing only
here admit the same values, and `with_order` refuses a claim inconsistent
with the shape it is attached to rather than storing it. That inertness is
also why it carries no ADR-0059 obligation — the Lean spec models the
extension, and the extension did not move. It is pinned in the domain's own
tests instead (`the_witness_is_extensionally_inert`), and `lean-check`
agrees at 12208 lines.

Two consumers, both of which are the split doing its job:

- **`is_list` is decided by the sequence, not the key set.** `[1 => $x,
  0 => $x]` has the key set of a two-element list and is not one. The
  canonically sorted fields cannot tell it from `[0 => …, 1 => …]`.
- **Spelling follows provenance.** A witnessed shape prints the order it
  saw — the order its `Singleton` sibling has always printed, and the
  reference implementation's — while a declared shape keeps the canonical
  order, which is §6 saying out loud that `array{b: int, a: string}` is an
  order-agnostic key set. The two must not look alike.

The order-dependent *projections* do not read it yet; that is issue #328.

### C3. §4's write row, on a witnessed base

The write row was implemented for a declared base only, so `$a = [];
$a['k'] = $x;` dropped the binding — the same cliff reached from the other
direction. A base holding an order-witnessed value now lifts and takes the
same path, and the witness changes one rule:

**Writing an undeclared key under a sealed tail unseals it only when the
sealing was declared.** A witnessed base has no docblock to have diverged
from — its sealing is a fact about the array the code built, and adding a
key yields another array the code built, still exactly known. There the
write *extends* the sealed shape (the field is added by hand;
`promote_present` can only promote a key a sealed shape already declares),
the witness grows by the new key at the end where PHP puts it, an
overwrite moves nothing, `unset` takes the key out of the sequence, and
`is_list` is recomputed from the new sequence. The declared case is
untouched: its sealing is a claim the write just falsified, so the tail
opens.

The written value's own fact now comes through the same argument-fact
ladder an ordinary assignment uses, so `$a['k'] = $x` with a native
`int $x` lands as `int` rather than unknown — and brings its stratum with
it, because the binding must not come out more trusted than what was
written into it.

### C4. Measurement

nsrt, same input both sides: headline 2414 → **2430**, admissible 2820 →
**2852**, differ 11071 → 11039. The full transition matrix is
one-directional — 16 `differ → match`, 14 `differ → subsumed`, 2
`differ → equal`, and **zero** rows moved the other way.

fp-gate GREEN: proof layer 0 across 6670 files, unchanged. One
contract-layer movement, triaged verbatim rather than reseeded:
`composer/composer` 19 → 21 `phpdoc.*`, both new findings the *same* true
positive the same file already carried — an undeclared `type` key passed to
a sealed `@param array{url: string}`, silent before only because the
`__DIR__ . '…'` element used to drop the whole argument's fact along with
its keys. Measured on a fixture that the unknown slot is not what fires: an
unknown value against a declared `string` slot stays silent, because Maybe
is silence.

## Amendment D (2026-08-13): the positional projections execute on the witnessed lane — PENDING ratification

Issue [#328](https://github.com/rigortype/steins/issues/328). Amendment C gave
`ShapeFact` an order witness and left it unread. This is the consumer. §4's
transfer table said "Concrete: execute on the witnessed order (sound)" for
`array_values` / `array_keys` / `array_slice` / `array_reverse`, and only
`array_slice` ever did (Amendment B); every other name fell through to the
`lift` and took the key-set widening — the answer a *declared* shape deserves,
handed to a subject whose construction had been observed.

**The rule.** A subject that is a sealed, all-required shape carrying an order
witness — a literal the walk saw built, or the `lift` of a proven `Val::Array` —
has its projection *executed* over that sequence rather than widened. The four
names, each from a probe at 8.5.9:

| name | rule | probe |
| --- | --- | --- |
| `array_keys` | keys become values, reindexed `0..` | `array_keys(['b' => 1, 'a' => 2]) === ['b', 'a']` |
| `array_values` | values reindexed `0..`, slots unread | `array_values(['b' => 1, 'a' => 2]) === [1, 2]` |
| `array_reverse` | reversed; string keys survive in place, integer keys renumbered `0..` in the new order | `array_reverse(['a' => 1, 5 => 2, 'b' => 3, 9 => 4]) === [0 => 4, 'b' => 3, 1 => 2, 'a' => 1]` |
| `array_flip` | keys and values swap, values normalized as keys, last wins, a non-`int\|string` value skipped | `array_flip(['x' => '1']) === [1 => 'x']`; `array_flip(['a', 'a']) === ['a' => 1]`; `array_flip(['a', 1.5, 'b']) === ['a' => 0, 'b' => 2]` |

Three of the four never read a value, which is what makes them answer where a
fold cannot: `array_keys(['a' => $x, 'b' => $y])` is `list{'a', 'b'}` because the
result's *values* are the subject's *keys*. `array_flip` is the exception and
**declines** on an unproven slot — its result's *keys* come from the subject's
*values*, so an unknown value is an unknown key and there is no honest partial
answer; it falls to the widening rather than to silence. The output is a
`Singleton` where every slot it needed was proven and a witnessed `Fact::Shape`
otherwise, so an unknown element costs one slot rather than the sequence.

**§7's declined import is untouched, and that is the point.** A shape with no
witness is a key set: `array_keys(array{a: int, b: int})` stays
`non-empty-list<'a'|'b'>` and may never become `list{'a', 'b'}`, because the
declaration admits `['b' => …, 'a' => …]` just as well. An *optional* field
declines too, witness or not — `list{int, 1?: string}` realizes as one entry or
two, so no single sequence describes every admitted value
(`ShapeFact::witnessed_order` carries that precondition). The admission gate is
`transfer_declaration_admits` over the engine's own `array` declaration, so a
silent engine withholds the family and ADR-0069's floor answers instead.

**Not folded, and why.** Handing these four to the sidecar would be the ADR-0004
answer; the reason it is not taken is coverage, not doctrine. A fold fires only
when *every* argument is a literal, so it cannot reach the partially-known
subject at all — and that subject is what real code is made of. These four
restructure the argument and read no value semantics beyond key normalization,
which this codebase already owns version-aware and which `array_slice` already
re-derives under the same justification. The names that genuinely need PHP's own
semantics — `explode`, `array_unique`, `array_merge`, `range` — are held back for
the fold-result slice (issue #330), where the engine answers for them.

**Measurement.** nsrt headline 2430 → **2433**, admissible 2852 → **2858**,
transition one-directional (3 `differ → match`, 2 `differ → subsumed`, 1
`differ → equal`, zero the other way). The gain is small *here* by design: nsrt's
array fixtures are overwhelmingly declared shapes, which correctly keep their
widening — the slice pays out on literal-heavy code, which the harness does not
sample. fp-gate GREEN with **no baseline movement at all**: proof layer 0,
`phpdoc.*` and `throw.*` unmoved.

## Amendment E (2026-08-13): an exact array is a value, not only a fact — PENDING ratification

Issue [#329](https://github.com/rigortype/steins/issues/329). Amendments C and D
made the value lane answer exactly; this is what makes the answers *compose*.
`resolve_literal`'s call arm consulted two sources — the zero-argument constant
function and the allowlist fold — and the transfer rung was not one of them. So a
call whose *fact* was a proven `Singleton` resolved to no *value*, and everything
reading values rather than facts went blind to it: value-position `===`, fold
arguments, nested folds, `concat_cast`. One hop through a binding worked and the
inline spelling did not, which is not a distinction PHP makes.

The rung's answer now resolves as a value when it is a `Singleton`, carrying the
rung's own stratum (ADR-0061 §3) so a projection over an `Asserted` subject
cannot launder into a `Verified` premise by taking the value road instead of the
fact road. Anything else — a `Shape`, a `OneOf`, a decline — resolves to nothing,
as before. The rung's *subject* accepts a call for the same reason, which is what
makes a projection of a projection compose; it terminates because each level
strips one call from a finite expression, and its fixture pins that it answers
what the two-statement spelling answers.

`array_keys(['a' => 1, 'b' => 2]) === ['a', 'b']` is `true`, and
`implode(',', array_keys(['a' => 1, 'b' => 2]))` is `'a,b'`.

**The guard position is deliberately not included, and the reason is worth
recording.** A call in comparison-guard position lowers to `CondOperand::Other`
and `operand_values` answers `None` for it — but measurement shows that gap is
not array-shaped at all: `if (strtoupper('a') === 'A')` does not decide either,
and neither does any other folded call. It is a general call-operand gap whose
fix moves branch decisions, dead-code marking and every finding downstream of
them, so it wants its own slice and its own measurement rather than a ride on
this one.

### E1. The latent defect this exposed

`resolve_cval`'s call arm wrapped whatever the value seam returned in
`CVal::Scalar`. That was correct for exactly as long as a call could only resolve
to a scalar — the fold's own results. The moment a projection's array became
visible there, an array travelled in the scalar carrier, and the acceptance
relation, asked whether a "scalar" inhabits `non-empty-list<string>`, correctly
said no.

The fp-gate caught it: six false positives in `guzzle/guzzle`, all one call site,
all `getLastHeaderBlock(array_values($headers))` under
`@param non-empty-list<string>` — a contract the argument plainly satisfies. The
same value written literally, and the same value through a binding, were both
silent; only the inline spelling was convicted. The fix routes the resolved value
back through `resolve_cval`, exactly as the variable arm already sent its
singleton back, so one value gets one verdict however it was produced. Pinned
both ways: five spellings of a satisfied contract stay silent, and a genuine
violation reaching the seam inline still fires.

### E2. Measurement

nsrt unmoved (headline 2433, admissible 2858, an empty transition matrix) — the
harness asserts *types*, and this slice moves what a value can be *used for*, not
what it is. fp-gate GREEN with no baseline movement: proof layer 0, and guzzle
back to its expected zero after E1.

## Amendment F (2026-08-14): the witnessed family, wave 2 — PENDING ratification

Issue [#328](https://github.com/rigortype/steins/issues/328), second wave.
Amendment D took the four restructuring projections. The same admission test —
*restructures the argument, reads no value semantics beyond key normalization* —
admits more names than that, and they were still answering the key-set widening.

### F1. The position readers

`array_key_first` / `array_key_last` / `array_first` / `array_last` are exact on
a witnessed order. §4 answers them from the key set — "SOME key of the set,
never the declared-first one", which is §2's rule at its sharpest and correct
for a declaration that admits every permutation. A witnessed order is the other
provenance: the sequence was observed, so first really is first. Probed at
8.5.9: `array_key_first(['b' => 1, 'a' => 2]) === 'b'`, `array_last(…) === 2`,
and all four answer `null` on `[]`.

The key readers answer whatever the values are — the result's *value* is the
subject's *key* — while the value readers hand back the slot's own fact and
decline on an unknown slot.

**The pointer family is deliberately excluded.** `key` / `current` / `reset` /
`end` read the internal array pointer, which Steins does not model. The existing
arm tolerates that only because a shape-derived fact can never premise a
proof-layer finding (A-G9's corollary); a witnessed literal is `Verified`, so an
exact answer here *would* be admissible as a premise and the pointer assumption
would ride into a proof with it. They keep the widening.

### F2. `array_slice` on a witnessed shape

The exact slice existed for a fully-proven `Val::Array` (Amendment B). It reads
offsets and keys and never a value, so nothing about it needed the values known:
`array_slice(['x', $s, 'z'], 1)` is `list{string, 'z'}`.

### F3. Values as keys: `array_fill_keys` and `array_combine`

Both take their result's keys from a value, through a cast that had to be
measured. Three neighbouring builtins turn out to have three different rules for
the value `1.5`:

| seam | answer |
| --- | --- |
| `$a[1.5] = v` | int `1` — truncation, with a deprecation |
| `array_fill_keys([1.5], v)` / `array_combine([1.5], [v])` | string `'1.5'` |
| `array_flip([1.5])` | the entry is **skipped** |

No amount of reasoning about "PHP's array key cast" produces that table; only
running the engine does, which is ADR-0004's whole point. The float **declines**
even so: PHP renders a float to string under the `precision` ini directive, so
the *key* of `array_fill_keys([0.1 + 0.2], v)` depends on the runtime's
configuration — the same reason `concat_cast` excludes floats.

`array_combine` additionally declines a length mismatch, because PHP raises a
`ValueError` there (probed): a call that does not return has no return value to
state.

### F4. Key-set work: `array_diff_key` and `array_intersect_key`

Pure key operations, and the order comes from the *first* array (probed:
`array_intersect_key(['b' => 2, 'a' => 1], ['a' => 9, 'b' => 8])
=== ['b' => 2, 'a' => 1]`). Key identity is the domain's own normalized `VKey`,
which is what makes `array_diff_key([5 => 1, '5x' => 2], ['5' => 9])
=== ['5x' => 2]` fall out — `'5'` and `5` are one key.

**Their second argument may be a declared shape**, and this is not a crack in
§7. What §7 declines is reading a declaration's key *order*; these two read its
key *set*, and a set has no order. What the set must be is **certain**, so the
reader refuses an optional field (a key that may or may not be there decides
neither the difference nor the intersection) and an unsealed tail (the set would
be a lower bound). `array_combine`, which zips *positionally*, still requires a
witnessed order on both sides — the same argument, the other way round.

### F5. Measurement, including two rows that left `match`

nsrt headline 2433 → **2444**, admissible 2858 → **2872**. The transition matrix
is 13 `differ → match`, 1 `differ → subsumed`, and — for the first time in this
line of work — **2 `match → subsumed`**. Those two are worth naming rather than
netting out.

Both are `tests/PHPStan/Analyser/nsrt/array_first_last.php`, which asserts

```php
assertType("'a'|'b'|'c'", array_first([1 => 'a', 0 => 'b', 2 => 'c']));
assertType("'a'|'b'|'c'", array_last([1 => 'a', 0 => 'b', 2 => 'c']));
```

Steins now answers `'a'` and `'c'`. The engine agrees: probed at 8.5.9, that
literal's entries are in insertion order `1 => 'a'`, `0 => 'b'`, `2 => 'c'`, so
first is `'a'` and last is `'c'`. The upstream expectation is an upper bound —
`subsumed` is the harness saying our answer lies inside it — and the reference
implementation does not consume constant-array order for this family. So the
headline dropping two while the truth improved is the harness measuring
*agreement*, not correctness, and this is a case where the two part company.

fp-gate GREEN with **no baseline movement**: proof layer 0, `phpdoc.*` and
`throw.*` unmoved.

## Amendment G (2026-08-14): the array-key cast at the type level, and the wall it hits — PENDING ratification

Owner request: support `array_flip` / `array_fill_keys` / `array_key_first` and
the `foreach` key position when what is known about the value is a *refined
string class* rather than a literal — the cases the reference implementation
does not cover either.

### G1. The cast is expressible, and the vocabulary was already there

PHP casts an array key eagerly, and the interesting half is the string one: a
string that spells an integer *the way PHP writes one back* becomes that
integer, and every other string keeps its identity. Those two classes are
exactly `StrPreds::DECIMAL_INT` and `StrPreds::NON_DECIMAL_INT`, which the
conformance work landed for precisely this reason — the `DECIMAL_INT` doc
already said "an array key made of it is cast to `int`".

`Fact::array_key_cast` is that grid, every row probed at 8.5.9:

| input | key |
| --- | --- |
| `int` (with its refinement) | itself |
| `bool` | `int` |
| `decimal-int-string` | `int` |
| `non-decimal-int-string` | itself, predicates and all |
| `numeric-string` | `int \| numeric-string&non-decimal-int-string` |
| `string` | `int \| non-decimal-int-string` |
| `float` | declines — the key is a `precision` ini setting away, and the seams disagree (`$a[1.5]` is `1`, `array_fill_keys([1.5], v)` is `'1.5'`) |

The last two rows are **sharper than `array-key`**: a string that survives the
cast is by definition one PHP does not rewrite, and that is a predicate this
domain carries.

### G2. The wall: a `Fact` carries one `Base`

Both sharp rows are **two-base unions**, and the four-layer domain (ADR-0035)
has no such form — the same wall `int|false` returns and `json_decode`'s
envelope already hit. So `array_key_cast` *declines* them rather than widening,
so that a caller can tell "no answer" from "the answer is `array-key`", and the
sharp forms are written down at the decline site for whichever lane grows to
hold them.

The unsealed tail's key slot is a second, smaller wall: `KeyClass` has three
values (`int`, `string`, `array-key`), so even `non-decimal-int-string` — which
*is* a single-base fact — lowers to `string` there.

### G3. What lands under those walls

`array_flip`'s key class is now read off the cast rather than off the values'
base, which is what lets a *string*-based value produce an *integer* key:

| subject | before | after | the sharp answer the walls hide |
| --- | --- | --- | --- |
| `list<decimal-int-string>` | `array<int>` | **`array<int, int>`** | `array<int, int<0, max>>` |
| `list<non-decimal-int-string>` | `array<int>` | **`array<string, int>`** | `array<non-decimal-int-string, int<0, max>>` |
| `list<string>` / `list<numeric-string>` / `list<array-key>` | `array<int>` | unchanged | `array<int\|non-decimal-int-string, …>` &c. |
| `[$decimalIntString, …]` (witnessed) | `array<0\|1>` | **`array<int, 0\|1>`** | `non-empty-array<int, 0\|1>` |

A finite value set still casts key by key, which is exact where the abstract
rung declines.

### G4. What is still out of reach, and why — the three open pieces

1. **The two-base union**, above. Every remaining row of the owner's spec needs
   it. Reachable either by giving `Fact` a union layer (ADR-0035 is explicitly
   single-base, so this is a domain-shape decision) or by moving the key slot to
   the **arm lane**, which already carries `string|false`. Not decided here.
2. **`array_fill_keys` has no shape-level arm at all**, so it declines on a
   declared subject rather than answering the key class. Additive once the key
   slot's expressiveness is settled.
3. **A non-literal array key is unrepresentable in the IR.** `[$k => $v]` lowers
   the *whole* literal to `Other`, so `array_key_first([$string => null])` and
   every `foreach ([$k => $v] as …)` row of the spec has no fact to work from.
   `ArrayKey` would need a non-literal form, and `normalize_array`'s next-int
   would have to go unknown behind it (an unknown key may be an integer, which
   moves the auto-index). Self-contained, and the largest of the three.

### G5. Measurement

nsrt unmoved (headline 2444, admissible 2872, empty transition matrix) — the
harness's array fixtures do not exercise the refined-string key classes, which
is itself a finding about coverage rather than about this change. fp-gate GREEN
with no baseline movement; proof layer 0. Tests 4471 → 4477.

## Amendment H (2026-09-01): `isset` decides in value position and nothing in guard position — PENDING ratification

Issue [#579](https://github.com/rigortype/steins/issues/579), the value-position
twin of [#414](https://github.com/rigortype/steins/issues/414). S4 gave the
`isset` **guard** a representation and A-G9's corollary gave it a stratum
discipline; the value side had neither, because `isset` is a construct rather
than a call and its value lowering was `ArgValue::Other`. Nothing declined it —
nothing was asked. Measured on master before the change, every value-position
`isset` answered `unknown`, weaker than the `bool` PHP guarantees, while
`array_key_exists` beside it answered `true` on the rung Amendment-era work
built.

The rule is [#343](https://github.com/rigortype/steins/issues/343)'s with one
conjunct added, and it needs no new machinery: `array_key_exists` asks for
presence, `isset` asks for presence **and** a provably non-null value, which
`Fact::is_null` already answers as a `Certainty`. `shape_read`'s four outcomes
map straight onto it — `Present(Some(v))` conjoins with `¬is_null(v)`,
`Present(None)` is undecided (A-G1a's honest floor says nothing about `null`),
`DeclaredAbsent` is `false`, and `MaybeMissing`/`Tail` are undecided on presence
alone, so the null question is never reached. A required field whose value is
provably `null` therefore answers `false` where `array_key_exists` answers
`true`, which is a row that table has no place for and this one produces
without a special case.

### H1. The two lanes answer differently, and that is the design

`eval_cond` still answers `Maybe` for `CondExpr::Isset` and
`CondExpr::IssetVar`. The line it draws is the one S4 wrote down: the only
evidence that could decide an `isset` guard is a shape fact, which is `Asserted`,
and deciding **reachability** from a docblock would let an author's claim silence
the env-free pass on a live path. Narrowing is that guard's whole payoff.

A *value* is not a reachability claim. It is a fact like any other, and it
carries the stratum of what it was derived from — so the same `Asserted` shape
that must not prune a branch may perfectly well say what an expression evaluates
to, because A-G9's corollary then keeps that value out of every proof-layer
premise. Two lanes, two answers, one reason. Worth recording precisely because
the pair looks inconsistent until the premise is named.

### H2. Totality, and why the floor is not silence

The carrier is total: `ArgValue::Isset` holds every operand, with
`IssetOperand::Unmodelled` for the shapes the vocabulary does not spell, and the
expression never widens back to `Other`. That is not conservatism, it is the
correction — `isset` evaluates to a `bool` whatever it tests, so `unknown` was
never the safe side of anything. Multi-argument `isset` is PHP's own conjunction
and folds through `Certainty::and`, so one operand proving `false` decides the
whole expression past unmodelled siblings.

The proven-whole-array leg runs before the abstract one and is exact rather than
conservative, which is what makes the table hold over a witnessed literal and not
only over a declared shape. It exists because a *fully* literal array binds a
`Fact::Singleton` of the value while one with an unresolved element binds a
`Fact::Shape` (the Amendment C ladder), and the offset resolver reads only the
latter — so without it `$z = ['k' => 1]; isset($z['k'])` answered `bool` while
the same expression over `@param array{k: int}` answered `true`.

### H3. What is deferred, and why each is deferred rather than missing

1. **A never-bound variable.** PHP answers `false`; the seam answers `bool`. The
   definedness lanes (`Scope::undefined_reads` / `maybe_undefined_reads`) are
   computed at lowering with `isset` operands **excluded by construction** —
   that exclusion is exactly what keeps the guard from reporting — so there is no
   witness at the value seam to read. Reaching it means giving the lowering a
   second, guard-blind read set, which is its own slice.
2. **`isset($var)` answering `true`.** Decided, but only from a `Verified` fact.
   ADR-0087 §4 states that `@var T|unset $x` means reads of `$x` may find no
   binding, and `ContractTy::is_unset` is filtered out of the arm list before it
   reaches the store — so a `T|unset` declaration and a plain `T` one leave the
   value lane identical. The stratum is the available discriminator: `Asserted`
   is an author's claim about a value, `Verified` is the walk's own record of a
   binding form. The `false` leg needs no such gate, because bound-and-null and
   never-bound both make `isset` false.
3. **A property or static-property operand**, whose binding question is the
   declared-but-uninitialized one the heap does not answer, and **a path deeper
   than one offset**, which A-G4's depth-one projection does not reach. Both
   answer the `bool` floor.
4. **A variable holding an object.** `$o = new C(); isset($o)` is `true` in PHP
   and answers `bool` here: an object binding lives in the heap store's
   reference table, not as an env `Fact`, and the store's binding record does not
   on its own separate a proven allocation from a declared — possibly nullable —
   receiver. Deciding it wants that distinction first.
5. **`empty(…)`.** `lower_cond` models it as `!isset(e) || !e`; the second
   disjunct is a truthiness reading of the operand's value, a question this
   carrier does not carry. Its own slice, with its own measurement.

### H4. Measurement

nsrt: 96 rows moved, every one an `isset` row or a variable bound from one, in
exactly two buckets — 52 `differ → match` (42 `bool`, 7 `true`, 3 `false`) and 44
`differ → differ`, all `unknown → bool`, a precision gain that does not reach the
asserted verdict. Headline `match` 2892 → 2944, `differ` 9984 → 9932; `equal`,
`subsumed`, `unsupported` and `skipped` unmoved, and no row regressed in any
direction. The 44 short rows are the deferrals above plus subjects whose own
fact the walk does not carry — an array-union receiver, a two-array-arm declared
return the arm lane keeps out of the value lane (A-G3), and a literal
invalidated by an intervening call. `SCHEMA_VERSION` 6 → 7, since `ArgValue` is
persisted trace IR (ADR-0092 §2) and a schema-6 trace spells the construct
`Other`.

## Amendment I (2026-09-02): a transfer rung may CONSTRUCT an abstract array — PENDING ratification

Issue #615 leg (a). Every producer of a `Fact::Shape` so far has been a *reader*:
a literal the walk saw built, a `@param array{…}` the arm lane seeded, a
projection over one of those. `filter_var($x, $filter, FILTER_FORCE_ARRAY)`
is the first rung to **mint** one from a scalar it computed — the answer is an
array the source never wrote and no declaration named.

### I1. The carrier was already there, and A-G1 is why

No new spelling was needed, and that is the amendment's whole point. A-G1 says
the degenerate shape ([`ShapeFact::plain_array`]) *is* plain `array`, with no
array-`General` variant beside it; put the element fact on that shape's tail and
the result **is** the abstract `array<T>`, spelling through the same
`spell_generic_array` a read-side shape uses. So a constructed answer and a read
answer are the same object, and the dump surface cannot tell them apart — which
is the property that makes minting safe rather than a second lane.

The rung's own scalar outcome is the element fact unchanged. The wrapping never
fails (`FORCE_ARRAY` always produces an array), so the constructed shape carries
no outer failure arm, and `REQUIRE_ARRAY`'s own failure is a plain `Singleton`,
not an array at all.

### I2. The bound: a slot that may be an array

The rule is confined to a **proven non-array input**, and the reason is
measured, not conservative. `filter_var` under either array flag does not map
its scalar filter over the input's slots — it walks the input *recursively*, and
a slot that is itself an array stays an array. At PINNED_PHP 8.5.9:

```text
filter_var([[1]],              FILTER_VALIDATE_INT, ['flags' => FORCE_ARRAY])   === [0 => [0 => 1]]
filter_var(['a'=>['b'=>'z']],  FILTER_VALIDATE_INT, ['flags' => REQUIRE_ARRAY]) === ['a' => ['b' => false]]
```

So over an input whose slots may be arrays — `mixed`, or an `array<string,
mixed>` map — the true element fact is `int|false|array<…>` at unbounded depth.
`Fact::Union`'s arms are scalar bases by construction (§3, and the union
declines an array arm), so no `Fact` spells it and the rung declines. The
reference implementation asserts a flat `array<string, int|false>` for exactly
that input and is unsound there; those rows stay `unknown`, the #40/#594
precedent applied once more.

This generalizes past `filter_var`: **any** rung minting an `array<T>` owes a
premise that `T` is expressible for every slot the input admits, and the
premise is about the input's *element* domain, not its own layer. A shape whose
slots are themselves proven non-array would map soundly; none is spelled by a
fixture, so the implementation asks the simpler question and says so.

The trap worth writing down, because it has now bitten three slices (#597's
self-review, #579's offset resolver, and this one at design time): a
fully-literal array binds `Fact::Singleton(Val::Array(…))`, **not**
`Fact::Shape`. A premise about array-ness that matches only `Shape` silently
takes the non-array branch for the most concrete input there is.
`fact_denotes_no_array` asks the values, and both spellings are pinned by test.

### I3. Not taken

Over a proven non-array input `FORCE_ARRAY` yields exactly one slot at key `0`,
so `list{outcome}` would be sound and strictly sharper. That is a claim about
the result's **cardinality**, separable from the element-type claim this rung
makes, and it is recorded rather than made — a rung that mints an array should
mint the weakest one its evidence supports until a caller needs more.

### I4. Measurement

Leg (a) alone: 15 rows moved, 5 `differ → match` and 10 `differ → subsumed`, all
in `filter-var.php` / `filterVar.php`; nsrt unknown-fall 6510 → 6495. No row
regressed and nothing outside the two fixtures moved. Legs (a)+(b) together:
6510 → 6407.

## Amendment J (2026-09-03): a write at a key nobody can name — PENDING ratification

Issue #636, leg B. §4's write row assumed the key was *nameable*: "field update
to `Required` with `v`'s fact". `$a[$i] = v` names nothing, and until now it did
not reach this table at all — `const_key_offset_path` demanded a concrete key,
so the statement lowered to `StmtKind::Barrier` and the environment went with
it. The row below is what §4 says when the key is unknown.

### J1. The row

| base | `$a[$i] = v` yields |
| --- | --- |
| any `Fact::Shape`, or a `Singleton(Val::Array)` lifted to one | every same-class key's presence kept and its slot **joined** with `v`'s fact; a proven-`Absent` same-class key back to `Optional`; the tail unsealed to the index's key class carrying `v`; `non_empty` set; `is_list` re-derived; the order witness dropped; covers kept; the count floor kept and its ceiling raised by one |
| anything else | decline — the barrier stands |

The operator is `ShapeFact::write_at_unknown_key`, and its law is stated
denotationally rather than component by component: for every array the receiver
admits, every key of the given class and every value, the array with that key
set to that value is still admitted. A test sweeps that law over the shape,
array and key universes §4's other operators are checked on.

Two components deserve their reason in prose.

**`is_list` does not survive, whatever the base was.** The issue proposed
`list<T>` → `non-empty-list<T|V>`, on the argument that a write at an integer
index of a list either overwrites or appends. It does neither when the index is
out of range, and out of range is exactly what an unnamed index cannot be ruled
out of:

```
$ php -r '$a=[1,2,3]; $i=7; $a[$i]=99; var_dump(array_is_list($a));'
bool(false)
```

So the row surrenders list-ness to `normalize`'s denotational recompute. This is
§7's rule reaching a new case: an order claim survives only what cannot disturb
the order, and a write at an unknown index can disturb it.

**Only `KeyClass::Int` is derivable from the index's own fact.** A `string`
index is *not* `KeyClass::Str`, because PHP normalizes a decimal-integer string
key to an integer key:

```
$ php -r '$a=[]; $a["5"]=1; var_dump(array_keys($a));'
array(1) { [0]=> int(5) }
```

A `string`-typed index can therefore land in either class, and answers
`ArrayKey`. `bool` and `float` indices key by integer too, through a conversion;
they are left at `ArrayKey` because answering them precisely buys nothing the
corpus asks for.

### J2. What this row is NOT

- **`unset($a[$i])` is not in it.** `mark_absent` at a key nobody can name could
  only weaken every presence, never remove one, and a rule whose whole content
  is "forget slightly less than the barrier" is worth its own measurement rather
  than a free ride on this one. `unset` lowers through `const_key_offset`, which
  is untouched, so it keeps the barrier it always had.
- **The index expression is never folded.** `$a[$i + 1] = v` takes this row, not
  a sharper one computed from `$i`. Width-sensitive integer arithmetic stays out
  under ADR-0028 §3.
- **A depth-two path still needs a literal inner key.** `$a['k'][$i] = v` lowers
  (the outer key is unnamed, and the nested arm already clears `'k'`'s slot to
  unknown, which is exactly right); `$a[$i]['k'] = v` does not, because the
  inner key comes from `const_key_offset`, which A-G4 keeps literal.
- **Nothing about aliasing changes.** The barrier still runs first and clears
  the whole environment and store; this row only decides what is put back.

### J3. Measurement

PHP 8.5.9, over the 15,643-observation nsrt run. 46 rows changed their answer,
all of them out of `unknown`: 4 changed verdict (`differ` → `match` at
`offset-value-after-assign.php:21`, `differ` → `equal` at `bug-14245.php:107`,
`bug-14245.php:137` and `unsealed-array-shapes.php:119`), and 42 more moved from
`unknown` to a shape that is still short of the assertion — overwhelmingly
because the assertion wants `non-empty-list<int>` and this row can only prove
`non-empty-array<int>`. Headline `differ` 9779 → 9775, `match` 3193 → 3194. No
row regressed.

Of the 101 computed-offset rows the issue counted, 45 moved and 56 did not. **43
of the 56 are loop-carried** — the write is inside a `for`/`foreach` whose own
fixpoint drops the binding before this row is ever consulted (`bug-12274.php`,
`for-loop-expr.php`). That is §4's out-of-scope loop-carried paragraph, not this
row, and it is the reason the leg's realized yield is 45 rather than 101.

`steins check --profile strict --no-cache` over the pinned corpus produced 1,810
lines byte-identical before and after: this row adds facts, and no finding moved
on them.

## Amendment K (2026-09-03): the auto-index append, and the row §4 got wrong — PENDING ratification

Issue #636, leg A. `$a[] = v` is the most common array write PHP has, and it
was the one form that never lowered: `const_key_offset_path` matches an
`ArrayAccess`, an append has no index node at all, and so the statement fell to
`StmtKind::Barrier`. It lowers to `StmtKind::OffsetAppend` now, which is why
`SCHEMA_VERSION` moves 10 → 11.

### K1. One rule, two spellings

`$a[] = v` and `array_push($a, v)` are the same operation, so the walk calls
`array_push_written_fact` rather than growing a second rule. Where an appended
value lands is decided in one place, and the two spellings cannot drift.

The landing index is `max(integer keys) + 1`, `0` when the array has no integer
key, and it counts negative keys since PHP 8.3 — the table Amendment §4 of
ADR-0077 already measured. This is index bookkeeping over the shape's own key
sequence, not folded arithmetic on an operand: ADR-0028 §3's ban stands, and
`$a[$i + 1] = v` still takes Amendment J's weak row.

A **nullable** base declines. `php -r '$a=null; $a[]="x"; var_export($a);'`
autovivifies to `[0 => 'x']`, an outcome the array arm alone does not describe.

### K2. §4's append row was wrong, and the correction is the amendment's core

§4 said: *"a Yes-list shape stays Yes (append preserves list-ness)"*. PHP
refutes it directly:

```
$ php -r '$x=[1,2,3]; unset($x[2]); var_dump(array_is_list($x));'
bool(true)
$ php -r '$x=[1,2,3]; unset($x[2]); $x[]=99; var_dump(array_keys($x)); var_dump(array_is_list($x));'
array(3) { [0]=> int(0) [1]=> int(1) [2]=> int(3) }
bool(false)
```

A value `array_is_list` calls a list can stop being one on its very next append.
The reason is that list-ness is a property of the key **set**, while the append
index is PHP's `nNextFreeElement` — a **high-water mark** that records the
largest integer key the array has ever held, and that `unset` does not lower:

```
$ php -r '$a=[]; $a[5]=1; unset($a[5]); $a[]=2; var_dump(array_keys($a));'
array(1) { [0]=> int(6) }        an empty array whose next index is 6
```

No `list<T>`, `array_is_list()` verdict, or key set constrains that counter. So
the only shape that may name an append's index is one whose **exact key
sequence is witnessed** — and even there `is_list` is re-derived by `normalize`
from the new sequence rather than carried.

`unset` is the one operation that moves the counter off `max + 1`. Every other
producer of a witnessed sequence rebuilds the array by insertion and resets the
counter with PHP, measured at 8.5.9: `array_pop([1,2,3])` then append lands on
`2`, `array_shift`, `array_splice` and `array_filter` likewise. So
`apply_offset_write` **drops the order witness on `unset`**, and that single
fence upgrades the witness's meaning from "this was the build order" to "this
was the build order and nothing has been removed since" — which is exactly the
premise `max + 1` needs.

### K3. Two live claims this corrected

Both were reachable before this slice, through `array_push`:

- `array_push_written_fact` derived its key from `determined_order`, whose
  second leg — a proven list under a sealed tail — is a sound claim about
  *order* (§7's sanctioned second source) and an unsound premise for the *next
  index*. It reads `append_order` now, which takes the witness alone.
- `general_append` carried `shape.is_list` through for both its callers.
  `array_unshift` renumbers every integer key from `0`, so it rebuilds and a
  list input really does come back a list (`unset($x[2])` then
  `array_unshift($x, 99)` is `array_is_list` true). `array_push` and `$a[] = v`
  do not. The parameter is the caller's now, and the two answer differently.

### K4. What still declines

- `$o->p[] = v` — ADR-0063 §2.3's aliasing family, not this lane.
- `$a['k'][] = v` — a nested-shape update, which A-G8 declines for `$a['k']['j']
  = v` on the same grounds.
- `$a[] = v` where the base is not an array fact at all; the barrier stands.

### K5. Measurement

PHP 8.5.9, against the Amendment J base (`differ` 9775 / `match` 3194).

| | differ | match | equal | subsumed |
| --- | ---: | ---: | ---: | ---: |
| Amendment J base | 9775 | 3194 | 188 | 422 |
| this amendment | **9749** | **3206** | **190** | **434** |

26 rows left `differ` — 12 to `match`, 12 to `subsumed`, 2 to `equal` — and 82
rows changed their answer in total. **No row regressed**, including the
`array_push` rows K3's two corrections touch: `bug-13510.php:22` stays `match`
because `array_unshift` keeps its list claim.

The named witnesses: `list-type.php:88` and `:90` answer `list{'1'}` and
`list{'1', '2'}` where they answered `unknown`, and `:92` — after
`unset($list[0])` — answers `array{1: '2'}`, the fence visible in the corpus.
`array-is-list-unset.php` moves seven rows, four of them to `match`. `bug-9985.php:20`, the string-key
sibling this must not regress, is byte-identical.

Across both legs of #636: `differ` 9779 → 9749, `match` 3193 → 3206.

`steins check --profile strict --no-cache` over the pinned corpus produced 1,810
lines byte-identical before and after.
