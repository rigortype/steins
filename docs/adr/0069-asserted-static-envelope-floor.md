# A builtin's declared return type gets an Asserted floor, imported by name and firewalled by grade

Without a live engine, a builtin call with variable operands types as
`unknown`: every rung of the return ladder is engine-gated, and ADR-0061's
ladder deliberately ends at `no sidecar → nothing`. This ADR raises that
floor with a static declared-envelope table whose data lineage is PHPStan's
functionMap — itself inherited from Phan — under an owner decision
(2026-07-31, issue #73) that consciously narrows the wholesale-import
refusals of ADR-0056 §6 and ADR-0064 §3. Status: PENDING ratification.

## 1. Context: the designed hole, and why it is now worth filling

`str_repeat($str, $times)` is `string` with the sidecar and `unknown`
without it. The gap is not an accident: ADR-0056 grounded builtin return
knowledge in what the engine that will run the code says at analysis time,
and refused the alternative — a hand-maintained signature map — as the rot
ADR-0014 warns about. The 13 curated rows cannot serve offline either; they
are refinements, structurally in need of a reflected envelope to subsume.

Two things changed. Issue #64 made the engine-present path reach the
browser, so the engine-absent population shrank to `--no-php`, the pre-boot
playground, and spawn failure past the respawn cap — exactly the places
where a *less precise but honest* answer beats silence. And the survey for
issue #73 established that the refusals' rationale is specific: bulk rows
rot **silently** when they carry authority they cannot re-earn. A floor
that never carries authority is outside the rationale's reach.

## 2. Decision: import into the Asserted lane, never the Verified one

The refusal stands, narrowed: no functionMap row enters the Verified lane,
ever. The import lands as a new bottom rung of the return ladder with the
grade of tool-shipped phpdoc:

- **Asserted, not Verified.** The floor seeds the dump surface and
  contracts-tier reasoning. It is never a proof-layer premise; the zero-FP
  default surface cannot cite it by construction.
- **Any engine answer wins — per name, not per run.** The rung sits
  strictly below `builtin_return_fact` and fires exactly where the folder
  yielded `None` for the asked name. `--no-php` is only the total case;
  with a live engine the floor still speaks where the engine is silent —
  a name whose extension is not loaded on the analyzing PHP
  (`mysqli_fetch_field` without mysqli), or a name with no declared
  return type in reflection. An absence-family finding may stand beside
  such a floor fact, and the pair is complementary, not contradictory:
  the call fails on the analyzing PHP, and if the runtime has it, this
  is its declared shape. Where the engine *answers*, the floor never
  overrides it — the consuming engine may not be the pinned one, and a
  static row must not outvote the real thing.
- **Return envelopes only.** Declared types, lowered through the same
  `lower_str` → `envelope_fact` seam the reflected envelope uses — one
  lowering, two provenances. No param types, no arity, no curated-grade
  refinement enters by this road.
- **The absence family never consumes it.** Existence is a boot-surface
  fact; a table answering `function_exists` is a false-absence FP factory
  (the php-wasm spike's missing-mbstring lesson, in static form).

This is the ADR-0067 shape applied to return types: a declared lane beside
the proven one, legible as such, never laundered into proof.

## 3. Decision: rot is answered by machinery, not diligence

ADR-0014's concern — a signature map that silently drifts from the engine —
is met structurally, the way `hierarchy.toml` already meets it:

- The source is **pinned**: one phpstan-src commit, mined into a committed
  TOML under `docs/research/`, regenerated only alongside `PINNED_PHP`
  bumps.
- Every candidate row is **cross-checked at generation time against the
  pinned engine's reflection** via the real sidecar. A row where
  functionMap and php 8.5 disagree is excluded and listed in the generated
  file with the disagreement verbatim. The per-row evidence bar of
  ADR-0056 is thereby automated, not waived.
- **Version discipline is A11-shaped.** The functionMap delta files are the
  change oracle: a function whose signature moved across the supported
  minors declines when the project's declared PhpTarget straddles the
  change; an unknown target admits, because the row is Asserted anyway and
  its consumers tolerate that grade.

## 4. Decision: the lineage is named where license law puts it

PHPStan's `resources/functionMap.php` opens by naming its own inheritance:
copied from Phan's `src/Phan/Language/Internal/FunctionSignatureMap.php`,
Copyright (c) 2015 Rasmus Lerdorf, Copyright (c) 2015 Andrew Morrison, MIT.
phpstan-src itself is MIT, Copyright (c) Ondřej Mirtes and contributors.

Steins reproduces the chain in a root `NOTICE` file — Steins ← phpstan-src
← Phan — with both MIT permission notices, and the generated table carries
a provenance header naming the pinned commit. `THIRD-PARTY-LICENSES.md`
is untouched: it is generated from the cargo dependency graph and this
data never enters that graph. The playground attribution precedent
(php-wasm, PHP License 3.01) already established that non-cargo lineage
lives beside the thing it licenses.

## 5. Consequences

- `--no-php` and the pre-boot browser gain declared types for builtin
  calls with variable operands; their notices say the types come from the
  catalog's declarations, unverified.
- Where the engine answers a name, behavior is unchanged. Where it is
  silent — engine absent, extension unloaded, no declared type — the
  floor now speaks, Asserted.
- The first slice is deliberately narrower than the source. It takes
  plain functions only (method rows like `Phar::getSignature` are
  skipped) and only rows whose type lowers to an envelope (base / `?T`);
  the rows where functionMap genuinely exceeds reflection — shaped
  arrays, `T|false` unions — are dropped at generation and counted, and
  await a contracts-grade Asserted slice that seeds through the full
  `lower_str` lowering rather than `envelope_fact`. *(Closed for the
  `T|false` and scalar-refinement rows by the 2026-08-01 amendment below;
  shaped arrays and object arms stay counted-and-dropped, for a reason
  that turned out to be the countersign rather than the lowering.)*
- A new failure mode exists and is accepted: a floor row can be wrong for
  the user's actual runtime (a patched PHP, an exotic build). It can
  mislead a dump or a contracts-tier fact; it cannot mint a proof-layer
  finding. That asymmetry is the entire design.
- ADR-0056 §6 and ADR-0064 §3 carry narrowed-by notes pointing here; their
  refusals remain in force for the Verified lane.

## Amendment (2026-07-31): the ladder is PHPStan's extension stack, graded

Owner review surfaced the correspondence this floor completes. PHPStan's
dynamic return extensions receive a constant **or a union of constants**,
call the real function per member, and compose the results; an extension
that cannot meet its condition returns `null` and PHPStan falls back to
the next provider, ultimately the functionMap signature. Steins' return
ladder is the same stack with grades made explicit:

| PHPStan | Steins | grade |
|---|---|---|
| extension, constant args, calls the real thing | fold lane (sidecar) | Verified |
| argument-dependent extension | DR3 dispatch + shape transfers (ADR-0061/0062/0064) | reflection-checked |
| extension returns `null` → next provider | rung returns `None` → next rung | — |
| functionMap fallback | this ADR's Asserted floor | Asserted |

Two consequences are recorded rather than implied. First, a future
extension-porting layer is coverage growth of the DR3 rung under the
ADR-0064 taxonomy, not a new mechanism (issue #75); a ported extension
whose essence is a value question routes through the fold lane, never a
Rust reimplementation. Second, the one condition Steins cannot yet meet
is the **union of constants**: the fold gate admits a single constant
tuple only. Member-wise engine calls over a bounded product, composed to
a union and declining on any widened member, are issue #74 — they ride
the existing fold memo, the width gate, and the #64 replay loop without
new wire machinery.

**Closed 2026-08-01.** The union of constants now folds member-wise; the
mechanism, its bounds and its replayability argument are ADR-0028's
2026-08-01 amendment. The table above therefore reads across without a
caveat: the fold lane answers a constant *or a bounded union of
constants*, and every rung below it is unmoved.

## Amendment (2026-08-01): the floor stops being envelope-only

§5's "first slice is deliberately narrower than the source" named its own
follow-up, and issue #79 is it. The floor no longer lowers through
`envelope_fact`; it lowers through the **declared-return arm lane** — the
same `lower_str` → `flatten_arms` → `refine_contract_arms` path a project
function's `: string` / `@return` takes at a call site (issue #60). Three
things follow, and none of them touches the grade.

**The lowering widened; the seam did not multiply.** The #73 rung is
*replaced*, not stacked: a bare base is a trivial arm set, so the envelope
case is subsumed and `str_repeat` still answers `string`. What is new is
that a `string|false` row now has somewhere to go. The rung seeds both
carriers, exactly as the `@param` entry state does: the arm lane holds the
declaration, and the value lane holds the one fact the arms denote *where
they denote one*. A genuinely multi-arm row has no single fact — the value
domain carries no scalar-union layer — so it lives in the arm lane alone,
and the dump surface spells it through `spell_arms`, the one speller.

**The countersign widened with it, and kept its catches.** Generation-time
agreement is no longer "the row's envelope subsumes the engine's
declaration". A row is admitted when *either* it **bounds** the engine
(`engine ⊆ row`, the #73 rule verbatim — a coarse upper bound says less
than the engine but nothing false) *or* it **refines** the engine
arm-wise: every row arm under some engine arm, and every engine arm over
some row arm. The second half of that second clause is load-bearing.
Without it, "the row refines" readmits precisely the rows #73 caught — a
`string` row hiding the `null` in the engine's `?string`, an `int` row
hiding the `false` in `int|false` — because dropping an arm is
indistinguishable from sharpening one unless you insist the engine's arms
all survive. All 33 of the #73 exclusions are still excluded, and the
richer candidate population brought 14 more, the sharpest being
`imageloadfont`: functionMap still says `int|false` where PHP 8 returns a
`GdFont`.

**The numbers, re-mined at the same phpstan-src pin
(`dcde2be6`, PHP 8.5.8) so the delta is the lowering and nothing else:**

| | #73 | #79 |
|---|---|---|
| carriable by the lowering | 3,051 | 3,612 |
| — of which richer than an envelope | 0 | 561 |
| dropped: shaped arrays / lists | 388 | 388 |
| dropped: multi-base unions | 1,119 | 630 |
| dropped: scalar refinements | 74 | 2 |
| dropped: objects / resource / callable | 620 | 620 |
| dropped: void / never / mixed | 139 | 139 |
| dropped: unparseable | 304 | 304 |
| engine disagreements | 33 | 47 |
| names the engine does not know | 2,099 | 2,207 |
| **admitted** | **919** | **1,358** |
| — of which richer than an envelope | 0 | 439 |

The four unmoved drop buckets are unmoved *because* the classification is
still made on the lowered top-level shape; the union and refinement rows
that moved are exactly the population this slice was sized against. The
919 envelope admissions are preserved name for name.

**What `lower_str` did and did not cover.** It covered every scalar arm
the contract vocabulary has: the four bases, their literals, `null`, the
integer intervals (`int<lo, hi>`, `positive-int`, `non-negative-int`) and
the string predicate classes (`non-empty-string`, `non-falsy-string`,
`numeric-string`, `lowercase-string`, `uppercase-string`) — and any union
of them, which is what admitted the `T|false` family whole. Nothing was
stretched to make that happen.

It did **not** cover, and §5's deferral therefore stands for:

- **Shaped arrays (388).** `lower_str` parses them perfectly well —
  `array{a: int}`, `list<string>`, `array<string, int>` all lower. The
  blocker is downstream and it is two-part: seeding needs the shape lane
  (`to_shape_fact` / `seed_shape_fact`, which admits a *single* array
  arm), and — decisively — `normalize::subsumes` has no denotation for an
  array arm and answers `Maybe`, so the engine countersign would be
  vacuous for every one of these rows. A shape-aware acceptance relation
  is what unblocks them, not more lowering.
- **Objects, class names, `callable`, `resource`, intersections (620).**
  The same vacuity, for the same reason: `subsumes` falls to the reflexive
  is-a floor here, and steins-contract carries no hierarchy. These rows
  would enter uncountersigned, which is the one thing §3 refuses.
- **`void` / `never` / `mixed` and the `mixed`-minus cuts (139).** Nothing
  to state.
- **The 304 unparseable rows** (PHPStan-internal spellings such as
  `__benevolent<…>`, and rows with an empty return type) and the opaque
  string form, which `spell_arms` cannot spell back.
- **The 6,658 method rows**, untouched — the floor is still function-keyed.

**Everything else is unchanged and re-pinned.** Asserted stratum, per-name
engine silence, engine-wins, the A11 version gate, and the absence family's
non-consumption all carry over verbatim, and `declared_return_floor.rs`
re-asserts each of them on a rich row as well as an envelope one. The
proof-layer negative pin is extended to the `T|false` case explicitly: a
row that says a call *can* return `false` is a strictly stronger premise
than an envelope, so it is the one most worth proving the firewall against.
The two mined tables remain disjoint at this pin (all four
version-sensitive names return arrays), so the version gate's end-to-end
decline still has no fixture; the catalog test asserting that disjointness
is the tripwire that will demand one.

Naming caught up with the data: the table, its TOML and its accessors are
`declared_return` rather than `declared_envelope`, because a
`string|false` row is not an envelope in this codebase's vocabulary.

## Amendment (2026-08-01, second): the array bucket discharges

The 388-row deferral above named its own blocker precisely — "a
shape-aware acceptance relation is what unblocks them, not more lowering"
— and ADR-0071 is that relation. With `subsumes` able to answer `Yes` and
`No` about an array pair, the generation-time countersign became a real
question for a shaped row, and `arm_is_carriable` widened to the array
vocabulary (`array`, `list<T>`, `array<K, V>`, `iterable<K, V>`,
`array{…}`). Objects stay deferred for the reason §5 gave, now recorded as
ADR-0071 §2.3.

**Re-mined at the same pin (`dcde2be6`, PHP 8.5.8) so the delta is the
carriability filter and nothing else:**

| | #73 | #79 | ADR-0071 |
|---|---|---|---|
| carriable by the lowering | 3,051 | 3,612 | 4,343 |
| — of which richer than an envelope | 0 | 561 | 1,292 |
| dropped: shaped arrays / lists | 388 | 388 | **0** |
| dropped: multi-base unions | 1,119 | 630 | 287 |
| dropped: scalar refinements | 74 | 2 | 2 |
| dropped: objects / resource / callable | 620 | 620 | 620 |
| dropped: void / never / mixed | 139 | 139 | 139 |
| dropped: unparseable | 304 | 304 | 304 |
| engine disagreements | 33 | 47 | 55 |
| names the engine does not know | 2,099 | 2,207 | 2,682 |
| **admitted** | **919** | **1,358** | **1,606** |
| — of which richer than an envelope | 0 | 439 | 687 |

**What moved.** The shaped-array bucket empties outright, and the union
bucket loses the 343 rows whose only uncarriable arm was an array one
(`string|array`, `false|list<string>`) — the two halves of the same
widening. Of the 731 new candidates, 248 are admitted, 8 disagree, and 475
are names the pinned engine does not know. That last number is the one
worth a sentence, because it is the only bucket that moved for a reason
outside this slice: array-returning builtins concentrate in PECL and
legacy extensions a stock build does not load — 158 of the 475 are
`trader_*` alone, then `cairo_*` (28), `imap_*` (21), `hw_*`, `mysqlnd_*`,
`cubrid_*`, `wincache_*`, `svn_*` — so widening carriability put 731 more
names to `reflect()` and most of them simply are not there. The 2,207
previously-unknown names are all still unknown; none crossed back. The
three drop buckets the widening does not touch —
objects, void/never/mixed, unparseable — read identically across all three
runs, which is the check that the classification is still made on the same
lowered top-level shape. All 1,358 previously admitted rows are preserved
name for name and spelling for spelling.

**The catches survived, and gained.** All 47 recorded engine
disagreements are excluded verbatim — same names, same
`[row, engine]` pairs — and eight join them. They are the same shape one
vocabulary over: `ftp_raw` says `array` where PHP 8.5 declares `?array`,
so the row hides a null exactly as `xml_error_string`'s `string` hid one
under `?string`; `mysqli_fetch_row` and `locale_get_keywords` say
`null|array` and hide the engine's `false`; `str_word_count` invents
instead, still carrying a `false` arm PHP 8 replaced with a `ValueError`.
`ftp_raw` is the sharpest because it is the minimal case: two spellings of
"an array", one of which admits null, and the arm-wise clause is the only
thing between the row and the table.

**The version gate stopped being hypothetical.** §5's last paragraph
recorded that all four version-sensitive names return arrays, so the two
mined tables were disjoint and the A11 gate had no end-to-end fixture; the
catalog test asserting that disjointness was named as the tripwire. It
fired. The tables now intersect, that assertion is inverted to say so with
this ADR as the reason, and the fixture it was written to demand exists:
a project declaring 8.1 gets no `str_split` row, one declaring 8.2 does, a
range straddling 8.2 declines, and an undeclared target admits.

**Consumption needed no new seam,** as ADR-0071 §3 predicted. A
single-array-arm row reaches the value lane through `seed_shape_fact`, so
`$r = imagecolorsforindex(...)` binds a shape fact and `$r['alpha']` reads
`int<0, 127>` at `Asserted`; a multi-arm row (`string|array`) lives in the
arm lane and spells through `spell_arms`. One narrowing is deliberate and
pinned: a `?array{…}` row declines the value lane, because the floor
states one nullability rule (`fact_with_null`) and that rule refuses a
shape. The arms still carry both the shape and the null, so the decline
costs the shape-lane consumer and nothing else.

**One residual is a widening, named here so it is a decision and not an
accident.** Carriability is a *top-level* check and the pinned engine
only ever declares `array`, so eleven of the new rows carry an element
claim the countersign never examined — `get_declared_classes` =
`list<class-string>`, `get_resources` = `array<int, resource>`,
`debug_backtrace`'s shape among them. The countersign vouches for the
array-ness; the element type is functionMap's word alone, which is the
Asserted grade stated honestly rather than a breach of §3 — and the fact
lane cannot over-consume it (`to_fact` returns `None` for those leaves,
so the seeded shape carries an untyped tail). Two rows
(`debug_backtrace`, `get_defined_functions`) additionally store their
raw source spelling because `spell_arms` refuses to spell them back;
they countersign and lower correctly but the dump surface declines them,
so they are inert there — carried for the arm lane, not the renderer. A
future slice that wants element-level countersigning knows where to
stand: it is the same sidecar `reflect` extension the object bucket
needs.

## Amendment (2026-08-01, third): the object half rides the reflexive floor

The 620-row bucket the two amendments above kept deferring was never one
population, and splitting it is most of this slice's result. ADR-0071 §2.3
designed the object half's admission and predicted it would need no new
relation; it did not. `subsumes_class` has been reflexive since N1, and
reflexivity is exactly the question a functionMap row poses — the row says
`GdFont`, the engine says `GdFont`, `GdFont ⊆ GdFont` answers `Yes` in both
directions, and the arm-wise clause closes. The only change is
`arm_is_carriable`, widened to `Class` and `ObjectAny`, per arm, so
`?ClassName` follows by composition.

**The asymmetry is the whole design, not a limitation of it.** A row naming a
*different* class leaves `subsumes_class` at `Maybe`, which is not `Yes`, so it
is refused and listed. The floor therefore only ever admits a name the engine
itself spelled, and it never states a hierarchy claim it has no hierarchy to
check. One direction does resolve without a hierarchy and is admitted: a class
row under a bare `object` declaration, the class analogue of `non-empty-string`
under `string`, decided by the universal every-instance-is-an-object rule.

**Re-mined at the same pin (`dcde2be6`, PHP 8.5.8) so the delta is the
carriability filter and nothing else:**

| | #73 | #79 | ADR-0071 | objects |
|---|---|---|---|---|
| carriable by the lowering | 3,051 | 3,612 | 4,343 | 4,576 |
| — of which richer than an envelope | 0 | 561 | 1,292 | 1,525 |
| dropped: shaped arrays / lists | 388 | 388 | 0 | 0 |
| dropped: multi-base unions | 1,119 | 630 | 287 | 200 |
| dropped: scalar refinements | 74 | 2 | 2 | 2 |
| dropped: objects / resource / callable | 620 | 620 | 620 | **474** |
| dropped: void / never / mixed | 139 | 139 | 139 | 139 |
| dropped: unparseable | 304 | 304 | 304 | 304 |
| engine disagreements | 33 | 47 | 55 | 75 |
| names the engine does not know | 2,099 | 2,207 | 2,682 | 2,793 |
| **admitted** | **919** | **1,358** | **1,606** | **1,708** |
| — of which richer than an envelope | 0 | 439 | 687 | 789 |

**What moved, and what the bucket actually held.** 146 rows leave the object
bucket and 87 leave the union bucket — the two halves of one widening, exactly
as the array slice's 388 and 343 were. Of those 233 new candidates, 102 are
admitted, 20 disagree, and 111 are names the pinned engine does not know
(`ast\*`, `cubrid_*`, the `gmp_*` and `tidy_*` families in a build without those
extensions). The refinement, void and unparseable buckets read identically across
all four runs, which is the check that the classification is still made on the
same lowered top-level shape.

The 474 that remain deserve their composition written down, because the bucket's
label has been overselling what it holds since #73: they are 322 `void`, 149
`resource`, 2 `Closure` and 1 `int-mask<…>`. `void` lowers to an opaque arm
rather than to a value type, so it lands here and not in the void/never/mixed
bucket beside it. The counts are left as they are — they are the comparison
series this table is built on, and moving a row between buckets for a naming
reason would make the columns incomparable — but the real object deferral is now
`resource` and `callable` alone, and it is smaller than 474 suggests.

**The catches survived, and the new ones are the value demonstration.** All 55
recorded disagreements are excluded verbatim — same names, same `[row, engine]`
pairs — and twenty join them. `stream_bucket_make_writeable` is the sharpest in
the table so far: functionMap says the call returns a bare `stdClass` where PHP 8
declares a real `StreamBucket`. That is the resource era's rot in its purest
form — a stand-in type that outlived the thing it stood in for — and nothing
about it is subtle to the countersign, because the two names simply differ. The
rest are the familiar dropped-arm shape wearing class names:
`intlcal_create_instance` and the four `tidy_get_*` rows hide the engine's `null`
exactly as `ftp_raw` hid one, `xmlwriter_open_uri` hides its `false`, and
`dom_import_simplexml` both drops the engine's `DOMAttr` arm and invents a
`false`. None of them could have been caught before, because none of them was a
candidate.

The `resource` rows are the same demonstration from the other side. They are
*not* class arms — `resource`, `open-resource` and `closed-resource` are
`KNOWN_UNENFORCED` keywords lowering to an opaque arm — so the 149 stale rows
where functionMap still says `resource` and PHP 8 returns a `GdImage` or a
`CurlHandle` stay uncarriable and counted, never admitted. The widening reaches
the rows the engine agrees with and leaves the rot where it was.

One refusal is worth naming because it is load-bearing rather than incidental. A
*constant* name is not vocabulary, so `lower_identifier`'s catch-all lowers
`JSON_ERROR_NONE` to a `Class` arm, and the object slice made class arms
carriable — which means `json_last_error` and `session_status` became candidates
spelling a union of constants as a union of classes. The countersign is the only
thing that keeps them out, and it does: the engine declares `int`, no class name
matches, and both rows are listed. The `engine_typeless` count is unchanged at 22,
so no new row was admitted down the uncountersigned path.

**Consumption needed no new seam, and the value lane needed nothing at all.** A
class row is *arm-lane only*, unconditionally — the value domain has no object
inhabitant (ADR-0035/0038), so `contractty_to_fact` and `to_shape_fact` both
decline and `floor_value_fact` returns `None` by two independent routes. That is
a stronger statement than the Asserted grade: the firewall keeps Asserted facts
out of proof premises, and here there is no fact to keep out. The arm lane in
exchange does real work — `?Collator` renders its null arm and a `!== null` guard
subtracts it. The `T|false` rows lacked the analogous leg while arm subtraction
was instanceof-driven; ADR-0052's 2026-08-01 note wired the `Value` subtrahend,
so `!== false` now strips the false arm of these rows the same way.

`builtin_return_floor`'s identity resolver was previously correct because no class
arm could reach it; it is now correct for a reason worth stating.
`refine_declared_arms`' resolver exists to qualify a *relative* class name against
a declaring namespace, and a functionMap row has no declaring namespace: every
class it names is a global builtin FQN, `ast\Node` included. A project-namespace
resolver would mangle them, so the identity is the only correct choice, not merely
a sufficient one.

**Two residuals, named so they are decisions.** First, `spell_arms` has no
faithful spelling for a class arm, so all 246 class-bearing rows store
functionMap's own string rather than a canonical respelling — which is the better
outcome, since `ContractTy::Class` case-folds and could not restate `GdFont`. Three
of those strings are PHPStan's `__benevolent<…>`, which the phpdoc parser expands
to the plain union it wraps before anything lowers it. Rows whose arms mix a class
with a non-class (`SimpleXMLElement|false`, bare `object`) are inert on the *dump
surface* — `render_contract_arms` spells a pure class/`null` list and refuses
anything else — while remaining fully live in the arm lane, the same posture the
array slice recorded for its two unspellable rows.

Second, and user-visible: a class row renders **lowercased**. `ContractTy::Class`
normalizes on the way in, which is what makes the countersign's `class_eq`
comparison work, and `Cx::class_display_fqn` recovers source casing from the
*project* index, which knows nothing about a builtin — so `dumpType(gmp_init($x))`
reads `gmp` where PHPStan reads `GMP`. The hierarchy catalog cannot answer it
either, since it keys on the lowercased name. Closing this needs a builtin-class
display-name table of its own; until then it is a fidelity gap in the dump surface
and nowhere else, because every judgment downstream compares through the
case-insensitive `class_eq`.

**Closed 2026-08-02.** The display-name table landed beside the hierarchy
catalog: `cargo xtask gen-catalog` now emits `display_names_generated.rs` from
the same `hierarchy.toml` pin — lowercased key → the casing php-src declares,
**enums included**, since the hierarchy table's enum exclusion guards the is-a
oracle against an incomplete super-edge set and a display name has no such
soundness gate. `Cx::class_display_fqn` consults it
(`steins_catalog::builtin_class_display`) exactly where the project index
misses, and only for a name no project file declares at all (`class_absent`,
so an ambiguous project name keeps issue #67 precedence and never reads the
catalog). `dumpType(gmp_init($x))` now reads `GMP`, `hash_init` renders
`HashContext`, `collator_create` renders `null|Collator`. The judgment half of
the residual's statement is unchanged and is the point: no judgment consults
the table, everything downstream still compares through `class_eq`, and the
pinning test moved from
`a_class_row_renders_lowercased_because_nothing_holds_the_builtins_casing` to
`a_class_row_renders_the_casing_php_src_declares`.

**What stays out**, and it is the same stand the element-level residual takes:
`callable` and the intersections, because a reflexive floor says nothing about a
signature and their countersign would still be vacuous; `resource`, because it is
not a class at all; and every genuinely *hierarchy-dependent* row — a functionMap
row naming a subclass or a superclass of what the engine declares stays `Maybe`
and is refused in both directions. Deciding those needs a real is-a oracle at
generation time, which is the same sidecar `reflect` extension element-level
countersigning needs. One extension, two deferrals, and both now have their
numbers written down.

## Note (2026-08-17): parameters stay out of the floor

ADR-0056 §9 gave builtins a **parameter** surface, read off the engine's own
`ReflectionFunction::getParameters()`. It does not reach this table, and the
asymmetry is §2's firewall rather than an ordering of work.

A declared *return* is consumed by the dump surface and by contracts-tier
reasoning, where an Asserted row is exactly the right grade: wrong, it costs
precision. A declared *parameter* is consumed by the proof layer — it premises
`type.argument-mismatch` on the default surface — where a wrong row costs a
false positive on green code. To be useful in the argument direction a row
would have to enter Verified, and no functionMap row may (§2, first bullet);
to be admissible here it would have to stay Asserted, where the proof layer
cannot cite it by construction and it would judge nothing at all. That is not
a gap to be closed later: it is the same reasoning that put the floor in the
Asserted lane in the first place, read in the other direction.

So the builtin parameter surface is engine-only. `--no-php`, the pre-boot
browser and a spawn failure answer `None` and judge no builtin argument —
which is the sound subset (ADR-0004), and which keeps `steins check` and
`steins check --no-php` differing in *reach* and never in *verdict*.
ADR-0069's scope is unchanged: returns, by name, Asserted.
