//! The drop sites (ADR-0100 §7, issue #882): the places a value may be released
//! and its destructor run, with no call written. Which classes' values do run
//! user code is the resolver's question; this lowering only says what a drop
//! releases.
//!
//! **Subjects.** A drop has a subject when the frame can name the class of the
//! value released:
//!
//! * a body-local variable whose first write, in source order, is `new C`
//!   (anonymous classes included: the syntax layer reads their bodies), which
//!   remembers every `new` class it is later written with;
//! * a by-value, non-variadic, non-promoted parameter whose native hint names a
//!   class, as a bound ([`EffectRecv::Bound`]), even where the frame writes it;
//! * a `new C` whose value nothing keeps: a statement, a method call's
//!   receiver, or a call's argument. A `new` that is assigned, returned, stored,
//!   yielded, captured or put in an array literal escapes, and is no site here.
//!
//! A `new` of a class named by `self`, `static`, `parent` or a variable has no
//! subject: a recorded residue, as is everything else a frame holds (an array, a
//! call result, an untyped value).
//!
//! **Forms.** `unset($v)`, an assignment over `$v`, one scope-exit site per
//! subject at the end of the body, and the temporary's own expression. The first
//! write to a local drops nothing unless a loop runs it again, so it is no
//! reassignment outside a loop. A site carries one operand per class the
//! subject may hold, as a receiver: [`EffectRecv::ClassName`] for a named class
//! (exact), [`EffectRecv::Bound`], [`EffectRecv::SelfKw`] and [`EffectRecv::Parent`]
//! for a parameter's hint, `None` for a class the lowering already knows declares
//! a destructor.

use std::collections::BTreeMap;

use mago_span::HasSpan;
use mago_syntax::cst::{
    AnonymousClass, Argument, ArgumentList, Expression, FunctionLikeParameterList, Hint, Node,
    Variable,
};

use super::SiteScope;
use crate::ast::{
    ArgShape, EffectRecv, OperatorConstruct as C, OperatorFamily as F, SiteKind, SiteOrigin, Span,
};
use crate::lower_decl::scan_body;
use crate::lower_expr::instantiation_class;
use crate::names::name_ref;
use crate::{bytes_to_string, children, strip_dollar, to_span};

/// What one variable of a frame may hold when it is dropped.
#[derive(Debug, Default)]
pub(crate) struct DropSubject {
    /// The classes it may hold, one receiver per class ([`EffectRecv::ClassName`]
    /// for a `new`, the hint's bounds for a parameter, `None` for a class that
    /// declares a destructor itself).
    receivers: Vec<Option<EffectRecv>>,
    /// The start of the first write to a local when no loop and no `goto` can run
    /// it twice: it releases nothing, so it is not a reassignment. `None` for a
    /// parameter, which holds a value on entry.
    clean_first: Option<u32>,
}

/// The variables of a frame that may hold a value to drop, by name, in a
/// deterministic order.
pub(crate) type DropSubjects = BTreeMap<String, DropSubject>;

/// What a `new` expression is to the drop sites.
enum Created {
    /// Not an object creation this lowering names a class for.
    NotNew,
    /// An anonymous class with no destructor of its own and no parent: its value
    /// runs nothing when dropped.
    Harmless,
    /// The classes the drop may run a destructor of, one receiver each: `None`
    /// when the lowering already knows the value declares one.
    Class(Vec<Option<EffectRecv>>),
}

/// What `expr` creates when it is a `new`.
fn created(expr: &Expression<'_>) -> Created {
    match expr.unparenthesized() {
        Expression::Instantiation(inst) => match instantiation_class(inst) {
            Some(class) => Created::Class(vec![Some(EffectRecv::ClassName(class))]),
            None => Created::NotNew,
        },
        Expression::AnonymousClass(ac) => anonymous(ac),
        _ => Created::NotNew,
    }
}

