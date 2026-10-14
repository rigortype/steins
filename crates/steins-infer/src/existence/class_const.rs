//! The `defined('C::K')` verdict (read-position run S0c, ADR-0049 A25).
//!
//! `defined()` on a class-constant spelling asks the *running* program whether
//! `C::K` resolves **from this scope**. Witnessed at 8.5.11: the class name is
//! case-insensitive and a leading `\` is accepted, the constant name is
//! case-sensitive, an enum case and an interface constant answer `true`, and an
//! inaccessible constant (a `private` one from outside, a `protected` one from an
//! unrelated scope, a `private` one of an ancestor) answers `false` rather than
//! throwing. An unknown class answers `false` too, but only the project's classes
//! are folded here.
//!
//! ```text
//! spelling                              declared on the reach   accessible here   verdict
//! 'C::K' / '\Ns\C::K' (named class)     nowhere (reach closed)  -                 No
//!                                       once, public/case       -                 Yes
//!                                       once, private/protected visible           Yes
//!                                       once, private/protected invisible         No
//!                                       once, scope not exact   -                 Maybe
//!                                       twice, or any obstacle  -                 Maybe
//! 'self::K' 'static::K' 'parent::K'     -                       -                 Maybe
//! anything else ('C::class', 'C::', …)  -                       -                 Maybe
//! ```
//!
//! "Obstacle" is the reach discipline of `class-const.undefined`: an unresolvable or
//! ambiguous edge, a trait-using node, a member-incomplete file, a conditional
//! declaration with the dam standing, a boot-surface homonym. `No` discharges the
//! true branch of the guard (so `class-const.undefined` and
//! `class-const.inaccessible` are silent inside it); `Yes` discharges the false one.
//! The false branch of a `Maybe` keeps both.

use std::collections::HashSet;

use steins_domain::Certainty;
use steins_syntax::Visibility;

use crate::absence::provides_class_const;
use crate::contract::IsA;
use crate::cx::Cx;
use crate::early_binding::redammed;
use crate::fold::Folder;
use crate::inaccessible::{class_scope_known, private_invisible};
use crate::walk::WalkCx;

/// Split a `defined()` argument into `(class, constant)` when it spells a named
/// class and a plain constant; `None` for `self`/`static`/`parent`, `::class`, or a
/// malformed name.
fn split_spelling(name: &str) -> Option<(&str, &str)> {
    let (class, konst) = name.split_once("::")?;
    let class = class.strip_prefix('\\').unwrap_or(class);
    let ident = |s: &str| {
        let mut chars = s.chars();
        chars.next().is_some_and(|c| c == '_' || c.is_ascii_alphabetic() || !c.is_ascii())
            && chars.all(|c| c == '_' || c.is_ascii_alphanumeric() || !c.is_ascii())
    };
    if !class.split('\\').all(ident) || !ident(konst) || konst.eq_ignore_ascii_case("class") {
        return None;
    }
    if !class.contains('\\')
        && ["self", "static", "parent"].iter().any(|k| class.eq_ignore_ascii_case(k))
    {
        return None;
    }
    Some((class, konst))
}

/// What the reach holds of one constant name.
struct Provider {
    fqn: String,
    /// `None` for an enum case (always public).
    visibility: Option<Visibility>,
    at_start: bool,
}

/// Walk the reach (parents and interfaces) collecting every provider of `name`.
/// `None` for any obstacle or when the conditional/homonym legs do not close.
fn providers(
    cx: &Cx,
    folder: &mut dyn Folder,
    start: &str,
    name: &str,
) -> Option<Vec<Provider>> {
    let mut found = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut fqns: Vec<String> = Vec::new();
    let mut any_conditional = false;
    let mut stack = vec![start.to_owned()];
    while let Some(fqn) = stack.pop() {
        if !seen.insert(fqn.to_ascii_lowercase()) {
            continue;
        }
        let (cfile, cd) = cx.find_class(&fqn)?;
        if cx.member_incomplete(cfile) || cd.is_trait || cd.uses_traits {
            return None;
        }
        if provides_class_const(cd, name) {
            let visibility =
                cd.const_visibility.iter().find(|(n, _)| n == name).map(|(_, v)| *v);
            found.push(Provider { fqn: fqn.clone(), visibility, at_start: seen.len() == 1 });
        }
        any_conditional |= cd.conditional;
        fqns.push(fqn.clone());
        let tree = cx.units[cfile].tree;
        for r in cd.parent.iter().chain(cd.implements.iter()) {
            stack.push(tree.resolve_class_fqn(r));
        }
    }
    if redammed(cx, any_conditional) {
        return None;
    }
    for fqn in &fqns {
        if folder.boot_surface_class_like(fqn) != Some(false) {
            return None;
        }
    }
    Some(found)
}

/// Whether `scope` provably sees a `protected` member of `declaring`: it is the
/// declaring class or related to it in either direction.
fn protected_visible(cx: &Cx, declaring: &str, scope: &str) -> IsA {
    if scope.eq_ignore_ascii_case(declaring) {
        return IsA::Yes;
    }
    match (cx.is_a(scope, declaring), cx.is_a(declaring, scope)) {
        (IsA::Yes, _) | (_, IsA::Yes) => IsA::Yes,
        (IsA::No, IsA::No) => IsA::No,
        _ => IsA::Unknown,
    }
}

/// The verdict of `defined('<class>::<const>')` at `w`'s scope; see the module doc.
pub(super) fn class_const_defined_verdict(
    w: &WalkCx,
    folder: &mut dyn Folder,
    name: &str,
) -> Certainty {
    let cx = w.cx;
    let Some((class, konst)) = split_spelling(name) else {
        return Certainty::Maybe;
    };
    let Some(found) = providers(cx, folder, class, konst) else {
        return Certainty::Maybe;
    };
    if found.is_empty() {
        return Certainty::No; // reach closed, `K` absent: defined nowhere in it.
    }
    let [p] = found.as_slice() else {
        return Certainty::Maybe;
    };
    // `No` and `Yes` for a non-public constant need the scope's class known exactly:
    // a closure (or arrow fn) can be bound to any class, and a trait method runs in
    // its user's scope. Top level and free functions have no class scope at all.
    let scope = w.enclosing_class;
    let scope_exact = class_scope_known(w.scope)
        && scope.is_none_or(|s| cx.find_class(s).is_some_and(|(_, cd)| !cd.is_trait));
    match p.visibility {
        None | Some(Visibility::Public) => Certainty::Yes,
        Some(Visibility::Private) if !p.at_start => {
            // Not inherited: invisible whatever the scope (witnessed, scope P included).
            Certainty::No
        }
        Some(_) if !scope_exact => Certainty::Maybe,
        Some(Visibility::Private) => {
            if private_invisible(&p.fqn, scope) { Certainty::No } else { Certainty::Yes }
        }
        Some(Visibility::Protected) => {
            let Some(scope) = scope else { return Certainty::No };
            match protected_visible(cx, &p.fqn, scope) {
                IsA::Yes => Certainty::Yes,
                IsA::No => Certainty::No,
                IsA::Unknown => Certainty::Maybe,
            }
        }
    }
}
