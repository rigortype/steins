//! `@phpstan-assert` application (ADR-0030, Feature D): after a call to an
//! annotated helper, the asserted lanes are applied to the variables the assert
//! names, with the guard-position kept lanes, condition invalidations and the
//! opaque reads a condition can perform.

use std::collections::{HashMap, HashSet};

use steins_contract::normalize;
use steins_domain::{Base, Fact, Refinement, Val};
use steins_phpdoc::AssertKind;
use steins_syntax::{ArgValue, CallExpr, Callee, CondExpr, CondOperand, Param, Receiver, Scope};

use crate::contract::{AssertSpec, ProjectIsa};
use crate::cx::Cx;
use crate::dispatch::resolve_call_target;
use crate::env::{ContractArm, Known, Store, Stratum};
use crate::predicates::{in_array_literals, type_predicate};
use crate::refine::{clear_null, refine_fact, subtract_contract_lane};
use crate::shapes::{
    apply_shape_guard, array_all_any_predicate, array_guard_base, array_guard_predicate,
    collect_shape_guards,
};
use crate::walk::{WalkCx, by_value_survivors};

// ---------------------------------------------------------------------------
// `@phpstan-assert` application (ADR-0030, Feature D). After a call to an
// assertion helper, the asserted type narrows the CALLER's env for the variable
// passed at the asserted position. `Always` asserts apply on the fall-through
// (statement position); `-if-true`/`-if-false` apply only in guard position.
// ---------------------------------------------------------------------------

/// Convert a lowered contract type to the domain [`Fact`] an assertion of it
/// establishes (conservative): `Base` → General, `IntIn` → Refined, `StrWith` →
/// Refined, `Null` → `Singleton(null)`, a nullable union (`X|null`) → `X`'s fact
/// with `nullable = true`; anything else → `None` (no application).
fn assert_fact_of(cty: &steins_contract::ContractTy) -> Option<Fact> {
    use steins_contract::ContractTy as C;
    match cty {
        C::Base(b) => Some(Fact::General { base: *b, nullable: false }),
        C::IntIn(r) => Some(Fact::refined(Base::Int, Refinement::Int(*r), false)),
        C::StrWith(p) => Some(Fact::refined(Base::String, Refinement::Str(*p), false)),
        C::Null => Some(Fact::Singleton(Val::Null)),
        // Literal claims (`@phpstan-assert 'hi' $v`, `42`, `true`) are Singleton
        // facts — the same denotation `steins_contract::fact_of` uses. Needed for
        // Asserted capture/fold summaries (issues #127 / #128) without laundering.
        C::LitInt(i) => Some(Fact::Singleton(Val::Int(*i))),
        C::LitFloat(f) => Some(Fact::Singleton(Val::Float(*f))),
        C::LitStr(s) => Some(Fact::Singleton(Val::Str(s.clone()))),
        C::LitBool(b) => Some(Fact::Singleton(Val::Bool(*b))),
        C::Union(members) => {
            let has_null = members.iter().any(|m| matches!(m, C::Null));
            let non_null: Vec<&C> = members.iter().filter(|m| !matches!(m, C::Null)).collect();
            // `X|null` (exactly one representable non-null member) → X, nullable.
            if has_null && non_null.len() == 1 {
                return Some(with_nullable(assert_fact_of(non_null[0])?));
            }
            None
        }
        _ => None,
    }
}

/// Set the `nullable` flag on an abstract fact (a `Singleton`/`OneOf` is left
/// unchanged — a nullable-union member never lowers to a finite fact here).
fn with_nullable(f: Fact) -> Fact {
    match f {
        Fact::General { base, .. } => Fact::General { base, nullable: true },
        Fact::Refined { base, refinement, .. } => Fact::refined(base, refinement, true),
        other => other,
    }
}

/// Apply the `Always` assertions of every statically-resolved call in a statement
/// to the caller's env (Feature D) — the fall-through position. `-if-true`/
/// `-if-false` asserts are conditional on the boolean result and belong to guard
/// position (see [`apply_guard_asserts`]).
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_stmt_asserts(
    cx: &Cx,
    scope: &Scope,
    call: &CallExpr,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
    this_exact: Option<&str>,
    enclosing_class: Option<&str>,
    asserted: &mut HashSet<String>,
) {
    apply_call_asserts(cx, scope, call, env, store, this_exact, enclosing_class, AssertKind::Always, asserted);
}

