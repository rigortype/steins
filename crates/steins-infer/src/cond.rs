//! Condition evaluation → `Certainty` (ADR-0031 stage 1): [`eval_cond`] over the
//! lowered condition tree, the engine version-id comparisons, binary / ternary /
//! comparison / `instanceof` facts, and the operand environment threading.

use std::collections::HashMap;

use steins_contract::ContractTy;
use steins_domain::{Base, Certainty, Fact, Val};
use steins_syntax::{ArgValue, CmpOp, CondExpr, CondOperand, NameRef, RefKind, Span, ValueOp};

use crate::fold::Folder;
use crate::asserts::cond_invalidations;
use crate::compare::{php_identical, php_loose_eq, php_truthy};
use crate::contract::IsA;
use crate::cx::Cx;
use crate::env::{Known, Member, Store, Stratum, arg_of_val, singleton_fact};
use crate::existence::eval_existence_call;
use crate::predicates::apply_type_narrowing;
use crate::refine::{apply_refinements, collect_refine};
use crate::transfers::transfer_arg_fact;
use crate::walk::{WalkCx, mark_dead_cond_calls, mark_dead_span, value_stratum};

// ---------------------------------------------------------------------------
// Condition evaluation → `Certainty` (ADR-0031 stage 1).
// ---------------------------------------------------------------------------

/// Evaluate a lowered [`CondExpr`] against the env to a unified [`Certainty`].
///
/// `folder` is threaded because a foldable existence-guard call
/// (`method_exists`/`function_exists`/`class_exists` …, ADR-0049 §4 / N3) folds to
/// a real verdict by asking the runtime boot surface (the A2ii homonym oracle);
/// every other arm is env-only and ignores it.
/// Whether `r` names the ENGINE's `PHP_VERSION_ID` in the current file (issue
/// #29). Constants are case-sensitive, so the match is exact. A fully-qualified
/// `\PHP_VERSION_ID` always does; an unqualified reference does unless the file
/// `use const`-imports an alias; qualified/relative spellings never do. A userland
/// `const`/`define` twin elsewhere in the project zeroes [`Cx::version_id`] before
/// this is consulted; a define with a *computed* name is the one modeled-out
/// corner (no known occurrence in the wild).
fn is_engine_version_id(cx: &Cx, r: &NameRef) -> bool {
    if r.raw != "PHP_VERSION_ID" {
        return false;
    }
    match r.kind {
        RefKind::FullyQualified => true,
        RefKind::Unqualified => !cx.tree().php_version_id_aliased(),
        _ => false,
    }
}

/// The issue-#29 version-guard fold: `Some(verdict)` when one operand is the
/// engine `PHP_VERSION_ID` and the other an int literal — including the
/// `Maybe` verdict for an interval the literal splits (a target range that
/// straddles the comparison keeps both arms live). `None` hands the comparison
/// to the ordinary evaluation.
fn eval_version_id_cmp(
    cx: &Cx,
    op: CmpOp,
    lhs: &CondOperand,
    rhs: &CondOperand,
) -> Option<Certainty> {
    let (lo, hi) = cx.version_id?;
    let (r, lit, flipped) = match (lhs, rhs) {
        (CondOperand::Const(r), CondOperand::Literal(ArgValue::Int(n))) => (r, *n, false),
        (CondOperand::Literal(ArgValue::Int(n)), CondOperand::Const(r)) => (r, *n, true),
        _ => return None,
    };
    if !is_engine_version_id(cx, r) {
        return None;
    }
    // Mirror the operator when the constant sits on the right (`80400 <=
    // PHP_VERSION_ID` asks `PHP_VERSION_ID >= 80400`).
    let op = if flipped {
        match op {
            CmpOp::Lt => CmpOp::Gt,
            CmpOp::Le => CmpOp::Ge,
            CmpOp::Gt => CmpOp::Lt,
            CmpOp::Ge => CmpOp::Le,
            other => other,
        }
    } else {
        op
    };
    let lo = i64::from(lo);
    let hi = hi.map(i64::from);
    // Interval-vs-point trichotomy. `hi = None` is an open upper bound: the
    // interval can then never sit entirely below a literal.
    let all_ge = |p: i64| lo >= p;
    let all_le = |p: i64| hi.is_some_and(|h| h <= p);
    let verdict = match op {
        CmpOp::Ge => {
            if all_ge(lit) {
                Certainty::Yes
            } else if all_le(lit - 1) {
                Certainty::No
            } else {
                Certainty::Maybe
            }
        }
        CmpOp::Gt => {
            if all_ge(lit + 1) {
                Certainty::Yes
            } else if all_le(lit) {
                Certainty::No
            } else {
                Certainty::Maybe
            }
        }
        CmpOp::Le => {
            if all_le(lit) {
                Certainty::Yes
            } else if all_ge(lit + 1) {
                Certainty::No
            } else {
                Certainty::Maybe
            }
        }
        CmpOp::Lt => {
            if all_le(lit - 1) {
                Certainty::Yes
            } else if all_ge(lit) {
                Certainty::No
            } else {
                Certainty::Maybe
            }
        }
        // `==`/`===` (int against int: the loose and strict tables agree): `Yes`
        // only when the interval IS the point — unreachable at minor precision
        // (the interval always spans 100 patch ids) but written generally; a
        // point outside the interval is a definite `No`.
        CmpOp::Identical | CmpOp::Loose => {
            if lit < lo || hi.is_some_and(|h| lit > h) {
                Certainty::No
            } else if hi == Some(lo) && lit == lo {
                Certainty::Yes
            } else {
                Certainty::Maybe
            }
        }
        CmpOp::NotIdentical | CmpOp::NotLoose => {
            if lit < lo || hi.is_some_and(|h| lit > h) {
                Certainty::Yes
            } else if hi == Some(lo) && lit == lo {
                Certainty::No
            } else {
                Certainty::Maybe
            }
        }
    };
    Some(verdict)
}

