//! The diagnostic-id vocabulary: every registry id the engine can emit, the
//! emitted / registered-but-not-yet-emitted tables the CLI and the suppression
//! layer read, and the reserved dump-family FQNs (ADR-0053 §5).
//!
//! Everything here is re-exported wholesale from the crate root (`pub use
//! ids::*`), so `steins_infer::ID` and its siblings keep the paths downstream
//! crates already use. The file is deliberately inert — constants and one
//! predicate; its only import is the suppression layer's own two ids, which
//! [`ALL_EMITTABLE_IDS`] lists — so it stays a leaf of the module graph.

use crate::suppress;

/// The registry id for the `type.argument-mismatch` proof-layer check (ADR-0022).
pub const ID: &str = "type.argument-mismatch";

/// The registry id for the **possibly-grade** argument check on an all-`Verified`
/// premise (ADR-0081 §8's 2026-08-16 amendment, issue #391): the argument's
/// abstract fact has at least one base arm (or a `null` side-flag) the native
/// parameter type rejects **and** at least one it accepts, at the call-site file's
/// coercion mode.
///
/// Not a definite No — an over-approximating type never proves the rejected arm is
/// inhabited on a live path — which is exactly why it takes the `Strict` floor and
/// the gate's non-increase posture rather than the zero bar [`ID`] carries. Its
/// all-arms-rejected sibling was measured empty on every public source and is
/// deliberately not built (issue #291).
pub const TYPE_MAYBE_ARGUMENT_MISMATCH_ID: &str = "type.maybe-argument-mismatch";

/// The contract-layer twin of [`TYPE_MAYBE_ARGUMENT_MISMATCH_ID`]: the same
/// judgment where any arm of the premise is `Asserted` — a docblock claim, a
/// curated refinement over a native envelope, or an ADR-0069 declared-return floor
/// row. ADR-0052 §5's consumption rule forbids an `Asserted` premise from reaching
/// a `type.*` id, so the pair is two registrations of one judgment.
///
/// `Floor::Strict` rather than the `phpdoc.*` family's `Contracts`, on the
/// `offset.maybe-missing` precedent: the layer says whose claim it is, the floor
/// says how sure it is, and the definite sibling ([`PARAM_MISMATCH_ID`]) keeps
/// `Contracts` so a `contracts` run keeps its meaning.
pub const PHPDOC_MAYBE_ARGUMENT_MISMATCH_ID: &str = "phpdoc.maybe-argument-mismatch";

/// The registry id for the `type.return-mismatch` proof-layer check (ADR-0022):
/// a function/method whose return type is a native scalar/union and one of its
/// (trace-visible) `return <literal>;` statements provably raises a `TypeError`.
pub const RETURN_ID: &str = "type.return-mismatch";

/// The registry id for the phpdoc declared-contract param check (ADR-0030 relation
/// #1): a proven value flowing into a parameter with a `@param` phpdoc envelope
/// that it provably does **not** inhabit under contract (set) acceptance — no
/// coercion (a numeric string `"5"` does not satisfy `int` here). Distinct from the
/// runtime relation ([`ID`]); phpdoc types are never enforced at runtime.
pub const PARAM_MISMATCH_ID: &str = "phpdoc.param-mismatch";

/// The registry id for the **sentinel-parameter** check (ADR-0088 §4, issue
/// #428): an argument passed to a `@param never` parameter whose own
/// most-refined declared type — the `@param`-refined domain where a docblock
/// narrows the argument's native declaration, the native declaration alone
/// otherwise (ADR-0037 trust order) — is still provably non-empty on the
/// current path. `never` is uninhabited, so it is excluded from
/// [`PARAM_MISMATCH_ID`] entirely rather than demoted or reworded (one id, one
/// remedy): that id says "fix this argument", this one says "the case analysis
/// reaching this call is incomplete".
///
/// The sentinel is spelled in a docblock, so the premise is `Asserted` by
/// construction — this is contract layer regardless of whether the surviving
/// domain is itself all-`Verified` (the claim being checked is the sentinel's,
/// not the surviving type's provenance). `Floor::Contracts`: the question is
/// definite ("still reachable" or silent), not a possibly-grade one, so it sits
/// beside [`PARAM_MISMATCH_ID`] rather than the `Strict`-floor maybe family.
pub const NEVER_PARAM_REACHABLE_ID: &str = "phpdoc.never-param-reachable";

/// The registry id for the phpdoc declared-contract return check (ADR-0030): a
/// proven `return <value>;` that provably does not inhabit the `@return` envelope
/// under contract acceptance.
pub const RETURN_MISMATCH_ID: &str = "phpdoc.return-mismatch";

/// The registry id for the **unknown-vocabulary** check (ADR-0091 §6, issue
/// #479): a **hyphenated** identifier in a phpdoc type position that survives
/// the `@template` shadow and is not recognized type vocabulary.
///
/// The hyphen is what makes this decidable where an unrecognized identifier
/// normally forces silence. PHP's compiler rejects `-` in a class-like name, so
/// the spelling can be no class; ADR-0091 §4.1 makes a hyphenated `@template`
/// name or `@phpstan-type` alias a refusal rather than a declaration. All three
/// readings that would otherwise demand silence are gone, and what is left is a
/// closed set of two — a misspelling of vocabulary (`non-empy-string`), or
/// vocabulary from a tool Steins does not model (`some-psalm-thing`). Neither
/// can be a false claim about the *program*: the identifier provably denotes
/// nothing.
///
/// It **adds** a finding and removes none. The value judgment is #478's and is
/// untouched — the spelling still lowers to `ContractTy::Opaque`, still admits
/// every value as `Maybe`.
pub const PHPDOC_UNKNOWN_VOCABULARY_ID: &str = "phpdoc.unknown-vocabulary";

/// The registry id for the branch-sensitive null-dereference proof (ADR-0031
/// stage 1): a method call whose receiver variable is **proven `null`** on the
/// current path (e.g. inside `if ($u === null) { $u->name(); }`) — a guaranteed
/// runtime `Error` ("Call to a member function on null"). Only a *`Singleton(null)`*
/// receiver fires; a `OneOf` that merely *includes* null is `Maybe` → silent.
pub const CALL_ON_NULL_ID: &str = "call.on-null";

/// The registry id for the native property-type check (ADR-0036): a proven value
/// assigned to a native-typed property provably raises a `TypeError` under the
/// assigning file's strict mode (`$x->p = "abc"` on `int $p`).
pub const PROP_MISMATCH_ID: &str = "type.property-mismatch";

/// The registry id for the phpdoc `@var` property-contract check (ADR-0036/0030):
/// a proven or abstract value assigned to a property provably does not inhabit its
/// `@var` contract type (definite `No` only; no double-report where native fired).
pub const PHPDOC_PROP_MISMATCH_ID: &str = "phpdoc.property-mismatch";

/// The registry id for the readonly-reassignment proof (ADR-0036): a second proven
/// write to a `readonly` property on one path — a guaranteed runtime `Error`.
pub const READONLY_REASSIGNED_ID: &str = "readonly.reassigned";

/// The registry id for the effect-envelope check (ADR-0005/0022): a function
/// declared `#[\Steins\Pure]` / `#[\Steins\Effect(...)]` whose inferred effects
/// exceed the declared envelope (ADR-0018 prefix subsumption).
pub const EFFECT_ID: &str = "effect.envelope-exceeded";

/// The one unified trinary judgment (ADR-0031), defined in `steins-domain`
/// and re-exported here: condition evaluation in the branch walk, phpdoc
/// contract acceptance (ADR-0030), and the domain's own fact queries all
/// speak the same `Certainty`.
pub use steins_domain::Certainty;

/// The registry id for the unknown-effect-label check (ADR-0018/0022): a declared
/// `#[\Steins\Effect(...)]` label that is not in the label registry
/// (`steins_catalog::LabelRegistry` — the builtin taxonomy plus whatever the
/// ADR-0068 plugin channel registered for this project) — a typo, or a private
/// label no plugin registers.
pub const UNKNOWN_LABEL_ID: &str = "effect.unknown-label";

/// The registry id for the **interop** vocabulary check (issue #311): a label in
/// an upstream purity tag that the registry does not know but that carries
/// evidence of label intent ([`steins_catalog::LabelRegistry::label_intent`] — a
/// near miss, a sibling in the same list, a dot-path shape, or a retired
/// spelling). The tag has already gone ⊤ (ADR-0082 amendment); this id keeps that
/// degradation visible.
///
/// Deliberately not [`UNKNOWN_LABEL_ID`] (mechanics: unsuppressable, fail-closed —
/// refused for docblocks by the same amendment). This is *contract* layer at the
/// `contracts` floor, so a mid-migration codebase can absorb it via baseline or
/// `@steins-ignore`.
pub const INTEROP_UNKNOWN_LABEL_ID: &str = "effect.interop-unknown-label";

