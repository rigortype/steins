//! The spanned PHPDoc type AST and its canonical rendering.
//!
//! [`Type`] is Steins' own type representation — no phpstan/phpdoc-parser type
//! leaks here. Every node carries a byte [`Span`] into the source type string.
//!
//! The [`std::fmt::Display`] impls reproduce phpstan/phpdoc-parser's node
//! `__toString()` **exactly** (ADR-0029): unions/intersections always
//! parenthesize (`(A | B)`, `(A & B)`), a nullable member inside a union/
//! intersection gets an extra pair of parens, postfix `[]` wraps callable/const/
//! nullable bases, and so on. This canonical form is the compatibility contract
//! checked against the real parser by the oracle harness.

use std::fmt;

/// A byte span `[start, end)` into the parsed type string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }
}

/// A spanned PHPDoc type node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Type {
    pub span: Span,
    pub kind: TypeKind,
}

impl Type {
    pub fn new(span: Span, kind: TypeKind) -> Self {
        Self { span, kind }
    }
}

/// The shape of a [`Type`]. Variants mirror phpstan/phpdoc-parser's type nodes,
/// restricted to the envelope-checking subset (ADR-0029) plus [`TypeKind::Unsupported`]
/// for a grammatical construct we deliberately keep opaque.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeKind {
    /// A keyword, scalar, class name, or FQCN: `int`, `\App\User`, `self`, `true`.
    Identifier(String),
    /// `$this`.
    This,
    /// `?T`.
    Nullable(Box<Type>),
    /// `A | B | …`. `benevolent` records provenance from `__benevolent<…>`
    /// expansion (ADR-0030): syntactically accepted, semantically a plain union.
    Union { types: Vec<Type>, benevolent: bool },
    /// `A & B & …`.
    Intersection(Vec<Type>),
    /// `T[]`.
    Array(Box<Type>),
    /// `Foo<A, B>`, `array<K, V>`, `list<T>`, `int<0, max>`, `Collection<T>`.
    Generic { base: String, args: Vec<GenericArg> },
    /// `callable(int, string=): bool`, `\Closure<T>(T): R`.
    Callable(CallableType),
    /// `array{…}`, `list{…}`, `non-empty-array{…}`, `non-empty-list{…}`.
    ArrayShape(ArrayShape),
    /// `object{a: int, b?: string}`.
    ObjectShape(Vec<ShapeItem>),
    /// `T[K]` — offset access.
    OffsetAccess { base: Box<Type>, offset: Box<Type> },
    /// A constant type: literal `'x'`/`"y"`/`123`/`1.5`, or a const fetch
    /// `Foo::BAR` / `Foo::*`.
    Const(ConstExpr),
    /// `(T is U ? A : B)` and `($param is U ? A : B)`.
    Conditional(Conditional),
    /// A construct inside the reference grammar that Steins keeps opaque: the raw
    /// source text is retained so callers can render it, but it carries no
    /// envelope. Currently unused by the parser (the whole reference grammar is
    /// modelled) — reserved for forward compatibility with upstream additions.
    Unsupported(String),
}

/// One argument of a generic type, with its declared variance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenericArg {
    pub variance: Variance,
    pub ty: Type,
}

/// Template argument variance inside `Foo<…>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variance {
    Invariant,
    Covariant,
    Contravariant,
    /// The `*` wildcard argument (phpdoc-parser models this as bivariant `mixed`).
    Bivariant,
}

/// A callable/Closure signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallableType {
    /// The callable identifier: `callable`, `Closure`, `\Closure`, `pure-callable`, …
    pub identifier: String,
    pub templates: Vec<TemplateParam>,
    pub params: Vec<CallableParam>,
    pub return_type: Box<Type>,
}

/// One parameter of a callable signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallableParam {
    pub ty: Type,
    pub is_reference: bool,
    pub is_variadic: bool,
    /// The `$name`, or empty when anonymous.
    pub name: String,
    pub is_optional: bool,
}

/// A callable template parameter: `T of Bound super Lower = Default`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateParam {
    pub name: String,
    pub bound: Option<Type>,
    pub lower: Option<Type>,
    pub default: Option<Type>,
}

/// The kind of an array-shape type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrayShapeKind {
    Array,
    List,
    NonEmptyArray,
    NonEmptyList,
}

impl ArrayShapeKind {
    fn as_str(self) -> &'static str {
        match self {
            ArrayShapeKind::Array => "array",
            ArrayShapeKind::List => "list",
            ArrayShapeKind::NonEmptyArray => "non-empty-array",
            ArrayShapeKind::NonEmptyList => "non-empty-list",
        }
    }
}

/// `array{…}` / `list{…}` and their non-empty variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArrayShape {
    pub kind: ArrayShapeKind,
    pub items: Vec<ShapeItem>,
    /// `false` when the shape is unsealed (`…`).
    pub sealed: bool,
    /// The optional value/key types of the unsealed tail: `…<V>` or `…<K, V>`.
    pub unsealed: Option<UnsealedType>,
}

