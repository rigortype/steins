# Call-site heap entry: an argument's object crosses the binding descent by copy

**Status:** proposed (2026-08-15), pending ratification. Designed autonomously
under the owner's standing delegation, recorded ahead of the implementation.
Extends ADR-0001's binding descent and ADR-0036's heap model; the inbound
counterpart of ADR-0057 (which stays the outbound design: a *returned*
allocation crossing back to the caller). **§2 (the argument leg, slice H1)
landed 2026-08-15 (#376); §3 (the receiver leg, slice H2) landed 2026-08-16
(#377), amending ADR-0075 §2.1 and §2.2; §7's H3 — ADR-0057 T1 itself —
landed 2026-08-16 (#378), amending ADR-0057 with the heap component's
shape.** All three slices are in.

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

**Landed 2026-08-16 (#377), with three things the implementation had to
decide.** *(a)* **A seeded `$this` counts as a binding**, the same trigger
rule §2 gave a seeded parameter — otherwise `$b->get()` (no arguments at all)
would not descend and could not be the parity's own witness. *(b)* **The
`this:` key component carries the object's canonical rendering** wherever a
copy was seeded, the bare class FQN wherever one was not: `$b1 = new Box(1)`
and `$b2 = new Box('s')` reach one inherited body with different entry states,
and the class alone would replay one summary for the other and suppress the
other's emission (ADR-0075 §2.1). *(c)* **The parity is observable at the
assignment rung only.** `$box->unwrap()` in argument or dump position is not a
value at all — `ArgValue::Call` carries a simple *function* name and a method
call never reaches the value IR (ADR-0075 §3's v1 exclusion, the same carrier
limit issue #374 measured for `Receiver::New`). So `$v = $box->unwrap();`
agrees with `$v = unwrap($box);`, a sink *inside* the method fires on the
receiver's own props, and the direct `takesString($box->unwrap())` stays
silent for a reason one layer below this ADR. That layer is where the
remaining half of the parity lives; it is not a heap-entry gap and must not be
re-diagnosed as one.

*(c) closed 2026-08-16 (#386), one layer below, exactly as it said.* The value
IR grew `ArgValue::MethodCall` and a `project_method_summary` that enters
`descend` through the same `resolve_call_target` this leg does, so
`takesString($box->unwrap())` and `dumpType($box->get())` now see the copy this
section seeds — the same summary, the same memo entry, the same one walk. The
receiver leg itself is unchanged by that slice: it still fills `receiver_var`
for an exact `Receiver::Var` and for nothing else, and the value position
simply became a second caller of it. Two of §3's stated non-seeders did move,
and neither is this leg's rule changing: **`Receiver::New`** now seeds `$this`
from the object the receiver's own `new` mints (its arguments having reached
the IR at last — ADR-0057 C7's deferral, discharged), and a nested-argument or
`best_dump_phpdoc_type` entry that holds no enclosing class declines
`$this`/`self`/`parent` receivers outright rather than seeding a weaker copy.

## 4. What stays out (each one line, each anchored)

- **Object-valued properties as return facts** — `return $b->inner` where
  `inner` holds an object: the value domain has no object carrier and
  `resolve_cval` has no `PropFetch` arm; the outbound heap channel (ADR-0057
  T1) is where an object crosses back, and a nested summary is depth. Out of
  v1, as ADR-0057 §5 keeps objects-in-arrays out. *Still out after T1 landed
  (#378): the heap holds a `Fact` per property, never an `ObjRef`, so there
  is no object at `$b->inner` for the outbound channel to snapshot. The gap
  is the object-in-carrier one, not the crossing.*
- **The outbound leg itself** — `return new Foo()`, `return $b`: ADR-0057 T1,
  designed, unchanged, sequenced after this ADR's slices (§7). **Landed
  2026-08-16 (#378).**
- **A parameter seeded from a declared `@param Box<int> $b` at a non-descent
  entry** — the plain per-scope pass still gives a parameter no `HeapObj`;
  ADR-0032 §3's declared-seed clause keeps waiting for its own slice. This ADR
  defines the *descent* entry only.

  *Closed 2026-08-16 (#388), by ADR-0032's declared-parameter-seed amendment: a
  parameter whose **native** hint is one non-nullable known class enters its
  scope as a lower-bound, pre-escaped, prop-free `HeapObj` carrying its `@param`'s
  type arguments as `CArg::Ty` — at the plain per-scope entry, and inside a
  descent wherever this ADR's copy did not land. The copy always wins where it
  landed: the seed's own gate is `!store.is_bound(param)`. The class comes from
  the native hint alone, `HeapObj::class` having no stratum to keep a docblock out
  of the proof-layer dispatch it premises; the `@param` contributes the arguments,
  which is what §3's clause names.*
- **By-ref parameters, variadics, named/spread argument lists** — the descent
  already declines them; the object seed inherits the decline.
- **Constructor bodies** — `build_new_object` never walks `__construct`, so a
  class whose constructor writes `$this->value = $v` (rather than promoting
  it) still yields no props. A real, separate gap, named here so it is not
  mistaken for this one.

  *Closed 2026-08-16 (#385), by ADR-0057's constructor-summary amendment: the
  descent that already walked `__construct` for its diagnostics now runs with
  `$this` seeded from the fresh allocation, and the caller's object becomes the
  snapshot that descent's exits agree on. **Walked constructors supersede the
  lexical rule below** — where the walk runs, its writes are the object's
  properties and every literal default it did not overwrite stands. The lexical
  rule stays as the **decline floor**, for the constructors no walk reaches
  (none declared, abstract, unresolvable, poisoned, named/spread argument list,
  budget or recursion, every path throwing), and its pins stay green as floor
  pins. The seed also flips one field of §2's table — `escaped` is `false`
  there, the one copy that is not pre-escaped, because a `new` site has no
  caller-side object to escape — and pays for it with the same-`$this` sweep
  ADR-0057 C5 adds: `$this->m(…)`, `parent::m(…)`, `self::m(…)` and
  `static::m(…)` sweep the receiver's own non-readonly props in every walk,
  resolved or not, which also closes the resolved-private-`$this->m()` hole
  this ADR's own §3 seeding had left open.*

  *Amended 2026-08-16 (#377): that gap had a second, unsound half, and the half
  is closed.* Not walking the constructor also meant the **literal property
  defaults were seeded as if nothing ran between the allocation and the first
  read**. `private $view = 0;` overwritten by `$this->view = $original_view -
  $this->ad_count;` stood on the caller's object as a proven `0` — a wrong
  `Verified` fact, not merely a missing one, and one the receiver leg then
  carried into `getView()`'s summary where a declared `positive-int` parameter
  convicted correct code. A default now survives only when the constructor that
  runs for the class never **mentions** `$this->{prop}`, by a whole-token
  lexical scan of that constructor's body text — the decidable, over-
  approximating question the ADR-0032 argument-pass gate already asks about
  parameters (`callee_cannot_reach_arg`), and for the same reason: the linear
  trace drops nested sub-expressions, so only the source text can answer it.
  Any mention drops the seed; no constructor keeps every default; an unreadable
  body or a poisoned constructor keeps none. Promoted parameters are untouched
  (their fact is the argument, proven at the call site), as is `readonly`
  bookkeeping. The *other* half stands: a constructor's writes still yield no
  props, so such a slot is simply unknown.

  **The per-property rule only speaks about slots the text spells, so a
  constructor that lets `$this` out of its own text drops every default.** A
  delegating `__construct() { $this->init(); }` whose `init()` writes
  `$this->view` spells no `$this->view`, and `view`'s default would have
  survived and been wrong by the identical argument. Four shapes therefore set
  the coarse answer, each a route by which a slot is written without this text
  naming it: a **bare `$this`** (not followed by `->` — passed to a function,
  assigned to a variable, returned, captured by a closure: an alias leaves, and
  any holder can write any property); **`$this->m(…)`**, a call into a body
  this scan is not reading; **`parent::m(…)` / `self::m(…)` / `static::m(…)`**,
  which are that same call under a spelling that keeps the very same `$this`
  (`parent::__construct()` above all, while a bare `self::CONST` runs nothing
  and is not a call); and the **dynamic** access. Where none of them occurs the
  per-property rule stays fine-grained: a constructor whose only `$this` uses
  are `$this->a = 1; $this->b = $x;` keeps `$c`'s default.

  **This is deliberately coarse and its precision cost is unknown**, not small:
  a constructor calling one `$this` method loses the defaults of properties that
  method could not possibly touch, and neither the conformance suite, nsrt, nor
  the public fp-gate moved by a single line in either direction, so no instrument
  available here can price it. Correctness decides instead — a wrong `Verified`
  default is a proof-layer false positive, a dropped one is lost knowledge, and
  the two are not comparable. **Refining it has a named precondition**: a
  per-callee property-write summary (*which slots can this call write?*), which
  is the same ADR-0055 Part II mutation inference the caller-side sweep refusal
  in §2 has been waiting on. Until that inference exists, "the constructor is
  trivially inspectable" is the whole of what makes a literal default
  trustworthy.
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
no object — the contribution is what it is today: nothing on the heap. ~~The
declared-`@param` seed remains an open clause with its own future slice.~~
*Amended 2026-08-16 (#388): it is no longer open, and the two contributions do
not overlap. A parameter whose native hint is one known class contributes the
declared object of ADR-0032's declared-parameter-seed amendment at exactly the
entries this ADR leaves bare — the plain per-scope pass, and a descent whose
argument resolved to no object. Its gate is `!store.is_bound(param)`, so a copy
this ADR seeded is never overwritten, and the copy is the stronger of the two
wherever both could speak.*

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
  (§3), sharing with argument copies; ADR-0075 §2.1 and §2.2 amended; the
  `$box->unwrap()` / `unwrap($box)` parity pin at the assignment rung, plus a
  pin on the value-IR limit that keeps the direct form out of reach. **Landed
  2026-08-16 (#377).** *The limit pin flipped 2026-08-16 (#386) when that
  layer landed: the direct form is now in reach and the parity is observable
  at every position, per §3(c).*
- **H3 — ADR-0057 T1 outbound**: unchanged in content, sequenced after H1/H2,
  with `targs` added to the snapshot's field list by amendment (it postdates
  ADR-0057) and `HeapSummary` given its shape then. **Landed 2026-08-16
  (#378)**: the shape is a wrapped `HeapObj` whose `escaped` reads as
  escaped-before-return, the rebind is the `apply_assign` rung for functions
  and methods alike, and the direct value/argument forms stay silent for the
  reason §3(c) already gave — see ADR-0057's 2026-08-16 amendment.

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
