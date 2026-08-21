//! Reading the generics carry as a type expression (ADR-0032's 2026-08-15
//! amendment, issue #362) — PHPStan's `getTemplateType(owner, name)` read out of
//! the carry by position, plus the callable / obligation violations over it.

use std::collections::HashMap;

use steins_domain::Certainty as Tri;
use steins_contract::{ContractTy, normalize};
use steins_domain::{Certainty, Key as VKey};
use steins_phpdoc::{Type as PType, Variance};
use steins_phpdoc::ast::{ArrayShapeKind, TypeKind as PKind};
use steins_syntax::{
    ArgValue, ClosureRef, NamedArg, NativeType, NormKey, Param, ScalarType, TypeMember,
};

use crate::{
    ClosureTarget, Cx, Diagnostic, FnResolution, Folder, Known, NEVER_PARAM_REACHABLE_ID,
    PARAM_MISMATCH_ID, Store, Sym, arg_abstract_fact, class_template_names, contract_touches_class,
    describe_fact, is_pure_class_contract, phpdoc_object_guard_blind, render_contract_arms,
    rendered_cval,
};
use crate::contract::{
    CArg, CVal, Envelopes, GenericCarry, accepts, accepts_class_generic, class_key, combine,
    declared_carrier, literal_contract,
};

// ---------------------------------------------------------------------------
// Reading the generics carry as a type expression (ADR-0032's 2026-08-15
// amendment, issue #362) — PHPStan's `getTemplateType(owner, name)`.
//
// Acceptance asks whether a value inhabits a declared argument; these ask what
// is carried at a named position. Same state, opposite direction, so no verdict
// and no variance gate lives here: reading an argument out by position asks
// nothing about substitution (the same reason #361's declared-side projection
// does not gate on it either).
// ---------------------------------------------------------------------------

/// The carry edge owned by `owner_fqn`, if any — the owner lookup shared by
/// acceptance ([`accepts_class_generic`]) and the reader ([`get_template_type`]).
pub(crate) fn carry_for_owner<'c>(carries: &'c [GenericCarry], owner_fqn: &str) -> Option<&'c GenericCarry> {
    let want = class_key(owner_fqn);
    carries.iter().find(|c| class_key(&c.owner) == want)
}

/// One carry argument together with the context its class names were written in
/// — what [`get_template_type`] hands back.
///
/// The site travels with the argument for the same reason [`accepts_carried_ty`]
/// takes one: a [`CArg::Ty`] holds names spelled against the *declaring* file's
/// namespace scope, and reading them anywhere else would name a different class.
/// `None` is a value carry, which needs no resolution context — its class is
/// already an FQN.
pub(crate) struct CarriedArg<'c> {
    pub(crate) arg: &'c CArg,
    pub(crate) site: Option<(usize, u32)>,
}

/// The argument `carries` holds for the `@template` named `template_name` on
/// `owner_fqn` — PHPStan's `Type::getTemplateType`, over Steins' carry.
///
/// The owner's edge, then the position `template_name` holds in that owner's own
/// `@template` list, then the argument sitting there. `None` — silence, never a
/// guess — when no edge is owned by that class, when the owner declares no
/// templates or not this one, or when the carry's arity disagrees with the
/// declared list, which is the same all-or-nothing alignment rule the carry is
/// built under.
///
/// The name is matched exactly first, then case-insensitively: the same
/// concession [`Cx::project_template_type`] makes, and folding case can only ever
/// pick the template the author plainly meant.
pub(crate) fn get_template_type<'c>(
    cx: &Cx,
    carries: &'c [GenericCarry],
    owner_fqn: &str,
    template_name: &str,
) -> Option<CarriedArg<'c>> {
    let carry = carry_for_owner(carries, owner_fqn)?;
    let names = class_template_names(cx, owner_fqn);
    if names.is_empty() || names.len() != carry.args.len() {
        return None;
    }
    let i = names
        .iter()
        .position(|n| n == template_name)
        .or_else(|| names.iter().position(|n| n.eq_ignore_ascii_case(template_name)))?;
    Some(CarriedArg { arg: carry.args.get(i)?, site: carry.site })
}

/// The carries a carry argument itself holds — the **second hop** of a
/// `template-type<T, Owner, 'TName'>` read, and the only hop after the first.
///
/// A value carry holding an object contributes that object's *own* carries,
/// whichever provenance [`Cx::infer_generic_carry`] chose for it when the value
/// was proven. A declared class contributes its inheritance edges, read through
/// the index. Anything else — a scalar, an array, a resource, a carried type
/// that is not a plain class — carries nothing, so the read declines.
///
/// One level, per ADR-0032: the subject asks for one hop and gets one. Following
/// a second edge would mean substituting through a generic intermediate, which is
/// wrong rather than merely incomplete.
pub(crate) fn template_arg_carries(cx: &Cx, arg: &CarriedArg<'_>) -> Vec<GenericCarry> {
    match arg.arg {
        CArg::Val(CVal::Object(_, carries)) => carries.clone(),
        CArg::Ty(steins_contract::ContractTy::Class(name)) => {
            let Some((file, off)) = arg.site else { return Vec::new() };
            cx.inheritance_edges(&cx.resolve_pclass(file, off, name))
        }
        _ => Vec::new(),
    }
}

