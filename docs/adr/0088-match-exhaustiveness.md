# `match` exhaustiveness: two coverage grades, the defensive terminator, and the sentinel parameter

Issue #427. **Status: proposed (2026-08-18), PENDING ratification.** Drafted
under the owner's standing delegation, ahead of the slices it governs (#428
sentinel grade, #429 enum case domain, #430/#431 the `match` lowering, #432
`match.dead-default`, #433 the `\UnhandledMatchError` origin, #434 the pedantic
leg). No emitter ships with this ADR.

## 1. Context: one worked example, twenty-one cells

The design question arrived as a single PHP file whose functions each pin one
cell of a grid. Two axes generate it.

The first axis is the **premise grade** of the subject's type — ADR-0037's
stratum, read at a `match`:

- a native `string|int` parameter: the union is a **Verified** fact, enforced by
  the engine at the call boundary;
- a native `mixed` parameter with `/** @param string|int */`: the union is
  **Asserted** only, and the engine enforces nothing, so a value outside it
  genuinely arrives;
- a native `int` parameter with `/** @param 1|2 */`: both, layered — the engine
  enforces `int`, and the docblock refines within it. This is the interesting
  row, because the two grades disagree about the same variable;
- an enum-typed parameter: **Verified and finite**, the only such type PHP has.

The second axis is what the `match` does when no arm matches: no `default` at
all, a `default` that produces a value, or `default => assertNever($foo)` where

```php
/** @param never $value */
function assertNever(mixed $value): never { throw new LogicException(); }
```

PHPStan gives the same answer to almost every cell: `is_int($foo)` (or the
second literal arm) "will always evaluate to true". That answer is backwards in
the Verified rows — an exhaustive `match (true)` chain is the well-written
thing, and its last arm is redundant *by construction*; complaining about it
asks the author to make the code worse. And it is unsound in the Asserted rows,
where the claim rests on a docblock the runtime never checks. Meanwhile the two
things in the grid that a reader would actually want reported — a `default` arm
that provably cannot run, and a case analysis that has silently stopped being
exhaustive — PHPStan says nothing about at all.

Steins should report the complement of what PHPStan reports here. This ADR fixes
how.

## 2. Decision: the coverage verdict has two grades, and they are asked separately

For one `match`, "do the arms exhaust the subject?" is **two questions**, and
every finding in this area names which one it answers:

- **Verified coverage** — do the arms exhaust the domain the engine enforces?
  This is what decides whether the construct can throw `\UnhandledMatchError` at
  runtime, because the engine's enforcement is the only thing that constrains
  what actually arrives.
- **Asserted coverage** — do the arms exhaust the subject's most-refined
  *declared* domain: the docblock's refinement where there is one, the native
  declaration where there is not? This is what decides whether the author's own
  case analysis is complete on the author's own terms.

Verified coverage implies Asserted coverage is *not* the direction of
implication: the declared domain is a subset of the enforced one (an authoritative
envelope refines, never widens), so **Asserted coverage is the weaker claim and
Verified coverage the stronger**. Exhausting `1|2` says nothing about `int`.

