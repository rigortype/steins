# Sidecar protocol: JSON-RPC over stdio, single-file runner, four core methods

Concretizes ADR-0004. **Wire**: JSON-RPC 2.0 over stdio with NDJSON framing —
the PHP side needs only `json_encode`/`json_decode`, zero dependencies on any
8.1+ runtime (requiring a composer install for the sidecar would break "the
project's own PHP as-is"), and the format family matches LSP/MCP so debug
tooling is shared. Binary formats are not worth their complexity at folding
payload sizes.

**Runner**: a single PHP file embedded in the steins binary, written to a
temp dir and launched as `php steins-runner.php` (the rigor-rs pattern). The
project's autoload is NOT loaded until required — folding never needs it;
only plugin bootstrap (ADR-0012) reads `vendor/autoload.php` — keeping the
contamination surface minimal.

**Core methods** (all idempotent and stateless, so a sidecar restart is
transparent — crash tolerance for long LSP sessions from day one):

- `fold(function, args)` → `{value}` | `{throw: class}` | `{widen: reason}` —
  an exception is a *result*, not an error: folding `1/0` returns
  `DivisionByZeroError` as type information, the measurement path for
  ADR-0008's `throw<E…>` payload.
- `reflect(target)` → signatures/attributes/constants — feeds the catalog
  audit (ADR-0014) and attribute reading. *Amended (issue #269): the
  class-world half is its own method, `reflect_class(target)` → the
  resident class-like's methods with their signatures, its constants, its
  properties and its hierarchy edges, plus the origin (`internal`,
  `extension`). Split rather than folded into `reflect` because that
  method is asked per function name on the hot path and must stay a
  small reply, while a class declaration is the largest payload on this
  wire. A declaration parses whole or not at all: a reply whose members
  do not read cleanly is unanswerable, never a class with fewer members
  — "we could not read them" must not be confusable with "it has none".
  The answer resolves; it does not convict (owner ruling, 2026-08-09):
  no absence-family finding is premised on a reflected declaration's
  completeness.*
- `env()` → PHP version, loaded extensions, relevant ini — coverage-posture
  material.
- `plugin(id, request)` — the ADR-0012 seam (stub initially).

Timeouts and the safe fallback to `widen` are part of the protocol spec:
sidecar misbehavior must never surface as a wrong diagnostic — the zero-FP
bulwark.

## Amendment (2026-10-02): the boot is not a request (issue #891) — PENDING ratification

The per-request timeout is charged from the write, so on a fresh child it used
to include PHP's own startup. On a loaded machine that cost the first answer,
then up to three respawns, then (ADR-0092's #784 amendment) the generation. A
child now has to answer an `env` handshake under its own boot timeout (20 s)
before the first request is sent; real requests keep the 2 s budget, so hang
detection on a running child is unchanged. The budget is read from
`STEINS_SIDECAR_BOOT_TIMEOUT_MS` (milliseconds, once per process; unset,
unparsable or zero mean 20 000), which is also how a test sees a hung boot fail
without waiting it out.

A child that fails the handshake (no answer in time, exit, garbage, a wrong id,
or a closed pipe) is a failed spawn, on a first spawn, or one respawn strike,
on a revive. The two failures of a spawn stay distinct (issue #110): `php` that
cannot be started at all is the sound subset and prints the "no PHP sidecar"
notice; `php` that started and failed its boot is a *degraded* run, prints the
"sound subset (degraded)" notice, once, and counts as one lost answer. The
engine is off for the rest of the run in both cases. A lost answer withholds
the publish (ADR-0092's #784 amendment), so a run that met a broken `php`
leaves no generation behind for later runs to replay, while a run with no `php`
at all still publishes under its engine-off stamp, as before.
