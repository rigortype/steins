//! [`resolve_site`]: the one place a site's callee is resolved, for either lane.
//!
//! The walk below is the union of what the effect lane's and the throw lane's
//! classifiers each did on their own, with each lane's decisions selected by the
//! [`Lane`] of the [`Knowledge`] it hands in. Both lanes resolve a name through
//! the same [`Cx::resolve_function`] (a spelling is a builtin exactly when the
//! catalog knows it), read the operands' reach and the unseen code the same way,
//! and differ only in the axis they read a row on: a known name with no row on that
//! axis is a gap on it, in one place per arm, not a second walk.
//!
//! # What is knowledge and what is lane
//!
//! [`Knowledge`] is one body of facts (the catalog) read by two [`Lane`]s. The arms
//! below test the lane (`self.effects()`) where the lanes ask different questions,
//! and each says which it is:
//!
//! * **Shared knowledge.** What a spelling resolves to ([`Cx::resolve_function`]);
//!   whether a builtin's operands reach user code ([`Reach`], a gap in both lanes);
//!   unseen code (`eval`, an inclusion, a gap in both); which constructor a `new`
//!   runs and which class a chain leaves the project at; the user method an
//!   operator runs through an operand's class ([`operator`], ADR-0099 §4.3); the
//!   cause a gap names.
//! * **Lane semantics, effect only.** ADR-0063 conditional-purity contracts and
//!   their untainting edges (`user_call`); the call-site certifications and the
//!   plugin channel (`effect_unrowed`, ADR-0068); the interface envelopes of a
//!   declared receiver and the engine method rows (`method_fallback`, ADR-0067);
//!   the constructs that prove a label of their own (output, exit, `eval`'s and an
//!   inclusion's labels) and the state constructs; the invoker's own colour.
//! * **Lane semantics, throw only.** The `throw` statement, the `match` with no
//!   `default`, the throw row of an invoker, a builtin and a constructor (the only
//!   method-shaped throw rows), and a flag-gated builtin's flags.
//!
//! A site is still resolved once **per lane** (ADR-0099 §2.2's single resolution is
//! a goal the two walks do not meet yet).

use steins_syntax::{
    ArgShape, CallbackRef, ConstArgs, ConstructKind, DynamicSite, EffectRecv, NameRef, RefKind,
    RefTarget, SiteKind, SiteOrigin, StaticClass, ThrownKind,
};

