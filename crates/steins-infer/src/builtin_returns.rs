//! Builtin return facts (ADR-0056 R1): the reflected envelope seeds the value
//! domain and a curated row refines strictly within it. The admission gate is
//! factored into pure functions so every leg is unit-testable without a sidecar;
//! the declared-return floor (ADR-0069) and the shape-builtin rows live here too.

use std::collections::HashMap;

use steins_contract::{ContractTy, normalize};
use steins_domain::{Base, Certainty, Fact, Refinement, ShapeFact, Key as VKey, Val};
use steins_syntax::ArgValue;

use crate::cx::Cx;
use crate::env::{ContractArm, Known, Store, Stratum, array_literal_fact, singleton_fact};
use crate::refine::{flatten_arms, refine_declared_arms, seed_shape_fact};
use crate::walk::value_stratum;
use crate::Folder;
use crate::shape_projection::{
    shape_projection_fact, witnessed_family_fact, witnessed_projection_fact,
};
use crate::transfers::{arg_dispatch_return_fact, transfer_arg_known};

// ---------------------------------------------------------------------------
// Builtin return facts (ADR-0056 R1): the reflected envelope seeds the value
// domain; a curated row refines strictly within it. The admission gate of §2 is
// factored into pure functions so every leg is unit-testable without a sidecar.
// ---------------------------------------------------------------------------

/// Combine a reflected return-type string with an optional curated refinement
/// into the value-domain [`Fact`] to seed (ADR-0056 §1–2), or `None` when nothing
/// representable can be seeded.
///
/// * The **reflected envelope** is `return_type` lowered to a single-base fact
///   (`bool`, `int`, `string`, `float`, or their `?T` nullable form). A multi-base
///   union, a non-scalar, or `mixed` is not representable as one [`Fact`] and
///   yields `None` — the union case belongs to the contract-lane arms (§4).
/// * A **curated refinement** (a phpdoc type string like `int<0, max>`) is
///   admitted only when `minor_matches_pin` holds (the A11 pin, §2), it lowers to
///   the SAME base as the envelope, AND the envelope extensionally subsumes it
///   ([`normalize::subsumes`] `== Yes`). Otherwise the envelope stands alone.
///   Curation may narrow within the envelope; it may never widen or cross bases.
pub(crate) fn admit_return_fact(return_type: &str, curated: Option<&str>, minor_matches_pin: bool) -> Option<Fact> {
    let envelope_ty = steins_contract::lower_str(return_type)?;
    let envelope = envelope_fact(&envelope_ty)?;
    // No curated row, or the minor pin fails: the envelope stands alone.
    let Some(curated) = curated.filter(|_| minor_matches_pin) else {
        return Some(envelope);
    };
    // The curated refinement must lower, be extensionally subsumed by the
    // envelope (curated ⊆ reflected, the §1.2 subset check), and share the
    // envelope's base. Any failure keeps the envelope alone (never widens).
    let refined = steins_contract::lower_str(curated).and_then(|cty| {
        if normalize::subsumes(&envelope_ty, &cty).is_yes() {
            contractty_to_fact(&cty).filter(|f| fact_base(f) == fact_base(&envelope))
        } else {
            None
        }
    });
    Some(refined.unwrap_or(envelope))
}

/// The scalar base of a [`General`]/[`Refined`] fact (`None` for the finite
/// layers, which the return-fact path never produces).
///
/// [`General`]: Fact::General
/// [`Refined`]: Fact::Refined
fn fact_base(f: &Fact) -> Option<Base> {
    match f {
        Fact::General { base, .. } | Fact::Refined { base, .. } => Some(*base),
        // A union has no single base — that is what it is for.
        Fact::Union { .. } => None,
        // The array stratum has no scalar base.
        Fact::Singleton(_) | Fact::OneOf(_) | Fact::Shape { .. } => None,
    }
}

