# Binding presence: a lowering-side reachability pass for the maybe-undefined pair

Issue #267. Status: **accepted 2026-08-09, owner-ratified.** Drafted under the
owner's standing delegation and then amended by the implementer with what the
corpus forced — the path-correct `try`/`catch`/`finally` rule that supersedes
the draft's uniform one, `match` and `?:` as leaf units, the `goto` dam, the
out-parameter-only residue, the §8 gate posture, and the never-returning-callee
deferral of §9. All of it is ratified as written; the amendments are the
decision, not a note on it.

## Problem

`variable.maybe-undefined` and `property.maybe-undefined` have been registered
since v0.1.4 — layer, floor (Strict) and suppression meaning fixed — with no
emitter. ADR-0078's emission table names the missing machinery: a claim that
some paths reaching a read carry no binding, with terminating paths subtracted
via `provably_terminates()`. The definite id (`variable.undefined`) is
deliberately ordering-blind — one binding form anywhere in the scope is
silence — so the some-paths shape (`if (...) { $x = 1; } echo $x;`) and the
use-before-assign shape (`$y = $x; $x = 1;`) both fall through it today.

## Decision

1. **The pass lives in lowering, not in the scope walk.** Binding presence is
   computed in `steins-syntax`, from the scope's CST alone, beside (not
   inside) the existing ordering-blind `scan_var_usage`. This is `BodyEnd`'s
   posture: env-free, index-free, deterministic from the CST. ADR-0048 §2
   (scope-walk replayability) is satisfied trivially — the walk is untouched —
   and §3 is discharged by construction: the fact kind is scope-closed and
   contributes nothing to any entry state. The alternative (threading a
   boundness map through `walk_trace`'s per-arm clone/join skeleton) was
   rejected for this slice: it turns a syntactic question into a walk-coupled
   one and makes §3 a live obligation for no additional recall on the shapes
   ADR-0078 names.

2. **Presence is a three-valued lattice per name: `Bound | Unbound | Maybe`,
   with `Bound ⊔ Unbound = Maybe`.** The join semantics are
   `steins_domain::Presence`'s (`Required`/`Optional`/`Absent` — the
   `offset.maybe-missing` engine); this pass either reuses that type or
   mirrors its documented join, and says which in the implementation note.

3. **Statement traversal, in order.** The pass walks the scope's statements
   in program order, updating presence per name: every binding form the
   definite pass recognizes (parameters and closure `use` seed `Bound`;
   assignments, `global`/`static`, `catch`, `foreach` targets, by-ref
   takes, increment/decrement, call-argument bindings) sets `Bound` from that
   point. Branch constructs (`if`/`elseif`/`else`, `switch`) evaluate each
   arm from the pre-branch state and join the survivors; **an arm whose
   `BodyEnd` is `Terminates` drops from the join** — this is
   `provably_terminates()`'s first production consumer, and `Unknown` stays
   on the safe side: not terminal, the path stays live, no claim.

   **Only an arm that falls through reaches the branch's successor**, which
   `BodyEnd` alone cannot say: a `break` terminates the statement list it
   sits in and yet lands on the enclosing *loop or switch*'s successor, a
   `continue` lands on the enclosing loop's *back edge*, and neither reaches
   the `if` it left. The pass therefore carries a four-valued flow
   (`Fell`/`Broke`/`Continued`/`Terminated`) beside `BodyEnd` and parks each
   jump's state for the construct it actually arrives at. This is not a
   refinement: reading a jump arm as reaching the `if`'s successor reports
   `foreach (…) { if (A) { $p = …; } elseif (B) { $p = …; } else { continue; }
   use($p); }`, the single most common shape in the corpus, and reading a
   `switch` arm's `break` as *not* reaching the switch's successor reports
   `switch ($c) { case 1: $x = 1; break; default: $x = 2; break; } use($x);`.
   Both were live bugs found by the corpus run, not by the unit suite.

   **`match` and `?:` are judged as leaf units in this slice**, not as branch
   constructs: a binding inside an arm reads as unconditional. That is the
   silence direction — it costs a finding on
   `$c ? $x = 1 : null; use($x);` and can never manufacture one — and it
   keeps the traversal at statement granularity, where the termination
   subtraction and the jump bookkeeping are defined. Promoting them is a
   later slice's option, not a correction owed.

