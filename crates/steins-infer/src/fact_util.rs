//! Fact and contract helpers that several passes share and none owns: the fact
//! arithmetic of the shape projections and the transfers ([`join_into`],
//! [`fact_admitting_null`], …), the abstract-argument arm and the
//! class-touching contract valves of the phpdoc checks (ADR-0043 stage 4), and
//! the two renderers a finding message names a value with — [`describe_fact`]
//! for an abstract fact, [`rendered_cval`] for a proven value.
//!
//! Re-exported at the crate root, so consumers import `crate::describe_fact`
//! and the like.

use std::collections::HashMap;

use steins_domain::{ArmKnown, Base, Fact, IntRange, Key as VKey, Refinement, StrPreds, Val};
use steins_phpdoc::Type as PType;
use steins_phpdoc::ast::TypeKind as PKind;
use steins_syntax::{ArgValue, ArrayKey, NormKey};

use crate::builtin_returns::fact_with_null;
use crate::contract::CVal;
use crate::cx::Cx;
use crate::dump::render_shape_fact;
use crate::env::{HandleState, Known};

/// Join `f` into an accumulator that may still be empty; `None` propagates the
/// unrepresentable join as the unknown floor.
pub(crate) fn join_into(acc: Option<Fact>, f: &Fact) -> Option<Option<Fact>> {
    match acc {
        None => Some(Some(f.clone())),
        Some(a) => a.join(f).map(Some),
    }
}

/// The domain value a shape key denotes (`Key::Int(5)` is the value `5`).
pub(crate) fn val_of_key(k: &VKey) -> Val {
    match k {
        VKey::Int(i) => Val::Int(*i),
        VKey::Str(s) => Val::Str(s.clone()),
    }
}

/// Is every value this fact admits an `int`? (`null` is immaterial to
/// [`project_flip`]'s question — a null value is skipped by the flip, not turned
/// into a key.)
///
/// [`project_flip`]: crate::shape_projection::project_flip
pub(crate) fn fact_is_int(f: &Fact) -> bool {
    match f.finite_members() {
        Some(vals) => vals.iter().all(|v| matches!(v, Val::Int(_) | Val::Null)),
        None => matches!(
            f,
            Fact::General { base: Base::Int, .. } | Fact::Refined { base: Base::Int, .. }
        ),
    }
}

/// Add `null` to a fact's denotation — the finite layers by value, the abstract
/// ones through their own `nullable` flag. `None` when the result is not
/// representable (a shape fact, or an over-cap finite widening).
pub(crate) fn fact_admitting_null(f: &Fact) -> Option<Fact> {
    match f.finite_members() {
        Some(vals) => {
            let mut vals = vals.to_vec();
            vals.push(Val::Null);
            Fact::from_vals(vals)
        }
        None => fact_with_null(f),
    }
}

/// The abstract fact an argument resolves to: a bare `$var` whose env fact is an
/// abstract layer (no finite members). Finite/proven values go through
/// `resolve_cval` instead, so this is the disjoint "abstract" arm of Feature E.
pub(crate) fn arg_abstract_fact<'e>(
    value: &ArgValue,
    env: &'e HashMap<String, Known>,
    poisoned: bool,
) -> Option<&'e Fact> {
    if poisoned {
        return None;
    }
    let ArgValue::Var(name) = value else { return None };
    let f = env.get(name)?.fact.as_ref()?;
    f.finite_members().is_none().then_some(f)
}

/// Whether a lowered contract type contains a class-name node — a bare identifier
/// that may actually be a template or a type-alias. The abstract-fact check stays
/// silent on these (see [`check_phpdoc_param`]).
///
/// [`check_phpdoc_param`]: crate::generics::check_phpdoc_param
pub(crate) fn contract_touches_class(ty: &steins_contract::ContractTy) -> bool {
    use steins_contract::ContractTy as C;
    match ty {
        C::Class(_) => true,
        C::Union(m) | C::Inter(m) => m.iter().any(contract_touches_class),
        C::ListOf { elem, .. } => contract_touches_class(elem),
        C::MapOf { key, val, .. } | C::IterableOf { key, val } => {
            contract_touches_class(key) || contract_touches_class(val)
        }
        C::Shape { fields, unsealed, .. } => {
            fields.iter().any(|f| contract_touches_class(&f.ty))
                || unsealed.as_ref().is_some_and(|(k, v)| {
                    k.as_ref().is_some_and(|k| contract_touches_class(k))
                        || contract_touches_class(v)
                })
        }
        _ => false,
    }
}

