//! The throw system (ADR-0040 damming / ADR-0007 checked accounting): the throw
//! fixpoint over the resolved call graph — `throws(f) = escaping own-throws(f) ∪
//! ⋃ filter(throws(callee), caller-guards)` with an exhaustiveness bit tainted by
//! dynamic calls — plus the whole-project `throw.undeclared` / `throw.liskov-widened`
//! diagnostics built on it. The escape sweep ([`crate::escapes`]) and the effects
//! pass ([`crate::purity`]) consume the fixpoint through the `pub(crate)` items.

use std::collections::{HashMap, HashSet};

use steins_domain::Certainty;
use steins_phpdoc::{Type as PType, TagKind, scan_docblock};
use steins_phpdoc::ast::TypeKind as PKind;
use steins_syntax::{
    CatchClause, ClassDecl, MethodDecl, NameRef, RefKind, ScopeOwner, ThrowKind, ThrowOrigin,
};

use crate::{
    Cx, Diagnostic, FileUnit, FnResolution, Index, Sym, THROW_LISKOV_ID, THROW_UNDECLARED_ID,
    parse_tag_type,
};
use crate::purity::resolve_effect_edge;
use crate::suppress::{Facet, Origin};

// ---------------------------------------------------------------------------
// Throw system (ADR-0040 damming / ADR-0007 checked accounting). Runs alongside
// the effect fixpoint over the same resolved call graph: `throws(f) = escaping
// own-throws(f) ∪ ⋃ filter(throws(callee), caller-guards)`, monotone to a
// fixpoint, with a throw-exhaustiveness bit tainted by dynamic/unresolved calls
// and opaque throws (mirroring effects).
// ---------------------------------------------------------------------------

/// One throw fact a unit can raise, with the provenance a `via` message needs.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ThrowFact {
    /// The thrown class, resolved to an FQN in its origin file's context.
    pub(crate) class: String,
    /// Display for the throwing construct (`new RuntimeException`, `intdiv()`).
    pub(crate) origin: String,
    /// The file the origin lives in (for cross-file position/provenance).
    pub(crate) origin_file: usize,
    /// The origin construct's span start in `origin_file`.
    pub(crate) offset: u32,
    pub(crate) line: u32,
    pub(crate) path: String,
}

/// A unit's throw fixpoint result: the set of throws that **escape** it (each
/// with an escape [`Certainty`] — only `Yes`/`Maybe` are stored; `No`/absorbed
/// throws never enter), plus whether the set is exhaustive (ADR-0040).
#[derive(Debug, Clone, Default)]
pub(crate) struct ThrowSet {
    pub(crate) facts: HashMap<ThrowFact, Certainty>,
    pub(crate) exhaustive: bool,
}

/// Wire a resolved callback's throws into the throw graph (ADR-0033), filtered by
/// the call site's `guards`: a closure/user callback is an edge; a builtin
/// callback contributes its curated throws directly; an unknown callback taints.
#[allow(clippy::too_many_arguments)]
fn add_callback_throws(
    cx: &Cx,
    file: usize,
    cbref: &steins_syntax::CallbackRef,
    span: steins_syntax::Span,
    guards: &[Vec<CatchClause>],
    d: &mut HashMap<ThrowFact, Certainty>,
    e: &mut Vec<(Sym, Vec<Vec<CatchClause>>)>,
    x: &mut bool,
) {
    match cbref {
        steins_syntax::CallbackRef::Closure(off) => {
            e.push((Sym::Closure(cx.path().to_owned(), *off), guards.to_vec()));
        }
        steins_syntax::CallbackRef::Named(name) => match cx.resolve_function(name) {
            FnResolution::User(site) => {
                e.push((Sym::Func(cx.fn_decl(site).fqn.clone()), guards.to_vec()));
            }
            FnResolution::Builtin(builtin_name) => {
                if let Some(classes) = steins_catalog::builtin_throws(&builtin_name) {
                    for c in classes {
                        let esc = escape_through_guards(cx, c, guards);
                        if esc == Certainty::No {
                            continue;
                        }
                        let line = cx.tree().position(span.start).line;
                        let fact = ThrowFact {
                            class: (*c).to_owned(),
                            origin: format!("{}()", name.simple()),
                            origin_file: file,
                            offset: span.start,
                            line,
                            path: cx.path().to_owned(),
                        };
                        let slot = d.entry(fact).or_insert(Certainty::No);
                        *slot = slot.or(esc);
                    }
                }
            }
            FnResolution::Unknown => *x = false,
        },
    }
}

