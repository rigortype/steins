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
use mago_syntax::cst::Argument;
use mago_syntax::cst::Attribute;
use mago_syntax::cst::Call;
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
use mago_syntax::cst::PartialArgument;
use mago_syntax::cst::Program;
use mago_syntax::cst::Statement;
use mago_syntax::cst::UnaryPrefixOperator;
use mago_syntax::cst::UseItems;
use mago_syntax::cst::Variable;

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

/// A simple scalar native parameter type (`int`, `?string`, …). Non-scalar,
/// union, and intersection hints are lowered to `None` on the [`Param`] so the
/// checker stays silent on them (zero-FP; ADR-0002).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ParamType {
    pub scalar: ScalarType,
    pub nullable: bool,
}

/// A single declared parameter.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Param {
    /// Parameter name without the leading `$`.
    pub name: String,
    /// Simple scalar type, or `None` when untyped / non-scalar / complex.
    pub ty: Option<ParamType>,
    /// `...$x` — the checker skips this and every later position.
    pub variadic: bool,
    /// `&$x` — by-reference; the checker skips it.
    pub by_ref: bool,
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
    /// A call to a statically-named function `name` (the last, unqualified
    /// segment) at `span` (the callee identifier). May resolve to a builtin
    /// (classified via the catalog) or a same-file user function (an effect
    /// propagation edge). Dynamic and method calls are not recorded.
    Call { name: String, span: Span },
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
    /// `Foo::m()` or `new Foo()->m()` — resolved on `Foo`'s chain, exact.
    ClassName(String),
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
    pub params: Vec<Param>,
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
    /// The span of the method name identifier (for diagnostic positions).
    pub span: Span,
    /// The recognized effect envelope, if declared (see [`FunctionDecl`]).
    pub effect_envelope: Option<EffectEnvelope>,
    /// Structural effect-origin candidates in the body (see [`EffectOrigin`]).
    /// Empty for abstract methods (no body).
    pub effect_origins: Vec<EffectOrigin>,
    pub visibility: Visibility,
    pub is_static: bool,
    pub is_final: bool,
    pub is_abstract: bool,
    /// `true` iff the method name is `__construct` (case-insensitive).
    pub is_constructor: bool,
}

/// A user-defined class declaration (top-level or namespaced). Interfaces,
/// traits, and enums are **not** lowered to this — they carry no method bodies
/// this slice checks (a class that *uses* a trait sets [`ClassDecl::uses_traits`]
/// so resolution gives up on it).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ClassDecl {
    /// Simple (unqualified) class name as written at the declaration site.
    pub name: String,
    pub is_final: bool,
    /// The `extends` parent's simple (last-segment) name, if any. Method
    /// resolution walks this chain in-file; a parent not defined in the same
    /// file makes the chain incomplete (→ unknown → silent).
    pub parent: Option<String>,
    pub methods: Vec<MethodDecl>,
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
    /// `new ClassName(args...)` — a construction rvalue. `String` is the simple
    /// class name as written. Carried so an assignment `$x = new Foo(...)` can
    /// record `$x`'s **exact class** in the propagation environment (the object's
    /// runtime class is fixed at construction). Not a scalar literal — it never
    /// flows into a scalar type check.
    New(String, Vec<ArgValue>),
    Other,
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
            ArgValue::Call(name, args) | ArgValue::New(name, args) => {
                name.hash(state);
                args.hash(state);
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
            ArgValue::New(name, _) => format!("new {name}()"),
            ArgValue::Other => "<expr>".to_owned(),
        }
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
    Method { receiver: Receiver, method: String },
    /// `Class::m(args...)` — a static (scope-resolution `::`) call.
    Static { class: StaticClass, method: String },
    /// `new Class(args...)` — a constructor call (`args` are the ctor args).
    Construct { class: String },
    /// A receiver or method name the lowering cannot represent (dynamic method
    /// name, `$obj[...]->m()`, `$var::m()`, …). Never resolves.
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
    /// `(new Foo(...))->m()` — an exact-class receiver (runtime class is `Foo`).
    New(String),
}

