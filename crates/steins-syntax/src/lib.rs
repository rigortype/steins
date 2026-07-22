//! Steins' syntax-tree contract and its Mago parser backend (ADR-0003).
//!
//! # Encapsulation (hard rule)
//!
//! The pinned Mago fork is a dependency of *this crate only*, and **no Mago type
//! appears in this crate's public API**. Everything the analyzer sees is the
//! owned, lowered representation defined here: [`SourceTree`] and its associated
//! plain-data structs. This is the seam ADR-0003 requires so parser backends can
//! be swapped without touching the analysis crates.
//!
//! For the first vertical slice the lowered tree is deliberately small: it
//! captures exactly what the `type.argument-mismatch` proof-layer check needs —
//! `declare(strict_types=1)`, user-defined function declarations with scalar
//! parameter types, and function-call expressions with literal arguments. Spans
//! are byte offsets, convertible to 1-based line/column via [`SourceTree::position`].

use mago_allocator::LocalArena;
use mago_database::file::FileId;
use mago_span::HasSpan;
use mago_syntax::cst::Access;
use mago_syntax::cst::Argument;
use mago_syntax::cst::ArrayElement;
use mago_syntax::cst::Attribute;
use mago_syntax::cst::Binary;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::Call;
use mago_syntax::cst::Construct;
use mago_syntax::cst::Class;
use mago_syntax::cst::ClassLikeMember;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::DeclareItem;
use mago_syntax::cst::Expression;
use mago_syntax::cst::Function;
use mago_syntax::cst::FunctionCall;
use mago_syntax::cst::Hint;
use mago_syntax::cst::Identifier;
use mago_syntax::cst::Instantiation;
use mago_syntax::cst::Literal;
use mago_syntax::cst::Method;
use mago_syntax::cst::MethodBody;
use mago_syntax::cst::Modifier;
use mago_syntax::cst::Node;
use mago_syntax::cst::PartialApplication;
use mago_syntax::cst::PartialArgument;
use mago_syntax::cst::PlainProperty;
use mago_syntax::cst::Property;
use mago_syntax::cst::PropertyItem;
use mago_syntax::cst::Program;
use mago_syntax::cst::Statement;
use mago_syntax::cst::Trivia;
use mago_syntax::cst::TriviaKind;
use mago_syntax::cst::UnaryPrefixOperator;
use mago_syntax::cst::UseItems;
use mago_syntax::cst::Variable;

use std::collections::HashMap;
use std::collections::HashSet;

// ---------------------------------------------------------------------------
// Public, Mago-free representation.
// ---------------------------------------------------------------------------

/// A byte-offset span into the source file. `end` is exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
/// (whole-project slice). This is the syntactic input the resolution rules key
/// on; the resolution itself (namespace fallback, `use` imports, builtin
/// catalog) lives in `steins-infer` against the project index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefKind {
    /// `\Foo\bar` — leading backslash: an absolute name; no import or current
    /// namespace applies. (The stored `raw` has the leading `\` stripped.)
    FullyQualified,
    /// `Sub\bar` — contains a namespace separator but no leading one: relative
    /// to the current namespace, first segment subject to `use` imports.
    Qualified,
    /// `bar` — a single bare segment: unqualified (subject to imports, then the
    /// namespace/global fallback rules).
    Unqualified,
}

/// A reference to a function or class name as written at a use site, carrying
/// exactly what cross-file resolution needs: the raw spelling (leading `\`
/// stripped, case preserved — PHP names are case-insensitive so callers fold
/// case at lookup), the qualification [`RefKind`], and the byte `offset` of the
/// reference (used to select the enclosing namespace context via
/// [`SourceTree::ctx_at`]).
///
/// `offset` is intentionally excluded from equality/hashing: two textually
/// identical references at different positions denote the same name.
#[derive(Debug, Clone)]
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
    /// The last (unqualified) segment of the raw name — the simple name used for
    /// diagnostics and same-file legacy paths.
    #[must_use]
    pub fn simple(&self) -> &str {
        match self.raw.rfind('\\') {
            Some(pos) => &self.raw[pos + 1..],
            None => &self.raw,
        }
    }
}

/// A file-region namespace context: the enclosing namespace name plus the `use`
/// imports in scope there (ADR: whole-project name resolution). Names and import
/// targets are **case-preserved** (no leading/trailing `\`); import-map *keys*
/// (the bound local alias) are lowercased, since PHP name lookup is
/// case-insensitive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NsCtx {
    /// The namespace path (`App\Models`), or empty for the global namespace.
    pub namespace: String,
    /// Class/namespace imports: lowercased alias → case-preserved target FQN.
    pub class_imports: HashMap<String, String>,
    /// `use function` imports: lowercased alias → case-preserved target FQN.
    pub fn_imports: HashMap<String, String>,
}

impl NsCtx {
    fn global() -> Self {
        Self { namespace: String::new(), class_imports: HashMap::new(), fn_imports: HashMap::new() }
    }
}

impl std::hash::Hash for NsCtx {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        // Order-independent: hash the namespace plus the sizes, so `NsCtx` can sit
        // inside the `Hash`-deriving [`SourceTree`] despite holding hash maps.
        self.namespace.hash(state);
        self.class_imports.len().hash(state);
        self.fn_imports.len().hash(state);
    }
}

/// The scalar native types the slice reasons about (PHP 8.1+; ADR-0011).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

/// One member of a native union type: one of the four scalars, or a `false` /
/// `true` bool-literal pseudo-member (PHP allows `false`/`true` as literal type
/// members, e.g. `string|false`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TypeMember {
    /// A full scalar type (`int`, `float`, `string`, `bool`).
    Scalar(ScalarType),
    /// A `false` / `true` literal type. It accepts **only** the exact matching
    /// bool value — no other value coerces into it (empirically verified against
    /// PHP 8.5: `0`/`""`/`true` into a `false`-only type all `TypeError`).
    BoolLiteral(bool),
}

impl TypeMember {
    /// The PHP keyword spelling of this member, for diagnostic messages.
    #[must_use]
    pub fn keyword(self) -> &'static str {
        match self {
            TypeMember::Scalar(s) => s.keyword(),
            TypeMember::BoolLiteral(false) => "false",
            TypeMember::BoolLiteral(true) => "true",
        }
    }
}

/// A native scalar/union parameter **or return** type Steins reasons about,
/// lowered from a single scalar, `?T`, or a `T1|T2|…[|null]` union of the four
/// scalars (plus `false`/`true` literal members). Any member that is not a
/// scalar or a bool-literal (a class, `array`, `mixed`, `iterable`, `callable`,
/// `object`, an intersection, `self`/`static`/`parent`, `void`/`never`, …)
/// lowers the **whole** type to `None` so the checker stays silent on it
/// (zero-FP; ADR-0002).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NativeType {
    /// The union members, in source order. Always non-empty: a hint that would
    /// lower to zero members (e.g. standalone `null`) lowers to `None` instead.
    /// Membership tests are existential, so duplicates are harmless.
    pub members: Vec<TypeMember>,
    /// `true` when `?T`, or a `null` union member, makes `null` acceptable.
    pub nullable: bool,
}

impl NativeType {
    /// Render the type for a diagnostic message: `int`, `?int`, `int|string`,
    /// `string|false`, `int|string|null`.
    #[must_use]
    pub fn render(&self) -> String {
        let mut parts: Vec<&str> = self.members.iter().map(|m| m.keyword()).collect();
        if self.nullable {
            if parts.len() == 1 {
                return format!("?{}", parts[0]);
            }
            parts.push("null");
        }
        parts.join("|")
    }
}

/// A single declared parameter.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Param {
    /// Parameter name without the leading `$`.
    pub name: String,
    /// Native scalar/union type, or `None` when untyped / non-scalar / complex.
    pub ty: Option<NativeType>,
    /// `...$x` — the checker skips this and every later position.
    pub variadic: bool,
    /// `&$x` — by-reference; the checker skips it.
    pub by_ref: bool,
    /// `$x = null` — a literal `null` default. PHP makes such a parameter
    /// **implicitly nullable** (its effective declared type accepts `null`), and
    /// PHPStan honors this; the phpdoc contract check uses it to accept `null`
    /// even against a non-nullable `@param` type (avoiding a false positive on the
    /// common `string $x = null` idiom).
    pub has_null_default: bool,
    pub span: Span,
}

/// A structural effect-origin candidate found by scanning a function body's CST
/// subtree (ADR-0005 effect envelopes). Syntax only reports *where* a primitive
/// effect could arise; the catalog/inference layer decides which are proven
/// findings (uncatalogued builtins widen to silence, same-file user calls become
/// propagation edges — [`steins_catalog::effect_labels`] and the effects pass).
///
/// The scan does **not** descend into nested function/closure/class bodies —
/// those are separate scopes (closures are deferred in this slice). It *does*
/// see constructs nested inside control flow (an `echo` inside an `if`), which
/// is why the effects pass reads this instead of the linear trace.
///
/// The scan is **structural**, not reachability-aware: an `echo` in provably
/// dead code is still reported as an origin. This is deliberate — an effect
/// envelope (ADR-0005) is a contract about the function's *code*, not a single
/// execution path, so the mere presence of an effectful construct in the body is
/// what `Pure` forbids.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EffectOrigin {
    /// A call to a statically-named function at `span` (the callee identifier).
    /// `name` carries the full reference (raw spelling + qualification) so the
    /// effects pass can resolve it project-wide: it may resolve to a builtin
    /// (classified via the catalog), a user function anywhere in the project (an
    /// effect propagation edge), or nothing (ambiguous → taints exhaustiveness).
    /// Dynamic and method calls are not recorded here.
    Call { name: NameRef, span: Span },
    /// An `echo` / `print` / short-echo (`<?=`) construct at `span` — the
    /// `output` effect. `keyword` is the spelling for diagnostics.
    Output { keyword: &'static str, span: Span },
    /// An `exit` / `die` construct at `span` — the `exit` effect (ADR-0019 rule
    /// 4: `Pure` forbids exit). `keyword` is the spelling for diagnostics.
    Exit { keyword: &'static str, span: Span },
    /// A method or static-method call whose *receiver* is one the effects pass
    /// can resolve without a flow environment (`$this->`, `self::`, `parent::`,
    /// `Foo::`, `new Foo()->`). Recorded so a `#[\Steins\Pure]` method can have
    /// its resolved method→method effect edges propagated (the class-world
    /// analogue of the `EffectOrigin::Call` function edge). Dynamic receivers
    /// (`$var->m()`, `static::m()`) are **not** recorded — no provable edge.
    MethodCall { receiver: EffectRecv, method: String, span: Span },
    /// A call the scan cannot classify to a statically-named target: a dynamic
    /// function call (`$f()`, `$arr['x']()`), or a method / static call whose
    /// receiver or selector is not statically resolvable (`$obj->m()`,
    /// `$var::m()`, `$o->$m()`). It contributes **no** proven effect finding (it
    /// stays silent, like every unprovable effect), but it marks the enclosing
    /// body's effect set **non-exhaustive**: the analyzer cannot prove the call
    /// is effect-free. Consumed only by the effects-exhaustiveness bit (the
    /// annotate `…?` marker); the envelope check ignores it. `span` is the call.
    Opaque { span: Span },
    /// A call to a statically-named function that passes at least one **resolvable
    /// callback argument** (an inline closure, a first-class callable, or a
    /// string-literal function name), at the given positional index (ADR-0033
    /// invocation shapes). Emitted *instead of* [`Self::Call`] for such calls. The
    /// effects pass consults `steins_catalog::invocation_shape` on `callee`: for a
    /// known higher-order builtin it edges to the callback at the shape's callback
    /// param (its own base is pure); otherwise it falls back to normal `callee`
    /// resolution (the callback is just an argument). `arg_count` is the positional
    /// arity, so a resolvable callback at a *non*-callback position still taints.
    HigherOrder { callee: NameRef, callbacks: Vec<(usize, CallbackRef)>, arg_count: usize, span: Span },
    /// A direct `$fn()` variable call resolved (by a body-local single-assignment
    /// analysis) to a known callback (ADR-0033). Its effects join the caller's
    /// (immediate invocation); `span` is the call. An unresolvable `$fn()` stays
    /// [`Self::Opaque`] (the honest taint).
    Callback { cbref: CallbackRef, span: Span },
}

/// A resolvable callback argument (ADR-0033 invocation shapes): an inline
/// closure/arrow scope (by definition-site offset), or a named free function (a
/// first-class callable, or a string-literal function name). Consumed by the
/// effects/throws passes to join the callback's sets into the caller's.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CallbackRef {
    /// An inline closure/arrow whose body scope is at this definition offset.
    Closure(u32),
    /// A named free function passed as a callback (`'strtolower'`, `strtolower(...)`).
    Named(NameRef),
}

/// The receiver of an [`EffectOrigin::MethodCall`], restricted to the forms the
/// effects pass can resolve to a same-file target without a flow environment.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EffectRecv {
    /// `$this->m()` — resolved against the enclosing class chain under the
    /// final/private guard (a non-final public method may be overridden).
    This,
    /// `self::m()` — same guard as `$this` (conservative; `self::` is early-bound
    /// in PHP but the guard is only ever stricter, so it stays sound).
    SelfKw,
    /// `parent::m()` — resolved on the parent chain, exact (parent is fixed).
    Parent,
    /// `Foo::m()` or `new Foo()->m()` — resolved on the referenced class's chain,
    /// exact. Carries the full [`NameRef`] so the class resolves project-wide to
    /// its FQN.
    ClassName(NameRef),
}

/// One `catch` clause's caught types plus its bound variable, for the throw
/// damming walk (ADR-0040). A multi-catch `catch (A|B $e)` records several
/// `classes`; a caught type the lowering cannot name statically (a dynamic or
/// non-identifier hint member) sets `has_unresolvable`, which forces absorption
/// to `Maybe` for the whole clause (the consumer-inverted safe side).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CatchClause {
    /// The statically-named caught classes (resolved to FQNs project-wide at
    /// inference time). Empty with `has_unresolvable` set means "caught, but we
    /// cannot name what".
    pub classes: Vec<NameRef>,
    /// The `$e` variable this clause binds (no `$`), for rethrow precision.
    pub var: Option<String>,
    /// A caught-type member the lowering could not name (→ absorption `Maybe`).
    pub has_unresolvable: bool,
}

/// What a [`ThrowOrigin`] contributes to a body's throw set (ADR-0040). The
/// explicit-throw variants carry the thrown class as written (resolved at
/// inference time); the call variants are propagation edges (the callee's
/// escaping throws flow in, re-filtered through this origin's guards).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ThrowKind {
    /// `throw new X(...)` — `X` is the class as written.
    New(NameRef),
    /// `throw $e` where `$e` is an enclosing catch's parameter — re-emits exactly
    /// that catch's absorbed set (ADR-0040 rethrow precision).
    Rethrow { caught: Vec<NameRef>, has_unresolvable: bool },
    /// A statically-named function call whose throws propagate.
    Call(NameRef),
    /// A method/static call with a statically-resolvable receiver (the class-world
    /// propagation edge, resolved exactly like [`EffectOrigin::MethodCall`]).
    MethodCall { receiver: EffectRecv, method: String },
    /// An unresolvable throw (`throw $x` of a non-catch var, `throw <expr>`) or a
    /// dynamic/unresolved call — contributes no reportable throw but **taints
    /// throw-exhaustiveness** (ADR-0040 safe side; envelope stays silent).
    Taint,
    /// A call to a named function passing resolvable callback argument(s) — the
    /// throw analogue of [`EffectOrigin::HigherOrder`] (ADR-0033). The callee's own
    /// throws AND the callback's throws (at the invocation shape's callback param)
    /// propagate, re-filtered through this origin's guards.
    HigherOrder { callee: NameRef, callbacks: Vec<(usize, CallbackRef)>, arg_count: usize },
    /// A direct `$fn()` call resolved to a known callback — the throw analogue of
    /// [`EffectOrigin::Callback`]: the callback's throws propagate (ADR-0033).
    Callback { cbref: CallbackRef },
}

/// One throw-relevant construct in a function/method body, with the ordered
/// enclosing `try`/`catch` guards that may dam it (ADR-0040 damming). Produced by
/// a structural CST walk (independent of the trace IR), for *all* functions and
/// methods, because the throw fixpoint propagates callee throw sets to callers
/// regardless of annotations.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ThrowOrigin {
    pub kind: ThrowKind,
    /// The span of the throwing/calling construct (diagnostic position).
    pub span: Span,
    /// The enclosing `try` catch-guards, **innermost first**. Each entry is one
    /// enclosing try's list of catch clauses. A throw is matched against each
    /// guard from innermost outward; `finally` bodies and a try's own catch
    /// bodies do **not** carry that try's own guard (the scanner omits it).
    pub guards: Vec<Vec<CatchClause>>,
}

/// A recognized effect-envelope declaration (ADR-0005/0006/0018): the upper
/// bound of effects a function or method promises not to exceed.
///
/// The `labels` are hierarchical dot-path effect labels (ADR-0018). The **empty**
/// set is the tightest bound — pure — spelled `#[\Steins\Pure]`; a non-empty set
/// comes from `#[\Steins\Effect('io', 'nondet.time')]`. When both `#[\Steins\Pure]`
/// and `#[\Steins\Effect(...)]` decorate the same declaration the two are
/// contradictory (`Pure` = empty upper bound, the tighter of the two); Pure wins
/// and `labels` is empty, with no diagnostic about the contradiction in this
/// slice (see [`attrs_effect_envelope`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EffectEnvelope {
    /// The declared effect labels (ADR-0018 dot-paths). Empty = `Pure`.
    pub labels: Vec<String>,
    /// The span of the recognized attribute (for diagnostic positions — e.g.
    /// `effect.unknown-label` points here).
    pub span: Span,
}

/// A user-defined function declaration (top-level or namespaced). `name` is the
/// simple (unqualified) name as written at the declaration site.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FunctionDecl {
    pub name: String,
    /// The fully-qualified name, lowercase-normalized (namespace + `\` + name;
    /// PHP function/namespace names are case-insensitive). The project index
    /// keys on this. For a global (un-namespaced) function it equals the
    /// lowercased simple name.
    pub fqn: String,
    pub params: Vec<Param>,
    /// The native scalar/union return type, or `None` when untyped / non-scalar
    /// / `void` / `never` — the return-type check skips those (zero-FP).
    pub ret: Option<NativeType>,
    pub span: Span,
    /// The recognized `#[\Steins\Pure]` / `#[\Steins\Effect(...)]` envelope on
    /// this function, if present (ADR-0005/0006/0018). `Some` opts the function
    /// into always-on envelope checking. Recognition is conservative — see
    /// [`attrs_effect_envelope`].
    pub effect_envelope: Option<EffectEnvelope>,
    /// Every structural effect-origin candidate in the body subtree, in source
    /// order (see [`EffectOrigin`]). Computed for *all* functions, not just
    /// `Pure`-declared ones, because the effects pass propagates a callee's
    /// effects to `Pure` callers regardless of the callee's own annotations.
    pub effect_origins: Vec<EffectOrigin>,
    /// Every throw-relevant construct in the body, with its enclosing try/catch
    /// guards (ADR-0040 damming). Computed for *all* functions (the throw
    /// fixpoint propagates callee throws regardless of annotations).
    pub throw_origins: Vec<ThrowOrigin>,
    /// The raw `/** … */` docblock trivia immediately preceding this declaration,
    /// if any (only whitespace between it and the declaration head — the same
    /// association discipline as attributes; ADR-0029). The phpdoc bridge parses
    /// `@param`/`@return` tags out of it into phpdoc envelopes.
    pub docblock: Option<String>,
}

/// A method's declared visibility. Absent visibility modifiers default to
/// `Public` (PHP semantics).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Visibility {
    Public,
    Protected,
    Private,
}

/// A user-defined method declaration — the class-world analogue of
/// [`FunctionDecl`], carrying the same param/pure-envelope/effect-origin data
/// plus the modifiers method resolution needs (ADR-0001 sound dispatch).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MethodDecl {
    /// The simple method name as written (case is preserved; matching is
    /// case-insensitive — PHP method names are).
    pub name: String,
    pub params: Vec<Param>,
    /// The native scalar/union return type, or `None` when untyped / non-scalar
    /// / `void` / `never` (the return-type check skips those; zero-FP).
    pub ret: Option<NativeType>,
    /// The span of the method name identifier (for diagnostic positions).
    pub span: Span,
    /// The recognized effect envelope, if declared (see [`FunctionDecl`]).
    pub effect_envelope: Option<EffectEnvelope>,
    /// Structural effect-origin candidates in the body (see [`EffectOrigin`]).
    /// Empty for abstract methods (no body).
    pub effect_origins: Vec<EffectOrigin>,
    /// Throw-relevant constructs with their try/catch guards (ADR-0040). Empty
    /// for abstract methods (no body).
    pub throw_origins: Vec<ThrowOrigin>,
    pub visibility: Visibility,
    pub is_static: bool,
    pub is_final: bool,
    pub is_abstract: bool,
    /// `true` iff the method name is `__construct` (case-insensitive).
    pub is_constructor: bool,
    /// The raw `/** … */` docblock trivia immediately preceding this method, if
    /// any (association discipline as [`FunctionDecl::docblock`]).
    pub docblock: Option<String>,
}

