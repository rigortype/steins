//! Declaration-incompatibility fatals (ADR-0078 / issue #183 + #184):
//! `class.abstract-unimplemented`, `class.extends-final`, and the rest of PHPStan's
//! `OverridingMethodRule` surface — `override.final`, `override.static-mismatch`,
//! `override.visibility-weakened`, `override.parameter-variance`,
//! `override.return-variance`.

use std::collections::HashSet;

use steins_contract::normalize;
use steins_domain::Certainty;
use steins_sidecar::{
    ReflectedClass, ReflectedClassKind, ReflectedMethod, Visibility as EngineVisibility,
};
use steins_syntax::{ClassDecl, MethodDecl, NameRef, NativeType, Param, Span, Visibility};

use crate::fold::Folder;
use crate::generics::native_to_contract;
use crate::cx::Cx;
use crate::trait_composition::HeldMethod;
use crate::early_binding::redammed;
use crate::project::{Diagnostic, Res};
use crate::return_arms::native_arms;
use crate::walk::in_dead;
use crate::{
    CLASS_ABSTRACT_UNIMPLEMENTED_ID, CLASS_EXTENDS_FINAL_ID, OVERRIDE_FINAL_ID,
    OVERRIDE_PARAMETER_VARIANCE_ID, OVERRIDE_RETURN_VARIANCE_ID, OVERRIDE_STATIC_MISMATCH_ID,
    OVERRIDE_VISIBILITY_WEAKENED_ID,
};

// ---------------------------------------------------------------------------
// The declaration-incompatibility fatals: `class.abstract-unimplemented` and
// `class.extends-final` (ADR-0078, issue #183 — the member-kind port's P5 tracer).
//
// Both read the DECLARATION GRAPH only — the same edges `resolve_in_chain` walks,
// read in the other direction. No flow analysis, no value domain, no receiver: PHP
// decides loadability at class load, before any statement runs. `php -r`-witnessed
// (PHP 8.5.9): `abstract class B { abstract public function m(); } class C extends
// B {}` → `Fatal error: Class C contains 1 abstract method and must therefore be
// declared abstract or implement the remaining method (B::m)`; `interface I {
// public function m(); } class C implements I {}` fatals the same way, `(I::m)` —
// an interface is a requirement source like an abstract ancestor; adding `public
// function __call($n, $a) {}` to C does NOT discharge it (unlike
// `call.undefined-method`'s leg (d), the magic fallback is not an obstacle here);
// and `final class F {} class C extends F {}` → `Class C cannot extend final class
// F` (also for `abstract class C`, and `new class extends F {}` → `Class
// F@anonymous cannot extend final class F`).
//
// The dam (ADR-0046/0049 A5) does NOT gate these ids — the immunity asymmetry
// (ADR-0049 A2) for a positive claim: only symbol *existence* is dammed (eval/
// out-of-universe includes mint names but cannot reopen a declared class to add a
// missing method body), and the fatal happens at declaration, not at a later call.
// A2's identification legs still apply: every consulted ancestor and the subject's
// own FQN must resolve UNIQUELY, and a `conditional` declaration anywhere re-dams
// the claim (the `if (!class_exists('F')) { final class F {} }` polyfill-stub
// leaves which declaration binds to load order). No sidecar leg for PROJECT
// ancestors; an ENGINE ancestor (issue #939) is read off the analysing PHP's own
// reflection under the rule table below.
//
// Silence legs: a `use`d trait ANYWHERE in the chain (members not flattened, leg
// (e) — could implement the method invisibly); an unresolvable/ambiguous PARENT
// (the implementation could live there); a misshapen edge (`extends` naming an
// interface/enum/trait, `implements` naming a non-interface — its own load-time
// fatal); the "Cannot make non abstract method A::m() abstract in class B" shape
// (fatals at that ancestor's own declaration, misnaming the subject); enums/traits
// at the declaration site (no members lowered) and abstract classes/interfaces
// (allowed abstract methods); anonymous classes for `class.abstract-unimplemented`
// only (`new class` lowers edge-only, ADR-0049 A4 — no members, unimplemented
// claim unfounded; `class.extends-final` needs no members and covers them).
//
// The one deliberate asymmetry: an unresolvable INTERFACE (one neither the project
// nor the analysing PHP declares) is dropped rather than silencing the class — it can only ADD requirements (no bodies), so dropping it
// loses findings but never manufactures one. A parent is a *definition* source, so
// an unresolvable one (project- and engine-absent) silences the whole claim.
// ---------------------------------------------------------------------------

/// How many unimplemented method names a `class.abstract-unimplemented` message
/// spells before summarizing the rest. PHP's own fatal truncates at three
/// (`(B::a, B::b, B::c, ...)`, witnessed); the message says how many it dropped.
const ABSTRACT_NAMES_IN_MESSAGE: usize = 3;

/// One inherited abstract method the subject must define: the method name as
/// written and the display FQN of the class-like declaring it abstract — the pair
/// PHP's own fatal renders (`(App\B::m)`, witnessed).
struct AbstractRequirement {
    name: String,
    declarer: String,
}