/// The source line of a guard call — the provenance line an out-parameter seed
/// binds at, which is where the callee performed the write.
pub(crate) fn guard_call_line(w: &WalkCx, call: &CallExpr) -> u32 {
    w.cx.tree().position(call.span.start).line
}

/// Apply a guard-position call's `@phpstan-assert-if-true`/`-if-false` specs to a
/// branch env (ADR-0052 §5, at the `Asserted` stratum). `kind` selects the
/// polarity: `IfTrue` on the true branch, `IfFalse` on the false branch. This is
/// the *minimal* guard-call tag consumption — the full retained-guard-call
/// machinery (§6) is N3.
pub(crate) fn apply_guard_asserts(
    w: &WalkCx,
    call: &CallExpr,
    kind: AssertKind,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
) {
    let mut asserted = HashSet::new();
    apply_call_asserts(
        w.cx, w.scope, call, env, store, w.this_exact, w.enclosing_class, kind, &mut asserted,
    );
}

/// Resolve a call's callee declaration and apply every assertion spec of a given
/// `kind`, mapping each spec's `@param` name to the call's positional argument
/// variable and narrowing it via [`apply_assert_to_var`] (always at the `Asserted`
/// stratum). Shared by the fall-through (`Always`) and guard (`IfTrue`/`IfFalse`)
/// consumption points.
#[allow(clippy::too_many_arguments)]
fn apply_call_asserts(
    cx: &Cx,
    scope: &Scope,
    call: &CallExpr,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
    this_exact: Option<&str>,
    enclosing_class: Option<&str>,
    kind: AssertKind,
    asserted: &mut HashSet<String>,
) {
    if scope.poisoned || !call.positional_only {
        return;
    }
    let Some(callee) = assert_callee(cx, call, store, this_exact, enclosing_class, scope.poisoned)
    else {
        return;
    };
    let (params, decl_at) = (callee.params, callee.decl_at);
    let Some(envelopes) = cx.envelopes_of(callee.docblock, decl_at.0, decl_at.1) else { return };
    for spec in &envelopes.asserts {
        if spec.kind != kind {
            continue;
        }
        let Some(pos) = params.iter().position(|p| p.name == spec.param) else { continue };
        // The assertion-helper leg: the spec asserts a BOOLEAN of the parameter,
        // and the argument at that position is a condition. Read it as the guard
        // it is, before the value-fact lane gets a chance to make nothing of it.
        if let Some(then) = asserted_boolean(spec)
            && let Some(cond) = call.arg_cond(pos)
        {
            apply_helper_guard(cx, call, cond, then, env, store, asserted);
            continue;
        }
        let Some(arg) = call.args.get(pos) else { continue };
        let ArgValue::Var(v) = &arg.value else { continue };
        if apply_assert_to_var(cx, decl_at, env, store, v, spec) {
            asserted.insert(v.clone());
        }
    }
}

/// The declaration a call's `@phpstan-assert` envelopes are read from: its
/// parameter list, its docblock, and its **own name-resolution context**
/// `(file, offset)`.
///
/// The last element makes a class-typed spec resolvable: an assert tag's class
/// name is written in the *callee's* namespace, not the caller's —
/// `@phpstan-assert Guest $v` declared in `App\Auth` means `App\Auth\Guest`
/// wherever called from. Reading it there is a query answer, not cross-scope
/// walk state (ADR-0048 §2 replayability).
struct AssertCallee<'a> {
    params: &'a [Param],
    docblock: Option<&'a str>,
    /// The declaration's own `(file, offset)` — the context a class name in an
    /// assert tag resolves in.
    decl_at: (usize, u32),
}

fn assert_callee<'a>(
    cx: &Cx<'a>,
    call: &CallExpr,
    store: &Store,
    this_exact: Option<&str>,
    enclosing_class: Option<&str>,
    poisoned: bool,
) -> Option<AssertCallee<'a>> {
    match &call.receiver {
        Callee::Function(_) => {
            let site = cx.resolve_user_fn(call)?;
            let decl = cx.fn_decl(site);
            Some(AssertCallee {
                params: &decl.params,
                docblock: decl.docblock.as_deref(),
                decl_at: (site.file, decl.span.start),
            })
        }
        Callee::Method { .. } | Callee::Static { .. } | Callee::Construct { .. } => {
            let target =
                resolve_call_target(cx, &call.receiver, store, this_exact, enclosing_class, poisoned)?;
            Some(AssertCallee {
                params: &target.method.params,
                docblock: target.method.docblock.as_deref(),
                decl_at: (target.class_file, target.method.span.start),
            })
        }
        // A `$fn(...)` variable call carries no static declaration to read
        // `@phpstan-assert` envelopes from — nothing to apply.
        Callee::DynamicVar(_) | Callee::Dynamic => None,
    }
}

