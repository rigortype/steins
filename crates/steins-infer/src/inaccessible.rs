//! Inaccessible members (ADR-0078 / issue #185): `call.inaccessible-method`,
//! `property.inaccessible` and `class-const.inaccessible` — a member that exists but
//! whose visibility the access site cannot reach, resolved along the member chain.

use std::collections::HashSet;

use steins_syntax::{
    CallExpr, Callee, ClassDecl, Receiver, Scope, ScopeOwner, Span, StaticClass, Visibility,
};

use crate::contract::IsA;
use crate::{
    CALL_INACCESSIBLE_METHOD_ID, CLASS_CONST_INACCESSIBLE_ID, Cx, Diagnostic,
    PROPERTY_INACCESSIBLE_ID, Resolution, Store, WalkCx, private_blocked, resolve_in_chain,
};
use crate::absence::magic_obstacles_in_reach;

// ---------------------------------------------------------------------------
// inaccessible members (ADR-0078, issue #185)
//
// `call.inaccessible-method`, `property.inaccessible`, `class-const.inaccessible`.
//
// A **positive** claim over a resolved declaration (the #183 shape): the member
// is found, its declared visibility read off it, and the site's scope compared to
// the declaring class. Unlike the absence family there is no dam and no sidecar
// leg — `eval` can mint a name, but it cannot reopen a declared class to widen a
// member's visibility, and existence (the boot surface's question) is not at
// issue here.
//
// What the absence family's closure conditions DO carry over, because a nearer
// declaration or a fallback would change the verdict: the receiver's hierarchy
// must be enumerable end to end (an unresolvable, `Ambiguous`, trait-using or
// trait ancestor anywhere is silence); `__call`/`__callStatic`/`__get`/`__set`
// anywhere in that chain is silence (PHP routes an *inaccessible* member through
// the magic fallback exactly as an undefined one, witnessed below — the leg that
// makes this slice non-trivial); an A14 `@method`/`@property`/`@mixin` tag in the
// class-like's reach is silence, one door earlier; a conditionally declared node
// re-dams the claim (A2i).
//
// And one condition of its own: the **receiver's runtime class must be exact**.
// `$this->m()` in `B` on a `private A::m()` is a fatal for a `B` instance but calls
// the override for a `C extends B` that declares a public `m()` — both witnessed —
// so a lower-bound receiver could be rescued by a descendant the walk cannot
// enumerate. `$this`, `self::`, `static::` and `parent::` are therefore silent, and
// what remains is exactly the lanes the existing member checks already reach: a
// `new`-typed receiver, an allocation-proven variable, and an explicit `C::m()`.
//
// `php -r` witnesses, PHP 8.5.9 (each quoted at the leg that consumes it):
//
//   private method, global scope   Call to private method C::m() from global scope
//   private method, subclass scope Call to private method A::m() from scope B
//   protected method, alien scope  Call to protected method A::m() from scope U
//   protected method, subclass     legal — prints `ok`
//   private ctor                   Call to private C::__construct() from global scope
//   private ctor + __call          same fatal — magic does NOT rescue a constructor
//   private method + __call        prints `__call:m` — no error
//   private static + __callStatic  prints `__callStatic:m` — no error
//   private prop read / write      Cannot access private property C::$p
//   private prop + __get / __set   prints `__get:p` / `__set:p=5` — no error
//   private const                  Cannot access private constant C::K
//   private const + __get/__callStatic  same fatal — constants have no magic leg
//   first-class callable `$c->m(...)` on a private method  the same fatal
// ---------------------------------------------------------------------------

/// Whether the walk's class scope is *known* at this site — the one place `None`
/// must not be read as "global scope".
///
/// A closure body lexically inside a class method **runs in that class's scope**
/// in PHP (`php -r` witness: a closure declared in `C::go()` calls `C`'s own
/// private `m()` and prints `ok`; `static` closures and arrow functions do the
/// same), but [`scope_class`] deliberately does not thread the enclosing class
/// into a closure scope — its `None` there means "unknown", not "no class", so a
/// visibility claim inside a closure would read a legal same-class access as a
/// global-scope violation. Silence, until the closure scope carries its owner.
///
/// A plain `function` nested inside a method genuinely has no class scope, and a
/// top-level statement is the global scope; both keep `None` and both answerable.
///
/// [`scope_class`]: crate::scope_class
pub(crate) fn class_scope_known(scope: &Scope) -> bool {
    !matches!(scope.owner, ScopeOwner::Closure { .. })
}

