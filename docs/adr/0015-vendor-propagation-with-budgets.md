# Inference descends into vendor/, per-package budgets, vendor diagnostics off

Call-site value propagation (ADR-0001) descends into `vendor/` bodies by
default: the composer world is source-visible, and the project/vendor seam is
where bugs live — if shapes collapsed to `array` there, the differentiation
would die exactly where users need it. Cost is contained two ways: vendor is
near-immutable input, so salsa memoization (ADR-0009) is at maximum
efficiency; and **per-package budgets** (Rigor's `budget_per_gem` imported)
cap runaway inference, with cutoffs naming themselves per the Certainty
discipline. Descent is for inference only — diagnostics *inside* vendor code
are off by default.

Considered: envelope-only vendor (PHPStan's model) — cheaper, but holds
precision hostage to vendor phpdoc quality and kills shape flow at the
boundary. Rejected.
