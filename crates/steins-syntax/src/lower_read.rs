//! The read positions of a statement (ADR-0104, issue #1050): every call, offset read,
//! property read and class-constant read its own expressions write, each at its own span
//! and with the construct that carries it.
//!
//! # Boundary
//!
//! The same four statement kinds [`string_context_sites`] reads — an expression statement,
//! `return`, and the two `echo` forms — plus a `static` initializer, because each is a
//! position where the walk's entry env is the env PHP evaluates the expression in. The scan
//! stops where [`scan_string_contexts`] stops and one construct further, each for the reason
//! its own scan gives:
//!
//! * a function-like (function, method, closure, arrow function, property hook) and a
//!   class-like (class, interface, trait, enum) is its own scope, lowered and judged
//!   separately; an anonymous class contributes its constructor arguments, which the
//!   enclosing statement evaluates, and none of its body;
//! * a `match` is not descended: a structured one is hoisted, each arm a statement of its own
//!   that carries its own positions, and an unstructured one is not a position the walk can
//!   claim is reached. Its subject belongs to the headers (a later slice).
//!
//! # Carriers
//!
//! A position is classified by the construct around it when it is lowered, not when it is
//! judged: the answer is a syntactic fact, and a consumer that had to rediscover it from the
//! lowered kind would have to re-derive the same table at every site. Constructs combine
//! by the order on [`Carrier`].
//!
//! # The rules
//!
//! One row per construct that writes, silences or short-circuits a read. "Inherited" is the
//! claim of the construct around it; a row says what the construct makes of each operand.
//! `tests/it/read_positions.rs` has a test per row.
//!
//! | Construct | Operand | Recorded as | PHP |
//! | --- | --- | --- | --- |
//! | `T = e` | `T` and its base levels | not recorded (write) | fetched for write, silent |
//! | | keys inside `T`, `e` | inherited, Plain keys | ordinary reads |
//! | `T op= e`, `++T`, `T--` | `T` | `CompoundRead` | read, then written: warns |
//! | | base levels of `T` | not recorded (write) | fetched for write |
//! | `T[] = e` | `T` and its base levels | not recorded (write) | `T` is created, silent |
//! | `[T1, k => T2] = e` | each target | not recorded (write) | silent |
//! | | each key `k` | Plain | ordinary read |
//! | `f(T)`, by-ref parameter | `T` and its base levels | `Argument` | the key or property is created: silent |
//! | | `position: None` | `Argument`, position unknown | the parameter is unknowable: a value-dependent family declines |
//! | `&T` | `T` and its base levels | not recorded (write) | reference taken: created, silent |
//! | `unset(T)` | the whole statement | not a scanned statement kind | silent for an absent key |
//! | `isset(T)`, `empty(T)` | offset and property levels | not recorded | silent |
//! | | class constant | `IssetLike` | throws |
//! | | keys inside `T` | Plain | ordinary reads |
//! | `T ?? e`, `T ??= e` | `T` | as `isset`, silent | silent |
//! | | `e` | `Conditional` | evaluated only when `T` is unset or null |
//! | `a ?-> b ...` | the receiver `a` | inherited | evaluated |
//! | | the rest of the chain: the member, its arguments, its keys, every later link | `Conditional` | none of it is evaluated when `a` is null |
//! | `a ?-> b::m(x)`, `(a ?-> b)::m(x)`, `a ?-> b::C` | the class operand | inherited | evaluated |
//! | | the call and its arguments | `Conditional` | a null class operand short-circuits them too |
//! | `a && b`, `a \|\| b`, `a and b`, `a or b` | `a` | inherited | evaluated |
//! | | `b` | `Conditional` | may not run |
//! | `c ? a : b` | `c` | inherited | evaluated |
//! | | `a`, `b` | `Conditional` | one of them runs |
//! | `c ?: b` | `c` | inherited | evaluated |
//! | | `b` | `Conditional` | runs only when `c` is falsy |
//! | `@e` | everything in `e` | `Suppressed` unless a weaker claim holds | warns and runs the handler |
//! | `match (s) { ... }` | the whole `match` | not descended | arms are statements of their own; conditions belong to the headers |
//! | `f(...)`, `$o->m(...)`, `C::m(...)` | the callable | kind `CallRef`, folded like a call | throws at creation if missing; no call runs |
//!
//! [`string_context_sites`]: crate::lower_stmt
//! [`scan_string_contexts`]: crate::lower_stmt::scan_string_contexts

