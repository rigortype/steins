# License boundaries: MIT vocabulary, AGPL core (relicensing kept open)

- **Attributes package** — `rigortype/steins-attributes`, pure PHP, **MIT**.
  It is vocabulary, not tooling: it lives in users' `require-dev` and is
  referenced from their source, and its spread is the goal — other tools
  reading `#[\Steins\Effect]` would be a win (the PSR-Effect ambition).
  Copyleft on seven inert classes buys nothing and costs adoption.
- **Core** — AGPL-3.0 today. Apache-2.0 or MPL-2.0 remain live candidates
  (AGPL lowers the odds of e.g. PhpStorm integration); the decision must be
  settled **before accepting external contributions**, after which
  relicensing requires every contributor's consent — until then the sole
  copyright holder can switch freely. If contributions arrive first, a
  DCO/CLA preserves the option.
- **Sidecar runner** — the embedded single-file PHP stays part of the AGPL
  binary; it never enters the project's artifacts (temp-dir execution,
  ADR-0024), so no boundary issue exists.
- **Mago fork** — `rigortype/mago`, rev-pinned (upstream MIT/Apache dual —
  compatible). Rebases are need-driven (parser fixes, new PHP syntax), never
  on a schedule.
