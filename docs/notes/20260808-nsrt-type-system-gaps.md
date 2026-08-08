# The type-system gap inventory, measured through nsrt

The [rule-port map](20260808-phpstan-rule-port-map.md) answered *which findings*
a PHPStan user would miss. It did not answer the question underneath: **where
does the type system itself fall short of PHPStan's**, and which of those gaps
are worth closing.

This note answers that from the instrument built for it — `cargo xtask nsrt`,
which runs phpstan-src's own `assertType` corpus as an inference oracle — rather
than from reading either codebase. Measured against master `7e78c3e` and
phpstan-src `55a7732`: 1,602 fixture files, **15,845 assertions**.

## Where the run stands

| verdict | n | |
| --- | ---: | --- |
| `match` | 1,621 | semantically equal after normalization |
| `equal` | 107 | proven-equal, differently spelled |
| `subsumed` | 220 | Steins strictly more precise |
| **admissible** | **1,948** | **12.3%** |
| `differ` | 11,103 | |
| `unsupported` | 2,486 | the oracle's spelling uses vocabulary Steins does not model |
| `skipped` | 308 | |

> **Correction (2026-08-08, issue #239):** the `mixed` row of the A table below
> was right that this is not an engine gap and wrong about where the rows
> belong. Follow-up 5 has since been taken: the harness no longer gates `mixed`,
> so its **329 rows moved out of `unsupported` and into `differ`** — 324 of them
> as reach (Steins renders `unknown`), five as precision. The totals in the table
> above therefore read, on the same engine, `unsupported` **2,157** and `differ`
> **11,432**. `match` (1,621), `equal` (107), `subsumed` (220) and **admissible
> (1,948)** are all **unchanged**: the movement is reclassification, not
> behaviour. No engine code changed and no release note was warranted.
>
> The five non-`unknown` rows are worth naming, because they are why an expected
> `mixed` now earns neither `equal` nor `subsumed`. `mixed` is the top type, so
> the acceptance relation answers the covering direction `Yes` for *every*
> parseable rendering — the verdict would report the oracle's silence, not a type
> relation. Measured, that is what it does: three of the four rows the relation
> would have booked as precision (`unresolvable-types.php:17,18`,
> `invalid-type-aliases.php:13`) are Steins rendering an **unresolvable phpdoc
> type** (`array<int, int, int>`, `iterable<int, int, int>`, `what{foo: 'bar'}`)
> as a class name — which is exactly why PHPStan says `mixed` there — and the
> fourth (`bug-14333.php:167`, `$c = [&$b]; foo($c);`) is a **missed by-ref
> invalidation** that the pre-existing int/float veto already held out. Exactly
> one row (`bug-13282.php:40`) was a genuine precision claim, and losing it is
> the acknowledged cost. The argument and the numbers live in `xtask/src/nsrt.rs`'s
> module docs.

The gaps split three ways, and the split is what the plan follows.

## A. Vocabulary — 2,486 rows

The oracle spells something Steins has no way to say. **Two thirds of the
headline number is not work**, which is the first thing measurement bought:

| class | n | reading |
| --- | ---: | --- |
| `phpstan-special` | 669 | `*ERROR*` (391) + `*NEVER*` (272) — PHPStan's own internal markers. Not vocabulary; nothing to port. |
| `intersection` | 519 | see below — this bucket is two unrelated problems |
| ~~`mixed`~~ | ~~329~~ | **not an engine gap** — and, as of issue #239, no longer here: the rows are measured and sit in `differ` (see the correction above). Leaving the bucket at **2,157**. |
| `generic-other` | 225 | non-array generics (`DOMNamedNodeMap<DOMAttr>`) — the ADR-0032 carry root, already tracked as issue #10 |
| `subtraction` | 157 | 133 of them are `mixed~…` — **not vocabulary work either** (#237): 154 of the rows render `unknown`, so see the correction below |
| `class-string` | 148 | |
| `compound` | 99 | `T of X (class Y, argument)` — template-parameter rendering |
| `object` / `callable` / `other-keyword` / `shape-other` | 212 | |

### The intersection bucket is two problems, not one

Splitting the 519 by whether every arm is an accessory/scalar refinement:

- **336 accessory intersections** — `lowercase-string&non-falsy-string`,
  `numeric-string&uppercase-string`, `lowercase-string&non-falsy-string&uppercase-string`.
  Steins spells each arm *separately* today; what is missing is their
  **conjunction**. This is the divergence the registry already names — PHPStan
  composes accessory types as intersections, Steins carries refinements in the
  value domain's predicate bitset — so the question is whether the *speller*
  and the acceptance relation can express a conjunction the domain may already
  hold. Possibly cheap; possibly a genuine domain gap. Unknown until probed.
- **183 object intersections** — `ArrayAccess&stdClass`, and PHPStan's
  `hasOffset('foo')` / `hasOffsetValue('foo', 17)` accessory predicates on
  arrays. The `FinalClass&MockObject` shape issue #234 guards is in this half,
  and it is the smaller half.

> **Corrected 2026-08-08 (#235):** the 336/183 split does not reproduce. The
> measured halves are **273 / 246**, and the probe below states the filter that
> produces them. The rest of this section's *reading* survives the correction;
> its *conclusion* does not — see the probe's verdict.

### Subtraction is a `mixed`-cut problem

133 of the 157 rows are `mixed~null`, `mixed~int`, `mixed~array<mixed, mixed>`,
`mixed~(0|0.0|''|'0'|array{}|false|null)`. Steins models exactly two cuts —
`MixedCut::Null` and `MixedCut::Falsy`, spelled `non-null-mixed` /
`non-empty-mixed` — against PHPStan's arbitrary type subtraction. So this is
not "subtraction is missing" (ADR-0052 subtraction exists); it is that the cut
vocabulary is a two-value enum where the oracle has a type algebra.

> **Correction (2026-08-08, issue #237): this section names the wrong gap, and
> the bucket is not work.** Read against what Steins renders — the reading this
> section skipped — the 158 rows (133 `mixed~…`; 158 rather than 157 only
> because the corpus checkout moved) say something else:
>
> - **44 of them are exact re-spellings of the cuts Steins already holds.**
>   `mixed~null` (33) *is* `MixedCut::Null`;
>   `mixed~(0|0.0|''|'0'|array{}|false|null)` (11) *is* `MixedCut::Falsy`, value
>   for value. Another 8 (`mixed~(array|object|resource)`) have a complement
>   Steins spells as the plain union `bool|float|int|string|null` — the domain
>   is object-free (ADR-0035) and has no `resource` (registry entry 4), so the
>   subtrahend removes exactly what Steins does not carry anyway. For a third of
>   the bucket "the vocabulary is a two-value enum" is a **spelling** claim, not
>   a representation one.
> - **154 of the 158 render `unknown`** — including all 52 of the above. These
>   are §B reach rows sitting in an §A bucket. Closing the spelling (the #239
>   move) would reclassify them into `differ` and award nothing: `unknown` is a
>   sentinel and the relation is not asked of a sentinel.
> - **The four rows that render a type cap the slice at +1 admissible.** Three
>   are class/enum subtractions (`Throwable~LogicException`,
>   `Bug7176Types\Suit~…::Clubs`, `AllowedSubtypesEnum\Foo~…::B`) where Steins
>   renders the **un-narrowed base** — wider than the oracle, so `differ` under
>   any cut vocabulary. The fourth (`bug-8249.php:19`, expected `mixed~int`,
>   Steins `null`) would earn `subsumed`, and it earns it from body-return
>   inference on `function foo(): mixed { return null; }`, not from subtraction.
>
> So the ladder in issue #237 was climbed and the answer is its third rung:
> **stop here**, recorded as ADR-0030 divergence-registry entry 6. The decisive
> structural fact is that `ContractTy::MixedMinus` is constructed in exactly one
> place — `lower_str`, from the two literal keywords — so the cut vocabulary is
> *declaration-side*: no enum extension can change a `got`, and no nsrt row can
> move until the arm lane narrows `mixed` at all. That reach work
> (`isset($arr[$mixed])` in `bug-11716.php`, `!is_array($m)` in
> `mixed-subtract.php`, the isset/coalesce cluster behind the 33 `mixed~null`
> rows) is priced in §B and is independent of any `~` spelling. The harness is
> unchanged, so the totals above and the `subtraction` count both stand; two
> tests in `xtask/src/nsrt.rs` pin the finding, and the module docs carry the
> argument.
>
> Also settled here, since #239 made it askable: the **top-type veto does not
> touch these rows**. `classify` consults `unsupported_pattern` first, so the
> veto is never reached, and `normalize` keeps `mixed~null` as its own atom, so
> `expected_is_top_type` answers `false` even when asked directly.

## B. Reach — 7,843 rows (70% of `differ`)

The vocabulary exists and nothing computes the value: Steins renders `unknown`
where PHPStan asserts a concrete type. Ranked by fixture:

> **Correction (2026-08-08, issue #239):** **8,167**, not 7,843 — the 324
> reclassified `mixed` rows are reach rows and belong in this section. They are
> spread across **112** fixtures — the largest single contribution is 39 to
> `filterVar.php` — so the ranking below keeps its order.

| n | fixture | area |
| ---: | --- | --- |
| 369 | `loose-comparisons.php` | `==` / `!=` narrowing |
| 276 + 117 | `filterVar.php`, `filter-var.php` | `filter_var` return shapes |
| 251 | `binary.php` | binary-operator result types |
| 189 | `bcmath-number.php` | ext-bcmath returns |
| 182 | `integer-range-types.php` | int-range arithmetic |
| 131 | `set-type-type-specifying.php` | `settype()` |
| 104 | `isset-coalesce-empty-type.php` | isset/`??` narrowing |
| 90 | `preg_match_shapes.php` | the preg slices' remainder |
| 88 | `pow.php` | |

## C. Precision — 3,260 rows (30% of `differ`)

Steins computes and renders something, and it differs. By shape:

| n | shape |
| ---: | --- |
| 1,609 | array vocabulary |
| 639 | string refinement |
| 371 | int range |
| 331 | union shape |
| 244 | other |
| 66 | literal / constant value |

The canonical row is `abs()`: expected `int<0, max>`, got `int<0, max>|float` —
a spurious union arm, not a missing one. Top fixtures: `array-column` (223 over
two files), `class-implements` (90), `array-functions` (84).

## What the measurement says about sequencing

**Vocabulary before reach.** A large share of B is downstream of A: an
expression whose result type has no spelling renders `unknown` whatever the
engine computed. Closing B first means touching the same code twice.

**And two thirds of A is already answered**: `phpstan-special` is not
vocabulary, `mixed` is modelled (and, since issue #239, no longer counted here
at all), `generic-other` is issue #10. What is left is smaller than the
headline: the two intersection halves, the `mixed`-cut vocabulary,
`class-string`, and template rendering — on the order of **1,000 rows**, not
2,486.

> **Correction (2026-08-08, #237):** the `mixed`-cut vocabulary is not in that
> remainder either. Its 158 rows are a reach item (154 render `unknown`) with a
> one-row ceiling, closed as ADR-0030 registry entry 6. Read together with #239
> and #235, the pattern is the note's most reusable finding: **an A bucket has
> to be read against what Steins renders before it is priced.** Of the four
> buckets so read, `mixed` (#239) and the `mixed` cuts (#237) turned out not to
> be work at all, the intersection bucket (#235) turned out to be speller and
> seed work rather than vocabulary, and only `class-string` (#236) was the
> vocabulary slice its row count implied.

## Follow-up

Sliced smallest-first, so each lands a measurable nsrt delta:

1. Probe whether accessory conjunctions are a speller gap or a domain gap (336
   rows) — a scoping slice, not an implementation one. **Done (#235); the
   answer moved this item out of the vocabulary plan entirely — see the probe
   below.**
2. ~~`class-string` and its parameterized form (148).~~ **Bare form landed
   (issue #236).** The predicate, its spelling, and the acceptance relation are
   in; `::class` and the declaration-flow producers are in. Measured outcome:
   37 of the 148 left `unsupported`, 2 of them straight to `match`, and the
   vocabulary paid for itself elsewhere — 64 rows outside the bucket moved
   `differ → match` (the `class-implements` block, which asserts
   `array<string, class-string>|false`, is 60 of them). **The parameterized
   form waits on the ADR-0032 carry (issue #10)**: 106 of the 148 spell
   `class-string<T>`, and 22 of those name a template parameter, a
   `static(C)`, a `$this(C)`, or a `hasMethod(…)` accessory — none of which the
   bare refinement could carry without inventing the generics vocabulary here.
   `class-string<T>` is meanwhile lowered as the bare predicate, a widening.
   The remaining 35, by what blocks each: builtin returns 10 (`get_class`,
   `get_parent_class` — the function-map miner refused all 41 `class-string`
   rows as unspellable and can now admit them, on the next pin bump), guards 8
   (`is_a` / `is_subclass_of` / the `*_exists` family), templates 7, native
   declarations masking a phpdoc refinement 5 (**not** class-string-specific —
   `positive-int` and `non-empty-string` are masked identically), property and
   method-return flow 3, `ltrim` predicate transfer 2.
3. ~~The `mixed` cut vocabulary beyond Null/Falsy (133).~~ **Closed as a
   ceiling (#237), nothing built.** 154 of the bucket's 158 rows render
   `unknown`, so it is a §B reach item shelved in an §A bucket, and the four
   rows that render a type cap the whole slice at **+1 admissible**. Recorded
   as ADR-0030 divergence-registry entry 6; see the correction in §A.
4. Object intersections, on #234's inhabitance rule (183 → 246).
5. ~~Decide what `mixed` should score as in the harness (329) — a measurement
   decision that moves no engine code.~~ **Done (issue #239)**; see the
   correction at the top. It moved 329 rows from `unsupported` to `differ`,
   324 of them into slice 6's reach pile, and left the headline and the
   admissible figure untouched.
6. Then reach: loose comparisons, `filter_var`, binary operators (896).
7. Then precision: the array-vocabulary block (1,609), decomposed.

## 2026-08-08 — the accessory-conjunction probe (#235)

Same instrument, same inputs: master `bc6df55`, phpstan-src `55a7732`, 15,845
assertions, the six verdict counts reproduce to the row. Everything below is
measured — `target/nsrt/nsrt-asserttype.json` for the counts, `steins check`
with `\PHPStan\dumpType` for the witnesses.

### The filter, and the corrected split

Take `verdict == "unsupported" && class == "intersection"` (519 rows) and keep
the rows where **every arm of every `&`-group is a string-refinement keyword**
(`lowercase-` / `uppercase-` / `non-empty-` / `non-falsy-` / `numeric-` /
`decimal-int-` / `non-decimal-int-` / `literal-` / `class-string`):

| half | n |
| --- | ---: |
| accessory conjunctions | **273** |
| object / array intersections (`hasOffset`, class names, `callable`) | **246** |

Not 336/183. No stated-or-implied filter reaches 336: the loosest reading that
still excludes class names ("any accessory keyword anywhere in `expected`")
gives 284, and the strictest gives 273. The 336 figure above is withdrawn.

**27 distinct arm-combinations** occur, and they are the whole population:

| n | combination | n | combination |
| ---: | --- | ---: | --- |
| 47 | `lowercase-string&non-falsy-string` | 6 | `lowercase-string&non-empty-string&uppercase-string` |
| 27 | `lowercase-string&non-empty-string` | 6 | `literal-string&non-falsy-string` |
| 23 | `lowercase-string&numeric-string` | 3 | `lowercase-string&non-empty-string&numeric-string` |
| 19 | `lowercase-string&non-falsy-string&uppercase-string` | 3 | `non-decimal-int-string&non-falsy-string` |
| 16 | `numeric-string&uppercase-string` | 2 | `lowercase-string&non-falsy-string&numeric-string` |
| 15 | `non-empty-string&uppercase-string` | 2 | `non-falsy-string&numeric-string&uppercase-string` |
| 15 | `decimal-int-string&non-falsy-string` | 2 | `literal-string&non-falsy-string&uppercase-string` |
| 14 | `lowercase-string&non-falsy-string&numeric-string&uppercase-string` | 2 | `literal-string&lowercase-string&non-falsy-string&numeric-string&uppercase-string` |
| 14 | `non-falsy-string&uppercase-string` | 2 | `literal-string&lowercase-string&non-falsy-string&uppercase-string` |
| 11 | `non-falsy-string&numeric-string` | 1 | `class-string&literal-string` |
| 10 | `non-empty-string&numeric-string` | 1 | `literal-string&lowercase-string&non-empty-string&numeric-string&uppercase-string` |
| 8 | `lowercase-string&uppercase-string` | 1 | `literal-string&lowercase-string&uppercase-string` |
| 8 | `lowercase-string&non-empty-string&numeric-string&uppercase-string` | | |
| 8 | `literal-string&non-empty-string` | | |

Thirty of the 273 carry a `literal-string` / `class-string` arm — `StrOpaque`,
not a `StrPreds` set, so no predicate spelling can ever reach them. That leaves
**243 pure-`StrPreds` rows**, and run through `StrPreds::close` they collapse to
**17 distinct closed predicate sets**. A closed set, decisively — not an open
conjunction algebra.

### The four answers

**1. Does the value domain hold two accessory refinements at once? Yes.**

`StrPreds` is a `u8` bitset closed under implication; a conjunction is the only
thing it can represent. The witness makes that observable through a surface
that shows one keyword at a time. `preds_keyword` ranks
`NUMERIC` > `NON_FALSY` > casing > `NON_EMPTY` and emits exactly one keyword, so
a set holding `{NON_FALSY, LOWERCASE}` renders `non-falsy-string` and the casing
half is *invisible*. `substr` drops the length predicates and keeps casing
(pinned in `str_pred_transfer.rs`), so it re-exposes what was underneath:

```php
/** @param non-falsy-string $nf */
function speller($nf): void {
    $x = strtolower($nf);
    \PHPStan\dumpType($x);            // non-falsy-string (asserted)
    \PHPStan\dumpType(substr($x, 3)); // lowercase-string (asserted)   <= LOWERCASE was held all along
}
```

The uppercase mirror behaves identically (`strtoupper` → `non-falsy-string`,
then `substr` → `uppercase-string`). And the corpus corroborates without any
constructed fixture: `str-casing.php:54`, `more-types.php:51` and
`lowercase-string-pad.php:18` all render `non-empty-lowercase-string` — the one
accessory conjunction that happens to have a single keyword today.

**2. Where is the loss? In the speller, and (separately) in the seed — not in
the acceptance relation.**

- *Renderer.* `render_dump_fact` routes the entire bitset through
  `steins_contract::spell::preds_keyword`, whose own doc already says it: "One
  keyword comes out, never an intersection." One call site, one ladder. This is
  the loss the witness above measures, and it is exactly localized.
- *Acceptance relation: not a blocker.* `steins_contract::lower` maps
  `TypeKind::Intersection` → `ContractTy::Inter`, and `admit`, `normalize` and
  `spell` all carry `Inter` arms (`spell` even joins them with `&`). `lower_str`
  parses `A&B` today.
- *Seed: a real hole.* A **declared** conjunction reaches nothing:

  ```php
  /** @param lowercase-string&non-falsy-string $both */  // dumpType => unknown
  /** @param lowercase-string&non-empty-string $ne */    // dumpType => unknown
  ```

  `$ne` is `non-empty-lowercase-string`, which Steins can already say — so this
  is not a spelling failure. `contractty_to_fact` has no `ContractTy::Inter`
  arm and returns `None`, so an intersected `@param` seeds no fact at all.

**3. Nothing is lost at a join or at storage — but most of the 243 rows never
had anything to lose.** Attributing each row by comparing Steins' `got` against
the *ceiling* `preds_keyword` could emit for the expected closed set:

| n | attribution |
| ---: | --- |
| 10 | Steins already renders **exactly** the expected set — harness-only, no engine change |
| 42 | the set is sayable with today's vocabulary, but Steins does not compute it |
| 9 | Steins sits at the speller's ceiling; only a conjunction spelling could add anything |
| 92 | `unknown` — nothing computed at all |
| 90 | below the ceiling *and* unsayable |
| 30 | a `StrOpaque` arm (`literal-string` / `class-string`) |

The reach failures are ordinary and reproducible:

```php
/** @param non-empty-string $ne */   if ($ne !== '0') dumpType($ne);  // non-empty-string, not non-falsy-string
/** @param lowercase-string $lc */   dumpType($lc . 'abc');           // unknown
/** @param numeric-string $n */      dumpType(strtoupper($n));        // non-empty-uppercase-string, NUMERIC dropped
```

`!== '0'` establishing `NON_FALSY`, concatenation carrying predicates, and
`strtoupper` preserving `NUMERIC` are three separate transfer gaps, and between
them they account for far more of the bucket than any spelling does.

**4. The spelling should be Steins' own compound keyword, not PHPStan's `&`.**

Steins already spells one of these conjunctions as a single word —
`non-empty-lowercase-string` — and the codebase already calls the
`LOWERCASE ∧ UPPERCASE` set *uncased*. Extending that pattern gives a closed
grid, core rung × casing:

- core ∈ {—, `non-empty-`, `non-falsy-`, `numeric-`, `non-falsy-numeric-`}
- casing ∈ {—, `lowercase-`, `uppercase-`, `uncased-`}

which names 15 of the 17 closed sets; two cells (`non-empty-lowercase-string`,
`non-empty-uppercase-string`) already exist. The two remaining sets carry
`DECIMAL_INT` / `NON_DECIMAL_INT`, which `preds_keyword` deliberately refuses as
rungs and should keep refusing. Because the harness normalizes and `lower`
already handles `Intersection`, an equivalent-but-different rendering scores
`match` / `equal` on its own terms — importing `&` buys nothing and costs the
phpdoc round-trip.

### Verdict

**A reach gap, dominant by a wide margin. Not a domain gap; a real but small
speller gap; no relation gap.**

- **Domain gap: none.** The bitset holds the conjunction, and the witness shows
  it surviving a render that cannot show it.
- **Relation gap: none on the contract side.** `lower_str` already parses `A&B`.
  One seed-side hole (`contractty_to_fact` refusing `Inter`), worth closing on
  its own merits, but it is not what costs the rows.
- **Speller gap: real, precisely localized to `preds_keyword`, and bounded at
  ≤ 19 of 243 rows** (the 10 already-exact plus the 9 at the ceiling) — and 10 of
  those 19 need no engine change at all.
- **Reach gap: 224 of 243 rows.** Steins renders `unknown` or a strictly weaker
  set because the predicates were never established.

The consequence for §A's plan is larger than the correction to its arithmetic:
**263 of these 273 rows are misfiled**. They sit in the *vocabulary* bucket only
because `is_supported_atom` refuses any atom containing `&` before the relation
is ever consulted. They are reach and precision failures wearing a vocabulary
costume, and the "vocabulary before reach" sequencing does not apply to them.
Dropping the `&`-gate for all-`StrPreds` atoms is a harness-only change that
re-files them honestly and flips 10 rows to admissible with zero engine change.

Sized and filed as issue #240, in three separable pieces: re-file the bucket
(harness only, +10 admissible for free), seed the declared `Inter` form, and
spell the closed grid. The reach gaps it exposes are deliberately left for
their own issues, once #240's first piece makes them visible as `differ` rows.

### Realized delta (#240, implemented)

Same instrument, master `5264878`, phpstan-src unchanged, 15,845 assertions. The
three pieces landed in the order the issue sized them, each measured on its own:

| run | match | unsupported | equal | subsumed | differ | skipped | **admissible** |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| baseline | 1,711 | 2,120 | 108 | 220 | 11,378 | 308 | **2,039** |
| + piece 1 (harness) | 1,711 | 1,877 | 110 | 228 | 11,611 | 308 | **2,049** |
| + piece 2 (seed) | 1,711 | 1,877 | 110 | 232 | 11,607 | 308 | **2,053** |
| + piece 3 (grid) | 1,711 | 1,877 | 110 | 243 | 11,596 | 308 | **2,064** |

The 273 accessory rows, before → after: `unsupported` 273 → 30, `differ` 0 →
223, `subsumed` 0 → 18, `equal` 0 → 2. **No row anywhere moved away from
admissible**, and `match` did not move at all.

Three corrections to the probe above:

1. **Piece 1 moves 243 rows, not 263.** The two figures cannot both hold: the 30
   rows with a `literal-string`/`class-string` arm stay gated *by the same
   decision*, so 233 land in `differ` and 10 flip to admissible. "263 to
   `differ`" double-counted the 30.
2. **`contractty_to_fact` is not the lowering a declared `@param` reaches.** That
   hole is real but sits on the *builtin* return path (curated rows, the
   ADR-0069 floor), where no intersection occurs today. A declared `@param`
   reaches `steins_contract::to_fact` through issue #242's
   `seed_refined_scalar_fact` when the parameter has a native type, and
   `spell_arms` through the arm lane when it does not — so closing the hole where
   the probe pointed would have changed nothing observable. The fold now lives in
   one place (`steins_contract::inter_str_preds`) and all three read it.
3. **The speller gap was bounded at ≤ 19 rows and paid 11.** The 10 already-exact
   rows were piece 1's, and 4 of the remainder were piece 2's; the grid's own
   +11 includes rows the probe's ceiling attribution had filed under "sayable but
   not computed", because the *conjunction* seeded by piece 2 became sayable only
   with the grid in front of it. The two pieces are not separable in the way the
   attribution table implies.

The reach gaps the re-filing exposes are now visible as `differ` rows and remain
for their own issues: `!== '0'` establishing `NON_FALSY`, concatenation carrying
predicates, and `strtoupper` preserving `NUMERIC`.
