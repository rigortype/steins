//! The finding-breadth absence family (ADR-0049 §4): `call.undefined-method` (the
//! flagship), the member ladder `property.undefined` / `property.maybe-undefined` /
//! `class-const.undefined` (issue #197), the existence ids `call.undefined-function`
//! / `class.undefined`, and `constant.undefined` (issue #198). Every id here is an
//! absence proof and is dam-gated (ADR-0046).

use std::collections::HashSet;

use steins_syntax::{
    CallExpr, Callee, ClassDecl, NameRef, Receiver, RefKind, Span, StaticClass, normalize_const_fqn,
};

use crate::cx::Cx;
use crate::early_binding::{Binding, class_use, redammed};
use crate::dump::resolved_fn_fqn;
use crate::env::{Store, Stratum};
use crate::project::{Diagnostic, MagicObstacle, Res};
use crate::walk::WalkCx;
use crate::fold::Folder;
use crate::{
    CALL_UNDEFINED_FUNCTION_ID, CALL_UNDEFINED_METHOD_ID, CLASS_CONST_UNDEFINED_ID,
    CLASS_UNDEFINED_ID, CONSTANT_UNDEFINED_ID, PROPERTY_MAYBE_UNDEFINED_ID, PROPERTY_UNDEFINED_ID,
    is_dump_family_fqn,
};
use crate::declared_receiver::{
    ArmFate, DescendantClosure, Held, LaneArm, MemberLadder, declared_receiver_conjuncts,
    descendant_closure, judge_arm,
};

// ---------------------------------------------------------------------------
// The finding-breadth flagship: `call.undefined-method` (ADR-0049 §4 / S2).
//
// An *absence* proof — fire only under complete closure over every place a method
// could hide. The ladder (ADR-0049 §4 + amendments A1/A2/A3/A9) is applied leg by
// leg; ANY doubt is silence (the zero-FP identity, ADR-0013). The cheap textual
// legs run first so the sidecar homonym IPC (A2ii) is reached only for a chain
// that already survived every local check.
// ---------------------------------------------------------------------------

/// Which magic fallback swallows an otherwise-undefined call, and the PHP phrasing
/// of the call kind — instance (`$recv->m()`, `__call`) vs static (`C::m()`,
/// `__callStatic`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum UndefKind {
    Instance,
    Static,
}

impl UndefKind {
    /// The magic method whose presence anywhere in the chain makes the call defined.
    fn magic(self) -> &'static str {
        match self {
            UndefKind::Instance => "__call",
            UndefKind::Static => "__callStatic",
        }
    }
}

/// The receiver a `call.undefined-method` claim can rest on, after legs (a)/(l).
/// Carries the *exact* receiver class FQN and the call kind. `None` from the
/// resolver means the receiver is out of scope for S2 (silence): `$this` (A1
/// membership), an inexact/lower-bound variable, a nullsafe `?->`,
/// `self`/`static`/`parent`, or any dynamic form. A first-class callable `$v->m(...)` /
/// `C::m(...)` is a site like a call (ADR-0104): PHP resolves the method when it builds the
/// `Closure`, and throws the same `Error` when it is missing.
fn undefined_method_receiver(
    cx: &Cx,
    call: &CallExpr,
    store: &Store,
    poisoned: bool,
) -> Option<(String, String, UndefKind)> {
    match &call.receiver {
        Callee::Method { receiver, method, nullsafe } => {
            if *nullsafe {
                return None; // leg (l): `?->` excluded in v1.
            }
            let class = match receiver {
                // Leg (a)/A1: an exact-class receiver only. `new Foo()` is exact by
                // construction; a `$var` is exact only when the heap says so (a
                // `new`/clone-of-exact allocation, never a `$this` lower bound or a
                // laundered alias). `class_exact` is set solely by Verified-origin
                // allocation sites, so the N2 stratum requirement on the receiver
                // identity holds by construction.
                Receiver::New { class, .. } => cx.class_fqn(class),
                Receiver::Var(v) => {
                    if poisoned {
                        return None;
                    }
                    let obj = store.obj_of(v)?;
                    if !obj.class_exact {
                        return None; // lower bound → S6's lane, not ours.
                    }
                    obj.class.clone()
                }
                // A1: `$this` is a membership fact, never exactness — silent in S2.
                Receiver::This => return None,
                // A depth-1 property-fetch receiver carries no exact-class proof for
                // absence dispatch (ADR-0052 §7) — silent, like an unknown receiver.
                Receiver::Prop { .. } => return None,
            };
            Some((class, method.clone(), UndefKind::Instance))
        }
        Callee::Static { class, method } => match class {
            // Textual, exact — no receiver proof needed. `self`/`static`/`parent`
            // stay unlowered and silent (ADR-0043 §1).
            StaticClass::Named(name) => {
                Some((cx.class_fqn(name), method.clone(), UndefKind::Static))
            }
            StaticClass::SelfKw | StaticClass::Parent | StaticClass::Static => None,
        },
        // `new C()` (no method), `$fn()`, dynamic method names → not our sites.
        Callee::Function(_)
        | Callee::Construct { .. }
        | Callee::DynamicVar(_)
        | Callee::Dynamic => None,
    }
}

/// The outcome of walking a class-like's ancestor chain for `method` under the
/// ADR-0049 §4 closure discipline (the C1 completeness standard).
///
/// Four-valued (issue #1092): a chain that **declares** the method and one that only
/// answers it through the magic fallback are told apart from each other and from an
/// obstacle, so the declared-receiver lane can judge each descendant by its chain, and
/// `method_exists` reads the same walk with no magic fallback at all.
pub(crate) enum ChainWalk {
    /// The method is absent from a fully-enumerated, obstacle-free chain: fire
    /// eligible. Carries the ordered simple class names (for the message), the
    /// ordered chain FQNs (for the A2ii homonym leg), and whether any node was
    /// declared conditionally (A2i — re-dams the claim).
    Absent { simple_chain: Vec<String>, fqns: Vec<String>, any_conditional: bool },
    /// A node holds the method, its own or through a trait, abstract, static or of
    /// any visibility. The walk stops there, so `fqns` ends at the declaring node.
    Declared { fqns: Vec<String>, any_conditional: bool },
    /// The method is held nowhere on a fully-enumerated chain, but a node holds the
    /// magic fallback, so the call runs.
    Magic { fqns: Vec<String>, any_conditional: bool },
    /// An obstacle taints closure somewhere on the chain: nothing is proven in either
    /// direction (the FP-safe verdict).
    Unknown,
}

/// Collect every magic-member obstacle record in `start_fqn`'s **resolved reach**
/// (ADR-0049 A14, issue #195): the class-like's own records, then those of its
/// parent chain, its interfaces, and its `@mixin` targets — each followed
/// transitively, so a mixin whose target is itself a mixin chains on.
///
/// Non-empty ⇒ the class-like is not enumerable for an absence proof. Three
/// deliberate asymmetries with the method-chain walk: **interfaces are walked
/// here** even though [`enumerate_method_chain`] ignores them (an interface
/// cannot *define* a method, but a `@method` tag on one still says the
/// implementors answer names the index cannot list); **an unresolvable
/// parent/interface is not an obstacle here** (that leg belongs to the chain
/// walk, which already silences on it; treating every vendor-unresolved
/// interface as a magic obstacle would silence through the wrong door); **an
/// unresolvable `@mixin` target needs no special case** (the `@mixin` record on
/// the carrier is already in the result, so a target naming nothing is silence).
///
/// The visited set is the cycle guard: `@mixin`-into-`@mixin` cycles (and the
/// diamond an interface list makes) terminate after one visit per class-like.
pub(crate) fn magic_obstacles_in_reach(cx: &Cx, start_fqn: &str) -> Vec<MagicObstacle> {
    if !cx.index.has_magic_obstacles() {
        return Vec::new(); // a project spelling none of the tags pays nothing.
    }
    let mut out: Vec<MagicObstacle> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut stack: Vec<String> = vec![start_fqn.to_owned()];
    while let Some(fqn) = stack.pop() {
        if !seen.insert(fqn.to_ascii_lowercase()) {
            continue;
        }
        let Some((file, cd)) = cx.find_class(&fqn) else { continue };
        for rec in cx.index.magic_obstacles_of(&cd.fqn) {
            if let Some(target) = &rec.mixin_target {
                stack.push(target.clone());
            }
            out.push(rec.clone());
        }
        let tree = cx.units[file].tree;
        for r in cd.parent.iter().chain(cd.implements.iter()) {
            stack.push(tree.resolve_class_fqn(r));
        }
        // A tag on a trait obstructs every class that imports it (ADR-0105).
        for (_, td) in cx.reached_traits(file, cd) {
            stack.push(td.fqn.clone());
        }
    }
    out
}

