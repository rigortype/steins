//! Scope / linear-trace lowering (ADR-0001 value propagation): one [`Scope`] per
//! function-like body with its statement trace, closure captures (ADR-0033), and
//! the local-variable usage scan behind `variable.undefined` (ADR-0078).

use std::collections::HashMap;
use std::collections::HashSet;

use mago_span::HasSpan;
use mago_syntax::cst::{
    Access, Argument, ArrayElement, ClassLikeMember, Construct, Expression, ExpressionStatement,
    Hint, MethodBody, Node, PartialArgument, Program, Statement, UnaryPrefixOperator, Variable,
};

use crate::ast::{
    BodyEnd, NsCtx, RetHint, RetHintKind, SUPERGLOBALS, Scope, ScopeOwner, Stmt, StmtKind,
    UndefinedRead, UnusedCapture,
};
use crate::lower_decl::{DocIndex, lower_hint, lower_params};
use crate::lower_effect::{
    EffectScanCx, ReceiverWrites, body_aliased, collect_body_callables, scan_effect_origins,
    scan_method_calls, scan_throw_origins,
};
use crate::lower_expr::lower_arg_value;
use crate::lower_presence::maybe_undefined_reads;
use crate::stack_guard;
use crate::{
    RefResolver, bytes_to_string, call_invalidation, children, ctx_of, lower_stmt, named_call,
    node_poisons, push_byref_captures, scan_guard_chain_no_default, scan_opaque,
    scan_string_contexts, strip_dollar, to_span,
};

// ---------------------------------------------------------------------------
// Scope / linear-trace lowering (ADR-0001 value propagation).
// ---------------------------------------------------------------------------

