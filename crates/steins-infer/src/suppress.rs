//! Inline `@steins-ignore` suppression (ADR-0023), following `@phpstan-ignore`'s
//! spec verbatim.
//!
//! `@steins-ignore <id-list> (optional reason)` suppresses matching object-level
//! diagnostics. Placement copies `@phpstan-ignore`: a comment **trailing code on
//! a line** suppresses findings on *that* line; a comment **alone on its own
//! line** suppresses findings on the *next* line ([`SourceTree::is_line_leading`]
//! draws the distinction).
//!
//! IDs are registry-governed ([`DIAGNOSTIC_IDS`]) under ADR-0022 prefix semantics
//! (`type.*`/bare `type` matches `type.argument-mismatch`). Two always-on
//! meta-diagnostics guard the channel, both reported at the comment and both
//! **exempt** from suppression (suppressing the suppressor would loop):
//! [`SUPPRESS_UNMATCHED_ID`] (an ignore id matching nothing on its target line)
//! and [`SUPPRESS_UNKNOWN_ID`] (an unknown/malformed id).

use std::collections::HashSet;

use steins_syntax::SourceTree;

use crate::project::Diagnostic;
use crate::{
    ARRAY_DUPLICATE_KEY_ID, CALL_ON_NULL_ID, CALL_PRINTF_TOO_FEW_ARGUMENTS_ID,
    CALL_TOO_FEW_ARGUMENTS_ID, CALL_TOO_MANY_ARGUMENTS_ID, CALL_UNDEFINED_FUNCTION_ID,
    CALL_UNDEFINED_METHOD_ID, CALL_UNKNOWN_NAMED_ARGUMENT_ID, CLASS_UNDEFINED_ID,
    DEBUG_PHPDOC_TYPE_ID, DEBUG_TRACE_ID, DEBUG_TYPE_ID, DEBUG_VAR_DUMP_ID, EFFECT_ID,
    CLASS_ABSTRACT_UNIMPLEMENTED_ID, CLASS_EXTENDS_FINAL_ID,
    EFFECT_LISKOV_ID, FOREACH_NON_ITERABLE_ID, ID, INTEROP_UNKNOWN_LABEL_ID, INVALID_OPERAND_ID,
    NEVER_PARAM_REACHABLE_ID,
    OFFSET_MAYBE_MISSING_ID,
    OFFSET_MISSING_ID,
    OFFSET_ON_UNSUPPORTED_ID, OFFSET_UNDECLARED_ID,
    PARAM_MISMATCH_ID, PHPDOC_MAYBE_ARGUMENT_MISMATCH_ID, TYPE_MAYBE_ARGUMENT_MISMATCH_ID,
    PHPDOC_MAYBE_RETURN_MISMATCH_ID, TYPE_MAYBE_RETURN_MISMATCH_ID,
    PHPDOC_PROP_MISMATCH_ID, PHPDOC_UNDEFINED_METHOD_ID, PREG_INVALID_PATTERN_ID, PROP_MISMATCH_ID,
    READONLY_REASSIGNED_ID,
    RETURN_ID, RETURN_MISMATCH_ID, THROW_LISKOV_ID, THROW_UNDECLARED_ID, UNKNOWN_LABEL_ID,
};
// string context (ADR-0078, issue #193)
use crate::{STRING_ARRAY_CONVERSION_ID, STRING_NON_STRINGABLE_ID};
// docblock hygiene (ADR-0078, issue #186)
use crate::{
    CLOSURE_UNUSED_USE_ID, PHPDOC_MISPLACED_VAR_ID, PHPDOC_STALE_PARAM_ID, PHPDOC_STALE_VAR_ID,
    PHPDOC_THROWS_NOT_THROWABLE_ID, PHPDOC_UNPARSABLE_ID,
};
// non-object receivers (ADR-0078, issue #190)
use crate::{CALL_ON_NON_OBJECT_ID, PROPERTY_ON_NON_OBJECT_ID};
// parse failure (ADR-0079, issue #180)
use crate::SYNTAX_UNPARSABLE_ID;
// inaccessible members (ADR-0078, issue #185)
use crate::{CALL_INACCESSIBLE_METHOD_ID, CLASS_CONST_INACCESSIBLE_ID, PROPERTY_INACCESSIBLE_ID};
// member absence (ADR-0078, issue #197)
use crate::{CLASS_CONST_UNDEFINED_ID, PROPERTY_MAYBE_UNDEFINED_ID, PROPERTY_UNDEFINED_ID};
// untyped surface (ADR-0078, issue #200)
use crate::{
    UNTYPED_CLASS_CONSTANT_ID, UNTYPED_GENERICS_ID, UNTYPED_ITERABLE_VALUE_ID,
    UNTYPED_PARAMETER_ID, UNTYPED_PROPERTY_ID, UNTYPED_RETURN_ID,
};
// return missing (ADR-0078, issue #199)
use crate::{TYPE_RETURN_MAYBE_MISSING_ID, TYPE_RETURN_MISSING_ID};
// overriding family (ADR-0078, issue #184)
use crate::{
    OVERRIDE_FINAL_ID, OVERRIDE_PARAMETER_VARIANCE_ID, OVERRIDE_RETURN_VARIANCE_ID,
    OVERRIDE_STATIC_MISMATCH_ID, OVERRIDE_VISIBILITY_WEAKENED_ID,
};
// global constants (ADR-0078, issue #198)
use crate::CONSTANT_UNDEFINED_ID;
// undefined variables (ADR-0078, issue #194)
use crate::{VARIABLE_MAYBE_UNDEFINED_ID, VARIABLE_UNDEFINED_ID};
// unset pseudo-type (ADR-0087 §4, issue #396)
use crate::PHPDOC_MAYBE_UNDEFINED_ID;
// the hyphen reservation's diagnostic (ADR-0091 §6, issue #479)
use crate::PHPDOC_UNKNOWN_VOCABULARY_ID;

