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
use mago_syntax::cst::ClassLikeConstantSelector;
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
use mago_syntax::cst::MagicConstant;
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
    /// `namespace\bar` — the PHP `namespace`-keyword relative form (ADR-0049 A8):
    /// the leading `namespace\` is dropped and the remainder resolves against the
    /// **enclosing namespace only**, with no `use` imports applied and — for
    /// functions — no global fallback (an undefined `Ns\bar` is a fatal error, not
    /// a fall-through to global `bar`). The stored `raw` is the remainder with the
    /// `namespace\` prefix already stripped (`bar`, `Sub\bar`). Lowering this as a
    /// distinct kind is what stops the pre-A8 doubled-prefix mis-resolution
    /// (`Ctx\namespace\bar`) that would manufacture spurious absence.
    Relative,
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

/// One member of a native union type: one of the four scalars, a `false` /
/// `true` bool-literal pseudo-member (PHP allows `false`/`true` as literal type
/// members, e.g. `string|false`), or a class/interface/enum **object** type
/// (ADR-0043 object/method world).
///
/// [`TypeMember::Instance`] carries the namespace-resolved FQN twice: the
/// lowercase-normalized form (matching [`ClassDecl::fqn`] — the matching key)
/// and the source-cased form (diagnostics only), so `Foo|null` / `A|B` are one
/// union shape alongside the scalars. It is **not** [`Copy`] (it owns
/// `String`s); the whole enum is therefore no longer `Copy`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TypeMember {
    /// A full scalar type (`int`, `float`, `string`, `bool`).
    Scalar(ScalarType),
    /// A `false` / `true` literal type. It accepts **only** the exact matching
    /// bool value — no other value coerces into it (empirically verified against
    /// PHP 8.5: `0`/`""`/`true` into a `false`-only type all `TypeError`).
    BoolLiteral(bool),
    /// An object type: a class / interface / enum name (ADR-0043). The is-a
    /// oracle consumes this in later stages; native scalar-value acceptance
    /// stays silent on any union that contains an `Instance` member until the
    /// definite-No arm opens.
    Instance {
        /// The namespace-resolved, **lowercase-normalized** FQN (matching
        /// [`ClassDecl::fqn`]). Every matching / resolution consumer keys on
        /// this — case-insensitivity lives here, never in `display`.
        fqn: String,
        /// The same resolved FQN with the source's declared casing preserved
        /// (`LogicException`, `App\Foo`). Diagnostic rendering only; carries no
        /// resolution semantics.
        display: String,
    },
    /// A native **intersection** of object types (`A&B&…`, ADR-0043 — the
    /// conjunctive object member the union shape deferred). A single union
    /// member that is itself a *conjunction*: an object satisfies it only when
    /// it is-a **every** listed class, so it is rejected the moment it is-a-`No`
    /// against **any** one of them. PHP forbids scalar/`null` members inside an
    /// intersection, so every element is an object type; a scalar value never
    /// satisfies it. Carrying the intersection as one member keeps DNF types
    /// (`(A&B)|C`) a single [`NativeType`] union.
    InstanceInter(Vec<ClassRef>),
}

/// One class/interface membership within a native object type — the resolved
/// FQN carried twice, exactly as [`TypeMember::Instance`] carries it: the
/// lowercase-normalized matching key and the source-cased display form. It is
/// the element of an intersection member ([`TypeMember::InstanceInter`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ClassRef {
    /// The namespace-resolved, **lowercase-normalized** FQN (matching
    /// [`ClassDecl::fqn`]) — the matching / is-a key.
    pub fqn: String,
    /// The same resolved FQN with the source's declared casing preserved.
    /// Diagnostic rendering only.
    pub display: String,
}

impl TypeMember {
    /// Render this member for a diagnostic message: the PHP keyword for a scalar
    /// or bool-literal, or the source-cased FQN for an object member.
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
        let mut parts: Vec<String> = self.members.iter().map(TypeMember::render_member).collect();
        if self.nullable {
            if parts.len() == 1 {
                return format!("?{}", parts[0]);
            }
            parts.push("null".to_owned());
        }
        parts.join("|")
    }

    /// `true` when any union member is an object ([`TypeMember::Instance`]) type.
    /// Every native **scalar-value** consumer treats an `Instance`-bearing type
    /// exactly as it treated an absent (`None`) type before ADR-0043 — the
    /// zero-behavior-change invariant of stage 1. The definite-No object arm
    /// (stage 3) is the only place this guard is lifted.
    #[must_use]
    pub fn has_instance(&self) -> bool {
        self.members
            .iter()
            .any(|m| matches!(m, TypeMember::Instance { .. } | TypeMember::InstanceInter(_)))
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
    /// `true` when the parameter declares a default value (`= …`), of any form.
    /// A default must be admitted by any native type promoted onto the parameter,
    /// or PHP rejects the declaration at compile time (`int $x = 'str'`).
    pub has_default: bool,
    /// The lowered default value when it is a representable literal / array; a
    /// non-representable default (a constant, `self::X`, an expression) lowers to
    /// `None` even though [`Self::has_default`] is `true`.
    pub default: Option<ArgValue>,
    pub span: Span,
}

/// A structural effect-origin candidate found by scanning a function body's CST
/// subtree (ADR-0005 effect envelopes). Syntax only reports *where* a primitive
/// effect could arise; the catalog/inference layer decides which are proven
/// findings (uncatalogued builtins widen to silence, same-file user calls become
/// propagation edges — `steins_catalog::effect_labels` and the effects pass).
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
/// The classification of one call argument's **lvalue root**, for by-ref
/// out-parameter effect coloring (ADR-0063 §2.3). Recorded for every positional
/// argument of a statically-named call; the effects pass reads only the
/// positions `steins_catalog::out_params` declares by-ref, so the classification
/// costs nothing when the callee has no out-parameter row.
///
/// The distinction is the whole point of the `mutate.local` color: `preg_match`
/// writing `$matches` and `preg_match` writing `$this->matches` are the same
/// function and *different effects*, and only the argument says which.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefTarget {
    /// A binding **private to the calling frame**: a plain `$v` (or an offset
    /// under one, `$v['k']`) that is not a by-ref parameter, in a frame with no
    /// aliasing construct. A write through it cannot be observed from outside
    /// the frame → `mutate.local`.
    Local,
    /// A superglobal root (`$_SESSION`, `$GLOBALS`, `$_SERVER`, …): an
    /// interpreter-global surface (ADR-0055 amendment) → `global.write`.
    Superglobal,
    /// The write escapes the frame, or its target cannot be classified: a
    /// property / static-property / class-constant root, a by-ref parameter, any
    /// variable in a frame carrying an aliasing construct (`global`, `static`,
    /// `$$v`, `extract`, `$a = &$b`, `use (&$x)`), or any other expression. The
    /// conservative parent `mutate` — never `mutate.local`.
    Escaping,
}

/// The nine PHP superglobals. A by-ref write whose root is one of these is an
/// interpreter-global write however local the syntax looks.
const SUPERGLOBALS: &[&str] = &[
    "GLOBALS", "_SERVER", "_GET", "_POST", "_FILES", "_COOKIE", "_SESSION", "_REQUEST", "_ENV",
];

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EffectOrigin {
    /// A call to a statically-named function at `span` (the callee identifier).
    /// `name` carries the full reference (raw spelling + qualification) so the
    /// effects pass can resolve it project-wide: it may resolve to a builtin
    /// (classified via the catalog), a user function anywhere in the project (an
    /// effect propagation edge), or nothing (ambiguous → taints exhaustiveness).
    /// Dynamic and method calls are not recorded here.
    ///
    /// `arg_targets` classifies each **positional** argument's lvalue root for
    /// by-ref out-parameter coloring (ADR-0063 §2.3), in order — so its length is
    /// the positional arity, which is the conditional row's arity leg. It is
    /// `None` when the argument list uses a named or spread argument: positional
    /// mapping is defeated there, and every argument-conditional judgment is then
    /// withheld rather than guessed (the same silence an uncatalogued builtin
    /// gets). `Some(vec![])` is a genuine zero-argument call and is *not* the
    /// same thing — `preg_match(matches: $m, …)` supplies its out-parameter,
    /// `preg_match()` does not.
    Call { name: NameRef, span: Span, arg_targets: Option<Vec<RefTarget>> },
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
    /// analogue of the `EffectOrigin::Call` function edge), and so a *declared*
    /// receiver ([`EffectRecv::Var`] / [`EffectRecv::PropRead`], ADR-0067) can
    /// carry an interface envelope into the caller's declared lane. Receivers
    /// outside those forms (`static::m()`, `$o->$m()`, a written-to variable) are
    /// **not** recorded — no provable edge and no declared bound either.
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
    ///
    /// `arg_targets` is the same per-position lvalue-root classification
    /// [`Self::Call`] carries, and is always `arg_count` long here (this variant
    /// is only produced for all-positional argument lists). Higher-order invokers
    /// are out-parameter writers too — `usort($rows, $cmp)` sorts `$rows` in
    /// place — so the by-ref row must be read on this arm as well.
    HigherOrder {
        callee: NameRef,
        callbacks: Vec<(usize, CallbackRef)>,
        arg_count: usize,
        arg_targets: Vec<RefTarget>,
        span: Span,
    },
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
    /// `$r->m()` where `$r` is a name this frame **never writes** (ADR-0067
    /// declared lane). Carries the variable name (no `$`); the effects pass reads
    /// the enclosing declaration's parameter list for its declared type, and
    /// contributes the *declared* envelope of a project interface's method — never
    /// a proven effect, and never a resolved body edge. A receiver whose declared
    /// type is not a project interface (or whose method carries no envelope)
    /// resolves to nothing and taints exhaustiveness, exactly as [`Self::Opaque`]
    /// did before this variant existed.
    Var(String),
    /// `$this->repo->m()` where `repo` is a property this frame never writes — the
    /// property-read twin of [`Self::Var`], carrying the property name. Resolved
    /// against the enclosing class's declared (or constructor-promoted) property
    /// type under the same declared-lane rules.
    PropRead(String),
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
/// slice (see `attrs_effect_envelope`).
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
    /// `attrs_effect_envelope`.
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
    /// The **file byte span** of the associated docblock (the same trivium whose
    /// text is [`Self::docblock`]), when one is adopted. `docblock` text is the
    /// exact substring `[span.start, span.end)` of the source, so a docblock-
    /// relative offset (e.g. a `steins_phpdoc` tag span) maps into the file by
    /// adding `span.start`. Retained for the transform engine (ADR-0034), which
    /// deletes a promoted `@param` tag's line in the file.
    pub docblock_span: Option<Span>,
    /// `true` when this function is declared inside a conditional/nested context
    /// (anything but the program root or a bare namespace) — the function analogue
    /// of [`ClassDecl::conditional`] (ADR-0049 A2i). A conditional function
    /// declaration leaves *which* body binds at runtime to load order (the
    /// `function_exists`-guarded polyfill beside a dam-site include is the shape),
    /// so the arity check re-dams the claim: an arity finding on a conditional
    /// target fires only when the whole-universe dam is clear.
    pub conditional: bool,
}

/// A method's declared visibility. Absent visibility modifiers default to
/// `Public` (PHP semantics).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Visibility {
    Public,
    Protected,
    Private,
}

/// The late-static-binding return keyword a method declares in return position:
/// a bare `self` / `static` / `parent` (ADR-0043 amendment). `lower_method` sees
/// the hint but has no class context, so it records only the keyword *kind* and
/// nullability here; the [`SourceTree`]-build FQN-stamping pass — which owns the
/// enclosing class's resolved name — resolves the kind to the actual bound and
/// synthesizes [`MethodDecl::ret`]. Any other return shape (a union containing
/// `static`, a plain class name, a scalar) leaves this `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RetBoundKind {
    /// `: self` — bound is the enclosing class directly (not late-bound).
    SelfKw,
    /// `: static` — bound is the enclosing class as the *minimum* late-bound
    /// class (the minimum-bound lemma: every late-bound `T` is-a the enclosing
    /// class, so the enclosing class is a necessary bound).
    Static,
    /// `: parent` — bound is the resolved `extends` parent.
    Parent,
}

/// The recorded return-position LSB keyword shape (kind + nullability) of a
/// method, before the enclosing-class context is available to resolve it to a
/// bound (ADR-0043 amendment §2). See [`RetBoundKind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RetBoundKeyword {
    pub kind: RetBoundKind,
    /// `true` when the hint was `?self` / `?static` / `?parent` (the nullable
    /// bound also accepts `null`, so `return null` stays silent).
    pub nullable: bool,
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
    /// / `void` / `never` (the return-type check skips those; zero-FP). For a
    /// bare/nullable `self`/`static`/`parent` return this is synthesized in the
    /// FQN-stamping pass (ADR-0043 amendment) to a single-member `Instance` of
    /// the resolved bound; see [`MethodDecl::ret_bound_keyword`].
    pub ret: Option<NativeType>,
    /// The recorded LSB return-keyword shape (`self`/`static`/`parent`,
    /// nullable-aware), when the return hint was one (ADR-0043 amendment). The
    /// FQN-stamping pass consumes it to synthesize [`Self::ret`] once the
    /// enclosing class's resolved name (and, for `parent`, the resolved parent)
    /// is known; `None` for every other return shape.
    pub ret_bound_keyword: Option<RetBoundKeyword>,
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
    /// The **file byte span** of the associated docblock, when one is adopted —
    /// the method-world analogue of [`FunctionDecl::docblock_span`]. `docblock`
    /// text is the exact substring `[span.start, span.end)` of the source, so a
    /// docblock-relative tag offset maps into the file by adding `span.start`.
    /// Retained for the transform engine (ADR-0034 / ADR-0043 §6), which deletes a
    /// promoted `@param` tag and rewrites a `@param`/`@return` type text.
    pub docblock_span: Option<Span>,
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
    /// `true` when this property carries a PHP 8.4 property hook (`get`/`set`),
    /// promoted or class-body (FP class 16). A hook is arbitrary user code: a `set`
    /// hook stores whatever it computes (not the assigned value), a `get` hook
    /// returns a computed value, and a virtual (backing-store-less) property holds
    /// no value at all. So a hooked property **binds no value fact ever** and is
    /// excluded from every value/mismatch check — the raw assigned/constructed value
    /// is not the property's value. Only class-surface facts (the name exists, its
    /// visibility) remain valid. Class-body hooked properties are dropped entirely at
    /// lowering (never reach here); this flag carries the promoted-param case, which
    /// must stay on the surface (its name is a real property) while binding nothing.
    pub hooked: bool,
    /// The raw `/** … */` docblock preceding a plain property (for `@var` contract
    /// extraction; promoted params carry `@param` on the ctor, not `@var`, so this
    /// stays `None` for them).
    pub docblock: Option<String>,
    pub span: Span,
}

/// One case of a lowered `enum` (ADR-0043). Minimal by design: the case name
/// (as written) plus the backed value **when it is a representable literal**
/// (`case A = 1;` / `case A = 'x';`). A unit-enum case, or a backed case whose
/// initializer is not a literal, carries `value: None`. Enum cases are *not*
/// heap-tracked properties — they are class constants whose value is an object
/// of the enum class — so they live here, off the property path.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EnumCaseDecl {
    /// The case name as written (e.g. `Hearts`).
    pub name: String,
    /// The backed value, when a representable literal; `None` for unit cases or
    /// non-literal initializers.
    pub value: Option<ArgValue>,
    pub span: Span,
}

/// A user-defined class, **interface**, or **enum** declaration (top-level or
/// namespaced). Interfaces are lowered (ADR-0033 Liskov), distinguished by
/// [`Self::is_interface`]; enums are lowered (ADR-0043 object/method world),
/// distinguished by [`Self::is_enum`] and carrying [`Self::enum_cases`] +
/// [`Self::enum_backing`]. A class that *uses* a trait sets
/// [`ClassDecl::uses_traits`] so resolution gives up on it.
///
/// Enum lowering in v1 is deliberately minimal: cases, backing type, and the
/// `implements` list (for the is-a oracle) are recorded, but enum **method
/// bodies are not analyzed** (no scope is built for them; [`Self::methods`] is
/// left empty). This keeps stage 1 zero-behavior-change — an enum body cannot
/// introduce new throw/effect/Liskov findings — while still placing the enum in
/// the class index so subtyping can reason about it. Deferred-with-design:
/// enum method resolution/analysis lands with the method-transform stage.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ClassDecl {
    /// Simple (unqualified) class name as written at the declaration site (used
    /// for diagnostics).
    pub name: String,
    /// The fully-qualified name, lowercase-normalized. The project index keys on
    /// this; for a global class it equals the lowercased simple name.
    pub fqn: String,
    /// The same resolved FQN with the source's declared casing preserved
    /// (`AllowedSubtypesEnum\Foo`, no leading `\`). Diagnostic / dump rendering
    /// only — carries no resolution semantics (mirrors [`TypeMember::Instance`]'s
    /// `display`). Filled in `parse` from the enclosing namespace ctx alongside
    /// [`Self::fqn`]; empty until then.
    pub display: String,
    pub is_final: bool,
    /// `true` when this declaration is an `abstract class`. An abstract class
    /// cannot be instantiated (`new AbstractC()` raises `Error: Cannot instantiate
    /// abstract class` — before any constructor `ArgumentCountError`), so the arity
    /// family (ADR-0049 §6) silences constructor claims on it. `false` for
    /// interfaces/enums/traits (each already flagged by its own bit).
    pub is_abstract: bool,
    /// `true` when this declaration is an `interface` (not a `class`). Interface
    /// methods are abstract; they carry envelopes/`@throws` but no bodies.
    pub is_interface: bool,
    /// `true` when this declaration is an `enum` (ADR-0043). An enum is
    /// implicitly `final`; [`Self::enum_cases`] and [`Self::enum_backing`] carry
    /// its cases and (for a backed enum) backing scalar. It implicitly implements
    /// `UnitEnum` (and `BackedEnum` when backed) — recorded in the catalog's
    /// interface tree, not here.
    pub is_enum: bool,
    /// `true` when this declaration is a `trait` (ADR-0049 §5, C8/A2i). A trait
    /// enters the class-*like* index as a **name** — the `class.undefined` closure
    /// set is the class-like name set, traits included, since a static call through
    /// a trait name runs (deprecated, not fatal). V1 lowers the name only: no
    /// member flattening (`methods`/`properties`/… are empty), so a trait is inert
    /// for every existing check and merely occupies its FQN in the symbol/ambiguity
    /// table (a trait sharing an FQN with a class is `Ambiguous`, both silent).
    pub is_trait: bool,
    /// `true` when this declaration is nested under anything but a plain
    /// namespace/program node — a function/method body, `if`, `try`, loop, or bare
    /// block (ADR-0049 A2i). A conditional declaration leaves *which* definition
    /// binds to runtime load order (the `if (!class_exists(…))` fallback-stub
    /// shape), so a chain containing one **re-dams** absence claims. Consumed by
    /// the finding-breadth family from S2 on; carried but unread in S1.
    pub conditional: bool,
    /// A backed enum's backing scalar (`enum E: int` / `enum E: string`), or
    /// `None` for a pure (unit) enum. Only `int`/`string` are legal backings.
    pub enum_backing: Option<ScalarType>,
    /// The enum's cases (empty for non-enums). See [`EnumCaseDecl`].
    pub enum_cases: Vec<EnumCaseDecl>,
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
    /// Class constants with a **literal** initializer, as `(name, value)` pairs
    /// (ADR-0043 §2). Only `const NAME = <literal>;` is recorded — a non-literal
    /// initializer (an expression, another const, `new`, …) is omitted entirely,
    /// so a name's *absence* means "no proven literal value", never "no such
    /// constant". The name is stored as written (constant names are
    /// case-sensitive); enum-case pseudo-constants live in [`Self::enum_cases`],
    /// not here. Consumed by the class-constant value resolution.
    pub consts: Vec<(String, ArgValue)>,
    /// `true` if the class `use`s any trait. Trait methods are merged into the
    /// class at compile time but their bodies live elsewhere, so a
    /// trait-using class is treated as unresolvable (give up → silent).
    pub uses_traits: bool,
    /// The raw `/** … */` docblock preceding the class-like declaration, if any.
    /// Read for class-level `@template` names, which shadow same-named classes in
    /// **every** member docblock of this class-like (issue #5). `None` for a trait
    /// (traits lower no members this slice, so a class-level template is inert).
    pub docblock: Option<String>,
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
    /// A call `name(args...)` to a statically-named function. `name` is the
    /// identifier's **last segment** (no namespace survives into the value IR), so
    /// value-position resolution is by unique simple name project-wide. `args` are
    /// the lowered argument values: a zero-argument call resolves through the
    /// constant-function lane, and a call **with** arguments resolves through the
    /// T0 binding-descent summary (issue #60) — as a dumped/checked argument and
    /// as a nested argument of another descent. (A builtin's fold gate still sees
    /// only direct literals: `strtoupper(g(1))` widens — the value lane must not
    /// be reachable from `resolve_literal`, whose call sites carry no recursion
    /// guard.)
    Call(String, Vec<ArgValue>),
    /// `new ClassName(args...)` — a construction rvalue. [`NameRef`] is the class
    /// reference as written (resolved to an FQN project-wide at use time).
    /// Carried so an assignment `$x = new Foo(...)` can record `$x`'s **exact
    /// class** in the propagation environment (the object's runtime class is
    /// fixed at construction). Not a scalar literal — it never flows into a
    /// scalar type check.
    /// The third field is the constructor's **named** arguments (`new Foo(n: 1)`),
    /// so a promoted-property seed can bind them by name exactly as it binds the
    /// positional `args` (Gap A value-binding side); empty for a positional-only
    /// construction.
    New(NameRef, Vec<ArgValue>, Vec<NamedArg>),
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
    /// so an assignment `$f = fn(...) => …;` records a `Fact`-carrying closure
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
    /// A class-constant / enum-case access `Class::NAME` (ADR-0043): the class
    /// portion (an explicit name or `self`/`static`/`parent`) plus the constant
    /// or case name. Syntactically a class-const and an enum-case are identical
    /// (`Suit::Hearts` vs `Config::TIMEOUT`); the enum distinction needs the
    /// project index, so lowering emits this uniform form and the inference layer
    /// reinterprets it against a resolved enum (→ an [`ArgValue::EnumCase`] object
    /// value) or resolves the literal constant value. Until then it is an
    /// **unproven** value — treated exactly like [`ArgValue::Other`] (never flows
    /// into a scalar check, resolves to no proven value).
    ClassConst(StaticClass, String),
    /// An enum-case object value `Enum::Case` (ADR-0043): the resolved,
    /// lowercase enum FQN plus the case name. This is an *object* value of the
    /// enum class (is-a the enum's interfaces + `UnitEnum`/`BackedEnum`). It is
    /// produced by the inference layer when a [`ArgValue::ClassConst`] resolves
    /// against a lowered enum — lowering never emits it directly (enum identity
    /// is a project-index fact, not a syntactic one). Like [`ArgValue::New`] it is
    /// not a scalar literal; native scalar checks stay silent on it.
    EnumCase(String, String),
    /// A null-coalescing rvalue `$a ?? $b` (ADR-0052 §6): the value is `$a` when it
    /// is set-and-non-null, else `$b`. The walk resolves it to
    /// `clear_null(fact($a)) join fact($b)` — the non-null part of `$a` unioned with
    /// `$b`. Only reached when both operands lower to a representable value; an
    /// operand the domain cannot spell (notably an array offset `$arr['k']`, which
    /// lowers to [`Self::Other`]) yields no fact, so `??` never manufactures a fact
    /// for a value it cannot see. Short-ternary `?:` still widens to `Other`.
    Coalesce(Box<ArgValue>, Box<ArgValue>),
    /// An array/offset read `$base[$key]` in **rvalue** position (ADR-0049 §7 / S3).
    /// `base` and `key` are the lowered sub-expressions (each may itself be any
    /// [`ArgValue`], commonly a [`Self::Var`] base and a literal/`Var` key). This is
    /// never a *proven* value (`val_of` yields `None`, [`Self::is_literal`] is
    /// `false`): the walk resolves the base to a container `Fact` and the key to a
    /// proven value, then judges `offset.missing` / `offset.on-unsupported` **only in
    /// the whitelisted read contexts** (ADR-0049 A7: plain assignment-RHS and return
    /// operands in v1). It is a *silence carrier* everywhere else — an operand of `??`
    /// ([`Self::Coalesce`]), a write lvalue, an `isset`/`array_key_exists` argument,
    /// or an array element never fires (the array element case collapses the whole
    /// literal to [`Self::Other`], as an offset read is not a proven element value).
    OffsetRead { base: Box<ArgValue>, key: Box<ArgValue> },
    /// A string concatenation `$a . $b` (issue #59). Lowered **structurally**, not
    /// folded here: the operands commonly include a [`Self::Var`] whose value only
    /// the walk knows (a parameter bound to a caller's literal, say), so the join
    /// belongs at resolution time where the env is in hand. Left-nested for a chain
    /// (`a . b . c` is `Concat(Concat(a, b), c)`), matching PHP's left associativity.
    ///
    /// This is not itself a proven value ([`Self::is_literal`] is `false`) — it
    /// resolves to one exactly when both operands resolve to values whose string
    /// cast is *total and environment-independent*. See the inference layer's
    /// `concat_cast` for that admission rule and why `float` is excluded.
    ///
    /// A compound `.=` still lowers its rvalue to [`Self::Other`] (see [`StmtKind`]);
    /// that is a documented deferral, not a semantic claim.
    Concat(Box<ArgValue>, Box<ArgValue>),
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

