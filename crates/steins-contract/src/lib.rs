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
    Base, Certainty, Fact, IntRange, KeyClass, Key as DKey, Presence as DPresence, Refinement,
    ShapeFact, StrPreds, Tail as DTail, Val,
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
    Str(String),
}

/// The semantic contract type — the lowered, normalized form phpdoc types
/// are checked through.
#[derive(Debug, Clone, PartialEq)]
pub enum ContractTy {
    /// `mixed` — admits everything, including null.
    Mixed,
    /// `never` — admits nothing.
    Never,
    /// The null type.
    Null,
    /// A scalar base. NOTE: `float` accepts ints (PHPStan core semantics).
    Base(Base),
    /// `int<lo, hi>`, `positive-int`, ….
    IntIn(IntRange),
    /// `numeric-string`, `non-empty-string`, `non-falsy-string`.
    StrWith(StrPreds),
    /// A string-based type whose membership is non-extensional or unmodeled
    /// (`class-string`, `literal-string`, `lowercase-string`, …): strings
    /// are `Maybe`, everything else `No` (ADR-0038).
    StrOpaque,
    /// Integer literal type.
    LitInt(i64),
    /// Float literal type (compared by PHP value equality — IEEE `==`, so
    /// int `5` satisfies `5.0`; deliberately unlike the domain's set
    /// equality).
    LitFloat(f64),
    /// String literal type.
    LitStr(String),
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
    /// `array<K, V>` / `T[]` / `non-empty-array<K, V>`.
    MapOf {
        /// Key contract.
        key: Box<ContractTy>,
        /// Value contract.
        val: Box<ContractTy>,
        /// Reject the empty array.
        non_empty: bool,
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
    CallableTy(Option<Box<CallableSig>>),
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
        TypeKind::Intersection(types) => {
            ContractTy::Inter(types.iter().map(lower).collect())
        }
        TypeKind::Array(elem) => ContractTy::MapOf {
            key: Box::new(array_key()),
            val: Box::new(lower(elem)),
            non_empty: false,
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

fn array_key() -> ContractTy {
    ContractTy::Union(vec![ContractTy::Base(Base::Int), ContractTy::Base(Base::String)])
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

fn lower_identifier(name: &str) -> ContractTy {
    let norm = name.trim_start_matches('\\').to_ascii_lowercase();
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
        "scalar" => ContractTy::Union(vec![
            ContractTy::Base(Base::Int),
            ContractTy::Base(Base::Float),
            ContractTy::Base(Base::String),
            ContractTy::Base(Base::Bool),
        ]),
        "array-key" => array_key(),
        "numeric" => ContractTy::Union(vec![
            ContractTy::Base(Base::Int),
            ContractTy::Base(Base::Float),
            ContractTy::StrWith(StrPreds::NUMERIC.close()),
        ]),
        "numeric-string" => ContractTy::StrWith(StrPreds::NUMERIC.close()),
        "non-empty-string" => ContractTy::StrWith(StrPreds::NON_EMPTY),
        "non-falsy-string" | "truthy-string" => ContractTy::StrWith(StrPreds::NON_FALSY.close()),
        "literal-string" | "class-string" | "interface-string" | "enum-string" | "trait-string"
        | "lowercase-string" | "uppercase-string" | "callable-string" | "numeric-int-string" => {
            ContractTy::StrOpaque
        }
        "positive-int" => ContractTy::IntIn(IntRange::POSITIVE),
        "negative-int" => ContractTy::IntIn(IntRange::NEGATIVE),
        "non-negative-int" => ContractTy::IntIn(IntRange::NON_NEGATIVE),
        "non-positive-int" => {
            ContractTy::IntIn(IntRange::new(i64::MIN, 0).expect("valid range"))
        }
        "array" => ContractTy::ArrayAny { non_empty: false },
        "non-empty-array" => ContractTy::ArrayAny { non_empty: true },
        "list" => ContractTy::ListOf { elem: Box::new(ContractTy::Mixed), non_empty: false },
        "non-empty-list" => {
            ContractTy::ListOf { elem: Box::new(ContractTy::Mixed), non_empty: true }
        }
        "iterable" => ContractTy::IterableOf {
            key: Box::new(ContractTy::Mixed),
            val: Box::new(ContractTy::Mixed),
        },
        "callable" | "pure-callable" | "callable-object" | "closure" => ContractTy::CallableTy(None),
        "object" => ContractTy::ObjectAny,
        "self" | "static" | "parent" | "key-of" | "value-of" => ContractTy::Opaque,
        _ => ContractTy::Class(norm),
    }
}

fn lower_generic(base: &str, args: &[steins_phpdoc::ast::GenericArg]) -> ContractTy {
    let norm = base.trim_start_matches('\\').to_ascii_lowercase();
    let arg = |i: usize| args.get(i).map(|a| lower(&a.ty));
    match (norm.as_str(), args.len()) {
        ("array" | "non-empty-array", 1) => ContractTy::MapOf {
            key: Box::new(array_key()),
            val: Box::new(arg(0).expect("len checked")),
            non_empty: norm.starts_with("non-empty"),
        },
        ("array" | "non-empty-array", 2) => ContractTy::MapOf {
            key: Box::new(arg(0).expect("len checked")),
            val: Box::new(arg(1).expect("len checked")),
            non_empty: norm.starts_with("non-empty"),
        },
        ("list" | "non-empty-list", 1) => ContractTy::ListOf {
            elem: Box::new(arg(0).expect("len checked")),
            non_empty: norm.starts_with("non-empty"),
        },
        ("int", 2) => lower_int_range(args),
        ("iterable", 1) => ContractTy::IterableOf {
            key: Box::new(ContractTy::Mixed),
            val: Box::new(arg(0).expect("len checked")),
        },
        ("iterable", 2) => ContractTy::IterableOf {
            key: Box::new(arg(0).expect("len checked")),
            val: Box::new(arg(1).expect("len checked")),
        },
        ("class-string", _) => ContractTy::StrOpaque,
        _ => ContractTy::Class(norm),
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
    if !c.templates.is_empty() {
        return ContractTy::CallableTy(None);
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
    ContractTy::CallableTy(Some(Box::new(CallableSig { params, ret: lower(&c.return_type) })))
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
        _ => CKey::Str(s.to_owned()),
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
        ContractTy::MapOf { key, val, non_empty } => Some(ShapeFact::normalize(
            Vec::new(),
            DTail::Unsealed {
                key: contract_key_class(key),
                value: to_fact(val).map(Box::new),
            },
            Certainty::Maybe,
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
/// * classes, `object`, `callable`, `iterable`, intersections — no fact form;
/// * `mixed` / `Opaque` / `never` — an unknown slot *is* `mixed`, so spelling
///   it costs a representation with no extra content;
/// * `class-string` &c. ([`ContractTy::StrOpaque`]) — non-extensional
///   (ADR-0038): a fact would claim membership the relation refuses to decide;
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
/// (ADR-0062 §6, D4): the ONE computation, reused from
/// [`steins_domain::ShapeFact::normalize`] rather than re-implemented here or
/// in the speller (`spell.rs` calls this, never its own copy) — now via the
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

fn ckey_to_domain(k: &CKey) -> DKey {
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
        ConstExpr::Str(lit) => ContractTy::LitStr(string_lit_value(lit)),
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
        s.field(&DKey::Str(key.to_owned())).and_then(|(_, _, v)| v.clone()).map(|b| *b)
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
            s.field(&DKey::Str("a".to_owned())).map(|(_, p, _)| *p),
            Some(DPresence::Required { witnessed: false })
        );
        assert_eq!(
            s.field(&DKey::Str("b".to_owned())).map(|(_, p, _)| *p),
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
        assert!(s.admits(&a(vec![(DKey::Str("a".to_owned()), Val::Str("x".to_owned()))])));
        assert!(s.admits(&a(vec![
            (DKey::Str("a".to_owned()), Val::Str("x".to_owned())),
            (DKey::Str("b".to_owned()), Val::Int(1)),
        ])));
        // missing required key / sealed-undeclared key / wrong value type
        assert!(!s.admits(&a(vec![(DKey::Str("b".to_owned()), Val::Int(1))])));
        assert!(!s.admits(&a(vec![
            (DKey::Str("a".to_owned()), Val::Str("x".to_owned())),
            (DKey::Str("z".to_owned()), Val::Int(1)),
        ])));
        assert!(!s.admits(&a(vec![(DKey::Str("a".to_owned()), Val::Int(1))])));
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
        assert_eq!(slot(&s, "c"), Some(Fact::Singleton(Val::Str("x".to_owned()))));
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
            Some(Fact::OneOf(vec![Val::Str("x".to_owned()), Val::Str("y".to_owned())]))
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
        // Classes, callables, `mixed`, unmergeable unions, and the
        // int-accepting `float` all floor — the honest `None` (A-G1a).
        let s = shape_of(
            "array{a: Foo, b: callable, c: mixed, d: int|string, e: float, f: class-string}",
        );
        for key in ["a", "b", "c", "d", "e", "f"] {
            assert_eq!(slot(&s, key), None, "slot {key} should floor to unknown");
        }
    }

    #[test]
    fn to_fact_on_scalars_matches_the_slot_rules() {
        assert_eq!(fact_of("int"), Some(Fact::General { base: Base::Int, nullable: false }));
        assert_eq!(fact_of("?string"), Some(Fact::General { base: Base::String, nullable: true }));
        assert_eq!(fact_of("5"), Some(Fact::Singleton(Val::Int(5))));
        assert_eq!(fact_of("int|string"), None);
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
