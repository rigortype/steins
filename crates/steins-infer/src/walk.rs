//! The linear-trace walk: [`analyze_scope`] over one scope with a given initial
//! env, [`WalkCx`], the statement loop [`walk_trace`], dead-path marking and the
//! by-value survivor / value-stratum helpers.

use std::collections::{HashMap, HashSet};

use steins_domain::{Fact, Key, Val};
use steins_phpdoc::Type as PType;
use steins_syntax::{
    ArgValue, CallExpr, Callee, CondExpr, InvalidatedVar, NameRef, NamedArg, NativeType, Receiver,
    RefKind, Scope, ScopeOwner, Span, Stmt, StmtKind,
};

use crate::fold::Folder;
use crate::is_dump_family_fqn;
use crate::annotate::LineFact;
use crate::assert_harness::{ASSERT_SINK, record_subject_probe};
use crate::asserts::apply_stmt_asserts;
use crate::assign::apply_assign;
use crate::branch::{GuardChainCoverage, guard_chain_subject, walk_if, walk_match};
use crate::builtin_returns::{
    ResourceEffects, apply_resource_effects, escape_mentioned_resources, resource_call_effects,
};
use crate::cx::Cx;
use crate::descent::{
    apply_call_escape_and_sweep, checkable_calls, return_heap_object, return_value_fact,
    scope_class,
};
use crate::dispatch::resolve_call_target;
use crate::dump::{
    ASSERT_TYPE_FQN, adopted_trace_docblock, emit_trace_annotations, name_reaches_global_var_dump,
    resolved_fn_fqn,
};
use crate::env::{AllocId, Descent, ExitContribution, HeapRes, Known, Store, Stratum, SummaryCtx};
use crate::foreach_check::check_foreach_subject;
use crate::heap::{apply_prop_assign, seed_declared_param_object, seed_this_object};
use crate::loops::walk_loop;
use crate::operands::check_operand_sites;
use crate::out_params::{
    apply_produced_places, apply_stmt_out_param_seeds, stmt_out_param_seeds, stmt_produced_places,
};
use crate::predicates::apply_type_narrowing;
use crate::project::{Diagnostic, FnResolution};
use crate::refine::{
    apply_class_narrowing, apply_inline_var_casts, apply_refinements, collect_guard_calls_any,
    expand_enum_case_arms, seed_contract_arms, seed_fact, seed_refined_scalar_fact, seed_shape_fact,
    then_refinements,
};
use crate::return_arms::return_envelope_arms;
use crate::shapes::{apply_offset_append, apply_offset_write, apply_shape_narrowing};
use crate::stmt_calls::check_stmt_calls;
use crate::stmt_checks::{check_read_positions, check_return_value};

