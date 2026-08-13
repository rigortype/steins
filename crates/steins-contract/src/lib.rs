//! Contract acceptance (ADR-0030 relation #1): phpdoc types × the value
//! domain, judged in the unified `Certainty`.
//!
//! This crate is the bridge between `steins-phpdoc`'s *syntactic* type AST
//! and `steins-domain`'s facts. Lowering normalizes keywords into a small
//! semantic [`ContractTy`] (e.g. `scalar` becomes the union of the four
//! bases, `positive-int` an interval, `numeric-string` a predicate set), so
//! acceptance is Kleene composition over a handful of leaf rules instead of
//! a keyword zoo.
//!
//! Trinary discipline: `Maybe` is the answer wherever membership is not
//! decided — notably every construct lowered to [`ContractTy::Opaque`]
//! (conditionals, templates, const fetches, `$this`, …) and every
//! provenance-flavored string type (`class-string`, `literal-string` —
//! non-extensional per ADR-0038, so they can never decide `Yes`).

mod admit;
pub mod normalize;
pub mod spell;

pub use admit::{ShapeSpec, admits_fact, admits_val, shape_verdict};

use steins_domain::{
    Base, Certainty, Fact, IntRange, KeyClass, Key as DKey, PhpStr, Presence as DPresence,
    Refinement, ShapeFact, StrPreds, Tail as DTail, Val,
};
use steins_phpdoc::ast::{ArrayShapeKind, ConstExpr, ShapeKey, StringLit, Type, TypeKind};

/// A lowered `callable(P1, P2=): R` signature (issue #11): the parameter
/// contracts (with optionality/variadic/by-ref markers as the phpdoc grammar
/// provides) and the return contract. A template-bearing signature
/// (`callable(T): T`) is never lowered to this — it drops to a bare
/// `CallableTy(None)` (ADR-0032/0051: no call-site template solver), so every
/// arm here is a ground contract type.
#[derive(Debug, Clone, PartialEq)]
pub struct CallableSig {
    /// The declared parameters, in source order.
    pub params: Vec<CallableParamTy>,
    /// The declared return contract.
    pub ret: ContractTy,
}

/// The obligations a **refined** callable spelling puts on the callable that is
/// bound to it (ADR-0063 §2 decision 4). All three are `false` for a plain
/// `callable`/`Closure`, which is why [`Default`] is the bare callable.
///
/// The three flags are orthogonal and the vocabulary composes them: `pure-callable`
/// is `pure`, `pure-closure` is `pure + closure_only`, `static-closure` is
/// `is_static + closure_only`, `static-pure-closure` is all three.
///
/// Only [`Self::closure_only`] is a *value-domain* obligation (a string or array is
/// never a `Closure` instance, so [`admits_val`]/[`admits_fact`] can decide it).
/// [`Self::pure`] and [`Self::is_static`] are properties of the bound callable's
/// **definition**, not of any runtime value this crate can see, so they are judged
/// where the definition is in scope — the closure-argument check in `steins-infer`
/// — exactly like [`CallableSig`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CallableObl {
    /// `pure-callable` / `pure-closure` / `static-pure-closure`: the bound
    /// callable's **inferred effect envelope** must be pure (ADR-0055 semantics).
    /// Judged against the effect fixpoint, never against a declaration flag —
    /// the metadata-only purity flag is the import ADR-0063 §3 declines.
    pub pure: bool,
    /// `static-closure` / `static-pure-closure`: the bound closure must carry the
    /// `static` keyword. A syntactic fact, so this is a mechanical binding check.
    pub is_static: bool,
    /// The `*-closure` spellings: the value must be a `Closure` **instance**. A
    /// callable-string or callable-array fails this half without any purity
    /// analysis — the two halves of `pure-closure` fail independently.
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

