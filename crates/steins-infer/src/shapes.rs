//! Shape narrowing and write invalidation (ADR-0062 S4 / A-G8): guards on the fact
//! lane narrow a shape by subtraction, presence disjuncts cover a key, and writes
//! through an offset invalidate what the shape claimed.

use std::collections::HashMap;

use steins_contract::ContractTy;
use steins_domain::{
    Base, Certainty, CoverFlavor, Fact, IntRange, Refinement, ShapeFact, Key as VKey, Val,
};
use steins_syntax::{ArgValue, CallExpr, CmpOp, CondExpr, CondOperand, Span};

use crate::Folder;
use crate::cond::eval_cmp;
use crate::cx::Cx;
use crate::dump::SHAPE_REFINED;
use crate::env::{ContractArm, Known, Store, Stratum, arg_of_val, singleton_fact, val_of};
use crate::existence::global_function_callee;
use crate::offsets::{OffsetGrade, offset_key_of};
use crate::project::Diagnostic;
use crate::refine::{flip_ordering, negate_ordering, operand_call, seed_shape_fact};
use crate::transfers::transfer_arg_known;
use crate::walk::WalkCx;

// ---------------------------------------------------------------------------
// Shape narrowing (ADR-0062 S4): guards on the fact lane, subtraction on the
// arm lane, and the collapse that mints one from the other.
//
// A binding with ONE array arm carries `Fact::Shape`, refined by the domain's
// narrowing operators; SEVERAL arms carry none (union lives in the arm lane,
// A-G3) and are refined by deleting arms. When subtraction leaves exactly one
// arm, `seed_shape_fact` mints the fact and the same guard promotes it.
//
// Reachability is deliberately untouched (feeds none of `eval_cond`,
// `mark_dead`, the dead-region set): a shape fact is `Asserted` (A-G9's
// corollary), and a dead region from an `Asserted` premise would silence the
// env-free direct pass on a live path — an FP class that must stay closed.
// `Fact::truthy`/`is_null`/`int_in`/`satisfies_str` are decisive on
// `Fact::Shape`, so tripwire test `shape_facts_do_not_decide_guard_verdicts`
// (tests/shape_guards.rs) guards against a caller reopening that question.
// ---------------------------------------------------------------------------

/// Which presence predicate a guard tests — A-G8's flavor discipline applied to
/// guards rather than covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PresenceFlavor {
    /// `isset($x[k])`: the key is present **and its value is not null**.
    Isset,
    /// `array_key_exists(k, $x)`: the key exists; the value may be null.
    KeyExists,
}

/// One guard form S4 consumes, already resolved to a branch polarity.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ShapeGuard {
    /// A key-presence guard (A-G3).
    Present { var: String, key: VKey, flavor: PresenceFlavor, positive: bool },
    /// `if ($x)` on an array binding.
    Truthy { var: String, positive: bool },
    /// `array_is_list($x)` — the RFC's C1 flag flip.
    IsList { var: String, positive: bool },
    /// A constant-key projection guard (A-G4): `$x[k] === <lit>`, or a
    /// `match`/`switch` arm on `$x[k]`. Several tags come from a stacked arm
    /// (`case 1: case 2:` / `1, 2 => …`); `loose` is the `switch` reading.
    Tag { var: String, key: VKey, tags: Vec<ArgValue>, loose: bool },
    /// A **disjunctive**-presence guard (A-G8, S5): `isset($x['a']) ||
    /// isset($x['b'])` and its `array_key_exists` twin, over ONE binding, at
    /// truth polarity. `flavor` is the weakest claim every disjunct implies —
    /// a mixed disjunction reads as [`PresenceFlavor::KeyExists`].
    Cover { var: String, keys: Vec<VKey>, flavor: PresenceFlavor },
    /// `array_all($x, $f)` falsy / `array_any($x, $f)` truthy (A8, ADR-0062
    /// §4, PHP 8.4): the ONE unconditional leg of each — `array_all([], f)`
    /// and `array_any([], f)` are respectively always true and always false,
    /// so only the branch that leg refutes proves `$x` non-empty.
    /// [`collect_shape_guards`] pushes this only at the firing polarity.
    NonEmpty { var: String },
    /// **A count comparison** (issue #272): `count($x) <op> <int>` resolved to
    /// the entry-count interval the branch proves. Both polarities record —
    /// the false arm of `count($x) > 0` proves `count($x) <= 0`, which is the
    /// complement interval and not a vacuity trap — and the interval is
    /// already the branch's, so nothing downstream re-reads the operator.
    Count { var: String, range: IntRange },
}

impl ShapeGuard {
    pub(crate) fn var(&self) -> &str {
        match self {
            ShapeGuard::Present { var, .. }
            | ShapeGuard::Truthy { var, .. }
            | ShapeGuard::IsList { var, .. }
            | ShapeGuard::Tag { var, .. }
            | ShapeGuard::Cover { var, .. }
            | ShapeGuard::NonEmpty { var, .. }
            | ShapeGuard::Count { var, .. } => var,
        }
    }
}

/// A guard's literal key, canonicalized by PHP's own key rule — the SAME
/// [`offset_key_of`] the read side uses, so a guard and a read can never
/// disagree about which key they mean.
pub(crate) fn guard_key(arg: &ArgValue, php_minor: Option<(u16, u16)>) -> Option<VKey> {
    offset_key_of(&val_of(arg, php_minor)?)
}

/// The recognized array-predicate a guard call names, or `None` for a call that
/// does not denote the global builtin ([`global_function_callee`]: a
/// `Foo\array_key_exists` or a same-named user function is a different function).
pub(crate) fn array_guard_predicate(cx: &Cx, call: &CallExpr) -> Option<&'static str> {
    let callee = global_function_callee(cx, call)?;
    if !call.positional_only {
        return None;
    }
    ["array_key_exists", "key_exists", "array_is_list"]
        .into_iter()
        .find(|p| callee.eq_ignore_ascii_case(p))
}

