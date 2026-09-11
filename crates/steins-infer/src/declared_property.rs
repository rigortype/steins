//! The declared-property lane (ADR-0049 A20, issue #620): what a property read
//! answers when the trace holds no fact for that `(object, property)` pair.
//!
//! ADR-0036 keeps declaration-derived property values **out of the heap** on
//! purpose — a property's value in an arbitrary method is whatever some other
//! method last stored, so seeding the declared default would manufacture null
//! properties past a `!== null` guard. That ruling is about the *value* lane. It
//! says nothing against the property's declared **type**, which PHP enforces on
//! every write to a natively typed slot and which is therefore a sound upper bound
//! on whatever was stored. This module is that upper bound, read below the heap
//! and never written into it.

use steins_contract::ContractTy;
use steins_syntax::{ClassDecl, PropertyDecl};

use crate::contract::{neutralize_templates, template_names_of};
use crate::cx::Cx;
use crate::dispatch::declared_receiver_class;
use crate::env::{ContractArm, Store};
use crate::heap::parse_var_type;
use crate::refine::{expand_enum_case_arms, refine_contract_arms};
use crate::return_arms::{demote_arms, native_arms};

/// A property declaration found on a class's own chain, with the file that
/// declared it — the docblock read needs the file, since a `@var Foo` names
/// whatever `Foo` means in the *declaring* namespace and `use` scope, not in the
/// reader's (the same reason [`Cx::find_ctor`] hands its file back).
struct FoundProperty<'a> {
    decl: &'a PropertyDecl,
    class_file: usize,
    class: &'a ClassDecl,
}

/// Resolve `prop` through `class_fqn`'s `extends` chain, nearest declaration first
/// — the same walk [`Cx::class_props`] does, stopping at the first class that
/// declares the name rather than collecting the whole surface.
///
/// Three outcomes, and the two refusals are the load-bearing half (they mirror
/// [`builtin_root`]'s, one rung over):
///
/// * a class on the chain **declares the property** — that declaration is the
///   answer, whether it is the receiver's own class or an ancestor's. A child
///   redeclaration wins, because the walk meets it first; PHP requires a
///   redeclared typed property to keep the parent's type exactly, so the two can
///   only differ in their docblocks;
/// * the chain **leaves the project** (a builtin ancestor, which has no
///   [`ClassDecl`] for `find_class` to answer with) — nothing. A builtin's
///   property types are a mined table this project does not have; that is issue
///   #715, and inventing an answer here would pre-empt it;
/// * the chain **ends** without the name — nothing. An undeclared read is the
///   member-absence family's question, not this lane's.
///
/// `static` properties are skipped outright (ADR-0052 N5: owner-deferred), as
/// [`Cx::class_props`] skips them.
///
/// [`builtin_root`]: crate::dispatch
fn find_declared_property<'a>(
    cx: &Cx<'a>,
    class_fqn: &str,
    prop: &str,
) -> Option<FoundProperty<'a>> {
    let mut cur = class_fqn.to_owned();
    let mut seen = std::collections::HashSet::new();
    loop {
        if !seen.insert(cur.to_ascii_lowercase()) {
            return None; // a cyclic `extends` is a declaration fatal, not this lane's.
        }
        let (file, cd) = cx.find_class(&cur)?;
        if let Some(decl) = cd.properties.iter().find(|p| !p.is_static && p.name == prop) {
            return Some(FoundProperty { decl, class_file: file, class: cd });
        }
        let pref = cd.parent.as_ref()?;
        cur = cx.units[file].tree.resolve_class_fqn(pref);
    }
}