pub(crate) fn eval_cond(
    w: &WalkCx,
    folder: &mut dyn Folder,
    cond: &CondExpr,
    env: &HashMap<String, Known>,
    store: &Store,
    poisoned: bool,
) -> Certainty {
    match cond {
        CondExpr::Cmp { op, lhs, rhs } => {
            // The PHP_VERSION_ID guard fold (issue #29): a comparison of the
            // engine's version id against an int literal decides against the
            // resolved target interval, and the ordinary decided-branch pruning
            // does the rest — a straddled boundary stays `Maybe` (both arms
            // live), never a guess.
            if let Some(v) = eval_version_id_cmp(w.cx, *op, lhs, rhs) {
                return v;
            }
            match (operand_values(lhs, env, poisoned), operand_values(rhs, env, poisoned)) {
                (Some(lv), Some(rv)) => eval_cmp(*op, &lv, &rv, w.cx.php_minor),
                _ => Certainty::Maybe,
            }
        }
        CondExpr::Truthy(op) => match operand_values(op, env, poisoned) {
            Some(vs) => all_agree(vs.iter().map(php_truthy)),
            None => Certainty::Maybe,
        },
        CondExpr::Instanceof { operand, class_ref } => {
            eval_instanceof(w, operand, class_ref, env, store, poisoned)
        }
        CondExpr::Not(c) => eval_cond(w, folder, c, env, store, poisoned).not(),
        // Short-circuit env threading (ADR-0052 §6 / N3): the RIGHT operand
        // evaluates under the env the LEFT operand's outcome establishes, as PHP's
        // `&&`/`||` sequence it — `b` in `a && b` sees `then_refinements(a)`; `b`
        // in `a || b` sees `else_refinements(a)` (De Morgan). Only the operand env
        // threads; the composed verdict stays the trinary `and`/`or`.
        CondExpr::And(a, b) => {
            let va = eval_cond(w, folder, a, env, store, poisoned);
            // `a` false => `b` never runs. `b`'s span is unevaluated code, not
            // merely unnarrowed, so a finding there would be a false positive —
            // record it dead, as a decided `if` records its skipped branch.
            if va == Certainty::No {
                mark_dead_cond_calls(w, b);
                return Certainty::No;
            }
            let (benv, bstore) =
                threaded_operand_env(w.cx, a, true, env, store, w.cx.php_minor, poisoned);
            va.and(eval_cond(w, folder, b, &benv, &bstore, poisoned))
        }
        CondExpr::Or(a, b) => {
            let va = eval_cond(w, folder, a, env, store, poisoned);
            // `a` true => `b` never runs. Same unevaluated-span reasoning, De Morgan-mirrored.
            if va == Certainty::Yes {
                mark_dead_cond_calls(w, b);
                return Certainty::Yes;
            }
            let (benv, bstore) =
                threaded_operand_env(w.cx, a, false, env, store, w.cx.php_minor, poisoned);
            va.or(eval_cond(w, folder, b, &benv, &bstore, poisoned))
        }
        // A foldable existence predicate in guard position folds to a Yes/No/Maybe
        // verdict against the closed world (ADR-0049 §4 / N3); an opaque condition or
        // any other guard call stays undecided.
        CondExpr::Call { call, .. } => eval_existence_call(w, folder, call),
        // `isset($x[k])` decides NOTHING (ADR-0062 S4). The only evidence that
        // could decide it is a shape fact, which is `Asserted` — deciding here
        // would let a docblock claim silence the env-free pass on a live path.
        // Narrowing is the whole payoff; reachability stays proof-only.
        CondExpr::Isset { .. } | CondExpr::IssetVar { .. } | CondExpr::Opaque { .. } => {
            Certainty::Maybe
        }
    }
}