/// The obligations named by a callable **identifier**, or `None` when the
/// identifier is not a callable spelling at all. Shared by the bare-identifier
/// lowering and the parenthesized-signature lowering, so `pure-callable(int): int`
/// carries the same obligation as bare `pure-callable`.
///
/// `callable-object` is deliberately *not* `closure_only`: it means "an object that
/// is callable" (any `__invoke`), which is wider than `Closure`. Bare `Closure` is
/// likewise obligation-free — tightening it is a separate, wider change than the
/// refined spellings ADR-0063 P3 names.
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
    /// PHPStan's `non-empty-mixed` (`MixedType` with `StaticTypeFactory::falsey()`
    /// subtracted). Subsumes [`MixedCut::Null`].
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
    /// A **negative** leaf, and the only one — every other variant here states
    /// what a value must *be*. It exists because neither spelling is a union of
    /// the forms above: there is no `Val::Object` to put in a union (so
    /// "anything but null" cannot be enumerated), and no float refinement (so
    /// "float minus `0.0`" cannot be spelled either). Judging the cut against a
    /// concrete value is exact — [`steins_domain::php_is_falsy`] *is* the
    /// definition; judging it against an abstract fact decides only where the
    /// fact's own refinement already answers (a string knowing `non-falsy`, an
    /// int range missing zero), and answers `Maybe` otherwise.
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
    /// `class-string` and kin left this variant with issue #236 — a class-like
    /// name is a *value* property, so it is a [`StrPreds`] predicate. What is
    /// still here is genuinely undecidable from the value: `literal-string` is
    /// provenance, and `callable-string` names a function table this crate has
    /// no view of.
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
        /// Phan's `associative-array` refinement: reject a realization whose keys
        /// happen to be a list (`0..n-1` in order). Seeds the shape fact's
        /// `is_list` at `No` instead of `Maybe` (`to_shape_fact`) — the same
        /// denotational trinary ADR-0062 landed for `list<T>`'s `Yes` seed, run
        /// in the other direction.
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
    /// The `object` keyword and object shapes.
    ObjectAny,
    /// `callable` and callable signatures: strings and arrays are `Maybe`
    /// (a string may name a function, a pair-array a method), other
    /// scalars `No`.
    ///
    /// `None` is a bare `callable`/`Closure` (no parenthesized signature) —
    /// it accepts any callable. `Some(sig)` carries a declared
    /// `callable(P1, P2=): R` signature (issue #11): the parameter contracts
    /// and the return contract, against which a bound closure/first-class
    /// callable is judged arm-wise (contravariant params, covariant return).
    /// Value/fact acceptance ([`admits_val`]/[`admits_fact`]) ignores the
    /// signature — a runtime string/array value cannot be judged against a
    /// call shape — so the signature is consumed only by the closure-argument
    /// variance check in `steins-infer`.
    ///
    /// `obl` carries the refined spellings' obligations (ADR-0063 P3):
    /// `pure-callable`, `pure-closure`, `static-closure`, `static-pure-closure`.
    /// A [`CallableObl::is_bare`] obligation is exactly the plain
    /// `callable`/`Closure` behavior, so every bare-callable consumer is unaffected.
    CallableTy { sig: Option<Box<CallableSig>>, obl: CallableObl },
    /// Union.
    Union(Vec<ContractTy>),
    /// Intersection.
    Inter(Vec<ContractTy>),
    /// Anything not modeled: conditionals, offset access, const fetches,
    /// `$this`/`self`/`static`, templates. Always `Maybe`.
    Opaque,
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
/// key of this shape is the honest floor a single-arg `array<V>`/`T[]`
/// lowers to (`lower`/`lower_generic`, above) — the speller collapses it back
/// to the terser single-arg spelling rather than the verbose
/// `array<int|string, V>` (round-trip faithful either way; terser is nicer).
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

/// Type-operator/pseudo-type spellings this crate **recognizes as vocabulary**
/// but does not (yet) model any relation for — `int-mask<...>`, `resource`,
/// Psalm's `properties-of<T>`, … — checked by both catch-alls below (the
/// identifier table and the generic table share one normalized-name space, so
/// one list serves both).
///
/// Without this list, each of these names would fall through the catch-all to
/// [`ContractTy::Class`] — a **nonexistent-class reference**, which is a hazard,
/// not mere silence: the class leg of acceptance answers a definite `No` for any
/// non-object value (`admits_val`/`accepts_class_name`), so `@param resource $h`
/// would report a false positive on every scalar/array argument the checker
/// could resolve. This is the same wrong-No hazard handled for
/// `key-of`/`value-of` (ADR-0062), applied to the names PHPStan's own
/// curation corpus already documents as "falls back to a nonexistent-class
/// reference" (`php-typing-conformance/conformance/results/steins/*.toml`,
/// `status` field) — Steins does not model these constructs, so the honest
/// floor is [`ContractTy::Opaque`] (always `Maybe`), never a manufactured `No`.
///
/// This is deliberately **not** the same thing as "any unrecognized name" — an
/// unknown identifier must still fall through to `Class` (see the catch-alls'
/// own docs): "not a keyword" is the load-bearing signal both lanes' class
/// machinery depends on. This list exists only for spellings Steins *knows* are
/// pseudo-types/type-operators it does not enforce, so their fallback is honest
/// rather than a coincidence of an unmodeled name looking like a class.
const KNOWN_UNENFORCED: &[&str] = &[
    // PHPStan's array accessory predicates (issue #238). Inside an intersection
    // beside an array arm they are *folded into* the array vocabulary by
    // [`fold_array_accessories`] and never reach this table. Standing alone —
    // `@param hasOffset('foo') $a`, or beside a non-array arm such as
    // `ArrayObject<…>&hasOffset(1)` — they have nothing to attach to, and this
    // entry is what keeps that case honest: without it `hasoffset` is not a
    // keyword, so the catch-all would mint `Class("hasoffset")` and the class leg
    // of acceptance would answer a definite `No` for every array argument the
    // checker could resolve. Exactly the wrong-No hazard this list exists for.
    "hasoffset",
    "hasoffsetvalue",
    "int-mask",
    "int-mask-of",
    "resource",
    "open-resource",
    "closed-resource",
    "non-empty-literal-string",
    "arraylike-object",
    "properties-of",
    "stringable-object",
    "class-string-map",
];

