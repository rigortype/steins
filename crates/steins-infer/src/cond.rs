//! Condition evaluation → `Certainty` (ADR-0031 stage 1): [`eval_cond`] over the
//! lowered condition tree, the engine version-id comparisons, binary / ternary /
//! comparison / `instanceof` facts, and the operand environment threading.

use std::collections::HashMap;

use steins_contract::ContractTy;
use steins_domain::{Base, Certainty, Fact, IntRange, Refinement, ShapeFact, Val};
use steins_syntax::{
    ArgValue, CastTarget, CmpOp, CondExpr, CondOperand, IssetOperand, LogicalOp, NameRef, RefKind,
    Span, ValueOp,
};

use crate::fold::Folder;
use crate::asserts::cond_invalidations;
use crate::coerce::php_cast_fact;
use crate::compare::{php_identical, php_loose_eq, php_truthy};
use crate::contract::IsA;
use crate::cx::Cx;
use crate::env::{Known, Member, Store, Stratum, arg_of_val, singleton_fact};
use crate::existence::eval_existence_call;
use crate::offsets::{ShapeRead, offset_key_of, offset_operand_fact, shape_read_at};
use crate::predicates::apply_type_narrowing;
use crate::refine::{apply_refinements, collect_refine};
use crate::transfers::{transfer_arg_fact, transfer_arg_known};
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
            let lv = cmp_operand_values(w, folder, lhs, env, poisoned);
            let rv = cmp_operand_values(w, folder, rhs, env, poisoned);
            match (lv, rv) {
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
        CondExpr::Isset { .. }
        | CondExpr::IssetVar { .. }
        | CondExpr::InstanceofDyn { .. }
        | CondExpr::Opaque { .. } => Certainty::Maybe,
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

/// Evaluate a **value-position comparison** (issue #260) to an env [`Fact`].
///
/// This is total, and that's the point: a PHP comparison operator evaluates to a
/// `bool` whatever its operands are, so the honest floor for an undecided one is
/// `bool`, not silence. `eval_cmp`'s three verdicts map straight onto three
/// renderings: `Yes -> true`, `No -> false`, `Maybe -> bool`. The decision
/// procedure is the condition path's, unchanged — only *where* it's asked from is
/// new.
///
/// It takes a [`CmpOp`] rather than the whole [`ValueOp`] precisely to keep that
/// totality true of its type (issue #615): [`ValueOp::BitOr`] joined the enum for
/// a carrier the `filter_var` flags roster reads by constant NAME, and a bitwise
/// `|` has NO total floor — GMP overloads it to return an object, so even
/// `int|string` would be a lie. So the four callers match [`ValueOp::Cmp`] and a
/// `|` simply falls through to the lower rungs, which say nothing.
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
    cop: CmpOp,
    lhs: &ArgValue,
    rhs: &ArgValue,
    env: &HashMap<String, Known>,
    store: Option<&Store>,
    poisoned: bool,
) -> (Fact, Stratum) {
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

/// The truthiness of a value-position expression, with the stratum the reading
/// enters at (issue #625).
///
/// The one truthiness reader the logical family shares. It asks [`Fact::truthy`]
/// and nothing else — `php_cast_fact` already set that precedent for the `bool`
/// cast, "so `'some-string'` casts to `true` and `'0'` to `false` by the same
/// rule every guard uses", and a second falsiness table here would be a second
/// place for PHP's rules to be wrong.
///
/// The dispatch above the plain value lane is what makes the family
/// **compositional**: a comparison, an `isset`, another connective and a
/// negation each already answer a total `bool`, so `!isset($foo)` and `$a && ($b
/// || $c)` fold rather than bottoming out at `Maybe`. Every other shape goes to
/// [`transfer_arg_known`], the same argument-fact reader every transfer rule
/// uses, and an expression with no fact at all is honestly `Maybe`.
///
/// # What this deliberately does not answer: an object
///
/// A variable the heap proves holds an object carries no value-domain fact, so
/// it answers `Maybe` — and the tempting rung, "an object is truthy", is
/// **refuted at `PINNED_PHP` 8.5.9**: a `BcMath\Number` built from the string
/// `"0"` casts to `false`, and so does a childless `SimpleXMLElement`. Neither is
/// exotic — the first is the very class the bcmath corpus rows are written
/// against, and the corpus itself agrees, asserting `bool` (not `true`) for
/// `Number || Number`. So the rung would be unsound in exactly the place it was
/// proposed for, and `Maybe` — rendering the `bool` floor — is the honest answer.
fn value_truthiness(
    w: &WalkCx,
    folder: &mut dyn Folder,
    value: &ArgValue,
    env: &HashMap<String, Known>,
    store: Option<&Store>,
    poisoned: bool,
) -> (Certainty, Stratum) {
    match value_operand_fact(w, folder, value, env, store, poisoned) {
        Some((fact, strat)) => (fact.truthy(), strat),
        None => (Certainty::Maybe, value_stratum(value, env, store)),
    }
}

/// **The fact a value-position expression denotes**, or `None` when nothing at
/// all is known about it — the one reader the composed operator family shares
/// ([`value_truthiness`] and [`eval_cast_fact`] both go through it).
///
/// The dispatch above the plain value lane is what makes the family
/// **compositional**: a comparison, an `isset`, a connective, a negation and a
/// cast each already answer for themselves, so `!isset($foo)`, `$a && ($b || $c)`
/// and `(int) ($a === $b)` fold rather than bottoming out. Every other shape goes
/// to [`transfer_arg_known`], the same argument-fact reader every transfer rule
/// uses — which resolves a literal operand to its `Singleton` on the way down.
fn value_operand_fact(
    w: &WalkCx,
    folder: &mut dyn Folder,
    value: &ArgValue,
    env: &HashMap<String, Known>,
    store: Option<&Store>,
    poisoned: bool,
) -> Option<(Fact, Stratum)> {
    match value {
        ArgValue::Binary { op: ValueOp::Cmp(op), lhs, rhs } => {
            Some(eval_binary_fact(w.cx, folder, *op, lhs, rhs, env, store, poisoned))
        }
        ArgValue::Binary { op: ValueOp::Spaceship, lhs, rhs } => {
            Some(eval_spaceship_fact(w.cx, folder, lhs, rhs, env, store, poisoned))
        }
        ArgValue::Isset(ops) => Some(eval_isset_fact(w.cx, ops, env, poisoned)),
        ArgValue::Logical { op, lhs, rhs, rhs_span } => {
            Some(eval_logical_fact(w, folder, *op, lhs, rhs, *rhs_span, env, store, poisoned))
        }
        ArgValue::Not(inner) => Some(eval_not_fact(w, folder, inner, env, store, poisoned)),
        ArgValue::Cast { target, operand } => {
            Some(eval_cast_fact(w, folder, *target, operand, env, store, poisoned))
        }
        _ => transfer_arg_known(w.cx, folder, value, env, store),
    }
}

/// Render a decided-or-not truthiness verdict as the family's [`Fact`], with the
/// issue #260 stratum ruling applied verbatim (issue #625).
///
/// `Yes -> true` / `No -> false` say **which** bool and rest on the operands, so
/// they keep the operands' `min`; `Maybe -> bool` is a claim about the operator,
/// premised on no operand, so it is `Verified` always.
fn bool_verdict_fact(verdict: Certainty, derived: Stratum) -> (Fact, Stratum) {
    match verdict {
        Certainty::Yes => (Fact::Singleton(Val::Bool(true)), derived),
        Certainty::No => (Fact::Singleton(Val::Bool(false)), derived),
        Certainty::Maybe => {
            (Fact::General { base: Base::Bool, nullable: false }, Stratum::Verified)
        }
    }
}

/// Evaluate a **value-position logical connective** `&& || and or xor` (issue
/// #625) to an env [`Fact`].
///
/// Total, for [`eval_binary_fact`]'s reason one step stronger: PHP has no
/// operator overloading for these connectives, so `$a && $b` is a `bool` no
/// matter what `$a` and `$b` are — which is exactly why `bcmath-number.php`
/// asserts `bool` for `Number || Number` while it asserts `BcMath\Number` for
/// `Number + Number`. The honest floor for an undecided one is `bool`, never
/// silence.
///
/// The truthiness table is not written here. [`Certainty::and`] / [`Certainty::or`]
/// / [`Certainty::not`] are Kleene-strong and already implement every cell of it,
/// and [`value_truthiness`] answers each operand through [`Fact::truthy`]. `xor`
/// has no `Certainty` method of its own and is composed as `(a || b) && !(a &&
/// b)`, which reproduces Kleene's table exactly: `Yes xor Maybe` is
/// `and(or(Yes, Maybe), not(and(Yes, Maybe)))` = `and(Yes, Maybe)` = `Maybe`.
///
/// # Short-circuiting: the decided operand is the only one consulted
///
/// PHP does not evaluate the right operand of a `&&` whose left is falsy, nor of
/// a `||` whose left is truthy — the same rule ADR-0052 §6 already applies to a
/// ternary's untaken arm — so such an operand is recorded dead through
/// [`mark_dead_span`] and a finding inside it would be a false positive. It was
/// one until this slice: `$y = $x === 2 && f("bad");` reported inside a call PHP
/// never makes, while the `if ($x === 2 && f("bad"))` spelling of the same test
/// was already silent, because only the CONDITION lowering modelled `&&`.
///
/// The short-circuited operand contributes no stratum either, for the same
/// reason: an expression PHP never evaluates is not evidence for the verdict.
/// `xor` never short-circuits, so both of its operands always count.
#[allow(clippy::too_many_arguments)]
pub(crate) fn eval_logical_fact(
    w: &WalkCx,
    folder: &mut dyn Folder,
    op: LogicalOp,
    lhs: &ArgValue,
    rhs: &ArgValue,
    rhs_span: Span,
    env: &HashMap<String, Known>,
    store: Option<&Store>,
    poisoned: bool,
) -> (Fact, Stratum) {
    let (l, ls) = value_truthiness(w, folder, lhs, env, store, poisoned);
    // The short-circuit legs, decided by the left operand alone.
    match (op, l) {
        (LogicalOp::And, Certainty::No) => {
            mark_dead_span(w, rhs_span);
            return bool_verdict_fact(Certainty::No, ls);
        }
        (LogicalOp::Or, Certainty::Yes) => {
            mark_dead_span(w, rhs_span);
            return bool_verdict_fact(Certainty::Yes, ls);
        }
        _ => {}
    }
    let (r, rs) = value_truthiness(w, folder, rhs, env, store, poisoned);
    let verdict = match op {
        LogicalOp::And => l.and(r),
        LogicalOp::Or => l.or(r),
        LogicalOp::Xor => l.or(r).and(l.and(r).not()),
    };
    bool_verdict_fact(verdict, ls.min(rs))
}

/// Evaluate a **value-position `!<operand>`** (issue #625) to an env [`Fact`].
///
/// Total for [`eval_logical_fact`]'s reason — `!` is a `bool` whatever it negates
/// — and three-valued through [`Certainty::not`], so `Maybe` stays `Maybe` and
/// renders the `bool` floor. `!` evaluates its operand always, so nothing here
/// records deadness.
///
/// The row this makes decidable that no other slice could: `!isset($foo)`.
/// Issue #579 taught the value seam to answer the inner `isset` and the negation
/// around it still widened to `Other`, so the expression answered `unknown` while
/// its own subexpression answered `false`.
pub(crate) fn eval_not_fact(
    w: &WalkCx,
    folder: &mut dyn Folder,
    operand: &ArgValue,
    env: &HashMap<String, Known>,
    store: Option<&Store>,
    poisoned: bool,
) -> (Fact, Stratum) {
    let (c, strat) = value_truthiness(w, folder, operand, env, store, poisoned);
    bool_verdict_fact(c.not(), strat)
}

/// Evaluate a **value-position cast** `(int) $x` (issue #626) to an env [`Fact`].
///
/// The semantics are not written here. [`php_cast_fact`] is the `settype` cast
/// grid of issue #595, `php -r`-measured cell by cell at `PINNED_PHP`, and this
/// is the second syntax over the same grid — so `settype($v, 'int')` and `(int)
/// $v` answer identically by construction, which is the property
/// `tests/cast_value_position.rs` pins. A cell the grid declines is a decline
/// here too, and then the floor below answers.
///
/// # Every cast is total, `(string)` included
///
/// The issue this implements reads the grid's declined array cell as "`(string)`
/// has no floor" and asks the slice to rule. **It has one, and the ruling is that
/// it is the same total floor the other four have.** A cast that produces a value
/// at all produces a value of its target's base — the only alternative is a
/// thrown `Error`, which produces no value for anything downstream to be about.
/// Measured at `PINNED_PHP` 8.5.9, `php -r`:
///
/// * `(string)[1, 2]` is `"Array"` (an `E_WARNING`, not an error — a string);
/// * `(string)$resource` is `"Resource id #5"`; `(string)$objectWithToString` is
///   that method's return, which PHP already forces to be a string;
/// * `(string)new stdClass` **throws** `Error: Object of class stdClass could not
///   be converted to string`, and so does an enum case — no value, so no claim;
/// * `(int)new ArrayObject([1])` is `1` and `(bool)new stdClass` is `true`, both
///   with a warning at most, so the other bases never even throw.
///
/// **The grid's array-to-string decline is untouched by this.** That cell refuses
/// to state the *value* `'Array'` — "right and useless", and in `settype`
/// position it keeps the by-ref invalidation — and it still refuses it here: the
/// floor is a claim about the operator's base, minted by this function, never by
/// [`php_cast_fact`], which continues to answer `None` for an array input and
/// leaves `settype($v, 'string')` bit-for-bit as it was. The four rows the ruling
/// wins (`(string) $mixed` under a `!==` guard) get `string` and no more.
///
/// # Stratum: the floor is the operator's, the grid's answer is the operand's
///
/// The #260 ruling, applied unchanged. A result that IS the floor is a claim
/// about the cast operator, owed to no operand and no docblock — `Verified`,
/// always. A result the grid computed from the operand's fact rests on that
/// fact, so it carries the operand's own stratum: `(int) $rangedByParam` stays
/// `Asserted` and can never premise a proof-layer finding (ADR-0061 §3).
///
/// Nothing here is width-sensitive arithmetic (ADR-0028 §3): `(int)` of a float
/// truncates through the grid's own `is_finite`-gated reader, `(int)` of an int
/// is the identity, and `(string)` of an int prints exactly.
#[allow(clippy::too_many_arguments)]
pub(crate) fn eval_cast_fact(
    w: &WalkCx,
    folder: &mut dyn Folder,
    target: CastTarget,
    operand: &ArgValue,
    env: &HashMap<String, Known>,
    store: Option<&Store>,
    poisoned: bool,
) -> (Fact, Stratum) {
    let floor = cast_floor(target);
    let answered = value_operand_fact(w, folder, operand, env, store, poisoned)
        .and_then(|(fact, strat)| Some((php_cast_fact(&fact, target)?, strat)));
    match answered {
        // The grid landing exactly on the floor is the operator's own guarantee
        // reached the long way round, so it enters `Verified` like the short way.
        Some((fact, _)) if fact == floor => (floor, Stratum::Verified),
        Some(pair) => pair,
        None => (floor, Stratum::Verified),
    }
}

/// The base a cast guarantees whatever it converts — the floor
/// [`eval_cast_fact`] states when the grid has nothing to say, and the reason no
/// cast expression answers `unknown`.
///
/// [`CastTarget::Null`] has no cast syntax to reach it (`(null)1` is a parse
/// error at `PINNED_PHP`); the row is `settype($v, 'null')`'s, whose grid cell
/// never declines, so this arm is written for totality rather than for a caller.
fn cast_floor(target: CastTarget) -> Fact {
    let general = |base| Fact::General { base, nullable: false };
    match target {
        CastTarget::Int => general(Base::Int),
        CastTarget::Float => general(Base::Float),
        CastTarget::String => general(Base::String),
        CastTarget::Bool => general(Base::Bool),
        // The degenerate shape IS plain `array` (ADR-0062 §3) — there is no
        // array arm of `Base` to take a `General` over.
        CastTarget::Array => {
            Fact::Shape { shape: Box::new(ShapeFact::plain_array()), nullable: false }
        }
        CastTarget::Null => Fact::Singleton(Val::Null),
    }
}

/// The pole a `<=>` decides over two candidate sets, or `None` when it is
/// undecided (issue #625).
///
/// The ONE place the spaceship's decision lives, so the fact seam
/// ([`eval_spaceship_fact`]) and the literal seam ([`Cx::resolve_literal_under`])
/// can never disagree about which pairings decide — the discipline `cmp_op_of`
/// holds for the syntax-to-operator map, one layer down.
///
/// `-1`/`0`/`1` is exactly "less than / equal / greater than", so this is
/// [`eval_cmp`] asked twice and nothing else: it inherits the whole comparison
/// decision procedure and adds no ordering of its own. It never subtracts the
/// operands — that is arithmetic, and ADR-0028 §3's engine-int-width trap.
pub(crate) fn spaceship_pole(l: &[ArgValue], r: &[ArgValue], php_minor: Option<(u16, u16)>) -> Option<i64> {
    match (eval_cmp(CmpOp::Lt, l, r, php_minor), eval_cmp(CmpOp::Gt, l, r, php_minor)) {
        (Certainty::Yes, Certainty::No) => Some(-1),
        (Certainty::No, Certainty::Yes) => Some(1),
        // Provably neither less nor greater is provably equal. `Maybe` on either
        // side leaves the pole open and takes the floor.
        (Certainty::No, Certainty::No) => Some(0),
        _ => None,
    }
}

/// Evaluate a **value-position `<=>`** (issue #625) to an env [`Fact`].
///
/// Total, one layer up from [`eval_binary_fact`]'s `bool`: a spaceship's value is
/// `-1`, `0` or `1` for **every** operand pairing PHP admits. Measured at
/// `PINNED_PHP` 8.5.9 rather than recalled, with the exotic spellings among the
/// probes: `[1,2] <=> [1,3]` is `-1`, `[1,2] <=> [1,2,3]` is `-1`, two fresh
/// `stdClass` instances compare `0`, `[] <=> []` is `0`, `null <=> 0` is `0`.
///
/// So the undecided floor is the fixed three-point range `int<-1, 1>` — a
/// [`Fact::refined`] over [`IntRange`], not a `Fact::OneOf([-1, 0, 1])`, which
/// would render `-1|0|1` and claim a finite-member layer the operator does not
/// justify.
///
/// # The decided arm reuses the comparison procedure and adds no ordering of its own
///
/// `-1`/`0`/`1` is exactly "less than / equal / greater than", so the verdict is
/// [`eval_cmp`] asked twice over the candidate sets [`cmp_operand_candidates`]
/// already produces. That inherits every ordering rule the guard path has and
/// invents none — which also means it inherits the declines: `php_num_order`
/// decides only for concrete numeric operands, so `'foo' <=> 'bar'` stays at the
/// `int<-1, 1>` floor rather than folding to `1`. Widening string ordering to
/// make that one row match is a comparison-family change, not a spaceship one.
///
/// **It is never decided by subtracting the operands.** That is arithmetic, it
/// belongs to issue #260's sidecar operator arm, and ADR-0028 §3 names the
/// engine-int-width trap it walks into. The answer is pinned to `-1|0|1` without
/// it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn eval_spaceship_fact(
    cx: &Cx<'_>,
    folder: &mut dyn Folder,
    lhs: &ArgValue,
    rhs: &ArgValue,
    env: &HashMap<String, Known>,
    store: Option<&Store>,
    poisoned: bool,
) -> (Fact, Stratum) {
    let l = cmp_operand_candidates(cx, folder, lhs, env, store, poisoned);
    let r = cmp_operand_candidates(cx, folder, rhs, env, store, poisoned);
    let derived = l
        .as_ref()
        .map_or_else(|| value_stratum(lhs, env, store), |(_, s)| *s)
        .min(r.as_ref().map_or_else(|| value_stratum(rhs, env, store), |(_, s)| *s));
    let decided = match (l, r) {
        (Some((l, _)), Some((r, _))) => spaceship_pole(&l, &r, cx.php_minor),
        _ => None,
    };
    match decided {
        Some(n) => (Fact::Singleton(Val::Int(n)), derived),
        // The operator's own guarantee, premised on neither operand: Verified,
        // by the issue #260 ruling this family shares.
        None => (
            Fact::refined(
                Base::Int,
                Refinement::Int(IntRange::new(-1, 1).expect("lo <= hi")),
                false,
            ),
            Stratum::Verified,
        ),
    }
}

