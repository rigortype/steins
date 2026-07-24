# Development Notes

Empirical **working notes** — measurements over the corpus, soundness audits,
adoption drills, and surveys of neighbouring tools. They record *what was
observed* and when, the analysis it prompted, and any follow-up that landed.

These notes are **non-normative and time-stamped to authorship**. They reflect
what was true when written, against the Steins revision named inside. A note may
feed an [ADR](../adr/README.md) or engine work, but the
[type specification](../type-specification/README.md) and the ADRs bind — the
note does not. Verify any named file, id, or flag still exists before acting on
one.

Filenames are `YYYYMMDD-<slug>.md`, dated to authorship.

## Measurements

- [Builtin-call frequency over the FP-gate corpus](20260722-builtin-frequency.md)
  — which builtins actually appear, and how often, used to sequence catalog
  seeding (ADR-0021).
- [G1 measurement: direct vs propagated `throw.undeclared` split](20260724-g1-throw-origin-measurement.md)
  — the evidence behind the `throws-direct` stage being the high-signal subset
  (ADR-0050).

## Audits

- [Soundness audit: ADR-0049 absence family + ADR-0052 narrowing](20260724-adr0049-0052-soundness-audit.md)
  — a pre-implementation audit of the silence legs the absence proofs depend on.

## Adoption and release

- [Adoption-drill record (M2 exit evidence)](20260724-adoption-drill-record.md)
  — running Steins over held-out real applications; the zero-FP acceptance
  evidence.
- [v0.1.0 run — auto-ADR decision log](20260724-v010-auto-adr-log.md) — the
  decisions taken during the v0.1.0 push, logged as they happened.

## Surveys

- [The four oxidized PHP checkers: Steins vs Mago / Pzoom / Mir](20260722-oxidized-php-checkers.md)
  — where the Rust-side PHP-tooling landscape stands and what Steins does
  differently.