/// The declared-arm lanes that must survive a guard's conservative read-set drop:
/// one entry per variable a **class-typed** assert spec of a guard call names,
/// and only where the callee provably cannot have rebound it.
///
/// Three gates, each a refusal rather than a heuristic: (1) the spec's type
/// must lower to a `Class` — the arm-lane road, no other spec kind reaches it;
/// (2) the callee's parameter at the asserted position must be **by value**
/// (ADR-0070) — a separate zval the call cannot write the caller's binding
/// through; (3) the variable must appear **nowhere else** in the condition's
/// calls — one by-value occurrence proves nothing about a second occurrence
/// that could take a reference.
///
/// Only the arm lane is carried across; the value lane and `Member` sets still
/// drop with everything else, so no *fact* survives a guard here that did not
/// survive one before.
pub(crate) fn guard_assert_kept_lanes(
    w: &WalkCx,
    cond: &CondExpr,
    guard_calls: &[&CallExpr],
    env: &HashMap<String, Known>,
    store: &Store,
) -> Vec<(String, Vec<ContractArm>)> {
    let mut kept: Vec<(String, Vec<ContractArm>)> = Vec::new();
    if w.scope.poisoned {
        return kept;
    }
    let invalidated = cond_invalidations(w.cx, cond, env, store, w.scope.poisoned);
    for call in guard_calls {
        if !call.positional_only {
            continue;
        }
        let Some(callee) = assert_callee(
            w.cx, call, store, w.this_exact, w.enclosing_class, w.scope.poisoned,
        ) else {
            continue;
        };
        let params = callee.params;
        let (cfile, coff) = callee.decl_at;
        let Some(envelopes) = w.cx.envelopes_of(callee.docblock, cfile, coff) else { continue };
        for spec in &envelopes.asserts {
            if !matches!(steins_contract::lower(&spec.ty), steins_contract::ContractTy::Class(_)) {
                continue;
            }
            let Some(pos) = params.iter().position(|p| p.name == spec.param) else { continue };
            if params[pos].by_ref {
                continue;
            }
            let Some(ArgValue::Var(v)) = call.args.get(pos).map(|a| &a.value) else { continue };
            if !invalidated.iter().any(|name| name == v) {
                // Nothing is dropping this lane — no rescue needed.
                continue;
            }
            if occurs_elsewhere_in_calls(guard_calls, pos, v) {
                continue;
            }
            let Some(arms) = store.contract.get(v) else { continue };
            if !kept.iter().any(|(name, _)| name == v) {
                kept.push((v.clone(), arms.clone()));
            }
        }
    }
    kept
}

/// Does `var` occur in any of these calls other than as the positional argument at
/// `pos` — as another argument, a named argument, or a method receiver?
fn occurs_elsewhere_in_calls(calls: &[&CallExpr], pos: usize, var: &str) -> bool {
    let mut seen_at_pos = false;
    for call in calls {
        if let Callee::Method { receiver: Receiver::Var(v), .. } = &call.receiver
            && v == var
        {
            return true;
        }
        for (i, arg) in call.args.iter().enumerate() {
            if matches!(&arg.value, ArgValue::Var(v) if v == var) {
                if i == pos && !seen_at_pos {
                    seen_at_pos = true;
                } else {
                    return true;
                }
            }
        }
        if call.named_args.iter().any(|n| matches!(&n.value, ArgValue::Var(v) if v == var)) {
            return true;
        }
    }
    false
}

/// The boolean an assertion spec claims of its parameter, or `None` when the spec
/// asserts something other than a literal `true`/`false`.
///
/// Four spellings collapse to two answers, and negation is a plain XOR because the
/// asserted subject is a *condition* — `isset(…)`, a comparison, a `&&` chain —
/// whose value is a `bool` by construction, so "not `true`" is "`false`":
/// `@phpstan-assert true $c` and `@phpstan-assert !false $c` both say the guard
/// held; `@phpstan-assert false $c` and `@phpstan-assert !true $c` both say it did
/// not.
fn asserted_boolean(spec: &AssertSpec) -> Option<bool> {
    match steins_contract::lower(&spec.ty) {
        steins_contract::ContractTy::LitBool(b) => Some(b != spec.negated),
        _ => None,
    }
}

