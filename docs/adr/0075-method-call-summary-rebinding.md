# A method call's summary rebinds where a function's does

Issue #126. Status: implementing (2026-08-02) — acceptance fixtures and
fp-gate green are the gate for "implemented". Siblings: #127 (fold gate
through project calls), #128 (closure return lane).

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

A resolved method or static call's `ReturnSummary` is consumed at the same
assignment rung free functions use: the `apply_assign` ladder. When the
summary binds (`Singleton` / `OneOf` / `Refined`), the assigned variable gets
that value fact; when it degrades to a bare `General` (or is absent), the
declared return arms of the resolved method seed the contract floor — the same
rung free functions already take via `call_return_arms`. The dump surface then
reads the env/contract of that variable through the existing
`best_dump_type` path; it does **not** resolve method calls in value position
directly (see §3). `summary_binds` is unchanged. No new binding machinery, no
new budget: `MAX_BINDING_DEPTH`, the on-stack `BindingKey` guard and the memo
replay discipline apply verbatim because they are the same code path.

Return composition (`return $o->m(...)` as an outer exit) uses the same
`stmt_summary` capture, so a method call returned from a function crosses
exactly as a nested free-function call does.

### 2.1 The binding key grows a receiver component

`resolve_exact` resolves through the inheritance chain, so the summary's key
name (`DeclaringClass::method`) can be reached from two *different* exact
receivers — `Sub1` and `Sub2` both inheriting `Base::m`. The walked body is
the same; the dispatch of `$this->hook()` *inside* it under `this_exact` is
not. A summary memoized under the bare declaring key would let the first
receiver's walk answer a memo hit for the second: the second body is not
re-walked, so its summary (and any emissions unique to that receiver) are
lost.

The key therefore gains a `this:` pseudo-binding carrying `body_this_exact`
when it is `Some` — the exact **class FQN**, not an allocation id. Same-class
allocations share a key (their entry state for `$this` dispatch is the same
class). *Amended 2026-08-16 (ADR-0086 §3): once `$this` is seeded from the
receiver's copy, the class FQN is no longer all of its entry state, so the
component carries the object's canonical rendering — class, exactness,
readonly bookkeeping, crossing props with their strata, carries — wherever a
copy was seeded, and the bare class FQN wherever one was not. Same-class
allocations holding different props stop sharing a key, which is the whole
point of the amendment.* The spelling has precedent: closure capture
snapshots already enter the key as `use:{name}` pseudo-bindings, and the
`this:` component sorts among them under the existing normalization. Guarded
resolutions (`this_exact = None`) key exactly as today — a final/private
body's inner dispatch is a pure function of its declaring class, so the bare
key is already sound there.

This component is a correctness sharpening that lands *with* the rebinding,
not after it: today's discard hides the collision on the value surface, but
the memo also suppresses re-walk (and thus re-emission) on hit.

### 2.2 What may reach `Singleton`

Facts derived from bound parameters and literals, exactly as for functions.
`$this`-property reads inside the callee seed from the canonical entry state
(ADR-0048; contract-fact lane, ADR-0052 §9) — and **what that entry state
holds depends on the receiver** (amended 2026-08-16, ADR-0086 §3): where the
receiver is an exact `Receiver::Var` with a bound heap object, `$this` is
seeded from a copy of that object, so a `$this`-property read contributes the
**receiver's own proven props** and can reach `Singleton`; at every other
receiver — a `$this`-origin one, a non-exact one, `Receiver::New`, a static
call — nothing is seeded and the read contributes the declared floor, never a
pin, exactly as this section originally stated for all of them. The rule
underneath is unchanged: a summary can only be as sharp as what the binding
proved, and the receiver leg is a binding the descent did not use to have.
Stratum flows by the existing min rule (ADR-0052 §5): an `Asserted` argument
yields an `Asserted` summary, never laundered to `Verified`.

### 2.3 Receivers that stay silent

Unchanged from `resolve_call_target`'s refusals: `Receiver::Prop` (ADR-0052
§7), `Dynamic`, `DynamicVar`, `static::` (late static binding), an
overridable method under the guard, and any poisoned scope on either side.
Silence widens to the arm floor; it never lies.

### 2.4 Return coverage is part of summary soundness

Enabling method rebinding surfaces pre-existing holes in the **shared**
return-summary collector:

* An `Opaque` construct (`foreach`, `try`, …) that contains a `return`
  contributes no exit, so a visible sibling `return null` could join alone as
  `Singleton(null)` and rebind a false premise (`call.on-null` on Composer
  `findPackage`). The Opaque variant therefore carries `may_return`; when set,
  a summary walk contributes the declared **Floor** (A3).
