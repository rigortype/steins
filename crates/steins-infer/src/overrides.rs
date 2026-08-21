//! Declaration-incompatibility fatals (ADR-0078 / issue #183 + #184):
//! `class.abstract-unimplemented`, `class.extends-final`, and the rest of PHPStan's
//! `OverridingMethodRule` surface — `override.final`, `override.static-mismatch`,
//! `override.visibility-weakened`, `override.parameter-variance`,
//! `override.return-variance`.

use std::collections::HashSet;

use steins_contract::normalize;
use steins_domain::Certainty;
use steins_syntax::{ClassDecl, MethodDecl, NameRef, NativeType, Param, Span, Visibility};

use crate::generics::native_to_contract;
use crate::{
    CLASS_ABSTRACT_UNIMPLEMENTED_ID, CLASS_EXTENDS_FINAL_ID, Cx, Diagnostic, OVERRIDE_FINAL_ID,
    OVERRIDE_PARAMETER_VARIANCE_ID, OVERRIDE_RETURN_VARIANCE_ID, OVERRIDE_STATIC_MISMATCH_ID,
    OVERRIDE_VISIBILITY_WEAKENED_ID, Res, in_dead, native_arms,
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
// leaves which declaration binds to load order). No sidecar leg needed.
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
// The one deliberate asymmetry: an unresolvable INTERFACE is dropped rather than
// silencing the class — it can only ADD requirements (no bodies), so dropping it
// loses findings but never manufactures one. A parent is a *definition* source, so
// an unresolvable one silences the whole claim.
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

/// A subject class's enumerated ancestry (ADR-0078 / issue #183).
struct Ancestry<'a> {
    /// The `extends` chain, SUBJECT FIRST. The only member source that can define a
    /// method body, so this is what answers "is the requirement discharged?" — and
    /// why it must be enumerable end to end.
    chain: Vec<&'a ClassDecl>,
    /// The transitively collected interfaces (`implements` on every chain node, plus
    /// each interface's own `extends` list). Requirement sources only; unresolvable
    /// ones are dropped, per the asymmetry above.
    interfaces: Vec<&'a ClassDecl>,
    /// Whether any consulted declaration is `conditional` (ADR-0049 A2i): the claim
    /// is re-dammed when one is.
    any_conditional: bool,
}

/// Enumerate `subject`'s ancestry, or `None` when any obstacle taints it (silence).
fn enumerate_ancestry<'a>(cx: &Cx<'a>, subject: &'a ClassDecl) -> Option<Ancestry<'a>> {
    let mut chain: Vec<&'a ClassDecl> = Vec::new();
    let mut iface_refs: Vec<String> = Vec::new();
    let mut any_conditional = false;
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
        // Trait obstacle (leg (e), header above) — witnessed silent: `trait T {
        // public function m() {} } class C implements I { use T; }` runs clean.
        if cd.uses_traits {
            return None;
        }
        any_conditional |= cd.conditional;
        iface_refs.extend(cd.implements.iter().map(|r| cx.units[file].tree.resolve_class_fqn(r)));
        chain.push(cd);
        cur = match &cd.parent {
            None => None,
            // A2 leg (header above): Absent or Ambiguous parent ⇒ silence.
            Some(pref) => Some(cx.find_class(&cx.units[file].tree.resolve_class_fqn(pref))?),
        };
    }

    let mut interfaces: Vec<&'a ClassDecl> = Vec::new();
    let mut iseen: HashSet<String> = HashSet::new();
    while let Some(fqn) = iface_refs.pop() {
        if !iseen.insert(fqn.to_ascii_lowercase()) {
            continue; // interface diamonds are legal PHP — dedupe, never an obstacle.
        }
        // The interface asymmetry (header above): dropped, not an obstacle.
        let Some((ifile, idecl)) = cx.find_class(&fqn) else { continue };
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

    Some(Ancestry { chain, interfaces, any_conditional })
}

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
fn method_satisfaction(chain: &[&ClassDecl], name: &str) -> Satisfaction {
    let mut saw_abstract = false;
    for node in chain {
        let Some(m) = node.methods.iter().find(|m| m.name.eq_ignore_ascii_case(name)) else {
            continue;
        };
        if !m.is_abstract {
            return if saw_abstract { Satisfaction::Refused } else { Satisfaction::Concrete };
        }
        saw_abstract = true;
    }
    Satisfaction::Missing
}

/// The display FQN of a class-like declaration (its declared casing and namespace),
/// falling back to the simple name for a declaration whose FQN was never stamped.
fn decl_display(cd: &ClassDecl) -> String {
    if cd.display.is_empty() { cd.name.clone() } else { cd.display.clone() }
}

