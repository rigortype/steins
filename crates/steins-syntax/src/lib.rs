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
//! The lowered tree exposes syntax needed by the analysis crates while keeping
//! parser-backend types private. Spans are byte offsets, convertible to 1-based
//! line/column via [`SourceTree::position`].

use steins_domain::PhpStr;

use mago_allocator::LocalArena;
use mago_database::file::FileId;
use mago_span::HasSpan;
use mago_syntax::cst::Access;
use mago_syntax::cst::Argument;
use mago_syntax::cst::ArrayElement;
use mago_syntax::cst::AssignmentOperator;
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
use mago_syntax::cst::ExpressionStatement;
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
use mago_syntax::cst::StringPart;
use mago_syntax::cst::Trivia;
use mago_syntax::cst::TriviaKind;
use mago_syntax::cst::UnaryPrefixOperator;
use mago_syntax::cst::UseItems;
use mago_syntax::cst::Variable;

use std::collections::HashMap;
use std::collections::HashSet;

pub mod stack_guard;

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

/// How a name was written at a *reference* site, driving PHP name resolution.
/// This is the syntactic input the resolution rules key
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
    /// `use const` imports (issue #198): **exact-case** alias → case-preserved
    /// target FQN. Unlike the two maps above, the keys are *not* lowercased:
    /// constant names are case-sensitive in PHP (the case-insensitive third
    /// argument to `define()` was removed in 8.0, and the workspace floor is 8.1
    /// per ADR-0011), so `use const A\FOO;` binds `FOO` and nothing else.
    pub const_imports: HashMap<String, String>,
}

impl NsCtx {
    fn global() -> Self {
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
/// union shape alongside the scalars. It is not [`Copy`] because it owns
/// `String`s.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TypeMember {
    /// A full scalar type (`int`, `float`, `string`, `bool`).
    Scalar(ScalarType),
    /// A `false` / `true` literal type. It accepts **only** the exact matching
    /// bool value — no other value coerces into it (empirically verified against
    /// PHP 8.5: `0`/`""`/`true` into a `false`-only type all `TypeError`).
    BoolLiteral(bool),
    /// An object type: a class / interface / enum name (ADR-0043). The is-a
    /// oracle consumes this; native scalar-value acceptance stays silent on
    /// unions containing an `Instance` unless object incompatibility is definite.
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
    /// A native **intersection** of object types (`A&B&…`, ADR-0043). A single union
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

/// A native parameter or return type Steins reasons about: scalar and object
/// members, intersections, nullable forms, and unions thereof. Unsupported members
/// (`array`, `mixed`, `iterable`, `callable`, `object`, `self`/`static`/`parent`,
/// `void`/`never`, …) lower the **whole** type to `None` so the checker stays silent
/// (ADR-0002).
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

    /// `true` when any union member is an object type. Native scalar-value
    /// consumers treat such types as unknown; object checks proceed only when
    /// incompatibility is definite.
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
    // untyped surface (ADR-0078, issue #200)
    /// The **file byte span of the native type hint as written**, or `None` when
    /// the parameter declares no native type at all.
    ///
    /// [`Self::ty`] cannot answer "is there a native type?" — it is a *modeling*
    /// answer, and `array`, `iterable`, `mixed`, `callable`, `object`,
    /// `self`/`static`/`parent`, `void`/`never` all lower to `None` there while
    /// being perfectly good declarations. This field is the *syntactic* answer:
    /// `None` means the source wrote nothing, full stop. Slice it out of the file
    /// with [`SourceTree::source_slice`] when the spelling itself matters (the
    /// `array`/`iterable` discrimination `untyped.iterable-value` makes).
    pub hint_span: Option<Span>,
    // end untyped surface (ADR-0078, issue #200)
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
/// The scan does **not** descend into nested function/closure/class bodies,
/// which are separate scopes. It *does*
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
    ///
    /// `const_args` carries the call's first two positional arguments in their
    /// proven-constant form, for the argument-dependent narrowing of a
    /// wrapper-capable stream row (issue #318; the ADR-0064 symbolic
    /// argument-dependent transfer seam) — see [`ConstArgs`].
    Call { name: NameRef, span: Span, arg_targets: Option<Vec<RefTarget>>, const_args: ConstArgs },
    /// An `echo` / `print` / short-echo (`<?=`) construct, or non-blank inline
    /// HTML between a `?>` and the next `<?php`, at `span` — the
    /// `io.output.buffer` effect (ADR-0083: this is the OB-capturable channel).
    /// `keyword` is the spelling for diagnostics.
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
    ///
    /// `const_args` is the same proven-constant pair [`Self::Call`] carries, and
    /// it is not redundant here: a string-literal argument *is* a resolvable
    /// callback reference, so `file_get_contents('/etc/hosts')` arrives on this
    /// arm rather than on `Call`, and the stream narrowing of issue #318 has to
    /// read it from both.
    HigherOrder {
        callee: NameRef,
        callbacks: Vec<(usize, CallbackRef)>,
        arg_count: usize,
        arg_targets: Vec<RefTarget>,
        const_args: ConstArgs,
        span: Span,
    },
    /// A direct `$fn()` variable call resolved (by a body-local single-assignment
    /// analysis) to a known callback (ADR-0033). Its effects join the caller's
    /// (immediate invocation); `span` is the call. An unresolvable `$fn()` stays
    /// [`Self::Opaque`] (the honest taint).
    Callback { cbref: CallbackRef, span: Span },
}

/// One call argument in the form a **structural** scan can prove constant
/// (issue #318): the evidence an argument-dependent catalog row is narrowed on.
///
/// There is deliberately no third variant. A variable, a concatenation, an
/// interpolated string, a class constant, an array element — each of them is a
/// dataflow question, and this scan does not ask dataflow questions (ADR-0001):
/// the argument is simply absent from [`ConstArgs`] and the consumer keeps
/// whatever its argument-blind default was.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CallTarget {
    /// A quoted string literal with **no interpolation**, by its decoded value
    /// (an interpolated `"…{$x}…"` is a composite string, never this).
    Literal(String),
    /// A bare global-constant fetch by its spelling, leading `\` stripped
    /// (`STDOUT`, `\STDERR`). Namespaced fetches (`App\STDOUT`) are excluded —
    /// they name somebody else's constant. An *unqualified* fetch inside a
    /// namespace is kept, because PHP falls back to the global constant.
    ConstFetch(String),
}

/// The **proven-constant leading arguments** of a named call (issue #318):
/// positions 0 and 1, each `None` unless [`CallTarget`] could read it.
///
/// Two positions, because two is what the argument-dependent stream rows need —
/// the target (`file_get_contents($path)`, `fwrite($handle, …)`), plus either a
/// mode (`fopen($path, $mode)`) or a second target (`copy($from, $to)`). A named
/// or spread argument anywhere in the list defeats positional mapping and leaves
/// both fields empty, exactly as `arg_targets` is withheld wholesale there.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct ConstArgs {
    /// Positional argument 0.
    pub first: Option<CallTarget>,
    /// Positional argument 1.
    pub second: Option<CallTarget>,
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
    /// resolves to nothing and taints exhaustiveness like
    /// [`EffectOrigin::Opaque`].
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
/// contradictory (`Pure` = empty upper bound, the tighter of the two); Pure wins,
/// `labels` is empty, and no contradiction diagnostic is emitted (see
/// `attrs_effect_envelope`).
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
    // untyped surface (ADR-0078, issue #200)
    /// The file byte span of the native **return** type hint as written, or
    /// `None` when the declaration writes none. The return-side twin of
    /// [`Param::hint_span`], and for the same reason: `void`, `never`, `array`
    /// and friends are real declarations that [`Self::ret`] models as `None`.
    pub ret_span: Option<Span>,
    // end untyped surface (ADR-0078, issue #200)
    pub span: Span,
    /// The file byte span of the function's **body block**, braces included.
    ///
    /// [`Self::span`] is the *name* span, so it cannot answer questions about what
    /// the body does. This one can, and it exists for exactly one consumer: the
    /// ADR-0032 argument-pass carry gate (issue #295), which must decide whether a
    /// callee could possibly touch an object handed to it. PHP locals are
    /// **lexical** — a name a body never spells cannot be read, written, captured
    /// or passed on by it — and every construct that reaches a binding
    /// non-lexically (`$$v`, `extract`/`compact`, `eval`, `include`, `global`, a
    /// by-ref `use`) is on the give-up list that sets [`Scope::poisoned`]. So a
    /// token scan over this span is a sound "the callee cannot reach it" oracle,
    /// where the linear trace — which drops nested sub-expressions to
    /// [`ArgValue::Other`] and unrecognized statements to [`StmtKind::Barrier`] —
    /// is not.
    pub body_span: Span,
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
    // untyped surface (ADR-0078, issue #200)
    /// The file byte span of the native return type hint as written, or `None`
    /// when the method writes none — the method-world twin of
    /// [`FunctionDecl::ret_span`].
    pub ret_span: Option<Span>,
    // end untyped surface (ADR-0078, issue #200)
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
/// Static properties are lowered to keep the class surface complete but are
/// **never tracked in the heap** because they are global state (ADR-0036).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PropertyDecl {
    /// Property name without the leading `$`.
    pub name: String,
    /// The native scalar/union type, or `None` when untyped / non-scalar / complex
    /// (same lowering as a param/return type; the property-mismatch check skips
    /// `None`-typed props, zero-FP).
    pub ty: Option<NativeType>,
    // untyped surface (ADR-0078, issue #200)
    /// The file byte span of the native property type hint as written, or `None`
    /// when the declaration writes none — the property-world twin of
    /// [`Param::hint_span`]. For a promoted constructor parameter this is the
    /// parameter's own hint span.
    pub hint_span: Option<Span>,
    // end untyped surface (ADR-0078, issue #200)
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

// untyped surface (ADR-0078, issue #200)
/// One class constant's **declaration shape** — the facts a declaration-reading
/// check needs and neither [`ClassDecl::consts`] (values) nor
/// [`ClassDecl::const_visibility`] (visibility) carries.
///
/// A third list rather than a wider tuple in either of those, for the reason the
/// second one was split off: each existing list's *absence* of a name already
/// means something specific to its own consumers, and this one records every
/// declared constant regardless of initializer, exactly as the visibility list
/// does. Enum cases are not constants for this purpose — they live in
/// [`ClassDecl::enum_cases`], and their type is the enum, so they are never
/// `untyped.class-constant` material.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ClassConstDecl {
    /// The constant name as written (constant names are case-sensitive).
    pub name: String,
    /// The file byte span of the PHP 8.3 native constant type hint as written
    /// (`const string FOO = 'x';`), or `None` when the declaration writes none.
    pub hint_span: Option<Span>,
    /// The raw `/** … */` docblock preceding the `const` declaration, if any —
    /// read for a `@var` claim. Shared by every item of a multi-item
    /// `const A = 1, B = 2;` declaration, exactly as the native hint is.
    pub docblock: Option<String>,
    /// The span of the constant name identifier (for diagnostic positions).
    pub span: Span,
}
// end untyped surface (ADR-0078, issue #200)

/// A user-defined class, **interface**, or **enum** declaration (top-level or
/// namespaced). Interfaces are lowered (ADR-0033 Liskov), distinguished by
/// [`Self::is_interface`]; enums are lowered (ADR-0043 object/method world),
/// distinguished by [`Self::is_enum`] and carrying [`Self::enum_cases`] +
/// [`Self::enum_backing`]. A class that *uses* a trait sets
/// [`ClassDecl::uses_traits`] so resolution gives up on it.
///
/// Enum lowering records cases, the backing type, and the `implements` list for
/// the is-a oracle. Enum **method bodies are not analyzed**: no scope is built and
/// [`Self::methods`] is empty. The enum still enters the class index for subtyping.
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
    /// a trait name runs (deprecated, not fatal). Only the name is lowered: no
    /// member flattening (`methods`/`properties`/… are empty), so a trait is inert
    /// for every existing check and merely occupies its FQN in the symbol/ambiguity
    /// table (a trait sharing an FQN with a class is `Ambiguous`, both silent).
    pub is_trait: bool,
    /// `true` when this declaration is nested under anything but a plain
    /// namespace/program node — a function/method body, `if`, `try`, loop, or bare
    /// block (ADR-0049 A2i). A conditional declaration leaves *which* definition
    /// binds to runtime load order (the `if (!class_exists(…))` fallback-stub
    /// shape), so a chain containing one **re-dams** absence claims.
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
    // inaccessible members (ADR-0078, issue #185)
    /// The **declared visibility** of every class constant, as `(name, visibility)`
    /// pairs — the surface [`Self::consts`] deliberately does not carry.
    ///
    /// Two differences from [`Self::consts`], and both are why this is a separate
    /// list rather than a third tuple field: a constant with a non-literal
    /// initializer (`const K = self::J . 'x';`) is **recorded here** though it is
    /// absent there, so this list's absence of a name really does mean "this
    /// class-like declares no such constant"; and the value list's meaning
    /// (ADR-0043 §2 "no proven literal") is a contract other consumers already
    /// read. Names are stored as written (constant names are case-sensitive).
    /// Enum cases are not constants for this purpose — they live in
    /// [`Self::enum_cases`] and are always public.
    pub const_visibility: Vec<(String, Visibility)>,
    // untyped surface (ADR-0078, issue #200)
    /// Every declared class constant's declaration shape (see [`ClassConstDecl`]),
    /// in source order. Populated for classes, interfaces and enums alike.
    pub const_decls: Vec<ClassConstDecl>,
    // end untyped surface (ADR-0078, issue #200)
    /// The names of the class-body **hooked** properties this declaration drops.
    ///
    /// A `public int $p { get => … }` member binds no value and is not lowered to a
    /// [`PropertyDecl`] at all, which is right for every value check and wrong for
    /// any check that asks "does this class declare `p`?". A hooked property
    /// **overrides an inherited one** (`php -r`-witnessed at 8.5.9: a child's
    /// `public int $p { get => 42; }` over a parent's `protected int $p` prints
    /// `42`), so a member-visibility claim that could not see it would convict legal
    /// code. Recording the bare names keeps that door shut without putting a
    /// value-less property back on the surface every other consumer walks.
    pub hooked_properties: Vec<String>,
    // end inaccessible members (ADR-0078, issue #185)
    // member absence (ADR-0078, issue #197)
    /// `true` when the declaration carries `#[AllowDynamicProperties]`.
    ///
    /// The attribute re-licences what PHP 8.2 deprecated: writing an undeclared
    /// property on such a class is legal and creates it, so the class's property
    /// set is **open** and no enumeration of its declarations can prove a name
    /// absent. `property.undefined` therefore treats the attribute anywhere in a
    /// receiver's chain as an obstacle, exactly as it treats `__get`.
    ///
    /// Recognized by the attribute's **name only**, in either the bare
    /// (`#[AllowDynamicProperties]`) or fully-qualified
    /// (`#[\AllowDynamicProperties]`) spelling. PHP itself resolves the name
    /// against the current namespace, so a namespaced file without a `use` import
    /// would not really get the engine attribute — but a class *meaning* to be
    /// open and failing to import it is still a class whose author writes dynamic
    /// properties, and over-silencing there costs an absence claim, never a wrong
    /// one. Matching is case-insensitive (PHP class-name semantics).
    pub allows_dynamic_properties: bool,
    // end member absence (ADR-0078, issue #197)
    /// `true` if the class `use`s any trait. Trait methods are merged into the
    /// class at compile time but their bodies live elsewhere, so a
    /// trait-using class is treated as unresolvable (give up → silent).
    pub uses_traits: bool,
    /// The raw `/** … */` docblock preceding the class-like declaration, if any.
    /// Read for class-level `@template` names, which shadow same-named classes in
    /// **every** member docblock of this class-like (issue #5). `None` for a trait,
    /// whose members are not lowered.
    pub docblock: Option<String>,
    /// The **file byte span** of that docblock, when one is adopted — the
    /// class-world analogue of [`FunctionDecl::docblock_span`], under the same
    /// contract: [`Self::docblock`] is the exact substring `[span.start, span.end)`
    /// of the source, so a docblock-relative tag offset maps into the file by
    /// adding `span.start`. Retained for the transform engine (ADR-0034), whose
    /// `effects-envelope` writes the **class-level** interop envelope
    /// (`@phpstan-all-methods-pure`, ADR-0082 §7) into this block.
    pub docblock_span: Option<Span>,
    /// The span of the class name identifier.
    pub span: Span,
}

/// A representable call argument or assignment right-hand side.
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
    /// A PHP string literal's value — a byte string, not a Rust `String`
    /// (ADR-0080).
    Str(PhpStr),
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
    /// as a nested argument of another descent. A foldable builtin's argument that
    /// is itself a project call resolves the same way (issue #127): `strtoupper(g(1))`
    /// folds once `g(1)`'s Singleton summary is a concrete arg, under the same
    /// descent guard as nested binding.
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
    /// value** (ADR-0031): the walk evaluates `cond` against the env and,
    /// when decided, resolves to the chosen arm; when undecided it joins the two
    /// arms (a `OneOf` if both are literal, else unknown). Short-ternary `?:` and
    /// null-coalescing `??` are **not** lowered here because their operands need
    /// negative/definedness facts outside this domain.
    ///
    /// `then_span`/`else_span` are the source extents of the two arms. PHP
    /// evaluates exactly one of them, so a decided guard proves the other one's
    /// span **unevaluated** — the walk records it as dead (ADR-0052 §6: "the
    /// direct env-free pass stands down on spans covered here exactly as
    /// `mark_dead` already models"). They are carried for that one purpose and
    /// are deliberately outside the [`Hash`] impl: two ternaries that differ only
    /// in position denote the same value.
    Ternary {
        cond: Box<CondExpr>,
        then_val: Box<ArgValue>,
        then_span: Span,
        else_val: Box<ArgValue>,
        else_span: Span,
    },
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
    /// [`ArgValue::Other`]. The walk resolves it against the heap: a known
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
    ///
    /// The third field is the RIGHT operand's source extent. `??` gates it the
    /// way a ternary gates its arms: a left operand proven set-and-non-null means
    /// PHP never evaluates the right, and the walk records that span dead
    /// (ADR-0052 §6). Outside the [`Hash`] impl, for the same reason as
    /// [`Self::Ternary`]'s arm spans.
    Coalesce(Box<ArgValue>, Box<ArgValue>, Span),
    /// An array/offset read `$base[$key]` in **rvalue** position (ADR-0049 §7 / S3).
    /// `base` and `key` are the lowered sub-expressions (each may itself be any
    /// [`ArgValue`], commonly a [`Self::Var`] base and a literal/`Var` key). This is
    /// never a *proven* value (`val_of` yields `None`, [`Self::is_literal`] is
    /// `false`): the walk resolves the base to a container `Fact` and the key to a
    /// proven value, then judges `offset.missing` / `offset.on-unsupported` **only in
    /// the whitelisted read contexts** (ADR-0049 A7: plain assignment-RHS and return
    /// operands). It is a *silence carrier* everywhere else — an operand of `??`
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
    /// A compound `.=` lowers its rvalue to [`Self::Other`] (see [`StmtKind`]).
    Concat(Box<ArgValue>, Box<ArgValue>),
    /// A bare **global-constant fetch** (`PREG_SET_ORDER`, `SOME_CONST`) in value
    /// position (issue #168), carried with its qualification kind. Like
    /// [`ArgValue::ClassConst`] this is an **unproven** value (never a literal,
    /// never concrete): PHP resolves an unqualified constant through the current
    /// namespace before falling back to the global one, so only a consumer that
    /// applies the engine-constant shadow discipline (issue #29's `PHP_VERSION_ID`
    /// rules) may read a value out of it. Every other consumer treats it exactly
    /// like the [`ArgValue::Other`] it lowered to before.
    GlobalConst(NameRef),
    /// A binary-operator expression in **value** position (issue #260): the
    /// operator-value node.
    ///
    /// Lowered structurally, exactly like [`Self::Concat`] and [`Self::Coalesce`]:
    /// an operand is commonly a [`Self::Var`] whose value only the walk knows, so
    /// the decision belongs at resolution time where the env is in hand. This is
    /// never a proven value on its own ([`Self::is_literal`] is `false`) — it
    /// resolves to one exactly when the evaluator can decide it.
    ///
    /// Only [`ValueOp`]-representable operators reach here; every other binary
    /// operator still lowers to [`Self::Other`], so the node never claims reach it
    /// does not have.
    Binary { op: ValueOp, lhs: Box<ArgValue>, rhs: Box<ArgValue> },
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
    /// as a function name through project/catalog resolution. Method and static
    /// first-class callables (`$o->m(...)`, `Foo::m(...)`) lower to
    /// [`ArgValue::Other`].
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
    /// A string key that is not integer-like. A byte string (ADR-0080).
    Str(PhpStr),
    /// **A key the source does not spell as a literal** (issue #336): `[$k => $v]`,
    /// `[f() => $v]`, `[FOO => $v]`.
    ///
    /// Before this existed, one such key lowered the **whole** literal to
    /// [`ArgValue::Other`], so `array_key_first([$string => null])` and every
    /// `foreach ([$k => $v] as …)` had no fact to work from at all. The key
    /// expression is carried instead, so the walk can ask what it *is* even
    /// though it cannot say which key it lands on.
    ///
    /// It is **not** a normalized key and never becomes one: [`normalize_array`]
    /// declines a literal containing it, because the next-auto-index chain runs
    /// through it (an unknown key may be an integer, which moves every following
    /// `Auto` position) and a guessed key set is wrong rather than wide.
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

/// The next PHP auto-index for an omitted array key, given the running max
/// integer key seen so far (`None` — no integer key yet — answers position
/// `0`) and the [`NextIntRule`] in force. Saturating: at `i64::MAX` PHP itself
/// refuses to append; the clamped index collides and last-wins folds it, which
/// is as close as a pure key model gets to that runtime error.
///
/// Shared by [`normalize_array_with`] (which also folds `max_seen`'s bump back
/// in afterward) and [`duplicate_array_keys`] (issue #187), which needs the
/// same arithmetic without the last-wins fold — one auto-index rule, not two.
#[must_use]
fn next_auto_index(max_seen: Option<i64>, rule: NextIntRule) -> i64 {
    let mut i = max_seen.map_or(0, |m: i64| m.saturating_add(1));
    if matches!(rule, NextIntRule::FloorAtZero) {
        i = i.max(0);
    }
    i
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
                let i = next_auto_index(max_seen, rule);
                max_seen = Some(max_seen.map_or(i, |m| m.max(i)));
                NormKey::Int(i)
            }
            // Unreachable through `normalize_array`, which declines a literal
            // holding one (issue #336); the raw `_with` form is only called
            // with a resolvable key set. Stopping is the honest total answer —
            // the entries after an unknown key have unknown positions, so
            // continuing would invent them.
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
    // A key the source did not spell as a literal is unresolvable under EVERY
    // rule (issue #336): it may be an integer, so it moves the next-auto-index
    // for each following `Auto` position, and it may collide with a written key,
    // so even the entry count is uncertain. This is checked before the minor is
    // consulted, because no PHP version resolves it.
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
//
// Purely syntactic — no inference, no evaluation of the *value* side at all —
// so [`ArrayLiteralSite`] is collected file-wide at parse time (like
// [`ForeachSite`]) rather than riding the value-domain [`ArgValue::Array`]
// lowering, which bails the WHOLE literal to `Other` the moment any single
// *value* fails to lower. A duplicate-key finding cares only about keys:
// `[1 => 'a', 1 => $x]` still has a provable duplicate even though `$x`
// defeats value representation.
// ---------------------------------------------------------------------------

/// One element of a literal array expression, reduced to what
/// [`duplicate_array_keys`] needs (issue #187): its resolved key and its own
/// span. The key coercion is the exact one `lower_array_key` applies at
/// every other array-literal use site — no second table.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ArrayLiteralElement {
    /// `Some(ArrayKey::Auto)` for a bare `value` element (no explicit key);
    /// `Some(ArrayKey::Int(_) | ArrayKey::Str(_))` for an explicit key the fold
    /// gate resolves (PHP's own coercion: `1`/`'1'`/`1.7`/`true` all fold to
    /// `Int(1)`, `null` to `Str("")`); `None` for an explicit key expression the
    /// fold gate cannot pin (a variable, a call, a nested expression, …), a
    /// `...$spread`, or a `list()`-style missing hole — none of the three
    /// contributes a knowable key.
    pub key: Option<ArrayKey>,
    /// The element's own span (`key => value`, or the bare value) — where a
    /// duplicate-key finding naming this element is positioned.
    pub span: Span,
}

/// One literal array expression in a file (`[...]` or legacy `array(...)`),
/// its elements in source order — the whole evidence `array.duplicate-key`
/// needs (issue #187). Collected file-wide, independent of whether the
/// literal is ever read on a live path (mechanics, not proof).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ArrayLiteralSite {
    pub elements: Vec<ArrayLiteralElement>,
}

/// One shadowed-then-overwritten pair `array.duplicate-key` reports (issue
/// #187): `shadowed_span` is the earlier element carrying the same
/// PHP-normalized key, `winner_span` is the later one that overwrites it (and
/// is where the finding is positioned), and `key` is the shared [`NormKey`]
/// for the message.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DuplicateArrayKey {
    pub key: NormKey,
    pub winner_span: Span,
    pub shadowed_span: Span,
}

/// Scan one [`ArrayLiteralSite`]'s elements for PHP-key-equal duplicates
/// (issue #187, `array.duplicate-key`), reusing the exact key coercion
/// (`lower_array_key`, via [`ArrayLiteralElement::key`]) and next-auto-index
/// rule (`next_auto_index`) [`normalize_array`] applies to fold a whole
/// literal — no second coercion table.
///
/// Pairing is **adjacent**: each later occurrence is reported against the
/// nearest earlier occurrence of the same key, matching how PHP itself
/// overwrites the slot in place (three occurrences of one key yield two
/// findings — first-shadowed-by-second, second-shadowed-by-third — never one
/// finding naming the first and third).
///
/// An element whose key the fold gate cannot pin (`None` — a variable, a
/// call, a spread, a destructuring hole) is skipped, and — since its actual
/// runtime key could be an integer that shifts the auto-increment counter —
/// every `Auto` element after it is unresolvable too and skipped as well
/// (silence, never a guess: `php -r '$x=5; var_export([$x=>"a","b"]);'` →
/// `[5=>"a", 6=>"b"]`, so an unproven key genuinely moves later positions).
///
/// `php_minor` selects the next-int rule exactly as [`normalize_array`] does.
/// When it is `None`, both rules run in parallel per `Auto` element; the
/// element resolves only while they agree, and the first disagreement poisons
/// every `Auto` element after it too (the version-dependent leg of ADR-0049
/// A12, resolved per element rather than for the whole literal).
///
/// String keys compare as **byte strings** (ADR-0080), so a literal spelling
/// four distinct invalid-UTF-8 bytes declares four distinct keys.
/// `corpus/symfony__console/Helper/QuestionHelper.php:356` —
/// `["\xC0"=>1, "\xD0"=>1, "\xE0"=>2, "\xF0"=>3]` — is silent because those
/// keys genuinely differ, not because the scan declines to look at them. Until
/// [`PhpStr`] landed they all decoded to one `"\u{FFFD}"` and this scan needed
/// a guard skipping any U+FFFD-bearing key (issue #187), which also cost the
/// true positive on a literal that repeats a real `"\u{FFFD}"`; both the guard
/// and its cost are gone.
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
            // A missing key (a `list()` hole) and an UNKNOWN key (issue #336)
            // poison the auto chain the same way: neither resolves a position,
            // so no later key can be compared against an earlier one.
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
            ArgValue::Str(v) => v.render_with('"'),
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
            ArgValue::Coalesce(l, r, _) => format!("({} ?? {})", l.render(), r.render()),
            ArgValue::Concat(l, r) => format!("({} . {})", l.render(), r.render()),
            ArgValue::Binary { op, lhs, rhs } => {
                format!("({} {} {})", lhs.render(), op.symbol(), rhs.render())
            }
            ArgValue::OffsetRead { base, key } => format!("{}[{}]", base.render(), key.render()),
            ArgValue::ClassConst(class, name) => format!("{}::{name}", class.render()),
            ArgValue::EnumCase(class, case) => format!("{class}::{case}"),
            ArgValue::GlobalConst(r) => r.raw.clone(),
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
        ArgValue::Str(s) => s.to_php_literal(),
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
/// class-world resolution rules dispatch on (ADR-0001 sound dispatch).
/// Method/static/constructor resolvability depends on receiver exactness; see
/// `steins-infer`.
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
    /// [`ArgValue`] cannot express `isset($d['a'])` as a value, but
    /// `Util_Assert::true(isset($d['a']));` is a guard the
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

/// A comparison operator in a lowered [`CondExpr`] (ADR-0031).
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
    /// (ADR-0031); at the verdict level it decides only for concrete
    /// numeric operands, else `Maybe`.
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
/// **value** position (issue #260).
///
/// The condition language ([`CondExpr::Cmp`]) already carries a comparison for
/// *guard* position; this is the same operator seen as the expression's value
/// (`$b = $x > 3;` rather than `if ($x > 3)`). The two share [`CmpOp`] so one
/// evaluator decides both — the value path reuses the condition path's decision
/// procedure rather than restating PHP's comparison semantics.
///
/// Only the operators an evaluator can actually answer are lowered here; an
/// operator with no value-position evaluation stays [`ArgValue::Other`], so an
/// unimplemented arm declines rather than manufacturing a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueOp {
    /// A comparison `=== !== == != < <= > >=`. Its value is a `bool`.
    Cmp(CmpOp),
}

impl ValueOp {
    /// The operator as written, for a rendered value expression.
    #[must_use]
    pub const fn symbol(self) -> &'static str {
        match self {
            ValueOp::Cmp(op) => op.symbol(),
        }
    }
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
    /// (ADR-0062 A-G4: binding base, constant key). Carried so a
    /// tagged-union guard (`$s['kind'] === 'circle'`, `match ($s['kind'])`,
    /// `switch ($s['kind'])`) can subtract the base's array arms by the field's
    /// `admits` verdict.
    ///
    /// Only the shape-narrowing pass reads this variant. Other consumers derive
    /// no verdict or value-lane refinement from it.
    Offset { var: String, key: Box<ArgValue> },
    /// A bare **global-constant fetch** (`PHP_VERSION_ID`, `SOME_CONST`), carried
    /// as the reference was written (issue #29). Lowered so the version-guard
    /// fold can recognize the engine's `PHP_VERSION_ID` and decide the branch
    /// against the resolved target range. Other consumers derive no verdict or
    /// refinement from it.
    Const(NameRef),
    /// Anything else (a call, a property fetch, an arithmetic sub-expression, …)
    /// — unrepresentable for the *verdict*, but never opaque about what it did.
    ///
    /// A comparison operand is a position where arbitrary PHP runs, and the
    /// operand variants above cannot say so: before these two fields existed,
    /// `preg_match($re, $s, $m) === 1` lowered to a `Cmp` that mentioned neither
    /// the call nor `$m`, so the branch walk had nothing to invalidate and an
    /// earlier `$m = []` survived into the branch as a false `list{}` (issue
    /// #158). What is unmodeled here is the operand's *value*, not its effects.
    Other {
        /// The call this operand **is**, when it is a statically-resolvable one
        /// (`named_call` — the same recognition [`CondExpr::Call`] uses in
        /// guard position). `None` for a dynamic callee and for an operand that
        /// merely *contains* a call (`f($x) + 1`), whose invalidation still
        /// lands through `invalidates`.
        call: Option<Box<CallExpr>>,
        /// The variables a write inside this operand may have rebound — the
        /// operand's **invalidation set**, and deliberately not "every variable
        /// it mentions": it is empty unless the subtree contains something that
        /// can write a variable of this scope at all
        /// (`operand_writers`). `$o->p === $s` writes nothing, so `$o` and
        /// `$s` keep their facts across it; `f($o->p) === $s` may write both.
        invalidates: Vec<String>,
        /// The ADR-0070 by-value evidence for the names in `invalidates`, in the
        /// shape [`Stmt::invalidated`] carries it — so a fact the callee provably
        /// cannot reach (`count($a) === count($b)` hands `$a` and `$b` to a
        /// by-value parameter) survives the comparison, exactly as it survives a
        /// statement-position call.
        ///
        /// **Empty means no exemption may be granted**, and it is empty whenever
        /// the operand contains a writer that is not a call
        /// (`OperandWriters::Any`): the gate describes what a *callee* can do
        /// to its arguments and says nothing about an assignment or an `++`
        /// sitting beside it, so `f($y) + ($y = 1)` keeps the blanket drop.
        sites: Vec<InvalidatedVar>,
    },
}

