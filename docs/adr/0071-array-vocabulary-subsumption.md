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
| `Shape` | No if `list` or any required field; Yes iff unsealed-untyped-or-mixed-tail, all fields optional with `ty ⊇ mixed`, `covers_ne`; else Maybe |
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
* `ListOf{T, ne}`: Yes iff `list'`, every field `ty ⊆ T`, and
  `sealed'` or a typed tail with `tailV ⊆ T`; `covers_ne`. A keyed
  (`¬list'`) `b` stays Maybe — its order-agnostic realizations
  (#14939) need not be lists even when the keys are `0..n-1`.
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
  * flags: `a.list ⇒ b` must be positional (`list'`) — a keyed `b`
    stays Maybe (see above); `covers_ne` with `ne(b)` as defined.
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