/// The recognized `array_all`/`array_any` name a guard call names (A8), through
/// the same [`global_function_callee`] as [`array_guard_predicate`] — kept as a
/// separate lookup because the two calls have a different arity (`$array,
/// $callback`) and a different firing rule than the presence/list-flag guards.
pub(crate) fn array_all_any_predicate(cx: &Cx, call: &CallExpr) -> Option<&'static str> {
    let callee = global_function_callee(cx, call)?;
    if !call.positional_only {
        return None;
    }
    ["array_all", "array_any"].into_iter().find(|p| callee.eq_ignore_ascii_case(p))
}

/// The binding whose entry count an operand **is** — `count($x)` / `sizeof($x)`
/// over a bare local (issue #272), or `None` for everything else.
///
/// Refusals, each a soundness/scope requirement: callee must denote the
/// **global builtin** ([`global_function_callee`], not a project function
/// named `count`); call must be **positional-only with exactly one argument**
/// (`count($x, COUNT_RECURSIVE)` counts nested entries — a different number —
/// and `count(value: $x)` is refused with it); argument must be a **bare
/// local** (the only thing fact lanes key by); operand must be a *resolvable*
/// call (`CondOperand::Other`'s `call`), so `count($x) + 1` declines.
fn count_subject<'a>(cx: &Cx, operand: &'a CondOperand) -> Option<&'a str> {
    let call = operand_call(operand)?;
    let callee = global_function_callee(cx, call)?;
    if !call.positional_only || call.args.len() != 1 {
        return None;
    }
    if !["count", "sizeof"].iter().any(|n| callee.eq_ignore_ascii_case(n)) {
        return None;
    }
    match &call.args[0].value {
        ArgValue::Var(v) => Some(v.as_str()),
        _ => None,
    }
}

/// The int interval an operand denotes, when the engine can bound it: a literal
/// int is a point, and a binding carrying an int fact contributes its own
/// interval (`int<3, 5>` bounds `count($x) === $range` exactly as a literal
/// does). Anything else — a float, a string, an unbounded or non-int fact —
/// declines, and the guard with it.
fn operand_int_bound(operand: &CondOperand, env: &HashMap<String, Known>) -> Option<IntRange> {
    match operand {
        CondOperand::Literal(ArgValue::Int(i)) => Some(IntRange::point(*i)),
        CondOperand::Var(v) => match env.get(v)?.fact.as_ref()? {
            Fact::Singleton(Val::Int(i)) => Some(IntRange::point(*i)),
            Fact::Refined { base: Base::Int, refinement: Refinement::Int(r), nullable: false } => {
                Some(*r)
            }
            _ => None,
        },
        _ => None,
    }
}

/// The logical negation of a comparison, for the false branch. Orderings flip
/// through [`negate_ordering`]; the equality pairs swap. Total, so the false
/// arm of a count guard is derived rather than special-cased at each operator.
fn negate_cmp(op: CmpOp) -> CmpOp {
    match op {
        CmpOp::Identical => CmpOp::NotIdentical,
        CmpOp::NotIdentical => CmpOp::Identical,
        CmpOp::Loose => CmpOp::NotLoose,
        CmpOp::NotLoose => CmpOp::Loose,
        other => negate_ordering(other),
    }
}

/// **The count-comparison guard** (issue #272): the entry-count interval
/// `count($x) <op> <bound>` proves on branch `then`.
///
/// The comparison is normalized to read left-to-right (`flip_ordering` mirrors
/// a Yoda `0 < count($x)`), negated on the false branch, then read against the
/// bound's own interval `[lo, hi]` (not a literal, so a bounded variable also
/// works) — each case is the weakest sound claim over that interval: `>`/`>=`
/// give at least `lo`(+1); `<`/`<=` mirror against `hi`; `===`/`==` give
/// `[lo, hi]`; `!==`/`!=` give nothing except against the point `0`, where the
/// complement (non-empty) is representable.
///
/// PHP compares `count($x)` as an int, so a loose comparison against an int
/// bound is the strict one; a non-int bound never reaches here
/// ([`operand_int_bound`] declines it).
///
/// An interval the ordering empties (`count($x) < 0`) declines — the branch is
/// impossible, and death is the verdict's business (ADR-0052 §2).
fn count_guard(
    cx: &Cx,
    env: &HashMap<String, Known>,
    op: CmpOp,
    lhs: &CondOperand,
    rhs: &CondOperand,
    then: bool,
) -> Option<ShapeGuard> {
    let (var, bound, count_on_left) = match (count_subject(cx, lhs), count_subject(cx, rhs)) {
        // `count($a) === count($b)` relates two bindings and bounds neither.
        (Some(_), Some(_)) => return None,
        (Some(v), None) => (v, rhs, true),
        (None, Some(v)) => (v, lhs, false),
        (None, None) => return None,
    };
    let bound = operand_int_bound(bound, env)?;
    let op = if count_on_left { op } else { flip_ordering(op) };
    let op = if then { op } else { negate_cmp(op) };
    let (lo, hi) = (bound.lo(), bound.hi());
    let range = match op {
        CmpOp::Gt => IntRange::new(lo.checked_add(1)?, i64::MAX),
        CmpOp::Ge => IntRange::new(lo, i64::MAX),
        CmpOp::Lt => IntRange::new(0, hi.checked_sub(1)?),
        CmpOp::Le => IntRange::new(0, hi),
        CmpOp::Identical | CmpOp::Loose => IntRange::new(lo, hi),
        CmpOp::NotIdentical | CmpOp::NotLoose => {
            (lo == 0 && hi == 0).then_some(IntRange::POSITIVE)
        }
    }?;
    Some(ShapeGuard::Count { var: var.to_owned(), range })
}

