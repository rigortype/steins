//! The drop sites (ADR-0100 §7, issue #882, widened by #1003): the places a value
//! may be released and its destructor run, with no call written. Which classes'
//! values do run user code is the resolver's question; this lowering only says
//! what a drop releases.
//!
//! **Subjects.** A drop has a subject when the frame can name the class of the
//! value released:
//!
//! * a body-local variable that some plain write stores a `new C` into, directly
//!   or through the arms of a ternary, a `match`, `??` or a `clone` (anonymous
//!   classes included: the syntax layer reads their bodies), which remembers every
//!   `new` class it is written with, wherever in the body the write is;
//! * a by-value, non-variadic, non-promoted parameter whose native hint names a
//!   class, as a bound ([`EffectRecv::Bound`]), even where the frame writes it, and
//!   any by-value parameter the body overwrites with a `new`;
//! * a `new C` whose value nothing keeps: a statement, a method call's receiver, a
//!   call's argument, or an operand the expression consumes (`clone`, a property or
//!   offset fetch, `echo`, a cast, a condition, `instanceof`, any binary operator but
//!   `??`). A `new` that is assigned, returned, stored, yielded, captured or put in
//!   an array literal escapes, and is no site here.
//!
//! A `new` of a class named by `self`, `static`, `parent` or a variable has no
//! subject: a recorded residue, as is everything else a frame holds (an array, a
//! call result, an untyped value).
//!
//! **Forms.** `unset($v)`, an assignment over `$v`, one scope-exit site per
//! subject at the end of the body, and the temporary's own expression. A write drops
//! nothing when no earlier write could have stored an object, outside a loop and in
//! a function with no `goto` (`$x = null; $x = new D;`): it is no reassignment. A
//! site carries one operand per class the subject may hold, as a receiver:
//! [`EffectRecv::ClassName`] for a named class (exact), [`EffectRecv::Bound`],
//! [`EffectRecv::SelfKw`] and [`EffectRecv::Parent`] for a parameter's hint, `None`
//! for a class the lowering already knows declares a destructor.
//!
//! **Properties** ([`property`]) are a fourth subject: a plain write to, or an
//! `unset` of, `$this->p` or a static property of a named class, which the
//! resolver reads by the property's declared hint.

mod collect;
mod property;

use std::collections::BTreeMap;

use mago_span::HasSpan;
use mago_syntax::cst::{
    AnonymousClass, Argument, ArgumentList, BinaryOperator, Expression, FunctionLikeParameterList,
    Hint, MatchArm, Node, UnaryPrefixOperator, Variable,
};

use super::SiteScope;
use crate::ast::{
    ArgShape, EffectRecv, NameRef, OperatorConstruct as C, OperatorFamily as F, SiteKind,
    SiteOrigin, Span,
};
use crate::lower_decl::scan_body;
use crate::lower_expr::instantiation_class;
use crate::names::name_ref;
use crate::{bytes_to_string, stack_guard, strip_dollar, to_span};

/// What one variable of a frame may hold when it is dropped.
#[derive(Debug, Default)]
pub(crate) struct DropSubject {
    /// The classes it may hold, one receiver per class ([`EffectRecv::ClassName`]
    /// for a `new`, the hint's bounds for a parameter, `None` for a class that
    /// declares a destructor itself).
    receivers: Vec<Option<EffectRecv>>,
    /// The starts of the plain writes to a local that release nothing: no earlier
    /// write could have stored an object, no loop and no `goto` can run it twice. A
    /// parameter holds a value on entry, so it has none.
    clean: Vec<u32>,
}

/// What a frame may drop: its variables that may hold a value to drop, by name in a
/// deterministic order, and the constructor writes that initialize a property.
#[derive(Debug, Default)]
pub(crate) struct DropSubjects {
    vars: BTreeMap<String, DropSubject>,
    /// The starts of the plain `$this->p = …` writes of a constructor body that are
    /// the first thing to touch `p` there ([`C::DropPropInit`]).
    init_writes: Vec<u32>,
}

impl DropSubjects {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn get(&self, name: &str) -> Option<&DropSubject> {
        self.vars.get(name)
    }

    fn values(&self) -> impl Iterator<Item = &DropSubject> {
        self.vars.values()
    }

    /// Whether the write starting at `start` is a constructor's first touch of its property.
    fn is_init_write(&self, start: u32) -> bool {
        self.init_writes.contains(&start)
    }
}

/// What the `new` expressions an expression may evaluate to are to the drop sites.
#[derive(Default)]
struct Made {
    /// Whether any is an object creation this lowering names a class for. An
    /// anonymous class with no destructor of its own and no parent counts, though its
    /// value runs nothing when dropped, so it adds no receiver.
    any: bool,
    /// The classes the drop may run a destructor of, one receiver each: `None` when the
    /// lowering already knows the value declares one.
    receivers: Vec<Option<EffectRecv>>,
}

