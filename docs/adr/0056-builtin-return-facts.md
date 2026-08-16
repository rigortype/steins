# Builtin return facts: reflected envelope, curated refinement within it

The nsrt harness (ADR-0053's assertType oracle, 15,513 assertions) ranks
Steins' inference reach gaps, and the top non-structural classes share one
root: builtin call returns render `unknown` where PHPStan asserts a
concrete type — bool predicates in value position (741 differs:
`array_key_exists`, `is_*`, `str_contains`), scalar unions (970: `abs`
and arithmetic-adjacent builtins), plain string returns (893), refined
strings (675: `non-empty-string` producers), int ranges (546: `count()`,
`strlen()` ≥ 0), plain ints (575). The catalog today tells inference
about builtins only what folding, effects, throws, failure arms, and the
class hierarchy need — **no return facts** — so an unfoldable or
non-literal-arg call yields no value fact, which is correct silence, and
every downstream assertion sees `unknown`. This ADR gives builtins a
return-fact surface without breaking the two rules that made the silence
correct: ask-the-real-thing (ADR-0004/0024 — no recalled
version-dependent signatures) and zero-FP (a wrong return fact could
premise a proof finding).

## 1. Two sources, one precedence rule: refinement within a runtime-confirmed envelope

Neither candidate source suffices alone. Sidecar reflection
(`reflect(target)`, the existing ADR-0024 method — no new protocol
surface) reports the return type the **project's own PHP** was built
with: real, version-correct, extension-aware — and coarse
(`string|false`, never `non-empty-string`, never `int<0,max>`).
Curation can carry PHPStan-grade precision — and is exactly the
hand-maintained function map ADR-0014 warns rots silently, with 8.x
minor drift (functions gaining `false|string` arms, deprecations) as
the live failure mode. The decision is **both, with reflection as the
authority and curation as a bounded refiner**:

1. **The reflected envelope is the base fact.** For a called builtin,
   the engine asks the sidecar (once per name per run, cached) for the
   reflected return type and seeds the call's value fact with its
   lowering — `General{Base}` floors and their unions. This is a native
   declaration read off the running engine's own arginfo: it enters at
   the **Verified** stratum (the existing "native declaration seed"
   clause of the stratum doc), and it is immune to version skew by
   construction — the runtime answered for itself. Reflection detail:
   the runner consults `getReturnType()` and, when null,
   `getTentativeReturnType()` (still the engine's own claim for its own
   builtins); a function reporting neither yields no envelope.
2. **Curated facts refine strictly within that envelope.** A curated
   catalog entry may say `count(): int<0,max>` or
   `sha1(): non-empty-string` — Refined-layer facts (ADR-0035) the
   engine's type system cannot express. A curated fact is consumed
   **only after an extensional subset check against the reflected
   envelope** (the existing acceptance relation, ADR-0030 core):
   curated ⊆ reflected, or the curated fact is discarded and the
   envelope stands alone. Curation may narrow; it may never widen or
   contradict — so a stale curated row can lose precision, never
   manufacture a wrong premise from a type the real runtime disowns.
   This is ADR-0014's continuous sidecar audit turned from a batch
   staleness detector into a per-fact admission gate, and it is the
   shape that satisfies ask-the-real-thing: the real thing draws the
   boundary; curation only sharpens inside it.

Precedence is therefore not a tie-break but a composition: envelope
always; refinement when admitted. `strpos` composes with ADR-0042
unchanged — the envelope carries `int|false`, curation refines the int
arm to `int<0,max>`, and the failure/sentinel classification stays the
sole authority on what the `false` arm *means*.

## 2. Certainty and the version-skew guard

**No new stratum.** The N2 machinery (ADR-0052 §5) has exactly two
strata and a min-rule; a third "curated" tier would fork every
derivation and join. Instead, admission is binary:

- A curated refinement is seeded **Verified** iff all three hold: the
  sidecar is present; the sidecar-reported PHP minor equals
  `PINNED_PHP` (the A11 pattern — the catalog-wide pin that already
  governs hierarchy-backed arm deletion, extended verbatim to return
  facts); and the extensional subset check of §1.2 passes.
- When any leg fails, the curated fact is **not seeded at all** — the
  call widens to the reflected envelope, or to nothing without a
  sidecar. Not demoted to Asserted: an Asserted seeding would make
  fixture and dump behavior diverge between sidecar modes and would
  put hand-authored rows into the narrowing stream on the strength of
  nobody's runtime — silence is the house answer to unconfirmed
  knowledge (ADR-0049 §1's dictum, value-domain edition: the catalog
  is not a truth oracle either; it is a refinement proposal the
  runtime countersigns).

The two guards close different holes. The subset check catches
build-configuration drift and the widening direction of staleness (a
runtime whose envelope grew an arm the curated row lacks). The minor
pin closes the narrowing direction the subset check *cannot* see: a
curated `string` row from a minor where the function could not return
`false` remains a subset of a later minor's `string|false` envelope
while being false about it. Within the pinned minor, every row was
verified against that exact php-src line, so both directions are
covered. Per-fact version tags are **refused** per the owner's
recorded instruction (2026-07-24: lower-version signature diffs are an
intended later direction with no implementation accommodation now);
per-minor table generation is already A11's recorded later refinement
and this surface inherits it for free when it lands. The sound subset
(`--no-php`) behaves per ADR-0004: builtin return facts widen away
entirely, and the coverage posture says so.

## 3. Sourcing discipline

The failure-arms pattern (ADR-0042, `docs/research/phpsrc-mining/
failure_arms.toml`) is the template: a TOML source of record,
`return_facts.toml`, one row per function, each row carrying its
evidence — the php-src stub type at the pinned version for the
envelope-shaped part, and for every refinement beyond the stub
(NonEmpty, IntRange bounds) a behavioral witness: a `php -r` probe
transcript or a php-src C citation, recorded in the row. Nothing is
recalled from memory; a row without evidence does not merge. The table
generates into the catalog crate (`cargo xtask gen-catalog`, the
hierarchy pipeline extended) behind one function-keyed,
case-insensitive lookup returning the value-domain fact. Rows are
hand-triaged and small — seeded in measured-priority order (§5), not
by sweeping any upstream map.

## 4. v1 scope bound: argument-insensitive facts only

A v1 row states a fact that holds for **every argument the function
returns normally on** — `count(): int<0,max>` always; a throw or
fatal on bad input is not a return and does not weaken the row.
Conditional shapes (`abs(int): int<0,max>` vs `abs(float): float`) are
**out of v1**: functions whose precise type is argument-dependent land
only their insensitive join (for `abs`, the envelope `int|float` —
still a reach improvement over `unknown`). The measured classes
support the bound: bool predicates, plain strings, int ranges, and
most refined strings are insensitive; only the scalar-union class
leans on conditionals. Deferred-with-design, one paragraph: the v2
form is a guarded arm keyed on one argument's General base
(`when arg#0: int ⇒ int<0,max>`), resolved by the base floor the
value domain already computes for the argument — a match on an
existing fact, not a dynamic-return-type plugin protocol. v1 is also
**function-keyed only**, like `failure_arms`: method-shaped returns
(the DOM accessor block of the plain-string class) wait for the M2
reflect slice's method-surface enumeration and ADR-0043's consumption
side. Return facts are complementary to folding, not overlapping: a
foldable call with all-literal args still folds to a Singleton via the
sidecar; the return fact is the floor for every call folding cannot
reach.

## 5. Rollout

Slices in the measured priority order, each Opus-sized, each
gate-verified. Return facts enter the value domain, which premises
both proofs and `phpdoc.*` honesty checks — the concrete FP channel
is a wrong fact "disproving" a correct docblock
(`phpdoc.return-mismatch`, ADR-0037) — so every slice runs the
fp-gate with verbatim 5-sample triage on any tripwire movement plus a
corpus measurement run, and records its **nsrt match-rate delta** as
the acceptance instrument (the harness that found the gap referees
the fix).

- **R1 — the plumbing + bool predicates** (741): the reflect-envelope
  seam (per-name cache, envelope lowering, stratum seeding), the
  admission gate of §2, the generated-table lookup; rows for the
  `is_*` family, `str_contains`/`str_starts_with`/`str_ends_with`,
  `array_key_exists`, `in_array` — all `General{Bool}`, zero
  conditional pressure, and the envelope machinery alone already
  serves every reflected builtin.
- **R2 — plain string returns** (893): `implode`, `sprintf`,
  `str_repeat`, `substr`/`trim` family (8.x: `string`, no `false`
  arms — evidence rows cite the 8.0 signature changes), `date`-shaped
  formatters.
- **R3 — int and int-range returns** (546 + 575): `count`, `strlen`,
  `str*len` siblings at `IntRange(0, max)`; plain-int rows.
- **R4 — refined strings** (675): `non-empty-string` producers —
  digest functions (`sha1`, `md5`, `bin2hex`, `number_format`,
  `uniqid`) — each NonEmpty bit carrying its behavioral witness.
- **R5 — scalar unions, insensitive remainder** (970): envelope-grade
  rows for the arithmetic-adjacent family; the conditional residue is
  measured and recorded as the v2 trigger, not chased.

## 6. Refusals

- **Wholesale functionMap import** (PHPStan's or any lineage): bulk
  unaudited rows are the rot ADR-0014 exists to prevent and the
  divergence registry's spirit rejects; the per-row evidence bar is
  the point, not an inconvenience. *Narrowed by ADR-0069 (owner
  decision, 2026-07-31): the refusal holds for this ADR's Verified
  lane; an Asserted floor with generation-time reflection audit is
  admitted there.*
- **Per-fact version tags / a version matrix**: owner-refused
  accommodation; the single `PINNED_PHP` gate plus A11's future
  per-minor generation is the whole story.
- **Version emulation**: standing refusal — Steins never models a PHP
  the project does not run.
- **A third "curated" stratum**: two strata plus a binary admission
  gate; forking the derivation clause for a tier nothing consumes is
  complexity without a consumer.
- **Asserted-mode seeding without sidecar confirmation**: mode-
  divergent fixtures and unconfirmed narrowing; the sound subset
  widens instead (ADR-0004).
- **Dynamic-return-type extension machinery in v1**: the v2 guarded
  arm is the bounded design; a plugin protocol waits for a plugin
  consumer (ADR-0039's seam is where it would live).
- **Method-keyed rows in v1**: waits for the reflect slice's method
  surface; a half-keyed method path would misclassify rather than
  refuse honestly (the ADR-0041 principle).

## 7. Open questions

- ~~Whether a reflected-existent but typeless builtin (no return type,
  no tentative type — rare on 8.5) may consume a curated row with no
  envelope to bound it: v1 says no (nothing to refine within);
  revisit with a measured case in hand.~~ **Resolved by §8** (owner
  decision, 2026-08-14): the measured case arrived — `resource`, where
  typelessness is structural rather than incidental — and the answer
  is a bounded yes, gated on the engine's continued silence.
- How the runner renders tentative return types and by-ref out-param
  interactions on the reflect wire — a protocol note on ADR-0024's
  surface when R1 lands.
- Whether admitted refinements should also feed contract-acceptance
  display (the PHPStan-vocabulary speller already renders Refined;
  expected to fall out, verify at R3's int-range rendering).

## 8. Amendment (2026-08-14): the engine-inexpressible type

§7's first open question asked whether a builtin the engine knows but
declares no return type for may consume a curated row with no envelope
to bound it. v1 said no, and asked for a measured case. The case is
`resource`, and it is not the incidental typelessness §7 imagined.

### 8.1 Why the envelope rule has a hole, and why it is exactly one hole

`resource` is **the one PHP type PHP cannot write down**. There is no
`function fopen(...): resource` because the language has no such type
declaration; `resource` in a hint position is read as a class name, and
PHP warns about it (probed at 8.5.9: `"resource" is not a supported
builtin type and will be interpreted as a class name`). So
`ReflectionFunction('fopen')->getReturnType()` is `null`, and it will
stay `null` for as long as the type exists.

§1's precedence rule reads reflection silence as *the engine having no
opinion*, and for every other type that reading is right — a builtin
with no declared return really is one nobody got round to annotating.
Here it is wrong in a specific, bounded way: the engine has an opinion
and no vocabulary. The hole is not "some types are hard to reflect", it
is one type, nameable in advance, and the amendment is scoped to it and
closes behind it.

### 8.2 The gate: three conditions, one of which is a tripwire

A curated resource row is admitted at a call site only when all three
hold. The first is data; the second and third are checked live.

1. **The php-src stub at the pin says `resource`.** Mined into
   `docs/research/phpsrc-mining/resource_returns.toml`, per-row, with a
   `get_debug_type()` transcript beside it.
2. **The analyzing engine declares NO return type for the name.**
3. **The project PHP minor equals `PINNED_PHP`** — the same version
   gate §2 already applies to every curated refinement.

Condition 2 is what replaces the envelope's authority, and it is worth
being precise about what it proves. It does not prove the function
returns a resource. It proves the engine has **not disowned** the claim
— because the one way this claim goes stale is the one PHP has actually
been doing for four minor versions: migrating a resource to an object.
A migrated function declares its new class, and a declaration is exactly
§1's "the engine speaks, curation yields". So the row switches itself
off, at the moment the migration lands, with no re-mining and no
staleness window.

The measurement says how much work condition 2 does. PHPStan's
`functionMap` names 110 resource-returning functions this engine knows;
**89 of them now return an object** (`curl_init` → `CurlHandle`, the
`imagecreatefrom*` family → `GdImage`, `ldap_*` → `LDAP\Result`,
`odbc_*` → `Odbc\Result`). That is the rot ADR-0069 §5 counted and
declined to carry. Condition 2 refuses all 89 without a denylist and
admits the remaining 19 — and mining php-src's stubs rather than
functionMap means those 89 were never candidates in the first place, so
the gate and the source agree independently. Two belts, and the
disagreement between the two sources is itself the cross-check.

### 8.3 Grade: Verified, and why that is not a widening of §1

A row admitted through §8.2 seeds at **Verified**, unlike ADR-0069's
declared floor. The difference is not confidence, it is checkability. A
`functionMap` row is Asserted because the row and the engine can
disagree silently and nothing asks. A resource row cannot fail that way:
the disagreement has one shape, the shape is observable through
reflection, and condition 2 observes it on every run. What survives is a
claim the engine corroborates in the only way the language leaves it.

Two consequences follow and are deliberate:

- The rows are the **only** thing this amendment lets past. No other
  typeless builtin gains anything; the gate names `resource` and the
  catalog table has nineteen entries.
- Without a live engine (`--no-php`) there is no tripwire, so nothing is
  admitted. The sound subset (ADR-0004) is unchanged.

### 8.4 The carrier: an arm lane, never a value

`resource` becomes `ContractTy::Resource` — a **leaf** in the contract
crate, with `open-resource` and `closed-resource` lowering to it. It
does not become a value-domain inhabitant, and the value domain stays
object-free and resource-free (ADR-0035/0038): a resource has no
extension to enumerate, no join to compute, and nothing the Lean vector
universe could check.

So a resource-returning call seeds the **contract arm lane** —
`resource` plus, where the stub declares one, `false` — and seeds no
value fact at all. The narrowing then costs nothing new: `if ($h ===
false) { throw; }` is the ordinary `Refine::Exclude` subtraction the arm
lane already performs, and what it leaves behind is a one-arm lane. No
resource-specific guard code exists anywhere.

`is_resource()` is deliberately **not** wired as a `TypePred` in this
slice. Arm *filtering* is done — a `Resource` arm is a
`RtKind::Resource`, which every existing `is_*` predicate rejects, all
of them correctly (`is_scalar`, `is_callable` and `is_iterable` on a
resource are all `false` at 8.5.9). What `is_resource` would add is the
*positive* branch binding a resource where none was proven, and that is
a producer question, not a filtering one. Deferred with its reason
recorded rather than half-built.

### 8.5 Where the definite `No` is claimed, and where it is refused

Acceptance is exact almost everywhere, because a resource is a leaf:
there is no hierarchy for an oracle to be unsure about.

- **Scalars, literals, `null`, arrays, `callable`** reject a resource,
  in **both** coercion modes. This is stronger than the object case and
  the difference is real: a `__toString` object coerces into a `string`
  parameter in coercive mode, so `member_rejects_object` demotes
  `string` to a strict-mode-only reject. There is no `__toResource`.
  Probed at 8.5.9 with no `declare(strict_types=1)`: `bool`, `int` and
  `string` parameters all `TypeError` on a resource argument. The
  finding therefore never consults the file's mode, and says so in its
  own message rather than naming a mode the reader might try to change.
- **`mixed` and both of its cuts** accept. No resource is null, and
  every resource is truthy — *including a closed one*
  (`fclose($h); (bool) $h === true`), which is the case a guess would
  get wrong.
- **`object` and a named class**, asked of a resource **value**, is a
  definite `No`: the value is proven and no class has resource
  instances.
- **The `resource` contract asked of an object value** is `Maybe`, and
  this is the one place the amendment declines a verdict it could
  technically justify. PHP 8 left a decade of `@param resource $ch`
  docblocks attached to parameters that now receive a `CurlHandle`.
  The docblock is wrong and the value is fine; convicting there would
  call the programmer a liar about rot they inherited. Named FP
  channel, refused on purpose.

### 8.6 The one lane-reading opening, and its three locks

ADR-0052 §3 keeps the contract arm lane away from the proof layer, for
the good reason that most of what seeds it is a docblock. The argument
families now read it, under a predicate narrow enough to state in one
line: **exactly one arm, that arm is `Resource`, and its stratum is
`Verified`**.

Each clause blocks a specific mistake. One arm, because `resource|false`
straight out of `fopen()` is not a proven resource and `false` genuinely
*is* accepted by a `bool` parameter. `Resource` exactly, not a supertype
and not an `Opaque` that might contain one. `Verified`, because that is
what excludes every docblock-seeded arm — a project function's
`@return resource` reaches the lane at `Asserted` and does not qualify.
The opening is this predicate and nothing wider; ADR-0052 §3's list is
otherwise unchanged.

### 8.7 What stays out

- **`stream_socket_pair`, `get_resources`** — arrays *of* resources. A
  `ShapeFact` holds `Fact`s and no `Fact` is a resource, so the element
  type has no carrier. Silent, as before.
- **Resource-consuming parameters.** `fwrite($notAResource, …)` is a
  separate direction and needs the builtin *parameter* surface, which
  this slice does not touch.
- **Open/closed state.** `fclose($h); fread($h, 1)` is a real bug and a
  real analysis, and it is a dataflow one, not a type one. The leaf
  models the kind; the state would need a different mechanism.
- **A `resource` value in the value domain.** Standing refusal, per
  ADR-0035/0038.

## 9. Amendment (2026-08-17): R1's parameter twin

Status: PENDING ratification.

§8.7 left "resource-consuming parameters" out because the builtin
*parameter* surface did not exist. Neither did any other parameter
judgment about a builtin: `Folder::builtin_return_type` answered what a
name gives back, `Folder::builtin_param_counts` (ADR-0064's arity leg)
how many arguments it takes, and nothing answered what those arguments
must **be**. So `strlen(1)` under `strict_types` was silent while
`f(1)` on `function f(string $s)` was a finding, for no reason a reader
of the code could account for — the argument relation was never the
missing part, only the parameter it had nothing to judge against.

This amendment gives R1 its parameter twin. It adds no new judgment, no
new id and no new stratum: it adds a *source*, on the road R1 already
built, and hands what it reads to the relation that has judged project
parameters since ADR-0043.

### 9.1 What the reflection answers

`ReflectionFunction::getParameters()`, per position, on the same
`reflect(target)` reply the return envelope rides (no new protocol
method — the reply grows a `params` array):

- `getName()` — so the finding can name `$string` the way PHP's own
  `TypeError` does;
- the `(string)` rendering of `getType()` (`"string"`, `"?int"`,
  `"array|string"`), or nothing where the position declares no type;
- `isPassedByReference()`, `isVariadic()`, `isOptional()`.

**Verified**, on the same ground the envelope is: this is the running
engine's own arginfo, read off the engine that will run the code, so it
is version-correct by construction (§1) and immune to the rot §6
refuses a signature map for. Nothing is recalled and nothing is
curated — see §9.5.

The gates are R1's, unchanged and shared: a live engine with no
ADR-0049 A9 monkey-patcher loaded, the name resident as a *function*,
and a project function of the same simple name refusing outright (its
own declaration is the better answer, and the builtin is not what runs).
Memoized once per lowercased name, exactly as the three rungs beside it.
`--no-php`, a spawn failure, and a replay table recorded before the
field all answer `None` — the reply parses whole or not at all, so an
older row is an unanswerable position, never a guessed one. The static
`functionMap` floor answers nothing here at all (§9.5).

### 9.2 The judgment is the existing relation, unchanged

At a call to a uniquely-resolved builtin, each positional argument is
judged by the same two rungs a project argument meets, in the same
order, at the **call-site file's** `strict_types`:

1. the proven-value definite No — `is_type_error`, emitting
   `type.argument-mismatch` on an all-`Verified` premise (ADR-0052 §5,
   ADR-0002's bar);
2. where no definite No fired, the possibly pair of ADR-0081's
   2026-08-16 amendment (issues #391/#418) on an abstract premise,
   with its carriers exactly as #418 leaves them.

One relation, one coercion table, one set of ids. A builtin parameter
and a project parameter of the same spelling are judged identically or
not at all — which is the property that makes this slice a source and
not a second checker.

**The lowering is the declaration lowering.** The reflected string goes
through `steins_contract::lower_str` — the seam the return envelope
already reads — and then into the `NativeType` shape a project
parameter's hint lowers to, under `lower_hint`'s discipline: the four
scalar bases, the `true`/`false` literal members, `null` as the
nullable flag, and **silence for everything else**. A single unmodeled
member collapses the whole position, so `array|string`
(`str_replace`'s first three) declines exactly as `array|string $x`
written in a project signature does, and `array` (`array_map`'s second)
declines exactly as `array $x` does. The issue's motivating list is
therefore only partly answered here, and deliberately: what the native
relation cannot say about a project parameter it does not learn to say
about a builtin one.

### 9.3 The one table difference: the internal-null coercive carve-out

PHP's coercion table is not quite the same on both sides of the
internal/userland line, in exactly one cell. From 8.1 on, passing
`null` to a **non-nullable scalar parameter of an internal function**
in coercive mode is a *deprecation*, not a `TypeError`:

```
$ php -r 'echo strlen(null);'
Deprecated: strlen(): Passing null to parameter #1 ($string) of type
string is deprecated
0
$ php -r 'declare(strict_types=1); echo strlen(null);'
Fatal error: Uncaught TypeError: strlen(): Argument #1 ($string) must
be of type string, null given
```

So a proven `null` into a builtin's non-nullable scalar parameter is
**silent in a coercive file** and a finding in a strict one, and the
possibly pair's `null` arm is suppressed on the same terms. The cell is
measured, not recalled: `harness/coercion-grid/` — issue #391's witness
harness — gains an internal grid
(`witness-internal-{strict,coercive}.tsv`, produced by running the
calls on the pinned PHP), and a test pins Steins' verdict against
PHP's cell for cell, the way the userland grid has been pinned since
#391. A deprecation is not this analyzer's business today (there is no
id for one); if it ever becomes one, it becomes one on the mechanics
layer with its own id, and never by weakening `type.argument-mismatch`
into a maybe.

### 9.4 What declines

Each of these is silence, and each has a reason that is not
"unimplemented":

- **an untyped position** — nothing to judge against;
- **`mixed`** — the total envelope, which refuses nothing (ADR-0064's
  reason for the arity leg, in the parameter direction);
- **a by-reference position** — the argument is an *out*-parameter
  (`preg_match`'s `$matches`); what PHP requires of it is a variable,
  not a value of a type;
- **a variadic position, and every position after it** — the tail is a
  spread, and one parameter binds many arguments;
- **any type the native relation does not model** — `array`, `iterable`,
  `callable`, `object`, `resource`, and (a v1 bound, unlike
  `lower_hint`) a **class-typed** position: the reflected string carries
  no source casing to display and the object-world definite No wants the
  project's own class oracle for a class the project may never index;
- **a named argument** — name-to-position binding for an internal
  target is its own slice (v1; ADR-0049 §6's named-argument machinery
  is userland-only today);
- **argument unpacking** — the position of an argument after a spread
  is a runtime fact;
- **a name the folder does not answer** — no engine, a monkey-patcher,
  a project function of the same name, a name this engine does not have.

### 9.5 No parameter types in the static floor (ADR-0069)

ADR-0069's `functionMap` import is **returns only, Asserted**, and this
amendment does not widen it. A parameter type is consumed by the proof
layer — it premises `type.argument-mismatch` on the default surface —
and ADR-0069 §2's whole firewall is that no imported row ever carries
that authority. A static parameter table would have to enter Verified
to be useful here and Asserted to be admissible there, which is the
contradiction that keeps it out. The parameter surface is engine-only,
by the same rule §1 states for the envelope: the real thing draws the
boundary.

### 9.6 What stays out

- **Method parameters.** §4's function-keyed bound, unchanged: the
  reflected class world (issue #269) already carries per-method
  parameter *counts*, and the types would arrive there, not here.
- **Resource-consuming parameters** (§8.7's entry) — still out. A
  `resource` position declines with the other unmodeled types, so
  `fwrite($notAResource, …)` is silent; the direction now has a road,
  not a judgment.
- **Curated parameter refinements.** §1's composition is stated for
  returns and is not extended: a curated `non-empty-string` *parameter*
  would refuse values the engine accepts, which is a finding-adding
  claim on hand-written evidence — the opposite of what curation is
  admitted for.
- **Deprecation findings.** See §9.3.
