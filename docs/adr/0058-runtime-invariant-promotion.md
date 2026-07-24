# Runtime-enforced invariant promotion: enforcement outranks annotation

The owner's core-concept statement (2026-07-25, verbatim intent):
*PHPStan treats explicit annotations (`/** @var positive-int */`) as
first-class type information. Steins additionally PROMOTES runtime type
assertions — `if ($n <= 0) throw ...` in constructors and property
hooks, `assert($n > 0)`, userland helpers like
`My\Assert::positiveInt($n)` — to equivalent type information, supplied
seamlessly through CLI, LSP, and MCP.*

Under ADR-0037's trust order the concept lands **stronger** than
annotation parity. An annotation is a claim; an unconditional throw-guard
is enforcement — the surviving path is runtime-proven, in production, on
every execution. The stratification this ADR binds:

| Source | Stratum | Why |
| --- | --- | --- |
| `@var` / `@param` / `@return` | Asserted | A docblock claim (ADR-0037). |
| `@phpstan-assert` / `-if-true` / `-if-false` tags | Asserted | Still a claim — a lying tag must not forge a proof (ADR-0052 §5, unchanged by the assert ruling). |
| `assert($expr)` | **Verified, unconditionally** | Owner ruling 2026-07-25: assert() reads as `if (!cond) throw`; `zend.assertions` is not consulted (ADR-0052 second amendment; the operator owns the disablement risk). |
| Unconditional throw-guard (`if (!cond) throw` / `exit`) | **Verified** | The continuation executes only if the runtime test passed. |
| Userland assertion helper, descent-proven | **Verified** | The descent proves the helper's throw-guard postcondition from its body — zero annotations needed (§3). |
| Userland helper, tag-declared only | Asserted | The tag lane as landed. |
| Userland helper, unresolvable | Unknown — silent | ADR-0004 sound-subset posture. |

Two consequences structure everything below: **intra-scope, most of the
promotion is already landed** and §1 states the current truth precisely
before anything new is designed; **cross-scope, the new core is the
class property invariant** — "every instance of `Foo` has `$n > 0`"
derived from write-site enforcement, held in a per-class table, consumed
at property reads (§4–§6).

## 1. The landed floor — inventory, code-cited

