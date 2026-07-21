# Own codemod engine + fix-its; no Rector integration; agent-first surface

Steins builds its own transform engine on the lossless CST (ADR-0003) and the
query engine (ADR-0009). Rector is a conceptual reference, never an
integration target: bridging to its nikic-AST world would re-import the
format-destruction problem ADR-0003 exists to solve. Deliberately lost and
accepted: Rector's migration rule assets (PHP-version upgrade sets) — the
near-term division of labor is "migrations go to Rector; type- and
effect-driven transforms are Steins'."

Two product surfaces:

- **Fix-its** — autofixes attached to diagnostics as first-class payloads
  (proof-layer findings ship a fix where one exists; annotation-restraint
  policy findings ship an exit, e.g. promote `array{...}` sprawl to a DTO
  class — prevention with a door, not a sermon).
- **Transforms** — standalone semantic rewrites whose *preconditions are
  spelled in types and effects* (ADR-0008's lattice cashes in a second time):
  loop→`array_map` requires the body pure; call deletion requires effects
  empty; statement reordering requires effect non-interference. No PHP tool
  can express these preconditions today.

The interaction model inherits **consult-rector** conceptually (no asset/DSL
compatibility): an AI agent drives refactoring conversationally through a
dry-run → diff → approve → apply loop over structured output (CLI + MCP), with
a completeness oracle enumerating unreached sites. In Steins the analyzer
itself is that oracle — call-site propagation enumerates affected sites
directly instead of measuring an error delta through an external tool.
