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
