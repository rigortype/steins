# Binding presence: a lowering-side reachability pass for the maybe-undefined pair

Issue #267. Status: PENDING ratification (autonomous design under the owner's
standing delegation; implementation may proceed, the owner ratifies post hoc).

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
   point. Branch constructs (`if`/`elseif`/`else`, `match`, `switch`,
   `?:`) evaluate each arm from the pre-branch state and join the survivors;
   **an arm whose `BodyEnd` is `Terminates` drops from the join** — this is
   `provably_terminates()`'s first production consumer, and `Unknown` stays
   on the safe side: not terminal, the path stays live, no claim.

4. **Loops iterate to a fixpoint.** A binding inside a loop body reaches the
   loop's exit as `Maybe` (zero iterations), and reaches the body's own
   earlier statements as `Maybe` on re-entry (a prior iteration may have
   bound it). The body is re-walked with its entry state joined with its exit
   state until stable; the lattice has height two, so two passes bound it.
   `try`/`catch`/`finally` is conservative: a binding inside `try` is `Maybe`
   after it (the body may have thrown at any point); `catch` arms join like
   branch arms; `finally` applies unconditionally.

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
   oracle-confirmed out-param binds from its call site forward). The floor
   stays Strict (already in the registry): a partial-path claim is a
   possibly-grade finding, and the default profile is untouched.

7. **`property.maybe-undefined` is the declared-shape possibly leg, not a
   reachability consumer.** Per ADR-0078's table it fires where the
   declared-receiver ladder proves the property absent on *some* union arms —
   `Presence::Optional` over the receiver's shape — following
   `offset.maybe-missing`'s emission pattern, and lands as its own small
   slice on the existing member-family premises. It shares this ADR only
   because the pair was registered together and ships together.

8. **Registry mechanics.** Landing each emitter takes the four coordinated
   edits the registry tests pin: add the id to `ALL_EMITTABLE_IDS`, remove it
   from `REGISTERED_NOT_YET_EMITTED`, update the cardinality assertion, and
   replace the no-emitter-yet assertions.

## Non-goals, each one line

- No promotion of all-paths-unbound to the definite id (ordering-blindness
  is that id's contract).
- No dead-code family, no `variable.unused` — different consumers, own ADRs.
- No walk integration and no narrowing coupling in this slice; a later slice
  may let the walk consume presence to narrow its `isset` shield.
- No cross-scope or cross-file reachability; the pass is scope-closed.
- No new configuration; the Strict floor is the whole opt-in surface.

## Measurement

Strict-profile yield on the legacy monorepo reported as a number before the
PR leaves draft; default and contracts profiles pinned unmoved over the
corpus (fp-gate); the defaulting idiom, the guard polarities, the loop
fixpoint, the terminating-arm subtraction and the try-conservatism each get
a fixture with both the firing and the silent spelling.
