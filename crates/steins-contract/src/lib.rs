//! Contract acceptance (ADR-0030 relation #1): phpdoc types × the value
//! domain, judged in the unified `Certainty`.
//!
//! Bridges `steins-phpdoc`'s syntactic type AST to `steins-domain`'s facts.
//! Lowering normalizes keywords into a small semantic [`ContractTy`] (e.g.
//! `scalar` becomes the union of the four bases, `positive-int` an interval,
//! `numeric-string` a predicate set), so acceptance is Kleene composition
//! over a handful of leaf rules instead of a keyword zoo.
//!
//! `Maybe` is the answer wherever membership is undecided: every construct
//! lowered to [`ContractTy::Opaque`] (conditionals, templates, const
//! fetches, `$this`, …), and every non-extensional string type
//! (`literal-string`, `callable-string` — provenance, ADR-0038).

mod admit;
pub mod normalize;
pub mod spell;

pub use admit::{ShapeSpec, admits_fact, admits_val, shape_verdict};

use steins_domain::{
    Base, Certainty, Fact, IntRange, KeyClass, Key as DKey, PhpStr, Presence as DPresence,
    Refinement, ShapeFact, StrPreds, Tail as DTail, Val,
};
use steins_phpdoc::ast::{ArrayShapeKind, ConstExpr, ShapeKey, StringLit, Type, TypeKind};

/// A lowered `callable(P1, P2=): R` signature (issue #11): parameter
/// contracts (optionality/variadic/by-ref markers as the grammar provides)
/// plus the return contract. Every arm is a ground type — see
/// `lower_callable` for why a template-bearing signature never reaches one.
#[derive(Debug, Clone, PartialEq)]
pub struct CallableSig {
    /// The declared parameters, in source order.
    pub params: Vec<CallableParamTy>,
    /// The declared return contract.
    pub ret: ContractTy,
}

/// Obligations a **refined** callable spelling puts on the bound callable
/// (ADR-0063 §2 decision 4); composed per spelling by `callable_obl` below.
/// All `false` for plain `callable`/`Closure` ([`Default`]).
///
/// Only [`Self::closure_only`] is a value-domain obligation ([`admits_val`]/
/// [`admits_fact`] decide it). [`Self::pure`]/[`Self::is_static`] are
/// properties of the callable's *definition*, judged where it is in scope
/// (`steins-infer`'s closure-argument check), like [`CallableSig`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CallableObl {
    /// `pure-callable` / `pure-closure` / `static-pure-closure`: the bound
    /// callable's inferred effect envelope must be pure (ADR-0055), judged
    /// against the effect fixpoint, never a declaration flag (ADR-0063 §3).
    pub pure: bool,
    /// `static-closure` / `static-pure-closure`: the bound closure must carry
    /// `static` — a mechanical syntactic check.
    pub is_static: bool,
    /// The `*-closure` spellings: the value must be a `Closure` **instance**,
    /// failing without purity analysis (independently of `pure`).
    pub closure_only: bool,
}

impl CallableObl {
    /// Whether this spelling imposes nothing beyond a bare `callable` — the fast
    /// path every existing consumer keeps.
    #[must_use]
    pub fn is_bare(self) -> bool {
        !self.pure && !self.is_static && !self.closure_only
    }
}

/// The obligations named by a callable **identifier**, or `None` if it is not
/// a callable spelling. Shared by the bare-identifier and the
/// parenthesized-signature lowering, so `pure-callable(int): int` carries the
/// same obligation as bare `pure-callable`.
///
/// `callable-object` is deliberately *not* `closure_only`: it means "an
/// object that is callable" (any `__invoke`), wider than `Closure`. Bare
/// `Closure` is likewise obligation-free (ADR-0063 P3).
#[must_use]
fn callable_obl(norm: &str) -> Option<CallableObl> {
    let obl = match norm {
        "callable" | "callable-object" | "closure" => CallableObl::default(),
        "pure-callable" => CallableObl { pure: true, ..CallableObl::default() },
        "pure-closure" => CallableObl { pure: true, closure_only: true, is_static: false },
        "static-closure" => CallableObl { is_static: true, closure_only: true, pure: false },
        "static-pure-closure" => CallableObl { pure: true, is_static: true, closure_only: true },
        _ => return None,
    };
    Some(obl)
}

/// One parameter of a lowered [`CallableSig`].
#[derive(Debug, Clone, PartialEq)]
pub struct CallableParamTy {
    /// The parameter's contract type.
    pub ty: ContractTy,
    /// `$x =` — the parameter is optional (a caller may omit it).
    pub optional: bool,
    /// `...$x` — variadic.
    pub variadic: bool,
    /// `&$x` — by-reference (the variance check stays silent on these).
    pub by_ref: bool,
}

/// A field of a lowered shape.
#[derive(Debug, Clone, PartialEq)]
pub struct CField {
    /// The normalized key (int or string), assigned automatically for
    /// positional fields (`array{int, string}` keys `0`, `1`).
    pub key: CKey,
    /// Whether the field may be absent.
    pub optional: bool,
    /// The field's value contract.
    pub ty: ContractTy,
}

/// A normalized shape key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CKey {
    /// Integer key.
    Int(i64),
    /// String key.
    Str(PhpStr),
}

/// What a [`ContractTy::MixedMinus`] subtracts from `mixed`.
///
/// Both cuts are defined by a *value* predicate the domain already owns, not by
/// a type-algebra difference: this is the whole reason the pair needs one leaf
/// variant rather than a general subtraction operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MixedCut {
    /// `null` only — Phan's `non-null-mixed`.
    Null,
    /// Every falsy value — `false`, `0`, `0.0`, `''`, `'0'`, `null`, `[]` —
    /// PHPStan's `non-empty-mixed`. Subsumes [`MixedCut::Null`].
    Falsy,
}

/// The semantic contract type — the lowered, normalized form phpdoc types
/// are checked through.
#[derive(Debug, Clone, PartialEq)]
pub enum ContractTy {
    /// `mixed` — admits everything, including null.
    Mixed,
    /// `mixed` with a cut removed: `non-null-mixed` (Phan) and
    /// `non-empty-mixed` (PHPStan).
    ///
    /// The only **negative** leaf, needed because neither spelling is a union
    /// of the forms above. Exact against a concrete value
    /// ([`steins_domain::php_is_falsy`] is the definition); against an
    /// abstract fact, `Maybe` unless the fact's own refinement decides it.
    MixedMinus(MixedCut),
    /// `never` — admits nothing.
    Never,
    /// The null type.
    Null,
    /// A scalar base. NOTE: `float` accepts ints (PHPStan core semantics).
    Base(Base),
    /// `int<lo, hi>`, `positive-int`, ….
    IntIn(IntRange),
    /// `numeric-string`, `non-empty-string`, `non-falsy-string`,
    /// `lowercase-string`, `uppercase-string` and their intersections.
    StrWith(StrPreds),
    /// A string-based type whose membership is non-extensional or unmodeled
    /// (`literal-string`, `callable-string`, `numeric-int-string`): strings
    /// are `Maybe`, everything else `No` (ADR-0038).
    ///
    /// `class-string` and kin left this variant with issue #236 (a value
    /// property, now a [`StrPreds`] predicate). What stays is genuinely
    /// undecidable from the value: provenance (`literal-string`) or an
    /// unseen function table (`callable-string`).
    StrOpaque,
    /// Integer literal type.
    LitInt(i64),
    /// Float literal type (compared by PHP value equality — IEEE `==`, so
    /// int `5` satisfies `5.0`; deliberately unlike the domain's set
    /// equality).
    LitFloat(f64),
    /// String literal type.
    LitStr(PhpStr),
    /// `true` / `false`.
    LitBool(bool),
    /// `array` / `non-empty-array` without parameters.
    ArrayAny {
        /// Reject empty arrays.
        non_empty: bool,
    },
    /// `list<T>` / `non-empty-list<T>` — #14939: keys exactly `0..n-1`.
    ListOf {
        /// Element contract.
        elem: Box<ContractTy>,
        /// Reject the empty list.
        non_empty: bool,
    },
    /// `array<K, V>` / `T[]` / `non-empty-array<K, V>`, and Phan's
    /// `associative-array<K, V>` / `non-empty-associative-array<K, V>` — the same
    /// key/value contract plus a refusal of list realizations.
    MapOf {
        /// Key contract.
        key: Box<ContractTy>,
        /// Value contract.
        val: Box<ContractTy>,
        /// Reject the empty array.
        non_empty: bool,
        /// Phan's `associative-array` refinement: reject a realization whose
        /// keys happen to be a list. Seeds `is_list` at `No` instead of
        /// `Maybe` (`to_shape_fact`) — `list<T>`'s `Yes` seed, in reverse.
        not_list: bool,
    },
    /// `iterable<K, V>` — arrays behave as `MapOf`; scalar values are `No`.
    IterableOf {
        /// Key contract.
        key: Box<ContractTy>,
        /// Value contract.
        val: Box<ContractTy>,
    },
    /// `array{…}` / `list{…}` shapes, per #14939 (ADR-0030): `array{}` is an
    /// order-agnostic key *set*, `list{}` a positional key *sequence*.
    Shape {
        /// `list{…}` (positional) vs `array{…}` (keyed set).
        list: bool,
        /// The declared fields.
        fields: Vec<CField>,
        /// Sealed shapes reject extra keys.
        sealed: bool,
        /// Reject the empty array (`non-empty-array{…}` forms).
        non_empty: bool,
        /// The unsealed tail contract `(key, value)`, when `...<K, V>` was
        /// given a type.
        unsealed: Option<(Option<Box<ContractTy>>, Box<ContractTy>)>,
    },
    /// A class or interface name (normalized: lowercased, leading `\`
    /// stripped). Scalars/arrays/null are never instances.
    Class(String),
    /// **One enum case** — `Suit::Hearts` (issue #429). The enum FQN carries
    /// [`Self::Class`]'s normalization (lowercased, leading `\` stripped); the
    /// case name is stored as declared, because PHP compares case names
    /// case-sensitively.
    ///
    /// The arm the finite enum domain is made of. A PHP enum case is an object
    /// with exactly one inhabitant, and the value domain is object-free
    /// (ADR-0035/0038/0043), so the singleton has no `Val` and no `Fact` — it
    /// lives in the arm lane, which is where ADR-0052 §1 puts declared
    /// alternatives anyway. An enum-typed declaration therefore seeds one arm
    /// per declared case, and identity narrowing deletes arms
    /// ([`normalize::Subtrahend::EnumCase`]).
    ///
    /// Never produced by phpdoc lowering: `@param Suit::Hearts` stays
    /// [`Self::Opaque`] (a `ConstExpr::Fetch`). The case set is a **Verified**
    /// fact read off a resolved native declaration, and ADR-0037's trust order
    /// forbids laundering one in from a docblock.
    EnumCase {
        /// The declaring enum's FQN, normalized as [`Self::Class`]'s is.
        enum_fqn: String,
        /// The case name, as declared (compared case-sensitively).
        case: String,
    },
    /// The `object` keyword and object shapes.
    ObjectAny,
    /// `resource` — a legacy PHP resource handle, the one type PHP itself
    /// **cannot spell** in a declaration (ADR-0056 §8). `open-resource`/
    /// `closed-resource` lower here too: Steins models only the *kind*, not
    /// open/closed state.
    ///
    /// [`admits_val`] answers a true `No` for every [`steins_domain::Val`]
    /// (probed at 8.5.9). The object case stays `Maybe`: PHP 8 migrated most
    /// resources to objects while docblocks kept saying `@param resource`,
    /// so `steins-infer`'s `unrepresentable_verdict` treats a stale docblock
    /// as rot to route around, not to convict.
    Resource,
    /// `callable` and callable signatures: strings and arrays are `Maybe`
    /// (a string may name a function, a pair-array a method), other
    /// scalars `No`.
    ///
    /// `None` is a bare `callable`/`Closure`, accepting any callable.
    /// `Some(sig)` carries a declared `callable(P1, P2=): R` signature (issue
    /// #11), judged arm-wise (contravariant params, covariant return) by
    /// `steins-infer`'s closure-argument variance check. Value/fact
    /// acceptance ignores the signature — a runtime value can't be judged
    /// against a call shape.
    ///
    /// `obl` carries the refined spellings' obligations ([`CallableObl`],
    /// ADR-0063 P3); [`CallableObl::is_bare`] leaves bare-callable consumers
    /// unaffected.
    CallableTy { sig: Option<Box<CallableSig>>, obl: CallableObl },
    /// `unset` — the possibly-undefined pseudo-type (ADR-0087): `@var
    /// \DateTime|unset $x` says the variable holds a `\DateTime` **or has no
    /// binding at all**. Not a class, and not any value: it is neither `null`
    /// nor `void` nor `never` nor `mixed`.
    ///
    /// Contributes **no value** to the type lane. The word is carried this far
    /// (rather than dropped at lowering) only so the speller can render it
    /// back — [`ContractTy::Opaque`] spells as `mixed` and would lose it. Every
    /// value-domain reader therefore drops it: [`admits_val`]/[`admits_fact`]
    /// answer `Maybe` (the honest floor — this variant states nothing about a
    /// value), [`to_fact`]/[`to_shape_fact`] answer `None`, and consumers that
    /// build arm lists filter it out with [`ContractTy::is_unset`] so
    /// `\DateTime|unset`'s value arms are *structurally* those of `\DateTime`.
    ///
    /// What the state MEANS for reads of the variable is issue #396; positions
    /// other than an inline top-level `@var` are issue #397. This slice is
    /// vocabulary only and emits nothing.
    Unset,
    /// Union.
    Union(Vec<ContractTy>),
    /// Intersection.
    Inter(Vec<ContractTy>),
    /// Anything not modeled: conditionals, offset access, const fetches,
    /// `$this`/`self`/`static`, templates. Always `Maybe`.
    Opaque,
}

