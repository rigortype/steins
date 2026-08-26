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

use crate::contract::parse_tag_type;
use crate::cx::Cx;
use crate::facts::FileFacts;
use crate::project::{Diagnostic, FileUnit, FnResolution, Index};
use crate::{Fixpoints, Gate, Sym, THROW_LISKOV_ID, THROW_UNDECLARED_ID};
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
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    derive(serde::Serialize, serde::Deserialize),
    serde(deny_unknown_fields)
)]
pub(crate) struct ThrowFact {
    /// The thrown class, resolved to an FQN in its origin file's context.
    pub(crate) class: String,
    /// Display for the throwing construct (`new RuntimeException`, `intdiv()`).
    pub(crate) origin: String,
    /// The origin construct's span start in the file `path` names.
    pub(crate) offset: u32,
    pub(crate) line: u32,
    /// The origin file's diagnostic path — the fact's file identity (issue #497).
    /// A consumer that needs the file's per-run units index (a `Cx` rebuild, the
    /// unit-order emission sort) derives it through [`Index::file_index_of`].
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

/// One `catch` clause with its caught class names already resolved to FQNs
/// (issue #516).
///
/// The lowering hands a [`CatchClause`] whose classes are [`NameRef`]s, and a
/// `NameRef` only means something against its own file's namespace and
/// imports. Resolving at *propagation* time therefore made the throw fixpoint
/// read the caller's tree — one of the whole-universe tree consumers issue
/// #516 exists to remove, and the only one hiding inside a fixpoint rather
/// than in plain sight. Resolving at *classification* time is where the tree
/// is in hand anyway, and it makes the own row a self-contained value: the
/// persisted form needs no file context to be read back.
///
/// Nothing about the verdict moves. `clause_absorbs` resolved exactly these
/// names through exactly this file's context; the only change is when.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    derive(serde::Serialize, serde::Deserialize),
    serde(deny_unknown_fields)
)]
pub(crate) struct ResolvedCatch {
    /// The caught classes as project FQNs. Empty with [`Self::has_unresolvable`]
    /// set means "caught, but we cannot name what".
    pub(crate) classes: Vec<String>,
    pub(crate) has_unresolvable: bool,
}

/// One call site's guard stack, innermost first, resolved.
pub(crate) type Guards = Vec<Vec<ResolvedCatch>>;

/// Resolve a lowered guard stack in `cx`'s file context — the one place a
/// catch clause's names meet a tree.
fn resolve_guards(cx: &Cx, guards: &[Vec<CatchClause>]) -> Guards {
    guards
        .iter()
        .map(|guard| {
            guard
                .iter()
                .map(|clause| ResolvedCatch {
                    classes: clause.classes.iter().map(|cref| cx.class_fqn(cref)).collect(),
                    has_unresolvable: clause.has_unresolvable,
                })
                .collect()
        })
        .collect()
}

/// One unit's **own** contribution to the throw fixpoint — everything
/// [`classify_throw_origins`] proves about a declaration in isolation, before
/// any propagation (issue #489). This is the propagation-independent half of
/// the throw system, and the value ADR-0092 §5's per-package artifact will
/// persist per declaration: the fixpoint itself is re-run from complete own
/// rows at every generation, never cached.
///
/// The edges here are the *resolved* `Sym` edges of this run; the persisted
/// form (the second half of #489) stores them unresolved and re-resolves
/// against the generation's merged index.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ThrowOwnRow {
    /// The unit's own escaping throw facts, each at its best escape
    /// [`Certainty`] past the origin's enclosing guards (`No` never enters).
    pub(crate) facts: HashMap<ThrowFact, Certainty>,
    /// The own-exhaustiveness bit: `false` once any origin in this body is
    /// dynamic/unresolved. Propagation can only lower it further.
    pub(crate) exhaustive: bool,
    /// Guarded call edges: the resolved callee plus the ordered
    /// (innermost-first) guard stacks the callee's throws must escape through,
    /// with the caught class names already resolved ([`ResolvedCatch`]).
    pub(crate) edges: Vec<(Sym, Guards)>,
}

impl ThrowOwnRow {
    /// Fold another row for the same [`Sym`] into this one — the twin of
    /// [`crate::purity::EffectOwnRow::absorb`], and equal to classifying both
    /// bodies into one row for the same reason: the fact map joins by
    /// [`Certainty::or`] exactly as `classify_throw_origins` does, the edge
    /// list concatenates (propagation is order-independent), and the
    /// exhaustiveness bit only ever clears.
    pub(crate) fn absorb(&mut self, other: &Self) {
        for (fact, cert) in &other.facts {
            let slot = self.facts.entry(fact.clone()).or_insert(Certainty::No);
            *slot = slot.or(*cert);
        }
        self.exhaustive &= other.exhaustive;
        self.edges.extend(other.edges.iter().cloned());
    }

