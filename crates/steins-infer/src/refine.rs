//! Guard refinement (ADR-0031 stage 1 → stage 2): seeding a parameter's fact from
//! its declaration, collecting the refinements a guard implies on each branch,
//! class narrowing, contract-lane subtraction, and the primitive refinements
//! (`clear_null`, `intersect_int`, `truthy_narrow`, …) they apply.

use std::collections::HashMap;

use steins_contract::{ContractTy, normalize};
use steins_domain::{Base, Certainty, Fact, IntRange, PhpStr, Refinement, StrPreds, Val};
use steins_phpdoc::{Type as PType, TagKind, scan_docblock};
use steins_syntax::{
    ArgValue, CallExpr, Callee, CmpOp, CondExpr, CondOperand, NameRef, Param, ScalarType,
    StaticClass, Stmt, TypeMember,
};

use crate::cond::eval_cmp;
use crate::contract::{ProjectIsa, TemplateShadow, neutralize_templates, parse_tag_type};
use crate::cx::Cx;
use crate::env::{ContractArm, Known, Store, Stratum, absorb_contract_arms, val_of};
use crate::return_arms::native_arms;
use crate::walk::WalkCx;

// ---------------------------------------------------------------------------
// Guard refinement (ADR-0031 stage 1 -> stage 2 negative facts). A guard narrows
// a variable's fact on the branch where it holds. Stage 1's positive `$x === v`
// binds a Singleton; stage 2 adds the negative facts: `!== null` clears
// nullability, `!== v` removes a member (or, for `!== ''`, adds NON_EMPTY),
// ordering guards intersect an int interval, truthiness adds NON_FALSY/clears
// null. Instanceof binds nothing — membership is not exactness.
//
// A refinement that would empty a fact (int-range intersection with no overlap,
// reachable only across `&&` of contradictory guards) drops the var's fact
// rather than signalling branch-death: the decided-guard verdict already prunes
// truly-dead branches, so dropping-to-no-fact is the simpler fallback here.
// ---------------------------------------------------------------------------

/// The fact a parameter's native type guarantees at runtime (Feature B), or
/// `None` when nothing representable can be seeded. Only a single scalar type
/// (optionally nullable) seeds a `General{base, nullable}` fact; unions and
/// bool-literal members (`string|false`) have no clean single-`Fact` form and are
/// skipped. By-ref params are never seeded; variadic params are skipped.
pub(crate) fn seed_fact(p: &Param) -> Option<Fact> {
    if p.by_ref || p.variadic {
        return None;
    }
    let ty = p.ty.as_ref()?;
    let [TypeMember::Scalar(scalar)] = ty.members.as_slice() else { return None };
    let base = match scalar {
        ScalarType::Int => Base::Int,
        ScalarType::Float => Base::Float,
        ScalarType::String => Base::String,
        ScalarType::Bool => Base::Bool,
    };
    // A `= null` default makes even a non-`?T` param implicitly nullable.
    let nullable = ty.nullable || p.has_null_default;
    Some(Fact::General { base, nullable })
}

/// Seed a parameter's contract-fact arm lane (ADR-0052 §9): the native member list
/// at [`Stratum::Verified`], refined by the `@param` phpdoc envelope at
/// [`Stratum::Asserted`] (ADR-0037 trust order). Returns the declaration-ordered
/// arm list, or `None` when neither source yields a representable arm.
///
/// Native only (`int|string $x`, no `@param`): scalar/instance/null arms, each
/// `Verified`. Phpdoc present (`object $value` + `@param User|Guest`): the
/// phpdoc's arms are the declared contract; an arm the native type also proves
/// stays `Verified`, every other (refined/added) arm is `Asserted`.
///
/// By-ref/variadic params are skipped, matching [`seed_fact`].
///
/// `resolve_class` namespace-resolves a phpdoc class arm's name to its normalized
/// project FQN, the same resolution [`Cx::resolve_pclass`] performs elsewhere.
/// Without it, a `@param User|Guest` under a `namespace` would seed unqualified
/// names while the `instanceof` subtrahend carries fully-qualified ones, so
/// subtraction would silently keep both arms. Native `Instance` arms are already
/// FQN-resolved at lowering, so only phpdoc arms are re-resolved.
pub(crate) fn seed_contract_arms(
    p: &Param,
    phpdoc: Option<&PType>,
    resolve_class: &dyn Fn(&str) -> String,
) -> Option<Vec<ContractArm>> {
    if p.by_ref || p.variadic {
        return None;
    }
    let native: Vec<ContractTy> = p.ty.as_ref().map(native_arms).unwrap_or_default();
    refine_contract_arms(&native, phpdoc, resolve_class)
}

/// Replace every enum-typed class arm with the enum's declared cases, one arm
/// each (issue #429) — the step that makes a declared `Suit $s` a **finite**
/// domain instead of a class membership.
///
/// Runs on the seeded lane, after [`refine_contract_arms`] has settled trust:
///
/// * only a `Verified` arm expands. The case set is what the engine enforces at
///   the boundary, so it is a Verified fact and stays one; an `Asserted` arm is a
///   docblock's claim about which values arrive, and expanding it would let a
///   refinement nobody checks mint a finite domain the exhaustiveness question
///   then reads as complete (ADR-0037's trust order, ADR-0052 §5).
/// * only where [`Cx::enum_case_names`] can state the WHOLE set. Everywhere else
///   the arm is left exactly as it was — a `Class` membership arm, which no
///   identity guard can subtract to empty, so nothing downstream can claim an
///   exhaustion the declaration never proved.
///
/// A no-op for every non-enum lane, which is nearly all of them.
pub(crate) fn expand_enum_case_arms(cx: &Cx, arms: &mut Vec<ContractArm>) {
    if !arms.iter().any(|a| matches!(a.ty, ContractTy::Class(_)) && a.stratum == Stratum::Verified)
    {
        return;
    }
    let mut out: Vec<ContractArm> = Vec::with_capacity(arms.len());
    for arm in arms.drain(..) {
        let expanded = match (&arm.ty, arm.stratum) {
            (ContractTy::Class(fqn), Stratum::Verified) => {
                cx.enum_case_names(fqn).map(|cases| (fqn.clone(), cases))
            }
            _ => None,
        };
        match expanded {
            Some((fqn, cases)) => out.extend(cases.into_iter().map(|case| ContractArm {
                ty: ContractTy::EnumCase { enum_fqn: fqn.clone(), case },
                stratum: Stratum::Verified,
            })),
            None => out.push(arm),
        }
    }
    *arms = out;
}