4. **Loops iterate to a fixpoint.** A binding inside a loop body reaches the
   loop's exit as `Maybe` (zero iterations), and reaches the body's own
   earlier statements as `Maybe` on re-entry (a prior iteration may have
   bound it). The body is re-walked with its entry state joined with
   everything that reaches its **back edge** — the fall-through end plus
   every `continue` — until stable; the lattice has height two, so two
   passes bound it. The loop's successor joins three sources, and which ones
   apply is the loop's own question: the entry state and the back edge reach
   it only when the loop can exit by its condition, while every `break` state
   reaches it unconditionally. That last one is why
   `while (true) { if ($c) { $x = 1; break; } } use($x);` is silent — the
   only way out of that loop binds.

   `try`/`catch`/`finally` is conservative, but **not uniformly so**, and the
   draft's rule is superseded here. Saying "a binding inside `try` is `Maybe`
   after it" conflates two different paths and reports
   `try { $x = f(); } catch (E $e) { $x = 0; } echo $x;`, where every path
   binds. The path-correct rule, and what the code does: the
   normal-completion path keeps the block's own exit state; a `catch` arm is
   entered with the pre-`try` state joined with the block's exit, because the
   block may have thrown at any point; `finally` runs on every path, so its
   bindings apply unconditionally while its reads are judged against the
   weakened state.

   One refinement inside that, for the prologue idiom: a statement at the head
   of a `try` that **provably cannot throw** has run before anything can go
   wrong, so a `catch` arm is not entered with it undone. The predicate is a
   whitelist narrow enough to need no PHP-semantics argument — a plain `=`
   assignment from a literal, an array of literals, or another local. That is
   `$count = 0;`, `$out = [];`, `$x = $y;` and nothing beyond, which is
   exactly the shape that reported
   `try { $count = 0; foreach (…) {…} } catch (…) {…} if (1 < $count)`.
   Answering `false` costs precision and never correctness.

5. **Boundness guards are consumed with polarity — this is load-bearing, not
   an optimization.** `isset($x)` refines the true-continuation to `Bound`;
   `empty($x)` and `!isset($x)` refine their *false*-continuations to
   `Bound`. No guard ever refines toward `Unbound` (`isset` is false on a
   bound null). Without this, the defaulting idiom
   `if (!isset($x)) { $x = fallback(); } use($x);` would report — the
   then-arm binds, the implicit else-arm holds `isset($x)` true, the join is
   `Bound`, silence. This narrows `guard_tested_names`' both-arms shield to
   the actual protected arm, for this pass only; the definite pass and the
   walk keep their existing shields unchanged.

   Two extensions the corpus made non-optional, both still purely syntactic:

   * **A guard reads through its offset and property chain to the root
     local.** `isset($info['subject']['commonName'])` cannot be true unless
     `$info` is bound, so the root is as guarded as a bare `isset($x)` would
     make it. The early-return prologue
     `if (!isset($info['subject']['commonName'])) { return null; }` is far
     more common than the bare spelling, and reading only the bare one
     reported every read after it.
   * **A statement-position `assert()` refines everything after it**, at the
     *true* polarity and only there. ADR-0052 slice I0 already reads
     `assert()` as Verified evidence, and with assertions enabled a failed one
     throws, so control reaches the next statement exactly when the condition
     held — the true-continuation, and the only continuation. With assertions
     compiled out the call does not run, and then neither does whatever the
     assertion was protecting, so the refinement cannot manufacture a claim
     either way. The polarity is this section's own, so
     `assert(isset($x) && $x > 1)` refines through the conjunction and
     `assert(!isset($x))` refines nothing.

   Nothing above refines toward `Unbound`, which is the invariant that makes
   every one of them safe to add: a guard can only ever move a name toward
   silence.

