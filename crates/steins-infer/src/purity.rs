//! The effects pass (ADR-0005): `#[\Steins\Pure]` envelope checking, project-wide.
//!
//! A monotone fixpoint over the resolved call graph — `effects(f) = own(f) ∪
//! ⋃ effects(callee)` with an exhaustiveness bit tainted by dynamic / unresolved
//! calls — feeding `effect.envelope-exceeded`, `effect.liskov-widened` and the
//! label-vocabulary ids, the [`PurityOracle`] the walker consults, and the
//! [`EffectSummary`] lane `annotate` and the JSON surface render. The graph's
//! node key, [`Sym`], stays in the crate root: the throw system and the escape
//! sweep key on it too.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use steins_db::{Db, EffectsPolicy, PluginFacts, Project, SourceFile, parse, project_index};
use steins_syntax::Span;
use steins_syntax::{
    ClassDecl, EffectEnvelope, EffectOrigin, EffectRecv, FunctionDecl, MethodDecl,
    NameRef, ScopeOwner, SourceTree, ThrowOrigin, Visibility,
};
use steins_phpdoc::{EnvelopeTag, TagKind, scan_docblock};

use crate::throws::{
    ThrowOwnRow, ThrowSet, classify_throw_origins, compute_throws,
    interface_abstraction_methods, last_segment,
};
use crate::cx::Cx;
use crate::dispatch::{Resolution, resolve_in_chain};
use crate::project::{Diagnostic, FileUnit, FnResolution, Index, LazyTree};
use crate::{
    EFFECT_ID, EFFECT_LISKOV_ID, Fixpoints, INTEROP_UNKNOWN_LABEL_ID, Sym, UNKNOWN_LABEL_ID,
};

// ---------------------------------------------------------------------------
// Effects pass (ADR-0005): `#[\Steins\Pure]` envelope checking, project-wide.
// ---------------------------------------------------------------------------

/// One proven effect a unit carries, with the provenance a transitive `via`
/// message needs.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct EffectFinding {
    pub(crate) label: String,
    origin: String,
    line: u32,
    /// The path of the file the origin lives in, so a transitive `via` message
    /// can name the other file when the effect arises cross-file.
    path: String,
    /// **How this copy arrived** (ADR-0084 §2): the attribution labels of every
    /// attributed symbol the effect crossed on its way here. Empty for a finding
    /// that arose in this unit's own body and for every project with no
    /// `[effects.attribution]` table.
    ///
    /// Part of `Hash`/`Eq`, so two copies of one effect that reached this unit
    /// along differently-attributed paths are distinct set elements — what makes
    /// leg 2 of the discharge rule a *must* over paths rather than a may: the
    /// copies are all present, and [`finding_groups`] quantifies over them.
    attributed: BTreeSet<String>,
}

impl EffectFinding {
    /// A finding with no attribution — everything the fixpoint proves directly,
    /// before any edge out of an attributed symbol has been crossed.
    fn direct(label: String, origin: String, line: u32, path: String) -> Self {
        Self { label, origin, line, path, attributed: BTreeSet::new() }
    }

    /// This finding as a caller receives it across an edge out of a symbol
    /// attributed `labels` (ADR-0084 §2). The attribution accumulates; nothing
    /// else moves.
    fn attributed_by(&self, labels: &[String]) -> Self {
        let mut copy = self.clone();
        copy.attributed.extend(labels.iter().cloned());
        copy
    }
}

/// One unit's fixpoint result: its proven effect findings, its **declared** lane,
/// and exhaustiveness.
///
/// The two lanes never mix (ADR-0067). `findings` is what inference *proved* —
/// the only lane `effect.envelope-exceeded` and `effect.liskov-widened` read, so
/// a declaration can never manufacture a finding. `declared` is what a
/// declaration *bounds*: the envelope labels imported at a call through an
/// interface-typed receiver, joined along call edges exactly as findings are. Both
/// lanes are stored raw — the display-time normalization that drops a declared
/// label already covered by a proven one lives in [`effect_summary_units`].
#[derive(Debug, Clone, Default)]
pub(crate) struct EffectSet {
    pub(crate) findings: HashSet<EffectFinding>,
    /// Declared-lane labels (ADR-0018 dot-paths), no provenance: they name a
    /// bound, not an origin, so nothing ever reports them at a source position.
    pub(crate) declared: HashSet<String>,
    pub(crate) exhaustive: bool,
    /// This unit's OWN `[effects.attribution]` labels (ADR-0084 §1), resolved once
    /// by [`compute_effects`]. Not a third lane: it says nothing about what this
    /// unit does, only what its effects are *for*, and it is read exclusively as
    /// the attribution a caller accumulates when [`Self::findings`] cross the edge
    /// out of here. Carried on the set so a reporting site holding a callee's
    /// [`EffectSet`] can fold in the same edge the fixpoint folds in.
    attribution: Vec<String>,
}

/// One unit's **own** contribution to the effect fixpoint — everything
/// [`classify_effect_origins`] proves about a declaration in isolation, before
/// any propagation (issue #489). This is the propagation-independent half of
/// the effects pass, and the value ADR-0092 §5's per-package artifact will
/// persist per declaration: the fixpoint itself is re-run from complete own
/// rows at every generation, never cached, which is what keeps warm ≡ cold
/// (a propagated finding embeds its *origin's* line/path, so caching it would
/// go stale on any callee-file edit).
///
/// The edges here are the *resolved* `Sym` edges of this run; the persisted
/// form (the second half of #489) stores them unresolved and re-resolves
/// against the generation's merged index. The unit's ADR-0084 attribution is
/// deliberately NOT a field: it is a fact of the `[effects]` policy table,
/// resolved by [`propagate_effects`] at propagation time.
#[derive(Debug, Clone)]
pub(crate) struct EffectOwnRow {
    /// The findings that arise in this unit's own body — with their attribution
    /// sets as full copies, never collapsed to labels (ADR-0084 §2).
    pub(crate) findings: HashSet<EffectFinding>,
    /// Declared-lane labels imported *locally* — one entry per call site whose
    /// receiver's declared interface method carries an envelope (ADR-0067).
    pub(crate) declared: HashSet<String>,
    /// The own-exhaustiveness bit: `false` once any origin in this body is
    /// dynamic/unresolved. Propagation can only lower it further.
    pub(crate) exhaustive: bool,
    /// Resolved call edges whose findings AND exhaustiveness taint propagate.
    pub(crate) edges: HashSet<Sym>,
    /// Edges whose findings propagate but whose exhaustiveness taint does not —
    /// a callee whose ADR-0063 conditional-purity contract was fully decided at
    /// the call site.
    pub(crate) untainting: HashSet<Sym>,
}

impl EffectOwnRow {
    /// The empty row: a unit with no origins has no effects and is exhaustive.
    pub(crate) fn new() -> Self {
        Self {
            findings: HashSet::new(),
            declared: HashSet::new(),
            exhaustive: true,
            edges: HashSet::new(),
            untainting: HashSet::new(),
        }
    }
}

/// Resolve a [`CallbackRef`] to its effect [`Sym`], for the [`Sym::Closure`] key.
/// A named callback resolving to a builtin/unknown returns `None` (the caller
/// handles those inline).
fn callback_effect_edge(cx: &Cx, cbref: &steins_syntax::CallbackRef) -> Option<Sym> {
    match cbref {
        steins_syntax::CallbackRef::Closure(off) => Some(Sym::Closure(cx.path().to_owned(), *off)),
        steins_syntax::CallbackRef::Named(name) => match cx.resolve_effect_function(name) {
            FnResolution::User(site) => Some(Sym::Func(cx.fn_decl(site).fqn.clone())),
            FnResolution::Builtin(_) | FnResolution::Unknown => None,
        },
    }
}

/// Wire a resolved callback into the effect graph (ADR-0033): a closure or user
/// function becomes an edge; a builtin callback contributes its catalog findings
/// directly; an unknown callback taints exhaustiveness (`…?`).
fn add_callback_effects(
    cx: &Cx,
    cbref: &steins_syntax::CallbackRef,
    span: steins_syntax::Span,
    policy: &EffectsPolicy,
    row: &mut EffectOwnRow,
) {
    match cbref {
        steins_syntax::CallbackRef::Closure(off) => {
            row.edges.insert(Sym::Closure(cx.path().to_owned(), *off));
        }
        steins_syntax::CallbackRef::Named(name) => match cx.resolve_effect_function(name) {
            FnResolution::User(site) => {
                row.edges.insert(Sym::Func(cx.fn_decl(site).fqn.clone()));
            }
            FnResolution::Builtin(builtin_name) => {
                // A builtin passed *as* a callback is invoked by the higher-order
                // callee with arguments of its choosing, never with an lvalue of
                // this frame — the conditional out-param row cannot apply.
                for f in
                    builtin_findings(&builtin_name, span, cx.tree(), cx.path(), None, None, policy)
                {
                    row.findings.insert(f);
                }
            }
            FnResolution::Unknown => row.exhaustive = false,
        },
    }
}

/// A function's declared **conditional-purity** contracts (ADR-0063 §2 decision 2),
/// resolved from parameter names to positional indices.
#[derive(Debug, Default, Clone)]
pub(crate) struct ConditionalPurity {
    /// Positions flagged by `@pure-unless-callable-is-impure $cb`: this
    /// function's envelope is the join of the callables bound here.
    callables: Vec<usize>,
    /// Positions flagged by `@pure-unless-parameter-passed $out`: this function
    /// is pure unless the argument is supplied. The declarative twin of a catalog
    /// out-param row — a userland row, written by the author instead of curated.
    passed: Vec<usize>,
}

impl ConditionalPurity {
    fn is_empty(&self) -> bool {
        self.callables.is_empty() && self.passed.is_empty()
    }
}

/// Read a declaration's conditional-purity tags, mapping each flagged parameter
/// name to its positional index. `None` when the docblock declares none.
///
/// A tag naming a parameter the signature does not have is dropped, not
/// diagnosed: the crate's tag discipline is that a malformed or stale tag costs
/// its own effect and nothing else.
fn conditional_purity(docblock: Option<&String>, params: &[steins_syntax::Param]) -> Option<ConditionalPurity> {
    let text = docblock?;
    // Cheap gate: both spellings share this substring, and it is vanishingly rare
    // in prose. Scanning every docblock in the project would not be.
    if !text.contains("pure-unless") {
        return None;
    }
    let mut cp = ConditionalPurity::default();
    for tag in scan_docblock(text) {
        let TagKind::ConditionalPurity(cond) = tag.kind else { continue };
        let Some(var) = &tag.var_name else { continue };
        let name = var.trim_start_matches('$');
        let Some(pos) = params.iter().position(|p| p.name == name) else { continue };
        let slot = match cond {
            steins_phpdoc::PurityCondition::CallableIsImpure => &mut cp.callables,
            steins_phpdoc::PurityCondition::ParameterIsPassed => &mut cp.passed,
        };
        if !slot.contains(&pos) {
            slot.push(pos);
        }
    }
    (!cp.is_empty()).then_some(cp)
}

/// How a call to a **user** function contributes to the caller's effect set, once
/// the callee's conditional-purity contracts (ADR-0063 §2 decision 2) are honored.
struct UserCallEffects {
    /// Whether the callee's *exhaustiveness taint* is discharged by its contract.
    ///
    /// A tagged function's body calls its callable parameter dynamically
    /// (`$cb(...)`), which is an [`EffectOrigin::Opaque`] and taints the callee
    /// forever — the very unprovability the contract exists to answer. When every
    /// flagged condition is decided at this call site (the callable is a
    /// resolvable callback, or the flagged argument is simply absent), the
    /// declaration discharges that taint.
    ///
    /// This does not invert ADR-0037's "proven beats declared": every finding the
    /// fixpoint *proved* about the callee still propagates. A declaration is only
    /// permitted to answer what inference left unknown.
    discharge_taint: bool,
    /// Labels the call contributes directly — the `@pure-unless-parameter-passed`
    /// leg, resolved against the argument's lvalue root exactly as a catalog
    /// out-param row would be.
    labels: Vec<&'static str>,
}

/// Evaluate a user callee's conditional-purity contracts against one call site.
///
/// `callbacks` are the resolvable callback arguments by position (empty for a
/// plain [`EffectOrigin::Call`]); `arg_targets` is `None` when positional mapping
/// was defeated, in which case no condition can be evaluated and nothing is
/// discharged.
fn eval_conditional_purity(
    cp: &ConditionalPurity,
    callbacks: &[(usize, steins_syntax::CallbackRef)],
    arg_targets: Option<&[steins_syntax::RefTarget]>,
    mut on_callback: impl FnMut(&steins_syntax::CallbackRef),
) -> UserCallEffects {
    let Some(targets) = arg_targets else {
        return UserCallEffects { discharge_taint: false, labels: Vec::new() };
    };
    let arity = targets.len();
    let mut discharge = true;
    let mut labels: Vec<&'static str> = Vec::new();
    for &p in &cp.callables {
        // Not supplied → the condition is vacuous and the function is pure.
        if p >= arity {
            continue;
        }
        match callbacks.iter().find(|(q, _)| *q == p) {
            // Visible callback: its envelope joins the caller's (ADR-0063
            // decision 1's semantic answer, reached through the declaration).
            Some((_, cbref)) => on_callback(cbref),
            // An opaque `callable` sits in the flagged slot — precisely the case
            // the contract cannot resolve either. The taint stands, as today.
            None => discharge = false,
        }
    }
    for &p in &cp.passed {
        let Some(&target) = targets.get(p) else { continue };
        let label = by_ref_label(target);
        if !labels.contains(&label) {
            labels.push(label);
        }
    }
    UserCallEffects { discharge_taint: discharge, labels }
}

