//! Arity of a callback handed to an internal function that provably invokes it
//! (issue #1051 slice S5, from #937).
//!
//! The callee's required parameter count is compared with the number of arguments
//! the invoking function passes. One row per invoker (PHP 8.5.11 `php -r`-checked):
//!
//! ```text
//! invoker                       arguments the callback gets   provably runs when
//! call_user_func(cb, ...a)      n - 1                         always
//! array_map(cb, ...arrays)      the number of arrays          every array is a proven
//!                                                             array, one is non-empty
//! array_filter(arr, cb)         1                             arr is a non-empty array
//! array_filter(arr, cb, 2)      1   (ARRAY_FILTER_USE_KEY)    "
//! array_filter(arr, cb, 1)      2   (ARRAY_FILTER_USE_BOTH)   "
//! usort/uasort/uksort(a, cb)    2                             a has at least two elements
//! array_walk(a, cb)             2   (3 with an extra arg)     a is a non-empty array
//! array_reduce(arr, cb, init)   2                             arr is a non-empty array
//! preg_replace_callback         1                             declined: it runs only on a
//!                                                             match, which the project's
//!                                                             PCRE decides (a false negative)
//! ```
//!
//! A callback is one the S4 resolution proves: a closure or arrow function written
//! in place or held by a proven variable, a first-class callable of a function, or
//! a proven string naming a function (project or internal). `[$o, 'm']`, `'A::m'`,
//! an invokable object, a OneOf of closures and every unproven value decline.
//! Fewer parameters than arguments is fine, so is a default or a variadic: only
//! `required > invoked` is a finding. A callback naming an internal function that
//! would receive too MANY arguments (`array_walk($a, 'strtoupper')`) is not
//! judged. A flag that is not a literal `0`, `1` or `2` declines `array_filter`.
//! The finding is `call.too-few-arguments`, at the callback argument.

use std::collections::HashMap;

use steins_domain::PhpStr;
use steins_syntax::{
    ArgValue, CallExpr, Callee, ClosureRef, NameRef, Param, normalize_array,
};

use super::internal::{builtin_signature, required_of};
use super::{required_param_count, resolve_arity_function, string_callee_ref};
use crate::cx::Cx;
use crate::env::{ClosureTarget, Known, Stratum};
use crate::existence::global_function_callee;
use crate::fold::Folder;
use crate::project::Diagnostic;
use crate::version_region::in_version_dependent_region;
use crate::CALL_TOO_FEW_ARGUMENTS_ID;

/// A resolved callback signature: how many arguments it needs, and its name.
struct Sig {
    required: usize,
    display: String,
}

/// Where the callback sits in the argument list, and how many arguments the
/// invoker hands it.
struct Invocation {
    callback: usize,
    invoked: usize,
}

/// The length of the array a call argument provably is, at the `Verified` stratum.
fn array_len(
    cx: &Cx,
    folder: &mut dyn Folder,
    value: &ArgValue,
    env: &HashMap<String, Known>,
    poisoned: bool,
) -> Option<usize> {
    let (ArgValue::Array(items), Stratum::Verified) =
        cx.resolve_literal_strat(value, env, poisoned, folder)?
    else {
        return None;
    };
    normalize_array(&items).map(|n| n.len())
}

/// The `array_filter` flag's invoker argument count: absent or `0` is the value, `2`
/// (`ARRAY_FILTER_USE_KEY`) the key, `1` (`ARRAY_FILTER_USE_BOTH`) both.
fn filter_invoked(
    cx: &Cx,
    folder: &mut dyn Folder,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    poisoned: bool,
) -> Option<usize> {
    let Some(flag) = call.args.get(2) else { return Some(1) };
    match cx.resolve_literal(&flag.value, env, poisoned, folder)? {
        ArgValue::Int(0 | 2) => Some(1),
        ArgValue::Int(1) => Some(2),
        _ => None,
    }
}

/// The invocation a call provably makes of its callback, or `None` where the
/// callee is not an invoker above or the callback may never run.
fn invocation(
    cx: &Cx,
    folder: &mut dyn Folder,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    poisoned: bool,
) -> Option<Invocation> {
    let name = global_function_callee(cx, call)?.to_ascii_lowercase();
    let n = call.args.len();
    let len = |i: usize, f: &mut dyn Folder| {
        array_len(cx, f, &call.args.get(i)?.value, env, poisoned)
    };
    match name.as_str() {
        "call_user_func" if n >= 1 => Some(Invocation { callback: 0, invoked: n - 1 }),
        "array_map" if n >= 2 => {
            let lens: Option<Vec<usize>> = (1..n).map(|i| len(i, folder)).collect();
            (lens?.iter().any(|&l| l >= 1)).then_some(Invocation { callback: 0, invoked: n - 1 })
        }
        "array_filter" if (2..=3).contains(&n) => {
            (len(0, folder)? >= 1).then_some(())?;
            Some(Invocation { callback: 1, invoked: filter_invoked(cx, folder, call, env, poisoned)? })
        }
        "usort" | "uasort" | "uksort" if n == 2 => {
            (len(0, folder)? >= 2).then_some(Invocation { callback: 1, invoked: 2 })
        }
        "array_walk" if (2..=3).contains(&n) => {
            (len(0, folder)? >= 1).then_some(Invocation { callback: 1, invoked: n })
        }
        "array_reduce" if (2..=3).contains(&n) => {
            (len(0, folder)? >= 1).then_some(Invocation { callback: 1, invoked: 2 })
        }
        _ => None,
    }
}