6. **Emission: `variable.maybe-undefined` fires on a read whose presence is
   `Maybe` or `Unbound`, in a scope where the name is bound somewhere.** The
   somewhere-bound condition keeps the pair disjoint by construction: a name
   bound nowhere is the definite id's, exactly as registered. The
   use-before-assign shape (all paths unbound, yet bound later in text)
   stays on the maybe id — promoting it would break the definite id's
   documented ordering-blind contract. Every premise of the definite id is
   inherited verbatim: the name dams (`$$x`, `eval`, `include`, `extract`,
   `compact`, `get_defined_vars`) blank the scope for both passes; top-level
   scripts and arrow-function bodies never report; `isset`/`empty`/`unset`
   reads, `??` left-hand sides, `@`-silenced reads and `always_bound` names
   are excluded at collection; the checker half applies the same ADR-0049 §7
   warning-handler gate and the same ADR-0077 out-parameter subtraction (an
   oracle-confirmed out-param binds from its call site forward — not
   scope-wide, which is the definite leg's rule and right there only because
   that leg's premise is ordering-blind). The floor stays Strict (already in
   the registry): a partial-path claim is a possibly-grade finding, and the
   default profile is untouched.

   One residue, recorded rather than fixed. A name whose **only** binding
   form is an out-parameter position is bound nowhere in the scope's text, so
   lowering routes its reads to the definite id, whose checker subtracts an
   oracle-confirmed candidate scope-wide — and the read *before* the call is
   therefore silent on both legs rather than reported on this one. Moving it
   would mean routing between the legs in the checker, which is exactly the
   coupling the disjoint-by-construction split exists to avoid. The cost is
   recall, in the direction the proof layer always errs.

7. **`property.maybe-undefined` is the declared-shape possibly leg, not a
   reachability consumer.** Per ADR-0078's table it fires where the
   declared-receiver ladder proves the property absent on *some* union arms —
   `Presence::Optional` over the receiver's shape — following
   `offset.maybe-missing`'s emission pattern, and lands as its own small
   slice on the existing member-family premises. It shares this ADR only
   because the pair was registered together and ships together.

8. **Registry mechanics, and the corpus gate's posture toward a
   possibly-grade id.** Landing each emitter takes the four coordinated edits
   the registry tests pin: add the id to `ALL_EMITTABLE_IDS`, remove it from
   `REGISTERED_NOT_YET_EMITTED`, update the cardinality assertion, and replace
   the no-emitter-yet assertions.

   The fp-gate needs one more, and it is a policy decision rather than
   bookkeeping. The gate partitioned findings by **layer** alone, so a
   proof-layer id floored at `strict` red the gate on sight — the posture
   `type.return-maybe-missing` was landed under, whose comment read "the proof
   layer is gated whole, floor or not". **That is superseded.** A
   possibly-grade id claims only that *a* path reaches the site; a true
   finding on working code is the id's own yield rather than a defect a clean
   corpus is supposed to be free of, and the `strict` floor already keeps it
   off every default-profile run. Gating it red-on-sight would make the corpus
   a reason not to ship a check the floor exists to make optional — which
   inverts the owner's calibration rule that FP tolerance for these ids is
   absorbed by the opt-in, never by omitting the check.

   So the gate grows a third bucket: **possibly-grade**, on the same
   per-package increase tripwire as `phpdoc.*` / `throw.*` / `effect.*`, with
   seeded per-package baselines. Membership is derived from the registry
   (`Layer::Proof` and `Floor::Strict`), not from an id list, so a future
   `maybe-` sibling takes the right posture the day it registers and no list
   can drift from the floors. A count that **grows** is still a regression and
   still reds. Per-finding pinning in `EXPECTED_PROOF_FINDINGS` stays for the
   definite ids, where the standing bar remains a strict zero; the eleven
   `type.return-maybe-missing` rows already pinned there move into the new
   bucket's counts, since they were that shape all along.

   **This is a scoped reading of the zero-FP identity, and the owner ratified
   it as such on 2026-08-09.** ADR-0002 promises that the proof layer reports
   only what provably breaks on a live path, and ADR-0050 §1 restates it as
   "held to the zero-FP bar; gates red on sight". Read without a scope, that
   sentence says a proof-layer finding on working code is a defect of the
   analyzer. The ruling narrows it: **the strict-zero promise covers the proof
   layer's *definite* ids.** A possibly-grade id does not claim the program
   breaks; it claims that *a* path reaching this site carries no binding, no
   returned value, no proof — a partial-path claim, and it declares itself one
   at registration, by taking the `Strict` floor rather than a default-profile
   one. Holding a partial-path claim to a bar written for a total one would
   force the choice the owner's calibration rule forbids: ship no check, or
   ship it with its floor and then let a clean corpus veto it anyway. The floor
   *is* the opt-in that absorbs the tolerance; the gate's job for these ids is
   therefore **non-increase**, not zero.

   The scope is a floor, not a family: it is not "ids spelled `maybe-`" and not
   "ids in this ADR". Concretely, the bucket holds
   `variable.maybe-undefined`, `property.maybe-undefined` and
   `type.return-maybe-missing` today, because those are the three registry rows
   that are `Layer::Proof` *and* `Floor::Strict`. `offset.maybe-missing` is
   **not** in it despite the spelling: it registers as `Layer::Contract`, so it
   was already a measurement-mode id under the contract bucket and nothing here
   moves it. The derivation is the guarantee — an id joins or leaves this
   posture by its registered layer and floor changing, which is an ADR-visible
   act, and never by a list being edited.

   Nothing else about the banner moves. Every definite proof id keeps the
   red-on-sight bar; the default profile still prints exactly the
   proven-runtime-break set, since a `Strict`-floored id is absent from it by
   construction; and a possibly-grade id that fires on a **default** run would
   be a bug in the floor, not an exercise of this exception.