impl ContractTy {
    /// Is this the `unset` pseudo-type — a member that carries a *spelling* but
    /// no value (ADR-0087)?
    ///
    /// The one predicate arm-list builders use to keep the word out of the value
    /// lane: filter with this **before** the emptiness check, so `\DateTime|unset`
    /// yields exactly `\DateTime`'s arms and a bare `unset` yields an empty list
    /// (the no-envelope outcome, ADR-0029: nothing is seeded, nothing reports).
    #[must_use]
    pub fn is_unset(&self) -> bool {
        matches!(self, ContractTy::Unset)
    }
}

/// Lower a parsed phpdoc type into its semantic contract form. Total: every
/// AST lowers, with [`ContractTy::Opaque`] as the honest floor.
#[must_use]
pub fn lower(ty: &Type) -> ContractTy {
    match &ty.kind {
        TypeKind::Identifier(name) => lower_identifier(name),
        TypeKind::This => ContractTy::Opaque,
        TypeKind::Nullable(inner) => {
            ContractTy::Union(vec![ContractTy::Null, lower(inner)])
        }
        TypeKind::Union { types, .. } => {
            ContractTy::Union(types.iter().map(lower).collect())
        }
        TypeKind::Intersection(types) => fold_array_accessories(types)
            .unwrap_or_else(|| ContractTy::Inter(types.iter().map(lower).collect())),
        TypeKind::Array(elem) => ContractTy::MapOf {
            key: Box::new(array_key()),
            val: Box::new(lower(elem)),
            non_empty: false,
            not_list: false,
        },
        TypeKind::Generic { base, args } => lower_generic(base, args),
        TypeKind::Callable(c) => lower_callable(c),
        TypeKind::ArrayShape(shape) => lower_shape(shape),
        TypeKind::ObjectShape(_) => ContractTy::ObjectAny,
        TypeKind::OffsetAccess { .. } | TypeKind::Conditional(_) | TypeKind::Unsupported(_) => {
            ContractTy::Opaque
        }
        TypeKind::Const(c) => lower_const(c),
    }
}

/// Parse a phpdoc type string and lower it. `None` on a parse error or a
/// trailing-garbage partial parse — the no-envelope outcome (ADR-0029).
#[must_use]
pub fn lower_str(input: &str) -> Option<ContractTy> {
    let parsed = steins_phpdoc::parse_type(input).ok()?;
    if !parsed.at_end {
        return None;
    }
    Some(lower(&parsed.ty))
}

/// The `array-key` union (`int|string`). `pub(crate)` so [`normalize`]'s array
/// rules (ADR-0071 §2.1) ask "does this key contract cover *every* array key?"
/// against the same spelling the lowering produces, rather than a second copy.
pub(crate) fn array_key() -> ContractTy {
    ContractTy::Union(vec![ContractTy::Base(Base::Int), ContractTy::Base(Base::String)])
}

/// The `scalar` union — the four scalar bases, in the canonical member order.
/// Shared by the `scalar` keyword and by `non-empty-scalar`, which is this
/// union intersected with the falsy cut.
fn scalar() -> ContractTy {
    ContractTy::Union(vec![
        ContractTy::Base(Base::Int),
        ContractTy::Base(Base::Float),
        ContractTy::Base(Base::String),
        ContractTy::Base(Base::Bool),
    ])
}

/// Is `ty` exactly the `array-key` union [`array_key`] lowers to? A `MapOf`
/// key of this shape is the honest floor a single-arg `array<V>`/`T[]` lowers
/// to (`lower`/`lower_generic`) — the speller collapses it back to the terser
/// single-arg spelling rather than `array<int|string, V>` (round-trip
/// faithful either way; terser is nicer).
#[must_use]
pub(crate) fn is_array_key_ty(ty: &ContractTy) -> bool {
    matches!(
        ty,
        ContractTy::Union(members)
            if members.len() == 2
                && members.contains(&ContractTy::Base(Base::Int))
                && members.contains(&ContractTy::Base(Base::String))
    )
}

/// Type-operator/pseudo-type spellings this crate recognizes as vocabulary
/// but does not model a relation for (`int-mask<...>`, Psalm's
/// `properties-of<T>`, …). Checked by both catch-alls below (identifier and
/// generic tables share one normalized-name space).
///
/// Without this list these names fall to [`ContractTy::Class`], a
/// nonexistent-class reference: acceptance's class leg answers a definite
/// `No` for any non-object value — the same wrong-No hazard `key-of`/
/// `value-of` solve (ADR-0062). The honest floor is [`ContractTy::Opaque`]
/// (`Maybe`), never a manufactured `No`. `resource` and its state spellings
/// left this list with ADR-0056 §8 — see [`ContractTy::Resource`].
///
/// The table is **arity-blind** — the name is checked before any argument
/// count is — so a misspelled arity floors to `Opaque` too rather than
/// falling through to the class catch-all it would otherwise reach.
///
/// Not "any unrecognized name": an unknown identifier still falls to
/// `Class`, the signal both lanes' class machinery depends on.
const KNOWN_UNENFORCED: &[&str] = &[
    // PHPStan's array accessory predicates (issue #238): beside an array arm
    // they fold via [`fold_array_accessories`] and never reach this table;
    // standing alone or beside a non-array arm they have nothing to attach
    // to, so this entry keeps that case `Opaque` rather than `Class("hasoffset")`.
    "hasoffset",
    "hasoffsetvalue",
    "int-mask",
    "int-mask-of",
    "non-empty-literal-string",
    "arraylike-object",
    "properties-of",
    "stringable-object",
    "class-string-map",
    // PHPStan's `template-type<Subject, Owner, 'TName'>` (issue #360): known
    // vocabulary with no resolution yet (issue #361). Without the entry the
    // spelling reads as a class named `template-type` — which the dump surface
    // printed back as one — and a wrong arity, which PHPStan resolves to an
    // error type, floors here silently rather than reporting.
    "template-type",
];

/// The **derived type operators** (ADR-0089): vocabulary this crate *does*
/// hold a relation for, at one arity each, and which must floor to
/// [`ContractTy::Opaque`] at **every other** arity and as a bare identifier.
///
/// Distinct from `KNOWN_UNENFORCED`, which is for names with no relation at
/// all — putting an operator there would floor it before [`lower_generic`]'s
/// match could project it. What the two tables share is the property that
/// matters: the name is decided **before** any argument count is, so a
/// misspelled arity can never fall through to the class catch-all.
///
/// That fall-through was a live wrong-`No`. Before ADR-0089,
/// `key-of<int, int>` lowered to `Class("key-of")` — a reference to a
/// nonexistent class, whose acceptance leg answers a definite `No` for every
/// non-object value — while `key-of<int>` correctly floored to `Opaque`. A
/// wrong `No` is a false positive rather than lost precision (the
/// closure-argument variance check raises findings on `No`), so `key-of` and
/// `value-of` join the new names here rather than keeping the hazard.
const DERIVED_OPERATORS: &[&str] = &[
    "key-of",
    "value-of",
    "non-nullable",
    "return-type",
    "parameters-of",
    "exclude-from",
    "extract-from",
];

/// Is `name` a [`DERIVED_OPERATORS`] spelling? Already normalized by both
/// callers (leading `\` stripped, lowercased).
fn is_derived_operator(name: &str) -> bool {
    DERIVED_OPERATORS.contains(&name)
}

/// **The one identifier table**: what every phpdoc *keyword* spelled as a bare
/// identifier means, lowered to a [`ContractTy`]. Both lanes read this table
/// (ADR-0030's no-second-relation discipline, as [`shape_verdict`] applies it
/// to shapes, ADR-0062 §5): the fact lane via [`lower`]; `steins-infer`'s
/// proven-value lane calls it directly and judges the result with [`admits_val`].
///
/// The catch-all is load-bearing: a name that is **not** a keyword lowers to
/// [`ContractTy::Class`], the signal to hand it to each lane's own class
/// machinery — the value domain has no object inhabitant (ADR-0035/0038), so
/// this crate cannot host that judgment. `KNOWN_UNENFORCED` is checked
/// first, since those names ARE keywords and must not reach class machinery.
#[must_use]
pub fn lower_identifier(name: &str) -> ContractTy {
    let norm = name.trim_start_matches('\\').to_ascii_lowercase();
    if KNOWN_UNENFORCED.contains(&norm.as_str()) {
        return ContractTy::Opaque;
    }
    match norm.as_str() {
        "int" | "integer" => ContractTy::Base(Base::Int),
        "float" | "double" => ContractTy::Base(Base::Float),
        "string" => ContractTy::Base(Base::String),
        "bool" | "boolean" => ContractTy::Base(Base::Bool),
        "true" => ContractTy::LitBool(true),
        "false" => ContractTy::LitBool(false),
        "null" => ContractTy::Null,
        "mixed" => ContractTy::Mixed,
        "never" | "never-return" | "never-returns" | "no-return" | "noreturn" => ContractTy::Never,
        "void" => ContractTy::Opaque,
        // The possibly-undefined pseudo-type (ADR-0087, issue #395). Its own
        // variant rather than a `KNOWN_UNENFORCED` entry: the opaque floor
        // spells back as `mixed` and would lose the word, and unlike the names
        // in that table `unset` is not an unmodeled *type* — it carries no
        // value at all. Without this arm the catch-all read it as a class named
        // `unset` in the current namespace (the one plainly wrong reading,
        // zonuexe/php-typing-conformance#7).
        "unset" => ContractTy::Unset,
        // Three spellings, one leaf — see ContractTy::Resource (ADR-0056 §8).
        "resource" | "open-resource" | "closed-resource" => ContractTy::Resource,
        "scalar" => scalar(),
        "array-key" => array_key(),
        // Three subtraction spellings share one cut: `non-null-mixed` is
        // Phan's, `non-empty-mixed` PHPStan's falsy-removed `mixed`, and
        // `non-empty-scalar` below is the same cut intersected with `scalar`.
        "non-null-mixed" => ContractTy::MixedMinus(MixedCut::Null),
        "non-empty-mixed" => ContractTy::MixedMinus(MixedCut::Falsy),
        // PHPStan resolves this to `float|int<min,-1>|int<1,max>|
        // non-falsy-string|true`, silently letting `0`/`0.0` back in through its
        // unnarrowed `float` member. Steins spells the subtraction directly so
        // `0`/`0.0` are rejected with the other three (deliberate; within the
        // fixture's `E?` latitude).
        "non-empty-scalar" => ContractTy::Inter(vec![
            scalar(),
            ContractTy::MixedMinus(MixedCut::Falsy),
        ]),
        "numeric" => ContractTy::Union(vec![
            ContractTy::Base(Base::Int),
            ContractTy::Base(Base::Float),
            ContractTy::StrWith(StrPreds::NUMERIC.close()),
        ]),
        // `number` is `numeric` minus its string member — `int|float` and nothing
        // else, so a numeric string is *not* a `number`.
        "number" => {
            ContractTy::Union(vec![ContractTy::Base(Base::Int), ContractTy::Base(Base::Float)])
        }
        "numeric-string" => ContractTy::StrWith(StrPreds::NUMERIC.close()),
        "non-empty-string" => ContractTy::StrWith(StrPreds::NON_EMPTY),
        "non-falsy-string" | "truthy-string" => ContractTy::StrWith(StrPreds::NON_FALSY.close()),
        // (The casing axis — `lowercase-string`, `uppercase-string`, `uncased-string`
        // and every core × casing compound — lives in [`grid_str_preds`], reached
        // from the catch-all below: one place, not twenty arms that could drift.)
        // The array-key-cast pair. `decimal-int-string` is the string PHP casts
        // to `int` as an array key; `non-decimal-int-string` is its complement
        // within string (wider than the name suggests: `'+1'`, `'1.2'`, `'foo'`
        // all qualify). Two positive bits, not a bit + negation (`StrPreds` doc).
        "decimal-int-string" => ContractTy::StrWith(StrPreds::DECIMAL_INT.close()),
        "non-decimal-int-string" => ContractTy::StrWith(StrPreds::NON_DECIMAL_INT),
        // The class-like family (issue #236): all four name the SAME predicate
        // (PHP shares one symbol table for classes/interfaces/traits/enums,
        // and PHPStan renders all back as `class-string`) — a value property
        // needing the class table `StrPreds::of` lacks, hence `Maybe`.
        "class-string" | "interface-string" | "enum-string" | "trait-string" => {
            ContractTy::StrWith(StrPreds::CLASS_STRING.close())
        }
        // Genuinely non-extensional: `literal-string` is provenance (ADR-0038),
        // `callable-string` needs a function table this crate cannot see, and
        // `numeric-int-string` is Phan-only vocabulary with no predicate here.
        "literal-string" | "callable-string" | "numeric-int-string" => ContractTy::StrOpaque,
        "positive-int" => ContractTy::IntIn(IntRange::POSITIVE),
        "negative-int" => ContractTy::IntIn(IntRange::NEGATIVE),
        "non-negative-int" => ContractTy::IntIn(IntRange::NON_NEGATIVE),
        "non-positive-int" => {
            ContractTy::IntIn(IntRange::new(i64::MIN, 0).expect("valid range"))
        }
        // The one sign refinement that is not a single interval — a union with
        // a hole punched at zero. Flattening to one range would lose the hole
        // (PHPStan resolves it the same way: `int<min,-1>|int<1,max>`).
        "non-zero-int" => ContractTy::Union(vec![
            ContractTy::IntIn(IntRange::new(i64::MIN, -1).expect("valid range")),
            ContractTy::IntIn(IntRange::new(1, i64::MAX).expect("valid range")),
        ]),
        "array" => ContractTy::ArrayAny { non_empty: false },
        "non-empty-array" => ContractTy::ArrayAny { non_empty: true },
        // Phan's `associative-array`/`non-empty-associative-array`: an array that
        // is additionally not a list. Bare (unparameterized) form — `<K, V>` is
        // `lower_generic`'s job, below.
        "associative-array" => ContractTy::MapOf {
            key: Box::new(array_key()),
            val: Box::new(ContractTy::Mixed),
            non_empty: false,
            not_list: true,
        },
        "non-empty-associative-array" => ContractTy::MapOf {
            key: Box::new(array_key()),
            val: Box::new(ContractTy::Mixed),
            non_empty: true,
            not_list: true,
        },
        "list" => ContractTy::ListOf { elem: Box::new(ContractTy::Mixed), non_empty: false },
        "non-empty-list" => {
            ContractTy::ListOf { elem: Box::new(ContractTy::Mixed), non_empty: true }
        }
        "iterable" => ContractTy::IterableOf {
            key: Box::new(ContractTy::Mixed),
            val: Box::new(ContractTy::Mixed),
        },
        // The callable family, bare and refined (ADR-0063 P3). `callable_obl` owns
        // the vocabulary so the parenthesized-signature path agrees by construction.
        "callable"
        | "callable-object"
        | "closure"
        | "pure-callable"
        | "pure-closure"
        | "static-closure"
        | "static-pure-closure" => {
            ContractTy::CallableTy { sig: None, obl: callable_obl(&norm).unwrap_or_default() }
        }
        "object" => ContractTy::ObjectAny,
        "self" | "static" | "parent" => ContractTy::Opaque,
        // A derived operator spelled bare (`@param key-of`, `@param
        // non-nullable`) names an operator with no operand, so it states
        // nothing: the honest floor, never the class catch-all (ADR-0089 §4).
        other if is_derived_operator(other) => ContractTy::Opaque,
        // The refined-string grid (issue #240), the last keyword rung before the
        // class catch-all: every cell the speller can emit lowers back to the set
        // it was spelled from, and anything else is a class name as before.
        other => match grid_str_preds(other) {
            Some(p) => ContractTy::StrWith(p),
            None => ContractTy::Class(norm),
        },
    }
}