/// The registry id for an `@steins-ignore` whose diagnostic id matches nothing on
/// its target line (ADR-0023 anti-rot). Exempt from suppression.
pub const SUPPRESS_UNMATCHED_ID: &str = "suppress.unmatched";

/// The registry id for an unknown/malformed diagnostic id in an `@steins-ignore`
/// (ADR-0022 registry-governed). Exempt from suppression.
pub const SUPPRESS_UNKNOWN_ID: &str = "suppress.unknown-id";

/// The **diagnostic layer** an id carries (ADR-0050 §1): semantic identity, not a
/// severity grade. The fp-gate (ADR-0050 §9) and user surfaces key on the layer,
/// not on a string prefix (`throw.*` is a config convenience only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Layer {
    /// Runtime survivability: provably breaks on a live path. Zero-FP bar; red on
    /// sight (ADR-0013).
    Proof,
    /// Declared-contract acceptance: a proven behavior violates a self-declared
    /// contract; the program still works. TRUE findings legitimately abound in
    /// released code, so this gates as an increase tripwire, never on sight.
    Contract,
    /// The apparatus's own hygiene: absence would silently rot another channel.
    /// Red on sight.
    Mechanics,
    /// Requested introspection — an **answered question** (ADR-0053 §1): exists
    /// because a call site asked (`PHPStan\dumpType()`, `var_dump()`); a fact
    /// rendering, not a claim about the program. Excluded from every fp-gate
    /// counter (§8); emitted from D3/D4, registered-but-unemitted before that.
    Debug,
}

impl Layer {
    /// The lowercase wire spelling (`"proof"|"contract"|"mechanics"|"debug"`) used by
    /// the `--format json` `layer` field (ADR-0050 §2 / ADR-0053 §4).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Layer::Proof => "proof",
            Layer::Contract => "contract",
            Layer::Mechanics => "mechanics",
            Layer::Debug => "debug",
        }
    }
}

/// The `origin` facet value (ADR-0050 §4): whether a `throw.undeclared` finding's
/// escaping-throw origin is the declaration's **own body** ([`Origin::Direct`]) or
/// arrived via a call edge ([`Origin::Propagated`]) — the split `throws-direct`
/// selects on, per `docs/notes/20260724-g1-throw-origin-measurement.md` (158 direct
/// vs 43,805 propagated on the legacy monorepo).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Origin {
    /// The origin lies in the annotated declaration's own body.
    Direct,
    /// The origin lies elsewhere, reached through a call hop.
    Propagated,
}

impl Origin {
    /// The wire spelling (`"direct"|"propagated"`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Origin::Direct => "direct",
            Origin::Propagated => "propagated",
        }
    }
}

/// The name of the only facet v1 defines (ADR-0050 §4/§11), carried solely by
/// `throw.undeclared`. Kept as a named constant so the emitter, the JSON key, and
/// [`declared_facet`] agree on one spelling.
pub const FACET_ORIGIN: &str = "origin";

/// A registry-declared **facet** value a finding carries (ADR-0050 §4): an
/// extra classification axis profile entries may select on. v1 declares exactly
/// one — `origin`, carried only by `throw.undeclared`. A small enum, not an open
/// string, so a second facet forces an ADR-driven variant, not an ad-hoc key.
/// `--format json` shows it additively as `"<key>": "<value>"` where declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Facet {
    /// The `origin` facet value (`direct|propagated`).
    Origin(Origin),
}

impl Facet {
    /// The wire key (`"origin"`) the additive JSON facet field uses.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Facet::Origin(_) => FACET_ORIGIN,
        }
    }

    /// The wire value (`"direct"|"propagated"`).
    #[must_use]
    pub const fn value(self) -> &'static str {
        match self {
            Facet::Origin(o) => o.as_str(),
        }
    }
}

/// The facet an emitted `id` **declares**, if any (ADR-0050 §4): only
/// `throw.undeclared` declares `origin` in v1. Returns the facet *name*, not a
/// value — what lets the emitter attach one and profiles select on it.
#[must_use]
pub fn declared_facet(id: &str) -> Option<&'static str> {
    if id == THROW_UNDECLARED_ID { Some(FACET_ORIGIN) } else { None }
}