/// The registry id for the `@throws` envelope check (ADR-0040/0007): a **checked**
/// exception that **provably escapes** (`Yes`) a function/method whose docblock
/// declares `@throws`, and is a subclass of **none** of the declared classes. Only
/// proven escapes report; `Maybe`-escape and unknown-hierarchy stay silent (the
/// consumer-inverted safe side of ADR-0040).
pub const THROW_UNDECLARED_ID: &str = "throw.undeclared";

/// The registry id for the Liskov throw-widening check (ADR-0033/0040 rule 4): an
/// override/implementation whose declared `@throws` names a checked class that is
/// a subclass of none of the parent method's declared `@throws` classes. Fires
/// only when **both** sides declare `@throws`; `Maybe` resolution stays silent.
pub const THROW_LISKOV_ID: &str = "throw.liskov-widened";

/// The registry id for the Liskov effect-widening check (ADR-0033 point 5): a
/// project method whose **proven** inferred effects exceed the effect envelope
/// (`#[\Steins\Pure]` / `#[\Steins\Effect(...)]`) declared on the abstraction it
/// overrides or implements (a parent class or interface method). Implementations
/// may be purer, never less pure; the exhaustiveness-tainted (unknown) remainder
/// stays silent — only the proven subset judges.
pub const EFFECT_LISKOV_ID: &str = "effect.liskov-widened";

// ---------------------------------------------------------------------------
// The finding-breadth family (ADR-0049): absence-proof ids. An id not yet wired to
// an emitter lives in `REGISTERED_NOT_YET_EMITTED` rather than `ALL_EMITTABLE_IDS`;
// the totality test binds the two lists to the registry.
// ---------------------------------------------------------------------------

/// The registry id for the undefined-function check (ADR-0049 §3, proof layer): a
/// call to a function no candidate FQN defines and the sidecar reports not-found,
/// with the dam clear.
pub const CALL_UNDEFINED_FUNCTION_ID: &str = "call.undefined-function";

/// The registry id for the undefined-method check (ADR-0049 §4, proof layer): a
/// method call on a proven-exact receiver whose fully-enumerated hierarchy defines
/// no such method, with no `__call`/trait obstacle.
pub const CALL_UNDEFINED_METHOD_ID: &str = "call.undefined-method";

/// The registry id for the undefined-class check (ADR-0049 §5, proof layer): a
/// class reference at a hard-error position (`new`, static call, class-const /
/// static-property fetch) whose FQN is absent from the index, the builtin
/// hierarchy, and the sidecar, with the dam clear.
pub const CLASS_UNDEFINED_ID: &str = "class.undefined";

// global constants (ADR-0078, issue #198)
/// The registry id for the undefined-**global-constant** check (ADR-0078, issue
/// #198, proof layer): a bare constant fetch (`FOO`, `\FOO`, `Ns\FOO`) that no
/// `const` statement and no literal `define()` in the universe declares, and the
/// project's own PHP reports as not defined, with the dam clear.
///
/// Fetching one is a fatal `Error: Undefined constant "FOO"` since PHP 8.0
/// (`php -r`-witnessed on 8.5.9). `X::CONST` is a **class** constant — a different
/// member namespace, and issue #197's id, not this one.
pub const CONSTANT_UNDEFINED_ID: &str = "constant.undefined";
// end global constants (ADR-0078, issue #198)

/// The registry id for the too-few-arguments check (ADR-0049 §6, proof layer): a
/// uniquely-resolved call passing fewer positional arguments than the target's
/// required parameters (always `ArgumentCountError`).
pub const CALL_TOO_FEW_ARGUMENTS_ID: &str = "call.too-few-arguments";

/// The registry id for the too-many-arguments check (ADR-0049 §6, proof layer):
/// extra arguments to an **internal** non-variadic target (userland silently
/// ignores them — never a finding).
pub const CALL_TOO_MANY_ARGUMENTS_ID: &str = "call.too-many-arguments";

/// The registry id for the unknown-named-argument check (ADR-0049 §6, proof
/// layer): a named argument binding no parameter of a resolved non-variadic target
/// (fatal `Error`).
pub const CALL_UNKNOWN_NAMED_ARGUMENT_ID: &str = "call.unknown-named-argument";

/// The registry id for the missing-offset check (ADR-0049 §7, proof layer): a read
/// of a key provably absent from a proven container value (`Undefined array key`).
pub const OFFSET_MISSING_ID: &str = "offset.missing";

/// The registry id for the offset-on-unsupported check (ADR-0049 §7, proof layer):
/// an offset read on a proven non-offsetable base (object → fatal `Error`;
/// scalar/null → warning).
pub const OFFSET_ON_UNSUPPORTED_ID: &str = "offset.on-unsupported";

/// The registry id for the undeclared-offset check (ADR-0062 A-G10, **contract**
/// layer, floor `strict`): a constant-key read of a key the base's *declared* shape
/// excludes — a field declared `Absent`, a key outside a `Sealed` shape's fields, or
/// a key the unsealed tail's own key class rejects (`ShapeRead::DeclaredAbsent`).
///
/// Absence is definite only conditional on the docblock (Asserted world), so this
/// is contract-grade, never a proof-layer claim (A-G9's corollary). v1 scope:
/// shape-declared bases with constant/env-resolved keys only.
pub const OFFSET_UNDECLARED_ID: &str = "offset.undeclared";

/// The registry id for the undischarged optional-offset read (ADR-0062 A-G10 /
/// issue #51, **contract** layer, floor `strict`): a constant-key read of a key the
/// base's declared shape marks `Optional` (`ShapeRead::MaybeMissing`) that no
/// proof on this path discharges — no `isset`/`array_key_exists` guard promoted it
/// (S4), and no KeyCover + `¬isset` premise ladder proved it (S5).
///
/// Runtime: warns `Undefined array key`, yields `null`, which then propagates — a
/// real soundness hazard, reported only on the strict surface because the evidence
/// is the declaration, not a proven value. `??` chain non-final arms never fire —
/// the operator itself protects them.
pub const OFFSET_MAYBE_MISSING_ID: &str = "offset.maybe-missing";

/// The registry id for the declared-receiver undefined-method check (ADR-0049 §8,
/// **contract** layer): a method absent on a phpdoc-declared receiver narrowed by
/// branch analysis, under descendant closure.
pub const PHPDOC_UNDEFINED_METHOD_ID: &str = "phpdoc.undefined-method";

// printf arity (ADR-0078, issue #188)
/// The registry id for the printf-family arity check (ADR-0078 / issue #188,
/// proof layer): a `printf`/`sprintf`/`fprintf`/`vprintf`/`vsprintf` call whose
/// folded literal format string demands more placeholders than the call proves
/// it supplies (a `printf`/`sprintf`/`fprintf` fatal `ArgumentCountError`; a
/// `vprintf`/`vsprintf` fatal `ValueError` on a proven-size array). Distinct from
/// [`CALL_TOO_FEW_ARGUMENTS_ID`]: the evidence is a folded format string, not a
/// resolved callee signature, keeping the M2 internal-arity slice
/// (`CALL_TOO_MANY_ARGUMENTS_ID`) clean of format-derived claims.
pub const CALL_PRINTF_TOO_FEW_ARGUMENTS_ID: &str = "call.printf-too-few-arguments";
// end printf arity (ADR-0078, issue #188)

// ---------------------------------------------------------------------------
// The member-kind port wave (ADR-0078): ids that fit neither the premise axis
// (`type.*`/`phpdoc.*`) nor the syntactic axis (`call.*`/`class.*`/`offset.*`),
// named instead by what kind of member or construct the finding is about.
// ---------------------------------------------------------------------------

/// The registry id for the duplicate-array-key check (ADR-0078, **mechanics**
/// layer, issue #187): a literal array expression declares the same
/// PHP-normalized key twice, so the earlier value is silently overwritten —
/// works-but-drops-a-value drift, not a runtime break, hence mechanics rather
/// than proof. Purely syntactic; key comparison reuses `steins_syntax`'s A12
/// coercion and next-auto-index resolution (`duplicate_array_keys`) rather than
/// a second coercion table. A key the fold gate cannot pin — a variable, a call,
/// a spread, and every `Auto` position after one of those — is silently skipped.
pub const ARRAY_DUPLICATE_KEY_ID: &str = "array.duplicate-key";

// ---------------------------------------------------------------------------
// The dump surface (ADR-0053): requested introspection in the debug layer.
// ---------------------------------------------------------------------------

/// The registry id for the explicit `PHPStan\dumpType($e)` dump (ADR-0053 §2, debug
/// layer): renders the walk's best knowledge of `$e` at the call position (the trust
/// order — proven value beats membership beats declared arms). Fail-level, fixed:
/// the named function does not exist at runtime, so a committed call is a guaranteed
/// fatal (§3). Emitted from D3.
pub const DEBUG_TYPE_ID: &str = "debug.type";

