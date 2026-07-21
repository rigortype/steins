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
  audit (ADR-0014) and attribute reading.
- `env()` → PHP version, loaded extensions, relevant ini — coverage-posture
  material.
- `plugin(id, request)` — the ADR-0012 seam (stub initially).

Timeouts and the safe fallback to `widen` are part of the protocol spec:
sidecar misbehavior must never surface as a wrong diagnostic — the zero-FP
bulwark.