The standing premise rules apply unchanged. A finding whose only premise is
Asserted may not reach a `type.*` id and may not sit on the proof layer
(ADR-0037 §2, ADR-0052's derivation clause). Nothing in this area is proof-layer
in any case: a `match` that fails to cover is a *reachable* break, not a proven
one, so the strongest home available is the contract layer.

## 3. Decision: a dead arm whose body only terminates is never a finding

A provably-dead arm whose body does nothing but **terminate** — `throw`, `exit`,
or a call to a function declared `: never` — is deliberate defensive code and is
never reported as dead, at either coverage grade. Name it the **defensive
terminator**.

ADR-0019 §2 already ruled the live half of this: "in the proof layer `exit` is a
*reachability input*, never a finding", and library code calling `exit` is
policy-profile material at most, on crying-wolf grounds. This ADR rules the dead
half the same way, and for a stronger reason. Writing a guard against a case the
type system already excludes is the conservative style Steins wants to
encourage: it is what keeps the code correct when the type later widens, when
the docblock turns out to have been a lie, or when the value crosses a boundary
the analyzer could not see. A tool that scolds it is teaching the author to
delete their own safety net.

This single carve-out is what silences every `default => assertNever($foo)` cell
in the grid, at every premise grade, without any special knowledge of
`assertNever` itself. The rule is structural: what the body *does*, not what it
is called.

The carve-out is about the **dead-code** finding only. It does not silence the
sentinel of §4 — that is the opposite finding, and it is asked of arms that are
*alive*.

## 4. Decision: `@param never` is a sentinel, and the rule it licenses is not about `match`

`throw new LogicException()` in a `default` arm and `assertNever($foo)` in a
`default` arm look alike and mean different things. The first is ordinary error
handling: the author is guarding a case they believe can happen. The second is a
**claim** — the author is saying no call reaches this point, and asking to be
told when that stops being true. The thing that carries the claim is the callee's
`/** @param never $value */`, because `never` is uninhabited: a parameter typed
`never` admits no argument at all, so writing a call to it is only coherent if
the call is unreachable.

So a `never`-declared parameter is the **sentinel parameter**, an explicit
opt-in, and the rule it licenses is:

> An argument passed to a `never`-declared parameter whose **most-refined
> declared type is non-empty on the current path** is a finding.

Three consequences follow, and all three are deliberate.

**It is not about `match`.** The same rule fires on an `if`/`elseif` chain, a
`switch`, or a bare call — anywhere a case analysis has narrowed a variable and
the sentinel says the residue should be empty. Its id must therefore not carry
`match` in its name.

**It asks at the Asserted grade.** The `mixed` + `/** @param string|int */` row
exhausts its declared domain, so the sentinel is silent there — even though at
runtime that `default` is genuinely reachable. That is not an oversight and not
an inconsistency with §2: the sentinel answers "is the author's case analysis
complete on the author's own terms", and the runtime reachability of the same
arm is answered by §5's throw origin, on a different surface, for a different
reader. Two questions about one arm, two answers, both true.

**It is contract-layer, always.** The sentinel is spelled in a docblock, so the
premise is Asserted by construction, and the finding takes a `phpdoc.` prefix. If
a native or attribute spelling is ever introduced, a Verified sibling joins it
then; there is none today.

**The residue must be a proven narrowing.** Added 2026-08-18, from the first
implementation's measurement (#428): reading "the declared domain is non-empty
here" off the arm lane is not sufficient, because a lane also sits at its full
seeded declaration when the guards *were* written and the lane simply cannot
model them. Measured, an exhaustive `enum` chain and an exhaustive
`$b === true` / `$b === false` chain both left the lane untouched and both
reported — two false positives of the same shape, from guard forms whose
subtraction is unimplemented rather than from any real reachability. So the rule
gains a second half: the sentinel reports only where the residue is non-empty
**and strictly smaller than what the declaration seeded**, i.e. where some
subtraction demonstrably landed on this path. An un-narrowed lane is ignorance,
not evidence, and the two are indistinguishable from inside the check.

The price is the unguarded call — `assertNever($foo)` with no case analysis above
it goes silent, though it is trivially reachable. That is the right trade: the id
exists for case analyses that have stopped being exhaustive, and buying the
weakest cell at the cost of a false-positive class is exactly the bargain the
crying-wolf prohibition forbids. The consequence is that this id's reach grows
with the narrowing vocabulary rather than ahead of it — the enum leg arrives when
#429 teaches the lane to subtract cases, not before.

The corollary is a carve-out in the other direction: a `never`-declared parameter
leaves `phpdoc.param-mismatch` entirely. That id says "this call site passes the
wrong thing" and its remedy is to fix the argument; the sentinel says "the case
analysis upstream of this call is incomplete" and its remedy is to handle a case.
One id must not carry two remedies (the ADR-0078 §1.4 discipline). And because
`never` is uninhabited, *every* argument to it is trivially a mismatch — the
acceptance relation answers `No` unconditionally — so the existing finding is
technically true and practically useless, and it currently fires as a false
positive on the `int` + `/** @param 1|2 */` row for exactly the grade reason §2
names.

## 5. Decision: an uncovered default-less `match` is a direct throw origin

A `match` with no `default` whose arms do not exhaust the subject's **Verified**
domain throws `\UnhandledMatchError` when the uncovered value arrives. That is a
throw like any other, and it enters the throw accounting as an `origin = direct`
contribution, propagating and damming exactly as a `throw` statement does.

No new id: `throw.undeclared` already asks this question at `Layer::Contract` /
`Floor::Contracts`. A bare `steins check` therefore stays quiet — an uncovered
`match` is reachable, not proven, and the crying-wolf constraint gives it no
claim on the default surface — while a project that has opted into throw
accounting learns that the function can throw something it does not declare.

The grade here is Verified precisely because the question is about runtime: what
the engine enforces at the boundary is the only thing that bounds what arrives.
A docblock refinement suppresses nothing, and believing it is the mistake this
finding exists to surface.

Enum subjects are where this pays: an enum `match` covering every case throws
nothing and is silent, and one that misses a case is a genuine undeclared
`\UnhandledMatchError` — which is also the reason the finite Verified case
domain (#429) is a prerequisite rather than a nicety.

## 6. Decision: no arm-condition truth diagnostic

Steins registers no `condition.*` family and emits nothing on the arm conditions
themselves. PHPStan's "always evaluate to true" has no Steins counterpart, in
either grade, and this is a permanent decision rather than an unimplemented one.

In the Verified rows the redundancy is the point: the last arm of an exhaustive
chain is redundant by construction, and the alternative the diagnostic pushes the
author toward — deleting the final `is_int($foo)` test, or replacing it with
`default` — makes the code less legible and less robust to a later widening of
the type. In the Asserted rows the claim rests on an unenforced docblock, and
reporting it is the `treatPhpDocTypesAsCertain` divergence already on the
registry (`docs/phpstan-divergences.md`), applied to arm conditions.

What Steins says instead is the complement: the `default` PHPStan is silent
about (§7), and the sentinel PHPStan cannot check (§4).

## 7. The ids

| id | layer | floor | says |
|---|---|---|---|
| `match.dead-default` | Mechanics | measured (#432) | the `default` cannot run, on Verified premises, and its body is not a defensive terminator |
| `phpdoc.dead-default` | Contract | `Pedantic` | the `default` cannot run *if the docblock is believed*, and its body is not a defensive terminator |
| `phpdoc.never-param-reachable` | Contract | `Contracts` | a sentinel parameter is reached with a declared type that is not empty |
| `throw.undeclared` (existing) | Contract | `Contracts` | §5's `\UnhandledMatchError`, as an `origin = direct` contribution |

The prefix split between the first two follows the **premise axis**, not the
syntactic one, on the `type.maybe-argument-mismatch` / `phpdoc.maybe-argument-mismatch`
precedent, where premise grade is exactly what separates two ids that answer the
same question. `match.dead-default` requires an all-Verified exhaustion; the
docblock-only claim may not reach it.

`match.dead-default`'s floor is **left to measurement** (#432, on the #35
precedent): measurement mode first, every hit triaged verbatim, the floor chosen
from the result. The open question the measurement answers is how much of the
population §3's carve-out removes. If what survives it is small and unambiguous,
`Floor::Default` is defensible on the `array.duplicate-key` precedent
(works-but-dead drift, not a runtime break); if defensive-but-not-terminating
`default` arms turn out to be common, the id belongs at `Floor::Contracts`. The
id is not registered until the triage justifies it.

`phpdoc.dead-default` sits at `Floor::Pedantic` — reached by no built-in rung —
because "my docblocks are my contract, tell me what they make unreachable" is a
house-style ask, the same question `untyped.class-constant` sits at that rung to
answer, and not the some-paths question `Strict` exists for.

## 8. The grid, resolved

Rows are the subject's premise grade; columns the no-match outcome. "silent"
means no finding on any built-in surface.

| subject | no `default` | value-producing `default` | `default => assertNever(…)` |
|---|---|---|---|
| native `string\|int`, arms cover both | silent (cannot throw) | `match.dead-default` | silent (§3) |
| `mixed` + `@param string\|int`, arms cover the docblock | `throw.undeclared` (§5) | `phpdoc.dead-default` (pedantic) | silent (§3) |
| `int` + `@param 1\|2`, arms cover `1` and `2` | `throw.undeclared` (§5) | `phpdoc.dead-default` (pedantic) | silent (§3) |
| enum, every case covered | silent (cannot throw) | `match.dead-default` | silent (§3) |
| enum, one case missed | `throw.undeclared` (§5) | silent (the `default` is live) | `phpdoc.never-param-reachable` (§4) |

The last row is the one the whole design is for: someone adds a case to an enum,
and every `match` that forgot it says so.

## 9. What this does not decide

- **The value of a `match` expression.** Slice #430 walks the arms of a
  value-position `match` for reachability and findings; joining the arm values
  into the expression's own type is a separate and separately valuable question,
  deliberately deferred.
- **`switch`.** The same verdicts apply in principle, but `switch` compares
  loosely and its arm truth sets are multi-valued, so its coverage question is
  not the one answered here. Out of scope until asked.
- **A native spelling of the sentinel.** `#[\Steins\Never]` or similar would put
  the claim in the Verified lane and license a `match.`-prefixed sibling. Not
  proposed; noted so that §4's "contract-layer, always" is read as a consequence
  of today's spelling and not as a law.
- **Whether `match.dead-default` ships at all.** §7 leaves that to the
  measurement.

## Note (2026-08-18): the no-match path is the keystone, and today's silence is accidental (issue #439)

Measured while landing value-position `match` (#430): every verdict this ADR
defines is a question about the **no-match path**, and `walk_match` refines that
path with nothing. It narrows the subject *inside* a conditional arm and leaves
the `default` arm reading the subject exactly as it arrived.

That has a consequence worth stating plainly, because it makes a rule above look
like it is working when it is not. `default => assertNever($foo)` is silent today
**because the sentinel's proven-narrowing gate (§4) declines on an untouched
lane** — not because §3's defensive terminator recognized anything. The right
answer is arriving for the wrong reason. Once the arms are subtracted on the
no-match path, the lane will empty, the gate will pass, and §3 will have to carry
the weight it is currently being credited with.

The same gap is why none of §7's ids can be built yet: "the arms exhausted the
Verified domain" *is* the no-match lane being empty, and "this `match` can throw
`\UnhandledMatchError`" is that lane being non-empty. §8's grid describes
outcomes that the machinery cannot currently distinguish. Issue #439 closes it,
for `match` and `switch` together, and it blocks #432, #433 and #434.

It is also where the enum leg (#429) meets `match`: covering every case subtracts
to empty and is silent; missing one leaves exactly the missing case and reports.
That pair is the finding this whole run exists to produce, and it does not exist
until the subtraction does.

## Note (2026-08-18): the keystone landed, and §4 gains an all-conditions clause (issue #439)

The no-match path now subtracts (ADR-0052's note of the same date). Three
consequences for this ADR.

**§4's silence has moved to the right place.** `default => assertNever($foo)` over
an exhausted declared domain is still silent, and it is now silent because the
arms subtracted the domain to nothing and there is no residue to report — the
reason §4 states. The note above predicted the gate would start passing; measured,
it does, and the emptied lane is what answers instead. §3's defensive terminator
was never carrying this and still is not: it governs the dead-*arm* findings of §7,
none of which are registered yet.

**§4 gains a clause.** The proven-narrowing rule was written against a single
guard, where "a subtraction landed" and "this path's narrowing is modelled" say the
same thing. A `match` is a chain of subtractions at once and the mark is one bit
per variable, so a chain where only *some* conditions landed would set it and offer
a residue that is ignorance about the rest. The rule therefore reads: the residue
is evidence only where **every** condition of every arm subtracted something. The
casualty is the chain mixing a modelled guard form with an unmodelled one; the
alternative was a manufactured finding, which ADR-0002 forbids outright.

**§7's ids are unblocked, and §9's `switch` line is now specific.** "The arms
exhausted the Verified domain" is readable (an emptied lane), and so is "this
`match` can throw" (a non-empty one), for the shapes the arm lane can model.
`switch` subtracts too, but its residue's *non-emptiness* proves nothing — loose
`==` consumes an infinite, unspellable set around each literal — so §9's "its
coverage question is not the one answered here" now has an operational reading:
`switch` buys silence and may never buy a finding. Every id in §7 that reports on a
non-empty residue is `match`-only until that changes.

One prerequisite is still missing, and it turns out to be §5's rather than §4's. A
class-constant arm condition keeps the whole construct opaque, so an enum `match`
is not structured at all and the `EnumCase` subtrahend wired into the no-match path
sits idle. Lifting that refusal experimentally does produce §8's last row — every
case covered goes silent, one case missed reports exactly the missing case, and the
public corpus does not move — but it also structures the *rest* of the construct,
and the idiomatic exhaustive enum `match` has no `default`. Every one of them would
begin surfacing an `\UnhandledMatchError` origin under §5, which §5 gates on
Verified coverage: the exhaustion check that the empty lane now makes *possible*
and nobody has *built*. So the enum row and §5's gate are one piece of work, not
two, and #431 should land them together.

## Note (2026-08-19): §5 lands, and the gate's actual shape (issue #433)

The class-constant refusal is lifted and §5's throw contribution ships together,
as the note above required. The gate reads `subtract_no_match_path`'s residue on
the subject's `Store::contract` lane exactly the way §4's sentinel reads the same
lane for the opposite verdict: `Store::contract_narrowed` says a real subtraction
landed (an untouched lane, or one only a `switch`'s over-approximate residue
touched, never counts); `Store::contract_emptied` says it landed on nothing —
true only for a lane that was all-`Verified` when it emptied, because
`subtract_contract_lane` drops a lane that empties with any surviving-`Asserted`
history to *absent* rather than *kept-empty*. That absence-on-taint is what makes
the layered worked-example row behave: `int` native narrowed by `@param 1|2`,
matched exactly on `1` and `2`, still reports — the lane was Asserted-tainted, so
its emptying proves nothing about `int`, and the gate reads that correctly without
any special case for docblocks at all.

Reaching `throw.undeclared` needed one more thing this ADR did not anticipate:
`UnhandledMatchError` extends `Error`, which ADR-0007 keeps unchecked by default,
and that default would have silenced every contribution this section exists to
make. The gate carries one class-specific carve-out in `emit_undeclared` —
`UnhandledMatchError` is checked, but only where this walk's own verdict proved
the specific construct live. ADR-0007's rationale is what licenses the exception:
the unchecked default exists because the proof layer is supposed to own `Error`,
by proving the throwing branch dead, and an uncovered `match` is precisely the
shape the proof layer has nothing to prove dead — coverage failure already
establishes the branch is live.

Two scope gaps, both in the safe (false-negative) direction, neither a §5
violation because §5 asks about `StmtKind::Match`, and neither shape reaches it as
one:

* **`match (true)` guard chains never reach the gate.** `lower_match_guard_chain`
  desugars a default-less `match (true) { is_int($x) => …, … }` straight into
  `StmtKind::If` with no `else` — the shape §1's own worked example uses for its
  exhaustive `string|int` row. An `If` with no `else` falls through silently in
  this walk; nothing marks it as a `match` that would have thrown. Measured: a
  non-exhaustive guard chain with no `default` — a genuine runtime
  `\UnhandledMatchError` — reports nothing, on a release build, at
  `--profile contracts`. Silent is the safe direction, but it means every native
  premise-grade row §1 opens with is currently unreachable by this gate; only the
  by-value and enum forms are covered. Left for a follow-up, since closing it
  means teaching `walk_if` that some `If`s are match-shaped.
* **A `try`/`catch` around the construct dams it unconditionally**, regardless of
  the caught type. The dataflow walk never structures a `try` body at all (it is
  `StmtKind::Opaque` end to end), so a `match` written inside one is invisible to
  this gate no matter what the `catch` clause names — `catch (\LogicException $e)`
  dams an uncovered enum `match` exactly as `catch (\UnhandledMatchError $e)`
  would. The structural throw scan's own guard-stack tracking (`scan_throw_origins`)
  is type-aware and would get this right if the dataflow verdict reached it; it
  currently cannot, because the verdict is computed on a walk that never enters
  the `try` body in the first place.

## Note (2026-08-19): §7's measurement ran, and `match.dead-default` is not registered (issue #432)

§7 left this id's floor to measurement rather than taste, and named the question
the measurement would answer: *how much of the population does §3's carve-out
remove?* It ran over both corpora — **100,359 files, 1,066 written `default`
arms** — and the answer is **all of it**. Full record and verbatim triage in
`docs/notes/20260819-match-dead-default-measurement.md`.

**Three provably-dead `default` arms exist in that thousand.** All three are
`switch` (§9 defers it), all three hold on `Asserted` premises alone (§2 refuses
them), and all three are a bare `throw` (§3 carves them out). Each gate removes
the whole population on its own. So **`match.dead-default` would report nothing,
and it is not registered** — §7's own last sentence applied to §7's own
measurement. All three triaged TRUE; there were no false positives to fix.

**§7's floor question stays open, not settled.** With a yield of zero there is no
triage to choose between `Floor::Default` and `Floor::Contracts`, and picking one
anyway would be exactly the taste call §7 wrote itself to avoid.
`REGISTERED_NOT_YET_EMITTED` is not the home either: that list is for ids whose
emitter is coming, and this one's emitter was built, run, and found nothing.

**#434 inherits a measured population of zero.** `phpdoc.dead-default` owns
exactly the Asserted grade these three sit at — and §7 gives it the same
terminator carve-out, so it reports nothing on them either. The two ids together
are silent on every provably-dead `default` arm in both corpora. Recorded here so
the next person does not measure it a second time.

**The carve-out was aimed at the right shape.** §3 was written ahead of any
measurement, from one worked example, and predicted that a dead `default` under
an exhaustive chain would be a guard rather than a value. Three for three, it is
— one of them annotated by its own author as *"有り得ないケース"*, "impossible
case". And it removes them on §3's *letter*, the sole-terminator reading: the
cell where the letter ("does nothing but terminate") and the rationale (a safety
net is a safety net) disagree did not occur once, so that disagreement is real
but not load-bearing, and settling it can wait for a case that exhibits it.

**The reach limit is the arm lane's, and it is what to re-measure.**
`Store::contract` is seeded from declared parameters and declared returns, so a
`match` on a local, on a property, or in a poisoned scope has no lane to empty
however exhaustive its arms are — all three hits are `switch ($parameter)`. When
ADR-0052's queued property leg or its return-summary leg lands, this measurement
should be re-run rather than trusted.

**The measurement machinery itself does not ship** (owner ruling, 2026-08-19).
With no id registered it emits nothing and finds nothing, and reaching the
guard-chain spelling had cost a syntactic-provenance bit on `StmtKind::If` —
which ADR-0031 deliberately keeps off that variant, and which issue #448 answers
without touching, off the CST. Two mechanisms for one provenance question is
worse than one, and re-measurement is gated on a distant change that will want to
build on whichever mechanism exists then. What survives is the conclusion above
and the method in the note.

One repair from the measurement **did** ship, because it is independently
correct: the type-predicate vocabulary was dropping an emptied all-`Verified`
lane where the value-subtrahend path keeps it, so §8's *first* row — a native
`string|int` exhausted by `is_string`/`is_int` — read as absence rather than
emptiness, which is the opposite claim. ADR-0052's note of the same date has it.
That fix is also what issue #448 met from the other side: its guard-chain throw
gate hit the same asymmetry and worked around it at its own call site by reading
`Store::contract_arms(..).is_some()`. Once the source repair lands, that
workaround is unnecessary and slightly lossy — `contract_arms` collapses *absent*
with *kept-empty*, and §5 wants to report on the first (the absence-on-taint
mechanism the 2026-08-19 note above credits for §8's layered row), so the
guard-chain spelling would go silent where its by-value twin reports. Measured on
#448's branch: the layered row reports by value and is silent through
`match (true)`.