/// One `key?: value` (or positional `value`) entry of an array/object shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShapeItem {
    pub key: Option<ShapeKey>,
    pub optional: bool,
    pub value: Type,
}

/// A shape item key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShapeKey {
    Int(String),
    Str(StringLit),
    ConstFetch { class: String, name: String },
    Ident(String),
}

/// The `…<V>` / `…<K, V>` type of an unsealed array shape's tail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsealedType {
    pub value: Box<Type>,
    pub key: Option<Box<Type>>,
}

/// A constant expression used as a type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConstExpr {
    /// Integer literal, underscores already stripped (e.g. `123`, `+0x1020`).
    Int(String),
    /// Float literal, underscores already stripped (e.g. `1.5`, `+8e+2`).
    Float(String),
    /// Quoted string literal (value stored unescaped).
    Str(StringLit),
    True,
    False,
    Null,
    /// `Class::CONST`, `Class::PREFIX_*`, or a bare `CONST` (empty class).
    Fetch { class: String, name: String },
}

/// A string literal and its original quote style — the style governs re-escaping
/// in the canonical form (double-quoted escapes `$`, single-quoted does not).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StringLit {
    Single(String),
    Double(String),
}

/// A conditional type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conditional {
    pub subject: ConditionalSubject,
    pub target: Box<Type>,
    pub if_type: Box<Type>,
    pub else_type: Box<Type>,
    pub negated: bool,
}

/// The subject tested by a conditional type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConditionalSubject {
    /// `T is U ? …` — an arbitrary type on the left.
    Type(Box<Type>),
    /// `$param is U ? …` — a parameter name (`$foo`).
    Parameter(String),
}

// ----------------------------------------------------------------------------
// Display — the canonical form. Mirrors phpstan/phpdoc-parser node __toString().
// ----------------------------------------------------------------------------

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.kind.fmt(f)
    }
}

impl fmt::Display for TypeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TypeKind::Identifier(name) => f.write_str(name),
            TypeKind::This => f.write_str("$this"),
            TypeKind::Nullable(t) => write!(f, "?{t}"),
            TypeKind::Union { types, .. } => {
                f.write_str("(")?;
                write_joined_wrapping_nullable(f, types, " | ")?;
                f.write_str(")")
            }
            TypeKind::Intersection(types) => {
                f.write_str("(")?;
                write_joined_wrapping_nullable(f, types, " & ")?;
                f.write_str(")")
            }
            TypeKind::Array(t) => {
                if matches!(
                    t.kind,
                    TypeKind::Callable(_) | TypeKind::Const(_) | TypeKind::Nullable(_)
                ) {
                    write!(f, "({t})[]")
                } else {
                    write!(f, "{t}[]")
                }
            }
            TypeKind::Generic { base, args } => {
                f.write_str(base)?;
                f.write_str("<")?;
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    arg.fmt(f)?;
                }
                f.write_str(">")
            }
            TypeKind::Callable(c) => c.fmt(f),
            TypeKind::ArrayShape(s) => s.fmt(f),
            TypeKind::ObjectShape(items) => {
                f.write_str("object{")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    item.fmt(f)?;
                }
                f.write_str("}")
            }
            TypeKind::OffsetAccess { base, offset } => {
                if matches!(base.kind, TypeKind::Callable(_) | TypeKind::Nullable(_)) {
                    write!(f, "({base})[{offset}]")
                } else {
                    write!(f, "{base}[{offset}]")
                }
            }
            TypeKind::Const(c) => c.fmt(f),
            TypeKind::Conditional(c) => c.fmt(f),
            TypeKind::Unsupported(raw) => f.write_str(raw),
        }
    }
}

/// Render `types` joined by `sep`, wrapping any [`TypeKind::Nullable`] member in
/// an extra pair of parentheses (phpdoc-parser's union/intersection convention).
fn write_joined_wrapping_nullable(
    f: &mut fmt::Formatter<'_>,
    types: &[Type],
    sep: &str,
) -> fmt::Result {
    for (i, t) in types.iter().enumerate() {
        if i > 0 {
            f.write_str(sep)?;
        }
        if matches!(t.kind, TypeKind::Nullable(_)) {
            write!(f, "({t})")?;
        } else {
            write!(f, "{t}")?;
        }
    }
    Ok(())
}

impl fmt::Display for GenericArg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.variance {
            Variance::Invariant => self.ty.fmt(f),
            Variance::Bivariant => f.write_str("*"),
            Variance::Covariant => write!(f, "covariant {}", self.ty),
            Variance::Contravariant => write!(f, "contravariant {}", self.ty),
        }
    }
}