/// The **refined-string grid** (issue #240), read as an input spelling: `core ×
/// casing`, where core ∈ {—, `non-empty-`, `non-falsy-`, `numeric-`,
/// `non-falsy-numeric-`} and casing ∈ {—, `lowercase-`, `uppercase-`,
/// `uncased-`}, all suffixed `-string`. `None` for anything that is not a cell.
///
/// Exact inverse of [`spell::preds_keyword`]; a *parse* rather than twenty
/// arms in [`lower_identifier`] (ADR-0030): one closed decision, one
/// implementation per side, round-trip tested (`every_grid_cell_round_trips`).
///
/// The casing axis is an *identity* under the case function, not "made of
/// lowercase letters": `strtolower($s) === $s`, so an uncased string (`''`,
/// `'123'`) satisfies both halves at once — Steins' word for what PHPStan
/// spells `lowercase-string&uppercase-string` (that intersection would not
/// round-trip through a single identifier). The length half is orthogonal.
///
/// `non-falsy-numeric-` is its own core rung, not a compound: `NUMERIC` does
/// not entail `NON_FALSY` (`'0'`/`'0.0'` are numeric and falsy), so it's
/// tighter than either (PHPStan: `non-falsy-string&numeric-string`). The
/// array-key-cast pair keeps its own arms above, deliberately not an axis —
/// see [`spell::preds_keyword`] for why neither may become a rung.
fn grid_str_preds(name: &str) -> Option<StrPreds> {
    // The core rungs, longest spelling first so `non-falsy-numeric-` is not read
    // as `non-falsy-` with a stray `numeric` casing token.
    const CORES: &[(&str, StrPreds)] = &[
        ("non-falsy-numeric", StrPreds::NON_FALSY.union(StrPreds::NUMERIC)),
        ("non-falsy", StrPreds::NON_FALSY),
        ("non-empty", StrPreds::NON_EMPTY),
        ("numeric", StrPreds::NUMERIC),
    ];
    let body = name.strip_suffix("-string")?;
    let mut core = StrPreds::empty();
    let mut rest = body;
    for (tok, p) in CORES {
        if let Some(tail) = rest.strip_prefix(tok)
            && (tail.is_empty() || tail.starts_with('-'))
        {
            core = *p;
            rest = tail.strip_prefix('-').unwrap_or(tail);
            break;
        }
    }
    let casing = match rest {
        "" => StrPreds::empty(),
        "lowercase" => StrPreds::LOWERCASE,
        "uppercase" => StrPreds::UPPERCASE,
        "uncased" => StrPreds::LOWERCASE.union(StrPreds::UPPERCASE),
        // Not a cell: `foo-string` is a class name, and `decimal-int-string` and
        // the rest of the string family are their own arms above.
        _ => return None,
    };
    // The empty cell is `string`, which has its own arm: reaching here with
    // nothing recognized means the name was never a grid spelling at all.
    if core.is_empty() && casing.is_empty() {
        return None;
    }
    Some(core.union(casing))
}

/// Whether a same-named class in scope takes precedence over this keyword —
/// the vocabulary half of PHPStan's pseudo-type/class precedence rule
/// (`TypeNodeResolver::tryResolvePseudoTypeClassType`).
///
/// PHP **reserves** its native type words and `unset` (listed in the match
/// below), so the keyword always wins there. Every other spelling
/// [`lower_identifier`] knows
/// is a phpdoc **pseudo-type** (`integer`, `number`, `scalar`, `closure`, …)
/// and a legal class name, so `Integer` in scope makes `@param Integer` that
/// class, not `int`. A hyphenated keyword (`positive-int`) is not a legal
/// identifier, so nothing can shadow it.
///
/// This is the whole rule this crate can answer — *precedence* needs a class
/// registry (`steins-infer`), so callers pair this with their own gate.
#[must_use]
pub fn is_shadowable_pseudo_type(name: &str) -> bool {
    let norm = name.trim_start_matches('\\').to_ascii_lowercase();
    if norm.contains('-') {
        return false;
    }
    !matches!(
        norm.as_str(),
        "int"
            | "float"
            | "string"
            | "bool"
            | "true"
            | "false"
            | "null"
            | "mixed"
            | "never"
            | "void"
            | "iterable"
            | "object"
            | "callable"
            | "array"
            | "static"
            | "self"
            | "parent"
            // Not a native type word but a reserved *language construct*
            // (`unset()`), so `class unset {}` is a parse error and nothing can
            // shadow the pseudo-type (ADR-0087).
            | "unset"
    )
}

/// **The one generic table**: parameterized phpdoc vocabulary, lowered to a
/// [`ContractTy`]. Companion of [`lower_identifier`], public for the same
/// reason — `steins-infer`'s proven-value lane reads it rather than restating
/// the grammar. Its catch-all carries the same meaning: a base name that is
/// not vocabulary lowers to [`ContractTy::Class`] (hand it to the caller's
/// class-generic machinery), except a `KNOWN_UNENFORCED` base
/// (`int-mask<...>`, `properties-of<T>`, …), which floors to
/// [`ContractTy::Opaque`] for the same nonexistent-class-hazard reason.
#[must_use]
pub fn lower_generic(base: &str, args: &[steins_phpdoc::ast::GenericArg]) -> ContractTy {
    let norm = base.trim_start_matches('\\').to_ascii_lowercase();
    if KNOWN_UNENFORCED.contains(&norm.as_str()) {
        return ContractTy::Opaque;
    }
    let arg = |i: usize| args.get(i).map(|a| lower(&a.ty));
    match (norm.as_str(), args.len()) {
        ("array" | "non-empty-array", 1) => ContractTy::MapOf {
            key: Box::new(array_key()),
            val: Box::new(arg(0).expect("len checked")),
            non_empty: norm.starts_with("non-empty"),
            not_list: false,
        },
        ("array" | "non-empty-array", 2) => ContractTy::MapOf {
            key: Box::new(arg(0).expect("len checked")),
            val: Box::new(arg(1).expect("len checked")),
            non_empty: norm.starts_with("non-empty"),
            not_list: false,
        },
        // Phan's `associative-array<K, V>` — the same `array<K, V>` lowering
        // plus the not-a-list refusal (ADR-0062's `is_list` trinary, seeded
        // via `to_shape_fact`'s `MapOf` arm below).
        ("associative-array" | "non-empty-associative-array", 1) => ContractTy::MapOf {
            key: Box::new(array_key()),
            val: Box::new(arg(0).expect("len checked")),
            non_empty: norm.starts_with("non-empty"),
            not_list: true,
        },
        ("associative-array" | "non-empty-associative-array", 2) => ContractTy::MapOf {
            key: Box::new(arg(0).expect("len checked")),
            val: Box::new(arg(1).expect("len checked")),
            non_empty: norm.starts_with("non-empty"),
            not_list: true,
        },
        ("list" | "non-empty-list", 1) => ContractTy::ListOf {
            elem: Box::new(arg(0).expect("len checked")),
            non_empty: norm.starts_with("non-empty"),
        },
        // `int<lo, hi>` (PHPStan/Psalm/Mago) and `int-range<lo, hi>` (Phan) are the
        // same bounded range under two base names — one lowering, not two.
        ("int" | "int-range", 2) => lower_int_range(args),
        ("iterable", 1) => ContractTy::IterableOf {
            key: Box::new(ContractTy::Mixed),
            val: Box::new(arg(0).expect("len checked")),
        },
        ("iterable", 2) => ContractTy::IterableOf {
            key: Box::new(arg(0).expect("len checked")),
            val: Box::new(arg(1).expect("len checked")),
        },
        // `class-string<T>` (issue #236) — carried as the BARE predicate, `T`
        // dropped (generics vocabulary owns `T`, ADR-0032/#10). Sound: every
        // `class-string<Foo>` member is a `class-string`; only the
        // unchecked bound is lost.
        ("class-string", _) => ContractTy::StrWith(StrPreds::CLASS_STRING.close()),
        // `key-of<T>` / `value-of<T>` — derived spellings, projected out of the
        // operand's lowered [`ContractTy`] rather than re-reading the AST: one
        // lowering, then one projection (ADR-0030).
        ("key-of", 1) => project_key_of(&arg(0).expect("len checked")),
        ("value-of", 1) => project_value_of(&arg(0).expect("len checked")),
        // The ADR-0089 roster, projected the same way and for the same reason.
        ("non-nullable", 1) => project_non_nullable(&arg(0).expect("len checked")),
        ("return-type", 1) => project_return_type(&arg(0).expect("len checked")),
        ("parameters-of", 1) => project_parameters_of(&arg(0).expect("len checked")),
        ("exclude-from", 2) => project_arm_filter(
            &arg(0).expect("len checked"),
            &arg(1).expect("len checked"),
            ArmFilter::Exclude,
        ),
        ("extract-from", 2) => project_arm_filter(
            &arg(0).expect("len checked"),
            &arg(1).expect("len checked"),
            ArmFilter::Extract,
        ),
        // Every OTHER arity of a derived operator, checked before the class
        // catch-all can manufacture a `No` out of it — see [`DERIVED_OPERATORS`].
        (other, _) if is_derived_operator(other) => ContractTy::Opaque,
        _ => ContractTy::Class(norm),
    }
}

/// Fold projected members into one contract: nothing is [`ContractTy::Never`]
/// (empty shape, no keys/values), one member is itself, and the rest is a
/// `Union` in declaration order with duplicates dropped —
/// `value-of<array{a: int, b: int}>` is `int`, not `int|int`.
fn union_of(members: Vec<ContractTy>) -> ContractTy {
    let mut uniq: Vec<ContractTy> = Vec::with_capacity(members.len());
    for m in members {
        if !uniq.contains(&m) {
            uniq.push(m);
        }
    }
    match uniq.len() {
        0 => ContractTy::Never,
        1 => uniq.pop().expect("len checked"),
        _ => ContractTy::Union(uniq),
    }
}

/// `key-of<T>`: the type of the keys `T`'s realizations carry, projected out
/// of the already-lowered operand. Enumerable exactly where the declaration
/// pins the key set down:
///
/// | Operand | `key-of` |
/// | --- | --- |
/// | sealed `array{a: int, b: string}` / `list{…}` | the literal key union (`'a'\|'b'` / `0\|1`) |
/// | `array<K, V>` / `associative-array<K, V>` | `K` — already the key contract |
/// | `list<T>` / `non-empty-list<T>` | `int<0, max>`, by #14939's `0..n-1` keys |
/// | `array` / `non-empty-array` | `array-key` |
/// | anything else | [`ContractTy::Opaque`] |
///
/// **Optional keys count**: a `b?:` field is still a key the array may carry
/// (PHPStan's `Type::getKeysArray()` includes it), so `CField::optional`
/// isn't filtered on. **An unsealed shape is not enumerable**: `array{a:
/// int, ...}` admits keys the declaration never named, so `Opaque` is
/// honest, not the declared prefix — same for a template, const fetch, or class.
#[must_use]
pub fn project_key_of(inner: &ContractTy) -> ContractTy {
    match inner {
        ContractTy::Shape { fields, sealed: true, .. } => union_of(
            fields
                .iter()
                .map(|f| match &f.key {
                    CKey::Int(i) => ContractTy::LitInt(*i),
                    CKey::Str(s) => ContractTy::LitStr(s.clone()),
                })
                .collect(),
        ),
        ContractTy::ListOf { .. } => ContractTy::IntIn(IntRange::NON_NEGATIVE),
        ContractTy::MapOf { key, .. } => (**key).clone(),
        ContractTy::ArrayAny { .. } => array_key(),
        _ => ContractTy::Opaque,
    }
}

/// `value-of<T>`: the type of the values `T`'s realizations carry — the mirror
/// of [`project_key_of`], enumerable under the same conditions.
///
/// `array` / `non-empty-array` is deliberately **not** projected to `mixed`
/// here: an unparameterized array states nothing about its values, and
/// `Opaque` is the same silence with less to go wrong.
#[must_use]
pub fn project_value_of(inner: &ContractTy) -> ContractTy {
    match inner {
        ContractTy::Shape { fields, sealed: true, .. } => {
            union_of(fields.iter().map(|f| f.ty.clone()).collect())
        }
        ContractTy::ListOf { elem, .. } => (**elem).clone(),
        ContractTy::MapOf { val, .. } => (**val).clone(),
        _ => ContractTy::Opaque,
    }
}

