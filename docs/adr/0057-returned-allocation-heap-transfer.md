# Returned-allocation heap transfer: return-object summaries, rebound by copy

The measured gap (owner probes, 2026-07-24): a callee's heap allocation
dies at the function boundary. `function createFoo(int $n): Foo { return
new Foo(n: $n); } $foo = createFoo(123);` — inside the callee the walk
holds everything (exact class `Foo`, promoted prop `n = 123` from the
bound argument, `class_exact = true`); the caller's `$foo` gets none of
it. Before the in-flight arm-seeding slice: unknown. After it: a
Verified Instance-membership arm from the declared return type — still
no exactness, no prop facts, no value precision. `createFoo(123)->n` can
never be `123`, and array-of-DTO pipelines
(`array_map(createFoo(...), $rows)` → `array_column(..., 'n')`) widen to
unknown at the first boundary. The modernization mission's flagship
idiom — build-and-return DTOs, array-shape → DTO transforms — is exactly
this flow. This ADR designs what crosses back.

## 1. The mechanism: a return-object summary, rebound by copy

Three candidates, one survivor:

- **(a) Return-object summary — ADOPTED.** When the binding descent
  (ADR-0001/0027, Feature B) walks a callee and the callee returns a
  locally-held allocation, the walk records a **value-object snapshot**
  at each return site: `{ class, class_exact, props (each fact with its
  stratum), readonly set, escaped-before-return }`. The caller
  **rebinds** the joined snapshot as a **fresh allocation in its own
  heap** — a new `AllocId`, copy semantics, no shared identity. Aliasing
  questions die at the boundary: no caller-side write can be observed
  through a callee-side name, because no callee-side name survives.
- **(b) Allocation-id threading — REFUSED.** Sharing the callee's
  `AllocId` into the caller's `Store` makes the caller's heap contain an
  object another scope's walk created — mid-walk mutable coupling across
  scopes, which ADR-0048 §2 bans by name. It also makes allocation ids
  depend on which descents ran and in what order (fresh-id counters are
  walk-local), violating §4's no-global-ordering rule, and it imports
  the full aliasing problem (a callee that also stored the object into a
  static property would share mutable state with the caller's copy) for
  zero additional yield the escape bit does not already deliver.
- **(c) Nothing new (arms only) — REFUSED as the end state, KEPT as the
  floor.** The declared-return arm seeding stays: it is the sound floor
  every path below widens to whenever the summary is refused. The gap
  measured above is precisely what the floor cannot close.

**The ADR-0048 §2 lens, which decides it.** A summary is a pure function
of (the callee's CST, the entry state the descent bound, salsa query
answers, the fold memo) — exactly the inputs the callee's walk is
already a deterministic function of under the replayability constraint.
Re-running the callee's walk later, alone, reproduces the identical
summary: it is a **legitimate query answer**, the same epistemic object
as a fold result or a reflected envelope. Identity threading is not a
query answer at all — it is a live pointer into a walk that has ended.
Copy-rebind is therefore not a pragmatic compromise; it is the only
shape the position-query architecture admits.

**What rebinding means, precisely.** At the call site the caller
allocates a fresh id, builds a `HeapObj` from the summary verbatim —
`class` and `class_exact` copied (never promoted, §6), `props` copied
with each fact's stratum intact (the rebind is a derivation step whose
only inputs are the summary facts; min-stratum over a singleton input
set is the identity — ADR-0052 amendment 1), `readonly` copied
(sweep immunity persists in the caller; the language guarantee that
justified it in ADR-0036 does not stop at a `return`), established
readonly props recorded as written — and binds the call-result variable
to it. `escaped` on the fresh object: `false` when the summary says the
allocation had not escaped before the return itself (the return was its
only exit — every callee-side alias died at scope exit, so the caller
holds the sole reference); `true` (pre-escaped) when the callee let it
escape elsewhere first (§2). Hooked properties never appear in a
summary because they never appear in the callee's heap (FP class 16,
commit "Hooked properties bind no facts") — the exclusion is inherited
by construction, and a fixture pins it anyway.

## 2. When the summary is honest

The summary claims only what the callee's walk holds at the return
point. That makes most honesty questions answer themselves:

1. **Snapshot position: at the return site, after every preceding
   effect, before the return's own escape-marking.** The current walk
   marks a returned object escaped as part of lowering the return; the
   snapshot is taken one instant earlier, so `escaped-before-return`
   distinguishes "the return is the only exit" from "it already got
   out". Both are summarizable; they differ only in the rebound
   object's pre-escape bit.