/// The registry id for the explicit `PHPStan\dumpPhpDocType($e)` dump (ADR-0053 §2,
/// debug layer): renders the **contract-fact arm list** (the declared envelope as
/// narrowed by guards) — the declared-side view. Fail-level, fixed (§3). Emitted
/// from D3.
pub const DEBUG_PHPDOC_TYPE_ID: &str = "debug.phpdoc-type";

/// The registry id for the default-on `var_dump($e)` dump (ADR-0053 §2, debug
/// layer): one `debug.type`-shaped report per argument expression. Warn-level,
/// fixed — exit-neutral forever (§3), profile-disableable (§4). Emitted from D4.
pub const DEBUG_VAR_DUMP_ID: &str = "debug.var-dump";

/// The registry id for `@psalm-trace` (ADR-0074, debug layer). It renders the
/// same trust-ordered fact as `PHPStan\dumpType($x)`, including the `(asserted)`
/// marker. It is warn-level and cannot be disabled by a profile: the runtime-inert
/// annotation is an authored question, removed by deleting the comment.
pub const DEBUG_TRACE_ID: &str = "debug.trace";

// ---------------------------------------------------------------------------
// declaration-incompatibility fatals (ADR-0078, issue #183)
// ---------------------------------------------------------------------------

/// The registry id for the unimplemented-abstract-method check (ADR-0078, proof
/// layer): a **non-abstract** class-like that inherits an abstract method (from an
/// abstract ancestor or an implemented interface) which no class in its chain ever
/// defines. PHP refuses the declaration itself — `php -r 'abstract class B {
/// abstract public function m(); } class C extends B {}'` →
/// `Fatal error: Class C contains 1 abstract method and must therefore be declared
/// abstract or implement the remaining method (B::m)`. A declaration-graph claim
/// only: no flow analysis, no receiver, no value domain.
pub const CLASS_ABSTRACT_UNIMPLEMENTED_ID: &str = "class.abstract-unimplemented";

/// The registry id for the extends-final check (ADR-0078, proof layer): `class X
/// extends F` where `F` resolves uniquely to a `final` project class. PHP refuses
/// the declaration — `php -r 'final class F {} class C extends F {}'` →
/// `Fatal error: Class C cannot extend final class F`.
pub const CLASS_EXTENDS_FINAL_ID: &str = "class.extends-final";

// overriding family (ADR-0078, issue #184)
// ---------------------------------------------------------------------------
// Five proof-layer ids for the rest of PHPStan's `OverridingMethodRule` surface,
// each a fatal PHP raises **at class load** — the same consequence class
// `class.extends-final` claims, from the same declaration graph, which is why they
// take the same layer and floor. v1 judges **native signatures only**: an
// `@param`/`@return`/generics premise is Asserted (ADR-0037/0052 N2) and cannot
// forge a proof-layer finding, so it is silence here and the phpdoc twin waits on
// ADR-0032's generics carry.
// ---------------------------------------------------------------------------

/// The registry id for overriding a `final` method: `php -r 'class P { final
/// public function m() {} } class C extends P { public function m() {} }'` →
/// `Fatal error: Cannot override final method P::m()`. Witnessed to fire on a
/// `static` method, on a grandparent's `final`, on an `abstract` re-declaration in
/// the child, and on `__construct` — finality is the one member of this family a
/// constructor does **not** escape.
pub const OVERRIDE_FINAL_ID: &str = "override.final";

/// The registry id for a static/non-static override mismatch, both directions:
/// `class P { public function m() {} } class C extends P { public static function
/// m() {} }` → `Fatal error: Cannot make non static method P::m() static in class
/// C`, and the reverse → `Cannot make static method P::m() non static in class C`.
/// `__construct` is excluded: `public static function __construct()` is the
/// standalone fatal `Method C::__construct() cannot be static`, a different
/// consequence that this id would misname.
pub const OVERRIDE_STATIC_MISMATCH_ID: &str = "override.static-mismatch";

/// The registry id for an override that weakens visibility along
/// `public` → `protected` → `private`: `class P { public function m() {} } class C
/// extends P { protected function m() {} }` → `Fatal error: Access level to C::m()
/// must be public (as in class P)`. Widening is legal and silent (all three
/// widening directions witnessed clean), as is any redeclaration of a **private**
/// parent method — a private method is not inherited at all.
pub const OVERRIDE_VISIBILITY_WEAKENED_ID: &str = "override.visibility-weakened";

/// The registry id for a parameter type an override NARROWS (contravariance
/// broken): `class P { public function m(int|string $x) {} } class C extends P {
/// public function m(int $x) {} }` → `Fatal error: Declaration of C::m(int $x) must
/// be compatible with P::m(string|int $x)`. Widening is legal and silent
/// (`int` → `int|string`, `int` → `?int`, `array` → `iterable`, all witnessed).
pub const OVERRIDE_PARAMETER_VARIANCE_ID: &str = "override.parameter-variance";

/// The registry id for a return type an override WIDENS (covariance broken):
/// `class P { public function m(): int {} } class C extends P { public function m():
/// int|string {} }` → `Fatal error: Declaration of C::m(): string|int must be
/// compatible with P::m(): int`. Narrowing is legal and silent (`int|string` → `int`,
/// `?int` → `int`, `iterable` → `array`, `int` → `never`, all witnessed).
pub const OVERRIDE_RETURN_VARIANCE_ID: &str = "override.return-variance";

// end overriding family (ADR-0078, issue #184)

// docblock hygiene (ADR-0078, issue #186)
// ---------------------------------------------------------------------------
// Six **mechanics**-layer anti-rot ids about annotations that drifted from the
// code they annotate. Every premise here is TEXTUAL — the tag's subject either
// exists or it does not — which is what lets them be red on sight,
// suppression-exempt and `disable`-proof (ADR-0078 §1.5: the `phpdoc.*` prefix
// now spans the contract and mechanics layers, and a prefix pattern matching
// across layers still cannot demote the mechanics ones).
//
// The bounded-tag-set discipline governs all of them: Steins reads a bounded tag
// vocabulary (`steins_phpdoc::TagKind`) and deliberately drops unknown/vendor
// tags. An unrecognized tag is NEVER a finding; only rot INSIDE the read set is.
// ---------------------------------------------------------------------------

/// The registry id for a docblock tag **inside the read set** whose type payload
/// the phpdoc parser rejects outright (`parse_type` → `Err`) — a `@param` /
/// `@return` / `@var` / `@throws` whose envelope is therefore silently lost.
///
/// Scoped to *genuine* rejections that cannot be a parser limitation: the payload
/// must be bracket-balanced (an unbalanced one is the line-wrapped array-shape
/// spelling the line-based scanner truncates, not rot), and the by-ref /
/// variadic punctuation the scanner leaves behind (`@param int &$x` →
/// `"int &"`) is trimmed before parsing. What is left is the trailing-`|` union,
/// the non-type payload, and their kin.
pub const PHPDOC_UNPARSABLE_ID: &str = "phpdoc.unparsable";

/// The registry id for a `@param $name` on a function-like whose signature has no
/// parameter `$name` — the annotation outlived a rename or a removal. Variadic
/// (`...$args`) and by-ref (`&$x`) spellings count as the parameter existing:
/// both name a real parameter. A `@param` with no name token is not this finding.
pub const PHPDOC_STALE_PARAM_ID: &str = "phpdoc.stale-param";

/// The registry id for an adopted statement-level `@var Type $x` naming a
/// variable the adopted statement does not bind (merging PHPStan's
/// `varTag.variableNotFound` and `varTag.differentVariable` — the same rot, one
/// id). Adoption is the ADR-0073/0074 `stmt_docblock` rule, reused verbatim; the
/// firing shape is the plain assignment, whose bound name is textual.
pub const PHPDOC_STALE_VAR_ID: &str = "phpdoc.stale-var";

/// The registry id for a `@var` docblock in a position where the ADR-0073
/// adoption rule adopts **nothing** — no construct can follow it at all
/// (`SourceTree::docblock_adopts_nothing`). The property-`@var` position is legal
/// and consumed elsewhere, so it is never this finding.
pub const PHPDOC_MISPLACED_VAR_ID: &str = "phpdoc.misplaced-var";

/// The registry id for a `@throws X` whose `X` resolves to a class-like **proven
/// not to be a `Throwable`**: the hierarchy is fully enumerable and no ancestor is
/// `Throwable`. A `Maybe` answer is silence, and an unresolvable `X` is silence
/// too — the absence-family condition, not a finding.
pub const PHPDOC_THROWS_NOT_THROWABLE_ID: &str = "phpdoc.throws-not-throwable";