/// **Assertion-helper discharge** (ADR-0058 §3's tag lane, ADR-0062 A-G10's
/// discharge ladder): route a userland assertion helper's condition argument
/// through the SAME guard walk `assert($cond)` uses.
///
/// A house helper asserting `isset($options['key'])` before the read is the
/// corpus pattern this exists for. `assert(isset(…))` discharges the strict leg
/// because its argument survives lowering as a condition; the helper form did
/// not, since `isset(…)`'s value lowering is [`ArgValue::Other`] — nothing to
/// consume. With the condition retained (`CallExpr::arg_conds`) the two forms
/// differ in exactly the one respect ADR-0058 legislates: **stratum**.
/// `assert()` is *Verified, unconditionally* (the 2026-07-25 ruling reads it as
/// `if (!$cond) throw`); a helper carrying only a `@phpstan-assert` tag is
/// **Asserted** (ADR-0058's table row "userland helper, tag-declared only" —
/// its §8: the tag lane is a claim, a lying tag must not forge a proof), so the
/// presence promotion here is `Required { witnessed: false }`. The discharge is
/// unaffected — `offset.maybe-missing` is a contract-layer finding over an
/// `Asserted` shape (A-G9's corollary), so an Asserted presence silences it just
/// as a witnessed one does, and no proof-layer id can be premised on either.
/// Raising this leg to Verified needs the descent proof of ADR-0058 §3 (slice
/// I2), reading the helper's throw-guard out of its body rather than trusting
/// the tag; outside this rule. Everything else — polarity, `&&`/`||`
/// distribution, the S5 disjunctive cover, tag discrimination, arm subtraction —
/// is the walk's, unmodified.
///
/// The by-ref exemption is the second half. `assert()` never forgets its
/// argument; a helper call *does* — the lowering conservatively forgets every
/// variable the call expression mentions, including the base inside
/// `isset($d['a'])`, erasing the narrowing one statement later. A variable
/// mentioned only inside a condition argument cannot be bound by reference (PHP
/// binds a reference to a variable or lvalue, never to the value of `isset(…)`
/// or `$a && $b`), so it is exempt — unless the SAME call also hands that
/// variable over directly, the one case that can mutate.
#[allow(clippy::too_many_arguments)]
fn apply_helper_guard(
    cx: &Cx,
    call: &CallExpr,
    cond: &CondExpr,
    then: bool,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
    asserted: &mut HashSet<String>,
) {
    let mut guards = Vec::new();
    collect_shape_guards(cx, cond, then, env, &mut guards);
    for g in &guards {
        // ADR-0058: tag-declared, so the presence stratum is the declared one.
        apply_shape_guard(cx, g, env, store, false);
        let var = g.var();
        let handed_over = call
            .args
            .iter()
            .map(|a| &a.value)
            .chain(call.named_args.iter().map(|n| &n.value))
            .any(|v| matches!(v, ArgValue::Var(name) if name == var));
        if !handed_over {
            asserted.insert(var.to_owned());
        }
    }
}