/// Lower a reflected envelope [`ContractTy`] to the single-base value-domain
/// [`Fact`] it seeds, or `None` when not a single representable scalar base: a
/// bare `Base(b)` → `General{b}`, a two-member `?T` union → `General{b, nullable}`.
/// Everything else (multi-base unions, non-scalars, `mixed`) yields `None`.
pub(crate) fn envelope_fact(ty: &ContractTy) -> Option<Fact> {
    match ty {
        ContractTy::Base(b) => Some(Fact::General { base: *b, nullable: false }),
        // **Still the nullable pair only, and now that is a decision** (issue
        // #339). Generalising `Fact::Union` here was tried and reverted: the
        // reflected declaration is coarse by construction (`abs` declares
        // `int|float`), while ADR-0069's curated floor carries the sharp row
        // (`int<1, max>|0|float`). The envelope rung sits ABOVE the floor, so a
        // wider envelope *shadows* the sharper row — 13 nsrt rows regressed from
        // `int<0, max>|float` to `int|float` on exactly that path. Widening waits
        // on whether the floor may refine *within* a union envelope (ADR-0061 §2's
        // question for the type rung), its own decision.
        ContractTy::Union(members) if members.len() == 2 && members.iter().any(|m| matches!(m, ContractTy::Null)) => {
            let base = members.iter().find_map(|m| match m {
                ContractTy::Base(b) => Some(*b),
                _ => None,
            })?;
            Some(Fact::General { base, nullable: true })
        }
        _ => None,
    }
}

/// Fold a declared union's members into one [`Fact`] through the domain's join
/// (issue #339), or `None` if any member does not lift.
///
/// `null` is not a member here but a flag: it lowers to `Fact::Singleton(Null)`
/// and the join folds it into `nullable` on the way, which is the same thing the
/// old two-member special case did by hand.
fn union_envelope(members: &[ContractTy]) -> Option<Fact> {
    let mut acc: Option<Fact> = None;
    for m in members {
        let f = match m {
            ContractTy::Null => Fact::Singleton(Val::Null),
            ContractTy::Base(b) => Fact::General { base: *b, nullable: false },
            ContractTy::IntIn(r) => Fact::refined(Base::Int, Refinement::Int(*r), false),
            ContractTy::StrWith(p) => Fact::refined(Base::String, Refinement::Str(*p), false),
            // Anything else — a class, a shape, a callable, a nested union —
            // is not a scalar arm, so the union has no fact form.
            _ => return None,
        };
        acc = Some(match acc {
            None => f,
            Some(prev) => prev.join(&f)?,
        });
    }
    acc
}

/// Lower a curated refinement [`ContractTy`] to a value-domain [`Fact`] (ADR-0056
/// §1.2), or `None` when not a scalar refinement the domain carries: the base
/// layer, the two Refined refinements (`int<lo, hi>`, string predicates), and a
/// two-member `?T` nullable wrapper. Unions past the nullable pair, and
/// non-scalars, yield `None` — the envelope stands alone.
fn contractty_to_fact(ty: &ContractTy) -> Option<Fact> {
    match ty {
        ContractTy::Base(b) => Some(Fact::General { base: *b, nullable: false }),
        ContractTy::IntIn(r) => Some(Fact::refined(Base::Int, Refinement::Int(*r), false)),
        ContractTy::StrWith(p) => Some(Fact::refined(Base::String, Refinement::Str(*p), false)),
        // An all-`StrWith` intersection is one predicate set (issue #240), folded
        // by `steins_contract::inter_str_preds` — the same fold
        // `steins_contract::to_fact` and the arm speller read, never a second one
        // here. Every other `Inter` still returns `None`: the honest floor.
        ContractTy::Inter(members) => steins_contract::inter_str_preds(members)
            .map(|p| Fact::refined(Base::String, Refinement::Str(p), false)),
        // Any scalar union (issue #339), by the same fold the envelope path uses
        // — the nullable pair is now just its two-member case.
        ContractTy::Union(members) => union_envelope(members),
        _ => None,
    }
}