/// Collect the shape guards a condition establishes at polarity `then`.
///
/// The polarity walk mirrors `collect_refine`'s structure: `Not` flips, `And`
/// contributes on the true path, `Or` on the false one (De Morgan). Everything
/// else contributes nothing, so an unmodeled condition narrows nothing rather
/// than narrowing wrongly.
///
/// `env` is read for **operand bounds only** (issue #272's `count($x) === $n`,
/// where the other side is a binding carrying an int interval); every guard is
/// still decided from the condition's own syntax, so the walk stays replayable
/// in the ADR-0048 §1 sense.
pub(crate) fn collect_shape_guards(
    cx: &Cx,
    cond: &CondExpr,
    then: bool,
    env: &HashMap<String, Known>,
    out: &mut Vec<ShapeGuard>,
) {
    let php_minor = cx.php_minor;
    match cond {
        CondExpr::Isset { var, key } => {
            if let Some(k) = guard_key(key, php_minor) {
                out.push(ShapeGuard::Present {
                    var: var.clone(),
                    key: k,
                    flavor: PresenceFlavor::Isset,
                    positive: then,
                });
            }
        }
        CondExpr::Truthy(CondOperand::Var(v)) => {
            out.push(ShapeGuard::Truthy { var: v.clone(), positive: then });
        }
        // `$x[k] === <lit>` and its negation-by-polarity twin `!($x[k] !== <lit>)`.
        // Only the *positive* reading subtracts: "tag is NOT 'circle'" kills an
        // arm only if that arm's slot admits nothing else — a residue question
        // A-G4 does not open in v1.
        CondExpr::Cmp { op, lhs, rhs } => {
            // A count comparison (issue #272) is the one comparison form whose
            // operand is a *call*; it cannot also be a tag guard.
            if let Some(g) = count_guard(cx, env, *op, lhs, rhs, then) {
                out.push(g);
                return;
            }
            let positive = match op {
                CmpOp::Identical | CmpOp::Loose => then,
                CmpOp::NotIdentical | CmpOp::NotLoose => !then,
                _ => return,
            };
            let loose = matches!(op, CmpOp::Loose | CmpOp::NotLoose);
            let (offset, lit) = match (lhs, rhs) {
                (CondOperand::Offset { var, key }, CondOperand::Literal(v))
                | (CondOperand::Literal(v), CondOperand::Offset { var, key }) => {
                    ((var, key), v)
                }
                _ => return,
            };
            // The **negative**-equality reading against `null` (`$x[k] !== null`
            // true, or `$x[k] === null` false) is `isset($x[k])`'s own truth
            // table (issue #421's follow-up to #418: the possibly-grade
            // argument pair convicted `needInt($a['k'])` under exactly this
            // guard, because nothing narrowed the field). PHP's `isset` on an
            // absent key and on a present-null one both read `false`, which is
            // this comparison's truth table too — strict only, since a loose
            // `!= null` is also true for `0`/`''`/`'0'`/`[]` and proves nothing
            // about this key's nullness (the same one-direction carve-out
            // `collect_cmp_refine` takes for a `Var`). Routes through the SAME
            // `ShapeGuard::Present`/`promote_present` isset already does, so
            // the two guards can never disagree about what a key's presence
            // narrows to.
            if !positive && !loose && matches!(lit, ArgValue::Null) {
                if let Some(k) = guard_key(offset.1, php_minor) {
                    out.push(ShapeGuard::Present {
                        var: offset.0.clone(),
                        key: k,
                        flavor: PresenceFlavor::Isset,
                        positive: true,
                    });
                }
                return;
            }
            if !positive {
                return;
            }
            if let Some(k) = guard_key(offset.1, php_minor) {
                out.push(ShapeGuard::Tag {
                    var: offset.0.clone(),
                    key: k,
                    tags: vec![lit.clone()],
                    loose,
                });
            }
        }
        CondExpr::Call { call, .. } => {
            if let Some(pred) = array_guard_predicate(cx, call) {
                match pred {
                    "array_key_exists" | "key_exists" => {
                        if call.args.len() != 2 {
                            return;
                        }
                        let ArgValue::Var(var) = &call.args[1].value else { return };
                        if let Some(k) = guard_key(&call.args[0].value, php_minor) {
                            out.push(ShapeGuard::Present {
                                var: var.clone(),
                                key: k,
                                flavor: PresenceFlavor::KeyExists,
                                positive: then,
                            });
                        }
                    }
                    _ => {
                        if call.args.len() != 1 {
                            return;
                        }
                        if let ArgValue::Var(var) = &call.args[0].value {
                            out.push(ShapeGuard::IsList { var: var.clone(), positive: then });
                        }
                    }
                }
                return;
            }
            // A8: `array_all` fires falsy (vacuously true on `[]`, so falsy means
            // an element existed and failed); `array_any` fires truthy (vacuously
            // false on `[]`). The opposite branch is a vacuity trap — no guard.
            if let Some(pred) = array_all_any_predicate(cx, call) {
                if call.args.len() != 2 {
                    return;
                }
                let fires = match pred {
                    "array_all" => !then,
                    "array_any" => then,
                    _ => false,
                };
                if fires && let ArgValue::Var(var) = &call.args[0].value {
                    out.push(ShapeGuard::NonEmpty { var: var.clone() });
                }
            }
        }
        CondExpr::Not(c) => collect_shape_guards(cx, c, !then, env, out),
        CondExpr::And(a, b) if then => {
            collect_shape_guards(cx, a, then, env, out);
            collect_shape_guards(cx, b, then, env, out);
        }
        // De Morgan: `¬(isset a ∨ isset b)` is `¬isset a ∧ ¬isset b`, so the false
        // branch is just both disjuncts at false polarity (per-key S4 narrowing);
        // the cover (S5) lives on the TRUE branch only.
        CondExpr::Or(a, b) if !then => {
            collect_shape_guards(cx, a, then, env, out);
            collect_shape_guards(cx, b, then, env, out);
        }
        // The true branch of a disjunction (A-G8, S5). Individually a disjunct
        // proves nothing, so only the disjunctive fact itself is recorded.
        CondExpr::Or(..) if then => {
            if let Some(g) = disjunctive_cover(cx, cond) {
                out.push(g);
            }
        }
        _ => {}
    }
}