/// Walk `start_fqn`'s parent chain proving the method's *absence* under complete
/// enumeration (ADR-0049 §4 (b)–(f), (j); A2i/A2iii). Interfaces are not walked:
/// a PHP interface never carries a method body, so it can never *define* the
/// method — only the `extends` (class-parent) chain can, exactly as
/// [`resolve_in_chain`] does. Any of these taints closure ⇒ `Silent`:
/// unresolvable/`Ambiguous`/builtin ancestor (leg b/f/i), a trait name or a node
/// whose trait composition declines (leg e, ADR-0105), an `is_enum` node (leg j /
/// A3), the magic fallback (`__call`/`__callStatic`, leg d, its own or a trait's),
/// a cycle (leg b), or the method being held (not undefined), its own or a trait's.
///
/// [`resolve_in_chain`]: crate::resolve_in_chain
pub(crate) fn enumerate_method_chain(cx: &Cx, start_fqn: &str, method: &str, kind: UndefKind) -> ChainWalk {
    // Leg A14 (issue #195): a `@method` / `@property*` / `@mixin` / `@phpstan-type`
    // tag anywhere in the class-like's resolved reach says members live where the
    // index cannot enumerate them — exactly the `__call` verdict, one door earlier.
    // The records are the reified decline: nothing in this slice reports them, but
    // they are what a `doctor` posture will count and what a plugin pack will
    // discharge member by member (ADR-0049 A14).
    if !magic_obstacles_in_reach(cx, start_fqn).is_empty() {
        return ChainWalk::Unknown;
    }
    // Leg (b)/(f)/(i): the start must resolve to a UNIQUE project declaration.
    let Some((file, cd)) = cx.find_class(start_fqn) else {
        return ChainWalk::Unknown;
    };
    walk_method_chain(cx, file, cd, start_fqn, method, Some(kind.magic()))
}

/// Walk the chain that starts at declaration `cd` (in file `file`, reached by the
/// name `start_name`) and climbs its `extends` edges, judging `method` at each node
/// ([`ChainWalk`]). The chain legs of [`enumerate_method_chain`] without its A14 reach
/// leg, which belongs to the caller: `method_exists` passes `magic: None`, since it
/// reports declared methods only and a `@method` tag is not one. Starting at a
/// declaration rather than a name lets the declared-receiver lane judge each half of
/// an Ambiguous descendant by its own chain (ADR-0049 A4, issue #1092).
pub(crate) fn walk_method_chain(
    cx: &Cx,
    file: usize,
    cd: &ClassDecl,
    start_name: &str,
    method: &str,
    magic: Option<&str>,
) -> ChainWalk {
    let (mut cfile, mut cd) = (file, cd);
    let mut cur = start_name.to_owned();
    let mut seen: HashSet<String> = HashSet::new();
    let mut simple_chain: Vec<String> = Vec::new();
    let mut fqns: Vec<String> = Vec::new();
    let mut any_conditional = false;
    let mut any_magic = false;
    loop {
        if !seen.insert(cur.to_ascii_lowercase()) {
            return ChainWalk::Unknown; // cycle — closure cannot terminate soundly.
        }
        // Leg (j)/A3: an enum's methods are lowered now (ADR-0105), but its cases
        // and the engine's `UnitEnum` methods are not methods of the declaration;
        // the enum chain stays Unknown.
        if cd.is_enum {
            return ChainWalk::Unknown;
        }
        // A trait name is never a chain node: `extends` naming one is its own fatal.
        if cd.is_trait {
            return ChainWalk::Unknown;
        }
        fqns.push(cur.clone());
        any_conditional |= cd.conditional;
        // Leg (e): a trait use adds methods the is-a oracle rightly ignores for
        // ancestry. The class holds them through its composition (ADR-0105); one that
        // declines leaves the chain unenumerable, as every trait use did before it.
        // The method is present (case-insensitively) — including an abstract
        // declaration: it is defined, so the call is not undefined.
        match cx.held_method(cfile, cd, method) {
            Err(_) => return ChainWalk::Unknown,
            Ok(Some(_)) => return ChainWalk::Declared { fqns, any_conditional },
            Ok(None) => {}
        }
        // parse failure (ADR-0079, issue #180), leg §2.5: a node declared in an
        // unparsable file is member-incomplete — the recovery kept the class but may
        // have dropped methods out of its body, so "the method is not in this chain"
        // is exactly the enumeration the break makes unprovable. The A14 verdict, one
        // door later: the whole-universe dam covers name existence, this leg covers
        // member enumeration, and both read the same site list. A method the
        // recovery kept is still declared, which is why the leg follows that test.
        if cx.member_incomplete(cfile) {
            return ChainWalk::Unknown;
        }
        // Leg (d): a magic fallback anywhere swallows the name — no error at runtime.
        // The walk goes on past it, so a declaration further up still reads Declared.
        if let Some(magic) = magic {
            match cx.held_method(cfile, cd, magic) {
                Err(_) => return ChainWalk::Unknown,
                Ok(Some(_)) => any_magic = true,
                Ok(None) => {}
            }
        }
        simple_chain.push(cd.name.clone());
        let Some(pref) = &cd.parent else {
            return if any_magic {
                ChainWalk::Magic { fqns, any_conditional }
            } else {
                ChainWalk::Absent { simple_chain, fqns, any_conditional }
            };
        };
        cur = cx.units[cfile].tree.resolve_class_fqn(pref);
        // Leg (b)/(f)/(i): every ancestor edge must resolve to a UNIQUE project
        // declaration. `find_class` returns `None` for `Absent` (a builtin/vendor-
        // unresolved ancestor — leg f, awaiting the reflect method surface, M2) and
        // for `Ambiguous` (a duplicate FQN or an alias/decl collision — leg i).
        let Some((pfile, pcd)) = cx.find_class(&cur) else {
            return ChainWalk::Unknown;
        };
        (cfile, cd) = (pfile, pcd);
    }
}

/// Run the full ADR-0049 §4 ladder for one method/static call and emit
/// `call.undefined-method` iff **every** leg holds. Called only from the plain
/// per-scope pass (`descent.is_none()`) so a site is judged once, never re-emitted
/// under an interprocedural descent.
pub(crate) fn check_undefined_method(
    cx: &Cx,
    folder: &mut dyn Folder,
    call: &CallExpr,
    store: &Store,
    poisoned: bool,
    out: &mut Vec<Diagnostic>,
) {
    // Legs (a)/(l): identify an exact receiver and the call kind, or bail.
    let Some((class_fqn, method, kind)) = undefined_method_receiver(cx, call, store, poisoned)
    else {
        return;
    };
    // Guard-respect leg (ADR-0049 §4): a positive `method_exists($this-or-C, 'm')`
    // guard dominating this site vouched `C::m` — the programmer supplied existence
    // evidence, so stay silent even if the chain enumeration below would reach Absent
    // (a `Maybe`-verdict guard whose branch we are walking live). Exact-textual match
    // on the RESOLVED class + method (case-insensitive).
    if store.vouches_method(&class_fqn, &method) {
        return;
    }
    // A9 (global) + A2ii's honest consequence: without a live sidecar, or with a
    // monkey-patch extension loaded, the id is entirely silent (checked once, cached).
    if !folder.absence_family_available() {
        return;
    }
    // Legs (b)–(f), (j), A2i/A2iii: textual closure over the ancestor chain.
    let ChainWalk::Absent { simple_chain, fqns, any_conditional } =
        enumerate_method_chain(cx, &class_fqn, &method, kind)
    else {
        return;
    };
    // Leg A2i and early binding: a conditional declaration in the chain re-dams the
    // claim — fire only when the whole-universe dam is clear (vouch machinery is not
    // available here) — and a use above a class PHP does not bind early declines.
    let binding = class_use(cx, folder, &class_fqn, any_conditional, call.span.start);
    if binding == Binding::Decline {
        return;
    }
    // Leg (h)/A2ii: every chain FQN must be answered NOT-present by the boot-surface
    // existence oracle. A homonym (`Some(true)`) or an unanswerable query (`None` —
    // a mid-run sidecar failure) is silence.
    for fqn in &fqns {
        match folder.boot_surface_class_like(fqn) {
            Some(false) => {}
            Some(true) | None => return,
        }
    }

    // Every leg holds — a proven `Error: Call to undefined method C::m()`.
    let pos = cx.tree().position(call.span.start);
    let simple_class = simple_chain.first().map_or(class_fqn.as_str(), String::as_str);
    let chain_render = simple_chain.join(" → ");
    let message = format!(
        "call to undefined method {simple_class}::{method}() — hierarchy fully enumerated ({chain_render}), \
         no {}, no @method/@property/@mixin{}",
        kind.magic(),
        binding.suffix(),
    );
    out.push(Diagnostic {
        id: CALL_UNDEFINED_METHOD_ID,
        path: cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        message,
        facet: None,
        fix: None,
    });
}

