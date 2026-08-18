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

A `Stmt` carries its kind, its whole-statement span, and `invalidated` — one
`InvalidatedVar { name, opaque, sites }` per variable passed as an argument to
*any* call within it. Every named variable is marked unknown *after* the
statement: PHP by-reference parameters could have mutated them, and an unseen
`&$x` signature must not be trusted.

Each entry also carries the ADR-0070 evidence for its name. The syntax layer
decides nothing with it — it knows no signatures — but it owns the invariant
the walk relies on: **`sites` lists every occurrence of the name in the
statement's call arguments as a `(callee, position)` pair, or `opaque` is set
and `sites` is empty** — there is no third state. One method call, dynamic
callee, named argument, spread, closure-body occurrence or echo-embedded write
turns the entry opaque and discards its sites. The walk then declines the drop
for a non-opaque entry whose every site resolves to a by-value parameter
(`Param::by_ref == false`, or a builtin position `steins_catalog::by_value_arg`
certifies) and whose binding is value-semantic rather than an object handle;
an opaque entry keeps the blanket drop.

## Statement kinds

| Kind | Models |
| --- | --- |
| `Assign { var, value, call }` | `$var = <value>` to a bare local; `call` carries the full `CallExpr` when the rvalue *is* a named call, so argument spans survive |
| `PropAssign { target_var, prop, value, value_call }` | `$o->p = <rvalue>` / `$this->p = …` with a static property name |
| `Call(CallExpr)` | a statement-position call |
| `Return { value, call, span }` | `return <value>` (`Other` for bare `return`) |
| `Echo(Vec<CallExpr>)` | `echo e1, e2` — carries named calls among the operands |
| `If { cond, then_trace, elseifs, else_trace }` | structured branches, recursively lowered |
| `Match { subject, arms, default, loose }` | `match` (strict, first-match, throws on no match) and `switch` (loose, falls through) |
| `Assert { cond }` | `assert($expr)` with a lowerable condition |
| `Throw { span }` / `Exit { span }` | trace terminators |
| `Opaque { writes, reads, poisons, may_return }` | a recognized control-flow construct whose internals are not modeled but whose write and read sets are; `may_return` is true when the subtree contains a `return` the walk cannot see as a top-level `Return` |
| `Barrier` | anything unmodeled *and* unbounded — `goto`, labels, `declare`, `__halt_compiler`. Erases all known values |

Compound assignment (`+=`, `.=`) lowers its value to `Other` — the statement is
modeled, the value is not. A dynamic property name (`$o->$p = …`) or a chained
lvalue (`$a->b->c = …`, `Foo::$s = …`) is a `Barrier`, never a `PropAssign`.

### Value-position `match`

A `match` whose result is consumed rather than discarded — `$r = match (…)`,
`return match (…)`, `echo match (…)`, `f(match (…))` — lowers to a `Match` entry
of its own, placed in the trace immediately **ahead of** the statement that
consumes it. The consuming statement is lowered exactly as it would be without
the `match`, so what this buys is the walk and nothing else: per-arm first-match
certainty, dead-arm marking, and every diagnostic an arm body emits.

The value stays out of it on purpose. `lower_arg_value` answers `Other` for a
`match` and `named_call` answers `None`, so the consuming statement's value lane
is what it always was; joining the arm values into the expression's result is a
separate question with its own consequences for return typing.

Only the positions PHP evaluates in the statement's own entry env are read — an
expression statement, `return`, and the two `echo` forms — the same boundary the
string-context scan draws. A `match` in an `if` condition or a loop header is
evaluated in an env this pass does not hold, and stays unstructured. A match-arm
body gets the same treatment, so a `match` nested inside one is walked too.

### All-or-nothing structuring

`match`/`switch` reaches the structured form only when the subject and every arm
condition lower to a bare variable or a literal, and (for `switch`) every
non-empty case terminates without fall-through. One unrepresentable arm makes
the **whole** construct `Opaque`. Partial structuring would be unsound for
`match`'s first-match rule and its `\UnhandledMatchError` on no match. A refused
`match` in value position is not descended into either, for the same reason: an
arm of an unstructured outer `match` is not a position the walk can claim is
reached.

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

**It stays a separate surface now that the value IR carries method calls too**
(issue #386: `ArgValue::MethodCall`, and `Receiver::New` carrying the
constructor's arguments). The two answer different questions and neither can
stand in for the other. `method_calls` must be **comprehensive** — one call it
misses is a rewrite that breaks a caller — so it is a structural scan that
takes every call at every depth, resolvable or not, and it keeps the calls
themselves. The value IR is **representational**: it carries the calls it can
say something about, and a dynamic method name, a spread argument list or a
deeper receiver chain still lowers to `ArgValue::Other` there. A carrier that
declines is silence in the checker and would be a wrong rewrite in the
transform.

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
