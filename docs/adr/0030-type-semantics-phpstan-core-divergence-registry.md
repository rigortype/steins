# Type-operation semantics: PHPStan's denotational core + a divergence registry

The semantics of type operations import in three tiers:

**Imported as-is** — the denotational core: types denote value sets,
acceptance is set inclusion, judgments are trinary (yes/maybe/no — the same
shape as the Certainty discipline), constant types sit below general types
in the lattice. Compatibility on this core is fixed mechanically by porting
phpstan-src's data-provider tests (`TypeCombinator`, accepts/isSuperTypeOf)
as fixtures — the ADR-0029 discipline applied to semantics.

**Deliberately divergent** — tracked in a **divergence registry** (the
rigor-rs concept imported: every intentional departure is recorded with its
rationale and, where applicable, the upstream proposal it feeds). Initial
entries:

1. **Two acceptance relations, not one.** Declared-contract acceptance
   (envelope checking; PHPStan-like, no coercion) is distinct from runtime
   survivability (proof layer; the empirically-verified coercion tables).
   PHPStan has only the former; the latter is Steins' differentiator and has
   no import source.
2. **Array-shape / list semantics follow the phpstan/phpstan#14939 RFC
   natively**: `array{...}` is an order-agnostic key *set*, `list{...}` a
   positional key *sequence*, `isList` trinary. Steins is the BC-free
   proving ground (ADR-0016) where the proposed semantics run first and
   produce evidence for the upstream discussion.
3. **No benevolent unions.** `BenevolentUnionType` is a compensation
   mechanism for worst-case-reporting false positives; a proof layer that
   acts only on proven values does not need it. **Grammar compatibility is
   preserved**: the parser accepts `__benevolent<T1|T2>` (it occurs in real
   stubs) and expands it to plain `T1|T2` — accepted syntactically,
   erased semantically. The replacement for what benevolence compensated
   (practically-infallible `|false` returns like `curl_init`) is
   failure-cause labels on union arms + policy-profile consumption:
   ADR-0042.
4. **No narrow-LHS `accepts` strictness.** PHPStan's `accepts` answers
   No where `isSuperTypeOf` answers Maybe (e.g. `1 accepts int`):
   worst-case reasoning on a declared wide type flowing into a narrow
   contract. The runtime value may comply, so the proof layer keeps the
   single overlap relation (`admits_*` == the `isSuperTypeOf` shape) —
   the accepts/isSuperTypeOf asymmetry deliberately collapses. Surfaced
   by the ported fixture net (`const1_accepts_general_int`).
5. **Semantic type equality is mutual subsumption only.** Semantic type
   equality in Steins is defined only as mutual subsumption (Yes/Yes) over
   extensional arms; provenance-flavored types (`literal-string` and kin,
   ADR-0038) are undecidable for equality and are barred from the
   normalizer's arm vocabulary — the `ContractTy` arm type carries no
   provenance slot, so the bar is enforced by the type system. Registered at
   extraction (ADR-0052 §4 / slice N1), as the no-type-combinator amendment
   below requires; this is the recorded reason Steins has no `Type::equals`
   beside a separate `isSuperTypeOf`.

**Deferred until needed** — narrowing details (co-evolving with the branch
analysis ratchet), template variance in full, subtraction types: decided in
envelope-checking priority order, not up front.

## No type combinator: the combinator lives in the value lattice

Steins deliberately has no TypeCombinator/TypeUtils layer — no
`union(A,B)` normalizer, no `remove(T,S)`, no semantic type-equality.
PHPStan needs one because modular analysis has only declared types to
compose; call-site propagation composes *value facts* instead, so the
combination PHPStan does with types happens here as the four-layer
domain's join (proven in production by the honesty transform). Types
stay syntactic lists judged arm-wise through the single acceptance
relation — which the ported phpstan-src fixture net now pins
mechanically, and which is exactly the pairwise-subsumption core a
normalizer would need.

Type-side operations are only needed at three boundaries: **rendering**
(writing a joined type back into phpdoc — exists today as the honesty
renderer's dedup/subsumption-collapse/precision ladder, the embryonic
normalizer), **narrowing/subtraction** (deferred above; inseparable
from subtraction-type design, so neither leads alone), and future
envelope simplification / boundary profiles. The commitment: when
narrowing/subtraction lands, the type-side normalizer is *extracted
from the rendering boundary*, not built up front. Semantic type
equality, when needed, is mutual subsumption (Yes/Yes) — definable only
for extensional types; provenance-flavored types (ADR-0038) are
undecidable for equality by construction, a divergence to register at
extraction time. Object-member subsumption arrives with ADR-0043's
trinary is-a oracle.

