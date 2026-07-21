# never, exit, and control effects: the type/effect division of labor

Resolves ADR-0006's open question. Five rules:

1. **`never` is the type; *why* it never returns is the effect.** A `: never`
   function is checked as "every path throws ∨ exits ∨ diverges"; conversely,
   a function inferred never-returning earns a `: never` fix-it (ADR-0010).
2. **`exit` propagates asymmetrically to `throw`.** `throw` is dammed by
   `try`/`catch` (the effect dies at the boundary — the basis of `@throws`
   checking); `exit` cannot be dammed and climbs the call chain
   unconditionally. In the proof layer `exit` is a *reachability input*
   (statements after it are dead), never a finding; "library code calls
   exit" complaints are policy-profile material — crying-wolf discipline.
3. **Divergence is not chased.** No termination analysis: `never` checking
   only verifies the absence of return paths and stays conservatively silent
   otherwise — the zero-FP trade, applied to control flow.
4. **`Pure` forbids `exit`, permits `throw`.** A throw is a recoverable way
   of delivering an outcome to the caller; an exit kills the program — not
   an alternative delivery path for a pure computation. This keeps
   ADR-0006's total-vs-exn asymmetry coherent.
5. **`goto` and `yield` are not effects.** `goto` is function-local (PHP has
   no non-local goto); `yield` shows up in the *type* (Generator return) and
   laziness is not impurity — a deliberate break from PHPStan, whose impure
   points include `yield`.