/// The declared type a carry argument denotes, for the lane that judges declared
/// types — `None` when the contract lane has no way to say it.
///
/// A carried **type** is already one. A carried **object** becomes membership of
/// its class, fully qualified so that resolving it again at the reading site is a
/// no-op rather than a re-namespacing. A carried **scalar** becomes its literal
/// arm, through the same [`literal_contract`] mapping every other declared-literal
/// consumer uses. An array or a resource declines: the first has no `ContractTy`
/// that states the carried value without inventing one, the second has no type at
/// all.
pub(crate) fn carg_contract_ty(arg: &CArg) -> Option<ContractTy> {
    match arg {
        CArg::Ty(t) => Some(t.clone()),
        CArg::Val(CVal::Scalar(v)) => literal_contract(v),
        CArg::Val(CVal::Object(class, _)) => {
            Some(ContractTy::Class(format!("\\{}", class_key(class))))
        }
        CArg::Val(CVal::Array(_) | CVal::Resource) => None,
    }
}

/// The declared variance of each class-level `@template` of `owner`, in declaration
/// order. Empty when the class is unresolvable or declares none — an absent entry
/// reads as [`Variance::Invariant`], the only reading that can produce a verdict.
pub(crate) fn template_variances(cx: &Cx, owner: &str) -> Vec<Variance> {
    cx.find_class(owner)
        .and_then(|(_, cd)| cd.docblock.as_deref())
        .map(steins_phpdoc::scan_template_decls)
        .unwrap_or_default()
        .iter()
        .map(|d| d.variance)
        .collect()
}

/// Judge a **declared** type argument against a **carried type** one — the
/// type-vs-type face of the argument half, reached only from an inheritance edge
/// (issue #294).
///
/// Delegates to [`steins_contract::subsumes`] ("does every inhabitant of `b`
/// inhabit `a`", ADR-0071 §2.1); no second relation introduced. Two gates keep it
/// FP-safe:
///
/// - A carried type mentioning an **unresolvable class name** stays silent (same
///   `is_known_class` safety valve [`accepts_class_name`] applies).
/// - `subsumes` carries no class hierarchy, so a cross-class position answers
///   `Maybe`.
///
/// [`accepts_class_name`]: crate::contract::accepts_class_name
pub(crate) fn accepts_carried_ty(
    cx: &Cx,
    site: Option<(usize, u32)>,
    declared: &PType,
    carried: &steins_contract::ContractTy,
) -> Tri {
    let Some((file, off)) = site else { return Tri::Maybe };
    if names_unknown_class(cx, file, off, carried) {
        return Tri::Maybe;
    }
    steins_contract::normalize::subsumes(&steins_contract::lower(declared), carried)
}

/// Whether `cty` mentions a class name that resolves to no known class in the
/// docblock's own file context — the silence gate of [`accepts_carried_ty`].
pub(crate) fn names_unknown_class(cx: &Cx, file: usize, off: u32, cty: &steins_contract::ContractTy) -> bool {
    use steins_contract::ContractTy as C;
    match cty {
        C::Class(n) => !cx.is_known_class(&cx.resolve_pclass(file, off, n)),
        C::Union(ms) | C::Inter(ms) => ms.iter().any(|m| names_unknown_class(cx, file, off, m)),
        C::ListOf { elem, .. } => names_unknown_class(cx, file, off, elem),
        C::MapOf { key, val, .. } => {
            names_unknown_class(cx, file, off, key) || names_unknown_class(cx, file, off, val)
        }
        C::IterableOf { key, val } => {
            names_unknown_class(cx, file, off, key) || names_unknown_class(cx, file, off, val)
        }
        _ => false,
    }
}

/// Membership for an `array`/`list` generic (per phpstan#14939): a value is a list
/// iff its normalized keys are exactly `0..n-1` in order; element (and, for
/// `array<K, V>`, key) membership is checked recursively; an uncertain element
/// makes the whole check `Maybe` (silent).
#[allow(clippy::too_many_arguments)]
pub(crate) fn check_arraylike(
    cx: &Cx,
    cfile: usize,
    coff: u32,
    entries: &[(NormKey, CVal)],
    key_ty: Option<&PType>,
    val_ty: &PType,
    require_list: bool,
    non_empty: bool,
) -> Tri {
    if non_empty && entries.is_empty() {
        return Tri::No;
    }
    if require_list && !is_list_shaped(entries) {
        return Tri::No;
    }
    let mut r = Tri::Yes;
    for (k, cv) in entries {
        if let Some(kt) = key_ty {
            r = combine(r, accepts(cx, cfile, coff, kt, &normkey_cval(k)));
            if r == Tri::No {
                return Tri::No;
            }
        }
        r = combine(r, accepts(cx, cfile, coff, val_ty, cv));
        if r == Tri::No {
            return Tri::No;
        }
    }
    r
}

/// Whether normalized `entries` form a list: keys exactly `0, 1, …, n-1` in order.
fn is_list_shaped(entries: &[(NormKey, CVal)]) -> bool {
    entries
        .iter()
        .enumerate()
        .all(|(i, (k, _))| matches!(k, NormKey::Int(n) if *n == i as i64))
}

/// A normalized key as a scalar [`CVal`] (for key membership).
fn normkey_cval(k: &NormKey) -> CVal {
    match k {
        NormKey::Int(i) => CVal::Scalar(ArgValue::Int(*i)),
        NormKey::Str(s) => CVal::Scalar(ArgValue::Str(s.clone())),
    }
}

