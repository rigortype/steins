# Tolerated effects: policy-declared discharge at judgment time

Issue #323. Status: **proposed 2026-08-13; pending ratification.** The
load-bearing choices are owner-decided (2026-08-13): config lives at the
top level and not on a profile; the discharge rule ships with both of its
legs (label subsumption and path attribution) in one run; tolerance
applies to both envelope strata; the marker is display-only. Provenance:
the effect-pollution finding on a large private codebase and the design
note `phpstan-notes/generated-report/20260813-effect-pollution-logger-
masking.md`. **Generalizes** the hard-coded `mutate.local` discharge.
**Leaves ADR-0050 §10 and ADR-0068 §1 intact** — deliberately, see "What
this is not."

## Problem

A system-wide logger transitively reaches the clock and the filesystem, so
under whole-program propagation every code path that can touch logging
loses purity. Measured on a real codebase: thousands of true-positive
`effect.envelope-exceeded` findings on `@phpstan-all-methods-pure`
classes, every one flowing through the logging facade. The reports are
honest and unactionable at once — effect pollution.

The decisive observation from that codebase: the author had already
written `@phpstan-ignore impure.methodCall` at the logger call site. The
impurity is known and accepted, and said so in the only vocabulary
available. But **ignore suppresses a report; it does not discharge an
effect.** Under modular analysis the ignore is where the story ends;
under whole-program propagation the effect travels on and resurfaces at
every pure declaration upstream. The intent — "this impurity is
understood and accepted" — has no spelling that travels with the effect
semantics.

