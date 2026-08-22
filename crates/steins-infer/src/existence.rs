//! Foldable existence-guard verdicts (ADR-0049 §4 / N3): `method_exists`,
//! `function_exists`, `class_exists` and kin answered from the project index with
//! the dam's vouch.

use std::collections::HashSet;

use steins_domain::Certainty;
use steins_syntax::{ArgValue, CallExpr, ClassDecl, NameRef, RefKind, StaticClass};

use crate::fold::Folder;
use crate::cx::Cx;
use crate::env::{Store, Vouch};
use crate::project::{FnResolution, Res};
use crate::walk::WalkCx;

// ---------------------------------------------------------------------------
// Foldable existence-guard verdicts (ADR-0049 §4 / N3).
//
// `method_exists`/`function_exists`/`class_exists` (and `interface_`/`trait_`/
// `enum_exists`) in guard position fold to a three-valued `Certainty` against the
// closed world, so ADR-0031 dead-region pruning drops the branch the runtime
// provably never takes. Rests on the same closure the absence family fires under
// (S1 existence + S2 chain enumeration + A2ii boot-surface homonym oracle + A2i
// conditional/dam leg): `Yes` — provably present; `No` — provably absent; `Maybe`
// — anything short of closure. `Maybe` is always the FP-safe fallback, walking
// both branches live and leaning on the guard-respect vouch for silence.
// ---------------------------------------------------------------------------

/// Whether a function reference **denotes the global function it spells** — the
/// one question every builtin recognizer in this file asks before matching a name
/// against its vocabulary. Lives here once so the next recognizer added is
/// correct by construction rather than by copying (issue #153).
///
/// The load-bearing distinction: `\foo` denotes the global function, `Ns\foo`
/// does not — not simply "reject a backslash", since [`NameRef::raw`] already has
/// a leading `\` and `namespace\` prefix stripped. Legs, measured against php
/// 8.5.9: `\is_string($x)` — always global; `is_string($x)` — PHP's function
/// fallback reaches global from any namespace; `Foo\is_string($x)` — relative,
/// never global; `namespace\is_string($x)` inside `namespace App;` — resolves to
/// `App\is_string` only, no fallback (in the root namespace this IS global);
/// `use function Other\thing as is_string;` — goes to `Other\thing`, never falls
/// back, while a plain `use function is_string;` still is global.
///
/// The rejected legs mirror [`name_reaches_global_var_dump`]. The shadowing leg
/// is separate: a project-defined function of the name is a different function
/// whatever the spelling otherwise denotes, and it is asked through
/// [`Cx::resolve_shadow`] rather than [`Cx::resolve_function`] — see there for
/// why the difference is load-bearing.
///
/// [`name_reaches_global_var_dump`]: crate::dump::name_reaches_global_var_dump
fn denotes_global_function(cx: &Cx, r: &NameRef) -> bool {
    let spells_global = match r.kind {
        RefKind::FullyQualified => !r.raw.contains('\\'),
        RefKind::Qualified => false,
        RefKind::Relative => {
            !r.raw.contains('\\') && cx.tree().ctx_at(r.offset).namespace.is_empty()
        }
        RefKind::Unqualified => {
            match cx.tree().ctx_at(r.offset).fn_imports.get(&r.raw.to_ascii_lowercase()) {
                Some(target) => target.eq_ignore_ascii_case(&r.raw),
                None => true,
            }
        }
    };
    spells_global && !matches!(cx.resolve_shadow(r), FnResolution::User(_))
}

/// The simple name a call's callee spells, when the reference denotes the
/// **global** function of that name ([`denotes_global_function`]) — the single
/// entry point every builtin recognizer below opens with. `None` for a dynamic
/// callee, a namespaced or namespace-relative twin, an aliased import, or a
/// userland shadow.
pub(crate) fn global_function_callee<'a>(cx: &Cx, call: &'a CallExpr) -> Option<&'a str> {
    let callee = call.callee.as_deref()?;
    let r = call.callee_ref.as_ref()?;
    denotes_global_function(cx, r).then_some(callee)
}

/// The recognized existence predicate a guard call names, or `None` when the call
/// is not one of them / does not denote the global builtin (a `Foo\class_exists`
/// or a same-named user function is a DIFFERENT function — see
/// [`global_function_callee`], which owns that whole rule).
fn existence_predicate(cx: &Cx, call: &CallExpr) -> Option<&'static str> {
    let callee = global_function_callee(cx, call)?;
    const PREDS: &[&str] = &[
        "method_exists",
        "function_exists",
        "class_exists",
        "interface_exists",
        "trait_exists",
        "enum_exists",
        // global constants (ADR-0078, issue #198)
        "defined",
        // end global constants (ADR-0078, issue #198)
    ];
    PREDS.iter().copied().find(|p| callee.eq_ignore_ascii_case(p))
}