/// The `private` leg of member visibility: a `private` member declared by
/// `declaring_fqn` is invisible everywhere but that class's own scope — it is not
/// inherited, so a subclass's scope is *outside* it (witnessed:
/// `Call to private method A::m() from scope B`).
///
/// Extracted from [`private_blocked`] so the resolver's suppression and the
/// [`CALL_INACCESSIBLE_METHOD_ID`] emitter, plus the property and class-constant
/// ids that have no resolver of their own, all read one definition.
pub(crate) fn private_invisible(declaring_fqn: &str, scope: Option<&str>) -> bool {
    !scope.is_some_and(|e| e.eq_ignore_ascii_case(declaring_fqn))
}

/// The `protected` leg: visible from any scope in the declaring class's *hierarchy*,
/// in either direction, and from nowhere else.
///
/// Witnessed at 8.5.9: a subclass scope may call it, a **superclass** scope may
/// call it on a child-declared member, a sibling subclass scope may call it on
/// another descendant, and an unrelated class or the global scope may not
/// (`Call to protected method A::m() from scope U`).
///
/// So this is the is-a oracle applied twice, inheriting its discipline (ADR-0043
/// §3): only a **definite** `No` in both directions — needing the hierarchy
/// completely enumerated — blocks. `Unknown` either way is silence. A site with
/// no class scope at all is blocked without asking.
fn protected_invisible(cx: &Cx, declaring_fqn: &str, scope: Option<&str>) -> bool {
    match scope {
        None => true,
        Some(s) => {
            matches!(cx.is_a(s, declaring_fqn), IsA::No)
                && matches!(cx.is_a(declaring_fqn, s), IsA::No)
        }
    }
}

/// The declared visibility of a member, read as "is it invisible at a site whose
/// class scope is `scope`?" — the shared verdict of all three ids. Returns the PHP
/// keyword for the message, or `None` when the member is visible (which includes
/// every `public` member, so the caller need not pre-filter).
fn member_inaccessible(
    cx: &Cx,
    visibility: Visibility,
    declaring_fqn: &str,
    scope: Option<&str>,
) -> Option<&'static str> {
    match visibility {
        Visibility::Public => None,
        Visibility::Private => private_invisible(declaring_fqn, scope).then_some("private"),
        Visibility::Protected => {
            protected_invisible(cx, declaring_fqn, scope).then_some("protected")
        }
    }
}

/// A receiver class's ancestor chain, enumerated end to end with no obstacle on it —
/// the closure a visibility claim needs (see the section header).
struct MemberChain<'a> {
    /// The chain from the receiver class up to the root, in PHP's own lookup order.
    nodes: Vec<(usize, &'a ClassDecl)>,
    /// Whether any node was declared conditionally (A2i — re-dams the claim).
    any_conditional: bool,
}

impl<'a> MemberChain<'a> {
    /// The chain rendered for a diagnostic message, most-derived first.
    fn render(&self) -> String {
        self.nodes.iter().map(|(_, cd)| cd.name.as_str()).collect::<Vec<_>>().join(" → ")
    }
}