/// `non-nullable<T>`: `T` with its `null` arm deleted — the arm-lane
/// subtraction ADR-0052 §1 already performs on declared alternatives, given a
/// name (ADR-0089 §5.1).
///
/// | Operand | `non-nullable` |
/// | --- | --- |
/// | `int\|null`, `?\DateTime` | the union minus its `null` arm |
/// | `mixed` | `non-null-mixed` ([`MixedCut::Null`]) |
/// | `null` | [`ContractTy::Never`] |
/// | anything carrying no `null` arm | itself |
/// | [`ContractTy::Opaque`] | `Opaque` — nothing is known, so nothing is cut |
///
/// **`mixed` is the cell that touches the registry.** It is a *second*
/// declaration-side construction site for [`ContractTy::MixedMinus`], where
/// ADR-0030's registry entry 6 recorded exactly one ([`lower_identifier`]'s
/// `non-null-mixed` keyword). Entry 6's claim survives verbatim — the variant
/// is still declaration-side vocabulary no inference path produces; only the
/// number of spellings that reach it moves.
///
/// **`non-nullable<null>` is `never`, and that is honest** rather than a
/// manufactured `No`: the empty set is what the operand asked for, and `never`
/// is a type an author can already spell. Whether an operator that provably
/// empties its operand should *also* report is left open (ADR-0089 §5.1).
#[must_use]
pub fn project_non_nullable(inner: &ContractTy) -> ContractTy {
    match inner {
        ContractTy::Null => ContractTy::Never,
        ContractTy::Mixed => ContractTy::MixedMinus(MixedCut::Null),
        // Arm-wise, so a `mixed` beside a `null` reaches the cut instead of
        // surviving as a `mixed` arm that still admits null.
        ContractTy::Union(members) => union_of(
            members
                .iter()
                .map(project_non_nullable)
                .filter(|m| !matches!(m, ContractTy::Never))
                .collect(),
        ),
        // `non-empty-mixed` already excludes null (it is the falsy cut), and
        // every other arm carries no `null` member to delete.
        other => other.clone(),
    }
}

/// `return-type<F>`: the `R` of a declared `callable(P): R`, read off
/// [`CallableSig::ret`] (ADR-0089 §5.2).
///
/// A bare `callable`/`Closure` carries no signature (`sig: None`) and floors,
/// as does every non-callable operand and every arity but one. A union floors
/// unless **every** arm is a signatured callable, per ADR-0089 §4: an arm the
/// rule declines takes the whole type down, rather than one arm widening to
/// `Opaque` while the others narrow.
///
/// **There is no `typeof`.** TypeScript writes `ReturnType<typeof f>`; Steins
/// has no operator turning a declared function's *name* into its type, so the
/// operand is a callable **type** spelling and never a reference to a
/// function. That is the limit on this operator, and it is deliberate.
#[must_use]
pub fn project_return_type(inner: &ContractTy) -> ContractTy {
    match inner {
        ContractTy::CallableTy { sig: Some(sig), .. } => sig.ret.clone(),
        ContractTy::Union(members) => {
            let rets: Vec<ContractTy> = members.iter().map(project_return_type).collect();
            if rets.iter().any(|r| matches!(r, ContractTy::Opaque)) {
                return ContractTy::Opaque;
            }
            union_of(rets)
        }
        _ => ContractTy::Opaque,
    }
}

/// `parameters-of<F>`: the argument list a declared signature describes, as a
/// positional shape — `\Closure(int, string=): bool` is `list{int, string?}`
/// (ADR-0089 §5.2).
///
/// Optional parameters become optional fields, and a variadic becomes the
/// unsealed tail (`list{int, ...<string>}`) — the faithful reading of the
/// array `call_user_func_array` would be handed.
///
/// The tail carries **no key contract**, deliberately. A keyed tail was the
/// first reading (a variadic really does collect into consecutive int keys),
/// but `list{int, ...<int, string>}` is not a spelling the grammar accepts, so
/// that shape would have been a [`ContractTy`] no phpdoc string can express —
/// the speller drops the key and the round-trip stops being structural. It
/// also bought nothing: the list flavour already forces int keys, so the two
/// forms answer identically for every value (`the_projections_round_trip`).
///
/// **A by-reference parameter floors the whole operator.** A `&$x` position
/// does not carry the declared type of a value the caller supplies; it names a
/// binding the callee writes back through, and the array that would stand in
/// for it cannot be spelled. [`CallableParamTy::by_ref`] is already the axis
/// the closure-argument variance check stays silent on, and this keeps that
/// silence rather than inventing an answer for it.
///
/// **The reading is positional by construction.** PHP 8's named arguments let
/// `call_user_func_array` take a string-keyed array, which this sealed
/// positional shape refuses — but that refusal is the *author's* claim, made
/// no differently than by writing `@param list{int, string}` out by hand, and
/// a declaration is an authoritative envelope (ADR-0001). The wrong-`No`
/// prohibition is about the analyzer manufacturing a `No` out of a name it did
/// not understand, not about honoring one an author spelled.
#[must_use]
pub fn project_parameters_of(inner: &ContractTy) -> ContractTy {
    let ContractTy::CallableTy { sig: Some(sig), .. } = inner else {
        return ContractTy::Opaque;
    };
    // Two signatures this projection declines to read: one that writes back
    // through a parameter, and one whose variadic is not last (the grammar
    // permits the spelling; no argument array corresponds to it).
    if sig.params.iter().any(|p| p.by_ref) {
        return ContractTy::Opaque;
    }
    if sig.params.iter().rev().skip(1).any(|p| p.variadic) {
        return ContractTy::Opaque;
    }
    let mut fields: Vec<CField> = Vec::with_capacity(sig.params.len());
    let mut unsealed = None;
    let mut key: i64 = 0;
    for p in &sig.params {
        if p.variadic {
            unsealed = Some((None, Box::new(p.ty.clone())));
            break;
        }
        fields.push(CField { key: CKey::Int(key), optional: p.optional, ty: p.ty.clone() });
        key += 1;
    }
    ContractTy::Shape {
        // The empty sealed shape is `array{}` in every lane: the list flavour
        // is vacuous there (the empty array is a list) and `lower_shape`
        // produces that form, so match it rather than minting a second
        // `ContractTy` for one type — two structures that spell alike are what
        // `arm_eq` and the dedup pass then have to paper over.
        list: !(fields.is_empty() && unsealed.is_none()),
        fields,
        sealed: unsealed.is_none(),
        // Left to the denotational computation, exactly as `lower_shape`
        // leaves it: a required field already implies non-emptiness, and
        // `ShapeFact::normalize` is what decides `is_list` besides.
        non_empty: false,
        unsealed,
    }
}

/// Which direction [`project_arm_filter`] reads its verdict in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArmFilter {
    /// `exclude-from<T, U>` — drop `T`'s arms that `U` **provably** subsumes.
    Exclude,
    /// `extract-from<T, U>` — keep `T`'s arms that `U` does not **provably**
    /// fail to subsume.
    Extract,
}

/// `exclude-from<T, U>` / `extract-from<T, U>`: `T`'s arm list filtered
/// against `U` through [`normalize::subsumes`], the single pairwise arm
/// relation everything else reduces to (ADR-0052 §4). TypeScript's
/// `Exclude`/`Extract` distribute over unions; Steins' arm lane *is*
/// distribution, so there is nothing to add for that (ADR-0089 §5.3).
///
/// **Both filters widen**, which is the only safe direction, and they reach it
/// from opposite ends of the trinary:
///
/// - `exclude-from` drops an arm on `Yes` **only**. Deleting an arm on an
///   undecided relation is exactly the arm deletion ADR-0052 forbids without
///   proof, so a `Maybe` keeps the arm and the result is wider than a perfect
///   `Exclude`.
/// - `extract-from` keeps an arm on `Yes` **and on `Maybe`**, dropping only on
///   a proven `No`. Keeping on `Yes` alone would run the *narrowing* way: an
///   arm the value may well inhabit would vanish, and a missing arm is a
///   manufactured `No` at the acceptance leg.
///
/// So an [`ContractTy::Opaque`] arm survives both, because `subsumes` answers
/// `Maybe` about it — `extract-from<Opaque, T>` is `Opaque`, never `Never`.
/// `exclude-from<T, mixed>` really is `never`, on the other hand: `mixed`
/// subsumes every arm with a proven `Yes`, which is the correct answer and not
/// an undecided one.
fn project_arm_filter(subject: &ContractTy, against: &ContractTy, filter: ArmFilter) -> ContractTy {
    let arms: &[ContractTy] = match subject {
        ContractTy::Union(members) => members,
        one => std::slice::from_ref(one),
    };
    let kept: Vec<ContractTy> = arms
        .iter()
        .filter(|arm| {
            let verdict = normalize::subsumes(against, arm);
            match filter {
                ArmFilter::Exclude => !verdict.is_yes(),
                ArmFilter::Extract => verdict != Certainty::No,
            }
        })
        .cloned()
        .collect();
    union_of(kept)
}

/// Lower a `callable(P1, P2=): R` / `\Closure(P): R` signature (issue #11).
///
/// A **template-bearing** signature (`callable(T): T`, `\Closure<T>(T): R`) is
/// unrepresentable: its type variables would lower to bare `Class` arms and
/// yield false judgments, and Steins runs no call-site template solver
/// (ADR-0032/0051). It drops to bare `CallableTy(None)` — the same silent
/// floor as an unsignatured `callable` — so every lowered [`CallableSig`]
/// carries only ground contract arms.
fn lower_callable(c: &steins_phpdoc::ast::CallableType) -> ContractTy {
    // The identifier before the `(` still names the refined spelling: a
    // `pure-callable(int): int` is both a signature and a purity obligation;
    // an identifier outside the callable vocabulary carries none.
    let obl = callable_obl(&c.identifier.trim_start_matches('\\').to_ascii_lowercase())
        .unwrap_or_default();
    if !c.templates.is_empty() {
        return ContractTy::CallableTy { sig: None, obl };
    }
    let params = c
        .params
        .iter()
        .map(|p| CallableParamTy {
            ty: lower(&p.ty),
            optional: p.is_optional,
            variadic: p.is_variadic,
            by_ref: p.is_reference,
        })
        .collect();
    ContractTy::CallableTy {
        sig: Some(Box::new(CallableSig { params, ret: lower(&c.return_type) })),
        obl,
    }
}

fn lower_int_range(args: &[steins_phpdoc::ast::GenericArg]) -> ContractTy {
    let bound = |ty: &Type, default: i64| -> Option<i64> {
        match &ty.kind {
            TypeKind::Identifier(id) if id.eq_ignore_ascii_case("min") => Some(i64::MIN),
            TypeKind::Identifier(id) if id.eq_ignore_ascii_case("max") => Some(i64::MAX),
            TypeKind::Const(ConstExpr::Int(s)) => s.replace('_', "").parse().ok(),
            _ => {
                let _ = default;
                None
            }
        }
    };
    match (bound(&args[0].ty, i64::MIN), bound(&args[1].ty, i64::MAX)) {
        (Some(lo), Some(hi)) => match IntRange::new(lo, hi) {
            Some(r) => ContractTy::IntIn(r),
            None => ContractTy::Never,
        },
        _ => ContractTy::Opaque,
    }
}

/// The normalized runtime keys a shape's items denote, in item order:
/// positional items take the running auto-index, and PHP folds an
/// integer-like string/bareword key to an int key (`array{'9': T}` declares
/// key `9`, as `[9 => …]` builds it).
///
/// `None` when a key is not resolvable (const-fetch key, unparseable int
/// literal), making the whole shape undecidable (`Opaque`/`Maybe`).
#[must_use]
pub fn shape_keys(shape: &steins_phpdoc::ast::ArrayShape) -> Option<Vec<CKey>> {
    let mut keys = Vec::with_capacity(shape.items.len());
    let mut next_auto: i64 = 0;
    for item in &shape.items {
        let key = match &item.key {
            None => CKey::Int(next_auto),
            Some(ShapeKey::Int(s)) => CKey::Int(s.replace('_', "").parse::<i64>().ok()?),
            Some(ShapeKey::Str(lit)) => norm_shape_key(&string_lit_value(lit)),
            Some(ShapeKey::Ident(name)) => norm_shape_key(name),
            Some(ShapeKey::ConstFetch { .. }) => return None,
        };
        // Every int key — declared or PHP-folded — advances the auto-index.
        if let CKey::Int(v) = key {
            next_auto = next_auto.max(v.saturating_add(1));
        }
        keys.push(key);
    }
    Some(keys)
}

/// PHP array-key normalization for a shape's string/bareword key: a canonical
/// decimal integer spelling denotes an int key, everything else a string key.
fn norm_shape_key(s: &str) -> CKey {
    match s.parse::<i64>() {
        Ok(i) if i.to_string() == s => CKey::Int(i),
        _ => CKey::Str(PhpStr::from(s)),
    }
}

fn lower_shape(shape: &steins_phpdoc::ast::ArrayShape) -> ContractTy {
    let list = matches!(shape.kind, ArrayShapeKind::List | ArrayShapeKind::NonEmptyList);
    let non_empty =
        matches!(shape.kind, ArrayShapeKind::NonEmptyArray | ArrayShapeKind::NonEmptyList);
    let Some(keys) = shape_keys(shape) else { return ContractTy::Opaque };
    let mut fields = Vec::with_capacity(shape.items.len());
    for (key, item) in keys.into_iter().zip(&shape.items) {
        fields.push(CField { key, optional: item.optional, ty: lower(&item.value) });
    }
    let unsealed = shape.unsealed.as_ref().map(|u| {
        (u.key.as_ref().map(|k| Box::new(lower(k))), Box::new(lower(&u.value)))
    });
    ContractTy::Shape { list, fields, sealed: shape.sealed, non_empty, unsealed }
}

