//! The effects pass (ADR-0005): `#[\Steins\Pure]` envelope checking, project-wide.
//!
//! A monotone fixpoint over the resolved call graph — `effects(f) = own(f) ∪
//! ⋃ effects(callee)` with an exhaustiveness bit tainted by dynamic / unresolved
//! calls — feeding `effect.envelope-exceeded`, `effect.liskov-widened` and the
//! label-vocabulary ids, the [`PurityOracle`] the walker consults, and the
//! [`EffectSummary`] lane `annotate` and the JSON surface render. The graph's
//! node key, [`Sym`], lives with the run's fixpoint holder in
//! [`crate::fixpoints`], not here: the throw system and the escape sweep key on
//! it too.

mod floor;
mod locale_fix;

pub(crate) use self::locale_fix::FIX_TITLE as LOCALE_FIX_TITLE;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use steins_db::{Db, EffectsPolicy, PluginFacts, Project, SourceFile, parse, project_index};
use steins_syntax::Span;
use steins_syntax::{
    ClassDecl, EffectEnvelope, EffectRecv, FunctionDecl, MethodDecl, ScopeOwner, SiteKind,
    SiteOrigin, SourceTree,
};
use steins_phpdoc::{EnvelopeTag, TagKind, scan_docblock};

use crate::throws::{
    ThrowOwnRow, ThrowSet, classify_throw_sites, compute_throws, interface_abstraction_methods,
    last_segment,
};
use crate::cx::Cx;
use crate::facts::FileFacts;
use crate::project::{Diagnostic, FileUnit, Index, LazyTree};
use self::floor::Floor;
use crate::site::engine::MUTATE_LOCAL;
use crate::site::method::declared_receiver_fqn;
use crate::site::reach::Frame;
use crate::site::{
    Edge, GapKind, GapMask, Hit, HitKind, Knowledge, Lane, ResolvedSite, Target, resolve_site,
};
use crate::{
    EFFECT_ID, EFFECT_LISKOV_ID, Fixpoints, Gate, INTEROP_UNKNOWN_LABEL_ID, Sym, UNKNOWN_LABEL_ID,
};

// ---------------------------------------------------------------------------
// Effects pass (ADR-0005): `#[\Steins\Pure]` envelope checking, project-wide.
// ---------------------------------------------------------------------------

/// One proven effect a unit carries, with the provenance a transitive `via`
/// message needs.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    derive(serde::Serialize, serde::Deserialize),
    serde(deny_unknown_fields)
)]
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
    /// The kinds of every gap this unit's answer inherits or makes: empty exactly
    /// when [`Self::exhaustive`], so it names the cause of each `…?`.
    pub(crate) gaps: GapMask,
    /// This unit's OWN `[effects.attribution]` labels (ADR-0084 §1), resolved once
    /// by [`compute_effects`]. Not a third lane: it says nothing about what this
    /// unit does, only what its effects are *for*, and it is read exclusively as
    /// the attribution a caller accumulates when [`Self::findings`] cross the edge
    /// out of here. Carried on the set so a reporting site holding a callee's
    /// [`EffectSet`] can fold in the same edge the fixpoint folds in.
    attribution: Vec<String>,
}

/// One unit's **own** contribution to the effect fixpoint — everything
/// [`classify_effect_sites`] proves about a declaration in isolation, before
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
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EffectOwnRow {
    /// The findings that arise in this unit's own body — with their attribution
    /// sets as full copies, never collapsed to labels (ADR-0084 §2).
    pub(crate) findings: HashSet<EffectFinding>,
    /// Declared-lane labels imported *locally* — one entry per call site whose
    /// receiver's declared interface method carries an envelope (ADR-0067).
    pub(crate) declared: HashSet<String>,
    /// Why this body's own answer is incomplete ([`GapKind`]): exhaustive exactly
    /// when empty. Propagation can only add to it.
    pub(crate) gaps: BTreeSet<GapKind>,
    /// Resolved call edges whose findings AND exhaustiveness taint propagate.
    pub(crate) edges: HashSet<Sym>,
    /// Edges whose findings propagate but whose exhaustiveness taint does not —
    /// a callee whose ADR-0063 conditional-purity contract was fully decided at
    /// the call site.
    pub(crate) untainting: HashSet<Sym>,
}