/// The **lowest profile rung** on which a registered id may reach the surface
/// (ADR-0062 A-G10). Cumulative — `default ⊂ contracts ⊂ strict` — a profile
/// admits an id exactly when its floor is at or below the profile's rung.
/// Smallest-first order (`Default < Contracts < Strict < Pedantic`) drives
/// admission via the `Ord` derive.
///
/// Finer than a layer *set*: lets one layer straddle two rungs (contract holds
/// both floor-`Contracts` and floor-`Strict` ids — the offset family's strict leg).
///
/// [`Floor::Pedantic`] tops the order and **no built-in carries it as a rung** —
/// an id there is off every built-in until named in an `enable` list. (The same
/// shape as `throws-direct`, read the other way: a rung *below* the id it reaches.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Floor {
    /// On the bare `steins check` surface (and every surface above it).
    Default,
    /// Reached first by the `contracts` opt-up stage.
    Contracts,
    /// Reached only by the `strict` opt-up stage (ADR-0062 A-G10).
    Strict,
    /// Reached by **no built-in rung** — only an explicit `enable`. Home for
    /// house-style asks (how code is *written*, not a finding Steins makes): those
    /// can't ride `Strict`, which asks a different question (a weaker some-paths
    /// claim worth seeing?), and bundling them would force it on every `strict` user.
    Pedantic,
}

impl Floor {
    /// The wire/config spelling (`"default"|"contracts"|"strict"|"pedantic"`) — also
    /// the rung name a baseline entry records as its capture surface (A-G10).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Floor::Default => "default",
            Floor::Contracts => "contracts",
            Floor::Strict => "strict",
            Floor::Pedantic => "pedantic",
        }
    }

    /// Parse a rung spelling back (the baseline round-trip). An inherent method,
    /// not `FromStr` — no error type is worth naming; unknown spellings yield
    /// `None` and the caller decides how to handle it.
    #[must_use]
    pub fn parse(s: &str) -> Option<Floor> {
        match s {
            "default" => Some(Floor::Default),
            "contracts" => Some(Floor::Contracts),
            "strict" => Some(Floor::Strict),
            "pedantic" => Some(Floor::Pedantic),
            _ => None,
        }
    }
}