/// Run the ADR-0078 ladder for one class-like declaration and emit
/// `class.abstract-unimplemented` iff every leg holds.
fn check_abstract_unimplemented(cx: &Cx, cd: &ClassDecl, out: &mut Vec<Diagnostic>) {
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
    let Some(anc) = enumerate_ancestry(cx, cd) else { return };

    // Requirements: every abstract method on the chain plus every interface method
    // (an interface method lowers with `is_abstract` set — it has no body). Deduped
    // by name, nearest declarer first, exactly as the runtime message lists them.
    let mut required: Vec<AbstractRequirement> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for node in anc.chain.iter().chain(anc.interfaces.iter()) {
        for m in node.methods.iter().filter(|m| m.is_abstract) {
            if seen.insert(m.name.to_ascii_lowercase()) {
                required
                    .push(AbstractRequirement { name: m.name.clone(), declarer: decl_display(node) });
            }
        }
    }

    let mut missing: Vec<String> = Vec::new();
    for req in &required {
        match method_satisfaction(&anc.chain, &req.name) {
            Satisfaction::Concrete => {}
            Satisfaction::Missing => missing.push(format!("{}::{}", req.declarer, req.name)),
            Satisfaction::Refused => return,
        }
    }
    if missing.is_empty() {
        return;
    }
    // A2i: a conditional declaration among the consulted set re-dams the claim.
    if anc.any_conditional && !cx.dam.is_clear() {
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
/// or an anonymous class's), iff the parent resolves uniquely to a `final` class.
fn check_extends_final(
    cx: &Cx,
    subject: &str,
    subject_conditional: bool,
    pref: &NameRef,
    out: &mut Vec<Diagnostic>,
) {
    let fqn = cx.class_fqn(pref);
    // A2 leg: Absent (issue #182's `class.undefined` territory) or Ambiguous ⇒ the
    // parent's finality is not proven.
    let Some((_, parent)) = cx.find_class(&fqn) else { return };
    // An enum lowers with `is_final` set (enums are implicitly final), but extending
    // one is the different fatal `Class C cannot extend enum E` (witnessed) — out.
    if !parent.is_final || parent.is_enum {
        return;
    }
    // A2i: a conditionally-declared `final class F` may not be the F that binds.
    if (subject_conditional || parent.conditional) && !cx.dam.is_clear() {
        return;
    }
    let pos = cx.tree().position(pref.offset);
    out.push(Diagnostic {
        id: CLASS_EXTENDS_FINAL_ID,
        path: cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        message: format!(
            "class {subject} cannot extend final class {} — fatal when the class is loaded",
            decl_display(parent),
        ),
        facet: None,
        fix: None,
    });
}

/// The per-file declaration-fatal pass (ADR-0078 / issue #183). Declarations in a
/// proven-dead region are skipped, exactly as the `class.undefined` pass skips
/// references in one.
pub(crate) fn check_declaration_fatals(cx: &Cx, dead: &[Span], out: &mut Vec<Diagnostic>) {
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
            check_extends_final(cx, &decl_display(cd), cd.conditional, pref, out);
        }
        check_abstract_unimplemented(cx, cd, out);
        // overriding family (ADR-0078, issue #184)
        check_override_family(cx, cd, out);
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
            check_extends_final(cx, "anonymous class", false, pref, out);
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
    method: &'a MethodDecl,
    declarer: String,
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
fn override_parents<'a>(anc: &Ancestry<'a>, name: &str) -> Option<Vec<OverrideParent<'a>>> {
    let mut parents: Vec<OverrideParent<'a>> = Vec::new();
    for node in anc.chain.iter().skip(1) {
        if let Some(m) = node.methods.iter().find(|m| m.name.eq_ignore_ascii_case(name)) {
            if m.visibility == Visibility::Private {
                return None;
            }
            parents.push(OverrideParent { method: m, declarer: decl_display(node) });
            break; // nearest declaration binds; farther ones are already compatible.
        }
    }
    for node in &anc.interfaces {
        if let Some(m) = node.methods.iter().find(|m| m.name.eq_ignore_ascii_case(name)) {
            parents.push(OverrideParent { method: m, declarer: decl_display(node) });
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
    subject: &ClassDecl,
    cm: &MethodDecl,
    parent: &OverrideParent,
    out: &mut Vec<Diagnostic>,
) -> bool {
    let pm = parent.method;
    let pos = cx.tree().position(cm.span.start);
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

/// Run the overriding family over one class declaration's own methods.
fn check_override_family(cx: &Cx, cd: &ClassDecl, out: &mut Vec<Diagnostic>) {
    // Only a CLASS declaration is the subject in v1. An enum's and a trait's members
    // are not lowered (ADR-0043); `interface I extends J` re-declaring a method is
    // the same fatal, but the ancestry walk `enumerate_ancestry` performs is
    // class-shaped (it refuses an interface node outright) — a recorded silence.
    if cd.is_interface || cd.is_enum || cd.is_trait {
        return;
    }
    // A2 leg, as the tracer's: a duplicate FQN leaves which declaration binds to
    // load order, so the subject's own signature is not the proven one.
    if !matches!(cx.index.resolve_class(&cd.fqn), Res::Unique(_)) {
        return;
    }
    let Some(anc) = enumerate_ancestry(cx, cd) else { return };
    // A2i, as the tracer's: a conditional declaration among the consulted set leaves
    // which signature binds to load order, so a standing dam re-dams the claim.
    if anc.any_conditional && !cx.dam.is_clear() {
        return;
    }
    for cm in &cd.methods {
        let Some(parents) = override_parents(&anc, &cm.name) else { continue };
        for parent in &parents {
            if emit_override_violation(cx, cd, cm, parent, out) {
                break; // one runtime fatal, one finding.
            }
        }
    }
}
// end overriding family (ADR-0078, issue #184)
