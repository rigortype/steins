# Branch-sensitive analysis: structured trace tree, trinary conditions, staged

The successor to the linear trace (ADR-0027) is a **structured trace tree**,
not a general CFG: `Opaque` is made recursive per construct (`If { cond,
then_trace, else_trace }`, `Match`, `While`, …) and the evaluator walks it
with an environment. PHP is a structured language (goto stays Barrier);
the ADR-0027 ratchet survives — constructs are structured one at a time —
and `analyze_scope`'s evaluator shape, descent wiring, and span fidelity
all carry over. If loop precision ever truly needs it, a *local* CFG for
loop constructs is the escape hatch; a whole-IR CFG migration is rejected.

**Conditions evaluate to a single unified `Certainty` (yes/no/maybe)** —
the same type used for contract acceptance (ADR-0030), `isList` (#14939),
and envelope verifiability. This is deliberately PHPStan's TrinaryLogic /
Rigor's Certainty: maybe never promotes, and "maybe → analyze both
branches and join" is the discipline's implementation. Stage-1 decidable
predicates: `===`/`!==` on literals; `==`/`!=` via an **empirically
measured table** (the coercion-table discipline: every cell verified
against the real PHP on record, uncertain cells stay maybe — PHP 8's
`null == 0`, `"" == null`, `"abc" == 0` traps make hearsay unacceptable);
truthiness incl. `"0"`; `instanceof` against exact-class facts through the
project chain; `&&`/`||`/`!` by trinary composition (short-circuit env
refinement is stage 2).

**Refinement is positive-only in stage 1**: inside a branch, its condition
holds — `if ($u === null)` binds `$u = null` in the then-trace (making a
`$u->method()` there a provable null dereference: the `call.on-null`
diagnostic), `=== false`/`=== <literal>` likewise. Negative facts
("not null" narrowing of unknowns) are stage 2, requiring the value
domain to carry negations.

**Joins**: stage 1 keeps a fact only when both live branches agree; the
env value representation is designed from the start as a small literal set
(`OneOf`, cap ~8) so stage 2 can enable union joins without re-plumbing.
**Early-exit pruning** is the payoff: branches ending in
return/throw/exit/continue/break drop out of the fall-through join — the
guard patterns that produced the field-test FPs become *proofs that the
bad path is dead* instead of reasons to forget. When no branch falls
through, subsequent code is unreachable and analysis stops (closing
ADR-0027's documented dead-fallthrough gap).

**Staging**: (1) if/elseif/else/ternary + early-exit + positive refinement
+ call.on-null; (2) match/switch, short-circuit refinement, union joins,
negative facts; (3) loops beyond the write-set treatment. Every stage
lands against the double-corpus fp-gate.
