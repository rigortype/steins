# Narrowing and subtraction: stratified guard facts, arm-wise subtraction, the extracted normalizer

Closes the design half of the roadmap's gap 2 (issue #9) and discharges two
standing commitments at once: ADR-0030 deferred "narrowing details
(co-evolving with the branch analysis ratchet)" and subtraction types as an
inseparable pair, and its no-type-combinator amendment commits that when
they land, the type-side normalizer is **extracted from the honesty
renderer's** dedup/subsumption-collapse/precision-ladder logic — never
built as a fresh TypeCombinator layer. Everything below extends the landed
ADR-0031 machinery (`CondExpr` → `eval_cond` verdicts → `Refine` collection
→ `apply_refinements`) in place; nothing replaces it.

1. **Three carriers, one stratum axis — the narrowing fact language.** What
   a guard may bind decomposes by *what kind of knowledge* it is, and each
   carrier already has (or gains) exactly one home:

   - **Value facts** — the four-layer domain (ADR-0035), env-carried
     `Known.fact`, as today. Guards refine them (`Exact`, `NotNull`,
     `Exclude`, `IntRange`, `Truthy` — landed) and this ADR adds the
     subtraction semantics of point 2.
   - **Contract facts** (NEW) — a variable's *declared* type as a lowered,
     syntactic **arm list** (`ContractTy` members), seeded from the
     declaration's envelopes at scope entry and narrowed by guards
     **arm-wise**. This is ADR-0030's sentence made operational: "types
     stay syntactic lists judged arm-wise through the single acceptance
     relation" — subtraction over a declared `User|Guest` is arm deletion,
     not a new union algebra. The four-layer domain stays object-free and
     union-of-bases-free (ADR-0035/0038/0043 §4); the arm list is where
     `int|string` and `A|B` live.
   - **Class facts** (NEW) — guard-derived is-a bounds on an object-holding
     variable: `Member { yes: [Fqn], no: [Fqn] }` ("is-a every class in
     `yes`, provably-not-is-a every class in `no`"), env-carried beside the
     heap's exact class. The heap's `class` stays exactness (allocation-
     proven, ADR-0036); `Member` is deliberately weaker — the fact kind
     ADR-0043 refused to fake with exact-class. Membership is extensional
     (the runtime class of a value is a property of the value), so it
     passes the ADR-0038 bar; what stays banned is provenance, not
     negation or membership.

   Every bound fact carries a **stratum**: `Verified` (a runtime-executed
   test on the live branch, or a native declaration) or `Asserted`
   (docblock claims — ADR-0037's third tier). The stratum is the trust
   design of point 5. Rejected: a single merged carrier ("everything is a
   type") — that is precisely PHPStan's accessory/intersection shape whose
   costs ADR-0035 already declined.

2. **Subtraction over the four layers — layer by layer, committed.** A
   guard's negative information (`!== v`, `!is_int`, `!== null`,
   `notinstanceof`) subtracts a set from a fact. The rule per layer, with
   its soundness argument:

   - **Singleton**: never subtracted — a subtrahend covering the singleton
     makes the *verdict* decide (`eval_cond` → No → the branch is dead and
     pruned). Facts never signal death; the verdict owns death. This
     division of labor is the existing documented choice, restated as law.
   - **OneOf**: exact member removal (landed `exclude_member`), result
     re-canonicalized through `Fact::from_vals` (one survivor collapses to
     Singleton). Subtraction on this layer is lossless by construction.
   - **Refined**: the complement is absorbed **only where the predicate
     vocabulary can spell it**: `null` → the nullable bit; `!== ''` →
     `NON_EMPTY`; falsy-exclusion → `NON_FALSY`; ordering guards →
     interval intersection (all landed); NEW: `!== k` where `k` is an
     endpoint of the current `IntRange` shrinks the interval by one
     (exact); an *interior* point subtraction is a no-op — a sound
     over-approximation, documented. Rationale: intervals minus interior
     points are not intervals, and the canonical-by-construction property
     of ADR-0035 (bitsets and intervals, no normalization pass) is worth
     more than the dead precision.
   - **General**: has **no point-complement representation, and gets
     none**. Base-level subtraction happens one carrier up: `!is_int($x)`
     on a declared `int|string` deletes the `int` *arm* of the contract
     fact (an arm dies when the subtrahend subsumes it with Certainty
     Yes; Maybe keeps it — the silence side). The single **partial**
     deletion on the arm carrier is the `Refined` clause's endpoint rule
     one carrier up: an `int<lo, hi>` arm minus one of its own endpoints
     shrinks by one (a two-point interval collapses to the surviving
     literal; the point interval dies), and an interior point leaves the
     arm whole — an interval minus an interior point is not an interval,
     and the gap has no arm spelling. Null subtracts via the nullable
     bit as everywhere. Rejected: a bounded excluded-value set on
     the abstract layers (`General{base, not: [Val]}`) — for a proof-layer
     tool its yield is a definite-No only when the exclusion removes the
     *last* admissible value of a literal contract, a shape the finite
     layers already cover; revisit precondition: a triaged corpus or
     conformance case whose finding needs an interior-point complement.
   - **Class arms** (contract-fact arms of `Instance` members, and the
     `Member` fact's sets): the polarity asymmetry is the is-a discipline
     (ADR-0043 §3) applied to subtraction. On the **negative** branch
     (`!($v instanceof T)`): arm `M` dies iff `is_a(M, T) = Yes` — is-a is
     inherited, so *every* possible value of arm `M` (any descendant) is a
     `T`, and none survives the negation. Clean, closure-free. On the
     **positive** branch (`$v instanceof T`): arm `M` dies only when `M`
     is `final` (or an enum) **and** `is_a(M, T) = No` — an open class
     could have a descendant that also implements `T`, so non-final arms
     survive Yes-side subtraction. `Unknown` is-a keeps the arm in both
     polarities (FP-safe). The surviving positive branch additionally
     binds `Member{yes: [T]}` at the Verified stratum — the runtime
     executed the test.
   - **Emptied carrier**: an arm list or finite fact that subtraction
     would empty **drops to no-fact** (the landed fallback) — never a
     death signal, per the Singleton rule above.

3. **What class facts feed — membership is still not exactness.**
   Enumerated consumers, nothing more in v1: (a) contract-arm filtering
   (point 2); (b) `eval_instanceof` implication — a later `instanceof T2`
   on a variable holding `Member{yes:[T1]}` answers Yes when
   `is_a(T1, T2) = Yes`, and No when some `T1' ∈ no` satisfies
   `is_a(T2, T1') = Yes` — a `T2` instance would be a `T1'`, which the
   guard excluded (guard-implication precision);
   (c) `catch`-arm matching (same oracle, already migrated by ADR-0043);
   (d) the **declared-receiver lane**: a narrowed arm list feeds
   `phpdoc.undefined-method` (ADR-0049 §8) — contract-layer, descendant-
   closure-laddered there, not here. Explicitly NOT fed: `call.on-null`
   receiver proofs, arity (unsound on declared receivers — ADR-0049 §6),
   `call.undefined-method` (requires exactness, ADR-0049 §4a), and
   binding descent (which impl runs is unknown under mere membership
   unless the membership class is final — deferred one line: a final
   `Member` class is exactness-equivalent and may later unlock descent).
   The heap learns nothing from instanceof on an allocation-bound
   variable: its exact class already answers the oracle.

4. **The extraction — what moves, where to, and the API.** The embryonic
   normalizer is `render_value_domain`/`render_string_group` in
   `crates/steins-edit/src/common.rs`. It currently interleaves two
   concerns; the cut is between them:

   - **Moves out** (to a new `steins_contract::normalize` module —
     steins-contract already owns `ContractTy` and the `admits_*`
     acceptance relation, and both steins-edit and steins-infer already
     depend on it): (i) value-set canonicalization — sort, dedup, the
     computed collapse of literal groups into their predicate class
     (numeric literals → numeric-string, the bool pair → bool, null-fold);
     (ii) the **precision ladder** as data — literal → literal-union →
     `NUMERIC` → `NON_FALSY` → `NON_EMPTY` → base, each step judged by
     the predicate summary, never guessed; (iii) **pairwise subsumption**:
     `subsumes(a, b) -> Certainty` over arms via the single acceptance
     relation, and `dedup_arms(&mut Vec<ContractTy>)` removing Yes-subsumed
     arms with stable surviving order. The narrowing engine's arm deletion
     (point 2) and the renderer's collapse become the same three calls.
   - **Stays in steins-edit** (rendering policy, not semantics): docblock
     literal-safety (`*/`, raw newlines), the CAP-bounded literal-union
     *spelling* decision, quoting/escaping, member spelling order. The
     extraction slice must reproduce today's rendered output
     **byte-identically** (the existing honesty tests are the oracle) and
     leave the fp-gate unmoved.
   - **API surface**, complete: `subsumes(&ContractTy, &ContractTy) ->
     Certainty`; `arm_eq(a, b) -> bool` (mutual subsumption Yes/Yes);
     `dedup_arms(&mut Vec<ContractTy>)`; `summarize_vals(&[Val]) ->
     Option<Vec<ContractTy>>` (the value-set → normal-form half);
     `subtract(&mut Vec<ContractTy>, &Subtrahend)` with `Subtrahend ∈
     { Null, Value(Val), Base(Base), Class { fqn, polarity } }`, judged
     trinary arm-wise per point 2. Nothing else — no `union(A,B)`, no
     generic `remove(T,S)`: joins stay the value domain's job (ADR-0030).

   **Divergence registry amendment (ADR-0030, entry 5 — registered now, as
   the amendment requires):** *semantic type equality in Steins is defined
   only as mutual subsumption (Yes/Yes) over extensional arms.
   Provenance-flavored types (`literal-string` and kin, ADR-0038) are
   undecidable for equality by construction and are barred from the
   normalizer's arm vocabulary — the `ContractTy` arm type carries no
   provenance slot, so the bar is enforced by the type system, not by
   review.* Upstream note: this is the recorded reason Steins has no
   `Type::equals` beside a separate `isSuperTypeOf`.

5. **Guard sources and trust — the stratum discipline.** Three sources,
   two strata, one consumption rule:

   - **Native conditions** (`===`, `!==`, ordering, truthiness, `is_int`
     family via the foldable catalog, `instanceof`): **Verified** — the
     branch executes only if the runtime test passed; the refinement is a
     fact about the live path, fit for the proof layer. Unchanged.
   - **Assert tags** (`@phpstan-assert`, `-if-true`, `-if-false`, negated
     forms — parsed since ADR-0029, `Always` consumed in statement
     position today): **Asserted**, always. A docblock is a claim
     (ADR-0037); a lying `@phpstan-assert-if-true` must not be able to
     forge a proof. Consumption: `-if-true` specs apply on the guard's
     true branch, `-if-false` on the false branch, `Always` on both plus
     statement position (as landed); negated types route through point 2's
     subtraction.
   - **`assert($expr)`**: **Asserted by default** *(amended 2026-07-25 —
     owner ruling: assert() reads as a throw-guard, Verified
     unconditionally, and the `[runtime] zend-assertions` pseudo-constant
     is abolished; see the second amendment below — this bullet is kept
     for the record of the original reasoning)* — verified PHP
     semantics: under `zend.assertions=-1` (production default) the
     expression is *never evaluated*, so the fall-through carries no
     runtime guarantee; trusting it would forge proofs in exactly the
     deployment where the guarded break ships. A `[runtime]
     zend-assertions = "enabled"` pseudo-constant in steins.toml (the
     ADR-0037 §2 PDO precedent; sidecar `env()` can confirm the ini)
     promotes assert()-derived narrowing to Verified — declare the boot
     truth, don't guess it. The lowering models the assert argument as a
     `CondExpr` and applies `then_refinements` to the fall-through env
     (a failed enabled assertion throws).
   - **The consumption rule (binding): a finding's premise stratum is the
     minimum stratum of every fact it consumed; proof-layer ids require
     all-Verified premises; `phpdoc.*` contract-layer ids accept Asserted
     premises** (their claim is conditional on the contract by
     definition — ADR-0050's taxonomy). Asserted facts therefore buy:
     silence (narrowing away a would-be report is always safe), and
     contract-layer findings (`phpdoc.undefined-method` on an
     assert-narrowed union is coherent: same stratum end to end). They
     never premise `type.*`/`call.*`/`offset.*`. This **tightens the
     landed code**: today `apply_assert_to_var` binds an Always-asserted
     fact that is indistinguishable from a proven one downstream (the
     `"asserted"` provenance string is prose, not consulted) — the
     stratum bit becomes a checked attribute of `Known`, and the
     replace-if-weaker rule gains its missing half: an Asserted fact
     never overwrites a Verified one of any layer.
   - Rejected: PHPStan's posture (assert() and assert tags trusted as
     certain) — `treatPhpDocTypesAsCertain` by another door, refused for
     the same reason as in ADR-0037/0050.

6. **Short-circuit refinement — env threading inside `eval_cond`.** Today
   every operand of `&&`/`||` evaluates in the same pre-branch env
   (documented stage-2 deferral). Committed: `And(a, b)` evaluates `b`
   under `entry + then_refinements(a)`; `Or(a, b)` evaluates `b` under
   `entry + else_refinements(a)` (De Morgan: `b` runs only when `a` was
   falsy). The composed verdict stays the trinary `and`/`or`; only the
   operand-evaluation env threads left to right. Refinement *collection*
   (`collect_refine`) is already polarity-correct and needs no change.

   **Guard calls are retained**, not opaqued: `CondExpr` gains a
   `Call(CallExpr)` guard form (today a call in a condition lowers to
   `Opaque { reads }`). Three payoffs, one obligation: (i) receiver checks
   run *inside* the threaded env — the `$x !== null && $x->foo()` shape
   stops seeing a possibly-null receiver (the named regression test of
   issue #9), and the direct env-free pass stands down on spans covered
   here exactly as `mark_dead` already models; (ii) the callee's
   `-if-true`/`-if-false` assert envelopes finally have a consumption
   point, on the matching polarity (the conformance-recorded
   `isNonEmpty($s)` shape); (iii) foldable predicate calls (`is_int` …)
   evaluate to verdicts where the catalog licenses it. The obligation: the
   call's effect semantics survive — by-ref argument invalidation and
   escaped-object sweeps apply at the call's position in the left-to-right
   evaluation order, exactly as the old `cond_invalidations` conservatism
   did, now sequenced instead of blanket.

   **Ternary and `??`**: a ternary's arms evaluate under the guard's
   then/else refinements respectively (the arm-selection verdict logic is
   landed; the arm *envs* join the refinements once guard calls are
   retained). `$a ?? $b` yields `clear_null(fact($a)) join fact($b)`; in
   guard position it refines like `$a !== null ? $a : $b` lowered by the
   same rule. No new machinery beyond the threading.

7. **Property chains and static properties — heap-resolved, explicitly
   scoped.** `CondOperand` gains `Prop { var, prop }`, **depth exactly
   one** (`$x->p`, `$this->p` — the acceptance criterion demands a stated
   scope, and this is it). Resolution rides the allocation-keyed heap
   (ADR-0036): the refinement reads and writes
   `heap[refs[var]].props[prop]`, so alias visibility is correct by
   construction and every existing invalidation stays load-bearing —
   escape sweeps, `$this` pre-escape, readonly immunity. A chain beyond
   depth one has no home today (a prop's `Fact` cannot hold an `ObjRef`);
   deferred-with-design, one line: property values holding allocation ids
   is the object-graph extension ADR-0036 already queued beside
   objects-in-arrays, and narrowing adopts it unchanged when it lands.

   **Static properties** (`self::$p` / `Foo::$p`) are global mutable
   state (their exclusion from ADR-0036 stage 1 was deliberate). Committed
   v1: a scope-local channel keyed `(class FQN, prop)`, populated only by
   guards, **invalidated by every call — resolved or not — and every
   loop/try boundary**, never seeded into any entry state. This closes
   exactly the guard-then-immediate-use shape and nothing more; the
   posture is printed in the fact's provenance so `annotate` shows why it
   died. Rejected: full deferral (the issue's criterion asks for a stated
   scope, and this fits in a page); rejected: treating resolved calls as
   non-invalidating (any callee path may write a static — enumerating
   static-write sets is a later precision axis, not v1).

8. **Loops beyond write-sets — structuring, not fixpoints.** Loops are
   still `Opaque` constructs (write-set ∪ read-set forgotten; `try` is
   the other remaining opaque family and is the throw-damming lane's
   concern, not this ADR's). Committed:
   `While`/`DoWhile`/`For`/`Foreach` become recursive trace constructs
   (the ADR-0031 ratchet, one family), walked **once**, with the
   write-set havoc moved from "forget at the construct" to "forget at
   body entry":

   - Body entry env = scope env minus the body's write-set (computed at
     lowering, including by-ref and invalidation channels), plus the
     guard's `then_refinements` **restricted to unwritten variables**.
     Soundness: any iteration's body sees facts only for variables no
     iteration can write, plus facts established earlier in the same
     straight-line body — both iteration-invariant, so one walk checks
     all iterations. No back edge is ever walked; termination is
     syntactic, not a fixpoint argument.
   - Exit env = join of the normal exit (entry-minus-writes plus
     `else_refinements(cond)` on unwritten variables) and every `break`
     env (without the negated-guard refinements — a break exits with the
     guard still true). `continue` ends its iteration walk and
     contributes nothing further. `while (true)` with no break
     contributes no fall-through — the successor is unreachable
     (the ADR-0031 early-exit discipline extended to loops).
   - `do…while` uses the same havoc discipline (the body must be safe for
     iterations ≥ 2); the tempting first-iteration precision (walking
     iteration 1 under the full entry env) is rejected v1 — two walks of
     one body double the check surface for a shape the corpus has not
     asked for.
   - `foreach` puts the key/value variables in the write-set by
     construction; element facts from a `list<T>`/`array<K,V>` contract
     arrive through the contract-fact lane (Asserted) for free once
     seeding lands — no special case.
   - **Invariant discovery is deferred-with-design**: facts on *written*
     variables (`$i` staying in an interval) need a bounded re-walk to a
     fixpoint, which terminates structurally over the finite-height
     domain (ADR-0035's computed layer descent is the widening).
     Precondition: a measured corpus demand; the local-CFG escape hatch
     of ADR-0031 remains the fallback if recursion ever fails a loop
     shape. Nothing in the one-walk design blocks either.

9. **Entry state and replayability — the ADR-0048 compliance argument,
   explicit.** §2 (scope-walk replayability): every narrowing carrier is
   created at scope entry from the declaration's envelopes and dies at
   scope exit; guards mutate walk-local clones (`benv`/`bclasses`, as
   landed); every oracle consulted mid-walk is a query answer — the is-a
   hierarchy, the callee's parsed docblock envelopes (assert tags), the
   foldable catalog, the `[runtime]` pseudo-constants. Re-running one
   scope's walk later reproduces identical facts from (CST, entry state,
   query answers, fold memo) — no mid-walk cross-scope coupling is
   introduced anywhere above. §3 (canonical entry state): the **contract-
   fact seeding is this ADR's entry-state contribution, defined at
   landing, not retrofitted** — per declaration: native member list
   (Verified) refined by the declared phpdoc envelope (Asserted), the
   ADR-0037 trust order verbatim; observed-caller evidence joins only
   under the exhaustive-enumeration rule ADR-0048 §3 already states. The
   other narrowing kinds contribute *nothing* to entry states (guard
   facts, short-circuit threading, static-prop channels, and loop havoc
   are scope-local by construction) — a deliberately boring answer, which
   is the point. §4 (no global ordering): arm lists are declaration-
   ordered (a CST property), normalization is order-stable, joins
   commute; no whole-project iteration order enters any fact.

10. **Conformance targets and their joint dependencies.** Of the current
    13 automated fails, exactly two are narrowing-owned, and **each needs
    ADR-0049 machinery too — neither ADR closes them alone**:

    - `assertions_instanceof_narrowing`: the else-branch of
      `$value instanceof User` over a declared `User|Guest` leaves
      `{Guest}` by point 2's negative-branch arm deletion (User is-a User
      = Yes); the finding at the `$value->name()` site is
      `phpdoc.undefined-method` — ADR-0049 §8/S6 supplies the id and its
      descendant-closure ladder (both classes final here, trivially
      closed). Sequencing is free: whichever lands second closes the
      case.
    - `assertions_assert_non_empty_list`: the expected finding is the
      `=== []` branch's `$values[0]` — `offset.missing` on the
      `Singleton([])` the landed `Exact` refinement already binds
      (ADR-0049 §7/S3 supplies the id). This ADR's contribution is the
      *other* function: `assert($values !== [])` must narrow at the
      Asserted stratum and stay silent on the fall-through read —
      which the stratum rule gives by construction.
    - Guard on regressions: `regressions_string_narrowing_assert_if_true`
      passes today (the bare `@assert-if-true` is not a recognized tag —
      ADR-0029 prefix rule — and Maybe is silent); consuming *prefixed*
      tags adds only silence, so it must keep passing — a pinned
      regression fixture in the assert slice.
    - No other current fail is narrowing-owned (callables/generics belong
      to issues #1–4 and the in-flight ADR-0051; the rest are registered
      silences or other queues). If ADR-0051's template work makes assert
      tags carry template types (`@phpstan-assert-if-true T $x`), those
      arms lower unrepresentable and are skipped silent — no dependency
      taken in either direction.

11. **Slices for issue #9** — each Opus-sized (one construct family, one
    crate region), each gate-verified (workspace tests, clippy zero,
    fp-gate with verbatim 5-sample triage on any tripwire movement,
    conformance rerun; corpus triage discipline wherever checker behavior
    can move):

    - **N1 — the extraction, zero behavior**: `steins_contract::normalize`
      (subsumes / arm_eq / dedup_arms / summarize_vals / subtract);
      renderer rewired to it with byte-identical output asserted against
      the existing honesty tests; ADR-0030 registry entry 5 recorded.
      Gate must be byte-identical.
    - **N2 — stratum + the assert family**: the checked stratum bit on
      `Known` (Asserted never overwrites Verified; premise-stratum rule
      wired into emitters); statement `assert($expr)` narrowing;
      `[runtime] zend-assertions`; guard-position `-if-true`/`-if-false`
      via the `CondExpr::Call` form; one integration test per tag per
      polarity plus the regression fixture of point 10.
    - **N3 — short-circuit threading**: env-threaded `And`/`Or`, retained
      guard calls with sequenced invalidation, ternary/`??`; the
      `$x !== null && $x->foo()` regression shape pinned. The FP-risk
      hotspot of the wave — measurement-mode corpus run before the slice
      merges.
    - **N4 — class facts and instanceof subtraction**: contract-fact
      seeding (the entry-state contribution), both-polarity arm deletion,
      `Member` bounds and their enumerated consumers; joint-closes
      `assertions_instanceof_narrowing` with ADR-0049 S6.
    - **N5 — property chains (depth 1) and static props**: the `Prop`
      operand over the heap; the every-call-invalidated static channel.
    - **N6 — structured loops**: the four constructs, entry-havoc walk,
      break/continue exit joins, unreachable-after-`while(true)`.

    N1 must land first (N4 consumes its API); *amended 2026-07-24: N2
    lands second — it is a hard prerequisite of every ADR-0049 absence
    slice (S2–S6), not order-free; see the amendment below*; N3–N6 are
    order-free after N2, N3 before N4 preferred (guard calls give
    instanceof guards their call-bearing neighbors). Every slice states
    its ADR-0048 §2 argument
    and §3 contribution in the PR description, per issue #9's acceptance
    criteria.

12. **Refusals** (each one line, each anchored):
    - **A TypeCombinator/TypeUtils layer** — the normalizer is extracted
      from the rendering boundary, never built up front (ADR-0030
      amendment, discharged by N1).
    - **Point-complements on the abstract layers** — near-zero proof
      yield (point 2); revisit only with a triaged case in hand.
    - **Trusting assert()/assert tags as certain** — forged proofs in
      production-disabled deployments; the stratum rule is the whole
      answer (point 5).
    - **Exactness from instanceof** — membership is not exactness;
      ADR-0043's note survives this ADR unchanged (point 3).
    - **A CFG migration for loops** — the recursive-construct ratchet
      covers the demand; ADR-0031's local-CFG escape hatch remains the
      recorded fallback.
    - **Loop fixpoints in v1** — havoc is sound and one-pass; invariant
      discovery is demand-gated with its termination argument already on
      file (point 8).
    - **Negative facts as a provenance-style label channel** — negation
      is extensional and lives in the carriers; ADR-0038's label registry
      stays reserved for provenance, not complements.

## Amendment (2026-07-24): the derivation clause, and N2's place in the order

Source: the pre-implementation soundness audit
(`docs/notes/20260724-adr0049-0052-soundness-audit.md`, G8). Both
points are normative.

1. **Point 5 gains its missing half — the derivation clause.** The
   consumption rule as written is an *emission-time* check: it inspects
   the facts a finding consumed, not the facts those facts were made
   from — necessary but **not sufficient**. Binding now: **a derived
   fact's stratum is the minimum stratum over every fact consumed in
   its derivation**, propagated through every fact constructor — fold
   results over an Asserted operand, composed arrays (an array literal
   containing an Asserted element), heap property writes from an
   Asserted value, and branch **joins** (a Verified arm joined with an
   Asserted arm yields Asserted) — so Asserted can never launder into
   Verified across one derivation step. Counterexample closed (the
   audit's G8 snippet): a lying `@phpstan-assert-if-true int $x`
   narrows `$x` at the Asserted stratum; `$pair = [$x, 99];` composes
   a Singleton array that would otherwise forget its element's
   stratum; `takes_string($pair[0])` then premises a proof-layer
   definite-No — an offset proof plus acceptance definite-No — on an
   Asserted-derived value. Fixtures pinned with N2: that snippet stays
   silent, plus the join fixture (Verified ⊔ Asserted ⇒ Asserted).

2. **Point 11 reordered — N2 is a hard prerequisite of every ADR-0049
   absence slice (S2–S6).** The leakage is live, not prospective:
   `apply_assert_to_var` binds an assert-tag fact whose `"asserted"`
   provenance is prose, and the landed proof-layer definite-No
   emitters consume it indistinguishably — a lying assert tag can
   already premise a `type.*` finding. N2 therefore stops being
   order-free: it lands immediately after N1 and **before any absence
   id fires** — every S-slice consumes env facts, and shipping any
   absence id against the un-stratified `Known` re-opens the hole
   point 5 exists to close. ADR-0049 §10's stage list inherits the
   prerequisite by reference (its amendment's sequencing point states
   the same order from the other side).

## Amendment (2026-07-25): assert() reads as a throw-guard

Source: owner ruling (2026-07-25), verbatim intent: **`assert()` is
treated as always equivalent to `if (!cond) throw` — Verified,
unconditionally. Steins does not consult `zend.assertions`** ("考慮
しません" — the setting is not read at all, not merely defaulted). The
operator who runs production with assertions compiled out or disabled
accepts that risk at the runtime level; static analysis reads
`assert($expr)` at face value as a type assertion, exactly as it reads
an unconditional throw-guard.

This **reverses point 5's assert() bullet**:

1. **`assert($expr)` narrows the fall-through env at the `Verified`
   stratum, always.** The lowering is unchanged (the argument is a
   `CondExpr`; `then_refinements` apply on the fall-through); only the
   stratum assignment moves. Assert-derived facts now premise
   proof-layer findings under the ordinary all-Verified consumption
   rule — the derivation clause (first amendment) applies to them
   unchanged, as it does to any Verified input.
2. **The `[runtime] zend-assertions` pseudo-constant is ABOLISHED.**
   Not demoted to an optional override — the key is removed from the
   `[runtime]` vocabulary; a config carrying it is an unknown-key
   config error like any other. The pseudo-constant *pattern*
   (ADR-0037 §2 — declare the boot truth, don't guess it) is untouched
   and remains the shape for `warning-handler`, `sapi`,
   `pdo-stringify-fetches`, and future boot truths; what the ruling
   decides is that assert() semantics are not a boot truth Steins
   models, but part of the source language's assertion vocabulary.
3. **The honest epistemic note, on the record**: for assert-derived
   facts the proof layer's claim is "proven under the assert-enabled
   reading". Under `zend.assertions=-1` the expression is never
   evaluated and the fall-through carries no runtime guarantee — the
   original bullet's observation stays true as a matter of PHP
   semantics. The ruling assigns that residual risk to the operator
   (the same party who chose to disable the runtime check), not to the
   analysis. ADR-0002's zero-FP identity is read accordingly: a
   finding premised on an assert-derived fact is not a false positive
   when the assertion would have caught the value; it is the runtime
   check the operator turned off, reported statically.
4. **Boundary: the ruling covers the `assert()` CONSTRUCT only.** The
   `@phpstan-assert` / `-if-true` / `-if-false` tag family stays
   **Asserted** — a docblock is a claim (ADR-0037), and a lying tag
   must still be unable to forge a proof. The Verified path for
   annotation-free assertion helpers is descent-proven postcondition
   extraction (ADR-0058), not tag trust.

**Status: implemented (slice I0).** The `StmtKind::Assert` narrowing now
binds `Verified` unconditionally; the `Cx::zend_assertions` flag, the
`[runtime] zend-assertions` config key, and their plumbing are deleted
(a `steins.toml` still carrying the key hits the `deny_unknown_fields`
exit-2 path — the intended hard-config-error outcome); the assert
fixtures are re-pinned at Verified. The `@phpstan-assert` tag family is
unchanged (Asserted). The type specification now describes the
Verified-always behavior.

## Amendment (2026-08-01): a dispatch rung may read the contract lane where the value lane holds only the envelope

Landed with the string-predicate transfer slice (issue #77) as a flagged
deviation; recorded here for ratification. Point 1 splits the carriers —
scalar declarations live in the **contract arm lane**, the value lane
holds proven values and guard refinements — and point 9 seeds entry
states accordingly: a native `string $s` contributes `Fact::General` to
the value lane, and the `@param non-empty-string` refinement exists only
as an `Asserted` arm beside it. The argument-dependent return rung
(ADR-0061) dispatched on the value lane alone, so a transfer rule that
asks "what do we know about the subject string?" was blind to the
declared refinement even though the checker carried it.

The amendment: **at a dispatch site, where the value-lane fact is only
the envelope (`Fact::General`), the rung may read the variable's
declared contract arm lane instead** — lowered to ONE fact
(`steins_contract::to_fact` over every arm; any arm that does not lower,
or a multi-fact list, declines), carrying the **weakest stratum any arm
holds**. A refinement the docblock merely claims therefore enters the
transfer `Asserted` and can never premise a proof-layer finding; a
value-lane fact stronger than the envelope still wins outright.

What this does **not** open, on purpose:

- **No second seeding.** Entry states are untouched; the read is
  dispatch-site-local and read-only, so point 9's replayability argument
  carries over verbatim (the arm lane consulted is walk-local state).
- **The carriers stay split.** The value lane gains no union-of-bases
  layer and no scalar-refinement copy of the arm lane; the read
  *projects* the arm lane at one consumption point. The union-fold seam
  (ADR-0028's 2026-08-01 amendment) deliberately declined the same
  projection for scalar `@param` unions — a projection is justified
  per-consumer, not granted globally.
- **The stratum rule is load-bearing, not decorative:** the projected
  fact's grade is what keeps a lying docblock unable to forge a proof
  through a transfer rule's output (point 5, ADR-0037).

## Note (2026-08-01): the `Value` subtrahend is wired to the arm lane

Completion of point 2, not new design. `Subtrahend::Value(Val)` has been
part of the point-2 API since the normalizer slice, and
`normalize::subtrahend_covers` has judged it arm-wise and soundly since
then; what was missing was the **wire** from the walk. Guard subtraction
on the contract arm lane reached it only for `instanceof`
(`Subtrahend::Class`) and `!== null` (`Subtrahend::Null`), so an
identity guard over a scalar arm list — `if (strpos($h, $n) !== false)`,
the single most recognizable PHPStan narrowing there is — left the
declared arm list wholly untouched.

`Refine::Exclude` now subtracts `Subtrahend::Value(v)` from the
variable's lane on the branch that establishes it. The judgment is the
one point 2 already states: **an arm dies iff the subtrahend covers it
with `Yes`; `Maybe` keeps it.** Nothing about the relation moved. What
made this worth doing now is the ADR-0069 / issue #79 declared-return
floor and the #81 line: 1,708 mined functionMap rows now enter the arm
lane, hundreds of them `T|false`, and every one of them was previously
un-narrowable at exactly the guard PHP code writes.

The rule's two sides are one rule, and both are pinned: `!== false`
deletes a `false` arm, and does **not** delete a general `bool` arm —
`bool` has an interior point (`true`) the guard says nothing about, so
the coverage is `Maybe`. The interior-point discipline of point 2's
`Refined` clause is the same discipline here, one carrier up.

Deliberately **not** landed:

- **`Refine::Truthy` reaches nothing.** `if ($pos)` over `int|false`
  excludes `0` as well as `false`, so it is not a value subtraction at
  all. Wiring it as one would silently narrow to `int` on a branch where
  the value cannot be `0` — PHPStan's classic `strpos` footgun, mirrored.
  It needs its own designed subtrahend and its own measured slice.
- **No keep-only narrowing on the positive branch.** `if ($x === false)`
  does not intersect the arm lane down to `{false}`; the value lane's
  `Refine::Exact` already owns that branch, and the arm lane is a
  subtraction carrier by construction — every mutation on it removes
  arms it can prove dead.

Fixtures: `crates/steins-infer/tests/false_arm_strip.rs` (the mechanism,
both directions, all four identity spellings, the two refusals);
`crates/steins-infer/tests/declared_return_floor.rs` (the floor-row /
hand-written-row parity pins, re-pinned by this change).

## Note (2026-08-02): adjacent int arms absorb, and an int range spells as PHPStan spells it

Two rulings from issue #90, on either side of the point-4 cut, which is
why they are recorded together.

**Semantics.** `positive-int|0` and `int<0, max>` are one denotation
spelled two ways, and the point-4 dedup could not see it: neither arm
subsumes the other, so both survive. `dedup_arms` therefore gains an
**interval absorption** — a literal adjacent to an interval extends it,
and two touching or overlapping intervals become their hull. This is a
*computed collapse* in exactly the sense point 4 already grants the
normalizer for literal groups (numeric literals → `numeric-string`, the
bool pair → `bool`), not a widening: the merge is refused wherever the
union is not itself an interval, so a gap is never bridged, and it never
narrows because both inputs stay inside the result.

The interior-point discipline holds here as it does for the `Value`
subtrahend: `5` beside `int<1, max>` is not an absorption case at all —
subsumption has already deleted it — and the merge refuses it besides.

The absorption's one known cost — an endpoint swallowed into the
interval could no longer be subtracted, so `!== 0` over the absorbed
`int<0, max>` narrowed nothing where `positive-int|0` would have lost
its `0` arm — is discharged by the follow-up slice: `subtract` gains
the absorption's mirror, the point-2 endpoint rule on the arm carrier
(`subtract_arm`, the per-arm judgment stratified carriers map with).
An `int<lo, hi>` arm minus one of its own endpoints shrinks by one, a
two-point interval collapses to the surviving literal, the point
interval dies, and an interior point keeps the arm whole. Merge and
clip are one refusal read in both directions: arms combine only where
the union IS a single arm, and an arm partially deletes only where the
remainder IS a single arm.

This adds one name to the point-4 API, `merge_int_arms(a, b) ->
Option<ContractTy>`, which is the pairwise primitive the fixpoint runs
and the only way a stratified carrier can reuse the rule without
reimplementing it. The arm lane in steins-infer does exactly that: it
keeps its own stratified dedup (arms carry strata, arm lists do not), and
a merged arm takes the **min** stratum of the pair, for the derivation
clause's reason — it is only as strongly held as the weaker of the two
claims it replaces.

**Spelling.** Separately, and on the renderer's side of the cut where
point 4 puts it: an int range now spells as `int<lo, hi>` with `min`/`max`
for the domain ends, never as `positive-int` / `non-negative-int` /
`negative-int`. Those three remain phpdoc **input** sugar that
`lower_identifier` accepts; they are not output, because PHPStan folds
each into an integer range and describes every range as the interval — no
nsrt fixture asserts a keyword form anywhere. Spelling the sugar back out
was the dump disagreeing with PHPStan about a set the two agreed on.
`describe_fact`'s diagnostic prose is unaffected: a finding message is not
the dump surface.

Because the collapse is semantic rather than rendered, the mined
functionMap canon is undisturbed by it — `floor_row` lowers and flattens
without deduping, and its round-trip check compares that pre-dedup arm
multiset — so the canon still states `int<1, max>|0` and the consumer
absorbs it when the row enters the lane. Had the collapse been renderer
policy instead, every such row would have re-lowered to one arm against a
countersigned two and silently fallen back to functionMap's own string.

Fixtures: `crates/steins-contract/src/normalize.rs` (the rule, its
fixpoint, the gap and interior-point refusals, the boundary
denotation-preservation and the `min`/`max` overflow guards — and, for
the clip, both endpoints, the literal collapse, the point-interval
death, and the i64 domain ends); `crates/steins-infer/tests/`
`false_arm_strip.rs` (`strpos` reads `int<0, max>|false`,
`int<0, max>` under the `!== false` guard, `int<1, max>|false` under
`!== 0`, and the interior-point refusal under `!== 5`).

## Note (2026-08-09): §6's stand-down clause, implemented (issue #266 slice 1) — ratified 2026-08-09

Completion of point 6, not new design. The clause was written into §6 with
the guard-call retention — "the direct env-free pass stands down on spans
covered here exactly as `mark_dead` already models" — and N3 landed the
verdict half (env-threaded `&&`/`||`, threaded ternary arm envs) without
it. The residue was a live false-positive class, because the two passes
disagreed about what runs: the walk knew `$x === 2 && f("bad")` never
evaluates its right operand, and the env-free direct pass reported inside
it anyway. Four shapes, all measured firing before this note:

* `a && b` with `a` decided **No** — `b` is unevaluated;
* `a || b` with `a` decided **Yes** — `b` is unevaluated (De Morgan mirror);
* `$c ? A : B` with `$c` decided — the untaken arm is unevaluated;
* `$a ?? $b` with `$a` proven set-and-non-null — `$b` is unevaluated.

Each is recorded through the same `dead` channel a decided `if` already
uses, and therefore inherits its whole discipline unchanged: only a
**decided** verdict records anything, and only the plain per-scope walk's
regions escape (a binding descent's regions are dead for that caller's
bindings alone, and are discarded as they always were).

Two boundaries are load-bearing:

* **Reachability stays proof-only.** A `??` left operand whose presence is
  only `Asserted` does **not** stand the direct pass down. Marking a span
  dead is a reachability claim, and §5's rule that a docblock claim buys
  silence applies to *narrowing*, never to declaring live code unreachable
  — the same line `eval_cond` already draws at `Isset`. Pinned as a
  fixture.
* **Spans, not calls.** A condition operand is a lowered `CondExpr` with no
  span of its own, so the record is per **call span** there; the ternary
  and `??` arms carry real CST extents (`ArgValue::Ternary`'s
  `then_span`/`else_span`, `ArgValue::Coalesce`'s third field — outside the
  `Hash` impl, since position is not denotation). A non-call site inside an
  unevaluated operand (a class reference, a constant fetch) filters on its
  own offset and is **not** covered; recorded as a known residue.

Direction of movement: **finding-removing only**. Nothing here mints a
verdict, a fact, or an id; php-typing-conformance is unmoved (206/214
before and after, same eight fails).

Fixtures: `crates/steins-infer/tests/short_circuit_dead_operands.rs` —
every shape as a decided/undecided pair, the Asserted-presence stratum pin,
the short-circuiting chain, and the per-span (not per-call) proof that an
identical call on a live path keeps firing.

## Note (2026-08-09): a class-typed assert tag reaches the arm lane (issue #266 slice 2) — ratified 2026-08-09

Completion of point 5's consumption rule for the one type shape it could not
carry, plus the §3(d) consumer it was always meant to feed. Not new design.

The state of the family before this note: `@phpstan-assert Guest $v`,
`-if-true`, `-if-false` and their negations parsed, resolved to a caller
variable, and then established **nothing at all**. The value lane is the
only carrier `apply_assert_to_var` wrote, and that lane is object-free by
construction (ADR-0035, ADR-0043 §4) — so `assert_fact_of` declined every
`Class` arm and the whole road ended in a silent `return false`. The same
claim written as `if ($v instanceof Guest)` narrowed the declared arm lane
and fed `phpdoc.undefined-method`; written as a tag it did not.

**What lands.** A class-typed spec narrows the **contract arm lane**,
arm-wise, through `normalize::subtract_arm` — the same single judgment the
`instanceof` guard uses, with the same polarity asymmetry (point 2's
class-arm rule): a positive spec deletes an arm only when it is final/enum
and provably not a `T`; a negated spec (`@phpstan-assert !Guest`) deletes an
arm iff it is-a `T`; an `Unknown` is-a keeps the arm either way. The class
name resolves in the **callee's** namespace context, which is where it was
written. Surviving arms keep their own strata, and the declared-receiver
lane already routes by minimum stratum (issue #196) — so the tag buys the
contract-layer finding it is entitled to under point 5, and no proof.

**The `Member` carrier is refused, and this is the slice's one deferral.**
`Member` has no stratum slot — point 2 binds it at `Verified` because a
runtime `instanceof` executed — and its consumers include point 3(b)'s
`eval_instanceof` implication, which decides verdicts, prunes branches and
marks regions dead. Routing a docblock claim into a *reachability* decision
is exactly the laundering point 5 exists to prevent. The precondition for
lifting this is a stratum on `Member` and a demand for it, not a quiet
insertion.

**One conservatism lifted, minimally.** A guard call's read set is dropped
wholesale before the branch clones, which erased the very lane a
`-if-true` tag exists to narrow. The arm lane the tag names is now taken
before that drop and restored after it — the statement-position rule
verbatim (an assertion tag's contract is a stronger statement about the
argument than "the call may have mutated this by reference"). Three gates
keep it narrow: class-typed specs only; the callee's parameter at the
asserted position must be **by value** (ADR-0070 — a separate zval cannot
reach the caller's binding); and the variable must occur nowhere else among
the condition's calls. The **arm lane only** is carried: the value lane and
the `Member` sets still drop, so no *fact* survives a guard here that did
not survive one before, and `collect_call_opaque_reads`'s standing refusal
to lift the general case stands.

Direction of movement: this **adds** contract-layer `phpdoc.*` findings
where a docblock claim narrows a declared union, and adds nothing on the
proof layer. php-typing-conformance is unmoved (206/214 before and after,
same eight fails — none narrowing-owned;
`regressions_string_narrowing_assert_if_true` keeps passing, as point 10
requires).

Fixtures: `crates/steins-infer/tests/assert_tag_class_lane.rs` — the
`instanceof` reference narrowing beside the tag forms, both polarities of
both guard kinds, the negated spec, and four pins: the proof-layer absence
id stays silent (exactness is not membership), a Verified null is not
overwritten, no value fact is minted, and ADR-0029's prefix rule still gates
the family.


## Note (2026-08-09): a count comparison narrows the shape it counts (issue #272) — ratified 2026-08-09

New **narrowing vocabulary**, not a new carrier and not a return rung. The
argument-dependent `count()` rung has read `ShapeFact::count_range` since
ADR-0062 S4; what was missing was the other direction — a `count($x)`
comparison in *guard* position telling the shape something. `collect_refine`
and `collect_shape_guards` dispatched on `Var`/`Literal` operands and a fixed
predicate name set, so `if (count($x) > 0)` narrowed nothing at all.

**The shape accessory.** `ShapeFact` gains one field, `count_bound: IntRange`,
whose default (`int<0, max>`) is "nothing learned". It is an accessory in the
S4 sense — a claim about the whole array, like `non_empty` and `is_list`, not
about a key — and its algebra is fixed by three rules, all inside
`normalize_counted` so no caller can restate them:

* **Meet** (the narrowing, `ShapeFact::narrow_count`) is interval
  intersection. An empty intersection **widens back to the receiver** rather
  than bottoming: a count claim that contradicts the structure is not a death
  signal (§2), and the shape lane has no bottom to signal it with.
* **Join** is the interval **hull**, and a side that learned nothing absorbs
  the other — the join of "at least 3 entries" with "no idea" is "no idea".
  This is the same shape as `non_empty`'s `&&` in the join, generalized from
  a floor of one to a floor of *n*, and it is why the two flags cannot
  disagree: `non_empty` is *derived* from a floor of 1 or more, in `normalize`.
* **Reading** it is `count_range()`, which meets the accessory with the
  structural interval (one entry per `Required` field; a `Sealed` tail's
  declared key set as the ceiling). The accessory is never read alone, so a
  declaration and a guard can only sharpen each other.

**The sealed/unsealed split, as the issue framed it.** An unsealed shape can
gain a floor and a ceiling, and that is all it can gain: a floor over an
unsealed tail says how many entries exist, never which keys they are. A
**sealed** shape whose declared key set the floor exhausts additionally pins
every declared key **present** — `array{0: string, 1?: string}` under
`count($x) > 1` has no room for key `1` to be absent. That discharge happens
in `normalize_counted`, at presence stratum `witnessed: false` (A-G9): the
evidence is a count comparison, not an observation of the key itself.

**Invalidation follows the accessory's two halves separately**, because the
two events are asymmetric. An offset write can only add an entry, so the
ceiling does not survive it (`relax_count_ceiling` at the write site) and the
floor does. An `unset` can only remove one, so the floor goes (with
`non_empty`, which `mark_absent` already dropped for the same reason) and the
ceiling stays.

**Where the dispatcher gained the operand.** `collect_shape_guards`'s `Cmp`
arm tries `count_guard` first: it recognizes `count($x)`/`sizeof($x)` on
either side (`count_subject`), bounds the other side to an `IntRange`
(`operand_int_bound` — a literal int, or a binding carrying an int interval,
so `count($x) === $n` with `$n` an `int<3, 5>` bounds as a literal would),
normalizes the operator to read left-to-right, negates it on the false branch,
and reads the weakest claim true over the whole bound interval. Both
polarities record. `assert(count($x) >= 1)` needs no plumbing: assert lowers
its argument to the same `CondExpr` and runs the same walk at its own stratum
(the 2026-07-25 amendment), and a docblock-only claim stays `Asserted` through
`apply_helper_guard` exactly as every other shape guard does.

One **lowering** change was required and is deliberately narrow. An ordering
comparison with an opaque operand lowered to `CondExpr::Opaque`, which
discarded the comparison before any consumer saw it. That fallback is now
lifted for a `count`/`sizeof` operand **only**, matched syntactically in
steins-syntax (which has no project view); whether the name denotes the global
builtin is still settled on the consuming side. Lifting it generally would let
`preg_match($re, $s, $m) > 0` reach the out-parameter seed, which is a
precision change of its own and is not taken here.

**Value-lane coherence, and why this guard needs it.** Every other guard here
narrows `Fact::Shape` and says nothing about a *proven*
`Fact::Singleton(Val::Array(…))` — it cannot, because presence and list-ness
are already decided on a literal. A count comparison can contradict one:
`count($x) > 0` on a binding the walk proved to be `[]` names a branch that
literal cannot reach.
The lowering lift is what makes this reachable at all — before it, such a
comparison lowered to `CondExpr::Opaque` and the opaque path dropped the guard
call's read set, so the stale literal was forgotten rather than narrowed. So the
count guard narrows **both** lanes in one place: a proven array whose entry
count the branch's interval excludes is replaced by the honest floor, "an array
whose entry count lies in the interval". Not by a lifted-and-narrowed shape —
the entries are exactly what the branch refutes, so neither their keys nor
their value types survive as proof — and not by a dead region, which is the
verdict's business (§2). A literal *inside* the interval is kept untouched: it
is sharper than any shape. A `OneOf` of arrays filters member-wise; a `OneOf`
with a non-array member is left alone, since `count()` accepts a `Countable`
too. Measured on the private corpus this is not optional: without it the
sharpened branch convicts on the stale literal, and with it four *pre-existing*
false positives of the same class (a `[]` from a defaulted parameter or a
call-site descent, reported inside the arm a count guard refutes) go away.

**Four refusals**, each a soundness or scope requirement: the mode argument
(`count($x, COUNT_RECURSIVE)` counts a different number, and the named
spelling refuses with it); a project function shadowing the simple name
(`global_function_callee`, the rule every builtin recognizer opens with); a
bound the engine cannot pin to an interval; and `count($a) === count($b)`,
which relates two bindings and bounds neither. `!==` narrows only against the
point `0`, where the complement is the domain's own floor — an interior point
exclusion has no interval spelling.

ADR-0048's three constraints hold unchanged: the guard is decided from the
condition's own syntax and applied in the scope walk that built the env it
reads (walk-local, replayable), it introduces no new fact kind — the accessory
rides `Fact::Shape` — and it carries no global ordering.

Direction of movement: **both**. It *adds* precision to `count()` readings and
to sealed-shape key presence, which can discharge an absence finding
(finding-removing) and can equally let a downstream check that needed a key's
presence now fire (finding-adding). Nothing here mints a verdict or marks a
region dead — `Fact::Shape` still does not decide guard verdicts, and
`shape_facts_do_not_decide_guard_verdicts` is still the tripwire that says so.

Measured on the private corpus, `phpdoc.*` moves 508 → 507: **four removed**,
all the value-lane class above; **three added**, all triaged TRUE against the
source — two `@param` declarations that omit a key the `@return` feeding them
declares (`Order::_capture`/`_refund`, missing `payment_policy_version`), and
one that omits two (`ActionHelper::_changeToCancelInTransaction`). The third
is the sharpening at work rather than a new claim: an `array<X>` whose element
type violates the contract still admits `[]`, which the contract accepts, so
the verdict was `Maybe`; a proven non-empty floor makes it a definite `No`. One
further site keeps its finding with a sharpened rendering for the same reason
(`array<array>` → `non-empty-array<array>`).

**Ratified 2026-08-09, and four judgment calls inside it confirmed as ruled
rather than left as implementation residue.** Each stays exactly as implemented:

* The exhausted-key-set discharge on a sealed shape writes
  `Presence::Required { witnessed: false }` and nothing stronger. `witnessed`
  means the key was *observed*; a count comparison is arithmetic evidence about
  how many entries exist, so the presence it forces is derived, not seen. A
  consumer that needs an observation must keep asking for one.
* ~~A count guard does not clear `nullable`.~~ **Superseded 2026-08-09 (issue
  #289) — see the note below.** The reasoning recorded here at ratification
  was backwards: it read `count(null)`'s TypeError as `array_key_exists`'s
  null-base answer, when the two are opposites (a raised exception is not an
  answer at all). The conservative reading itself was sound — it only
  withheld precision — but the ground it stood on was not §2 reachability
  the way the text claimed; issue #289 corrects both the ground and the
  behavior.
* **The lane-emptying refusal is scoped to `Count`, and the general rule is
  untouched.** `subtract_shape_arms` still removes a binding from the
  contract lane when every arm dies — an emptied lane under a structural
  guard means the binding is out of vocabulary, which is the honest outcome.
  A count guard refutes on arithmetic, so an emptied lane would instead
  assert that the branch is unreachable, and that claim belongs to the
  verdict, not to a subtraction — besides which the erasure would outlive
  the branch and reach the join. Only the `Count` arm therefore leaves the
  lane whole; no other guard's behaviour moves with it.
* **A refuted `Singleton` widens to `plain_array()` narrowed by the interval,
  not to a summary of the tail it lost.** The branch refutes the entry count,
  which is precisely what the literal's keys and value types were evidence of,
  so none of them survive as proof. "An array whose entry count lies in this
  interval" is the whole of what is left, and it is still a narrowing. A literal
  inside the interval keeps its proven value untouched, being sharper than any
  shape.

Fixtures: `crates/steins-infer/tests/count_guards.rs` — both polarities of the
floor, the Yoda spelling, `sizeof`, the ceiling, the identity pin, the bounded
variable, the sealed exact-count pin beside its unsealed complement, the
assert lane, conjunction distribution and negation, the four refusals, the
write/unset invalidation pair, and the value-lane trio (both corpus shapes
pinned silent, the unguarded literal still convicting, and a surviving literal
keeping its proven value); `crates/steins-domain/src/shape.rs` — the
accessory's meet, join, clamp and contradiction-widening, and its extensional
reading in `admits`.

## Note (2026-08-09): a type may be read as a value only when it denotes exactly one value (issue #260) — ratified 2026-08-09

The comparison-operand slice reads a declared contract arm as a *value*:
`cmp_operand_candidates` falls back to the arm lane when the proven lane has
no candidates, and `contract_literal_value` turns each arm into an
`ArgValue`. The literal arms are unremarkable — `ContractTy::LitInt(1)`
denotes `1` and there is nothing to argue about. One row is not: a `Shape`
that is sealed, field-less, not `non_empty` and carries no unsealed tail is
read as the value `[]`. That is the only place in the engine where a *type*
is read as a *value*, and the owner ratified it together with the boundary
that explains why it is not an exception.

**The rule, general.** The projection from a type to a value is admissible
exactly when the type's denotation is a **singleton set** — when the type
admits one value and no other. `array{}` qualifies: sealed says no key
beyond the declared ones, field-less says there are no declared ones, and
not-`non-empty` is the third condition rather than a redundancy, so the set
of admitted arrays has exactly one member. `int<1, 1>` would qualify for the
same reason and may be added when a consumer wants it. `non-empty-string`
and `array<int>` do not, and no amount of narrowness makes them: a set with
two members has no value to project to, and picking a representative would
be a guess of the kind "silence over guess" exists to refuse. This is why
the rule is stated as a denotational test and not as a list of accepted
`ContractTy` constructors — a future constructor is admitted by proving its
denotation is a singleton, which is an argument someone has to make, not a
pattern someone can add.

**Why this note lives here and not in ADR-0061.** ADR-0061 governs the
argument-dependent return ladder — what a *builtin call* computes from its
arguments — and this projection is not a rung on it: no builtin is involved,
the consumer is a comparison operand, and the result is a candidate set for
a verdict rather than a return fact. The governing precedent is this ADR's
2026-08-01 amendment, which established that a consumer may read the
declared arm lane where the value lane holds nothing sharper, at the
declaration's own stratum, and said in the same breath that **a projection
is justified per-consumer, not granted globally**. Issue #260 is the second
consumer to claim it, and the singleton-denotation test is the general form
of the licence: the 2026-08-01 amendment projects an arm list to *one fact*,
this one projects an arm to *one value*, and both refuse where the
projection would have to choose.

**Two limits, both load-bearing.** The projection stays at the **fact
seam**: the candidates enter at the arms' own (minimum) stratum, so a
declaration-backed comparison is `Asserted` and can premise a contract-layer
finding and nothing more. `resolve_literal` — the proof-layer seam — must
not see it, and does not: the arm lane is read in `cmp_operand_candidates`
alone, and a value that reached the proof layer through this road would be a
docblock forging a proof (point 5, ADR-0037). And the forthcoming
**family-lift** slice — the one that takes the operator-value node past
comparisons to the arithmetic and bitwise operators issue #260 left
declining — **may not widen this projection without a new ruling**. Lifting
it to more `ContractTy` constructors, to more consumers, or past the fact
seam is each a separate decision, and the singleton-denotation test is the
bar every one of them has to clear.

## Note (2026-08-09): a value-position comparison's undecided `bool` is Verified (issue #260)

Owner ruling, binding, and recorded here rather than in ADR-0061 because what
it settles is an application of **§5's derivation clause** — ADR-0061 §3
consumes that clause for the builtin-return rung and explicitly adds no tier
and forks no join, so it is a reader of this rule, not its home.

`eval_binary_fact` (issue #260) evaluates a value-position comparison through
`eval_cmp` and maps the three verdicts onto `Yes → true`, `No → false`,
`Maybe → bool`. As it shipped it gave all three the **minimum** stratum over
its operands, so `bool (asserted)` appeared whenever an operand's value came
from the declared arm lane (§1) — a docblock. That is now split, because the
three verdicts do not make the same **kind** of claim:

* **`Maybe → bool` is Verified, always.** A PHP comparison operator evaluates
  to a `bool` whatever it is handed; that is the operator's own language
  guarantee, owed to nobody's docblock, and not a claim inherited from any
  operand. In this arm no operand refinement survived into the fact at all —
  the verdict is undecided precisely because nothing about the operands was
  usable — so an inherited `Asserted` marker records a premise the conclusion
  never consumed. The derivation clause is a rule about facts a derivation
  *consumed*; a vestigial marker is not conservatism, it is noise. This is the
  same shape as ADR-0061 §3's closing line, "the envelope floor is Verified
  always".
* **`Yes → true` / `No → false` keep the operand-derived minimum.** These are
  claims about **which** bool, and that claim genuinely rests on the operands
  — it is `eval_cmp`'s verdict over candidate values the declared arm lane may
  have supplied. A lying `@param 1 $one` must never launder into the proof
  lane through `$b = ($one === 1)`, and it still cannot.

So the stratum here is a function of the verdict, not of the operands alone.
The split lands in `eval_binary_fact`'s final `match`: the `Yes`/`No` arms
carry `derived` (the unchanged `min` computation), the `Maybe` arm carries
`Stratum::Verified` unconditionally.

**What it buys, stated honestly.** A Verified `bool` may premise a proof-layer
finding where an Asserted one may not (the all-Verified premise rule), and it
is not held off the Verified envelope by N2's replace-if-weaker half. That is
latent headroom rather than banked recall: **measured, this slice adds no
finding.** nsrt is unmoved (2411 headline / 2817 admissible / 11042 differ,
identical before and after), and the private corpus gate is unmoved on every
grade (`phpdoc.*` 556, `throw.*` 44107, possibly-grade 150, proof-layer
diagnostics 0 — identical before and after; the `phpdoc.*` RED against its 549
baseline is pre-existing drift on master, not this change).

Two independent reasons account for the flat measurement, and both are worth
recording because the ruling was framed around a recall example that does not
hold:

1. **`bool` into `int` is not a definite No at all.** In coercive mode PHP
   accepts it, and `is_type_error` says so — a *proven* `true` passed to
   `f(int $i)` is silent today and should be. Only a class-typed parameter
   makes a bool a proven `TypeError`, which is what the new fixture pair uses.
2. **No `Fact::General` premises the native `type.argument-mismatch`,
   whatever its stratum.** That emitter resolves its argument to a concrete
   value and consumes only `Fact::Singleton`; a `bool` with no known value has
   nothing to resolve. Widening it to convict on a base-type envelope is a
   separate slice with its own FP surface, and is not taken here.

The consequence for the ruling is that it is correct and worth landing on its
own terms — it removes a marker that was never earned — but it is a
**precision** change on the dump surface, not a recall change, until a
consumer of abstract facts exists. Recorded so the next reader does not
mistake the flat gate for a failed measurement.

Fixtures: `crates/steins-infer/tests/comparison_value.rs` pins the split from
both sides so a refactor cannot collapse it in either direction —
`a_union_operand_decides_only_when_every_pair_agrees` (declared operands: the
decided verdict keeps `(asserted)`, the undecided one drops it),
`the_undecided_bool_is_verified_even_from_declared_operands` (the same two
operands, three dumps, one marker), and
`a_decided_comparison_over_declared_operands_stays_out_of_the_proof_lane`
(the Asserted decided verdict stays silent against a class-typed parameter
while its Verified twin convicts — the stratum, not the verdict, is the gate).

## Note (2026-08-09): the count guard clears `nullable` on both arms (issue #289) — ratified 2026-08-09

Recorded during this same ratification pass, and lifted the same day: the
2026-08-09 count note's "**A count guard does not clear `nullable`**" bullet
argued its conservatism backwards, and issue #289 corrects the argument and
the behavior together.

**The argument.** `count(null)` raises a `TypeError` under PHP 8.1+ — it does
not answer `false`, it does not fall through, it never reaches either side of
the `if`. So if a branch was taken **at all**, the subject provably was not
null on entry to `count()`: the exception reaches neither arm, and both arms
therefore prove non-null. This is the same §2 reachability shape as everything
else on this page, not an exception to it — the mistake in the original note
was reading a *raised exception* as if it were `array_key_exists`'s *false
answer*, and the two are opposites. `array_key_exists($k, null)` also raises a
`TypeError`, so `array_key_exists`'s existing conservatism (`Present` in
`refine_shape_fact`, `shape_arm_survives`) was never the right analogy either
— but that guard's true/false split narrows key presence, not nullness, so
its own conservatism is untouched here; only the *nullness* reading, and only
`count()`'s, moves.

**Where it lands.** Two sites, the fact lane and the arm lane, matching the
`Present`/`array_key_exists` precedent's own two-lane shape:

* `refine_shape_fact`'s `ShapeGuard::Count` arm now returns
  `nullable: false` unconditionally (was: `nullable` carried through
  unchanged) — the fact lane's own claim, for a binding already narrowed to
  one `Fact::Shape`.
* `shape_arm_survives`'s `ShapeGuard::Count` arm now kills the `Null` arm
  outright, before `to_shape_fact` is even consulted — the arm lane's claim,
  for a still-disjunctive contract (a declared `?array`, `?Countable`, or any
  union with a `Null` member). This is what makes the lift reach a
  `Countable` receiver identically to an array one: the argument is about
  `count()`'s own null case, not about arrays, and `shape_arm_survives`
  already treats a non-array arm (a `Class("Countable")` arm, say) as
  surviving the count comparison on its own — killing `Null` is orthogonal to
  that and applies whether the surviving arm is an array shape or not.

**The one place it does *not* reach: a catch that could have caught the
guard's own failure.** `try`/`catch` stays `StmtKind::Opaque` in this engine
(ADR-0027's one-construct-at-a-time ratchet — the trace does not model try's
control flow at all, so a catch body never inherits a preceding guard's
narrowed env; the whole construct forgets what it reads and writes on entry).
A `count($x) <op> N` guard therefore cannot narrow anything a
`catch (\TypeError $e)` body downstream can see, by construction — not
because of a special case added here, but because the surrounding
`try`/`catch` was never structured enough to carry the narrowing across the
boundary in the first place. `crates/steins-infer/tests/count_guards.rs`
pins this as a regression: a `count($x)` guard immediately followed by a
`catch (\TypeError)` that could plausibly have caught the guard's own call
failing must not see `$x` proven non-null.

**Direction of movement: both**, same as the parent note. Clearing `nullable`
removes null from what the branch admits, which can discharge a
`call.on-null`/non-object finding downstream (finding-removing) and can
equally let a definite verdict fire where the nullable arm was previously
absorbing it to `Maybe` (finding-adding) — exactly the class ADR-0052 §2
reserves for the verdict layer to decide, not this narrowing.

Fixtures: `crates/steins-infer/tests/count_guards.rs` — both arms clearing
`nullable` on a declared-nullable array, the `Countable` receiver reading
identically, the sealed/unsealed cases unaffected, an expression `count()`
cannot bind to a subject still doing nothing, and the catch-arm regression
above.

## Note (2026-08-18): the enum case set is a finite Verified domain, and an emptied Verified lane is readable (issue #429)

Completion of point 1 and an **amendment to point 2's emptied-carrier
rule**, recorded together because the second exists for the first.

**The domain.** A PHP enum is the one place the runtime-enforced type is
*finite*, so it is the one place an exhaustiveness question is answerable on
Verified premises alone. A case is an object with exactly one inhabitant, and
the value domain is object-free (ADR-0035/0038/0043), so the singleton has no
`Val` and no `Fact`. Point 1 already says where declared alternatives live,
and that is where it goes: `ContractTy::EnumCase { enum_fqn, case }` is one
arm, and a native `Suit $s` seeds one arm per declared case at `Verified`.
`normalize::Subtrahend::EnumCase` subtracts them, mirroring
`Subtrahend::Class`'s polarity pair.

Two judgments differ from the class subtrahend's, both because the subtrahend
is a single **value** rather than a class extent:

* **No finality question.** The positive branch deletes a `Class(M)` arm iff
  `is_a(E, M) = No`. Whether `M` has unseen descendants cannot change whether
  `E::C` is one of `M`'s instances, so point 2's `final`-gate — which exists
  because a subtrahend covering a class extent must reason about the whole
  extent — has nothing to gate here.
* **The positive branch narrows.** The 2026-08-01 note refused keep-only
  narrowing on the arm lane because "the value lane's `Refine::Exact` already
  owns that branch". For an enum case the value lane cannot own it — there is
  no `Val` to be exact about. Rather than add an intersection operator, the
  positive branch is spelled as the subtraction it is: `$s === Suit::Hearts`
  removes every value that is not `Suit::Hearts`. The lane stays what point 2
  built, a carrier every mutation removes provably-dead arms from.

**The amendment.** Point 2's last bullet says an emptied carrier "**drops to
no-fact** … never a death signal". The drop is what makes the finite domain
unusable: a chain that covers every case produces exactly the emptiness a
consumer needs to read, and dropping it makes that outcome indistinguishable
from a variable with no lane at all — which is the *absence* answer, and the
opposite claim. Amended: **an emptied lane whose every arm was `Verified` is
kept, empty**, and a new accessor reads it. A lane holding any `Asserted` arm
keeps the landed drop, because emptying a docblock's claim proves nothing.

The rest of the bullet is untouched and load-bearing: an empty lane is still
**not a death signal**. No branch is pruned by it, `eval_cond` is taught
nothing about enum identity, and the verdict keeps owning death. What the
empty lane buys is a consumer's *silence* — the direction that cannot
manufacture a finding.

**The absence discipline decides where the domain exists** (ADR-0049,
ADR-0002 outranking coverage). The case set is complete only when one
declaration can be read whole: a uniquely-resolved, unconditionally declared
enum in a file that parsed, with at least one case. Anything else keeps its
`Class` arm — which no identity guard can subtract to empty, so nothing
downstream can claim an exhaustion the declaration never proved. The guard
side asks the same gate as the seed, so a lane that was never expanded is
never subtracted from as though it had been. And only a `Verified` arm
expands: `@param Suit` over an untyped parameter is a claim, and a claim may
not mint a finite domain (ADR-0037).

**Rendering** stays on the point-4 cut's far side. The expansion is semantics;
`Suit` and `Suit::Hearts|Suit::Spades|Suit::Clubs` denote one set, so an enum
whose whole case set survives collapses back to the enum's name before it is
spelled, and only a narrowed domain shows its cases. An emptied domain dumps
as PHPStan's own `*NEVER*`.

**Deliberately not landed**, each its own question:

* **`match`/`switch`.** Value-position `match` is not analyzed at all, and
  statement-position structuring still refuses a class-constant arm outright,
  exactly as it did when the operand had no variant of its own — issues
  #430/#431 inherit this domain when they land.
* **Backed-enum `->value` / `->name`.** Narrowing is on case identity;
  the backing slot is a separate question and claims nothing today.
* **Loose `==`.** PHP decides `==` between two cases through their own
  `name`/`value` slots — a different question with a different proof.
* **The property carrier.** A declared enum-typed *property* has no arm lane:
  heap property slots hold a `Fact`, and a `Fact` cannot hold an object. The
  parameter and declared-return legs land; the property leg needs the
  object-graph extension ADR-0036 already queues.
* **Return through a body summary.** A callee that summarizes hands its caller
  a heap object, which has no arm lane to narrow; the declared-return floor is
  where this leg lands today.

Fixtures: `crates/steins-infer/tests/enum_case_domain.rs` (both directions,
the accumulating chain and the two chain outcomes, the backed/pure split, the
five absence shapes, the out-of-scope pins); `crates/steins-contract/src/`
`normalize.rs` (the arm's subsumption, both polarities of the subtrahend, the
no-finality rule, the unknown-hierarchy keep).

## Note (2026-08-18): the no-match path of a `match`/`switch` subtracts the arms (issue #439)

Completion of point 2, not new design: the subtraction machinery was already
built, and the `match` construct simply never called it. `walk_match` refined the
subject *inside* a conditional arm and refined **nothing** on the path reached
because every arm failed, so a `default` read the subject exactly as it arrived.
The negated-guard reasoning an `elseif` chain has done since ADR-0031 stopped at
the `match` keyword.

**The rule.** Reaching the no-match path — a `default` body, or the fall-through
of a `default`-less `switch` — means every arm was tried and every arm failed, so
the path carries the **conjunction** of the negated conditions. Each condition is
therefore subtracted on its own, and an arm mixing a subtractable literal with an
unrepresentable operand still contributes the literal. That is the mirror image of
the positive side, where an arm's conditions are a *disjunction* and one
unrepresentable operand voids the whole arm's refinement. Both ADR-0052 carriers
are subtracted, through the guard path's own machinery: the value lane via the
`NotNull`/`Exclude` refinements at the `Verified` stratum, the arm lane via
`Subtrahend::Null` / `Subtrahend::Value`, plus `Subtrahend::EnumCase` for an
`Enum::Case` arm condition — the one subtrahend the value lane cannot carry.

**`switch` subtracts the same set, and its residue is not evidence.** `switch`
compares loosely, so its no-match path proves `$s != c`, and `$s === c` implies
`$s == c`, so the failure of the loose test carries the failure of the strict one.
Subtracting the exact literal is sound — the same one-directional reading the
2026-08-18 `$x == null` carve-out (issue #391) already applies to the failing
branch of a loose comparison, one construct up. What does not carry over is the
converse: `case 0` also consumes `"0"`, `false` and `0.0`, and the loose-equal set
of a literal is infinite, so it has no finite subtrahend spelling. A `switch`'s
modelled residue is therefore an **over-approximation** where a `match`'s is
exact, and that decides what may be read off it. An *empty* residue still proves
emptiness — an over-approximation that is empty leaves nothing underneath — but a
*non-empty* one proves nothing, because what it still holds may be precisely what
a loose comparison consumed. So a `switch` subtraction narrows the lane (buying
silence, the direction that cannot manufacture a finding) and never sets the
proven-narrowing mark ADR-0088 §4 reads.

**A partially-landed chain claims nothing.** ADR-0088 §4's proven-narrowing rule
was designed against a single guard, where "a subtraction landed" and "this path's
narrowing is modelled" are the same statement. A `match` is a whole chain at once
and the mark is one bit, so a chain where some conditions landed and others did
not would set it and hand the consumer a residue that is ignorance about the arms
it could not model. Measured: `match ($b) { null => …, true => …, false => … }`
over a `?bool` kills the `null` arm and leaves the general `bool` arm standing
(point 2's interior-point rule — neither literal covers it), so the residue reads
`bool` on a chain that is in fact exhaustive. Amended, therefore: **the mark
survives a no-match subtraction only when every condition's subtraction landed.**
The narrowing itself is kept either way; it is only the claim that is withheld.
The same `?bool` shape spelled as an `if`/`elseif` chain reports today and is
untouched by this — it is the bool-literal gap ADR-0088 §4's note names, and it
closes when `Base::Bool` learns point 2's endpoint clip (its two-point domain is
the interval rule's one other finite base), not here.

**Deliberately not landed:** the class-constant arm condition still keeps the
whole construct opaque (`usable_operand`), so a `match`/`switch` over an enum is
not structured and the `EnumCase` subtrahend wired here has nothing to run on yet.
The refusal was lifted experimentally to check the wiring: an enum `match` covering
every case goes silent and one missing a case reports exactly the missing case, and
the 6670-file public corpus answers identically with the lift and without it,
across its 184 `case X::C:` labels and 463 `X::C =>` arms. It is still not this
slice's to land, and the reason is not caution about the subtraction. Structuring
turns on the rest of the construct's modelling too, and the idiomatic exhaustive
enum `match` has **no `default`** — so the day class-constant arms structure, every
one of them starts surfacing an `\UnhandledMatchError` it cannot throw. ADR-0088 §5
gates that origin on *Verified* coverage, which is the exhaustion check nothing has
built yet, so the lift needs that check beside it. Issue #431 owns the pair.

Fixtures: `crates/steins-infer/tests/match_no_match_subtraction.rs` (the
reproducer in both positions and its `if` twin, the `switch` pair, the loose
weakness, the two evidence refusals, the inexpressible condition, the arm-local
rebinding); `crates/steins-infer/tests/match_value_position.rs` (the residue
fixture this closes, and the two `assertNever` tripwires whose silence now comes
from an emptied domain rather than an untouched lane).
