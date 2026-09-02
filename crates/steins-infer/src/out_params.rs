//! Out-parameter seeding: what a by-ref argument holds after the call, with the
//! `preg_match` / `preg_match_all` `$matches` shapes read off the pattern (issue
//! #156), the `settype` cast write at statement position (issue #595), the array
//! out-state rows of [`crate::array_out_state`] (issue #635), the preg flag
//! constants, and the `preg.invalid-pattern` entry points.

use std::collections::HashMap;

use steins_domain::{
    Base, Certainty, Fact, PhpStr, Refinement, ShapeFact, StrPreds, Key as VKey, Val,
};
use steins_syntax::{ArgValue, ArrayKey, CallExpr, CondExpr, NameRef, RefKind, StmtKind};

use crate::fold::Folder;
use crate::PREG_INVALID_PATTERN_ID;
use crate::array_out_state::{array_out_rule, byref_array_shape};
use crate::asserts::guard_call_line;
use crate::builtin_returns::transfer_declaration_admits;
use crate::coerce::{CastTarget, php_cast_fact};
use crate::cx::Cx;
use crate::env::{Known, Store, Stratum};
use crate::existence::global_function_callee;
use crate::project::Diagnostic;
use crate::refine::collect_truthy_calls;
use crate::transfers::list_transfer_fact;
use crate::walk::WalkCx;

/// **Where** an out-parameter seed is being asked for, which is the same
/// question as *what the caller has proven about the call* (ADR-0077 §3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SeedPosition {
    /// A call in a condition whose result this branch proved **truthy**.
    Guard,
    /// A bare call **statement**, where all the caller proved is that control
    /// reached the next statement — i.e. that the call returned at all.
    Statement,
}

impl SeedPosition {
    /// Whether a catalog witness is discharged at this position.
    ///
    /// [`WrittenWhen::CallReturns`] is strictly stronger than
    /// [`WrittenWhen::ReturnTruthy`] — a truthy return is a return — so it is
    /// admitted at the guard position too, where the branch proved more than it
    /// needs. The converse never holds: a statement proves nothing about the
    /// return value, so a truthiness witness stays a guard-only claim.
    ///
    /// [`WrittenWhen::CallReturns`]: steins_catalog::WrittenWhen::CallReturns
    /// [`WrittenWhen::ReturnTruthy`]: steins_catalog::WrittenWhen::ReturnTruthy
    fn admits(self, witness: steins_catalog::WrittenWhen) -> bool {
        use steins_catalog::WrittenWhen::{CallReturns, ReturnTruthy};
        match (self, witness) {
            (_, CallReturns) | (SeedPosition::Guard, ReturnTruthy) => true,
            (SeedPosition::Statement, ReturnTruthy) => false,
        }
    }
}

/// Seed the out-parameters of every call this branch proves returned truthy
/// (ADR-0077), in source order.
///
/// A truthy result is the callee's own witness that it performed its by-ref
/// write, and the ONLY branch where the written fact is sound — `preg_match` on
/// an uncompilable pattern returns `false` and writes nothing at all. Runs after
/// `walk_if` step 2's invalidation rebinds what it forgot (§3.4) and after the
/// branch's assert narrowings, so an explicit `@phpstan-assert` envelope is not
/// overwritten by a seed at a further call in the same condition.
pub(crate) fn seed_out_params(
    w: &WalkCx,
    folder: &mut dyn Folder,
    cond: &CondExpr,
    then: bool,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
) {
    let mut calls = Vec::new();
    collect_truthy_calls(cond, then, w.cx.php_minor, &mut calls);
    for call in calls {
        let seeds = out_param_seed(w, folder, call, env, store, SeedPosition::Guard);
        let line = guard_call_line(w, call);
        for (var, fact, stratum) in seeds {
            seed_out_param(&var, fact, stratum, OUT_PARAM_SEEDED, line, env, store);
        }
    }
}

/// **The statement-position out-parameter seed** (issue #595), computed from the
/// **pre-call** env and store and applied after the statement's by-ref
/// invalidation has run.
///
/// **Reading and binding are two halves on purpose.** The input a cast consumes
/// is what the variable held *before* the call, and by the time the walk reaches
/// step 4 the name is already forgotten — a reader placed there would find
/// nothing and decline every time. So this half runs on the entry env, and
/// [`apply_stmt_out_param_seeds`] binds what it computed *after* the forgetting,
/// which is how the written fact replaces the drop rather than racing it (the
/// ADR-0077 §3.4 ordering, at the statement rung).
///
/// **A bare call statement, or an assignment that does not take the name back**
/// (issue #635 widened the original issue-#595 rung).
///
/// The refusal this started as was `$v = settype($v, 'int')`: the call performs
/// its by-ref write and the assignment *then* overwrites `$v` with the call's
/// `true`, so the last word is the assignment's and a seed would state a value
/// that never existed. But that argument is about the **target**, not about
/// assignment: `$extract = array_splice($brr, 0, 0, 1)` writes `$brr` and binds
/// `$extract`, two different names, and the write is the last word on `$brr`
/// exactly as it is at a bare call statement. So the RHS of an assignment seeds
/// every out-parameter *except* the one the assignment is about to rebind.
///
/// `return`/`echo` stay out: a `return` position's seed could only be read by a
/// statement that does not run.
///
/// **A call nested inside another call's arguments stays out too** — the form
/// `assertType('string', array_shift($arr));` takes, which is why
/// `array-shift.php:15` is still unreached. The IR lowers such a call to
/// [`ArgValue::Call`], which keeps only the name's **last segment**: nothing
/// there can tell the global `array_shift` from a namespaced function of the
/// same name, and [`global_function_callee`]'s whole job is to refuse that
/// confusion. Reaching it needs a `NameRef` in `ArgValue::Call`, which is an IR
/// change and a `SCHEMA_VERSION` bump — deliberately out of this slice.
///
/// **Reading and binding are two halves on purpose.** The input a cast consumes
/// is what the variable held *before* the call, and by the time the walk reaches
/// step 4 the name is already forgotten — a reader placed there would find
/// nothing and decline every time. So this half runs on the entry env, and
/// [`apply_stmt_out_param_seeds`] binds what it computed *after* the forgetting,
/// which is how the written fact replaces the drop rather than racing it (the
/// ADR-0077 §3.4 ordering, at the statement rung).
pub(crate) fn stmt_out_param_seeds(
    w: &WalkCx,
    folder: &mut dyn Folder,
    kind: &StmtKind,
    env: &HashMap<String, Known>,
    store: &Store,
) -> Vec<(String, Fact, Stratum, u32)> {
    let (call, rebound) = match kind {
        StmtKind::Call(call) => (call, None),
        StmtKind::Assign { var, call: Some(call), .. } => (call, Some(var.as_str())),
        _ => return Vec::new(),
    };
    out_param_seed(w, folder, call, env, store, SeedPosition::Statement)
        .into_iter()
        .filter(|(var, _, _)| Some(var.as_str()) != rebound)
        .map(|(var, fact, stratum)| (var, fact, stratum, guard_call_line(w, call)))
        .collect()
}

