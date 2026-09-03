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
previous iteration's exit; it is the post-forget env, in which every name the
body can touch is already ⊤. Nothing in it is specific to an iteration, so
it is valid for all of them, the first included — and a body whose last
statement reassigns the very subject the header narrowed on re-derives the
fact at the next entry rather than losing it. That shape is the motivating
one: an AST traversal walking a parent pointer types its subject from the
loop header and from nowhere else, because the accessor it calls is
untyped.

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

Two costs stay, both deliberate, and the first is larger than it looks. The
entry env forgets **both** sets, and while forgetting `writes` is the by-ref
conservatism, forgetting `reads` is over-strong for an entry: a name the loop
neither assigns nor hands to a call holds its value on every iteration, and
the paragraph above says as much. So a binding the body merely mentions —
including a method receiver, which is not an argument — arrives unknown, and
only what the header narrows on is re-derived. That is why the traversal
above works and a declared parameter used inside the same loop proves nothing
(issue #653: the value, declared-arm and class lanes can stay; the referenced
object's mutable properties cannot, since a method call the body makes
changes them without the receiver ever entering `writes`). Second, `for`,
`foreach` and `do`/`while` are still body-less (issue #650); `do`/`while`
will need the entry narrowing **withheld** when it arrives, its first
iteration running before its condition is ever evaluated.

`StmtKind::While` and `StmtKind::LoopJump` both sit before `Opaque` in the
enum and the wire codec carries a variant by index, so `SCHEMA_VERSION` moves
11 → 12.