use super::contract::{conditional_purity, eval_conditional_purity};
use super::engine;
use super::method::{
    EngineMethod, engine_class_of, engine_method, method_edge, new_hooks, parent_constructor_hooks,
    throwable_creation_hooks,
};
use super::operator;
use super::reach::{Frame, builtin_reach, callback_reaches_user_code, engine_method_reach};
use super::{
    Edge, GapKind, Hit, HitKind, Knowledge, Lane, NewTarget, Reach, ResolvedSite, Target,
    new_origin, resolve_new,
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

impl<'a> Resolver<'a, '_, '_> {
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
            SiteKind::Dynamic(
                DynamicSite::Call { .. } | DynamicSite::MethodCall | DynamicSite::StaticCall,
            ) => self.gap(GapKind::DynamicCallee),
            SiteKind::Dynamic(DynamicSite::New | DynamicSite::AnonymousClass) => {
                self.gap(GapKind::UnknownClass);
            }
            SiteKind::Throw(thrown) => self.throw(thrown),
            SiteKind::Construct(construct) => self.construct(construct),
            SiteKind::Operator { family, construct, receivers, member } => {
                self.operator((*family, *construct), receivers, member.as_deref());
            }
        }
    }

    /// An operator site (ADR-0099 §4.3): the user method the engine runs through
    /// an operand's class, resolved alike for both lanes.
    fn operator(
        &mut self,
        form: (steins_syntax::OperatorFamily, steins_syntax::OperatorConstruct),
        receivers: &[Option<EffectRecv>],
        member: Option<&str>,
    ) {
        let resolved = operator::resolve(self.cx, self.frame, self.site, form, receivers, member);
        self.out.targets.extend(resolved.targets);
        self.out.gaps.extend(resolved.gaps);
    }

    fn gap(&mut self, kind: GapKind) {
        self.out.gaps.insert(kind);
    }

    fn push(&mut self, target: Target) {
        self.out.targets.push(target);
    }

    /// Record what the engine may run through an operand: [`Reach::Possible`] is
    /// the gap [`GapKind::UserCodeReach`]. **Both lanes** read it (ADR-0099 §4):
    /// user code reached through an argument may do anything, throwing included.
    fn note_reach(&mut self, reach: Reach) {
        if reach == Reach::Possible {
            self.gap(GapKind::UserCodeReach);
        }
    }

    /// The reach of a call to the engine method or constructor `class::method`: what
    /// its operands can run through the engine (issue #858). The same rule as a
    /// builtin function's, read off the site's own operand shapes, in **both lanes**:
    /// a row answers what the method does, and an operand that may reach user code
    /// adds the gap beside it. Asked only where a row answers; a method with no row
    /// on the lane's axis is a gap of its own already.
    fn engine_operands(&mut self, class: &str, method: &str) {
        let operands = self.site.operands.as_deref();
        let reach = engine_method_reach(self.cx, self.frame, (class, method), operands);
        self.note_reach(reach);
    }

    /// Whether this resolution answers the effect lane.
    fn effects(&self) -> bool {
        self.knowledge.lane() == Lane::Effects
    }

    /// The effect lane's plugin channel (ADR-0068); the throw lane has none.
    fn plugins(&self) -> Option<&'a steins_db::PluginFacts> {
        let Knowledge::Catalog { plugins, .. } = self.knowledge;
        *plugins
    }

    /// The reach of a call to the builtin `name`: what its operands can run through
    /// the engine (issue #856). A name certified pure at this call's arity
    /// (`array_keys($a)`) copies and compares nothing, so no operand reaches
    /// anything.
    fn call_reach(&self, builtin: &str, args: &CallArgs<'_>) -> Reach {
        if engine::pure_at_call_arity(builtin, args.targets.map(<[_]>::len)) {
            return Reach::RuledOut;
        }
        builtin_reach(self.cx, self.frame, builtin, args.shapes, &[])
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
    /// row, ADR-0063 §2), a builtin's row on the lane's axis, or the gap of a name
    /// nothing resolves.
    ///
    /// A builtin answers with its row and, in both lanes, the gap of an argument
    /// that can reach user code the call site does not rule out (issue #856). A
    /// known name with no row on the lane's axis is a gap, never an empty answer
    /// (ADR-0099 §3.2).
    fn named_call(&mut self, name: &NameRef, args: &CallArgs<'_>) {
        match self.cx.resolve_function(name) {
            FnResolution::User(site) => self.user_call(name, site, args),
            FnResolution::Builtin(builtin) if self.effects() => {
                self.effect_builtin(name, &builtin, args);
            }
            FnResolution::Builtin(builtin) => self.throw_builtin(name, &builtin, args),
            FnResolution::Unknown => self.unresolved_call(name),
        }
    }

    fn user_call(&mut self, name: &NameRef, site: Site, args: &CallArgs<'_>) {
        let decl = self.cx.fn_decl(site);
        let sym = Sym::Func(decl.fqn.clone());
        let contract = if self.effects() {
            conditional_purity(decl.docblock.as_ref(), &decl.params)
        } else {
            None
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

    /// A builtin call in the effect lane: its colour and out-parameter labels, or,
    /// for a known name with neither, what its call site certifies.
    fn effect_builtin(&mut self, name: &NameRef, builtin: &str, args: &CallArgs<'_>) {
        if !engine::has_effect_row(builtin) {
            self.effect_unrowed(name, builtin, args);
            return;
        }
        let labels = engine::function_effects(builtin, args.targets, Some(args.consts));
        self.function_hit(builtin, name.simple(), labels, &[]);
        let reach = builtin_reach(self.cx, self.frame, builtin, args.shapes, &[]);
        self.note_reach(reach);
    }

    /// A known builtin with no colour and no out-parameter row. Two certifications
    /// answer for it at the call site, before the plugin channel does (ADR-0068
    /// precedence): `array_keys($a)` at one argument (issue #851), and a
    /// string-family name whose every reaching argument is ruled out (issue #856).
    /// Anything else is a [`GapKind::NoEffectRow`].
    fn effect_unrowed(&mut self, name: &NameRef, builtin: &str, args: &CallArgs<'_>) {
        let positional = args.targets.map(<[_]>::len);
        if engine::pure_at_call_arity(builtin, positional) {
            return;
        }
        let mut gap = GapKind::NoEffectRow;
        if engine::certified_at_call_site(builtin) {
            match builtin_reach(self.cx, self.frame, builtin, args.shapes, &[]) {
                Reach::RuledOut => return,
                // Certifiable, but an operand may reach user code: that is the gap.
                reach => {
                    self.note_reach(reach);
                    gap = GapKind::UserCodeReach;
                }
            }
        } else if engine::arity_defeated(builtin, positional) {
            gap = GapKind::ArgumentList;
        }
        self.plugin_declaration(name);
        self.gap(gap);
    }

    /// A builtin call in the throw lane: its throw row, the audited throwless
    /// table, or a gap, and the gap of an operand that may reach user code.
    fn throw_builtin(&mut self, name: &NameRef, builtin: &str, args: &CallArgs<'_>) {
        let call = Some((args.targets.map(<[_]>::len), args.consts));
        match engine::function_throws(builtin, call) {
            Ok(throws) => self.function_hit(builtin, name.simple(), Vec::new(), throws),
            Err(kind) => self.gap(kind),
        }
        let reach = self.call_reach(builtin, args);
        self.note_reach(reach);
    }

    /// A call [`Cx::resolve_function`] left unresolved: no project body and no
    /// catalog knowledge, or an ambiguous or shadowed name. The plugin channel gets
    /// the last word here (effect lane) and nowhere else (ADR-0068 precedence): a
    /// project body and a catalog name are both already spoken for.
    fn unresolved_call(&mut self, name: &NameRef) {
        self.plugin_declaration(name);
        self.gap(GapKind::UnknownFunction);
    }

    /// The effect lane's plugin channel (ADR-0068): a plugin's declaration for a
    /// global function no row covers enters the declared lane, and the caller's gap
    /// stays.
    fn plugin_declaration(&mut self, name: &NameRef) {
        let Some(plugins) = self.plugins() else { return };
        if let Some(labels) = plugin_call_labels(self.cx, plugins, name) {
            self.push(Target::Declared(labels.to_vec()));
        }
    }

    // ---- higher-order calls and callbacks ---------------------------------

    /// A call handing over resolvable callbacks (ADR-0033): the invoker's own row
    /// and the callback's target, or the base call resolved normally for a callee
    /// that is not an invoker.
    fn higher_order(&mut self, name: &NameRef, callbacks: &[(usize, CallbackRef)]) {
        let site = self.site;
        let targets = Some(site.ref_targets.as_deref().unwrap_or(&[]));
        let shapes = Some(site.operands.as_deref().unwrap_or(&[]));
        let invoker = match self.cx.resolve_function(name) {
            FnResolution::Builtin(builtin) => {
                engine::invoker_callback_param(&builtin).map(|param| (builtin, param))
            }
            _ => None,
        };
        let Some((builtin, callback_param)) = invoker else {
            // Not a known invoker: the callee is a normal call, unless it is a user
            // function declaring a conditional-purity contract (ADR-0063 §2).
            let args = CallArgs { targets, consts: &site.const_args, shapes, callbacks };
            self.named_call(name, &args);
            return;
        };
        let arg_count = site.ref_targets.as_ref().map_or(0, Vec::len);
        // ADR-0063 P1: the call's effect is the invoker's OWN catalog color joined
        // with the envelope of the callback it immediately invokes. The own-color
        // leg is unconditional — an unresolvable (or absent) callback never
        // *weakens* the invoker's declared color; it only adds a gap. The throw
        // lane reads the invoker's own throw row the same way, and a row the audit
        // has not given it is a gap there.
        if self.effects() {
            let labels = engine::function_effects(&builtin, targets, Some(&site.const_args));
            self.function_hit(&builtin, name.simple(), labels, &[]);
        } else {
            let call = Some((site.ref_targets.as_ref().map(Vec::len), &site.const_args));
            match engine::function_throws(&builtin, call) {
                Ok(throws) => self.function_hit(&builtin, name.simple(), Vec::new(), throws),
                Err(kind) => self.gap(kind),
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
        // (`preg_replace_callback`'s subject).
        self.note_reach(builtin_reach(self.cx, self.frame, &builtin, shapes, &handled));
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
        match self.cx.resolve_function(name) {
            FnResolution::User(site) => {
                self.push(Edge::call(Sym::Func(self.cx.fn_decl(site).fqn.clone())));
            }
            FnResolution::Builtin(builtin) => self.builtin_callback(name, &builtin),
            FnResolution::Unknown => self.gap(GapKind::UnresolvedCallback),
        }
    }

    /// A builtin passed *as* a callback is invoked by the higher-order callee with
    /// arguments of its choosing, never with an lvalue of this frame — the
    /// conditional out-param row cannot apply, nor a flags argument be read — nor
    /// can the call site rule out what those arguments reach, and the engine calls
    /// it in coercive mode (issue #856). Its row on the lane's axis answers as it
    /// does for a plain call, and a known name with none is a gap.
    fn builtin_callback(&mut self, name: &NameRef, builtin: &str) {
        if self.effects() {
            if engine::has_effect_row(builtin) {
                let labels = engine::function_effects(builtin, None, None);
                self.function_hit(builtin, name.simple(), labels, &[]);
            } else {
                self.gap(GapKind::NoEffectRow);
            }
        } else {
            match engine::function_throws(builtin, None) {
                Ok(throws) => self.function_hit(builtin, name.simple(), Vec::new(), throws),
                Err(kind) => self.gap(kind),
            }
        }
        if callback_reaches_user_code(builtin) {
            self.note_reach(Reach::Possible);
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
        // A class declaring `__call` answers the method it lacks (ADR-0099 §4.3).
        if miss == GapKind::MethodNotFound {
            let magic = operator::magic_call_edges(self.cx, self.frame.class_fqn, receiver);
            if !magic.is_empty() {
                magic.into_iter().for_each(|sym| self.push(Edge::call(sym)));
                return;
            }
        }
        if let Some(plugins) = self.plugins() {
            self.method_fallback(plugins, receiver, method, miss);
            return;
        }
        // `parent::__construct(...)` into an engine class runs the constructor
        // `new parent` would, and answers from the same row, so a project exception
        // forwarding to the engine's stays exhaustive. Any other such call taints,
        // as it always has.
        let parent_ctor =
            matches!(receiver, EffectRecv::Parent) && method.eq_ignore_ascii_case("__construct");
        if parent_ctor
            && let NewTarget::Engine(fqn) =
                resolve_new(self.cx, self.frame.class_fqn, &StaticClass::Parent)
        {
            self.hooked_engine_code(parent_constructor_hooks(self.cx, self.frame.class_fqn));
            self.engine_constructor_throws(&fqn, "parent::__construct".to_owned());
            return;
        }
        // A method of an engine class the catalog knows has no throw row (the only
        // method-shaped throw rows are constructors): the site names the missing
        // row, as the effect lane names its own, instead of the chain's exit.
        // A declared receiver stays a gap of its own (ADR-0099 §8, ADR-0067).
        let engine = match miss {
            GapKind::DeclaredReceiver => None,
            _ => {
                let (class_fqn, params) = (self.frame.class_fqn, self.frame.params);
                engine_class_of(self.cx, class_fqn, params, receiver, method)
            }
        };
        match engine {
            Some(fqn) => self.gap(engine::missing_row(&fqn, GapKind::NoThrowRow)),
            None => self.gap(miss),
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
            EngineMethod::Row { hit, hooked } => {
                self.engine_operands(&hit.callee, &hit.method);
                self.hooked_engine_code(hooked);
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
        let target = resolve_new(self.cx, self.frame.class_fqn, class);
        if matches!(target, NewTarget::Engine(_)) {
            self.hooked_engine_code(new_hooks(self.cx, self.frame.class_fqn, class));
        } else {
            self.hooked_engine_code(throwable_creation_hooks(self.cx, self.frame.class_fqn, class));
        }
        match target {
            NewTarget::Edge(sym) => self.push(Edge::call(sym)),
            NewTarget::Absent => {}
            NewTarget::Engine(fqn) if self.effects() => match engine::constructor_effects(&fqn) {
                Some(labels) => {
                    self.engine_operands(&fqn, "__construct");
                    let origin = new_origin(class);
                    self.constructor_hit(fqn, origin, labels, &[]);
                }
                None => self.gap(engine::missing_row(&fqn, GapKind::NoEffectRow)),
            },
            NewTarget::Engine(fqn) => self.engine_constructor_throws(&fqn, new_origin(class)),
            NewTarget::Unknown(kind) => self.gap(kind),
        }
    }

    /// The engine's own code, run on an object whose class chain hooks a property,
    /// may run the hook: the gap beside the engine row (ADR-0099 §4.2, issue #875).
    fn hooked_engine_code(&mut self, hooked: bool) {
        if hooked {
            self.gap(GapKind::OperatorMagicProperty);
        }
    }

    /// The throws an engine class's constructor contributes, displayed as
    /// `origin`: each class its row names. No row is a gap.
    fn engine_constructor_throws(&mut self, fqn: &str, origin: String) {
        match engine::constructor_throws(fqn) {
            Some(classes) => {
                self.engine_operands(fqn, "__construct");
                self.constructor_hit(fqn.to_owned(), origin, &[], classes);
            }
            None => self.gap(engine::missing_row(fqn, GapKind::NoThrowRow)),
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

    /// A language construct. The effect lane owns output, exit and the state
    /// constructs; both lanes read unseen code (`eval`, an inclusion) as a gap, and
    /// the throw lane records the `match` with no `default`, which can raise
    /// `\UnhandledMatchError` (an `Error`, unchecked, so it never enters
    /// `throw.undeclared` but surfaces in the annotate margin).
    fn construct(&mut self, construct: &ConstructKind) {
        // Dynamic code (ADR-0046 amendment): the code it runs is unseen, and may
        // throw anything as it may do anything, so the body is `…?` in both lanes.
        if matches!(construct, ConstructKind::Eval | ConstructKind::Include(_)) {
            if self.effects() {
                // The construct itself is proven in the effect lane — `eval`, or
                // the file read an inclusion is — and the gap sits beside it.
                match construct {
                    ConstructKind::Include(kw) => self.push(Target::Construct {
                        label: "io.fs.read",
                        spelling: kw.spelling(),
                    }),
                    _ => self.push(Target::Construct { label: "eval", spelling: "eval" }),
                }
            }
            self.gap(GapKind::UnseenCode);
            return;
        }
        match (self.effects(), construct) {
            (true, ConstructKind::Output(kw)) => {
                self.push(Target::Construct { label: "io.output.buffer", spelling: kw.spelling() });
            }
            (true, ConstructKind::Exit(kw)) => {
                self.push(Target::Construct { label: "exit", spelling: kw.spelling() });
            }
            // A state construct's label (`global.*`, `mutate.*`) is not inferred
            // yet (ADR-0055), and `{}` over one would read as proven-pure, so it
            // marks the body `…?` until it is (ADR-0055 amendment, 2026-09-26).
            (true, ConstructKind::State(_)) => self.gap(GapKind::StateConstruct),
            (false, ConstructKind::MatchNoDefault) => {
                let class = NameRef {
                    raw: "UnhandledMatchError".to_owned(),
                    kind: RefKind::FullyQualified,
                    offset: self.site.span.start,
                };
                self.thrown(&class, "new");
            }
            // The effect lane records no `match`, and the throw lane none of the
            // constructs the effect lane owns alone.
            _ => {}
        }
    }

    /// A `throw` statement (throw lane only).
    fn throw(&mut self, thrown: &ThrownKind) {
        if self.effects() {
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
