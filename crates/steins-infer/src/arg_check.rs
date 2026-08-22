//! The argument-acceptance checks on the value lane: `type.argument-mismatch`
//! rendering, the proof-layer `TypeError` relation (ADR-0011), the possibly-grade
//! pair `type.maybe-argument-mismatch` / `phpdoc.maybe-argument-mismatch` (ADR-0081
//! §8), and builtin arguments judged against the reflected parameter list through
//! the same relation.

use std::collections::HashMap;

use steins_contract::ContractTy;
use steins_domain::{Base, Fact, Refinement};
use steins_sidecar::BuiltinParam;
use steins_syntax::{ArgValue, CallExpr, NativeType, Param, ScalarType, Span, TypeMember};

use crate::cx::Cx;
use crate::descent::{
    project_call_summary, project_method_summary, propagated_arg_value, summary_binds,
};
use crate::dump::render_contract_arms;
use crate::env::{ContractArm, Known, Store, Stratum};
use crate::heap::simple_class;
use crate::project::Diagnostic;
use crate::return_arms::{call_return_arms_by_name, method_return_arms_by_callee};
use crate::fold::Folder;
use crate::{PHPDOC_MAYBE_ARGUMENT_MISMATCH_ID, TYPE_MAYBE_ARGUMENT_MISMATCH_ID, describe_fact};
use crate::builtin_returns::{builtin_call_return_fact, builtin_return_floor, store_holds_resource};
use crate::coerce::{member_accepts_coercive, member_accepts_strict};
use crate::offsets::shape_read_at;

// ---------------------------------------------------------------------------
// Value / type helpers.
// ---------------------------------------------------------------------------

/// Render a call with its literal arguments for a folding provenance string.
pub(crate) fn render_call(name: &str, args: &[ArgValue]) -> String {
    let inner: Vec<String> = args.iter().map(ArgValue::render).collect();
    format!("{name}({})", inner.join(", "))
}

/// The generalized truth table: does passing (or returning) a **literal** `arg`
/// where a native scalar/union type `ty` is required provably raise a
/// `TypeError` under PHP 8.1+ (honoring `strict`)?
///
/// Settled **empirically against PHP 8.5.8** (analyzer floor 8.1, ADR-0011;
/// union-coercion rules stable since 8.0):
///
/// ```text
/// COERCIVE (error iff the value coerces to NO member):
///   1.5   -> int|string  => OK  (becomes int 1; the string sink also accepts)
///   1.5   -> string|bool => OK  (becomes '1.5')
///   true  -> int|string  => OK  (becomes int 1)
///   "abc" -> int|float    => TypeError   (non-numeric string, no string sink)
///   "abc" -> int|false    => TypeError   (false-literal accepts only `false`)
///   "5"   -> int|float    => OK  (numeric string coerces)
///   false -> string|false => OK  (matches the `false` literal member exactly)
///   true  -> string|false => OK  (becomes '1' via the string member)
///   null  -> int|string   => TypeError   (non-nullable)
///   0/""/true -> false     => TypeError   (no coercion into a bool-literal)
/// STRICT (value must match SOME member; only int->float widening is implicit):
///   1.5   -> int|string  => TypeError   (float, no float member)
///   true  -> int|string  => TypeError   (bool, no bool/bool-literal member)
///   5     -> int|float    => OK  (int member; also OK via int->float widening)
///   false -> string|false => OK  (matches the `false` literal member)
///   true  -> string|false => TypeError   (`false` literal ≠ `true`; no bool)
///   5     -> string|false => TypeError   (int, no int member)
/// ```
///
/// Uncertain cells resolve to "not an error" (silence is always safe; ADR-0002).
///
/// ADR-0043 stage 3 opens two definite-No arms, both riding the trinary is-a
/// oracle on `cx`: a **proven object value** (`new` / enum case) errors iff every
/// union member provenly rejects its exact class; a **scalar value** sees through
/// any `Instance` union members (no coercion exists for those, exactly as the
/// `member_accepts_*` tables' `Instance => false` arms encode). A `null` value
/// against an object-bearing type stays silent (out of scope; sidesteps the
/// `has_null_default` implicit-nullable interplay).
/// Whether `p` is **implicitly nullable** and `arg` is the `null` its default
/// admits — the argument-side half of a bit the callee side has read all along
/// (issue #391).
///
/// `function f(string $s = null)` declares a non-nullable hint whose `= null`
/// default makes PHP widen the parameter to `?string`: `f(null)` runs, emitting
/// only the 8.4 "implicitly marking parameter as nullable" deprecation. Every
/// argument-position native check therefore has to consult the default beside the
/// hint, exactly as [`seed_fact`] does on the declaration side
/// (`ty.nullable || p.has_null_default`). Without it a proven `null` — and, since
/// issue #391, a fact whose `null` side-flag is set — convicted a parameter PHP
/// accepts, a live proof-layer false positive on the pinned corpus.
///
/// A guard rather than a widened [`NativeType`] so the diagnostic keeps rendering
/// the spelling the declaration actually carries.
///
/// [`seed_fact`]: crate::seed_fact
pub(crate) fn implicit_null_accepted(p: &Param, arg: &ArgValue) -> bool {
    p.has_null_default && matches!(arg, ArgValue::Null)
}

