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

## Measurement

Strict-profile yield on the legacy monorepo reported as a number before the
PR leaves draft; default and contracts profiles pinned unmoved over the
corpus (fp-gate); the defaulting idiom, the guard polarities, the loop
fixpoint, the terminating-arm subtraction and the try-conservatism each get
a fixture with both the firing and the silent spelling.