impl Made {
    fn push(&mut self, receiver: Option<EffectRecv>) {
        if !self.receivers.contains(&receiver) {
            self.receivers.push(receiver);
        }
    }
}

/// Collect into `out` the `new` expressions `expr` may evaluate to: itself, the arms of
/// a ternary, a `match` or `??`, the operand of a `clone`, and, with `through_assign`,
/// the value of a nested assignment (`$y = $x = new D`). A statement's own assignment
/// is not one: its value is kept.
fn creations(expr: &Expression<'_>, through_assign: bool, out: &mut Made) {
    if stack_guard::exhausted() {
        return;
    }
    match expr.unparenthesized() {
        Expression::Instantiation(inst) => {
            if let Some(class) = instantiation_class(inst) {
                out.any = true;
                out.push(Some(EffectRecv::ClassName(class)));
            }
        }
        Expression::AnonymousClass(ac) => anonymous(ac, out),
        Expression::Conditional(c) => {
            // `a ?: b` evaluates to `a` when it is truthy.
            creations(c.then.unwrap_or(c.condition), through_assign, out);
            creations(c.r#else, through_assign, out);
        }
        Expression::Binary(b) if matches!(b.operator, BinaryOperator::NullCoalesce(_)) => {
            creations(b.lhs, through_assign, out);
            creations(b.rhs, through_assign, out);
        }
        Expression::Match(m) => {
            for arm in m.arms.iter() {
                let value = match arm {
                    MatchArm::Expression(a) => a.expression,
                    MatchArm::Default(a) => a.expression,
                };
                creations(value, through_assign, out);
            }
        }
        Expression::Clone(c) => creations(c.object, through_assign, out),
        // `(object)` and `@` hand the very value on.
        Expression::UnaryPrefix(u)
            if matches!(
                u.operator,
                UnaryPrefixOperator::ObjectCast(..) | UnaryPrefixOperator::ErrorControl(_)
            ) =>
        {
            creations(u.operand, through_assign, out);
        }
        Expression::Assignment(a) if through_assign && a.operator.is_assign() => {
            creations(a.rhs, through_assign, out);
        }
        _ => {}
    }
}

/// An anonymous class's value: one declaring `__destruct`, or aliasing an imported
/// trait's method as one, runs user code itself. Otherwise it is each trait it
/// imports and the class it extends, which the resolver reads as classes (a
/// trait's name answers whether the trait declares a destructor or cannot be
/// read); one with neither runs nothing.
fn anonymous(ac: &AnonymousClass<'_>, out: &mut Made) {
    out.any = true;
    let body = scan_body(ac.members.iter());
    if body.declares_destructor {
        out.push(None);
        return;
    }
    let parent = ac.extends.as_ref().and_then(|e| e.types.iter().next()).map(name_ref);
    // A trait is a bound, not a `new`: one no file declares records no unknown-class gap.
    for t in body.used_traits {
        out.push(Some(EffectRecv::Bound(t)));
    }
    if let Some(class) = parent {
        out.push(Some(EffectRecv::ClassName(class)));
    }
}

/// Whether `expr` stores no object into the variable it is assigned to: a literal, an
/// array (its elements are not the variable's value), an interpolated string, or an
/// operator that yields a scalar.
fn object_free(expr: &Expression<'_>) -> bool {
    match expr.unparenthesized() {
        Expression::Literal(_)
        | Expression::Array(_)
        | Expression::LegacyArray(_)
        | Expression::CompositeString(_) => true,
        Expression::UnaryPrefix(u) => {
            !matches!(
                u.operator,
                UnaryPrefixOperator::ObjectCast(..)
                    | UnaryPrefixOperator::ErrorControl(_)
                    | UnaryPrefixOperator::Reference(_)
            )
        }
        _ => false,
    }
}

/// The classes a native hint names, as bounds: a value of the class or of a
/// subclass. Every class member of a union, an intersection or a nullable counts
/// whatever else the hint holds (`array|D` names `D`); `self` and `parent` name the
/// enclosing class and its parent, which the resolver reads. `array`, `mixed`,
/// `object`, `iterable`, `callable` and the scalars name none.
fn hint_receivers(hint: &Hint<'_>, out: &mut Vec<Option<EffectRecv>>) {
    let mut push = |r: EffectRecv| {
        let r = Some(r);
        if !out.contains(&r) {
            out.push(r);
        }
    };
    match hint {
        Hint::Identifier(id) => push(EffectRecv::Bound(name_ref(id))),
        Hint::Self_(_) => push(EffectRecv::SelfKw),
        Hint::Parent(_) => push(EffectRecv::Parent),
        Hint::Nullable(n) => hint_receivers(n.hint, out),
        Hint::Parenthesized(p) => hint_receivers(p.hint, out),
        Hint::Union(u) => {
            hint_receivers(u.left, out);
            hint_receivers(u.right, out);
        }
        Hint::Intersection(i) => {
            hint_receivers(i.left, out);
            hint_receivers(i.right, out);
        }
        _ => {}
    }
}

/// What a property's native hint names: the classes, as written, by every `Identifier`
/// member of a union, an intersection or a nullable, whatever else the hint holds, and
/// whether a member is `self` or `parent`. The scalars, `array`, `mixed`, `object`,
/// `iterable` and `callable` are none.
#[derive(Default)]
pub(crate) struct HintClasses {
    pub(crate) names: Vec<NameRef>,
    pub(crate) has_self: bool,
    pub(crate) has_parent: bool,
}

impl HintClasses {
    pub(crate) fn read(&mut self, hint: &Hint<'_>) {
        match hint {
            Hint::Identifier(id) => self.names.push(name_ref(id)),
            Hint::Self_(_) => self.has_self = true,
            Hint::Parent(_) => self.has_parent = true,
            Hint::Nullable(n) => self.read(n.hint),
            Hint::Parenthesized(p) => self.read(p.hint),
            Hint::Union(u) => {
                self.read(u.left);
                self.read(u.right);
            }
            Hint::Intersection(i) => {
                self.read(i.left);
                self.read(i.right);
            }
            _ => {}
        }
    }
}

/// The subjects of a frame: its parameters that name a class, the parameters and
/// locals some write stores a `new` into, and, for a constructor, the writes that
/// initialize a property.
pub(crate) fn subjects<'a, 'arena: 'a>(
    params: &FunctionLikeParameterList<'_>,
    body: impl Iterator<Item = Node<'a, 'arena>>,
    constructor: bool,
) -> DropSubjects {
    let mut walk = collect::Collect::new(constructor);
    for p in params.parameters.iter() {
        let name = strip_dollar(bytes_to_string(p.variable.name));
        walk.add_param(name.clone(), p.is_reference());
        let by_value = !p.is_reference() && !p.is_variadic() && !p.is_promoted_property();
        if by_value && let Some(hint) = &p.hint {
            let mut receivers = Vec::new();
            hint_receivers(hint, &mut receivers);
            if !receivers.is_empty() {
                walk.add_subject(name, DropSubject { receivers, clean: Vec::new() });
            }
        }
    }
    for node in body {
        walk.visit(&node);
    }
    walk.finish()
}

