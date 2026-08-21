//! Non-object receivers: the branch-sensitive `call.on-null` proof (ADR-0031) and
//! the `call.on-non-object` / `property.on-non-object` pair (ADR-0078 / issue #190)
//! — a member access whose receiver the value lane proves is null or a non-object
//! scalar on the current path.

use std::collections::HashMap;

use steins_domain::{Base, Fact, Val};
use steins_syntax::{CallExpr, Callee, Receiver, Span};

use crate::{
    CALL_ON_NON_OBJECT_ID, CALL_ON_NULL_ID, Cx, Diagnostic, Known, PROPERTY_ON_NON_OBJECT_ID, Store,
    Stratum, WalkCx,
};

/// The branch-sensitive null-dereference proof (ADR-0031, `call.on-null`): a
/// non-null-safe `$v->m(...)` whose receiver `$v` is proven `Singleton(null)` on
/// the current path is a guaranteed runtime `Error`. A `OneOf` that merely
/// includes null stays `Maybe` (silent), and `?->` never fires.
pub(crate) fn check_call_on_null(
    w: &WalkCx,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    store: &Store,
    out: &mut Vec<Diagnostic>,
) {
    if w.scope.poisoned {
        return;
    }
    let Callee::Method { receiver, method, nullsafe: false } = &call.receiver else {
        return;
    };
    let Some((fact, stratum, display)) = call_receiver_fact(receiver, env, store) else {
        return;
    };
    if !matches!(fact, Fact::Singleton(Val::Null)) {
        return;
    }
    // Proof-layer consumption rule (ADR-0052 §5): a receiver proven null only by an
    // `Asserted` fact (e.g. `@phpstan-assert null $x`) cannot premise this proof —
    // stay silent.
    if stratum != Stratum::Verified {
        return;
    }
    let pos = w.cx.tree().position(call.span.start);
    out.push(Diagnostic {
        id: CALL_ON_NULL_ID,
        facet: None,
        fix: None,
        path: w.cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        message: format!(
            "method call {display}->{method}() — {display} is proven null on this path — proven Error (Call to a member function on null)"
        ),
    });
}

// non-object receivers (ADR-0078, issue #190)

/// The receiver fact the call-site proofs consume: the value-domain fact behind a
/// method call's receiver, its trust stratum, and the receiver's rendering.
///
/// A bare `$v` reads the env value fact; a depth-1 `$v->prop` receiver reads the
/// allocation-keyed heap property fact (ADR-0052 §7, ADR-0036) — escaped-then-swept
/// props return `None`, a readonly prop survives the sweep. `$this` and
/// `(new C)->…` are objects by construction with no fact lane at all.
///
/// The env fact and the [`Store`] object binding are mutually exclusive by
/// construction: every rvalue arm of `apply_assign` pairs `env.remove(var)` with
/// `store.unbind(var)`.
fn call_receiver_fact<'a>(
    receiver: &Receiver,
    env: &'a HashMap<String, Known>,
    store: &'a Store,
) -> Option<(&'a Fact, Stratum, String)> {
    match receiver {
        Receiver::Var(v) => {
            let k = env.get(v)?;
            Some((k.fact.as_ref()?, k.stratum, format!("${v}")))
        }
        Receiver::Prop { var, prop } => Some((
            store.prop_fact(var, prop)?,
            store.prop_stratum(var, prop),
            format!("${var}->{prop}"),
        )),
        Receiver::This | Receiver::New { .. } => None,
    }
}

/// The PHP type name a fact's denotation is confined to, when that denotation
/// contains no object — the "definitely not an object" premise both members of
/// the ADR-0078 non-object family consume (issue #190).
///
/// The four-layer domain proves this for free: [`Val`] has no object variant and
/// [`Base`] no object base, so no fact can denote an object. What's left is
/// whether the fact names ONE type. Three shapes decline: a `nullable: true`
/// abstract layer (non-object either way, but no single word to print); a `OneOf`
/// mixing bases; and an absent fact, where every `Maybe`-object or unknown-class
/// receiver lands.
///
/// `null` IS named here: `property.on-non-object` owns that receiver, and the call
/// side filters it out since [`CALL_ON_NULL_ID`] already does.
fn definite_non_object_type(fact: &Fact) -> Option<&'static str> {
    match fact {
        // A union is several type words at once, and this surface names one.
        Fact::Union { .. } => None,
        Fact::Singleton(v) => Some(val_type_name(v)),
        Fact::OneOf(vals) => {
            let first = val_type_name(vals.first()?);
            vals.iter().all(|v| val_type_name(v) == first).then_some(first)
        }
        Fact::Refined { base, nullable: false, .. } | Fact::General { base, nullable: false } => {
            Some(base_type_name(*base))
        }
        Fact::Shape { nullable: false, .. } => Some("array"),
        Fact::Refined { nullable: true, .. }
        | Fact::General { nullable: true, .. }
        | Fact::Shape { nullable: true, .. } => None,
    }
}

