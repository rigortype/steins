//! One file's walk and the fan-out that runs many (issue #490): the run's
//! shared inputs every walk reads ([`WalkInputs`]), the sink each fills
//! ([`FileSink`]), and the walk itself — the scope walk, then the file's own
//! passes in a fixed order ([`walk_one_file`]).
//!
//! Nothing here decides the run's diagnostic vector: [`check_units_controlled`]
//! merges the sinks in unit order, wherever and in whatever order they were
//! filled.
//!
//! [`check_units_controlled`]: crate::check_units_controlled

use std::collections::{HashMap, HashSet};

use steins_db::{PluginFacts, ProjectLayout};
use steins_syntax::{ArgValue, SourceTree, Span};

use crate::absence::{check_undefined_class, check_undefined_constant};
use crate::arg_check::{implicit_null_accepted, is_type_error};
use crate::cx::Cx;
use crate::dam::DamFacts;
use crate::docblock_hygiene::docblock_hygiene;
use crate::env::{Known, Store};
use crate::fold::Folder;
use crate::generics::{check_callable_arg, check_phpdoc_param};
use crate::mechanics::check_array_duplicate_keys;
use crate::overrides::check_declaration_fatals;
use crate::project::{Diagnostic, FileUnit, Index, LazyTree};
use crate::purity::PurityOracle;
use crate::return_missing::check_return_missing;
use crate::undefined_var::{check_phpdoc_maybe_undefined, check_undefined_variables};
use crate::unknown_vocabulary::{VocabularyAllowlist, unknown_vocabulary};
use crate::untyped::untyped_surface;
use crate::walk::{analyze_scope, in_dead};
#[cfg(not(target_arch = "wasm32"))]
use crate::walk_fleet;
use crate::walk_fleet::WalkFleet;
use crate::RuntimePostures;

/// What one file's walk produced, before it is merged into the run's vector:
/// the diagnostics the block appended and the `uncovered_matches` entry it
/// made. The same two values [`FileWalk`] records — this is the in-flight form,
/// held per file so the walk can run out of unit order and the merge can put it
/// back in.
///
/// [`FileWalk`]: crate::walk_plan::FileWalk
pub(crate) struct FileSink {
    pub(crate) diagnostics: Vec<Diagnostic>,
    pub(crate) uncovered: Option<Vec<u32>>,
}

/// Everything one file's walk reads that is neither its own `&mut` state nor
/// the file index: the run's shared, immutable inputs, gathered so the fan-out
/// (issue #490) has exactly one thing to share and one thing to prove `Sync`.
///
/// Every field is a shared borrow or a `Copy` scalar. That is the whole
/// argument for fanning the loop out: a walk mutates a [`Folder`] and a
/// diagnostic sink, both of which the worker owns, and reads this — which no
/// walk can change.
pub(crate) struct WalkInputs<'a> {
    pub(crate) units: &'a [FileUnit<'a>],
    pub(crate) index: &'a Index,
    pub(crate) dam: &'a DamFacts,
    pub(crate) unparsable: &'a HashSet<&'a str>,
    pub(crate) postures: RuntimePostures,
    pub(crate) php_minor: Option<(u16, u16)>,
    pub(crate) catalog_skew: bool,
    pub(crate) version_id: Option<(u32, Option<u32>)>,
    pub(crate) purity: Option<&'a PurityOracle<'a>>,
    pub(crate) layout: &'a ProjectLayout,
    /// The loaded plugin channel (ADR-0068). Read by
    /// `phpdoc.unknown-vocabulary`'s allowlist, which ADR-0091 §4.1 requires be
    /// assembled after plugin load.
    pub(crate) plugins: &'a PluginFacts,
    pub(crate) never_returning: &'a HashSet<String>,
}

impl WalkInputs<'_> {
    /// Walk unit `fi` on `folder`, into a sink of its own.
    pub(crate) fn walk(&self, folder: &mut dyn Folder, fi: usize) -> FileSink {
        let mut diagnostics = Vec::new();
        let uncovered = walk_one_file(self, folder, fi, &mut diagnostics);
        FileSink { diagnostics, uncovered }
    }
}

