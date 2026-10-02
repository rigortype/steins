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
4. **Further residue, recorded.** A class whose parent no file declares reaches nothing the analysis can see; `new self`, `new static` and `new $c` name no class; a `foreach`, `catch` or `list()` binding over a subject, `??=`, and a by-reference or variadic parameter are not forms; code outside a function-like body (a script's top level) carries no sites; a property hinted `array|D` or `self` lowers to no hint, so the hop does not read it. Three shapes are left to a follow-up slice, each witnessed on PHP 8.5 and each a drop that runs user code with no gap:
   - a `new` that reaches a local through anything but its first write: `$x = null; $x = new D();`, a ternary or `match` arm (`$x = $c ? new D() : null;`), an untyped parameter overwritten with one (`function a($x = null) { $x = new D(); }`), each `<body>[D]` at the scope exit;
   - a `new` temporary in a position the three forms do not name: `clone new D()`, `(new D())->p`, `echo new D()`, `new D() instanceof D` (`[D]<body>` for the last, `s[D]<body>` for `echo`);
   - overwriting, unsetting or throwing through a typed or static property whose hint reaches a destructor: `private ?D $d; … $this->d = null;` and `self::$d = null` run `[D]` before the next statement, and a destructor that throws surfaces at the assignment (`<caught: from destructor>`), the usual close-and-reset pattern.
5. **The volume is the gate's, and it is large where the gate is conservative.** On the ten public packages the first measurement marked 3,823 sites (2,903 scope exits, 578 temporaries, 323 reassignments, 19 `unset`s); seeding the ancestors from every inheriting class and reading `array|D` and `self` hints added a few (85 functions lose exhaustiveness where 81 did). Roughly 58% reach through a class that imports a trait (the shard table counts one whether or not its trait declares `__destruct`), 21% through an anonymous class's parent (the table lists the parent whatever the anonymous body declares), 21% through a declared destructor or a property that holds one (monolog's `Handler`, symfony's `Process`). Two refinements would take the first two away without touching the rule, and each moves the shard's table, so each is a `SCHEMA_VERSION` bump of its own: record, per class, whether a trait it imports is a project trait that declares `__destruct` (a trait's body is not lowered today), and list an anonymous class's parent only when its body declares one or imports a trait.
6. **Warm runs.** The typed-property hop reads the property's class in another file; the affected set's call-graph leg reaches that file through the class names the declarations carry, to `MAX_BINDING_DEPTH` hops, as it does for every other cross-file class read. A hop deeper than the bound is the same open edge the bound leaves everywhere.
