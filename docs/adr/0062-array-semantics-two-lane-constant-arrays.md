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

**Lean obligation (ADR-0059).** `Fact::Shape` enters `steins-domain` only
together with its spec extension: `join_sound` over the field-wise join
(required∧required stays required, else optional; values join arm-wise;
`is_list` joins by Certainty; sealed only when both sides seal the same key
universe), `summarize_admits` for the OneOf descent, and agreement between
Rust `admits_shape` and the Lean acceptance on the differential vectors.

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
| `array_values` / `array_keys` / `array_slice` / `array_reverse` | execute on the witnessed order (sound) | **sound widening only**: value/key unions with list-ness and size bounds from §3 — never declaration-order `list{k1, k2}` (the declined import, §2) |
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

## 7. Declined imports (divergence registry)

1. Declared-order trust in positional projections (§2) — replaced by the
   provenance split.
2. Abstract `nextAutoIndexes` (§3) — concrete-only via A12.
3. `ARRAY_COUNT_LIMIT = 256` union degradation — replaced by OneOf-cap
   computed descent.
4. Loop list-ness rescue heuristics (§4) — not imported; loop-schema
   inference is future, separate work.
5. (Standing, ADR-0030) benevolent unions; the accepts/isSuperTypeOf
   asymmetry.

## 8. Slices

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
2. **A6 timing.** Arms-first (A1–A5 need no domain change) keeps the
   proved core untouched until the consumers exist. The alternative —
   domain-first — front-loads the Lean work. Default: arms-first.
3. **D4 spelling immediately vs behind the strict surface.** Default:
   immediately — the dump surface is debug-grade and BC-free here.