/// A class property declaration (ADR-0036 object state). Covers both plain
/// `public int $x = 0;` members and **promoted constructor parameters**
/// (`public function __construct(public readonly int $x)`), which are properties
/// too (they carry a native type and populate the object's props at construction).
///
/// Static properties are lowered (so the class surface is complete) but are
/// **never tracked in the heap** — they are global state, out of the object-state
/// slice (ADR-0036 "Out of stage 1"); the heap walk skips `is_static` props.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PropertyDecl {
    /// Property name without the leading `$`.
    pub name: String,
    /// The native scalar/union type, or `None` when untyped / non-scalar / complex
    /// (same lowering as a param/return type; the property-mismatch check skips
    /// `None`-typed props, zero-FP).
    pub ty: Option<NativeType>,
    /// `true` when declared `readonly` (or a promoted `readonly` ctor param). A
    /// readonly prop, once established, is sweep-immune (ADR-0036 readonly immunity).
    pub readonly: bool,
    /// `true` for a `static` property — lowered but never heap-tracked.
    pub is_static: bool,
    pub visibility: Visibility,
    /// `true` when the declaration carries a default value (`= …`). For a promoted
    /// param, `true` when the param has a default.
    pub has_default: bool,
    /// The lowered default value, when it is representable (a literal / array / …).
    /// A non-representable default lowers to `None` (the prop simply starts unknown).
    pub default: Option<ArgValue>,
    /// `true` when this property is a promoted constructor parameter. Promoted
    /// params are checked as constructor arguments (the ctor param check), so the
    /// property-assign check skips them to avoid a double-report (ADR-0036).
    pub promoted: bool,
    /// The raw `/** … */` docblock preceding a plain property (for `@var` contract
    /// extraction; promoted params carry `@param` on the ctor, not `@var`, so this
    /// stays `None` for them).
    pub docblock: Option<String>,
    pub span: Span,
}

/// A user-defined class **or interface** declaration (top-level or namespaced).
/// Interfaces are lowered too (ADR-0033 Liskov), distinguished by
/// [`Self::is_interface`]; their methods are abstract signatures carrying effect
/// envelopes and `@throws` docblocks (the abstraction envelopes Liskov checks
/// against). Traits and enums are still **not** lowered (a class that *uses* a
/// trait sets [`ClassDecl::uses_traits`] so resolution gives up on it).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ClassDecl {
    /// Simple (unqualified) class name as written at the declaration site (used
    /// for diagnostics).
    pub name: String,
    /// The fully-qualified name, lowercase-normalized. The project index keys on
    /// this; for a global class it equals the lowercased simple name.
    pub fqn: String,
    pub is_final: bool,
    /// `true` when this declaration is an `interface` (not a `class`). Interface
    /// methods are abstract; they carry envelopes/`@throws` but no bodies.
    pub is_interface: bool,
    /// The `extends` parent as written, if any (raw spelling + qualification).
    /// Method resolution resolves this to an FQN against the project index and
    /// walks the chain; a parent not defined anywhere in the project makes the
    /// chain incomplete (→ unknown → silent). For an interface this is its first
    /// extended interface (further ones go in [`Self::implements`]).
    pub parent: Option<NameRef>,
    /// The interfaces this class `implements` (ADR-0033 Liskov abstraction
    /// carriers). For an interface declaration, the interfaces it `extends` beyond
    /// the first. Each resolves to an FQN project-wide at use time.
    pub implements: Vec<NameRef>,
    pub methods: Vec<MethodDecl>,
    /// The class's properties (plain members + promoted constructor params;
    /// ADR-0036). Static properties are included but never heap-tracked.
    pub properties: Vec<PropertyDecl>,
    /// `true` if the class `use`s any trait. Trait methods are merged into the
    /// class at compile time but their bodies live elsewhere, so a
    /// trait-using class is treated as unresolvable (give up → silent).
    pub uses_traits: bool,
    /// The span of the class name identifier.
    pub span: Span,
}

/// The value of a call argument (or an assignment right-hand side), restricted
/// to what the slice can prove about.
///
/// The first five variants are *literals* — concrete, self-evident values. The
/// [`ArgValue::Var`] and [`ArgValue::Call`] variants are the value-propagation
/// carriers (ADR-0001): a bare local variable reference, and a call to a
/// statically-named function, respectively. They are *not* proven values on
/// their own — the checker resolves them against a per-scope linear trace
/// before deciding anything. Everything else lowers to [`ArgValue::Other`].
#[derive(Debug, Clone, PartialEq)]
pub enum ArgValue {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Null,
    /// A bare local variable reference `$name` (name stored without the `$`).
    Var(String),
    /// A call `name(args...)` to a statically-named function. `args` are the
    /// lowered argument values (only zero-argument calls are resolvable in this
    /// slice, so the vector's contents matter only for `is_empty()`).
    Call(String, Vec<ArgValue>),
    /// `new ClassName(args...)` — a construction rvalue. [`NameRef`] is the class
    /// reference as written (resolved to an FQN project-wide at use time).
    /// Carried so an assignment `$x = new Foo(...)` can record `$x`'s **exact
    /// class** in the propagation environment (the object's runtime class is
    /// fixed at construction). Not a scalar literal — it never flows into a
    /// scalar type check.
    New(NameRef, Vec<ArgValue>),
    /// An array literal `[...]` / `array(...)` whose keys are all literal-or-absent
    /// and whose element values recursively lower (ADR-0001 array values in the
    /// trace IR). Each entry pairs a lowered [`ArrayKey`] with its value. A spread
    /// (`...`), an unrepresentable element, or a non-literal key lowers the **whole**
    /// array to [`ArgValue::Other`] (the safe side). Keys carry PHP key-normalization
    /// (`"5"` → `Int(5)`, floats truncate, `bool`→`int`, `null`→`""`); auto keys
    /// (`ArrayKey::Auto`) receive their next-int position during normalization
    /// ([`normalize_array`]), where duplicate keys resolve last-wins.
    Array(Vec<(ArrayKey, ArgValue)>),
    /// A ternary `$c ? A : B` in rvalue position, lowered as a **conditional
    /// value** (ADR-0031 stage 1): the walk evaluates `cond` against the env and,
    /// when decided, resolves to the chosen arm; when undecided it joins the two
    /// arms (a `OneOf` if both are literal, else unknown). Short-ternary `?:` and
    /// null-coalescing `??` are **not** lowered here — they widen to
    /// [`ArgValue::Other`] this stage (their operands need negative/definedness
    /// facts the domain does not yet carry).
    Ternary { cond: Box<CondExpr>, then_val: Box<ArgValue>, else_val: Box<ArgValue> },
    /// A closure value (ADR-0033): a `function (...) use (...) {...}` / arrow
    /// `fn(...) => …` expression lowered to its own [`Scope`], or a first-class
    /// callable (`strtolower(...)`) naming a function target. Carried in the trace
    /// so an assignment `$f = fn(...) => …;` records a [`Fact`]-carrying closure
    /// value (in `steins-infer`), and a later `$f(...)` resolves by binding descent
    /// into the closure's scope. Not a scalar — never flows into a scalar check.
    Closure(ClosureRef),
    /// A property read `$var->prop` in rvalue position (ADR-0036 object state). Only
    /// a **simple variable receiver** is represented (`$this->p` uses `var = "this"`);
    /// a chain `$a->b->c` or a dynamic property name (`$a->$p`) lowers to
    /// [`ArgValue::Other`] this slice. The walk resolves it against the heap: a known
    /// object ref with a props entry flows that fact; an unknown receiver yields no
    /// fact (silent).
    PropFetch { var: String, prop: String },
    /// `clone $var` (ADR-0036): a shallow copy of the object `$var` holds. The walk
    /// mints a NEW allocation id with a COPY of the source object's props (PHP shallow
    /// clone), so post-clone writes to one are invisible to the other. Only a bare
    /// variable operand is represented; `clone <expr>` lowers to [`ArgValue::Other`].
    Clone(String),
    Other,
}

/// Identifies the target of an [`ArgValue::Closure`] (ADR-0033). Either an
/// anonymous closure/arrow expression lowered to its own [`Scope`] (addressed by
/// the definition-site byte offset, matching [`ScopeOwner::Closure`]), or a
/// first-class callable naming a free function.
///
/// The captured environment snapshot (by-value `use`/arrow auto-capture) is **not**
/// stored here — `captures` lists only the captured *names*; the value snapshot of
/// each is taken at closure-creation time by the inference walk (reading the
/// definition-site env), which is the semantically correct PHP by-value capture.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ClosureRef {
    /// A closure/arrow expression with its own scope at `def_offset` (the closure
    /// keyword's byte offset; the closure scope's [`ScopeOwner::Closure`] carries
    /// the same). `captures` are the by-value captured variable names — explicit
    /// `use ($x)` for closures, the free variables of the body for arrow fns.
    Anonymous { def_offset: u32, captures: Vec<String> },
    /// A first-class callable of a named free function: `strtolower(...)`. Resolves
    /// as a function name through the existing project/catalog resolution. (Method
    /// and static first-class callables — `$o->m(...)`, `Foo::m(...)` — lower to
    /// [`ArgValue::Other`] this slice; documented deferral.)
    FunctionName(NameRef),
}

/// A lowered array-literal key. `Auto` is an absent key (`[$a, $b]`) that receives
/// its concrete integer position only during [`normalize_array`] (PHP next-int
/// rules); `Int`/`Str` are already-normalized explicit keys.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ArrayKey {
    /// An absent key — normalized to the next integer position.
    Auto,
    /// An integer key (already PHP-normalized: integer-like string keys, floats,
    /// and bools all fold to this).
    Int(i64),
    /// A string key that is not integer-like.
    Str(String),
}

/// A fully PHP-normalized array key (no `Auto`): the runtime key an entry occupies.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NormKey {
    Int(i64),
    Str(String),
}

impl NormKey {
    /// Render the key for a compact array message (`5`, `'foo'`).
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            NormKey::Int(i) => i.to_string(),
            NormKey::Str(s) => format!("'{s}'"),
        }
    }
}

/// Resolve an array literal's raw `(ArrayKey, value)` entries to their PHP runtime
/// key→value map, applying next-int assignment for `Auto` keys and **last-wins**
/// for duplicates (a repeated key updates the value in place, keeping the first
/// position — PHP semantics). The result is insertion-ordered.
#[must_use]
pub fn normalize_array(items: &[(ArrayKey, ArgValue)]) -> Vec<(NormKey, ArgValue)> {
    let mut out: Vec<(NormKey, ArgValue)> = Vec::with_capacity(items.len());
    // PHP's next auto-index: one past the largest integer key seen so far
    // (explicit or auto). Starts at 0; never goes negative below that floor.
    let mut next_auto: i64 = 0;
    for (k, v) in items {
        let key = match k {
            ArrayKey::Auto => {
                let i = next_auto;
                next_auto = next_auto.saturating_add(1);
                NormKey::Int(i)
            }
            ArrayKey::Int(i) => {
                if *i >= next_auto {
                    next_auto = i.saturating_add(1);
                }
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
            ArgValue::New(name, args) => {
                name.hash(state);
                args.hash(state);
            }
            ArgValue::Array(items) => items.hash(state),
            ArgValue::Ternary { cond, then_val, else_val } => {
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

    /// Render the value as it should appear in a diagnostic message.
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            ArgValue::Int(v) => v.to_string(),
            ArgValue::Float(v) => {
                // Keep a float visibly a float: `5.0`, not `5`.
                if v.fract() == 0.0 && v.is_finite() { format!("{v:.1}") } else { v.to_string() }
            }
            ArgValue::Str(v) => format!("\"{v}\""),
            ArgValue::Bool(v) => v.to_string(),
            ArgValue::Null => "null".to_owned(),
            ArgValue::Var(v) => format!("${v}"),
            ArgValue::Call(name, _) => format!("{name}()"),
            ArgValue::New(name, _) => format!("new {}()", name.simple()),
            ArgValue::Array(items) => render_array(items),
            ArgValue::Ternary { then_val, else_val, .. } => {
                format!("(… ? {} : {})", then_val.render(), else_val.render())
            }
            ArgValue::Closure(ClosureRef::FunctionName(n)) => format!("{}(...)", n.simple()),
            ArgValue::Closure(ClosureRef::Anonymous { .. }) => "Closure".to_owned(),
            ArgValue::PropFetch { var, prop } => format!("${var}->{prop}"),
            ArgValue::Clone(v) => format!("clone ${v}"),
            ArgValue::Other => "<expr>".to_owned(),
        }
    }
}

/// Render an array literal compactly for a diagnostic message: `['a', 'b']`,
/// `['k' => 1]`, list-shaped arrays without keys, truncating with `…` after the
/// first five entries.
fn render_array(items: &[(ArrayKey, ArgValue)]) -> String {
    let normalized = normalize_array(items);
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

/// Render an array element value in PHP-literal style (single-quoted strings, so
/// a rendered array reads like source: `['a', 'b']`); non-strings defer to the
/// shared [`ArgValue::render`].
fn render_array_value(v: &ArgValue) -> String {
    match v {
        ArgValue::Str(s) => format!("'{s}'"),
        other => other.render(),
    }
}

/// A single positional call argument.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Arg {
    pub value: ArgValue,
    pub span: Span,
}

/// What a [`CallExpr`] is called *on* — the receiver dimension that the
/// class-world resolution rules dispatch on (ADR-0001 sound dispatch). Plain
/// function calls stay `Function`, so every existing function-world path is
/// unchanged; the other variants are the method/static/constructor forms whose
/// resolvability depends on the receiver's exactness (see `steins-infer`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Callee {
    /// `f(args...)` — a statically-named function (the last, unqualified name).
    Function(String),
    /// `$recv->m(args...)` / `$recv?->m(args...)` — an instance-method call.
    /// `nullsafe` is `true` for the `?->` form, whose call on a `null` receiver
    /// is defined (short-circuits to `null`), so the `call.on-null` proof must
    /// never fire on it.
    Method { receiver: Receiver, method: String, nullsafe: bool },
    /// `Class::m(args...)` — a static (scope-resolution `::`) call.
    Static { class: StaticClass, method: String },
    /// `new Class(args...)` — a constructor call (`args` are the ctor args).
    /// `class` is the class reference as written (resolved to an FQN at use).
    Construct { class: NameRef },
    /// `$fn(args...)` — a call through a bare local variable (ADR-0033). The
    /// variable name is retained (no `$`) so the propagation walk can resolve it
    /// against the env: a proven closure fact descends into the closure's scope, a
    /// proven `Singleton(Str)` fact resolves it as a function name. An unresolved
    /// `$fn` stays honestly opaque (no proven target, exhaustiveness taints).
    DynamicVar(String),
    /// A receiver or method name the lowering cannot represent (dynamic method
    /// name, `$obj[...]->m()`, `$var::m()`, `$arr['x']()`, …). Never resolves.
    Dynamic,
}

/// The object an instance-method call is dispatched on, restricted to the forms
/// resolution can reason about.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Receiver {
    /// `$this->m()` — inside a class body.
    This,
    /// `$var->m()` — resolvable only when the environment knows `$var`'s exact
    /// class (`$var = new Foo();`).
    Var(String),
    /// `(new Foo(...))->m()` — an exact-class receiver (runtime class is the
    /// referenced class, resolved to an FQN project-wide).
    New(NameRef),
}

/// The class portion of a static `Class::m()` call, as written.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum StaticClass {
    /// An explicit class reference, e.g. `Foo::m()` / `Sub\Foo::m()` — exact
    /// (resolved to an FQN project-wide).
    Named(NameRef),
    /// `self::m()` — the lexical class, resolved under the final/private guard.
    SelfKw,
    /// `static::m()` — late static binding, always unknown (LSB).
    Static,
    /// `parent::m()` — the parent chain, exact.
    Parent,
}

/// A function-call (or method / static / constructor call) expression.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CallExpr {
    /// The simple callee name, if the callee is a statically-known **function**
    /// identifier; `None` for dynamic and method/static/constructor calls. Kept
    /// for the function-world call path; the full receiver is in [`Self::receiver`].
    pub callee: Option<String>,
    /// The full function reference (raw spelling + qualification) when the callee
    /// is a statically-known function, for project-wide resolution; `None`
    /// otherwise. Parallel to [`Self::callee`].
    pub callee_ref: Option<NameRef>,
    /// The receiver dimension (function / method / static / constructor). For a
    /// plain function call this is [`Callee::Function`] with the same name as
    /// [`Self::callee`].
    pub receiver: Callee,
    /// Arguments in source order. Only meaningful when `positional_only`.
    pub args: Vec<Arg>,
    /// `false` if the call used a named or spread (`...`) argument; the checker
    /// skips such calls (positional mapping is not reliable).
    pub positional_only: bool,
    pub span: Span,
}

/// A comparison operator in a lowered [`CondExpr`] (ADR-0031 stage 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CmpOp {
    /// `===` — strict identity.
    Identical,
    /// `!==` — strict non-identity.
    NotIdentical,
    /// `==` — loose equality (empirically-tabled coercion).
    Loose,
    /// `!=` / `<>` — loose inequality.
    NotLoose,
    /// `<` — less-than (ordering). Used for int-range guard refinement
    /// (ADR-0031 stage 2); at the verdict level it decides only for concrete
    /// numeric operands, else `Maybe`.
    Lt,
    /// `<=` — less-than-or-equal.
    Le,
    /// `>` — greater-than.
    Gt,
    /// `>=` — greater-than-or-equal.
    Ge,
}

/// A lowered operand of a [`CondExpr`] comparison (ADR-0031): a bare local
/// variable (whose fact the env may know), a concrete literal value, or anything
/// the lowering does not represent.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CondOperand {
    /// `$name` — a bare local variable (name without the `$`).
    Var(String),
    /// A literal value (`5`, `null`, `"x"`, `true`, …). Only literal [`ArgValue`]s
    /// appear here; a non-literal expression lowers the operand to [`Self::Other`].
    Literal(ArgValue),
    /// Anything else (a call, a property fetch, an arithmetic sub-expression, …).
    Other,
}

/// A small lowered condition language (ADR-0031 stage 1). The trace evaluator
/// walks it against the env to a unified `Certainty` (yes/no/maybe). Anything the
/// lowering does not recognize becomes [`CondExpr::Opaque`], carrying the
/// variables it reads so the walk can still forget them on the excluded path.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
    /// A condition the lowering cannot model. `reads` lists every bare variable it
    /// mentions, so a branch guarded by an opaque condition still invalidates
    /// those variables on the path that excludes it (the ADR-0027 read-set rule,
    /// preserved for opaque conditions).
    Opaque { reads: Vec<String> },
}

/// One arm of a structured [`StmtKind::Match`] (ADR-0031 Part B). `conditions`
/// are the arm's comparison operands (a match/switch arm may list several:
/// `1, 2 => …` / stacked `case 1: case 2:`); the arm is taken when the subject
/// equals **any** of them (`===` for match, loose `==` for switch). `trace` is
/// the arm body lowered by the same statement rules as every other sub-trace (a
/// match arm's single body expression becomes a one-statement trace; a switch
/// arm's statement list is lowered with its terminating `break` stripped — a
/// `break` models "end of arm / fall through to after the construct", never a
/// trace terminator, so it is simply removed during lowering).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MatchArmT {
    pub conditions: Vec<CondOperand>,
    pub trace: Vec<Stmt>,
}