/// Evaluate a **value-position `isset(…)`** (issue #579) to an env [`Fact`].
///
/// Total, for the same reason [`eval_binary_fact`] is: `isset` evaluates to a
/// `bool` whatever it tests, so the honest floor for an undecided one is `bool`,
/// never silence. The operands are PHP's conjunction, folded through
/// [`Certainty::and`], and the three verdicts map onto the three renderings the
/// comparison node already uses: `Yes -> true`, `No -> false`, `Maybe -> bool`.
///
/// # The rule is issue #343's, one step stronger
///
/// `array_key_exists` asks whether the key is **present**; `isset` asks that AND
/// that the value is not `null`, which [`Fact::is_null`] answers as a
/// `Certainty`. So the presence verdict and the negated null verdict conjoin,
/// which is the whole of the offset leg:
///
/// | shape | `array_key_exists` | `isset` |
/// | --- | --- | --- |
/// | required field, non-null value | `true` | `true` |
/// | required field, `?int` value | `true` | `bool` |
/// | proven-absent field, or undeclared key under a sealed tail | `false` | `false` |
/// | optional field, or unsealed tail | `bool` | `bool` |
///
/// A required field whose value is provably `null` answers `false` — a row the
/// table does not spell because `array_key_exists` has no such row, and the same
/// conjunction produces it.
///
/// # Stratum: the undecided arm is Verified, the decided arms are derived
///
/// The issue #260 ruling, verbatim: `Maybe -> bool` is a claim about the
/// **construct**, premised on no operand, so it is Verified always; `Yes`/`No`
/// say *which* bool and rest on the subject's fact, so they carry its stratum —
/// which for a shape read out of a `@param array{…}` is `Asserted`, and ADR-0062
/// A-G9's corollary then keeps the verdict out of every proof-layer premise
/// exactly as it keeps every other shape-derived fact out. A decided verdict
/// takes the `min` over every operand rather than only the deciding one: less
/// trust is always the safe side of this ledger.
pub(crate) fn eval_isset_fact(
    cx: &Cx<'_>,
    ops: &[IssetOperand],
    env: &HashMap<String, Known>,
    poisoned: bool,
) -> (Fact, Stratum) {
    // `isset()` with no operand is not PHP, so the fold's neutral element is
    // never the answer on its own — but `Yes` is still the right seed for a
    // conjunction, and an empty list would answer `true` only for source no
    // parser accepts.
    let mut verdict = Certainty::Yes;
    let mut derived = Stratum::Verified;
    for op in ops {
        let (c, s) = isset_operand_verdict(cx, op, env, poisoned);
        verdict = verdict.and(c);
        derived = derived.min(s);
    }
    match verdict {
        Certainty::Yes => (Fact::Singleton(Val::Bool(true)), derived),
        Certainty::No => (Fact::Singleton(Val::Bool(false)), derived),
        Certainty::Maybe => {
            (Fact::General { base: Base::Bool, nullable: false }, Stratum::Verified)
        }
    }
}