// member absence (ADR-0078, issue #197)
// ---------------------------------------------------------------------------
// The absence ladder over the remaining member kinds: `property.undefined`
// (a read of a property nothing declares) and `class-const.undefined` (a fetch of
// a constant nothing provides).
//
// The ADR-0049 §4 ladder is one ladder and it does not move; what moves per member
// kind is **what counts as a member source** and **what obstacle hides one**:
//
// | kind      | sources                                   | obstacles beyond the chain |
// | --------- | ----------------------------------------- | -------------------------- |
// | method    | class chain                               | `__call`/`__callStatic`     |
// | property  | class chain (plain/promoted/static/hooked) | `__get`/`__set`/`__isset`, `#[AllowDynamicProperties]`, `stdClass` descent, a project-wide dynamic write |
// | class-const | chain + **interfaces** + **enum cases**  | none — PHP gives constants no magic channel at all |
//
// Every `php -r` witness quoted here is at PHP 8.5.9, reproduced at the leg that
// consumes it. The two consequences differ, and ADR-0078 §1.4 makes that an id
// boundary rather than a message detail:
//
//   $c = new C; echo $c->nope;   Warning: Undefined property: C::$nope   → null
//   echo C::NOPE;                Error: Undefined constant C::NOPE       → fatal
//   echo C::$nope;               Error: Access to undeclared static property C::$nope
//
// Both read ids are judged at every read position the statement records
// (ADR-0104, issues #936 and #949), at the read's own span, through the carrier
// table in `stmt_checks`. `class-const.undefined` reads no env, so every carrier
// judges it (`A::NOPE ?? 1` and `@A::NOPE` throw too); only a dead extent of a
// decided guard does not. `property.undefined` is value-dependent: a plain operand
// and a by-value function argument judge, while a conditional, `@`, compound or
// possibly-by-reference argument position declines (a by-reference parameter
// creates the property), as does a read whose receiver the same statement may
// rebind.
//
// The static-property row is recorded, not implemented: `C::$prop` on an
// undeclared static property is a fatal (a different consequence from the
// instance read, so it could not ride `property.undefined` anyway), and the
// trace IR has no static-property *read* site at all — a lowering change, not a
// cheap one.
//
// Discharge (owner's 2026-08-08 policy, ADR-0049 A14): the A14 magic-tag leg is
// *dischargeable* — its records are reified, and a plugin manifest/pack
// (ADR-0039/0044/0045) declaring a magic property restores the absolute check
// for exactly what it declared (demand side: PHPStan's 15,554 Eloquent-shaped
// `property.notFound` sites). The three legs this slice adds — `__get`/`__set`/
// `__isset`, `#[AllowDynamicProperties]`, the project-wide dynamic-write set —
// are read off the code rather than a docblock, so nothing can discharge them
// today; reifying them is a `MagicObstacle`-vocabulary change, not a ladder one.
//
// The write side is deferred with its design (ADR-0078 §3): `property.dynamic-
// write` (writing an undeclared property on a plain class) is a deprecation
// today (witnessed: `Deprecated: Creation of dynamic property Plain::$dyn is
// deprecated`) and a fatal at PHP 9.0. Ask-the-real-thing forbids calling it
// proof while the project's own PHP tolerates it, so the id ships when the
// sidecar reports ≥ 9.0 and not before. Designed, named, not registered.
// ---------------------------------------------------------------------------

/// The magic methods that route an instance property access away from the
/// declaration set (ADR-0078, issue #197).
///
/// Only `__get` genuinely rescues a **read** — witnessed at 8.5.9: a class with
/// a `__get` prints `__get:nope`, while `__isset`/`__set` alone still raise
/// `Warning: Undefined property`. All three are obstacles anyway: any of them
/// runs the magic-property protocol, and the over-silence keeps ONE
/// enumerability rule rather than a second, laxer one (the
/// [`STRING_NON_STRINGABLE_ID`] precedent).
///
/// [`STRING_NON_STRINGABLE_ID`]: crate::STRING_NON_STRINGABLE_ID
const PROPERTY_MAGIC: &[&str] = &["__get", "__set", "__isset"];

/// A receiver class's ancestor chain, enumerated end to end with no obstacle on it
/// and with the property provably absent from every node (ADR-0078, issue #197).
struct PropertyChain {
    /// The chain's simple names, most-derived first (for the message).
    simple: Vec<String>,
    /// The chain's FQNs (for the A2ii boot-surface homonym leg).
    fqns: Vec<String>,
    /// Whether any node was declared conditionally (A2i — re-dams the claim).
    any_conditional: bool,
}

/// Whether a class-like declaration **provides** `prop` as a member — the
/// declaration set an absence claim must find empty.
///
/// Every spelling counts. A class-body **hooked** property (`public int $p {
/// get => 42; }`, added by #185) binds no value and is not lowered to a
/// `PropertyDecl`, but it IS declared (witnessed: the read prints `42`) —
/// `ClassDecl::hooked_properties` keeps the bare name. A `static` declaration
/// counts too, though `$obj->staticName` really does warn (witnessed:
/// `Accessing static property S1::$sp as non static` then `Undefined property:
/// S1::$sp`) — treating the name as present costs one true positive, never a
/// false one.
///
/// A property a trait gives the class counts as the class's own (ADR-0105 C11); a
/// composition that declines is [`property_walk_obstacle`]'s, asked first.
fn declares_property(cx: &Cx, file: usize, cd: &ClassDecl, prop: &str) -> bool {
    cd.hooked_properties.iter().any(|h| h == prop)
        || !matches!(cx.held_property(file, cd, prop), Ok(None))
}

/// Whether this node hides members from the property walk however the walk asks
/// (ADR-0078, issue #197) — the obstacle set shared by the chain walk and the
/// descendant scan: a **trait** name, or a node whose trait composition declines its
/// properties or methods (ADR-0105 — a trait can declare properties, witnessed
/// `UT::$tp` reads `4` through `use TP`, and a `__get` it gives the class is the
/// class's); an **enum** (`name`/`value` are engine-provided,
/// not declared); an **interface** (declares no properties at all, so its own
/// emptiness proves nothing about the object behind it); `__get`/`__set`/
/// `__isset` (see [`PROPERTY_MAGIC`] — the fallback is inherited, witnessed a
/// parent's `__get` rescues a read on the child, which is why this is asked at
/// every node); `#[AllowDynamicProperties]` (re-licenses the write PHP 8.2
/// deprecated, leaving the property set open for good).
fn property_walk_obstacle(cx: &Cx, file: usize, cd: &ClassDecl) -> bool {
    cd.is_trait
        || cd.is_enum
        || cd.is_interface
        || cd.allows_dynamic_properties
        || cx.properties_open(file, cd)
        || PROPERTY_MAGIC.iter().any(|g| !matches!(cx.held_method(file, cd, g), Ok(None)))
}

/// Walk `start_fqn`'s parent chain proving `prop`'s absence under complete
/// enumeration (ADR-0078, issue #197). `None` is silence — either an obstacle
/// taints closure or the property is declared.
///
/// Interfaces are not walked (a PHP interface cannot declare a property), and
/// **`stdClass` needs no leg of its own**: it is not a project declaration, so
/// `find_class` answers `None` and the chain simply never closes — covering
/// `stdClass` and every descendant in one edge. Deliberately conservative: a
/// never-written read on `stdClass` really does warn (witnessed: `Undefined
/// property: stdClass::$nope`), a true positive Steins declines in v1 because
/// `stdClass` is the language's own property bag and a dynamic property written
/// anywhere would make the read clean.
fn enumerate_property_chain(cx: &Cx, start_fqn: &str, prop: &str) -> Option<PropertyChain> {
    match enumerate_property_chain_outcome(cx, start_fqn, prop) {
        PropertyChainOutcome::Absent(chain) => Some(chain),
        PropertyChainOutcome::Declared | PropertyChainOutcome::Unknown => None,
    }
}

/// What a parent-chain walk actually established — the three-valued form of
/// [`enumerate_property_chain`] (ADR-0081 §7).
///
/// The definite leg needs only "absent or not", and collapses the other two into
/// silence. The possibly leg needs them apart: a union arm that **declares** the
/// property is a path on which the read is clean, while an arm the walk could not
/// close is a path nothing is known about — and a claim that "some arms lack it"
/// may rest on the first but never on the second.
enum PropertyChainOutcome {
    /// Every node of a fully enumerated chain lacks the property.
    Absent(PropertyChain),
    /// A node in the chain declares it — plain, promoted, static, readonly or
    /// hooked.
    Declared,
    /// An obstacle taints the closure: nothing is proven in either direction.
    Unknown,
}

fn enumerate_property_chain_outcome(
    cx: &Cx,
    start_fqn: &str,
    prop: &str,
) -> PropertyChainOutcome {
    // Leg A14 (issue #195): a `@property*` / `@method` / `@mixin` / `@phpstan-type`
    // tag anywhere in the class-like's resolved reach says members live where the
    // index cannot enumerate them. This is the leg that keeps the Eloquent shape
    // silent, and it is reused verbatim — the same records, the same reach walk, the
    // same discharge channel a plugin pack will open member by member.
    if !magic_obstacles_in_reach(cx, start_fqn).is_empty() {
        return PropertyChainOutcome::Unknown;
    }
    let Some((file, cd)) = cx.find_class(start_fqn) else {
        return PropertyChainOutcome::Unknown;
    };
    walk_property_chain(cx, file, cd, start_fqn, prop)
}

/// Walk the chain that starts at declaration `cd` (in file `file`, reached by the
/// name `start_name`) for `prop`: the chain legs of
/// [`enumerate_property_chain_outcome`] without its A14 reach leg, which the caller
/// asks. Starting at a declaration lets the declared-receiver lane judge each half of
/// an Ambiguous descendant by its own chain (issue #1092).
fn walk_property_chain(
    cx: &Cx,
    file: usize,
    cd: &ClassDecl,
    start_name: &str,
    prop: &str,
) -> PropertyChainOutcome {
    let mut found = Some((file, cd));
    let mut cur = start_name.to_owned();
    let mut seen: HashSet<String> = HashSet::new();
    let mut chain = PropertyChain { simple: Vec::new(), fqns: Vec::new(), any_conditional: false };
    loop {
        if !seen.insert(cur.to_ascii_lowercase()) {
            // A cycle — closure cannot terminate soundly.
            return PropertyChainOutcome::Unknown;
        }
        // Every ancestor edge must resolve to a UNIQUE project declaration:
        // `find_class` is `None` for an absent ancestor (a builtin — `stdClass`
        // included — or a vendor class awaiting the reflect surface) and for an
        // `Ambiguous` FQN alike.
        let Some((cfile, cd)) = found.take().or_else(|| cx.find_class(&cur)) else {
            return PropertyChainOutcome::Unknown;
        };
        // ADR-0079 §2.5: a node declared in an unparsable file is member-incomplete —
        // recovery kept the class but may have dropped members out of its body.
        if cx.member_incomplete(cfile) || property_walk_obstacle(cx, cfile, cd) {
            return PropertyChainOutcome::Unknown;
        }
        if declares_property(cx, cfile, cd, prop) {
            // Declared — plain, promoted, static, readonly or hooked. This is the
            // one exit that proves the read CLEAN rather than merely unprovable.
            return PropertyChainOutcome::Declared;
        }
        chain.simple.push(cd.name.clone());
        chain.fqns.push(cur.clone());
        chain.any_conditional |= cd.conditional;
        match &cd.parent {
            None => return PropertyChainOutcome::Absent(chain),
            Some(pref) => cur = cx.units[cfile].tree.resolve_class_fqn(pref),
        }
    }
}