/// ADR-0043 stage 4 — the phpdoc-side analogue of [`object_world_guard_blind`]. A
/// class-touching phpdoc verdict is unsound inside a binding descent: the callee's
/// in-body type guards on the rebound value are unmodeled. "Touches a class"
/// means the proven value is an object, or the contract references a class name.
/// Scalar-vs-scalar phpdoc checks are unaffected. Always `false` outside a descent.
///
/// [`object_world_guard_blind`]: crate::arg_check::object_world_guard_blind
pub(crate) fn phpdoc_object_guard_blind(in_descent: bool, ty: &PType, cv: Option<&CVal>) -> bool {
    in_descent
        && (matches!(cv, Some(CVal::Object(..)))
            || contract_touches_class(&steins_contract::lower(ty)))
}

/// ADR-0043 stage 4 — is `ty` a **pure class contract**: a known class name, or a
/// union/nullable built only from known class names and `null` (e.g. `Foo`,
/// `Foo|null`, `?Foo`, `A|B`)? Only such a contract may let a definite scalar fact
/// open the [`contract_touches_class`] valve. `is_known_class` is the safety
/// valve — an unresolved bare identifier may be a `@template`/`@phpstan-type`
/// alias denoting a scalar, disqualifying the whole contract. A contract touching
/// array/generic/shape/intersection/callable, or any scalar/pseudo-type keyword,
/// is *not* pure-class.
pub(crate) fn is_pure_class_contract(cx: &Cx, cfile: usize, coff: u32, ty: &PType) -> bool {
    fn walk(cx: &Cx, cfile: usize, coff: u32, ty: &PType, saw_class: &mut bool) -> bool {
        match &ty.kind {
            PKind::Identifier(name) => {
                // A `null` companion (the `class|null` shape) is allowed but is not
                // itself the class that satisfies the "at least one class" rule.
                if name.eq_ignore_ascii_case("null") {
                    return true;
                }
                let target = cx.resolve_pclass(cfile, coff, name);
                if cx.is_known_class(&target) {
                    *saw_class = true;
                    true
                } else {
                    false
                }
            }
            PKind::Nullable(inner) => walk(cx, cfile, coff, inner, saw_class),
            PKind::Union { types, .. } => {
                types.iter().all(|t| walk(cx, cfile, coff, t, saw_class))
            }
            _ => false,
        }
    }
    let mut saw_class = false;
    walk(cx, cfile, coff, ty, &mut saw_class) && saw_class
}

