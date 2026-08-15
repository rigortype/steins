# The Divergence Registry

**Status: implemented as policy** — every entry below corresponds to shipped
behavior or a recorded refusal. ADR-0030.

## What this is

Steins tracks PHPStan's *denotational core* for type-operation semantics
([overview.md](overview.md), compatibility hierarchy). Where it departs
deliberately, the departure is a numbered entry here rather than a surprise in a
diagnostic. Entries are deliberate and justified; this is not a defect list.

The governing rule, from ADR-0030's amendment:

> Vocabulary and minor judgments track PHPStan's model — familiarity is cheap
> and compounding. But when a decision touches the *nature of the inference* and
> a fundamentally better outcome is in reach, Steins replaces the PHPStan
> approach without hesitation. The registry is what makes that boldness safe.

The precedents for "without hesitation" are call-site propagation over modular
analysis (ADR-0001) and no template solver where propagation reaches (ADR-0032).

## Core semantic entries

**1. Two acceptance relations, not one.** Declared-contract acceptance (PHPDoc)
is pure set semantics with **no coercion**; runtime acceptance is PHP's own rule
under the calling file's strict mode. PHPStan runs one relation with coercion
rules folded in. See [contract-types.md](contract-types.md).

**2. Array-shape / list semantics follow phpstan/phpstan#14939.** `array{}` is
an order-agnostic key *set*, `list{}` a positional key *sequence*, and
`list<T>` requires keys exactly `0..n-1`. Steins implements the RFC's
resolution, which current PHPStan does not — since ADR-0062 this includes
the denotational `isList` trinary (computed over the admitted value set,
optional-key combinatorics included) and, since issue #163, the *rendering*:
a sealed shape's head keyword is decided by its own `isList` — `list{…}` for a
proven key sequence, `array{…}` for anything else — so re-parsing what we print
yields a shape with the same `isList`. **The spelling follows the model, not the
oracle**, and the cost is named rather than hidden: PHPStan stable's
`ConstantArrayType` conflates the two (its `array{A, B}` retains key order, so
it means what we spell `list{A, B}`), and stating the sequence fact moves those
nsrt rows off the headline `match` count onto `subsumed` — "narrower than the
assertion", which is the correct verdict when we state a sequence and the oracle
states a set. Issue #159's attempt to adopt the conflated spelling bought that
headline at the price of a rendering that did not round-trip; it is reverted in
the head keyword only. Key *layout* still spells as PHPStan spells it —
positional fields for keys `0..n-1` all required, every key printed otherwise,
`non-empty-` dropped where a required key implies it, and the empty shape
`array{}` (vacuously a list, but the braces already say so and both spellings
re-parse alike). Unsealed shapes keep the native spelling, where the modifiers
still carry information the braces do not.

**3. No benevolent unions.** `BenevolentUnionType` compensates for worst-case
false positives that a proof layer does not emit. The grammar is accepted
(`__benevolent<A|B>` parses, with a provenance flag), the semantics erased — it
is a plain union. Failure-arm labels (`failure.environment`, `failure.input`,
`failure.resource`; ADR-0042) are the replacement mechanism where the real need
is "why does this arm exist".

**4. No narrow-LHS `accepts` strictness.** PHPStan's `accepts` answers a
worst-case question about declared types. Steins keeps a single overlap relation.

**5. Semantic type equality is mutual subsumption only.** No structural
equality, no provenance-sensitive equality. Provenance-flavored types are barred
from the normalizer's arm vocabulary by the type system, so equality cannot be
asked about them ([contract-types.md](contract-types.md)).

**6. No type-combinator layer.** Combination happens in the value lattice; the
type-side normalizer was *extracted from the honesty renderer* rather than built
as a parallel `TypeCombinator`/`TypeUtils` stack (ADR-0030 amendment, discharged
by ADR-0052 slice N1).