/// Membership for an array-shape / list-shape (per phpstan#14939): `array{…}` is an
/// order-agnostic required-key map (optional `?` keys may be absent; sealed unless
/// `…`); `list{…}` is positional. A missing required key, a definite element-type
/// violation, an extra key in a sealed shape, or an extra key/value violating the
/// unsealed tail contract → `No`. An unresolvable shape key (a const-fetch) makes
/// the whole check `Maybe`.
///
/// **One acceptance relation** (ADR-0030's no-second-relation discipline, ADR-0062
/// §5). The structural rules are *not* implemented here: this lowers the proven
/// value's keys into the domain's key vocabulary, hands `steins-contract` the
/// declared shape, and lets [`steins_contract::shape_verdict`] — the same code the
/// fact path's `admits_shape` runs — decide. Only the leaf judgment stays local,
/// because the proven lane's values include objects (judged through the is-a
/// oracle) which the value domain cannot express. The divergence this convergence
/// removed: the tail **key** contract went unchecked here, so `['a' => 1, 9 => 2]`
/// passed `array{a: int, ...<string, int>}` on this path while the fact path
/// rejected it.
pub(crate) fn accepts_shape(cx: &Cx, cfile: usize, coff: u32, shape: &steins_phpdoc::ast::ArrayShape, v: &CVal) -> Tri {
    let CVal::Array(entries) = v else { return Tri::No };
    // An unresolvable shape key (const-fetch) → no verdict.
    let Some(keys) = steins_contract::shape_keys(shape) else { return Tri::Maybe };
    let spec = steins_contract::ShapeSpec {
        list: matches!(shape.kind, ArrayShapeKind::List | ArrayShapeKind::NonEmptyList),
        sealed: shape.sealed,
        non_empty: matches!(
            shape.kind,
            ArrayShapeKind::NonEmptyArray | ArrayShapeKind::NonEmptyList
        ),
        fields: keys
            .into_iter()
            .zip(&shape.items)
            .map(|(k, item)| (k, item.optional, &item.value))
            .collect(),
        tail: shape.unsealed.as_ref().map(|u| (u.key.as_deref(), &*u.value)),
    };
    let items: Vec<(VKey, &CVal)> =
        entries.iter().map(|(k, cv)| (domain_key(k), cv)).collect();
    steins_contract::shape_verdict(
        &spec,
        &items,
        &mut |ty, cv| accepts(cx, cfile, coff, ty, cv),
        &mut |ty, k| accepts(cx, cfile, coff, ty, &key_cval(k)),
    )
}

/// A proven array's normalized key in the domain's key vocabulary (the shared
/// acceptance relation speaks [`VKey`], the trace IR speaks [`NormKey`]).
pub(crate) fn domain_key(k: &NormKey) -> VKey {
    match k {
        NormKey::Int(i) => VKey::Int(*i),
        NormKey::Str(s) => VKey::Str(s.clone()),
    }
}

/// A runtime array key as a proven scalar value — the subject of an unsealed
/// tail's key contract.
fn key_cval(k: &VKey) -> CVal {
    match k {
        VKey::Int(i) => CVal::Scalar(ArgValue::Int(*i)),
        VKey::Str(s) => CVal::Scalar(ArgValue::Str(s.clone())),
    }
}

