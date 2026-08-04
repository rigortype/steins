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
   witness equivalence of two evaluation orders. Declared-lane facts
   neither qualify nor disqualify; only the proven lane and the taint bit
   are consulted.
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
