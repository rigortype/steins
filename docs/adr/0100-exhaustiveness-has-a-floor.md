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
   (`effects-envelope`, `throws-envelope`, `loop-to-array-map`), `annotate` and
   `effect-diff` read the same gaps as before and do not move. The floor is two
   reporting loops over the resolution those lanes already compute.

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
   project body whose own set is `…?` (`EffectSet.gaps`, `ThrowSet.gaps`) and
   that has no envelope of its own on that lane (discharge 4), one finding at
   the site, naming the callee and the callee's gap kinds (the inherited mask).
   An untainting edge (ADR-0063: a conditional-purity contract decided in full
   at the call site) draws none, because its `…?` does not propagate. A
   closure reached as a callback is a project body with no envelope, so it is
   reported at its edge.
3. **Why not one per unit.** One finding per unit would hide the site, and a
   `@steins-ignore` placed on the declaration would silence sites added later.
   **Why not one per transitive origin.** The volume is where the fixpoint
   multiplies one cause across every caller; the per-edge form reports each
   cause once, at the nearest unit that owes it.

## 4. Decision: five discharges, and what is deliberately not discharged

A finding is not reported when:

1. **A ⊤ envelope.** Not a unit (§2.3).
2. **A call through a parameter whose purity the call sites enforce** (effect
   lane only). `$f()` where `$f` is a parameter of the frame typed
   `pure-callable`, `pure-closure` or `static-pure-closure`, or flagged
   `@pure-unless-callable-is-impure`: the argument is held to purity at every
   call site the analyzer sees (ADR-0063), so the call is answered there. This
   discharges only the `dynamic-callee` gap of that site, and only when the
   parameter is by-value, non-variadic, and never rebound in the frame: no
   statement writes it, and no named call of the frame may take it by
   reference. The syntax layer decides the first half and carries the name on
   the site (`DynamicSite::Call { var }`, set only for such a parameter; it is in
   the trace payload, past the analyzer gate, so there is no `SCHEMA_VERSION`
   bump); the resolver's `Frame::rebound_by_call` decides the second. A purity
   contract says nothing about what is thrown, so the throw lane takes no
   discharge from it.
3. **An interop envelope whose imported bound fits** (effect lane). An interop
   envelope (ADR-0082) answered a declared-receiver call: its labels entered the
   declared lane and the answer stayed open, as an unchecked claim must
   (`GapKind::InteropEnvelope`). When every label the site imported fits the
   declaration's own envelope (`!exceeds`, under the project's tolerance
   policy), or the import is empty (`@phpstan-pure`), the claim cannot break
   this envelope and the finding is discharged. A bound that does not fit stays a
   finding of kind `interop-envelope`.
4. **An edge to an enveloped callee.** The callee is itself a unit and reports
   its gaps once, itself. "Enveloped" means a non-⊤ envelope on that lane: a
   `@phpstan-impure` callee, or one declaring `@throws \Throwable`, bounds
   nothing and is still reported at its callers' edges.
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

**Deliberately not discharged:** every other kind, `NoEffectRow` and
`NoThrowRow` included. A catalog name with no row is the analyzer's own
coverage hole; reading it as covered is the unsafe default ADR-0099 §3 removed,
and it is exactly the finding a strict reader wants to see, because it is
fixable (a row, an audit, #881).

A known imprecision, recorded: discharge 2 applies to the unit's own sites, not
through edges. A callee with a `pure-callable` parameter and no envelope of its
own is `…?` for its `$f()`, so an enveloped caller is reported at the edge
unless the call decides the contract (ADR-0063's untainting edge). Moving the
discharge into the propagation would change the exhaustiveness bit, which this
ADR does not touch.

## 5. Decision O1: emit `throw.maybe-undeclared` now

The throw sibling is loud: on the public corpus (ten packages, measured on the
slice's branch) it reports 8,202 findings, 7,313 direct and 889 inherited,
against 149 findings of both throw ids before (the ten public packages; the fp-gate's `phpstan-src` row is measured separately); per package, 4 (nikic/PHP-Parser)
to 3,374 (composer/composer). By direct kind the volume is dynamic callees
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

**Writers.** `effects-envelope` writes only from an exhaustive summary (ADR-0082 §7), so a tag it writes draws no effect sibling (measured: 0 over 723 tags across the public corpora). `throws-envelope` writes the escapes it *proved*, in lockstep with `throw.undeclared`'s fire set (`proven && !covered`, whether or not the body is `…?`; ADR-0040 §1, ADR-0037), and `throw.undeclared` itself reads certainty, never the exhaustiveness bit. On the declaring side that is also PHPStan's reading: `missingCheckedExceptionInThrows` skips implicit throw points. But a callee's `@throws` is consumed as its throw type at PHPStan call sites (absent, the call is an implicit `Throwable`), so on a `…?` body the written tag is a verified lower bound the consumer reads as an upper one. The floor names the difference, at `strict` only: a tag written on a `…?` body draws `throw.maybe-undeclared` for the body's gaps. Measured on scratch copies of the ten public packages: 1,934 tags; PHP-Parser 4 → 276, monolog 174 → 936, composer 3,170 → 13,595; all 11 tags on symfony/process land on gapped units. The writer is unchanged here; whether it should refuse a `…?` body, write and report the gap in its own report, or keep writing, is #921, decided after S2–S8 have narrowed the gaps the count depends on.

The slice's stated acceptance, "zero sibling findings on written tags", holds for `effects-envelope` and was wrong for `throws-envelope`, for the reason above.

**Known gaps.** #922: the fp-gate's throw table conflates the definite and the sibling counts in one number. #923: a `Maybe`-certainty escape that carries no gap kind is not reported by the floor.

## 6. What this does not change

The `effect.liskov-widened`, `throw.liskov-widened` and every proof-layer id;
the exhaustiveness bit and the gap kinds' codec numbering; `annotate`'s margin
and JSON; the effect baseline and `effect-diff`; the three transforms' plans;
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