use mago_span::HasSpan;
use mago_syntax::cst::{
    Access, ArgumentList, ArrayElement, Call, AssignmentOperator, BinaryOperator, Expression, Node,
    PartialArgument, PartialArgumentList, Statement, StaticItem, UnaryPrefixOperator, Variable,
};

use crate::ast::{ArgValue, Carrier, Key, Place, ReadKind, ReadPosition, Span};
use crate::lower_expr::{class_const_name, lower_arg_value, prop_fetch_of, trace_static_class};
use crate::{bytes_to_string, children, stack_guard, strip_dollar, to_span};

/// The read positions of one statement's **own** expressions, `None` when there are none.
pub(crate) fn read_positions_of(s: &Statement<'_>) -> Option<Vec<ReadPosition>> {
    let mut scan = Scan::default();
    match s {
        Statement::Expression(es) => scan.plain(es.expression),
        Statement::Return(r) => {
            if let Some(e) = r.value {
                scan.plain(e);
            }
        }
        Statement::Echo(e) => e.values.iter().for_each(|v| scan.plain(v)),
        Statement::EchoTag(e) => e.values.iter().for_each(|v| scan.plain(v)),
        Statement::Static(st) => {
            for item in st.items.iter() {
                if let StaticItem::Concrete(c) = item {
                    scan.plain(c.value);
                }
            }
        }
        _ => {}
    }
    scan.finish()
}

/// The read positions of one expression PHP evaluates in statement position (an arrow
/// function's body, a `get` hook's, a `match` arm's), `None` when there are none.
pub(crate) fn read_positions_of_expr(e: &Expression<'_>) -> Option<Vec<ReadPosition>> {
    let mut scan = Scan::default();
    scan.plain(e);
    scan.finish()
}

/// What encloses a node, as far as it is inherited by everything below it.
#[derive(Clone, Copy, Default)]
struct Ctx {
    /// Under an operand PHP evaluates only when something else holds.
    conditional: bool,
    /// Under `@`.
    suppressed: bool,
}

impl Ctx {
    fn conditional(self) -> Self {
        Ctx { conditional: true, ..self }
    }

    fn suppressed(self) -> Self {
        Ctx { suppressed: true, ..self }
    }
}

/// What the **innermost** construct makes of the node being scanned; [`resolve`] folds the
/// inherited [`Ctx`] in.
#[derive(Clone, Copy)]
enum Local {
    Plain,
    Argument { call: Option<u32>, position: Option<u32> },
    IssetLike,
    Write,
    CompoundRead,
}

impl Local {
    /// What the base of a chain makes of itself when the level above it is `self`: the level
    /// above is fetched for write, so a compound target's base is written, and every other
    /// carrier passes through unchanged.
    fn for_base(self) -> Local {
        match self {
            Local::CompoundRead => Local::Write,
            other => other,
        }
    }
}

/// The [`Carrier`] of a read under `local` and `ctx`: the weakest claim wins (see
/// [`Carrier`]). `None` for a read no consumer judges and so is not recorded: a write, and
/// an `isset`-like read unless `isset_recorded` (a class constant, which throws there).
fn resolve(local: Local, ctx: Ctx, isset_recorded: bool) -> Option<Carrier> {
    Some(match local {
        Local::Write => return None,
        Local::IssetLike if !isset_recorded => return None,
        Local::IssetLike => Carrier::IssetLike,
        _ if ctx.conditional => Carrier::Conditional,
        _ if ctx.suppressed => Carrier::Suppressed,
        Local::CompoundRead => Carrier::CompoundRead,
        Local::Argument { call, position } => Carrier::Argument { call, position },
        Local::Plain => Carrier::Plain,
    })
}

/// What a call is made on, as far as its operands go.
enum Operand<'a, 'arena> {
    /// The callee expression of `f(...)`, or the class of `new C(...)`.
    Callee(&'a Expression<'arena>),
    /// The object or class expression and the selector of a method or static call.
    Member(&'a Expression<'arena>, &'a mago_syntax::cst::ClassLikeMemberSelector<'arena>),
}