/// The value-lane seed a seeded contract-arm lane contributes (ADR-0062 S3): the
/// canonical [`Fact::Shape`] of a lane whose array vocabulary is ONE arm, plus a
/// `null` arm's nullability (A-G2, a side-flag, never a field inside the shape).
///
/// `None` in every other case: two or more array arms (a shape∪shape union stays
/// in the arm lane until a guard subtracts it to one, A-G3 — joining would lose
/// the discrimination the arms carry); a mixed union (`array{…}|string`,
/// un-facted like scalars, A-G2); no array arm at all.
///
/// The lowering is [`steins_contract::to_shape_fact`] — the one lowering, shared
/// with the speller's `is_list` computation, so a seeded fact and its spelled arm
/// can never disagree.
pub(crate) fn seed_shape_fact(arms: &[ContractArm]) -> Option<Fact> {
    let mut shape: Option<steins_domain::ShapeFact> = None;
    let mut nullable = false;
    for arm in arms {
        if matches!(arm.ty, ContractTy::Null) {
            nullable = true;
            continue;
        }
        let lowered = steins_contract::to_shape_fact(&arm.ty)?;
        if shape.is_some() {
            return None;
        }
        shape = Some(lowered);
    }
    Some(Fact::Shape { shape: Box::new(shape?), nullable })
}

/// The scalar mirror of [`seed_shape_fact`] (issue #242): the value-lane fact a
/// declared arm list contributes when it REFINES the native scalar envelope the
/// entry pass already seeded.
///
/// Exists because the two lanes seed in the wrong order for scalars: the native
/// pass ([`seed_fact`]) plants `Fact::General { base }` before the ADR-0052 §9
/// arm lane is built, and the value lane outranks the arm lane wherever a fact is
/// read (ADR-0037 trust order). So `@param non-empty-string $s` on a `string $s`
/// reached its arm lane intact and was then shadowed by the coarser `string`
/// already in front of it — exactly the asymmetry issue #242 measured (arrays
/// never hit this, since [`seed_fact`] only matches a scalar member).
///
/// Admitted only under conditions that make it a strict narrowing of the native
/// fact it replaces: the lane lowers ([`steins_contract::to_fact`]) to a
/// `Fact::Refined` (a `@param int $x` on `float $x` lowers to `Fact::General`
/// instead, refused, since PHP's coercion makes it hold a float regardless); the
/// refined base equals the native seed's base; and nullability only ever
/// shrinks — an implicitly-nullable parameter (`string $s = null`) keeps its
/// `null` since [`native_arms`] reads `ty.nullable` alone.
///
/// Stratum comes from the arms: a refinement the native type doesn't itself
/// prove is `Asserted` and stays `Asserted` (ADR-0037; ADR-0052 N2), exactly as
/// the shape seed's A-G9 corollary pins for arrays.
pub(crate) fn seed_refined_scalar_fact(
    p: &Param,
    native: &Fact,
    arms: &[ContractArm],
) -> Option<(Fact, Stratum)> {
    let Fact::General { base, nullable } = native else { return None };
    let lowered = steins_contract::to_fact(&ContractTy::Union(
        arms.iter().map(|a| a.ty.clone()).collect(),
    ))?;
    let Fact::Refined { base: rbase, nullable: rnullable, .. } = &lowered else { return None };
    if rbase != base || (*rnullable && !*nullable) || (!*rnullable && p.has_null_default) {
        return None;
    }
    let stratum = if arms.iter().any(|a| a.stratum == Stratum::Asserted) {
        Stratum::Asserted
    } else {
        Stratum::Verified
    };
    Some((lowered, stratum))
}

/// Statement-level inline `/** @var T $x */` casts (ADR-0073): the docblock
/// immediately preceding a trace statement re-declares the named variable's type
/// from that statement on — PHPStan's inline-`@var` reading. Lowering mirrors
/// `@param` entry seeding ([`seed_contract_arms`]) minus the native envelope:
/// every arm seeds `Asserted` (ADR-0037), and a lane collapsed to one array arm
/// seeds the value lane with its canonical shape fact (ADR-0062 S3).
///
/// A cast, not a refinement: every carrier of the old value dies first
/// (`env.remove` + [`Store::unbind`]), since the tag re-declares rather than
/// narrows. Losing a proven value to an assertion only ever silences (proof-layer
/// consumers take `Verified` facts, which the cast never mints), so the trade is
/// PHPStan parity at zero FP cost.
///
/// Guards: property targets (`@var T $this->p`, bare `$this`) never cast, since
/// casting the receiver could manufacture declared-receiver findings (S6);
/// `@template` names shadow as in declaration envelopes (issue #5); an
/// unparseable/unlowerable type casts nothing (ADR-0029: a missing envelope only
/// silences); a prefixed `@phpstan-var`/`@psalm-var` displaces the plain `@var`
/// for the same variable (ADR-0029 precedence).
///
/// Plain per-scope pass only: a binding descent carries call-site-proven values,
/// which outrank a docblock assertion.
pub(crate) fn apply_inline_var_casts(
    w: &WalkCx,
    stmt: &Stmt,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
) {
    let cx = w.cx;
    let Some(doc) = cx.tree().stmt_docblock(stmt.span.start) else { return };
    let tags = scan_docblock(&doc.text);
    // Computed on the first acting tag only — most adjacent docblocks carry none.
    let mut shadow: Option<TemplateShadow> = None;
    for tag in &tags {
        if !matches!(tag.kind, TagKind::Var) || tag.property_target {
            continue;
        }
        let Some(var) = &tag.var_name else { continue };
        let name = var.trim_start_matches('$');
        if name.is_empty() || name == "this" {
            continue;
        }
        if !tag.prefixed
            && tags.iter().any(|t| {
                matches!(t.kind, TagKind::Var) && t.prefixed && t.var_name == tag.var_name
            })
        {
            continue;
        }
        let Some(mut pt) = parse_tag_type(&tag.type_text) else { continue };
        let sh = shadow.get_or_insert_with(|| cx.scope_template_shadow(w.scope));
        neutralize_templates(&mut pt, sh);
        // Class arms resolve in the statement's namespace context, matching the
        // FQNs the `instanceof` subtrahend and S6's `find_class` carry.
        let resolve = |n: &str| {
            cx.resolve_pclass(cx.cur, stmt.span.start, n).trim_start_matches('\\').to_ascii_lowercase()
        };
        let Some(arms) = refine_contract_arms(&[], Some(&pt), &resolve) else { continue };
        if arms.is_empty() {
            continue;
        }
        env.remove(name);
        store.unbind(name);
        if let Some(fact) = seed_shape_fact(&arms) {
            let line = cx.tree().position(stmt.span.start).line;
            env.insert(
                name.to_owned(),
                // ALWAYS `Asserted` — the same A-G9 corollary the entry seeding
                // pins: shape-derived facts never feed proof-layer findings.
                Known::value_strat(
                    fact,
                    line,
                    Some("declared array shape".to_owned()),
                    Stratum::Asserted,
                ),
            );
        }
        store.contract.insert(name.to_owned(), arms);
    }
}

