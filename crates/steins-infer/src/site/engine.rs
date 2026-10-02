//! The only place a lane's effect and throw rows are read (ADR-0099 §2). The
//! argument tables a call's operands are held to (`arg_reach`, the by-value
//! certification) are [`super::reach`]'s.
//!
//! Each function answers one question of the engine's body of knowledge on one
//! axis: what a builtin function, an engine method or an engine constructor does
//! (effect labels), what it raises (throw classes), and what a call site can
//! certify about it. The resolver ([`super::resolve_site`]) calls these and
//! attaches the answer to the site it returns, so a lane folds rows it was handed
//! and never asks the catalog.
//!
//! Both lanes know the same names ([`steins_catalog::knows`]); what differs is the
//! axis a row is read on. A known name the axis has no row for comes back as a
//! gap-bearing answer (`None`, or an `Err` naming the [`GapKind`]), never as an
//! empty row.

use steins_syntax::{CallTarget, ConstArgs, ConstInt, RefTarget};

use crate::site::GapKind;

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

/// The classes a call to the builtin `name` raises, on the throw axis
/// (ADR-0099 §3.2, §3.3), or the gap that says the catalog cannot tell.
///
/// `call` is the call's argument facts: the positional arity (`None` for a named
/// or spread list) and the constant expressions its arguments are. It is `None`
/// where there are no arguments to read, a builtin handed over as a callback,
/// which its invoker calls with arguments of the invoker's choosing.
///
/// * A row, or the audited throwless table, answers ([`steins_catalog::throws_of`]).
/// * A name that raises only under a flag (`json_encode`, `json_decode`) answers
///   with its flag-free throws when the call shows its flags absent or a constant
///   expression without the flag, and is a [`GapKind::FlagDependentThrow`]
///   otherwise: flags the scan cannot read may hold `JSON_THROW_ON_ERROR`, and a
///   flag that is set makes the call raise a class this table does not state.
/// * Any other known name is a [`GapKind::NoThrowRow`]: unaudited, never throwless.
pub(crate) fn function_throws(
    name: &str,
    call: Option<(Option<usize>, &ConstArgs)>,
) -> Result<&'static [&'static str], GapKind> {
    let Some(gate) = steins_catalog::flag_gated_throw(name) else {
        return steins_catalog::throws_of(name).ok_or(GapKind::NoThrowRow);
    };
    let (arity, consts) = call.ok_or(GapKind::FlagDependentThrow)?;
    // A named or spread list hides which argument is the flags.
    let arity = arity.ok_or(GapKind::FlagDependentThrow)?;
    // An omitted flags argument is its default, `0`.
    let flags = if arity <= gate.position {
        Some(0)
    } else {
        let position = u8::try_from(gate.position).expect("a flags position is small");
        consts.ints.iter().find(|(p, _)| *p == position).and_then(|(_, expr)| eval_const_int(expr))
    };
    match flags {
        Some(flags) if flags & gate.flag == 0 => Ok(gate.base),
        _ => Err(GapKind::FlagDependentThrow),
    }
}

/// The value of a constant integer expression ([`ConstInt`]): a literal, an engine
/// constant the catalog states ([`steins_catalog::engine_constant`]), and their
/// `|`. `None` for a constant the catalog does not state an integer for (a user
/// `const`, an extension this build lacks): the flags are then unreadable.
fn eval_const_int(expr: &ConstInt) -> Option<i64> {
    match expr {
        ConstInt::Int(v) => Some(*v),
        ConstInt::Const(name) => match steins_catalog::engine_constant(name)?.value? {
            steins_catalog::ConstValue::Int(v) => Some(v),
            _ => None,
        },
        ConstInt::Or(terms) => {
            terms.iter().try_fold(0, |acc, term| Some(acc | eval_const_int(term)?))
        }
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

/// Whether the builtin `name` called with `positional` positional arguments is
/// **certified pure at that arity** (issue #851, [`steins_catalog::pure_at_arity`]):
/// `array_keys($a)` copies keys, while `array_keys($a, $v)` compares `$v` loosely
/// with every element, which runs an object's `__toString`. The argument-blind
/// effect row therefore stays absent, and the effect lane asks again with the
/// call's arity. `positional` is the argument count; `None` (a named or spread
/// list) has no arity to read.
pub(crate) fn pure_at_call_arity(name: &str, positional: Option<usize>) -> bool {
    positional.is_some_and(|n| steins_catalog::pure_at_arity(name, n))
}

/// Whether the builtin `name` is certified pure **at a call site that rules out
/// every argument reaching user code** (issue #856,
/// [`steins_catalog::certified_at_call_site`]): the string family, whose `string`
/// parameters run an object's `__toString` under coercive typing. The caller then
/// holds the call to the reach rule.
pub(crate) fn certified_at_call_site(name: &str) -> bool {
    steins_catalog::certified_at_call_site(name)
}

/// Whether a call with no readable positional argument list (`positional` is
/// `None`) names a builtin the catalog certifies pure at **one** positional
/// argument (`array_keys`, the only such name): [`pure_at_call_arity`] declines
/// it only for want of an arity to read.
pub(crate) fn arity_defeated(name: &str, positional: Option<usize>) -> bool {
    positional.is_none() && steins_catalog::pure_at_arity(name, 1)
}

/// Whether the effect axis has a row for the builtin `name` of its own: an
/// effect colour or a by-ref out-parameter row. A known name without one is
/// certified at its call or a [`GapKind::NoEffectRow`].
pub(crate) fn has_effect_row(name: &str) -> bool {
    steins_catalog::effect_labels(name).is_some() || steins_catalog::out_params(name).is_some()
}

/// Whether `class` (an FQN, case-insensitive) is an **engine class**: the mined hierarchy
/// declares it (a class, interface or enum php-src's stubs declare, a namespaced one under its
/// namespace, `Random\RandomException`, as a global one under its bare name) and the PHP the
/// hierarchy was cross-checked against has it too.
///
/// The hierarchy keys classes by FQN, so a name the user's namespace made up (`App\PDO`, or an
/// unimported `PDO` inside `namespace App`) is not in it. A row the stubs declare and that PHP
/// does not (`Io\Poll\PollException`, newer than the pinned minor) is the stubs' claim, not the
/// engine's: `new` of it is an `Error`, so no row may answer for it and it stays a gap
/// ([`steins_catalog::builtin_class_absent_on_pinned`], ADR-0099 §3).
pub(crate) fn declares_engine_class(class: &str) -> bool {
    (steins_catalog::builtin_class_display(class).is_some()
        || steins_catalog::builtin_class_supers(class).is_some())
        && !steins_catalog::builtin_class_absent_on_pinned(class)
}

/// The gap for a known name the catalog has no row for on `axis`'s side, at a
/// class the chain leaves the project at: the axis's missing-row kind when the
/// catalog knows the class at all, [`GapKind::UnknownClass`] when it does not (a
/// name nobody declares in the project, such as `new Engine`, is not thereby an
/// engine class).
pub(crate) fn missing_row(class: &str, axis: GapKind) -> GapKind {
    if declares_engine_class(class) { axis } else { GapKind::UnknownClass }
}