/// One operand's contribution to [`eval_isset_fact`]'s conjunction.
///
/// # Why the bare-variable leg decides `false` freely and `true` only from the walk
///
/// `isset($x)` is false when `$x` holds `null` **and** when `$x` has no binding
/// at all, so a fact proving the value `null` decides `false` with no definedness
/// premise whatsoever — both readings of "how did that fact get here" agree.
///
/// `true` is the leg that needs the binding, and a declaration cannot supply it.
/// ADR-0087 §4 is the case in point: `@var \DateTime|unset $x` states that reads
/// of `$x` may find no binding, and `ContractTy::is_unset` is filtered out of the
/// arm list before it reaches the store — so a `T|unset` declaration and a plain
/// `T` one leave the value lane in the same state, and answering `true` from that
/// state would contradict the ADR's own reading of the guard. The stratum is the
/// available discriminator: an `Asserted` fact came from an author's claim about
/// the value, a `Verified` one from the walk's own record of a binding form. So
/// the `true` leg reads only `Verified`, and the definedness questions a
/// declaration raises are left where ADR-0087 left them.
///
/// Deliberately **deferred**, and each answers the `bool` floor: a never-bound
/// variable (`isset($nope)` is `false` in PHP, but the lowering's own definedness
/// lanes exclude an `isset` operand from the read sets by construction — that is
/// what makes the guard silent — so the seam has no witness to read); a property
/// or static-property operand, whose binding question is a declared-but-
/// uninitialized one the heap does not answer; a path deeper than one offset; and
/// a variable holding an OBJECT, whose binding lives in the heap store's
/// reference table rather than as a `Fact` here — and that table does not on its
/// own separate a proven allocation from a declared, possibly nullable, receiver.
fn isset_operand_verdict(
    cx: &Cx<'_>,
    op: &IssetOperand,
    env: &HashMap<String, Known>,
    poisoned: bool,
) -> (Certainty, Stratum) {
    /// The undecided contribution: the construct's own `bool`, premised on nothing.
    const UNDECIDED: (Certainty, Stratum) = (Certainty::Maybe, Stratum::Verified);
    match op {
        IssetOperand::Unmodelled => UNDECIDED,
        // The subject's shape already carries the presence answer (issue #343);
        // `isset` conjoins the non-null one. `shape_read_at` is the offset
        // family's own resolver, so this verdict and an `$var[key]` read can
        // never disagree about which field they mean — and it declines on a
        // poisoned scope, a nullable base and an unproven key for us.
        IssetOperand::Offset { var, key } => {
            // A **proven whole** array answers exactly, and it has to be tried
            // first: a fully-literal `['k' => 1]` binds a `Fact::Singleton` of the
            // value itself, not a `Fact::Shape`, so the abstract rung below never
            // sees it. The verdict is not an approximation here — the entries ARE
            // the array, so an absent key is absent and a present one's value is
            // known. This is the leg that makes the table hold over a *witnessed*
            // literal and not only over a declared shape.
            if let Some(decided) = proven_array_isset(cx, var, key, env, poisoned) {
                return decided;
            }
            let base = ArgValue::Var(var.clone());
            let Some((read, stratum)) = shape_read_at(&base, key, env, poisoned, cx.php_minor)
            else {
                return UNDECIDED;
            };
            let verdict = match read {
                ShapeRead::Present(Some(slot)) => slot.is_null().not(),
                // A required field with no value slot is present but says nothing
                // about `null` (A-G1a's honest floor), so the conjunction is Maybe.
                ShapeRead::Present(None) => Certainty::Maybe,
                ShapeRead::DeclaredAbsent => Certainty::No,
                // An optional field and an unsealed tail are genuinely undecided
                // on presence alone, so the null question never gets asked.
                ShapeRead::MaybeMissing(_) | ShapeRead::Tail(_) => Certainty::Maybe,
            };
            match verdict {
                Certainty::Maybe => UNDECIDED,
                decided => (decided, stratum),
            }
        }
        IssetOperand::Var(name) => {
            if poisoned {
                return UNDECIDED;
            }
            let Some(known) = env.get(name) else { return UNDECIDED };
            let Some(fact) = &known.fact else { return UNDECIDED };
            match fact.is_null() {
                Certainty::Yes => (Certainty::No, known.stratum),
                Certainty::No if known.stratum == Stratum::Verified => {
                    (Certainty::Yes, known.stratum)
                }
                _ => UNDECIDED,
            }
        }
    }
}