/// **The one lowering** from the contract lane's array vocabulary to the fact
/// domain's canonical array form (ADR-0062 A-G1): all four array-flavored
/// [`ContractTy`] variants become a single [`ShapeFact`]; anything else
/// (scalars, classes, `iterable`, unions, …) is `None` — not an array truth.
/// The degenerate forms, per A-G1:
///
/// | Contract | Shape fact |
/// | --- | --- |
/// | `array` / `non-empty-array` | no fields, untyped unsealed tail |
/// | `array<K, V>` | no fields, tail typed by `K`'s key class and `V`'s fact |
/// | `list<T>` | no fields, int-keyed tail typed by `T`, `is_list` seeded `Yes` |
/// | `associative-array<K, V>` | no fields, tail as `array<K, V>`, `is_list` seeded `No` |
/// | `array{…}` / `list{…}` | the declared fields and tail |
///
/// `is_list` is never taken as truth: [`ShapeFact::normalize`]'s denotational
/// computation *sharpens* the seed and may contradict a `list`-flavored
/// declaration whose keys make it impossible.
#[must_use]
pub fn to_shape_fact(ty: &ContractTy) -> Option<ShapeFact> {
    match ty {
        ContractTy::ArrayAny { non_empty } => Some(ShapeFact::normalize(
            Vec::new(),
            DTail::Unsealed { key: KeyClass::ArrayKey, value: None },
            Certainty::Maybe,
            *non_empty,
            Vec::new(),
        )),
        ContractTy::ListOf { elem, non_empty } => Some(ShapeFact::normalize(
            Vec::new(),
            DTail::Unsealed { key: KeyClass::Int, value: to_fact(elem).map(Box::new) },
            Certainty::Yes,
            *non_empty,
            Vec::new(),
        )),
        ContractTy::MapOf { key, val, non_empty, not_list } => Some(ShapeFact::normalize(
            Vec::new(),
            DTail::Unsealed {
                key: contract_key_class(key),
                value: to_fact(val).map(Box::new),
            },
            if *not_list { Certainty::No } else { Certainty::Maybe },
            *non_empty,
            Vec::new(),
        )),
        ContractTy::Shape { list, fields, sealed, non_empty, unsealed } => {
            Some(shape_fact_of_parts(*list, fields, *sealed, *non_empty, unsealed))
        }
        // `iterable<K, V>` is deliberately absent: a Traversable is not an
        // array, so it states no array truth (its array case is judged by
        // acceptance, not by a fact).
        _ => None,
    }
}

/// The value-slot lowering (A-G1a): the [`Fact`] a declared contract states
/// about ONE value, or `None` where the fact domain can't express it (costs
/// no fidelity — the declared contract still lives in the arm lane).
/// `None` covers, deliberately:
///
/// * classes, `object`, `callable`, `iterable`, and any intersection not ALL
///   string refinements — no fact form (all-`StrWith` exception, issue #240,
///   is [`inter_str_preds`]);
/// * `mixed` / `Opaque` / `never` — an unknown slot already *is* `mixed`;
/// * `literal-string` &c. ([`ContractTy::StrOpaque`]) — non-extensional
///   (ADR-0038). `class-string` left this bucket with issue #236 and gets
///   the ordinary refined-string fact;
/// * **`float`/float literals** — `Base(Float)` accepts ints (PHPStan core
///   semantics) but `Fact::General { base: Float }` does not, so lowering
///   would reject values the declaration admits. Floor stays the sound side;
/// * unions the domain cannot join into one fact (`int|string`) — the join
///   itself decides, so `?int`/`'a'|'b'` do lower.
#[must_use]
pub fn to_fact(ty: &ContractTy) -> Option<Fact> {
    match ty {
        ContractTy::Base(Base::Float) | ContractTy::LitFloat(_) => None,
        ContractTy::Base(b) => Some(Fact::General { base: *b, nullable: false }),
        ContractTy::IntIn(r) => Some(Fact::refined(Base::Int, Refinement::Int(*r), false)),
        ContractTy::StrWith(p) => Some(Fact::refined(Base::String, Refinement::Str(*p), false)),
        // A conjunction of string refinements IS a predicate set (issue #240):
        // the domain's own representation of `A&B` is the closed union of the
        // two bit sets, so this is the same lowering as the arm above with the
        // fold in front of it, not a second reading of the vocabulary.
        ContractTy::Inter(members) => inter_str_preds(members)
            .map(|p| Fact::refined(Base::String, Refinement::Str(p), false)),
        ContractTy::LitInt(i) => Some(Fact::Singleton(Val::Int(*i))),
        ContractTy::LitStr(s) => Some(Fact::Singleton(Val::Str(s.clone()))),
        ContractTy::LitBool(b) => Some(Fact::Singleton(Val::Bool(*b))),
        ContractTy::Null => Some(Fact::Singleton(Val::Null)),
        ContractTy::ArrayAny { .. }
        | ContractTy::ListOf { .. }
        | ContractTy::MapOf { .. }
        | ContractTy::Shape { .. } => {
            to_shape_fact(ty).map(|shape| Fact::Shape { shape: Box::new(shape), nullable: false })
        }
        // A union is one fact only when the domain's own join can hold it:
        // `?int` folds null into nullability, `'a'|'b'` stays a `OneOf`,
        // `int|string` is unrepresentable and floors.
        ContractTy::Union(members) => {
            let mut acc: Option<Fact> = None;
            for m in members {
                let f = to_fact(m)?;
                acc = Some(match acc {
                    None => f,
                    Some(prev) => prev.join(&f)?,
                });
            }
            acc
        }
        _ => None,
    }
}

/// The single closed [`StrPreds`] set an intersection of string refinements
/// denotes, or `None` as soon as one member is anything else (issue #240).
///
/// `A&B` over string refinements needs no intersection *algebra*: a refined
/// string is already a conjunction of predicates (ADR-0035), so intersecting
/// two is the union of their bits ([`StrPreds::union`] closes under
/// implication) — one fold, shared by [`to_fact`], the arm speller, and
/// `steins-infer`'s curated-row lowering.
///
/// A non-`StrWith` member refuses the WHOLE intersection (stays in the arm
/// lane, judged by `Inter` arms). Complementary bits are not a special case:
/// `decimal-int-string&non-decimal-int-string` folds to the set carrying
/// both, denoting ∅ ([`StrPreds`] module doc) — a legitimate `StrWith`.
/// [`StrPreds::CLASS_STRING`] folds like any other bit (issue #236).
#[must_use]
pub fn inter_str_preds(members: &[ContractTy]) -> Option<StrPreds> {
    let mut acc = StrPreds::empty();
    for m in members {
        let ContractTy::StrWith(p) = m else { return None };
        acc = acc.union(*p);
    }
    // An empty intersection denotes `mixed`, not a string: it has no member to
    // take a predicate from, and answering `string` would invent a claim.
    (!members.is_empty()).then_some(acc)
}

/// Fold PHPStan's array **accessory predicates** into ADR-0062's array
/// vocabulary, or `None` when the intersection is not of that shape (issue
/// #238). `non-empty-array<string, int>&hasOffset('foo')` is PHPStan's
/// spelling for a fact Steins already carries — an unsealed shape with a
/// required key — so this is a translation, not an intersection algebra:
///
/// ```text
/// non-empty-array<string, int>&hasOffset('foo')        → non-empty-array{foo: int, ...<string, int>}
/// non-empty-array<string, int>&hasOffsetValue('foo', 17) → non-empty-array{foo: 17, ...<string, int>}
/// non-empty-list<int>&hasOffsetValue(0, 17)            → non-empty-list{17, ...<int, int>}
/// ```
///
/// `hasOffset(K)` states presence only, so the field takes the base's own
/// value contract; `hasOffsetValue(K, V)` states both, so the field takes
/// `V`. Runs on the **AST**, before lowering — `ContractTy` gains no variant
/// here, and accessory names lower to [`ContractTy::Opaque`]
/// (`KNOWN_UNENFORCED`) wherever this fold declines.
///
/// # Refusals (the honest floor, never a widening)
///
/// * **a non-array base** — the predicate would sit on a class arm, where
///   ADR-0062's vocabulary says nothing;
/// * **more than one non-accessory arm** — no rule for which to build from;
/// * **a non-literal key** — [`CKey`] is a literal by construction;
/// * **an already-`Shape` base** — PHPStan never spells one, and merging is
///   a second rule this slice skips.
fn fold_array_accessories(types: &[steins_phpdoc::ast::Type]) -> Option<ContractTy> {
    use steins_phpdoc::ast::TypeKind;

    let mut base: Option<&steins_phpdoc::ast::Type> = None;
    let mut accessories: Vec<(&str, &[steins_phpdoc::ast::GenericArg])> = Vec::new();
    for t in types {
        if let TypeKind::Generic { base: b, args } = &t.kind
            && is_accessory_base(b)
        {
            accessories.push((b.as_str(), args.as_slice()));
        } else if base.replace(t).is_some() {
            // A second non-accessory arm has no fold rule — refuse the whole thing.
            return None;
        }
    }
    if accessories.is_empty() {
        return None;
    }
    let (list, key_ty, val_ty) = match lower(base?) {
        ContractTy::ArrayAny { .. } => (false, None, ContractTy::Mixed),
        ContractTy::MapOf { key, val, .. } => (false, Some(*key), *val),
        // A list tail carries no key: `list: true` already pins keys to
        // `0..n-1`; naming `int` again would be a second, weaker copy, and
        // `list{…, ...<V>}` (the spelling this must agree with) lowers to
        // exactly this `None`.
        ContractTy::ListOf { elem, .. } => (true, None, *elem),
        _ => return None,
    };

    let mut fields: Vec<CField> = Vec::new();
    for (name, args) in accessories {
        let (key_arg, value) = match (name.to_ascii_lowercase().as_str(), args.len()) {
            ("hasoffset", 1) => (&args[0], val_ty.clone()),
            ("hasoffsetvalue", 2) => (&args[0], lower(&args[1].ty)),
            _ => return None,
        };
        let key = match lower(&key_arg.ty) {
            ContractTy::LitInt(i) => CKey::Int(i),
            ContractTy::LitStr(s) => CKey::Str(s),
            _ => return None,
        };
        // A repeated key is one key: the later predicate is the more specific
        // statement about it (`hasOffset('a')&hasOffsetValue('a', 17)`), and two
        // fields with one key is not a shape any producer here can build.
        match fields.iter_mut().find(|f| f.key == key) {
            Some(existing) => existing.ty = value,
            None => fields.push(CField { key, optional: false, ty: value }),
        }
    }

    Some(ContractTy::Shape {
        list,
        fields,
        // The predicates say which keys are *present*, never which are all there
        // are — the tail stays open and carries the base's own key/value contract.
        sealed: false,
        // A required key already forbids the empty array, so this holds whatever
        // the base's own `non-empty-` modifier said.
        non_empty: true,
        unsealed: Some((key_ty.map(Box::new), Box::new(val_ty))),
    })
}

/// Whether a generic base name is one of the array accessory predicates
/// [`fold_array_accessories`] consumes. Mirrors the parser's own closed list
/// (`steins_phpdoc`'s `is_accessory_predicate`); both are case-blind.
fn is_accessory_base(base: &str) -> bool {
    base.eq_ignore_ascii_case("hasOffset") || base.eq_ignore_ascii_case("hasOffsetValue")
}

/// Lower a declared shape's parts into the canonical [`ShapeFact`] — the
/// shared core of [`to_shape_fact`]'s `Shape` arm and [`shape_is_list`].
///
/// Field presence is the declared optionality at the *declared* stratum
/// (`Required { witnessed: false }`): a docblock states presence, it does
/// not witness it (§3 — only a guard that really executed promotes it).
fn shape_fact_of_parts(
    list: bool,
    fields: &[CField],
    sealed: bool,
    non_empty: bool,
    unsealed: &Option<(Option<Box<ContractTy>>, Box<ContractTy>)>,
) -> ShapeFact {
    let dfields: Vec<(DKey, DPresence, Option<Box<Fact>>)> = fields
        .iter()
        .map(|f| {
            let key = ckey_to_domain(&f.key);
            let presence =
                if f.optional { DPresence::Optional } else { DPresence::Required { witnessed: false } };
            (key, presence, to_fact(&f.ty).map(Box::new))
        })
        .collect();
    let tail = if sealed {
        DTail::Sealed
    } else {
        let key_class = unsealed
            .as_ref()
            .and_then(|(k, _)| k.as_deref())
            .map(contract_key_class)
            .unwrap_or(KeyClass::ArrayKey);
        let value = unsealed.as_ref().and_then(|(_, v)| to_fact(v)).map(Box::new);
        DTail::Unsealed { key: key_class, value }
    };
    let given = if list { Certainty::Yes } else { Certainty::Maybe };
    ShapeFact::normalize(dfields, tail, given, non_empty, Vec::new())
}

/// The denotational `is_list` trinary for a declared `Shape` arm's fields/tail
/// (ADR-0062 §6): reused from [`steins_domain::ShapeFact::normalize`] rather
/// than reimplemented here or in the speller (`spell.rs` calls this, never
/// its own copy) — via the same [`shape_fact_of_parts`] lowering
/// [`to_shape_fact`] uses, so the spelled verdict and the seeded fact can
/// never disagree.
///
/// `list` is the declared `list{…}`/`array{…}` keyword: it seeds the
/// `Certainty` that [`ShapeFact::normalize`] sharpens (never contradicts), as
/// `list<T>`'s own lowering does (A-G1) — forced `Yes` unless the fields
/// themselves prove otherwise (e.g. a required string key).
#[must_use]
pub(crate) fn shape_is_list(
    list: bool,
    fields: &[CField],
    sealed: bool,
    non_empty: bool,
    unsealed: &Option<(Option<Box<ContractTy>>, Box<ContractTy>)>,
) -> Certainty {
    shape_fact_of_parts(list, fields, sealed, non_empty, unsealed).is_list
}

pub(crate) fn ckey_to_domain(k: &CKey) -> DKey {
    match k {
        CKey::Int(i) => DKey::Int(*i),
        CKey::Str(s) => DKey::Str(s.clone()),
    }
}

/// The unsealed-tail key class a lowered key contract denotes: bare `int`/
/// `string` narrow the tail's key class, anything else (including the
/// `array-key` union) is the honest `ArrayKey` floor.
fn contract_key_class(ty: &ContractTy) -> KeyClass {
    match ty {
        ContractTy::Base(Base::Int) => KeyClass::Int,
        ContractTy::Base(Base::String) => KeyClass::Str,
        _ => KeyClass::ArrayKey,
    }
}

