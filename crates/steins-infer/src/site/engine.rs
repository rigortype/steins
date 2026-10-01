//! The only place a lane's catalog rows are read (ADR-0099 §2).
//!
//! Each function answers one question of the engine's body of knowledge on one
//! axis: what a builtin function, an engine method or an engine constructor does
//! (effect labels), what it raises (throw classes), and what a call site can
//! certify about it. The resolver ([`super::resolve_site`]) calls these and
//! attaches the answer to the site it returns, so a lane folds rows it was handed
//! and never asks the catalog.
//!
//! The effect lane and the throw lane do not know the same names yet (#864): each
//! row function documents the knowledge it reproduces.

use steins_syntax::{ArgShape, CallTarget, ConstArgs, NameRef, RefTarget};

use crate::cx::Cx;
use crate::project::FnResolution;
use crate::site::reach::{Frame, reaches_user_code};

/// The by-ref-into-a-caller-local color (ADR-0063 §2.3).
pub(crate) const MUTATE_LOCAL: &str = "mutate.local";

/// The effect label a by-ref write through an argument with this lvalue root
/// carries (ADR-0063 §2.3) — the **target leg** of the conditional out-param row.
///
/// Three genuinely different contracts, so not a per-function flag:
/// `preg_match($p, $s, $m)` writes only the frame, `preg_match($p, $s,
/// $this->m)` mutates an object every caller shares, and `preg_match($p, $s,
/// $_SESSION['m'])` writes interpreter-global state.
///
/// Non-local targets stop at the conservative parent `mutate` rather than pick
/// an ADR-0055 child (`mutate.self`/`mutate.instance`/`mutate.static`): that
/// taxonomy's *inference* is not built, and a coarse-but-true label beats a
/// precise guess. Steins still distinguishes targets — property-rooted by-ref
/// writes never claim `mutate.local` — while declining to name the flavor.
pub(crate) fn by_ref_label(target: RefTarget) -> &'static str {
    match target {
        RefTarget::Local => MUTATE_LOCAL,
        RefTarget::Superglobal => "global.write",
        RefTarget::Escaping => "mutate",
    }
}

/// The by-ref out-parameter labels a call to `name` carries, given the classified
/// argument list (ADR-0063 §2.3). `arg_targets` is `None` when positional mapping
/// was defeated by a named/spread argument — every conditional judgment is then
/// withheld, because `preg_match(matches: $m, …)` and `preg_match($p, $s)` cannot
/// be told apart by position and a guess in either direction is a lie.
fn out_param_labels(name: &str, arg_targets: Option<&[RefTarget]>) -> Vec<&'static str> {
    let (Some(positions), Some(targets)) = (steins_catalog::out_params(name), arg_targets) else {
        return Vec::new();
    };
    let mut labels: Vec<&'static str> = Vec::new();
    for &p in positions {
        // The arity leg: an argument that was not supplied is not written.
        let Some(&target) = targets.get(p) else { continue };
        let label = by_ref_label(target);
        if !labels.contains(&label) {
            labels.push(label);
        }
    }
    labels
}

/// One [`CallTarget`] as the catalog spells it. The two crates keep their own tiny
/// enum on purpose: `steins-catalog` depends on nothing (it is a body of knowledge
/// about PHP, testable without a parser) and `steins-syntax` is the Mago-lowering
/// layer that knows no catalog, so the translation lives here, in the crate that
/// already depends on both.
fn stream_target(target: Option<&CallTarget>) -> Option<steins_catalog::StreamTarget<'_>> {
    match target? {
        CallTarget::Literal(s) => Some(steins_catalog::StreamTarget::Literal(s)),
        CallTarget::ConstFetch(s) => Some(steins_catalog::StreamTarget::Constant(s)),
        // A return-mode flag is no stream target (issue #352): `fopen($p, true)`
        // is a program with a type error, not a mode string, and the row keeps
        // its arg-blind default.
        CallTarget::Bool(_) => None,
    }
}

/// The effect labels a builtin `name` carries at one call: its unconditional
/// catalog color ([`steins_catalog::effect_labels`]) joined with the
/// **conditional** by-ref out-parameter color this particular call earns
/// ([`steins_catalog::out_params`]).
///
/// The two axes are independent and both may fire: `shuffle($rows)` is
/// `nondet.random` *and* `mutate.local`. Empty for a pure or uncatalogued builtin
/// called without an out-parameter.
///
/// `const_args` is the third axis and the only one that can make a row *narrower*
/// (issues #318, #352): a wrapper-capable stream row is `io` until the call site
/// proves which channel it opens, and a dumper is `io.output.buffer` until the
/// call site proves return-mode — [`steins_catalog::narrowed_stream_labels`] and
/// [`steins_catalog::narrowed_output_labels`] are what read the two proofs. `None`
/// is the honest answer wherever the arguments are not in hand (a builtin passed
/// *as* a callback is invoked with arguments of the invoker's choosing, never ones
/// written here).
pub(crate) fn function_effects(
    name: &str,
    arg_targets: Option<&[RefTarget]>,
    const_args: Option<&ConstArgs>,
) -> Vec<&'static str> {
    let narrowed = const_args.and_then(|c| {
        steins_catalog::narrowed_stream_labels(
            name,
            stream_target(c.first.as_ref()),
            stream_target(c.second.as_ref()),
        )
    });
    // The output family's own narrowing (issue #352), on the same axis and with
    // the same syntactic bar: `print_r($x, true)` renders into a return value and
    // writes nothing. Disjoint from the stream narrowing above by name — no row
    // is both wrapper-capable and a dumper — so the two never contend.
    let return_mode =
        const_args.is_some_and(|c| matches!(c.second, Some(CallTarget::Bool(true))));
    let colored: &[&str] = match narrowed.as_deref() {
        Some(labels) => labels,
        None => steins_catalog::narrowed_output_labels(name, return_mode)
            .or_else(|| steins_catalog::effect_labels(name))
            .unwrap_or(&[]),
    };
    let mut labels: Vec<&'static str> = colored.to_vec();
    labels.extend(out_param_labels(name, arg_targets));
    labels
}