/// One entry of a scope's linear trace IR (ADR-0001). A scope's body is lowered
/// to an ordered list of these; anything the lowering does not recognize exactly
/// becomes [`StmtKind::Barrier`] (over-lowering to `Barrier` is always sound —
/// it just makes prior known values unknown from that point).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum StmtKind {
    /// `$var = <value>;` — a plain (`=`) assignment to a bare local variable.
    /// `span` is the assignment's left-hand `$var` (for provenance line numbers).
    /// `call` carries the full [`CallExpr`] when the right-hand side *is* a
    /// statically-named call (`$x = f($s);`), so the propagation pass can check
    /// and descend into it — `ArgValue::Call` alone loses the argument spans.
    Assign { var: String, value: ArgValue, span: Span, call: Option<CallExpr> },
    /// `$var->prop = <rvalue>;` / `$this->prop = <rvalue>;` — a property assignment
    /// (ADR-0036 object state). `target_var` is the receiver variable name (`"this"`
    /// for `$this`); `prop` is the static property name. `value` is the lowered
    /// rvalue (a compound `+=`/`.=` lowers `value` to [`ArgValue::Other`]); `value_call`
    /// carries the full [`CallExpr`] when the rvalue *is* a statically-named call, so
    /// the propagation pass checks/descends it. A dynamic property name (`$o->$p = …`)
    /// or a chained/complex lvalue (`$a->b->c = …`, `Foo::$s = …`) stays a
    /// [`StmtKind::Barrier`], never a `PropAssign`.
    PropAssign {
        target_var: String,
        prop: String,
        value: ArgValue,
        value_call: Option<CallExpr>,
        span: Span,
    },
    /// A statement-level function call `f(args);`.
    Call(CallExpr),
    /// `return <value>;` (value is [`ArgValue::Other`] for `return;`). `call`
    /// carries the full [`CallExpr`] when the returned expression *is* a
    /// statically-named call (`return f($s);` — one of the most common shapes in
    /// real PHP), so the propagation pass and interprocedural descent reach it.
    /// `span` points at the returned value (or the `return` keyword when there is
    /// no value), so the return-type check can locate its diagnostic.
    Return { value: ArgValue, call: Option<CallExpr>, span: Span },
    /// `echo e1, e2, …;` — carries the statically-named calls among its operands
    /// so the propagation pass checks/descends them. Echo assigns nothing, so its
    /// env effect stays conservative (a `Barrier`-equivalent clear afterward).
    Echo(Vec<CallExpr>),
    /// A structured `if`/`elseif`/`else` (ADR-0031 stage 1): the trace models its
    /// control flow instead of erasing it. `then_trace` is the primary branch;
    /// `elseifs` are the `(condition, branch)` pairs in order; `else_trace` is the
    /// `else` branch when present. Each sub-trace is lowered by the same rules
    /// (nested ifs recurse; a construct that stays `Opaque` — a loop, `switch`,
    /// `try` — appears as an `Opaque` inside the relevant sub-trace). Only the
    /// *statement* form of `if` lowers here; every other control-flow construct
    /// remains [`StmtKind::Opaque`] (the ADR-0027 ratchet: one construct at a time).
    If {
        cond: CondExpr,
        then_trace: Vec<Stmt>,
        elseifs: Vec<(CondExpr, Vec<Stmt>)>,
        else_trace: Option<Vec<Stmt>>,
    },
    /// A structured statement-position `match` or `switch` (ADR-0031 Part B): the
    /// trace models its arm control flow instead of erasing it. `subject` is the
    /// scrutinee operand (`match ($subject)` / `switch ($subject)`); `arms` are
    /// the conditional arms in source order; `default` is the `default`/`default:`
    /// arm body when present. `loose` distinguishes the two comparison semantics:
    /// `false` for `match` (strict `===`, first-match, and a missing `default`
    /// throws `\UnhandledMatchError` on no match), `true` for `switch` (loose
    /// `==`, and a missing `default` simply falls through on no match).
    ///
    /// Only constructs the lowering can fully model reach here — the subject and
    /// every arm condition must lower to a bare variable or a literal, and (for
    /// `switch`) every non-empty case must end in `break`/`return`/`throw`/`exit`
    /// with no fall-through. Any construct that fails these stays [`StmtKind::Opaque`]
    /// (partial structuring of a `match`/`switch` would be unsound for the
    /// first-match and no-`default`-throws rules), so an unrepresentable arm makes
    /// the WHOLE construct opaque, never a mixed lowering.
    Match {
        subject: CondOperand,
        arms: Vec<MatchArmT>,
        default: Option<Vec<Stmt>>,
        loose: bool,
    },
    /// `throw <expr>;` — a trace terminator (the statement never falls through).
    /// `span` points at the `throw`. The thrown expression is not modeled; only
    /// the terminating control effect is.
    Throw { span: Span },
    /// `exit;` / `die;` (as an expression-statement) — a trace terminator. `span`
    /// points at the construct.
    Exit { span: Span },
    /// A recognized *control-flow* construct (`while`/`for`/`foreach`/
    /// `do-while`/`switch`/`match`-statement/`try`/nested block) whose internal
    /// data-flow the trace does not model, but whose **write set and read set** it
    /// does. This is the ADR-0027 ratchet applied to what used to be a blanket
    /// [`StmtKind::Barrier`]: instead of erasing *all* known values, the walk
    /// forgets only the variables the construct might touch **or branch on**.
    ///
    /// * `writes` — the over-approximated set of variable names the subtree may
    ///   assign (any assignment lvalue, compound assign, increment/decrement,
    ///   `foreach` value/key binding, `catch` parameter, `list()`
    ///   destructuring) *plus* every variable handed to any call inside it
    ///   (by-ref conservatism). Over-collection is always sound — it only
    ///   forgets more. Nested function/closure bodies are separate scopes and
    ///   their internal writes are **not** counted.
    /// * `reads` — every *other* variable the subtree merely *mentions*
    ///   (conditions included), i.e. every direct variable in the subtree not
    ///   already in `writes`. A construct that **reads** a variable may branch on
    ///   it and early-return, so the fall-through path can *exclude* the currently-
    ///   known value: continuing with the binding intact would assert an
    ///   unreachable path (a real soundness hole — a `?int` guard `if ($x == null)
    ///   { return; }` filters `null` out, yet the tail would otherwise still see
    ///   `$x = null`). Invalidating reads too closes it. Over-collection is sound;
    ///   nested function/closure bodies are not descended, same as `writes`.
    /// * `poisons` — `true` if the subtree contains any ADR-0001 poison marker
    ///   (reference/`global`/`static`/variable-variable/`extract`/`include`/
    ///   by-ref `use`, …). When set, the walk clears the whole env, exactly as a
    ///   `Barrier` would; the enclosing scope is independently poisoned too.
    ///
    /// Remaining theoretical gap (NOT closed here; ADR-0027 ratchet direction): a
    /// construct that early-returns on *every* branch makes all fall-through code
    /// dead, so even a fact about a variable the construct never reads could
    /// describe an unreachable path. Recovering that precision needs real
    /// branch/reachability analysis, deferred until the trace models control flow.
    Opaque { writes: Vec<String>, reads: Vec<String>, poisons: bool },
    /// Any construct the trace does not model *and* whose write set it cannot
    /// bound (`goto`, labels, `declare`, `__halt_compiler`, and anything the
    /// lowering is unsure of). Erases all known values — the sound floor.
    Barrier,
}

/// A trace entry plus the local variables it feeds into a call.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Stmt {
    pub kind: StmtKind,
    /// The source span of the whole statement (set centrally by `lower_stmt`
    /// from the CST statement node; nested constructs' inner statements carry
    /// their own spans). Used by the walk to record proven-dead regions.
    pub span: Span,
    /// Variables passed as an argument to *any* call within this statement. The
    /// checker marks them unknown *after* the statement — PHP by-reference
    /// parameters could mutate them, so a value can't be trusted past a call it
    /// was handed to (conservatively covering unseen `&$x` signatures).
    pub invalidated: Vec<String>,
}

/// Placeholder span for [`Stmt`]s under construction — overwritten with the
/// real statement span by `lower_stmt` before the statement enters a trace.
const ZERO_SPAN: Span = Span { start: 0, end: 0 };

/// Who owns an analysis [`Scope`] — the top-level script, a free function, or a
/// class method. Method scopes carry their declaring class so `$this->`, `self::`,
/// and `parent::` calls inside them resolve against the right chain (ADR-0001).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ScopeOwner {
    TopLevel,
    Function(String),
    Method { class: String, method: String },
    /// A closure / arrow-function body (ADR-0033), addressed by the definition-site
    /// byte offset (the closure/`fn` keyword span start). An [`ArgValue::Closure`]
    /// value naming this offset descends into this scope. Its params/effects/throws
    /// are carried on the [`Scope`] itself (a closure has no [`FunctionDecl`]).
    Closure { def_offset: u32 },
}

/// One analysis scope: the top-level script, a function body, or a method body.
/// Carries the linear trace and a whole-scope `poisoned` flag (ADR-0001 give-up
/// list).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Scope {
    /// `None` for the top-level script *and for method bodies*; `Some(name)` for
    /// a free function body. Retained for the function-world propagation paths
    /// (constant-function resolution, function binding descent), which key on a
    /// free-function name — a method never matches. Method scopes are addressed
    /// via [`Self::owner`].
    pub function_name: Option<String>,
    /// The precise owner of this scope (top-level / function / method).
    pub owner: ScopeOwner,
    /// `true` if the scope contains any construct that defeats local value
    /// tracking (`extract`/`compact`, `global`, `static $x`, variable-variables,
    /// reference assignment, by-ref closure capture, `include`/`require`/`eval`).
    /// When poisoned, no variable value is ever considered known in the scope.
    pub poisoned: bool,
    pub stmts: Vec<Stmt>,
    /// Parameters of a closure/arrow scope ([`ScopeOwner::Closure`]) — a closure
    /// has no [`FunctionDecl`] to look them up on, so binding descent and native
    /// parameter seeding read them here. Empty for function/method/top-level
    /// scopes (which resolve params via [`Self::owner`]).
    pub params: Vec<Param>,
    /// Effect-origin candidates of a closure/arrow body ([`ScopeOwner::Closure`]),
    /// so a closure can be an effect node in the fixpoint (ADR-0033 point 3).
    /// Empty for non-closure scopes (their origins live on the decl).
    pub effect_origins: Vec<EffectOrigin>,
    /// Throw-origin candidates of a closure/arrow body ([`ScopeOwner::Closure`]),
    /// the throw-fixpoint analogue of [`Self::effect_origins`].
    pub throw_origins: Vec<ThrowOrigin>,
}

/// A recovered parse error with its span (ADR-0003: error-tolerant).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ParseError {
    pub message: String,
    pub span: Span,
}

/// The lexical form of a source [`Comment`] — the three trivia comment shapes the
/// `@steins-ignore` channel reads (ADR-0023). Doc-block (`/** */`) comments are
/// exposed too so a directive placed in one is still seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

/// A comment trivium recovered from the parse (ADR-0023 inline-ignore channel).
/// `text` is the raw comment spelling including its delimiters (`// …`, `# …`,
/// `/* … */`); the suppression layer scans it for `@steins-ignore`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Comment {
    pub kind: CommentKind,
    pub span: Span,
    pub text: String,
}

/// An owned, Mago-free lowering of one parsed PHP file — the syntax-tree
/// contract for the slice.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceTree {
    strict_types: bool,
    functions: Vec<FunctionDecl>,
    classes: Vec<ClassDecl>,
    calls: Vec<CallExpr>,
    scopes: Vec<Scope>,
    parse_errors: Vec<ParseError>,
    /// The comment trivia in the file, in source order (ADR-0023 inline ignores).
    comments: Vec<Comment>,
    /// The namespace contexts of the file; index 0 is always the global context.
    contexts: Vec<NsCtx>,
    /// One `(start, end, ctx_index)` per namespace declaration in the file, so a
    /// byte offset can be mapped to its enclosing namespace context. Offsets not
    /// inside any namespace fall back to the global context (index 0).
    regions: Vec<(u32, u32, usize)>,
    /// Byte offset of the start of each line (index 0 == line 1).
    line_starts: Vec<u32>,
    text: String,
}

impl SourceTree {
    /// Parse PHP source into the lowered tree. Never panics: parse errors are
    /// recovered and reported via [`SourceTree::parse_errors`].
    #[must_use]
    pub fn parse(source: &str) -> Self {
        let arena = LocalArena::new();
        let file_id = FileId::new(b"<steins>");
        let program = mago_syntax::parser::parse_file_content(&arena, file_id, source.as_bytes());

        // File-level `use` imports that bind `Steins\Pure` / `Steins\Effect` to a
        // local name, so a bare `#[Pure]` / aliased `#[P]` / `#[Effect(...)]`
        // attribute can be recognized.
        let aliases = collect_steins_aliases(&Node::Program(program));

        // Namespace contexts (name + `use` imports) and the byte regions they
        // cover, so every declaration and reference resolves in the right scope.
        let (contexts, regions) = build_contexts(program);

        // Docblock index: every `/** … */` trivium, so a declaration can adopt the
        // one immediately preceding it (only whitespace between; ADR-0029).
        let docs = DocIndex::build(source, program);

        let mut lowered = Lowered::default();
        walk(&Node::Program(program), &aliases, &docs, &mut lowered);

        let mut classes = lower_classes(&Node::Program(program), &aliases, &docs);
        let scopes = lower_scopes(program, &contexts, &regions);

        // Comment trivia (ADR-0023 inline ignores): whitespace trivia is dropped;
        // every comment shape is kept with its raw spelling and span.
        let comments: Vec<Comment> = program.trivia.iter().filter_map(lower_comment).collect();

        // Fill the lowercase-normalized FQN on every declaration from the context
        // that encloses its name.
        for f in &mut lowered.functions {
            f.fqn = fqn_of(ctx_of(&contexts, &regions, f.span.start), &f.name);
        }
        for c in &mut classes {
            c.fqn = fqn_of(ctx_of(&contexts, &regions, c.span.start), &c.name);
        }

        let parse_errors = program
            .errors
            .iter()
            .map(|e| ParseError { message: e.to_string(), span: to_span(e.span()) })
            .collect();

        Self {
            strict_types: lowered.strict_types,
            functions: lowered.functions,
            classes,
            calls: lowered.calls,
            scopes,
            parse_errors,
            comments,
            contexts,
            regions,
            line_starts: line_starts(source),
            text: source.to_owned(),
        }
    }

    /// The namespace context enclosing `offset` (its namespace name and the
    /// `use` imports in scope), for whole-project name resolution.
    #[must_use]
    pub fn ctx_at(&self, offset: u32) -> &NsCtx {
        ctx_of(&self.contexts, &self.regions, offset)
    }

    /// Resolve a **class** reference to its FQN (case preserved, no leading `\`),
    /// applying PHP class-name resolution: fully-qualified names pass through;
    /// qualified/unqualified names apply `use` class imports on the first
    /// segment, else prepend the current namespace. Class references have **no**
    /// global fallback (unlike functions), so this is a pure syntactic function
    /// of the reference and its context — no project index needed. Callers fold
    /// case at lookup.
    #[must_use]
    pub fn resolve_class_fqn(&self, r: &NameRef) -> String {
        let ctx = self.ctx_at(r.offset);
        match r.kind {
            RefKind::FullyQualified => r.raw.clone(),
            RefKind::Qualified => {
                // First segment via class/namespace imports, else current ns.
                let first_len = r.raw.find('\\').unwrap_or(r.raw.len());
                let first = &r.raw[..first_len];
                if let Some(target) = ctx.class_imports.get(&first.to_ascii_lowercase()) {
                    format!("{target}{}", &r.raw[first_len..])
                } else if ctx.namespace.is_empty() {
                    r.raw.clone()
                } else {
                    format!("{}\\{}", ctx.namespace, r.raw)
                }
            }
            RefKind::Unqualified => {
                if let Some(target) = ctx.class_imports.get(&r.raw.to_ascii_lowercase()) {
                    target.clone()
                } else if ctx.namespace.is_empty() {
                    r.raw.clone()
                } else {
                    format!("{}\\{}", ctx.namespace, r.raw)
                }
            }
        }
    }

    /// Whether the file begins with `declare(strict_types=1)`.
    #[must_use]
    pub const fn has_strict_types(&self) -> bool {
        self.strict_types
    }

    /// The user-defined function declarations found in the file.
    #[must_use]
    pub fn functions(&self) -> &[FunctionDecl] {
        &self.functions
    }

    /// The user-defined class declarations found in the file (interfaces,
    /// traits, and enums are not lowered here).
    #[must_use]
    pub fn classes(&self) -> &[ClassDecl] {
        &self.classes
    }

    /// The function-call expressions found in the file.
    #[must_use]
    pub fn calls(&self) -> &[CallExpr] {
        &self.calls
    }

    /// The analysis scopes (top-level script + one per function body), each with
    /// its linear trace IR and poison flag (ADR-0001 value propagation).
    #[must_use]
    pub fn scopes(&self) -> &[Scope] {
        &self.scopes
    }

    /// The recovered parse errors.
    #[must_use]
    pub fn parse_errors(&self) -> &[ParseError] {
        &self.parse_errors
    }

    /// The comment trivia found in the file, in source order (ADR-0023 inline
    /// `@steins-ignore` channel). Whitespace trivia is not included.
    #[must_use]
    pub fn comments(&self) -> &[Comment] {
        &self.comments
    }

    /// Whether everything on `offset`'s line *before* `offset` is whitespace —
    /// i.e. the token at `offset` is the first non-whitespace on its line. Drives
    /// the `@steins-ignore` placement rule (ADR-0023): a comment that leads its
    /// line suppresses the *next* line; a trailing one suppresses *its own* line.
    #[must_use]
    pub fn is_line_leading(&self, offset: u32) -> bool {
        let line_idx = self.line_starts.partition_point(|&s| s <= offset).saturating_sub(1);
        let line_start = self.line_starts.get(line_idx).copied().unwrap_or(0) as usize;
        let end = (offset as usize).min(self.text.len());
        self.text.get(line_start..end).is_none_or(|s| s.trim().is_empty())
    }

    /// Resolve a byte offset to a 1-based line/column (column counted in
    /// Unicode scalar values).
    #[must_use]
    pub fn position(&self, offset: u32) -> Position {
        let line_idx = self.line_starts.partition_point(|&s| s <= offset).saturating_sub(1);
        let line_start = self.line_starts.get(line_idx).copied().unwrap_or(0) as usize;
        let end = (offset as usize).min(self.text.len());
        let column = self.text.get(line_start..end).map_or(0, |s| s.chars().count());
        Position { line: line_idx as u32 + 1, column: column as u32 + 1 }
    }
}

// ---------------------------------------------------------------------------
// Lowering (private): walk the Mago CST, emit owned data.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Lowered {
    strict_types: bool,
    functions: Vec<FunctionDecl>,
    calls: Vec<CallExpr>,
}

fn walk(node: &Node<'_, '_>, aliases: &SteinsAttrAliases, docs: &DocIndex, out: &mut Lowered) {
    match node {
        Node::Function(f) => out.functions.push(lower_function(f, aliases, docs)),
        Node::FunctionCall(c) => out.calls.push(lower_call(c)),
        Node::DeclareItem(d) if is_strict_types_one(d) => out.strict_types = true,
        _ => {}
    }
    for child in node.children() {
        walk(&child, aliases, docs, out);
    }
}

fn lower_function(f: &Function<'_>, aliases: &SteinsAttrAliases, docs: &DocIndex) -> FunctionDecl {
    let mut effect_origins = Vec::new();
    let mut throw_origins = Vec::new();
    let locals = collect_body_callables(f.body.statements.iter());
    for s in f.body.statements.iter() {
        scan_effect_origins(&Node::Statement(s), &locals, &mut effect_origins);
        scan_throw_origins(&Node::Statement(s), &[], &[], &locals, &mut throw_origins);
    }

    FunctionDecl {
        name: bytes_to_string(f.name.value),
        fqn: String::new(), // filled in `parse` from the enclosing namespace ctx
        params: lower_params(&f.parameter_list),
        ret: f.return_type_hint.as_ref().and_then(|r| lower_hint(&r.hint)),
        span: to_span(f.name.span()),
        effect_envelope: attrs_effect_envelope(&f.attribute_lists, aliases),
        effect_origins,
        throw_origins,
        docblock: docs.preceding(to_span(f.span()).start),
    }
}

/// Lower a parameter list to owned [`Param`]s (shared by functions and methods).
fn lower_params(list: &mago_syntax::cst::FunctionLikeParameterList<'_>) -> Vec<Param> {
    list.parameters
        .iter()
        .map(|p| Param {
            name: strip_dollar(bytes_to_string(p.variable.name)),
            ty: p.hint.as_ref().and_then(lower_hint),
            variadic: p.is_variadic(),
            by_ref: p.is_reference(),
            has_null_default: p
                .default_value
                .as_ref()
                .is_some_and(|d| matches!(d.value.unparenthesized(), Expression::Literal(Literal::Null(_)))),
            span: to_span(p.span()),
        })
        .collect()
}

/// Lower every `class` declaration reachable from `node` (interfaces, traits,
/// and enums are skipped — they carry no method bodies this slice checks).
fn lower_classes(node: &Node<'_, '_>, aliases: &SteinsAttrAliases, docs: &DocIndex) -> Vec<ClassDecl> {
    let mut out = Vec::new();
    lower_classes_into(node, aliases, docs, &mut out);
    out
}