Three ways out, two of them wrong. Honest full reporting is semantically
correct and practically unusable at scale. Lying in the catalog ("the
logger is pure") is the metadata lie this project exists to refuse, and
it destroys the audit question "what touches the clock." The third way is
to make the concealment a named, auditable, first-class operation. That
is this ADR.

Steins already has one such operation, hard-coded: `mutate.local` is
"the label every envelope tolerates," justified by caller-
unobservability, implemented as a judgment-time predicate inside
`exceeds()` that never touches the propagated set. Telemetry is its
weaker cousin — unobservable *to the program* (fire-and-forget: PSR-3
returns `void`, and a corpus survey of 17 real applications found zero
sites consuming a logger call's return value) but observable to the
world (the log file grows). That is exactly why it cannot be promoted to
a built-in tolerance: "unobservable to whom?" is the project's call, not
the analyzer's. So the mechanism generalizes and the *set* becomes
policy.

One recon fact shapes the whole design. Semantic labels have **no path
into the proven lane** today: plugin labels enter the declared lane only,
and only for unresolvable callees; a resolvable logger body contributes
its transport effects (`nondet.time`, `io.fs.write`) to every caller,
unchanged, hop after hop. Subtracting a semantic label from a flat proven
set therefore discharges nothing real — and subtracting the transport
labels instead would also tolerate business logic that reads the clock.
The issue's composition ("the same `time()` call, distinguishable by what
it was for") requires the judgment to know *how the effect arrived*.

## Decision

### 1. Config: a top-level `[effects]` table

```toml
[effects]
tolerated = ["telemetry"]

[effects.attribution]
"Monolog\\Logger" = ["telemetry"]        # class: every method
"App\\Log::debug" = ["telemetry"]        # one method
"app_trace" = ["telemetry"]              # a global function
```

- `tolerated` is the policy: the set of labels the envelope judgment
  discharges. Any label is admissible — transport labels included; the
  docs recommend tolerating semantic (attributed) labels, but bluntness
  is the project's right.
- `[effects.attribution]` is fact, not policy: it marks symbols as
  *being for* something. Attribution alone changes no judgment; it only
  gives the policy something precise to grip. Keys name a class (all
  methods), a `Class::method`, or a global function. Values are label
  lists; a label first introduced here is thereby project-declared and
  enters the `LabelRegistry` view for validation purposes.
- `steins check --no-tolerated-effects` runs the judgment with an empty
  tolerance set — the audit switch: every discharged finding comes back.
- This is **not** a `[profile.*]` field. ADR-0050 §10's refusal stands
  unamended: profiles select surfaces, never inference behavior. The
  tolerance changes which findings exist (and what the purity oracle
  answers), so it lives with the project config that is already a salsa
  input, and profile switching stays free.

### 2. The discharge rule

During `compute_effects`, when findings cross a call edge out of an
attributed symbol, each copied finding accumulates that symbol's
attribution labels. Findings differing only in attribution are distinct
set elements; nothing is ever removed or rewritten. The proven lane —
labels, origins, propagation — is byte-for-byte what it was before this
ADR.

At every judgment site, a proven finding is **discharged** iff

1. its label is subsumed by a tolerated label (`subsumes(t, f.label)`),
   or
2. **every** copy of its finding group (same label, ultimate origin,
   site) carries at least one attribution subsumed by a tolerated label.

Leg 2 is must-semantics over paths: an effect that reaches a declaration
both through the attributed facade *and* through a direct `time()` call
is discharged for neither — the direct path keeps its report (in
practice the two arrivals are already distinct findings, because the
ultimate origins differ; the group rule covers the residue where they do
not). `mutate.local` remains the built-in, unconditional degenerate case
of leg 1 and is not spelled in config.

### 3. Where the rule applies — and where it never will

Discharge is a property of the **judgment**, not of any spelling, so it
applies at every site that judges a proven set against a bound:

- `effect.envelope-exceeded`, both strata — checked attributes and
  interop envelopes (ADR-0082) share `report_unit`/`exceeds` and are
  covered by the same rule. The attribute stratum's Liskov conjunction
  (`effect.liskov-widened`) judges the same proven set against each
  abstraction's bound; the subtraction is on the proven side, the
  conjunction on the declared side, and they do not interact.
- The purity oracle (`pure-callable`/`pure-closure` acceptance) — else
  purity queries and envelope judgments would disagree about the same
  function.

It applies at **no spelling-producing site**. The docblock-writing
transforms keep judging by the undischarged proven set: a method whose
only effects are tolerated telemetry does *not* earn a written
`@phpstan-pure`, because the docblock outlives and out-travels the
policy — a spelling is a portable claim, and the policy is not portable.
The asymmetry (check says the envelope holds; transform declines to
write the tag) is intended and documented.

### 4. Rendering: the policy is visible, never load-bearing

`annotate` continues to show every proven label. A label wholly
discharged at a unit under the current policy renders with a tilde —
`effects: {~nondet.time, io.db}` — so the margin shows the policy at
work; a partially discharged label (some finding groups survive) stays
unmarked. The JSON surface gains a `tolerated` array beside `labels`;
`labels` itself is unchanged. Docblock-writing output carries plain
labels only — the marker is display vocabulary, not tag vocabulary.

### 5. Validation

Every entry of `tolerated` and every attribution value is checked
against the registry view (builtin ∪ plugin ∪ attribution-declared):
unknown labels get the existing nearest-suggestion treatment at config
load. Attribution keys that resolve to no known symbol are reported as
notices, not errors — vendor code comes and goes.

## The three properties, restated as invariants

1. **The catalog never lies.** `time()` stays `nondet.time`; the fixpoint
   is untouched; `annotate` shows every label. Only judgments consult
   the policy.
2. **The concealment is named and auditable.** One `[effects]` table in
   one reviewed file; it shows up in config diffs; and
   `--no-tolerated-effects` reproduces the unconcealed world on demand.
3. **Reversible per question.** "What touches the clock" reads the
   labels, which are all present. "Does this violate its pure envelope"
   is the only question that applies the tolerance.

## What this is not

- **Not a profile field.** ADR-0050 §10 keeps its full strength; no
  carve-out. Should per-profile tolerance ever earn a use case, that is
  a new discussion with this ADR as prior art.
- **Not a plugin power.** ADR-0068 §1 stands: plugin facts never
  discharge taint. The follow-up (plugin manifests shipping method/class
  attributions plus a *recommended* tolerance) keeps that shape — a
  plugin attribution is inert until the project's own `[effects]` policy
  opts in. Nothing a plugin ships can silence a finding by itself.
- **Not complement bounds.** The interop spec reserves `-except` — a
  per-declaration exclusion spelled in a tag. This ADR is a project-wide
  policy applied at judgment for every declaration; the two are family
  (subtract-from-the-bound by vocabulary / by HOF / by policy) but not
  the same operation, and neither preempts the other.
- **Not path-sensitive.** Throw-path-only effects (assert helpers that
  log only on the failure path) stay out of scope, recorded in the
  design note as an open direction.

## Consequences

- The logger-pollution shape becomes fixable without a lie: attribute
  the facade, tolerate `telemetry`, and the true-positive
  envelope-exceeded findings flowing through it discharge — while a
  business-logic clock read beside them keeps its report.
- Corpus validation (2026-08-13, steins-survey) confirmed the mechanism
  and corrected the target. Attributing a resolvable vendor class on
  firefly-iii under `--vendor-diagnostics` discharged exactly its 19
  cross-edge arrivals, kept the attributed class's own direct call
  reported, and `--no-tolerated-effects` restored the unconcealed output
  byte for byte. But framework logging itself is effect-invisible to
  today's propagation — Laravel's `Log::` facade (`__callStatic`), the
  `logger()` helper, injected PSR-3 interfaces, and even direct Monolog
  (bottoming out in uncatalogued internal constructors and interface
  dispatch) contribute no proven effects, so on the public corpus there
  is no logging pollution to discharge yet. The pollution this ADR fixes
  presupposes a logging path the analyzer resolves: project-local
  facades over builtins have it, and builtin production sites take
  attribution directly. Closing the framework-visibility gaps (catalog
  rows for effectful internal constructors and `trigger_error`, facade
  edge resolution, interface-dispatch propagation) is issue #326.
- Survey guidance encoded in docs, not mechanism: PSR-14 `dispatch()`
  must not be attributed telemetry (its return value is routinely
  consumed); compliance/audit logging deserves its own label (`audit`)
  rather than riding `telemetry`, because "safe to stop watching" and
  "must always fire" are different risk profiles that happen to share a
  shape.
- `EffectFinding` grows an attribution set; fixpoint memory grows by the
  number of distinct attribution combinations per finding, bounded in
  practice by the handful of attributed boundaries a path crosses.
- The interop spec gains an informative section beside the reserved
  complement-bounds section: the ignore-vs-discharge distinction and the
  subtract-family taxonomy, as shared-spec material for the upstream
  conversation.
- The private-side effect tripwire stays red until that project writes
  its own attribution and tolerance — greening it is now a config
  exercise for that repo, not a Steins code change.