## Non-goals, each one line

- No promotion of all-paths-unbound to the definite id (ordering-blindness
  is that id's contract).
- No dead-code family, no `variable.unused` — different consumers, own ADRs.
- No walk integration and no narrowing coupling in this slice; a later slice
  may let the walk consume presence to narrow its `isset` shield.
- No cross-scope or cross-file reachability; the pass is scope-closed.
- No new configuration; the Strict floor is the whole opt-in surface.
- No `match`/`?:` arm joining; they are leaf units here (§3).
- A `goto` or a label anywhere in the scope **dams this id** — every other
  construct's exit edges are bounded by the traversal, and a jump to an
  arbitrary label is not. Silence is the honest answer to an unbounded edge.

## 9. Deferred with design: the never-returning callee

The one false-positive class the corpus triage found that this pass cannot
close, named here so its silence is not mistaken for an oversight.

`$this->fail('…')`, `self::fail(…)`, `markTestSkipped(…)`,
`exitWithErrorMessage(…)` never return, so a branch ending in one carries no
path to the read after it. `stmt_end` answers `FallsThrough` for every
statement-position call, and deliberately: deciding otherwise means asking
*which callee* and *does it declare `: never`*, which needs the project index
that this pass — lowering-side, env-free, index-free by §1 — does not have.
ADR-0078 records the same obstacle for `type.return-missing` and puts the
refinement at the emitter, where the index lives; the same answer applies
here, and the same place. It is a slice of its own, not a patch to this one:
the presence pass would have to publish enough structure for the checker to
re-subtract a branch arm after the fact, or the pass would have to move.

Measured cost, 2026-08-09: nine of the twenty OSS corpus findings and one of
the phpstan-src ones. Two other classes are FP-adjacent and stay: a binding
in an **argument position** of a throwing call inside a `try` (PHP evaluates
arguments before entering the callee, so it is done before anything can
throw — the pass weakens at statement granularity and cannot see inside), and
**correlated conditions** (bound under `if (C)` and read under a second,
textually identical `if (C)`), which is path feasibility and outside any
reading this id is defined over.

### 9.1 Amendment (2026-09-01, issue #599): the walk closed its half

**Status: PENDING ratification.** §9 named one symptom, and the gate's class-4
taxonomy inherited the name. It is two seams, and issue #599 leg 1 closed only
the one that was never this pass's:

- **The walk** (`walk_trace`) *does* hold the project index, so a
  statement-position call whose callee resolves to a declaration writing a
  **native** `: never` now terminates its trace there, as `throw`/`exit` do. The
  possibly-grade rows that were walk findings — `type.maybe-argument-mismatch`,
  and the contract-layer `phpdoc.maybe-*` siblings — are gone, and the branch
  they rode contributes nothing to the join.
- **This pass is unchanged.** `variable.maybe-undefined` fires off
  `Scope::maybe_undefined_reads`, and `stmt_end` still answers `FallsThrough`
  for every statement-position call, for the §1 reason unchanged by the above.
  §9's obligation is therefore *not* discharged: `check_undefined_variables` is
  still the place, and it still needs this pass to publish enough branch
  structure to re-subtract an arm after the fact.

Two constraints leg 1 records that any later discharge of §9 inherits, because
this refinement SILENCES (ADR-0046) rather than reporting:

1. **Native only.** A docblock's `@return never` is `Asserted` (ADR-0069); a
   comment must not delete code from the analysis.
2. **Resolution must be settled.** A dynamic callee, an unresolved receiver, a
   `static::` late-bound target, a conditional declaration (ADR-0049 A2i) and a
   namespaced bare name that matched only in the global namespace all decline.
   An **overridable** method needs no extra guard once resolved: PHP's return
   covariance admits only narrowing and `never` is the bottom type, so an
   override of a `: never` method must itself declare `never`.

Leg 2 — a `: void` helper that throws on every path — is out of scope for both
seams and stays recorded on issue #599; it needs an interprocedural
always-throws summary, not a resolution.

## Measurement

Strict-profile yield on the legacy monorepo reported as a number before the
PR leaves draft; default and contracts profiles pinned unmoved over the
corpus (fp-gate); the defaulting idiom, the guard polarities, the loop
fixpoint, the terminating-arm subtraction and the try-conservatism each get
a fixture with both the firing and the silent spelling.

## Amendment (2026-08-16): the argument side joins the possibly grade — a pair of ids, split by premise stratum (issue #391)

**Status: PENDING ratification.** Issue #391, the answer issue #291's
probe came back with. §8's membership rule is derivation, not a list, so
this amendment adds no mechanism to the gate; what it decides is the
shape of the first possibly-grade id whose premise is a *type* rather
than a *binding*, and — because that premise can arrive at either
stratum — why it is two ids and not one.

### A1. The claim, and why it is possibly-grade rather than definite

An argument's abstract fact (a `Fact::General`/`Refined`/`Union`, or the
declared-arm lane lowered the same way) has one or more base arms plus a
`null` side-flag. Against the callee's native parameter type, at the
call-site file's own coercion mode, three verdicts are possible: every
arm rejected, some rejected and some accepted, none rejected.