fn lower_classes_into(
    node: &Node<'_, '_>,
    aliases: &SteinsAttrAliases,
    docs: &DocIndex,
    out: &mut Vec<ClassDecl>,
) {
    match node {
        Node::Class(c) => out.push(lower_class(c, aliases, docs)),
        Node::Interface(i) => out.push(lower_interface(i, aliases, docs)),
        _ => {}
    }
    for child in node.children() {
        lower_classes_into(&child, aliases, docs, out);
    }
}

fn lower_class(c: &Class<'_>, aliases: &SteinsAttrAliases, docs: &DocIndex) -> ClassDecl {
    let parent = c
        .extends
        .as_ref()
        .and_then(|e| e.types.iter().next())
        .map(name_ref);
    let implements: Vec<NameRef> = c
        .implements
        .as_ref()
        .map(|i| i.types.iter().map(name_ref).collect())
        .unwrap_or_default();

    let mut methods = Vec::new();
    let mut properties = Vec::new();
    let mut uses_traits = false;
    for member in c.members.iter() {
        match member {
            ClassLikeMember::Method(m) => {
                // A constructor's promoted params are properties too (ADR-0036).
                if bytes_to_string(m.name.value).eq_ignore_ascii_case("__construct") {
                    lower_promoted_params(m, &mut properties);
                }
                methods.push(lower_method(m, aliases, docs));
            }
            ClassLikeMember::Property(Property::Plain(p)) => {
                lower_plain_property(p, docs, &mut properties);
            }
            // Hooked properties (`public $x { get => … }`) are virtual/computed —
            // not lowered this slice (out of object-state scope; never heap-tracked,
            // so no property check fires on them — the safe side).
            ClassLikeMember::Property(Property::Hooked(_)) => {}
            ClassLikeMember::TraitUse(_) => uses_traits = true,
            _ => {}
        }
    }

    ClassDecl {
        name: bytes_to_string(c.name.value),
        fqn: String::new(), // filled in `parse` from the enclosing namespace ctx
        is_final: c.modifiers.iter().any(Modifier::is_final),
        is_interface: false,
        parent,
        implements,
        methods,
        properties,
        uses_traits,
        span: to_span(c.name.span()),
    }
}

/// The read-visibility a modifier sequence declares, defaulting to `Public`
/// (PHP semantics: absent visibility is public).
fn visibility_of(modifiers: &mago_syntax::cst::Sequence<'_, Modifier<'_>>) -> Visibility {
    if modifiers.iter().any(Modifier::is_private) {
        Visibility::Private
    } else if modifiers.iter().any(Modifier::is_protected) {
        Visibility::Protected
    } else {
        Visibility::Public
    }
}

/// Lower a plain property declaration (possibly multi-item `public int $a, $b;`)
/// into one [`PropertyDecl`] per declared variable (ADR-0036).
fn lower_plain_property(p: &PlainProperty<'_>, docs: &DocIndex, out: &mut Vec<PropertyDecl>) {
    let readonly = p.modifiers.iter().any(Modifier::is_readonly);
    let is_static = p.modifiers.iter().any(Modifier::is_static);
    let visibility = visibility_of(&p.modifiers);
    let ty = p.hint.as_ref().and_then(lower_hint);
    let docblock = docs.preceding(to_span(p.span()).start);
    let span = to_span(p.span());
    for item in p.items.iter() {
        let (name, has_default, default) = match item {
            PropertyItem::Abstract(a) => (strip_dollar(bytes_to_string(a.variable.name)), false, None),
            PropertyItem::Concrete(ci) => {
                let v = lower_arg_value(ci.value);
                let default = (!matches!(v, ArgValue::Other)).then_some(v);
                (strip_dollar(bytes_to_string(ci.variable.name)), true, default)
            }
        };
        out.push(PropertyDecl {
            name,
            ty: ty.clone(),
            readonly,
            is_static,
            visibility,
            has_default,
            default,
            promoted: false,
            docblock: docblock.clone(),
            span,
        });
    }
}

/// Lower a constructor's promoted parameters into [`PropertyDecl`]s (ADR-0036).
/// A parameter is promoted iff it carries a modifier (visibility / `readonly`).
fn lower_promoted_params(m: &Method<'_>, out: &mut Vec<PropertyDecl>) {
    for p in m.parameter_list.parameters.iter() {
        if !p.is_promoted_property() {
            continue;
        }
        let readonly = p.modifiers.iter().any(Modifier::is_readonly);
        let visibility = visibility_of(&p.modifiers);
        let ty = p.hint.as_ref().and_then(lower_hint);
        let has_default = p.default_value.is_some();
        let default = p
            .default_value
            .as_ref()
            .map(|d| lower_arg_value(d.value))
            .filter(|v| !matches!(v, ArgValue::Other));
        out.push(PropertyDecl {
            name: strip_dollar(bytes_to_string(p.variable.name)),
            ty,
            readonly,
            is_static: false,
            visibility,
            has_default,
            default,
            promoted: true,
            docblock: None,
            span: to_span(p.span()),
        });
    }
}

/// Lower an `interface` declaration to a [`ClassDecl`] with `is_interface = true`
/// (ADR-0033 Liskov): its methods are abstract signatures carrying effect
/// envelopes and `@throws` docblocks. An interface's `extends` list (interfaces
/// can extend several) becomes `parent` (the first) plus `implements` (the rest).
fn lower_interface(i: &mago_syntax::cst::Interface<'_>, aliases: &SteinsAttrAliases, docs: &DocIndex) -> ClassDecl {
    let mut extended: Vec<NameRef> =
        i.extends.as_ref().map(|e| e.types.iter().map(name_ref).collect()).unwrap_or_default();
    let parent = if extended.is_empty() { None } else { Some(extended.remove(0)) };

    let mut methods = Vec::new();
    for member in i.members.iter() {
        if let ClassLikeMember::Method(m) = member {
            methods.push(lower_method(m, aliases, docs));
        }
    }

    ClassDecl {
        name: bytes_to_string(i.name.value),
        fqn: String::new(),
        is_final: false,
        is_interface: true,
        parent,
        implements: extended,
        methods,
        properties: Vec::new(),
        uses_traits: false,
        span: to_span(i.name.span()),
    }
}

fn lower_method(m: &Method<'_>, aliases: &SteinsAttrAliases, docs: &DocIndex) -> MethodDecl {
    let mut effect_origins = Vec::new();
    let mut throw_origins = Vec::new();
    if let MethodBody::Concrete(block) = &m.body {
        let locals = collect_body_callables(block.statements.iter());
        for s in block.statements.iter() {
            scan_effect_origins(&Node::Statement(s), &locals, &mut effect_origins);
            scan_throw_origins(&Node::Statement(s), &[], &[], &locals, &mut throw_origins);
        }
    }

    let visibility = visibility_of(&m.modifiers);

    let name = bytes_to_string(m.name.value);
    let is_constructor = name.eq_ignore_ascii_case("__construct");

    MethodDecl {
        name,
        params: lower_params(&m.parameter_list),
        ret: m.return_type_hint.as_ref().and_then(|r| lower_hint(&r.hint)),
        span: to_span(m.name.span()),
        effect_envelope: attrs_effect_envelope(&m.attribute_lists, aliases),
        effect_origins,
        throw_origins,
        visibility,
        is_static: m.modifiers.iter().any(Modifier::is_static),
        is_final: m.modifiers.iter().any(Modifier::is_final),
        is_abstract: m.is_abstract(),
        is_constructor,
        docblock: docs.preceding(to_span(m.span()).start),
    }
}

/// An index of the file's `/** … */` docblock trivia, letting a declaration adopt
/// the docblock immediately preceding its head (ADR-0029). A docblock is
/// associated only when nothing but whitespace separates its end from the
/// declaration's span start (which begins at the attribute list / modifiers /
/// `function` keyword — so intervening attributes are already inside the gap-free
/// side). A wrong association would be a wrong contract (a false-positive vector),
/// so the whitespace-only rule is deliberately strict.
struct DocIndex<'a> {
    source: &'a str,
    /// `(end_offset, text)` of each docblock, in source order.
    blocks: Vec<(u32, String)>,
}

impl<'a> DocIndex<'a> {
    fn build(source: &'a str, program: &Program<'_>) -> Self {
        let blocks = program
            .trivia
            .iter()
            .filter(|t| matches!(t.kind, TriviaKind::DocBlockComment))
            .map(|t| (to_span(t.span).end, bytes_to_string(t.value)))
            .collect();
        Self { source, blocks }
    }

    /// The text of the docblock immediately preceding `decl_start`, if any.
    fn preceding(&self, decl_start: u32) -> Option<String> {
        let mut best: Option<(u32, &String)> = None;
        for (end, text) in &self.blocks {
            if *end <= decl_start && best.is_none_or(|(be, _)| *end > be) {
                best = Some((*end, text));
            }
        }
        let (end, text) = best?;
        let gap = self.source.get(end as usize..decl_start as usize)?;
        gap.chars().all(char::is_whitespace).then(|| text.clone())
    }
}

/// The canonical, case-folded identity of the `Steins\Pure` class — leading
/// namespace separators stripped, ASCII-lowercased (PHP class names are
/// case-insensitive).
const PURE_CLASS: &str = "steins\\pure";

/// The canonical, case-folded identity of the `Steins\Effect` class (ADR-0018).
const EFFECT_CLASS: &str = "steins\\effect";

/// The local names a file's `use` statements bind to `Steins\Pure` and
/// `Steins\Effect` (lowercased), so a bare `#[Pure]` / `#[Effect(...)]` or an
/// aliased `#[P]` attribute can be recognized (see [`collect_steins_aliases`]).
#[derive(Default)]
struct SteinsAttrAliases {
    pure: HashSet<String>,
    effect: HashSet<String>,
}

/// Normalize an attribute / use identifier to compare against [`PURE_CLASS`]:
/// drop a leading `\` (fully-qualified spelling) and lowercase.
fn normalize_class(name: &str) -> String {
    name.trim_start_matches('\\').to_ascii_lowercase()
}

/// Collect the local names (lowercased) that a file's `use` statements bind to
/// `Steins\Pure` and `Steins\Effect`, so a bare `#[Pure]` / `#[Effect(...)]` or
/// an aliased `#[P]` attribute can be resolved. `use Steins\Pure;` binds `pure`;
/// `use Steins\Effect as X;` binds `x` in the effect set.
///
/// Only the plain `use A\B;` / `use A\B as C;` sequence form is lowered (the
/// grouped `use A\{B};` form is not) — a miss here only *fails to recognize* an
/// envelope, which is the conservative side: it never imposes checks the author
/// did not ask for.
fn collect_steins_aliases(node: &Node<'_, '_>) -> SteinsAttrAliases {
    let mut aliases = SteinsAttrAliases::default();
    collect_steins_aliases_into(node, &mut aliases);
    aliases
}

fn collect_steins_aliases_into(node: &Node<'_, '_>, out: &mut SteinsAttrAliases) {
    if let Node::Use(u) = node
        && let UseItems::Sequence(seq) = &u.items
    {
        for item in seq.items.iter() {
            let full = normalize_class(&bytes_to_string(item.name.value()));
            let set = if full == PURE_CLASS {
                &mut out.pure
            } else if full == EFFECT_CLASS {
                &mut out.effect
            } else {
                continue;
            };
            // The bound local name: the explicit alias, else the last segment.
            let local = match &item.alias {
                Some(a) => bytes_to_string(a.identifier.value),
                None => bytes_to_string(item.name.last_segment()),
            };
            set.insert(local.to_ascii_lowercase());
        }
    }
    for child in node.children() {
        collect_steins_aliases_into(&child, out);
    }
}

/// Recognize a `#[\Steins\Pure]` or `#[\Steins\Effect(...)]` envelope attribute
/// in an attribute-list sequence (a function or method declaration), returning
/// the resolved [`EffectEnvelope`]. Recognition is deliberately conservative (a
/// false match imposes always-on checks the author never requested): a name
/// matches only when it is
///
/// * a fully-qualified `\Steins\Pure` / `\Steins\Effect` or qualified
///   `Steins\Pure` / `Steins\Effect`, or
/// * a bare / aliased name that a `use Steins\Pure[ as X];` /
///   `use Steins\Effect[ as X];` import binds.
///
/// So JetBrains' `#[Pure]` **without** the import, and `#[JetBrains\PhpStorm\Pure]`,
/// do not match. Matching is case-insensitive (PHP class-name semantics).
///
/// For `#[\Steins\Effect(...)]` the arguments must be **plain string literals**
/// (`'io'`, `'nondet.time'`); any non-literal argument (a class constant like
/// `Effects::IO`, a concatenation, or a named argument) — which this slice cannot
/// resolve without constant resolution — makes the whole attribute *unrecognized*
/// (no envelope, no checking), the conservative choice. Class-constant support is
/// deferred until constant resolution exists.
///
/// `#[\Steins\Pure]` and `#[\Steins\Effect(...)]` on the same declaration are
/// contradictory (Pure = empty upper bound, the tighter one); **Pure wins**
/// (empty `labels`), with no diagnostic about the contradiction here.
fn attrs_effect_envelope(
    attribute_lists: &mago_syntax::cst::Sequence<'_, mago_syntax::cst::AttributeList<'_>>,
    aliases: &SteinsAttrAliases,
) -> Option<EffectEnvelope> {
    let mut pure_span: Option<Span> = None;
    let mut effect: Option<(Vec<String>, Span)> = None;

    for list in attribute_lists.iter() {
        for attr in list.attributes.iter() {
            let norm = normalize_class(&bytes_to_string(attr.name.value()));
            let is_pure = match attr.name {
                Identifier::Local(_) => aliases.pure.contains(&norm),
                Identifier::Qualified(_) | Identifier::FullyQualified(_) => norm == PURE_CLASS,
            };
            let is_effect = match attr.name {
                Identifier::Local(_) => aliases.effect.contains(&norm),
                Identifier::Qualified(_) | Identifier::FullyQualified(_) => norm == EFFECT_CLASS,
            };

            if is_pure {
                pure_span.get_or_insert_with(|| to_span(attr.span()));
            } else if is_effect
                && effect.is_none()
                && let Some(labels) = effect_attr_labels(attr)
            {
                // Only recognized when *all* arguments are string literals; a
                // non-literal arg yields `None` and leaves the attribute ignored.
                effect = Some((labels, to_span(attr.span())));
            }
        }
    }

    // Pure wins the contradiction (empty upper bound is the tighter bound).
    if let Some(span) = pure_span {
        return Some(EffectEnvelope { labels: Vec::new(), span });
    }
    effect.map(|(labels, span)| EffectEnvelope { labels, span })
}

/// The effect labels declared by a recognized `#[\Steins\Effect(...)]` attribute,
/// or `None` when any argument is not a plain string literal (→ the whole
/// attribute is unrecognized). No argument list, or an empty one, yields an empty
/// label set (an empty upper bound — the same tight bound as `Pure`).
fn effect_attr_labels(attr: &Attribute<'_>) -> Option<Vec<String>> {
    let Some(list) = attr.argument_list.as_ref() else {
        return Some(Vec::new());
    };
    let mut labels = Vec::new();
    for arg in list.arguments.iter() {
        let PartialArgument::Positional(p) = arg else {
            return None; // named / placeholder / variadic-placeholder → unrecognized
        };
        if p.ellipsis.is_some() {
            return None; // spread argument → unrecognized
        }
        match p.value.unparenthesized() {
            // `?` widens an undecodable string literal (`ls.value == None`) to the
            // unrecognized path, exactly like a non-string argument.
            Expression::Literal(Literal::String(ls)) => labels.push(bytes_to_string(ls.value?)),
            _ => return None, // constant / concatenation / non-string literal → unrecognized
        }
    }
    Some(labels)
}

/// A resolvable [`CallbackRef`] for a callback argument expression (ADR-0033): an
/// inline closure/arrow (by its scope offset), a first-class callable of a named
/// function, or a string-literal function name. `None` for anything else (a
/// `$var`, an array `[$o, 'm']` callable, a non-literal — the honest opaque side).
fn callback_ref_of_arg(expr: &Expression<'_>) -> Option<CallbackRef> {
    match expr.unparenthesized() {
        Expression::Closure(cl) => Some(CallbackRef::Closure(closure_def_offset(cl))),
        Expression::ArrowFunction(af) => Some(CallbackRef::Closure(arrow_def_offset(af))),
        Expression::PartialApplication(PartialApplication::Function(fpa))
            if fpa.argument_list.is_first_class_callable() =>
        {
            match fpa.function {
                Expression::Identifier(id) => Some(CallbackRef::Named(name_ref(id))),
                _ => None,
            }
        }
        Expression::Literal(Literal::String(ls)) => {
            let raw = bytes_to_string(ls.value?);
            // A string callable naming a method (`Foo::m`) is deferred → not resolved.
            if raw.contains("::") || raw.is_empty() {
                return None;
            }
            Some(CallbackRef::Named(NameRef {
                raw: raw.trim_start_matches('\\').to_owned(),
                kind: if bytes_to_string(ls.value?).starts_with('\\') {
                    RefKind::FullyQualified
                } else {
                    RefKind::Unqualified
                },
                offset: to_span(expr.span()).start,
            }))
        }
        _ => None,
    }
}

/// A higher-order call decomposition: `(callee, callbacks by position, positional
/// arg count)`.
type HigherOrderCall = (NameRef, Vec<(usize, CallbackRef)>, usize);

/// The positional callback arguments of a named-function call, when at least one
/// argument is a resolvable [`CallbackRef`] (ADR-0033). `None` when the call is not
/// a named function, uses a named/spread argument, or carries no resolvable
/// callback.
fn higher_order_of_call(fc: &FunctionCall<'_>) -> Option<HigherOrderCall> {
    let Expression::Identifier(id) = fc.function else { return None };
    let mut callbacks: Vec<(usize, CallbackRef)> = Vec::new();
    let mut pos = 0usize;
    for arg in fc.argument_list.arguments.iter() {
        match arg {
            Argument::Positional(p) if p.ellipsis.is_none() => {
                if let Some(cb) = callback_ref_of_arg(p.value) {
                    callbacks.push((pos, cb));
                }
                pos += 1;
            }
            // A named or spread argument defeats positional callback mapping.
            _ => return None,
        }
    }
    if callbacks.is_empty() {
        return None;
    }
    Some((name_ref(id), callbacks, pos))
}

/// The bare callee variable name of a `$fn(...)` dynamic function call, if the
/// callee is a direct variable (`$fn`); `None` for other dynamic callees.
fn direct_var_callee(fc: &FunctionCall<'_>) -> Option<String> {
    match fc.function.unparenthesized() {
        Expression::Variable(Variable::Direct(dv)) => Some(strip_dollar(bytes_to_string(dv.name))),
        _ => None,
    }
}

/// A body-local single-assignment map `var → CallbackRef` (ADR-0033): a variable
/// assigned **exactly once** in the body, to a resolvable callback literal (inline
/// closure / first-class callable / string-literal function name), and written
/// nowhere else, resolves a later `$var()` call to that callback. A variable with
/// more than one write is excluded (its callback is ambiguous → the `$var()` call
/// stays an honest opaque taint). Sound with the *structural* envelope semantics:
/// a conditional single assignment still counts (an effect envelope is about the
/// code, not one path — like every other structural origin).
fn collect_body_callables<'a, 'arena>(
    statements: impl Iterator<Item = &'a Statement<'arena>>,
) -> HashMap<String, CallbackRef>
where
    'arena: 'a,
{
    let mut candidates: HashMap<String, CallbackRef> = HashMap::new();
    let mut writes: HashMap<String, usize> = HashMap::new();
    let mut passed: Vec<String> = Vec::new();
    for s in statements {
        let node = Node::Statement(s);
        collect_callable_assigns(&node, &mut candidates, &mut writes);
        // A variable handed to any call may be rebound by reference (by-ref
        // conservatism, matching the value-env's invalidation) — treat it as an
        // extra write so its callback resolution is dropped (sound).
        collect_call_vars(&node, &mut passed);
    }
    for v in passed {
        *writes.entry(v).or_insert(0) += 1;
    }
    candidates.into_iter().filter(|(v, _)| writes.get(v).copied() == Some(1)).collect()
}

