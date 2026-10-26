//! Foldable existence-guard verdicts (ADR-0049 §4 / N3): `method_exists`,
//! `function_exists`, `class_exists` and kin answered from the project index with
//! the dam's vouch.

use steins_domain::Certainty;
use steins_syntax::{
    ArgValue, ArrayKey, CallExpr, ClassDecl, CondExpr, EXISTENCE_PREDICATES, LogicalOp, NameRef,
    OperandSpan, ReadKind, RefKind, StaticClass, Stmt,
};

use crate::absence::{ChainWalk, walk_method_chain};
use crate::cx::Cx;
use crate::early_binding::redammed;
use crate::dam::DamKind;
use crate::declared_receiver::{CALL_FALLBACK, declared_receiver_lacks_method};
use crate::env::{GuardVar, Store, Vouch};
use crate::fold::Folder;
use crate::project::{FnResolution, Res};
use crate::walk::WalkCx;

mod class_const;

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
        let Some(name) = defined_name(w.cx, &call.args[0].value) else {
            return Certainty::Maybe;
        };
        if name.contains("::") {
            return class_const::class_const_defined_verdict(w, folder, &name);
        }
        constant_defined_verdict(w.cx, folder, &name)
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
///
/// The two predicates differ on `__call`: `method_exists` reports declared methods only,
/// so a chain that answers `m` through an inherited `__call` still lacks it there, while
/// `is_callable` is true on it. Each asks the lane with its own magic fallback.
fn member_guard_verdict(
    w: &WalkCx,
    folder: &mut dyn Folder,
    store: &Store,
    call: &CallExpr,
) -> Option<Certainty> {
    let shape = member_guard_shape(w.cx, call)?;
    let magic = match shape.kind {
        MemberGuard::Method => None,
        MemberGuard::Callable => CALL_FALLBACK,
        MemberGuard::Property => return None,
    };
    let ArgValue::Var(var) = shape.receiver else { return None };
    let ArgValue::Str(method) = shape.member else { return None };
    let method = method.as_str()?;
    let lacks =
        declared_receiver_lacks_method(w.cx, folder, store, w.scope.poisoned, var, method, magic);
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
/// (visibility-blind), a class's own or one a trait gives it (ADR-0105). Any obstacle
/// to closure (an enum node, a trait composition that declines,
/// unresolvable ancestor, cycle, conditional node with the dam standing, a node in an
/// unparsable file for the absent answer, or an unanswerable/positive boot-surface
/// homonym) collapses to `Maybe`. The walk is the one the absence lanes judge with
/// ([`walk_method_chain`]), asked with no magic fallback.
fn method_exists_verdict(
    cx: &Cx,
    folder: &mut dyn Folder,
    start_fqn: &str,
    method: &str,
) -> Certainty {
    let Some((file, cd)) = cx.find_class(start_fqn) else {
        return Certainty::Maybe; // the class leaves the project / ambiguous.
    };
    let (present, fqns, any_conditional) =
        match walk_method_chain(cx, file, cd, start_fqn, method, None) {
            ChainWalk::Declared { fqns, any_conditional } => (true, fqns, any_conditional),
            ChainWalk::Absent { fqns, any_conditional, .. } => (false, fqns, any_conditional),
            ChainWalk::Magic { .. } | ChainWalk::Unknown => return Certainty::Maybe,
        };
    // A2i: a conditional declaration on the chain re-dams the claim — only the clear
    // whole-universe dam lets either verdict stand.
    if redammed(cx, any_conditional) {
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
            if redammed(cx, cx.fn_decl(site).conditional) {
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
/// A `C::K` spelling is not a global constant: [`class_const`] answers it.
///
/// `name` is case-sensitive on its final segment; [`steins_syntax::normalize_const_fqn`]
/// decides which half folds case.
/// The name a `defined()` argument spells: a string literal, or a concatenation of
/// string literals and `X::class` fetches of a named class (`defined(K::class .
/// '::NOPE')`). `X::class` is the class's fully qualified name without the leading
/// `\`, which is how `defined()` reads a name. `None` for anything else.
fn defined_name(cx: &Cx, arg: &ArgValue) -> Option<String> {
    match arg {
        ArgValue::Str(s) => s.as_str().map(str::to_owned),
        ArgValue::ClassConst(steins_syntax::StaticClass::Named(r), name)
            if name.eq_ignore_ascii_case("class") =>
        {
            Some(cx.class_fqn(r).trim_start_matches('\\').to_owned())
        }
        ArgValue::Concat(l, r) => Some(defined_name(cx, l)? + &defined_name(cx, r)?),
        _ => None,
    }
}

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
    if let Some(vouches) = callable_name_vouch(cx, call) {
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

/// The vouch of `is_callable('<name>')` with a literal string (the #1060 review): the
/// function it names, or `C::m` for a `"C::m"` string — the same symbols
/// `function_exists('f')` and `method_exists('C', 'm')` vouch, since a true answer
/// proves exactly that the name is callable. The array form is a member guard
/// ([`member_guard_shape`]). `None` when `call` is not this guard.
///
/// The arguments bind as PHP binds them, positionally or by name: `value`, then
/// `syntax_only`, then the by-reference `callable_name`. Only a check that asks for
/// real callability vouches — `syntax_only` absent or a literal `false`; a `true` or
/// an unproven one asks about the spelling alone (the #1060 re-review). A spread or
/// an unknown name declines.
fn callable_name_vouch(cx: &Cx, call: &CallExpr) -> Option<Vec<Vouch>> {
    let callee = global_function_callee(cx, call)?;
    if !callee.eq_ignore_ascii_case("is_callable") || call.has_spread {
        return None;
    }
    let mut bound: [Option<&ArgValue>; 3] = [None; 3];
    for (slot, arg) in bound.iter_mut().zip(&call.args) {
        *slot = Some(&arg.value);
    }
    if call.args.len() > 3 {
        return None;
    }
    for named in &call.named_args {
        let i = ["value", "syntax_only", "callable_name"].iter().position(|n| *n == named.name)?;
        if bound[i].replace(&named.value).is_some() {
            return None;
        }
    }
    if !matches!(bound[1], None | Some(ArgValue::Bool(false))) {
        return Some(Vec::new());
    }
    let name = match bound[0]? {
        ArgValue::Str(name) => name,
        // `is_callable(['C', 'm'], false)`: the pair as a member guard reads it,
        // for a literal class (the one-argument form is `member_guard_vouch`'s).
        ArgValue::Array(items) => {
            let [(k0, ArgValue::Str(class)), (k1, ArgValue::Str(method))] = items.as_slice() else {
                return Some(Vec::new());
            };
            let positional =
                |k: &ArrayKey, i: i64| matches!(k, ArrayKey::Auto) || *k == ArrayKey::Int(i);
            let (Some(class), Some(method)) = (class.as_str(), method.as_str()) else {
                return Some(Vec::new());
            };
            if !positional(k0, 0) || !positional(k1, 1) {
                return Some(Vec::new());
            }
            return Some(vec![Vouch::Method {
                class: class.trim_start_matches('\\').to_ascii_lowercase(),
                method: method.to_ascii_lowercase(),
            }]);
        }
        _ => return None,
    };
    let Some(name) = name.as_str() else { return Some(Vec::new()) };
    let name = name.trim_start_matches('\\');
    Some(match name.split_once("::") {
        Some((class, method)) => vec![Vouch::Method {
            class: class.trim_start_matches('\\').to_ascii_lowercase(),
            method: method.to_ascii_lowercase(),
        }],
        None => vec![Vouch::Function(name.to_ascii_lowercase())],
    })
}

/// Which member a member guard asks about, and how.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MemberGuard {
    /// `method_exists`: a declared method; `__call` does not count.
    Method,
    /// `is_callable([$r, 'm'])`: a method, or one `__call` answers.
    Callable,
    Property,
}

/// A recognized member guard's parts: `method_exists($r, 'm')`
/// ([`MemberGuard::Method`]), `is_callable([$r, 'm'])` ([`MemberGuard::Callable`]) and
/// `property_exists($r, 'p')`.
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
        Some(MemberShape { kind: MemberGuard::Callable, receiver: recv, member: name })
    } else {
        None
    }
}

/// Bind `var` to the existence guard the assignment `$var = value` stores, when it stores
/// one ([`Store::guard_vars`]): the guard call, its negation, or a conjunction with a guard
/// among its operands (`!G`, `G && x`). A disjunction, a comparison or any other shape stores
/// nothing: `$v = G || H` truthy says neither. The guard's arguments are literals or bare
/// variables other than `var`. Called after the assignment's own rebind of `var`, which
/// dropped any earlier guard, and never in a poisoned scope, where a reference, `extract`,
/// `global` or by-reference capture may rewrite `var` unseen.
pub(crate) fn remember_guard_var(
    w: &WalkCx,
    stmt: &Stmt,
    store: &mut Store,
    var: &str,
    value: &ArgValue,
) {
    if w.scope.poisoned {
        return;
    }
    let mut deps = Vec::new();
    let mut guards = 0usize;
    let Some(cond) = guard_cond_of(w, stmt, value, &mut deps, &mut guards) else { return };
    if guards == 0 || deps.iter().any(|d| d == var) {
        return;
    }
    store.guard_vars.insert(var.to_owned(), GuardVar { cond, deps });
}

/// The condition `value` stores, or `None` for a shape that is neither a guard nor built from
/// one. A conjunct that is not a guard is kept as an opaque operand: it can only make the
/// whole false, which a vouch never reads.
fn guard_cond_of(
    w: &WalkCx,
    stmt: &Stmt,
    value: &ArgValue,
    deps: &mut Vec<String>,
    guards: &mut usize,
) -> Option<CondExpr> {
    match value {
        ArgValue::Call(name, args) => {
            let call = call_with_arguments(w, stmt, name, args)?;
            if existence_predicate(w.cx, call).is_none() && member_guard_shape(w.cx, call).is_none()
                || !call.positional_only
                || !call.args.iter().all(|a| guard_arg_deps(&a.value, deps))
            {
                return None;
            }
            *guards += 1;
            Some(CondExpr::Call { call: Box::new(call.clone()), reads: Vec::new(), writes: Vec::new() })
        }
        ArgValue::Not(inner) => {
            guard_cond_of(w, stmt, inner, deps, guards).map(|c| CondExpr::Not(Box::new(c)))
        }
        ArgValue::Logical { op: LogicalOp::And, lhs, rhs, .. } => {
            let opaque = || CondExpr::Opaque { reads: Vec::new(), writes: Vec::new() };
            let l = guard_cond_of(w, stmt, lhs, deps, guards).unwrap_or_else(opaque);
            let r = guard_cond_of(w, stmt, rhs, deps, guards).unwrap_or_else(opaque);
            Some(CondExpr::And(Box::new(l), Box::new(r), OperandSpan::NONE))
        }
        _ => None,
    }
}

/// The lowered call of `stmt` named `name` whose arguments are `args`, among the call
/// positions the statement writes (the value lowering keeps no span for it).
fn call_with_arguments<'a>(
    w: &'a WalkCx,
    stmt: &'a Stmt,
    name: &str,
    args: &[ArgValue],
) -> Option<&'a CallExpr> {
    stmt.read_positions()
        .iter()
        .filter(|p| matches!(p.kind, ReadKind::Call))
        .filter_map(|p| crate::stmt_calls::call_at(w, stmt, p))
        .find(|c| {
            c.callee.as_deref() == Some(name)
                && c.args.len() == args.len()
                && c.args.iter().zip(args).all(|(a, b)| a.value == *b)
        })
}