/// **The one identifier table**: what every phpdoc *keyword* spelled as a bare
/// identifier means, lowered to a [`ContractTy`]. Both lanes read this table and
/// neither keeps a sibling — ADR-0030's no-second-relation discipline applied to
/// atoms, exactly as [`shape_verdict`] applies it to shapes (ADR-0062 §5): one
/// relation, lane-local leaf judges. The fact lane arrives here through [`lower`];
/// `steins-infer`'s proven-value lane calls it directly and judges the result with
/// [`admits_val`].
///
/// The catch-all is load-bearing, not a fallback: a name that is **not** a keyword
/// lowers to [`ContractTy::Class`], which is each lane's signal to hand the name to
/// its own class machinery (the trinary is-a oracle and the `is_known_class` gate
/// in `steins-infer`) — the one identifier judgment this crate cannot host, since
/// the value domain has no object inhabitant (ADR-0035/0038). `KNOWN_UNENFORCED`
/// is checked first: those names ARE keywords (deliberately unmodeled ones), so
/// they must not reach the class machinery at all.
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
        "scalar" => scalar(),
        "array-key" => array_key(),
        // The three subtraction spellings (census bucket x). `non-null-mixed`
        // is Phan's; `non-empty-mixed` is PHPStan's `mixed` with the falsy
        // values removed; `non-empty-scalar` is the same cut intersected with
        // `scalar` — one cut, reused, rather than three hand-written value sets.
        "non-null-mixed" => ContractTy::MixedMinus(MixedCut::Null),
        "non-empty-mixed" => ContractTy::MixedMinus(MixedCut::Falsy),
        // PHPStan resolves this to `float|int<min, -1>|int<1, max>|
        // non-falsy-string|true` and so stays silent on `0`/`0.0` — its `float`
        // member is never narrowed and an int is accepted wherever a float is
        // expected, which lets both falsy numbers back in through the side door.
        // Steins spells the subtraction itself instead, so `0` and `0.0` are
        // rejected with the other three. Deliberate, and within the fixture's
        // `E?` latitude (it names the PHPStan behaviour as an artifact of that
        // widening, not as the semantics of the keyword).
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
        // and every core × casing compound — is [`grid_str_preds`], reached from the
        // catch-all below. It is the inverse of the speller's own grid, in one place,
        // rather than twenty arms here that could drift from it.)
        // The array-key-cast pair. `decimal-int-string` is the string PHP writes
        // an integer back as, so it is cast to `int` as an array key; the
        // `non-` form is its complement *within string*, which is wider than
        // the name suggests — `'+1'`, `'00'`, `'18E+3'`, `'1.2'` and `'foo'`
        // all keep their string identity, so all qualify. Two bits rather than
        // one bit and a negation: the predicate set is a conjunction over
        // positive literals, and the set that carries both denotes ∅
        // (`StrPreds`'s module doc, and the ceiling noted on `admits_fact`).
        "decimal-int-string" => ContractTy::StrWith(StrPreds::DECIMAL_INT.close()),
        "non-decimal-int-string" => ContractTy::StrWith(StrPreds::NON_DECIMAL_INT),
        // The class-like family (issue #236). All four name the SAME predicate —
        // "this string names a class-like" — because PHP has one symbol table
        // for classes, interfaces, traits and enums, and PHPStan renders every
        // one of them back as `class-string`. It is a *value* property, so
        // ADR-0038's provenance bar does not reach it: two identical strings are
        // both class-strings or neither. What it needs is the class table, which
        // `StrPreds::of` has not got — hence the contextual bit, and hence the
        // `Maybe` the acceptance relation still answers for a concrete string.
        "class-string" | "interface-string" | "enum-string" | "trait-string" => {
            ContractTy::StrWith(StrPreds::CLASS_STRING.close())
        }
        // Genuinely non-extensional, and staying so: `literal-string` is
        // provenance (ADR-0038), `callable-string` is a property of a function
        // table this crate carries no view of, and `numeric-int-string` is
        // Phan-only vocabulary with no predicate here.
        "literal-string" | "callable-string" | "numeric-int-string" => ContractTy::StrOpaque,
        "positive-int" => ContractTy::IntIn(IntRange::POSITIVE),
        "negative-int" => ContractTy::IntIn(IntRange::NEGATIVE),
        "non-negative-int" => ContractTy::IntIn(IntRange::NON_NEGATIVE),
        "non-positive-int" => {
            ContractTy::IntIn(IntRange::new(i64::MIN, 0).expect("valid range"))
        }
        // The one sign refinement that is not a single interval: a union with the
        // hole punched at zero, which is the whole point of the spelling. Flattening
        // it back to one range would lose the hole (PHPStan resolves it the same
        // way: `int<min, -1>|int<1, max>`).
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
        "self" | "static" | "parent" | "key-of" | "value-of" => ContractTy::Opaque,
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
/// This is the exact inverse of [`spell::preds_keyword`], and it is a *parse*
/// rather than twenty arms in [`lower_identifier`] for the reason ADR-0030 gives
/// the vocabulary in general: the grid is one closed decision, so it should have
/// one implementation on each side and a round-trip test between them
/// (`every_grid_cell_round_trips`), not a table that can drift a cell at a time.
///
/// The casing axis is an *identity* under the case function, not "made of
/// lowercase letters": `strtolower($s) === $s`, so an uncased string (`''`,
/// `'123'`) satisfies both halves at once — which is what `uncased-` names, and
/// why it is Steins' own word for the set PHPStan spells
/// `lowercase-string&uppercase-string` (ADR-0030: the vocabulary is Steins',
/// and the intersection would not round-trip through a single identifier).
/// The length half is genuinely orthogonal to it (`''` fails only `non-empty-`,
/// `'ABC'` only the casing half).
///
/// `non-falsy-numeric-` is a core rung and not a compound of two, because
/// `NUMERIC` does not entail `NON_FALSY` (`'0'` and `'0.0'` are numeric and
/// falsy) — so the set really is tighter than either, and PHPStan spells it
/// `non-falsy-string&numeric-string`.
///
/// The array-key-cast pair keeps its own arms above and is deliberately not an
/// axis here — see [`spell::preds_keyword`] for why neither may become a rung.
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