/// The receiver a `property.undefined` claim can rest on (ADR-0078, issue #197),
/// or `None` for silence.
///
/// The reach is the two lanes the member family already has: **the exact lane
/// (S2's)** — an allocation-proven `$var` whose `class_exact` holds (`$this`
/// never qualifies, it's a membership fact not exactness, A1); **the declared
/// lane (S6's, routed by A13)** — a receiver carrying a narrowed contract-arm
/// lane, admitted only when the minimum stratum over the *participating* arms
/// is `Verified`. **Any `Asserted` arm is silence**: the property family has no
/// phpdoc twin to route an Asserted claim to (ADR-0078's floor table registers
/// none), so a docblock-premised property absence gets no id in v1 rather than
/// laundering onto the proof surface (ADR-0052 §5). The twin, if ever needed, is
/// a registry addition.
enum PropertyReceiver<'s> {
    Exact(String),
    /// A declared/narrowed arm lane, as **conjunct lists** with their guards — per
    /// arm, the classes a receiver of that arm satisfies all of. A plain `Foo` arm is
    /// a one-element list; a declared `Foo&Bar` is a two-element one (issue #238).
    /// Built by [`declared_receiver_conjuncts`].
    Declared(Vec<LaneArm<'s>>),
}

/// Classify a `$var->prop` receiver into [`PropertyReceiver`]. Disjoint by
/// construction, exactly as S2/S6 are: the exact lane is taken first, and a
/// lane-carrying variable is never `class_exact`.
fn undefined_property_receiver<'s>(
    cx: &Cx,
    store: &'s Store,
    var: &str,
) -> Option<PropertyReceiver<'s>> {
    if let Some(obj) = store.obj_of(var)
        && obj.class_exact
    {
        return Some(PropertyReceiver::Exact(obj.class.clone()));
    }
    if store.is_exact(var) {
        return None; // an exact object without a usable class — not a subject.
    }
    let arms = store.contract_arms(var)?;
    if arms.is_empty() {
        return None;
    }
    // A13: the minimum over the PARTICIPATING arms, computed next to the arm read so
    // it can never drift from the arms the claim rests on.
    if arms.iter().fold(Stratum::Verified, |acc, a| acc.min(a.stratum)) != Stratum::Verified {
        return None; // an Asserted premise — the calibration boundary, see above.
    }
    // The same arm read the method lane uses, so the two declared-receiver lanes
    // cannot disagree about what an arm IS: a class, an intersection of classes
    // (issue #238), or out of scope. A scalar/array/null arm means the runtime
    // receiver may be a non-object — a different finding (`property.on-non-object`),
    // not this one — and an intersection issue #234's posture proves uninhabited is
    // no receiver at all. The arms carry their guards, which the ladder reads as the
    // method lane does (issue #1095).
    Some(PropertyReceiver::Declared(declared_receiver_conjuncts(cx, arms)?))
}

/// One descendant declaration, judged for `prop` by its **whole chain**, not by its
/// own body (issue #1092), so a property or a `__get` it inherits from a class outside
/// the arm's hierarchy counts. The property twin of the method lane's candidate
/// verdict: an A14 tag in its reach or an obstacle anywhere on the chain is
/// `Unknown`, a declaration anywhere on it `Declared`, and an absence holds only when
/// no chain node is conditional (A2i) or a homonym (A2ii).
fn descendant_property(
    cx: &Cx,
    folder: &mut dyn Folder,
    file: usize,
    cd: &ClassDecl,
    prop: &str,
) -> Held {
    if !magic_obstacles_in_reach(cx, &cd.fqn).is_empty() {
        return Held::Unknown;
    }
    let chain = match walk_property_chain(cx, file, cd, &cd.fqn, prop) {
        PropertyChainOutcome::Absent(chain) => chain,
        PropertyChainOutcome::Declared => return Held::Declared,
        PropertyChainOutcome::Unknown => return Held::Unknown,
    };
    // A homonym node may be dead code shadowed by a loaded class (A2ii).
    if !redammed(cx, chain.any_conditional)
        && chain.fqns.iter().all(|fqn| folder.boot_surface_class_like(fqn) == Some(false))
    {
        Held::Absent
    } else {
        Held::Unknown
    }
}

/// What the §8 ladder established about `prop` on one class-like of a narrowed
/// contract arm, with no guard read (ADR-0081 §7): [`Held::Absent`] across the
/// class-like's hierarchy and its complete descendant set, [`Held::Declared`] when a
/// node of its own chain declares the property, else [`Held::Unknown`].
///
/// Three-valued on purpose. The definite leg only ever needed "absent or not" and
/// collapsed the other two into one silence; splitting them is what lets the
/// possibly leg rest on an arm that genuinely declares the property while still
/// refusing an arm the ladder could not close.
fn arm_property_presence(cx: &Cx, folder: &mut dyn Folder, arm_fqn: &str, prop: &str) -> Held {
    let chain = match enumerate_property_chain_outcome(cx, arm_fqn, prop) {
        PropertyChainOutcome::Absent(chain) => chain,
        PropertyChainOutcome::Declared => return Held::Declared,
        PropertyChainOutcome::Unknown => return Held::Unknown,
    };
    if redammed(cx, chain.any_conditional) {
        return Held::Unknown; // A2i.
    }
    for fqn in &chain.fqns {
        if folder.boot_surface_class_like(fqn) != Some(false) {
            return Held::Unknown; // A2ii homonym.
        }
    }
    match descendant_closure(cx, arm_fqn) {
        DescendantClosure::Immune => {}
        DescendantClosure::Obstacle => return Held::Unknown,
        DescendantClosure::Enumerated(descendants) => {
            if !cx.dam.is_clear() {
                // `eval` could mint a subclass declaring the property.
                return Held::Unknown;
            }
            for (dfile, dcd) in &descendants {
                // A descendant whose chain declares the property is NOT the
                // `Declared` answer: the receiver may or may not be that descendant,
                // which is precisely an unknown rather than a clean path.
                if descendant_property(cx, folder, *dfile, dcd, prop) != Held::Absent {
                    return Held::Unknown;
                }
            }
        }
    }
    Held::Absent
}

/// The property questions of the declared-receiver ladder
/// ([`judge_arm`](crate::declared_receiver::judge_arm)). An interface anchor keeps
/// the walk's `Unknown` (an interface declares no property), so only an arm whose
/// guards reach its implementors can speak.
struct PropertyLadder<'f, 'p> {
    folder: &'f mut dyn Folder,
    prop: &'p str,
}

impl MemberLadder for PropertyLadder<'_, '_> {
    fn conjunct(&mut self, cx: &Cx, fqn: &str) -> Held {
        arm_property_presence(cx, self.folder, fqn, self.prop)
    }
    fn declares(&mut self, cx: &Cx, fqn: &str) -> bool {
        matches!(enumerate_property_chain_outcome(cx, fqn, self.prop), PropertyChainOutcome::Declared)
    }
    fn anchor_clear(&mut self, cx: &Cx, fqn: &str) -> bool {
        magic_obstacles_in_reach(cx, fqn).is_empty()
            && cx.find_class(fqn).is_some_and(|(_, cd)| !redammed(cx, cd.conditional))
            && self.folder.boot_surface_class_like(fqn) == Some(false)
    }
    fn candidate(&mut self, cx: &Cx, file: usize, cd: &ClassDecl) -> Held {
        descendant_property(cx, self.folder, file, cd, self.prop)
    }
}

/// Split the arms of a declared receiver by what they hold of `prop`: the display
/// names of the live arms that provably lack it and of those that declare it, or
/// `None` for silence — a live arm the ladder could not close, or an arm a
/// `@phpstan-assert` guard decided, since the property family has no contract-layer
/// twin (ADR-0049 A13). A dead arm drops out (issue #1095).
fn split_property_arms(
    cx: &Cx,
    folder: &mut dyn Folder,
    arms: &[LaneArm],
    prop: &str,
) -> Option<(Vec<String>, Vec<String>)> {
    let mut ladder = PropertyLadder { folder, prop };
    let mut absent: Vec<String> = Vec::new();
    let mut declared: Vec<String> = Vec::new();
    for arm in arms {
        let judged = judge_arm(cx, arm, &mut ladder);
        if judged.rests_on_assert {
            return None;
        }
        match judged.fate {
            ArmFate::Dead => {}
            ArmFate::Live(Held::Absent) => absent.push(judged.display),
            ArmFate::Live(Held::Declared) => declared.push(judged.display),
            ArmFate::Live(Held::Magic | Held::Unknown) => return None,
        }
    }
    Some((absent, declared))
}

