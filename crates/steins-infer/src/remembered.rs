//! Remembered call results (ADR-0102, slice 1): what a guard proved about a builtin
//! call's result stays true of the next identical call in the frame, until a site
//! the effect lane names rewrites a place the call reads or a setting its row reads.
//!
//! ```php
//! if (strpos($h, '=') !== false) {
//!     dumpType(strpos($h, '='));   // int<0, max>, not int<0, max>|false
//! }
//! ```
//!
//! # The key
//!
//! A [`Remembered`] is held on a **call key**: the builtin's spelling and one
//! component per positional argument, each a local variable (a *place*) or a
//! literal ([`key_of`]). It is structural, never textual (`strlen($s)` and
//! `strlen( $s )` are one key), and no variable can take its spelling, since it
//! contains `(`. A named or spread argument, a nested call, an operator expression, a
//! property or an element gives the call no key (ADR-0102 D2).
//!
//! # The gate
//!
//! A call produces a key only where its **ADR-0099 site answers exhaustive** with
//! every label under `global.read.setting` ([`gate_of`]): a certified-pure name, a
//! call-site certified name whose reach is ruled out, a foldable name on non-literal
//! places, a printf call whose literal format drops every read or keeps only the
//! locale read. `nondet.*`, `io*`, an effect-lane gap, a project callee and a method
//! never produce one. The stat family never does either: its rows carry `io.fs.read`,
//! which is not a setting read (ADR-0102 D1). An argument position the catalog says
//! is by reference, a place holding an object, a resource or a closure, a by-reference
//! parameter and a superglobal each refuse the key, since the call could rewrite the
//! place or the place could change without a site naming it.
//!
//! # Production, the consumer, forgetting
//!
//! *Production* is guard survival: [`produce`] reads the refinements a condition
//! establishes on a branch for the calls it names, by the machinery a variable's
//! refinements take. It binds a scratch variable per key to the call's own answer
//! (the builtin-call ladder, [`builtin_call_rung`]), rewrites the condition so the
//! key stands where the call stood, and applies the refinements and the
//! contract-lane subtraction to that scratch pair, so `!== false`, `=== 3`, an
//! ordering and a truthiness test narrow the key exactly as they narrow a variable.
//! What the scratch pair holds afterwards is the key's [`Lanes`].
//!
//! The *consumer* is the ladder itself ([`compose`]): a call whose key the store
//! holds answers the remembered lanes where its own rungs say less, and the
//! condition evaluator reads a remembered `Verified` finite fact as the call's
//! candidate values ([`candidates`]), so a second call decided by the first is
//! decided, and a decided guard marks its dead branch (ADR-0002).
//!
//! A key is *forgotten* by three mechanisms, none of which tracks anything new:
//!
//! 1. a place it names is rebound: [`Store::unbind`] — ADR-0070's statement-end
//!    forgetting — reaches the keys through [`Store::forget_keys_naming`], and
//!    [`forget_statement_writes`] covers the writes that never pass through it (an
//!    assignment to the place, an offset write to it);
//! 2. a statement holds a site the effect lane says may rewrite a setting — a label
//!    under `global.write`, `eval` or `ffi`, a project callee, or any gap — and
//!    every key whose row reads a setting goes ([`forget_for_statement`]);
//! 3. the join: a key survives a merge only where every branch holds it
//!    ([`join_remembered`]).
//!
//! The stratum is the refinement's: a fact the call's own row seeds `Asserted` (the
//! declared-return floor) stays `Asserted`, and a `=== literal` test pins a
//! `Verified` singleton, which is the only thing the condition evaluator reads.

use std::cell::{Cell, OnceCell};
use std::collections::HashMap;

use steins_catalog::subsumes;
use steins_domain::Fact;
use steins_syntax::{
    ArgValue, CallExpr, Callee, CondExpr, CondOperand, SUPERGLOBALS, SiteKind, Span, StmtKind,
};