What exists today in `crates/steins-infer/src/lib.rs` (verified against
the code, not the ADRs' intent):

1. **Early-throw arm refinement is LANDED.** `walk_if` walks each live
   branch on a cloned env; `StmtKind::Throw` / `StmtKind::Exit` return
   `Flow::Terminated`, a terminated branch contributes nothing to the
   join, and the fall-through env is the surviving branch's — with
   `else_refinements(cond)` applied at **`Stratum::Verified`**
   (`walk_if`, the `apply_refinements(&else_refinements(cond), …,
   Stratum::Verified)` leg; terminators at `StmtKind::Throw` in
   `walk_trace`). So `if ($n <= 0) { throw new ...; }` narrows `$n` to
   `[1, ∞)` on the code after the `if`, today, as a Verified fact —
   early-throw arms already behave exactly like early-return refinement.
   The same holds inside `match`/`switch` (`walk_match`: a throwing arm
   is `Terminated` and joins nothing) and for a decided-dead guard.
2. **The refinement vocabulary bounds what a guard can say.**
   `collect_refine`/`collect_cmp_refine` produce `Exact` / `NotNull` /
   `Exclude` / `IntRange` / `Truthy` over **bare local variables only**:
   `CondOperand` lowers property fetches, calls, and arithmetic to
   `Other`, and ordering guards refine only against int literals. So
   `if ($this->n <= 0) throw` refines nothing today (property-chain
   narrowing is ADR-0052's deferred N5), and `if ($n <= $m) throw`
   refines nothing (no var-var intervals). `instanceof` narrows through
   the separate `collect_instanceof`/`apply_class_narrowing` lane
   (Member facts, ADR-0052 N4 as landed).
3. **Throw-expression guard forms are NOT consumed.** `$n > 0 || throw
   new ...`, `$x ?? throw ...`, `$x ?: throw ...` lower outside the
   `CondExpr` statement shapes (opaque expression statements /
   assignment right-hands); no refinement, no termination modeling.
   These are the missing intra-scope throw-guard shapes — slice I1.
4. **`assert($expr)` is consumed** (`StmtKind::Assert` in `walk_trace`):
   `then_refinements` on the fall-through, stratum currently gated on
   `Cx::zend_assertions` (Verified iff the flag; Asserted otherwise).
   The 2026-07-25 ruling makes this unconditionally Verified; the gate's
   removal is slice I0 (ADR-0052 second amendment,
   decided-not-yet-implemented).
5. **Tag-declared helper postconditions are consumed, Asserted, both
   positions.** Statement position: `apply_stmt_asserts` (Feature D)
   applies `@phpstan-assert` (Always) specs after every statement's
   calls, maps `@param` names to positional argument variables, narrows
   via `apply_assert_to_var`, and protects the asserted variable from
   the conservative by-ref forget. Guard position: `collect_guard_calls`
   distributes `-if-true`/`-if-false` through nested `&&`/`||`
   (ADR-0052 §6) and `apply_guard_asserts` applies them per branch
   polarity. Always at the Asserted stratum (`apply_call_asserts`),
   positional-only, poisoned-scope-guarded.
6. **Existence-guard vouches are landed** (FP class 15, ADR-0049 §4):
   a positive `method_exists`/`function_exists` guard vouches its symbol
   on the branch where it holds, on both polarities of the `if`.
7. **The heap holds per-walk property facts but seeds none.**
   `HeapObj.props` carries per-property facts with strata; `readonly`
   props are sweep-immune (ADR-0036); constructor walks record facts for
   `$this->p = $n` writes — including the guard-refined fact when a
   ctor param was throw-guarded above the write. But everything dies at
   scope exit: `seed_this_object` deliberately seeds **no property value
   facts** (its comment records the null-property FP that seeding
   declared defaults produced), so a property read in an arbitrary
   method starts from nothing — the exact read-side gap the invariant
   table (§4) exists to fill, and the documented reason it must be
   filled only by facts that survive *every* write.
8. **Hooked properties bind no facts** (FP class 16): a hooked promoted
   param carries `hooked: true` and is excluded from seeding, write
   fact-recording, and readonly; **class-body hooked properties are
   dropped entirely at lowering** (`steins-syntax` `PropertyDecl.hooked`
   doc; only `lower_promoted_params` ever sets the flag). Hook *bodies*
   are not lowered at all today — §5's carve-out needs syntax work
   before any analysis can run.

Summary of the delta this ADR designs: I0 assert-stratum flip (decided
in ADR-0052); I1 throw-expression guard shapes; I2 descent-proven helper
postconditions (§3); I3 the invariant table + hook passthrough (§4–5);
I4 read-side seeding + surfacing (§6–7).

## 2. The promotion principle

**Enforcement, not intent, is what promotes.** A pattern is promotable
iff the analyzed continuation is *unreachable* when the condition fails
— the runtime eliminates the bad path (`throw`, `exit`, an enabled
assertion under the ruling's reading). A pattern that observes the bad
value and continues (a logged warning, a `filter_var` whose failure
branch proceeds, a collecting validator) enforces nothing and promotes
nothing (§8). This is ADR-0031's live-path discipline read as a trust
principle: the walk already only carries facts about paths that run;
promotion adds no new trust mechanism, it extends the set of places the
existing one looks — helper bodies (§3) and property write-universes
(§4).

## 3. Helper postconditions: descent-proven, annotation-free

`My\Assert::positiveInt($n); use($n);` — statement position, no tags.
Today: nothing (the tag lane needs a tag; the descent checks the callee
but returns no facts). The design — a **postcondition summary**, sibling
of ADR-0057's return-object summaries and compliant with ADR-0048 §2 by
the identical argument: the summary is a pure function of (callee CST,
bound entry state, query answers), a legitimate query answer computed on
the same descent, memoized in the same `BindingKey → value` memo
ADR-0057 upgrades the suppression set into.

**Postcondition extraction, precisely.** For a resolved call `H(...,
$v, ...)` where `$v` is a bare variable bound to a **by-value** param
`$p` of `H`:

1. The descent walks `H` (the existing binding descent; zero-binding
   summary-only walks per ADR-0057 §3 when no facts bind).
2. `H` is **postcondition-eligible for `$p`** iff the walk never writes
   `$p` (no reassignment, no by-ref handoff of `$p`) — the entry binding
   survives to every exit, so a fact about `$p` at an exit is a fact
   about the argument *value* at entry.
3. The postcondition is the **join, over every exit that returns**
   (explicit return or fall-off-the-end), of the walk's env fact for
   `$p` at that exit — per-fact join with strata min, exactly the
   ADR-0057 §2.4 join. Exits that terminate without returning (throw,
   exit) contribute nothing: they are the enforcement.
4. If some returning exit holds no fact for `$p`: no postcondition
   (silence over guess, the ADR-0027 ratchet).
5. At the call site the postcondition applies to `$v` as a refinement at
   the joined stratum — throw-guard-derived legs are Verified; a leg
   established only by an inner tag-consumption is Asserted and the min
   rule carries it (§6); the derivation clause (ADR-0052 first
   amendment) needs no extension.

The soundness argument is the by-value copy: `H` returning normally
proves the guards passed *on the value `$v` held at the call*; `$v`
still holds that value after the call (the statement-level by-ref
invalidation already handles the cases where it might not — a
postcondition simply joins the same protected set `apply_stmt_asserts`
uses today). Objects need one more leg: a fact like `NotNull`/`Member`
is a value fact and crosses; interior-state facts about a passed object
do not (the escape/sweep already ran on the call, unchanged).

**Interplay with tags**: when both a tag and a proven postcondition
exist, both apply through the existing replace-if-weaker rule — an
Asserted tag fact never overwrites a Verified proven one (ADR-0052
point 5). A tag claiming more than the body proves keeps applying at
Asserted; the mismatch is deliberately NOT a v1 finding (no
`phpdoc.assert-mismatch` id yet — no triaged case; the lane exists when
one arrives). Guard-position proven postconditions (`if
(Str::isNonEmpty($s)))` proven from the body, i.e. `-if-true` without
the tag) require correlating the return *value* with the path — new
machinery beyond the exit join; deferred, the tag lane covers guard
position meanwhile (§9 open questions).

## 4. The class property invariant

**The fact**: `Inv(C::$p) = (fact, stratum)` — "every reachable
instance of `C` satisfies `fact` of `$p` whenever `$p` is initialized"
— derived purely from enforcement at write sites. For typed properties
PHP adds the complementary runtime guarantee for free: reading an
uninitialized typed property throws `Error`, so a read that runs
observes a written value (or a declared default) — the invariant is a
fact about every *read that returns*, the same enforcement shape as §2.

**Definition (the write-completeness closure).** ADR-0049's enumeration
discipline applied to property writes: the invariant is the **join over
the entire write universe** of `C::$p` — every textual write's proven
fact for the written value at the write site (the constructor's
guard-refined param, a setter's guarded assignment, the declared
default if reachable), strata min through the join (§6). One write with
no provable fact ⇒ no invariant (the declared native type is already
the floor; a table entry exists only where it beats the floor). One
write outside the enumerable universe ⇒ no invariant. The universe legs
per property kind:

- **`readonly` (incl. promoted) — airtight, v1.** Initialization is
  language-restricted to the declaring class's scope; the universe is
  the class body (plus trait bodies flattened into it, plus `__clone`
  reinitialization since PHP 8.3 — enumerated, they are class-body
  text). Sweep immunity is already modeled (ADR-0036); the invariant is
  the natural cross-method generalization of the same language
  guarantee.
- **`private` — class-local enumeration, v1.** All textual writes sit
  in the declaring class body (same-class other-instance access
  included — still class text). Dam legs that void the entry (the
  ADR-0046 posture, each a named check, any hit ⇒ no invariant for the
  affected class): a `Closure::bind`/`bindTo` whose scope argument is
  `C` or unresolved; `ReflectionProperty`/`ReflectionObject` write API
  targeting `C` or an unresolved class; `unserialize` whose class
  universe may include `C` (v1: any un-refuted `unserialize` in the
  project dams every class — honest and cheap; refinement via
  `allowed_classes` literals when a case demands); a dynamic
  property-name write (`$o->{$e} = …`) on a receiver that may be `C`.
  `__set` does not breach the universe: it fires only for inaccessible
  properties and writes the real slot only via class-body text already
  enumerated.
- **`protected` — declaring class + project descendant closure, v1.**
  The ADR-0049 amendment's descendant-closure machinery (class_alias
  edges, anonymous-class obstacle) bounds the universe; the ADR-0047
  partitioning posture makes the project the analysis universe, and the
  vendor-subclass presumption is a recorded leg: a non-final class
  reachable by vendor code keeps its invariant only under the same
  presumption that scopes every other closed-world claim.
- **`public` non-readonly — NO invariant in v1, decided honestly.** The
  universe is every file plus every dynamic leg; the dam-adjacent
  enumeration ("every write site guarded, no dynamic writes, no
  reflection, no hydrator") is exactly the whole-universe leg family
  ADR-0046 exists to refuse guessing about. Unknown-silent. Revisit
  trigger: a measured corpus where public-mutable ctor-guarded props
  carry real yield (the 2007-monorepo density measurement, §9).
- **Hooked (set-hook present)** — only under the §5 passthrough proof.
- **Static properties — out of scope** (instance invariants only; the
  static channel is ADR-0052 §7's conservative invalidation).

**Where it lives.** A per-class **property-invariant table**, computed
as a whole-project query beside the class surface (the `compute_throws`
precedent: a fixpoint-free scan over lowered bodies plus the dam
checks), keyed `(class FQN, prop name)`, each entry carrying fact,
stratum, and the establishing write-site list (provenance for rendering
and for ADR-0048 replay). It is a query answer — a pure function of the
project CSTs and config — so caching, replay, and the LSP/MCP surfaces
consume it like any other (ADR-0048 §2 compliant by construction; no
walk-to-walk mutable coupling, the table reads only lowered bodies).

**Where it is consumed.** At **read time, as a floor** — deliberately
not stored into per-object heap state: a property read (`$this->p`, or
`$o->p` on a receiver with class knowledge) that finds no walk-local
heap fact falls back to `Inv(C::$p)`. Consuming at read time makes
sweep interaction trivial: sweeps erase walk-local knowledge that
arbitrary code may have invalidated, but the invariant survives
arbitrary code *by definition* (every write in the universe
re-establishes it), so it is correct that sweeps cannot erase it — and
wrong to make them try. Membership-only receivers consume the invariant
of the membership bound only if every project descendant's write
universe also proves it (the closure already computed for protected;
for exact receivers, `C`'s own entry suffices). `seed_this_object`'s
no-seeding rule stands unchanged — the invariant is not a seed, it is a
fallback the read resolution consults, which is precisely what the
recorded null-property FP demands: only facts that survive every write
may answer a cold read.

**The `@var` comparison.** An invariant CAN disagree with a written
`@var` — a `@var positive-int` on a prop with an unguarded `= 0` write
leg. That is a **contract finding on the `@var`** at the write site
(the landed property-write contract lane already carries it), never a
trust conflict: the invariant is walk truth, the docblock is a claim,
and claims do not edit proofs (ADR-0037 iron rule; ADR-0057 §2.6's
sentence for returns, verbatim for properties). The read side never
intersects the invariant with the `@var`; where there is no invariant,
the `@var` arm floor applies at Asserted as today.

## 5. The FP-16 carve-out: proven-passthrough hooks

FP class 16 banned fact-binding on ARBITRARY hooks — a set hook stores
whatever it computes, so the assigned value is not the property's
value. The carve-out does not special-case triviality; it **analyzes
the hook body as a function** (the same walk, param `$value`, `$this`
seeded) and proves the stored value:

A set hook is **proven-passthrough for guards `G`** iff:

1. every non-throwing path of the hook body executes exactly one write
   to the backing store, `$this->p = $value`, with `$value` the
   *unwritten* entry param (the §3 eligibility bit, same machinery);
2. no other write to `$this->p` occurs on any path;
3. the guards dominating that write refine `$value` by `G` at Verified.

Then: writes through the hook bind the assigned value's fact
intersected with `G` (ordinary assignment fact-binding becomes sound
again — the stored value provably IS the assigned value), and the hook
contributes `G` as an invariant leg for `C::$p`. **Read-side trust
additionally requires get-hook absence** (a get hook computes its own
answer; a proven set with an arbitrary get still yields no read fact) —
and virtual properties (no backing store) never qualify (condition 1
is unsatisfiable). Hooks are overridable: for membership-only
receivers the carve-out holds only under the descendant closure over
overriding hooks (each override must itself prove passthrough); exact
receivers consult `C`'s own hook only. Soundness altitude: this is the
§2 principle applied inside the hook — the same walk, the same strata,
no pattern-match shortcut; FP-16's ban remains the default for every
hook the analysis cannot prove.

Precondition (from §1.8): steins-syntax must stop dropping class-body
hooked properties at lowering and must lower hook bodies as walkable
scopes — shared groundwork with ADR-0055's effect analysis of hooks,
sequenced once in I3.

## 6. Stratum arithmetic

Nothing new — the ADR-0052 derivation clause composes unchanged:

- An invariant's stratum is the **min over every write leg** (a
  throw-guarded leg is Verified; a leg established by an inner tag
  consumption is Asserted; post-I0, an `assert()`-guarded ctor leg is
  Verified like any throw-guard — the arithmetic simplification the
  ruling buys).
- A §3 postcondition's stratum is the min over returning exits.
- Consumption follows point 5: proof-layer ids require all-Verified
  premises; an Asserted invariant buys silence and contract-layer
  findings only.

## 7. Surfacing: CLI, LSP, MCP — the existing pipeline

The owner's "supplied seamlessly" is a property the fact pipeline
already has; this section states it and adds no machinery. Invariant-
and postcondition-refined facts are ordinary env/heap facts, so:
`check` findings premise on them like any fact; the dump family
(ADR-0053) renders a refined property read through the same
carrier-to-spelling path (`debug.type` on `$foo->n` says the refined
fact, with the `(asserted)` marker where the stratum says so);
`annotate` margins render them through the shared steins-contract
spelling; LSP hover/completion is the ADR-0048 replay answering a
position query whose read resolution consults the same table; MCP
exposes the same query answers through the ADR-0010 agent-first
surface. One addition of substance, presentational only: provenance
rendering ("invariant: established at Foo.php:12, Foo.php:30") from
the table's write-site list — decided in I4.

## 8. Refusals (each one line, each anchored)

- **Promoting non-enforcing patterns** — a logged warning, a
  non-throwing filter, a collecting validator: the bad path continues,
  nothing is proven (§2; ADR-0037).
- **Trusting catch-swallowed guards** — `try { throw-guard } catch
  (Throwable) {}`: the guard does not dominate the continuation. The
  landed try-lowering is opaque (safe today); recorded so a future
  try-model cannot promote through a swallowing catch.
- **Cross-request / persistence invariants** — unserialize, hydrators,
  ORMs materializing instances bypass the write universe; the
  ADR-0046 dam family, named as legs in §4.
- **`@phpstan-assert` trusted-as-certain** — stays Asserted; the
  ruling covers the assert() construct, not tags (ADR-0052 second
  amendment, boundary point).
- **Invariant inference from method preconditions** — a method opening
  with `assert($this->n > 0)` states a hope, not a write; v1 scope is
  constructors + write-interceptors (setters as write sites, hooks per
  §5) only.
- **Public-mutable invariants in v1** — whole-universe enumeration
  refused as guessing (§4; ADR-0046).
- **Storing invariants into per-object heap state** — read-time floor
  instead; storing re-opens sweep bookkeeping for facts sweeps must
  not erase (§4).
- **A third stratum for "enforced"** — Verified already means
  runtime-proven; enforcement is a *route* to Verified, not a new
  trust level (ADR-0052 §5's two-strata economy).

## 9. Slices and instruments (post-release track)

Sequenced against ADR-0055 (hook lowering shared), ADR-0057 (the
descent memo value-map and summary channel shared), and the in-flight
arm-seeding slice (the floor everything widens to). Each slice: full
verification protocol, fp-gate foreground, corpus triage.

- **I0 — assert-stratum flip** (ADR-0052 second amendment): delete
  `Cx::zend_assertions` and the `[runtime]` key; re-pin assert
  fixtures at Verified. Small, first — §6's arithmetic assumes it.
- **I1 — throw-expression guard shapes**: `|| throw`, `?? throw`,
  `?: throw` lowered into the `CondExpr`/terminator model; fixtures
  pin the already-landed `if`-statement behavior (§1.1) alongside, as
  the owner probes verbatim.
- **I2 — helper postconditions**: the §3 extraction on the descent,
  memo entry beside ADR-0057's summaries (sequence with/after T1 —
  both upgrade the memo to a value map, one refactor). Fixtures: the
  `My\Assert::positiveInt` probe, a reassigning helper (no
  postcondition), a tag-vs-proven disagreement (tag stays Asserted),
  a throwing-only helper (`never` return — postcondition vacuous,
  callee termination already modeled).
- **I3 — hook lowering + passthrough + the invariant table**:
  syntax retention of hook bodies (shared with ADR-0055 E-slices);
  the §5 proof; the §4 table for readonly/private/protected with the
  dam legs; fixture families per property kind and per dam leg.
- **I4 — read-side seeding + surfacing**: the read-time floor in
  property resolution, dump/annotate/hover rendering with provenance,
  and the measurement: the legacy monorepo's **ctor-guard density**
  (2007-era code with guards, no docblocks) via `annotate` margins —
  property reads gaining facts counted before/after, the concept's
  acceptance instrument; owner Foo1/Foo2 probes as fixtures.

## Open questions

- Guard-position proven postconditions (`-if-true` without the tag)
  need return-value/path correlation — deferred; is the tag lane's
  coverage enough until a triaged case demands it?
- The `unserialize` dam's granularity: v1 dams every class on any
  un-refuted call — measure how often this voids otherwise-airtight
  private invariants on the monorepo before refining via
  `allowed_classes`.
- Vendor-subclass presumption for protected invariants (§4): does the
  ADR-0047 posture need an explicit leg for "invariant-bearing
  non-final class reachable from vendor", or is the existing
  presumption's scope sufficient?
- Whether the promote transform (phpdoc→native, ADR-0041 family)
  should offer the inverse keystroke: materialize a proven invariant
  as a `@var`/native refinement suggestion in `annotate` margins —
  presentation, post-I4.
