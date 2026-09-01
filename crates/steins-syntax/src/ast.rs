//! The public, Mago-free syntax representation (ADR-0003): the owned plain-data
//! structs and enums one parsed PHP file lowers to, re-exported unchanged from the
//! crate root as `steins_syntax::*`. Pure data plus the array-key normalization
//! helpers that operate on it; no Mago type appears here.

use std::collections::HashMap;

use steins_domain::PhpStr;

// ---------------------------------------------------------------------------
// Public, Mago-free representation.
// ---------------------------------------------------------------------------

/// A byte-offset span into the source file. `end` is exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

/// A 1-based line/column position, resolved from a byte offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Position {
    pub line: u32,
    pub column: u32,
}

/// How a name was written at a *reference* site, driving PHP name resolution
/// (namespace fallback, `use` imports, builtin catalog — resolved in `steins-infer`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub enum RefKind {
    /// `\Foo\bar` — absolute, leading `\` stripped from `raw`.
    FullyQualified,
    /// `Sub\bar` — relative to the current namespace, first segment subject to `use`.
    Qualified,
    /// `bar` — subject to imports, then namespace/global fallback.
    Unqualified,
    /// `namespace\bar` (ADR-0049 A8): resolves against the **enclosing namespace
    /// only** — no `use`/global fallback (undefined `Ns\bar` is fatal); `raw` has
    /// the prefix stripped. Distinct kind avoids pre-A8 doubled-prefix mis-resolution.
    Relative,
}

/// A reference to a function/class name at a use site: raw spelling (leading
/// `\` stripped, case preserved), [`RefKind`], and byte `offset` (selects the
/// namespace via [`SourceTree::ctx_at`]); `offset` excluded from equality/hashing.
///
/// [`SourceTree::ctx_at`]: crate::SourceTree::ctx_at
#[derive(Debug, Clone)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct NameRef {
    pub raw: String,
    pub kind: RefKind,
    pub offset: u32,
}

impl PartialEq for NameRef {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw && self.kind == other.kind
    }
}
impl Eq for NameRef {}
impl std::hash::Hash for NameRef {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.raw.hash(state);
        self.kind.hash(state);
    }
}

impl NameRef {
    /// Last (unqualified) segment of the raw name — for diagnostics/same-file legacy paths.
    #[must_use]
    pub fn simple(&self) -> &str {
        match self.raw.rfind('\\') {
            Some(pos) => &self.raw[pos + 1..],
            None => &self.raw,
        }
    }
}

/// A file-region namespace context: enclosing namespace plus `use` imports in
/// scope. Names/targets **case-preserved**; import-map keys lowercased (PHP lookup is case-insensitive).
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct NsCtx {
    /// The namespace path (`App\Models`), or empty for the global namespace.
    pub namespace: String,
    /// Class/namespace imports: lowercased alias → case-preserved target FQN.
    pub class_imports: HashMap<String, String>,
    /// `use function` imports: lowercased alias → case-preserved target FQN.
    pub fn_imports: HashMap<String, String>,
    /// `use const` imports (issue #198): **exact-case** alias → case-preserved
    /// target FQN (constant names are case-sensitive in PHP, floor 8.1, ADR-0011).
    pub const_imports: HashMap<String, String>,
}

impl NsCtx {
    pub(crate) fn global() -> Self {
        Self {
            namespace: String::new(),
            class_imports: HashMap::new(),
            fn_imports: HashMap::new(),
            const_imports: HashMap::new(),
        }
    }
}

impl std::hash::Hash for NsCtx {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        // Order-independent: hash the namespace plus the sizes, so `NsCtx` can sit
        // inside the `Hash`-deriving [`SourceTree`] despite holding hash maps.
        self.namespace.hash(state);
        self.class_imports.len().hash(state);
        self.fn_imports.len().hash(state);
        self.const_imports.len().hash(state);
    }
}

/// The supported scalar native types (PHP 8.1+; ADR-0011).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub enum ScalarType {
    Int,
    Float,
    String,
    Bool,
}

impl ScalarType {
    /// The PHP keyword spelling, for diagnostic messages.
    #[must_use]
    pub const fn keyword(self) -> &'static str {
        match self {
            ScalarType::Int => "int",
            ScalarType::Float => "float",
            ScalarType::String => "string",
            ScalarType::Bool => "bool",
        }
    }
}

/// One member of a native union type: a scalar, a `false`/`true` bool-literal
/// pseudo-member, or a class/interface/enum **object** type (ADR-0043).
/// [`TypeMember::Instance`] carries the FQN twice: lowercase matching key + source-cased display. Not [`Copy`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub enum TypeMember {
    /// A full scalar type (`int`, `float`, `string`, `bool`).
    Scalar(ScalarType),
    /// A `false`/`true` literal type — accepts **only** the exact matching bool
    /// value (verified PHP 8.5: `0`/`""`/`true` into a `false`-only type all `TypeError`).
    BoolLiteral(bool),
    /// An object type: class/interface/enum name (ADR-0043), consumed by the is-a
    /// oracle; scalar-value acceptance stays silent unless incompatibility is definite.
    Instance {
        /// Namespace-resolved, **lowercase-normalized** FQN (matches
        /// [`ClassDecl::fqn`]) — every consumer keys on this, never `display`.
        fqn: String,
        /// Resolved FQN with source-declared casing preserved — diagnostics only.
        display: String,
    },
    /// A native **intersection** of object types (`A&B&…`, ADR-0043) — satisfied
    /// only when is-a **every** listed class; kept as one member so DNF types (`(A&B)|C`) stay a single union.
    InstanceInter(Vec<ClassRef>),
}

/// One class/interface membership in a native object type — FQN carried twice as
/// in [`TypeMember::Instance`]; the element of [`TypeMember::InstanceInter`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct ClassRef {
    /// The namespace-resolved, **lowercase-normalized** FQN — the matching / is-a key.
    pub fqn: String,
    /// The resolved FQN with source-declared casing preserved. Diagnostic rendering only.
    pub display: String,
}

impl TypeMember {
    /// Render for a diagnostic: PHP keyword for a scalar/bool-literal, source-cased FQN for an object.
    #[must_use]
    pub fn render_member(&self) -> String {
        match self {
            TypeMember::Scalar(s) => s.keyword().to_owned(),
            TypeMember::BoolLiteral(false) => "false".to_owned(),
            TypeMember::BoolLiteral(true) => "true".to_owned(),
            TypeMember::Instance { display, .. } => display.clone(),
            TypeMember::InstanceInter(cs) => {
                cs.iter().map(|c| c.display.as_str()).collect::<Vec<_>>().join("&")
            }
        }
    }
}

/// A native parameter/return type: scalar/object members, intersections,
/// nullable, unions. Unsupported members (`array`, `mixed`, `iterable`, etc.)
/// lower the **whole** type to `None` so the checker stays silent (ADR-0002).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct NativeType {
    /// The union members, in source order. Always non-empty — a hint that would
    /// lower to zero members (e.g. standalone `null`) lowers to `None` instead.
    pub members: Vec<TypeMember>,
    /// `true` when `?T`, or a `null` union member, makes `null` acceptable.
    pub nullable: bool,
}

impl NativeType {
    /// Render the type for a diagnostic: `int`, `?int`, `int|string`, `int|string|null`.
    #[must_use]
    pub fn render(&self) -> String {
        let mut parts: Vec<String> = self.members.iter().map(TypeMember::render_member).collect();
        if self.nullable {
            if parts.len() == 1 {
                return format!("?{}", parts[0]);
            }
            parts.push("null".to_owned());
        }
        parts.join("|")
    }

    /// `true` when any member is an object type — scalar-value consumers treat it as unknown.
    #[must_use]
    pub fn has_instance(&self) -> bool {
        self.members
            .iter()
            .any(|m| matches!(m, TypeMember::Instance { .. } | TypeMember::InstanceInter(_)))
    }
}

/// A single declared parameter.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct Param {
    /// Parameter name without the leading `$`.
    pub name: String,
    /// Native scalar/union type, or `None` when untyped / non-scalar / complex.
    pub ty: Option<NativeType>,
    // untyped surface (ADR-0078, issue #200)
    /// **File byte span of the native type hint**, `None` if none declared —
    /// [`Self::ty`] also lowers unsupported-but-valid hints to `None`; slice with [`SourceTree::source_slice`] for spelling.
    ///
    /// [`SourceTree::source_slice`]: crate::SourceTree::source_slice
    pub hint_span: Option<Span>,
    // end untyped surface (ADR-0078, issue #200)
    /// `...$x` — the checker skips this and every later position.
    pub variadic: bool,
    /// `&$x` — by-reference; the checker skips it.
    pub by_ref: bool,
    /// `$x = null` default makes the param **implicitly nullable** in PHP; used to
    /// accept `null` against a non-nullable `@param` (`string $x = null` idiom).
    pub has_null_default: bool,
    /// `true` when the parameter declares a default (`= …`) of any form — PHP
    /// requires any native type to admit it, or rejects at compile time.
    pub has_default: bool,
    /// Lowered default when representable (literal/array); non-representable
    /// (constant, `self::X`, expression) is `None` even if [`Self::has_default`].
    pub default: Option<ArgValue>,
    pub span: Span,
}

/// [`EffectOrigin`] is a structural effect-origin candidate from a CST scan of
/// a function body (ADR-0005): reports only *where* an effect could arise, not
/// whether it's proven; skips nested function/closure/class bodies, structural
/// not reachability-aware. Classifies each call argument's **lvalue root** for
/// by-ref out-parameter coloring (ADR-0063 §2.3) — distinguishes `mutate.local`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub enum RefTarget {
    /// A binding **private to the calling frame**: a plain `$v` (or `$v['k']`),
    /// not a by-ref param, no aliasing construct — unobservable → `mutate.local`.
    Local,
    /// A superglobal root (`$_SESSION`, `$GLOBALS`, …) — interpreter-global
    /// surface (ADR-0055 amendment) → `global.write`.
    Superglobal,
    /// Escapes the frame or can't be classified: property/static/const root, a
    /// by-ref param, or a variable in an aliasing frame (`global`, `static`,
    /// `$$v`, `extract`, `&`). Conservative parent `mutate` — never `.local`.
    Escaping,
}

/// The nine PHP superglobals. A by-ref write whose root is one of these is an
/// interpreter-global write however local the syntax looks.
pub(crate) const SUPERGLOBALS: &[&str] = &[
    "GLOBALS", "_SERVER", "_GET", "_POST", "_FILES", "_COOKIE", "_SESSION", "_REQUEST", "_ENV",
];

// `Deserialize` is hand-written (`crate::persist`), not derived: serde's
// derive implicitly borrows a `&str` field from the input, which a
// `&'static str` keyword can never satisfy, and `serde(with)` does not lift
// the implicit borrow. The derived `Serialize` and the hand-written inverse
// share one wire shape, pinned by the round-trip tests in `steins-db`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize))]
pub enum EffectOrigin {
    /// A call to a statically-named function at `span`. `name` resolves
    /// project-wide (builtin/user function/ambiguous → taints exhaustiveness);
    /// dynamic/method calls aren't recorded here. `arg_targets` classifies each
    /// positional argument's lvalue root (ADR-0063 §2.3); `None` for named/spread.
    /// `const_args` carries the first two args in proven-constant form (issue #318, ADR-0064).
    Call { name: NameRef, span: Span, arg_targets: Option<Vec<RefTarget>>, const_args: ConstArgs },
    /// An `echo`/`print`/short-echo, or non-blank inline HTML between `?>` and
    /// `<?php`, at `span` — `io.output.buffer` effect (ADR-0083, OB-capturable).
    Output {
        #[cfg_attr(feature = "persist", serde(serialize_with = "crate::persist::keyword::serialize"))]
        keyword: &'static str,
        span: Span,
    },
    /// An `exit` / `die` construct at `span` — the `exit` effect (ADR-0019 rule
    /// 4: `Pure` forbids exit). `keyword` is the spelling for diagnostics.
    Exit {
        #[cfg_attr(feature = "persist", serde(serialize_with = "crate::persist::keyword::serialize"))]
        keyword: &'static str,
        span: Span,
    },
    /// A method/static call whose *receiver* resolves without a flow env
    /// (`$this->`, `self::`, `parent::`, `Foo::`, `new Foo()->`) — propagates
    /// `#[\Steins\Pure]` edges, and a *declared* receiver (ADR-0067) carries an
    /// interface envelope. Other forms unrecorded.
    MethodCall { receiver: EffectRecv, method: String, span: Span },
    /// A call the scan can't classify: dynamic or unresolvable receiver/selector.
    /// No proven effect, but marks the body **non-exhaustive** (`…?` marker);
    /// ignored by the envelope check.
    Opaque { span: Span },
    /// A call passing a **resolvable callback argument** (closure, first-class
    /// callable, string-literal name; ADR-0033), instead of [`Self::Call`].
    /// Consults `steins_catalog::invocation_shape` for the callback param, else
    /// falls back to normal resolution. `arg_targets`/`const_args` mirror
    /// [`Self::Call`]'s (higher-order invokers write out-params too).
    HigherOrder {
        callee: NameRef,
        callbacks: Vec<(usize, CallbackRef)>,
        arg_count: usize,
        arg_targets: Vec<RefTarget>,
        const_args: ConstArgs,
        span: Span,
    },
    /// A direct `$fn()` call resolved (body-local single-assignment) to a known
    /// callback (ADR-0033); its effects join the caller's. Unresolvable stays [`Self::Opaque`].
    Callback { cbref: CallbackRef, span: Span },
}

/// One call argument in the form a **structural** scan can prove constant
/// (issue #318). Anything requiring dataflow (variable, concatenation,
/// interpolation, class constant, array element) is simply absent from
/// [`ConstArgs`]; the consumer keeps its argument-blind default.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub enum CallTarget {
    /// A quoted string literal with **no interpolation** (decoded value); an
    /// interpolated `"…{$x}…"` is a composite string, never this.
    Literal(String),
    /// A bare global-constant fetch by spelling, leading `\` stripped (`STDOUT`,
    /// `\STDERR`); namespaced fetches excluded, unqualified kept (PHP global fallback).
    ConstFetch(String),
}

/// The **proven-constant leading arguments** of a named call (issue #318):
/// positions 0/1, `None` unless [`CallTarget`] could read it — matching what
/// stream rows need (a target + mode/second target). Named/spread empties both.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct ConstArgs {
    /// Positional argument 0.
    pub first: Option<CallTarget>,
    /// Positional argument 1.
    pub second: Option<CallTarget>,
}

/// A resolvable callback argument (ADR-0033): an inline closure/arrow scope (by
/// definition offset) or a named free function; joins into the caller's effects/throws.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub enum CallbackRef {
    /// An inline closure/arrow whose body scope is at this definition offset.
    Closure(u32),
    /// A named free function passed as a callback (`'strtolower'`, `strtolower(...)`).
    Named(NameRef),
}

/// The receiver of an [`EffectOrigin::MethodCall`], restricted to the forms the
/// effects pass can resolve to a same-file target without a flow environment.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub enum EffectRecv {
    /// `$this->m()` — resolved against the enclosing class chain under the
    /// final/private guard (a non-final public method may be overridden).
    This,
    /// `self::m()` — same guard as `$this` (early-bound in PHP, but guard is only ever stricter).
    SelfKw,
    /// `parent::m()` — resolved on the parent chain, exact (parent is fixed).
    Parent,
    /// `Foo::m()` or `new Foo()->m()` — resolved on the referenced class's chain,
    /// exact. Carries the full [`NameRef`] so the class resolves project-wide.
    ClassName(NameRef),
    /// `$r->m()` where `$r` is a name this frame **never writes** (ADR-0067) —
    /// contributes the *declared* envelope of a project interface method; taints like [`EffectOrigin::Opaque`] otherwise.
    Var(String),
    /// `$this->repo->m()` where `repo` is a never-written property — the
    /// property-read twin of [`Self::Var`], same declared-lane rules.
    PropRead(String),
}

/// One `catch` clause's caught types + bound variable, for the throw damming
/// walk (ADR-0040); an unnameable caught type sets `has_unresolvable` → `Maybe`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct CatchClause {
    /// Statically-named caught classes (resolved to FQNs project-wide). Empty
    /// with `has_unresolvable` set means "caught, but we cannot name what".
    pub classes: Vec<NameRef>,
    /// The `$e` variable this clause binds (no `$`), for rethrow precision.
    pub var: Option<String>,
    /// A caught-type member the lowering could not name (→ absorption `Maybe`).
    pub has_unresolvable: bool,
}