/// A clone of `(env, store)` with the refinements `operand` establishes on the
/// given branch polarity applied (ADR-0052 §6 short-circuit threading). Used only
/// to evaluate the *right* operand of an `&&`/`||` at the precision the left
/// operand's runtime outcome guarantees. Native-condition refinements are
/// `Verified` (the runtime executed the test); the clone is discarded after the
/// verdict, so nothing leaks into the caller's env (ADR-0048 §2 walk-locality).
fn threaded_operand_env(
    cx: &Cx,
    operand: &CondExpr,
    then: bool,
    env: &HashMap<String, Known>,
    store: &Store,
    php_minor: Option<(u16, u16)>,
    poisoned: bool,
) -> (HashMap<String, Known>, Store) {
    let mut benv = env.clone();
    let mut bstore = store.clone();
    let mut refs = Vec::new();
    // The DR2 type vocabulary threads with the rest, running before the scalar
    // refinements so a minted fact is what they refine: `is_string($s) &&
    // strlen($s)` narrows `$s` in that order (ADR-0052 §6).
    apply_type_narrowing(cx, operand, then, &mut benv, &mut bstore);
    collect_refine(operand, then, &mut refs, php_minor);
    apply_refinements(&refs, &mut benv, &mut bstore, Stratum::Verified);
    // The operand's own side effects land after its test narrowed (a by-ref call
    // may rebind a variable the test just constrained): forget them.
    for v in cond_invalidations(cx, operand, env, store, poisoned) {
        benv.remove(&v);
        bstore.unbind(&v);
    }
    (benv, bstore)
}

/// The concrete value a **declared** contract arm denotes, or `None` when the arm
/// is not a single value (`int`, `int<1, 5>`, `non-empty-string`, a class, …).
///
/// The inverse of `literal_contract`, and deliberately as narrow: `array{}` is
/// included because a sealed, field-less, non-`non-empty` shape denotes exactly one
/// array — the empty one — which is what makes `$x == $emptyArr` decidable.
fn contract_literal_value(ty: &ContractTy) -> Option<ArgValue> {
    Some(match ty {
        ContractTy::LitInt(i) => ArgValue::Int(*i),
        ContractTy::LitFloat(f) => ArgValue::Float(*f),
        ContractTy::LitStr(s) => ArgValue::Str(s.clone()),
        ContractTy::LitBool(b) => ArgValue::Bool(*b),
        ContractTy::Null => ArgValue::Null,
        ContractTy::Shape { fields, sealed: true, non_empty: false, unsealed: None, .. }
            if fields.is_empty() =>
        {
            ArgValue::Array(Vec::new())
        }
        _ => return None,
    })
}