// engine ancestors (ADR-0078 amendment, issue #939)
// ---------------------------------------------------------------------------
// ENGINE ANCESTORS — the rule table. An ancestor no project file declares is asked
// of the analysing PHP (`Folder::reflected_class`); the answer is the engine's own
// declaration, and the pass JUDGES from it only where the engine's fatal is
// witnessed (PHP 8.5.11, `php -r`, each row), DECLINES where the fatal is a
// different one, and stays SILENT where PHP only deprecates.
//
// ancestor kind × check                      → verdict
//   engine PARENT, final                     → judge `class.extends-final` (Closure,
//                                              Generator, WeakMap); every other check
//                                              declines (one fatal, one finding)
//   engine PARENT, not a plain class         → decline (`extends` naming an interface
//                                              or enum is its own fatal)
//   engine PARENT abstract method, unimpl.   → judge `class.abstract-unimplemented`
//                                              (FilterIterator::accept); a concrete
//                                              reflected method discharges a
//                                              requirement (ArrayIterator::count)
//   engine INTERFACE method, unimplemented   → judge `class.abstract-unimplemented`
//                                              (Countable, JsonSerializable, Stringable,
//                                              IteratorAggregate, a project interface
//                                              extending one)
//   engine final method overridden           → judge `override.final`
//   engine method made static/non-static     → judge `override.static-mismatch`
//   engine visibility narrowed               → judge `override.visibility-weakened`
//   engine parameter COUNT incompatible      → judge `override.parameter-variance`:
//                                              required count above the engine's, or
//                                              fewer parameters than the engine's with
//                                              no variadic. Reflection carries counts
//                                              only, so a parameter TYPE mismatch has
//                                              no engine-side premise: silence
//   engine return type vs a child's          → SILENT, never `override.return-variance`:
//                                              a tentative return type mismatch is a
//                                              deprecation that reaches
//                                              `set_error_handler` (D7), and a real
//                                              (non-tentative) engine return type is
//                                              not parsed here either
//   Throwable / Traversable / DateTimeInterface / UnitEnum / BackedEnum named directly
//     in an `implements` list or an interface's `extends`, and Iterator together with
//     IteratorAggregate anywhere in the set → decline (each is its own fatal)
//   engine class absent from the analysing PHP, no live engine, an origin that is not
//     internal, or a run where the absence family is unavailable → silence
//
// The gate is `Folder::absence_family_available`: a live engine inside the declared
// target range with no runtime-redefinition extension, the same witness the
// absence family asks for. A reflected declaration was envelope-grade (the
// 2026-08-09 "resolves, does not convict" ruling); this amendment promotes it to a
// premise for these ids only, each backed by a fatal PHP raised on the witness run.
// ---------------------------------------------------------------------------

/// The interfaces whose direct `implements`/`extends` naming is a different fatal
/// from this pass's ids (witnessed e1–e4, e10): refused, not judged.
const REFUSED_ENGINE_INTERFACES: [&str; 5] =
    ["throwable", "traversable", "datetimeinterface", "unitenum", "backedenum"];

/// The engine class-likes a subject's chain ends in, as the analysing PHP declares
/// them. Owned: the folder's memo is mutably borrowed per query.
#[derive(Default)]
struct EngineAncestry {
    /// The engine class the `extends` chain terminates in, if any. Its method list is
    /// the RESOLVED set, inherited members and the interfaces it implements included.
    parent: Option<ReflectedClass>,
    /// Engine interfaces named by a project class or interface in the chain: each a
    /// requirement source whose methods are all abstract.
    interfaces: Vec<ReflectedClass>,
}

/// One class of the `extends` chain and the methods it holds: its own, then what its
/// traits give it (ADR-0105).
struct ChainNode<'a> {
    cd: &'a ClassDecl,
    held: Vec<HeldMethod<'a>>,
}

impl<'a> ChainNode<'a> {
    /// The method `name` this node holds, its own before an imported one (C1).
    fn held(&self, name: &str) -> Option<&HeldMethod<'a>> {
        self.held.iter().find(|m| m.decl.name.eq_ignore_ascii_case(name))
    }
}

/// A subject class's enumerated ancestry (ADR-0078 / issue #183).
struct Ancestry<'a> {
    /// The `extends` chain, SUBJECT FIRST. The only member source that can define a
    /// method body, so this is what answers "is the requirement discharged?" — and
    /// why it must be enumerable end to end.
    chain: Vec<ChainNode<'a>>,
    /// The transitively collected interfaces (`implements` on every chain node, plus
    /// each interface's own `extends` list). Requirement sources only; unresolvable
    /// ones are dropped, per the asymmetry above.
    interfaces: Vec<&'a ClassDecl>,
    /// The engine declarations behind the project's edges (issue #939).
    engine: EngineAncestry,
    /// Whether any consulted declaration is `conditional` (ADR-0049 A2i): the claim
    /// is re-dammed when one is.
    any_conditional: bool,
}

/// The engine's own declaration of `fqn`, when the analysing PHP has it as an
/// INTERNAL class-like. `None` — absent, unanswerable, or user-land (a class a
/// bootstrap loaded is project code this pass has no declaration of).
fn engine_class(folder: &mut dyn Folder, fqn: &str) -> Option<ReflectedClass> {
    let decl = folder.reflected_class(fqn.trim_start_matches('\\'))?.declaration?;
    decl.internal.then_some(decl)
}

/// Whether `fqn` names no project class and the analysing PHP may be asked about it.
fn engine_askable(cx: &Cx, engine_ok: bool, fqn: &str) -> bool {
    engine_ok && cx.class_absent(fqn)
}