/// A synthetic call to the function a reference names, so the function arms of the
/// arity ladder resolve it as they resolve any written call.
fn function_call(call: &CallExpr, r: &NameRef) -> CallExpr {
    CallExpr {
        callee: Some(r.raw.clone()),
        callee_ref: Some(r.clone()),
        receiver: Callee::Function(r.raw.clone()),
        args: Vec::new(),
        named_args: Vec::new(),
        has_spread: false,
        positional_only: true,
        span: call.span,
        arg_conds: Vec::new(),
        dynamic_callee: None,
    }
}

/// The signature of the function a reference names: a project function under the
/// ladder's own gates, else the internal function the analysing PHP reflects.
fn function_sig(cx: &Cx, folder: &mut dyn Folder, call: &CallExpr, r: &NameRef) -> Option<Sig> {
    let synth = function_call(call, r);
    if let Some(target) = resolve_arity_function(cx, folder, &synth) {
        return Some(Sig { required: required_param_count(target.params), display: target.display });
    }
    let (name, params) = builtin_signature(cx, folder, &synth)?;
    Some(Sig { required: required_of(&params), display: name })
}

fn closure_sig(cx: &Cx, def_offset: u32) -> Option<Sig> {
    let params: &[Param] = &cx.closure_scope(def_offset)?.params;
    Some(Sig { required: required_param_count(params), display: "closure".to_owned() })
}

/// A string callee: a function name, never `Class::method`.
fn string_sig(cx: &Cx, folder: &mut dyn Folder, call: &CallExpr, s: &PhpStr) -> Option<Sig> {
    let s = s.as_str()?;
    if s.contains("::") {
        return None;
    }
    function_sig(cx, folder, call, &string_callee_ref(s, call.span.start))
}

/// The callback a call argument provably is, under the S4 callee table.
fn callback_sig(
    cx: &Cx,
    folder: &mut dyn Folder,
    call: &CallExpr,
    value: &ArgValue,
    env: &HashMap<String, Known>,
) -> Option<Sig> {
    match value {
        ArgValue::Closure(ClosureRef::Anonymous { def_offset, .. }) => closure_sig(cx, *def_offset),
        ArgValue::Closure(ClosureRef::FunctionName(r)) => function_sig(cx, folder, call, r),
        ArgValue::Str(s) => string_sig(cx, folder, call, s),
        ArgValue::Var(name) => {
            let known = env.get(name)?;
            if let Some(cv) = &known.closure {
                return match &cv.target {
                    ClosureTarget::Scope(def_offset) => closure_sig(cx, *def_offset),
                    ClosureTarget::Named(r) => function_sig(cx, folder, call, r),
                };
            }
            match known.singleton()? {
                ArgValue::Str(s) if known.stratum == Stratum::Verified => {
                    string_sig(cx, folder, call, &s)
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// Judge the callback of one call to an invoking internal function. Called only from
/// the plain per-scope pass, like every arity arm.
pub(crate) fn check_callback_arity(
    cx: &Cx,
    folder: &mut dyn Folder,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    poisoned: bool,
    out: &mut Vec<Diagnostic>,
) {
    if poisoned || !call.positional_only || call.has_spread {
        return;
    }
    let Callee::Function(_) = &call.receiver else { return };
    // The invoker must be the engine's own function, not a project twin.
    if !builtin_known(cx, folder, call) {
        return;
    }
    let Some(inv) = invocation(cx, folder, call, env, poisoned) else { return };
    let Some(arg) = call.args.get(inv.callback) else { return };
    let Some(sig) = callback_sig(cx, folder, call, &arg.value, env) else { return };
    if sig.required <= inv.invoked {
        return;
    }
    if in_version_dependent_region(cx, call.span.start) {
        return;
    }
    let at = cx.tree().position(arg.span.start);
    out.push(Diagnostic {
        id: CALL_TOO_FEW_ARGUMENTS_ID,
        facet: None,
        fix: None,
        path: cx.path().to_owned(),
        line: at.line,
        column: at.column,
        message: format!(
            "too few arguments to {}() as a callback: {}() passes {}, {} required — provable ArgumentCountError",
            sig.display,
            call.callee.as_deref().unwrap_or("callee"),
            inv.invoked,
            sig.required,
        ),
    });
}

/// Whether the call names a global builtin the analysing PHP reflects.
fn builtin_known(cx: &Cx, folder: &mut dyn Folder, call: &CallExpr) -> bool {
    builtin_signature(cx, folder, call).is_some()
}