/// The diagnostic-id registry (ADR-0022/0050/0062): the closed set of ids Steins
/// emits, each paired with its [`Layer`] (ADR-0050 §2) and [`Floor`] (ADR-0062
/// A-G10). Single source of truth — `DIAGNOSTIC_IDS`, `layer()`, and
/// `surface_floor()` derive from it; an entry lacking either attribute does not
/// compile. A workspace totality test asserts every emittable id appears here.
///
/// The floor column reproduces pre-S6 behavior exactly (pinned by
/// `tests/registry.rs`): proof/mechanics/debug and pre-S6 contract ids carry
/// `Floor::Default`/`Floor::Contracts`; only the two S6 ids carry `Floor::Strict`.
///
/// `@steins-ignore` ids are validated against it (prefix-aware); the baseline
/// records ids verbatim.
pub const DIAGNOSTIC_REGISTRY: &[(&str, Layer, Floor)] = &[
    // proof — runtime survivability (zero-FP, red on sight).
    (ID, Layer::Proof, Floor::Default),
    (RETURN_ID, Layer::Proof, Floor::Default),
    (CALL_ON_NULL_ID, Layer::Proof, Floor::Default),
    (PROP_MISMATCH_ID, Layer::Proof, Floor::Default),
    (READONLY_REASSIGNED_ID, Layer::Proof, Floor::Default),
    // proof — finding-breadth family (ADR-0049).
    (CALL_UNDEFINED_FUNCTION_ID, Layer::Proof, Floor::Default),
    (CALL_UNDEFINED_METHOD_ID, Layer::Proof, Floor::Default),
    (CLASS_UNDEFINED_ID, Layer::Proof, Floor::Default),
    (CALL_TOO_FEW_ARGUMENTS_ID, Layer::Proof, Floor::Default),
    (CALL_TOO_MANY_ARGUMENTS_ID, Layer::Proof, Floor::Default),
    (CALL_UNKNOWN_NAMED_ARGUMENT_ID, Layer::Proof, Floor::Default),
    (OFFSET_MISSING_ID, Layer::Proof, Floor::Default),
    (OFFSET_ON_UNSUPPORTED_ID, Layer::Proof, Floor::Default),
    // printf arity (ADR-0078, issue #188)
    (CALL_PRINTF_TOO_FEW_ARGUMENTS_ID, Layer::Proof, Floor::Default),
    // declaration-incompatibility fatals (ADR-0078, issue #183): load-time fatals
    // read off the declaration graph alone.
    (CLASS_ABSTRACT_UNIMPLEMENTED_ID, Layer::Proof, Floor::Default),
    (CLASS_EXTENDS_FINAL_ID, Layer::Proof, Floor::Default),
    // overriding family (ADR-0078, issue #184): same declaration-graph fatal as
    // `class.extends-final`. Differs from `throw.liskov-widened`/
    // `effect.liskov-widened` (contract layer) because every premise here is a
    // NATIVE, PHP-enforced declaration, not an unenforced docblock/envelope.
    (OVERRIDE_FINAL_ID, Layer::Proof, Floor::Default),
    (OVERRIDE_STATIC_MISMATCH_ID, Layer::Proof, Floor::Default),
    (OVERRIDE_VISIBILITY_WEAKENED_ID, Layer::Proof, Floor::Default),
    (OVERRIDE_PARAMETER_VARIANCE_ID, Layer::Proof, Floor::Default),
    (OVERRIDE_RETURN_VARIANCE_ID, Layer::Proof, Floor::Default),
    // "(demotes)" below = degrades under a declared `warning-handler = "null"`
    // posture like `offset.missing` (ADR-0049 §7); unmarked = fatal, never demotes.
    // preg pattern refusal (ADR-0078, #189): PCRE refuses a proven pattern (demotes).
    (PREG_INVALID_PATTERN_ID, Layer::Proof, Floor::Default),
    // non-object receivers (ADR-0078, #190): `call.on-non-object` siblings the
    // fatal `call.on-null` rather than widening it, so the null case's baseline
    // entries keep their meaning (ADR-0022). `property.on-non-object` (demotes).
    (CALL_ON_NON_OBJECT_ID, Layer::Proof, Floor::Default),
    (PROPERTY_ON_NON_OBJECT_ID, Layer::Proof, Floor::Default),
    // foreach subject (ADR-0078, #192): non-array/null subject skips the loop
    // body (demotes).
    (FOREACH_NON_ITERABLE_ID, Layer::Proof, Floor::Default),
    // string context (ADR-0078, #193): two ids because the ADR-0049 §7 gate cuts
    // between them (ADR-0078 §1.4) — object case is fatal (never demotes); array
    // case warns with literal "Array" (demotes).
    (STRING_NON_STRINGABLE_ID, Layer::Proof, Floor::Default),
    (STRING_ARRAY_CONVERSION_ID, Layer::Proof, Floor::Default),
    // inaccessible members (ADR-0078, #185): visibility violation is fatal before
    // the member is reached — none demotes.
    (CALL_INACCESSIBLE_METHOD_ID, Layer::Proof, Floor::Default),
    (PROPERTY_INACCESSIBLE_ID, Layer::Proof, Floor::Default),
    (CLASS_CONST_INACCESSIBLE_ID, Layer::Proof, Floor::Default),
    // member absence (ADR-0078, #197): undeclared property read (demotes); undefined
    // class constant is fatal (never demotes) — the ADR-0078 §1.4 gate boundary is
    // why these are two ids, not one.
    (PROPERTY_UNDEFINED_ID, Layer::Proof, Floor::Default),
    (CLASS_CONST_UNDEFINED_ID, Layer::Proof, Floor::Default),
    // `maybe-` sibling, registered ahead of emission (ADR-0078 §1.3): a
    // possibly-claim belongs on the strict surface.
    (PROPERTY_MAYBE_UNDEFINED_ID, Layer::Proof, Floor::Strict),
    // return missing (ADR-0078, #199): falling off a non-void-typed function-like
    // is a fatal `TypeError` — never demotes.
    (TYPE_RETURN_MISSING_ID, Layer::Proof, Floor::Default),
    // Same fatal, only on paths the body's own returns don't cover (`maybe-`
    // sibling, ADR-0078 §1.3). Floor set by corpus measurement (phpstan-src's own
    // `src/` carries two and passes its own missing-return rule).
    (TYPE_RETURN_MAYBE_MISSING_ID, Layer::Proof, Floor::Strict),
    // invalid operands (ADR-0078, #191): operand kinds PHP's table refuses with a
    // `TypeError`. Fatal rows only — never demotes.
    (INVALID_OPERAND_ID, Layer::Proof, Floor::Default),
    // global constants (ADR-0078, #198): undefined constant is fatal since PHP
    // 8.0 — never demotes.
    (CONSTANT_UNDEFINED_ID, Layer::Proof, Floor::Default),
    // undefined variables (ADR-0078, #194): unbound-name read (demotes).
    (VARIABLE_UNDEFINED_ID, Layer::Proof, Floor::Default),
    // `strict`-floor some-paths-only sibling, registered ahead of its emitter
    // (#199): a weaker, deliberately-defensive claim, so opt-in.
    (VARIABLE_MAYBE_UNDEFINED_ID, Layer::Proof, Floor::Strict),
    // The argument side's possibly grade (ADR-0081's 2026-08-16 amendment, issue
    // #391): some arm of the argument's abstract fact is rejected by the native
    // parameter and some is accepted. `Layer::Proof` + `Floor::Strict` is the
    // §8 derivation, so the fp-gate routes it to the tripwire bucket with no list
    // to edit. Its ALL-arms-rejected sibling was measured empty and is not built.
    (TYPE_MAYBE_ARGUMENT_MISMATCH_ID, Layer::Proof, Floor::Strict),
    // The same judgment at the return seam (ADR-0081's 2026-08-27 amendment,
    // issue #537): some arm of the returned variable's declared type is rejected
    // by the enclosing function's native return type and some is accepted. Same
    // layer and floor as its argument sibling above, so the fp-gate's tripwire
    // bucket takes it by derivation with no list to edit; the all-arms-rejected
    // verdict is not built here either.
    (TYPE_MAYBE_RETURN_MISMATCH_ID, Layer::Proof, Floor::Strict),
    // contract — declared-contract acceptance (increase tripwires).
    (PARAM_MISMATCH_ID, Layer::Contract, Floor::Contracts),
    // Sentinel parameter (ADR-0088 §4, issue #428): the `never`-declared carve-out
    // out of `phpdoc.param-mismatch` above. `Layer::Contract` because the sentinel
    // is spelled in a docblock, so the premise is `Asserted` by construction —
    // regardless of whether the surviving type that still reaches it is itself
    // Verified. `Floor::Contracts`, the declared-contract family's own floor, not
    // the possibly-grade `Strict`: the question this asks is definite ("the
    // most-refined declared domain is still non-empty here", or silence), never a
    // some-arms-rejected uncertainty.
    (NEVER_PARAM_REACHABLE_ID, Layer::Contract, Floor::Contracts),
    // The `unset` pseudo-type's read (ADR-0087 §4, issue #396). `Layer::Contract`
    // because the premise is the author's own `@var T|unset`, not a reachability
    // fact — the reason it is not `variable.maybe-undefined`, which is proof-layer.
    // `Contracts`, the family floor, not the possibly grade's `Strict`: the read
    // contradicts an explicit declaration exactly as `phpdoc.param-mismatch` does,
    // and the uncertainty the `Strict` rung answers for is uncertainty about the
    // *premise*, which a declaration does not have.
    (PHPDOC_MAYBE_UNDEFINED_ID, Layer::Contract, Floor::Contracts),
    // The same judgment on an `Asserted` premise (ADR-0052 §5 forbids it reaching
    // a `type.*` id). `Strict`, not the family's `Contracts`, on the
    // `offset.maybe-missing` precedent: layer answers whose claim, floor answers
    // how sure, and `phpdoc.param-mismatch` above keeps `Contracts` for the
    // definite question.
    (PHPDOC_MAYBE_ARGUMENT_MISMATCH_ID, Layer::Contract, Floor::Strict),
    (RETURN_MISMATCH_ID, Layer::Contract, Floor::Contracts),
    // The return seam's `Asserted`-premise half (issue #537), on the same split
    // its argument sibling takes: `Contract` because a docblock arm is what the
    // claim rests on, `Strict` because the question is a possibly-grade one and
    // `phpdoc.return-mismatch` above keeps `Contracts` for the definite version.
    (PHPDOC_MAYBE_RETURN_MISMATCH_ID, Layer::Contract, Floor::Strict),
    // The hyphen reservation's diagnostic (ADR-0091 §6, issue #479).
    // `Layer::Contract`: the premise is a docblock's own spelling, and the
    // program runs either way.
    //
    // `Floor::Contracts` — the family floor, **ruled from measurement** at this
    // slice's review (2026-08-27; §6 refuses to fix this floor in advance).
    //
    // §6 named one thing to calibrate: `KNOWN_UNENFORCED` exists because other
    // tools have spellings Steins recognizes without enforcing, and the ones it
    // has never heard of are exactly what this id would convict. Measured: the
    // pinned public corpus reports ZERO over 6,670 files carrying 2,903
    // hyphenated type-position sites in 13 distinct spellings, every one
    // recognized; the private corpus reports exactly ONE, a misspelling of
    // `non-empty-string` in a test docblock — a true positive of the class the
    // id exists for. The anticipated FP source is measured absent everywhere it
    // is measurable, so a proven-yield check does not sit behind a rung nobody
    // reaches. The residual risk is unmeasured conjecture with three absorbers
    // when it materializes: the spelling joins `KNOWN_UNENFORCED`, §4.1's plugin
    // registration once it lands, and ADR-0022's baseline.
    //
    // NOT `Strict`: that rung asks a different question (is a weaker some-paths
    // claim worth seeing?), and this judgment is definite.
    //
    // **The baseline moves with configuration, not only with code** (ADR-0091
    // §4.1, §9). The allowlist is builtin tables ∪ plugin registrations, so the
    // id is computed after plugin load and is plugin-set dependent: dropping a
    // plugin introduces findings on every docblock that used its vocabulary.
    // That is the correct answer — the vocabulary really did go away — but
    // ADR-0022's baseline discipline is told here rather than left to discover
    // it, because no code changed. The registration kind does not exist on the
    // `steins-plugin.json` manifest yet, so the plugin half is empty for every
    // project today; the coupling bites when it lands.
    (PHPDOC_UNKNOWN_VOCABULARY_ID, Layer::Contract, Floor::Contracts),
    (PHPDOC_PROP_MISMATCH_ID, Layer::Contract, Floor::Contracts),
    (THROW_UNDECLARED_ID, Layer::Contract, Floor::Contracts),
    (THROW_LISKOV_ID, Layer::Contract, Floor::Contracts),
    (EFFECT_ID, Layer::Contract, Floor::Contracts),
    (EFFECT_LISKOV_ID, Layer::Contract, Floor::Contracts),
    // interop-label hygiene (ADR-0082 amendment, issue #311): NOT the mechanics
    // layer its twin `effect.unknown-label` carries — mechanics is unsuppressable
    // and always-on, the fail-closed posture the owner ruling refused for
    // docblocks. Floor rides with the envelope family, so a bare `check` stays
    // silent and a mid-migration project can baseline the pile.
    (INTEROP_UNKNOWN_LABEL_ID, Layer::Contract, Floor::Contracts),
    // contract — finding-breadth declared-receiver lane (ADR-0049 §8).
    (PHPDOC_UNDEFINED_METHOD_ID, Layer::Contract, Floor::Contracts),
    // offset family's STRICT leg (ADR-0062 A-G10, #51): `offset.undeclared` sits
    // at `Contracts` (a corpus sweep measured zero findings); `offset.maybe-missing`
    // stays `Strict` until the `isset`→`@phpstan-assert` discharge gap closes.
    (OFFSET_UNDECLARED_ID, Layer::Contract, Floor::Contracts),
    (OFFSET_MAYBE_MISSING_ID, Layer::Contract, Floor::Strict),
    // untyped surface (ADR-0078, #200): declared debt, not a proof (ADR-0078 §2's
    // lint boundary). FIVE land at `Contracts`; the ADR marks
    // `untyped.iterable-value`/`untyped.generics` `Contracts→Strict by
    // measurement` (noisiest, most-content arms) — one-line moves once measured.
    //
    // `untyped.class-constant` LEFT the family floor (2026-08-09 owner ruling): a
    // constant's initializer is a constant expression, so the type is pinned
    // whether written or not — unlike every other arm, whose silence yields real
    // withheld `mixed`. Not `Strict` either (that rung asks an unrelated
    // some-paths question). Goes to `Pedantic` (no built-in rung reaches it; the
    // `pedantic` profile names it), measured on the php-typing-conformance suite
    // firing on `key-of<C::MAP>`/`value-of<C::MAP>`/`int-mask-of<…>` fixtures
    // typed exhaustively BY their values.
    (UNTYPED_PARAMETER_ID, Layer::Contract, Floor::Contracts),
    (UNTYPED_RETURN_ID, Layer::Contract, Floor::Contracts),
    (UNTYPED_PROPERTY_ID, Layer::Contract, Floor::Contracts),
    (UNTYPED_CLASS_CONSTANT_ID, Layer::Contract, Floor::Pedantic),
    (UNTYPED_ITERABLE_VALUE_ID, Layer::Contract, Floor::Contracts),
    (UNTYPED_GENERICS_ID, Layer::Contract, Floor::Contracts),
    // mechanics — apparatus hygiene (red on sight, suppression-exempt).
    (SUPPRESS_UNMATCHED_ID, Layer::Mechanics, Floor::Default),
    (SUPPRESS_UNKNOWN_ID, Layer::Mechanics, Floor::Default),
    (UNKNOWN_LABEL_ID, Layer::Mechanics, Floor::Default),
    // member-kind port wave's first id (ADR-0078, issue #187): works-but-drops-a-
    // value drift, not a runtime break.
    (ARRAY_DUPLICATE_KEY_ID, Layer::Mechanics, Floor::Default),
    // docblock hygiene (ADR-0078, issue #186): annotations drifted from code.
    // `phpdoc.*` spans two layers (ADR-0078 §1.5) — the layer, never the prefix,
    // decides.
    (PHPDOC_UNPARSABLE_ID, Layer::Mechanics, Floor::Default),
    (PHPDOC_STALE_PARAM_ID, Layer::Mechanics, Floor::Default),
    (PHPDOC_STALE_VAR_ID, Layer::Mechanics, Floor::Default),
    (PHPDOC_MISPLACED_VAR_ID, Layer::Mechanics, Floor::Default),
    (PHPDOC_THROWS_NOT_THROWABLE_ID, Layer::Mechanics, Floor::Default),
    (CLOSURE_UNUSED_USE_ID, Layer::Mechanics, Floor::Default),
    // parse failure (ADR-0079, issue #180): a `php -l`-rejected file is apparatus
    // rot — undemotable, suppression-exempt; the remedy is fixing the file.
    (SYNTAX_UNPARSABLE_ID, Layer::Mechanics, Floor::Default),
    // debug — the dump surface (ADR-0053): requested introspection, not a finding.
    // Suppression-, baseline-, and fp-gate-exempt (§4/§8), decided as a layer
    // property before the ladder is consulted.
    (DEBUG_TYPE_ID, Layer::Debug, Floor::Default),
    (DEBUG_PHPDOC_TYPE_ID, Layer::Debug, Floor::Default),
    (DEBUG_VAR_DUMP_ID, Layer::Debug, Floor::Default),
    // The trace annotation (ADR-0074 §4): docblock spelling of the same question.
    (DEBUG_TRACE_ID, Layer::Debug, Floor::Default),
];