/// The candidate values of a value-position comparison operand, over **both**
/// value lanes (issue #260).
///
/// The proven lane first (`cmp_candidates_under`: a fact's finite members, a
/// literal, a fold). Then the declared arm lane (ADR-0052 §1), where a `@param 1
/// $one` lives — a parameter declared as a literal type carries no *fact*, only
/// an arm. Read at the same `Asserted` stratum the dump surface uses for
/// `dumpType($one)`, so the comparison inherits the declaration's trust without
/// laundering it: `resolve_literal` (the proof-layer seam) still refuses to see it.
///
/// `None` = no candidates ⇒ the caller's verdict is `Maybe`.
fn cmp_operand_candidates(
    cx: &Cx<'_>,
    folder: &mut dyn Folder,
    value: &ArgValue,
    env: &HashMap<String, Known>,
    store: Option<&Store>,
    poisoned: bool,
) -> Option<(Vec<ArgValue>, Stratum)> {
    if let Some(vals) = cx.cmp_candidates_under(value, env, poisoned, folder, None, None) {
        return Some((vals, value_stratum(value, env, store)));
    }
    if poisoned {
        return None;
    }
    let ArgValue::Var(name) = value else { return None };
    let arms = store?.contract_arms(name)?;
    if arms.is_empty() {
        return None;
    }
    let mut vals = Vec::with_capacity(arms.len());
    for arm in arms {
        vals.push(contract_literal_value(&arm.ty)?);
    }
    let strat = arms.iter().fold(Stratum::Verified, |acc, a| acc.min(a.stratum));
    Some((vals, strat))
}

/// Evaluate a **value-position binary operator** (issue #260) to an env [`Fact`].
///
/// For a comparison this is total, and that's the point: a PHP comparison
/// operator evaluates to a `bool` whatever its operands are, so the honest floor
/// for an undecided one is `bool`, not silence. `eval_cmp`'s three verdicts map
/// straight onto three renderings: `Yes -> true`, `No -> false`, `Maybe -> bool`.
/// The decision procedure is the condition path's, unchanged — only *where* it's
/// asked from is new.
///
/// # Stratum: the undecided arm is Verified, the decided arms are derived
///
/// Owner ruling (2026-08-09), departing from a flat reading of the derivation
/// clause (ADR-0052 §5): the three verdicts make different kinds of claims.
/// `Maybe -> bool` is a claim about the operator, not either operand — PHP's
/// guarantee, owed to nobody's docblock — so no operand refinement survives into
/// it: Verified, always (as ADR-0061 §3 floors its envelope). `Yes -> true` /
/// `No -> false` are claims about *which* bool, resting on the operands, so they
/// keep the operands' `min` stratum — a lying `@param 1 $one` can never launder
/// into the proof lane.
///
/// The recall this buys: a Verified `bool` may premise a proof-layer finding, so
/// `$x = ($a === $b); f($x);` against `function f(int $i)` is now a definite No
/// instead of silence.
#[allow(clippy::too_many_arguments)]
pub(crate) fn eval_binary_fact(
    cx: &Cx<'_>,
    folder: &mut dyn Folder,
    op: ValueOp,
    lhs: &ArgValue,
    rhs: &ArgValue,
    env: &HashMap<String, Known>,
    store: Option<&Store>,
    poisoned: bool,
) -> (Fact, Stratum) {
    let ValueOp::Cmp(cop) = op;
    let l = cmp_operand_candidates(cx, folder, lhs, env, store, poisoned);
    let r = cmp_operand_candidates(cx, folder, rhs, env, store, poisoned);
    // Derivation clause (ADR-0052 §5): a *decided* verdict consumes both operands'
    // facts, so it carries their `min` stratum. An operand with no candidates
    // leaves the verdict undecided — never wrong — and contributes the stratum its
    // own value lane would.
    let derived = l
        .as_ref()
        .map_or_else(|| value_stratum(lhs, env, store), |(_, s)| *s)
        .min(r.as_ref().map_or_else(|| value_stratum(rhs, env, store), |(_, s)| *s));
    let verdict = match (l, r) {
        (Some((l, _)), Some((r, _))) => eval_cmp(cop, &l, &r, cx.php_minor),
        _ => Certainty::Maybe,
    };
    match verdict {
        Certainty::Yes => (Fact::Singleton(Val::Bool(true)), derived),
        Certainty::No => (Fact::Singleton(Val::Bool(false)), derived),
        // The operator's own guarantee, premised on neither operand: Verified,
        // whatever the operands' lanes were (see this function's doc comment).
        Certainty::Maybe => {
            (Fact::General { base: Base::Bool, nullable: false }, Stratum::Verified)
        }
    }
}