/// The shared inputs really are shared: if any of them ever grows interior
/// mutability, the fan-out stops compiling here rather than racing at runtime.
/// Named individually rather than only through [`WalkInputs`] so the failure
/// says *which* input moved.
const _: () = {
    const fn sync<T: Sync + ?Sized>() {}
    sync::<Index>();
    sync::<DamFacts>();
    sync::<ProjectLayout>();
    sync::<SourceTree>();
    sync::<LazyTree<'_>>();
    sync::<[FileUnit<'_>]>();
    sync::<PurityOracle<'_>>();
    sync::<PluginFacts>();
    sync::<WalkInputs<'_>>();
};

/// Walk `order` across `fleet`'s workers, filling each file's own sink
/// (issue #490).
///
/// The universe is cut into one contiguous chunk per worker rather than
/// scheduled file by file, and that is deliberate on two counts: a chunk hires
/// exactly one folder, so the fleet's live folders — and so its `php` children
/// — are bounded by the fan-out's width whatever the scheduler does; and
/// contiguous slots are mostly one package, so two workers rarely contend for
/// the same artifact reader.
///
/// Nothing here decides the diagnostic vector: the caller merges the sinks in
/// unit order afterwards, exactly where the sequential loop appended them.
/// Returns the width it actually fanned out to, which the ledger reports.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn fan_out(
    inputs: &WalkInputs<'_>,
    order: &[usize],
    fleet: &dyn WalkFleet,
    sinks: &mut [Option<FileSink>],
) -> usize {
    use rayon::prelude::*;

    let Some(pool) = walk_fleet::walk_pool() else {
        // No pool to be had. The caller's own folder is not reachable from
        // here, so the honest answer is one of the fleet's own, walking the
        // whole order on this thread — the sequential answer, one folder over.
        fleet.run_chunk(0, &mut |folder| {
            for &fi in order {
                sinks[fi] = Some(inputs.walk(folder, fi));
            }
        });
        return 1;
    };
    let workers = fleet.width(order.len());
    let chunks: Vec<(usize, &[usize])> =
        order.chunks(order.len().div_ceil(workers)).enumerate().collect();
    let walked: Vec<Vec<(usize, FileSink)>> = pool.install(|| {
        chunks
            .into_par_iter()
            .map(|(chunk, slots)| {
                let mut produced = Vec::with_capacity(slots.len());
                fleet.run_chunk(chunk, &mut |folder| {
                    for &fi in slots {
                        produced.push((fi, inputs.walk(folder, fi)));
                    }
                });
                produced
            })
            .collect()
    });
    for (fi, sink) in walked.into_iter().flatten() {
        sinks[fi] = Some(sink);
    }
    workers
}

/// The browser build has no threads and never carries a fleet, so the fan-out
/// is a walk in place on the one folder a chunk is given.
#[cfg(target_arch = "wasm32")]
pub(crate) fn fan_out(
    inputs: &WalkInputs<'_>,
    order: &[usize],
    fleet: &dyn WalkFleet,
    sinks: &mut [Option<FileSink>],
) -> usize {
    fleet.run_chunk(0, &mut |folder| {
        for &fi in order {
            sinks[fi] = Some(inputs.walk(folder, fi));
        }
    });
    1
}

/// One file's walk block, lifted out of [`check_units_controlled`]'s loop
/// verbatim so the replay seam has something to be a peer of. Appends
/// everything the block produces to `out` and returns the `uncovered_matches`
/// entry it makes — `None` for an unparsable file, which makes none.
///
/// Three phases, in this order and no other: [`walk_scopes`] first, because
/// the passes after it skip the dead regions it proves; then [`file_passes`];
/// then [`direct_pass`]. The block is the findings in the order they were
/// appended, and the paranoid verifier compares a replayed block with a walked
/// one position by position, so the order is part of what a replay replays.
///
/// [`check_units_controlled`]: crate::check_units_controlled
fn walk_one_file(
    inputs: &WalkInputs<'_>,
    folder: &mut dyn Folder,
    fi: usize,
    out: &mut Vec<Diagnostic>,
) -> Option<Vec<u32>> {
    // parse failure (ADR-0079, issue #180): the broken file's own passes do not
    // run at all. The project-wide passes (effects, throws) are filtered by path
    // instead — they walk the whole universe in one go, so there is no per-file
    // switch to turn off there.
    if inputs.unparsable.contains(inputs.units[fi].path) {
        return None;
    }
    let cx = Cx::new_with(
        inputs.units,
        inputs.index,
        fi,
        inputs.dam,
        inputs.postures,
        inputs.php_minor,
        inputs.catalog_skew,
        inputs.version_id,
        inputs.purity,
        inputs.layout.php_target(),
    );
    let (dead_spans, uncovered_entry) = walk_scopes(&cx, folder, out);
    file_passes(&cx, folder, &dead_spans, inputs.plugins, inputs.never_returning, out);
    direct_pass(&cx, folder, &dead_spans, out);
    Some(uncovered_entry)
}