/// Walk one scope's trace with a given initial environment.
#[allow(clippy::too_many_arguments)]
pub(crate) fn analyze_scope(
    cx: &Cx,
    folder: &mut dyn Folder,
    scope: &Scope,
    mut env: HashMap<String, Known>,
    mut store: Store,
    this_exact: Option<String>,
    mut descent: Option<Descent<'_>>,
    mut facts: Option<&mut Vec<LineFact>>,
    dead_out: Option<&mut Vec<Span>>,
    // Spans of default-less `match` statements this walk proved do not cover the
    // subject's Verified domain (ADR-0088 §5, issue #433) — the [`WalkCx::uncovered_matches`]
    // out-channel, same `Some`-only-for-the-plain-walk discipline as `dead_out`.
    uncovered_out: Option<&mut Vec<Span>>,
    ret_exits: Option<&mut Vec<ExitContribution>>,
    // The `$this`-snapshot out-channel (ADR-0057 C2, generalized by the 2026-08-17
    // amendment's D3): `Some` exactly where this walk's `$this` was seeded from a
    // caller object, so every exit records what the walk left it holding. Meaningful
    // only alongside `ret_exits`, which is what turns summary collection on at all.
    this_exits: Option<&mut Vec<ExitContribution>>,
    out: &mut Vec<Diagnostic>,
) {
    let enclosing_class = scope_class(scope);

    // The owning function/method's native return type, resolved once. Checking runs
    // only in the plain per-scope pass (`descent.is_none()`): a descent rebinds the
    // callee's params, not its return, so re-checking there would only duplicate the
    // per-scope finding.
    let ret_info: Option<(&NativeType, String)> =
        if descent.is_none() { cx.scope_return(scope) } else { None };
    // Same restriction for the `@return` phpdoc envelope; skipped where the native
    // return check already fired (no double-report).
    let ret_phpdoc: Option<(PType, String)> =
        if descent.is_none() { cx.scope_return_phpdoc(scope) } else { None };

    // Native-type parameter seeding (Feature B): sound in both strict and coercive
    // modes since the engine coerces or throws at entry, so inside the body an
    // `int $x` param IS an int post-coercion. A descent already binds params the
    // caller supplied; only params still absent from the env get seeded here.
    let scope_params = cx.scope_params(scope);
    if let Some(params) = scope_params {
        for p in params {
            if env.contains_key(&p.name) || store.is_bound(&p.name) {
                continue;
            }
            if let Some(fact) = seed_fact(p) {
                env.insert(p.name.clone(), Known::value(fact, 0, Some("native parameter type".to_owned())));
            }
        }
    }

    // One parse of the owning declaration's docblock serves both parameter-seed
    // lanes below — the arm lane and the declared object — so a documented scope is
    // never read twice for the same envelopes.
    let param_envelopes = scope_params.and_then(|_| cx.scope_envelopes(scope));

    // Contract-fact seeding (ADR-0052 §9), the canonical entry-state contribution
    // (ADR-0048 §3): per declared parameter, the native member list (`Verified`)
    // refined by the declared `@param` phpdoc envelope (`Asserted`, ADR-0037 trust
    // order). The arm lane lives in the walk-local `Store`; a descent that already
    // bound a param's value gets no lane. No other narrowing carrier (guard facts,
    // members, static-prop channels) contributes to entry state.
    if descent.is_none()
        && let Some(params) = scope_params
    {
        let envelopes = param_envelopes.as_ref();
        for p in params {
            if store.contract.contains_key(&p.name) {
                continue;
            }
            let phpdoc = envelopes.and_then(|e| {
                // An assertion-target `@param` states a post-condition, not the
                // parameter's declared type — never seed a lane from it.
                if e.is_assert_target(&p.name) { None } else { e.param(&p.name) }
            });
            // Resolve phpdoc class arms in the param's namespace context (its offset
            // falls in the same region as the `@param` docblock), matching the FQNs
            // the `instanceof` subtrahend and S6's `find_class` use.
            let resolve = |n: &str| {
                cx.resolve_pclass(cx.cur, p.span.start, n).trim_start_matches('\\').to_ascii_lowercase()
            };
            if let Some(mut arms) = seed_contract_arms(p, phpdoc, &resolve)
                && !arms.is_empty()
            {
                // The finite enum domain (issue #429), planted here rather than
                // inside the shared refinement so the trust question is already
                // settled when it is asked: only a `Verified` class arm expands.
                expand_enum_case_arms(cx, &mut arms);
                // Abstract array stratum's entry state (ADR-0062 S3): a lane whose
                // array vocabulary collapsed to ONE arm also seeds the value lane
                // with that arm's shape fact. Multi-arm lanes seed nothing — the
                // shape∪shape union lives in the arm lane until a guard subtracts it
                // to one (A-G3, S4's job).
                if !env.contains_key(&p.name)
                    && let Some(fact) = seed_shape_fact(&arms)
                {
                    env.insert(
                        p.name.clone(),
                        // ALWAYS `Asserted` even where the arm itself is `Verified`
                        // (a native `array $x`): A-G9's corollary is normative —
                        // shape-derived facts never feed proof-layer findings.
                        Known::value_strat(
                            fact,
                            0,
                            Some("declared array shape".to_owned()),
                            Stratum::Asserted,
                        ),
                    );
                }
                // The scalar half (issue #242): the native pass already planted the
                // coarse `Fact::General`, which outranks the arm lane, so a declared
                // refinement must REPLACE it, not yield. The `== native` test
                // overwrites only the seed this same entry pass planted, never a
                // descent-bound value or guard fact.
                else if let Some(native) = seed_fact(p)
                    && env.get(&p.name).is_some_and(|k| k.fact.as_ref() == Some(&native))
                    && let Some((fact, stratum)) = seed_refined_scalar_fact(p, &native, &arms)
                {
                    env.insert(
                        p.name.clone(),
                        Known::value_strat(
                            fact,
                            0,
                            Some("declared parameter refinement".to_owned()),
                            stratum,
                        ),
                    );
                }
                store.contract.insert(p.name.clone(), arms);
            }
        }
    }

    // Seed the `$this` object in a method scope (ADR-0036): props/readonly from the
    // class surface, only when the class declares tracked properties (otherwise
    // `$this` stays unbound). A descent that already bound `this` is left untouched —
    // that is the receiver leg's seam (ADR-0086 §3): a method call on an exact
    // `Receiver::Var` hands the callee a copy of the receiver's own object, props and
    // carries included, and re-seeding the class shell over it would throw exactly the
    // knowledge the crossing bought away. Every other receiver arrives here with `this`
    // unbound and is seeded below, as before.
    if let Some(class) = enclosing_class
        && !store.is_bound("this")
    {
        // G1: `$this`'s heap class is a lower bound (any subclass instance may be
        // running this method) unless exactness is locally provable — a descent
        // that proved the exact receiver (`this_exact`), or the enclosing class
        // itself when `final` or an enum.
        let (this_class, exact): (&str, bool) = match this_exact.as_deref() {
            Some(exact) => (exact, true),
            None => (class, cx.this_class_exact(class)),
        };
        if let Some(obj) = seed_this_object(cx, this_class, exact) {
            let id = store.heap.keys().copied().max().map_or(0, |m| m + 1);
            store.heap.insert(id, obj);
            store.refs.insert("this".to_owned(), id);
        }
    }

    // Seed the **declared** parameter objects (ADR-0032's 2026-08-16 amendment,
    // issue #388): a parameter that is an object by declaration enters its scope on
    // the heap wherever no ADR-0086 copy landed — the plain per-scope pass, which
    // has never given a parameter a `HeapObj` at all, and a descent whose argument
    // resolved to no object. A parameter the caller already bound is left alone in
    // both lanes: a copied object in `refs` and a proven value in `env` are each
    // stronger than the declaration that would have stood in for them.
    if let Some(params) = scope_params {
        let shadow = cx.scope_template_shadow(scope);
        for p in params {
            if env.contains_key(&p.name) || store.is_bound(&p.name) {
                continue;
            }
            // An assertion-target `@param` states a post-condition, not the
            // parameter's declared type — read as absent here exactly as the arm
            // lane reads it.
            let phpdoc = param_envelopes.as_ref().and_then(|e| {
                if e.is_assert_target(&p.name) { None } else { e.param(&p.name) }
            });
            if let Some(obj) = seed_declared_param_object(cx, p, phpdoc, &shadow) {
                let id = store.heap.keys().copied().max().map_or(0, |m| m + 1);
                store.heap.insert(id, obj);
                store.refs.insert(p.name.clone(), id);
            }
        }
    }

    // The allocation counter starts past any id already in the store (the seeded
    // `$this`), so a fresh `new`/`clone` never collides with it.
    let alloc_start = store.heap.keys().copied().max().map_or(0, |m| m + 1);
    // Return-fact summary collection (ADR-0057 T0): active only when the caller
    // requested exits. Native return arms resolved once, as the A2 drop oracle — the
    // lowered `NativeType` where the hint had one, else the enforced top of a bare
    // `: array`/`: object`/`: iterable` (ADR-0057 note, issue #603), which PHP checks
    // at this very boundary and which therefore drops a non-array exit just as `: int`
    // drops a string one.
    let summary = ret_exits.as_ref().map(|_| SummaryCtx {
        native: return_envelope_arms(
            cx.scope_return(scope).map(|(ty, _)| ty),
            cx.scope_return_top(scope),
        ),
        exits: std::cell::RefCell::new(Vec::new()),
        this_exits: this_exits.as_ref().map(|_| std::cell::RefCell::new(Vec::new())),
    });
    let w = WalkCx {
        cx,
        scope,
        enclosing_class,
        this_exact: this_exact.as_deref(),
        ret_info: &ret_info,
        ret_phpdoc: &ret_phpdoc,
        dead: std::cell::RefCell::new(Vec::new()),
        uncovered_matches: std::cell::RefCell::new(Vec::new()),
        alloc: std::cell::Cell::new(alloc_start),
        summary,
    };
    let _frame = FrameGuard::enter(scope);
    let flow =
        walk_trace(&w, folder, &scope.stmts, &mut env, &mut store, &mut descent, &mut facts, false, out);
    if let Some(out) = ret_exits
        && let Some(sc) = w.summary
    {
        let mut exits = sc.exits.into_inner();
        let mut this_out = sc.this_exits.map(std::cell::RefCell::into_inner);
        // Untyped fallthrough is PHP's implicit `return null` (ADR-0057 §5). The
        // test is the **raw** written return hint (`ret_hint`), not whether Steins
        // lowers a representable `NativeType`: `void` / `never` / `: object` /
        // `: array` all leave `scope_return` as `None` but must not contribute null.
        // A written non-void hint that falls through is a boundary TypeError —
        // nothing is contributed (same as an A2-dropped exit). Generators refuse
        // summaries entirely (`join_summary`); they also skip fallthrough null.
        if flow == Flow::FellThrough {
            // The fall-through is a constructor's NORMAL exit and an ordinary
            // method's last one (ADR-0057 C2/D3): the `$this` the joined paths
            // reaching here left behind. Where `this` is no longer in the store — a
            // `Barrier` cleared it — the exit contributes the value floor, which per
            // §2.5 ends the component and lands the call on its decline floor.
            if let Some(te) = &mut this_out {
                te.push(this_exit_contribution(&store));
            }
            if scope.ret_hint.is_none() && !scope.is_generator {
                exits.push(ExitContribution::Fact(Fact::Singleton(Val::Null), Stratum::Verified));
            }
        }
        *out = exits;
        if let Some((sink, collected)) = this_exits.zip(this_out) {
            *sink = collected;
        }
    }
    if let Some(sink) = dead_out {
        sink.extend(w.dead.into_inner());
    }
    if let Some(sink) = uncovered_out {
        sink.extend(w.uncovered_matches.into_inner());
    }
}

/// One constructor-walk exit's contribution (ADR-0057 C2): the snapshot of the
/// callee's `$this`, or the value floor where `$this` is no longer in the store —
/// which per §2.5 ends the heap summary and drops the `new` site to the ADR-0086 §4
/// lexical floor. Shared by the `return;` classifier and the fall-through.
fn this_exit_contribution(store: &Store) -> ExitContribution {
    match store.obj_of("this") {
        Some(obj) => ExitContribution::Heap(Box::new(obj.clone())),
        None => ExitContribution::Floor,
    }
}

thread_local! {
    /// Whether the scope being walked is the top-level one — the frame whose
    /// locals are globals, so a builtin that runs userland through a
    /// non-`callable` argument (a generator under `iterator_to_array`, a
    /// `JsonSerializable` under `json_encode`) can rebind them by `global`.
    /// ADR-0070's mined certification is refused there (issue #637's adversarial
    /// review); set for the length of [`analyze_scope`]'s walk and restored
    /// after, so a descent into a callee reads its own frame.
    static FRAME_TOP_LEVEL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// RAII for [`FRAME_TOP_LEVEL`]: the previous value comes back on drop.
struct FrameGuard(bool);

impl FrameGuard {
    fn enter(scope: &Scope) -> Self {
        let prev = FRAME_TOP_LEVEL.replace(matches!(scope.owner, ScopeOwner::TopLevel));
        Self(prev)
    }
}

impl Drop for FrameGuard {
    fn drop(&mut self) {
        FRAME_TOP_LEVEL.set(self.0);
    }
}

/// Whether the frame being walked is the top-level one, whose locals ARE the
/// globals — so a name in it can be rebound by any code that runs, through
/// `global $x` or `$GLOBALS['x']`, without the caller's source mentioning it.
/// See [`FRAME_TOP_LEVEL`].
pub(crate) fn frame_is_top_level() -> bool {
    FRAME_TOP_LEVEL.get()
}

/// Whether the mined by-value arm may certify in the frame being walked — every
/// frame but the top-level one. See [`FRAME_TOP_LEVEL`] and
/// [`steins_catalog::by_value_arg_frame`].
pub(crate) fn mined_arm_admitted() -> bool {
    !frame_is_top_level()
}

/// Whether a walked (sub-)trace runs off its end (its successor is reachable) or
/// terminates (`return`/`throw`/`exit`, a statement-position call to a `: never`
/// callee, or an `if` where no branch falls through).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Flow {
    FellThrough,
    Terminated,
}