/// Is a `??` left operand proven **set and non-null**, so PHP's own evaluation
/// order proves the right operand unevaluated (ADR-0052 §6)?
///
/// Three refusals keep this on the FP-safe side: an offset read never answers
/// yes (presence is the very question `??` asks of it, and
/// [`ArgValue::OffsetRead`] is a silence carrier by construction, ADR-0049 A7);
/// an operand that doesn't resolve to a concrete value answers no; and an
/// operand whose fact is `Asserted` answers no — reachability stays proof-only,
/// the same line [`eval_cond`] draws at `Isset`.
pub(crate) fn coalesce_lhs_proven_present(
    w: &WalkCx,
    folder: &mut dyn Folder,
    lhs: &ArgValue,
    env: &HashMap<String, Known>,
    store: &Store,
) -> bool {
    if matches!(lhs, ArgValue::OffsetRead { .. }) {
        return false;
    }
    if value_stratum(lhs, env, Some(store)) != Stratum::Verified {
        return false;
    }
    match w.cx.resolve_literal(lhs, env, w.scope.poisoned, folder) {
        Some(v) => !matches!(v, ArgValue::Null),
        None => false,
    }
}

/// Evaluate a ternary rvalue to an env [`Fact`] (ADR-0031): a decided guard picks
/// the chosen arm's proven value; an undecided guard yields a `OneOf` of the two
/// arms when both resolve to literals, else `None` (unknown → the var is dropped).
///
/// A decided guard also proves the **untaken arm unevaluated** (ADR-0052 §6): PHP
/// evaluates exactly one arm of a ternary, so `$x === 2 ? f("bad") : 0` with `$x`
/// proven `1` never runs `f` at all, and a finding on it would be a false positive.
/// `arms` carries the two source extents for exactly that record.
#[allow(clippy::too_many_arguments)]
pub(crate) fn eval_ternary_fact(
    w: &WalkCx,
    folder: &mut dyn Folder,
    cond: &CondExpr,
    then_val: &ArgValue,
    else_val: &ArgValue,
    arms: (Span, Span),
    env: &HashMap<String, Known>,
    store: &Store,
) -> Option<Fact> {
    let poisoned = w.scope.poisoned;
    let verdict = eval_cond(w, folder, cond, env, store, poisoned);
    match verdict {
        Certainty::Yes => mark_dead_span(w, arms.1),
        Certainty::No => mark_dead_span(w, arms.0),
        Certainty::Maybe => {}
    }
    // The arms evaluate under the guard's respective refinements (ADR-0052 §6):
    // `$c ? A : B` — `A` sees `then_refinements($c)`, `B` sees `else_refinements`.
    // Only the arm envs thread; the verdict logic is unchanged.
    let (tenv, _) = threaded_operand_env(w.cx, cond, true, env, store, w.cx.php_minor, poisoned);
    let (eenv, _) = threaded_operand_env(w.cx, cond, false, env, store, w.cx.php_minor, poisoned);
    match verdict {
        Certainty::Yes => {
            w.cx
                .resolve_literal(then_val, &tenv, poisoned, folder)
                .and_then(|a| singleton_fact(&a, w.cx.php_minor))
        }
        Certainty::No => {
            w.cx
                .resolve_literal(else_val, &eenv, poisoned, folder)
                .and_then(|a| singleton_fact(&a, w.cx.php_minor))
        }
        Certainty::Maybe => {
            // Undecided guard: the value is one of the two arms, so the fact is
            // their join. Both arms proven is the finite case, unchanged —
            // `Fact::from_vals` gives a `Singleton` when equal, `OneOf` otherwise.
            //
            // An arm that proves no value isn't the end of it (issue #339): `$c ?
            // $i : $s` used to drop whole since `val_of` needs a `Val` per arm and
            // `int|string` had no form to live in. Now each arm falls back to
            // whatever fact it carries and the two join. An arm with no fact at
            // all still drops the binding.
            let arm = |value: &ArgValue, aenv: &HashMap<String, Known>, folder: &mut dyn Folder| {
                w.cx
                    .resolve_literal(value, aenv, poisoned, folder)
                    .and_then(|lit| singleton_fact(&lit, w.cx.php_minor))
                    .or_else(|| transfer_arg_fact(w.cx, folder, value, aenv, Some(store)))
            };
            let t = arm(then_val, &tenv, folder)?;
            let e = arm(else_val, &eenv, folder)?;
            t.join(&e)
        }
    }
}