/// What a [`ThrowOrigin`] contributes to a body's throw set (ADR-0040) — the
/// thrown class (explicit-throw) or a propagation edge (call variants), re-filtered by this origin's guards.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub enum ThrowKind {
    /// `throw new X(...)` — `X` is the class as written.
    New(NameRef),
    /// `throw $e` where `$e` is an enclosing catch's parameter — re-emits that
    /// catch's absorbed set (ADR-0040 rethrow precision).
    Rethrow { caught: Vec<NameRef>, has_unresolvable: bool },
    /// A statically-named function call whose throws propagate.
    Call(NameRef),
    /// A method/static call with a resolvable receiver — class-world propagation edge, like [`EffectOrigin::MethodCall`].
    MethodCall { receiver: EffectRecv, method: String },
    /// An unresolvable throw (`throw $x` non-catch var, `throw <expr>`) or
    /// dynamic/unresolved call — no reportable throw but **taints throw-exhaustiveness**.
    Taint,
    /// A call passing resolvable callback argument(s) — throw analogue of
    /// [`EffectOrigin::HigherOrder`] (ADR-0033): callee's + callback's throws propagate, re-filtered by guards.
    HigherOrder { callee: NameRef, callbacks: Vec<(usize, CallbackRef)>, arg_count: usize },
    /// A direct `$fn()` call resolved to a known callback — throw analogue of [`EffectOrigin::Callback`] (ADR-0033).
    Callback { cbref: CallbackRef },
}

/// One throw-relevant construct in a function/method body, with ordered
/// `try`/`catch` guards that may dam it (ADR-0040); computed for *all* functions/methods.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct ThrowOrigin {
    pub kind: ThrowKind,
    /// The span of the throwing/calling construct (diagnostic position).
    pub span: Span,
    /// Enclosing `try` catch-guards, **innermost first**. `finally` bodies and a
    /// try's own catch bodies do **not** carry that try's own guard (omitted).
    pub guards: Vec<Vec<CatchClause>>,
}

/// A recognized effect-envelope declaration (ADR-0005/0006/0018): the upper
/// bound of effects a function/method promises not to exceed. `labels` are
/// hierarchical dot-path labels (ADR-0018); empty = tightest bound
/// (`#[\Steins\Pure]`), non-empty from `#[\Steins\Effect(...)]`. Both present →
/// `Pure` wins, no contradiction fires.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct EffectEnvelope {
    /// The declared effect labels (ADR-0018 dot-paths). Empty = `Pure`.
    pub labels: Vec<String>,
    /// Span of the recognized attribute (diagnostic position, e.g. `effect.unknown-label`).
    pub span: Span,
}

/// A user-defined function declaration (top-level or namespaced); `name` is the simple name as written.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct FunctionDecl {
    pub name: String,
    /// Fully-qualified, lowercase-normalized name (PHP names case-insensitive); project index keys on this.
    pub fqn: String,
    pub params: Vec<Param>,
    /// Native scalar/union return type, `None` if untyped/non-scalar/`void`/`never` (return check skips, zero-FP).
    pub ret: Option<NativeType>,
    // untyped surface (ADR-0078, issue #200)
    /// File byte span of the native **return** type hint as written, `None` if
    /// none written — return-side twin of [`Param::hint_span`] (`void`, `never`,
    /// `array` etc. are real declarations [`Self::ret`] models as `None`).
    pub ret_span: Option<Span>,
    // end untyped surface (ADR-0078, issue #200)
    pub span: Span,
    /// File byte span of the function's **body block**, braces included —
    /// [`Self::span`] is only the *name* span. Exists for the ADR-0032
    /// argument-pass carry gate (issue #295): PHP locals are **lexical**; any
    /// non-lexical escape (`$$v`, `extract`/`compact`, `eval`, `include`,
    /// `global`, by-ref `use`) sets [`Scope::poisoned`] — a token scan over
    /// this span is a sound "callee cannot reach it" oracle.
    pub body_span: Span,
    /// Recognized `#[\Steins\Pure]`/`#[\Steins\Effect(...)]` envelope, if present
    /// (ADR-0005/0006/0018). `Some` opts into always-on checking (conservative recognition).
    pub effect_envelope: Option<EffectEnvelope>,
    /// Every structural effect-origin candidate in body order ([`EffectOrigin`]).
    /// Computed for *all* functions — effects propagate to `Pure` callers regardless of annotations.
    pub effect_origins: Vec<EffectOrigin>,
    /// Every throw-relevant construct in the body with its try/catch guards (ADR-0040); computed for *all* functions.
    pub throw_origins: Vec<ThrowOrigin>,
    /// Raw `/** … */` docblock immediately preceding this declaration (ADR-0029 adjacency); phpdoc bridge parses `@param`/`@return`.
    pub docblock: Option<String>,
    /// File byte span of the docblock, when adopted — `docblock` text is the
    /// exact substring; used by the transform engine (ADR-0034) to delete a
    /// promoted `@param` line.
    pub docblock_span: Option<Span>,
    /// `true` when declared inside a conditional/nested context (function
    /// analogue of [`ClassDecl::conditional`], ADR-0049 A2i) — the arity check
    /// re-dams until the whole-universe dam is clear.
    pub conditional: bool,
}

/// A method's declared visibility; absent modifiers default to `Public` (PHP semantics).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub enum Visibility {
    Public,
    Protected,
    Private,
}

/// The late-static-binding return keyword a method declares in return position
/// (bare `self`/`static`/`parent`, ADR-0043 amendment). `lower_method` has no
/// class context yet, so only kind + nullability are recorded; FQN-stamping resolves the bound later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub enum RetBoundKind {
    /// `: self` — bound is the enclosing class directly (not late-bound).
    SelfKw,
    /// `: static` — enclosing class as the *minimum* late-bound class (every
    /// late-bound `T` is-a the enclosing class, so it's a necessary bound).
    Static,
    /// `: parent` — bound is the resolved `extends` parent.
    Parent,
}

/// Recorded return-position LSB keyword shape (kind + nullability), before the
/// enclosing-class context resolves it to a bound (ADR-0043 amendment §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct RetBoundKeyword {
    pub kind: RetBoundKind,
    /// `true` when the hint was `?self`/`?static`/`?parent` (nullable bound also accepts `null`).
    pub nullable: bool,
}

/// A user-defined method declaration — class-world analogue of [`FunctionDecl`],
/// carrying the same data plus dispatch modifiers (ADR-0001).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct MethodDecl {
    /// Simple method name as written (case preserved; matching is case-insensitive).
    pub name: String,
    pub params: Vec<Param>,
    /// Native scalar/union return type, `None` if untyped/non-scalar/`void`/
    /// `never` (zero-FP); bare/nullable `self`/`static`/`parent` synthesized in the FQN-stamping pass (ADR-0043).
    pub ret: Option<NativeType>,
    /// Recorded LSB return-keyword shape (`self`/`static`/`parent`, nullable-aware),
    /// consumed by the FQN-stamping pass to synthesize [`Self::ret`]; `None` otherwise.
    pub ret_bound_keyword: Option<RetBoundKeyword>,
    // untyped surface (ADR-0078, issue #200)
    /// File byte span of the native return type hint, `None` if none written —
    /// method-world twin of [`FunctionDecl::ret_span`].
    pub ret_span: Option<Span>,
    // end untyped surface (ADR-0078, issue #200)
    /// The span of the method name identifier (for diagnostic positions).
    pub span: Span,
    /// File byte span of the method's **body block**, braces included — the
    /// method-world twin of [`FunctionDecl::body_span`], and `None` exactly where
    /// there is no body to span (an `abstract` method, an interface method).
    ///
    /// Exists for the same reason: a token scan over the body's source text is a
    /// sound over-approximating oracle where the linear trace is not, because the
    /// trace drops nested sub-expressions. Its heap-world reader is ADR-0086 §4's
    /// stale-default gate — a literal property default only survives into a `new`
    /// allocation when the constructor's own text never spells `$this->{prop}`.
    pub body_span: Option<Span>,
    /// The recognized effect envelope, if declared (see [`FunctionDecl`]).
    pub effect_envelope: Option<EffectEnvelope>,
    /// Structural effect-origin candidates in the body (see [`EffectOrigin`]); empty for abstract methods.
    pub effect_origins: Vec<EffectOrigin>,
    /// Throw-relevant constructs with try/catch guards (ADR-0040); empty for abstract methods.
    pub throw_origins: Vec<ThrowOrigin>,
    pub visibility: Visibility,
    pub is_static: bool,
    pub is_final: bool,
    pub is_abstract: bool,
    /// `true` iff the method name is `__construct` (case-insensitive).
    pub is_constructor: bool,
    /// Raw `/** … */` docblock immediately preceding this method (same adjacency as [`FunctionDecl::docblock`]).
    pub docblock: Option<String>,
    /// **File byte span** of the docblock — method-world analogue of
    /// [`FunctionDecl::docblock_span`]; retained for the transform engine (ADR-0034/0043 §6).
    pub docblock_span: Option<Span>,
}

/// A class property declaration (ADR-0036 object state): plain `public int $x
/// = 0;` members and **promoted constructor parameters** alike.
///
/// Static properties are lowered for a complete class surface but **never
/// tracked in the heap** (global state, ADR-0036).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct PropertyDecl {
    /// Property name without the leading `$`.
    pub name: String,
    /// Native scalar/union type, `None` if untyped/non-scalar/complex (mismatch check skips `None`-typed props).
    pub ty: Option<NativeType>,
    // untyped surface (ADR-0078, issue #200)
    /// File byte span of the native property type hint, `None` if none written —
    /// property-world twin of [`Param::hint_span`] (promoted param's own hint span).
    pub hint_span: Option<Span>,
    // end untyped surface (ADR-0078, issue #200)
    /// `true` when declared `readonly` (or promoted `readonly` ctor param); once established, sweep-immune (ADR-0036).
    pub readonly: bool,
    /// `true` for a `static` property — lowered but never heap-tracked.
    pub is_static: bool,
    pub visibility: Visibility,
    /// `true` when the declaration (or promoted param) carries a default value (`= …`).
    pub has_default: bool,
    /// Lowered default value when representable (literal/array/…); non-representable defaults are `None` (starts unknown).
    pub default: Option<ArgValue>,
    /// `true` for a promoted constructor parameter — checked as ctor arguments,
    /// so property-assign check skips it to avoid a double-report (ADR-0036).
    pub promoted: bool,
    /// `true` for a PHP 8.4 property hook (`get`/`set`), promoted or class-body
    /// (FP class 16). A hook is arbitrary user code, so a hooked property
    /// **binds no value fact ever** — excluded from every value/mismatch check.
    /// Class-body hooked properties are dropped entirely at lowering; this flag
    /// only ever carries the promoted-param case.
    pub hooked: bool,
    /// Raw `/** … */` docblock preceding a plain property (`@var` extraction);
    /// `None` for promoted params, which carry `@param` on the ctor instead.
    pub docblock: Option<String>,
    pub span: Span,
}

/// One case of a lowered `enum` (ADR-0043): case name plus backed value **when
/// representable literal** (`case A = 1;`); unit cases/non-literal initializers
/// carry `value: None`. Not a heap-tracked property — a class constant holding
/// an object of the enum class — so it lives here, off the property path.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct EnumCaseDecl {
    /// The case name as written (e.g. `Hearts`).
    pub name: String,
    /// Backed value when a representable literal; `None` for unit cases or non-literal initializers.
    pub value: Option<ArgValue>,
    pub span: Span,
}

// untyped surface (ADR-0078, issue #200)
/// One class constant's **declaration shape** — facts neither [`ClassDecl::consts`]
/// (values) nor [`ClassDecl::const_visibility`] (visibility) carries. Records
/// every declared constant regardless of initializer; enum cases live in
/// [`ClassDecl::enum_cases`], not here.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct ClassConstDecl {
    /// The constant name as written (constant names are case-sensitive).
    pub name: String,
    /// File byte span of the PHP 8.3 native constant type hint (`const string FOO = 'x';`), `None` if none written.
    pub hint_span: Option<Span>,
    /// Raw `/** … */` docblock preceding the `const` (read for `@var`), shared by
    /// every item of a multi-item `const A = 1, B = 2;` declaration.
    pub docblock: Option<String>,
    /// The span of the constant name identifier (for diagnostic positions).
    pub span: Span,
}
// end untyped surface (ADR-0078, issue #200)

/// A user-defined class, **interface**, or **enum** declaration (top-level or
/// namespaced). Interfaces are lowered (ADR-0033 Liskov, [`Self::is_interface`]);
/// enums are lowered (ADR-0043) with [`Self::enum_cases`] + [`Self::enum_backing`],
/// method bodies unanalyzed ([`Self::methods`] empty, still class-indexed).
/// A class *using* a trait sets [`ClassDecl::uses_traits`] so resolution gives up.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct ClassDecl {
    /// Simple (unqualified) class name as written at the declaration site (used for diagnostics).
    pub name: String,
    /// Fully-qualified, lowercase-normalized name; project index keys on this.
    pub fqn: String,
    /// Resolved FQN with source-declared casing preserved (no leading `\`) —
    /// diagnostics/dump only, mirrors [`TypeMember::Instance`]'s `display`.
    pub display: String,
    pub is_final: bool,
    /// `true` for an `abstract class` — `new AbstractC()` raises `Error` before
    /// ctor `ArgumentCountError`, so the arity family (ADR-0049 §6) silences ctor claims.
    pub is_abstract: bool,
    /// `true` for an `interface` (not a `class`). Interface methods are
    /// abstract; they carry envelopes/`@throws` but no bodies.
    pub is_interface: bool,
    /// `true` for an `enum` (ADR-0043, implicitly `final`); cases/backing in
    /// [`Self::enum_cases`]/[`Self::enum_backing`]; implicitly implements `UnitEnum`/`BackedEnum`.
    pub is_enum: bool,
    /// `true` for a `trait` (ADR-0049 §5, C8/A2i): enters the class-*like* index
    /// as a **name** only, inert — sharing an FQN with a class makes both `Ambiguous`.
    pub is_trait: bool,
    /// `true` when nested under anything but namespace/program (ADR-0049 A2i);
    /// runtime load order decides which binds, so a chain with one **re-dams**.
    pub conditional: bool,
    /// A backed enum's backing scalar (`enum E: int`/`string`), or `None` for a pure (unit) enum.
    pub enum_backing: Option<ScalarType>,
    /// The enum's cases (empty for non-enums). See [`EnumCaseDecl`].
    pub enum_cases: Vec<EnumCaseDecl>,
    /// The `extends` parent as written, resolved to an FQN and walked; undefined
    /// leaves the chain incomplete (unknown→silent). Interface: first extended (rest in `implements`).
    pub parent: Option<NameRef>,
    /// Interfaces this class `implements` (ADR-0033 Liskov carriers); for an
    /// interface, those it `extends` beyond the first. Resolved to FQN at use time.
    pub implements: Vec<NameRef>,
    pub methods: Vec<MethodDecl>,
    /// The class's properties (plain members + promoted constructor params;
    /// ADR-0036). Static properties are included but never heap-tracked.
    pub properties: Vec<PropertyDecl>,
    /// Class constants with a **literal** initializer, `(name, value)` pairs
    /// (ADR-0043 §2) — non-literal omitted (absence ≠ "no such constant"); case-sensitive.
    pub consts: Vec<(String, ArgValue)>,
    // inaccessible members (ADR-0078, issue #185)
    /// **Declared visibility** of every class constant, `(name, visibility)` —
    /// separate from [`Self::consts`] since non-literal constants are recorded here but not there.
    pub const_visibility: Vec<(String, Visibility)>,
    // untyped surface (ADR-0078, issue #200)
    /// Every declared class constant's declaration shape (see [`ClassConstDecl`]),
    /// in source order. Populated for classes, interfaces and enums alike.
    pub const_decls: Vec<ClassConstDecl>,
    // end untyped surface (ADR-0078, issue #200)
    /// Names of class-body **hooked** properties this drops — a hooked `$p`
    /// binds no value, yet **overrides an inherited plain property** (PHP 8.5.9).
    pub hooked_properties: Vec<String>,
    // end inaccessible members (ADR-0078, issue #185)
    // member absence (ADR-0078, issue #197)
    /// `true` when the declaration carries `#[AllowDynamicProperties]` — PHP 8.2
    /// deprecated undeclared-property writes, but this re-opens the property set;
    /// `property.undefined` treats it as an obstacle anywhere in the chain, like `__get`.
    pub allows_dynamic_properties: bool,
    // end member absence (ADR-0078, issue #197)
    /// `true` if the class `use`s any trait — trait bodies live elsewhere, so a
    /// trait-using class is treated as unresolvable (give up → silent).
    pub uses_traits: bool,
    /// Raw `/** … */` docblock preceding the class-like, if any — read for
    /// class-level `@template` names shadowing same-named classes (issue #5). `None` for a trait.
    pub docblock: Option<String>,
    /// **File byte span** of that docblock — class-world analogue of
    /// [`FunctionDecl::docblock_span`], used by the transform engine's interop envelope write.
    pub docblock_span: Option<Span>,
    /// The span of the class name identifier.
    pub span: Span,
}