/// The PHP type name of a concrete value, as the engine spells it in the
/// `Call to a member function m() on <type>` / `Attempt to read property "p" on
/// <type>` messages. Witnessed at PHP 8.5.9: a bool receiver renders as `true` /
/// `false` there rather than `bool`, but this is the *type* name the finding's
/// sentence reports, so the base name is what both callers want.
fn val_type_name(v: &Val) -> &'static str {
    match v {
        Val::Int(_) => "int",
        Val::Float(_) => "float",
        Val::Str(_) => "string",
        Val::Bool(_) => "bool",
        Val::Null => "null",
        Val::Array(_) => "array",
    }
}

/// The PHP type name of a scalar base.
fn base_type_name(base: Base) -> &'static str {
    match base {
        Base::Int => "int",
        Base::Float => "float",
        Base::String => "string",
        Base::Bool => "bool",
    }
}

/// The non-object receiver proof for a method call (`call.on-non-object`, ADR-0078,
/// issue #190): `$x = 1; $x->m();` is the same guaranteed `Error` `call.on-null`
/// reports, with the receiver's runtime type in place of null — witnessed at PHP
/// 8.5.9 as `Call to a member function m() on int` for `int`/`string`/`float`/
/// `true`/`false`/`array`.
///
/// Two deliberate boundaries: `?->` is not an excuse — nullsafe short-circuits on
/// `null` alone, so a proven non-null non-object receiver still fatals (witnessed),
/// so unlike [`check_call_on_null`] the `nullsafe` flag isn't read; and `null`
/// belongs to [`CALL_ON_NULL_ID`] (ADR-0022), disjoint by this one filter.
///
/// The receiver lane is exactly [`check_call_on_null`]'s, deliberately not
/// widened here (issue #196 owns reach).
pub(crate) fn check_call_on_non_object(
    w: &WalkCx,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    store: &Store,
    out: &mut Vec<Diagnostic>,
) {
    if w.scope.poisoned {
        return;
    }
    let Callee::Method { receiver, method, .. } = &call.receiver else {
        return;
    };
    let Some((fact, stratum, display)) = call_receiver_fact(receiver, env, store) else {
        return;
    };
    let Some(ty) = definite_non_object_type(fact) else {
        return;
    };
    if ty == "null" {
        return;
    }
    // Proof-layer consumption rule (ADR-0052 §5): an `Asserted` premise (a
    // `@phpstan-assert int $x`) cannot premise a proof-layer fatal.
    if stratum != Stratum::Verified {
        return;
    }
    let pos = w.cx.tree().position(call.span.start);
    out.push(Diagnostic {
        id: CALL_ON_NON_OBJECT_ID,
        facet: None,
        fix: None,
        path: w.cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        message: format!(
            "method call {display}->{method}() — {display} is proven {ty} on this path — proven Error (Call to a member function on {ty})"
        ),
    });
}

/// The non-object receiver proof for a property fetch (`property.on-non-object`,
/// ADR-0078, issue #190): `$x = 1; $y = $x->p;` raises
/// `Warning: Attempt to read property "p" on int` and evaluates to `null`
/// (witnessed at PHP 8.5.9 for `int`/`string`/`float`/`true`/`false`/`array`/`null`).
///
/// Warning-grade, so the ADR-0049 §7 posture gate comes first: under a declared
/// `warning-handler = "null"` the finding leaves the proof surface, like
/// `offset.missing`.
///
/// The receiver is only ever a bare variable — [`ArgValue::PropFetch`] is the only
/// lowered shape of a property read (a chain, dynamic name, or `?->` all lower to
/// `ArgValue::Other`). `$this` carries no fact lane, so `$this->p` is silent.
///
/// [`ArgValue::PropFetch`]: steins_syntax::ArgValue::PropFetch
pub(crate) fn check_property_on_non_object(
    cx: &Cx,
    var: &str,
    prop: &str,
    env: &HashMap<String, Known>,
    poisoned: bool,
    span: Span,
    out: &mut Vec<Diagnostic>,
) {
    if poisoned || !cx.warning_handler_abort {
        return;
    }
    let Some(k) = env.get(var) else { return };
    let Some(ty) = k.fact.as_ref().and_then(definite_non_object_type) else {
        return;
    };
    // Proof-layer consumption rule (ADR-0052 §5): an `Asserted` premise stays silent.
    if k.stratum != Stratum::Verified {
        return;
    }
    let pos = cx.tree().position(span.start);
    out.push(Diagnostic {
        id: PROPERTY_ON_NON_OBJECT_ID,
        facet: None,
        fix: None,
        path: cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        message: format!(
            "property fetch ${var}->{prop} — ${var} is proven {ty} on this path — proven E_WARNING (Attempt to read property on {ty}), evaluating to null"
        ),
    });
}

// end non-object receivers (ADR-0078, issue #190)