2. **Mid-body escape is fine — the sweep already ran.** An object
   passed to an unresolved call before being returned has had its
   non-readonly props swept (ADR-0036 escape discipline); the summary
   carries the post-sweep state, which is sound by the same argument
   that made the sweep sound. The rebound object is born pre-escaped:
   any later unknown call in the *caller* sweeps it too, exactly as if
   the caller had leaked it itself.
3. **Origin does not matter; exactness bookkeeping does.** An object
   the callee received (a param, another call's result) and returned:
   the summary is a snapshot of the walk's knowledge, and the walk's
   knowledge about that value is sound regardless of where the value
   came from — so the summary is sound. But exactness crosses only per
   the A1 discipline (ADR-0049 amendment): a param-origin or
   `$this`-origin object carries `class_exact = false` (membership, a
   lower bound), and the summary copies the bit as-is. Chaining
   composes correctly: a callee that itself rebound an exact summary
   from a deeper factory holds `class_exact = true`, and passes it up.
4. **Multiple return paths join.** Per-prop: the existing `Fact` join,
   strata min (ADR-0052 amendment 1 — a Verified arm joined with an
   Asserted arm yields Asserted); a prop survives only if present on
   every object-returning path. `escaped-before-return` ORs. Class: if
   every path returns the same class, it survives; `class_exact` only
   if every path is exact **and** the classes agree. **Differing
   classes ⇒ no summary** (v1) — a joined "one of Foo or Bar" object is
   the `Member`-fact shape, not the heap's exactness shape, and the
   declared-return arms already carry the membership floor; inventing a
   weaker heap object buys nothing the floor lacks.
5. **Any non-allocation return path kills the summary.** A path
   returning `null`, a scalar, an unresolved expression, or falling off
   the end (implicit `return null`) means the call result is not always
   this object; the summary would be a partial lie. No summary — the
   arm floor (which for a `?Foo` declaration honestly carries the
   nullable) stands. Nullable factories are the arm lane's business.
6. **The summary never intersects the declared return type.** The
   declaration is a claim (ADR-0037); the summary is what the walk
   proved runs. A conflict between them is the callee's
   `type.return-mismatch` / `phpdoc.return-mismatch` finding, emitted
   where it belongs; the caller consumes the walk truth. When there is
   no summary, the declaration's arms are the floor — refinement within
   an envelope when both exist is automatic, because the walk itself
   ran under the callee's declared param seeds (ADR-0001's
   envelope-refinement posture end to end).

## 3. Budget, memo, and the descent it rides

**No second walk.** The summary is computed during the same descent the
checker already performs: `analyze_scope` gains a summary out-channel in
the established optional-out-param pattern (`facts`, `dead_out`), filled
at return sites when a descent requests it. Cost is a snapshot per
return site — noise against the walk itself.

**Depth exhaustion widens, never lies.** A call at
`next_depth > MAX_BINDING_DEPTH` (= 8) does not descend, so it produces
no summary; the caller keeps the arm floor. There is no partial
summary — the rule is the ADR-0027 ratchet's: silence over guess.

