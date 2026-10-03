# Exhaustiveness has a floor: the maybe- siblings of the envelope checks, and destructors

**Status: proposed (2026-10-02), PENDING ratification.** Designed by the
architect and decided under the owner's standing delegation (issue #915, run 2
of #895; owner decision D4). The floor (§1–§6) lands with slice S1; the
destructor design (§7) is decided here and lands in S6. Decision **O1** (§5) is
recorded as a decision, not left implicit. Tracking issues #915 and #800; the
destructor hole is #882.

ADR-0099 gave every body's `…?` a set of recorded reasons (§5) and refused to
discharge any of them with a new policy, because "no consumer of exhaustiveness
has a floor" (§5.2). This ADR is the floor: two ids that read the gaps behind
the two envelope checks and report them at `strict`, so that "declared, but the
body runs something the analyzer cannot see" has somewhere to surface instead
of being the family's silent end state.

## 1. Context

`effect.envelope-exceeded` judges a body against its declared envelope with
what inference **proved**. A site the lane cannot resolve contributes a gap, no
label, and so no finding: `{…?}` is not an occurrence (ADR-0067 §1,
ADR-0099 §5). `throw.undeclared` is the same shape: only a `Yes` escape of a
checked class reports, and a `Maybe` is silent (ADR-0040 §1, "the safe side
inverts by consumer"). That silence is right for the default floor, where
zero-FP is the calibration. It is wrong as the *end state*: no profile can ask
the question "which of my declared envelopes could Steins not verify, and why".
The glossary's **maybe- sibling** names the pattern (`offset.missing` /
`offset.maybe-missing` is the precedent): the possibly-grade twin of a definite
id, at the `strict` floor, rather than the possibly-leg scoped out of existence.
Issue #800 asked for the effect half under a working name; this ADR spells it
by the glossary rule and adds its throw twin.

What the public corpus looks like before this ADR (master c7f4b2af, `--no-cache
--no-php`): 28,847 functions and methods, 5,899 effect-exhaustive and 6,534
throw-exhaustive; no effect envelope anywhere; of 1,306 `@throws`
declarations, 1,058 (81%) are throw-non-exhaustive.

## 2. Decision: two ids