/// The plugin channel's coloring for a statically-named call that resolved to
/// **nothing** — no project body, no catalog row (ADR-0068 §1).
///
/// Precedence is structural rather than compared: this is only ever reached from
/// the `FnResolution::Unknown` arm, so a builtin row and a project function have
/// both already won. The extra guards below are for the two shapes `Unknown` also
/// covers and a plugin must not speak for: an **ambiguous** name (the project does
/// define it, twice) and a **namespaced** name (a plugin manifest colors global
/// functions, which is what `acme_cache_get` is).
///
/// The caller puts the answer in the DECLARED lane and keeps the exhaustiveness
/// taint. That is the opposite of ADR-0067's interface-envelope import, and
/// deliberately so: an envelope is a checked contract (`effect.liskov-widened`
/// holds every analyzed implementation to it), while nothing checks a plugin's
/// assertion. Assert, never prove — so the summary reads "declared `acme.cache`,
/// and possibly more", which is the truth of an unchecked claim.
fn plugin_call_labels<'p>(
    cx: &Cx,
    plugins: &'p PluginFacts,
    name: &NameRef,
) -> Option<&'p [String]> {
    let simple = name.simple();
    if simple != name.raw.trim_start_matches('\\') {
        return None; // a namespaced userland name is not a global function
    }
    if cx.index.has_simple_function(simple) {
        return None; // the project defines it (ambiguously, or we would be elsewhere)
    }
    plugins.effect_labels(simple)
}

/// The unified effect fixpoint for **every** function and method in the whole
/// project, keyed by [`Sym`] (FQN-based, so cross-file edges match): the own
/// rows, then propagation over them. The two halves are separate functions
/// because they have separate futures (issue #489 / ADR-0092 §5): the rows
/// become the persisted per-declaration summaries, while propagation re-runs
/// from complete rows at every generation.
pub(crate) fn compute_effects(
    units: &[FileUnit],
    index: &Index,
    plugins: &PluginFacts,
    policy: &EffectsPolicy,
) -> HashMap<Sym, EffectSet> {
    let (syms, rows) = effect_own_rows(units, index, plugins, policy);
    propagate_effects(&syms, &rows, policy)
}

/// Classify every unit's own effect contribution into one [`EffectOwnRow`] per
/// [`Sym`]. Also returns the syms in unit order (duplicates preserved — the
/// final collect depends on the order).
fn effect_own_rows(
    units: &[FileUnit],
    index: &Index,
    plugins: &PluginFacts,
    policy: &EffectsPolicy,
) -> (Vec<Sym>, HashMap<Sym, EffectOwnRow>) {
    // Each effect unit with the file it lives in and its enclosing class FQN.
    struct Unit<'a> {
        sym: Sym,
        file: usize,
        class_fqn: Option<String>,
        origins: &'a [EffectOrigin],
        /// The frame's declared parameters — read only to type an ADR-0067
        /// declared receiver (`EffectRecv::Var`).
        params: &'a [steins_syntax::Param],
    }
    let mut ulist: Vec<Unit> = Vec::new();
    for (fi, u) in units.iter().enumerate() {
        for f in u.tree.functions() {
            ulist.push(Unit {
                sym: Sym::Func(f.fqn.clone()),
                file: fi,
                class_fqn: None,
                origins: &f.effect_origins,
                params: &f.params,
            });
        }
        for c in u.tree.classes() {
            for m in &c.methods {
                ulist.push(Unit {
                    sym: Sym::Method(c.fqn.clone(), m.name.clone()),
                    file: fi,
                    class_fqn: Some(c.fqn.clone()),
                    origins: &m.effect_origins,
                    params: &m.params,
                });
            }
        }
        // Closure/arrow bodies are effect nodes too (ADR-0033) — a HigherOrder /
        // Callback edge into one carries the callback's proven effects.
        for scope in u.tree.scopes() {
            if let ScopeOwner::Closure { def_offset } = &scope.owner {
                ulist.push(Unit {
                    sym: Sym::Closure(u.path.to_owned(), *def_offset),
                    file: fi,
                    class_fqn: None,
                    origins: &scope.effect_origins,
                    params: &scope.params,
                });
            }
        }
    }

    let mut rows: HashMap<Sym, EffectOwnRow> = HashMap::new();
    for unit in &ulist {
        let cx = Cx::new(units, index, unit.file);
        let row = rows.entry(unit.sym.clone()).or_insert_with(EffectOwnRow::new);
        classify_effect_origins(
            &cx,
            unit.class_fqn.as_deref(),
            unit.params,
            unit.origins,
            plugins,
            policy,
            row,
        );
    }
    (ulist.into_iter().map(|u| u.sym).collect(), rows)
}