fn lower_const(c: &ConstExpr) -> ContractTy {
    match c {
        ConstExpr::Int(s) => {
            s.replace('_', "").parse().map_or(ContractTy::Opaque, ContractTy::LitInt)
        }
        ConstExpr::Float(s) => {
            s.replace('_', "").parse().map_or(ContractTy::Opaque, ContractTy::LitFloat)
        }
        ConstExpr::Str(lit) => ContractTy::LitStr(PhpStr::from(string_lit_value(lit))),
        ConstExpr::True => ContractTy::LitBool(true),
        ConstExpr::False => ContractTy::LitBool(false),
        ConstExpr::Null => ContractTy::Null,
        ConstExpr::Fetch { .. } => ContractTy::Opaque,
    }
}

fn string_lit_value(lit: &StringLit) -> String {
    match lit {
        StringLit::Single(v) | StringLit::Double(v) => v.clone(),
    }
}

/// The [`KNOWN_UNENFORCED`] hazard fix — a known-vocabulary pseudo-type
/// spelling floors to [`ContractTy::Opaque`] (always `Maybe`) instead of the
/// nonexistent-class-reference `Class` catch-all, which would otherwise
/// manufacture a `No` for every non-object value (same hazard as `key-of`/
/// `value-of`).
#[cfg(test)]
mod known_unenforced_tests {
    use super::*;
    use steins_domain::Val;

    #[test]
    fn known_unenforced_identifiers_lower_to_opaque() {
        for name in [
            "non-empty-literal-string",
            "arraylike-object",
            "stringable-object",
        ] {
            assert_eq!(lower_identifier(name), ContractTy::Opaque, "{name} should lower to Opaque");
        }
    }

    /// The three spellings that LEFT the list with ADR-0056 §8: the class
    /// catch-all's `No` was right for the wrong reason; the leaf reaches it
    /// directly and leaves the object case undecided (`steins-infer`).
    #[test]
    fn the_three_resource_spellings_lower_to_one_leaf() {
        for name in ["resource", "open-resource", "closed-resource", "RESOURCE", "\\resource"] {
            assert_eq!(
                lower_identifier(name),
                ContractTy::Resource,
                "{name} should lower to the resource leaf",
            );
        }
        // Kind, not state, is modeled: all three spell back as the one thing.
        for spelling in ["resource", "open-resource", "closed-resource"] {
            assert_eq!(
                spell::spell_arms(std::slice::from_ref(&lower_str(spelling).unwrap())).as_deref(),
                Some("resource"),
            );
        }
    }

    /// No domain value is a resource, and each is a definite `No` (not the old
    /// opaque floor's `Maybe`) — no coercion path in either mode (probed at
    /// 8.5.9).
    #[test]
    fn no_domain_value_inhabits_the_resource_leaf() {
        let vals = [
            Val::Int(0),
            Val::Int(7),
            Val::Float(1.5),
            Val::Str("stream".into()),
            Val::Str(String::new().into()),
            Val::Bool(true),
            Val::Bool(false),
            Val::Null,
            Val::Array(vec![]),
        ];
        for v in &vals {
            assert_eq!(
                admits_val(&ContractTy::Resource, v),
                Certainty::No,
                "{v:?} is not a resource",
            );
        }
    }

    /// The containment direction `dedup_arms`/`subtract` read: no hierarchy,
    /// so both cuts of `mixed` are exact — every resource is truthy, even
    /// closed ones (`fclose($h); (bool) $h === true`).
    #[test]
    fn only_mixed_its_cuts_and_the_leaf_itself_cover_a_resource() {
        for covering in ["mixed", "non-null-mixed", "non-empty-mixed", "resource"] {
            assert_eq!(
                normalize::subsumes(&lower_str(covering).unwrap(), &ContractTy::Resource),
                Certainty::Yes,
                "{covering} covers every resource",
            );
        }
        for refusing in ["int", "string", "bool", "float", "null", "array", "object", "callable"] {
            assert_eq!(
                normalize::subsumes(&lower_str(refusing).unwrap(), &ContractTy::Resource),
                Certainty::No,
                "{refusing} covers no resource",
            );
        }
        // A union covers it iff some arm does; `Opaque` says nothing.
        assert_eq!(
            normalize::subsumes(&lower_str("int|resource").unwrap(), &ContractTy::Resource),
            Certainty::Yes,
        );
        assert_eq!(
            normalize::subsumes(&ContractTy::Opaque, &ContractTy::Resource),
            Certainty::Maybe,
        );
    }

    /// The `false` subtraction ADR-0056 §8.4 narrowing rests on: `resource|false`
    /// minus `false` must leave the resource arm standing.
    #[test]
    fn subtracting_false_leaves_the_resource_arm() {
        struct NoHierarchy;
        impl normalize::IsaOracle for NoHierarchy {
            fn is_a(&self, _sub: &str, _sup: &str) -> Certainty {
                Certainty::Maybe
            }
            fn is_final(&self, _class: &str) -> bool {
                false
            }
        }
        let sub = normalize::Subtrahend::Value(Val::Bool(false));
        assert!(matches!(
            normalize::subtract_arm(&sub, &ContractTy::Resource, &NoHierarchy),
            normalize::ArmFate::Survives,
        ));
        assert!(matches!(
            normalize::subtract_arm(&sub, &ContractTy::LitBool(false), &NoHierarchy),
            normalize::ArmFate::Dies,
        ));
    }

    #[test]
    fn known_unenforced_generics_lower_to_opaque_regardless_of_args() {
        let ty = lower_str("int-mask<1, 2, 4>").unwrap();
        assert_eq!(ty, ContractTy::Opaque);
        let ty = lower_str("int-mask-of<Permissions::*>").unwrap();
        assert_eq!(ty, ContractTy::Opaque);
        let ty = lower_str("properties-of<User>").unwrap();
        assert_eq!(ty, ContractTy::Opaque);
        let ty = lower_str("class-string-map<Foo, Bar>").unwrap();
        assert_eq!(ty, ContractTy::Opaque);
    }

    /// Pin: `int-mask<1, 2, 4>` admits an int as `Maybe`, not the `No` the
    /// old `Class("int-mask")` catch-all would have manufactured.
    #[test]
    fn int_mask_admits_an_int_as_maybe_not_no() {
        let ty = lower_str("int-mask<1, 2, 4>").unwrap();
        assert_eq!(admits_val(&ty, &Val::Int(5)), Certainty::Maybe);
    }

    /// The floor this fix must not touch: an unknown name still lowers to
    /// `Class`, the signal both lanes' class machinery depends on.
    #[test]
    fn a_genuinely_unknown_name_still_lowers_to_class() {
        assert_eq!(lower_identifier("TotallyUnknownFrobnicator"), ContractTy::Class("totallyunknownfrobnicator".to_owned()));
        assert!(matches!(
            lower_generic("SomeUnknownGeneric", &[]),
            ContractTy::Class(name) if name == "someunknowngeneric"
        ));
    }
}

/// Issue #240 (refined-string grid) and #238 (object intersections):
/// `ArrayAccess&stdClass` uses arms Steins already has, conjoined.
#[cfg(test)]
mod object_intersection_tests {
    use super::*;
    use crate::normalize::subsumes;

    fn inter(s: &str) -> ContractTy {
        lower_str(s).unwrap_or_else(|| panic!("{s} did not lower"))
    }

    /// Representable: lowers to a conjunction of class arms, intact and in
    /// order, normalized as a lone class arm is (lowercased, `\` stripped).
    #[test]
    fn an_object_intersection_is_representable() {
        let ty = inter("ArrayAccess&stdClass");
        assert_eq!(
            ty,
            ContractTy::Inter(vec![
                ContractTy::Class("arrayaccess".to_owned()),
                ContractTy::Class("stdclass".to_owned()),
            ])
        );
        assert_eq!(inter(r"\Foo\Bar&Baz"), inter(r"foo\bar&baz"));
    }

    /// Spellable: `spell_nested` joins arms with `&`, round-tripping the same
    /// conjunction. [`spell::spell_arms`] still refuses it, for the same
    /// reason it refuses a bare `Class` arm (no faithful *scalar* spelling).
    #[test]
    fn an_object_intersection_is_spellable() {
        let ty = inter("ArrayAccess&stdClass");
        assert_eq!(spell::spell_nested_for_test(&ty), "arrayaccess&stdclass");
        assert_eq!(lower_str(&spell::spell_nested_for_test(&ty)), Some(ty));
        // The scalar speller's refusal is the arms', not the conjunction's.
        let one = ContractTy::Class("stdclass".to_owned());
        assert_eq!(spell::spell_arms(std::slice::from_ref(&one)), None);
        assert_eq!(spell::spell_arms(&[inter("ArrayAccess&stdClass")]), None);
    }

    #[test]
    fn the_relation_judges_an_object_intersection_arm_wise() {
        let ab = inter("ArrayAccess&stdClass");
        let a = ContractTy::Class("arrayaccess".to_owned());

        // `A ⊇ A∩B` — proven: the intersection is a subset of each arm.
        assert!(subsumes(&a, &ab).is_yes(), "an arm covers the conjunction");
        // `A∩B ⊇ A` — NOT proven: a plain `ArrayAccess` need not be `stdClass`.
        assert_eq!(subsumes(&ab, &a), Certainty::Maybe);
        // Reflexivity: reached only via the arm-wise rule, folding to `Maybe`.
        assert!(subsumes(&ab, &ab).is_yes(), "a conjunction subsumes itself");
        // Order is not identity: equal in both directions either way spelled.
        let ba = inter("stdClass&ArrayAccess");
        assert!(subsumes(&ab, &ba).is_yes() && subsumes(&ba, &ab).is_yes());
    }

    /// A conjunction is never confused with a union: `A∩B ⊇ A∪B` must not be
    /// claimed, and a scalar never covers an object conjunction.
    #[test]
    fn the_relation_never_over_claims() {
        let ab = inter("ArrayAccess&stdClass");
        let union = inter("ArrayAccess|stdClass");
        assert!(!subsumes(&ab, &union).is_yes(), "a conjunction does not cover the union");
        assert!(subsumes(&union, &ab).is_yes(), "the union does cover the conjunction");
        // A scalar never *covers* an object conjunction — pinned: never `Yes`.
        assert!(!subsumes(&ContractTy::Base(Base::Int), &ab).is_yes());
        // `mixed` covers every object, conjoined or not.
        assert!(subsumes(&ContractTy::Mixed, &ab).is_yes());
    }
}

/// The array accessory fold (issue #238): PHPStan's `hasOffset` dialect
/// against the ADR-0062 vocabulary Steins already speaks. Strings here are
/// copied from nsrt rows, pinning the *translation*, not a constructed pair.
#[cfg(test)]
mod array_accessory_tests {
    use super::*;
    use crate::normalize::subsumes;

    /// The two lowerings denote the same set, proven in both directions —
    /// what earns an nsrt `equal`.
    #[track_caller]
    fn mutually_subsume(phpstan: &str, steins: &str) {
        let a = lower_str(phpstan).unwrap_or_else(|| panic!("{phpstan} did not lower"));
        let b = lower_str(steins).unwrap_or_else(|| panic!("{steins} did not lower"));
        assert!(subsumes(&a, &b).is_yes(), "{phpstan} ⊇ {steins} was not proven");
        assert!(subsumes(&b, &a).is_yes(), "{steins} ⊇ {phpstan} was not proven");
    }

    /// `hasOffset` states presence only, so the field carries the base's own value
    /// contract — `array-flip.php:74`, the row this fold was measured on.
    #[test]
    fn has_offset_is_a_required_key_at_the_base_value_type() {
        mutually_subsume(
            "non-empty-array<string, int>&hasOffset('foo')",
            "non-empty-array{foo: int, ...<string, int>}",
        );
    }

    /// `hasOffsetValue` states presence AND the value at the key
    /// (`unsealed-array-shapes.php:95`).
    #[test]
    fn has_offset_value_carries_the_value_contract() {
        mutually_subsume(
            "non-empty-array<int, string>&hasOffsetValue(1, 'foo')",
            "non-empty-array{1: 'foo', ...<int, string>}",
        );
    }

    /// A list base keeps its list-ness; the stacked form folds every predicate
    /// into one shape (`list-type.php:116`). Spelled `...<int>`, not
    /// `...<int, int>`: a list-shape tail has no key slot in the grammar.
    #[test]
    fn stacked_predicates_over_a_list_fold_to_one_shape() {
        mutually_subsume(
            "non-empty-list<int>&hasOffsetValue(0, 17)&hasOffsetValue(1, 19)",
            "non-empty-list{17, 19, ...<int>}",
        );
    }

    /// A bare `non-empty-array` base has no value contract to give the field, so
    /// the field is `mixed` and the tail stays untyped (`bug-11518-types.php:17`).
    #[test]
    fn a_bare_array_base_yields_a_mixed_field() {
        mutually_subsume("non-empty-array&hasOffset('thing')", "non-empty-array{thing: mixed, ...}");
    }

    /// The fold never invents an ungiven key, never attaches to a base
    /// ADR-0062 doesn't speak for, and never seals the tail — the honest
    /// floor, not a widening.
    #[test]
    fn the_refusals_keep_the_intersection() {
        // A class base.
        let ty = lower_str("ArrayObject<int, string>&hasOffset(1)").unwrap();
        assert!(matches!(ty, ContractTy::Inter(_)), "a class base must not fold: {ty:?}");
        // A non-literal key names no shape key.
        let ty = lower_str("non-empty-array<string, int>&hasOffset(string)").unwrap();
        assert!(matches!(ty, ContractTy::Inter(_)), "a non-literal key must not fold: {ty:?}");
        // Two non-accessory arms: no rule for which one to build from.
        let ty = lower_str("array<string, int>&Countable&hasOffset('a')").unwrap();
        assert!(matches!(ty, ContractTy::Inter(_)), "two bases must not fold: {ty:?}");
    }