/// Recursively count per-variable writes and record `$v = <callback>` candidates
/// over a CST subtree, NOT descending into nested closures/functions/classes
/// (their assignments are a separate scope). A write is any direct-variable
/// assignment lvalue, increment/decrement, or `foreach`/`catch` binding.
fn collect_callable_assigns(
    node: &Node<'_, '_>,
    candidates: &mut HashMap<String, CallbackRef>,
    writes: &mut HashMap<String, usize>,
) {
    match node {
        Node::Assignment(a) => {
            // Count every direct-variable write target in the lvalue.
            let mut targets = Vec::new();
            collect_direct_vars(&Node::Expression(a.lhs), &mut targets);
            for t in &targets {
                *writes.entry(t.clone()).or_insert(0) += 1;
            }
            // A plain `$v = <callback>` records a candidate for `$v`.
            if a.operator.is_assign()
                && let Expression::Variable(Variable::Direct(dv)) = a.lhs.unparenthesized()
                && let Some(cb) = callback_ref_of_arg(a.rhs)
            {
                candidates.insert(strip_dollar(bytes_to_string(dv.name)), cb);
            }
            // The rhs may itself contain writes (a nested assignment).
            collect_callable_assigns(&Node::Expression(a.rhs), candidates, writes);
            return;
        }
        Node::UnaryPrefix(u) => {
            if matches!(
                u.operator,
                UnaryPrefixOperator::PreIncrement(_) | UnaryPrefixOperator::PreDecrement(_)
            ) {
                let mut t = Vec::new();
                collect_direct_vars(&Node::Expression(u.operand), &mut t);
                for v in t {
                    *writes.entry(v).or_insert(0) += 1;
                }
            }
        }
        Node::UnaryPostfix(u) => {
            let mut t = Vec::new();
            collect_direct_vars(&Node::Expression(u.operand), &mut t);
            for v in t {
                *writes.entry(v).or_insert(0) += 1;
            }
        }
        Node::ForeachValueTarget(t) => {
            let mut vs = Vec::new();
            collect_direct_vars(&Node::Expression(t.value), &mut vs);
            for v in vs {
                *writes.entry(v).or_insert(0) += 1;
            }
        }
        Node::ForeachKeyValueTarget(t) => {
            let mut vs = Vec::new();
            collect_direct_vars(&Node::Expression(t.key), &mut vs);
            collect_direct_vars(&Node::Expression(t.value), &mut vs);
            for v in vs {
                *writes.entry(v).or_insert(0) += 1;
            }
        }
        Node::TryCatchClause(c) => {
            if let Some(v) = &c.variable {
                *writes.entry(strip_dollar(bytes_to_string(v.name))).or_insert(0) += 1;
            }
        }
        // Nested scopes are their own concern — do not descend.
        Node::Function(_)
        | Node::Closure(_)
        | Node::ArrowFunction(_)
        | Node::AnonymousClass(_)
        | Node::Class(_)
        | Node::Interface(_)
        | Node::Trait(_)
        | Node::Enum(_) => return,
        _ => {}
    }
    for child in node.children() {
        collect_callable_assigns(&child, candidates, writes);
    }
}

/// Walk a function-body subtree, appending every [`EffectOrigin`] found. Does not
/// descend into nested scopes (function/closure/arrow/class-like bodies), whose
/// effects are their own concern. `locals` resolves a `$fn()` variable call to a
/// body-local single-assignment closure (ADR-0033).
fn scan_effect_origins(
    node: &Node<'_, '_>,
    locals: &HashMap<String, CallbackRef>,
    out: &mut Vec<EffectOrigin>,
) {
    match node {
        // A statically-named call is either a builtin (catalog-classified) or a
        // same-file user function (a propagation edge) — the effects pass decides.
        Node::FunctionCall(fc) => {
            if let Expression::Identifier(id) = fc.function {
                // A named call passing a resolvable callback is a HigherOrder origin
                // (the invocation-shape redemption); otherwise a plain Call edge.
                match higher_order_of_call(fc) {
                    Some((callee, callbacks, arg_count)) => out.push(EffectOrigin::HigherOrder {
                        callee,
                        callbacks,
                        arg_count,
                        span: to_span(fc.span()),
                    }),
                    None => out.push(EffectOrigin::Call { name: name_ref(id), span: to_span(id.span()) }),
                }
            } else if let Some(cb) = direct_var_callee(fc).and_then(|v| locals.get(&v).cloned()) {
                // `$fn()` resolved to a body-local single-assignment closure.
                out.push(EffectOrigin::Callback { cbref: cb, span: to_span(fc.span()) });
            } else {
                // A dynamic function call (`$f()`, `($cb)()`) — unprovable.
                out.push(EffectOrigin::Opaque { span: to_span(fc.span()) });
            }
        }
        // Output-stream writes.
        Node::Echo(e) => out.push(EffectOrigin::Output { keyword: "echo", span: to_span(e.span()) }),
        Node::EchoTag(e) => {
            out.push(EffectOrigin::Output { keyword: "echo", span: to_span(e.span()) });
        }
        Node::PrintConstruct(p) => {
            out.push(EffectOrigin::Output { keyword: "print", span: to_span(p.span()) });
        }
        // Non-local program exit.
        Node::ExitConstruct(x) => {
            out.push(EffectOrigin::Exit { keyword: "exit", span: to_span(x.span()) });
        }
        Node::DieConstruct(d) => {
            out.push(EffectOrigin::Exit { keyword: "die", span: to_span(d.span()) });
        }
        // Instance / static method calls with a statically-resolvable receiver
        // become effect edges (`$this->`, `self::`, `parent::`, `Foo::`,
        // `new Foo()->`). Dynamic receivers record nothing.
        Node::MethodCall(mc) => {
            if let (Some(recv), Some(method)) =
                (effect_recv_of_object(mc.object), method_name_of(&mc.method))
            {
                out.push(EffectOrigin::MethodCall { receiver: recv, method, span: to_span(mc.span()) });
            } else {
                // `$var->m()` / `$o->$m()` — receiver or selector not resolvable.
                out.push(EffectOrigin::Opaque { span: to_span(mc.span()) });
            }
        }
        Node::NullSafeMethodCall(mc) => {
            if let (Some(recv), Some(method)) =
                (effect_recv_of_object(mc.object), method_name_of(&mc.method))
            {
                out.push(EffectOrigin::MethodCall { receiver: recv, method, span: to_span(mc.span()) });
            } else {
                out.push(EffectOrigin::Opaque { span: to_span(mc.span()) });
            }
        }
        Node::StaticMethodCall(sc) => {
            if let (Some(recv), Some(method)) =
                (effect_recv_of_class(sc.class), method_name_of(&sc.method))
            {
                out.push(EffectOrigin::MethodCall { receiver: recv, method, span: to_span(sc.span()) });
            } else {
                // `$var::m()` / `static::m()` / `Foo::$m()` — unresolvable.
                out.push(EffectOrigin::Opaque { span: to_span(sc.span()) });
            }
        }
        // Nested scopes — do not descend (closures deferred this slice).
        Node::Function(_)
        | Node::Closure(_)
        | Node::ArrowFunction(_)
        | Node::AnonymousClass(_)
        | Node::Class(_)
        | Node::Interface(_)
        | Node::Trait(_)
        | Node::Enum(_) => return,
        _ => {}
    }
    for child in node.children() {
        scan_effect_origins(&child, locals, out);
    }
}

/// The structural throw-origin walk (ADR-0040 damming). Produces every
/// throw-relevant construct in a body — explicit throws, and function/method
/// call edges — each tagged with the ordered enclosing `try`/`catch` guards that
/// may dam it. It is independent of the trace IR: try/catch nesting is handled by
/// threading a guard stack (`guards`, outer→inner) and a catch-variable scope
/// (`catch_scope`, for rethrow precision) through the descent.
///
/// * A `try` block is walked with this try's guard pushed; its `catch` and
///   `finally` blocks are walked WITHOUT it (a catch body is outside its own
///   clause but inside outer trys; `finally` absorbs nothing). Nested trys
///   compose naturally through the recursion.
/// * `throw new X` records the class; `throw $e` of an enclosing catch parameter
///   re-emits that catch's absorbed set (rethrow); any other throw taints.
fn scan_throw_origins(
    node: &Node<'_, '_>,
    guards: &[Vec<CatchClause>],
    catch_scope: &[(String, Vec<NameRef>, bool)],
    locals: &HashMap<String, CallbackRef>,
    out: &mut Vec<ThrowOrigin>,
) {
    // Innermost-first snapshot of the active guards for an origin at this point.
    let snapshot = || -> Vec<Vec<CatchClause>> {
        let mut g = guards.to_vec();
        g.reverse();
        g
    };

    match node {
        // A `try` composes the damming: its own guard wraps the try block only.
        Node::Try(t) => {
            let clauses: Vec<CatchClause> =
                t.catch_clauses.iter().map(lower_catch_clause).collect();
            // Try block: this try's guard is active (innermost).
            let mut inner_guards = guards.to_vec();
            inner_guards.push(clauses.clone());
            for s in t.block.statements.iter() {
                scan_throw_origins(&Node::Statement(s), &inner_guards, catch_scope, locals, out);
            }
            // Catch blocks: outer guards only; the clause's `$e` enters scope for
            // rethrow precision inside its own body.
            for c in t.catch_clauses.iter() {
                let clause = lower_catch_clause(c);
                let mut inner_scope = catch_scope.to_vec();
                if let Some(var) = &clause.var {
                    // Rethrow precision is only sound while `$e` still holds the
                    // caught exception. If the clause body writes the variable —
                    // by assignment or by handing it to any call (a by-ref
                    // signature could rebind it) — a later `throw $e` may throw
                    // something else entirely, so the variable must NOT enter
                    // the rethrow scope (its throws degrade to Taint).
                    // Review counterexample: `catch (RuntimeException $e) {
                    // $e = new JsonException(); throw $e; }` under
                    // `@throws JsonException` falsely reported RuntimeException.
                    let mut written = Vec::new();
                    for s in c.block.statements.iter() {
                        collect_assign_writes(&Node::Statement(s), &mut written);
                        collect_call_vars(&Node::Statement(s), &mut written);
                    }
                    if !written.contains(var) {
                        inner_scope.push((var.clone(), clause.classes.clone(), clause.has_unresolvable));
                    }
                }
                for s in c.block.statements.iter() {
                    scan_throw_origins(&Node::Statement(s), guards, &inner_scope, locals, out);
                }
            }
            // Finally: outer guards only; this try's catches never absorb it.
            if let Some(fin) = &t.finally_clause {
                for s in fin.block.statements.iter() {
                    scan_throw_origins(&Node::Statement(s), guards, catch_scope, locals, out);
                }
            }
            return; // children handled manually with the right guard/scope
        }
        // `throw <expr>` — classify the thrown expression.
        Node::Throw(t) => {
            let kind = match t.exception.unparenthesized() {
                Expression::Instantiation(inst) => match instantiation_class(inst) {
                    Some(class) => ThrowKind::New(class),
                    None => ThrowKind::Taint, // `throw new $c()` — dynamic class
                },
                Expression::Variable(Variable::Direct(dv)) => {
                    let name = strip_dollar(bytes_to_string(dv.name));
                    match catch_scope.iter().rev().find(|(v, _, _)| *v == name) {
                        Some((_, caught, unresolvable)) => ThrowKind::Rethrow {
                            caught: caught.clone(),
                            has_unresolvable: *unresolvable,
                        },
                        None => ThrowKind::Taint, // throwing a non-catch variable
                    }
                }
                _ => ThrowKind::Taint,
            };
            out.push(ThrowOrigin { kind, span: to_span(t.span()), guards: snapshot() });
            // Descend into the exception expression too (a call inside it — e.g.
            // `throw wrap(inner())` — is its own propagation edge).
        }
        // Statically-named function call → propagation edge. A named call passing
        // resolvable callbacks becomes a HigherOrder edge (ADR-0033); a `$fn()`
        // resolved to a body-local closure becomes a Callback edge.
        Node::FunctionCall(fc) => {
            if let Expression::Identifier(id) = fc.function {
                match higher_order_of_call(fc) {
                    Some((callee, callbacks, arg_count)) => out.push(ThrowOrigin {
                        kind: ThrowKind::HigherOrder { callee, callbacks, arg_count },
                        span: to_span(fc.span()),
                        guards: snapshot(),
                    }),
                    None => out.push(ThrowOrigin {
                        kind: ThrowKind::Call(name_ref(id)),
                        span: to_span(id.span()),
                        guards: snapshot(),
                    }),
                }
            } else if let Some(cb) = direct_var_callee(fc).and_then(|v| locals.get(&v).cloned()) {
                out.push(ThrowOrigin {
                    kind: ThrowKind::Callback { cbref: cb },
                    span: to_span(fc.span()),
                    guards: snapshot(),
                });
            } else {
                out.push(ThrowOrigin { kind: ThrowKind::Taint, span: to_span(fc.span()), guards: snapshot() });
            }
        }
        // Method / static calls with a resolvable receiver → edge; else taint.
        Node::MethodCall(mc) => {
            match (effect_recv_of_object(mc.object), method_name_of(&mc.method)) {
                (Some(recv), Some(method)) => out.push(ThrowOrigin {
                    kind: ThrowKind::MethodCall { receiver: recv, method },
                    span: to_span(mc.span()),
                    guards: snapshot(),
                }),
                _ => out.push(ThrowOrigin { kind: ThrowKind::Taint, span: to_span(mc.span()), guards: snapshot() }),
            }
        }
        Node::NullSafeMethodCall(mc) => {
            match (effect_recv_of_object(mc.object), method_name_of(&mc.method)) {
                (Some(recv), Some(method)) => out.push(ThrowOrigin {
                    kind: ThrowKind::MethodCall { receiver: recv, method },
                    span: to_span(mc.span()),
                    guards: snapshot(),
                }),
                _ => out.push(ThrowOrigin { kind: ThrowKind::Taint, span: to_span(mc.span()), guards: snapshot() }),
            }
        }
        Node::StaticMethodCall(sc) => {
            match (effect_recv_of_class(sc.class), method_name_of(&sc.method)) {
                (Some(recv), Some(method)) => out.push(ThrowOrigin {
                    kind: ThrowKind::MethodCall { receiver: recv, method },
                    span: to_span(sc.span()),
                    guards: snapshot(),
                }),
                _ => out.push(ThrowOrigin { kind: ThrowKind::Taint, span: to_span(sc.span()), guards: snapshot() }),
            }
        }
        // A `match` with no `default` arm can raise `\UnhandledMatchError` at
        // runtime when the subject matches no arm (ADR-0031 Part B). This is a
        // genuine *possible* throw of every default-less match — structural, like
        // every other throw origin — so it is recorded here (env-independent);
        // the trace walk separately proves when it is a *certain* terminator.
        // `UnhandledMatchError` is an `Error` (unchecked), so it never enters
        // `throw.undeclared`; it surfaces only in the annotate throws margin.
        Node::Match(m) => {
            if !m.arms.iter().any(mago_syntax::cst::MatchArm::is_default) {
                out.push(ThrowOrigin {
                    kind: ThrowKind::New(NameRef {
                        raw: "UnhandledMatchError".to_owned(),
                        kind: RefKind::FullyQualified,
                        offset: to_span(m.span()).start,
                    }),
                    span: to_span(m.span()),
                    guards: snapshot(),
                });
            }
            // Fall through to descend into the arms for their own throws.
        }
        // Nested scopes are their own concern — do not descend.
        Node::Function(_)
        | Node::Closure(_)
        | Node::ArrowFunction(_)
        | Node::AnonymousClass(_)
        | Node::Class(_)
        | Node::Interface(_)
        | Node::Trait(_)
        | Node::Enum(_) => return,
        _ => {}
    }
    for child in node.children() {
        scan_throw_origins(&child, guards, catch_scope, locals, out);
    }
}

/// Lower a `catch (A|B $e)` clause to its caught classes plus bound variable
/// (ADR-0040). A caught-type member that is not a plain class name marks the
/// clause `has_unresolvable` (→ absorption `Maybe`).
fn lower_catch_clause(c: &mago_syntax::cst::TryCatchClause<'_>) -> CatchClause {
    let mut classes = Vec::new();
    let mut has_unresolvable = false;
    lower_catch_hint(&c.hint, &mut classes, &mut has_unresolvable);
    let var = c.variable.as_ref().map(|v| strip_dollar(bytes_to_string(v.name)));
    CatchClause { classes, var, has_unresolvable }
}

/// Flatten a catch type hint (a plain class or a `|`-union of them) into class
/// [`NameRef`]s; any non-identifier member sets `unresolvable`.
fn lower_catch_hint(hint: &Hint<'_>, classes: &mut Vec<NameRef>, unresolvable: &mut bool) {
    match hint {
        Hint::Identifier(id) => classes.push(name_ref(id)),
        Hint::Union(u) => {
            lower_catch_hint(u.left, classes, unresolvable);
            lower_catch_hint(u.right, classes, unresolvable);
        }
        Hint::Parenthesized(p) => lower_catch_hint(p.hint, classes, unresolvable),
        _ => *unresolvable = true,
    }
}

/// Lower a type hint to a [`NativeType`] (single scalar, `?T`, or a union of the
/// four scalars + `false`/`true`/`null`), or `None` for anything the slice does
/// not model. A single non-scalar member anywhere (class type, `array`, `mixed`,
/// `iterable`, `callable`, `object`, an intersection, `self`/`static`/`parent`,
/// `void`/`never`) collapses the **whole** hint to `None` (silent; zero-FP).
fn lower_hint(hint: &Hint<'_>) -> Option<NativeType> {
    let mut members = Vec::new();
    let mut nullable = false;
    lower_hint_into(hint, &mut members, &mut nullable)?;
    // A hint with no non-null members (standalone `null`) is not modeled.
    if members.is_empty() {
        return None;
    }
    Some(NativeType { members, nullable })
}

/// Accumulate a hint's members into `members`, recording `null` in `nullable`.
/// Returns `None` (propagated up) the moment any part is a type Steins does not
/// model, collapsing the whole hint to silence.
fn lower_hint_into(
    hint: &Hint<'_>,
    members: &mut Vec<TypeMember>,
    nullable: &mut bool,
) -> Option<()> {
    match hint {
        Hint::Integer(_) => members.push(TypeMember::Scalar(ScalarType::Int)),
        Hint::Float(_) => members.push(TypeMember::Scalar(ScalarType::Float)),
        Hint::String(_) => members.push(TypeMember::Scalar(ScalarType::String)),
        Hint::Bool(_) => members.push(TypeMember::Scalar(ScalarType::Bool)),
        Hint::False(_) => members.push(TypeMember::BoolLiteral(false)),
        Hint::True(_) => members.push(TypeMember::BoolLiteral(true)),
        Hint::Null(_) => *nullable = true,
        Hint::Nullable(n) => {
            *nullable = true;
            lower_hint_into(n.hint, members, nullable)?;
        }
        Hint::Union(u) => {
            lower_hint_into(u.left, members, nullable)?;
            lower_hint_into(u.right, members, nullable)?;
        }
        Hint::Parenthesized(p) => lower_hint_into(p.hint, members, nullable)?,
        // Class `Identifier`, `array`, `mixed`, `iterable`, `callable`, `object`,
        // `Intersection`, `self`/`static`/`parent`, `void`/`never`, … → silence.
        _ => return None,
    }
    Some(())
}

fn lower_call(c: &FunctionCall<'_>) -> CallExpr {
    let (callee, callee_ref) = match c.function {
        Expression::Identifier(id) => (Some(bytes_to_string(id.last_segment())), Some(name_ref(id))),
        _ => (None, None),
    };
    // Receiver: a named function (`f(...)`), a variable call (`$fn(...)` — the
    // closure/callable dispatch of ADR-0033), or an unresolvable dynamic callee.
    let receiver = match (&callee, c.function.unparenthesized()) {
        (Some(name), _) => Callee::Function(name.clone()),
        (None, Expression::Variable(Variable::Direct(dv))) => {
            Callee::DynamicVar(strip_dollar(bytes_to_string(dv.name)))
        }
        (None, _) => Callee::Dynamic,
    };

    let (args, positional_only) = lower_argument_list(&c.argument_list);
    CallExpr { callee, callee_ref, receiver, args, positional_only, span: to_span(c.span()) }
}

/// Lower an argument list to `(args, positional_only)`, shared by every call
/// shape (function / method / static / constructor).
fn lower_argument_list(list: &mago_syntax::cst::ArgumentList<'_>) -> (Vec<Arg>, bool) {
    let mut positional_only = true;
    let mut args = Vec::new();
    for arg in list.arguments.iter() {
        match arg {
            Argument::Positional(p) if p.ellipsis.is_none() => {
                args.push(Arg { value: lower_arg_value(p.value), span: to_span(p.value.span()) });
            }
            _ => positional_only = false,
        }
    }
    (args, positional_only)
}

/// The simple method name of a member selector, if it is a plain identifier
/// (`->m`, `::m`). Dynamic selectors (`->$m`, `->{...}`) yield `None`.
fn method_name_of(selector: &ClassLikeMemberSelector<'_>) -> Option<String> {
    match selector {
        ClassLikeMemberSelector::Identifier(id) => Some(bytes_to_string(id.value)),
        _ => None,
    }
}

/// The class reference of an instantiation's class expression, if statically
/// named (`new Foo(...)`). Dynamic (`new $c()`) yields `None`.
fn instantiation_class(inst: &Instantiation<'_>) -> Option<NameRef> {
    match inst.class {
        Expression::Identifier(id) => Some(name_ref(id)),
        _ => None,
    }
}