use crate::asserts::cond_invalidations;
use crate::builtin_returns::{BuiltinRung, OptionalRungs, builtin_call_rung, floor_value_fact};
use crate::by_value::{arg_is_by_value, is_assert_read_site, is_dump_read_site};
use crate::cx::Cx;
use crate::env::{ContractArm, Known, Store, Stratum, arg_of_val, dedup_contract_arms};
use crate::fold::Folder;
use crate::project::FnResolution;
use crate::refine::{
    Refine, apply_class_narrowing, apply_refinements, else_refinements, then_refinements,
};
use crate::site::reach::Frame;
use crate::site::{HitKind, Knowledge, Lane, ResolvedSite, Target, resolve_site};
use crate::walk::WalkCx;

/// The most keys a store holds: a guard-heavy frame stops remembering past this, so
/// the per-branch clone of the map stays cheap. A key not remembered is silence.
const MAX_KEYS: usize = 64;

/// What a [`Known::bound`] says of a fact a call key supplied.
pub(crate) const REMEMBERED_BOUND: &str = "remembered from a guard on the same call";

/// A call's result as the two lanes a variable carries it in: the value-domain fact
/// with its stratum, and the declared-contract arm list (each arm its own stratum).
/// Either may be absent; a [`Remembered`] holds at least one.
#[derive(Clone, PartialEq, Default)]
pub(crate) struct Lanes {
    pub(crate) fact: Option<(Fact, Stratum)>,
    pub(crate) arms: Option<Vec<ContractArm>>,
}

/// One remembered call result: its [`Lanes`], the places its key names, and whether
/// its row reads a setting (so a site that may rewrite one forgets it).
#[derive(Clone)]
pub(crate) struct Remembered {
    pub(crate) lanes: Lanes,
    pub(crate) places: Vec<String>,
    pub(crate) reads_setting: bool,
}

/// Which gate a call passed: its result is a function of its arguments alone
/// (`Pure`), or also of a setting a site may rewrite (`SettingRead`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Gate {
    Pure,
    SettingRead,
}

/// A name no key may take as a place. `$this` and the superglobals can change
/// without a statement of this frame naming them, and `$http_response_header` is
/// written by the engine itself.
fn is_special_name(name: &str) -> bool {
    name == "this" || name == "http_response_header" || SUPERGLOBALS.contains(&name)
}

/// The key of the call `name(args)` and the places it names, or `None` where a
/// component is not a local variable or a literal (ADR-0102 §2.1).
pub(crate) fn key_of(name: &str, args: &[ArgValue]) -> Option<(String, Vec<String>)> {
    let mut parts = Vec::with_capacity(args.len());
    let mut places: Vec<String> = Vec::new();
    for arg in args {
        match arg {
            ArgValue::Var(v) => {
                if is_special_name(v) {
                    return None;
                }
                parts.push(format!("${v}"));
                if !places.contains(v) {
                    places.push(v.clone());
                }
            }
            ArgValue::Int(_)
            | ArgValue::Float(_)
            | ArgValue::Str(_)
            | ArgValue::Bool(_)
            | ArgValue::Null => parts.push(arg.render()),
            // A constant is fixed once defined; spelled as written, so a second spelling
            // of the same constant is a second key and never a wrong one.
            ArgValue::GlobalConst(r) => parts.push(r.raw.clone()),
            _ => return None,
        }
    }
    let name = name.trim_start_matches('\\').to_ascii_lowercase();
    Some((format!("{name}({})", parts.join(", ")), places))
}

/// The function name and positional arguments of a plain call a key could stand for.
fn call_parts(call: &CallExpr) -> Option<(&str, Vec<ArgValue>)> {
    if !call.positional_only || call.has_spread || !call.named_args.is_empty() {
        return None;
    }
    let Callee::Function(name) = &call.receiver else { return None };
    Some((name.as_str(), call.args.iter().map(|a| a.value.clone()).collect()))
}

/// The gate of a resolved site (ADR-0102 §2.5): exhaustive, and every label a read of
/// a setting. A project edge, a declared bound, a construct and a method each refuse.
fn gate_of(site: &ResolvedSite) -> Option<Gate> {
    if !site.gaps.is_empty() {
        return None;
    }
    let mut reads = false;
    for target in &site.targets {
        let Target::Engine(hit) = target else { return None };
        if !matches!(hit.kind, HitKind::Function) {
            return None;
        }
        for label in &hit.labels {
            if !subsumes("global.read.setting", label) {
                return None;
            }
            reads = true;
        }
    }
    Some(if reads { Gate::SettingRead } else { Gate::Pure })
}