/// Whether a statement-position call **provably** never returns, so the trace stops
/// at it exactly as a `throw` does (issue #599, ADR-0081 §9's leg 1).
///
/// The claim is the callee's own **native** `: never`, and nothing else. PHP fatals
/// if a `never` function returns, so the declaration is a runtime-enforced contract
/// — the `Verified` stratum ADR-0037 reserves for what the engine itself holds to,
/// which is what entitles this to *silence* the code after the call (ADR-0046: this
/// prunes findings, so the premise has to be the strong one).
///
/// Four declines, each a way the written name can mean something else at runtime:
///
/// * **A docblock's `@return never` is not read here.** It is `Asserted` (ADR-0069),
///   a comment's claim, and pruning on it would let a lying comment delete real code
///   from the analysis. `FunctionDecl::ret`/`MethodDecl::ret` cannot answer the
///   question either — `never`, `void`, `array` and an absent hint all lower to
///   `None` — so the hint is read back as WRITTEN off ADR-0078's `ret_span`, the
///   same way `purity::returns_void` reads the one signature fact it needs.
/// * **A conditional declaration declines** (ADR-0049 A2i, the
///   `function_exists`-guarded polyfill shape): which body binds is a load-order
///   fact, so the `never` on the one this index happens to hold proves nothing.
///   Ungated by the dam on purpose — the arity check re-dams because it is deciding
///   whether to *fire*, and this decides whether to go *quiet*.
/// * **A namespaced unqualified name that resolved through PHP's GLOBAL fallback
///   declines.** `namespace A; foo();` binds `A\foo` if anything defines it before
///   the call and `\foo` otherwise, so a global match is the answer only until it
///   is not. A `use function` import is compile-time and wins outright, so it is
///   not this shape and is kept.
/// * **Dispatch is the walk's existing machinery** ([`resolve_call_target`]): an
///   unresolved receiver, a `static::` late bound target, a property-fetch
///   receiver, a trait-using or off-project chain, an abstract declaration and a
///   privately-invisible method all answer `None` there and therefore fall through
///   here, exactly as they do today.
///
/// An **overridable** method's `never` is nevertheless trustworthy once resolved,
/// and that is a property of the bottom type rather than of the resolution: PHP's
/// return-type covariance admits only narrowing, and nothing is narrower than
/// `never`, so every override of a `: never` method must itself declare `never`.
/// The `final_or_private` guard [`resolve_call_target`] applies to `$this->`/`self::`
/// is therefore stricter than this claim needs — kept anyway, because it is the
/// dispatch rule the rest of the walk is calibrated on and loosening it here would
/// be a second, unmeasured change.
///
/// Two catalog gates deliberately do **not** apply, both because no catalog is read:
/// ADR-0052 A11's version-skew demotion ([`Cx::a11_demote_catalog`]) guards
/// catalog-backed is-a edges, and the mined [`steins_catalog::declared_return`] table
/// is `Asserted` by construction and could not premise a prune even if it carried a
/// row. It carries none: reflected against PHP 8.5.9, `exit` and `die` are the only
/// internal functions declaring `never`, and both lower to [`StmtKind::Exit`], a
/// terminator already. The builtin arm of this predicate is therefore empty by
/// measurement rather than by omission.
///
/// [`resolve_call_target`]: crate::dispatch::resolve_call_target
fn call_never_returns(w: &WalkCx, call: &CallExpr, store: &Store) -> bool {
    let cx = w.cx;
    match &call.receiver {
        Callee::Function(_) => {
            let Some(r) = call.callee_ref.as_ref() else { return false };
            let FnResolution::User(site) = cx.resolve_function(r) else { return false };
            let decl = cx.fn_decl(site);
            // The hint first, the resolution rule second: the hint is a span read
            // and the rule allocates, and every call in the corpus that is not a
            // `never` call stops at the read.
            !decl.conditional
                && declares_never(cx, site.file, decl.ret_span)
                && namespace_binding_is_settled(cx, r, &decl.fqn)
        }
        Callee::Method { .. } | Callee::Static { .. } => {
            let Some(t) = resolve_call_target(
                cx,
                &call.receiver,
                store,
                w.this_exact,
                w.enclosing_class,
                w.scope.poisoned,
            ) else {
                return false;
            };
            if t.declaring_class.conditional {
                return false;
            }
            declares_never(cx, t.class_file, t.method.ret_span)
        }
        // A constructor cannot declare a return type at all, and a dynamic callee
        // names no declaration to read.
        Callee::Construct { .. } | Callee::DynamicVar(_) | Callee::Dynamic => false,
    }
}

/// Whether the native return hint at `ret_span` is written `never` in `file`.
///
/// `None` — no hint written at all — is not `never`; see [`call_never_returns`] for
/// why the docblock gets no say here.
fn declares_never(cx: &Cx, file: usize, ret_span: Option<Span>) -> bool {
    let Some(span) = ret_span else { return false };
    cx.units[file].tree.text_at(span).is_some_and(|t| t.trim().eq_ignore_ascii_case("never"))
}

/// Whether an unqualified call reference inside a namespace bound to the function it
/// resolved to for a reason that holds at every load order (see
/// [`call_never_returns`]'s third decline).
///
/// True for everything but the one hazard: a bare name written inside a non-empty
/// namespace, not covered by a `use function` import, that the project index matched
/// only in the global namespace. `resolved_fqn` is the resolved declaration's own
/// lowercase FQN, so the comparison is against what PHP would have preferred.
fn namespace_binding_is_settled(cx: &Cx, r: &NameRef, resolved_fqn: &str) -> bool {
    if r.kind != RefKind::Unqualified {
        return true;
    }
    let ctx = cx.tree().ctx_at(r.offset);
    let name = r.raw.to_ascii_lowercase();
    if ctx.namespace.is_empty() || ctx.fn_imports.contains_key(&name) {
        return true;
    }
    resolved_fqn == format!("{}\\{name}", ctx.namespace.to_ascii_lowercase())
}

/// The immutable context shared across a scope's recursive branch walk (ADR-0031).
pub(crate) struct WalkCx<'a, 'w> {
    pub(crate) cx: &'w Cx<'a>,
    pub(crate) scope: &'w Scope,
    pub(crate) enclosing_class: Option<&'w str>,
    pub(crate) this_exact: Option<&'w str>,
    pub(crate) ret_info: &'w Option<(&'a NativeType, String)>,
    pub(crate) ret_phpdoc: &'w Option<(PType, String)>,
    /// Proven-dead statement spans discovered during this walk. Only the plain
    /// per-scope walk's regions are universal truths — a binding descent's dead
    /// branches are dead only for that binding, so descents discard theirs.
    pub(crate) dead: std::cell::RefCell<Vec<Span>>,
    /// Spans of default-less `match` statements proven, on this walk, not to
    /// cover the subject's Verified domain (ADR-0088 §5, issue #433) — the
    /// dataflow half of the `throw.undeclared` `\UnhandledMatchError` gate. Same
    /// plain-walk-only discipline as [`Self::dead`]: a descent's hypothetical
    /// bindings are per-call-site, not a fact about the declaration, so a
    /// descent never pushes here (see [`walk_match`]'s `descent.is_none()`
    /// guard). The throw system correlates these against
    /// [`ThrowKind::New`]'s own span for the same construct (both trace back to
    /// the same CST `Match` node's start offset), so nothing pairs this set
    /// with a file index here — the caller does that.
    pub(crate) uncovered_matches: std::cell::RefCell<Vec<Span>>,
    /// A monotone allocation-id counter for this scope walk (ADR-0036). Shared
    /// across branch clones (they clone the `Store`, not this cell), so a `new` in
    /// one branch never collides with one in another that later joins.
    pub(crate) alloc: std::cell::Cell<AllocId>,
    /// Return-fact summary collection (ADR-0057 T0): `Some` only while a callee
    /// body is walked for its summary. Each returning exit pushes its contribution
    /// here; the join happens in [`descend`].
    ///
    /// [`descend`]: crate::descent::descend
    pub(crate) summary: Option<SummaryCtx>,
}

impl WalkCx<'_, '_> {
    /// Mint a fresh allocation id for a `new`/`clone`.
    pub(crate) fn fresh_id(&self) -> AllocId {
        let id = self.alloc.get();
        self.alloc.set(id + 1);
        id
    }
}

