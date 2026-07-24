# Dynamism and Absence Proofs

**Status: partial** — the dam is computed and consumed by the absence ids that
have landed; the ids waiting on ADR-0049 S4 are marked below. ADR-0046,
ADR-0049.

## Unanalyzability, not nondeterminism

`eval`, dynamic `include`, `unserialize`, variable-variables: the honest posture
is that these make parts of the program **unanalyzable**, not that they make it
nondeterministic (ADR-0046). The distinction matters because it decides what
Steins does about them — it does not model them probabilistically or heuristically;
it identifies precisely which *claims* they invalidate and withholds those.

## What an absence proof needs

Most findings claim "this value breaks here". The **finding-breadth family**
claims something harder: "this thing does not exist". A negative claim needs
**closure** over every place the thing could hide.

For method absence, closure is over the class hierarchy — and PHP cannot reopen
a defined class, so a fully-enumerated hierarchy is genuinely closed. This is the
**immunity asymmetry** of ADR-0049 §2: method-absence claims need no dam.

For function and class *existence*, closure is over the whole universe of names —
and dynamic code can mint names no reference scan will ever see. Those claims
need the dam.

## The dam

The dam is a whole-universe fact, recomputed per run from the lowered project.
It is a **query answer**, with no entry state, no ordering dependence, and no
cross-scope coupling (the ADR-0048 constraint).

A dam site is any of:

| Kind | Site |
| --- | --- |
| `Eval` | every `eval(...)` — code as data, universe havoc |
| `Include` | every **non-vendor** `include`/`require` whose path is not provably in-universe |
| `ClassAlias` | every **non-literal** `class_alias(...)` — a runtime class-name mint |

"Not provably in-universe" is deliberately strict for includes: an unproven
path, a bare-relative literal (the runtime resolves it against `include_path`,
then the script directory, then the CWD — verified empirically on PHP 8.5.8),
a `./`-prefixed literal (verified to bind to the **CWD**, not the including
file's directory — `./` does not anchor), or an absolute / `__DIR__`-anchored
literal that resolves *outside* the analyzed universe. Directory-relative
belief is unsound in every one of these shapes.

The **vendor presumption**: `eval` and dynamic includes inside a `vendor/` path
are Composer plumbing and are presumed universe-internal. A *literal*
`class_alias` is never a dam site at all — it contributes an index edge.

With the dam raised, existence claims are withheld for the whole run.

## The sidecar's role

Absence proofs additionally require a **live PHP sidecar**, and the whole family
goes silent without one (the sound subset — [overview.md](overview.md)). Two
questions only the runtime can answer:

1. **Is this name resident on this PHP?** A function or class may exist as a
   builtin or in a loaded extension without appearing in any source file. The
   sidecar's `reflect` answers with a *structured not-found* rather than an
   error, so "definitely absent" and "unanswerable" stay distinguishable.
2. **The homonym question.** A textual class in the project may be dead code
   shadowed by a loaded builtin of the same name. Without asking, the question
   has no textual answer.

The family is additionally disabled outright when a **runtime-redefinition
extension** (`uopz`, `runkit7`, `Componere`) is loaded: with any of those
present, no absence claim holds.

## The family

| Id | Layer | Requires | Status |
| --- | --- | --- | --- |
| `call.undefined-method` | proof | proven-exact receiver, fully-enumerated hierarchy, no `__call`, no trait obstacle, no builtin homonym | **emits** |
| `offset.missing` | proof | a fully-proven container value and a provably-absent key | **emits** |
| `offset.on-unsupported` | proof | a proven non-offsetable base (object → fatal; scalar/null → warning) | **emits** |
| `call.too-few-arguments` | proof | a uniquely-resolved target, fewer positional args than required params | **emits** |
| `call.unknown-named-argument` | proof | a named argument binding no parameter of a resolved non-variadic target | **emits** |
| `phpdoc.undefined-method` | contract | narrowed declared-receiver arms, each under descendant closure | **emits** |
| `call.undefined-function` | proof | the dam clear, no candidate FQN, sidecar not-found | registered, **not emitted** (ADR-0049 S4) |
| `class.undefined` | proof | the same, at a hard-error position (`new`, static call, class-const fetch) | registered, **not emitted** (S4) |
| `call.too-many-arguments` | proof | an **internal** non-variadic target | registered, **not emitted** (needs the reflect slice) |

The arity table is **asymmetric on purpose**, and every row was `php -r`-verified
on PHP 8.5: too few arguments to a userland target is always a fatal
`ArgumentCountError`; too *many* to a non-variadic userland function runs clean
(extras are ignored) and is therefore **never a finding**, whatever PHPStan
reports. Consequences beat conventions.

## The vouch valve

For the transform engine, where "all callers are proven" must hold over a whole
universe, a user can *vouch* that a specific dynamic-code site does not mint the
names in question:

```toml
# steins.toml
[transform.vouch]
sites = ["src/Legacy/Loader.php:88"]
```

A vouched run **downgrades its completeness claim** loudly: the report says the
claim is conditional on N user-vouched exemptions and names them. A vouch that
matches no obstacle is reported as a no-op rather than silently ignored.

The vouch valve is transform-side only. Checker-side region scoping (ADR-0047
§9) is deferred; the checker's dam is whole-universe.

## Not implemented

- **Checker-side vouching and region scoping** — the checker is whole-universe.
- **`unserialize` shape recovery** — named in ADR-0046 as unanalyzable; no
  machinery models it.
- **Reflection-driven dispatch** (`call_user_func` with a computed name,
  `$class::$method()`) — an opaque taint, never a proven edge.
