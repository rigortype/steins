# Call-site value propagation as the core analysis model

Steins discourages hand-written structural phpdoc types (`array{foo: int}`,
scattered `@var`), so shapes and literal values must cross function boundaries
by inference. We therefore adopt Rigor's interprocedural model — actual
argument types/values flow from each call site into the callee, analyzed
flow-sensitively — instead of PHPStan's modular per-function checking against
declared signatures. PHPStan's annotation sprawl is a structural consequence of
modular analysis; rejecting the sprawl means rejecting the model.

Declared types are not discarded: a native declaration or phpdoc type acts as
an **authoritative envelope** — an upper bound the analyzer trusts and refines
within (call-site precision may tighten inside it, never widen beyond it).
Where no declaration exists, inference runs unassisted.

## Consequences

- Whole-program analysis cost must be actively managed (budgets, caching,
  LSP incrementality) — these are separate decisions.
- Annotation restraint becomes viable: precision without `array{...}` sprawl.