/// The contract arms a depth-1 property read `$var->prop` answers from its
/// **declaration**, or `None` when the lane says nothing (silence).
///
/// The carrier is the receiver's declared class through the declared-receiver lane
/// (ADR-0049 A17) — [`declared_receiver_class`], the very function the
/// declaration-only dispatch path reads its receiver with, so a property read and a
/// method call on the same `$o` can never disagree about what `$o` is declared to
/// be. An allocation, an `instanceof` fact, a narrowed one-class contract lane and
/// a declared heap object all arrive here; a surviving lane of two or more class
/// arms declines, for A17's reason (the answer would be a union of two classes'
/// declarations and this slice does not build that floor).
///
/// What the lane then reads off the declaration is exactly what A16 reads off a
/// method's return:
///
/// * the **native** hint at its native stratum. PHP enforces a typed property's
///   type on every write — including writes through a reference, and including the
///   `get` hook's return value — so the hint is a sound upper bound on the value
///   read out, under every descendant (a redeclaration must keep the type
///   identically; there is no property-type covariance to worry about);
/// * the **`@var`** merged at `Asserted` (ADR-0069 §2's grade), rendered
///   `(asserted)` by the dump surface and never premising a proof-layer finding —
///   [`refine_contract_arms`] already merges the two halves and marks the docblock
///   side, exactly as it does for a declared return;
/// * **unrepresentable** hints lower to nothing: `array`, `iterable`, `mixed`,
///   `object`, DNF — [`native_arms`] carries no member for them, so a property with
///   such a hint and no `@var` produces an empty list and this function answers
///   `None`, the same silence an absent hint gives.
///
/// The receiver's own stratum demotes the whole answer ([`demote_arms`], A13/A17):
/// a native `int` property read through an `@param Foo $o` receiver is `Asserted`,
/// because the premise that `$o` is a `Foo` at all is.
///
/// **A hooked property (PHP 8.4 `get`/`set`) answers nothing.** Its value is
/// whatever arbitrary user code computes, and this crate's standing rule for the
/// hooked surface is that it binds no fact ever (FP class 16) — the read side keeps
/// that rule rather than reasoning separately about which half of a hook pair PHP
/// type-checks. A class-body hooked declaration does not even survive lowering, so
/// the name is asked for separately, as the write side asks it.
///
/// **The heap wins.** This is a floor: every caller consults its in-trace
/// `(object, property)` fact first and only reaches here when there is none.
///
/// One route past the enforcement argument is known and deliberately not gated
/// (ADR-0049 A21): `unset($o->typed)` removes a typed property's slot, so a later
/// read on a class declaring `__get` reaches the magic method, which may return
/// anything. What this lane feeds is the introspection surface, where it answers
/// what the upstream oracle answers at the same site, and no proof-layer finding
/// premises on it. A reader that ever wants to convict on a declared property type
/// must re-ask the question with [`magic_obstacles_in_reach`] in hand.
///
/// [`magic_obstacles_in_reach`]: crate::absence::magic_obstacles_in_reach
pub(crate) fn declared_property_arms(
    cx: &Cx,
    store: &Store,
    var: &str,
    prop: &str,
) -> Option<Vec<ContractArm>> {
    let (class, receiver_stratum) = declared_receiver_class(cx, store, var)?;
    let found = find_declared_property(cx, &class, prop)?;
    if found.decl.hooked || cx.class_body_hooked(&class, prop) {
        return None;
    }
    let arms = property_arms(cx, &found)?;
    Some(demote_arms(arms, receiver_stratum))
}

/// The native + `@var` merge for one located declaration — the body of
/// [`declared_property_arms`] below the carrier, split out so the resolution rules
/// above and the lowering rules here read separately.
///
/// Class names inside the `@var` resolve in the **declaring** class's file at the
/// property's own offset, which is what makes an inherited `@var Foo` mean what its
/// author wrote. Class-level `@template` names shadow same-named classes in the
/// type (issue #5) — a property docblock is a member docblock too, and the write
/// side ([`apply_prop_assign`]) neutralizes them for the same reason.
///
/// [`apply_prop_assign`]: crate::heap::apply_prop_assign
fn property_arms(cx: &Cx, found: &FoundProperty<'_>) -> Option<Vec<ContractArm>> {
    let native: Vec<ContractTy> =
        found.decl.ty.as_ref().map(native_arms).unwrap_or_default();
    let mut phpdoc = found.decl.docblock.as_deref().and_then(parse_var_type);
    if let Some(pt) = phpdoc.as_mut() {
        neutralize_templates(pt, &template_names_of(found.class.docblock.as_deref()));
    }
    let off = found.decl.span.start;
    let file = found.class_file;
    let resolve = |n: &str| {
        cx.resolve_pclass(file, off, n).trim_start_matches('\\').to_ascii_lowercase()
    };
    let mut arms = refine_contract_arms(&native, phpdoc.as_ref(), &resolve)?;
    // The finite enum domain travels this direction too (issue #429): a `Suit $s`
    // property declares the same enforced case set a `: Suit` return does.
    expand_enum_case_arms(cx, &mut arms);
    Some(arms)
}
