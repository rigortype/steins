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

pub use admit::{admits_fact, admits_val};

use steins_domain::{Base, IntRange, StrPreds};
use steins_phpdoc::ast::{ArrayShapeKind, ConstExpr, ShapeKey, StringLit, Type, TypeKind};

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
    CallableTy,
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
        TypeKind::Callable(_) => ContractTy::CallableTy,
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
        "callable" | "pure-callable" | "callable-object" | "closure" => ContractTy::CallableTy,
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

fn lower_shape(shape: &steins_phpdoc::ast::ArrayShape) -> ContractTy {
    let list = matches!(shape.kind, ArrayShapeKind::List | ArrayShapeKind::NonEmptyList);
    let non_empty =
        matches!(shape.kind, ArrayShapeKind::NonEmptyArray | ArrayShapeKind::NonEmptyList);
    let mut fields = Vec::with_capacity(shape.items.len());
    let mut next_auto: i64 = 0;
    for item in &shape.items {
        let key = match &item.key {
            None => {
                let k = CKey::Int(next_auto);
                next_auto += 1;
                k
            }
            Some(ShapeKey::Int(s)) => match s.parse::<i64>() {
                Ok(v) => {
                    next_auto = next_auto.max(v.saturating_add(1));
                    CKey::Int(v)
                }
                Err(_) => return ContractTy::Opaque,
            },
            Some(ShapeKey::Str(lit)) => CKey::Str(string_lit_value(lit)),
            Some(ShapeKey::Ident(name)) => CKey::Str(name.clone()),
            Some(ShapeKey::ConstFetch { .. }) => return ContractTy::Opaque,
        };
        fields.push(CField { key, optional: item.optional, ty: lower(&item.value) });
    }
    let unsealed = shape.unsealed.as_ref().map(|u| {
        (u.key.as_ref().map(|k| Box::new(lower(k))), Box::new(lower(&u.value)))
    });
    ContractTy::Shape { list, fields, sealed: shape.sealed, non_empty, unsealed }
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
