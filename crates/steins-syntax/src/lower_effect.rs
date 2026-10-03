//! Effect and throw origin scanning over one function-like body (ADR-0018,
//! ADR-0040): the call/output/exit origins the body performs, the receivers and
//! callbacks they act on, and the `throw`/`try`/`catch` structure that decides
//! which throws escape.
//!
//! What remains here is the frame context a site scan reads (`EffectScanCx`) and the
//! helpers it shares with `lower_site`, which lowers the sites themselves once for
//! both lanes.

use std::collections::HashMap;
use std::collections::HashSet;

use mago_span::HasSpan;
use mago_syntax::cst::{
    Access, AnonymousClass, Argument, ArrayElement, BinaryOperator, ClassLikeMember, Expression,
    FunctionCall, Literal, Node, PartialApplication, Statement, UnaryPrefixOperator, Variable,
};

use crate::ast::{
    ArgValue, CallExpr, CallTarget, CallbackRef, ConstArgs, ConstInt, NameRef, RefKind, RefTarget,
    SUPERGLOBALS, StaticClass,
};
use crate::lower_arg_shape::{Captures, FrameBindings};
use crate::lower_expr::{
    first_class_method_ref, first_class_static_ref, lower_int_literal, lower_method_call,
    lower_static_call, prop_fetch_of,
};
use crate::lower_scope::{arrow_def_offset, closure_def_offset};
use crate::lower_stmt::{
    collect_assign_writes, collect_call_vars, collect_direct_vars, node_poisons,
};
use crate::names::name_ref;
use crate::utf8_loss;
use crate::{bytes_to_string, children, strip_dollar, to_span};

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
            let raw = utf8_loss::literal_name(ls)?;
            // Method string callables (`Foo::m`) are not resolved.
            if raw.contains("::") || raw.is_empty() {
                return None;
            }
            Some(CallbackRef::Named(NameRef {
                raw: raw.trim_start_matches('\\').to_owned(),
                kind: if raw.starts_with('\\') {
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
pub(crate) fn higher_order_of_call(fc: &FunctionCall<'_>) -> Option<HigherOrderCall> {
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
/// mapping — see [`crate::ast::EffectOrigin::Call`]'s `arg_targets`.
pub(crate) fn arg_targets_of_call(fc: &FunctionCall<'_>, cx: &EffectScanCx) -> Option<Vec<RefTarget>> {
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
/// ([`ConstArgs`], issue #318), its flag-like integers and its literal booleans at
/// positions 2 and 3. Empty when a named or spread argument defeats
/// positional mapping — the same list shapes [`arg_targets_of_call`] withholds.
pub(crate) fn const_args_of_call(fc: &FunctionCall<'_>) -> ConstArgs {
    let mut out = ConstArgs::default();
    for (pos, arg) in fc.argument_list.arguments.iter().enumerate() {
        let Argument::Positional(p) = arg else { return ConstArgs::default() };
        if p.ellipsis.is_some() {
            return ConstArgs::default();
        }
        match pos {
            0 => out.first = const_arg_of(p.value),
            1 => out.second = const_arg_of(p.value),
            // Nothing past position 3 is read; the loop runs on only to catch a
            // named/spread argument further along.
            _ => {}
        }
        if (1..=3).contains(&pos)
            && let Some(int) = const_int_of(p.value)
        {
            out.ints.push((u8::try_from(pos).expect("a position of 1 to 3"), int));
        }
        if (2..=3).contains(&pos)
            && let Some(CallTarget::Bool(flag)) = const_arg_of(p.value)
        {
            out.bools.push((u8::try_from(pos).expect("a position of 2 or 3"), flag));
        }
    }
    out
}

/// One argument expression as a [`ConstInt`], or `None` when it is anything
/// other than integer literals, bare global constants and their `|` (ADR-0099
/// §3.3).
fn const_int_of(expr: &Expression<'_>) -> Option<ConstInt> {
    match expr.unparenthesized() {
        Expression::Literal(Literal::Integer(li)) => match lower_int_literal(li.raw) {
            ArgValue::Int(v) => Some(ConstInt::Int(v)),
            _ => None,
        },
        Expression::ConstantAccess(ca) => {
            let name = name_ref(&ca.name);
            (!name.raw.contains('\\')).then_some(ConstInt::Const(name.raw))
        }
        Expression::Binary(b) if matches!(b.operator, BinaryOperator::BitwiseOr(_)) => {
            let mut terms = Vec::new();
            for side in [b.lhs, b.rhs] {
                match const_int_of(side)? {
                    ConstInt::Or(inner) => terms.extend(inner),
                    term => terms.push(term),
                }
            }
            Some(ConstInt::Or(terms))
        }
        _ => None,
    }
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
        // The return-mode flag (issue #352). PHP's `true`/`false` are keywords to
        // the lexer, case-insensitively, so they never reach the `ConstantAccess`
        // arm below however they are spelled.
        Expression::Literal(Literal::True(_)) => Some(CallTarget::Bool(true)),
        Expression::Literal(Literal::False(_)) => Some(CallTarget::Bool(false)),
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

/// The per-frame context the site scan consults: the ADR-0033
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
    /// Whether the frame is a `__construct` body, whose writes to `$this`'s own
    /// properties are exempt from [`crate::ast::StateConstruct::PropertyWrite`]
    /// ([`property_write_span`]). Only a method sets it: a closure or arrow
    /// function defined in a constructor is a frame of its own.
    pub(crate) constructor: bool,
    /// What the frame's variables are shown to hold, for the [`crate::ast::ArgShape`] of a
    /// bare variable argument; opaque until [`Self::with_body`] builds it.
    pub(crate) bindings: FrameBindings,
    /// The variables of the frame that may hold a value to drop (ADR-0100 §7);
    /// empty until [`Self::with_body`] reads the frame.
    pub(crate) drops: crate::lower_site::DropSubjects,
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
        let bindings = FrameBindings::opaque();
        let drops = crate::lower_site::DropSubjects::new();
        Self { locals, byref_params, frame_aliased, writes, constructor: false, bindings, drops }
    }

    /// Mark the frame as a `__construct` body ([`Self::constructor`]).
    pub(crate) const fn in_constructor(mut self, constructor: bool) -> Self {
        self.constructor = constructor;
        self
    }

    /// Summarize what `body` stores into the frame's variables
    /// ([`FrameBindings`]); an aliasing frame keeps the opaque summary.
    pub(crate) fn with_body<'a, 'arena: 'a>(
        mut self,
        params: &mago_syntax::cst::FunctionLikeParameterList<'_>,
        captures: Captures<'_>,
        body: impl Iterator<Item = Node<'a, 'arena>> + Clone,
    ) -> Self {
        self.drops = crate::lower_site::drop_subjects(params, body.clone(), self.constructor);
        if !self.frame_aliased {
            self.bindings = FrameBindings::new(params, captures, body);
        }
        self
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
pub(crate) fn direct_var_callee(fc: &FunctionCall<'_>) -> Option<String> {
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

/// The constructor a `new class(...) {...}` expression runs, as the effect and
/// throw scans both read it (issues #804, #849).
pub(crate) enum AnonymousConstructor {
    /// The class body is never indexed, so a constructor it may bring — its
    /// own, or one a trait it uses supplies — is unseen, and taints like a
    /// dynamic `new`.
    Unseen,
    /// With neither, the constructor is the parent's, if the class extends one.
    Inherited(StaticClass),
    /// And there is none at all otherwise.
    None,
}

/// Read the [`AnonymousConstructor`] off an anonymous class's members and
/// `extends`.
pub(crate) fn anonymous_class_constructor(ac: &AnonymousClass<'_>) -> AnonymousConstructor {
    let own_constructor = ac.members.iter().any(|m| match m {
        ClassLikeMember::Method(m) => {
            bytes_to_string(m.name.value).eq_ignore_ascii_case("__construct")
        }
        ClassLikeMember::TraitUse(_) => true,
        _ => false,
    });
    if own_constructor {
        return AnonymousConstructor::Unseen;
    }
    match ac.extends.as_ref().and_then(|e| e.types.iter().next()) {
        Some(parent) => AnonymousConstructor::Inherited(StaticClass::Named(name_ref(parent))),
        None => AnonymousConstructor::None,
    }
}

/// Whether a direct variable's spelled name (`$` included) is one of the
/// [`SUPERGLOBALS`]. PHP spells them case-sensitively, and a variable variable
/// cannot reach one inside a function-like, so the direct spelling is all there is.
pub(crate) fn is_superglobal(name: &[u8]) -> bool {
    name.strip_prefix(b"$").is_some_and(|n| SUPERGLOBALS.iter().any(|s| s.as_bytes() == n))
}

/// Where `node` writes an instance property, if it does
/// ([`crate::ast::StateConstruct::PropertyWrite`]): the written lvalue's span. A `&` binding
/// counts, since a later write through the alias lands in the property; that
/// includes a by-reference `foreach` over a property, whose elements the loop
/// variable aliases.
///
/// `constructor` is ADR-0055's constructor-creation exemption (point 13, #313),
/// applied to **exhaustiveness only**: inside a `__construct` body, a write whose
/// base is literally `$this` is initialization rather than state the call
/// changes, so it records nothing. It mirrors the exclusion PHPStan's
/// `ClassMethodHandler` makes for a property assignment in the declaring class's
/// constructor, and so covers exactly the writes PHPStan reports there as a
/// property assignment — an assignment of any operator, `++`/`--`, a write or
/// `unset` through an offset, a destructuring or `foreach` target, and a
/// `foreach` by reference over the property, which PHPStan reports nothing for.
/// Two `$this` writes stay recorded, because PHPStan reports each under another
/// identifier its constructor exclusion does not reach: `unset($this->p)`
/// (`propertyUnset`) and a `&` binding of `$this->p` (`propertyAssignByRef`).
/// So is `$self = $this; $self->p = …` (the base is not literally `$this`), a
/// write to another object's property, and a `$this` write in a closure or arrow
/// function defined in the constructor, which can run after construction.
pub(crate) fn property_write_span(node: &Node<'_, '_>, constructor: bool) -> Option<mago_span::Span> {
    let written = |e: &Expression<'_>, exempt: bool| writes_property(e, exempt).then(|| e.span());
    match node {
        Node::Assignment(a) => written(a.lhs, constructor),
        Node::UnaryPrefix(u) => match u.operator {
            UnaryPrefixOperator::PreIncrement(_) | UnaryPrefixOperator::PreDecrement(_) => {
                written(u.operand, constructor)
            }
            UnaryPrefixOperator::Reference(_) => written(u.operand, false),
            _ => None,
        },
        // `$x++` / `$x--`, the only postfix operators, write their operand.
        Node::UnaryPostfix(u) => written(u.operand, constructor),
        // Unsetting a whole property is never exempt; unsetting through an offset is.
        Node::Unset(u) => u.values.iter().find_map(|v| {
            let whole = matches!(v.unparenthesized(), Expression::Access(Access::Property(_)));
            written(v, constructor && !whole)
        }),
        Node::Foreach(fe) => {
            let target = &fe.target;
            let aliased =
                if target.value().is_reference() { written(fe.expression, constructor) } else { None };
            aliased
                .or_else(|| target.key().and_then(|k| written(k, constructor)))
                .or_else(|| written(target.value(), constructor))
        }
        _ => None,
    }
}

/// Whether an lvalue's write lands in an instance property. Offsets peel down to
/// the base written through (`$o->p[] = …` and `$o->p['k'] = …` write `$o->p`),
/// a property access there is the write (`$a[0]->p = …` included), and a
/// destructuring pattern asks each of its targets. An offset's *index* is a read.
/// `exempt_this` exempts a property whose object is literally `$this` — the
/// constructor exemption of [`property_write_span`], decided per target.
fn writes_property(lvalue: &Expression<'_>, exempt_this: bool) -> bool {
    let each = |element: &ArrayElement<'_>| element_writes_property(element, exempt_this);
    let mut cur = lvalue.unparenthesized();
    loop {
        cur = match cur {
            Expression::ArrayAccess(aa) => aa.array.unparenthesized(),
            Expression::ArrayAppend(ap) => ap.array.unparenthesized(),
            Expression::Access(Access::Property(pa)) => return !(exempt_this && is_this_expr(pa.object)),
            Expression::Access(Access::NullSafeProperty(_)) => return true,
            Expression::Array(a) => return a.elements.iter().any(each),
            Expression::LegacyArray(a) => return a.elements.iter().any(each),
            Expression::List(l) => return l.elements.iter().any(each),
            _ => return false,
        };
    }
}

/// [`writes_property`] for one destructuring target; a key is a read.
fn element_writes_property(element: &ArrayElement<'_>, exempt_this: bool) -> bool {
    match element {
        ArrayElement::KeyValue(kv) => writes_property(kv.value, exempt_this),
        ArrayElement::Value(v) => writes_property(v.value, exempt_this),
        ArrayElement::Variadic(_) | ArrayElement::Missing(_) => false,
    }
}

/// Walk a body subtree, appending every instance/static method call as a
/// [`CallExpr`] (ADR-0043 §6 comprehensive method-call surface). Mirrors
/// the site scan's traversal discipline: descends control flow and
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