/// One disjunct of a truth-context `||` chain, as A-G11's v1 scope admits it: a
/// depth-1 constant-key presence test over a named binding.
type PresenceDisjunct = (String, VKey, PresenceFlavor);

/// Flatten a `||` chain into its presence disjuncts. Returns `false` — the
/// whole cover abandoned — if ANY disjunct is something else: a non-presence
/// condition, a deeper path, or a non-constant key. A disjunction is only as
/// strong as its weakest disjunct, so one unmodelled arm voids the whole claim.
fn presence_disjuncts(cx: &Cx, cond: &CondExpr, out: &mut Vec<PresenceDisjunct>) -> bool {
    match cond {
        CondExpr::Or(a, b) => {
            presence_disjuncts(cx, a, out) && presence_disjuncts(cx, b, out)
        }
        CondExpr::Isset { var, key } => match guard_key(key, cx.php_minor) {
            Some(k) => {
                out.push((var.clone(), k, PresenceFlavor::Isset));
                true
            }
            None => false,
        },
        CondExpr::Call { call, .. } => {
            let Some(pred) = array_guard_predicate(cx, call) else { return false };
            if !matches!(pred, "array_key_exists" | "key_exists") || call.args.len() != 2 {
                return false;
            }
            let ArgValue::Var(var) = &call.args[1].value else { return false };
            match guard_key(&call.args[0].value, cx.php_minor) {
                Some(k) => {
                    out.push((var.clone(), k, PresenceFlavor::KeyExists));
                    true
                }
                None => false,
            }
        },
        _ => false,
    }
}

/// The [`ShapeGuard::Cover`] a truth-context disjunction records, or `None` when
/// A-G11's v1 scope declines it.
///
/// Requires: **every** disjunct is a modelled presence test (see
/// [`presence_disjuncts`]); they all test the **same** binding (a cover is a
/// fact about one array); at least two distinct keys remain (a singleton would
/// be presence, which `normalize` promotes anyway).
///
/// Flavor is the **weakest** claim every disjunct implies: all-`isset` gives an
/// Isset-cover; any `array_key_exists` disjunct drags the whole cover down to
/// KeyExists, since a present-null entry satisfies that disjunct without
/// satisfying `isset`.
fn disjunctive_cover(cx: &Cx, cond: &CondExpr) -> Option<ShapeGuard> {
    let mut parts: Vec<PresenceDisjunct> = Vec::new();
    if !presence_disjuncts(cx, cond, &mut parts) || parts.len() < 2 {
        return None;
    }
    let var = parts[0].0.clone();
    if parts.iter().any(|(v, _, _)| *v != var) {
        return None;
    }
    let flavor = if parts.iter().all(|(_, _, f)| *f == PresenceFlavor::Isset) {
        PresenceFlavor::Isset
    } else {
        PresenceFlavor::KeyExists
    };
    let mut keys: Vec<VKey> = parts.into_iter().map(|(_, k, _)| k).collect();
    keys.dedup();
    Some(ShapeGuard::Cover { var, keys, flavor })
}

/// **Apply every shape guard of `cond` at polarity `then`** to a branch's cloned
/// env and store. Runs after `apply_refinements`, so a shape operator is the
/// last word on a `Fact::Shape` binding — no scalar refinement operator can
/// express anything about one anyway.
///
/// `witnessed` is the **evidence stratum of the condition itself** (ADR-0058's
/// table): `true` for a runtime-evaluated condition (`if`, `assert()`), `false`
/// for docblock-only evidence (a `@phpstan-assert true $cond` tag on a userland
/// helper). It reaches exactly one operator, [`ShapeFact::promote_present`];
/// every other narrowing here is already confined to the `Asserted` shape lane
/// (A-G9's corollary), whose stratum does not vary per guard.
pub(crate) fn apply_shape_narrowing(
    cx: &Cx,
    cond: &CondExpr,
    then: bool,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
    witnessed: bool,
) {
    let mut guards = Vec::new();
    collect_shape_guards(cx, cond, then, env, &mut guards);
    for g in &guards {
        apply_shape_guard(cx, g, env, store, witnessed);
    }
}

/// One guard, both lanes: subtract the arm lane, mint a fact if the subtraction
/// collapsed the union to one array arm, then refine the fact.
pub(crate) fn apply_shape_guard(
    cx: &Cx,
    g: &ShapeGuard,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
    witnessed: bool,
) {
    subtract_shape_arms(cx, g, store);
    mint_collapsed_shape(g.var(), env, store);
    refine_shape_fact(g, env, witnessed);
}

/// **Arm subtraction** (A-G3/A-G4): delete the arms of `var`'s contract lane the
/// guard proves cannot be the live one. Non-array arms are left alone except for
/// the one case PHP decides — `isset` on an offset of `null` is false, so an
/// `isset`-true branch kills a `null` arm.
///
/// An emptied lane drops to no-fact, never a death signal (ADR-0052 §2). Marks
/// [`Store::narrowed`] on the way out (issue #428) — every path past the
/// `kept.len() == arms.len()` no-op return below has already proven a kill.
fn subtract_shape_arms(cx: &Cx, g: &ShapeGuard, store: &mut Store) {
    let Some(arms) = store.contract.get(g.var()) else { return };
    // A single-arm lane has no discrimination to do.
    if arms.len() < 2 {
        return;
    }
    let kept: Vec<ContractArm> =
        arms.iter().filter(|a| shape_arm_survives(g, &a.ty, cx.php_minor)).cloned().collect();
    if kept.len() == arms.len() {
        return;
    }
    // **A count guard never empties the lane** (issue #272). Other guards refute
    // an arm on structural grounds, where an emptied lane means out-of-vocabulary;
    // a count guard refutes on arithmetic, where an emptied lane would mean the
    // branch is unreachable — a claim §2 reserves for the verdict. Left whole.
    if kept.is_empty() && matches!(g, ShapeGuard::Count { .. }) {
        return;
    }
    store.narrowed.insert(g.var().to_owned());
    if kept.is_empty() {
        store.contract.remove(g.var());
    } else {
        store.contract.insert(g.var().to_owned(), kept);
    }
}

