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
  owner:        TopLevel | Function{name, fqn} | Method{class, method} | Closure{def_offset}
  function_name: Option<String>          // free functions only: the owner's fqn
  poisoned:     bool
  stmts:        Vec<Stmt>                // the trace
  returns:      Vec<ReturnSite>          // every `return`, bare or valued, see below
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

`unspelled` (issue #1048) names every variable the statement mentions when a
value its kind carries — an `Assign`, `PropAssign` or `Return` value, a call's
argument or receiver — lowered to `Other`. A `match`, an `(object)` cast or an
array with a computed key can store an object handle as surely as an array
literal can, and the walk escapes each object these names hold. `None` when
every value was spelled. `suspends` says that the statement's own expressions
(an expression, an `echo` or `return` operand, an `if` condition, a loop header,
a `switch` subject) `yield`: the generator's consumer runs before the statement
resumes, and with `unspelled` naming the operands the walk escapes them and
sweeps the escaped objects. `writes_through` lists the receivers those
expressions write a property through where the kind does not spell it
(`$y = $q->p = 1`, `if ($q->p = f())`, `foreach ($xs as $q->p)`); the leftmost
variable of the chain, or `""` for a chain that starts at no variable.
`Scope::yields_by_ref` marks a generator declared `function &g()`.

`read_positions` (ADR-0104, issue #1050) lists every call, offset read, property
read and class-constant read the statement's own expressions write, each as a
`ReadPosition { span, carrier, kind }` (40 bytes) at the read's own span, outermost
first and in source order. The kind is `Call` (by span alone; the lowered call is found through the tree,
the scope or `read_calls`), `Offset(place, key)` (a key is an integer or string literal, a bare
variable `Key::Var` resolved against the env, or `Other`), `Prop(var, prop)` or `ClassConst(class, name)`; every
level of an access chain and every call is a position, and `?->` and `Class::class`
are not. A write target, and an offset or property under `isset`, `empty`, `??`
or `??=`, are not recorded. The carrier names the construct around the read:
`Plain`, `Argument { call, position }` (`call` indexes the call's own position
in the list; `position` is `None` for a named argument or one at or after a
spread), `Conditional`, `IssetLike` (class constants only), `Suppressed`,
and `CompoundRead` (a first-class callable is the kind `CallRef`, its carrier folded
like a call's; the S2 row "FCC existence" consumes `kind == CallRef`); everything past a `?->` is `Conditional`, and the base of an append
is a write. A by-reference argument also silences the base levels of its chain
(`f($a['x']['y'])` gives both levels `Argument`), and `position: None` means the
by-reference parameter is unknowable, so a value-dependent family declines. The
rule table is in `lower_read.rs`. Under several constructs the weakest
claim wins, in the order on `Carrier`.
It is read for an expression statement, `return`, `echo` and `<?= ?>` and a
`static` initializer, and for an arrow function's body, a hook's and a hoisted
`match` arm's; a header is not read yet. The scan stops at a function-like, a
class-like (an anonymous class keeps its constructor arguments) and a `match`.
`None` when there is none. A call position's lowered `CallExpr` is found by its span (slice
S2): a function call in `SourceTree::calls` (`SourceTree::call_at`), a method or static call
and a method or static first-class callable in `Scope::method_calls`
(`Scope::method_call_at`), and a `new` of a named class and a function first-class
callable in `Stmt::read_calls`. A computed callee has no `CallExpr` to judge and a `new` of a
dynamic class has none at all. The call consumer is `stmt_calls/nested.rs`; the offset,
property and class-constant positions are not read yet.

## Statement kinds

| Kind | Models |
| --- | --- |
| `Assign { var, value, call }` | `$var = <value>` to a bare local; `call` carries the full `CallExpr` when the rvalue *is* a named call, so argument spans survive |
| `PropAssign { target_var, prop, value, value_call }` | `$o->p = <rvalue>` / `$this->p = …` with a static property name |
| `Call(CallExpr)` | a statement-position call |
| `Return { value, call, span }` | `return <value>` (`Other` for bare `return`; the bare and valued forms are told apart by `Scope::returns`, not by this value) |
| `Echo(Vec<CallExpr>)` | `echo e1, e2` — carries named calls among the operands |
| `If { cond, then_trace, elseifs, else_trace }` | structured branches, recursively lowered |
| `Match { subject, arms, default, default_lands, loose }` | `match` (strict, first-match, throws on no match) and `switch` (loose, falls through). Each `MatchArmT` carries `lands`, and the `default` body `default_lands`: an `ArmLanding` (the case body's `writes`, `reads`, `clears` and `runs`) when a jump in it lands on the successor (ADR-0103), which adds a fall-through edge from the arm's entry with those sets forgotten |
| `Assert { cond }` | `assert($expr)` with a lowerable condition |
| `Throw { span }` / `Exit { span }` | trace terminators |
| `Opaque { writes, reads, poisons, may_return }` | a recognized control-flow construct whose internals are not modeled but whose write and read sets are; `may_return` is true when the subtree contains a `return` the walk cannot see as a top-level `Return` |
| `Barrier` | anything unmodeled *and* unbounded — `declare`, `__halt_compiler`, declarations. Erases all known values |
| `Label { span }` | a `goto` label (ADR-0103): a barrier, and a point where a walk a terminator stopped resumes. A `goto` lowers as `LoopJump`, ending its list; `Stmt::holds_label` marks a label, or an `if`/`try` holding one, as the next live statement |
| `Try { body, catches, finally, catches_live, has_goto, body_writes, body_reads, catch_writes, catch_reads, writes, reads, poisons, may_return }` | `try`/`catch`/`finally` (ADR-0027's 2026-10-11 amendment): the block walks straight-line; each `catch` (a `CatchArm`: its `CatchClause` and body) enters with the block's sets forgotten and the caught variable seeded, and `finally` with the block's and the catches' sets forgotten; the successor is the join of the envs at the normal exits (the block's end and each live `catch`'s, where they fall through), carried through `finally` when there is one; with `has_goto` it applies `writes`/`reads` as `Opaque` does. `catches_live` is `false` when no statement of the block can throw, `has_goto` keeps the successor live. The construct terminates when `finally` does or when the block and every live `catch` do |

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
non-empty case but the last ends without running into the next (ADR-0103: the
last may run off its end, and trailing empty labels are empty arms). A jump in a
case is credited by its level and never refuses the construct, except in a
`switch (true)` chain, whose `if` has no landing edge. One unrepresentable arm makes
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

## `returns`: the compile-time return surface

`Scope::returns` lists every `return` of the scope's own body in source order as a
`ReturnSite { form, span }`, where `form` is `Bare` (`return;`), `Valued`
(`return <expr>;`, `return null;` included) or `ArrowBody` (an arrow function's
body expression, which the lowering turns into the trace's one `Return`). It is
read off the syntax tree, not the trace: the trace lowers a bare `return;` to
`Return { value: Other }`, indistinguishable from a returned expression the
lowering cannot spell, and drops what an `Opaque` construct swallows, while PHP's
compile-time return rules (`return.compile-fatal`, ADR-0078's 2026-10-11
amendment) hold in dead code and inside any construct. Nested function-like
bodies are their own scopes and are not descended. The list is empty when the
scope writes no return hint, the only reader's premise.

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

A call origin also carries one `ArgShape` per positional argument (issue #856):
what the scan can show the argument holds, so the effects pass can rule out the
user code a builtin reaches through it. The shapes are structural. An
expression qualifies by its form (a literal, a concatenation, a comparison, an
array literal of such). A bare variable qualifies through a flow-insensitive
summary of every write the frame makes to it, met with its declared type when
it is a parameter, and only in a frame that makes all its bindings (no
`global`, `static`, `$$v`, `extract`, `include`, reference or by-ref capture). A
variable handed bare to a named call, a `$this->`/`self::`/`parent::`/`Foo::`
method or a `new` is left for the effects pass to check against the callee's
by-reference flags; handed to anything else, it counts as written. `$this->p`
reads the property's declared type.

A call result is a shape too (issue #877): `ArgShape::Call` names a plain call's
function and `ArgShape::MethodCall` a method or static call's receiver and
method, the receiver spelled as a method-call site's is. The syntax crate cannot
consult the catalog or the project, so the shape carries the callee only and the
engine reads what its declared return holds: a builtin's mined row, a project
function's or method's native hint, an engine method's mined row. Both variants
are appended after `Unknown`, so no persisted variant index moves.

## Not implemented

- **Loop bodies as traces.** Loops are `Opaque`.
- **Catch matching in the walk.** The catch *guards* are carried on throw
  origins separately, and the walk does not use them to decide which `catch` is
  live.
- **Expression-position `match`.**
- **Array element tracking.** `ArgValue` carries array literals; there is no
  per-element fact lane.