Issue #291 asked for the **first** one. Measured, it is empty — 0
firings on the pinned corpus, on phpstan-src's nsrt and on
php-typing-conformance, against 2,333 judged abstract-premise argument
positions on the corpus alone — and structurally so: a Verified
`General{base}` comes from a native declaration, so it is exactly the
runtime truth, and callers of a `string` parameter overwhelmingly hold a
string. #291 closes with "don't build it", and nothing here does.

The **second** is what the probe found instead, 12 times on the corpus,
and it is a different kind of claim. "Some arm the parameter rejects" is
not "this call breaks": an over-approximating type never proves an arm
is inhabited on a live path. It is exactly §8's partial-path shape one
carrier over — `variable.maybe-undefined` says *a* path reaches this
read unbound, and this says *a* value this argument's own type admits
would not bind. So it takes the possibly grade, by the same reasoning
and through the same registry derivation: `Layer::Proof` +
`Floor::Strict` puts it in the gate's tripwire bucket automatically, off
every default run, held to non-increase rather than to zero.

The corpus population says the same thing from the other side. Ten of
the twelve are one shape: a builtin whose declared return is `T|false`
(or `T|null`) handed straight into a native `T` with no check in
between — `file_get_contents`, `realpath`, `inet_pton`, `json_encode`,
`preg_replace`. That is PHPStan's `expects string, string|false given`
at level 5, and it is real latent breakage in released code, which is
the definition of a finding a clean corpus legitimately contains.