/// A representable call argument or assignment right-hand side. The first five
/// variants are *literals* — concrete, self-evident values.
/// [`ArgValue::Var`]/[`ArgValue::Call`]/[`ArgValue::MethodCall`] are
/// value-propagation carriers (ADR-0001), not proven on their own — resolved
/// against a per-scope linear trace. Everything else lowers to
/// [`ArgValue::Other`].
///
/// What a **call** in value position still lowers to `Other` (issue #386, the
/// list `docs/type-specification/not-implemented.md` states for users), each
/// because this vocabulary has no way to say it: a dynamic receiver or method
/// name ([`Callee::Dynamic`] — `$o->$m()`, `$obj[0]->m()`, `$var::m()`), a
/// receiver deeper than one property hop (depth 1 is a [`Receiver::Prop`],
/// carried here and declined as a dispatch target by ADR-0052 §7), an argument
/// list carrying a **spread** (its positional prefix is not the call), and a
/// method **first-class callable** (`$o->m(...)` is a value, not a call — see
/// [`ClosureRef::FunctionName`], which carries the free-function form only).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub enum ArgValue {
    Int(i64),
    // Persisted as IEEE bits (`u64`), not a JSON number: JSON has no spelling
    // for the non-finite floats a literal like `1e999` lowers to (serde_json
    // would write `null` and the round-trip would refuse itself), and bits are
    // exact for every value including them.
    Float(#[cfg_attr(feature = "persist", serde(with = "crate::persist::f64_bits"))] f64),
    /// A PHP string literal's value — a byte string, not a Rust `String` (ADR-0080).
    Str(PhpStr),
    Bool(bool),
    Null,
    /// A bare local variable reference `$name` (name stored without the `$`).
    Var(String),
    /// A call `name(args...)` to a statically-named function. `name` is the
    /// **last segment** (no namespace survives). Zero-arg calls resolve through
    /// the constant-function lane, calls with arguments through the T0
    /// binding-descent summary (issue #60); a foldable builtin's own-call
    /// argument resolves the same way (issue #127: `strtoupper(g(1))` folds).
    Call(String, Vec<ArgValue>),
    /// A **method or static** call in value position (issue #386): `$b->m(…)`,
    /// `$b?->m(…)`, `Foo::m(…)`, `(new C(1))->m(…)`. Carries the statement
    /// vocabulary verbatim — the same [`Callee`] the trace's [`CallExpr`] holds,
    /// so the value lane and the statement lane resolve one target through one
    /// `resolve_call_target` and walk one body once (ADR-0075 §3 as amended).
    ///
    /// [`Callee::Function`] never appears here — a free function stays
    /// [`ArgValue::Call`], which carries a simple name and resolves by a rule of
    /// its own. `named` holds the named arguments; the binding descent is
    /// positional-only and declines on them, exactly as `f(x: 1)` is declined.
    ///
    /// The **constant-expression** lowerings (a parameter default, a property
    /// default, a class-constant or enum-case initializer) keep whatever
    /// the value-lane lowering returns that is not [`Self::Other`], and are given no
    /// guard against this variant: PHP forbids a method call in a constant
    /// expression outright, so only unparseable-as-PHP source could put one there,
    /// and every consumer of those slots (`is_literal`, the domain conversion, the
    /// literal resolution) declines it anyway. A guard would refuse what cannot
    /// arrive.
    MethodCall { callee: Callee, args: Vec<ArgValue>, named: Vec<NamedArg> },
    /// `new ClassName(args...)` — construction rvalue. [`NameRef`] resolves to
    /// an FQN at use time, so `$x = new Foo(...)` records `$x`'s exact class.
    /// Third field is the ctor's **named** args (promoted-property seeding binds by name too).
    New(NameRef, Vec<ArgValue>, Vec<NamedArg>),
    /// An array literal `[...]`/`array(...)` whose keys are all literal-or-absent
    /// and whose elements recursively lower (ADR-0001). A spread, unrepresentable
    /// element, or non-literal key lowers the **whole** array to [`ArgValue::Other`].
    /// Keys carry PHP key-normalization; `Auto` keys resolve during
    /// [`normalize_array`] (last-wins on duplicates).
    Array(Vec<(ArrayKey, ArgValue)>),
    /// A ternary `$c ? A : B` in rvalue position, a **conditional value**
    /// (ADR-0031): evaluates `cond`, resolves to the chosen arm when decided,
    /// else joins both. Short-ternary `?:`/`??` aren't lowered here. Arm
    /// spans record the unevaluated one dead (ADR-0052 §6); excluded from [`Hash`].
    Ternary {
        cond: Box<CondExpr>,
        then_val: Box<ArgValue>,
        then_span: Span,
        else_val: Box<ArgValue>,
        else_span: Span,
    },
    /// A closure value (ADR-0033): `function (...) use (...) {...}`/arrow
    /// `fn(...) => …` lowered to its own [`Scope`], or a first-class callable
    /// naming a function target — lets `$f(...)` resolve by binding descent.
    Closure(ClosureRef),
    /// A property read `$var->prop` in rvalue position (ADR-0036). Only a
    /// **simple variable receiver** is represented (`$this->p` → `var = "this"`);
    /// a chain/dynamic name lowers to [`ArgValue::Other`]. Resolved against the heap.
    PropFetch { var: String, prop: String },
    /// `clone $var` (ADR-0036): shallow copy, minting a new allocation id with a
    /// copy of the source's props — post-clone writes don't cross. Bare-variable
    /// operand only; `clone <expr>` lowers to [`ArgValue::Other`].
    Clone(String),
    /// A class-constant/enum-case access `Class::NAME` (ADR-0043): class portion
    /// plus constant/case name — syntactically identical to an enum-case;
    /// **unproven** until inference resolves it (→ [`ArgValue::EnumCase`] or a literal constant).
    ClassConst(StaticClass, String),
    /// An enum-case object `Enum::Case` (ADR-0043): resolved lowercase FQN +
    /// case name, produced when [`ArgValue::ClassConst`] resolves against an enum. Not a scalar.
    EnumCase(String, String),
    /// A null-coalescing rvalue `$a ?? $b` (ADR-0052 §6): `$a` when
    /// set-and-non-null, else `$b`, as `clear_null(fact($a)) join fact($b)`.
    /// Reached only when both operands lower to a representable value — an
    /// unspellable operand (e.g. `$arr['k']`) yields no fact; `?:` still widens
    /// to `Other`. Third field is the RIGHT operand's extent: a proven left
    /// means PHP never evaluates it, recorded dead. Excluded from [`Hash`].
    Coalesce(Box<ArgValue>, Box<ArgValue>, Span),
    /// An array/offset read `$base[$key]` in **rvalue** position (ADR-0049 §7/S3).
    /// Never proven: resolves `base` to a `Fact` and `key` to a value, judging
    /// `offset.missing`/`offset.on-unsupported` **only in whitelisted read
    /// contexts** (assignment-RHS, return operands); elsewhere a silence carrier.
    OffsetRead { base: Box<ArgValue>, key: Box<ArgValue> },
    /// A string concatenation `$a . $b` (issue #59). Lowered **structurally**,
    /// not folded — operands commonly include a [`Self::Var`] whose value only
    /// the walk knows. Left-nested (`a . b . c` is `Concat(Concat(a, b), c)`),
    /// matching PHP associativity. Not itself proven — resolves only when both
    /// operands' string cast is *total and environment-independent* (see
    /// `concat_cast`; `float` excluded). A compound `.=` lowers to [`Self::Other`].
    Concat(Box<ArgValue>, Box<ArgValue>),
    /// A bare **global-constant fetch** (`PREG_SET_ORDER`, `SOME_CONST`) in value
    /// position (issue #168), **unproven** like [`ArgValue::ClassConst`] — PHP
    /// resolves unqualified constants namespace-first; only the issue #29 shadow discipline may read it.
    GlobalConst(NameRef),
    /// A binary-operator expression in **value** position (issue #260), lowered
    /// structurally like [`Self::Concat`]/[`Self::Coalesce`] — not itself proven.
    /// Only [`ValueOp`]-representable operators reach here; others lower to [`Self::Other`].
    Binary { op: ValueOp, lhs: Box<ArgValue>, rhs: Box<ArgValue> },
    /// `isset(<operand>, …)` in **value** position (issue #579): `$b =
    /// isset($a['k']);`, `return isset($x);`, `f(isset($x))`.
    ///
    /// The condition side of this construct has had a representation since ADR-0062
    /// S4 ([`CondExpr::Isset`]) and issue #414 ([`CondExpr::IssetVar`]); the value
    /// side had none, so every read of an `isset` answered nothing — not even the
    /// `bool` PHP guarantees. `isset` is a construct, not a call, so no call seam
    /// could be asked for it either.
    ///
    /// **Total by construction, and that is the whole design.** `isset` evaluates
    /// to a `bool` whatever it tests, so an operand this vocabulary cannot spell
    /// must still arrive here as [`IssetOperand::Unmodelled`] rather than widen the
    /// expression to [`Self::Other`] — widening is exactly the defect. The
    /// operands are PHP's own conjunction (`isset($a, $b)` is `isset($a) &&
    /// isset($b)`), and the fact seam answers them as one.
    ///
    /// `empty(…)` is deliberately NOT lowered here: its verdict wants a truthiness
    /// reading of the operand's value stacked on the presence one, which is a
    /// question this carrier does not ask.
    Isset(Vec<IssetOperand>),
    Other,
}

/// One operand of a value-position [`ArgValue::Isset`] (issue #579).
///
/// Not [`CondExpr`] under another name, though the guard side spells the same two
/// shapes: this vocabulary is **total** over what `isset` may be written with,
/// because the value seam must answer every `isset` and the guard seam may
/// decline one. `CondExpr::Opaque`'s read set has no counterpart here for the
/// reason issue #414 gave [`CondExpr::IssetVar`] its own variant — `isset` is a
/// construct and cannot write what it tests, so there is nothing to forget.
#[derive(Debug, Clone, PartialEq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub enum IssetOperand {
    /// `isset($var)` — the question about a **binding** rather than a key.
    Var(String),
    /// `isset($var[<key>])` — depth exactly one, over a bare-variable base.
    ///
    /// The key is whatever the value IR spells it as, NOT the concrete literal the
    /// guard form requires: [`CondExpr::Isset`]'s literal-key restriction is
    /// ADR-0062 A-G4's tag-discrimination scope, while this operand is read
    /// through the offset family's own key resolution, which resolves a proven
    /// variable key and declines an unproven one.
    Offset { var: String, key: Box<ArgValue> },
    /// An operand this vocabulary does not spell — a property (`$o->p`), a static
    /// property, a deeper path (`$a['x']['y']`), a dynamic name. It contributes
    /// `Maybe` to the conjunction, which is what keeps the whole expression at the
    /// `bool` floor instead of `unknown`.
    Unmodelled,
}

impl IssetOperand {
    /// Render the operand as it appears in a diagnostic message.
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            IssetOperand::Var(v) => format!("${v}"),
            IssetOperand::Offset { var, key } => format!("${var}[{}]", key.render()),
            IssetOperand::Unmodelled => "<expr>".to_owned(),
        }
    }
}

/// Identifies the target of an [`ArgValue::Closure`] (ADR-0033): an anonymous
/// closure/arrow's own [`Scope`] (by definition offset), or a named free
/// function. `captures` lists only names — snapshots taken at closure-creation time (PHP's by-value capture).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub enum ClosureRef {
    /// A closure/arrow with its own scope at `def_offset` (matches
    /// [`ScopeOwner::Closure`]); `captures` are by-value captured names.
    Anonymous { def_offset: u32, captures: Vec<String> },
    /// A first-class callable of a named free function: `strtolower(...)`.
    /// Method/static first-class callables lower to [`ArgValue::Other`].
    FunctionName(NameRef),
}

/// A lowered array-literal key. `Auto` is an absent key, resolved to its next
/// integer position by [`normalize_array`]; `Int`/`Str` are already-normalized.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub enum ArrayKey {
    /// An absent key — normalized to the next integer position.
    Auto,
    /// An integer key (already PHP-normalized: integer-like string keys, floats,
    /// and bools all fold to this).
    Int(i64),
    /// A string key that is not integer-like. A byte string (ADR-0080).
    Str(PhpStr),
    /// **A key the source does not spell as a literal** (issue #336): `[$k =>
    /// $v]`, `[f() => $v]`, `[FOO => $v]`. Previously such a key demoted the
    /// **whole** literal to [`ArgValue::Other`]; now the key expression is kept.
    /// Never normalized — [`normalize_array`] declines a literal containing one
    /// (an unknown key may be an integer, shifting every following `Auto`).
    Expr(Box<ArgValue>),
}

/// A fully PHP-normalized array key (no `Auto`): the runtime key an entry occupies.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NormKey {
    Int(i64),
    Str(PhpStr),
}

impl NormKey {
    /// Render the key for a compact array message (`5`, `'foo'`).
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            NormKey::Int(i) => i.to_string(),
            NormKey::Str(s) => s.to_php_literal(),
        }
    }
}

/// The PHP minor that changed the next-auto-index edge case for negative keys.
const NEXT_INT_RULE_CHANGED_IN: (u16, u16) = (8, 3);

/// PHP's next-auto-index rule for an omitted array key (`[$a, $b]`); the two
/// variants differ **only** when every integer key seen so far is negative.
/// PHP 8.3 changed the edge case: before it, next-auto-index floored at `0`
/// (`[-5 => 'a', 'b']` put `'b'` at `0`); from 8.3, one past the largest
/// integer key seen (`'b'` lands at `-4`). Verified PHP 8.5.8: `php -r
/// 'var_export([-5=>"a","b"]);'` → `-5, -4`. Steins' floor is 8.1 (ADR-0011):
/// 8.1/8.2 take [`NextIntRule::FloorAtZero`], 8.3+ take [`NextIntRule::MaxPlusOne`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NextIntRule {
    /// PHP < 8.3 (Steins' floor 8.1 through 8.2): the next auto-index never goes below `0`.
    FloorAtZero,
    /// PHP >= 8.3: one past the largest integer key seen, negative or not.
    MaxPlusOne,
}

impl NextIntRule {
    /// The rule a project on PHP `(major, minor)` follows.
    #[must_use]
    pub fn for_minor(minor: (u16, u16)) -> Self {
        if minor >= NEXT_INT_RULE_CHANGED_IN { Self::MaxPlusOne } else { Self::FloorAtZero }
    }
}

/// Whether `items` normalizes *differently* under the two [`NextIntRule`]s —
/// an omitted key falls where every integer key seen so far is negative, so
/// pre-8.3 floor and 8.3+ max+1 disagree. Exactly the ambiguity
/// [`normalize_array`] declines when the PHP minor is unknown; purely syntactic.
#[must_use]
pub fn next_int_is_version_dependent(items: &[(ArrayKey, ArgValue)]) -> bool {
    let mut max_seen: Option<i64> = None;
    for (k, _) in items {
        match k {
            ArrayKey::Auto => {
                // `None` → position 0 under both rules. Otherwise the rules split
                // exactly when one past the running max is still negative.
                let next = max_seen.map_or(0, |m: i64| m.saturating_add(1));
                if next < 0 {
                    return true;
                }
                max_seen = Some(max_seen.map_or(next, |m| m.max(next)));
            }
            ArrayKey::Int(i) => max_seen = Some(max_seen.map_or(*i, |m| m.max(*i))),
            ArrayKey::Str(_) => {}
            // An unknown key may be an integer, so it may move the running max
            // — which makes every following `Auto` position unresolvable under
            // *either* rule, not merely different between them. Answering
            // `true` routes the literal to `normalize_array`'s decline, which
            // is where an unresolvable key set belongs (issue #336).
            ArrayKey::Expr(_) => return true,
        }
    }
    false
}

/// Next PHP auto-index for an omitted array key, given the running max integer
/// key seen (`None` → `0`) and the [`NextIntRule`] in force. Saturating: at
/// `i64::MAX` PHP refuses to append; the clamped index collides and last-wins
/// folds it. Shared by [`normalize_array_with`] and [`duplicate_array_keys`]
/// (issue #187), which need the same arithmetic without the last-wins fold.
#[must_use]
fn next_auto_index(max_seen: Option<i64>, rule: NextIntRule) -> i64 {
    let mut i = max_seen.map_or(0, |m: i64| m.saturating_add(1));
    if matches!(rule, NextIntRule::FloorAtZero) {
        i = i.max(0);
    }
    i
}

/// Resolve an array literal under an explicit [`NextIntRule`]: next-int
/// assignment for `Auto` keys, **last-wins** for duplicates (PHP semantics,
/// insertion-ordered). Prefer [`normalize_array`], which picks the rule from
/// the PHP minor and declines to guess; use this only where the rule is known
/// or the result isn't a proof-layer premise.
#[must_use]
pub fn normalize_array_with(
    items: &[(ArrayKey, ArgValue)],
    rule: NextIntRule,
) -> Vec<(NormKey, ArgValue)> {
    let mut out: Vec<(NormKey, ArgValue)> = Vec::with_capacity(items.len());
    // PHP's next auto-index: one past the largest integer key seen so far,
    // explicit or auto (verified: `[5=>'a',5=>'b','c']` → 5, 6). `None` → 0.
    let mut max_seen: Option<i64> = None;
    for (k, v) in items {
        let key = match k {
            ArrayKey::Auto => {
                let i = next_auto_index(max_seen, rule);
                max_seen = Some(max_seen.map_or(i, |m| m.max(i)));
                NormKey::Int(i)
            }
            // Unreachable through `normalize_array`, which declines a literal
            // holding one (issue #336). Stopping here is the honest total
            // answer — entries after an unknown key have unknown positions.
            ArrayKey::Expr(_) => return out,
            ArrayKey::Int(i) => {
                max_seen = Some(max_seen.map_or(*i, |m| m.max(*i)));
                NormKey::Int(*i)
            }
            ArrayKey::Str(s) => NormKey::Str(s.clone()),
        };
        // Last-wins: update in place if the key already occupies a slot.
        if let Some(slot) = out.iter_mut().find(|(ek, _)| *ek == key) {
            slot.1 = v.clone();
        } else {
            out.push((key, v.clone()));
        }
    }
    out
}

