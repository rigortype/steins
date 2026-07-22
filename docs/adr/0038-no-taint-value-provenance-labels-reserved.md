# No taint analysis; value provenance labels reserved as the general mechanism

Steins does not pursue taint analysis. Taint is essentially worst-case
reachability ("may reach a sink unsanitized") — its practical value lives
in maybe-warnings, which the zero-FP identity cannot carry without either
looking weak (proven-only taint) or breaking the banner (maybe taint).
Ruby's runtime taint flag is the cautionary precedent of the family
(removed in Ruby 3.0); static taint exists competently elsewhere
(Psalm-lineage tools, dedicated scanners), and real-world protective
effect is rate-limited by annotation adoption regardless of engine
quality.

**PHPStan's `literal-string` is assessed as the same family**, differing
in polarity (known-safe origin vs known-bad), not in kind: it is a
*provenance* property, not a value property — two identical strings can
differ in literal-string status, so it is inexpressible as an extensional
refinement and is deliberately NOT imported into ADR-0035's Refined layer
(doing so would break the value domain's extensionality).

**The reserved expansion mechanism — value provenance labels**: open-
registry dot-path labels attached to *values* (`taint.user-input`,
`provenance.literal`, `zone.db-illusion` for ADR-0037's dialect
boundaries), propagated through operations per catalog rules, and consumed
by **boundary policy profiles**. This reuses the ADR-0018 machinery
(registry, prefix subsumption, plugin registration) wholesale as the value
counterpart of effect labels — one mechanism covering what taint,
literal-string, and dialect zones each partially address.
Reconsideration preconditions: the Refined layer and label registry
mature, and a boundary-policy consumer with demonstrated demand.