/// The PHP minor that changed the next-auto-index edge case for negative keys.
const NEXT_INT_RULE_CHANGED_IN: (u16, u16) = (8, 3);

/// PHP's next-auto-index rule for an omitted array key (`[$a, $b]`). The two
/// variants differ **only** when every integer key seen so far is negative —
/// every other literal normalizes identically under both.
///
/// PHP 8.3 changed the edge case: before it, the next auto-index floored at `0`,
/// so `[-5 => 'a', 'b']` put `'b'` at `0`; from 8.3 it is one past the largest
/// integer key seen, negative or not, so `'b'` lands at `-4`. Verified on PHP
/// 8.5.8 — `php -r 'var_export([-5=>"a","b"]);'` → `-5, -4`.
///
/// Steins' floor is 8.1 (ADR-0011), so both rules are live: 8.1/8.2 take
/// [`NextIntRule::FloorAtZero`], 8.3+ take [`NextIntRule::MaxPlusOne`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NextIntRule {
    /// PHP < 8.3 (Steins' floor 8.1 through 8.2): the next auto-index never
    /// goes below `0`.
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

/// Whether `items` normalizes *differently* under the two [`NextIntRule`]s: an
/// omitted key falls where every integer key seen so far is negative, so the
/// pre-8.3 floor and the 8.3+ max+1 disagree on its position.
///
/// This is the exact ambiguity [`normalize_array`] refuses to guess at when the
/// project's PHP minor is unknown. It is a purely syntactic property — no
/// version input — and false for every literal without negative integer keys.
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
        }
    }
    false
}

