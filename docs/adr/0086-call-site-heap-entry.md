# Call-site heap entry: an argument's object crosses the binding descent by copy

**Status:** proposed (2026-08-15), pending ratification. Designed autonomously
under the owner's standing delegation, recorded ahead of the implementation.
Extends ADR-0001's binding descent and ADR-0036's heap model; the inbound
counterpart of ADR-0057 (which stays the outbound design: a *returned*
allocation crossing back to the caller).

## 1. The gap, measured

The binding descent (ADR-0001, ADR-0027 Feature B) hands a callee the
caller's argument *values*: `bound_env` is built from `singleton_fact` over
each argument, and the callee is walked with `Store::default()` — an empty
heap. Three consequences follow, each measured on the release binary at
master `666e73d`:

1. **An object argument carries nothing in.** `function h(Box $b) {
   takesString($b->value); }` with `h(new Box(1))` reports nothing inside
   `h`, although the caller's heap holds `$b->value = 1` and the callee's
   arg-check lane (`Store::prop_fact`, `Fact::Singleton` only) is ready to
   fire the moment the store holds the prop.
2. **A property read never becomes a return fact.** `function g(Box $b) {
   return $b->value; }` dumps `unknown` at `g(new Box(1))`. Not for want of a
   reader — `return_value_fact` already has a `PropFetch` arm and
   `join_summary` needs no change — but because the store it reads is empty.
3. **An object-only argument list does not descend at all.** `ArgValue::New`
   and an object-holding `ArgValue::Var` (objects live in `Store::refs`, not
   `env`) both fail `resolve_literal_under`, so `bound` stays empty and
   `descend` returns before walking. Consequence 1 is therefore *silence*, not
   imprecision: the callee body is only ever walked by the plain per-scope
   pass, whose parameter has no heap object.