/// The builtins that rewrite an ini setting. A call to one forgets **every** key, the
/// `{}`-row ones too: those rows read the ini without saying so (a float rendered to
/// a string goes through `precision`, a pattern match through the PCRE limits),
/// which ADR-0101 §3.8 records as the catalog's known imprecision.
const INI_WRITERS: &[&str] = &["ini_set", "ini_alter", "ini_restore"];

/// Whether a resolved site may rewrite a setting a remembered result reads
/// (ADR-0102 §2.4, rules 2 and 3): any gap, a project edge or declared bound (what
/// the callee does is the fixpoint's, which the walk does not read), a label under
/// `global.write`, `eval` or `ffi`.
fn forgets_settings(site: &ResolvedSite) -> bool {
    let forgetting = |label: &str| {
        subsumes("global.write", label) || subsumes("eval", label) || subsumes("ffi", label)
    };
    !site.gaps.is_empty()
        || site.targets.iter().any(|t| match t {
            Target::Edge(_) | Target::Declared(_) => true,
            Target::Engine(hit) => hit.labels.iter().any(|l| forgetting(l)),
            Target::Construct { label, .. } => forgetting(label),
            Target::Thrown { .. } => false,
        })
}

/// The per-walk cache of what the effect lane says about the frame's sites, and the
/// statement the walk is inside. Built lazily: a frame that remembers nothing never
/// resolves a site.
pub(crate) struct RememberCache<'a> {
    frame: OnceCell<Option<Frame<'a>>>,
    settings: OnceCell<Vec<Span>>,
    ini: OnceCell<Vec<Span>>,
    stmt: Cell<Span>,
}

impl Default for RememberCache<'_> {
    fn default() -> Self {
        Self {
            frame: OnceCell::new(),
            settings: OnceCell::new(),
            ini: OnceCell::new(),
            stmt: Cell::new(Span { start: 0, end: 0 }),
        }
    }
}

/// Restores the statement the walk was inside when the walk of the next one ends, on
/// every exit of its loop iteration.
pub(crate) struct StmtGuard<'c> {
    cell: &'c Cell<Span>,
    prev: Span,
}

impl Drop for StmtGuard<'_> {
    fn drop(&mut self) {
        self.cell.set(self.prev);
    }
}

/// The knowledge a site is resolved with here: the effect lane's, with no plugin
/// channel. The channel colours what no row covers and leaves the gap, and a gap
/// is a refusal either way.
const EFFECTS: Knowledge<'static> = Knowledge::Catalog { lane: Lane::Effects, plugins: None };

impl<'a> RememberCache<'a> {
    /// Record the statement being walked until the guard drops. Its span is what a
    /// condition asks "does a site in here rewrite a setting?" of.
    pub(crate) fn enter(&self, span: Span) -> StmtGuard<'_> {
        StmtGuard { cell: &self.stmt, prev: self.stmt.replace(span) }
    }

    /// The frame the scope's sites are read against, once.
    fn frame(&self, w: &WalkCx<'a, '_>) -> Option<&Frame<'a>> {
        self.frame
            .get_or_init(|| {
                let (class, params, sites) = w.cx.scope_site_frame(w.scope)?;
                Some(Frame::new(class, params, sites))
            })
            .as_ref()
    }

    /// The gate of the call's own site, or `None` where the site is not found, not a
    /// plain call, or not exhaustive over setting reads.
    fn gate(&self, w: &WalkCx<'a, '_>, call: &CallExpr) -> Option<Gate> {
        let frame = self.frame(w)?;
        let site = frame.sites.iter().find(|s| {
            s.span.start == call.span.start && matches!(&s.kind, SiteKind::Call { .. })
        })?;
        gate_of(&resolve_site(w.cx, frame, site, &EFFECTS))
    }

    /// Whether some site inside `span` may rewrite a setting. The frame's sites are
    /// resolved once, on the first ask.
    fn rewrites_settings_in(&self, w: &WalkCx<'a, '_>, span: Span) -> bool {
        let sites = self.settings.get_or_init(|| {
            let Some(frame) = self.frame(w) else { return Vec::new() };
            frame
                .sites
                .iter()
                .filter(|s| !is_read_only_dump(w.cx, s))
                .filter(|s| forgets_settings(&resolve_site(w.cx, frame, s, &EFFECTS)))
                .map(|s| s.span)
                .collect()
        });
        sites.iter().any(|s| s.start >= span.start && s.end <= span.end)
    }

    /// Whether some call inside `span` is to an ini writer, by the name it spells:
    /// no site is resolved for it, so a frame with no such call pays a name scan.
    fn writes_ini_in(&self, w: &WalkCx<'a, '_>, span: Span) -> bool {
        let sites = self.ini.get_or_init(|| {
            let Some(frame) = self.frame(w) else { return Vec::new() };
            frame
                .sites
                .iter()
                .filter(|s| {
                    matches!(&s.kind, SiteKind::Call { name, .. }
                        if INI_WRITERS.contains(&name.simple().to_ascii_lowercase().as_str()))
                })
                .map(|s| s.span)
                .collect()
        });
        sites.iter().any(|s| s.start >= span.start && s.end <= span.end)
    }
}