#[derive(Default)]
struct Scan {
    out: Vec<ReadPosition>,
}

impl Scan {
    fn finish(mut self) -> Option<Vec<ReadPosition>> {
        self.out.shrink_to_fit();
        (!self.out.is_empty()).then_some(self.out)
    }

    /// Record a position unless its carrier resolved to `None` (not judged by anyone).
    fn push(&mut self, kind: ReadKind, span: Span, carrier: Option<Carrier>) {
        if let Some(carrier) = carrier {
            self.out.push(ReadPosition { span, carrier, kind });
        }
    }

    fn plain(&mut self, e: &Expression<'_>) {
        self.expr(e, Ctx::default(), &Local::Plain);
    }

    fn expr(&mut self, e: &Expression<'_>, ctx: Ctx, local: &Local) {
        self.node(&Node::Expression(e), ctx, local);
    }

    /// A call is never a target, never silent: only an argument position changes it.
    fn call_local(local: &Local) -> Local {
        match local {
            Local::Argument { .. } => *local,
            _ => Local::Plain,
        }
    }

    fn node(&mut self, node: &Node<'_, '_>, ctx: Ctx, local: &Local) {
        if stack_guard::exhausted() {
            return;
        }
        match node {
            // A nested scope, and a `match` (see the module doc).
            Node::Function(_)
            | Node::Method(_)
            | Node::Closure(_)
            | Node::ArrowFunction(_)
            | Node::PropertyHook(_)
            | Node::Class(_)
            | Node::Interface(_)
            | Node::Trait(_)
            | Node::Enum(_)
            | Node::Match(_) => {}
            Node::AnonymousClass(ac) => {
                if let Some(list) = &ac.argument_list {
                    self.partial_args(list, None, ctx);
                }
            }
            // Wrappers that stand between an operand and its parent keep the parent's claim.
            Node::Expression(_)
            | Node::Parenthesized(_)
            | Node::Access(_)
            | Node::Call(_)
            | Node::PartialApplication(_) => self.descend(node, ctx, local),
            Node::ArrayAccess(_)
            | Node::PropertyAccess(_)
            | Node::NullSafePropertyAccess(_)
            | Node::ClassConstantAccess(_) => self.access(node, ctx, local),
            Node::FunctionCall(_)
            | Node::MethodCall(_)
            | Node::NullSafeMethodCall(_)
            | Node::StaticMethodCall(_)
            | Node::Instantiation(_) => self.call(node, ctx, &Self::call_local(local)),
            Node::FunctionPartialApplication(_)
            | Node::MethodPartialApplication(_)
            | Node::StaticMethodPartialApplication(_) => self.first_class_callable(node, ctx),
            Node::Binary(_) | Node::Conditional(_) | Node::Assignment(_) => {
                self.operator(node, ctx);
            }
            Node::UnaryPrefix(_) | Node::UnaryPostfix(_) => self.unary(node, ctx),
            Node::IssetConstruct(i) => {
                i.values.iter().for_each(|v| self.expr(v, ctx, &Local::IssetLike));
            }
            Node::EmptyConstruct(e) => self.expr(e.value, ctx, &Local::IssetLike),
            // `T[] = e` creates `T`: its levels are fetched for write, never read.
            Node::ArrayAppend(aa) => self.base(aa.array, ctx, &Local::Write),
            _ => self.descend(node, ctx, &Local::Plain),
        }
    }

    fn descend(&mut self, node: &Node<'_, '_>, ctx: Ctx, local: &Local) {
        for child in children(node) {
            self.node(&child, ctx, local);
        }
    }

    /// The base of an access chain: it inherits the level above's claim when it is itself a
    /// level of the chain, and is an ordinary operand otherwise (`f()['k']` evaluates the
    /// call plainly, whatever the access is under).
    fn base(&mut self, e: &Expression<'_>, ctx: Ctx, local: &Local) {
        let chain = matches!(
            e.unparenthesized(),
            Expression::ArrayAccess(_)
                | Expression::Access(Access::Property(_) | Access::NullSafeProperty(_))
        );
        self.expr(e, ctx, &if chain { (*local).for_base() } else { Local::Plain });
    }