/// Enumerate `subject`'s ancestry, or `None` when any obstacle taints it (silence).
fn enumerate_ancestry<'a>(
    cx: &Cx<'a>,
    folder: &mut dyn Folder,
    subject: &'a ClassDecl,
) -> Option<Ancestry<'a>> {
    let engine_ok = folder.absence_family_available();
    let mut chain: Vec<ChainNode<'a>> = Vec::new();
    let mut iface_refs: Vec<String> = Vec::new();
    let mut any_conditional = false;
    let mut engine = EngineAncestry::default();
    let mut seen: HashSet<String> = HashSet::new();
    let mut cur: Option<(usize, &'a ClassDecl)> = Some((cx.cur, subject));

    while let Some((file, cd)) = cur {
        if !seen.insert(cd.fqn.to_ascii_lowercase()) {
            return None; // a cycle — PHP refuses it too, and closure cannot terminate.
        }
        // A parent chain runs through CLASSES. `extends` naming an interface, enum or
        // trait is its own load-time fatal ("cannot extend"), never this id's business.
        if cd.is_interface || cd.is_enum || cd.is_trait {
            return None;
        }
        // What the class holds through its traits counts as its own (ADR-0105) —
        // witnessed: `trait T { public function m() {} } class C implements I { use
        // T; }` runs clean. A composition that declines is the trait obstacle.
        let held = cx.held_methods(file, cd).ok()?;
        any_conditional |= cd.conditional;
        iface_refs.extend(cd.implements.iter().map(|r| cx.units[file].tree.resolve_class_fqn(r)));
        chain.push(ChainNode { cd, held });
        cur = match &cd.parent {
            None => None,
            Some(pref) => {
                let fqn = cx.units[file].tree.resolve_class_fqn(pref);
                if let Some(found) = cx.find_class(&fqn) {
                    Some(found)
                } else if engine_askable(cx, engine_ok, &fqn) {
                    // An engine parent ends the chain. Its resolved method list stands
                    // in for every ancestor above it; a final or non-class parent is a
                    // different fatal (`class.extends-final` / "cannot extend").
                    let parent = engine_class(folder, &fqn)?;
                    if parent.kind != ReflectedClassKind::Class || parent.is_final {
                        return None;
                    }
                    engine.parent = Some(parent);
                    None
                } else {
                    // A2 leg (header above): Absent or Ambiguous parent ⇒ silence.
                    return None;
                }
            }
        };
    }

    let mut interfaces: Vec<&'a ClassDecl> = Vec::new();
    let mut iseen: HashSet<String> = HashSet::new();
    while let Some(fqn) = iface_refs.pop() {
        if !iseen.insert(fqn.to_ascii_lowercase()) {
            continue; // interface diamonds are legal PHP — dedupe, never an obstacle.
        }
        let Some((ifile, idecl)) = cx.find_class(&fqn) else {
            // The interface asymmetry (header above): an interface nothing declares is
            // dropped, not an obstacle — unless the analysing PHP has it (issue #939).
            if engine_askable(cx, engine_ok, &fqn) {
                if REFUSED_ENGINE_INTERFACES.contains(&fqn.to_ascii_lowercase().as_str()) {
                    return None;
                }
                if let Some(decl) = engine_class(folder, &fqn) {
                    if decl.kind != ReflectedClassKind::Interface {
                        return None; // "cannot implement X - it is not an interface".
                    }
                    engine.interfaces.push(decl);
                }
            }
            continue;
        };
        // `implements` naming a non-interface is another fatal entirely ("I cannot
        // implement F - it is not an interface", witnessed).
        if !idecl.is_interface || idecl.uses_traits {
            return None;
        }
        any_conditional |= idecl.conditional;
        if let Some(pref) = &idecl.parent {
            iface_refs.push(cx.units[ifile].tree.resolve_class_fqn(pref));
        }
        iface_refs
            .extend(idecl.implements.iter().map(|r| cx.units[ifile].tree.resolve_class_fqn(r)));
        interfaces.push(idecl);
    }

    // `Iterator` together with `IteratorAggregate` is its own fatal (witnessed e9).
    let mut engine_names: HashSet<String> = HashSet::new();
    for decl in engine.interfaces.iter().chain(engine.parent.iter()) {
        engine_names.insert(decl.name.to_ascii_lowercase());
        engine_names.extend(decl.interfaces.iter().map(|n| n.to_ascii_lowercase()));
    }
    if engine_names.contains("iterator") && engine_names.contains("iteratoraggregate") {
        return None;
    }

    Some(Ancestry { chain, interfaces, engine, any_conditional })
}
// end engine ancestors (ADR-0078 amendment, issue #939)

/// Whether a requirement is discharged by the enumerated class chain.
enum Satisfaction {
    /// The nearest declaration of the name carries a body — implemented.
    Concrete,
    /// No chain node declares the name with a body — the fatal.
    Missing,
    /// The nearest declaration is abstract while a FARTHER ancestor's is concrete:
    /// the "Cannot make non abstract method A::m() abstract in class B" shape, which
    /// fatals at that ancestor's own declaration. Naming the subject would misname
    /// the consequence — refuse the whole claim.
    Refused,
}

/// Resolve `name` against the class chain (subject first), nearest declaration wins
/// — the same first-wins rule [`resolve_in_chain`] applies to dispatch.
///
/// [`resolve_in_chain`]: crate::resolve_in_chain
///
/// An abstract method a trait gives a node is a requirement, never a declaration that
/// shadows an inherited one (ADR-0105 C8a: an inherited concrete method implements
/// it), so the walk passes it by.
fn method_satisfaction(anc: &Ancestry, name: &str) -> Satisfaction {
    let mut saw_abstract = false;
    for node in &anc.chain {
        let Some(held) = node.held(name) else {
            continue;
        };
        let m = held.decl;
        if m.is_abstract && held.imported() {
            continue;
        }
        if !m.is_abstract {
            return if saw_abstract { Satisfaction::Refused } else { Satisfaction::Concrete };
        }
        saw_abstract = true;
    }
    // The chain's engine tail: the reflected parent's resolved method set. A private
    // method is not inherited, so it neither satisfies nor is judged — refuse.
    if let Some(m) = anc.engine.parent.as_ref().and_then(|p| engine_method(p, name)) {
        if m.visibility == EngineVisibility::Private {
            return Satisfaction::Refused;
        }
        if !m.is_abstract {
            return if saw_abstract { Satisfaction::Refused } else { Satisfaction::Concrete };
        }
    }
    Satisfaction::Missing
}

/// The class-like that DECLARES the engine method `name` the subject inherits through
/// `class` — the name PHP's own fatal renders. Reflection carries no declaring class,
/// so this climbs the `extends` chain while the parent reflects the same member
/// shape, then (for an abstract member) prefers the interface that carries it.
/// A best-effort display name: it never changes a verdict.
fn engine_declarer(folder: &mut dyn Folder, class: &ReflectedClass, name: &str) -> String {
    let mut cur = class.clone();
    let Some(mut shape) = engine_method(&cur, name).cloned() else { return class.name.clone() };
    while let Some(parent) = cur.parent.clone() {
        let Some(up) = engine_class(folder, &parent) else { break };
        match engine_method(&up, name) {
            Some(m) if same_shape(m, &shape) => {
                shape = m.clone();
                cur = up;
            }
            _ => break,
        }
    }
    if shape.is_abstract {
        for iface in cur.interfaces.clone() {
            if let Some(decl) = engine_class(folder, &iface)
                && engine_method(&decl, name).is_some()
            {
                return decl.name;
            }
        }
    }
    cur.name
}