**The memo becomes a value memo.** Today the descent memo is a
suppression set (`HashSet<BindingKey>`: "this binding already checked,
do not re-emit"). A summary is a value, so a memo hit must *replay* it:
the memo entry for a binding key gains the computed
`Option<ReturnSummary>`. This is legitimate caching, not retention: the
key — callee identity plus the sorted `(param name, bound value)` list
plus capture snapshots — determines the entry state, and the summary is
a pure function of (CST, entry state), so the cached value is exactly
the query answer a re-walk would produce. Recursion (a key already on
the descent stack, the recursive-factory case): the walk is suppressed
and no summary exists yet — **no summary**, arm floor, terminating and
sound.

**Named arguments, reconciled with commit "Named arguments bind in the
contract lane".** The binding descent is positional-only today; named
and mixed calls never descend, so in v1 they get no summary (arm floor)
— consistent, not a new hole. The forward path is already key-safe: the
binding key is name-keyed and sorted, so when named-arg descent lands,
a named call and its positional twin canonicalize to the **same** key
and share one memoized summary — no key ambiguity to design around
later. (The owner probe's *inner* `new Foo(n: $n)` is unaffected:
named-arg constructor binding inside the callee walk already works.)

**Zero-argument factories descend for the summary only.** Today a call
with no bound args and no captures does not descend (nothing to bind,
and an unbound walk would duplicate the per-scope pass's findings).
`function make(): Foo { return new Foo(); }` is the minimal factory,
and it must not be the one shape that stays dark. Committed: a
zero-binding call may run a **summary-only walk** — emission
suppressed (its findings are byte-for-byte the per-scope pass's own,
already reported once; discarding is deduplication, not silencing) —
memoized under the empty binding key, so it runs once per callee per
run. The replayability argument is the same; the entry state is just
the native-type seeds.

**Which calls qualify: whatever the descent already reaches.** Plain
functions, static methods, and instance methods on exactness-bearing
receivers — the summary rides the existing resolution and dispatch
guards and adds no new resolution machinery. A membership-only receiver
does not descend (ADR-0052 §3's NOT-fed list), so it gets no summary;
the final-Member unlock noted there would extend this automatically.

## 4. Constructors: `new` is the depth-0 summary

`build_new_object` already does at the allocation site what this ADR
does across a boundary: bind the exact class, seed promoted props from
arguments (strata via the derivation clause), record readonly, exclude
hooked props. The unification is stated as a semantic identity, not a
code merge: **`new Foo(123)` is the degenerate factory whose summary is
assembled inline at depth 0** — the transfer generalizes it to
arbitrary factory depth, and the composition is literal: the summary
the caller rebinds from `createFoo(123)` must equal what
`build_new_object` would have produced for an inline
`new Foo(n: 123)`, because the callee's walk ran `build_new_object`
under the bound argument and the snapshot copied its output. A fixture
pins the equivalence (`$a = new Foo(123);` and `$b = createFoo(123);`
bind identical class/exactness/prop facts). `build_new_object` itself
stays as-is in v1; folding it into a shared summary constructor is an
implementation option the identity licenses, not an obligation.

## 5. What stays out (each one line, each anchored)

- **Objects in arrays** — the `array_map(createFoo(...), $rows)` →
  `array_column(..., 'n')` composition needs a `Fact` that can hold
  ObjRefs (ADR-0036's queued objects-in-arrays adjacent); it is the
  NEXT adjacent, and this ADR's summaries become its element seeds
  when it lands — designed there, not here.
- **By-ref out-params carrying objects** — the by-ref invalidation
  channel stays conservative (ADR-0031/0052 §6 obligation); an
  out-param "summary" is a write-back, a different mechanism.
- **Generators / `yield`** — the return value *is* the Generator;
  yielded objects cross a different boundary (iteration), out of scope.
- **Exceptions as values** — thrown objects travel the throw lane
  (ADR-0040 damming), never the return channel.
- **Membership-only summaries for divergent-class joins** — the
  `Member`-fact carrier could hold them, but the declared-arm floor
  already does (§2.4); revisit with a triaged case in hand.

## 6. The FP surface — the soundness legs, enumerated

A wrong summary premises proof findings: rebound prop facts feed type
checks (`type.property-mismatch`, argument acceptance), rebound
exactness feeds S2 `call.undefined-method` and the arity family. The
legs a reviewer must walk:

1. **Hooked-prop exclusion (FP-16)** — inherited by construction
   (hooked props never enter the callee heap), pinned by fixture: a
   factory returning an object with a hooked promoted param must rebind
   no fact for it.
2. **Escape-sweep honored at summary time** — the snapshot is
   post-sweep (§2.2) and the pre-escape bit is carried (§2.1), so a
   caller-side unknown call sweeps a callee-escaped object exactly as
   ADR-0036 requires.
3. **Stratum preservation** — every prop fact crosses with its stratum;
   the rebind adds no laundering step (ADR-0052 amendment 1). A
   factory whose prop fact derives from an assert-tag narrowing rebinds
   Asserted and can never premise a proof-layer finding in the caller.
4. **A1 exactness for the rebound object** — `class_exact` is copied,
   never promoted: a summary of an exact allocation rebinds exact; a
   membership-only origin (param, `$this`, inexact chain) stays
   membership, and every definite-No consumer gates on the bit as
   today.
5. **Readonly bookkeeping transfer** — the readonly set crosses and
   stays sweep-immune; established readonly props are recorded written
   (caller-side `readonly.reassigned` reasoning stays coherent).
6. **Join discipline** — per-prop join with strata min, all-paths
   presence, exactness only under class agreement, any non-allocation
   path ⇒ no summary (§2.4–5). The join is the walk's existing join;
   no new lattice.
7. **Replayability leg** — the summary depends on the caller only
   through the bound entry state; the memo key canonicalizes name-wise;
   no allocation id crosses; no global ordering enters (§1, §3).

Adversarial probes for review (each becomes a fixture):

- **Conditionally returns param-vs-fresh**
  (`function f(Foo $x, bool $b): Foo { return $b ? $x : new Foo(1); }`)
  — join: same class, exactness `false` (param arm is membership),
  props intersect; no fact survives that is not true on both paths.
- **Recursive factory** (`make()` calling `make()` on some path) —
  stack hit ⇒ inner call yields no summary ⇒ if that path's return
  expression is not itself a local allocation, the outer summary dies
  too. Arm floor; terminates.
- **Factory mutating a global then returning** — the summary describes
  only the returned allocation; caller-side static-property channels
  are already invalidated by every call (ADR-0052 §7), and superglobal
  effects are the effect system's business (ADR-0055). Nothing leaks.
- **Factory storing the object into a static property, then returning
  it** — escaped-before-return ⇒ rebound pre-escaped ⇒ the caller's
  next unknown call sweeps; the shared mutable state can never be
  observed stale through the copy.
- **`return $this`** — membership-only (unless the A1 exactness
  conditions hold), pre-escaped (`$this` is pre-escaped by
  definition); fluent-setter chains get class continuity, not forged
  exactness.
- **Lying `@return` phpdoc on the factory** — irrelevant to the
  summary (walk-derived, §2.6); the lie is the callee's contract
  finding, and where the walk proves nothing the arm floor carries the
  claim at the Asserted stratum as the arm-seeding slice already
  decided.

## 7. Slices and instruments (post-release track)

Sequenced after the v0.1.0 landing point (this ADR binds design, not
release scope). Each slice: full verification protocol, fp-gate
foreground with verbatim triage on any tripwire movement, adversarial
review against §6's probe list, corpus triage wherever checker
behavior can move.

- **T1 — the summary channel over positional descents**: snapshot at
  return sites, the join, the memo-to-value-map upgrade, rebind at
  function-call and method-call sites the descent already reaches.
  Fixtures: the owner probe shapes verbatim (`createFoo(123)->n` is
  `123`; S2/arity fire on the rebound exact class), the
  new-vs-factory equivalence pin (§4), every §6 adversarial probe,
  the FP-16 exclusion, the stratum-crossing pin.
- **T2 — zero-binding factories**: the emission-suppressed
  summary-only walk, empty-key memoization, the duplicate-findings
  non-regression pin (per-scope findings unchanged byte-for-byte).
- **T3 — measurement**: nsrt carries few object-return assertions, so
  the instruments are (i) the legacy-monorepo `annotate` margins
  (object-typed call results gaining class/prop facts, counted
  before/after), (ii) a new fixture family (object-return assertions:
  factory, chain, join, escape, nullable-refusal shapes) added to the
  conformance surface, (iii) fp-gate zero movement on the proof layer.
  Acceptance: the probe fixtures green; margin delta reported with the
  slice, not asserted in advance.

Dependency note: T1 needs nothing in flight beyond the landed heap and
descent; the arm-seeding slice is the floor it widens to and must land
first (it is in flight). The objects-in-arrays adjacent (§5) consumes
T1's summaries and is designed under ADR-0036's queue when scheduled.

## 8. Refusals (each one line, each anchored)

- **Allocation-id threading across scopes** — bans itself under
  ADR-0048 §2/§4 (§1); copy semantics is the architecture's shape.
- **Partial summaries under depth exhaustion or recursion** — a
  partial lie premises findings; widen to the arm floor (§3).
- **Summaries for named/mixed calls in v1** — the descent is
  positional-only; the key design already accommodates the extension
  (§3), so this is sequencing, not architecture.
- **Intersecting the summary with the declared return type** — claims
  do not edit proofs; conflicts are callee findings (§2.6).
- **A membership-only heap object for divergent joins** — the arm
  floor already carries membership; no consumer gains (§5).
- **Exactness promotion anywhere on the path** — A1 verbatim; the bit
  is copied, never computed at the boundary (§6.4).

## Open questions

- Should a membership-only summary also carry the callee's
  guard-derived `Member{yes}` bounds (richer than the declared arm when
  the callee narrowed before returning)? v1: no consumer demands it;
  revisit with a case.
- Provenance rendering: how `annotate`/dump spell a rebound fact's
  origin ("bound at createFoo(...) return" vs the descent's existing
  provenance string) — presentational, decided in T1.
- Whether the summary-only walk (T2) should ever be admitted for
  membership-final receivers once the ADR-0052 §3 final-Member unlock
  lands — deferred with that unlock.