/// Does `ty` survive `g`? The FP-safe answer is always `true`: an arm dies only
/// on a definite verdict, exactly as ADR-0052 §2's arm deletion does.
fn shape_arm_survives(g: &ShapeGuard, ty: &ContractTy, php_minor: Option<(u16, u16)>) -> bool {
    use steins_domain::Presence;
    let shape = steins_contract::to_shape_fact(ty);
    match g {
        ShapeGuard::Present { key, flavor, positive, .. } => {
            // `isset($x[k])` on a `null` base is false — PHP decides it, so the
            // arm dies. `array_key_exists` on null is a TypeError, so it kills
            // nothing (the conservative reading).
            if matches!(ty, ContractTy::Null) {
                return !(*positive && *flavor == PresenceFlavor::Isset);
            }
            let Some(shape) = shape else { return true };
            if *positive {
                // Sealed-by-default makes the idiom sound (A-G3): an arm that
                // cannot hold `k` at all cannot be the live one.
                arm_can_hold_key(&shape, key)
            } else {
                // False branch kills arms where `k` is `Required` — for `isset`
                // only when the value cannot be null, since a present-null entry
                // makes `isset` false without the key being absent (A-G8 S2).
                match shape.field(key) {
                    Some((_, p, slot)) if p.is_required() => match flavor {
                        PresenceFlavor::KeyExists => false,
                        PresenceFlavor::Isset => {
                            !slot.as_ref().is_some_and(|f| f.is_null().is_no())
                        }
                    },
                    _ => true,
                }
            }
        }
        // Asks the field's declared value contract whether it admits the literal;
        // a `No` verdict kills the arm (A-G4). Undeclared/unknown-slot keeps it.
        ShapeGuard::Tag { key, tags, loose, .. } => {
            let Some(shape) = shape else { return true };
            let Some((_, presence, Some(slot))) = shape.field(key) else { return true };
            if matches!(presence, Presence::Absent) {
                return false;
            }
            tags.iter().any(|t| tag_possible(slot, t, *loose, php_minor))
        }
        // Truthiness, list-ness, and A8's non-emptiness are whole-array
        // properties, and every arm can be non-empty/a list for *some* value it
        // admits — v1 subtracts nothing here, refines the fact lane only.
        ShapeGuard::Truthy { .. } | ShapeGuard::IsList { .. } | ShapeGuard::NonEmpty { .. } => {
            true
        }
        // A count guard **does** discriminate arms (issue #272), through each
        // array arm's own count interval: an arm whose interval excludes `range`
        // cannot be live (`array{}` dies under `count($x) > 0`; `array<int>`
        // survives, interval `int<0, max>`). Non-array arms keep their place
        // (`count()` accepts `Countable`) EXCEPT `Null`, which dies unconditionally
        // (issue #289): `count(null)` raises a `TypeError`, so reaching this
        // branch at all already proves the subject was not null — arithmetic
        // reachability, decided before `to_shape_fact` is even consulted.
        ShapeGuard::Count { range, .. } => {
            if matches!(ty, ContractTy::Null) {
                return false;
            }
            shape.is_none_or(|s| s.count_range().intersect(*range).is_some())
        }
        // **Covers on arms are future work.** An arm holding NONE of the covered
        // keys is in fact refuted (same sealed-by-default reasoning as
        // `Present { positive: true }`), but v1 records covers on a single-shape
        // fact only, so nothing is subtracted here.
        ShapeGuard::Cover { .. } => true,
    }
}

/// Can an array admitted by `shape` have the key `k` at all?
fn arm_can_hold_key(shape: &ShapeFact, k: &VKey) -> bool {
    use steins_domain::{Presence, Tail};
    match shape.field(k) {
        Some((_, Presence::Absent, _)) => false,
        Some(_) => true,
        None => match &shape.tail {
            Tail::Sealed => false,
            Tail::Unsealed { key: class, .. } => class.admits_key(k),
        },
    }
}

/// Could the slot's declared value equal the tag? `loose` selects PHP's `==`
/// (the `switch` reading), whose truth set is a superset of `===`'s — so a
/// finite slot is compared through [`eval_cmp`] rather than by `admits`, and an
/// abstract slot under a loose comparison keeps the arm (undecidable from the
/// fact alone).
fn tag_possible(slot: &Fact, tag: &ArgValue, loose: bool, php_minor: Option<(u16, u16)>) -> bool {
    match slot.finite_members() {
        Some(members) => {
            let op = if loose { CmpOp::Loose } else { CmpOp::Identical };
            let args: Vec<ArgValue> = members.iter().map(arg_of_val).collect();
            eval_cmp(op, &args, std::slice::from_ref(tag), php_minor) != Certainty::No
        }
        // An abstract slot decides only under `===`, where membership *is* the
        // question `admits` answers.
        None => loose || val_of(tag, php_minor).is_none_or(|v| slot.admits(&v)),
    }
}

/// **The collapse rule** (A-G3): once subtraction leaves a lane with one array
/// arm, that lane states a single shape truth — mint it into the fact lane
/// through the S3 lowering ([`seed_shape_fact`], the same one entry-state
/// seeding uses).
///
/// Only ever *adds* a fact: a binding that already carries one (seeded, or
/// minted by an earlier guard) is left to the refinement step; a binding
/// carrying a non-shape fact (e.g. a proven `Singleton` array) is strictly
/// better information and is never overwritten.
pub(crate) fn mint_collapsed_shape(var: &str, env: &mut HashMap<String, Known>, store: &Store) {
    if env.get(var).is_some_and(|k| k.fact.is_some()) {
        return;
    }
    let Some(arms) = store.contract.get(var) else { return };
    let Some(fact) = seed_shape_fact(arms) else { return };
    let line = env.get(var).map_or(0, |k| k.line);
    env.insert(
        var.to_owned(),
        // `Asserted`, same reason entry-state seeding is (A-G9's corollary).
        Known::value_strat(fact, line, Some(SHAPE_REFINED.to_owned()), Stratum::Asserted),
    );
}