/// Resolve an array literal under an explicitly chosen [`NextIntRule`], applying
/// next-int assignment for `Auto` keys and **last-wins** for duplicates (a
/// repeated key updates the value in place, keeping the first position — PHP
/// semantics). The result is insertion-ordered.
///
/// Prefer [`normalize_array`], which picks the rule from the project's PHP minor
/// and declines to answer when the answer would be a guess. Use this only where
/// a rule is genuinely known, or where the result is not a proof-layer premise
/// (diagnostic rendering).
#[must_use]
pub fn normalize_array_with(
    items: &[(ArrayKey, ArgValue)],
    rule: NextIntRule,
) -> Vec<(NormKey, ArgValue)> {
    let mut out: Vec<(NormKey, ArgValue)> = Vec::with_capacity(items.len());
    // PHP's next auto-index: one past the largest integer key seen so far —
    // explicit or auto-assigned, and a duplicate still counts (verified:
    // `[5=>'a',5=>'b','c']` → 5, 6). `None` = no integer key yet → position 0.
    let mut max_seen: Option<i64> = None;
    for (k, v) in items {
        let key = match k {
            ArrayKey::Auto => {
                // Saturating: at `i64::MAX` PHP itself refuses to append; the
                // clamped index collides and last-wins folds it, which is as
                // close as a pure key model gets to that runtime error.
                let mut i = max_seen.map_or(0, |m: i64| m.saturating_add(1));
                if matches!(rule, NextIntRule::FloorAtZero) {
                    i = i.max(0);
                }
                max_seen = Some(max_seen.map_or(i, |m| m.max(i)));
                NormKey::Int(i)
            }
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

/// Resolve an array literal's raw `(ArrayKey, value)` entries to their PHP runtime
/// key→value map, choosing the next-auto-index rule from the project's own PHP
/// minor (ADR-0049 A12).
///
/// `php_minor` is the sidecar-reported `(major, minor)` — `Folder::php_minor()`,
/// the same input ADR-0052 A11 consumes — or `None` when no sidecar answered.
/// Returns `None` only when the minor is unknown *and* the literal actually
/// straddles the 8.3 rule change ([`next_int_is_version_dependent`]): the runtime
/// keys are then genuinely unproven, and a caller holding a proof-layer premise
/// (`===`/`==`, a `Val::Array` singleton fact) must drop the fact rather than
/// guess a key. Every version-independent literal — the overwhelming majority —
/// still answers under an unknown minor.
#[must_use]
pub fn normalize_array(
    items: &[(ArrayKey, ArgValue)],
    php_minor: Option<(u16, u16)>,
) -> Option<Vec<(NormKey, ArgValue)>> {
    match php_minor {
        Some(m) => Some(normalize_array_with(items, NextIntRule::for_minor(m))),
        None if next_int_is_version_dependent(items) => None,
        // The rules agree on this literal, so either one resolves it.
        None => Some(normalize_array_with(items, NextIntRule::MaxPlusOne)),
    }
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
            ArgValue::New(name, args, named) => {
                name.hash(state);
                args.hash(state);
                named.hash(state);
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
            ArgValue::Coalesce(l, r) => {
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
            ArgValue::ClassConst(class, name) => {
                class.hash(state);
                name.hash(state);
            }
            ArgValue::EnumCase(class, case) => {
                class.hash(state);
                case.hash(state);
            }
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
    /// literal whose every element is itself self-evident (recursively).
    ///
    /// This is [`Self::is_literal`] extended over the array carrier, and it is the
    /// predicate both guard narrowing ([`CondOperand::Literal`]) and the fold seam
    /// (ADR-0028) need: an array is a *value* exactly when nothing inside it is
    /// still unresolved. One `Var`/call/offset-read element anywhere in the tree
    /// leaves the whole array unproven — it widens rather than folding (issue #39),
    /// which is the only reading compatible with the zero-FP bar (ADR-0002).
    ///
    /// The empty array is concrete (`count([])` is a fold, not a widen). Keys need
    /// no test: lowering already refuses a non-literal key by collapsing the whole
    /// literal to [`Self::Other`] (see `lower_array_key`).
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
            ArgValue::Str(v) => format!("\"{v}\""),
            ArgValue::Bool(v) => v.to_string(),
            ArgValue::Null => "null".to_owned(),
            ArgValue::Var(v) => format!("${v}"),
            ArgValue::Call(name, _) => format!("{name}()"),
            ArgValue::New(name, _, _) => format!("new {}()", name.simple()),
            ArgValue::Array(items) => render_array(items),
            ArgValue::Ternary { then_val, else_val, .. } => {
                format!("(… ? {} : {})", then_val.render(), else_val.render())
            }
            ArgValue::Closure(ClosureRef::FunctionName(n)) => format!("{}(...)", n.simple()),
            ArgValue::Closure(ClosureRef::Anonymous { .. }) => "Closure".to_owned(),
            ArgValue::PropFetch { var, prop } => format!("${var}->{prop}"),
            ArgValue::Clone(v) => format!("clone ${v}"),
            ArgValue::Coalesce(l, r) => format!("({} ?? {})", l.render(), r.render()),
            ArgValue::Concat(l, r) => format!("({} . {})", l.render(), r.render()),
            ArgValue::OffsetRead { base, key } => format!("{}[{}]", base.render(), key.render()),
            ArgValue::ClassConst(class, name) => format!("{}::{name}", class.render()),
            ArgValue::EnumCase(class, case) => format!("{class}::{case}"),
            ArgValue::Other => "<expr>".to_owned(),
        }
    }
}

/// Render an array literal compactly for a diagnostic message: `['a', 'b']`,
/// `['k' => 1]`, list-shaped arrays without keys, truncating with `…` after the
/// first five entries.
fn render_array(items: &[(ArrayKey, ArgValue)]) -> String {
    // Rendering is cosmetic — a message never carries a proof-layer premise — so
    // it takes the pinned rule unconditionally rather than threading the project
    // minor through `ArgValue::render()`'s config-free `&self` surface (ADR-0049
    // A12). A pre-8.3 project can therefore see a negative-key literal rendered
    // with 8.3+ positions; no diagnostic decision rides on it.
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

/// A **named argument** (`name: <expr>`) at a call site (ADR-0049 §6 arity). The
/// arity check needs only the parameter *name* it binds (matched case-sensitively
/// against the target's parameter names, as PHP does); the phpdoc declared-contract
/// lane also binds the argument's **value** to that parameter, so the lowered value
/// is retained alongside the span. A named argument makes the call
/// non-[`CallExpr::positional_only`]; the positional args that accompany it stay
/// in [`CallExpr::args`], so the two lists together describe the full binding.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NamedArg {
    /// The parameter name being bound, without the leading `$` (e.g. `b` in
    /// `f(b: 2)`). PHP parameter names are **case-sensitive** for named-argument
    /// binding (`f(A: 1)` on `function f($a)` is a fatal `Error`), so this is
    /// compared case-sensitively.
    pub name: String,
    /// The lowered argument value bound to the parameter (`2` in `f(b: 2)`), so the
    /// declared-contract lane can judge it against the target's `@param` envelope
    /// exactly as it judges a positional argument.
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
    /// `$var->prop->m()` — a **depth-1** property-fetch receiver (ADR-0052 §7). The
    /// receiver object is whatever the heap says `$var->prop` holds; only a bare
    /// variable object and a static property identifier are represented (a chain
    /// `$a->b->c->m()` or a dynamic name lowers to [`Callee::Dynamic`], never this).
    /// The method target is not resolved from it (the §7 scope is the null-receiver
    /// proof and the dump/receiver heap reads, not dispatch), so every method-
    /// resolution path treats it as unresolvable — exactly like `Dynamic` — while the
    /// `call.on-null` proof reads the heap property fact.
    Prop { var: String, prop: String },
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

impl StaticClass {
    /// Render the class portion for a diagnostic message (the simple name for an
    /// explicit reference, else the keyword).
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
    /// The **positional** arguments in source order (spread `...$x` unpacking
    /// excluded — see [`Self::has_spread`]). This is the full argument list when
    /// [`Self::positional_only`]; alongside [`Self::named_args`] it is the
    /// positional prefix of a mixed call (`f(1, b: 2)` → `args = [1]`,
    /// `named_args = [b]`).
    pub args: Vec<Arg>,
    /// The **named** arguments (`name: <expr>`) in source order (ADR-0049 §6
    /// arity). Empty for a purely positional call. Populated even though
    /// [`Self::positional_only`] is then `false`, so the arity check can bind
    /// named arguments to parameters.
    pub named_args: Vec<NamedArg>,
    /// `true` when the call carries **argument unpacking** (`...$args`) — the
    /// argument count is then unproven (the spread's cardinality is a runtime
    /// value), so the arity check stays silent. Also set for a **non-canonical**
    /// argument order (a positional argument after a named one — a PHP compile
    /// error, hence absent from valid corpus), which is likewise unanalyzable.
    pub has_spread: bool,
    /// `false` if the call used a named or spread (`...`) argument; the existing
    /// checks (positional argument mapping) skip such calls. Equivalent to
    /// `named_args.is_empty() && !has_spread` for a normally-lowered call — the
    /// **first-class-callable** shape (`f(...)`) is the one exception: it lowers
    /// to an arg-less non-positional call (`positional_only == false` with all
    /// three of `args` / `named_args` empty and `has_spread == false`), so it is
    /// never a call for arity purposes.
    pub positional_only: bool,
    pub span: Span,
    /// The **guard reading of each positional argument**, index-parallel with
    /// [`Self::args`] — `Some` only where the argument is a condition the
    /// [`CondExpr`] vocabulary models (`isset(…)`, `empty(…)`, their `!`/`&&`/
    /// `||` compositions, a constant-key comparison, a named call), `None`
    /// everywhere else.
    ///
    /// Why a *second* reading of the same arguments: [`ArgValue`] is a value
    /// lowering, and `isset($d['a'])` has no value it can express — it lowers to
    /// [`ArgValue::Other`], which is where a userland assertion helper's argument
    /// used to disappear. `Util_Assert::true(isset($d['a']));` is a guard the
    /// analysis can consume exactly as it consumes `assert(isset($d['a']))`
    /// (ADR-0058's tag lane), but only if the *condition* survives lowering; this
    /// field is where it survives. Populated purely syntactically — the lowering
    /// knows nothing about which callees carry `@phpstan-assert` tags — and read
    /// only by that consumer.
    ///
    /// **Empty when no argument has a guard reading** (the overwhelming case), so
    /// an ordinary call allocates nothing; index with [`Self::arg_cond`], which
    /// treats a short vector as all-`None`. It is deliberately NOT a condition
    /// the branch walk may evaluate as an `if`: a [`CondExpr::Call`] built here
    /// carries its real `reads`, but the walk never sees this field as a guard
    /// position.
    pub arg_conds: Vec<Option<CondExpr>>,
}

impl CallExpr {
    /// The guard reading of the positional argument at `pos` (see
    /// [`Self::arg_conds`]). `None` when the argument is not a modelled
    /// condition, when the index is out of range, and for every argument of a
    /// call whose arguments have no guard readings at all.
    #[must_use]
    pub fn arg_cond(&self, pos: usize) -> Option<&CondExpr> {
        self.arg_conds.get(pos)?.as_ref()
    }
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
    /// `$var[<literal>]` — a **constant-key projection**, depth exactly one
    /// (ADR-0062 A-G4's v1 scope: binding base, constant key). Carried so a
    /// tagged-union guard (`$s['kind'] === 'circle'`, `match ($s['kind'])`,
    /// `switch ($s['kind'])`) can subtract the base's array arms by the field's
    /// `admits` verdict.
    ///
    /// For every *other* consumer this variant behaves exactly as
    /// [`Self::Other`] did before it existed — it decides no verdict and
    /// contributes no value-lane refinement. Only the shape-narrowing pass reads
    /// it, which is what keeps this a purely additive lowering change.
    Offset { var: String, key: Box<ArgValue> },
    /// A bare **global-constant fetch** (`PHP_VERSION_ID`, `SOME_CONST`), carried
    /// as the reference was written (issue #29). Lowered so the version-guard
    /// fold can recognize the engine's `PHP_VERSION_ID` and decide the branch
    /// against the resolved target range; for every other consumer this variant
    /// behaves exactly as [`Self::Other`] did before it existed — it decides no
    /// verdict and contributes no refinement, which is what keeps this a purely
    /// additive lowering change.
    Const(NameRef),
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
    /// A resolvable **call in guard position** (`if (isFoo($x))`). Retained (not
    /// opaqued) so the inference layer can (a) consume the callee's
    /// `@phpstan-assert-if-true`/`-if-false` envelopes on the matching branch
    /// (ADR-0052 §5, at the `Asserted` stratum) and (b) fold a recognized existence
    /// predicate to a real Yes/No/Maybe verdict (`method_exists`/`function_exists`/
    /// `class_exists` …, ADR-0049 §4 / N3): the env-threaded short-circuit evaluation
    /// and the foldable-predicate verdicts landed with N3, so a guard call is no
    /// longer uniformly `Maybe`. Other (unrecognized) guard calls still evaluate to
    /// `Maybe`, and `reads` (identical to what the equivalent [`Self::Opaque`] carried)
    /// invalidates its variables on the excluded path exactly as before.
    Call { call: Box<CallExpr>, reads: Vec<String> },
    /// `isset($var[<literal>])` — a **key-presence guard**, depth exactly one
    /// (ADR-0062 S4). PHP's `isset` is true when the key exists *and* its value
    /// is not null, which is the distinction the narrowing consumes: the true
    /// branch promotes presence and strips `null` from the value slot.
    ///
    /// Only this exact form is lowered. `isset($x)` on a bare variable and an
    /// `isset` over a property/dynamic key keep their pre-S4 [`Self::Opaque`]
    /// lowering, so no other lane's behavior moves. `empty($x[<literal>])` —
    /// the same depth-one scope — lowers to `!isset(…) || !…` in terms of this
    /// variant (PHP's own definition of the construct); every other `empty`
    /// form stays `Opaque`.
    /// A multi-argument `isset($a['x'], $b['y'])` — a conjunction by PHP
    /// semantics — lowers to an [`Self::And`] chain of these, but only when
    /// *every* operand fits the form; otherwise the whole construct stays
    /// `Opaque`.
    Isset { var: String, key: Box<ArgValue> },
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
    /// `assert(<expr>);` — a statement-position `assert` call whose argument is a
    /// condition (ADR-0052 §5). `cond` is the lowered guard; the walk applies its
    /// `then_refinements` to the fall-through env at the **`Verified`** stratum,
    /// unconditionally (the 2026-07-25 owner ruling reads `assert($expr)` as a
    /// throw-guard `if (!$expr) throw` and never consults `zend.assertions`). Only a
    /// bare `assert($expr)` (or `assert($expr, $description)`) with a lowerable
    /// condition reaches here; anything else stays a plain [`StmtKind::Call`].
    Assert { cond: CondExpr },
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
    /// `$var[<lit>] = <rvalue>;` / `$var[<lit>][<lit>] = <rvalue>;` — a
    /// **constant-key offset write** (ADR-0062 A-G8's invalidation table).
    ///
    /// This is a [`Self::Barrier`] carrying one extra piece of information. The
    /// walk still forgets the whole env and store exactly as a barrier does —
    /// an array write can alias anything the lowering cannot bound — and then
    /// re-establishes *only* the base binding's array shape with the key
    /// promoted. That containment is deliberate: it means the S4 write rule can
    /// move the shape lane and nothing else.
    ///
    /// `keys` has one or two entries (depth 1, plus the autovivification case
    /// A-G8 names); `$x[] = v` (append), a dynamic key, and a compound operator
    /// (`+=`, `.=`) all stay a plain `Barrier`.
    OffsetWrite { base: String, keys: Vec<ArgValue>, value: ArgValue },
    /// `unset($var[<lit>]);` — a **constant-key offset unset** (A-G8). Same
    /// containment as [`Self::OffsetWrite`]: barrier semantics plus a
    /// `mark_absent` on the base's shape. A multi-target `unset`, a dynamic key,
    /// and `unset($var)` itself all stay a plain `Barrier`.
    OffsetUnset { base: String, key: ArgValue },
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

/// A construct on the ADR-0001 whole-scope give-up list: code the analyzer parses
/// and then declines to reason about (ADR-0046 §1 "scope havoc"). Each variant is a
/// *reason* [`Scope::poisoned`] is set, and the set of variants is the poison
/// predicate's own vocabulary rather than a description of it: `scan_opaque` is the
/// single walk behind both the predicate and this inventory, so a construct added to
/// the give-up list cannot fail to appear in what `steins doctor` reports. A
/// hand-maintained parallel list would drift from the real behaviour, which is
/// exactly the silence this inventory exists to measure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    /// `use (&$x)` — a closure captures a local by reference. Poisons the enclosing
    /// scope *and* the closure's own scope (ADR-0033).
    ByRefCapture,
}

impl OpaqueConstruct {
    /// Every variant, in report order (the order `steins doctor` prints them). The
    /// array is hand-maintained — adding a variant without extending it compiles
    /// fine and silently drops the kind from the report — so a workspace test pins
    /// the length and the distinctness of the labels.
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

    /// The construct's label as a posture report spells it — PHP's own spelling
    /// where there is one, because the reader is looking for it in their source.
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

/// One give-up-list construct, where it stands. Collected per scope (see
/// [`Scope::opaque`]) rather than file-wide, because the fact it explains — "no
/// local is known here" — is a *scope* fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OpaqueSite {
    pub construct: OpaqueConstruct,
    /// The construct's source span (the outermost construct when they nest — the
    /// predicate stops there too).
    pub span: Span,
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
    /// Every give-up-list construct found in this scope, in source order — the
    /// *reasons* [`Self::poisoned`] holds. `poisoned == !opaque.is_empty()` by
    /// construction (both come from one `scan_opaque` walk), so the inventory can
    /// never drift from the predicate. Read by `steins doctor`'s coverage posture
    /// (ADR-0054 §9.2); the checker reads only [`Self::poisoned`].
    pub opaque: Vec<OpaqueSite>,
    pub stmts: Vec<Stmt>,
    /// Every instance/static method call in this scope's body, **comprehensively**
    /// (including calls nested inside sub-expressions the linear trace drops to
    /// [`ArgValue::Other`]), in source order, and NOT descending into nested
    /// function/closure/class bodies (those are their own scopes). Unlike
    /// [`Self::stmts`] — which captures only statement-position calls — this is the
    /// sound caller-enumeration surface the method-transform reverse sweep needs
    /// (ADR-0043 §6): a candidate method is safe to rewrite only when *every* call
    /// that could reach it is accounted for, so a nested `$this->m($bad)` must be
    /// visible here even though the trace never modeled it. Constructor (`new`)
    /// calls are omitted — the constructor is magic and never a transform
    /// candidate. Empty when the body has no method calls.
    pub method_calls: Vec<CallExpr>,
    /// Parameters of a closure/arrow scope ([`ScopeOwner::Closure`]) — a closure
    /// has no [`FunctionDecl`] to look them up on, so binding descent and native
    /// parameter seeding read them here. Empty for function/method/top-level
    /// scopes (which resolve params via [`Self::owner`]).
    pub params: Vec<Param>,
    /// Declared native return type of a closure/arrow scope
    /// ([`ScopeOwner::Closure`]) — a closure has no [`FunctionDecl`] carrying it,
    /// so the callable-signature variance check (issue #11) reads the closure's
    /// `: R` here. `None` for a closure with no/unrepresentable return hint and
    /// for every non-closure scope.
    pub ret_ty: Option<NativeType>,
    /// Effect-origin candidates of a closure/arrow body ([`ScopeOwner::Closure`]),
    /// so a closure can be an effect node in the fixpoint (ADR-0033 point 3).
    /// Empty for non-closure scopes (their origins live on the decl).
    pub effect_origins: Vec<EffectOrigin>,
    /// Throw-origin candidates of a closure/arrow body ([`ScopeOwner::Closure`]),
    /// the throw-fixpoint analogue of [`Self::effect_origins`].
    pub throw_origins: Vec<ThrowOrigin>,
    /// `true` when a closure/arrow scope ([`ScopeOwner::Closure`]) was declared
    /// with the `static` keyword (`static function () {}`, `static fn () => …`) —
    /// the closure can never be bound to an object and so can never touch `$this`.
    /// This is a **syntactic** fact, written in the source, which is what makes the
    /// `static-closure` family's binding obligation a mechanical check rather than
    /// an inference (ADR-0063 §2 decision 4). Always `false` for function, method
    /// and top-level scopes (the keyword has no meaning there).
    pub is_static: bool,
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

/// A statically-judgeable form of an `include`/`require` path argument
/// (ADR-0046 §2). Only the decidable shapes are represented; every other
/// expression is [`IncludePath::Unproven`] — the sound default, since a path a
/// modular tool cannot prove could pull in out-of-universe code (compiled
/// template caches) that calls any function with no visible call site.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum IncludePath {
    /// A fully-proven literal path (`'inc/util.php'`, or a literal-only
    /// concatenation `'a' . 'b'`). Resolved against the analyzed universe at
    /// obstacle time (relative → against the including file's directory).
    Literal(String),
    /// `__DIR__ . '<suffix>'` — a directory-relative literal. The suffix is the
    /// proven text after `__DIR__`; it resolves against the including file's own
    /// directory. Covers the common project-relative include idiom.
    DirRelative(String),
    /// A path that is not statically proven (a variable, a call, a non-literal
    /// concatenation): a caller-enumeration obstacle.
    Unproven,
}

/// The kind of a dynamic-code construct that can invisibly reach code the
/// call-site sweep cannot see (ADR-0046 §2, "universe havoc").
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DynamismKind {
    /// `eval(<expr>)` — code as data; can call any function with no CST call site.
    Eval,
    /// `include`/`include_once`/`require`/`require_once <path>` — pulls in code;
    /// carries the lowered path so provenness / in-universe resolution is
    /// judgeable.
    Include(IncludePath),
    /// A `class_alias(...)` call with a **runtime-minted** name argument (ADR-0046
    /// §2, ADR-0049 §2) — a class name the reference scan cannot resolve. A
    /// `class_alias` whose two names are known at compile time (string literals, or
    /// the `X::class` constant — issue #36) instead contributes a
    /// [`ClassAliasEdge`] to the index (see [`SourceTree::class_alias_edges`]) and
    /// is *not* a dam site. The checker-side finding-breadth dam treats this as a
    /// dam site (S2+); the transform-side obstacle scan deliberately ignores it in
    /// S1 to stay byte-identical (ADR-0049 S1 groundwork).
    ClassAlias,
}

/// One dynamic-code construct in a file (ADR-0046 §2). Collected file-wide —
/// across every scope, including nested function bodies — and kept distinct from
/// the coarse per-scope [`Scope::poisoned`] *value*-havoc flag: this records
/// *invisible callers / out-of-universe code*, a different soundness hole.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DynamismSite {
    pub kind: DynamismKind,
    /// The construct's source span (its starting line is the vouching key).
    pub span: Span,
}

/// A **reflection-driven invocation** shape: code that reaches a function or method
/// through a value rather than a call site, so the call-site sweep never sees the
/// target (issue #30).
///
/// Unlike [`OpaqueConstruct`], this is **not** derived from a predicate the analyzer
/// already acts on — nothing poisons a scope or dams a claim here, and the analyzer's
/// behaviour is unchanged by it. **The list is a guess until measured**: it is the
/// shapes a cross-analyzer survey named, recognized syntactically and therefore both
/// over- and under-inclusive (a `$queue->invoke()` on a plain domain object counts; a
/// reflective call reached through a helper does not). It is inventoried so the
/// guess has numbers attached to it and can be corrected against a corpus, which is
/// exactly what an unmeasured silence cannot offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReflectionKind {
    /// `$r->invoke(...)` / `$r->invokeArgs(...)` — any `->invoke*()` method call
    /// (`ReflectionMethod`, `ReflectionFunction`, `Closure::__invoke`).
    Invoke,
    /// `$r->newInstance(...)` / `->newInstanceArgs(...)` /
    /// `->newInstanceWithoutConstructor()` — any `->newInstance*()` method call.
    NewInstance,
    /// `Closure::bind($fn, $obj, <computed>)` — a rebind whose **scope** argument is
    /// not a literal class name (a string literal or `X::class`), so the bound
    /// private/protected surface is not statically known. A `Closure::bind` with a
    /// literal scope, and the instance form `$fn->bindTo(...)`, are deliberately not
    /// counted — the guess is kept narrow rather than padded.
    ClosureBindComputedScope,
    /// `func_get_args()` inside a declaration whose signature declares any type (a
    /// parameter hint or a return hint): the signature says one thing and the body
    /// reads another argument list entirely.
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

/// One reflection-driven invocation site, collected file-wide like
/// [`DynamismSite`]. Consumed only by `steins doctor`'s coverage posture — no
/// checker, dam, or transform decision reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReflectionSite {
    pub kind: ReflectionKind,
    pub span: Span,
}

/// A compile-time `class_alias('Target', 'Alias')` edge (ADR-0049 §2 / A2iii): both
/// arguments name a class at compile time — a string literal, or the `X::class`
/// constant resolved through the file's namespace context (issue #36) — so the
/// alias name resolves, for **existence** purposes, to the target declaration's
/// site. Folded into the project index
/// after every textual declaration, sharing the duplicate-decl ambiguity
/// discipline: an alias colliding with a textual declaration of the same FQN, or
/// two alias edges for one name, marks that FQN `Ambiguous` (existence present,
/// identity unresolved). FQNs are lowercase-normalized, leading `\` stripped — the
/// same key shape [`ClassDecl::fqn`] and the index use.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ClassAliasEdge {
    /// The alias name being minted (`class_alias`'s 2nd arg), lowercase FQN.
    pub alias_fqn: String,
    /// The existing class the alias points at (`class_alias`'s 1st arg), lowercase FQN.
    pub target_fqn: String,
    /// The `class_alias(...)` call's source span.
    pub span: Span,
}

/// An **anonymous class** declaration's inheritance edges (ADR-0049 A4 —
/// descendant-closure obstacle detection). Anonymous classes (`new class extends
/// Report {...}`) carry no FQN and never enter the class index, so a "completely
/// enumerated" descendant set of a union member would silently miss one that
/// `extends`/`implements` the member and defines the sought method. The
/// declared-receiver lane (S6) reads these **edge-only** lowerings (parent +
/// implements refs, no members) to taint closure: any anon-class edge that
/// resolves to — or is Unknown against — a union member forces `Unknown` (silence).
/// Refs resolve to FQNs project-wide at query time, like every other [`NameRef`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AnonClassEdge {
    /// The `extends` parent as written, if any.
    pub parent: Option<NameRef>,
    /// The interfaces the anonymous class `implements`.
    pub implements: Vec<NameRef>,
    /// The `new class` construct's source span.
    pub span: Span,
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
    /// Dynamic-code constructs (eval / include / require) found file-wide
    /// (ADR-0046 §2). Consumed by the transform engine's caller-enumeration
    /// obstacle detection; the checker never reads it (zero behavior change).
    dynamism: Vec<DynamismSite>,
    /// Compile-time `class_alias('Target', 'Alias')` edges found file-wide (ADR-0049
    /// §2). Folded into the project index for existence resolution; a `class_alias`
    /// naming a runtime-minted class is a [`DynamismKind::ClassAlias`] dam site in
    /// [`Self::dynamism`] instead. Carried but consumed by nothing in the S1
    /// groundwork slice.
    class_alias_edges: Vec<ClassAliasEdge>,
    /// Anonymous-class inheritance edges found file-wide (ADR-0049 A4). Read by the
    /// declared-receiver lane's descendant closure (S6) — an invisible descendant
    /// obstacle. Consumed by nothing else.
    anon_class_edges: Vec<AnonClassEdge>,
    /// Reflection-driven invocation sites found file-wide (issue #30). Report-only:
    /// consumed by `steins doctor`'s coverage posture and by nothing that decides a
    /// finding. See [`ReflectionKind`] — the list is a guess until measured.
    reflection: Vec<ReflectionSite>,
    /// Whether this file declares a userland constant named `PHP_VERSION_ID` —
    /// a `const PHP_VERSION_ID = …;` statement (in any namespace: the check is
    /// deliberately name-only and project-conservative) or a
    /// `define('…PHP_VERSION_ID', …)` with a literal name (issue #29). One such
    /// declaration anywhere disables the engine-constant version-guard fold for
    /// the whole project — the honest reading when constant resolution is
    /// otherwise unmodeled.
    php_version_id_declared: bool,
    /// Whether this file `use const`-imports something under the alias
    /// `PHP_VERSION_ID` (issue #29). File-scoped: an unqualified
    /// `PHP_VERSION_ID` in such a file names the import, not the engine
    /// constant, so the version-guard fold declines here. Constants are
    /// case-sensitive; the match is exact.
    php_version_id_aliased: bool,
    /// Class references at the four hard-error positions (ADR-0049 §5 / S4), read by
    /// the `class.undefined` per-file pass.
    hard_class_refs: Vec<NameRef>,
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

        // Object type hints (ADR-0043) resolve to their namespace FQN at lowering,
        // like declaration names; the resolver carries the file's ns contexts.
        let rc = RefResolver { contexts: &contexts, regions: &regions };

        let mut lowered = Lowered::default();
        walk(&Node::Program(program), &aliases, &docs, &rc, false, false, &mut lowered);

        let mut classes = lower_classes(&Node::Program(program), &aliases, &docs, &rc);
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
            let ctx = ctx_of(&contexts, &regions, c.span.start);
            c.fqn = fqn_of(ctx, &c.name);
            // ADR-0043 amendment: resolve any recorded `self`/`static`/`parent`
            // return keyword to its bound and synthesize the method's `ret` as a
            // single-member `Instance` of that bound. `self`/`static` bind to the
            // enclosing class (the minimum-bound lemma); `parent` binds to the
            // resolved `extends` parent, skipping when the class has none. The
            // source-cased display renders the bound class in the diagnostic
            // ("should return BrokenBuilder"); the lowercased FQN is the is-a key.
            let self_display = if ctx.namespace.is_empty() {
                c.name.clone()
            } else {
                format!("{}\\{}", ctx.namespace, c.name)
            };
            // The source-cased, namespace-qualified FQN for diagnostic / dump
            // rendering (no leading `\`, matching PHPStan). Same construction the
            // `self`/`static` bound uses below.
            c.display = self_display.clone();
            let self_fqn = c.fqn.clone();
            let parent_bound = c.parent.as_ref().map(|p| {
                let display = resolve_class_ref(ctx_of(&contexts, &regions, p.offset), p);
                (display.to_ascii_lowercase(), display)
            });
            for m in &mut c.methods {
                let Some(kw) = m.ret_bound_keyword else { continue };
                let bound = match kw.kind {
                    RetBoundKind::SelfKw | RetBoundKind::Static => {
                        Some((self_fqn.clone(), self_display.clone()))
                    }
                    RetBoundKind::Parent => parent_bound.clone(),
                };
                if let Some((fqn, display)) = bound {
                    m.ret = Some(NativeType {
                        members: vec![TypeMember::Instance { fqn, display }],
                        nullable: kw.nullable,
                    });
                }
            }
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
            dynamism: lowered.dynamism,
            class_alias_edges: lowered.class_alias_edges,
            anon_class_edges: lowered.anon_class_edges,
            reflection: lowered.reflection,
            php_version_id_declared: lowered.php_version_id_declared,
            php_version_id_aliased: lowered.php_version_id_aliased,
            hard_class_refs: lowered.hard_class_refs,
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
        resolve_class_ref(self.ctx_at(r.offset), r)
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

    /// The dynamic-code constructs (eval / include / require) found file-wide
    /// (ADR-0046 §2). Distinct from the coarse per-scope [`Scope::poisoned`]
    /// value-havoc flag: these are the caller-enumeration obstacles the transform
    /// engine consults before claiming "all callers proven".
    #[must_use]
    pub fn dynamism_sites(&self) -> &[DynamismSite] {
        &self.dynamism
    }

    /// The compile-time `class_alias('Target', 'Alias')` edges found file-wide
    /// (ADR-0049 §2) — both names given as string literals or as `X::class`. The
    /// project index folds these in for existence resolution; a `class_alias`
    /// naming a runtime-minted class is a [`DynamismKind::ClassAlias`] dam site in
    /// [`Self::dynamism_sites`] instead.
    #[must_use]
    pub fn class_alias_edges(&self) -> &[ClassAliasEdge] {
        &self.class_alias_edges
    }

    /// The anonymous-class inheritance edges found file-wide (ADR-0049 A4). Read by
    /// the declared-receiver lane's descendant closure (S6) to detect an invisible
    /// descendant of a union member (an anon class is never in the class index).
    /// Class references at the four hard-error positions — `new X`, `X::m()`,
    /// `X::CONST`, `X::$prop` (ADR-0049 §5 / S4). Consumed by the `class.undefined`
    /// per-file pass; `self`/`static`/`parent`, dynamic classes, and `X::class` are
    /// excluded at collection.
    #[must_use]
    pub fn hard_class_refs(&self) -> &[NameRef] {
        &self.hard_class_refs
    }

    #[must_use]
    pub fn anonymous_class_edges(&self) -> &[AnonClassEdge] {
        &self.anon_class_edges
    }

    /// The reflection-driven invocation sites found file-wide (issue #30). These
    /// poison no scope and dam no claim — they are inventoried so a quiet run can
    /// say what it declined to follow. The recognizer is an admitted guess; see
    /// [`ReflectionKind`].
    #[must_use]
    pub fn reflection_sites(&self) -> &[ReflectionSite] {
        &self.reflection
    }

    /// Whether the file contains any `eval(...)` construct.
    #[must_use]
    pub fn contains_eval(&self) -> bool {
        self.dynamism.iter().any(|d| matches!(d.kind, DynamismKind::Eval))
    }

    /// The recovered parse errors.
    #[must_use]
    pub fn parse_errors(&self) -> &[ParseError] {
        &self.parse_errors
    }

    /// Whether this file declares a userland constant named `PHP_VERSION_ID`
    /// (issue #29) — see the field docs for the project-wide consequence.
    #[must_use]
    pub fn php_version_id_declared(&self) -> bool {
        self.php_version_id_declared
    }

    /// Whether this file `use const`-imports the alias `PHP_VERSION_ID`
    /// (issue #29) — file-scoped; an unqualified reference here is the import.
    #[must_use]
    pub fn php_version_id_aliased(&self) -> bool {
        self.php_version_id_aliased
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
    dynamism: Vec<DynamismSite>,
    class_alias_edges: Vec<ClassAliasEdge>,
    anon_class_edges: Vec<AnonClassEdge>,
    /// Reflection-driven invocation sites (issue #30) — report-only.
    reflection: Vec<ReflectionSite>,
    /// Issue #29: see [`SourceTree::php_version_id_declared`].
    php_version_id_declared: bool,
    /// Issue #29: see [`SourceTree::php_version_id_aliased`].
    php_version_id_aliased: bool,
    /// Class references at the four **hard-error positions** (ADR-0049 §5 / S4):
    /// `new X`, `X::m()`, `X::CONST`, `X::$prop`. Explicit named classes only —
    /// `self`/`static`/`parent`, dynamic class exprs, and the `X::class` magic
    /// constant (a plain string since 8.0, never an error) are excluded at
    /// collection, so the collection IS exactly the verified finding-position set
    /// (`instanceof`/`catch`/type-decls are other node kinds, never collected here).
    hard_class_refs: Vec<NameRef>,
}

fn walk(
    node: &Node<'_, '_>,
    aliases: &SteinsAttrAliases,
    docs: &DocIndex,
    rc: &RefResolver,
    conditional: bool,
    typed_sig: bool,
    out: &mut Lowered,
) {
    match node {
        Node::Function(f) => out.functions.push(lower_function(f, aliases, docs, rc, conditional)),
        Node::FunctionCall(c) => {
            // `class_alias(...)` (ADR-0049 §2): a call whose two names are known at
            // compile time (string literals, or `X::class` — issue #36) mints an index
            // alias edge; a runtime-minted name makes it a dam site instead. Recognized
            // here so both facts are collected file-wide (like the dynamism set),
            // before the call itself is lowered. `rc` resolves the `X::class` spelling
            // against the enclosing namespace context, exactly as PHP does.
            classify_class_alias(c, rc, out);
            // `func_get_args()` under a typed signature (issue #30, report-only):
            // the declaration announces an argument shape the body then bypasses.
            if typed_sig
                && let Expression::Identifier(id) = c.function
                && bytes_to_string(id.last_segment()).eq_ignore_ascii_case("func_get_args")
            {
                out.reflection.push(ReflectionSite {
                    kind: ReflectionKind::FuncGetArgsInTypedSignature,
                    span: to_span(c.span()),
                });
            }
            // `define('…PHP_VERSION_ID', …)` with a literal name (issue #29): a
            // userland constant that could shadow the engine's version id in some
            // namespace's fallback resolution. Name-only, deliberately over-broad
            // (any namespace prefix counts) — one hit disables the version-guard
            // fold project-wide. A define with a computed name is not scanned;
            // that residue is recorded in the fold's documentation.
            if let Expression::Identifier(id) = c.function
                && bytes_to_string(id.last_segment()).eq_ignore_ascii_case("define")
                && let Some(first) = c.argument_list.arguments.iter().next()
                && let Expression::Literal(Literal::String(ls)) = first.value().unparenthesized()
                && ls.value.is_some_and(|bytes| bytes_to_string(bytes).ends_with("PHP_VERSION_ID"))
            {
                out.php_version_id_declared = true;
            }
            out.calls.push(lower_call(c));
        }
        // `const PHP_VERSION_ID = …;` (issue #29): a userland twin of the engine
        // constant. Name-only and namespace-blind on purpose — the conservative
        // reading disables the version-guard fold project-wide.
        Node::Constant(con) => {
            if con.items.iter().any(|i| bytes_to_string(i.name.value) == "PHP_VERSION_ID") {
                out.php_version_id_declared = true;
            }
        }
        // `use const … as PHP_VERSION_ID` / `use const …\PHP_VERSION_ID` (issue
        // #29): an unqualified `PHP_VERSION_ID` in this FILE names the import,
        // not the engine constant. Constant names are case-sensitive; the match
        // is exact. Const imports are otherwise unlowered (out of scope), so
        // this flag is the only thing read from them.
        Node::Use(u) => {
            if use_binds_php_version_id(u) {
                out.php_version_id_aliased = true;
            }
        }
        // Reflection-driven invocation through a method name (issue #30,
        // report-only): recognized by the method name alone, which is why the
        // inventory is documented as a guess — see [`ReflectionKind`].
        Node::MethodCall(mc) => push_reflection_method(&mc.method, to_span(mc.span()), out),
        Node::NullSafeMethodCall(mc) => push_reflection_method(&mc.method, to_span(mc.span()), out),
        // Anonymous class (`new class extends P implements I {...}`, ADR-0049 A4):
        // edge-only lowering — its inheritance refs, no members and no FQN. A
        // descendant-closure walk (S6) reads these to taint closure when one could
        // extend/implement a union member.
        Node::AnonymousClass(ac) => {
            out.anon_class_edges.push(AnonClassEdge {
                parent: ac.extends.as_ref().and_then(|e| e.types.iter().next()).map(name_ref),
                implements: ac
                    .implements
                    .as_ref()
                    .map(|i| i.types.iter().map(name_ref).collect())
                    .unwrap_or_default(),
                span: to_span(ac.span()),
            });
        }
        // The four hard-error class-reference positions (ADR-0049 §5 / S4). Each
        // collects only an explicitly-named class (`trace_static_class` /
        // `instantiation_class` return `None`/non-`Named` for self/static/parent and
        // dynamic class exprs), so `class.undefined` never fires on those forms.
        Node::Instantiation(inst) => {
            if let Some(r) = instantiation_class(inst) {
                out.hard_class_refs.push(r);
            }
        }
        Node::StaticMethodCall(sc) => {
            if let Some(StaticClass::Named(r)) = trace_static_class(sc.class) {
                if closure_bind_computed_scope(&r, sc) {
                    out.reflection.push(ReflectionSite {
                        kind: ReflectionKind::ClosureBindComputedScope,
                        span: to_span(sc.span()),
                    });
                }
                out.hard_class_refs.push(r);
            }
        }
        Node::ClassConstantAccess(cc) => {
            // `X::class` is a plain string since PHP 8.0 — never a hard-error site.
            let is_class_const =
                class_const_name(&cc.constant).is_some_and(|n| n.eq_ignore_ascii_case("class"));
            if !is_class_const
                && let Some(StaticClass::Named(r)) = trace_static_class(cc.class)
            {
                out.hard_class_refs.push(r);
            }
        }
        Node::StaticPropertyAccess(sp) => {
            if let Some(StaticClass::Named(r)) = trace_static_class(sp.class) {
                out.hard_class_refs.push(r);
            }
        }
        Node::DeclareItem(d) if is_strict_types_one(d) => out.strict_types = true,
        // Dynamic-code constructs (ADR-0046 §2). Collected file-wide (the walk
        // descends into every scope), not per-scope like the poison flag.
        Node::EvalConstruct(ec) => {
            out.dynamism.push(DynamismSite { kind: DynamismKind::Eval, span: to_span(ec.span()) });
        }
        Node::IncludeConstruct(ic) => out.dynamism.push(DynamismSite {
            kind: DynamismKind::Include(lower_include_path(ic.value)),
            span: to_span(ic.span()),
        }),
        Node::IncludeOnceConstruct(ic) => out.dynamism.push(DynamismSite {
            kind: DynamismKind::Include(lower_include_path(ic.value)),
            span: to_span(ic.span()),
        }),
        Node::RequireConstruct(rq) => out.dynamism.push(DynamismSite {
            kind: DynamismKind::Include(lower_include_path(rq.value)),
            span: to_span(rq.span()),
        }),
        Node::RequireOnceConstruct(rq) => out.dynamism.push(DynamismSite {
            kind: DynamismKind::Include(lower_include_path(rq.value)),
            span: to_span(rq.span()),
        }),
        _ => {}
    }
    // A function reached only through the program root / a namespace is
    // unconditional (ADR-0049 A2i); passing through anything else (an `if`, a
    // function/method body, a bare block) makes nested declarations conditional —
    // the same transparency rule the class conditional flag uses.
    let child_conditional = conditional || !is_decl_transparent(node);
    // The typed-signature flag is a property of the *nearest enclosing*
    // function-like, so every function-like node recomputes it for its subtree
    // (a nested untyped closure inside a typed method is untyped).
    let child_typed = match node {
        Node::Function(f) => signature_is_typed(&f.parameter_list, f.return_type_hint.as_ref()),
        Node::Method(m) => signature_is_typed(&m.parameter_list, m.return_type_hint.as_ref()),
        Node::Closure(c) => signature_is_typed(&c.parameter_list, c.return_type_hint.as_ref()),
        Node::ArrowFunction(a) => signature_is_typed(&a.parameter_list, a.return_type_hint.as_ref()),
        _ => typed_sig,
    };
    for child in node.children() {
        walk(&child, aliases, docs, rc, child_conditional, child_typed, out);
    }
}

/// Whether a function-like signature declares **any** native type — one parameter
/// hint or a return hint. Deliberately "any", not "all": the point of the
/// `func_get_args()` inventory line is a signature that *claims* a shape while the
/// body reads a different argument list, and one hint is already such a claim.
fn signature_is_typed(
    params: &mago_syntax::cst::FunctionLikeParameterList<'_>,
    ret: Option<&mago_syntax::cst::FunctionLikeReturnTypeHint<'_>>,
) -> bool {
    ret.is_some() || params.parameters.iter().any(|p| p.hint.is_some())
}

/// Record an `->invoke*()` / `->newInstance*()` reflection site (issue #30). Matched
/// on the method name only: no receiver type is required (and none is knowable at
/// lowering time), so `$q->invokeHandler()` on a plain domain object counts too —
/// the over-inclusion the [`ReflectionKind`] docs own up to. `__invoke` is not
/// matched (the prefix is `invoke`, not `_`).
fn push_reflection_method(selector: &ClassLikeMemberSelector<'_>, span: Span, out: &mut Lowered) {
    let Some(name) = method_name_of(selector) else { return };
    // `get(..n)`, never `[..n]`: PHP identifiers can be multibyte (a method named
    // in Japanese is real code — ec-cube), and a byte-length check does not make
    // byte index `n` a char boundary. A prefix slice that lands mid-character is
    // simply not the ASCII prefix `invoke`/`newInstance`.
    let has_prefix =
        |p: &str| name.get(..p.len()).is_some_and(|head| head.eq_ignore_ascii_case(p));
    let kind = if has_prefix("invoke") {
        ReflectionKind::Invoke
    } else if has_prefix("newInstance") {
        ReflectionKind::NewInstance
    } else {
        return;
    };
    out.reflection.push(ReflectionSite { kind, span });
}

/// Whether a static call is `Closure::bind(...)` whose **scope** argument (the
/// third positional one) is computed — anything but a string literal, `X::class`, or
/// `null`. A computed scope means the private/protected surface the rebound closure
/// may reach is not statically known. Named arguments (`newScope:`) and the instance
/// form `$fn->bindTo(...)` are not matched: the guess stays narrow (issue #30).
fn closure_bind_computed_scope(class: &NameRef, sc: &mago_syntax::cst::StaticMethodCall<'_>) -> bool {
    if !class.simple().eq_ignore_ascii_case("Closure")
        || !method_name_of(&sc.method).is_some_and(|m| m.eq_ignore_ascii_case("bind"))
    {
        return false;
    }
    let scope = sc
        .argument_list
        .arguments
        .iter()
        .filter_map(|a| match a {
            Argument::Positional(p) if p.ellipsis.is_none() => Some(p.value),
            _ => None,
        })
        .nth(2);
    scope.is_some_and(|e| !is_literal_class_name(e))
}

/// Whether an expression names a class statically for `Closure::bind`'s scope
/// argument: a string literal, `X::class`, or the `null` unbind.
fn is_literal_class_name(expr: &Expression<'_>) -> bool {
    match expr.unparenthesized() {
        Expression::Literal(Literal::String(_) | Literal::Null(_)) => true,
        Expression::Access(Access::ClassConstant(cc)) => {
            class_const_name(&cc.constant).is_some_and(|n| n.eq_ignore_ascii_case("class"))
        }
        _ => false,
    }
}

/// The proven prefix of a concatenation chain: a literal string, a
/// `__DIR__`-anchored directory-relative literal, or unproven.
enum ConcatVal {
    Str(String),
    DirRel(String),
    Unproven,
}

/// Lower an `include`/`require` path expression to a judgeable [`IncludePath`]
/// (ADR-0046 §2). Recognizes string literals, literal-only concatenations, and
/// the `__DIR__ . '<suffix>'` project-relative idiom; every other shape is
/// [`IncludePath::Unproven`] (the sound default — an unprovable path is an
/// obstacle).
fn lower_include_path(expr: &Expression<'_>) -> IncludePath {
    match lower_concat(expr) {
        ConcatVal::Str(s) => IncludePath::Literal(s),
        ConcatVal::DirRel(s) => IncludePath::DirRelative(s),
        ConcatVal::Unproven => IncludePath::Unproven,
    }
}

/// Fold a string-concatenation subtree into its proven value. `__DIR__` anchors a
/// directory-relative result; a literal-only chain folds to a plain literal;
/// anything else (a variable, a call, a second `__DIR__`) is unproven.
fn lower_concat(expr: &Expression<'_>) -> ConcatVal {
    match expr.unparenthesized() {
        Expression::Literal(Literal::String(ls)) => {
            ls.value.map_or(ConcatVal::Unproven, |bytes| ConcatVal::Str(bytes_to_string(bytes)))
        }
        Expression::MagicConstant(MagicConstant::Directory(_)) => ConcatVal::DirRel(String::new()),
        Expression::Binary(b) if b.operator.is_concatenation() => {
            match (lower_concat(b.lhs), lower_concat(b.rhs)) {
                (ConcatVal::Str(l), ConcatVal::Str(r)) => ConcatVal::Str(format!("{l}{r}")),
                (ConcatVal::DirRel(l), ConcatVal::Str(r)) => ConcatVal::DirRel(format!("{l}{r}")),
                _ => ConcatVal::Unproven,
            }
        }
        _ => ConcatVal::Unproven,
    }
}

/// Classify a `class_alias(...)` call (ADR-0049 §2): two **compile-time** class-name
/// arguments mint an index [`ClassAliasEdge`] (existence resolution); an argument
/// whose name only exists at run time — a variable, a call, a computed string —
/// makes it a [`DynamismKind::ClassAlias`] dam site. Only the global `class_alias`
/// (unqualified, so subject to PHP's global function fallback, or fully-qualified
/// `\class_alias`) is recognized; a namespaced `Foo\class_alias` is a different
/// symbol. Called for every `FunctionCall` node file-wide; a non-`class_alias`
/// callee is a no-op.
///
/// The compile-time set is decided by [`lower_alias_name`], which is where the
/// `X::class` form joins the string literal (issue #36) — that argument shape is
/// resolved by the *compiler*, so treating it as a runtime mint dammed the whole
/// universe over a constant.
fn classify_class_alias(c: &FunctionCall<'_>, rc: &RefResolver, out: &mut Lowered) {
    let Expression::Identifier(id) = c.function else { return };
    if !matches!(id, Identifier::Local(_) | Identifier::FullyQualified(_)) {
        return;
    }
    if !bytes_to_string(id.last_segment()).eq_ignore_ascii_case("class_alias") {
        return;
    }
    let span = to_span(c.span());

    // The first two positional (non-spread) arguments must both name a class at
    // compile time for an edge; a named/spread argument or a runtime-minted name
    // makes it a dam site. Both are already normalized to the index key shape.
    let mut names: Vec<String> = Vec::new();
    let mut clean = true;
    for arg in c.argument_list.arguments.iter() {
        if names.len() >= 2 {
            break;
        }
        match arg {
            Argument::Positional(p) if p.ellipsis.is_none() => match lower_alias_name(p.value, rc) {
                Some(s) => names.push(s),
                None => {
                    clean = false;
                    break;
                }
            },
            _ => {
                clean = false;
                break;
            }
        }
    }

    if clean && names.len() == 2 {
        // `class_alias(string $class /* target */, string $alias)` — arg 0 is the
        // existing class, arg 1 the new name; the alias resolves to the target.
        out.class_alias_edges.push(ClassAliasEdge {
            alias_fqn: names[1].clone(),
            target_fqn: names[0].clone(),
            span,
        });
    } else {
        out.dynamism.push(DynamismSite { kind: DynamismKind::ClassAlias, span });
    }
}

/// Lower one `class_alias` argument to the normalized index-key FQN it names at
/// **compile time**, or `None` when the name is only known at run time (which dams
/// — ADR-0049 §2). Two shapes qualify, and they normalize *differently* on purpose:
///
/// - a **string literal** (including a literal-only concatenation): a runtime FQN,
///   spelled out in full. PHP does not resolve it against `use` imports or the
///   current namespace, so neither does [`normalize_alias_fqn`].
/// - **`X::class`** (issue #36): since PHP 8.0 this is a plain compile-time string —
///   no autoload, no runtime lookup, the named class need not even exist. It is
///   therefore *not* a runtime class-name mint and must not dam. Its spelling **is**
///   subject to ordinary class-name resolution, so it goes through the same
///   [`RefResolver`] every other class reference uses — `use` imports (plain,
///   aliased, and the grouped forms `use A\{B, C}` whose omission previously
///   mis-resolved names), the enclosing namespace, and the `namespace\X` relative
///   form — rather than being taken as spelled. Taking the raw spelling would key
///   the edge on a name no declaration ever carries.
///
/// Deliberately **not** widened past those two:
/// - `self::class` / `parent::class` are lexically knowable in principle, but this
///   walk is file-wide and carries no enclosing-class context; `static::class` is
///   late-static-bound and not knowable at the site at all. All three keep damming,
///   which is the sound direction (a dam only silences absence claims).
/// - a variable, a constant, a function call, or any concatenation touching one:
///   [`lower_concat`] folds only literals and `__DIR__`, so `Foo::class . $suffix`
///   and friends stay unproven and keep damming.
fn lower_alias_name(expr: &Expression<'_>, rc: &RefResolver) -> Option<String> {
    let expr = expr.unparenthesized();
    // `X::class` — an explicitly-named class only (`self`/`static`/`parent` and a
    // dynamic class expr are other `Expression` variants and fall through to the
    // literal path, which rejects them).
    if let Expression::Access(Access::ClassConstant(cc)) = expr
        && class_const_name(&cc.constant).is_some_and(|n| n.eq_ignore_ascii_case("class"))
    {
        return match cc.class {
            Expression::Identifier(id) => {
                Some(normalize_alias_fqn(&rc.class_display_fqn(&name_ref(id))))
            }
            _ => None,
        };
    }
    match lower_concat(expr) {
        ConcatVal::Str(s) => Some(normalize_alias_fqn(&s)),
        _ => None,
    }
}

/// Normalize a `class_alias` class name to the index key shape: trimmed, leading
/// `\` stripped, lowercased. Applied to an already-resolved name — a literal
/// argument is a runtime FQN (never resolved against `use` imports or the current
/// namespace), and an `X::class` argument was resolved by [`RefResolver`] before it
/// got here — so this step itself does no context lookup.
fn normalize_alias_fqn(s: &str) -> String {
    s.trim().trim_start_matches('\\').to_ascii_lowercase()
}

fn lower_function(
    f: &Function<'_>,
    aliases: &SteinsAttrAliases,
    docs: &DocIndex,
    rc: &RefResolver,
    conditional: bool,
) -> FunctionDecl {
    let mut effect_origins = Vec::new();
    let mut throw_origins = Vec::new();
    let cx = EffectScanCx::new(
        &f.parameter_list,
        collect_body_callables(f.body.statements.iter()),
        body_aliased(f.body.statements.iter()),
        receiver_writes(f.body.statements.iter()),
    );
    for s in f.body.statements.iter() {
        scan_effect_origins(&Node::Statement(s), &cx, &mut effect_origins);
        scan_throw_origins(&Node::Statement(s), &[], &[], &cx.locals, &mut throw_origins);
    }

    FunctionDecl {
        name: bytes_to_string(f.name.value),
        fqn: String::new(), // filled in `parse` from the enclosing namespace ctx
        params: lower_params(&f.parameter_list, rc),
        ret: f.return_type_hint.as_ref().and_then(|r| lower_hint(&r.hint, rc)),
        span: to_span(f.name.span()),
        effect_envelope: attrs_effect_envelope(&f.attribute_lists, aliases),
        effect_origins,
        throw_origins,
        docblock: docs.preceding(to_span(f.span()).start),
        docblock_span: docs.preceding_span(to_span(f.span()).start),
        conditional,
    }
}

/// Lower a parameter list to owned [`Param`]s (shared by functions and methods).
fn lower_params(list: &mago_syntax::cst::FunctionLikeParameterList<'_>, rc: &RefResolver) -> Vec<Param> {
    list.parameters
        .iter()
        .map(|p| Param {
            name: strip_dollar(bytes_to_string(p.variable.name)),
            ty: p.hint.as_ref().and_then(|h| lower_hint(h, rc)),
            variadic: p.is_variadic(),
            by_ref: p.is_reference(),
            has_null_default: p
                .default_value
                .as_ref()
                .is_some_and(|d| matches!(d.value.unparenthesized(), Expression::Literal(Literal::Null(_)))),
            has_default: p.default_value.is_some(),
            default: p
                .default_value
                .as_ref()
                .map(|d| lower_arg_value(d.value))
                .filter(|v| !matches!(v, ArgValue::Other)),
            span: to_span(p.span()),
        })
        .collect()
}

/// Lower every `class`, `interface`, `enum`, and `trait` declaration reachable
/// from `node` (ADR-0043 lowers enums; ADR-0049 §5 adds trait *names*). The
/// `conditional` flag (ADR-0049 A2i) starts `false` at the program root and turns
/// `true` for any declaration nested under a non-namespace/program node.
fn lower_classes(
    node: &Node<'_, '_>,
    aliases: &SteinsAttrAliases,
    docs: &DocIndex,
    rc: &RefResolver,
) -> Vec<ClassDecl> {
    let mut out = Vec::new();
    lower_classes_into(node, aliases, docs, rc, false, &mut out);
    out
}

fn lower_classes_into(
    node: &Node<'_, '_>,
    aliases: &SteinsAttrAliases,
    docs: &DocIndex,
    rc: &RefResolver,
    conditional: bool,
    out: &mut Vec<ClassDecl>,
) {
    match node {
        Node::Class(c) => out.push(lower_class(c, aliases, docs, rc, conditional)),
        Node::Interface(i) => out.push(lower_interface(i, aliases, docs, rc, conditional)),
        Node::Enum(e) => out.push(lower_enum(e, aliases, docs, rc, conditional)),
        Node::Trait(t) => out.push(lower_trait(t, conditional)),
        _ => {}
    }
    // A declaration reached only through a plain namespace/program node is
    // unconditional; passing through anything else (a function/method body, `if`,
    // `try`, loop, bare block) makes every declaration below it conditional.
    let child_conditional = conditional || !is_decl_transparent(node);
    for child in node.children() {
        lower_classes_into(&child, aliases, docs, rc, child_conditional, out);
    }
}

/// Whether descending through `node` keeps a declaration **unconditional**
/// (ADR-0049 A2i): only the program root and namespace nodes (and the `Statement`
/// enum wrapper that links them to declarations) are transparent. Every other node
/// — control flow, a function/method body, a bare block — marks nested
/// declarations conditional.
fn is_decl_transparent(node: &Node<'_, '_>) -> bool {
    matches!(
        node,
        Node::Program(_)
            | Node::Statement(_)
            | Node::Namespace(_)
            | Node::NamespaceBody(_)
            | Node::NamespaceImplicitBody(_)
    )
}

/// Lower a `trait` declaration to a name-only [`ClassDecl`] (ADR-0049 §5, C8/A2i).
/// A trait joins the class-*like* index as a name — the `class.undefined` closure
/// set is the class-like name set, traits included. V1 lowers **no members** (no
/// flattening), so the trait is inert for every existing check; it merely occupies
/// its FQN in the symbol/ambiguity table.
fn lower_trait(t: &mago_syntax::cst::Trait<'_>, conditional: bool) -> ClassDecl {
    ClassDecl {
        name: bytes_to_string(t.name.value),
        fqn: String::new(), // filled in `parse` from the enclosing namespace ctx
        display: String::new(),
        is_final: false,
        is_abstract: false,
        is_interface: false,
        is_enum: false,
        is_trait: true,
        conditional,
        enum_backing: None,
        enum_cases: Vec::new(),
        parent: None,
        implements: Vec::new(),
        methods: Vec::new(),
        properties: Vec::new(),
        consts: Vec::new(),
        uses_traits: false,
        // A trait lowers no members this slice, so class-level `@template` names on
        // it never reach a member docblock — carry `None` (nothing to shadow).
        docblock: None,
        span: to_span(t.name.span()),
    }
}

fn lower_class(c: &Class<'_>, aliases: &SteinsAttrAliases, docs: &DocIndex, rc: &RefResolver, conditional: bool) -> ClassDecl {
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
    let mut consts = Vec::new();
    let mut uses_traits = false;
    for member in c.members.iter() {
        match member {
            ClassLikeMember::Method(m) => {
                // A constructor's promoted params are properties too (ADR-0036).
                if bytes_to_string(m.name.value).eq_ignore_ascii_case("__construct") {
                    lower_promoted_params(m, rc, &mut properties);
                }
                methods.push(lower_method(m, aliases, docs, rc));
            }
            ClassLikeMember::Property(Property::Plain(p)) => {
                lower_plain_property(p, docs, rc, &mut properties);
            }
            // Hooked properties (`public $x { get => … }`) are virtual/computed —
            // not lowered this slice (out of object-state scope; never heap-tracked,
            // so no property check fires on them — the safe side).
            ClassLikeMember::Property(Property::Hooked(_)) => {}
            ClassLikeMember::Constant(k) => lower_class_consts(k, &mut consts),
            ClassLikeMember::TraitUse(_) => uses_traits = true,
            _ => {}
        }
    }

    ClassDecl {
        name: bytes_to_string(c.name.value),
        fqn: String::new(), // filled in `parse` from the enclosing namespace ctx
        display: String::new(),
        is_final: c.modifiers.iter().any(Modifier::is_final),
        is_abstract: c.modifiers.iter().any(Modifier::is_abstract),
        is_interface: false,
        is_enum: false,
        is_trait: false,
        conditional,
        enum_backing: None,
        enum_cases: Vec::new(),
        parent,
        implements,
        methods,
        properties,
        consts,
        uses_traits,
        // Class-level docblock (preceding the whole declaration incl. attributes/
        // modifiers, mirroring the function/method lookup) — read for `@template`
        // names that shadow same-named classes in member docblocks (issue #5).
        docblock: docs.preceding(to_span(c.span()).start),
        span: to_span(c.name.span()),
    }
}

/// Lower a `const NAME = <expr>[, …];` class-member declaration into `(name,
/// value)` pairs, keeping **only literal** initializers (ADR-0043 §2). A
/// non-literal value lowers to [`ArgValue::Other`] and is dropped, so a name's
/// absence means "no proven literal", never "no such constant".
fn lower_class_consts(k: &mago_syntax::cst::ClassLikeConstant<'_>, out: &mut Vec<(String, ArgValue)>) {
    for item in k.items.iter() {
        let v = lower_arg_value(item.value);
        if !matches!(v, ArgValue::Other) {
            out.push((bytes_to_string(item.name.value), v));
        }
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
fn lower_plain_property(p: &PlainProperty<'_>, docs: &DocIndex, rc: &RefResolver, out: &mut Vec<PropertyDecl>) {
    let readonly = p.modifiers.iter().any(Modifier::is_readonly);
    let is_static = p.modifiers.iter().any(Modifier::is_static);
    let visibility = visibility_of(&p.modifiers);
    let ty = p.hint.as_ref().and_then(|h| lower_hint(h, rc));
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
            hooked: false,
            docblock: docblock.clone(),
            span,
        });
    }
}

