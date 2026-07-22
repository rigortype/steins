# Generics under call-site propagation: no solver where values flow

`@template` handling has three tiers, inverting PHPStan's solver-centric
model (a registered divergence, ADR-0030):

1. **Where propagation reaches, templates are transparent.** Bound
   arguments carry actual values/exact classes through binding descent;
   `T` *is* whatever flowed in — a value, which holds strictly more
   information than a solved type variable. No call-site template solving
   runs (running a solver beside propagation would create dual-inference
   disagreement, a bug factory). A declared bound (`@template T of Foo`)
   still participates as an upper-bound contract.
2. **Where propagation cannot reach** (public-API entry points, opaque
   callers), templates act as **contracts**: signature-internal
   consistency and bound checking only, imported from the PHPStan
   denotational core.
3. **Class-level generics (`Collection<int>`) are state, not solving** —
   an extension of the exact-class fact. Stage 1 reads *declared* type
   parameters (phpdoc/`@var`) as envelopes on element operations; growing
   parameters from observed element flow is deferred to the same machinery
   as shape evolution.

Accepted cost, stated honestly: library-author diagnostics about
internally-inconsistent generic signatures (a PHPStan strength) stay thin.
Steins' battlefield is application/monorepo defect-finding, where
propagation is the stronger weapon.