/// A drop site at `span`.
fn site(
    sx: &SiteScope<'_>,
    span: Span,
    construct: C,
    receivers: &[Option<EffectRecv>],
    member: Option<String>,
) -> SiteOrigin {
    let kind = SiteKind::Operator {
        family: F::Drop,
        construct,
        receivers: receivers.to_vec(),
        member,
    };
    let mut site = sx.site(span, kind);
    site.operands = Some(vec![ArgShape::Unknown; receivers.len()]);
    site
}

/// The drop sites `node` is, if it is one: appended without consuming the node,
/// since the generic walk still has its children to visit.
pub(super) fn visit(node: &Node<'_, '_>, sx: &SiteScope<'_>, out: &mut Vec<SiteOrigin>) {
    match node {
        Node::Assignment(a) if a.operator.is_assign() => {
            let start = to_span(a.span()).start;
            if let Some(subject) = subject_of(a.lhs, sx)
                && !subject.clean.contains(&start)
            {
                let span = to_span(a.span());
                out.push(site(sx, span, C::DropReassign, &subject.receivers, None));
            }
            property::write(a, sx, out);
        }
        Node::Unset(u) => {
            for value in u.values.iter() {
                if let Some(subject) = subject_of(value, sx) {
                    let span = to_span(value.span());
                    out.push(site(sx, span, C::DropUnset, &subject.receivers, None));
                }
                property::unset(value, sx, out);
            }
        }
        Node::ExpressionStatement(s) => temporary(s.expression, sx, out),
        Node::FunctionCall(c) => {
            temporary(c.function, sx, out);
            arguments(&c.argument_list, sx, out);
        }
        Node::MethodCall(c) => {
            temporary(c.object, sx, out);
            arguments(&c.argument_list, sx, out);
        }
        Node::NullSafeMethodCall(c) => {
            temporary(c.object, sx, out);
            arguments(&c.argument_list, sx, out);
        }
        Node::StaticMethodCall(c) => {
            temporary(c.class, sx, out);
            arguments(&c.argument_list, sx, out);
        }
        Node::Instantiation(i) => {
            if let Some(list) = &i.argument_list {
                arguments(list, sx, out);
            }
        }
        _ => consumed(node, sx, out),
    }
}