impl fmt::Display for CallableType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.identifier)?;
        if !self.templates.is_empty() {
            f.write_str("<")?;
            for (i, t) in self.templates.iter().enumerate() {
                if i > 0 {
                    f.write_str(", ")?;
                }
                t.fmt(f)?;
            }
            f.write_str(">")?;
        }
        f.write_str("(")?;
        for (i, p) in self.params.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            p.fmt(f)?;
        }
        f.write_str("): ")?;
        // A callable return type is itself parenthesized.
        if matches!(self.return_type.kind, TypeKind::Callable(_)) {
            write!(f, "({})", self.return_type)
        } else {
            self.return_type.fmt(f)
        }
    }
}

impl fmt::Display for CallableParam {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Mirrors CallableTypeParameterNode::__toString: trim("{type} {&}{...}{name}") + optional "=".
        let reference = if self.is_reference { "&" } else { "" };
        let variadic = if self.is_variadic { "..." } else { "" };
        let core = format!("{} {reference}{variadic}{}", self.ty, self.name);
        f.write_str(core.trim())?;
        if self.is_optional {
            f.write_str("=")?;
        }
        Ok(())
    }
}

impl fmt::Display for TemplateParam {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Mirrors TemplateTagValueNode::__toString with an empty description.
        f.write_str(&self.name)?;
        if let Some(b) = &self.bound {
            write!(f, " of {b}")?;
        }
        if let Some(l) = &self.lower {
            write!(f, " super {l}")?;
        }
        if let Some(d) = &self.default {
            write!(f, " = {d}")?;
        }
        Ok(())
    }
}

impl fmt::Display for ArrayShape {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.kind.as_str())?;
        f.write_str("{")?;
        let mut first = true;
        for item in &self.items {
            if !first {
                f.write_str(", ")?;
            }
            first = false;
            item.fmt(f)?;
        }
        if !self.sealed {
            if !first {
                f.write_str(", ")?;
            }
            f.write_str("...")?;
            if let Some(u) = &self.unsealed {
                u.fmt(f)?;
            }
        }
        f.write_str("}")
    }
}

impl fmt::Display for ShapeItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(key) = &self.key {
            write!(
                f,
                "{}{}: {}",
                key,
                if self.optional { "?" } else { "" },
                self.value
            )
        } else {
            self.value.fmt(f)
        }
    }
}

impl fmt::Display for ShapeKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ShapeKey::Int(v) => f.write_str(v),
            ShapeKey::Str(s) => s.fmt(f),
            ShapeKey::ConstFetch { class, name } => {
                if class.is_empty() {
                    f.write_str(name)
                } else {
                    write!(f, "{class}::{name}")
                }
            }
            ShapeKey::Ident(s) => f.write_str(s),
        }
    }
}

impl fmt::Display for UnsealedType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.key {
            Some(k) => write!(f, "<{}, {}>", k, self.value),
            None => write!(f, "<{}>", self.value),
        }
    }
}

impl fmt::Display for ConstExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConstExpr::Int(v) | ConstExpr::Float(v) => f.write_str(v),
            ConstExpr::Str(s) => s.fmt(f),
            ConstExpr::True => f.write_str("true"),
            ConstExpr::False => f.write_str("false"),
            ConstExpr::Null => f.write_str("null"),
            ConstExpr::Fetch { class, name } => {
                if class.is_empty() {
                    f.write_str(name)
                } else {
                    write!(f, "{class}::{name}")
                }
            }
        }
    }
}

impl fmt::Display for StringLit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StringLit::Single(v) => write!(f, "'{}'", escape_single_quoted(v)),
            StringLit::Double(v) => write!(f, "\"{}\"", escape_double_quoted(v)),
        }
    }
}

impl fmt::Display for Conditional {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let op = if self.negated { "is not" } else { "is" };
        write!(
            f,
            "({} {op} {} ? {} : {})",
            self.subject, self.target, self.if_type, self.else_type
        )
    }
}

impl fmt::Display for ConditionalSubject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConditionalSubject::Type(t) => t.fmt(f),
            ConditionalSubject::Parameter(name) => f.write_str(name),
        }
    }
}

/// `addcslashes($v, "'\\")` — escape `'` and `\` for a single-quoted literal.
fn escape_single_quoted(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for ch in v.chars() {
        if ch == '\'' || ch == '\\' {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// `addcslashes($v, "\n\r\t\f\v$\"\\")` — escape the C-notation controls plus
/// `$`, `"`, `\` for a double-quoted literal. The reference additionally hex-
/// escapes stray control/invalid-UTF-8 bytes; those don't occur in valid type
/// strings, so the common escapes suffice for canonical equality.
fn escape_double_quoted(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for ch in v.chars() {
        match ch {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{0c}' => out.push_str("\\f"),
            '\u{0b}' => out.push_str("\\v"),
            '$' => out.push_str("\\$"),
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            _ => out.push(ch),
        }
    }
    out
}