/// The registry id for a closure `use ($x)` the body never mentions. A by-ref
/// `use (&$x)` is an out-channel and never fires; a mention inside a nested
/// closure (its body *or* its own `use` clause) counts as used; and a body
/// holding `compact`/`extract`/`get_defined_vars`/`$$x`/`eval`/`include` dams the
/// whole closure — the scope-local dam.
pub const CLOSURE_UNUSED_USE_ID: &str = "closure.unused-use";

// ---------------------------------------------------------------------------
// untyped surface (ADR-0078, issue #200)
//
// The `untyped.*` family: **declaration reading only** — no inference, no
// receiver, no value domain, no sidecar. Each id reports a claim the code *does
// not make*, which is precisely the contract layer's business and precisely what
// keeps the family off the lint side of the boundary: a missing type is declared
// debt, a style opinion is not (ADR-0078 §2). The family is deliberately NOT
// `phpdoc.*`: that prefix reports a claim that *disagrees* with the code.
//
// The `@param`/`@return`/`@var` boundary is ADR-0078's: a claim ANYWHERE — even an
// `Asserted` docblock one, even a wrong one — makes the declaration typed here. A
// wrong claim is `phpdoc.*`'s finding, never this family's.
// ---------------------------------------------------------------------------

/// The registry id for a function-like **parameter** with no native type and no
/// `@param` covering it in the owning docblock. Variadic (`...$args`) and by-ref
/// (`&$x`) spellings still name the parameter. A promoted constructor parameter is
/// reported here — and *only* here, never again as `untyped.property`.
pub const UNTYPED_PARAMETER_ID: &str = "untyped.parameter";

/// The registry id for a function-like with no native return type and no
/// `@return`. `__construct` / `__destruct` are excluded by construction: PHP
/// forbids a return type on either, so their absence is not a claim withheld.
pub const UNTYPED_RETURN_ID: &str = "untyped.return";

/// The registry id for a property with no native type and no `@var` on its
/// declaration. Promoted constructor parameters belong to
/// [`UNTYPED_PARAMETER_ID`]'s arm instead (one declaration, one finding).
pub const UNTYPED_PROPERTY_ID: &str = "untyped.property";

/// The registry id for a class constant with no native (PHP 8.3) constant type and
/// no `@var`. Enum cases are excluded: a case's type *is* its enum.
///
/// The family's one `strict`-floor arm, and the only one whose silence withholds
/// nothing: a constant's initializer is a constant expression, so its type is
/// pinned by the declaration either way. The written type buys the interface
/// contract and the child-class covariance check, not information — a strict-tier
/// concern. See the registry row for the measurement.
pub const UNTYPED_CLASS_CONSTANT_ID: &str = "untyped.class-constant";

/// The registry id for a native `array` / `iterable` declaration (parameter,
/// return, or property) whose docblock leaves the **value type** unstated.
/// `array<T>`, `T[]`, `list<T>`, `iterable<T>` and an array shape all state it;
/// a bare `array` — natively, or restated bare in the docblock — does not.
pub const UNTYPED_ITERABLE_VALUE_ID: &str = "untyped.iterable-value";

/// The registry id for a docblock type naming a class that declares `@template`
/// parameters, written **without** type arguments (`@param Collection $c` where
/// `Collection` carries `@template T`).
pub const UNTYPED_GENERICS_ID: &str = "untyped.generics";
// end untyped surface (ADR-0078, issue #200)

/// The resolved FQN of `PHPStan\dumpType` (ADR-0053 §2), lowercase-normalized and
/// leading-`\`-stripped — the case-insensitive matching key (PHP function names are
/// case-insensitive).
pub const DUMP_TYPE_FQN: &str = "phpstan\\dumptype";

/// The resolved FQN of `PHPStan\dumpPhpDocType` (ADR-0053 §2), lowercase-normalized.
pub const DUMP_PHPDOC_TYPE_FQN: &str = "phpstan\\dumpphpdoctype";

/// The reserved dump-family FQNs (ADR-0053 §5), recognized case-insensitively by
/// resolved FQN even if userland defines the name. Undefined-function reporting
/// excludes this set because the fail-level dump diagnostic already accounts for
/// the call.
pub const DUMP_FQNS: &[&str] = &[DUMP_TYPE_FQN, DUMP_PHPDOC_TYPE_FQN];

/// Whether normalized `fqn` belongs to the reserved dump family.
#[must_use]
pub fn is_dump_family_fqn(fqn: &str) -> bool {
    DUMP_FQNS.contains(&fqn)
}

// preg pattern refusal (ADR-0078, issue #189)

/// The registry id for the invalid-pattern check (ADR-0078, **proof** layer, floor
/// `Default`): a `preg_*` call whose pattern argument is a proven literal that the
/// project's OWN PCRE refuses to compile.
///
/// Consequence, measured at PHP 8.5.9: an `E_WARNING` carrying PCRE's own
/// complaint, plus a useless return (`false` for `preg_match`/`preg_match_all`/
/// `preg_split`/`preg_grep`, `null` for the `preg_replace*`/`preg_filter` family).
/// A live-path break, hence **proof** — the same warning-plus-degraded-value shape
/// `offset.missing` carries, riding the same ADR-0049 §7 warning-handler lever:
/// silent under a declared `warning-handler = "null"` posture.
///
/// The refusal comes from the project's own PCRE, through the sidecar (ADR-0004),
/// never from Steins' own pattern reader — whose job stays deciding that the
/// pattern IS a proven literal worth asking about (#148/#149/#156/#168/#177). A
/// reader-derived refusal would report patterns PCRE compiles happily, which the
/// zero-FP bar forbids. No sidecar ⇒ no refusal ⇒ silence.
pub const PREG_INVALID_PATTERN_ID: &str = "preg.invalid-pattern";

// non-object receivers (ADR-0078, issue #190)

/// The registry id for a method call on a receiver proven to be a **non-object,
/// non-null** value (ADR-0078, issue #190): `$x = 1; $x->m();` is the same fatal
/// `Error` [`CALL_ON_NULL_ID`] names for `null`, with the runtime type in its
/// place — `Call to a member function m() on int` (witnessed at PHP 8.5.9 for
/// `int`, `string`, `float`, `true`, `false`, `array`).
///
/// A sibling id, not a widening of `call.on-null`: ADR-0022 makes an id's meaning
/// a contract, and `call.on-null` is already in baselines/`@steins-ignore`
/// comments. The two are disjoint by construction (null is
/// `Fact::Singleton(Val::Null)`, which this id's emitter refuses), so no site
/// reports both.
///
/// Premise: the four-layer value domain has no object denotation at all (`Val`
/// cannot spell one), so a receiver carrying a `Fact` is already proven not an
/// object; a receiver with no fact is silence (every `Maybe`-object, object-arm
/// union, unknown-class receiver). `?->` does not excuse it — nullsafe
/// short-circuits on `null` alone, so a proven non-null non-object still fatals
/// (witnessed: `$x = 1; $x?->m();`).
pub const CALL_ON_NON_OBJECT_ID: &str = "call.on-non-object";

/// The registry id for a property fetch on a receiver proven to be a **non-object,
/// non-null** value (ADR-0078, issue #190): `$x = 1; $y = $x->p;` raises
/// `Warning: Attempt to read property "p" on int` and evaluates to `null`
/// (witnessed at PHP 8.5.9 for `int`, `string`, `float`, `true`, `false`, `array`).
/// Warning-grade, riding the ADR-0049 §7 lever like `offset.missing`.
///
/// Unlike its `call.` sibling this id owns the proven-`null` receiver too: PHP
/// raises the same warning for it (`Attempt to read property "p" on null`), and
/// there is no `property.on-null` to defer to — carving null out would leave the
/// commonest receiver unreported. `call.on-non-object` carves null out only
/// because [`CALL_ON_NULL_ID`] already owns it.
pub const PROPERTY_ON_NON_OBJECT_ID: &str = "property.on-non-object";

// end non-object receivers (ADR-0078, issue #190)

/// `foreach.non-iterable` (ADR-0078, issue #192): a `foreach` subject proven — in
/// the same value-domain lane `offset.missing` reads — to be a non-array
/// scalar/`null`. PHP's own consequence (`php -r`-witnessed, 8.5.9):
/// `foreach() argument must be of type array|object, {type} given`; the loop body
/// is skipped entirely, not merely warned about. Single-id family like
/// `readonly.reassigned` — no companion "unsupported" arm, since every leg this
/// proves already IS the one runtime consequence.
///
/// Warning-grade, riding the same `warning-handler` gate as `offset.missing`
/// (ADR-0049 §7): silent under a declared `warning-handler = "null"` posture.
pub const FOREACH_NON_ITERABLE_ID: &str = "foreach.non-iterable";

// string context (ADR-0078, issue #193)

