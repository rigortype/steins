# The `unset` pseudo-type: a phpdoc union member that says the variable may not be there

Issue #395. **Status: proposed (2026-08-16), PENDING ratification.** Drafted
under the owner's standing delegation, recorded with the vocabulary slice it
governs. The semantics of §4 are owner-approved (2026-08-16) and ship in issue
#396; §5's position question is **decided** (2026-08-16, issue #397), amended by
§9.

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

- **A Steins-only spelling.** PHPStan *reports* `unset` as an unknown class, so
  a docblock Steins writes carrying the word does not merely fail to lower
  elsewhere — it is a finding there, and in the five other tools that resolve it
  the same way. This is the cost the refined-string grid already accepts by
  owner ruling (2026-08-08), reached by the same route: a word PHPStan has no
  keyword for is a class reference to it. Steins' vocabulary is Steins'
  (ADR-0030). Registered as divergence-registry core entry 15. Reading a
  PHPStan-shaped annotation is unaffected — only what Steins writes diverges.
- **`ContractTy` grows a variant that is not a type.** Five exhaustive matches
  in `steins-contract` and one each in `steins-infer` and `xtask` were extended;
  every other reader has a catch-all that already lands on an honest floor. The
  compiler is the completeness check, which is why the variant was preferred to
  a bool on the union: a flag has no such check.
- **The word is silent for now.** A reader of a `T|unset` variable is reported
  by nothing this slice adds, and the union behaves exactly as `T`. That is the
  status quo's *behavior* with the reading corrected underneath it — the
  correction is a precondition for §4, not a finding.

## 4. The semantics of the tracer bullet — **implemented** (issue #396)

Owner-approved 2026-08-16, recorded here so the vocabulary above is not a
decision in isolation. **Landed with issue #396**; §8 amends it with what
building it forced.

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

## 5. Positions other than an inline top-level `@var` — **decided** (issue #397)

**Decided 2026-08-16 with issue #397**, resolving what this section previously
left open. The ruling: **the spelling is accepted everywhere, and the definedness
semantics of §4 attach to exactly one position.**

1. **Spelling, everywhere.** Lowering is position-blind, so `@param
   \DateTime|unset $d`, `@return T|unset`, a property `@var T|unset`, an inline
   `@var T|unset $this->p` and a function-scope inline `@var T|unset $x` all
   reach `ContractTy::Unset` — never a class, never a type-resolution finding.
   `// T: unset` means the same thing in every position, which is what makes the
   conformance measurement comparable across them.

2. **Semantics, only on an inline `@var` naming a top-level local.** Everywhere
   else the member is **inert**: dropped from the value arms, seeding no presence
   claim, adding no diagnostic. One line per position for why "undefined" has no
   subject there:

   | Position | Reading | Why inert |
   | --- | --- | --- |
   | `@param T\|unset $d` | inert | A parameter is always bound by the call; PHP already has native syntax for optionality, so the member would restate it or contradict it. |
   | `@return T\|unset` | inert | A function returns a value or does not return; there is no third outcome for the member to name. |
   | property `@var T\|unset` | inert | An uninitialized *typed* property is a native PHP concept with its own errors, and an untyped one is `null`; neither is a missing binding. |
   | inline `@var T\|unset $this->p` | inert | The tag speaks about a property slot, which exists whenever the object does. |
   | function-scope inline `@var T\|unset $x` | inert *for this id* | A function's locals are bound by its own body, so the CST already decides presence — see 3. |
   | nested (`array<int, unset>`) | inert | It speaks about an array's values, not about whether the variable is bound. |

3. **A function scope keeps the proof-layer pair, unchanged.** A never-bound
   local still reports `variable.undefined` and a conditionally-bound one still
   reports `variable.maybe-undefined` at `strict`; the docblock neither
   manufactures a binding nor silences a proof. This holds in a plain function, a
   method and a closure body alike. An arrow function's body is an expression, so
   a statement-adjacent inline `@var` sits in the enclosing scope and the body
   keeps its documented silence — the member changes nothing there either.