/// Whether a site is the analyzer's own observer (`dumpType`, `var_dump`, and under
/// the harness `assertType`): it reads and binds nothing, and it runs no code that
/// could rewrite a setting. The same recognition ADR-0070's by-value survival makes.
fn is_read_only_dump(cx: &Cx, site: &steins_syntax::SiteOrigin) -> bool {
    let SiteKind::Call { name, .. } = &site.kind else { return false };
    is_dump_read_site(cx, name) || is_assert_read_site(cx, name)
}

/// Forget the setting reads at the start of a statement that holds a site which may
/// rewrite a setting (ADR-0102 §2.4, rules 2 and 3), and every key at one that
/// writes an ini setting. Before the statement rather
/// than at its end: a consumer in the same statement as the rewriting site has no
/// order to read, and nothing between the two re-produces a key.
pub(crate) fn forget_for_statement(w: &WalkCx, span: Span, store: &mut Store) {
    if store.remembered.is_empty() {
        return;
    }
    if w.remember.writes_ini_in(w, span) {
        store.remembered.clear();
    } else if store.has_setting_keys() && w.remember.rewrites_settings_in(w, span) {
        store.forget_setting_keys();
    }
}

/// Forget the keys a statement's own write to a place reaches without passing
/// through [`Store::unbind`]: an assignment to the variable, an offset write,
/// append or unset on it.
pub(crate) fn forget_statement_writes(kind: &StmtKind, store: &mut Store) {
    if store.remembered.is_empty() {
        return;
    }
    match kind {
        StmtKind::Assign { var, .. } => store.forget_keys_naming(var),
        StmtKind::OffsetWrite { base, .. }
        | StmtKind::OffsetUnset { base, .. }
        | StmtKind::OffsetAppend { base, .. } => store.forget_keys_naming(base),
        _ => {}
    }
}

/// A call a condition names that may be remembered: its key, the places it names, the
/// spans it appears at, and the gate it passed.
struct Candidate {
    key: String,
    name: String,
    args: Vec<ArgValue>,
    places: Vec<String>,
    spans: Vec<u32>,
    gate: Gate,
}

/// Whether `var` may stand as a place in a key: a value held in the frame alone. An
/// object, a resource or a closure can change without the name being written; a
/// by-reference parameter is another frame's variable.
fn is_value_place(w: &WalkCx, var: &str, env: &HashMap<String, Known>, store: &Store) -> bool {
    if store.refs.contains_key(var)
        || store.members.contains_key(var)
        || env.get(var).is_some_and(|k| k.closure.is_some())
    {
        return false;
    }
    !w.cx.scope_params(w.scope).is_some_and(|ps| ps.iter().any(|p| p.by_ref && p.name == var))
}