/// The reflected method `name` on `class`, case-insensitively.
fn engine_method<'r>(class: &'r ReflectedClass, name: &str) -> Option<&'r ReflectedMethod> {
    class.methods.iter().find(|m| m.name.eq_ignore_ascii_case(name))
}

/// Whether two reflections of one method name are the same declaration to the eye.
fn same_shape(a: &ReflectedMethod, b: &ReflectedMethod) -> bool {
    a.is_abstract == b.is_abstract
        && a.is_final == b.is_final
        && a.is_static == b.is_static
        && a.visibility == b.visibility
        && a.params_total == b.params_total
        && a.params_required == b.params_required
        && a.return_type == b.return_type
}

/// The display FQN of a class-like declaration (its declared casing and namespace),
/// falling back to the simple name for a declaration whose FQN was never stamped.
fn decl_display(cd: &ClassDecl) -> String {
    if cd.display.is_empty() { cd.name.clone() } else { cd.display.clone() }
}

/// Whether a chain node above `at`, or the engine parent, holds a method `name`.
fn inherited_holds(anc: &Ancestry, at: usize, name: &str) -> bool {
    anc.chain[at + 1..].iter().any(|node| node.held(name).is_some())
        || anc.engine.parent.as_ref().is_some_and(|p| engine_method(p, name).is_some())
}

/// Run the ADR-0078 ladder for one class-like declaration and emit
/// `class.abstract-unimplemented` iff every leg holds.
fn check_abstract_unimplemented(
    cx: &Cx,
    folder: &mut dyn Folder,
    cd: &ClassDecl,
    out: &mut Vec<Diagnostic>,
) {
    // Declaration-site gate: only a CONCRETE class must implement everything. An
    // abstract class and an interface may carry abstract methods (witnessed silent);
    // a trait's and an enum's members are not lowered at all (an enum cannot be
    // abstract, and its own `Enum E must implement …` fatal is a different message).
    if cd.is_abstract || cd.is_interface || cd.is_enum || cd.is_trait {
        return;
    }
    // A2 leg: a duplicate FQN leaves which declaration binds to load order.
    if !matches!(cx.index.resolve_class(&cd.fqn), Res::Unique(_)) {
        return;
    }
    let Some(anc) = enumerate_ancestry(cx, folder, cd) else { return };

    // Requirements: every abstract method on the chain plus every interface method
    // (an interface method lowers with `is_abstract` set — it has no body). Deduped
    // by name, nearest declarer first, exactly as the runtime message lists them.
    let mut required: Vec<AbstractRequirement> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (at, node) in anc.chain.iter().enumerate() {
        for held in node.held.iter().filter(|m| m.decl.is_abstract) {
            // An abstract trait method binds to an inherited method of its name, and
            // PHP names that one (witnessed: `(P::m)`); else it is the importer's own
            // requirement, named `Importer::m` (ADR-0105 C8).
            if held.imported() && inherited_holds(&anc, at, &held.decl.name) {
                continue;
            }
            if seen.insert(held.decl.name.to_ascii_lowercase()) {
                required.push(AbstractRequirement {
                    name: held.decl.name.clone(),
                    declarer: decl_display(node.cd),
                });
            }
        }
    }
    for node in &anc.interfaces {
        for m in node.methods.iter().filter(|m| m.is_abstract) {
            if seen.insert(m.name.to_ascii_lowercase()) {
                required
                    .push(AbstractRequirement { name: m.name.clone(), declarer: decl_display(node) });
            }
        }
    }
    // The engine's requirements (issue #939): the reflected parent's abstract methods,
    // then every method of an engine interface a project edge names.
    for engine in anc.engine.parent.iter().chain(anc.engine.interfaces.iter()) {
        for m in engine.methods.iter().filter(|m| m.is_abstract) {
            if seen.insert(m.name.to_ascii_lowercase()) {
                let declarer = engine_declarer(folder, engine, &m.name);
                required.push(AbstractRequirement { name: m.name.clone(), declarer });
            }
        }
    }

    let mut missing: Vec<String> = Vec::new();
    for req in &required {
        match method_satisfaction(&anc, &req.name) {
            Satisfaction::Concrete => {}
            Satisfaction::Missing => missing.push(format!("{}::{}", req.declarer, req.name)),
            Satisfaction::Refused => return,
        }
    }
    if missing.is_empty() {
        return;
    }
    // A2i: a conditional declaration among the consulted set re-dams the claim.
    if redammed(cx, anc.any_conditional) {
        return;
    }

    let pos = cx.tree().position(cd.span.start);
    let count = missing.len();
    let listed = if count > ABSTRACT_NAMES_IN_MESSAGE {
        format!(
            "{}, and {} more",
            missing[..ABSTRACT_NAMES_IN_MESSAGE].join(", "),
            count - ABSTRACT_NAMES_IN_MESSAGE
        )
    } else {
        missing.join(", ")
    };
    let plural = if count == 1 { "method" } else { "methods" };
    out.push(Diagnostic {
        id: CLASS_ABSTRACT_UNIMPLEMENTED_ID,
        path: cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        message: format!(
            "class {} leaves {count} inherited abstract {plural} unimplemented ({listed}) — fatal when the class is loaded",
            decl_display(cd),
        ),
        facet: None,
        fix: None,
    });
}

