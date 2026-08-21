//! Effect and throw origin scanning over one function-like body (ADR-0018,
//! ADR-0040): the call/output/exit origins the body performs, the receivers and
//! callbacks they act on, and the `throw`/`try`/`catch` structure that decides
//! which throws escape.

use std::collections::HashMap;
use std::collections::HashSet;

use mago_span::HasSpan;
use mago_syntax::cst::{
    Argument, Expression, FunctionCall, Literal, Node, PartialApplication, Statement,
    UnaryPrefixOperator, Variable,
};

use crate::ast::{
    CallExpr, CallTarget, CallbackRef, CatchClause, ConstArgs, EffectOrigin, NameRef, RefKind,
    RefTarget, SUPERGLOBALS, ThrowKind, ThrowOrigin,
};
use crate::lower_decl::lower_catch_clause;
use crate::lower_expr::{
    effect_recv_of_class, effect_recv_of_object, effect_recv_of_object_declared,
    first_class_method_ref, first_class_static_ref, instantiation_class, lower_method_call,
    lower_static_call, method_name_of, prop_fetch_of,
};
use crate::{
    arrow_def_offset, bytes_to_string, children, closure_def_offset, collect_assign_writes,
    collect_call_vars, collect_direct_vars, name_ref, node_poisons, strip_dollar, to_span,
};

/// A resolvable [`CallbackRef`] for a callback argument expression (ADR-0033): an
/// inline closure/arrow, a first-class callable, or a string-literal function name.
/// `None` for anything else (`$var`, `[$o, 'm']`, non-literal) — the opaque side.
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

/// A higher-order call decomposition: `(callee, positional callbacks, arg count)`.
type HigherOrderCall = (NameRef, Vec<(usize, CallbackRef)>, usize);

/// The positional callback arguments of a named-function call, when at least one is a
/// resolvable [`CallbackRef`] (ADR-0033). `None` for a non-named-function call, a
/// named/spread argument, or no resolvable callback.
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
            // Nothing past position 1 decides a target; the loop runs on only to catch
            // a named/spread argument further along.
            _ => {}
        }
    }
    out
}