/// The class portion of a static `Class::m()` call, as written.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum StaticClass {
    /// An explicit class name (last segment), e.g. `Foo::m()` — exact.
    Named(String),
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
    /// A statement-level function call `f(args);`.
    Call(CallExpr),
    /// `return <value>;` (value is [`ArgValue::Other`] for `return;`). `call`
    /// carries the full [`CallExpr`] when the returned expression *is* a
    /// statically-named call (`return f($s);` — one of the most common shapes in
    /// real PHP), so the propagation pass and interprocedural descent reach it.
    Return { value: ArgValue, call: Option<CallExpr> },
    /// `echo e1, e2, …;` — carries the statically-named calls among its operands
    /// so the propagation pass checks/descends them. Echo assigns nothing, so its
    /// env effect stays conservative (a `Barrier`-equivalent clear afterward).
    Echo(Vec<CallExpr>),
    /// A recognized *control-flow* construct (`if`/`while`/`for`/`foreach`/
    /// `do-while`/`switch`/`match`-statement/`try`/nested block) whose internal
    /// data-flow the trace does not model, but whose **write set** it does. This
    /// is the ADR-0027 ratchet applied to what used to be a blanket
    /// [`StmtKind::Barrier`]: instead of erasing *all* known values, the walk
    /// forgets only the variables the construct might touch.
    ///
    /// * `writes` — the over-approximated set of variable names the subtree may
    ///   assign (any assignment lvalue, compound assign, increment/decrement,
    ///   `foreach` value/key binding, `catch` parameter, `list()`
    ///   destructuring) *plus* every variable handed to any call inside it
    ///   (by-ref conservatism). Over-collection is always sound — it only
    ///   forgets more. Nested function/closure bodies are separate scopes and
    ///   their internal writes are **not** counted.
    /// * `poisons` — `true` if the subtree contains any ADR-0001 poison marker
    ///   (reference/`global`/`static`/variable-variable/`extract`/`include`/
    ///   by-ref `use`, …). When set, the walk clears the whole env, exactly as a
    ///   `Barrier` would; the enclosing scope is independently poisoned too.
    Opaque { writes: Vec<String>, poisons: bool },
    /// Any construct the trace does not model *and* whose write set it cannot
    /// bound (`goto`, labels, `declare`, `__halt_compiler`, and anything the
    /// lowering is unsure of). Erases all known values — the sound floor.
    Barrier,
}

/// A trace entry plus the local variables it feeds into a call.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Stmt {
    pub kind: StmtKind,
    /// Variables passed as an argument to *any* call within this statement. The
    /// checker marks them unknown *after* the statement — PHP by-reference
    /// parameters could mutate them, so a value can't be trusted past a call it
    /// was handed to (conservatively covering unseen `&$x` signatures).
    pub invalidated: Vec<String>,
}

/// Who owns an analysis [`Scope`] — the top-level script, a free function, or a
/// class method. Method scopes carry their declaring class so `$this->`, `self::`,
/// and `parent::` calls inside them resolve against the right chain (ADR-0001).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ScopeOwner {
    TopLevel,
    Function(String),
    Method { class: String, method: String },
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
}

/// A recovered parse error with its span (ADR-0003: error-tolerant).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ParseError {
    pub message: String,
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
    parse_errors: Vec<ParseError>,
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

        let mut lowered = Lowered::default();
        walk(&Node::Program(program), &aliases, &mut lowered);

        let classes = lower_classes(&Node::Program(program), &aliases);
        let scopes = lower_scopes(program);

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
            line_starts: line_starts(source),
            text: source.to_owned(),
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

fn walk(node: &Node<'_, '_>, aliases: &SteinsAttrAliases, out: &mut Lowered) {
    match node {
        Node::Function(f) => out.functions.push(lower_function(f, aliases)),
        Node::FunctionCall(c) => out.calls.push(lower_call(c)),
        Node::DeclareItem(d) if is_strict_types_one(d) => out.strict_types = true,
        _ => {}
    }
    for child in node.children() {
        walk(&child, aliases, out);
    }
}

fn lower_function(f: &Function<'_>, aliases: &SteinsAttrAliases) -> FunctionDecl {
    let mut effect_origins = Vec::new();
    for s in f.body.statements.iter() {
        scan_effect_origins(&Node::Statement(s), &mut effect_origins);
    }

    FunctionDecl {
        name: bytes_to_string(f.name.value),
        params: lower_params(&f.parameter_list),
        span: to_span(f.name.span()),
        effect_envelope: attrs_effect_envelope(&f.attribute_lists, aliases),
        effect_origins,
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
            span: to_span(p.span()),
        })
        .collect()
}

/// Lower every `class` declaration reachable from `node` (interfaces, traits,
/// and enums are skipped — they carry no method bodies this slice checks).
fn lower_classes(node: &Node<'_, '_>, aliases: &SteinsAttrAliases) -> Vec<ClassDecl> {
    let mut out = Vec::new();
    lower_classes_into(node, aliases, &mut out);
    out
}