pub(crate) fn is_type_error(cx: &Cx, ty: &NativeType, arg: &ArgValue) -> bool {
    let strict = cx.strict();
    match arg {
        // `null` is accepted iff the type is nullable (`?T` / `null` member). An
        // object-bearing type stays silent on `null`.
        ArgValue::Null => !ty.nullable && !ty.has_instance(),
        // A concrete non-null literal: an error iff no member accepts it. `Instance`
        // members contribute nothing (they never accept a scalar) — ADR-0043 stage-3
        // scalar-vs-object opening (e.g. a raw string where an enum is required).
        ArgValue::Int(_) | ArgValue::Float(_) | ArgValue::Str(_) | ArgValue::Bool(_) => {
            if strict {
                !ty.members.iter().any(|m| member_accepts_strict(m, arg))
            } else {
                !ty.members.iter().any(|m| member_accepts_coercive(m, arg))
            }
        }
        // A proven object value (ADR-0043 stage 3): a definite No iff every union
        // member provenly rejects an object of its exact class. An unresolvable /
        // ambiguous class stays unproven (silent).
        ArgValue::New(..) | ArgValue::EnumCase(..) => match cx.proven_object_class(arg) {
            Some(class) => cx.object_is_type_error(ty, &class),
            None => false,
        },
        // An array is never a native scalar/union finding (arrays only ever fail
        // the phpdoc contract relation, checked separately).
        ArgValue::Array(_) => false,
        // Non-provable carriers: silent (a `Ternary` is resolved to a concrete arm
        // before this point; a `ClassConst` is resolved upstream to an enum case /
        // literal, so an unresolved one is genuinely unproven).
        ArgValue::Var(_)
        | ArgValue::Call(..)
        // A method call reaching here did not resolve to a value — a resolved one
        // arrives as the literal its summary proved (issue #386).
        | ArgValue::MethodCall { .. }
        | ArgValue::Ternary { .. }
        | ArgValue::Coalesce(..)
        | ArgValue::OffsetRead { .. }
        | ArgValue::PropFetch { .. }
        | ArgValue::Clone(_)
        | ArgValue::ClassConst(..)
        // A concatenation reaching here did not resolve — an operand's value is
        // unknown, so the result string is too. (A resolved one arrives as `Str`.)
        | ArgValue::Concat(..)
        // An undecided comparison (issue #260) proves no value; a decided one
        // arrives here already resolved to its `Bool`.
        | ArgValue::Binary { .. }
        // A closure value against a scalar/union param is never a scalar finding
        // (a `callable`/`Closure` param is not a native scalar type this checks).
        | ArgValue::Closure(_)
        // A global-constant fetch (issue #168) is genuinely unproven here.
        | ArgValue::GlobalConst(..)
        | ArgValue::Other => false,
    }
}

/// ADR-0043 stage 3 — the object-world native definite-No is **guard-blind inside
/// a binding descent** and must be suppressed there. A descent rebinds a callee's
/// parameter to a hypothetical caller value, but the callee's in-body `instanceof`
/// guards that would narrow it are unmodeled (e.g. Carbon's
/// `if ($x instanceof DateTimeInterface) { … $x … }` is dead for a string `$x`,
/// but the walk cannot prove it — the guard flows through an intermediate
/// boolean). Checking an object-world mismatch on a descent-bound value is thus
/// unsound, the same reason descent-bound property writes are unchecked (see
/// `apply_prop_assign`). Only a judgment touching an object type is suppressed;
/// scalar-vs-scalar descent checks are unaffected. Always `false` outside descent.
pub(crate) fn object_world_guard_blind(in_descent: bool, ty: &NativeType, value: &ArgValue) -> bool {
    in_descent
        && (ty.has_instance() || matches!(value, ArgValue::New(..) | ArgValue::EnumCase(..)))
}

// ===========================================================================
// The argument side's possibly grade (ADR-0081's 2026-08-16 amendment, issue
// #391): SOME arm of the argument's abstract fact is rejected by the native
// parameter type and some is accepted. Two ids, one judgment, split by the
// premise's minimum stratum (ADR-0052 §5).
//
// The ALL-arms-rejected verdict is deliberately NOT emitted: issue #291
// measured it empty on the pinned corpus, on phpstan-src's nsrt and on
// php-typing-conformance, and closes with "don't build it".
// ===========================================================================

/// The witnesses that decide, for one [`Base`], whether a native type accepts
/// **any** value of that base.
///
/// The base-level question is not "does a representative value pass" but "does
/// *every* value of this base fail", so a base whose acceptance is not uniform
/// across its own values needs one witness per equivalence class of PHP's
/// coercion behaviour:
///
/// * `int` / `float` — uniform in both modes, one witness each.
/// * `bool` — a `false` literal member (`string|false`) accepts exactly one of
///   the two, so both are needed.
/// * `string` — [`member_accepts_coercive`] splits on `php_is_numeric`, so a
///   numeric and a non-numeric witness are both needed. That split is the entire
///   reason a `string` base is not a coercive-mode definite No against `int`.
///
/// The classes are measured, not asserted: `harness/coercion-grid` runs all 72
/// cells per mode on PHP itself, and `tests/coercion_witness_grid.rs` pins
/// Steins against it. The witnesses go to [`is_type_error`] — there is **no
/// second coercion table** anywhere in this judgment.
fn maybe_arg_witnesses(base: Base) -> Vec<ArgValue> {
    match base {
        Base::Int => vec![ArgValue::Int(0)],
        Base::Float => vec![ArgValue::Float(1.5)],
        Base::Bool => vec![ArgValue::Bool(true), ArgValue::Bool(false)],
        Base::String => vec![ArgValue::Str("5".into()), ArgValue::Str("abc".into())],
    }
}

