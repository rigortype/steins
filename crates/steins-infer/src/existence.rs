//! Foldable existence-guard verdicts (ADR-0049 §4 / N3): `method_exists`,
//! `function_exists`, `class_exists` and kin answered from the project index with
//! the dam's vouch.

use std::collections::HashSet;

use steins_domain::Certainty;
use steins_syntax::{
    ArgValue, ArrayKey, CallExpr, ClassDecl, EXISTENCE_PREDICATES, NameRef, RefKind, StaticClass,
};

use crate::cx::Cx;
use crate::dam::DamKind;
use crate::declared_receiver::declared_receiver_lacks_method;
use crate::env::{Store, Vouch};
use crate::fold::Folder;
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
pub(crate) fn denotes_global_function(cx: &Cx, r: &NameRef) -> bool {
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
    // The vocabulary lives in the syntax crate, which reads it too: a region whose test is
    // one of these is the only call-bearing guard region the lowering carries.
    EXISTENCE_PREDICATES.iter().copied().find(|p| callee.eq_ignore_ascii_case(p))
}

/// Fold a recognized existence-guard call to a verdict (the N3 machinery). Anything
/// unrecognized or short of closure is `Maybe`. `store` is the branch's, read only by
/// the member guards over a `$var` receiver ([`member_guard_verdict`]).
pub(crate) fn eval_existence_call(
    w: &WalkCx,
    folder: &mut dyn Folder,
    store: &Store,
    call: &CallExpr,
) -> Certainty {
    if let Some(verdict) = member_guard_verdict(w, folder, store, call) {
        return verdict;
    }
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
    } else if pred == "extension_loaded" {
        if !call.positional_only || call.args.len() != 1 {
            return Certainty::Maybe;
        }
        let ArgValue::Str(name) = &call.args[0].value else {
            return Certainty::Maybe;
        };
        let Some(name) = name.as_str() else {
            return Certainty::Maybe;
        };
        extension_loaded_verdict(w.cx, folder, name)
    } else {
        // `class_exists`/`interface_exists`/`trait_exists`/`enum_exists('Name')`.
        if !call.positional_only || call.args.is_empty() {
            return Certainty::Maybe;
        }
        let Some(name) = existence_class_literal(w.cx, &call.args[0].value) else {
            return Certainty::Maybe;
        };
        // The `$autoload` argument: absent is `true`, and a literal `true` is the same
        // question. Anything else asks whether the class is *already loaded*, which
        // a declaration in the project does not answer.
        let autoload = call.args.get(1).is_none_or(|a| matches!(a.value, ArgValue::Bool(true)));
        classlike_exists_verdict(w.cx, folder, pred, &name, autoload)
    }
}

