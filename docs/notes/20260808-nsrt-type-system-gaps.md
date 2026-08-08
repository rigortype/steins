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

The gaps split three ways, and the split is what the plan follows.

## A. Vocabulary — 2,486 rows

The oracle spells something Steins has no way to say. **Two thirds of the
headline number is not work**, which is the first thing measurement bought:

| class | n | reading |
| --- | ---: | --- |
| `phpstan-special` | 669 | `*ERROR*` (391) + `*NEVER*` (272) — PHPStan's own internal markers. Not vocabulary; nothing to port. |
| `intersection` | 519 | see below — this bucket is two unrelated problems |
| `mixed` | 329 | **not an engine gap.** `ContractTy::Mixed` exists and spells `mixed`; the harness classifies the spelling as out-of-scope. A harness-semantics question, not a hole. |
| `generic-other` | 225 | non-array generics (`DOMNamedNodeMap<DOMAttr>`) — the ADR-0032 carry root, already tracked as issue #10 |
| `subtraction` | 157 | 133 of them are `mixed~…` |
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

### Subtraction is a `mixed`-cut problem

133 of the 157 rows are `mixed~null`, `mixed~int`, `mixed~array<mixed, mixed>`,
`mixed~(0|0.0|''|'0'|array{}|false|null)`. Steins models exactly two cuts —
`MixedCut::Null` and `MixedCut::Falsy`, spelled `non-null-mixed` /
`non-empty-mixed` — against PHPStan's arbitrary type subtraction. So this is
not "subtraction is missing" (ADR-0052 subtraction exists); it is that the cut
vocabulary is a two-value enum where the oracle has a type algebra.

## B. Reach — 7,843 rows (70% of `differ`)

The vocabulary exists and nothing computes the value: Steins renders `unknown`
where PHPStan asserts a concrete type. Ranked by fixture:

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
vocabulary, `mixed` is modelled, `generic-other` is issue #10. What is left is
smaller than the headline: the two intersection halves, the `mixed`-cut
vocabulary, `class-string`, and template rendering — on the order of **1,000
rows**, not 2,486.

## Follow-up

Sliced smallest-first, so each lands a measurable nsrt delta:

1. Probe whether accessory conjunctions are a speller gap or a domain gap (336
   rows) — a scoping slice, not an implementation one.
2. `class-string` and its parameterized form (148).
3. The `mixed` cut vocabulary beyond Null/Falsy (133).
4. Object intersections, on #234's inhabitance rule (183).
5. Decide what `mixed` should score as in the harness (329) — a measurement
   decision that moves no engine code.
6. Then reach: loose comparisons, `filter_var`, binary operators (896).
7. Then precision: the array-vocabulary block (1,609), decomposed.
