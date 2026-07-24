# Operational Guide

How to run Steins and shape what it reports. This is the practical half of the
documentation; the [handbook](../handbook/README.md) explains what the analysis
*means*, and the [type specification](../type-specification/README.md) is the
binding record when either disagrees with it.

- **[Quickstart](quickstart.md)** — install, the three subcommands, your first
  `steins check .`, reading the default surface, the PHP sidecar and the
  `--no-php` sound subset, exit codes, and the honest v0.1.0 limits.
- **[Profiles and baseline](profiles-and-baseline.md)** — the named strictness
  stages (`default` → `throws-direct` → `contracts`), the JSONL baseline
  ratchet, user profiles in `steins.toml`, inline `@steins-ignore`, and why
  mechanics ids can never be switched off.

Two things worth knowing before you start:

1. **A bare `check` is proof-only.** It reports what provably breaks at runtime
   and nothing else. Debt reporting — undeclared `@throws`, PHPDoc mismatches,
   effect-envelope violations — is real and abundant in released code, so it
   lives behind a named profile you opt into, never in a first run.
2. **Silence is not a safety claim.** Steins printing nothing about a call means
   it could not decide, not that it decided the call is fine.