/// Add null admissibility to a single-base fact (the `?T` curated wrapper). `None`
/// for a finite fact (never produced here).
pub(crate) fn fact_with_null(f: &Fact) -> Option<Fact> {
    match f {
        Fact::General { base, .. } => Some(Fact::General { base: *base, nullable: true }),
        Fact::Refined { base, refinement, .. } => Some(Fact::refined(*base, *refinement, true)),
        Fact::Union { arms, .. } => Fact::union(arms.clone(), true),
        // The curated `?T` wrapper is a scalar path; a shape fact refuses
        // rather than acquiring nullability here.
        Fact::Singleton(_) | Fact::OneOf(_) | Fact::Shape { .. } => None,
    }
}

/// The value-domain fact to seed for a call to builtin `name` at a call site
/// (ADR-0056 R1), or `None` when no fact may be seeded. The call must resolve
/// **uniquely to the builtin**: any project user function sharing the simple name
/// shadows (or makes ambiguous) the builtin, so — exactly as [`Cx::try_fold`]
/// does — a simple-name collision refuses (conservative, never an FP). The fact
/// itself, and the sidecar/monkey-patch/pin gating, come from
/// [`Folder::builtin_return_fact`].
pub(crate) fn builtin_call_return_fact(cx: &Cx, folder: &mut dyn Folder, name: &str) -> Option<Fact> {
    if cx.index.has_simple_function(name) {
        return None;
    }
    folder.builtin_return_fact(name)
}

/// The `Known::bound` provenance a floor-seeded fact carries. A constant rather
/// than a literal at each site because two rungs stamp it and a reader comparing
/// them must see one string.
pub(crate) const CATALOG_FLOOR: &str = "declared in the builtin catalog, unverified";

/// The **declared-return floor** (ADR-0069, issues #73/#79): the bottom rung of the
/// return ladder, seeded from `steins_catalog::declared_return` as a declared-contract
/// **arm list**, every arm `Asserted`.
///
/// It fires exactly where [`builtin_call_return_fact`] yielded `None` for this
/// name — which is *per name*, not per run. `--no-php` (and the browser before
/// php-wasm loads) is only the total case; with a live engine the floor still
/// speaks where that engine is **silent** about a name: an extension the analyzing
/// PHP does not load, a builtin with no declared return type. Where the engine
/// answers, the caller never reaches here, so a static row can never outvote the
/// real thing — the consuming engine may not be the pinned one.
///
/// Three gates, and each is the same gate an existing rung already applies:
///
/// 1. **Project shadow wins** — `has_simple_function` refuses exactly as
///    [`builtin_call_return_fact`] does: a project function of the same simple name
///    shadows (or makes ambiguous) the builtin, and the project's own definition is
///    the better answer.
/// 2. **Version discipline** ([`floor_target_admits`]) — the A11-shaped target gate.
/// 3. **The lowering is the declared-return lowering** — `lower_str` →
///    [`flatten_arms`] → [`refine_declared_arms`] against an empty native list,
///    which is byte for byte the path a project function's `@return` takes at a call
///    site ([`fn_return_arms`], issue #60). One lowering, two provenances
///    (ADR-0069 §2). Issue #79 replaced the #73 `envelope_fact` rung with this one
///    rather than stacking a second: a bare base is a trivial arm set, so the
///    envelope case is subsumed, and a `string|false` row now seeds the same way.
///
/// The stratum is not returned: `refine_declared_arms` over an empty native list
/// marks every arm `Asserted`, and the proof layer's all-Verified premise rule then
/// keeps the fact out of every finding by construction. The absence family never
/// comes here at all — existence is a boot-surface fact, and this table answers only
/// about return types.
/// The **resource-return arms** of a builtin call (ADR-0056 §8): `resource` plus,
/// where the stub declares one, the `false` failure arm — both `Verified`.
///
/// # Why these arms are `Verified` when the declared floor's are not
///
/// ADR-0069's floor is `Asserted` because a `functionMap` row is an unconfirmed
/// claim about the analyzing PHP. These rows cannot disagree with the engine in
/// that way: [`Folder::builtin_resource_return`] admits the row only while this
/// engine declares NO return type for the name. A migrated function
/// (`curl_init` → `CurlHandle|false`) declares one and is refused; a genuine
/// resource producer declares none because the language has no syntax for it.
///
/// The project-shadowing check comes first, as for the floor.
/// Whether `var`'s contract lane says it holds a **resource and nothing else**
/// (ADR-0056 §8) — the single condition under which the argument families may
/// read that lane.
///
/// Three requirements, each ruling out a specific way of being wrong:
///
/// * **exactly one arm.** `resource|false` straight out of `fopen()` is not a
///   proven resource until the `=== false` guard kills that arm.
/// * **that arm is [`ContractTy::Resource`].** Not a supertype, not an `Opaque`
///   that might contain one.
/// * **`Verified`.** ADR-0052 §3 keeps the contract lane away from the proof
///   layer — a lane arm reaching `Asserted` by any route, including a
///   `@return resource` docblock, does not qualify.
///
/// [`fn_return_arms`]: crate::fn_return_arms
pub(crate) fn store_holds_resource(store: &Store, var: &str) -> bool {
    matches!(
        store.contract_arms(var),
        Some([ContractArm { ty: steins_contract::ContractTy::Resource, stratum: Stratum::Verified }])
    )
}