/// One abstract arm: a scalar base with the refinement (if any) carried with it.
/// The `null` side-flag is never an arm — it rides beside the list, as in [`Fact`].
type AbstractArm = (Base, Option<Refinement>);

/// How much of an abstract fact's denotation a native parameter rejects.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MaybeArgVerdict {
    /// Every arm rejected — the definite No of issue #291. Measured empty, and
    /// never emitted: see the module header above.
    Every,
    /// Some arm rejected, some accepted — the possibly-grade claim this pair of
    /// ids carries.
    Partial,
    /// Nothing provably rejected, or a fact the judgment declines.
    Nothing,
}

/// The abstract arms of `fact` — one entry for a single-base layer, several for a
/// union — plus its `null` side-flag; `None` for a finite or array fact.
fn maybe_arg_arms(fact: &Fact) -> Option<(Vec<AbstractArm>, bool)> {
    match fact {
        Fact::Refined { base, refinement, nullable } => {
            Some((vec![(*base, Some(*refinement))], *nullable))
        }
        Fact::General { base, nullable } => Some((vec![(*base, None)], *nullable)),
        Fact::Union { arms, nullable } => Some((arms.clone(), *nullable)),
        Fact::Singleton(_) | Fact::OneOf(_) | Fact::Shape { .. } => None,
    }
}

/// Spell one abstract union arm the way [`describe_fact`] spells a whole fact —
/// used to name the rejected arms in the message.
fn spell_arm(arm: &AbstractArm) -> String {
    let (base, refinement) = arm;
    let f = match refinement {
        Some(r) => Fact::refined(*base, *r, false),
        None => Fact::General { base: *base, nullable: false },
    };
    describe_fact(&f).trim_start_matches("a value of type ").to_owned()
}

/// The base-level analogue of [`is_type_error`], built out of it rather than
/// beside it: per arm of `fact` (plus the `null` side-flag), ask whether the
/// native parameter rejects every witness of that arm's base.
///
/// A `Refined` arm decomposes to its **base**, dropping the refinement: a refined
/// set is a subset of its base's set, so base-rejection implies refined-rejection.
/// The converse (`numeric-string` into a coercive `int`) is sharper and is not
/// taken here — that is a second judgment with its own FP surface.
///
/// `Singleton`/`OneOf` decline: those are the concrete lane [`is_type_error`]
/// already owns, and owns more precisely. `Shape` declines: an array against a
/// native scalar parameter is a real `TypeError`, but `is_type_error` answers
/// `false` for an array by construction, so admitting it here would smuggle in
/// the second table this judgment refuses.
///
/// `null_tolerated` is the internal-null coercive carve-out (ADR-0056 §9.3),
/// `false` for every userland parameter: the `null` side-flag cannot be a rejected
/// arm where PHP only deprecates it. See [`internal_null_tolerated`].
///
/// Returns the verdict, the rejected bases, and whether the `null` side-flag is
/// rejected. Spelling is the caller's, since the declared-arm lane can name an arm
/// (`false`) more precisely than its base (`bool`).
fn maybe_arg_verdict(
    cx: &Cx,
    p: &Param,
    ty: &NativeType,
    fact: &Fact,
    null_tolerated: bool,
) -> (MaybeArgVerdict, Vec<AbstractArm>, bool) {
    let Some((arms, nullable)) = maybe_arg_arms(fact) else {
        return (MaybeArgVerdict::Nothing, Vec::new(), false);
    };
    if arms.is_empty() {
        return (MaybeArgVerdict::Nothing, Vec::new(), false);
    }
    let rejected: Vec<AbstractArm> = arms
        .iter()
        .filter(|arm| maybe_arg_witnesses(arm.0).iter().all(|w| is_type_error(cx, ty, w)))
        .copied()
        .collect();
    // The implicit-nullable default is part of the parameter's acceptance, exactly
    // as it is for a proven `null` (issue #391's second repair).
    let null_rejected = nullable
        && is_type_error(cx, ty, &ArgValue::Null)
        && !implicit_null_accepted(p, &ArgValue::Null)
        && !null_tolerated;
    let total = arms.len() + usize::from(nullable);
    let n = rejected.len() + usize::from(null_rejected);
    let verdict = if n == total {
        MaybeArgVerdict::Every
    } else if n == 0 {
        MaybeArgVerdict::Nothing
    } else {
        MaybeArgVerdict::Partial
    };
    (verdict, rejected, null_rejected)
}

