//! The call-like sites: function calls, method and static calls, `new`, and an
//! anonymous class's `new`.

use mago_span::HasSpan;
use mago_syntax::cst::{
    AnonymousClass, ClassLikeMemberSelector, Expression, FunctionCall, Instantiation, MethodCall,
    NullSafeMethodCall, StaticMethodCall,
};

use super::SiteScope;
use crate::ast::{ArgShape, DynamicSite, SiteKind, SiteOrigin, Span};
use crate::lower_arg_shape::{arg_shapes_of, method_call_shapes};
use crate::lower_effect::{
    AnonymousConstructor, anonymous_class_constructor, arg_targets_of_call, const_args_of_call,
    direct_var_callee, higher_order_of_call,
};
use crate::lower_expr::{
    effect_recv_of_class, effect_recv_of_object_declared, method_name_of, trace_static_class,
};
use crate::names::name_ref;
use crate::to_span;

/// A `f(...)` call: a plain call, a higher-order call, a call of a body-local
/// callback, or a dynamic call.
pub(super) fn function_call(fc: &FunctionCall<'_>, sx: &SiteScope<'_>, out: &mut Vec<SiteOrigin>) {
    let cx = sx.cx;
    if let Expression::Identifier(id) = fc.function {
        // A plain call is anchored at its name, a higher-order call at the whole
        // call, as the legacy origins were.
        let (span, callbacks) = match higher_order_of_call(fc) {
            Some((_, callbacks, _)) => (to_span(fc.span()), callbacks),
            None => (to_span(id.span()), Vec::new()),
        };
        let ref_targets = arg_targets_of_call(fc, cx);
        // `derive` reads a higher-order call's arity off `ref_targets`.
        debug_assert!(callbacks.is_empty() || ref_targets.is_some());
        let mut site = sx.site(span, SiteKind::Call { name: name_ref(id), callbacks });
        site.ref_targets = ref_targets;
        site.const_args = const_args_of_call(fc);
        site.operands = arg_shapes_of(&fc.argument_list, cx);
        out.push(site);
    } else {
        let var = direct_var_callee(fc);
        if let Some(cbref) = var.as_ref().and_then(|v| cx.locals.get(v).cloned()) {
            // `$fn()` resolved to a body-local single-assignment closure.
            out.push(sx.site(to_span(fc.span()), SiteKind::Callback { cbref }));
        } else {
            // A dynamic function call (`$f()`, `($cb)()`) — unprovable. The callee's
            // name travels when it is a parameter nothing rebinds.
            let var = var.filter(|v| cx.bindings.unrebound_param(v));
            out.push(sx.site(to_span(fc.span()), SiteKind::Dynamic(DynamicSite::Call { var })));
        }
    }
}

/// The site of an instance method call on `object` selecting `selector`;
/// `operands` reads the argument shapes where the call records them.
fn instance_call(
    object: &Expression<'_>,
    selector: &ClassLikeMemberSelector<'_>,
    span: Span,
    sx: &SiteScope<'_>,
    operands: impl FnOnce() -> Option<Vec<ArgShape>>,
) -> SiteOrigin {
    match (effect_recv_of_object_declared(object, sx.cx), method_name_of(selector)) {
        (Some(receiver), Some(method)) => {
            let mut site = sx.site(span, SiteKind::MethodCall { receiver, method });
            site.operands = operands();
            site
        }
        // `$var->m()` / `$o->$m()` — receiver or selector not resolvable.
        _ => sx.site(span, SiteKind::Dynamic(DynamicSite::MethodCall)),
    }
}

/// A `$o->m(...)` call.
pub(super) fn method_call(mc: &MethodCall<'_>, sx: &SiteScope<'_>, out: &mut Vec<SiteOrigin>) {
    let shapes = || method_call_shapes(mc.object, &mc.argument_list, sx.cx);
    out.push(instance_call(mc.object, &mc.method, to_span(mc.span()), sx, shapes));
}

/// A `$o?->m(...)` call, whose arguments record no shapes.
pub(super) fn nullsafe_method_call(
    mc: &NullSafeMethodCall<'_>,
    sx: &SiteScope<'_>,
    out: &mut Vec<SiteOrigin>,
) {
    out.push(instance_call(mc.object, &mc.method, to_span(mc.span()), sx, || None));
}

/// A `Foo::m(...)` call.
pub(super) fn static_method_call(
    sc: &StaticMethodCall<'_>,
    sx: &SiteScope<'_>,
    out: &mut Vec<SiteOrigin>,
) {
    let span = to_span(sc.span());
    if let (Some(receiver), Some(method)) =
        (effect_recv_of_class(sc.class), method_name_of(&sc.method))
    {
        let mut site = sx.site(span, SiteKind::MethodCall { receiver, method });
        site.operands = method_call_shapes(sc.class, &sc.argument_list, sx.cx);
        out.push(site);
    } else {
        // `$var::m()` / `static::m()` / `Foo::$m()` — unresolvable.
        out.push(sx.site(span, SiteKind::Dynamic(DynamicSite::StaticCall)));
    }
}

/// A `new C(...)`: the constructor it runs, or a dynamic site when the class is computed.
pub(super) fn instantiation(
    inst: &Instantiation<'_>,
    sx: &SiteScope<'_>,
    out: &mut Vec<SiteOrigin>,
) {
    let span = to_span(inst.span());
    match trace_static_class(inst.class) {
        Some(class) => {
            let mut site = sx.site(span, SiteKind::New { class });
            site.operands = match &inst.argument_list {
                Some(list) => arg_shapes_of(list, sx.cx),
                None => Some(Vec::new()),
            };
            out.push(site);
        }
        None => out.push(sx.site(span, SiteKind::Dynamic(DynamicSite::New))),
    }
}

/// The constructor a `new class(...) {...}` runs, as a site. Its arguments are
/// the caller's to walk.
pub(super) fn anonymous_class(
    ac: &AnonymousClass<'_>,
    sx: &SiteScope<'_>,
    out: &mut Vec<SiteOrigin>,
) {
    let span = to_span(ac.span());
    match anonymous_class_constructor(ac) {
        AnonymousConstructor::Unseen => {
            out.push(sx.site(span, SiteKind::Dynamic(DynamicSite::AnonymousClass)));
        }
        AnonymousConstructor::Inherited(class) => {
            out.push(sx.site(span, SiteKind::New { class }));
        }
        AnonymousConstructor::None => {}
    }
}