/// Fixpoint: effects(u) = own(u) ∪ ⋃ effects(callees); exhaustive taints, from
/// the complete own rows. The declared lane rides the same edges, monotone in
/// the same way (ADR-0067): declared(u) = locally-imported bounds(u) ∪
/// ⋃ declared(callees). A declared label never crosses into `findings`, in
/// either direction. Monotone and order-independent (ADR-0048 §4); the rows
/// are read-only — propagated state never flows back into an own row.
fn propagate_effects(
    syms: &[Sym],
    rows: &HashMap<Sym, EffectOwnRow>,
    policy: &EffectsPolicy,
) -> HashMap<Sym, EffectSet> {
    // Each unit's own attribution (ADR-0084 §1), resolved once against the policy.
    // A closure is unnamed and so unattributable — the config has no key that could
    // reach one, which is why the table is keyed by [`Sym`] rather than consulted
    // per edge.
    let mut attribution: HashMap<Sym, Vec<String>> = HashMap::new();
    if !policy.is_empty() {
        for sym in syms {
            let labels = match sym {
                Sym::Func(fqn) => policy.function_attribution(fqn).to_vec(),
                Sym::Method(class, method) => policy.method_attribution(class, method),
                Sym::Closure(..) => Vec::new(),
            };
            if !labels.is_empty() {
                attribution.insert(sym.clone(), labels);
            }
        }
    }
    let mut findings: HashMap<Sym, HashSet<EffectFinding>> =
        rows.iter().map(|(s, r)| (s.clone(), r.findings.clone())).collect();
    let mut declared: HashMap<Sym, HashSet<String>> =
        rows.iter().map(|(s, r)| (s.clone(), r.declared.clone())).collect();
    let mut exhaustive: HashMap<Sym, bool> =
        rows.iter().map(|(s, r)| (s.clone(), r.exhaustive)).collect();
    loop {
        let mut changed = false;
        for sym in syms {
            let Some(row) = rows.get(sym) else { continue };
            let mut incoming: Vec<EffectFinding> = Vec::new();
            let mut incoming_declared: Vec<String> = Vec::new();
            let mut callee_taint = false;
            // Contract-discharged callees (ADR-0063 §2 decision 2): their proven
            // findings still join, their unknown remainder does not.
            for c in row.edges.iter().chain(row.untainting.iter()) {
                if let Some(ce) = findings.get(c) {
                    // Crossing out of an attributed callee stamps the copies with
                    // that callee's labels (ADR-0084 §2). The originals stay where
                    // they are: nothing is removed, nothing is rewritten, and the
                    // label/origin/line/path of every copy is byte-identical.
                    match attribution.get(c) {
                        Some(labels) => incoming.extend(ce.iter().map(|f| f.attributed_by(labels))),
                        None => incoming.extend(ce.iter().cloned()),
                    }
                }
                if let Some(cd) = declared.get(c) {
                    incoming_declared.extend(cd.iter().cloned());
                }
            }
            for c in &row.edges {
                if exhaustive.get(c).copied() == Some(false) {
                    callee_taint = true;
                }
            }
            let set = findings.entry(sym.clone()).or_default();
            for ef in incoming {
                changed |= set.insert(ef);
            }
            let dset = declared.entry(sym.clone()).or_default();
            for label in incoming_declared {
                changed |= dset.insert(label);
            }
            if callee_taint && exhaustive.get(sym).copied() != Some(false) {
                exhaustive.insert(sym.clone(), false);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    syms.iter()
        .map(|s| {
            let f = findings.remove(s).unwrap_or_default();
            let dc = declared.remove(s).unwrap_or_default();
            let ex = exhaustive.get(s).copied().unwrap_or(true);
            let at = attribution.remove(s).unwrap_or_default();
            (s.clone(), EffectSet { findings: f, declared: dc, exhaustive: ex, attribution: at })
        })
        .collect()
}

/// Classify one unit's (or one **region**'s — ADR-0076) effect origins into its
/// [`EffectOwnRow`]. Split out of
/// [`compute_effects`] so a *sub-span* of a body can be asked the same question
/// the whole body is asked, through exactly the same code: the loop→`array_map`
/// transform's purity precondition is the fixpoint's own verdict restricted to
/// the loop body, never a second opinion about what an effect is.
fn classify_effect_origins(
    cx: &Cx,
    class_fqn: Option<&str>,
    params: &[steins_syntax::Param],
    origins: &[EffectOrigin],
    plugins: &PluginFacts,
    policy: &EffectsPolicy,
    row: &mut EffectOwnRow,
) {
    for origin in origins {
        match origin {
            EffectOrigin::Call { name, span, arg_targets, const_args } => {
                let targets = arg_targets.as_deref();
                match cx.resolve_effect_function(name) {
                    FnResolution::User(site) => {
                        let decl = cx.fn_decl(site);
                        let sym = Sym::Func(decl.fqn.clone());
                        match conditional_purity(decl.docblock.as_ref(), &decl.params) {
                            Some(cp) => {
                                let r = eval_conditional_purity(&cp, &[], targets, |cbref| {
                                    add_callback_effects(cx, cbref, *span, policy, row);
                                });
                                // A userland out-param row is produced at this
                                // call site but is the CALLEE's contract, so it
                                // carries the callee's attribution — the same
                                // reasoning that stamps a builtin's findings,
                                // for the same reason: no edge carries these.
                                let attributed = policy.function_attribution(&decl.fqn);
                                for label in r.labels {
                                    row.findings.insert(
                                        EffectFinding::direct(
                                            label.to_owned(),
                                            name.simple().to_owned(),
                                            cx.tree().position(span.start).line,
                                            cx.path().to_owned(),
                                        )
                                        .attributed_by(attributed),
                                    );
                                }
                                if r.discharge_taint {
                                    row.untainting.insert(sym);
                                } else {
                                    row.edges.insert(sym);
                                }
                            }
                            None => {
                                row.edges.insert(sym);
                            }
                        }
                    }
                    FnResolution::Builtin(builtin_name) => {
                        for f in builtin_findings(
                            &builtin_name,
                            *span,
                            cx.tree(),
                            cx.path(),
                            targets,
                            Some(const_args),
                            policy,
                        ) {
                            row.findings.insert(f);
                        }
                    }
                    // Ambiguous / unresolved: effects unknown → non-exhaustive.
                    // The plugin channel gets the last word here and nowhere
                    // else (ADR-0068 precedence): a project body and a catalog
                    // row are both already spoken for above.
                    FnResolution::Unknown => {
                        if let Some(labels) = plugin_call_labels(cx, plugins, name) {
                            row.declared.extend(labels.iter().cloned());
                        }
                        row.exhaustive = false;
                    }
                }
            }
            EffectOrigin::Output { keyword, span } => {
                row.findings.insert(EffectFinding::direct(
                    "io.output.buffer".to_owned(),
                    (*keyword).to_owned(),
                    cx.tree().position(span.start).line,
                    cx.path().to_owned(),
                ));
            }
            EffectOrigin::Exit { keyword, span } => {
                row.findings.insert(EffectFinding::direct(
                    "exit".to_owned(),
                    (*keyword).to_owned(),
                    cx.tree().position(span.start).line,
                    cx.path().to_owned(),
                ));
            }
            EffectOrigin::MethodCall { receiver, method, span } => {
                match resolve_effect_edge(cx, class_fqn, receiver, method) {
                    Some(callee) => {
                        row.edges.insert(callee);
                    }
                    // No project edge — the builtin-class catalog gets its say
                    // (`new PDO(...)->query()` is `io.db`), and failing that the
                    // receiver may still carry a *declared* bound: an interface
                    // envelope caps what the call can do even when no body is
                    // resolvable (ADR-0067). Importing it discharges **this**
                    // call site's taint and nothing else — another unresolved
                    // call in the same body still marks the summary `…?`. An
                    // uncatalogued, undeclared receiver stays the taint it has
                    // always been.
                    //
                    // The two legs cannot both fire: `builtin_method_findings`
                    // answers only for `EffectRecv::ClassName` (a catalogued
                    // external class), `resolve_declared_bound` only for the
                    // declared receivers, which name no class here.
                    None => match builtin_method_findings(cx, receiver, method, *span, policy) {
                        Some(fs) => {
                            for f in fs {
                                row.findings.insert(f);
                            }
                        }
                        None => match resolve_declared_bound(
                            cx,
                            plugins.registry(),
                            class_fqn,
                            params,
                            receiver,
                            method,
                        ) {
                            // A checked envelope answers this call site outright.
                            Some(DeclaredBound::Checked(labels)) => row.declared.extend(labels),
                            // An interop envelope (ADR-0082) contributes its bound
                            // and keeps the taint: ADR-0068's plugin discipline,
                            // applied to the unchecked stratum. An empty bound
                            // (`@phpstan-pure`) adds no label and still claims no
                            // exhaustiveness — the summary reads "≤ this, and
                            // possibly more", which is the truth of a claim
                            // nothing here has verified.
                            Some(DeclaredBound::Interop(labels)) => {
                                row.declared.extend(labels);
                                row.exhaustive = false;
                            }
                            None => row.exhaustive = false,
                        },
                    },
                }
            }
            // A higher-order call: the callback's effects join the caller's, or
            // the base call resolves normally for a non-invoker callee (ADR-0033).
            EffectOrigin::HigherOrder {
                callee,
                callbacks,
                arg_count,
                arg_targets,
                const_args,
                span,
            } => {
                let targets = Some(arg_targets.as_slice());
                match cx.resolve_invoker_function(callee) {
                    FnResolution::Builtin(builtin_name) => {
                        let shape = steins_catalog::invocation_shape(&builtin_name)
                            .expect("resolve_invoker_function's catalog_knows guarantees a shape row");
                        // ADR-0063 P1: the call's effect is the invoker's OWN
                        // catalog color ⊔ the envelope of the callback it
                        // immediately invokes. The own-color leg runs first and
                        // unconditionally — an unresolvable (or absent) callback
                        // never *weakens* the invoker's declared color; it only
                        // adds the `…?` taint below. P2 is what puts anything in
                        // that leg for the sort family: `usort`'s own color is
                        // the by-ref write to its array argument.
                        for f in builtin_findings(
                            &builtin_name,
                            *span,
                            cx.tree(),
                            cx.path(),
                            targets,
                            Some(const_args),
                            policy,
                        ) {
                            row.findings.insert(f);
                        }
                        if shape.callback_param < *arg_count {
                            match callbacks.iter().find(|(p, _)| *p == shape.callback_param) {
                                Some((_, cbref)) => {
                                    add_callback_effects(cx, cbref, *span, policy, row);
                                }
                                // Callback slot filled by an unresolvable value.
                                None => row.exhaustive = false,
                            }
                        }
                    }
                    // Not a known invoker: the callee is a normal edge, unless it
                    // is a user function declaring a conditional-purity contract
                    // — a userland catalog row (ADR-0063 §2 decision 2).
                    FnResolution::User(_) | FnResolution::Unknown => match cx.resolve_effect_function(callee) {
                        FnResolution::User(site) => {
                            let decl = cx.fn_decl(site);
                            let sym = Sym::Func(decl.fqn.clone());
                            match conditional_purity(decl.docblock.as_ref(), &decl.params) {
                                Some(cp) => {
                                    let r = eval_conditional_purity(
                                        &cp,
                                        callbacks,
                                        targets,
                                        |cbref| add_callback_effects(cx, cbref, *span, policy, row),
                                    );
                                    let attributed = policy.function_attribution(&decl.fqn);
                                    for label in r.labels {
                                        row.findings.insert(
                                            EffectFinding::direct(
                                                label.to_owned(),
                                                callee.simple().to_owned(),
                                                cx.tree().position(span.start).line,
                                                cx.path().to_owned(),
                                            )
                                            .attributed_by(attributed),
                                        );
                                    }
                                    if r.discharge_taint {
                                        row.untainting.insert(sym);
                                    } else {
                                        row.edges.insert(sym);
                                    }
                                }
                                None => {
                                    row.edges.insert(sym);
                                }
                            }
                        }
                        FnResolution::Builtin(builtin_name) => {
                            for f in builtin_findings(
                                &builtin_name,
                                *span,
                                cx.tree(),
                                cx.path(),
                                targets,
                                Some(const_args),
                                policy,
                            ) {
                                row.findings.insert(f);
                            }
                        }
                        FnResolution::Unknown => {
                            if let Some(labels) = plugin_call_labels(cx, plugins, callee) {
                                row.declared.extend(labels.iter().cloned());
                            }
                            row.exhaustive = false;
                        }
                    },
                }
            }
            // A `$fn()` resolved to a body-local closure — its effects join.
            EffectOrigin::Callback { cbref, span } => {
                add_callback_effects(cx, cbref, *span, policy, row);
            }
            EffectOrigin::Opaque { .. } => row.exhaustive = false,
        }
    }
}

/// One line of the `annotate` effect margin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectSummary {
    pub symbol: String,
    /// The same function under its **namespace-qualified** name — `App\Checkout::confirm`
    /// for a method, `App\render_page` for a function — while [`Self::symbol`] stays the
    /// short display name the margin prints. Two declarations with the same short name in
    /// one file (one per namespace block) are legal PHP; a consumer that *keys* on a
    /// summary (the effect baseline of issue #69) needs the name that tells them apart.
    ///
    /// Casing follows the declaration for the simple name and the resolved class FQN;
    /// a function's namespace prefix is the index's lowercase-normalized one, since PHP
    /// folds namespace and function case anyway.
    pub qualified: String,
    pub line: u32,
    pub labels: Vec<String>,
    /// The **declared** effect labels (ADR-0067), sorted: bounds imported from an
    /// interface envelope at a call through an injected receiver, not effects
    /// inference proved. Rendered with a `≤` prefix; never a finding's input.
    ///
    /// Normalized for display: a declared label already covered by a proven label
    /// of this same summary is dropped, since the proven lane says strictly more.
    pub declared: Vec<String>,
    /// The subset of [`Self::labels`] the project's `[effects]` policy discharges
    /// **wholly** at this unit (ADR-0084 §4), sorted. Rendered with a `~` prefix.
    ///
    /// Wholly means every finding group carrying the label is discharged here; a
    /// label with one surviving group is absent, because the unit still answers
    /// for it. [`Self::labels`] is unaffected — the tolerance is a fact about the
    /// judgment, not about the proven lane, so a consumer reading only `labels`
    /// reads what it always did.
    ///
    /// The built-in `mutate.local` tolerance is never listed: the marker reports
    /// the *configured* policy at work, and no project configured that one.
    pub tolerated: Vec<String>,
    pub exhaustive: bool,
    /// The inferred escaping throw classes (ADR-0040), sorted; empty when none.
    pub throws: Vec<String>,
    /// Whether the throw set is exhaustive (no dynamic/unresolved taint).
    pub throws_exhaustive: bool,
}

/// The proven effect set of every concrete function/method in a single file
/// (ADR-0020 annotate margin). Analyzed as a one-file project.
#[must_use]
pub fn effect_summary(
    tree: &SourceTree,
    functions: &[FunctionDecl],
    classes: &[ClassDecl],
) -> Vec<EffectSummary> {
    let _ = (functions, classes);
    let lazy = LazyTree::borrowed(tree);
    let units = [FileUnit { path: "", tree: &lazy }];
    let index = Index::from_units(&units);
    effect_summary_units(&units, &index, 0, &PluginFacts::none(), &EffectsPolicy::none())
}

/// The proven effect set of every concrete function/method in the `target` file.
#[must_use]
pub(crate) fn effect_summary_units(
    units: &[FileUnit],
    index: &Index,
    target: usize,
    plugins: &PluginFacts,
    policy: &EffectsPolicy,
) -> Vec<EffectSummary> {
    let effects = compute_effects(units, index, plugins, policy);
    let throws = compute_throws(units, index);
    let tree = units[target].tree;
    let sorted_labels = |sym: &Sym| -> Vec<String> {
        let mut labels: Vec<String> = effects
            .get(sym)
            .into_iter()
            .flat_map(|e| e.findings.iter().map(|f| f.label.clone()))
            .collect();
        labels.sort();
        labels.dedup();
        labels
    };
    // The labels the policy discharges wholly at this unit (ADR-0084 §4). The
    // subset is read off the same [`finding_groups`] the judgment sites read, with
    // the same empty edge the purity oracle and the Liskov check pass, so the
    // margin and the verdict cannot disagree about one unit.
    //
    // `mutate.local` is excluded unless the project named it: the built-in
    // tolerance predates the policy and has never worn a marker, and the tilde
    // reports a *configured* judgment call.
    let tolerated_labels = |sym: &Sym| -> Vec<String> {
        let Some(e) = effects.get(sym) else { return Vec::new() };
        let mut discharged: BTreeSet<&str> = BTreeSet::new();
        let mut surviving: HashSet<&str> = HashSet::new();
        for (f, ok) in finding_groups(&e.findings, &[], policy) {
            if ok {
                discharged.insert(&f.label);
            } else {
                surviving.insert(&f.label);
            }
        }
        discharged
            .into_iter()
            .filter(|l| {
                !surviving.contains(l) && (!tolerated_by_every_envelope(l) || policy.tolerates(l))
            })
            .map(str::to_owned)
            .collect()
    };
    // The declared lane, normalized against this summary's own proven labels
    // (ADR-0067 rendering rule): `≤io.db` beside a proven `io` (or a proven
    // `io.db`) says nothing the proven lane has not already said, so it is dropped
    // from the display. The stored lanes keep their raw sets.
    let declared_labels = |sym: &Sym, proven: &[String]| -> Vec<String> {
        let mut labels: Vec<String> = effects
            .get(sym)
            .into_iter()
            .flat_map(|e| e.declared.iter().cloned())
            .filter(|l| !proven.iter().any(|p| steins_catalog::subsumes(p, l)))
            .collect();
        labels.sort();
        labels.dedup();
        labels
    };
    let exhaustive = |sym: &Sym| effects.get(sym).is_none_or(|e| e.exhaustive);
    // Escaping throw classes (Yes or Maybe escape) as compact simple names.
    let throw_classes = |sym: &Sym| -> Vec<String> {
        let mut cs: Vec<String> = throws
            .get(sym)
            .into_iter()
            .flat_map(|t| t.facts.keys().map(|f| last_segment(&f.class).to_owned()))
            .collect();
        cs.sort();
        cs.dedup();
        cs
    };
    let throws_exhaustive = |sym: &Sym| throws.get(sym).is_none_or(|t| t.exhaustive);

    // The namespace prefix of a function's index FQN (lowercase-normalized), rejoined
    // with the simple name as declared: `app\renderPage` rather than `app\renderpage`.
    // A global function has no prefix and reads exactly like its declaration.
    let qualify_func = |f: &FunctionDecl| -> String {
        match f.fqn.rsplit_once('\\') {
            Some((ns, _)) => format!("{ns}\\{}", f.name),
            None => f.name.clone(),
        }
    };

    let mut out = Vec::new();
    for f in tree.functions() {
        let sym = Sym::Func(f.fqn.clone());
        let labels = sorted_labels(&sym);
        let declared = declared_labels(&sym, &labels);
        out.push(EffectSummary {
            symbol: f.name.clone(),
            qualified: qualify_func(f),
            line: tree.position(f.span.start).line,
            labels,
            declared,
            tolerated: tolerated_labels(&sym),
            exhaustive: exhaustive(&sym),
            throws: throw_classes(&sym),
            throws_exhaustive: throws_exhaustive(&sym),
        });
    }
    for c in tree.classes() {
        // The resolved FQN with the source's casing, when the tree-build pass has
        // stamped it; the simple name is the pre-stamp (and global-namespace) reading.
        let class_display = if c.display.is_empty() { c.name.as_str() } else { c.display.as_str() };
        for m in &c.methods {
            if m.is_abstract {
                continue;
            }
            let sym = Sym::Method(c.fqn.clone(), m.name.clone());
            let labels = sorted_labels(&sym);
            let declared = declared_labels(&sym, &labels);
            out.push(EffectSummary {
                symbol: format!("{}::{}", c.name, m.name),
                qualified: format!("{class_display}::{}", m.name),
                line: tree.position(m.span.start).line,
                labels,
                declared,
                tolerated: tolerated_labels(&sym),
                exhaustive: exhaustive(&sym),
                throws: throw_classes(&sym),
                throws_exhaustive: throws_exhaustive(&sym),
            });
        }
    }
    out
}

/// What the effect and throw fixpoints prove about one **region** of source —
/// a byte span inside a function body (ADR-0076 §2). The purity precondition of
/// the loop→`array_map` transform is spelled entirely in these four fields.
///
/// The two lanes stay apart, exactly as ADR-0067 built them. [`Self::labels`] is
/// what inference **proved**; [`Self::declared`] is what a declaration merely
/// **bounds** — an envelope imported at an interface-typed receiver, or a plugin
/// coloring. A cap is not an occurrence proof, so a consumer needing "provably no
/// effects" must read a non-empty declared lane as *unproven*. Reported separately
/// rather than merged, so that reading is the consumer's explicit decision.
///
/// Carrying the declared lane is load-bearing: the effect pass deliberately
/// **discharges** the exhaustiveness taint at a call whose declared receiver
/// answered (ADR-0067 — a checked contract, so the call site is no longer
/// "unknown"), which would otherwise let a declared-only call through a
/// proven-purity gate reading [`Self::exhaustive`] alone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegionPurity {
    /// The proven effect labels arising inside the region, sorted and deduped.
    pub labels: Vec<String>,
    /// The **declared** effect labels bounding calls inside the region, sorted
    /// and deduped (ADR-0067). Never a proof; a non-empty set means some call was
    /// answered by a contract rather than by inference.
    pub declared: Vec<String>,
    /// Whether every call inside the region resolved. `false` means some callee
    /// is unanalyzable — the region *may* have effects nothing proved. A call
    /// answered by a declared envelope is *resolved* for this bit's purposes and
    /// shows up in [`Self::declared`] instead.
    pub exhaustive: bool,
    /// The throw classes (compact simple names) that would escape the region
    /// **with the enclosing `try`/`catch` guards stripped**, sorted and deduped.
    /// Stripping is the point: an enclosing `catch` is exactly the observer that
    /// can tell partial accumulation from an unassigned accumulator, so a body
    /// whose throw an outer `catch` absorbs is still ineligible (ADR-0076 §2.3).
    pub throws: Vec<String>,
    /// Whether the throw set is exhaustive (no dynamic / unresolved taint).
    pub throws_exhaustive: bool,
}

/// Ask the effect and throw fixpoints what they prove about each of `regions`
/// (ADR-0076 §2). Each region is a `(path, start, end)` byte span; the answer at
/// index `i` is the verdict for `regions[i]`.
///
/// The whole-project fixpoints run **once** for the batch, so a transform run
/// over a project pays for them once however many loops it enumerates.
///
/// An origin counts for a region when its span falls inside it, taken over every
/// effect/throw unit of the region's file — the enclosing function's own origins
/// plus those of any closure defined inside the region. Counting a closure that
/// is never invoked can only *refuse* a rewrite, never permit one, which is the
/// direction conservatism has to fall.
#[must_use]
pub fn region_purity_project(
    db: &dyn Db,
    project: Project,
    regions: &[(String, u32, u32)],
) -> Vec<RegionPurity> {
    if regions.is_empty() {
        return Vec::new();
    }
    let handles: Vec<SourceFile> = project.files(db).to_vec();
    // One `LazyTree` per file, borrowing the database's own parse: the salsa
    // path holds every tree already, so nothing here is ever deferred.
    let lazy: Vec<LazyTree<'_>> =
        handles.iter().map(|&f| LazyTree::borrowed(parse(db, f))).collect();
    let units: Vec<FileUnit> = handles
        .iter()
        .zip(&lazy)
        .map(|(&f, tree)| FileUnit { path: f.path(db), tree })
        .collect();
    let db_index = project_index(db, project);
    let pos: HashMap<SourceFile, usize> =
        handles.iter().enumerate().map(|(i, &f)| (f, i)).collect();
    let index = Index::from_db(db_index, &pos, &units);
    let plugins = project.plugins(db);
    let effects = compute_effects(&units, &index, plugins, project.effects(db));
    let throws = compute_throws(&units, &index);

    regions
        .iter()
        .map(|(path, start, end)| {
            let Some(fi) = units.iter().position(|u| u.path == path) else {
                return RegionPurity::default();
            };
            region_purity_in(
                &units,
                &index,
                plugins,
                project.effects(db),
                fi,
                (*start, *end),
                &effects,
                &throws,
            )
        })
        .collect()
}

/// The per-region half of [`region_purity_project`], against already-computed
/// fixpoints.
#[allow(clippy::too_many_arguments)]
fn region_purity_in(
    units: &[FileUnit],
    index: &Index,
    plugins: &PluginFacts,
    policy: &EffectsPolicy,
    file: usize,
    region: (u32, u32),
    effects: &HashMap<Sym, EffectSet>,
    throws: &HashMap<Sym, ThrowSet>,
) -> RegionPurity {
    let inside = |s: steins_syntax::Span| s.start >= region.0 && s.end <= region.1;
    let cx = Cx::new(units, index, file);
    let tree = units[file].tree;

    // Every effect/throw origin of this file that falls inside the region, kept
    // with the frame facts its classification needs (the enclosing class for a
    // `$this->`/`self::` edge, the parameter list for an ADR-0067 receiver type).
    // The region is classified into an own row exactly as a whole unit is
    // (issue #489) — the same value, restricted to a sub-span.
    let mut row = EffectOwnRow::new();
    let mut trow = ThrowOwnRow::new();

    let mut take = |class_fqn: Option<&str>,
                    params: &[steins_syntax::Param],
                    eo: &[EffectOrigin],
                    to: &[ThrowOrigin]| {
        let picked: Vec<EffectOrigin> =
            eo.iter().filter(|o| inside(effect_origin_span(o))).cloned().collect();
        classify_effect_origins(&cx, class_fqn, params, &picked, plugins, policy, &mut row);
        // The guards are dropped, not carried: this region's own body cannot
        // hold a `try` (a `try` is a statement, and the eligible body is one
        // append), so every guard on a picked origin is an ENCLOSING one — and
        // an enclosing `catch` is the observer that distinguishes the two
        // spellings, so it must not absorb anything here (ADR-0076 §2.3).
        let picked_throws: Vec<ThrowOrigin> = to
            .iter()
            .filter(|o| inside(o.span))
            .map(|o| ThrowOrigin { kind: o.kind.clone(), span: o.span, guards: Vec::new() })
            .collect();
        classify_throw_origins(&cx, class_fqn, &picked_throws, &mut trow);
    };

    for f in tree.functions() {
        take(None, &f.params, &f.effect_origins, &f.throw_origins);
    }
    for c in tree.classes() {
        for m in &c.methods {
            take(Some(&c.fqn), &m.params, &m.effect_origins, &m.throw_origins);
        }
    }
    for scope in tree.scopes() {
        take(None, &scope.params, &scope.effect_origins, &scope.throw_origins);
    }

    // Join the callees' fixpoint results — the region's transitive answer. Both
    // lanes ride the same edges, monotone in the same way, and never mix.
    let mut exhaustive = row.exhaustive;
    let mut labels: Vec<String> = row.findings.iter().map(|f| f.label.clone()).collect();
    let mut declared_labels: Vec<String> = row.declared.into_iter().collect();
    for callee in row.edges.iter().chain(row.untainting.iter()) {
        if let Some(set) = effects.get(callee) {
            labels.extend(set.findings.iter().map(|f| f.label.clone()));
            declared_labels.extend(set.declared.iter().cloned());
        }
    }
    for callee in &row.edges {
        if effects.get(callee).is_some_and(|s| !s.exhaustive) {
            exhaustive = false;
        }
    }
    labels.sort();
    labels.dedup();
    declared_labels.sort();
    declared_labels.dedup();

    let mut throws_exhaustive = trow.exhaustive;
    let mut classes: Vec<String> =
        trow.facts.keys().map(|f| last_segment(&f.class).to_owned()).collect();
    for (callee, _) in &trow.edges {
        if let Some(set) = throws.get(callee) {
            classes.extend(set.facts.keys().map(|f| last_segment(&f.class).to_owned()));
            if !set.exhaustive {
                throws_exhaustive = false;
            }
        }
    }
    classes.sort();
    classes.dedup();

    RegionPurity {
        labels,
        declared: declared_labels,
        exhaustive,
        throws: classes,
        throws_exhaustive,
    }
}

/// The source span of an [`EffectOrigin`], whatever its shape.
const fn effect_origin_span(o: &EffectOrigin) -> steins_syntax::Span {
    match o {
        EffectOrigin::Call { span, .. }
        | EffectOrigin::Output { span, .. }
        | EffectOrigin::Exit { span, .. }
        | EffectOrigin::MethodCall { span, .. }
        | EffectOrigin::Opaque { span }
        | EffectOrigin::HigherOrder { span, .. }
        | EffectOrigin::Callback { span, .. } => *span,
    }
}

/// The bridge between the effect fixpoint and the contract judgment (ADR-0063 P3).
///
/// The purity half of `pure-callable`/`pure-closure`/`static-pure-closure` asks one
/// question of a bound callable — "is its inferred effect envelope pure?" — and the
/// machinery that answers it ([`compute_effects`]) already exists, keyed by exactly
/// the [`Sym`] a `ClosureRef`/`ClosureTarget` names. What did not exist was a way to
/// ask it from a *call site*, since [`compute_effects`] ran only inside
/// [`effect_diagnostics`], a whole-project pass strictly **after** the
/// per-call-site loop. This type is just that connection — no effect semantics of
/// its own.
///
/// Purity is read against the same relation the envelope check uses, so the two
/// consumers cannot disagree: a label is disqualifying here exactly when
/// [`exceeds`] would report it against an empty envelope. In particular a closure
/// that `preg_match`es into one of its own locals satisfies `pure-callable` —
/// ADR-0063 §2.3's `mutate.local` tolerance — while the same closure writing
/// `$this->matches` does not.
pub(crate) struct PurityOracle<'a> {
    /// The run's shared effect fixpoint result, borrowed from the
    /// [`Fixpoints`] holder (issue #489) so the oracle and the envelope
    /// diagnostics read one computation rather than each running their own.
    effects: &'a HashMap<Sym, EffectSet>,
    /// The project's tolerated-effects policy (ADR-0084 §3), borrowed from the
    /// same holder — one lifetime already threads through [`Cx`], so the clone
    /// the previous owned form paid for is no longer buying anything.
    policy: &'a EffectsPolicy,
}

