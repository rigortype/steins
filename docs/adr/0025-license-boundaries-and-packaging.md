# License boundaries: MIT vocabulary, Apache-2.0 core (G3 settled)

- **Attributes package** — `rigortype/steins-attributes`, pure PHP, **MIT**.
  It is vocabulary, not tooling: it lives in users' `require-dev` and is
  referenced from their source, and its spread is the goal — other tools
  reading `#[\Steins\Effect]` would be a win (the PSR-Effect ambition).
  Copyleft on seven inert classes buys nothing and costs adoption.
- **Core** — *(SUPERSEDED by the 2026-07-25 amendment below: the core is
  Apache-2.0. The original text is kept for the reasoning trail.)*
  AGPL-3.0 today. Apache-2.0 or MPL-2.0 remain live candidates
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

## Amendment (2026-07-25): G3 settled — the core is Apache-2.0

The **Core** bullet's open question is closed by owner decision: the core
relicenses from AGPL-3.0-only to **Apache-2.0**, effective for the first
tagged release. The bullet's own condition is what made this the moment to
do it — relicensing had to be settled *before accepting external
contributions*, while the sole copyright holder can still switch freely; no
external contribution has landed, so no consent was needed and the DCO/CLA
fallback never had to be invoked.

The reasoning the original bullet recorded is what carried: AGPL lowers the
odds of the integrations Steins wants to exist inside (an IDE plugin
embedding the analyzer, a CI vendor running it as a service). For a tool
whose value is being adopted into other people's pipelines, copyleft taxes
exactly the adoption path that matters. Apache-2.0 over MIT for its express
patent grant and its explicit contribution terms (§5), which suit a project
that expects outside contributors.

This **supersedes** the entry in
[`docs/notes/20260724-v010-auto-adr-log.md`](../notes/20260724-v010-auto-adr-log.md)
recording "G3 (license): no relicense for v0.1.0 — core ships AGPL as-is …
decision = keep" (2026-07-24, batch-ratified). That entry was a decision to
defer, and it is reversed here, one day later, before it bound anything: the
only artifact ever published under the old terms is the throwaway
`v0.1.0-rc1` pipeline rehearsal.

Consequences, each already applied:

- `[workspace.package] license` is `Apache-2.0`; all eleven members inherit it.
- The **sidecar runner** bullet's phrase "part of the AGPL binary" now reads
  "part of the Apache-2.0 binary". Its substance is untouched — the embedded
  PHP never enters the project's artifacts (temp-dir execution, ADR-0024), so
  there was no boundary issue under either licence.
- `deny.toml`'s permissive-only allow-list **gains** force rather than losing
  it. Under AGPL a copyleft dependency merely foreclosed this relicense; now
  it would contradict the licence users are already handed, so weak-copyleft
  entries (MPL-2.0, LGPL) need a deliberate decision, not a list edit.
- The **Attributes package** stays **MIT** — unchanged. Its rationale was
  never about the core's licence: it is vocabulary meant to spread, and MIT
  remains the lowest-friction choice for seven inert classes.
- Every release archive ships `LICENSE`, satisfying Apache-2.0 §4(a).

Changing this again is no longer cheap. After the first external
contribution it requires every contributor's consent; treat it as fixed.
