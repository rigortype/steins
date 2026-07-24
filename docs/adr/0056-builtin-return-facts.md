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
  the point, not an inconvenience.
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

- Whether a reflected-existent but typeless builtin (no return type,
  no tentative type — rare on 8.5) may consume a curated row with no
  envelope to bound it: v1 says no (nothing to refine within);
  revisit with a measured case in hand.
- How the runner renders tentative return types and by-ref out-param
  interactions on the reflect wire — a protocol note on ADR-0024's
  surface when R1 lands.
- Whether admitted refinements should also feed contract-acceptance
  display (the PHPStan-vocabulary speller already renders Refined;
  expected to fall out, verify at R3's int-range rendering).