impl<'a> PurityOracle<'a> {
    /// Build the oracle, or `None` when no docblock in the project spells a
    /// purity-bearing callable. The fixpoint is a whole-project pass and
    /// [`effect_diagnostics`] already guards its own use of it the same way; without
    /// such a spelling nothing could consult the answer, so the work is pure cost.
    ///
    /// The gate is exact rather than merely cheap: an obligation can only reach a
    /// judgment by being *written*, and every purity-bearing spelling in the
    /// vocabulary (`pure-callable`, `pure-closure`, `static-pure-closure`) contains
    /// one of the two literal substrings tested.
    pub(crate) fn build(fx: &'a Fixpoints<'a>) -> Option<Self> {
        let spells_purity = |doc: Option<&String>| {
            doc.is_some_and(|t| t.contains("pure-callable") || t.contains("pure-closure"))
        };
        let any = fx.units().iter().any(|u| {
            u.tree.functions().iter().any(|f| spells_purity(f.docblock.as_ref()))
                || u.tree
                    .classes()
                    .iter()
                    .any(|c| c.methods.iter().any(|m| spells_purity(m.docblock.as_ref())))
        });
        if !any {
            return None;
        }
        Some(PurityOracle { effects: fx.effects(), policy: fx.policy() })
    }

    /// Whether `sym`'s inferred effect envelope is **provably** not pure: the
    /// fixpoint proved at least one effect finding for it.
    ///
    /// Deliberately one-sided. An unknown symbol answers `false`, and so does a
    /// symbol whose proven finding set is empty but whose `exhaustive` bit is off
    /// (an unresolved callee somewhere below it) — "not proven impure" is the only
    /// answer that can never manufacture a finding. Non-exhaustiveness can hide an
    /// effect, never invent one, so a *non-empty* finding set is a definite verdict
    /// regardless of it.
    ///
    /// Discharged findings ([`finding_groups`]) do not count: they are proven, but
    /// they are not impurity — for the built-in `mutate.local` tolerance and for
    /// the project's own policy alike. Reading the same rule the envelope check
    /// reads is the point (ADR-0084 §3): otherwise a purity query and an envelope
    /// judgment would disagree about one function.
    ///
    /// The symbol's own attribution is deliberately not folded in. This asks what
    /// `sym` *is*, exactly as `report_unit` judges a unit against its own bound;
    /// attribution answers how an effect reached a **caller**, and there is no
    /// caller here.
    pub(crate) fn provably_impure(&self, sym: &Sym) -> bool {
        self.effects.get(sym).is_some_and(|e| {
            finding_groups(&e.findings, &[], self.policy).iter().any(|&(_, d)| !d)
        })
    }

    /// Every symbol this oracle answers [`Self::provably_impure`] for, spelled
    /// canonically and sorted — the oracle's *whole* answer surface, since
    /// `provably_impure` is the only question it takes.
    ///
    /// The generation planner (issue #489 slice B) digests this to decide
    /// whether the oracle moved between generations; the walk of any file may
    /// consult any symbol, so nothing narrower would be sound. Paid only when
    /// the oracle exists at all — i.e. when some docblock spells a
    /// purity-bearing callable — and it reads the fixpoint the oracle already
    /// borrowed, forcing nothing new.
    pub(crate) fn impurity_answers(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .effects
            .keys()
            .filter(|sym| self.provably_impure(sym))
            .map(|sym| format!("{sym:?}"))
            .collect();
        out.sort_unstable();
        out
    }
}

/// Effect-envelope diagnostics for the whole project (proven violations only).
pub(crate) fn effect_diagnostics(fx: &Fixpoints<'_>) -> Vec<Diagnostic> {
    let (units, index) = (fx.units(), fx.index());
    let (plugins, policy) = (fx.plugins(), fx.policy());
    // Fast path: no envelope anywhere → nothing to check. Interop envelopes
    // (ADR-0082 role B) are checked here too, so the gate admits their carriers
    // through [`docblock_envelope_tag`]'s own substring test — over-approximate
    // by design (a docblock merely *saying* "pure" opens the gate and then
    // resolves to no envelope), which costs a pass and can never lose a finding.
    let any_envelope = units.iter().any(|u| {
        u.tree
            .functions()
            .iter()
            .any(|f| f.effect_envelope.is_some() || spells_interop_envelope(f.docblock.as_ref()))
            || u.tree.classes().iter().any(|c| {
                spells_interop_envelope(c.docblock.as_ref())
                    || c.methods.iter().any(|m| {
                        m.effect_envelope.is_some()
                            || spells_interop_envelope(m.docblock.as_ref())
                    })
            })
    });
    if !any_envelope {
        return Vec::new();
    }

    let effects = fx.effects();
    // The registry this project's declared labels are judged against (ADR-0068):
    // builtin taxonomy plus whatever the plugin channel registered.
    let registry = plugins.registry();
    let mut out = Vec::new();
    for fi in 0..units.len() {
        let cx = Cx::new(units, index, fi);
        for f in cx.tree().functions() {
            // The docblock is read only when no attribute is written — the other
            // half of ADR-0082 §1's shadowing, and the half `operative_bound`
            // cannot enforce. A free function reads its *own* docblock: there is
            // no class-like for `@phpstan-all-methods-*` to distribute from (§5).
            // Both consumers of that docblock stand behind the same gate: its
            // vocabulary (issue #311) and its bound.
            if f.effect_envelope.is_none() {
                report_interop_vocabulary(
                    &mut out,
                    &cx,
                    &format!("{}()", f.name),
                    f.docblock.as_ref(),
                    own_tag,
                    f.span,
                    registry,
                );
            }
            let interop = f
                .effect_envelope
                .is_none()
                .then(|| own_interop_envelope(registry, f.docblock.as_ref()).into_bound())
                .flatten();
            let Some(bound) =
                operative_bound(f.effect_envelope.as_ref(), interop.as_ref(), f.span, policy)
            else {
                continue;
            };
            report_unit(&mut out, &cx, None, &f.name, bound, &f.effect_origins, effects, registry);
        }
        for c in cx.tree().classes() {
            // The class-level tag is one declaration, so its vocabulary is judged
            // once here rather than once per covered method. Nothing about a
            // method's own attribute shadows it: `@phpstan-all-methods-impure
            // io.netw` is a claim the class wrote, and it went ⊤ whoever reads it.
            report_interop_vocabulary(
                &mut out,
                &cx,
                &format!("class {}", c.name),
                c.docblock.as_ref(),
                class_tag,
                c.span,
                registry,
            );
            for m in &c.methods {
                // Judged only when the docblock is CONSULTED: an attribute envelope
                // shadows it outright (ADR-0082 §1), and a bound nobody read cannot
                // have misled anybody. The class-level tag above is a separate
                // declaration and keeps its own report.
                if m.effect_envelope.is_none() {
                    report_interop_vocabulary(
                        &mut out,
                        &cx,
                        &format!("{}::{}()", c.name, m.name),
                        m.docblock.as_ref(),
                        own_tag,
                        m.span,
                        registry,
                    );
                }
                let interop = m
                    .effect_envelope
                    .is_none()
                    .then(|| interop_envelope(registry, cx.tree(), c, m).into_bound())
                    .flatten();
                if let Some(bound) =
                    operative_bound(m.effect_envelope.as_ref(), interop.as_ref(), m.span, policy)
                {
                    let display = format!("{}::{}", c.name, m.name);
                    report_unit(
                        &mut out,
                        &cx,
                        Some(&c.fqn),
                        &display,
                        bound,
                        &m.effect_origins,
                        effects,
                        registry,
                    );
                }
                // Liskov (ADR-0033 point 5): a concrete implementation whose PROVEN
                // effects exceed an abstraction's effect envelope. Interfaces carry
                // no bodies, so only concrete class methods are judged.
                //
                // Interop envelopes deliberately do NOT participate: within that
                // stratum upstream's nearest-wins override is the whole contract,
                // and there is no conjunction rule to widen (ADR-0082 §5). So
                // `collect_abstraction_effects` keeps reading `effect_envelope`
                // only, and an interop-declared abstraction never yields
                // `effect.liskov-widened`.
                if !c.is_interface && !m.is_abstract {
                    emit_effect_liskov(&mut out, &cx, c, m, effects, policy);
                }
            }
        }
    }
    out
}