/// Emit `class.extends-final` for one `extends` edge (a named class declaration's
/// or an anonymous class's), iff the parent resolves uniquely to a `final` class —
/// or, when no project file declares it, the analysing PHP reflects it as a final
/// internal class (`Closure`, `Generator`, `WeakMap`; issue #939).
fn check_extends_final(
    cx: &Cx,
    folder: &mut dyn Folder,
    subject: &str,
    subject_conditional: bool,
    pref: &NameRef,
    out: &mut Vec<Diagnostic>,
) {
    let fqn = cx.class_fqn(pref);
    // A2 leg: Absent (issue #182's `class.undefined` territory) or Ambiguous ⇒ the
    // parent's finality is not proven — except an Absent name the engine declares.
    let parent_display = match cx.find_class(&fqn) {
        Some((_, parent)) => {
            // An enum lowers with `is_final` set (enums are implicitly final), but
            // extending one is the different fatal `Class C cannot extend enum E`
            // (witnessed) — out.
            if !parent.is_final || parent.is_enum {
                return;
            }
            // A2i: a conditionally-declared `final class F` may not be the F that binds.
            if redammed(cx, subject_conditional || parent.conditional) {
                return;
            }
            decl_display(parent)
        }
        None => {
            if !folder.absence_family_available() || !cx.class_absent(&fqn) {
                return;
            }
            if redammed(cx, subject_conditional) {
                return;
            }
            match engine_class(folder, &fqn) {
                Some(d) if d.kind == ReflectedClassKind::Class && d.is_final => d.name,
                _ => return,
            }
        }
    };
    let pos = cx.tree().position(pref.offset);
    out.push(Diagnostic {
        id: CLASS_EXTENDS_FINAL_ID,
        path: cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        message: format!(
            "class {subject} cannot extend final class {parent_display} — fatal when the class is loaded",
        ),
        facet: None,
        fix: None,
    });
}

/// The per-file declaration-fatal pass (ADR-0078 / issue #183). Declarations in a
/// proven-dead region are skipped, exactly as the `class.undefined` pass skips
/// references in one.
pub(crate) fn check_declaration_fatals(
    cx: &Cx,
    folder: &mut dyn Folder,
    dead: &[Span],
    out: &mut Vec<Diagnostic>,
) {
    for cd in cx.tree().classes() {
        if in_dead(dead, cd.span.start) {
            continue;
        }
        // `extends` on an interface names interfaces, and enums/traits cannot extend
        // at all — only a class declaration can carry this fatal.
        if !cd.is_interface
            && !cd.is_enum
            && !cd.is_trait
            && let Some(pref) = cd.parent.as_ref()
        {
            check_extends_final(cx, folder, &decl_display(cd), cd.conditional, pref, out);
        }
        check_abstract_unimplemented(cx, folder, cd, out);
        // overriding family (ADR-0078, issue #184)
        check_override_family(cx, folder, cd, out);
        // end overriding family (ADR-0078, issue #184)
    }
    // Anonymous classes carry the same extends-final fatal (`Class F@anonymous cannot
    // extend final class F`, witnessed) and need no members to prove it. They are
    // never `conditional` in the A2i sense: the flag is about which declaration a
    // NAME binds to, and an anonymous class has no name to contest.
    for edge in cx.tree().anonymous_class_edges() {
        if in_dead(dead, edge.span.start) {
            continue;
        }
        if let Some(pref) = edge.parent.as_ref() {
            check_extends_final(cx, folder, "anonymous class", false, pref, out);
        }
    }
}

// overriding family (ADR-0078, issue #184)
// ---------------------------------------------------------------------------
// The rest of PHPStan's `OverridingMethodRule` surface: `override.final`,
// `override.static-mismatch`, `override.visibility-weakened`,
// `override.parameter-variance`, `override.return-variance` — fatals PHP raises
// **at class load**, off the same declaration graph the tracer above reads, so
// they share its closure discipline verbatim (unique ancestor resolution, silence
// on a `use`d trait anywhere in the chain, no sidecar leg, no dam gate;
// `enumerate_ancestry` reused as-is). v1 judges native signatures only — a
// docblock premise is Asserted (ADR-0037/0052 N2) and PHP ignores docblocks for
// this fatal, so it's absent here, not demoted; the phpdoc twin waits on
// ADR-0032's generics carry.
//
// Rules, each witnessed on PHP 8.5.9 (`php -r`, legal counterparts confirmed clean):
//   final — overriding a `final` method fatals (through a grandparent, from an
//     `abstract`/anonymous child, for `__construct`); a final CHILD is legal.
//   static mismatch — fatal both directions; `__construct` excluded (a separate,
//     parent-less fatal).
//   visibility — narrowing fatals (public→protected/private, protected→private);
//     widening is clean.
//   parameter variance (contravariance) — narrowing the accepted set fatals
//     (`int|string`→`int`, `?int`→`int`, untyped→`int`, `iterable`→`array`,
//     `bool`→`true`); widening/dropping/renaming/adding an OPTIONAL parameter are
//     clean. Deferred (own id, an arity change this name would misname):
//     adding/removing a REQUIRED parameter, by-ref mismatch.
//   return variance (covariance) — widening the promise fatals (`int`→`int|string`,
//     `int`→`?int`, `never`→`int`, `true`→`bool`); narrowing and adding a return
//     type over none are clean. Dropping the parent's return type is also a fatal
//     but a deliberate v1 silence: unrepresentable hints (`void`, `iterable`,
//     `mixed`, DNF) lower to the same `None` an absent hint gives, indistinguishable
//     — both sides must carry a lowered type.
//   __construct — exempt from visibility/variance only while the parent's
//     constructor is CONCRETE (abstract re-imposes both); not exempt from `final`.
//     No other magic method is exempt from anything.
//   private parent methods — silence: not inherited, so nothing to override.
//   interfaces — same path; `enumerate_ancestry` already collects the transitive set.
//   precedence — **final ≻ static ≻ visibility ≻ variance**, one finding per method.
//
// Loss-only gaps: a `bool` arm vs a `true`/`false` literal folds to `Maybe`; the
// relation carries PHP's weak-mode int→float widening, this pure subtype test
// doesn't. Measured against the whole native-type matrix (13×13 over parameter/
// return/interface/constructor positions, plus the modifier matrix — 774 fixtures,
// `php -r` on 8.5.9): zero false positives, 49 yield losses exactly the
// class-vs-class `Maybe` leg, the two allowances above, and the static-constructor
// exclusion.
//
// Further tested silences: an interface SUBJECT (class-shaped ancestry walk); an
// enum/anonymous-class subject (members not lowered, ADR-0043/ADR-0049 A4); a
// child method declared `abstract` over a concrete parent (`Satisfaction::Refused`);
// a `self`/`static`/`parent` return keyword (bound is the *declaring* class, would
// misname the comparison); a variadic or by-reference position on either side.
// ---------------------------------------------------------------------------