/// The trace [`Receiver`] of a method-call object expression, or `None` when the
/// receiver is not one resolution can reason about.
fn trace_recv_of_object(object: &Expression<'_>) -> Option<Receiver> {
    match object.unparenthesized() {
        Expression::Variable(Variable::Direct(dv)) => {
            let name = strip_dollar(bytes_to_string(dv.name));
            Some(if name == "this" { Receiver::This } else { Receiver::Var(name) })
        }
        Expression::Instantiation(inst) => instantiation_class(inst).map(Receiver::New),
        _ => None,
    }
}

/// A simple property access `$var->prop` decomposed into `(var, prop)` (ADR-0036),
/// or `None` when the receiver is not a bare variable or the selector is not a
/// static identifier (dynamic name `$o->$p`, chain `$a->b->c`, `list()`-lvalue …).
fn prop_fetch_of(object: &Expression<'_>, selector: &ClassLikeMemberSelector<'_>) -> Option<(String, String)> {
    let var = match object.unparenthesized() {
        Expression::Variable(Variable::Direct(dv)) => strip_dollar(bytes_to_string(dv.name)),
        _ => return None,
    };
    let prop = method_name_of(selector)?;
    Some((var, prop))
}

/// The trace [`StaticClass`] of a static-call class expression.
fn trace_static_class(class: &Expression<'_>) -> Option<StaticClass> {
    match class {
        Expression::Identifier(id) => Some(StaticClass::Named(name_ref(id))),
        Expression::Self_(_) => Some(StaticClass::SelfKw),
        Expression::Static(_) => Some(StaticClass::Static),
        Expression::Parent(_) => Some(StaticClass::Parent),
        _ => None,
    }
}

/// The effect-graph receiver of a method-call object (no `$var` form — the
/// effects pass has no flow environment to resolve a variable's class).
fn effect_recv_of_object(object: &Expression<'_>) -> Option<EffectRecv> {
    match object.unparenthesized() {
        Expression::Variable(Variable::Direct(dv))
            if strip_dollar(bytes_to_string(dv.name)) == "this" =>
        {
            Some(EffectRecv::This)
        }
        Expression::Instantiation(inst) => instantiation_class(inst).map(EffectRecv::ClassName),
        _ => None,
    }
}

/// The effect-graph receiver of a static-call class expression (`static::` and
/// dynamic classes are unresolvable → `None`).
fn effect_recv_of_class(class: &Expression<'_>) -> Option<EffectRecv> {
    match class {
        Expression::Identifier(id) => Some(EffectRecv::ClassName(name_ref(id))),
        Expression::Self_(_) => Some(EffectRecv::SelfKw),
        Expression::Parent(_) => Some(EffectRecv::Parent),
        _ => None,
    }
}

/// Lower a method call (`MethodCall` / `NullSafeMethodCall`) into a [`CallExpr`].
/// `nullsafe` marks the `?->` form (see [`Callee::Method`]).
fn lower_method_call(object: &Expression<'_>, selector: &ClassLikeMemberSelector<'_>, list: &mago_syntax::cst::ArgumentList<'_>, span: Span, nullsafe: bool) -> CallExpr {
    let receiver = match (trace_recv_of_object(object), method_name_of(selector)) {
        (Some(recv), Some(method)) => Callee::Method { receiver: recv, method, nullsafe },
        _ => Callee::Dynamic,
    };
    let (args, positional_only) = lower_argument_list(list);
    CallExpr { callee: None, callee_ref: None, receiver, args, positional_only, span }
}

/// Lower a static method call into a [`CallExpr`].
fn lower_static_call(class: &Expression<'_>, selector: &ClassLikeMemberSelector<'_>, list: &mago_syntax::cst::ArgumentList<'_>, span: Span) -> CallExpr {
    let receiver = match (trace_static_class(class), method_name_of(selector)) {
        (Some(class), Some(method)) => Callee::Static { class, method },
        _ => Callee::Dynamic,
    };
    let (args, positional_only) = lower_argument_list(list);
    CallExpr { callee: None, callee_ref: None, receiver, args, positional_only, span }
}

/// Lower a `new Class(args...)` instantiation into a constructor [`CallExpr`],
/// or `None` when the class is not statically named.
fn lower_construct_call(inst: &Instantiation<'_>) -> Option<CallExpr> {
    let class = instantiation_class(inst)?;
    let (args, positional_only) = match &inst.argument_list {
        Some(list) => lower_argument_list(list),
        None => (Vec::new(), true),
    };
    Some(CallExpr {
        callee: None,
        callee_ref: None,
        receiver: Callee::Construct { class },
        args,
        positional_only,
        span: to_span(inst.span()),
    })
}

