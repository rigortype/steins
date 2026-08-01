# A method call's summary rebinds where a function's does

Issue #126. Implemented 2026-08-02 (branch `feature/126-method-call-summary-rebinding`).
Siblings: #127 (fold gate through project calls), #128 (closure return lane).

## 1. Context: the walk already pays for what the call site never reads

T0 (ADR-0057) landed call-site value reflection for functions: `greet(3,
"World")` binds to its literal through the binding-descent summary, and the
flagship fixtures pin it in both value and argument position (#59/#60).

The method leg is not missing — it is discarded. The method/static/constructor
check resolves its target through `resolve_call_target` (exact
allocation-proven receivers; the final/private override guard for
`$this`/`self`; named static classes) and then runs `descend` over the callee
body for the in-body diagnostics. The summary comes back joined, memoized and
correct, and the call site drops it on the floor:

> `let _ = descend(...)` — "T0 consumes summaries only at
> direct-function-call assignment sites; a method call's summary is computed
> (memo/machinery shared) but not rebound here (T1)."

So `$g = new Greeter(); $g->greet(3, 'World')` walks the body, proves the
value, fires callee-side diagnostics *from* that value — and then types the
call as the declared envelope. This ADR is about consuming what is already
computed, not about a new walk. (The reference model re-types method bodies
per call site and degrades overridable targets; `resolve_call_target` is
Steins' standing equivalent of that degrade, already trusted by the effect,
throw and heap layers.)

## 2. Decision

A resolved method or static call's `ReturnSummary` is consumed at exactly the
rungs where the function summary is consumed today: the `apply_assign` ladder
and the dump surface (`best_dump_type`), which remain observationally
identical by construction. `summary_binds` is unchanged — `Singleton`,
`OneOf` and `Refined` bind; a bare `General` stays the arm floor it already
is. No new binding machinery, no new budget: `MAX_BINDING_DEPTH`, the
on-stack `BindingKey` guard and the memo replay discipline apply verbatim
because they are the same code path.

### 2.1 The binding key grows a receiver component

`resolve_exact` resolves through the inheritance chain, so the summary's key
name (`DeclaringClass::method`) can be reached from two *different* exact
receivers — `Sub1` and `Sub2` both inheriting `Base::m`. The walked body is
the same; the dispatch of `$this->hook()` *inside* it under `this_exact` is
not. A summary memoized under the bare declaring key would replay one
receiver's value for the other.

The key therefore gains a `this:` pseudo-binding carrying `body_this_exact`
when it is `Some`. The spelling has precedent: closure capture snapshots
already enter the key as `use:{name}` pseudo-bindings, and the `this:`
component sorts among them under the existing normalization. Guarded
resolutions (`this_exact = None`) key exactly as today — a final/private
body's inner dispatch is a pure function of its declaring class, so the bare
key is already sound there.

This component is a correctness sharpening that lands *with* the rebinding,
not after it: today's discard hides the collision on the value surface, but
the memo also replays emissions, so the audit of existing behavior is part of
the slice.

### 2.2 What may reach `Singleton`

Facts derived from bound parameters and literals, exactly as for functions.
`$this`-property reads inside the callee seed from the canonical entry state
(ADR-0048; contract-fact lane, ADR-0052 §9), so they contribute declared
floors, never pins — a summary can only be as sharp as what the binding
proved. Stratum flows by the existing min rule (ADR-0052 §5): an `Asserted`
argument yields an `Asserted` summary, never laundered to `Verified`.

### 2.3 Receivers that stay silent

Unchanged from `resolve_call_target`'s refusals: `Receiver::Prop` (ADR-0052
§7), `Dynamic`, `DynamicVar`, `static::` (late static binding), an
overridable method under the guard, and any poisoned scope on either side.
Silence widens to the arm floor; it never lies.

## 3. v1 exclusions — kept deliberately

* **Value/argument-position method calls.** `ArgValue::Call` carries a simple
  function name only; a method call in argument position never reaches the
  value IR. Extending the carrier is a syntax-layer design round of its own,
  and #127's fold-gate lane sees no methods until it happens.
* **Constructors.** `new Foo(...)` is not a value-returning call on this
  surface — its descend keeps running for diagnostics, its summary stays
  unread. The construction rvalue's exactness lane (ADR-0036) is untouched.
* **Heap transfer.** `ReturnSummary.heap` stays `None` — that is T1 proper
  (ADR-0057), and rebinding scalar/string/array facts neither needs it nor
  advances it.
* **By-ref parameters refuse, variadics stop the binding prefix** — inherited
  from `descend` unchanged, restated here only so the slice review has the
  full refusal list in one place.

## 4. Replayability (ADR-0048)

The rebind verdict is a pure function of the statement's `CallExpr`, the
project index, the walk-local env/store at that point, and the memoized
summary under the extended key. The engine enters only through folds the
callee body itself performs, which are already memo-disciplined in
`EngineFolder`; `--no-php`, a live sidecar and a browser replay decide
identically because the summary machinery is shared with functions, where
this is the standing property.

## 5. Consequences

* The flagship reflects across the receiver seam: `$g->greet(3, 'World')`
  types as its literal, and every fixture family that today works for a
  function twin gains a method row.
* A rebound fact is a **new premise**. The fp-gate is the instrument;
  movement in either direction is a triage event. Conformance rows involving
  method returns may flip and each flip is read, not assumed.
* Memo entries multiply by receiver exactness — bounded by allocation sites,
  which the store already tracks; no new cap is introduced.
* The `this:` key component retroactively tightens diagnostic replay for
  inherited-body descents, a behavior audit that must be run and recorded
  even where no fixture moves.