/// The phpdoc contract-acceptance check for one argument at a call site. Runs only
/// when the native check did **not** fire at this site (no double-report). Reports
/// `phpdoc.param-mismatch` iff the proven value provably does not inhabit the
/// `@param` type. `cfile`/`coff` locate the callee's docblock context (class-name
/// resolution). Returns nothing for `Maybe`/`Yes`.
///
/// # Assertion-helper exemption (ADR-0030)
///
/// A function/method whose docblock carries an assertion tag (`@phpstan-assert`
/// and its `-if-true`/`-if-false`/negated variants) targeting parameter `$x` is an
/// **assertion helper for `$x`**: its `@param` for `$x` states a *post*-condition
/// the helper establishes, not a precondition callers must satisfy — such a helper
/// is meant to be called with a *wider* value and narrow it. So
/// `phpdoc.param-mismatch` is skipped for that parameter, for all three assert
/// kinds and the negated form.
///
/// Scope, deliberately narrow: other parameters are still checked; `@return`
/// checking is unaffected; native runtime checks are unaffected (a real runtime
/// gate fires regardless, and firing first already suppresses this check).
///
/// This slice does **not** apply the asserted type to the caller's environment
/// after the call (a branch-analysis capability landing with the structured trace
/// tree); it only suppresses the incorrect precondition reading.
#[allow(clippy::too_many_arguments)]
pub(crate) fn check_phpdoc_param(
    cx: &Cx,
    folder: &mut dyn Folder,
    envelopes: &Envelopes,
    param: &Param,
    cfile: usize,
    coff: u32,
    callee: &str,
    arg_offset: u32,
    value: &ArgValue,
    env: &HashMap<String, Known>,
    store: &Store,
    poisoned: bool,
    in_descent: bool,
    out: &mut Vec<Diagnostic>,
) {
    // Assertion-helper exemption (see the doc comment above): this parameter's
    // `@param` is a post-condition, so a call-site argument cannot violate it.
    if envelopes.is_assert_target(&param.name) {
        return;
    }
    let Some(ty) = envelopes.param(&param.name) else { return };

    // Sentinel-parameter carve-out (ADR-0088 §4, issue #428): `@param never` is an
    // explicit reachability claim, not an ordinary declared contract — `never` is
    // uninhabited, so [`admits_fact`]/[`accepts`] read `ContractTy::Never` as a
    // blanket `No` and every argument would trivially "violate" it below. That is
    // the wrong id (the remedy for a bad argument is "fix the call"; the remedy
    // here is "your case analysis upstream is incomplete") and the wrong grade
    // (the ordinary path asks the VERIFIED type, but a `match`/`if`-`elseif` chain
    // narrows the Asserted arm lane, which subtraction CAN empty). One id must not
    // carry two remedies (ADR-0088 §4), so `never` leaves this check
    // entirely and asks [`check_never_sentinel`]'s question instead.
    if matches!(steins_contract::lower(ty), ContractTy::Never) {
        check_never_sentinel(
            cx, folder, ty, &param.name, callee, arg_offset, value, env, store, poisoned, out,
        );
        return;
    }

    // ADR-0063 P3 — the refined callable spellings' obligations, propagation-pass
    // lane. A variable bound to a PROVEN closure value carries no scalar fact
    // (`Known::closure` sets `fact: None`), so neither lane below can see it; the
    // obligation is judged here against the closure's definition, exactly as
    // [`check_callable_arg`] judges one written directly in argument position.
    //
    // Restricted to a non-descent site on purpose: a `ClosureVal` records only its
    // definition *offset*, and the fixpoint's closure symbol is file-keyed —
    // outside a descent, env and `cx` are the same file by construction; inside
    // one they can differ. Documented ceiling rather than a guess.
    if !in_descent
        && let ArgValue::Var(name) = value
        && let Some(cv) = env.get(name).and_then(|k| k.closure.as_ref())
        && let ContractTy::CallableTy { obl, .. } = steins_contract::lower(ty)
        && !obl.is_bare()
        && let Some(violation) = callable_obl_violation(cx, obl, &cv.target)
    {
        push_obligation_diag(cx, violation, callee, ty, &param.name, arg_offset, out);
        return;
    }

    let param_name = &param.name;
    let rendered = match cx.resolve_cval(value, env, store, poisoned, folder) {
        Some(cv) => {
            // A parameter nullable by its native type, or implicitly nullable via a
            // `= null` default, accepts `null` regardless of a non-nullable
            // `@param` spelling — PHP/PHPStan honor this.
            if matches!(cv, CVal::Scalar(ArgValue::Null))
                // ADR-0043 stage 1: consult native nullability only for scalar-value
                // types — `?Foo` (object-bearing) contributes no signal here.
                && (param.has_null_default
                    || param.ty.as_ref().is_some_and(|t| t.nullable && !t.has_instance()))
            {
                return;
            }
            if accepts(cx, cfile, coff, ty, &cv) != Tri::No {
                return;
            }
            // ADR-0043 stage 4: a class-touching verdict is guard-blind inside a
            // binding descent (mirror of `object_world_guard_blind`). Scalar-vs-
            // scalar phpdoc checks stay live.
            if phpdoc_object_guard_blind(in_descent, ty, Some(&cv)) {
                return;
            }
            rendered_cval(&cv)
        }
        // Abstract-fact path (Feature E, ADR-0030/0035): an argument resolving to
        // an abstract fact (not a proven value) is judged by the domain's **set**
        // acceptance via `steins_contract::admits_fact`. Only a definite `No`
        // reports; `Maybe` is silent.
        None => {
            // Before it, the **argument half** of a declared `Class<A, …>` for an
            // argument bound to a NON-exact heap object — a declared parameter seed
            // above all (ADR-0032's 2026-08-16 amendment, issue #388).
            // `resolve_cval` answers `None` there on purpose: its `CVal::Object`
            // licenses the bare-class path's No-side `is_a`, which a lower bound
            // would make unsound (audit G1). The argument half needs no such
            // licence — `accepts_class_generic` gates on the **Yes** side of is-a,
            // which every descendant of the proven class satisfies, and its only
            // `No` comes from a carried argument that provably violates a declared
            // one. So the class half stays `Maybe`, exactly as tier 3 leaves it.
            if let Some((class, carries)) = declared_carrier(value, store, poisoned)
                && let PKind::Generic { base, args } = &ty.kind
            {
                let cv = CVal::Object(class, carries);
                if accepts_class_generic(cx, cfile, coff, base, args, &cv) != Tri::No
                    || phpdoc_object_guard_blind(in_descent, ty, Some(&cv))
                {
                    return;
                }
                // The variable's own spelling, not [`rendered_cval`]'s `new C()`:
                // nothing here was constructed at this site, and the reader's next
                // move is to look at where `$b` was declared.
                value.render()
            } else {
                let Some(fact) = arg_abstract_fact(value, env, poisoned) else { return };
                let cty = steins_contract::lower(ty);
                // ADR-0043 stage 4 — the class valve. A class-touching contract used
                // to stay silent against every fact; it opens for exactly one sound
                // case: a **pure class contract of known classes** against a definite
                // scalar fact (the abstract-fact domain is scalar-only,
                // ADR-0035/0038, and a scalar is never a class member — pure set
                // membership, no coercion). Stays shut for an unknown identifier (may
                // be a `@template`/`@phpstan-type` alias) and, like the proven path,
                // inside a descent.
                let open_class_valve = is_pure_class_contract(cx, cfile, coff, ty)
                    && !phpdoc_object_guard_blind(in_descent, ty, None);
                if contract_touches_class(&cty) && !open_class_valve {
                    return;
                }
                if steins_contract::admits_fact(&cty, fact) != Certainty::No {
                    return;
                }
                describe_fact(fact)
            }
        }
    };
    let pos = cx.tree().position(arg_offset);
    let message = format!(
        "argument {rendered} to {callee}() violates declared @param {ty} ${param_name} — declared contract violation",
    );
    out.push(Diagnostic {
        id: PARAM_MISMATCH_ID,
        path: cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        message,
        facet: None,
        fix: None,
    });
}

