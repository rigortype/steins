# A `@psalm-trace` docblock asks the dump surface's question

Issue #92. Status: PENDING ratification (autonomous design under the
owner's post-hoc-ratification mode).

## 1. Context: a second author, not a second question

The dump surface (ADR-0053) answers requested introspection: the user
asks "what do you know here?" and the engine answers, through the one
honesty renderer, at fail level for the explicit `PHPStan\dumpType()`
pair because a committed call to a function that does not exist at
runtime is a guaranteed fatal. That posture is right and it has a cost
the author pays every time: the question can only be asked by writing a
call that must be deleted before commit, and that reds CI until it is.

Psalm's users ask the same question without paying it: a
`/** @psalm-trace $var */` docblock above a statement makes Psalm report
the variable's inferred type at that point. The trigger is a comment —
runtime-inert, committable, syntactically incapable of breaking the
program — and the vocabulary is established upstream, exactly the
situation ADR-0029 exists for. This ADR gives the dump surface that
second spelling. Everything below the trigger is deliberately *not* new:
the annotation asks the SAME question `PHPStan\dumpType($x)` asks, and
it gets the same answer through the same machinery. What this ADR
decides is the trigger, its id, its exit posture, and its placement
rules; what it pointedly does not do is reopen anything ADR-0053
settled about how the question is answered.

## 2. The trigger: tag `trace`, prefixed-only

The scanner (`steins-phpdoc`, `TagKind::from_name`) recognizes a
docblock tag `trace` in **prefixed form only**, through the same
uniform `@phpstan-`/`@psalm-` prefix strip every other tag goes
through. Three consequences, each deliberate:

- **`@psalm-trace` is the canonical spelling** — it is the established
  Psalm vocabulary (Psalm's `Trace` issue), and compat vocabulary is
  adopted, not invented (ADR-0029).
- **`@phpstan-trace` is accepted**, as a consequence of the mechanism
  rather than as a vocabulary claim: the prefix strip is uniform across
  tags, and special-casing one prefix out of it would complicate the
  scanner for zero soundness gain. PHPStan has no trace tag; if it ever
  grows one with different semantics, that is a divergence-registry
  entry (ADR-0030's pattern), not a reason to pre-fragment the strip.
- **Bare `@trace` is not a tag at all** — neither upstream tool
  recognizes it, so recognizing it would be invented vocabulary. This
  is the assertion-family precedent verbatim: `@phpstan-assert` /
  `@psalm-assert` exist, bare `@assert` does not.

The tag's payload is **variable names only** — `$var`, and (§7) a
comma-separated list of them. No type text, no expression. This is the
`ConditionalPurity` shape already in the scanner: a tag whose payload
is a name in the shared `var_name` field with empty `type_text`.
Matching Psalm here is not deference for its own sake — an expression
argument would need an expression grammar inside a comment and a
position at which to evaluate it, neither of which the docblock
carries; a variable name plus the adopted statement's position is
exactly determined.

## 3. Why ADR-0053 §12 does not bar this

Two of ADR-0053's refusals read as if they might, and neither does:

- **The native-alias refusal** (`Steins\dumpType`) barred a second
  *call* spelling that Steins would have invented — two spellings of
  one introspection call, vocabulary sprawl. `@psalm-trace` is the
  opposite case on both axes: it is upstream-established compat
  vocabulary, and it is a *docblock* trigger, not a second call. The
  reasoning that kept `\PHPStan\dumpType` as the one call spelling —
  the compat spelling *is* the vocabulary (ADR-0029) — is the same
  reasoning that admits `@psalm-trace` as the one docblock spelling.
- **The added-trigger refusal** (`print_r`/`var_export`/`dd`/`dump`)
  barred multiplying the call-recognition surface: every added call
  trigger re-runs ADR-0053 §5's resolution matrix — FQN reservation,
  fallback legs, homonym existence, callable forms. A docblock trigger
  touches none of that machinery: there is no name to resolve, no
  namespace fallback, no homonym question, no first-class-callable
  form. Recognition is a pure function of the file's trivia and the
  statement it precedes (§6), which is *less* resolution surface than
  the existing triggers, not more.

## 4. The id: `debug.trace`, and the naming rule for code

One id, family `debug`, kebab-case per ADR-0022: **`debug.trace`**,
registered `(DEBUG_TRACE_ID, Layer::Debug, Floor::Default)` through the
S1 pattern (ADR-0049) — listed in `REGISTERED_NOT_YET_EMITTED` by the
groundwork slice, moved to `ALL_EMITTABLE_IDS` by the emit slice, the
totality test binding each step. The user-facing rule-name "trace"
matches Psalm's issue vocabulary and sits beside its siblings in the
dump family's naming (`debug.type`, `debug.phpdoc-type`,
`debug.var-dump`).

**The naming rule, recorded because it will otherwise be violated in
the first slice:** in this codebase "trace" already means the trace IR
— ADR-0027's linear trace, ADR-0031's structured trace tree,
`walk_trace`, sub-traces. Internal symbols for this feature therefore
never use bare `trace`: the tag kind, recognizer, association query,
emitter and fixtures use `trace_annotation` / `TraceTag`-style names
(`TagKind::TraceTag`, `emit_trace_annotations`, and the like). The
diagnostic id string is the one place the bare word appears, because
there it names the user-facing vocabulary, not a code concept.

## 5. Answer semantics: the same question, the shared machinery

The governing principle, from which every sub-decision below follows:
**the annotation is a second spelling of the question
`PHPStan\dumpType($x)` asks, so it shares the answer machinery
wholesale.** Concretely:

- **The fact source is the trust-ordered lookup** ADR-0053 §2 fixed for
  `debug.type`: the four-layer value fact where one is bound, else the
  heap's exact class / `Member` bounds, else the narrowed contract-fact
  arm list, else honest `unknown` — proven beats membership beats
  declared (ADR-0037). Not the phpdoc-side view; the trace asks what
  the engine *knows*, like `dumpType`, not what the code declares.
- **The renderer is the one renderer**: the N1 normalizer plus the
  shared plain-text arm spelling (ADR-0053 §7), the `(asserted)`
  stratum marker (ADR-0052 §5), the honest-`unknown` posture. The
  **annotate byte-parity obligation extends to this id**: the trace's
  rendered fact and `annotate`'s margin fact for the same variable at
  the same position are byte-equal, pinned by the same parity test
  discipline.
- **Only the trigger and the message label differ.** The label is
  "traced type"; the frame wording around the rendered fact is not a
  contract (ADR-0023) and may improve, the rendered fact is pinned.
- **Position semantics are Psalm's: the answer is the adopted
  statement's EXIT facts.** Psalm documents the annotation as "applied
  to the next statement" and reports the type that statement leaves
  behind — `/** @psalm-trace $x */ $x = $_GET['x'];` prints what `$x`
  became. The compat spelling carries the compat semantics (ADR-0029's
  reasoning applied to behavior, not just vocabulary): the trace
  reports what `dumpType($x)` would report were it the *following*
  statement — after the adopted statement's own effect (an
  assignment's binding, a guard's narrowing) has applied; inside a
  loop body, a branch arm, wherever. The answer is produced mid-walk
  like every dump, and it emits even when the adopted statement
  diverges — a `return $x;` under the annotation still answers.
- **Emission is descent-gated exactly like the dump calls**: the
  emitter runs in the plain per-scope pass only (never under an
  interprocedural binding descent), so a site emits once — an annotated
  statement inside a function body does not re-report per caller.

A variable name with no fact at that point — never assigned, out of
scope — renders `unknown` like any other unanswerable dump; a missing
answer is honest incompleteness, never silence and never a guess.

## 6. Placement: the shared statement-adoption rule