/// Enumerate `start_fqn`'s ancestor chain for a member-visibility claim, refusing
/// (`None` — silence) on anything that leaves the lookup path incomplete or gives
/// PHP somewhere else to route the access.
///
/// `magic` lists the fallback method names that would swallow the access at this
/// kind of site (`__call`, `__callStatic`, `__get`, `__set`); it is empty where PHP
/// is witnessed to have no fallback at all — a constructor and a class constant.
fn enumerate_member_chain<'a>(
    cx: &Cx<'a>,
    start_fqn: &str,
    magic: &[&str],
) -> Option<MemberChain<'a>> {
    // A14 (issue #195): a `@method` / `@property*` / `@mixin` tag anywhere in the
    // class-like's resolved reach says members live where the index cannot enumerate
    // them — the `__call` verdict, one door earlier.
    if !magic_obstacles_in_reach(cx, start_fqn).is_empty() {
        return None;
    }
    let mut cur = start_fqn.to_owned();
    let mut seen: HashSet<String> = HashSet::new();
    let mut nodes: Vec<(usize, &ClassDecl)> = Vec::new();
    let mut any_conditional = false;
    loop {
        if !seen.insert(cur.to_ascii_lowercase()) {
            return None; // a cycle — the enumeration cannot terminate soundly.
        }
        // Every ancestor edge must resolve to a UNIQUE project declaration:
        // `find_class` is `None` for an absent (vendor/builtin) ancestor and for an
        // `Ambiguous` FQN alike, and either could be where a nearer declaration or a
        // `__call` lives.
        let (cfile, cd) = cx.find_class(&cur)?;
        // A trait's members are not flattened into the using class (S1/leg (e)), so a
        // trait name and a trait-using node both hide declarations from this walk —
        // including, possibly, a public override of the member in hand.
        if cd.is_trait || cd.uses_traits {
            return None;
        }
        // The magic fallback: present anywhere in the chain, PHP routes the
        // inaccessible access through it and raises nothing at all.
        if cd.methods.iter().any(|m| magic.iter().any(|g| m.name.eq_ignore_ascii_case(g))) {
            return None;
        }
        any_conditional |= cd.conditional;
        nodes.push((cfile, cd));
        match &cd.parent {
            None => return Some(MemberChain { nodes, any_conditional }),
            Some(pref) => cur = cx.units[cfile].tree.resolve_class_fqn(pref),
        }
    }
}

/// Render a class scope for a diagnostic message the way PHP's own fatal does:
/// `global scope` for a site with no enclosing class, `scope B` otherwise (with the
/// declaration's source casing when the class is in the project).
fn scope_render(cx: &Cx, scope: Option<&str>) -> String {
    match scope {
        None => "global scope".to_owned(),
        Some(s) => {
            let name = cx.find_class(s).map_or(s, |(_, cd)| cd.name.as_str());
            format!("scope {name}")
        }
    }
}

/// Which magic fallback (if any) a call site's kind routes an inaccessible call
/// through, plus the PHP phrasing of the site.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CallSiteKind {
    /// `$recv->m()` — `__call` rescues it (witnessed), including for a `private
    /// static` method reached through an instance receiver.
    Instance,
    /// `C::m()` — `__callStatic` rescues it (witnessed); an instance `__call` does
    /// **not** (`Call to private method C::m() from global scope` with a `__call`
    /// present, witnessed).
    Static,
    /// `new C()` — **nothing** rescues it: a class with both a private
    /// `__construct` and a `__call` still raises
    /// `Call to private C::__construct() from global scope` (witnessed).
    Construct,
}

impl CallSiteKind {
    fn magic(self) -> &'static [&'static str] {
        match self {
            CallSiteKind::Instance => &["__call"],
            CallSiteKind::Static => &["__callStatic"],
            CallSiteKind::Construct => &[],
        }
    }
}

