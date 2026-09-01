# Dynamic return typing: the PHPStan extension-point import lands in five named seams

Owner directive (2026-07-29): organize and advance the port of PHPStan's
dynamic-typing machinery. Status: PENDING ratification (post-hoc mode).
Scope: the behaviors PHPStan ships as `Dynamic{Function,Method,StaticMethod}ReturnTypeExtension`
and `{Function,Method,StaticMethod}TypeSpecifyingExtension` — per-call
computed return types and per-guard narrowing — for builtins and the
stdlib. Framework-level dynamics (Larastan-class magic) are explicitly the
plugin surface (ADR-0012) and out of scope here.

## 1. Decision: one behavior, one seam

Every imported extension behavior is classified into exactly one of five
existing Steins seams — no new extension mechanism is built:

| Seam | Steins machinery | What imports here |
| --- | --- | --- |
| (i) Fold | sidecar folding (ADR-0004/0028, foldable allowlist) | literal-argument calls the real PHP can answer (`strtoupper('a')`, `range(1,3)`) — already live; grows by allowlist row |
| (ii) Symbolic transfer | argument-dependent returns (ADR-0061 A11/A12 machinery) | shape/arg-structure-dependent returns computable without execution: `explode` non-empty-list, `json_decode` by flags, `range` bounds, array positional projections (= ADR-0062 S7), `str_split`, `array_chunk`, … |
| (iii) Curated rows | builtin return facts under the THREE-legged admission gate (ADR-0056, extended by ADR-0061) | envelope refinements no rule computes but probes verify — the existing 11-row discipline, verbatim probes required, refused-row table maintained |
| (iv) Plugin | sidecar plugins (ADR-0012) | userland/framework dynamic returns; deferred post-v0.1.x |
| (v) Guard vocabulary | narrowing operators (ADR-0052; ADR-0062 S4 pattern) | TypeSpecifyingExtension imports: `is_numeric`/`ctype_*` → StrPreds, `array_is_list` (S4), `count()` (S3/S7), `in_array`, `str_starts_with`, … |

The classification question for any candidate is mechanical: *can the real
PHP answer it on literals* (i) — *can a total rule compute it from argument
structure* (ii) — *is it only measurable, not derivable* (iii) — *does it
require framework boot* (iv) — *is it a narrowing, not a return* (v).

## 2. Priority is measured, not curated

The import queue is ordered by two instruments, not taste: the conformance
table's failing/unenforced rows (which name specific spellings), and
`cargo xtask freq` corpus frequency for builtin call sites. A candidate
absent from both waits. Each (ii)/(iii) addition opens in measurement mode
per the standing protocol; (iii) additions must pass the full admission
gate — a curated row a later PHP minor could falsify is the documented trap
(ADR-0056 §2) and stays refused.

## 3. Declined imports

- A runtime-pluggable DynamicReturnTypeExtension interface in Steins core —
  the five seams cover the behaviors; a sixth open-ended hook would be a
  second extension mechanism competing with ADR-0012 plugins.
- Bulk functionMap transcription — rows enter through (ii)/(iii) with their
  gates, never by mass import (the ADR-0056 subset-hole lesson). *Narrowed
  by ADR-0069: mass import is admitted into the Asserted floor only, never
  into these Verified-lane seams.*

## 4. Slices