/// The candidate `call` is, if it is one: a builtin function the project does not
/// shadow, called with a key's components, by value at every place, whose site passes
/// the gate.
fn candidate(
    w: &WalkCx,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    store: &Store,
) -> Option<Candidate> {
    let cx = w.cx;
    let (name, args) = call_parts(call)?;
    let r = call.callee_ref.as_ref()?;
    if !matches!(cx.resolve_function(r), FnResolution::Builtin(_))
        || cx.index.has_simple_function(name)
    {
        return None;
    }
    let (key, places) = key_of(name, &args)?;
    for (i, arg) in args.iter().enumerate() {
        if let ArgValue::Var(v) = arg

            && !(arg_is_by_value(cx, r, u32::try_from(i).ok()?) && is_value_place(w, v, env, store))
        {
            return None;
        }
    }
    let gate = w.remember.gate(w, call)?;
    Some(Candidate {
        key,
        name: name.to_owned(),
        args,
        places,
        spans: vec![call.span.start],
        gate,
    })
}

/// Collect the candidates a condition names, in source order, one per key.
fn collect(
    w: &WalkCx,
    cond: &CondExpr,
    env: &HashMap<String, Known>,
    store: &Store,
    out: &mut Vec<Candidate>,
) {
    let note = |call: &CallExpr, out: &mut Vec<Candidate>| {
        let Some(c) = candidate(w, call, env, store) else { return };
        match out.iter_mut().find(|o| o.key == c.key) {
            Some(o) => o.spans.extend(c.spans),
            None => out.push(c),
        }
    };
    match cond {
        CondExpr::Call { call, .. } => note(call, out),
        CondExpr::Cmp { lhs, rhs, .. } => {
            for op in [lhs, rhs] {
                if let CondOperand::Other { call: Some(call), .. } = op {
                    note(call, out);
                }
            }
        }
        CondExpr::Truthy(CondOperand::Other { call: Some(call), .. }) => note(call, out),
        CondExpr::Not(c) => collect(w, c, env, store, out),
        CondExpr::And(a, b, _) | CondExpr::Or(a, b, _) => {
            collect(w, a, env, store, out);
            collect(w, b, env, store, out);
        }
        _ => {}
    }
}

/// The condition with each candidate call replaced by the key standing as a variable,
/// so the refinement machinery reads a call's guard as a variable's.
fn rewrite(cond: &CondExpr, keys: &HashMap<u32, &str>) -> CondExpr {
    let operand = |op: &CondOperand| match op {
        CondOperand::Other { call: Some(call), .. } => match keys.get(&call.span.start) {
            Some(key) => CondOperand::Var((*key).to_owned()),
            None => op.clone(),
        },
        _ => op.clone(),
    };
    match cond {
        CondExpr::Call { call, .. } => match keys.get(&call.span.start) {
            Some(key) => CondExpr::Truthy(CondOperand::Var((*key).to_owned())),
            None => cond.clone(),
        },
        CondExpr::Cmp { op, lhs, rhs } => {
            CondExpr::Cmp { op: *op, lhs: operand(lhs), rhs: operand(rhs) }
        }
        CondExpr::Truthy(op) => CondExpr::Truthy(operand(op)),
        CondExpr::Not(c) => CondExpr::Not(Box::new(rewrite(c, keys))),
        CondExpr::And(a, b, span) => {
            CondExpr::And(Box::new(rewrite(a, keys)), Box::new(rewrite(b, keys)), *span)
        }
        CondExpr::Or(a, b, span) => {
            CondExpr::Or(Box::new(rewrite(a, keys)), Box::new(rewrite(b, keys)), *span)
        }
        _ => cond.clone(),
    }
}

/// The variable a refinement narrows.
fn refine_var(r: &Refine) -> &str {
    match r {
        Refine::Exact(v, _)
        | Refine::NotNull(v)
        | Refine::Exclude(v, _)
        | Refine::IntRange(v, _)
        | Refine::Truthy(v) => v,
    }
}

/// The lanes a ladder rung answers, or `None` for a rung that binds a handle
/// (a handle is never remembered: its state is the heap's, not the key's).
fn lanes_of(rung: BuiltinRung) -> Option<Lanes> {
    Some(match rung {
        BuiltinRung::ResourceFold(..) | BuiltinRung::ResourceArms(..) => return None,
        BuiltinRung::Shape(fact, stratum) => Lanes { fact: Some((fact, stratum)), arms: None },
        BuiltinRung::Envelope(fact) => Lanes { fact: Some((fact, Stratum::Verified)), arms: None },
        BuiltinRung::Floor(arms) => Lanes {
            fact: floor_value_fact(&arms).map(|f| (f, Stratum::Asserted)),
            arms: Some(arms),
        },
        BuiltinRung::Remembered(lanes) => lanes,
    })
}

