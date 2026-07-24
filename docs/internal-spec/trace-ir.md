# The Trace IR

**Status: implemented** (`steins-syntax`; ADR-0027, ADR-0031, ADR-0033).

## Shape

Propagation runs over a per-scope **trace**: a list of `Stmt`s the analyzer
understands, with everything it does not understand lowered to an explicit
unknown. It began linear (ADR-0027) and grew structured control flow one
construct at a time (ADR-0031) — the **ratchet**: what the trace does not model
is visible *in the IR*, not hidden in the walk.

```text
Scope {
  owner:        TopLevel | Function(name) | Method{class, method} | Closure{def_offset}
  function_name: Option<String>          // free functions only
  poisoned:     bool
  stmts:        Vec<Stmt>                // the trace
  method_calls: Vec<CallExpr>            // comprehensive, see below
  params:       Vec<Param>               // closure scopes
  ret_ty:       Option<NativeType>       // closure scopes
  …closure effect/throw origins
}
```

A `Stmt` carries its kind, its whole-statement span, and `invalidated` — the
variables passed as an argument to *any* call within it. Those are marked
unknown *after* the statement: PHP by-reference parameters could have mutated
them, and an unseen `&$x` signature must not be trusted.

## Statement kinds

| Kind | Models |
| --- | --- |
| `Assign { var, value, call }` | `$var = <value>` to a bare local; `call` carries the full `CallExpr` when the rvalue *is* a named call, so argument spans survive |
| `PropAssign { target_var, prop, value, value_call }` | `$o->p = <rvalue>` / `$this->p = …` with a static property name |
| `Call(CallExpr)` | a statement-position call |
| `Return { value, call, span }` | `return <value>` (`Other` for bare `return`) |
| `Echo(Vec<CallExpr>)` | `echo e1, e2` — carries named calls among the operands |
| `If { cond, then_trace, elseifs, else_trace }` | structured branches, recursively lowered |
| `Match { subject, arms, default, loose }` | statement-position `match` (strict, first-match, throws on no match) and `switch` (loose, falls through) |
| `Assert { cond }` | `assert($expr)` with a lowerable condition |
| `Throw { span }` / `Exit { span }` | trace terminators |
| `Opaque { writes, reads, poisons }` | a recognized control-flow construct whose internals are not modeled but whose write and read sets are |
| `Barrier` | anything unmodeled *and* unbounded — `goto`, labels, `declare`, `__halt_compiler`. Erases all known values |

Compound assignment (`+=`, `.=`) lowers its value to `Other` — the statement is
modeled, the value is not. A dynamic property name (`$o->$p = …`) or a chained
lvalue (`$a->b->c = …`, `Foo::$s = …`) is a `Barrier`, never a `PropAssign`.

### All-or-nothing structuring

`match`/`switch` reaches the structured form only when the subject and every arm
condition lower to a bare variable or a literal, and (for `switch`) every
non-empty case terminates without fall-through. One unrepresentable arm makes
the **whole** construct `Opaque`. Partial structuring would be unsound for
`match`'s first-match rule and its `\UnhandledMatchError` on no match.

### `Opaque` versus `Barrier`

`Opaque` is the ratchet applied to what used to be a blanket `Barrier`. Instead
of erasing everything, the walk forgets:

- **`writes`** — over-approximated: every assignment lvalue, compound assign,
  `++`/`--`, `foreach` value/key binding, `catch` parameter, `list()`
  destructuring, *plus* every variable handed to any call inside the subtree.
- **`reads`** — every *other* variable the subtree merely mentions, conditions
  included. A construct that reads a variable may branch on it and early-return,
  so the fall-through path can exclude the currently-known value; keeping it
  would assert an unreachable path. This closed a real soundness hole.
- **everything**, when `poisons` is set.

Nested function and closure bodies are separate scopes and are never descended
for either set. Over-collection is always sound: it forgets more.

## Scope poisoning

`Scope::poisoned` is set by any ADR-0001 give-up construct in the body:
`extract`/`compact`, `global`, `static $x`, variable-variables, reference
assignment, by-ref closure capture, `include`/`require`/`eval`. In a poisoned
scope **no variable value is ever considered known**.

`Opaque { poisons: true }` is the local form of the same fact: the enclosing
scope is independently poisoned too.

## `method_calls`: the sound enumeration surface

`Scope::method_calls` lists **every** instance/static method call in the body —
including calls nested inside sub-expressions the linear trace drops to `Other`
— in source order, without descending into nested function/closure/class bodies.

It exists for the transform engine's reverse sweep (ADR-0043 §6): a candidate
method is safe to rewrite only when *every* call that could reach it is
accounted for, so a nested `$this->m($bad)` must be visible even though the
trace never modeled it. Constructor (`new`) calls are omitted — a constructor is
never a transform candidate.

The distinction is worth stating plainly: `stmts` is what the *checker* walks;
`method_calls` is what the *transform* enumerates. They have different
completeness requirements and are therefore different surfaces.

## Effect and throw origins

Effect and throw origins are **not** in the trace. They are produced by a
separate structural CST walk over the whole body — including constructs nested
inside control flow the trace erases — because the effect and throw fixpoints
propagate callee sets to callers regardless of annotations, and must not miss an
`echo` inside a loop.

That scan is deliberately *not* reachability-aware: an effect origin in provably
dead code still counts, because an envelope is a contract about the function's
code, not about one execution path.

## Not implemented

- **Loop bodies as traces.** Loops are `Opaque`.
- **`try`/`catch`/`finally` as trace structure.** `Opaque` for value flow; the
  catch *guards* are carried on throw origins separately.
- **Expression-position `match`.**
- **Array element tracking.** `ArgValue` carries array literals; there is no
  per-element fact lane.