/// Resolve an array literal's raw `(ArrayKey, value)` entries to their PHP
/// runtime key→value map, picking the next-auto-index rule from the project's
/// PHP minor (ADR-0049 A12; `Folder::php_minor()`'s `(major, minor)`, `None`
/// if unanswered). Returns `None` only when unknown *and* the literal straddles
/// the 8.3 rule change; version-independent literals still answer.
#[must_use]
pub fn normalize_array(
    items: &[(ArrayKey, ArgValue)],
    php_minor: Option<(u16, u16)>,
) -> Option<Vec<(NormKey, ArgValue)>> {
    // A key the source did not spell as a literal is unresolvable under EVERY
    // rule (issue #336) — it may be an integer or collide with a written key,
    // so no PHP version resolves it; checked before consulting the minor.
    if items.iter().any(|(k, _)| matches!(k, ArrayKey::Expr(_))) {
        return None;
    }
    match php_minor {
        Some(m) => Some(normalize_array_with(items, NextIntRule::for_minor(m))),
        None if next_int_is_version_dependent(items) => None,
        // The rules agree on this literal, so either one resolves it.
        None => Some(normalize_array_with(items, NextIntRule::MaxPlusOne)),
    }
}

// ---------------------------------------------------------------------------
// `array.duplicate-key` (ADR-0078, issue #187): duplicate literal array keys.
// Purely syntactic — no inference, no value evaluation — so [`ArrayLiteralSite`]
// is collected file-wide (like [`ForeachSite`]) rather than riding the
// [`ArgValue::Array`] lowering, which bails the WHOLE literal to `Other` on one
// bad value. Keys alone matter: `[1 => 'a', 1 => $x]` still has a provable duplicate.
// ---------------------------------------------------------------------------

/// One element of a literal array expression, reduced to what
/// [`duplicate_array_keys`] needs (issue #187): resolved key + own span.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct ArrayLiteralElement {
    /// `Some(ArrayKey::Auto)` for a bare `value` element; `Some(Int/Str)` for an
    /// explicit key the fold gate resolves (PHP's own coercion: `1`/`'1'`/`1.7`/
    /// `true` → `Int(1)`, `null` → `Str("")`); `None` for an unpinnable key
    /// (variable, call, nested expr), a `...$spread`, or a `list()` hole.
    pub key: Option<ArrayKey>,
    /// The element's own span (`key => value`, or the bare value) — where a
    /// duplicate-key finding naming this element is positioned.
    pub span: Span,
}

/// One literal array expression (`[...]`/`array(...)`), elements in source
/// order — the evidence `array.duplicate-key` needs (issue #187).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct ArrayLiteralSite {
    pub elements: Vec<ArrayLiteralElement>,
}

/// One shadowed-then-overwritten pair `array.duplicate-key` reports (issue
/// #187): `shadowed_span`/`winner_span` are the earlier/later element, `key` the shared [`NormKey`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DuplicateArrayKey {
    pub key: NormKey,
    pub winner_span: Span,
    pub shadowed_span: Span,
}

/// Scan one [`ArrayLiteralSite`]'s elements for PHP-key-equal duplicates
/// (issue #187, `array.duplicate-key`), reusing [`normalize_array`]'s key
/// coercion. Pairing is **adjacent** (each later occurrence reported against
/// the nearest earlier one, matching PHP's in-place overwrite). An unpinnable
/// key (variable, call, spread, destructuring hole) skips itself and every
/// `Auto` element after it — silence, not a guess. `php_minor` selects the
/// rule as [`normalize_array`] does; `None` runs both in parallel per `Auto`
/// element until they disagree. Keys compare as **byte strings** (ADR-0080):
/// four distinct invalid-UTF-8 bytes are four distinct keys, unlike pre-[`PhpStr`].
#[must_use]
pub fn duplicate_array_keys(
    site: &ArrayLiteralSite,
    php_minor: Option<(u16, u16)>,
) -> Vec<DuplicateArrayKey> {
    let known_rule = php_minor.map(NextIntRule::for_minor);
    let mut max_plus_one: Option<i64> = None;
    let mut max_floor_zero: Option<i64> = None;
    let mut poisoned = false;
    let mut last_seen: HashMap<NormKey, Span> = HashMap::new();
    let mut out = Vec::new();

    for el in &site.elements {
        let resolved = match &el.key {
            // A missing key (`list()` hole) and an UNKNOWN key (issue #336)
            // poison the auto chain the same way — neither resolves a position.
            None | Some(ArrayKey::Expr(_)) => {
                poisoned = true;
                None
            }
            Some(ArrayKey::Int(i)) => {
                max_plus_one = Some(max_plus_one.map_or(*i, |m| m.max(*i)));
                max_floor_zero = Some(max_floor_zero.map_or(*i, |m| m.max(*i)));
                Some(NormKey::Int(*i))
            }
            Some(ArrayKey::Str(s)) => Some(NormKey::Str(s.clone())),
            Some(ArrayKey::Auto) if poisoned => None,
            Some(ArrayKey::Auto) => {
                let a = next_auto_index(max_plus_one, NextIntRule::MaxPlusOne);
                let b = next_auto_index(max_floor_zero, NextIntRule::FloorAtZero);
                max_plus_one = Some(max_plus_one.map_or(a, |m| m.max(a)));
                max_floor_zero = Some(max_floor_zero.map_or(b, |m| m.max(b)));
                match known_rule {
                    Some(NextIntRule::MaxPlusOne) => Some(NormKey::Int(a)),
                    Some(NextIntRule::FloorAtZero) => Some(NormKey::Int(b)),
                    None if a == b => Some(NormKey::Int(a)),
                    None => {
                        poisoned = true;
                        None
                    }
                }
            }
        };
        let Some(key) = resolved else { continue };
        if let Some(shadowed_span) = last_seen.insert(key.clone(), el.span) {
            out.push(DuplicateArrayKey { key, winner_span: el.span, shadowed_span });
        }
    }
    out
}

impl Eq for ArgValue {}

impl std::hash::Hash for ArgValue {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        core::mem::discriminant(self).hash(state);
        match self {
            ArgValue::Int(v) => v.hash(state),
            ArgValue::Float(v) => v.to_bits().hash(state),
            ArgValue::Str(v) => v.hash(state),
            ArgValue::Bool(v) => v.hash(state),
            ArgValue::Var(v) => v.hash(state),
            ArgValue::Call(name, args) => {
                name.hash(state);
                args.hash(state);
            }
            // The receiver, method and arguments all denote; the spans a nested
            // `Ternary`/`Coalesce` argument carries do not, and the arm below
            // excludes them by delegating to this very impl (issue #386).
            ArgValue::MethodCall { callee, args, named } => {
                callee.hash(state);
                args.hash(state);
                named.hash(state);
            }
            ArgValue::New(name, args, named) => {
                name.hash(state);
                args.hash(state);
                named.hash(state);
            }
            ArgValue::Array(items) => items.hash(state),
            // The arm spans are position, not denotation — excluded (a narrower
            // hash than `PartialEq` is always sound).
            ArgValue::Ternary { cond, then_val, else_val, .. } => {
                cond.hash(state);
                then_val.hash(state);
                else_val.hash(state);
            }
            ArgValue::Closure(r) => r.hash(state),
            ArgValue::PropFetch { var, prop } => {
                var.hash(state);
                prop.hash(state);
            }
            ArgValue::Clone(v) => v.hash(state),
            ArgValue::Coalesce(l, r, _) => {
                l.hash(state);
                r.hash(state);
            }
            ArgValue::OffsetRead { base, key } => {
                base.hash(state);
                key.hash(state);
            }
            ArgValue::Concat(l, r) => {
                l.hash(state);
                r.hash(state);
            }
            ArgValue::Binary { op, lhs, rhs } => {
                op.hash(state);
                lhs.hash(state);
                rhs.hash(state);
            }
            ArgValue::ClassConst(class, name) => {
                class.hash(state);
                name.hash(state);
            }
            ArgValue::EnumCase(class, case) => {
                class.hash(state);
                case.hash(state);
            }
            ArgValue::GlobalConst(r) => r.hash(state),
            ArgValue::Isset(ops) => ops.hash(state),
            ArgValue::Null | ArgValue::Other => {}
        }
    }
}

impl ArgValue {
    /// Whether this is a concrete literal (`Int`/`Float`/`Str`/`Bool`/`Null`) —
    /// i.e. a self-evident, already-proven value.
    #[must_use]
    pub const fn is_literal(&self) -> bool {
        matches!(
            self,
            ArgValue::Int(_)
                | ArgValue::Float(_)
                | ArgValue::Str(_)
                | ArgValue::Bool(_)
                | ArgValue::Null
        )
    }

    /// Whether this is a **self-evident value**: a scalar literal, or an array
    /// literal whose every element is itself self-evident (recursively) —
    /// extends [`Self::is_literal`] for guard narrowing ([`CondOperand::Literal`])
    /// and the fold seam (ADR-0028). A `Var`/call/offset-read element anywhere
    /// widens the whole array rather than folding it (issue #39, ADR-0002 zero-FP).
    #[must_use]
    pub fn is_concrete_value(&self) -> bool {
        match self {
            ArgValue::Array(items) => items.iter().all(|(_, v)| v.is_concrete_value()),
            v => v.is_literal(),
        }
    }

    /// Render the value as it should appear in a diagnostic message.
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            ArgValue::Int(v) => v.to_string(),
            ArgValue::Float(v) => {
                // Keep a float visibly a float: `5.0`, not `5`.
                if v.fract() == 0.0 && v.is_finite() { format!("{v:.1}") } else { v.to_string() }
            }
            ArgValue::Str(v) => v.render_with('"'),
            ArgValue::Bool(v) => v.to_string(),
            ArgValue::Null => "null".to_owned(),
            ArgValue::Var(v) => format!("${v}"),
            ArgValue::Call(name, _) => format!("{name}()"),
            // `$b->m()` / `$b?->m()` / `(new C())->m()` / `Foo::m()`. Only the two
            // callee forms the variant admits are spelled; the rest are
            // unreachable by construction (the lowering builds no other).
            ArgValue::MethodCall { callee, .. } => match callee {
                Callee::Method { receiver, method, nullsafe } => {
                    let arrow = if *nullsafe { "?->" } else { "->" };
                    format!("{}{arrow}{method}()", receiver.render())
                }
                Callee::Static { class, method } => format!("{}::{method}()", class.render()),
                _ => "<expr>".to_owned(),
            },
            ArgValue::New(name, _, _) => format!("new {}()", name.simple()),
            ArgValue::Array(items) => render_array(items),
            ArgValue::Ternary { then_val, else_val, .. } => {
                format!("(… ? {} : {})", then_val.render(), else_val.render())
            }
            ArgValue::Closure(ClosureRef::FunctionName(n)) => format!("{}(...)", n.simple()),
            ArgValue::Closure(ClosureRef::Anonymous { .. }) => "Closure".to_owned(),
            ArgValue::PropFetch { var, prop } => format!("${var}->{prop}"),
            ArgValue::Clone(v) => format!("clone ${v}"),
            ArgValue::Coalesce(l, r, _) => format!("({} ?? {})", l.render(), r.render()),
            ArgValue::Concat(l, r) => format!("({} . {})", l.render(), r.render()),
            ArgValue::Binary { op, lhs, rhs } => {
                format!("({} {} {})", lhs.render(), op.symbol(), rhs.render())
            }
            ArgValue::OffsetRead { base, key } => format!("{}[{}]", base.render(), key.render()),
            ArgValue::ClassConst(class, name) => format!("{}::{name}", class.render()),
            ArgValue::EnumCase(class, case) => format!("{class}::{case}"),
            ArgValue::GlobalConst(r) => r.raw.clone(),
            ArgValue::Isset(ops) => {
                let parts: Vec<String> = ops.iter().map(IssetOperand::render).collect();
                format!("isset({})", parts.join(", "))
            }
            ArgValue::Other => "<expr>".to_owned(),
        }
    }
}

/// Render an array literal compactly for a diagnostic message: `['a', 'b']`,
/// `['k' => 1]`, list-shaped arrays without keys, truncating with `…` after the
/// first five entries.
fn render_array(items: &[(ArrayKey, ArgValue)]) -> String {
    // Rendering is cosmetic — never a proof-layer premise — so it takes the
    // pinned rule unconditionally (ADR-0049 A12) rather than threading the
    // project minor through `render()`'s config-free surface.
    let normalized = normalize_array_with(items, NextIntRule::MaxPlusOne);
    // A pure list (keys exactly 0..n-1) renders without keys.
    let is_list = normalized
        .iter()
        .enumerate()
        .all(|(i, (k, _))| matches!(k, NormKey::Int(n) if *n == i as i64));
    let mut parts: Vec<String> = Vec::new();
    for (k, v) in normalized.iter().take(5) {
        if is_list {
            parts.push(render_array_value(v));
        } else {
            parts.push(format!("{} => {}", k.render(), render_array_value(v)));
        }
    }
    if normalized.len() > 5 {
        parts.push("…".to_owned());
    }
    format!("[{}]", parts.join(", "))
}

/// Render an array element in PHP-literal style (single-quoted strings, so a
/// rendered array reads like source); non-strings defer to [`ArgValue::render`].
fn render_array_value(v: &ArgValue) -> String {
    match v {
        ArgValue::Str(s) => s.to_php_literal(),
        other => other.render(),
    }
}

/// A single positional call argument.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct Arg {
    pub value: ArgValue,
    pub span: Span,
}

/// A **named argument** (`name: <expr>`) at a call site (ADR-0049 §6 arity).
/// The arity check needs only the bound parameter *name* (case-sensitive match);
/// the phpdoc declared-contract lane also binds the argument's **value**. Makes
/// the call non-[`CallExpr::positional_only`]; positional args stay in [`CallExpr::args`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct NamedArg {
    /// Parameter name bound, without the leading `$` (e.g. `b` in `f(b: 2)`).
    /// PHP compares **case-sensitively** here (`f(A: 1)` on `function f($a)` is fatal).
    pub name: String,
    /// Lowered value bound to the parameter (`2` in `f(b: 2)`) — the
    /// declared-contract lane judges it against the target's `@param` envelope.
    pub value: ArgValue,
    pub span: Span,
}

/// What a [`CallExpr`] is called *on* — the receiver dimension class-world
/// resolution dispatches on (ADR-0001); resolvability depends on receiver exactness (`steins-infer`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub enum Callee {
    /// `f(args...)` — a statically-named function (the last, unqualified name).
    Function(String),
    /// `$recv->m(args...)` / `$recv?->m(args...)` — instance-method call.
    /// `nullsafe` is `true` for `?->` (call on `null` short-circuits), so
    /// `call.on-null` must never fire on it.
    Method { receiver: Receiver, method: String, nullsafe: bool },
    /// `Class::m(args...)` — a static (scope-resolution `::`) call.
    Static { class: StaticClass, method: String },
    /// `new Class(args...)` — a constructor call (`args` are the ctor args).
    /// `class` is the class reference as written (resolved to an FQN at use).
    Construct { class: NameRef },
    /// `$fn(args...)` — call through a bare local variable (ADR-0033). Variable
    /// name retained (no `$`) so the propagation walk can resolve it against the
    /// env (closure fact descends into scope, `Singleton(Str)` resolves as a
    /// function name); unresolved stays honestly opaque.
    DynamicVar(String),
    /// A receiver or method name the lowering cannot represent (dynamic method
    /// name, `$obj[...]->m()`, `$var::m()`, `$arr['x']()`, …). Never resolves.
    Dynamic,
}

/// Object an instance-method call dispatches on, restricted to forms resolution can reason about.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub enum Receiver {
    /// `$this->m()` — inside a class body.
    This,
    /// `$var->m()` — resolvable only when the environment knows `$var`'s exact
    /// class (`$var = new Foo();`).
    Var(String),
    /// `(new Foo(...))->m()` — an exact-class receiver (runtime class is the
    /// referenced class, resolved to an FQN project-wide), carrying the
    /// constructor's own positional and named arguments (issue #386): the
    /// receiver object is minted here, so the constructor summary and the
    /// generic carry the arguments prove are readable at this call — the half
    /// issue #374 measured missing and ADR-0057 C7 deferred to the value IR.
    New { class: NameRef, args: Vec<ArgValue>, named: Vec<NamedArg> },
    /// `$var->prop->m()` — a **depth-1** property-fetch receiver (ADR-0052 §7).
    /// Receiver object is whatever the heap says `$var->prop` holds; only a bare
    /// variable object and static property identifier are represented (a deeper
    /// chain or dynamic name lowers to [`Callee::Dynamic`]). Method target is
    /// **not** resolved from it — every resolution path treats it like `Dynamic`,
    /// while `call.on-null` reads the heap property fact.
    Prop { var: String, prop: String },
}