/// A small lowered condition language (ADR-0031). The trace evaluator
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
    /// `class_exists` …, ADR-0049 §4 / N3). Other guard calls evaluate to
    /// `Maybe`; `reads` invalidates their variables on the excluded path.
    Call { call: Box<CallExpr>, reads: Vec<String> },
    /// `isset($var[<literal>])` — a **key-presence guard**, depth exactly one
    /// (ADR-0062 S4). PHP's `isset` is true when the key exists *and* its value
    /// is not null, which is the distinction the narrowing consumes: the true
    /// branch promotes presence and strips `null` from the value slot.
    ///
    /// Only this exact form is lowered. `isset($x)` on a bare variable and an
    /// `isset` over a property/dynamic key lower to [`Self::Opaque`].
    /// `empty($x[<literal>])` —
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
    /// A structured `if`/`elseif`/`else` (ADR-0031): the trace models its
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
    /// does. Under the ADR-0027 ratchet, the walk forgets only the variables the
    /// construct might touch **or branch on**, rather than all known values.
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
    /// * `may_return` — `true` if the subtree contains a `return` the walk cannot
    ///   see as a top-level [`StmtKind::Return`] (e.g. `return` inside a
    ///   `foreach`/`try` body). Return-fact summaries (ADR-0057 T0) contribute the
    ///   declared floor for such a construct so a visible sibling `return null`
    ///   cannot become a false Singleton (ADR-0075 / issue #126 review). Nested
    ///   function/closure bodies are separate scopes and are not counted.
    ///
    /// Limitation: if every branch returns early, fall-through code is dead, but
    /// this representation cannot prove that without branch/reachability analysis.
    Opaque { writes: Vec<String>, reads: Vec<String>, poisons: bool, may_return: bool },
    /// `$var[<lit>] = <rvalue>;` / `$var[<lit>][<lit>] = <rvalue>;` — a
    /// **constant-key offset write** (ADR-0062 A-G8's invalidation table).
    ///
    /// This is a [`Self::Barrier`] carrying one extra piece of information. The
    /// walk still forgets the whole env and store exactly as a barrier does —
    /// an array write can alias anything the lowering cannot bound — and then
    /// re-establishes *only* the base binding's array shape with the key promoted.
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
    /// `[$a, $b] = <source>;` / `list($a, $b) = <source>;` — an array-destructuring
    /// assignment (issue #288).
    ///
    /// This is a [`Self::Barrier`] carrying one extra piece of information: the
    /// **reads the source undergoes**. A destructure evaluates `<source>[k]` once
    /// per target, exactly as a plain assignment-RHS read would, and PHP warns
    /// `Undefined array key k` for every key that is not there. The targets are
    /// *writes* and are not modeled — the walk still forgets the whole env and
    /// store, as a barrier does — so this variant adds a read position and no
    /// value lane at all.
    ///
    /// `reads` holds one **key path** per target, outermost key first, in source
    /// order: `[$a, $b]` is `[[0], [1]]`, `['a' => $x]` is `[["a"]]`, and a nested
    /// pattern `[[$a], $b]` recurses to `[[0], [0, 0], [1]]` — the outer read
    /// happens too, and it is the one PHP warns about first. A skipped hole
    /// (`[, $b]`) consumes its index without reading it, so it contributes no path.
    /// `call` carries the full [`CallExpr`] when the source *is* a statically-named
    /// call (`[$a, $b] = f();`), so the read judgment can reach its declared return.
    ///
    /// A pattern the lowering cannot read faithfully stays a plain [`Self::Barrier`]:
    /// a by-reference target (`[&$a] = $m` aliases `$m[0]` into existence with no
    /// warning — it is not a read), a spread, or a non-literal key.
    Destructure { source: ArgValue, call: Option<CallExpr>, reads: Vec<Vec<ArgValue>>, span: Span },
    /// Any construct the trace does not model *and* whose write set it cannot
    /// bound (`goto`, labels, `declare`, `__halt_compiler`, and anything the
    /// lowering is unsure of). Erases all known values — the sound floor.
    Barrier,
}

// reachability foundation (ADR-0078, issue #199)

/// Where a statement — or a whole statement list — leaves control (ADR-0078,
/// issue #199): the **terminality judgment** the reachability foundation is built
/// from. Computed at lowering time from the CST (see `stmt_end`), so every
/// consumer reads one answer instead of re-deriving control flow from the
/// deliberately lossy trace IR.
///
/// The judgment is over the **syntactic control-flow graph**, not over path
/// feasibility: a branch condition is read as non-deterministic (both edges
/// live), exactly as PHP's own compiler and every other reachability analysis
/// read it. What the judgment does model precisely is a construct with *no exit
/// edge at all* — `return`, `throw`, `exit`, an `unhandled` `match`, a
/// `while (true)` with no `break`.
///
/// # The safe-side asymmetry — the spine of this design
///
/// [`Self::Unknown`] is not a third outcome to be smoothed away; it is the honest
/// answer for a construct whose exit edges the judgment cannot bound (a
/// `try`/`catch`, a `goto`, a `switch` that resisted structuring). **The safe
/// side of `Unknown` differs by consumer, and each consumer must say which side
/// it takes:**
///
/// * `type.return-missing` (issue #199) — the accusation is *"this body runs off
///   its end"*. Only [`Self::FallsThrough`] may be accused, so `Unknown` counts
///   as **terminating**: silence. It asks [`Self::provably_falls_through`].
/// * a future dead-code consumer (`UnreachableStatementRule` and the level-4
///   family) — the accusation is *"the statement after this one never runs"*.
///   Only [`Self::Terminates`] may be accused, so `Unknown` counts as **not
///   terminal**: silence. It asks [`Self::provably_terminates`].
///
/// Both predicates exist so that neither consumer is ever tempted to write
/// `!= Terminates` or `!= FallsThrough` — the two negations are exactly the
/// mistake this type exists to prevent, and each would invert the other
/// consumer's safe side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BodyEnd {
    /// Control provably never reaches the end: `return` / `throw` / `exit`, a
    /// branch construct whose every arm terminates, a loop with no exit edge.
    Terminates,
    /// Control provably *can* reach the end — there is a terminator-free
    /// syntactic path through the construct (an `if` with no `else`, a loop whose
    /// condition can be false, a plain statement).
    FallsThrough,
    /// Undecided: the construct's exit edges are not bounded by this judgment
    /// (`try`/`catch`/`finally`, `goto`/labels, an unstructurable `switch`, a
    /// provably-infinite loop containing a `break` whose target is unknown). Every
    /// consumer must name which way it reads this — see the type docs.
    Unknown,
}

impl BodyEnd {
    /// `true` only for [`Self::FallsThrough`] — the predicate `type.return-missing`
    /// asks. `Unknown` answers `false`: never accuse a body of running off its end
    /// when the analysis could not prove that it does.
    #[must_use]
    pub const fn provably_falls_through(self) -> bool {
        matches!(self, Self::FallsThrough)
    }

    /// `true` only for [`Self::Terminates`] — the predicate a dead-code consumer
    /// asks. `Unknown` answers `false`: never call a statement unreachable when the
    /// analysis could not prove that its predecessor terminates.
    #[must_use]
    pub const fn provably_terminates(self) -> bool {
        matches!(self, Self::Terminates)
    }

    /// Join the ends of the **alternative arms** of a branch construct (the arms of
    /// an `if`/`match`/`switch`, including the implicit no-match arm). The join is
    /// the arms' least upper bound in the order *every arm terminates* →
    /// *some arm provably falls through* → *undecided*:
    ///
    /// * every arm [`Self::Terminates`] ⇒ the construct terminates;
    /// * any arm [`Self::FallsThrough`] ⇒ the construct falls through (that arm is
    ///   a terminator-free path to the successor, whatever the others do);
    /// * otherwise (only `Terminates` and `Unknown` arms) ⇒ [`Self::Unknown`].
    ///
    /// An empty arm list answers [`Self::Terminates`], the identity: a construct
    /// with no arms at all offers no path to its successor. (Callers supply the
    /// implicit arm explicitly — an `if` with no `else` joins in a
    /// `FallsThrough`, a `match` with no `default` joins in a `Terminates`
    /// because PHP throws `\UnhandledMatchError` there.)
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

/// The terminality of an ordered statement list — the tail judgment the whole
/// foundation exists to answer (ADR-0078, issue #199). Reads each entry's
/// [`Stmt::end`], which the lowering already computed from the CST.
///
/// The fold is *not* "the last statement decides":
///
/// * the **first** [`BodyEnd::Terminates`] wins outright — everything after it is
///   unreachable, so the list terminates however the tail is written. This is what
///   makes `[try { … } catch { … }, return 1;]` answer `Terminates` rather than
///   inheriting the `try`'s `Unknown`;
/// * a [`BodyEnd::Unknown`] entry does **not** stop the scan; it is remembered.
///   Control may or may not reach past it, so the list's end is undecided *unless*
///   a later entry terminates unconditionally (see above);
/// * an empty list [`BodyEnd::FallsThrough`] — the identity — which is exactly
///   what makes an `if` with no `else` fall through.
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

/// Whether a statement list contains a **function exit** anywhere at all — a
/// `return`, a `throw`, an `exit`/`die` — however deeply nested, and whether or not
/// it is on a path [`body_end`] can see.
///
/// The second question of the foundation, and deliberately a *separate* one from
/// [`body_end`]. That answers "does control reach the end"; this answers "does the
/// body exit anywhere". Together they split a falling-through body into two
/// populations that the zero-FP policy floors differently (ADR-0078 §1.3, the
/// `maybe-` convention):
///
/// * **no exit anywhere** — a stub, an empty body, a body of pure side effects.
///   *Every* execution runs off the end, so the consequence is unconditional.
/// * **an exit somewhere, but not on every path** — the shape a `return` in every
///   taken arm with an uncovered escape edge produces (a no-`default` `switch`
///   whose every case returns, an `if` with no `else`, a loop that returns on a
///   match). The fall-through edge exists but may be taken only for inputs the
///   program never sees, which is exactly the definite/possibly boundary.
///
/// It reads [`Stmt::has_terminator`], which the lowering computed over the **whole
/// CST subtree** — so a `return` inside a `foreach` body, a `try` block or a
/// `switch` case counts, even though the trace IR erased those constructs into an
/// opaque node with nothing inside.
#[must_use]
pub fn body_has_terminator(stmts: &[Stmt]) -> bool {
    stmts.iter().any(|s| s.has_terminator)
}

// end reachability foundation (ADR-0078, issue #199)

/// A trace entry plus the local variables it feeds into a call.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Stmt {
    pub kind: StmtKind,
    /// The source span of the whole statement (set centrally by `lower_stmt`
    /// from the CST statement node; nested constructs' inner statements carry
    /// their own spans). Used by the walk to record proven-dead regions.
    pub span: Span,
    /// The variables this statement passes as an argument to *any* call, one
    /// entry per name, each carrying the evidence ADR-0070's by-value gate
    /// consults. The checker marks every entry's name unknown *after* the
    /// statement — PHP by-reference parameters could mutate them, so a value
    /// can't be trusted past a call it was handed to (conservatively covering
    /// unseen `&$x` signatures). That blanket drop is the sound floor, and the
    /// name set here is complete; an entry whose every occurrence is a provable
    /// by-value site is the one thing the walk may excuse from it, and the
    /// entry itself now says which it is.
    pub invalidated: Vec<InvalidatedVar>,
    /// Every place this statement puts a value into PHP's **string context**
    /// (ADR-0078, issue #193) — an `echo`/`print` operand, an interpolated
    /// string's embedded expressions, a `(string)` cast, and both operands of a
    /// `.` concatenation. Collected centrally by `lower_stmt` from the statement's
    /// own expressions, so a consumer reads one list instead of re-deriving five
    /// syntactic shapes; the walk judges each site against the statement's ENTRY
    /// env, which is where PHP evaluates them.
    ///
    /// Empty for every statement kind whose expressions are not collected — see
    /// `string_context_sites` for the boundary and why it is where it is.
    pub string_contexts: Vec<StringContextSite>,
    /// Where this statement leaves control (ADR-0078, issue #199) — the
    /// per-statement half of the reachability foundation, and the only thing
    /// [`body_end`] reads.
    ///
    /// Computed centrally by `lower_stmt` from the **CST**, not from
    /// [`Self::kind`]: the trace IR erases `while`/`for`/`try`/`switch` into an
    /// undifferentiated [`StmtKind::Opaque`] and `goto` into a
    /// [`StmtKind::Barrier`], so a judgment derived from the IR alone could never
    /// tell a `while (true)` (no exit edge) from a `foreach` (always one) nor a
    /// `goto` (unbounded) from a `global $x;` (plainly falls through). The CST
    /// still has all of it in hand at lowering time, which is why the answer is
    /// computed there and carried here.
    ///
    /// Deliberately independent of [`Self::kind`]: a `break;` is a
    /// [`StmtKind::Barrier`] whose `end` is [`BodyEnd::Terminates`] (control
    /// leaves the enclosing list), and a `while (true) {}` is a
    /// [`StmtKind::Opaque`] whose `end` is [`BodyEnd::Terminates`] too. Nothing
    /// may infer one field from the other.
    pub end: BodyEnd,
    /// Whether this statement's **whole CST subtree** contains a function exit — a
    /// `return`, a `throw`, an `exit`/`die` (ADR-0078 §5, issue #199). Nested
    /// function-likes are not counted: their exits are their own scope's.
    ///
    /// Orthogonal to [`Self::end`], and that is the point. `end` asks *does control
    /// reach past this statement*; this asks *does the subtree exit the function
    /// anywhere*. `if ($c) { return 1; }` answers `FallsThrough` and `true`;
    /// `foreach ($xs as $x) { $s .= $x; }` answers `FallsThrough` and `false`. The
    /// pair is what splits a falling-through body into the unconditional and the
    /// conditional class — see [`body_has_terminator`].
    ///
    /// Computed from the CST for the same reason `end` is: the trace IR erases a
    /// loop body, a `try` block and an unstructured `switch` case entirely, so a
    /// `return` inside any of them is invisible from the IR alone.
    pub has_terminator: bool,
}

impl Stmt {
    /// A statement **under construction**: its kind and by-ref evidence, with the
    /// span, the string-context sites and the terminality left for `lower_stmt` to
    /// fill in centrally (`span` is [`ZERO_SPAN`] until then, the invariant that
    /// function documents).
    ///
    /// `end` starts at [`BodyEnd::FallsThrough`] — the value that is correct for
    /// every straight-line statement and therefore the one whose *absence* of a
    /// central fill would be least surprising; the two lowering paths that bypass
    /// `lower_stmt` (`lower_arm_body`, the arrow-function body) set it themselves.
    fn lowered(kind: StmtKind, invalidated: Vec<InvalidatedVar>) -> Stmt {
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

/// One value a statement hands to PHP's string conversion (ADR-0078, issue #193):
/// the lowered operand, the span to report at, and which syntactic context it is.
///
/// The syntax layer decides nothing here — it knows no values. What it owns is
/// **where the conversions are**; whether a given one is legal (`int`, `null` and
/// every other scalar are), a warning (an array) or a fatal (an object with no
/// reachable `__toString`) is the inference layer's judgement.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StringContextSite {
    /// The converted operand, lowered by the same rules as a call argument. An
    /// operand the lowering cannot spell arrives as [`ArgValue::Other`], which
    /// proves nothing and is therefore silence — never a manufactured finding.
    pub value: ArgValue,
    /// The span of the operand (not of the enclosing construct): the text to fix.
    pub span: Span,
    /// Which conversion this is, for the finding's own words.
    pub kind: StringContextKind,
}

/// The syntactic form of a [`StringContextSite`]. PHP converts identically in all
/// of them — the distinction exists so a finding can name the construct the reader
/// is looking at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StringContextKind {
    /// An expression embedded in a double-quoted string or heredoc (`"x $v"`,
    /// `"{$v}"`), and in a backtick shell-exec string, which converts the same way.
    Interpolation,
    /// An `echo` operand, including the `<?= … ?>` short-tag form.
    Echo,
    /// A `print` operand.
    Print,
    /// A `(string)` cast.
    Cast,
    /// Either operand of `.`, and both sides of `.=` — a compound concatenation
    /// reads its target in string context exactly as the binary form does.
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

/// One local variable a statement hands to a call — the name plus the ADR-0070
/// evidence for it. The syntax layer decides nothing with the evidence (it
/// knows no signatures); what it owns is the **completeness guarantee** the
/// walk relies on: `sites` lists EVERY occurrence of the name in the
/// statement's call arguments, or `opaque` is set and `sites` is empty. There
/// is no third state, so a consumer cannot mistake missing evidence for "no
/// evidence needed".
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InvalidatedVar {
    /// The local variable's name, without the leading `$`.
    pub name: String,
    /// At least one occurrence is unprovable (method / static / dynamic callee,
    /// named or spread args, closure-body occurrence, echo-embedded write), so
    /// no protection may be granted however the provable sites resolve.
    pub opaque: bool,
    /// The provable occurrences: callee reference + 0-based argument position.
    /// The callee is the same [`NameRef`] a [`CallExpr`] carries, for
    /// project-wide resolution.
    pub sites: Vec<(NameRef, u32)>,
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

/// Classification of a written return type hint (`: T`), independent of whether
/// Steins lowers `T` to a [`NativeType`]. Used by return-fact summary fallthrough
/// (ADR-0075): only a fully **untyped** declaration (no hint at all) contributes
/// PHP's implicit `return null`; `void` / `never` / any other written hint do not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RetHintKind {
    /// `: void`
    Void,
    /// `: never`
    Never,
    /// Any non-void/never hint — scalar, class, `array`, union, …
    Other,
}

/// A written return type hint (`: T`) — its [`RetHintKind`] classification plus
/// the **source span of the hint itself** (the `T`, not the colon).
///
/// The span exists so a diagnostic about the declaration can quote the type
/// exactly as the author wrote it ([`SourceTree::text_at`]): `: array` and
/// `: mixed` lower to no [`NativeType`] at all, so `ret`/[`Scope::ret_ty`] cannot
/// name them, yet PHP's own `TypeError` does — `Return value must be of type
/// array, none returned`. Kind and span travel together in one struct so they
/// cannot drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RetHint {
    pub kind: RetHintKind,
    /// The hint's own file byte span; `SourceTree::text_at` maps it back to text.
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
    /// The written return type hint, if any — its kind and its source span (see
    /// [`RetHint`]). `None` means untyped (no `: T`). Distinct from
    /// [`Self::ret_ty`] / the decl's lowered `ret`, which collapse
    /// void/never/object/array to silence for the return-type check.
    pub ret_hint: Option<RetHint>,
    /// `true` when the body contains `yield` / `yield from` — the call result is
    /// a `Generator`, not the value of a trailing `return` (ADR-0057 §5). Return
    /// summaries refuse such scopes.
    pub is_generator: bool,
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
    /// Adopted docblock of a closure/arrow scope ([`ScopeOwner::Closure`]) —
    /// a closure has no [`FunctionDecl`] to adopt one on, so the `@return`
    /// phpdoc lane (issue #128) reads it here. Adoption is the shared
    /// whitespace-gap discipline (ADR-0029) in two positions, inline winning
    /// over statement-level; see `adopt_closure_docblock`. `None` for a closure
    /// with no adopted docblock and for every non-closure scope (functions and
    /// methods adopt on their decls).
    pub docblock: Option<String>,
    /// The by-value `use ($x)` captures of a `function (…) use (…) {…}` scope
    /// whose name the closure body **never mentions** (issue #186, the
    /// `closure.unused-use` mechanics id). A syntactic fact, computed at lowering
    /// where the CST is still in hand.
    ///
    /// Three deliberate silences make the list a safe firing set:
    ///
    /// * A by-ref `use (&$x)` capture is **never** listed — it is an out-channel
    ///   (the closure writes through it), so "unread" says nothing about it. It
    ///   is recorded as an [`OpaqueConstruct::ByRefCapture`] site instead.
    /// * A *mention* — not a read — clears a capture: any `$x` token anywhere in
    ///   the body subtree counts, **including inside nested closures and their
    ///   own `use ($x)` clauses**, which the scope-local walks stop at.
    ///   Over-collection only removes entries, so it is the safe direction.
    /// * The whole list is empty when the body holds a construct that can consume
    ///   a name without spelling it (`compact` / `extract` / `get_defined_vars` /
    ///   `$$x` / `eval` / `include`) — the scope-local dam.
    ///
    /// Empty for arrow functions (their captures are *derived* from the body, so
    /// an unused one cannot exist) and for every non-closure scope.
    pub unused_captures: Vec<UnusedCapture>,
    // undefined variables (ADR-0078, issue #194)
    /// Every read of a name this scope **never binds** — the firing set of
    /// `variable.undefined` (issue #194). A syntactic fact, computed at lowering
    /// where the CST is still in hand, exactly as [`Self::unused_captures`] is.
    ///
    /// The proof is closed-world over one scope's own text and deliberately
    /// **ordering-blind**: a name that appears as a binding form *anywhere* in the
    /// scope — before, after, or in a branch that never runs — is bound here. A read
    /// that precedes its only assignment is therefore silence; that is
    /// `variable.maybe-undefined`'s territory and waits on the reachability
    /// foundation (issue #199).
    ///
    /// The list is empty — never merely shorter — in four cases, each a place where
    /// the closed world does not hold:
    ///
    /// * **Top-level script scopes.** `include` splices the *including* scope's whole
    ///   symbol table into the included file's top level, so a file's own text can
    ///   never prove a top-level name unbound (the template-partial idiom). No
    ///   function-like scope has that channel: a call frame starts empty.
    /// * **Arrow-function scopes.** `fn () => $x` auto-captures every free variable
    ///   from the enclosing scope, so an arrow body's reads are the *enclosing*
    ///   scope's question, not this one's (witnessed: `$x = 3; fn () => $x + 1` is
    ///   silent at 8.5.9).
    /// * **A scope carrying a name dam** — `extract` / `compact` /
    ///   `get_defined_vars` / `$$x` / `${…}` / `eval` / `include` / `require`. The
    ///   same scope-local dam [`Self::unused_captures`] uses, for the same reason:
    ///   each can consume or mint a binding without spelling it.
    /// * A read whose name is a superglobal, `$this`, or `$http_response_header`
    ///   (the engine binds all three), which is filtered at collection.
    ///
    /// `isset($x)` / `empty($x)` / `$x ?? d` / `unset($x)` / `@$x` reads are excluded
    /// at collection too, but for a different reason: PHP *legalizes* them (witnessed
    /// silent at 8.5.9), so they are not this finding at all (ADR-0078 §3 defers the
    /// pointless-guard reading entirely).
    ///
    /// One residue the syntax side cannot settle is left for the checker: a bare
    /// `$x` passed to a **statically-named function** may be an out-parameter, and
    /// whether the callee declares `&$p` needs the cross-file index. Those reads are
    /// collected here and subtracted in `steins-infer` (ADR-0077's by-value oracle).
    /// Every *other* call shape — method, static, dynamic, `new`, and every named
    /// argument — binds its bare-variable arguments right here, since no
    /// name is even available to resolve.
    pub undefined_reads: Vec<UndefinedRead>,
    /// The reads of names *some* paths through this scope reach unbound (ADR-0081,
    /// issue #267) — the firing set of `variable.maybe-undefined`, and **disjoint
    /// from [`Self::undefined_reads`] by construction**: a name the scope binds
    /// nowhere is the definite id's, a name it binds somewhere can only be this
    /// one's.
    ///
    /// Produced by the binding-presence pass, which walks the same statements in
    /// program order over a three-valued lattice, subtracts a branch arm that
    /// provably terminates, iterates loop bodies to a fixpoint and consumes
    /// `isset`/`empty` guards with polarity. Every silence [`Self::undefined_reads`]
    /// documents is inherited verbatim — the name dams, the top-level and
    /// arrow-function scopes, the guard exclusions, the superglobals — plus one of
    /// its own: a `goto` or a label anywhere in the scope dams the pass, because a
    /// jump to an arbitrary label is an exit edge the traversal cannot bound.
    ///
    /// The same [`Self::ref_arg_candidates`] residue is left for the checker here,
    /// with one refinement the definite id does not need: an out-parameter binds
    /// from its **call site forward**, so a candidate subtracts only the reads that
    /// follow it.
    pub maybe_undefined_reads: Vec<UndefinedRead>,
    /// Every bare-variable positional argument at a **function call** in this scope
    /// — the out-parameter candidates [`Self::undefined_reads`] cannot settle alone,
    /// since whether `f($x)` writes `$x` is a property of `f`'s declaration and needs
    /// the cross-file index.
    ///
    /// Recorded **independently of [`Self::undefined_reads`]**, and deliberately so:
    /// an out-parameter is a *binding form*, and a binding must not depend on whether
    /// its argument occurrence happened to be collected as a read. It is not —
    /// `@proc_open($cmd, $spec, $pipes)` sits inside an error-control guard, so the
    /// `$pipes` occurrence is withheld from the read list while PHP binds `$pipes`
    /// exactly as it would without the `@` (symfony/console `Terminal.php`, the
    /// corpus site that made the point).
    ///
    /// Only plain function calls appear here. Every other call shape — method,
    /// static, dynamic, `new`, and every named argument — has no callee name to
    /// resolve, so it binds its bare-variable arguments outright at lowering.
    /// Empty whenever [`Self::undefined_reads`] is (a dammed or non-reporting
    /// scope has nothing to subtract from).
    pub ref_arg_candidates: Vec<UndefinedRead>,
    // end undefined variables (ADR-0078, issue #194)
}

/// One by-value `use ($x)` capture a closure body never mentions — an entry of
/// [`Scope::unused_captures`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
pub struct UndefinedRead {
    /// The read name without the leading `$`.
    pub name: String,
    /// The file byte span of the `$x` token at the **read**, which is also the
    /// span `lower_argument_list` records for a bare-variable positional argument
    /// — the join the checker's out-parameter subtraction keys on.
    pub span: Span,
}

// end undefined variables (ADR-0078, issue #194)

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
    /// is *not* a dam site. The finding-breadth dam treats this as a dam site;
    /// transform obstacle detection does not (ADR-0049).
    ClassAlias,
    /// A `define(...)` call with a **runtime-minted** name argument (ADR-0078,
    /// issue #198) — a computed name mints a *constant* nobody can enumerate, the
    /// exact parallel to [`Self::ClassAlias`] on the class side. A `define()` whose
    /// name is a string literal instead contributes a [`GlobalConstDecl`] (see
    /// [`SourceTree::global_const_decls`]) and is *not* a dam site.
    ///
    /// Damming is deliberately narrower than "any dynamism": this kind is read by
    /// the `constant.undefined` ladder only, because only constant existence is put
    /// in doubt by it — `define()` cannot mint a function or a class.
    DefineDynamic,
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
/// Unlike [`OpaqueConstruct`], this does not poison a scope or dam a claim.
/// **The list is a guess until measured**: it contains shapes named by a
/// cross-analyzer survey, recognized syntactically and therefore both
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
/// site. Folded into the project index with textual declarations, sharing the
/// duplicate-declaration ambiguity discipline: an alias colliding with a textual
/// declaration of the same FQN, or
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

/// A **global constant declaration** (ADR-0078, issue #198): a `const FOO = …;`
/// statement outside any class-like — resolved against its enclosing namespace,
/// exactly like a function declaration — or a `define('FOO', …)` call whose name
/// argument is a string literal. Class constants are a different namespace
/// entirely (they live on [`ClassDecl::consts`] and are issue #197's territory)
/// and never appear here.
///
/// The two forms differ in one way that matters and is honoured below: a `const`
/// statement declares into the *current* namespace, while `define()` always takes
/// an absolute name — `define('FOO', 1)` inside `namespace App;` declares the
/// **global** `FOO`, not `App\FOO` (`php -r`-witnessed).
///
/// A `define()` whose name argument is *not* a literal declares a name no scan can
/// read; it contributes a [`DynamismKind::DefineDynamic`] dam site instead, the
/// same split [`ClassAliasEdge`] makes for `class_alias`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GlobalConstDecl {
    /// The declared name, normalized by [`normalize_const_fqn`]: leading `\`
    /// stripped, namespace segments lowercased, the final segment case-preserved.
    pub fqn: String,
    /// The declaration's source span.
    pub span: Span,
}

/// Normalize a global constant name into the index's matching key: leading `\`
/// stripped, every **namespace** segment lowercased, the final segment left
/// exactly as written.
///
/// This split is PHP's, not a convenience. Constant names are case-sensitive —
/// `define('Foo', 1)` leaves `defined('FOO')` false — while the namespace prefix
/// they sit under is case-insensitive like every other namespace: with
/// `namespace App; const LOCAL = 'l';`, both `defined('App\LOCAL')` and
/// `defined('app\LOCAL')` are true and `defined('App\local')` is false (all three
/// `php -r`-witnessed on 8.5.9). The case-insensitive third argument to `define()`
/// died in PHP 8.0 and the workspace floor is 8.1 (ADR-0011), so there is no
/// version fork here: constants are case-sensitive, full stop.
#[must_use]
pub fn normalize_const_fqn(name: &str) -> String {
    let name = name.trim_start_matches('\\');
    match name.rfind('\\') {
        Some(pos) => format!("{}{}", name[..=pos].to_ascii_lowercase(), &name[pos + 1..]),
        None => name.to_owned(),
    }
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

/// One `foreach` statement, lowered for the loop→`array_map` transform
/// (ADR-0076). **Every** `foreach` in a file produces one of these — the
/// transform's candidate domain is the whole construct family, so its refusal
/// distribution measures how narrow v1 is instead of hiding it (ADR-0076 §4).
///
/// Syntax only reports *shape*. Whether the body is pure, whether the subject
/// proves `array`/`is_list`, and therefore whether the rewrite is legal are all
/// inference questions answered elsewhere; nothing here decides a rewrite.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
    /// The value target when it is a plain `$var` (no `$`); `None` for a
    /// destructuring (`as [$a, $b]` / `as list($a, $b)`) or otherwise
    /// non-variable target.
    pub value_var: Option<String>,
    /// The body's lowered shape.
    pub body: ForeachBodyShape,
    /// The immediately preceding sibling statement, when the `foreach` is not the
    /// first statement of its block.
    pub prev_stmt: Option<PrevStmt>,
    /// The end offset of the enclosing **variable** scope (the function / method /
    /// closure body, else the file), so a consumer can scan the remainder of the
    /// scope for an iteration variable that outlives the loop (ADR-0076 §3).
    pub scope_end: u32,
}

/// The statement immediately preceding a [`ForeachSite`], reduced to what the
/// adjacency rule needs: its span (the rewrite consumes it) and whether it is an
/// accumulator initializer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PrevStmt {
    /// The preceding statement's own span.
    pub span: Span,
    /// The assignment target when the statement is `$name = <anything>;` with the
    /// plain `=` operator and a bare variable lvalue; `None` otherwise.
    pub assign_target: Option<String>,
    /// `true` when that assignment's right-hand side is an **empty array
    /// literal** (`[]` or `array()`).
    pub assigns_empty_array: bool,
}

/// A [`ForeachSite`] body's lowered shape.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ForeachBodyShape {
    /// How many statements the body holds (`0` for `{}` / a bare `;`).
    pub stmt_count: usize,
    /// The append, when the body is exactly one `$acc[] = <expr>;` statement.
    pub append: Option<AppendStmt>,
    /// `true` when the body contains a `break`, `continue`, `return` or `goto`
    /// outside any nested function-like body — the loop can end early, so no
    /// whole-array rewrite reproduces it.
    pub early_exit: bool,
}