/// The registry id for an **object with no reachable `__toString`** put into string
/// context (ADR-0078, **proof** layer, floor `Default`): `"x $o"`, `echo $o`,
/// `print $o`, `(string) $o`, `'a' . $o`.
///
/// Witnessed on PHP 8.5.9, identically in all five contexts:
///
/// ```text
/// Error: Object of class A could not be converted to string
/// ```
///
/// A fatal, so proof layer, and **not** behind the ADR-0049 §7 warning-handler
/// gate (which demotes warning-grade findings only) — exactly why this is a
/// separate id from [`STRING_ARRAY_CONVERSION_ID`] rather than one id with a
/// precise message (ADR-0078 §1.4: an id demotes whole or not at all).
///
/// # What has to be proven
///
/// The receiver's class must be **exactly** known and its `__toString` provably
/// absent under complete enumeration — the same ladder `call.undefined-method`
/// walks. Silence cases: a `Stringable` implementor, a `__toString` inherited
/// from an unresolvable parent, a trait anywhere in the chain (traits are not
/// flattened), or an A14 magic-tag obstacle in the class-like's resolved reach.
///
/// The magic-tag leg is deliberate over-silence: measured on 8.5.9, `__call` does
/// **not** rescue a string conversion (`WithCall` still raises the `Error`), so no
/// docblock claim about magic members makes the conversion legal — reusing the
/// obstacle records instead keeps one enumerability rule rather than a laxer
/// second one.
pub const STRING_NON_STRINGABLE_ID: &str = "string.non-stringable";

/// The registry id for an **array** put into string context (ADR-0078, **proof**
/// layer, floor `Default`, behind the warning-handler gate).
///
/// Witnessed on PHP 8.5.9, identically in all five contexts:
///
/// ```text
/// Warning: Array to string conversion
/// ```
///
/// producing the literal string `"Array"` (`(string) [1,2,3]` is `"Array"`,
/// `'x' . [1,2,3]` is `"xArray"`) — the same warning-plus-degraded-value shape
/// `offset.missing` and `preg.invalid-pattern` carry, demoted off the proof
/// surface under a declared `warning-handler = "null"` posture (ADR-0049 §7).
///
/// Evidence is a value-domain fact alone — no class world, no sidecar. A `Maybe`
/// (`array|string`) proves nothing and is silence; an `Asserted` fact (a docblock
/// `@var array`) is not proof-layer evidence (ADR-0052 §5).
pub const STRING_ARRAY_CONVERSION_ID: &str = "string.array-conversion";

// end string context (ADR-0078, issue #193)

// parse failure (ADR-0079, issue #180)

/// The registry id for a file the parser could not read (ADR-0079 §2.1,
/// **mechanics** layer, floor `Default`): `SourceTree::parse` recovered from at
/// least one error, so `php -l` would reject the file outright.
///
/// Emitted **once per file**, positioned at the first error, naming the count of
/// further errors — one per file because recovery cascades make every later
/// position unreliable (a second "error" is as likely the recovery's own
/// confusion as a second mistake).
///
/// Full mechanics semantics: fail level, red on sight, profile-`disable`-proof
/// and undemotable, suppression-exempt. The finding is the only thing the broken
/// file emits (§2.4) — a finding built on a misparse would be the manufactured-FP
/// shape ADR-0002 forbids. A **non-vendor** broken file also joins the
/// whole-universe dam as [`DamKind::Unparsable`] (§2.2); in `vendor/` the
/// ADR-0046 §2 presumption carries over (§2.3): not a dam site, ordinary vendor
/// filter applies.
///
/// [`DamKind::Unparsable`]: crate::DamKind::Unparsable
pub const SYNTAX_UNPARSABLE_ID: &str = "syntax.unparsable";

// end parse failure (ADR-0079, issue #180)

// inaccessible members (ADR-0078, issue #185)

/// The registry id for a call to a method the call site's scope **cannot see**
/// (ADR-0078, issue #185): a `private` method called from outside its own
/// declaring class, or a `protected` one called from outside its hierarchy. PHP
/// raises a fatal `Error` before the body runs — `php -r`-witnessed at 8.5.9:
///
/// ```text
/// Call to private method C::m() from global scope
/// Call to private method A::m() from scope B     (private is NOT inherited)
/// Call to protected method A::m() from scope U
/// Call to private C::__construct() from global scope
/// ```
///
/// Consumes a predicate the resolver already computes and discards
/// (`private_blocked`): a blocked method resolves to `None`, so the call goes
/// unchecked by resolution (which must keep suppressing it — arity, effects and
/// every downstream consumer need `None` too) and is instead flagged by this
/// separate check.
///
/// The `__call`/`__callStatic` leg makes this non-trivial: PHP routes an
/// *inaccessible* call through the magic fallback exactly as an undefined one
/// (witnessed: a private `m()` plus `__call` prints `__call:m`, no error). So a
/// magic method anywhere in the receiver's chain, or an A14 `@method`/`@mixin`
/// tag in its reach, is silence — same terms as `call.undefined-method` leg (d).
/// The constructor is the one exception: `__call` does **not** rescue `new C()`
/// on a private `__construct`.
pub const CALL_INACCESSIBLE_METHOD_ID: &str = "call.inaccessible-method";

/// The registry id for a property read or write the site's scope **cannot see**
/// (ADR-0078, issue #185). A fatal `Error`, not the `E_WARNING`
/// [`PROPERTY_ON_NON_OBJECT_ID`] carries — `php -r`-witnessed at 8.5.9 for both
/// directions:
///
/// ```text
/// $c = new C; echo $c->p;   Cannot access private property C::$p
/// $c = new C; $c->p = 2;    Cannot access private property C::$p
/// $c = new C; echo $c->p;   Cannot access protected property C::$p
/// ```
///
/// A private property declared by an ancestor is NOT this id: PHP mangles a
/// private property into its declaring class's own slot, so a subclass simply
/// has no such name (`class A { private $p; } class B extends A {}`, `(new
/// B)->p` is `Warning: Undefined property: B::$p` — a different id). So the
/// declaration must sit on the receiver's own exact class for `private`, while
/// `protected` is inherited and fires from anywhere in the chain (witnessed:
/// `Cannot access protected property B::$p`).
///
/// `__get`/`__set` anywhere in the chain is silence, witnessed like the method
/// id's `__call`: an inaccessible read prints `__get:p`, a write `__set:p=5`,
/// neither erroring.
pub const PROPERTY_INACCESSIBLE_ID: &str = "property.inaccessible";

/// The registry id for a class-constant fetch the site's scope **cannot see**
/// (ADR-0078, issue #185): `Cannot access private constant C::K` /
/// `Cannot access protected constant B::K`, both fatal `Error`s witnessed at PHP
/// 8.5.9.
///
/// Constants have no magic fallback at all (`__get`/`__callStatic` witnessed not
/// to intercept `C::K`), so the obstacle leg that shapes the other two ids is
/// absent here; only the shared hierarchy-enumeration closure remains.
///
/// Same private/protected asymmetry as the property id: `class A { private
/// const K = 1; } class B extends A {}` with `B::K` is `Error: Undefined
/// constant B::K` (absence, not inaccessibility), while `protected` gives
/// `Cannot access protected constant B::K`. Naming the declaring class directly
/// (`A::K` from a subclass's scope) is inaccessibility either way, and fires.
pub const CLASS_CONST_INACCESSIBLE_ID: &str = "class-const.inaccessible";

// end inaccessible members (ADR-0078, issue #185)

// member absence (ADR-0078, issue #197)