    /// A predicate with no array arm to attach to lowers to `Opaque`, NOT a
    /// nonexistent class — `KNOWN_UNENFORCED`'s wrong-No hazard again.
    #[test]
    fn a_stray_predicate_is_opaque_never_a_class() {
        assert_eq!(lower_str("hasOffset('foo')"), Some(ContractTy::Opaque));
        assert_eq!(lower_str("hasOffsetValue('foo', 17)"), Some(ContractTy::Opaque));
        // …and Opaque is `Maybe` against an array, never `No`.
        let opaque = lower_str("hasOffset('foo')").unwrap();
        let arr = lower_str("array<string, int>").unwrap();
        assert!(!subsumes(&opaque, &arr).is_no(), "a stray predicate must never refute an array");
    }

    /// The tail stays OPEN: the predicates say which keys are present, never that
    /// they are all the keys there are. A sealed fold would claim the array has no
    /// other key — a claim PHPStan's spelling never makes.
    #[test]
    fn the_folded_tail_is_never_sealed() {
        let ty = lower_str("non-empty-array<string, int>&hasOffset('foo')").unwrap();
        let ContractTy::Shape { sealed, non_empty, fields, .. } = &ty else {
            panic!("expected a Shape, got {ty:?}")
        };
        assert!(!sealed, "the predicates never seal the tail");
        assert!(non_empty, "a required key forbids the empty array");
        assert_eq!(fields.len(), 1);
        assert!(!fields[0].optional, "hasOffset states presence, not optionality");
    }
}

#[cfg(test)]
mod refined_string_grid_tests {
    use super::*;
    use crate::spell::preds_keyword;

    /// Every cell, built from its own axes, not a copy of the speller's table.
    fn cells() -> Vec<StrPreds> {
        let cores = [
            StrPreds::empty(),
            StrPreds::NON_EMPTY,
            StrPreds::NON_FALSY,
            StrPreds::NUMERIC,
            StrPreds::NON_FALSY.union(StrPreds::NUMERIC),
        ];
        let casings = [
            StrPreds::empty(),
            StrPreds::LOWERCASE,
            StrPreds::UPPERCASE,
            StrPreds::LOWERCASE.union(StrPreds::UPPERCASE),
        ];
        cores
            .iter()
            .flat_map(|c| casings.iter().map(|k| c.union(*k)))
            .collect()
    }

    /// **The round trip**: every keyword [`preds_keyword`] can emit lowers
    /// back through [`lower_identifier`] to the set it was spelled from —
    /// what lets `spell` emit phpdoc Steins re-reads without loss.
    #[test]
    fn every_grid_cell_round_trips() {
        for preds in cells() {
            let kw = preds_keyword(preds);
            // The empty cell is the base type: `string` has always lowered to
            // `Base(String)`, which denotes exactly `StrWith` with no predicate.
            let expected = if preds.is_empty() {
                ContractTy::Base(Base::String)
            } else {
                ContractTy::StrWith(preds)
            };
            assert_eq!(
                lower_identifier(&kw),
                expected,
                "{kw} did not lower back to the set it spells"
            );
        }
    }

    /// The twenty cells are twenty distinct words — else the round trip above
    /// would be lossy in one direction.
    #[test]
    fn the_grid_is_injective() {
        let mut seen: Vec<String> = cells().iter().map(|p| preds_keyword(*p)).collect();
        seen.sort();
        let n = seen.len();
        seen.dedup();
        assert_eq!(seen.len(), n, "two cells share a spelling");
    }

    /// The spellings themselves, written out, including the two that predate
    /// the grid.
    #[test]
    fn the_named_cells() {
        let k = |p: StrPreds| preds_keyword(p);
        assert_eq!(k(StrPreds::empty()), "string");
        assert_eq!(k(StrPreds::LOWERCASE), "lowercase-string");
        assert_eq!(k(StrPreds::LOWERCASE.union(StrPreds::UPPERCASE)), "uncased-string");
        assert_eq!(k(StrPreds::NON_EMPTY.union(StrPreds::LOWERCASE)), "non-empty-lowercase-string");
        assert_eq!(k(StrPreds::NON_EMPTY.union(StrPreds::UPPERCASE)), "non-empty-uppercase-string");
        assert_eq!(k(StrPreds::NON_FALSY.union(StrPreds::LOWERCASE)), "non-falsy-lowercase-string");
        assert_eq!(k(StrPreds::NUMERIC.union(StrPreds::UPPERCASE)), "numeric-uppercase-string");
        assert_eq!(
            k(StrPreds::NON_FALSY.union(StrPreds::NUMERIC)),
            "non-falsy-numeric-string"
        );
        assert_eq!(
            k(StrPreds::NON_FALSY.union(StrPreds::NUMERIC).union(StrPreds::LOWERCASE)),
            "non-falsy-numeric-lowercase-string"
        );
    }

    /// The array-key-cast pair is still not an axis: `decimal-int-string`
    /// widens to the cell its closure names, never to a keyword of its own.
    #[test]
    fn the_array_key_cast_pair_is_still_not_a_rung() {
        assert_eq!(preds_keyword(StrPreds::DECIMAL_INT.close()), "numeric-uncased-string");
        assert_eq!(preds_keyword(StrPreds::NON_DECIMAL_INT), "string");
    }

    /// `class-string` outranks the grid — the contextual bit says something no
    /// character-level rung can (issue #236) — and round-trips as itself.
    #[test]
    fn the_contextual_bit_outranks_the_grid() {
        let cs = StrPreds::CLASS_STRING.close();
        assert_eq!(preds_keyword(cs), "class-string");
        assert_eq!(preds_keyword(cs.union(StrPreds::LOWERCASE)), "class-string");
        assert_eq!(lower_identifier("class-string"), ContractTy::StrWith(cs));
    }

    /// A name that merely ends in `-string` is not a cell: the class catch-all is
    /// load-bearing and must still claim it.
    #[test]
    fn a_non_cell_still_lowers_to_class() {
        assert_eq!(grid_str_preds("foo-string"), None);
        assert_eq!(grid_str_preds("string"), None);
        assert_eq!(grid_str_preds("non-decimal-int-string"), None);
        assert_eq!(
            lower_identifier("Non-Cell-String"),
            ContractTy::Class("non-cell-string".to_owned())
        );
        // …and the family members with their own arms keep them.
        assert_eq!(lower_identifier("literal-string"), ContractTy::StrOpaque);
        assert_eq!(
            lower_identifier("decimal-int-string"),
            ContractTy::StrWith(StrPreds::DECIMAL_INT.close())
        );
    }

    /// `A&B` over string refinements is the closed union of their bits, so a
    /// declared conjunction lowers to the same set a computed one would carry.
    #[test]
    fn an_all_string_intersection_folds_to_one_set() {
        let ty = lower_str("lowercase-string&non-empty-string").unwrap();
        assert_eq!(
            to_fact(&ty),
            Some(Fact::refined(
                Base::String,
                Refinement::Str(StrPreds::NON_EMPTY.union(StrPreds::LOWERCASE)),
                false
            ))
        );
        // Three arms, and the closure runs: `numeric` entails `non-empty`.
        let ty = lower_str("lowercase-string&numeric-string&uppercase-string").unwrap();
        let expected = StrPreds::NUMERIC
            .union(StrPreds::LOWERCASE)
            .union(StrPreds::UPPERCASE);
        assert_eq!(
            inter_str_preds(std::slice::from_ref(&ty)),
            None,
            "a non-StrWith member is not this fold's business"
        );
        let ContractTy::Inter(members) = &ty else { panic!("expected an Inter, got {ty:?}") };
        assert_eq!(inter_str_preds(members), Some(expected));
        assert!(expected.contains_all(StrPreds::NON_EMPTY));
    }

    /// Complementary bits need no special case: the fold builds the set
    /// denoting ∅, a legitimate `StrWith` (`StrPreds` module doc).
    #[test]
    fn complementary_arms_fold_to_the_empty_denotation() {
        let ty = lower_str("decimal-int-string&non-decimal-int-string").unwrap();
        let ContractTy::Inter(members) = &ty else { panic!("expected an Inter, got {ty:?}") };
        let folded = inter_str_preds(members).expect("both arms are StrWith");
        assert!(folded.contains_all(StrPreds::DECIMAL_INT));
        assert!(folded.contains_all(StrPreds::NON_DECIMAL_INT));
        assert_eq!(admits_val(&ContractTy::StrWith(folded), &Val::Str("1".into())), Certainty::No);
        assert_eq!(admits_val(&ContractTy::StrWith(folded), &Val::Str("x".into())), Certainty::No);
    }

    /// One non-`StrWith` arm refuses the whole intersection: an
    /// object/provenance conjunction stays in the arm lane alone.
    #[test]
    fn a_non_string_arm_refuses_the_fold() {
        for src in [
            "literal-string&non-falsy-string",
            "Foo&Bar",
            "non-empty-string&Countable",
            "int&object",
        ] {
            let ty = lower_str(src).unwrap();
            let ContractTy::Inter(members) = &ty else { panic!("{src} is not an Inter") };
            assert_eq!(inter_str_preds(members), None, "{src} must not fold");
            assert_eq!(to_fact(&ty), None, "{src} must seed no fact");
        }
        assert_eq!(inter_str_preds(&[]), None, "an empty intersection is not a string");
    }

    /// The contextual bit folds like any other — a value property (#236) —
    /// but membership queries keep reading it extensionally.
    #[test]
    fn a_class_string_arm_folds_and_stays_contextual() {
        let ty = lower_str("class-string&non-empty-string").unwrap();
        let ContractTy::Inter(members) = &ty else { panic!("expected an Inter, got {ty:?}") };
        let folded = inter_str_preds(members).expect("both arms are StrWith");
        assert!(folded.contains_all(StrPreds::CLASS_STRING));
        assert!(!folded.is_extensional(), "the class-table bit must still be contextual");
        // Undecidable from the characters alone: the honest `Maybe`, unchanged.
        assert_eq!(
            admits_val(&ContractTy::StrWith(folded), &Val::Str("Foo".into())),
            Certainty::Maybe
        );
        // …and refuted where the extensional half refutes it.
        assert_eq!(
            admits_val(&ContractTy::StrWith(folded), &Val::Str("".into())),
            Certainty::No
        );
    }
}

/// ADR-0062 S3 — the one lowering from the contract lane's array vocabulary to
/// the canonical [`ShapeFact`], and the value-slot lowering under it.
#[cfg(test)]
mod shape_fact_lowering_tests {
    use super::*;
    use steins_domain::Tail;

    fn shape_of(src: &str) -> ShapeFact {
        let ty = lower_str(src).unwrap_or_else(|| panic!("{src} failed to lower"));
        to_shape_fact(&ty).unwrap_or_else(|| panic!("{src} is not an array truth"))
    }

    fn fact_of(src: &str) -> Option<Fact> {
        to_fact(&lower_str(src).unwrap_or_else(|| panic!("{src} failed to lower")))
    }

    fn slot(s: &ShapeFact, key: &str) -> Option<Fact> {
        s.field(&DKey::Str(key.into())).and_then(|(_, _, v)| v.clone()).map(|b| *b)
    }

    // ---- the four degenerate forms (A-G1) ---------------------------------

    #[test]
    fn plain_array_lowers_to_the_degenerate_shape() {
        assert_eq!(shape_of("array"), ShapeFact::plain_array());
        let ne = shape_of("non-empty-array");
        assert!(ne.non_empty);
        assert!(ne.fields.is_empty());
    }

    #[test]
    fn list_of_types_the_tail_and_pins_is_list_yes() {
        let s = shape_of("list<string>");
        assert_eq!(s.is_list, Certainty::Yes);
        assert!(s.fields.is_empty());
        assert_eq!(
            s.tail,
            Tail::Unsealed {
                key: KeyClass::Int,
                value: Some(Box::new(Fact::General { base: Base::String, nullable: false })),
            }
        );
        assert!(shape_of("non-empty-list<int>").non_empty);
    }

    #[test]
    fn map_of_types_the_tail_key_class_and_value() {
        let s = shape_of("array<string, int>");
        assert_eq!(s.is_list, Certainty::Maybe);
        assert_eq!(
            s.tail,
            Tail::Unsealed {
                key: KeyClass::Str,
                value: Some(Box::new(Fact::General { base: Base::Int, nullable: false })),
            }
        );
        // A key contract narrower than a bare base floors to `array-key`.
        assert!(matches!(
            shape_of("array<positive-int, int>").tail,
            Tail::Unsealed { key: KeyClass::ArrayKey, .. }
        ));
    }

    #[test]
    fn declared_shape_carries_presence_at_the_declared_stratum() {
        let s = shape_of("array{a: string, b?: int}");
        assert_eq!(
            s.field(&DKey::Str("a".into())).map(|(_, p, _)| *p),
            Some(DPresence::Required { witnessed: false })
        );
        assert_eq!(
            s.field(&DKey::Str("b".into())).map(|(_, p, _)| *p),
            Some(DPresence::Optional)
        );
        assert_eq!(s.tail, Tail::Sealed);
        assert!(s.non_empty, "a required field implies non-emptiness");
    }

    #[test]
    fn declared_unsealed_tail_lowers_key_class_and_value() {
        assert_eq!(
            shape_of("array{a: int, ...<string, int>}").tail,
            Tail::Unsealed {
                key: KeyClass::Str,
                value: Some(Box::new(Fact::General { base: Base::Int, nullable: false })),
            }
        );
        assert_eq!(
            shape_of("array{a: int, ...}").tail,
            Tail::Unsealed { key: KeyClass::ArrayKey, value: None }
        );
    }

    #[test]
    fn a_non_array_contract_states_no_array_truth() {
        for src in ["int", "string|null", "Foo", "callable", "iterable<int>", "mixed", "object"] {
            let ty = lower_str(src).unwrap_or_else(|| panic!("{src} failed to lower"));
            assert!(to_shape_fact(&ty).is_none(), "{src} lowered to a shape fact");
        }
    }