    /// The empty row: a unit with no origins raises nothing and is exhaustive.
    pub(crate) fn new() -> Self {
        Self { facts: HashMap::new(), exhaustive: true, edges: Vec::new() }
    }
}

/// Wire a resolved callback's throws into the throw graph (ADR-0033), filtered by
/// the call site's `guards`: a closure/user callback is an edge; a builtin
/// callback contributes its curated throws directly; an unknown callback taints.
fn add_callback_throws(
    cx: &Cx,
    cbref: &steins_syntax::CallbackRef,
    span: steins_syntax::Span,
    guards: &[Vec<ResolvedCatch>],
    row: &mut ThrowOwnRow,
) {
    match cbref {
        steins_syntax::CallbackRef::Closure(off) => {
            row.edges.push((Sym::Closure(cx.path().to_owned(), *off), guards.to_vec()));
        }
        steins_syntax::CallbackRef::Named(name) => match cx.resolve_function(name) {
            FnResolution::User(site) => {
                row.edges.push((Sym::Func(cx.fn_decl(site).fqn.clone()), guards.to_vec()));
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
                            offset: span.start,
                            line,
                            path: cx.path().to_owned(),
                        };
                        let slot = row.facts.entry(fact).or_insert(Certainty::No);
                        *slot = slot.or(esc);
                    }
                }
            }
            FnResolution::Unknown => row.exhaustive = false,
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
fn clause_absorbs(cx: &Cx, sub: &str, clause: &ResolvedCatch) -> Certainty {
    let mut r = if clause.has_unresolvable { Certainty::Maybe } else { Certainty::No };
    for d in &clause.classes {
        r = r.or(throw_subtype(cx, sub, d));
        if r == Certainty::Yes {
            return Certainty::Yes;
        }
    }
    r
}