/// A parent-side declaration a subject method overrides: the method and the display
/// FQN of the class-like that declares it.
struct OverrideParent<'a> {
    sig: ParentSig<'a>,
    declarer: String,
}

/// Where a parent-side signature comes from: a project declaration, or the analysing
/// PHP's reflection of an engine class-like (issue #939).
enum ParentSig<'a> {
    Source(&'a MethodDecl),
    /// The reflected method and the reflected class it was read off (the display
    /// declarer is resolved lazily, only when a finding names it).
    Engine(&'a ReflectedMethod, &'a ReflectedClass),
}

/// Order visibility for the weakening test: `public` (2) is the widest.
fn visibility_rank(v: Visibility) -> u8 {
    match v {
        Visibility::Public => 2,
        Visibility::Protected => 1,
        Visibility::Private => 0,
    }
}

/// The PHP keyword for a visibility, for the message.
fn visibility_word(v: Visibility) -> &'static str {
    match v {
        Visibility::Public => "public",
        Visibility::Protected => "protected",
        Visibility::Private => "private",
    }
}

/// The parent declarations `name` overrides, nearest first: the nearest `extends`
/// chain declaration (if any), then every interface declaration of the name.
///
/// `None` — silence for the whole method — when the nearest chain declaration is
/// **private**: a private method is not inherited, so the subject is not overriding
/// anything through the chain, and any interface question that survives is the
/// *ancestor's* fatal, not the subject's (witnessed: `class A { public function
/// m(int|string $x) {} } class B extends A { private function m() {} } class C
/// extends B { public function m(int $x) {} }` fatals at **B**, naming B::m's access
/// level — naming C would misname it).
fn override_parents<'s>(anc: &'s Ancestry, name: &str) -> Option<Vec<OverrideParent<'s>>> {
    let mut parents: Vec<OverrideParent<'s>> = Vec::new();
    let mut found = false;
    for node in anc.chain.iter().skip(1) {
        if let Some(held) = node.held(name) {
            // An abstract trait method binds to an inherited one rather than
            // overriding it (ADR-0105 C9a), so the nearer declaration is farther up.
            if held.imported() && held.decl.is_abstract {
                continue;
            }
            let m = held.decl;
            if m.visibility == Visibility::Private {
                return None;
            }
            // A method a trait gives an ancestor is that ancestor's (`P::m`, C9).
            parents.push(OverrideParent { sig: ParentSig::Source(m), declarer: decl_display(node.cd) });
            found = true;
            break; // nearest declaration binds; farther ones are already compatible.
        }
    }
    // The chain's engine tail binds after every project node: the reflected parent's
    // resolved method set. A private engine method is not inherited either.
    if !found
        && let Some(ep) = &anc.engine.parent
        && let Some(m) = engine_method(ep, name)
    {
        if m.visibility == EngineVisibility::Private {
            return None;
        }
        parents.push(OverrideParent { sig: ParentSig::Engine(m, ep), declarer: ep.name.clone() });
    }
    for node in &anc.interfaces {
        if let Some(m) = node.methods.iter().find(|m| m.name.eq_ignore_ascii_case(name)) {
            parents.push(OverrideParent { sig: ParentSig::Source(m), declarer: decl_display(node) });
        }
    }
    for ei in &anc.engine.interfaces {
        if let Some(m) = engine_method(ei, name) {
            parents.push(OverrideParent { sig: ParentSig::Engine(m, ei), declarer: ei.name.clone() });
        }
    }
    Some(parents)
}

/// Whether a native type pair is comparable at all for variance: both sides must
/// carry a lowered [`NativeType`] (the syntax layer collapses every unrepresentable
/// hint to the same `None` an absent hint lowers to, so `None` proves nothing).
fn variance_pair<'a>(
    child: Option<&'a NativeType>,
    parent: Option<&'a NativeType>,
) -> Option<(&'a NativeType, &'a NativeType)> {
    Some((child?, parent?))
}

/// Whether `consumer` provably REFUSES some whole arm of `produced` — the one
/// variance question both directions of the LSP check reduce to.
///
/// Routes through **the** acceptance relation — `steins_contract`'s
/// `normalize::subsumes(a, b)` = "every value of `b`'s denotation is admitted by
/// `a`" (the `isSuperTypeOf` shape, ADR-0030 registry entry 5) — applied **arm-wise**
/// (as `dedup_arms`/`subtract` do), via `native_arms`'s decomposition. Arm-wise is
/// what makes this decidable: asked whole, `subsumes(int, int|string)` folds
/// `[Yes, No]` to `Maybe`, since the fold can't distinguish partial coverage from
/// ignorance. LSP asks the sharper question — is there an arm the consumer provably
/// rejects? — so each arm is judged alone and one `No` convicts; a `Maybe` arm never
/// does (two unrelated class arms judge only through the reflexive is-a floor, so
/// `Class(A)` vs `Class(B)` is `Maybe` and stays silent).
fn provably_refuses_an_arm(consumer: &NativeType, produced: &NativeType) -> bool {
    let c = native_to_contract(consumer);
    native_arms(produced).iter().any(|arm| normalize::subsumes(&c, arm) == Certainty::No)
}