/// The `new` operands a form consumes: its value is read and the object dies with the
/// expression, unless the form hands the very value on (`(object) new C`, `new C ?? x`,
/// the arms of a ternary), which the enclosing form then judges.
fn consumed(node: &Node<'_, '_>, sx: &SiteScope<'_>, out: &mut Vec<SiteOrigin>) {
    match node {
        Node::Clone(c) => temporary(c.object, sx, out),
        Node::PropertyAccess(p) => temporary(p.object, sx, out),
        Node::ClassConstantAccess(a) => temporary(a.class, sx, out),
        Node::StaticPropertyAccess(a) => temporary(a.class, sx, out),
        Node::For(f) => {
            let lists = [&f.initializations, &f.conditions, &f.increments];
            lists.iter().for_each(|l| l.iter().for_each(|e| temporary(e, sx, out)));
        }
        Node::NullSafePropertyAccess(p) => temporary(p.object, sx, out),
        Node::ArrayAccess(a) => temporary(a.array, sx, out),
        Node::Binary(b) if !matches!(b.operator, BinaryOperator::NullCoalesce(_)) => {
            temporary(b.lhs, sx, out);
            temporary(b.rhs, sx, out);
        }
        Node::UnaryPrefix(u) if consumes(&u.operator) => temporary(u.operand, sx, out),
        Node::Conditional(c) if c.then.is_some() => temporary(c.condition, sx, out),
        Node::Echo(e) => e.values.iter().for_each(|v| temporary(v, sx, out)),
        Node::EchoTag(e) => e.values.iter().for_each(|v| temporary(v, sx, out)),
        Node::PrintConstruct(p) => temporary(p.value, sx, out),
        Node::EmptyConstruct(e) => temporary(e.value, sx, out),
        Node::If(i) => temporary(i.condition, sx, out),
        Node::While(w) => temporary(w.condition, sx, out),
        Node::DoWhile(d) => temporary(d.condition, sx, out),
        Node::Switch(s) => temporary(s.expression, sx, out),
        Node::Match(m) => temporary(m.expression, sx, out),
        Node::Foreach(f) => temporary(f.expression, sx, out),
        _ => {}
    }
}

/// Whether a prefix operator reads its operand's value and yields another: every one
/// but `(object)`, which returns the same object, `@`, `&` and the increments.
const fn consumes(operator: &UnaryPrefixOperator<'_>) -> bool {
    !matches!(
        operator,
        UnaryPrefixOperator::ObjectCast(..)
            | UnaryPrefixOperator::ErrorControl(_)
            | UnaryPrefixOperator::Reference(_)
            | UnaryPrefixOperator::PreIncrement(_)
            | UnaryPrefixOperator::PreDecrement(_)
    )
}

/// The subject a bare `$v` names in this frame.
fn subject_of<'s>(expr: &Expression<'_>, sx: &'s SiteScope<'_>) -> Option<&'s DropSubject> {
    let Expression::Variable(Variable::Direct(dv)) = expr.unparenthesized() else { return None };
    sx.cx.drops.get(&strip_dollar(bytes_to_string(dv.name)))
}

/// Each argument of a call that is a `new` temporary.
fn arguments(list: &ArgumentList<'_>, sx: &SiteScope<'_>, out: &mut Vec<SiteOrigin>) {
    for argument in list.arguments.iter() {
        let value = match argument {
            Argument::Positional(p) if p.ellipsis.is_none() => p.value,
            Argument::Named(n) => n.value,
            Argument::Positional(_) => continue,
        };
        temporary(value, sx, out);
    }
}

/// A site for `expr` when it is, or may evaluate to, a `new` whose value dies in the
/// expression that holds it.
pub(super) fn temporary(expr: &Expression<'_>, sx: &SiteScope<'_>, out: &mut Vec<SiteOrigin>) {
    let mut made = Made::default();
    creations(expr, false, &mut made);
    if !made.receivers.is_empty() {
        let span = to_span(expr.unparenthesized().span());
        out.push(site(sx, span, C::DropTemporary, &made.receivers, None));
    }
}

/// The last byte of a body: where the scope-exit sites sit.
pub(crate) fn body_end(body: mago_span::Span) -> Span {
    let end = to_span(body).end;
    Span { start: end.saturating_sub(1), end }
}

/// The scope-exit sites of a frame: one per subject, at `end`, the end of its
/// body.
pub(crate) fn scope_exit_sites(cx: &crate::lower_effect::EffectScanCx, end: Span, out: &mut Vec<SiteOrigin>) {
    let sx = SiteScope { cx, guards: &[], catch_scope: &[] };
    for subject in cx.drops.values() {
        out.push(site(&sx, end, C::DropScopeExit, &subject.receivers, None));
    }
}