/// **The value lane's answer to a count guard** (issue #272): the fact that
/// replaces a *proven* array the branch's count interval excludes, or `None`
/// when the value survives unchanged.
///
/// Other shape guards narrow only `Fact::Shape` and say nothing about a proven
/// `Fact::Singleton(Val::Array(…))`, since presence/list-ness are already
/// decided on a literal. A count comparison can still contradict a literal
/// (`count($x) > 0` where `$x` is proven `[]`): before this guard existed, such
/// a comparison lowered to `CondExpr::Opaque`, which dropped the binding, so
/// the stale literal never survived; keeping the comparison means the literal
/// must be narrowed here too, or the contract checker (`resolve_cval`) would
/// convict on a stale value. Both lanes are narrowed in one place so they
/// cannot disagree.
///
/// The replacement is **not** a lifted-and-narrowed shape: nothing about the
/// refuted entries (keys, value types) survives as proof, only "an array whose
/// entry count lies in `range`" — the honest floor, still a narrowing. Marking
/// the branch dead is the verdict's business, not a narrowing operator's
/// (ADR-0052 §2).
///
/// A `OneOf` of arrays filters member-wise (sharper: only excluded members
/// drop). A `OneOf` with any non-array member is left alone — `count()`
/// accepts a `Countable` too, and this rule does not guess at one.
fn refuted_array_value(fact: &Fact, range: IntRange) -> Option<Fact> {
    let in_range = |entries: &[(VKey, Val)]| {
        i64::try_from(entries.len()).is_ok_and(|n| range.contains(n))
    };
    // "An array with this entry count": everything the guard leaves standing.
    let widened = || Fact::Shape {
        shape: Box::new(ShapeFact::plain_array().narrow_count(range)),
        nullable: false,
    };
    match fact {
        Fact::Singleton(Val::Array(entries)) => (!in_range(entries)).then(widened),
        Fact::OneOf(vals) => {
            if !vals.iter().all(|v| matches!(v, Val::Array(_))) {
                return None;
            }
            let kept: Vec<Val> = vals
                .iter()
                .filter(|v| matches!(v, Val::Array(e) if in_range(e)))
                .cloned()
                .collect();
            if kept.len() == vals.len() {
                return None;
            }
            Some(Fact::from_vals(kept).unwrap_or_else(widened))
        }
        _ => None,
    }
}

/// **Fact-lane refinement**: apply the guard's domain operator to `var`'s
/// `Fact::Shape`, if it has one. Every operator is a narrowing
/// (`crates/steins-domain/src/shape.rs`), so the result admits everything the
/// binding admitted that satisfies the guard.
fn refine_shape_fact(g: &ShapeGuard, env: &mut HashMap<String, Known>, witnessed: bool) {
    use steins_domain::Presence;
    let Some(known) = env.get(g.var()) else { return };
    // **Value-lane coherence first** (issue #272): a count guard is the one
    // guard that can refute a *proven* array outright. See [`refuted_array_value`].
    if let ShapeGuard::Count { range, .. } = g
        && let Some(fact) = known.fact.as_ref()
        && let Some(next) = refuted_array_value(fact, *range)
    {
        let (line, stratum) = (known.line, known.stratum);
        env.insert(
            g.var().to_owned(),
            Known::value_strat(next, line, Some(SHAPE_REFINED.to_owned()), stratum),
        );
        return;
    }
    let Some(Fact::Shape { shape, nullable }) = &known.fact else { return };
    let (shape, nullable) = (shape.as_ref(), *nullable);
    let next = match g {
        ShapeGuard::Present { key, flavor, positive: true, .. } => Some(Fact::Shape {
            shape: Box::new(shape.promote_present(
                key,
                *flavor == PresenceFlavor::Isset,
                witnessed,
            )),
            // `isset($x[k])` false when `$x` is null, so true branch also proves
            // non-null. `array_key_exists` on null raises TypeError, proves nothing.
            nullable: nullable && *flavor == PresenceFlavor::KeyExists,
        }),
        ShapeGuard::Present { key, flavor, positive: false, .. } => {
            match (flavor, shape.field(key)) {
                // `!array_key_exists(k, $x)` tests key existence and nothing
                // else: the key is proven absent whatever its declared value.
                (PresenceFlavor::KeyExists, _) => {
                    Some(Fact::Shape { shape: Box::new(shape.mark_absent(key)), nullable })
                }
                // `!isset($x[k])` on an optional non-nullable slot: the only way
                // the guard can be false is the key being absent.
                (PresenceFlavor::Isset, Some((_, Presence::Optional, slot)))
                    if slot.as_ref().is_some_and(|f| f.is_null().is_no()) =>
                {
                    Some(Fact::Shape { shape: Box::new(shape.mark_absent(key)), nullable })
                }
                // A `Required` field with a non-nullable slot makes this branch
                // runtime-impossible. **Deliberate v1 conservatism**: env left
                // unchanged rather than marking the region dead — death is the
                // verdict's business (ADR-0052 §2), and this premise is `Asserted`
                // (A-G9), whose dead region would silence a live path.
                _ => None,
            }
        }
        ShapeGuard::Truthy { positive: true, .. } => {
            Some(Fact::Shape { shape: Box::new(shape.set_non_empty()), nullable })
        }
        // A falsy array is the empty array — only when the base cannot be null,
        // since `null` is falsy too.
        ShapeGuard::Truthy { positive: false, .. } => {
            (!nullable).then(|| Fact::Singleton(Val::Array(Vec::new())))
        }
        ShapeGuard::IsList { positive, .. } => Some(Fact::Shape {
            shape: Box::new(shape.set_is_list(Certainty::from_bool(*positive))),
            nullable,
        }),
        // A8: fires only on the leg that proves non-emptiness (unlike `Truthy`,
        // no positive/negative split needed). Value lane untouched: `array_all`/
        // `array_any` say nothing about order or values, only that an entry exists.
        ShapeGuard::NonEmpty { .. } => {
            Some(Fact::Shape { shape: Box::new(shape.set_non_empty()), nullable })
        }
        // The S5 recording (A-G8): the disjunction's own claim, stored on the
        // fact for A-G11's `??` discharge to consume.
        ShapeGuard::Cover { keys, flavor, .. } => Some(Fact::Shape {
            shape: Box::new(shape.record_cover(
                keys.clone(),
                match flavor {
                    PresenceFlavor::Isset => CoverFlavor::Isset,
                    PresenceFlavor::KeyExists => CoverFlavor::KeyExists,
                },
            )),
            // The WHOLE disjunction being true means at least one `isset` returned
            // true — and `isset` on an offset of `null` is false — so an all-`isset`
            // disjunction proves the base is a non-null array. (A single *false*
            // `isset($x['a'])` would prove nothing of the sort; it is the truth of
            // the disjunction, not of any one disjunct, that carries this.) An
            // `array_key_exists` disjunct raises a TypeError on `null` rather than
            // answering, so a KeyExists-flavored cover proves nothing here —
            // exactly the reading `Present { positive: true }` already takes.
            nullable: nullable && *flavor == PresenceFlavor::KeyExists,
        }),
        // The count accessory (issue #272, lifted by issue #289). `nullable` IS
        // cleared here, on both arms. `count(null)` raises a `TypeError` —
        // reaching *either* branch at all means the call returned, which means
        // the subject was not null: the exception reaches neither the true nor
        // the false arm, so both prove non-null. This is the mirror image of
        // `array_key_exists`'s reading (`Present`, above): that guard *answers
        // false* on a null base, so its false-answering tells the analysis
        // nothing about nullness; `count()` on a null base never answers at
        // all, and the branch existing is itself the proof. Treating the two
        // alike was the argued-backwards reading ADR-0052's 2026-08-09 count
        // note recorded and issue #289 lifts.
        ShapeGuard::Count { range, .. } => {
            Some(Fact::Shape { shape: Box::new(shape.narrow_count(*range)), nullable: false })
        }
        // A tag guard's job is arm subtraction; the collapsed arm's own slot is
        // already the declared literal, so refining the fact adds nothing v1.
        ShapeGuard::Tag { .. } => None,
    };
    let Some(next) = next else { return };
    let (line, stratum) = (known.line, known.stratum);
    env.insert(
        g.var().to_owned(),
        Known::value_strat(next, line, Some(SHAPE_REFINED.to_owned()), stratum),
    );
}