/// `isset($var[key])` where `$var` holds a **proven whole array** — the witnessed
/// half of the table (issue #579). `None` where the base is not one, or the key is
/// not a proven single value, leaving the abstract shape rung to answer.
///
/// Exact, not conservative: a `Fact::Singleton(Val::Array(..))` says the array IS
/// those entries, so a key not among them is absent and a key among them has that
/// value. Both halves of `isset` are then decided outright, which is why this arm
/// never returns `Maybe`.
///
/// The key travels through the offset family's own resolution and PHP's own key
/// cast, so `$a[5]` and `$a["5"]` are one key here as everywhere else.
fn proven_array_isset(
    cx: &Cx<'_>,
    var: &str,
    key: &ArgValue,
    env: &HashMap<String, Known>,
    poisoned: bool,
) -> Option<(Certainty, Stratum)> {
    if poisoned {
        return None;
    }
    let Some(Fact::Singleton(key_val)) = offset_operand_fact(key, env, poisoned, cx.php_minor)
    else {
        return None;
    };
    let canon = offset_key_of(&key_val)?;
    let known = env.get(var)?;
    let Some(Fact::Singleton(Val::Array(entries))) = &known.fact else { return None };
    let verdict = match entries.iter().find(|(k, _)| *k == canon) {
        Some((_, v)) => Certainty::from_bool(*v != Val::Null),
        None => Certainty::No,
    };
    Some((verdict, known.stratum))
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
/// A comparison operand's candidate values in GUARD position, agreeing with what
/// the same expression answers in VALUE position (issue #342).
///
/// [`operand_values`] is the pure half: a literal, or a variable whose fact has
/// finite members. It answers `None` for every [`CondOperand::Other`], whatever
/// produced it — a fold, a transfer rung, a constant function — so
/// `strtoupper('a') === 'A'` decided in a `dumpType` and stayed `Maybe` one
/// character away inside an `if`. `cmp_candidates_under` is documented as "the
/// value-side twin of `operand_values`", and the two disagreed on exactly this
/// operand.
///
/// The call is rebuilt as the [`ArgValue::Call`] the value seam reads. Only a
/// **plain, positional, statically-named function call** converts: a method or
/// static call carries a receiver this variant cannot spell, a named or spread
/// argument is not a positional list, and each of those keeps today's `Maybe`.
///
/// `descent` and `out` are `None` deliberately. Resolution here answers a guard,
/// and a guard is not a place to walk a callee body or emit a diagnostic — what
/// this seam adds is the *value* of an operand, which is issue #158's own
/// distinction in the other direction: that variant is unmodeled about its
/// value, never about its effects, and the operand's invalidation set keeps
/// doing its existing job untouched.
fn cmp_operand_values(
    w: &WalkCx,
    folder: &mut dyn Folder,
    op: &CondOperand,
    env: &HashMap<String, Known>,
    poisoned: bool,
) -> Option<Vec<ArgValue>> {
    if let Some(vs) = operand_values(op, env, poisoned) {
        return Some(vs);
    }
    let CondOperand::Other { call: Some(call), .. } = op else { return None };
    if !call.positional_only || call.has_spread {
        return None;
    }
    let name = call.callee.clone()?;
    let args: Vec<ArgValue> = call.args.iter().map(|a| a.value.clone()).collect();
    w.cx.cmp_candidates_under(&ArgValue::Call(name, args), env, poisoned, folder, None, None)
}

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