/// Fold a recognized existence-guard call to a verdict (the N3 machinery). Anything
/// unrecognized or short of closure is `Maybe`.
pub(crate) fn eval_existence_call(w: &WalkCx, folder: &mut dyn Folder, call: &CallExpr) -> Certainty {
    let Some(pred) = existence_predicate(w.cx, call) else {
        return Certainty::Maybe;
    };
    // A2ii/A9: without a live boot surface (or with a runtime-redefinition extension
    // loaded), neither presence nor absence is decidable — the sound subset is Maybe.
    if !folder.absence_family_available() {
        return Certainty::Maybe;
    }
    if pred == "method_exists" {
        // `method_exists(class, 'name')` — two positional literal arguments.
        if !call.positional_only || call.args.len() != 2 {
            return Certainty::Maybe;
        }
        let Some(class_fqn) = existence_class_literal(w.cx, &call.args[0].value) else {
            return Certainty::Maybe;
        };
        // A name lane: a byte string names no PHP method, so the verdict stays
        // Maybe rather than resolving against a lossy spelling (ADR-0080 §2.5).
        let ArgValue::Str(method) = &call.args[1].value else {
            return Certainty::Maybe;
        };
        let Some(method) = method.as_str() else {
            return Certainty::Maybe;
        };
        method_exists_verdict(w.cx, folder, &class_fqn, method)
    } else if pred == "function_exists" {
        if !call.positional_only || call.args.len() != 1 {
            return Certainty::Maybe;
        }
        let ArgValue::Str(name) = &call.args[0].value else {
            return Certainty::Maybe;
        };
        let Some(name) = name.as_str() else {
            return Certainty::Maybe;
        };
        function_exists_verdict(w.cx, folder, name)
    // global constants (ADR-0078, issue #198)
    } else if pred == "defined" {
        if !call.positional_only || call.args.len() != 1 {
            return Certainty::Maybe;
        }
        let ArgValue::Str(name) = &call.args[0].value else {
            return Certainty::Maybe;
        };
        let Some(name) = name.as_str() else {
            return Certainty::Maybe;
        };
        constant_defined_verdict(w.cx, folder, name)
    // end global constants (ADR-0078, issue #198)
    } else {
        // `class_exists`/`interface_exists`/`trait_exists`/`enum_exists('Name')`.
        if !call.positional_only || call.args.is_empty() {
            return Certainty::Maybe;
        }
        let Some(name) = existence_class_literal(w.cx, &call.args[0].value) else {
            return Certainty::Maybe;
        };
        classlike_exists_verdict(w.cx, folder, pred, &name)
    }
}

/// Resolve a *literal* class reference in an existence-predicate argument to an FQN:
/// the `C::class` magic constant (resolved in the call site's namespace context) or
/// a string class name (which PHP treats as fully qualified). A `$var` receiver or
/// any other form is `None` — the verdict then stays `Maybe`, and the conservative
/// guard-respect leg (which CAN read the store) carries the silence for a proven-class
/// variable.
fn existence_class_literal(cx: &Cx, v: &ArgValue) -> Option<String> {
    match v {
        ArgValue::ClassConst(StaticClass::Named(r), name) if name.eq_ignore_ascii_case("class") => {
            Some(cx.class_fqn(r))
        }
        ArgValue::Str(s) => Some(s.as_str()?.trim_start_matches('\\').to_owned()),
        _ => None,
    }
}

