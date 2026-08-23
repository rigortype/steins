# Derived type operators: kebab-case vocabulary, projection over representation, and the non-shape roster

**Status: proposed (2026-08-23), PENDING ratification.** Drafted under the
owner's standing delegation, ahead of the slices it governs (#473 the
vocabulary rule and the five `lower_generic` operators, #474
`constructor-parameters-of`). No lowering ships with this ADR. It governs the
whole operator family; [ADR-0090](0090-shape-modifiers.md) takes the six
operators that act on a shape, which carry a prerequisite this roster does not.

## 1. Context: a request arriving in TypeScript's vocabulary

The question was whether TypeScript's utility types belong in Steins as builtin
phpdoc vocabulary, spelled in lowercase so that an operator is visually
distinct from a class name.

The handbook lists twenty-two: `Awaited`, `Partial`, `Required`, `Readonly`,
`Record`, `Pick`, `Omit`, `Exclude`, `Extract`, `NonNullable`, `Parameters`,
`ConstructorParameters`, `ReturnType`, `InstanceType`, `NoInfer`,
`ThisParameterType`, `OmitThisParameter`, `ThisType`, and the four casing
intrinsics `Uppercase` / `Lowercase` / `Capitalize` / `Uncapitalize`. They are
not one thing. Some name a transform Steins' own type model already performs
internally and has no spelling for; some name a transform whose spelling
already exists; and some name a TypeScript mechanism that has no referent in
PHP at all.

Steins already has three operators of exactly this kind — `key-of<T>`,
`value-of<T>` and `template-type<Subject, Owner, 'TName'>` — so the family is
not new. What is new is the size of the batch, and a batch is where a naming
rule and a lowering discipline have to be stated once instead of re-derived per
operator.

## 2. Decision: the discriminator is kebab-case, not lowercase

**Lowercase does not distinguish an operator from a class name in PHP, because
PHP class names are case-insensitive.** `Partial` and `partial` are the same
name to the engine, and `lower_identifier` normalizes with
`to_ascii_lowercase()` before it matches, so a lowercase keyword table entry and
a project's `class Partial` collide by construction. The pseudo-type/class
precedence rule (`steins-infer`'s `accepts_class_name` delegation, PHPStan's
`tryResolvePseudoTypeClassType`) exists precisely to arbitrate that collision,
and it arbitrates it *against* the keyword: a project that declares the class
means the class.

The repository already carries the rule that does work.
`is_shadowable_pseudo_type` opens with

```rust
if norm.contains('-') { return false; }
```

because **a hyphenated spelling is not a legal PHP identifier**, so no class can
ever be declared with that name and nothing can shadow it. Every operator and
refined keyword Steins holds is already on that side of the line: `key-of`,
`value-of`, `class-string`, `int-mask-of`, `template-type`, `non-empty-array`,
`array-key`, the whole refined-string grid. The single-word pseudo-types
(`integer`, `number`, `closure`, `scalar`) are the shadowable ones, and they are
shadowable *because* they are single words.

**Decision.** Every derived type operator is spelled in kebab-case. Where the
natural name is a single word, a suffix is added to force the hyphen rather than
leaving the name shadowable:

- `-of` where the operand is the thing being read or projected — following
  `key-of` / `value-of`, which is the house precedent.
- `-from` where the second operand is subtracted from or filtered against the
  first, so that the name reads in argument order.

This is not a cosmetic preference. It is the same non-shadowability guarantee
ADR-0087 §2.2 gave `unset` by a different route: `unset` is safe because PHP
*reserves* it, and these are safe because PHP cannot *spell* them. A lowercase
roster would have had to add twelve entries to `is_shadowable_pseudo_type`'s
reserved list — entries PHP does not reserve, which would be a lie about the
language.

## 3. Decision: operators are projections, never representation

**No operator gets a `ContractTy` variant.** Each is lowered by projecting the
operand's already-lowered `ContractTy` into an existing one, exactly as
`key-of` / `value-of` do in `lower_generic`: one lowering, then one projection.