/// Emit `effect.liskov-widened` when a concrete method's PROVEN inferred effects
/// exceed the effect envelope declared on an abstraction it overrides/implements
/// (a parent class or interface method — ADR-0033 point 5). Only the proven part
/// judges: the exhaustiveness-tainted (unknown) remainder stays silent.
fn emit_effect_liskov(
    out: &mut Vec<Diagnostic>,
    cx: &Cx,
    class: &ClassDecl,
    m: &MethodDecl,
    effects: &HashMap<Sym, EffectSet>,
    policy: &EffectsPolicy,
) {
    let abstractions = collect_abstraction_effects(cx, class, &m.name);
    if abstractions.is_empty() {
        return;
    }
    let sym = Sym::Method(class.fqn.clone(), m.name.clone());
    let Some(set) = effects.get(&sym) else { return };
    // The impl's proven effect labels (deduplicated, sorted for stable output),
    // less whatever the policy discharges. The subtraction is on the PROVEN side;
    // the conjunction over abstractions below is on the declared side, and the two
    // do not interact (ADR-0084 §3).
    let mut proven: Vec<&str> = finding_groups(&set.findings, &[], policy)
        .into_iter()
        .filter(|(_, discharged)| !discharged)
        .map(|(f, _)| f.label.as_str())
        .collect();
    proven.sort_unstable();
    proven.dedup();
    if proven.is_empty() {
        return;
    }
    for (abs_display, labels) in abstractions {
        for label in &proven {
            if !exceeds(&labels, label, policy) {
                continue; // within the abstraction's envelope (purer OK)
            }
            let clause = if labels.is_empty() {
                "#[\\Steins\\Pure]".to_owned()
            } else {
                let quoted: Vec<String> = labels.iter().map(|l| format!("'{l}'")).collect();
                format!("#[\\Steins\\Effect({})]", quoted.join(", "))
            };
            let pos = cx.tree().position(m.span.start);
            let msg = format!(
                "{}::{}() has proven effect {label} but {abs_display}::{}() (its abstraction) is declared {clause} — Liskov effect widening",
                class.name, m.name, m.name
            );
            out.push(Diagnostic {
                id: EFFECT_LISKOV_ID,
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

/// Every abstraction carrier of `method` with a declared effect envelope: the
/// nearest parent CLASS declaring it, plus each interface the class
/// implements/extends (transitively) declaring it — `(display, envelope labels)`.
fn collect_abstraction_effects(cx: &Cx, class: &ClassDecl, method: &str) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    // Nearest parent class with an effect envelope on this method.
    if let Some((display, labels)) = nearest_parent_effect(cx, class, method) {
        out.push((display, labels));
    }
    // Implemented/extended interfaces declaring the method with an envelope.
    for (display, _file, im) in interface_abstraction_methods(cx, class, method) {
        if let Some(env) = &im.effect_envelope {
            out.push((display, env.labels.clone()));
        }
    }
    out
}

/// The nearest ancestor CLASS (walking `extends`, non-interfaces) declaring
/// `method` with an effect envelope — `(class name, envelope labels)`.
fn nearest_parent_effect(cx: &Cx, class: &ClassDecl, method: &str) -> Option<(String, Vec<String>)> {
    let mut cur = class.parent.as_ref().map(|p| cx.class_fqn(p))?;
    let mut seen: HashSet<String> = HashSet::new();
    loop {
        if !seen.insert(cur.to_ascii_lowercase()) {
            return None;
        }
        let (file, cd) = cx.find_class(&cur)?;
        if cd.is_interface {
            return None;
        }
        if let Some(pm) = cd.methods.iter().find(|pm| pm.name.eq_ignore_ascii_case(method))
            && let Some(env) = &pm.effect_envelope
        {
            return Some((cd.name.clone(), env.labels.clone()));
        }
        cur = cx.units[file].tree.resolve_class_fqn(cd.parent.as_ref()?);
    }
}

/// **How the author spelled** the envelope a unit is checked against — the one
/// thing the diagnostics need beyond its labels.
///
/// A finding must quote back syntax the reader actually wrote: telling someone who
/// wrote `@phpstan-impure io.db` that their declaration is `#[\Steins\Effect('io.db')]`
/// names a line that does not exist in their file, and for a feature whose whole
/// point is reading upstream's tags that is exactly backwards. Message wording is
/// not contract (ADR-0023 — the *ids* are), so it varies with the source; the id,
/// the judgment and the anchor do not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnvelopeSpelling {
    /// The checked stratum: `#[\Steins\Pure]` / `#[\Steins\Effect(...)]`.
    Attribute,
    /// An interop envelope (ADR-0082), quoted back in the tag family that wrote
    /// it. The family, not the exact alias: `@pure`, `@psalm-pure` and
    /// `@phpstan-pure` are one bound, and the canonical `@phpstan-` spelling is
    /// the one the interop spec documents.
    Interop(EnvelopeTag),
}

impl EnvelopeSpelling {
    /// The bare tag/attribute name — what the `effect.unknown-label` message
    /// points the reader at.
    const fn tag_name(self) -> &'static str {
        match self {
            // The label-bearing attribute; `#[\Steins\Pure]` carries none, so an
            // unknown label can never have come from it.
            Self::Attribute => "#[\\Steins\\Effect]",
            Self::Interop(EnvelopeTag::Pure) => "@phpstan-pure",
            Self::Interop(EnvelopeTag::Impure) => "@phpstan-impure",
            Self::Interop(EnvelopeTag::AllMethodsPure) => "@phpstan-all-methods-pure",
            Self::Interop(EnvelopeTag::AllMethodsImpure) => "@phpstan-all-methods-impure",
        }
    }
}

/// The envelope a unit is **actually held to** (ADR-0082 §1): its labels, the
/// anchor a declaration-level finding points at, and the spelling the findings
/// quote back.
///
/// Passed as one value down the whole reporting chain so that no site can judge
/// against one envelope and name another.
#[derive(Debug, Clone, Copy)]
struct OperativeBound<'a> {
    /// The declared upper bound. Empty is the *pure* envelope — a real claim, not
    /// a missing one; the ⊤ (unconstrained) case never builds a bound at all.
    labels: &'a [String],
    /// Where a finding about the declaration itself lands: the attribute's own
    /// span, or — for an interop envelope, which lives in trivia the attribute
    /// path has no analogue for — the declaration's name.
    span: Span,
    spelling: EnvelopeSpelling,
    /// The project's tolerated-effects policy (ADR-0084 §3). It rides on the bound
    /// because discharge is a property of the **judgment**: every site that asks
    /// whether something exceeds this envelope must ask under the same policy, and
    /// carrying it here is what makes that structural rather than a convention six
    /// call sites happen to keep.
    policy: &'a EffectsPolicy,
}

impl OperativeBound<'_> {
    /// Whether an inferred label exceeds this bound. The relation is
    /// [`exceeds`] verbatim: the two strata differ in trust and in wording,
    /// never in what counts as a violation.
    fn exceeds(self, effect_label: &str) -> bool {
        exceeds(self.labels, effect_label, self.policy)
    }

    /// Whether a **freshly produced** finding must be reported against this
    /// bound: it exceeds the envelope and nothing discharges it.
    ///
    /// A production site is a one-member finding group by construction (a
    /// builtin draws no edge, so this call is the only way this label reached
    /// this origin at this line), so leg 2 collapses from `every member` to
    /// this copy's own attribution — [`finding_groups`]'s answer for a
    /// singleton group with no edge, computed through the same
    /// [`attribution_tolerated`] predicate so the two sites cannot disagree.
    fn reports(self, f: &EffectFinding) -> bool {
        self.exceeds(&f.label) && !attribution_tolerated(f, self.policy)
    }

    /// How `effect.envelope-exceeded` quotes the declaration back, in the
    /// author's own syntax.
    fn declared_clause(self, exceeding_label: &str) -> String {
        let tag = self.spelling.tag_name();
        match self.spelling {
            EnvelopeSpelling::Attribute if self.labels.is_empty() => "#[\\Steins\\Pure]".to_owned(),
            EnvelopeSpelling::Attribute => {
                let quoted: Vec<String> = self.labels.iter().map(|l| format!("'{l}'")).collect();
                format!(
                    "#[\\Steins\\Effect({})] — {exceeding_label} exceeds the envelope",
                    quoted.join(", ")
                )
            }
            // The pure tags take no labels, so the tag name is the whole bound.
            EnvelopeSpelling::Interop(_) if self.labels.is_empty() => tag.to_owned(),
            // The label list is written in the tag's own grammar (ADR-0082 §4):
            // comma-separated dot-paths, unquoted.
            EnvelopeSpelling::Interop(_) => format!(
                "{tag} {} — {exceeding_label} exceeds the envelope",
                self.labels.join(", ")
            ),
        }
    }
}

/// Emit the diagnostics for one declared-envelope unit (ADR-0005/0018).
#[allow(clippy::too_many_arguments)]
fn report_unit(
    out: &mut Vec<Diagnostic>,
    cx: &Cx,
    class_fqn: Option<&str>,
    display: &str,
    bound: OperativeBound<'_>,
    origins: &[EffectOrigin],
    effects: &HashMap<Sym, EffectSet>,
    registry: &steins_catalog::LabelRegistry,
) {
    // 1. Unknown declared labels (one diagnostic each, at the bound's anchor).
    //
    // Reachable from the **attribute** stratum only, and by construction: an
    // interop tag naming a label this registry does not know never becomes a bound
    // in the first place ([`interop_tag`], owner ruling 2026-08-12), so it arrives
    // here with every label known and the loop body never runs for it. Typos in
    // upstream's tags are somebody else's rule; typos in a Steins attribute are
    // this one's, unchanged.
    for label in bound.labels {
        if registry.is_known(label) {
            continue;
        }
        // A retirement outranks the edit-distance suggestion and reaches where it
        // cannot: `output` → `io.output` is distance 3, past the cap, so before the
        // table this message ended at the label name and left an ADR-0083 migration
        // with nowhere to go (issue #311). Only the wording changes here — the id,
        // the layer, the floor and the firing condition are untouched.
        let suggestion = steins_catalog::retired_label(label)
            .map(|r| format!(" — {}", retirement_clause(r)))
            .or_else(|| registry.nearest(label).map(|s| format!(" — did you mean '{s}'?")))
            .unwrap_or_default();
        let msg = format!(
            "unknown effect label '{label}' in {} on {display}(){suggestion}",
            bound.spelling.tag_name()
        );
        let pos = cx.tree().position(bound.span.start);
        out.push(Diagnostic {
            id: UNKNOWN_LABEL_ID,
            path: cx.path().to_owned(),
            line: pos.line,
            column: pos.column,
            message: msg,
            facet: None,
            fix: None,
        });
    }

    // 2. Envelope-exceeded violations.
    for origin in origins {
        match origin {
            EffectOrigin::Call { name, span, arg_targets, const_args } => {
                let targets = arg_targets.as_deref();
                match cx.resolve_effect_function(name) {
                    FnResolution::User(site) => {
                        let decl = cx.fn_decl(site);
                        let callee = Sym::Func(decl.fqn.clone());
                        emit_transitive(out, cx, &callee, effects, span.start, display, bound);
                        // The `@pure-unless-parameter-passed` leg: a userland
                        // out-param row, reported like the catalog's.
                        report_conditional_purity(
                            out, cx, decl, &[], targets, effects, *span, display, bound,
                        );
                    }
                    FnResolution::Builtin(builtin_name) => {
                        for f in builtin_findings(
                            &builtin_name,
                            *span,
                            cx.tree(),
                            cx.path(),
                            targets,
                            Some(const_args),
                            bound.policy,
                        ) {
                            if bound.reports(&f) {
                                let prefix = format!("{}() has effect {}", name.simple(), f.label);
                                out.push(exceeded_diag(
                                    cx, span.start, &prefix, display, bound, &f.label,
                                ));
                            }
                        }
                    }
                    FnResolution::Unknown => {}
                }
            }
            EffectOrigin::Output { keyword, span } if bound.exceeds("io.output.buffer") => {
                let prefix = format!("{keyword} has effect io.output.buffer");
                out.push(exceeded_diag(cx, span.start, &prefix, display, bound, "io.output.buffer"));
            }
            EffectOrigin::Exit { keyword, span } if bound.exceeds("exit") => {
                let prefix = format!("{keyword} has effect exit");
                out.push(exceeded_diag(cx, span.start, &prefix, display, bound, "exit"));
            }
            EffectOrigin::MethodCall { receiver, method, span } => {
                if let Some(callee) = resolve_effect_edge(cx, class_fqn, receiver, method) {
                    emit_transitive(out, cx, &callee, effects, span.start, display, bound);
                // There is deliberately no declared-lane leg here: a declared bound
                // is not a proven effect, and this function only reports proven
                // ones (ADR-0067 decision 5). An ADR-0067 receiver reaches neither
                // arm and so reports nothing, which is the whole point.
                } else if let Some(fs) =
                    builtin_method_findings(cx, receiver, method, *span, bound.policy)
                {
                    // A builtin-class catalog row, reported like a builtin call's.
                    for f in fs {
                        if bound.reports(&f) {
                            let prefix = format!("{}() has effect {}", f.origin, f.label);
                            out.push(exceeded_diag(
                                cx, span.start, &prefix, display, bound, &f.label,
                            ));
                        }
                    }
                }
            }
            // A higher-order call (the array_map redemption): a resolvable callback
            // at the shape's callback param contributes its effects with the
            // callback's own origin in the provenance (ADR-0033). A non-invoker
            // callee resolves as a normal edge.
            EffectOrigin::HigherOrder {
                callee,
                callbacks,
                arg_count,
                arg_targets,
                const_args,
                span,
            } => {
                let targets = Some(arg_targets.as_slice());
                match cx.resolve_invoker_function(callee) {
                    FnResolution::Builtin(builtin_name) => {
                        let shape = steins_catalog::invocation_shape(&builtin_name)
                            .expect("resolve_invoker_function's catalog_knows guarantees a shape row");
                        // ADR-0063 P1 own-color leg, mirroring `compute_effects`:
                        // the invoker's own catalog color is reported whether or not
                        // the callback at the shape's position resolves.
                        for f in builtin_findings(
                            &builtin_name,
                            *span,
                            cx.tree(),
                            cx.path(),
                            targets,
                            Some(const_args),
                            bound.policy,
                        ) {
                            if bound.reports(&f) {
                                let prefix = format!("{}() has effect {}", callee.simple(), f.label);
                                out.push(exceeded_diag(cx, span.start, &prefix, display, bound, &f.label));
                            }
                        }
                        if shape.callback_param < *arg_count
                            && let Some((_, cbref)) =
                                callbacks.iter().find(|(p, _)| *p == shape.callback_param)
                        {
                            report_callback(out, cx, cbref, effects, span.start, display, bound);
                        }
                    }
                    FnResolution::User(_) | FnResolution::Unknown => {
                        if let FnResolution::User(site) = cx.resolve_effect_function(callee) {
                            let decl = cx.fn_decl(site);
                            let cs = Sym::Func(decl.fqn.clone());
                            emit_transitive(out, cx, &cs, effects, span.start, display, bound);
                            report_conditional_purity(
                                out, cx, decl, callbacks, targets, effects, *span, display, bound,
                            );
                        } else if let FnResolution::Builtin(builtin_name) =
                            cx.resolve_effect_function(callee)
                        {
                            for f in builtin_findings(
                                &builtin_name,
                                *span,
                                cx.tree(),
                                cx.path(),
                                targets,
                                Some(const_args),
                                bound.policy,
                            ) {
                                if bound.reports(&f) {
                                    let prefix = format!("{}() has effect {}", callee.simple(), f.label);
                                    out.push(exceeded_diag(cx, span.start, &prefix, display, bound, &f.label));
                                }
                            }
                        }
                    }
                }
            }
            // A `$fn()` resolved to a body-local closure — report its effects.
            EffectOrigin::Callback { cbref, span } => {
                report_callback(out, cx, cbref, effects, span.start, display, bound);
            }
            EffectOrigin::Output { .. } | EffectOrigin::Exit { .. } => {}
            EffectOrigin::Opaque { .. } => {}
        }
    }
}