/// The shared core of declared-contract arm refinement (ADR-0052 §9), used for both
/// the `@param` entry-state seeding ([`seed_contract_arms`]) and the declared-return
/// call-site seeding ([`call_return_arms`]): the runtime-guaranteed native member
/// list refined by a declared phpdoc envelope, under the trust order's subset
/// discipline.
///
/// With no phpdoc envelope, each native member is a `Verified` arm. With one, each
/// lowered phpdoc arm refines within the native envelope: a phpdoc arm the native
/// base provably cannot cover is a contradiction (`string` under `int`) and seeds
/// nothing. An arm covering a native member exactly is `Verified`; a strict
/// refinement within it is `Asserted`. An undecidable is-a is NOT a contradiction —
/// the arm stays `Asserted`, the FP-safe side. Where no native type exists, every
/// phpdoc arm seeds `Asserted`.
/// Resolve every class name in a contract arm against the declaring namespace.
///
/// One level of intersection is walked and nothing else: `Foo&Bar` is the shape a
/// declared conjunction has, and an arm list is already union-flattened
/// ([`flatten_arms`]) by this point. Non-class intersection members (array, scalar)
/// are left as-is.
///
/// [`call_return_arms`]: crate::return_arms::call_return_arms
fn resolve_class_arms(ty: ContractTy, resolve_class: &dyn Fn(&str) -> String) -> ContractTy {
    match ty {
        ContractTy::Class(n) => ContractTy::Class(resolve_class(&n)),
        ContractTy::Inter(members) => ContractTy::Inter(
            members
                .into_iter()
                .map(|m| match m {
                    ContractTy::Class(n) => ContractTy::Class(resolve_class(&n)),
                    other => other,
                })
                .collect(),
        ),
        other => other,
    }
}

pub(crate) fn refine_contract_arms(
    native: &[ContractTy],
    phpdoc: Option<&PType>,
    resolve_class: &dyn Fn(&str) -> String,
) -> Option<Vec<ContractArm>> {
    match phpdoc {
        Some(pt) => {
            refine_declared_arms(native, flatten_arms(steins_contract::lower(pt)), resolve_class)
        }
        None => {
            let out: Vec<ContractArm> =
                native.iter().cloned().map(|ty| ContractArm { ty, stratum: Stratum::Verified }).collect();
            (!out.is_empty()).then_some(out)
        }
    }
}

/// [`refine_contract_arms`]' declared-side body, over an already-flattened arm
/// list rather than a parsed docblock.
///
/// Split out for the ADR-0069 builtin floor (issue #79), which reaches the same law
/// from a catalog type string: lowered by `lower_str`, flattened by
/// [`flatten_arms`], refined against an empty native list — so a builtin's declared
/// return and a project function's travel one lowering path, and the builtin's
/// arms come out `Asserted` for the same structural reason a phpdoc arm over an
/// untyped signature does.
pub(crate) fn refine_declared_arms(
    native: &[ContractTy],
    declared: Vec<ContractTy>,
    resolve_class: &dyn Fn(&str) -> String,
) -> Option<Vec<ContractArm>> {
    let out: Vec<ContractArm> = declared
        .into_iter()
        .filter_map(|ty| {
            // Resolve a class arm against the declaring namespace to align with the
            // native member list's FQNs. An intersection's members are class arms
            // too (`@param Foo&Bar`, issue #238) and need the same resolution.
            let ty = resolve_class_arms(ty, resolve_class);
            if !native.is_empty() {
                let covered = native
                    .iter()
                    .fold(Certainty::No, |acc, n| acc.or(normalize::subsumes(n, &ty)));
                if covered.is_no() {
                    return None;
                }
            }
            let stratum = if native.iter().any(|n| normalize::arm_eq(n, &ty)) {
                Stratum::Verified
            } else {
                Stratum::Asserted
            };
            Some(ContractArm { ty, stratum })
        })
        .collect();
    // Canonicalize before it enters the lane: a mined row like `strpos`'s
    // `positive-int|0|false` carries the two-armed spelling of one interval, and
    // every later reader should see the denotation once (issue #90).
    let mut out = out;
    absorb_contract_arms(&mut out);
    (!out.is_empty()).then_some(out)
}

/// Flatten a lowered contract into a top-level arm list, dissolving nested unions
/// (a declared `User|Guest|null` lowers to a `Union`; each member is one arm). A
/// non-union lowers to a single arm.
///
/// **The value-lane boundary for `unset`** (ADR-0087): the possibly-undefined
/// pseudo-type carries a spelling but no value, so it is dropped here — the one
/// place every declared arm list is built. `@var \DateTime|unset $x` therefore
/// yields *structurally* the arm list of `@var \DateTime $x`, and no downstream
/// reader learns the variant exists. A bare `@var unset $x` yields an empty list,
/// which every caller already reads as "no envelope, seed nothing" (ADR-0029).
pub(crate) fn flatten_arms(cty: ContractTy) -> Vec<ContractTy> {
    match cty {
        ContractTy::Union(members) => members.into_iter().flat_map(flatten_arms).collect(),
        // Dropped at every depth, since a nested union recurses through here.
        other if other.is_unset() => Vec::new(),
        other => vec![other],
    }
}

/// One narrowing a guard establishes for a variable on a given branch.
pub(crate) enum Refine {
    /// `$x === v` (then) — narrow to exactly this value.
    Exact(String, Val),
    /// `$x !== null` (then) / `$x === null` (else) — drop nullability: clear the
    /// abstract `nullable` flag, or remove the `null` member of a finite fact.
    NotNull(String),
    /// `$x !== v` (non-null, then) — remove `v` from a finite fact; for a
    /// String-based abstract fact and `v == ""`, add `NON_EMPTY` instead.
    Exclude(String, Val),
    /// `$x > k` &c. — intersect an Int-based abstract fact with this interval.
    IntRange(String, IntRange),
    /// `if ($x)` (then) — truthiness: clear nullability and, for a String-based
    /// fact, add `NON_FALSY` (a truthy string is neither `""` nor `"0"`).
    Truthy(String),
}

/// The refinements that hold when `cond` is TRUE (the then-branch).
pub(crate) fn then_refinements(cond: &CondExpr, php_minor: Option<(u16, u16)>) -> Vec<Refine> {
    let mut out = Vec::new();
    collect_refine(cond, true, &mut out, php_minor);
    out
}

/// The refinements that hold when `cond` is FALSE (the else-branch).
pub(crate) fn else_refinements(cond: &CondExpr, php_minor: Option<(u16, u16)>) -> Vec<Refine> {
    let mut out = Vec::new();
    collect_refine(cond, false, &mut out, php_minor);
    out
}

/// Guard calls whose `@phpstan-assert-if-true`/`-if-false` envelope applies on the
/// given branch polarity, in source order — same And-then/Or-else distribution as
/// [`collect_refine`], so a call nested in a threaded `&&`/`||` reaches its
/// consumption point (ADR-0052 §6). The paired `bool` is whether the call
/// returned `true` on this branch (flips under `Not`), selecting the
/// [`AssertKind`] polarity.
///
/// [`AssertKind`]: steins_phpdoc::AssertKind
pub(crate) fn collect_guard_calls<'a>(cond: &'a CondExpr, then: bool, out: &mut Vec<(&'a CallExpr, bool)>) {
    match cond {
        CondExpr::Call { call, .. } => out.push((call, then)),
        CondExpr::Not(c) => collect_guard_calls(c, !then, out),
        CondExpr::And(a, b) if then => {
            collect_guard_calls(a, true, out);
            collect_guard_calls(b, true, out);
        }
        CondExpr::Or(a, b) if !then => {
            collect_guard_calls(a, false, out);
            collect_guard_calls(b, false, out);
        }
        _ => {}
    }
}