/// The bases (and the `null` flag) one declared-lane arm denotes, or `None` for an
/// arm the value lattice has no scalar reading of (an array/callable/class arm).
fn arm_base_set(ty: &ContractTy) -> Option<(Vec<Base>, bool)> {
    if matches!(ty, ContractTy::Null) {
        return Some((Vec::new(), true));
    }
    let f = steins_contract::to_fact(ty)?;
    if let Some(vals) = f.finite_members() {
        let mut bases = Vec::new();
        let mut nullable = false;
        for v in vals {
            match v.base() {
                Some(b) if !bases.contains(&b) => bases.push(b),
                Some(_) => {}
                None => nullable = true,
            }
        }
        return Some((bases, nullable));
    }
    maybe_arg_arms(&f).map(|(arms, n)| (arms.into_iter().map(|a| a.0).collect(), n))
}

/// Name the rejected arms for the message, preferring the **declared** spelling
/// where the premise came from the arm lane: a lane that says `string|false`
/// should not have its rejected arm reported as `bool`, which is only what the
/// lowering widened it to. Falls back to the base spelling for a value-lane
/// premise, which has no finer spelling to offer.
fn spell_rejected_arms(
    cx: &Cx,
    lane: Option<&[ContractArm]>,
    rejected: &[AbstractArm],
    null_rejected: bool,
) -> Vec<String> {
    if let Some(arms) = lane {
        let named: Vec<String> = arms
            .iter()
            .filter(|a| match arm_base_set(&a.ty) {
                // An arm is named iff everything it denotes was rejected — the
                // same all-or-nothing rule the base-level verdict applies.
                Some((bases, nullable)) => {
                    (!bases.is_empty() || nullable)
                        && bases.iter().all(|b| rejected.iter().any(|r| r.0 == *b))
                        && (!nullable || null_rejected)
                }
                None => false,
            })
            .filter_map(|a| render_contract_arms(cx, std::slice::from_ref(a)))
            .collect();
        if !named.is_empty() {
            return named;
        }
    }
    let mut out: Vec<String> = rejected.iter().map(spell_arm).collect();
    if null_rejected {
        out.push("null".to_owned());
    }
    out
}

/// Lower a declared-arm list to the possibly-grade judgment's premise (issue #391
/// A4 / issue #418): one [`Fact`] through the same `to_fact` the scalar seeding
/// uses, the arms' own minimum [`Stratum`] (ADR-0052 §5's consumption rule), and
/// the arm list itself for the message speller. Shared by every carrier whose
/// value lane may be empty — a `Var`, a nested call, a nested method call — so
/// the fallback reads one way everywhere.
///
/// `None` for an empty arm list, an arm list the value domain cannot lower at
/// all, or one that lowers to a finite fact (`Singleton`/`OneOf` are the
/// concrete lane's, already judged exactly by [`is_type_error`]).
fn arm_lane_premise(arms: Vec<ContractArm>) -> Option<(Fact, Stratum, Option<Vec<ContractArm>>)> {
    if arms.is_empty() {
        return None;
    }
    let lowered = steins_contract::to_fact(&steins_contract::ContractTy::Union(
        arms.iter().map(|a| a.ty.clone()).collect(),
    ))?;
    if lowered.finite_members().is_some() {
        return None;
    }
    let stratum = arms.iter().fold(Stratum::Verified, |acc, a| acc.min(a.stratum));
    Some((lowered, stratum, Some(arms)))
}

