# Throw Accounting

**Status: implemented** (ADR-0007, ADR-0040). The default posture of
`throw.undeclared` is an **open user decision** (roadmap gate G1); see below.

## Checked and unchecked

Not every exception is worth declaring. Steins splits the `Throwable` tree the
way ADR-0007 decided:

| Family | Class | Rationale |
| --- | --- | --- |
| `Error` and descendants | **unchecked** | engine faults — a `TypeError` is a bug to fix, not a contract to declare |
| `LogicException` and descendants | **unchecked** | programmer errors by definition |
| everything else under `Throwable` | **checked** | the recoverable, declarable ones |

The classification is computed through the same hierarchy walk as `is_a`, so it
is trinary: a class whose chain leaves the project and the builtin exception
table is **unknown**, and unknown never counts as checked. Only proven-checked
classes participate.

## Origins

Throw origins are produced by a **structural CST walk, independent of the trace
IR**, for *all* functions and methods — the fixpoint propagates callee throw sets
to callers whether or not anything is annotated.

| Origin | Contributes |
| --- | --- |
| `throw new X(...)` | `X` as written, resolved project-wide |
| `throw $e` where `$e` is an enclosing catch parameter | exactly that catch's absorbed set (rethrow precision) |
| a statically-named function call | the callee's escaping throws |
| a resolvable method/static call | the same, through the class-world edge |
| a higher-order call with resolvable callbacks | callee throws ∪ callback throws |
| a resolved `$fn()` call | the callback's throws |
| `throw $x` of a non-catch variable, `throw <expr>`, a dynamic call | **nothing reportable** — taints throw-exhaustiveness |

## Damming

The one effect that dies (ADR-0040). Each origin carries its enclosing `try`
catch-guards, **innermost first**, and a throw is matched against each guard from
the inside out:

```php
try {
    mayThrowFoo();          // origin, guarded by [catch (FooException)]
} catch (FooException $e) { // dams it — FooException does not escape
}
```

The matching is trinary and **consumer-inverted**: absorption must be *proven* to
remove a throw, so where the is-a relation between the thrown class and the
caught class is `Maybe`, the throw is treated as *escaping*. Two specific
conservatisms:

- A multi-catch `catch (A|B $e)` records several classes; a caught type the
  lowering cannot name statically forces the whole clause's absorption to
  `Maybe`.
- `finally` bodies and a `try`'s own catch bodies do **not** carry that `try`'s
  guard — a throw from inside a catch block is not dammed by the catch it is in.

The fixpoint is therefore:

```text
throws(f) = escaping own-throws(f) ∪ ⋃ filter(throws(callee), caller-guards)
```

monotone to a fixpoint, with an exhaustiveness bit tainted by dynamic or
unresolved calls and by opaque throws — mirroring the effect pass exactly.

## `@throws` envelopes

`@throws` is the declaration side, and it stays **Throwable-only** — it is not
the effect syntax (ADR-0006).

`throw.undeclared` (contract layer) fires when a **checked** exception
**provably escapes** (`Yes`) a function or method whose docblock declares
`@throws`, and is a subclass of **none** of the declared classes. Only proven
escapes report; a `Maybe` escape and an unknown hierarchy stay silent.

An undocumented function is never a finding: absent a declaration there is no
contract to violate. This is the envelope discipline from
[effects.md](effects.md), applied to throws.

`throw.liskov-widened` (contract layer) fires when an override or implementation
declares an `@throws` naming a checked class that is a subclass of none of the
parent method's declared classes. It requires **both** sides to declare
`@throws`; a `Maybe` resolution is silent.

## The `origin` facet

`throw.undeclared` is the only id in v1 that carries a **facet** — an additional
registry-declared classification axis a profile can select on (ADR-0050 §4):

| Value | Meaning |
| --- | --- |
| `direct` | the escaping throw's origin site is in the annotated declaration's own body |
| `propagated` | the origin is elsewhere, reached through one or more call hops |

The split is measurement-driven, not aesthetic: on the private legacy monorepo,
**158 direct versus 43,805 propagated**. A `@throws` that is wrong about the
method you are reading is a different kind of finding from one that is wrong
because something eight frames down changed. The built-in `throws-direct` profile
selects `origin = direct`.

## Gate G1: the default posture

**Open, and deliberately not pre-decided here.** `throw.undeclared` is a
contract-layer id, so a bare `steins check` does not print it; reaching it
requires `--profile throws-direct` or `--profile contracts`. Whether that is the
right long-term default — keep it behind profiles, split it, or promote the
direct arm — is the user's call, and it blocks the M2 milestone exit. See
[`docs/ROADMAP.md`](../ROADMAP.md).

## Not implemented

- **`never` return-type interaction** beyond the `exit`/`die` control effect
  (ADR-0019 covers the division of labor; the type side is not built).
- **Exception-shape reasoning** — the thrown *value* is not modeled, only its
  class.
- **Per-throw suppression finer than the id** — the `@steins-ignore` channel
  works at id granularity.