/// One argument expression as a [`CallTarget`], or `None` when not written in source.
fn const_arg_of(expr: &Expression<'_>) -> Option<CallTarget> {
    match expr.unparenthesized() {
        // The parser hands escape-decoded bytes; a stream target is a path/URL/wrapper name,
        // so a lossy decode of non-UTF-8 bytes can only lose a narrowing, never invent
        // a scheme.
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

/// Classify one argument expression's **lvalue root** ([`RefTarget`]): offsets are
/// transparent (`sort($rows[3])` writes into `$rows`), so an `ArrayAccess` chain's root
/// decides; anything but a plain variable root is [`RefTarget::Escaping`].
fn ref_target_of_arg(expr: &Expression<'_>, cx: &EffectScanCx) -> RefTarget {
    let mut cur = expr.unparenthesized();
    // Peel offsets down to the base being written through.
    while let Expression::ArrayAccess(aa) = cur {
        cur = aa.array.unparenthesized();
    }
    let Expression::Variable(Variable::Direct(dv)) = cur else {
        // Property/static-property/class-constant roots, `$$v`, calls — none frame-private.
        return RefTarget::Escaping;
    };
    let name = strip_dollar(bytes_to_string(dv.name));
    if SUPERGLOBALS.contains(&name.as_str()) {
        return RefTarget::Superglobal;
    }
    // A by-ref parameter aliases the *caller's* binding: writing it is caller-observable.
    if cx.byref_params.contains(&name) {
        return RefTarget::Escaping;
    }
    // In an aliased frame no name is provably frame-private (`global`, `$a = &$b`,
    // `extract()`/`$$v` can rebind anything); proving *which* names survive is a
    // dataflow question this structural scan doesn't ask (ADR-0001 give-up discipline).
    if cx.frame_aliased {
        return RefTarget::Escaping;
    }
    RefTarget::Local
}

/// The per-frame context [`scan_effect_origins`] consults: the ADR-0033
/// callback-resolution map, plus the two facts by-ref out-parameter coloring
/// needs about the enclosing frame (ADR-0063 §2.3).
pub(crate) struct EffectScanCx {
    /// Body-local single-assignment `$var → CallbackRef` map (ADR-0033).
    pub(crate) locals: HashMap<String, CallbackRef>,
    /// Names bound by a by-ref parameter: writes through them are caller-observable.
    byref_params: HashSet<String>,
    /// Whether the frame carries any construct defeating "this name is frame-private"
    /// — `global`, `static`, `$$v`, `extract`/`compact`, `eval`, `include`, a reference
    /// assignment, or by-ref `use (&$x)`. Exactly the ADR-0001 give-up list ([`scan_opaque`]).
    pub(crate) frame_aliased: bool,
    /// What this frame writes, for the ADR-0067 declared-receiver gate.
    pub(crate) writes: ReceiverWrites,
}

impl EffectScanCx {
    /// Build the context for a function-like frame: parameter list, callback map,
    /// aliasing verdict, and receiver-write set (ADR-0067).
    pub(crate) fn new(
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

/// What a frame **writes**, for the ADR-0067 declared-receiver gate: a receiver keeps
/// its declaration's effect envelope only while its binding is still the one declared,
/// so any write anywhere in the body — assignment, increment, `foreach`/`catch` binding,
/// or a by-ref-capable call — disqualifies **every** use of that name (pre-ADR-0067 taint).
#[derive(Debug, Default)]
pub(crate) struct ReceiverWrites {
    /// Variable names (no `$`) the body may write, over-approximated.
    vars: HashSet<String>,
    /// `$this->…` property names the body may write, over-approximated.
    props: HashSet<String>,
    /// Treat *every* name as written — a frame the gate doesn't model (a closure/
    /// arrow body, or one where `$this` escapes to another name).
    all: bool,
}

impl ReceiverWrites {
    /// The verdict for a frame the gate does not model: nothing is stable.
    pub(crate) fn poisoned() -> Self {
        Self { vars: HashSet::new(), props: HashSet::new(), all: true }
    }

    pub(crate) fn writes_var(&self, name: &str) -> bool {
        self.all || self.vars.contains(name)
    }

    pub(crate) fn writes_prop(&self, name: &str) -> bool {
        self.all || self.props.contains(name)
    }
}

/// Collect a statement body's [`ReceiverWrites`]. Variables reuse the existing
/// over-approximating collectors (assignment/increment/binding lvalues, plus any
/// variable handed to a call), joined with [`collect_frame_rebinds`] for constructs
/// those collectors miss; properties get the same treatment via [`collect_this_prop_writes`].
pub(crate) fn receiver_writes<'a, 'arena>(statements: impl Iterator<Item = &'a Statement<'arena>>) -> ReceiverWrites
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

/// The two ways a frame's *binding* changes without an assignment the shared
/// collectors see — both count as writes for the declared-receiver gate:
///
/// * a **by-ref closure capture**, `use (&$r)`: the closure can rebind `$r` whenever
///   called, so it's written unconditionally. A by-value `use ($r)`/arrow capture
///   is a copy and rebinds nothing.
/// * a **`global $r;`** statement, rebinding to the interpreter's global — legal
///   even when `$r` is a parameter.
///
/// Over-collection is sound (falls back to pre-ADR-0067 taint). Descends into nested
/// closures (own binding) but not named function/class-like declarations.
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

/// Record every `$this->prop` a subtree may **write** (poisoning the whole property
/// set when `$this` escapes into another binding). Mirrors [`collect_assign_writes`]'s
/// traversal but **descends into closures/arrow functions** — a non-static one shares
/// the enclosing `$this`. Descending into a `static function(){}` over-collects (sound);
/// named function/class-like declarations, whose `$this` is foreign, are not descended.
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
        // An argument may be taken by reference, so handing a property to a call counts
        // as a write here — and handing `$this` itself escapes entirely.
        Node::FunctionCall(c) => note_argument_escapes(&c.argument_list, w),
        Node::MethodCall(c) => note_argument_escapes(&c.argument_list, w),
        Node::NullSafeMethodCall(c) => note_argument_escapes(&c.argument_list, w),
        Node::StaticMethodCall(c) => note_argument_escapes(&c.argument_list, w),
        // A foreign `$this` — this is a foreign world; closures/arrows share ours instead.
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
pub(crate) fn body_aliased<'a, 'arena>(statements: impl Iterator<Item = &'a Statement<'arena>>) -> bool
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
/// written **exactly once** in the body, to a resolvable callback literal (closure /
/// first-class callable / string-literal function name), resolves a later `$var()`
/// call to that callback. Multiple writes exclude it (ambiguous → opaque taint). A
/// conditional single assignment still counts — structural, not path-sensitive.
pub(crate) fn collect_body_callables<'a, 'arena>(
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
pub(crate) fn scan_effect_origins(node: &Node<'_, '_>, cx: &EffectScanCx, out: &mut Vec<EffectOrigin>) {
    match node {
        // A statically-named call is either a builtin (catalog-classified) or a
        // same-file user function (a propagation edge) — the effects pass decides.
        Node::FunctionCall(fc) => {
            if let Expression::Identifier(id) = fc.function {
                // A named call passing a resolvable callback is a HigherOrder origin;
                // otherwise a plain Call edge. `higher_order_of_call` and
                // `arg_targets_of_call` reject the same named/spread argument lists,
                // so on the `Some` arm the target vector is exactly `arg_count` long.
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
        // Raw text between `?>` and the next `<?php` inside a body: the engine writes
        // it to the output channel exactly as `echo` does (ADR-0008 always said so;
        // ADR-0083 wired it). Whitespace-only inline text is skipped — layout
        // punctuation between tag pairs isn't output anyone writes a function for,
        // and coloring it would tie the effect to template indentation.
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
/// [`scan_effect_origins`]'s traversal discipline: descends control flow and
/// sub-expressions (`foo($this->m($x))` is captured) but not nested
/// function/closure/class-like bodies, their own scopes. Dynamic receivers/
/// selectors are still recorded ([`Callee::Dynamic`]) so the sweep can taint them.
/// Constructor calls are omitted — the constructor is magic, never a transform
/// candidate.
pub(crate) fn scan_method_calls(node: &Node<'_, '_>, out: &mut Vec<CallExpr>) {
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
        // 8.1) — is not a call but a reference to the method as a value, making its
        // callers unenumerable exactly as `[$o, 'm']` does. Lowers to
        // [`ArgValue::Other`], invisible to the value scan; recorded here as a
        // non-positional reference-"call" so the reverse sweep taints the method
        // instead of promoting it. Constructor first-class callables cannot exist.
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
/// throw-relevant construct in a body — explicit throws, function/method call
/// edges — tagged with the ordered enclosing `try`/`catch` guards that may dam it.
/// Independent of the trace IR: try/catch nesting is handled by threading a guard
/// stack (`guards`, outer→inner) and a catch-variable scope (`catch_scope`, for
/// rethrow precision) through the descent.
///
/// * A `try` block is walked with this try's guard pushed; its `catch` and
///   `finally` blocks are walked WITHOUT it (a catch body is outside its own
///   clause but inside outer trys; `finally` absorbs nothing).
/// * `throw new X` records the class; `throw $e` of an enclosing catch parameter
///   re-emits that catch's absorbed set (rethrow); any other throw taints.
pub(crate) fn scan_throw_origins(
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
                    // Rethrow precision is only sound while `$e` still holds the caught
                    // exception. If the clause body writes the variable — by assignment
                    // or by handing it to any call (a by-ref signature could rebind it)
                    // — a later `throw $e` may throw something else, so the variable
                    // must NOT enter the rethrow scope (its throws degrade to Taint).
                    // Counterexample this fixed: `catch (RuntimeException $e) { $e =
                    // new JsonException(); throw $e; }` under `@throws JsonException`
                    // falsely reported RuntimeException.
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
        // runtime (ADR-0031 Part B) — recorded here as a structural possible-throw;
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