/// The call sites a `call.inaccessible-method` claim can rest on: the receiver's
/// **exact** runtime class, the member name, and the site kind. `None` is silence.
///
/// The reach is the existing member checks', not a new one — `new`-typed
/// receivers, allocation-proven variables and explicit `C::m()`. Three deliberate
/// exclusions: **`$this`/`self::`/`static::`/`parent::`** (the enclosing object's
/// runtime class is a *lower bound*, and a descendant may both override the
/// member publicly and carry a `__call`; both rescues are witnessed —
/// `class C extends B { public function m(){} }` makes `$this->m()` in `B` print
/// `C`); **a non-`static` method through `C::m()`** (with a `$this` in scope PHP
/// treats it as an instance call, the lower-bound case again; without one it's a
/// different fatal); **a depth-1 property receiver** (no exact-class proof,
/// ADR-0052 §7).
///
/// `nullsafe` is not read: `?->` short-circuits on `null` alone, and every
/// receiver this lane admits is allocation-proven, never null (`$c = new C;
/// $c?->m();` on a private `m()` is the same fatal, witnessed).
///
/// The first-class-callable form `$c->m(...)` is a **recorded boundary**: PHP
/// raises the same fatal at closure-*creation* time for it (witnessed), but the
/// form does not lower to a method call in the trace IR at all, so no site
/// reaches here — it joins the lane for free the day the lowering carries it.
fn inaccessible_call_subject(
    cx: &Cx,
    call: &CallExpr,
    store: &Store,
    poisoned: bool,
) -> Option<(String, String, CallSiteKind)> {
    match &call.receiver {
        Callee::Construct { class } => {
            Some((cx.class_fqn(class), "__construct".to_owned(), CallSiteKind::Construct))
        }
        Callee::Method { receiver, method, .. } => {
            let class = match receiver {
                Receiver::New { class, .. } => cx.class_fqn(class),
                Receiver::Var(v) => {
                    if poisoned {
                        return None;
                    }
                    let obj = store.obj_of(v)?;
                    if !obj.class_exact {
                        return None;
                    }
                    obj.class.clone()
                }
                Receiver::This | Receiver::Prop { .. } => return None,
            };
            Some((class, method.clone(), CallSiteKind::Instance))
        }
        Callee::Static { class: StaticClass::Named(name), method } => {
            Some((cx.class_fqn(name), method.clone(), CallSiteKind::Static))
        }
        Callee::Static { .. }
        | Callee::Function(_)
        | Callee::DynamicVar(_)
        | Callee::Dynamic => None,
    }
}

/// `call.inaccessible-method` (ADR-0078, issue #185): a call to a method whose
/// declared visibility hides it from this site's scope — the fatal `Error` PHP
/// raises before the body runs.
///
/// Called only from the plain per-scope pass (`descent.is_none()`), like every other
/// once-per-site judgement, so a binding descent never re-emits it.
pub(crate) fn check_inaccessible_method(
    w: &WalkCx,
    call: &CallExpr,
    store: &Store,
    out: &mut Vec<Diagnostic>,
) {
    let cx = w.cx;
    if !class_scope_known(w.scope) {
        return;
    }
    let Some((class_fqn, method, kind)) =
        inaccessible_call_subject(cx, call, store, w.scope.poisoned)
    else {
        return;
    };
    // The resolver's own resolution, so the emitter and the suppression can only
    // ever be talking about the same declaration. `Unknown` (an unresolvable or
    // trait-using node, an abstract declaration) and `NotFoundChainComplete` (the
    // absence family's business, not ours) are both silence.
    let Resolution::Found(r) = resolve_in_chain(cx, &class_fqn, &method) else {
        return;
    };
    // A `C::m()` on a non-static method is not this site's fatal — see
    // `inaccessible_call_subject`.
    if kind == CallSiteKind::Static && !r.method.is_static {
        return;
    }
    let scope = w.enclosing_class;
    // The `private` leg IS the resolver's predicate, called here for the finding
    // instead of for the suppression; the `protected` leg is this check's own (the
    // resolver deliberately keeps resolving protected members — a protected call is
    // still a dispatch target for arity and effects).
    let vis = match r.method.visibility {
        Visibility::Private => private_blocked(&r, scope).then_some("private"),
        v => member_inaccessible(cx, v, &r.declaring_class.fqn, scope),
    };
    let Some(vis) = vis else { return };
    let Some(chain) = enumerate_member_chain(cx, &class_fqn, kind.magic()) else {
        return;
    };
    // A class that cannot be instantiated at all raises its OWN fatal first, before
    // any visibility check: `Cannot instantiate abstract class A` / `… interface I` /
    // `… enum E` (all witnessed). Naming one of those sites with this id would
    // misname the consequence, the #183 discipline.
    if kind == CallSiteKind::Construct
        && chain.nodes.first().is_some_and(|(_, cd)| {
            cd.is_abstract || cd.is_interface || cd.is_enum || cd.is_trait
        })
    {
        return;
    }
    // A2i: a conditional declaration anywhere leaves which body binds to load order —
    // fire only when the whole-universe dam is clear.
    if chain.any_conditional && !cx.dam.is_clear() {
        return;
    }
    let pos = cx.tree().position(call.span.start);
    // PHP's own wording, which the sentence quotes: `Call to private method C::m()
    // from global scope`, but `Call to private C::__construct() from scope B` — the
    // constructor's message carries no `method` word (both witnessed).
    let kind_word = if kind == CallSiteKind::Construct { "" } else { "method " };
    let call_render = format!("{}::{method}()", r.declaring_class.name);
    let magic_render = match kind.magic() {
        [] => "a constructor has no magic fallback".to_owned(),
        names => format!("no {}", names.join("/")),
    };
    let sentence =
        format!("Call to {vis} {kind_word}{call_render} from {}", scope_render(cx, scope));
    out.push(Diagnostic {
        id: CALL_INACCESSIBLE_METHOD_ID,
        facet: None,
        fix: None,
        path: cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        message: format!(
            "call to {vis} {kind_word}{call_render} — hierarchy fully enumerated ({}), \
             {magic_render} — proven Error ({sentence})",
            chain.render(),
        ),
    });
}

