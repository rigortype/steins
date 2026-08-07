# Loop-to-array_map: the first effect-preconditioned transform

Issue #116. Status: PENDING ratification (autonomous design under the
owner's post-hoc-ratification mode, per the ADR-0063/0067 precedent).
Context: ADR-0010 names loop→`array_map` the flagship transform whose
precondition no rule-driven codemod can express; ADR-0034 supplies the
skeleton (EditPlan, code preconditions, dual verification); ADR-0008 the
effect lattice the precondition is spelled in; ADR-0067 the proven/declared
lane split this ADR leans on.

## 1. The v1 shape

Exactly one loop form is eligible:

```php
$out = [];
foreach ($xs as $x) {
    $out[] = f($x);
}
```

- The subject is a plain variable. The binding is by-value, no key form.
- The body is a single statement, an append `$out[] = <expr>;` whose
  expression may use `$x` and enclosing-scope reads freely.
- The accumulator's `$out = [];` initializer is the statement immediately
  preceding the loop, and the rewrite consumes both statements.
- The accumulator does not occur in the body except as the append target,
  and the iteration variable does not occur anywhere after the loop.

Everything else is a named refusal (§4). Conservatism over yield is the
deliberate ADR-0002 posture the issue asks for: v1 transforms only the
shape whose equivalence is provable line by line, and the completeness
oracle's counts make the narrowness visible instead of silent.

## 2. The precondition: proven purity, and strictly more than `Pure`

The body must meet a bar the engine *proves*; a declaration never
qualifies:

1. **Proven lane empty, all labels** — no `output`, `io`, `global.read`,
   `global.write`, `nondet`, `mutate`, `exit` in the body's proven effect
   set, and the exhaustiveness bit intact: any unresolved call in the body
   (opaque receiver, dynamic callee, unanalyzable target) refuses. Every
   called declaration must have a proven-empty summary through the effect
   fixpoint.
