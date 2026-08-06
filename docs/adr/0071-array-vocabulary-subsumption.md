# The acceptance relation learns the array vocabulary

Follow-up named by ADR-0069's 2026-08-01 amendment. Status: PENDING
ratification (autonomous design under the owner's post-hoc-ratification
mode).

## 1. Context: one vacuum, two populations

`normalize::subsumes` — the single pairwise arm relation everything
else reduces to (ADR-0052 §4) — answers the honest `Maybe` for every
arm of the array vocabulary (`ArrayAny`, `ListOf`, `MapOf`,
`IterableOf`, `Shape`). That was the right floor while nothing needed
more: `Maybe` never collapses an arm unsoundly, never deletes one
under subtraction, never admits a curated row it shouldn't.

ADR-0069's amendment turned the floor into a named blocker. The
functionMap mining countersign is `subsumes` verbatim — a row is
admitted only when it *bounds* the engine (`engine ⊆ row`) or
*refines* it arm-wise in both directions — and a relation that cannot
say `Yes` about `array{dirname: string}` under `array` makes the
countersign vacuous for the 388 shaped-array rows. The 620
object/callable/resource rows sit behind the same wall for a
different reason: `subsumes_class` is a reflexive floor, so only a
same-name row could ever countersign, and the carriability filter
(`arm_is_carriable`) refuses the whole vocabulary before the question
is even asked.

Two facts found while scoping bound the design:

* **`No` is load-bearing, not just `Yes`.** The closure-argument
  variance check (issue #11) raises a finding on
  `subsumes(...) == Certainty::No`. A wrong `No` is therefore a false
  positive, not merely lost precision. The existing scalar rules keep
  `No` proven; the new rules must too.
* **A union can cover jointly what no member covers alone.**
  `list|non-empty-array` covers `array` — every empty array is a
  list, every other array is non-empty — but each member alone is a
  proven non-cover. An or-fold over members would answer a wrong `No`
  here. The scalar side already documents this family of
  under-approximation (admit.rs's jointly-covering-union note); the
  array rules must degrade the union's `No` to `Maybe` unless every
  member rejects *for a base reason* (it can admit no array at all).

## 2. Decision

`subsumes` gains a structural denotation for the array vocabulary,
**in place** — no second relation, no parallel judge. Where `b`
denotes finitely describable structure, the rules are type-vs-type
mirrors of the structural laws `shape_verdict` already owns
(required/optional presence, sealing, tails, list-ness,
non-emptiness); leaf questions recurse through `subsumes` itself, so
scalar element types are judged by the same rules as scalar arms.
`shape_verdict` remains the only *type-vs-value* shape relation;
this ADR adds the *type-vs-type* face beside `subsumes_class`, which
is the precedent for a structural rule inside `subsumes`.

The soundness posture is unchanged and is restated as the review
gate for every rule:

* **`Yes` only when provable.** Every value in `b`'s denotation is
  admitted by `a`, by argument, with the empty array and the
  order-agnostic keyed-shape realization (#14939) explicitly
  considered.
* **`No` only when refutable.** A concrete witness family in `b`'s
  denotation that `a` rejects. The empty array is the most common
  witness (`non-empty-*` as `a`); a `Traversable` object is the
  witness that no array type covers `iterable`.
* **`Maybe` is the floor,** and the a-side union fold may *end* at
  `No` only when every member is array-incapable (admits no array
  value at all: scalars, literals, `null`, classes, `object`,
  closure-only callables). Otherwise a fold that reaches `No`
  degrades to `Maybe` — the joint-cover haircut.

### 2.1 The rules, per `b` arm

Notation: `⊆` below is `subsumes(outer, inner).is_yes()` recursion;
`ne(b)` means `b` *guarantees* non-emptiness (its `non_empty` flag,
or a shape with at least one required field). For our vocabulary
`¬ne(b)` is exact, not conservative: a `b` without the guarantee
admits `[]`, so `non-empty` `a`-side flags refute with a witness.
`covers_ne` means `a.non_empty ⇒ ne(b)`.

**`b = ArrayAny{ne}`** (all arrays, minus `[]` when `ne`):

| `a` | verdict |
|---|---|
| `Mixed`, `MixedMinus(Null)` | Yes |
| `MixedMinus(Falsy)` | Yes iff `ne`, else No (`[]` is falsy) |
| `ArrayAny` | Yes iff `covers_ne` |
| `MapOf{K,V,…}` | Yes iff `K ⊇ int\|string`, `V ⊇ mixed`, `¬not_list`, `covers_ne`; No never (Maybe otherwise) |
| `IterableOf{K,V}` | same key/value rule, Yes/Maybe |
| `ListOf` | No (`['a'=>1]` witness) |
| `Shape` | No if `list`, any required field, or the keys alone prove every realization a list (Amendment B, issue #169 — witness `['a' => 0]`); Yes iff unsealed-untyped-or-mixed-tail, all fields optional with `ty ⊇ mixed`, `covers_ne`; else Maybe |
| scalar / literal / `Null` / `IntIn` / `StrWith` / `StrOpaque` / `Class` / `ObjectAny` | No |
| `CallableTy` | No if `closure_only`, else Maybe (a pair-array may be callable) |
| `Opaque` | Maybe |

**`b = ListOf{T, ne}`** (keys exactly `0..n-1`):

* `ArrayAny`: Yes iff `covers_ne`.
* `ListOf{T', ne'}`: Yes iff `T' ⊇ T` and `covers_ne`; element
  verdicts compose by `and`, so a `Maybe` element stays `Maybe`.
* `MapOf{K,V,…}`: Yes iff `K ⊇ int<0, max>`, `V ⊇ T`, `¬not_list`
  (a list realization is exactly what `not_list` rejects — with the
  `[]` exception only when `ne(b)`), `covers_ne`.
* `IterableOf{K,V}`: Yes iff `K ⊇ int<0, max>`, `V ⊇ T`.
* `Shape`: Maybe (a positional shape covers only bounded lengths;
  no Yes rule worth its complexity), except No by the required-field
  / list-flag witnesses where they apply.
* Everything scalar-side: No; `Mixed`/`MixedMinus` as above
  (`MixedMinus(Falsy)`: Yes iff `ne`).

**`b = MapOf{K', V', ne', nl'}`**:

* `ArrayAny`: Yes iff `covers_ne`.
* `MapOf{K,V,ne,nl}`: Yes iff `K ⊇ K'`, `V ⊇ V'`, `covers_ne`, and
  `nl ⇒ nl'` (an `associative-array` `a` rejects list realizations
  `b` may admit — including `[]` when `¬ne(b)`).
* `IterableOf{K,V}`: Yes iff `K ⊇ K'`, `V ⊇ V'`.
* `ListOf`: Maybe in general (`b`'s denotation holds non-lists
  unless `K'` is degenerate); No when `nl'` (a `not_list` `b` still
  admits nothing `a` wants only if… it admits `[]`; keep Maybe when
  in doubt — rule: No iff `nl'` and `ne'`, witness = any admitted
  member, which is then a non-empty non-list).
* `Shape`: Maybe.
* Scalars: No; `Mixed`/`MixedMinus` as above.

**`b = IterableOf{K', V'}`** (arrays *plus* `Traversable` objects):

* `Mixed`, `MixedMinus(Null)`: Yes. `MixedMinus(Falsy)`: No (`[]`).
* `IterableOf{K,V}`: Yes iff `K ⊇ K'`, `V ⊇ V'`.
* Every array arm, `Shape` included: No — the `Traversable` witness.
* `ObjectAny`/`Class`: No — the array witness. Scalars: No.

**`b = Shape{list', fields', sealed', ne', tail'}`**:

* `ArrayAny{ne}`: Yes iff `covers_ne` (`ne(b)` = flag or a required
  field). This is the mining workhorse: `array ⊇ array{…}`.
* `MapOf{K,V,ne,nl}` / `IterableOf{K,V}`: Yes iff every field's key
  literal (`CKey::Int → LitInt`, `CKey::Str → LitStr`) `⊆ K`, every
  field `ty ⊆ V`, and the extra-entry surface is covered: `sealed'`,
  or a typed tail with `tailK ⊆ K` and `tailV ⊆ V` (untyped-unsealed
  `b` → Maybe unless `K ⊇ int|string` and `V ⊇ mixed`); plus
  `covers_ne`, and for `nl`: Yes only with a required string-keyed
  field in `b` (else the list/`[]` realizations refute or stay
  Maybe).
* `ListOf{T, ne}`: Yes iff `b` is proven all-list — `list'`, or the
  domain's denotational `is_list` answers Yes on `b`'s key skeleton
  (Amendment A, issue #161) — and every field `ty ⊆ T`, and
  `sealed'` or a typed tail with `tailV ⊆ T`; `covers_ne`. A keyed
  `b` whose realizations can hold two keys stays Maybe — its
  order-agnostic realizations (#14939) need not be lists even when
  the keys are `0..n-1`.
* `Shape{list, fields, sealed, ne, tail}`:
  * every **required** `a`-field must be guaranteed by `b`: a
    same-key `b`-field, required, with `b.ty ⊆ a.ty`; a `b`-optional
    same-key field or a missing one refutes (witness: the `b`-member
    without the key) → No.
  * every `b`-field (optional included) must land somewhere in `a`:
    same-key `a`-field with `b.ty ⊆ a.ty`; else `a`'s tail
    (`keyLit ⊆ tailK`, `b.ty ⊆ tailV`); else `a` unsealed-untyped
    (anything goes); else No when `a` is sealed (witness: the member
    with that key present), Maybe otherwise.
  * `b`'s extra-entry surface must be covered by `a`'s:
    `b` sealed → nothing to cover; `b` typed tail → `a`
    unsealed-untyped, or `a` typed tail with `b.tailK ⊆ a.tailK`,
    `b.tailV ⊆ a.tailV`, or No when `a` sealed; `b` untyped-unsealed
    → `a` unsealed-untyped, else No when `a` sealed, Maybe when `a`
    has a typed tail short of `int|string → mixed`.
  * flags: `a.list ⇒ b` proven all-list — `list'`, or the domain's
    denotational `is_list` answers Yes on `b`'s key skeleton
    (Amendment B, issue #169); a keyed `b` whose realizations can
    hold two keys stays Maybe (see above); `covers_ne` with `ne(b)`
    as defined.
* Scalars/classes: No. `Mixed`/`MixedMinus`: as `ArrayAny`'s row.
* Degenerate sealed empty shape (`array{}`): denotes exactly `[]`;
  the rules above already decide it (`ListOf ⊇ array{}` = Yes,
  `non-empty-anything ⊇ array{}` = No) — pin it in tests.

`a = Union` folds `or` across members with the §2 haircut; `a =
Inter` folds `and` (sound both directions). `b = Union`/`Inter`
dispatch before any of this, unchanged.

### 2.2 What deliberately does not change

* **`admits_fact`'s `Fact::Shape` row stays `Maybe`.** Judging a
  *shape fact* against a contract is still the acceptance-convergence
  work this ADR does not do; the floor's seeded facts stay silent at
  that seam, which is FP-safe. It is the named follow-up once the
  type-vs-type face proves out.
* **`Cover::subsumes`** (steins-domain) is the fact lane's own
  carrier and is untouched.
* **`StrOpaque`/provenance arms** keep their bar: never `Yes` on
  either side (ADR-0038).
* **Class arms** keep the reflexive floor. No hierarchy enters
  steins-contract.

### 2.3 The object rows ride the reflexive floor, not new rules

The 620-row bucket needs no new relation: a functionMap `GdFont`
against the engine's `GdFont` countersigns `Yes` *today* through
`subsumes_class`'s reflexive case. What refuses those rows is
`arm_is_carriable`. The mining slice widens carriability to
`Class`/`ObjectAny` (and their nullable unions) alongside the array
vocabulary; rows whose names differ from the engine's (the stale
pre-8.0 `resource` spellings) fail the countersign exactly as they
should, and stay counted. `callable`, intersections and `resource`
stay refused-and-counted: `resource` lowers outside the class
vocabulary, and a `CallableTy`/`Inter` countersign would still be
vacuous.

## 3. Consequences

* **Behavioral blast radius is real and gated.** Array arms become
  `arm_eq`-reflexive, so `dedup_arms` starts collapsing duplicate
  array spellings in the phpdoc/dump surfaces, and the
  stratified-arm keep logic in steins-infer follows; the
  non-reflexivity tripwire test must flip for array arms with this
  ADR as the reason. Referees: full native suite, fp-gate phpdoc
  536 EXACT, nsrt set-diff LOST 0.
* **The countersign stops being vacuous** for shaped rows; the
  mining slice re-runs at the recorded phpstan-src pin (`dcde2be6`)
  so the delta is the relation and nothing else, and all 47 recorded
  engine-disagreement catches must survive by name.
* **Floor seeding is already plumbed:** a single-array-arm row seeds
  the shape lane through `seed_shape_fact` exactly as a project
  `@return array{…}` does; multi-arm rows live in the arm lane and
  spell through `spell_arms`. No new seam.
* **A wrong `No` is now a checked risk**, not an unexamined one: the
  union haircut is the rule that keeps the closure-variance seam
  honest, and it gets its own adversarial tests
  (`list|non-empty-array ⊇ array` must not answer No).

## Amendment (2026-08-01): as-built sharpenings

The implementing slice held §2's posture and corrected §2.1's table
where its entries could not be argued. Each correction below replaces
the table's entry; the posture (§2) is unchanged and every change is
pinned by a test.

**Two laws subsume the `covers_ne` column.** Before any `a`-side
dispatch: (1) a `b` whose entry-bearing members provably do not exist
(`list<never>`, a required-`never` field) collapses the question to
`admits_val(a, [])` — an uninhabited `b` is subsumed by everything,
generalizing the `Never` row; (2) `admits_val(·, [])` is *exact* on
both sides of this vocabulary, so "`b` admits `[]` and `a` rejects it"
is a proven `No`. Non-emptiness is therefore *computed*, not flagged:
`associative-array<K,V> ⊉ array<K,V>` is a proven `No` because `[]` is
a list.

**Softened (the table licensed a `No` with no witness):** a typed-tail
shape `b` against a sealed shape `a` (and typed-tail-vs-typed-tail
mismatches) answers `Maybe`, not `No` — the tail's key *type* alone
does not prove a key outside `a`'s fields is admitted. The
untyped-unsealed case stays `No` (any key is free there).

**Strengthened (a witness existed where the table said `Maybe`, or the
rule was wrong):**

* `MapOf`-b vs `ListOf`-a: `No` whenever `b`'s key type admits a key
  that cannot begin a list (probes `1` and `"k"`). This *replaces* the
  table's `No iff nl' ∧ ne'`, which could not even answer the required
  `list<int> ⊉ array<int,int>`.
* `ArrayAny`-b / `ListOf`-b vs `MapOf{not_list}`-a: proven `No`
  (`[0 => v]`, or every-member-is-a-list). The `ArrayAny` row's "No
  never" is withdrawn.
* `MapOf`-b vs `MapOf{not_list}`-a: decided by `0 ∈ K'` (list
  realization provable / refuted / unknown), not the flat `nl ⇒ nl'`.
* Untyped-unsealed shape `b` vs `MapOf`-a: `No` via one concrete
  refused extra entry (keyed by the first undeclared int).
* `ListOf`-b vs sealed shape `a`: `No` — a list longer than every
  declared key escapes the seal.

**Recorded residual:** uninhabitedness detection covers `never` and
its algebraic closures only; an element type uninhabited for an
unmodeled reason (`int&string`) can make an entry witness vacuous — a
wrong `No` confined to seams where `No` is not a finding trigger (the
closure-variance seam excludes the vocabulary via `scalar_decidable`).
Documented at `denotes_nothing`; an inhabitation oracle is not worth
its weight today.

## Amendment A (2026-08-06): denotational list-acceptance (issue #161) — PENDING ratification

The `ListOf ⊇ Shape` row originally gated `Yes` on `list'` alone —
the **syntactic** flag, true only when the subject was *spelled*
with the `list` keyword. That made the verdict depend on which of
two legal spellings introduced the same denotation: a sealed
`array{null}` stayed `Maybe` where `list{null}` answered `Yes`,
although both denote exactly `{[0 => v]}`.

The gate is now denotational: `Yes`-eligibility holds when `list'`
**or** when the subject's key structure alone proves every
realization a list. The judgment is not re-derived in the contract
layer — `normalize::keys_prove_list` routes the subject's key
skeleton (keys, presence, sealing; value slots at the unknown
floor, which the judgment never reads) through the domain's own
`ShapeFact::normalize` and reads back its denotational `is_list`,
so list-ness keeps exactly one definition in the codebase (§3 of
ADR-0062, RFC #14939).

The direction of care is unchanged: under the order-agnostic key-set
model, the keys prove list-ness only when no permutation and no gap
is realizable — a sealed subject whose only possible key is `0`. A
keyed subject whose realizations can hold two keys stays `Maybe`
(`array{0: int, 1: string}` admits `[1 => 's', 0 => 1]`), and a
shape with optional keys below a required one admits a gapped
realization (`[0 => v, 2 => v]` fails `array_is_list`, measured),
so it stays `Maybe` too. Both are pinned in tests, alongside a
matrix that walks the same spellings through the acceptance
relation and the domain judgment so the routing cannot drift.

## Amendment B (2026-08-07): the remaining flag reads (issue #169) — PENDING ratification

Amendment A left two rows reading the syntactic `list` flag where
the denotational judgment belongs; issue #169 closes them with the
same bridge (`normalize::keys_prove_list`, deliberately unwidened:
`Yes` only for possible keys ⊆ {0} under a sealed tail — for n ≥ 2
the `list` keyword carries order information a key set does not).

* **`Shape ⊇ Shape`, the flags obligation.** A positional `a` no
  longer degrades a keys-prove-list `b` to `Maybe`: when the keys
  alone prove every realization of `b` a list, the flag mismatch is
  spelling, not denotation, and any `Yes` is still earned field by
  field by the presence/field/tail obligations. A `b` whose
  realizations can hold two keys or a gapped key set stays `Maybe`,
  pinned exactly as in Amendment A.
* **`ArrayAny ⊇ Shape`, a No-sharpening.** A keys-prove-list `a`
  spelled `array{…}` fell to `Maybe` where its `list{…}` twin
  answered `No`. Never-wrong-No demands a member witness, and it is
  the same one the positional case always had: `['a' => 0]` is a
  member of `array` — string-keyed and non-empty, so both `ne`
  flavors of `b` hold it — that no sealed key-`0`-only shape
  admits, because such an `a`'s possible keys are ⊆ {0} and no
  member of `a` carries the key `'a'`. The witness is exercised in
  a test through `admits_val` on both sides, not asserted by
  analogy with the flag case.

The one other `.list` read in the file (`entries_vs_shape`'s
untyped-unsealed branch) is a genuine spelling consumer, not a
stand-in for the denotational judgment: it guards the fresh-key
refutation on the tail's *sequencing* semantics, which the
`list{…}` keyword carries and a key set cannot, and
`keys_prove_list` is identically false under an open tail, so the
bridge has nothing to add there.