| Slice | Content |
| --- | --- |
| DR1 | Candidate census: conformance rows × freq sweep → classified queue (design artifact, no code) |
| DR2 | Seam (v) batch: the guard-vocabulary imports the census ranks first |
| DR3 | Seam (ii) batch: top symbolic transfers (excluding S7's array set, already scheduled) |
| DR4 | Seam (iii) batch: probe-verified curated rows from the census remainder |

DR1 is a half-day design task and gates the rest; DR2–DR4 are ordinary
protocol slices.

## 5. DR1 census outcomes (2026-07-29, amendment)

The census (scratchpad dr_census/DR-CENSUS.md) confirmed the taxonomy with
two recorded exceptions rather than forced fits:

- **By-ref out-param facts fit no seam.** `preg_match`'s real value is the
  `$matches` shape, an out-parameter fact channel — that belongs to
  ADR-0063 P2's `mutate.local` world plus a future out-param fact lane,
  not to any of the five seams. Recorded here so no DR slice force-fits it.
- **`array_replace_recursive`** (surprisingly frequent) exceeds the
  current shape algebra's reach (recursive N-array merging); deferred with
  its frequency noted, not silently dropped.
- Queue headline: the `is_*`/`ctype_*` type-narrowing guard family is
  entirely unported (seam v) and `assert` inherits every addition for
  free; `sprintf` (the #1 builtin by frequency) is already fully served by
  the fold seam — no work fits.

## Amendment B (2026-07-31): a `mixed` declaration pin is inadmissible alone —
## seam (ii) rules for such names carry an arity second leg

Owner-directed, landed with the array read-position family (issue #76). Where
this amendment contradicts §1's seam-(ii) description, the amendment governs.

**The hole.** Every seam-(ii) transfer is admitted by countersignature: the
running engine's own reflected *declaration* must be the one the rule was
written against (ADR-0061 §2, as `shape_projection_fact` and
`arg_dispatch_return_fact` implement it). The check is meaningful because a
declaration is a real, movable claim — when php-src adds an arm, the reflected
string changes and the stale rule stops firing. That reasoning fails exactly
when the declaration is **`mixed`**. `mixed` is the top of the type lattice, so
every possible rule output is inside it; the check degenerates from "this engine
still agrees with the rule" to "this engine has heard of the name". A rule
admitted on a `mixed` pin is, in gate terms, admitted on nothing.

This is not a corner case. The whole array read-position family — `current`,
`reset`, `end`, `next`, `prev`, `array_pop`, `array_shift`, `array_first`,
`array_last` — declares `mixed` (`key` alone declares `string|int|null` and
needs none of this), and the survey for issue #75 ranked that family first by
measured corpus impact. Declining every `mixed`-declared name outright would
have forfeited the top of the import queue; admitting them on the degenerate
pin would have put the family's facts into the tree on nobody's evidence.

**The ruling.** A seam-(ii) rule whose name's reflected declaration is a bare
`mixed` is **inadmissible on the declaration alone** and must carry a second
leg: an **arity pin** against the live signature. The `reflect` wire reply gains
`params_total` / `params_required` (`ReflectionFunction::getNumberOfParameters()`
and `getNumberOfRequiredParameters()`), surfaced through
`Folder::builtin_param_counts`, and the rule fires only when the engine reports
the signature the rule was written against — `(1, 1)` for all ten names of this
family, measured at `PINNED_PHP` rather than assumed.

Three properties make this a real countersignature and not a ceremony:

- **It is a claim about *this* engine that can fail.** A parameter added,
  removed, or made optional across a PHP minor moves the counts, and a rule
  written against the old signature stops firing — the same failure direction
  the declaration check gives for a widened return type.
- **Absence withholds, exactly as declaration-absence does.** An engine that
  reports no arity — an older runner, a canned replay table recorded before the
  field, a reflection failure — yields `None`, and the rule is withheld rather
  than admitted un-countersigned. Nothing here weakens ADR-0061 §2; it is a
  conjunct added to it, never a substitute for it.
- **It is checked mechanically, not by discipline.** The rule table pairs each
  arm's declaration pin with its arity pin, and a debug assertion refuses any arm
  that names `mixed` without one, so the obligation cannot be forgotten by the
  next author.

**What it does not license.** The arity pin admits a rule; it never *supplies*
one. A `mixed`-declared name still gets no envelope to be extensionally inside
(ADR-0061 §2's honest limit), so its rule remains responsible for stating an
answer the four-layer domain can actually spell, and declines when it cannot —
`json_decode` stays declined under this amendment for exactly the reason §1
recorded, and the read-position family declines every `∪ false` that would need
a two-base union. Nor does the pin extend to seam (iii): a curated row's
admission gate is unchanged.

**Side effect, recorded so it is not rediscovered as new work.** The reflect
arity surface is precisely what `call.too-many-arguments` for internal targets
has been waiting on (docs/internal-spec/catalog.md, "Builtin *signatures*").
That checker is a separate slice; this amendment lands the surface only, and no
consumer of it exists beyond the pin.

**Extent, corrected (2026-08-02, issue #118).** The amendment landed with the
arity leg on the *shape-projection* rung alone, and the argument-dispatched rung
carried a debug assertion **refusing** `mixed` outright — the note there read
"this rung has no arity second leg to offer". That was a statement about the
implementation, not about the taxonomy: both rungs are seam (ii), both are
admitted by the same countersignature, and the obligation stated above is
written for the seam, not for one of its two tables. `min`/`max` declare a bare
`mixed` and are the batch's highest-ranked remaining import, so the dispatch
rung grew the same leg, pinned at the measured `(2, 1)`, and its assertion
became the same obligation the projection rung's already was: **name `mixed` and
you must pin the signature.** The seam classification of neither name moved —
`min`/`max` are seam (ii) argument-dispatched as surveyed, `array_slice` is seam
(ii) shape-projection as surveyed. `json_decode` is unaffected: its decline was
never about the pin.

## Amendment C (2026-09-02): a seam-(ii) rung may read an argument's SYNTAX, and when it must — PENDING ratification

Issue #615 leg (b). ADR-0070 institutionalized "ask the value domain, not the
syntax" for arguments, and `filter_var`'s input argument obeys it through
`transfer_arg_fact`. Its **flags** argument cannot, and the reason is a fact
about the domain rather than a preference:

> A global constant carries no proven value (issue #168). `$nullFilter =
> \FILTER_NULL_ON_FAILURE;` binds no `Fact` at all — `\PHPStan\dumpType($nullFilter)`
> on that very assignment answers `unknown` — so a rule that asks the value
> domain for a flag receives nothing, for every spelling that is not a literal.

So the roster keys on the constant's **name**, and the amendment records that
this is the correct reading rather than a shortfall against ADR-0070:

1. **A rule whose argument is drawn from a fixed engine vocabulary reads names.**
   `FILTER_FLAG_HOSTNAME`, `_IPV4` and `_EMAIL_UNICODE` share one engine value, so
   a value-keyed reading could not tell them apart even if #598 supplied values.
   Names are also what makes the accepted list auditable.
2. **The composers are read from the syntax, one per kind of combination.** A `|`
   chain combines flags into one set (PHP's own `|` over the bits, a boolean union
   over the roster); a `?:` ternary offers two sets as *alternatives*, which the
   rung answers separately and joins — declining the whole call, never one arm,
   when the domain cannot unite them.
3. **An unrecognized term anywhere declines the whole call.** Load-bearing: an
   unread flag may be `FILTER_FLAG_STRIP_LOW`, which rewrites the string and makes
   `FILTER_DEFAULT` stop being the identity. A composer must poison, never drop.
4. **A variable stays a decline**, recorded against issue #598 rather than fixed
   here. That is the boundary: the seam reads syntax it can name, and a name it
   must resolve through a *binding* is the value domain's question, unanswerable
   until the engine-constant ruling lands.

Stratum is unaffected and needs no new machinery: the seam already takes `min`
over every argument's stratum, so an input read from a docblock-claimed lane
floors an answer whose flags came from a `|` chain, exactly as ADR-0061 §3
requires.

**The IR cost, and the shape of the carrier.** The `|` chain needed a value form;
`ValueOp` gained `BitOr` and `SCHEMA_VERSION` went 7 → 8 (`ArgValue` is persisted
trace IR, ADR-0092 §2). The variant deliberately reaches **no fact seam**: a
bitwise `|` has no total floor — `int|int` is an `int`, `string|string` a
`string`, and PHP's GMP extension overloads the operator to return an object, so
even `int|string` would be a lie — so `eval_binary_fact` keeps its totality by
taking a `CmpOp`, and a `|` falls through saying nothing, as it did when it was
`ArgValue::Other`. Carrying it is nonetheless what unlocked the leg, for a reason
worth generalizing: **an `Other` element collapses its whole enclosing array
literal to `Other`**, so `['flags' => A | B]` was not an array at all and no rule
could read even its key. A form that answers nothing can still be worth its
schema bump when it keeps a *container* representable.

### Measurement

Legs (a)+(b): nsrt unknown-fall 6510 → 6407 (−103), 42 `differ → match`, 60
`differ → subsumed`, 1 `differ → differ` (a sound-but-wider `array<int|null>`
where the fixture asserts `array<null>` — the rung cannot prove `'foo'` fails
`FILTER_VALIDATE_INT`, the same class as the `filter-var.php:86` row #608 already
shipped). Nothing outside `filter-var.php` / `filterVar.php` moved, so the new
value form changed no answer of its own.

## Amendment D (2026-09-02): a rung may dispatch on the argument COUNT, and a name whose return type depends on it belongs here rather than in the declared-envelope floor — PENDING ratification

Issue #617. `sscanf` is the first seam-(ii) rung whose answer's **base** — not
its refinement — is decided by how many arguments the call passes:

```php
sscanf($s, '%d-%d')            // array{int|null, int|null}|null
sscanf($s, '%d-%d', $a, $b)    // int|null
```

Both are the same function. The second form assigns through by-reference
out-parameters and returns the *count* of assigned conversions; the first
returns the conversions themselves. No fact about any argument distinguishes
them — only the arity of the call site does.

1. **The count is read before any argument is.** The rung's dispatch is
   `args.len()` first, format second. Reading the format first and then
   discovering the arity would be a wasted read at best and, for a format the
   scanner declines, a wrong `None` at worst: a 4-argument `sscanf($s, $s, $a,
   $b)` answers `int|null` while its format proves nothing at all.
2. **This is why the name has no ADR-0069 declared-return floor, and that is
   correct rather than an omission.** `declared_returns.toml` holds two alternate
   signatures for `sscanf` that *disagree on the return type*, and the generated
   catalog skips every such name. A floor is a single envelope; a name whose
   return type genuinely depends on its argument count has no single envelope to
   floor, and ADR-0064 §1's seam (ii) is exactly the machinery for it.
3. **The arity pin is carried even where Amendment B does not force it.**
   `sscanf` reflects `array|int|null`, not a bare `mixed`, so Amendment B's second
   leg is silent. The rung pins `(3, 2)` regardless, because it reads argument 1
   positionally *and* dispatches on the count — a php-src signature that grew a
   parameter in front of `$format` would leave both reads stale while
   `array|int|null` still held. Generalizing: **an arity pin is required whenever
   the count is load-bearing, not only when the declaration is uninformative.**

### The corollary that cost the most: a measurement may refute the fixture in the sharpening direction

ADR-0061 §4 already requires every cell to come from a `php -r` probe, and the
issue #40 / #594 precedent already covers a measurement refuting a fixture by
being *weaker*. This slice adds the other direction, and both showed up in the
same specifier:

- **Weaker.** The fixture asserts `sscanf($s, '%2s')` yields a
  `non-falsy-string`. A width bounds a `%s` read from *above*, so
  `sscanf('0', '%2s') === ['0']` — falsy. Every fixture row carrying the claim has
  a literal subject, so the refinement is being read off the subject, not the
  width. Two rows are a deliberate non-win.
- **Sharper.** The same probe proves `%s` can never yield `''` at any width
  (0 empties in 40,000 randomized subject × format trials), so the rung answers
  `non-empty-string` where the fixture says plain `string`. Two *other* rows
  become non-wins for the opposite reason.

The rule the amendment fixes: **the measurement is authored in both directions,
and a fixture row lost to a sharper answer is as acceptable as one lost to a
weaker one.** Trimming a proven refinement to match a fixture would be
transcription with extra steps, and would leave the rung unable to explain its
own table.

`%u` is the same discipline at the decline: `sscanf('-8', '%u')` is the *string*
`'18446744073709551608'`, so its true slot is `int|string|null` — a two-base union
no shape slot spells — and the whole call declines rather than emit the fixture's
unsound `int|null`. One unreadable specifier declining the *whole* call is the
`filter_var` invariant (Amendment C leg 3) applied to a format string.

### Two things deliberately not done

**The fold route was priced and declined.** Seven rows carry a literal subject
*and* a literal format, and folding `sscanf` would answer them exactly. It was
not taken: the format table already wins five of the seven, the remaining two are
precisely the rows the measurement says the fixture gets wrong, and `sscanf` is a
variadic-by-reference name — putting it on the hand-picked allowlist means the
folder invoking a function whose extra arguments the analyzed source names as
out-parameters, which touches the boundary ADR-0070 §3 keeps closed. Recorded as
available on its own evidence, not smuggled in on this slice's.

**`fscanf` shares the format table and not the rung.** It reflects
`array|int|false|null`; `Fact::Shape` carries a `nullable` side-flag and no
`false` one, and `Fact::Union` admits no array arm by construction (ADR-0062 §3).
Answering `array{…}|null` for it would be *unsound*, not coarse. The scanner is a
free function so that the table is written once when a domain can spell that
outer arm.

### Measurement

nsrt unknown-fall 6548 → 6523 (−25); admissible 3583 → 3605 (+22). 25 rows moved,
every one an `sscanf` call: 3 `differ → match`, 12 `differ → equal`, 7
`differ → subsumed`, 2 `differ → differ` (the refuted `non-falsy-string` rows),
plus `bug-7563.php:25`, answered now but still differing because its explicit
`null ===` guard does not narrow a `Fact::Shape`'s null arm — a pre-existing gap
in shape-nullable narrowing, unrelated to this rung. 21 of the fixture's 22 rows
left `unknown`; the 22nd is the `%u` decline. The six `list()`-destructuring rows
downstream did **not** move, which pins that gap as genuinely separate: the answer
is now available at the call, and the destructuring slice is what has to read it.