/// The candidate values of a condition operand: the fact's value set for a known
/// variable, the literal itself, else `None` (unknown → the caller yields `Maybe`).
pub(crate) fn operand_values(
    op: &CondOperand,
    env: &HashMap<String, Known>,
    poisoned: bool,
) -> Option<Vec<ArgValue>> {
    match op {
        CondOperand::Literal(v) => Some(vec![v.clone()]),
        // Only the finite layers (`Singleton`/`OneOf`) offer concrete candidate
        // values for a comparison; an abstract fact has none → `None` → `Maybe`
        // (the sound side). Condition evaluation over `finite_members()`.
        CondOperand::Var(name) if !poisoned => {
            env.get(name).and_then(|k| k.fact.as_ref()?.finite_members().map(|vs| vs.iter().map(arg_of_val).collect()))
        }
        _ => None,
    }
}

/// Evaluate a comparison over two candidate value sets (ADR-0031 OneOf rule: all
/// member pairs agree → that verdict; any disagreement or undecidable pair → Maybe).
pub(crate) fn eval_cmp(op: CmpOp, lhs: &[ArgValue], rhs: &[ArgValue], php_minor: Option<(u16, u16)>) -> Certainty {
    let mut acc: Option<bool> = None;
    for l in lhs {
        for r in rhs {
            let b = match op {
                CmpOp::Identical => php_identical(l, r, php_minor),
                CmpOp::NotIdentical => php_identical(l, r, php_minor).map(|x| !x),
                CmpOp::Loose => php_loose_eq(l, r, php_minor),
                CmpOp::NotLoose => php_loose_eq(l, r, php_minor).map(|x| !x),
                // Ordering: decide only for concrete numeric operands (PHP numeric
                // ordering); any other pairing is undecidable here → `Maybe`. The
                // refinement machinery consumes these guards regardless of verdict.
                CmpOp::Lt => php_num_order(l, r).map(|o| o == std::cmp::Ordering::Less),
                CmpOp::Le => php_num_order(l, r).map(|o| o != std::cmp::Ordering::Greater),
                CmpOp::Gt => php_num_order(l, r).map(|o| o == std::cmp::Ordering::Greater),
                CmpOp::Ge => php_num_order(l, r).map(|o| o != std::cmp::Ordering::Less),
            };
            match b {
                None => return Certainty::Maybe,
                Some(v) => match acc {
                    None => acc = Some(v),
                    Some(prev) if prev != v => return Certainty::Maybe,
                    _ => {}
                },
            }
        }
    }
    Certainty::from_opt(acc)
}

/// PHP numeric ordering of two concrete operands, decided only when **both** are
/// `int`/`float` (comparing as f64); any other pairing (strings, bools, null,
/// arrays) is `None` — undecidable here, so the guard verdict is `Maybe` (sound).
fn php_num_order(a: &ArgValue, b: &ArgValue) -> Option<std::cmp::Ordering> {
    let num = |v: &ArgValue| match v {
        #[allow(clippy::cast_precision_loss)]
        ArgValue::Int(i) => Some(*i as f64),
        ArgValue::Float(f) => Some(*f),
        _ => None,
    };
    let (x, y) = (num(a)?, num(b)?);
    x.partial_cmp(&y)
}

/// Fold a sequence of per-member truth verdicts (`None` = undecidable) into one
/// [`Certainty`]: all-agree → that pole, else `Maybe`.
fn all_agree(iter: impl Iterator<Item = Option<bool>>) -> Certainty {
    let mut acc: Option<bool> = None;
    for b in iter {
        match b {
            None => return Certainty::Maybe,
            Some(v) => match acc {
                None => acc = Some(v),
                Some(prev) if prev != v => return Certainty::Maybe,
                _ => {}
            },
        }
    }
    Certainty::from_opt(acc)
}

