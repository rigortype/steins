# Demand-driven incremental engine (salsa-style) from the start

Call-site value propagation (ADR-0001) plus LSP-as-premise creates a problem
neither parent had: one edit can invalidate inference across the whole
project, and the tool must still answer per keystroke. We adopt a salsa-style
demand-driven query architecture (the rust-analyzer lineage) from the first
commit: all analysis is memoized queries; the framework tracks dependencies
and invalidates minimally. Hand-written cache invalidation over
interprocedural propagation would certainly be wrong; retrofitting LSP onto a
batch engine is the path PHPStan/Psalm/Rector walked and suffered
(Rector gave up editor integration entirely).

Accepted cost: the query decomposition makes the road to a first working CLI
longer than a naive batch engine — deliberately paid, even at the price of
false starts, because the batch design would otherwise become a permanent
shackle. Steins has no reference implementation to lean on (unlike rigor-rs),
which makes the "working thing first" temptation strong; we resist it.

Budgets (inference cutoffs) are imported from Rigor, and a budget cutoff
**names itself** — Rigor's Certainty discipline: `maybe` is reported as
`maybe`, silence is never manufactured. The dual of the crying-wolf
prohibition: quiet misses must announce themselves too.