/// How the throw lane reads a builtin it knows.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ThrowsRole {
    /// A plain call (`strlen($s)`), or the callee of a higher-order call that is
    /// not an invoker.
    Call,
    /// A builtin handed over as a callback: the invoker calls it.
    Callback,
    /// The invoker of a higher-order call (`array_map`, `usort`).
    Invoker,
}

/// The classes a builtin the throw lane **knows** raises, in `role`.
///
/// **LEGACY DEFAULT — removed by #864 (ADR-0099 §3.2).** A known name with no
/// `builtin_throws` row reads as *throwless* here, and an invoker's own row is
/// never read at all. That is unsafe — `strlen($o)` under `@throws void` has an
/// empty, exhaustive throw set while `$o->__toString()` throws — and it is kept in
/// this one arm only because this slice must not move an answer. #864 turns the
/// missing row into [`super::GapKind::NoThrowRow`] and reads the invoker's row.
pub(crate) fn legacy_throws(name: &str, role: ThrowsRole) -> &'static [&'static str] {
    match role {
        ThrowsRole::Call | ThrowsRole::Callback => steins_catalog::builtin_throws(name).unwrap_or(&[]),
        ThrowsRole::Invoker => &[],
    }
}

/// The positional index of the callback argument of the builtin invoker `name`
/// (`array_map`, `usort`, `call_user_func`).
pub(crate) fn invoker_callback_param(name: &str) -> Option<usize> {
    steins_catalog::invocation_shape(name).map(|shape| shape.callback_param)
}

/// What the engine's catalog says of a method of an engine class, for a receiver
/// that names its class `exact`ly or only as a bound.
pub(crate) enum MethodRow {
    /// The labels of the method's row.
    Labels(&'static [&'static str]),
    /// The method has a row, but a subclass of the bound may replace it: only a
    /// final method's row answers for a bound receiver (issue #847).
    Open,
    /// No row.
    Missing,
}

/// The effect row of `class::method` for a receiver that names its class
/// `exact`ly (`new Foo`, `Foo::`, `parent::`) or only as a bound (`$this`,
/// `self::`, a declared receiver), which a subclass declared anywhere may stand in
/// for and so reaches only a row no subclass can override
/// ([`steins_catalog::final_method_effect_labels`], issue #847). PHP refuses a
/// subclass that redeclares a final method, so whatever the walk passes on the
/// way, the engine's body is the one that runs.
pub(crate) fn method_effects(class: &str, method: &str, exact: bool) -> MethodRow {
    if exact {
        return match steins_catalog::method_effect_labels(class, method) {
            Some(labels) => MethodRow::Labels(labels),
            None => MethodRow::Missing,
        };
    }
    match steins_catalog::final_method_effect_labels(class, method) {
        Some(labels) => MethodRow::Labels(labels),
        None if steins_catalog::method_effect_labels(class, method).is_some() => MethodRow::Open,
        None => MethodRow::Missing,
    }
}

/// The effect row of an engine class's constructor.
pub(crate) fn constructor_effects(class: &str) -> Option<&'static [&'static str]> {
    steins_catalog::method_effect_labels(class, "__construct")
}

/// The throw row of an engine class's constructor.
pub(crate) fn constructor_throws(class: &str) -> Option<&'static [&'static str]> {
    steins_catalog::method_throws(class, "__construct")
}

/// Whether a call [`Cx::resolve_effect_function`] left unresolved is a builtin
/// the catalog certifies pure **at this call's arity** (issue #851,
/// [`steins_catalog::pure_at_arity`]).
///
/// `array_keys($a)` copies keys, while `array_keys($a, $v)` compares `$v`
/// loosely with every element, which runs an object's `__toString`. The
/// argument-blind row therefore stays uncatalogued, and resolution does not
/// know the name. This asks resolution again with the arity-aware predicate,
/// so a namespaced shadow or an ambiguous global keeps the `…?` exactly as an
/// uncatalogued name would. `targets` is the positional argument list; `None`
/// (a named or spread argument) has no arity to read.
pub(crate) fn pure_at_call_arity(
    cx: &Cx,
    name: &NameRef,
    targets: Option<&[RefTarget]>,
) -> bool {
    let Some(positional) = targets.map(<[_]>::len) else { return false };
    let certified = |n: &str| steins_catalog::pure_at_arity(n, positional);
    matches!(cx.resolve_function_with(name, &certified), FnResolution::Builtin(_))
}

/// Whether a call [`Cx::resolve_effect_function`] left unresolved is a builtin
/// the catalog certifies pure **at a call site that rules out every argument
/// reaching user code** (issue #856, [`steins_catalog::certified_at_call_site`]):
/// the string family, whose `string` parameters run an object's `__toString`
/// under coercive typing. Resolution is asked again with that list, so a
/// namespaced shadow or an ambiguous global keeps its `…?`.
pub(crate) fn certified_at_call_site(
    cx: &Cx,
    frame: &Frame,
    name: &NameRef,
    shapes: Option<&[ArgShape]>,
) -> bool {
    match cx.resolve_function_with(name, &steins_catalog::certified_at_call_site) {
        FnResolution::Builtin(builtin) => {
            !reaches_user_code(cx, frame, &builtin, shapes, cx.strict(), &[])
        }
        FnResolution::User(_) | FnResolution::Unknown => false,
    }
}
