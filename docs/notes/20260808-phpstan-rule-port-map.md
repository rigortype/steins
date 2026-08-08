# PHPStan rule-port map — what is worth porting, and in what order

The [cross-check note](20260725-phpstan-cross-check.md) established that
PHPStan and Steins agree where they overlap, and named four recall gaps. It
did not answer the adoption question underneath: **a team that uses PHPStan
today — what would have to exist in Steins before they could stop?**

This note answers that by enumerating PHPStan's actual rule inventory,
measuring which of it fires on real code, and mapping each surviving class
onto the Steins machinery that would carry it. It is a *port map*, not a
plan: sequencing belongs to [`docs/ROADMAP.md`](../ROADMAP.md), and every
layer assignment proposed here is a proposal until an ADR takes it.

Measured against phpstan-src `55a7732` (2026-07-31) and Steins `eb31178`
(v0.1.3, 2026-08-05, 29 registered ids).

## Method

Three sources, deliberately independent:

1. **The inventory.** `src/Rules/**/*Rule.php` in phpstan-src — 357 rule
   classes, each carrying a `#[RegisteredRule(level: N)]` attribute, and
   ~500 distinct `identifier('…')` strings across the tree. Level
   attribution is read from the attribute, not from the `conf/` neons
   (which now only carry parameters and conditionally-tagged services).
2. **The frequency.** The fourteen held-out applications from the
   [adoption drill](20260724-adoption-drill-record.md), re-bucketed **by
   identifier** out of the raw PHPStan JSON the cross-check harness already
   produced (`~/repo/php/steins-survey/.phpstan-survey/out/*.json`, level 2,
   per-app `phpVersion` and bootstrap).
3. **A hand probe.** One 26-line file exercising each candidate rule once,
   run through both tools, to confirm from the tree rather than from the
   docs what Steins is silent about today.

The probe result, stated plainly: **PHPStan reports 19 findings, Steins
reports 1** (the `new`-typed receiver's `call.undefined-method`). Every
other line is silence.

## The inventory collapses under measurement

At level 2 over the fourteen applications, **86 of PHPStan's ~500
identifiers ever fire**, and **nine of them cover 95%** of the 49,032
findings; the top ten cover 96.2%. Porting "PHPStan's rules" is not a
357-rule project. It is roughly a twenty-id project, and the twenty are
knowable.

Two caveats bind everything below.

**Volume is not signal.** The cross-check note's §5 already showed that
most of PHPStan's count is setup quality and framework magic — MISP's
unchecked-out CakePHP 2 core is most of `class.notFound`, Eloquent and
facades are most of `property.notFound`, and larastan erases 60% of
firefly-iii's sites. The **breadth** column (how many of the fourteen apps
show the id at all) is the more robust ranking key, and rules whose volume
is insensitive to framework extensions — arity, undefined variables,
docblock hygiene — rank higher than their counts suggest.

**This is a level-2 view.** Level 3–6 ids (`argument.type`, the
constant-condition family, `missingType.*`) are absent from the table
because the harness never ran them, not because they are rare.