impl Receiver {
    /// Render the receiver for a diagnostic message — the source spelling as far
    /// as the vocabulary keeps it (a `new`'s arguments are not re-rendered).
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            Receiver::This => "$this".to_owned(),
            Receiver::Var(v) => format!("${v}"),
            Receiver::New { class, .. } => format!("(new {}())", class.simple()),
            Receiver::Prop { var, prop } => format!("${var}->{prop}"),
        }
    }
}

/// The class portion of a static `Class::m()` call, as written.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub enum StaticClass {
    /// Explicit class reference (`Foo::m()`/`Sub\Foo::m()`) — exact, FQN-resolved.
    Named(NameRef),
    /// `self::m()` — the lexical class, resolved under the final/private guard.
    SelfKw,
    /// `static::m()` — late static binding, always unknown (LSB).
    Static,
    /// `parent::m()` — the parent chain, exact.
    Parent,
}

impl StaticClass {
    /// Render the class portion for a diagnostic (simple name for an explicit reference, else the keyword).
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            StaticClass::Named(r) => r.simple().to_owned(),
            StaticClass::SelfKw => "self".to_owned(),
            StaticClass::Static => "static".to_owned(),
            StaticClass::Parent => "parent".to_owned(),
        }
    }
}

/// A function-call (or method / static / constructor call) expression.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct CallExpr {
    /// Simple callee name, if statically-known **function** identifier; `None`
    /// for dynamic/method/static/constructor calls. Full receiver is [`Self::receiver`].
    pub callee: Option<String>,
    /// Full function reference (raw + qualification) when callee is a
    /// statically-known function, for project-wide resolution; `None` otherwise.
    pub callee_ref: Option<NameRef>,
    /// The receiver dimension (function/method/static/constructor). For a plain
    /// function call, [`Callee::Function`] with the same name as [`Self::callee`].
    pub receiver: Callee,
    /// **Positional** arguments in source order (spread `...$x` excluded — see
    /// [`Self::has_spread`]). Full list when [`Self::positional_only`]; else the
    /// positional prefix of a mixed call (`f(1, b: 2)` → `args=[1]`, `named_args=[b]`).
    pub args: Vec<Arg>,
    /// **Named** arguments (`name: <expr>`) in source order (ADR-0049 §6 arity);
    /// empty for a purely positional call, populated so arity can bind named args regardless.
    pub named_args: Vec<NamedArg>,
    /// `true` when the call carries **argument unpacking** (`...$args`) —
    /// spread's cardinality is runtime, so arity stays silent. Also set for
    /// **non-canonical** order (positional after named — a compile error).
    pub has_spread: bool,
    /// `false` if the call used a named or spread argument; existing checks
    /// skip such calls — equivalent to `named_args.is_empty() && !has_spread`,
    /// except the **first-class-callable** shape (`f(...)`), arg-less
    /// non-positional and never a call for arity purposes.
    pub positional_only: bool,
    pub span: Span,
    /// **Guard reading of each positional argument**, index-parallel with
    /// [`Self::args`] — `Some` only where the argument is a condition the
    /// [`CondExpr`] vocabulary models (`isset(…)`, `empty(…)`, `!`/`&&`/`||`
    /// compositions, a constant-key comparison, a named call). [`ArgValue`]
    /// can't express `isset($d['a'])` as a value, but a userland assertion
    /// helper called on it is a guard the ADR-0058 tag lane consumes the same
    /// way — populated purely syntactically. Empty in the common case
    /// (allocates nothing); index via [`Self::arg_cond`] (short vector = all-`None`).
    pub arg_conds: Vec<Option<CondExpr>>,
}

impl CallExpr {
    /// Guard reading of the positional argument at `pos` (see [`Self::arg_conds`]).
    /// `None` if not a modelled condition, out of range, or the call has no guard readings.
    #[must_use]
    pub fn arg_cond(&self, pos: usize) -> Option<&CondExpr> {
        self.arg_conds.get(pos)?.as_ref()
    }
}

/// A comparison operator in a lowered [`CondExpr`] (ADR-0031).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub enum CmpOp {
    /// `===` — strict identity.
    Identical,
    /// `!==` — strict non-identity.
    NotIdentical,
    /// `==` — loose equality (empirically-tabled coercion).
    Loose,
    /// `!=` / `<>` — loose inequality.
    NotLoose,
    /// `<` — less-than (ordering); used for int-range guard refinement
    /// (ADR-0031), decided only for concrete numeric operands, else `Maybe`.
    Lt,
    /// `<=` — less-than-or-equal.
    Le,
    /// `>` — greater-than.
    Gt,
    /// `>=` — greater-than-or-equal.
    Ge,
}

impl CmpOp {
    /// The operator as written, for a rendered value expression.
    #[must_use]
    pub const fn symbol(self) -> &'static str {
        match self {
            CmpOp::Identical => "===",
            CmpOp::NotIdentical => "!==",
            CmpOp::Loose => "==",
            CmpOp::NotLoose => "!=",
            CmpOp::Lt => "<",
            CmpOp::Le => "<=",
            CmpOp::Gt => ">",
            CmpOp::Ge => ">=",
        }
    }
}

/// The operator half of an [`ArgValue::Binary`] — a binary operator carried in
/// **value** position (issue #260), same operator as [`CondExpr::Cmp`]'s guard
/// form (`$b = $x > 3;` vs `if ($x > 3)`); unrepresentable operators stay [`ArgValue::Other`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub enum ValueOp {
    /// A comparison `=== !== == != < <= > >=`. Its value is a `bool`.
    Cmp(CmpOp),
    /// A bitwise or `|` (issue #615): a **flag combination**, `FILTER_A |
    /// FILTER_B`. Carried structurally like the rest of this enum, and — unlike
    /// [`Self::Cmp`] — it reaches **no** fact seam of its own.
    ///
    /// That asymmetry is deliberate, and it is why this variant exists rather
    /// than a total `|` evaluator. `eval_binary_fact` is total for a comparison:
    /// every caller binds its answer unconditionally, because a comparison is a
    /// `bool` whatever its operands are. A bitwise `|` has no such floor —
    /// `int|int` is an `int`, `string|string` is a `string`, and PHP's own GMP
    /// extension overloads the operator to return a **GMP object**, so even
    /// `int|string` would be an unsound claim. So the comparison seam keeps its
    /// totality (its callers match [`Self::Cmp`] and take a [`CmpOp`]), and this
    /// variant is read only where a rule can say what the combination means:
    /// `filter_var`'s flags roster, which resolves by constant NAME and never
    /// needs a value at all.
    BitOr,
}

impl ValueOp {
    /// The operator as written, for a rendered value expression.
    #[must_use]
    pub const fn symbol(self) -> &'static str {
        match self {
            ValueOp::Cmp(op) => op.symbol(),
            ValueOp::BitOr => "|",
        }
    }
}

/// A lowered operand of a [`CondExpr`] comparison (ADR-0031): a bare local
/// variable, a concrete literal value, or anything the lowering can't represent.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub enum CondOperand {
    /// `$name` — a bare local variable (name without the `$`).
    Var(String),
    /// A literal value (`5`, `null`, `"x"`, `true`); non-literal exprs lower to [`Self::Other`].
    Literal(ArgValue),
    /// `$var[<literal>]` — constant-key projection, depth one (ADR-0062 A-G4).
    /// Lets a tagged-union guard subtract array arms by `admits`; only shape-narrowing reads it.
    Offset { var: String, key: Box<ArgValue> },
    /// A bare global-constant fetch (`PHP_VERSION_ID`, `SOME_CONST`), carried as written
    /// (issue #29) so the version-guard fold can resolve it against the target range.
    Const(NameRef),
    /// A class-constant / enum-case fetch `Class::NAME` (issue #429), carried as
    /// written — the [`Self::Const`] shape one scope in. **Unproven**, exactly as
    /// [`ArgValue::ClassConst`] is: inference resolves it against the class index,
    /// and only the enum-case resolution is consumed today (identity narrowing over
    /// the finite case domain). Nothing folds it to a verdict, so a comparison
    /// against one still evaluates `Maybe`.
    ClassConst(StaticClass, String),
    /// Anything else (call, property fetch, arithmetic sub-expression, …) — unrepresentable
    /// for the verdict but never opaque about its effects: under-modeling once let a stale
    /// `$m = []` survive `preg_match($re,$s,$m)===1` as a false `list{}` (issue #158).
    Other {
        /// The call this operand **is**, when statically-resolvable (same recognition as
        /// [`CondExpr::Call`]). `None` for a dynamic callee or one that merely contains a
        /// call (`f($x) + 1`) — that case's invalidation lands through `invalidates`.
        call: Option<Box<CallExpr>>,
        /// Variables a write inside this operand may have rebound (the invalidation set) —
        /// empty unless the subtree contains a writer. `$o->p === $s` writes nothing;
        /// `f($o->p) === $s` may write both.
        invalidates: Vec<String>,
        /// ADR-0070 by-value evidence for `invalidates`, in [`Stmt::invalidated`]'s shape —
        /// a fact the callee provably can't reach survives the comparison. Empty for a
        /// non-call writer (`OperandWriters::Any`), so `f($y)+($y=1)` keeps the blanket drop.
        sites: Vec<InvalidatedVar>,
    },
}

/// A small lowered condition language (ADR-0031); the trace evaluator walks it against the
/// env to a `Certainty` (yes/no/maybe). Unrecognized conditions become [`CondExpr::Opaque`],
/// carrying the read variables so the walk can forget them on the excluded path.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub enum CondExpr {
    /// `lhs <op> rhs` — a comparison (`===`/`!==`/`==`/`!=`).
    Cmp { op: CmpOp, lhs: CondOperand, rhs: CondOperand },
    /// A bare truthiness test (`if ($x)`, `if (foo())`).
    Truthy(CondOperand),
    /// `operand instanceof Class` — `class_ref` is the class as written (resolved
    /// project-wide at evaluation time).
    Instanceof { operand: CondOperand, class_ref: NameRef },
    /// `!cond`.
    Not(Box<CondExpr>),
    /// `a && b` / `a and b`.
    And(Box<CondExpr>, Box<CondExpr>),
    /// `a || b` / `a or b`.
    Or(Box<CondExpr>, Box<CondExpr>),
    /// A resolvable call in guard position (`if (isFoo($x))`). Retained (not opaqued) so
    /// inference can consume `@phpstan-assert-if-true`/`-if-false` (ADR-0052 §5, `Asserted`
    /// stratum) and fold existence predicates (`method_exists`/etc, ADR-0049 §4/N3) to a real
    /// verdict; other guard calls evaluate `Maybe`, and `reads` invalidates their variables.
    Call { call: Box<CallExpr>, reads: Vec<String> },
    /// `isset($var[<literal>])` — a key-presence guard, depth exactly one (ADR-0062 S4).
    /// True branch promotes presence and strips `null` (PHP's own `isset` semantics); only
    /// this exact form lowers here — bare `isset($x)` is [`Self::IssetVar`] (issue #414)
    /// and property/dynamic keys go to [`Self::Opaque`]. `empty($x[<literal>])` lowers to `!isset(…) || !…`; multi-arg
    /// `isset($a['x'],$b['y'])` lowers to an [`Self::And`] chain only when every operand fits.
    Isset { var: String, key: Box<ArgValue> },
    /// `operand instanceof <dynamic class>` — the class is an expression rather
    /// than a written name (issue #571). `$v instanceof $class` is the common
    /// spelling; `$v instanceof $this->cls` and kin land here too.
    ///
    /// It exists for the same reason [`Self::IssetVar`] does: `instanceof` is an
    /// **operator** and cannot write either side, so sending this shape to
    /// [`Self::Opaque`] charged the subject the by-reference conservatism an
    /// unmodellable condition owes, and the guard destroyed the very fact it was
    /// written to refine.
    ///
    /// `class` is carried rather than discarded so a later reader can ask what
    /// the operand's own type says — a `class-string<T>` value proves `T` here
    /// exactly as a written name does (issue #573). Nothing asks yet: this form
    /// evaluates `Maybe` and refines nothing.
    ///
    /// `reads` is **not** an invalidation set — this condition invalidates
    /// nothing. It is the over-inclusive variable mention set
    /// `guard_chain_subject` reads, carried verbatim from what [`Self::Opaque`]
    /// recorded for this shape so that subject selection is byte-identical
    /// across the change. The two questions shared one field before, which is
    /// why the cheap fix (an empty `Opaque` read set) was not available.
    InstanceofDyn { operand: CondOperand, class: CondOperand, reads: Vec<String> },
    /// `isset($var)` over a bare variable — a presence test with no offset (issue #414).
    ///
    /// It exists to keep this shape OUT of [`Self::Opaque`]. `isset` is a language
    /// construct, not a call: it cannot mutate its operand, so charging its read set
    /// the by-reference conservatism [`Self::Opaque`] owes an unmodellable condition
    /// discarded every fact the variable had — inside the branch and after it. What
    /// the variant claims is only that: nothing to forget. It evaluates to `Maybe`
    /// and refines nothing, so a caller that wants the presence proof itself
    /// (ADR-0087 §4's `|unset` read inside its own guard) has a place to put it.
    IssetVar { var: String },
    /// A condition the lowering cannot model. `reads` lists every bare variable it mentions,
    /// so the excluded path still invalidates them (ADR-0027 read-set rule).
    Opaque { reads: Vec<String> },
}

/// One arm of a structured [`StmtKind::Match`] (ADR-0031 Part B). `conditions` are the
/// arm's comparison operands (may list several: `1, 2 => …` / stacked `case`s); taken when
/// the subject equals any of them (`===` for match, loose `==` for switch). `trace` is the
/// arm body, lowered like any sub-trace; a switch arm's terminating `break` is stripped.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct MatchArmT {
    pub conditions: Vec<CondOperand>,
    pub trace: Vec<Stmt>,
}