/// Bind what [`stmt_out_param_seeds`] computed, after the statement's by-ref
/// invalidation forgot the same names.
pub(crate) fn apply_stmt_out_param_seeds(
    seeds: Vec<(String, Fact, Stratum, u32)>,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
) {
    for (var, fact, stratum, line) in seeds {
        seed_out_param(&var, fact, stratum, OUT_PARAM_SEEDED_STMT, line, env, store);
    }
}

/// The [`Known::bound`] provenance an out-parameter seed stamps (ADR-0077), read
/// as the clause it becomes: "from `$m`, written by the guard call on this
/// branch". Both halves of the claim are in it — the fact is the callee's, and it
/// holds *here* because the branch proved the write happened.
const OUT_PARAM_SEEDED: &str = "written by the guard call on this branch";

/// [`OUT_PARAM_SEEDED`]'s statement-position twin (issue #595). The second half
/// of the claim is weaker and says so: the write holds because the call
/// *returned*, which is all a statement proves.
const OUT_PARAM_SEEDED_STMT: &str = "written by the call at this statement";

/// **The out-parameter seed** (ADR-0077): the by-ref arguments a guard call
/// proved it wrote, paired with the fact its contract determines for each.
///
/// Called only from the branch where the call's result is proven truthy. `preg_match`
/// returns `1` and assigns the success shape, `0` and assigns `[]` — and on a
/// pattern PCRE refuses to compile it returns `false` and assigns **nothing at
/// all**, leaving the caller's variable holding whatever it held (measured, PHP
/// 8.5.9). That third outcome is the absence of an assignment, not a value a fact
/// could widen to include — so truthiness is the only place any fact is sound,
/// and a seed at the call statement would manufacture one on a reachable path.
///
/// Every leg refuses **silently**: the name stays forgotten, no diagnostic is
/// produced, and nothing distinguishes a pattern this engine cannot read from one
/// it never looked at.
fn out_param_seed(
    w: &WalkCx,
    folder: &mut dyn Folder,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    store: &Store,
    position_kind: SeedPosition,
) -> Vec<(String, Fact, Stratum)> {
    // A poisoned scope (ADR-0046) has already lost the right to say which name a
    // binding is — `extract()` and variable-variables can rewrite the frame the
    // seed would land in. The same gate `apply_call_asserts` applies.
    if w.scope.poisoned {
        return Vec::new();
    }
    let Some(name) = out_param_seed_callee(w.cx, call) else { return Vec::new() };
    let Some(positions) = steins_catalog::out_params(name) else { return Vec::new() };
    let mut seeds = Vec::new();
    for &position in positions {
        // The witness leg (§3.2): only a *stated* written-when seeds, and it must
        // be one this position has proven ([`SeedPosition::admits`]). Nothing is
        // inferred from the mere existence of an `out_params` row.
        let admitted = steins_catalog::out_param_written_when(name, position)
            .is_some_and(|w| position_kind.admits(w));
        if !admitted {
            continue;
        }
        // The arity leg: an argument the call never supplied was never written.
        let Some(arg) = call.args.get(position) else { continue };
        // The aliasing leg (§3.6): only a plain local variable. `$this->m`,
        // `$arr['k']` and a variable-variable all refuse, because the write may be
        // visible to callers this scope cannot see (ADR-0063 §2.3) and because
        // nothing here could name the target if it were.
        let ArgValue::Var(var) = &arg.value else { continue };
        let Some((fact, stratum)) =
            out_param_written_fact(w, folder, name, position, call, env, store)
        else {
            continue;
        };
        seeds.push((var.clone(), fact, stratum));
    }
    seeds
}

/// The builtin an out-parameter seed may consult: the name must denote the
/// **global** function ([`global_function_callee`] — a namespaced spelling or a
/// user function of the same name is a *different function*), and a call whose
/// positional mapping a named or spread argument defeated cannot say which
/// argument is which.
fn out_param_seed_callee<'a>(cx: &Cx, call: &'a CallExpr) -> Option<&'a str> {
    let callee = global_function_callee(cx, call)?;
    call.positional_only.then_some(callee)
}

/// The fact the callee's contract determines for the out-parameter at
/// `position`, computed from **proven arguments only** (ADR-0077 §3.3), paired
/// with the trust stratum it is bound at. Rows are dispatched by (name,
/// position) — the key the witness is indexed by.
///
/// The two preg rows bind `Asserted` (§3.3): the shape rests on a declared
/// contract plus proven inputs, never on observing a run. The `settype` row
/// carries its **input's** stratum instead, because the fact it states is the
/// input's own value put through a measured conversion — an `Asserted` phpdoc
/// input stays `Asserted`, and a `Verified` one has nothing weaker to inherit
/// (the grid itself is `Verified` engine behaviour). The issue-#635 array rows
/// carry the input's stratum for exactly that reason: each states the caller's
/// own array put through a measured rearrangement.
fn out_param_written_fact(
    w: &WalkCx,
    folder: &mut dyn Folder,
    name: &str,
    position: usize,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    store: &Store,
) -> Option<(Fact, Stratum)> {
    match (name.to_ascii_lowercase().as_str(), position) {
        ("preg_match", 2) => {
            preg_match_written_fact(w, folder, call, env).map(|f| (f, Stratum::Asserted))
        }
        ("preg_match_all", 2) => {
            preg_match_all_written_fact(w, folder, call, env).map(|f| (f, Stratum::Asserted))
        }
        ("settype", 0) => settype_written_fact(w, folder, call, env, store),
        (n, 0) => array_out_state_fact(w, folder, n, call, env, store),
        _ => None,
    }
}

/// **The array out-state rows** (issue #635): the sort family, the pointer
/// moves and the two queue ends, each of which rewrites argument 0 and returns
/// something that says nothing about it.
///
/// Two premises, refusing cheapest-first:
///
/// 1. **The declaration pin** (ADR-0061 §2): the running engine must still
///    declare the return the rule was written against — `true` for the twelve
///    sorts, `mixed` for the other four — *and* the arity measured at
///    `PINNED_PHP`, since neither return spelling pins which parameter is the
///    array (ADR-0064 Amendment B).
/// 2. **A plain local variable at argument 0.** Everything after that is
///    [`ArrayOutRule::written_fact`]'s business, and it does not decline: a
///    claim it cannot use falls to the floor the witness alone establishes.
///
/// **Stratum.** A precise answer carries the input's own, for the reason the
/// `settype` row does: it states the caller's array put through a measured
/// rearrangement, so an `Asserted` phpdoc input stays `Asserted`. The floor is
/// `Verified` — it is the engine's behaviour and inherits nothing from a claim
/// that was never made.
fn array_out_state_fact(
    w: &WalkCx,
    folder: &mut dyn Folder,
    name: &str,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    store: &Store,
) -> Option<(Fact, Stratum)> {
    let rule = array_out_rule(name)?;
    if !transfer_declaration_admits(w.cx, folder, name, rule.declared, Some(rule.arity)) {
        return None;
    }
    let ArgValue::Var(var) = &call.args.first()?.value else { return None };
    let claim = out_param_input_claim(env, store, var);
    let shape = claim.as_ref().and_then(|(input, _)| byref_array_shape(input));
    let stratum = match &shape {
        Some(_) => claim.as_ref().expect("a shape came from a claim").1,
        None => Stratum::Verified,
    };
    Some((rule.written_fact(shape.as_ref()), stratum))
}