/// The registry id for a **read** of a property no declaration in the receiver's
/// hierarchy provides (ADR-0078, issue #197) — the member-kind twin of
/// [`CALL_UNDEFINED_METHOD_ID`], and PHPStan's single highest-volume identifier.
///
/// `php -r`-witnessed at PHP 8.5.9:
///
/// ```text
/// class C { public int $a = 1; } $c = new C; var_dump($c->nope);
///   Warning: Undefined property: C::$nope
///   NULL
/// ```
///
/// Warning-grade (the `offset.missing` shape): read yields `null`, program keeps
/// running. Proof layer at the `Default` floor, behind the ADR-0049 §7
/// warning-handler gate (settled once, 2026-08-08 amendment, for
/// `variable.undefined`, `foreach.non-iterable` and this id alike).
///
/// # The ladder (ADR-0049 §4, per member kind)
///
/// The method ladder with the property's own obstacles substituted for
/// `__call`, plus what PHP 8.2 made of dynamic properties:
///
/// * `__get`/`__set`/`__isset` anywhere in the chain — only `__get` truly
///   rescues a read (witnessed: prints `__get:nope`; `__isset`/`__set` alone
///   still warn), but all three are taken as obstacles (over-silence, one
///   enumerability rule — the [`STRING_NON_STRINGABLE_ID`] precedent).
/// * `#[AllowDynamicProperties]` anywhere in the chain, re-licensing the write
///   PHP 8.2 deprecated.
/// * `stdClass` and its descent: silence entirely, reads included. A
///   never-written read on `stdClass` really does warn (witnessed), but its
///   property bag may have been written anywhere (deliberate v1
///   conservatism; needs no separate leg since `stdClass` is not a project
///   declaration).
/// * A project-wide dynamic-write obstacle (`SourceTree::property_write_names`):
///   any name written anywhere could have been created dynamically here (a
///   plain-class dynamic write is deprecated, not an error). A computed-name
///   write (`$o->$n = …`) anywhere takes the id off the surface.
/// * Everything the method ladder already had: the A14 magic-tag obstacle, a
///   trait name/using node, an enum node, an unresolvable/`Ambiguous`/builtin
///   ancestor, a member-incomplete file (ADR-0079 §2.5), a cycle, the A2i
///   conditional-declaration re-dam, and the A2ii boot-surface homonym leg.
///
/// A **declared** property is silence however spelled — plain, `static` (safe
/// under-firing), promoted, `readonly`, inherited, or hooked (kept in
/// `ClassDecl::hooked_properties` so an absence claim cannot miss it). A
/// `private` property declared by an *ancestor* is genuinely absent on the
/// child (witnessed: `Warning: Undefined property: B1::$p`) but treated as
/// present here — v1 under-fires rather than reason about name mangling,
/// keeping this id disjoint from [`PROPERTY_INACCESSIBLE_ID`].
pub const PROPERTY_UNDEFINED_ID: &str = "property.undefined";

/// The `maybe-` sibling of [`PROPERTY_UNDEFINED_ID`] (ADR-0078 §1.3): the
/// declared-shape possibly-grade leg — a read whose receiver was narrowed to a
/// **union** of declared types, where the §8 ladder proves the property absent
/// on some arms and finds it declared on the rest. Proof layer at the `Strict`
/// floor, the `offset.maybe-missing` precedent. Registered ahead of emission in
/// v0.1.4, emitting since ADR-0081 §7 (issue #267) — shares that ADR only
/// because the pair was registered together, carrying no reachability premise
/// of its own (the arms are a union of declared types, not control-flow paths).
///
/// Disjoint from [`PROPERTY_UNDEFINED_ID`] by a partition, not a filter: every
/// arm absent is the definite id, some arms absent is this one, and a single
/// arm the ladder cannot close silences both.
pub const PROPERTY_MAYBE_UNDEFINED_ID: &str = "property.maybe-undefined";

/// The registry id for a class-constant fetch no declaration in the receiver's
/// member reach provides (ADR-0078, issue #197).
///
/// `php -r`-witnessed at PHP 8.5.9 — a **fatal `Error`**, with no gate and no
/// posture that survives it:
///
/// ```text
/// class C { const K = 1; } echo C::NOPE;   Error: Undefined constant C::NOPE
/// enum Suit { case Hearts; } Suit::Nope    Error: Undefined constant Suit::Nope
/// interface I { const IK = 1; } I::NOPE    Error: Undefined constant I::NOPE
/// ```
///
/// The cleanest member in the family: PHP gives it **no magic channel at all**
/// (witnessed at 8.5.9, per #185: a class carrying both `__get` and
/// `__callStatic` still raises `Error: Undefined constant Magic::NOPE`). What
/// remains is enumeration, wider than a method's: a constant may come from the
/// parent chain, **any interface in the reach** (`class C implements I`
/// answers `C::IK`; `interface IB extends IA` carries `IA`'s constants through
/// to `CB::AK` — both witnessed), a **trait** the class uses (`CT::TK`,
/// witnessed 8.2+, so `uses_traits` is an obstacle), or an enum's **cases**
/// (`Suit::Hearts`).
///
/// `X::class` is excluded at the site: it is a plain string since PHP 8.0 and
/// errors on nothing (witnessed on an undefined class name).
pub const CLASS_CONST_UNDEFINED_ID: &str = "class-const.undefined";

// end member absence (ADR-0078, issue #197)

// return missing (ADR-0078, issue #199)

/// The registry id for a function-like that **runs off the end of its body** while
/// declaring a native return type PHP demands a value for (ADR-0078, issue #199).
///
/// Witnessed on PHP 8.5.9: a fatal `TypeError` at the moment control reaches the
/// closing brace (`f(): Return value must be of type int, none returned`), not at
/// declaration time. A live-path break, so **proof** layer at the `Default` floor,
/// and not behind the ADR-0049 §7 warning-handler gate — no declared posture makes
/// a `TypeError` survivable.
///
/// `type.*` rather than `return.*` because ADR-0078 §1 puts an id in the family
/// its premise names — the written native return declaration, the same
/// Verified evidence [`RETURN_MISMATCH_ID`] reads, asked one step earlier.
///
/// # The definite/possibly split
///
/// A `FallsThrough` body comes in two populations (measured on the corpus, 26
/// findings, 2026-08-08), floored differently (ADR-0078 §1.3):
///
/// * **no function exit anywhere** — every execution reaches the brace, fatal
///   unconditional. **This id**, `Default` floor.
/// * **an exit somewhere, not covering every path** — a no-`default` `switch`
///   whose cases all return, an `if` with no `else`: real edge, but may be
///   taken only for inputs the program never produces (phpstan-src's `src/`
///   carries two such shapes and passes its own missing-return rule).
///   [`TYPE_RETURN_MAYBE_MISSING_ID`], `Strict` floor.
///
/// Discriminator: [`body_has_terminator`], a separate question from the fold.
///
/// # The two premises, both required
///
/// 1. **The declaration demands a value** — a written, non-`void`, non-`never`
///    hint from `Scope::ret_hint` (the *raw* hint: `: array`/`: mixed`/`: self`
///    lower to no `NativeType` yet all three fatal identically). `?int` and
///    `int|string` demand one too. A generator body is excluded (its declared
///    type describes the `Generator` the *call* produces, ADR-0057 §5), and
///    `never` is excluded (its fall-through is a different fatal).
/// 2. **The body provably falls through** ([`BodyEnd::provably_falls_through`]):
///    an undecided body (`try`/`catch`, `goto`, unstructurable `switch`) counts
///    as terminating.
///
/// Excluded by construction: abstract/interface methods (no concrete `Scope`),
/// `__construct`/`__destruct` (PHP forbids a return type), and arrow functions.
///
/// # The recorded obstacle: never-returning callees
///
/// `function g(): never { exit(1); } function f(): int { g(); }` runs clean
/// (witnessed 8.5.9): a scope containing such a call to a resolvable callee
/// declaring `: never` is silent, whole. A callee that never returns without
/// *declaring* it — the legacy `function redirect($u) { header(…); exit; }` —
/// is not modelled, and is this id's one named over-report risk.
///
/// [`body_has_terminator`]: steins_syntax::body_has_terminator
/// [`BodyEnd::provably_falls_through`]: steins_syntax::BodyEnd::provably_falls_through
pub const TYPE_RETURN_MISSING_ID: &str = "type.return-missing";

/// The registry id for the **possibly** leg of [`TYPE_RETURN_MISSING_ID`]
/// (ADR-0078 §1.3's `maybe-` convention, issue #199): a function-like whose body
/// falls through *and* returns, throws or exits somewhere — so the fatal is real
/// but reached only along the paths the returns do not cover.
///
/// The consequence is the same fatal, witnessed identically:
///
/// ```text
/// TypeError: f(): Return value must be of type int, none returned
/// ```
///
/// so this is **proof** layer like its definite sibling, but at `Strict` floor —
/// the first proof-layer id at that rung: registered and emitted rather than
/// scoped out of existence (ADR-0078 §1.3), but silent on a bare `steins check`.
///
/// # Why the floor, in one measurement
///
/// phpstan-src's `src/` passes PHPStan's own `MissingReturnRule` and still carries
/// this shape twice — `TypeNodeResolver.php:697` and `ClassNameUsageLocation.php:128`,
/// each a no-`default` `switch` over a string whose every case returns. The escape
/// edge exists in the CFG; the inputs that take it do not exist in the program.
/// Reporting that at `Default` would be the crying-wolf failure the floor ladder
/// exists to prevent; `Strict` keeps it named, addressable and measurable.
///
/// Every premise, silence leg and recorded obstacle is [`TYPE_RETURN_MISSING_ID`]'s
/// — the two ids differ in exactly one predicate, [`body_has_terminator`], and are
/// disjoint by construction, so no site can ever report both.
///
/// [`body_has_terminator`]: steins_syntax::body_has_terminator
pub const TYPE_RETURN_MAYBE_MISSING_ID: &str = "type.return-maybe-missing";

// end return missing (ADR-0078, issue #199)

// invalid operands (ADR-0078, issue #191)