| PHPStan id | n | apps | Steins id today |
| --- | ---: | ---: | --- |
| `property.notFound` | 15,554 | 12 | — |
| `method.notFound` | 9,614 | 14 | `call.undefined-method` |
| `class.notFound` | 9,072 | 12 | `class.undefined` (4 positions) |
| `staticMethod.notFound` | 4,363 | 9 | `call.undefined-method` (static leg) |
| `function.notFound` | 4,211 | 6 | `call.undefined-function` |
| `constant.notFound` | 1,561 | 5 | — |
| `varTag.nativeType` | 1,167 | 9 | — (cf. issue #35) |
| `variable.undefined` | 851 | 9 | — |
| `property.nonObject` | 442 | 4 | — |
| `arguments.count` | 203 | 11 | `call.too-few-arguments` |
| `phpDoc.parseError` | 191 | 7 | — |
| `parameter.notFound` | 145 | 6 | — |
| `return.missing` | 103 | 6 | — |
| `method.nonObject` | 94 | 9 | `call.on-null` (null slice only) |
| `array.duplicateKey` | 79 | 4 | — |
| `throws.notThrowable` | 62 | 6 | — |
| `empty` / `isset` / `nullCoalesce.variable` | 80 | 5 | — |
| `parameter.phpDocType` | 42 | 8 | — |
| `varTag.variableNotFound` / `.differentVariable` | 67 | 6 | — |
| `closure.unusedUse` | 35 | 4 | — |
| `binaryOp.invalid` | 23 | 7 | — |
| `property.private` / `method.private` / `staticClassAccess.private*` | 48 | 4 | — (computed, used only to silence) |
| `classConstant.notFound` | 10 | 3 | — |

## The precondition: member reach — and what it actually is

The [cross-check note](20260725-phpstan-cross-check.md) §3 recorded that
member checks reach only `new`-typed and static receivers, and this note's
first draft repeated it as an inference gap. **Re-probed against
`eb31178`, that is not what is happening.** The correction matters, because
it changes what the work is.

The ADR-0049 §8 declared-receiver lane (S6) **exists and reaches**
native parameter declarations, `@param` docblocks, `@var`-annotated locals
and union declarations with per-arm descendant closure. It is simply not on
the default surface:

```php
function nativeParam(C $o): void { $o->nope(); }   // reported --profile contracts
/** @param C $o */
function docParam($o): void { $o->nope(); }        // reported --profile contracts
function viaNew(): void { (new C())->nope(); }     // reported at default
```

All three report. The first two carry `phpdoc.undefined-method`, which
lives on the **contract** layer at the `Contracts` floor, so a bare
`steins check` prints nothing — and a bare `steins check` is what the
cross-check measured. The 14-of-14-versus-1 gap is therefore a **floor and
stratum question**, not missing inference.

That reframes it into four separable pieces, in descending leverage.

> **Status 2026-08-08 (issue #196).** Piece 1 landed as ADR-0049 A13's
> minimum-stratum routing, with the copied-variable half of piece 2. Piece 4
> **dissolved**: once the routing is in, `phpdoc.undefined-method` fires only
> where a docblock premise participates, so the name is correct and no rename
> or deprecation path is needed. The remaining piece-2 shapes and piece 3 are
> still open — see the per-piece notes below.

**1. The Verified half of S6 is proof-grade evidence shipped as contract.**
The lane's own code comment says it accepts Asserted premises because the
arms *may* be Asserted (`@param`). A **native** `C $o` produces a
**Verified** arm, and the ladder it passes is the same one S2 uses — chain
closure, no `__call`, descendant closure or `final` immunity, dam clear,
A9 sidecar, A11 version-skew demotion. Splitting the lane by *arm stratum*
— all-Verified arms to a proof-layer id at the `Default` floor, any
Asserted arm staying on `phpdoc.undefined-method` — puts `method.notFound`
on the default surface for exactly the shapes a PHPStan user expects, with
**no new inference and no new closure conditions**. ADR-0052 N2's
minimum-stratum rule already computes the input this needs.

**2. The receiver-kind gate.** `phpdoc_undefined_method_receiver` admits
only `Receiver::Var` — a plain `$var->m()`. Measured misses: a property
receiver (`$this->prop->m()`), a promoted-property receiver, a
return-typed call receiver (`mk()->m()`), and a parameter copied into
another variable (`$c = $o; $c->m()` — silent, though the `@var` spelling
of the same thing works). These are the genuine reach gaps, and they are
about propagating a declared fact to more receiver positions, not about
building a lane.

**3. Nothing else consumes the lane.** `call.too-few-arguments` on a
declared receiver is silent (probed), and so is everything P1/P2 would
add. Whatever S6 knows about a receiver, only S6 uses.

**4. The id is misnamed.** `phpdoc.undefined-method` fires on
`function f(C $o)` where no docblock exists. Under ADR-0022 an id is a
contract, so this is a rename with a deprecation path, not a typo fix —
worth folding into piece 1 rather than doing twice. *(Superseded: piece 1's
routing sends that case to `call.undefined-method`, which leaves the
`phpdoc.` id firing only on docblock premises. Nothing to rename.)*

The false-positive guard is unchanged by all of this and still comes
first: `@method`, `@property`, `@mixin` and `@phpstan-type` are listed in
[not-implemented.md](../type-specification/not-implemented.md) as tags
*not read*. An unread `@method` on a framework base class is a
false-positive generator the moment any of this reaches the default
surface. Reading them as **obstacles** (silence) is far cheaper than
reading them as member sources, and is the minimum piece 1 needs before
its corpus measurement means anything.

## The posture every port inherits

Restated by the owner while this slate was being cut, because the first
draft of these issues drifted from it: **zero-FP is not blanket silence
toward anything that could possibly be an FP.** It is: detect every cause
of unsoundness as far as possible, then absorb each project's FP tolerance
through the calibration channels —

- **the plugin lane** (ADR-0039/0044/0045) is where metaprogramming gets
  *declared*: what a framework's `__call`/`@method` magic actually provides
  enters through the manifest and packs, discharging the obstacle;
- **ignore / baseline / triage** absorb what a team, at its test coverage
  and maturity, should not be distracted by — that is what the suppression
  channels are *for*;
- **the strict floor** carries the possibly-grade legs, measurement-first
  (the `offset.missing` / `offset.maybe-missing` pair is the precedent;
  issues #50/#51 are the ruling).

Three consequences for every issue in this slate. A "Maybe ⇒ silence"
criterion describes the **default floor**, never the family's end state —
each family should name its possibly-grade sibling behind the strict
surface, even when that sibling is deferred. An obstacle leg (`__call`,
`@method`/`@mixin`, dams) is default calibration whose designed discharge
channel is the plugin lane — issues and ADRs are written so the obstacle
is *dischargeable*, not terminal. And discharge machinery must land before
or with the strict tier that needs it, or the tier cries wolf on
correctly-guarded code.

## Port classes, ranked

Ranked by (breadth × machinery already present ÷ new foundation required).
Each names the PHPStan rule, the runtime consequence, and the Steins
machinery it would ride.

### P1 — the absence family's missing members

`property.notFound` (12 apps), `classConstant.notFound` (3),
`constant.notFound` (5). PHPStan's `AccessPropertiesRule`,
`ClassConstantRule`, `ConstantRule`.

The ADR-0049 ladder is already built for methods: complete hierarchy
enumeration, magic-method obstacle, dam check, boot-surface reflect. These
three are the same ladder over a different member kind — the obstacle for
properties is `__get`/`__set` rather than `__call`, and global constants
have no hierarchy at all, only the sidecar's existence oracle (which
already answers for functions and class-likes).

**Layer is not uniform here, and the note should not pretend it is.** An
undefined class constant is a fatal `Error`; a `define()`-less constant
fetch is a fatal `Error` since PHP 8.0; but *reading* an undefined property
is a PHP 8 `Warning` evaluating to `null`. Under the proof layer's
definition — runtime breakage — the first two qualify outright and the
third is a layer decision, not a mechanical one. The honest options are
proof (a warning is breakage) or contract; what is not defensible is
inventing a fourth layer for it.

### P2 — visibility, which is already computed and thrown away

`method.private` / `method.protected`, `property.private` /
`property.protected`, `staticClassAccess.private*`. Four apps, ~48 sites.

`private_blocked()` in `crates/steins-infer/src/lib.rs:15389` resolves
visibility today and uses it to **suppress** a finding: an out-of-scope
private method resolves to `None`, and the call goes unchecked rather than
reported. Calling a private method from outside its declaring class is a
fatal `Error` — a proof-layer finding by any reading, and the smallest
port in this note, because the predicate exists and only its consumer is
missing.

### P3 — `class.undefined` beyond the four hard-error positions

`class.notFound` (12 apps). Steins collects class references at exactly
four positions — `new X`, `X::m()`, `X::CONST`, `X::$prop`
(`hard_class_refs`, ADR-0049 §5). PHPStan additionally reads:

| Position | PHP consequence | Suggested layer |
| --- | --- | --- |
| `extends` / `implements` / `use` trait | fatal at class load | proof |
| `catch (X $e)` | never matches — silent wrong behaviour | proof |
| param / return / property typehint | `TypeError` on the first typed call | proof |
| `instanceof X` | evaluates `false`, no error | contract |
| `@param` / `@return` / `@var` naming a missing class | none (annotation) | contract (`parameter.phpDocType`, 8 apps) |

The collection side is a widening of one lowering walk; the proof side is
the existing `check_undefined_class` unchanged. This is the highest
value-to-effort ratio in the note, and it is independent of member reach.

### P4 — the undefined-variable family

`variable.undefined` (9 apps, 851), plus `isset.variable`,
`empty.variable`, `nullCoalesce.variable` (5 apps). PHPStan's
`DefinedVariableRule` and the level-1 isset/empty/coalesce trio.

Framework-insensitive — no extension erases these — which makes 851 a
truer number than `property.notFound`'s 15,554. Reading an undefined
variable is a PHP 8 `Warning` evaluating to `null`, so it shares P1's layer
question.

The provable subset is an ADR-0049-shaped absence proof and needs no
reachability analysis: a name read in a scope where it never appears as an
assignment target, parameter, `global`, `static`, `use`, by-ref
out-parameter or `foreach` binding, **and** where the scope carries no
`extract()`, `compact()`, variable-variable, `eval` or out-of-universe
`include`. The last clause is the ADR-0046 dam, which already exists and
already gates the absence family. The imprecise half — a variable assigned
on only *some* paths — needs the reachability foundation in P7 and should
not be attempted with it.

### P5 — declaration-incompatibility fatals

`method.abstract` (a non-abstract class carrying an inherited abstract
method), `class.extendsFinal`, `method.final` (overriding a final method),
static/non-static mismatch, weakened visibility, and the LSP variance pair
`method.childParameterType` / `method.childReturnType`. PHPStan's
`OverridingMethodRule`, `MissingMethodImplementationRule`,
`ExistingClassInClassExtendsRule`.

The cross-check note already flagged this family as real defects Steins has
no check for — Sylius's three Doctrine test doubles, omeka-s's
`FallbackRenderer::render()`, pixelfed's `BearerTokenResponse`. Every one
is a **fatal at class load**, which makes them the purest proof-layer
findings available: no flow analysis, no value domain, no receiver binding
— only the declaration graph Steins already builds for `resolve_in_chain`,
plus the arm-wise acceptance relation it already has for the variance pair.

Low frequency in the survey (PHPStan's own workers die on these, so they
under-report), high severity, and directly on the legacy-modernization
path.

### P6 — docblock hygiene as mechanics

`phpDoc.parseError` (7 apps), `parameter.notFound` (`@param` naming a
parameter that does not exist, 6 apps), `throws.notThrowable` (6),
`varTag.variableNotFound` / `varTag.differentVariable` /
`varTag.misplaced` (6/5/4), `closure.unusedUse` (4).

These are anti-rot findings about annotations that have drifted from the
code they annotate — precisely what the **mechanics** layer exists for
(red on sight, suppression-exempt, `disable`-proof). Steins has a full
PHPDoc parser and, since ADR-0073, an inline-`@var` consumer; what is
missing is the check that the tag's *subject* still exists. Cheap, broad,
and the FP risk is near zero because the premise is textual, not inferred.

### P7 — reachability, and `return.missing`

`return.missing` (6 apps, 103). Falling off the end of a function with a
non-void declared return type is a fatal `TypeError`.

`not-implemented.md` records the blocker honestly: *no reachability
analysis*. This port is therefore a foundation build, not a rule port —
which is also its argument, because the same foundation carries the level-4
dead-code family (`UnreachableStatementRule`, `CatchWithUnthrownException`,
the unused-private trio) and sharpens every existing check that currently
reasons over statements it cannot prove are live.

Sequence it as a foundation with `return.missing` as its tracer, not as an
item in the rule slate.

### P8 — value-domain consumers

`binaryOp.invalid` (7 apps), `method.nonObject` (9) / `property.nonObject`
(4), `foreach.nonIterable`, `encapsedStringPart.nonString`. PHPStan's
`InvalidBinaryOperationRule`, `NullsafeMethodCallRule`,
`IterableInForeachRule`, `InvalidPartOfEncapsedStringRule`.

Steins' `call.on-null` is the **null slice** of `method.nonObject`. In PHP
8 a method call on an `int`, `string` or `array` is the same fatal `Error`
as on `null`, and `[] + 1` is a fatal `TypeError`. The four-layer value
domain already proves "definitely not an object" and "definitely an array"
— these ports are consumers of facts already computed, in the same shape
`call.on-null` established. `foreach` over a proven non-iterable is the
same move against ADR-0062's array facts.

Self-contained members of the same class, worth naming separately because
each is small: `array.duplicateKey` (4 apps — literal arrays only, no
inference at all), `argument.printf` (a folded literal format string
against the argument count; Steins folds and has the builtin catalog),
`regexp.pattern` (a literal pattern that PCRE refuses — the pattern reader
landed for issue #148/#177 and would carry it nearly free).

### P9 — the level-6 surface, as contract-layer debt

`missingType.parameter` / `.return` / `.property` / `.iterableValue` /
`.generics`. Unmeasured here (the harness ran level 2), but level 6 is the
level a large share of PHPStan shops actually target, and "how much
untyped surface do I still have" is the question a modernization project
asks first.

The machinery is declaration reading only. The contract layer is the
natural home — this is debt reporting, opt-in, exactly the crying-wolf
posture ADR-0050 fixed. Worth stating the boundary out loud, since
"missing typehint" reads like a lint rule and Steins refuses lint: the
distinction is that a missing type is a *claim the code does not make*,
which the contract layer reports, whereas a lint rule is a claim about
style, which no layer reports.

## Prerequisites that are not rule ports

Three items block the value of everything above and are already recorded
in the cross-check note. Both of the first two still reproduce against
`eb31178`:

1. **Symlinked duplicates suppress declaration-dependent findings.**
   `steins check src` reports the arity error; `steins check src mirror`
   (where `mirror/src` is a directory symlink) reports nothing. Every
   port in P1–P5 is declaration-dependent and inherits this. Canonicalise
   paths before dedup in `collect_php_files`.
2. **Syntax errors pass silently** — a file `php -l` rejects analyses to
   `exit 0` with no diagnostic. A checker that accepts broken PHP without
   comment is not adoptable whatever its rule count, and the second half
   of the question (may a recovered tree carry proof-grade findings?) is an
   ADR, not a patch.
3. **`is_vendor_path` is a `vendor` literal** — 68% of nextcloud's contract
   findings are vendored third-party code under `3rdparty/`. Any port that
   raises finding volume multiplies this error.

## Not worth porting

Recorded so a later reader does not file them as gaps:

- **The constant-condition and dead-code family** (level 4:
  `IfConstantConditionRule` and its ~18 siblings, `ImpossibleInstanceOf`,
  `StrictComparisonOfDifferentTypes`). These are the ids an application
  needs a baseline to live with, and PHPStan gates several behind feature
  toggles. A *provable* subset exists (an `instanceof` refuted by a fully
  enumerated final hierarchy) and could ship at the `strict` floor, but not
  before P1–P5, and not as a family.
- **Levels 7–10** (`reportMaybes`, `checkNullables`, `checkExplicitMixed`,
  `checkImplicitMixed`). The numeric ladder is a recorded refusal, and
  Steins answers the same question with the Certainty trinary plus named
  stages. Porting the *rules* is meaningless; the strictness they encode is
  already spelled differently.
- **`src/Rules/Api/*`** (16 rules) — PHPStan's own backward-compatibility
  guard for extension authors. Not a PHP check.
- **`Whitespace/FileWhitespaceRule`, `DeclareStrictTypesRule`,
  `Names/UsedNamesRule`** — lint, refused by ADR.
- **`TooWideTypehints/*`** — a genuinely interesting family, but it belongs
  to the transform engine (a too-wide return type is a `phpdoc-honesty`
  candidate), not to `check`.
- **`Generics/*` beyond the ancestor checks** — blocked on ADR-0032 carry,
  which is already tracked (issue #10). Not a separate port.

## Follow-up

Ticketed as issues #179–#200 on 2026-08-08, ordered by dependency rather
than by value: P3, P5, P6, P8 and the prerequisites are independent of
member reach; P1, P2 and the arity checks are not.

| | Slice | Issue |
| --- | --- | --- |
| | **Prerequisites** | |
| | Canonicalise paths in `collect_php_files` | #179 |
| | Surface parse errors + the recovered-tree ADR | #180 |
| | `is_vendor_path` beyond the `vendor` literal | #181 |
| | **Independent of member reach** | |
| P3 | `class.undefined` at the declaration positions | #182 |
| P5 | Declaration-incompatibility tracer (`method.abstract`, `class.extendsFinal`) | #183 |
| P5 | The overriding family, incl. the LSP variance pair | #184 |
| P2 | Visibility findings from the existing predicate | #185 |
| P6 | Docblock hygiene as mechanics ids | #186 |
| P8 | `array.duplicateKey` | #187 |
| P8 | printf/sprintf arity from a folded format string | #188 |
| P8 | `regexp.pattern` on the landed pattern reader | #189 |
| P8 | `call.on-null` widened to a proven non-object | #190 |
| P8 | `binaryOp.invalid` | #191 |
| P8 | `foreach.nonIterable` | #192 |
| P8 | String-context conversions PHP 8 refuses | #193 |
| P4 | The undefined-variable family, dam-gated | #194 |
| P1 | `constant.notFound` (global — no receiver) | #198 |
| | **The member-reach chain** | |
| | `@method` / `@property` / `@mixin` as silence obstacles | #195 |
| | The declaration-typed receiver lane (ADR + tracer) | #196 |
| P1 | `property.notFound`, `classConstant.notFound` | #197 |
| | **Foundations and other axes** | |
| P7 | Reachability, tracer `return.missing` | #199 |
| P9 | The level-6 `missingType.*` contract surface | #200 |

**Two corrections to this note's own first draft**, both found by
re-probing the tree rather than trusting the July cross-check:

- The warning-versus-fatal layer question raised against #189, #192, #194
  and #197 is **already decided**. ADR-0049 §7 rules that *a proven
  `E_WARNING` is proof-layer reportable*, and its amendment makes the
  posture a pseudo-constant: `[runtime] warning-handler = "abort" |
  "null"`, defaulting to `"abort"`, **implemented** as
  `warning_handler_abort`. Warning-grade ids go to proof under the default
  posture and demote (not vanish) under `"null"`. Those issues should
  follow the `offset.missing` precedent, not re-open the question.
- Member reach is **largely built** — see the section above. #196 was
  filed on the first draft's premise and has been corrected in place.

## Design session outcome (2026-08-08)

The grilled design session over this note settled the open questions.
The binding records are the drafts (all PENDING ratification):

- **[ADR-0078](../adr/0078-member-kind-diagnostic-families.md)** — the
  member-kind family axis, every port id's spelling/layer/floor, the
  `maybe-` sibling and gate-boundary conventions, and the named
  deferrals (guard trio, dynamic-write, contract twins). One row
  (`type.return-missing`) was added beyond the approved table and is
  flagged there.
- **[ADR-0079](../adr/0079-parse-failure-dam.md)** — `syntax.unparsable`
  (mechanics) plus `DamKind::Unparsable`: a non-vendor unparsable file
  dams the absence family universe-wide, with the vendor presumption
  carried over and position-aware refinement deferred-with-design.
- **ADR-0049 A13/A14** (amendment in place) — the declared-receiver lane
  routes by minimum stratum (all-Verified → `call.undefined-method`, no
  rename needed), and obstacle legs become per-site dischargeable
  records for the plugin lane.

Issues #180 and #182–#200 carry the settled ids in their bodies;
`CONTEXT.md` gained the session's four terms.