/// The single `$acc[] = <expr>;` statement of an eligible loop body.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AppendStmt {
    /// The accumulator variable's name (no `$`).
    pub acc: String,
    /// The appended expression's span — the arrow function's body, verbatim.
    pub value_span: Span,
    /// Every direct variable the appended expression mentions, in first-seen
    /// order (nested function-like bodies included: an arrow function's body
    /// reads the enclosing scope by value, so its names are the loop's names).
    pub value_vars: Vec<String>,
    /// `true` when the appended expression **writes** a variable (an embedded
    /// assignment, `++`, or `--`). No effect label covers a function-local write,
    /// but `fn` captures by value, so such an expression is not equivalence-
    /// preserving under the rewrite.
    pub value_writes: bool,
    /// `true` when the appended expression carries a construct the effect scan
    /// does not model as a call: `new`, `clone`, `yield`, a backtick shell
    /// execute, an ADR-0001 poison construct, or one of the scope-sensitive
    /// builtins (`compact`, `get_defined_vars`, `func_get_args`,
    /// `func_num_args`) whose meaning changes inside an arrow function's scope.
    pub value_unmodelled: bool,
}

// invalid operands (ADR-0078, issue #191)

/// One **operator application** whose operand types PHP's own arithmetic table
/// can refuse: an arithmetic, bitwise or shift binary operator, or a unary
/// `-`/`+`/`~` (issue #191). Every such application in the file produces one of
/// these, with both operands lowered to [`ArgValue`] — the value IR is
/// where a literal `[]` and a `$var` carrying an env fact are spelled the same
/// way, so the `type.invalid-operand` judge reads one shape.
///
/// Purely syntactic, like [`ForeachSite`]: nothing here decides a finding. What
/// the operands *are* is a value-domain question answered in `steins-infer`
/// against the enclosing statement's entry env.
///
/// Three operator families are deliberately **absent** (each a witnessed
/// non-fatal or another id's territory, see the judge's table):
///
/// * `.` — concatenation is issue #193's id (`string.non-stringable` /
///   `string.array-conversion`); array-in-concat is a *warning*, not a fatal;
/// * every comparison (`< > <= >= <=> == != === !== <>`) — `php -r`-witnessed at
///   8.5.9 to be legal for **every** operand pair, arrays and objects included,
///   so `InvalidComparisonOperationRule` contributes no fatal row at all;
/// * `++`/`--` — fatal on an array (`Cannot increment array`), but a mutation
///   statement rather than an operand expression; out of v1's reach.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OperandSite {
    /// The whole operator application's span (`$a + $b`, `-$a`) — where the
    /// finding points.
    pub span: Span,
    /// The operator and its lowered operands.
    pub kind: OperandSiteKind,
    /// The span of the innermost enclosing **function-like body** (function,
    /// method, closure, arrow function, property hook), or `None` at file
    /// scope.
    ///
    /// This is the one field that is not about the operator: it keeps a site
    /// inside a closure from being judged against the *enclosing* scope's env.
    /// A statement `$f = fn() => $s + 1;` textually contains the `$s + 1` site,
    /// but `$s` there is the closure's own binding, so the consumer requires
    /// the statement it judges at to itself lie inside this body.
    pub enclosing_body: Option<Span>,
}

/// The operator half of an [`OperandSite`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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

/// The binary operators whose operand types PHP's arithmetic table can refuse.
/// `.`, `??`, the comparisons and the logical operators are not here (see
/// [`OperandSite`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

/// The unary prefix operators whose operand type PHP can refuse. `!` is not
/// here — it is total (`php -r`-witnessed legal on every operand kind,
/// including arrays and objects).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