/// Record every top-level statement span of the given traces as proven dead.
/// Nested constructs' calls lie within their statement's span, so containment
/// filtering over these covers them. (Skipped `elseif` *conditions* are not
/// yet marked — a literal-arg call inside one is vanishingly rare; TODO.)
pub(crate) fn mark_dead(w: &WalkCx, traces: &[&[Stmt]]) {
    let mut dead = w.dead.borrow_mut();
    for trace in traces {
        for stmt in *trace {
            dead.push(stmt.span);
        }
    }
}

/// Record one source extent as proven dead — an expression PHP's own evaluation
/// order proves is never reached (ADR-0052 §6: a `&&`/`||` operand the left side
/// short-circuits past, the untaken arm of a decided ternary, the right operand of
/// a `??` whose left is proven set-and-non-null).
///
/// Shares one consumer with [`mark_dead`], [`in_dead`], and one discipline: only a
/// decided verdict is recorded, and only the plain per-scope walk's regions escape
/// (a binding descent discards its own — its verdicts hold for that caller only).
pub(crate) fn mark_dead_span(w: &WalkCx, span: Span) {
    w.dead.borrow_mut().push(span);
}

/// Record every call this condition carries as dead. Used where a whole operand of
/// `&&`/`||` is proven unevaluated: the operand itself has no span (a [`CondExpr`]
/// is a lowered form, not a CST node), but its calls do.
///
/// A non-call site inside such an operand (a class reference, a constant fetch)
/// keeps its own filter and is NOT covered — a known residue, not papered over.
pub(crate) fn mark_dead_cond_calls(w: &WalkCx, cond: &CondExpr) {
    let calls = collect_guard_calls_any(cond);
    if calls.is_empty() {
        return;
    }
    let mut dead = w.dead.borrow_mut();
    for call in calls {
        dead.push(call.span);
    }
}

/// Whether a byte position falls inside any proven-dead region.
pub(crate) fn in_dead(dead: &[Span], pos: u32) -> bool {
    dead.iter().any(|s| s.start <= pos && pos < s.end)
}

/// Apply a control-flow construct's write/read sets and hidden-exit floor — the
/// whole of what [`StmtKind::Opaque`] does, and the whole of what a structured
/// [`StmtKind::While`] does to the code that follows it (ADR-0027 ratchet: forget
/// only touched/branched variables, not the whole env).
///
/// When the subtree may `return`, a summary walk contributes the declared floor so
/// hidden exits join the visible ones (ADR-0057 A3; ADR-0075/#126 — without this a
/// sibling `return null` alone pins `Singleton(null)` and manufactures
/// `call.on-null` FPs). A walked `while` body keeps contributing it: what that walk
/// sees is a return read off a forgotten env, which is no stronger than the floor
/// and must not be mistaken for the whole exit set.
fn forget_construct_sets(
    w: &WalkCx,
    writes: &[String],
    reads: &[String],
    poisons: bool,
    may_return: bool,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
) {
    if poisons {
        env.clear();
        store.clear();
    } else {
        for v in writes.iter().chain(reads) {
            // A handle the construct names may have been closed inside it, and
            // the entry outlives the name for its aliases (ADR-0097 §2.4).
            store.escape_resource(v);
            env.remove(v);
            store.unbind(v);
        }
    }
    push_hidden_exit_floor(w, may_return);
}

/// The floor a construct with a hidden `return` owes the component (ADR-0057 C2/D3):
/// a hidden exit is an exit whose value and whose `$this` this walk never saw, so it
/// ends the component exactly as an unbound one does. Shared by every construct that
/// carries `may_return`, structured or not.
pub(crate) fn push_hidden_exit_floor(w: &WalkCx, may_return: bool) {
    if may_return
        && let Some(sc) = &w.summary
    {
        sc.exits.borrow_mut().push(ExitContribution::Floor);
        if let Some(te) = &sc.this_exits {
            te.borrow_mut().push(ExitContribution::Floor);
        }
    }
}