/// The flat id list, **derived** from [`DIAGNOSTIC_REGISTRY`] so there is exactly
/// one source of truth. Kept as a `&[&str]` for the prefix-matching consumers and
/// the baseline.
pub const DIAGNOSTIC_IDS: &[&str] = &derive_ids();

/// Project the registry down to its ids at compile time (keeps `DIAGNOSTIC_IDS` a
/// pure derivation of [`DIAGNOSTIC_REGISTRY`], never a parallel hand-list).
const fn derive_ids() -> [&'static str; DIAGNOSTIC_REGISTRY.len()] {
    let mut arr = [""; DIAGNOSTIC_REGISTRY.len()];
    let mut i = 0;
    while i < DIAGNOSTIC_REGISTRY.len() {
        arr[i] = DIAGNOSTIC_REGISTRY[i].0;
        i += 1;
    }
    arr
}

/// The [`Layer`] a diagnostic `id` carries, or `None` if unregistered (ADR-0050
/// §2). Exact-id lookup — prefix subsumption is [`pattern_is_known`]'s concern.
#[must_use]
pub fn layer(id: &str) -> Option<Layer> {
    DIAGNOSTIC_REGISTRY.iter().find(|(i, ..)| *i == id).map(|(_, l, _)| *l)
}

/// The [`Floor`] a diagnostic `id` carries (ADR-0062 A-G10), or `None` if
/// unregistered. Exact-id lookup, the sibling of [`layer`].
#[must_use]
pub fn surface_floor(id: &str) -> Option<Floor> {
    DIAGNOSTIC_REGISTRY.iter().find(|(i, ..)| *i == id).map(|(.., f)| *f)
}