/// One entry of a scope's linear trace IR (ADR-0001). A scope's body lowers to an ordered
/// list of these; anything unrecognized becomes [`StmtKind::Barrier`] (sound over-lowering).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub enum StmtKind {
    /// `$var = <value>;` — a plain assignment to a bare local. `span` is the left-hand
    /// `$var`. `call` carries the full [`CallExpr`] when the RHS is a statically-named call
    /// (`$x = f($s);`), so propagation can check/descend — `ArgValue::Call` alone loses arg spans.
    Assign { var: String, value: ArgValue, span: Span, call: Option<CallExpr> },
    /// `$var->prop = <rvalue>;` — a property assignment (ADR-0036). `target_var` is the
    /// receiver (`"this"` for `$this`); `value` is the lowered rvalue (compound `+=`/`.=`
    /// lowers to [`ArgValue::Other`]); `value_call` carries the full call when statically-named.
    /// A dynamic property or chained lvalue (`$a->b->c=…`) stays [`StmtKind::Barrier`].
    PropAssign {
        target_var: String,
        prop: String,
        value: ArgValue,
        value_call: Option<CallExpr>,
        span: Span,
    },
    /// A statement-level function call `f(args);`.
    Call(CallExpr),
    /// `return <value>;` ([`ArgValue::Other`] for bare `return;`). `call` carries the full
    /// call when statically-named, reaching interprocedural descent. `span` points at the
    /// value (or `return` itself), for the return-type check's diagnostic.
    Return { value: ArgValue, call: Option<CallExpr>, span: Span },
    /// `echo e1, e2, …;` — carries the statically-named calls among its operands so
    /// propagation checks/descends them. Echo assigns nothing, so its env effect stays a
    /// conservative `Barrier`-equivalent clear.
    Echo(Vec<CallExpr>),
    /// A structured `if`/`elseif`/`else` (ADR-0031): models control flow instead of erasing
    /// it. `then_trace` is the primary branch; `elseifs` are `(condition, branch)` pairs in
    /// order; `else_trace` is the `else` branch when present. Sub-traces lower by the same
    /// rules (nested ifs recurse; unlowerable constructs appear as `Opaque` inside). Only the
    /// statement form of `if` lowers here; everything else stays [`StmtKind::Opaque`] (ADR-0027).
    If {
        cond: CondExpr,
        then_trace: Vec<Stmt>,
        elseifs: Vec<(CondExpr, Vec<Stmt>)>,
        else_trace: Option<Vec<Stmt>>,
    },
    /// A structured statement-position `match` or `switch` (ADR-0031 Part B): the trace
    /// models arm control flow. `subject` is the scrutinee; `arms` are the conditional arms
    /// in source order; `default` is the default arm when present. `loose` distinguishes
    /// `match` (`false`: strict `===`, first-match, missing default throws
    /// `\UnhandledMatchError`) from `switch` (`true`: loose `==`, falls through).
    ///
    /// Only fully-modelable constructs reach here — subject and every arm condition must
    /// lower to a bare variable or literal, and (for `switch`) every non-empty case must
    /// end in `break`/`return`/`throw`/`exit` with no fall-through. Any failure stays
    /// [`StmtKind::Opaque`] wholesale — an unrepresentable arm opaques the whole construct.
    Match {
        subject: CondOperand,
        arms: Vec<MatchArmT>,
        default: Option<Vec<Stmt>>,
        loose: bool,
    },
    /// `assert(<expr>);` (ADR-0052 §5). `cond` is the lowered guard; the walk applies its
    /// `then_refinements` unconditionally at the `Verified` stratum (2026-07-25 ruling:
    /// `assert()` reads as a throw-guard, never consulting `zend.assertions`). Only a
    /// bare `assert($expr)` (with optional `$description`) reaches here; else plain `Call`.
    Assert { cond: CondExpr },
    /// `throw <expr>;` — a trace terminator; `span` points at the `throw`. The thrown
    /// expression is not modeled, only the terminating control effect.
    Throw { span: Span },
    /// `exit;` / `die;` (as an expression-statement) — a trace terminator; `span` points at it.
    Exit { span: Span },
    /// A recognized control-flow construct (`while`/`for`/`foreach`/`switch`/`match`-stmt/
    /// `try`/nested block) whose data-flow isn't modeled, but whose write/read sets are
    /// (ADR-0027 ratchet: forgets only touched/branched variables, not all known values).
    ///
    /// * `writes` — over-approximated names the subtree may assign (any lvalue, compound/
    ///   inc-dec, `foreach`/`catch`/`list()` bindings) plus everything handed to any call
    ///   (by-ref conservatism); nested function/closure bodies don't count.
    /// * `reads` — every other mentioned variable not in `writes`; a construct that reads
    ///   and branches may early-return, so the fall-through path excludes the known value
    ///   (`if ($x == null) { return; }` filters `null` from the tail). Same scope exclusion.
    /// * `poisons` — `true` if the subtree has an ADR-0001 poison marker (reference/`global`/
    ///   `static`/var-variable/`extract`/`include`/by-ref `use`); clears env like `Barrier`.
    /// * `may_return` — `true` if the subtree has a `return` not visible as a top-level
    ///   [`StmtKind::Return`] (e.g. in `foreach`/`try`). ADR-0057 T0 return-fact floors
    ///   prevent a visible sibling `return null` becoming a false Singleton (ADR-0075 / issue #126).
    ///
    /// Limitation: can't prove fall-through dead when every branch returns early, without
    /// reachability analysis.
    Opaque { writes: Vec<String>, reads: Vec<String>, poisons: bool, may_return: bool },
    /// `$var[<lit>] = <rvalue>;` — constant-key offset write (ADR-0062 A-G8). A
    /// [`Self::Barrier`] plus one fact: after forgetting env/store, only the base binding's
    /// array shape re-establishes with the key promoted. `keys` has one or two entries
    /// (depth 1, plus autovivification); append (`$x[]=`), dynamic keys, and `+=`/`.=` stay
    /// plain `Barrier`.
    OffsetWrite { base: String, keys: Vec<ArgValue>, value: ArgValue },
    /// `unset($var[<lit>]);` — constant-key offset unset (A-G8), same containment as
    /// [`Self::OffsetWrite`] plus a `mark_absent` on the shape. Multi-target unset, dynamic
    /// keys, and `unset($var)` itself all stay plain `Barrier`.
    OffsetUnset { base: String, key: ArgValue },
    /// `[$a, $b] = <source>;` / `list(...) = <source>;` — array destructuring (issue #288).
    /// A [`Self::Barrier`] plus one fact: the reads the source undergoes. Each target reads
    /// `<source>[k]` once (assignment-RHS-like); PHP warns `Undefined array key k` for a
    /// missing key. Targets are writes and aren't modeled — env/store still fully forgotten.
    ///
    /// `reads` holds one key path per target, outermost first, in source order: `[$a, $b]`
    /// is `[[0],[1]]`, `['a'=>$x]` is `[["a"]]`, nested `[[$a], $b]` recurses to
    /// `[[0],[0,0],[1]]` (outer read happens too, and warns first). A skipped hole (`[, $b]`)
    /// consumes its index without reading. `call` carries the full [`CallExpr`] when the
    /// source is a statically-named call, so the read judgment reaches its declared return.
    ///
    /// A pattern the lowering can't read faithfully stays plain [`Self::Barrier`]: a
    /// by-reference target (`[&$a]=$m` aliases `$m[0]` into existence, not a read), a
    /// spread, or a non-literal key.
    Destructure { source: ArgValue, call: Option<CallExpr>, reads: Vec<Vec<ArgValue>>, span: Span },
    /// Any construct the trace can't model *and* can't bound the write set of (`goto`,
    /// labels, `declare`, `__halt_compiler`, unsure cases). Erases all known values — the sound floor.
    Barrier,
}

// reachability foundation (ADR-0078, issue #199)

/// Where a statement — or a whole statement list — leaves control (ADR-0078, issue #199):
/// the terminality judgment the reachability foundation is built from, computed at lowering
/// time from the CST so every consumer reads one answer instead of re-deriving control flow.
///
/// Over the syntactic control-flow graph, not path feasibility (branches are
/// non-deterministic); models precisely a construct with no exit edge at all
/// (`return`/`throw`/`exit`, unhandled `match`, `while (true)` with no `break`).
///
/// # The safe-side asymmetry
///
/// [`Self::Unknown`] is honest when exit edges aren't bounded (`try`/`catch`, `goto`, an
/// unstructurable `switch`) — but its safe side differs by consumer:
/// * `type.return-missing` accuses "runs off its end"; only [`Self::FallsThrough`] may be
///   accused, so `Unknown` is silence ([`Self::provably_falls_through`]).
/// * a dead-code consumer accuses "the next statement never runs"; only [`Self::Terminates`]
///   may be accused, so `Unknown` is silence ([`Self::provably_terminates`]).
///
/// Both predicates exist so neither consumer writes `!= Terminates` / `!= FallsThrough` —
/// exactly the mistake that would invert the other's safe side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub enum BodyEnd {
    /// Control provably never reaches the end: `return`/`throw`/`exit`, a branch whose every
    /// arm terminates, or a loop with no exit edge.
    Terminates,
    /// Control provably can reach the end — a terminator-free syntactic path exists (an `if`
    /// with no `else`, a loop whose condition can be false, a plain statement).
    FallsThrough,
    /// Undecided: exit edges aren't bounded (`try`/`catch`/`finally`, `goto`/labels, an
    /// unstructurable `switch`). Every consumer must name which way it reads this (see above).
    Unknown,
}

impl BodyEnd {
    /// `true` only for [`Self::FallsThrough`]. `Unknown` answers `false`: never accuse a
    /// body of running off its end without proof.
    #[must_use]
    pub const fn provably_falls_through(self) -> bool {
        matches!(self, Self::FallsThrough)
    }

    /// `true` only for [`Self::Terminates`]. `Unknown` answers `false`: never call a
    /// statement unreachable without proof its predecessor terminates.
    #[must_use]
    pub const fn provably_terminates(self) -> bool {
        matches!(self, Self::Terminates)
    }

    /// Join the ends of a branch construct's alternative arms (`if`/`match`/`switch`,
    /// including the implicit no-match arm): every arm `Terminates` ⇒ terminates; any arm
    /// `FallsThrough` ⇒ falls through; otherwise (only `Terminates`/`Unknown`) ⇒ `Unknown`.
    ///
    /// Empty list ⇒ `Terminates` (identity). Callers supply the implicit arm: `if` with no
    /// `else` joins `FallsThrough`; `match` with no `default` joins `Terminates` (PHP throws).
    #[must_use]
    pub fn join_arms(arms: impl IntoIterator<Item = Self>) -> Self {
        let mut acc = Self::Terminates;
        for arm in arms {
            acc = match (acc, arm) {
                (Self::FallsThrough, _) | (_, Self::FallsThrough) => Self::FallsThrough,
                (Self::Unknown, _) | (_, Self::Unknown) => Self::Unknown,
                (Self::Terminates, Self::Terminates) => Self::Terminates,
            };
        }
        acc
    }
}

/// The terminality of an ordered statement list (ADR-0078, issue #199): reads each entry's
/// [`Stmt::end`]. Not "the last statement decides": the first [`BodyEnd::Terminates`] wins
/// outright (everything after is unreachable, so `[try{…}catch{…}, return 1;]` answers
/// `Terminates`, not the `try`'s `Unknown`); an [`BodyEnd::Unknown`] entry is remembered but
/// not stopped on; an empty list answers [`BodyEnd::FallsThrough`] (the identity).
#[must_use]
pub fn body_end(stmts: &[Stmt]) -> BodyEnd {
    let mut undecided = false;
    for stmt in stmts {
        match stmt.end {
            BodyEnd::Terminates => return BodyEnd::Terminates,
            BodyEnd::Unknown => undecided = true,
            BodyEnd::FallsThrough => {}
        }
    }
    if undecided { BodyEnd::Unknown } else { BodyEnd::FallsThrough }
}

/// Whether a statement list contains a **function exit** anywhere at all (`return`/`throw`/
/// `exit`/`die`), however deeply nested, whether or not it is on a path [`body_end`] can see.
///
/// Separate from [`body_end`] ("does control reach the end" vs "does the body exit
/// anywhere"): together they split a falling-through body into the populations the zero-FP
/// policy floors differently (ADR-0078 §1.3, the `maybe-` convention):
///
/// * **no exit anywhere** — a stub, empty body, or pure side effects: every execution runs
///   off the end, so the consequence is unconditional.
/// * **an exit somewhere, but not every path** — e.g. a no-`default` `switch` whose every
///   case returns, or an `if` with no `else`: the fall-through edge exists but may be taken
///   only for inputs the program never sees — exactly the definite/possibly boundary.
///
/// Reads [`Stmt::has_terminator`], computed over the **whole CST subtree**, so a `return`
/// inside a `foreach`/`try`/`switch` counts even though the trace IR erased those into an
/// opaque node with nothing inside.
#[must_use]
pub fn body_has_terminator(stmts: &[Stmt]) -> bool {
    stmts.iter().any(|s| s.has_terminator)
}

// end reachability foundation (ADR-0078, issue #199)

/// A trace entry plus the local variables it feeds into a call.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct Stmt {
    pub kind: StmtKind,
    /// The whole statement's source span (set by `lower_stmt` from the CST node; nested
    /// constructs' inner statements carry their own). Used to record proven-dead regions.
    pub span: Span,
    /// Variables this statement passes as an argument to any call, one entry per name,
    /// carrying the evidence ADR-0070's by-value gate consults. The checker marks each name
    /// unknown after the statement (sound floor: a by-ref param could mutate it), unless
    /// every occurrence is a provable by-value site.
    pub invalidated: Vec<InvalidatedVar>,
    /// Every place this statement puts a value into PHP's string context (ADR-0078, issue
    /// #193) — `echo`/`print` operands, interpolated-string expressions, `(string)` casts,
    /// both operands of `.`. Collected by `lower_stmt`, judged against the statement's ENTRY env.
    pub string_contexts: Vec<StringContextSite>,
    /// Where this statement leaves control (ADR-0078, issue #199) — the per-statement half
    /// of the reachability foundation [`body_end`] reads.
    ///
    /// Computed from the CST, not [`Self::kind`]: the trace IR erases `while`/`try`/`switch`
    /// into undifferentiated [`StmtKind::Opaque`], so only the CST can tell a no-exit
    /// `while (true)` from an always-exits `foreach`. Independent of `kind` by design: a
    /// `break;` (`Barrier`) and `while (true) {}` (`Opaque`) can both have `end: Terminates`.
    pub end: BodyEnd,
    /// Whether this statement's whole CST subtree contains a function exit (`return`/
    /// `throw`/`exit`, ADR-0078 §5, issue #199); nested function-likes aren't counted.
    /// Orthogonal to [`Self::end`] by design (`end`: does control reach past this statement;
    /// this: does the subtree exit anywhere) — splits a falling-through body into the
    /// unconditional/conditional classes (see [`body_has_terminator`]).
    pub has_terminator: bool,
}

impl Stmt {
    /// A statement under construction: `kind` and by-ref evidence set; span, string-context
    /// sites, and terminality left for `lower_stmt` to fill centrally (`span` is
    /// [`ZERO_SPAN`] until then). `end` starts `FallsThrough` (correct for straight-line
    /// code); the two paths that bypass `lower_stmt` (`lower_arm_body`, arrow-function body)
    /// set it themselves.
    pub(crate) fn lowered(kind: StmtKind, invalidated: Vec<InvalidatedVar>) -> Stmt {
        Stmt {
            kind,
            span: ZERO_SPAN,
            invalidated,
            string_contexts: Vec::new(),
            end: BodyEnd::FallsThrough,
            has_terminator: false,
        }
    }
}

/// One value a statement hands to PHP's string conversion (ADR-0078, issue #193): the
/// lowered operand, its span, and which syntactic context it is. The syntax layer only
/// records where conversions are; legality (`int`/`null`/other scalars fine, array warns,
/// no-`__toString` object fatals) is the inference layer's judgement.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct StringContextSite {
    /// The converted operand, lowered like a call argument. An operand the lowering can't
    /// spell arrives as [`ArgValue::Other`] — silence, never a manufactured finding.
    pub value: ArgValue,
    /// The span of the operand (not of the enclosing construct): the text to fix.
    pub span: Span,
    /// Which conversion this is, for the finding's own words.
    pub kind: StringContextKind,
}

/// The syntactic form of a [`StringContextSite`]. PHP converts identically in all of
/// them — the distinction exists only so a finding can name the construct.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub enum StringContextKind {
    /// An expression embedded in a double-quoted string/heredoc (`"x $v"`, `"{$v}"`), or a
    /// backtick shell-exec string (same conversion).
    Interpolation,
    /// An `echo` operand, including the `<?= … ?>` short-tag form.
    Echo,
    /// A `print` operand.
    Print,
    /// A `(string)` cast.
    Cast,
    /// Either operand of `.`, and both sides of `.=` (reads its target in string context too).
    Concat,
}

impl StringContextKind {
    /// How a finding names this context.
    #[must_use]
    pub fn render(self) -> &'static str {
        match self {
            StringContextKind::Interpolation => "string interpolation",
            StringContextKind::Echo => "`echo`",
            StringContextKind::Print => "`print`",
            StringContextKind::Cast => "a `(string)` cast",
            StringContextKind::Concat => "a `.` concatenation",
        }
    }
}

/// One local variable a statement hands to a call — its name plus ADR-0070 evidence. The
/// syntax layer only guarantees completeness: `sites` lists EVERY occurrence in the
/// statement's call arguments, or `opaque` is set and `sites` is empty — no third state.
/// An occurrence is a bare `$v` argument or a pure offset chain rooted at one (`$v[0]`,
/// issue #609) — the root is the name recorded, since a by-ref write through the chain
/// lands in the root's binding.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct InvalidatedVar {
    /// The local variable's name, without the leading `$`.
    pub name: String,
    /// `true` if at least one occurrence is unprovable (method/static/dynamic callee, named
    /// or spread args, closure-body occurrence, echo-embedded write) — no site is protected.
    pub opaque: bool,
    /// The provable occurrences: callee reference + 0-based argument position (same
    /// [`NameRef`] a [`CallExpr`] carries, for project-wide resolution).
    pub sites: Vec<(NameRef, u32)>,
}

/// Placeholder span for [`Stmt`]s under construction — overwritten with the
/// real statement span by `lower_stmt` before the statement enters a trace.
const ZERO_SPAN: Span = Span { start: 0, end: 0 };

/// Who owns an analysis [`Scope`] — top-level script, free function, or class method. Method
/// scopes carry their declaring class so `$this->`/`self::`/`parent::` resolve (ADR-0001).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub enum ScopeOwner {
    TopLevel,
    Function(String),
    Method { class: String, method: String },
    /// A closure/arrow-function body (ADR-0033), addressed by definition-site byte offset
    /// (the closure/`fn` keyword span start); an [`ArgValue::Closure`] naming this offset
    /// descends here. Params/effects/throws live on the [`Scope`] itself (no [`FunctionDecl`]).
    Closure { def_offset: u32 },
    /// A PHP 8.4 property hook body (issue #544), plain or constructor-promoted. `class`
    /// is the declaring class's case-preserved FQN, `property` the hooked name without
    /// its `$`; the triple is unique within a file, which is how [`Scope`] lookups
    /// address it.
    ///
    /// It runs in the declaring class's scope with `$this` bound, so it is a method body
    /// in every way the walk cares about — but it is not a method: no [`MethodDecl`]
    /// carries its signature, so, exactly as for a closure, the hook's parameters and
    /// its native return type ride on the [`Scope`] itself.
    PropertyHook { class: String, property: String, hook: HookKind },
}