/// `type.invalid-operand` (ADR-0078, issue #191): an arithmetic, bitwise, shift
/// or unary operator applied to operands PHP's own table refuses with a
/// `TypeError`. One id for the whole operator family (binary, unary,
/// comparison), **fatal rows only** — a row that merely warns or deprecates is
/// not this finding at any posture, so the id carries no `warning-handler` gate.
///
/// # The table
///
/// Every row `php -r`-witnessed at **PHP 8.5.9**, with runtime variables so the
/// compiler could not constant-fold the expression into a compile-time error.
/// Operand words below are the *proven* kinds: `array`, `fatal-string` (a string
/// literal with **no leading numeric prefix** at all — `'abc'`, `''`, `' '`,
/// `'e5'`, `'INF'`; `'5abc'` has a prefix, merely warns, and is *not* this id),
/// `string` (any other proven string), `int`/`float`/`bool`/`null`.
///
/// | operator(s) | fatal when | witness |
/// | --- | --- | --- |
/// | `+` | one operand `array`, the other **not** `array` | `[] + 1` → `TypeError: Unsupported operand types: array + int` |
/// | `+` | either operand `fatal-string` | `'abc' + 1`, `'' + 1`, `'abc' + 'abc'`, `'abc' + null` → `Unsupported operand types: string + …` |
/// | `-` `*` `/` `%` `**` `<<` `>>` | either operand `array` (**including** `array` on both sides) | `[] - []` → `Unsupported operand types: array - array` |
/// | `-` `*` `/` `%` `**` `<<` `>>` | either operand `fatal-string` | `'abc' * 2`, `'abc' << '5'` → `Unsupported operand types: string … ` |
/// | `&` `\|` `^` | either operand `array` | `[] & 1` → `Unsupported operand types: array & int` |
/// | `&` `\|` `^` | one operand `fatal-string` and the other **not** a string | `'abc' & 1` → `TypeError`; `'abc' & 'abc'` is the byte-wise operator and is **legal** (`'abc'`) |
/// | unary `-` / `+` | operand `array` or `fatal-string` | `-[]` → `Unsupported operand types: array * int` (the engine compiles unary minus as `* -1`) |
/// | unary `~` | operand `array`, `bool` or `null` | `~[]` → `Cannot perform bitwise not on array`; `~true` → `… on true` |
///
/// The survivors that make this the highest-FP-risk port, each also witnessed:
/// `[] + []` is the array **union**; `'5' + 5` is `10`, and so are `' 5' + 5`,
/// `'5 ' + 5`, `'5.5' + 5`, `'017' + 5`; `null + 1` is `1`; `true + 1` is `2`;
/// `1.5 + 1` is `2.5`; `'abc' & 'abc'` and `~'abc'` are the byte-wise string
/// operators; and every **comparison** — `<`, `>`, `<=`, `>=`, `<=>`, `==`,
/// `===` and their negations — is legal for *every* operand pair, arrays and
/// objects included (`[] < 1` is `false`, `[] <=> 1` is `1`), so
/// PHPStan's `InvalidComparisonOperationRule` folds into this id with **zero**
/// rows.
///
/// # Version sensitivity
///
/// None, deliberately: the two moving boundaries — non-numeric-string arithmetic
/// and array arithmetic becoming `TypeError` — both moved in **PHP 8.0**, and
/// the workspace floor is 8.1 (ADR-0011). Every row holds unchanged across
/// 8.1…8.5, so no row consults `Folder::php_minor()`.
///
/// # Objects
///
/// Silence by construction, and correctly so: PHP has no userland operator
/// overloading, so any plain object in `+` is a `TypeError` (witnessed: `new
/// stdClass() + 1`) — but internal classes DO overload (`GMP` arithmetic is
/// the standard counterexample), and the four-layer value domain has no object
/// denotation at all ([`Val`] cannot spell one). An object operand carries no
/// `Fact`, resolves to no operand kind, and is silent without a special case.
///
/// [`Val`]: steins_domain::Val
pub const INVALID_OPERAND_ID: &str = "type.invalid-operand";

// end invalid operands (ADR-0078, issue #191)

// undefined variables (ADR-0078, issue #194)

/// `variable.undefined` (ADR-0078, issue #194, **proof** layer, floor `Default`): a
/// read of a name its scope **never binds**, by any binding form, anywhere in the
/// scope. PHP's own consequence (`php -r`-witnessed, 8.5.9):
/// `Warning: Undefined variable $x`, and the read evaluates to `null`.
///
/// Warning-plus-a-degraded-value is `offset.missing`'s shape, so it rides the
/// same ADR-0049 §7 lever: silent under a declared `warning-handler = "null"`
/// posture.
///
/// The premise is deliberately weaker than PHP's own and reachability-blind: it
/// fires only when the name has **no** binding form at all in the scope's text —
/// not a parameter, assignment target (compound, destructuring, `list()`, offset
/// or property write), `global`/`static` declaration, closure `use`, `catch`
/// binding, `foreach` binding, or out-parameter position. Ordering/branching are
/// ignored on purpose: a read that *precedes* its only assignment is
/// [`VARIABLE_MAYBE_UNDEFINED_ID`]'s territory instead, and needs the
/// reachability foundation (issue #199) — the two are disjoint by construction.
///
/// Computed at lowering (`Scope::undefined_reads`), which already accounts for
/// binding forms, `isset`/`empty`/`??`/`unset`/`@` guards, superglobal/`$this`
/// exclusions and the `extract`/`compact`/`$$x`/`eval`/`include` scope dam. The
/// checker adds one premise lowering cannot reach: whether a bare `$x` argument
/// at a statically-named call is an out-parameter (ADR-0077's by-value oracle,
/// which needs the cross-file index).
pub const VARIABLE_UNDEFINED_ID: &str = "variable.undefined";

/// `variable.maybe-undefined` (ADR-0078, issue #194, **proof** layer, floor
/// `Strict`): a read of a name bound on only *some* paths reaching it — PHPStan's
/// `checkMaybeUndefinedVariables`. Registered ahead of emission in v0.1.4,
/// emitting since the binding-presence pass landed (ADR-0081, issue #267): the
/// firing set is `Scope::maybe_undefined_reads`, computed at lowering by a
/// statement-ordered walk over a three-valued presence lattice that subtracts a
/// provably-terminating branch arm, iterates loop bodies to a fixpoint and
/// consumes `isset`/`empty` guards with polarity.
///
/// `strict` floor, not `default`, because the claim is weaker: `variable.undefined`
/// proves the binding absent from the whole scope, this one only that *a* path
/// reaches the read unbound — a shape defensive house styles produce on purpose.
///
/// Disjoint from [`VARIABLE_UNDEFINED_ID`] by construction, not a filter: this id
/// fires only where the scope binds the name **somewhere**. The use-before-assign
/// shape (`$y = $x; $x = 1;`) stays here even though no path reaches the read
/// bound — promoting it would break the definite id's ordering-blindness
/// (ADR-0081, non-goal 1).
pub const VARIABLE_MAYBE_UNDEFINED_ID: &str = "variable.maybe-undefined";

// end undefined variables (ADR-0078, issue #194)

// unset pseudo-type (ADR-0087 §4, issue #396)

/// `phpdoc.maybe-undefined` (ADR-0087 §4, issue #396, **contract** layer, floor
/// `Contracts`): a read of a top-level variable the author declared
/// `/** @var T|unset $x */`, at a point where nothing has discharged the
/// possibly-undefined state the `unset` member states.
///
/// **Not [`VARIABLE_MAYBE_UNDEFINED_ID`], and the split is the layer split.** That
/// id's premise is a reachability fact the lowering pass computes from the CST,
/// which is why it is `Layer::Proof`. This one's premise is a *declaration* — an
/// author's assertion, unverifiable by definition — so it belongs with the rest of
/// the phpdoc-premised family, and reports one surface lower (`Contracts`, its
/// definite sibling `phpdoc.param-mismatch`'s rung) because a declared
/// possibly-undefined read is a stated fact rather than an inferred one. Sharing
/// ADR-0081's id would put an `Asserted` premise behind a proof-layer id, which the
/// layer split exists to prevent (ADR-0052 §5).
///
/// The firing set is `SourceTree::unset_seed_facts`, computed at lowering by
/// ADR-0081's own presence pass over the top-level statement list — same lattice,
/// same polarity engine, so `isset($x)`, `!isset($x)`/`empty($x)` early exits,
/// `??` / `??=`, an assignment and the defaulting idiom all discharge the state
/// exactly as they do for the proof-layer sibling. Those candidates are unconfirmed
/// by construction (`steins-syntax` cannot lower a phpdoc type); this checker lowers
/// the named tag and drops every candidate whose declaration has no `unset` member.
///
/// **Deliberately not gated on the ADR-0049 §7 warning-handler posture**, unlike the
/// `variable.*` pair. That lever exists for findings whose whole claim is "PHP emits
/// a warning here" — a project that has installed a fatal-on-warning handler has
/// changed what the warning *means*, so `offset.missing` and the `variable.*` pair
/// ride it. This id's claim is that the read contradicts the file's own docblock,
/// which is true whatever the runtime does with the warning, and it is judged on the
/// contract layer where no runtime posture is consulted at all.
pub const PHPDOC_MAYBE_UNDEFINED_ID: &str = "phpdoc.maybe-undefined";