    #[test]
    fn the_lowered_shape_admits_what_the_declaration_admits() {
        let s = shape_of("array{a: string, b?: int}");
        let a = |v: Vec<(DKey, Val)>| v;
        assert!(s.admits(&a(vec![(DKey::Str("a".into()), Val::Str("x".into()))])));
        assert!(s.admits(&a(vec![
            (DKey::Str("a".into()), Val::Str("x".into())),
            (DKey::Str("b".into()), Val::Int(1)),
        ])));
        // missing required key / sealed-undeclared key / wrong value type
        assert!(!s.admits(&a(vec![(DKey::Str("b".into()), Val::Int(1))])));
        assert!(!s.admits(&a(vec![
            (DKey::Str("a".into()), Val::Str("x".into())),
            (DKey::Str("z".into()), Val::Int(1)),
        ])));
        assert!(!s.admits(&a(vec![(DKey::Str("a".into()), Val::Int(1))])));
    }

    // ---- value slots (A-G1a) ----------------------------------------------

    #[test]
    fn value_slots_lower_bases_refinements_and_literals() {
        let s = shape_of("array{a: string, b: positive-int, c: 'x', d: non-empty-string}");
        assert_eq!(slot(&s, "a"), Some(Fact::General { base: Base::String, nullable: false }));
        assert_eq!(
            slot(&s, "b"),
            Some(Fact::refined(Base::Int, Refinement::Int(IntRange::POSITIVE), false))
        );
        assert_eq!(slot(&s, "c"), Some(Fact::Singleton(Val::Str("x".into()))));
        assert_eq!(
            slot(&s, "d"),
            Some(Fact::refined(Base::String, Refinement::Str(StrPreds::NON_EMPTY), false))
        );
    }

    #[test]
    fn a_null_bearing_union_slot_lowers_to_nullability() {
        let s = shape_of("array{a: ?int, b: 'x'|'y'}");
        assert_eq!(slot(&s, "a"), Some(Fact::General { base: Base::Int, nullable: true }));
        assert_eq!(
            slot(&s, "b"),
            Some(Fact::OneOf(vec![Val::Str("x".into()), Val::Str("y".into())]))
        );
    }

    #[test]
    fn a_nested_array_slot_recurses() {
        let s = shape_of("array{a: list<int>}");
        let Some(Fact::Shape { shape, nullable }) = slot(&s, "a") else {
            panic!("nested array slot did not recurse");
        };
        assert!(!nullable);
        assert_eq!(shape.is_list, Certainty::Yes);
    }

    #[test]
    fn unrepresentable_slots_floor_to_unknown() {
        // Classes, callables, `mixed` and the int-accepting `float` floor —
        // the honest `None` (A-G1a).
        let s = shape_of(
            "array{a: Foo, b: callable, c: mixed, d: int|string, e: float, f: literal-string}",
        );
        for key in ["a", "b", "c", "e", "f"] {
            assert_eq!(slot(&s, key), None, "slot {key} should floor to unknown");
        }
        // …but a scalar UNION no longer floors (issue #339): the value domain
        // now has a two-base form, so the slot carries the union.
        assert_eq!(
            slot(&s, "d"),
            Fact::union(vec![(Base::Int, None), (Base::String, None)], false)
        );
        // …nor does `class-string` (issue #236): a string refinement now,
        // it carries the predicate.
        let cs = shape_of("array{f: class-string}");
        assert_eq!(
            slot(&cs, "f"),
            Some(Fact::refined(
                Base::String,
                Refinement::Str(StrPreds::CLASS_STRING.close()),
                false
            ))
        );
    }

    #[test]
    fn to_fact_on_scalars_matches_the_slot_rules() {
        assert_eq!(fact_of("int"), Some(Fact::General { base: Base::Int, nullable: false }));
        assert_eq!(fact_of("?string"), Some(Fact::General { base: Base::String, nullable: true }));
        assert_eq!(fact_of("5"), Some(Fact::Singleton(Val::Int(5))));
        // Scalar unions lower into the value lane as of issue #339; `float`
        // still floors (it accepts an int, so the base alone isn't acceptance).
        assert_eq!(
            fact_of("int|string"),
            Fact::union(vec![(Base::Int, None), (Base::String, None)], false)
        );
        assert_eq!(fact_of("float"), None);
    }

    // ---- count_range through the lowering (ADR-0062 §4) -------------------

    #[test]
    fn count_range_of_lowered_declarations() {
        assert_eq!(shape_of("array{x: int, y: int}").count_range(), IntRange::point(2));
        assert_eq!(
            shape_of("array{a: string, b?: int}").count_range(),
            IntRange::new(1, 2).expect("ordered")
        );
        assert_eq!(shape_of("list<string>").count_range(), IntRange::NON_NEGATIVE);
        assert_eq!(shape_of("non-empty-list<string>").count_range(), IntRange::POSITIVE);
    }
}

#[cfg(test)]
mod unset_pseudo_type_tests {
    use super::*;
    use steins_domain::Val;

    /// The spelling is vocabulary, not a class in the current namespace — the
    /// one plainly wrong reading (zonuexe/php-typing-conformance#7, ADR-0087).
    #[test]
    fn the_spelling_is_never_a_class() {
        for name in ["unset", "UNSET", "Unset", "\\unset"] {
            assert_eq!(
                lower_identifier(name),
                ContractTy::Unset,
                "{name} should lower to the unset pseudo-type",
            );
        }
    }

    /// `unset` is not any of the four words it is easy to mistake it for: not
    /// the null type, not `void`'s opaque floor, not `never`'s empty
    /// denotation, and not `mixed`.
    #[test]
    fn the_pseudo_type_is_none_of_the_neighbouring_words() {
        let unset = lower_str("unset").expect("lowers");
        for other in ["null", "void", "never", "mixed"] {
            assert_ne!(
                unset,
                lower_str(other).expect("lowers"),
                "unset must not lower like {other}",
            );
        }
    }

    /// A reserved language construct (`unset()`), so no class in scope shadows
    /// it — unlike the phpdoc pseudo-types (`integer`, `number`, `closure`).
    #[test]
    fn the_pseudo_type_is_not_shadowable() {
        assert!(!is_shadowable_pseudo_type("unset"));
        assert!(!is_shadowable_pseudo_type("UNSET"));
        // The control: a spelling that IS a legal class name, hence shadowable.
        assert!(is_shadowable_pseudo_type("integer"));
    }

    /// **The acceptance criterion**: the member contributes no value, so the
    /// union carries its neighbours plus the marker — and once the marker is
    /// filtered (what every arm-list builder does with [`ContractTy::is_unset`])
    /// what remains is *structurally* the lowering of the union without it.
    #[test]
    fn the_member_contributes_no_value_to_the_union() {
        for (with, without) in [
            ("\\DateTime|unset", "\\DateTime"),
            ("int|unset", "int"),
            ("\\DateTime|null|unset", "\\DateTime|null"),
            ("unset|int", "int"),
        ] {
            let ContractTy::Union(members) = lower_str(with).expect("lowers") else {
                panic!("{with} should lower to a union")
            };
            let kept: Vec<ContractTy> = members.into_iter().filter(|m| !m.is_unset()).collect();
            let expected = match lower_str(without).expect("lowers") {
                ContractTy::Union(m) => m,
                one => vec![one],
            };
            assert_eq!(kept, expected, "{with} must carry exactly {without}'s value arms");
        }
    }

    /// No `Class("unset")` arm survives anywhere in the lowering — the defect
    /// this slice removes, stated as its own assertion.
    #[test]
    fn no_phantom_class_arm_is_invented() {
        for src in ["unset", "\\DateTime|unset", "int|null|unset", "array<int, unset>"] {
            let lowered = lower_str(src).expect("lowers");
            assert!(
                !mentions_unset_class(&lowered),
                "{src} lowered to a phantom class: {lowered:?}",
            );
        }
    }

    fn mentions_unset_class(ty: &ContractTy) -> bool {
        match ty {
            ContractTy::Class(n) => n == "unset",
            ContractTy::Union(m) | ContractTy::Inter(m) => m.iter().any(mentions_unset_class),
            ContractTy::MapOf { key, val, .. } | ContractTy::IterableOf { key, val } => {
                mentions_unset_class(key) || mentions_unset_class(val)
            }
            ContractTy::ListOf { elem, .. } => mentions_unset_class(elem),
            ContractTy::Shape { fields, unsealed, .. } => {
                fields.iter().any(|f| mentions_unset_class(&f.ty))
                    || unsealed.as_ref().is_some_and(|(k, v)| {
                        k.as_deref().is_some_and(mentions_unset_class)
                            || mentions_unset_class(v)
                    })
            }
            _ => false,
        }
    }

    /// The leaf decides nothing about a value (`Maybe`), so a bare
    /// `@var unset $x` convicts nothing. `Never`'s `No` would convict every
    /// value the variable holds.
    #[test]
    fn the_leaf_decides_nothing_about_a_value() {
        for v in [Val::Null, Val::Int(1), Val::Bool(false), Val::Str(PhpStr::from("x"))] {
            assert_eq!(
                admits_val(&ContractTy::Unset, &v),
                Certainty::Maybe,
                "unset must stay undecided about {v:?}",
            );
        }
    }

    /// It states no value-slot fact either — a union carrying it stops lowering
    /// to one fact, which only ever widens.
    #[test]
    fn the_leaf_states_no_fact() {
        assert_eq!(to_fact(&ContractTy::Unset), None);
        assert_eq!(to_shape_fact(&ContractTy::Unset), None);
    }

    /// **The round trip** — the reason for a dedicated variant: the word
    /// survives spelling, so lower → spell → lower is the identity. Parked on
    /// [`ContractTy::Opaque`] it would have spelled back as `mixed`.
    #[test]
    fn the_spelling_round_trips() {
        for (src, spelled) in [
            ("\\DateTime|unset", "datetime|unset"),
            ("int|unset", "int|unset"),
            ("unset", "unset"),
        ] {
            let lowered = lower_str(src).expect("lowers");
            let back = spell::spell_nested_for_test(&lowered);
            assert_eq!(back, spelled, "{src} spelled back wrong");
            assert_eq!(
                lower_str(&back).expect("re-lowers"),
                lowered,
                "{src} did not round-trip through {back}",
            );
        }
    }
}

/// ADR-0089's **vocabulary** half: what the operator names are, and what every
/// spelling that is not the operator's own arity does. The *projections* are
/// judged end to end in `tests/end_to_end.rs`, against real values.
#[cfg(test)]
mod derived_operator_tests {
    use super::*;

    /// The naming rule's whole point. A kebab-case operator is non-shadowable
    /// **by construction** — it is not a legal PHP identifier, so no class can
    /// carry the name — and that needs no entry in
    /// [`is_shadowable_pseudo_type`]'s reserved-word list, which would have
    /// been a claim about PHP that PHP does not make.
    ///
    /// The lowercase spelling the operators were first proposed in could not
    /// have done this: PHP class names are case-insensitive, so `partial` and
    /// a project's `class Partial` are one name, and the precedence rule
    /// resolves that collision in the class's favour.
    #[test]
    fn every_operator_is_non_shadowable_without_a_reserved_word() {
        for name in DERIVED_OPERATORS {
            assert!(
                !is_shadowable_pseudo_type(name),
                "{name} must be non-shadowable — the hyphen rule, not a reserved word",
            );
            assert!(name.contains('-'), "{name} must be kebab-case (ADR-0089 §2)");
        }
        // The control: a single-word pseudo-type IS a legal class name.
        assert!(is_shadowable_pseudo_type("integer"));
    }

    /// An operator spelled bare names an operator with no operand, so it
    /// states nothing. `Opaque` is the floor; the class catch-all is not.
    #[test]
    fn a_bare_operator_spelling_floors() {
        for name in DERIVED_OPERATORS {
            let lowered = lower_identifier(name);
            assert_eq!(lowered, ContractTy::Opaque, "bare {name} must floor to Opaque");
        }
    }

    /// **The arity-blind floor** (ADR-0089 §4), and a regression: before this
    /// slice `key-of<int, int>` lowered to `Class("key-of")`, whose acceptance
    /// leg answers a definite `No` for every non-object value — a false
    /// positive, since the closure-argument variance check raises findings on
    /// `No`. A misspelled arity is now silent instead of wrong.
    #[test]
    fn every_wrong_arity_floors_and_is_never_a_class() {
        for src in [
            "key-of<int, int>",
            "value-of<int, int>",
            "non-nullable<int, int>",
            "return-type<int, int>",
            "parameters-of<int, int>",
            "exclude-from<int>",
            "extract-from<int>",
            "exclude-from<int, int, int>",
            "key-of<int, int, int>",
        ] {
            let lowered = lower_str(src).unwrap_or_else(|| panic!("{src} must lower"));
            assert_eq!(lowered, ContractTy::Opaque, "{src} must floor to Opaque");
        }
    }

    /// The refusals of ADR-0089 §6 are refusals, not omissions: they are
    /// **not** vocabulary and keep the ordinary class reading, which is the
    /// behaviour that was already there. Stated as an assertion so a later
    /// reader does not mistake the absence for an oversight.
    #[test]
    fn the_refused_utility_types_are_not_vocabulary() {
        for src in [
            "record<string, int>",
            "readonly<int>",
            "instance-type<int>",
            "no-infer<int>",
            "awaited<int>",
            "this-type<int>",
            "this-parameter-type<int>",
            "omit-this-parameter<int>",
        ] {
            let lowered = lower_str(src).unwrap_or_else(|| panic!("{src} must lower"));
            assert!(
                matches!(lowered, ContractTy::Class(_)),
                "{src} is refused vocabulary and must stay a class name, got {lowered:?}",
            );
        }
    }

    /// The two tables are disjoint and mean different things: a
    /// `KNOWN_UNENFORCED` name has **no** relation here, a `DERIVED_OPERATORS`
    /// name has one at exactly one arity. Putting an operator in the former
    /// would floor it before [`lower_generic`] could project it.
    #[test]
    fn the_two_vocabulary_tables_do_not_overlap() {
        for name in DERIVED_OPERATORS {
            assert!(
                !KNOWN_UNENFORCED.contains(name),
                "{name} has a relation, so it must not be KNOWN_UNENFORCED",
            );
        }
    }
}