    fn access(&mut self, node: &Node<'_, '_>, ctx: Ctx, local: &Local) {
        match node {
            Node::ArrayAccess(aa) => {
                // Past a `?->` below, the access and its key run only for a non-null receiver.
                let inner = past_nullsafe(aa.array, ctx);
                // Not recorded when no consumer judges it, so the place is not built either.
                if let Some(carrier) = resolve(*local, inner, false) {
                    let kind = ReadKind::Offset(Box::new((place_of(aa.array), key_of(aa.index))));
                    self.push(kind, to_span(aa.span()), Some(carrier));
                }
                self.base(aa.array, ctx, local);
                self.expr(aa.index, inner, &Local::Plain);
            }
            Node::PropertyAccess(pa) => {
                let inner = past_nullsafe(pa.object, ctx);
                if let Some(carrier) = resolve(*local, inner, false)
                    && let Some(names) = prop_fetch_of(pa.object, &pa.property)
                {
                    let span = to_span(pa.span());
                    self.push(ReadKind::Prop(Box::new(names)), span, Some(carrier));
                }
                self.base(pa.object, ctx, local);
                self.descend(&Node::ClassLikeMemberSelector(&pa.property), inner, &Local::Plain);
            }
            // The member of a `?->` is itself evaluated only for a non-null receiver.
            Node::NullSafePropertyAccess(pa) => {
                self.base(pa.object, ctx, local);
                let inner = ctx.conditional();
                self.descend(&Node::ClassLikeMemberSelector(&pa.property), inner, &Local::Plain);
            }
            Node::ClassConstantAccess(cc) => {
                if let (Some(class), Some(name)) =
                    (trace_static_class(cc.class), class_const_name(&cc.constant))
                    && !name.eq_ignore_ascii_case("class")
                {
                    let span = to_span(cc.span());
                    let kind = ReadKind::ClassConst(Box::new((class, name)));
                    self.push(kind, span, resolve(*local, ctx, true));
                }
                self.expr(cc.class, ctx, &Local::Plain);
            }
            _ => {}
        }
    }

    fn call(&mut self, node: &Node<'_, '_>, ctx: Ctx, local: &Local) {
        let (span, operand, args) = match node {
            Node::FunctionCall(fc) => {
                (fc.span(), Operand::Callee(fc.function), Some(&fc.argument_list))
            }
            Node::MethodCall(mc) => {
                (mc.span(), Operand::Member(mc.object, &mc.method), Some(&mc.argument_list))
            }
            Node::NullSafeMethodCall(mc) => {
                (mc.span(), Operand::Member(mc.object, &mc.method), Some(&mc.argument_list))
            }
            Node::StaticMethodCall(sc) => {
                (sc.span(), Operand::Member(sc.class, &sc.method), Some(&sc.argument_list))
            }
            Node::Instantiation(inst) => {
                (inst.span(), Operand::Callee(inst.class), inst.argument_list.as_ref())
            }
            _ => return,
        };
        // A `?->` call, and any call further along a chain that has one, runs, with its
        // arguments, only for a non-null receiver.
        let inner = match node {
            Node::NullSafeMethodCall(_) => ctx.conditional(),
            Node::MethodCall(mc) => past_nullsafe(mc.object, ctx),
            Node::StaticMethodCall(sc) => past_nullsafe(sc.class, ctx),
            _ => ctx,
        };
        // Recorded by span alone and before its operands, so an argument can name it by index.
        let index = u32::try_from(self.out.len()).ok();
        self.push(ReadKind::Call, to_span(span), resolve(*local, inner, false));
        match operand {
            // A plain function or class name is no operand; a computed one is.
            Operand::Callee(Expression::Identifier(_)) => {}
            Operand::Callee(e) => self.expr(e, ctx, &Local::Plain),
            Operand::Member(object, selector) => {
                self.expr(object, ctx, &Local::Plain);
                self.descend(&Node::ClassLikeMemberSelector(selector), inner, &Local::Plain);
            }
        }
        if let Some(list) = args {
            self.args(list, index, inner);
        }
    }

    /// Each argument of a call, as an operand of it: the position among the written
    /// arguments while no named argument or spread has come before.
    fn args(&mut self, list: &ArgumentList<'_>, call: Option<u32>, ctx: Ctx) {
        let mut bound = true;
        for (i, arg) in list.arguments.iter().enumerate() {
            let spread = arg.is_unpacked();
            let named = matches!(arg, mago_syntax::cst::Argument::Named(_));
            let position = (bound && !spread && !named).then_some(i as u32);
            bound &= !spread && !named;
            let local = if spread {
                Local::Plain
            } else {
                Local::Argument { call, position }
            };
            self.expr(arg.value(), ctx, &local);
        }
    }