/// The result of applying inline ignores to a batch of object-level findings.
pub struct InlineOutcome {
    /// Findings **not** suppressed (fed onward to baseline, then printed).
    pub kept: Vec<Diagnostic>,
    /// How many findings inline ignores suppressed.
    pub suppressed: usize,
    /// The meta-diagnostics produced (`suppress.unmatched` / `suppress.unknown-id`),
    /// never themselves suppressed or baselined.
    pub meta: Vec<Diagnostic>,
}

/// A parsed `@steins-ignore` directive from one comment.
struct Directive {
    /// The raw id tokens (comma-separated; may include unknown/malformed).
    patterns: Vec<String>,
    /// The line this directive suppresses on.
    target_line: u32,
    /// The comment's own 1-based line/column (meta-diagnostics report here).
    line: u32,
    column: u32,
}

/// Whether an ignore `pattern` (`type`, `type.*`, or `type.argument-mismatch`)
/// **matches** a concrete diagnostic `id` under ADR-0022 prefix subsumption: a
/// bare/`.*` family matches every id beneath it; an exact id matches itself.
/// Segment-aware, so `type` does not match `typex.*`. Shared by the inline-ignore
/// channel and the profile engine's `enable`/`disable`/`warn` arrays (ADR-0050 §5).
#[must_use]
pub fn pattern_matches(pattern: &str, id: &str) -> bool {
    let norm = pattern.strip_suffix(".*").unwrap_or(pattern);
    id == norm || id.strip_prefix(norm).is_some_and(|rest| rest.starts_with('.'))
}

