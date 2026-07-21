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
