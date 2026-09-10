# The value domain grows one bool literal; objects are spelled in the contract lane, not the value domain

**Status: proposed (2026-09-10), PENDING ratification.** Drafted from the
owner's grilled design session of 2026-09-10 over issues #600 and #618,
in post-hoc-ratification mode (ADR-0077 precedent). This ADR settles
*where the domain may grow* for two walls that several queued slices
decline on: the missing spelling of `bool` minus `false` in the value
lane (#600), and the absence of any object denotation in the value
domain (#618). It amends nothing in ADR-0035 (the four layers stand),
ADR-0052 (the two lanes stand) or ADR-0059 (the Lean posture stands); it
draws one line each side of them.

## 1. Context: two walls, one neighbourhood

`Fact::Union` (issue #339) carries one `(Base, Option<Refinement>)` arm
per scalar base, `Base` has four inhabitants
(`crates/steins-domain/src/value.rs`) and `Refinement` has two
(`crates/steins-domain/src/fact.rs`): `Str(StrPreds)` and
`Int(IntRange)`. Two consequences, both measured on the 2026-09-02
unknown-fall recount (`target/nsrt/analysis-20260901/recount-e731986.md`):

- **`bool` has no refinement**, so `string|bool` under
  `assert($v !== false)` keeps its whole `bool` arm. The arm lane already
  narrows `bool` to `true` under `!== false` (ADR-0052's note of
  2026-08-19, issue #443); the value lane cannot spell the result. Two of
  PR #592's fp-gate seeds (`phpunit` `Util/PHP/JobRunner.php:244`) are
  this: a guarded `$stdout`/`$stderr` still reports
  `phpdoc.maybe-argument-mismatch`. The corpus-wide `|false` heuristic is
  237 rows.
- **No `Val` and no `Fact` is an object** — the specification says so in
  one sentence (`docs/type-specification/value-domain.md`: "No object
  values. Objects live in the heap store, keyed by allocation identity.").
  So a builtin whose answer is `DateTime|false`, `'foo'|stdClass` or a
  bare `DateTimeInterface` has no answer at all: 81 func-call rows
  directly, at least 131 corpus-wide by a floor scan, the ~245-row
  builtin class-method return table behind them (ADR-0069 §5 and
  ADR-0071 §2.3 defer it for exactly this reason), and the 474
  object/class-name/callable/resource functionMap rows the miner already
  counts as excluded (`crates/steins-catalog/src/declared_returns_generated.rs`).

The two walls sit in the same `UnionArm` neighbourhood and were filed
as design questions because every candidate fix touches either the
domain algebra — `join`, `admits`, widening, the array-key cast grid,
subsumption — or the Lean spec that ADR-0059 binds to it, or both.
Issue #339 is the precedent for how such a change actually lands: Rust
first, Lean second, and the second half is where it stalled. Issue #345
is the standing cost of one recorded divergence.

## 2. Decision: the bool base carries a literal member set, and nothing else grows

The value domain grows in exactly one place. A `Fact::Union` arm whose
base is `Bool` may carry its inhabitants as a **literal member set**,
`{true}` or `{false}`, alongside the existing `None` meaning the whole
base. No new `Refinement` variant is introduced.

The reason is arithmetic, not taste: `bool` has two inhabitants, so
"`bool` minus `false`" is the base-level `OneOf {true}`, and every
operation the algebra must define is a set operation over a two-point
domain. `join({true}, {false}) = bool`, `admits` is membership,
widening is the whole base, subsumption is subset, and the array-key
cast grid already maps `true → 1` and `false → 0`. Adding
`Refinement::Bool(bool)` instead would grow the `Refinement` inductive,
and with it every case split in the Rust algebra and every proof over
`Refinement` in `spike/lean-domain`, to express a relation that the
existing `OneOf` layer expresses with no new vocabulary. ADR-0052's
note of 2026-08-19 already reads `Base::Bool` as a two-point domain on
the arm lane; this decision gives the value lane the same reading.

Consequences the implementer owes:

- The `Fact::Union` invariants (arms sorted by base, one arm per base,
  `2..=4` arms) are unchanged. A bool arm with a full member set
  normalizes to the bare base; an empty member set is the emptied lane
  and follows ADR-0052's emptied-Verified rule, never a silent drop.
- The `!== false` / `!== true` subtraction lands in the value lane with
  the stratum of the fact it narrows, mirroring the arm-lane rule of
  #443. `=== false` on the positive branch is keep-only and stays with
  `Refine::Exact`, as today.
- The Lean spec grows in lockstep in the **same PR**, and `lean-check`
  green is a merge gate for that PR. This is the ADR-0059 posture
  applied, not an exception to it: the change is small enough that
  deferring the proof would cost more in a recorded divergence than the
  proof costs. `join_comm` (pinned in `Axioms.lean`) must survive the
  new arm's normalization.

## 3. Decision: an object is spelled in the contract lane, and the value domain does not learn about classes

Three shapes were on the table for #618 and the owner chose the third.

**A rejected: an arm carrying class identity.** It answers the 81 rows
directly and it is the wrong place. `Fact::join(DateTime,
DateTimeImmutable)` needs an is-a oracle; the oracle lives in
`steins-infer` (`ProjectIsa`) with the project index behind it, and
`steins-domain` is a self-contained algebra precisely because it does
not. Either the domain carries an opaque name and answers `Maybe` for
every class-vs-class question — making `join` partial in a new place —
or the oracle is injected and the algebra stops being one. The Lean
spec would have to model, or explicitly abstract over, a relation it
has no source of truth for. That is a larger version of the #345
question, and #345 has been open since #339 landed.

**B deferred: an opaque object arm.** "Some object", no identity. Cheap
and total, but it wins none of the rows: `DateTime|false` would spell
`object|false`, which is *wider* than the oracle's answer, and the nsrt
harness deliberately refuses to launder the reverse direction as
subsumption (`xtask/src/nsrt.rs`,
`reverse_direction_is_never_subsumption`). An opaque arm may still be
wanted as the thing a guard subtracts from and as `ContractTy::ObjectAny`'s
value-side twin; that case is not this ADR's and is not made here.

**C adopted: grow the spelling, not the domain.** The contract lane
already represents every one of these types: `ContractTy::Class(name)`
and `ContractTy::LitBool(false)` are live variants
(`crates/steins-contract/src/lib.rs`), and a *pure* class/null arm list
already renders (`crates/steins-infer/src/dump.rs`). What cannot render
is a **mixed** class-and-scalar list: `spell_arms` returns `None` on a
`Class` arm (`crates/steins-contract/src/spell.rs`), and
`render_contract_arms`'s fallback returns `None` on a `LitBool` arm. So
`DateTime|false` is representable today and unspellable by two
functions. The guard side already works: the arm lane strips the
`false` arm of a `T|false` row under `!== false`
(`crates/steins-infer/src/refine.rs`, `apply_class_narrowing`'s doc,
point 3), so `if ($d === false) { return; } $d->format(...)` resolves
through the declared-receiver lane (ADR-0049 §8 / S6) with no value-lane
object at all. Candidate C has no Lean obligation, because ADR-0059's
spec covers the value domain and not the contract lane, and it is the
only candidate that reaches the builtin class-method table without a
domain change.

### 3.1 What may put an object arm into the contract lane, and at what grade

Candidate C raises the question it displaces: the arm lane is a
*declared* carrier ("a subtraction carrier by construction",
`refine.rs`), and a transfer rule's computed answer is not a declaration.
The ruling is the narrow one:

- An object arm may enter the contract lane only when it is **sourced
  from a declaration**: a builtin's declared return (the functionMap /
  stub lineage ADR-0069 §4 names), or the contract arms of an argument
  the rule projects through, or the exact class of a heap object an
  argument denotes (`new Analyser` in `filter_var`'s `default` option
  is this case — the heap's exact object reads as the contract arm
  `Class(Analyser)`).
- Its grade is `Asserted`, unchanged from ADR-0069 §2: "every row seeds
  `Asserted`, never `Verified`", so the proof layer's all-Verified
  premise rule excludes it from every finding by construction, and the
  dump surface renders it with the `(asserted)` marker.
- No third lane is opened. A general "computed answers as contract
  arms" carrier, with per-rule strata, was the alternative and is
  declined: it would make ADR-0052 §1's two-layer reading of declared
  versus proven a three-layer one, and this issue does not earn that.

### 3.2 What this decision does not reach

`min($dates)` and `array_pop($objects)` want a **bare** class, and their
argument is an array *of objects*; `Val::Array` holds `Val` and no `Val`
is an object, so those rows (8 of the 81 in #618's table) stay where
they are under C. Projecting the element's contract arm out of a
declared `array<DateTimeInterface>` would be the C-shaped answer, but
it makes a transfer rule take contract arms as *input*, which is half a
step into the third lane §3.1 declined. Whether an object has a value
denotation at all is filed separately and is not ruled here; #618's
target population is stated as 73, not 81.

## 4. Consequences

- #600 becomes a bounded slice: the bool member set in `steins-domain`,
  its Lean twin in `spike/lean-domain`, the `!== false` value-lane
  subtraction, and the two fp-gate seeds unwound — one PR, lean-check
  green as its gate.
- #618 becomes a spelling slice: `spell_arms` and `render_contract_arms`
  learn mixed lists, the ADR-0069 §5 / ADR-0071 §2.3 deferral of
  object-returning functionMap rows lifts under §3.1's sourcing rule,
  and the builtin class-method return table (filed separately, blocked
  on #618) can be built on the same carrier.
- The value domain's "no object values" sentence in
  `docs/type-specification/value-domain.md` stands verbatim. ADR-0057
  A3's asymmetry — "the value domain has no object top to degrade to" —
  stands too; the contract lane is where an object degrades.
- ADR-0059's scope sentence should be read as confirmed rather than
  narrowed: the spec covers the value domain; a contract-lane change
  carries no proof obligation, and this ADR is the first to lean on that
  boundary deliberately.

## 5. Related

- #600, #618 (the two issues ruled), #339 (the union layer and its Lean
  stall), #345 (the standing divergence), #443 (bool as a two-point
  domain on the arm lane), #557 (whole-arm truthiness subtraction).
- ADR-0035 (four layers), ADR-0052 (two lanes, subtraction), ADR-0057
  A3, ADR-0059 (Lean posture), ADR-0069 (Asserted floor and its object
  half), ADR-0071 §2.3.