/// Walk an ordered statement (sub-)trace against a mutable env, threading the same
/// findings sink, descent, and facts. Returns whether the trace falls through.
/// Statements after a terminator are unreachable and are **not** walked (ADR-0031
/// closes ADR-0027's dead-fallthrough gap).
#[allow(clippy::too_many_arguments)]
pub(crate) fn walk_trace(
    w: &WalkCx,
    folder: &mut dyn Folder,
    stmts: &[Stmt],
    env: &mut HashMap<String, Known>,
    store: &mut Store,
    descent: &mut Option<Descent<'_>>,
    facts: &mut Option<&mut Vec<LineFact>>,
    guarded: bool,
    out: &mut Vec<Diagnostic>,
) -> Flow {
    let cx = w.cx;
    let scope = w.scope;
    for (stmt_idx, stmt) in stmts.iter().enumerate() {
        // 0. Statement-level inline `@var` casts (ADR-0073), applied before the
        // statement's own checks read the env. A tag above an `Assign` to the same
        // variable is erased by step 2's own rebind — the assignment-form `@var`
        // is a separate unsupported feature that stays silent. Plain per-scope pass
        // only (see `apply_inline_var_casts`).
        if descent.is_none() {
            apply_inline_var_casts(w, stmt, env, store);
            // 0a. The ADR-0076 loop-subject probe, read from the entry env (after
            // the inline cast). Gated on an installed probe, so a normal check pays
            // one `is_some()` and sees no behaviour change.
            record_subject_probe(cx, stmt, env);
            // 0a-bis. `foreach.non-iterable` (ADR-0078, issue #192), judged from the
            // same entry env the probe above just read (nothing has touched `env`
            // for this construct yet). The match stays by span against the ADR-0076
            // enumeration even though `StmtKind::Foreach` now names the subject too
            // (issue #652): the site carries the rest of the shape this family reads,
            // and one construct wants one reader.
            if let Some(site) = cx.tree().foreach_sites().iter().find(|s| s.span == stmt.span) {
                check_foreach_subject(cx, site, env, scope.poisoned, out);
            }
            // 0a-ter. `type.invalid-operand` (ADR-0078, issue #191), judged from the
            // same entry env — an assignment's operands evaluate before its own
            // binding lands. Leaf statements only, so a branch's sites are judged
            // once, when the walk reaches them.
            check_operand_sites(cx, stmt, env, scope.poisoned, out);
        }
        // 0b. The trace annotation (ADR-0074, issue #94): a statement-adopted
        // `@psalm-trace $x` docblock asks the dump surface's question against this
        // statement's exit facts (§5, Psalm semantics: "applied to the next
        // statement", reporting what it leaves behind). The answer is flushed via
        // `.take()` on whichever exit this iteration takes — divergent `return`s in
        // step 2, or the common bottom — so every annotated site answers exactly
        // once. Plain per-scope pass only.
        let mut pending_trace = if descent.is_none() {
            adopted_trace_docblock(w, stmt)
        } else {
            None
        };
        // What the calls will do to the names and resources they are handed, read
        // before step 1 runs any of them and applied in step 5.
        let pre_call = PreCall::read(w, folder, stmt, env, store);
        // 1. Check + descend every statically-named call this statement carries.
        let stmt_calls = check_stmt_calls(w, folder, stmt, env, store, descent, out);

        // 1z. The checks at the read positions this IR spells, judged against the
        // pre-statement env (`check_read_positions`).
        check_read_positions(w, folder, stmt, env, store, descent, out);

        // 1a. Escape + sweep (ADR-0036): passing an object into a call escapes it;
        // an unknown/overridable call — or any call an object was passed into —
        // sweeps every escaped object's non-readonly props. `$this` is pre-escaped,
        // so an overridable call on it sweeps it, while a resolved private/final
        // call with no object args leaves it intact. Then the `$this` copy-backs, over
        // whatever the sweeps left (ADR-0057's 2026-08-17 amendment, D4).
        apply_call_escape_and_sweep(w, &stmt.kind, store, &stmt_calls.this_backs);

        // 1b. Return-type check (native + phpdoc contract).
        check_return_value(w, folder, stmt, env, store, descent, out);

        // 1c. Return summary (ADR-0057 T0 value component, T1 heap component): when a
        // descent is building this callee's summary, snapshot each returning exit.
        // Read here — before the return's own escape/invalidation effect (step 2) —
        // so the returned variable's env fact is still live and its object still
        // carries the escape bit it had **before** the return marked it (§2.1's
        // escaped-before-return). The join is deferred to `descend`; here we only
        // classify the exit (A2 drop/cross, A3 floor, T1 allocation).
        //
        // The `$this` channel first, and independently (ADR-0057 C2 as generalized by
        // D3): where this walk's `$this` came from a caller object, every exit records
        // what it holds — a constructor's bare `return;`, and an ordinary method's
        // value `return`, which summarizes its value on the other channel at the very
        // same exit. Read at the same instant, before the return's own effects.
        if let StmtKind::Return { .. } = &stmt.kind
            && let Some(te) = w.summary.as_ref().and_then(|sc| sc.this_exits.as_ref())
        {
            te.borrow_mut().push(this_exit_contribution(store));
        }
        if let StmtKind::Return { value, .. } = &stmt.kind
            && let Some(sc) = &w.summary
        {
            // Composition (A1): when the returned expression IS a call whose
            // summary step 1 captured, that summary is this exit's fact — `return
            // g(...)`, `return $o->m(...)`/`C::m(...)` (ADR-0075) cross the proven
            // fact. A constructor `return new Foo(...)` never composes (object
            // return is T1). A recursive/unbindable inner call left `stmt_calls.summary`
            // empty, falling through to the direct value fact (thence A3 floor).
            let composed = if matches!(value, ArgValue::New(..)) {
                None
            } else {
                stmt_calls.summary
                    .as_ref()
                    .and_then(|s| s.value.as_ref())
                    .map(|sv| (sv.fact.clone(), sv.stratum))
            };
            let exit_fact = composed.or_else(|| return_value_fact(w, folder, value, env, store));
            let contrib = match exit_fact {
                // A2 — native-envelope violation: a proven boundary `TypeError`, the
                // value never reaches the caller. Drop the exit (record nothing); the
                // callee's own `type.return-mismatch` is the standing record.
                Some((fact, _)) if sc.native_violates(&fact) => None,
                // An informative exit within the envelope: it crosses with its stratum
                // (a phpdoc-only violation crosses HERE — the walk truth, A2).
                Some((fact, strat)) => Some(ExitContribution::Fact(fact, strat)),
                // A factless returning exit. T1: when it returns a locally-held
                // ALLOCATION, its snapshot is the heap component's contribution —
                // read strictly under the value classification, so the value
                // component's A3 semantics are what they were (a `Heap` exit joins
                // as a `Floor` on that side, which is what an object exit always
                // was). Otherwise A3 verbatim: degrade to the declared arm floor.
                None => Some(
                    match return_heap_object(
                        w,
                        folder,
                        value,
                        env,
                        store,
                        stmt_calls.summary.as_ref(),
                        stmt_calls.ctor_heap.as_ref(),
                    ) {
                        Some(obj) => ExitContribution::Heap(Box::new(obj)),
                        None => ExitContribution::Floor,
                    },
                ),
            };
            if let Some(c) = contrib {
                sc.exits.borrow_mut().push(c);
            }
        }

        // 2. Apply the statement's own effect on the environment + compute its flow.
        let flow = match &stmt.kind {
            StmtKind::Barrier => {
                env.clear();
                store.clear();
                Flow::FellThrough
            }
            // A destructuring assignment (issue #288) is a barrier that also names
            // the reads its source undergoes (judged above against the pre-statement
            // env); the env effect is the barrier's, since the pattern's targets are
            // writes this walk does not model.
            StmtKind::Destructure { .. } => {
                env.clear();
                store.clear();
                Flow::FellThrough
            }
            // The A-G8 invalidation table: barrier semantics, plus the base
            // binding's array shape carried across with the key promoted/removed.
            StmtKind::OffsetWrite { base, keys, value } => {
                // A handle stored into an array escapes (ADR-0097 §2.4).
                escape_mentioned_resources(value, store);
                apply_offset_write(w, folder, base, keys, Some(value), env, store);
                Flow::FellThrough
            }
            StmtKind::OffsetUnset { base, key } => {
                apply_offset_write(w, folder, base, std::slice::from_ref(key), None, env, store);
                Flow::FellThrough
            }
            // `$var[] = v` (issue #636): the same containment, with the landing
            // key computed rather than read from the source.
            StmtKind::OffsetAppend { base, value } => {
                escape_mentioned_resources(value, store);
                apply_offset_append(w, folder, base, value, env, store);
                Flow::FellThrough
            }
            // `echo` assigns nothing on its own; anything it *can* mutate (embedded
            // assignment / by-ref call) is in `invalidated` (step 3). Reading a
            // variable in an echo no longer forgets it (ADR-0031 precision payoff).
            StmtKind::Echo(_) => Flow::FellThrough,
            // A still-`Opaque` construct (loop/switch/try) forgets what it may write
            // AND what it branches on (ADR-0027) since the trace does not model its
            // control flow. When the subtree may `return`, a summary walk
            // contributes the declared floor so hidden exits join the visible ones
            // (ADR-0057 A3; ADR-0075/#126 — without this a sibling `return null`
            // alone pins Singleton(null) and manufactures call.on-null FPs).
            StmtKind::Opaque { writes, reads, poisons, may_return } => {
                forget_construct_sets(w, writes, reads, *poisons, *may_return, env, store);
                Flow::FellThrough
            }
            // The structured loops (ADR-0027 amendment): `walk_loop` holds each one's rule.
            StmtKind::While { .. }
            | StmtKind::For { .. }
            | StmtKind::Foreach { .. }
            | StmtKind::DoWhile { .. } => {
                walk_loop(w, folder, stmt, env, store, descent, facts, guarded, out)
            }
            // A statement-position call to a resolved `: never` callee is a
            // terminator (issue #599), and the arm below is the `Throw`/`Exit` arm
            // verbatim for that reason: the branch it ends contributes nothing to
            // the join, so a guard whose arm ends in one subtracts, and the code
            // after it stops being analyzed. Judged AFTER step 1, so the call is
            // checked and descended exactly as it was before it terminated anything.
            StmtKind::Call(call) if call_never_returns(w, call, store) => {
                for v in &stmt.invalidated {
                    env.remove(&v.name);
                    store.unbind(&v.name);
                }
                emit_trace_annotations(w, folder, pending_trace.take(), stmt, env, store, out);
                return Flow::Terminated;
            }
            StmtKind::Call(_) => Flow::FellThrough,
            // `assert($expr)` narrows the fall-through env with the guard's
            // true-branch refinements (ADR-0052 §5, amended 2026-07-25 — owner
            // ruling: `assert($expr)` reads as `if (!$expr) throw`, unconditionally,
            // at `Verified` stratum; `zend.assertions` is never consulted).
            StmtKind::Assert { cond } => {
                apply_type_narrowing(w.cx, cond, true, env, store);
                let refs = then_refinements(cond, w.cx.php_minor);
                apply_refinements(&refs, env, store, Stratum::Verified);
                // The declared-arm lane, in the same order `walk_if` applies it
                // (issue #391): `apply_refinements` above reaches the VALUE lane
                // only, and a `T|false` binding has no value-lane carrier at all
                // (`seed_refined_scalar_fact` mints one only for a refinement
                // *within* one base), so without this call `assert($x !== false)`
                // narrowed nothing while its `if` twin narrowed to `string`. The
                // arm lane is a subtraction carrier (ADR-0052 §2 / the 2026-08-01
                // `Value`-subtrahend note): each surviving arm keeps its own
                // stratum, so a `Verified` arm cannot launder and an `Asserted`
                // one cannot be promoted by having been asserted about.
                apply_class_narrowing(w, cond, true, store);
                // The assert lowering models its argument as a `CondExpr`, so both
                // `assert(isset(...))` and the DR2 type-predicate vocabulary
                // (`assert(is_string($x))`) route through the same `if`-guard
                // narrowing with no assert-specific plumbing (ADR-0064 §5).
                apply_shape_narrowing(w.cx, cond, true, env, store, true);
                Flow::FellThrough
            }
            // Terminators: the trace stops; the remainder is unreachable.
            StmtKind::Return { value, .. } => {
                // `return $o;` escapes the returned object (ADR-0036), and a
                // returned handle — named anywhere in the operand — escapes the
                // same way (ADR-0097 §2.4).
                if let ArgValue::Var(v) = value {
                    store.mark_escaped(v);
                }
                escape_mentioned_resources(value, store);
                for v in &stmt.invalidated {
                    env.remove(&v.name);
                    store.unbind(&v.name);
                }
                // A `return $x;` under the annotation still answers (ADR-0074
                // §5): flush the pending trace at this divergent exit.
                emit_trace_annotations(w, folder, pending_trace.take(), stmt, env, store, out);
                return Flow::Terminated;
            }
            // `break`/`continue` end the block they sit in, so they end this
            // sub-trace: the statements after one are unreachable, and a branch
            // ending in one contributes nothing to its `if`'s join. Identical to
            // the `Throw`/`Exit` arm for that reason — and NOT the `Barrier` these
            // used to lower to, which fell through with a cleared env and so
            // erased, through the join, everything the rest of a loop body knew
            // (issue #649).
            StmtKind::LoopJump { .. } | StmtKind::Throw { .. } | StmtKind::Exit { .. } => {
                for v in &stmt.invalidated {
                    env.remove(&v.name);
                    store.unbind(&v.name);
                }
                // A diverging `throw`/`exit` under the annotation still
                // answers (ADR-0074 §5).
                emit_trace_annotations(w, folder, pending_trace.take(), stmt, env, store, out);
                return Flow::Terminated;
            }
            StmtKind::Assign { var, value, span, call } => {
                apply_assign(
                    w,
                    folder,
                    var,
                    value,
                    call.as_ref(),
                    span.start,
                    env,
                    store,
                    facts,
                    stmt_calls.summary.as_ref(),
                    stmt_calls.ctor_heap.as_ref(),
                    stmt_calls.return_arms.as_deref(),
                    out,
                );
                Flow::FellThrough
            }
            StmtKind::PropAssign { target_var, prop, value, span, .. } => {
                // Property checks run only in the plain per-scope pass (like the
                // return check): a binding descent rebinds the callee's params to
                // hypothetical caller values that in-body guards (unmodeled here)
                // would narrow — checking a descent-bound property write is
                // guard-blind and unsound. The heap update always runs so reads
                // within the descent still resolve.
                let checks_enabled = descent.is_none();
                apply_prop_assign(
                    w, folder, target_var, prop, value, span.start, guarded, checks_enabled, env,
                    store, out,
                );
                Flow::FellThrough
            }
            StmtKind::If { cond, then_trace, elseifs, else_trace } => {
                // ADR-0088 §5 (issue #448): a `match (true)`/`match (false)`
                // guard chain with no `default` desugars to exactly this shape
                // (`else_trace: None`, issue #431) — the coverage question
                // `walk_match` asks of a by-value `match` must still be asked
                // here. `scope.guard_chain_no_default` is the structural,
                // span-keyed record of which `If`s are such a chain (computed
                // off the CST, independently of this trace — ADR-0031 keeps
                // `StmtKind::If` itself free of the bit); `guard_chain_subject`
                // is the further restriction to a chain every arm of which
                // addresses one common variable, the only shape a single
                // `Store::contract` lane can answer for. Plain per-scope walk
                // only, mirroring `walk_match`'s own `descent.is_none()` gate.
                let chain = (descent.is_none() && else_trace.is_none())
                    .then(|| {
                        scope
                            .guard_chain_no_default
                            .iter()
                            .find(|s| s.start == stmt.span.start)
                            .and_then(|&span| {
                                guard_chain_subject(cond, elseifs)
                                    .map(|subject| GuardChainCoverage { span, subject })
                            })
                    })
                    .flatten();
                walk_if(
                    w, folder, cond, then_trace, elseifs, else_trace.as_deref(), chain.as_ref(),
                    env, store, descent, facts, out,
                )
            }
            StmtKind::Match { subject, arms, default, loose } => walk_match(
                w, folder, subject, arms, default.as_deref(), *loose, stmt.span, env, store,
                descent, facts, out,
            ),
        };

        // 3 to 5. The asserts, the by-ref invalidation, then what `pre_call` read.
        settle_stmt(w, stmt, pre_call, env, store);

        // Flush the pending trace annotation at the iteration's common exit —
        // the statement's own effect (step 2), its assert narrowings (step 3)
        // and the by-ref invalidation (step 4) have all applied, so this env IS
        // the state the next statement would enter with (ADR-0074 §5's exit
        // facts). Covers the fall-through path and the diverging `If`/`Match`
        // below alike; the step-2 terminators flushed at their own `return`s.
        emit_trace_annotations(w, folder, pending_trace.take(), stmt, env, store, out);

        if flow == Flow::Terminated {
            // The rest of this trace is proven unreachable (ADR-0031).
            mark_dead(w, &[&stmts[stmt_idx + 1..]]);
            return Flow::Terminated;
        }
    }
    Flow::FellThrough
}