/// Apply one assertion spec to a caller variable at the **`Asserted`** stratum
/// (ADR-0052 §5 — a docblock is a claim, never a proof). Replace-if-weaker: a
/// stronger finite fact (`Singleton`/`OneOf`) is kept (an assert never coarsens
/// known-exact knowledge), and **an `Asserted` fact never overwrites a
/// `Verified` one of any layer** (a lying `@phpstan-assert` cannot downgrade a
/// proven fact into a forgeable one, nor launder its claim past the stratum
/// gate). A negated `!null` clears nullability (also `Asserted`); other negated
/// forms are not representable as a positive fact and are skipped.
///
/// A **class-typed** spec (`@phpstan-assert Guest $v`, and its negation) takes
/// the other road: the four-layer value domain is object-free by construction
/// (ADR-0035/0043 §4), so `assert_fact_of` declines every `Class` arm. Class
/// claims belong to the **class carriers** of §1, routed to the one that is
/// stratified: the **contract arm lane**, narrowed arm-wise through
/// `normalize::subtract_arm` — the same judgment an `instanceof` guard uses,
/// same polarity asymmetry (§2's class-arm rule). That lane is ADR-0052 §3(d)'s
/// named consumer (the declared-receiver lane, `phpdoc.undefined-method`,
/// contract-layer), and its routing already grades by minimum arm stratum, so a
/// lying tag can buy the contract-layer finding it is entitled to and no proof.
///
/// The **`Member` carrier is refused here**: it has no stratum slot (documented
/// bound at `Verified`), and its consumers include `eval_instanceof` implication
/// (§3(b)), which decides verdicts and prunes branches. Feeding a docblock claim
/// into a *reachability* decision is the laundering §5 exists to prevent — the
/// fix is a stratum on `Member`, not a quiet insertion. Recorded deferral.
///
/// Returns whether the variable now carries an established fact (protecting it
/// from the by-ref invalidation) — `true` when a fact was set or a
/// stronger/Verified fact was deliberately kept, `false` otherwise.
fn apply_assert_to_var(
    cx: &Cx,
    decl_at: (usize, u32),
    env: &mut HashMap<String, Known>,
    store: &mut Store,
    var: &str,
    spec: &AssertSpec,
) -> bool {
    let cty = steins_contract::lower(&spec.ty);
    // The class carrier, both polarities, before the value-lane road: a positive
    // spec subtracts as `instanceof T` does (an arm dies only when it is final/enum
    // and provably not a `T`), a negated one as `!($v instanceof T)` does (an arm
    // dies iff it is-a `T`). An `Unknown` is-a keeps the arm either way.
    if let steins_contract::ContractTy::Class(name) = &cty {
        return assert_class_to_lane(cx, decl_at, store, var, name, !spec.negated);
    }
    if spec.negated {
        // A negated **scalar base** subtracts on the arm lane, the road the negated
        // class spec above already takes (issue #391): `@phpstan-assert !int $x`
        // over a declared `int|string` leaves `{string}`. The judgment is ADR-0052
        // §2's, unchanged — an arm dies iff the subtrahend covers it with `Yes`, so
        // a `mixed`/`scalar` arm keeps its interior points and survives.
        //
        // Arm lane only. The value lane's operator for "this base is gone" is a
        // union subtraction the refinement vocabulary does not have, and inventing
        // one here would be a second narrowing relation for one tag.
        //
        // **`!float` is refused**, and the refusal is the interesting half. The
        // arm judgment is contract *acceptance*, under which `int` is subsumed by
        // `float` (a `float` parameter takes an int) — but `is_float(1)` is false,
        // so reading that acceptance as "the value is a float" and deleting the
        // `int` arm would narrow away a live value. No other base widens across
        // bases this way, so the carve-out is exactly one row wide.
        if let steins_contract::ContractTy::Base(b) = cty
            && b != Base::Float
            && store.contract.contains_key(var)
        {
            let oracle = ProjectIsa { cx, demote_catalog: cx.a11_demote_catalog() };
            subtract_contract_lane(store, var, &normalize::Subtrahend::Base(b), &oracle);
            return true;
        }
        // Only `!null` is representable as a positive narrowing (clear nullable);
        // other negated forms establish nothing. The narrowing is `Asserted`, so
        // `refine_fact` mins the result to `Asserted`.
        // The presence test is deliberately blind to a `Fact::Shape` entry
        // (ADR-0062 S3): the shape seed is entry-state provenance, not an
        // assert-visible narrowing target, and `clear_null` is a no-op on it
        // anyway. Counting it would flip this to `true` for every array param and
        // so protect the variable from the by-ref invalidation sweep — a control
        // change with nothing behind it. Shape narrowing (including clearing a
        // nullable shape) is S4's, and arrives with its own refinement operator.
        if matches!(cty, steins_contract::ContractTy::Null)
            && env
                .get(var)
                .is_some_and(|k| !matches!(k.fact, Some(Fact::Shape { .. })))
        {
            refine_fact(env, var, Stratum::Asserted, clear_null);
            return true;
        }
        return false;
    }
    let Some(fact) = assert_fact_of(&cty) else { return false };
    // Keep the existing fact when it is a stronger finite layer OR when it is
    // already `Verified` (replace-if-weaker, both halves): an `Asserted` claim may
    // neither coarsen exact knowledge nor overwrite a proven fact. Either way the
    // variable stays protected from the by-ref invalidation (a by-value assert did
    // not mutate it).
    if env.get(var).is_some_and(|k| {
        k.stratum == Stratum::Verified || k.fact.as_ref().is_some_and(|f| f.finite_members().is_some())
    }) {
        return true;
    }
    env.insert(var.to_owned(), Known::value_strat(fact, 0, Some("asserted".to_owned()), Stratum::Asserted));
    store.unbind(var);
    true
}

