//! Expression lowering: calls and their arguments ([`ArgValue`]), receivers and
//! static classes, literals and array literals, and the condition grammar
//! ([`CondExpr`], ADR-0031) the linear-trace statements carry.

use mago_span::HasSpan;
use mago_syntax::cst::{
    Access, Argument, ArrayElement, Binary, BinaryOperator, Call, ClassLikeConstantSelector,
    ClassLikeMemberSelector, Construct, DeclareItem, Expression, FunctionCall, Instantiation,
    Literal, Node, PartialApplication, Statement, UnaryPrefixOperator, Variable,
};
use steins_domain::PhpStr;

use crate::ast::{
    Arg, ArgValue, ArrayKey, CallExpr, Callee, ClosureRef, CmpOp, CondExpr, CondOperand, EffectRecv,
    IssetOperand, NameRef, NamedArg, Receiver, RefKind, Span, StaticClass, Stmt, StmtKind, ValueOp,
};
use crate::lower_effect::EffectScanCx;
use crate::lower_scope::{
    arrow_def_offset, arrow_free_vars, closure_def_offset, closure_use_captures,
};
use crate::lower_stmt::{
    call_invalidation, collect_assign_writes, collect_call_vars, collect_read_vars, named_call,
    node_poisons,
};
use crate::names::name_ref;
use crate::stack_guard;
use crate::{bytes_to_string, children, strip_dollar, to_span};