/// `sub <: super` through the project inheritance chain **and** the builtin
/// exception table (ADR-0040), as a [`Certainty`]: `Yes` when the chain reaches
/// `super`; `No` when the chain is fully known (terminates at a project root or a
/// builtin root like `Throwable`) without reaching it; `Maybe` when the chain
/// leaves both the project and the builtin table (an unknown external class —
/// the FP-safe middle).
pub(crate) fn throw_subtype(cx: &Cx, sub_fqn: &str, sup_fqn: &str) -> Certainty {
    let sup = sup_fqn.trim_start_matches('\\');
    let mut cur = sub_fqn.trim_start_matches('\\').to_owned();
    let mut seen: HashSet<String> = HashSet::new();
    loop {
        if cur.eq_ignore_ascii_case(sup) {
            return Certainty::Yes;
        }
        if !seen.insert(cur.to_ascii_lowercase()) {
            return Certainty::Maybe; // cycle → give up
        }
        if let Some((file, cd)) = cx.find_class(&cur) {
            match &cd.parent {
                Some(pref) => cur = cx.units[file].tree.resolve_class_fqn(pref),
                None => return Certainty::No, // known project root, no match
            }
        } else if let Some(parent) = steins_catalog::builtin_exception_parent(&cur) {
            cur = parent.to_owned();
        } else if cur.eq_ignore_ascii_case("Throwable") {
            return Certainty::No; // known builtin root, no match
        } else {
            return Certainty::Maybe; // unknown external class — chain incomplete
        }
    }
}

/// Whether a thrown `class` is **checked** for envelope purposes (ADR-0007):
/// `No` (unchecked) for the `Error` / `LogicException` families, `Yes` when
/// provably neither, `Maybe` when the hierarchy is unknown (checked-but-Maybe —
/// envelope-silent, exhaustiveness-tainting).
pub(crate) fn throw_checked(cx: &Cx, class: &str) -> Certainty {
    let unchecked = throw_subtype(cx, class, "Error").or(throw_subtype(cx, class, "LogicException"));
    match unchecked {
        Certainty::Yes => Certainty::No,
        Certainty::No => Certainty::Yes,
        Certainty::Maybe => Certainty::Maybe,
    }
}

/// Whether one catch `clause` absorbs a thrown `sub` class (ADR-0040): `Yes` when
/// a caught member is provably a supertype; `Maybe` when a member might be (chain
/// leaves known territory) or the clause has an unnameable caught member; `No`
/// when no member can catch it.
fn clause_absorbs(cx: &Cx, sub: &str, clause: &CatchClause) -> Certainty {
    let mut r = if clause.has_unresolvable { Certainty::Maybe } else { Certainty::No };
    for cref in &clause.classes {
        let d = cx.class_fqn(cref);
        r = r.or(throw_subtype(cx, sub, &d));
        if r == Certainty::Yes {
            return Certainty::Yes;
        }
    }
    r
}

/// The escape [`Certainty`] of a `Yes`-arriving throw of `sub` past an ordered
/// (innermost-first) guard stack: `No` once a guard provably absorbs it; `Maybe`
/// if a guard might; else `Yes` (ADR-0040 damming, envelope-consumer side).
fn escape_through_guards(cx: &Cx, sub: &str, guards: &[Vec<CatchClause>]) -> Certainty {
    let mut maybe = false;
    for guard in guards {
        let mut absorb = Certainty::No;
        for clause in guard {
            absorb = absorb.or(clause_absorbs(cx, sub, clause));
            if absorb == Certainty::Yes {
                break;
            }
        }
        match absorb {
            Certainty::Yes => return Certainty::No,
            Certainty::Maybe => maybe = true,
            Certainty::No => {}
        }
    }
    if maybe { Certainty::Maybe } else { Certainty::Yes }
}