/// Lower a constructor's promoted parameters into [`PropertyDecl`]s (ADR-0036).
/// A parameter is promoted iff it carries a modifier (visibility / `readonly`).
fn lower_promoted_params(m: &Method<'_>, rc: &RefResolver, out: &mut Vec<PropertyDecl>) {
    for p in m.parameter_list.parameters.iter() {
        if !p.is_promoted_property() {
            continue;
        }
        let readonly = p.modifiers.iter().any(Modifier::is_readonly);
        let visibility = visibility_of(&p.modifiers);
        let ty = p.hint.as_ref().and_then(|h| lower_hint(h, rc));
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
            // A hook on a promoted param (`public int $n { set { … } }`, PHP 8.4) makes
            // every write/read go through arbitrary code — bind no fact (FP class 16).
            // `readonly` + any hook is a PHP fatal, so a hooked prop is never readonly.
            hooked: p.hooks.is_some(),
            docblock: None,
            span: to_span(p.span()),
        });
    }
}

/// Lower an `interface` declaration to a [`ClassDecl`] with `is_interface = true`
/// (ADR-0033 Liskov): its methods are abstract signatures carrying effect
/// envelopes and `@throws` docblocks. An interface's `extends` list (interfaces
/// can extend several) becomes `parent` (the first) plus `implements` (the rest).
fn lower_interface(i: &mago_syntax::cst::Interface<'_>, aliases: &SteinsAttrAliases, docs: &DocIndex, rc: &RefResolver, conditional: bool) -> ClassDecl {
    let mut extended: Vec<NameRef> =
        i.extends.as_ref().map(|e| e.types.iter().map(name_ref).collect()).unwrap_or_default();
    let parent = if extended.is_empty() { None } else { Some(extended.remove(0)) };

    let mut methods = Vec::new();
    let mut consts = Vec::new();
    for member in i.members.iter() {
        match member {
            ClassLikeMember::Method(m) => methods.push(lower_method(m, aliases, docs, rc)),
            ClassLikeMember::Constant(k) => lower_class_consts(k, &mut consts),
            _ => {}
        }
    }

    ClassDecl {
        name: bytes_to_string(i.name.value),
        fqn: String::new(),
        display: String::new(),
        is_final: false,
        is_abstract: false,
        is_interface: true,
        is_enum: false,
        is_trait: false,
        conditional,
        enum_backing: None,
        enum_cases: Vec::new(),
        parent,
        implements: extended,
        methods,
        properties: Vec::new(),
        consts,
        uses_traits: false,
        // Class-level docblock — `@template` names shadow same-named classes in the
        // interface's method docblocks (issue #5).
        docblock: docs.preceding(to_span(i.span()).start),
        span: to_span(i.name.span()),
    }
}