/// **What `settype($var, $type)` wrote into `$var`** (issue #595), for a call
/// whose every premise is proven — else `None`, which leaves the caller's by-ref
/// invalidation standing (the FP-safe floor).
///
/// Four premises, in the order that refuses cheapest-first:
///
/// 1. **The declaration pin** (ADR-0061 §2 through
///    [`transfer_declaration_admits`]): the running engine must still declare
///    `settype(): bool`, and — since the parameter this rule writes is declared
///    `mixed`, which pins nothing on its own — its arity must still be the
///    `(2, 2)` measured at `PINNED_PHP` (ADR-0064 Amendment B). A silent engine
///    withholds rather than being trusted.
/// 2. **A proven type string.** The second argument must resolve to a literal
///    string through the fold gate every other reader here uses
///    ([`Cx::resolve_literal`]) — an unproven variable declines. A byte string
///    that is not valid UTF-8 declines with it: it names no type php-src accepts.
/// 3. **A target the value domain can spell** ([`CastTarget::from_type_string`]):
///    `'object'` writes a `stdClass`, which is not a [`Fact`], and every
///    spelling php-src refuses raises a `ValueError` before writing anything.
/// 4. **A pre-call claim about the input** ([`out_param_input_claim`]) and a grid cell
///    for it ([`php_cast_fact`]).
///
/// [`Cx::resolve_literal`]: crate::cx::Cx::resolve_literal
fn settype_written_fact(
    w: &WalkCx,
    folder: &mut dyn Folder,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    store: &Store,
) -> Option<(Fact, Stratum)> {
    if !transfer_declaration_admits(w.cx, folder, "settype", &["bool"], Some((2, 2))) {
        return None;
    }
    let type_arg = &call.args.get(1)?.value;
    let ArgValue::Str(spelling) = w.cx.resolve_literal(type_arg, env, w.scope.poisoned, folder)?
    else {
        return None;
    };
    let target = CastTarget::from_type_string(spelling.as_str()?)?;
    let ArgValue::Var(var) = &call.args.first()?.value else { return None };
    let (input, stratum) = out_param_input_claim(env, store, var)?;
    Some((php_cast_fact(&input, target)?, stratum))
}

/// The claim this walk holds about what `$var` **holds** on entry to the call,
/// with its stratum — the cast's input.
///
/// Both lanes, in the order every fact read takes them (ADR-0037): the env value
/// fact first, then the declared-arm lane lowered as one union
/// ([`steins_contract::to_fact`], the issue-#589 `lane_claim` reading).
///
/// The lane fallback carries **one clause of its own**: a lane whose only arm is
/// `float` lowers to `float` here, where `to_fact` floors it to nothing. The
/// floor is about *slot admission* — a `float` declaration ACCEPTS an int
/// (PHPStan core semantics), so a `Fact::General { base: Float }` would reject
/// values the declaration admits. This reader asks the other question: what the
/// slot HOLDS, which for a float-declared name is a float on every path PHP can
/// reach it by (the boundary converts in coercive mode and fatals in strict).
/// That is the same claim the dump surface already renders for such a name.
fn out_param_input_claim(
    env: &HashMap<String, Known>,
    store: &Store,
    var: &str,
) -> Option<(Fact, Stratum)> {
    if let Some(known) = env.get(var)
        && let Some(fact) = &known.fact
    {
        return Some((fact.clone(), known.stratum));
    }
    let arms = store.contract_arms(var)?;
    let stratum = if arms.iter().any(|a| a.stratum == Stratum::Asserted) {
        Stratum::Asserted
    } else {
        Stratum::Verified
    };
    let union =
        steins_contract::ContractTy::Union(arms.iter().map(|a| a.ty.clone()).collect());
    let fact = steins_contract::to_fact(&union).or_else(|| match arms {
        [only] if only.ty == steins_contract::ContractTy::Base(Base::Float) => {
            Some(Fact::General { base: Base::Float, nullable: false })
        }
        _ => None,
    })?;
    Some((fact, stratum))
}

/// The modeled `$flags` bits of the `preg_match` family, by **value** (issue
/// #168 rule 6). Each value was verified by running `php -r 'echo PREG_…;'`
/// (PHP 8.5.9) — a flag is a bit the callee tests, so the values, not the
/// names, are the contract.
const PREG_FLAG_PATTERN_ORDER: i64 = 1;
/// See [`PREG_FLAG_PATTERN_ORDER`].
const PREG_FLAG_SET_ORDER: i64 = 2;
/// See [`PREG_FLAG_PATTERN_ORDER`].
const PREG_FLAG_OFFSET_CAPTURE: i64 = 256;
/// See [`PREG_FLAG_PATTERN_ORDER`].
const PREG_FLAG_UNMATCHED_AS_NULL: i64 = 512;

/// The resolved, fully-modeled `$flags` of a `preg_match`/`preg_match_all` call
/// (issue #168). Only ever produced by [`preg_resolved_flags`], so holding one
/// *is* the proof that every set bit of the argument is modeled here.
///
/// `PREG_PATTERN_ORDER` has no field: it is `preg_match_all`'s default, so the
/// bit adds no information — `set_order: false` is that mode.
#[derive(Debug, Clone, Copy, Default)]
struct PregFlags {
    /// `PREG_SET_ORDER`: one array per match instead of one column per group.
    set_order: bool,
    /// `PREG_OFFSET_CAPTURE`: every entry becomes a `[text, offset]` pair.
    offset_capture: bool,
    /// `PREG_UNMATCHED_AS_NULL`: an unmatched group's entry is `null` instead of
    /// being dropped (`preg_match` trailing) or padded as `''` (interior /
    /// PATTERN_ORDER columns).
    unmatched_as_null: bool,
}