* Untyped fallthrough contributes `Singleton(null)` (PHP's implicit return).
  The test is the **raw written return hint** on the scope (`ret_hint`), not
  whether Steins lowers a representable `NativeType`. A written hint that
  falls through does not get a fallthrough-null contribution here.
* A written return hint Steins cannot lower (`: object`, `: array`, `: void`,
  `: never`, …) leaves `scope_return` as `None`, so the A2 native oracle has
  no arms and cannot drop boundary TypeErrors (`return null` under
  `: object`). **The whole value summary is refused** rather than rebinding an
  uncheckable exit. (Note on `: void`: PHP *does* yield `NULL` for
  `$x = f()` when `f(): void {}`, but v1 deliberately does not put that in a
  value summary — the same refuse path as other unrepresentable hints.)
* **`: mixed` is exempt from that refusal** (issue #364). It lowers to `None`
  like the others, but the refusal's premise is that the empty oracle might be
  hiding a drop; `mixed` is the TOTAL envelope, so there is nothing to hide —
  no value violates it, no conversion happens at the boundary, and the exit
  that crosses is the exit the body proved. It therefore reads as **no hint**
  in the summary and nowhere else: the declared value floor stays absent (a
  total envelope has no single base to floor to, so a factless exit still
  floors the whole summary out, A3), a `@return` docblock refines the proof
  instead of replacing it, and a `: mixed` body that falls off its end remains
  the runtime `TypeError` the return-missing pair reports. `RetHintKind::Mixed`
  carries the distinction so no consumer has to re-read the source text.
* Generators (`yield` / `yield from` in the body, `is_generator` on the scope)
  refuse the whole value summary (ADR-0057 §5): the call result is a
  `Generator`, not the value of a trailing `return`.

These rules apply to free functions and methods alike.

### 2.5 Declared return arms are captured at resolution

Method declared-return arms are computed when `resolve_call_target` succeeds,
**before** `apply_assign` may unbind the assignment target. Self-assign
`$o = $o->m(1)` therefore keeps the floor even though the receiver binding is
gone by the time the floor is seeded.

## 3. v1 exclusions — kept deliberately

* **Value/argument-position method calls.** `ArgValue::Call` carries a simple
  function name only; a method call in argument position never reaches the
  value IR. Extending the carrier is a syntax-layer design round of its own,
  and #127's fold-gate lane sees no methods until it happens. Observability
  for methods is via `$x = $o->m(...); dumpType($x)` (assignment rebind +
  existing dump of the variable).
* **Constructors.** `new Foo(...)` is not a value-returning call on this
  surface — its descend keeps running for diagnostics, its summary stays
  unread. The construction rvalue's exactness lane (ADR-0036) is untouched.
  *Superseded 2026-08-16 (#385): "for diagnostics only" is over. The
  constructor descent is now seeded with the fresh allocation as its `$this`
  and read for its **heap** component — the snapshot its exits agree on becomes
  the object `new` yields (ADR-0057's constructor-summary amendment). The
  **value** component stays unread, and for the reason this bullet gave: a
  constructor evaluates to an object, and an object is not a value (ADR-0035).
  The exactness lane is still untouched — the snapshot copies `class` and
  `class_exact` from the seed and cannot alter either.*
* **Heap transfer.** `ReturnSummary.heap` stays `None` — that is T1 proper
  (ADR-0057), and rebinding scalar/string/array facts neither needs it nor
  advances it. *No longer an exclusion as of 2026-08-16 (#378): T1 landed, and
  because a method's summary reaches the SAME `apply_assign` rung a function's
  does, the heap component rebinds for methods and statics with no method-side
  work at all — which is this ADR's whole point, restated by the slice that
  needed nothing from it.*
* **By-ref parameters refuse, variadics stop the binding prefix** — inherited
  from `descend` unchanged, restated here only so the slice review has the
  full refusal list in one place.
* **Closures** (`$fn(...)`) — still deferred (#128).

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
  types as its literal (via assignment), and function-family fixtures gain
  method rows where the same proof is reachable.
* A rebound fact is a **new premise**. The fp-gate is the instrument;
  movement in either direction is a triage event. Conformance rows involving
  method returns may flip and each flip is read, not assumed.
* Memo entries multiply by **exact receiver class FQN** — bounded by the
  distinct exact classes seen as receivers for a given declaring body, not by
  allocation count.
* The `this:` key component tightens diagnostic separation for inherited-body
  descents under a shared memo; fixtures must force both calls into one outer
  descent to exercise the collision.