/// A short, phpdoc-flavored description of an abstract fact for a diagnostic
/// message (`a value of type int`, `a non-empty-string value`, `an int|null
/// value`). Finite facts never reach here (they render as concrete values).
pub(crate) fn describe_fact(f: &Fact) -> String {
    let base_kw = |b: Base| match b {
        Base::Int => "int",
        Base::Float => "float",
        Base::String => "string",
        Base::Bool => "bool",
    };
    let (name, nullable) = match f {
        Fact::General { base, nullable } => (base_kw(*base).to_owned(), *nullable),
        Fact::Refined { base: Base::Int, refinement: Refinement::Int(r), nullable } => {
            let n = if *r == IntRange::POSITIVE {
                "positive-int".to_owned()
            } else if *r == IntRange::NEGATIVE {
                "negative-int".to_owned()
            } else if *r == IntRange::NON_NEGATIVE {
                "non-negative-int".to_owned()
            } else {
                format!("int<{}, {}>", r.lo(), r.hi())
            };
            (n, *nullable)
        }
        Fact::Refined { base: Base::String, refinement: Refinement::Str(p), nullable } => {
            let casing = match (
                p.contains_all(StrPreds::LOWERCASE),
                p.contains_all(StrPreds::UPPERCASE),
            ) {
                (true, false) => Some("lowercase"),
                (false, true) => Some("uppercase"),
                // Neither, or both (nothing cased to change): no single keyword.
                _ => None,
            };
            let n = if p.contains_all(StrPreds::NON_FALSY) {
                "non-falsy-string".to_owned()
            } else if p.contains_all(StrPreds::NUMERIC) {
                "numeric-string".to_owned()
            } else if let Some(c) = casing {
                if p.contains_all(StrPreds::NON_EMPTY) {
                    format!("non-empty-{c}-string")
                } else {
                    format!("{c}-string")
                }
            } else if p.contains_all(StrPreds::NON_EMPTY) {
                "non-empty-string".to_owned()
            } else {
                "string".to_owned()
            };
            (n, *nullable)
        }
        Fact::Refined { base, nullable, .. } => (base_kw(*base).to_owned(), *nullable),
        // A union spells arm by arm through this same speller, joined by `|`
        // (issue #339). The arms carry no `null` of their own — the union's
        // flag does — so each is rendered non-nullable and the null half is
        // added once, below, exactly as it is for a single base.
        Fact::Union { arms, nullable } => {
            let spelled: Vec<String> = arms
                .iter()
                .map(|(base, known)| {
                    // A bool-literal arm is one value (ADR-0093 §2), and the message
                    // names it: `string|true`, not `string|bool`. It is spelled here
                    // rather than through a `Fact`, because the finite layers do not
                    // reach this speller at all — its callers gate on
                    // `finite_members`, and the arm below answers `"value"`.
                    let arm = match known {
                        ArmKnown::Bool(b) => {
                            return if *b { "true" } else { "false" }.to_owned();
                        }
                        ArmKnown::Refined(r) => Fact::refined(*base, *r, false),
                        ArmKnown::Whole => Fact::General { base: *base, nullable: false },
                    };
                    describe_fact(&arm)
                        .trim_start_matches("a value of type ")
                        .to_owned()
                })
                .collect();
            (spelled.join("|"), *nullable)
        }
        // The array stratum reaches this surface as of ADR-0072 (a shape fact is
        // now judged against a contract, so it can be the thing a
        // `phpdoc.*-mismatch` names). It spells through the ONE speller the dump
        // surface uses — `render_shape_fact` already carries the null half, so
        // the `nullable` flag stays `false` here rather than doubling it.
        Fact::Shape { shape, nullable } => (render_shape_fact(shape, *nullable), false),
        // Finite facts do not reach here: the callers gate on `finite_members`.
        Fact::Singleton(_) | Fact::OneOf(_) => ("value".to_owned(), false),
    };
    if nullable {
        format!("a value of type {name}|null")
    } else {
        format!("a value of type {name}")
    }
}

/// Render a proven [`CVal`] for a diagnostic message (delegates arrays/scalars to
/// [`ArgValue::render`]; objects show `new Class()`).
pub(crate) fn rendered_cval(v: &CVal) -> String {
    match v {
        CVal::Scalar(s) => s.render(),
        CVal::Object(class, _) => format!("new {}()", class.rsplit('\\').next().unwrap_or(class)),
        CVal::Resource { state: HandleState::Open } => "an open resource".to_owned(),
        CVal::Resource { state: HandleState::Closed } => "a closed resource".to_owned(),
        CVal::Resource { state: HandleState::Unknown } => "a resource".to_owned(),
        CVal::Array(entries) => {
            // Rebuild an `ArgValue::Array` with explicit keys so the shared compact
            // renderer applies (it re-normalizes; explicit keys round-trip).
            let items: Vec<(ArrayKey, ArgValue)> = entries
                .iter()
                .map(|(k, cv)| {
                    let key = match k {
                        NormKey::Int(i) => ArrayKey::Int(*i),
                        NormKey::Str(s) => ArrayKey::Str(s.clone()),
                    };
                    (key, cval_to_argvalue(cv))
                })
                .collect();
            ArgValue::Array(items).render()
        }
    }
}

/// A best-effort [`ArgValue`] reconstruction of a [`CVal`], for rendering only.
fn cval_to_argvalue(v: &CVal) -> ArgValue {
    match v {
        CVal::Scalar(s) => s.clone(),
        CVal::Object(..) | CVal::Resource { .. } => ArgValue::Other,
        CVal::Array(entries) => ArgValue::Array(
            entries
                .iter()
                .map(|(k, cv)| {
                    let key = match k {
                        NormKey::Int(i) => ArrayKey::Int(*i),
                        NormKey::Str(s) => ArrayKey::Str(s.clone()),
                    };
                    (key, cval_to_argvalue(cv))
                })
                .collect(),
        ),
    }
}