/// The proven flags of a preg call, or `None` — the gate of issue #168 rule 6:
/// **a present flags argument seeds only when it is a proven int whose every
/// set bit is one this slice models**, where "models" is per callee (`allowed`).
///
/// An absent argument is the measured default (PATTERN_ORDER, no pairs, no
/// nulls). A present one must resolve to a proven int — a literal, a modeled
/// engine constant ([`preg_flag_const_value`]), or a variable the walk proves —
/// and any unknown bit, any bit outside `allowed`, or any unproven value
/// declines the whole seed, silently. Both order bits together also decline:
/// measured (PHP 8.5.9), `preg_match_all(…, 3)` throws a `ValueError` and writes
/// nothing, so there is no written shape to state.
fn preg_resolved_flags(
    w: &WalkCx,
    folder: &mut dyn Folder,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    allowed: i64,
) -> Option<PregFlags> {
    let Some(arg) = call.args.get(3) else { return Some(PregFlags::default()) };
    let bits = match &arg.value {
        ArgValue::GlobalConst(r) => preg_flag_const_value(w.cx, r)?,
        v => match w.cx.resolve_literal(v, env, w.scope.poisoned, folder)? {
            ArgValue::Int(n) => n,
            _ => return None,
        },
    };
    if bits & !allowed != 0 {
        return None;
    }
    if bits & PREG_FLAG_PATTERN_ORDER != 0 && bits & PREG_FLAG_SET_ORDER != 0 {
        return None;
    }
    Some(PregFlags {
        set_order: bits & PREG_FLAG_SET_ORDER != 0,
        offset_capture: bits & PREG_FLAG_OFFSET_CAPTURE != 0,
        unmatched_as_null: bits & PREG_FLAG_UNMATCHED_AS_NULL != 0,
    })
}

/// The engine **value** of a modeled `PREG_*` flag-constant reference, or
/// `None` (issue #168 rule 6: constants resolve to values, never match by name —
/// the name is only the route to the engine's constant).
///
/// Guarded by the same shadow discipline as `PHP_VERSION_ID`
/// ([`is_engine_version_id`], issue #29): a fully-qualified `\PREG_SET_ORDER`
/// always denotes the engine constant (defined first; a `define` of an
/// already-defined constant is a no-op). An unqualified one does too, unless
/// this file `use const`-imports one of the names or any project file declares a
/// userland twin — PHP's namespace fallback would then resolve to the twin,
/// whose value is unknown. Qualified and `namespace\`-relative spellings never
/// denote the engine constant.
///
/// [`is_engine_version_id`]: crate::cond::is_engine_version_id
fn preg_flag_const_value(cx: &Cx, r: &NameRef) -> Option<i64> {
    let value = match r.raw.as_str() {
        "PREG_PATTERN_ORDER" => PREG_FLAG_PATTERN_ORDER,
        "PREG_SET_ORDER" => PREG_FLAG_SET_ORDER,
        "PREG_OFFSET_CAPTURE" => PREG_FLAG_OFFSET_CAPTURE,
        "PREG_UNMATCHED_AS_NULL" => PREG_FLAG_UNMATCHED_AS_NULL,
        _ => return None,
    };
    let ok = match r.kind {
        RefKind::FullyQualified => true,
        RefKind::Unqualified => {
            !cx.tree().preg_flag_const_aliased()
                && !cx.units.iter().any(|u| u.tree.preg_flag_const_declared())
        }
        _ => false,
    };
    ok.then_some(value)
}

/// What `preg_match` wrote into `$matches`, for a call whose every premise is
/// proven — else `None`. Three refusals: the pattern must resolve to a proven
/// `Singleton` string (ADR-0037; a computed/widened pattern says nothing about
/// group structure); the group reader (#149) must establish the numbering; the
/// flags argument, if supplied, must be a proven int whose every set bit is
/// modeled ([`preg_resolved_flags`]) — for `preg_match` that's
/// `PREG_OFFSET_CAPTURE` and `PREG_UNMATCHED_AS_NULL` (the order bits are
/// `preg_match_all` vocabulary and measured `ValueError` here, so stay outside
/// `allowed`). The `$offset` argument (position 4) is not consulted: it moves
/// where matching starts but measurement confirms it leaves the written key set
/// alone.
fn preg_match_written_fact(
    w: &WalkCx,
    folder: &mut dyn Folder,
    call: &CallExpr,
    env: &HashMap<String, Known>,
) -> Option<Fact> {
    let flags = preg_resolved_flags(
        w,
        folder,
        call,
        env,
        PREG_FLAG_OFFSET_CAPTURE | PREG_FLAG_UNMATCHED_AS_NULL,
    )?;
    let groups = preg_proven_groups(w, folder, call, env)?;
    preg_success_shape(&groups, flags)
}

/// What `preg_match_all` wrote into `$matches` (issue #168), for a call whose
/// every premise is proven — else `None`. Same pattern and reader refusals as
/// [`preg_match_written_fact`]; the flags gate additionally admits the two order
/// bits, mutually exclusive (measured: both together is a `ValueError`).
///
/// Called only on the proven-truthy branch (ADR-0077): the return is
/// `int|false`, and a truthy value (int >= 1) proves both that the pattern
/// compiled and that at least one match landed — so every column
/// (PATTERN_ORDER) is a written, non-empty list, and the SET_ORDER outer list is
/// non-empty. The zero-match write (`ret = 0`, empty columns — measured) is real
/// but indistinguishable from `false` on the falsy branch, so it stays unseeded.
fn preg_match_all_written_fact(
    w: &WalkCx,
    folder: &mut dyn Folder,
    call: &CallExpr,
    env: &HashMap<String, Known>,
) -> Option<Fact> {
    let flags = preg_resolved_flags(
        w,
        folder,
        call,
        env,
        PREG_FLAG_PATTERN_ORDER
            | PREG_FLAG_SET_ORDER
            | PREG_FLAG_OFFSET_CAPTURE
            | PREG_FLAG_UNMATCHED_AS_NULL,
    )?;
    let groups = preg_proven_groups(w, folder, call, env)?;
    if flags.set_order {
        // SET_ORDER: a non-empty list of per-match sets, and each set follows the
        // `preg_match` success-shape rules under the same entry flags — measured,
        // trailing absence applies per set (`['2', '2']` has no key 2), interior
        // padding is `''`, and the flag variants match entry for entry. One
        // constructor, deliberately: re-deriving the set shape here would let the
        // two paths drift (issue #168 rule 3).
        let set = preg_success_shape(&groups, flags)?;
        Some(list_transfer_fact(true, Some(set)))
    } else {
        preg_pattern_order_shape(&groups, flags)
    }
}

/// The proven capture-group structure of a preg call's pattern argument, or
/// `None`: the pattern must resolve to a proven `Singleton` string (ADR-0037)
/// and the slice-A reader (#149) must fully establish its numbering.
fn preg_proven_groups(
    w: &WalkCx,
    folder: &mut dyn Folder,
    call: &CallExpr,
    env: &HashMap<String, Known>,
) -> Option<steins_catalog::preg::CaptureGroups> {
    let pattern = preg_proven_pattern(w, folder, &call.args.first()?.value, env)?;
    steins_catalog::preg::capture_groups(&pattern)
}