### A2. Two ids, because the premise has two strata (ADR-0052 §5)

The consumption rule is binding: a finding's premise stratum is the
minimum over every fact it consumed, and proof-layer ids require
all-Verified premises. This judgment consults **every** arm — it has to,
since "some rejected and some accepted" is a statement about the whole
arm list — so the minimum runs over all of them, not only over the
rejected ones. The two cases are genuinely different claims and get
genuinely different ids:

- **`type.maybe-argument-mismatch`** — `Layer::Proof`, `Floor::Strict`.
  Every arm Verified: a native declaration, or the reflected native
  return of a callee. The arms are the runtime truth; only inhabitation
  is unproven, which is what the possibly grade already declares.
- **`phpdoc.maybe-argument-mismatch`** — `Layer::Contract`,
  `Floor::Strict`. Some arm Asserted: a docblock claim, a curated
  refinement over a native envelope, or an ADR-0069 declared-return
  floor row. The claim is conditional on the contract being honest,
  which is the contract layer's definition, and ADR-0052 §5 forbids
  routing it anywhere else. An `Asserted` arm can never premise a
  `type.*` id; that is pinned by a fixture, not only by review.

**The `Strict` floor on the contract half is the `offset.maybe-missing`
precedent, not a deviation from the `phpdoc.*` family floor.** The two
legs of the offset family sit at exactly this split — `offset.undeclared`
at `Contracts`, its possibly sibling at `Strict` — because the floor
answers "how sure is this?" while the layer answers "whose claim is it?".
`phpdoc.param-mismatch` is this id's own definite sibling and stays at
`Contracts`. A partial-path claim over a declared type belongs one rung
up, and a `contracts` run keeps meaning what it means today.

The gate posture follows from the registry with no edit: `Layer::Proof` +
`Floor::Strict` routes the proof half to §8's tripwire bucket, and any
`phpdoc.*` contract id is already measurement-mode. Two ids, one
judgment, two buckets — and no third posture invented for the pair.

### A3. The judgment is `is_type_error`, asked once per equivalence class

There is **no second coercion table**. The base-level verdict is built
out of the existing concrete-value relation by handing it witnesses: an
arm of base `B` is rejected iff `is_type_error` rejects *every* witness
of `B`, and the `null` side-flag is judged with `ArgValue::Null`.

The witness set is the whole addition, and it is not one per base,
because acceptance is not uniform inside every base: `bool` needs `true`
and `false` (a `string|false` parameter accepts exactly one of them),
and `string` needs a numeric and a non-numeric one (coercive mode splits
on `is_numeric`, which is the entire reason a `string` base is not a
coercive-mode definite No against `int`). `int` and `float` need one
each. The equivalence classes are measured, not asserted: PHP itself
answers all 72 cells per mode in `harness/coercion-grid`, and a test
pins Steins' grid against it cell for cell.

A `Refined` fact decomposes to its **base**, dropping the refinement: a
refined set is a subset of its base's set, so base-rejection implies
refined-rejection. The converse — a `numeric-string` into a coercive
`int` — is sharper and is *not* taken here; it is a second judgment with
its own FP surface. `Singleton`/`OneOf` decline (the concrete lane
already owns them, and owns them better) and `Shape` declines (an array
against a scalar parameter is a real `TypeError`, but `is_type_error`
answers `false` for an array by construction, so admitting it here would
be that second table by another door).

### A4. The premise lane, and what it costs to read

The value lane is read where it has a fact; otherwise the declared-arm
lane (`Store::contract_arms`) is, lowered through the same `to_fact` the
scalar seeding uses. The arm lane is not an optional extra: 10 of the 12
corpus hits arrive there and nowhere else, because the value lane has no
carrier for a docblock-or-reflection `T|false` — `seed_refined_scalar_fact`
mints a value-lane fact only when a native `General` is refined *within
its own base*, and the inline-`@var` seeding only for array shapes. A
judgment that read the value lane alone would measure two hits and
conclude the shape is rare.