pub(crate) fn builtin_resource_arms(
    cx: &Cx,
    folder: &mut dyn Folder,
    name: &str,
) -> Option<Vec<ContractArm>> {
    if cx.index.has_simple_function(name) {
        return None;
    }
    let may_be_false = folder.builtin_resource_return(name)?;
    let mut arms = vec![ContractArm {
        ty: steins_contract::ContractTy::Resource,
        stratum: Stratum::Verified,
    }];
    if may_be_false {
        arms.push(ContractArm {
            ty: steins_contract::ContractTy::LitBool(false),
            stratum: Stratum::Verified,
        });
    }
    Some(arms)
}

pub(crate) fn builtin_return_floor(cx: &Cx, name: &str) -> Option<Vec<ContractArm>> {
    if cx.index.has_simple_function(name) {
        return None;
    }
    if !floor_target_admits(name, cx.php_target) {
        return None;
    }
    let declared = steins_catalog::declared_return(name)?;
    let arms = flatten_arms(steins_contract::lower_str(declared)?);
    // The resolver is the **identity**, a claim worth stating now that mining
    // admits class rows (`imageloadfont` = `GdFont`).
    //
    // `refine_declared_arms`' resolver exists to turn a *relative* class name in a
    // project docblock into an FQN against the declaring namespace. A functionMap
    // row has no declaring namespace: every class it names is a global builtin FQN
    // as PHP resolves it (`GdFont`, `CurlHandle`, already-qualified `ast\Node`).
    // Running a project namespace resolver over those would MANGLE them (`GdFont`
    // inside `namespace App;` would become `App\GdFont`), so identity is the only
    // correct resolver — and it preserves `ContractTy::Class`'s own normalization
    // (`lower_identifier` strips a leading `\`, case-folds), matching the
    // generation-time countersign. Same argument for a class inside an array row's
    // element type.
    refine_declared_arms(&[], arms, &|n: &str| n.to_owned())
}