/// Lower an `enum` declaration to a [`ClassDecl`] with `is_enum = true`
/// (ADR-0043 object/method world). An enum is implicitly `final`, cannot extend,
/// and joins the class index like a class/interface so subtyping can reason about
/// it. Its `implements` list is recorded (the is-a oracle walks it, plus the
/// implicit `UnitEnum`/`BackedEnum` catalog tree); its cases + backing scalar are
/// recorded for value reasoning.
///
/// V1 deliberately does **not** analyze enum method bodies: [`methods`] is left
/// empty and no scope is built (see [`ClassDecl`]), so an enum body introduces no
/// new throw/effect/Liskov findings — the zero-behavior-change invariant of
/// stage 1. Deferred-with-design: enum methods land with the method-transform
/// stage that needs them.
fn lower_enum(e: &mago_syntax::cst::Enum<'_>, _aliases: &SteinsAttrAliases, _docs: &DocIndex, rc: &RefResolver, conditional: bool) -> ClassDecl {
    let implements: Vec<NameRef> = e
        .implements
        .as_ref()
        .map(|i| i.types.iter().map(name_ref).collect())
        .unwrap_or_default();

    // Backing scalar: only `int`/`string` are legal enum backings; anything else
    // (should not occur) records no backing.
    let enum_backing = e.backing_type_hint.as_ref().and_then(|b| match &b.hint {
        Hint::Integer(_) => Some(ScalarType::Int),
        Hint::String(_) => Some(ScalarType::String),
        _ => None,
    });

    let mut enum_cases = Vec::new();
    let mut consts = Vec::new();
    for member in e.members.iter() {
        match member {
            ClassLikeMember::EnumCase(case) => {
                let (name_id, value) = match &case.item {
                    mago_syntax::cst::EnumCaseItem::Unit(u) => (&u.name, None),
                    mago_syntax::cst::EnumCaseItem::Backed(b) => {
                        let v = lower_arg_value(b.value);
                        (&b.name, (!matches!(v, ArgValue::Other)).then_some(v))
                    }
                };
                enum_cases.push(EnumCaseDecl {
                    name: bytes_to_string(name_id.value),
                    value,
                    span: to_span(case.span()),
                });
            }
            ClassLikeMember::Constant(k) => lower_class_consts(k, &mut consts),
            _ => {}
        }
    }

    // `rc` is unused today (enum name hints are not resolved through it), but kept
    // in the signature for symmetry with the other class-like lowerers and for the
    // deferred method-lowering path.
    let _ = rc;

    ClassDecl {
        name: bytes_to_string(e.name.value),
        fqn: String::new(),
        display: String::new(),
        is_final: true, // enums are implicitly final in PHP
        is_abstract: false,
        is_interface: false,
        is_enum: true,
        is_trait: false,
        conditional,
        enum_backing,
        enum_cases,
        parent: None,
        implements,
        methods: Vec::new(),
        properties: Vec::new(),
        consts,
        uses_traits: false,
        // An enum lowers no method bodies this slice (see above), so a class-level
        // `@template` on it reaches no analyzed member — carry `None`.
        docblock: None,
        span: to_span(e.name.span()),
    }
}

fn lower_method(m: &Method<'_>, aliases: &SteinsAttrAliases, docs: &DocIndex, rc: &RefResolver) -> MethodDecl {
    let mut effect_origins = Vec::new();
    let mut throw_origins = Vec::new();
    if let MethodBody::Concrete(block) = &m.body {
        let cx = EffectScanCx::new(
            &m.parameter_list,
            collect_body_callables(block.statements.iter()),
            body_aliased(block.statements.iter()),
            receiver_writes(block.statements.iter()),
        );
        for s in block.statements.iter() {
            scan_effect_origins(&Node::Statement(s), &cx, &mut effect_origins);
            scan_throw_origins(&Node::Statement(s), &[], &[], &cx.locals, &mut throw_origins);
        }
    }

    let visibility = visibility_of(&m.modifiers);

    let name = bytes_to_string(m.name.value);
    let is_constructor = name.eq_ignore_ascii_case("__construct");

    MethodDecl {
        name,
        params: lower_params(&m.parameter_list, rc),
        ret: m.return_type_hint.as_ref().and_then(|r| lower_hint(&r.hint, rc)),
        ret_bound_keyword: m.return_type_hint.as_ref().and_then(|r| ret_bound_keyword(&r.hint)),
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
        docblock_span: docs.preceding_span(to_span(m.span()).start),
    }
}

