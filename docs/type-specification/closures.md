# Closures and Callables

**Status: implemented** (ADR-0033).

## Closures are values

A closure is carried in the environment as a **proven closure value**, under the
same value discipline as any other binding: a reassignment or an invalidation
drops it, so a closure dies exactly like a scalar fact does.

```text
ClosureVal {
  target:   Scope(def_offset) | Named(fn_ref)
  captures: [(var, Fact)]       // by-value, snapshotted at creation
  def_line: u32
}
```

Two target forms exist: an anonymous closure or arrow function, addressed by the
byte offset of its definition site; and a **first-class callable** naming a free
function (`strtolower(...)`).

The capture snapshot is taken at **closure-creation time**, from the
definition-site environment. That is PHP's by-value capture semantics precisely:
mutating the captured variable afterwards does not change what the closure sees.
By-reference capture (`use (&$x)`) is a poison marker — see
[narrowing.md](narrowing.md).

At a branch join, two closure bindings survive only if they denote the *same*
closure (the same anonymous scope, or the same named function).

## Closure bodies are scopes

A closure body is a first-class analysis scope (`ScopeOwner::Closure`), carrying
its own parameters, native return type, effect origins, and throw origins.
Consequences:

- Binding descent can **descend into a closure body** with the arguments an
  invocation site provides.
- A closure is an effect node in the effect fixpoint, and a throw node in the
  throw fixpoint. Its effects are its caller's effects
  ([effects.md](effects.md)).

## Invocation shapes

Higher-order builtins are described by a small **invocation-shape catalog**
(`steins_catalog::invocation_shape`), so `array_map($cb, $xs)` can be treated as
*callback effects ∪ own effects* instead of an opaque taint — the redemption of
ADR-0005's `array_map` claim.

A shape records the callback's positional index, whether the invocation is
immediate or deferred, and where the callback's arguments come from:

| Function | Callback at | Invocation | Argument source |
| --- | --- | --- | --- |
| `array_map` | 0 | immediate | elements of param 1 |
| `array_filter` | 1 | immediate | elements of param 0 |
| `array_walk` | 1 | immediate | elements of param 0 (first callback param is by-ref) |
| `usort` / `uasort` / `uksort` | 1 | immediate | not element-shaped |
| `array_reduce` | 1 | immediate | not element-shaped |
| `call_user_func` / `call_user_func_array` | 0 | immediate | not modeled |
| `register_shutdown_function` | 0 | **deferred** | not modeled |
| `preg_replace_callback` | 1 | immediate | not modeled (match arrays) |

The table exists because the argument order is genuinely irregular —
`array_map` puts the callback first, `array_filter` second — and a rule would be
wrong. A function absent from the table is **not** treated as a higher-order
invoker: its callback argument stays an opaque taint, which is the FP-safe side.

"Not element-shaped" means effects and throws still join, but value folding does
not apply. `array_walk`'s by-ref first callback parameter additionally blocks
binding descent — a by-ref parameter cannot be soundly value-bound — while its
effects still join.

## Callable-signature variance

When a closure is passed to a parameter declaring a `callable(P1, P2=): R`
signature, the closure is judged **arm-wise** against it: parameters
contravariantly, return covariantly.

Two boundaries:

- A **template-bearing** signature (`callable(T): T`) is never lowered to a
  checked signature. It drops to a bare `callable`, per ADR-0032/ADR-0051's
  refusal of a call-site template solver, so every checked signature arm is a
  ground contract type.
- By-reference parameters (`&$x`) are skipped by the variance check.

Value acceptance ignores the signature entirely: a runtime string or array value
cannot be judged against a call shape ([contract-types.md](contract-types.md)).

## Liskov as a standing rule

Any envelope on an abstraction binds every implementation and override
(ADR-0033). Implementations may be **purer, narrower-throwing, wider-in,
narrower-out** — never the reverse. This applies to:

- **Effects** — `effect.liskov-widened` fires when a method's *proven* inferred
  effects exceed the envelope declared on the class or interface method it
  overrides or implements. The exhaustiveness-tainted (unknown) remainder stays
  silent; only the proven subset judges.
- **Throws** — `throw.liskov-widened` fires when an override's declared
  `@throws` names a checked class that is a subclass of none of the parent's
  declared classes. It requires **both** sides to declare `@throws`; a `Maybe`
  resolution is silent. See [throws.md](throws.md).

The term is always written out as "Liskov" in this project. "LSP" is reserved
exclusively for the Language Server Protocol.

## Not implemented

- **Callable signatures inferred from a callback's use site.** The variance
  check consumes a *declared* signature only.
- **`Closure::bind`/`bindTo`/`fromCallable` rebinding semantics** — a rebound
  closure is not tracked; the binding drops.
- **Method first-class callables** (`$obj->method(...)`, `Foo::bar(...)`) as
  closure targets — only free functions are a `Named` target today.