/// The scope walk, which runs FIRST: the propagation pass walks every scope
/// and, as a side product, proves dead regions (decided branches, unreachable
/// tails) — the env-free passes after it must not report inside them
/// (live-path discipline, ADR-0002/0031). Binding descents contribute nothing
/// here: their deadness is per-binding, not universal.
///
/// Returns the dead spans and the file's `uncovered_matches` entry.
fn walk_scopes(
    cx: &Cx,
    folder: &mut dyn Folder,
    out: &mut Vec<Diagnostic>,
) -> (Vec<Span>, Vec<u32>) {
    let mut dead_spans: Vec<Span> = Vec::new();
    let mut uncovered_spans: Vec<Span> = Vec::new();
    for scope in cx.tree().scopes() {
        analyze_scope(
            cx,
            folder,
            scope,
            HashMap::new(),
            Store::default(),
            None,
            None,
            None,
            Some(&mut dead_spans),
            Some(&mut uncovered_spans),
            None,
            None,
            out,
        );
    }
    // Sorted and deduplicated: the consumer builds a `HashSet` from this,
    // so the canonical form costs nothing and makes the persisted row and
    // the verifier's comparison order-free.
    let mut uncovered_entry: Vec<u32> = uncovered_spans.iter().map(|s| s.start).collect();
    uncovered_entry.sort_unstable();
    uncovered_entry.dedup();
    (dead_spans, uncovered_entry)
}

/// The file's own passes after the scope walk, each judged once per file, in
/// the one order their findings are appended. A fixed list rather than a
/// registry: the passes read different inputs, and only some of them skip the
/// dead spans.
fn file_passes(
    cx: &Cx,
    folder: &mut dyn Folder,
    dead_spans: &[Span],
    plugins: &PluginFacts,
    never_returning: &HashSet<String>,
    out: &mut Vec<Diagnostic>,
) {
    // --- The `class.undefined` pass (ADR-0049 §5 / S4): the file's hard-error
    // class references, judged once each. A reference in a proven-dead region is
    // skipped — which IS this id's guard leg (a `class_exists('X')` whose class
    // meets the firing conditions folds its branch dead under the same closure).
    for r in cx.tree().hard_class_refs() {
        if in_dead(dead_spans, r.offset) {
            continue;
        }
        check_undefined_class(cx, folder, r, out);
    }

    // --- The `constant.undefined` pass (ADR-0078, issue #198): the file's bare
    // constant fetches, judged once each, with the same dead-region skip — which
    // IS this id's guard leg, exactly as it is for `class.undefined` above.
    for r in cx.tree().const_refs() {
        if in_dead(dead_spans, r.offset) {
            continue;
        }
        check_undefined_constant(cx, folder, r, out);
    }

    // --- `array.duplicate-key` (ADR-0078, issue #187): every literal array
    // in the file, judged once each. No dead-region gate — unlike the
    // passes above, this is a mechanics finding about how the literal is
    // WRITTEN, not a proof of a live runtime path, so it fires the same
    // whether or not the array is ever reached. -----------------------
    check_array_duplicate_keys(cx, out);

    // --- The declaration-fatal pass (ADR-0078 / issue #183): the file's own
    // class-like declarations, judged against the enumerated declaration graph.
    // Sidecar-free (a positive claim about resolved declarations, not an absence
    // of a symbol) and dam-free (the immunity asymmetry — no runtime construct
    // adds a method to a declared class), so it runs beside the pass above
    // without borrowing its ladder. -------------------------------------------
    check_declaration_fatals(cx, dead_spans, out);

    // --- Docblock hygiene (ADR-0078 / issue #186): the mechanics-layer
    // anti-rot family. Textual premises only — no env, no folder, no dead-region
    // filter: an annotation that names a subject the code no longer has is rot
    // wherever it sits, including in a branch that never runs.
    docblock_hygiene(cx, out);

    // --- `phpdoc.unknown-vocabulary` (ADR-0091 §6 / issue #479): a
    // hyphenated identifier in a type position that denotes nothing. Reads
    // the docblock and the loaded plugin channel, nothing else — the claim
    // is about what the annotation can possibly mean, so no env, no folder,
    // no dead-region filter, exactly as the hygiene family above. The
    // allowlist is assembled here because it is a per-project value: the
    // question must be asked after plugin load (ADR-0091 §4.1).
    unknown_vocabulary(cx, &VocabularyAllowlist::for_project(plugins), out);

    // --- The untyped surface (ADR-0078 / issue #200): the contract-layer
    // `untyped.*` family. Declaration reading only — no env, no folder, no
    // dead-region filter, no sidecar: a declaration that withholds its type
    // withholds it wherever it sits. -----------------------------------------
    untyped_surface(cx, out);

    // --- `type.return-missing` (ADR-0078 / issue #199): the reachability
    // foundation's tracer. Declaration premise plus a structural terminality
    // verdict, so — like the two passes above — no env, no folder, no
    // dead-region filter: a body that runs off its end does so wherever it
    // sits, and the judgement is about the body's own shape.
    check_return_missing(cx, never_returning, out);

    // --- `variable.undefined` (ADR-0078 / issue #194): every read of a name
    // its scope never binds. A per-scope textual/structural pass over the
    // lowering-computed firing set, plus the warning-handler posture and the
    // out-parameter subtraction. No dead-region filter and no folder: the
    // premise is that the scope's own text holds no binding form, which is
    // true wherever the read sits. -------------------------------------------
    check_undefined_variables(cx, out);
    check_phpdoc_maybe_undefined(cx, out);
}