/// Whether a same-named class in scope takes precedence over this keyword — the
/// vocabulary half of the pseudo-type/class precedence rule (PHPStan's
/// `TypeNodeResolver::tryResolvePseudoTypeClassType`).
///
/// PHP **reserves** its native type words — `int`, `float`, `string`, `bool`,
/// `true`, `false`, `null`, `mixed`, `never`, `void`, `iterable`, `object`,
/// `callable`, `array`, `static`, `self`, `parent` — so no class can be declared
/// with one of those names and the keyword always wins. Every other spelling
/// [`lower_identifier`] knows is a phpdoc **pseudo-type**: `integer`, `boolean`,
/// `double`, `number`, `numeric`, `scalar`, `closure`, … are all legal class names,
/// so a class named `Integer` in scope makes `@param Integer` *that class*, not
/// `int`. A hyphenated keyword (`positive-int`, `non-empty-string`) is not a legal
/// PHP identifier at all, so nothing can shadow it.
///
/// This is the whole of the rule this crate can answer. The *precedence* half needs
/// a class registry to ask whether such a class is actually in scope, and that
/// lives in `steins-infer` — so a caller pairs this predicate with its own
/// class-lookup gate.
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
    )
}

/// **The one generic table**: the parameterized phpdoc vocabulary, lowered to a
/// [`ContractTy`]. The companion of [`lower_identifier`], and public for the same
/// reason — `steins-infer`'s proven-value lane reads it rather than restating the
/// bound grammar or the recognized base names. Its catch-all carries the same
/// meaning: a base name that is not vocabulary lowers to [`ContractTy::Class`], the
/// signal to hand it to the caller's class-generic machinery — except a
/// `KNOWN_UNENFORCED` base name (`int-mask<...>`, `properties-of<T>`, …), which
/// floors to [`ContractTy::Opaque`] instead, for the same nonexistent-class-hazard
/// reason [`lower_identifier`] does.
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
        // Phan's `associative-array<K, V>` / `non-empty-associative-array<K, V>` —
        // the same `array<K, V>` lowering plus the not-a-list refusal (census
        // bucket ix; ADR-0062's landed `is_list` trinary is exactly the predicate
        // this needs, seeded via `to_shape_fact`'s `MapOf` arm below).
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
        // `class-string<T>` (issue #236) — carried as the BARE predicate, with
        // the parameter dropped. The generics vocabulary owns `T` (ADR-0032's
        // carry, issue #10); until it lands, widening to `class-string` is
        // strictly better than the `StrOpaque` this used to be: every member of
        // `class-string<Foo>` is a `class-string`, so the widening is sound, and
        // it keeps the parameterized spelling from being *less* precise than the
        // bare one. What is lost is only the bound, which nothing here could
        // check anyway.
        ("class-string", _) => ContractTy::StrWith(StrPreds::CLASS_STRING.close()),
        // `key-of<T>` / `value-of<T>` — the two *derived* spellings: their content
        // is not written down, it is projected out of another type. The operand is
        // lowered first through this same table, so the projection sees a
        // [`ContractTy`] and never re-reads the AST — one lowering, then one
        // projection over its result (ADR-0030's no-second-relation discipline).
        ("key-of", 1) => project_key_of(&arg(0).expect("len checked")),
        ("value-of", 1) => project_value_of(&arg(0).expect("len checked")),
        _ => ContractTy::Class(norm),
    }
}