### A5. What this slice does not reach, stated as a bound

- The definite No of #291. Measured empty; not built.
- **Builtin parameters.** Arguments to builtins are not param-type-checked
  at all today — there is no builtin parameter-type source in the check;
  the builtin catalog supplies arity only — so every argument-side finding
  is capped at project-defined callees. A separate slice with its own
  measurement, and recorded in the divergence registry rather than left
  to be discovered. *(Closed by issue #423 / ADR-0056 §9, 2026-08-17: the
  source is the sidecar's own `getParameters()`, and this pair judges a
  builtin argument on the same terms it judges a project one — with the
  `null` arm taking the internal coercive carve-out §9.3 measures.)*
- **Non-`Var` argument carriers** (`$o->p`, `f(g($x))`, `$a['k']`): 74%
  of the 39,754 argument positions the probe saw. The same judgment at a
  different seam.
- Any relaxation of §8's derivation rule. An id joins or leaves the
  possibly posture by its registered layer and floor changing, which is
  an ADR-visible act.

### A6. Four narrowing repairs the measurement forced, each landed alone

None of them belongs to this id, and each is worth its own fix
regardless; they are recorded here because the id's corpus count is only
meaningful after them. The issue named the first two from the probe's
triage; the last two the implementation found, one of them by this id
firing on a shape the narrowing could not see.

1. **`assert($expr)` reached the value lane and not the declared-arm
   lane.** The 2026-07-25 ruling reads `assert()` as `if (!$expr) throw`,
   but the statement arm applied only the value-lane refinements and the
   type-predicate vocabulary, never the arm-lane subtraction — so
   `assert($x !== false)` narrowed nothing on exactly the `T|false`
   binding the guard exists for, while its `if` twin narrowed to
   `string`. One narrowing, one code path, now.
2. **An implicitly-nullable parameter (`f(string $s = null)`) rejected
   the `null` its own default admits** at the argument position, though
   the declaration side has read the same bit all along. That was a
   *definite* proof-layer false positive, not a possibly-grade one, and
   the corpus carried it. (The issue predicted a *join* bug here, from
   the shape of the corpus line. The join is correct: a local assigned a
   string on every arm of a nested `if`/`else` is non-nullable at the
   join, and a fixture now pins that it is. The line's real cause is the
   parameter default.)
3. **`if ($x == null) { return; }` left the fall-through nullable.** Only
   the identity spellings reached the refinement table. One direction of
   the loose pair is sound — `null == null` is true, so the branch where
   the guard *fails* proves non-null — and the other stays refused,
   since `0 == null` is true too. Found by this id firing on the shape.
4. **`@phpstan-assert !int $x` subtracted nothing.** The negated class
   spelling has narrowed since ADR-0052 §3(d); the scalar one fell
   through to the `!null` special case. `Subtrahend::Base` and its
   judgment already existed, so this was the missing wire. One carve-out:
   `!float` is refused, because arm judgment is contract *acceptance*,
   under which `int` is subsumed by `float` — reading that as identity
   would delete a live `int` arm. This is the third instance of the class
   the issue's conformance pair belongs to, so that pair is closed by a
   fix rather than by a baseline entry.

### A7. Measurement owed

Corpus partial count at 10 after both repairs, every line triaged
against its source in the PR body; nsrt headline non-decreasing;
conformance and the fp-gate diffed with causes; the possibly-grade
bucket's per-package baselines re-seeded for the proof half and the
`phpdoc.*` baselines for the contract half.

## Amendment (2026-08-27): the return seam joins the same pair (issue #537)

**Status: PENDING ratification.** The 2026-08-16 amendment above built the
argument side's possibly grade and named the seams it did not reach.
This one is the `return` statement, and it is a re-registration
rather than a new mechanism: same relation, same witness set, same
minimum-stratum split, same §8 derivation into the gate's tripwire
bucket. What it adds is one thing the argument seam never needed.

