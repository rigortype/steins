//! Scope / linear-trace lowering (ADR-0001 value propagation): one [`Scope`] per
//! function-like body with its statement trace, closure captures (ADR-0033), and
//! the local-variable usage scan behind `variable.undefined` (ADR-0078).

use std::collections::HashMap;
use std::collections::HashSet;

use mago_span::HasSpan;
use mago_syntax::cst::{
    Access, Argument, ArrayElement, AssignmentOperator, ClassLikeMember, Construct, Expression, ExpressionStatement,
    Hint, MethodBody, Node, PartialArgument, Program, Property, PropertyHookBody,
    PropertyHookConcreteBody, Statement, UnaryPrefixOperator, Variable,
};

use crate::ast::{
    BodyEnd, HookKind, NsCtx, Param, RetHint, RetHintKind, ReturnForm, ReturnSite, Runs, SUPERGLOBALS,
    Scope, ScopeOwner, Stmt, StmtKind, UndefinedRead, UnusedCapture,
};
use crate::lower_arg_shape::Captures;
use crate::lower_decl::{DocIndex, lower_hint, lower_params};
use crate::lower_effect::{
    EffectScanCx, ReceiverWrites, body_aliased, collect_body_callables, scan_method_calls,
};
use crate::lower_expr::lower_arg_value;
use crate::lower_globals::{GlobalsKey, globals_fetch};
use crate::lower_guards::scan_guard_regions;
use crate::lower_read::read_positions_of_expr;
use crate::lower_presence::{
    InfoMap, Refinement, binary_info, elvis_info, maybe_undefined_reads, push_guard_root, span_key,
    subtree_has_goto,
};
use crate::lower_site::{
    arrow_return_site, body_end, param_default_sites, scan_owner_sites, scope_exit_sites,
};
use crate::lower_stmt::{
    block_end, call_invalidation, expr_end, lower_expr_stmt, lower_stmt, named_call, node_poisons,
    push_byref_captures, scan_guard_chain_no_default, scan_opaque, scan_string_contexts,
    subtree_has_function_exit, with_heap_fill,
};
use crate::names::{RefResolver, ctx_of};
use crate::stack_guard;
use crate::{bytes_to_string, children, strip_dollar, to_span};

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
            let ctx = ctx_of(contexts, regions, to_span(f.name.span()).start);
            // Case-preserved, as the class arm's: a lookup folds case against
            // `FunctionDecl::fqn`, and the namespace is what tells two same-named
            // functions of one file apart.
            let fqn = if ctx.namespace.is_empty() {
                name.clone()
            } else {
                format!("{}\\{}", ctx.namespace, name)
            };
            let mut scope = build_scope(
                ScopeOwner::Function { name, fqn },
                f.body.statements.as_slice(),
                ret_hint_of(f.return_type_hint.as_ref()),
                Some(&f.parameter_list),
            );
            scope.yields_by_ref = scope.is_generator && f.ampersand.is_some();
            out.push(scope);
        }
        // A trait's and an enum's methods get scopes as a class's do (ADR-0105). The
        // owner names the declaring class-like; for a trait, which class `$this`,
        // `self` and `static` name is the importing one, and the walk reads that
        // receiver as open (`steins_infer`'s scope receiver).
        Node::Class(c) => {
            let class_fqn = member_owner_fqn(contexts, regions, &c.name);
            collect_member_scopes(&class_fqn, c.members.iter(), false, rc, out);
        }
        Node::Trait(t) => {
            let class_fqn = member_owner_fqn(contexts, regions, &t.name);
            collect_member_scopes(&class_fqn, t.members.iter(), true, rc, out);
        }
        Node::Enum(e) => {
            let class_fqn = member_owner_fqn(contexts, regions, &e.name);
            collect_member_scopes(&class_fqn, e.members.iter(), false, rc, out);
        }
        Node::AnonymousClass(ac) => collect_anon_method_scopes(ac, rc, out),
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

/// The case-preserved FQN of a named class-like, as a method scope's owner spells it:
/// cross-file lookups fold case, but keeping the written case makes the owner readable
/// and stable for same-file code.
fn member_owner_fqn(
    contexts: &[NsCtx],
    regions: &[(u32, u32, usize)],
    name: &mago_syntax::cst::LocalIdentifier<'_>,
) -> String {
    let simple = bytes_to_string(name.value);
    let ctx = ctx_of(contexts, regions, to_span(name.span()).start);
    if ctx.namespace.is_empty() { simple } else { format!("{}\\{}", ctx.namespace, simple) }
}

/// One scope per concrete method of a class-like body, and one per property hook.
///
/// A trait's methods are [`ScopeOwner::TraitMethod`]s, and its property hooks get no
/// scope: a hook body runs with `$this` bound to the importing class, which a
/// [`ScopeOwner::PropertyHook`] would name as the trait.
fn collect_member_scopes<'a, 'arena: 'a>(
    class_fqn: &str,
    members: impl Iterator<Item = &'a ClassLikeMember<'arena>>,
    in_trait: bool,
    rc: &RefResolver,
    out: &mut Vec<Scope>,
) {
    for member in members {
        match member {
            ClassLikeMember::Method(m) => {
                if let MethodBody::Concrete(block) = &m.body {
                    let method = bytes_to_string(m.name.value);
                    let class = class_fqn.to_owned();
                    let owner = if in_trait {
                        ScopeOwner::TraitMethod { class, method }
                    } else {
                        ScopeOwner::Method { class, method }
                    };
                    let mut scope = build_scope(
                        owner,
                        block.statements.as_slice(),
                        ret_hint_of(m.return_type_hint.as_ref()),
                        Some(&m.parameter_list),
                    );
                    scope.yields_by_ref = scope.is_generator && m.ampersand.is_some();
                    out.push(scope);
                }
                // A promoted parameter's hooks are the constructor's syntax but the
                // property's semantics (issue #544) — same scopes as the class-body form
                // below, and reached only from here because the hook list hangs off the
                // parameter.
                for p in m.parameter_list.parameters.iter().filter(|_| !in_trait) {
                    if let Some(hooks) = &p.hooks {
                        collect_hook_scopes(
                            class_fqn,
                            &strip_dollar(bytes_to_string(p.variable.name)),
                            p.hint.as_ref(),
                            hooks,
                            rc,
                            out,
                        );
                    }
                }
            }
            ClassLikeMember::Property(Property::Hooked(h)) if !in_trait => {
                collect_hook_scopes(
                    class_fqn,
                    &strip_dollar(bytes_to_string(h.item.variable().name)),
                    h.hint.as_ref(),
                    &h.hook_list,
                    rc,
                    out,
                );
            }
            _ => {}
        }
    }
}