/// The value-lane seed a floor arm list contributes: the single value-domain
/// [`Fact`] its arms denote, or `None` when they denote more than one.
///
/// Both abstract layers are reachable from here, through the lowering each already
/// owns — [`seed_shape_fact`] for an array arm (ADR-0062 S3), [`contractty_to_fact`]
/// for a scalar one. A builtin row and a project function's `@return array{…}` are
/// the same arm list by the time they arrive here, so the array vocabulary needed
/// no new seam.
///
/// The single-fact rule is why #73's pins survive every widening unchanged. A
/// one-arm row binds `$r = f(...)` to one fact, premising the contract-layer
/// return check. A genuinely multi-arm row (`string|false`, `false|array`) has no
/// single fact — the value domain carries no union layer over either vocabulary —
/// so it stays in the arm lane alone.
///
/// A `?T` **scalar** pair is one fact (`nullable` is a side flag), which is how
/// `?string` rows keep their #73 rendering. A `?array{…}` row is **not**:
/// [`fact_with_null`] refuses a shape, so it lives in the arm lane alone — a
/// designed refusal, the FP-safe side (the arms still carry the null).
///
/// A **class** row (`GdFont`, `?GdFont`, bare `object`) declines for a stronger
/// reason: the value domain has no object inhabitant at all (ADR-0035/0038), so
/// there is no fact to seed. Both lowerings say so independently
/// ([`contractty_to_fact`] has no `Class`/`ObjectAny` arm, `to_shape_fact` has
/// none either) — a class row is **arm-lane only**, unconditionally.
pub(crate) fn floor_value_fact(arms: &[ContractArm]) -> Option<Fact> {
    let (nulls, rest): (Vec<ContractArm>, Vec<ContractArm>) =
        arms.iter().cloned().partition(|a| matches!(a.ty, ContractTy::Null));
    let [only] = rest.as_slice() else { return None };
    let fact = match seed_shape_fact(&rest) {
        Some(shape) => shape,
        None => contractty_to_fact(&only.ty)?,
    };
    if nulls.is_empty() { Some(fact) } else { fact_with_null(&fact) }
}

/// The floor's version gate (ADR-0069 §3, A11-shaped): whether the project's
/// declared PHP target agrees with the minor the mined row was stated at.
///
/// `steins_catalog::declared_return_changed_at` is the change oracle — a
/// `Some(m)` says the builtin's declared return type last moved at minor `m`, so
/// the mined row is only known good for a target lying **wholly at or above** `m`
/// (stricter than "does not straddle": a target entirely below the boundary is
/// just as wrong).
///
/// An **undeclared target admits**: the row is Asserted anyway, and its consumers
/// tolerate that grade. A name the oracle does not list admits unconditionally.
pub(crate) fn floor_target_admits(name: &str, target: Option<&steins_db::PhpTarget>) -> bool {
    let Some(boundary) = steins_catalog::declared_return_changed_at(name) else {
        return true;
    };
    match target {
        Some(t) => t.floor >= boundary,
        None => true,
    }
}

