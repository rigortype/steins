# Proven and declared are two effect lanes; an envelope can be a source only in the second

Issue #66. Status: PENDING ratification (autonomous design under the owner's
post-hoc-ratification mode, per the ADR-0063 precedent). Context: the effect
system (ADR-0005 origins, ADR-0018 labels, ADR-0019 envelopes), the honesty
discipline (`docs/type-specification/certainty.md`), and the why-effects.md
promise this ADR exists to make true: a call through an interface retains
effect information after dependency injection breaks the concrete call graph.

## 1. The conflict this resolves

An envelope today is only a check target. Making it also a *source* — a call
through an interface-typed receiver contributing the interface envelope's
labels to the caller's summary — collides with two standing rules:

1. `effect.envelope-exceeded` and `effect.liskov-widened` read **proven**
   effects only. If declaration-origin labels entered the proven set, a
   declaration could manufacture a finding, which breaks the zero-FP bar at
   its foundation.
2. The exhaustiveness bit says "these effects, and possibly more". An
   envelope-covered call is *not* "possibly more" — it is "at most this" —
   and neither the proven set nor the taint bit can say that.

The resolution is that these are different kinds of fact and get different
lanes. A proven effect is an **occurrence**: this code demonstrably does
this. A declared effect is a **bound**: whatever this call does, a checked
contract caps it here. Collapsing bounds into occurrences is the same lie in
both directions — as occurrences they manufacture findings, as omissions
they waste the contract.

## 2. Decisions

1. **Two lanes in every summary.** `EffectSummary` carries `proven` (today's
   set, unchanged in meaning) and `declared` (upper bounds imported from
   envelopes at otherwise-opaque call sites). The lanes never mix during
   propagation. Both grow monotonically in the fixpoint; termination
   arguments are unchanged.
2. **The source rule.** A resolvable-receiver-*type* method call that today
   degrades to `Opaque` taint — receiver statically typed to an interface or
   non-final class whose method (or nearest abstraction ancestor's method)
   carries an envelope — instead contributes the envelope's labels to the
   caller's **declared** lane. The proven lane is untouched. A method
   without an envelope stays exactly today's taint: absence of a contract is
   not a contract.
3. **Per-site taint discharge.** The covered call no longer taints the
   exhaustiveness bit: the bound replaces "possibly anything" with "at most
   this", which is the entire value of the import. Discharge is
   **per call site** — any other unresolved call in the body taints as
   before. Trust justification: the envelope is a *checked* declaration —
   every analyzed implementation is held to it by `effect.liskov-widened` —
   which places it in the native-attribute trust stratum, above phpdoc and
   far above nothing. An unanalyzed implementation can still lie; that risk
   is the same one Steins already accepts for every consumed declaration,
   and the lane separation is what keeps the risk out of the diagnostics.
4. **Propagation joins per lane.** Caller `proven` ⊇ join of callee
   `proven`; caller `declared` ⊇ join of callee `declared` and locally
   imported bounds. A declared bound does not become proven by traveling.
   **Normalization at rendering**: a declared label subsumed by a proven
   label of the same summary is dropped (`declared \ proven-subsumed`) — the
   occurrence already implies the bound.
5. **Diagnostics stay proven-only.** `effect.envelope-exceeded` and
   `effect.liskov-widened` read the proven lane, exactly as today, and no
   declared label ever produces a finding. A future policy surface may
   consume the declared lane ("no `io.db` even *declared* in this layer");
   that is a policy decision (ADR-0023 territory), not this ADR.
6. **Rendering names the lane.** The annotate margin renders a declared
   label with a `≤` prefix — `effects: {output, ≤io.db}` — read "at most".
   The JSON surface (issue #65) carries `declared` as a separate array
   beside the proven `effects` and `exhaustive`; nothing is flattened. The
   effect-diff surface (issue #69) reports lane transitions distinctly: a
   label moving declared→proven is a materialization, not an addition.
7. **Bound/occurrence conflicts are the implementation's problem.** When a
   proven effect (via another path) exceeds a declared bound imported into
   the same summary, the caller does nothing special — normalization keeps
   both lanes honest, and the finding, if any, fires where it belongs: on
   the implementation whose proven effects exceed *its* envelope.

## 3. Considered and rejected

- **One lane with a per-label origin flag.** Loses the semantics: a bound
  is not an event with a footnote, and every consumer (subsumption checks,
  diff, policies) would need the flag anyway — that is two lanes with worse
  types.
- **Importing envelopes into the proven lane.** Declarations manufacturing
  findings; rejected on the zero-FP bar.
- **Importing the bound but keeping the taint.** Double-counts the unknown:
  "at most io.db, and also possibly anything" makes the contract worthless
  and the summary unreadable.
- **A third lane for unchecked declarations (phpdoc-origin bounds).** Not
  now: `@phpstan-pure`/`@phpstan-impure` are deliberately unread
  (`docs/type-specification/not-implemented.md`), and a stratum-per-lane
  explosion is exactly what the trust-stratum vocabulary (issue #33) exists
  to avoid. If phpdoc bounds are ever consumed, they enter the declared
  lane tagged by stratum, not a new lane.