/// One [`ScopeOwner::AnonMethod`] scope per concrete method of an anonymous class
/// (ADR-0105), its parameters and native return type on the scope. Property hooks of
/// an anonymous class get no scope: a hook scope names its class.
fn collect_anon_method_scopes(
    ac: &mago_syntax::cst::AnonymousClass<'_>,
    rc: &RefResolver,
    out: &mut Vec<Scope>,
) {
    let def_offset = to_span(ac.span()).start;
    for member in ac.members.iter() {
        let ClassLikeMember::Method(m) = member else { continue };
        let MethodBody::Concrete(block) = &m.body else { continue };
        let owner = ScopeOwner::AnonMethod { def_offset, method: bytes_to_string(m.name.value) };
        let mut scope = build_scope(
            owner,
            block.statements.as_slice(),
            ret_hint_of(m.return_type_hint.as_ref()),
            Some(&m.parameter_list),
        );
        scope.yields_by_ref = scope.is_generator && m.ampersand.is_some();
        scope.params = lower_params(&m.parameter_list, rc);
        scope.ret_ty = m.return_type_hint.as_ref().and_then(|r| lower_hint(&r.hint, rc));
        out.push(scope);
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
    let mut returns = Vec::new();
    let mut guard_chain_no_default = Vec::new();
    for s in statements {
        lower_stmt(s, &mut stmts);
        if ret_hint.is_some() {
            scan_returns(&Node::Statement(s), &mut returns);
        }
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
        ScopeOwner::Function { fqn, .. } => Some(fqn.clone()),
        ScopeOwner::TopLevel
        | ScopeOwner::Method { .. }
        | ScopeOwner::TraitMethod { .. }
        | ScopeOwner::AnonMethod { .. }
        | ScopeOwner::Closure { .. }
        | ScopeOwner::PropertyHook { .. } => None,
    };
    let vars = undefined_variable_reads(params, None, statements);
    Scope {
        function_name,
        owner,
        ret_hint,
        is_generator,
        yields_by_ref: false,
        poisoned,
        opaque,
        stmts,
        returns,
        method_calls,
        params: Vec::new(),
        ret_ty: None,
        sites: Vec::new(),
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
        // `array`/`object`/`iterable`: unrepresentable in `NativeType`, enforced by PHP
        // at the boundary (issue #603). `enforced_top` recognizes exactly the bare
        // spellings, so `?array` keeps falling to `Other`.
        _ => match crate::lower_decl::enforced_top(hint) {
            Some(top) => RetHintKind::Top(top),
            None => RetHintKind::Other,
        },
    }
}

// property hook bodies (issue #544)

/// One scope per **concrete** hook body of a hooked property — plain or promoted,
/// which differ only in where the hook list hangs (issue #544).
///
/// `hint` is the *property's* declared type, and it is the whole reason this is not
/// simply another `build_scope` call: it is a `get` hook's return type and the type
/// of a `set` hook's implicit parameter, so the two facts the walk needs about a
/// hook body are facts about the property, not about the hook.
///
/// An abstract hook (`get;` on an interface or an `abstract` property) has no body
/// and gets no scope, exactly as an abstract method does.
fn collect_hook_scopes(
    class_fqn: &str,
    property: &str,
    hint: Option<&Hint<'_>>,
    hook_list: &mago_syntax::cst::PropertyHookList<'_>,
    rc: &RefResolver,
    out: &mut Vec<Scope>,
) {
    for hook in hook_list.hooks.iter() {
        let name = bytes_to_string(hook.name.value);
        let kind = if name.eq_ignore_ascii_case("get") {
            HookKind::Get
        } else if name.eq_ignore_ascii_case("set") {
            HookKind::Set
        } else {
            // PHP declares exactly two hooks; any other spelling is a parse error
            // upstream, and a recovered tree must not invent a scope for it.
            continue;
        };
        let PropertyHookBody::Concrete(body) = &hook.body else { continue };
        let owner = ScopeOwner::PropertyHook {
            class: class_fqn.to_owned(),
            property: property.to_owned(),
            hook: kind,
        };
        // A `get` hook returns the property's own type; a `set` hook returns nothing
        // (a `return <value>;` inside one is a PHP compile error, so there is no
        // return position for the walk to check).
        let (ret_hint, ret_ty) = match kind {
            HookKind::Get => (
                hint.map(|h| RetHint { kind: classify_ret_hint(h), span: to_span(h.span()) }),
                hint.and_then(|h| lower_hint(h, rc)),
            ),
            HookKind::Set => (None, None),
        };
        let mut scope = match body {
            PropertyHookConcreteBody::Block(block) => {
                let refs: Vec<&Statement<'_>> = block.statements.iter().collect();
                // The parameter list is handed on ONLY when the hook writes one:
                // `undefined_variable_reads` reads `None` as "this scope cannot
                // report at all", which is exactly right for the implicit-`$value`
                // form, whose binding has no parameter node to be read off.
                build_scope_from(owner, &refs, ret_hint, hook.parameter_list.as_ref())
            }
            PropertyHookConcreteBody::Expression(e) => {
                build_hook_expr_scope(owner, kind, e.expression, ret_hint)
            }
        };
        scope.params = hook_params(kind, hook, hint, rc);
        scope.ret_ty = ret_ty;
        out.push(scope);
    }
}

/// A hook's parameters as the engine sees them.
///
/// A written list is lowered as any other. An omitted one means no parameters for
/// `get`, and for `set` the engine's implicit `$value`, typed as the property —
/// witnessed on 8.5.9 through `ReflectionProperty::getHooks()` (`set` reports
/// `value:int` for `public int $v { set { … } }`) and through the `TypeError` a bad
/// assignment raises (`A::$v::set(): Argument #1 ($value) must be of type int`).
fn hook_params(
    kind: HookKind,
    hook: &mago_syntax::cst::PropertyHook<'_>,
    hint: Option<&Hint<'_>>,
    rc: &RefResolver,
) -> Vec<Param> {
    if let Some(list) = &hook.parameter_list {
        return lower_params(list, rc);
    }
    if kind == HookKind::Get {
        return Vec::new();
    }
    vec![Param {
        name: "value".to_owned(),
        ty: hint.and_then(|h| lower_hint(h, rc)),
        top: hint.and_then(crate::lower_decl::param_top),
        hint_span: hint.map(|h| to_span(h.span())),
        variadic: false,
        by_ref: false,
        has_null_default: false,
        has_default: false,
        default: None,
        // The parameter is unwritten, so the hook's own name is the nearest thing
        // to a declaration site a diagnostic can point at.
        span: to_span(hook.name.span()),
    }]
}

/// The scope of an arrow-bodied hook (`get => <expr>;` / `set => <expr>;`).
///
/// The two arrows are not the same construct. `get => e` is a `return e`, so it
/// lowers to a return trace exactly as an arrow function's body does. `set => e`
/// **assigns** `e` to the backing property (witnessed 8.5.9: `public int $n { set
/// => "nope"; }` raises `Cannot assign string to property G::$n of type int`), so
/// its expression is in statement position, and the assignment itself stays
/// unmodelled — the same silence a written `$this->prop = …` inside a hook body
/// already gets, hooked properties carrying no value fact at all (FP class 16).
fn build_hook_expr_scope(
    owner: ScopeOwner,
    kind: HookKind,
    expr: &Expression<'_>,
    ret_hint: Option<RetHint>,
) -> Scope {
    let node = Node::Expression(expr);
    let mut method_calls = Vec::new();
    scan_method_calls(&node, &mut method_calls);
    let mut opaque = Vec::new();
    scan_opaque(&node, &mut opaque, false);
    let mut guard_chain_no_default = Vec::new();
    scan_guard_chain_no_default(&node, &mut guard_chain_no_default);
    let mut string_contexts = Vec::new();
    scan_string_contexts(&node, &mut string_contexts);
    let mut guards = Vec::new();
    scan_guard_regions(&node, &mut guards);
    let stmt = match kind {
        HookKind::Get => {
            let kind = StmtKind::Return {
                value: lower_arg_value(expr),
                call: named_call(expr),
                span: to_span(expr.span()),
            };
            let reads = read_positions_of_expr(expr, &kind);
            let stmt = Stmt {
                span: to_span(expr.span()),
                kind,
                invalidated: call_invalidation(&node),
                string_contexts,
                guards,
                // The body IS the return, so the trace always terminates — which is
                // why an arrow-bodied `get` can never be a `type.return-missing` site.
                end: BodyEnd::Terminates,
                has_terminator: true,
                value_position: false,
                // A hook body is never the top-level frame (issue #762).
                runs: Runs::default(),
                holds_label: false,
                unspelled: None,
                suspends: false,
                writes_through: None,
                read_positions: reads,
            };
            with_heap_fill(stmt, &node, &[expr])
        }
        HookKind::Set => {
            let base = lower_expr_stmt(expr);
            let stmt = Stmt {
                span: to_span(expr.span()),
                string_contexts,
                guards,
                end: expr_end(expr),
                has_terminator: subtree_has_function_exit(&node),
                read_positions: read_positions_of_expr(expr, &base.kind),
                ..base
            };
            with_heap_fill(stmt, &node, &[expr])
        }
    };
    Scope {
        function_name: None,
        owner,
        ret_hint,
        is_generator: node_is_generator(&node),
        yields_by_ref: false,
        // The flag IS the inventory being non-empty (never a second computation).
        poisoned: !opaque.is_empty(),
        opaque,
        stmts: vec![stmt],
        returns: Vec::new(),
        method_calls,
        // Filled by the caller, which is the only place the property's type is known.
        params: Vec::new(),
        ret_ty: None,
        sites: Vec::new(),
        guard_chain_no_default,
        is_static: false,
        docblock: None,
        unused_captures: Vec::new(),
        // An arrow body has no written parameter list to close the world over — the
        // same reason the block form withholds these for an implicit `$value`.
        undefined_reads: Vec::new(),
        maybe_undefined_reads: Vec::new(),
        ref_arg_candidates: Vec::new(),
    }
}

// end property hook bodies (issue #544)

/// Whether the subtree contains a `yield` / `yield from` that makes this scope a
/// generator. Nested function/method/closure bodies are their own scopes and are
/// not counted.
pub(crate) fn node_is_generator(node: &Node<'_, '_>) -> bool {
    match node {
        Node::Yield(_) | Node::YieldFrom(_) | Node::YieldPair(_) | Node::YieldValue(_) => true,
        _ if opens_function_like(node) => false,
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

/// Whether `node` opens a function-like body PHP compiles separately, so a `return` or
/// `yield` inside it belongs to that body's own scope and never to the enclosing one.
/// The one boundary `scan_returns` and `node_is_generator` share.
///
/// Boundaries (the parser's complete list of such nodes): `Function`, `Method`,
/// `Closure`, `ArrowFunction`, and `PropertyHook` (a `get`/`set` hook in block or short
/// `=>` form, on a plain, promoted or anonymous-class property: the hook node holds the
/// whole body, and a promoted-property hook sits inside its constructor's `Method`).
///
/// Not boundaries, so a walk descends them and meets any boundary inside: class,
/// interface, trait, enum and anonymous-class bodies (they hold members, never a
/// statement-level `return`), `HookedProperty` and `PropertyHookList` (the list around
/// the hooks), attribute arguments, class and global constant expressions, enum case
/// values, and parameter default values (all expressions, where a closure or an
/// anonymous class may appear and is then its own boundary).
pub(crate) fn opens_function_like(node: &Node<'_, '_>) -> bool {
    matches!(
        node,
        Node::Function(_)
            | Node::Method(_)
            | Node::Closure(_)
            | Node::ArrowFunction(_)
            | Node::PropertyHook(_)
    )
}

/// Every `return` statement of `node`'s subtree, in source order, into `out`. Nested
/// function / method / closure / arrow bodies are their own scopes and are not descended.
fn scan_returns(node: &Node<'_, '_>, out: &mut Vec<ReturnSite>) {
    match node {
        Node::Return(r) => {
            let form = if r.value.is_some() { ReturnForm::Valued } else { ReturnForm::Bare };
            out.push(ReturnSite { form, span: to_span(r.span()) });
        }
        _ if opens_function_like(node) => {}
        _ => {
            if stack_guard::exhausted() {
                return;
            }
            for child in children(node) {
                scan_returns(&child, out);
            }
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
    let mut sites = Vec::new();
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
    let uses: Vec<String> = cl
        .use_clause
        .iter()
        .flat_map(|u| u.variables.iter())
        .map(|v| strip_dollar(bytes_to_string(v.variable.name)))
        .collect();
    let cx = EffectScanCx::new(
        &cl.parameter_list,
        collect_body_callables(cl.body.statements.iter()),
        !opaque.is_empty() || body_aliased(cl.body.statements.iter()),
        ReceiverWrites::poisoned(),
    )
    .returning(cl.return_type_hint.as_ref().map(|r| &r.hint))
    .generator(cl.body.statements.iter().any(|s| node_is_generator(&Node::Statement(s))))
    .with_body(
        &cl.parameter_list,
        Captures::Uses(&uses),
        cl.body.statements.iter().map(Node::Statement),
    );
    param_default_sites(&cl.parameter_list, &cx, &mut sites);
    let mut is_generator = false;
    let mut returns = Vec::new();
    for s in cl.body.statements.iter() {
        lower_stmt(s, &mut stmts);
        if cl.return_type_hint.is_some() {
            scan_returns(&Node::Statement(s), &mut returns);
        }
        scan_owner_sites(&Node::Statement(s), &cx, &mut sites);
        scan_method_calls(&Node::Statement(s), &mut method_calls);
        scan_opaque(&Node::Statement(s), &mut opaque, false);
        scan_guard_chain_no_default(&Node::Statement(s), &mut guard_chain_no_default);
        if !is_generator {
            is_generator = node_is_generator(&Node::Statement(s));
        }
    }
    scope_exit_sites(&cx, body_end(cl.body.span()), &mut sites);
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
        yields_by_ref: is_generator && cl.ampersand.is_some(),
        poisoned,
        opaque,
        stmts,
        returns,
        method_calls,
        params: lower_params(&cl.parameter_list, rc),
        ret_ty: cl.return_type_hint.as_ref().and_then(|r| lower_hint(&r.hint, rc)),
        sites,
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
    /// The subset of [`Self::bound`] that every normal completion of the scanned
    /// subtree binds: a name bound only under the right operand of a short-circuit
    /// operator, in one arm of a ternary or `match`, or by a `??=` right-hand side is in
    /// `bound` and not here. Maintained through [`Self::branch`]; the presence pass reads
    /// the difference as `Maybe` (issue #948).
    pub(crate) uncond_bound: std::collections::HashSet<String>,
    /// The refinement of every short-circuit operator the scan has left, by span: each is
    /// computed once, from its operands' unconditional bindings and their own entries
    /// here, so the cost of the whole is linear in the expression.
    pub(crate) infos: InfoMap,
    pub(crate) reads: Vec<UndefinedRead>,
    /// See [`Scope::ref_arg_candidates`] — collected in the same walk but on its own
    /// terms, because a binding form must not depend on a read being recorded.
    pub(crate) arg_candidates: Vec<UndefinedRead>,
    /// Reads a **disjunctive** or dead-arm shield covers: each is discharged only when
    /// every name in one of its [`Shield::joint`] sets is never bound, so [`Self::settle`]
    /// decides them once the whole scope's bindings are known. A read can sit under
    /// several.
    conditional: Vec<(UndefinedRead, Vec<Vec<String>>)>,
    /// The root local of **every** function-call argument, bare or reached through an
    /// offset or property. `bound` leaves these out (a by-value read of `$y` in `f($y)`
    /// binds nothing), but a callee may take the slot by reference and bind it
    /// (`preg_match($p, $s, $y)`, `parse_str($q, $y['k'])`), so [`Self::settle`] counts
    /// them as possibly bound.
    call_arg_roots: std::collections::HashSet<String>,
    /// The scope holds a `goto` or a label, which lands control after a guard, or inside
    /// one of its arms, without evaluating the condition: the rules that read "only this
    /// outcome reaches here" ([`terminating_guard_names`], the sided arms of an `if`)
    /// stand down. The right-operand rule needs no such guard — see `scan_var_usage`.
    goto_present: bool,
    /// The arms of an `if` or `?:` also take the dead-arm shields of
    /// [`with_dead_arm`]: set by the definite pass only, whose [`Self::settle`] sees
    /// the whole scope's bindings. The presence pass settles one unit at a time, where
    /// "never bound" would mean only "not bound in this unit".
    dead_arms: bool,
    dammed: bool,
}

impl VarUsage {
    /// Record `$name` as bound. An indirect/nested spelling cannot be named, so it
    /// dams instead (`$$n = 1` mints a binding this pass cannot see).
    fn bind_variable(&mut self, var: &mago_syntax::cst::Variable<'_>) {
        match var {
            mago_syntax::cst::Variable::Direct(dv) => {
                self.bind_direct(dv);
            }
            mago_syntax::cst::Variable::Indirect(_) | mago_syntax::cst::Variable::Nested(_) => {
                self.dammed = true;
            }
        }
    }

    fn bind_direct(&mut self, dv: &mago_syntax::cst::DirectVariable<'_>) {
        let name = strip_dollar(bytes_to_string(dv.name));
        self.uncond_bound.insert(name.clone());
        self.bound.insert(name);
    }

    /// Run `scan` over a conditionally evaluated part and answer the names it binds on
    /// every path through it. Those names are in [`Self::bound`] either way and are not
    /// put in [`Self::uncond_bound`] here: the caller decides whether the part runs
    /// always (never, for a branch), or is joined with its sibling arms.
    fn branch(&mut self, scan: impl FnOnce(&mut Self)) -> std::collections::HashSet<String> {
        let outer = std::mem::take(&mut self.uncond_bound);
        scan(self);
        std::mem::replace(&mut self.uncond_bound, outer)
    }

    /// Record a short-circuit operator's refinement and add what it binds on both of its
    /// outcomes (the intersection of the two sides) to the unconditional set.
    fn finish_operator(&mut self, key: (u32, u32), refinement: Refinement) {
        self.uncond_bound.extend(refinement.0.intersection(&refinement.1).cloned());
        self.infos.insert(key, refinement);
    }

    /// Scan arms of which exactly one runs (a ternary's, a `match`'s): a name every
    /// completing arm binds is bound after the expression. `None` is an arm that never
    /// completes (a `throw` expression), the identity of the intersection.
    fn join_arms(&mut self, arms: Vec<Option<std::collections::HashSet<String>>>) {
        let mut joined: Option<std::collections::HashSet<String>> = None;
        for arm in arms.into_iter().flatten() {
            joined = Some(match joined {
                None => arm,
                Some(acc) => acc.intersection(&arm).cloned().collect(),
            });
        }
        self.uncond_bound.extend(joined.unwrap_or_default());
    }

    /// Record a read of `$x`, unless the engine binds the name unconditionally or
    /// an enclosing same-variable guard shields it (see [`guard_tested_names`]). A read
    /// only a disjunctive shield covers is parked for [`Self::settle`].
    fn read_direct(&mut self, dv: &mago_syntax::cst::DirectVariable<'_>, shielded: &[Shield]) {
        let name = strip_dollar(bytes_to_string(dv.name));
        if always_bound(&name) {
            return;
        }
        let mut joints: Vec<Vec<String>> = Vec::new();
        for shield in shielded.iter().filter(|s| s.any || s.name == name) {
            if shield.joint.is_empty() {
                return;
            }
            joints.push(shield.joint.clone());
        }
        let read = UndefinedRead { name, span: to_span(dv.span()) };
        if joints.is_empty() {
            self.reads.push(read);
        } else {
            self.conditional.push((read, joints));
        }
    }

    /// Decide the parked reads, now that `bound` is complete: a disjunction guard
    /// (`isset($x) || isset($y)`) leaves its body unreachable only when **no** disjunct
    /// can hold, so a read stays discharged only while every name one of the guards
    /// covering it tests is *provably* never bound. Otherwise the read is recorded as if
    /// nothing shielded it, which is what it was before the guard was read at all.
    ///
    /// "Never bound" here is wider than the definite pass's own set: `bound` omits a name
    /// a callee binds by reference through a function-call argument, and the names PHP
    /// supplies ([`always_bound`]: the superglobals, `$GLOBALS`, `$this`), and either
    /// can make a disjunct hold.
    pub(crate) fn settle(&mut self) {
        for (read, joints) in std::mem::take(&mut self.conditional) {
            let possibly_bound = |n: &String| {
                always_bound(n) || self.bound.contains(n) || self.call_arg_roots.contains(n)
            };
            if joints.iter().all(|joint| joint.iter().any(possibly_bound)) {
                self.reads.push(read);
            }
        }
    }
}

/// A name a guard tests, and the names that must all be never-bound for the guard's
/// silence to hold. `joint` is empty for a plain `isset($x)` / `empty($x)` test; a
/// disjunction lists every name any disjunct tests, since the body it guards runs when
/// *any* of them is bound (see [`VarUsage::settle`]). An `any` shield is a dead-arm
/// shield ([`with_dead_arm`]): it covers a read of **every** name, and its `joint` is
/// never empty.
#[derive(Clone)]
pub(crate) struct Shield {
    name: String,
    joint: Vec<String>,
    any: bool,
}

impl Shield {
    pub(crate) fn plain(name: String) -> Self {
        Self { name, joint: Vec::new(), any: false }
    }
}

/// `proved`, the names a condition proves bound on one side (see [`bound_when`]), with
/// a **dead-arm** shield added for each when `on` (issue #1108). A region that only an
/// outcome proving `$v` bound reaches never runs when nothing in the scope can bind
/// `$v`, so no read in it warns, whichever name it reads: `if (isset($v)) { echo $u; }`
/// is dead code over a never-bound `$v`. Each dead-arm shield covers every name and
/// parks the read on the proved name (or on a disjunction's whole `joint`), for
/// [`VarUsage::settle`] to discharge once the scope's bindings are known.
fn with_dead_arm(mut proved: Vec<Shield>, on: bool) -> Vec<Shield> {
    if on {
        let dead: Vec<Shield> = proved
            .iter()
            .map(|s| {
                let joint = if s.joint.is_empty() { vec![s.name.clone()] } else { s.joint.clone() };
                Shield { name: s.name.clone(), joint, any: true }
            })
            .collect();
        proved.extend(dead);
    }
    proved
}

/// The variable names an `isset`/`empty` condition **tests**, at either polarity —
/// the shield an enclosing `isset($x) ? … : …` or `if (empty($x)) { … }` casts over
/// its arms **in a scope that holds a `goto`** (issue #1108). This is the `??`
/// discharge idiom in conditional spelling: `empty($page) ? 0 : ($page - 1) * $view`
/// reaches the `$page` read only when `$page` is non-empty, hence bound, so this id's
/// runtime claim — "PHP warns and the read evaluates to null" — is simply false there.
///
/// **Not reachability, and deliberately not.** The rule asks only what the
/// condition spells, then withholds reads in *both* arms without deciding which arm
/// the guard protects — costing a finding on a "wrong" polarity but never
/// manufacturing one. Without a `goto`, [`bound_when`] decides which side a guard
/// proves and is the whole rule, so this containment would only cost findings
/// (`if (empty($u)) { echo $u; }` warns); with one, a jump can land inside an arm
/// without evaluating the condition, the sided rule stands down, and this
/// containment is what is left. `!` and parentheses are transparent; a conjunction
/// (`isset($x) && $y`) tests nothing here.
fn guard_tested_names(cond: &Expression<'_>) -> Vec<Shield> {
    let mut out = Vec::new();
    collect_guard_tested_names(cond, &mut out);
    out.into_iter().map(Shield::plain).collect()
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

/// The names a condition **proves bound** where it evaluates to `want_true` — the
/// shield for a region only that outcome reaches (issue #929). For a name nothing in
/// the scope binds, "proved bound" is "unreachable": `isset` is false and `empty` is
/// true on every run, so a read in such a region is dead code and PHP runs it without
/// the warning. It is [`guard_bound_names`](crate::lower_presence)'s polarity, so a
/// guard shields only the side it actually protects:
///
/// * `isset($x)` proves `$x` when true, and `empty($x)` when false; `!` flips the ask.
///   An offset or property chain proves its root, as in the presence pass.
/// * `&&` when true and `||` when false hold **both** operands, so their names add.
/// * `&&` when false and `||` when true hold *either* operand, so one disjunct's names
///   prove nothing alone: the region is reachable through whichever disjunct holds.
///   Only when **every** disjunct proves something is the region dead for a name no
///   disjunct's name can bind, and each name then carries the whole set as its
///   [`Shield::joint`] — `isset($x) || isset($y)` with a bound `$y` runs its body with
///   `$x` unbound. A disjunct that proves nothing (`$y > 0`) voids the whole.
///
/// The names a condition spells but does not prove on this side stay judged:
/// `isset($x) || print($x)` and `if (isset($x)) { return 1; } return $x;` warn in PHP.
fn bound_when(cond: &Expression<'_>, want_true: bool) -> Vec<Shield> {
    let mut out = Vec::new();
    // A left-nested chain re-enters this per link, so the visit budget bounds the
    // quadratic worst case; running out only withholds a shield, never adds one.
    collect_bound_when(cond, want_true, &mut out, &mut 256);
    out
}

fn collect_bound_when(
    cond: &Expression<'_>,
    want_true: bool,
    out: &mut Vec<Shield>,
    budget: &mut u32,
) {
    if stack_guard::exhausted() || *budget == 0 {
        return;
    }
    *budget -= 1;
    match cond.unparenthesized() {
        Expression::Construct(Construct::Isset(i)) if want_true => {
            let mut roots = Vec::new();
            for value in i.values.iter() {
                push_guard_root(value, &mut roots);
            }
            out.extend(roots.into_iter().map(Shield::plain));
        }
        Expression::Construct(Construct::Empty(e)) if !want_true => {
            let mut roots = Vec::new();
            push_guard_root(e.value, &mut roots);
            out.extend(roots.into_iter().map(Shield::plain));
        }
        Expression::UnaryPrefix(up) if matches!(up.operator, UnaryPrefixOperator::Not(_)) => {
            collect_bound_when(up.operand, !want_true, out, budget);
        }
        Expression::Binary(b) if is_logical_and(&b.operator) || is_logical_or(&b.operator) => {
            // `&&` true and `||` false are conjunctive; the other two are disjunctive.
            if is_logical_and(&b.operator) == want_true {
                collect_bound_when(b.lhs, want_true, out, budget);
                collect_bound_when(b.rhs, want_true, out, budget);
                return;
            }
            let (mut lhs, mut rhs) = (Vec::new(), Vec::new());
            collect_bound_when(b.lhs, want_true, &mut lhs, budget);
            collect_bound_when(b.rhs, want_true, &mut rhs, budget);
            if lhs.is_empty() || rhs.is_empty() {
                return;
            }
            let mut joint: Vec<String> = Vec::new();
            for shield in lhs.iter().chain(&rhs) {
                for name in std::iter::once(&shield.name).chain(&shield.joint) {
                    if !joint.contains(name) {
                        joint.push(name.clone());
                    }
                }
            }
            out.extend(lhs.into_iter().chain(rhs).map(|s| Shield {
                name: s.name,
                joint: joint.clone(),
                any: false,
            }));
        }
        _ => {}
    }
}

fn is_logical_and(op: &mago_syntax::cst::BinaryOperator<'_>) -> bool {
    matches!(
        op,
        mago_syntax::cst::BinaryOperator::And(_) | mago_syntax::cst::BinaryOperator::LowAnd(_)
    )
}

fn is_logical_or(op: &mago_syntax::cst::BinaryOperator<'_>) -> bool {
    matches!(
        op,
        mago_syntax::cst::BinaryOperator::Or(_) | mago_syntax::cst::BinaryOperator::LowOr(_)
    )
}

/// The shield in force inside a guarded construct's arms: `None` when the condition
/// tests nothing, so the caller keeps borrowing its own slice and no allocation
/// happens on the overwhelmingly common path.
fn extend_shield(base: &[Shield], added: Vec<Shield>) -> Option<Vec<Shield>> {
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
        // `$GLOBALS['x'] = …` binds the global `$x` (issue #902): at file scope that is the
        // local; in a function body it binds no local. A key named at run time binds a
        // name nobody can list.
        Expression::ArrayAccess(aa) => {
            match globals_fetch(expr) {
                Some(GlobalsKey::Literal(name)) => {
                    acc.bound.insert(name);
                }
                Some(GlobalsKey::Dynamic) => acc.dammed = true,
                Some(GlobalsKey::Inert) | None => {}
            }
            bind_lvalue_roots(aa.array, acc);
        }
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
    shielded: &[Shield],
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
#[expect(clippy::too_many_lines, reason = "predates the #778 ratchet; split when next reworked")]
pub(crate) fn scan_var_usage(node: &Node<'_, '_>, guarded: bool, shielded: &[Shield], acc: &mut VarUsage) {
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
                let mut roots = Vec::new();
                push_guard_root(arg.value(), &mut roots);
                acc.call_arg_roots.extend(roots);
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
        // id's runtime claim is false there. Each arm takes what the condition proves
        // on its own side (`bound_when`), so `empty($u) ? $u : 0` is still judged
        // (issue #1108). Only with a `goto` in the scope are both arms shielded by
        // the containment rule of `guard_tested_names`. Only the TESTED name is
        // shielded, so `$view` above is still judged. ---
        Node::Conditional(c) => {
            let cond_bound =
                acc.branch(|a| scan_var_usage(&Node::Expression(c.condition), guarded, shielded, a));
            acc.uncond_bound.extend(cond_bound.iter().cloned());
            let extended = acc
                .goto_present
                .then(|| extend_shield(shielded, guard_tested_names(c.condition)))
                .flatten();
            let base = extended.as_deref().unwrap_or(shielded);
            // A dead arm is the sided rule's reading, so it stands down under a `goto`
            // like the `if`'s does, although a jump cannot land inside an expression.
            let dead_arms = acc.dead_arms && !acc.goto_present;
            // `?:` has no `then` arm; its condition IS the value, already walked. Each
            // arm also takes what the condition proves on its own side (issue #929).
            let mut arms = Vec::new();
            if let Some(then) = c.then {
                let proved =
                    extend_shield(base, with_dead_arm(bound_when(c.condition, true), dead_arms));
                let inner = proved.as_deref().unwrap_or(base);
                let bound = acc.branch(|a| scan_var_usage(&Node::Expression(then), guarded, inner, a));
                arms.push((!arm_diverges(then)).then_some(bound));
            }
            let proved =
                extend_shield(base, with_dead_arm(bound_when(c.condition, false), dead_arms));
            let inner = proved.as_deref().unwrap_or(base);
            let bound = acc.branch(|a| scan_var_usage(&Node::Expression(c.r#else), guarded, inner, a));
            arms.push((!arm_diverges(c.r#else)).then_some(bound));
            // Only the full ternary completes through exactly one arm; `?:` may complete
            // on the condition alone, so its `else` binds nothing unconditionally.
            if c.then.is_some() {
                acc.join_arms(arms);
            } else if let Some(else_bound) = arms.pop().flatten() {
                let refinement = elvis_info(&acc.infos, c, &cond_bound, &else_bound);
                acc.finish_operator(span_key(c.span()), refinement);
            }
            return;
        }
        // `match` evaluates its subject, then the first condition of the first arm, then
        // whatever the earlier conditions leave. One arm's result is the value and an
        // unmatched subject throws, so a name every completing arm binds is bound after.
        Node::Match(m) => {
            scan_var_usage(&Node::Expression(m.expression), guarded, shielded, acc);
            let mut arms = Vec::new();
            let mut first_condition = true;
            for arm in m.arms.iter() {
                if let mago_syntax::cst::MatchArm::Expression(e) = arm {
                    for cond in e.conditions.iter() {
                        if std::mem::take(&mut first_condition) {
                            scan_var_usage(&Node::Expression(cond), guarded, shielded, acc);
                        } else {
                            acc.branch(|a| scan_var_usage(&Node::Expression(cond), guarded, shielded, a));
                        }
                    }
                }
                first_condition = false;
                let body = arm.expression();
                let bound = acc.branch(|a| scan_var_usage(&Node::Expression(body), guarded, shielded, a));
                arms.push((!arm_diverges(body)).then_some(bound));
            }
            acc.join_arms(arms);
            return;
        }
        // `$o?->m($x = 1)` skips the arguments on a null receiver, but this pass cannot see
        // whether the receiver may be null, so they bind unconditionally (the silent side).
        Node::NullSafeMethodCall(c) => bind_call_arguments(&c.argument_list, acc),

        // The statement spelling of the same idiom. What the condition *proves*
        // (issue #929) is sided: the then-body takes its true outcome, the
        // `elseif`/`else` clauses its false one, and that is the whole rule (issue
        // #1108): `if (!isset($u)) { echo $u; }` is judged. A read AFTER the `if` is
        // still judged too. With a `goto` in the scope the sided rule stands down and
        // the containment of `guard_tested_names` shields the whole body, including
        // its `elseif`/`else` clauses, in its place.
        Node::If(i) => {
            scan_var_usage(&Node::Expression(i.condition), guarded, shielded, acc);
            // A `goto` can land inside an arm without evaluating the condition, so with
            // one in the scope the arms take no sided shield, and the containment
            // stands in for it.
            let goto_present = acc.goto_present;
            let extended = goto_present
                .then(|| extend_shield(shielded, guard_tested_names(i.condition)))
                .flatten();
            let base = extended.as_deref().unwrap_or(shielded);
            let dead_arms = acc.dead_arms;
            let sided = |want_true| {
                if goto_present {
                    Vec::new()
                } else {
                    with_dead_arm(bound_when(i.condition, want_true), dead_arms)
                }
            };
            let proved = extend_shield(base, sided(true));
            let then = proved.as_deref().unwrap_or(base);
            scan_statement_seq(i.body.statements().iter(), guarded, then, acc);
            let proved = extend_shield(base, sided(false));
            let rest = proved.as_deref().unwrap_or(base);
            for (cond, stmts) in i.body.else_if_clauses() {
                scan_var_usage(&Node::Expression(cond), guarded, rest, acc);
                scan_statement_seq(stmts.iter(), guarded, rest, acc);
            }
            if let Some(stmts) = i.body.else_statements() {
                scan_statement_seq(stmts.iter(), guarded, rest, acc);
            }
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
            let lhs = acc.branch(|a| scan_var_usage(&Node::Expression(b.lhs), true, shielded, a));
            acc.uncond_bound.extend(lhs.iter().cloned());
            let rhs = acc.branch(|a| scan_var_usage(&Node::Expression(b.rhs), guarded, shielded, a));
            if let Some(refinement) = binary_info(&acc.infos, b, &lhs, &rhs) {
                acc.finish_operator(span_key(b.span()), refinement);
            }
            return;
        }
        // The operands of a short-circuit operator are the guard idiom in expression
        // spelling (issue #929): the right operand is reached only where the left
        // operand came out one way — true for `&&`, false for `||` — so it takes the
        // names the left proves bound on that side. `isset($x) && $x > 1` and
        // `!isset($x) || print($x)` are silent; `isset($x) || print($x)` is not. Walked
        // directly rather than through `children`, so the stack guard is asked here.
        Node::Binary(b) if is_logical_and(&b.operator) || is_logical_or(&b.operator) => {
            if stack_guard::exhausted() {
                return;
            }
            let lhs = acc.branch(|a| scan_var_usage(&Node::Expression(b.lhs), guarded, shielded, a));
            acc.uncond_bound.extend(lhs.iter().cloned());
            let proved = extend_shield(shielded, bound_when(b.lhs, is_logical_and(&b.operator)));
            let inner = proved.as_deref().unwrap_or(shielded);
            let rhs = acc.branch(|a| scan_var_usage(&Node::Expression(b.rhs), guarded, inner, a));
            if let Some(refinement) = binary_info(&acc.infos, b, &lhs, &rhs) {
                acc.finish_operator(span_key(b.span()), refinement);
            }
            return;
        }

        // --- Binding forms. None of these return: the main recursion may re-visit
        // the very same token as a read, which the final set difference discards. ---
        // `$a ??= <rhs>` binds `$a` whichever way it goes (a set non-null `$a` is bound
        // already), but runs `<rhs>` only when `$a` is null or unset.
        Node::Assignment(a) if matches!(a.operator, AssignmentOperator::Coalesce(_)) => {
            bind_lvalue_roots(a.lhs, acc);
            scan_var_usage(&Node::Expression(a.lhs), guarded, shielded, acc);
            acc.branch(|x| scan_var_usage(&Node::Expression(a.rhs), guarded, shielded, x));
            return;
        }
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
    // The children of a statement list are its statements, in order — the one place a
    // guard can speak for what *follows* it (issue #929); see `terminating_guard_names`.
    let mut running: Option<Vec<Shield>> = None;
    for child in children(node) {
        // Only a later *statement* is reached through the guard: the `elseif`/`else`
        // clauses that follow a colon-delimited body's statements are not.
        let inner = match &child {
            Node::Statement(_) => running.as_deref().unwrap_or(shielded),
            _ => shielded,
        };
        scan_var_usage(&child, guarded, inner, acc);
        if let Node::Statement(Statement::If(i)) = &child
            && let Some(added) = terminating_guard_names(i, acc.goto_present)
        {
            running = extend_shield(inner, added);
        }
    }
}

/// The shield an `if` casts over the statements **after** it in the same list: the
/// names its condition proves bound where it is **false**, when it has no
/// `elseif`/`else` and its body provably terminates. Control reaches the next
/// statement only by the condition coming out false, so
/// `if (!isset($x)) { return; } print($x);` reads `$x` only with `$x` set, and the read
/// is no `variable.undefined`.
///
/// The body's terminality is the CFG's own ([`block_end`]), so a `try` or a `goto` in
/// it is `Unknown` and shields nothing. The false outcome is [`bound_when`]'s, so
/// `if (isset($x)) {} echo $x;` (nothing terminates), `if (isset($x)) { return 1; }
/// return $x;` (a false `isset` proves nothing) and
/// `if (!isset($x) && $c) { return; } print($x);` (the other conjunct can be what
/// came out false) all keep reporting.
///
/// A `goto` or label anywhere in the scope turns the rule off (`goto_present`): a jump
/// forward lands after the guard without evaluating it, so "only the false outcome
/// reaches here" no longer holds.
fn terminating_guard_names(i: &mago_syntax::cst::If<'_>, goto_present: bool) -> Option<Vec<Shield>> {
    if goto_present || !i.body.else_if_clauses().is_empty() || i.body.else_statements().is_some() {
        return None;
    }
    let names = bound_when(i.condition, false);
    (!names.is_empty() && block_end(i.body.statements()).provably_terminates()).then_some(names)
}

/// Scan one statement list in order, letting a terminating `if` shield the statements
/// after it (see [`terminating_guard_names`]).
fn scan_statement_seq<'ast, 'arena>(
    statements: impl Iterator<Item = &'ast Statement<'arena>>,
    guarded: bool,
    shielded: &[Shield],
    acc: &mut VarUsage,
) where
    'arena: 'ast,
{
    let mut running: Option<Vec<Shield>> = None;
    for s in statements {
        let inner = running.as_deref().unwrap_or(shielded);
        scan_var_usage(&Node::Statement(s), guarded, inner, acc);
        if let Statement::If(i) = s
            && let Some(added) = terminating_guard_names(i, acc.goto_present)
        {
            running = extend_shield(inner, added);
        }
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
    acc.goto_present = statements.iter().any(|s| subtree_has_goto(&Node::Statement(s)));
    acc.dead_arms = true;
    scan_statement_seq(statements.iter().copied(), false, &[], &mut acc);
    acc.settle();
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
    let mut sites = Vec::new();
    // An arrow body is a single expression — no local assignments to resolve.
    let cx = EffectScanCx::new(
        &af.parameter_list,
        HashMap::new(),
        node_poisons(&Node::Expression(af.expression)),
        ReceiverWrites::poisoned(),
    )
    .returning(af.return_type_hint.as_ref().map(|r| &r.hint))
    .generator(node_is_generator(&Node::Expression(af.expression)))
    .with_body(&af.parameter_list, Captures::All, std::iter::once(Node::Expression(af.expression)));
    param_default_sites(&af.parameter_list, &cx, &mut sites);
    scan_owner_sites(&Node::Expression(af.expression), &cx, &mut sites);
    arrow_return_site(af.expression, &cx, &mut sites);
    scope_exit_sites(&cx, body_end(af.expression.span()), &mut sites);
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
    let mut guards = Vec::new();
    scan_guard_regions(&Node::Expression(af.expression), &mut guards);
    let kind = StmtKind::Return { value, call, span };
    let reads = read_positions_of_expr(af.expression, &kind);
    let ret = Stmt {
        span,
        kind,
        invalidated,
        string_contexts,
        guards,
        // An arrow body IS a `return`, so the scope's trace always terminates —
        // which is precisely why `fn () => …` can never be a `type.return-missing`
        // site, no matter what it declares (ADR-0078, issue #199).
        end: BodyEnd::Terminates,
        has_terminator: true,
        value_position: false,
        // An arrow body is never the top-level frame (issue #762).
        runs: Runs::default(),
        holds_label: false,
        unspelled: None,
        suspends: false,
        writes_through: None,
        read_positions: reads,
    };
    let ret = with_heap_fill(ret, &Node::Expression(af.expression), &[af.expression]);
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
        yields_by_ref: is_generator && af.ampersand.is_some(),
        poisoned,
        opaque,
        stmts: vec![ret],
        returns: vec![ReturnSite { form: ReturnForm::ArrowBody, span }],
        method_calls,
        params: lower_params(&af.parameter_list, rc),
        ret_ty: af.return_type_hint.as_ref().and_then(|r| lower_hint(&r.hint, rc)),
        sites,
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

/// Whether a ternary or `match` arm may never complete, so that it drops out of the
/// "every arm binds" intersection: a `throw` expression, `exit`/`die`, or a call, since
/// whether a callee is `: never` is the project index's to say and this pass has none.
fn arm_diverges(expr: &Expression<'_>) -> bool {
    matches!(
        expr.unparenthesized(),
        Expression::Throw(_)
            | Expression::Construct(Construct::Exit(_) | Construct::Die(_))
            | Expression::Call(_)
    )
}