/// Recognize a bare `self`/`static`/`parent` return hint — or the `?`-nullable of
/// one — and record its keyword shape (ADR-0043 amendment §2). Anything else
/// (a union containing the keyword, a plain class name, a scalar, `void`) returns
/// `None`, keeping the pre-amendment silence. This runs at method lowering, which
/// has no class context; the FQN-stamping pass resolves the kind to a bound.
fn ret_bound_keyword(hint: &Hint<'_>) -> Option<RetBoundKeyword> {
    match hint {
        Hint::Static(_) => Some(RetBoundKeyword { kind: RetBoundKind::Static, nullable: false }),
        Hint::Self_(_) => Some(RetBoundKeyword { kind: RetBoundKind::SelfKw, nullable: false }),
        Hint::Parent(_) => Some(RetBoundKeyword { kind: RetBoundKind::Parent, nullable: false }),
        // `?self` / `?static` / `?parent`: the nullable of a bare keyword. Any
        // other nullable inner shape falls through to `None` via the inner call.
        Hint::Nullable(n) => {
            let mut kw = ret_bound_keyword(n.hint)?;
            kw.nullable = true;
            Some(kw)
        }
        _ => None,
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
    /// `(span, text)` of each docblock, in source order. `span` is the full file
    /// span of the `/** … */` trivium; `text` is its exact source substring.
    blocks: Vec<(Span, String)>,
}

impl<'a> DocIndex<'a> {
    fn build(source: &'a str, program: &Program<'_>) -> Self {
        let blocks = program
            .trivia
            .iter()
            .filter(|t| matches!(t.kind, TriviaKind::DocBlockComment))
            .map(|t| (to_span(t.span), bytes_to_string(t.value)))
            .collect();
        Self { source, blocks }
    }

    /// The docblock immediately preceding `decl_start` (only whitespace between
    /// its end and `decl_start`), if any — as `(span, text)`.
    fn preceding_block(&self, decl_start: u32) -> Option<(Span, &String)> {
        let mut best: Option<(Span, &String)> = None;
        for (span, text) in &self.blocks {
            if span.end <= decl_start && best.is_none_or(|(bs, _)| span.end > bs.end) {
                best = Some((*span, text));
            }
        }
        let (span, text) = best?;
        let gap = self.source.get(span.end as usize..decl_start as usize)?;
        gap.chars().all(char::is_whitespace).then_some((span, text))
    }

    /// The text of the docblock immediately preceding `decl_start`, if any.
    fn preceding(&self, decl_start: u32) -> Option<String> {
        self.preceding_block(decl_start).map(|(_, text)| text.clone())
    }

    /// The file span of the docblock immediately preceding `decl_start`, if any.
    fn preceding_span(&self, decl_start: u32) -> Option<Span> {
        self.preceding_block(decl_start).map(|(span, _)| span)
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

/// The per-position lvalue-root classification of a named call's arguments
/// (ADR-0063 §2.3). `None` when a named or spread argument defeats positional
/// mapping — see [`EffectOrigin::Call`]'s `arg_targets`.
fn arg_targets_of_call(fc: &FunctionCall<'_>, cx: &EffectScanCx) -> Option<Vec<RefTarget>> {
    let mut targets = Vec::new();
    for arg in fc.argument_list.arguments.iter() {
        match arg {
            Argument::Positional(p) if p.ellipsis.is_none() => {
                targets.push(ref_target_of_arg(p.value, cx));
            }
            _ => return None,
        }
    }
    Some(targets)
}

/// Classify one argument expression's **lvalue root** ([`RefTarget`]).
///
/// Offsets are transparent: `sort($rows[3])` writes into `$rows`, so the root of
/// an `ArrayAccess` chain is what decides. Everything that is not a plain
/// variable root — a property fetch, a static property, a call result, `$$v` —
/// is [`RefTarget::Escaping`], the conservative answer.
fn ref_target_of_arg(expr: &Expression<'_>, cx: &EffectScanCx) -> RefTarget {
    let mut cur = expr.unparenthesized();
    // Peel offsets down to the base being written through.
    while let Expression::ArrayAccess(aa) = cur {
        cur = aa.array.unparenthesized();
    }
    let Expression::Variable(Variable::Direct(dv)) = cur else {
        // Property / static-property / class-constant roots, `$$v`, call
        // results, literals — none of them is a frame-private binding.
        return RefTarget::Escaping;
    };
    let name = strip_dollar(bytes_to_string(dv.name));
    if SUPERGLOBALS.contains(&name.as_str()) {
        return RefTarget::Superglobal;
    }
    // A by-ref parameter is an alias of the *caller's* binding: writing it is
    // caller-observable, so it is not local to this frame.
    if cx.byref_params.contains(&name) {
        return RefTarget::Escaping;
    }
    // In an aliased frame no name is provably frame-private: `global $rows;`
    // makes `$rows` the interpreter's, `$a = &$b` makes two names one binding,
    // `extract()`/`$$v` can bind anything. The flag is frame-wide because the
    // constructs are: proving *which* names survive is a dataflow question this
    // structural scan deliberately does not ask (ADR-0001 give-up discipline).
    if cx.frame_aliased {
        return RefTarget::Escaping;
    }
    RefTarget::Local
}

/// The per-frame context [`scan_effect_origins`] consults: the ADR-0033
/// callback-resolution map, plus the two facts by-ref out-parameter coloring
/// needs about the enclosing frame (ADR-0063 §2.3).
struct EffectScanCx {
    /// Body-local single-assignment `$var → CallbackRef` map (ADR-0033).
    locals: HashMap<String, CallbackRef>,
    /// Names bound by a by-ref parameter (`function f(array &$rows)`): writes
    /// through them are caller-observable.
    byref_params: HashSet<String>,
    /// Whether the frame carries any construct that defeats "this name is a
    /// frame-private binding" — `global`, `static`, `$$v`, `extract`/`compact`,
    /// `eval`, `include`, a reference assignment, or a by-ref `use (&$x)`. This
    /// is exactly the ADR-0001 give-up list ([`scan_opaque`]), reused: every
    /// member of it is a locality-defeating aliasing or scope-injection
    /// construct, so the give-up list and the locality question have the same
    /// answer and cannot drift apart.
    frame_aliased: bool,
    /// What this frame writes, for the ADR-0067 declared-receiver gate.
    writes: ReceiverWrites,
}

impl EffectScanCx {
    /// Build the context for a function-like frame from its parameter list, its
    /// already-collected callback map, the frame's aliasing verdict, and the
    /// frame's receiver-write set (ADR-0067).
    fn new(
        params: &mago_syntax::cst::FunctionLikeParameterList<'_>,
        locals: HashMap<String, CallbackRef>,
        frame_aliased: bool,
        writes: ReceiverWrites,
    ) -> Self {
        let byref_params = params
            .parameters
            .iter()
            .filter(|p| p.is_reference())
            .map(|p| strip_dollar(bytes_to_string(p.variable.name)))
            .collect();
        Self { locals, byref_params, frame_aliased, writes }
    }
}

/// What a frame **writes**, for the ADR-0067 declared-receiver gate.
///
/// A receiver carries its declaration's effect envelope only while the binding it
/// names is still the one the declaration typed. So the gate is not a dataflow
/// question but a frame-wide veto: any write to the name anywhere in the body —
/// an assignment, an increment, a `foreach`/`catch` binding, or merely handing it
/// to a call that could take it by reference — disqualifies **every** use of that
/// name as a declared receiver. The fallback is the pre-ADR-0067 behavior: the
/// receiver resolves to nothing and taints exhaustiveness.
#[derive(Debug, Default)]
struct ReceiverWrites {
    /// Variable names (no `$`) the body may write, over-approximated.
    vars: HashSet<String>,
    /// `$this->…` property names the body may write, over-approximated.
    props: HashSet<String>,
    /// Treat *every* name as written — a frame the gate does not model (a
    /// closure/arrow body, or one that lets `$this` itself escape to another
    /// name, through which any property could be written behind our back).
    all: bool,
}

impl ReceiverWrites {
    /// The verdict for a frame the gate does not model: nothing is stable.
    fn poisoned() -> Self {
        Self { vars: HashSet::new(), props: HashSet::new(), all: true }
    }

    fn writes_var(&self, name: &str) -> bool {
        self.all || self.vars.contains(name)
    }

    fn writes_prop(&self, name: &str) -> bool {
        self.all || self.props.contains(name)
    }
}

/// Collect a statement body's [`ReceiverWrites`]. Variables reuse the existing
/// over-approximating collectors (every assignment lvalue, increment, binding —
/// plus every variable handed to a call, which a by-ref parameter could rebind),
/// joined with the frame-rebinding constructs those collectors deliberately do
/// not see ([`collect_frame_rebinds`]); properties get the same treatment through
/// [`collect_this_prop_writes`].
fn receiver_writes<'a, 'arena>(statements: impl Iterator<Item = &'a Statement<'arena>>) -> ReceiverWrites
where
    'arena: 'a,
{
    let mut vars: Vec<String> = Vec::new();
    let mut w = ReceiverWrites::default();
    for s in statements {
        let node = Node::Statement(s);
        collect_assign_writes(&node, &mut vars);
        collect_call_vars(&node, &mut vars);
        collect_frame_rebinds(&node, &mut vars);
        collect_this_prop_writes(&node, &mut w);
    }
    w.vars = vars.into_iter().collect();
    w
}

/// The two ways a frame's *binding* changes without any assignment the shared
/// collectors can see — both of them writes as far as the declared-receiver gate
/// is concerned:
///
/// * a **by-ref closure capture**, `function () use (&$r) { … }`. The capture
///   aliases the enclosing binding, so the closure can rebind `$r` from inside a
///   scope [`collect_assign_writes`] deliberately stops at — and it can do so
///   whenever it is *called*, which is not a fact this structural scan tracks.
///   The name is therefore written unconditionally, whatever the closure body
///   does with it. A by-value `use ($r)` or an arrow-function capture is a copy
///   and rebinds nothing, so neither disqualifies the receiver.
/// * a **`global $r;`** statement, which rebinds the name to the interpreter's
///   global of that name — legal even when `$r` is a parameter. (`static $r;`
///   over a parameter name is a PHP compile error, so there is nothing to catch.)
///
/// Over-collection is sound here: it only ever makes a receiver fall back to the
/// pre-ADR-0067 taint. So the walk descends through nested closures too — a
/// capture found there names *that* frame's binding, and forgetting one more name
/// in ours costs nothing. Named function/class-like declarations are their own
/// lexical world and are not descended.
fn collect_frame_rebinds(node: &Node<'_, '_>, out: &mut Vec<String>) {
    match node {
        Node::Closure(cl) => {
            if let Some(use_clause) = &cl.use_clause {
                for v in use_clause.variables.iter() {
                    if v.ampersand.is_some() {
                        let name = strip_dollar(bytes_to_string(v.variable.name));
                        if !out.contains(&name) {
                            out.push(name);
                        }
                    }
                }
            }
        }
        Node::Global(g) => {
            for v in g.variables.iter() {
                if let Variable::Direct(dv) = v {
                    let name = strip_dollar(bytes_to_string(dv.name));
                    if !out.contains(&name) {
                        out.push(name);
                    }
                }
            }
        }
        Node::Function(_)
        | Node::AnonymousClass(_)
        | Node::Class(_)
        | Node::Interface(_)
        | Node::Trait(_)
        | Node::Enum(_) => return,
        _ => {}
    }
    for child in node.children() {
        collect_frame_rebinds(&child, out);
    }
}

/// Record every `$this->prop` a subtree may **write** (and poison the whole
/// property set when `$this` itself escapes into another binding). Mirrors
/// [`collect_assign_writes`]'s traversal discipline with one deliberate
/// difference: it **descends into closures and arrow functions**, because a
/// non-static one declared in a method binds the very same `$this`, so
/// `function () { $this->repo = …; }` writes *this* frame's property. Descending
/// into a `static function () {}` (whose `$this` is unbound) over-collects, which
/// is the sound direction. Named function/class-like declarations, whose `$this`
/// is genuinely foreign, are still not descended.
fn collect_this_prop_writes(node: &Node<'_, '_>, w: &mut ReceiverWrites) {
    match node {
        Node::Assignment(a) => {
            collect_this_props(&Node::Expression(a.lhs), &mut w.props);
            // `$x = $this;` — every property is writable through the other name.
            if is_this_expr(a.rhs) {
                w.all = true;
            }
            collect_this_prop_writes(&Node::Expression(a.rhs), w);
            return;
        }
        Node::UnaryPrefix(u) => {
            if matches!(
                u.operator,
                UnaryPrefixOperator::PreIncrement(_) | UnaryPrefixOperator::PreDecrement(_)
            ) {
                collect_this_props(&Node::Expression(u.operand), &mut w.props);
            }
        }
        Node::UnaryPostfix(u) => collect_this_props(&Node::Expression(u.operand), &mut w.props),
        Node::ForeachValueTarget(t) => {
            collect_this_props(&Node::Expression(t.value), &mut w.props);
            return;
        }
        Node::ForeachKeyValueTarget(t) => {
            collect_this_props(&Node::Expression(t.key), &mut w.props);
            collect_this_props(&Node::Expression(t.value), &mut w.props);
            return;
        }
        Node::Unset(u) => {
            for v in u.values.iter() {
                collect_this_props(&Node::Expression(v), &mut w.props);
            }
        }
        // An argument may be taken by reference (or stored), so a property handed
        // to a call is written as far as this gate is concerned — and `$this`
        // handed to one escapes entirely.
        Node::FunctionCall(c) => note_argument_escapes(&c.argument_list, w),
        Node::MethodCall(c) => note_argument_escapes(&c.argument_list, w),
        Node::NullSafeMethodCall(c) => note_argument_escapes(&c.argument_list, w),
        Node::StaticMethodCall(c) => note_argument_escapes(&c.argument_list, w),
        // A foreign `$this` — these declarations are their own object's world.
        // Closures and arrow functions are pointedly NOT here: theirs is ours.
        Node::Function(_)
        | Node::AnonymousClass(_)
        | Node::Class(_)
        | Node::Interface(_)
        | Node::Trait(_)
        | Node::Enum(_) => return,
        _ => {}
    }
    for child in node.children() {
        collect_this_prop_writes(&child, w);
    }
}

/// Record one call's argument list into the declared-receiver write set.
fn note_argument_escapes(list: &mago_syntax::cst::ArgumentList<'_>, w: &mut ReceiverWrites) {
    for arg in list.arguments.iter() {
        let value = arg.value().unparenthesized();
        if is_this_expr(value) {
            w.all = true;
        }
        collect_this_props(&Node::Expression(value), &mut w.props);
    }
}

/// Collect every `$this->prop` property name in a subtree (over-collection is
/// intended: this feeds write positions, where forgetting more is sound).
fn collect_this_props(node: &Node<'_, '_>, out: &mut HashSet<String>) {
    if let Node::PropertyAccess(pa) = node
        && let Some((var, prop)) = prop_fetch_of(pa.object, &pa.property)
        && var == "this"
    {
        out.insert(prop);
    }
    for child in node.children() {
        collect_this_props(&child, out);
    }
}

/// Whether an expression is exactly `$this`.
fn is_this_expr(expr: &Expression<'_>) -> bool {
    matches!(
        expr.unparenthesized(),
        Expression::Variable(Variable::Direct(dv)) if strip_dollar(bytes_to_string(dv.name)) == "this"
    )
}

/// Whether any statement of a frame carries an ADR-0001 give-up-list construct —
/// the [`EffectScanCx::frame_aliased`] verdict for a statement body.
fn body_aliased<'a, 'arena>(statements: impl Iterator<Item = &'a Statement<'arena>>) -> bool
where
    'arena: 'a,
{
    statements.into_iter().any(|s| node_poisons(&Node::Statement(s)))
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
fn scan_effect_origins(node: &Node<'_, '_>, cx: &EffectScanCx, out: &mut Vec<EffectOrigin>) {
    match node {
        // A statically-named call is either a builtin (catalog-classified) or a
        // same-file user function (a propagation edge) — the effects pass decides.
        Node::FunctionCall(fc) => {
            if let Expression::Identifier(id) = fc.function {
                // A named call passing a resolvable callback is a HigherOrder origin
                // (the invocation-shape redemption); otherwise a plain Call edge.
                // `higher_order_of_call` and `arg_targets_of_call` reject the same
                // named/spread argument lists, so on the `Some` arm the target
                // vector is exactly `arg_count` long.
                let arg_targets = arg_targets_of_call(fc, cx);
                match higher_order_of_call(fc) {
                    Some((callee, callbacks, arg_count)) => {
                        out.push(EffectOrigin::HigherOrder {
                            callee,
                            callbacks,
                            arg_count,
                            // Both helpers reject the same argument lists, so this
                            // is always `Some` on this arm.
                            arg_targets: arg_targets.clone().unwrap_or_default(),
                            span: to_span(fc.span()),
                        });
                    }
                    None => out.push(EffectOrigin::Call {
                        name: name_ref(id),
                        span: to_span(id.span()),
                        arg_targets,
                    }),
                }
            } else if let Some(cb) = direct_var_callee(fc).and_then(|v| cx.locals.get(&v).cloned()) {
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
                (effect_recv_of_object_declared(mc.object, cx), method_name_of(&mc.method))
            {
                out.push(EffectOrigin::MethodCall { receiver: recv, method, span: to_span(mc.span()) });
            } else {
                // `$var->m()` / `$o->$m()` — receiver or selector not resolvable.
                out.push(EffectOrigin::Opaque { span: to_span(mc.span()) });
            }
        }
        Node::NullSafeMethodCall(mc) => {
            if let (Some(recv), Some(method)) =
                (effect_recv_of_object_declared(mc.object, cx), method_name_of(&mc.method))
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
        scan_effect_origins(&child, cx, out);
    }
}

/// Walk a body subtree, appending every instance/static method call as a
/// [`CallExpr`] (ADR-0043 §6 comprehensive method-call surface). Mirrors
/// [`scan_effect_origins`]'s traversal discipline: it descends into control flow
/// and sub-expressions (so a nested `foo($this->m($x))` is captured) but NOT into
/// nested function/closure/class-like bodies, which are their own scopes. Dynamic
/// receivers/selectors are still recorded (as [`Callee::Dynamic`]) — the sweep
/// needs to see them to taint. Constructor calls are intentionally omitted (the
/// constructor is magic; never a transform candidate).
fn scan_method_calls(node: &Node<'_, '_>, out: &mut Vec<CallExpr>) {
    match node {
        Node::MethodCall(mc) => out.push(lower_method_call(
            mc.object,
            &mc.method,
            &mc.argument_list,
            to_span(mc.span()),
            false,
        )),
        Node::NullSafeMethodCall(mc) => out.push(lower_method_call(
            mc.object,
            &mc.method,
            &mc.argument_list,
            to_span(mc.span()),
            true,
        )),
        Node::StaticMethodCall(sc) => {
            out.push(lower_static_call(sc.class, &sc.method, &sc.argument_list, to_span(sc.span())));
        }
        // A method/static **first-class callable** — `$o->m(...)`, `Foo::m(...)` (PHP
        // 8.1) — is not a call but references the method as a value: it produces a
        // `Closure` that can be invoked with *any* arguments later, so it makes the
        // method's callers unenumerable exactly as `[$o, 'm']` does. These lower to
        // [`ArgValue::Other`] as values (a documented deferral) and so are invisible
        // to the value scan; record them here as non-positional reference-"calls" so
        // the reverse sweep taints the method (unknown receiver → `resolution-
        // ambiguous`; a resolved receiver → `named-or-spread-args`) and never
        // promotes it. Constructor first-class callables cannot exist.
        Node::MethodPartialApplication(mpa) => {
            out.push(first_class_method_ref(mpa.object, &mpa.method, to_span(mpa.span())));
        }
        Node::StaticMethodPartialApplication(spa) => {
            out.push(first_class_static_ref(spa.class, &spa.method, to_span(spa.span())));
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
        scan_method_calls(&child, out);
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
fn lower_hint(hint: &Hint<'_>, rc: &RefResolver) -> Option<NativeType> {
    let mut members = Vec::new();
    let mut nullable = false;
    lower_hint_into(hint, rc, &mut members, &mut nullable)?;
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
    rc: &RefResolver,
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
        // A class / interface / enum name (ADR-0043): resolve to its FQN and
        // join the union as an `Instance` member — lowercase-normalized for
        // matching, source-cased for diagnostics. `self`/`static`/`parent` are
        // *not* `Hint::Identifier` (they are their own hint variants) — they
        // stay in the silence arm below, per ADR-0043 (late-static-binding is
        // not v1).
        Hint::Identifier(id) => {
            let display = rc.class_display_fqn(&name_ref(id));
            members.push(TypeMember::Instance { fqn: display.to_ascii_lowercase(), display });
        }
        Hint::Nullable(n) => {
            *nullable = true;
            lower_hint_into(n.hint, rc, members, nullable)?;
        }
        Hint::Union(u) => {
            lower_hint_into(u.left, rc, members, nullable)?;
            lower_hint_into(u.right, rc, members, nullable)?;
        }
        Hint::Parenthesized(p) => lower_hint_into(p.hint, rc, members, nullable)?,
        // An intersection of object types (`A&B&…`, ADR-0043): collect every
        // conjunct's resolved class and join them as one conjunctive
        // `InstanceInter` member. Any non-class conjunct (`array`, `mixed`,
        // `iterable`, `callable`, `object`, `self`/`static`/`parent`, a nested
        // scalar) collapses the whole hint to silence (zero-FP) via the `?`.
        Hint::Intersection(_) => {
            let mut classes = Vec::new();
            collect_intersection_classes(hint, rc, &mut classes)?;
            members.push(TypeMember::InstanceInter(classes));
        }
        // `array`, `mixed`, `iterable`, `callable`, `object`, `self`/`static`/
        // `parent`, `void`/`never` → silence.
        _ => return None,
    }
    Some(())
}

/// Accumulate the resolved classes of an intersection hint into `out`. Recurses
/// through nested `Intersection`/`Parenthesized` nodes; each leaf must be a
/// class/interface identifier (PHP forbids scalar or `null` intersection
/// members). Returns `None` — propagated up to collapse the whole hint to
/// silence — the moment a leaf is anything other than a class name.
fn collect_intersection_classes(
    hint: &Hint<'_>,
    rc: &RefResolver,
    out: &mut Vec<ClassRef>,
) -> Option<()> {
    match hint {
        Hint::Intersection(i) => {
            collect_intersection_classes(i.left, rc, out)?;
            collect_intersection_classes(i.right, rc, out)?;
        }
        Hint::Parenthesized(p) => collect_intersection_classes(p.hint, rc, out)?,
        Hint::Identifier(id) => {
            let display = rc.class_display_fqn(&name_ref(id));
            out.push(ClassRef { fqn: display.to_ascii_lowercase(), display });
        }
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

    let LoweredArgs { args, named_args, has_spread, positional_only, arg_conds } =
        lower_argument_list(&c.argument_list);
    CallExpr {
        callee,
        callee_ref,
        receiver,
        args,
        named_args,
        has_spread,
        positional_only,
        span: to_span(c.span()),
        arg_conds,
    }
}

/// The lowered condition of a statement-position `assert(<expr>[, <desc>])` call
/// (ADR-0052 §5), or `None` when the callee is not the global `assert` builtin or
/// the call has no positional first argument. The name match is case-insensitive
/// and accepts the unqualified `assert` and the root-qualified `\assert`; a
/// namespaced `Foo\assert` (a different function) is rejected.
fn assert_stmt_cond(c: &FunctionCall<'_>) -> Option<CondExpr> {
    let Expression::Identifier(id) = c.function else { return None };
    let name = bytes_to_string(id.last_segment());
    if !name.eq_ignore_ascii_case("assert")
        || !matches!(name_ref(id).kind, RefKind::Unqualified | RefKind::FullyQualified)
    {
        return None;
    }
    let first = c.argument_list.arguments.iter().find_map(|arg| match arg {
        Argument::Positional(p) if p.ellipsis.is_none() => Some(p.value),
        _ => None,
    })?;
    Some(lower_cond(first))
}

/// The lowered form of an argument list, shared by every call shape (function /
/// method / static / constructor). See [`CallExpr`] for the field semantics.
struct LoweredArgs {
    args: Vec<Arg>,
    named_args: Vec<NamedArg>,
    has_spread: bool,
    positional_only: bool,
    arg_conds: Vec<Option<CondExpr>>,
}

/// Lower an argument list, separating positional and named arguments and flagging
/// argument unpacking (ADR-0049 §6). A positional argument that appears *after* a
/// named or spread argument is a PHP compile error; it is folded into `has_spread`
/// (the "unanalyzable shape" signal) so the arity check stays silent on it.
fn lower_argument_list(list: &mago_syntax::cst::ArgumentList<'_>) -> LoweredArgs {
    let mut positional_only = true;
    let mut has_spread = false;
    let mut seen_non_positional = false;
    let mut args = Vec::new();
    let mut named_args = Vec::new();
    let mut arg_conds: Vec<Option<CondExpr>> = Vec::new();
    for arg in list.arguments.iter() {
        match arg {
            Argument::Positional(p) if p.ellipsis.is_none() => {
                // A plain positional after a named/spread argument is non-canonical
                // (a compile error) — mark the whole list unanalyzable.
                if seen_non_positional {
                    has_spread = true;
                }
                args.push(Arg { value: lower_arg_value(p.value), span: to_span(p.value.span()) });
                arg_conds.push(lower_guard_arg(p.value));
            }
            Argument::Named(n) => {
                positional_only = false;
                seen_non_positional = true;
                named_args.push(NamedArg {
                    name: bytes_to_string(n.name.value),
                    value: lower_arg_value(n.value),
                    span: to_span(n.span()),
                });
            }
            // A spread `...$x` positional argument: unpacking, count unproven.
            Argument::Positional(_) => {
                positional_only = false;
                has_spread = true;
                seen_non_positional = true;
            }
        }
    }
    // The common case is "no argument is a condition"; keep the parallel vector
    // empty then, so an ordinary call carries no extra allocation.
    if arg_conds.iter().all(Option::is_none) {
        arg_conds.clear();
    }
    LoweredArgs { args, named_args, has_spread, positional_only, arg_conds }
}

/// The **guard reading** of one call argument (see [`CallExpr::arg_conds`]), or
/// `None` when the argument is not a condition the [`CondExpr`] vocabulary
/// models.
///
/// This is not `lower_cond` under another name, and the difference is the point:
/// `lower_cond` is total (it answers `Opaque { reads }` for everything it cannot
/// model, walking the subtree to collect those reads), whereas this runs on
/// **every argument of every call in the project** and must therefore decline in
/// O(1) for the shapes that dominate real code — a variable, a literal, a
/// property fetch, a concatenation. So each arm is a positive recognition, the
/// fallback is a bare `None`, and only the recognized arms may walk anything.
fn lower_guard_arg(expr: &Expression<'_>) -> Option<CondExpr> {
    match expr.unparenthesized() {
        // `isset(…)` / `empty(…)`: `lower_cond` owns both forms and their
        // scope rules; an unmodelled one comes back `Opaque` and is declined.
        Expression::Construct(Construct::Isset(_) | Construct::Empty(_)) => {
            match lower_cond(expr) {
                CondExpr::Opaque { .. } => None,
                c => Some(c),
            }
        }
        Expression::UnaryPrefix(u) if matches!(u.operator, UnaryPrefixOperator::Not(_)) => {
            Some(CondExpr::Not(Box::new(lower_guard_arg(u.operand)?)))
        }
        Expression::Binary(b) => match b.operator {
            // A composition is modelled only when BOTH halves are: a guard whose
            // one half is unknown claims nothing on either polarity.
            BinaryOperator::And(_) | BinaryOperator::LowAnd(_) => Some(CondExpr::And(
                Box::new(lower_guard_arg(b.lhs)?),
                Box::new(lower_guard_arg(b.rhs)?),
            )),
            BinaryOperator::Or(_) | BinaryOperator::LowOr(_) => Some(CondExpr::Or(
                Box::new(lower_guard_arg(b.lhs)?),
                Box::new(lower_guard_arg(b.rhs)?),
            )),
            // Equality/identity over a constant-key projection is the tag
            // discrimination guard (A-G4); `lower_binary_cond` decides whether the
            // operands are representable and says `Opaque` when they are not.
            BinaryOperator::Identical(_)
            | BinaryOperator::NotIdentical(_)
            | BinaryOperator::Equal(_)
            | BinaryOperator::NotEqual(_)
            | BinaryOperator::AngledNotEqual(_) => match lower_binary_cond(b) {
                CondExpr::Opaque { .. } => None,
                c => Some(c),
            },
            _ => None,
        },
        // A named call — `array_key_exists('a', $d)` and its siblings. `reads` is
        // the honest set, as the guard-position lowering computes it.
        other @ (Expression::Call(_) | Expression::Instantiation(_)) => {
            let call = named_call(other)?;
            Some(CondExpr::Call { call: Box::new(call), reads: cond_reads(other) })
        }
        _ => None,
    }
}

/// The simple method name of a member selector, if it is a plain identifier
/// (`->m`, `::m`). Dynamic selectors (`->$m`, `->{...}`) yield `None`.
fn method_name_of(selector: &ClassLikeMemberSelector<'_>) -> Option<String> {
    match selector {
        ClassLikeMemberSelector::Identifier(id) => Some(bytes_to_string(id.value)),
        _ => None,
    }
}

/// The constant / enum-case name of a `Class::NAME` access, if statically named
/// (`::CONST`, `::Case`). A dynamic name (`Class::{$x}`) yields `None`.
fn class_const_name(selector: &ClassLikeConstantSelector<'_>) -> Option<String> {
    match selector {
        ClassLikeConstantSelector::Identifier(id) => Some(bytes_to_string(id.value)),
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
        // A depth-1 property-fetch receiver `$var->prop->m()` (ADR-0052 §7): the
        // object is read from the heap `$var->prop` fact. A chain or a dynamic name
        // (`prop_fetch_of` returns `None`) falls through to `Dynamic`. The receiver
        // var is never `$this` here — `$this->prop->m()` still decomposes as a
        // `$this` property whose object is `prop`, kept out of the guarded `$this`
        // dispatch lane by construction (it is a Prop, not `Receiver::This`).
        Expression::Access(Access::Property(pa)) => {
            prop_fetch_of(pa.object, &pa.property).map(|(var, prop)| Receiver::Prop { var, prop })
        }
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

/// The effect-graph receiver of a method-call object **including** the ADR-0067
/// declared forms: a never-written variable (`$r->m()`) and a never-written
/// `$this` property read (`$this->repo->m()`). Both are recorded by *name* only —
/// they name no class here; the effects pass resolves the declared type and
/// decides whether an interface envelope applies (and taints exactly as before
/// when it does not).
///
/// The proven forms come first and unchanged: this is a strict extension of
/// [`effect_recv_of_object`], which the throw scan keeps using as-is.
fn effect_recv_of_object_declared(object: &Expression<'_>, cx: &EffectScanCx) -> Option<EffectRecv> {
    if let Some(recv) = effect_recv_of_object(object) {
        return Some(recv);
    }
    // In an aliased frame no name is provably still its own binding (the same
    // give-up list `RefTarget` reads), so no declared receiver survives it.
    if cx.frame_aliased {
        return None;
    }
    match object.unparenthesized() {
        Expression::Variable(Variable::Direct(dv)) => {
            let name = strip_dollar(bytes_to_string(dv.name));
            // `$this` is handled by `effect_recv_of_object` above; anything else
            // qualifies exactly while the frame never writes it.
            (!cx.writes.writes_var(&name)).then_some(EffectRecv::Var(name))
        }
        Expression::Access(Access::Property(pa)) => {
            let (var, prop) = prop_fetch_of(pa.object, &pa.property)?;
            (var == "this" && !cx.writes.writes_prop(&prop)).then_some(EffectRecv::PropRead(prop))
        }
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
    let LoweredArgs { args, named_args, has_spread, positional_only, arg_conds } =
        lower_argument_list(list);
    CallExpr { callee: None, callee_ref: None, receiver, args, named_args, has_spread, positional_only, span, arg_conds }
}

/// Lower a static method call into a [`CallExpr`].
fn lower_static_call(class: &Expression<'_>, selector: &ClassLikeMemberSelector<'_>, list: &mago_syntax::cst::ArgumentList<'_>, span: Span) -> CallExpr {
    let receiver = match (trace_static_class(class), method_name_of(selector)) {
        (Some(class), Some(method)) => Callee::Static { class, method },
        _ => Callee::Dynamic,
    };
    let LoweredArgs { args, named_args, has_spread, positional_only, arg_conds } =
        lower_argument_list(list);
    CallExpr { callee: None, callee_ref: None, receiver, args, named_args, has_spread, positional_only, span, arg_conds }
}

/// Lower a **method first-class callable** `$o->m(...)` into a reference-"call": a
/// [`CallExpr`] with no positional arguments (`positional_only = false`), so the
/// method-call reverse sweep (ADR-0043 §6) treats it as an unenumerable caller and
/// taints the method rather than promoting it. Receiver construction mirrors
/// [`lower_method_call`] — a resolvable receiver + literal selector keeps the method
/// name (name-scoped taint); a dynamic selector falls to [`Callee::Dynamic`].
fn first_class_method_ref(
    object: &Expression<'_>,
    selector: &ClassLikeMemberSelector<'_>,
    span: Span,
) -> CallExpr {
    let receiver = match (trace_recv_of_object(object), method_name_of(selector)) {
        (Some(recv), Some(method)) => Callee::Method { receiver: recv, method, nullsafe: false },
        _ => Callee::Dynamic,
    };
    CallExpr {
        callee: None,
        callee_ref: None,
        receiver,
        args: Vec::new(),
        named_args: Vec::new(),
        has_spread: false,
        positional_only: false,
        span,
        arg_conds: Vec::new(),
    }
}

/// Lower a **static-method first-class callable** `Foo::m(...)` into a
/// reference-"call" (the static analogue of [`first_class_method_ref`]).
fn first_class_static_ref(
    class: &Expression<'_>,
    selector: &ClassLikeMemberSelector<'_>,
    span: Span,
) -> CallExpr {
    let receiver = match (trace_static_class(class), method_name_of(selector)) {
        (Some(class), Some(method)) => Callee::Static { class, method },
        _ => Callee::Dynamic,
    };
    CallExpr {
        callee: None,
        callee_ref: None,
        receiver,
        args: Vec::new(),
        named_args: Vec::new(),
        has_spread: false,
        positional_only: false,
        span,
        arg_conds: Vec::new(),
    }
}

/// Lower a `new Class(args...)` instantiation into a constructor [`CallExpr`],
/// or `None` when the class is not statically named.
fn lower_construct_call(inst: &Instantiation<'_>) -> Option<CallExpr> {
    let class = instantiation_class(inst)?;
    let LoweredArgs { args, named_args, has_spread, positional_only, arg_conds } =
        match &inst.argument_list {
            Some(list) => lower_argument_list(list),
            // `new C` / `new C()` with no argument list — zero positional arguments.
            None => LoweredArgs {
                args: Vec::new(),
                named_args: Vec::new(),
                has_spread: false,
                positional_only: true,
                arg_conds: Vec::new(),
            },
        };
    Some(CallExpr {
        callee: None,
        callee_ref: None,
        receiver: Callee::Construct { class },
        args,
        named_args,
        has_spread,
        positional_only,
        span: to_span(inst.span()),
        arg_conds,
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
        // A class-constant / enum-case access `Class::NAME` (ADR-0043). The class
        // portion resolves through the same static-class path as `Class::m()`
        // (explicit name or `self`/`static`/`parent`); a dynamic class expr or a
        // dynamic constant name (`Foo::{$x}`) lowers to `Other`. This is an
        // **unproven** value (== `Other`) until the inference layer reinterprets
        // it against a resolved enum or a literal class-constant initializer.
        Expression::Access(Access::ClassConstant(cc)) => {
            match (trace_static_class(cc.class), class_const_name(&cc.constant)) {
                (Some(class), Some(name)) => ArgValue::ClassConst(class, name),
                _ => ArgValue::Other,
            }
        }
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
        // class env tracking) plus its positional and named arguments (both feed the
        // promoted-property seed; only the class name is load-bearing for the class
        // fact). A spread positional is dropped, as before.
        Expression::Instantiation(inst) => match instantiation_class(inst) {
            Some(class) => match inst.argument_list.as_ref() {
                Some(list) => {
                    let LoweredArgs { args, named_args, .. } = lower_argument_list(list);
                    let args = args.into_iter().map(|a| a.value).collect();
                    ArgValue::New(class, args, named_args)
                }
                None => ArgValue::New(class, Vec::new(), Vec::new()),
            },
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
        // Null-coalescing `$a ?? $b` (ADR-0052 §6): a conditional value the walk
        // resolves to `clear_null(fact($a)) join fact($b)`. Lowered structurally;
        // an operand the domain cannot spell lowers to `Other`, and the walk then
        // yields no fact (so `$arr['k'] ?? …` manufactures nothing).
        Expression::Binary(b) if b.operator.is_null_coalesce() => {
            ArgValue::Coalesce(Box::new(lower_arg_value(b.lhs)), Box::new(lower_arg_value(b.rhs)))
        }
        // String concatenation `$a . $b` (issue #59). Structural, like `??` above:
        // an operand's value is an env fact, so the join runs in the walk. Note this
        // is the ONE binary operator lowered as a value — arithmetic still widens to
        // `Other`, because `+`/`-`/`*` carry overflow and int/float promotion
        // questions that byte concatenation does not.
        //
        // Unrepresentable operands are lowered anyway rather than collapsing the
        // whole node: resolution fails on the operand, which is the same silence,
        // and keeping the tree lets a later slice resolve one side independently.
        Expression::Binary(b) if b.operator.is_concatenation() => {
            ArgValue::Concat(Box::new(lower_arg_value(b.lhs)), Box::new(lower_arg_value(b.rhs)))
        }
        // An array/offset read `$base[$key]` (ADR-0049 §7 / S3). Lowered
        // structurally in every rvalue position; the walk fires `offset.missing` /
        // `offset.on-unsupported` **only** at the whitelisted read positions (A7).
        // In an array-*element* position it collapses the literal to `Other` (see
        // [`lower_array_elements`]) — an offset read is not a proven element value.
        Expression::ArrayAccess(aa) => ArgValue::OffsetRead {
            base: Box::new(lower_arg_value(aa.array)),
            key: Box::new(lower_arg_value(aa.index)),
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
                // An offset read is not a proven element value — collapse the whole
                // literal to `Other` exactly as any other unrepresentable element,
                // so `[$a[0]]` never carries an `OffsetRead` into a "concrete array".
                if matches!(value, ArgValue::Other | ArgValue::OffsetRead { .. }) {
                    return ArgValue::Other;
                }
                items.push((ArrayKey::Auto, value));
            }
            ArrayElement::KeyValue(kv) => {
                let Some(key) = lower_array_key(kv.key) else {
                    return ArgValue::Other;
                };
                let value = lower_arg_value(kv.value);
                if matches!(value, ArgValue::Other | ArgValue::OffsetRead { .. }) {
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
        // A float key truncates toward zero — but only when the truncated value is
        // actually an `int`. Outside that range PHP does not produce a key at all:
        // it emits "The float … is not representable as an int, cast occurred"
        // (a WARNING, i.e. a proven runtime break under the abort posture) and the
        // resulting key is the C wraparound, which Rust's saturating `as` does not
        // reproduce — `9.2e18 as i64` is `i64::MAX` here and `i64::MIN` there. So the
        // range test is load-bearing, not defensive: without it this arm would fold a
        // key to the wrong value. Reachable since issue #62 made an out-of-range
        // integer literal a `Float`, which is exactly the input that lands here.
        ArgValue::Float(f)
            if f.is_finite()
                && f.trunc() >= -9_223_372_036_854_775_808.0
                && f.trunc() < 9_223_372_036_854_775_808.0 =>
        {
            Some(ArrayKey::Int(f.trunc() as i64))
        }
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
///
/// Public so the offset-read side (ADR-0049 A10) canonicalizes a runtime string key
/// through the **same** primitive the write/lowering side uses — never a parallel
/// comparison, so `$a = [5 => 'x']; $a["5"]` resolves to the present key 5.
#[must_use]
pub fn php_canonical_int_string(s: &str) -> Option<i64> {
    let i: i64 = s.parse().ok()?;
    (i.to_string() == s).then_some(i)
}

/// Lower an integer literal from its **source spelling** (issue #62).
///
/// PHP's lexer promotes an integer literal that does not fit `int` to `float`, and
/// the promotion is base-blind: decimal, `0x`, `0b`, `0o`, legacy-octal and
/// underscore-separated spellings all follow it. So the decision is made on the
/// magnitude, and the magnitude has to come from the text — see the call site for
/// why the parser's `value` cannot answer it.
///
/// Three outcomes:
/// * fits `i64` → [`ArgValue::Int`], the overwhelmingly common case;
/// * fits `u64` but not `i64` → [`ArgValue::Float`], PHP's promotion;
/// * beyond `u64` → a decimal literal still converts exactly (Rust and PHP both
///   round the digit string to the nearest double, so `99999999999999999999` is
///   `1.0E+20` in both), and any other base yields [`ArgValue::Other`]. Converting a
///   hex/octal/binary literal wider than 64 bits would need big-integer arithmetic
///   for a spelling that essentially does not occur; silence is the safe side, and
///   it is a ceiling rather than a wrong value.
fn lower_int_literal(raw: &[u8]) -> ArgValue {
    let text = String::from_utf8_lossy(raw);
    // Underscores are digit separators anywhere in the literal (PHP 7.4+).
    let text: String = text.chars().filter(|c| *c != '_').collect();
    let (digits, radix) = match text.as_bytes() {
        [b'0', b'x' | b'X', rest @ ..] => (rest, 16),
        [b'0', b'b' | b'B', rest @ ..] => (rest, 2),
        [b'0', b'o' | b'O', rest @ ..] => (rest, 8),
        // Legacy octal: a leading `0` followed by more digits. Bare `0` is decimal
        // zero, and `0` alone must not fall into the octal arm with empty digits.
        [b'0', rest @ ..] if !rest.is_empty() => (rest, 8),
        all => (all, 10),
    };
    let Ok(digits) = std::str::from_utf8(digits) else { return ArgValue::Other };
    match u64::from_str_radix(digits, radix) {
        Ok(v) => i64::try_from(v).map_or_else(|_| ArgValue::Float(v as f64), ArgValue::Int),
        // Beyond `u64`. Decimal converts exactly the way PHP's does; other bases
        // decline rather than guess.
        Err(_) if radix == 10 => {
            digits.parse::<f64>().map_or(ArgValue::Other, ArgValue::Float)
        }
        Err(_) => ArgValue::Other,
    }
}

fn lower_literal(lit: &Literal<'_>) -> ArgValue {
    match lit {
        // An integer literal that does not fit `int` is a **float** in PHP, not a
        // wrapped int (issue #62). The promotion is the lexer's and applies to every
        // base — decimal, hex, octal, binary, underscore-separated alike — so the
        // test is on the parsed value, not the spelling. `9223372036854775808` was
        // previously `v as i64` = `i64::MIN`, a wrong *value* the analyzer then
        // propagated with full confidence; `-9223372036854775808` reached the same
        // place because `wrapping_neg` is a no-op there. (`PHP_INT_MIN` has no
        // integer-literal spelling at all — it is written `-PHP_INT_MAX - 1`.)
        //
        // The parser's own `value` is NOT usable for the overflow decision: it is a
        // `u64` that SATURATES, so `99999999999999999999` arrives as `u64::MAX` —
        // indistinguishable from a real `0xFFFFFFFFFFFFFFFF` and three orders of
        // magnitude off PHP's `1.0E+20`. The spelling is re-read instead.
        Literal::Integer(li) => lower_int_literal(li.raw),
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
    let rc = RefResolver { contexts, regions };
    let mut scopes = vec![build_scope_from(ScopeOwner::TopLevel, &top)];
    collect_scopes(&Node::Program(program), contexts, regions, &rc, &mut scopes);
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
    rc: &RefResolver,
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
        Node::Closure(cl) => out.push(build_closure_scope_from_closure(cl, rc)),
        Node::ArrowFunction(af) => out.push(build_closure_scope_from_arrow(af, rc)),
        _ => {}
    }
    // Recurse so nested functions (inside methods or blocks) and nested classes
    // also get their scopes. Method scopes are only created above (matching
    // `Node::Class`), so this recursion never double-creates one.
    for child in node.children() {
        collect_scopes(&child, contexts, regions, rc, out);
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
    let mut opaque = Vec::new();
    let mut stmts = Vec::new();
    let mut method_calls = Vec::new();
    for s in statements {
        lower_stmt(s, &mut stmts);
        scan_method_calls(&Node::Statement(s), &mut method_calls);
        scan_opaque(&Node::Statement(s), &mut opaque, false);
    }
    // The flag IS the inventory being non-empty (never a second computation).
    let poisoned = !opaque.is_empty();
    let function_name = match &owner {
        ScopeOwner::Function(name) => Some(name.clone()),
        ScopeOwner::TopLevel | ScopeOwner::Method { .. } | ScopeOwner::Closure { .. } => None,
    };
    Scope {
        function_name,
        owner,
        poisoned,
        opaque,
        stmts,
        method_calls,
        params: Vec::new(),
        ret_ty: None,
        effect_origins: Vec::new(),
        throw_origins: Vec::new(),
        is_static: false,
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

/// Build the [`Scope`] for a `function (...) use (...) {...}` closure (ADR-0033).
fn build_closure_scope_from_closure(cl: &mago_syntax::cst::Closure<'_>, rc: &RefResolver) -> Scope {
    let mut stmts = Vec::new();
    let mut effect_origins = Vec::new();
    let mut throw_origins = Vec::new();
    let mut method_calls = Vec::new();
    // The closure's own scope is poisoned by a by-ref `use (&$x)` capture (its
    // captured var is a reference alias) or any in-body poison marker.
    let mut opaque = Vec::new();
    push_byref_captures(cl, &mut opaque, false);
    // A by-ref capture aliases an enclosing binding, so it defeats frame-locality
    // for the whole closure body just as an in-body `global` would.
    // A closure body is not a declared-receiver frame: the effects pass keys it by
    // definition offset and has no parameter list to read a receiver's declared
    // type from, so every name stays unmodelled (today's `Opaque` taint).
    let cx = EffectScanCx::new(
        &cl.parameter_list,
        collect_body_callables(cl.body.statements.iter()),
        !opaque.is_empty() || body_aliased(cl.body.statements.iter()),
        ReceiverWrites::poisoned(),
    );
    for s in cl.body.statements.iter() {
        lower_stmt(s, &mut stmts);
        scan_effect_origins(&Node::Statement(s), &cx, &mut effect_origins);
        scan_throw_origins(&Node::Statement(s), &[], &[], &cx.locals, &mut throw_origins);
        scan_method_calls(&Node::Statement(s), &mut method_calls);
        scan_opaque(&Node::Statement(s), &mut opaque, false);
    }
    let poisoned = !opaque.is_empty();
    Scope {
        function_name: None,
        owner: ScopeOwner::Closure { def_offset: closure_def_offset(cl) },
        poisoned,
        opaque,
        stmts,
        method_calls,
        params: lower_params(&cl.parameter_list, rc),
        ret_ty: cl.return_type_hint.as_ref().and_then(|r| lower_hint(&r.hint, rc)),
        effect_origins,
        throw_origins,
        is_static: cl.r#static.is_some(),
    }
}

/// Build the [`Scope`] for an arrow function `fn(...) => expr` (ADR-0033). The
/// single body expression lowers to one `return <expr>;` statement so a call
/// inside it (`fn($x) => width($x)`) is a reachable propagation/descent edge.
fn build_closure_scope_from_arrow(af: &mago_syntax::cst::ArrowFunction<'_>, rc: &RefResolver) -> Scope {
    let mut effect_origins = Vec::new();
    let mut throw_origins = Vec::new();
    // An arrow body is a single expression — no local assignments to resolve.
    let cx = EffectScanCx::new(
        &af.parameter_list,
        HashMap::new(),
        node_poisons(&Node::Expression(af.expression)),
        ReceiverWrites::poisoned(),
    );
    scan_effect_origins(&Node::Expression(af.expression), &cx, &mut effect_origins);
    scan_throw_origins(&Node::Expression(af.expression), &[], &[], &cx.locals, &mut throw_origins);
    let mut method_calls = Vec::new();
    scan_method_calls(&Node::Expression(af.expression), &mut method_calls);
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
    let mut opaque = Vec::new();
    scan_opaque(&Node::Expression(af.expression), &mut opaque, false);
    let poisoned = !opaque.is_empty();
    Scope {
        function_name: None,
        owner: ScopeOwner::Closure { def_offset: arrow_def_offset(af) },
        poisoned,
        opaque,
        stmts: vec![ret],
        method_calls,
        params: lower_params(&af.parameter_list, rc),
        ret_ty: af.return_type_hint.as_ref().and_then(|r| lower_hint(&r.hint, rc)),
        effect_origins,
        throw_origins,
        is_static: af.r#static.is_some(),
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
        // `unset($var[<lit>]);` — a constant-key offset unset (ADR-0062 A-G8).
        // Barrier semantics plus the base and key, exactly as `OffsetWrite`; a
        // multi-target unset, `unset($var)` itself, and a dynamic key all fall
        // through to the plain barrier below.
        Statement::Unset(u)
            if u.values.len() == 1
                && u.values.iter().next().is_some_and(|v| const_key_offset(v).is_some()) =>
        {
            let (base, key) = u
                .values
                .iter()
                .next()
                .and_then(|v| const_key_offset(v))
                .expect("guarded above");
            Stmt {
                span: ZERO_SPAN,
                kind: StmtKind::OffsetUnset { base, key },
                invalidated: Vec::new(),
            }
        }
        // Everything else (declarations, `goto`, labels, `declare`, other unsets,
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
        // `empty($x['k'])` — PHP's own definition, lowered rather than special-
        // cased: `empty(e)` is true iff `e` is not set OR `e` is falsy, i.e.
        // `!isset(e) || !e`. The narrowing then falls out of the compositional
        // walk with no `empty`-aware code anywhere downstream: the true branch of
        // a disjunction of two negations records nothing (correct — `empty` true
        // leaves both "absent" and "present-falsy" open), and its false branch is
        // De Morgan'd back to `isset(e) && e`, which is exactly the presence
        // promotion `!empty($x['k'])` deserves.
        //
        // Scope is `isset`'s, deliberately (A-G4's depth-one projection): only
        // `empty($var[<literal>])`. `empty($x)` on a bare variable, a property or
        // dynamic key, and every deeper path keep the pre-existing `Opaque`
        // lowering — a bare-variable `empty` would newly feed the scalar
        // refinement lane (`Truthy` over a plain local), a much wider behavior
        // change than this leg is measuring.
        Expression::Construct(Construct::Empty(e)) => match const_key_offset(e.value) {
            Some((var, key)) => CondExpr::Or(
                Box::new(CondExpr::Not(Box::new(CondExpr::Isset {
                    var: var.clone(),
                    key: Box::new(key.clone()),
                }))),
                Box::new(CondExpr::Not(Box::new(CondExpr::Truthy(CondOperand::Offset {
                    var,
                    key: Box::new(key),
                })))),
            ),
            None => CondExpr::Opaque { reads: cond_reads(expr) },
        },
        // `isset($x['k'])` (ADR-0062 S4). Recognized ONLY when every operand is a
        // depth-one constant-key projection; a multi-argument isset is a
        // conjunction by PHP semantics and lowers to the matching `And` chain.
        // Anything else — `isset($x)`, a property or dynamic key, a mixed list —
        // falls through to the pre-S4 `Opaque` lowering below, unchanged.
        Expression::Construct(Construct::Isset(iss)) => {
            let operands: Option<Vec<CondExpr>> = iss
                .values
                .iter()
                .map(|v| {
                    const_key_offset(v)
                        .map(|(var, key)| CondExpr::Isset { var, key: Box::new(key) })
                })
                .collect();
            match operands {
                Some(parts) if !parts.is_empty() => parts
                    .into_iter()
                    .reduce(|a, b| CondExpr::And(Box::new(a), Box::new(b)))
                    .expect("non-empty"),
                _ => CondExpr::Opaque { reads: cond_reads(expr) },
            }
        }
        other => match lower_cond_operand(other) {
            // A resolvable call in guard position is retained as `Call` (minimal
            // recognition for `-if-true`/`-if-false` consumption, ADR-0052 §5); every
            // other unmodeled condition stays `Opaque`. `Call` and `Opaque` are
            // interchangeable for the verdict and the invalidation set — the only
            // added behavior is the tag consumption in the branch walk.
            CondOperand::Other => {
                let reads = cond_reads(other);
                match named_call(other) {
                    Some(call) => CondExpr::Call { call: Box::new(call), reads },
                    None => CondExpr::Opaque { reads },
                }
            }
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

/// `$var[<literal>]` — the depth-one constant-key projection ADR-0062 A-G4
/// scopes tag discrimination to. `None` for a non-variable base, a nested
/// access, or a key that is not a concrete literal.
fn const_key_offset(expr: &Expression<'_>) -> Option<(String, ArgValue)> {
    let Expression::ArrayAccess(aa) = expr.unparenthesized() else { return None };
    let Expression::Variable(Variable::Direct(dv)) = aa.array.unparenthesized() else {
        return None;
    };
    let key = lower_arg_value(aa.index);
    key.is_concrete_value().then(|| (strip_dollar(bytes_to_string(dv.name)), key))
}

/// The base and constant-key path of an offset **lvalue**, depth one or two:
/// `$var[<lit>]` → `("var", [lit])`, `$var[<lit>][<lit>]` → `("var", [k1, k2])`.
/// `None` for an append (`$var[] = …`), a dynamic key, a deeper chain, or a
/// non-variable base — each of which stays a plain barrier.
fn const_key_offset_path(expr: &Expression<'_>) -> Option<(String, Vec<ArgValue>)> {
    let Expression::ArrayAccess(aa) = expr.unparenthesized() else { return None };
    let key = lower_arg_value(aa.index);
    if !key.is_concrete_value() {
        return None;
    }
    match aa.array.unparenthesized() {
        Expression::Variable(Variable::Direct(dv)) => {
            Some((strip_dollar(bytes_to_string(dv.name)), vec![key]))
        }
        inner => {
            let (base, mut keys) = const_key_offset(inner).map(|(v, k)| (v, vec![k]))?;
            keys.push(key);
            Some((base, keys))
        }
    }
}

/// Lower a comparison operand: a bare `$var`, a literal, a constant-key
/// projection, or [`CondOperand::Other`].
fn lower_cond_operand(expr: &Expression<'_>) -> CondOperand {
    match expr.unparenthesized() {
        Expression::Variable(Variable::Direct(dv)) => {
            CondOperand::Var(strip_dollar(bytes_to_string(dv.name)))
        }
        other if const_key_offset(other).is_some() => {
            let (var, key) = const_key_offset(other).expect("checked");
            CondOperand::Offset { var, key: Box::new(key) }
        }
        // A bare constant fetch (issue #29). `true`/`false`/`null` never reach
        // here — they lex as literals and lower through the arm below.
        Expression::ConstantAccess(ca) => CondOperand::Const(name_ref(&ca.name)),
        other => match lower_arg_value(other) {
            // A scalar literal, or a fully-concrete array literal — the latter lets a
            // `$x === []` / `$x === [1, 2]` guard narrow `$x` to a `Singleton` array
            // (ADR-0049 §7: the `=== []` branch is what proves offset 0 missing). A
            // non-concrete array (an element that is a `Var`/call/offset read) stays
            // `Other`, so nothing unproven is ever treated as a decided literal.
            v if v.is_concrete_value() => CondOperand::Literal(v),
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
            } else if a.operator.is_assign()
                && let Some((base, keys)) = const_key_offset_path(a.lhs)
            {
                // `$var[<lit>] = …` / `$var[<lit>][<lit>] = …` (ADR-0062 A-G8).
                // Still a barrier in the walk — see `StmtKind::OffsetWrite` — but
                // one that names the base and key so the shape lane survives it.
                let mut invalidated = Vec::new();
                collect_call_vars(&Node::Expression(a.rhs), &mut invalidated);
                Stmt {
                    span: ZERO_SPAN,
                    kind: StmtKind::OffsetWrite { base, keys, value: lower_arg_value(a.rhs) },
                    invalidated,
                }
            } else {
                // Assignment to a non-simple lvalue (`$a[] = …`, `$a[$i] = …`,
                // `$o->$p = …`, `$a->b->c = …`, `Foo::$s = …`). Barrier (the sound
                // floor); a by-ref property alias `$r = &$x->p` is caught by the
                // poison family above.
                Stmt { span: ZERO_SPAN, kind: StmtKind::Barrier, invalidated: Vec::new() }
            }
        }
        Expression::Call(Call::Function(fc)) => {
            // `assert(<expr>)` — a statement-position assert whose argument lowers to
            // a condition (ADR-0052 §5). `assert` is a pure by-value builtin (it never
            // mutates its argument by reference), so the narrowed variables carry no
            // invalidation; a non-lowerable argument falls back to a plain `Call`.
            if let Some(cond) = assert_stmt_cond(fc) {
                Stmt { span: ZERO_SPAN, kind: StmtKind::Assert { cond }, invalidated: Vec::new() }
            } else {
                let mut invalidated = Vec::new();
                collect_call_vars(&Node::Expression(expr), &mut invalidated);
                Stmt { span: ZERO_SPAN, kind: StmtKind::Call(lower_call(fc)), invalidated }
            }
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
///
/// The predicate is `scan_opaque` asking for the first site only: one walk decides
/// poisoning and enumerates the reasons, so [`Scope::opaque`] cannot disagree with
/// [`Scope::poisoned`].
fn node_poisons(node: &Node<'_, '_>) -> bool {
    // No heap allocation on the (overwhelmingly common) clean path: `Vec::new` does
    // not allocate, and `stop_at_first` pushes at most once.
    let mut first = Vec::new();
    scan_opaque(node, &mut first, true);
    !first.is_empty()
}

/// Collect the ADR-0001 give-up-list constructs in `node`'s subtree, appending one
/// [`OpaqueSite`] per construct in source order. Nested function-like bodies are
/// their own scopes and are not descended (they get their own [`Scope`], hence their
/// own sites) — a closure's `use (&$x)` clause is the one exception: a by-ref
/// capture poisons the *enclosing* scope, so it is recorded here and, separately, on
/// the closure's own scope (ADR-0033).
///
/// `stop_at_first` makes the walk exit as soon as one site exists — the predicate
/// path ([`node_poisons`]), which asks only whether the scope is poisoned. The
/// inventory path passes `false` and gets every site. Both share this control flow
/// exactly, so the predicate cannot recognize a construct the inventory misses.
///
/// A matched construct is not descended into: the outermost construct is the site
/// (`extract(compact($a))` is one `extract`), which is where the predicate stops too.
fn scan_opaque(node: &Node<'_, '_>, out: &mut Vec<OpaqueSite>, stop_at_first: bool) {
    let direct = match node {
        // Direct markers.
        Node::Global(_) => Some(OpaqueConstruct::Global),
        Node::Static(_) => Some(OpaqueConstruct::StaticVar),
        Node::EvalConstruct(_) => Some(OpaqueConstruct::Eval),
        Node::IncludeConstruct(_)
        | Node::IncludeOnceConstruct(_)
        | Node::RequireConstruct(_)
        | Node::RequireOnceConstruct(_) => Some(OpaqueConstruct::Include),
        Node::NestedVariable(_) | Node::IndirectVariable(_) => {
            Some(OpaqueConstruct::VariableVariable)
        }
        // `extract(...)` / `compact(...)`.
        Node::FunctionCall(fc) => {
            if let Expression::Identifier(id) = fc.function {
                match bytes_to_string(id.last_segment()).as_str() {
                    "extract" => Some(OpaqueConstruct::Extract),
                    "compact" => Some(OpaqueConstruct::Compact),
                    _ => None,
                }
            } else {
                None
            }
        }
        // Reference assignment `$x = &$y`.
        Node::Assignment(a) => a.rhs.is_reference().then_some(OpaqueConstruct::ReferenceAssign),
        // Closure: inspect its `use (&$x)` capture list, but do not descend into
        // its body (a separate scope).
        Node::Closure(c) => {
            push_byref_captures(c, out, stop_at_first);
            return;
        }
        // Other nested scopes — skip entirely (their own give-up list is their
        // own concern).
        Node::Function(_)
        | Node::ArrowFunction(_)
        | Node::AnonymousClass(_)
        | Node::Class(_)
        | Node::Interface(_)
        | Node::Trait(_)
        | Node::Enum(_) => return,
        _ => None,
    };
    if let Some(construct) = direct {
        out.push(OpaqueSite { construct, span: to_span(node.span()) });
        return;
    }
    for child in node.children() {
        scan_opaque(&child, out, stop_at_first);
        if stop_at_first && !out.is_empty() {
            return;
        }
    }
}

/// Record one [`OpaqueConstruct::ByRefCapture`] site per `use (&$x)` variable of a
/// closure. Shared by the enclosing-scope walk ([`scan_opaque`]) and the closure's
/// own scope build, which is why the by-ref capture appears on both scopes — it is
/// one aliasing fact that defeats value tracking on either side of the capture.
fn push_byref_captures(
    cl: &mago_syntax::cst::Closure<'_>,
    out: &mut Vec<OpaqueSite>,
    stop_at_first: bool,
) {
    let Some(use_clause) = &cl.use_clause else { return };
    for v in use_clause.variables.iter() {
        if v.ampersand.is_some() {
            out.push(OpaqueSite {
                construct: OpaqueConstruct::ByRefCapture,
                span: to_span(v.variable.span()),
            });
            if stop_at_first {
                return;
            }
        }
    }
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
    let offset = to_span(id.span()).start;
    // ADR-0049 A8: the `namespace\bar` relative form lexes as a `QualifiedIdentifier`
    // whose first segment is the reserved `namespace` keyword (never a real segment
    // name). Rewrite it to the distinct `Relative` kind, dropping the prefix, so the
    // remainder resolves against the enclosing namespace instead of being appended to
    // it (the doubled-prefix bug). Case-insensitive: PHP keywords fold case.
    if kind == RefKind::Qualified {
        let first_len = raw.find('\\').unwrap_or(raw.len());
        if raw[..first_len].eq_ignore_ascii_case("namespace") {
            let remainder = raw.get(first_len + 1..).unwrap_or("").to_owned();
            return NameRef { raw: remainder, kind: RefKind::Relative, offset };
        }
    }
    NameRef { raw, kind, offset }
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

/// Fold one `use` statement's items into a context — every class/function import
/// form: the plain sequence (`use A\B, C\D;`), the typed sequence
/// (`use function a\b;`), and the **grouped** forms (`use A\{B, C}`,
/// `use function A\{b, c}`, and the mixed `use A\{B, function c, const D}`). Only
/// `use const` items are skipped (constant resolution is out of scope).
///
/// Grouped-use lowering was previously skipped on the belief that "a miss only
/// fails to resolve, never mis-resolves" — but that belief is false: an unresolved
/// grouped import falls back through [`resolve_class_ref`] to the enclosing
/// namespace (bare, in the global namespace), which can collide with a *different*
/// real class of that fallback name and mis-resolve. That is a genuine FP source
/// (ADR-0049 §6 arity surfaced it on `use Contentful\{Delivery\Query}; new Query()`
/// resolving to an unrelated `Query`), so the grouped forms are now lowered.
fn add_use(u: &mago_syntax::cst::Use<'_>, ctx: &mut NsCtx) {
    match &u.items {
        UseItems::Sequence(seq) => {
            for item in seq.items.iter() {
                let target = bytes_to_string(item.name.value()).trim_start_matches('\\').to_owned();
                ctx.class_imports.insert(use_item_alias(item), target);
            }
        }
        UseItems::TypedSequence(seq) if seq.r#type.is_function() => {
            for item in seq.items.iter() {
                let target = bytes_to_string(item.name.value()).trim_start_matches('\\').to_owned();
                ctx.fn_imports.insert(use_item_alias(item), target);
            }
        }
        // Grouped `use function A\{b, c}` / `use const A\{X, Y}`: one leading type
        // applies to every item under the `A\` prefix.
        UseItems::TypedList(list) => {
            if list.r#type.is_function() {
                let prefix = bytes_to_string(list.namespace.value());
                for item in list.items.iter() {
                    ctx.fn_imports.insert(use_item_alias(item), group_target(&prefix, item));
                }
            }
        }
        // Grouped `use A\{B, function c, const D}`: each item carries its own
        // optional type (`None` ⇒ class, `Function` ⇒ function, `Const` ⇒ skip).
        UseItems::MixedList(list) => {
            let prefix = bytes_to_string(list.namespace.value());
            for mti in list.items.iter() {
                let target = group_target(&prefix, &mti.item);
                let alias = use_item_alias(&mti.item);
                match &mti.r#type {
                    None => {
                        ctx.class_imports.insert(alias, target);
                    }
                    Some(t) if t.is_function() => {
                        ctx.fn_imports.insert(alias, target);
                    }
                    Some(_) => {} // `const` — out of scope.
                }
            }
        }
        // `use const A\B;` — out of scope.
        UseItems::TypedSequence(_) => {}
    }
}

/// The lowercase-normalized import alias for a `use` item: its explicit `as` alias,
/// else the last segment of the imported name (PHP class/function names are
/// case-insensitive, so the map keys on the lowercased form).
/// Whether a `use` statement binds the (case-sensitive) alias `PHP_VERSION_ID`
/// through any of its **const** item forms (issue #29). The exact-case binding
/// name is the explicit `as` alias, else the imported name's last segment.
fn use_binds_php_version_id(u: &mago_syntax::cst::Use<'_>) -> bool {
    let item_binds = |item: &mago_syntax::cst::UseItem<'_>| -> bool {
        let bound = match &item.alias {
            Some(a) => bytes_to_string(a.identifier.value),
            None => bytes_to_string(item.name.last_segment()),
        };
        bound == "PHP_VERSION_ID"
    };
    match &u.items {
        UseItems::TypedSequence(seq) if seq.r#type.is_const() => seq.items.iter().any(item_binds),
        UseItems::TypedList(list) if list.r#type.is_const() => list.items.iter().any(item_binds),
        UseItems::MixedList(list) => list
            .items
            .iter()
            .any(|mti| mti.r#type.as_ref().is_some_and(|t| t.is_const()) && item_binds(&mti.item)),
        _ => false,
    }
}

fn use_item_alias(item: &mago_syntax::cst::UseItem<'_>) -> String {
    match &item.alias {
        Some(a) => bytes_to_string(a.identifier.value),
        None => bytes_to_string(item.name.last_segment()),
    }
    .to_ascii_lowercase()
}

/// The full target FQN of a grouped-`use` item: `<prefix>\<item name>`, each side
/// trimmed of a stray leading backslash (grouped items are relative to the prefix).
fn group_target(prefix: &str, item: &mago_syntax::cst::UseItem<'_>) -> String {
    let prefix = prefix.trim_start_matches('\\');
    let name = bytes_to_string(item.name.value());
    let name = name.trim_start_matches('\\');
    format!("{prefix}\\{name}")
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

/// Resolve a **class** reference to its FQN (case preserved, no leading `\`) in
/// namespace context `ctx`, applying PHP class-name resolution: fully-qualified
/// names pass through; qualified/unqualified names apply `use` class imports on
/// the first segment, else prepend the current namespace. Class references have
/// no global fallback (unlike functions), so this is a pure function of the
/// reference and its context. Shared by [`SourceTree::resolve_class_fqn`]
/// (use-time) and [`RefResolver`] (lowering-time); both are case-preserved —
/// callers needing the normalized matching key lowercase the result.
fn resolve_class_ref(ctx: &NsCtx, r: &NameRef) -> String {
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
        // ADR-0049 A8: `namespace\Bar` — the remainder resolves against the enclosing
        // namespace only, no imports (`use` never rebinds a `namespace\`-relative
        // name). In the global namespace it is the remainder itself.
        RefKind::Relative => {
            if ctx.namespace.is_empty() {
                r.raw.clone()
            } else {
                format!("{}\\{}", ctx.namespace, r.raw)
            }
        }
    }
}

/// Lowering-time namespace resolver for object type hints (ADR-0043). Carries the
/// file's namespace contexts + regions so a class/interface/enum name in a native
/// hint can be resolved to its FQN (case-preserved; lowercased by the caller into
/// the normalized matching key that matches [`ClassDecl::fqn`]) at the point of
/// lowering, exactly like the FQN post-pass does for declaration names. Threaded
/// alongside the attribute aliases + docs through the hint-bearing lowering
/// functions.
struct RefResolver<'a> {
    contexts: &'a [NsCtx],
    regions: &'a [(u32, u32, usize)],
}

impl RefResolver<'_> {
    /// The case-preserved (source-cased) FQN a class-name reference resolves to,
    /// in the namespace context enclosing its offset. Lowercase the result to get
    /// the normalized matching key.
    fn class_display_fqn(&self, r: &NameRef) -> String {
        resolve_class_ref(ctx_of(self.contexts, self.regions, r.offset), r)
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