/// The calls this branch proves returned a truthy value — the witness the
/// out-parameter seed (ADR-0077 §3.2) consumes, in source order.
///
/// Its `&&`/`||`/`!` distribution matches [`collect_guard_calls`]'; what it adds
/// is the compared call: `preg_match($re, $s, $m) === 1` proves the result is
/// `1`, as truthy as a bare `preg_match(…)` guard.
///
/// Kept apart from [`collect_guard_calls`] deliberately: `@phpstan-assert-if-true`
/// is stated about the callee *returning `true`*, and `f($x) === 1` doesn't
/// witness that (`1` is truthy but isn't `true`). Feeding compared calls into the
/// assert consumption would silently widen every envelope in the project.
pub(crate) fn collect_truthy_calls<'a>(
    cond: &'a CondExpr,
    then: bool,
    php_minor: Option<(u16, u16)>,
    out: &mut Vec<&'a CallExpr>,
) {
    match cond {
        CondExpr::Call { call, .. } if then => out.push(call),
        CondExpr::Cmp { op, lhs, rhs } => {
            out.extend(cmp_truthy_witness(*op, lhs, rhs, then, php_minor));
        }
        CondExpr::Not(c) => collect_truthy_calls(c, !then, php_minor, out),
        CondExpr::And(a, b) if then => {
            collect_truthy_calls(a, true, php_minor, out);
            collect_truthy_calls(b, true, php_minor, out);
        }
        CondExpr::Or(a, b) if !then => {
            collect_truthy_calls(a, false, php_minor, out);
            collect_truthy_calls(b, false, php_minor, out);
        }
        _ => {}
    }
}

/// The call a comparison proves truthy on this branch polarity, or `None`.
///
/// Not a table of blessed shapes — the claim itself, checked: every value that
/// satisfies the comparison must be truthy, i.e. no falsy value may satisfy it.
/// PHP's falsy values are a finite set ([`FALSY_LITERALS`]), so the question is
/// answered by running each through [`eval_cmp`] rather than reasoning in prose.
///
/// `f() === 1` and `f() == 1` prove truthy on the then branch (nothing falsy is
/// identical/loosely equal to `1`); `f() !== 1`/`f() != 1` prove it on the else
/// branch; `f() === 0`, `f() !== false`, `f() === ''` prove nothing either way.
fn cmp_truthy_witness<'a>(
    op: CmpOp,
    lhs: &'a CondOperand,
    rhs: &'a CondOperand,
    then: bool,
    php_minor: Option<(u16, u16)>,
) -> Option<&'a CallExpr> {
    let (call, lit) = match (lhs, rhs) {
        (CondOperand::Other { call, .. }, CondOperand::Literal(v))
        | (CondOperand::Literal(v), CondOperand::Other { call, .. }) => (call.as_deref()?, v),
        _ => return None,
    };
    // On the then branch the comparison held, so a falsy result must be one the
    // comparison *rejects*; on the else branch it failed, so a falsy result must
    // be one it *accepts*. Anything undecidable (`Maybe`) refuses, silently.
    let excluded = if then { Certainty::No } else { Certainty::Yes };
    falsy_literals()
        .iter()
        .all(|f| {
            eval_cmp(op, std::slice::from_ref(f), std::slice::from_ref(lit), php_minor) == excluded
        })
        .then_some(call)
}

/// Every falsy value in PHP, as a literal. The list is exhaustive by the language
/// definition — `null`, `false`, `0`, `0.0`, `''`, `'0'` and the empty array —
/// and [`php_truthy`] is its other half (each of these is a value it answers
/// `Some(false)` for, and nothing else is).
///
/// [`php_truthy`]: crate::compare::php_truthy
fn falsy_literals() -> [ArgValue; 7] {
    [
        ArgValue::Null,
        ArgValue::Bool(false),
        ArgValue::Int(0),
        ArgValue::Float(0.0),
        ArgValue::Str(PhpStr::new()),
        ArgValue::Str("0".into()),
        ArgValue::Array(Vec::new()),
    ]
}

/// Every retained guard call anywhere in the condition (both polarities), for the
/// position-sequenced escape/sweep and by-ref invalidation that apply on *every*
/// resulting path (a call in either operand may have executed on the excluded path).
pub(crate) fn collect_guard_calls_any(cond: &CondExpr) -> Vec<&CallExpr> {
    let mut out = Vec::new();
    collect_all_calls(cond, &mut out);
    out
}

fn collect_all_calls<'a>(cond: &'a CondExpr, out: &mut Vec<&'a CallExpr>) {
    match cond {
        CondExpr::Call { call, .. } => out.push(call),
        // A call in a comparison/`instanceof` operand escapes its arguments and
        // sweeps their props exactly as one in guard position does (issue #158 —
        // the same traversal gap that lost the by-ref invalidation).
        CondExpr::Cmp { lhs, rhs, .. } => {
            out.extend(operand_call(lhs));
            out.extend(operand_call(rhs));
        }
        CondExpr::Instanceof { operand, .. } => out.extend(operand_call(operand)),
        CondExpr::Not(c) => collect_all_calls(c, out),
        CondExpr::And(a, b) | CondExpr::Or(a, b) => {
            collect_all_calls(a, out);
            collect_all_calls(b, out);
        }
        _ => {}
    }
}

/// The resolvable call an operand **is**, when it is one.
pub(crate) fn operand_call(operand: &CondOperand) -> Option<&CallExpr> {
    match operand {
        CondOperand::Other { call, .. } => call.as_deref(),
        _ => None,
    }
}

/// Collect the refinements a condition implies on the given polarity (`then` =
/// true-path, `!then` = false-path). Negation flips polarity; `&&` distributes on
/// the true-path, `||` on the false-path (De Morgan).
pub(crate) fn collect_refine(cond: &CondExpr, then: bool, out: &mut Vec<Refine>, php_minor: Option<(u16, u16)>) {
    match cond {
        CondExpr::Cmp { op, lhs, rhs } => {
            collect_cmp_refine(*op, lhs, rhs, then, out, php_minor);
        }
        CondExpr::Truthy(op) => {
            // Only the true-path of a bare truthiness test refines (the false-path
            // — "falsy" — is not cleanly representable: `""`, `"0"`, `0`, null …).
            if then && let CondOperand::Var(v) = op {
                out.push(Refine::Truthy(v.clone()));
            }
        }
        CondExpr::Not(c) => collect_refine(c, !then, out, php_minor),
        CondExpr::And(a, b) if then => {
            collect_refine(a, true, out, php_minor);
            collect_refine(b, true, out, php_minor);
        }
        CondExpr::Or(a, b) if !then => {
            collect_refine(a, false, out, php_minor);
            collect_refine(b, false, out, php_minor);
        }
        _ => {}
    }
}