// ---------------------------------------------------------------------------
// Write invalidation (ADR-0062 A-G8's table)
// ---------------------------------------------------------------------------

/// `$var[k] = v` / `$var[k1][k2] = v` and `unset($var[k])`.
///
/// **Barrier first, then one binding.** The walk still clears the whole env and
/// store, exactly as the pre-S4 `Barrier` lowering did — an offset write can
/// alias through references the trace does not model — and only then puts back
/// the base binding's array shape with the key promoted or removed. This rule
/// can move the shape lane and nothing else, so a finding that did not premise
/// a shape fact cannot move with it.
///
/// The by-ref sweep needs no separate fence: the restore reads facts captured
/// *before* the clear, so a by-ref exposure dropped earlier leaves nothing to
/// restore.
pub(crate) fn apply_offset_write(
    w: &WalkCx,
    folder: &mut dyn Folder,
    base: &str,
    keys: &[ArgValue],
    value: Option<&ArgValue>,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
) {
    use steins_domain::{KeyClass, Tail};
    let php_minor = w.cx.php_minor;
    // Capture before the barrier clears everything.
    let before = env.get(base).cloned();
    let arms = store.contract.get(base).cloned();
    // The written value's fact, resolved in the PRE-write env through
    // [`transfer_arg_known`]'s ladder (issue #327), so a value that is
    // *abstract but known* (`$a['k'] = $x` with natively-typed `int $x`) lands
    // as `int` rather than unknown, carrying its own stratum (ADR-0061 §3: the
    // binding cannot come out more trusted than what was written into it).
    // `None` (unresolvable rvalue, or `unset`) leaves the slot unknown. A
    // poisoned scope keeps the literal-only path — an env read there isn't evidence.
    let (slot, slot_stratum) = match value {
        None => (None, Stratum::Verified),
        Some(v) if w.scope.poisoned => (
            w.cx
                .resolve_literal(v, env, true, folder)
                .and_then(|lit| singleton_fact(&lit, php_minor)),
            Stratum::Verified,
        ),
        Some(v) => match transfer_arg_known(w.cx, folder, v, env, Some(&*store)) {
            Some((fact, s)) => (Some(fact), s),
            None => (None, Stratum::Verified),
        },
    };

    env.clear();
    store.clear();

    let Some(known) = before else { return };
    // A base holding an order-witnessed VALUE takes the same path, by lifting
    // (issue #327). Before this, `$b = ['p' => 1]; $b['q'] = 2;` dropped `$b`
    // entirely — the [`array_literal_fact`] cliff reached from the other
    // direction. The lift turns the exact value into a shape; the update rules
    // below are ADR-0062 §4's, unchanged.
    let lifted;
    let (shape, nullable): (&ShapeFact, &bool) = match &known.fact {
        Some(Fact::Shape { shape, nullable }) => (shape.as_ref(), nullable),
        Some(Fact::Singleton(Val::Array(entries))) => {
            lifted = ShapeFact::lift(entries);
            (&lifted, &false)
        }
        _ => return,
    };
    let Some(first) = keys.first().and_then(|k| guard_key(k, php_minor)) else { return };

    // The order witness this write hands on, when the base had one (issue #327).
    // A witnessed base stays witnessed through a write: PHP appends a new key at
    // the end, leaves an existing key where it is, and `unset` removes one from
    // the sequence. Rebuilds below drop the witness; it is re-attached once, at
    // the end, from the sequence computed here.
    let witnessed_order: Option<Vec<VKey>> = shape.order.as_ref().map(|order| {
        let mut order: Vec<VKey> = order.clone();
        match value {
            None => order.retain(|k| *k != first),
            Some(_) => {
                if !order.contains(&first) {
                    order.push(first.clone());
                }
            }
        }
        order
    });

    let next = match value {
        None => {
            // `unset($x[k])` — the key is proven gone. Cover interplay (a cover
            // containing `k` dies with it, A-G8) is handled inside `mark_absent`;
            // the *sharper* law shrinking a cover instead of dropping it is S5's.
            shape.mark_absent(&first)
        }
        Some(_) => {
            // A write makes the key real: `Required { witnessed: true }` with the
            // value's own fact. A nested write (`$x['a']['b'] = v`) autovivifies
            // the OUTER key and **clears its slot**: the inner array just changed
            // in a way the declared slot may no longer describe, so carrying the
            // declaration across could state something the write falsified.
            // Unknown is the honest floor; a real nested-shape update is not v1.
            let nested = keys.len() > 1;
            // A write can only add an entry, so the learned count **ceiling**
            // (issue #272) does not survive it; the floor does.
            let mut next = shape.relax_count_ceiling().promote_present(&first, false, true);
            // Writing an UNDECLARED key under a `Sealed` tail: the runtime value
            // has diverged from the docblock. Resolved the A-G5 way — the write
            // is order-witnessed truth, so the field is added AND the tail
            // unseals. Keeping `Sealed` would reject the very array the code just
            // built; unsealing loses only the declared sealing, from this point on.
            //
            // **Unless the sealing was witnessed too** (issue #327). A base whose
            // construction this walk observed (`$a = []; $a['k'] = $x;`) has no
            // docblock to have diverged from, so the write EXTENDS the sealed
            // shape by the new key instead of opening the tail — added by hand,
            // since `promote_present` only promotes a key a sealed shape already
            // declares.
            if let Some(order) = witnessed_order.as_ref()
                && next.field(&first).is_none()
            {
                let mut fields = next.fields.clone();
                fields.push((
                    first.clone(),
                    steins_domain::Presence::Required { witnessed: true },
                    None,
                ));
                next = ShapeFact::normalize_counted(
                    fields,
                    Tail::Sealed,
                    // Recomputed denotationally from the NEW key sequence: an
                    // append can make a list or break one, and the old flag
                    // survives neither. The canonically sorted fields cannot
                    // tell `[1 => …, 0 => …]` from a list — the sequence can.
                    Certainty::from_bool(steins_domain::keys_are_a_list(order.iter())),
                    true,
                    next.covers.clone(),
                    next.count_bound,
                );
            } else if witnessed_order.is_none() && next.field(&first).is_none() {
                next = ShapeFact::normalize_counted(
                    next.fields.clone(),
                    Tail::Unsealed { key: KeyClass::ArrayKey, value: None },
                    next.is_list,
                    next.non_empty,
                    next.covers.clone(),
                    next.count_bound,
                )
                .promote_present(&first, false, true);
            }
            if nested { set_slot_fact(&next, &first, None) } else { set_slot_fact(&next, &first, slot) }
        }
    };
    // Re-attach the witness the rebuilds dropped. `with_order` re-checks it
    // against the landing shape, so a sequence the update invalidated (unsealed
    // tail, mismatched key count) is refused rather than believed.
    let next = match witnessed_order {
        Some(order) => next.with_order(order),
        None => next,
    };
    env.insert(
        base.to_owned(),
        Known::value_strat(
            Fact::Shape { shape: Box::new(next), nullable: *nullable },
            known.line,
            Some(SHAPE_REFINED.to_owned()),
            known.stratum.min(slot_stratum),
        ),
    );
    if let Some(arms) = arms {
        store.contract.insert(base.to_owned(), arms);
    }
}