/// The three-valued `method_exists(start_fqn, method)` verdict: walk `start_fqn`'s
/// class chain under the S2 closure discipline (ADR-0049 §4). Unlike the absence
/// flagship this ignores `__call`/`__callStatic` — `method_exists` reports only
/// declared methods. Abstract or any-visibility declarations count as present
/// (visibility-blind). Any obstacle to closure (trait-bearing/enum node,
/// unresolvable ancestor, cycle, conditional node with the dam standing, or an
/// unanswerable/positive boot-surface homonym) collapses to `Maybe`.
fn method_exists_verdict(
    cx: &Cx,
    folder: &mut dyn Folder,
    start_fqn: &str,
    method: &str,
) -> Certainty {
    let mut cur = start_fqn.to_owned();
    let mut seen: HashSet<String> = HashSet::new();
    let mut fqns: Vec<String> = Vec::new();
    let mut any_conditional = false;
    let present;
    loop {
        if !seen.insert(cur.to_ascii_lowercase()) {
            return Certainty::Maybe; // cycle — closure cannot terminate soundly.
        }
        let Some((cfile, cd)) = cx.find_class(&cur) else {
            return Certainty::Maybe; // ancestor leaves the project / ambiguous.
        };
        // Enum methods are not lowered; a trait/`uses_traits` node could carry the
        // method invisibly to this walk — either way, closure is unproven.
        if cd.is_enum || cd.is_trait || cd.uses_traits {
            return Certainty::Maybe;
        }
        fqns.push(cur.clone());
        if cd.conditional {
            any_conditional = true;
        }
        if cd.methods.iter().any(|m| m.name.eq_ignore_ascii_case(method)) {
            present = true;
            break;
        }
        match &cd.parent {
            None => {
                present = false;
                break;
            }
            Some(pref) => cur = cx.units[cfile].tree.resolve_class_fqn(pref),
        }
    }
    // A2i: a conditional declaration on the chain re-dams the claim — only the clear
    // whole-universe dam lets either verdict stand.
    if any_conditional && !cx.dam.is_clear() {
        return Certainty::Maybe;
    }
    // A2ii: every traversed FQN must be boot-surface homonym-clear, else the runtime
    // class differs from the textual one and neither presence nor absence is decidable.
    for fqn in &fqns {
        match folder.boot_surface_class_like(fqn) {
            Some(false) => {}
            Some(true) | None => return Certainty::Maybe,
        }
    }
    if present { Certainty::Yes } else { Certainty::No }
}

/// The three-valued `function_exists('name')` verdict (ADR-0049 §6 / S1 existence).
/// A catalog builtin is always present; a uniquely-indexed unconditional userland
/// function is present; an absent name that the boot surface answers NOT-a-function
/// is provably absent. A conditional declaration (dam standing), an ambiguous name,
/// or an unanswerable homonym is `Maybe`.
fn function_exists_verdict(cx: &Cx, folder: &mut dyn Folder, name: &str) -> Certainty {
    let lname = name.trim_start_matches('\\').to_ascii_lowercase();
    // A catalogued builtin is a resident function (`strlen`, `array_map`, …).
    if steins_catalog::effect_labels(&lname).is_some() {
        return Certainty::Yes;
    }
    match cx.index.resolve_function(&lname) {
        Res::Unique(site) => {
            if cx.fn_decl(site).conditional && !cx.dam.is_clear() {
                Certainty::Maybe // a conditional polyfill with the dam standing.
            } else {
                Certainty::Yes
            }
        }
        Res::Ambiguous => Certainty::Maybe,
        Res::Absent => match folder.boot_surface_function(&lname) {
            Some(true) => Certainty::Yes,  // a resident extension function.
            Some(false) => Certainty::No,  // provably absent everywhere.
            None => Certainty::Maybe,
        },
    }
}

// global constants (ADR-0078, issue #198)
/// The three-valued `defined('NAME')` verdict — deliberately **two**-valued in
/// practice: it answers `No` or `Maybe`, and never `Yes`.
///
/// That asymmetry is the whole point. `defined()` asks about the state of the
/// *running* process, not the text: a `define('X', 1)` in the universe hasn't
/// necessarily executed yet, and the common shape is exactly
/// `if (!defined('X')) { define('X', …); }`, whose body exists because `defined`
/// is false there. Folding to `Yes` would mark that body dead on a claim PHP
/// doesn't make.
///
/// `No` is safe the other way and buys `constant.undefined` its guard leg for
/// free: when nothing declares the name, the dam is clear, and PHP reports it not
/// defined, the call provably returns `false` — so `if (defined('X')) { echo X; }`
/// folds its body dead. Same mechanism `class.undefined` uses.
///
/// `name` is case-sensitive on its final segment; [`steins_syntax::normalize_const_fqn`]
/// decides which half folds case.
fn constant_defined_verdict(cx: &Cx, folder: &mut dyn Folder, name: &str) -> Certainty {
    let key = steins_syntax::normalize_const_fqn(name);
    if cx.index.declares_constant(&key) {
        // Declared somewhere — but "declared" is not "already executed". Maybe.
        return Certainty::Maybe;
    }
    if !cx.dam.constants_are_clear() {
        return Certainty::Maybe;
    }
    match folder.boot_surface_constant(&key) {
        Some(false) => Certainty::No,
        Some(true) | None => Certainty::Maybe,
    }
}
// end global constants (ADR-0078, issue #198)