/// Emit the envelope-exceeded violations a callee's **conditional-purity**
/// contracts produce at this call site (ADR-0063 §2 decision 2), mirroring the
/// `compute_effects` arm: the bound callables' effects for
/// `@pure-unless-callable-is-impure`, the by-ref color for
/// `@pure-unless-parameter-passed`.
#[expect(clippy::too_many_arguments, reason = "mirrors report_unit's own parameter set")]
fn report_conditional_purity(
    out: &mut Vec<Diagnostic>,
    cx: &Cx,
    decl: &FunctionDecl,
    callbacks: &[(usize, steins_syntax::CallbackRef)],
    arg_targets: Option<&[steins_syntax::RefTarget]>,
    effects: &HashMap<Sym, EffectSet>,
    span: steins_syntax::Span,
    display: &str,
    bound: OperativeBound<'_>,
) {
    let Some(cp) = conditional_purity(decl.docblock.as_ref(), &decl.params) else { return };
    let mut pending: Vec<steins_syntax::CallbackRef> = Vec::new();
    let r = eval_conditional_purity(&cp, callbacks, arg_targets, |cbref| {
        pending.push(cbref.clone());
    });
    for cbref in &pending {
        report_callback(out, cx, cbref, effects, span.start, display, bound);
    }
    // Mirrors the fixpoint arm: the row is the callee's contract, so the callee's
    // attribution decides it. A row is a one-member group (this call site is the
    // only way it arises), so leg 2 is the labels test directly.
    if any_tolerated(bound.policy.function_attribution(&decl.fqn), bound.policy) {
        return;
    }
    for label in r.labels {
        if bound.exceeds(label) {
            let prefix = format!("{}() has effect {label}", decl.name);
            out.push(exceeded_diag(cx, span.start, &prefix, display, bound, label));
        }
    }
}

/// Emit envelope-exceeded violations for a resolved callback (ADR-0033): a
/// closure/user callback's transitive effects, or a builtin callback's catalog
/// effect, each named with the callback in the provenance.
fn report_callback(
    out: &mut Vec<Diagnostic>,
    cx: &Cx,
    cbref: &steins_syntax::CallbackRef,
    effects: &HashMap<Sym, EffectSet>,
    offset: u32,
    display: &str,
    bound: OperativeBound<'_>,
) {
    if let Some(sym) = callback_effect_edge(cx, cbref) {
        emit_transitive(out, cx, &sym, effects, offset, display, bound);
    } else if let steins_syntax::CallbackRef::Named(name) = cbref
        && let FnResolution::Builtin(builtin_name) = cx.resolve_effect_function(name)
    {
        for f in builtin_findings(
            &builtin_name,
            steins_syntax::Span { start: offset, end: offset },
            cx.tree(),
            cx.path(),
            None,
            None,
            bound.policy,
        ) {
            if bound.reports(&f) {
                let prefix = format!("{}() has effect {}", name.simple(), f.label);
                out.push(exceeded_diag(cx, offset, &prefix, display, bound, &f.label));
            }
        }
    }
}

/// Emit each proven effect of `callee` not subsumed by the envelope as a
/// transitive violation, naming the ultimate origin.
fn emit_transitive(
    out: &mut Vec<Diagnostic>,
    cx: &Cx,
    callee: &Sym,
    effects: &HashMap<Sym, EffectSet>,
    offset: u32,
    display: &str,
    bound: OperativeBound<'_>,
) {
    let callee_display = cx.sym_display(callee);
    let Some(set) = effects.get(callee) else { return };
    // One diagnostic per finding GROUP, never one per attribution variant: the
    // copies that differ only in how the effect arrived are one effect at one
    // origin, and the reader has one thing to fix (ADR-0084 §2).
    for (ef, discharged) in finding_groups(&set.findings, &set.attribution, bound.policy) {
        if discharged || !bound.exceeds(&ef.label) {
            continue;
        }
        // Name the file when the ultimate origin arises in a different file than
        // the declared-envelope unit being reported (cross-file provenance).
        let loc = if ef.path == cx.path() {
            format!("line {}", ef.line)
        } else {
            format!("{} line {}", ef.path, ef.line)
        };
        let prefix =
            format!("{callee_display}() has effect {} (via {} at {loc})", ef.label, ef.origin);
        out.push(exceeded_diag(cx, offset, &prefix, display, bound, &ef.label));
    }
}

/// The by-ref-into-a-caller-local color (ADR-0063 §2.3).
const MUTATE_LOCAL: &str = "mutate.local";

/// Whether an effect label is tolerated by **every** envelope, `#[\Steins\Pure]`
/// included (ADR-0063 §2.3).
///
/// `mutate.local` is the only member and, by construction, the only one there can
/// be: it names a write whose target lives inside the calling frame, so no
/// observer outside that frame can distinguish a run where it happened from one
/// where it did not — an envelope constrains what a *caller* may observe, and a
/// label no caller can observe cannot exceed one.
///
/// The ADR states the tolerance for `Pure` specifically, but it is implemented
/// for every envelope: `Pure` is the *tightest* envelope in the lattice, and
/// tolerating a label there while rejecting it under a wider declaration would
/// make the check non-monotone.
fn tolerated_by_every_envelope(effect_label: &str) -> bool {
    effect_label == MUTATE_LOCAL
}

/// **Leg 1** of the ADR-0084 discharge rule: the label itself is tolerated, so
/// every finding carrying it is discharged wherever it arrived from.
///
/// The built-in `mutate.local` case is the degenerate member and stays here
/// rather than in config: it is a fact about the language (nothing outside the
/// frame can observe the write), not a judgment call any project gets to make.
/// The policy's own labels are the project's call, and only they are configurable.
fn tolerated_label(policy: &EffectsPolicy, effect_label: &str) -> bool {
    tolerated_by_every_envelope(effect_label) || policy.tolerates(effect_label)
}

/// Whether an inferred `effect_label` **exceeds** the declared `labels` under
/// `policy`.
fn exceeds(labels: &[String], effect_label: &str, policy: &EffectsPolicy) -> bool {
    if tolerated_label(policy, effect_label) {
        return false;
    }
    !labels.iter().any(|l| steins_catalog::subsumes(l, effect_label))
}

/// The identity of a finding **group** (ADR-0084 §2): everything a proven effect
/// says about itself and where it arose, with *how it arrived* left out. Copies
/// differing only in attribution are one group, and leg 2 quantifies over exactly
/// that group.
///
/// The field order is the report order [`emit_transitive`] has always sorted by,
/// with `path` added as a final tiebreaker — two findings agreeing on line, label
/// and origin used to come out in whatever order the hash set yielded them.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct FindingKey<'a> {
    line: u32,
    label: &'a str,
    origin: &'a str,
    path: &'a str,
}

/// One representative per finding group of `set`, paired with the group's
/// ADR-0084 discharge verdict, in report order.
///
/// `edge` is the attribution a caller accumulates as these findings cross out
/// of the unit that owns them: a judgment site reads the *callee's* stored
/// set, so the edge is folded in here rather than already present, matching
/// the fixpoint's own fold at copy time. Pass an empty slice to judge a unit's
/// own set as it stands (the purity oracle and the Liskov check both do).
///
/// A group is discharged iff its label is tolerated (leg 1), or **every**
/// member carries at least one attribution the policy tolerates (leg 2) — the
/// `all` is must-semantics: an effect reaching a declaration both through an
/// attributed facade and through a bare call is discharged for neither.
fn any_tolerated<'a>(
    labels: impl IntoIterator<Item = &'a String>,
    policy: &EffectsPolicy,
) -> bool {
    labels.into_iter().any(|a| policy.tolerates(a))
}

/// [`any_tolerated`] for a finding's own accumulated attribution.
fn attribution_tolerated(f: &EffectFinding, policy: &EffectsPolicy) -> bool {
    any_tolerated(&f.attributed, policy)
}

fn finding_groups<'f>(
    set: &'f HashSet<EffectFinding>,
    edge: &[String],
    policy: &EffectsPolicy,
) -> Vec<(&'f EffectFinding, bool)> {
    // The whole edge is one attribution act, so it is judged once rather than per
    // finding — and when it discharges, it discharges every copy that crosses it.
    let edge_tolerated = edge.iter().any(|a| policy.tolerates(a));
    let mut groups: BTreeMap<FindingKey<'f>, (&'f EffectFinding, bool)> = BTreeMap::new();
    for f in set {
        let key =
            FindingKey { line: f.line, label: &f.label, origin: &f.origin, path: &f.path };
        let attributed = edge_tolerated || attribution_tolerated(f, policy);
        groups
            .entry(key)
            .and_modify(|slot| slot.1 = slot.1 && attributed)
            .or_insert((f, attributed));
    }
    groups
        .into_values()
        .map(|(f, attributed)| (f, attributed || tolerated_label(policy, &f.label)))
        .collect()
}

/// Build an `effect.envelope-exceeded` diagnostic.
fn exceeded_diag(
    cx: &Cx,
    offset: u32,
    prefix: &str,
    display: &str,
    bound: OperativeBound<'_>,
    exceeding_label: &str,
) -> Diagnostic {
    let clause = bound.declared_clause(exceeding_label);
    let msg = format!("{prefix}, but {display}() is declared {clause}");
    let pos = cx.tree().position(offset);
    Diagnostic { id: EFFECT_ID, path: cx.path().to_owned(), line: pos.line, column: pos.column, message: msg, facet: None, fix: None }
}

/// The effect label a by-ref write through an argument with this lvalue root
/// carries (ADR-0063 §2.3) — the **target leg** of the conditional out-param row.
///
/// Three genuinely different contracts, so not a per-function flag:
/// `preg_match($p, $s, $m)` writes only the frame, `preg_match($p, $s,
/// $this->m)` mutates an object every caller shares, and `preg_match($p, $s,
/// $_SESSION['m'])` writes interpreter-global state.
///
/// Non-local targets stop at the conservative parent `mutate` rather than pick
/// an ADR-0055 child (`mutate.self`/`mutate.instance`/`mutate.static`): that
/// taxonomy's *inference* is not built, and a coarse-but-true label beats a
/// precise guess. Steins still distinguishes targets — property-rooted by-ref
/// writes never claim `mutate.local` — while declining to name the flavor.
fn by_ref_label(target: steins_syntax::RefTarget) -> &'static str {
    match target {
        steins_syntax::RefTarget::Local => MUTATE_LOCAL,
        steins_syntax::RefTarget::Superglobal => "global.write",
        steins_syntax::RefTarget::Escaping => "mutate",
    }
}

/// The by-ref out-parameter labels a call to `name` carries, given the classified
/// argument list (ADR-0063 §2.3). `arg_targets` is `None` when positional mapping
/// was defeated by a named/spread argument — every conditional judgment is then
/// withheld, because `preg_match(matches: $m, …)` and `preg_match($p, $s)` cannot
/// be told apart by position and a guess in either direction is a lie.
fn out_param_labels(name: &str, arg_targets: Option<&[steins_syntax::RefTarget]>) -> Vec<&'static str> {
    let (Some(positions), Some(targets)) = (steins_catalog::out_params(name), arg_targets) else {
        return Vec::new();
    };
    let mut labels: Vec<&'static str> = Vec::new();
    for &p in positions {
        // The arity leg: an argument that was not supplied is not written.
        let Some(&target) = targets.get(p) else { continue };
        let label = by_ref_label(target);
        if !labels.contains(&label) {
            labels.push(label);
        }
    }
    labels
}

/// One [`steins_syntax::CallTarget`] as the catalog spells it. The two crates
/// keep their own tiny enum on purpose: `steins-catalog` depends on nothing (it
/// is a body of knowledge about PHP, testable without a parser) and
/// `steins-syntax` is the Mago-lowering layer that knows no catalog, so the
/// translation lives here, in the crate that already depends on both.
fn stream_target(
    target: Option<&steins_syntax::CallTarget>,
) -> Option<steins_catalog::StreamTarget<'_>> {
    match target? {
        steins_syntax::CallTarget::Literal(s) => Some(steins_catalog::StreamTarget::Literal(s)),
        steins_syntax::CallTarget::ConstFetch(s) => Some(steins_catalog::StreamTarget::Constant(s)),
    }
}