/// Find `member` in an enumerated chain, returning the node that declares it and
/// whether that node is the receiver's own class.
///
/// The second half separates inaccessibility from absence for the two
/// *unmangled* member kinds. PHP stores a private property under its declaring
/// class's own key and does not inherit a private constant, so when the
/// declaration sits on an ancestor rather than the receiver's own class the name
/// is simply **not there** for this site: `class A { private $p; } class B
/// extends A {}` gives `Warning: Undefined property: B::$p`, and the constant
/// form gives `Error: Undefined constant B::K` — different consequences, not
/// this id. A `protected` member is inherited normally and fires from anywhere
/// in the chain (`Cannot access protected property B::$p`, witnessed).
fn declared_in_chain<'a, T>(
    chain: &MemberChain<'a>,
    mut find: impl FnMut(&'a ClassDecl) -> Option<T>,
) -> Option<(&'a ClassDecl, T, bool)> {
    for (i, (_, cd)) in chain.nodes.iter().enumerate() {
        if let Some(found) = find(cd) {
            return Some((cd, found, i == 0));
        }
    }
    None
}

/// `property.inaccessible` (ADR-0078, issue #185) at a property read or write whose
/// receiver is an allocation-proven object.
///
/// `write` selects the magic fallback PHP would route through (`__set` for a write,
/// `__get` for a read) and the sentence's verb; both are witnessed to swallow the
/// access entirely.
pub(crate) fn check_inaccessible_property(
    w: &WalkCx,
    var: &str,
    prop: &str,
    store: &Store,
    write: bool,
    span: Span,
    out: &mut Vec<Diagnostic>,
) {
    let cx = w.cx;
    if w.scope.poisoned || !class_scope_known(w.scope) {
        return;
    }
    // The exact-receiver lane (`$x = new C;`). `$this` is a lower bound and never
    // reaches here, which costs nothing: a private member of `$this`'s own class is
    // visible, and a private one from an ancestor is the *undefined*-property
    // warning, not this fatal.
    let Some(obj) = store.obj_of(var) else { return };
    if !obj.class_exact {
        return;
    }
    let magic: &[&str] = if write { &["__set"] } else { &["__get"] };
    let Some(chain) = enumerate_member_chain(cx, &obj.class, magic) else {
        return;
    };
    // A property hook makes the member virtual, and a hooked declaration OVERRIDES an
    // inherited one (`php -r` witness at 8.5.9: a child's
    // `public int $p { get => 42; }` over a parent's `protected int $p` prints `42`).
    // Class-body hooked properties are dropped at lowering, so the walk cannot judge
    // their visibility — anywhere in the chain, the name is silence.
    if chain.nodes.iter().any(|(_, cd)| cd.hooked_properties.iter().any(|h| h == prop)) {
        return;
    }
    // A static property is a different access form (`C::$p`) with a different lookup;
    // `$obj->p` never reaches one.
    let Some((declaring, decl, on_own_class)) = declared_in_chain(&chain, |cd| {
        cd.properties.iter().find(|p| !p.is_static && p.name == prop)
    }) else {
        return;
    };
    // The promoted-param spelling of the same thing, which DOES stay on the surface.
    if decl.hooked {
        return;
    }
    if decl.visibility == Visibility::Private && !on_own_class {
        return; // absence, not inaccessibility — see `declared_in_chain`.
    }
    let Some(vis) = member_inaccessible(cx, decl.visibility, &declaring.fqn, w.enclosing_class)
    else {
        return;
    };
    if chain.any_conditional && !cx.dam.is_clear() {
        return;
    }
    let pos = cx.tree().position(span.start);
    let (verb, magic_name) = if write { ("write", "__set") } else { ("read", "__get") };
    out.push(Diagnostic {
        id: PROPERTY_INACCESSIBLE_ID,
        facet: None,
        fix: None,
        path: cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        message: format!(
            "{verb} of {vis} property ${var}->{prop} from {} — declared by {}, \
             hierarchy fully enumerated ({}), no {magic_name} — proven Error \
             (Cannot access {vis} property {}::${prop})",
            scope_render(cx, w.enclosing_class),
            declaring.name,
            chain.render(),
            declaring.name,
        ),
    });
}