/// The **one** fold-gate fact both preg consumers rest on: `value` resolves to a
/// proven `Singleton` string (ADR-0037), i.e. a pattern the analysis can name.
///
/// Extracted so the ADR-0078 refusal check and the capture-group reader ask the
/// same question out of the same seam rather than re-deriving "is this a literal
/// pattern" and drifting apart. Everything the reader's resolution admits — a
/// written literal, a variable bound to one, a concatenation of proven halves, a
/// folded builtin call — is admitted here, and nothing else is.
fn preg_proven_pattern(
    w: &WalkCx,
    folder: &mut dyn Folder,
    value: &ArgValue,
    env: &HashMap<String, Known>,
) -> Option<String> {
    match w.cx.resolve_literal(value, env, w.scope.poisoned, folder)? {
        // A byte-string pattern is not a spelling either preg reader can parse,
        // so the seam declines once for both rather than guessing at a lossy
        // decoding of it (ADR-0080 §2.5).
        ArgValue::Str(pattern) => pattern.as_str().map(ToOwned::to_owned),
        _ => None,
    }
}

// preg pattern refusal (ADR-0078, issue #189)

/// Where a recognized `preg_*` entry point keeps its PCRE pattern(s) in argument 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PregPatternForm {
    /// A single pattern string, and nothing else is accepted there.
    Single,
    /// A single pattern string **or** a list of them
    /// (`preg_replace(['/a/', '/b/'], 'z', $s)`).
    SingleOrList,
    /// An array whose **keys** are the patterns
    /// (`preg_replace_callback_array(['/a/' => $cb], $s)`).
    Keys,
}

/// One `preg_*` entry point that takes a PCRE pattern in argument 0 (ADR-0078).
struct PregEntryPoint {
    /// The builtin's name, matched case-insensitively (PHP function names are).
    name: &'static str,
    form: PregPatternForm,
    /// What the call evaluates to when PCRE refuses the pattern. Measured at PHP
    /// 8.5.9 with `@f('/(unclosed/', …)`: the `preg_match` family and the two
    /// splitters answer `false`, the four replacers answer `null`. Both are
    /// preceded by the same `E_WARNING`, which is why the whole set rides the
    /// ADR-0049 §7 warning-handler gate together.
    refusal_value: &'static str,
}

/// **Every** `preg_*` function that takes a pattern, and no others.
///
/// The `preg_*` surface is `preg_match`, `preg_match_all`, `preg_replace`,
/// `preg_replace_callback`, `preg_replace_callback_array`, `preg_filter`,
/// `preg_split`, `preg_grep`, `preg_quote`, `preg_last_error` and
/// `preg_last_error_msg`. The last three take no pattern: `preg_quote` takes the
/// text to escape (never compiled), and the two error readers take nothing.
const PREG_PATTERN_ENTRY_POINTS: &[PregEntryPoint] = &[
    PregEntryPoint { name: "preg_match", form: PregPatternForm::Single, refusal_value: "false" },
    PregEntryPoint { name: "preg_match_all", form: PregPatternForm::Single, refusal_value: "false" },
    PregEntryPoint { name: "preg_split", form: PregPatternForm::Single, refusal_value: "false" },
    PregEntryPoint { name: "preg_grep", form: PregPatternForm::Single, refusal_value: "false" },
    PregEntryPoint { name: "preg_replace", form: PregPatternForm::SingleOrList, refusal_value: "null" },
    PregEntryPoint {
        name: "preg_replace_callback",
        form: PregPatternForm::SingleOrList,
        refusal_value: "null",
    },
    PregEntryPoint { name: "preg_filter", form: PregPatternForm::SingleOrList, refusal_value: "null" },
    PregEntryPoint {
        name: "preg_replace_callback_array",
        form: PregPatternForm::Keys,
        refusal_value: "null",
    },
];

/// The entry point `call` names, or `None` when it names none — or names one
/// textually while denoting something else. [`global_function_callee`] owns that
/// second rule: a `Foo\preg_match` or a project function of the same simple name
/// is a DIFFERENT function, and asking PCRE about its first argument would be a
/// claim about code we did not analyze.
fn preg_entry_point(cx: &Cx, call: &CallExpr) -> Option<&'static PregEntryPoint> {
    let callee = global_function_callee(cx, call)?;
    PREG_PATTERN_ENTRY_POINTS.iter().find(|e| callee.eq_ignore_ascii_case(e.name))
}

