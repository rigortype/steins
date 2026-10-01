//! [`resolve_site`]: the one place a site's callee is resolved, for either lane.
//!
//! The walk below is the union of what the effect lane's and the throw lane's
//! classifiers each did on their own, with each lane's decisions selected by the
//! [`Knowledge`] it hands in. Every arm names the legacy behavior it reproduces;
//! where the two lanes still differ (#864 makes them one), the difference is a
//! `match` on the knowledge in one place, not a second walk.

use steins_syntax::{
    ArgShape, CallbackRef, ConstArgs, ConstructKind, DynamicSite, EffectRecv, NameRef, RefKind,
    RefTarget, SiteKind, SiteOrigin, StaticClass, ThrownKind,
};

use super::contract::{conditional_purity, eval_conditional_purity};
use super::engine::{self, ThrowsRole};
use super::method::{EngineMethod, engine_method, method_edge};
use super::reach::{Frame, builtin_reach, callback_reaches_user_code};
use super::{
    Edge, GapKind, Hit, HitKind, Knowledge, NewTarget, Reach, ResolvedSite, Target, new_origin,
    resolve_new,
};
use crate::Sym;
use crate::cx::Cx;
use crate::project::{FnResolution, Site};
use crate::purity::{DeclaredBound, resolve_declared_bound};
use crate::throws::last_segment;

/// Resolve one site for a lane: what runs there, what it may reach, and why the
/// answer is incomplete. A site the lane does not record resolves to an empty
/// [`ResolvedSite`].
pub(crate) fn resolve_site(
    cx: &Cx,
    frame: &Frame,
    site: &SiteOrigin,
    knowledge: &Knowledge,
) -> ResolvedSite {
    let mut resolver = Resolver { cx, frame, knowledge, site, out: ResolvedSite::default() };
    resolver.run();
    resolver.out
}

/// The argument facts of a named-function call, as the call's site carries them.
struct CallArgs<'s> {
    targets: Option<&'s [RefTarget]>,
    consts: &'s ConstArgs,
    shapes: Option<&'s [ArgShape]>,
    callbacks: &'s [(usize, CallbackRef)],
}

struct Resolver<'a, 'c, 'f> {
    cx: &'a Cx<'c>,
    frame: &'a Frame<'f>,
    knowledge: &'a Knowledge<'a>,
    site: &'a SiteOrigin,
    out: ResolvedSite,
}