/// What a statement's calls do to the names and heap resources they are handed (the
/// out-parameter seeds, the produced places and the resource effects), read on the
/// entry env and store before step 1 checks or descends any of them, and applied by
/// [`settle_stmt`] over step 4's forgetting.
struct PreCall {
    seeds: Vec<(String, Fact, Stratum, u32)>,
    places: Vec<(String, Vec<(Key, HeapRes)>)>,
    resources: ResourceEffects,
}

impl PreCall {
    fn read(
        w: &WalkCx,
        folder: &mut dyn Folder,
        stmt: &Stmt,
        env: &HashMap<String, Known>,
        store: &Store,
    ) -> Self {
        // The statement-position out-parameter seeds (issue #595), read HERE and
        // applied in step 5. The input a cast consumes is what the variable held
        // before the call, and step 4 is about to forget exactly that — so the
        // read has to happen while the entry env still holds it.
        let stmt_out_seeds = stmt_out_param_seeds(w, folder, &stmt.kind, env, store);
        // The out-parameter **places** of the same statement (ADR-0098 §2.2):
        // `proc_open($cmd, $spec, $pipes)` hands back one handle per `pipe`
        // descriptor of a proven spec. Read here with the seeds above and for
        // the same reason — the spec is an argument, and step 4 is about to
        // forget the name it may be held in.
        let stmt_places = stmt_produced_places(w, folder, &stmt.kind, env);
        // What this statement's calls do to the heap resources they are handed
        // (ADR-0097 §2.4) — a close, a keeper, an escape — read on the same
        // pre-call store and applied beside the out-parameter seeds, after the
        // statement's checks have judged the arguments in the state they were passed in.
        let stmt_resources = resource_call_effects(
            w.cx,
            folder,
            w.scope.poisoned,
            &checkable_calls(&stmt.kind),
            Some(&stmt.invalidated),
            store,
        );
        Self { seeds: stmt_out_seeds, places: stmt_places, resources: stmt_resources }
    }
}

/// Steps 3 to 5 of [`walk_trace`]: the `@phpstan-assert` narrowings, the by-ref
/// invalidation they and the by-value survivors are exempt from, and then what
/// [`PreCall`] read before the calls ran. The step-2 arms that return from the walk
/// directly skip all three.
fn settle_stmt(
    w: &WalkCx,
    stmt: &Stmt,
    pre_call: PreCall,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
) {
    let cx = w.cx;
    let scope = w.scope;
    let PreCall { seeds: stmt_out_seeds, places: stmt_places, resources: stmt_resources } =
        pre_call;

    // 3. Apply `@phpstan-assert` (Always) narrowings from every call in this
    // statement (Feature D), collecting the vars they establish. This runs
    // BEFORE the by-ref invalidation below so the replace-if-weaker decision
    // sees a proven `Singleton`/`OneOf` (kept over a weaker asserted fact); the
    // asserted vars are then protected from the conservative forget, since the
    // assertion helper's contract is a *stronger* statement than "the call may
    // have mutated this by reference".
    let mut asserted: HashSet<String> = HashSet::new();
    for call in checkable_calls(&stmt.kind) {
        apply_stmt_asserts(w, call, env, store, &mut asserted);
    }

    // 4. After the statement, invalidate any variable handed to a call — except
    // one an assertion just narrowed (its post-call fact is known), except
    // one every occurrence of which is a proven by-value argument (ADR-0070),
    // and except a handle every occurrence of which a keeper or a closing
    // call received (ADR-0097 §2.4): a by-value builtin argument never rebinds
    // the variable, so the resource lane survives and only the heap state
    // moves (step 5).
    let (by_value, object_kept) =
        by_value_survivors(cx, scope.poisoned, &stmt.invalidated, env, store);
    for v in &stmt.invalidated {
        if asserted.contains(&v.name)
            || by_value.contains(v.name.as_str())
            || stmt_resources.kept.contains(&v.name)
        {
            continue;
        }
        env.remove(&v.name);
        store.unbind(&v.name);
    }
    // A kept object handle that reached a by-value call site OTHER than as a
    // direct argument — an offset root, `strstr($a['k'], …)` on an `ArrayAccess`
    // receiver — keeps the handle and loses the mutable state
    // (`by_value_survivors`' second set). A direct object argument is the
    // ADR-0036 escape rule's, which sweeps only for a callee that can reach
    // the object and keeps the carry through one that provably cannot; this
    // sweep must not second-guess it.
    let direct: HashSet<&str> = checkable_calls(&stmt.kind)
        .iter()
        .flat_map(|c| c.args.iter())
        .filter_map(|a| match &a.value {
            ArgValue::Var(v) => Some(v.as_str()),
            _ => None,
        })
        .collect();
    for v in object_kept {
        if !direct.contains(v) {
            store.sweep_object(v);
        }
    }

    // 5. Rebind what a proven by-ref write left behind (issue #595), over the
    // forgetting step 4 just did — the ADR-0077 §3.4 ordering at the statement
    // rung: the callee's stated write REPLACES the conservative drop rather
    // than racing it. Empty for every statement that carries no such call.
    apply_stmt_out_param_seeds(stmt_out_seeds, env, store);
    // The produced places, over the same forgetting: step 4's `unbind` of
    // `$pipes` is what dropped the previous call's places (ADR-0098 §2.3),
    // so these are this call's alone.
    apply_produced_places(w, stmt_places, store);
    // The heap resources' state transitions (ADR-0097 §2.4): a closing call's
    // return proves `Closed`, an escape forgets. By allocation id, so a name
    // step 4 dropped or the statement rebound still reaches the entry its
    // aliases share.
    apply_resource_effects(&stmt_resources, store);
}