/// Replace one field's value slot, keeping every other component. `None` sets the
/// unknown floor. The field is known to exist (the caller promoted it first), so
/// a miss is a no-op.
fn set_slot_fact(shape: &ShapeFact, key: &VKey, fact: Option<Fact>) -> ShapeFact {
    let fields = shape
        .fields
        .iter()
        .map(|(k, p, slot)| {
            if k == key {
                (k.clone(), *p, fact.clone().map(Box::new))
            } else {
                (k.clone(), *p, slot.clone())
            }
        })
        .collect();
    ShapeFact::normalize_counted(
        fields,
        shape.tail.clone(),
        shape.is_list,
        shape.non_empty,
        shape.covers.clone(),
        // A slot replacement changes a value, never the entry count.
        shape.count_bound,
    )
}

/// Whether a normalized array-entry list contains `key` (the read-side membership
/// check, over the already-canonical [`VKey`]s the domain stores).
pub(crate) fn array_has_key(entries: &[(VKey, Val)], key: &VKey) -> bool {
    entries.iter().any(|(k, _)| k == key)
}

/// The `Val` inside a `Singleton` fact (for rendering the container); a no-op clone
/// guarded by the caller having matched `Singleton`.
pub(crate) fn base_fact_val(f: &Fact) -> Val {
    match f {
        Fact::Singleton(v) => v.clone(),
        _ => Val::Null,
    }
}

/// Emit one offset finding, honoring the `warning-handler` posture (ADR-0049 §7):
/// under `"null"` (`!warning_handler_abort`) a warning-grade finding leaves the
/// proof surface and is not emitted; a `Fatal`-grade finding would emit under both
/// (none are currently produced).
pub(crate) fn emit_offset(
    cx: &Cx,
    span: Span,
    id: &'static str,
    grade: OffsetGrade,
    message: String,
    out: &mut Vec<Diagnostic>,
) {
    if grade == OffsetGrade::Warning && !cx.warning_handler_abort {
        return;
    }
    let pos = cx.tree().position(span.start);
    out.push(Diagnostic { id, path: cx.path().to_owned(), line: pos.line, column: pos.column, message, facet: None, fix: None });
}