/// `property.undefined` (ADR-0078, issue #197) at a `$var->prop` **read**.
///
/// The warning-handler gate is the FIRST question asked, because under a declared
/// `warning-handler = "null"` posture the application has said it tolerates
/// `Undefined property` and the whole id leaves the proof surface (ADR-0049 §7) —
/// there is nothing further to compute.
pub(crate) fn check_undefined_property(
    w: &WalkCx,
    folder: &mut dyn Folder,
    var: &str,
    prop: &str,
    store: &Store,
    span: Span,
    out: &mut Vec<Diagnostic>,
) {
    let cx = w.cx;
    if !cx.postures.warning_handler_abort || w.scope.poisoned {
        return;
    }
    // The dynamic-write obstacle. A name written anywhere in the project could have
    // been created on this object before the read — a plain-class dynamic write is
    // a deprecation, not an error (witnessed), so the read that follows is clean —
    // and a computed-name write could have created any name at all. Asked before any
    // class work: it is a hash lookup, and it is the leg most likely to answer.
    if cx.index.property_write_obstacle(prop) {
        return;
    }
    let Some(receiver) = undefined_property_receiver(cx, store, var) else {
        return;
    };
    // Guard-respect leg (ADR-0049 §4, issue #930): a positive `property_exists($var,
    // 'p')` dominating this read vouched `$var->p` for this binding — `property_exists`
    // is also true for a dynamic property, so it says nothing of the class's other
    // instances, and the vouch is dropped with the binding.
    if store.vouches_var_property(var, prop) {
        return;
    }
    // A9 (monkey-patch) + A2ii's honest consequence: without a live sidecar, or with
    // a runtime-redefinition extension loaded, the id is silent (checked once).
    if !folder.absence_family_available() {
        return;
    }
    let (subject, chain_render) = match receiver {
        PropertyReceiver::Exact(class_fqn) => {
            let Some(chain) = enumerate_property_chain(cx, &class_fqn, prop) else {
                return;
            };
            if redammed(cx, chain.any_conditional) {
                return; // A2i.
            }
            for fqn in &chain.fqns {
                match folder.boot_surface_class_like(fqn) {
                    Some(false) => {}
                    Some(true) | None => return, // A2ii homonym / unanswerable.
                }
            }
            let subject = chain.simple.first().cloned().unwrap_or(class_fqn);
            let render = chain.simple.join(" → ");
            (subject, format!("hierarchy fully enumerated ({render})"))
        }
        PropertyReceiver::Declared(arms) => {
            // Every arm, and within an intersection arm every conjunct: a property
            // declared on EITHER conjunct resolves, because member lookup over an
            // inhabited intersection is the union of its arms (issue #234).
            //
            // The fold is three-valued now (ADR-0081 §7), which is what splits the
            // pair: an arm that PROVES the property absent, an arm that declares it,
            // and an arm the ladder could not close. Every arm absent is the
            // definite id; some absent and the rest declared is the possibly id;
            // a single unknown arm is silence on both, because a possibly-grade
            // claim about "some arms" is still a claim about ALL of them. An arm its
            // guards leave no inhabitant drops out (issue #1095).
            let Some((absent, declared)) = split_property_arms(cx, folder, &arms, prop) else {
                return;
            };
            if absent.is_empty() {
                return; // every arm declares it — the read is clean.
            }
            if !declared.is_empty() {
                // The possibly leg: the read is clean on the declared arms and
                // warns on the absent ones.
                check_maybe_undefined_property(
                    cx, var, prop, &absent, &declared, span, out,
                );
                return;
            }
            let joined = absent.join("|");
            (
                joined.clone(),
                format!(
                    "declared receiver ${var} narrowed to {{{joined}}}, hierarchy and \
                     descendants fully enumerated"
                ),
            )
        }
    };
    let pos = cx.tree().position(span.start);
    out.push(Diagnostic {
        id: PROPERTY_UNDEFINED_ID,
        facet: None,
        fix: None,
        path: cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        message: format!(
            "read of undefined property ${var}->{prop} — {chain_render}, no __get/__set/__isset, \
             no #[AllowDynamicProperties], no @property/@method/@mixin, no dynamic write of \
             `{prop}` anywhere — PHP warns \"Undefined property: {subject}::${prop}\" and \
             evaluates to null"
        ),
    });
}

/// `property.maybe-undefined` (ADR-0078's floor table, ADR-0081 §7): the
/// declared-shape possibly leg, where the declared-receiver ladder proves the
/// property absent on **some** union arms and present on the rest.
///
/// `offset.maybe-missing`'s emission pattern one member kind over: the definite
/// leg claims absence over the whole receiver, this one over a proper subset of
/// the narrowed arms, disjoint because the routing in
/// [`check_undefined_property`] is a partition — every arm absent, some arms
/// absent, or an unclosable arm that silences both.
///
/// Carries **no reachability premise** — the arms are a union of declared
/// types, not control-flow paths, so nothing here consults the binding-presence
/// pass; it shares ADR-0081 only because the pair was registered together.
///
/// Every premise of the definite leg is already discharged by the caller: the
/// ADR-0049 §7 warning-handler gate, `Scope::poisoned`, the project-wide
/// dynamic-write obstacle, A9 sidecar availability, the A13 Verified-stratum
/// floor, and the full §8 ladder per arm.
fn check_maybe_undefined_property(
    cx: &Cx,
    var: &str,
    prop: &str,
    absent: &[String],
    declared: &[String],
    span: Span,
    out: &mut Vec<Diagnostic>,
) {
    let missing = absent.join("|");
    let carrying = declared.join("|");
    let subject = absent.first().cloned().unwrap_or_default();
    let pos = cx.tree().position(span.start);
    out.push(Diagnostic {
        id: PROPERTY_MAYBE_UNDEFINED_ID,
        facet: None,
        fix: None,
        path: cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        message: format!(
            "property ${var}->{prop} is declared on only some arms of the receiver's \
             declared type — {{{carrying}}} declare it, {{{missing}}} do not, with \
             hierarchy and descendants fully enumerated, no __get/__set/__isset, no \
             #[AllowDynamicProperties], no @property/@method/@mixin and no dynamic \
             write of `{prop}` anywhere — on a {subject} PHP warns \"Undefined \
             property: {subject}::${prop}\" and the read evaluates to null"
        ),
    });
}

/// A class-like's **member reach** for a constant fetch, enumerated end to end with
/// the constant provably absent from every node (ADR-0078, issue #197).
struct ConstReach {
    /// Every FQN visited, for the A2ii boot-surface homonym leg.
    fqns: Vec<String>,
    /// The number of class-likes the reach covers, for the message.
    width: usize,
    /// Whether any node was declared conditionally (A2i — re-dams the claim).
    any_conditional: bool,
}

/// Whether a class-like **provides** `name` as a class constant.
///
/// Two member sources, both witnessed: the declared constants
/// (`ClassDecl::const_visibility` lists every declared name, including one whose
/// initializer is not a literal — the value list would not), and an enum's
/// **cases**, which answer `Suit::Hearts` in exactly the same syntactic position.
/// Names are matched case-sensitively, as PHP matches them.
pub(crate) fn provides_class_const(cd: &ClassDecl, name: &str) -> bool {
    cd.const_visibility.iter().any(|(n, _)| n == name)
        || cd.enum_cases.iter().any(|c| c.name == name)
}

/// Walk `start_fqn`'s whole member reach — parent chain **and** interfaces,
/// transitively — proving `name`'s absence (ADR-0078, issue #197). `None` is
/// silence.
///
/// Where the method and property walks follow the `extends` chain alone, a
/// constant can arrive from anywhere in the reach, all three routes witnessed at
/// 8.5.9: `class CImpl implements I1 {}` answers `CImpl::IK`, `interface IB
/// extends IA` carries `IA::AK` through to `CB::AK`, and a trait's constant
/// answers through the using class (`CT::TK`) — so a trait-using node is an
/// obstacle here rather than a node to skip.
///
/// An enum node is **not** an obstacle: both an enum's constants and cases are
/// lowered, so an enum reach is enumerable for this member kind.
fn enumerate_const_reach(cx: &Cx, start_fqn: &str, name: &str) -> Option<ConstReach> {
    // A14 (issue #195), one door earlier. Constants have no magic channel at all —
    // a `@property`/`@method` tag cannot make `C::K` resolve — so this leg is pure
    // over-silence, taken for the same reason `string.non-stringable` takes it: ONE
    // enumerability rule, not a second laxer one that happens to be sound here.
    if !magic_obstacles_in_reach(cx, start_fqn).is_empty() {
        return None;
    }
    let mut reach = ConstReach { fqns: Vec::new(), width: 0, any_conditional: false };
    let mut seen: HashSet<String> = HashSet::new();
    let mut stack: Vec<String> = vec![start_fqn.to_owned()];
    while let Some(fqn) = stack.pop() {
        if !seen.insert(fqn.to_ascii_lowercase()) {
            continue; // already visited — the diamond an interface list makes.
        }
        // Every edge must resolve to a UNIQUE project declaration; `None` covers an
        // absent (builtin/vendor) and an `Ambiguous` class-like alike, and either
        // could be where the constant lives.
        let (cfile, cd) = cx.find_class(&fqn)?;
        if cx.member_incomplete(cfile) {
            return None; // ADR-0079 §2.5: members may have been dropped by recovery.
        }
        // A trait constant answers through the class that imports it (ADR-0105 C12):
        // a trait name, or a composition that declines its constants, is an obstacle.
        if cd.is_trait {
            return None;
        }
        match cx.held_const(cfile, cd, name) {
            Err(_) | Ok(Some(_)) => return None,
            Ok(None) => {}
        }
        if provides_class_const(cd, name) {
            return None; // an enum case — not undefined.
        }
        reach.any_conditional |= cd.conditional;
        reach.fqns.push(fqn.clone());
        reach.width += 1;
        let tree = cx.units[cfile].tree;
        for r in cd.parent.iter().chain(cd.implements.iter()) {
            stack.push(tree.resolve_class_fqn(r));
        }
    }
    Some(reach)
}