/// Narrow `var`'s contract arm lane by a class-typed assertion spec, resolved in
/// the **callee's** namespace context (`decl_at`).
///
/// Returns whether the lane was actually there to narrow — the by-ref protection
/// answer, and honest: a subject with no declared arms learned nothing here, so
/// there is nothing to protect.
fn assert_class_to_lane(
    cx: &Cx,
    decl_at: (usize, u32),
    store: &mut Store,
    var: &str,
    name: &str,
    positive: bool,
) -> bool {
    if !store.contract.contains_key(var) {
        return false;
    }
    let (file, offset) = decl_at;
    let fqn = cx.resolve_pclass(file, offset, name).trim_start_matches('\\').to_ascii_lowercase();
    let oracle = ProjectIsa { cx, demote_catalog: cx.a11_demote_catalog() };
    subtract_contract_lane(
        store,
        var,
        &normalize::Subtrahend::Class { fqn, polarity: positive },
        &oracle,
    );
    true
}

/// Every bare variable a guard may mutate by reference, and therefore forgets on
/// both paths.
///
/// **The S4 exemption, and how narrow it is.** `isset($x['k'])`,
/// `array_key_exists('k', $x)` and `array_is_list($x)` cannot mutate anything —
/// the first is not even a function call, the other two are pure by-value
/// builtins. Before S4 all three forgot their base regardless (pure
/// conservatism). Lifting that wholesale would let every lane see facts across
/// such a guard for the first time, and a proven `Singleton` array surviving
/// into the branch can premise a proof-layer `offset.missing` that did not fire
/// before. So the exemption is granted only to a base carrying the **shape
/// lane** — a `Fact::Shape`, or a contract lane with an array arm — exactly what
/// this narrowing consumes and `Asserted` end to end (A-G9). A base mentioned
/// anywhere else in the same condition keeps the old forgetting, since that
/// other mention is what might mutate it.
///
/// What the exemption is *about* is the base: the arguments a pure guard reads
/// to answer a question about that base are [`push_pure_guard_bases`]' concern,
/// and never forgotten (issue #536).
pub(crate) fn cond_invalidations(
    cx: &Cx,
    cond: &CondExpr,
    env: &HashMap<String, Known>,
    store: &Store,
    poisoned: bool,
) -> Vec<String> {
    let mut out = Vec::new();
    collect_cond_opaque_reads(cx, cond, &CondEnv { env, store, poisoned }, &mut out);
    let mut pure = Vec::new();
    collect_pure_guard_bases(cx, cond, &mut pure);
    for v in pure {
        if out.contains(&v) || shape_lane_present(&v, env, store) {
            continue;
        }
        out.push(v);
    }
    out
}

/// The bases of guards that provably cannot mutate: the `isset` form and the
/// recognized pure array builtins.
fn collect_pure_guard_bases(cx: &Cx, cond: &CondExpr, out: &mut Vec<String>) {
    match cond {
        CondExpr::Isset { var, .. } => {
            if !out.contains(var) {
                out.push(var.clone());
            }
        }
        CondExpr::Call { call, reads }
            if array_guard_predicate(cx, call).is_some()
                || array_all_any_predicate(cx, call).is_some() =>
        {
            push_pure_guard_bases(cx, call, reads, out);
        }
        // A pure predicate keeps its exemption when it is *compared* rather than
        // tested (`array_is_list($x) === true`): position within the condition
        // does not change what a call can do, in either direction.
        CondExpr::Cmp { lhs, rhs, .. } => {
            for operand in [lhs, rhs] {
                let CondOperand::Other { call: Some(call), invalidates, .. } = operand else {
                    continue;
                };
                if array_guard_predicate(cx, call).is_some()
                    || array_all_any_predicate(cx, call).is_some()
                {
                    push_pure_guard_bases(cx, call, invalidates, out);
                }
            }
        }
        CondExpr::Not(c) => collect_pure_guard_bases(cx, c, out),
        CondExpr::And(a, b) | CondExpr::Or(a, b) => {
            collect_pure_guard_bases(cx, a, out);
            collect_pure_guard_bases(cx, b, out);
        }
        _ => {}
    }
}