/// `class-const.inaccessible` (ADR-0078, issue #185) at a `C::K` fetch.
///
/// Only an explicitly **named** class is a subject. `self::` and `parent::` are
/// lexically fixed and could be read here too, but neither can ever fire: `self::K`
/// resolves in the enclosing class's own scope, and `parent::`/`self::` reaching a
/// `protected` constant is by definition a related scope. `static::K` is late-bound
/// and unproven (ADR-0043 §1).
///
/// There is no magic leg: `__get` and `__callStatic` are both witnessed *not* to
/// intercept a constant fetch, so the chain is enumerated with an empty fallback
/// list and only its completeness matters.
pub(crate) fn check_inaccessible_class_const(
    w: &WalkCx,
    sc: &StaticClass,
    name: &str,
    span: Span,
    out: &mut Vec<Diagnostic>,
) {
    let cx = w.cx;
    if w.scope.poisoned || !class_scope_known(w.scope) || name.eq_ignore_ascii_case("class") {
        return;
    }
    let StaticClass::Named(r) = sc else { return };
    let class_fqn = cx.class_fqn(r);
    let Some(chain) = enumerate_member_chain(cx, &class_fqn, &[]) else {
        return;
    };
    // Constant names are case-sensitive in PHP, so the match is exact. An enum case
    // is not a constant here — cases live in `enum_cases` and are always public — so
    // `Suit::Hearts` finds nothing and stays silent.
    let Some((declaring, visibility, on_own_class)) = declared_in_chain(&chain, |cd| {
        cd.const_visibility.iter().find(|(n, _)| n == name).map(|(_, v)| *v)
    }) else {
        return;
    };
    if visibility == Visibility::Private && !on_own_class {
        return; // `Undefined constant B::K` — absence, not inaccessibility.
    }
    let Some(vis) = member_inaccessible(cx, visibility, &declaring.fqn, w.enclosing_class) else {
        return;
    };
    if chain.any_conditional && !cx.dam.is_clear() {
        return;
    }
    // PHP names the class as *written* at the site in this message
    // (`Cannot access protected constant B::K` for an inherited constant), not the
    // declaring class — witnessed.
    let written = chain.nodes.first().map_or(class_fqn.as_str(), |(_, cd)| cd.name.as_str());
    let pos = cx.tree().position(span.start);
    out.push(Diagnostic {
        id: CLASS_CONST_INACCESSIBLE_ID,
        facet: None,
        fix: None,
        path: cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        message: format!(
            "fetch of {vis} class constant {written}::{name} from {} — declared by {}, \
             hierarchy fully enumerated ({}), constants have no magic fallback — \
             proven Error (Cannot access {vis} constant {written}::{name})",
            scope_render(cx, w.enclosing_class),
            declaring.name,
            chain.render(),
        ),
    });
}

// end inaccessible members (ADR-0078, issue #185)
