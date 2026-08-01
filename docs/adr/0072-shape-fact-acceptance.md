# The acceptance relation's third face: a shape fact judged against a contract

Named as the next step by ADR-0071 §2.2. Status: PENDING ratification
(autonomous design under the owner's post-hoc-ratification mode).

## 1. Context: the last `Maybe` row, and why it is now load-bearing

`admits_fact(ty, Fact::Shape { … })` answers the honest `Maybe`, with a
comment dating it: judging a shape fact against a contract was "the
acceptance-convergence slice's work, not this one". That deferral was
free while shape facts were rare. Two changes made it a cost. ADR-0062
put a shape fact on every constant array a walk can see, and the #81
line's floor seeds one for every single-array-arm functionMap row — so
the fact now regularly stands on one side of a `@param`/`@return`
check whose other side is an array contract, and the check shrugs.

The relation's three faces, for the record: `shape_verdict` is
type-vs-value (ADR-0062 S0), `subsumes_array` is type-vs-type
(ADR-0071), and this ADR is type-vs-**abstract-fact** — the for-all
judgment over everything a [`ShapeFact`] admits. No fourth face exists:
every consumer reduces to one of these.

## 2. The inverted hazard: here, a wrong `No` is a finding

`subsumes`' dangerous direction was a wrong `Yes` (unsound collapse).
This relation's consumers invert that: three of the four `admits_fact`
call sites fire a contract-layer finding **on `No`** (the
`phpdoc.param-mismatch` / `return-mismatch` family), and `Yes` merely
silences. The posture is therefore stated with the emphasis reversed:

* **`No` only with a witness** — a member of the fact's denotation the
  contract provably rejects, with the two exact lemmas below doing most
  of the work. Every entry-shaped witness is gated on the entry being
  realizable (a `None` value slot realizes as anything; a field with
  [`Presence::Absent`] realizes never).
* **`Yes` only with a coverage argument** (it feeds the or-fold seeding
  site and future dedup surfaces, not findings).
* **`Maybe` is the floor**, and unknowns (`None` value slots, an
  untyped unsealed tail, `is_list == Maybe`) fall there rather than to
  either pole.

Two lemmas are exact for this carrier, not conservative:

1. **The fact admits `[]`** iff it has no `Required` field and
   `non_empty` is false. (`Optional`/`Absent` fields and any tail
   force nothing.)
2. **`is_list` is denotational and trinary** (RFC #14939): `Yes` — every
   member is a list; `No` — none is; `Maybe` — unknown. It is never
   recomputed here from the key set (the ADR-0062 A-G lesson).

## 3. The rules, per contract arm

`covers_ne(a)` abbreviates `a.non_empty ⇒ ¬(fact admits [])` via lemma 1.
Field/tail value slots recurse through `admits_fact`; key literals
through `admits_val`; `KeyClass` maps onto the key contract by its three
cases.

| contract `a` | verdict |
|---|---|
| `Mixed`, `MixedMinus(Null)`, `Opaque` | Yes / Yes / Maybe |
| `MixedMinus(Falsy)` | Yes iff fact cannot admit `[]`; else No (lemma 1 is exact both ways) |
| `Never` | No (a shape fact's denotation is never empty: value slots are `Fact`s, and no `Fact` is uninhabited) |
| scalar bases, literals, `Null`, `IntIn`, `StrWith`, `StrOpaque`, `Class`, `ObjectAny` | No |
| `CallableTy` | No when `closure_only`; else Maybe (a pair-array may be callable) |
| `ArrayAny{ne}` | Yes iff `covers_ne`; else No (`[]` witness) |
| `ListOf{T, ne}` | `is_list = No` → No; `Maybe` → Maybe; `Yes` → every Required/Optional field's value fact ⊆ T **and** the tail's value ⊆ T (Sealed contributes nothing; `None` slots → Maybe), and `covers_ne` |
| `MapOf{K, V, ne, nl}` | every non-`Absent` field: key literal ⊆ K and value fact ⊆ V; tail `Unsealed{kc, v}`: `kc` ⊆ K and `v` ⊆ V (`None` → Maybe unless V is `Mixed`); `covers_ne`; `nl` demands `is_list = No` (else Maybe, or No when `is_list = Yes` and the denotation is nonvacuous) |
| `IterableOf{K, V}` | as `MapOf` without `ne`/`nl` — the fact denotes arrays only, all of which `iterable` covers when K/V do |
| `Shape` (contract) | field-wise, below |
| `Union` | or-fold; a fold ending at No degrades to Maybe unless every member is array-incapable (the ADR-0071 §2 haircut verbatim — same joint-cover argument, same witness sharing) |
| `Inter` | and-fold (sound both directions) |

**Contract shape vs fact shape**, the structural heart:

* every **required contract field** must be guaranteed: a fact field at
  `Required` with value fact ⊆ field ty (`None` slot → Maybe). A fact
  field at `Optional` → No (the member lacking the key is the witness);
  `Absent` or未declared-with-sealed-tail → No; undeclared with an
  unsealed tail admitting the key → No is NOT provable (the tail says
  *may*, not *must*) — Maybe.
* every fact field at `Required`/`Optional` must land in the contract:
  same-key field (value fact ⊆ ty), else the contract's typed tail
  (key literal ⊆ tailK, value fact ⊆ tailV), else unsealed-untyped
  (anything), else **sealed → No** — the member carrying the key is the
  witness, and for an `Optional` fact field that member is still in the
  denotation, so the witness stands.
* the fact's tail vs the contract's extra surface: fact `Sealed` →
  nothing to cover; fact `Unsealed{kc, v}` vs contract sealed → No
  (members with an undeclared key exist unless `kc` is uninhabited —
  it never is); vs typed tail → `kc` ⊆ tailK and `v` ⊆ tailV
  (`None` → Maybe); vs unsealed-untyped → covered.
* contract `list` flag → demands `is_list = Yes` (else Maybe/No by the
  trinary); `covers_ne` as everywhere.
* **`covers` (disjunctive presence, A-G8) is deliberately not
  consulted** in v1: ignoring it only widens toward `Maybe`, never
  toward a wrong pole. A future sharpening can use it to discharge
  required-field obligations disjunctively.

## 4. Measurement discipline: this slice moves finding counts on purpose

New `No` verdicts fire `phpdoc.*` findings wherever an array-bearing
argument or return meets an incompatible declared contract — an array
literal passed to `@param string`, a `list<int>` returned against
`@return array<string, int>`. These are contract-layer,
measurement-mode findings (fp-gate's EXACT tables), so the slice ships
as a **conscious baseline move**, not a green-preserving change:

1. land the relation with its unit vectors;
2. run fp-gate; every moved package gets verbatim triage (5+ samples)
   — the expectation is TRUE declared-contract violations, the same
   class the tables already hold;
3. reseed the `phpdoc.*` tables in the same slice with a dated comment
   citing this ADR (the reseed-in-its-own-pass rule is about drift;
   a designed unlock reseeds with its cause);
4. nsrt is expected to gain and must lose nothing (set-diff LOST 0);
5. any triage sample that is NOT a true violation is a stop-the-line
   defect in the rules above — the FP identity outranks the unlock.

## 5. Amendment (2026-08-01): as-built ratification — the `No` is disjointness

The implementing slice found §3's table written in the wrong idiom, and
the correction is ratified here as the design.

**`admits_fact`'s `No` means *disjoint* — every value the fact admits is
rejected — not "a witness escapes".** The consumers document exactly
that contract ("only a definite `No` reports"), and the scalar rows
already implement it: `int<0, 5>` against a fact admitting `0` and `7`
is `Maybe`, although a witness escapes. §2's "`No` only with a witness"
is necessary, never sufficient; several §3 rows read it as sufficient,
and implementing them literally would have fired `param-mismatch` on
`array $a` against `@param non-empty-array` — the §4.5 stop-the-line
class, caught before landing. The corrected rows, each at the sound
verdict:

| §3 row as written | ratified |
|---|---|
| `ArrayAny{ne}` / `MixedMinus(Falsy)`: "else No" | `Maybe`; `No` only when the fact admits *only* `[]` |
| required contract field vs `Optional` fact field → No | `Maybe`, unless the value obligation is *also* `No` (then every realization fails one way or the other — genuine disjointness) |
| sealed contract vs `Optional` fact field → No | `Maybe` (the member *with* the key may be the one that violates nothing else) |
| sealed contract vs unsealed fact tail → No | `Maybe` (`Unsealed` says *may*, not *must*) |

Rows that survive as written: `Never`, the array-incapable arms,
`is_list = No` vs `ListOf`, `not_list` vs `is_list = Yes`,
`Absent`-or-sealed vs a required contract field, a `Required` fact
field vs a sealed contract, and `Inter`.

**The union haircut is REMOVED from this relation** (a ratified
strengthening, not a deviation): disjointness is member-wise exact —
a union rejects a value iff every member does, so an or-fold ending at
`No` *is* the proof, with no shared-witness argument needed. ADR-0071's
haircut answers a coverage question, which is not member-wise; importing
it here cost true positives (`string|list<int>` against a
definitely-keyed fact is a genuine, now-firing disjointness). The
jointly-covering case it protected needs no protection: the
`non-empty-array` member answers `Maybe` from its own corrected row,
and `Maybe` survives the fold.

Also ratified from the slice: lemma 1 is *computed* as
`ShapeFact::admits(&[])` rather than restated (it additionally sees
`is_list = No` and non-empty `covers`, both of which make refutation
rarer); a provably uninhabited shape answers `Maybe` (the vacuity
guard — `normalize` does not guarantee nonemptiness, so `Never`'s row
argument needed the guard); `Fact::Shape` travels `Asserted` and every
consumer already accepts that stratum — no new stratum rule. Recorded
residual, same class as ADR-0071's `denotes_nothing` note: a
`covers`-bearing shape whose covered keys are all `Absent` is
uninhabited and escapes the guard. The slice's definitional oracle
(probe each fact with concrete arrays it admits; `admits_val` must
agree) caught one genuine FP in the fact-tail rule before landing —
the oracle pair is the reviewer this table keeps.

## 6. Refusals

* **No `Fact::Shape` vs callable-signature refinement** — the
  pair-array case stays Maybe; judging `[$obj, 'method']` shapes
  against `CallableTy` signatures is variance work, not acceptance.
* **No `covers` consultation** (§3) — precision deferral, zero
  soundness cost.
* **No new carrier and no fourth face** — the rules live in `admit.rs`
  beside `admits_val`/`admits_fact`, dispatching on the existing
  carriers only.