/// Fold projected members into one contract: nothing is [`ContractTy::Never`]
/// (the empty shape genuinely has no keys and no values), one member is itself,
/// and the rest is a `Union` in declaration order with duplicates dropped —
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

/// `key-of<T>`: the type of the keys `T`'s realizations carry, projected out of
/// the already-lowered operand.
///
/// Enumerable exactly where the declaration pins the key set down:
///
/// | Operand | `key-of` |
/// | --- | --- |
/// | sealed `array{a: int, b: string}` / `list{…}` | the literal key union (`'a'\|'b'` / `0\|1`) |
/// | `array<K, V>` / `associative-array<K, V>` | `K` — already the key contract |
/// | `list<T>` / `non-empty-list<T>` | `int<0, max>`, by #14939's `0..n-1` keys |
/// | `array` / `non-empty-array` | `array-key` |
/// | anything else | [`ContractTy::Opaque`] |
///
/// **Optional keys count.** A `b?:` field is still a key the array *may* carry,
/// and PHPStan's `Type::getKeysArray()` includes it, so the projection does not
/// filter on `CField::optional`. (No conformance fixture exercises an optional
/// key here — the rule is taken from PHPStan's semantics, not derived from a
/// probe.)
///
/// **An unsealed shape is not enumerable**: `array{a: int, ...}` admits keys the
/// declaration never named, so its key set is open and the honest answer is
/// `Opaque` rather than the declared prefix. Same for a template, a const fetch,
/// a class or any non-array operand — all of which reach here already lowered to
/// something this table cannot read a key set out of.
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