/// The variables `stmt` hands to a call whose facts nevertheless **survive** it
/// (ADR-0070) — the precise reading of the blanket `Stmt::invalidated` drop.
///
/// Evidence comes from two positions: [`Stmt::invalidated`], and a comparison
/// operand's [`CondOperand::Other`] `sites` (issue #158 — `count($a) === count($b)`
/// hands `$a` to the same by-value parameter `count($a);` does).
///
/// Survival is possible because PHP passes scalars, strings and arrays **by value**
/// (copy-on-write): the callee's parameter is a separate zval, so forgetting the
/// caller's shape is precision loss, not a soundness risk. A `&$x` parameter and an
/// object *handle* pierce that — the first is refused below; the second is admitted
/// since the 2026-08-09 amendment (issue #295) because ADR-0036 already sweeps the
/// referent's state earlier in the statement. See [`is_value_semantic`].
///
/// # The gate — all five must hold, per variable
///
/// 1. Every occurrence of the name in this statement's call arguments is a recorded
///    site on its [`steins_syntax::InvalidatedVar`] entry (an unprovable occurrence
///    makes the entry `opaque`, with no sites), and each callee resolves with a known
///    signature — project ([`Param::by_ref`]) or catalog builtin
///    ([`steins_catalog::by_value_arg`]). An unknown callee refuses. An occurrence is
///    a bare `$v` argument or a pure offset chain rooted at one (`count($v[0])` —
///    issue #609): a by-value position copies the element out and the root survives;
///    a by-ref one writes through the chain into the root's binding and condemns it.
/// 2. The argument is by value at that position (call-time pass-by-reference was
///    removed in PHP 8, so this is fixed by the declaration): a `&$x` parameter, an
///    argument past declared arity, or a variadic position refuses.
/// 3. The variable is value-semantic or a heap object handle — a closure value or a
///    bare guard-derived class bound still drops.
/// 4. The scope is not poisoned. Every aliasing/scope-injection construct (`$x = &$y`,
///    `global`, `static $x`, `$$v`, `extract`/`compact`, `eval`, `include`, a by-ref
///    `use (&$x)`) poisons the whole scope — including inside a project callee's own
///    body, closing the route by-value alone can't: reaching a caller local via
///    `global`.
/// 5. Language constructs (`isset`/`empty`/`unset`/`list`) never reach this path —
///    they aren't call nodes, so the lowering records no site.
///
/// # Read-site exceptions
///
/// A recognized dump callee — `PHPStan\dumpType` (D3) or global `var_dump` (D4) — is a
/// read that binds nothing (ADR-0053 §10 §3), exempt from conditions 1–3, keeping the
/// dump surface idempotent. Recognition is the emitters' own resolved-FQN rule
/// ([`dump_family`]), so gate and emitter can't disagree.
///
/// In the harness universe only ([`ASSERT_SINK`], installed by
/// [`collect_assert_types`]), a `PHPStan\Testing\assertType` site is the same kind of
/// read, keeping repeated-assert nsrt files honest. Not unconditional like the dumps —
/// with the sink absent, [`is_assert_read_site`] is `false` everywhere and the check
/// surface stays byte-identical (the [`emit_asserts`] pin).
///
/// # Replayability (ADR-0048)
///
/// The verdict is a pure function of the statement's recorded sites, the project
/// index, the static catalog, and the walk-local env/store — no reflection, boot
/// surface, or fold, so no per-name engine state needs memoizing.
///
/// [`CondOperand::Other`]: steins_syntax::CondOperand::Other
/// [`Param::by_ref`]: steins_syntax::Param::by_ref
/// [`dump_family`]: crate::dump::dump_family
/// [`collect_assert_types`]: crate::assert_harness::collect_assert_types
pub(crate) fn by_value_survivors<'s>(
    cx: &Cx<'_>,
    poisoned: bool,
    invalidated: &'s [InvalidatedVar],
    env: &HashMap<String, Known>,
    store: &Store,
) -> (HashSet<&'s str>, HashSet<&'s str>) {
    let mut kept: HashSet<&'s str> = HashSet::new();
    // The kept names whose binding is an object handle AND that reached a real
    // by-value call site (not only a dump/assert read): the call may have run
    // userland on that object — an offset read on an `ArrayAccess` receiver is
    // `offsetGet`, a userland body (issue #637's adversarial review) — so the
    // object's mutable state must take the ADR-0036 sweep even though the name
    // itself keeps its handle. Direct object arguments are swept by the escape
    // rule already; an offset ROOT is not an argument, and this is its sweep.
    let mut object_kept: HashSet<&'s str> = HashSet::new();
    // Condition 4 (this scope's half): every scope on the ADR-0001 give-up list
    // keeps the blanket drop outright.
    if poisoned {
        return (kept, object_kept);
    }
    for entry in invalidated {
        // An opaque entry has an unprovable occurrence somewhere in the
        // statement — the lowering already discarded whatever provable sites
        // the name had, so no protection may be granted (the blanket drop).
        if entry.opaque {
            continue;
        }
        let var = entry.name.as_str();
        // Whether the name already passed the value-semantic gate (condition 3)
        // — a memo of its own, since `keep` can also record dump-read survival,
        // which never takes that gate.
        let mut sem_ok = false;
        let mut keep = false;
        let mut via_call = false;
        for (callee, position) in &entry.sites {
            // The read-site exceptions (docs above): a dump (ADR-0053) — and, in
            // the harness universe only, an `assertType` observation (oracle idea
            // B) — reads and binds nothing, so this occurrence keeps the name —
            // object bindings included — and never condemns it.
            if is_dump_read_site(cx, callee) || is_assert_read_site(cx, callee) {
                keep = true;
                continue;
            }
            // Condition 3, asked once per name and BEFORE any index work: a name
            // with an object binding refuses whatever its callees say, and a name
            // with no binding at all has nothing to save (dropping it is already a
            // no-op), so neither is worth resolving a callee for.
            if !sem_ok {
                if !is_value_semantic(var, env, store) {
                    keep = false;
                    break;
                }
                sem_ok = true;
            }
            if arg_is_by_value(cx, callee, *position) {
                keep = true;
                via_call = true;
            } else {
                // One by-ref (or unresolvable) occurrence condemns the name for
                // the whole statement, whatever its other occurrences promised.
                keep = false;
                break;
            }
        }
        if keep {
            kept.insert(var);
            if via_call && store.refs.contains_key(var) {
                object_kept.insert(var);
            }
        }
    }
    (kept, object_kept)
}

/// Whether a call-argument site's callee is a **dump-surface read** (ADR-0053):
/// the reserved `PHPStan\dumpType` pair (D3) by resolved FQN, or the global
/// `var_dump` (D4) by the PHP fallback rule — each by exactly the recognizer its
/// emitter uses ([`dump_family`]'s FQN rule, [`recognizes_var_dump`]'s name
/// core), so the survival gate and the emitters can never disagree about what a
/// dump is. See the exception paragraph on [`by_value_survivors`].
///
/// [`dump_family`]: crate::dump::dump_family
/// [`recognizes_var_dump`]: crate::dump::recognizes_var_dump
fn is_dump_read_site(cx: &Cx<'_>, r: &NameRef) -> bool {
    is_dump_family_fqn(&resolved_fn_fqn(cx, r)) || name_reaches_global_var_dump(cx, r)
}