2. **Declared purity is not sufficient** (ADR-0067): a `≤` bound imported
   from an envelope is a cap, not an occurrence proof, and a cap cannot
   witness equivalence of two evaluation orders. The declared lane must be
   **empty**, and a non-empty one refuses with the bound named.

   *Amendment (2026-08-05, issue #116 implementation).* This clause first
   read "declared-lane facts neither qualify nor disqualify; only the
   proven lane and the taint bit are consulted", and that was not a
   sufficient specification of the bar — it silently assumed the taint bit
   still reports what the declared lane answers. It does not: ADR-0067
   decision 3 **discharges a covered call site's taint**, precisely because
   the bound is the value of the import. So a body whose only unresolved
   call is answered by an envelope presents as `exhaustive` with an empty
   proven lane — provably pure to a reader of those two signals alone,
   while its purity rests entirely on a declaration. Consulting "the proven
   lane and the taint bit" would therefore have let a declared bound act as
   proof, which is the lane collapse ADR-0067 exists to prevent. The bar is
   restated: **the proven lane empty, the declared lane empty, and both
   exhaustiveness bits intact.** The declared lane must be reported to this
   transform separately and never merged into the proven one.
3. **The proven throw set must be empty — stricter than `Pure`.**
   ADR-0006's `Pure` admits `throw`; this transform must not. If the body
   throws on element `k`, the `foreach` leaves `$out` holding the first
   `k` results — observable state in every enclosing `catch` — while the
   rewritten form leaves `$out` unassigned. All-or-nothing assignment is
   the rewrite's own semantics and cannot reproduce partial accumulation.

## 3. The rewrite and its parity obligations

```php
$out = array_map(fn ($x) => <expr>, $xs);
```

- **List-ness of the subject is load-bearing.** `array_map` with a single
  array *preserves keys* (`array_map($f, ['a' => 1]) === ['a' => $f(1)]`),
  while the append renumbers to `0..n-1`. The subject's value fact must
  prove `is_list = Yes`; anything weaker refuses. Wrapping the subject in
  `array_values(...)` instead is rejected (§6).
- **The subject must prove `array`.** `foreach` iterates any `Traversable`;
  `array_map` TypeErrors on one. The subject's type fact must prove a
  plain array.
- **Arrow-function capture is safe exactly because of §2.** `fn` captures
  enclosing reads by value at call time; a body with proven-empty `mutate`
  and `global.*` cannot observe the difference between by-value capture
  and the loop's direct scope access. No parameter type is written on the
  arrow function — inventing one could fail at runtime on inputs the
  engine never saw.
- **Iteration-variable liveness.** `foreach` leaks `$x` (last element)
  into the scope; the arrow function does not. Any occurrence of `$x`
  after the loop refuses. The v1 check is textual occurrence in the
  remainder of the scope — sound in the refusing direction.
- **Initializer adjacency.** Replacing loop-only would strand a dead
  `$out = [];` and, worse, an accumulator proven empty *distantly* invites
  reasoning about every path between init and loop. v1 demands the
  adjacent-initializer spelling and replaces both statements with one.

## 4. Refusal taxonomy

Candidates are **every `foreach` statement** in the analyzed set; each is
transformed or refused with exactly one named, stable reason — the
completeness oracle's accounting (ADR-0034 §3). v1 reasons:

`body-not-single-append`, `key-binding`, `reference-binding`,
`subject-not-variable`, `subject-not-proven-array`,
`subject-not-proven-list`, `accumulator-init-not-adjacent`,
`accumulator-not-empty`, `accumulator-read-in-body`,
`iteration-var-live-after`, `early-exit` (break/continue/return/goto),
`body-effects {…}` (the proven labels found, named in the refusal),
`body-throws {…}`, `body-call-unresolved`.

## 5. Verification

- **Differential fixture**: the flagship rewrite is pinned by executing
  both spellings under the real PHP (the sidecar's pinned binary) and
  comparing outputs — behavior identity is measured, not argued.
- **Post-check**: `--apply` holds ADR-0034's zero-new-diagnostics gate on
  the default surface, matching the existing transforms.
- **fp-gate and nsrt untouched**: the transform never runs inside `check`.

## 6. Considered and rejected

- **`array_values($xs)` wrapping to lift the list requirement.** Adds a
  call the source never wrote and widens the diff beyond the provable
  minimum; a non-list subject is a refusal v1 reports, not a shape it
  quietly launders.
- **`array_merge($out, array_map(...))` for a non-empty accumulator.**
  Compound rewrite, compound proof obligation; later slice if the counts
  demand it.
- **A `function () use (...)` closure instead of `fn`.** The `use` list
  is a second thing to prove and to get wrong; by-value arrow capture is
  safe under the purity bar, and shorter.
- **Admitting proven-throwing bodies.** Partial accumulation is
  observable in `catch`; equivalence fails on a reachable path (§2.3).
- **Qualifying on declared purity (`≤` bounds).** ADR-0067 built the lane
  wall precisely so bounds cannot manufacture proof; a transform consuming
  bounds as proof would re-collapse the lanes at the first consumer.
- **Enumerating only append-shaped loops.** Hides the narrowness the
  oracle exists to expose; every `foreach` counts, so the refusal
  distribution is the roadmap for v2.

## Amendment (2026-08-07, issue #175): an opt-in admits an Asserted subject, labeled at the site

Issue #145's measurement across ~26.5k `foreach` candidates in 11 trees
(ten public packages plus the locally-configured project) settled what the
§4 histogram is for: 26,549 candidates, 592 loops passing every structural
check, 591 of those dying at the subject gate, exactly 1 transforming. The
binding cell is `subject-not-proven-array` (591 with the list cell), and
`subject-not-proven-list`'s zero is a **check-order artifact**, not
evidence: the `array` proof runs first, so a subject with no Verified shape
fact at all — a parameter, an `@param`-annotated variable, a call result, a
loop under an Opaque enclosing construct — never reaches the list check.
~94.8% of all refusals are structural shape (`key-binding`,
`body-not-single-append`, `subject-not-variable`, `early-exit`) that no
subject-gate policy touches, so the ~592 structural survivors, not the
26.5k, are the ceiling of any admission change. And the purity cells
(`body-effects`, `body-throws`, `body-call-unresolved`) never fired in the
field: the subject gate starves them, because purity is consulted only
after a subject qualifies.

**The proof-only default stands.** A v2 opt-in admits an **Asserted**
subject under three conditions:

1. **Explicit opt-in, never default.** `steins transform loop-to-array-map
   --asserted-subjects` on the command line, and the same-named boolean
   argument of the MCP `plan_transform` tool. Without it the gate is
   byte-identical to §3's proven-only reading, pinned by test. This is a
   per-run policy flag, deliberately *not* an ADR-0046 §2 vouch entry: a
   vouch exempts named `file:line` obstacle sites from a caller-enumeration
   question this transform never asks (it consumes no vouches at all),
   while this opt-in is one trust decision about the whole run's admission
   bar. One mechanism, and the flag is it.
2. **Admission requires the Asserted evidence to prove BOTH halves.** The
   subject's declared type must establish `array` AND list-ness at the
   Asserted stratum. A docblock `list<T>` (with or without a native `array`
   hint) qualifies. A bare native `array $xs` does NOT — the native
   lowering represents no `array` member at all (ADR-0002 silence), so it
   is evidence at *neither* stratum and still refuses
   `subject-not-proven-array`. A declared `array` / `array<K, V>`
   establishes the array half only and refuses `subject-not-proven-list`
   with the declared-evidence detail. `array_values(...)` wrapping stays
   rejected (§6): a non-list subject is a refusal to report, not a shape to
   launder. The unlock sits exactly where the modernization story wants it:
   annotate, then transform.
3. **The plan labels each admitted site's trust in its own output.** An
   admitted-under-opt-in site carries, in its own report entry (text,
   `--format json`, and the MCP plan document alike), that the subject's
   list-ness is *declared rather than proven*, and the concrete risk: if
   the claim is wrong — the value is actually string-keyed or gapped — the
   rewrite changes behavior, because `array_map` preserves keys where the
   append renumbered them `0..n-1`. The post-check cannot catch a wrong
   list claim (both spellings type-check; only the keys differ), and the
   label says so rather than implying otherwise. The approve step is the
   human gate.

Nothing else moves. Every structural gate (§1), the whole purity bar (§2)
and the remaining parity gates (§3) are unchanged; the probe's proven lane
is untouched, and an Asserted fact never masquerades as Verified — the
`SubjectFact` stratum split is the seam, and the opt-in reads its
unverified side instead of collapsing the two. The completeness oracle
stays exact (`enumerated == transformed + refused`) and gains a
`transformed_asserted` count, so a re-measure can report the proven yield
and the opted-in yield as the separate numbers they are. Under the opt-in
the purity cells become load-bearing for the first time: some fraction of
the ~591 subject-gate refusals will now refuse on effects, throws, or
unresolved calls instead — those zeros were starvation, never absence, and
the next reading of the histogram should expect them to move.

**Rejected alternative, recorded: closing as proof-only.** A yield of
1/26,549 leaves the flagship effect-preconditioned transform inert in
exactly the legacy-codebase setting ADR-0010 built it for, while every
unsoundness the opt-in accepts is *visible* — a behavioral key-numbering
change reviewed in a diff under an explicit flag with a per-site label —
never silent. Also recorded for the next measurement: the list cell's zero
is the check-order artifact above, and the 591 mixes probe-answered
negatives with subjects that never received a probe at all (the Opaque
enclosing-construct lowering), a split the histogram as shipped cannot
report.