/// Refinements from a comparison guard on a given polarity.
fn collect_cmp_refine(
    op: CmpOp,
    lhs: &CondOperand,
    rhs: &CondOperand,
    then: bool,
    out: &mut Vec<Refine>,
    php_minor: Option<(u16, u16)>,
) {
    // Identity/equality guards over a (var, literal) pair.
    if let Some((v, val)) = var_literal(lhs, rhs) {
        // The *effective* operator on this branch: `===`/`!==` flip under `!then`.
        let identical = match (op, then) {
            (CmpOp::Identical, true) | (CmpOp::NotIdentical, false) => Some(true),
            (CmpOp::NotIdentical, true) | (CmpOp::Identical, false) => Some(false),
            _ => None,
        };
        if let Some(identical) = identical
            && let Some(vv) = val_of(&val, php_minor)
        {
            match (identical, &vv) {
                (true, _) => out.push(Refine::Exact(v, vv)),
                (false, Val::Null) => out.push(Refine::NotNull(v)),
                (false, _) => out.push(Refine::Exclude(v, vv)),
            }
            return;
        }
        // The loose null pair, **one direction only** (issue #391). `$x == null` is
        // true for `0`, `''`, `'0'` and `[]` as well, so the branch where it holds
        // proves nothing about nullness — but `null == null` is true, so the branch
        // where it *fails* proves `$x` is not null. `if ($x == null) { return; }`
        // is the idiom this exists for; it is a `NotNull` on the fall-through and
        // nothing anywhere else, which is why it is not a `Refine::Exclude`.
        if matches!((op, then), (CmpOp::Loose, false) | (CmpOp::NotLoose, true))
            && matches!(val_of(&val, php_minor), Some(Val::Null))
        {
            out.push(Refine::NotNull(v));
            return;
        }
    }
    // Ordering guards over a (var, int-literal) pair → an interval intersection.
    if let Some((v, k, var_on_left)) = var_int_literal(lhs, rhs) {
        // Normalize so the operator reads `var <op> k`.
        let eff_op = if var_on_left { op } else { flip_ordering(op) };
        // On the false-path the guard is negated.
        let branch_op = if then { eff_op } else { negate_ordering(eff_op) };
        if let Some(range) = ordering_range(branch_op, k) {
            out.push(Refine::IntRange(v, range));
        }
    }
}

/// Reconstruct the value-position [`ArgValue`] a guard condition's call operand
/// denotes (issue #421) — the same shape [`ArgValue::Call`]/[`ArgValue::MethodCall`]
/// carry, built from the trace IR's own [`CallExpr`] rather than re-parsed, so a
/// guard and an argument written identically can never fail to compare equal.
///
/// `None` for a spread call (`has_spread`): neither `ArgValue` variant has a slot
/// for one, and a spread's cardinality is runtime-decided, so "the same call"
/// is not even a claim this can state. A **free function** additionally requires
/// `positional_only` — `ArgValue::Call` carries no named-argument slot at all,
/// unlike its method twin.
fn guard_call_as_value(call: &CallExpr) -> Option<ArgValue> {
    if call.has_spread {
        return None;
    }
    let args: Vec<ArgValue> = call.args.iter().map(|a| a.value.clone()).collect();
    match &call.receiver {
        Callee::Function(name) => {
            if call.positional_only { Some(ArgValue::Call(name.clone(), args)) } else { None }
        }
        Callee::Method { .. } | Callee::Static { .. } => Some(ArgValue::MethodCall {
            callee: call.receiver.clone(),
            args,
            named: call.named_args.clone(),
        }),
        Callee::Construct { .. } | Callee::DynamicVar(_) | Callee::Dynamic => None,
    }
}

/// Collect the same-expression call guards (issue #421) a condition establishes
/// on the given polarity — the possibly-grade argument pair's own decline
/// premise, for the one operand shape [`collect_cmp_refine`] (`Var`-keyed) and
/// [`collect_shape_guards`] (`Offset`-keyed) cannot narrow: a call.
///
/// Only the **negative**-equality reading against `null`/`false` fires (`$e !==
/// null` true, or `$e === null` false — the same "not null" pair
/// [`collect_cmp_refine`] reads for a `Var`, minus the loose one-direction
/// carve-out: a loose `$e != null` is also true for `0`/`''`/`'0'`/`[]`, which
/// says nothing about a call's own return, so only the strict operators qualify),
/// plus a bare truthy `if ($e)` on its true branch. The positive reading (`$e
/// === null` proven) says nothing about representability — the value this
/// judgment would have read is exactly the one arm it never reaches — so it
/// records nothing.
///
/// Same De Morgan distribution as [`collect_instanceof`]: negation flips
/// polarity, `&&` distributes on the true-path, `||` on the false-path.
///
/// [`collect_shape_guards`]: crate::shapes::collect_shape_guards
pub(crate) fn collect_same_expr_call_guards(cond: &CondExpr, then: bool, out: &mut Vec<ArgValue>) {
    match cond {
        CondExpr::Cmp { op: op @ (CmpOp::Identical | CmpOp::NotIdentical), lhs, rhs } => {
            let identical = match (op, then) {
                (CmpOp::Identical, true) | (CmpOp::NotIdentical, false) => true,
                (CmpOp::NotIdentical, true) | (CmpOp::Identical, false) => false,
                _ => unreachable!("op is Identical or NotIdentical by the outer match"),
            };
            if identical {
                return;
            }
            for (operand, other) in [(lhs, rhs), (rhs, lhs)] {
                let CondOperand::Other { call: Some(call), .. } = operand else { continue };
                if matches!(other, CondOperand::Literal(ArgValue::Null | ArgValue::Bool(false)))
                    && let Some(v) = guard_call_as_value(call)
                {
                    out.push(v);
                }
            }
        }
        CondExpr::Truthy(CondOperand::Other { call: Some(call), .. }) if then => {
            if let Some(v) = guard_call_as_value(call) {
                out.push(v);
            }
        }
        CondExpr::Not(c) => collect_same_expr_call_guards(c, !then, out),
        CondExpr::And(a, b) if then => {
            collect_same_expr_call_guards(a, true, out);
            collect_same_expr_call_guards(b, true, out);
        }
        CondExpr::Or(a, b) if !then => {
            collect_same_expr_call_guards(a, false, out);
            collect_same_expr_call_guards(b, false, out);
        }
        _ => {}
    }
}

/// Collect the `instanceof` guards a condition establishes on the given polarity
/// (`then` = true-path, `!then` = false-path), with the branch polarity per guard.
/// Negation flips polarity; `&&` distributes on the true-path, `||` on the
/// false-path — same De Morgan distribution as [`collect_refine`]. Only
/// bare-variable operands are collected (`$x->p instanceof T` is N5's concern).
fn collect_instanceof<'a>(
    cond: &'a CondExpr,
    then: bool,
    out: &mut Vec<(&'a str, &'a NameRef, bool)>,
) {
    match cond {
        CondExpr::Instanceof { operand: CondOperand::Var(v), class_ref } => {
            out.push((v.as_str(), class_ref, then));
        }
        CondExpr::Not(c) => collect_instanceof(c, !then, out),
        CondExpr::And(a, b) if then => {
            collect_instanceof(a, true, out);
            collect_instanceof(b, true, out);
        }
        CondExpr::Or(a, b) if !then => {
            collect_instanceof(a, false, out);
            collect_instanceof(b, false, out);
        }
        _ => {}
    }
}