/// Whether an ignore `pattern` is **registry-governed** (ADR-0022): after
/// stripping a trailing `.*`, it equals a registry id or is a family prefix of at
/// least one. Unknown/malformed patterns earn `suppress.unknown-id`.
#[must_use]
pub fn pattern_is_known(pattern: &str) -> bool {
    let norm = pattern.strip_suffix(".*").unwrap_or(pattern);
    if norm.is_empty() {
        return false;
    }
    DIAGNOSTIC_IDS
        .iter()
        .any(|&r| r == norm || r.strip_prefix(norm).is_some_and(|rest| rest.starts_with('.')))
}

/// Extract the text following `@steins-ignore`, trimmed of `*/` and whitespace.
/// `None` if the marker is absent.
fn extract_directive(text: &str) -> Option<&str> {
    let idx = text.find(INLINE_IGNORE)?;
    let mut rest = &text[idx + INLINE_IGNORE.len()..];
    if let Some(end) = rest.find("*/") {
        rest = &rest[..end];
    }
    Some(rest.trim())
}

/// Parse the id list from a directive body: comma-separated, trimmed, non-empty
/// tokens before an optional parenthesized reason.
fn parse_id_list(rest: &str) -> Vec<String> {
    let id_part = rest.find('(').map_or(rest, |p| &rest[..p]);
    id_part
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Collect every `@steins-ignore` directive in a file, resolving placement.
fn directives(tree: &SourceTree) -> Vec<Directive> {
    let mut out = Vec::new();
    for c in tree.comments() {
        let Some(body) = extract_directive(&c.text) else { continue };
        let patterns = parse_id_list(body);
        let pos = tree.position(c.span.start);
        // Trailing comment → this line; own-line comment → next line.
        let target_line =
            if tree.is_line_leading(c.span.start) { pos.line + 1 } else { pos.line };
        out.push(Directive { patterns, target_line, line: pos.line, column: pos.column });
    }
    out
}

/// The inline-suppression marker, verbatim. Public so a caller can decide
/// *whether* a file needs scanning without decoding its tree (issue #516): a
/// file whose text does not contain this cannot carry a directive, and so can
/// only take part in the scan as the subject of a finding.
pub const INLINE_IGNORE: &str = "@steins-ignore";

/// Apply inline `@steins-ignore` suppression to object-level `findings`. `files`
/// pairs every analyzed file's diagnostic path with its parsed tree, so each
/// finding's comments can be consulted. Findings whose path is not among `files`
/// are kept untouched.
pub fn apply_inline_ignores(
    findings: Vec<Diagnostic>,
    files: &[(String, &SourceTree)],
) -> InlineOutcome {
    let mut kept = Vec::new();
    let mut suppressed = 0usize;
    let mut meta = Vec::new();
    let known_paths: HashSet<&str> = files.iter().map(|(p, _)| p.as_str()).collect();

    for (path, tree) in files {
        let dirs = directives(tree);
        // Per-directive, per-pattern "used" flags, to drive `suppress.unmatched`.
        let mut used: Vec<Vec<bool>> = dirs.iter().map(|d| vec![false; d.patterns.len()]).collect();

        for f in findings.iter().filter(|f| &f.path == path) {
            // ADR-0053 §4: the debug lane is exempt from inline ignores — a dump is an
            // answered question, not a finding (remedy: delete the call). It's never
            // suppressed and never marks a pattern used, so `@steins-ignore debug.type`
            // stays unmatched and earns `suppress.unmatched` (anti-rot doing its job).
            if matches!(layer(f.id), Some(Layer::Debug)) {
                kept.push(f.clone());
                continue;
            }
            let mut is_suppressed = false;
            for (di, d) in dirs.iter().enumerate() {
                if d.target_line != f.line {
                    continue;
                }
                for (pi, pat) in d.patterns.iter().enumerate() {
                    if pattern_is_known(pat) && pattern_matches(pat, f.id) {
                        used[di][pi] = true;
                        is_suppressed = true;
                    }
                }
            }
            if is_suppressed {
                suppressed += 1;
            } else {
                kept.push(f.clone());
            }
        }

        // Meta-diagnostics: unknown ids, then unmatched (still-unused) valid ids.
        for (di, d) in dirs.iter().enumerate() {
            if d.patterns.is_empty() {
                meta.push(meta_diag(
                    SUPPRESS_UNKNOWN_ID,
                    path,
                    d,
                    "malformed @steins-ignore (no diagnostic id given)".to_owned(),
                ));
                continue;
            }
            for (pi, pat) in d.patterns.iter().enumerate() {
                if !pattern_is_known(pat) {
                    meta.push(meta_diag(
                        SUPPRESS_UNKNOWN_ID,
                        path,
                        d,
                        format!("@steins-ignore names unknown diagnostic id '{pat}'"),
                    ));
                } else if !used[di][pi] {
                    meta.push(meta_diag(
                        SUPPRESS_UNMATCHED_ID,
                        path,
                        d,
                        format!(
                            "@steins-ignore of {pat} matches no diagnostic on line {}",
                            d.target_line
                        ),
                    ));
                }
            }
        }
    }

    // Findings for files not in the batch (should not arise) pass through.
    for f in findings {
        if !known_paths.contains(f.path.as_str()) {
            kept.push(f);
        }
    }

    InlineOutcome { kept, suppressed, meta }
}

/// Build a meta-diagnostic at a directive's comment location.
fn meta_diag(id: &'static str, path: &str, d: &Directive, message: String) -> Diagnostic {
    // Mechanics meta-diagnostics declare no facet (ADR-0050 §4: only
    // `throw.undeclared` does).
    Diagnostic { id, path: path.to_owned(), line: d.line, column: d.column, message, facet: None, fix: None }
}

#[cfg(test)]
mod tests {
    use super::{extract_directive, parse_id_list, pattern_is_known, pattern_matches};

    #[test]
    fn prefix_and_bare_family_match() {
        assert!(pattern_matches("type.argument-mismatch", "type.argument-mismatch"));
        assert!(pattern_matches("type.*", "type.argument-mismatch"));
        assert!(pattern_matches("type", "type.argument-mismatch"));
        // The `type.return-mismatch` id joins the same `type.*` family.
        assert!(pattern_matches("type.return-mismatch", "type.return-mismatch"));
        assert!(pattern_matches("type.*", "type.return-mismatch"));
        assert!(pattern_matches("type", "type.return-mismatch"));
        assert!(pattern_matches("effect", "effect.envelope-exceeded"));
        // Segment-aware: `type` must not match a differently-rooted family.
        assert!(!pattern_matches("type", "typex.foo"));
        assert!(!pattern_matches("effect", "type.argument-mismatch"));
    }

    #[test]
    fn known_vs_unknown_ids() {
        assert!(pattern_is_known("type.argument-mismatch"));
        assert!(pattern_is_known("type.return-mismatch"));
        assert!(pattern_is_known("type.*"));
        assert!(pattern_is_known("type"));
        assert!(pattern_is_known("effect.envelope-exceeded"));
        assert!(pattern_is_known("suppress.unmatched"));
        // Debug ids are registry-governed, so naming one is *known* (never
        // suppress.unknown-id); it matches no dump finding (exempt, §4), so it
        // reports suppress.unmatched instead — anti-rot doing its normal job.
        assert!(pattern_is_known("debug.type"));
        assert!(pattern_is_known("debug.phpdoc-type"));
        assert!(pattern_is_known("debug.var-dump"));
        assert!(pattern_is_known("debug.*"));
        assert!(pattern_is_known("debug"));
        // Typos and unknown families.
        assert!(!pattern_is_known("type.bogus"));
        assert!(!pattern_is_known("nope"));
        assert!(!pattern_is_known(""));
    }

    #[test]
    fn directive_extraction_handles_all_comment_shapes() {
        assert_eq!(extract_directive("// @steins-ignore type.x"), Some("type.x"));
        assert_eq!(extract_directive("# @steins-ignore type.x (why)"), Some("type.x (why)"));
        assert_eq!(extract_directive("/* @steins-ignore type.x */"), Some("type.x"));
        assert_eq!(extract_directive("// unrelated comment"), None);
    }

    #[test]
    fn id_list_splits_comma_and_strips_reason() {
        assert_eq!(parse_id_list("type.x, effect.y (reason here)"), vec!["type.x", "effect.y"]);
        assert_eq!(parse_id_list("type.x"), vec!["type.x"]);
        assert!(parse_id_list("(only a reason)").is_empty());
    }
}