/// The names one recognized pure array call still owes the S4 forgetting.
///
/// For the presence/list family that is its **array argument alone** (issue
/// #536): `array_key_exists($key, $values)` reads `$key` to answer a question
/// ABOUT `$values`, and charging the forgetting to `$key` as well took its
/// declared arms away on both branches of a guard that never narrowed them —
/// and on the fall-through of a guard nobody even consumed. A base that is not
/// a bare name (`array_key_exists($k, $o->p)`)
/// keeps the blanket forgetting: with no name to tell the base from the key,
/// telling them apart here would be guesswork.
///
/// `array_all`/`array_any` keep it too, unconditionally: their second argument
/// is a **callback**, and what a callback does to the names this condition
/// mentions is not a question the by-value signature answers.
fn push_pure_guard_bases(cx: &Cx, call: &CallExpr, reads: &[String], out: &mut Vec<String>) {
    if let Some(base) = array_guard_base(cx, call) {
        if !out.contains(base) {
            out.push(base.clone());
        }
        return;
    }
    for r in reads {
        if !out.contains(r) {
            out.push(r.clone());
        }
    }
}

/// Does `var` carry the shape lane — either half of it? The fact half is empty
/// for a *union* of array shapes by design (A-G3 keeps the union in the arm
/// lane), so testing the fact alone would leave exactly the discriminated-union
/// case unexempted.
fn shape_lane_present(var: &str, env: &HashMap<String, Known>, store: &Store) -> bool {
    if env.get(var).is_some_and(|k| matches!(k.fact, Some(Fact::Shape { .. }))) {
        return true;
    }
    store
        .contract
        .get(var)
        .is_some_and(|arms| arms.iter().any(|a| steins_contract::to_shape_fact(&a.ty).is_some()))
}

/// The walk-local state the operand arm of [`collect_cond_opaque_reads`] needs
/// to run ADR-0070's by-value gate — everything the gate asks that the condition
/// itself cannot answer.
struct CondEnv<'a> {
    env: &'a HashMap<String, Known>,
    store: &'a Store,
    poisoned: bool,
}

fn collect_cond_opaque_reads(cx: &Cx, cond: &CondExpr, ce: &CondEnv, out: &mut Vec<String>) {
    match cond {
        // An opaque condition may mutate any variable it reads by reference — the
        // whole read-set is forgotten (the conservative floor, unchanged).
        CondExpr::Opaque { reads } => {
            for r in reads {
                if !out.contains(r) {
                    out.push(r.clone());
                }
            }
        }
        CondExpr::Call { call, reads } => collect_call_opaque_reads(cx, call, reads, out),
        // **Operand position** (issue #158). A call does not become harmless by
        // sitting inside a comparison: `preg_match($re, $s, $m) === 1` writes `$m`
        // exactly as the bare guard does. The rule: an operand's writes are judged
        // by the same policy as a guard call's — position within the condition
        // changes nothing about what a call can do.
        CondExpr::Cmp { lhs, rhs, .. } => {
            collect_operand_opaque_reads(cx, lhs, ce, out);
            collect_operand_opaque_reads(cx, rhs, ce, out);
        }
        // `f($x, $m) instanceof Foo` — the same gap, through the other
        // operand-carrying variant. (`Truthy`'s operand can only be an `Offset`
        // here: `lower_cond` routes a call or other unrepresentable condition to
        // `Call`/`Opaque` before it can become a `Truthy(Other)`.)
        CondExpr::Instanceof { operand, .. } => {
            collect_operand_opaque_reads(cx, operand, ce, out);
        }
        CondExpr::Not(c) => collect_cond_opaque_reads(cx, c, ce, out),
        CondExpr::And(a, b) | CondExpr::Or(a, b) => {
            collect_cond_opaque_reads(cx, a, ce, out);
            collect_cond_opaque_reads(cx, b, ce, out);
        }
        _ => {}
    }
}