/// The sentinel-parameter question (ADR-0088 §4, issue #428), asked in place of
/// [`check_phpdoc_param`]'s ordinary declared-contract check where `ty` lowers to
/// `never` — see the carve-out at that call site. Not "does this argument satisfy
/// `never`" (nothing does); "does the argument's own MOST-REFINED DECLARED type
/// still admit a value here" — the `@param`-refined domain where a docblock
/// narrows the argument's native declaration, the native declaration alone
/// otherwise (ADR-0037 trust order), evaluated on the CURRENT branch.
///
/// Two sources, in the same priority [`check_phpdoc_param`]'s proven-value lane
/// uses:
///
/// - a **proven value** ([`Cx::resolve_cval`] — a literal, a proven scalar/object/
///   resource) is trivially non-empty and always reports, named by its own
///   rendering;
/// - a bare **`$var`** reports iff [`Store::contract_arms`] still holds a
///   non-empty arm list for it AND [`Store::contract_narrowed`] says a
///   subtraction actually landed on it on this path: the seeded declared arms
///   (native, refined by `@param` where one narrows it) minus every guard
///   subtraction the current branch proved. Reading the arm lane rather than the
///   value lane is the whole fix — a `@param 1|2` over a native `int` never
///   reaches the value lane at all (`seed_refined_scalar_fact` declines to
///   overwrite a `General` base with a `OneOf`), so the narrowed domain lives
///   here and nowhere else.
///
/// # The proven-narrowing rule (issue #428 amendment, audited)
///
/// A non-empty arm list is NOT by itself evidence of reachability — it is also
/// what an **untouched** lane looks like, and the two are indistinguishable
/// without a separate mark. A guard the arm lane cannot yet model — enum-case
/// identity, boolean-literal equality (issue #429 teaches these; not this
/// slice) — leaves the seeded arms exactly as wide as they started, so an
/// exhaustive `if`/`elseif` over every enum case or both booleans would report
/// "still reaches" on a lane that was never actually subtracted: a manufactured
/// finding, the one thing ADR-0002 forbids outright. [`Store::contract_narrowed`]
/// is the bit that tells the two apart — set only where an arm demonstrably died
/// or shrank ([`subtract_contract_lane`], [`subtract_pred_arms`],
/// [`subtract_shape_arms`]), so an un-narrowed lane reads as ignorance about
/// reachability, not evidence for it. One casualty, accepted: a completely
/// unguarded call (`assertNever($foo)` with no chain above it) now declines too
/// — its lane is non-empty and un-narrowed by the same test, even though the
/// call is in fact always reachable. Losing that cell is the trade for never
/// firing on the enum/bool cells; the partial-coverage cells (a union missing an
/// arm, `1|2` missing a value) are unaffected — their residue is precisely an
/// arm a subtraction proved dead around it, so the mark is set.
///
/// An **absent** lane is the silent case regardless, and it deliberately
/// conflates three situations this check cannot tell apart: a lane subtraction
/// emptied (the `elseif` chain's own `1|2` narrowed to nothing — the case the id
/// exists to stay quiet about), a lane never seeded (an argument with no
/// declared type to refine), and a lane invalidated (a by-reference call between
/// the guard and the sentinel drops it). Only the first is a proven emptiness;
/// the other two are ignorance. Conflating them costs findings and never
/// manufactures one, so it is the direction the zero-false-positive bar requires
/// (ADR-0002) — a known residue, not papered over. A non-`Var`/non-literal
/// argument declines for the same reason.
///
/// [`subtract_contract_lane`]: crate::subtract_contract_lane
/// [`subtract_pred_arms`]: crate::subtract_pred_arms
/// [`subtract_shape_arms`]: crate::subtract_shape_arms
#[allow(clippy::too_many_arguments)]
fn check_never_sentinel(
    cx: &Cx,
    folder: &mut dyn Folder,
    ty: &PType,
    param_name: &str,
    callee: &str,
    arg_offset: u32,
    value: &ArgValue,
    env: &HashMap<String, Known>,
    store: &Store,
    poisoned: bool,
    out: &mut Vec<Diagnostic>,
) {
    let surviving = match cx.resolve_cval(value, env, store, poisoned, folder) {
        Some(cv) => rendered_cval(&cv),
        None => {
            if poisoned {
                return;
            }
            let ArgValue::Var(name) = value else { return };
            if !store.contract_narrowed(name) {
                return;
            }
            let Some(arms) = store.contract_arms(name) else { return };
            let Some(text) = render_contract_arms(cx, arms) else { return };
            text
        }
    };
    let pos = cx.tree().position(arg_offset);
    out.push(Diagnostic {
        id: NEVER_PARAM_REACHABLE_ID,
        path: cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        message: format!(
            "{surviving} can still reach {callee}()'s @param {ty} ${param_name} — the unreachability claim is refuted",
        ),
        facet: None,
        fix: None,
    });
}