    /// The same for the constructor arguments of an anonymous class.
    fn partial_args(&mut self, list: &PartialArgumentList<'_>, call: Option<u32>, ctx: Ctx) {
        let mut bound = true;
        for (i, arg) in list.arguments.iter().enumerate() {
            let (value, spread, named) = match arg {
                PartialArgument::Positional(p) => (p.value, p.ellipsis.is_some(), false),
                PartialArgument::Named(n) => (n.value, false, true),
                _ => continue,
            };
            let position = (bound && !spread && !named).then_some(i as u32);
            bound &= !spread && !named;
            let local = if spread {
                Local::Plain
            } else {
                Local::Argument { call, position }
            };
            self.expr(value, ctx, &local);
        }
    }

    /// `f(...)`, `$o->m(...)`, `C::m(...)`: a reference to the callee, whose existence PHP
    /// checks at creation. A partial application with placeholders is not one.
    fn first_class_callable(&mut self, node: &Node<'_, '_>, ctx: Ctx) {
        let (span, object, selector) = match node {
            Node::FunctionPartialApplication(fpa)
                if fpa.argument_list.is_first_class_callable() =>
            {
                (fpa.span(), None, None)
            }
            Node::MethodPartialApplication(mpa)
                if mpa.argument_list.is_first_class_callable() =>
            {
                (mpa.span(), Some(mpa.object), Some(&mpa.method))
            }
            Node::StaticMethodPartialApplication(spa)
                if spa.argument_list.is_first_class_callable() =>
            {
                (spa.span(), Some(spa.class), Some(&spa.method))
            }
            _ => {
                self.descend(node, ctx, &Local::Plain);
                return;
            }
        };
        let inner = object.map_or(ctx, |o| past_nullsafe(o, ctx));
        if let Some(object) = object {
            self.expr(object, ctx, &Local::Plain);
        }
        if let Some(selector) = selector {
            self.descend(&Node::ClassLikeMemberSelector(selector), inner, &Local::Plain);
        }
        self.push(ReadKind::CallRef, to_span(span), resolve(Local::Plain, inner, false));
    }