/// Lower a `callable(P1, P2=): R` / `\Closure(P): R` signature (issue #11).
///
/// A **template-bearing** signature (`callable(T): T`, `\Closure<T>(T): R`) is
/// unrepresentable: its type variables would lower to bare `Class` arms and
/// yield false judgments, and Steins runs no call-site template solver
/// (ADR-0032/0051). Such a signature drops to a bare `CallableTy(None)` — the
/// same silent floor as an unsignatured `callable` — so a closure bound to it is
/// never judged. Every lowered [`CallableSig`] therefore carries only ground
/// contract arms.
fn lower_callable(c: &steins_phpdoc::ast::CallableType) -> ContractTy {
    // The identifier before the `(` still names the refined spelling — a
    // `pure-callable(int): int` is both a signature and a purity obligation, and an
    // identifier outside the callable vocabulary (a `@template` alias resolved to a
    // call shape) carries none.
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

/// The normalized runtime keys a shape's items denote, in item order — the ONE
/// shape-key rule: positional items take the running auto-index, and PHP folds an
/// integer-like string/bareword key to an int key (`array{'9': T}` declares the
/// key `9`, exactly as `[9 => …]` builds it).
///
/// `None` when a key is not resolvable at all — a const-fetch key, or an int
/// literal that does not parse — which makes the whole shape undecidable
/// (`Opaque` when lowering, `Maybe` when judging).
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
/// [`ContractTy`] variants become a single [`ShapeFact`], and every other
/// contract (scalars, classes, `iterable`, unions, …) is `None` — "not an
/// array truth this crate can state", the honest floor.
///
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
/// `is_list` is never taken from the caller as truth: the seed is *sharpened*
/// by [`ShapeFact::normalize`]'s denotational computation, which is free to
/// contradict a `list`-flavored declaration whose keys make it impossible.
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
/// about ONE value, or `None` where the fact domain cannot express it — which
/// is not a failure but the floor the ADR names, since a `None` slot admits
/// everything and the declared fidelity stays in the aligned arm lane.
///
/// `None` covers, deliberately:
///
/// * classes, `object`, `callable`, `iterable`, and every intersection whose
///   members are not ALL string refinements — no fact form. The all-`StrWith`
///   intersection is the one exception (issue #240): see [`inter_str_preds`];
/// * `mixed` / `Opaque` / `never` — an unknown slot *is* `mixed`, so spelling
///   it costs a representation with no extra content;
/// * `literal-string` &c. ([`ContractTy::StrOpaque`]) — non-extensional
///   (ADR-0038): a fact would claim membership the relation refuses to decide.
///   `class-string` is NOT in this bucket since issue #236 — it lowers to
///   [`ContractTy::StrWith`] and gets the ordinary refined-string fact;
/// * **`float` and float literals** — `ContractTy::Base(Base::Float)` accepts
///   ints (PHPStan core semantics, noted on the variant), while
///   `Fact::General { base: Float }` does not. Lowering it would make the fact
///   *narrower* than the contract it came from, i.e. the fact would reject
///   arrays the declaration admits. The floor is the sound side.
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
/// `A&B` over string refinements needs no intersection *algebra*: the domain's
/// representation of a refined string is already a conjunction of predicates
/// (ADR-0035), so the intersection of two of them is the union of their bits —
/// and [`StrPreds::union`] closes under implication, so the fold lands on the
/// canonical set every other producer would have built. There is exactly one
/// fold, read by the three consumers that need it ([`to_fact`], the arm speller,
/// and `steins-infer`'s curated-row lowering), so a seeded conjunction and its
/// spelling cannot disagree.
///
/// Two boundaries, both inherited rather than invented here:
///
/// * a non-`StrWith` member refuses the WHOLE intersection — `literal-string`
///   ([`ContractTy::StrOpaque`], provenance, ADR-0038) and every object/array arm
///   included. The honest floor stays: an intersection this cannot fold keeps
///   living in the arm lane, judged by the acceptance relation's `Inter` arms;
/// * **complementary bits are not a special case.** `decimal-int-string&non-decimal-int-string`
///   folds to the set carrying both, which [`StrPreds`]' module doc establishes
///   denotes ∅ — a legitimate value of the type, and the one `StrWith` that
///   admits no string at all. Nothing here needs to detect it.
///
/// [`StrPreds::CLASS_STRING`] folds like any other bit: it is a value property
/// decided against the class table (issue #236), so an intersection carrying it
/// records it and every membership query keeps reading the set through
/// [`StrPreds::extensional`], exactly as it does for a `class-string` that
/// arrived alone.
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
/// vocabulary, or `None` when the intersection is not of that shape (issue #238).
///
/// `non-empty-array<string, int>&hasOffset('foo')` is PHPStan's spelling for a
/// fact Steins already carries: an unsealed shape with a required key. It is not
/// an intersection Steins needs an *algebra* for — it is a shape written in
/// another dialect, and this is the translation:
///
/// ```text
/// non-empty-array<string, int>&hasOffset('foo')        → non-empty-array{foo: int, ...<string, int>}
/// non-empty-array<string, int>&hasOffsetValue('foo', 17) → non-empty-array{foo: 17, ...<string, int>}
/// non-empty-list<int>&hasOffsetValue(0, 17)            → non-empty-list{17, ...<int, int>}
/// ```
///
/// `hasOffset(K)` states presence and nothing about the value, so the field takes
/// the base's own value contract — which is exactly what Steins' own speller
/// prints for the shape it computes. `hasOffsetValue(K, V)` states both, so the
/// field takes `V`.
///
/// This runs on the **AST**, before the members are lowered, because a predicate
/// has no contract form of its own to fold from: `ContractTy` gains no variant
/// here, and the accessory names lower to [`ContractTy::Opaque`] (via
/// `KNOWN_UNENFORCED`) wherever this fold declines.
///
/// # What it refuses, and why the refusals are the honest floor
///
/// * **a non-array base** — `ArrayObject<int, …>&hasOffset(1)` puts the predicate
///   on a class arm, where ADR-0062's vocabulary says nothing. Refused, so the row
///   keeps its `Inter` and its `Maybe`;
/// * **more than one non-accessory arm** — there is one shape to build and no rule
///   for which arm it is built from;
/// * **a non-literal key** — a shape key is a literal by construction ([`CKey`]),
///   and inventing one from `hasOffset(int)` would claim a key nobody named;
/// * **an already-`Shape` base** — PHPStan does not spell one, and merging a
///   predicate into declared fields is a second merge rule this slice does not need.
///
/// Every refusal leaves the intersection exactly where it was, so nothing this
/// fold declines becomes *less* precise than it is today.
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
        // A list tail carries no key: `list: true` already pins the keys to
        // `0..n-1`, so naming `int` again would be a second, weaker copy of the
        // same fact — and `list{…, ...<V>}`, the spelling this must agree with,
        // lowers to exactly this `None`.
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
/// (`steins_phpdoc`'s `is_accessory_predicate`) — the parser decides what gets a
/// [`TypeKind::Generic`](steins_phpdoc::ast::TypeKind::Generic) node, this decides
/// what the fold does with one, and both are case-blind.
fn is_accessory_base(base: &str) -> bool {
    base.eq_ignore_ascii_case("hasOffset") || base.eq_ignore_ascii_case("hasOffsetValue")
}

/// Lower a declared shape's parts into the canonical [`ShapeFact`] — the
/// shared core of [`to_shape_fact`]'s `Shape` arm and [`shape_is_list`].
///
/// Field presence is the declared optionality at the *declared* presence
/// stratum (`Required { witnessed: false }`): a docblock states presence, it
/// does not witness it (§3 — presence carries its own stratum, and only a
/// guard that really executed promotes it).
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
/// (ADR-0062 §6): the ONE computation, reused from
/// [`steins_domain::ShapeFact::normalize`] rather than re-implemented here or
/// in the speller (`spell.rs` calls this, never its own copy) — via the
/// same [`shape_fact_of_parts`] lowering [`to_shape_fact`] uses, so the
/// spelled verdict and the seeded fact can never disagree.
///
/// `list` is the declared `list{…}`/`array{…}` keyword: it seeds the
/// `Certainty` [`ShapeFact::normalize`] sharpens (never contradicts) exactly
/// as `list<T>`'s own lowering does (A-G1) — a `list{…}`-declared shape is
/// forced `Yes` unless the fields themselves prove otherwise (e.g. a required
/// string key, a genuine contradiction).
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

/// The [`KNOWN_UNENFORCED`] hazard fix — a known-vocabulary pseudo-type spelling
/// floors to [`ContractTy::Opaque`] (always `Maybe`) rather than the
/// nonexistent-class-reference `Class` catch-all, which would otherwise
/// manufacture a `No` for every non-object value (the same hazard handled for
/// `key-of`/`value-of`).
#[cfg(test)]
mod known_unenforced_tests {
    use super::*;
    use steins_domain::Val;

    #[test]
    fn known_unenforced_identifiers_lower_to_opaque() {
        for name in [
            "resource",
            "open-resource",
            "closed-resource",
            "non-empty-literal-string",
            "arraylike-object",
            "stringable-object",
        ] {
            assert_eq!(lower_identifier(name), ContractTy::Opaque, "{name} should lower to Opaque");
        }
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

    /// The pin: `int-mask<1, 2, 4>` admits an int as `Maybe` — not the `No` the
    /// old `Class("int-mask")` catch-all would have manufactured for every
    /// scalar (the wrong-No hazard this fix retires).
    #[test]
    fn int_mask_admits_an_int_as_maybe_not_no() {
        let ty = lower_str("int-mask<1, 2, 4>").unwrap();
        assert_eq!(admits_val(&ty, &Val::Int(5)), Certainty::Maybe);
    }

    /// The load-bearing floor this fix must not touch: a genuinely unknown name
    /// (not in [`KNOWN_UNENFORCED`], not a keyword) still lowers to `Class` — the
    /// signal both lanes' class machinery depends on.
    #[test]
    fn a_genuinely_unknown_name_still_lowers_to_class() {
        assert_eq!(lower_identifier("TotallyUnknownFrobnicator"), ContractTy::Class("totallyunknownfrobnicator".to_owned()));
        assert!(matches!(
            lower_generic("SomeUnknownGeneric", &[]),
            ContractTy::Class(name) if name == "someunknowngeneric"
        ));
    }
}

/// Issue #240 — the refined-string grid and the intersection fold: one closed
/// vocabulary, spelled and re-read by one pair of inverse functions.
/// Plain object intersections (issue #238): `ArrayAccess&stdClass` — the arms
/// Steins already has, conjoined. What was missing was never the arms.
#[cfg(test)]
mod object_intersection_tests {
    use super::*;
    use crate::normalize::subsumes;

    fn inter(s: &str) -> ContractTy {
        lower_str(s).unwrap_or_else(|| panic!("{s} did not lower"))
    }

    /// Representable: the declared spelling lowers to a conjunction of class arms,
    /// with the arms intact and in order. Class names normalize exactly as a lone
    /// class arm's does (lowercased, leading `\` stripped) — the conjunction adds no
    /// naming rule of its own.
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

    /// Spellable: `spell_nested` joins the arms with `&`, so an intersection
    /// travelling inside an array or a union round-trips to the same conjunction.
    ///
    /// The scalar-arm speller ([`spell::spell_arms`]) still refuses it — but it
    /// refuses a bare `Class` arm identically, for the identical reason (no faithful
    /// *scalar* spelling exists). An intersection is exactly as spellable as its
    /// arms are, which is the property that matters: the conjunction adds no new
    /// refusal.
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

    /// The acceptance relation judges the conjunction ARM-WISE, in both directions.
    #[test]
    fn the_relation_judges_an_object_intersection_arm_wise() {
        let ab = inter("ArrayAccess&stdClass");
        let a = ContractTy::Class("arrayaccess".to_owned());

        // `A ⊇ A∩B` — proven: the intersection is a subset of each arm.
        assert!(subsumes(&a, &ab).is_yes(), "an arm covers the conjunction");
        // `A∩B ⊇ A` — NOT proven: a plain `ArrayAccess` need not be a `stdClass`.
        // The honest `Maybe`, never a `No`.
        assert_eq!(subsumes(&ab, &a), Certainty::Maybe);
        // Reflexivity, which only the arm-wise rule can reach: asked whole, the
        // conjunction reduces to `A∩B ⊇ A` above and folds to `Maybe`.
        assert!(subsumes(&ab, &ab).is_yes(), "a conjunction subsumes itself");
        // Order is not identity: the same conjunction spelled the other way round
        // is proven equal in both directions.
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
        // A scalar never *covers* an object conjunction. `Maybe` rather than `No` is
        // the `b`-is-an-intersection floor this crate already keeps everywhere — what
        // is pinned here is only that it can never be a `Yes`.
        assert!(!subsumes(&ContractTy::Base(Base::Int), &ab).is_yes());
        // `mixed` covers every object, conjoined or not.
        assert!(subsumes(&ContractTy::Mixed, &ab).is_yes());
    }
}

/// The array accessory fold (issue #238): PHPStan's `hasOffset` dialect against
/// the ADR-0062 vocabulary Steins already speaks. Every `expected` string here is
/// copied from an nsrt row, and every `got` string is what Steins renders for that
/// same row today — so these pin the *translation*, not a constructed pair.
#[cfg(test)]
mod array_accessory_tests {
    use super::*;
    use crate::normalize::subsumes;

    /// The two lowerings denote the same set: the relation proves it in both
    /// directions, which is exactly what earns an nsrt `equal`.
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

    /// A list base keeps its list-ness, and the stacked form folds every predicate
    /// into one shape (`list-type.php:116`).
    ///
    /// The Steins side is spelled `...<int>`, not `...<int, int>`: a list-shape
    /// tail has no key slot in the phpdoc grammar, and the speller emitting one was
    /// a round-trip defect this slice had to fix to reach the row at all.
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

    /// The fold never invents a key it was not given, never attaches to a base
    /// ADR-0062 does not speak for, and never seals the tail. Each of these keeps
    /// the intersection it had — the honest floor, not a widening.
    #[test]
    fn the_refusals_keep_the_intersection() {
        // A class base: `ArrayObject<int, array<string, mixed>>&hasOffset(1)`.
        let ty = lower_str("ArrayObject<int, string>&hasOffset(1)").unwrap();
        assert!(matches!(ty, ContractTy::Inter(_)), "a class base must not fold: {ty:?}");
        // A non-literal key names no shape key.
        let ty = lower_str("non-empty-array<string, int>&hasOffset(string)").unwrap();
        assert!(matches!(ty, ContractTy::Inter(_)), "a non-literal key must not fold: {ty:?}");
        // Two non-accessory arms: no rule for which one the shape is built from.
        let ty = lower_str("array<string, int>&Countable&hasOffset('a')").unwrap();
        assert!(matches!(ty, ContractTy::Inter(_)), "two bases must not fold: {ty:?}");
    }

    /// A predicate with no array arm to attach to lowers to the honest `Opaque`,
    /// NOT to a nonexistent class. `Class("hasoffset")` would make the class leg of
    /// acceptance answer a definite `No` for every array value — a manufactured
    /// false positive, which is what `KNOWN_UNENFORCED` exists to prevent.
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

    /// Every cell of the grid, built from its own axes rather than from a copy of
    /// the speller's table.
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

    /// **The round trip** the speller's docs promise: every keyword
    /// [`preds_keyword`] can emit lowers back through [`lower_identifier`] to the
    /// very set it was spelled from. This is what lets `spell` emit phpdoc that
    /// Steins re-reads without loss, and it is why the grid is a parse on one side
    /// and a `format!` on the other instead of two tables.
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

    /// The twenty cells are twenty distinct words: a grid that named two sets the
    /// same would make the round trip above lossy in one direction only.
    #[test]
    fn the_grid_is_injective() {
        let mut seen: Vec<String> = cells().iter().map(|p| preds_keyword(*p)).collect();
        seen.sort();
        let n = seen.len();
        seen.dedup();
        assert_eq!(seen.len(), n, "two cells share a spelling");
    }

    /// The spellings themselves, written out — the table a reader of the issue can
    /// check against, including the two cells that predate the grid.
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

    /// The array-key-cast pair is still not an axis (the decision #240 left alone):
    /// a declared `decimal-int-string` widens to the cell its closure names, never
    /// to a keyword of its own.
    #[test]
    fn the_array_key_cast_pair_is_still_not_a_rung() {
        assert_eq!(preds_keyword(StrPreds::DECIMAL_INT.close()), "numeric-uncased-string");
        assert_eq!(preds_keyword(StrPreds::NON_DECIMAL_INT), "string");
    }

    /// `class-string` still outranks the grid — the contextual bit says something
    /// no character-level rung can (issue #236), and it round-trips as itself.
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

    /// The fold: `A&B` over string refinements is the closed union of their bits,
    /// so a declared conjunction lowers to exactly the set an equivalent computed
    /// one would carry.
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

    /// Complementary bits need no special case: the fold builds the set that
    /// denotes ∅, which is a legitimate `StrWith` (the `StrPreds` module doc).
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

    /// The floor stays: one non-`StrWith` arm refuses the whole intersection, so
    /// an object/provenance conjunction still lives in the arm lane alone.
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

    /// The contextual bit folds like any other — it is a value property (#236),
    /// and every membership query keeps reading the set extensionally.
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
        // …but a scalar UNION no longer floors (issue #339). `int|string` was
        // in this list only because the value domain had no two-base form; it
        // has one now, so the slot carries the union instead of nothing.
        assert_eq!(
            slot(&s, "d"),
            Fact::union(vec![(Base::Int, None), (Base::String, None)], false)
        );
        // …but `class-string` no longer does (issue #236): it is a string
        // refinement now, so the slot carries the predicate instead of nothing.
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
        // A scalar union lowers into the value lane as of issue #339; `float`
        // still floors, for its own reason (it accepts an int at the native
        // seam, so the base alone is not the acceptance).
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