/// The proven effect findings a builtin `name` carries: its unconditional catalog
/// color ([`steins_catalog::effect_labels`]) joined with the **conditional**
/// by-ref out-parameter color this particular call earns
/// ([`steins_catalog::out_params`]).
///
/// The two axes are independent and both may fire: `shuffle($rows)` is
/// `nondet.random` *and* `mutate.local`. Empty for a pure or uncatalogued builtin
/// called without an out-parameter.
///
/// `policy` supplies the ADR-0084 attribution of the builtin being called. A
/// builtin draws no edge in the effect graph — its findings are inserted straight
/// into the caller's direct set — so the *production site* is the boundary the
/// attribution has to be stamped at: every path to this effect passes through
/// this call by construction, so a finding born attributed is attributed on all
/// of them, exactly what leg 2's `every` asks.
///
/// `const_args` is the third axis and the only one that can make a row *narrower*
/// (issue #318): a wrapper-capable stream row is `io` until the call site proves
/// which channel it opens, and [`steins_catalog::narrowed_stream_labels`] is what
/// reads the proof. Both consumers of a call origin — the summary fixpoint and
/// the envelope check — reach the decision through this one function, so the two
/// cannot answer differently. `None` is the honest answer wherever the arguments
/// are not in hand (a builtin passed *as* a callback is invoked with arguments of
/// the invoker's choosing, never ones written here).
fn builtin_findings(
    name: &str,
    span: steins_syntax::Span,
    tree: &SourceTree,
    path: &str,
    arg_targets: Option<&[steins_syntax::RefTarget]>,
    const_args: Option<&steins_syntax::ConstArgs>,
    policy: &EffectsPolicy,
) -> Vec<EffectFinding> {
    let narrowed = const_args.and_then(|c| {
        steins_catalog::narrowed_stream_labels(
            name,
            stream_target(c.first.as_ref()),
            stream_target(c.second.as_ref()),
        )
    });
    let colored: &[&str] =
        narrowed.as_deref().unwrap_or_else(|| steins_catalog::effect_labels(name).unwrap_or(&[]));
    let by_ref = out_param_labels(name, arg_targets);
    if colored.is_empty() && by_ref.is_empty() {
        return Vec::new();
    }
    let line = tree.position(span.start).line;
    let attributed = policy.function_attribution(name);
    colored
        .iter()
        .copied()
        .chain(by_ref)
        .map(|label| {
            EffectFinding::direct(label.to_owned(), name.to_owned(), line, path.to_owned())
                .attributed_by(attributed)
        })
        .collect()
}

/// Resolve a method-call effect origin to the unit it edges to (project-wide).
pub(crate) fn resolve_effect_edge(
    cx: &Cx,
    enclosing: Option<&str>,
    receiver: &EffectRecv,
    method: &str,
) -> Option<Sym> {
    let (start, exact) = match receiver {
        EffectRecv::This | EffectRecv::SelfKw => (enclosing?.to_owned(), false),
        EffectRecv::Parent => (cx.parent_fqn(enclosing?)?, true),
        EffectRecv::ClassName(name) => (cx.class_fqn(name), true),
        // A declared receiver (ADR-0067) names an abstraction, never a body: the
        // whole point is that dependency injection put an unknown implementation
        // behind it. It draws no propagation edge — see [`resolve_declared_bound`].
        EffectRecv::Var(_) | EffectRecv::PropRead(_) => return None,
    };
    let Resolution::Found(r) = resolve_in_chain(cx, &start, method) else { return None };
    if r.method.visibility == Visibility::Private
        && !enclosing.is_some_and(|e| e.eq_ignore_ascii_case(&r.declaring_class.fqn))
    {
        return None;
    }
    if !exact {
        let declaring_final = r.declaring_class.is_final;
        if !(r.method.is_final || r.method.visibility == Visibility::Private || declaring_final) {
            return None;
        }
    }
    Some(Sym::Method(r.declaring_class.fqn.clone(), r.method.name.clone()))
}

/// The **builtin-class catalog** answer for a method-call origin whose receiver
/// draws no project edge (issue #67): the findings a
/// [`steins_catalog::method_effect_labels`] row contributes, `Some(vec![])` for a
/// catalogued-pure row, and `None` when the catalog says nothing — which is the
/// caller's cue to taint exhaustiveness, exactly as an unresolved receiver does
/// today.
///
/// Three gates stand between a method call and a row, and each one is the
/// FP-safe side of a question the analyzer cannot otherwise answer:
///
/// * the receiver must be a **named class** (`new PDO(...)->query()`,
///   `PDO::…`) — `$this`/`self`/`parent` are the project's own world, and a
///   `$pdo->query()` variable receiver names no class to look up (it is either an
///   [`EffectOrigin::Opaque`] or an ADR-0067 declared receiver, and both taint
///   here);
/// * the name must resolve to a class the project **does not define**
///   ([`Cx::class_absent`]) — a project `PDO` shadows the catalog, because its
///   body is the truth and [`resolve_effect_edge`] already drew that edge;
/// * the resolved FQN must be **global** — the engine's classes are unnamespaced,
///   so an unimported `PDO` inside `namespace App;` is `App\PDO`, some class of
///   the user's that Steins simply has not indexed, and coloring it `io.db` would
///   be the guess this analyzer does not make.
fn builtin_method_findings(
    cx: &Cx,
    receiver: &EffectRecv,
    method: &str,
    span: steins_syntax::Span,
    policy: &EffectsPolicy,
) -> Option<Vec<EffectFinding>> {
    let EffectRecv::ClassName(name) = receiver else { return None };
    let fqn = cx.class_fqn(name);
    if fqn.contains('\\') || !cx.class_absent(&fqn) {
        return None;
    }
    let labels = steins_catalog::method_effect_labels(&fqn, method)?;
    // The source spelling, as the function rows use `name.simple()`.
    let origin = format!("{}::{}", name.simple(), method);
    let line = cx.tree().position(span.start).line;
    // A catalogued external class is attributable the same way a builtin function
    // is, and by the same argument: the call site is where the effect is produced.
    let attributed = policy.method_attribution(&fqn, method);
    Some(
        labels
            .iter()
            .map(|label| {
                EffectFinding::direct(
                    (*label).to_owned(),
                    origin.clone(),
                    line,
                    cx.path().to_owned(),
                )
                .attributed_by(&attributed)
            })
            .collect(),
    )
}

/// Which **trust stratum** a declared bound was written in — the one thing a call
/// site needs to know about an envelope beyond its labels (ADR-0082 §1).
///
/// Both strata feed the same declared lane; they differ in what the call site may
/// conclude from the *absence* of further information.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DeclaredBound {
    /// A **checked** envelope: `#[\Steins\Effect(...)]` / `#[\Steins\Pure]`, which
    /// `effect.envelope-exceeded` and `effect.liskov-widened` hold every analyzed
    /// implementation to. Importing it discharges this call site's taint.
    Checked(Vec<String>),
    /// An **interop envelope** (ADR-0082): one of upstream's purity tags in a
    /// docblock. Nothing has checked it at this call site, so it follows
    /// ADR-0068's plugin discipline — the labels enter the declared lane and the
    /// exhaustiveness taint stays. Assert, never prove.
    Interop(Vec<String>),
}

/// The **declared** effect bound a call through a declared receiver imports
/// (ADR-0067): the effect envelope carried by `method` on the project interface
/// the receiver's declared type names.
///
/// `None` is the pre-ADR-0067 answer — the receiver has no declared type, the type
/// is not a single project interface, the interface does not declare the method,
/// or the declaration carries no envelope. In every one of those cases the call
/// site keeps tainting exhaustiveness: *absence of a contract is not a contract*.
///
/// Only interfaces qualify. A non-final class is an abstraction carrier too, but a
/// class *has* a body, so its envelope and its inferred effects are two different
/// facts that the proven lane already reasons about; keeping the declared lane to
/// interfaces keeps the two from arguing.
fn resolve_declared_bound(
    cx: &Cx,
    registry: &steins_catalog::LabelRegistry,
    enclosing: Option<&str>,
    params: &[steins_syntax::Param],
    receiver: &EffectRecv,
    method: &str,
) -> Option<DeclaredBound> {
    let ty = match receiver {
        // `f(Repo $r) { $r->find(); }` — the parameter's own declared type. The
        // syntax gate already proved this frame never writes `$r`, so the binding
        // still holds what the signature typed.
        EffectRecv::Var(name) => params.iter().find(|p| &p.name == name)?.ty.as_ref()?,
        // `$this->repo->find()` — the declared (or constructor-promoted) type of
        // the property, inherited members included.
        EffectRecv::PropRead(prop) => {
            cx.class_props(enclosing?).into_iter().find(|p| &p.name == prop)?.ty.as_ref()?
        }
        EffectRecv::This | EffectRecv::SelfKw | EffectRecv::Parent | EffectRecv::ClassName(_) => {
            return None;
        }
    };
    let fqn = sole_object_fqn(ty)?;
    let (file, decl) = cx.find_class(&fqn)?;
    if !decl.is_interface {
        return None;
    }
    // The checked stratum beats the unchecked one (ADR-0082 §1): the attribute
    // walk runs first and unchanged, and the docblock walk is consulted only when
    // it came back empty — so an interop tag never preempts an attribute
    // envelope, neither on the same declaration nor anywhere up the hierarchy.
    nearest_interface_envelope(cx, file, decl, method).map(DeclaredBound::Checked).or_else(|| {
        nearest_interop_envelope(cx, registry, file, decl, method).map(DeclaredBound::Interop)
    })
}

/// The FQN of a declared type that names **exactly one** object type, or `None`
/// for a union, an intersection, or a scalar. A nullable single object type still
/// qualifies: `null` never reaches the method, so the interface's envelope still
/// bounds every call that actually happens.
fn sole_object_fqn(ty: &steins_syntax::NativeType) -> Option<String> {
    match ty.members.as_slice() {
        [steins_syntax::TypeMember::Instance { fqn, .. }] => Some(fqn.clone()),
        _ => None,
    }
}

/// The nearest effect envelope declared for `method` on an interface hierarchy,
/// searched breadth-first from the interface itself outward through the
/// interfaces it extends — so the nearest carrier wins. An interface that
/// redeclares the method without an envelope does not erase an ancestor's bound
/// (an implementation owes both, and the ancestor's is the one that was written).
fn nearest_interface_envelope<'a>(
    cx: &Cx<'a>,
    start_file: usize,
    start: &'a ClassDecl,
    method: &str,
) -> Option<Vec<String>> {
    let mut level: Vec<(usize, &'a ClassDecl)> = vec![(start_file, start)];
    let mut seen: HashSet<String> = HashSet::new();
    while !level.is_empty() {
        let mut next: Vec<(usize, &'a ClassDecl)> = Vec::new();
        for (file, id) in level {
            if !seen.insert(id.fqn.to_ascii_lowercase()) {
                continue;
            }
            if let Some(m) = id.methods.iter().find(|m| m.name.eq_ignore_ascii_case(method))
                && let Some(env) = &m.effect_envelope
            {
                return Some(env.labels.clone());
            }
            let tree = cx.units[file].tree;
            let parents = id
                .parent
                .iter()
                .chain(id.implements.iter())
                .map(|r| tree.resolve_class_fqn(r))
                .collect::<Vec<String>>();
            for fqn in parents {
                if let Some((f, d)) = cx.find_class(&fqn)
                    && d.is_interface
                {
                    next.push((f, d));
                }
            }
        }
        level = next;
    }
    None
}

/// The nearest **interop envelope** (ADR-0082) declared for `method` on an
/// interface hierarchy — the docblock twin of [`nearest_interface_envelope`],
/// walked breadth-first in exactly the same order, so the nearest carrier wins
/// here too.
///
/// The stopping rule differs in two ways, and both follow from where the tags
/// live. An interface that redeclares the method but carries no purity tag of its
/// own *and* no class-level one keeps the search going outward, because a
/// class-level tag only ever distributes over the methods its own class-like
/// declares (upstream's rule, ADR-0082 §5). An interface that *does* carry a tag
/// stops the search even when that tag is [`InteropTag::Unbounded`]: it won its
/// nearest-wins contest, and ⊤ is its answer.
fn nearest_interop_envelope<'a>(
    cx: &Cx<'a>,
    registry: &steins_catalog::LabelRegistry,
    start_file: usize,
    start: &'a ClassDecl,
    method: &str,
) -> Option<Vec<String>> {
    let mut level: Vec<(usize, &'a ClassDecl)> = vec![(start_file, start)];
    let mut seen: HashSet<String> = HashSet::new();
    while !level.is_empty() {
        let mut next: Vec<(usize, &'a ClassDecl)> = Vec::new();
        for (file, id) in level {
            if !seen.insert(id.fqn.to_ascii_lowercase()) {
                continue;
            }
            let tree = cx.units[file].tree;
            if let Some(m) = id.methods.iter().find(|m| m.name.eq_ignore_ascii_case(method)) {
                match interop_envelope(registry, tree, id, m) {
                    // The declared lane wants the bound, not its spelling: a call
                    // site imports labels, and a bare ⊤ tag imports none of them.
                    InteropTag::Bound(_, labels) => return Some(labels),
                    // A tag was written and it says nothing. Importing an
                    // ancestor's narrower bound instead would speak over the
                    // carrier that actually won.
                    InteropTag::Unbounded => return None,
                    InteropTag::Absent => {}
                }
            }
            let parents = id
                .parent
                .iter()
                .chain(id.implements.iter())
                .map(|r| tree.resolve_class_fqn(r))
                .collect::<Vec<String>>();
            for fqn in parents {
                if let Some((f, d)) = cx.find_class(&fqn)
                    && d.is_interface
                {
                    next.push((f, d));
                }
            }
        }
        level = next;
    }
    None
}

/// The envelope a declaration is **actually held to** (ADR-0082 role B), or `None`
/// when nothing constrains it.
///
/// The checked stratum wins outright (ADR-0082 §1). The shadowing is total: the
/// caller does not even *read* the docblock when an attribute is present, so the
/// interop bound is then neither checked nor label-validated — checking both
/// would let a docblock manufacture a finding against a declaration whose author
/// already wrote the authoritative bound one line down.
///
/// `anchor` is where a finding about the declaration itself lands when the interop
/// envelope wins — the declaration's name, since the tag lives in trivia the
/// attribute path has no analogue for, and the name is where
/// [`emit_effect_liskov`] already anchors declaration-level effect findings.
fn operative_bound<'a>(
    attr: Option<&'a EffectEnvelope>,
    interop: Option<&'a (EnvelopeTag, Vec<String>)>,
    anchor: Span,
    policy: &'a EffectsPolicy,
) -> Option<OperativeBound<'a>> {
    if let Some(env) = attr {
        return Some(OperativeBound {
            labels: &env.labels,
            span: env.span,
            spelling: EnvelopeSpelling::Attribute,
            policy,
        });
    }
    let (tag, labels) = interop?;
    // A bare `@phpstan-all-methods-impure` is ⊤ — every effect possible — and the
    // only tag that reaches here carrying no labels while meaning "unconstrained"
    // (ADR-0082 §3). It must not build a bound: an empty label list is the *pure*
    // envelope everywhere else in this pass, so checking it would read upstream's
    // widest claim as its narrowest and flag every method in the class.
    if labels.is_empty() && matches!(tag, EnvelopeTag::AllMethodsImpure) {
        return None;
    }
    Some(OperativeBound { labels, span: anchor, spelling: EnvelopeSpelling::Interop(*tag), policy })
}

/// What one docblock says about a declaration's interop envelope.
///
/// Three answers, not two, because *no tag* and *a tag that says nothing* behave
/// differently under upstream's nearest-wins precedence (ADR-0082 §5): the first
/// lets an outer carrier speak for the declaration, the second does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InteropTag {
    /// No tag of the consulted families is written here. Keep looking outward.
    Absent,
    /// A tag is written, and it bounds nothing — the ⊤ envelope, which is what the
    /// absence of information already means. It still won its precedence contest:
    /// nothing further out gets to speak for this declaration.
    Unbounded,
    /// A usable bound: the tag family (for quoting the declaration back) and its
    /// labels, every one of them known to the registry.
    Bound(EnvelopeTag, Vec<String>),
}