/// The abstract premise available for `value` at an argument position (issue #391
/// A4, extended to the non-`Var` carriers by issue #418): the value-lane fact
/// where there is one, else the declared-arm lane lowered through
/// [`arm_lane_premise`], carrying the arms' own stratum.
///
/// **`Var`.** The arm lane is not an optional extra: the value lane has **no
/// carrier** for a docblock-or-reflection `T|false` ([`seed_refined_scalar_fact`]
/// mints a value-lane fact only when a native `General` is refined within its own
/// base). Ten of the twelve corpus hits issue #391 measured arrived on the arm
/// lane and nowhere else.
///
/// **`Call`/`MethodCall` (a nested call, issue #386's value-IR carrier, issue
/// #418).** The callee's proven return summary
/// ([`project_call_summary`]/[`project_method_summary`]) where the body proved
/// one strictly sharper than its declared floor ([`summary_binds`] — a bare
/// `General{base}` degraded join carries nothing beyond the arms, so the floor is
/// preferred there for the same reason the dump surface prefers it); else the
/// declared return arms
/// ([`call_return_arms_by_name`]/[`method_return_arms_by_callee`]) at their own
/// stratum, through [`arm_lane_premise`] — an `Asserted` arm premises
/// `phpdoc.maybe-argument-mismatch`, never the `type.*` sibling (ADR-0052 §5).
/// A **builtin** callee reads the builtin ladder instead (ADR-0056 §9): the
/// reflected return fact, else the ADR-0069 declared floor, in the order the
/// assignment path takes them, so `f(realpath($p))` and `$r = realpath($p);
/// f($r)` cannot answer differently about one call.
/// The summary read is skipped inside a binding descent (`in_descent`, the same
/// recursion-guard reason the native definite check skips it — a fresh descent
/// tree started from inside a live one would evade the on-stack recursion guard);
/// the arm-lane fallback is not, since it walks no body.
///
/// **Declines when the argument's own call is a same-expression guard**
/// (issue #421's follow-up): a call has no binding to narrow — `if ($e !==
/// null) { f($e); }` narrows `$e` for a `Var` because `$e` names one heap slot
/// both the guard and the read consult, but a call is a fresh evaluation each
/// time it is written, so a repeated `f($o->m())` under `if ($o->m() !== null)`
/// is not the SAME value on any proof this analyzer holds — only evidence the
/// caller already reasoned about this exact expression's null/false-ness.
/// [`Store::expr_is_guarded`] is that decline, checked before either premise
/// source is even read (a guarded call is silent whether the premise would have
/// come from the summary or the floor).
///
/// **`OffsetRead` (`$a['k']`, issue #418).** The shape lane's fact for the key,
/// through [`shape_read_at`] — the same resolver the array-shape read row (ADR-0062
/// §4 S3) and the strict offset legs go through, so a read and this judgment can
/// never disagree about which field they mean. Declines unless `base` is a `Var`
/// carrying a `Fact::Shape` (`shape_read_at`'s own gate). Always `Asserted`: a
/// shape fact is seeded at `Asserted` unconditionally (A-G9's corollary), so this
/// carrier premises only `phpdoc.maybe-argument-mismatch`, never `type.*`. A
/// `!== null` guard on the same key narrows the field itself
/// ([`collect_shape_guards`]'s isset-equivalent reading, issue #421) rather than
/// needing a decline here — the read this function makes already sees it.
///
/// **`PropFetch` (`$o->p`) does not reach this function at all.** Issue #421:
/// a depth-1 property fetch has no [`CondOperand`] variant of its own —
/// `$o->p !== null` lowers its `$o->p` side to [`CondOperand::Other`], which
/// carries a call's invalidation footprint but nothing that identifies the
/// property read itself, so neither [`Store::prop_fact`] narrowing (there is
/// none — confirmed by the same audit that added the `Call`/`MethodCall`
/// decline above) nor a same-expression guard (there is nothing to structurally
/// compare) is reachable for this carrier without a new lowering variant, which
/// is a wider change than this slice makes. A carrier that would convict guarded
/// code cannot ship even at the strict floor, so it is dropped rather than
/// shipped unguarded (ADR-0002).
///
/// A finite fact declines on every lane: `Singleton`/`OneOf` are the concrete
/// lane's, and [`is_type_error`] has already judged them exactly.
///
/// [`seed_refined_scalar_fact`]: crate::seed_refined_scalar_fact
/// [`collect_shape_guards`]: crate::collect_shape_guards
/// [`CondOperand`]: steins_syntax::CondOperand
/// [`CondOperand::Other`]: steins_syntax::CondOperand::Other
#[allow(clippy::too_many_arguments)]
fn maybe_arg_premise(
    cx: &Cx,
    folder: &mut dyn Folder,
    value: &ArgValue,
    env: &HashMap<String, Known>,
    store: &Store,
    poisoned: bool,
    in_descent: bool,
    this_exact: Option<&str>,
    enclosing_class: Option<&str>,
    span_start: u32,
    out: &mut Vec<Diagnostic>,
) -> Option<(Fact, Stratum, Option<Vec<ContractArm>>)> {
    if poisoned {
        return None;
    }
    if matches!(value, ArgValue::Call(..) | ArgValue::MethodCall { .. }) && store.expr_is_guarded(value)
    {
        return None;
    }
    match value {
        ArgValue::Var(name) => {
            if let Some(k) = env.get(name)
                && let Some(f) = &k.fact
            {
                return f.finite_members().is_none().then(|| (f.clone(), k.stratum, None));
            }
            arm_lane_premise(store.contract_arms(name)?.to_vec())
        }
        ArgValue::Call(name, args) => {
            if !in_descent
                && let Some(sv) = project_call_summary(
                    cx, folder, name, args, env, store, poisoned, span_start, None, out,
                )
                .and_then(|s| s.value)
                && summary_binds(&sv.fact)
                && sv.fact.finite_members().is_none()
            {
                return Some((sv.fact, sv.stratum, None));
            }
            if let Some(arms) =
                call_return_arms_by_name(cx, folder, name, args, env, store, poisoned)
            {
                return arm_lane_premise(arms);
            }
            // A **builtin** callee (ADR-0056 §9): `value_lane_fn_site` above answers
            // only for a project function, so `realpath($p)` reached no premise at
            // all — while `$r = realpath($p); f($r)` did, off the very same rungs.
            // The two spellings are one call, so they read the same ladder, in the
            // assignment path's own order: the engine's own return fact first
            // (`Verified`, ADR-0056 §2), and only where the engine seeds nothing the
            // ADR-0069 declared floor (`Asserted`, so it premises
            // `phpdoc.maybe-argument-mismatch` and never the `type.*` sibling).
            builtin_call_return_fact(cx, folder, name).map_or_else(
                || arm_lane_premise(builtin_return_floor(cx, name)?),
                |f| f.finite_members().is_none().then_some((f, Stratum::Verified, None)),
            )
        }
        ArgValue::MethodCall { callee, args, named } => {
            if !in_descent
                && let Some(sv) = project_method_summary(
                    cx, folder, callee, args, named, env, store, this_exact, enclosing_class,
                    poisoned, span_start, None, out,
                )
                .and_then(|s| s.value)
                && summary_binds(&sv.fact)
                && sv.fact.finite_members().is_none()
            {
                return Some((sv.fact, sv.stratum, None));
            }
            arm_lane_premise(method_return_arms_by_callee(
                cx,
                folder,
                callee,
                args,
                env,
                store,
                this_exact,
                enclosing_class,
                poisoned,
            )?)
        }
        ArgValue::OffsetRead { base, key } => {
            let (read, stratum) = shape_read_at(base, key, env, poisoned, cx.php_minor)?;
            let f = read.into_fact()?;
            f.finite_members().is_none().then_some((f, stratum, None))
        }
        _ => None,
    }
}