/// The escape [`Certainty`] of a `Yes`-arriving throw of `sub` past an ordered
/// (innermost-first) guard stack: `No` once a guard provably absorbs it; `Maybe`
/// if a guard might; else `Yes` (ADR-0040 damming, envelope-consumer side).
fn escape_through_guards(cx: &Cx, sub: &str, guards: &[Vec<ResolvedCatch>]) -> Certainty {
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
/// [`Sym`] (shared with the effect graph): the own rows, then propagation over
/// them. The two halves are separate functions because they have separate
/// futures (issue #489 / ADR-0092 §5): the rows become the persisted
/// per-declaration summaries, while propagation re-runs from complete rows at
/// every generation.
pub(crate) fn compute_throws(
    units: &[FileUnit],
    index: &Index,
    facts: &[FileFacts],
) -> HashMap<Sym, ThrowSet> {
    let (syms, files, rows) = throw_own_rows(units, index, facts);
    propagate_throws(units, index, &syms, &files, &rows)
}

/// Classify every unit's own throw contribution into one [`ThrowOwnRow`] per
/// [`Sym`]. Also returns the syms in unit order (duplicates preserved — the
/// final collect depends on the order) and each sym's file index, which the
/// propagation loop needs to resolve guard classes in the caller's context.
fn throw_own_rows(
    units: &[FileUnit],
    index: &Index,
    facts: &[FileFacts],
) -> (Vec<Sym>, HashMap<Sym, usize>, HashMap<Sym, ThrowOwnRow>) {
    struct Unit<'a> {
        sym: Sym,
        file: usize,
        class_fqn: Option<String>,
        origins: &'a [ThrowOrigin],
    }
    // The files whose rows this run already holds (issue #516) — folded in
    // without their trees being decoded, in the order the enumeration below
    // would have produced.
    let mut syms: Vec<Sym> = Vec::new();
    let mut rows: HashMap<Sym, ThrowOwnRow> = HashMap::new();
    let mut sym_file: HashMap<Sym, usize> = HashMap::new();
    let mut persisted_order: Vec<(usize, Sym)> = Vec::new();
    let mut ulist: Vec<Unit> = Vec::new();
    for (fi, u) in units.iter().enumerate() {
        if let Some(persisted) = facts.get(fi).and_then(|f| f.rows.as_ref()) {
            syms.extend(persisted.syms.iter().cloned());
            persisted_order.extend(persisted.syms.iter().map(|s| (fi, s.clone())));
            for (sym, row) in &persisted.throws {
                sym_file.insert(sym.clone(), fi);
                rows.entry(sym.clone()).or_insert_with(ThrowOwnRow::new).absorb(row);
            }
            continue;
        }
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

    for unit in &ulist {
        let cx = Cx::new(units, index, unit.file);
        sym_file.insert(unit.sym.clone(), unit.file);
        let row = rows.entry(unit.sym.clone()).or_insert_with(ThrowOwnRow::new);
        classify_throw_origins(&cx, unit.class_fqn.as_deref(), unit.origins, row);
    }
    if syms.is_empty() {
        return (ulist.into_iter().map(|u| u.sym).collect(), sym_file, rows);
    }
    let mut merged: Vec<Sym> = Vec::with_capacity(persisted_order.len() + ulist.len());
    let mut fresh = ulist.into_iter().peekable();
    for (fi, sym) in persisted_order {
        while fresh.peek().is_some_and(|u| u.file < fi) {
            merged.push(fresh.next().expect("peeked").sym);
        }
        merged.push(sym);
    }
    merged.extend(fresh.map(|u| u.sym));
    (merged, sym_file, rows)
}

/// Fixpoint: propagate callee throws through each call site's guards, from the
/// complete own rows. Monotone and order-independent (ADR-0048 §4); the rows
/// are read-only — propagated state never flows back into an own row.
fn propagate_throws(
    units: &[FileUnit],
    index: &Index,
    syms: &[Sym],
    files: &HashMap<Sym, usize>,
    rows: &HashMap<Sym, ThrowOwnRow>,
) -> HashMap<Sym, ThrowSet> {
    let mut facts: HashMap<Sym, HashMap<ThrowFact, Certainty>> =
        rows.iter().map(|(s, r)| (s.clone(), r.facts.clone())).collect();
    let mut ex: HashMap<Sym, bool> =
        rows.iter().map(|(s, r)| (s.clone(), r.exhaustive)).collect();
    loop {
        let mut changed = false;
        for sym in syms {
            let file = files[sym];
            let cx = Cx::new(units, index, file);
            let Some(row) = rows.get(sym) else { continue };
            for (callee, guards) in &row.edges {
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

    syms.iter()
        .map(|s| {
            let f = facts.remove(s).unwrap_or_default();
            let x = ex.get(s).copied().unwrap_or(true);
            (s.clone(), ThrowSet { facts: f, exhaustive: x })
        })
        .collect()
}

/// Classify one unit's (or one **region**'s — ADR-0076) throw origins into its
/// [`ThrowOwnRow`]. The regional twin of [`classify_effect_origins`]:
/// a sub-span of a body is asked exactly the question the whole body is asked, so
/// the loop transform's "proven throw set empty" precondition is the throw pass's
/// own verdict rather than a second opinion about what a throw is.
pub(crate) fn classify_throw_origins(
    cx: &Cx,
    class_fqn: Option<&str>,
    origins: &[ThrowOrigin],
    row: &mut ThrowOwnRow,
) {
    let add_fact = |class: String, origin: String, span: steins_syntax::Span, cert: Certainty, d: &mut HashMap<ThrowFact, Certainty>| {
        if cert == Certainty::No {
            return;
        }
        let line = cx.tree().position(span.start).line;
        let fact = ThrowFact {
            class,
            origin,
            offset: span.start,
            line,
            path: cx.path().to_owned(),
        };
        let slot = d.entry(fact).or_insert(Certainty::No);
        *slot = slot.or(cert);
    };
    for origin in origins {
        // One resolution per origin, in the file whose tree is in hand — see
        // [`ResolvedCatch`] for why this moved out of propagation.
        let guards = resolve_guards(cx, &origin.guards);
        match &origin.kind {
            ThrowKind::New(class) => {
                let d_fqn = cx.class_fqn(class);
                let esc = escape_through_guards(cx, &d_fqn, &guards);
                let display = format!("new {}", last_segment(&d_fqn));
                add_fact(d_fqn, display, origin.span, esc, &mut row.facts);
            }
            ThrowKind::Rethrow { caught, has_unresolvable } => {
                for cref in caught {
                    let d_fqn = cx.class_fqn(cref);
                    let esc = escape_through_guards(cx, &d_fqn, &guards);
                    let display = format!("rethrow {}", last_segment(&d_fqn));
                    add_fact(d_fqn, display, origin.span, esc, &mut row.facts);
                }
                if *has_unresolvable {
                    row.exhaustive = false;
                }
            }
            ThrowKind::Call(name) => match cx.resolve_function(name) {
                FnResolution::User(site) => {
                    row.edges.push((Sym::Func(cx.fn_decl(site).fqn.clone()), guards.clone()));
                }
                FnResolution::Builtin(builtin_name) => {
                    if let Some(classes) = steins_catalog::builtin_throws(&builtin_name) {
                        for c in classes {
                            let esc = escape_through_guards(cx, c, &guards);
                            add_fact((*c).to_owned(), format!("{}()", name.simple()), origin.span, esc, &mut row.facts);
                        }
                    }
                }
                FnResolution::Unknown => row.exhaustive = false,
            },
            ThrowKind::MethodCall { receiver, method } => {
                match resolve_effect_edge(cx, class_fqn, receiver, method) {
                    Some(callee) => row.edges.push((callee, guards.clone())),
                    None => row.exhaustive = false,
                }
            }
            // A resolved callback's throws propagate through this call site's
            // guards (ADR-0033): a closure/user callback is an edge; a builtin
            // callback contributes its curated throws; unknown taints.
            ThrowKind::Callback { cbref } => {
                add_callback_throws(cx, cbref, origin.span, &guards, row);
            }
            ThrowKind::HigherOrder { callee, callbacks, arg_count } => {
                match cx.resolve_invoker_function(callee) {
                    FnResolution::Builtin(builtin_name) => {
                        let shape = steins_catalog::invocation_shape(&builtin_name)
                            .expect("resolve_invoker_function's catalog_knows guarantees a shape row");
                        if shape.callback_param < *arg_count {
                            match callbacks.iter().find(|(p, _)| *p == shape.callback_param) {
                                Some((_, cbref)) => add_callback_throws(
                                    cx, cbref, origin.span, &guards, row,
                                ),
                                None => row.exhaustive = false,
                            }
                        }
                    }
                    FnResolution::User(_) | FnResolution::Unknown => match cx.resolve_function(callee) {
                        FnResolution::User(site) => {
                            row.edges.push((Sym::Func(cx.fn_decl(site).fqn.clone()), guards.clone()));
                        }
                        FnResolution::Builtin(builtin_name) => {
                            if let Some(classes) = steins_catalog::builtin_throws(&builtin_name) {
                                for c in classes {
                                    let esc = escape_through_guards(cx, c, &guards);
                                    add_fact((*c).to_owned(), format!("{}()", callee.simple()), origin.span, esc, &mut row.facts);
                                }
                            }
                        }
                        FnResolution::Unknown => row.exhaustive = false,
                    },
                }
            }
            ThrowKind::Taint => row.exhaustive = false,
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
    fx: &Fixpoints<'_>,
    uncovered: &HashMap<usize, HashSet<u32>>,
) -> Vec<Diagnostic> {
    let (units, index) = (fx.units(), fx.index());
    // Fast path: nothing to check without a `@throws` tag anywhere. Read off
    // the per-file facts where the run has them (issue #516) — this gate used
    // to decode every tree in the universe to answer "no".
    if !fx.any(Gate::Throws) {
        return Vec::new();
    }

    let throws = fx.throws();
    let mut out = Vec::new();
    for fi in 0..units.len() {
        // Everything below is gated on `declared_throws` being non-empty —
        // `emit_undeclared` is skipped outright and `emit_liskov` returns at
        // its first line — so a file no declaration of which spells `@throws`
        // contributes nothing, and its tree stays undecoded.
        if !fx.spells(fi, Gate::Throws) {
            continue;
        }
        let cx = Cx::new(units, index, fi);
        for f in cx.tree().functions() {
            let declared = declared_throws(&cx, f.span.start, f.docblock.as_deref());
            if declared.is_empty() {
                continue;
            }
            let sym = Sym::Func(f.fqn.clone());
            emit_undeclared(
                &mut out, &cx, index, units, &sym, &f.name, &declared, throws, &f.throw_origins,
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
                        &mut out, &cx, index, units, &sym, &display, &declared, throws,
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
    // Each fact's origin file as its per-run units index, derived from the fact's
    // path (issue #497) — for the unit-order sort below, the uncovered lookup, and
    // the origin-file `Cx` rebuild.
    let origin_unit = |f: &ThrowFact| {
        index.file_index_of(&f.path).expect("a ThrowFact's path names a unit of this run")
    };
    let mut facts: Vec<(usize, &ThrowFact, Certainty)> =
        set.facts.iter().map(|(f, c)| (origin_unit(f), f, *c)).collect();
    // Sorted by the derived index, not the path string: unit order is what the
    // embedded per-run index gave here before the path became the identity, so
    // intermediate emission order is unchanged.
    facts.sort_by(|a, b| (a.0, a.1.offset, &a.1.class).cmp(&(b.0, b.1.offset, &b.1.class)));
    for (ofile, fact, cert) in facts {
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
                .get(&ofile)
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
        let ocx = Cx::new(units, index, ofile);
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
        let origin = if ofile == cx.cur
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
