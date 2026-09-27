//! The **top-level rebind rule** (issue #762, ADR-0063's 2026-09-28 amendment):
//! in the frame whose locals are the globals, a statement that runs a call the
//! walk cannot resolve to an engine builtin forgets every name the frame holds.
//! One predicate ([`top_level_rebind_risk`]) for every carrier — the value lane,
//! the heap objects and the heap resources are all forgotten by the one clear the
//! walk applies on its answer.

use steins_catalog::CarrierShape;
use steins_syntax::{RunArg, RunCall, Runs};

use crate::by_value::{is_assert_read_site, is_dump_read_site};
use crate::cx::Cx;
use crate::existence::denotes_global_function;
use crate::fold::Folder;

/// Whether the statement whose [`Runs`] record this is, walked in the current
/// frame, may **rebind a name the frame holds** — in which case every fact the
/// walk proved about a name is a fact about what the name used to hold.
///
/// In the **top-level frame** the locals are the globals, so any userland body
/// that runs can rebind one through `global $s` or `$GLOBALS['s']` while the
/// call site mentions nothing — no argument, no receiver, so the statement's
/// invalidation set ([`Stmt::invalidated`], keyed on the names it mentions)
/// never learns of it. Probed at 8.5.10, this exits 0:
///
/// ```php
/// function bump(): void { global $s; $s = 5; }
/// $s = 'abc';
/// bump();
/// intdiv($s, 1); // $s is 5: no TypeError
/// ```
///
/// Inside a function body the question does not arise: `$s` is a local no
/// callee can see, and the analyzed scope's own `global $s` poisons it
/// already. So this answers `false` in every other frame.
///
/// In the top-level frame it answers `true` when the evaluation runs anything a
/// function name cannot describe ([`Runs::other`]: a method, static or
/// constructor call, a call through a value, a pipe, `include`/`require`/
/// `eval`), or any named call [`call_may_run_userland`] cannot clear.
/// Forgetting is strictly weaker than any fact it drops: it removes a proof and
/// never adds one.
///
/// **What it does not see** is userland the engine runs behind a signature or
/// an operator this reads as data: a `__toString` under a string conversion,
/// `__clone` under `clone`, `offsetGet` under an offset read, `__get`, `__set`
/// under a property write, a generator or an `Iterator` under `foreach`, a
/// `JsonSerializable` under `json_encode`, a destructor, an autoloader, a
/// registered error or output handler, a user stream wrapper. Any of them can
/// `global $s` too; that is the calibration ADR-0070's top-level refusal and
/// ADR-0097 §2.4's stream-wrapper note already accept, unchanged here.
///
/// [`Stmt::invalidated`]: steins_syntax::Stmt::invalidated
pub(crate) fn top_level_rebind_risk(cx: &Cx, folder: &mut dyn Folder, runs: &Runs) -> bool {
    crate::walk::frame_is_top_level()
        && (runs.other || runs.functions.iter().any(|call| call_may_run_userland(cx, folder, call)))
}

/// Whether one statically named call may run a userland body.
///
/// It may unless the name **denotes an engine builtin** — the global function it
/// spells ([`denotes_global_function`], so a namespaced twin and a project
/// shadow are both userland), known to the engine either by the mined arginfo
/// table or by the live engine's reflection — **and** no position that carries
/// a callee receives one ([`passes_a_callee`]). The table answers first: it
/// needs no reflection and it answers under `--no-php`, where a rule keyed on
/// reflection alone would forget across every `strlen()`.
///
/// The dump surface and the harness `assertType` are let through by the very
/// recognizers the by-value lane exempts them with ([`is_dump_read_site`],
/// [`is_assert_read_site`]): each is a question put to the walk rather than a
/// body that runs, and one that forgot the frame would answer every later
/// question about it with `unknown`.
fn call_may_run_userland(cx: &Cx, folder: &mut dyn Folder, call: &RunCall) -> bool {
    if is_dump_read_site(cx, &call.callee) || is_assert_read_site(cx, &call.callee) {
        return false;
    }
    if !denotes_global_function(cx, &call.callee) {
        return true;
    }
    // A reference that denotes the global function is spelled without a
    // namespace, so `raw` is the function's own name.
    let name = call.callee.raw.as_str();
    let mined = steins_catalog::param_facts_mined(name);
    if !mined && folder.builtin_param_types(name).is_none() {
        return true;
    }
    passes_a_callee(folder, name, mined, call)
}

/// Whether the builtin `name` receives, at this call, an argument in a position
/// that can carry a userland callee — [`steins_catalog::callback_carriers`], the
/// one carrier rule the by-value lane and the fold seam already share, so a
/// callback under `array_map`, `usort` or `call_user_func` counts exactly where
/// those two consumers refuse it.
///
/// One route is left out: [`CarrierShape::DeferredMixed`] (`ob_start`,
/// `pcntl_signal`, `assert`). A deferred callback does not run during the call
/// that stores it; it runs later, under a flush, a signal or a failing
/// assertion's `ini` handler, which is the registered-handler calibration of
/// [`top_level_rebind_risk`] and not this call's evaluation.
///
/// A builtin the table does not have but the engine reflects answers from its
/// reflected types instead: a position declared `callable` or `Closure`.
///
/// Per carrying position, an argument that names no callee is let through: the
/// literal `null` (`array_map(null, $a, $b)`), and a string naming a mined
/// builtin that carries none of its own (`array_map('intval', $a)`,
/// `usort($a, 'strcmp')`) — PHP resolves a string callable as a fully
/// qualified name, and a project cannot declare a function an engine already
/// has. A position a named or spread argument may fill counts as receiving one.
fn passes_a_callee(folder: &mut dyn Folder, name: &str, mined: bool, call: &RunCall) -> bool {
    // (position, whether it is a variadic tail: every argument from there on)
    let mut carriers: Vec<(usize, bool)> = steins_catalog::callback_carriers(name)
        .positions()
        .filter(|c| c.shape != CarrierShape::DeferredMixed)
        .map(|c| (c.position, c.shape == CarrierShape::UndeclaredTail))
        .collect();
    if !mined && let Some(params) = folder.builtin_param_types(name) {
        carriers.extend(params.iter().enumerate().filter_map(|(i, p)| {
            let ty = p.ty.as_deref()?;
            (ty.contains("callable") || ty.contains("Closure")).then_some((i, p.variadic))
        }));
    }
    carriers.into_iter().any(|(at, tail)| {
        if !call.positional_only && (tail || at >= call.args.len()) {
            return true;
        }
        let args = match call.args.get(at..) {
            Some(rest) if tail => rest,
            Some(rest) => &rest[..rest.len().min(1)],
            None => &[],
        };
        args.iter().any(|arg| !names_no_userland(arg))
    })
}

/// Whether an argument at a callee position is proven not to be a userland
/// callee (see [`passes_a_callee`]).
fn names_no_userland(arg: &RunArg) -> bool {
    match arg {
        RunArg::Null => true,
        RunArg::Name(n) => {
            steins_catalog::param_facts_mined(n) && steins_catalog::callback_carriers(n).is_empty()
        }
        RunArg::Other => false,
    }
}