/// The parameter-contravariance verdict for one overriding method: the index of the
/// first position the child provably NARROWS, or `None`. The child must accept
/// everything the parent's declaration accepts.
fn override_param_violation(child: &[Param], parent: &[Param]) -> Option<usize> {
    for (i, pp) in parent.iter().enumerate() {
        let cp = child.get(i)?;
        // A variadic or by-ref position on either side is a different shape (an
        // arity/binding change), deferred — see the header's witness table.
        if pp.variadic || pp.by_ref || cp.variadic || cp.by_ref {
            continue;
        }
        let Some((cty, pty)) = variance_pair(cp.ty.as_ref(), pp.ty.as_ref()) else { continue };
        // Contravariance: the child must accept everything the parent accepts.
        if provably_refuses_an_arm(cty, pty) {
            return Some(i);
        }
    }
    None
}

/// The return-covariance verdict: `true` when the child provably WIDENS the parent's
/// return type. The same acceptance relation, asked in the other direction — the
/// parent's declared return must subsume the child's.
fn override_return_widens(child: &MethodDecl, parent: &MethodDecl) -> bool {
    // A `self`/`static`/`parent` return keyword is synthesized to an `Instance` of
    // the DECLARING class (ADR-0043 amendment), so comparing the two sides would
    // compare `P` against `C` and misname whatever it found. Silence.
    if child.ret_bound_keyword.is_some() || parent.ret_bound_keyword.is_some() {
        return false;
    }
    let Some((cty, pty)) = variance_pair(child.ret.as_ref(), parent.ret.as_ref()) else {
        return false;
    };
    // Covariance: the parent's promise must cover everything the child returns.
    provably_refuses_an_arm(pty, cty)
}

/// Judge one subject method against one parent declaration and push the FIRST
/// violation in PHP's own witnessed precedence (final ≻ static ≻ visibility ≻
/// parameter ≻ return). Returns `true` when something was emitted, so the caller
/// stops at one finding per overriding method — one runtime fatal, one finding.
fn emit_override_violation(
    cx: &Cx,
    folder: &mut dyn Folder,
    subject: &ClassDecl,
    (cm, at): (&MethodDecl, u32),
    parent: &OverrideParent,
    out: &mut Vec<Diagnostic>,
) -> bool {
    let pm = match &parent.sig {
        ParentSig::Source(pm) => *pm,
        ParentSig::Engine(pm, class) => {
            return emit_engine_violation(cx, folder, subject, (cm, at), (pm, class), out);
        }
    };
    let pos = cx.tree().position(at);
    let child_name = format!("{}::{}", decl_display(subject), cm.name);
    let parent_name = format!("{}::{}", parent.declarer, pm.name);
    let mut emit = |id: &'static str, message: String| {
        out.push(Diagnostic {
            id,
            path: cx.path().to_owned(),
            line: pos.line,
            column: pos.column,
            message,
            facet: None,
            fix: None,
        });
    };

    // 1. `final` — the top of the precedence, and the one member `__construct` does
    //    not escape. An interface method can never be `final`, so this only ever
    //    fires against a chain declaration.
    if pm.is_final {
        emit(
            OVERRIDE_FINAL_ID,
            format!(
                "{child_name}() overrides final method {parent_name}() — fatal when the class is loaded"
            ),
        );
        return true;
    }
    // A child re-declaring a CONCRETE parent method `abstract` is the different fatal
    // `Cannot make non abstract method P::m() abstract in class C` (witnessed) —
    // naming it with one of these ids would misname the consequence.
    if cm.is_abstract && !pm.is_abstract {
        return false;
    }
    // 2. static/non-static, both directions. `__construct` is excluded: a static
    //    constructor is its own standalone fatal (witnessed).
    if cm.is_static != pm.is_static && !cm.is_constructor {
        let (verb, adj) = if cm.is_static { ("makes", "static") } else { ("makes", "non-static") };
        emit(
            OVERRIDE_STATIC_MISMATCH_ID,
            format!(
                "{child_name}() {verb} {parent_name}() {adj} — fatal when the class is loaded"
            ),
        );
        return true;
    }
    // A constructor escapes visibility and variance too, but only while the parent's
    // `__construct` is CONCRETE; an abstract one (an interface method, or an
    // `abstract` declaration) re-imposes both (witnessed).
    if cm.is_constructor && !pm.is_abstract {
        return false;
    }
    // 3. visibility weakened along public → protected → private.
    if visibility_rank(cm.visibility) < visibility_rank(pm.visibility) {
        emit(
            OVERRIDE_VISIBILITY_WEAKENED_ID,
            format!(
                "{child_name}() weakens the visibility of {parent_name}() from {} to {} — fatal when the class is loaded",
                visibility_word(pm.visibility),
                visibility_word(cm.visibility),
            ),
        );
        return true;
    }
    // 4. parameter contravariance.
    if let Some(i) = override_param_violation(&cm.params, &pm.params) {
        let cp = &cm.params[i];
        let pp = &pm.params[i];
        emit(
            OVERRIDE_PARAMETER_VARIANCE_ID,
            format!(
                "{child_name}() narrows parameter ${} from `{}` to `{}`, which {parent_name}() accepts — fatal when the class is loaded",
                cp.name,
                pp.ty.as_ref().expect("compared pair carries both types").render(),
                cp.ty.as_ref().expect("compared pair carries both types").render(),
            ),
        );
        return true;
    }
    // 5. return covariance.
    if override_return_widens(cm, pm) {
        emit(
            OVERRIDE_RETURN_VARIANCE_ID,
            format!(
                "{child_name}() widens the return type of {parent_name}() from `{}` to `{}` — fatal when the class is loaded",
                pm.ret.as_ref().expect("compared pair carries both types").render(),
                cm.ret.as_ref().expect("compared pair carries both types").render(),
            ),
        );
        return true;
    }
    false
}

