# Propagation is staged through a linear trace IR; unknown lowers to Barrier

ADR-0001's engine is grown in provable stages, not built whole. The first
stage lowers each scope (function body, top-level script) to a **linear
trace IR** — an ordered statement list of `Assign`/`Call`/`Return`/`Barrier`
— where **everything not exactly recognized lowers to `Barrier`**.
Over-lowering to Barrier is always sound: it can only cause silence, never a
wrong finding. Control flow of any kind is a Barrier in this stage; a
variable's value is *known* at a use only along a barrier-free straight line
from its last literal assignment, and a scope containing aliasing machinery
(references, by-ref closure captures, variable-variables, `extract`,
`compact`, `global`, `include`, `eval`, `static`) is **poisoned** — nothing
in it is ever known. A constant function (body = exactly `return <literal>`)
propagates its value to zero-argument call sites in the same file.

The point of recording this: the IR's ratchet direction. Every future
precision gain (branch joins, interprocedural argument binding, shapes) is a
*refinement that removes Barriers or narrows poisoning* — each step lands
against a green fp-gate (ADR-0026), so precision only ever grows from a
sound floor. This is how "the program works outranks the worst-case
reading" becomes an implementation strategy rather than a slogan: we never
guess and then patch false positives away; we widen first and prove our way
narrower.

## Amendment (2026-09-03): a `while` body is a sub-trace, entered under its own header — PENDING ratification

Issue #649. The ratchet above forgets a construct's write and read sets and
keeps the rest of the env; that is what it does *to the code after* the
construct. What went unstated is what it does to the code *inside* one:
nothing at all, because the body never lowered. `while`, `for`, `foreach`,
`do`/`while` and `try` all became `StmtKind::Opaque`, a variant with sets
and no statements, so the walk had no body to enter and every trace-borne
finding inside a loop was silence. The same `call.undefined-method` fixture
fires at the top level and inside an `if` and said nothing inside a `while`;
scope-level families reading the CST (`variable.undefined`) fired in there
regardless, so the surface was inconsistent as well as incomplete.

`while` now lowers to `StmtKind::While`, carrying its condition and its body
as a sub-trace. The sets are unchanged and land exactly as before, so the
construct's effect on its successor is byte-identical. What they leave
standing is then also the body's **entry env**, narrowed by the header's
true-side refinements — the same application an `if`'s then-branch takes,
for the same reason: PHP evaluates the header immediately before every entry
to the body.

**No fixpoint, and this is the load-bearing part.** The entry env is not the
previous iteration's exit; it is an env in which every name the loop can
rebind is already ⊤ and every object it can mutate has been swept. Nothing
in it is specific to an iteration, so it is valid for all of them, the first
included — and a body whose last statement reassigns the very subject the
header narrowed on re-derives the fact at the next entry rather than losing
it. That shape is the motivating one: an AST traversal walking a parent
pointer types its subject from the loop header and from nowhere else,
because the accessor it calls is untyped.

The body contributes **findings, not facts**. Its exit env is discarded, so
what a loop computes cannot reach the code after it, and the negated
condition does not ride the fall-through — a `break` leaves a loop without
falsifying its header, so that is a separate question with a separate gate
(issue #651). A header the walk decides false leaves its body unwalked; the
region is not marked dead, since withdrawing what the env-free direct pass
already reports there is a separate judgment from adding what the walk now
reports.

`break` and `continue` had to stop being `Barrier`s for any of this to be
worth having. A `Barrier` *falls through* with a cleared env, so a guarded
`break` handed its `if`'s join an empty env and erased what the rest of the
body knew — free while bodies were unwalked, and the first thing a real body
hits now. They lower to `StmtKind::LoopJump`, a terminator: the statements
after one are unreachable and the branch holding one contributes nothing to
the join, exactly as a `return` does. Which loop a `break 2;` leaves stays
unmodelled and need not be modelled — the question a walker asks is whether
the code after it in *this* block is reachable, and the answer is no at every
level.

One cost stays, deliberately: `for`, `foreach` and `do`/`while` are still
body-less (issue #650), and `do`/`while` will need the entry narrowing
**withheld** when it arrives, its first iteration running before its
condition is ever evaluated.

`StmtKind::While` and `StmtKind::LoopJump` both sit before `Opaque` in the
enum and the wire codec carries a variant by index, so `SCHEMA_VERSION` moves
11 → 12.

## Amendment (2026-09-04): a loop body's entry env keeps what the loop cannot change — PENDING ratification

Issue #653, found by an adversarial review of the amendment above, which
first claimed the entry env forgot only `writes` and then had to own up: it
forgot **both** sets, and only one of those is right for an entry.

Forgetting `writes` is the by-ref conservatism — every name assigned in the
subtree and every name handed to any call in it — and stays. Forgetting
`reads` is right for the construct's **fall-through**, where a subtree that
reads and branches may have early-returned, so the tail must exclude the
value. It is wrong for a **body entry**: a name in `reads` is assigned by
nothing in the loop and handed to no call in it, so its binding holds on
every iteration. The paragraph two above says exactly this and the code did
the opposite.

Measured, that was most of the feature. A receiver is not an argument, so it
lands in `reads`, and `$o->tyop()` on a declared parameter was named inside
an `if` and silent inside a `while` — same statement, same proof available.
The same forgetting cost the **subtractive** guard vocabulary its base:
`instanceof` and the `is_*` family mint a fact and narrowed a loop header
from nothing, while `!== null` and truthiness subtract from a declared lane
that was no longer there. So inside a `while`, every fact was header-minted,
statement-local or env-free, and a declared parameter used in the loop
contributed nothing.

The construct therefore answers two questions from the same starting env,
neither of which can see the other. Its **fall-through** is what it always
was, sets and all. Its **body entry** drops `writes`, keeps `reads`, and is
then narrowed by the header.

**What a kept name may not keep** is the mutable state of the object it
refers to. A method call the body makes writes through a receiver that never
enters `writes`, so iteration 2 would read iteration 1's properties. Every
object a kept name points at therefore takes the sweep an escaping call
already performs: non-readonly properties and value generic carries go, the
class and the readonly properties stay. This is why the rule is not "forget
less" — the value, declared-arm and class lanes describe a binding the loop
cannot rebind, and the heap describes state it can. The sweep is
unconditional, not conditioned on the body actually calling a mutator: a
walk that proves a particular call harmless proves it for that call, and the
entry env is answering for all of them at once.

`poisons` clears both envs, unchanged: a subtree that aliases, `extract`s or
`eval`s has no binding worth carrying anywhere. The zero-iteration reading
moves to the entry env along with the narrowing, for the reason the
narrowing is sound there — the env holds at every evaluation of the header,
so a `No` there is a `No` at all of them.

The `writes` half is untouched and is a real cost still: recovering those is
ADR-0070's by-value survivor rule applied to a construct's sets rather than
a statement's, which moves every `Opaque`'s fall-through too. `for`,
`foreach` and `do`/`while` inherit this rule when their bodies start walking
(issue #650).

`SCHEMA_VERSION` does not move. Nothing about the trace's spelling changes,
so a stored trace from the amendment above replays into this walk unchanged;
what separates the two readings of it is the analyzer version, which the
generation fingerprint already covers (ADR-0092).