/// Emit the possibly-grade argument finding for one argument position. Called from
/// both propagated-call checks and from the builtin arm (ADR-0056 §9.2) at the
/// point the native proof did **not** fire, so a definite No is never shadowed by
/// its own weaker sibling.
///
/// `null_tolerated` is the internal-null coercive carve-out, `false` everywhere but
/// the builtin arm in a coercive file — see [`internal_null_tolerated`].
#[allow(clippy::too_many_arguments)]
pub(crate) fn check_maybe_argument_mismatch(
    cx: &Cx,
    folder: &mut dyn Folder,
    param: &Param,
    callee: &str,
    arg_offset: u32,
    value: &ArgValue,
    env: &HashMap<String, Known>,
    store: &Store,
    this_exact: Option<&str>,
    enclosing_class: Option<&str>,
    poisoned: bool,
    in_descent: bool,
    null_tolerated: bool,
    out: &mut Vec<Diagnostic>,
) {
    let Some(ty) = param.ty.as_ref() else { return };
    // The same guard-blindness the object-world proof carries (ADR-0043 stage 3):
    // a rebound parameter's in-body `instanceof` guards are unmodeled in a descent.
    if in_descent && ty.has_instance() {
        return;
    }
    let Some((fact, stratum, lane)) = maybe_arg_premise(
        cx, folder, value, env, store, poisoned, in_descent, this_exact, enclosing_class,
        arg_offset, out,
    ) else {
        return;
    };
    let (verdict, rejected, null_rejected) =
        maybe_arg_verdict(cx, param, ty, &fact, null_tolerated);
    if verdict != MaybeArgVerdict::Partial {
        return;
    }
    let id = if stratum == Stratum::Verified {
        TYPE_MAYBE_ARGUMENT_MISMATCH_ID
    } else {
        PHPDOC_MAYBE_ARGUMENT_MISMATCH_ID
    };
    // Spell the subject the way `PHPStan\dumpType($x)` would, so the finding and
    // the dump a reader reaches for cannot disagree. `value.render()` is the
    // subject's own spelling — `$x` for a `Var`, but also `$o->p`, `g()`,
    // `$b->m()`, `$a['k']` for issue #418's carriers — one speller for the
    // subject that appears twice in the message.
    let subject_name = value.render();
    let subject = lane
        .as_deref()
        .and_then(|arms| render_contract_arms(cx, arms))
        .unwrap_or_else(|| describe_fact(&fact).trim_start_matches("a value of type ").to_owned());
    let named = spell_rejected_arms(cx, lane.as_deref(), &rejected, null_rejected);
    let arms = if named.len() == 1 {
        format!("its {} arm raises a TypeError", named[0])
    } else {
        format!("its {} arms raise a TypeError", named.join(" and "))
    };
    let pos = cx.tree().position(arg_offset);
    out.push(Diagnostic {
        id,
        path: cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        message: format!(
            "argument {subject_name} to {callee}() may not become {} ${} — {subject_name} is {subject}, and {arms} ({} mode)",
            ty.render(),
            param.name,
            if cx.strict() { "strict" } else { "coercive" },
        ),
        facet: None,
        fix: None,
    });
}

// ===========================================================================
// Builtin arguments (ADR-0056 §9, R1's parameter twin): the engine's own
// reflected parameter list, judged by the relation that has judged project
// parameters since ADR-0043.
//
// No new id, no new stratum and — with exactly one measured exception — no new
// coercion table. What this arm adds is a SOURCE: `Folder::builtin_param_types`
// answers what `strlen`'s parameter is, and everything downstream of that is the
// code the project arm runs. That is the whole design, and it is why the two
// arms cannot drift: they are the same judgment reached from two declarations.
// ===========================================================================