/// `operand instanceof Class`: `Yes` only when the operand's proven exact class
/// is-a the target through the project chain; a non-object literal is `No`;
/// everything else (unknown class, chain leaving the project) is `Maybe`.
fn eval_instanceof(
    w: &WalkCx,
    operand: &CondOperand,
    class_ref: &NameRef,
    env: &HashMap<String, Known>,
    store: &Store,
    poisoned: bool,
) -> Certainty {
    match operand {
        CondOperand::Var(name) if !poisoned => {
            let target = w.cx.class_fqn(class_ref);
            match store.class_of(name) {
                Some(obj_fqn) => {
                    // The trinary is-a oracle (ADR-0043): a proven supertype path is a
                    // definite `Yes`; a completely-enumerated hierarchy excluding the
                    // target is `No` (branch dead); an incomplete hierarchy stays
                    // `Maybe`. Instanceof still binds no exactness fact.
                    match w.cx.is_a(obj_fqn, &target) {
                        // Yes-side holds for a lower bound too (every descendant of the
                        // proven class is still a `T`).
                        IsA::Yes => Certainty::Yes,
                        // No-side needs exactness (audit G1): with a lower-bound
                        // `$this`, the runtime object may be a descendant that IS a
                        // `T`, so `No` here is not decisive.
                        IsA::No if store.is_exact(name) => Certainty::No,
                        // A lower-bound class decides nothing, so it must not
                        // *shadow* the lane that can: a prior `instanceof` bound a
                        // `Member` whose implication is a live-branch `Verified`
                        // fact, strictly stronger than the declaration underneath it.
                        // Only reachable since a declared parameter is a heap object
                        // (issue #388) — before it, a variable with a `Member` had no
                        // object — and monotone either way: `Maybe` in, decided out.
                        IsA::No | IsA::Unknown => {
                            member_instanceof(w.cx, store.member_of(name), &target)
                        }
                    }
                }
                // No heap object. First the value side (survey FP class 14): if the
                // fact proves a non-object value on this path, `instanceof T` is
                // definitionally `false` for every `T`. Needs no class reasoning and
                // no exactness (`store.is_exact` is scoped to the heap path above).
                None if env.get(name).and_then(|k| k.fact.as_ref()).is_some_and(fact_is_non_object) => {
                    Certainty::No
                }
                // Otherwise a prior `instanceof` guard may have bound a `Member` fact
                // whose is-a implication decides this test (ADR-0052 §3b). A11 does
                // NOT thread here — scoped to the arm-deletion consumers.
                None => member_instanceof(w.cx, store.member_of(name), &target),
            }
        }
        // A concrete non-object literal (`null`, `5`, `"x"`, …) is never an
        // instance of a class.
        CondOperand::Literal(v) if v.is_literal() => Certainty::No,
        _ => Certainty::Maybe,
    }
}

/// The `instanceof T2` verdict implied by a variable's guard-derived [`Member`]
/// fact (ADR-0052 §3b), when no exact heap class is known: `Yes` when some
/// proven `T1 ∈ yes` has `is_a(T1, T2) = Yes`; `No` when some excluded
/// `T1' ∈ no` has `is_a(T2, T1') = Yes`; `Maybe` otherwise. Monotone: only turns
/// `Maybe` into a decided verdict, never emits.
pub(crate) fn member_instanceof(cx: &Cx, member: Option<&Member>, target: &str) -> Certainty {
    let Some(m) = member else { return Certainty::Maybe };
    if m.yes.iter().any(|t1| cx.is_a(t1, target) == IsA::Yes) {
        return Certainty::Yes;
    }
    if m.no.iter().any(|excluded| cx.is_a(target, excluded) == IsA::Yes) {
        return Certainty::No;
    }
    Certainty::Maybe
}

/// Whether a value-domain [`Fact`] proves the variable holds a non-object value
/// on this path (survey FP class 14). Every inhabitant must be a non-object PHP
/// value; then `instanceof T` is `false` for every `T`. All four fact layers
/// denote non-object values — objects live in the heap, never the value domain.
/// A `Singleton`/`OneOf` is checked inhabitant-wise so the rule stays correct if
/// the value domain ever gains an object inhabitant.
fn fact_is_non_object(f: &Fact) -> bool {
    match f {
        Fact::Singleton(v) => val_is_non_object(v),
        Fact::OneOf(vs) => vs.iter().all(val_is_non_object),
        // Every arm is a scalar base, and no scalar base is an object.
        Fact::Union { .. } => true,
        Fact::Refined { .. } | Fact::General { .. } => true,
        // A shape fact does denote arrays, which are non-objects — but the
        // value-side `instanceof` rule gains no proof from it, so
        // the arm answers "not proven" (no narrowing), the no-knowledge side.
        Fact::Shape { .. } => false,
    }
}

/// Whether a concrete [`Val`] is a non-object PHP value. Exhaustive by design:
/// no current `Val` variant denotes an object, and if one is ever added this
/// match forces a deliberate decision rather than silently answering `No` to an
/// `instanceof` on a value that could be an object.
fn val_is_non_object(v: &Val) -> bool {
    match v {
        Val::Int(_) | Val::Float(_) | Val::Str(_) | Val::Bool(_) | Val::Null | Val::Array(_) => true,
    }
}