## Conformance-suite divergences (intentional silences)

Departures from php-typing-conformance expectations. First triaged
2026-07-23 (18 non-#14939 fails: 0 bugs, all absent-machinery or
intentional); re-triaged at the **M1 conformance-closing sweep,
2026-07-24**, where native object acceptance (including `A&B`
intersections, below) closed the bulk, and the surviving non-#14939 fails
are each registered here as either a standing refusal or an honest
deferral.

1. **Vendor-prefixed tags** (`phpdoc_advanced_vendor_prefixed_param_phan`):
   only `@phpstan-*`/`@psalm-*` prefixes carry contracts (ADR-0029; ROADMAP
   Won't-build, "Tool-specific tags beyond `@phpstan-*`/`@psalm-*`").
   `@phan-param` and other tool-specific tags are erased. On this fixture
   PHPStan *does* consume `@phan-param` (it is a PHPStan Pass — the
   `argument.type` error is emitted), so Steins' erasure is a **registered
   divergence from PHPStan's actual behavior here**, correcting the "PHPStan
   parity" originally recorded; the ADR-0029 tool-tag scope is deliberate
   and stands. Standing refusal.
2. **No declaration-coherence lints**
   (`phpdoc_advanced_param_typehint_nullable_mismatch`,
   `phpdoc_advanced_param_typehint_array_nullable_mismatch`). "Native
   `?string` wider than `@param string`" — and the `?array` vs `list<int>`
   variant — is not reported: the code is type-safe, and a proof layer
   speaks on proven value breaks, not declaration style; tolerating
   native-nullable widening is deliberate (the `$x = null` idiom). At most a
   future policy profile, never core (ROADMAP Won't-build,
   "Declaration-coherence lints"). PHPStan itself fails both **by design**
   (phpstan/phpstan#7572), so this is a shared refusal, not a divergence
   from PHPStan. Standing refusal.
3. **`static`/`self` return-position acceptance — deferral discharged,
   superseded by the ADR-0043 amendment (2026-07-24)**
   (`objects_static_return_mismatch`). Originally registered as an honest
   deferral: ADR-0043 §1 left `self`/`static`/`parent` unlowered. The
   amendment brings return position into v1 via the minimum-bound lemma —
   every late-bound class `T` satisfies `is_a(T, C) = Yes`, so an exact
   returned class with `is_a(V, C) = No` fails *every* possible `T`: an
   unconditional runtime `TypeError` (verified PHP 8.5.8), proof-layer
   under the existing `type.return-mismatch`, no worst-case reasoning.
   What stays out, now as a **standing refusal** rather than a deferral:
   the conditional shapes — `new self()` under `: static` in an open
   class (breaks only on proper-descendant receivers; PHPStan reports it
   by worst-casing, ADR-0002 refuses it; silent by construction since
   `is_a(C, C) = Yes`) and sibling-subclass returns. No conformance case
   exercises the conditional shapes (the fixture class is `final`), so
   this entry no longer records a suite divergence — only the
   PHPStan-behavior delta on the refused shapes.
4. **No `resource` type nor resource-value tracking**
   (`native_types_resource_argument`). `resource` is not a native type — a
   `resource $x` hint is a reference to a non-existent class `…\resource`
   (PHPStan reports `class.notFound`), and the call-site rejections require
   modeling `fopen()`-style resource **values** through a `=== false`
   narrowing and rejecting them against scalar params. Neither an
   undefined-class-in-type-position finding nor a `resource` value domain
   exists yet: an honest **deferral** in the non-scalar / object-world
   value-modeling cluster, not a refusal.

The remaining non-registered gaps stay the prioritized unimplemented queue:
generic type-argument carry (ADR-0032) and callable signatures beyond the
closure-variance arm. Native **object** acceptance — single classes, unions,
enum cases, class constants, and now `A&B` **intersections** (the
conjunctive `InstanceInter` member, ADR-0043) — has landed; the
`instanceof` / offset-access / undefined-method finding kinds exist.

## Governing rule (amendment)

Vocabulary and minor judgments track PHPStan's model (yes/no/maybe,
message idioms, familiar spellings) — familiarity is cheap and compounding.
But when a decision touches the *nature of the inference* and a
fundamentally better outcome is in reach, Steins replaces the PHPStan
approach **without hesitation** (precedents: call-site propagation over
modular analysis, ADR-0001; no template solver where propagation reaches,
ADR-0032). The divergence registry is what makes this boldness safe:
every replacement is recorded, justified, and traceable back upstream.