/// The **internal-null coercive carve-out** (ADR-0056 §9.3) — PHP's one
/// internal/userland difference in the parameter-coercion table.
///
/// From 8.1 on, `null` into a non-nullable scalar parameter of an *internal*
/// function is a deprecation in coercive mode, and a `TypeError` only under
/// `declare(strict_types=1)`. Probed at 8.5.9:
///
/// ```text
/// $ php -r 'echo strlen(null);'
/// Deprecated: strlen(): Passing null to parameter #1 ($string) of type string
/// 0
/// $ php -r 'declare(strict_types=1); echo strlen(null);'
/// Fatal error: Uncaught TypeError: strlen(): Argument #1 ($string) must be of
/// type string, null given
/// ```
///
/// Measured rather than recalled: `harness/coercion-grid/witness.php internal`
/// runs the cells on the project's own PHP and `tests/builtin_param_types.rs`
/// pins Steins against the recorded rows in both modes, the way the userland
/// grid has been pinned since issue #391.
///
/// A deprecation is not a finding here — there is no id for one, and claiming a
/// `TypeError` where PHP raises none is precisely the false positive ADR-0002
/// bars. The userland arms pass `false`: `f(null)` on `function f(string $s)`
/// fatals in both modes, and nothing about this carve-out reaches them.
fn internal_null_tolerated(cx: &Cx) -> bool {
    !cx.strict()
}

/// Lower a **reflected parameter type** — the `(string)` rendering of
/// `ReflectionParameter::getType()` — to the [`NativeType`] the same spelling
/// written in a project signature lowers to, or `None` where the native relation
/// models nothing (ADR-0056 §9.2).
///
/// Two steps, both existing seams. [`steins_contract::lower_str`] is the reader
/// the reflected *return* envelope already goes through, so one wire form has one
/// parser; the member discipline below then mirrors `steins_syntax`'s
/// `lower_hint` — the four scalar bases, the `true`/`false` literal members,
/// `null` as the nullable flag, and a single unmodeled member collapsing the
/// whole position to silence.
///
/// That mirroring is the point rather than a convenience. `"string"`, `"?int"`
/// and `"int|string"` judge exactly as those hints judge on a project parameter;
/// `"array|string"` (`str_replace`'s first three) and `"array"` (`array_map`'s
/// second) decline exactly as they do there, because [`NativeType`] has no array
/// member and inventing one here would be the second coercion table §9.2 refuses.
///
/// **One deliberate narrowing against `lower_hint`** (§9.4): a class-typed
/// position declines. The reader lowercases the class name, so there is no source
/// casing left to display, and the object-world definite No wants the project's
/// own is-a oracle for a class the project may never index.
fn builtin_param_native_type(rendered: &str) -> Option<NativeType> {
    let mut members = Vec::new();
    let mut nullable = false;
    lower_reflected_param_member(&steins_contract::lower_str(rendered)?, &mut members, &mut nullable)?;
    // A type with no non-null member (a standalone `null`) is not modeled — the
    // same refusal `lower_hint` makes for the same spelling.
    (!members.is_empty()).then_some(NativeType { members, nullable })
}

/// Accumulate one lowered member into `members`, recording `null` in `nullable`.
/// `None` the moment any part is a type the native relation does not model, which
/// the caller propagates into silence for the whole position.
fn lower_reflected_param_member(
    ty: &ContractTy,
    members: &mut Vec<TypeMember>,
    nullable: &mut bool,
) -> Option<()> {
    match ty {
        ContractTy::Null => *nullable = true,
        ContractTy::Base(Base::Int) => members.push(TypeMember::Scalar(ScalarType::Int)),
        ContractTy::Base(Base::Float) => members.push(TypeMember::Scalar(ScalarType::Float)),
        ContractTy::Base(Base::String) => members.push(TypeMember::Scalar(ScalarType::String)),
        ContractTy::Base(Base::Bool) => members.push(TypeMember::Scalar(ScalarType::Bool)),
        ContractTy::LitBool(b) => members.push(TypeMember::BoolLiteral(*b)),
        ContractTy::Union(ms) => {
            for m in ms {
                lower_reflected_param_member(m, members, nullable)?;
            }
        }
        // `mixed`, `array`, `iterable`, `callable`, `object`, `resource`, `void`,
        // a class, a refinement the native lane cannot spell — all silence.
        _ => return None,
    }
    Some(())
}

/// The synthetic [`Param`] one reflected position judges as. Carries the engine's
/// own parameter name, so the message names `$string` the way PHP's own
/// `TypeError` does.
///
/// `has_null_default` is deliberately `false` even for an optional position: the
/// null question at a builtin is [`internal_null_tolerated`]'s, which is about the
/// internal boundary and not about a default this signature may not have. Reading
/// a `null` default off reflection and folding it in here would answer the same
/// question twice, out of two different sources.
fn builtin_param_as_param(bp: &BuiltinParam, ty: NativeType, span: Span) -> Param {
    Param {
        name: bp.name.clone(),
        ty: Some(ty),
        hint_span: None,
        variadic: false,
        by_ref: false,
        has_null_default: false,
        has_default: bp.optional,
        default: None,
        span,
    }
}

