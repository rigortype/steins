# Throw damming: the one effect that dies, and how it dies

The `throw` color is the only dammable effect; its semantics land together
with `try` structuring (ADR-0031 stage 2:
`Try { try_trace, catches, finally_trace }`).

1. **Absorption is a Certainty judgment**: `catch (FooException)` absorbs
   `throw<E>` iff `E <: FooException` resolves Yes through the project
   inheritance chain. On Maybe, the safe side **inverts by consumer**:
   envelope checking (`throw.undeclared`) stays silent (escape unproven —
   zero-FP), while exhaustiveness (`…?`) names the lower bound. This
   asymmetry is the design's load-bearing joint.
2. **Four sources of throw facts**: explicit `throw new X` (resolved
   class); the catalog plus *measured* fold results (ADR-0024's
   `{throw: class}` — `intdiv(1,0)` seeds `DivisionByZeroError`
   empirically); propagated callee sets under ADR-0007's checked
   accounting (Error/LogicException families never count against
   envelopes); and **rethrow**: `throw $e` of a catch parameter re-emits
   exactly the absorbed subset, while wrap-and-throw emits the new class.
3. `catch (Throwable)` absorbs hierarchically. **`finally` absorbs
   nothing**; its effects join; its control-overriding semantics
   (return-in-finally) are explicitly deferred.
4. **Liskov applies** (ADR-0033 standing rule): overrides and interface
   implementations may not declare or infer broader checked throws than
   the abstraction's `@throws`.
5. Unannotated functions are never envelope-checked (opt-in principle),
   but inferred throw sets always propagate — callers' checks and
   annotate need them.

Reserved IDs: `throw.undeclared` (checked escape past a written
`@throws`), `throw.impossible-catch` (a catch whose absorbable set is
provably empty — dead catch, policy layer).
