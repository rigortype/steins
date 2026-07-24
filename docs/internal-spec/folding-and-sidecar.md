# Folding and the PHP Sidecar

**Status: implemented** for `env`, `fold`, and `reflect`. The `plugin` method is
a documented stub. ADR-0004, ADR-0008, ADR-0024.

## What folding is

**Folding** is evaluating an expression to a value-precise type at analysis time
by *executing the real PHP function* in the sidecar. It is not constant
propagation — that is the static notion, and this is the other thing.

The reason to execute rather than model: a folded value is what this code
produces on the runtime it actually runs on — the project's own PHP version,
extensions, and configuration. No emulation matrix can promise that (ADR-0004).

## The `Folder` seam

The engine never talks to the sidecar directly. It talks to a trait:

```rust
trait Folder {
    fn fold(&mut self, name: &str, args: &[ArgValue]) -> Option<ArgValue>;
    fn absence_family_available(&mut self) -> bool { false }
    fn boot_surface_class_like(&mut self, fqn: &str) -> Option<bool> { None }
    fn boot_surface_function(&mut self, fqn: &str) -> Option<bool> { None }
    fn php_minor(&mut self) -> Option<(u16, u16)> { None }
}
```

Every default is the conservative answer, so **the sound subset is what you get
by implementing nothing**. `NoFold` is literally the defaults. `php_minor` is
the ADR-0052 A11 version-skew input: `None` means "no detectable skew", so the
catalog pin stands.

`absence_family_available()` returns true only when a live sidecar is answering
*and* no runtime-redefinition extension (`uopz`, `runkit7`, `Componere`) is
loaded — read from `env`'s extension list and memoized as a whole-run property.
With any of those present, no absence claim holds at all. The
`boot_surface_*` homonym answers are memoized per FQN the same way, so a
repeated chain class never re-asks the sidecar.

## The folding gate

An expression folds only when three things hold:

1. The callee is on the catalog's **folding allowlist** — a hand-picked set of
   builtins that are pure and deterministic under ADR-0008's rule (empty effect
   set, no `nondet` on the concrete path). See [catalog.md](catalog.md).
2. The callee is **not a user function** — user functions are propagation edges,
   not folds.
3. Every argument is a **literal the trace IR carries** — `int`, `float`,
   `string`, `bool`, `null`.

An allowlist entry is *permission* to fold, not a promise that a call folds.
Several allowlisted functions (`sprintf`, `implode`, `in_array`, `count`) commonly
take array arguments, which the fold protocol cannot yet carry; those calls
simply do not qualify, and light up automatically when array arguments arrive.

## The protocol

JSON-RPC 2.0 with NDJSON framing over the child's stdin/stdout. The PHP side is
a **single, dependency-free file** (`runner.php`) embedded in the binary via
`include_str!` and written to a per-process temp dir, launched as
`php <runner>`. `php` is resolved from `PATH` at spawn time — the *project's
own* PHP.

| Method | Answers | Status |
| --- | --- | --- |
| `env` | `{php_version, extensions, sapi}` — coverage-posture material and the PHP-minor check for catalog version skew | implemented |
| `fold` | a call's value, tagged with its PHP type | implemented |
| `reflect` | whether a name is a resident function and/or class-like on this PHP, autoload **disabled** | implemented |
| `plugin` | — | **stub**: returns `{kind: "widen", reason: "unimplemented"}` |

`reflect`'s reply is always structured: a name that exists nowhere is a
*structured not-found* (`exists: false`), never an error. Only a malformed
request widens. The distinction is load-bearing — "definitely absent" and
"unanswerable" must not collapse into each other, or an absence proof becomes
unsound.

Autoload is deliberately disabled: the sidecar runs no project autoloader, and
the question is strictly "is this name resident on this PHP".

`fold` returns one of three outcomes, and the middle one is the interesting one:

```text
Value(FoldValue)          // Int | Float | Str | Bool | Null
Throw { class }           // an exception is a RESULT, not an error: 1/0 → DivisionByZeroError
Widen { reason }          // anything we cannot turn into type information
```

## The zero-FP contract under failure

Binding, from ADR-0024:

> Sidecar misbehavior must NEVER become a wrong diagnostic.

Every failure mode — spawn failure, IO error, per-request timeout, malformed
response — maps to `Widen`, never to a value. On any such failure the child is
killed and the instance is **poisoned**: later calls widen immediately rather
than hanging or reviving a half-dead process.

Default per-request timeout: 2 seconds. Generous for a local `php` call;
anything slower is treated as misbehavior.

## Concurrency model

No async runtime. A single background thread drains the child's stdout into a
channel; each request writes a line and waits with `recv_timeout`. Requests are
strictly serialized (`&mut self`) and **stateless**, so a restart would be
transparent to the caller — which is also the property an LSP session needs to
survive a sidecar kill without a wrong or lost diagnostic.

## The coverage posture

A run without a sidecar prints one line to stderr and continues:

```text
note: running as sound subset (no PHP sidecar) — findings that require executing PHP are omitted
```

The zero-FP bar still holds; the run is *incomplete*, not *degraded*, and it says
so. Naming the guarantee rather than the deficiency is deliberate vocabulary.

## Not implemented

- **The `plugin` method's behavior** — the seam exists, nothing is behind it
  (ADR-0012, ADR-0039). See [plugin-contract.md](plugin-contract.md).
- **`reflect` in class resolution.** It answers the absence family's homonym
  question; classes from unloaded extensions are still `Unknown`-silent in
  ordinary type resolution.
- **Array-valued fold arguments and results.**
- **The pseudo-constant settings opt-in** that would let locale- and
  timezone-sensitive functions fold (ADR-0008).
- **`doctor`**, which would surface `env` and sidecar health as a user-facing
  posture report (ADR-0054).