/// The enum-case identity guards this branch proves, as
/// `(var, class ref, case name, positive)` — the [`collect_instanceof`] shape for
/// `===`/`!==` against an enum case (issue #429). `positive` is whether the branch
/// proves the variable IS that case; the `&&`/`||`/`!` distribution is
/// [`collect_refine`]'s, so a guard nested in a threaded condition still reaches
/// its consumption point (ADR-0052 §6).
///
/// **`===`/`!==` only.** Loose `==` on two enum cases is a property comparison PHP
/// decides by the cases' own `name`/`value` slots, a different question with a
/// different proof; the issue asks for identity and identity is what this reads.
fn collect_enum_identity<'a>(
    cond: &'a CondExpr,
    then: bool,
    out: &mut Vec<(&'a str, &'a StaticClass, &'a str, bool)>,
) {
    match cond {
        CondExpr::Cmp { op, lhs, rhs } => {
            let positive = match op {
                CmpOp::Identical => then,
                CmpOp::NotIdentical => !then,
                _ => return,
            };
            let (var, sc, case) = match (lhs, rhs) {
                (CondOperand::Var(v), CondOperand::ClassConst(sc, n))
                | (CondOperand::ClassConst(sc, n), CondOperand::Var(v)) => {
                    (v.as_str(), sc, n.as_str())
                }
                _ => return,
            };
            out.push((var, sc, case, positive));
        }
        CondExpr::Not(c) => collect_enum_identity(c, !then, out),
        CondExpr::And(a, b) if then => {
            collect_enum_identity(a, true, out);
            collect_enum_identity(b, true, out);
        }
        CondExpr::Or(a, b) if !then => {
            collect_enum_identity(a, false, out);
            collect_enum_identity(b, false, out);
        }
        _ => {}
    }
}

/// Apply a branch's class-fact narrowing (ADR-0052 N4) to its cloned `Store`: the
/// two NEW carriers, mutated arm-wise through the real is-a oracle. Runs beside
/// [`apply_refinements`] (which owns the value-domain `Fact` carrier).
///
/// 1. Each `instanceof T` guard subtracts from the variable's contract lane: the
///    negative branch deletes arm `M` iff `is_a(M, T) = Yes`; the positive branch
///    deletes `M` only when `M` is final/enum and `is_a(M, T) = No`
///    ([`normalize::subtract_arm`]), preserving each surviving arm's stratum. An
///    emptied lane drops to no-fact (never a death signal, §2). `oracle` threads
///    the A11 demotion into these arm-deletion queries.
/// 2. The same guard binds the `Member` fact at `Verified`: positive adds `T` to
///    `yes`, negative to `no`.
/// 3. `!== null` subtracts the `null` arm from the contract lane.
/// 4. `!== v` subtracts value `v` from the contract lane
///    ([`normalize::Subtrahend::Value`], ADR-0052 §2): an arm dies iff the literal
///    provably covers the whole arm. Strips the `false` arm of a `T|false` row
///    under `if (strpos(…) !== false)`, general over any literal. Two partial
///    deletions replace outright survival: an interval endpoint
///    (`!== 0` clips `int<0, max>` to `int<1, max>`; an interior `!== 5` leaves
///    it whole — the gap has no arm spelling) and, since issue #443, a general
///    `bool` arm's one of two literals — `!== false` narrows `bool` to `true`,
///    because `bool` has no interior point to protect and every non-covering
///    subtrahend of it is the other literal ([`normalize::ArmFate::Narrows`]).
///
/// Two neighbouring narrowings are deliberately NOT here: `Refine::Truthy` (`if
/// ($pos)` over `int|false` — truthiness kills `0`/`''` too, not a value
/// subtraction, PHPStan's classic `strpos` footgun needing its own subtrahend);
/// and keep-only narrowing on the positive branch (`if ($x === false)` — the
/// value lane's `Refine::Exact` already owns this, and the arm lane is a
/// subtraction carrier by construction).
pub(crate) fn apply_class_narrowing(w: &WalkCx, cond: &CondExpr, then: bool, store: &mut Store) {
    let oracle = ProjectIsa { cx: w.cx, demote_catalog: w.cx.a11_demote_catalog() };

    let mut ins = Vec::new();
    collect_instanceof(cond, then, &mut ins);
    for (var, class_ref, positive) in ins {
        let norm = w.cx.class_fqn(class_ref).trim_start_matches('\\').to_ascii_lowercase();
        // (1) Contract-arm subtraction (both polarities), strata preserved.
        subtract_contract_lane(
            store,
            var,
            &normalize::Subtrahend::Class { fqn: norm.clone(), polarity: positive },
            &oracle,
        );
        // (2) Member binding: positive → `yes`, negative → `no`.
        let m = store.members.entry(var.to_owned()).or_default();
        let bucket = if positive { &mut m.yes } else { &mut m.no };
        if !bucket.iter().any(|c| c.eq_ignore_ascii_case(&norm)) {
            bucket.push(norm);
        }
    }

    // (3)/(4) `!== null` and `!== v` on this branch subtract from the contract
    // lane. `collect_refine` already carries the branch polarity, so both spellings
    // arrive as `Exclude`. `Refine::Exact`, `Truthy`, `IntRange` are the value
    // lane's — see the refusals above.
    let mut refs = Vec::new();
    collect_refine(cond, then, &mut refs, w.cx.php_minor);
    for r in &refs {
        match r {
            Refine::NotNull(var) => {
                subtract_contract_lane(store, var, &normalize::Subtrahend::Null, &oracle);
            }
            Refine::Exclude(var, v) => {
                subtract_contract_lane(
                    store,
                    var,
                    &normalize::Subtrahend::Value(v.clone()),
                    &oracle,
                );
            }
            _ => {}
        }
    }

    // (5) `$s === Enum::Case` / `!== Enum::Case` subtracts from the enum-case arm
    // lane (issue #429), the one identity guard the value lane cannot own: an enum
    // case is an object and has no `Val`. Both polarities are subtractions — the
    // true branch removes every OTHER case — so the lane keeps the shape ADR-0052
    // §2 gave it. A case whose enum the absence discipline could not complete
    // resolves to nothing here and subtracts nothing.
    let mut ids = Vec::new();
    collect_enum_identity(cond, then, &mut ids);
    for (var, sc, case, positive) in ids {
        let Some(enum_fqn) = w.cx.resolve_enum_case(sc, case, w.enclosing_class) else {
            continue;
        };
        subtract_contract_lane(
            store,
            var,
            &normalize::Subtrahend::EnumCase {
                enum_fqn,
                case: case.to_owned(),
                polarity: positive,
            },
            &oracle,
        );
    }
}