/// **Production** (ADR-0102 §2.2): record on the branch's store what the condition,
/// held on the polarity `then`, proves about the calls it names.
///
/// Each candidate becomes a scratch variable seeded with the call's own answer, the
/// condition is rewritten to name it, and the refinements the condition establishes
/// on that variable are applied to the scratch pair by the same functions that apply
/// them to a real one. What the pair holds afterwards, where it moved, is the key's
/// [`Lanes`].
///
/// The keys naming a place the condition itself may rebind are dropped at the end
/// (the condition's invalidation set, read as `walk_if` reads it): a conjunct that
/// writes `$s` after `strlen($s) > 3` has made the refinement stale.
pub(crate) fn produce(
    w: &WalkCx,
    folder: &mut dyn Folder,
    cond: &CondExpr,
    then: bool,
    env: &HashMap<String, Known>,
    store: &mut Store,
) {
    if w.scope.poisoned || store.remembered.len() >= MAX_KEYS {
        return;
    }
    let mut cands = Vec::new();
    collect(w, cond, env, store, &mut cands);
    // A key is not produced from a statement that holds a site which may rewrite what
    // it reads: the site may run after the call, within the condition. A generator
    // yields to code that may rewrite any setting between its statements.
    let stmt = w.remember.stmt.get();
    if w.remember.writes_ini_in(w, stmt) {
        return;
    }
    cands.retain(|c| {
        c.gate == Gate::Pure
            || (!w.scope.is_generator && !w.remember.rewrites_settings_in(w, stmt))
    });
    if cands.is_empty() {
        return;
    }
    let keys: HashMap<u32, &str> =
        cands.iter().flat_map(|c| c.spans.iter().map(|s| (*s, c.key.as_str()))).collect();
    let rewritten = rewrite(cond, &keys);
    let refs: Vec<Refine> =
        if then { then_refinements(&rewritten) } else { else_refinements(&rewritten) }
            .into_iter()
            .filter(|r| refine_var(r).contains('('))
            .collect();
    cands.retain(|c| refs.iter().any(|r| refine_var(r) == c.key));
    if cands.is_empty() {
        return;
    }
    let mut senv: HashMap<String, Known> = HashMap::new();
    let mut sstore = Store::default();
    let mut seeded: HashMap<String, Lanes> = HashMap::new();
    for c in &cands {
        let rungs = OptionalRungs { resource_folds: false, resource_arms: false };
        let rung = builtin_call_rung(w.cx, folder, &c.name, &c.args, env, Some(store), false, rungs);
        let lanes = match rung {
            Some(rung) => match lanes_of(rung) {
                Some(lanes) => lanes,
                None => continue,
            },
            None => Lanes::default(),
        };
        if let Some((fact, stratum)) = lanes.fact.clone() {
            let bound = Some(REMEMBERED_BOUND.to_owned());
            senv.insert(c.key.clone(), Known::value_strat(fact, 0, bound, stratum));
        }
        if let Some(arms) = lanes.arms.clone() {
            sstore.contract.insert(c.key.clone(), arms);
        }
        seeded.insert(c.key.clone(), lanes);
    }
    apply_refinements(&refs, &mut senv, &mut sstore, Stratum::Verified);
    apply_class_narrowing(w, &rewritten, then, &mut sstore);
    for c in &cands {
        let Some(seed) = seeded.get(&c.key) else { continue };
        let learned = Lanes {
            fact: senv.get(&c.key).and_then(|k| k.fact.clone().map(|f| (f, k.stratum))),
            // A lane subtracted to nothing says no value reaches here, which is a verdict's
            // to give and not a fact's: nothing is remembered.
            arms: sstore.contract.get(&c.key).filter(|a| !a.is_empty()).cloned(),
        };
        if &learned == seed || (learned.fact.is_none() && learned.arms.is_none()) {
            continue;
        }
        store.remembered.insert(
            c.key.clone(),
            Remembered {
                lanes: learned,
                places: c.places.clone(),
                reads_setting: c.gate == Gate::SettingRead,
            },
        );
    }
    for v in cond_invalidations(w.cx, cond, env, store, false) {
        store.forget_keys_naming(&v);
    }
}