/// The direct pass, last: literal / array / `new` arguments at every function
/// call site (env-free; propagation adds `$var`/folded resolution). Native
/// scalar checks and the phpdoc declared-contract check both run here; a site
/// where the native check fired is skipped by the phpdoc check (no
/// double-report; ADR-0030). Calls in proven-dead regions are skipped.
fn direct_pass(cx: &Cx, folder: &mut dyn Folder, dead_spans: &[Span], out: &mut Vec<Diagnostic>) {
    let empty_env: HashMap<String, Known> = HashMap::new();
    let empty_classes: Store = Store::default();
    for call in cx.tree().calls() {
        if in_dead(dead_spans, call.span.start) {
            continue;
        }
        // Resolve the positional prefix of a mixed call too (Gap A) — the guard
        // that keeps the binding descent positional-only lives on the descent path.
        let Some(site) = cx.resolve_user_fn_any(call) else { continue };
        let decl = cx.fn_decl(site);
        let envelopes = cx.envelopes_of(decl.docblock.as_deref(), site.file, decl.span.start);
        for (i, arg) in call.args.iter().enumerate() {
            let Some(param) = decl.params.get(i) else { break };
            if param.variadic {
                break;
            }
            if param.by_ref {
                continue;
            }
            let mut native_fired = false;
            // Env-free resolution: a literal, a proven object (`new` / enum
            // case), or a resolved class constant (ADR-0043 stage 3). At file
            // scope there is no enclosing class for `self`/`parent`.
            if let Some(ty) = param.ty.as_ref()
                && let Some(checkable) = cx.resolve_static_value(&arg.value, None)
                && is_type_error(cx, ty, &checkable)
                && !implicit_null_accepted(param, &checkable)
            {
                out.push(cx.diagnostic(
                    arg.span.start,
                    &checkable,
                    None,
                    &decl.name,
                    &param.name,
                    ty,
                ));
                native_fired = true;
            }
            // The direct pass owns env-free arg kinds (literal / array / `new`,
            // plus enum-case / class-const object values — ADR-0043 stage 4);
            // `$var`/`call()` resolution — and their phpdoc check — belong to the
            // propagation pass, so the two never both fire on one arg.
            let env_free = arg.value.is_literal()
                || matches!(
                    arg.value,
                    ArgValue::Array(_)
                        | ArgValue::New(..)
                        | ArgValue::EnumCase(..)
                        | ArgValue::ClassConst(..)
                );
            if !native_fired
                && env_free
                && let Some(env) = &envelopes
            {
                check_phpdoc_param(
                    cx,
                    folder,
                    env,
                    param,
                    site.file,
                    decl.span.start,
                    &decl.name,
                    arg.span.start,
                    &arg.value,
                    &empty_env,
                    &empty_classes,
                    false,
                    false, // in_descent — the direct pass is never a descent
                    out,
                );
            }
            // Callable-signature variance (issue #11): a closure / first-class
            // callable argument against a signature-bearing `callable(...)`
            // @param. Env-free (a closure's declared signature is a static CST
            // fact), so the direct pass owns it — no overlap with the
            // propagation pass, which owns `$var`/`call()` arg kinds.
            if let ArgValue::Closure(closure) = &arg.value
                && let Some(env) = &envelopes
            {
                check_callable_arg(cx, env, param, &decl.name, arg.span.start, closure, out);
            }
        }
    }
}