This is ADR-0030's one-relation discipline and its §6 refusal of a
`TypeCombinator` / `TypeUtils` layer, applied to vocabulary. An operator that
carried its own variant would be a second representation of a type the arm
vocabulary can already hold, and every consumer — acceptance, the normalizer,
the speller, `to_fact` / `to_shape_fact` — would grow an arm for it. A
projection grows nothing.

Two consequences follow and are accepted:

**The spelling does not survive.** Lowering `non-nullable<int|null>` and
spelling it back yields `int`, not the operator. This is `key-of`'s behavior
already, and it is the behavior we want: `annotate` writes Steins' own
vocabulary (owner ruling, 2026-08-08), and the projected form *is* that
vocabulary. An operator is an author-side convenience at the declaration site,
never an analyzer output.

**Two lowering sites, not one.** An operator whose projection needs nothing but
the operand's `ContractTy` lowers in `steins-contract`'s `lower_generic`. An
operator that must read the **class registry** cannot: `steins-contract` has no
registry and deliberately hosts no object judgment (ADR-0035/0038/0043). Those
lower as a **rewrite over the parsed type before anything is lowered**, in
`steins-infer`'s `Cx::resolve_template_types` seam — the precedent
`template-type`'s declared-side resolution already set, and the reason a
resolved node is judged, dumped and stored exactly as the type it names is.

## 4. Decision: the floor is arity-blind `Opaque`, and never a manufactured `No`

Every operator name enters `KNOWN_UNENFORCED` **before** any arity is checked,
for the reason that table's own comment gives: without the entry the name falls
to `ContractTy::Class`, a reference to a nonexistent class, and acceptance's
class leg answers a definite `No` for every non-object value. A wrong `No` is a
false positive, not lost precision — the closure-argument variance check raises
findings on `No` (ADR-0071 §1).

So: a misspelled arity, an operand the projection cannot read, an unresolved
`@template`, an alias, a union member the rule declines — all floor to
`ContractTy::Opaque`, which is `Maybe`, which reports nothing. **No operator
ever narrows its way into a `No` it did not prove.** The one operator whose
naive reading would violate this is `sealed-of`, and ADR-0090 §3 is where that
is handled.

**Union distribution.** An operator applied to a union maps over the arms and
leaves an arm it does not apply to unchanged —
`non-nullable<array{a: int}|null>` deletes the `null` arm and returns the
shape. Where an operator's rule would
have to decline for *one* arm, the whole type floors to `Opaque` rather than
transforming the readable arms and passing the rest through: a per-arm floor
widens one arm while the others narrow, producing a type that is neither the
operator's answer nor the operand.

## 5. The roster this ADR takes

### 5.1 `non-nullable<T>` — delete the `null` arm

The arm-lane subtraction ADR-0052 §1 already puts on declared alternatives,
given a name. `non-nullable<int|null>` is `int`; `non-nullable<?\DateTime>` is
`\DateTime`.

Two edges are worth stating because they touch existing entries:

- **`non-nullable<mixed>` is `non-null-mixed`** —
  `ContractTy::MixedMinus(MixedCut::Null)`, the cut Steins already spells.
  This makes a **second** construction site for that variant, where
  divergence registry entry 6 records exactly one
  (`lower_identifier`'s keyword arm). Entry 6's *reasoning* is unaffected and
  its claim survives verbatim: `MixedMinus` remains **declaration-side**
  vocabulary that no inference path produces. Only the count of declaration-side
  spellings that reach it changes, and it changes from one keyword to one
  keyword plus one operator over `mixed`. Entry 6 is amended to say so.
- **`non-nullable<null>` is `never`.** That is the honest denotation of the
  empty set, not a manufactured `No` — `never` is already a spellable type and
  an author who wrote this wrote it. Whether an operator that provably yields
  `never` should *also* raise a `phpdoc.*` id at the contracts floor is a real
  question and is **not decided here**; it is the same question `omit-of` with
  an unknown key raises (ADR-0090 §6), and the two should be answered together
  or not at all.

### 5.2 `return-type<F>` and `parameters-of<F>` — read a callable signature

`ContractTy::CallableTy` already carries `Some(CallableSig { params, ret })` for
a declared `callable(P1, P2=): R` (issue #11), and today exactly one consumer
reads it. These give the two halves a spelling:

- `return-type<\Closure(int): string>` is `string` — `CallableSig::ret`.
- `parameters-of<\Closure(int, string=): bool>` is `list{int, string?}` —
  `CallableSig::params` as a positional shape, `optional` becoming an optional
  field.

A **variadic** parameter becomes the shape's unsealed tail, which is the
faithful reading: `parameters-of<\Closure(int, string...): void>` is
`list{int, ...<string>}`.

A **by-reference** parameter floors the whole operator to `Opaque`. The argument
list a `&$x` signature describes cannot be spelled as a plain array — the array
that would be passed to `call_user_func_array` does not have the by-ref
parameter's declared type in the position the projection would put it — and
`CallableParamTy::by_ref` is already the flag the closure-variance check stays
silent on. Silence is the established answer for that axis; this keeps it.

A bare `callable` / `Closure` (`sig: None`) floors to `Opaque`. So does every
refined spelling that carries no signature.

**There is no `typeof`.** TypeScript writes `ReturnType<typeof f>`; Steins has
no operator that turns a *function name* into its type, and this ADR does not
add one. The operand is a callable **type** spelling, never a reference to a
declared function. §6.2 records why the missing `typeof` is what limits this
pair rather than a defect in it.

### 5.3 `exclude-from<T, U>` and `extract-from<T, U>` — filter arms

`exclude-from<T, U>` keeps `T`'s arms that `U` does not subsume;
`extract-from<T, U>` keeps the ones it does. TypeScript's `Exclude`/`Extract`
distribute over unions, and Steins' arm lane *is* distribution — the relation
they need is `normalize::subsumes`, which ADR-0052 §4 already makes the single
pairwise arm relation.

The filter reads a `Certainty`, so the rule states all three answers: an arm is
removed by `exclude-from` on `Yes` only, and kept on `Maybe`. `Maybe` means the
subsumption is undecided, and removing an arm on an undecided relation is
exactly the arm deletion ADR-0052 forbids without proof. `extract-from` mirrors
it — an arm is kept on `Yes` only. Both therefore **widen** relative to a
perfect filter, which is the safe direction.

`exclude-from<T, null>` and `non-nullable<T>` denote the same type. The
redundancy is accepted: `non-nullable` is the spelling authors will reach for,
and it is the one that reaches `MixedMinus`.

### 5.4 `constructor-parameters-of<C>` — read a class's constructor

The one operator on this roster whose operand is a **name Steins resolves
today**. `constructor-parameters-of<\Foo>` is the positional shape of `\Foo`'s
`__construct` parameters, under §5.2's rules for optional, variadic and by-ref.

It is the `new $class(...$args)` idiom — factories, DI containers, and the
argument arrays that get handed around before a class is instantiated — and
there is no existing spelling for it at all.

It lowers as a §3 pre-lowering rewrite in `steins-infer`, not in
`steins-contract`. A class the index cannot resolve, a class with no declared
constructor, and an operand that is not a class reference all floor to `Opaque`.
This is the last slice of the roster, not the first: it is the only one that
needs the second lowering site.

## 6. Refusals

### 6.1 Refused because the spelling already exists

**`Record<K, V>` → refused.** `array<K, V>` denotes the identical set. The
governing rule of the divergence registry — vocabulary tracks PHPStan, because
familiarity is cheap and compounding — cuts against adding a synonym that
PHPStan does not have and that denotes nothing new. Registry entry 6 is the
measured precedent for exactly this shape of decision: 44 of the `mixed~…`
rows were "exact re-spellings of the two cuts Steins already holds", and the
entry's
conclusion was that this "names a spelling gap, not a representation one" and
changes nothing either way. A `record` spelling is a spelling gap by
construction, and this one we know the size of in advance: zero.

**The casing intrinsics `Uppercase` / `Lowercase` / `Capitalize` /
`Uncapitalize` → refused.** Two mechanisms already cover the ground from both
ends. As *predicates* over an unknown string, the refined-string grid holds
`lowercase-string` / `uppercase-string` / `uncased-string` (issue #240) with the
casing axis defined as an identity under the case function. As *transforms* over
a known string, the sidecar folds the real `strtolower` / `ucfirst` call and
returns the actual value (ADR-0004/0028) — a literal answer, not a type-level
approximation of one. TypeScript needs the intrinsics because it has no
evaluator; Steins has one, and it is the default.

### 6.2 Refused because PHP has no referent

**`NoInfer<Type>` → refused.** It steers a type-argument inference algorithm.
ADR-0032 decided Steins has no template solver, because call-site propagation
reaches what a solver would reach. There is no inference to steer.

**`ThisType<Type>`, `ThisParameterType<Type>`, `OmitThisParameter<Type>` →
refused.** They model TypeScript's `this` parameter and contextual `this`
typing. PHP's closures rebind through `Closure::bind` / `Closure::call` at
runtime, which is a value operation, not a signature one; there is no `this`
parameter in a PHP callable type to read or remove.

**`Awaited<Type>` → refused.** PHP core has no promise. The concurrency
libraries that do (Amp, React) are ecosystem surface, which ADR-0044/0045 route
through packs, not through core vocabulary.

**`Readonly<Type>` → refused.** PHP arrays are value types with copy-on-write
semantics: an array a caller hands in cannot be mutated through the callee's
copy, so a readonly shape asserts something already true of every shape and
denotes the same set. PHP's actual `readonly` is a *property* modifier, which is
a class-member fact, not an array-shape one.

### 6.3 Refused because the mechanism it would need is deliberately absent

**`InstanceType<Type>` → refused.** The PHP analogue would read the class out of
a `class-string<Foo>`, and **Steins drops that bound at lowering by design**
(issue #236): `("class-string", _) => StrWith(StrPreds::CLASS_STRING)`, carrying
the bare predicate because generics vocabulary owns `T`. The operand therefore
carries nothing to read. Restoring the bound to serve one operator would reverse
a decision made for the whole class-string family.

The idiom this operator exists for — `$container->get(Foo::class)` returning a
`Foo` — is already served, and served better, by `@template T of object`,
`@param class-string<T>`, `@return T` and issue #363's argument-carry binding.
That reads the *actual* argument at the call site, which is the ADR-0001 answer;
`InstanceType` is the answer a modular checker needs.

## 7. What this does to the divergence registry

Two entries are touched, and one is added.

**Entry 6 is amended** (§5.1): `MixedMinus` gains a second declaration-side
construction site. The entry's substantive claim — that it is declaration-side
vocabulary no inference path produces, so extending it buys nothing — is
unchanged and is the reason the amendment is small.

**A new entry records the family.** These operators are Steins vocabulary that
PHPStan does not have, so PHPStan reports each of them as `class.notFound` —
the same posture ADR-0087 §1 measured for `unset`, and for the same mechanical
reason: the name is absent from the keyword table and falls through to the named
object atom. This is a real adoption cost and it is named rather than hidden. It
is bounded by §3: the operators appear only in **hand-written** declarations,
never in Steins output, so a project adopting them chooses the divergence
file by file, and `annotate` never introduces one.

The entry does **not** claim these are proposals to upstream. `key-of` and
`value-of` came from PHPStan; this roster comes from TypeScript by way of a
request, and whether any of it belongs upstream is a separate conversation with
separate evidence.

## 8. Consequences

**Accepted.** Twelve spellings a PHPStan user cannot share (§7). A second
lowering site for one operator (§3). A `never`-producing operator whose
diagnostic treatment is left open (§5.1).

**Bounded.** Nothing here changes a denotation. Every operator projects into a
type the arm vocabulary already holds, so acceptance, narrowing, the normalizer
and the speller are untouched — the whole family is a front-end that runs before
they do.

**Sequencing.** §5.4 is last; the rest of the roster is independent of it and of
each other. The honest note about value, which ADR-0090 §7 develops: **only
`constructor-parameters-of` resolves a name today.** Every other operator on
this roster, applied to an operand written inline, is longer than the type it
projects to. The family earns its keep when an operand can be a name — a
`@template` bound by issue #363's carry, or a `@phpstan-type` alias, which
Steins does not resolve at all — issue #195 put the tags in the index without
giving them a meaning (ADR-0049 A14), and issue #472 is what would. That is not
an argument against the roster; it is the argument for landing the discipline in
§§2–4 first and letting the slices follow the prerequisite.