The value domain is object-free by decision (ADR-0035/0038): `Known`, `Fact`
and `Val` have no object carrier, and this ADR does not add one. The object
already has a home — `HeapObj`, keyed by `AllocId` in `Store::heap` — and
`analyze_scope` is already written to accept a pre-populated store (its
`$this` seed defers to a bound `this`, with the comment "impossible today —
descents pass an empty store"). The gap is exactly one boundary.

The generics work (issues #360–#363) made this the load-bearing gap: the
conformance case `phpdoc_advanced_phpstan_template_type` line 47,
`takesString(unwrap(new Box(1)))`, cannot be enforced by any declared read
(an Asserted return arm premises no argument mismatch, by trust order) and
*can* be enforced by the body-proven route the moment `$box->value` is `1`
inside `unwrap`. Divergence-registry conformance entry 5 records the case as
"waits on the heap component"; this ADR is that component's inbound half.

## 2. Decision: the object crosses by copy, into the callee's store

**Mechanism.** At the descent boundary, for every positional argument that
resolves to a caller heap object — a direct `new` in argument position, or a
variable bound in `Store::refs` — the descent seeds the callee's `Store` with a
**copy** of that `HeapObj` under a **fresh callee-local `AllocId`**, bound to
the parameter's name. `descend` gains the caller's `&Store` (it has never had
one) and builds the seed store instead of `Store::default()`; the four call
sites thread it. `bound_env` and `Known` are untouched.

**Copy, not identity — the ADR-0057 §1 argument, in the other direction.**
The callee's copy is a fresh allocation: no caller-side name survives into the
callee, no callee-side write is observable from the caller through this
channel. Threading the caller's `AllocId` would couple two walks' heaps
mid-walk (ADR-0048 §2 bans it by name) and would make ids depend on descent
order (§4). Copy semantics is the only shape the position-query architecture
admits — inbound exactly as outbound.

**What crosses (per field, with the reason).**

| field | crosses? | why |
| --- | --- | --- |
| `class` | yes, verbatim | a by-value call cannot change what class an object is (ADR-0070 amendment) |
| `class_exact` | yes, verbatim, **never promoted** | ADR-0057 §6.4 (A1); audit G1; the FP direction `this_exactness.rs` pins |
| `readonly`, `ro_written` | yes, verbatim | ADR-0036's language guarantee does not stop at a call boundary; `ro_written` keeps `readonly.reassigned`'s bookkeeping honest even though a descent emits no such check |
| `targs` | yes, verbatim (`CArg::Val` and `CArg::Ty`) | the callee's own receiver-call sweeps apply inside; the copy is what makes a method-level `@param Box<T>` read (#363) and a `$this`-side carry read (#362) see the same state the caller saw |
| `escaped` | **always `true`** | the caller's object is marked escaped by this very call (step 1a runs after the descent); a copy claiming `false` would let an inner unknown call skip the sweep it owes |
| `props` (non-readonly) | **only when the caller's object has `escaped == false`** | see the aliasing rule below |
| `props` (readonly) | yes | immune by language guarantee, whatever the escape state |
| the `AllocId` | never | walk-local, counter-derived; ADR-0057 §1/§4 |

**The aliasing rule, which decides which props cross.** PHP passes objects
by handle. Inside the callee, the copy under the parameter's name is written
by the callee's own `$b->x = …` (the descent already records prop writes into
its store — only the *checks* are descent-gated), so the callee's reads of its
own writes are right by construction. What could go wrong is a write through
**another name for the same object**. Two such names exist:

- **The same caller object passed twice**, or passed and also the receiver
  (`f($b, $b)`, `$b->m($b)`). Two independent copies would let
  `function f(Box $x, Box $y) { $x->value = 's'; takesString($y->value); }`
  convict correct code. So the seed is **one callee allocation per distinct
  caller `AllocId`**, and every parameter (and `$this`, §3) bound to the same
  caller object binds to that one copy. Aliasing structure *among the
  arguments* is preserved; aliasing with anything else is excluded by the next
  rule.
- **An alias the caller cannot see** — the object also sits in a static
  property, an array, a global, another object's property. Every such route
  marks the caller's object `escaped` at the moment it is taken (ADR-0036).
  An object with `escaped == false` is held by the caller's variables alone,
  so no name outside the argument list can reach it inside the callee, and its
  non-readonly props may cross. An object with `escaped == true` may have
  regained props since (`$b->value = 2` after the escape) and *may* be reached
  through the alias, so its non-readonly props do **not** cross — class,
  exactness, readonly props and carries do.

**The caller-side sweep is unchanged, and this is a refusal, not an
omission.** After the call, the caller's object is marked escaped and its
non-readonly props and value carries are swept as today (ADR-0036 escape
discipline, #295's argument-pass gate for carries). The obvious "optimization"
— the descent walked the body and saw no write to `$b->value`, so keep the
caller's fact — is precisely the judgment `callee_cannot_reach_arg`'s own
documentation refuses: a per-parameter *non-mutation* proof needs ADR-0055
Part II's mutation inference (property writes colour nothing today; a body
that only writes properties has an empty proven finding set). Until that
inference exists, the copy flows in and the sweep flows out, independently.
The lexical gate stays the floor for value carries.

**Descent trigger.** A seeded object counts as a binding: the "nothing bound,
nothing to walk" test admits a call whose only bindable arguments are objects.
Consequence 3 above disappears; the memo and the emission-dedupe (ADR-0075
§2.1) then govern the walk exactly as for value bindings.

**The binding key.** The memo key must distinguish `h(new Box(1))` from
`h(new Box(2))` or it replays the wrong summary and suppresses the wrong
re-emission. An object contributes a **canonical rendering** to the key:
class, exactness, readonly set, the sorted `(prop, fact-key, stratum)` list of
the props that cross, and the carries. `arg_of_fact_key` is the existing
precedent (captures). A prop whose fact is not key-representable **does not
cross** — the seed contains only what the key states, so the key is a faithful
name for the entry state and the memo stays a pure function of it (ADR-0048
§2). This is strictly less knowledge, never wrong knowledge. No `AllocId`
enters the key.

**Strata cross with their facts.** A prop fact carries its stratum into the
copy (an Asserted prop stays Asserted; the arg-check lane's `Verified` gate
still applies inside the callee) — ADR-0052 amendment 1, no laundering.

## 3. The receiver leg is the same decision

`$this` inside a method descent is seeded fresh (`seed_this_object`: class,
exactness, pre-escaped, readonly bookkeeping — **no props, no carries**), and
ADR-0075 §2.2 recorded that as intended: `$this`-property reads contribute
declared floors, never pins. With the argument leg landed, `unwrap($box)` and
`$box->unwrap()` would diverge for no reason a user can see, and #362 already
reads the receiver's carries at the call site while the descent's `$this`
drops them. So the receiver is the *zeroth argument*: an exact `Receiver::Var`
whose caller object is bound seeds the callee's `$this` from a copy of that
object under the same field table and the same aliasing rule (a receiver
passed again as an argument shares its copy). A `$this`-origin receiver
(`this_exact` from an enclosing method) is pre-escaped by construction, so its
non-readonly props never cross — nothing changes for it. ADR-0075 §2.2's
sentence becomes provisional-until-this-slice rather than a statement of
intent, and is amended when the receiver slice lands.

## 4. What stays out (each one line, each anchored)

- **Object-valued properties as return facts** — `return $b->inner` where
  `inner` holds an object: the value domain has no object carrier and
  `resolve_cval` has no `PropFetch` arm; the outbound heap channel (ADR-0057
  T1) is where an object crosses back, and a nested summary is depth. Out of
  v1, as ADR-0057 §5 keeps objects-in-arrays out.
- **The outbound leg itself** — `return new Foo()`, `return $b`: ADR-0057 T1,
  designed, unchanged, sequenced after this ADR's slices (§7).
- **A parameter seeded from a declared `@param Box<int> $b` at a non-descent
  entry** — the plain per-scope pass still gives a parameter no `HeapObj`;
  ADR-0032 §3's declared-seed clause keeps waiting for its own slice. This ADR
  defines the *descent* entry only.
- **By-ref parameters, variadics, named/spread argument lists** — the descent
  already declines them; the object seed inherits the decline.
- **Constructor bodies** — `build_new_object` never walks `__construct`, so a
  class whose constructor writes `$this->value = $v` (rather than promoting
  it) still yields no props. A real, separate gap, named here so it is not
  mistaken for this one.
- **Weakening the caller-side sweep on descent evidence** — refused above;
  precondition ADR-0055 Part II.

## 5. Soundness legs, each a fixture

1. **Aliasing among arguments**: `f($b, $b)` with a write through the first
   and a read through the second reports nothing; `$b->m($b)` likewise.
2. **Escaped objects keep their props to themselves**: an object stored into
   a static property, then written, then passed — the callee sees no
   non-readonly prop; a readonly prop still crosses.
3. **Exactness is copied, not promoted**: a non-exact caller object (a
   laundered `$this` alias) seeds a non-exact copy; `this_exactness.rs` stays
   green.
4. **The caller-side sweep is untouched**: `mutate($b)` (a callee that writes
   `$b->value = 's'`) followed by `takesString($b->value)` in the caller
   reports nothing, before and after; `dumpType($b->value)` after any call is
   `unknown` as today.
5. **The key distinguishes entry states**: `h(new Box(1)); h(new Box('s'))`
   — the first fires inside `h`, the second does not, and both are emitted
   once (dedupe by key, ADR-0075 §2.1).
6. **Strata cross**: an Asserted prop (seeded from a declared default) stays
   Asserted inside the callee and premises no proof-layer finding.
7. **Hooked properties never cross** — inherited by construction (they never
   enter the caller's heap, FP class 16); pinned anyway.
8. **The three probes go green**: `takesString($b->value)` inside `h` fires at
   `h(new Box(1))`; `dumpType(g(new Box(1)))` is `1`; and the conformance
   shape `takesString(unwrap(new Box(1)))` reports `type.argument-mismatch`
   — divergence-registry conformance entry 5 is retired in the same slice,
   and the three `generics_carry.rs` pins that named the heap component as
   the blocker flip.
9. **Budget**: an object-only call now descends; the depth/recursion
   discipline (ADR-0057 A5) and the memo bound the cost; a fixture with a
   recursive object-passing pair degrades to the floor rather than looping.

## 6. ADR-0048 obligations

**§2 (replayable).** The seed is a pure function of the caller's heap state at
the call (which the caller's walk is a deterministic function of), the callee's
CST, and the index. The memo key names the seed exactly (§2's key rule), so
re-walking the callee under the same key reproduces the same store and the
same summary. Nothing crosses that the key does not state.

**§3 (entry-state contribution) — the load-bearing one.** This ADR introduces
a new contributor to a scope's entry state: **a parameter bound by a binding
descent contributes a copy of the argument's heap object**, per the field
table in §2, and `$this` in a method descent contributes a copy of the exact
receiver's object (§3). At any *other* entry — the plain per-scope pass, a
non-exact or `$this`-origin receiver, a parameter whose argument resolved to
no object — the contribution is what it is today: nothing on the heap. The
declared-`@param` seed remains an open clause with its own future slice.

**§4 (no global ordering).** The seed depends on the caller's statement order
(the caller's heap at the call) and on nothing across scopes or files. Fresh
callee `AllocId`s are walk-local, exactly as every other allocation the walk
mints.

## 7. Slices

- **H1 — the argument leg**: `descend` takes the caller store, seeds copies
  per §2 (field table, aliasing rule, escaped rule, key rendering, trigger);
  legs 1–9 above; registry entry 5 retired; the `generics_carry.rs` pins
  flipped; ADR-0032/registry/`generics_carry.rs` references to "ADR-0057 T1's
  heap component" re-pointed to this ADR. Finding-adding on the proof layer:
  fp-gate movement triaged per site.
- **H2 — the receiver leg**: `$this` seeded from the exact receiver's copy
  (§3), sharing with argument copies; ADR-0075 §2.2 amended; the
  `$box->unwrap()` / `unwrap($box)` parity pin.
- **H3 — ADR-0057 T1 outbound**: unchanged in content, sequenced after H1/H2,
  with `targs` added to the snapshot's field list by amendment (it postdates
  ADR-0057) and `HeapSummary` given its shape then.

## 8. Refusals (each one line, each anchored)

- **Allocation-id threading into the callee** — ADR-0048 §2/§4, ADR-0057 §1.
- **An object carrier in `Known`/`Fact`/`Val`** — ADR-0035/0038; the heap is
  the object's home.
- **Two copies for one caller object** — the aliasing leg (§2); one copy per
  caller `AllocId`.
- **Crossing non-readonly props of an escaped object** — the alias route
  (§2).
- **Keying on class and exactness alone** — memo collisions replay the wrong
  summary and suppress the wrong emission (ADR-0075 §2.1).
- **Skipping the caller-side sweep on descent evidence** — ADR-0055 Part II is
  the precondition; until then the copy and the sweep are independent.