/// The unified throw fixpoint for every function/method in the project, keyed by
/// [`Sym`] (shared with the effect graph).
pub(crate) fn compute_throws(units: &[FileUnit], index: &Index) -> HashMap<Sym, ThrowSet> {
    struct Unit<'a> {
        sym: Sym,
        file: usize,
        class_fqn: Option<String>,
        origins: &'a [ThrowOrigin],
    }
    let mut ulist: Vec<Unit> = Vec::new();
    for (fi, u) in units.iter().enumerate() {
        for f in u.tree.functions() {
            ulist.push(Unit { sym: Sym::Func(f.fqn.clone()), file: fi, class_fqn: None, origins: &f.throw_origins });
        }
        for c in u.tree.classes() {
            for m in &c.methods {
                ulist.push(Unit {
                    sym: Sym::Method(c.fqn.clone(), m.name.clone()),
                    file: fi,
                    class_fqn: Some(c.fqn.clone()),
                    origins: &m.throw_origins,
                });
            }
        }
        // Closure/arrow bodies are throw nodes too (ADR-0033).
        for scope in u.tree.scopes() {
            if let ScopeOwner::Closure { def_offset } = &scope.owner {
                ulist.push(Unit {
                    sym: Sym::Closure(u.path.to_owned(), *def_offset),
                    file: fi,
                    class_fqn: None,
                    origins: &scope.throw_origins,
                });
            }
        }
    }

    type Edge = (Sym, Vec<Vec<CatchClause>>);
    let mut direct: HashMap<Sym, HashMap<ThrowFact, Certainty>> = HashMap::new();
    let mut edges: HashMap<Sym, Vec<Edge>> = HashMap::new();
    let mut ex: HashMap<Sym, bool> = HashMap::new();
    let mut sym_file: HashMap<Sym, usize> = HashMap::new();

    for unit in &ulist {
        let cx = Cx::new(units, index, unit.file);
        sym_file.insert(unit.sym.clone(), unit.file);
        let d = direct.entry(unit.sym.clone()).or_default();
        let e = edges.entry(unit.sym.clone()).or_default();
        let x = ex.entry(unit.sym.clone()).or_insert(true);
        classify_throw_origins(
            &cx,
            unit.file,
            unit.class_fqn.as_deref(),
            unit.origins,
            d,
            e,
            x,
        );
    }

    // Fixpoint: propagate callee throws through each call site's guards.
    let syms: Vec<Sym> = ulist.iter().map(|u| u.sym.clone()).collect();
    let mut facts = direct;
    loop {
        let mut changed = false;
        for sym in &syms {
            let file = sym_file[sym];
            let cx = Cx::new(units, index, file);
            let sym_edges: Vec<Edge> = edges.get(sym).cloned().unwrap_or_default();
            for (callee, guards) in &sym_edges {
                if ex.get(callee).copied() == Some(false) && ex.get(sym).copied() != Some(false) {
                    ex.insert(sym.clone(), false);
                    changed = true;
                }
                let callee_facts: Vec<(ThrowFact, Certainty)> =
                    facts.get(callee).into_iter().flatten().map(|(f, c)| (f.clone(), *c)).collect();
                for (fact, cert) in callee_facts {
                    let esc = escape_through_guards(&cx, &fact.class, guards);
                    let nc = cert.and(esc);
                    if nc == Certainty::No {
                        continue;
                    }
                    let slot = facts.entry(sym.clone()).or_default();
                    match slot.get(&fact).copied() {
                        Some(prev) => {
                            let merged = prev.or(nc);
                            if merged != prev {
                                slot.insert(fact, merged);
                                changed = true;
                            }
                        }
                        None => {
                            slot.insert(fact, nc);
                            changed = true;
                        }
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }

    syms.into_iter()
        .map(|s| {
            let f = facts.remove(&s).unwrap_or_default();
            let x = ex.get(&s).copied().unwrap_or(true);
            (s, ThrowSet { facts: f, exhaustive: x })
        })
        .collect()
}

/// Classify one unit's (or one **region**'s — ADR-0076) throw origins into the
/// throw fixpoint's accumulators. The regional twin of [`classify_effect_origins`]:
/// a sub-span of a body is asked exactly the question the whole body is asked, so
/// the loop transform's "proven throw set empty" precondition is the throw pass's
/// own verdict rather than a second opinion about what a throw is.
pub(crate) fn classify_throw_origins(
    cx: &Cx,
    file: usize,
    class_fqn: Option<&str>,
    origins: &[ThrowOrigin],
    d: &mut HashMap<ThrowFact, Certainty>,
    e: &mut Vec<(Sym, Vec<Vec<CatchClause>>)>,
    x: &mut bool,
) {
    let add_fact = |class: String, origin: String, span: steins_syntax::Span, cert: Certainty, d: &mut HashMap<ThrowFact, Certainty>| {
        if cert == Certainty::No {
            return;
        }
        let line = cx.tree().position(span.start).line;
        let fact = ThrowFact {
            class,
            origin,
            origin_file: file,
            offset: span.start,
            line,
            path: cx.path().to_owned(),
        };
        let slot = d.entry(fact).or_insert(Certainty::No);
        *slot = slot.or(cert);
    };
    for origin in origins {
        match &origin.kind {
            ThrowKind::New(class) => {
                let d_fqn = cx.class_fqn(class);
                let esc = escape_through_guards(cx, &d_fqn, &origin.guards);
                let display = format!("new {}", last_segment(&d_fqn));
                add_fact(d_fqn, display, origin.span, esc, d);
            }
            ThrowKind::Rethrow { caught, has_unresolvable } => {
                for cref in caught {
                    let d_fqn = cx.class_fqn(cref);
                    let esc = escape_through_guards(cx, &d_fqn, &origin.guards);
                    let display = format!("rethrow {}", last_segment(&d_fqn));
                    add_fact(d_fqn, display, origin.span, esc, d);
                }
                if *has_unresolvable {
                    *x = false;
                }
            }
            ThrowKind::Call(name) => match cx.resolve_function(name) {
                FnResolution::User(site) => {
                    e.push((Sym::Func(cx.fn_decl(site).fqn.clone()), origin.guards.clone()));
                }
                FnResolution::Builtin(builtin_name) => {
                    if let Some(classes) = steins_catalog::builtin_throws(&builtin_name) {
                        for c in classes {
                            let esc = escape_through_guards(cx, c, &origin.guards);
                            add_fact((*c).to_owned(), format!("{}()", name.simple()), origin.span, esc, d);
                        }
                    }
                }
                FnResolution::Unknown => *x = false,
            },
            ThrowKind::MethodCall { receiver, method } => {
                match resolve_effect_edge(cx, class_fqn, receiver, method) {
                    Some(callee) => e.push((callee, origin.guards.clone())),
                    None => *x = false,
                }
            }
            // A resolved callback's throws propagate through this call site's
            // guards (ADR-0033): a closure/user callback is an edge; a builtin
            // callback contributes its curated throws; unknown taints.
            ThrowKind::Callback { cbref } => {
                add_callback_throws(cx, file, cbref, origin.span, &origin.guards, d, e, x);
            }
            ThrowKind::HigherOrder { callee, callbacks, arg_count } => {
                match cx.resolve_invoker_function(callee) {
                    FnResolution::Builtin(builtin_name) => {
                        let shape = steins_catalog::invocation_shape(&builtin_name)
                            .expect("resolve_invoker_function's catalog_knows guarantees a shape row");
                        if shape.callback_param < *arg_count {
                            match callbacks.iter().find(|(p, _)| *p == shape.callback_param) {
                                Some((_, cbref)) => add_callback_throws(
                                    cx, file, cbref, origin.span, &origin.guards, d, e, x,
                                ),
                                None => *x = false,
                            }
                        }
                    }
                    FnResolution::User(_) | FnResolution::Unknown => match cx.resolve_function(callee) {
                        FnResolution::User(site) => {
                            e.push((Sym::Func(cx.fn_decl(site).fqn.clone()), origin.guards.clone()));
                        }
                        FnResolution::Builtin(builtin_name) => {
                            if let Some(classes) = steins_catalog::builtin_throws(&builtin_name) {
                                for c in classes {
                                    let esc = escape_through_guards(cx, c, &origin.guards);
                                    add_fact((*c).to_owned(), format!("{}()", callee.simple()), origin.span, esc, d);
                                }
                            }
                        }
                        FnResolution::Unknown => *x = false,
                    },
                }
            }
            ThrowKind::Taint => *x = false,
        }
    }
}

/// The last `\`-segment of an FQN (for a compact throw display).
pub(crate) fn last_segment(fqn: &str) -> &str {
    fqn.rsplit('\\').next().unwrap_or(fqn)
}

/// The declared `@throws` class FQNs of one docblock, resolved in the file's
/// context at `offset` (ADR-0040 envelope opt-in). Accepts bare class names and
/// unions of them; anything else contributes nothing. Empty ⇒ no envelope.
pub(crate) fn declared_throws(cx: &Cx, offset: u32, docblock: Option<&str>) -> Vec<String> {
    let Some(text) = docblock else { return Vec::new() };
    let mut out = Vec::new();
    for tag in scan_docblock(text) {
        if tag.kind != TagKind::Throws {
            continue;
        }
        let Some(ty) = parse_tag_type(&tag.type_text) else { continue };
        collect_class_names(&ty, &mut |name| {
            let fqn = resolve_class_name(cx, offset, name);
            if !out.contains(&fqn) {
                out.push(fqn);
            }
        });
    }
    out
}

/// Resolve a phpdoc class name to an FQN in the current file at `offset`.
pub(crate) fn resolve_class_name(cx: &Cx, offset: u32, name: &str) -> String {
    let raw = name.trim_start_matches('\\').to_owned();
    let kind = if name.starts_with('\\') {
        RefKind::FullyQualified
    } else if raw.contains('\\') {
        RefKind::Qualified
    } else {
        RefKind::Unqualified
    };
    cx.tree().resolve_class_fqn(&NameRef { raw, kind, offset })
}

/// Visit each plain class-name identifier in a phpdoc type that is a class name
/// or a union of class names; non-class members are ignored (no envelope).
pub(crate) fn collect_class_names(ty: &PType, f: &mut dyn FnMut(&str)) {
    match &ty.kind {
        PKind::Identifier(name) => f(name),
        PKind::Union { types, .. } => {
            for t in types {
                collect_class_names(t, f);
            }
        }
        PKind::Nullable(inner) => collect_class_names(inner, f),
        _ => {}
    }
}

/// The whole-project throw diagnostics: `throw.undeclared` envelope escapes and
/// `throw.liskov-widened` overrides (ADR-0040/0033).
///
/// `uncovered` is the dataflow walk's own ADR-0088 §5 verdict (issue #433):
/// per file index, the span-starts of default-less `match` statements proven
/// not to cover their subject's Verified domain. [`emit_undeclared`] consults it
/// to decide whether a structurally-recorded `UnhandledMatchError` origin (every
/// default-less `match`, scanned independently of coverage — see
/// [`scan_throw_origins`]'s own `Node::Match` arm) is a REPORTABLE contribution.
pub(crate) fn throw_diagnostics(
    units: &[FileUnit],
    index: &Index,
    uncovered: &HashMap<usize, HashSet<u32>>,
) -> Vec<Diagnostic> {
    // Fast path: nothing to check without a `@throws` tag anywhere.
    let any_throws = units.iter().any(|u| {
        let has = |d: Option<&str>| d.is_some_and(|t| t.contains("@throws") || t.contains("throws"));
        u.tree.functions().iter().any(|f| f.docblock.as_deref().is_some_and(|t| t.contains("throws")))
            || u.tree.classes().iter().any(|c| {
                c.methods.iter().any(|m| has(m.docblock.as_deref()))
            })
    });
    if !any_throws {
        return Vec::new();
    }

    let throws = compute_throws(units, index);
    let mut out = Vec::new();
    for fi in 0..units.len() {
        let cx = Cx::new(units, index, fi);
        for f in cx.tree().functions() {
            let declared = declared_throws(&cx, f.span.start, f.docblock.as_deref());
            if declared.is_empty() {
                continue;
            }
            let sym = Sym::Func(f.fqn.clone());
            emit_undeclared(
                &mut out, &cx, index, units, &sym, &f.name, &declared, &throws, &f.throw_origins,
                uncovered,
            );
        }
        for c in cx.tree().classes() {
            for m in &c.methods {
                let declared = declared_throws(&cx, m.span.start, m.docblock.as_deref());
                let display = format!("{}::{}", c.name, m.name);
                if !declared.is_empty() {
                    let sym = Sym::Method(c.fqn.clone(), m.name.clone());
                    emit_undeclared(
                        &mut out, &cx, index, units, &sym, &display, &declared, &throws,
                        &m.throw_origins, uncovered,
                    );
                }
                // Liskov: an override/impl whose declared throws widen the parent's.
                emit_liskov(&mut out, &cx, c, m, &declared);
            }
        }
    }
    out
}

/// Emit `throw.undeclared` for each checked, proven-escaping throw of `sym` not
/// covered by its declared `@throws` set.
#[allow(clippy::too_many_arguments)]
fn emit_undeclared(
    out: &mut Vec<Diagnostic>,
    cx: &Cx,
    index: &Index,
    units: &[FileUnit],
    sym: &Sym,
    display: &str,
    declared: &[String],
    throws: &HashMap<Sym, ThrowSet>,
    decl_origins: &[ThrowOrigin],
    uncovered: &HashMap<usize, HashSet<u32>>,
) {
    let Some(set) = throws.get(sym) else { return };
    let declared_list = declared.iter().map(|d| last_segment(d).to_owned()).collect::<Vec<_>>().join("|");
    let mut facts: Vec<(&ThrowFact, Certainty)> = set.facts.iter().map(|(f, c)| (f, *c)).collect();
    facts.sort_by(|a, b| (a.0.origin_file, a.0.offset, &a.0.class).cmp(&(b.0.origin_file, b.0.offset, &b.0.class)));
    for (fact, cert) in facts {
        if cert != Certainty::Yes {
            continue; // Maybe-escape is silent (ADR-0040)
        }
        // ADR-0088 §5 (issue #433): `UnhandledMatchError` is an `Error` — unchecked
        // by ADR-0007's family default, `throw_checked` below says so, and every
        // OTHER `Error`/`LogicException` throw stays exactly that unchecked. This
        // one class earns the opposite answer for the reason ADR-0007's own
        // rationale gives for the default: the proof layer is supposed to own
        // `Error`, by proving the throwing branch dead. A default-less `match`
        // missing a case is precisely the shape the proof layer has nothing to
        // prove dead — the coverage verdict already establishes it is LIVE — so
        // it is checked, but ONLY where `fact.offset` names a construct
        // `file_uncovered` actually proved uncovered. Every other
        // `UnhandledMatchError` fact (a covered match, an unstructured/opaque one
        // the walk never judged, or one this pass's own `descent.is_none()`
        // restriction left unrecorded) falls through to the ordinary unchecked
        // answer below and never reaches this id — silence being the safe
        // default for anything this gate did not itself prove live.
        let is_unhandled_match_error = fact.class.trim_start_matches('\\').eq_ignore_ascii_case("UnhandledMatchError");
        let checked = if is_unhandled_match_error {
            let uncovered_here = uncovered
                .get(&fact.origin_file)
                .is_some_and(|spans| spans.contains(&fact.offset));
            if uncovered_here { Certainty::Yes } else { Certainty::No }
        } else {
            throw_checked(cx, &fact.class)
        };
        if checked != Certainty::Yes {
            continue; // unchecked or unknown-hierarchy — never counts
        }
        // Covered iff a subclass of some declared class (Yes through chain).
        let covered = declared.iter().any(|d| throw_subtype(cx, &fact.class, d) != Certainty::No);
        if covered {
            continue; // Yes (covered) or Maybe (unproven) → silent
        }
        let ocx = Cx::new(units, index, fact.origin_file);
        let pos = ocx.tree().position(fact.offset);
        let simple = last_segment(&fact.class);
        let msg = format!(
            "{simple} can escape {display}() but is not declared (@throws {declared_list}) — proven escape"
        );
        // The `origin` facet (ADR-0050 §4), productionizing the measurement note's
        // rule: DIRECT iff the escaping throw's origin is in the annotated
        // declaration's OWN body — same file as the declaration (`cx.cur`) *and* a
        // member of its own scanned `throw_origins` — else PROPAGATED (it arrived up
        // a call edge). The origin offset is a unique file byte position and
        // `throw_origins` is scoped to this one declaration's body, so the
        // same-file-plus-own-origin test is exact even when a callee shares the file.
        let origin = if fact.origin_file == cx.cur
            && decl_origins.iter().any(|o| o.span.start == fact.offset)
        {
            Origin::Direct
        } else {
            Origin::Propagated
        };
        out.push(Diagnostic {
            id: THROW_UNDECLARED_ID,
            path: fact.path.clone(),
            line: pos.line,
            column: pos.column,
            message: msg,
            facet: Some(Facet::Origin(origin)),
            fix: None,
        });
    }
}

/// Emit `throw.liskov-widened` when a child method's declared `@throws` names a
/// checked class covered by none of the nearest ancestor method's declared
/// `@throws` (both sides must declare; `Maybe` resolution is silent).
fn emit_liskov(out: &mut Vec<Diagnostic>, cx: &Cx, class: &ClassDecl, m: &MethodDecl, child_declared: &[String]) {
    if child_declared.is_empty() {
        return;
    }
    // Every abstraction carrier of this method: the nearest parent class declaring
    // `@throws`, plus every implemented/extended interface declaring it (ADR-0033).
    for (abs_display, abs_declared) in collect_abstraction_throws(cx, class, &m.name) {
        if abs_declared.is_empty() {
            continue;
        }
        let abs_list = abs_declared.iter().map(|d| last_segment(d)).collect::<Vec<_>>().join("|");
        for c in child_declared {
            // A child-declared class widens iff it is a subclass of NO abstraction class.
            let covered = abs_declared.iter().any(|p| throw_subtype(cx, c, p) != Certainty::No);
            if covered {
                continue;
            }
            let pos = cx.tree().position(m.span.start);
            let msg = format!(
                "{} is declared thrown by {}::{}() but {abs_display}::{}() (its abstraction) declares only @throws {abs_list} — Liskov widening",
                last_segment(c), class.name, m.name, m.name
            );
            out.push(Diagnostic {
                id: THROW_LISKOV_ID,
                path: cx.path().to_owned(),
                line: pos.line,
                column: pos.column,
                message: msg,
                facet: None,
                fix: None,
            });
        }
    }
}

/// Every abstraction carrier of `method` with a declared `@throws` envelope: the
/// nearest parent CLASS declaring it (existing behavior), plus each interface the
/// class implements/extends (transitively) declaring it (ADR-0033 Liskov).
fn collect_abstraction_throws(cx: &Cx, class: &ClassDecl, method: &str) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    if let Some(p) = nearest_parent_throws(cx, class, method) {
        out.push(p);
    }
    for (display, file, im) in interface_abstraction_methods(cx, class, method) {
        let icx = Cx::new(cx.units, cx.index, file);
        let declared = declared_throws(&icx, im.span.start, im.docblock.as_deref());
        if !declared.is_empty() {
            out.push((display, declared));
        }
    }
    out
}

/// The nearest ancestor class (walking `extends`, non-interfaces only) that
/// declares a method named `method` with a `@throws` docblock, returning its class
/// name and declared set.
fn nearest_parent_throws(cx: &Cx, class: &ClassDecl, method: &str) -> Option<(String, Vec<String>)> {
    let mut cur = class.parent.as_ref().map(|p| cx.class_fqn(p))?;
    let mut seen: HashSet<String> = HashSet::new();
    loop {
        if !seen.insert(cur.to_ascii_lowercase()) {
            return None;
        }
        let (file, cd) = cx.find_class(&cur)?;
        // An interface reached via `parent` (an interface's `extends`) is handled by
        // the interface walker, not the parent-class chain.
        if cd.is_interface {
            return None;
        }
        if let Some(pm) = cd.methods.iter().find(|pm| pm.name.eq_ignore_ascii_case(method)) {
            let pcx = Cx::new(cx.units, cx.index, file);
            let declared = declared_throws(&pcx, pm.span.start, pm.docblock.as_deref());
            if !declared.is_empty() {
                return Some((cd.name.clone(), declared));
            }
        }
        cur = cx.units[file].tree.resolve_class_fqn(cd.parent.as_ref()?);
    }
}

/// Every interface method (in an implemented/extended interface, transitively) a
/// class's `method` implements — `(interface display, file, &MethodDecl)`
/// (ADR-0033 Liskov). BFS over `implements` (and each interface's own
/// `parent`/`implements` extends chain); dedup by interface FQN.
pub(crate) fn interface_abstraction_methods<'a>(
    cx: &Cx<'a>,
    class: &ClassDecl,
    method: &str,
) -> Vec<(String, usize, &'a MethodDecl)> {
    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    // Seed with the class's directly-implemented interfaces.
    let mut queue: Vec<String> = class.implements.iter().map(|r| cx.class_fqn(r)).collect();
    while let Some(fqn) = queue.pop() {
        if !seen.insert(fqn.to_ascii_lowercase()) {
            continue;
        }
        let Some((file, id)) = cx.find_class(&fqn) else { continue };
        if !id.is_interface {
            continue; // only interfaces are abstraction carriers here
        }
        if let Some(im) = id.methods.iter().find(|im| im.name.eq_ignore_ascii_case(method)) {
            out.push((id.name.clone(), file, im));
        }
        // An interface's extended interfaces (parent + implements) are abstractions too.
        let itree = cx.units[file].tree;
        if let Some(p) = &id.parent {
            queue.push(itree.resolve_class_fqn(p));
        }
        for r in &id.implements {
            queue.push(itree.resolve_class_fqn(r));
        }
    }
    out
}