/// An anonymous class's value: one declaring `__destruct`, or using a trait
/// whose body is not lowered (as the shard's table counts a trait user), runs
/// user code itself; one extending a class is that class for the chain's sake.
fn anonymous(ac: &AnonymousClass<'_>) -> Created {
    let body = scan_body(ac.members.iter());
    if body.declares_destructor || !body.used_traits.is_empty() {
        return Created::Class(vec![None]);
    }
    match ac.extends.as_ref().and_then(|e| e.types.iter().next()) {
        Some(parent) => Created::Class(vec![Some(EffectRecv::ClassName(name_ref(parent)))]),
        None => Created::Harmless,
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

/// The subjects of a frame: its parameters that name a class, and the locals the
/// body first writes with a `new`.
pub(crate) fn subjects<'a, 'arena: 'a>(
    params: &FunctionLikeParameterList<'_>,
    body: impl Iterator<Item = Node<'a, 'arena>>,
) -> DropSubjects {
    let mut walk = Collect::default();
    for p in params.parameters.iter() {
        let name = strip_dollar(bytes_to_string(p.variable.name));
        walk.params.push(name.clone());
        let by_value = !p.is_reference() && !p.is_variadic() && !p.is_promoted_property();
        if by_value && let Some(hint) = &p.hint {
            let mut receivers = Vec::new();
            hint_receivers(hint, &mut receivers);
            if !receivers.is_empty() {
                walk.subjects.insert(name, DropSubject { receivers, clean_first: None });
            }
        }
    }
    for node in body {
        walk.visit(&node);
    }
    if walk.goto {
        walk.subjects.values_mut().for_each(|s| s.clean_first = None);
    }
    walk.subjects.retain(|_, s| !s.receivers.is_empty());
    walk.subjects
}

#[derive(Default)]
struct Collect {
    subjects: DropSubjects,
    /// Every parameter, whatever its hint: a write to one is no local's first.
    params: Vec<String>,
    /// Locals whose first write is not a `new`.
    excluded: Vec<String>,
    loops: u32,
    goto: bool,
}

impl Collect {
    fn visit(&mut self, node: &Node<'_, '_>) {
        match node {
            Node::Assignment(a) if a.operator.is_assign() => self.assign(a.lhs, a.rhs, a.span()),
            Node::While(_) | Node::DoWhile(_) | Node::For(_) | Node::Foreach(_) => {
                self.loops += 1;
                children(node).iter().for_each(|c| self.visit(c));
                self.loops -= 1;
                return;
            }
            Node::Goto(_) | Node::Label(_) => self.goto = true,
            // Nested scopes are their own frames.
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
        children(node).iter().for_each(|c| self.visit(c));
    }

    /// A plain assignment `lhs = rhs`.
    fn assign(&mut self, lhs: &Expression<'_>, rhs: &Expression<'_>, span: mago_span::Span) {
        let Expression::Variable(Variable::Direct(dv)) = lhs.unparenthesized() else { return };
        let name = strip_dollar(bytes_to_string(dv.name));
        if name == "this" || self.params.contains(&name) || self.excluded.contains(&name) {
            return;
        }
        let created = created(rhs);
        if matches!(created, Created::NotNew) {
            if !self.subjects.contains_key(&name) {
                self.excluded.push(name);
            }
            return;
        }
        let first = !self.subjects.contains_key(&name);
        let subject = self.subjects.entry(name).or_default();
        if first {
            subject.clean_first = (self.loops == 0).then(|| to_span(span).start);
        }
        if let Created::Class(receivers) = created {
            for receiver in receivers {
                if !subject.receivers.contains(&receiver) {
                    subject.receivers.push(receiver);
                }
            }
        }
    }
}

/// A drop site at `span`.
fn site(
    sx: &SiteScope<'_>,
    span: Span,
    construct: C,
    receivers: &[Option<EffectRecv>],
) -> SiteOrigin {
    let kind = SiteKind::Operator {
        family: F::Drop,
        construct,
        receivers: receivers.to_vec(),
        member: None,
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
            if let Some(subject) = subject_of(a.lhs, sx)
                && subject.clean_first != Some(to_span(a.span()).start)
            {
                out.push(site(sx, to_span(a.span()), C::DropReassign, &subject.receivers));
            }
        }
        Node::Unset(u) => {
            for value in u.values.iter() {
                if let Some(subject) = subject_of(value, sx) {
                    out.push(site(sx, to_span(value.span()), C::DropUnset, &subject.receivers));
                }
            }
        }
        Node::ExpressionStatement(s) => temporary(s.expression, sx, out),
        Node::FunctionCall(c) => arguments(&c.argument_list, sx, out),
        Node::MethodCall(c) => {
            temporary(c.object, sx, out);
            arguments(&c.argument_list, sx, out);
        }
        Node::NullSafeMethodCall(c) => {
            temporary(c.object, sx, out);
            arguments(&c.argument_list, sx, out);
        }
        Node::StaticMethodCall(c) => arguments(&c.argument_list, sx, out),
        Node::Instantiation(i) => {
            if let Some(list) = &i.argument_list {
                arguments(list, sx, out);
            }
        }
        _ => {}
    }
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

/// A site for `expr` when it is a `new` whose value dies in the expression that
/// holds it.
fn temporary(expr: &Expression<'_>, sx: &SiteScope<'_>, out: &mut Vec<SiteOrigin>) {
    if let Created::Class(receivers) = created(expr) {
        let span = to_span(expr.unparenthesized().span());
        out.push(site(sx, span, C::DropTemporary, &receivers));
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
        out.push(site(&sx, end, C::DropScopeExit, &subject.receivers));
    }
}