/// Judge the positional arguments of a call to a uniquely-resolved **builtin**
/// against the engine's own reflected parameter types (ADR-0056 §9).
///
/// Called from the statement walk beside [`check_propagated_call`], which returns
/// early for a callee it cannot resolve to a project function — so this is the
/// only reporter at a builtin call site and there is no double-report to avoid.
/// It runs inside a binding descent on the same terms its neighbour does: the
/// callee's body is real code, and an argument proven there is proven at the line
/// it is written on.
///
/// Two resolutions feed one relation, because a builtin's single arm has to make
/// the split the project side carries as two whole passes: the propagated value
/// ([`propagated_arg_value`] — `$v`, `g()`, `$o->m()`, `$o->p`, at its own
/// stratum) and the env-free static one (`Cx::resolve_static_value` — a literal,
/// a `new`, an enum case, a class constant). Then the same four rungs in the same
/// order the project arm runs them: the proven-value definite No, the proven
/// object, the proven resource, and — only where none of those fired — the
/// possibly pair.
///
/// [`check_propagated_call`]: crate::check_propagated_call
#[allow(clippy::too_many_arguments)]
pub(crate) fn check_builtin_call_args(
    cx: &Cx,
    folder: &mut dyn Folder,
    poisoned: bool,
    in_descent: bool,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    store: &Store,
    this_exact: Option<&str>,
    enclosing_class: Option<&str>,
    out: &mut Vec<Diagnostic>,
) {
    // A named argument or an argument unpacking breaks the position→parameter map
    // this whole judgment is indexed by (§9.4, v1). `positional_only` is `false`
    // for both, and for the first-class-callable shape `f(...)`, which is a value
    // and not a call at all.
    if !call.positional_only {
        return;
    }
    let Some(name) = call.callee.as_deref() else { return };
    // A project function of this simple name shadows the builtin (or makes the
    // name ambiguous) — the same refusal `builtin_call_return_fact` makes, for the
    // same reason: what runs is not what the engine reflected.
    if cx.index.has_simple_function(name) {
        return;
    }
    let Some(params) = folder.builtin_param_types(name) else { return };
    let null_tolerated = internal_null_tolerated(cx);
    for (i, arg) in call.args.iter().enumerate() {
        // Past the declared list: an extra argument is an arity question, not a
        // type one, and `call.too-many-arguments` for internal targets is its own
        // slice (ADR-0049 §6, still REGISTERED_NOT_YET_EMITTED).
        let Some(bp) = params.get(i) else { break };
        // A variadic binds every argument from here on, so there is no position
        // left to index by — `break`, exactly as the project arm does.
        if bp.variadic {
            break;
        }
        // An out-parameter (`preg_match`'s `$matches`): what PHP requires there is
        // a variable, not a value of a type.
        if bp.by_ref {
            continue;
        }
        // An untyped or unmodeled position (`var_dump`'s `mixed`, `array_map`'s
        // `array`) declines; §9.4 lists the whole set and why each is silence.
        let Some(ty) = bp.ty.as_deref().and_then(builtin_param_native_type) else { continue };
        let param = builtin_param_as_param(bp, ty, arg.span);
        let ty = param.ty.as_ref().expect("set just above");

        let mut native_fired = false;
        let proven = propagated_arg_value(
            cx, folder, &arg.value, env, store, this_exact, enclosing_class, poisoned, in_descent,
            arg.span.start, out,
        )
        .map(|(v, prov, s)| (v, Some(prov), s))
        .or_else(|| {
            // A literal in the source IS the value, so it enters `Verified` with no
            // provenance phrase — the direct pass's shape, reached from here.
            cx.resolve_static_value(&arg.value, enclosing_class)
                .map(|v| (v, None, Stratum::Verified))
        });
        // Proof-layer consumption rule (ADR-0052 §5): an all-`Verified` premise, or
        // silence. The carve-out sits beside the coercion table rather than inside
        // it — `is_type_error` answers what PHP does at a *userland* boundary, and
        // this is the one cell where the internal boundary differs (§9.3).
        if let Some((value, provenance, strat)) = proven
            && strat == Stratum::Verified
            && is_type_error(cx, ty, &value)
            && !(null_tolerated && matches!(value, ArgValue::Null))
            && !object_world_guard_blind(in_descent, ty, &value)
        {
            out.push(cx.diagnostic(
                arg.span.start,
                &value,
                provenance.as_deref(),
                name,
                &param.name,
                ty,
            ));
            native_fired = true;
        }
        // A variable bound to a proven object, and one whose contract lane is a
        // bare `Verified` resource (ADR-0056 §8) — the two non-scalar definite-No
        // branches the project arm carries, on the same guards.
        if !native_fired
            && !poisoned
            && !in_descent
            && let ArgValue::Var(v) = &arg.value
            && store.is_exact(v)
            && let Some(class) = store.class_of(v)
            && cx.object_is_type_error(ty, class)
        {
            out.push(cx.diagnostic(
                arg.span.start,
                &arg.value,
                Some(&format!("holds a {}", simple_class(class))),
                name,
                &param.name,
                ty,
            ));
            native_fired = true;
        }
        if !native_fired
            && !poisoned
            && let ArgValue::Var(v) = &arg.value
            && store_holds_resource(store, v)
            && cx.resource_is_type_error(ty)
        {
            out.push(cx.resource_diagnostic(arg.span.start, v, name, &param.name, ty));
            native_fired = true;
        }
        // The possibly pair (issues #391/#418), where no definite No fired — so the
        // weaker claim never shadows the stronger one about the same argument.
        if !native_fired {
            check_maybe_argument_mismatch(
                cx,
                folder,
                &param,
                name,
                arg.span.start,
                &arg.value,
                env,
                store,
                this_exact,
                enclosing_class,
                poisoned,
                in_descent,
                null_tolerated,
                out,
            );
        }
    }
}
