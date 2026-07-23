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
     Yes; Maybe keeps it — the silence side). Null subtracts via the
     nullable bit as everywhere. Rejected: a bounded excluded-value set on
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
   - **`assert($expr)`**: **Asserted by default** — verified PHP
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

    N1 must land first (N4 consumes its API); N2–N6 are order-free after
    it, N3 before N4 preferred (guard calls give instanceof guards their
    call-bearing neighbors). Every slice states its ADR-0048 §2 argument
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