/// Build every analysis scope: the top-level script first, then one per
/// function declaration and one per concrete method body found anywhere in the
/// file (nested functions and class methods alike get scopes).
pub(crate) fn lower_scopes(
    program: &Program<'_>,
    contexts: &[NsCtx],
    regions: &[(u32, u32, usize)],
    docs: &DocIndex<'_>,
) -> Vec<Scope> {
    // The script (top-level) scope spans all namespace bodies too: file-scoped
    // `namespace A;` nests following statements inside the namespace node, so
    // flatten those back out so namespaced top-level code is analyzed.
    // Function/class declarations still get their own scopes below.
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
pub(crate) fn flatten_top_level<'a, 'arena>(
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
/// closure, carried down unchanged elsewhere (its def-offset gate means only that
/// one closure can pick it up).
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
        // Statement-level docblock adoption (issue #128): a simple assignment whose
        // RHS is exactly a closure/arrow expression hands the statement's docblock
        // down to that closure's scope (`/** @return string */\n$f = function () {…};`).
        // The def-offset gate keeps every other closure position (a call argument, a
        // nested closure) statement-silent — inline adjacency is their only route.
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

/// The statement-level docblock adoption context of `collect_scopes` (issue #128):
/// the docblock preceding a simple-assignment statement, addressed to the closure
/// that is the statement's whole RHS — the trace-IR shape whose `Assign` value is
/// `ArgValue::Closure`, re-read here on the CST because scopes are built before
/// any trace consumer runs.
struct StmtAdoption {
    /// Definition offset of the closure/arrow that is the statement's whole RHS
    /// — the gate that keeps the docblock from drifting to any other closure.
    def_offset: u32,
    /// The enclosing statement's docblock text.
    doc: String,
}

/// Recognize `/** … */\n$f = <closure>;` — a docblock-led statement that is a
/// plain `=` assignment to a direct variable whose RHS is exactly a closure or
/// arrow expression. Any other shape — a closure in a call argument, a compound
/// op, a non-variable lvalue — adopts nothing at statement level.
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
/// whitespace-gap discipline (ADR-0029, the same grammar
/// `SourceTree::stmt_docblock` gives the inline-`@var` lane), in precedence order:
///
/// 1. **Inline** — the docblock immediately preceding the closure's own first
///    token (`$f = /** @return string */ function () {…}`).
/// 2. **Statement-level** — the enclosing statement's docblock, handed down by
///    `collect_scopes` only when that statement is a simple assignment whose
///    whole RHS is this closure (the [`StmtAdoption`] def-offset gate).
///
/// Both positions read one grammar (`DocIndex::preceding`): a blank line still
/// adopts, but an intervening non-doc comment or code breaks adjacency.
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
/// `params` is the scope's own parameter list, or `None` for the top-level script —
/// both "no parameters" and the reason that scope reports no undefined reads at
/// all (see [`Scope::undefined_reads`]).
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
    let mut guard_chain_no_default = Vec::new();
    for s in statements {
        lower_stmt(s, &mut stmts);
        scan_method_calls(&Node::Statement(s), &mut method_calls);
        scan_opaque(&Node::Statement(s), &mut opaque, false);
        scan_guard_chain_no_default(&Node::Statement(s), &mut guard_chain_no_default);
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
        guard_chain_no_default,
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
        // `mixed` cannot appear inside a union or behind `?` (PHP forbids both), so the
        // bare — possibly parenthesized — spelling is the only one there is.
        Hint::Mixed(_) => RetHintKind::Mixed,
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
pub(crate) fn closure_def_offset(cl: &mago_syntax::cst::Closure<'_>) -> u32 {
    to_span(cl.function.span()).start
}

/// The definition-site byte offset of an arrow function — the `fn` keyword.
pub(crate) fn arrow_def_offset(af: &mago_syntax::cst::ArrowFunction<'_>) -> u32 {
    to_span(af.r#fn.span()).start
}

/// The by-value captured names of a closure's `use (...)` clause (by-ref `&$x`
/// captures are excluded — they poison instead, ADR-0033/0001).
pub(crate) fn closure_use_captures(cl: &mago_syntax::cst::Closure<'_>) -> Vec<String> {
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
    let mut guard_chain_no_default = Vec::new();
    // The closure's own scope is poisoned by a by-ref `use (&$x)` capture (its
    // captured var is a reference alias) or any in-body poison marker — it defeats
    // frame-locality for the whole body just as an in-body `global` would.
    let mut opaque = Vec::new();
    push_byref_captures(cl, &mut opaque, false);
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
        scan_guard_chain_no_default(&Node::Statement(s), &mut guard_chain_no_default);
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
        guard_chain_no_default,
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
/// the CST the lowered trace deliberately forgets. The walk is the **deep** one:
/// it descends nested closures, arrow functions and their `use (…)` clauses, so
/// `use ($x) { return fn () => $x; }` counts `$x` as mentioned. A body that can
/// mint or consume names without spelling them dams the whole list.
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
/// `extract`/`compact`/`get_defined_vars`). Unlike [`collect_var_reads`] this walk
/// **descends every nested construct**, including closures and arrow functions and
/// their `use (…)` clauses: a name mentioned by an inner scope is a use of the
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
/// which lets the walk be ordering-blind and duplication-tolerant: a name that is
/// both bound and read (`$x = 1; echo $x;`) filters out no matter which the walk
/// saw first, so binding forms need no read-suppression machinery. Only the
/// positions that bind *nothing* — the `isset`/`empty`/`??`/`unset`/`@` guards —
/// need the walk to actually withhold a read.
#[derive(Default)]
pub(crate) struct VarUsage {
    pub(crate) bound: std::collections::HashSet<String>,
    pub(crate) reads: Vec<UndefinedRead>,
    /// See [`Scope::ref_arg_candidates`] — collected in the same walk but on its own
    /// terms, because a binding form must not depend on a read being recorded.
    pub(crate) arg_candidates: Vec<UndefinedRead>,
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
/// its arms. This is the `??` discharge idiom in conditional spelling:
/// `empty($page) ? 0 : ($page - 1) * $view` reaches the `$page` read only when
/// `$page` is non-empty, hence bound, so this id's runtime claim — "PHP warns and
/// the read evaluates to null" — is simply false there.
///
/// **Not reachability, and deliberately not.** The rule asks only what the
/// condition spells, then withholds reads in *both* arms without deciding which arm
/// the guard protects — costing a finding on a "wrong" polarity but never
/// manufacturing one, letting a purely syntactic containment test stand in for a
/// flow analysis Steins does not have (the `variable.maybe-undefined` foundation,
/// issue #199). `!` and parentheses are transparent; a conjunction
/// (`isset($x) && $y`) tests nothing here, matching the corpus's shapes.
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

/// Bind the **root local** of an lvalue, and nothing else. `$x = …` binds `x`; so
/// does `$x['k'] = …` (witnessed: the offset write auto-vivifies `$x` with no
/// warning at 8.5.9) and `$x->p = …`. The *index* of an offset write is an
/// ordinary read, left to the main walk. Destructuring recurses into every
/// element, so `[$a, [$b]] = …` and `list(, $b) = …` bind exactly the names they
/// write. A non-lvalue shape binds nothing — this is called on argument positions
/// too, where `f($a + $b)` must not pretend to bind.
pub(crate) fn bind_lvalue_roots(expr: &Expression<'_>, acc: &mut VarUsage) {
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
pub(crate) fn scan_var_usage(node: &Node<'_, '_>, guarded: bool, shielded: &[String], acc: &mut VarUsage) {
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
/// deliberately forgets. `params` seeds the binding set with the scope's own
/// parameters (promoted constructor properties included), and `use_clause` with a
/// closure's captures, by value and by reference alike. `None` for both is a
/// top-level or arrow scope, which reports nothing at all — see
/// [`Scope::undefined_reads`] for why.
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
    // An arrow body lowers straight to a `return <expr>;` (below) rather than
    // through `value_position_matches`/`lower_stmt`, so no `StmtKind::If` this
    // scope's own `stmts` could ever carry actually traces back to a `match` here
    // — kept for the same reason every sibling scan runs uniformly across scope
    // kinds (`Scope::guard_chain_no_default`'s doc), not because it fires today.
    let mut guard_chain_no_default = Vec::new();
    scan_guard_chain_no_default(&Node::Expression(af.expression), &mut guard_chain_no_default);
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
        guard_chain_no_default,
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
pub(crate) fn arrow_free_vars(af: &mago_syntax::cst::ArrowFunction<'_>) -> Vec<String> {
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
