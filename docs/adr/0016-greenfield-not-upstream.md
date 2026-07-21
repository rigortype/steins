# Why a new design rather than evolving an existing checker

**Official concept: PHP;STEINS is a parody of PHPStan** — an affectionate
homage, as the name declares — **and a proving ground for proofs-of-concept
that entail major BC breaks**, the kind no tool with an install base could
absorb directly. Ideas validated here are candidates to flow back upstream
in narrower, BC-safe form; we intend to keep contributing to PHPStan.

Every load-bearing premise of Steins — call-site value propagation
(ADR-0001), the zero-FP proof layer (ADR-0002), the lossless rewritable CST
(ADR-0003), the default-on PHP sidecar (ADR-0004), effects as an inferred
dimension (ADR-0005), and the demand-driven engine (ADR-0009) — is a
*foundation*, not a feature. None can be retrofitted into an existing
checker's architecture; each contradicts a structural commitment (modular
analysis, level-based reporting, lossy ASTs, static-only analysis, batch
pipelines) that the incumbents are correctly unwilling to unwind for their
existing users.

That "correctly" is meant sincerely: a tool with PHPStan's install base
should weigh backward compatibility heavily, and cross-cutting proposals
there rightly move at consensus speed. Our own upstream experience —
conditional-purity annotations, array-shape ordering semantics — confirms
the timescale of that process, which is mismatched with foundations-level
experimentation, not wrong. We continue contributing narrow improvements
upstream; Steins is where the different premises get tested whole.

Performance is explicitly a secondary motive. We broadly share the
assessment in [From Psalm to Pzoom](https://mattbrown.dev/articles/from-psalm-to-pzoom)
that raw speed undersells a PHP analyzer, precisely because so much of PHP
is only observable at runtime — which is why the sidecar, not the Rust, is
the load-bearing bet. Rust is the right vehicle for a salsa-style engine and
a single-binary LSP; it is not the point.