impl Resolver<'_, '_, '_> {
    fn run(&mut self) {
        let site = self.site;
        match &site.kind {
            SiteKind::Call { name, callbacks } if callbacks.is_empty() => {
                let args = CallArgs {
                    targets: site.ref_targets.as_deref(),
                    consts: &site.const_args,
                    shapes: site.operands.as_deref(),
                    callbacks: &[],
                };
                self.named_call(name, &args);
            }
            SiteKind::Call { name, callbacks } => self.higher_order(name, callbacks),
            SiteKind::MethodCall { receiver, method } => self.method_call(receiver, method),
            SiteKind::New { class } => self.new_site(class),
            SiteKind::Callback { cbref } => self.callback(cbref),
            // A `$f()` the scan cannot name, in either lane.
            SiteKind::Dynamic(DynamicSite::Call | DynamicSite::MethodCall | DynamicSite::StaticCall) => {
                self.gap(GapKind::DynamicCallee);
            }
            SiteKind::Dynamic(DynamicSite::New | DynamicSite::AnonymousClass) => {
                self.gap(GapKind::UnknownClass);
            }
            SiteKind::Throw(thrown) => self.throw(thrown),
            SiteKind::Construct(construct) => self.construct(construct),
        }
    }

    fn gap(&mut self, kind: GapKind) {
        self.out.gaps.insert(kind);
    }

    fn push(&mut self, target: Target) {
        self.out.targets.push(target);
    }

    /// Record what the engine may run through an operand: [`Reach::Possible`] is
    /// the gap [`GapKind::UserCodeReach`].
    fn note_reach(&mut self, reach: Reach) {
        if reach == Reach::Possible {
            self.gap(GapKind::UserCodeReach);
        }
        self.out.reach = self.out.reach.max(reach);
    }

    /// The lane's own resolution of a named function: the effect lane knows a
    /// builtin by a color or an out-parameter row, the throw lane by a color alone.
    fn function(&self, name: &NameRef) -> FnResolution {
        match self.knowledge {
            Knowledge::Effects { .. } => self.cx.resolve_effect_function(name),
            Knowledge::ThrowsLegacy => self.cx.resolve_function(name),
        }
    }

    /// Record an engine function's hit, if it carries anything on the lane's
    /// axis: an empty one contributes nothing to either consumer.
    fn function_hit(
        &mut self,
        builtin: &str,
        spelled: &str,
        labels: Vec<&'static str>,
        throws: &'static [&'static str],
    ) {
        if labels.is_empty() && throws.is_empty() {
            return;
        }
        self.push(Target::Engine(Hit {
            kind: HitKind::Function,
            callee: builtin.to_owned(),
            method: String::new(),
            origin: builtin.to_owned(),
            spelled: spelled.to_owned(),
            labels,
            throws,
        }));
    }

    // ---- named calls ------------------------------------------------------

    /// One named call: an edge to a project function (or its conditional-purity
    /// row, ADR-0063 §2), a builtin's row, or the gap of a name nothing resolves.
    ///
    /// A builtin answers with its row, and the effect lane adds the gap of an
    /// argument that can reach user code the call site does not rule out (issue
    /// #856). A name only the call site certifies, `array_keys($a)` at one argument
    /// (issue #851) or a string-family name whose every reaching argument is ruled
    /// out (issue #856), answers as a pure builtin before the plugin channel does.
    fn named_call(&mut self, name: &NameRef, args: &CallArgs<'_>) {
        match self.function(name) {
            FnResolution::User(site) => self.user_call(name, site, args),
            FnResolution::Builtin(builtin) => self.builtin_call(name, &builtin, args),
            FnResolution::Unknown => self.unresolved_call(name, args),
        }
    }

    fn user_call(&mut self, name: &NameRef, site: Site, args: &CallArgs<'_>) {
        let decl = self.cx.fn_decl(site);
        let sym = Sym::Func(decl.fqn.clone());
        let contract = match self.knowledge {
            Knowledge::Effects { .. } => conditional_purity(decl.docblock.as_ref(), &decl.params),
            Knowledge::ThrowsLegacy => None,
        };
        let Some(cp) = contract else {
            self.push(Edge::call(sym));
            return;
        };
        let mut pending: Vec<CallbackRef> = Vec::new();
        let r = eval_conditional_purity(&cp, args.callbacks, args.targets, |cbref| {
            pending.push(cbref.clone());
        });
        // The call's edge leads, then the callbacks the contract binds, then the
        // userland out-param row: the order a report names them in. The row is
        // produced at this call site but is the CALLEE's contract, so it is
        // attributed as the callee (`callee` is its FQN).
        self.push(Target::Edge(Edge { sym, untainting: r.discharge_taint }));
        for cbref in &pending {
            self.callback(cbref);
        }
        if !r.labels.is_empty() {
            self.push(Target::Engine(Hit {
                kind: HitKind::Contract { declared: decl.name.clone() },
                callee: decl.fqn.clone(),
                method: String::new(),
                origin: name.simple().to_owned(),
                spelled: name.simple().to_owned(),
                labels: r.labels,
                throws: &[],
            }));
        }
    }

    fn builtin_call(&mut self, name: &NameRef, builtin: &str, args: &CallArgs<'_>) {
        match self.knowledge {
            Knowledge::Effects { .. } => {
                let labels = engine::function_effects(builtin, args.targets, Some(args.consts));
                self.function_hit(builtin, name.simple(), labels, &[]);
                let reach = builtin_reach(self.cx, self.frame, builtin, args.shapes, &[]);
                self.note_reach(reach);
            }
            Knowledge::ThrowsLegacy => {
                let throws = engine::legacy_throws(builtin, ThrowsRole::Call);
                self.function_hit(builtin, name.simple(), Vec::new(), throws);
            }
        }
    }

    /// A call [`Self::function`] left unresolved.
    fn unresolved_call(&mut self, name: &NameRef, args: &CallArgs<'_>) {
        let mut gap = GapKind::UnknownFunction;
        if let Knowledge::Effects { plugins } = self.knowledge {
            // A builtin certified pure at this call's arity or operands answers
            // before the plugin channel, which gets the last word here and nowhere
            // else (ADR-0068 precedence): a project body and a catalog row are both
            // already spoken for.
            if engine::pure_at_call_arity(self.cx, name, args.targets) {
                return;
            }
            match engine::certified_at_call_site(self.cx, self.frame, name, args.shapes) {
                Some(Reach::RuledOut) => return,
                // Certifiable, but an operand may reach user code: that is the gap.
                Some(reach) => {
                    self.note_reach(reach);
                    gap = GapKind::UserCodeReach;
                }
                None if engine::arity_defeated(self.cx, name, args.targets) => {
                    gap = GapKind::ArgumentList;
                }
                None => {}
            }
            if let Some(labels) = plugin_call_labels(self.cx, plugins, name) {
                self.push(Target::Declared(labels.to_vec()));
            }
        }
        self.gap(gap);
    }

    // ---- higher-order calls and callbacks ---------------------------------

    /// A call handing over resolvable callbacks (ADR-0033): the invoker's own row
    /// and the callback's target, or the base call resolved normally for a callee
    /// that is not an invoker.
    fn higher_order(&mut self, name: &NameRef, callbacks: &[(usize, CallbackRef)]) {
        let site = self.site;
        let targets = Some(site.ref_targets.as_deref().unwrap_or(&[]));
        let shapes = Some(site.operands.as_deref().unwrap_or(&[]));
        let FnResolution::Builtin(builtin) = self.cx.resolve_invoker_function(name) else {
            // Not a known invoker: the callee is a normal call, unless it is a user
            // function declaring a conditional-purity contract (ADR-0063 §2).
            let args = CallArgs { targets, consts: &site.const_args, shapes, callbacks };
            self.named_call(name, &args);
            return;
        };
        let callback_param = engine::invoker_callback_param(&builtin)
            .expect("resolve_invoker_function's catalog_knows guarantees a shape row");
        let arg_count = site.ref_targets.as_ref().map_or(0, Vec::len);
        // ADR-0063 P1: the call's effect is the invoker's OWN catalog color joined
        // with the envelope of the callback it immediately invokes. The own-color
        // leg is unconditional — an unresolvable (or absent) callback never
        // *weakens* the invoker's declared color; it only adds a gap.
        match self.knowledge {
            Knowledge::Effects { .. } => {
                let labels = engine::function_effects(&builtin, targets, Some(&site.const_args));
                self.function_hit(&builtin, name.simple(), labels, &[]);
            }
            Knowledge::ThrowsLegacy => {
                let throws = engine::legacy_throws(&builtin, ThrowsRole::Invoker);
                self.function_hit(&builtin, name.simple(), Vec::new(), throws);
            }
        }
        let mut handled = Vec::new();
        if callback_param < arg_count {
            match callbacks.iter().find(|(p, _)| *p == callback_param) {
                Some((_, cbref)) => {
                    self.callback(cbref);
                    handled.push(callback_param);
                }
                // Callback slot filled by an unresolvable value.
                None => self.gap(GapKind::UnresolvedCallback),
            }
        }
        // The invoker's other arguments can reach user code as a plain call's can
        // (`preg_replace_callback`'s subject). The throw lane does not ask yet.
        if matches!(self.knowledge, Knowledge::Effects { .. }) {
            self.note_reach(builtin_reach(self.cx, self.frame, &builtin, shapes, &handled));
        }
    }

    /// One resolved callback, wired into the lane's graph (ADR-0033): a closure or
    /// user function is an edge; a builtin callback contributes its row directly;
    /// an unknown callback is a gap.
    fn callback(&mut self, cbref: &CallbackRef) {
        let name = match cbref {
            CallbackRef::Closure(off) => {
                self.push(Edge::call(Sym::Closure(self.cx.path().to_owned(), *off)));
                return;
            }
            CallbackRef::Named(name) => name,
        };
        match self.function(name) {
            FnResolution::User(site) => {
                self.push(Edge::call(Sym::Func(self.cx.fn_decl(site).fqn.clone())));
            }
            FnResolution::Builtin(builtin) => match self.knowledge {
                Knowledge::Effects { .. } => {
                    // A builtin passed *as* a callback is invoked by the
                    // higher-order callee with arguments of its choosing, never
                    // with an lvalue of this frame — the conditional out-param row
                    // cannot apply — nor can the call site rule out what those
                    // arguments reach, and the engine calls it in coercive mode
                    // (issue #856).
                    let labels = engine::function_effects(&builtin, None, None);
                    self.function_hit(&builtin, name.simple(), labels, &[]);
                    if callback_reaches_user_code(&builtin) {
                        self.note_reach(Reach::Possible);
                    }
                }
                Knowledge::ThrowsLegacy => {
                    let throws = engine::legacy_throws(&builtin, ThrowsRole::Callback);
                    self.function_hit(&builtin, name.simple(), Vec::new(), throws);
                }
            },
            FnResolution::Unknown => self.gap(GapKind::UnresolvedCallback),
        }
    }

    // ---- method calls and `new` -------------------------------------------

    /// A method call with a resolvable receiver: the project body it runs, else
    /// (effect lane) the engine class's row or the receiver's declared bound, else
    /// (throw lane) the constructor `parent::__construct` forwards to.
    fn method_call(&mut self, receiver: &EffectRecv, method: &str) {
        let miss = match method_edge(self.cx, self.frame.class_fqn, receiver, method) {
            Ok(sym) => {
                self.push(Edge::call(sym));
                return;
            }
            Err(miss) => miss,
        };
        match self.knowledge {
            Knowledge::Effects { plugins } => self.method_fallback(plugins, receiver, method, miss),
            Knowledge::ThrowsLegacy => {
                // `parent::__construct(...)` into an engine class runs the
                // constructor `new parent` would, and answers from the same row, so
                // a project exception forwarding to the engine's stays exhaustive.
                // Any other such call taints, as it always has.
                let parent_ctor = matches!(receiver, EffectRecv::Parent)
                    && method.eq_ignore_ascii_case("__construct");
                if parent_ctor
                    && let NewTarget::Engine(fqn) =
                        resolve_new(self.cx, self.frame.class_fqn, &StaticClass::Parent)
                {
                    self.engine_constructor_throws(&fqn, "parent::__construct".to_owned());
                } else {
                    self.gap(miss);
                }
            }
        }
    }

    /// The effect lane's answer to a method call no project body answers: the
    /// builtin-class catalog gets its say (`new PDO(...)->query()` is `io.db`), and
    /// failing that the receiver may still carry a *declared* bound: an interface
    /// envelope caps what the call can do even when no body is resolvable
    /// (ADR-0067). Importing it discharges **this** site's gap and nothing else.
    /// An uncatalogued, undeclared receiver stays the gap `miss` names.
    ///
    /// The catalog leg goes first. Over a declared receiver it answers only for a
    /// final engine method (`$e->getMessage()` on a `Throwable $e`, issue #847),
    /// whose body is the one that runs, so an envelope could only restate or
    /// loosen it.
    fn method_fallback(
        &mut self,
        plugins: &steins_db::PluginFacts,
        receiver: &EffectRecv,
        method: &str,
        miss: GapKind,
    ) {
        let (cx, frame) = (self.cx, self.frame);
        let gap = match engine_method(cx, frame.class_fqn, frame.params, receiver, method) {
            EngineMethod::Row(hit) => {
                if !hit.labels.is_empty() {
                    self.push(Target::Engine(hit));
                }
                return;
            }
            EngineMethod::Gap(gap) => gap,
            EngineMethod::NotEngine => miss,
        };
        let bound = resolve_declared_bound(
            cx,
            plugins.registry(),
            frame.class_fqn,
            frame.params,
            receiver,
            method,
        );
        match bound {
            // A checked envelope answers this call site outright.
            Some(DeclaredBound::Checked(labels)) => self.declared(labels),
            // An interop envelope (ADR-0082) contributes its bound and keeps the
            // gap: ADR-0068's plugin discipline, applied to the unchecked stratum.
            // An empty bound (`@phpstan-pure`) adds no label and still claims no
            // exhaustiveness — "≤ this, and possibly more", which is the truth of
            // a claim nothing here has verified.
            Some(DeclaredBound::Interop(labels)) => {
                self.declared(labels);
                self.gap(GapKind::InteropEnvelope);
            }
            None => self.gap(gap),
        }
    }

    fn declared(&mut self, labels: Vec<String>) {
        if !labels.is_empty() {
            self.push(Target::Declared(labels));
        }
    }

    /// `new C(...)` runs `C`'s constructor (issues #804, #849): an edge to it, the
    /// catalog's row for an engine one, nothing for a class with none, and a gap
    /// for one that cannot be resolved.
    fn new_site(&mut self, class: &StaticClass) {
        match resolve_new(self.cx, self.frame.class_fqn, class) {
            NewTarget::Edge(sym) => self.push(Edge::call(sym)),
            NewTarget::Absent => {}
            NewTarget::Engine(fqn) => match self.knowledge {
                Knowledge::Effects { .. } => match engine::constructor_effects(&fqn) {
                    Some(labels) => {
                        let origin = new_origin(class);
                        self.constructor_hit(fqn, origin, labels, &[]);
                    }
                    None => self.gap(GapKind::NoEffectRow),
                },
                Knowledge::ThrowsLegacy => self.engine_constructor_throws(&fqn, new_origin(class)),
            },
            NewTarget::Unknown => self.gap(GapKind::UnknownClass),
        }
    }

    /// The throws an engine class's constructor contributes, displayed as
    /// `origin`: each class its row names. No row is a gap.
    fn engine_constructor_throws(&mut self, fqn: &str, origin: String) {
        match engine::constructor_throws(fqn) {
            Some(classes) => self.constructor_hit(fqn.to_owned(), origin, &[], classes),
            None => self.gap(GapKind::NoThrowRow),
        }
    }

    fn constructor_hit(
        &mut self,
        fqn: String,
        origin: String,
        labels: &[&'static str],
        throws: &'static [&'static str],
    ) {
        if labels.is_empty() && throws.is_empty() {
            return;
        }
        self.push(Target::Engine(Hit {
            kind: HitKind::Constructor,
            callee: fqn,
            method: "__construct".to_owned(),
            spelled: origin.clone(),
            origin,
            labels: labels.to_vec(),
            throws,
        }));
    }

    // ---- constructs and throws --------------------------------------------

    /// A language construct. The effect lane owns output, exit, `eval`, inclusion
    /// and the state constructs; the throw lane records only a `match` with no
    /// `default`, which can raise `\UnhandledMatchError` (an `Error`, unchecked, so
    /// it never enters `throw.undeclared` but surfaces in the annotate margin).
    fn construct(&mut self, construct: &ConstructKind) {
        match (self.knowledge, construct) {
            (Knowledge::Effects { .. }, ConstructKind::Output(kw)) => {
                self.push(Target::Construct { label: "io.output.buffer", spelling: kw.spelling() });
            }
            (Knowledge::Effects { .. }, ConstructKind::Exit(kw)) => {
                self.push(Target::Construct { label: "exit", spelling: kw.spelling() });
            }
            // Dynamic code (ADR-0046 amendment): the construct itself is proven —
            // `eval`, or the file read an inclusion is — and the code it runs is
            // unseen, so the body is `…?` beside it.
            (Knowledge::Effects { .. }, ConstructKind::Eval) => {
                self.push(Target::Construct { label: "eval", spelling: "eval" });
                self.gap(GapKind::UnseenCode);
            }
            (Knowledge::Effects { .. }, ConstructKind::Include(kw)) => {
                self.push(Target::Construct { label: "io.fs.read", spelling: kw.spelling() });
                self.gap(GapKind::UnseenCode);
            }
            // A state construct's label (`global.*`, `mutate.*`) is not inferred
            // yet (ADR-0055), and `{}` over one would read as proven-pure, so it
            // marks the body `…?` until it is (ADR-0055 amendment, 2026-09-26).
            (Knowledge::Effects { .. }, ConstructKind::State(_)) => {
                self.gap(GapKind::StateConstruct);
            }
            (Knowledge::ThrowsLegacy, ConstructKind::MatchNoDefault) => {
                let class = NameRef {
                    raw: "UnhandledMatchError".to_owned(),
                    kind: RefKind::FullyQualified,
                    offset: self.site.span.start,
                };
                self.thrown(&class, "new");
            }
            // `eval` and an inclusion have no throw arm (#864), and the effect lane
            // records no `match`.
            (Knowledge::ThrowsLegacy, _) | (Knowledge::Effects { .. }, ConstructKind::MatchNoDefault) => {}
        }
    }

    /// A `throw` statement (throw lane only).
    fn throw(&mut self, thrown: &ThrownKind) {
        if matches!(self.knowledge, Knowledge::Effects { .. }) {
            return;
        }
        match thrown {
            ThrownKind::New(class) => self.thrown(class, "new"),
            ThrownKind::Rethrow { caught, has_unresolvable } => {
                for cref in caught {
                    self.thrown(cref, "rethrow");
                }
                if *has_unresolvable {
                    self.gap(GapKind::UnresolvedThrow);
                }
            }
            ThrownKind::Unresolved => self.gap(GapKind::UnresolvedThrow),
        }
    }

    fn thrown(&mut self, class: &NameRef, verb: &str) {
        let fqn = self.cx.class_fqn(class);
        let display = format!("{verb} {}", last_segment(&fqn));
        self.push(Target::Thrown { class: fqn, display });
    }
}

/// The plugin channel's coloring for a statically-named call that resolved to
/// **nothing** — no project body, no catalog row (ADR-0068 §1).
///
/// Precedence is structural rather than compared: this is only ever reached from
/// the unresolved arm, so a builtin row and a project function have both already
/// won. The extra guards below are for the two shapes unresolved also covers and a
/// plugin must not speak for: an **ambiguous** name (the project does define it,
/// twice) and a **namespaced** name (a plugin manifest colors global functions,
/// which is what `acme_cache_get` is).
///
/// The answer goes in the DECLARED lane and the gap stays. That is the opposite of
/// ADR-0067's interface-envelope import, and deliberately so: an envelope is a
/// checked contract (`effect.liskov-widened` holds every analyzed implementation to
/// it), while nothing checks a plugin's assertion. Assert, never prove — so the
/// summary reads "declared `acme.cache`, and possibly more", which is the truth of
/// an unchecked claim.
fn plugin_call_labels<'p>(
    cx: &Cx,
    plugins: &'p steins_db::PluginFacts,
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