/// `class-const.undefined` (ADR-0078, issue #197) at a `C::K` fetch.
///
/// Only an explicitly **named** class is a subject, the reach `class-const.inaccessible`
/// already has: `self::`/`parent::` resolve in a lexically fixed scope this walk does
/// not thread, and `static::K` is late-bound and unproven (ADR-0043 §1). `X::class`
/// is excluded outright — a plain string since PHP 8.0 that errors on nothing, even
/// for a class that does not exist (witnessed).
///
/// No warning-handler gate: the consequence is a fatal `Error` and no posture makes
/// it survivable.
pub(crate) fn check_undefined_class_const(
    w: &WalkCx,
    folder: &mut dyn Folder,
    sc: &StaticClass,
    name: &str,
    span: Span,
    out: &mut Vec<Diagnostic>,
) {
    let cx = w.cx;
    if w.scope.poisoned || name.eq_ignore_ascii_case("class") {
        return;
    }
    let StaticClass::Named(r) = sc else { return };
    let class_fqn = cx.class_fqn(r);
    // A9 + A2ii: without a live, monkey-patch-free sidecar the id is silent.
    if !folder.absence_family_available() {
        return;
    }
    let Some(reach) = enumerate_const_reach(cx, &class_fqn, name) else {
        return;
    };
    // A2i: a conditional declaration anywhere leaves which body binds to load order;
    // a fetch above a class PHP does not bind early declines.
    let binding = class_use(cx, folder, &class_fqn, reach.any_conditional, span.start);
    if binding == Binding::Decline {
        return;
    }
    // A2ii: every class-like in the reach must be answered NOT-present by the
    // boot-surface existence oracle. A homonym (`Some(true)`) or an unanswerable
    // query (`None` — a mid-run sidecar failure) is silence.
    for fqn in &reach.fqns {
        match folder.boot_surface_class_like(fqn) {
            Some(false) => {}
            Some(true) | None => return,
        }
    }
    let written = cx.find_class(&class_fqn).map_or(class_fqn.as_str(), |(_, cd)| cd.name.as_str());
    let pos = cx.tree().position(span.start);
    let width = reach.width;
    out.push(Diagnostic {
        id: CLASS_CONST_UNDEFINED_ID,
        facet: None,
        fix: None,
        path: cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        message: format!(
            "fetch of undefined class constant {written}::{name} — member reach fully \
             enumerated ({width} class-like(s): parents, interfaces and enum cases), \
             constants have no magic fallback — proven Error (Undefined constant \
             {written}::{name}){}",
            binding.suffix(),
        ),
    });
}

// end member absence (ADR-0078, issue #197)

// ---------------------------------------------------------------------------
// The existence ids: `call.undefined-function` + `class.undefined`
// (ADR-0049 §3/§5 / S4 — the FP-risk hotspot).
//
// Both are DAMMED absence claims (unlike S2's method-absence, which is immune: PHP
// cannot reopen a resolved class, but eval and out-of-universe includes DO mint
// functions/classes at runtime) — a standing dam site ANYWHERE in the universe
// silences the whole slice (§10's falsifiable prediction). Shared ladder, any leg
// failing ⇒ silence (zero-FP identity ADR-0013):
//   - a direct function reference or hard-error class position (`$fn()`, `::class`,
//     instanceof, catch, type-decls are §5's verified NON-findings; a first-class
//     callable `f(...)` is a reference, ADR-0104);
//   - `absence_family_available` (A9 monkey-patch void + no-sidecar sound subset);
//   - not a reserved dump FQN (D3) and not a curated SAPI-provided name (A6);
//   - every candidate FQN index-Absent (not Unique, not Ambiguous, not a catalog builtin);
//   - the whole-universe dam clear (A5-corrected; the vouch valve is out of scope here);
//   - the boot surface answers not-found for every candidate (A2ii / reflect);
//   - not vouched by a dominating `function_exists`/`class_exists` guard — via the
//     branch store for `call.undefined-function` (FP-15), via dead-region pruning for
//     `class.undefined` (a decided guard meeting the firing conditions folds its
//     branch dead under the SAME closure);
//   - PHP function-/class-name case-insensitivity throughout.
// ---------------------------------------------------------------------------

/// The curated SAPI-provided *exact* names (ADR-0049 A6) — the non-prefix half of
/// `is_sapi_provided_function`'s list, exported so `steins doctor`'s Runtime
/// section (ADR-0054 §9.1's A6 line) can name the same set it silences, without a
/// second copy of the list drifting from this one.
pub const SAPI_PROVIDED_FUNCTIONS_EXACT: &[&str] = &["fastcgi_finish_request", "getallheaders", "virtual"];

/// The curated SAPI-provided *prefix* families (ADR-0049 A6), the doctor-facing
/// twin of [`SAPI_PROVIDED_FUNCTIONS_EXACT`].
pub const SAPI_PROVIDED_FUNCTION_PREFIXES: &[&str] = &["apache_", "litespeed_"];

/// The curated SAPI-provided functions (ADR-0049 A6): symbols a CLI sidecar lacks
/// but the serving FPM/Apache/LiteSpeed runtime provides, so they are NEVER Absent
/// while the serving SAPI is undeclared (the default — `[runtime] sapi` would unlock
/// a firing claim, deferred-with-design). Matched case-insensitively on the already-
/// lowercased candidate; the `apache_`/`litespeed_` families are prefix-matched.
fn is_sapi_provided_function(lname: &str) -> bool {
    SAPI_PROVIDED_FUNCTION_PREFIXES.iter().any(|p| lname.starts_with(p))
        || SAPI_PROVIDED_FUNCTIONS_EXACT.contains(&lname)
}

/// Resolve a function-call reference to its provably-**absent** target under PHP name
/// resolution (ADR-0049 §3 (a) + A8): `Some((display, candidates))` when EVERY FQN the
/// call could denote is index-`Absent` (no Unique decl — conditional decls are indexed
/// too, so this excludes the polyfill idiom — and no `Ambiguous`), and no candidate is
/// a catalog builtin; `None` otherwise (resolved / ambiguous / builtin ⇒ silence).
/// `display` is the source-cased primary target (the current-ns candidate for an
/// unqualified call); `candidates` are the lowercased FQNs whose runtime existence the
/// boot-surface leg must also refute (two for an unqualified in-namespace call: the
/// `Ns\name` candidate PHP tries first, then the global fallback).
fn undefined_function_target(cx: &Cx, r: &NameRef) -> Option<(String, Vec<String>)> {
    let catalog_knows = |n: &str| steins_catalog::effect_labels(n).is_some();
    let absent = |fqn: &str| matches!(cx.index.resolve_function(fqn), Res::Absent);
    match r.kind {
        RefKind::FullyQualified => {
            let lname = r.raw.to_ascii_lowercase();
            // A single-segment global name (`\strlen`) may be a builtin.
            if !r.raw.contains('\\') && catalog_knows(&lname) {
                return None;
            }
            absent(&lname).then(|| (r.raw.clone(), vec![lname]))
        }
        RefKind::Qualified => {
            let ctx = cx.tree().ctx_at(r.offset);
            let first_len = r.raw.find('\\').unwrap_or(r.raw.len());
            let first = &r.raw[..first_len];
            let fqn = if let Some(t) = ctx.class_imports.get(&first.to_ascii_lowercase()) {
                format!("{t}{}", &r.raw[first_len..])
            } else if ctx.namespace.is_empty() {
                r.raw.clone()
            } else {
                format!("{}\\{}", ctx.namespace, r.raw)
            };
            let lname = fqn.to_ascii_lowercase();
            absent(&lname).then_some((fqn, vec![lname]))
        }
        RefKind::Relative => {
            // A8: `namespace\name` — the enclosing-ns candidate only, no fallback.
            let ctx = cx.tree().ctx_at(r.offset);
            if ctx.namespace.is_empty() {
                let lname = r.raw.to_ascii_lowercase();
                if catalog_knows(&lname) {
                    return None;
                }
                absent(&lname).then(|| (r.raw.clone(), vec![lname]))
            } else {
                let fqn = format!("{}\\{}", ctx.namespace, r.raw);
                let lname = fqn.to_ascii_lowercase();
                absent(&lname).then_some((fqn, vec![lname]))
            }
        }
        RefKind::Unqualified => {
            let ctx = cx.tree().ctx_at(r.offset);
            let name = r.raw.to_ascii_lowercase();
            // `use function` import wins outright: the single candidate is its target.
            if let Some(t) = ctx.fn_imports.get(&name) {
                let lt = t.to_ascii_lowercase();
                if !lt.contains('\\') && catalog_knows(&lt) {
                    return None;
                }
                return absent(&lt).then(|| (t.clone(), vec![lt]));
            }
            let (display, ns_candidate) = if ctx.namespace.is_empty() {
                (r.raw.clone(), None)
            } else {
                // PHP tries `Ns\name` first; a Unique or Ambiguous there ⇒ not absent.
                let ns_l = format!("{}\\{}", ctx.namespace, name);
                if !absent(&ns_l) {
                    return None;
                }
                (format!("{}\\{}", ctx.namespace, r.raw), Some(ns_l))
            };
            // Global fallback candidate: a catalog builtin or any user/ambiguous global
            // means the call resolves — silence.
            if catalog_knows(&name) || !absent(&name) {
                return None;
            }
            let mut candidates: Vec<String> = ns_candidate.into_iter().collect();
            candidates.push(name);
            Some((display, candidates))
        }
    }
}