/// Bind each **named** argument (`name: <expr>`) to its target parameter by name
/// and run the same declared-contract acceptance judgment [`check_phpdoc_param`]
/// applies to a positional argument (Gap A). Named-argument binding is PHP-exact:
///
/// - **case-sensitive** name matching (`f(A: 1)` on `$a` is a fatal `Error`), so an
///   unmatched name binds nothing (the arity lane owns that `Error`);
/// - a **variadic** collector parameter takes the named argument as a keyed
///   element, never a scalar contract, so it is skipped;
/// - a **by-ref** parameter is skipped exactly as the positional lane skips it;
/// - a name resolving to a parameter already filled **positionally** (index
///   `< positional_count`) is the deferred overwrite `Error` — a fatal, so neither
///   lane reports it (mirrors [`emit_arity`]'s overwrite guard).
///
/// `positional_count` is `call.args.len()`; every other argument is shared
/// verbatim with the positional-lane call.
///
/// [`emit_arity`]: arity::emit_arity
#[allow(clippy::too_many_arguments)]
pub(crate) fn check_named_phpdoc_params(
    cx: &Cx,
    folder: &mut dyn Folder,
    envelopes: &Envelopes,
    params: &[Param],
    positional_count: usize,
    cfile: usize,
    coff: u32,
    callee: &str,
    named_args: &[NamedArg],
    env: &HashMap<String, Known>,
    store: &Store,
    poisoned: bool,
    in_descent: bool,
    out: &mut Vec<Diagnostic>,
) {
    for na in named_args {
        let Some((idx, param)) = params.iter().enumerate().find(|(_, p)| p.name == na.name) else {
            continue;
        };
        if param.variadic || param.by_ref || idx < positional_count {
            continue;
        }
        check_phpdoc_param(
            cx,
            folder,
            envelopes,
            param,
            cfile,
            coff,
            callee,
            na.span.start,
            &na.value,
            env,
            store,
            poisoned,
            in_descent,
            out,
        );
    }
}

/// The kind of callable-signature incompatibility a bound closure / first-class
/// callable exhibits against a declared `callable(...)` contract (issue #11).
#[derive(Debug, Clone, Copy)]
enum CallableViolation {
    /// The closure's declared parameter at this position is narrower than the
    /// contract supplies (parameter contravariance broken).
    Param(usize),
    /// The closure's declared return is provably incompatible with the contract's
    /// (return covariance broken).
    Return,
    /// The closure requires more parameters than the contract supplies, so the
    /// callee's invocation would `ArgumentCountError` (arity).
    Arity,
}

/// Lower a native scalar/union type to a [`ContractTy`] for the callable-signature
/// variance check (issue #11). Scalars and bool-literals map to their contract
/// arm; an object member maps to a class arm ([`normalize::subsumes`] judges it
/// only reflexively, so cross-class comparisons stay `Maybe`); a nullable hint
/// adds a `null` arm. A [`NativeType`] is always representable — the syntax layer
/// already dropped `mixed`/`iterable`/`callable`/intersection hints to `None`.
pub(crate) fn native_to_contract(nt: &NativeType) -> ContractTy {
    let mut arms: Vec<ContractTy> = nt
        .members
        .iter()
        .map(|m| match m {
            TypeMember::Scalar(ScalarType::Int) => ContractTy::Base(steins_domain::Base::Int),
            TypeMember::Scalar(ScalarType::Float) => ContractTy::Base(steins_domain::Base::Float),
            TypeMember::Scalar(ScalarType::String) => ContractTy::Base(steins_domain::Base::String),
            TypeMember::Scalar(ScalarType::Bool) => ContractTy::Base(steins_domain::Base::Bool),
            TypeMember::BoolLiteral(b) => ContractTy::LitBool(*b),
            TypeMember::Instance { fqn, .. } => ContractTy::Class(fqn.clone()),
            TypeMember::InstanceInter(cs) => {
                ContractTy::Inter(cs.iter().map(|c| ContractTy::Class(c.fqn.clone())).collect())
            }
        })
        .collect();
    if nt.nullable {
        arms.push(ContractTy::Null);
    }
    match arms.len() {
        1 => arms.pop().expect("len checked"),
        _ => ContractTy::Union(arms),
    }
}

/// Whether a contract arm is decidable by the **scalar** overlap relation — the
/// only positions the callable-variance check fires a definite `No` on (issue
/// #11). A bare identifier in a callable signature is syntactically
/// indistinguishable from a class name and far more often an unbound `@template`
/// than a real class (no call-site template solver), so `Class`/`ObjectAny`/
/// `Opaque`/array/callable arms stay silent (zero-FP). Only scalar/literal/null
/// arms — where `subsumes` gives a sound `No` and no template can hide — are judged.
fn scalar_decidable(ty: &ContractTy) -> bool {
    match ty {
        ContractTy::Base(_)
        | ContractTy::IntIn(_)
        | ContractTy::StrWith(_)
        | ContractTy::LitInt(_)
        | ContractTy::LitFloat(_)
        | ContractTy::LitStr(_)
        | ContractTy::LitBool(_)
        | ContractTy::Null
        | ContractTy::Never => true,
        ContractTy::Union(m) | ContractTy::Inter(m) => m.iter().all(scalar_decidable),
        _ => false,
    }
}