    /// The binary, ternary and assignment operators: the ones that decide which of their
    /// operands are conditional, silent or targets.
    fn operator(&mut self, node: &Node<'_, '_>, ctx: Ctx) {
        let plain = Local::Plain;
        match node {
            Node::Binary(b) => match b.operator {
                BinaryOperator::And(_)
                | BinaryOperator::Or(_)
                | BinaryOperator::LowAnd(_)
                | BinaryOperator::LowOr(_) => {
                    self.expr(b.lhs, ctx, &plain);
                    self.expr(b.rhs, ctx.conditional(), &plain);
                }
                BinaryOperator::NullCoalesce(_) => {
                    self.expr(b.lhs, ctx, &Local::IssetLike);
                    self.expr(b.rhs, ctx.conditional(), &plain);
                }
                _ => {
                    self.expr(b.lhs, ctx, &plain);
                    self.expr(b.rhs, ctx, &plain);
                }
            },
            // `c ? a : b` evaluates one arm, `c ?: b` evaluates `b` only when `c` is falsy.
            Node::Conditional(c) => {
                self.expr(c.condition, ctx, &plain);
                if let Some(then) = c.then {
                    self.expr(then, ctx.conditional(), &plain);
                }
                self.expr(c.r#else, ctx.conditional(), &plain);
            }
            Node::Assignment(a) => {
                let (local, rhs_ctx) = match a.operator {
                    AssignmentOperator::Assign(_) => (Local::Write, ctx),
                    AssignmentOperator::Coalesce(_) => (Local::IssetLike, ctx.conditional()),
                    _ => (Local::CompoundRead, ctx),
                };
                self.target(a.lhs, ctx, &local);
                self.expr(a.rhs, rhs_ctx, &plain);
            }
            _ => {}
        }
    }

    /// An assignment target: a destructuring pattern writes each of its elements and reads
    /// its keys; anything else is one lvalue.
    fn target(&mut self, e: &Expression<'_>, ctx: Ctx, local: &Local) {
        let elements = match e.unparenthesized() {
            Expression::Array(a) if matches!(local, Local::Write) => Some(&a.elements),
            Expression::List(l) if matches!(local, Local::Write) => Some(&l.elements),
            Expression::LegacyArray(l) if matches!(local, Local::Write) => Some(&l.elements),
            _ => None,
        };
        let Some(elements) = elements else {
            self.expr(e, ctx, local);
            return;
        };
        for element in elements.iter() {
            match element {
                ArrayElement::KeyValue(kv) => {
                    self.expr(kv.key, ctx, &Local::Plain);
                    self.target(kv.value, ctx, local);
                }
                ArrayElement::Value(v) => self.target(v.value, ctx, local),
                ArrayElement::Variadic(_) | ArrayElement::Missing(_) => {}
            }
        }
    }

    fn unary(&mut self, node: &Node<'_, '_>, ctx: Ctx) {
        match node {
            Node::UnaryPrefix(u) => match u.operator {
                UnaryPrefixOperator::ErrorControl(_) => {
                    self.expr(u.operand, ctx.suppressed(), &Local::Plain);
                }
                UnaryPrefixOperator::Reference(_) => self.expr(u.operand, ctx, &Local::Write),
                UnaryPrefixOperator::PreIncrement(_) | UnaryPrefixOperator::PreDecrement(_) => {
                    self.expr(u.operand, ctx, &Local::CompoundRead);
                }
                _ => self.expr(u.operand, ctx, &Local::Plain),
            },
            Node::UnaryPostfix(u) => self.expr(u.operand, ctx, &Local::CompoundRead),
            _ => {}
        }
    }
}

/// `ctx` for the operands of a chain link whose base is `base`: conditional when a `?->`
/// lies below, because PHP then evaluates nothing further along the chain (its keys, its
/// arguments, its later links) once the receiver is null.
fn past_nullsafe(base: &Expression<'_>, ctx: Ctx) -> Ctx {
    if has_nullsafe(base) { ctx.conditional() } else { ctx }
}

/// Whether the chain `e` (property, offset and method links) contains a `?->`.
fn has_nullsafe(e: &Expression<'_>) -> bool {
    match e.unparenthesized() {
        Expression::Access(Access::NullSafeProperty(_))
        | Expression::Call(Call::NullSafeMethod(_)) => true,
        Expression::Access(Access::Property(pa)) => has_nullsafe(pa.object),
        Expression::ArrayAccess(aa) => has_nullsafe(aa.array),
        Expression::Call(Call::Method(mc)) => has_nullsafe(mc.object),
        // The class operand of `X::m()`, `X::$p` and `X::C` is evaluated first and
        // short-circuits the same way: `$n?->s::m($a['k'])`, `($n?->p)::m(...)`.
        Expression::Call(Call::StaticMethod(sc)) => has_nullsafe(sc.class),
        Expression::Access(Access::StaticProperty(sp)) => has_nullsafe(sp.class),
        Expression::Access(Access::ClassConstant(cc)) => has_nullsafe(cc.class),
        _ => false,
    }
}

/// The place an offset read's base names: a variable, or an offset of one.
fn place_of(base: &Expression<'_>) -> Place {
    match base.unparenthesized() {
        Expression::Variable(Variable::Direct(dv)) => {
            Place::Var(strip_dollar(bytes_to_string(dv.name)))
        }
        Expression::ArrayAccess(aa) => {
            Place::Index(Box::new((place_of(aa.array), key_of(aa.index))))
        }
        _ => Place::Other,
    }
}

/// The key of an offset read when it is an integer or string literal (a signed one included)
/// or a bare variable.
fn key_of(key: &Expression<'_>) -> Key {
    match key.unparenthesized() {
        Expression::Variable(Variable::Direct(dv)) => Key::Var(strip_dollar(bytes_to_string(dv.name))),
        Expression::Literal(_) | Expression::UnaryPrefix(_) => match lower_arg_value(key) {
            ArgValue::Int(i) => Key::Int(i),
            ArgValue::Str(s) => Key::Str(s),
            _ => Key::Other,
        },
        _ => Key::Other,
    }
}