impl EffectOwnRow {
    /// Fold another row for the same [`Sym`] into this one — what a
    /// declaration split across two files (one FQN, two definitions) produces,
    /// and what a persisted row does when it rejoins the run's table.
    ///
    /// Equal to classifying both bodies into one row, which is what the
    /// enumeration does: every lane is a union and so is the gap set (exhaustive
    /// is a conjunction), because `classify_effect_sites` only ever inserts.
    pub(crate) fn absorb(&mut self, other: &Self) {
        self.findings.extend(other.findings.iter().cloned());
        self.declared.extend(other.declared.iter().cloned());
        self.gaps.extend(other.gaps.iter().copied());
        self.edges.extend(other.edges.iter().cloned());
        self.untainting.extend(other.untainting.iter().cloned());
    }

    /// Whether every site of this body resolved: no gap was recorded.
    pub(crate) fn exhaustive(&self) -> bool {
        self.gaps.is_empty()
    }

    /// The empty row: a unit with no sites has no effects and is exhaustive.
    pub(crate) fn new() -> Self {
        Self {
            findings: HashSet::new(),
            declared: HashSet::new(),
            gaps: BTreeSet::new(),
            edges: HashSet::new(),
            untainting: HashSet::new(),
        }
    }
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
    facts: &[FileFacts],
) -> HashMap<Sym, EffectSet> {
    let (syms, rows) = effect_own_rows(units, index, plugins, policy, facts);
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
    facts: &[FileFacts],
) -> (Vec<Sym>, HashMap<Sym, EffectOwnRow>) {
    // Each effect unit with the file it lives in and its enclosing class FQN.
    struct Unit<'a> {
        sym: Sym,
        file: usize,
        class_fqn: Option<String>,
        sites: &'a [SiteOrigin],
        /// The frame's declared parameters — read only to type an ADR-0067
        /// declared receiver (`EffectRecv::Var`).
        params: &'a [steins_syntax::Param],
    }
    // The files whose rows this run already holds (issue #516): their
    // declarations enter the sym list in the same order the enumeration below
    // would have produced, and their rows are folded in without their trees
    // being decoded. Every other file is enumerated as before.
    let mut syms: Vec<Sym> = Vec::new();
    let mut rows: HashMap<Sym, EffectOwnRow> = HashMap::new();
    let mut ulist: Vec<Unit> = Vec::new();
    for (fi, u) in units.iter().enumerate() {
        if let Some(persisted) = facts.get(fi).and_then(|f| f.rows.as_ref()) {
            syms.extend(persisted.syms.iter().cloned());
            for (sym, row) in &persisted.effects {
                rows.entry(sym.clone()).or_insert_with(EffectOwnRow::new).absorb(row);
            }
            continue;
        }
        for f in u.tree.functions() {
            ulist.push(Unit {
                sym: Sym::Func(f.fqn.clone()),
                file: fi,
                class_fqn: None,
                sites: &f.sites,
                params: &f.params,
            });
        }
        for c in u.tree.classes() {
            for m in &c.methods {
                ulist.push(Unit {
                    sym: Sym::Method(c.fqn.clone(), m.name.clone()),
                    file: fi,
                    class_fqn: Some(c.fqn.clone()),
                    sites: &m.sites,
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
                    sites: &scope.sites,
                    params: &scope.params,
                });
            }
        }
    }

    for unit in &ulist {
        let cx = Cx::new(units, index, unit.file);
        let row = rows.entry(unit.sym.clone()).or_insert_with(EffectOwnRow::new);
        let frame = Frame::new(unit.class_fqn.as_deref(), unit.params, unit.sites);
        classify_effect_sites(&cx, &frame, unit.sites, plugins, policy, row);
    }
    if syms.is_empty() {
        return (ulist.into_iter().map(|u| u.sym).collect(), rows);
    }
    // A mixed run: the enumerated declarations join the persisted ones. Order
    // is by file either way, and the enumeration above skipped exactly the
    // files whose syms are already in `syms`.
    let mut order: Vec<(usize, Sym)> = Vec::new();
    for (fi, _) in units.iter().enumerate() {
        if let Some(persisted) = facts.get(fi).and_then(|f| f.rows.as_ref()) {
            order.extend(persisted.syms.iter().map(|s| (fi, s.clone())));
        }
    }
    let mut merged: Vec<Sym> = Vec::with_capacity(order.len() + ulist.len());
    let mut fresh = ulist.into_iter().peekable();
    for (fi, sym) in order {
        while fresh.peek().is_some_and(|u| u.file < fi) {
            merged.push(fresh.next().expect("peeked").sym);
        }
        merged.push(sym);
    }
    merged.extend(fresh.map(|u| u.sym));
    (merged, rows)
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
    let mut gaps: HashMap<Sym, GapMask> =
        rows.iter().map(|(s, r)| (s.clone(), GapMask::of(&r.gaps))).collect();
    loop {
        let mut changed = false;
        for sym in syms {
            let Some(row) = rows.get(sym) else { continue };
            let mut incoming: Vec<EffectFinding> = Vec::new();
            let mut incoming_declared: Vec<String> = Vec::new();
            let mut callee_gaps = GapMask::default();
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
                callee_gaps = callee_gaps.union(gaps.get(c).copied().unwrap_or_default());
            }
            let set = findings.entry(sym.clone()).or_default();
            for ef in incoming {
                changed |= set.insert(ef);
            }
            let dset = declared.entry(sym.clone()).or_default();
            for label in incoming_declared {
                changed |= dset.insert(label);
            }
            let own = gaps.entry(sym.clone()).or_default();
            let joined = own.union(callee_gaps);
            if joined != *own {
                *own = joined;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // A duplicate FQN is listed once per declaration but owns one merged row; its
    // first listing takes the set, and a later one must not overwrite it with the
    // emptied remainder — that read a body calling `time()` as proven pure (#825).
    let mut seen: HashSet<&Sym> = HashSet::with_capacity(syms.len());
    syms.iter()
        .filter(|s| seen.insert(*s))
        .map(|s| {
            let f = findings.remove(s).unwrap_or_default();
            let dc = declared.remove(s).unwrap_or_default();
            let g = gaps.get(s).copied().unwrap_or_default();
            let at = attribution.remove(s).unwrap_or_default();
            let set = EffectSet {
                findings: f,
                declared: dc,
                exhaustive: g.is_empty(),
                gaps: g,
                attribution: at,
            };
            (s.clone(), set)
        })
        .collect()
}

/// A language construct's own finding ([`Target::Construct`]), at the site's line.
fn construct_finding(cx: &Cx, span: Span, label: &str, spelling: &str) -> EffectFinding {
    EffectFinding::direct(
        label.to_owned(),
        spelling.to_owned(),
        cx.tree().position(span.start).line,
        cx.path().to_owned(),
    )
}

/// The findings an engine [`Hit`] contributes at `span`, attributed as ADR-0084
/// §2 attributes the callee. A builtin draws no edge in the effect graph — its
/// findings are inserted straight into the caller's direct set — so the
/// *production site* is the boundary the attribution has to be stamped at: every
/// path to this effect passes through this call by construction, so a finding born
/// attributed is attributed on all of them, exactly what leg 2's `every` asks. A
/// catalogued external class is attributable the same way, and by the same
/// argument.
///
/// Both consumers of a site — the summary fixpoint and the envelope check — reach
/// the decision through this one function, so the two cannot answer differently.
fn hit_findings(cx: &Cx, span: Span, hit: &Hit, policy: &EffectsPolicy) -> Vec<EffectFinding> {
    if hit.labels.is_empty() {
        return Vec::new();
    }
    let line = cx.tree().position(span.start).line;
    let attributed = match hit.kind {
        HitKind::Function | HitKind::Contract { .. } => {
            policy.function_attribution(&hit.callee).to_vec()
        }
        HitKind::Method | HitKind::Constructor => policy.method_attribution(&hit.callee, &hit.method),
    };
    hit.labels
        .iter()
        .map(|label| {
            EffectFinding::direct(
                (*label).to_owned(),
                hit.origin.clone(),
                line,
                cx.path().to_owned(),
            )
            .attributed_by(&attributed)
        })
        .collect()
}

/// Classify one unit's (or one **region**'s — ADR-0076) sites into its
/// [`EffectOwnRow`]. Split out of
/// [`compute_effects`] so a *sub-span* of a body can be asked the same question
/// the whole body is asked, through exactly the same code: the loop→`array_map`
/// transform's purity precondition is the fixpoint's own verdict restricted to
/// the loop body, never a second opinion about what an effect is.
///
/// Each site is resolved once ([`resolve_site`]) and folded into the row: an edge
/// propagates, a finding is a proven effect, a declared label is a bound, and a
/// gap makes the row non-exhaustive.
pub(crate) fn classify_effect_sites(
    cx: &Cx,
    frame: &Frame,
    sites: &[SiteOrigin],
    plugins: &PluginFacts,
    policy: &EffectsPolicy,
    row: &mut EffectOwnRow,
) {
    let knowledge = Knowledge::Catalog { lane: Lane::Effects, plugins: Some(plugins) };
    for site in sites {
        let resolved = resolve_site(cx, frame, site, &knowledge);
        row.gaps.extend(resolved.gaps.iter().copied());
        for target in &resolved.targets {
            match target {
                Target::Edge(Edge { sym, untainting: false }) => {
                    row.edges.insert(sym.clone());
                }
                Target::Edge(Edge { sym, untainting: true }) => {
                    row.untainting.insert(sym.clone());
                }
                Target::Engine(hit) => {
                    row.findings.extend(hit_findings(cx, site.span, hit, policy));
                }
                Target::Declared(labels) => row.declared.extend(labels.iter().cloned()),
                Target::Construct { label, spelling } => {
                    row.findings.insert(construct_finding(cx, site.span, label, spelling));
                }
                Target::Thrown { .. } => {}
            }
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
    /// The kinds of gap behind `exhaustive == false` (ADR-0099 §5): this body's
    /// own and those it inherits through its call edges, in kebab-case (`no-effect-row`)
    /// and in the order the facts codec numbers the kinds. Empty exactly when
    /// `exhaustive`.
    pub gaps: Vec<&'static str>,
    /// The same for the throw lane: empty exactly when `throws_exhaustive`.
    pub throws_gaps: Vec<&'static str>,
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
    let fixpoints = Fixpoints::new(units, index, plugins, policy, &[]);
    summarize_unit(&fixpoints, target)
}

/// The [`effect_summary_units`] answer for one `target`, read off fixpoints the
/// caller already holds (issue #861).
///
/// The two whole-project fixpoints do not depend on the target, so a caller
/// that summarizes many files of one project builds one [`Fixpoints`] and asks
/// this once per file; each fixpoint then runs at most once however many
/// targets are asked.
#[must_use]
pub(crate) fn summarize_unit(fixpoints: &Fixpoints, target: usize) -> Vec<EffectSummary> {
    let effects = fixpoints.effects();
    let throws = fixpoints.throws();
    let policy = fixpoints.policy();
    let tree = fixpoints.units()[target].tree;
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
    let gap_names = |sym: &Sym| effects.get(sym).map_or_else(Vec::new, |e| e.gaps.names());
    let throws_gap_names = |sym: &Sym| throws.get(sym).map_or_else(Vec::new, |t| t.gaps.names());

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
            gaps: gap_names(&sym),
            throws_gaps: throws_gap_names(&sym),
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
                gaps: gap_names(&sym),
                throws_gaps: throws_gap_names(&sym),
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
/// A region that is exactly a `foreach` statement leaves out that loop's own
/// iteration site (ADR-0099 §4.3's Iterate family; the site carries the
/// statement's span): the verdict is about the loop body, and the caller must
/// prove the subject itself, as the loop→`array_map` transform does by requiring
/// an array (`array_map` raises a `TypeError` on a `Traversable`). A `foreach`
/// nested in the body, in a closure or not, counts.
///
/// A site counts for a region when its span falls inside it, taken over every
/// effect/throw unit of the region's file — the enclosing function's own sites
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
    let effects = compute_effects(&units, &index, plugins, project.effects(db), &[]);
    let throws = compute_throws(&units, &index, &[]);

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

    // Every site of this file that falls inside the region, kept with the frame
    // facts its resolution needs (the enclosing class for a `$this->`/`self::`
    // edge, the parameter list for an ADR-0067 receiver type). The region is
    // classified into an own row exactly as a whole unit is (issue #489) — the same
    // value, restricted to a sub-span.
    let mut row = EffectOwnRow::new();
    let mut trow = ThrowOwnRow::new();
    // The region is the loop statement, and the loop's own iteration site carries the
    // statement's span (ADR-0099 §4.3's Iterate family): the region's purity leaves it
    // out. Whether the subject is an array, proven or vouched for, is the question the
    // transform asks of the subject separately (`array_map` raises a `TypeError` on a
    // `Traversable`); a `foreach` in the body, in a closure or not, is a site like any
    // other, since its span is its own.
    let is_own_subject = |s: &SiteOrigin| {
        (s.span.start, s.span.end) == region
            && matches!(
                &s.kind,
                SiteKind::Operator { construct: steins_syntax::OperatorConstruct::Foreach, .. }
            )
    };

    let mut take = |class_fqn: Option<&str>, params: &[steins_syntax::Param], sites: &[SiteOrigin]| {
        // The frame is the whole body's: a call outside the region can still
        // rebind a parameter an argument inside it names.
        let frame = Frame::new(class_fqn, params, sites);
        let picked: Vec<SiteOrigin> = sites
            .iter()
            .filter(|s| inside(s.span) && !is_own_subject(s))
            .cloned()
            .collect();
        classify_effect_sites(&cx, &frame, &picked, plugins, policy, &mut row);
        // The guards are dropped, not carried: this region's own body cannot
        // hold a `try` (a `try` is a statement, and the eligible body is one
        // append), so every guard on a picked site is an ENCLOSING one — and
        // an enclosing `catch` is the observer that distinguishes the two
        // spellings, so it must not absorb anything here (ADR-0076 §2.3).
        let unguarded: Vec<SiteOrigin> =
            picked.into_iter().map(|s| SiteOrigin { guards: Vec::new(), ..s }).collect();
        classify_throw_sites(&cx, &frame, &unguarded, &mut trow);
    };

    for f in tree.functions() {
        take(None, &f.params, &f.sites);
    }
    for c in tree.classes() {
        for m in &c.methods {
            take(Some(&c.fqn), &m.params, &m.sites);
        }
    }
    for scope in tree.scopes() {
        take(None, &scope.params, &scope.sites);
    }

    // Join the callees' fixpoint results — the region's transitive answer. Both
    // lanes ride the same edges, monotone in the same way, and never mix.
    let mut exhaustive = row.exhaustive();
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

    let mut throws_exhaustive = trow.exhaustive();
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
        if !fx.any(Gate::Purity) {
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
    //
    // Read off the per-file facts where the run has them (issue #516). The
    // *loop* below is deliberately not gated per file the way the throw pass
    // is: `emit_effect_liskov` reads a class's ancestors' envelopes, so a file
    // declaring none can still emit, and narrowing that needs a persisted
    // class → envelope table. A project that declares an envelope anywhere
    // therefore decodes every tree here — recorded rather than hidden, and
    // measured at zero on a corpus that declares none.
    if !fx.any(Gate::Envelope) {
        return Vec::new();
    }

    let effects = fx.effects();
    // The registry this project's declared labels are judged against (ADR-0068):
    // builtin taxonomy plus whatever the plugin channel registered.
    let registry = plugins.registry();
    // The declarations a caller's inherited gap is owed to (the strict floor's
    // discharge 4, ADR-0100 §4): read before the loop, because a callee may live in
    // a file the loop has not reached.
    let enveloped = floor::enveloped_syms(units, index, registry, policy);
    // The `h` and `H` conversions exist from PHP 8.0; only a known floor that reaches it offers
    // them (the locale fix-it), so a floor below it, or one neither declared nor answered by the
    // runtime, offers none.
    let h_ok = fx.php_floor().is_some_and(|floor| floor >= (8, 0));
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
            let bound = OperativeBound { h_ok, ..bound };
            let frame = Frame::new(None, &f.params, &f.sites);
            let floor = Floor::new(&enveloped, f.docblock.as_ref(), &f.params, true);
            report_unit(&mut out, &cx, &frame, plugins, &f.name, bound, (effects, registry, &floor));
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
                        .map(|bound| OperativeBound { h_ok, ..bound })
                {
                    let display = format!("{}::{}", c.name, m.name);
                    let frame = Frame::new(Some(&c.fqn), &m.params, &m.sites);
                    let floor = Floor::new(&enveloped, m.docblock.as_ref(), &m.params, false);
                    let judged = (effects, registry, &floor);
                    report_unit(&mut out, &cx, &frame, plugins, &display, bound, judged);
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
                    emit_effect_liskov(&mut out, &cx, c, m, (effects, plugins, h_ok), policy);
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
    (effects, plugins, h_ok): (&HashMap<Sym, EffectSet>, &PluginFacts, bool),
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
    // The remedy of a proven locale read (ADR-0101 §3.6), offered only when it takes every
    // origin of the read out of the body.
    let locale_fix = proven.contains(&locale_fix::LOCALE).then(|| {
        let frame = Frame::new(Some(&class.fqn), &m.params, &m.sites);
        locale_fix::method_edits(cx, &frame, (effects, plugins, h_ok))
            .and_then(locale_fix::fix_of)
    });
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
                fix: if *label == locale_fix::LOCALE { locale_fix.clone().flatten() } else { None },
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
    /// Whether the run's PHP floor reaches 8.0, where the `h` and `H` conversions exist: the
    /// one thing the locale fix-it needs to know about the target ([`locale_fix`]).
    h_ok: bool,
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

    /// The envelope as its author spelled it: the attribute, or the interop tag
    /// with its label list in the tag's own grammar (ADR-0082 §4: comma-separated
    /// dot-paths, unquoted). The pure tags take no labels, so the tag name is the
    /// whole bound.
    fn spelled(self) -> String {
        let tag = self.spelling.tag_name();
        match self.spelling {
            EnvelopeSpelling::Attribute if self.labels.is_empty() => "#[\\Steins\\Pure]".to_owned(),
            EnvelopeSpelling::Attribute => {
                let quoted: Vec<String> = self.labels.iter().map(|l| format!("'{l}'")).collect();
                format!("#[\\Steins\\Effect({})]", quoted.join(", "))
            }
            EnvelopeSpelling::Interop(_) if self.labels.is_empty() => tag.to_owned(),
            EnvelopeSpelling::Interop(_) => format!("{tag} {}", self.labels.join(", ")),
        }
    }

    /// How `effect.envelope-exceeded` quotes the declaration back, in the
    /// author's own syntax.
    fn declared_clause(self, exceeding_label: &str) -> String {
        let spelled = self.spelled();
        if self.labels.is_empty() {
            return spelled;
        }
        format!("{spelled} — {exceeding_label} exceeds the envelope")
    }
}

/// Emit the diagnostics for one declared-envelope unit (ADR-0005/0018).
///
/// `judged` is what the unit is judged **with**: the fixpoint's effect sets, the
/// label registry, and the strict floor's per-unit facts ([`Floor`]).
fn report_unit(
    out: &mut Vec<Diagnostic>,
    cx: &Cx,
    frame: &Frame,
    plugins: &PluginFacts,
    display: &str,
    bound: OperativeBound<'_>,
    judged: (&HashMap<Sym, EffectSet>, &steins_catalog::LabelRegistry, &Floor<'_>),
) {
    let (effects, registry, floor) = judged;
    report_unknown_labels(out, cx, display, bound, registry);

    // Envelope-exceeded violations: each site is resolved as the fixpoint resolved
    // it, and what it runs is held to the envelope. The strict floor reads the same
    // resolution for the gaps the proven lane is silent about.
    let knowledge = Knowledge::Catalog { lane: Lane::Effects, plugins: Some(plugins) };
    for site in frame.sites {
        let resolved = resolve_site(cx, frame, site, &knowledge);
        report_site(out, cx, site, &resolved, effects, display, bound);
        floor.report_site(out, cx, frame, site, &resolved, effects, display, bound);
    }
}

/// Report a declared label the registry does not know.
fn report_unknown_labels(
    out: &mut Vec<Diagnostic>,
    cx: &Cx,
    display: &str,
    bound: OperativeBound<'_>,
    registry: &steins_catalog::LabelRegistry,
) {
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
}

/// Report what one resolved site runs against the envelope: a project callee's
/// transitive effects (`emit_transitive`), an engine callee's own findings, a
/// language construct's label. Declared bounds and thrown classes are not proven
/// effects (ADR-0067 decision 5), and a gap reports nothing.
fn report_site(
    out: &mut Vec<Diagnostic>,
    cx: &Cx,
    site: &SiteOrigin,
    resolved: &ResolvedSite,
    effects: &HashMap<Sym, EffectSet>,
    display: &str,
    bound: OperativeBound<'_>,
) {
    let span = site.span;
    for target in &resolved.targets {
        match target {
            Target::Edge(edge) => {
                emit_transitive(out, cx, &edge.sym, effects, span.start, display, bound);
            }
            Target::Engine(hit) => {
                for f in hit_findings(cx, span, hit, bound.policy) {
                    if bound.reports(&f) {
                        let prefix = format!("{} has effect {}", hit.shown(), f.label);
                        let mut diag =
                            exceeded_diag(cx, span.start, &prefix, display, bound, &f.label);
                        // The remedy of a proven locale read at a literal printf format
                        // (ADR-0101 §3.6): it removes exactly this finding.
                        if f.label == locale_fix::LOCALE {
                            diag.fix = locale_fix::locale_edits(cx, site, hit, bound.h_ok)
                                .and_then(locale_fix::fix_of);
                        }
                        out.push(diag);
                    }
                }
            }
            Target::Construct { label, spelling } => {
                if bound.exceeds(label) {
                    let prefix = format!("{spelling} has effect {label}");
                    out.push(exceeded_diag(cx, span.start, &prefix, display, bound, label));
                }
            }
            Target::Declared(_) | Target::Thrown { .. } => {}
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

/// Which **trust stratum** a declared bound was written in — the one thing a call
/// site needs to know about an envelope beyond its labels (ADR-0082 §1).
///
/// Both strata feed the same declared lane; they differ in what the call site may
/// conclude from the *absence* of further information.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DeclaredBound {
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
pub(crate) fn resolve_declared_bound(
    cx: &Cx,
    registry: &steins_catalog::LabelRegistry,
    enclosing: Option<&str>,
    params: &[steins_syntax::Param],
    receiver: &EffectRecv,
    method: &str,
) -> Option<DeclaredBound> {
    let fqn = declared_receiver_fqn(cx, enclosing, params, receiver)?;
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
            h_ok: true,
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
    Some(OperativeBound {
        labels,
        span: anchor,
        spelling: EnvelopeSpelling::Interop(*tag),
        policy,
        h_ok: true,
    })
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
    if labels.iter().any(|l| !is_interop_label(registry, l)) {
        return InteropTag::Unbounded;
    }
    InteropTag::Bound(env, labels)
}

/// Whether `label` is in the **interop vocabulary** (ADR-0082 §4): known to the
/// run's `registry`, and outside `failure.*`, which names value provenance
/// (ADR-0042) rather than an effect. A `failure.*` label is therefore unknown to
/// an interop tag exactly as a typo is, while `#[\Steins\Effect]` keeps taking it.
/// The one membership test the bound reading, its vocabulary diagnostic, and the
/// transform's emission guard share (issue #805).
pub(crate) fn is_interop_label(registry: &steins_catalog::LabelRegistry, label: &str) -> bool {
    registry.is_known(label) && !is_provenance_label(label)
}

/// Whether `label` lies under the `failure.*` provenance family (ADR-0042).
fn is_provenance_label(label: &str) -> bool {
    steins_catalog::subsumes("failure", label)
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
        if is_interop_label(registry, label) {
            continue;
        }
        // Every variant states the consequence, because it is the part a reader
        // cannot see: their tag is still there, and it is checking nothing.
        let head = format!(
            "unknown effect label '{label}' in {tag} on {subject} — the whole tag reads as \
             unspecified and bounds nothing"
        );
        // Known to the registry yet outside the interop vocabulary is `failure.*`
        // (issue #805). A registry spelling is its own evidence of intent, and the
        // nearest-label suggestion would only name it back.
        let message = if registry.is_known(label) {
            format!("{head}; failure.* names value provenance, not an effect")
        } else {
            let Some(intent) = registry.label_intent(label, &labels) else {
                continue;
            };
            match intent {
                // Never suggest a label the interop vocabulary refuses in turn.
                steins_catalog::LabelIntent::Near(near) if !is_interop_label(registry, near) => {
                    head
                }
                steins_catalog::LabelIntent::Near(near) => {
                    format!("{head}; did you mean '{near}'?")
                }
                steins_catalog::LabelIntent::Retired(r) => {
                    format!("{head}; {}", retirement_clause(r))
                }
                // Intent is evident, but nothing in the vocabulary is a candidate to
                // suggest — naming a far-off label here would be a worse guess than
                // saying nothing.
                steins_catalog::LabelIntent::KnownSibling
                | steins_catalog::LabelIntent::DotPath => head,
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
pub(crate) fn spells_interop_envelope(docblock: Option<&String>) -> bool {
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