/// Judge a bound closure's declared native signature against a `callable(...)`
/// contract (issue #11), returning the first definite incompatibility or `None`
/// when compatible or undecidable (zero-FP silence).
///
/// This is the **declared-contract** relation (ADR-0030 divergence #1 — envelope
/// checking, no runtime coercion; PHP does *not* enforce a `callable(int): string`
/// docblock at runtime, verified with `php -r`), reusing the single overlap
/// relation [`normalize::subsumes`] as its comparator:
///
/// - **Parameters are contravariant**: `subsumes(closure_param, contract_param)`.
///   A closure accepting WIDER than the contract is fine; NARROWER is the
///   violation. Only a definite `No` reports; template/cross-class is `Maybe`. A
///   by-reference position (either side) is skipped — semantics unverified.
/// - **Return is covariant**: `subsumes(contract_ret, closure_ret)`. Narrower/
///   equal is fine; a provably-disjoint return is the violation. Undeclared
///   return is silent.
/// - **Arity.** A closure REQUIRING more parameters (no default, non-variadic)
///   than the contract supplies would `ArgumentCountError` (verified PHP 8.5,
///   `Too few arguments`). Extra OPTIONAL/variadic params are fine. Skipped when
///   the contract is itself variadic.
fn callable_sig_violation(
    sig: &steins_contract::CallableSig,
    closure_params: &[Param],
    closure_ret: Option<&NativeType>,
) -> Option<CallableViolation> {
    // Parameter contravariance, positional.
    for (i, cparam) in sig.params.iter().enumerate() {
        if cparam.by_ref || cparam.variadic {
            continue;
        }
        let Some(closure_param) = closure_params.get(i) else { continue };
        if closure_param.by_ref {
            continue;
        }
        let Some(pty) = closure_param.ty.as_ref() else { continue };
        let closure_ty = native_to_contract(pty);
        if scalar_decidable(&closure_ty)
            && scalar_decidable(&cparam.ty)
            && normalize::subsumes(&closure_ty, &cparam.ty) == Certainty::No
        {
            return Some(CallableViolation::Param(i));
        }
    }
    // Return covariance.
    if let Some(ret) = closure_ret {
        let closure_ret_ty = native_to_contract(ret);
        if scalar_decidable(&sig.ret)
            && scalar_decidable(&closure_ret_ty)
            && normalize::subsumes(&sig.ret, &closure_ret_ty) == Certainty::No
        {
            return Some(CallableViolation::Return);
        }
    }
    // Arity: the closure demands more parameters than the contract will supply.
    let contract_variadic = sig.params.iter().any(|p| p.variadic);
    if !contract_variadic {
        let required =
            closure_params.iter().filter(|p| !p.has_default && !p.variadic).count();
        if required > sig.params.len() {
            return Some(CallableViolation::Arity);
        }
    }
    None
}

/// A violated obligation of a **refined** callable spelling (ADR-0063 P3) — the
/// obligation half of the callable contract, beside [`CallableViolation`]'s
/// signature half.
#[derive(Clone, Copy)]
enum ObligationViolation {
    /// `pure-callable`/`pure-closure`/`static-pure-closure`: the bound callable's
    /// inferred effect envelope is provably not pure.
    Purity,
    /// `static-closure`/`static-pure-closure`: the bound closure is not declared
    /// `static`, so it can be bound to an object and reach `$this`.
    StaticBinding,
}

/// The [`ClosureTarget`] a lowered [`ClosureRef`] argument denotes — the one shape
/// [`callable_obl_violation`] judges, so the direct pass and the propagation pass
/// ask exactly the same question.
fn closure_target_of_ref(cref: &ClosureRef) -> ClosureTarget {
    match cref {
        ClosureRef::Anonymous { def_offset, .. } => ClosureTarget::Scope(*def_offset),
        ClosureRef::FunctionName(name) => ClosureTarget::Named(name.clone()),
    }
}

