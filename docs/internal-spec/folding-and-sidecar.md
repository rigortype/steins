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
    fn boot_surface_label(&mut self) -> Option<String> { None }
    fn builtin_return_fact(&mut self, name: &str) -> Option<Fact> { None }
}
```

Every default is the conservative answer, so **the sound subset is what you get
by implementing nothing**. `NoFold` is literally the defaults. `php_minor` is
the ADR-0052 A11 version-skew input: `None` means "no detectable skew", so the
catalog pin stands. `boot_surface_label` (ADR-0049 §9) is the boot surface's
self-description for the existence-id message register (`PHP 8.5.8 (32
extensions)`); `None` falls back to a version-agnostic phrasing.
`builtin_return_fact` (ADR-0056 R1) is the value-domain return fact of a
uniquely-resolved builtin — the reflected return envelope refined by an admitted
curated row, always seeded at the `Verified` stratum; `None` when nothing may be
seeded (no sidecar, a monkey-patch extension loaded, an unknown name, or a
return type not representable as a single value-domain `Fact`).

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
3. Every argument resolves to a **concrete value**: a scalar literal (`int`,
   `float`, `string`, `bool`, `null`), an **array literal** every element of
   which — key and value, at every depth — is itself concrete (ADR-0028's
   2026-07-26 amendment, issue #39), or a bounded `OneOf` union of such values,
   which folds member-wise (the 2026-08-01 amendment, issue #74). One unproven
   element anywhere widens the whole array — `count([1, $x])` is not `2`,
   because `$x` is not known to be one entry.

An allowlist entry is *permission* to fold, not a promise that a call folds —
the gates below still apply per call.

### Arrays cross the seam in both directions

One envelope serves both directions: an ordered entry list
(`{"__steins_array": [[key, value], …]}`, nested recursively), with **PHP
owning array semantics on the wire** (ADR-0004 — no array rule is
reimplemented in Rust to be wrong about later). The *argument* direction
spells an absent key as `null`, so the engine assigns its own next-int and its
own last-wins on duplicates. A *result* is an array PHP has already finished
building — every key materialized, duplicates resolved, `"5"` cast to `5` —
so the result decoder **rejects** those spellings as malformed rather than
re-deriving in Rust a choice the engine already made; a runner bug of that
class becomes a widen. The folded array lands on the concrete path
(`Val::Array` → `Fact::Singleton`), never a synthesized shape (ADR-0028's
2026-08-14 amendment, issue #330).

Both directions are charged against the same **array budget**: at most 256
entries counted recursively, at most 8 levels of nesting; over either bound
the call widens. An argument is charged before the memo key is built, so a
generated lookup table is never cloned, hashed, serialized, or executed. A
result is charged on **both sides of the wire** — in the runner before
encoding, so an oversized answer never becomes a megabyte of JSON, and again
on arrival, through the argument side's own gate, so the two verdicts are the
same computation and cannot drift. The invariant is symmetry: a shape
admissible as an argument is admissible as a result.

### The integer-width gate

Behind the allowlist sits the width gate (issue #64). The catalog's primitive
is `width_class(name)` — `foldable` is derived from it, so "on the allowlist"
is exactly "has a width verdict at all". Three classes, split by *evidence*:

- **`Safe`** — verified by differential probes, 32-bit against 64-bit.
- **`Refused`** — one recorded divergence per row (`sprintf("%x", -1)`).
- **`Unverified`** — no evidence, and zero probes is the correct number:
  nobody looked (ADR-0028's 2026-08-14 amendment §4). This is where
  array-returning names (`array_merge`, `explode`) sit today; the promotion
  path is php-wasm differential probes, then `Safe`.

What the class buys depends on the engine. On a **provably 64-bit** engine
(`PHP_INT_SIZE = 8`, read from `env` and memoized) all three classes fold. On
a provably 32-bit engine (the browser's php-wasm), only `Safe` names fold, and
only for argument tuples whose every integer the range guard admits — both
legs are required, because the probe verdict is stated for exactly those
tuples. Anything else — an unreported width, a machine nobody has probed —
folds nothing. Default-deny throughout. `Refused` and `Unverified` are
mechanically identical here; they are kept apart because mixing unevidenced
rows into the refused list would erase its one-witness-per-row discipline
(see [catalog.md](catalog.md)).

### Admission: strictly stronger than the rung it shadows

A name joins the allowlist only when the fold is **strictly stronger** than
the Rust rung it would shadow. `explode`'s rung is type-level
(`non-empty-list<string>`), so the fold upgrades a type to a value on the
all-literal path — and the rung survives beneath it as the no-sidecar floor.
`array_slice`'s rung is already exact and covers non-literal elements a fold
never can, so it stays off the list: a fold there would buy a second
implementation of the same answer and a fixture to keep them agreeing
(`array_combine` and `array_fill_keys` are excluded the same way). The rule
cuts both ways — a name whose rung later becomes exact should *leave* the
allowlist.

## The protocol

JSON-RPC 2.0 with NDJSON framing over the child's stdin/stdout. The PHP side is
a **single, dependency-free file** (`runner.php`) embedded in the binary via
`include_str!` and launched as `php -r <source>` — the source (minus its
leading `<?php` tag, which `-r` forbids) passed as a single argv element,
never written to disk. `php` is resolved from `PATH` at spawn time — the
*project's own* PHP. stdin is reserved for the NDJSON request stream, so argv
is the only channel available to hand the runner its own source.

| Method | Answers | Status |
| --- | --- | --- |
| `env` | `{php_version, extensions, sapi}` — coverage-posture material and the PHP-minor check for catalog version skew | implemented |
| `fold` | a call's value, tagged with its PHP type | implemented |
| `reflect` | whether a name is a resident function and/or class-like on this PHP, autoload **disabled**; for a resident function also its **reflected return type** (`return_type`, with `return_type_tentative` when the engine carries only a tentative type) — the envelope the ADR-0056 return-fact seeder reads — and its **parameter counts** (`params_total`, `params_required`) | implemented |
| `plugin` | — | **stub**: returns `{kind: "widen", reason: "unimplemented"}` |

`reflect`'s reply is always structured: a name that exists nowhere is a
*structured not-found* (`exists: false`), never an error. Only a malformed
request widens. The distinction is load-bearing — "definitely absent" and
"unanswerable" must not collapse into each other, or an absence proof becomes
unsound.

Autoload is deliberately disabled: the sidecar runs no project autoloader, and
the question is strictly "is this name resident on this PHP".

The **parameter counts** (`ReflectionFunction::getNumberOfParameters()` /
`getNumberOfRequiredParameters()`) sit inside the same try/catch as the return
type, so a reflection failure leaves both `null` rather than guessing. They exist
for ADR-0064 Amendment B's mixed-pin ruling: a builtin declaring a bare `mixed`
return countersigns a transfer rule with nothing, so such a rule pins the live
*signature* instead. Absent counts — an older runner, a canned replay table
recorded before the field, a reflection failure — withhold the rule exactly as an
absent declaration does; **older replies keep parsing unchanged**, which is what
lets the pre-existing replay tables stay valid.

`fold` returns one of three outcomes, and the middle one is the interesting one:

```text
Value(FoldValue)          // Int | Float | Str | Bool | Null | Array
Throw { class }           // an exception is a RESULT, not an error: 1/0 → DivisionByZeroError
Widen { reason }          // anything we cannot turn into type information
```

## The zero-FP contract under failure

Binding, from ADR-0024:

> Sidecar misbehavior must NEVER become a wrong diagnostic.

Every failure mode — spawn failure, IO error, per-request timeout, malformed
response, a child that died outright — maps to `Widen`, never to a value. On any
such failure the child is killed and the instance is **poisoned**: the request in
flight is lost, and no half-dead process is ever trusted for an answer.

Default per-request timeout: 2 seconds. Generous for a local `php` call;
anything slower is treated as misbehavior.

### Poison is a lost answer, not a lost run

A child can die in ways PHP cannot catch. An allocation past `memory_limit` is a
FATAL, not a `Throwable`, and `str_repeat("x", 2000000000)` is an ordinary
literal call on the folding allowlist — so a single snippet could once kill the
resident runner and widen every later request in the run. Stack overflows and
extension segfaults are the same class.

Two defences, neither of which tries to predict the bomb:

* The runner pins `memory_limit = 256M`. It cannot make the fatal catchable; it
  bounds the blast radius, and it makes fold outcomes a property of the *code*
  rather than of the host's `php.ini`.
* The transport replaces a dead child. The request that killed it still widens
  and is **never** retried on the replacement — it is the likely bomb. The *next*
  request revives the instance, at most three times per `Sidecar` (the storm
  brake against input engineered to kill children; past it the instance is
  permanently poisoned, as it always was).

Respawn is deliberately blind to *why* the child died, which is what makes it
cover the whole class. The rejected alternative — a per-function result-size
budget in Rust, bounding `str_repeat`'s `count × strlen` and so on — needs
resource knowledge of every builtin, is silently incomplete for every future
allowlist member, and does nothing about a segfault.

Nothing is replayed into the fresh child, because the runner keeps no
cross-request state (see below).

## Concurrency model

No async runtime. A single background thread drains the child's stdout into a
channel; each request writes a line and waits with `recv_timeout`. Requests are
strictly serialized (`&mut self`) and **stateless**, which is precisely what
makes a restart transparent to the caller — the property the respawn above
relies on, and the one an LSP session needs to survive a sidecar kill without a
wrong or lost diagnostic.

## The coverage posture

A run without a sidecar prints one line to stderr and continues:

```text
note: running as sound subset (no PHP sidecar) — findings that require executing PHP are omitted, and builtin return types come from the catalog's declarations, unverified
```

The zero-FP bar still holds; the run is *incomplete*, not *degraded*, and it says
so. Naming the guarantee rather than the deficiency is deliberate vocabulary.

## Not implemented

- **The `plugin` method's behavior** — the seam exists, nothing is behind it
  (ADR-0012, ADR-0039). See [plugin-contract.md](plugin-contract.md).
- **`reflect` in class resolution.** It answers the absence family's homonym
  question; classes from unloaded extensions are still `Unknown`-silent in
  ordinary type resolution.
- **The pseudo-constant settings opt-in** that would let locale- and
  timezone-sensitive functions fold (ADR-0008).