/// The **argument-dependent** return rung (ADR-0061 §1) for the two ADR-0062 §4
/// transfers that read the abstract array stratum: `count($x)` and
/// `array_is_list($x)`. `None` — decline — is a first-class outcome, and the
/// caller falls through to the argument-insensitive envelope rung.
///
/// The rule fires only on a single-argument call whose one argument is a bare
/// variable carrying a non-nullable [`Fact::Shape`] — or a [`Fact::Singleton`]
/// array, lifted to a shape ([`ShapeFact::lift`]) once the value lane's own
/// order-dependent projections (issue #118) have first refused the name, so a
/// literal array is never worse off than a declared one. A nullable base
/// declines, a second argument declines (`count($x, COUNT_RECURSIVE)` counts
/// something else), and a project function shadowing the name declines through
/// [`builtin_call_return_fact`]'s own check.
///
/// **The admission gate is ADR-0061 §2's, unweakened**: seeded only when the
/// sidecar-backed envelope for this name exists AND the fact is extensionally
/// inside it (`envelope ⊔ out == envelope`). A rule claiming something the
/// running engine's own declaration disowns is discarded, never demoted.
///
/// **Stratum is ADR-0061 §3's derivation clause**: the output carries the
/// argument fact's stratum, `Asserted` for a declared shape — so
/// `count($declaredShape)` can never premise a proof-layer finding (A-G9's
/// corollary), while `count()` of a *proven* array folds to a Singleton unchanged.
pub(crate) fn shape_builtin_return_fact(
    cx: &Cx,
    folder: &mut dyn Folder,
    name: &str,
    args: &[ArgValue],
    env: &HashMap<String, Known>,
    store: Option<&Store>,
    poisoned: bool,
) -> Option<(Fact, Stratum)> {
    if poisoned {
        return None;
    }
    // The argument-DISPATCHED family (ADR-0064 seam ii, DR3) sits at the same
    // seam, one step earlier: its rules read arguments this rung's single-shape
    // pattern cannot even bind. Declining falls straight through to the shape
    // rung below, unchanged.
    if let Some(out) = arg_dispatch_return_fact(cx, folder, name, args, env, store) {
        return Some(out);
    }
    // **The subject binds by what it resolves to, not by how it was spelled**
    // (issue #328 L1). A bare variable reads the env; an array written *at the
    // call site* resolves through the seeding ladder, so
    // `count(['a' => $x, 'b' => $x])` is no worse than the two-statement spelling.
    //
    // Deliberately only these three forms — every other spelling would need
    // resolving to find out it is not an array, and most calls reaching this
    // rung are not in the family at all.
    //
    // The `Call` form is what makes a projection *of* a projection compose
    // (`array_values(array_keys([…]))`, issue #329). It terminates because each
    // level strips one call from a finite expression.
    let seeded;
    let (subject_fact, subject_stratum) = match args {
        [ArgValue::Var(var), ..] => {
            let known = env.get(var)?;
            (known.fact.as_ref()?, known.stratum)
        }
        [ArgValue::Array(items), ..] => {
            seeded = cx
                .resolve_literal(&args[0], env, poisoned, folder)
                .and_then(|lit| singleton_fact(&lit, cx.php_minor))
                .map(|f| (f, value_stratum(&args[0], env, store)))
                .or_else(|| array_literal_fact(cx, folder, items, env, poisoned, store))?;
            (&seeded.0, seeded.1)
        }
        [call @ ArgValue::Call(..), ..] => {
            let (lit, strat) = cx.resolve_literal_strat(call, env, poisoned, folder)?;
            seeded = (singleton_fact(&lit, cx.php_minor)?, strat);
            (&seeded.0, seeded.1)
        }
        _ => return None,
    };
    let [_, rest @ ..] = args else { return None };

    // **The value lane's own privilege** (ADR-0062 §2): a subject whose fact is a
    // witnessed `Val::Array` carries true insertion order, so the order-dependent
    // projection may be *executed* rather than widened. Taken before the shape
    // binding below, since a `Singleton` is not a `Fact::Shape`.
    //
    // A name that projection declines is not a dead end: the same entries LIFT to
    // a `ShapeFact` (issue #262) and fall through to the rung below exactly as a
    // seeded `Fact::Shape` would — a literal array only sharpens what that rung
    // can answer.
    let lifted;
    let shape: &ShapeFact = match subject_fact {
        Fact::Singleton(Val::Array(entries)) => {
            if let Some(out) = witnessed_projection_fact(cx, folder, name, entries, args, env, store) {
                return Some((out, derivation_stratum(cx, folder, args, env, store, subject_stratum)));
            }
            lifted = ShapeFact::lift(entries);
            &lifted
        }
        Fact::Shape { shape, nullable: false } => shape.as_ref(),
        _ => return None,
    };

    // **The positional projections, executed** (issue #328). A shape that
    // witnessed its own construction carries a realizable key sequence, so the
    // family may run over it instead of taking the key-set widening below. A
    // shape that witnessed nothing falls straight through (ADR-0062 §7's
    // declined import, stays declined).
    if let Some(order) = shape.witnessed_order() {
        let entries: Vec<(VKey, Option<Fact>)> = order
            .iter()
            .filter_map(|k| shape.field(k).map(|(_, _, slot)| (k.clone(), slot.clone().map(|f| *f))))
            .collect();
        if entries.len() == order.len()
            && let Some(out) =
                witnessed_family_fact(cx, folder, name, &entries, args, env, store)
        {
            return Some((out, derivation_stratum(cx, folder, args, env, store, subject_stratum)));
        }
    }

    let out = if rest.is_empty()
        && (name.eq_ignore_ascii_case("count") || name.eq_ignore_ascii_case("sizeof"))
    {
        let range = shape.count_range();
        if range.lo() == range.hi() {
            // The one place a shape has an exact size: a sealed, all-required
            // shape (ADR-0062 §4, mirroring PHPStan's own exactness).
            Fact::Singleton(Val::Int(range.lo()))
        } else {
            Fact::refined(Base::Int, Refinement::Int(range), false)
        }
    } else if rest.is_empty() && name.eq_ignore_ascii_case("array_is_list") {
        match shape.is_list {
            // The answer IS the denotational flag (§4's row) — no structural
            // inspection, and `Maybe` answers nothing.
            Certainty::Yes => Fact::Singleton(Val::Bool(true)),
            Certainty::No => Fact::Singleton(Val::Bool(false)),
            Certainty::Maybe => return None,
        }
    } else {
        // The positional-projection family (ADR-0062 S7) carries its own
        // admission gate — the reflected *declaration*, since its results are not
        // facts the scalar envelope path can name.
        let fact = shape_projection_fact(cx, folder, name, shape, args, env, store)?;
        return Some((fact, derivation_stratum(cx, folder, args, env, store, subject_stratum)));
    };

    let envelope = builtin_call_return_fact(cx, folder, name)?;
    (envelope.join(&out).as_ref() == Some(&envelope)).then_some((out, subject_stratum))
}