A leading docblock is adopted by the **next statement** under the
statement-adoption rule this codebase already has: ADR-0073's
`stmt_docblock`, built for the inline `@var` cast — the nearest
preceding comment trivium, adopted iff it is a docblock and nothing
but whitespace separates its end from the statement's start (the
declaration rule of ADR-0029, transposed). The rule is deliberately
**shared, not per-tag**: the inline cast and the trace annotation are
two tags read at the same position, and two adoption grammars for one
position would be the fragmentation this ADR refuses everywhere else.
An earlier draft carried two statement-side tightenings (a one-line-
break maximum in the gap; a line-leading requirement); both were
dropped when ADR-0073 landed first and ratified the shared rule —
diverging from the cast's adoption would cost more than the
tightenings bought, and the cast surface, where a wrong association
changes analysis rather than merely a report, accepted the same
looseness. Consequences, each a fixture:

- Any intervening non-whitespace — code, a `//`/`#` comment (the
  nearest trivium is then not a docblock), another docblock — breaks
  adoption. The annotation then triggers nothing, silently; a missed
  trace is a missed service, never an FP, so silence is the free safe
  side (the ADR-0053 §5c posture).
- A blank line does **not** break adoption, and a docblock trailing
  another statement's line adopts forward onto the next statement —
  both inherited from the shared rule, identical for `@var` casts.
- Association is a **per-file query**, usable at any statement nesting
  depth — a statement inside a loop body, a branch arm, a closure body
  adopts its leading docblock by the same rule.
- **Declaration statements are inert at the emitter** (§5's machinery
  skips function/class/interface/enum/trait declaration statements): a
  `@psalm-trace` inside a declaration's docblock is a tag on a
  contract surface, and it triggers nothing — no diagnostic, no error.
  The adoption query itself stays tag-agnostic and shared; the
  trace-specific exclusion lives with the trace emitter.

## 7. Multi-variable lists, staged

