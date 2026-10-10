//! The call-like sites: function calls, method and static calls, `new`, and an
//! anonymous class's `new`.

use mago_span::HasSpan;
use mago_syntax::cst::{
    AnonymousClass, Argument, ArgumentList, ClassLikeMemberSelector, Expression, FunctionCall,
    Instantiation, MethodCall, NullSafeMethodCall, StaticMethodCall,
};

use super::{SiteScope, coerce};
use crate::ast::{ArgShape, ConstArgs, DynamicSite, SiteKind, SiteOrigin, Span, StaticClass};
use crate::lower_arg_shape::{
    arg_shapes_of, float_evidence_of_args, method_call_shapes, not_text_of_args,
    null_evidence_of_args, rendered_evidence_of_args,
};
use crate::lower_effect::{
    AnonymousConstructor, anonymous_class_constructor, arg_targets_of_call, const_args_of_call,
    const_int_of, direct_var_callee, higher_order_of_call, literals_of_args, literals_of_call,
    pattern_list_of,
};
use crate::lower_expr::{
    effect_recv_of_class, effect_recv_of_object_declared, method_name_of, trace_static_class,
};
use crate::names::name_ref;
use crate::to_span;

/// The functions whose timestamp argument decides whether they read the clock (ADR-0101 §3.14).
const TIME_FAMILY: [&str; 9] = [
    "date",
    "idate",
    "gmdate",
    "gmmktime",
    "strtotime",
    "getdate",
    "localtime",
    "strftime",
    "gmstrftime",
];

/// The function spellings of the `DateTime` constructors (ADR-0101 §3.17): the string, or the
/// format, and the zone object decide whether they read the default zone and the clock.
const DATE_FACTORIES: [&str; 4] = [
    "date_create",
    "date_create_immutable",
    "date_create_from_format",
    "date_create_immutable_from_format",
];

/// The residue cell's gated functions (ADR-0101 §3.16): the bcmath calls whose `$scale` decides
/// the read, and the two that return the old value of the entry they may rewrite. The catalog's
/// `setting_read_gate` names the same set; the literal arguments show a `null` scale, and the null
/// evidence shows a non-`null` one.
const INI_RESIDUE: [&str; 12] = [
    "bcadd",
    "bccomp",
    "bcdiv",
    "bcdivmod",
    "bcmod",
    "bcmul",
    "bcpow",
    "bcpowmod",
    "bcscale",
    "bcsqrt",
    "bcsub",
    "error_reporting",
];

