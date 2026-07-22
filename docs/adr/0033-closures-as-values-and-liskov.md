# Closures as values, invocation-shape catalog, and Liskov as a standing rule

Redeems ADR-0005's claim that opaque-callback problems dissolve under
propagation:

1. **Closures / arrow fns / first-class callables are env values**
   (`Closure` facts carrying their scope; string callables via literal +
   context). Normal value discipline — reassignment and Opaque reads drop
   them; by-ref `use (&$x)` still poisons.
2. **Calling a known callable is binding descent** into its body (shared
   depth budget/memo), with `use` captures bound into the initial env.
3. **Higher-order builtins get invocation-shape metadata** in the catalog
   (Steins layer, ADR-0014): `array_map` is described not by an effect but
   by *how it calls* — (callback param, immediate, args drawn from array
   elements). Effects become callback-effects ∪ own-effects; with proven
   element values the callback descends and even folds. Deferred invokers
   (`register_shutdown_function`) propagate effects without claiming when.
4. **Exhaustiveness stays honest**: an unknown callable still taints with
   `…?`; a known one flows proven effects — `#[Pure]` bodies calling
   `array_map($impureCb, …)` report with via-provenance (the final Steins
   answer to the PHPStan conditional-purity saga).
5. **Liskov substitutability is a standing rule, deliberately weighted**
   (user directive): wherever an envelope sits on an abstraction —
   interface or parent method; effects, `@throws`, phpdoc contracts —
   every project implementation/override is checked against it:
   implementations may be *purer, throw narrower, accept wider, return
   narrower*, never the reverse. Carrier interfaces (PSR-20 class,
   ADR-0005) are the canonical case; the same check covers ordinary
   inheritance. Violations report at the implementation site, naming the
   abstraction's envelope.

**Ordering**: branch-analysis stage 1 (ADR-0031) → this closure wave →
the `throw` color (`@throws` envelope checking) — throw is the one
dammable color, so try/catch structuring must exist first.