/// The verdict of `method_exists($v, 'm')` / `is_callable([$v, 'm'])` over a `$var`
/// receiver (issue #930), or `None` when `call` is neither — so literal-class
/// `method_exists` keeps its N3 path.
///
/// `No` where the declared-receiver lane **proves** `m` absent on every narrowed arm
/// ([`declared_receiver_lacks_method`], the ladder the lane reports with): the guard is
/// provably false and the body it guards is dead, exactly where the lane would have
/// reported inside it. Otherwise `Maybe` — an arm that may have `m` leaves the guard
/// undecided, and an allocation-proven receiver stays with the exact-class vouch.
fn member_guard_verdict(
    w: &WalkCx,
    folder: &mut dyn Folder,
    store: &Store,
    call: &CallExpr,
) -> Option<Certainty> {
    let shape = member_guard_shape(w.cx, call)?;
    if shape.kind != MemberGuard::Method {
        return None;
    }
    let ArgValue::Var(var) = shape.receiver else { return None };
    let ArgValue::Str(method) = shape.member else { return None };
    let method = method.as_str()?;
    let lacks =
        declared_receiver_lacks_method(w.cx, folder, store, w.scope.poisoned, var, method);
    Some(if lacks { Certainty::No } else { Certainty::Maybe })
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

/// The three-valued `extension_loaded('name')` verdict (issue #928): the sidecar's
/// loaded-extension list answers it, case-insensitively, as PHP's own lookup does.
///
/// The question is about the running process, as `defined()`'s is, and the answer is
/// the boot surface's only while nothing can change it: a `dl(...)` call anywhere in the
/// universe ([`DamKind::ExtensionLoad`]) loads extensions at run time, so with one
/// standing neither polarity is decidable. No sidecar answers `Maybe` (the caller has
/// already refused an unavailable absence family, and the folder answers `None` for an
/// unanswerable `env()`).
///
/// This is what discharges the common optional-extension guard
/// (`if (extension_loaded('redis')) { new Redis; }`) on a runtime that lacks the
/// extension: the guard is provably false, so its body is a dead region.
fn extension_loaded_verdict(cx: &Cx, folder: &mut dyn Folder, name: &str) -> Certainty {
    if cx.dam.sites().iter().any(|s| s.kind == DamKind::ExtensionLoad) {
        return Certainty::Maybe;
    }
    match folder.boot_surface_extension(name) {
        Some(true) => Certainty::Yes,
        Some(false) => Certainty::No,
        None => Certainty::Maybe,
    }
}

/// The three-valued `class_exists`/`interface_exists`/`trait_exists`/`enum_exists`
/// verdict (ADR-0049 §4 / S1 existence). A uniquely-indexed unconditional project
/// class-like of the MATCHING kind is present; an absent name the boot surface reports
/// as resident is present; an absent name the boot surface reports NOT-resident is
/// provably absent. A conditional declaration, a project declaration under an
/// `$autoload` that is not literally `true`, an ambiguous name, a kind mismatch
/// (`class_exists` on an interface), or an unanswerable homonym is `Maybe`.
///
/// A conditional declaration is `Maybe` whatever the dam says: whether it ran is a
/// run-time fact (`if (PHP_VERSION_ID < 80000) { class Polyfill {} }`,
/// `if (getenv('X')) { class C {} }`), so `Yes` would kill the `else` of a guard that is
/// the very way the program tolerates the declaration being absent (issue #978 on the
/// polyfill trade-off). A declaration is not a loaded class either:
/// `class_exists('Later', false)` is false until the declaring file has run.
fn classlike_exists_verdict(
    cx: &Cx,
    folder: &mut dyn Folder,
    pred: &str,
    name: &str,
    autoload: bool,
) -> Certainty {
    let lname = name.trim_start_matches('\\').to_ascii_lowercase();
    match cx.index.resolve_class(&lname) {
        Res::Unique(site) => {
            let (_, cd) = cx.class_decl(site);
            if cd.conditional || !autoload {
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

/// The symbols a positive existence guard call vouches for (ADR-0049 §4 guard-respect
/// leg), resolved against the branch store. Empty when the call isn't a recognized
/// guard or its subject can't be pinned.
///
/// The member guards vouch **this binding**, never a class through a declared arm:
/// `method_exists($o, 'm')` and `is_callable([$o, 'm'])` vouch `C::m` for the one class
/// an allocation-proven `$o` holds (the N3 exact-class vouch; a declared receiver is
/// *folded* instead, [`member_guard_verdict`]), and `property_exists($v, 'p')` vouches
/// `$v->p` for the binding `$v` holds ([`Vouch::VarProperty`]).
pub(crate) fn existence_vouch(cx: &Cx, store: &Store, call: &CallExpr) -> Vec<Vouch> {
    if let Some(vouches) = member_guard_vouch(cx, store, call) {
        return vouches;
    }
    let Some(pred) = existence_predicate(cx, call) else {
        return Vec::new();
    };
    match pred {
        "function_exists" => {
            if !call.positional_only || call.args.len() != 1 {
                return Vec::new();
            }
            let ArgValue::Str(name) = &call.args[0].value else {
                return Vec::new();
            };
            name.as_str()
                .map(|n| Vouch::Function(n.trim_start_matches('\\').to_ascii_lowercase()))
                .into_iter()
                .collect()
        }
        // global constants (ADR-0078, issue #198)
        //
        // `defined('X')` vouches nothing, on purpose: `constant.undefined` is
        // judged by a file-wide pass with no branch store, like `class.undefined`,
        // and takes its guard leg from dead-region pruning instead (see
        // `constant_defined_verdict`). Likewise `extension_loaded`: it names no symbol.
        // The arm exists so the class-predicate arm below can't mistake a constant
        // or an extension name for a class name. `method_exists` is read by
        // `member_guard_vouch` above, and answers here only when that declined.
        "defined" | "extension_loaded" | "method_exists" => Vec::new(),
        // end global constants (ADR-0078, issue #198)
        _ => {
            if !call.positional_only || call.args.is_empty() {
                return Vec::new();
            }
            existence_class_literal(cx, &call.args[0].value)
                .map(|name| Vouch::Class(name.trim_start_matches('\\').to_ascii_lowercase()))
                .into_iter()
                .collect()
        }
    }
}

/// Which member a member guard asks about.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MemberGuard {
    Method,
    Property,
}

/// A recognized member guard's parts: `method_exists($r, 'm')`, `is_callable([$r, 'm'])`
/// (both [`MemberGuard::Method`]) and `property_exists($r, 'p')`.
struct MemberShape<'a> {
    kind: MemberGuard,
    receiver: &'a ArgValue,
    member: &'a ArgValue,
}

/// Read `call` as a member guard of the global builtin, or `None`. Positional
/// arguments only; the `is_callable` pair is its only argument, in either key spelling.
fn member_guard_shape<'a>(cx: &Cx, call: &'a CallExpr) -> Option<MemberShape<'a>> {
    let callee = global_function_callee(cx, call)?;
    if !call.positional_only {
        return None;
    }
    if callee.eq_ignore_ascii_case("method_exists") {
        let [recv, name] = call.args.as_slice() else { return None };
        Some(MemberShape { kind: MemberGuard::Method, receiver: &recv.value, member: &name.value })
    } else if callee.eq_ignore_ascii_case("property_exists") {
        let [recv, name] = call.args.as_slice() else { return None };
        Some(MemberShape {
            kind: MemberGuard::Property,
            receiver: &recv.value,
            member: &name.value,
        })
    } else if callee.eq_ignore_ascii_case("is_callable") {
        let [arg] = call.args.as_slice() else { return None };
        let ArgValue::Array(items) = &arg.value else { return None };
        let [(k0, recv), (k1, name)] = items.as_slice() else { return None };
        let positional =
            |k: &ArrayKey, i: i64| matches!(k, ArrayKey::Auto) || *k == ArrayKey::Int(i);
        if !positional(k0, 0) || !positional(k1, 1) {
            return None;
        }
        Some(MemberShape { kind: MemberGuard::Method, receiver: recv, member: name })
    } else {
        None
    }
}

/// The vouches of the member-existence guards, or `None` when `call` is not one. The
/// method name is lowercased (PHP method names are case-insensitive), the property name
/// kept as written.
fn member_guard_vouch(cx: &Cx, store: &Store, call: &CallExpr) -> Option<Vec<Vouch>> {
    let shape = member_guard_shape(cx, call)?;
    let ArgValue::Str(member) = shape.member else { return Some(Vec::new()) };
    let Some(member) = member.as_str() else { return Some(Vec::new()) };
    Some(match shape.kind {
        MemberGuard::Method => receiver_classes(cx, store, shape.receiver)
            .into_iter()
            .map(|class| Vouch::Method {
                class: class.trim_start_matches('\\').to_ascii_lowercase(),
                method: member.to_ascii_lowercase(),
            })
            .collect(),
        // Only a variable receiver names a binding the vouch can be about.
        MemberGuard::Property => match shape.receiver {
            ArgValue::Var(var) => {
                vec![Vouch::VarProperty { var: var.clone(), property: member.to_owned() }]
            }
            _ => Vec::new(),
        },
    })
}

/// The classes a method guard's receiver argument names: a literal class (`N::class`,
/// `'N'`), or the heap class of an allocation-proven `$var`. Empty for anything else —
/// in particular **not** the classes of a declared arm, which is a union the guard's
/// truth names only some of.
fn receiver_classes(cx: &Cx, store: &Store, receiver: &ArgValue) -> Vec<String> {
    let ArgValue::Var(v) = receiver else {
        return existence_class_literal(cx, receiver).into_iter().collect();
    };
    store.class_of(v).map(str::to_owned).into_iter().collect()
}