/// Which of a hooked property's two hooks a [`ScopeOwner::PropertyHook`] scope is.
///
/// The engine names them `$prop::get` / `$prop::set` (witnessed 8.5.9, in the
/// `TypeError` a hook raises), which is the spelling every diagnostic subject uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub enum HookKind {
    /// `get` — takes nothing, and must return the property's declared type
    /// (witnessed: falling off the end raises `C::$v::get(): Return value must be of
    /// type int, none returned`).
    Get,
    /// `set` — takes exactly one parameter and returns nothing. Written explicitly
    /// (`set(T $v)`) or left implicit, in which case the engine names it `$value` and
    /// types it as the property (witnessed by `ReflectionProperty::getHooks()`).
    Set,
}

impl HookKind {
    /// The hook name as PHP writes it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            HookKind::Get => "get",
            HookKind::Set => "set",
        }
    }
}

/// A construct on the ADR-0001 whole-scope give-up list: code the analyzer parses and then
/// declines to reason about (ADR-0046 §1 "scope havoc"). Each variant is a reason
/// [`Scope::poisoned`] is set; `scan_opaque` backs both the predicate and this inventory, so
/// `steins doctor`'s report can never drift from a hand-maintained parallel list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub enum OpaqueConstruct {
    /// `eval(<expr>)` — code as data (also a [`DynamismKind::Eval`] dam site).
    Eval,
    /// `include` / `include_once` / `require` / `require_once` — code pulled in at
    /// runtime, able to write any local of the including scope.
    Include,
    /// `extract(...)` — names minted into the scope from array keys.
    Extract,
    /// `compact(...)` — the scope's own names read as data.
    Compact,
    /// `$$name` / `${<expr>}` — a variable variable (read or write).
    VariableVariable,
    /// `$x = &$y` — reference assignment: two names, one cell.
    ReferenceAssign,
    /// `global $x` — the local is an alias of a global cell.
    Global,
    /// `static $x` — the local outlives the call and other calls write it.
    StaticVar,
    /// `use (&$x)` — a closure captures a local by reference; poisons both the enclosing
    /// scope and the closure's own scope (ADR-0033).
    ByRefCapture,
}

impl OpaqueConstruct {
    /// Every variant, in `steins doctor`'s report order. Hand-maintained — adding a variant
    /// without extending it compiles but silently drops it, so a workspace test pins length/labels.
    pub const ALL: [Self; 9] = [
        Self::Eval,
        Self::Include,
        Self::Extract,
        Self::Compact,
        Self::VariableVariable,
        Self::ReferenceAssign,
        Self::Global,
        Self::StaticVar,
        Self::ByRefCapture,
    ];

    /// The construct's label as a posture report spells it — PHP's own spelling where one exists.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Eval => "eval",
            Self::Include => "include/require",
            Self::Extract => "extract",
            Self::Compact => "compact",
            Self::VariableVariable => "variable variable",
            Self::ReferenceAssign => "reference assignment",
            Self::Global => "global",
            Self::StaticVar => "static variable",
            Self::ByRefCapture => "by-ref capture",
        }
    }
}

/// One give-up-list construct, where it stands. Collected per scope (see [`Scope::opaque`]),
/// since "no local is known here" is a scope-level fact, not a file-wide one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct OpaqueSite {
    pub construct: OpaqueConstruct,
    /// The construct's source span (the outermost, when nested — the predicate stops there too).
    pub span: Span,
}

/// Classification of a written return type hint (`: T`), independent of whether Steins
/// lowers `T` to a [`NativeType`]. Only a fully untyped declaration (no hint) contributes
/// PHP's implicit `return null` to return-fact fallthrough (ADR-0075); `void`/`never`/other
/// don't.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub enum RetHintKind {
    /// `: void`
    Void,
    /// `: never`
    Never,
    /// `: mixed` — the TOTAL envelope. Lowers to no [`NativeType`] like `: array` and
    /// `: object` do, but unlike them it admits every value, so the return summary reads
    /// it as no hint at all (ADR-0075 §2.4, issue #364). Everywhere else it is an ordinary
    /// written hint: a `: mixed` body that falls off its end is a runtime `TypeError`
    /// exactly as `: int` is.
    Mixed,
    /// Any other hint — scalar, class, `array`, union, …
    Other,
}

/// A written return type hint (`: T`) — its [`RetHintKind`] plus the source span of the
/// hint itself (the `T`, not the colon), so a diagnostic can quote the type exactly as
/// written ([`SourceTree::text_at`]): `: array`/`: mixed` lower to no [`NativeType`], so
/// `ret`/[`Scope::ret_ty`] can't name them, yet PHP's own `TypeError` does. Kind and span
/// travel together so they can't drift apart.
///
/// [`SourceTree::text_at`]: crate::SourceTree::text_at
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct RetHint {
    pub kind: RetHintKind,
    /// The hint's own file byte span; `SourceTree::text_at` maps it back to text.
    pub span: Span,
}

/// One analysis scope: top-level script, function body, or method body. Carries the linear
/// trace and a whole-scope `poisoned` flag (ADR-0001 give-up list).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct Scope {
    /// `None` for the top-level script and method bodies; `Some(name)` for a free function
    /// body — needed by function-world propagation paths that key on a free-function name.
    pub function_name: Option<String>,
    /// The precise owner of this scope (top-level / function / method).
    pub owner: ScopeOwner,
    /// The written return type hint, if any (kind + source span; see [`RetHint`]). `None`
    /// means untyped. Distinct from [`Self::ret_ty`], which collapses void/never/object/array to silence.
    pub ret_hint: Option<RetHint>,
    /// `true` when the body has `yield`/`yield from` — the call result is a `Generator`,
    /// not the trailing `return` value (ADR-0057 §5); return summaries refuse such scopes.
    pub is_generator: bool,
    /// `true` if the scope has any construct defeating local value tracking (`extract`/
    /// `compact`, `global`, `static $x`, variable-variables, ref assignment, by-ref closure
    /// capture, `include`/`require`/`eval`) — no variable is ever known when poisoned.
    pub poisoned: bool,
    /// Every give-up-list construct found in this scope, in source order — the reasons
    /// [`Self::poisoned`] holds (`poisoned == !opaque.is_empty()` by construction, one
    /// `scan_opaque` walk backing both). Read by `steins doctor`'s coverage posture (ADR-0054 §9.2).
    pub opaque: Vec<OpaqueSite>,
    pub stmts: Vec<Stmt>,
    /// Every instance/static method call in this scope's body, comprehensively (including
    /// calls nested in sub-expressions the trace drops to [`ArgValue::Other`]), not
    /// descending into nested scopes — the sound caller-enumeration surface the
    /// method-transform reverse sweep needs (ADR-0043 §6): a candidate method is safe to
    /// rewrite only when every reaching call is accounted for. `new` calls are omitted.
    pub method_calls: Vec<CallExpr>,
    /// Parameters of a closure/arrow scope ([`ScopeOwner::Closure`]) or a property-hook
    /// scope ([`ScopeOwner::PropertyHook`]) — no [`FunctionDecl`] to look them up on, so
    /// binding descent and native parameter seeding read them here. A `set` hook written
    /// without a parameter list carries the engine's implicit one (`$value`, typed as the
    /// property). Empty for function/method/top-level scopes (resolved via [`Self::owner`]).
    pub params: Vec<Param>,
    /// Declared native return type of a closure/arrow scope ([`ScopeOwner::Closure`]) or a
    /// `get` hook ([`ScopeOwner::PropertyHook`], where it is the property's own declared
    /// type) — no [`FunctionDecl`] carries it, so the callable-signature variance check
    /// (issue #11) reads the closure's `: R` here. `None` for no/unrepresentable hint, for
    /// a `set` hook (which returns nothing), or for any other scope.
    pub ret_ty: Option<NativeType>,
    /// Effect-origin candidates of a closure/arrow body ([`ScopeOwner::Closure`]), so a
    /// closure can be an effect node in the fixpoint (ADR-0033 point 3); empty otherwise.
    pub effect_origins: Vec<EffectOrigin>,
    /// Throw-origin candidates of a closure/arrow body — the throw-fixpoint analogue of [`Self::effect_origins`].
    pub throw_origins: Vec<ThrowOrigin>,
    /// Spans of `match (true)`/`match (false)` guard chains in this scope whose
    /// `default` arm is absent (ADR-0088 §5's note on issue #448) — every one is
    /// the same span [`ThrowKind::New`]'s synthetic `UnhandledMatchError` origin
    /// for the construct already carries (`scan_throw_origins`'s `Node::Match`
    /// arm), computed independently by `scan_guard_chain_no_default`.
    ///
    /// `lower_match_guard_chain` desugars such a chain to [`StmtKind::If`] with
    /// `else_trace: None` — deliberately (issue #431), so the guard vocabulary and
    /// the join stay the `if` path's — but that erases the one bit the coverage
    /// gate needs: whether a missing `else` is an ordinary fall-through or a
    /// `\UnhandledMatchError`. ADR-0031 keeps the trace IR itself free of
    /// syntactic-provenance bits, so the question is answered *here*, off the CST,
    /// independently of [`Self::stmts`], and consulted by the walk purely by span
    /// — the same discipline [`ForeachSite`]/[`OperandSite`] already use for a
    /// question the trace's own node shape cannot carry. Never an answer: whether
    /// a listed construct actually throws is an inference-time question (the
    /// subject's Verified domain) this pass cannot ask.
    pub guard_chain_no_default: Vec<Span>,
    /// `true` when a closure/arrow scope ([`ScopeOwner::Closure`]) was declared `static`
    /// (`static function () {}`, `static fn () => …`) — can never bind to an object or touch
    /// `$this`. A syntactic fact (ADR-0063 §2 decision 4), making `static-closure`'s binding
    /// obligation a mechanical check, not inference. Always `false` for non-closure scopes.
    pub is_static: bool,
    /// Adopted docblock of a closure/arrow scope ([`ScopeOwner::Closure`]) — no
    /// [`FunctionDecl`] exists to adopt one on, so `@return` phpdoc (issue #128) reads it
    /// here via the whitespace-gap discipline (ADR-0029, `adopt_closure_docblock`); `None`
    /// for functions/methods, which adopt on their decls.
    pub docblock: Option<String>,
    /// The by-value `use ($x)` captures of a `function (…) use (…) {…}` scope whose name
    /// the closure body never mentions (issue #186, `closure.unused-use`), computed at
    /// lowering. Three silences keep it safe: a by-ref `use (&$x)` is never listed (it's an
    /// out-channel, recorded as [`OpaqueConstruct::ByRefCapture`] instead); any `$x` mention
    /// anywhere in the body subtree (including nested closures) clears a capture, not just a
    /// read; and the whole list is empty when the body holds a name dam (`compact`/`extract`/
    /// `get_defined_vars`/`$$x`/`eval`/`include`). Empty for arrow functions and non-closures.
    pub unused_captures: Vec<UnusedCapture>,
    // undefined variables (ADR-0078, issue #194)
    /// Every read of a name this scope never binds — the firing set of `variable.undefined`
    /// (issue #194), computed at lowering like [`Self::unused_captures`]. Closed-world over
    /// the scope's own text and ordering-blind: a name bound anywhere (before, after, in a
    /// dead branch) counts as bound; a read preceding its only assignment is silence here —
    /// that's `variable.maybe-undefined`'s territory (issue #199).
    ///
    /// The list is empty — never merely shorter — where the closed world doesn't hold:
    /// * top-level script scopes, where `include` splices in the including scope's whole
    ///   symbol table, so no top-level name can ever be proven unbound from the file's text;
    /// * arrow-function scopes (`fn () => $x` auto-captures every free variable, so its reads
    ///   are the enclosing scope's question — witnessed silent: `$x = 3; fn () => $x + 1` on
    ///   8.5.9);
    /// * a scope carrying a name dam (`extract`/`compact`/`get_defined_vars`/`$$x`/`${…}`/
    ///   `eval`/`include`/`require` — the same dam [`Self::unused_captures`] uses), which can
    ///   mint or consume a binding invisibly;
    /// * reads of a superglobal, `$this`, or `$http_response_header` (engine-bound), filtered
    ///   at collection.
    ///
    /// `isset($x)`/`empty($x)`/`$x ?? d`/`unset($x)`/`@$x` reads are also excluded, since PHP
    /// legalizes them (witnessed silent at 8.5.9) — ADR-0078 §3 defers that reading entirely.
    ///
    /// One residue is left for the checker: a bare `$x` passed to a statically-named function
    /// may be an out-parameter, needing the cross-file index to know if the callee declares
    /// `&$p` (ADR-0077's by-value oracle) — collected here, subtracted in `steins-infer`.
    /// Every other call shape binds its bare-variable arguments outright.
    pub undefined_reads: Vec<UndefinedRead>,
    /// Reads of names some paths through this scope reach unbound (ADR-0081, issue #267) —
    /// the firing set of `variable.maybe-undefined`, disjoint from [`Self::undefined_reads`]
    /// by construction: a name bound nowhere is the definite id's, bound somewhere is this one's.
    ///
    /// Produced by the binding-presence pass: walks statements in program order over a
    /// three-valued lattice, subtracts a provably terminating branch arm, iterates loops to a
    /// fixpoint, consumes `isset`/`empty` guards with polarity. Inherits every silence
    /// [`Self::undefined_reads`] documents, plus one of its own: a `goto`/label anywhere dams
    /// the pass (unbounded jump target).
    ///
    /// Same [`Self::ref_arg_candidates`] residue, with one refinement: an out-parameter binds
    /// from its call site forward, so a candidate subtracts only the reads that follow it.
    pub maybe_undefined_reads: Vec<UndefinedRead>,
    /// Every bare-variable positional argument at a function call in this scope — the
    /// out-parameter candidates [`Self::undefined_reads`] can't settle alone, since whether
    /// `f($x)` writes `$x` needs the cross-file index.
    ///
    /// Recorded independently of [`Self::undefined_reads`]: an out-parameter is a binding
    /// form and must not depend on whether its argument was collected as a read — it isn't:
    /// `@proc_open($cmd, $spec, $pipes)` inside an error-control guard withholds `$pipes`
    /// from the read list while PHP still binds it (symfony/console's `Terminal.php`).
    ///
    /// Only plain function calls appear here — every other call shape has no callee name to
    /// resolve, so it binds bare-variable arguments outright at lowering. Empty whenever
    /// [`Self::undefined_reads`] is.
    pub ref_arg_candidates: Vec<UndefinedRead>,
    // end undefined variables (ADR-0078, issue #194)
}

/// One by-value `use ($x)` capture a closure body never mentions — an entry of
/// [`Scope::unused_captures`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct UnusedCapture {
    /// The captured name without the leading `$`.
    pub name: String,
    /// The file byte span of the `$x` token in the `use (…)` clause.
    pub span: Span,
}

// undefined variables (ADR-0078, issue #194)

/// One read of a name its scope never binds — an entry of
/// [`Scope::undefined_reads`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct UndefinedRead {
    /// The read name without the leading `$`.
    pub name: String,
    /// The file byte span of the `$x` token at the read, also what `lower_argument_list`
    /// records for a bare-variable positional argument — the join the checker's
    /// out-parameter subtraction keys on.
    pub span: Span,
}

// end undefined variables (ADR-0078, issue #194)

// unset pseudo-type (ADR-0087 §4, issue #396)

/// One read of a name a top-level `/** @var T|unset $x */` may have declared
/// possibly-unbound — a **candidate** for `phpdoc.maybe-undefined`, an entry of
/// [`UnsetSeedFacts::reads`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct UnsetSeedRead {
    /// The read name without the leading `$`.
    pub name: String,
    /// The file byte span of the `$x` token at the read — the same key
    /// [`UndefinedRead::span`] carries, so the out-parameter subtraction joins.
    pub span: Span,
    /// Byte offset of the statement whose adopted docblock seeded the name, so the
    /// confirming reader can reach that docblock through [`SourceTree::stmt_docblock`].
    ///
    /// [`SourceTree::stmt_docblock`]: crate::SourceTree::stmt_docblock
    pub seed_stmt: u32,
}

/// What lowering can say alone about the `unset` pseudo-type idiom (ADR-0087 §4):
/// the reads a declared-possibly-unbound name reaches without a discharging guard,
/// over the top-level script scope.
///
/// **These are candidates, not findings**, and the split is forced rather than
/// chosen. Deciding that `@var \DateTime|unset $x` carries the pseudo-type means
/// lowering the tag, which is `steins-phpdoc`/`steins-contract` work this crate has
/// no edge to; and the CST does not outlive [`SourceTree::parse`], so a caller that
/// *can* lower it cannot hand back seeds afterwards. So the pass runs here over a
/// **syntactic superset** of the seeds — every `$name` token of an adjacent docblock
/// whose text contains the substring `unset`, which is the only spelling any
/// lowering can reach the `ContractTy::Unset` leaf from — and `steins-infer` confirms
/// each candidate by lowering the named tag before emitting. The superset can only
/// ever be too large; an unconfirmed candidate emits nothing.
///
/// Top-level scope only. ADR-0081 §6's silence over a script scope is deliberate —
/// an included file inherits the includer's symbol table, so the CST cannot claim
/// absence — and an explicit `|unset` is exactly the declaration that lifts it, for
/// the declared name and nothing else.
///
/// [`SourceTree::parse`]: crate::SourceTree::parse
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct UnsetSeedFacts {
    /// The candidate reads, in source order.
    pub reads: Vec<UnsetSeedRead>,
    /// The top-level out-parameter candidates, restricted to the seeded names — the
    /// same residue [`Scope::ref_arg_candidates`] leaves the checker, which the script
    /// scope never collects because it reports neither `variable.*` id.
    pub ref_arg_candidates: Vec<UndefinedRead>,
}