/// **ADR-0061 §3's derivation clause over every argument the call passes**: `min`
/// of the subject's own stratum and each other argument's.
///
/// For single-argument arms this is the subject's stratum unchanged; the
/// argument-reading arm (`array_slice`, issue #118) is where an offset read out
/// of a docblock-claimed binding can lower it. Computed **after** a rule has
/// produced a fact and never before, since most calls arriving here are not in
/// this family at all.
fn derivation_stratum(
    cx: &Cx,
    folder: &mut dyn Folder,
    args: &[ArgValue],
    env: &HashMap<String, Known>,
    store: Option<&Store>,
    subject: Stratum,
) -> Stratum {
    args.iter().fold(subject, |acc, v| {
        acc.min(
            transfer_arg_known(cx, folder, v, env, store)
                .map_or_else(|| value_stratum(v, env, store), |(_, s)| s),
        )
    })
}

/// **The admission gate both symbolic-transfer rungs share** (ADR-0061 §2): the
/// running engine's own reflected *declaration* must be the one the rule was
/// written against, and — where the declaration pins too little on its own —
/// its arity must be too (ADR-0064 Amendment B).
///
/// Three refusals, each an existing rung already applied by hand before issue
/// #118 gave them one home:
///
/// 1. **A project function shadowing the simple name** is not the builtin.
/// 2. **A silent engine withholds.** No sidecar, an A9 monkey-patch, or a name
///    the engine declares nothing about: withheld rather than trusted.
/// 3. **A moved signature withholds.** An engine answering no arity withholds
///    exactly as a silent declaration does; a non-pinned arity means the
///    signature has moved and the rule is stale.
pub(crate) fn transfer_declaration_admits(
    cx: &Cx,
    folder: &mut dyn Folder,
    name: &str,
    declared: &[&str],
    arity: Option<(u32, u32)>,
) -> bool {
    if cx.index.has_simple_function(name) {
        return false;
    }
    let Some(reflected) = folder.builtin_return_type(name) else { return false };
    if !declared.iter().any(|d| d.eq_ignore_ascii_case(&reflected)) {
        return false;
    }
    match arity {
        None => true,
        Some(pin) => folder.builtin_param_counts(name) == Some(pin),
    }
}