/// The invalidation a comparison/`instanceof` operand contributes: nothing for
/// the modelled variants (a bare variable, a literal, a constant-key projection,
/// a constant fetch — none of them writes), and for [`CondOperand::Other`]
/// whatever its write set earns under the guard-call policy above, **minus what
/// ADR-0070's by-value gate proves the callee could not reach**.
///
/// The gate runs here rather than on [`CondExpr::Call`] because this is where
/// the floor is chosen for the first time — the blanket read-set drop in guard
/// position is pre-existing and measured, and lifting it there would let facts
/// survive guards that never let them through before. Choosing the *precise*
/// sound rule for a path that had no rule at all costs nothing and keeps 191
/// nsrt observations (`count($listA) === count($listB)`, `strstr($s, 'a') ===
/// 'b'`) that the missing invalidation had been holding up.
fn collect_operand_opaque_reads(
    cx: &Cx,
    operand: &CondOperand,
    ce: &CondEnv,
    out: &mut Vec<String>,
) {
    let CondOperand::Other { call, invalidates, sites } = operand else { return };
    let mut floor = Vec::new();
    match call {
        // The operand *is* a resolvable call: it gets every exemption a guard
        // call gets, including the by-value predicate families and the
        // method-receiver survival.
        Some(call) => collect_call_opaque_reads(cx, call, invalidates, &mut floor),
        // A write the lowering could not name a callee for — a dynamic call, a
        // call nested inside arithmetic, an assignment or an increment
        // (`($x = f()) === 1`, `$i++ === 5`). No call-shaped exemption applies to
        // something this walk cannot identify, so the whole set is the floor.
        None => floor = invalidates.clone(),
    }
    let survivors = by_value_survivors(cx, ce.poisoned, sites, ce.env, ce.store);
    for r in floor {
        if !survivors.contains(r.as_str()) && !out.contains(&r) {
            out.push(r);
        }
    }
}

/// One retained call's contribution to the invalidation set — **the single
/// policy for what a call in a condition forgets**, wherever in the condition it
/// sits (guard position, or an operand of a comparison/`instanceof`).
///
/// The general rule is its `reads`, minus the pure method receiver (`$x` in
/// `$x->m()` is not rebound by the call, only its object's props are swept;
/// ADR-0052 §6 payoff (i)) — surviving only when not also handed in as an
/// argument (`$x->m($x)` is still forgotten). Two families are exempt outright:
///
/// **The S4 exemption.** A recognized pure array builtin mutates nothing; its
/// bases are decided by [`collect_pure_guard_bases`] + the shape-lane test
/// instead. `array_all`/`array_any` (A8) join this set: neither parameter is
/// by-ref in PHP's own signature, so the base array is as safe as
/// `array_is_list`'s — any *other* alias risk (a callback closing over the base
/// by reference) is still caught, since the shape-lane gate only exempts a read
/// that carries it, and a by-ref-captured var generally won't.
///
/// **The DR2 exemption, unconditional** (unlike S4's shape-lane gate). The
/// `is_*` family and `in_array` declare every parameter BY VALUE in PHP's own
/// signature and are side-effect free, so a guard call cannot have changed the
/// base between the test and the branch — here the base's scalar fact surviving
/// IS the point of the slice, and every finding it premises is true by
/// construction, since the value the branch sees is the value the predicate
/// tested. A base mentioned by any OTHER call in the same condition is still
/// forgotten — that mention is what might mutate it, collected by that call's
/// own visit.
fn collect_call_opaque_reads(cx: &Cx, call: &CallExpr, reads: &[String], out: &mut Vec<String>) {
    if array_guard_predicate(cx, call).is_some()
        || array_all_any_predicate(cx, call).is_some()
        || type_predicate(cx, call).is_some()
        || in_array_literals(cx, call, cx.php_minor).is_some()
    {
        return;
    }
    let recv = call_method_receiver_var(call);
    let recv_is_arg = recv
        .is_some_and(|r| call.args.iter().any(|a| matches!(&a.value, ArgValue::Var(v) if v == r)));
    for r in reads {
        if Some(r.as_str()) == recv && !recv_is_arg {
            continue;
        }
        if !out.contains(r) {
            out.push(r.clone());
        }
    }
}

/// The bare method-receiver variable of a call (`$x` in `$x->m(...)`), or `None`
/// for a function/static/constructor/dynamic call or a non-variable receiver.
fn call_method_receiver_var(call: &CallExpr) -> Option<&str> {
    match &call.receiver {
        Callee::Method { receiver: Receiver::Var(v), .. } => Some(v),
        _ => None,
    }
}