1. **`effect.maybe-envelope-exceeded`** and **`throw.maybe-undeclared`**, each
   `(Layer::Contract, Floor::Strict)`. Contract layer because their definite
   twins are (the claim is about what the code's own declaration says); strict
   because the claim is a gap, never a proof. The definite twins keep
   `Floor::Contracts`, so a `contracts` run keeps its meaning. Neither carries a
   facet (`origin` stays `throw.undeclared`'s alone, ADR-0050 §4).
2. **Spelling.** The glossary's rule, which supersedes #800's working name
   `effect.envelope-maybe-exceeded`: the `maybe-` prefix goes on the *rule
   name*.
3. **Units** are the declarations the definite checks already judge, less the
   ⊤ envelope:
   - effect lane: every declaration for which `operative_bound` builds a bound
     (an attribute, or an interop tag that bounds something; a class-level
     `@phpstan-all-methods-pure` makes each method a unit). A bare
     `@phpstan-impure` and a bare `@phpstan-all-methods-impure` bound nothing
     and are no unit.
   - throw lane: every function and method whose docblock declares `@throws`
     (`declared_throws` non-empty). A declared set that names `\Throwable` is ⊤
     and is no unit.
   Closures are never units (no envelope); a gap in one surfaces at the edge
   that reaches it (§3.2).
4. **Nothing is fed back.** The exhaustiveness bit, the fixpoints, the writers
   (`effects-envelope`, `throws-envelope`, `loop-to-array-map`) and
   `effect-diff` read the same gaps as before and do not move. `annotate`'s `…?`
   marker and gap kinds are unchanged too; its text margin lists every emitted id
   regardless of profile, so the two ids appear there as the other strict ids do
   (its JSON carries no finding ids). The floor is two reporting loops over the resolution those lanes already
   compute.

## 3. Decision: granularity

1. **Direct: one finding per (own site, gap kind).** For each site of the unit,
   the same `resolve_site` the lane folds into its row; each `GapKind` in the
   site's `gaps` that no discharge answers (§4) is one finding, anchored at the
   site and naming the kind by `GapKind::as_str` (`dynamic-callee`,
   `declared-receiver`, …) with a clause saying what it means
   (`GapKind::reason`). A site with two kinds (`array_keys(...$a)` is an
   argument-list gap and an iteration gap) is two findings; two sites of one
   kind are two findings. This is the granularity a reader can act on: the
   finding names the line to fix or to declare.
2. **Inherited: one finding per call edge.** For each edge a site draws into a
   project body whose own set is `…?` (`EffectSet.gaps`, `ThrowSet.gaps`),
   unless discharge 4 or 6 answers it, one finding at the site, naming the callee
   and the callee's gap kinds (the inherited mask). An untainting edge (ADR-0063:
   a conditional-purity contract decided in full at the call site) draws none,
   because its `…?` does not propagate. A closure reached as a callback is a
   project body with no envelope, so it is reported at its edge.
3. **What is reported where.** One finding per (unit, own site, kind) and per
   (unit, edge). A cause behind an enveloped callee is reported there and at no
   caller (§4.4). A cause behind unenveloped bodies is reported at every unit
   that reaches it through them, once per unit. A unit in a recursion with an
   unenveloped helper reports its own site and the edge. One finding per unit
   would hide the site, and a `@steins-ignore` placed on the declaration would
   silence sites added later; one per transitive origin would multiply one cause
   across every caller, where this form reports it once per unit.

## 4. Decision: six discharges, and what is deliberately not discharged

A finding is not reported when:

1. **A ⊤ envelope.** Not a unit (§2.3).
2. **A call through a parameter flagged `@pure-unless-callable-is-impure`**
   (effect lane only). `$f()` where `$f` is flagged by that tag (or its
   `@phpstan-` spelling): the contract says the function is pure unless the
   bound callable is impure, and the call sites decide it (ADR-0063's
   untainting edge). All of these hold: (a) the parameter is flagged; (b) the
   unit is a free function (the tag is honoured on free functions only, per
   ADR-0063's amendment, so a method is a unit with no discharge 2); (c) the
   parameter is by-value, non-variadic, never written in the frame and never
   taken by reference by a named call of it; (d) it has no default
   (`function f($f = 'impure_fn')` with `f()` fills the slot with a callable no
   call site decided). The syntax layer decides (c)'s first half and carries the
   name on the site (`DynamicSite::Call { var }`; trace payload, past the
   analyzer gate, so there is no `SCHEMA_VERSION` bump); `Frame::rebound_by_call`
   decides the second. The typed spellings `pure-callable`, `pure-closure` and
   `static-pure-closure` are **not** discharged: the call-site obligation check
   for them proves impurity only of a closure or a first-class callable, so a
   string or array callable bound to the parameter is never decided by the type
   (follow-up #957). A purity contract says nothing about what is thrown, so the
   throw lane takes no discharge from it. The discharge answers the unit's own
   `$f()` site only; a caller reaches it through discharge 4's conditions.
3. **An interop envelope whose imported bound fits** (effect lane). An interop
   envelope (ADR-0082) answered a declared-receiver call: its labels entered the
   declared lane and the answer stayed open, as an unchecked claim must
   (`GapKind::InteropEnvelope`). When every label the site imported fits the
   declaration's own envelope (`!exceeds`, under the project's tolerance
   policy), or the import is empty (`@phpstan-pure`), the claim cannot break
   this envelope and the finding is discharged. A bound that does not fit stays a
   finding of kind `interop-envelope`.
4. **An edge to an enveloped callee** (a callee's discharges are relative to its
   envelope; the caller inherits them only through an envelope that fits). The
   callee is itself a unit and reports its gaps once, itself. "Enveloped" means a
   non-⊤ envelope on that lane: a `@phpstan-impure` callee, or one declaring
   `@throws \Throwable`, bounds nothing and is still reported at its callers'
   edges. In the effect lane two more conditions hold: (f) every label of the
   callee's operative bound fits the caller's (`!exceeds`); otherwise the finding
   lands at the call, naming the envelope it does not admit (a callee bounded by
   `#[\Steins\Effect('io.db')]` under a caller declared `#[\Steins\Pure]`); and
   (g) the edge is not a tainting edge into a tag-flagged free function: the
   function's purity is conditional on the callable bound at the call, and a call
   that does not decide it (`f($c)` with `$c` the caller's own parameter) leaves
   the contract undecided, so the finding lands at the caller's call site and
   names it. An untainting edge (a visible callback decides the contract) draws
   nothing, as for any callee. The throw lane has no fit relation: whether a
   callee's `@throws` fits a caller's is #958, not a floor discharge. A known
   conservative imprecision: a flagged function that forwards its own flagged
   parameter to another flagged function (`g(callable $c) { return f($c); }`, both
   flagged) is reported at the forwarding call, because that call does not decide
   the callable either, though its own caller's would.
5. **A site guarded by a catch that absorbs `Throwable`** (throw lane). Some
   clause of some enclosing guard names `\Throwable` itself (resolved to its
   FQN, so `catch (Throwable)` in a namespace with no import is a different
   class and discharges nothing). The whole site is discharged, its edges
   included: whatever it throws is caught there. A narrower catch discharges
   nothing, since the unknown may be outside it. A catch bounds what is thrown,
   never what is done, so the effect lane takes no discharge from it. (This
   completes ADR-0099 §5.2's "a gap under a provably absorbing `catch
   (\Throwable)` is not dammed yet" for the reporting side only; the throw
   set's exhaustiveness bit is unchanged.)

6. **The project's tolerance policy** (effect lane; ADR-0084 §3's attribution).
   Two halves, both keyed as the definite check keys them: by the body that
   *runs*, the resolved `Sym::Method`, under `policy.method_attribution(class,
   method)` with the class that declares it and no inheritance.
   - An edge into a callee whose `EffectSet.attribution` holds a label the policy
     tolerates is discharged (the predicate `finding_groups` uses).
   - The `declared-receiver` and `interop-envelope` gaps of a method call on a
     declared receiver are discharged only where dispatch is exact: the declared
     class is final, or the resolved method is final or declared in a final class.
     The attribution is then read on the class that declares the resolved
     method. A declared receiver is an abstraction, so on a non-final class, an
     interface, or a `$this->log` typed to either, a subclass or an
     implementation may run a body the policy does not attribute (`Logger` is
     attributed `telemetry` while `LoudLogger::info` echoes), and the gaps stay.
   A tolerance is the project's calibrated floor; reporting beneath it would make
   the strict id a second, uncalibrated policy surface.

**Deliberately not discharged:** every other kind, `NoEffectRow` and
`NoThrowRow` included. A catalog name with no row is the analyzer's own
coverage hole; reading it as covered is the unsafe default ADR-0099 §3 removed,
and it is exactly the finding a strict reader wants to see, because it is
fixable (a row, an audit, #881).

Known gap, recorded and not masked (witness `t7_absorbing_callee`): an inherited
throw edge is reported into a callee whose only gaps lie under that callee's own
`catch (\Throwable)`. Discharge 5 reads the guards at the *unit's* sites, not the
gaps the callee already dammed, because the callee's throw set carries the gap
mask without the guard that absorbed it. Measured over the public corpus, 0 of
880 inherited findings are of this form; the fix is #956.

## 5. Decision O1: emit `throw.maybe-undeclared` now

The throw sibling is loud: on the public corpus (ten packages, measured on the
slice's branch; every figure in this paragraph counts vendor paths, the gate's
basis, and without them the same run reports 7,938: 7,058 direct and 880
inherited) it reports 8,202 findings, 7,313 direct and 889 inherited, against 149
findings of both throw ids before (the fp-gate's `phpstan-src` row is measured
separately); per package, 4 (nikic/PHP-Parser) to 3,374 (composer/composer). By direct kind the volume is dynamic callees
(2,320), operand reach at operators (2,378), declared receivers (737),
user-code reach at builtins (711), unknown classes (397), non-final `$this`
(359) and catalog names with no throw row (349). It fires on about 81% of
`@throws` units at `strict`.

**It is emitted anyway.** The zero-FP rule is calibrated defaults, not omitted
checks (`feedback-zero-fp-calibrated-defaults`, the invariant that every cause
of unsoundness is detected and tolerance is absorbed by plugins, ignores and
strict-tier floors): the volume is the measurement of how often an author's
`@throws` is unverifiable today, and the strict floor is where that is allowed
to be loud. It changes nothing at the default or `contracts` floor. The cost is
absorbed where ADR-0084's tolerance and the plugin lane already live: a plugin
that declares what a dynamic call provides, `@steins-ignore` with a reason, the
baseline. The fp-gate's `THROW_EXPECTED` is reseeded to the measured counts
(`xtask/fp-gate/throw_expected.toml`, with the triage by kind).

**Writers.** `effects-envelope` writes only from an exhaustive summary (ADR-0082 §7), so a tag it writes draws no effect sibling (measured: 0 over 723 tags across the public corpora). `throws-envelope` writes the escapes it *proved*, in lockstep with `throw.undeclared`'s fire set (`proven && !covered`, whether or not the body is `…?`; ADR-0040 §1, ADR-0037), and `throw.undeclared` itself reads certainty, never the exhaustiveness bit. On the declaring side that is also PHPStan's reading: `missingCheckedExceptionInThrows` skips implicit throw points. But a callee's `@throws` is consumed as its throw type at PHPStan call sites (absent, the call is an implicit `Throwable`), so on a `…?` body the written tag is a verified lower bound the consumer reads as an upper one. The floor names the difference, at `strict` only: a tag written on a `…?` body draws `throw.maybe-undeclared` for the body's gaps. Measured on scratch copies of the ten public packages (these three counts are non-vendor, the `check` basis): 1,934 tags; PHP-Parser 4 → 276, monolog 174 → 936, composer 3,170 → 13,595; all 11 tags on symfony/process land on gapped units. The writer is unchanged here; whether it should refuse a `…?` body, write and report the gap in its own report, or keep writing, is #921, decided after S2–S8 have narrowed the gaps the count depends on.

The slice's stated acceptance, "zero sibling findings on written tags", holds for `effects-envelope` and was wrong for `throws-envelope`, for the reason above.

**Known gaps.** #922: the fp-gate now routes by posture (every strict-floor id leaves its family table for the possibly-grade table, `xtask/src/gate.rs`), and the remainder is a per-id split of that one table. #923: a `Maybe`-certainty escape that carries no gap kind is not reported by the floor. #956: an inherited edge into a body whose only gaps are under its own `catch (\Throwable)` (§4). #959: an untainting edge into a flagged free function drops that callee's unrelated gaps from the exhaustiveness bit (pre-existing; the floor skips untainting edges and so inherits it). #957: typed `pure-callable` spellings. #958: the throw lane's fit question.

## 6. What this does not change

The `effect.liskov-widened`, `throw.liskov-widened` and every proof-layer id;
the exhaustiveness bit and the gap kinds' codec numbering; `annotate`'s `…?`
marker and gap kinds (its text margin lists every emitted id regardless of
profile, so the two ids appear there as the other strict ids do; its JSON carries
no finding ids); the effect
baseline and `effect-diff`; the three transforms' plans;
`SCHEMA_VERSION`. `vendor_suppressed` in `check`'s summary moves: the count is tallied before the profile surface (ADR-0015 order, `crates/steins-cli/src/check.rs`), a pre-existing overstatement every strict-only id already causes; #920.

## 7. Destructors: decided here, lands in S6

ADR-0099 §7.1 left the destructor hole open for want of a floor to calibrate a
per-site gap against. The floor now exists, so the design is decided here and
staged, and its implementation is slice S6 of #915 (issue #882).

1. **Stage 1: an exact edge.** An exact `new Foo` whose closed chain declares
   `__destruct` gets an edge to it, in both lanes (the exact-class rule of
   ADR-0099 §4.3, applied to the destructor). Sound for the exact case, and
   cheap: 20 corpus files declare one. No schema change.
2. **Stage 2: a gap kind.** `GapKind::Destructor` is appended to
   `GapKind::ALL` (the codec numbers by position, so the existing numbers stay).
   It applies to values of a *bound* class whose chain, or an in-universe
   subclass, declares `__destruct`; trait users and anonymous subclasses count,
   as §4.4 of ADR-0099 counts them. The gate needs a set beside
   `magic_property_classes` in the package shard (`steins-db`'s `shard.rs`), so
   **`SCHEMA_VERSION` goes from 23 to 24**, with a row in
   `docs/internal-spec/generation-schema.md`. The forms that run a destructor
   (`unset`, reassignment, scope exit, a bound parameter set to `null`,
   `array_splice`) are sites of the new kind.
3. **A universe-wide gate is refused as a per-body gap.** "Some class declares
   `__destruct`" is on for 8 of the 10 public packages, which would make
   exhaustiveness vanish. The residue, a value of an unknown class, is recorded
   under ADR-0099 §7.1, which is then closed except for it.
4. **The floor reads it.** The new kind is a gap like any other: the sibling
   ids of §2 report it at `strict`, with no further decision.

## 8. Considered and refused

- **One id covering both lanes.** The lanes' units, discharges and callee
  envelopes differ, and an id is what `@steins-ignore`, profiles and the
  baseline address. The twins the definite ids already are keep the pairing.
- **A `Pedantic` floor.** `pedantic` branches off `contracts` and holds
  house-style asks; these are not style, they are unverified claims, so
  `strict`.
- **Discharging `NoEffectRow` / `NoThrowRow`.** §4.
- **A wall-clock or count cutoff on the findings.** Cutoffs are structural and
  name themselves (the invariant); the volume is calibrated, not cut.
- **Reporting every transitive origin.** §3.3.

## 9. Relation to earlier decisions

- **ADR-0050**: two registry rows at the `Strict` floor; a one-line dated note
  there lists the ids.
- **ADR-0067**: the declared lane stays proven-only for the definite ids; the
  floor reads the gap an interop envelope leaves (§4.3) and an interface
  envelope's discharge of the call site is unchanged.
- **ADR-0099 §5.2** ("no consumer of exhaustiveness has a floor") is answered by
  this ADR for the two reporting consumers; §5.2's refusal to discharge a gap
  in the exhaustiveness bit itself stands. §7.1 is answered by §7 above.
- **ADR-0063**: its discharges (an untainting edge, a checked envelope at a
  call) keep their reach; §4.2 and §4.4 are the reporting side's reading of
  them.
- **ADR-0084**: the project's tolerance policy applies to the §4.3 fit test as
  it does to the definite check, through the same bound.

## Amendment (2026-10-03): §7's exact edge at `new` is refused, and the gap sits at the drop (#882) — PENDING ratification

Amends §7 (destructors), found while designing its implementation, slice S6 of
#915.

1. **The exact edge at `new` is refused.** §7.1's stage 1 puts an edge to
   `__destruct` at an exact `new Foo` whose closed chain declares one. A witness
   against PHP 8.5 refutes the anchor: `function d(): D { return new D; }` prints
   nothing itself, and the destructor runs in the caller, when the caller drops
   what it was handed. An edge at the `new` proves `io.output.buffer` on a
   factory, which is a false `effect.envelope-exceeded` on any enveloped factory
   and a wrong tag for `effects-envelope`. An edge at a drop is provable only
   for a local that never escapes (not passed, stored, returned, captured or
   referenced), an escape analysis the syntax layer does not have. The same
   witness set shows a parameter's drop is a may-run, not an edge:
   `d(D $d) { $d = null; }` runs the destructor when the caller handed it a
   temporary and not when the caller keeps a reference.
2. **One mechanism, a gap at the drop.** §7.2's `GapKind::Destructor` replaces
   stage 1 for both subjects: a body-local `new` binding and a parameter bound
   to a class whose chain, or some in-universe subclass, declares `__destruct`
   (trait users and anonymous subclasses count, §7.2). The sites are the
   forms that drop a value: `unset`, a reassignment, the end of the scope, and a
   `new` temporary that is dropped in the statement that makes it. No drop is an
   edge. A statement-position `new D;` is a may-run too, and three witnesses on
   PHP 8.5 defer or cancel its destructor: a constructor that stores `$this` in a
   static keeps the object alive past the statement, so `__destruct` runs later,
   in the caller; an object that references itself is freed by the garbage
   collector, not at the statement; and a constructor that throws never runs the
   destructor at all. An edge there would prove `io.output.buffer` on a path that
   may not run it, the same false positive as the factory. The exact edge for a
   non-escaping local is a follow-up whose escape rule must count the
   constructor, every method call on the object (either can leak `$this`) and a
   cycle (`$d->self = $d; unset($d)` runs at collection) as escapes, besides
   assignment targets, call arguments, `return`, `yield`, captures and
   references.
3. **`array_splice` is not a form.** §7.2 lists it, and it is cut: an array
   operand carries no element class, so by the per-value rule an element dropped
   through `array_splice` is §7.3's recorded residue (a value of an unknown
   class), not a site.
4. **The gate and the sites land apart.** The gate lands first: `destructor_classes`
   in the package shard, `GapKind::Destructor` appended to `GapKind::ALL`, and the
   schema bump of §7.2 (#882), with no site producing the kind, so findings are
   byte-identical. The sites land after it, in a second slice, which also reseeds
   the fp-gate's possibly-expected table by kind as the earlier floor slices did.
5. **The gate is a lower bound.** "The dropped value's class declares
   `__destruct`" under-approximates "this drop runs user code": a final class with
   no destructor whose property holds a `D` runs `D::__destruct` when it is
   dropped. The sound, cheap extension is the typed-property hop. Define
   `reaches_destructor(C)` as holding when (i) `C`'s chain, or an in-universe
   subclass of `C` (`destructor_classes` and the anonymous subclasses' parents,
   as ADR-0099 §4.4 reads them), declares `__destruct`; or (ii) a non-static
   property declared on `C`'s chain has a hint naming a project class `P` with
   `reaches_destructor(P)`, recursively and with a seen set, scalar and `null`
   members ignored. A hint member that is `array`, `mixed`, `object`, `iterable`,
   `callable`, absent, or an engine class (`Closure`, `Generator`, `Fiber`,
   `stdClass`, ...) contributes nothing. A gate that fired on any class with an
   object-capable property would fire on every untyped, `array` or `mixed`
   property, most of the public corpora, which is the universe gate §7.3 refused.
   The shard set of this amendment's gate is clause (i); clause (ii) reads the
   class's property hints and lands with the sites in S6b, not with the gate.
   §7.3's residue, "a value of an unknown class", is rewritten as a list, each
   entry a drop that runs user code with no gap and each witnessed on PHP 8.5:
   - a value of an unknown class (untyped, `mixed`, a call result);
   - an array, local or parameter, whatever it holds;
   - a property of a known class that is untyped or hinted
     `array`/`mixed`/`object`/`iterable`;
   - a subclass's own properties when the subject is bound (a `private D $d` on a
     subclass, dropped through its parent's type);
   - a closure's captures;
   - a suspended generator's or fiber's `finally`;
   - dynamic properties and engine containers (`stdClass`, `SplObjectStorage`,
     `WeakMap`);
   - builtins that drop elements (`array_splice`).
   The strict floor's `destructor` findings are therefore a lower bound on the
   drops that run user code, and this ADR says so.

## Amendment (2026-10-03): the drop sites land, and what landing them settled (#882) — PENDING ratification

Slice S6b of #915 lands the sites the amendment above decided; the gate (`destructor_classes`, the `destructor` kind, schema 24) landed with S6a. The rules are the amendment's. This note records what the implementation fixed that the amendment left open, and the witnesses that decided it (PHP 8.5; rows 6.1 to 6.26 of the S6 revision on #915 are tests in `operator_rules.rs`).

1. **The sites.** A `Drop` operator family with four forms: `unset($v)`, an assignment over `$v`, the end of the frame's body (one site per subject, at its last byte), and a `new` whose value dies in the expression holding it (a statement, a method call's receiver, a call's argument). A `new` that is assigned, returned, stored, yielded, captured or put in an array literal is no site. The subjects are a local whose first write in source order is `new C` (it remembers every `new` class it is written with, an anonymous class's body read by the syntax layer: a destructor or a trait use is user code at the drop, a parent is the class), and a by-value, non-variadic, non-promoted parameter whose native hint names a class, read by the hint though the frame writes it. Both lanes read the one resolution, so an enveloped function that drops such a value is `…?` in both, and `effects-envelope` refuses to tag it.
2. **The first write releases nothing, unless it runs again.** `for ($i…) { $d = new D($i); }` and a `goto` back over the write drop the previous value at the assignment (witnessed: `<it0>[D0]<it1>`), so a first write is no reassignment only outside a loop and a function with no `goto`.
3. **`reaches_destructor` as built.** Clause (i) reads the chain through the shard's `destructor_classes` (which counts a trait user, as the amendment says) and, for a class that is only a bound, an index-wide closure built once on first use (`Index::destructor_ancestors`): the ancestors, interfaces included, of every class whose chain declares one, of the shard's table and of an anonymous class's parent, so a subclass test is one lookup and an interface a subclass implements is reached (witnessed: `final class C extends P implements I`, `function g(I $i) { $i = null; }` runs `P::__destruct`). An exact class (`new C`, a final class) asks its own chain only. A class declared twice cannot be read, may be the declaration that has the destructor, and counts; a name no file declares is residue. Clause (ii) walks `class_props` (inherited properties, not a subclass's own), each hinted class once per exactness it is asked under (a `?Node` property on an exact `new Node` holds a bound `Node`, whose subclass may declare one), a union or intersection member by member. A parameter's hint is read syntactically, not from its lowered type, because the lowered type of `array|D` and of `self` is none: every class member counts whatever else the union holds, and `self` and `parent` name the enclosing class and its parent, as bounds.
4. **Further residue, recorded.** (A class whose parent no file declares was residue here; the amendment below counts it.) `new self`, `new static` and `new $c` name no class; a `foreach`, `catch` or `list()` binding over a subject, `??=`, and a by-reference or variadic parameter are not forms; code outside a function-like body (a script's top level) carries no sites; a property hinted `array|D` or `self` lowers to no hint, so the hop does not read it. Three shapes were left to a follow-up slice, each witnessed on PHP 8.5 and each a drop that runs user code with no gap (all three are closed by the amendment of S6d below):
   - a `new` that reaches a local through anything but its first write: `$x = null; $x = new D();`, a ternary or `match` arm (`$x = $c ? new D() : null;`), an untyped parameter overwritten with one (`function a($x = null) { $x = new D(); }`), each `<body>[D]` at the scope exit;
   - a `new` temporary in a position the three forms do not name: `clone new D()`, `(new D())->p`, `echo new D()`, `new D() instanceof D` (`[D]<body>` for the last, `s[D]<body>` for `echo`);
   - overwriting, unsetting or throwing through a typed or static property whose hint reaches a destructor: `private ?D $d; … $this->d = null;` and `self::$d = null` run `[D]` before the next statement, and a destructor that throws surfaces at the assignment (`<caught: from destructor>`), the usual close-and-reset pattern.
5. **The volume is the gate's, and it is large where the gate is conservative.** On the ten public packages the first measurement marked 3,823 sites (2,903 scope exits, 578 temporaries, 323 reassignments, 19 `unset`s); seeding the ancestors from every inheriting class and reading `array|D` and `self` hints added a few (85 functions lose exhaustiveness where 81 did). Roughly 58% reach through a class that imports a trait (the shard table counts one whether or not its trait declares `__destruct`), 21% through an anonymous class's parent (the table lists the parent whatever the anonymous body declares), 21% through a declared destructor or a property that holds one (monolog's `Handler`, symfony's `Process`). Two refinements would take the first two away without touching the rule, and each moves the shard's table, so each is a `SCHEMA_VERSION` bump of its own: record, per class, whether a trait it imports is a project trait that declares `__destruct` (a trait's body is not lowered today), and list an anonymous class's parent only when its body declares one or imports a trait. Both landed in S6c, in the amendment below.
6. **Warm runs.** The typed-property hop reads the property's class in another file; the affected set's call-graph leg reaches that file through the class names the declarations carry, to `MAX_BINDING_DEPTH` hops, as it does for every other cross-file class read. A hop deeper than the bound is the same open edge the bound leaves everywhere.

## Amendment (2026-10-03): the gate's two proxies become readings, and a name nothing declares may declare one (#882, S6c) — PENDING ratification

The first measurement of the drop sites (3,823 sites on the ten public packages, the note above) showed what the gate was made of: 58% through a class that imports a trait whatever the trait declares, 21% through an anonymous class's parent whatever the anonymous body declares, and the 21% that is the rule working. Neither proxy protects anything the syntax layer cannot read. S6c replaces both with readings, and closes the chain at the one place it was left open. One `SCHEMA_VERSION` bump, 24 to 25.

1. **An anonymous class counts for what it reaches.** Its parent and interfaces join the destructor closure whenever the anonymous class itself reaches a destructor: its parent's chain does, its body declares `__destruct` or aliases an imported method as one, it imports a trait that counts (item 2), or a non-static property of it, a promoted parameter or a trait's, is hinted with a class that reaches one. An anonymous class that reaches none adds nothing to its parent (the proxy "whatever the body declares" is gone, not the reading of the body). The first form of this item took the opposite for any clean-bodied class, and was wrong for an anonymous class whose *parent* declares one or holds one (`new class extends P implements I {}` with `P::__destruct`: a bound `I` runs `[P]`), which the review of #1006 witnessed; the closure is now a fixpoint (item 5). The shard records each anonymous class with a parent or interfaces (`anonymous_classes`: parent, interfaces, own destructor, imports, held classes) and the engine reads them at the drop; `anonymous_subclass_parents` is unchanged, because §4.4's property gate of ADR-0099 still needs every anonymous subclass. A local bound to an anonymous class is read the same way: the trait names it imports are its receivers, beside its parent, and a trait answers as a class does.
2. **A class imports a trait into the closure only when the trait counts.** A trait counts when it declares `__destruct` (read by member name: no trait body is lowered into a unit), when a `use T { x as __destruct; }` adaptation names one (witnessed on PHP 8.5: `[bye]<after>`, at the class level and in a trait that imports it), when it imports a trait that counts, or when it **cannot be read**: no file declares it (a vendor trait the checkout lacks), two do, a condition guards its declaration, a literal `class_alias` names it, or it imports one of those. The merge resolves this as a least fixpoint over the import graph from the shards' `destructor_declarers` and `trait_users`, so the engine reads one set (`destructor_classes`) as before. A trait's properties are not lowered, so the typed-property hop reads the class names of a trait's non-static properties and promoted parameters off the declaration (`ClassDecl::held_classes`, every class member of a union; a hint `self` names the class that imports the trait and `parent` its parent, as bounds) and hops through the importing classes and their ancestors' traits. Witnessed: `trait T { private ?D $d; } class U { use T; }` runs `[D]<after>`.
3. **A name nothing declares may declare a destructor.** `chain_declares` reads a class that no file declares, and the engine does not (`declares_engine_class`), as a class that may: an `extends`, a parameter hint, a property hint, an imported trait, the way every other family reads a chain the project does not hold end to end (ADR-0099 §4.3). An engine class closes the chain; a name the namespace made up (`App\Exception` for an unimported `Exception`) is not the engine's. An unseen *subclass* stays §7.2's open question. This is the incompleteness of a checkout with no `vendor/` and reads as any class once the tree is indexed (witnessed across two files: the same function reads clean, loud and unseen as the declaring file is clean, has a destructor or is absent). One line is drawn inside it: a `new` of a class no file declares (and an anonymous class that extends one) already records an unknown-class gap at the `new`, so the body is `…?` through the same name and a destructor gap on that subject's drops would only repeat it; those drops are left to it. A `new` of a *declared* class whose chain reaches an unseen name records no unknown-class gap, and keeps the destructor gap as the only signal, as does a hint that names one.
4. **Recorded.** The trait-property read in item 2 is what keeps the gate sound once the trait proxy is gone: a trait with a typed property holding a destructor class would otherwise read clean. A trait's own `self` and `parent` *parameter* hints have no frame to be read in (a trait's methods are not lowered, so there is no site to answer); its *property* hints are read, as item 2 says. Top-level code is not residue and has no site: residue is a hole in a body's answer, and there is no body.
5. **The closure is a fixpoint, seeded by what a class reaches.** A class reaches a destructor through its own declaration (a trait or an alias included), its parent chain, or a non-static class-typed property it declares or imports from a trait whose hint reaches one, recursively; every ancestor of such a class, and of an anonymous class that reaches one, is a bound that may run it. What a class holds may itself be an ancestor, so the closure is grown until a round adds nothing: it only grows over a finite universe, so it ends, and it is a function of the declarations and not of the order the shards are merged in (the permutation test pins the tables it reads). This also closes a hole S6b recorded as residue (a subclass's own typed property seen through a bound parent, row 6.22: `class U extends P { public ?D $d; }` through `P $p` runs `[D]`). A class that holds an unseen class reaches one by item 3, so it and its ancestors are gaps in a checkout without `vendor/`.
6. **Measured**, `steins check --profile strict --no-php --vendor-diagnostics --no-cache` on the ten public packages, the merge base against the head, one change at a time (items 1, 2, 3, then the fixpoint of item 5, which the review of #1006 added): drop sites that resolve to a gap 2,982 on the base, 2,375 after the first form of item 1 (607 removed: phpunit's `Constraint`, `Operator` and `Printer` trees, flysystem's adapters), 1,258 after item 2 (1,117 removed, Carbon's `Date` users 640 and console's 477 above all), 4,032 after item 3 (2,774 added: 1,604 a `new` of a declared class whose chain or properties reach an unseen name, 1,156 a parameter's or property's hint), 4,857 after item 5 (825 added: console 362, guzzle 183, Carbon 165, phpunit 92, flysystem 23; 403 hints, 341 a `new` of a class whose chain reaches a holder, 81 hints naming an unseen class; 504 in a test directory). The exemption of item 3 kept 1,510 further sites out of the third step and moved no function. Functions that are effect-exhaustive: 5,896, 5,949, 5,960, 5,884, 5,862 of 28,846 (throw lane 6,547, 6,603, 6,618, 6,538, 6,507), so over the whole change 89 become `…?` and 55 stop being, and the strict floor's possibly-grade findings go from 8,122 to 8,104 (the fp-gate's row notes carry the per-package split). The third and fourth steps are the vendor-less volume the owner reads apart from the rest.
7. **Warm runs.** The merge resolves traits over every package, so a trait's file changing, a vendor file appearing or a trait it imports changing reaches every file that read the answer through the class names the declarations carry (the trait names are in the file's class references already); checked cold against warm over eleven edits of a trait, a vendor class, an anonymous importer and a removed vendor tree.

## Amendment (2026-10-03): the drop subjects widen to every write, every consumed temporary and the properties (#1003, S6d) — PENDING ratification

Slice S6d of #915 closes the three shapes S6b recorded as residue (the last list of the drop-sites note above), each witnessed on PHP 8.5 (rows `a01` to `a25`, `t01` to `t10`, `p01` to `p34` and the must-stay rows `m01` to `m16` of the S6d revision on #1003 are tests in `operator_sites.rs` and `operator_rules.rs`, each checked to fail on the merge base where it is a change row and to pass on both where it is a must-stay row). The gate (`reaches_destructor`) and the sites' four forms are unchanged; the subjects and one new form are what move. No `SCHEMA_VERSION` bump: `OperatorConstruct` gains three variants at its end, `PropertyDecl` three fields and `ClassDecl` one, all of the trace payload, decoded past the analyzer gate.

1. **A local is a subject by any write that stores a `new`, not by its first.** Every plain write `$x = e` is read for the `new` expressions `e` may evaluate to: itself, the arms of a ternary (`a ?: b` reads `a`), `match` and `??`, the operand of a `clone`, of `(object)` and of `@`, and the value of a nested assignment (`$y = $x = new D` makes `$y` a subject as `$x`). Witnessed `<body>[D]` for `$x = $c ? new D : null`, `$x = null; $x = new D`, `$x = $y ?? new D`, a `match` arm and an untyped parameter overwritten (`function a($x = null) { $x = new D; }`), `<it0>[D]<it1><body>[D]` for the write in a loop. The receivers are the union of the classes written. A by-value parameter is a subject once the body writes a `new` into it, whatever its hint, and a hinted one keeps its bound and adds the class; `static $x = new D;` is a write of `$x` (witnessed `[D]<body>` for the `null` after it); a by-reference parameter is the caller's variable and stays residue (`mk(&$out) { $out = new D; }` runs `[D]` in the caller's scope, witnessed).
2. **A write releases nothing while no earlier write could have stored an object.** The first-write rule of S6b generalizes: a write is no reassignment when every textually earlier write of the variable stored a literal, an array, an interpolated string or a scalar operator's value, it is outside a loop and the function has no `goto`. So `$x = null; $x = new D;` has the scope-exit site alone (witnessed: nothing runs at the second write), `$x = make(); $x = new D;` has a reassignment site (the call may have returned an object), and a `null` written after a `new` drops it. The earlier write is read by source order, so the two branches of an `if` that each write a `new` still make the second a reassignment, as before.
3. **A `new` is a temporary wherever the expression consumes it.** The forms are the S6b three (a statement, a method call's receiver, a call's argument) and now a function call's callee and a static call's class expression (`(new D)()`, `(new D)::s()`), the object of a property fetch or write, `isset`, `empty`, `??` or `unset` (`(new D)->p`), the class of a constant or static-property fetch (`(new D)::X`, `(new D)::$s`), the array of an offset access, the initialization, condition and step of a `for`, the operand of `clone` (two drops: `clone new D` runs `[D]<body>[D]`), of a cast other than `(object)`, of `!`, `-`, `+`, `~`, of any binary operator but `??` (`instanceof`, `==`, `.`, `&&`), of `echo` and `print` (`s[D]<body>`), the condition of `if`, `while`, `do`, a ternary with a middle arm and `switch`, and the subject of `match` and `foreach`. A value that flows through a ternary, `??` or `match` arm, `(object)` or `@` to such a form, or to a statement or an argument, is judged there, so `foo($c ? new D : null)` and `$c ? new D : null;` are one temporary of `D` each. A statement's own assignment is not a temporary (its value is kept). A class that reaches no destructor is still a site: the resolver reads the class.
4. **A property write or `unset` is a drop of what the property held.** Three forms, `DropPropWrite` (a plain `=` to `$this->p`, `$this->{'p'}`, `self::$p`, `static::$p`, `parent::$p` or `C::$p`; `static` is its own receiver, `StaticKw`, the late-bound class), `DropPropUnset` (`unset($this->p)`; a static property cannot be unset) and `DropPropInit` (below), each with one receiver for the class that holds the property and the property as `member`. The resolver walks the holder's chain for the declaration, class by class: its own properties, then the properties of the traits it imports (a trait is read off its member list into `ClassDecl::trait_props`, since a trait's body is not lowered; a trait that imports one is followed), then the parent. The first declaration found decides, a property's type being invariant down a chain, and the value is its **declared hint read as written** (`PropertyDecl::hint_classes`, `hint_self`, `hint_parent`, the classes the lowered type loses for `array|D` or `?self`): a bound that reaches a destructor by `reaches_destructor`, as a parameter's hint does. Witnessed `[D]<close>[D]<drop>` (`$this->d = null`, `unset($this->d)`), `[D]<close>` for `self::$d`, `static::$d`, `parent::$d`, an inherited property, a trait's, an `array|D|null` one and `?self`, `<caught: from destructor>` at the assignment for a destructor that throws (the throw lane carries the gap there as the effect lane does). A readonly property, a property of a `readonly class` (`ClassDecl::is_readonly`) and one with a hook are no drop (a write either initializes the first or raises an `Error`, and `__clone` reinitializes a clone's while the original holds the value; the hook is the magic-property family's gap). A `private` declaration of another class than the frame's is invisible to it: a subclass writing its parent's private property creates a dynamic property and nothing drops (witnessed). Where the chain declares the property nowhere and the receiver is `$this` or `static::`, the subclasses' declarations are asked, a named subclass, one that imports the trait declaring it, or an anonymous one (`new class extends P { public static ?T $t; }`), by one lookup in a table of the universe's property declarers built once on the first site that asks, sorted so it does not depend on the order the files were read in (witnessed `<caught>` for each; an abstract base writing what its subclass declares runs `[D]<close>`; `static::$p` declared only by a subclass runs it and throws its destructor's exception at the assignment). A class declared more than once is asked in every declaration. The chain leaving the project at a name no file declares, and the engine does not, may declare the property (S6c item 3's unclosed-chain rule), as a trait no file declares may; a property no class of a closed chain declares is dynamic and untyped.
5. **An untyped property is residue; a constructor's first touch of its own property initializes.** Both are decisions the witnesses made. An untyped property names no class, so what it holds is whatever the class stores into it (`private $d; … $this->d = null` runs `[D]`, row `p16`): it is recorded residue as an untyped parameter is, and a gap there would mark every `$this->x = null` of an untyped class. A write over an **uninitialized** typed property drops nothing (`p17`: `<unset><set>[D]<unset>`, the first `unset` of an uninitialized property and the first write run nothing), so the first thing a constructor does to a property its own class declares, that no ancestor or imported trait declares as well and that is not promoted, is `DropPropInit` and resolves to nothing (`m02`, `m09`, `m15`, `p20`), over any default (a property default is a constant expression, so no object it holds ends with the write; witnessed `array|D $d = []` and a constant object `G`), unlike a promoted parameter whose `new` default is the last reference to its value and keeps the gap. First means: no earlier occurrence of `$this->p` in the body, no earlier use of `$this` but a property fetch with a literal name, no earlier `self::`, `static::` or `parent::` call or closure (`p19`: `parent::__construct()` first, then a write over what the parent initialized runs `[D]`; `p21`: a method that ran first), outside a loop and in a body with no `goto` (`p31`: `<it1>[D]<it2>`); the right-hand side runs first. A later write, in another branch too, is `DropPropWrite` (`p18`, `p32`). One shape stays residue, witnessed: a second explicit call of `__construct` over an initialized property (`p23`). A subclass constructor that writes the parent's property before calling the parent's runs the destructor in the parent's frame (`p22`, `r03`, `r38`: `<c>[D]<p>`); the child's write is a gap in the frame that caused it, whether it writes an inherited property (not its own declaration) or redeclares it (an ancestor declares the slot too), and the parent's own first write stays an initialization.
6. **Residue that remains.** A property of another object (`$o->d = null`, `$h->d = null` through a typed parameter: `p25`), `??=` and compound writes (nothing runs at `$this->d ??= new D` when the property is null, `p26`), a property written through `list()`, `foreach` or by reference, an untyped property (item 5) and one hinted `array`, `mixed`, `object` or `iterable` (`r29`: what it holds is whatever the class stores into it, `[D]<o>[D]<m>[D]<a>` on PHP), a promoted parameter overwritten as a local (it is a subject like any parameter, and its drop is a may-run beside the property's), the property writes of an anonymous class's methods and of a trait's, whose bodies carry no unit. The strict floor's `destructor` findings stay a lower bound on the drops that run user code.
7. **Measured**, `steins check --profile strict --no-php --vendor-diagnostics --no-cache` on the ten public packages, the merge base against the head, one group at a time (an instrumented build whose environment switches a group off; with none switched off it reproduces the head finding for finding). Groups 1 and 2 move nothing on these packages: no class a destructor reaches is written in those shapes. Group 3 adds 34 `throw.maybe-undeclared` (phpunit 21, guzzle 5, Carbon 4, monolog 2, console 2) and rewords 12 (the kind list of a callee gains `destructor`: phpunit 9, process 2, console 1), none disappears: 16 are the callers of one lazy-singleton write (`self::$instance = new self` in phpunit's `CodeCoverage::instance()`, hint `?self`, a class that reaches a destructor through its properties), 14 are hints that name an interface or an exception class that a class reaching a destructor implements or extends (guzzle's `?ResponseInterface` and `?ResponseException` 5, Carbon's `?CarbonInterface` 4, phpunit's rule and exception properties 5), 2 are writes of a hint that names a class this checkout lacks after `parent::__construct()` (monolog), and 2 are console's `?InvokableCommand`. No function changes exhaustiveness on these packages, so `effect-diff` reports no event, and the three `transform` dry-runs plan the same edits byte for byte. A method that writes a property already carries `state-construct`, which hides the new kind; a constructor does not (its writes to its own properties are exempt), so a constructor's second write, a write in a loop or after a use of `$this` or a `parent::` call flips `H::__construct` to `…?` (rows `r16`, `r34`), which none of the ten packages has. The first form of item 4 read an undeclared property as one any imported trait may hold and a class declared twice as unreadable, and counted 63 new and 12 reworded: 29 of the new went away when a trait's property is read by name and each declaration of a duplicated class is asked (Carbon's `CarbonInterval` writing the engine's `DateInterval` properties 13, the nine writes of composer's copies of `ClassLoader` and one a trait fallback flagged 10, phpunit's two copies 6). The possibly-grade rows move by the measured delta (the fp-gate's rows note the split).
8. **Warm runs.** A property's hint is read from the file that declares it, on the holder's chain, in a trait or in a duplicate declaration: the class names the declarations carry reach those files in the affected set as for every cross-file class read. Checked cold against warm over six edits (a parent's property hint to a clean class and back, a trait's property hint, the destructor of the class named, removed and restored); the fp-gate's parity check passes on the ten packages.