pub(crate) fn lower_call(c: &FunctionCall<'_>) -> CallExpr {
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
/// the call has no positional first argument. Case-insensitive; accepts `assert`
/// and `\assert`, rejects a namespaced `Foo\assert`.
pub(crate) fn assert_stmt_cond(c: &FunctionCall<'_>) -> Option<CondExpr> {
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
/// argument unpacking (ADR-0049 §6). A positional argument after a named/spread
/// one is a PHP compile error; folded into `has_spread` (the "unanalyzable shape"
/// signal) so the arity check stays silent on it.
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
/// `None` when the argument is not a condition the [`CondExpr`] vocabulary models.
///
/// Not `lower_cond` under another name: that's total (walks the subtree, answering
/// `Opaque { reads }` for anything unmodeled), but this runs on **every argument of
/// every call in the project** and must decline in O(1) for the dominant shapes
/// (variable, literal, property fetch, concatenation) — only recognized arms walk.
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
pub(crate) fn method_name_of(selector: &ClassLikeMemberSelector<'_>) -> Option<String> {
    match selector {
        ClassLikeMemberSelector::Identifier(id) => Some(bytes_to_string(id.value)),
        _ => None,
    }
}

/// The constant / enum-case name of a `Class::NAME` access, if statically named
/// (`::CONST`, `::Case`). A dynamic name (`Class::{$x}`) yields `None`.
pub(crate) fn class_const_name(selector: &ClassLikeConstantSelector<'_>) -> Option<String> {
    match selector {
        ClassLikeConstantSelector::Identifier(id) => Some(bytes_to_string(id.value)),
        _ => None,
    }
}

/// The class reference of an instantiation's class expression, if statically
/// named (`new Foo(...)`). Dynamic (`new $c()`) yields `None`.
pub(crate) fn instantiation_class(inst: &Instantiation<'_>) -> Option<NameRef> {
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
        // `(new Foo(args))->m()`: the constructor's arguments travel with the
        // receiver (issue #386), because the receiver object is minted right here
        // and its state is what the call dispatches against. Same lowering the
        // `Instantiation` arm of [`lower_arg_value`] gives an argument-position
        // `new`, so the two positions cannot disagree about what was written.
        Expression::Instantiation(inst) => instantiation_class(inst).map(|class| {
            let (args, named) = match &inst.argument_list {
                Some(list) => {
                    let LoweredArgs { args, named_args, .. } = lower_argument_list(list);
                    (args.into_iter().map(|a| a.value).collect(), named_args)
                }
                None => (Vec::new(), Vec::new()),
            };
            Receiver::New { class, args, named }
        }),
        // A depth-1 property-fetch receiver `$var->prop->m()` (ADR-0052 §7): the
        // object is read from the heap `$var->prop` fact. A chain or dynamic name
        // (`prop_fetch_of` returns `None`) falls through to `Dynamic`. The receiver
        // var is never `$this` here — `$this->prop->m()` decomposes as a `$this`
        // property whose object is `prop` (a Prop, not `Receiver::This`), kept out
        // of the guarded `$this` dispatch lane by construction.
        Expression::Access(Access::Property(pa)) => {
            prop_fetch_of(pa.object, &pa.property).map(|(var, prop)| Receiver::Prop { var, prop })
        }
        _ => None,
    }
}

/// A simple property access `$var->prop` decomposed into `(var, prop)` (ADR-0036),
/// or `None` when the receiver is not a bare variable or the selector is not a
/// static identifier (dynamic name `$o->$p`, chain `$a->b->c`, `list()`-lvalue …).
pub(crate) fn prop_fetch_of(object: &Expression<'_>, selector: &ClassLikeMemberSelector<'_>) -> Option<(String, String)> {
    let var = match object.unparenthesized() {
        Expression::Variable(Variable::Direct(dv)) => strip_dollar(bytes_to_string(dv.name)),
        _ => return None,
    };
    let prop = method_name_of(selector)?;
    Some((var, prop))
}

/// The trace [`StaticClass`] of a static-call class expression.
pub(crate) fn trace_static_class(class: &Expression<'_>) -> Option<StaticClass> {
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
pub(crate) fn effect_recv_of_object(object: &Expression<'_>) -> Option<EffectRecv> {
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
/// declared forms: a never-written variable (`$r->m()`) and a never-written `$this`
/// property read (`$this->repo->m()`). Both are recorded by *name* only — no class
/// here; the effects pass resolves the declared type and decides whether an
/// interface envelope applies, and failure taints exhaustiveness. Proven forms
/// delegate to [`effect_recv_of_object`], which the throw scan also uses.
pub(crate) fn effect_recv_of_object_declared(object: &Expression<'_>, cx: &EffectScanCx) -> Option<EffectRecv> {
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
pub(crate) fn effect_recv_of_class(class: &Expression<'_>) -> Option<EffectRecv> {
    match class {
        Expression::Identifier(id) => Some(EffectRecv::ClassName(name_ref(id))),
        Expression::Self_(_) => Some(EffectRecv::SelfKw),
        Expression::Parent(_) => Some(EffectRecv::Parent),
        _ => None,
    }
}

/// The [`Callee`] of an instance-method call — [`Callee::Dynamic`] when either
/// half (receiver, method name) is one resolution cannot reason about. The ONE
/// receiver lowering: the statement form, the first-class-callable reference and
/// the value-position [`ArgValue::MethodCall`] all come through here, so a
/// receiver can never be spelled two ways (issue #386).
fn trace_method_callee(object: &Expression<'_>, selector: &ClassLikeMemberSelector<'_>, nullsafe: bool) -> Callee {
    match (trace_recv_of_object(object), method_name_of(selector)) {
        (Some(recv), Some(method)) => Callee::Method { receiver: recv, method, nullsafe },
        _ => Callee::Dynamic,
    }
}

/// The [`Callee`] of a static call — the `::` twin of [`trace_method_callee`],
/// shared by the same three lowerings.
fn trace_static_callee(class: &Expression<'_>, selector: &ClassLikeMemberSelector<'_>) -> Callee {
    match (trace_static_class(class), method_name_of(selector)) {
        (Some(class), Some(method)) => Callee::Static { class, method },
        _ => Callee::Dynamic,
    }
}

/// Lower a method call (`MethodCall` / `NullSafeMethodCall`) into a [`CallExpr`].
/// `nullsafe` marks the `?->` form (see [`Callee::Method`]).
pub(crate) fn lower_method_call(object: &Expression<'_>, selector: &ClassLikeMemberSelector<'_>, list: &mago_syntax::cst::ArgumentList<'_>, span: Span, nullsafe: bool) -> CallExpr {
    let receiver = trace_method_callee(object, selector, nullsafe);
    let LoweredArgs { args, named_args, has_spread, positional_only, arg_conds } =
        lower_argument_list(list);
    CallExpr { callee: None, callee_ref: None, receiver, args, named_args, has_spread, positional_only, span, arg_conds }
}

/// Lower a static method call into a [`CallExpr`].
pub(crate) fn lower_static_call(class: &Expression<'_>, selector: &ClassLikeMemberSelector<'_>, list: &mago_syntax::cst::ArgumentList<'_>, span: Span) -> CallExpr {
    let receiver = trace_static_callee(class, selector);
    let LoweredArgs { args, named_args, has_spread, positional_only, arg_conds } =
        lower_argument_list(list);
    CallExpr { callee: None, callee_ref: None, receiver, args, named_args, has_spread, positional_only, span, arg_conds }
}

/// Lower a method/static call written in **value** position to
/// [`ArgValue::MethodCall`] (issue #386), or [`ArgValue::Other`] when the callee
/// is one no resolution reaches ([`Callee::Dynamic`]) or the argument list
/// carries a **spread** — whose positional prefix is not the call that was
/// written, so claiming it would be claiming a different call.
fn method_call_arg_value(callee: Callee, list: &mago_syntax::cst::ArgumentList<'_>) -> ArgValue {
    if matches!(callee, Callee::Dynamic) {
        return ArgValue::Other;
    }
    let LoweredArgs { args, named_args, has_spread, .. } = lower_argument_list(list);
    if has_spread {
        return ArgValue::Other;
    }
    ArgValue::MethodCall {
        callee,
        args: args.into_iter().map(|a| a.value).collect(),
        named: named_args,
    }
}

/// Lower a **method first-class callable** `$o->m(...)` into a reference-"call": a
/// [`CallExpr`] with no positional arguments (`positional_only = false`), so the
/// method-call reverse sweep (ADR-0043 §6) treats it as an unenumerable caller and
/// taints the method rather than promoting it. Receiver construction mirrors
/// [`lower_method_call`].
pub(crate) fn first_class_method_ref(
    object: &Expression<'_>,
    selector: &ClassLikeMemberSelector<'_>,
    span: Span,
) -> CallExpr {
    CallExpr {
        callee: None,
        callee_ref: None,
        receiver: trace_method_callee(object, selector, false),
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
pub(crate) fn first_class_static_ref(
    class: &Expression<'_>,
    selector: &ClassLikeMemberSelector<'_>,
    span: Span,
) -> CallExpr {
    CallExpr {
        callee: None,
        callee_ref: None,
        receiver: trace_static_callee(class, selector),
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
pub(crate) fn lower_construct_call(inst: &Instantiation<'_>) -> Option<CallExpr> {
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

/// Lower an expression to an [`ArgValue`] — the shared lowering for call arguments
/// and assignment right-hand sides. Recognizes literals, bare local variables
/// (`$x` → [`ArgValue::Var`]), and calls to a statically-named function
/// (`f(...)` → [`ArgValue::Call`]); everything else is [`ArgValue::Other`].
pub(crate) fn lower_arg_value(expr: &Expression<'_>) -> ArgValue {
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
        // (explicit name or `self`/`static`/`parent`); a dynamic class expr or
        // constant name (`Foo::{$x}`) lowers to `Other`. Unproven until the
        // inference layer reinterprets it against a resolved enum or literal
        // class-constant initializer.
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
        // A method / nullsafe-method / static call in value position (issue #386):
        // the statement vocabulary, carried verbatim. Receiver and static-class
        // lowering are the statement form's own (`trace_method_callee` /
        // `trace_static_callee`), so `$b->m()` written as an argument denotes
        // exactly what `$b->m();` written as a statement denotes.
        Expression::Call(Call::Method(mc)) => {
            method_call_arg_value(trace_method_callee(mc.object, &mc.method, false), &mc.argument_list)
        }
        Expression::Call(Call::NullSafeMethod(mc)) => {
            method_call_arg_value(trace_method_callee(mc.object, &mc.method, true), &mc.argument_list)
        }
        Expression::Call(Call::StaticMethod(sc)) => {
            method_call_arg_value(trace_static_callee(sc.class, &sc.method), &sc.argument_list)
        }
        // `new Foo(...)` — a construction rvalue carrying its class (exact-class env
        // tracking) plus its positional and named arguments (both feed the
        // promoted-property seed). Spread arguments are not represented.
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
        // `if ($x > 3)`. Structural like `.` and `??` above — the SAME `eval_cmp`
        // that decides a guard decides this one. Arithmetic, bitwise and logical
        // operators still widen to `Other` (Certainty discipline — an unimplemented
        // arm declines).
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
        // In an array-*element* position it collapses to `Other` instead (see
        // [`lower_array_elements`]) — an offset read is not a proven element value.
        Expression::ArrayAccess(aa) => ArgValue::OffsetRead {
            base: Box::new(lower_arg_value(aa.array)),
            key: Box::new(lower_arg_value(aa.index)),
        },
        // A bare global-constant fetch (`PREG_SET_ORDER`, `SOME_CONST`) in value
        // position (issue #168). `true`/`false`/`null` lex as literals, not this.
        // Carried with its qualification kind so a consumer can apply the
        // engine-constant discipline (issue #29's `PHP_VERSION_ID` rules).
        Expression::ConstantAccess(ca) => ArgValue::GlobalConst(name_ref(&ca.name)),
        // `isset(…)` in VALUE position (issue #579) — the twin of what issue #414
        // did for the condition side. **Total**: every operand lowers, the two
        // shapes this vocabulary spells to themselves and everything else to
        // `IssetOperand::Unmodelled`, so the expression never widens to `Other`.
        // Declining here would be the defect, not the safe side: `isset` returns a
        // `bool` whatever it tests, and `Other` answers `unknown`.
        //
        // `empty(…)` is NOT lowered here. `lower_cond` models it as `!isset(e) ||
        // !e`, whose second disjunct is a truthiness reading of the operand's
        // value — a question this carrier does not carry, so the construct keeps
        // its `Other` lowering until a slice asks it.
        Expression::Construct(Construct::Isset(iss)) => {
            ArgValue::Isset(iss.values.iter().map(|v| lower_isset_operand(v)).collect())
        }
        _ => ArgValue::Other,
    }
}

/// One operand of a value-position `isset(…)` (issue #579). Total — an operand
/// this vocabulary does not spell is [`IssetOperand::Unmodelled`], never a
/// refusal.
///
/// The key of an offset operand is deliberately **not** required to be a concrete
/// literal, unlike [`const_key_offset`]'s guard reading: A-G4 restricts the guard
/// to a literal key because a tag discrimination is a claim about a written key,
/// while this operand is resolved through the offset family's own key resolution,
/// which proves a variable key or declines it.
fn lower_isset_operand(expr: &Expression<'_>) -> IssetOperand {
    if let Some(var) = bare_var_name(expr) {
        return IssetOperand::Var(var);
    }
    if let Expression::ArrayAccess(aa) = expr.unparenthesized()
        && let Expression::Variable(Variable::Direct(dv)) = aa.array.unparenthesized()
    {
        return IssetOperand::Offset {
            var: strip_dollar(bytes_to_string(dv.name)),
            key: Box::new(lower_arg_value(aa.index)),
        };
    }
    IssetOperand::Unmodelled
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
                // A key the source does not spell as a literal is CARRIED now (issue
                // #336) rather than collapsing the whole literal — the walk can ask
                // what the key expression is even without knowing which key it lands
                // on. An unrepresentable key expression still collapses.
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
pub(crate) fn lower_array_key(expr: &Expression<'_>) -> Option<ArrayKey> {
    match lower_arg_value(expr) {
        ArgValue::Int(i) => Some(ArrayKey::Int(i)),
        ArgValue::Bool(b) => Some(ArrayKey::Int(i64::from(b))),
        ArgValue::Null => Some(ArrayKey::Str(PhpStr::new())),
        // A float key truncates toward zero — but only when the truncated value is
        // actually an `int`. Outside that range PHP does not produce a key at all:
        // it emits "The float … is not representable as an int, cast occurred" (a
        // WARNING, a proven runtime break under the abort posture), and the
        // resulting key is the C wraparound, which Rust's saturating `as` does not
        // reproduce — `9.2e18 as i64` is `i64::MAX` here, `i64::MIN` there. The
        // range test is load-bearing: without it this arm folds to the wrong value.
        // Reachable since issue #62 made an out-of-range integer literal a `Float`.
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

/// Whether a string is a PHP *canonical* decimal integer (the form array keys fold
/// to `int` on): round-trips exactly through `i64` (`"5"` → 5, but `"05"`, `"+5"`,
/// `" 5"`, `"-0"`, and out-of-range values stay strings).
///
/// Public so the offset-read side (ADR-0049 A10) canonicalizes a runtime string key
/// through the **same** primitive the write/lowering side uses, so `$a = [5 => 'x'];
/// $a["5"]` resolves to the present key 5.
#[must_use]
pub fn php_canonical_int_string(s: impl AsRef<[u8]>) -> Option<i64> {
    let s = std::str::from_utf8(s.as_ref()).ok()?;
    let i: i64 = s.parse().ok()?;
    (i.to_string() == s).then_some(i)
}

/// Lower an integer literal from its **source spelling** (issue #62).
///
/// PHP's lexer promotes an integer literal that does not fit `int` to `float`,
/// base-blind: decimal, `0x`, `0b`, `0o`, legacy-octal and underscore-separated
/// spellings all follow it. The decision is on the magnitude, which must come from
/// the text — see the call site for why the parser's `value` cannot answer it.
///
/// Three outcomes:
/// * fits `i64` → [`ArgValue::Int`], the overwhelmingly common case;
/// * fits `u64` but not `i64` → [`ArgValue::Float`], PHP's promotion;
/// * beyond `u64` → a decimal literal still converts exactly (Rust and PHP both
///   round the digit string to the nearest double, so `99999999999999999999` is
///   `1.0E+20` in both); any other base yields [`ArgValue::Other`] — converting a
///   hex/octal/binary literal wider than 64 bits would need big-integer arithmetic
///   for a spelling that essentially never occurs, so silence is a ceiling, not a
///   wrong value.
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
        // wrapped int (issue #62), for every base alike, so the test is on the
        // parsed value: casting `9223372036854775808` to `i64` would give the wrong
        // value, `i64::MIN`. `PHP_INT_MIN` has no integer-literal spelling at all —
        // it's written `-PHP_INT_MAX - 1`.
        //
        // The parser's own `value` is NOT usable for the overflow decision: it's a
        // `u64` that SATURATES, so `99999999999999999999` arrives as `u64::MAX` —
        // indistinguishable from `0xFFFFFFFFFFFFFFFF` and off PHP's `1.0E+20` by
        // three orders of magnitude. The spelling is re-read instead.
        Literal::Integer(li) => lower_int_literal(li.raw),
        Literal::Float(lf) => ArgValue::Float(lf.value.0),
        // The parser hands over the escape-decoded **bytes** (`"\xC0"` arrives as
        // `[0xC0]`), and a PHP string is a byte string, so they carry through
        // unchanged. Decoding them lossily here was issue #208: it made `"\xC0"`
        // and `"\xD0"` the same value everywhere downstream.
        Literal::String(ls) => {
            ls.value.map_or(ArgValue::Other, |bytes| ArgValue::Str(PhpStr::from_bytes(bytes)))
        }
        Literal::True(_) => ArgValue::Bool(true),
        Literal::False(_) => ArgValue::Bool(false),
        Literal::Null(_) => ArgValue::Null,
    }
}

pub(crate) fn is_strict_types_one(item: &DeclareItem<'_>) -> bool {
    item.name.value == b"strict_types"
        && matches!(item.value, Expression::Literal(Literal::Integer(li)) if li.value == Some(1))
}

/// Lower a condition expression to a [`CondExpr`] (ADR-0031). Recognized:
/// `===`/`!==`/`==`/`!=` comparisons, `instanceof`, `!`/`&&`/`||` (incl. the
/// low-precedence `and`/`or`), and bare truthiness. Everything else becomes
/// [`CondExpr::Opaque`] carrying the variables it reads.
pub(crate) fn lower_cond(expr: &Expression<'_>) -> CondExpr {
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
        // `isset($x['k'])` (ADR-0062 S4) and bare `isset($x)` (issue #414). A
        // multi-argument isset is a conjunction by PHP semantics and lowers to the
        // matching `And` chain, each operand taking whichever form fits it. Anything
        // else — a property, a dynamic key — lowers to `Opaque`.
        //
        // The bare form is `CondExpr::IssetVar` rather than `Opaque` because `isset`
        // is a construct and cannot mutate what it tests: sending it to `Opaque`
        // charged its operand the by-reference conservatism an unmodellable
        // condition owes, which discarded every fact the variable had.
        Expression::Construct(Construct::Isset(iss)) => {
            let operands: Option<Vec<CondExpr>> = iss
                .values
                .iter()
                .map(|v| {
                    const_key_offset(v)
                        .map(|(var, key)| CondExpr::Isset { var, key: Box::new(key) })
                        .or_else(|| bare_var_name(v).map(|var| CondExpr::IssetVar { var }))
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

/// Lower a binary-operator condition (comparison / `instanceof` / `&&` / `||`).
fn lower_binary_cond(b: &Binary<'_>) -> CondExpr {
    let op = cmp_op_of(&b.operator);
    if let Some(op) = op {
        let lhs = lower_cond_operand(b.lhs);
        let rhs = lower_cond_operand(b.rhs);
        // Every comparison keeps its `Cmp` form, ordering included (issue #577).
        //
        // Ordering comparisons used to fall back to `Opaque` when either side was
        // unrepresentable, with `count()`/`sizeof()` excepted so the
        // shape-narrowing dispatcher could still read them (issue #272). That arm
        // was never a soundness requirement — since issue #158 a
        // `CondOperand::Other` carries what it may write — and its cost was that
        // an ORDERING comparison forgot every variable the condition mentions
        // while the identity spelling of the same test forgot nothing:
        // `strlen($s) > 0` erased `$s` where `strlen($s) === 0` kept it.
        //
        // Dropping it costs no refinement. `collect_cmp_refine` needs a
        // (variable, literal) pair and an opaque operand is not one, and
        // `operand_values` answers `None` for a `CondOperand::Other`, so such a
        // comparison evaluates `Maybe` either way. What it gains is that
        // invalidation runs through the operand rule — ADR-0070's by-value gate
        // and the pure-question exemption (#575) — instead of the read-set floor,
        // which is the same policy every other condition position already uses.
        //
        // The comment this replaces predicted one further change — that
        // `preg_match($re, $s, $m) > 0` would reach the out-parameter seed the
        // identity spelling reaches. Measured: it does not. The seed is gated
        // further in, so `=== 1` still seeds `$m` and `> 0` still does not, and
        // this arm's removal is exactly the invalidation change and nothing
        // else. Recorded here because the prediction was the reason the arm was
        // kept, and it was wrong.
        return CondExpr::Cmp { op, lhs, rhs };
    }
    match b.operator {
        BinaryOperator::Instanceof(_) => {
            // `operand instanceof Class` — the class is the rhs when a plain name.
            if let Expression::Identifier(id) = b.rhs.unparenthesized() {
                CondExpr::Instanceof { operand: lower_cond_operand(b.lhs), class_ref: name_ref(id) }
            } else {
                // A dynamic class (issue #571). `instanceof` is an operator and
                // writes neither side, so this must not become an `Opaque` whose
                // read set is an invalidation set. `reads` rides along unchanged
                // for `guard_chain_subject`, which is the field's other consumer.
                CondExpr::InstanceofDyn {
                    operand: lower_cond_operand(b.lhs),
                    class: lower_cond_operand(b.rhs),
                    reads: cond_reads(b.lhs),
                }
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

/// A bare `$var` and nothing else — the operand shape `isset($var)` tests
/// (issue #414). `None` for a property, an offset, a dynamic name, or any
/// expression that is not exactly one direct variable.
pub(crate) fn bare_var_name(expr: &Expression<'_>) -> Option<String> {
    match expr.unparenthesized() {
        Expression::Variable(Variable::Direct(dv)) => {
            Some(strip_dollar(bytes_to_string(dv.name)))
        }
        _ => None,
    }
}

/// `$var[<literal>]` — the depth-one constant-key projection ADR-0062 A-G4
/// scopes tag discrimination to. `None` for a non-variable base, a nested
/// access, or a key that is not a concrete literal.
pub(crate) fn const_key_offset(expr: &Expression<'_>) -> Option<(String, ArgValue)> {
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
pub(crate) fn const_key_offset_path(expr: &Expression<'_>) -> Option<(String, Vec<ArgValue>)> {
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
pub(crate) fn lower_cond_operand(expr: &Expression<'_>) -> CondOperand {
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
        // A class-constant / enum-case fetch (issue #429), recognized by the same
        // static-class path `lower_arg_value` uses; a dynamic class or constant
        // name falls through to `Other` as it always did.
        Expression::Access(Access::ClassConstant(cc)) => {
            match (trace_static_class(cc.class), class_const_name(&cc.constant)) {
                (Some(class), Some(name)) => CondOperand::ClassConst(class, name),
                _ => lower_cond_operand_other(expr.unparenthesized()),
            }
        }
        other => match lower_arg_value(other) {
            // A scalar literal, or a fully-concrete array literal — the latter lets a
            // `$x === []` / `$x === [1, 2]` guard narrow `$x` to a `Singleton` array
            // (ADR-0049 §7: the `=== []` branch is what proves offset 0 missing). A
            // non-concrete array (an element that is a `Var`/call/offset read) stays
            // `Other`, so nothing unproven is ever treated as a decided literal.
            v if v.is_concrete_value() => CondOperand::Literal(v),
            _ => lower_cond_operand_other(other),
        },
    }
}

/// The [`CondOperand::Other`] floor of [`lower_cond_operand`], with its
/// invalidation bookkeeping. The invalidation set is collected only when the
/// operand can write at all — `$o->p === 1` reads `$o` and rebinds nothing, and
/// forgetting there would be a precision loss with no soundness content
/// (issue #158).
fn lower_cond_operand_other(other: &Expression<'_>) -> CondOperand {
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
pub(crate) fn lower_opaque(s: &Statement<'_>) -> Stmt {
    let node = Node::Statement(s);
    let (writes, reads, poisons, may_return) = opaque_sets(&node);
    Stmt::lowered(StmtKind::Opaque { writes, reads, poisons, may_return }, Vec::new())
}

/// Compute an `Opaque` construct's `(writes, reads, poisons, may_return)` over its
/// subtree. `reads` is every direct variable mentioned that is not already a write
/// — including branch conditions — so a construct that branches on a variable and
/// early-returns invalidates the fall-through binding (soundness; see the
/// [`StmtKind::Opaque`] docs). Nested function-like bodies are not descended.
pub(crate) fn opaque_sets(node: &Node<'_, '_>) -> (Vec<String>, Vec<String>, bool, bool) {
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
/// [`StmtKind::Destructure`]. `None` when `lhs` is not a destructuring pattern, or
/// is one this lowering cannot read faithfully.
///
/// PHP's own key rule is the whole derivation: a positional element reads the next
/// auto index (a skipped hole `[, $b]` consumes its index without reading it), a
/// keyed element reads its own key, and a nested pattern reads the outer key AND
/// everything beneath it. Mixing the two spellings is a compile-time fatal in PHP,
/// so a mixed pattern is refused rather than given a derivation no runtime would
/// ever exercise.
pub(crate) fn destructure_reads(lhs: &Expression<'_>) -> Option<Vec<Vec<ArgValue>>> {
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