/// An owned, Mago-free lowering of one parsed PHP file.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceTree {
    strict_types: bool,
    functions: Vec<FunctionDecl>,
    classes: Vec<ClassDecl>,
    calls: Vec<CallExpr>,
    scopes: Vec<Scope>,
    /// Dynamic-code constructs (eval / include / require) found file-wide
    /// (ADR-0046 §2), used for caller-enumeration obstacle detection.
    dynamism: Vec<DynamismSite>,
    /// Compile-time `class_alias('Target', 'Alias')` edges found file-wide (ADR-0049
    /// §2). Folded into the project index for existence resolution; a `class_alias`
    /// naming a runtime-minted class is a [`DynamismKind::ClassAlias`] dam site in
    /// [`Self::dynamism`] instead.
    class_alias_edges: Vec<ClassAliasEdge>,
    /// Anonymous-class inheritance edges found file-wide (ADR-0049 A4), used by
    /// declared-receiver descendant closure to detect invisible descendants.
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
    /// Whether this file declares a userland twin of one of the modeled `PREG_*`
    /// flag constants (issue #168) — a `const PREG_SET_ORDER = …;` in any
    /// namespace, or a `define('…PREG_SET_ORDER', …)` with a literal name. Same
    /// deliberately name-only, project-conservative reading as
    /// [`Self::php_version_id_declared`]: one such declaration anywhere disables
    /// the engine-constant flags resolution for the whole project.
    preg_flag_const_declared: bool,
    /// Whether this file `use const`-imports something under the alias of one of
    /// the modeled `PREG_*` flag constants (issue #168). File-scoped, exact-case,
    /// mirroring [`Self::php_version_id_aliased`]: an unqualified `PREG_SET_ORDER`
    /// in such a file names the import, not the engine constant.
    preg_flag_const_aliased: bool,
    /// Every `foreach` statement in the file, lowered to its transform-relevant
    /// shape (ADR-0076). Read by the loop→`array_map` transform and nothing else;
    /// no finding consults it.
    foreach_sites: Vec<ForeachSite>,
    /// Every literal array expression in the file (`[...]` / legacy `array(...)`),
    /// in source order (issue #187). Read by the `array.duplicate-key` per-file
    /// pass.
    array_literal_sites: Vec<ArrayLiteralSite>,
    // invalid operands (ADR-0078, issue #191)
    /// Every arithmetic/bitwise/shift operator application in the file, in
    /// source order, with both operands lowered. Read by the
    /// `type.invalid-operand` judge and nothing else.
    operand_sites: Vec<OperandSite>,
    // end invalid operands (ADR-0078, issue #191)
    /// Class references at the positions that break at run time (ADR-0049 §5 / S4,
    /// widened by issue #182), read by the `class.undefined` per-file pass.
    hard_class_refs: Vec<NameRef>,
    // member absence (ADR-0078, issue #197)
    /// Every property name written anywhere in this file, and whether any write
    /// went through a computed name. See [`SourceTree::property_write_names`].
    property_writes: PropertyWrites,
    // end member absence (ADR-0078, issue #197)
    /// Every **global constant declaration** in the file (ADR-0078, issue #198) —
    /// `const FOO = …;` outside a class-like, and `define('FOO', …)` with a literal
    /// name. The project-index evidence leg of the `constant.undefined` ladder.
    global_const_decls: Vec<GlobalConstDecl>,
    /// Every **bare constant fetch** in the file (`FOO`, `\FOO`, `Ns\FOO`), in
    /// source order (ADR-0078, issue #198), read by the `constant.undefined`
    /// per-file pass. `X::CONST` is a class constant — a different namespace, issue
    /// #197's — and never appears here; nor do `true`/`false`/`null` or the magic
    /// `__LINE__` family, which the grammar lowers as other node kinds.
    const_refs: Vec<NameRef>,
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
        // The lowering walkers below recurse once per CST node, so the depth of
        // the deepest expression in `source` is a stack cost. Where headroom can
        // be bought the entry point buys it (issue #246) and this guard is off;
        // where it cannot — the wasm playground, whose shadow stack is fixed at
        // link time — the guard stops the walk while the machine can still
        // report, and the refusal is appended to `parse_errors` below.
        let guard = stack_guard::Scope::enter();
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
        let scopes = lower_scopes(program, &contexts, &regions, &docs);

        // Every `foreach` in the file, lowered to its transform-relevant shape
        // (ADR-0076 §4: the candidate domain is the whole construct family). The
        // file itself is the outermost variable scope.
        let mut foreach_sites = Vec::new();
        collect_foreach_sites(
            &Node::Program(program),
            source.len().try_into().unwrap_or(u32::MAX),
            &mut foreach_sites,
        );

        // Every literal array expression in the file, in source order (issue
        // #187): purely syntactic keys-and-spans, the `array.duplicate-key`
        // check's whole evidence.
        let mut array_literal_sites = Vec::new();
        collect_array_literal_sites(&Node::Program(program), &mut array_literal_sites);

        // Every arithmetic/bitwise/shift operator application in the file
        // (ADR-0078, issue #191), with both operands lowered to the value IR.
        // File scope carries no enclosing function-like body.
        let mut operand_sites = Vec::new();
        collect_operand_sites(&Node::Program(program), None, &mut operand_sites);

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

        let mut parse_errors: Vec<ParseError> = program
            .errors
            .iter()
            .map(|e| ParseError { message: e.to_string(), span: to_span(e.span()) })
            .collect();

        // A refused walk is a recovered parse error like any other: the checker
        // names it once as `syntax.unparsable` (ADR-0079) — the same vocabulary
        // Mago's own `MAX_RECURSION_DEPTH` already surfaces through — and dams
        // the file's other findings, which is the honest reading of a tree that
        // was only partly lowered. The refusal is about the file (it carries no
        // line, for the reason `stack_guard::REFUSAL` records), so it goes first.
        if guard.tripped() {
            let span = Span { start: 0, end: 0 };
            parse_errors.insert(0, ParseError { message: stack_guard::REFUSAL.to_owned(), span });
        }
        drop(guard);

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
            preg_flag_const_declared: lowered.preg_flag_const_declared,
            preg_flag_const_aliased: lowered.preg_flag_const_aliased,
            foreach_sites,
            array_literal_sites,
            operand_sites,
            hard_class_refs: lowered.hard_class_refs,
            // member absence (ADR-0078, issue #197)
            property_writes: lowered.property_writes,
            // end member absence (ADR-0078, issue #197)
            global_const_decls: lowered.global_const_decls,
            const_refs: lowered.const_refs,
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
    /// Class references at the positions verified to break at run time (ADR-0049
    /// §5 / S4, widened by issue #182): the hard-error expressions (`new X`,
    /// `X::m()`, `X::CONST`, `X::$prop`), inheritance (`extends` / `implements` /
    /// `use <Trait>`), `catch (X $e)`, and parameter / return / property native
    /// type declarations. Consumed by the `class.undefined` per-file pass;
    /// `self`/`static`/`parent`, dynamic classes, `X::class`, `instanceof` and the
    /// docblock positions are excluded at collection.
    #[must_use]
    pub fn hard_class_refs(&self) -> &[NameRef] {
        &self.hard_class_refs
    }

    /// Every **global constant declaration** the file makes (ADR-0078, issue #198):
    /// `const FOO = …;` outside a class-like (resolved against its namespace) and
    /// `define('FOO', …)` with a literal name (always absolute). Folded into the
    /// project index as the textual half of the `constant.undefined` evidence.
    ///
    /// Conditionality is deliberately *not* recorded: the `if (!defined('X'))
    /// define('X', …)` idiom declares `X` for the purposes of an absence proof
    /// exactly as an unconditional `define` does — the branch exists precisely so
    /// the constant ends up defined either way.
    #[must_use]
    pub fn global_const_decls(&self) -> &[GlobalConstDecl] {
        &self.global_const_decls
    }

    /// Every **bare constant fetch** in the file (`FOO`, `\FOO`, `Ns\FOO`), in
    /// source order (ADR-0078, issue #198) — the finding-position set of the
    /// `constant.undefined` per-file pass, exactly as [`Self::hard_class_refs`] is
    /// for `class.undefined`. `X::CONST` (issue #197's class-constant namespace),
    /// `true`/`false`/`null` and the `__LINE__` magic family are excluded at
    /// collection, so the list IS the verified finding-position set.
    #[must_use]
    pub fn const_refs(&self) -> &[NameRef] {
        &self.const_refs
    }

    /// Every `foreach` statement in the file, in source order, lowered to the
    /// shape the loop→`array_map` transform enumerates (ADR-0076 §4). Purely
    /// syntactic: no purity, no value fact, no rewrite decision lives here.
    #[must_use]
    pub fn foreach_sites(&self) -> &[ForeachSite] {
        &self.foreach_sites
    }

    /// Every literal array expression in the file, in source order (issue
    /// #187). Purely syntactic — no value lowering, no inference — the whole
    /// evidence the `array.duplicate-key` per-file pass reads.
    #[must_use]
    pub fn array_literal_sites(&self) -> &[ArrayLiteralSite] {
        &self.array_literal_sites
    }

    // member absence (ADR-0078, issue #197)
    /// Every property name this file writes, deduplicated, in source order
    /// (ADR-0078, issue #197). Purely syntactic — no receiver is resolved — and
    /// read project-wide as the dynamic-property obstacle for
    /// `property.undefined`.
    ///
    /// The over-approximation is the point. A write is what creates a dynamic
    /// property, so a class may declare nothing named `p` and still answer `$o->p`
    /// at runtime because some other file did `$o->p = 1` first — deprecated on a
    /// plain class since PHP 8.2, deprecated and not an error, with the read that
    /// follows clean (witnessed at 8.5.9). Resolving *which* object each write
    /// lands on is the deferred `property.dynamic-write` slice's problem, and a
    /// name-keyed obstacle needs none of it: it costs the absence claims for names
    /// the project assigns somewhere — exactly the names a dynamic write could have
    /// created — while the typo shape the check exists for (`$user->emial`) is
    /// written nowhere and survives.
    #[must_use]
    pub fn property_write_names(&self) -> &[String] {
        &self.property_writes.names
    }

    /// Whether this file writes a property through a **computed** name
    /// (`$o->$n = …`, ADR-0078, issue #197). Such a write can create any name at
    /// all, so one anywhere in the project takes `property.undefined` off the
    /// surface entirely.
    #[must_use]
    pub fn writes_computed_property_name(&self) -> bool {
        self.property_writes.dynamic
    }
    // end member absence (ADR-0078, issue #197)

    // invalid operands (ADR-0078, issue #191)
    /// Every arithmetic/bitwise/shift operator application in the file, in
    /// source order (ADR-0078, issue #191) — ordered by span start, so a
    /// consumer may binary-search the sites a statement's span covers.
    /// Purely syntactic: the operands are lowered, never resolved.
    #[must_use]
    pub fn operand_sites(&self) -> &[OperandSite] {
        &self.operand_sites
    }
    // end invalid operands (ADR-0078, issue #191)

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

    /// Whether this file declares a userland twin of a modeled `PREG_*` flag
    /// constant (issue #168) — see the field docs for the project-wide consequence.
    #[must_use]
    pub fn preg_flag_const_declared(&self) -> bool {
        self.preg_flag_const_declared
    }

    /// Whether this file `use const`-imports the alias of a modeled `PREG_*` flag
    /// constant (issue #168) — file-scoped; an unqualified reference here is the
    /// import.
    #[must_use]
    pub fn preg_flag_const_aliased(&self) -> bool {
        self.preg_flag_const_aliased
    }

    /// The comment trivia found in the file, in source order (ADR-0023 inline
    /// `@steins-ignore` channel). Whitespace trivia is not included.
    #[must_use]
    pub fn comments(&self) -> &[Comment] {
        &self.comments
    }

    /// The `/** … */` docblock immediately preceding `stmt_start` — only
    /// whitespace between its end and `stmt_start` — or `None`. The
    /// statement-level analogue of the declaration adoption rule (ADR-0029's
    /// "only whitespace between"), consumed by the inline-`@var` cast seeding
    /// (ADR-0073): a docblock that leads a trace statement speaks about that
    /// statement. A non-doc comment sitting in the gap breaks the adjacency,
    /// exactly as any code would.
    #[must_use]
    pub fn stmt_docblock(&self, stmt_start: u32) -> Option<&Comment> {
        // Comment trivia are recovered in source order and never overlap, so
        // `span.end` is monotone and the nearest preceding one is binary-searchable.
        let idx = self.comments.partition_point(|c| c.span.end <= stmt_start).checked_sub(1)?;
        let c = &self.comments[idx];
        if c.kind != CommentKind::DocBlock {
            return None;
        }
        let gap = self.text.get(c.span.end as usize..stmt_start as usize)?;
        gap.chars().all(char::is_whitespace).then_some(c)
    }

    // untyped surface (ADR-0078, issue #200)
    /// The exact source text a file byte [`Span`] covers, or `None` when the span
    /// is out of range or does not land on `char` boundaries.
    ///
    /// The lowered tree records spans, not spellings, for everything whose
    /// *spelling* is not itself a modeled fact. This is the one accessor that
    /// turns such a span back into text — used by the declaration-reading
    /// `untyped.*` family to tell an `array` hint from an `int` one, which no
    /// lowered [`NativeType`] can express (both model to `None` and `Some`
    /// respectively for unrelated reasons).
    #[must_use]
    pub fn source_slice(&self, span: Span) -> Option<&str> {
        self.text.get(span.start as usize..span.end as usize)
    }
    // end untyped surface (ADR-0078, issue #200)

    /// Whether a docblock trivium ending at `doc_end` is followed by **nothing an
    /// adoption rule could attach it to** — the negative side of
    /// [`Self::stmt_docblock`]'s grammar, and the ADR-0029 declaration grammar's
    /// too (issue #186, the `phpdoc.misplaced-var` mechanics id).
    ///
    /// Deliberately answered from the *text*, not from the lowered trace: a
    /// statement inside a construct the trace keeps opaque (a loop body, a `try`,
    /// a `switch` arm) has no [`Stmt`] to query, so enumerating adopters would call
    /// a perfectly ordinary annotation misplaced. What this asks instead is
    /// whether any construct can follow **at all**: skipping whitespace from
    /// `doc_end` lands on end-of-file, a closing `}`, or another comment (which
    /// then becomes the nearest preceding trivium for whatever follows, so this
    /// docblock adopts nothing either way).
    ///
    /// A `?>` close tag is deliberately **not** a proof: the inline HTML after it
    /// is an output statement, and the template idiom
    /// `<?php /** @var View $v */ ?>` is a legal annotation, not rot.
    ///
    /// A `true` answer is therefore a proof of non-adoption; a `false` answer is
    /// merely "something follows", not a proof that it adopts.
    #[must_use]
    pub fn docblock_adopts_nothing(&self, doc_end: u32) -> bool {
        let Some(rest) = self.text.get(doc_end as usize..) else { return true };
        let rest = rest.trim_start();
        rest.is_empty() || rest.starts_with('}') || rest.starts_with("/*")
    }

    /// Whether the variable spelling `$name` occurs anywhere in the file **before**
    /// `offset` — a deliberately crude textual probe, used by
    /// `phpdoc.stale-var` to ask whether a named variable plausibly exists at all
    /// (issue #186).
    ///
    /// It counts every occurrence alike — a parameter, an assignment target, a
    /// `use` capture, a `foreach` binding, a plain read, even a mention in an
    /// earlier docblock — because the question is existence, not liveness. And its
    /// window is the whole file prefix rather than the enclosing scope's, which is
    /// a **superset**: every over-match only produces more silence, and the check
    /// it serves fires only on a name that occurs nowhere at all.
    ///
    /// The match is token-exact at both ends: `$ec` does not match `$echo`, and
    /// `$$echo` does not match `$echo`.
    #[must_use]
    pub fn variable_mentioned_before(&self, name: &str, offset: u32) -> bool {
        if name.is_empty() {
            return false;
        }
        let Some(prefix) = self.text.get(..offset as usize) else { return false };
        let bytes = prefix.as_bytes();
        let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80;
        let needle = format!("${name}");
        let mut from = 0usize;
        while let Some(rel) = prefix[from..].find(&needle) {
            let at = from + rel;
            let after = at + needle.len();
            let before_ok = at == 0 || bytes[at - 1] != b'$';
            let after_ok = bytes.get(after).is_none_or(|&b| !ident(b));
            if before_ok && after_ok {
                return true;
            }
            from = at + 1;
        }
        false
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

    /// The source text a span covers, or `None` when the span is out of bounds or
    /// does not land on a character boundary.
    ///
    /// The one way to read the file's own words back out of the tree. Its first
    /// consumer is `type.return-missing` (issue #199), which quotes a declared
    /// return type **as written** — `: array`, `: mixed`, `: self` all lower to no
    /// [`NativeType`], so nothing else in the tree can name them, yet PHP's
    /// `TypeError` does.
    #[must_use]
    pub fn text_at(&self, span: Span) -> Option<&str> {
        self.text.get(span.start as usize..span.end as usize)
    }

    /// Widen a statement `span` to its whole physical line(s) when nothing else
    /// shares them: with only whitespace before `span.start` on its first line
    /// and only whitespace after `span.end` up to the line break, the returned
    /// span starts at the line start and swallows the trailing newline (CRLF
    /// included), so deleting it leaves no blank gutter line — the same
    /// no-leftover discipline steins-edit's docblock tag deletion applies.
    /// A span that shares a line with anything else — code before it, a
    /// trailing comment after it — comes back exactly as given, so a deletion
    /// removes the statement and nothing beside it.
    #[must_use]
    pub fn whole_line_span(&self, span: Span) -> Span {
        let bytes = self.text.as_bytes();
        let line_idx = self.line_starts.partition_point(|&s| s <= span.start).saturating_sub(1);
        let line_start = self.line_starts.get(line_idx).copied().unwrap_or(0) as usize;
        let leading_blank = self
            .text
            .get(line_start..span.start as usize)
            .is_some_and(|s| s.chars().all(char::is_whitespace));
        if !leading_blank {
            return span;
        }
        // Skip horizontal whitespace (and a CR) after the span, then require the
        // line to actually end there — at a newline, or at end of file.
        let mut end = span.end as usize;
        while bytes.get(end).is_some_and(|&b| b == b' ' || b == b'\t' || b == b'\r') {
            end += 1;
        }
        match bytes.get(end) {
            Some(&b'\n') => Span { start: line_start as u32, end: (end + 1) as u32 },
            None => Span { start: line_start as u32, end: end as u32 },
            Some(_) => span,
        }
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
    /// Issue #168: see [`SourceTree::preg_flag_const_declared`].
    preg_flag_const_declared: bool,
    /// Issue #168: see [`SourceTree::preg_flag_const_aliased`].
    preg_flag_const_aliased: bool,
    /// Class references at the **positions verified to break at run time**
    /// (ADR-0049 §5 / S4, widened by issue #182), in four groups:
    /// the original hard-error expressions (`new X`, `X::m()`, `X::CONST`,
    /// `X::$prop` — fatal `Error`); inheritance (`extends`, `implements`, `use
    /// <Trait>` — fatal at class load); `catch (X $e)` (never matches, the handler
    /// is silently dead); and native type declarations on a parameter, return or
    /// property (`TypeError` on the first typed use).
    ///
    /// Explicit named classes only — `self`/`static`/`parent`, dynamic class exprs,
    /// and the `X::class` magic constant (a plain string since 8.0, never an error)
    /// are excluded at collection, so the collection IS exactly the verified
    /// finding-position set. `instanceof` and the docblock positions error nothing
    /// and are deliberately absent (the ADR-0078 contract twins).
    hard_class_refs: Vec<NameRef>,
    // member absence (ADR-0078, issue #197)
    property_writes: PropertyWrites,
    // end member absence (ADR-0078, issue #197)
    global_const_decls: Vec<GlobalConstDecl>,
    const_refs: Vec<NameRef>,
}

// member absence (ADR-0078, issue #197)
/// Every property name a file **writes**, plus whether it writes one under a name
/// only the runtime knows (ADR-0078, issue #197).
///
/// A write is what creates a dynamic property, so this is the evidence a
/// `property.undefined` claim needs and cannot get from a class declaration: a
/// class may declare nothing named `p` and still answer `$o->p` at runtime because
/// some other file did `$o->p = 1` first (deprecated on a plain class since PHP
/// 8.2 — deprecated, not an error, and the read that follows is clean; witnessed
/// at 8.5.9).
///
/// Names, not receivers: resolving *which* object each write lands on is the
/// deferred `property.dynamic-write` slice's problem, and a name-keyed obstacle
/// needs none of it. The over-silence costs the absence claims for names the
/// project assigns somewhere — which are exactly the names a dynamic write could
/// have created — while the typo shape the check exists for (`$user->emial`) is
/// written nowhere and survives.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
struct PropertyWrites {
    /// The written names, deduplicated, as written (property names are
    /// case-sensitive in PHP).
    names: Vec<String>,
    /// `true` when the file writes a property through a runtime-computed name
    /// (`$o->$n = …`). Such a write can create **any** name, so one anywhere in
    /// the project takes the whole id off the surface.
    dynamic: bool,
}

impl PropertyWrites {
    /// Record a property-write lvalue. A `$o->p` / `$this->p` / `$a->b->c` target
    /// contributes its (last) name; a computed selector sets [`Self::dynamic`];
    /// anything else is not a property write and contributes nothing.
    fn push_lvalue(&mut self, lvalue: &Expression<'_>) {
        let Expression::Access(Access::Property(pa)) = lvalue.unparenthesized() else {
            return;
        };
        match method_name_of(&pa.property) {
            Some(name) => {
                if !self.names.contains(&name) {
                    self.names.push(name);
                }
            }
            None => self.dynamic = true,
        }
    }
}
// end member absence (ADR-0078, issue #197)

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
            // `define(...)` (ADR-0078, issue #198): a literal name mints a global
            // constant declaration, a computed one mints a dam site — the same
            // compile-time/runtime split `classify_class_alias` makes just above.
            classify_define(c, out);
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
            // `define('…PREG_SET_ORDER', …)` and siblings (issue #168): the same
            // name-only, deliberately over-broad scan for the modeled preg flag
            // constants — one hit disables the engine-constant flags resolution
            // project-wide.
            if let Expression::Identifier(id) = c.function
                && bytes_to_string(id.last_segment()).eq_ignore_ascii_case("define")
                && let Some(first) = c.argument_list.arguments.iter().next()
                && let Expression::Literal(Literal::String(ls)) = first.value().unparenthesized()
                && ls.value.is_some_and(|bytes| {
                    let name = bytes_to_string(bytes);
                    PREG_FLAG_CONST_NAMES.iter().any(|n| name.ends_with(n))
                })
            {
                out.preg_flag_const_declared = true;
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
            // `const PREG_SET_ORDER = …;` and siblings (issue #168): a userland
            // twin of a modeled preg flag constant, same conservative reading.
            if con
                .items
                .iter()
                .any(|i| PREG_FLAG_CONST_NAMES.contains(&bytes_to_string(i.name.value).as_str()))
            {
                out.preg_flag_const_declared = true;
            }
            // …and the same statement declares global constants (ADR-0078, issue
            // #198), one per item, resolved against the enclosing namespace exactly
            // as a function declaration is. A class constant is a `ClassLikeConstant`
            // node, never this one, so nothing here can leak into issue #197's
            // namespace.
            for item in con.items.iter() {
                let name = bytes_to_string(item.name.value);
                let offset = to_span(item.name.span).start;
                out.global_const_decls.push(GlobalConstDecl {
                    fqn: normalize_const_fqn(&qualify_const_decl(rc, offset, &name)),
                    span: to_span(item.name.span),
                });
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
            if use_binds_preg_flag_const(u) {
                out.preg_flag_const_aliased = true;
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
            // …and the SAME names are hard refs (issue #182): an anonymous class
            // whose parent or interface is missing fatals at the `new` that
            // declares it, exactly like a named class fatals at load. The two
            // lists are read by different passes and never joined, so collecting
            // both duplicates no diagnostic.
            push_inheritance_refs(ac.extends.as_ref(), ac.implements.as_ref(), out);
        }
        // The class-reference positions verified to break at run time (ADR-0049 §5
        // / S4, widened by issue #182). Each collects only an explicitly-named
        // class (`trace_static_class` / `instantiation_class` return
        // `None`/non-`Named` for self/static/parent and dynamic class exprs), so
        // `class.undefined` never fires on those forms.
        //
        // (a) The original four hard-error expression positions.
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
        // member absence (ADR-0078, issue #197)
        // Every property-write lvalue in the file, collected where the walk already
        // visits every node — so a write buried in a sub-expression the linear
        // trace drops to `ArgValue::Other` (`f($o->dyn = 1)`) is seen too, which is
        // what makes the resulting obstacle set an over-approximation rather than a
        // trace artefact. Nested scopes are NOT skipped here (unlike
        // `collect_assign_writes`, which asks a per-scope question): a dynamic
        // property written inside a closure is written all the same.
        Node::Assignment(a) => out.property_writes.push_lvalue(a.lhs),
        Node::UnaryPrefix(u) => {
            if matches!(
                u.operator,
                UnaryPrefixOperator::PreIncrement(_) | UnaryPrefixOperator::PreDecrement(_)
            ) {
                out.property_writes.push_lvalue(u.operand);
            }
        }
        Node::UnaryPostfix(u) => out.property_writes.push_lvalue(u.operand),
        // `foreach ($xs as $o->p)` binds the property on every iteration.
        Node::ForeachValueTarget(t) => out.property_writes.push_lvalue(t.value),
        Node::ForeachKeyValueTarget(t) => {
            out.property_writes.push_lvalue(t.key);
            out.property_writes.push_lvalue(t.value);
        }
        // end member absence (ADR-0078, issue #197)
        // A **bare constant fetch** (ADR-0078, issue #198) — the one position a
        // global constant is read from, and a fatal `Error: Undefined constant "X"`
        // since PHP 8.0 (`php -r`-witnessed on 8.5.9). The grammar does the
        // excluding for us: `X::CONST` is `ClassConstantAccess` (issue #197's
        // namespace), `__LINE__` and friends are `MagicConstant`, and `true` /
        // `false` / `null` lex as `Literal` — none of them reach this arm. The one
        // textual exclusion below is belt-and-braces for the reserved trio, which
        // are case-insensitive keywords and could never be a finding whatever a
        // future grammar change did with them.
        Node::ConstantAccess(ca) => {
            let r = name_ref(&ca.name);
            let reserved = !r.raw.contains('\\')
                && ["true", "false", "null"].iter().any(|k| r.raw.eq_ignore_ascii_case(k));
            if !reserved {
                out.const_refs.push(r);
            }
        }
        // (b) Inheritance (issue #182): `extends` / `implements` on a class, an
        // enum's `implements`, an interface's (multiple) `extends`, and a `use`
        // of a trait in a class body. Every one of these is a fatal at CLASS LOAD
        // time — the strongest consequence in the family.
        Node::Class(c) => push_inheritance_refs(c.extends.as_ref(), c.implements.as_ref(), out),
        Node::Interface(i) => push_inheritance_refs(i.extends.as_ref(), None, out),
        Node::Enum(e) => push_inheritance_refs(None, e.implements.as_ref(), out),
        Node::TraitUse(tu) => out.hard_class_refs.extend(tu.trait_names.iter().map(name_ref)),
        // (c) `catch (X $e)` (issue #182): a clause naming a class that does not
        // exist never matches, so the handler is silently dead. The clause is
        // lowered by the SAME `lower_catch_clause` the throw walk uses, so the
        // caught-name set here is the one ADR-0040 already agreed on; a clause
        // that lowering could not name statically (`has_unresolvable`) contributes
        // nothing at all — not even its resolvable arms, since the unresolvable
        // member may well be the one that catches.
        Node::TryCatchClause(c) => {
            let clause = lower_catch_clause(c);
            if !clause.has_unresolvable {
                out.hard_class_refs.extend(clause.classes);
            }
        }
        // (d) Native type declarations (issue #182): a parameter (promoted
        // constructor properties and property-hook parameters included), a return
        // type, or a property type naming a class-like that does not exist raises
        // a `TypeError` on the first typed use. `collect_hint_class_refs` walks
        // the declaration's arms; the built-in type keywords are excluded by
        // construction (see its doc comment).
        Node::FunctionLikeParameter(p) => {
            if let Some(hint) = &p.hint {
                collect_hint_class_refs(hint, &mut out.hard_class_refs);
            }
        }
        Node::FunctionLikeReturnTypeHint(r) => {
            collect_hint_class_refs(&r.hint, &mut out.hard_class_refs);
        }
        Node::PlainProperty(p) => {
            if let Some(hint) = &p.hint {
                collect_hint_class_refs(hint, &mut out.hard_class_refs);
            }
        }
        Node::HookedProperty(p) => {
            if let Some(hint) = &p.hint {
                collect_hint_class_refs(hint, &mut out.hard_class_refs);
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
    for child in children(node) {
        walk(&child, aliases, docs, rc, child_conditional, child_typed, out);
    }
}

/// Push every name an inheritance clause pair mentions onto the hard-reference
/// list (issue #182). An `extends` on an interface carries several names, a class's
/// carries one, and `implements` carries a list on both classes and enums — the
/// same `Identifier` sequence in every case, so one helper serves them all.
/// Inheritance names are always textual identifiers in the grammar: there is no
/// `extends $x` and no `extends self`, so nothing needs excluding here.
fn push_inheritance_refs(
    extends: Option<&mago_syntax::cst::Extends<'_>>,
    implements: Option<&mago_syntax::cst::Implements<'_>>,
    out: &mut Lowered,
) {
    if let Some(e) = extends {
        out.hard_class_refs.extend(e.types.iter().map(name_ref));
    }
    if let Some(i) = implements {
        out.hard_class_refs.extend(i.types.iter().map(name_ref));
    }
}

/// Collect every class-like name a **native type declaration** mentions (issue
/// #182), one [`NameRef`] per named arm: `?X` contributes `X`, `X|Y` contributes
/// both, `X&Y` contributes both, and a DNF declaration `(A&B)|null` contributes
/// `A` and `B`.
///
/// The exclusion of the built-in type keywords is structural, not a name list:
/// every one of them — `int`, `float`, `string`, `bool`, `true`, `false`, `null`,
/// `array`, `callable`, `iterable`, `object`, `mixed`, `void`, `never`, and
/// `self`/`static`/`parent` — is its OWN `Hint` variant in the CST, so only
/// `Hint::Identifier` can name a class-like and the catch-all arm drops the rest
/// by construction. This is the same discrimination [`lower_hint_into`] makes.
fn collect_hint_class_refs(hint: &Hint<'_>, out: &mut Vec<NameRef>) {
    match hint {
        Hint::Identifier(id) => out.push(name_ref(id)),
        Hint::Nullable(n) => collect_hint_class_refs(n.hint, out),
        Hint::Union(u) => {
            collect_hint_class_refs(u.left, out);
            collect_hint_class_refs(u.right, out);
        }
        Hint::Intersection(i) => {
            collect_hint_class_refs(i.left, out);
            collect_hint_class_refs(i.right, out);
        }
        Hint::Parenthesized(p) => collect_hint_class_refs(p.hint, out),
        _ => {}
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
    // A long `.` chain recurses once per operand (issue #264); out of headroom,
    // the value is simply unproven — the same answer any unmodelled operand gets.
    if stack_guard::exhausted() {
        return ConcatVal::Unproven;
    }
    match expr.unparenthesized() {
        // A name lane (include paths, `class_alias` arguments), not a value lane:
        // the result is looked up in a `String`-keyed universe, so a literal
        // whose bytes are not UTF-8 is unproven rather than lossily decoded
        // (ADR-0080 §2.5 — silence, never a guess).
        Expression::Literal(Literal::String(ls)) => ls
            .value
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .map_or(ConcatVal::Unproven, |s| ConcatVal::Str(s.to_owned())),
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

/// Classify a `define(...)` call (ADR-0078, issue #198), the constant-side twin of
/// [`classify_class_alias`]: a **compile-time** name argument mints a
/// [`GlobalConstDecl`] the index reads as evidence of definition; a name that only
/// exists at run time makes it a [`DynamismKind::DefineDynamic`] dam site, because a
/// computed `define` can mint *any* constant name and no scan can enumerate it.
///
/// Two deliberate differences from the `class_alias` classifier:
///
/// * the name is **not** resolved against the namespace or `use` imports.
///   `define('FOO', 1)` inside `namespace App;` declares the global `FOO`, not
///   `App\FOO` — `php -r`-witnessed on 8.5.9 — so the literal is the whole FQN.
/// * `X::class` is not accepted as a name. It is a compile-time string, but a
///   `define` keyed on a class name is not an idiom worth a special case; it falls
///   through to the dam, which is the sound direction.
///
/// Callee recognition is `class_alias`'s: the unqualified spelling (subject to PHP's
/// global function fallback) or the fully-qualified `\define`; a namespaced
/// `Foo\define` is a different symbol and is ignored entirely.
fn classify_define(c: &FunctionCall<'_>, out: &mut Lowered) {
    let Expression::Identifier(id) = c.function else { return };
    if !matches!(id, Identifier::Local(_) | Identifier::FullyQualified(_)) {
        return;
    }
    if !bytes_to_string(id.last_segment()).eq_ignore_ascii_case("define") {
        return;
    }
    let span = to_span(c.span());
    // The name is the FIRST positional (non-spread) argument. A named or spread
    // first argument is not read — it dams, like every other unreadable name.
    let literal = match c.argument_list.arguments.iter().next() {
        Some(Argument::Positional(p)) if p.ellipsis.is_none() => {
            match lower_concat(p.value.unparenthesized()) {
                ConcatVal::Str(s) => Some(s),
                _ => None,
            }
        }
        _ => None,
    };
    match literal {
        Some(name) => out
            .global_const_decls
            .push(GlobalConstDecl { fqn: normalize_const_fqn(name.trim()), span }),
        None => out.dynamism.push(DynamismSite { kind: DynamismKind::DefineDynamic, span }),
    }
}

/// The FQN a `const NAME = …;` statement at `offset` declares: the enclosing
/// namespace joined to the written name, or the name alone in the global namespace.
/// Case is preserved on both halves — [`normalize_const_fqn`] is what decides which
/// half folds.
fn qualify_const_decl(rc: &RefResolver, offset: u32, name: &str) -> String {
    let ns = &ctx_of(rc.contexts, rc.regions, offset).namespace;
    if ns.is_empty() { name.to_owned() } else { format!("{ns}\\{name}") }
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
///   [`RefResolver`] every other class reference uses — plain, aliased, and grouped
///   `use` imports; the enclosing namespace; and the `namespace\X` relative
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
        ret_span: f.return_type_hint.as_ref().map(|r| to_span(r.hint.span())),
        span: to_span(f.name.span()),
        body_span: to_span(f.body.span()),
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
            // The syntactic answer, kept beside the modeling one (issue #200).
            hint_span: p.hint.as_ref().map(|h| to_span(h.span())),
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
    for child in children(node) {
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
/// A trait joins the class-*like* index but has no lowered members or flattening,
/// making it inert except for occupying its FQN in the symbol/ambiguity table.
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
        const_visibility: Vec::new(),
        const_decls: Vec::new(),
        hooked_properties: Vec::new(),
        // A trait is inert for member absence: `uses_traits` on the using class is
        // already an obstacle, so the attribute here would change nothing.
        allows_dynamic_properties: false,
        uses_traits: false,
        // No member docblock can observe a trait-level `@template`.
        docblock: None,
        docblock_span: None,
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
    let mut const_visibility = Vec::new();
    let mut const_decls = Vec::new();
    let mut hooked_properties = Vec::new();
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
            // Hooked properties are virtual/computed and therefore not heap-tracked
            // or checked as stored values — only their NAME is kept, so a check that
            // asks whether the class declares the member at all is not fooled by the
            // drop (ADR-0078, issue #185).
            ClassLikeMember::Property(Property::Hooked(h)) => {
                hooked_properties.push(strip_dollar(bytes_to_string(match &h.item {
                    PropertyItem::Abstract(a) => a.variable.name,
                    PropertyItem::Concrete(c) => c.variable.name,
                })));
            }
            ClassLikeMember::Constant(k) => {
                lower_class_consts(k, docs, &mut consts, &mut const_visibility, &mut const_decls);
            }
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
        const_visibility,
        const_decls,
        hooked_properties,
        // member absence (ADR-0078, issue #197)
        allows_dynamic_properties: attrs_allow_dynamic_properties(&c.attribute_lists),
        // end member absence (ADR-0078, issue #197)
        uses_traits,
        // Class-level docblock (preceding the whole declaration incl. attributes/
        // modifiers, mirroring the function/method lookup) — read for `@template`
        // names that shadow same-named classes in member docblocks (issue #5).
        docblock: docs.preceding(to_span(c.span()).start),
        docblock_span: docs.preceding_span(to_span(c.span()).start),
        span: to_span(c.name.span()),
    }
}

/// Lower a `const NAME = <expr>[, …];` class-member declaration into `(name,
/// value)` pairs, keeping **only literal** initializers (ADR-0043 §2). A
/// non-literal value lowers to [`ArgValue::Other`] and is dropped from `out`, so a
/// name's absence there means "no proven literal", never "no such constant".
///
/// `vis` receives every declared name with its visibility (ADR-0078, issue #185),
/// literal initializer or not — the one list whose absence of a name does mean the
/// class-like declares no such constant.
///
/// `decls` receives every declared name's **declaration shape** (ADR-0078, issue
/// #200): the PHP 8.3 native constant type's span and the `const` declaration's
/// own docblock. A multi-item `const A = 1, B = 2;` shares both, exactly as it
/// shares its visibility.
fn lower_class_consts(
    k: &mago_syntax::cst::ClassLikeConstant<'_>,
    docs: &DocIndex,
    out: &mut Vec<(String, ArgValue)>,
    vis: &mut Vec<(String, Visibility)>,
    decls: &mut Vec<ClassConstDecl>,
) {
    let visibility = visibility_of(&k.modifiers);
    // untyped surface (ADR-0078, issue #200)
    let hint_span = k.hint.as_ref().map(|h| to_span(h.span()));
    let docblock = docs.preceding(to_span(k.span()).start);
    // end untyped surface (ADR-0078, issue #200)
    for item in k.items.iter() {
        let name = bytes_to_string(item.name.value);
        vis.push((name.clone(), visibility));
        decls.push(ClassConstDecl {
            name: name.clone(),
            hint_span,
            docblock: docblock.clone(),
            span: to_span(item.name.span()),
        });
        let v = lower_arg_value(item.value);
        if !matches!(v, ArgValue::Other) {
            out.push((name, v));
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
    let hint_span = p.hint.as_ref().map(|h| to_span(h.span()));
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
            hint_span,
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
            hint_span: p.hint.as_ref().map(|h| to_span(h.span())),
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
    let mut const_visibility = Vec::new();
    let mut const_decls = Vec::new();
    for member in i.members.iter() {
        match member {
            ClassLikeMember::Method(m) => methods.push(lower_method(m, aliases, docs, rc)),
            ClassLikeMember::Constant(k) => {
                lower_class_consts(k, docs, &mut consts, &mut const_visibility, &mut const_decls);
            }
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
        const_visibility,
        const_decls,
        hooked_properties: Vec::new(),
        // An interface declares no properties at all, so it can never be open.
        allows_dynamic_properties: false,
        uses_traits: false,
        // Class-level docblock — `@template` names shadow same-named classes in the
        // interface's method docblocks (issue #5).
        docblock: docs.preceding(to_span(i.span()).start),
        docblock_span: docs.preceding_span(to_span(i.span()).start),
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
/// Enum method bodies are not analyzed: [`methods`] is empty and no scope is
/// built (see [`ClassDecl`]).
fn lower_enum(e: &mago_syntax::cst::Enum<'_>, _aliases: &SteinsAttrAliases, docs: &DocIndex, rc: &RefResolver, conditional: bool) -> ClassDecl {
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
    let mut const_visibility = Vec::new();
    let mut const_decls = Vec::new();
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
            ClassLikeMember::Constant(k) => {
                lower_class_consts(k, docs, &mut consts, &mut const_visibility, &mut const_decls);
            }
            _ => {}
        }
    }

    // Keep the class-like lowerer signature uniform; enum members need no name
    // resolution here.
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
        const_visibility,
        const_decls,
        hooked_properties: Vec::new(),
        // An enum cannot declare a property, dynamic or otherwise.
        allows_dynamic_properties: false,
        uses_traits: false,
        // No analyzed member can observe an enum-level `@template`.
        docblock: None,
        docblock_span: None,
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
        ret_span: m.return_type_hint.as_ref().map(|r| to_span(r.hint.span())),
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
    for child in children(node) {
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
/// `Effects::IO`, a concatenation, or a named argument) cannot be resolved
/// without constant resolution and makes the whole attribute *unrecognized* (no
/// envelope or checking), the conservative choice.
///
/// `#[\Steins\Pure]` and `#[\Steins\Effect(...)]` on the same declaration are
/// contradictory (Pure = empty upper bound, the tighter one); **Pure wins**
/// (empty `labels`), with no diagnostic about the contradiction here.
// member absence (ADR-0078, issue #197)
/// Whether an attribute list carries PHP's own `#[AllowDynamicProperties]`
/// (ADR-0078, issue #197) — see [`ClassDecl::allows_dynamic_properties`] for what
/// the flag licenses and why the match is name-only.
fn attrs_allow_dynamic_properties(
    attribute_lists: &mago_syntax::cst::Sequence<'_, mago_syntax::cst::AttributeList<'_>>,
) -> bool {
    attribute_lists.iter().any(|list| {
        list.attributes
            .iter()
            .any(|attr| normalize_class(&bytes_to_string(attr.name.value())) == "allowdynamicproperties")
    })
}
// end member absence (ADR-0078, issue #197)

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
            // Method string callables (`Foo::m`) are not resolved.
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

/// The proven-constant form of a named call's first two positional arguments
/// ([`ConstArgs`], issue #318). Empty when a named or spread argument defeats
/// positional mapping — the same list shapes [`arg_targets_of_call`] withholds.
fn const_args_of_call(fc: &FunctionCall<'_>) -> ConstArgs {
    let mut out = ConstArgs::default();
    for (pos, arg) in fc.argument_list.arguments.iter().enumerate() {
        let Argument::Positional(p) = arg else { return ConstArgs::default() };
        if p.ellipsis.is_some() {
            return ConstArgs::default();
        }
        match pos {
            0 => out.first = const_arg_of(p.value),
            1 => out.second = const_arg_of(p.value),
            // Nothing past position 1 decides a stream target; the loop runs on
            // only to catch a named/spread argument further along.
            _ => {}
        }
    }
    out
}

/// One argument expression as a [`CallTarget`], or `None` for every expression
/// whose value is not written in the source text.
fn const_arg_of(expr: &Expression<'_>) -> Option<CallTarget> {
    match expr.unparenthesized() {
        // The parser hands over the escape-decoded bytes; a stream target is a
        // path, URL or wrapper name, so a lossy decode of a non-UTF-8 tail
        // cannot invent a scheme (`://` is ASCII) and can only lose a narrowing.
        Expression::Literal(Literal::String(ls)) => {
            Some(CallTarget::Literal(bytes_to_string(ls.value?)))
        }
        Expression::ConstantAccess(ca) => {
            let name = name_ref(&ca.name);
            (!name.raw.contains('\\')).then_some(CallTarget::ConstFetch(name.raw))
        }
        _ => None,
    }
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
    for child in children(node) {
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
    for child in children(node) {
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
    for child in children(node) {
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
    for child in children(node) {
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
                let const_args = const_args_of_call(fc);
                match higher_order_of_call(fc) {
                    Some((callee, callbacks, arg_count)) => {
                        out.push(EffectOrigin::HigherOrder {
                            callee,
                            callbacks,
                            arg_count,
                            // Both helpers reject the same argument lists, so this
                            // is always `Some` on this arm.
                            arg_targets: arg_targets.clone().unwrap_or_default(),
                            const_args,
                            span: to_span(fc.span()),
                        });
                    }
                    None => out.push(EffectOrigin::Call {
                        name: name_ref(id),
                        span: to_span(id.span()),
                        arg_targets,
                        const_args,
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
        // Raw text between a `?>` and the next `<?php` inside a body: the engine
        // writes it to the output channel exactly as `echo` does, so it is an
        // output origin (ADR-0008 always said so; ADR-0083 is what wired it).
        // Whitespace-only inline text is skipped — the newline and indentation
        // separating two tag pairs are punctuation of the source layout, not
        // output anyone writes a function for, and coloring them would make the
        // effect depend on how the template is indented. A shebang is not output
        // at all (the SAPI eats it) and cannot occur inside a body regardless.
        Node::Inline(i) => {
            if i.kind.is_text() && !i.value.iter().all(u8::is_ascii_whitespace) {
                out.push(EffectOrigin::Output { keyword: "inline HTML", span: to_span(i.span()) });
            }
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
        // Nested scopes are scanned independently.
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
    for child in children(node) {
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
        // [`ArgValue::Other`] as values and so are invisible
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
    for child in children(node) {
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
    for child in children(node) {
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
/// four scalars + `false`/`true`/`null`), or `None` for unsupported types. A
/// single non-scalar member anywhere (`array`, `mixed`,
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
        // stay in the silence arm below because late-static binding is unsupported
        // (ADR-0043).
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
    // Out of headroom (issue #264): the guard is unmodelled, which claims nothing
    // on either polarity — exactly what an unrecognized operand already yields.
    if stack_guard::exhausted() {
        return None;
    }
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
/// decides whether an interface envelope applies; failure taints
/// exhaustiveness. Proven forms delegate to [`effect_recv_of_object`], which the
/// throw scan also uses.
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
    // `$a[0][0][…]` and long `.` chains recurse once per level (issue #264). Out
    // of headroom the value is `Other` — the unproven answer this lowering
    // already gives every shape it does not model.
    if stack_guard::exhausted() {
        return ArgValue::Other;
    }
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
        // fact). Spread arguments are not represented.
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
                then_span: to_span(then_expr.span()),
                then_val: Box::new(lower_arg_value(then_expr)),
                else_span: to_span(cond.r#else.span()),
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
        // First-class callable of a named free function `strtolower(...)`.
        // Method and static first-class callables lower to `Other`.
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
            ArgValue::Coalesce(
                Box::new(lower_arg_value(b.lhs)),
                Box::new(lower_arg_value(b.rhs)),
                to_span(b.rhs.span()),
            )
        }
        // String concatenation `$a . $b` (issue #59). Structural, like `??` above:
        // an operand's value is an env fact, so the join runs in the walk. Note this
        // is the ONE binary operator lowered as a value — arithmetic still widens to
        // `Other`, because `+`/`-`/`*` carry overflow and int/float promotion
        // questions that byte concatenation does not.
        //
        // Keep unrepresentable operands in the tree; resolution remains silent
        // unless both operands become known.
        Expression::Binary(b) if b.operator.is_concatenation() => {
            ArgValue::Concat(Box::new(lower_arg_value(b.lhs)), Box::new(lower_arg_value(b.rhs)))
        }
        // A comparison in VALUE position (issue #260): `$b = $x > 3;` rather than
        // `if ($x > 3)`. Structural, like `.` and `??` above — the operands' values
        // are env facts, so the decision runs in the walk, where the SAME `eval_cmp`
        // that decides a guard decides this one. Arithmetic, bitwise and logical
        // operators still widen to `Other`: this node carries only what an
        // evaluator answers (Certainty discipline — an unimplemented arm declines).
        Expression::Binary(b) if cmp_op_of(&b.operator).is_some() => {
            let op = ValueOp::Cmp(cmp_op_of(&b.operator).expect("matched above"));
            ArgValue::Binary {
                op,
                lhs: Box::new(lower_arg_value(b.lhs)),
                rhs: Box::new(lower_arg_value(b.rhs)),
            }
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
        // A bare global-constant fetch (`PREG_SET_ORDER`, `SOME_CONST`) in value
        // position (issue #168). `true`/`false`/`null` never reach here — they lex
        // as literals. Carried with its qualification kind so a consumer can apply
        // the engine-constant discipline (issue #29's `PHP_VERSION_ID` rules); an
        // unconsumed fetch stays exactly as unproven as the `Other` it replaced.
        Expression::ConstantAccess(ca) => ArgValue::GlobalConst(name_ref(&ca.name)),
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
                // A key the source does not spell as a literal is CARRIED now
                // (issue #336) rather than collapsing the whole literal: the
                // walk can ask what the key expression is even though it cannot
                // say which key it lands on. An unrepresentable key expression
                // still collapses — there is nothing to carry.
                let key = match lower_array_key(kv.key) {
                    Some(k) => k,
                    None => match lower_arg_value(kv.key) {
                        ArgValue::Other | ArgValue::OffsetRead { .. } => return ArgValue::Other,
                        e => ArrayKey::Expr(Box::new(e)),
                    },
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
        ArgValue::Null => Some(ArrayKey::Str(PhpStr::new())),
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
            // A byte string is never a canonical integer spelling (every byte of
            // one is an ASCII digit or `-`), so a non-UTF-8 key always lands in
            // the `Str` arm and never disturbs the auto-index counter.
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
pub fn php_canonical_int_string(s: impl AsRef<[u8]>) -> Option<i64> {
    let s = std::str::from_utf8(s.as_ref()).ok()?;
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
        // test is on the parsed value, not the spelling. A cast of
        // `9223372036854775808` to `i64` would produce the wrong value, `i64::MIN`;
        // negating it also wraps to itself. (`PHP_INT_MIN` has no
        // integer-literal spelling at all — it is written `-PHP_INT_MAX - 1`.)
        //
        // The parser's own `value` is NOT usable for the overflow decision: it is a
        // `u64` that SATURATES, so `99999999999999999999` arrives as `u64::MAX` —
        // indistinguishable from a real `0xFFFFFFFFFFFFFFFF` and three orders of
        // magnitude off PHP's `1.0E+20`. The spelling is re-read instead.
        Literal::Integer(li) => lower_int_literal(li.raw),
        Literal::Float(lf) => ArgValue::Float(lf.value.0),
        // The parser hands over the escape-decoded **bytes** (`"\xC0"` arrives as
        // `[0xC0]`), and a PHP string is a byte string, so they are carried
        // through unchanged. Decoding them lossily here was issue #208: it made
        // `"\xC0"` and `"\xD0"` the same value everywhere downstream.
        Literal::String(ls) => {
            ls.value.map_or(ArgValue::Other, |bytes| ArgValue::Str(PhpStr::from_bytes(bytes)))
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
    docs: &DocIndex<'_>,
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
    let mut scopes = vec![build_scope_from(ScopeOwner::TopLevel, &top, None, None)];
    collect_scopes(&Node::Program(program), contexts, regions, &rc, docs, None, &mut scopes);
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
///
/// `stmt_doc` is the statement-level docblock adoption context (issue #128): set
/// when the walk passed a simple-assignment statement whose RHS is exactly a
/// closure, carried down unchanged everywhere else (its def-offset gate means
/// only that one closure can pick it up).
fn collect_scopes(
    node: &Node<'_, '_>,
    contexts: &[NsCtx],
    regions: &[(u32, u32, usize)],
    rc: &RefResolver,
    docs: &DocIndex<'_>,
    stmt_doc: Option<&StmtAdoption>,
    out: &mut Vec<Scope>,
) {
    match node {
        Node::Function(f) => {
            let name = bytes_to_string(f.name.value);
            out.push(build_scope(
                ScopeOwner::Function(name),
                f.body.statements.as_slice(),
                ret_hint_of(f.return_type_hint.as_ref()),
                Some(&f.parameter_list),
            ));
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
                    out.push(build_scope(
                        owner,
                        block.statements.as_slice(),
                        ret_hint_of(m.return_type_hint.as_ref()),
                        Some(&m.parameter_list),
                    ));
                }
            }
        }
        // Closures / arrow fns get their own scope (ADR-0033), addressed by the
        // definition-site byte offset. Params/effects/throws ride on the scope.
        Node::Closure(cl) => out.push(build_closure_scope_from_closure(cl, rc, docs, stmt_doc)),
        Node::ArrowFunction(af) => out.push(build_closure_scope_from_arrow(af, rc, docs, stmt_doc)),
        // Statement-level docblock adoption (issue #128): a simple assignment
        // whose RHS is exactly a closure/arrow expression hands the statement's
        // docblock down to that closure's scope
        // (`/** @return string */\n$f = function () {…};`). The def-offset gate
        // keeps every other closure position (a call argument, a nested closure)
        // statement-silent — inline adjacency is their only route.
        Node::ExpressionStatement(es) => {
            let adopt = stmt_closure_adoption(es, docs);
            for child in children(node) {
                collect_scopes(&child, contexts, regions, rc, docs, adopt.as_ref(), out);
            }
            return;
        }
        _ => {}
    }
    // Recurse so nested functions (inside methods or blocks) and nested classes
    // also get their scopes. Method scopes are only created above (matching
    // `Node::Class`), so this recursion never double-creates one.
    for child in children(node) {
        collect_scopes(&child, contexts, regions, rc, docs, stmt_doc, out);
    }
}

/// The statement-level docblock adoption context of `collect_scopes` (issue
/// #128): the docblock preceding a simple-assignment statement, addressed to the
/// closure that is the statement's whole RHS — the exact trace-IR shape whose
/// `Assign` value is `ArgValue::Closure` (`lower_expr_stmt`'s direct-variable,
/// plain-`=` arm), re-read here on the CST because scopes are built before any
/// trace consumer runs.
struct StmtAdoption {
    /// Definition offset of the closure/arrow that is the statement's whole RHS
    /// — the gate that keeps the docblock from drifting to any other closure.
    def_offset: u32,
    /// The enclosing statement's docblock text.
    doc: String,
}

/// Recognize `/** … */\n$f = <closure>;` — a docblock-led statement that is a
/// plain `=` assignment to a direct variable whose RHS (unparenthesized, as
/// `lower_arg_value` reads it) is exactly a closure or arrow expression. Any
/// other statement shape — a closure in a call argument, a compound op, a
/// non-variable lvalue — adopts nothing at statement level.
fn stmt_closure_adoption(es: &ExpressionStatement<'_>, docs: &DocIndex<'_>) -> Option<StmtAdoption> {
    let Expression::Assignment(a) = es.expression.unparenthesized() else { return None };
    if !a.operator.is_assign() {
        return None;
    }
    let Expression::Variable(Variable::Direct(_)) = a.lhs.unparenthesized() else { return None };
    let def_offset = match a.rhs.unparenthesized() {
        Expression::Closure(cl) => closure_def_offset(cl),
        Expression::ArrowFunction(af) => arrow_def_offset(af),
        _ => return None,
    };
    let doc = docs.preceding(to_span(es.span()).start)?;
    Some(StmtAdoption { def_offset, doc })
}

/// The docblock a closure/arrow scope adopts (issue #128), by the shared
/// whitespace-gap discipline (ADR-0029's "only whitespace between", the same
/// grammar `SourceTree::stmt_docblock` gives the inline-`@var` lane) in two
/// positions, in precedence order:
///
/// 1. **Inline** — the docblock immediately preceding the closure expression's
///    own first token (`$f = /** @return string */ function () {…}`; the first
///    token is the attribute list or `static` when present, else
///    `function`/`fn` — exactly `span().start`).
/// 2. **Statement-level** — the enclosing statement's docblock, handed down by
///    `collect_scopes` only when that statement is a simple assignment whose
///    whole RHS is this very closure (the [`StmtAdoption`] def-offset gate).
///
/// Both positions read one grammar (`DocIndex::preceding`): a blank line still
/// adopts, while an intervening non-doc comment — or any code — breaks the
/// adjacency.
fn adopt_closure_docblock(
    docs: &DocIndex<'_>,
    first_token: u32,
    def_offset: u32,
    stmt_doc: Option<&StmtAdoption>,
) -> Option<String> {
    docs.preceding(first_token).or_else(|| {
        stmt_doc.filter(|sd| sd.def_offset == def_offset).map(|sd| sd.doc.clone())
    })
}

/// Lower one scope's statements to a linear trace, and compute its poison flag.
fn build_scope(
    owner: ScopeOwner,
    statements: &[Statement<'_>],
    ret_hint: Option<RetHint>,
    params: Option<&mago_syntax::cst::FunctionLikeParameterList<'_>>,
) -> Scope {
    let refs: Vec<&Statement<'_>> = statements.iter().collect();
    build_scope_from(owner, &refs, ret_hint, params)
}

/// Lower a scope from a borrowed statement list (shared by the flattened
/// top-level scope and the direct function/method paths).
///
/// `params` is the scope's own parameter list, or `None` for the top-level script
/// — which is both "no parameters" and the reason that scope reports no undefined
/// reads at all (see [`Scope::undefined_reads`]).
fn build_scope_from(
    owner: ScopeOwner,
    statements: &[&Statement<'_>],
    ret_hint: Option<RetHint>,
    params: Option<&mago_syntax::cst::FunctionLikeParameterList<'_>>,
) -> Scope {
    let mut opaque = Vec::new();
    let mut stmts = Vec::new();
    let mut method_calls = Vec::new();
    let mut is_generator = false;
    for s in statements {
        lower_stmt(s, &mut stmts);
        scan_method_calls(&Node::Statement(s), &mut method_calls);
        scan_opaque(&Node::Statement(s), &mut opaque, false);
        if !is_generator {
            is_generator = node_is_generator(&Node::Statement(s));
        }
    }
    // The flag IS the inventory being non-empty (never a second computation).
    let poisoned = !opaque.is_empty();
    let function_name = match &owner {
        ScopeOwner::Function(name) => Some(name.clone()),
        ScopeOwner::TopLevel | ScopeOwner::Method { .. } | ScopeOwner::Closure { .. } => None,
    };
    let vars = undefined_variable_reads(params, None, statements);
    Scope {
        function_name,
        owner,
        ret_hint,
        is_generator,
        poisoned,
        opaque,
        stmts,
        method_calls,
        params: Vec::new(),
        ret_ty: None,
        effect_origins: Vec::new(),
        throw_origins: Vec::new(),
        is_static: false,
        docblock: None,
        unused_captures: Vec::new(),
        undefined_reads: vars.undefined_reads,
        maybe_undefined_reads: vars.maybe_undefined_reads,
        ref_arg_candidates: vars.ref_arg_candidates,
    }
}

/// Classify a written return type hint for summary fallthrough (ADR-0075) and
/// record its span for the declaration-quoting diagnostics (issue #199).
fn ret_hint_of(hint: Option<&mago_syntax::cst::FunctionLikeReturnTypeHint<'_>>) -> Option<RetHint> {
    hint.map(|r| RetHint { kind: classify_ret_hint(&r.hint), span: to_span(r.hint.span()) })
}

fn classify_ret_hint(hint: &Hint<'_>) -> RetHintKind {
    match hint {
        Hint::Void(_) => RetHintKind::Void,
        Hint::Never(_) => RetHintKind::Never,
        Hint::Parenthesized(p) => classify_ret_hint(p.hint),
        _ => RetHintKind::Other,
    }
}

/// Whether the subtree contains a `yield` / `yield from` that makes this scope a
/// generator. Nested function/method/closure bodies are their own scopes and are
/// not counted.
fn node_is_generator(node: &Node<'_, '_>) -> bool {
    match node {
        Node::Yield(_) | Node::YieldFrom(_) | Node::YieldPair(_) | Node::YieldValue(_) => true,
        Node::Function(_) | Node::Method(_) | Node::Closure(_) | Node::ArrowFunction(_) => false,
        _ => {
            for child in children(node) {
                if node_is_generator(&child) {
                    return true;
                }
            }
            false
        }
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
fn build_closure_scope_from_closure(
    cl: &mago_syntax::cst::Closure<'_>,
    rc: &RefResolver,
    docs: &DocIndex<'_>,
    stmt_doc: Option<&StmtAdoption>,
) -> Scope {
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
    let mut is_generator = false;
    for s in cl.body.statements.iter() {
        lower_stmt(s, &mut stmts);
        scan_effect_origins(&Node::Statement(s), &cx, &mut effect_origins);
        scan_throw_origins(&Node::Statement(s), &[], &[], &cx.locals, &mut throw_origins);
        scan_method_calls(&Node::Statement(s), &mut method_calls);
        scan_opaque(&Node::Statement(s), &mut opaque, false);
        if !is_generator {
            is_generator = node_is_generator(&Node::Statement(s));
        }
    }
    let poisoned = !opaque.is_empty();
    let def_offset = closure_def_offset(cl);
    let vars = undefined_variable_reads(
        Some(&cl.parameter_list),
        cl.use_clause.as_ref(),
        &cl.body.statements.iter().collect::<Vec<_>>(),
    );
    Scope {
        function_name: None,
        owner: ScopeOwner::Closure { def_offset },
        ret_hint: ret_hint_of(cl.return_type_hint.as_ref()),
        is_generator,
        poisoned,
        opaque,
        stmts,
        method_calls,
        params: lower_params(&cl.parameter_list, rc),
        ret_ty: cl.return_type_hint.as_ref().and_then(|r| lower_hint(&r.hint, rc)),
        effect_origins,
        throw_origins,
        is_static: cl.r#static.is_some(),
        docblock: adopt_closure_docblock(docs, to_span(cl.span()).start, def_offset, stmt_doc),
        unused_captures: unused_by_value_captures(cl),
        undefined_reads: vars.undefined_reads,
        maybe_undefined_reads: vars.maybe_undefined_reads,
        ref_arg_candidates: vars.ref_arg_candidates,
    }
}

/// The by-value `use ($x)` captures a closure body never mentions (issue #186) —
/// the computation behind [`Scope::unused_captures`], done here because it needs
/// the CST the lowered trace deliberately forgets.
///
/// The walk is the **deep** one: it descends nested closures, arrow functions and
/// their `use (…)` clauses, so `use ($x) { return fn () => $x; }` and
/// `use ($x) { return function () use ($x) {…}; }` both count `$x` as mentioned.
/// A body that can mint or consume names without spelling them dams the whole
/// list ([`body_dams_names`]).
fn unused_by_value_captures(cl: &mago_syntax::cst::Closure<'_>) -> Vec<UnusedCapture> {
    let Some(uc) = cl.use_clause.as_ref() else { return Vec::new() };
    if uc.variables.iter().all(|v| v.ampersand.is_some()) {
        return Vec::new();
    }
    let mut mentioned = std::collections::HashSet::new();
    let mut dammed = false;
    for s in cl.body.statements.iter() {
        scan_var_mentions(&Node::Statement(s), &mut mentioned, &mut dammed);
    }
    if dammed {
        return Vec::new();
    }
    uc.variables
        .iter()
        .filter(|v| v.ampersand.is_none())
        .filter_map(|v| {
            let name = strip_dollar(bytes_to_string(v.variable.name));
            (!mentioned.contains(&name))
                .then(|| UnusedCapture { name, span: to_span(v.variable.span()) })
        })
        .collect()
}

/// Collect every `$var` token mentioned in a subtree (name without `$`), and set
/// `dammed` when the subtree holds a construct that can read or mint a binding
/// without naming it (`eval`, `include`/`require`, a variable-variable, or
/// `extract`/`compact`/`get_defined_vars`).
///
/// Unlike [`collect_var_reads`] this walk **descends every nested construct**,
/// including closures, arrow functions and their `use (…)` clauses: for the
/// unused-capture question a name mentioned by an inner scope is a use of the
/// outer capture, and over-collection only removes findings.
fn scan_var_mentions(
    node: &Node<'_, '_>,
    mentioned: &mut std::collections::HashSet<String>,
    dammed: &mut bool,
) {
    match node {
        Node::DirectVariable(dv) => {
            mentioned.insert(strip_dollar(bytes_to_string(dv.name)));
        }
        Node::NestedVariable(_)
        | Node::IndirectVariable(_)
        | Node::EvalConstruct(_)
        | Node::IncludeConstruct(_)
        | Node::IncludeOnceConstruct(_)
        | Node::RequireConstruct(_)
        | Node::RequireOnceConstruct(_) => *dammed = true,
        Node::FunctionCall(fc) => {
            if let Expression::Identifier(id) = fc.function
                && matches!(
                    bytes_to_string(id.last_segment()).as_str(),
                    "extract" | "compact" | "get_defined_vars"
                )
            {
                *dammed = true;
            }
        }
        _ => {}
    }
    for child in children(node) {
        scan_var_mentions(&child, mentioned, dammed);
    }
}

// undefined variables (ADR-0078, issue #194)

/// The accumulator behind [`Scope::undefined_reads`]: one scope's binding set, its
/// read sites, and whether a name dam stands anywhere in it.
///
/// Bindings and reads are collected in **one** walk and reconciled only at the end,
/// which is what lets the walk be ordering-blind and duplication-tolerant: a name
/// that is both bound and read (`$x = 1; echo $x;`) filters out no matter which the
/// walk saw first, so binding forms need no read-suppression machinery. Only the
/// positions that bind *nothing* — the `isset`/`empty`/`??`/`unset`/`@` guards —
/// need the walk to actually withhold a read.
#[derive(Default)]
struct VarUsage {
    bound: std::collections::HashSet<String>,
    reads: Vec<UndefinedRead>,
    /// See [`Scope::ref_arg_candidates`] — collected in the same walk but on its own
    /// terms, because a binding form must not depend on a read being recorded.
    arg_candidates: Vec<UndefinedRead>,
    dammed: bool,
}

impl VarUsage {
    /// Record `$name` as bound. An indirect/nested spelling cannot be named, so it
    /// dams instead (`$$n = 1` mints a binding this pass cannot see).
    fn bind_variable(&mut self, var: &mago_syntax::cst::Variable<'_>) {
        match var {
            mago_syntax::cst::Variable::Direct(dv) => {
                self.bound.insert(strip_dollar(bytes_to_string(dv.name)));
            }
            mago_syntax::cst::Variable::Indirect(_) | mago_syntax::cst::Variable::Nested(_) => {
                self.dammed = true;
            }
        }
    }

    fn bind_direct(&mut self, dv: &mago_syntax::cst::DirectVariable<'_>) {
        self.bound.insert(strip_dollar(bytes_to_string(dv.name)));
    }

    /// Record a read of `$x`, unless the engine binds the name unconditionally or
    /// an enclosing same-variable guard shields it (see [`guard_tested_names`]).
    fn read_direct(&mut self, dv: &mago_syntax::cst::DirectVariable<'_>, shielded: &[String]) {
        let name = strip_dollar(bytes_to_string(dv.name));
        if always_bound(&name) || shielded.contains(&name) {
            return;
        }
        self.reads.push(UndefinedRead { name, span: to_span(dv.span()) });
    }
}

/// The variable names an `isset`/`empty` condition **tests**, at either polarity —
/// the shield an enclosing `isset($x) ? … : …` or `if (empty($x)) { … }` casts over
/// its arms.
///
/// This is the `??` discharge idiom in conditional spelling, and it is the same
/// finding-defeating fact: `empty($page) ? 0 : ($page - 1) * $view` reaches the
/// `$page` read only when `$page` is non-empty, hence bound, so this id's runtime
/// claim — "PHP warns and the read evaluates to null" — is simply false there.
///
/// **Not reachability, and deliberately not.** The rule asks only what the
/// condition spells, and then withholds reads in *both* arms without deciding which
/// arm the guard protects. Withholding is the silence direction, so getting the
/// polarity "wrong" costs a finding and never manufactures one — which is what lets
/// a purely syntactic containment test stand in for a flow analysis Steins does not
/// have (the `variable.maybe-undefined` foundation, issue #199).
///
/// `!` is transparent (`!isset($x)` tests `$x` too) and so are parentheses. Nothing
/// else is: a conjunction (`isset($x) && $y`) tests nothing here, which keeps the
/// carve-out to the shape the corpus actually produced.
fn guard_tested_names(cond: &Expression<'_>) -> Vec<String> {
    let mut out = Vec::new();
    collect_guard_tested_names(cond, &mut out);
    out
}

fn collect_guard_tested_names(cond: &Expression<'_>, out: &mut Vec<String>) {
    match cond.unparenthesized() {
        Expression::Construct(Construct::Isset(i)) => {
            for value in i.values.iter() {
                if let Expression::Variable(mago_syntax::cst::Variable::Direct(dv)) =
                    value.unparenthesized()
                {
                    out.push(strip_dollar(bytes_to_string(dv.name)));
                }
            }
        }
        Expression::Construct(Construct::Empty(e)) => {
            if let Expression::Variable(mago_syntax::cst::Variable::Direct(dv)) =
                e.value.unparenthesized()
            {
                out.push(strip_dollar(bytes_to_string(dv.name)));
            }
        }
        Expression::UnaryPrefix(up) if matches!(up.operator, UnaryPrefixOperator::Not(_)) => {
            collect_guard_tested_names(up.operand, out);
        }
        _ => {}
    }
}

/// The shield in force inside a guarded construct's arms: `None` when the condition
/// tests nothing, so the caller keeps borrowing its own slice and no allocation
/// happens on the overwhelmingly common path.
fn extend_shield(base: &[String], added: Vec<String>) -> Option<Vec<String>> {
    if added.is_empty() {
        return None;
    }
    let mut extended = base.to_vec();
    extended.extend(added);
    Some(extended)
}

/// Names PHP itself always provides, so a read of one is never undefined: the nine
/// superglobals, `$this`, and `$http_response_header` — which the HTTP stream
/// wrappers mint into whatever scope performed the request, with nothing in the
/// scope's own text to show for it.
fn always_bound(name: &str) -> bool {
    name == "this" || name == "http_response_header" || SUPERGLOBALS.contains(&name)
}

/// Bind the **root local** of an lvalue, and nothing else.
///
/// `$x = …` binds `x`; so does `$x['k'] = …` (witnessed: the offset write
/// auto-vivifies `$x` with no warning at 8.5.9) and `$x->p = …`. The *index* of an
/// offset write is an ordinary read and is left to the main walk, which visits it
/// anyway. Destructuring recurses into every element, so `[$a, [$b]] = …` and
/// `list(, $b) = …` bind exactly the names they write. A non-lvalue shape binds
/// nothing — this is called on argument positions too, where `f($a + $b)` must not
/// pretend to bind.
fn bind_lvalue_roots(expr: &Expression<'_>, acc: &mut VarUsage) {
    // Issue #264: `$a[0][0][…] = …` walks one frame per subscript.
    if stack_guard::exhausted() {
        return;
    }
    match expr.unparenthesized() {
        Expression::Variable(v) => acc.bind_variable(v),
        Expression::ArrayAccess(aa) => bind_lvalue_roots(aa.array, acc),
        Expression::ArrayAppend(ap) => bind_lvalue_roots(ap.array, acc),
        Expression::Access(Access::Property(pa)) => bind_lvalue_roots(pa.object, acc),
        Expression::Access(Access::NullSafeProperty(pa)) => bind_lvalue_roots(pa.object, acc),
        Expression::Array(a) => bind_destructured(a.elements.iter(), acc),
        Expression::LegacyArray(a) => bind_destructured(a.elements.iter(), acc),
        Expression::List(l) => bind_destructured(l.elements.iter(), acc),
        // `$a = &$b` binds `$b` as well as `$a` (witnessed: no warning, and the two
        // names alias from then on).
        Expression::UnaryPrefix(up) if matches!(up.operator, UnaryPrefixOperator::Reference(_)) => {
            bind_lvalue_roots(up.operand, acc);
        }
        _ => {}
    }
}

/// Bind every destructuring target of an array/list pattern. A `Missing` element
/// (`[, $b]`) writes nothing, and a key is a read rather than a target.
fn bind_destructured<'a>(
    elements: impl Iterator<Item = &'a ArrayElement<'a>>,
    acc: &mut VarUsage,
) {
    for element in elements {
        match element {
            ArrayElement::KeyValue(kv) => bind_lvalue_roots(kv.value, acc),
            ArrayElement::Value(v) => bind_lvalue_roots(v.value, acc),
            ArrayElement::Variadic(v) => bind_lvalue_roots(v.value, acc),
            ArrayElement::Missing(_) => {}
        }
    }
}

/// Bind the bare-variable arguments of a call whose target this pass cannot name —
/// a method, static, dynamic or constructor call. Any of them may declare `&$p`,
/// and with no resolvable callee spelling there is nothing for the checker's
/// out-parameter oracle to ask, so the closed-world-safe reading is that every
/// argument position might be an out-parameter.
fn bind_call_arguments(list: &mago_syntax::cst::ArgumentList<'_>, acc: &mut VarUsage) {
    for arg in list.arguments.iter() {
        bind_lvalue_roots(arg.value(), acc);
    }
}

/// The [`bind_call_arguments`] analogue for a partial-application argument list
/// (`new class ($x) {…}`), whose placeholders carry no value.
fn bind_partial_arguments(list: &mago_syntax::cst::PartialArgumentList<'_>, acc: &mut VarUsage) {
    for arg in list.arguments.iter() {
        match arg {
            PartialArgument::Positional(p) => bind_lvalue_roots(p.value, acc),
            PartialArgument::Named(n) => bind_lvalue_roots(n.value, acc),
            PartialArgument::NamedPlaceholder(_)
            | PartialArgument::Placeholder(_)
            | PartialArgument::VariadicPlaceholder(_) => {}
        }
    }
}

/// Read one variable in **local** position — the inner name of a dynamic
/// static-property spelling (`Server::$$v`), which is an ordinary read of `$v`.
/// A further indirection (`Server::$$$v`) reaches a local whose name is computed,
/// which is the `$$x` dam.
fn scan_local_variable(
    var: &mago_syntax::cst::Variable<'_>,
    guarded: bool,
    shielded: &[String],
    acc: &mut VarUsage,
) {
    match var {
        mago_syntax::cst::Variable::Direct(dv) => {
            if !guarded {
                acc.read_direct(dv, shielded);
            }
        }
        mago_syntax::cst::Variable::Indirect(_) | mago_syntax::cst::Variable::Nested(_) => {
            acc.dammed = true;
        }
    }
}

/// The single walk behind [`Scope::undefined_reads`]: collect this scope's binding
/// forms, its read sites, and its name dams, without descending into any nested
/// scope.
///
/// `guarded` marks a subtree PHP legalizes a read in (`isset`/`empty`/`unset`, the
/// left operand of `??`, and the `@` error-control operand — all witnessed silent at
/// 8.5.9). Bindings are still collected there; only the read is withheld.
fn scan_var_usage(node: &Node<'_, '_>, guarded: bool, shielded: &[String], acc: &mut VarUsage) {
    match node {
        // --- Nested scopes: their reads are their own scope's question. ---
        //
        // A closure is the one nested scope that still speaks about THIS one: a
        // by-value `use ($x)` reads the enclosing binding (witnessed: warns at the
        // use clause), while a by-ref `use (&$x)` *creates* it (witnessed: silent,
        // and the name reads back null afterwards).
        Node::Closure(cl) => {
            if let Some(uc) = cl.use_clause.as_ref() {
                for v in uc.variables.iter() {
                    if v.ampersand.is_some() {
                        acc.bind_direct(&v.variable);
                    } else if !guarded {
                        acc.read_direct(&v.variable, shielded);
                    }
                }
            }
            return;
        }
        Node::ArrowFunction(_)
        | Node::Function(_)
        | Node::Class(_)
        | Node::Interface(_)
        | Node::Trait(_)
        | Node::Enum(_) => return,
        Node::AnonymousClass(ac) => {
            if let Some(list) = ac.argument_list.as_ref() {
                bind_partial_arguments(list, acc);
            }
            return;
        }

        // --- Name dams: a construct that can mint or consume a binding without
        // spelling it. The same set `unused_by_value_captures` dams on. ---
        Node::NestedVariable(_)
        | Node::IndirectVariable(_)
        | Node::EvalConstruct(_)
        | Node::IncludeConstruct(_)
        | Node::IncludeOnceConstruct(_)
        | Node::RequireConstruct(_)
        | Node::RequireOnceConstruct(_) => acc.dammed = true,
        Node::FunctionCall(fc) => {
            // Out-parameter candidates, recorded whatever the guard state — a binding
            // form must not depend on its argument occurrence being collected as a
            // read. See `Scope::ref_arg_candidates`.
            for arg in fc.argument_list.arguments.iter() {
                if let Argument::Positional(p) = arg
                    && p.ellipsis.is_none()
                    && let Expression::Variable(mago_syntax::cst::Variable::Direct(dv)) =
                        p.value.unparenthesized()
                {
                    let name = strip_dollar(bytes_to_string(dv.name));
                    if !always_bound(&name) {
                        acc.arg_candidates.push(UndefinedRead { name, span: to_span(dv.span()) });
                    }
                }
            }
            if let Expression::Identifier(id) = fc.function
                && matches!(
                    bytes_to_string(id.last_segment()).as_str(),
                    "extract" | "compact" | "get_defined_vars"
                )
            {
                // `extract` mints; `$$x` mints; `get_defined_vars` consumes the whole
                // table. `compact` only READS names, spelled as strings, and answers
                // an undefined one with its OWN warning
                // (`compact(): Undefined variable $nope`, witnessed at 8.5.9) rather
                // than this id's — so it cannot un-prove a binding. It dams anyway,
                // matching `closure.unused-use`: the cost is silence in a scope that
                // is already handling names as data, and the alternative is a finding
                // whose sentence would describe the wrong warning.
                acc.dammed = true;
            }
        }

        // --- Same-variable guarded constructs: the `??` discharge idiom in
        // conditional spelling. `empty($page) ? 0 : ($page - 1) * $view` reaches
        // the `$page` read only when `$page` is non-empty, hence bound, so this
        // id's runtime claim is false there. Both arms are shielded without asking
        // which one the guard protects — see `guard_tested_names` for why that is
        // a containment rule and not a reachability one. Only the TESTED name is
        // shielded, so `$view` above is still judged. ---
        Node::Conditional(c) => {
            scan_var_usage(&Node::Expression(c.condition), guarded, shielded, acc);
            let extended = extend_shield(shielded, guard_tested_names(c.condition));
            let inner = extended.as_deref().unwrap_or(shielded);
            // `?:` has no `then` arm; its condition IS the value, already walked.
            if let Some(then) = c.then {
                scan_var_usage(&Node::Expression(then), guarded, inner, acc);
            }
            scan_var_usage(&Node::Expression(c.r#else), guarded, inner, acc);
            return;
        }
        // The statement spelling of the same idiom. It needs no block-scoped
        // tracking: the `if`'s whole body — including its `elseif`/`else` clauses —
        // is one subtree, and shielding all of it is the same silence-direction
        // containment rule. A read AFTER the `if` is outside that subtree and is
        // still judged.
        Node::If(i) => {
            scan_var_usage(&Node::Expression(i.condition), guarded, shielded, acc);
            let extended = extend_shield(shielded, guard_tested_names(i.condition));
            let inner = extended.as_deref().unwrap_or(shielded);
            scan_var_usage(&Node::IfBody(&i.body), guarded, inner, acc);
            return;
        }

        // --- Guards: PHP legalizes the read, so it is not this finding. ---
        Node::IssetConstruct(_) | Node::EmptyConstruct(_) | Node::Unset(_) => {
            for child in children(node) {
                scan_var_usage(&child, true, shielded, acc);
            }
            return;
        }
        Node::UnaryPrefix(up) if up.operator.is_error_control() => {
            scan_var_usage(&Node::Expression(up.operand), true, shielded, acc);
            return;
        }
        Node::Binary(b) if b.operator.is_null_coalesce() => {
            scan_var_usage(&Node::Expression(b.lhs), true, shielded, acc);
            scan_var_usage(&Node::Expression(b.rhs), guarded, shielded, acc);
            return;
        }

        // --- Binding forms. None of these return: the main recursion may re-visit
        // the very same token as a read, which the final set difference discards. ---
        Node::Assignment(a) => bind_lvalue_roots(a.lhs, acc),
        Node::Global(g) => {
            for v in g.variables.iter() {
                acc.bind_variable(v);
            }
        }
        Node::Static(s) => {
            for item in s.items.iter() {
                acc.bind_direct(item.variable());
            }
        }
        Node::TryCatchClause(tc) => {
            if let Some(v) = tc.variable.as_ref() {
                acc.bind_direct(v);
            }
        }
        Node::ForeachValueTarget(t) => bind_lvalue_roots(t.value, acc),
        Node::ForeachKeyValueTarget(t) => {
            bind_lvalue_roots(t.key, acc);
            bind_lvalue_roots(t.value, acc);
        }
        // `&$x` (reference), `++$x` / `--$x` and `$x++` / `$x--` all write through
        // the operand, so each is a binding form.
        Node::UnaryPrefix(up)
            if matches!(
                up.operator,
                UnaryPrefixOperator::Reference(_)
                    | UnaryPrefixOperator::PreIncrement(_)
                    | UnaryPrefixOperator::PreDecrement(_)
            ) =>
        {
            bind_lvalue_roots(up.operand, acc);
        }
        Node::UnaryPostfix(up) => bind_lvalue_roots(up.operand, acc),
        // Calls whose target cannot be named here — see `bind_call_arguments`.
        Node::MethodCall(c) => bind_call_arguments(&c.argument_list, acc),
        Node::NullSafeMethodCall(c) => bind_call_arguments(&c.argument_list, acc),
        Node::StaticMethodCall(c) => bind_call_arguments(&c.argument_list, acc),
        Node::Instantiation(i) => {
            if let Some(list) = i.argument_list.as_ref() {
                bind_call_arguments(list, acc);
            }
        }
        // A named argument binds too: `lower_argument_list` records the whole
        // `name: value` span for one, so the checker's span-keyed out-parameter
        // subtraction cannot reach it.
        Node::NamedArgument(n) => bind_lvalue_roots(n.value, acc),

        // --- The one position where a `$name` token is NOT a local. ---
        //
        // `Server::$url` spells a **static property**, whose `$url` names a slot on
        // the class, not a variable in this frame (witnessed silent at 8.5.9, and
        // the same for `static::`/`self::`/`parent::`). Left to the generic read
        // arm below this is a false positive on one of the most common shapes in
        // legacy PHP, so the property token is skipped here — while the class
        // expression, which may well be a local (`$obj::$url`), is still walked.
        //
        // The dynamic spellings behave the other way round: `Server::$$v` and
        // `Server::${$v}` name the property at runtime, so `$v` IS an ordinary
        // local read (witnessed: `Server::$$nope` warns `Undefined variable $nope`
        // before it fatals on the empty property name). They are deliberately NOT
        // dams either, which is consistent with the `$$x` dam rather than an
        // exception to it: that dam exists because a variable-variable can mint or
        // consume a **local** binding, and an indirection in this position reaches
        // the class's static table instead, where no local can be minted.
        Node::StaticPropertyAccess(spa) => {
            scan_var_usage(&Node::Expression(spa.class), guarded, shielded, acc);
            match &spa.property {
                mago_syntax::cst::Variable::Direct(_) => {}
                mago_syntax::cst::Variable::Indirect(iv) => {
                    scan_var_usage(&Node::Expression(iv.expression), guarded, shielded, acc);
                }
                mago_syntax::cst::Variable::Nested(nv) => {
                    scan_local_variable(nv.variable, guarded, shielded, acc);
                }
            }
            return;
        }

        // --- Reads. ---
        Node::DirectVariable(dv) if !guarded => acc.read_direct(dv, shielded),
        _ => {}
    }
    for child in children(node) {
        scan_var_usage(&child, guarded, shielded, acc);
    }
}

/// The reads of names a scope never binds (issue #194) — the computation behind
/// [`Scope::undefined_reads`], done here because it needs the CST the lowered trace
/// deliberately forgets.
///
/// `params` seeds the binding set with the scope's own parameters (promoted
/// constructor properties included — they are ordinary entries of the same list),
/// and `use_clause` with a closure's captures, by value and by reference alike.
/// `None` for both is a top-level or arrow scope, which reports nothing at all; see
/// [`Scope::undefined_reads`] for why each is silent.
fn undefined_variable_reads(
    params: Option<&mago_syntax::cst::FunctionLikeParameterList<'_>>,
    use_clause: Option<&mago_syntax::cst::ClosureUseClause<'_>>,
    statements: &[&Statement<'_>],
) -> ScopeVarFacts {
    let mut acc = VarUsage::default();
    let Some(params) = params else { return ScopeVarFacts::default() };
    for p in params.parameters.iter() {
        acc.bind_direct(&p.variable);
    }
    if let Some(uc) = use_clause {
        for v in uc.variables.iter() {
            acc.bind_direct(&v.variable);
        }
    }
    for s in statements {
        scan_var_usage(&Node::Statement(s), false, &[], &mut acc);
    }
    if acc.dammed {
        return ScopeVarFacts::default();
    }
    let VarUsage { bound, reads, arg_candidates, .. } = acc;
    // A read whose name the scope binds *somewhere* is never the definite id's —
    // that id is ordering-blind by contract. It is the presence pass's candidate
    // instead, and running that pass at all is worth it only when one exists.
    let has_presence_candidate = reads.iter().any(|r| bound.contains(&r.name));
    let definite: Vec<UndefinedRead> =
        reads.into_iter().filter(|r| !bound.contains(&r.name)).collect();
    let maybe = if has_presence_candidate {
        maybe_undefined_reads(params, use_clause, statements, &bound)
    } else {
        Vec::new()
    };
    if definite.is_empty() && maybe.is_empty() {
        // Nothing to subtract from: keep the candidate list off every scope that
        // cannot report, which is nearly all of them.
        return ScopeVarFacts::default();
    }
    let judged: HashSet<String> =
        definite.iter().chain(maybe.iter()).map(|r| r.name.clone()).collect();
    let arg_candidates =
        arg_candidates.into_iter().filter(|c| judged.contains(&c.name)).collect();
    ScopeVarFacts {
        undefined_reads: definite,
        maybe_undefined_reads: maybe,
        ref_arg_candidates: arg_candidates,
    }
}

/// The three lists [`undefined_variable_reads`] produces for one scope — the reads
/// to judge on each leg of the pair, and the out-parameter candidates the checker
/// must subtract first.
#[derive(Default)]
struct ScopeVarFacts {
    undefined_reads: Vec<UndefinedRead>,
    maybe_undefined_reads: Vec<UndefinedRead>,
    ref_arg_candidates: Vec<UndefinedRead>,
}

// end undefined variables (ADR-0078, issue #194)

// binding presence (ADR-0081, issue #267)

/// Whether a name carries a binding at a program point, over the three-valued
/// lattice ADR-0081 §2 fixes: `Bound ⊔ Unbound = Maybe`.
///
/// This **mirrors** [`steins_domain::Presence`]'s documented join rather than
/// reusing the type: that enum's `Required` arm carries a `witnessed` provenance
/// bit (the Verified/Asserted split of the array-shape engine), and a local
/// binding has no such stratum to record — a parameter and an assignment bind
/// identically. The join is the same relation with the same three outcomes:
/// agreement survives, disagreement degrades to the middle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BindingPresence {
    /// Every path from scope entry to this point carries a binding.
    Bound,
    /// Some paths carry a binding and some do not.
    Maybe,
    /// No path carries a binding.
    Unbound,
}

impl BindingPresence {
    /// The join of two paths meeting at a control-flow merge.
    fn join(self, other: Self) -> Self {
        match (self, other) {
            (Self::Bound, Self::Bound) => Self::Bound,
            (Self::Unbound, Self::Unbound) => Self::Unbound,
            _ => Self::Maybe,
        }
    }
}

/// The presence state at one program point. A name absent from the map is
/// [`BindingPresence::Unbound`] — scope entry is the empty map plus the
/// parameters and captures, which is exactly PHP's own frame.
type PresenceState = HashMap<String, BindingPresence>;

/// Join two states at a control-flow merge, over the union of their names.
fn join_states(a: &PresenceState, b: &PresenceState) -> PresenceState {
    let mut out = PresenceState::with_capacity(a.len().max(b.len()));
    for (name, pa) in a {
        let pb = b.get(name).copied().unwrap_or(BindingPresence::Unbound);
        out.insert(name.clone(), pa.join(pb));
    }
    for (name, pb) in b {
        if !a.contains_key(name) {
            out.insert(name.clone(), pb.join(BindingPresence::Unbound));
        }
    }
    out
}

/// Refine a state by a boundness guard: every listed name is [`BindingPresence::Bound`]
/// on this continuation. Guards only ever refine *toward* boundness (ADR-0081 §5) —
/// `isset` is false on a bound null, so no polarity can prove absence.
fn refine_bound(state: &mut PresenceState, names: &[String]) {
    for name in names {
        state.insert(name.clone(), BindingPresence::Bound);
    }
}

/// Where control leaves a statement list, at the granularity the presence pass
/// needs. Deliberately finer than [`BodyEnd`], because the three ways out land in
/// three different places and only one of them is the enclosing statement's
/// successor:
///
/// * a `break` leaves for the enclosing **loop or switch**'s successor — so a
///   `switch` arm ending in `break` stays in the switch's arm join (`switch { case
///   1: $x = 1; break; default: $x = 2; break; }` binds on every arm), while an
///   `if` arm ending in `break` does **not** reach the `if`'s successor;
/// * a `continue` leaves for the enclosing loop's **back edge**;
/// * `return`/`throw`/`exit` leave the scope entirely.
///
/// The states carried out by the first two are not lost: [`PresenceCx`] collects
/// them, and the enclosing loop or switch joins them where they actually arrive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PresenceFlow {
    /// Control reached the end of the list.
    Fell,
    /// A `break` left for the enclosing loop or switch's successor.
    Broke,
    /// A `continue` left for the enclosing loop's back edge.
    Continued,
    /// `return`/`throw`/`exit`: the list's state reaches no successor at all, and
    /// this is the arm ADR-0081 §3 subtracts from a branch join.
    Terminated,
}

/// The accumulator of the presence pass: the reads it judged, and the premises it
/// judges them against.
struct PresenceCx<'a> {
    /// The scope's whole binding set — the definite pass's `bound`. A read of a
    /// name absent from it is `variable.undefined`'s, never this id's (ADR-0081 §6),
    /// which is what makes the pair disjoint by construction.
    scope_bound: &'a HashSet<String>,
    /// Reads whose presence was `Maybe` or `Unbound`.
    out: Vec<UndefinedRead>,
    /// Set while a loop body is being walked for its fixpoint rather than for its
    /// findings: the state is not yet stable, so nothing may be reported from it.
    silent: bool,
    /// Read spans already reported — a loop body is walked more than once.
    seen: HashSet<u32>,
    /// The states carried out by `break`, waiting for the enclosing loop or switch
    /// to join them into its successor. Saved and cleared around each such
    /// construct, so a jump is never credited to the wrong one.
    breaks: Vec<PresenceState>,
    /// The states carried out by `continue`, waiting for the enclosing loop's back
    /// edge — and, for a loop that can exit by its condition, its successor too.
    continues: Vec<PresenceState>,
}

impl PresenceCx<'_> {
    fn record(&mut self, read: &UndefinedRead, state: &PresenceState) {
        if self.silent || !self.scope_bound.contains(&read.name) {
            return;
        }
        let presence =
            state.get(&read.name).copied().unwrap_or(BindingPresence::Unbound);
        if presence == BindingPresence::Bound {
            return;
        }
        if self.seen.insert(read.span.start) {
            self.out.push(read.clone());
        }
    }
}

/// Judge one **leaf unit** — a statement with no control-flow structure of its
/// own, or a branch condition — against `state`, then apply its bindings.
///
/// Reads are judged against the *pre-unit* state and bindings applied after, which
/// is what makes `$y = $x; $x = 1;` report at the first statement. **Within** a
/// unit the definite pass's ordering-blindness is kept: a name the unit binds
/// anywhere has its reads in that unit withheld, so `$a = ($b = 1) + $b;` and
/// `$x .= 'a'` cannot manufacture a finding out of an intra-expression evaluation
/// order this pass does not model.
fn presence_leaf(node: &Node<'_, '_>, state: &mut PresenceState, cx: &mut PresenceCx) {
    let mut acc = VarUsage::default();
    let mut shield = Vec::new();
    collect_presence_shield(node, &mut shield);
    scan_var_usage(node, false, &shield, &mut acc);
    for read in &acc.reads {
        if acc.bound.contains(&read.name) {
            continue;
        }
        cx.record(read, state);
    }
    for name in acc.bound {
        state.insert(name, BindingPresence::Bound);
    }
}

/// Every name an `isset`/`empty` construct anywhere in this unit tests — the shield
/// the presence pass casts over the *whole* unit.
///
/// [`guard_tested_names`] is deliberately shallow (it reads a condition's top-level
/// spelling only), which is right for the definite pass: there, a read of a name the
/// scope never binds is a finding wherever it sits. Here it is not enough —
/// `if (isset($x) && $x > 1)` reaches the `$x > 1` read only when `$x` is bound, and
/// judging the condition as one unit against the pre-`if` state would report it.
/// Over-shielding costs recall and never manufactures a finding, which is the same
/// trade `guard_tested_names` already documents.
fn collect_presence_shield(node: &Node<'_, '_>, out: &mut Vec<String>) {
    match node {
        Node::Function(_)
        | Node::Closure(_)
        | Node::ArrowFunction(_)
        | Node::AnonymousClass(_)
        | Node::Class(_)
        | Node::Interface(_)
        | Node::Trait(_)
        | Node::Enum(_) => return,
        Node::IssetConstruct(_) | Node::EmptyConstruct(_) => {
            collect_direct_variable_names(node, out);
            return;
        }
        _ => {}
    }
    for child in node.children() {
        collect_presence_shield(&child, out);
    }
}

/// Every `$name` token in a subtree, without descending into a nested scope.
fn collect_direct_variable_names(node: &Node<'_, '_>, out: &mut Vec<String>) {
    match node {
        Node::Function(_)
        | Node::Closure(_)
        | Node::ArrowFunction(_)
        | Node::AnonymousClass(_)
        | Node::Class(_)
        | Node::Interface(_)
        | Node::Trait(_)
        | Node::Enum(_) => return,
        Node::DirectVariable(dv) => out.push(strip_dollar(bytes_to_string(dv.name))),
        _ => {}
    }
    for child in node.children() {
        collect_direct_variable_names(&child, out);
    }
}

/// The names a condition **proves bound** on one of its continuations
/// (ADR-0081 §5).
///
/// `want_true` asks for the true-continuation. `isset($x)` proves `$x` bound when
/// true; `empty($x)` and `!isset($x)` prove it bound when **false** — which is the
/// whole reason the defaulting idiom `if (!isset($x)) { $x = f(); } use($x);` is
/// silent: the then-arm binds and the implicit else-arm holds `isset($x)`.
///
/// `&&` conjoins on the true side (both operands hold), `||` on the false side
/// (neither holds). No polarity ever refines toward absence: `isset` is false on a
/// bound null, so a false `isset($x)` proves nothing about the binding.
fn guard_bound_names(cond: &Expression<'_>, want_true: bool, out: &mut Vec<String>) {
    match cond.unparenthesized() {
        Expression::Construct(Construct::Isset(i)) if want_true => {
            for value in i.values.iter() {
                push_guard_root(value, out);
            }
        }
        Expression::Construct(Construct::Empty(e)) if !want_true => {
            push_guard_root(e.value, out);
        }
        Expression::UnaryPrefix(up) if matches!(up.operator, UnaryPrefixOperator::Not(_)) => {
            guard_bound_names(up.operand, !want_true, out);
        }
        Expression::Binary(b)
            if want_true
                && matches!(b.operator, BinaryOperator::And(_) | BinaryOperator::LowAnd(_)) =>
        {
            guard_bound_names(b.lhs, true, out);
            guard_bound_names(b.rhs, true, out);
        }
        Expression::Binary(b)
            if !want_true
                && matches!(b.operator, BinaryOperator::Or(_) | BinaryOperator::LowOr(_)) =>
        {
            guard_bound_names(b.lhs, false, out);
            guard_bound_names(b.rhs, false, out);
        }
        _ => {}
    }
}

/// The **root local** a guarded expression reads through: `$x`, `$x['a']['b']`,
/// `$x->p` and `$x->p['k']` all reach `x`, and nothing else does.
///
/// `isset($info['subject']['commonName'])` cannot be true unless `$info` is bound
/// (witnessed: PHP evaluates the whole chain and answers false at the first missing
/// link, without warning), so the root is exactly as guarded as a bare `isset($x)`
/// would make it. This is the shape the corpus produces far more often than the
/// bare one — the `if (!isset($info['subject']['commonName'])) { return null; }`
/// prologue — and reading only the bare spelling reported every read after it.
fn push_guard_root(expr: &Expression<'_>, out: &mut Vec<String>) {
    match expr.unparenthesized() {
        Expression::Variable(Variable::Direct(dv)) => {
            out.push(strip_dollar(bytes_to_string(dv.name)));
        }
        Expression::ArrayAccess(aa) => push_guard_root(aa.array, out),
        Expression::Access(Access::Property(pa)) => push_guard_root(pa.object, out),
        Expression::Access(Access::NullSafeProperty(pa)) => push_guard_root(pa.object, out),
        _ => {}
    }
}

fn guarded_names(cond: &Expression<'_>, want_true: bool) -> Vec<String> {
    let mut out = Vec::new();
    guard_bound_names(cond, want_true, &mut out);
    out
}

/// Walk an ordered statement list, threading `state` through it, and answer where
/// control left it. The scan stops at the first statement that does not fall
/// through — everything after it is unreachable, and judging unreachable reads
/// against a state no execution has would be a claim about nothing.
fn presence_seq<'ast, 'arena>(
    stmts: impl IntoIterator<Item = &'ast Statement<'arena>>,
    state: &mut PresenceState,
    cx: &mut PresenceCx,
) -> PresenceFlow
where
    'arena: 'ast,
{
    for s in stmts {
        match presence_stmt(s, state, cx) {
            PresenceFlow::Fell => {}
            other => return other,
        }
    }
    PresenceFlow::Fell
}

/// The presence transfer function for one statement.
fn presence_stmt(
    s: &Statement<'_>,
    state: &mut PresenceState,
    cx: &mut PresenceCx,
) -> PresenceFlow {
    match s {
        Statement::Break(_) => {
            cx.breaks.push(state.clone());
            PresenceFlow::Broke
        }
        Statement::Continue(_) => {
            cx.continues.push(state.clone());
            PresenceFlow::Continued
        }
        Statement::Block(b) => presence_seq(b.statements.iter(), state, cx),
        Statement::If(i) => presence_if(i, state, cx),
        Statement::Switch(sw) => presence_switch(sw, state, cx),
        Statement::While(w) => {
            presence_leaf(&Node::Expression(w.condition), state, cx);
            let mut entry = state.clone();
            refine_bound(&mut entry, &guarded_names(w.condition, true));
            let exits = presence_loop_body(w.body.statements(), &entry, cx);
            if expr_is_true(w.condition) {
                // No false-condition exit edge: zero iterations is impossible and
                // the back edge never leaves, so only a `break` reaches the
                // successor.
                *state = join_loop_exit(&entry, exits, false);
            } else {
                let after = join_loop_exit(&entry, exits, true);
                *state = join_states(state, &after);
                refine_bound(state, &guarded_names(w.condition, false));
            }
            PresenceFlow::Fell
        }
        Statement::DoWhile(d) => {
            // The body runs at least once, so there is no zero-iteration path to
            // join in — but the condition can still be false, so the back edge does
            // reach the successor.
            let entry = state.clone();
            let exits = presence_loop_body(std::slice::from_ref(d.statement), &entry, cx);
            *state = join_loop_exit(&entry, exits, true);
            presence_leaf(&Node::Expression(d.condition), state, cx);
            PresenceFlow::Fell
        }
        Statement::For(f) => {
            for init in f.initializations.iter() {
                presence_leaf(&Node::Expression(init), state, cx);
            }
            for cond in f.conditions.iter() {
                presence_leaf(&Node::Expression(cond), state, cx);
            }
            let last = f.conditions.iter().next_back();
            let mut entry = state.clone();
            if let Some(cond) = last {
                refine_bound(&mut entry, &guarded_names(cond, true));
            }
            let mut exits = presence_loop_body(f.body.statements(), &entry, cx);
            // The increments run on the back edge, not on a `break`.
            for inc in f.increments.iter() {
                presence_leaf(&Node::Expression(inc), &mut exits.looped, cx);
            }
            if last.is_none_or(|c| expr_is_true(c)) {
                *state = join_loop_exit(&entry, exits, false);
            } else {
                let after = join_loop_exit(&entry, exits, true);
                *state = join_states(state, &after);
            }
            PresenceFlow::Fell
        }
        Statement::Foreach(fe) => {
            presence_leaf(&Node::Expression(fe.expression), state, cx);
            let mut entry = state.clone();
            let mut targets = VarUsage::default();
            match &fe.target {
                mago_syntax::cst::ForeachTarget::Value(t) => {
                    bind_lvalue_roots(t.value, &mut targets);
                }
                mago_syntax::cst::ForeachTarget::KeyValue(t) => {
                    bind_lvalue_roots(t.key, &mut targets);
                    bind_lvalue_roots(t.value, &mut targets);
                }
            }
            for name in targets.bound {
                entry.insert(name, BindingPresence::Bound);
            }
            let exits = presence_loop_body(fe.body.statements(), &entry, cx);
            let after = join_loop_exit(&entry, exits, true);
            // Zero iterations is always possible over an empty subject — which is
            // exactly why `foreach ($xs as $v) {} echo $v;` is this id and not
            // silence.
            *state = join_states(state, &after);
            PresenceFlow::Fell
        }
        Statement::Try(t) => presence_try(t, state, cx),
        _ => {
            presence_leaf(&Node::Statement(s), state, cx);
            refine_bound(state, &assert_bound_names(s));
            if stmt_end(s).provably_terminates() {
                PresenceFlow::Terminated
            } else {
                PresenceFlow::Fell
            }
        }
    }
}

/// The names a statement-position `assert()` proves bound in everything after it.
///
/// `assert(isset($x));` is a boundness guard whose *only* continuation is the
/// true-polarity one: ADR-0052 slice I0 already reads `assert()` as Verified
/// evidence, and with assertions enabled a failed one throws `AssertionError`
/// (witnessed at 8.5.9 under `zend.assertions=1`), so control reaches the next
/// statement exactly when the condition held. With assertions compiled out the call
/// does not run at all — and then neither does anything the assertion was
/// protecting, so the refinement cannot manufacture a claim either way.
///
/// The polarity is [`guard_bound_names`]' own, so `assert(isset($x) && $x > 1)`
/// refines through the conjunction and `assert(!isset($x))` refines nothing —
/// there is no continuation on which that spelling proves a binding.
fn assert_bound_names(s: &Statement<'_>) -> Vec<String> {
    let Statement::Expression(es) = s else {
        return Vec::new();
    };
    let Expression::Call(Call::Function(fc)) = es.expression.unparenthesized() else {
        return Vec::new();
    };
    let Expression::Identifier(id) = fc.function else {
        return Vec::new();
    };
    if !bytes_to_string(id.last_segment()).eq_ignore_ascii_case("assert") {
        return Vec::new();
    }
    // The first argument is the condition; a second is the description, which
    // asserts nothing. A named or spread argument is not this shape.
    let Some(Argument::Positional(first)) = fc.argument_list.arguments.iter().next() else {
        return Vec::new();
    };
    if first.ellipsis.is_some() {
        return Vec::new();
    }
    guarded_names(first.value, true)
}

/// The `if`/`elseif`/`else` chain: each arm is evaluated from the pre-branch state
/// refined by its own guard polarity, and the arms that reach the successor are
/// joined. **An arm that provably terminates drops out of the join** — ADR-0081 §3,
/// and [`BodyEnd::provably_terminates`]'s first production consumer, reached here
/// through [`PresenceFlow`] so that a `break` is not mistaken for a terminator.
///
/// Conditions are evaluated in written order against the running state, so an
/// `elseif` reads what the preceding conditions bound, and every preceding
/// condition's *false* polarity is in force by the time it is judged.
fn presence_if(
    i: &mago_syntax::cst::If<'_>,
    state: &mut PresenceState,
    cx: &mut PresenceCx,
) -> PresenceFlow {
    let body = &i.body;
    let mut chain: Vec<(&Expression<'_>, &[Statement<'_>])> =
        vec![(i.condition, body.statements())];
    chain.extend(body.else_if_clauses());

    let mut arms: Vec<PresenceState> = Vec::new();
    let mut open = true;
    for (cond, stmts) in chain {
        presence_leaf(&Node::Expression(cond), state, cx);
        if expr_is_false(cond) {
            // The arm can never be taken; it contributes no path (`if_end`'s rule).
            continue;
        }
        let mut arm = state.clone();
        refine_bound(&mut arm, &guarded_names(cond, true));
        // Only a fall-through arm reaches THIS statement's successor. A `break` or
        // `continue` arm leaves for an enclosing construct, which joins its state
        // where it actually arrives — keeping it here is what would report
        // `foreach (…) { if (A) { $p = …; } elseif (B) { $p = …; } else { continue; }
        // use($p); }`, the shape the corpus produces most.
        if presence_seq(stmts.iter(), &mut arm, cx) == PresenceFlow::Fell {
            arms.push(arm);
        }
        if expr_is_true(cond) {
            // Always taken: no later arm and no no-branch path exist.
            open = false;
            break;
        }
        refine_bound(state, &guarded_names(cond, false));
    }
    if open {
        match body.else_statements() {
            Some(stmts) => {
                let mut arm = state.clone();
                if presence_seq(stmts.iter(), &mut arm, cx) == PresenceFlow::Fell {
                    arms.push(arm);
                }
            }
            // No `else`: the no-branch-taken path runs straight to the successor,
            // carrying whatever the conditions' false polarities established.
            None => arms.push(state.clone()),
        }
    }
    let Some(joined) = arms.into_iter().reduce(|a, b| join_states(&a, &b)) else {
        // No arm falls through. Whatever each one did — terminate or jump — nothing
        // after this `if` in this list runs, and the jump states are already parked
        // on the enclosing construct.
        return PresenceFlow::Terminated;
    };
    *state = joined;
    PresenceFlow::Fell
}

/// A `switch`: every non-empty case body is evaluated from the pre-switch state and
/// the surviving arms are joined, with the implicit no-match arm joined in when
/// there is no `default`.
///
/// Case entry is deliberately the **pre-switch** state and not the previous case's
/// exit: PHP enters a case directly on a match, so a name the previous case bound is
/// genuinely absent there. Fall-through only ever *adds* a path, and adding it would
/// weaken nothing.
fn presence_switch(
    sw: &mago_syntax::cst::Switch<'_>,
    state: &mut PresenceState,
    cx: &mut PresenceCx,
) -> PresenceFlow {
    presence_leaf(&Node::Expression(sw.expression), state, cx);
    let pre = state.clone();
    // A `break` in a case body targets THIS switch, so its state is ours to join.
    // A `continue` targets the enclosing loop and must stay parked for it.
    let outer_breaks = std::mem::take(&mut cx.breaks);
    let mut arms: Vec<PresenceState> = Vec::new();
    let mut has_default = false;
    for case in sw.body.cases() {
        match case.expression() {
            Some(e) => {
                let mut probe = pre.clone();
                presence_leaf(&Node::Expression(e), &mut probe, cx);
            }
            None => has_default = true,
        }
        if case.is_empty() {
            // `case 1: case 2: body` — an empty label contributes no arm of its own.
            continue;
        }
        let mut arm = pre.clone();
        // A case body that falls off its end runs into the NEXT case rather than
        // past the switch, so only its `break` state — already parked — reaches the
        // successor. Keeping the fall-off state here would be the fall-through edge
        // this pass does not model.
        if presence_seq(case.statements().iter(), &mut arm, cx) == PresenceFlow::Fell {
            arms.push(arm);
        }
    }
    if !has_default {
        arms.push(pre);
    }
    arms.extend(std::mem::replace(&mut cx.breaks, outer_breaks));
    let Some(joined) = arms.into_iter().reduce(|a, b| join_states(&a, &b)) else {
        return PresenceFlow::Terminated;
    };
    *state = joined;
    PresenceFlow::Fell
}

/// `try`/`catch`/`finally`, conservative in the one direction that matters
/// (ADR-0081 §4).
///
/// The `try` block may throw **at any point**, so a `catch` arm is entered with the
/// pre-`try` state joined with the block's exit state: a name the block binds is
/// `Maybe` there, never `Bound`. The normal-completion path keeps the block's own
/// exit state — weakening *that* too would report
/// `try { $x = f(); } catch (E $e) { $x = 0; } echo $x;`, where every path does
/// bind. `finally` runs on every path, so its bindings apply unconditionally while
/// its reads are judged against the weakened state.
fn presence_try(
    t: &mago_syntax::cst::Try<'_>,
    state: &mut PresenceState,
    cx: &mut PresenceCx,
) -> PresenceFlow {
    let mut certain = state.clone();
    let mut block_flow = PresenceFlow::Fell;
    let mut rest = t.block.statements.iter().peekable();
    // The prologue that cannot throw. `$count = 0;` at the head of a `try` runs
    // before anything can go wrong, so a `catch` arm must not be entered with it
    // undone — the shape that reported `try { $count = 0; foreach (…) {…} } catch
    // (…) {…} if (1 < $count)`, where every path does bind.
    while let Some(s) = rest.peek() {
        if !stmt_cannot_throw(s) {
            break;
        }
        let s = rest.next().expect("peeked");
        block_flow = presence_stmt(s, &mut certain, cx);
        if block_flow != PresenceFlow::Fell {
            break;
        }
    }
    let pre = certain;
    let mut block = pre.clone();
    if block_flow == PresenceFlow::Fell {
        block_flow = presence_seq(rest, &mut block, cx);
    }
    let thrown = join_states(&pre, &block);

    let mut arms: Vec<PresenceState> = Vec::new();
    if block_flow == PresenceFlow::Fell {
        arms.push(block);
    }
    for clause in t.catch_clauses.iter() {
        let mut arm = thrown.clone();
        if let Some(v) = clause.variable.as_ref() {
            arm.insert(strip_dollar(bytes_to_string(v.name)), BindingPresence::Bound);
        }
        if presence_seq(clause.block.statements.iter(), &mut arm, cx) == PresenceFlow::Fell {
            arms.push(arm);
        }
    }
    let mut result = arms
        .into_iter()
        .reduce(|a, b| join_states(&a, &b))
        .unwrap_or_else(|| thrown.clone());

    if let Some(f) = t.finally_clause.as_ref() {
        let mut fin = join_states(&thrown, &result);
        presence_seq(f.block.statements.iter(), &mut fin, cx);
        for (name, presence) in &fin {
            if *presence == BindingPresence::Bound {
                result.insert(name.clone(), BindingPresence::Bound);
            }
        }
    }
    *state = result;
    // A `try` never stops the enclosing scan: `stmt_end` calls it `Unknown`, and
    // `Unknown` on the safe side here means "the successor may run".
    PresenceFlow::Fell
}

/// Walk a loop body to a fixpoint and answer the states that leave it
/// (ADR-0081 §4).
///
/// The body's entry is its own entry joined with everything that reaches the **back
/// edge** — the fall-through end of the body and every `continue`. A prior iteration
/// may have bound a name the first one did not, so a read *earlier* in the body than
/// the binding is `Maybe` rather than `Unbound`. The lattice has height two, so the
/// iteration is bounded at two rounds; the rounds that only compute the fixpoint
/// report nothing, because the state they run against is not yet the one any
/// execution has.
///
/// The two exits are answered apart because they are reached differently: `looped`
/// is the state at the back edge, which becomes the loop's successor only when the
/// loop can exit by its condition, while `broke` is every `break` state, which
/// reaches the successor unconditionally — including out of a `while (true)`.
struct LoopExits {
    /// The state at the back edge: the body's fall-through end joined with every
    /// `continue`.
    looped: PresenceState,
    /// Every `break` state, in order.
    broke: Vec<PresenceState>,
}

fn presence_loop_body(
    body: &[Statement<'_>],
    entry: &PresenceState,
    cx: &mut PresenceCx,
) -> LoopExits {
    // A jump inside this body targets THIS loop; anything parked by an enclosing
    // one must not be credited to it, and vice versa.
    let outer_breaks = std::mem::take(&mut cx.breaks);
    let outer_continues = std::mem::take(&mut cx.continues);

    let mut body_entry = entry.clone();
    let was_silent = cx.silent;
    cx.silent = true;
    for _ in 0..2 {
        let mut s = body_entry.clone();
        let flow = presence_seq(body.iter(), &mut s, cx);
        let mut back = std::mem::take(&mut cx.continues);
        if flow == PresenceFlow::Fell {
            back.push(s);
        }
        cx.breaks.clear();
        let Some(reached) = back.into_iter().reduce(|a, b| join_states(&a, &b)) else {
            break; // nothing reaches the back edge at all.
        };
        let next = join_states(&body_entry, &reached);
        if next == body_entry {
            break;
        }
        body_entry = next;
    }
    cx.silent = was_silent;
    cx.breaks.clear();
    cx.continues.clear();

    let mut fell = body_entry.clone();
    let flow = presence_seq(body.iter(), &mut fell, cx);
    let mut back = std::mem::replace(&mut cx.continues, outer_continues);
    if flow == PresenceFlow::Fell {
        back.push(fell);
    }
    let looped = back.into_iter().reduce(|a, b| join_states(&a, &b)).unwrap_or(body_entry);
    let broke = std::mem::replace(&mut cx.breaks, outer_breaks);
    LoopExits { looped, broke }
}

/// Join a loop's exits into the state after it. `entry` is folded in for the
/// zero-iteration path and `looped` for the condition-became-false path, both only
/// when the loop can exit that way at all; a `break` always reaches the successor.
fn join_loop_exit(
    entry: &PresenceState,
    exits: LoopExits,
    can_exit_by_condition: bool,
) -> PresenceState {
    let LoopExits { looped, broke } = exits;
    let mut states = broke;
    if can_exit_by_condition {
        states.push(looped);
    } else if states.is_empty() {
        // `while (true)` with no `break`: the successor is unreachable, and the
        // back-edge state is the only honest thing to carry into dead code.
        states.push(looped);
    }
    states.into_iter().reduce(|a, b| join_states(&a, &b)).unwrap_or_else(|| entry.clone())
}

/// The reads whose binding only *some* paths carry (issue #267) — the computation
/// behind [`Scope::maybe_undefined_reads`].
///
/// Every premise of the definite id is inherited: this runs only after
/// [`undefined_variable_reads`] has cleared the scope's name dams, only for
/// function-like scopes (top-level and arrow scopes never reach here), and only
/// over the reads `scan_var_usage` collects — so the `isset`/`empty`/`??`/`unset`/
/// `@` guards, the superglobal and `$this` exclusions and the nested-scope boundary
/// are all already settled. `scope_bound` adds the disjointness premise: a name the
/// scope binds nowhere is `variable.undefined`'s, never this id's.
///
/// A `goto` or a label anywhere in the scope dams the pass outright. Every other
/// construct's exit edges are bounded by the traversal above; a jump to an arbitrary
/// label is not, and the honest answer to an unbounded edge is silence.
fn maybe_undefined_reads(
    params: &mago_syntax::cst::FunctionLikeParameterList<'_>,
    use_clause: Option<&mago_syntax::cst::ClosureUseClause<'_>>,
    statements: &[&Statement<'_>],
    scope_bound: &HashSet<String>,
) -> Vec<UndefinedRead> {
    if statements.iter().any(|s| subtree_has_goto(&Node::Statement(s))) {
        return Vec::new();
    }
    let mut state = PresenceState::new();
    for p in params.parameters.iter() {
        state.insert(strip_dollar(bytes_to_string(p.variable.name)), BindingPresence::Bound);
    }
    if let Some(uc) = use_clause {
        for v in uc.variables.iter() {
            state.insert(strip_dollar(bytes_to_string(v.variable.name)), BindingPresence::Bound);
        }
    }
    let mut cx = PresenceCx {
        scope_bound,
        out: Vec::new(),
        silent: false,
        seen: HashSet::new(),
        breaks: Vec::new(),
        continues: Vec::new(),
    };
    for s in statements {
        if presence_stmt(s, &mut state, &mut cx) != PresenceFlow::Fell {
            break;
        }
    }
    let mut out = cx.out;
    out.sort_by_key(|r| r.span.start);
    out
}

/// Whether a statement **provably cannot throw**, over a whitelist narrow enough
/// that no PHP semantics argument is needed to read it.
///
/// Almost every PHP construct can raise something: a call, a property fetch, a
/// division, a concatenation with an object, an undefined constant. So this answers
/// `true` only for a plain `=` assignment from a literal, an array of literals or
/// another local — the prologue idiom (`$count = 0;`, `$out = [];`, `$x = $y;`) and
/// nothing beyond it. Answering `false` costs precision and never correctness: it
/// puts the statement back on the "may have thrown before this" side, which is the
/// conservative reading [`presence_try`] applies to the whole block anyway.
fn stmt_cannot_throw(s: &Statement<'_>) -> bool {
    match s {
        Statement::Noop(_) => true,
        Statement::Expression(es) => match es.expression.unparenthesized() {
            Expression::Assignment(a) => {
                a.operator.is_assign()
                    && matches!(a.lhs.unparenthesized(), Expression::Variable(Variable::Direct(_)))
                    && expr_cannot_throw(a.rhs)
            }
            _ => false,
        },
        _ => false,
    }
}

/// The value half of [`stmt_cannot_throw`]: a literal, an array literal of such
/// values, another local, or a sign/negation over one.
fn expr_cannot_throw(expr: &Expression<'_>) -> bool {
    match expr.unparenthesized() {
        Expression::Literal(_) => true,
        Expression::Variable(Variable::Direct(_)) => true,
        Expression::Array(a) => a.elements.iter().all(element_cannot_throw),
        Expression::LegacyArray(a) => a.elements.iter().all(element_cannot_throw),
        Expression::UnaryPrefix(up) => {
            matches!(
                up.operator,
                UnaryPrefixOperator::Not(_)
                    | UnaryPrefixOperator::Negation(_)
                    | UnaryPrefixOperator::Plus(_)
            ) && expr_cannot_throw(up.operand)
        }
        _ => false,
    }
}

fn element_cannot_throw(element: &ArrayElement<'_>) -> bool {
    match element {
        ArrayElement::KeyValue(kv) => expr_cannot_throw(kv.key) && expr_cannot_throw(kv.value),
        ArrayElement::Value(v) => expr_cannot_throw(v.value),
        ArrayElement::Missing(_) => true,
        ArrayElement::Variadic(_) => false,
    }
}

/// Whether a `goto` or a label stands anywhere in this subtree, without descending
/// into a nested scope.
fn subtree_has_goto(node: &Node<'_, '_>) -> bool {
    match node {
        Node::Goto(_) | Node::Label(_) => true,
        Node::Function(_)
        | Node::Method(_)
        | Node::Closure(_)
        | Node::ArrowFunction(_)
        | Node::AnonymousClass(_)
        | Node::Class(_)
        | Node::Interface(_)
        | Node::Trait(_)
        | Node::Enum(_) => false,
        _ => node.children().iter().any(subtree_has_goto),
    }
}

// end binding presence (ADR-0081, issue #267)

/// Build the [`Scope`] for an arrow function `fn(...) => expr` (ADR-0033). The
/// single body expression lowers to one `return <expr>;` statement so a call
/// inside it (`fn($x) => width($x)`) is a reachable propagation/descent edge.
fn build_closure_scope_from_arrow(
    af: &mago_syntax::cst::ArrowFunction<'_>,
    rc: &RefResolver,
    docs: &DocIndex<'_>,
    stmt_doc: Option<&StmtAdoption>,
) -> Scope {
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
    let invalidated = call_invalidation(&Node::Expression(af.expression));
    let call = named_call(af.expression);
    let span = to_span(af.expression.span());
    // An arrow body is a `return` position with a real env, so its string contexts
    // (ADR-0078, issue #193) are collected here — `lower_stmt`, which does that
    // centrally for every other statement, is bypassed by this one-statement trace.
    let mut string_contexts = Vec::new();
    scan_string_contexts(&Node::Expression(af.expression), &mut string_contexts);
    let ret = Stmt {
        span,
        kind: StmtKind::Return { value, call, span },
        invalidated,
        string_contexts,
        // An arrow body IS a `return`, so the scope's trace always terminates —
        // which is precisely why `fn () => …` can never be a `type.return-missing`
        // site, no matter what it declares (ADR-0078, issue #199).
        end: BodyEnd::Terminates,
        has_terminator: true,
    };
    let mut opaque = Vec::new();
    scan_opaque(&Node::Expression(af.expression), &mut opaque, false);
    let poisoned = !opaque.is_empty();
    let is_generator = node_is_generator(&Node::Expression(af.expression));
    let def_offset = arrow_def_offset(af);
    Scope {
        function_name: None,
        owner: ScopeOwner::Closure { def_offset },
        ret_hint: ret_hint_of(af.return_type_hint.as_ref()),
        is_generator,
        poisoned,
        opaque,
        stmts: vec![ret],
        method_calls,
        params: lower_params(&af.parameter_list, rc),
        ret_ty: af.return_type_hint.as_ref().and_then(|r| lower_hint(&r.hint, rc)),
        effect_origins,
        throw_origins,
        is_static: af.r#static.is_some(),
        docblock: adopt_closure_docblock(docs, to_span(af.span()).start, def_offset, stmt_doc),
        // An arrow function's captures are *derived* from its body's free
        // variables, so an unused one is not expressible.
        unused_captures: Vec::new(),
        // …and by the same derivation an arrow body cannot read an unbound name of
        // its OWN: every free variable it mentions is auto-captured from the
        // enclosing scope, whose question this is not.
        undefined_reads: Vec::new(),
        maybe_undefined_reads: Vec::new(),
        ref_arg_candidates: Vec::new(),
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
    for child in children(node) {
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
                invalidated = call_invalidation(&Node::Expression(e));
                // `return f($s);` — carry the call so propagation/descent reach it.
                call = named_call(e);
            }
            Stmt::lowered(StmtKind::Return { value, call, span }, invalidated)
        }
        // `echo e1, e2, …;` — collect the statically-named calls among the
        // operands so propagation/descent check them; env stays conservative.
        Statement::Echo(e) => {
            let mut calls = Vec::new();
            // The ADR-0070 evidence is accumulated over the WHOLE echo: every
            // operand feeds the same per-name entries, so `echo trim($x), $o->m($x);`
            // turns `$x` opaque and discards its `trim` site too — the verdict is
            // statement-scoped, never per operand.
            let mut invalidated = Vec::new();
            for v in e.values.iter() {
                scan_invalidated(&Node::Expression(v), &mut invalidated, false);
                // Echo invalidates variables written by embedded assignments
                // (`echo $x = 5;`) or mutable calls (ADR-0031) — and a name this
                // echo WRITES is not a by-value-argument question at all: the
                // write is the reason it is invalidated, no signature can excuse
                // it, so its entry is opaque.
                let mut writes = Vec::new();
                collect_assign_writes(&Node::Expression(v), &mut writes);
                for name in writes {
                    note_occurrence(&mut invalidated, name, None);
                }
                if let Some(c) = named_call(v) {
                    calls.push(c);
                }
            }
            Stmt::lowered(StmtKind::Echo(calls), invalidated)
        }
        // `if`/`elseif`/`else` is structured (ADR-0031): its control flow
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
            Stmt::lowered(StmtKind::OffsetUnset { base, key }, Vec::new())
        }
        // Everything else (declarations, `goto`, labels, `declare`, other unsets,
        // `__halt_compiler`, …) stays a full Barrier: the sound floor for
        // anything whose write set the lowering cannot bound.
        _ => Stmt::lowered(StmtKind::Barrier, Vec::new()),
    };
    out.push(Stmt {
        span: stmt_span,
        string_contexts: string_context_sites(s),
        // The reachability foundation's central fill (ADR-0078, issue #199) — read
        // off the CST statement, never off the lowered `kind`. See `stmt_end`.
        end: stmt_end(s),
        has_terminator: subtree_has_function_exit(&Node::Statement(s)),
        ..stmt
    });
}

// reachability foundation (ADR-0078, issue #199)

/// Where one CST statement leaves control — the per-statement half of the
/// terminality judgment ([`BodyEnd`]), computed here because this is the last
/// place the full construct is in hand.
///
/// # The per-construct table, and why each row answers what it does
///
/// | construct | answer | why |
/// | --- | --- | --- |
/// | `return`, `throw` (expression-statement), `exit`/`die`, `__halt_compiler` | `Terminates` | no edge to the successor at all |
/// | `break`, `continue` | `Terminates` | control leaves *this* statement list; where it lands is the enclosing construct's business, not this list's |
/// | `if` | join of every arm, with a missing `else` joined in as `FallsThrough`; a literal-true condition ends the chain at its own arm and a literal-false one drops it | the implicit empty else IS a terminator-free path to the successor — unless the condition is a literal, where there is no branch at all |
/// | `match` (statement position) | join of every arm, with a missing `default` joined in as `Terminates` | PHP throws `\UnhandledMatchError` on no match — witnessed 8.5.9 |
/// | `switch` | join of every case body, with a missing `default` joined in as `FallsThrough`; `Unknown` when the subtree holds any `break`/`continue`/`goto`, or when a case body runs into the next | a `break` exits the *switch* rather than the list it sits in, and resolving which is which is not this judgment's job; case-to-case fall-through is a real edge it does not model |
/// | `foreach` | `FallsThrough` | the iteration exhausts; see the recorded obstacle below |
/// | `while` / `for` / `do-while` with a provably-true condition and no `break`/`goto` in the subtree | `Terminates` | there is no exit edge to take |
/// | the same with a `break`/`goto` somewhere inside | `Unknown` | the jump's target is not resolved here, so whether *this* loop has an exit edge is undecided |
/// | every other loop | `FallsThrough` | the condition can be false, which is an exit edge |
/// | `try` | `Unknown` | recorded exclusion — see below |
/// | `goto`, a `label:` | `Unknown` | an unbounded jump; a label is an unbounded *incoming* edge, so the tail may be re-entered |
/// | everything else (assignments, calls, `echo`, `global`, `static`, `unset`, declarations, `use`, `declare`, `namespace`) | `FallsThrough` | straight-line |
///
/// # Recorded obstacles — silences this judgment names rather than hides
///
/// * **`try`/`catch`/`finally` is `Unknown`, full stop (issue #199's "handled or
///   explicitly excluded with a reason").** The reason is `finally`, which
///   *overwrites the exit point*: witnessed on 8.5.9,
///   `try { return 1; } finally { return 2; }` returns `2`, and a `finally` that
///   returns also swallows an in-flight exception from the `try`. So a `try`
///   whose block and every `catch` terminate can still fall through, and a `try`
///   that plainly falls through can still terminate. Judging either direction
///   from the block ends alone would be wrong in both, so the whole construct is
///   undecided until a later slice models `finally` properly.
/// * **A call to a function proven never to return is not judged here.** A
///   statement-position call answers `FallsThrough`, because deciding otherwise
///   needs the project index (which callee? does it declare `: never`?) and this
///   judgment is deliberately index-free and env-free. `type.return-missing`
///   applies that refinement itself, at the emitter, where the index lives.
/// * **An infinite `Traversable`.** `foreach ($generator as $v)` over a generator
///   that never ends has no exit edge, and this judgment says `FallsThrough`
///   anyway. Bounding it would need the iterator's value, which is exactly the
///   whole-program reasoning the syntactic CFG reading rules out.
fn stmt_end(s: &Statement<'_>) -> BodyEnd {
    match s {
        Statement::Return(_) => BodyEnd::Terminates,
        // `break` / `continue` leave the enclosing statement list. A `switch`
        // case's trailing `break` is stripped before its body is lowered
        // (`strip_trailing_break`), so this row never mis-reads an arm as
        // terminating when it only ends the arm.
        Statement::Break(_) | Statement::Continue(_) => BodyEnd::Terminates,
        Statement::HaltCompiler(_) => BodyEnd::Terminates,
        Statement::Expression(es) => expr_end(es.expression),
        Statement::Block(b) => block_end(b.statements.as_slice()),
        Statement::If(i) => if_end(i),
        Statement::Switch(sw) => switch_end(sw),
        Statement::Foreach(_) => BodyEnd::FallsThrough,
        Statement::While(w) => loop_end(expr_is_true(w.condition), &Node::Statement(s)),
        Statement::DoWhile(d) => loop_end(expr_is_true(d.condition), &Node::Statement(s)),
        // `for (;;)` — no condition at all — is the canonical infinite `for`; a
        // written condition list is infinite when its LAST expression (the one PHP
        // actually tests) is a true literal.
        Statement::For(f) => {
            let infinite = f.conditions.iter().next_back().is_none_or(|c| expr_is_true(c));
            loop_end(infinite, &Node::Statement(s))
        }
        Statement::Try(_) => BodyEnd::Unknown,
        Statement::Goto(_) | Statement::Label(_) => BodyEnd::Unknown,
        _ => BodyEnd::FallsThrough,
    }
}

/// [`body_end`] over a borrowed CST statement list — the same fold, one level
/// earlier. Kept separate from [`body_end`] (which reads lowered [`Stmt`]s) because
/// a branch body is judged here *before* it is lowered, and the two must agree by
/// sharing this shape rather than by coincidence.
fn block_end(statements: &[Statement<'_>]) -> BodyEnd {
    let mut undecided = false;
    for s in statements {
        match stmt_end(s) {
            BodyEnd::Terminates => return BodyEnd::Terminates,
            BodyEnd::Unknown => undecided = true,
            BodyEnd::FallsThrough => {}
        }
    }
    if undecided { BodyEnd::Unknown } else { BodyEnd::FallsThrough }
}

/// An `if`'s terminality: the join over its arms, with the **implicit empty
/// `else`** joined in as [`BodyEnd::FallsThrough`] when no `else` is written.
///
/// That implicit arm is the whole reason `if ($c) { return 1; }` is reported by
/// `type.return-missing` and `if ($c) { return 1; } else { return 2; }` is not.
///
/// # The one place a condition is read
///
/// Branch conditions are otherwise non-deterministic here (see [`stmt_end`]), but
/// a **literal** one is not a branch at all: `if (true) { return 1; }` has no
/// no-branch path to add, and reading it as one would accuse a function that
/// demonstrably returns. So a provably-true condition ends the chain at its own
/// arm (no later `elseif`, no `else`, no implicit arm), and a provably-false one
/// contributes no arm.
///
/// **Recorded obstacle:** only *literals* are read. A constant-folded guard —
/// `if (PHP_VERSION_ID >= 80000) { return 1; }`, `if (self::ENABLED) { … }` —
/// still contributes the implicit fall-through arm, because folding it needs the
/// project index and the version view this judgment deliberately does without.
/// A guard of that shape with no `else` is `type.return-missing`'s second named
/// over-report risk, alongside the undeclared never-returning callee.
fn if_end(i: &mago_syntax::cst::If<'_>) -> BodyEnd {
    let body = &i.body;
    let mut chain: Vec<(&Expression<'_>, &[Statement<'_>])> =
        vec![(i.condition, body.statements())];
    chain.extend(body.else_if_clauses());

    let mut arms = Vec::new();
    for (cond, stmts) in chain {
        if expr_is_false(cond) {
            // The arm can never be taken; it contributes no path.
            continue;
        }
        arms.push(block_end(stmts));
        if expr_is_true(cond) {
            // Always taken: no later arm and no implicit no-branch path exist.
            return BodyEnd::join_arms(arms);
        }
    }
    match body.else_statements() {
        Some(stmts) => arms.push(block_end(stmts)),
        // No `else`: the no-branch-taken path runs straight to the successor.
        None => arms.push(BodyEnd::FallsThrough),
    }
    BodyEnd::join_arms(arms)
}

/// A `switch`'s terminality: the join over its case bodies, with the **implicit
/// no-match arm** joined in as [`BodyEnd::FallsThrough`] when there is no
/// `default`.
///
/// Two shapes make the whole construct [`BodyEnd::Unknown`], and both are the
/// honest answer rather than a shortcut:
///
/// * **any `break` / `continue` / `goto` in the subtree.** A `break` in a case
///   body exits the *switch* and lands on its successor — the exact opposite of
///   what [`stmt_end`] says about a `break` in isolation, where it terminates the
///   list it sits in. Telling the two apart means resolving the jump's target
///   through nested `if`s, loops and inner switches, which this judgment does not
///   do. A `switch` whose every case `break`s would otherwise be read as
///   *terminating*, and a dead-code consumer would then call everything after it
///   unreachable — the single worst mistake available here.
/// * **a non-empty case body that runs off its end** into the *next* case. PHP's
///   case-to-case fall-through is a real control-flow edge and is not modelled;
///   an empty case label (`case 1: case 2: body`) is that shape used
///   deliberately, and contributes no arm of its own.
fn switch_end(sw: &mago_syntax::cst::Switch<'_>) -> BodyEnd {
    if subtree_has_switch_jump(&Node::Switch(sw)) {
        return BodyEnd::Unknown;
    }
    let mut arms = Vec::new();
    let mut has_default = false;
    for case in sw.body.cases() {
        if case.expression().is_none() {
            has_default = true;
        }
        if case.is_empty() {
            continue;
        }
        let end = block_end(case.statements());
        if !end.provably_terminates() {
            // The body runs off its end — into the next case, not past the switch.
            return BodyEnd::Unknown;
        }
        arms.push(end);
    }
    if !has_default {
        arms.push(BodyEnd::FallsThrough);
    }
    BodyEnd::join_arms(arms)
}

/// Whether `node`'s subtree contains a jump whose target could be this `switch`:
/// a `break`, a `continue` (which PHP accepts inside a `switch`, where it acts on
/// the enclosing loop) or a `goto`. Nested function-likes are not descended.
fn subtree_has_switch_jump(node: &Node<'_, '_>) -> bool {
    match node {
        Node::Break(_) | Node::Continue(_) | Node::Goto(_) => true,
        Node::Function(_) | Node::Method(_) | Node::Closure(_) | Node::ArrowFunction(_) => false,
        _ => children(node).iter().any(subtree_has_switch_jump),
    }
}

/// A loop's terminality from the two facts that decide it: whether its condition
/// is a proven-true literal, and whether its subtree contains a jump that could
/// leave it.
///
/// * not provably infinite → [`BodyEnd::FallsThrough`]: the false-condition exit
///   edge exists (a `while ($x)` whose `$x` happens to always be true is a hang,
///   not a fall-through, but proving that is path feasibility — outside this
///   judgment by design, and the same reading `if ($c) { return 1; }` gets);
/// * infinite with no `break`/`goto` anywhere inside → [`BodyEnd::Terminates`]:
///   there is no exit edge at all;
/// * infinite *with* one → [`BodyEnd::Unknown`]: a `break` may belong to a nested
///   `switch` or loop rather than to this one, and resolving jump targets is not
///   this judgment's job.
///
/// `continue` is deliberately not a jump here: it re-enters the loop, it never
/// leaves it — not even `continue 2` from a nested loop, which targets *this*
/// loop's next iteration.
fn loop_end(infinite: bool, node: &Node<'_, '_>) -> BodyEnd {
    if !infinite {
        return BodyEnd::FallsThrough;
    }
    if subtree_has_exit_jump(node) { BodyEnd::Unknown } else { BodyEnd::Terminates }
}

/// Whether `node`'s subtree contains a **function exit** — a `return`, a `throw`
/// or an `exit`/`die` — at any depth (ADR-0078 §5). Nested function-likes are their
/// own scopes and are not descended: a `return` inside a closure exits the closure,
/// not the body that defines it.
///
/// Deliberately **not** counting `break`/`continue`: those leave a construct, never
/// the function, and a `switch` full of `break`s is no evidence at all that the
/// author meant to return something.
fn subtree_has_function_exit(node: &Node<'_, '_>) -> bool {
    match node {
        Node::Return(_)
        | Node::Throw(_)
        | Node::ExitConstruct(_)
        | Node::DieConstruct(_)
        | Node::HaltCompiler(_) => true,
        Node::Function(_) | Node::Method(_) | Node::Closure(_) | Node::ArrowFunction(_) => false,
        _ => children(node).iter().any(subtree_has_function_exit),
    }
}

/// Whether `node`'s subtree contains a `break` or a `goto` — a jump that could
/// leave an enclosing loop. Nested function-likes are their own scopes and are not
/// descended (their jumps cannot leave this loop).
fn subtree_has_exit_jump(node: &Node<'_, '_>) -> bool {
    match node {
        Node::Break(_) | Node::Goto(_) => true,
        Node::Function(_) | Node::Method(_) | Node::Closure(_) | Node::ArrowFunction(_) => false,
        _ => children(node).iter().any(subtree_has_exit_jump),
    }
}

/// Whether an expression is a **proven-true literal** — the only conditions this
/// judgment reads as always-taken. `while (true)`, `while (1)` and `for (;;)` are
/// the idioms; anything else (a variable, a call, a comparison) is left to the
/// non-deterministic reading, which is the safe side for a *loop* condition
/// because it produces [`BodyEnd::FallsThrough`], never a claim of termination.
fn expr_is_true(expr: &Expression<'_>) -> bool {
    match lower_arg_value(expr.unparenthesized()) {
        ArgValue::Bool(b) => b,
        ArgValue::Int(i) => i != 0,
        _ => false,
    }
}

/// Whether an expression is a **proven-false literal** — the mirror of
/// [`expr_is_true`], read only for an `if`/`elseif` condition (see [`if_end`]).
/// `false`, `0` and `null` are the spellings that appear; anything non-literal is
/// left to the non-deterministic reading.
fn expr_is_false(expr: &Expression<'_>) -> bool {
    match lower_arg_value(expr.unparenthesized()) {
        ArgValue::Bool(b) => !b,
        ArgValue::Int(i) => i == 0,
        ArgValue::Null => true,
        _ => false,
    }
}

/// Where an expression in **statement position** leaves control. The expression
/// forms that terminate are exactly the three the trace IR already models as
/// terminators — `throw`, `exit`, `die` — plus a statement-position `match`,
/// whose arms are themselves expressions.
///
/// A plain call answers [`BodyEnd::FallsThrough`]; see `stmt_end`'s recorded
/// obstacle on never-returning callees for why, and where that refinement lives.
fn expr_end(expr: &Expression<'_>) -> BodyEnd {
    match expr.unparenthesized() {
        Expression::Throw(_) => BodyEnd::Terminates,
        Expression::Construct(Construct::Exit(_) | Construct::Die(_)) => BodyEnd::Terminates,
        Expression::Match(m) => match_end(m),
        _ => BodyEnd::FallsThrough,
    }
}

/// A `match`'s terminality: the join over its arm bodies, with the **implicit
/// no-match arm** joined in as [`BodyEnd::Terminates`] when there is no
/// `default` — PHP throws `\UnhandledMatchError` there (witnessed 8.5.9), and a
/// throw is a terminator.
///
/// This is the one place where a missing default makes a construct *more*
/// terminal rather than less, and it is the exact opposite of `switch`'s rule
/// above. The two are different constructs with different semantics; sharing one
/// rule between them would be wrong for one of them.
fn match_end(m: &mago_syntax::cst::Match<'_>) -> BodyEnd {
    let mut arms = Vec::new();
    let mut has_default = false;
    for arm in m.arms.iter() {
        match arm {
            mago_syntax::cst::MatchArm::Expression(a) => arms.push(expr_end(a.expression)),
            mago_syntax::cst::MatchArm::Default(a) => {
                has_default = true;
                arms.push(expr_end(a.expression));
            }
        }
    }
    if !has_default {
        arms.push(BodyEnd::Terminates);
    }
    BodyEnd::join_arms(arms)
}

// end reachability foundation (ADR-0078, issue #199)

/// Every [`StringContextSite`] a statement's **own** expressions carry (ADR-0078,
/// issue #193).
///
/// # The position boundary, and why it is here
///
/// Four statement kinds are read: an expression statement, a `return`, and the two
/// `echo` forms. Together they carry the constructs the ids exist for —
/// `$s = "x $v";`, `f((string) $v)`, `return 'a' . $v;`, `echo $v;`, `print $v;`,
/// `<?= $v ?>` — and each is a position where the walk's ENTRY env is exactly the
/// env PHP evaluates the expression in.
///
/// Everything else is recorded silence, for one reason: a branch condition, a loop
/// header, a `match` subject and a `switch` case are evaluated in an env the
/// statement-position pass does not hold (an `elseif` condition runs only after the
/// previous branch was refuted; a loop header runs once per iteration), and the
/// bodies of the unstructured constructs are not lowered as statements at all
/// (`lower_opaque` keeps only their write/read sets). This is the same position
/// boundary every other value-reading check carries — the preg pattern check's
/// "statement pass plus the `if` guard" — minus the guard, because `if ((string) $v)`
/// is not an idiom the way `if (preg_match(…))` is.
///
/// Nested statements are never descended: an `if` branch's body is lowered by
/// [`lower_stmt`] itself and collects its own sites, so nothing is counted twice.
fn string_context_sites(s: &Statement<'_>) -> Vec<StringContextSite> {
    let mut out = Vec::new();
    match s {
        Statement::Expression(es) => {
            scan_string_contexts(&Node::Expression(es.expression), &mut out);
        }
        Statement::Return(r) => {
            if let Some(e) = r.value {
                scan_string_contexts(&Node::Expression(e), &mut out);
            }
        }
        // Each `echo` operand is itself a conversion. An operand that is a composite
        // string or a cast lowers to `Other` here (proving nothing) and is collected
        // again, precisely, by the scan — so a value is reported once, at the
        // innermost construct that names it.
        Statement::Echo(e) => {
            for v in e.values.iter() {
                out.push(echo_site(v));
                scan_string_contexts(&Node::Expression(v), &mut out);
            }
        }
        Statement::EchoTag(e) => {
            for v in e.values.iter() {
                out.push(echo_site(v));
                scan_string_contexts(&Node::Expression(v), &mut out);
            }
        }
        _ => {}
    }
    out
}

/// One `echo` / `<?= ?>` operand as a site.
fn echo_site(v: &Expression<'_>) -> StringContextSite {
    StringContextSite {
        value: lower_arg_value(v),
        span: to_span(v.span()),
        kind: StringContextKind::Echo,
    }
}

/// Collect the string conversions inside one expression subtree.
///
/// Function-like bodies are not descended — a closure, an arrow function and a
/// nested declaration are their own scopes, lowered (and judged) separately, and
/// their free variables are not this statement's env.
fn scan_string_contexts(node: &Node<'_, '_>, out: &mut Vec<StringContextSite>) {
    let mut site = |e: &Expression<'_>, kind| {
        out.push(StringContextSite { value: lower_arg_value(e), span: to_span(e.span()), kind });
    };
    match node {
        Node::Function(_) | Node::Method(_) | Node::Closure(_) | Node::ArrowFunction(_) => return,
        // `"a $v"`, `"{$v}"`, a heredoc body, and a backtick string: every embedded
        // expression is converted. A nowdoc and a single-quoted string carry only
        // literal parts and so contribute nothing.
        Node::CompositeString(cs) => {
            for part in cs.parts().iter() {
                match part {
                    StringPart::Literal(_) => {}
                    StringPart::Expression(e) => site(e, StringContextKind::Interpolation),
                    StringPart::BracedExpression(b) => {
                        site(b.expression, StringContextKind::Interpolation);
                    }
                }
            }
        }
        // `(string) $v`. Every other cast converts to something else entirely and is
        // not these ids' business.
        Node::UnaryPrefix(u) if matches!(u.operator, UnaryPrefixOperator::StringCast(..)) => {
            site(u.operand, StringContextKind::Cast);
        }
        // `$a . $b` — BOTH operands convert, and PHP warns once per array operand, so
        // both are sites. A left-nested chain `'a' . $x . $y` visits each inner
        // `Binary` in turn, so every leaf is collected exactly once (the nested
        // `Binary` operand itself lowers to `ArgValue::Concat`, which proves no value
        // unless both its own operands do — never a second report).
        Node::Binary(b) if b.operator.is_concatenation() => {
            site(b.lhs, StringContextKind::Concat);
            site(b.rhs, StringContextKind::Concat);
        }
        // `$a .= $b` reads `$a` in string context too — `$arr .= 'x'` warns on the
        // left-hand side exactly as `$arr . 'x'` does.
        Node::Assignment(a) if matches!(a.operator, AssignmentOperator::Concat(_)) => {
            site(a.lhs, StringContextKind::Concat);
            site(a.rhs, StringContextKind::Concat);
        }
        Node::PrintConstruct(p) => site(p.value, StringContextKind::Print),
        _ => {}
    }
    for child in children(node) {
        scan_string_contexts(&child, out);
    }
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

/// Lower a structured `if`/`elseif`/`else` statement (ADR-0031) to
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
    Stmt::lowered(StmtKind::If { cond, then_trace, elseifs, else_trace }, Vec::new())
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
    // This path bypasses `lower_stmt`, so it owns its own terminality fill
    // (ADR-0078, issue #199) — from the arm's expression, the same `expr_end` a
    // statement-position expression gets.
    vec![Stmt {
        span: to_span(expr.span()),
        end: expr_end(expr),
        has_terminator: subtree_has_function_exit(&Node::Expression(expr)),
        ..st
    }]
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
    Some(Stmt::lowered(StmtKind::Match { subject, arms, default, loose: false }, Vec::new()))
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
    Some(Stmt::lowered(StmtKind::Match { subject, arms, default, loose: true }, Vec::new()))
}

/// Lower an operand to a *usable* [`CondOperand`] — a bare variable or a literal —
/// or `None` for anything else (a call, property fetch, arithmetic). Used to gate
/// whether a `match`/`switch` can be structured at all.
fn usable_operand(expr: &Expression<'_>) -> Option<CondOperand> {
    match lower_cond_operand(expr) {
        CondOperand::Other { .. } => None,
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
    children(node).iter().any(|child| match child {
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

/// Lower a condition expression to a [`CondExpr`] (ADR-0031). Recognized:
/// `===`/`!==`/`==`/`!=` comparisons, `instanceof`, `!`/`&&`/`||` (incl. the
/// low-precedence `and`/`or`), and bare truthiness. Everything else becomes
/// [`CondExpr::Opaque`] carrying the variables it reads.
fn lower_cond(expr: &Expression<'_>) -> CondExpr {
    // A long `&&` / `||` chain recurses once per conjunct (issue #264). Out of
    // headroom the condition is opaque, and its read set is what the (equally
    // guarded) scan can still reach — which is why the refusal also travels to
    // the caller as a parse error: a partly-walked condition is exactly the case
    // ADR-0079's dam exists for, and the file's other findings are dropped with
    // it rather than drawn from a tree the walk did not finish.
    if stack_guard::exhausted() {
        return CondExpr::Opaque { reads: Vec::new() };
    }
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
        // lowers to `Opaque`.
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
            // The whole-condition position keeps the conservative floor: `reads`
            // here is every variable the condition mentions, not the narrower
            // `CondOperand::Other::invalidates` set. Widening this one to match
            // would be a precision change (`if ($o->p)` would stop forgetting
            // `$o`) with its own measurement, and it is not what issue #158 is.
            CondOperand::Other { .. } => {
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

/// The [`CmpOp`] a parsed binary operator denotes, or `None` when the operator is
/// not a comparison. The ONE place the syntax-to-`CmpOp` map lives: guard position
/// ([`lower_binary_cond`]) and value position (`lower_arg_value`, issue #260) read
/// the same map, so the two positions can never drift apart on which operators
/// count as comparisons.
fn cmp_op_of(operator: &BinaryOperator<'_>) -> Option<CmpOp> {
    match operator {
        BinaryOperator::Identical(_) => Some(CmpOp::Identical),
        BinaryOperator::NotIdentical(_) => Some(CmpOp::NotIdentical),
        BinaryOperator::Equal(_) => Some(CmpOp::Loose),
        BinaryOperator::NotEqual(_) | BinaryOperator::AngledNotEqual(_) => Some(CmpOp::NotLoose),
        BinaryOperator::LessThan(_) => Some(CmpOp::Lt),
        BinaryOperator::LessThanOrEqual(_) => Some(CmpOp::Le),
        BinaryOperator::GreaterThan(_) => Some(CmpOp::Gt),
        BinaryOperator::GreaterThanOrEqual(_) => Some(CmpOp::Ge),
        _ => None,
    }
}

/// Is this operand a `count(…)` / `sizeof(…)` call **as written** (issue #272)?
///
/// A syntactic question, deliberately: the lowering decides only whether the
/// comparison is worth carrying as a [`CondExpr::Cmp`], and the semantic
/// question — does the name denote the global builtin here — belongs to the
/// consumer, which has the project view this crate does not.
fn names_count_call(operand: &CondOperand) -> bool {
    let CondOperand::Other { call: Some(call), .. } = operand else { return false };
    call.callee.as_deref().is_some_and(|c| {
        let bare = c.rsplit('\\').next().unwrap_or(c);
        ["count", "sizeof"].iter().any(|n| bare.eq_ignore_ascii_case(n))
    })
}

/// Lower a binary-operator condition (comparison / `instanceof` / `&&` / `||`).
fn lower_binary_cond(b: &Binary<'_>) -> CondExpr {
    let op = cmp_op_of(&b.operator);
    if let Some(op) = op {
        let lhs = lower_cond_operand(b.lhs);
        let rhs = lower_cond_operand(b.rhs);
        // Ordering comparisons (`<`/`<=`/`>`/`>=`) are only useful for guard
        // refinement when one side is a bare variable and the other a literal,
        // so an unrepresentable operand falls back to `Opaque` (collecting
        // reads). Since issue #158 a `CondOperand::Other` no longer drops what
        // it may write, so this arm is now about *refinement value*, not
        // soundness — lifting it would let `preg_match($re, $s, $m) > 0` reach
        // the out-parameter seed, which is a precision change of its own.
        //
        // **The count exception** (issue #272): an ordering comparison whose
        // opaque side is a `count()`/`sizeof()` call keeps its `Cmp` form, so
        // the shape-narrowing dispatcher can read it. The name is matched
        // syntactically here because this crate has no project view; whether it
        // denotes the *global builtin* is settled on the consuming side
        // (`count_subject`), which is also where the mode argument and a
        // shadowing project function decline.
        let ordering = matches!(op, CmpOp::Lt | CmpOp::Le | CmpOp::Gt | CmpOp::Ge);
        let opaque_side = |o: &CondOperand| {
            matches!(o, CondOperand::Other { .. }) && !names_count_call(o)
        };
        if ordering && (opaque_side(&lhs) || opaque_side(&rhs)) {
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
            _ => {
                // The invalidation set is collected only when the operand can
                // write at all — `$o->p === 1` reads `$o` and rebinds nothing,
                // and forgetting there would be a precision loss with no
                // soundness content (issue #158).
                let node = Node::Expression(other);
                let writers = operand_writers(&node);
                CondOperand::Other {
                    call: named_call(other).map(Box::new),
                    invalidates: match writers {
                        OperandWriters::None => Vec::new(),
                        _ => cond_reads(other),
                    },
                    sites: match writers {
                        OperandWriters::Calls => call_invalidation(&node),
                        _ => Vec::new(),
                    },
                }
            }
        },
    }
}

/// What, if anything, in an operand subtree can **rebind a variable of the
/// enclosing scope** (issue #158) — the question behind both
/// [`CondOperand::Other`] fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OperandWriters {
    /// Nothing can: the operand reads and returns. A property or offset read,
    /// arithmetic, concatenation, a cast, `isset`/`empty`/`print` are all this.
    None,
    /// Only calls and `new` — each may declare a parameter `&$x` and write
    /// through the caller's binding (`preg_match($re, $s, $m)`), and each is
    /// describable by the ADR-0070 by-value evidence.
    Calls,
    /// A writer the by-value evidence does not describe: an assignment in any
    /// form (`($x = f()) === 1`), an increment/decrement prefix or postfix
    /// (`$i++ === 5` — the branch sees the incremented `$i`, never the tested
    /// one), or `eval`/`include`/`require`, which run statements in this very
    /// frame.
    Any,
}

/// Classify an operand subtree's writers. A nested function-like is a separate
/// scope whose body does not run here, exactly as [`collect_read_vars`] treats
/// it (and a closure that *is* invoked is a `Node::Call` at the invocation).
fn operand_writers(node: &Node<'_, '_>) -> OperandWriters {
    match node {
        Node::Assignment(_)
        | Node::UnaryPostfix(_)
        | Node::EvalConstruct(_)
        | Node::IncludeConstruct(_)
        | Node::IncludeOnceConstruct(_)
        | Node::RequireConstruct(_)
        | Node::RequireOnceConstruct(_) => return OperandWriters::Any,
        Node::UnaryPrefix(u) if u.operator.is_increment_or_decrement() => {
            return OperandWriters::Any;
        }
        // Nested scopes are their own concern — their bodies do not run here.
        Node::Function(_)
        | Node::Closure(_)
        | Node::ArrowFunction(_)
        | Node::AnonymousClass(_)
        | Node::Class(_)
        | Node::Interface(_)
        | Node::Trait(_)
        | Node::Enum(_) => return OperandWriters::None,
        _ => {}
    }
    // A call still has to be descended: `f($x = 1)` is both.
    let seen_call = matches!(node, Node::Call(_) | Node::Instantiation(_));
    let mut worst = if seen_call { OperandWriters::Calls } else { OperandWriters::None };
    for child in children(node) {
        match operand_writers(&child) {
            OperandWriters::Any => return OperandWriters::Any,
            OperandWriters::Calls => worst = OperandWriters::Calls,
            OperandWriters::None => {}
        }
    }
    worst
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
    let (writes, reads, poisons, may_return) = opaque_sets(&node);
    Stmt::lowered(StmtKind::Opaque { writes, reads, poisons, may_return }, Vec::new())
}

/// Compute an `Opaque` construct's `(writes, reads, poisons, may_return)` over its
/// subtree. `reads` is every direct variable mentioned that is not already a write
/// — including branch conditions — so a construct that branches on a variable and
/// early-returns invalidates the fall-through binding (soundness; see the
/// [`StmtKind::Opaque`] docs). Nested function-like bodies are not descended.
fn opaque_sets(node: &Node<'_, '_>) -> (Vec<String>, Vec<String>, bool, bool) {
    let poisons = node_poisons(node);
    let may_return = node_may_return(node);
    let mut writes = Vec::new();
    // By-ref conservatism: every variable handed to any call in the subtree.
    collect_call_vars(node, &mut writes);
    // Assignment / increment / foreach-binding / catch-param write targets.
    collect_assign_writes(node, &mut writes);
    // Everything else the subtree merely reads / branches on.
    let mut reads = Vec::new();
    collect_read_vars(node, &writes, &mut reads);
    (writes, reads, poisons, may_return)
}

/// Whether `node`'s subtree contains a `return` statement the walk will not see as
/// a top-level [`StmtKind::Return`] — the load-bearing bit of [`StmtKind::Opaque`]'s
/// `may_return`. Nested function / method / closure / arrow bodies are their own
/// scopes and are not descended (their returns are not this scope's exits).
fn node_may_return(node: &Node<'_, '_>) -> bool {
    match node {
        Node::Return(_) => true,
        Node::Function(_) | Node::Method(_) | Node::Closure(_) | Node::ArrowFunction(_) => false,
        _ => {
            for child in children(node) {
                if node_may_return(&child) {
                    return true;
                }
            }
            false
        }
    }
}

/// The **source reads** a destructuring assignment target performs (issue #288):
/// one key path per target, outermost key first, in source order — see
/// [`StmtKind::Destructure`]. `None` when `lhs` is not a destructuring pattern at
/// all, or when it is one this lowering cannot read faithfully.
///
/// PHP's own key rule is the whole derivation: a positional element reads the next
/// auto index (a skipped hole `[, $b]` consumes its index without reading it), a
/// keyed element reads its own key, and a nested pattern reads the outer key AND
/// everything beneath it. Mixing the two spellings is a compile-time fatal in PHP,
/// so a mixed pattern is refused rather than given a key derivation no runtime
/// would ever exercise.
fn destructure_reads(lhs: &Expression<'_>) -> Option<Vec<Vec<ArgValue>>> {
    let mut reads = Vec::new();
    destructure_pattern_reads(lhs, &[], &mut reads)?;
    // `[] = $x;` is a fatal, not a read — and a pattern of nothing but holes
    // (`[, ,] = $x;`) reads nothing, so there is no read position to carry.
    (!reads.is_empty()).then_some(reads)
}

/// Walk one destructuring pattern level, appending each target's key path to
/// `out`. `Some(())` only for a pattern every element of which is faithfully
/// readable; see [`destructure_reads`].
fn destructure_pattern_reads(
    pattern: &Expression<'_>,
    prefix: &[ArgValue],
    out: &mut Vec<Vec<ArgValue>>,
) -> Option<()> {
    // Issue #246: a nested pattern walks one frame per level.
    if stack_guard::exhausted() {
        return None;
    }
    let elements: Vec<&ArrayElement<'_>> = match pattern.unparenthesized() {
        Expression::Array(a) => a.elements.iter().collect(),
        Expression::LegacyArray(a) => a.elements.iter().collect(),
        Expression::List(l) => l.elements.iter().collect(),
        _ => return None,
    };
    let mut auto: i64 = 0;
    let mut keyed = false;
    let mut positional = false;
    for element in elements {
        let (key, value) = match element {
            // A hole consumes its index and reads nothing (witnessed at 8.5.9:
            // `[, $b] = [];` warns for key 1 only).
            ArrayElement::Missing(_) => {
                auto = auto.checked_add(1)?;
                positional = true;
                continue;
            }
            ArrayElement::Value(v) => {
                let key = ArgValue::Int(auto);
                auto = auto.checked_add(1)?;
                positional = true;
                (key, v.value)
            }
            ArrayElement::KeyValue(kv) => {
                keyed = true;
                (destructure_key(kv.key)?, kv.value)
            }
            // A spread is not a destructuring target spelling.
            ArrayElement::Variadic(_) => return None,
        };
        if keyed && positional {
            return None;
        }
        let mut path = prefix.to_vec();
        path.push(key);
        // A by-reference target aliases the offset into existence rather than
        // reading it (`[&$a] = $m;` autovivifies `$m[0]` with no warning), so the
        // whole pattern is refused instead of being read as something it is not.
        if let Expression::UnaryPrefix(up) = value.unparenthesized()
            && matches!(up.operator, UnaryPrefixOperator::Reference(_))
        {
            return None;
        }
        out.push(path.clone());
        // A nested pattern reads the outer key (pushed above) and then recurses.
        if matches!(
            value.unparenthesized(),
            Expression::Array(_) | Expression::LegacyArray(_) | Expression::List(_)
        ) {
            destructure_pattern_reads(value, &path, out)?;
        }
    }
    Some(())
}

/// Lower a destructuring pattern's explicit key (`['a' => $x] = $m`) to the literal
/// the read judgment canonicalizes; `None` for any key the lowering cannot prove
/// (a variable, a call, a constant fetch), which refuses the whole pattern.
fn destructure_key(key: &Expression<'_>) -> Option<ArgValue> {
    match lower_arg_value(key) {
        v @ (ArgValue::Int(_) | ArgValue::Str(_) | ArgValue::Bool(_) | ArgValue::Null) => Some(v),
        _ => None,
    }
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
                let invalidated = call_invalidation(&Node::Expression(a.rhs));
                // `$x = f($s);` — carry the RHS call for propagation/descent.
                let call = if a.operator.is_assign() { named_call(a.rhs) } else { None };
                let span = to_span(a.lhs.span());
                Stmt::lowered(StmtKind::Assign { var, value, span, call }, invalidated)
            } else if let Expression::Access(Access::Property(pa)) = a.lhs.unparenthesized()
                && let Some((target_var, prop)) = prop_fetch_of(pa.object, &pa.property)
            {
                // `$var->prop = <rvalue>` / `$this->prop = <rvalue>` (ADR-0036). A
                // compound op (`+=`, `.=`, …) makes the property value unknown.
                let value = if a.operator.is_assign() { lower_arg_value(a.rhs) } else { ArgValue::Other };
                let value_call = if a.operator.is_assign() { named_call(a.rhs) } else { None };
                let invalidated = call_invalidation(&Node::Expression(a.rhs));
                let span = to_span(a.lhs.span());
                let kind = StmtKind::PropAssign { target_var, prop, value, value_call, span };
                Stmt::lowered(kind, invalidated)
            } else if a.operator.is_assign()
                && let Some((base, keys)) = const_key_offset_path(a.lhs)
            {
                // `$var[<lit>] = …` / `$var[<lit>][<lit>] = …` (ADR-0062 A-G8).
                // Still a barrier in the walk — see `StmtKind::OffsetWrite` — but
                // one that names the base and key so the shape lane survives it.
                let invalidated = call_invalidation(&Node::Expression(a.rhs));
                let value = lower_arg_value(a.rhs);
                Stmt::lowered(StmtKind::OffsetWrite { base, keys, value }, invalidated)
            } else if a.operator.is_assign()
                && let Some(reads) = destructure_reads(a.lhs)
            {
                // `[$a, $b] = <source>;` / `list($a, $b) = <source>;` (issue #288).
                // Barrier semantics for the targets, plus the source's own reads —
                // see `StmtKind::Destructure`.
                let invalidated = call_invalidation(&Node::Expression(a.rhs));
                let source = lower_arg_value(a.rhs);
                let call = named_call(a.rhs);
                let span = to_span(a.lhs.span());
                Stmt::lowered(StmtKind::Destructure { source, call, reads, span }, invalidated)
            } else {
                // Assignment to a non-simple lvalue (`$a[] = …`, `$a[$i] = …`,
                // `$o->$p = …`, `$a->b->c = …`, `Foo::$s = …`). Barrier (the sound
                // floor); a by-ref property alias `$r = &$x->p` is caught by the
                // poison family above.
                Stmt::lowered(StmtKind::Barrier, Vec::new())
            }
        }
        Expression::Call(Call::Function(fc)) => {
            // `assert(<expr>)` — a statement-position assert whose argument lowers to
            // a condition (ADR-0052 §5). `assert` is a pure by-value builtin (it never
            // mutates its argument by reference), so the narrowed variables carry no
            // invalidation; a non-lowerable argument falls back to a plain `Call`.
            if let Some(cond) = assert_stmt_cond(fc) {
                Stmt::lowered(StmtKind::Assert { cond }, Vec::new())
            } else {
                let invalidated = call_invalidation(&Node::Expression(expr));
                Stmt::lowered(StmtKind::Call(lower_call(fc)), invalidated)
            }
        }
        // Statement-level method / static / constructor calls. A resolvable
        // receiver becomes a `Call`; a dynamic one is a `Barrier` (but its
        // call-var invalidation is still collected below via the fallthrough).
        Expression::Call(Call::Method(_) | Call::NullSafeMethod(_) | Call::StaticMethod(_))
        | Expression::Instantiation(_) => match named_call(expr) {
            Some(call) => {
                let invalidated = call_invalidation(&Node::Expression(expr));
                Stmt::lowered(StmtKind::Call(call), invalidated)
            }
            None => {
                let invalidated = call_invalidation(&Node::Expression(expr));
                Stmt::lowered(StmtKind::Barrier, invalidated)
            }
        },
        // A statement-position `match` (ADR-0031 Part B): structure its arms when
        // the subject and every arm condition lower to a variable/literal; else
        // fall back to `Opaque` over the whole subtree (partial structuring is
        // unsound for the first-match / no-default-throws rules).
        Expression::Match(m) => lower_match_stmt(m).unwrap_or_else(|| {
            let node = Node::Expression(expr);
            let (writes, reads, poisons, may_return) = opaque_sets(&node);
            Stmt::lowered(StmtKind::Opaque { writes, reads, poisons, may_return }, Vec::new())
        }),
        // `throw <expr>;` — a trace terminator (ADR-0031). Variables the thrown
        // expression hands to a call are still invalidated (by-ref conservatism),
        // though the terminator makes anything after it unreachable.
        Expression::Throw(t) => {
            let invalidated = call_invalidation(&Node::Expression(t.exception));
            Stmt::lowered(StmtKind::Throw { span: to_span(expr.span()) }, invalidated)
        }
        // `exit;` / `die;` — a trace terminator (ADR-0019 never-returns).
        Expression::Construct(Construct::Exit(_) | Construct::Die(_)) => {
            Stmt::lowered(StmtKind::Exit { span: to_span(expr.span()) }, Vec::new())
        }
        _ => Stmt::lowered(StmtKind::Barrier, Vec::new()),
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
    for child in children(node) {
        collect_call_vars(&child, out);
    }
}

/// The [`InvalidatedVar`] entries a trace entry carries: every variable the
/// subtree hands to a call, one entry per name in first-occurrence order, each
/// carrying its provable sites or the opaque verdict (ADR-0070). Every
/// construction site takes the whole answer from this one walk, so a name and
/// its evidence can never be computed over different subtrees.
fn call_invalidation(node: &Node<'_, '_>) -> Vec<InvalidatedVar> {
    let mut invalidated = Vec::new();
    scan_invalidated(node, &mut invalidated, false);
    invalidated
}

/// The one walk behind [`Stmt::invalidated`]: exactly [`collect_call_vars`]'
/// shape — the same four call nodes, the same "a bare `$v` argument"
/// recognition, the same descent — but recording each occurrence's evidence on
/// the name's entry as it collects the name, so the name set and its evidence
/// are one answer by construction. A describable occurrence appends a
/// `(callee, position)` site; an unprovable one marks the entry opaque, which
/// discards every site the name has and refuses it any future one — an opaque
/// entry carries no sites, whatever the occurrence order was.
///
/// Unprovable, and therefore opaque (kept on the blanket drop):
///
/// * a method / nullsafe-method / static-method call — the receiver's own
///   mutability is a separate question (ADR-0070 §4) and no `NameRef` names the
///   target anyway;
/// * a dynamic function callee (`$f($a)`, `($o->cb)($a)`) — nothing to resolve;
/// * an argument list carrying a **named** or **spread** argument, or a
///   first-class callable (`f(...)`) — positional mapping is defeated there, so
///   a position index would be a guess;
/// * an occurrence inside a nested function-like body (`nested`) — a closure,
///   arrow-function or (anonymous-)class member body is a different variable
///   scope, so a bare `$v` there is not this scope's `$v`; the name is still
///   collected (the blanket drop's conservatism) but no site may vouch for it.
///
/// Language constructs (`isset`, `empty`, `unset`, `list`, `eval`, `exit`) are
/// not call nodes and never reach this walk, so this path cannot change their
/// semantics.
fn scan_invalidated(node: &Node<'_, '_>, out: &mut Vec<InvalidatedVar>, nested: bool) {
    let nested = nested
        || matches!(
            node,
            Node::Function(_)
                | Node::Closure(_)
                | Node::ArrowFunction(_)
                | Node::AnonymousClass(_)
                | Node::Class(_)
                | Node::Interface(_)
                | Node::Trait(_)
                | Node::Enum(_)
        );
    let arguments = match node {
        Node::FunctionCall(c) => {
            let callee = match c.function {
                Expression::Identifier(id) => Some(name_ref(id)),
                _ => None,
            };
            Some((&c.argument_list, callee))
        }
        Node::MethodCall(c) => Some((&c.argument_list, None)),
        Node::NullSafeMethodCall(c) => Some((&c.argument_list, None)),
        Node::StaticMethodCall(c) => Some((&c.argument_list, None)),
        _ => None,
    };
    if let Some((list, callee)) = arguments {
        // One named or spread argument anywhere makes every index in the list
        // unreliable, so the verdict is taken over the whole list, not per
        // argument.
        let all_positional = list
            .arguments
            .iter()
            .all(|a| matches!(a, Argument::Positional(p) if p.ellipsis.is_none()));
        for (position, arg) in list.arguments.iter().enumerate() {
            if let Expression::Variable(Variable::Direct(dv)) = arg.value().unparenthesized() {
                let var = strip_dollar(bytes_to_string(dv.name));
                let site = match &callee {
                    Some(c) if all_positional && !nested => Some((c.clone(), position as u32)),
                    _ => None,
                };
                note_occurrence(out, var, site);
            }
        }
    }
    for child in children(node) {
        scan_invalidated(&child, out, nested);
    }
}

/// Record one occurrence of `name` on its [`InvalidatedVar`] entry (created on
/// first sight, so entries keep first-occurrence order): a provable occurrence
/// carries its `(callee, position)` site, an unprovable one (`None`) marks the
/// entry opaque. The entry invariant is maintained here and nowhere else — an
/// opaque entry carries no sites, so turning opaque discards the sites already
/// gathered and a site arriving after the verdict is dropped.
fn note_occurrence(out: &mut Vec<InvalidatedVar>, name: String, site: Option<(NameRef, u32)>) {
    let entry = match out.iter().position(|e| e.name == name) {
        Some(i) => &mut out[i],
        None => {
            out.push(InvalidatedVar { name, opaque: false, sites: Vec::new() });
            out.last_mut().expect("just pushed")
        }
    };
    match site {
        Some(s) if !entry.opaque => entry.sites.push(s),
        Some(_) => {}
        None => {
            entry.opaque = true;
            entry.sites.clear();
        }
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
    for child in children(node) {
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
    for child in children(node) {
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
    for child in children(node) {
        collect_read_vars(&child, writes, out);
    }
}

/// Collect one [`ForeachSite`] per `foreach` statement in the subtree, in source
/// order (ADR-0076 §4: **every** `foreach` is a candidate, so the transform's
/// refusal distribution measures its own narrowness).
///
/// `scope_end` is the end offset of the enclosing **variable** scope, refreshed
/// whenever the walk enters a function-like body — PHP's variable scope is the
/// function, so that is the region an iteration variable can outlive the loop in.
///
/// Sibling order comes straight from [`Node::children`]: every statement-sequence
/// container (the program, a block, a colon-delimited body) emits its statements
/// as consecutive `Node::Statement` children, so the statement preceding a
/// `foreach` is whichever statement child came before it here.
fn collect_foreach_sites(node: &Node<'_, '_>, scope_end: u32, out: &mut Vec<ForeachSite>) {
    let scope_end = match node {
        Node::Function(_)
        | Node::Method(_)
        | Node::Closure(_)
        | Node::ArrowFunction(_)
        | Node::PropertyHook(_) => to_span(node.span()).end,
        _ => scope_end,
    };
    let mut prev: Option<&Statement<'_>> = None;
    for child in children(node) {
        if let Node::Statement(s) = child {
            if let Statement::Foreach(fe) = s {
                out.push(lower_foreach_site(fe, to_span(s.span()), prev, scope_end));
            }
            prev = Some(s);
        }
        collect_foreach_sites(&child, scope_end, out);
    }
}

// invalid operands (ADR-0078, issue #191)

/// Collect every arithmetic/bitwise/shift operator application in the file, in
/// pre-order (ADR-0078, issue #191). Recursion is unconditional, matching
/// [`collect_array_literal_sites`]: a site nested in a call argument, an array
/// element or a closure body is still found — the `enclosing_body` each site
/// carries is what keeps a closure's site from being judged against the
/// enclosing scope's env, not a truncated walk.
///
/// Pre-order plus source-ordered children means the output is sorted by span
/// start (a parent's span starts at or before every child's), which is the
/// ordering [`SourceTree::operand_sites`] promises.
fn collect_operand_sites(node: &Node<'_, '_>, body: Option<Span>, out: &mut Vec<OperandSite>) {
    // The innermost enclosing function-like body, refreshed on entry exactly as
    // `collect_foreach_sites` refreshes `scope_end` — PHP's variable scope is
    // the function, and this field's whole job is to name that scope.
    let body = match node {
        Node::Function(_)
        | Node::Method(_)
        | Node::Closure(_)
        | Node::ArrowFunction(_)
        | Node::PropertyHook(_) => Some(to_span(node.span())),
        _ => body,
    };
    match node {
        Node::Binary(b) => {
            if let Some(op) = binary_operand_op(&b.operator) {
                out.push(OperandSite {
                    span: to_span(node.span()),
                    kind: OperandSiteKind::Binary {
                        op,
                        lhs: lower_arg_value(b.lhs),
                        rhs: lower_arg_value(b.rhs),
                    },
                    enclosing_body: body,
                });
            }
        }
        Node::UnaryPrefix(u) => {
            if let Some(op) = unary_operand_op(&u.operator) {
                out.push(OperandSite {
                    span: to_span(node.span()),
                    kind: OperandSiteKind::Unary { op, operand: lower_arg_value(u.operand) },
                    enclosing_body: body,
                });
            }
        }
        _ => {}
    }
    for child in children(node) {
        collect_operand_sites(&child, body, out);
    }
}

/// The [`BinaryOperandOp`] of a CST binary operator, or `None` for an operator
/// whose operand types PHP never refuses fatally — see [`OperandSite`] for why
/// concatenation, the comparisons and the logical operators are all `None`.
fn binary_operand_op(op: &BinaryOperator<'_>) -> Option<BinaryOperandOp> {
    match op {
        BinaryOperator::Addition(_) => Some(BinaryOperandOp::Add),
        BinaryOperator::Subtraction(_) => Some(BinaryOperandOp::Sub),
        BinaryOperator::Multiplication(_) => Some(BinaryOperandOp::Mul),
        BinaryOperator::Division(_) => Some(BinaryOperandOp::Div),
        BinaryOperator::Modulo(_) => Some(BinaryOperandOp::Mod),
        BinaryOperator::Exponentiation(_) => Some(BinaryOperandOp::Pow),
        BinaryOperator::BitwiseAnd(_) => Some(BinaryOperandOp::BitAnd),
        BinaryOperator::BitwiseOr(_) => Some(BinaryOperandOp::BitOr),
        BinaryOperator::BitwiseXor(_) => Some(BinaryOperandOp::BitXor),
        BinaryOperator::LeftShift(_) => Some(BinaryOperandOp::ShiftLeft),
        BinaryOperator::RightShift(_) => Some(BinaryOperandOp::ShiftRight),
        _ => None,
    }
}

/// The [`UnaryOperandOp`] of a CST unary prefix operator. `!`, the casts, `@`,
/// `&` and `++`/`--` are all `None` (see [`OperandSite`]).
fn unary_operand_op(op: &UnaryPrefixOperator<'_>) -> Option<UnaryOperandOp> {
    match op {
        UnaryPrefixOperator::Negation(_) => Some(UnaryOperandOp::Minus),
        UnaryPrefixOperator::Plus(_) => Some(UnaryOperandOp::Plus),
        UnaryPrefixOperator::BitwiseNot(_) => Some(UnaryOperandOp::BitNot),
        _ => None,
    }
}

// end invalid operands (ADR-0078, issue #191)

/// Collect every literal array expression in the file, file-wide, including
/// nested ones (issue #187) — recursion is unconditional, matching
/// [`collect_foreach_sites`], so an array literal nested inside another
/// array's value, a call argument, a closure body, … is still found.
fn collect_array_literal_sites(node: &Node<'_, '_>, out: &mut Vec<ArrayLiteralSite>) {
    match node {
        Node::Array(a) => out.push(lower_array_literal_site(a.elements.iter())),
        Node::LegacyArray(a) => out.push(lower_array_literal_site(a.elements.iter())),
        _ => {}
    }
    for child in children(node) {
        collect_array_literal_sites(&child, out);
    }
}

/// Lower one array literal's elements to their [`ArrayLiteralSite`] shape.
/// Purely syntactic: only the key side is resolved (`lower_array_key`'s
/// coercion); the value side is never lowered or evaluated.
fn lower_array_literal_site<'a>(
    elements: impl Iterator<Item = &'a ArrayElement<'a>>,
) -> ArrayLiteralSite {
    let elements = elements
        .map(|el| {
            let span = to_span(el.span());
            let key = match el {
                ArrayElement::Value(_) => Some(ArrayKey::Auto),
                ArrayElement::KeyValue(kv) => lower_array_key(kv.key),
                // A spread contributes an unknown number of unknown keys; a
                // destructuring hole (only ever seen in `list()` lvalue
                // position, never a legal literal) contributes none either —
                // both are `None`, the same "no knowable key here" the fold
                // gate already uses for an unresolvable explicit key.
                ArrayElement::Variadic(_) | ArrayElement::Missing(_) => None,
            };
            ArrayLiteralElement { key, span }
        })
        .collect();
    ArrayLiteralSite { elements }
}

/// Lower one `foreach` into its [`ForeachSite`] shape. Purely syntactic — every
/// field is a fact about how the loop is *written*.
fn lower_foreach_site(
    fe: &mago_syntax::cst::Foreach<'_>,
    span: Span,
    prev: Option<&Statement<'_>>,
    scope_end: u32,
) -> ForeachSite {
    let target = &fe.target;
    let value = target.value();
    ForeachSite {
        span,
        subject: direct_var_name(fe.expression),
        key_binding: target.key().is_some(),
        by_ref_binding: value.is_reference(),
        // A by-ref target's operand is still a variable; the by-ref flag is the
        // refusal-bearing fact, so the name is reported either way.
        value_var: direct_var_name(strip_reference(value)),
        body: lower_foreach_body(&fe.body),
        prev_stmt: prev.map(lower_prev_stmt),
        scope_end,
    }
}

/// The variable name of an expression that is exactly `$name` (no `$`); `None`
/// for every other expression, including `$$name` and `${…}`.
fn direct_var_name(expr: &Expression<'_>) -> Option<String> {
    match expr {
        Expression::Variable(Variable::Direct(dv)) => {
            Some(strip_dollar(bytes_to_string(dv.name)))
        }
        _ => None,
    }
}

/// Peel a leading `&` off a by-reference binding target, so the bound name is
/// still readable.
fn strip_reference<'a, 'arena: 'a>(expr: &'a Expression<'arena>) -> &'a Expression<'arena> {
    match expr {
        Expression::UnaryPrefix(u) if matches!(u.operator, UnaryPrefixOperator::Reference(_)) => {
            u.operand
        }
        _ => expr,
    }
}

/// Reduce the statement preceding a `foreach` to the adjacency rule's inputs
/// (ADR-0076 §3): is it an assignment, to which variable, and is the right-hand
/// side an empty array literal?
fn lower_prev_stmt(s: &Statement<'_>) -> PrevStmt {
    let span = to_span(s.span());
    let Statement::Expression(es) = s else {
        return PrevStmt { span, assign_target: None, assigns_empty_array: false };
    };
    let Expression::Assignment(a) = es.expression else {
        return PrevStmt { span, assign_target: None, assigns_empty_array: false };
    };
    if !a.operator.is_assign() {
        return PrevStmt { span, assign_target: None, assigns_empty_array: false };
    }
    PrevStmt {
        span,
        assign_target: direct_var_name(a.lhs),
        assigns_empty_array: is_empty_array_literal(a.rhs),
    }
}

/// Whether an expression is an empty array literal — `[]` or `array()`.
fn is_empty_array_literal(expr: &Expression<'_>) -> bool {
    match expr {
        Expression::Array(a) => a.elements.is_empty(),
        Expression::LegacyArray(a) => a.elements.is_empty(),
        _ => false,
    }
}

/// Lower a `foreach` body to its [`ForeachBodyShape`].
///
/// The braced form `foreach (…) { … }` arrives as a single `Statement::Block`,
/// so the block is unwrapped: `{ $out[] = $x; }` is a **one**-statement body, not
/// a one-block one. A `Noop` (`foreach (…) ;`) is an empty body, not a
/// one-statement one.
fn lower_foreach_body(body: &mago_syntax::cst::ForeachBody<'_>) -> ForeachBodyShape {
    let raw: &[Statement<'_>] = match body.statements() {
        [Statement::Block(b)] => b.statements.as_slice(),
        other => other,
    };
    let statements: Vec<&Statement<'_>> =
        raw.iter().filter(|s| !matches!(s, Statement::Noop(_))).collect();
    let append = match statements.as_slice() {
        [only] => lower_append_stmt(only),
        _ => None,
    };
    let early_exit =
        statements.iter().copied().any(|s| body_has_early_exit(&Node::Statement(s)));
    ForeachBodyShape { stmt_count: statements.len(), append, early_exit }
}

/// Lower a statement that is exactly `$acc[] = <expr>;` into an [`AppendStmt`];
/// `None` for anything else (a compound `.=`, an offset write `$acc[$k] = …`, a
/// non-variable base, a call, a nested construct).
fn lower_append_stmt(s: &Statement<'_>) -> Option<AppendStmt> {
    let Statement::Expression(es) = s else { return None };
    let Expression::Assignment(a) = es.expression else { return None };
    if !a.operator.is_assign() {
        return None;
    }
    let Expression::ArrayAppend(app) = a.lhs else { return None };
    let acc = direct_var_name(app.array)?;

    let mut value_vars = Vec::new();
    collect_direct_vars(&Node::Expression(a.rhs), &mut value_vars);
    let mut writes = Vec::new();
    collect_assign_writes(&Node::Expression(a.rhs), &mut writes);
    Some(AppendStmt {
        acc,
        value_span: to_span(a.rhs.span()),
        value_vars,
        value_writes: !writes.is_empty(),
        value_unmodelled: expr_is_unmodelled(&Node::Expression(a.rhs)),
    })
}

/// Whether a subtree carries a `break` / `continue` / `return` / `goto` that
/// belongs to the enclosing loop. Nested function-like bodies are skipped: a
/// `return` inside a closure returns from the closure, not from the loop.
fn body_has_early_exit(node: &Node<'_, '_>) -> bool {
    match node {
        Node::Break(_) | Node::Continue(_) | Node::Return(_) | Node::Goto(_) => return true,
        Node::Function(_)
        | Node::Closure(_)
        | Node::ArrowFunction(_)
        | Node::AnonymousClass(_)
        | Node::Class(_)
        | Node::Interface(_)
        | Node::Trait(_)
        | Node::Enum(_) => return false,
        _ => {}
    }
    children(node).iter().any(body_has_early_exit)
}

/// The scope-sensitive builtins whose meaning is defined by the *frame* they are
/// written in, so moving the expression into an arrow function changes what they
/// answer (ADR-0076: read as an unanalyzable call target).
const FRAME_SENSITIVE_BUILTINS: &[&str] =
    &["compact", "get_defined_vars", "func_get_args", "func_num_args"];

/// Whether an expression carries a construct the effect scan does not model as a
/// call, and which therefore cannot be shown effect-free: `new` (constructor
/// effects are not on the fixpoint), `clone` (`__clone`), `yield`, a backtick
/// shell execute, an ADR-0001 poison construct, or a frame-sensitive builtin.
/// Nested function-like bodies are descended deliberately — an arrow function in
/// the appended expression is part of what the rewrite moves.
fn expr_is_unmodelled(node: &Node<'_, '_>) -> bool {
    node_poisons(node) || scan_unmodelled(node)
}

/// The construct half of [`expr_is_unmodelled`] (the poison half runs once, over
/// the whole expression, in the caller).
fn scan_unmodelled(node: &Node<'_, '_>) -> bool {
    match node {
        Node::Instantiation(_)
        | Node::Clone(_)
        | Node::Yield(_)
        | Node::YieldFrom(_)
        | Node::YieldPair(_)
        | Node::YieldValue(_)
        | Node::ShellExecuteString(_)
        | Node::AnonymousClass(_) => return true,
        Node::FunctionCall(fc) => {
            if let Expression::Identifier(id) = fc.function {
                let name = bytes_to_string(id.last_segment()).to_ascii_lowercase();
                if FRAME_SENSITIVE_BUILTINS.contains(&name.as_str()) {
                    return true;
                }
            }
        }
        _ => {}
    }
    children(node).iter().any(scan_unmodelled)
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
    for child in children(node) {
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
    for child in children(node) {
        collect_namespaces(&child, contexts, regions);
    }
}

/// Fold one `use` statement's items into a context — every import form: the plain
/// sequence (`use A\B, C\D;`), the typed sequences (`use function a\b;`,
/// `use const A\FOO;`), and the **grouped** forms (`use A\{B, C}`,
/// `use function A\{b, c}`, `use const A\{X, Y}`, and the mixed
/// `use A\{B, function c, const D}`).
///
/// Grouped imports must be lowered because an unresolved import falls back through
/// [`resolve_class_ref`] to the enclosing namespace and can collide with a different
/// class, producing a false positive (ADR-0049 §6). `use const` items joined the
/// same discipline with issue #198, for the same reason on the constant side: an
/// unlowered const import would make `FOO` read as `Ns\FOO` and manufacture an
/// absence. Their alias keys are exact-case — see [`NsCtx::const_imports`].
fn add_use(u: &mago_syntax::cst::Use<'_>, ctx: &mut NsCtx) {
    match &u.items {
        UseItems::Sequence(seq) => {
            for item in seq.items.iter() {
                let target = bytes_to_string(item.name.value()).trim_start_matches('\\').to_owned();
                ctx.class_imports.insert(use_item_alias(item), target);
            }
        }
        // `use function a\b;` and `use const A\FOO, B\BAR;` (the latter, issue #198,
        // with exact-case alias keys).
        UseItems::TypedSequence(seq) => {
            let is_fn = seq.r#type.is_function();
            for item in seq.items.iter() {
                let target = bytes_to_string(item.name.value()).trim_start_matches('\\').to_owned();
                if is_fn {
                    ctx.fn_imports.insert(use_item_alias(item), target);
                } else {
                    ctx.const_imports.insert(use_item_bound_name(item), target);
                }
            }
        }
        // Grouped `use function A\{b, c}` / `use const A\{X, Y}`: one leading type
        // applies to every item under the `A\` prefix.
        UseItems::TypedList(list) => {
            let prefix = bytes_to_string(list.namespace.value());
            if list.r#type.is_function() {
                for item in list.items.iter() {
                    ctx.fn_imports.insert(use_item_alias(item), group_target(&prefix, item));
                }
            } else if list.r#type.is_const() {
                for item in list.items.iter() {
                    ctx.const_imports
                        .insert(use_item_bound_name(item), group_target(&prefix, item));
                }
            }
        }
        // Grouped `use A\{B, function c, const D}`: each item carries its own
        // optional type (`None` ⇒ class, `Function` ⇒ function, `Const` ⇒ constant).
        UseItems::MixedList(list) => {
            let prefix = bytes_to_string(list.namespace.value());
            for mti in list.items.iter() {
                let target = group_target(&prefix, &mti.item);
                match &mti.r#type {
                    None => {
                        ctx.class_imports.insert(use_item_alias(&mti.item), target);
                    }
                    Some(t) if t.is_function() => {
                        ctx.fn_imports.insert(use_item_alias(&mti.item), target);
                    }
                    Some(_) => {
                        ctx.const_imports.insert(use_item_bound_name(&mti.item), target);
                    }
                }
            }
        }
    }
}

/// The lowercase-normalized import alias for a `use` item: its explicit `as` alias,
/// else the last segment of the imported name (PHP class/function names are
/// case-insensitive, so the map keys on the lowercased form).
/// Whether a `use` statement binds the (case-sensitive) alias `PHP_VERSION_ID`
/// through any of its **const** item forms (issue #29). The exact-case binding
/// name is the explicit `as` alias, else the imported name's last segment.
fn use_binds_php_version_id(u: &mago_syntax::cst::Use<'_>) -> bool {
    use_binds_const_named(u, |bound| bound == "PHP_VERSION_ID")
}

/// The modeled `PREG_*` flag constant names (issue #168) — the four whose values
/// the out-parameter seed resolves. Kept beside the shadow scans that consult it;
/// the values live with the consumer (`steins-infer`), not here.
const PREG_FLAG_CONST_NAMES: &[&str] =
    &["PREG_PATTERN_ORDER", "PREG_SET_ORDER", "PREG_OFFSET_CAPTURE", "PREG_UNMATCHED_AS_NULL"];

/// `use const … as PREG_SET_ORDER` / `use const …\PREG_SET_ORDER` and siblings
/// (issue #168) — see [`use_binds_php_version_id`], whose rules this mirrors for
/// the modeled preg flag constants.
fn use_binds_preg_flag_const(u: &mago_syntax::cst::Use<'_>) -> bool {
    use_binds_const_named(u, |bound| PREG_FLAG_CONST_NAMES.contains(&bound))
}

/// Whether a `use` statement `use const`-imports something whose **bound name**
/// (the alias if present, else the last segment) satisfies `wanted`. Constant
/// names are case-sensitive; the match is exact. Const imports are otherwise
/// unlowered (out of scope), so the flags fed from this are the only thing read
/// from them.
fn use_binds_const_named(u: &mago_syntax::cst::Use<'_>, wanted: impl Fn(&str) -> bool) -> bool {
    let item_binds = |item: &mago_syntax::cst::UseItem<'_>| -> bool {
        let bound = match &item.alias {
            Some(a) => bytes_to_string(a.identifier.value),
            None => bytes_to_string(item.name.last_segment()),
        };
        wanted(&bound)
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

/// The **exact-case** name a `use` item binds — [`use_item_alias`]'s constant-side
/// twin (issue #198). Same rule (the explicit `as` alias, else the imported name's
/// last segment) with the lowercasing omitted, because constant names are
/// case-sensitive and `use const A\FOO;` binds `FOO`, never `foo`.
fn use_item_bound_name(item: &mago_syntax::cst::UseItem<'_>) -> String {
    match &item.alias {
        Some(a) => bytes_to_string(a.identifier.value),
        None => bytes_to_string(item.name.last_segment()),
    }
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

/// The children of `node`, **or none when the stack is spent** (issue #264).
///
/// Every walker in this file descends through here, so this one function is the
/// whole depth guard for the CST walk: when [`stack_guard::exhausted`] says the
/// remaining headroom is gone, a walker is handed an empty child list and returns
/// the way it would at a leaf. No walker's control flow changes, no walker
/// unwinds, and the parse still produces a tree — a partial one, which
/// [`SourceTree::parse`] then reports as a recovered parse error rather than
/// letting the process (or the wasm module) die walking it.
///
/// On every native target the guard is off by default and this is
/// `node.children()` behind one thread-local read; see [`stack_guard`].
fn children<'ast, 'arena>(node: &Node<'ast, 'arena>) -> Vec<Node<'ast, 'arena>> {
    if stack_guard::exhausted() {
        return Vec::new();
    }
    node.children()
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