/// The required parameter count a project method declares: the positions without a
/// default that are not variadic.
fn required_params(params: &[Param]) -> usize {
    params.iter().filter(|p| !p.has_default && !p.variadic).count()
}

/// Whether a project override is provably INCOMPATIBLE in arity with an engine method
/// (witnessed: `ArrayIterator::count()` overridden as `count($x)`; `seek(int $offset)`
/// overridden as `seek()` or `seek(int $a, int $b)`; `count(int $x = 1)` is fine):
/// the child demands more arguments than the engine's callers always pass, or accepts
/// fewer than the engine's declaration without a variadic to collect the rest.
/// Reflection carries counts only, so types and by-reference shapes are not judged.
fn engine_arity_violation(cm: &MethodDecl, pm: &ReflectedMethod) -> Option<String> {
    let child_required = required_params(&cm.params);
    let child_total = cm.params.len();
    let child_variadic = cm.params.iter().any(|p| p.variadic);
    if child_required > pm.params_required as usize {
        return Some(format!(
            "requires {child_required} parameters where the engine's declaration requires {}",
            pm.params_required
        ));
    }
    if !child_variadic && child_total < pm.params_total as usize {
        return Some(format!(
            "accepts {child_total} parameters where the engine's declaration accepts {}",
            pm.params_total
        ));
    }
    None
}

/// The engine twin of [`emit_override_violation`]: one subject method against a
/// reflected engine method, in the same precedence (final ≻ static ≻ visibility ≻
/// parameter), with the return comparison DROPPED — a tentative return mismatch is a
/// deprecation (D7), so no `override.return-variance` is ever premised on a reflection.
fn emit_engine_violation(
    cx: &Cx,
    folder: &mut dyn Folder,
    subject: &ClassDecl,
    (cm, at): (&MethodDecl, u32),
    (pm, class): (&ReflectedMethod, &ReflectedClass),
    out: &mut Vec<Diagnostic>,
) -> bool {
    let pos = cx.tree().position(at);
    let child_name = format!("{}::{}", decl_display(subject), cm.name);
    let parent_name = format!("{}::{}", engine_declarer(folder, class, &pm.name), pm.name);
    let mut emit = |id: &'static str, message: String| {
        out.push(Diagnostic {
            id,
            path: cx.path().to_owned(),
            line: pos.line,
            column: pos.column,
            message,
            facet: None,
            fix: None,
        });
    };
    if pm.is_final {
        emit(
            OVERRIDE_FINAL_ID,
            format!(
                "{child_name}() overrides final method {parent_name}() — fatal when the class is loaded"
            ),
        );
        return true;
    }
    if cm.is_abstract && !pm.is_abstract {
        return false;
    }
    if cm.is_static != pm.is_static && !cm.is_constructor {
        let adj = if cm.is_static { "static" } else { "non-static" };
        emit(
            OVERRIDE_STATIC_MISMATCH_ID,
            format!("{child_name}() makes {parent_name}() {adj} — fatal when the class is loaded"),
        );
        return true;
    }
    if cm.is_constructor && !pm.is_abstract {
        return false;
    }
    let (parent_rank, from) = match pm.visibility {
        EngineVisibility::Public => (2, "public"),
        EngineVisibility::Protected => (1, "protected"),
        EngineVisibility::Private => (0, "private"),
    };
    if visibility_rank(cm.visibility) < parent_rank {
        emit(
            OVERRIDE_VISIBILITY_WEAKENED_ID,
            format!(
                "{child_name}() weakens the visibility of {parent_name}() from {from} to {} — fatal when the class is loaded",
                visibility_word(cm.visibility),
            ),
        );
        return true;
    }
    if let Some(why) = engine_arity_violation(cm, pm) {
        emit(
            OVERRIDE_PARAMETER_VARIANCE_ID,
            format!(
                "{child_name}() {why} and is not compatible with {parent_name}() — fatal when the class is loaded"
            ),
        );
        return true;
    }
    false
}

/// Run the overriding family over one class declaration's own methods.
fn check_override_family(
    cx: &Cx,
    folder: &mut dyn Folder,
    cd: &ClassDecl,
    out: &mut Vec<Diagnostic>,
) {
    // Only a CLASS declaration is the subject in v1. An enum cannot extend, and a
    // trait's methods are judged in each class that imports them; `interface I
    // extends J` re-declaring a method is the same fatal, but the ancestry walk
    // `enumerate_ancestry` performs is class-shaped (it refuses an interface node
    // outright) — a recorded silence.
    if cd.is_interface || cd.is_enum || cd.is_trait {
        return;
    }
    // A2 leg, as the tracer's: a duplicate FQN leaves which declaration binds to
    // load order, so the subject's own signature is not the proven one.
    if !matches!(cx.index.resolve_class(&cd.fqn), Res::Unique(_)) {
        return;
    }
    let Some(anc) = enumerate_ancestry(cx, folder, cd) else { return };
    // A2i, as the tracer's: a conditional declaration among the consulted set leaves
    // which signature binds to load order, so a standing dam re-dams the claim.
    if redammed(cx, anc.any_conditional) {
        return;
    }
    // The subject's own methods, and the concrete ones its traits give it: PHP checks
    // an imported method against the parent's as the importer's own (`C::m`,
    // ADR-0105 C9), reported at the `use` it came through. An abstract imported
    // method binds to the inherited one instead (C9a) and is never judged here.
    let Some(subject) = anc.chain.first() else { return };
    for held in subject.held.iter().filter(|m| !(m.imported() && m.decl.is_abstract)) {
        let Some(parents) = override_parents(&anc, &held.decl.name) else { continue };
        let at = held.via.map_or(held.decl.span.start, |r| r.offset);
        for parent in &parents {
            if emit_override_violation(cx, folder, cd, (held.decl, at), parent, out) {
                break; // one runtime fatal, one finding.
            }
        }
    }
}
// end overriding family (ADR-0078, issue #184)