impl InteropTag {
    /// The bound, for the consumers that treat ⊤ and silence alike.
    fn into_bound(self) -> Option<(EnvelopeTag, Vec<String>)> {
        match self {
            Self::Bound(env, labels) => Some((env, labels)),
            Self::Absent | Self::Unbounded => None,
        }
    }
}

/// The interop tag one docblock carries, with any label the `registry` does not
/// know collapsing the **whole tag** to [`InteropTag::Unbounded`] (owner ruling,
/// 2026-08-12).
///
/// Current PHPStan discards everything after `@phpstan-impure`, so wild code
/// legitimately carries one-word prose — `@phpstan-impure database` — that this
/// grammar would otherwise read as a label. Treating an unrecognized label as
/// *unspecified* keeps such a docblock from failing a run; a separate rule owns
/// typo reporting, on the checked stratum.
///
/// The whole tag goes inert rather than dropping just the unknown labels: an
/// unknown label is ⊤ (no information), and an upper bound containing ⊤ is ⊤.
/// Checking the body against the *known subset* would hold it to a narrower
/// bound than written (`@phpstan-impure io.db, io.netw` is not a claim of
/// `io.db`), manufacturing findings the zero-FP bar forbids. Widening to ⊤ can
/// only lose findings, never invent them.
pub(crate) fn interop_tag(
    registry: &steins_catalog::LabelRegistry,
    docblock: Option<&String>,
    accept: impl Fn(EnvelopeTag) -> bool,
) -> InteropTag {
    let Some((env, labels)) = docblock_envelope_tag(docblock, accept) else {
        return InteropTag::Absent;
    };
    if labels.iter().any(|l| !registry.is_known(l)) {
        return InteropTag::Unbounded;
    }
    InteropTag::Bound(env, labels)
}

/// The tag families a declaration's **own** docblock may carry (the method-level
/// pair). A named predicate, not a closure at each call site, so the reader
/// ([`own_interop_envelope`]) and the vocabulary check
/// ([`report_interop_vocabulary`]) cannot come to consult different tags.
const fn own_tag(env: EnvelopeTag) -> bool {
    matches!(env, EnvelopeTag::Pure | EnvelopeTag::Impure)
}

/// The class-level pair, which distributes over the methods of the class-like it
/// annotates (ADR-0082 §5). The [`own_tag`] counterpart.
const fn class_tag(env: EnvelopeTag) -> bool {
    matches!(env, EnvelopeTag::AllMethodsPure | EnvelopeTag::AllMethodsImpure)
}

/// Emit `effect.interop-unknown-label` (issue #311) for a docblock whose interop
/// tag [`interop_tag`] just read as **unspecified** — but only for the labels that
/// carry evidence of label intent.
///
/// This is the vocabulary-conformance diagnostic the interop spec's fail-open
/// paragraph asks an enforcing checker for. The bound-reading rule stays exactly
/// as it was: the tag is inert either way, and this function is told so by
/// [`interop_tag`] rather than re-deriving it, so the ruling has one
/// implementation. Firing on an inert tag is the entire point — a declaration
/// that checks nothing while looking like it checks something is the
/// degradation the ruling asked to be made visible.
///
/// What it must never do is read a human's prose as a bound the author fumbled.
/// [`steins_catalog::LabelRegistry::label_intent`] owns that judgment and answers
/// `None` for a lone far-off word, which is silence on every surface, permanently.
///
/// `subject` names the declaration as the message quotes it (`f()`,
/// `C::save()`, `class C`); `accept` is the tag family this site consults, and
/// `anchor` the declaration's own name — where the attribute path anchors the same
/// kind of finding.
fn report_interop_vocabulary(
    out: &mut Vec<Diagnostic>,
    cx: &Cx,
    subject: &str,
    docblock: Option<&String>,
    accept: fn(EnvelopeTag) -> bool,
    anchor: Span,
    registry: &steins_catalog::LabelRegistry,
) {
    if !matches!(interop_tag(registry, docblock, accept), InteropTag::Unbounded) {
        return;
    }
    // The re-scan is how the labels are recovered for the message: `Unbounded`
    // deliberately carries none of them, because a ⊤ tag has no bound to carry. The
    // `else` arm is unreachable — only a tag that scanned can classify as
    // `Unbounded` — and returns rather than assert, since a message is not worth a
    // panic.
    let Some((env, labels)) = docblock_envelope_tag(docblock, accept) else { return };
    let tag = EnvelopeSpelling::Interop(env).tag_name();
    let pos = cx.tree().position(anchor.start);
    for label in &labels {
        if registry.is_known(label) {
            continue;
        }
        let Some(intent) = registry.label_intent(label, &labels) else {
            continue;
        };
        // Every variant states the consequence, because it is the part a reader
        // cannot see: their tag is still there, and it is checking nothing.
        let head = format!(
            "unknown effect label '{label}' in {tag} on {subject} — the whole tag reads as \
             unspecified and bounds nothing"
        );
        let message = match intent {
            steins_catalog::LabelIntent::Near(near) => format!("{head}; did you mean '{near}'?"),
            steins_catalog::LabelIntent::Retired(r) => {
                format!("{head}; {}", retirement_clause(r))
            }
            // Intent is evident, but nothing in the vocabulary is a candidate to
            // suggest — naming a far-off label here would be a worse guess than
            // saying nothing.
            steins_catalog::LabelIntent::KnownSibling | steins_catalog::LabelIntent::DotPath => {
                head
            }
        };
        out.push(Diagnostic {
            id: INTEROP_UNKNOWN_LABEL_ID,
            path: cx.path().to_owned(),
            line: pos.line,
            column: pos.column,
            message,
            facet: None,
            fix: None,
        });
    }
}

/// How both unknown-label checks spell a **retirement** (issue #311): the one
/// migration sentence, shared so the docblock and the attribute stratum cannot
/// give a reader two different answers about the same renamed node.
fn retirement_clause(r: &steins_catalog::RetiredLabel) -> String {
    format!("'{}' was retired, so write {}", r.spelling, r.guidance)
}

/// The **interop envelope** (ADR-0082) written on one declaration's *own*
/// docblock: the `@phpstan-pure` / `@phpstan-impure <labels>` families, with no
/// class-level fallback. The tag family travels with the labels — a finding has to
/// quote the declaration back in the spelling its author used.
///
/// This is the nearest-wins half of [`interop_envelope`]'s precedence, and the
/// whole of a top-level function's: the class-level `all-methods-*` pair
/// distributes over the methods of the class-like it annotates (upstream's rule,
/// ADR-0082 §5), so no class tag anywhere can reach a free function.
fn own_interop_envelope(
    registry: &steins_catalog::LabelRegistry,
    docblock: Option<&String>,
) -> InteropTag {
    match interop_tag(registry, docblock, own_tag) {
        // `@phpstan-impure <labels>` is `≤labels`; the bare spelling is ⊤ and never
        // scans to a tag at all, so `labels` is never empty here.
        InteropTag::Bound(EnvelopeTag::Impure, labels) => {
            InteropTag::Bound(EnvelopeTag::Impure, labels)
        }
        // `@phpstan-pure` takes no labels: the empty bound.
        InteropTag::Bound(env, _) => InteropTag::Bound(env, Vec::new()),
        other => other,
    }
}

/// Whether a docblock could possibly carry an interop-envelope tag — the same
/// cheap substring gate [`docblock_envelope_tag`] opens with, exposed for the
/// whole-project fast path that decides whether the effect fixpoint runs at all.
fn spells_interop_envelope(docblock: Option<&String>) -> bool {
    docblock.is_some_and(|t| t.contains("pure"))
}

/// The **interop envelope** (ADR-0082) one method declaration carries: the effect
/// bound written in upstream's purity tags, read from the method's own docblock
/// and, failing that, from the declaring class-like's.
///
/// [`InteropTag::Absent`] means *nothing was written* — the same answer an absent
/// docblock gives, and the caller's cue to keep looking or keep its taint. An
/// empty label list on a [`InteropTag::Bound`] is the **empty** bound
/// (`@phpstan-pure`): a real claim, not a missing one — except under
/// `AllMethodsImpure`, whose bare form is ⊤. The tag family is returned
/// alongside so a consumer can tell those two apart and quote the declaration
/// back as its author spelled it.
///
/// Precedence is upstream's **nearest-wins**, not Steins' Liskov conjunction: a
/// method-level tag replaces the class-level one outright rather than joining
/// it (ADR-0082 §5 — rewriting the semantics of someone else's implemented tag
/// is not "interop"). Within one docblock the first envelope tag wins; two
/// contradictory ones is a user error this reader does not diagnose.
fn interop_envelope(
    registry: &steins_catalog::LabelRegistry,
    tree: &SourceTree,
    class: &ClassDecl,
    method: &MethodDecl,
) -> InteropTag {
    // A method-level tag always wins; the class-level pair written *on a method*
    // says nothing about that method upstream, so it is not a method-level tag.
    //
    // "Wins" includes winning with ⊤: a method whose own tag went inert is
    // unbounded, NOT a method that said nothing. Falling back to the class tag
    // there would check `/** @phpstan-impure database */ function save()` against
    // its class's `@phpstan-all-methods-pure` — holding an author to the opposite
    // of what they wrote.
    match own_interop_envelope(registry, method.docblock.as_ref()) {
        InteropTag::Absent => {}
        won => return won,
    }
    match interop_tag(registry, class.docblock.as_ref(), class_tag) {
        // `all-methods-impure` covers every declared method unconditionally —
        // bare, it is the ⊤ bound, which contributes no labels.
        InteropTag::Bound(env @ EnvelopeTag::AllMethodsImpure, labels) => {
            InteropTag::Bound(env, labels)
        }
        // `all-methods-pure` covers the constructor (upstream's fixtures bless a
        // property-initializing pure constructor) but **not** a void-returning
        // method. Upstream's quirk, adopted verbatim (ADR-0082 §5).
        InteropTag::Bound(env, _) => {
            if method.is_constructor || !returns_void(tree, method) {
                InteropTag::Bound(env, Vec::new())
            } else {
                InteropTag::Absent
            }
        }
        other => other,
    }
}

/// The first interop-envelope tag in a docblock whose family `accept` admits,
/// with its label list as written.
///
/// The cheap substring gate ([`spells_interop_envelope`]) is [`conditional_purity`]'s
/// idiom, and it is exact for this family: every accepted spelling — `@pure`,
/// `@psalm-pure`, `@impure`, `@phpstan-all-methods-pure`,
/// `@phpstan-all-methods-impure` — contains `pure`. Scanning every docblock in the
/// project would not be cheap.
fn docblock_envelope_tag(
    docblock: Option<&String>,
    accept: impl Fn(EnvelopeTag) -> bool,
) -> Option<(EnvelopeTag, Vec<String>)> {
    if !spells_interop_envelope(docblock) {
        return None;
    }
    let text = docblock?;
    scan_docblock(text).into_iter().find_map(|tag| match tag.kind {
        TagKind::InteropEnvelope(env) if accept(env) => Some((env, tag.labels)),
        _ => None,
    })
}

/// Whether a method declares a `void` return — the one signature fact
/// `@phpstan-all-methods-pure` reads (ADR-0082 §5).
///
/// [`MethodDecl::ret`] cannot answer it: `void`, `array`, `mixed` and an absent
/// hint all lower to `None`. So the native hint is read back **as written**
/// (ADR-0078's `ret_span`), and only when none was written does the docblock's
/// `@return` get a say. A method declaring neither return type is *not* void:
/// the envelope should be dropped only where the void quirk provably applies.
fn returns_void(tree: &SourceTree, method: &MethodDecl) -> bool {
    if let Some(span) = method.ret_span {
        return tree.text_at(span).is_some_and(|t| t.trim().eq_ignore_ascii_case("void"));
    }
    method.docblock.as_ref().is_some_and(|doc| {
        scan_docblock(doc).iter().any(|t| {
            t.kind == TagKind::Return
                // `type_text` may carry a trailing description; only the type leads.
                && t.type_text
                    .split_whitespace()
                    .next()
                    .is_some_and(|w| w.eq_ignore_ascii_case("void"))
        })
    })
}