4. **Inert is two-sided, and the second side needed a fix.** "No new finding" was
   already true. "No *lost* finding" was not: `Unset`'s acceptance leaf answers
   `Maybe` (§2's table), and the union folds that judge a lowered or parsed type
   directly — rather than through `flatten_arms` — folded it in, where a `Maybe`
   swallows a sibling's `No`. `f(1)` against `@param \DateTime|unset $d` went
   silent, as did a violating `@return` and a violating property assignment. The
   fold now skips the member in `accepts`'s phpdoc-AST union (`steins-infer`) and
   in `steins-contract`'s three acceptance unions; a union with no value member
   left keeps the bare-`unset` floor. §9 records this.

5. **`transform`/`annotate` never promote the member away.** `phpdoc-to-native`
   refuses a `T|unset` `@param` as `type-not-natively-representable`, which is
   the behavior it already had and the correct one: no native declaration can
   spell "the argument may not be there", so rewriting `@param int|unset $x` to
   `int $x` would delete the author's own statement and emit a declaration
   stricter than the docblock it replaced.

6. **Deferred, not refused: a function-scope `T|unset` that participates in the
   emitter.** `include`-inside-a-function is a real idiom — a function that
   `include`s a partial and reads what the partial bound — and §8.3's positional
   dam rule is exactly the machinery it would need. It is not built here: the
   §8.2 premise that makes the top-level claim honest (a script scope has no
   proof of absence, so the declaration is the only premise) does not transfer,
   because a function scope *does* have one, and the two premises would have to
   be ordered before the id could speak. Recorded in `not-implemented.md`.

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
  `--profile strict`, its `// V` controls included. (Superseded by §8: with the
  tracer bullet landed the fixture reports its two `// E?` reads and nothing
  else. `--profile default` is still silent on it.)

## 8. Amendment (2026-08-16): the tracer bullet as built (issue #396)

**Status: PENDING ratification**, with §4. What §4 decided stands unchanged;
this records the four things building it forced, each of which is a decision
rather than a note.

### 8.1. The seeds cannot arrive from the crate that can read them

§4 describes a declaration seeding a presence state. Two facts about where the
code lives make the obvious wiring impossible. `steins-syntax` owns ADR-0081's
presence pass and has no edge to `steins-phpdoc`/`steins-contract`, so it cannot
decide that `\DateTime|unset` carries the pseudo-type; `steins-infer` can, but
the CST does not outlive `SourceTree::parse`, so it cannot hand seeds back
afterwards and have anything left to run them over.

So the pass runs at parse time over a **syntactic superset** of the seeds:
every `$name` token of a statement-adjacent docblock whose text contains the
substring `unset`, case-blind. That gate is exact in the direction that matters
— `ContractTy::Unset` is reachable from no other spelling — and coarse in the
direction that is free, since an over-large candidate set can only produce
candidate reads that the checker then drops. `steins-infer` confirms each one
authoritatively: it lowers the named tag through the same `parse_tag_type` +
`steins_contract::lower` the cast lane uses and asks whether a **top-level**
union member `is_unset`. A nested one (`array<int, unset>`) is §5's undecided
question and seeds nothing.

The alternative — giving `steins-syntax` a dependency on the contract crates —
is acyclic and was rejected as too large a structural change for a tracer
bullet: it would move the phpdoc grammar into the crate whose whole discipline
is being env-free and index-free. ADR-0048 §2 is satisfied the same way ADR-0081
satisfies it: the walk is untouched.

Two costs, both recorded rather than fixed. A file whose docblocks never spell
`unset` pays one substring scan per docblock and nothing else. And where two
docblocks seed the same name and only the later one is unconfirmed, the read
attributes to the later declaration and goes silent — the silence direction.

### 8.2. Three premises differ from ADR-0081's run, and only three

The engine is reused, not copied: the same `PresenceCx`, the same
`guard_bound_names` polarity table, the same `presence_stmt` transfer function,
the same terminating-arm subtraction and loop fixpoint. What the new entry point
changes:

1. **Scope entry is `Bound`.** ADR-0081 §6 silences a script scope because an
   included file inherits the includer's symbol table. That silence is kept
   *literally*: a declared name starts bound, and only the author's own `|unset`
   moves it to `Maybe`. A read **before** the declaration is therefore silent —
   it has no premise yet — and a read after it is judged.
2. **The reportable set is the declared names.** ADR-0081's disjointness premise
   ("the scope binds this name somewhere") is a proof-layer premise about
   reachability; here the premise is the declaration, so a name nothing declares
   is nobody's finding. The two claims meet without merging, exactly as §4.5
   asks: a declared name in a *function* scope keeps `variable.undefined`, and
   nothing in ADR-0081's pass was modified.
3. **The declaration re-declares, it does not narrow.** An inline `@var` is a
   cast (ADR-0073 §2), so the seed applies at the adopted statement regardless
   of the prior state: `$x = new \DateTime(); /** @var \DateTime|unset $x */`
   reports at the read below, because the author's own tag says so.

### 8.3. The name dams end the pass instead of blanking the scope

ADR-0081 §6 inherits the definite id's rule: `extract`, `compact`,
`get_defined_vars`, `$$x`, `eval`, `include`/`require` blank the whole scope for
both passes. Kept as-is, that rule would kill this feature outright — a
top-level template that receives its variables from an includer is exactly the
kind of file that `include`s partials of its own, so the dam and the idiom
co-occur by construction rather than by accident.

**The rule here is positional**: every declared name becomes `Bound` from the
first dam onwards. Reads *before* it are still judged; after it nothing is
claimed. That is the silence direction on both sides of the line — the dam can
only remove findings, never add one — and it is honest about what a dam means,
which is that the symbol table stopped being readable from the text, not that
the text before it was never read. A `goto` or a label anywhere still dams the
pass outright: ADR-0081's non-goal, and an unbounded jump edge is not a
positional fact.

### 8.4. Two checker premises: no warning-handler gate, and a by-reference oracle

**No ADR-0049 §7 gate.** The `variable.*` pair and `offset.missing` ride the
declared `warning-handler` posture because their whole claim is "PHP emits an
`E_WARNING` here", and a project that has installed a fatal handler has changed
what that warning means. This id's claim is that the read contradicts the file's
own docblock, which is true whatever the runtime does with the warning — and it
is judged on the contract layer, where no runtime posture is consulted at all.
Gating it would make a declared-contract finding depend on a runtime setting
that has nothing to do with the declaration.

**The out-parameter subtraction is inverted, and had to be.** ADR-0077's
`arg_is_by_value` oracle answers "not by value" for every uncertainty — an
unresolved callee, a builtin with no `out_params` row — which is right for a
proof-layer id trading recall for a zero-FP bar. Reused verbatim it deletes the
claim wholesale: `date_format` carries no row, so the conformance fixture's own
`date_format($passed, …)` probe would be "maybe an out-parameter" and go silent,
which is an acceptance criterion. So this id subtracts on a **confirmed**
by-reference argument instead (a declared `&$p`, or a catalog `out_params` row),
on the maybe leg's call-site-forward rule. An unresolvable callee proves nothing
about the binding, and this id reports it.

## 9. Amendment (2026-08-16): the position ruling as built (issue #397)

**Status: PENDING ratification**, with §4 and §8. §5's ruling stands as written;
this records the one thing pinning it forced, and the shape of the evidence.

### 9.1. `Maybe` at the acceptance leaf was not defense after all

§2 called the leaves' `Maybe` "defense, not the mechanism", on the reasoning that
`flatten_arms` drops the member before acceptance is asked. That is true of every
consumer that builds a `Vec<ContractArm>` — the cast lane, the shape seed, the
stratum rule, the speller — and it is false of the consumers that judge a
declared type *directly*. `check_phpdoc_param` walks the phpdoc AST through
`accepts` for a proven value and the lowered `ContractTy` through `admits_fact`
for an abstract one; neither passes through an arm list. There the `Maybe` was
the mechanism, and a union's or-fold turned it into silence: a `Maybe` absorbs a
sibling's `No`, so the member **deleted** findings — `phpdoc.param-mismatch`,
`phpdoc.return-mismatch`, `phpdoc.property-mismatch` — that the same declaration
without it reports.

The fix keeps the leaves as they are and changes the **folds**: a union's value
members are its non-`unset` members, in `accepts` (`steins-infer`) and in
`admits_val` / `base_only` / `admits_shape_fact` (`steins-contract`). A union
left with no value member answers `Maybe`, which is the bare-`unset` floor of
§2.4 reached by a different route. The `Inter` folds are untouched: `T&unset` is
not an idiom, and an intersection's `and` degrades toward `Maybe`, so its residue
is silence rather than a manufactured `No`.

Nothing outside a docblock that already spells the word can move, so the change
is inert on any code written before #400 — which is every line of both corpora.

### 9.2. The fixtures compare lists, because absence is the wrong question

Each position is pinned by rendering the same source twice, with the member and
without, and asserting the two **complete** diagnostic lists agree. Asking
instead whether some id is absent would have passed on all six positions while
the deletion above was live. The one licensed difference is the message's
rendering of the declaration, which quotes the author's own spelling
(`(\DateTime | unset)`) so a reader is shown the docblock that is actually in the
file; the comparison normalizes it and a separate fixture pins that it is there.