/// **The consumer** (ADR-0102 §2.2) at the builtin-call ladder: where the store
/// remembers the call's key, the remembered lanes answer, unless a rung that reads
/// the arguments has already folded the call to one value or the call binds a
/// handle. Every rung's answer is an over-approximation of the one value, and so
/// is the remembered one; the sharper of the two is taken whole rather than met.
pub(crate) fn compose(
    cx: &Cx,
    inner: Option<BuiltinRung>,
    name: &str,
    args: &[ArgValue],
    store: Option<&Store>,
    poisoned: bool,
) -> Option<BuiltinRung> {
    let Some(store) = store else { return inner };
    if poisoned || store.remembered.is_empty() {
        return inner;
    }
    let Some((key, _)) = key_of(name, args) else { return inner };
    let Some(remembered) = store.remembered.get(&key) else { return inner };
    if cx.index.has_simple_function(name) {
        return inner;
    }
    match &inner {
        Some(BuiltinRung::ResourceFold(..) | BuiltinRung::ResourceArms(..)) => inner,
        Some(BuiltinRung::Shape(Fact::Singleton(_), _)) => inner,
        _ => Some(BuiltinRung::Remembered(remembered.lanes.clone())),
    }
}

/// The candidate values of a condition operand that is a remembered call: the finite
/// members of a `Verified` remembered fact, which is what a decided guard may rest on
/// (an `Asserted` fact answers nothing here, as an `Asserted` binding does not).
pub(crate) fn candidates(w: &WalkCx, op: &CondOperand, store: &Store) -> Option<Vec<ArgValue>> {
    let CondOperand::Other { call: Some(call), .. } = op else { return None };
    if store.remembered.is_empty() || w.scope.poisoned {
        return None;
    }
    let (name, args) = call_parts(call)?;
    let (key, _) = key_of(name, &args)?;
    let remembered = store.remembered.get(&key)?;
    if w.cx.index.has_simple_function(name) {
        return None;
    }
    let (fact, stratum) = remembered.lanes.fact.as_ref()?;
    if *stratum != Stratum::Verified {
        return None;
    }
    fact.finite_members().map(|vals| vals.iter().map(arg_of_val).collect())
}

/// The join of two remembered lanes: the fact is their join at the weaker stratum,
/// the arms their union. A lane one side lacks is lacking in the join.
fn join_lanes(a: &Lanes, b: &Lanes) -> Option<Lanes> {
    let fact = match (&a.fact, &b.fact) {
        (Some((fa, sa)), Some((fb, sb))) => fa.join(fb).map(|f| (f, sa.min(*sb))),
        _ => None,
    };
    let arms = match (&a.arms, &b.arms) {
        (Some(x), Some(y)) => {
            let mut merged = x.clone();
            merged.extend(y.iter().cloned());
            dedup_contract_arms(&mut merged);
            Some(merged)
        }
        _ => None,
    };
    (fact.is_some() || arms.is_some()).then_some(Lanes { fact, arms })
}

/// The remembered call results after a merge (ADR-0102 §2.3): a key survives only
/// where every branch holds it, and holds the join of the branches' lanes.
pub(crate) fn join_remembered(first: &Store, rest: &[&Store]) -> HashMap<String, Remembered> {
    let mut out = HashMap::new();
    'keys: for (key, r0) in &first.remembered {
        let mut lanes = r0.lanes.clone();
        for s in rest {
            let Some(r) = s.remembered.get(key) else { continue 'keys };
            let Some(joined) = join_lanes(&lanes, &r.lanes) else { continue 'keys };
            lanes = joined;
        }
        out.insert(
            key.clone(),
            Remembered { lanes, places: r0.places.clone(), reads_setting: r0.reads_setting },
        );
    }
    out
}