/// The three-valued `class_exists`/`interface_exists`/`trait_exists`/`enum_exists`
/// verdict (ADR-0049 §4 / S1 existence). A uniquely-indexed unconditional project
/// class-like of the MATCHING kind is present; an absent name the boot surface reports
/// as resident is present; an absent name the boot surface reports NOT-resident is
/// provably absent. A conditional decl (dam standing), an ambiguous name, a kind
/// mismatch (`class_exists` on an interface), or an unanswerable homonym is `Maybe`.
fn classlike_exists_verdict(
    cx: &Cx,
    folder: &mut dyn Folder,
    pred: &str,
    name: &str,
) -> Certainty {
    let lname = name.trim_start_matches('\\').to_ascii_lowercase();
    match cx.index.resolve_class(&lname) {
        Res::Unique(site) => {
            let (_, cd) = cx.class_decl(site);
            if cd.conditional && !cx.dam.is_clear() {
                return Certainty::Maybe;
            }
            // A PHP enum satisfies both `enum_exists` and `class_exists`; a plain
            // interface/trait never satisfies `class_exists`. A mismatch cannot be
            // proven true (a boot-surface homonym might still match), so `Maybe`.
            if classlike_kind_matches(pred, cd) {
                Certainty::Yes
            } else {
                Certainty::Maybe
            }
        }
        Res::Ambiguous => Certainty::Maybe,
        Res::Absent => match folder.boot_surface_class_like(&lname) {
            Some(true) => Certainty::Yes,
            Some(false) => Certainty::No,
            None => Certainty::Maybe,
        },
    }
}

/// Whether a resolved class-like declaration satisfies the given existence predicate:
/// `class_exists` accepts a class or enum (never a bare interface/trait);
/// `interface_exists`/`trait_exists`/`enum_exists` each accept only their own kind.
fn classlike_kind_matches(pred: &str, cd: &ClassDecl) -> bool {
    match pred {
        "class_exists" => !cd.is_interface && !cd.is_trait,
        "interface_exists" => cd.is_interface,
        "trait_exists" => cd.is_trait,
        "enum_exists" => cd.is_enum,
        _ => false,
    }
}

/// The symbol a positive existence guard call vouches for (ADR-0049 §4 guard-respect
/// leg), resolved against the branch store. `None` when the call isn't a recognized
/// existence predicate or its subject can't be pinned to a concrete symbol.
/// `method_exists` additionally resolves a `$var` receiver to its store-known class,
/// so the instance idiom `if (method_exists($o,'m')) { $o->m(); }` vouches `C::m` —
/// the vouch key is the resolved class + name.
pub(crate) fn existence_vouch(cx: &Cx, store: &Store, call: &CallExpr) -> Option<Vouch> {
    let pred = existence_predicate(cx, call)?;
    if pred == "method_exists" {
        if !call.positional_only || call.args.len() != 2 {
            return None;
        }
        let ArgValue::Str(method) = &call.args[1].value else {
            return None;
        };
        let class = match &call.args[0].value {
            ArgValue::Var(v) => store.class_of(v)?.to_owned(),
            other => existence_class_literal(cx, other)?,
        };
        Some(Vouch::Method {
            class: class.trim_start_matches('\\').to_ascii_lowercase(),
            method: method.as_str()?.to_ascii_lowercase(),
        })
    } else if pred == "function_exists" {
        if !call.positional_only || call.args.len() != 1 {
            return None;
        }
        let ArgValue::Str(name) = &call.args[0].value else {
            return None;
        };
        Some(Vouch::Function(name.as_str()?.trim_start_matches('\\').to_ascii_lowercase()))
    // global constants (ADR-0078, issue #198)
    } else if pred == "defined" {
        // `defined('X')` vouches nothing, on purpose: `constant.undefined` is
        // judged by a file-wide pass with no branch store, like `class.undefined`,
        // and takes its guard leg from dead-region pruning instead (see
        // `constant_defined_verdict`). The arm exists so the class-predicate arm
        // below can't mistake a constant name for a class name.
        None
    // end global constants (ADR-0078, issue #198)
    } else {
        if !call.positional_only || call.args.is_empty() {
            return None;
        }
        let name = existence_class_literal(cx, &call.args[0].value)?;
        Some(Vouch::Class(name.trim_start_matches('\\').to_ascii_lowercase()))
    }
}