/// Run the ADR-0049 §3 ladder for one function call and emit
/// `call.undefined-function` iff **every** leg holds. Called only in the plain
/// per-scope pass (`descent.is_none()`), and never inside a proven-dead region (the
/// walk prunes those), so a site is judged once with the branch store live.
pub(crate) fn check_undefined_function(
    cx: &Cx,
    folder: &mut dyn Folder,
    call: &CallExpr,
    store: &Store,
    out: &mut Vec<Diagnostic>,
) {
    // Leg (d): a direct function reference — a call `f(...)` or a first-class callable
    // `f(...)`, which PHP resolves when it builds the `Closure` (ADR-0104); `$fn()` is
    // another `Callee` variant.
    let Callee::Function(_) = &call.receiver else {
        return;
    };
    let Some(r) = call.callee_ref.as_ref() else {
        return;
    };
    // Index leg (cheap, first, header above).
    let Some((display, candidates)) = undefined_function_target(cx, r) else {
        return;
    };
    // D3 carve-out: a reserved dump FQN already reds CI with a fail-level dump — a
    // second finding for one deletable line is noise (ADR-0053 §6).
    if is_dump_family_fqn(&resolved_fn_fqn(cx, r)) {
        return;
    }
    // A6: curated SAPI-provided name.
    if candidates.iter().any(|c| is_sapi_provided_function(c)) {
        return;
    }
    // FP-15 guard leg: a dominating positive `function_exists('f')` vouched the name.
    if candidates.iter().any(|c| store.vouches_function(c)) {
        return;
    }
    // Dam leg (A5, header above).
    if !cx.dam.is_clear() {
        return;
    }
    // A9 sidecar leg (header above).
    if !folder.absence_family_available() {
        return;
    }
    // Boot-surface leg (A2ii, header above): a homonym or unanswerable candidate ⇒ silence.
    for c in &candidates {
        match folder.boot_surface_function(c) {
            Some(false) => {}
            Some(true) | None => return,
        }
    }

    // Every leg holds — a proven `Error: Call to undefined function f()`.
    let pos = cx.tree().position(call.span.start);
    let evidence = existence_evidence(folder);
    out.push(Diagnostic {
        id: CALL_UNDEFINED_FUNCTION_ID,
        path: cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        message: format!("call to undefined function {display}() — {evidence}"),
        facet: None,
        fix: None,
    });
}

// ---------------------------------------------------------------------------
// Callable names (ADR-0043's 2026-10-11 amendment, issue #941): the ladders above,
// asked of the name a string or array spells where a native `callable` is
// required, rather than of a call. The legs are the same legs, in the same order;
// only what names the target differs. A string callable is resolved by PHP as a
// fully-qualified name, so there is one candidate and no namespace fallback.
// ---------------------------------------------------------------------------

/// Whether the function a callable string names is provably absent — the
/// `call.undefined-function` ladder for one fully-qualified candidate. `store` is
/// the branch's, for the `function_exists` / `is_callable` vouch.
pub(crate) fn callable_function_absent(
    cx: &Cx,
    folder: &mut dyn Folder,
    name: &str,
    store: &Store,
) -> bool {
    let lname = name.trim_start_matches('\\').to_ascii_lowercase();
    if steins_catalog::effect_labels(&lname).is_some()
        || !matches!(cx.index.resolve_function(&lname), Res::Absent)
        || is_sapi_provided_function(&lname)
        || store.vouches_function(&lname)
    {
        return false;
    }
    cx.dam.is_clear()
        && folder.absence_family_available()
        && folder.boot_surface_function(&lname) == Some(false)
}

/// Whether the class a callable's class half names is provably absent — the
/// `class.undefined` ladder ([`check_undefined_class`]), plus the branch's
/// `class_exists` vouch, which that file-wide id takes from dead-region pruning.
pub(crate) fn callable_class_absent(
    cx: &Cx,
    folder: &mut dyn Folder,
    name: &str,
    store: &Store,
) -> bool {
    let lname = name.trim_start_matches('\\').to_ascii_lowercase();
    if !matches!(cx.index.resolve_class(&lname), Res::Absent)
        || steins_catalog::builtin_class_supers(&lname).is_some()
        || store.vouches_class(&lname)
    {
        return false;
    }
    cx.dam.is_clear()
        && folder.absence_family_available()
        && folder.boot_surface_class_like(&lname) == Some(false)
}

/// Whether `method` is provably absent from the exact project class `class_fqn` —
/// the `call.undefined-method` ladder ([`check_undefined_method`]), walked for
/// **both** call kinds. A callable reaches the method from whatever scope calls
/// it, so `__call` and `__callStatic` are each an obstacle, whichever the
/// spelling suggests. A builtin class anywhere on the chain is silence, as it is
/// for the call: a reflected declaration never convicts (`Folder::reflected_class`).
pub(crate) fn callable_method_absent(
    cx: &Cx,
    folder: &mut dyn Folder,
    class_fqn: &str,
    method: &str,
    store: &Store,
) -> bool {
    // The guard-respect leg: `method_exists('C', 'm')`, `is_callable(['C', 'm'])` or
    // `is_callable('C::m')` on this path vouched the method.
    if store.vouches_method(class_fqn, method) || !folder.absence_family_available() {
        return false;
    }
    let mut chain: Vec<String> = Vec::new();
    let mut conditional = false;
    for kind in [UndefKind::Static, UndefKind::Instance] {
        let ChainWalk::Absent { fqns, any_conditional, .. } =
            enumerate_method_chain(cx, class_fqn, method, kind)
        else {
            return false;
        };
        conditional |= any_conditional;
        chain = fqns;
    }
    if redammed(cx, conditional) {
        return false;
    }
    chain.iter().all(|fqn| folder.boot_surface_class_like(fqn) == Some(false))
}

/// The ADR-0049 §9 closure-evidence clause for the existence ids: the boot surface's
/// self-description (`not defined in the project, not on PHP 8.5.8 (32 extensions)`),
/// or a version-agnostic phrasing when the sidecar reports no label.
fn existence_evidence(folder: &mut dyn Folder) -> String {
    folder.boot_surface_label().map_or_else(
        || "not defined in the project, not on the analyzing PHP".to_owned(),
        |label| format!("not defined in the project, not on {label}"),
    )
}

/// Run the ADR-0049 §5 ladder for the file's hard-error class references and emit
/// `class.undefined` for each provably-absent one. Called once per file (never under
/// a descent); dead-region references are skipped by the caller, which is exactly the
/// guard leg for this id (a `class_exists('X')` whose class meets the firing
/// conditions folds its branch dead under the SAME closure this ladder rests on).
pub(crate) fn check_undefined_class(cx: &Cx, folder: &mut dyn Folder, r: &NameRef, out: &mut Vec<Diagnostic>) {
    // The class-LIKE closure set (classes + interfaces + enums + traits, alias edges
    // followed) — index Absent, not Ambiguous. `self`/`static`/`parent`, dynamic
    // classes, and `X::class` were excluded at collection.
    let display = cx.tree().resolve_class_fqn(r);
    let lname = display.to_ascii_lowercase();
    if !matches!(cx.index.resolve_class(&lname), Res::Absent) {
        return; // Unique (defined / aliased) or Ambiguous ⇒ silence.
    }
    // Extra silence (a subset of the sidecar's answer, never a firing license): a class
    // the catalog knows as a builtin/extension class-like in its hierarchy.
    if steins_catalog::builtin_class_supers(&lname).is_some() {
        return;
    }
    // Dam leg (A5, header above).
    if !cx.dam.is_clear() {
        return;
    }
    // A9 sidecar leg (header above).
    if !folder.absence_family_available() {
        return;
    }
    // Boot-surface leg (A2ii, header above).
    match folder.boot_surface_class_like(&lname) {
        Some(false) => {}
        Some(true) | None => return,
    }

    let pos = cx.tree().position(r.offset);
    let evidence = existence_evidence(folder);
    // The word `resource` (ADR-0097 §2.2): PHP has no declarable resource type and
    // reads the word as a class name, warning at compile time that it will
    // (`"resource" is not a supported builtin type and will be interpreted as a
    // class name`, 8.5.10). The diagnosis is the same absent class and keeps the
    // id; the sentence names the type the author meant and where it can be
    // written. Unqualified only — `\resource` and `App\resource` are class
    // references an author spelled as such.
    let message = if r.kind == RefKind::Unqualified && r.raw.eq_ignore_ascii_case("resource") {
        format!(
            "`resource` is not a type PHP can declare — this is a reference to a class \
             {display}, {evidence}; the resource type is spelled in a docblock (`@param resource`)",
        )
    } else {
        format!("reference to undefined class {display} — {evidence}")
    };
    out.push(Diagnostic {
        id: CLASS_UNDEFINED_ID,
        path: cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        message,
        facet: None,
        fix: None,
    });
}

// ---------------------------------------------------------------------------
// `constant.undefined` (ADR-0078, issue #198) — the absence family's global-constant
// member. Shorter than the class ladder only in that there is no hierarchy to
// enumerate; every other leg is the same, and one is stricter.
// ---------------------------------------------------------------------------

