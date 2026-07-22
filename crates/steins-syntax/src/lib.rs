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
use mago_syntax::cst::Call;
use mago_syntax::cst::DeclareItem;
use mago_syntax::cst::Expression;
use mago_syntax::cst::Function;
use mago_syntax::cst::FunctionCall;
use mago_syntax::cst::Hint;
use mago_syntax::cst::Literal;
use mago_syntax::cst::Node;
use mago_syntax::cst::Program;
use mago_syntax::cst::Statement;
use mago_syntax::cst::UnaryPrefixOperator;
use mago_syntax::cst::Variable;

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

/// A user-defined function declaration (top-level or namespaced). `name` is the
/// simple (unqualified) name as written at the declaration site.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FunctionDecl {
    pub name: String,
    pub params: Vec<Param>,
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
            ArgValue::Call(name, args) => {
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

/// A function-call expression.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CallExpr {
    /// The simple callee name, if the callee is a statically-known identifier;
    /// `None` for dynamic calls (`$f()`, method calls, …).
    pub callee: Option<String>,
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

/// One analysis scope: the top-level script, or a single function body. Carries
/// the linear trace and a whole-scope `poisoned` flag (ADR-0001 give-up list).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Scope {
    /// `None` for the top-level script; `Some(name)` for a function body.
    pub function_name: Option<String>,
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

        let mut lowered = Lowered::default();
        walk(&Node::Program(program), &mut lowered);

        let scopes = lower_scopes(program);

        let parse_errors = program
            .errors
            .iter()
            .map(|e| ParseError { message: e.to_string(), span: to_span(e.span()) })
            .collect();

        Self {
            strict_types: lowered.strict_types,
            functions: lowered.functions,
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

fn walk(node: &Node<'_, '_>, out: &mut Lowered) {
    match node {
        Node::Function(f) => out.functions.push(lower_function(f)),
        Node::FunctionCall(c) => out.calls.push(lower_call(c)),
        Node::DeclareItem(d) if is_strict_types_one(d) => out.strict_types = true,
        _ => {}
    }
    for child in node.children() {
        walk(&child, out);
    }
}

fn lower_function(f: &Function<'_>) -> FunctionDecl {
    let params = f
        .parameter_list
        .parameters
        .iter()
        .map(|p| Param {
            name: strip_dollar(bytes_to_string(p.variable.name)),
            ty: p.hint.as_ref().and_then(lower_hint),
            variadic: p.is_variadic(),
            by_ref: p.is_reference(),
            span: to_span(p.span()),
        })
        .collect();

    FunctionDecl { name: bytes_to_string(f.name.value), params, span: to_span(f.name.span()) }
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

    let mut positional_only = true;
    let mut args = Vec::new();
    for arg in c.argument_list.arguments.iter() {
        match arg {
            Argument::Positional(p) if p.ellipsis.is_none() => {
                args.push(Arg { value: lower_arg_value(p.value), span: to_span(p.value.span()) });
            }
            // Named or spread argument: positional mapping is unreliable.
            _ => positional_only = false,
        }
    }

    CallExpr { callee, args, positional_only, span: to_span(c.span()) }
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
/// function declaration found anywhere in the file (matching the flat function
/// index, so nested functions get scopes too).
fn lower_scopes(program: &Program<'_>) -> Vec<Scope> {
    let mut scopes = vec![build_scope(None, program.statements.as_slice())];
    collect_function_scopes(&Node::Program(program), &mut scopes);
    scopes
}

/// Recursively find `function` declarations and build a scope for each body.
fn collect_function_scopes(node: &Node<'_, '_>, out: &mut Vec<Scope>) {
    if let Node::Function(f) = node {
        let name = bytes_to_string(f.name.value);
        out.push(build_scope(Some(name), f.body.statements.as_slice()));
    }
    for child in node.children() {
        collect_function_scopes(&child, out);
    }
}

/// Lower one scope's statements to a linear trace, and compute its poison flag.
fn build_scope(name: Option<String>, statements: &[Statement<'_>]) -> Scope {
    let poisoned = statements.iter().any(|s| node_poisons(&Node::Statement(s)));
    let mut stmts = Vec::new();
    for s in statements {
        lower_stmt(s, &mut stmts);
    }
    Scope { function_name: name, poisoned, stmts }
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

/// The full [`CallExpr`] when `expr` (unparenthesized) is a call to a
/// statically-named function, else `None` (dynamic calls carry no callee name
/// the checker can resolve, so they are dropped rather than tracked).
fn named_call(expr: &Expression<'_>) -> Option<CallExpr> {
    if let Expression::Call(Call::Function(fc)) = expr.unparenthesized() {
        let call = lower_call(fc);
        return call.callee.is_some().then_some(call);
    }
    None
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