/// The variables a guard argument names, or `false` for an argument that is not a literal, a
/// bare variable or an array of them.
fn guard_arg_deps(arg: &ArgValue, deps: &mut Vec<String>) -> bool {
    match arg {
        ArgValue::Var(v) => {
            deps.push(v.clone());
            true
        }
        ArgValue::Str(_) | ArgValue::Int(_) | ArgValue::Bool(_) | ArgValue::Null => true,
        ArgValue::Array(items) => items.iter().all(|(_, v)| guard_arg_deps(v, deps)),
        ArgValue::ClassConst(..) => true,
        _ => false,
    }
}

/// `store` with the vouches every **undecided** guard stored in a variable would give if it
/// held, on either side of a test of it: the view a position that may run under any of those
/// variables' tests is judged in (ADR-0104). The statement's own tests are not modeled in an
/// expression, so which variable it tests, and at which polarity, is not asked; vouching more
/// only silences. A decided guard vouches nothing: the arm it kills is a dead extent already,
/// and the arm it keeps really runs (`$h = !function_exists('f'); $h ? f() : 0`).
pub(crate) fn with_stored_guard_vouches(
    cx: &Cx,
    store: &Store,
    rebound: &dyn Fn(&str) -> bool,
    undecided: &mut dyn FnMut(&GuardVar) -> bool,
) -> Store {
    let mut out = store.clone();
    for (var, g) in &store.guard_vars {
        // A guard the statement itself may rebind is not the one its tests read.
        if rebound(var) || g.deps.iter().any(|d| rebound(d)) || !undecided(g) {
            continue;
        }
        for truthy in [true, false] {
            let mut calls = Vec::new();
            crate::refine::collect_guard_calls(&g.cond, truthy, &mut calls);
            for (call, returns_true) in calls {
                if returns_true {
                    for v in existence_vouch(cx, &out, call) {
                        out.vouch(v);
                    }
                }
            }
        }
    }
    out
}

/// Whether `call` is a member-existence guard (`method_exists`, `property_exists`,
/// `is_callable`) of the global builtin: a test whose truth vouches a member for the one
/// binding it names, which a same-expression use sees only through a branch store.
pub(crate) fn is_member_guard_call(cx: &Cx, call: &CallExpr) -> bool {
    global_function_callee(cx, call).is_some_and(|callee| {
        ["method_exists", "property_exists", "is_callable"]
            .iter()
            .any(|g| callee.eq_ignore_ascii_case(g))
    })
}

/// The vouches of the member-existence guards, or `None` when `call` is not one. The
/// method name is lowercased (PHP method names are case-insensitive), the property name
/// kept as written.
fn member_guard_vouch(cx: &Cx, store: &Store, call: &CallExpr) -> Option<Vec<Vouch>> {
    let shape = member_guard_shape(cx, call)?;
    let ArgValue::Str(member) = shape.member else { return Some(Vec::new()) };
    let Some(member) = member.as_str() else { return Some(Vec::new()) };
    Some(match shape.kind {
        MemberGuard::Method | MemberGuard::Callable => receiver_classes(cx, store, shape.receiver)
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