// end unset pseudo-type (ADR-0087 §4, issue #396)

/// Every id constant that reaches a `Diagnostic { id: … }` construction site — the
/// canonical enumeration of what the emitters can produce (ADR-0050 §2 totality).
///
/// **Invariant, checked by the workspace totality test** (`tests/registry.rs`):
/// this list and [`DIAGNOSTIC_REGISTRY`] are the same set, both directions — so a
/// new emitter whose id is added here but not registered (or the reverse) fails to
/// build the tests. Adding a `*_ID` constant and emitting it therefore *forces*
/// both a registry entry (with a layer) and an entry here. The two live in
/// different files on purpose: the registry carries the layer attribute, this
/// carries "is emitted", and the test binds them.
///
/// `SUPPRESS_UNMATCHED_ID` / `SUPPRESS_UNKNOWN_ID` are emitted from
/// [`suppress`] and so are covered via the registry side of the test.
///
/// [`DIAGNOSTIC_REGISTRY`]: suppress::DIAGNOSTIC_REGISTRY
pub const ALL_EMITTABLE_IDS: &[&str] = &[
    ID,
    RETURN_ID,
    CALL_ON_NULL_ID,
    PROP_MISMATCH_ID,
    READONLY_REASSIGNED_ID,
    PARAM_MISMATCH_ID,
    RETURN_MISMATCH_ID,
    PHPDOC_PROP_MISMATCH_ID,
    THROW_UNDECLARED_ID,
    THROW_LISKOV_ID,
    EFFECT_ID,
    EFFECT_LISKOV_ID,
    UNKNOWN_LABEL_ID,
    // interop-label hygiene (ADR-0082 amendment, issue #311)
    INTEROP_UNKNOWN_LABEL_ID,
    CALL_UNDEFINED_METHOD_ID,
    OFFSET_MISSING_ID,
    OFFSET_ON_UNSUPPORTED_ID,
    OFFSET_UNDECLARED_ID,
    OFFSET_MAYBE_MISSING_ID,
    PHPDOC_UNDEFINED_METHOD_ID,
    ARRAY_DUPLICATE_KEY_ID,
    CALL_TOO_FEW_ARGUMENTS_ID,
    CALL_UNKNOWN_NAMED_ARGUMENT_ID,
    CALL_UNDEFINED_FUNCTION_ID,
    CLASS_UNDEFINED_ID,
    DEBUG_TYPE_ID,
    DEBUG_PHPDOC_TYPE_ID,
    DEBUG_VAR_DUMP_ID,
    DEBUG_TRACE_ID,
    suppress::SUPPRESS_UNMATCHED_ID,
    suppress::SUPPRESS_UNKNOWN_ID,
    // printf arity (ADR-0078, issue #188)
    CALL_PRINTF_TOO_FEW_ARGUMENTS_ID,
    // end printf arity (ADR-0078, issue #188)
    // declaration-incompatibility fatals (ADR-0078, issue #183)
    CLASS_ABSTRACT_UNIMPLEMENTED_ID,
    CLASS_EXTENDS_FINAL_ID,
    // overriding family (ADR-0078, issue #184)
    OVERRIDE_FINAL_ID,
    OVERRIDE_STATIC_MISMATCH_ID,
    OVERRIDE_VISIBILITY_WEAKENED_ID,
    OVERRIDE_PARAMETER_VARIANCE_ID,
    OVERRIDE_RETURN_VARIANCE_ID,
    // end overriding family (ADR-0078, issue #184)
    // docblock hygiene (ADR-0078, issue #186)
    PHPDOC_UNPARSABLE_ID,
    PHPDOC_STALE_PARAM_ID,
    PHPDOC_STALE_VAR_ID,
    PHPDOC_MISPLACED_VAR_ID,
    PHPDOC_THROWS_NOT_THROWABLE_ID,
    CLOSURE_UNUSED_USE_ID,
    // preg pattern refusal (ADR-0078, issue #189)
    PREG_INVALID_PATTERN_ID,
    // non-object receivers (ADR-0078, issue #190)
    CALL_ON_NON_OBJECT_ID,
    PROPERTY_ON_NON_OBJECT_ID,
    // end non-object receivers (ADR-0078, issue #190)
    // foreach subject (ADR-0078, issue #192)
    FOREACH_NON_ITERABLE_ID,
    // end foreach subject (ADR-0078, issue #192)
    // string context (ADR-0078, issue #193)
    STRING_NON_STRINGABLE_ID,
    STRING_ARRAY_CONVERSION_ID,
    // end string context (ADR-0078, issue #193)
    // parse failure (ADR-0079, issue #180)
    SYNTAX_UNPARSABLE_ID,
    // end parse failure (ADR-0079, issue #180)
    // inaccessible members (ADR-0078, issue #185)
    CALL_INACCESSIBLE_METHOD_ID,
    PROPERTY_INACCESSIBLE_ID,
    CLASS_CONST_INACCESSIBLE_ID,
    // end inaccessible members (ADR-0078, issue #185)
    // member absence (ADR-0078, issue #197)
    PROPERTY_UNDEFINED_ID,
    // The declared-shape possibly leg, emitting since ADR-0081 (issue #267).
    PROPERTY_MAYBE_UNDEFINED_ID,
    CLASS_CONST_UNDEFINED_ID,
    // end member absence (ADR-0078, issue #197)
    // untyped surface (ADR-0078, issue #200)
    UNTYPED_PARAMETER_ID,
    UNTYPED_RETURN_ID,
    UNTYPED_PROPERTY_ID,
    UNTYPED_CLASS_CONSTANT_ID,
    UNTYPED_ITERABLE_VALUE_ID,
    UNTYPED_GENERICS_ID,
    // end untyped surface (ADR-0078, issue #200)
    // return missing (ADR-0078, issue #199)
    TYPE_RETURN_MISSING_ID,
    TYPE_RETURN_MAYBE_MISSING_ID,
    // end return missing (ADR-0078, issue #199)
    // invalid operands (ADR-0078, issue #191)
    INVALID_OPERAND_ID,
    // end invalid operands (ADR-0078, issue #191)
    // global constants (ADR-0078, issue #198)
    CONSTANT_UNDEFINED_ID,
    // end global constants (ADR-0078, issue #198)
    // undefined variables (ADR-0078, issue #194)
    VARIABLE_UNDEFINED_ID,
    // The some-paths sibling, emitting since the binding-presence pass landed
    // (ADR-0081, issue #267).
    VARIABLE_MAYBE_UNDEFINED_ID,
    // end undefined variables (ADR-0078, issue #194)
    // unset pseudo-type (ADR-0087 §4, issue #396): the declared possibly-undefined
    // read, premised on the docblock rather than on reachability.
    PHPDOC_MAYBE_UNDEFINED_ID,
    // the argument side's possibly grade (ADR-0081 amendment, issue #391): one
    // judgment, two ids, routed by the premise's minimum stratum.
    TYPE_MAYBE_ARGUMENT_MISMATCH_ID,
    PHPDOC_MAYBE_ARGUMENT_MISMATCH_ID,
    // sentinel parameter (ADR-0088 §4, issue #428): the never-declared carve-out
    // out of `phpdoc.param-mismatch`.
    NEVER_PARAM_REACHABLE_ID,
    // the hyphen reservation's diagnostic (ADR-0091 §6, issue #479).
    PHPDOC_UNKNOWN_VOCABULARY_ID,
];

/// Ids **registered ahead of emission**: they exist in [`DIAGNOSTIC_REGISTRY`]
/// (so `@steins-ignore` can name them and their layer is pinned) but no emitter
/// produces them yet. An id moves into [`ALL_EMITTABLE_IDS`] once its emitter lands.
///
/// The totality test (`tests/registry.rs`) keeps this honest: every registered id
/// must be in `ALL_EMITTABLE_IDS ∪ REGISTERED_NOT_YET_EMITTED`, the two lists are
/// **disjoint**, and every id here must actually be registered. An emitted id
/// missing from `ALL_EMITTABLE_IDS` still fails forward totality.
///
/// [`DIAGNOSTIC_REGISTRY`]: suppress::DIAGNOSTIC_REGISTRY
pub const REGISTERED_NOT_YET_EMITTED: &[&str] = &[
    // The too-many-arguments arm fires for INTERNAL targets only (userland
    // too-many runs clean), so it waits for the reflect slice (M2).
    CALL_TOO_MANY_ARGUMENTS_ID,
];