/// Subtract `sub` from `var`'s contract lane in `store`, arm-wise, preserving each
/// surviving arm's stratum ([`normalize::subtract_arm`] applied to the stratified
/// lane — a partial deletion, an interval arm clipped at its endpoint, keeps the
/// stratum of the arm it shrinks); an emptied lane drops to no-fact. Marks
/// [`Store::narrowed`] whenever an arm actually died or shrank (issue #428) — a
/// `sub` every arm survives unchanged (the guard shape this subtrahend can't
/// touch, e.g. an enum-case/bool-literal exclusion against a bare class/bool arm,
/// pre-#429) leaves the mark unset.
///
/// Returns whether the subtraction **landed** — the same question the mark answers,
/// handed back so a caller subtracting a whole set of conditions at once can tell a
/// residue that every condition shaped from one that some condition never touched
/// ([`subtract_no_match_path`]).
///
/// [`subtract_no_match_path`]: crate::branch::subtract_no_match_path
pub(crate) fn subtract_contract_lane(
    store: &mut Store,
    var: &str,
    sub: &normalize::Subtrahend,
    oracle: &dyn normalize::IsaOracle,
) -> bool {
    let mut landed = false;
    if let Some(arms) = store.contract.get_mut(var) {
        let mut changed = false;
        // Whether the lane, before this subtraction, was held on Verified premises
        // alone — the question the emptied-lane rule below asks (issue #429).
        let all_verified = arms.iter().all(|a| a.stratum == Stratum::Verified);
        arms.retain_mut(|a| match normalize::subtract_arm(sub, &a.ty, oracle) {
            normalize::ArmFate::Survives => true,
            normalize::ArmFate::Dies => {
                changed = true;
                false
            }
            normalize::ArmFate::Narrows(narrowed) => {
                a.ty = narrowed;
                changed = true;
                true
            }
        });
        // The emptied carrier (ADR-0052 §2). An all-`Verified` lane subtracted to
        // nothing is **kept, empty**, so [`Store::contract_emptied`] can read it:
        // every arm was a runtime-enforced alternative and every subtrahend that
        // reaches here is a native guard, so "no value of this variable reaches
        // here" is a Verified statement. It is still not a death signal — the
        // verdict owns death, and no branch is pruned by it; what the empty lane
        // buys is a consumer's *silence*.
        //
        // A lane holding any `Asserted` arm keeps the landed fallback and drops to
        // no-fact: emptying a docblock's claim proves nothing, and a consumer must
        // not read a lying `@param` as an exhaustion.
        if arms.is_empty() && !all_verified {
            store.contract.remove(var);
        }
        if changed {
            store.narrowed.insert(var.to_owned());
        }
        landed = changed;
    }
    landed
}

/// The `($var, literal)` of a comparison whose two operands are exactly one bare
/// variable and one literal (in either order).
fn var_literal(lhs: &CondOperand, rhs: &CondOperand) -> Option<(String, ArgValue)> {
    match (lhs, rhs) {
        (CondOperand::Var(v), CondOperand::Literal(val))
        | (CondOperand::Literal(val), CondOperand::Var(v)) => Some((v.clone(), val.clone())),
        _ => None,
    }
}

/// The `($var, int_literal, var_on_left)` of a comparison with one bare variable
/// and one **int** literal (ordering refinement only applies to int bounds).
fn var_int_literal(lhs: &CondOperand, rhs: &CondOperand) -> Option<(String, i64, bool)> {
    match (lhs, rhs) {
        (CondOperand::Var(v), CondOperand::Literal(ArgValue::Int(i))) => Some((v.clone(), *i, true)),
        (CondOperand::Literal(ArgValue::Int(i)), CondOperand::Var(v)) => Some((v.clone(), *i, false)),
        _ => None,
    }
}

/// Mirror an ordering operator (used when the variable is the right operand).
pub(crate) fn flip_ordering(op: CmpOp) -> CmpOp {
    match op {
        CmpOp::Lt => CmpOp::Gt,
        CmpOp::Le => CmpOp::Ge,
        CmpOp::Gt => CmpOp::Lt,
        CmpOp::Ge => CmpOp::Le,
        other => other,
    }
}

/// The logical negation of an ordering operator (for the false-path).
pub(crate) fn negate_ordering(op: CmpOp) -> CmpOp {
    match op {
        CmpOp::Lt => CmpOp::Ge,
        CmpOp::Le => CmpOp::Gt,
        CmpOp::Gt => CmpOp::Le,
        CmpOp::Ge => CmpOp::Lt,
        other => other,
    }
}

/// The int interval `var <op> k` denotes (`> 0` → positive, `>= 0` → non-negative,
/// `<`/`<=` symmetric). `None` when the interval is empty (a saturating bound
/// overflow) — the caller adds no refinement.
fn ordering_range(op: CmpOp, k: i64) -> Option<IntRange> {
    match op {
        CmpOp::Gt => IntRange::new(k.checked_add(1)?, i64::MAX),
        CmpOp::Ge => IntRange::new(k, i64::MAX),
        CmpOp::Lt => IntRange::new(i64::MIN, k.checked_sub(1)?),
        CmpOp::Le => IntRange::new(i64::MIN, k),
        _ => None,
    }
}

/// Apply a branch's refinements to its cloned env (clearing any stale exact-class
/// fact for a positively-narrowed variable), at trust stratum `stratum` (ADR-0052
/// §5): `Verified` for native-condition branches (the runtime test executed) and for
/// `assert($expr)` (the owner ruling reads it as a throw-guard), `Asserted` for
/// `@phpstan-assert`-tag-derived narrowings (a docblock claim).
pub(crate) fn apply_refinements(
    refs: &[Refine],
    env: &mut HashMap<String, Known>,
    store: &mut Store,
    stratum: Stratum,
) {
    for r in refs {
        match r {
            // `=== v` INTERSECTS the branch's knowledge with proven equality
            // (issue #445). Where the intersection is non-empty the singleton is
            // the whole of it — `{v}` meets anything admitting `v` at `{v}` — so
            // the fact is exactly as trustworthy as the test that established it
            // (`stratum`), regardless of any prior (weaker) knowledge; and the arm
            // lane, which the value lane now strictly outranks, is unbound as
            // before. Where the guards above this one already refuted `v`, the
            // intersection is EMPTY, and the empty domain is what must be left —
            // never `v` itself, which would state on an unreachable path exactly
            // what the chain disproved.
            Refine::Exact(var, val) => {
                if refinement_refuted(env, store, var, val) {
                    leave_empty_domain(env, store, var);
                    continue;
                }
                env.insert(
                    var.clone(),
                    Known::value_strat(
                        Fact::Singleton(val.clone()),
                        0,
                        Some("proven on this branch".to_owned()),
                        stratum,
                    ),
                );
                store.unbind(var);
            }
            Refine::NotNull(var) => refine_fact(env, var, stratum, clear_null),
            Refine::Exclude(var, val) => {
                refine_fact(env, var, stratum, |f| exclude_member(f, val));
            }
            Refine::IntRange(var, range) => {
                refine_fact(env, var, stratum, |f| intersect_int(f, *range));
            }
            Refine::Truthy(var) => refine_fact(env, var, stratum, truthy_narrow),
        }
    }
}

/// **Is a positive refinement's own claim already refuted on this branch?**
/// (issue #445) — the composition question a chain of guards asks and a single
/// guard cannot: one guard states `$var === val`, and the guards above it may
/// already have proved that nothing `$var` can still hold IS `val`. Two carriers
/// can hold that proof, and either alone is enough:
///
/// * the **value lane** — a fact that does not admit `val` ([`Fact::admits`],
///   extensional membership);
/// * the **arm lane** — a lane whose every surviving arm provably cannot hold
///   `val` ([`steins_contract::admits_val`] answering `No`). `Maybe` keeps the arm
///   in play, so an arm the judgment cannot decide never refutes: short of proof
///   the answer is always "the refinement stands".
///
/// The arm lane matters on its own, not merely as a second opinion: a `@param 1|2`
/// over a native `int` never reaches the value lane at all
/// ([`seed_refined_scalar_fact`] declines to overwrite a `General` base with a
/// `OneOf`), so the residue `2` that such a chain's first guard leaves lives there
/// and nowhere else — which is exactly why replacing the lane rather than
/// intersecting it went unnoticed for as long as it did.
fn refinement_refuted(env: &HashMap<String, Known>, store: &Store, var: &str, val: &Val) -> bool {
    if env.get(var).and_then(|k| k.fact.as_ref()).is_some_and(|f| !f.admits(val)) {
        return true;
    }
    arms_refute(store, var, |arm| steins_contract::admits_val(arm, val) == Certainty::No)
}