fn lower_classes_into(node: &Node<'_, '_>, aliases: &SteinsAttrAliases, out: &mut Vec<ClassDecl>) {
    if let Node::Class(c) = node {
        out.push(lower_class(c, aliases));
    }
    for child in node.children() {
        lower_classes_into(&child, aliases, out);
    }
}

fn lower_class(c: &Class<'_>, aliases: &SteinsAttrAliases) -> ClassDecl {
    let parent = c
        .extends
        .as_ref()
        .and_then(|e| e.types.iter().next())
        .map(|id| bytes_to_string(id.last_segment()));

    let mut methods = Vec::new();
    let mut uses_traits = false;
    for member in c.members.iter() {
        match member {
            ClassLikeMember::Method(m) => methods.push(lower_method(m, aliases)),
            ClassLikeMember::TraitUse(_) => uses_traits = true,
            _ => {}
        }
    }

    ClassDecl {
        name: bytes_to_string(c.name.value),
        is_final: c.modifiers.iter().any(Modifier::is_final),
        parent,
        methods,
        uses_traits,
        span: to_span(c.name.span()),
    }
}

fn lower_method(m: &Method<'_>, aliases: &SteinsAttrAliases) -> MethodDecl {
    let mut effect_origins = Vec::new();
    if let MethodBody::Concrete(block) = &m.body {
        for s in block.statements.iter() {
            scan_effect_origins(&Node::Statement(s), &mut effect_origins);
        }
    }

    let visibility = if m.modifiers.iter().any(Modifier::is_private) {
        Visibility::Private
    } else if m.modifiers.iter().any(Modifier::is_protected) {
        Visibility::Protected
    } else {
        Visibility::Public
    };

    let name = bytes_to_string(m.name.value);
    let is_constructor = name.eq_ignore_ascii_case("__construct");

    MethodDecl {
        name,
        params: lower_params(&m.parameter_list),
        span: to_span(m.name.span()),
        effect_envelope: attrs_effect_envelope(&m.attribute_lists, aliases),
        effect_origins,
        visibility,
        is_static: m.modifiers.iter().any(Modifier::is_static),
        is_final: m.modifiers.iter().any(Modifier::is_final),
        is_abstract: m.is_abstract(),
        is_constructor,
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

/// Walk a function-body subtree, appending every [`EffectOrigin`] found. Does not
/// descend into nested scopes (function/closure/arrow/class-like bodies), whose
/// effects are their own concern.
fn scan_effect_origins(node: &Node<'_, '_>, out: &mut Vec<EffectOrigin>) {
    match node {
        // A statically-named call is either a builtin (catalog-classified) or a
        // same-file user function (a propagation edge) — the effects pass decides.
        Node::FunctionCall(fc) => {
            if let Expression::Identifier(id) = fc.function {
                out.push(EffectOrigin::Call {
                    name: bytes_to_string(id.last_segment()),
                    span: to_span(id.span()),
                });
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
        scan_effect_origins(&child, out);
    }
}

/// Lower a type hint to a simple scalar type, or `None` for anything the slice
/// does not model (unions, intersections, class types, `array`, `mixed`, …).
fn lower_hint(hint: &Hint<'_>) -> Option<ParamType> {
    match hint {
        Hint::Integer(_) => Some(ParamType { scalar: ScalarType::Int, nullable: false }),
        Hint::Float(_) => Some(ParamType { scalar: ScalarType::Float, nullable: false }),
        Hint::String(_) => Some(ParamType { scalar: ScalarType::String, nullable: false }),
        Hint::Bool(_) => Some(ParamType { scalar: ScalarType::Bool, nullable: false }),
        Hint::Nullable(n) => {
            // `?int` etc. — a nullable wrapper over a bare scalar. Anything more
            // complex inside the `?` is not a simple scalar and is skipped.
            lower_hint(n.hint).map(|inner| ParamType { scalar: inner.scalar, nullable: true })
        }
        _ => None,
    }
}

fn lower_call(c: &FunctionCall<'_>) -> CallExpr {
    let callee = match c.function {
        Expression::Identifier(id) => Some(bytes_to_string(id.last_segment())),
        _ => None,
    };
    let receiver = callee.clone().map_or(Callee::Dynamic, Callee::Function);

    let (args, positional_only) = lower_argument_list(&c.argument_list);
    CallExpr { callee, receiver, args, positional_only, span: to_span(c.span()) }
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

/// The simple class name of an instantiation's class expression, if statically
/// named (`new Foo(...)`). Dynamic (`new $c()`) yields `None`.
fn instantiation_class(inst: &Instantiation<'_>) -> Option<String> {
    match inst.class {
        Expression::Identifier(id) => Some(bytes_to_string(id.last_segment())),
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

/// The trace [`StaticClass`] of a static-call class expression.
fn trace_static_class(class: &Expression<'_>) -> Option<StaticClass> {
    match class {
        Expression::Identifier(id) => Some(StaticClass::Named(bytes_to_string(id.last_segment()))),
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
        Expression::Identifier(id) => Some(EffectRecv::ClassName(bytes_to_string(id.last_segment()))),
        Expression::Self_(_) => Some(EffectRecv::SelfKw),
        Expression::Parent(_) => Some(EffectRecv::Parent),
        _ => None,
    }
}

/// Lower a method call (`MethodCall` / `NullSafeMethodCall`) into a [`CallExpr`].
fn lower_method_call(object: &Expression<'_>, selector: &ClassLikeMemberSelector<'_>, list: &mago_syntax::cst::ArgumentList<'_>, span: Span) -> CallExpr {
    let receiver = match (trace_recv_of_object(object), method_name_of(selector)) {
        (Some(recv), Some(method)) => Callee::Method { receiver: recv, method },
        _ => Callee::Dynamic,
    };
    let (args, positional_only) = lower_argument_list(list);
    CallExpr { callee: None, receiver, args, positional_only, span }
}

/// Lower a static method call into a [`CallExpr`].
fn lower_static_call(class: &Expression<'_>, selector: &ClassLikeMemberSelector<'_>, list: &mago_syntax::cst::ArgumentList<'_>, span: Span) -> CallExpr {
    let receiver = match (trace_static_class(class), method_name_of(selector)) {
        (Some(class), Some(method)) => Callee::Static { class, method },
        _ => Callee::Dynamic,
    };
    let (args, positional_only) = lower_argument_list(list);
    CallExpr { callee: None, receiver, args, positional_only, span }
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
        _ => ArgValue::Other,
    }
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
fn lower_scopes(program: &Program<'_>) -> Vec<Scope> {
    let mut scopes = vec![build_scope(ScopeOwner::TopLevel, program.statements.as_slice())];
    collect_scopes(&Node::Program(program), &mut scopes);
    scopes
}

/// Recursively find `function` declarations (→ function scopes) and `class`
/// declarations (→ one scope per concrete method), building a scope for each.
fn collect_scopes(node: &Node<'_, '_>, out: &mut Vec<Scope>) {
    match node {
        Node::Function(f) => {
            let name = bytes_to_string(f.name.value);
            out.push(build_scope(ScopeOwner::Function(name), f.body.statements.as_slice()));
        }
        Node::Class(c) => {
            let class = bytes_to_string(c.name.value);
            for member in c.members.iter() {
                if let ClassLikeMember::Method(m) = member
                    && let MethodBody::Concrete(block) = &m.body
                {
                    let method = bytes_to_string(m.name.value);
                    let owner = ScopeOwner::Method { class: class.clone(), method };
                    out.push(build_scope(owner, block.statements.as_slice()));
                }
            }
        }
        _ => {}
    }
    // Recurse so nested functions (inside methods or blocks) and nested classes
    // also get their scopes. Method scopes are only created above (matching
    // `Node::Class`), so this recursion never double-creates one.
    for child in node.children() {
        collect_scopes(&child, out);
    }
}

/// Lower one scope's statements to a linear trace, and compute its poison flag.
fn build_scope(owner: ScopeOwner, statements: &[Statement<'_>]) -> Scope {
    let poisoned = statements.iter().any(|s| node_poisons(&Node::Statement(s)));
    let mut stmts = Vec::new();
    for s in statements {
        lower_stmt(s, &mut stmts);
    }
    let function_name = match &owner {
        ScopeOwner::Function(name) => Some(name.clone()),
        ScopeOwner::TopLevel | ScopeOwner::Method { .. } => None,
    };
    Scope { function_name, owner, poisoned, stmts }
}

/// Append the lowered [`Stmt`] for one source statement (or nothing, for benign
/// statements that neither define values nor disturb them).
fn lower_stmt(s: &Statement<'_>, out: &mut Vec<Stmt>) {
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
            if let Some(e) = r.value {
                collect_call_vars(&Node::Expression(e), &mut invalidated);
                // `return f($s);` — carry the call so propagation/descent reach it.
                call = named_call(e);
            }
            Stmt { kind: StmtKind::Return { value, call }, invalidated }
        }
        // `echo e1, e2, …;` — collect the statically-named calls among the
        // operands so propagation/descent check them; env stays conservative.
        Statement::Echo(e) => {
            let mut calls = Vec::new();
            let mut invalidated = Vec::new();
            for v in e.values.iter() {
                collect_call_vars(&Node::Expression(v), &mut invalidated);
                if let Some(c) = named_call(v) {
                    calls.push(c);
                }
            }
            Stmt { kind: StmtKind::Echo(calls), invalidated }
        }
        // Control-flow constructs: lowered to `Opaque` (ADR-0027 ratchet) — the
        // walk forgets only their write set, not the whole env.
        Statement::If(_)
        | Statement::While(_)
        | Statement::For(_)
        | Statement::Foreach(_)
        | Statement::DoWhile(_)
        | Statement::Switch(_)
        | Statement::Try(_)
        | Statement::Block(_) => lower_opaque(s),
        // Everything else (declarations, `goto`, labels, `declare`, unset,
        // `__halt_compiler`, …) stays a full Barrier: the sound floor for
        // anything whose write set the lowering cannot bound.
        _ => Stmt { kind: StmtKind::Barrier, invalidated: Vec::new() },
    };
    out.push(stmt);
}

/// The full [`CallExpr`] when `expr` (unparenthesized) is a resolvable call —
/// a statically-named function, an instance/static method call, or a `new`
/// construction — else `None` (dynamic receivers carry nothing the checker can
/// resolve, so they are dropped rather than tracked).
fn named_call(expr: &Expression<'_>) -> Option<CallExpr> {
    match expr.unparenthesized() {
        Expression::Call(Call::Function(fc)) => {
            let call = lower_call(fc);
            call.callee.is_some().then_some(call)
        }
        Expression::Call(Call::Method(mc)) => {
            let call = lower_method_call(mc.object, &mc.method, &mc.argument_list, to_span(mc.span()));
            (call.receiver != Callee::Dynamic).then_some(call)
        }
        Expression::Call(Call::NullSafeMethod(mc)) => {
            let call = lower_method_call(mc.object, &mc.method, &mc.argument_list, to_span(mc.span()));
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

/// Lower a recognized control-flow construct to [`StmtKind::Opaque`]: compute
/// its poison flag and its over-approximated write set (see the variant docs).
fn lower_opaque(s: &Statement<'_>) -> Stmt {
    let node = Node::Statement(s);
    let poisons = node_poisons(&node);
    let mut writes = Vec::new();
    // By-ref conservatism: every variable handed to any call in the subtree.
    collect_call_vars(&node, &mut writes);
    // Assignment / increment / foreach-binding / catch-param write targets.
    collect_assign_writes(&node, &mut writes);
    Stmt { kind: StmtKind::Opaque { writes, poisons }, invalidated: Vec::new() }
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
                    kind: StmtKind::Assign { var, value, span: to_span(a.lhs.span()), call },
                    invalidated,
                }
            } else {
                // Assignment to a non-simple lvalue (`$a[i] = …`, `$o->p = …`).
                Stmt { kind: StmtKind::Barrier, invalidated: Vec::new() }
            }
        }
        Expression::Call(Call::Function(fc)) => {
            let mut invalidated = Vec::new();
            collect_call_vars(&Node::Expression(expr), &mut invalidated);
            Stmt { kind: StmtKind::Call(lower_call(fc)), invalidated }
        }
        // Statement-level method / static / constructor calls. A resolvable
        // receiver becomes a `Call`; a dynamic one is a `Barrier` (but its
        // call-var invalidation is still collected below via the fallthrough).
        Expression::Call(Call::Method(_) | Call::NullSafeMethod(_) | Call::StaticMethod(_))
        | Expression::Instantiation(_) => match named_call(expr) {
            Some(call) => {
                let mut invalidated = Vec::new();
                collect_call_vars(&Node::Expression(expr), &mut invalidated);
                Stmt { kind: StmtKind::Call(call), invalidated }
            }
            None => {
                let mut invalidated = Vec::new();
                collect_call_vars(&Node::Expression(expr), &mut invalidated);
                Stmt { kind: StmtKind::Barrier, invalidated }
            }
        },
        // A statement-level `match` is a control-flow construct: lower to
        // `Opaque` over its subtree, like the block-form constructs above.
        Expression::Match(_) => {
            let node = Node::Expression(expr);
            let poisons = node_poisons(&node);
            let mut writes = Vec::new();
            collect_call_vars(&node, &mut writes);
            collect_assign_writes(&node, &mut writes);
            Stmt { kind: StmtKind::Opaque { writes, poisons }, invalidated: Vec::new() }
        }
        _ => Stmt { kind: StmtKind::Barrier, invalidated: Vec::new() },
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
// Small helpers.
// ---------------------------------------------------------------------------

fn to_span(span: mago_span::Span) -> Span {
    Span { start: span.start.offset, end: span.end.offset }
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