`@psalm-trace $a, $b` — the comma-separated list Psalm accepts — is in
scope: **one diagnostic per named variable, in source order**, each
rendered independently through §5's machinery (one variable's `unknown`
does not perturb its neighbors). Delivery is staged: the tracer-bullet
slice lands the single-variable form end to end (issue #94); the list
form rides the breadth slice (issue #95). The staging is delivery
sequencing, not scope doubt — the list form is decided here.

## 8. Exit posture: warn, fixed, and no escape hatch

**`debug.trace` is born at warn and fixed there.** The explicit pair's
fail level was *forced*, not chosen: `PHPStan\dumpType()` names a
function that does not exist at runtime, so a committed call is a
guaranteed fatal on any live path, and failing CI on it is the zero-FP
identity agreeing with reality. A docblock is runtime-inert — the
forcing argument simply does not apply. What remains is authorship of
the question (ADR-0053 §3's axis), and warn-level visibility answers
it: the asked question is answered, visibly, on every run, without
holding CI hostage to an annotation that is legal to commit. This
mirrors `debug.var-dump`'s level with the opposite justification —
there, exit-neutrality protected pre-existing incidental calls; here,
it acknowledges a deliberately committable trigger.

**Layer-inherited properties are reaffirmed, not re-decided**: as a
`Layer::Debug` id, `debug.trace` is never written by `--set-baseline`
and never matched by a baseline entry, exempt from all three
suppression channels (ADR-0023 — an ignore naming it reports
`suppress.unmatched`), excluded from every fp-gate counter (ADR-0053
§8's partition), and carried as `layer: "debug"` in JSON output. None
of that is new machinery; it is what the layer means.

**Unlike `debug.var-dump`, there is no profile-disable escape hatch.**
The var_dump opt-out exists because dump-on-var_dump is a default
*service* layered onto calls whose authors never asked Steins anything
— an incidental trigger, decline-able. An annotation has no incidental
case: `@psalm-trace` is always an authored question addressed to the
analyzer, first-person intent in the code, and a profile that muted it
would be the silence ADR-0053 §4 refuses for the explicit pair. The
remedy for an unwanted trace is deleting the comment, one keystroke
away. `debug.trace` is therefore profile-inert like `debug.type`.

## 9. Transparency and replay

**The annotation contributes nothing to entry state** (ADR-0048 §3
stance, restated for this trigger): it reads facts, never binds them —
no env write, no heap write, no carrier contribution, no narrowing
side-effect. Analysis output with and without the annotation is
identical except the Debug-lane diagnostics themselves; the fixture
matrix includes the transparency case, mirroring ADR-0053 §10's
dump-is-transparent fixture.

Replayability (ADR-0048 §2) holds trivially — more trivially than for
the call triggers: recognition is a pure function of the file's trivia
and CST (no resolution query, no existence surface, no sidecar), and
the answer is the walk's fact at a position, reproduced byte-for-byte
by re-walking the scope. Ordering stays positional within a file and
presentational across files (§4), like every diagnostic.

## 10. Refusals

- **Bare `@trace`** — not established vocabulary in either upstream
  tool; recognizing it is invented vocabulary (ADR-0029; the
  assertion-family precedent, §2).
- **A Steins-native alias tag** — two docblock spellings of one
  question is the sprawl the `Steins\dumpType` refusal barred, now on
  the tag surface (ADR-0053 §12); revisit only with a Steins-only
  capability the compat spelling cannot name.
- **A trace-specific adoption rule** — the annotation reads the
  position through ADR-0073's shared `stmt_docblock`, verbatim; a
  per-tag placement grammar (an earlier draft's blank-line and
  line-leading tightenings included) would fragment one position into
  two rules (§6).
- **Expression arguments** — variables only, matching Psalm; a comment
  carries no expression grammar and no evaluation position (§2).
- **A boolean setting instead of the layer** — reaffirming ADR-0053
  §12: the compiler-forced posture statement of the `Layer` match is
  the design (ADR-0050 §1).
- **A profile-disable for `debug.trace`** — an annotation is always an
  authored question; there is no incidental case to decline, and the
  remedy is deleting the comment (§8).

## 11. Slices

Standard verification protocol throughout (workspace tests grow,
clippy 0, release build, foreground gate). Downstream issues cite this
ADR by section:

- **Prefactor (issue #93)** — retired: the statement-adoption query
  this slice was to build landed on master as ADR-0073's
  `stmt_docblock` (the inline `@var` cast's seam) while this ADR was
  in flight. The trace annotation consumes the shared query (§6);
  there is no trace-side syntax work left.
- **Tracer bullet (issue #94)** — the single-variable form emits end
  to end: the tag kind in `steins-phpdoc` under §2's prefixed-only
  rule and §4's naming rule; `debug.trace` registered
  `(Layer::Debug, Floor::Default)` with every exhaustive posture site
  (§8) extended, riding the S1 register-ahead-of-emission pattern
  (ADR-0049) within the slice's own commits; §5's shared-machinery
  wiring with the annotate parity test extended to the new id; §6's
  adoption fixtures (intervening-comment silence, blank-line and
  trailing adopt-forward inheritance, declaration-statement
  inertness); §9's transparency fixture.
- **Breadth (issue #95)** — §7's comma list, one diagnostic per
  variable in source order; the remaining placement and nesting-depth
  edge fixtures; `unknown`-per-variable independence.
- **Docs seal (issue #96)** — the user-facing documentation and
  type-specification catch-up, sealing the surface as built.

## Amendment (2026-08-02): emission tracks the walk's coverage

The breadth slice (issue #95) pinned two postures §5's dump-parity
principle decides but the original text did not spell out. Both are
parity holding as **silence-parity**, and both are recorded so the
fixtures that pin them read as intent, not accident:

- **A dead statement answers nothing.** The walk proves a
  post-terminator region dead and never enters it; a `dumpType` at the
  mirror position is equally silent. The annotation asks the walk's
  question, and a question about a point the walk proves unreachable
  has no answering walk state — for either spelling.
- **Emission is gated by the walk's construct coverage.** A `while`
  body or `try`/`catch` interior is still opaque to the walk
  (ADR-0027's ratchet), so neither the annotation nor a mirror
  `dumpType` emits there today. §6's adoption promise is unaffected —
  the per-file query associates at any depth — but an adopted tag only
  answers where the walk walks. The parity fixtures hold the two
  spellings together, so when a loop or try lowering lands, both
  surfaces light up in the same commit, or the fixtures fail.

Neither posture is a refusal: both are the current coverage honestly
stated, and both widen automatically with the walk.