/// Ask `refutes` of **every** surviving arm of `var`'s declared lane and answer
/// whether the lane both exists and answered unanimously.
///
/// The three-way distinction [`Store::contract_arms`] draws is the whole point of
/// routing through it: an ABSENT lane states nothing at all — an undeclared
/// variable, one a by-reference call invalidated, an enum whose case set the
/// absence discipline refused to complete — and answers `false`, leaving the
/// caller's refinement exactly as it was. Only a lane that is present, non-empty
/// and unanimous refutes anything.
pub(crate) fn arms_refute(store: &Store, var: &str, refutes: impl Fn(&ContractTy) -> bool) -> bool {
    store.contract_arms(var).is_some_and(|arms| arms.iter().all(|a| refutes(&a.ty)))
}

/// **The empty domain** — what a positive refinement leaves behind when the
/// intersection with what the branch already proved comes out empty (issue #445).
///
/// Not a death signal: ADR-0052 §2 puts death with the verdict, and nothing here
/// prunes a branch or marks one unreachable. It is the same shape an exhausted
/// guard chain already reaches by subtraction alone — no value fact, no arm lane —
/// so every consumer of either carrier answers about this position the way it
/// already answers about the `default` of a chain that covered its subject:
/// silence, on the ground that it knows of no value that gets here. The
/// alternative, keeping the refinement's own seed, is the one answer that is
/// certainly wrong: it states on a path PHP never takes precisely the value the
/// guards above disproved.
pub(crate) fn leave_empty_domain(env: &mut HashMap<String, Known>, store: &mut Store, var: &str) {
    env.remove(var);
    store.unbind(var);
}

/// Transform the fact of `var` in place with `f` (a `None` result drops the fact —
/// the conservative empty-fact fallback); a no-op when `var` has no fact. The
/// result stratum is `min(existing, refine_stratum)`: a narrowing (`!== null`,
/// interval, truthy, member exclusion) constrains the *existing* fact, so it is
/// only as trustworthy as its weakest component (ADR-0052 §5 derivation clause).
pub(crate) fn refine_fact(
    env: &mut HashMap<String, Known>,
    var: &str,
    refine_stratum: Stratum,
    f: impl FnOnce(&Fact) -> Option<Fact>,
) {
    let Some(k) = env.get(var) else { return };
    // A closure-only binding carries no scalar fact — value refinements do not
    // apply to it; leave it intact.
    let Some(kf) = &k.fact else { return };
    match f(kf) {
        Some(nf) => {
            let (line, bound, stratum) = (k.line, k.bound.clone(), k.stratum.min(refine_stratum));
            env.insert(var.to_owned(), Known::value_strat(nf, line, bound, stratum));
        }
        None => {
            env.remove(var);
        }
    }
}

/// Clear nullability: an abstract fact loses its `nullable` flag; a finite fact
/// loses its `null` member. `None` only if that empties a finite fact.
pub(crate) fn clear_null(f: &Fact) -> Option<Fact> {
    match f {
        Fact::Refined { base, refinement, nullable: true } => {
            Some(Fact::refined(*base, *refinement, false))
        }
        Fact::General { base, nullable: true } => Some(Fact::General { base: *base, nullable: false }),
        Fact::Singleton(_) | Fact::OneOf(_) => exclude_member(f, &Val::Null),
        // Already non-nullable abstract fact — unchanged.
        other => Some(other.clone()),
    }
}

/// Remove `val` from a finite fact; for a String-based abstract fact excluding
/// `""`, add `NON_EMPTY` (the `!== ''` refinement). Otherwise unchanged.
pub(crate) fn exclude_member(f: &Fact, val: &Val) -> Option<Fact> {
    match f.finite_members() {
        Some(members) => {
            let kept: Vec<Val> = members.iter().filter(|m| *m != val).cloned().collect();
            // Empty → drop the fact (conservative fallback; a truly-dead branch is
            // already pruned by the decided-guard verdict).
            Fact::from_vals(kept)
        }
        None => match (f, val) {
            (Fact::Refined { base: Base::String, .. } | Fact::General { base: Base::String, .. }, Val::Str(s))
                if s.is_empty() =>
            {
                Some(add_str_preds(f, StrPreds::NON_EMPTY))
            }
            _ => Some(f.clone()),
        },
    }
}

/// Intersect an Int-based abstract fact with `range`; a finite/other fact is left
/// unchanged. `None` when the intersection is empty.
fn intersect_int(f: &Fact, range: IntRange) -> Option<Fact> {
    match f {
        Fact::Refined { base: Base::Int, refinement: Refinement::Int(have), nullable } => {
            let r = have.intersect(range)?;
            Some(Fact::refined(Base::Int, Refinement::Int(r), *nullable))
        }
        Fact::General { base: Base::Int, nullable } => {
            Some(Fact::refined(Base::Int, Refinement::Int(range), *nullable))
        }
        other => Some(other.clone()),
    }
}

/// Truthiness narrowing on the true-path: clear nullability (null is falsy) and,
/// for a String-based fact, add `NON_FALSY`. Int-based facts gain nothing usable
/// (nonzero is not an interval — skipped, documented). Never empties.
fn truthy_narrow(f: &Fact) -> Option<Fact> {
    let f = clear_null(f)?;
    Some(match &f {
        Fact::Refined { base: Base::String, .. } | Fact::General { base: Base::String, .. } => {
            add_str_preds(&f, StrPreds::NON_FALSY)
        }
        other => other.clone(),
    })
}

/// Add string predicates to a String-based abstract fact (union-closed); a
/// non-string or finite fact is returned unchanged.
///
/// Both arms close under implication: the `Refined` arm gets it free from
/// [`StrPreds::union`], and the `General` arm (starting from no predicates) must
/// ask for it explicitly. Without this, a raw `NON_FALSY` bit would render as
/// `non-falsy-string` but answer `false` to "is it non-empty?" — a consumer
/// testing the weaker predicate (DR3's `explode` separator gate is the first)
/// would silently miss the stronger one.
pub(crate) fn add_str_preds(f: &Fact, preds: StrPreds) -> Fact {
    match f {
        Fact::Refined { base: Base::String, refinement: Refinement::Str(have), nullable } => {
            Fact::refined(Base::String, Refinement::Str(have.union(preds)), *nullable)
        }
        Fact::General { base: Base::String, nullable } => {
            Fact::refined(Base::String, Refinement::Str(preds.close()), *nullable)
        }
        other => other.clone(),
    }
}