/// Judge a bound callable against the obligations of a refined callable spelling
/// (ADR-0063 §2 decision 4). `None` is "no proven violation" — every leg that
/// cannot see the callable's definition answers `None` rather than guessing.
///
/// The two obligations are decided by different machinery: `static` is written in
/// the syntax ([`Scope::is_static`]), purity is a property of the body only the
/// effect fixpoint can answer ([`Cx::provably_impure`]).
///
/// `closure_only` is **not** judged here: a closure literal and a first-class
/// callable both evaluate to a real `Closure` instance, satisfying it by
/// construction. The spelling's closure half bites on the *value* side instead
/// (`steins_contract::admits_val`/`admits_fact`) — the two halves of
/// `pure-closure` fail independently.
///
/// [`Scope::is_static`]: steins_syntax::Scope::is_static
fn callable_obl_violation(
    cx: &Cx,
    obl: steins_contract::CallableObl,
    target: &ClosureTarget,
) -> Option<ObligationViolation> {
    match target {
        ClosureTarget::Scope(def_offset) => {
            let scope = cx.closure_scope(*def_offset)?;
            if obl.is_static && !scope.is_static {
                return Some(ObligationViolation::StaticBinding);
            }
            // Closures are same-file, so the fixpoint's file-keyed closure symbol
            // matches by construction.
            if obl.pure
                && cx.provably_impure(&Sym::Closure(cx.path().to_owned(), *def_offset))
            {
                return Some(ObligationViolation::Purity);
            }
            None
        }
        ClosureTarget::Named(nameref) => {
            // `f(...)` — a first-class callable of a free function. It evaluates to a
            // `Closure` with no bound `$this`, satisfying the static-binding
            // obligation like `static function () {}`; only purity can be violated,
            // and only when the name resolves to a *user* function the fixpoint
            // actually read (builtin/ambiguous names have no envelope, stay silent).
            if !obl.pure {
                return None;
            }
            let FnResolution::User(site) = cx.resolve_function(nameref) else { return None };
            let fqn = cx.fn_decl(site).fqn.clone();
            if cx.provably_impure(&Sym::Func(fqn)) {
                return Some(ObligationViolation::Purity);
            }
            None
        }
    }
}

/// Emit the one `phpdoc.param-mismatch` a violated callable obligation produces.
/// Shared by both lanes so a closure written in argument position and a variable
/// holding the same closure report identically.
#[allow(clippy::too_many_arguments)]
fn push_obligation_diag(
    cx: &Cx,
    violation: ObligationViolation,
    callee: &str,
    ty: &PType,
    param_name: &str,
    arg_offset: u32,
    out: &mut Vec<Diagnostic>,
) {
    let reason = match violation {
        ObligationViolation::Purity => {
            "the bound callable's inferred effect envelope is not pure"
        }
        ObligationViolation::StaticBinding => "the bound closure is not declared static",
    };
    let pos = cx.tree().position(arg_offset);
    out.push(Diagnostic {
        id: PARAM_MISMATCH_ID,
        path: cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        message: format!(
            "callable argument to {callee}() violates declared @param {ty} ${param_name} — {reason}",
        ),
        facet: None,
        fix: None,
    });
}

/// Check a closure / first-class-callable argument at a call site against a
/// declared `callable(...)` `@param` contract (issue #11), emitting at most one
/// `phpdoc.param-mismatch`. Silent unless the contract carries a signature AND the
/// bound callable's declared *native* signature provably violates it.
///
/// The closure's declared signature is a static CST fact — captures do not change
/// parameter/return hints — so this rides the env-free direct pass.
pub(crate) fn check_callable_arg(
    cx: &Cx,
    envelopes: &Envelopes,
    param: &Param,
    callee: &str,
    arg_offset: u32,
    closure: &ClosureRef,
    out: &mut Vec<Diagnostic>,
) {
    let Some(ty) = envelopes.param(&param.name) else { return };
    let ContractTy::CallableTy { sig, obl } = steins_contract::lower(ty) else { return };

    // ADR-0063 P3: the refined spellings' obligations come first. Decided from
    // the bound callable's *definition*, not the declared call shape, so they
    // apply to a bare `pure-callable` with no signature; when both halves are
    // violated, the obligation is the more specific report.
    if !obl.is_bare()
        && let Some(violation) = callable_obl_violation(cx, obl, &closure_target_of_ref(closure))
    {
        push_obligation_diag(cx, violation, callee, ty, &param.name, arg_offset, out);
        return;
    }

    let Some(sig) = sig else { return };

    // Resolve the bound callable's declared native signature. Anonymous closures
    // address their own scope by definition offset; a first-class callable naming
    // a user function reuses the function-resolution leg (S5) — a builtin or
    // unresolvable name has no ground-truth signature, stays silent.
    let (closure_params, closure_ret): (&[Param], Option<&NativeType>) = match closure {
        ClosureRef::Anonymous { def_offset, .. } => {
            let Some(scope) = cx.closure_scope(*def_offset) else { return };
            (scope.params.as_slice(), scope.ret_ty.as_ref())
        }
        ClosureRef::FunctionName(name) => match cx.resolve_function(name) {
            FnResolution::User(site) => {
                let decl = cx.fn_decl(site);
                (decl.params.as_slice(), decl.ret.as_ref())
            }
            _ => return,
        },
    };

    let Some(violation) = callable_sig_violation(&sig, closure_params, closure_ret) else {
        return;
    };
    let param_name = &param.name;
    let message = match violation {
        CallableViolation::Param(i) => format!(
            "callable argument to {callee}() violates declared @param {ty} ${param_name} — parameter #{} type is incompatible (callable parameter contravariance)",
            i + 1,
        ),
        CallableViolation::Return => format!(
            "callable argument to {callee}() violates declared @param {ty} ${param_name} — return type is incompatible (callable return covariance)",
        ),
        CallableViolation::Arity => format!(
            "callable argument to {callee}() violates declared @param {ty} ${param_name} — it requires more parameters than the callable signature supplies",
        ),
    };
    let pos = cx.tree().position(arg_offset);
    out.push(Diagnostic {
        id: PARAM_MISMATCH_ID,
        path: cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        message,
        facet: None,
        fix: None,
    });
}