/// Whether a call-argument site's callee is the **harness assertType read**
/// (oracle idea B): the reserved `PHPStan\Testing\assertType` FQN, recognized
/// only while the [`ASSERT_SINK`] is installed — the same condition, and the
/// same resolved-FQN rule ([`ASSERT_TYPE_FQN`]), that gates [`emit_asserts`],
/// so the survival gate and the observer can never disagree about what an
/// assertion is. With no sink (every normal check) this is `false` for every
/// site and `assertType` stays an ordinary call — the check surface is
/// byte-identical. See the exception paragraph on [`by_value_survivors`].
fn is_assert_read_site(cx: &Cx<'_>, r: &NameRef) -> bool {
    ASSERT_SINK.with(|s| s.borrow().is_some()) && resolved_fn_fqn(cx, r) == ASSERT_TYPE_FQN
}

/// Whether `var` holds a **value-semantic** binding worth saving — a scalar,
/// string, or array — rather than an object handle or nothing at all (ADR-0070
/// condition 3).
///
/// [`Fact`] has no object layer by construction, so the object question is asked
/// of the carriers that hold one: the heap handle lane, the guard-derived
/// class-bound lane, and the closure value on the binding itself.
///
/// The heap handle lane admits (ADR-0070 amendment, issue #295): a by-value call
/// cannot change an object's class or rebind the caller's variable — only its
/// mutable state, which is already invalidated earlier in the statement by the
/// ADR-0036 escape-and-sweep. Dropping the var→id link on top of that would erase
/// the allocation identity (exact class, readonly facts, generic carry) for no
/// soundness gain. The route that could rebind (`&$x`) is refused by condition 2;
/// sideways routes (`global`, `extract`, `$$v`, `eval`) by condition 4.
///
/// A guard-derived class bound alone (`Member` with no heap object) deliberately
/// does not follow it here — a separate consumer set to measure later.
///
/// A name none of the lanes mention answers `false` too, purely as a cost gate:
/// invalidating an unbound name is already a no-op.
fn is_value_semantic(var: &str, env: &HashMap<String, Known>, store: &Store) -> bool {
    if store.refs.contains_key(var) {
        return true;
    }
    if store.members.contains_key(var) {
        return false;
    }
    match env.get(var) {
        Some(k) => k.closure.is_none(),
        // No value binding: only a declared-arm (contract) lane is left to save.
        None => store.contract.contains_key(var),
    }
}

/// Whether one recorded site — callee reference plus 0-based argument
/// `position` — is a **by-value** argument position of a callee with a known
/// signature (ADR-0070 conditions 1 and 2). The refusing answer is `false` for
/// every uncertainty — an unresolved name, an ambiguous one, a method, an
/// argument past the declared arity.
pub(crate) fn arg_is_by_value(cx: &Cx<'_>, callee: &NameRef, position: u32) -> bool {
    let position = position as usize;
    match cx.resolve_arg_function(callee) {
        // The catalog states this name's argument semantics; `Some(true)` is the
        // only admitting answer (`None` cannot occur — it is what made the name
        // resolve to `Builtin` — but is spelled out rather than assumed). Keyed
        // by the resolved catalog name, not `callee.raw`: an aliased import
        // (`use function trim as t;`) spells the call `t`, which the catalog
        // has never heard of (issue #279).
        FnResolution::Builtin(builtin_name) => {
            steins_catalog::by_value_arg_frame(&builtin_name, position, mined_arm_admitted())
                == Some(true)
        }
        FnResolution::User(fn_site) => {
            // The declaration answers condition 2 directly, and it is the cheap
            // half — asked first so a by-ref parameter refuses without the scope
            // lookup below. A variadic position refuses: the analysis does not
            // model spread/variadic binding (v1). An argument past the declared
            // arity is `func_get_args()` territory, with nothing to read.
            match cx.fn_decl(fn_site).params.get(position) {
                Some(p) if !p.by_ref && !p.variadic => {}
                _ => return false,
            }
            // Condition 4's callee half: a body that itself defeats value
            // tracking (`global $w`, `extract`, `$$v`, `eval`) may reach the
            // caller's binding by a route argument passing does not describe.
            matches!(cx.fn_scope(fn_site), Some((_, body)) if !body.poisoned)
        }
        FnResolution::Unknown => false,
    }
}

/// The trust stratum a resolved value carries (ADR-0052 §5 derivation clause): the
/// minimum over every env/heap fact consumed while resolving `value`. A literal or
/// fully-literal subtree is `Verified`; a bare `$var` takes its env stratum; a
/// property fetch takes the prop's stratum; an array/call/ternary takes the min
/// over its parts. Stamps the derived binding with `min(inputs)`, closing the
/// laundering hazard the audit's `$pair = [$x, 99]` snippet names.
pub(crate) fn value_stratum(
    cx: &Cx<'_>,
    value: &ArgValue,
    env: &HashMap<String, Known>,
    store: Option<&Store>,
) -> Stratum {
    match value {
        ArgValue::Var(name) => env.get(name).map_or(Stratum::Verified, |k| k.stratum),
        // A property fetch takes its prop's stratum; with no store in scope (the
        // variable-call check) a prop fetch never resolves to a proof premise, so
        // `Verified` is the correct neutral answer.
        ArgValue::PropFetch { var, prop } => {
            store.map_or(Stratum::Verified, |s| s.prop_stratum(var, prop))
        }
        ArgValue::Array(items) => items
            .iter()
            .fold(Stratum::Verified, |acc, (_, v)| acc.min(value_stratum(cx, v, env, store))),
        ArgValue::Call(_, args) => {
            args.iter().fold(Stratum::Verified, |acc, v| acc.min(value_stratum(cx, v, env, store)))
        }
        // A method call's own arguments, plus a receiver `new`'s (issue #386): every
        // value the call consumes is a value its result derives from, and the
        // receiver's construction arguments are consumed exactly as the call's are.
        // The receiver *object*'s stratum is not read here — this seam sees no heap
        // beyond a prop fetch, and the summary that does carries its own `min`.
        ArgValue::MethodCall { callee, args, named } => {
            let recv = match callee {
                Callee::Method { receiver: Receiver::New { args, named, .. }, .. } => {
                    value_stratum_of_args(cx, args, named, env, store)
                }
                _ => Stratum::Verified,
            };
            recv.min(value_stratum_of_args(cx, args, named, env, store))
        }
        ArgValue::Ternary { then_val, else_val, .. } => {
            value_stratum(cx, then_val, env, store).min(value_stratum(cx, else_val, env, store))
        }
        // `$a ?? $b` consumes both operands' facts (a widening join): `min` (§5).
        ArgValue::Coalesce(a, b, _) => {
            value_stratum(cx, a, env, store).min(value_stratum(cx, b, env, store))
        }
        // `$a . $b` consumes both operands' facts to build one string — the same
        // derivation clause: `min`. An asserted operand must not launder itself into
        // a verified result string.
        ArgValue::Concat(a, b) => {
            value_stratum(cx, a, env, store).min(value_stratum(cx, b, env, store))
        }
        // A bare global constant (ADR-0094 §3.2). Everything this resolver
        // answers is `Verified` — true of every host the project can run on —
        // except a value fixed by the `[runtime] os` pin, which is the user's
        // claim about the deployment host and must not launder into a
        // proof-layer premise through a comparison or a concatenation.
        //
        // By the name PHP RESOLVES the reference to, which is what the `Cx` is
        // for: `use const PHP_EOL as EOL;` spells `EOL`, and matching the raw
        // spelling let the alias carry the pinned value at `Verified`.
        ArgValue::GlobalConst(r) => {
            if crate::global_consts::denotes_os_pinned_constant(cx, r) {
                Stratum::Asserted
            } else {
                Stratum::Verified
            }
        }
        _ => Stratum::Verified,
    }
}

/// The `min` stratum over one call's positional and named argument values — the
/// [`value_stratum`] derivation clause applied to an argument list.
fn value_stratum_of_args(
    cx: &Cx<'_>,
    args: &[ArgValue],
    named: &[NamedArg],
    env: &HashMap<String, Known>,
    store: Option<&Store>,
) -> Stratum {
    args.iter()
        .map(|v| value_stratum(cx, v, env, store))
        .chain(named.iter().map(|n| value_stratum(cx, &n.value, env, store)))
        .fold(Stratum::Verified, Stratum::min)
}