/// The proven pattern strings in a `Single`/`SingleOrList` argument.
///
/// The whole-value resolution comes first (a written literal, a variable bound
/// to one, or an array every element of which is proven). A **partial** array
/// needs the second leg: `resolve_literal` is all-or-nothing over an array, so
/// `preg_replace(['/(unclosed/', $dynamic], …)` resolves to nothing at all — yet
/// each element is still its own question, and the literal one is refused no
/// matter what `$dynamic` holds. Elements that do not resolve contribute nothing
/// (silence, never a guess).
fn preg_pattern_list(
    w: &WalkCx,
    folder: &mut dyn Folder,
    value: &ArgValue,
    env: &HashMap<String, Known>,
) -> Vec<String> {
    if let Some(resolved) = w.cx.resolve_literal(value, env, w.scope.poisoned, folder) {
        // A byte-string pattern contributes nothing: PCRE is asked about a
        // pattern this reader can name, never about a lossy decoding of one
        // (ADR-0080 §2.5) — the same silence a non-literal element gets.
        return match resolved {
            ArgValue::Str(p) => p.as_str().map(ToOwned::to_owned).into_iter().collect(),
            ArgValue::Array(items) => items
                .into_iter()
                .filter_map(|(_, v)| match v {
                    ArgValue::Str(p) => p.as_str().map(ToOwned::to_owned),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
    }
    let ArgValue::Array(items) = value else { return Vec::new() };
    items.iter().filter_map(|(_, v)| preg_proven_pattern(w, folder, v, env)).collect()
}

/// The pattern strings in a `Keys` argument (`preg_replace_callback_array`).
///
/// Keys are fixed at lowering, so no element value needs to resolve for a key to be
/// known — the callback values (closures) never resolve to literals anyway. Only a
/// `Str` key can be a pattern: PHP normalizes integer-like string keys to `Int`, and
/// a PCRE pattern always opens with a non-alphanumeric delimiter, so a pattern is
/// never integer-like and an `Int`/`Auto` key is never one.
fn preg_pattern_keys(
    w: &WalkCx,
    folder: &mut dyn Folder,
    value: &ArgValue,
    env: &HashMap<String, Known>,
) -> Vec<String> {
    let resolved = w.cx.resolve_literal(value, env, w.scope.poisoned, folder);
    let ArgValue::Array(items) = resolved.as_ref().unwrap_or(value) else { return Vec::new() };
    items
        .iter()
        .filter_map(|(k, _)| match k {
            ArrayKey::Str(s) => s.as_str().map(ToOwned::to_owned),
            // An unknown key names nothing this reader can quote (issue #336).
            ArrayKey::Int(_) | ArrayKey::Auto | ArrayKey::Expr(_) => None,
        })
        .collect()
}

/// Emit `preg.invalid-pattern` for every proven-literal pattern at this `preg_*`
/// call that the project's own PCRE refuses (ADR-0078, issue #189).
///
/// The ladder, cheapest legs first: (1) the warning-handler posture (ADR-0049
/// §7) — under a declared `warning-handler = "null"` the finding leaves the
/// proof surface, wired exactly as `offset.missing`; (2) a recognized entry
/// point, denoting the real builtin; (3) positional arguments only — a named
/// `pattern:` argument, a spread, or a first-class callable (`preg_match(...)`,
/// which builds a Closure and compiles nothing) is skipped, the same
/// conservatism `out_param_seed_callee` applies; (4) a live, legitimate boot
/// surface (`absence_family_available`) — no runtime-redefinition extension has
/// redefined `preg_match` (ADR-0049 A9), and the runtime is a version the
/// project declares it ships on (issue #28); `--no-php` and a missing `php` both
/// fail this leg, the sound subset (ADR-0004); (5) a proven literal pattern from
/// the same fold gate the capture-group reader consumes
/// ([`preg_proven_pattern`]); (6) PCRE's own refusal, asked of the project's own
/// engine and deduped per distinct pattern for the whole run
/// ([`Folder::preg_pattern_refusal`]).
///
/// Positions covered: the statement-position pass (a bare call, and the
/// `return`/assign/echo positions [`checkable_calls`] yields) and `walk_if`'s
/// guard, where `if (preg_match(…))` lives. A call in a loop header, ternary
/// condition, or `match` subject is not reached by either — silence, the same
/// position boundary every other call-site check here carries.
///
/// [`checkable_calls`]: crate::descent::checkable_calls
pub(crate) fn check_preg_pattern(
    w: &WalkCx,
    folder: &mut dyn Folder,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    out: &mut Vec<Diagnostic>,
) {
    let cx = w.cx;
    if !cx.warning_handler_abort {
        return;
    }
    let Some(entry) = preg_entry_point(cx, call) else {
        return;
    };
    if !call.positional_only {
        return;
    }
    let Some(arg) = call.args.first() else {
        return;
    };
    if !folder.absence_family_available() {
        return;
    }

    let patterns = match entry.form {
        PregPatternForm::Single => {
            preg_proven_pattern(w, folder, &arg.value, env).into_iter().collect()
        }
        PregPatternForm::SingleOrList => preg_pattern_list(w, folder, &arg.value, env),
        PregPatternForm::Keys => preg_pattern_keys(w, folder, &arg.value, env),
    };

    // Report at the PATTERN argument, not the call: that is the text to fix, and in
    // the array form every element shares it (a lowered array element carries no
    // span of its own — the pattern text in the message is what tells them apart).
    let pos = cx.tree().position(arg.span.start);
    for pattern in patterns {
        let Some(message) = folder.preg_pattern_refusal(&pattern) else {
            continue;
        };
        out.push(Diagnostic {
            id: PREG_INVALID_PATTERN_ID,
            path: cx.path().to_owned(),
            line: pos.line,
            column: pos.column,
            message: format!(
                "pattern '{pattern}' is refused by this PHP's PCRE; the call warns and \
                 returns {} with \"{}(): {message}\"",
                entry.refusal_value, entry.name
            ),
            facet: None,
            fix: None,
        });
    }
}

/// The element type of one `$matches` entry, read off the sub-pattern that
/// produced it (issue #156). Three rules, each falling back to plain `string`
/// when it cannot establish more: a floor of one character rules out `''`
/// (`non-empty-string`); a floor of **two** rules out `'0'` too — the falsy
/// strings — giving `non-falsy-string`; a sub-pattern producing only ASCII
/// digits, at least one, is `numeric-string` (measured: PHP calls every
/// non-empty ASCII digit run numeric, leading zeros and four hundred digits
/// included). Floors are counted in **characters**, so a multi-byte literal
/// like `(£|€)` (one-character floor, two-byte floor) stays `non-empty-string`,
/// agreeing with PHPStan.
///
/// Not implemented: "a mandatory literal character other than `'0'`" as a
/// second route to `non-falsy-string` (the issue's alternative). Sound but
/// claims more than PHPStan does for `/(a|b)|(?:c)/` (`non-empty-string`
/// there), trading agreement for sharpness — declined, cheap to add later.
fn preg_element_fact(text: &steins_catalog::preg::MatchedText) -> Fact {
    let mut preds = StrPreds::empty();
    if text.min_chars >= 1 {
        preds = preds.union(StrPreds::NON_EMPTY);
        if text.digits_only {
            preds = preds.union(StrPreds::NUMERIC);
        }
    }
    if text.min_chars >= 2 {
        preds = preds.union(StrPreds::NON_FALSY);
    }
    Fact::refined(Base::String, Refinement::Str(preds), false)
}

/// The element type of one capture group's entry when the group participates —
/// the literal union where the reader enumerated the body's language (issue
/// #177, slice F), and the slice-E [`preg_element_fact`] refinement everywhere
/// else.
///
/// The union is the sharper answer to the same question the refinement
/// answers, so it slots in above it and changes nothing about presence: the
/// unmatched paths (`''` padding, `null`, an absent key) stay with the
/// positional projections and flag seams, which join their extra member onto
/// whatever element this function produced. The reader caps the union at the
/// domain's own finite layer ([`steins_catalog::preg::LITERAL_UNION_CAP`], the
/// `OneOf` cap), so `from_vals` never widens here; the fallthrough is
/// belt-and-suspenders for an empty or unrepresentable set.
fn preg_group_element_fact(g: &steins_catalog::preg::CaptureGroup) -> Fact {
    if let Some(literals) = &g.literals {
        let vals = literals.iter().map(|s| Val::Str(s.clone().into())).collect();
        if let Some(fact) = Fact::from_vals(vals) {
            return fact;
        }
    }
    preg_element_fact(&g.body)
}

/// The success shape of `$matches`: key `0` plus one key per capture group in
/// numeric order, each value a string refined from the sub-pattern that fills
/// it, named groups additionally under their string key, sealed.
///
/// Every claim below is `php -r`-measured (8.5.9), three counter-intuitively: a
/// **trailing** unmatched group is dropped outright
/// (`preg_match('/(a)(b)?/', 'a', $m)` gives keys `[0, 1]`), so the slice-A
/// reader's `can_be_trailing_absent` becomes an **optional** key; an
/// **interior** unmatched group is present as `''`
/// (`preg_match('/(a)(b)?(c)/', 'ac', $m)` gives `[0, 1, 2 => '', 3]`), so it
/// stays **required** — absence is trailing-only; a named group occupies both
/// its string key and its numeric one, present together.
///
/// **The element type is coupled to that same absence rule.** PHPStan expects
/// `array{0: non-falsy-string, 1: 'a', 2: string, 3: 'c', 4?: non-empty-string}`
/// for `/(a)(b)*(c)(d)*/` — the middle `(b)*` and trailing `(d)*` are the same
/// sub-pattern with **different** element types. A group that can be *present*
/// while unmatched holds `''` on that path, so no bare body refinement may
/// stand: a floor collapses to plain `string`, and a literal union (slice F,
/// issue #177) must carry `''` as an explicit member (measured:
/// `preg_match('/(a)(b)?(c)/', 'ac', $m)` gives `$m[2] === ''`, entry `''|'b'`);
/// only a group whose unmatched case is *absence* keeps the bare claim. The
/// reader distinguishes which
/// ([`can_be_present_empty`](steins_catalog::preg::CaptureGroup::can_be_present_empty));
/// getting it backwards puts a false fact on a reachable path.
///
/// **List-ness.** PHP writes keys `0..n` ascending and the trailing drop
/// removes a suffix, so every realizable key set is a prefix — `array_is_list`
/// measured `true` for every group-only pattern probed, including
/// `/(a)(b)?(c)?/` matching only `a`. The shape alone can't see this (it also
/// admits a hole-bearing `{0, 2}` PCRE never produces), so the flag is asserted.
/// A named group ends it: its string key makes `array_is_list` false whenever
/// the group participates, which it may not (`/(a)(?<b>x)?/` on `'a'` measured
/// a list, on `'ax'` not) — so a pattern with any name asserts nothing and lets
/// denotational computation answer (`No` if the name always participates,
/// `Maybe` otherwise).
///
/// **The per-match constructor is this one function** for both consumers: a
/// `preg_match` seed and one `PREG_SET_ORDER` set of `preg_match_all` are the
/// same measured shape (issue #168 rule 3) — no second derivation.
///
/// `flags`, each variant measured: `PREG_UNMATCHED_AS_NULL` turns optionality
/// into nullability — an unmatched entry is **present** with value `null`
/// (`preg_match('/(a)(b)?/', 'a', $m, PREG_UNMATCHED_AS_NULL)` gives `['a',
/// 'a', null]`), every key required, a can-go-unmatched group's element gains
/// `|null`, and the interior-`''` padding disappears (`/(a)(b)?(c)/` on `'ac'`
/// gives `['ac', 'a', null, 'c']`). `PREG_OFFSET_CAPTURE` turns every entry
/// into a `[text, offset]` pair ([`preg_offset_pair`]); presence is unchanged
/// (a trailing unmatched group still drops), and a present-but-unmatched entry
/// is `['', -1]` (`[null, -1]` with both flags).
fn preg_success_shape(
    groups: &steins_catalog::preg::CaptureGroups,
    flags: PregFlags,
) -> Option<Fact> {
    use steins_domain::{Presence, SHAPE_WIDTH_LIMIT, Tail};

    // Presence is `witnessed: false` throughout (ADR-0062 §3): this is a declared
    // contract read off the callee, not a guard that observed the array.
    let required = Presence::Required { witnessed: false };
    let slot = |fact: Fact| Some(Box::new(fact));

    // The whole-match entry always participates: never null, offset floor 0.
    let entry0 = {
        let e = preg_element_fact(&groups.whole);
        if flags.offset_capture { preg_offset_pair(e, false)? } else { e }
    };
    let mut fields = vec![(VKey::Int(0), required, slot(entry0))];
    for (i, g) in groups.groups.iter().enumerate() {
        let presence = if flags.unmatched_as_null {
            // Optionality became nullability: the unmatched entry is written.
            required
        } else if g.can_be_trailing_absent {
            Presence::Optional
        } else {
            required
        };
        let element = if flags.unmatched_as_null {
            // The unmatched case is an explicit `null`, and the `''` padding is
            // gone (measured above) — so the body's element holds wherever the
            // value is a string, and `|null` covers the rest (measured with a
            // literal body: `preg_match('/(a)(b)?/', 'a', $m,
            // PREG_UNMATCHED_AS_NULL)` gives `['a', 'a', null]`, so entry 2 is
            // `'b'|null`).
            let body = preg_group_element_fact(g);
            if g.can_go_unmatched { preg_nullable_element(body) } else { body }
        } else if g.can_be_present_empty {
            // A group that may be present as `''` admits the empty string on a
            // reachable path, so its body's *floor* says nothing about its
            // entry — while an enumerated body keeps its union with `''`
            // joined on (measured: `preg_match('/(a)(b)?(c)/', 'ac', $m)`
            // gives `$m[2] === ''`, so the entry is `''|'b'`). One seam for
            // both: the padded join collapses a floor against `''` to plain
            // `string` and extends a literal union by the one member.
            preg_padded_element(preg_group_element_fact(g))
        } else {
            preg_group_element_fact(g)
        };
        let element = if flags.offset_capture {
            // `-1` is reachable exactly where an unmatched instance of this
            // group has a written entry: the interior-`''` case, widened to
            // every can-go-unmatched group under PREG_UNMATCHED_AS_NULL
            // (trailing absence no longer removes the entry).
            let unmatched_written = if flags.unmatched_as_null {
                g.can_go_unmatched
            } else {
                g.can_be_present_empty
            };
            preg_offset_pair(element, unmatched_written)?
        } else {
            element
        };
        let index = i64::try_from(i + 1).ok()?;
        fields.push((VKey::Int(index), presence, slot(element.clone())));
        if let Some(name) = &g.name {
            fields.push((VKey::Str(name.clone().into()), presence, slot(element)));
        }
    }
    // A pattern with more groups than a shape may carry has no faithful shape, and
    // a truncated one would claim a seal it cannot honor.
    if fields.len() > SHAPE_WIDTH_LIMIT {
        return None;
    }
    let list = groups.groups.iter().all(|g| g.name.is_none());
    let shape = ShapeFact::normalize(
        fields,
        // Sealed: a match writes these keys and no others. The verb family that
        // would add one (`(*MARK:x)`) is a decline in the reader, not a key here.
        Tail::Sealed,
        if list { Certainty::Yes } else { Certainty::Maybe },
        true,
        Vec::new(),
    );
    Some(Fact::Shape { shape: Box::new(shape), nullable: false })
}

/// The PATTERN_ORDER shape of `preg_match_all` (issue #168 rule 2, the default
/// and explicit `PREG_PATTERN_ORDER`): sealed, one **always-present** column
/// per entry — key `0` plus `1..n` with each name beside its numeric twin —
/// every column a `non-empty-list<elem>` on the proven-truthy branch (an
/// int >= 1 matches landed, and every column holds exactly that many entries).
///
/// **Columns are PADDED — the trap this slice exists to avoid.** `preg_match`'s
/// trailing-absence rule does not apply here: any can-go-unmatched group
/// contributes `''` (or `null` under PREG_UNMATCHED_AS_NULL) to its column
/// wherever it sits (measured: `preg_match_all('/(\d)(a)?/', '1a 2 3a', $m)`
/// gives `$m[2] === ['a', '', 'a']`). So the element consults the reader's raw
/// [`can_go_unmatched`](steins_catalog::preg::CaptureGroup::can_go_unmatched)
/// bit and NEVER the slice-E middle-vs-trailing projections — reusing the
/// per-set element rule would refine a column that holds `''` on a reachable
/// path.
///
/// Entry `0`'s element is the whole-expression refinement from slice E: the
/// whole match always participates (a zero-width match contributes `''`, which
/// its own floor already accounts for — a floor of 0 is plain `string`).
fn preg_pattern_order_shape(
    groups: &steins_catalog::preg::CaptureGroups,
    flags: PregFlags,
) -> Option<Fact> {
    use steins_domain::{Presence, SHAPE_WIDTH_LIMIT, Tail};

    let required = Presence::Required { witnessed: false };
    let slot = |fact: Fact| Some(Box::new(fact));
    let column = |elem: Fact| list_transfer_fact(true, Some(elem));

    let entry0 = {
        let e = preg_element_fact(&groups.whole);
        if flags.offset_capture { preg_offset_pair(e, false)? } else { e }
    };
    let mut fields = vec![(VKey::Int(0), required, slot(column(entry0)))];
    for (i, g) in groups.groups.iter().enumerate() {
        // The padding rule: position never matters, only whether the group can
        // go unmatched at all.
        let elem = if flags.unmatched_as_null {
            let body = preg_group_element_fact(g);
            if g.can_go_unmatched { preg_nullable_element(body) } else { body }
        } else if g.can_go_unmatched {
            preg_padded_element(preg_group_element_fact(g))
        } else {
            preg_group_element_fact(g)
        };
        let elem = if flags.offset_capture {
            // A padded entry is `['', -1]` / `[null, -1]` (measured), so `-1` is
            // reachable exactly for the groups whose column is padded.
            preg_offset_pair(elem, g.can_go_unmatched)?
        } else {
            elem
        };
        let col = column(elem);
        let index = i64::try_from(i + 1).ok()?;
        fields.push((VKey::Int(index), required, slot(col.clone())));
        if let Some(name) = &g.name {
            fields.push((VKey::Str(name.clone().into()), required, slot(col)));
        }
    }
    if fields.len() > SHAPE_WIDTH_LIMIT {
        return None;
    }
    let list = groups.groups.iter().all(|g| g.name.is_none());
    let shape = ShapeFact::normalize(
        fields,
        Tail::Sealed,
        if list { Certainty::Yes } else { Certainty::Maybe },
        true,
        Vec::new(),
    );
    Some(Fact::Shape { shape: Box::new(shape), nullable: false })
}

/// An element for an entry whose unmatched case is a **written** `''` — the
/// PATTERN_ORDER padded column (issue #168 rule 2) and the `preg_match`
/// present-empty interior entry (issue #177). The union is computed by the
/// domain's own join: a floor refinement collapses against `''` to plain
/// `string` (none of the floor predicates admit the empty string), a literal
/// union (slice F) gains `''` as one more member, and an unrepresentable join
/// degrades to plain `string`, which is the sound side either way.
fn preg_padded_element(body: Fact) -> Fact {
    body.join(&Fact::Singleton(Val::Str(PhpStr::new())))
        .unwrap_or(Fact::General { base: Base::String, nullable: false })
}

/// A group element under `PREG_UNMATCHED_AS_NULL`: the body's element type with
/// `null` added — optionality (or `''` padding) turned into nullability, which
/// the flag is for.
fn preg_nullable_element(body: Fact) -> Fact {
    match body {
        Fact::Refined { base, refinement, .. } => {
            Fact::Refined { base, refinement, nullable: true }
        }
        Fact::General { base, .. } => Fact::General { base, nullable: true },
        // The literal-element layers (issue #177): the join folds `null` into
        // the finite set (`'b'` becomes `'b'|null`), and an unrepresentable
        // join degrades to nullable `string`, the sound side.
        other => other
            .join(&Fact::Singleton(Val::Null))
            .unwrap_or(Fact::General { base: Base::String, nullable: true }),
    }
}

/// One entry under `PREG_OFFSET_CAPTURE`: the measured `[text, offset]` pair as
/// a sealed `list{T, int<lo, max>}`.
///
/// The offset floor was probed rather than assumed (issue #168 rule 5): a
/// participating group's offset is a byte position, `>= 0`; an unmatched
/// group's **written** entry is `['', -1]` — `[null, -1]` under
/// `PREG_UNMATCHED_AS_NULL` — so `-1` is reachable exactly when
/// `unmatched_written`, and the floor is `0` everywhere else (notably entry
/// `0`, which participates by definition).
fn preg_offset_pair(elem: Fact, unmatched_written: bool) -> Option<Fact> {
    use steins_domain::{IntRange, Presence, Tail};

    let required = Presence::Required { witnessed: false };
    let lo = if unmatched_written { -1 } else { 0 };
    let offset = Fact::refined(Base::Int, Refinement::Int(IntRange::new(lo, i64::MAX)?), false);
    let shape = ShapeFact::normalize(
        vec![
            (VKey::Int(0), required, Some(Box::new(elem))),
            (VKey::Int(1), required, Some(Box::new(offset))),
        ],
        Tail::Sealed,
        Certainty::Yes,
        true,
        Vec::new(),
    );
    Some(Fact::Shape { shape: Box::new(shape), nullable: false })
}

/// Bind an out-parameter seed on a branch env (ADR-0077 §3.4).
///
/// The order is fixed, seed second: [`walk_if`] has already run
/// [`cond_invalidations`] over the pre-branch env, so the name reaches this
/// forgotten and this rebinds it — no race. The statement rung reaches the same
/// arrangement by its own route ([`stmt_out_param_seeds`] reads first, then
/// [`apply_stmt_out_param_seeds`] binds after step 4's forgetting).
///
/// `stratum` is the row's, not this function's: the preg rows bind `Asserted`
/// (§3.3 — a declared contract plus proven inputs, never an observed run), the
/// `settype` row inherits its input's. Either way a seed may silence but only a
/// `Verified` one may premise a proof.
///
/// [`walk_if`]: crate::branch::walk_if
/// [`cond_invalidations`]: crate::asserts::cond_invalidations
fn seed_out_param(
    var: &str,
    fact: Fact,
    stratum: Stratum,
    bound: &str,
    line: u32,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
) {
    env.insert(var.to_owned(), Known::value_strat(fact, line, Some(bound.to_owned()), stratum));
    // The callee assigned the whole variable, so the guard-derived class facts and
    // the declared-arm lane the old value earned are void — the same reasoning any
    // rebinding applies.
    store.unbind(var);
}
