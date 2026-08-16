# The `unset` pseudo-type: a phpdoc union member that says the variable may not be there

Issue #395. **Status: proposed (2026-08-16), PENDING ratification.** Drafted
under the owner's standing delegation, recorded with the vocabulary slice it
governs. The semantics of §4 are owner-approved (2026-08-16) and ship in issue
#396; §5's position question is open.

Parent: zonuexe/php-typing-conformance#7, the cross-tool measurement of the
spelling. Prior art: PHPantom-dev/phpantom_lsp#366, where the same docblock
reports "Class 'unset' not found".

## 1. The spelling, and what every tool does with it

`/** @var \DateTime|unset $x */` is the Blade-view and included-partial idiom:
the template is handed `$x` by whatever included it, so `$x` is either a
`\DateTime` or **the variable is not defined at all**. The union's second member
is not a type of value. It is a statement about the binding.

The conformance suite measured what the tools resolve it to. Every one but
Intelephense reads `unset` as a **class name**: the spelling is absent from the
docblock keyword table, falls through to the named-object atom, and is
**reported as an unknown class** — PHPStan (`class.notFound`: "PHPDoc tag @var
… contains unknown class …\unset"), Psalm, Mago, PHPantom (issue #366), Qodana,
php.py and NoVerify all say so out loud. Intelephense is the one that keeps the
spelling, and rejects the union as non-object rather than resolving it. Two
silences in the measurement are not readings: mir's was the harness's — its
diagnostic is info severity, which the adapter used to request on `debug_*`
files only — and Steins' was its own catch-all's, below. The **grammar** is a
separate question and agrees everywhere: phpstan/phpdoc-parser parses
`\DateTime|unset` without complaint, because the spelling is a well-formed
union of identifiers. What each tool then *resolves* the identifier to is where
this ADR sits.

Reading `unset` as the undefined state and reading it as a nullable union are
both honest interpretations; resolving it to a class is the one plainly wrong
reading, because no value of any class ever satisfies the member the author
wrote.

Steins was in the wrong column. `steins-phpdoc` has no keyword table — a bare
identifier is a bare identifier — and `lower_identifier`'s catch-all lowers an
unrecognized name to `ContractTy::Class(norm)`, so `unset` became a class named
`unset` in the current namespace. Nothing observable broke, but only by
accident: Steins deliberately emits no unknown-class-in-phpdoc diagnostic, and a
nonexistent-class arm answers `Maybe` for every object value. The silence was
the right output from the wrong reading — the shape ADR-0056 §8 already named
once for `resource`, where "the class catch-all's `No` was right for the wrong
reason".

## 2. Decision: `unset` is vocabulary, and it carries no value

1. **`unset` lowers to its own `ContractTy::Unset` leaf.** In a union
   (`T|unset`, `T|null|unset`) the member is the possibly-undefined pseudo-type,
   never a class. It contributes **no value** to the type lane: it is not
   `null`, not `void`, not `never`, not `mixed`. For contract acceptance the
   arms of `\DateTime|unset` are exactly the arms of `\DateTime`.

2. **`unset` is non-shadowable.** It is a reserved PHP *language construct*
   (`unset()`), so `class unset {}` does not parse and no class in scope can
   outrank the spelling. It joins the native type words in
   `is_shadowable_pseudo_type`'s non-shadowable list, unlike every phpdoc
   pseudo-type (`integer`, `number`, `closure`, …) that is also a legal class
   name.

3. **The spelling round-trips.** Lowering `\DateTime|unset` and spelling it back
   yields `\DateTime|unset`; re-lowering that is the identity.

4. **A bare `@var unset $x` is accepted and states no envelope.** It lowers to
   an empty value-arm list, which every consumer already reads as "no envelope,
   seed nothing" (ADR-0029). It casts nothing, reports nothing, and panics
   nowhere. What such a declaration *means* is §4's question, not this slice's.

5. **This slice emits nothing.** No new diagnostic id, no `steins.toml` or CLI
   surface, no change to any existing finding.

### Why a dedicated variant and not the opaque floor

`KNOWN_UNENFORCED` is the established parking lot for known-but-unmodeled
vocabulary (`int-mask<…>`, `properties-of<T>`, `template-type<…>`): those names
floor to `ContractTy::Opaque` precisely to avoid the class catch-all's
manufactured `No`. `unset` is refused that floor for two reasons.

- **`Opaque` spells back as `mixed`.** The speller's `Opaque` arm emits
  `"mixed"`, so `\DateTime|unset` would round-trip as `\DateTime|mixed` — a
  strictly *wider* type, written into the user's own docblock by `transform` and
  `annotate`. Decision 3 would be unsatisfiable. The word has to survive.
- **`unset` is not an unmodeled type.** Every `KNOWN_UNENFORCED` name denotes
  some set of values Steins has not modeled yet; the honest answer about them is
  "undecided". `unset` denotes no set of values at all, in any future slice. Its
  content is about the binding, which is a different domain from the one
  `ContractTy` inhabits.

### Where the member is dropped: carried through lowering, filtered at the arms

Two designs reach decision 1. Either the union lowering strips the member and
records a flag beside the arm list, or the member is carried as a `ContractTy`
and the value lane filters it. **The variant is carried, and dropped at one
boundary: `flatten_arms`**, the single place in `steins-infer` where a declared
arm list is built from a lowered contract.

The forcing reason is decision 3: a flag on the arm list is not reachable from
the speller, which takes a `&ContractTy` and nothing else. Stripping at lowering
would put the word somewhere the round-trip cannot see it.

Filtering at `flatten_arms` gives the property decision 1 asks for in its
strongest form — **structural equality**, not merely equivalent behavior:
`@var \DateTime|unset $x` produces the same `Vec<ContractArm>` as
`@var \DateTime $x`, so no downstream reader — the shape seed, the stratum rule,
the arm speller, the narrowing lane, the dump surface — learns the variant
exists. The filter runs *before* the "empty arm list ⇒ no envelope" check, so a
bare `unset` reaches that existing outcome and the remaining arms of a mixed
union are untouched.

The leaves that a caller could still reach directly floor honestly rather than
being left to a `todo!()`:

| Reader | `Unset` answers | Why not the alternative |
| --- | --- | --- |
| `admits_val` / `admits_fact` / `base_only` | `Maybe` | `Never`'s `No` would convict every value a bare-`unset` variable holds. |
| `normalize::subsumes` (as `b`) | `Maybe` | An empty *value* denotation makes `a ⊇ unset` a free `Yes` — a claim about a member no value inhabits. |
| `to_fact` / `to_shape_fact` | `None` | It states no value-slot truth; `None` only ever widens. |
| `spell_nested` | `"unset"` | The round trip (decision 3). |
| `spell_arms` | refused | It spells summarized *value* sets, which never contain the member. |

`Maybe` at the acceptance leaves is defense, not the mechanism: the arm filter
means acceptance is not asked. Both together make the slice unable to emit.

## 3. Consequences accepted

- **A Steins-only spelling.** PHPStan reads `unset` as a class, so a docblock
  Steins writes carrying the word is a phantom class reference to PHPStan. This
  is the same cost the refined-string grid already accepts by owner ruling
  (2026-08-08): Steins' vocabulary is Steins' (ADR-0030). Registered as
  divergence-registry core entry 15.
- **`ContractTy` grows a variant that is not a type.** Five exhaustive matches
  in `steins-contract` and one each in `steins-infer` and `xtask` were extended;
  every other reader has a catch-all that already lands on an honest floor. The
  compiler is the completeness check, which is why the variant was preferred to
  a bool on the union: a flag has no such check.
- **The word is silent for now.** A reader of a `T|unset` variable is reported
  by nothing this slice adds, and the union behaves exactly as `T`. That is the
  status quo's *behavior* with the reading corrected underneath it — the
  correction is a precondition for §4, not a finding.

## 4. The semantics the tracer bullet will implement (issue #396)

Owner-approved 2026-08-16, recorded here so the vocabulary above is not a
decision in isolation.

1. **The claim.** A **top-level** inline `@var T|unset $x` states that reads of
   `$x` may find no binding. The `unset` member is read as the undefined state,
   not as a nullable widening of `T`: `\DateTime|unset` says nothing about `$x`
   holding `null`.
2. **Discharge.** `isset($x)`, `empty($x)`, `$x ?? …`, `$x ??= …` and an
   assignment to `$x` all discharge the state. Inside the guarded branch the
   undefined path is gone and the guarded reads are silent — which is why the
   conformance fixture's `isset()` block carries `Q` lines, not `E?` ones.
3. **The guard is never redundant — a constraint recorded ahead of the id that
   would need it.** Steins has **no redundant-`isset` diagnostic today**: no
   `isset.*`, always-true or redundant-condition id is registered, so nothing
   can report the guard now and #396 adds nothing that could. What #396 pins is
   the *property*: a `T|unset` variable's `isset($x)` produces nothing. A future
   redundancy id inherits the constraint rather than discovering it — the
   guard's `Maybe` presence has to reach whatever judgment that id makes, or the
   family would arrive reporting a guard the declaration makes meaningful. The
   question of a pointless-guard id was already deferred once, when
   `variable.maybe-undefined` shipped (ADR-0081) — mechanics there would be
   un-disableable against defensive house styles, so the shape gets measured
   before it gets a name. This is a second input to that measurement. Worth
   stating here precisely because it is not enforceable by a test yet: there is
   no id to hold to it, only this record.
4. **A new id: `phpdoc.maybe-undefined`,** `Layer::Contract`, `Floor::Contracts`.
   Deliberately **not** `variable.maybe-undefined` (ADR-0081): that id is
   `Layer::Proof`, `Floor::Strict`, and its premise is a reachability fact the
   lowering pass computes from the CST. This claim's premise is a *declaration* —
   an author's assertion, unverifiable by definition — so it belongs on the
   contract layer with the rest of the phpdoc-premised family, and reports one
   surface lower because a declared possibly-undefined read is a stated fact
   rather than an inferred one. Sharing ADR-0081's id would put an Asserted
   premise behind a proof-layer id, which the layer split exists to prevent.
5. **The two claims meet, they do not merge.** ADR-0081's pass and this
   declaration both talk about binding presence. A variable declared `T|unset`
   and *also* only conditionally bound is one read with two premises at
   different grades; the tracer bullet decides the ordering, and the ADR-0081
   pass is not modified by this slice.

## 5. Open item: positions other than an inline top-level `@var` (issue #397)

Undecided, and deliberately not decided here. The vocabulary of §2 applies
wherever the phpdoc grammar reaches, because lowering is position-blind: `@param
\DateTime|unset $x`, `@return T|unset`, a property `@var T|unset` and a
function-scope inline `@var T|unset` all lower without a phantom class arm
today. What none of them has is a *meaning*, and the candidates are not
obviously the same in each position:

- `@param T|unset` could mean an optional parameter, a by-reference out
  parameter that may stay unwritten, or nothing at all — PHP has native syntax
  for optionality, so the spelling may be a redundancy to refuse.
- `@return T|unset` has no obvious reading: a function either returns a value or
  does not return.
- A property `@var T|unset` is arguably the uninitialized-typed-property state,
  which is a native PHP concept with its own errors.
- A function-scope inline `@var T|unset` is the closest to §4's case, but a
  function's locals are bound by its own body rather than by an includer, so the
  claim has a different justification.

Until #397 decides, these positions carry the corrected vocabulary and no
semantics: the member is dropped from the value arms and nothing is reported.
Silence is the safe residue, and it is the same silence they have today.

## 6. Alternatives considered

- **Read `unset` as `null`.** The nullable interpretation is one the conformance
  issue calls honest, and it would need no new variant. Refused: it manufactures
  a claim the author did not make. `\DateTime|unset` would then accept `null`,
  and a `$x === null` comparison would become meaningful — a narrowing built on
  a guess. The undefined state and the null value are distinct in PHP, and
  ADR-0081 already models the distinction on the proof side.
- **Read `unset` as `never` (an empty member, silently dropped).** Arithmetically
  appealing — a member no value inhabits is exactly `never` in a value-set
  reading, and `never` is already absorbed out of unions. Refused: `never`
  spells back as `never`, so the round trip would rewrite the author's docblock
  to `\DateTime|never`, and the `No` leaf would convict every value a bare
  `@var unset $x` variable holds.
- **Leave the class reading and add an unknown-class-in-phpdoc diagnostic.** The
  measured tools' behavior, and it would at least make the wrong reading
  visible. Refused twice over: the diagnostic is a standing refusal of its own
  (a phpdoc class reference is not a proven break), and the reading would still
  be wrong — an author writing the idiom correctly would be reported.

## 7. Verification

- Lowering: `unset`, `UNSET`, `\unset` all reach the leaf; no `Class("unset")`
  arm survives anywhere in `unset`, `\DateTime|unset`, `int|null|unset`,
  `array<int, unset>`.
- The acceptance criterion, as structural equality of the filtered arms against
  the union without the member, for `\DateTime|unset`, `int|unset`,
  `\DateTime|null|unset`, `unset|int`.
- Non-shadowability, against `integer` as the shadowable control.
- The round trip, `lower → spell → lower`, pinned as the identity.
- Through the cast lane (ADR-0073): `T|unset` casts what `T` casts, at the same
  stratum and with the same shape seed; a bare `unset` casts nothing and reaches
  the same dump an un-tagged variable does.
- The conformance fixture `regressions_unset_pseudo_type.php` stays silent under
  `--profile strict`, its `// V` controls included.