**7. Declared-order trust in positional projections is declined.** PHPStan's
`ConstantArrayType` stores declared key order and lets the positional
projections (`array_keys`, `array_values`, `array_slice`, `array_reverse`)
read it as runtime order, while acceptance stays order-insensitive — a
real-FP class (phpstan/phpstan#14940). Steins splits by provenance
(ADR-0062 §2): order-dependent results are computed only on
**order-witnessed** concrete values; over shape-only (order-declared) truth
they take the sound widening, never declaration order.

**8. No abstract `nextAutoIndexes`.** The next-auto-index prediction exists
only for concrete arrays and is PHP-minor-aware (ADR-0049 A12). An abstract
shape declines the prediction: append widens the tail (ADR-0062 §3).

**9. No union-degradation threshold.** PHPStan collapses >256-member
constant-array unions (`ARRAY_COUNT_LIMIT`) as a heuristic. Steins' finite
layer caps at the OneOf bound and then **computes** the shape summary
member-by-member (keys-in-all required, keys-in-some optional, residue in
the tail; ADR-0062 §3). The 256 constant survives only as the single-shape
field-width bound (A-G6), deliberately imported rather than invented.

**10. A mis-arity `template-type<…>` is silence, not an error type.** PHPStan
resolves `template-type` at any arity but three to an error type and reports it.
Steins checks the *name* before the argument count, so every arity lowers to
`Opaque` and the declaration constrains nothing (issue #360). The reason is the
floor's own discipline: the alternative reading — falling through to a class
named `template-type` — is a nonexistent-class reference that would manufacture
a definite `No` for every non-object value, and inventing a *new* finding for a
malformed docblock is a claim about spelling rather than about a value break.
The resolution (issue #361) did not change this: it runs *after* the name is
recognized and only on the three-argument shape, so a mis-arity spelling never
reaches a decision procedure that could report it. Revisiting the entry means
deciding to emit a docblock-spelling finding, which is a separate call.

**11. `template-type`'s ancestor lookup walks one level, not the chain.**
PHPStan's `Type::getTemplateType(ancestor, name)` resolves the subject's
ancestor *transitively*: any class in the hierarchy that leads to the owner
contributes, with the intermediates' type arguments substituted along the way.
Steins reads the subject's own `@extends`/`@implements` edges and stops there
(issue #361), which is the same one-level rule the generics carry already
follows for the same reason (ADR-0032's inheritance-edge amendment): the moment
an intermediate class is generic, following the chain is *substitution*, and a
one-level walk that pretended otherwise would be wrong rather than merely
incomplete. So `IntBox` declaring `@extends Box<int>` resolves, and a subject
reaching `Box` only through a `@template U`-declaring `Mid` declines. The
reconsideration precondition is the same as the amendment's — a substitution
mechanism, not a longer walk.

**12. An unresolvable `template-type<…>` is `Opaque`, not an error type.**
PHPStan yields an error type when the utility resolves to nothing — an unknown
owner, a template name the owner does not declare, an unrelated subject — and
also resolves an *unresolved template* to its declared bound. Steins floors all
of it to `Opaque` (issue #361), which admits every value and constrains none.
The two halves have one reason between them: an error type is a claim that a
docblock is wrong, and this analyzer's proof layer speaks about values breaking,
not about spellings. The bound half is not even a gap — Steins substitutes a
*vocabulary* bound for the template before the utility is read (issue #293), so
`@template T of int` already projects `int`; what stays opaque is the class
bound, which #293 declines on its own terms.

**13. A `template-type` subject naming the receiver's template reads a *carry*,
where PHPStan reads a declared type.** For `@return template-type<T, Owner,
'TName'>` on a `Helper<T>` method, PHPStan asks the receiver's type what it is
parameterized by — and a `@param Helper<Model> $h` answers, because the
declaration is the type. Steins asks the receiver *object* what it carries
(issue #362), which is a different question with three consequences worth
stating rather than discovering. A **declared** receiver seeds no `HeapObj`
today (ADR-0032's binding amendment names this exact gap), so it carries
nothing and the read declines where PHPStan resolves. A receiver whose value
carry an earlier method call **swept** (issue #295) declines for the same
reason, including a *second* read on the same receiver — PHPStan re-reads the
declaration and answers both times. And the hop out of a carried value walks
**one level** of that value's own edges, entry 11's rule applied a second time.
What Steins gets in exchange is the case the declaration cannot state: the
receiver built from `new Helper(new Model())`, where the parameterization is a
proven value and no `@var` was written. The reconsideration precondition for
the first consequence is the parameter seed ADR-0032 §3 already specifies and
has no site to fire at; for the second and third, a substitution mechanism.

**14. PHPStan *solves* a call-site template by unification over every position;
Steins *binds* it from a carry, at top-level positions only.** PHPStan collects
every occurrence of `T` across a signature — nested inside `list<Box<T>>`, under
a `Box<T>|null`, inside a callable's parameters — generates constraints from
each, unifies them, and substitutes the solution back through the signature,
falling back to `T`'s declared bound where nothing decides it. Steins performs
one positional **read** instead (issue #363): a `@param Owner<…, T, …>` at the
top level asks the argument's generics carry what sits at `T`'s position in
`Owner`'s own `@template` list, `@param T $p` reads the argument's proven value,
and nothing else binds. **Every consequence is a silence, deliberately.** A
nested, nullable or callable occurrence is not merely unread — it *contests* the
name, so the whole read declines rather than answering from the positions that
happen to be legible. Where PHPStan unifies `@param \Closure():T $t1` with
`@param T $t2` and joins the two into `A1|A2`, Steins says nothing: answering
`A2` from the one readable position would be **narrower than the declaration
supports**, and narrower-than-true is the direction this family refuses even at
the Asserted grade, because contract arms feed narrowing and the dump surface.
Two occurrences that disagree decline for the same reason, where PHPStan joins
or errors. A named or spread argument list declines the whole call, because
position stops naming a parameter. A **bounded**
template reads its bound rather than the value carried at it — `@template T of
int` under `@param Box<T>` handed `new Box(1)` reads `int`, where PHPStan reads
`1`; the bound is what the author promised, and reading through it would be an
inhabitation check ADR-0032 keeps thin. And where a body summary exists it
outranks the read entirely, which is the point: Steins' primary answer to "what
is `T`" is the *value that flowed in* (ADR-0001 propagation, ADR-0032 tier 1),
and the carry read is the floor under it rather than a rival inference.
Reconsideration precondition: none — this is a standing refusal of the solver,
not a deferral (ADR-0032, ADR-0030 reg. entry above).

## Conformance-suite divergences (intentional silences)

Steins runs `php-typing-conformance`. Standing at the last recorded run
(2026-07-24, at the v0.1.0 landing point): **93/98**, with every remaining
non-#14939 failure registered
below as either a standing refusal or an honest deferral, and zero
absent-machinery failures among them at the time of triage. The ceiling is set
by the intentional entries.

**1. Vendor-prefixed tags — standing refusal.** Only `@phpstan-*` / `@psalm-*`
prefixes carry contracts (ADR-0029). `@phan-param` and other tool-specific tags
are erased. PHPStan *does* consume `@phan-param` on the relevant fixture, so this
is a registered divergence from PHPStan's actual behavior; the tool-tag scope is
deliberate and stands.

**2. No declaration-coherence lints — standing refusal (shared with PHPStan).**
"Native `?string` wider than `@param string`" is not reported: the code is
type-safe, and a proof layer speaks on proven value breaks, not declaration
style. Tolerating native-nullable widening is deliberate (the `$x = null`
idiom). At most a future policy profile, never core. PHPStan fails these fixtures
by design too (phpstan/phpstan#7572), so this is a shared refusal.

**3. `static`/`self` return-position acceptance — deferral discharged; the
conditional shapes are now a standing refusal.** Return position landed via the
minimum-bound lemma: every late-bound class `T` satisfies `is_a(T, C) = Yes`, so
an exact returned class with `is_a(V, C) = No` fails *every* possible `T` — an
unconditional runtime `TypeError`, reportable under `type.return-mismatch` with
no worst-case reasoning. What stays out: `new self()` under `: static` in an open
class (breaks only on proper-descendant receivers — PHPStan reports it by
worst-casing) and sibling-subclass returns.

**4. `resource` — deferral discharged (2026-08-14, ADR-0056 §8).**
`resource` is not a native type; a `resource $x` hint references a non-existent
class, and `class.undefined` has reported that since S4. The *value* half is now
modeled: 19 php-src-mined producers seed a `resource`/`resource|false` contract
arm, `=== false` discharges the failure arm through the ordinary subtraction, and
a proven resource handed to a scalar or class parameter is a `type.argument-mismatch`
— mode-independent, since a resource coerces to nothing. The blocker was never
the narrowing but the *grade*: `fopen` declares no return type (PHP has no syntax
for one), so the reflected envelope every builtin return fact anchors to was
unavailable. §8 substitutes a tripwire — a curated row stands only while the
engine still declares nothing for the name, which is exactly what the PHP 8
resource-to-object migration ends. The value domain is unchanged and still
object- and resource-free (ADR-0035/0038). Still deferred (§8.7): arrays *of*
resources, resource-consuming *parameters*, and open/closed state.

**5. `phpdoc_advanced_phpstan_template_type` line 47 — RETIRED (2026-08-15,
ADR-0086 §2 / #376): the case is enforced.** The entry recorded a standing
silence: `takesString(unwrap(new Box(1)))` reported nothing, because `unwrap`'s
`@return T` binds to the carried `1` as an **Asserted** fact and an Asserted
argument fact premises no `type.argument-mismatch`. That much is still true and
was never the gap. The gap was the route beside it — the body-proven one, tier 1,
`return $box->value` proving `1` — which needed heap properties to cross a
binding descent. ADR-0086's argument leg is that crossing: an argument's object
now enters the callee's store as a copy, so `$box->value` is `1` inside `unwrap`,
the summary is `1` at the `Verified` stratum, and line 47 reports
`type.argument-mismatch` under the case's own `strict_types`. Line 42's
`unwrap(new Box('ok'))` stays silent, as it should. The divergence is gone rather
than reclassified; entry 14 (the solver) is unaffected.

## Not registered — just unimplemented

These are gaps, not divergences, and they are tracked in
[not-implemented.md](not-implemented.md):

- generic type-argument carry *through a variable binding* (the direct-`new`
  argument position landed as ADR-0032 stage 1; the heap carries no type
  arguments);
- callable signatures beyond the closure-variance arm;
- template scope transfer (ADR-0051).

Native **object** acceptance — single classes, unions, enum cases, class
constants, and `A&B` intersections — has landed, along with `instanceof`,
offset-access, and undefined-method finding kinds.