/// Lower an expression to an [`ArgValue`] — the shared lowering for both call
/// arguments and assignment right-hand sides. Recognizes literals, bare local
/// variables (`$x` → [`ArgValue::Var`]), and calls to a statically-named
/// function (`f(...)` → [`ArgValue::Call`]); everything else is
/// [`ArgValue::Other`].
fn lower_arg_value(expr: &Expression<'_>) -> ArgValue {
    match expr.unparenthesized() {
        Expression::Literal(lit) => lower_literal(lit),
        Expression::Variable(Variable::Direct(dv)) => {
            ArgValue::Var(strip_dollar(bytes_to_string(dv.name)))
        }
        // A property read `$var->prop` (ADR-0036): only a simple variable receiver
        // and a static property identifier are represented; a chain `$a->b->c`
        // (object is itself an access) or a dynamic name lowers to `Other`.
        Expression::Access(Access::Property(pa)) => match prop_fetch_of(pa.object, &pa.property) {
            Some((var, prop)) => ArgValue::PropFetch { var, prop },
            None => ArgValue::Other,
        },
        // `clone $var` (ADR-0036): a shallow object copy of a bare variable operand.
        Expression::Clone(c) => match c.object.unparenthesized() {
            Expression::Variable(Variable::Direct(dv)) => {
                ArgValue::Clone(strip_dollar(bytes_to_string(dv.name)))
            }
            _ => ArgValue::Other,
        },
        Expression::Call(Call::Function(fc)) => match fc.function {
            Expression::Identifier(id) => {
                let name = bytes_to_string(id.last_segment());
                let mut args = Vec::new();
                let mut ok = true;
                for arg in fc.argument_list.arguments.iter() {
                    match arg {
                        Argument::Positional(p) if p.ellipsis.is_none() => {
                            args.push(lower_arg_value(p.value));
                        }
                        // Named or spread argument: not modeled — the call is
                        // still recorded but with no resolvable arguments.
                        _ => ok = false,
                    }
                }
                if ok { ArgValue::Call(name, args) } else { ArgValue::Other }
            }
            _ => ArgValue::Other,
        },
        // `new Foo(...)` — a construction rvalue carrying its class (for exact-
        // class env tracking). Args are lowered best-effort; only the class name
        // is load-bearing.
        Expression::Instantiation(inst) => match instantiation_class(inst) {
            Some(class) => {
                let args = inst
                    .argument_list
                    .as_ref()
                    .map(|list| {
                        list.arguments
                            .iter()
                            .filter_map(|a| match a {
                                Argument::Positional(p) if p.ellipsis.is_none() => {
                                    Some(lower_arg_value(p.value))
                                }
                                _ => None,
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                ArgValue::New(class, args)
            }
            None => ArgValue::Other,
        },
        // Array literals `[...]` and legacy `array(...)`. Both share the same
        // element sequence shape; a spread, an unrepresentable element, or a
        // non-literal key collapses the whole array to `Other`.
        Expression::Array(a) => lower_array_elements(a.elements.iter()),
        Expression::LegacyArray(a) => lower_array_elements(a.elements.iter()),
        // Full ternary `$c ? A : B` (ADR-0031): a conditional value the walk can
        // evaluate. A short-ternary `?:` (`then` absent) widens to `Other` — it
        // needs the value on the true side, a definedness fact not carried yet.
        Expression::Conditional(cond) => match cond.then {
            Some(then_expr) => ArgValue::Ternary {
                cond: Box::new(lower_cond(cond.condition)),
                then_val: Box::new(lower_arg_value(then_expr)),
                else_val: Box::new(lower_arg_value(cond.r#else)),
            },
            None => ArgValue::Other,
        },
        // Closure expression `function (...) use (...) {...}` (ADR-0033): a closure
        // value naming its own scope (definition-site offset) and by-value captures.
        Expression::Closure(cl) => ArgValue::Closure(ClosureRef::Anonymous {
            def_offset: closure_def_offset(cl),
            captures: closure_use_captures(cl),
        }),
        // Arrow function `fn(...) => expr` (ADR-0033): auto-captures its free
        // variables by value.
        Expression::ArrowFunction(af) => ArgValue::Closure(ClosureRef::Anonymous {
            def_offset: arrow_def_offset(af),
            captures: arrow_free_vars(af),
        }),
        // First-class callable of a named free function `strtolower(...)`. Method
        // and static first-class callables are deferred → `Other` (documented).
        Expression::PartialApplication(PartialApplication::Function(fpa))
            if fpa.argument_list.is_first_class_callable() =>
        {
            match fpa.function {
                Expression::Identifier(id) => {
                    ArgValue::Closure(ClosureRef::FunctionName(name_ref(id)))
                }
                _ => ArgValue::Other,
            }
        }
        // Unary `-`/`+` on a numeric literal is itself a proven numeric literal
        // (so `-5` is `Int(-5)`, not `Other`). Any other operator/operand widens.
        Expression::UnaryPrefix(u) => match (&u.operator, lower_arg_value(u.operand)) {
            (UnaryPrefixOperator::Negation(_), ArgValue::Int(i)) => ArgValue::Int(i.wrapping_neg()),
            (UnaryPrefixOperator::Negation(_), ArgValue::Float(f)) => ArgValue::Float(-f),
            (UnaryPrefixOperator::Plus(_), v @ (ArgValue::Int(_) | ArgValue::Float(_))) => v,
            _ => ArgValue::Other,
        },
        _ => ArgValue::Other,
    }
}

/// Lower an array-literal element sequence to [`ArgValue::Array`], or
/// [`ArgValue::Other`] when any element defeats representation (a spread `...`, a
/// `list()`-style missing hole, a non-literal key, or an element whose value
/// lowers to `Other`). Nested arrays lower recursively and stay representable.
fn lower_array_elements<'a>(elements: impl Iterator<Item = &'a ArrayElement<'a>>) -> ArgValue {
    let mut items: Vec<(ArrayKey, ArgValue)> = Vec::new();
    for el in elements {
        match el {
            ArrayElement::Value(v) => {
                let value = lower_arg_value(v.value);
                if matches!(value, ArgValue::Other) {
                    return ArgValue::Other;
                }
                items.push((ArrayKey::Auto, value));
            }
            ArrayElement::KeyValue(kv) => {
                let Some(key) = lower_array_key(kv.key) else {
                    return ArgValue::Other;
                };
                let value = lower_arg_value(kv.value);
                if matches!(value, ArgValue::Other) {
                    return ArgValue::Other;
                }
                items.push((key, value));
            }
            // `...$spread`, or a `list()` destructuring hole — not representable.
            ArrayElement::Variadic(_) | ArrayElement::Missing(_) => return ArgValue::Other,
        }
    }
    ArgValue::Array(items)
}

/// Lower an array-literal key expression to a PHP-normalized [`ArrayKey`], or
/// `None` when the key is not a literal (a variable, call, nested array, …). PHP
/// key normalization: integer-like strings fold to `Int`, floats truncate toward
/// zero, `bool`→`int`, `null`→`""`.
fn lower_array_key(expr: &Expression<'_>) -> Option<ArrayKey> {
    match lower_arg_value(expr) {
        ArgValue::Int(i) => Some(ArrayKey::Int(i)),
        ArgValue::Bool(b) => Some(ArrayKey::Int(i64::from(b))),
        ArgValue::Null => Some(ArrayKey::Str(String::new())),
        ArgValue::Float(f) if f.is_finite() => Some(ArrayKey::Int(f.trunc() as i64)),
        ArgValue::Str(s) => Some(match php_canonical_int_string(&s) {
            Some(i) => ArrayKey::Int(i),
            None => ArrayKey::Str(s),
        }),
        // Non-literal key (variable/call/…) or a non-finite float → not provable.
        _ => None,
    }
}

/// Whether a string is a PHP *canonical* decimal integer (the form array keys
/// fold to `int` on): it round-trips exactly through `i64` (`"5"` → 5, but
/// `"05"`, `"+5"`, `" 5"`, `"-0"`, and out-of-range values stay strings).
fn php_canonical_int_string(s: &str) -> Option<i64> {
    let i: i64 = s.parse().ok()?;
    (i.to_string() == s).then_some(i)
}

fn lower_literal(lit: &Literal<'_>) -> ArgValue {
    match lit {
        Literal::Integer(li) => li.value.map_or(ArgValue::Other, |v| ArgValue::Int(v as i64)),
        Literal::Float(lf) => ArgValue::Float(lf.value.0),
        Literal::String(ls) => {
            ls.value.map_or(ArgValue::Other, |bytes| ArgValue::Str(bytes_to_string(bytes)))
        }
        Literal::True(_) => ArgValue::Bool(true),
        Literal::False(_) => ArgValue::Bool(false),
        Literal::Null(_) => ArgValue::Null,
    }
}

fn is_strict_types_one(item: &DeclareItem<'_>) -> bool {
    item.name.value == b"strict_types"
        && matches!(item.value, Expression::Literal(Literal::Integer(li)) if li.value == Some(1))
}

// ---------------------------------------------------------------------------
// Scope / linear-trace lowering (ADR-0001 value propagation).
// ---------------------------------------------------------------------------

/// Build every analysis scope: the top-level script first, then one per
/// function declaration and one per concrete method body found anywhere in the
/// file (nested functions and class methods alike get scopes).
fn lower_scopes(
    program: &Program<'_>,
    contexts: &[NsCtx],
    regions: &[(u32, u32, usize)],
) -> Vec<Scope> {
    // The script (top-level) scope spans all namespace bodies too: file-scoped
    // `namespace A;` nests the following statements inside the namespace node, so
    // flatten those back out so namespaced top-level code (e.g. `new User(...)`)
    // is analyzed. Function/class declarations still get their own scopes below.
    let mut top: Vec<&Statement<'_>> = Vec::new();
    for s in program.statements.iter() {
        flatten_top_level(s, &mut top);
    }
    let mut scopes = vec![build_scope_from(ScopeOwner::TopLevel, &top)];
    collect_scopes(&Node::Program(program), contexts, regions, &mut scopes);
    scopes
}

/// Collect script-level statements, descending through `namespace` bodies so
/// their top-level code joins the script scope in source order.
fn flatten_top_level<'a, 'arena>(
    s: &'a Statement<'arena>,
    out: &mut Vec<&'a Statement<'arena>>,
) {
    if let Statement::Namespace(ns) = s {
        for inner in ns.statements().iter() {
            flatten_top_level(inner, out);
        }
    } else {
        out.push(s);
    }
}

/// Recursively find `function` declarations (→ function scopes) and `class`
/// declarations (→ one scope per concrete method), building a scope for each.
/// A method scope's owner carries the class **FQN** (lowercase-normalized), so
/// cross-file resolution addresses it unambiguously.
fn collect_scopes(
    node: &Node<'_, '_>,
    contexts: &[NsCtx],
    regions: &[(u32, u32, usize)],
    out: &mut Vec<Scope>,
) {
    match node {
        Node::Function(f) => {
            let name = bytes_to_string(f.name.value);
            out.push(build_scope(ScopeOwner::Function(name), f.body.statements.as_slice()));
        }
        Node::Class(c) => {
            let simple = bytes_to_string(c.name.value);
            let ctx = ctx_of(contexts, regions, to_span(c.name.span()).start);
            // Case-preserved FQN: cross-file lookups fold case, but keeping the
            // written case makes the owner readable and stable for same-file code.
            let class_fqn = if ctx.namespace.is_empty() {
                simple.clone()
            } else {
                format!("{}\\{}", ctx.namespace, simple)
            };
            for member in c.members.iter() {
                if let ClassLikeMember::Method(m) = member
                    && let MethodBody::Concrete(block) = &m.body
                {
                    let method = bytes_to_string(m.name.value);
                    let owner = ScopeOwner::Method { class: class_fqn.clone(), method };
                    out.push(build_scope(owner, block.statements.as_slice()));
                }
            }
        }
        // Closures / arrow fns get their own scope (ADR-0033), addressed by the
        // definition-site byte offset. Params/effects/throws ride on the scope.
        Node::Closure(cl) => out.push(build_closure_scope_from_closure(cl)),
        Node::ArrowFunction(af) => out.push(build_closure_scope_from_arrow(af)),
        _ => {}
    }
    // Recurse so nested functions (inside methods or blocks) and nested classes
    // also get their scopes. Method scopes are only created above (matching
    // `Node::Class`), so this recursion never double-creates one.
    for child in node.children() {
        collect_scopes(&child, contexts, regions, out);
    }
}

/// Lower one scope's statements to a linear trace, and compute its poison flag.
fn build_scope(owner: ScopeOwner, statements: &[Statement<'_>]) -> Scope {
    let refs: Vec<&Statement<'_>> = statements.iter().collect();
    build_scope_from(owner, &refs)
}

/// Lower a scope from a borrowed statement list (shared by the flattened
/// top-level scope and the direct function/method paths).
fn build_scope_from(owner: ScopeOwner, statements: &[&Statement<'_>]) -> Scope {
    let poisoned = statements.iter().any(|s| node_poisons(&Node::Statement(s)));
    let mut stmts = Vec::new();
    for s in statements {
        lower_stmt(s, &mut stmts);
    }
    let function_name = match &owner {
        ScopeOwner::Function(name) => Some(name.clone()),
        ScopeOwner::TopLevel | ScopeOwner::Method { .. } | ScopeOwner::Closure { .. } => None,
    };
    Scope {
        function_name,
        owner,
        poisoned,
        stmts,
        params: Vec::new(),
        effect_origins: Vec::new(),
        throw_origins: Vec::new(),
    }
}

/// The definition-site byte offset that identifies a closure scope — the
/// `function` keyword's span start. An [`ArgValue::Closure`] value naming this
/// offset descends into the built scope.
fn closure_def_offset(cl: &mago_syntax::cst::Closure<'_>) -> u32 {
    to_span(cl.function.span()).start
}

/// The definition-site byte offset of an arrow function — the `fn` keyword.
fn arrow_def_offset(af: &mago_syntax::cst::ArrowFunction<'_>) -> u32 {
    to_span(af.r#fn.span()).start
}

/// The by-value captured names of a closure's `use (...)` clause (by-ref `&$x`
/// captures are excluded — they poison instead, ADR-0033/0001).
fn closure_use_captures(cl: &mago_syntax::cst::Closure<'_>) -> Vec<String> {
    cl.use_clause
        .as_ref()
        .map(|uc| {
            uc.variables
                .iter()
                .filter(|v| v.ampersand.is_none())
                .map(|v| strip_dollar(bytes_to_string(v.variable.name)))
                .collect()
        })
        .unwrap_or_default()
}

/// Whether a closure's `use (...)` clause captures anything by reference — this
/// poisons the enclosing scope AND the closure's own scope (ADR-0033).
fn closure_has_byref_use(cl: &mago_syntax::cst::Closure<'_>) -> bool {
    cl.use_clause
        .as_ref()
        .is_some_and(|uc| uc.variables.iter().any(|v| v.ampersand.is_some()))
}

/// Build the [`Scope`] for a `function (...) use (...) {...}` closure (ADR-0033).
fn build_closure_scope_from_closure(cl: &mago_syntax::cst::Closure<'_>) -> Scope {
    let mut stmts = Vec::new();
    let mut effect_origins = Vec::new();
    let mut throw_origins = Vec::new();
    let locals = collect_body_callables(cl.body.statements.iter());
    for s in cl.body.statements.iter() {
        lower_stmt(s, &mut stmts);
        scan_effect_origins(&Node::Statement(s), &locals, &mut effect_origins);
        scan_throw_origins(&Node::Statement(s), &[], &[], &locals, &mut throw_origins);
    }
    // The closure's own scope is poisoned by a by-ref `use (&$x)` capture (its
    // captured var is a reference alias) or any in-body poison marker.
    let poisoned = closure_has_byref_use(cl)
        || cl.body.statements.iter().any(|s| node_poisons(&Node::Statement(s)));
    Scope {
        function_name: None,
        owner: ScopeOwner::Closure { def_offset: closure_def_offset(cl) },
        poisoned,
        stmts,
        params: lower_params(&cl.parameter_list),
        effect_origins,
        throw_origins,
    }
}

/// Build the [`Scope`] for an arrow function `fn(...) => expr` (ADR-0033). The
/// single body expression lowers to one `return <expr>;` statement so a call
/// inside it (`fn($x) => width($x)`) is a reachable propagation/descent edge.
fn build_closure_scope_from_arrow(af: &mago_syntax::cst::ArrowFunction<'_>) -> Scope {
    let mut effect_origins = Vec::new();
    let mut throw_origins = Vec::new();
    // An arrow body is a single expression — no local assignments to resolve.
    let locals: HashMap<String, CallbackRef> = HashMap::new();
    scan_effect_origins(&Node::Expression(af.expression), &locals, &mut effect_origins);
    scan_throw_origins(&Node::Expression(af.expression), &[], &[], &locals, &mut throw_origins);
    // The arrow body is its return value: lower as a `return <expr>;` trace.
    let value = lower_arg_value(af.expression);
    let mut invalidated = Vec::new();
    collect_call_vars(&Node::Expression(af.expression), &mut invalidated);
    let call = named_call(af.expression);
    let span = to_span(af.expression.span());
    let ret = Stmt {
        span,
        kind: StmtKind::Return { value, call, span },
        invalidated,
    };
    let poisoned = node_poisons(&Node::Expression(af.expression));
    Scope {
        function_name: None,
        owner: ScopeOwner::Closure { def_offset: arrow_def_offset(af) },
        poisoned,
        stmts: vec![ret],
        params: lower_params(&af.parameter_list),
        effect_origins,
        throw_origins,
    }
}

/// The free (captured) variable names of an arrow-function body: every bare
/// variable it reads that is not one of its own parameters (arrow fns auto-capture
/// free variables by value). Over-collection is harmless — an extra name simply
/// snapshots a value the body ignores; a missing one would lose a capture.
fn arrow_free_vars(af: &mago_syntax::cst::ArrowFunction<'_>) -> Vec<String> {
    let params: std::collections::HashSet<String> = af
        .parameter_list
        .parameters
        .iter()
        .map(|p| strip_dollar(bytes_to_string(p.variable.name)))
        .collect();
    let mut vars = Vec::new();
    collect_var_reads(&Node::Expression(af.expression), &mut vars);
    let mut out: Vec<String> = Vec::new();
    for v in vars {
        if !params.contains(&v) && !out.contains(&v) {
            out.push(v);
        }
    }
    out
}

/// Collect every bare `$var` read in a subtree (name without `$`), NOT descending
/// into nested closures/arrows/functions/classes (their free-var capture is their
/// own concern). Used for arrow-fn auto-capture (ADR-0033).
fn collect_var_reads(node: &Node<'_, '_>, out: &mut Vec<String>) {
    match node {
        Node::DirectVariable(dv) => {
            let name = strip_dollar(bytes_to_string(dv.name));
            if name != "this" {
                out.push(name);
            }
        }
        Node::Closure(_)
        | Node::ArrowFunction(_)
        | Node::Function(_)
        | Node::AnonymousClass(_)
        | Node::Class(_)
        | Node::Interface(_)
        | Node::Trait(_)
        | Node::Enum(_) => return,
        _ => {}
    }
    for child in node.children() {
        collect_var_reads(&child, out);
    }
}

/// Append the lowered [`Stmt`] for one source statement (or nothing, for benign
/// statements that neither define values nor disturb them).
fn lower_stmt(s: &Statement<'_>, out: &mut Vec<Stmt>) {
    // A brace block creates no PHP scope: flatten it into the enclosing trace so a
    // branch body `{ return; … }` is lowered statement-by-statement (its `return`
    // is a real terminator, not hidden inside an `Opaque`). This is what makes the
    // structured-`if` branches see their terminators (ADR-0031).
    if let Statement::Block(b) = s {
        for inner in b.statements.iter() {
            lower_stmt(inner, out);
        }
        return;
    }
    let stmt_span = to_span(s.span());
    let stmt = match s {
        // Benign: no effect on local values — keep known values flowing across.
        Statement::OpeningTag(_)
        | Statement::ClosingTag(_)
        | Statement::Inline(_)
        | Statement::Noop(_)
        | Statement::Use(_) => return,
        Statement::Expression(es) => lower_expr_stmt(es.expression),
        Statement::Return(r) => {
            let value = r.value.map_or(ArgValue::Other, lower_arg_value);
            let mut invalidated = Vec::new();
            let mut call = None;
            // Point the diagnostic at the returned value, else the `return` word.
            let span = r.value.map_or_else(|| to_span(r.span()), |e| to_span(e.span()));
            if let Some(e) = r.value {
                collect_call_vars(&Node::Expression(e), &mut invalidated);
                // `return f($s);` — carry the call so propagation/descent reach it.
                call = named_call(e);
            }
            Stmt { span: ZERO_SPAN, kind: StmtKind::Return { value, call, span }, invalidated }
        }
        // `echo e1, e2, …;` — collect the statically-named calls among the
        // operands so propagation/descent check them; env stays conservative.
        Statement::Echo(e) => {
            let mut calls = Vec::new();
            let mut invalidated = Vec::new();
            for v in e.values.iter() {
                collect_call_vars(&Node::Expression(v), &mut invalidated);
                // An embedded assignment (`echo $x = 5;`) writes a variable, so
                // collect its write targets too: the walk no longer blanket-clears
                // on echo (ADR-0031), it invalidates only what echo can mutate.
                collect_assign_writes(&Node::Expression(v), &mut invalidated);
                if let Some(c) = named_call(v) {
                    calls.push(c);
                }
            }
            Stmt { span: ZERO_SPAN, kind: StmtKind::Echo(calls), invalidated }
        }
        // `if`/`elseif`/`else` is structured (ADR-0031 stage 1): its control flow
        // is modeled, not erased.
        Statement::If(if_stmt) => lower_if(if_stmt),
        // A `switch` is structured (ADR-0031 Part B) when its subject and every
        // case condition lower to a variable/literal AND every non-empty case
        // ends in break/return/throw/exit (no fall-through); else it stays
        // `Opaque` like the loop constructs below.
        Statement::Switch(sw) => lower_switch(sw).unwrap_or_else(|| lower_opaque(s)),
        // Every OTHER control-flow construct stays `Opaque` (ADR-0027 ratchet) —
        // the walk forgets only its write/read set, not the whole env.
        Statement::While(_)
        | Statement::For(_)
        | Statement::Foreach(_)
        | Statement::DoWhile(_)
        | Statement::Try(_) => lower_opaque(s),
        // Everything else (declarations, `goto`, labels, `declare`, unset,
        // `__halt_compiler`, …) stays a full Barrier: the sound floor for
        // anything whose write set the lowering cannot bound.
        _ => Stmt { span: ZERO_SPAN, kind: StmtKind::Barrier, invalidated: Vec::new() },
    };
    out.push(Stmt { span: stmt_span, ..stmt });
}

/// The full [`CallExpr`] when `expr` (unparenthesized) is a resolvable call —
/// a statically-named function, an instance/static method call, or a `new`
/// construction — else `None` (dynamic receivers carry nothing the checker can
/// resolve, so they are dropped rather than tracked).
fn named_call(expr: &Expression<'_>) -> Option<CallExpr> {
    match expr.unparenthesized() {
        Expression::Call(Call::Function(fc)) => {
            let call = lower_call(fc);
            // A named function (`f(...)`) or a variable call (`$fn(...)`) is
            // resolvable by the propagation walk; a fully dynamic callee is not.
            (call.receiver != Callee::Dynamic).then_some(call)
        }
        Expression::Call(Call::Method(mc)) => {
            let call = lower_method_call(mc.object, &mc.method, &mc.argument_list, to_span(mc.span()), false);
            (call.receiver != Callee::Dynamic).then_some(call)
        }
        Expression::Call(Call::NullSafeMethod(mc)) => {
            let call = lower_method_call(mc.object, &mc.method, &mc.argument_list, to_span(mc.span()), true);
            (call.receiver != Callee::Dynamic).then_some(call)
        }
        Expression::Call(Call::StaticMethod(sc)) => {
            let call = lower_static_call(sc.class, &sc.method, &sc.argument_list, to_span(sc.span()));
            (call.receiver != Callee::Dynamic).then_some(call)
        }
        Expression::Instantiation(inst) => lower_construct_call(inst),
        _ => None,
    }
}

/// Lower a structured `if`/`elseif`/`else` statement (ADR-0031 stage 1) to
/// [`StmtKind::If`]. Each branch body is lowered by the same statement rules as
/// the enclosing scope (so nested ifs recurse and unstructured constructs inside
/// a branch appear as `Opaque`/`Barrier` within the sub-trace). Both the brace
/// body and the colon-delimited (`if: … endif;`) form are handled via the CST's
/// body accessors.
fn lower_if(if_stmt: &mago_syntax::cst::If<'_>) -> Stmt {
    let body = &if_stmt.body;
    let cond = lower_cond(if_stmt.condition);
    let then_trace = lower_trace(body.statements());
    let elseifs = body
        .else_if_clauses()
        .into_iter()
        .map(|(c, stmts)| (lower_cond(c), lower_trace(stmts)))
        .collect();
    let else_trace = body.else_statements().map(lower_trace);
    Stmt {
        span: ZERO_SPAN,
        kind: StmtKind::If { cond, then_trace, elseifs, else_trace },
        invalidated: Vec::new(),
    }
}

/// Lower a borrowed statement list to a sub-trace (a branch body). Shares the
/// per-statement lowering with the top-level scope walk.
fn lower_trace(statements: &[Statement<'_>]) -> Vec<Stmt> {
    let mut out = Vec::new();
    for s in statements {
        lower_stmt(s, &mut out);
    }
    out
}

/// Lower a match-arm body expression (`… => <expr>`) to a one-statement sub-trace.
/// The body is an expression, so it reuses [`lower_expr_stmt`] (an arm body that
/// is `throw …` therefore lowers to a real [`StmtKind::Throw`] terminator).
fn lower_arm_body(expr: &Expression<'_>) -> Vec<Stmt> {
    let st = lower_expr_stmt(expr);
    vec![Stmt { span: to_span(expr.span()), ..st }]
}

/// Structure a statement-position `match ($subject) { … }` (ADR-0031 Part B).
/// Returns `None` — falling back to `Opaque` — when the subject or any arm
/// condition does not lower to a variable/literal, or when more than one
/// `default` arm is present (partial structuring is unsound for the first-match
/// and no-`default`-throws rules, so it is all-or-nothing).
fn lower_match_stmt(m: &mago_syntax::cst::Match<'_>) -> Option<Stmt> {
    let subject = usable_operand(m.expression)?;
    let mut arms = Vec::new();
    let mut default: Option<Vec<Stmt>> = None;
    for arm in m.arms.iter() {
        match arm {
            mago_syntax::cst::MatchArm::Expression(a) => {
                let mut conditions = Vec::new();
                for c in a.conditions.iter() {
                    conditions.push(usable_operand(c)?);
                }
                arms.push(MatchArmT { conditions, trace: lower_arm_body(a.expression) });
            }
            mago_syntax::cst::MatchArm::Default(a) => {
                if default.is_some() {
                    return None; // two defaults — give up (unreachable in valid PHP)
                }
                default = Some(lower_arm_body(a.expression));
            }
        }
    }
    Some(Stmt {
        span: ZERO_SPAN,
        kind: StmtKind::Match { subject, arms, default, loose: false },
        invalidated: Vec::new(),
    })
}

/// Structure a `switch ($subject) { … }` (ADR-0031 Part B) into the same
/// [`StmtKind::Match`] node with `loose: true`. Returns `None` — falling back to
/// `Opaque` — unless the subject and every case condition lower to a
/// variable/literal AND every non-empty case ends in `break`/`return`/`throw`/
/// `exit` with no fall-through. Empty case labels stack onto the following
/// non-empty case as extra conditions (`case 1: case 2: body`), matching PHP
/// fall-through-to-the-body semantics; a trailing `break` is stripped (it means
/// end-of-arm, not a trace terminator). A stray `break`/`continue`/`goto` inside
/// a case body (targeting the switch from within a nested `if`, say) makes the
/// whole construct opaque — modeling it as an arm would be unsound.
fn lower_switch(sw: &mago_syntax::cst::Switch<'_>) -> Option<Stmt> {
    let subject = usable_operand(sw.expression)?;
    let mut arms: Vec<MatchArmT> = Vec::new();
    let mut default: Option<Vec<Stmt>> = None;
    // Conditions of consecutive empty case labels, waiting to stack onto the next
    // non-empty case body; `pending_default` records an empty `default:` label.
    let mut pending: Vec<CondOperand> = Vec::new();
    let mut pending_default = false;

    for case in sw.body.cases() {
        // The case's own comparison operand (None for `default`), rejected early
        // if it does not lower to a variable/literal.
        let cond = match case.expression() {
            Some(e) => Some(usable_operand(e)?),
            None => None,
        };
        if case.is_empty() {
            // An empty label falls through to the next case body: remember it.
            match cond {
                Some(c) => pending.push(c),
                None => {
                    if default.is_some() {
                        return None;
                    }
                    pending_default = true;
                }
            }
            continue;
        }
        // A non-empty case must end cleanly: strip a trailing plain `break;`, else
        // require a terminator; a stray jump anywhere in the body is unsound.
        let raw = case.statements();
        let (body, ends_break) = strip_trailing_break(raw)?;
        if case_has_stray_jump(body) {
            return None;
        }
        let trace = lower_trace(body);
        if !ends_break {
            // No break: the body must terminate, or it would fall through to the
            // next case (which structuring cannot model).
            let terminates = matches!(
                trace.last().map(|s| &s.kind),
                Some(StmtKind::Return { .. } | StmtKind::Throw { .. } | StmtKind::Exit { .. })
            );
            if !terminates {
                return None;
            }
        }
        // Build this arm, stacking any pending empty-label conditions in front.
        match cond {
            Some(c) if !pending_default => {
                let mut conditions = std::mem::take(&mut pending);
                conditions.push(c);
                arms.push(MatchArmT { conditions, trace });
            }
            // This body is (or is reached by fall-through from) `default:`; a
            // default subsumes any stacked case conditions (it catches all).
            _ => {
                if default.is_some() {
                    return None;
                }
                default = Some(trace);
            }
        }
        pending.clear();
        pending_default = false;
    }
    // Trailing empty labels with no following body do nothing at runtime, but
    // structuring them as no-op arms is fiddly; bail to Opaque (sound).
    if !pending.is_empty() || pending_default {
        return None;
    }
    Some(Stmt {
        span: ZERO_SPAN,
        kind: StmtKind::Match { subject, arms, default, loose: true },
        invalidated: Vec::new(),
    })
}

/// Lower an operand to a *usable* [`CondOperand`] — a bare variable or a literal —
/// or `None` for anything else (a call, property fetch, arithmetic). Used to gate
/// whether a `match`/`switch` can be structured at all.
fn usable_operand(expr: &Expression<'_>) -> Option<CondOperand> {
    match lower_cond_operand(expr) {
        CondOperand::Other => None,
        operand => Some(operand),
    }
}

/// Split a case body into (body-without-terminating-break, ended-in-break). A
/// trailing `break;` / `break 1;` is stripped; a `break N` (N > 1) or a
/// non-literal level targets an outer construct — unrepresentable, so `None`.
fn strip_trailing_break<'a, 'arena>(
    raw: &'a [Statement<'arena>],
) -> Option<(&'a [Statement<'arena>], bool)> {
    match raw.last() {
        Some(Statement::Break(b)) => {
            if break_is_plain(b) { Some((&raw[..raw.len() - 1], true)) } else { None }
        }
        _ => Some((raw, false)),
    }
}

/// Whether a `break` targets its immediately-enclosing construct (`break;` or
/// `break 1;`) as opposed to an outer one (`break 2;`, `break $n;`).
fn break_is_plain(b: &mago_syntax::cst::Break<'_>) -> bool {
    match b.level {
        None => true,
        Some(e) => matches!(lower_arg_value(e), ArgValue::Int(1)),
    }
}

/// Whether a switch-case body contains a `break`/`continue`/`goto` that would
/// target the switch from inside the case (making arm modeling unsound). Nested
/// loops and switches consume their own `break`/`continue`, so the scan does not
/// descend into them; nested function-likes are separate scopes. Any `goto` at
/// all disqualifies (its target is unbounded).
fn case_has_stray_jump(body: &[Statement<'_>]) -> bool {
    body.iter().any(|s| stmt_has_stray_jump(s))
}

fn stmt_has_stray_jump(s: &Statement<'_>) -> bool {
    match s {
        Statement::Break(_) | Statement::Continue(_) | Statement::Goto(_) => true,
        // Nested loops/switch absorb their own break/continue — do not descend.
        Statement::While(_)
        | Statement::For(_)
        | Statement::Foreach(_)
        | Statement::DoWhile(_)
        | Statement::Switch(_) => false,
        _ => node_has_stray_jump(&Node::Statement(s)),
    }
}

/// Recurse through a node's children looking for a stray jump, stopping at nested
/// loops/switches (which consume their own) and nested function-like scopes.
fn node_has_stray_jump(node: &Node<'_, '_>) -> bool {
    node.children().iter().any(|child| match child {
        Node::Break(_) | Node::Continue(_) | Node::Goto(_) => true,
        Node::While(_)
        | Node::For(_)
        | Node::Foreach(_)
        | Node::DoWhile(_)
        | Node::Switch(_)
        | Node::Function(_)
        | Node::Closure(_)
        | Node::ArrowFunction(_)
        | Node::AnonymousClass(_)
        | Node::Class(_)
        | Node::Interface(_)
        | Node::Trait(_)
        | Node::Enum(_) => false,
        other => node_has_stray_jump(other),
    })
}

/// Lower a condition expression to a [`CondExpr`] (ADR-0031 stage 1). Recognized:
/// `===`/`!==`/`==`/`!=` comparisons, `instanceof`, `!`/`&&`/`||` (incl. the
/// low-precedence `and`/`or`), and bare truthiness. Everything else becomes
/// [`CondExpr::Opaque`] carrying the variables it reads.
fn lower_cond(expr: &Expression<'_>) -> CondExpr {
    match expr.unparenthesized() {
        Expression::Binary(b) => lower_binary_cond(b),
        Expression::UnaryPrefix(u) if matches!(u.operator, UnaryPrefixOperator::Not(_)) => {
            CondExpr::Not(Box::new(lower_cond(u.operand)))
        }
        other => match lower_cond_operand(other) {
            CondOperand::Other => CondExpr::Opaque { reads: cond_reads(other) },
            operand => CondExpr::Truthy(operand),
        },
    }
}

/// Lower a binary-operator condition (comparison / `instanceof` / `&&` / `||`).
fn lower_binary_cond(b: &Binary<'_>) -> CondExpr {
    let op = match b.operator {
        BinaryOperator::Identical(_) => Some(CmpOp::Identical),
        BinaryOperator::NotIdentical(_) => Some(CmpOp::NotIdentical),
        BinaryOperator::Equal(_) => Some(CmpOp::Loose),
        BinaryOperator::NotEqual(_) | BinaryOperator::AngledNotEqual(_) => Some(CmpOp::NotLoose),
        BinaryOperator::LessThan(_) => Some(CmpOp::Lt),
        BinaryOperator::LessThanOrEqual(_) => Some(CmpOp::Le),
        BinaryOperator::GreaterThan(_) => Some(CmpOp::Gt),
        BinaryOperator::GreaterThanOrEqual(_) => Some(CmpOp::Ge),
        _ => None,
    };
    if let Some(op) = op {
        let lhs = lower_cond_operand(b.lhs);
        let rhs = lower_cond_operand(b.rhs);
        // Ordering comparisons (`<`/`<=`/`>`/`>=`) are only useful for guard
        // refinement when one side is a bare variable and the other a literal;
        // an unrepresentable operand would otherwise silently drop the reads it
        // may mutate by reference, so fall back to `Opaque` (collecting reads).
        let ordering = matches!(op, CmpOp::Lt | CmpOp::Le | CmpOp::Gt | CmpOp::Ge);
        if ordering
            && (matches!(lhs, CondOperand::Other) || matches!(rhs, CondOperand::Other))
        {
            let mut reads = Vec::new();
            collect_read_vars(&Node::Expression(b.lhs), &[], &mut reads);
            collect_read_vars(&Node::Expression(b.rhs), &[], &mut reads);
            return CondExpr::Opaque { reads };
        }
        return CondExpr::Cmp { op, lhs, rhs };
    }
    match b.operator {
        BinaryOperator::Instanceof(_) => {
            // `operand instanceof Class` — the class is the rhs when a plain name.
            if let Expression::Identifier(id) = b.rhs.unparenthesized() {
                CondExpr::Instanceof { operand: lower_cond_operand(b.lhs), class_ref: name_ref(id) }
            } else {
                CondExpr::Opaque { reads: cond_reads(b.lhs) }
            }
        }
        BinaryOperator::And(_) | BinaryOperator::LowAnd(_) => {
            CondExpr::And(Box::new(lower_cond(b.lhs)), Box::new(lower_cond(b.rhs)))
        }
        BinaryOperator::Or(_) | BinaryOperator::LowOr(_) => {
            CondExpr::Or(Box::new(lower_cond(b.lhs)), Box::new(lower_cond(b.rhs)))
        }
        // Any other binary operator (arithmetic, `<`, `.`, …): opaque, reading its
        // whole subtree.
        _ => {
            let mut reads = Vec::new();
            collect_read_vars(&Node::Expression(b.lhs), &[], &mut reads);
            collect_read_vars(&Node::Expression(b.rhs), &[], &mut reads);
            CondExpr::Opaque { reads }
        }
    }
}

/// Lower a comparison operand: a bare `$var`, a literal, or [`CondOperand::Other`].
fn lower_cond_operand(expr: &Expression<'_>) -> CondOperand {
    match expr.unparenthesized() {
        Expression::Variable(Variable::Direct(dv)) => {
            CondOperand::Var(strip_dollar(bytes_to_string(dv.name)))
        }
        other => match lower_arg_value(other) {
            v if v.is_literal() => CondOperand::Literal(v),
            _ => CondOperand::Other,
        },
    }
}

/// The bare variables a condition subtree reads (for the opaque-condition read-set
/// rule: a branch guarded by an opaque condition still forgets these on the path
/// that excludes it).
fn cond_reads(expr: &Expression<'_>) -> Vec<String> {
    let mut reads = Vec::new();
    collect_read_vars(&Node::Expression(expr), &[], &mut reads);
    reads
}

/// Lower a recognized control-flow construct to [`StmtKind::Opaque`]: compute
/// its poison flag and its over-approximated write set (see the variant docs).
fn lower_opaque(s: &Statement<'_>) -> Stmt {
    let node = Node::Statement(s);
    let (writes, reads, poisons) = opaque_sets(&node);
    Stmt { span: ZERO_SPAN, kind: StmtKind::Opaque { writes, reads, poisons }, invalidated: Vec::new() }
}

/// Compute an `Opaque` construct's `(writes, reads, poisons)` over its subtree.
/// `reads` is every direct variable mentioned that is not already a write —
/// including branch conditions — so a construct that branches on a variable and
/// early-returns invalidates the fall-through binding (soundness; see the
/// [`StmtKind::Opaque`] docs). Nested function-like bodies are not descended.
fn opaque_sets(node: &Node<'_, '_>) -> (Vec<String>, Vec<String>, bool) {
    let poisons = node_poisons(node);
    let mut writes = Vec::new();
    // By-ref conservatism: every variable handed to any call in the subtree.
    collect_call_vars(node, &mut writes);
    // Assignment / increment / foreach-binding / catch-param write targets.
    collect_assign_writes(node, &mut writes);
    // Everything else the subtree merely reads / branches on.
    let mut reads = Vec::new();
    collect_read_vars(node, &writes, &mut reads);
    (writes, reads, poisons)
}

/// Lower an expression-statement to a trace entry.
fn lower_expr_stmt(expr: &Expression<'_>) -> Stmt {
    match expr.unparenthesized() {
        Expression::Assignment(a) => {
            if let Expression::Variable(Variable::Direct(dv)) = a.lhs.unparenthesized() {
                let var = strip_dollar(bytes_to_string(dv.name));
                // Only a plain `=` yields a value; compound ops (`+=`, `.=`, …)
                // make the variable unknown.
                let value = if a.operator.is_assign() { lower_arg_value(a.rhs) } else { ArgValue::Other };
                let mut invalidated = Vec::new();
                collect_call_vars(&Node::Expression(a.rhs), &mut invalidated);
                // `$x = f($s);` — carry the RHS call for propagation/descent.
                let call = if a.operator.is_assign() { named_call(a.rhs) } else { None };
                Stmt {
                    span: ZERO_SPAN,
                    kind: StmtKind::Assign { var, value, span: to_span(a.lhs.span()), call },
                    invalidated,
                }
            } else if let Expression::Access(Access::Property(pa)) = a.lhs.unparenthesized()
                && let Some((target_var, prop)) = prop_fetch_of(pa.object, &pa.property)
            {
                // `$var->prop = <rvalue>` / `$this->prop = <rvalue>` (ADR-0036). A
                // compound op (`+=`, `.=`, …) makes the property value unknown.
                let value = if a.operator.is_assign() { lower_arg_value(a.rhs) } else { ArgValue::Other };
                let value_call = if a.operator.is_assign() { named_call(a.rhs) } else { None };
                let mut invalidated = Vec::new();
                collect_call_vars(&Node::Expression(a.rhs), &mut invalidated);
                Stmt {
                    span: ZERO_SPAN,
                    kind: StmtKind::PropAssign { target_var, prop, value, value_call, span: to_span(a.lhs.span()) },
                    invalidated,
                }
            } else {
                // Assignment to a non-simple lvalue (`$a[i] = …`, `$o->$p = …`,
                // `$a->b->c = …`, `Foo::$s = …`). Barrier (the sound floor); a by-ref
                // property alias `$r = &$x->p` is caught by the poison family above.
                Stmt { span: ZERO_SPAN, kind: StmtKind::Barrier, invalidated: Vec::new() }
            }
        }
        Expression::Call(Call::Function(fc)) => {
            let mut invalidated = Vec::new();
            collect_call_vars(&Node::Expression(expr), &mut invalidated);
            Stmt { span: ZERO_SPAN, kind: StmtKind::Call(lower_call(fc)), invalidated }
        }
        // Statement-level method / static / constructor calls. A resolvable
        // receiver becomes a `Call`; a dynamic one is a `Barrier` (but its
        // call-var invalidation is still collected below via the fallthrough).
        Expression::Call(Call::Method(_) | Call::NullSafeMethod(_) | Call::StaticMethod(_))
        | Expression::Instantiation(_) => match named_call(expr) {
            Some(call) => {
                let mut invalidated = Vec::new();
                collect_call_vars(&Node::Expression(expr), &mut invalidated);
                Stmt { span: ZERO_SPAN, kind: StmtKind::Call(call), invalidated }
            }
            None => {
                let mut invalidated = Vec::new();
                collect_call_vars(&Node::Expression(expr), &mut invalidated);
                Stmt { span: ZERO_SPAN, kind: StmtKind::Barrier, invalidated }
            }
        },
        // A statement-position `match` (ADR-0031 Part B): structure its arms when
        // the subject and every arm condition lower to a variable/literal; else
        // fall back to `Opaque` over the whole subtree (partial structuring is
        // unsound for the first-match / no-default-throws rules).
        Expression::Match(m) => lower_match_stmt(m).unwrap_or_else(|| {
            let node = Node::Expression(expr);
            let (writes, reads, poisons) = opaque_sets(&node);
            Stmt { span: ZERO_SPAN, kind: StmtKind::Opaque { writes, reads, poisons }, invalidated: Vec::new() }
        }),
        // `throw <expr>;` — a trace terminator (ADR-0031). Variables the thrown
        // expression hands to a call are still invalidated (by-ref conservatism),
        // though the terminator makes anything after it unreachable.
        Expression::Throw(t) => {
            let mut invalidated = Vec::new();
            collect_call_vars(&Node::Expression(t.exception), &mut invalidated);
            Stmt { span: ZERO_SPAN, kind: StmtKind::Throw { span: to_span(expr.span()) }, invalidated }
        }
        // `exit;` / `die;` — a trace terminator (ADR-0019 never-returns).
        Expression::Construct(Construct::Exit(_) | Construct::Die(_)) => {
            Stmt { span: ZERO_SPAN, kind: StmtKind::Exit { span: to_span(expr.span()) }, invalidated: Vec::new() }
        }
        _ => Stmt { span: ZERO_SPAN, kind: StmtKind::Barrier, invalidated: Vec::new() },
    }
}

/// Collect the names of bare local variables passed as an argument to any call
/// within `node`. Used to invalidate those variables after the statement.
fn collect_call_vars(node: &Node<'_, '_>, out: &mut Vec<String>) {
    let arguments = match node {
        Node::FunctionCall(c) => Some(&c.argument_list),
        Node::MethodCall(c) => Some(&c.argument_list),
        Node::NullSafeMethodCall(c) => Some(&c.argument_list),
        Node::StaticMethodCall(c) => Some(&c.argument_list),
        _ => None,
    };
    if let Some(list) = arguments {
        for arg in list.arguments.iter() {
            if let Expression::Variable(Variable::Direct(dv)) = arg.value().unparenthesized() {
                let name = strip_dollar(bytes_to_string(dv.name));
                if !out.contains(&name) {
                    out.push(name);
                }
            }
        }
    }
    for child in node.children() {
        collect_call_vars(&child, out);
    }
}

/// Collect the names of variables a subtree may **write** — over-approximated,
/// which is always sound (it only makes the walk forget more). Covers every
/// assignment lvalue, compound assignment, increment/decrement, `foreach`
/// value/key binding, `catch` parameter, and `list()`/array destructuring
/// target. Does **not** descend into nested function-like bodies (separate
/// scopes); their internal writes are not the enclosing construct's concern.
fn collect_assign_writes(node: &Node<'_, '_>, out: &mut Vec<String>) {
    match node {
        // Any direct variable in an assignment lvalue is a write target
        // (`$a[$i] = …` over-collects `$i` too — sound). Recurse into the rhs
        // for nested writes/increments; the lhs is handled here in full.
        Node::Assignment(a) => {
            collect_direct_vars(&Node::Expression(a.lhs), out);
            collect_assign_writes(&Node::Expression(a.rhs), out);
            return;
        }
        // `++$x` / `--$x` write their operand; other prefix operators do not.
        Node::UnaryPrefix(u) => {
            if matches!(
                u.operator,
                UnaryPrefixOperator::PreIncrement(_) | UnaryPrefixOperator::PreDecrement(_)
            ) {
                collect_direct_vars(&Node::Expression(u.operand), out);
            }
        }
        // `$x++` / `$x--` (the only postfix operators) write their operand.
        Node::UnaryPostfix(u) => collect_direct_vars(&Node::Expression(u.operand), out),
        // `foreach ($it as $v)` / `foreach ($it as $k => $v)` bind their targets.
        Node::ForeachValueTarget(t) => {
            collect_direct_vars(&Node::Expression(t.value), out);
            return;
        }
        Node::ForeachKeyValueTarget(t) => {
            collect_direct_vars(&Node::Expression(t.key), out);
            collect_direct_vars(&Node::Expression(t.value), out);
            return;
        }
        // `catch (T $e)` binds the exception variable; recurse into the block.
        Node::TryCatchClause(c) => {
            if let Some(v) = &c.variable {
                let name = strip_dollar(bytes_to_string(v.name));
                if !out.contains(&name) {
                    out.push(name);
                }
            }
        }
        // Nested scopes are their own concern — do not count their writes.
        Node::Function(_)
        | Node::Closure(_)
        | Node::ArrowFunction(_)
        | Node::AnonymousClass(_)
        | Node::Class(_)
        | Node::Interface(_)
        | Node::Trait(_)
        | Node::Enum(_) => return,
        _ => {}
    }
    for child in node.children() {
        collect_assign_writes(&child, out);
    }
}

/// Collect every direct variable name (`$x` → `x`) anywhere in a subtree. Used
/// for assignment-lvalue / binding positions where over-collection is intended.
fn collect_direct_vars(node: &Node<'_, '_>, out: &mut Vec<String>) {
    if let Node::DirectVariable(dv) = node {
        let name = strip_dollar(bytes_to_string(dv.name));
        if !out.contains(&name) {
            out.push(name);
        }
    }
    for child in node.children() {
        collect_direct_vars(&child, out);
    }
}

/// Collect the **read set** of an `Opaque` construct: every direct variable
/// mentioned anywhere in the subtree (conditions, call arguments, expressions)
/// that is not already a `write`. Over-collection is sound (it only forgets
/// more). Nested function-like bodies are their own scopes and are **not**
/// descended, exactly as [`collect_assign_writes`] treats them.
fn collect_read_vars(node: &Node<'_, '_>, writes: &[String], out: &mut Vec<String>) {
    match node {
        Node::DirectVariable(dv) => {
            let name = strip_dollar(bytes_to_string(dv.name));
            if !writes.contains(&name) && !out.contains(&name) {
                out.push(name);
            }
        }
        // Nested scopes are their own concern — do not read their internals.
        Node::Function(_)
        | Node::Closure(_)
        | Node::ArrowFunction(_)
        | Node::AnonymousClass(_)
        | Node::Class(_)
        | Node::Interface(_)
        | Node::Trait(_)
        | Node::Enum(_) => return,
        _ => {}
    }
    for child in node.children() {
        collect_read_vars(&child, writes, out);
    }
}

/// Whether a node (scanned within a single scope, not descending into nested
/// function-like bodies) contains a construct on the ADR-0001 whole-scope
/// give-up list. Over-detection is always safe — it only silences the scope.
fn node_poisons(node: &Node<'_, '_>) -> bool {
    match node {
        // Direct markers.
        Node::Global(_)
        | Node::Static(_)
        | Node::EvalConstruct(_)
        | Node::IncludeConstruct(_)
        | Node::IncludeOnceConstruct(_)
        | Node::RequireConstruct(_)
        | Node::RequireOnceConstruct(_)
        | Node::NestedVariable(_)
        | Node::IndirectVariable(_) => return true,
        // `extract(...)` / `compact(...)`.
        Node::FunctionCall(fc) => {
            if let Expression::Identifier(id) = fc.function {
                let name = bytes_to_string(id.last_segment());
                if name == "extract" || name == "compact" {
                    return true;
                }
            }
        }
        // Reference assignment `$x = &$y`.
        Node::Assignment(a) => {
            if a.rhs.is_reference() {
                return true;
            }
        }
        // Closure: inspect its `use (&$x)` capture list, but do not descend into
        // its body (a separate scope).
        Node::Closure(c) => {
            if let Some(use_clause) = &c.use_clause {
                for v in use_clause.variables.iter() {
                    if v.ampersand.is_some() {
                        return true;
                    }
                }
            }
            return false;
        }
        // Other nested scopes — skip entirely (their own give-up list is their
        // own concern).
        Node::Function(_)
        | Node::ArrowFunction(_)
        | Node::AnonymousClass(_)
        | Node::Class(_)
        | Node::Interface(_)
        | Node::Trait(_)
        | Node::Enum(_) => return false,
        _ => {}
    }
    node.children().iter().any(node_poisons)
}

// ---------------------------------------------------------------------------
// Namespace contexts and name resolution helpers.
// ---------------------------------------------------------------------------

/// Build a [`NameRef`] from a Mago identifier: its raw spelling (leading `\`
/// stripped for fully-qualified names), the qualification [`RefKind`], and the
/// reference's byte offset (for context lookup).
fn name_ref(id: &Identifier<'_>) -> NameRef {
    let kind = match id {
        Identifier::Local(_) => RefKind::Unqualified,
        Identifier::Qualified(_) => RefKind::Qualified,
        Identifier::FullyQualified(_) => RefKind::FullyQualified,
    };
    let raw = bytes_to_string(id.value()).trim_start_matches('\\').to_owned();
    NameRef { raw, kind, offset: to_span(id.span()).start }
}

/// Build the file's namespace contexts (index 0 = global) and the byte regions
/// each namespace declaration covers. Every `namespace` node in the file becomes
/// one context (its name plus the `use` imports at its body's top level);
/// top-level `use` statements outside any namespace populate the global context.
fn build_contexts(program: &Program<'_>) -> (Vec<NsCtx>, Vec<(u32, u32, usize)>) {
    let mut contexts = vec![NsCtx::global()];
    let mut regions: Vec<(u32, u32, usize)> = Vec::new();

    // Global-context imports: top-level `use` statements (a file with a
    // file-scoped `namespace A;` has none — its statements nest under the node).
    for stmt in program.statements.iter() {
        if let Statement::Use(u) = stmt {
            add_use(u, &mut contexts[0]);
        }
    }

    // One context per namespace declaration, anywhere in the tree. Namespaces do
    // not nest semantically, but a second file-scoped `namespace B;` may sit
    // inside the first's implicit body sequence; a byte offset then falls inside
    // both spans and [`ctx_of`] picks the innermost (latest-starting) region.
    collect_namespaces(&Node::Program(program), &mut contexts, &mut regions);
    (contexts, regions)
}

fn collect_namespaces(
    node: &Node<'_, '_>,
    contexts: &mut Vec<NsCtx>,
    regions: &mut Vec<(u32, u32, usize)>,
) {
    if let Node::Namespace(ns) = node {
        let name = ns
            .name
            .as_ref()
            .map(|id| bytes_to_string(id.value()).trim_start_matches('\\').to_owned())
            .unwrap_or_default();
        let mut ctx = NsCtx { namespace: name, ..NsCtx::global() };
        // `use` imports at the namespace body's top level.
        for stmt in ns.statements().iter() {
            if let Statement::Use(u) = stmt {
                add_use(u, &mut ctx);
            }
        }
        let idx = contexts.len();
        contexts.push(ctx);
        let span = to_span(ns.span());
        regions.push((span.start, span.end, idx));
    }
    for child in node.children() {
        collect_namespaces(&child, contexts, regions);
    }
}

/// Fold one `use` statement's items into a context. Only the plain sequence form
/// (`use A\B, C\D;` — class/namespace imports) and the typed-sequence
/// `use function a\b;` form are lowered; grouped `use A\{B, C}` and `use const`
/// are conservatively skipped (a miss only *fails to resolve*, never mis-resolves).
fn add_use(u: &mago_syntax::cst::Use<'_>, ctx: &mut NsCtx) {
    match &u.items {
        UseItems::Sequence(seq) => {
            for item in seq.items.iter() {
                let target = bytes_to_string(item.name.value()).trim_start_matches('\\').to_owned();
                let alias = match &item.alias {
                    Some(a) => bytes_to_string(a.identifier.value),
                    None => bytes_to_string(item.name.last_segment()),
                };
                ctx.class_imports.insert(alias.to_ascii_lowercase(), target);
            }
        }
        UseItems::TypedSequence(seq) if seq.r#type.is_function() => {
            for item in seq.items.iter() {
                let target = bytes_to_string(item.name.value()).trim_start_matches('\\').to_owned();
                let alias = match &item.alias {
                    Some(a) => bytes_to_string(a.identifier.value),
                    None => bytes_to_string(item.name.last_segment()),
                };
                ctx.fn_imports.insert(alias.to_ascii_lowercase(), target);
            }
        }
        // `use const …`, grouped `use A\{…}` — conservatively un-lowered.
        UseItems::TypedSequence(_) | UseItems::TypedList(_) | UseItems::MixedList(_) => {}
    }
}

/// The namespace context enclosing `offset`: the innermost (latest-starting)
/// namespace region containing it, else the global context (index 0).
fn ctx_of<'a>(contexts: &'a [NsCtx], regions: &[(u32, u32, usize)], offset: u32) -> &'a NsCtx {
    let mut best: Option<(u32, usize)> = None;
    for &(start, end, idx) in regions {
        if offset >= start && offset < end && best.is_none_or(|(bstart, _)| start >= bstart) {
            best = Some((start, idx));
        }
    }
    &contexts[best.map_or(0, |(_, idx)| idx)]
}

/// The lowercase-normalized FQN of a declaration named `name` in context `ctx`.
fn fqn_of(ctx: &NsCtx, name: &str) -> String {
    if ctx.namespace.is_empty() {
        name.to_ascii_lowercase()
    } else {
        format!("{}\\{}", ctx.namespace, name).to_ascii_lowercase()
    }
}

// ---------------------------------------------------------------------------
// Small helpers.
// ---------------------------------------------------------------------------

fn to_span(span: mago_span::Span) -> Span {
    Span { start: span.start.offset, end: span.end.offset }
}

/// Lower one trivium to a [`Comment`], dropping whitespace trivia (`None`).
fn lower_comment(t: &Trivia<'_>) -> Option<Comment> {
    let kind = match t.kind {
        TriviaKind::SingleLineComment => CommentKind::Line,
        TriviaKind::HashComment => CommentKind::Hash,
        TriviaKind::MultiLineComment => CommentKind::Block,
        TriviaKind::DocBlockComment => CommentKind::DocBlock,
        TriviaKind::WhiteSpace => return None,
    };
    Some(Comment { kind, span: to_span(t.span), text: bytes_to_string(t.value) })
}

fn bytes_to_string(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn strip_dollar(name: String) -> String {
    name.strip_prefix('$').map_or(name.clone(), ToOwned::to_owned)
}

fn line_starts(source: &str) -> Vec<u32> {
    let mut starts = vec![0u32];
    for (i, b) in source.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i as u32 + 1);
        }
    }
    starts
}