/// The functions that render a value through the `precision` or `serialize_precision` ini
/// (ADR-0101 §3.15): the catalog's `precision_gate` names the same set.
const PRECISION_RENDERERS: [&str; 10] = [
    "strval",
    "implode",
    "join",
    "print_r",
    "var_export",
    "json_encode",
    "serialize",
    "var_dump",
    "debug_zval_dump",
    "settype",
];

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
        let name = name_ref(id);
        let simple = name.simple().to_ascii_lowercase();
        let printf = ["sprintf", "printf"].contains(&simple.as_str());
        // `ini_set` renders its value as a string first: a float goes through `precision`.
        let ini_value = ["ini_set", "ini_alter"].contains(&simple.as_str());
        let ctype = simple.starts_with("ctype_");
        let mut site = sx.site(span, SiteKind::Call { name, callbacks });
        site.ref_targets = ref_targets;
        site.const_args = const_args_of_call(fc);
        if simple.starts_with("preg_") {
            // An array of patterns is the one literal shape `first` cannot hold (ADR-0101 §3.10).
            let keys = simple == "preg_replace_callback_array";
            site.const_args.patterns = pattern_list_of(fc, keys);
        }
        if ["mb_", "iconv", "html", "get_html"].iter().any(|prefix| simple.starts_with(prefix))
            || TIME_FAMILY.contains(&simple.as_str())
            || INI_RESIDUE.contains(&simple.as_str())
            || DATE_FACTORIES.contains(&simple.as_str())
        {
            // The encoding argument can sit at any position up to the fifth (ADR-0101 §3.13), and
            // so can a time-family timestamp or field (`gmmktime` takes six, §3.14).
            site.const_args.literals = literals_of_call(fc);
            if TIME_FAMILY.contains(&simple.as_str())
                || INI_RESIDUE.contains(&simple.as_str())
                || DATE_FACTORIES.contains(&simple.as_str())
            {
                site.const_args.timestamps = null_evidence_of_args(&fc.argument_list, cx);
            }
        }
        if printf || ini_value {
            site.const_args.float_evidence = float_evidence_of_args(&fc.argument_list, cx);
        }
        if PRECISION_RENDERERS.contains(&simple.as_str()) {
            site.const_args.rendered = rendered_evidence_of_args(&fc.argument_list, cx);
        }
        if ctype {
            // A predicate's text is at position 0: an integer there is a character code.
            if let Some(Argument::Positional(first)) = fc.argument_list.arguments.iter().next()
                && let Some(int) = const_int_of(first.value)
            {
                site.const_args.ints.insert(0, (0, int));
            }
            site.const_args.not_text = not_text_of_args(&fc.argument_list, cx);
        }
        site.operands = arg_shapes_of(&fc.argument_list, cx);
        site.args = coerce::call_args(&fc.argument_list, sx);
        out.push(site);
    } else {
        let var = direct_var_callee(fc);
        if let Some(cbref) = var.as_ref().and_then(|v| cx.locals.get(v).cloned()) {
            // `$fn()` resolved to a body-local single-assignment closure.
            let mut site = sx.site(to_span(fc.span()), SiteKind::Callback { cbref });
            site.args = coerce::call_args(&fc.argument_list, sx);
            out.push(site);
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
    (list, operands): (&ArgumentList<'_>, impl FnOnce() -> Option<Vec<ArgShape>>),
) -> SiteOrigin {
    match (effect_recv_of_object_declared(object, sx.cx), method_name_of(selector)) {
        (Some(receiver), Some(method)) => {
            let mut site = sx.site(span, SiteKind::MethodCall { receiver, method });
            site.operands = operands();
            site.args = coerce::call_args(list, sx);
            site
        }
        // `$var->m()` / `$o->$m()` — receiver or selector not resolvable.
        _ => sx.site(span, SiteKind::Dynamic(DynamicSite::MethodCall)),
    }
}

/// A `$o->m(...)` call.
pub(super) fn method_call(mc: &MethodCall<'_>, sx: &SiteScope<'_>, out: &mut Vec<SiteOrigin>) {
    let shapes = || method_call_shapes(mc.object, &mc.argument_list, sx.cx);
    let span = to_span(mc.span());
    out.push(instance_call(mc.object, &mc.method, span, sx, (&mc.argument_list, shapes)));
}

/// A `$o?->m(...)` call, whose arguments record no shapes.
pub(super) fn nullsafe_method_call(
    mc: &NullSafeMethodCall<'_>,
    sx: &SiteScope<'_>,
    out: &mut Vec<SiteOrigin>,
) {
    let span = to_span(mc.span());
    out.push(instance_call(mc.object, &mc.method, span, sx, (&mc.argument_list, || None)));
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
        let date_ctor = ["__construct", "createfromformat"]
            .contains(&method.to_ascii_lowercase().as_str());
        let mut site = sx.site(span, SiteKind::MethodCall { receiver, method });
        site.operands = method_call_shapes(sc.class, &sc.argument_list, sx.cx);
        site.args = coerce::call_args(&sc.argument_list, sx);
        if date_ctor {
            // `DateTime::createFromFormat(...)`, `parent::__construct(...)` (ADR-0101 §3.17).
            site.const_args = date_const_args(&sc.argument_list, sx);
        }
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
            let date = names_date_class(&class);
            let mut site = sx.site(span, SiteKind::New { class });
            site.operands = match &inst.argument_list {
                Some(list) => arg_shapes_of(list, sx.cx),
                None => Some(Vec::new()),
            };
            if let Some(list) = &inst.argument_list {
                site.args = coerce::call_args(list, sx);
                if date {
                    site.const_args = date_const_args(list, sx);
                }
            }
            out.push(site);
        }
        None => out.push(sx.site(span, SiteKind::Dynamic(DynamicSite::New))),
    }
}

/// Whether a `new` spells `DateTime` or `DateTimeImmutable` as its last segment: the classes
/// whose constructor's string and zone object decide its reads (ADR-0101 §3.17). The engine
/// resolves the name; a project class of that name, or an alias, reads the arguments for
/// nothing, and any other spelling records none and keeps the row.
fn names_date_class(class: &StaticClass) -> bool {
    let StaticClass::Named(name) = class else { return false };
    ["datetime", "datetimeimmutable"].contains(&name.simple().to_ascii_lowercase().as_str())
}

/// The literal arguments and the null evidence of a `DateTime` constructor-side call: the string
/// or format, and whether the zone object is shown passed (ADR-0101 §3.17).
fn date_const_args(list: &ArgumentList<'_>, sx: &SiteScope<'_>) -> ConstArgs {
    ConstArgs {
        literals: literals_of_args(list),
        timestamps: null_evidence_of_args(list, sx.cx),
        ..ConstArgs::default()
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
            let mut site = sx.site(span, SiteKind::New { class });
            if let Some(list) = &ac.argument_list {
                site.args = coerce::partial_call_args(list, sx);
            }
            out.push(site);
        }
        AnonymousConstructor::None => {}
    }
}