// end unset pseudo-type (ADR-0087 §4, issue #396)

/// A recovered parse error with its span (ADR-0003: error-tolerant).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct ParseError {
    pub message: String,
    pub span: Span,
}

/// The lexical form of a source [`Comment`] — the trivia shapes the `@steins-ignore`
/// channel reads (ADR-0023); doc-blocks are exposed too, in case a directive sits in one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub enum CommentKind {
    /// `// …` single-line comment.
    Line,
    /// `# …` hash comment.
    Hash,
    /// `/* … */` block comment.
    Block,
    /// `/** … */` doc-block comment.
    DocBlock,
}

/// A comment trivium recovered from the parse (ADR-0023 inline-ignore channel). `text` is
/// the raw spelling including delimiters; the suppression layer scans it for `@steins-ignore`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct Comment {
    pub kind: CommentKind,
    pub span: Span,
    pub text: String,
}

/// A statically-judgeable form of an `include`/`require` path argument (ADR-0046 §2). Only
/// decidable shapes are represented; everything else is [`IncludePath::Unproven`] — sound,
/// since an unprovable path could pull in out-of-universe code (e.g. compiled template
/// caches) that calls any function with no visible call site.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub enum IncludePath {
    /// A fully-proven literal path (`'inc/util.php'`, or literal-only concatenation
    /// `'a'.'b'`), resolved against the analyzed universe (relative → including file's dir).
    Literal(String),
    /// `__DIR__ . '<suffix>'` — directory-relative literal; the proven suffix resolves
    /// against the including file's own directory (the common project-relative include idiom).
    DirRelative(String),
    /// A path that is not statically proven (a variable, a call, a non-literal
    /// concatenation): a caller-enumeration obstacle.
    Unproven,
}

/// The kind of a dynamic-code construct that can invisibly reach code the
/// call-site sweep cannot see (ADR-0046 §2, "universe havoc").
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub enum DynamismKind {
    /// `eval(<expr>)` — code as data; can call any function with no CST call site.
    Eval,
    /// `include`/`include_once`/`require`/`require_once <path>` — pulls in code; carries
    /// the lowered path so provenness/in-universe resolution is judgeable.
    Include(IncludePath),
    /// A `class_alias(...)` call with a runtime-minted name argument (ADR-0046 §2, ADR-0049
    /// §2), unresolvable by the reference scan. A `class_alias` with both names known at
    /// compile time (literals or `X::class`, issue #36) contributes a [`ClassAliasEdge`]
    /// instead and isn't a dam site. The finding-breadth dam treats this as a dam site;
    /// transform obstacle detection does not (ADR-0049).
    ClassAlias,
    /// A `define(...)` call with a runtime-minted name argument (ADR-0078, issue #198) — a
    /// computed name mints a constant nobody can enumerate, the exact parallel to
    /// [`Self::ClassAlias`]. A `define()` with a literal name instead contributes a
    /// [`GlobalConstDecl`] and isn't a dam site. Damming is narrower than "any dynamism":
    /// read only by the `constant.undefined` ladder, since `define()` can't mint a function or class.
    DefineDynamic,
}

/// One dynamic-code construct in a file (ADR-0046 §2), collected file-wide (every scope,
/// including nested bodies) — distinct from the coarse per-scope [`Scope::poisoned`]
/// value-havoc flag: this records invisible callers/out-of-universe code, a different
/// soundness hole.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct DynamismSite {
    pub kind: DynamismKind,
    /// The construct's source span (its starting line is the vouching key).
    pub span: Span,
}

/// A reflection-driven invocation shape: code reaching a function/method through a value
/// rather than a call site, invisible to the call-site sweep (issue #30). Unlike
/// [`OpaqueConstruct`], this doesn't poison a scope or dam a claim — the list is a guess
/// until measured: shapes named by a cross-analyzer survey, recognized syntactically and so
/// both over- and under-inclusive, inventoried so the guess can be corrected against a corpus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub enum ReflectionKind {
    /// `$r->invoke(...)` / `$r->invokeArgs(...)` — any `->invoke*()` method call
    /// (`ReflectionMethod`, `ReflectionFunction`, `Closure::__invoke`).
    Invoke,
    /// `$r->newInstance(...)` / `->newInstanceArgs(...)` /
    /// `->newInstanceWithoutConstructor()` — any `->newInstance*()` method call.
    NewInstance,
    /// `Closure::bind($fn, $obj, <computed>)` — a rebind whose scope argument isn't a
    /// literal class name (string literal or `X::class`), so the bound private/protected
    /// surface isn't statically known. Literal-scope binds and `$fn->bindTo(...)` aren't
    /// counted — the guess stays narrow.
    ClosureBindComputedScope,
    /// `func_get_args()` inside a declaration whose signature declares any type (param or
    /// return hint): the signature says one thing, the body reads another argument list entirely.
    FuncGetArgsInTypedSignature,
}

impl ReflectionKind {
    /// Every variant, in report order.
    pub const ALL: [Self; 4] =
        [Self::Invoke, Self::NewInstance, Self::ClosureBindComputedScope, Self::FuncGetArgsInTypedSignature];

    /// The kind's label as a posture report spells it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Invoke => "->invoke*()",
            Self::NewInstance => "->newInstance*()",
            Self::ClosureBindComputedScope => "Closure::bind (computed scope)",
            Self::FuncGetArgsInTypedSignature => "func_get_args() in a typed signature",
        }
    }
}

/// One reflection-driven invocation site, collected file-wide like [`DynamismSite`].
/// Consumed only by `steins doctor`'s coverage posture — no checker/dam/transform reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct ReflectionSite {
    pub kind: ReflectionKind,
    pub span: Span,
}

/// A compile-time `class_alias('Target', 'Alias')` edge (ADR-0049 §2 / A2iii): both
/// arguments name a class at compile time (literal, or `X::class` resolved via namespace
/// context, issue #36), so the alias resolves for existence purposes to the target's
/// declaration site. Folded into the project index sharing the duplicate-declaration
/// discipline: a collision with a textual decl, or two alias edges for one name, marks the
/// FQN `Ambiguous`. FQNs are lowercase-normalized, leading `\` stripped, like
/// [`ClassDecl::fqn`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct ClassAliasEdge {
    /// The alias name being minted (`class_alias`'s 2nd arg), lowercase FQN.
    pub alias_fqn: String,
    /// The existing class the alias points at (`class_alias`'s 1st arg), lowercase FQN.
    pub target_fqn: String,
    /// The `class_alias(...)` call's source span.
    pub span: Span,
}

/// A global constant declaration (ADR-0078, issue #198): a `const FOO = …;` statement
/// outside any class-like (resolved against its namespace, like a function), or a
/// `define('FOO', …)` call with a literal name. Class constants are a different namespace
/// ([`ClassDecl::consts`], issue #197) and never appear here.
///
/// The two forms differ: `const` declares into the current namespace, while `define()`
/// always takes an absolute name — `define('FOO', 1)` inside `namespace App;` declares the
/// global `FOO`, not `App\FOO` (`php -r`-witnessed). A `define()` with a non-literal name
/// contributes a [`DynamismKind::DefineDynamic`] dam site instead (same split as
/// `class_alias`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct GlobalConstDecl {
    /// The declared name, normalized by [`normalize_const_fqn`]: leading `\`
    /// stripped, namespace segments lowercased, the final segment case-preserved.
    pub fqn: String,
    /// The declaration's source span.
    pub span: Span,
}

/// Normalize a global constant name into the index's matching key: leading `\` stripped,
/// namespace segments lowercased, final segment left exactly as written.
///
/// PHP's split, not a convenience: constant names are case-sensitive (`define('Foo',1)`
/// leaves `defined('FOO')` false) while the namespace prefix is case-insensitive — with
/// `namespace App; const LOCAL = 'l';`, both `defined('App\LOCAL')` and `defined('app\LOCAL')`
/// are true, `defined('App\local')` false (`php -r`-witnessed on 8.5.9). No version fork:
/// `define()`'s case-insensitive 3rd argument died in PHP 8.0, workspace floor is 8.1
/// (ADR-0011).
#[must_use]
pub fn normalize_const_fqn(name: &str) -> String {
    let name = name.trim_start_matches('\\');
    match name.rfind('\\') {
        Some(pos) => format!("{}{}", name[..=pos].to_ascii_lowercase(), &name[pos + 1..]),
        None => name.to_owned(),
    }
}

/// An anonymous class declaration's inheritance edges (ADR-0049 A4, descendant-closure
/// obstacle detection). Anonymous classes (`new class extends Report {...}`) carry no FQN
/// and never enter the class index, so a "completely enumerated" descendant set of a union
/// member could silently miss one. The declared-receiver lane (S6) reads these edge-only
/// lowerings (parent + implements, no members) to taint closure: any anon-class edge
/// resolving to — or `Unknown` against — a union member forces `Unknown`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct AnonClassEdge {
    /// The `extends` parent as written, if any.
    pub parent: Option<NameRef>,
    /// The interfaces the anonymous class `implements`.
    pub implements: Vec<NameRef>,
    /// The `new class` construct's source span.
    pub span: Span,
}

/// One `foreach` statement, lowered for the loop→`array_map` transform (ADR-0076). Every
/// `foreach` in a file produces one, so the transform's refusal distribution measures how
/// narrow v1 is instead of hiding it (ADR-0076 §4). Syntax reports shape only — purity,
/// `array`/`is_list` proof, and rewrite legality are inference questions answered elsewhere.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct ForeachSite {
    /// The whole `foreach (…) …` statement's span — what the rewrite replaces,
    /// together with [`Self::prev_stmt`]'s span.
    pub span: Span,
    /// The iterated expression when it is a plain `$var` (no `$`); `None` for
    /// every other subject (a call, a property, an offset read, …).
    pub subject: Option<String>,
    /// `true` for the `$k => $v` key form.
    pub key_binding: bool,
    /// `true` when the value target is bound by reference (`as &$v`).
    pub by_ref_binding: bool,
    /// The value target when it is a plain `$var`; `None` for a destructuring
    /// (`as [$a, $b]` / `as list($a, $b)`) or otherwise non-variable target.
    pub value_var: Option<String>,
    /// The body's lowered shape.
    pub body: ForeachBodyShape,
    /// The immediately preceding sibling statement, when `foreach` isn't its block's first statement.
    pub prev_stmt: Option<PrevStmt>,
    /// The end offset of the enclosing variable scope (function/method/closure body, else
    /// the file), so a consumer can scan the remainder for an outliving iteration variable (ADR-0076 §3).
    pub scope_end: u32,
}

/// The statement immediately preceding a [`ForeachSite`], reduced to what the adjacency
/// rule needs: its span (the rewrite consumes it) and whether it's an accumulator initializer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct PrevStmt {
    /// The preceding statement's own span.
    pub span: Span,
    /// The assignment target when the statement is `$name = <anything>;` with the
    /// plain `=` operator and a bare variable lvalue; `None` otherwise.
    pub assign_target: Option<String>,
    /// `true` when that assignment's right-hand side is an empty array literal (`[]` or `array()`).
    pub assigns_empty_array: bool,
}

/// A [`ForeachSite`] body's lowered shape.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct ForeachBodyShape {
    /// How many statements the body holds (`0` for `{}` / a bare `;`).
    pub stmt_count: usize,
    /// The append, when the body is exactly one `$acc[] = <expr>;` statement.
    pub append: Option<AppendStmt>,
    /// `true` when the body has a `break`/`continue`/`return`/`goto` outside any nested
    /// function-like body — the loop can end early, so no whole-array rewrite reproduces it.
    pub early_exit: bool,
}

/// The single `$acc[] = <expr>;` statement of an eligible loop body.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct AppendStmt {
    /// The accumulator variable's name (no `$`).
    pub acc: String,
    /// The appended expression's span — the arrow function's body, verbatim.
    pub value_span: Span,
    /// Every direct variable the appended expression mentions, first-seen order (nested
    /// function-likes included: an arrow body reads the enclosing scope by value).
    pub value_vars: Vec<String>,
    /// `true` when the appended expression writes a variable (embedded assignment, `++`,
    /// `--`) — `fn` captures by value, so such a write isn't equivalence-preserving under the rewrite.
    pub value_writes: bool,
    /// `true` when the appended expression carries a construct the effect scan doesn't
    /// model as a call: `new`/`clone`/`yield`/backtick shell-exec/an ADR-0001 poison
    /// construct, or a scope-sensitive builtin (`compact`/`get_defined_vars`/`func_get_args`/`func_num_args`).
    pub value_unmodelled: bool,
}

// invalid operands (ADR-0078, issue #191)

/// One operator application whose operand types PHP's own arithmetic table can refuse:
/// arithmetic/bitwise/shift binary operators, or unary `-`/`+`/`~` (issue #191). Both
/// operands lower to [`ArgValue`] — the value IR where a literal `[]` and a `$var` carrying
/// an env fact are spelled the same way — so `type.invalid-operand` reads one shape.
/// Purely syntactic, like [`ForeachSite`]: what the operands are is `steins-infer`'s question.
///
/// Three operator families are deliberately absent: `.` (array-in-concat warns, not fatal —
/// issue #193's territory); every comparison (`php -r`-witnessed legal on 8.5.9 for every
/// operand pair, arrays and objects included); `++`/`--` (fatal on an array, but a mutation
/// statement, not an operand expression — out of v1's reach).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub struct OperandSite {
    /// The whole operator application's span (`$a + $b`, `-$a`) — where the finding points.
    pub span: Span,
    /// The operator and its lowered operands.
    pub kind: OperandSiteKind,
    /// The span of the innermost enclosing function-like body (function, method, closure,
    /// arrow function, property hook), or `None` at file scope. Keeps a site inside a
    /// closure from being judged against the enclosing scope's env — `fn() => $s + 1`'s `$s`
    /// is the closure's own binding, so the consumer requires its judged statement to lie inside this body.
    pub enclosing_body: Option<Span>,
}

/// The operator half of an [`OperandSite`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub enum OperandSiteKind {
    /// A binary application `lhs <op> rhs`.
    Binary {
        /// The operator.
        op: BinaryOperandOp,
        /// The left operand, lowered.
        lhs: ArgValue,
        /// The right operand, lowered.
        rhs: ArgValue,
    },
    /// A unary prefix application `<op> operand`.
    Unary {
        /// The operator.
        op: UnaryOperandOp,
        /// The operand, lowered.
        operand: ArgValue,
    },
}

/// The binary operators whose operand types PHP's arithmetic table can refuse. `.`, `??`,
/// comparisons, and logical operators are excluded (see [`OperandSite`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub enum BinaryOperandOp {
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `%`
    Mod,
    /// `**`
    Pow,
    /// `&`
    BitAnd,
    /// `|`
    BitOr,
    /// `^`
    BitXor,
    /// `<<`
    ShiftLeft,
    /// `>>`
    ShiftRight,
}

impl BinaryOperandOp {
    /// The operator as written, for the diagnostic sentence.
    #[must_use]
    pub fn symbol(self) -> &'static str {
        match self {
            BinaryOperandOp::Add => "+",
            BinaryOperandOp::Sub => "-",
            BinaryOperandOp::Mul => "*",
            BinaryOperandOp::Div => "/",
            BinaryOperandOp::Mod => "%",
            BinaryOperandOp::Pow => "**",
            BinaryOperandOp::BitAnd => "&",
            BinaryOperandOp::BitOr => "|",
            BinaryOperandOp::BitXor => "^",
            BinaryOperandOp::ShiftLeft => "<<",
            BinaryOperandOp::ShiftRight => ">>",
        }
    }
}

/// The unary prefix operators whose operand type PHP can refuse. `!` is not here — it's
/// total (`php -r`-witnessed legal on every operand kind, including arrays and objects).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "persist", derive(serde::Serialize, serde::Deserialize))]
pub enum UnaryOperandOp {
    /// `-`
    Minus,
    /// `+`
    Plus,
    /// `~`
    BitNot,
}

impl UnaryOperandOp {
    /// The operator as written, for the diagnostic sentence.
    #[must_use]
    pub fn symbol(self) -> &'static str {
        match self {
            UnaryOperandOp::Minus => "-",
            UnaryOperandOp::Plus => "+",
            UnaryOperandOp::BitNot => "~",
        }
    }
}

// end invalid operands (ADR-0078, issue #191)
