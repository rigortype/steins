# Object/method world: native object acceptance over a trinary is-a oracle

Motivation, measured: 100% of the legacy monorepo's 405 phpdoc.* sites are method
calls (Transform #2 measurement), and 9 of the 18 triaged conformance
fails share one root cause — non-scalar types lower to `None` and are
never checked. Method call-site *resolution* already exists (all six
receiver forms, chain walk, final/private override guard, arg check,
descent); this milestone gives it types to check.

1. **Object types join `NativeType`**, not a parallel type:
   `TypeMember::Instance(fqn)` (namespace-resolved at lowering) sits
   beside the scalars, so `Foo|null`, `Foo|false`, `A|B` are one union
   shape and ADR-0042 failure arms extend to object-returning builtins
   unchanged. `self`/`static`/`parent` remain unlowered (silent) —
   late-static-binding is not v1.
2. **Enums become class-likes**: `ClassDecl { is_enum }` with cases as
   members; `ArgValue::EnumCase(class, case)` (an object value of the
   enum class) and `ArgValue::ClassConst(class, konst)` replace the
   `Other` erasure. Class-const *value* resolution imports only literal
   initializers in v1; anything else stays unproven.
3. **The is-a oracle is trinary** — Yes / No / Unknown — and **No
   requires a completely enumerated hierarchy**: every ancestor edge
   (parent chain + transitive `implements`) resolved inside the project
   or found in the catalog's builtin hierarchy, and the target absent
   from that closed set. A chain that leaves the project into an
   uncatalogued builtin, an ambiguous class, or a trait-using region
   yields Unknown (silent). This is the Certainty discipline applied to
   subtyping: membership is provable, non-membership only under closure.
   `instanceof` evaluation and catch matching move onto the same oracle;
   the existing exact-class heap (ADR-0036) supplies the value's class,
   and instanceof still binds no exactness fact (membership is not
   exactness).
4. **Acceptance**: `is_type_error` gains a definite-No arm — object
   value whose exact class is-a-No against every union member. The
   phpdoc side opens `contract_touches_class` on the same oracle. The
   four-layer domain stays object-free (ADR-0035/0038 extensionality);
   objects ride the CVal/heap path, as today.
5. **Gate discipline for the opening**: object acceptance can surface
   *true* positives on the corpus where the expectation is zero
   proof-layer diagnostics. Every new corpus finding is triaged verbatim
   (5-sample minimum per class); false positives are fixes, true
   positives are reported to the orchestrator with the triage before
   any expectation-table change. Silence-by-default (Unknown) is the
   safety valve at every stage boundary.
6. **Transforms unlock last**: method sweeps key on `Sym::Method`, the
   eligibility split reuses the `final_or_private` predicate
   (promotable) vs everything else (`method-inheritance` refusal per
   ADR-0041), and the legacy monorepo's 405-site re-measurement is the milestone's
   closing number.

Staging (each boundary gate-verified): (1) lowering only — enums,
`Instance` members, new `ArgValue` forms, zero behavior change; (2) the
is-a oracle + instanceof/catch migration; (3) native acceptance
definite-No + corpus triage; (4) phpdoc-side class contracts (tripwire
management); (5) transforms to methods + re-measurement.

## Amendment (2026-07-24): return-position `static`/`self`/`parent` — the minimum-bound check

§1's sentence — "`self`/`static`/`parent` remain unlowered (silent) —
late-static-binding is not v1" — covered two different problems with one
posture, and only one of them is hard. General LSB (resolving *which*
class `static` names at a use site: `new static`, `static::` dispatch,
`static` flowing through phpdoc into call-result types) needs
receiver-tracking machinery v1 does not have; that half stands. Return-
**position** checking needs none of it: a downward-closure lemma turns
the check into a plain is-a question the landed trinary oracle already
answers, so the ADR-0030 registry entry-3 deferral is discharged and the
position comes into v1 scope. Empirical anchor (PHP 8.5.8, `php -r`,
both modes — object return checks are strict/coercive-independent): a
`: static` method returning an object not is-a the *declaring* class
throws `TypeError` on **every** receiver, including the declaring class
itself; returning `new self()` from an open class throws only on
proper-descendant receivers.

1. **The minimum-bound lemma (the soundness core).** A method of class
   `C` declared `: static` must return an instance of the late-bound
   class `T` — the class the method was called on. Every possible `T`
   satisfies `is_a(T, C) = Yes` (only `C` and its descendants inherit
   the method). Is-a is transitive, so for a returned value of **exact**
   class `V`: `is_a(V, C) = No` implies `is_a(V, T) = No` for every
   valid `T` — a value that fails the declaring class fails every
   possible late-bound class. `C` is therefore a *necessary* bound, and
   violating it is a proven `TypeError` on every execution of that
   return, for every receiver: no assumption about which subclass is
   live is ever made, so the ADR-0002 worst-case refusal is not touched.
   The same argument covers `: self` (bound = `C` directly, not even
   late-bound) and `: parent` (bound = the resolved parent) — strictly
   easier cases of the same shape.

2. **The lowering (the only code change).** Return position only: a
   native return hint that is bare `self`/`static`/`parent` — or the
   `?`-nullable of one — lowers to an ordinary `TypeMember::Instance`
   of its **bound**: the enclosing class for `self`/`static`, the
   resolved `extends` parent for `parent` (no parent, or an
   unresolvable one, leaves the hint unlowered — silent). Concretely:
   `lower_method` records the keyword shape (it has no class context),
   and the FQN-stamping pass in `SyntaxTree::build` — which owns the
   enclosing class's resolved name — synthesizes `MethodDecl.ret`;
   `display` carries the source-cased resolved FQN, so the message
   renders the bound class (PHPStan message parity: "should return
   BrokenBuilder"). Everything else keeps §1's silence, each leg
   deliberate: the keywords inside a union type (`static|Foo` — legal
   PHP) stay unlowered v1; parameter and property positions stay
   unlowered (out of this amendment's scope, not blocked by it);
   closure `: static` stays unlowered (closures lower without class
   context); phpdoc `@return static` stays `ContractTy::Opaque`
   (ADR-0029 side unchanged — a contract-layer bound could ride the
   same lemma later, taken up on demand).

3. **The check is the existing check.** No new registry id, no new
   emitter: `type.return-mismatch` fires through the landed return-site
   pipeline, and every fact strength the lemma requires is already
   gated there:
   - **Exactness** — the object definite-No arm consumes only
     allocation-proven exact classes (`ArgValue::New` / `EnumCase` via
     `proven_object_class`); a membership-only class never reaches it.
     This is load-bearing, not incidental (the e0b0472 G1 discipline):
     for a value known only as lower bound `L`, the runtime class `R`
     may sit *between* `C` and `L` (`R` extends `C` extends `L`), so
     `is_a(L, C) = No` proves nothing about `R` — the No is licensed
     only by an exact class.
   - **`$this` is silent by construction** — `return $this` (the
     universal fluent shape) resolves to no exact allocation, and
     rightly so: `$this` *is* a late-bound instance and satisfies
     `: static` on every receiver.
   - **Oracle discipline** — the is-a `No` requires the completely
     enumerated hierarchy (§3); any open edge yields Unknown → silent.
   - **Stratum** — the native return check already requires all-
     Verified premises (ADR-0052 §5); an Asserted-derived value stays
     silent.
   - **Descent guard-blindness** — `object_world_guard_blind` applies
     unchanged; the check runs only in the plain per-scope pass.
   Scalar returns against the lowered bound come for free and are
   equally sound: `return 42` from `: static` is an unconditional
   `TypeError` in both modes (no scalar coerces to an object;
   `member_accepts_*` already rejects scalars against `Instance`
   members).

4. **The refusal, anchored — `new self()` under `: static` in an open
   class.** Verified above: it runs clean when called on the declaring
   class and breaks only on proper-descendant receivers — a
   works-but-worst-case shape, exactly what ADR-0002 refuses ("the
   program works" outranks the worst-case static reading). PHPStan
   reports it by worst-casing the declared type; Steins is silent *by
   construction*, not by special case: `is_a(C, C) = Yes`, and the
   check tests only the necessary bound (point 1). A contract-layer
   (`phpdoc.*`-style) reading was considered and refused: `: static`
   is a per-receiver *family* of contracts, and `new self()` satisfies
   the family member actually exercised on the declaring class — even
   "declared contract violation" over-claims. If demand materializes
   it is a policy-profile candidate (ADR-0050's named-stage lane),
   never core. No conformance case requires it: the suite's fixture
   class is `final`, where no conditional shape can exist.

5. **Sufficiency is never checked.** The lowered bound is
   necessary-only: Yes (a subclass instance, `$this`) and every
   Unknown are silent. A `: static` method returning an exact
   *sibling-branch* subclass instance breaks on some receivers and not
   others — the same conditional family as point 4, the same refusal.

6. **One-slice implementation, gates.** One slice: (a) steins-syntax —
   `MethodDecl` gains the recorded return-keyword shape (kind +
   nullability), captured in `lower_method` for bare/nullable
   `Hint::Static`/`Hint::Self_`/`Hint::Parent`; the `SyntaxTree::build`
   FQN-stamping loop synthesizes `ret` (parent leg consults
   `ClassDecl.parent`, skipping when absent); (b) steins-infer —
   **zero changes**; (c) tests pinning: the fixture shape (final class,
   `return new Unrelated()`) → one `type.return-mismatch` at the return
   line; `return $this` silent; `new self()` in an open `: static`
   class silent (the point-4 refusal); subclass return silent;
   open-hierarchy (uncatalogued parent on the returned class) silent;
   `?static` + `return null` silent; `static|Foo` union silent;
   `return 42` from `: static` reported; the honesty transform
   unmoved on `: static` methods (`decide_return` already skips
   `Instance`-bearing rets — the `has_instance` filter — so the
   transform sees exactly the pre-amendment `None` behavior). Gates:
   workspace tests, clippy `-Dwarnings`, fp-gate (proof-layer zero
   expectation stands; any monorepo hit is a §5 verbatim-triage — a
   true `: static`-vs-unrelated-class return is a genuine runtime
   fatality, reported to the orchestrator before any expectation-table
   change), conformance rerun (`objects_static_return_mismatch` closes:
   the harness tag group accepts the single return-site finding; zero
   flips elsewhere).

## Amendment (2026-10-03): class identity is resolved, not spelled — PENDING ratification

**Status: PENDING ratification.** Designed autonomously under the owner's standing
direction (issues #926 and #917, the default-surface false-positive sweep #963).

§3's oracle compared two class names as spellings: `Real` is-a `Alias` was `No`
whenever `Alias` was not literally on `Real`'s supertype closure. Two names for one class
reach the walk from two places, and both were proven false positives under `strict_types`
and in `throw.undeclared` (PHP accepts every one of them):

- a project's `class_alias(Real::class, 'Alias')`, which the index already folded for
  existence (the alias name resolves to the target's declaration site) but the is-a walk
  never consulted; and
- php-src's one class-level stub `@alias`: `DOMException` is also `Dom\DOMException`, and
  `ReflectionClass::getName()` answers `DOMException` for either.

**The rule.** Names meet as **identities**. `Cx::class_identity(name)` is the project
declaration's own lowercase FQN when `find_class` answers `Unique` (an alias edge shares
its target's site, so alias and target answer one declaration); else, for a name no
project file declares, the declared name of the catalog class it is a second name of
(`steins_catalog::builtin_class_alias`); else the name itself. `supertype_walk` compares
the identity of every visited node with the identity of the target, so the reflexive
`Yes`, the `seen` set and the `Stringable` special case all speak identity. An ambiguous
name (two declarations, nothing to pick) stays its own identity: a guess may widen a `Yes`
and never a `No`, and the walk already reads an ambiguous ancestor as incomplete.

**The alias fold is a fixpoint.** The index mints `class_alias` edges in rounds: each round
resolves every pending edge against the snapshot it began with and mints the round
together, so `class_alias(Real::class, 'A1'); class_alias('A1', 'A2');` resolves whatever
order the calls are written in, and a result is a fact about the multiset of edges, never
about visit order (ADR-0048). An edge whose target is ambiguous is dropped (ambiguity only
grows); one whose target is absent waits; a cycle no declaration stands under mints nothing
and ends because every round resolves an edge or stops. The affected-set declaration table
repeats its alias pass to the same fixpoint, so a file naming the end of a chain still
reaches the declaring file in one hop.

**An unseen target declines.** A walk that would answer `No` for a target resolved in
neither the project (ambiguous names included) nor the catalog, while the dam is not clear
(a dynamic `class_alias`, an `eval`, an unprovable include), answers `Unknown`: the
runtime-minted class might be the target, and might be the class the walk enumerated. A
known class's mismatch still proves, since an alias renames nothing that exists, and in a
clear universe a name nothing declares stays a proven `No`, `class.undefined`'s case.

**The catalog learns aliases.** `extract_hierarchy.py` reads a class-level `/** @alias X */`
docblock (the docblock directly above the declaration; method- and function-level `@alias`
tags are other things) and emits `aliases = [...]` on the declared class's row; at
`php-8.5.11` that is exactly `Dom\DOMException` on `DOMException`. `gen-catalog` renders
`class_aliases_generated.rs`, a second name is no row of the hierarchy or display tables, and
`builtin_class_supers` and `builtin_class_display` answer through `builtin_class_alias`, so
no name has two answers. The sidecar's `ReflectedClass.name` as a second source, for classes
the pinned stubs do not declare (PECL), is a follow-up.

Witnessed on PHP 8.5.11: `takes(\Dom\DOMException $e)` accepts `new \DOMException`; a
`catch (\Dom\DOMException)` catches a thrown `\DOMException` and the converse; `is_a(new
DOMException, 'Dom\DOMException')` is true; an alias passed where its target (or another
alias of it) is declared is accepted in strict mode, for classes, interfaces and enums.
Controls that stay findings: an unrelated class against a param typed with an alias of
`Real`, an alias of `Real` against a param typed `Other`, and, under a dynamic
`class_alias`, a known class's mismatch (while a param typed with an unknown name declines).