/// Resolve a bare constant fetch to its provably-**absent** target under PHP's
/// constant name resolution (ADR-0078, issue #198): `Some((display, candidates))`
/// when every name the fetch could denote is undeclared in the universe, `None`
/// otherwise (something declares it ⇒ silence).
///
/// Reuses `undefined_function_target`'s resolution shape — PHP resolves the two the
/// same way, including the global fallback classes lack: an unqualified `FOO` inside
/// `namespace App;` tries `App\FOO` then global `FOO` (`php -r`-witnessed, 8.5.9).
/// Three language-forced differences: constants are **case-sensitive** (only the
/// namespace prefix folds, [`steins_syntax::normalize_const_fqn`] —
/// `defined('App\LOCAL')`/`defined('app\LOCAL')` both true, `defined('App\local')`
/// false); an unqualified name consults `use const` **imports**
/// ([`steins_syntax::NsCtx::const_imports`]), not `use function` (a qualified name's
/// first segment still uses the ordinary class/namespace imports); and there is
/// **no catalog presence leg** — the builtin catalog is never an absence oracle
/// (ADR-0049 §1), and a presence catalog for constants would be a second, staler
/// copy of the sidecar's answer, so engine/extension constants are refuted only by
/// the boot surface. The engine-constant table's value-less rows are the one thing
/// read out of a catalog here, and they are read in the other direction — see
/// [`check_undefined_constant`]'s boot-surface leg.
///
/// `display` is the source-cased primary target (PHP's own phrasing at the fatal);
/// `candidates` are the normalized keys the boot-surface leg must also refute — two
/// for an unqualified in-namespace fetch, one otherwise.
fn undefined_constant_target(cx: &Cx, r: &NameRef) -> Option<(String, Vec<String>)> {
    let undeclared = |key: &str| !cx.index.declares_constant(key);
    match r.kind {
        RefKind::FullyQualified => {
            let key = normalize_const_fqn(&r.raw);
            undeclared(&key).then(|| (r.raw.clone(), vec![key]))
        }
        RefKind::Qualified => {
            // The first segment is a namespace, so it resolves through the class /
            // namespace import map — the same rule `undefined_function_target` uses.
            // No global fallback for a qualified name.
            let ctx = cx.tree().ctx_at(r.offset);
            let first_len = r.raw.find('\\').unwrap_or(r.raw.len());
            let first = &r.raw[..first_len];
            let fqn = if let Some(t) = ctx.class_imports.get(&first.to_ascii_lowercase()) {
                format!("{t}{}", &r.raw[first_len..])
            } else if ctx.namespace.is_empty() {
                r.raw.clone()
            } else {
                format!("{}\\{}", ctx.namespace, r.raw)
            };
            let key = normalize_const_fqn(&fqn);
            undeclared(&key).then_some((fqn, vec![key]))
        }
        RefKind::Relative => {
            // A8: `namespace\FOO` — the enclosing-ns candidate only, no fallback.
            let ctx = cx.tree().ctx_at(r.offset);
            let fqn = if ctx.namespace.is_empty() {
                r.raw.clone()
            } else {
                format!("{}\\{}", ctx.namespace, r.raw)
            };
            let key = normalize_const_fqn(&fqn);
            undeclared(&key).then_some((fqn, vec![key]))
        }
        RefKind::Unqualified => {
            let ctx = cx.tree().ctx_at(r.offset);
            // A `use const` import wins outright: the single candidate is its target,
            // and there is no fallback past it.
            if let Some(t) = ctx.const_imports.get(&r.raw) {
                let key = normalize_const_fqn(t);
                return undeclared(&key).then(|| (t.clone(), vec![key]));
            }
            let (display, ns_candidate) = if ctx.namespace.is_empty() {
                (r.raw.clone(), None)
            } else {
                // PHP tries `Ns\FOO` first; anything declaring it ⇒ not absent.
                let ns_key = normalize_const_fqn(&format!("{}\\{}", ctx.namespace, r.raw));
                if !undeclared(&ns_key) {
                    return None;
                }
                (format!("{}\\{}", ctx.namespace, r.raw), Some(ns_key))
            };
            // The global fallback candidate.
            let global_key = normalize_const_fqn(&r.raw);
            if !undeclared(&global_key) {
                return None;
            }
            let mut candidates: Vec<String> = ns_candidate.into_iter().collect();
            candidates.push(global_key);
            Some((display, candidates))
        }
    }
}

/// Run the `constant.undefined` ladder for the file's bare constant fetches and
/// emit one finding per provably-absent one (ADR-0078, issue #198). Called once per
/// file, never under a descent; a fetch in a dead region is skipped by the caller,
/// which IS this id's guard leg — a `defined('X')` meeting the firing conditions
/// folds its branch dead under the same closure (`constant_defined_verdict`), so
/// `if (defined('X')) { echo X; }` is silent without a second mechanism.
pub(crate) fn check_undefined_constant(cx: &Cx, folder: &mut dyn Folder, r: &NameRef, out: &mut Vec<Diagnostic>) {
    // Index leg (cheap, first): every candidate undeclared anywhere in the universe.
    // `const` statements and literal `define()` calls both land here, conditional or
    // not — which is what makes `if (!defined('X')) define('X', …)` silent.
    let Some((display, candidates)) = undefined_constant_target(cx, r) else {
        return;
    };
    // Dam leg (A5, constant edition): ANY dam site closes this valve — `eval`, an
    // unproven include, a broken file, and the computed `define()` that only this id
    // reads (`DamKind::DefineDynamic`). Stricter than `is_clear()` on purpose.
    if !cx.dam.constants_are_clear() {
        return;
    }
    // A9 sidecar leg (header above).
    if !folder.absence_family_available() {
        return;
    }
    // Boot-surface leg (A2ii): extension constants and an already-loaded bootstrap's
    // `define()`s declare themselves here; the builtin catalog is not a presence
    // oracle and is never consulted for one — in EITHER direction (ADR-0094 §5).
    //
    // The mined table's value-less rows (issue #718) record that a constant left
    // the engine at some minor, and it is tempting to read one here: a project
    // whose floor is above the departure has no minor that still has the name. The
    // reading buys nothing. The A9 leg above is issue #28's version-skew gate — it
    // has already declined every claim from a runtime outside the declared target
    // — so a runtime that reaches this line is inside the target, and a target
    // whose floor is past the departure admits only runtimes past it, which report
    // the name absent of their own accord. The only outcome such a clause could
    // change is one where the row's `until` is WRONG, and there it manufactures a
    // finding no engine agrees with.
    for c in &candidates {
        match folder.boot_surface_constant(c) {
            Some(false) => {}
            Some(true) | None => return,
        }
    }

    let pos = cx.tree().position(r.offset);
    let evidence = existence_evidence(folder);
    out.push(Diagnostic {
        id: CONSTANT_UNDEFINED_ID,
        path: cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        // PHP's own words: `Uncaught Error: Undefined constant "FOO"`.
        message: format!("undefined constant {display} — {evidence}"),
        facet: None,
        fix: None,
    });
}

#[cfg(test)]
mod tests {
    use steins_syntax::SourceTree;

    use crate::FileUnit;
    use crate::cx::Cx;
    use crate::project::{Index, LazyTree};

    use super::{ChainWalk, UndefKind, enumerate_method_chain, walk_method_chain};

    /// The verdict's name, so each test reads as the row of the four-way table it pins.
    fn name(w: &ChainWalk) -> &'static str {
        match w {
            ChainWalk::Absent { .. } => "absent",
            ChainWalk::Declared { .. } => "declared",
            ChainWalk::Magic { .. } => "magic",
            ChainWalk::Unknown => "unknown",
        }
    }

    /// Walk `start` for `method` in a one-file project, with the instance fallback
    /// (`magic`) or with none, which is `method_exists`'s question.
    fn walk(src: &str, start: &str, method: &str, magic: bool) -> &'static str {
        let tree = LazyTree::ready(SourceTree::parse(src));
        let units = [FileUnit { path: "t.php", tree: &tree }];
        let index = Index::from_units(&units);
        let cx = Cx::new(&units, &index, 0);
        if magic {
            return name(&enumerate_method_chain(&cx, start, method, UndefKind::Instance));
        }
        let Some((file, cd)) = cx.find_class(start) else { return "unresolved" };
        name(&walk_method_chain(&cx, file, cd, start, method, None))
    }

    const SRC: &str = "<?php
class Top { public function up(): int { return 1; } }
class Magic extends Top { public function __call(string $n, array $a): int { return 5; } }
class Leaf extends Magic {}
class Plain {}
class Kid extends Plain {}
class Lost extends Missing {}
enum E { case A; }
";

    #[test]
    fn a_closed_chain_lacking_the_method_is_absent() {
        assert_eq!(walk(SRC, "Kid", "m", true), "absent");
        assert_eq!(walk(SRC, "Kid", "m", false), "absent");
    }

    #[test]
    fn a_method_on_an_ancestor_is_declared() {
        assert_eq!(walk(SRC, "Magic", "up", true), "declared");
        assert_eq!(walk(SRC, "Magic", "up", false), "declared");
    }

    #[test]
    fn the_walk_continues_past_call_to_a_higher_declaration() {
        // `Magic` holds `__call` below `Top::up()`: the declaration still wins.
        assert_eq!(walk(SRC, "Leaf", "up", true), "declared");
    }

    #[test]
    fn an_inherited_call_alone_is_magic_and_absent_without_a_fallback() {
        assert_eq!(walk(SRC, "Leaf", "m", true), "magic");
        // `method_exists` ignores `__call`.
        assert_eq!(walk(SRC, "Leaf", "m", false), "absent");
    }

    #[test]
    fn an_unresolved_ancestor_or_an_enum_is_unknown() {
        assert_eq!(walk(SRC, "Lost", "m", true), "unknown");
        assert_eq!(walk(SRC, "Lost", "m", false), "unknown");
        assert_eq!(walk(SRC, "E", "m", true), "unknown");
    }
}