### B1. Object arms, and the exactness they demand

The shape the issue is about is `function f(A|B $x): B { return $x; }`
with `A` and `B` unrelated. That has no `Fact` anywhere: the value
domain is object-free (ADR-0035/0038/0043), so an object union exists
only in the declared-arm lane. Every scalar shape the argument pair
judges reaches the return seam unchanged, but the shape the issue
names reaches it only if a class arm can be judged at all.

`Cx::object_is_type_error` decides an object of an **exact** class. A
declared arm spelled `A` denotes every instance of `A` *and of every
subclass*, and a subclass may implement an interface the return type
accepts — `is_a(A, I) == No` says nothing about `class C extends A
implements I`. So a class arm is decidable exactly where the class can
have no subclass: `final`, or an enum (implicitly final, which also
settles the per-case arms an enum declaration seeds, §A of ADR-0088's
issue #429 work). Every other class arm is **undecided**, and one
undecided arm silences the whole position — "some rejected, some
accepted" is a claim about the whole arm list, so a list with a hole in
it supports neither half of it. That rule also swallows the `float`
arm, which `steins_contract::to_fact` refuses to spell for reasons of
its own, and it is why the return seam stays silent on `int|float`
into `string` exactly as the argument seam does.

The narrower rule is not the sharpest available one — `is_a(A, B) == No
&& is_a(B, A) == No` with a non-interface `B` is also sound under
single inheritance, and a full descendant closure (ADR-0049 §8) is
sharper still — but neither is needed by the shapes this seam was
opened for, and both are their own measurement.

### B2. One coercion table, measured at the boundary it is used at

The argument side's §A3 asserts nothing about `return`. PHP's return
coercion was therefore re-measured rather than assumed: all 144
return-position cells of `harness/coercion-grid`'s type × value grid
(8 native types × 9 witness values × 2 modes) answer exactly as their
parameter twins on PHP 8.5.9, and so do the object cells — a
`__toString` object into `string` in coercive mode and nothing else.
`is_type_error` therefore transfers verbatim, and a divergence would
have to be measured before it could be modeled.

### B3. Carrier, and what stays out

`return $variable;` only. The nested-call carriers issue #418 opened on
the argument side (`return g();`, `return $o->m();`, `return $a['k'];`)
need that seam's same-expression guard-decline surface (issue #421)
and their own corpus measurement; they are named, not shipped. Also
out, each because there is nothing to judge: a generator (its declared
type names the object the *call* yields — the guard `Cx::scope_return`
has carried since issue #128), `void`/`never` (no `NativeType` at all),
and the all-arms-rejected verdict, on §A1's own reasoning.

A `get` hook's body **is** in: since issue #544 the property's declared
type rides on the scope as the body's native return type, so the hook
gets this check by riding the same path, which is also what PHP
enforces there.

### B4. Measurement owed

Public corpus: the proof half at **zero**, the contract half **+3**,
all three in one file and all three the same missing narrowing — a bare
truthiness guard (`if ($x)`) subtracts nothing from a `T|false` arm
lane, so the arm the `if` just excluded is still standing at the
`return`. The argument side carries the identical gap and simply has no
public-corpus site for it. Seeded into `PHPDOC_EXPECTED` with the
triage beside it, on §A6's precedent: the narrowing repair is its own
slice, and the count comes back down when it lands. nsrt: unmoved (this
id changes no fact and no narrowing).

Conformance: **unmoved, and for a reason worth recording.** At
`--profile strict` the suite gains exactly four findings — the two
scored `stillAB`/`stillUnion` lines this issue exists for, and the two
guarded siblings `fromClassString`/`pick`, which stay noisy until
`::class` negative-branch narrowing (#538) and object-property
discriminant narrowing (#539) land. But `SteinsChecker` runs
`--profile contracts`, and a `Strict`-floored id is off there, so
neither the win nor the noise reaches the score. The score moves only
if the suite's profile is raised or this pair is re-floored; the floor
here follows §A2's split for the argument pair, and changing it is that
suite's question, not this id's.
