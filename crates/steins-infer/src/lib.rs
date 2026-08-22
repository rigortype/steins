//! The inference engine — now whole-project (cross-file) resolution.
//!
//! It implements the proof-layer diagnostics (ADR-0002, held to the
//! zero-false-positive bar): [`ID`] = `type.argument-mismatch`, plus the
//! effect-envelope checks. A call to a **user-defined function or method
//! resolved anywhere in the project** that passes a **literal** argument which
//! **provably** raises a runtime `TypeError` under PHP 8.1+ semantics
//! (ADR-0011), honoring the calling file's `declare(strict_types=1)`, is
//! flagged. Everything not provable is silent.
//!
//! Name resolution follows PHP semantics conservatively (ADR-0001): fully-
//! qualified / qualified / unqualified names resolve against a project symbol
//! index ([`steins_db::project_index`]) plus the builtin catalog, with
//! `use` imports and the namespace/global fallback applied. Ambiguous symbols
//! (duplicate FQN, builtin-shadowing) are never resolved — silent.
//!
//! The single-file entry points ([`check`], [`check_file`], [`diagnostics`])
//! run over a one-file project, so every same-file soundness guard keeps
//! working unchanged; [`check_project`] / [`annotate_project`] run over many.

mod absence;
mod annotate;
mod arg_check;
mod arity;
mod assert_harness;
mod asserts;
mod assign;
mod branch;
mod builtin_returns;
mod coerce;
mod compare;
mod cond;
mod contract;
mod cx;
pub mod dam;
mod declared_receiver;
mod descent;
mod dispatch;
mod docblock_hygiene;
mod dump;
pub mod effects;
mod env;
pub mod escapes;
mod existence;
mod fold;
mod fold_args;
#[cfg(not(target_arch = "wasm32"))]
mod fold_process;
mod fold_table;
mod foreach_check;
mod generics;
mod heap;
mod ids;
mod inaccessible;
mod mechanics;
mod method_call;
mod non_object;
mod offsets;
mod operands;
mod out_params;
mod overrides;
mod predicates;
pub mod profile;
mod project;
pub mod promote;
mod purity;
mod refine;
mod return_arms;
mod return_missing;
mod shape_projection;
mod shapes;
mod string_context;
pub mod suppress;
mod throws;
mod transfers;
mod undefined_var;
mod untyped;
mod walk;

pub use dam::{DamFacts, DamKind, DamSite, dam_facts};
pub use ids::*;
pub use purity::{EffectSummary, RegionPurity, effect_summary, region_purity_project};
pub use absence::{SAPI_PROVIDED_FUNCTIONS_EXACT, SAPI_PROVIDED_FUNCTION_PREFIXES};
pub use annotate::{
    FactKind, LineFact, annotate_facts, annotate_file, annotate_project, effect_summaries_file,
    effect_summaries_project,
};
pub use assert_harness::{AssertObservation, SubjectFact, collect_assert_types, probe_subjects};
pub use project::{
    Diagnostic, FileUnit, Fix, FixEdit, MagicObstacle, is_vendor_path, magic_obstacles,
    magic_obstacles_reaching, resolves_to_user_function,
};

use absence::{check_undefined_class, check_undefined_constant};
use mechanics::{check_array_duplicate_keys, emit_parse_failure};
use overrides::check_declaration_fatals;
use return_missing::{check_return_missing, never_returning_names};

use arg_check::{implicit_null_accepted, is_type_error};
use builtin_returns::fact_with_null;
use contract::CVal;
use generics::{check_callable_arg, check_phpdoc_param};

use cx::Cx;
use dump::render_shape_fact;
use env::{Known, Store};
use project::Index;
use walk::{analyze_scope, in_dead};

use fold_args::effective_php_view;

/// The `[runtime] final-keyword` posture (issue #234), re-exported so the CLI can
/// resolve `steins.toml` into it without depending on steins-contract directly —
/// mirrors [`check_project_with_runtime`]'s `warning_handler_abort` parameter.
/// Unused until intersection consumption (issue #238) joins it on `Cx`.
pub use steins_contract::normalize::FinalKeyword;
/// The catalog's refusal axis, re-exported: a consumer of [`SurfaceSummary`]
/// reads the classification without naming `steins-catalog`.
pub use steins_catalog::RefusalAxis;
pub use suppress::{
    DIAGNOSTIC_IDS, DIAGNOSTIC_REGISTRY, FACET_ORIGIN, Facet, Floor, InlineOutcome, Layer, Origin,
    SUPPRESS_UNKNOWN_ID, SUPPRESS_UNMATCHED_ID, apply_inline_ignores, declared_facet, layer,
    pattern_is_known, pattern_matches, surface_floor,
};

use std::collections::{HashMap, HashSet};

use steins_db::{
    Db, EffectsPolicy, PluginFacts, Project, ProjectLayout, SourceFile, parse, project_index,
};
use steins_syntax::Span;
use steins_syntax::{ArgValue, ArrayKey, FunctionDecl, NormKey, SourceTree};
// return missing (ADR-0078, issue #199)
pub use steins_syntax::{BodyEnd, body_end, body_has_terminator};
pub use fold::{
    EngineFolder, FoldEngine, FoldLane, FoldPosture, Folder, MONKEY_PATCH_EXTENSIONS, NoFold,
    RefusalNote, SurfaceSummary,
};
#[cfg(not(target_arch = "wasm32"))]
pub use fold_process::{ProcessEngine, SidecarFolder};
pub use fold_table::{TableEngine, TableFolder, request_key};
// end return missing (ADR-0078, issue #199)

use steins_phpdoc::ast::TypeKind as PKind;
use steins_domain::{Base, Fact, IntRange, Key as VKey, Refinement, StrPreds, Val};
use steins_phpdoc::Type as PType;

use docblock_hygiene::docblock_hygiene;
use purity::{PurityOracle, effect_diagnostics};
use throws::throw_diagnostics;
use undefined_var::{check_phpdoc_maybe_undefined, check_undefined_variables};
use untyped::untyped_surface;

/// The maximum depth of interprocedural argument-binding descent (Feature B).
///
/// ADR-0009 makes inference cutoffs a first-class budget discipline: a chain of
/// calls propagating a literal is followed at most this many frames deep, after
/// which the descent stops with **no** diagnostic (a cutoff names itself as
/// silence, never a manufactured finding). Direct and indirect recursion is
/// caught earlier by the on-stack binding set; this bound guards against merely
/// long, non-cyclic chains.
pub const MAX_BINDING_DEPTH: usize = 8;

/// The one-line coverage-posture notice (ADR-0004): printed to stderr when a run
/// executes as the sound subset because the PHP sidecar is unavailable, and served
/// as the browser envelope's `notice` field for the engine-off playground (ADR-0065).
///
/// The second clause is ADR-0069's: with no engine to reflect them, a builtin's
/// return type comes from the catalog's mined declaration, which is a claim rather
/// than a runtime answer — so the sentence says so where the posture is stated.
pub const SOUND_SUBSET_NOTICE: &str = "note: running as sound subset (no PHP sidecar) — findings that require executing PHP are omitted, and builtin return types come from the catalog's declarations, unverified";

/// The notice for issue #110's degradation mode: PHP spawned but the opening
/// handshake or a later request stopped answering. This differs from
/// [`SOUND_SUBSET_NOTICE`], where PHP is unavailable; `steins doctor` diagnoses
/// the unresponsive-process case. [`ProcessEngine`] emits this at most once per
/// run, and it never changes the exit status.
pub const SIDECAR_HANDSHAKE_NOTICE: &str = "note: PHP sidecar stopped answering — running as sound subset (degraded): findings that require executing PHP are omitted, and builtin return types come from the catalog's declarations, unverified; run `steins doctor` for detail";

// ---------------------------------------------------------------------------
// Public entry points.
// ---------------------------------------------------------------------------

/// The proof-layer diagnostics for one file, as a memoized salsa query (sound
/// subset — [`NoFold`], no PHP). Analyzes the file as a one-file project.
#[salsa::tracked]
pub fn diagnostics(db: &dyn Db, file: SourceFile) -> Vec<Diagnostic> {
    let tree = parse(db, file);
    let units = [FileUnit { path: file.path(db), tree }];
    let index = Index::from_units(&units);
    check_units(
        &units,
        &index,
        &mut NoFold,
        true,
        FinalKeyword::Enforced,
        &ProjectLayout::fallback(),
        &PluginFacts::none(),
        &EffectsPolicy::none(),
    )
}

/// The folding-aware check for one file (run **outside** salsa; ADR-0004),
/// analyzed as a one-file project.
#[must_use]
pub fn check_file(db: &dyn Db, file: SourceFile, folder: &mut dyn Folder) -> Vec<Diagnostic> {
    let tree = parse(db, file);
    let units = [FileUnit { path: file.path(db), tree }];
    let index = Index::from_units(&units);
    check_units(
        &units,
        &index,
        folder,
        true,
        FinalKeyword::Enforced,
        &ProjectLayout::fallback(),
        &PluginFacts::none(),
        &EffectsPolicy::none(),
    )
}

/// The folding-aware check for a whole **project** (ADR-0009/0015): every file
/// in `project` is analyzed as one unit, so cross-file calls, class chains, and
/// effects resolve. Resolution is driven by the salsa [`project_index`] query.
#[must_use]
pub fn check_project(db: &dyn Db, project: Project, folder: &mut dyn Folder) -> Vec<Diagnostic> {
    check_project_with_runtime(db, project, folder, true)
}

/// [`check_project`] with the `[runtime]` pseudo-constants declared (ADR-0049 §7):
/// `warning_handler_abort` (the `warning-handler` posture) is `true` for the default
/// `"abort"` — proven warning-grade offset findings emit — and `false` for `"null"`,
/// which silences them. The default entry point ([`check_project`]) passes `true`:
/// the safe production default. (The former `zend_assertions` knob was abolished by
/// the 2026-07-25 owner ruling — `assert($expr)` is `Verified` unconditionally.)
#[must_use]
pub fn check_project_with_runtime(
    db: &dyn Db,
    project: Project,
    folder: &mut dyn Folder,
    warning_handler_abort: bool,
) -> Vec<Diagnostic> {
    check_project_with_postures(db, project, folder, warning_handler_abort, FinalKeyword::Enforced)
}

/// [`check_project_with_runtime`] plus the `[runtime] final-keyword` posture
/// (issue #234, consumed by #238).
///
/// Both `[runtime]` pseudo-constants in one entry point, since they are one
/// family (ADR-0037 §2): a boot truth no amount of reading source settles,
/// which the project declares and Steins reasons under. `final_keyword` reaches
/// exactly one consumer — the declared-receiver lane's intersection leg — and
/// [`FinalKeyword::Enforced`] is what declaring nothing means, so
/// [`check_project_with_runtime`] delegating with it keeps every existing
/// caller's semantics byte-identical.
#[must_use]
pub fn check_project_with_postures(
    db: &dyn Db,
    project: Project,
    folder: &mut dyn Folder,
    warning_handler_abort: bool,
    final_keyword: FinalKeyword,
) -> Vec<Diagnostic> {
    let handles: Vec<SourceFile> = project.files(db).to_vec();
    let units: Vec<FileUnit> =
        handles.iter().map(|&f| FileUnit { path: f.path(db), tree: parse(db, f) }).collect();
    let db_index = project_index(db, project);
    let pos: HashMap<SourceFile, usize> =
        handles.iter().enumerate().map(|(i, &f)| (f, i)).collect();
    let index = Index::from_db(db_index, &pos, &units);
    check_units(
        &units,
        &index,
        folder,
        warning_handler_abort,
        final_keyword,
        project.layout(db),
        project.plugins(db),
        project.effects(db),
    )
}

/// The pure single-file check (sound subset). Kept for unit tests and callers
/// that never execute PHP. `functions` is accepted for signature stability; the
/// tree's own function list is authoritative.
#[must_use]
pub fn check(tree: &SourceTree, functions: &[FunctionDecl], path: &str) -> Vec<Diagnostic> {
    check_with(tree, functions, path, &mut NoFold)
}

/// The folding-aware single-file check core, analyzed as a one-file project.
#[must_use]
pub fn check_with(
    tree: &SourceTree,
    functions: &[FunctionDecl],
    path: &str,
    folder: &mut dyn Folder,
) -> Vec<Diagnostic> {
    let _ = functions; // authoritative list comes from `tree.functions()`
    let units = [FileUnit { path, tree }];
    let index = Index::from_units(&units);
    check_units(
        &units,
        &index,
        folder,
        true,
        FinalKeyword::Enforced,
        &ProjectLayout::fallback(),
        &PluginFacts::none(),
        &EffectsPolicy::none(),
    )
}

/// The single-file check with a folder **and** the `warning-handler` posture
/// (`warning_handler_abort`, ADR-0049 §7). Kept for tests that must exercise both a
/// live folder (the offset family is gated on [`Folder::absence_family_available`],
/// ADR-0049 A9) and a chosen `warning-handler` posture. (The former `zend_assertions`
/// knob was abolished by the 2026-07-25 owner ruling — `assert($expr)` is `Verified`
/// unconditionally, so no runtime knob controls its stratum.)
#[must_use]
pub fn check_full(
    tree: &SourceTree,
    path: &str,
    folder: &mut dyn Folder,
    warning_handler_abort: bool,
) -> Vec<Diagnostic> {
    let units = [FileUnit { path, tree }];
    let index = Index::from_units(&units);
    check_units(
        &units,
        &index,
        folder,
        warning_handler_abort,
        FinalKeyword::Enforced,
        &ProjectLayout::fallback(),
        &PluginFacts::none(),
        &EffectsPolicy::none(),
    )
}

/// The project checking core: direct + propagation passes over every file's
/// calls and scopes, then the one project-wide effects pass.
#[allow(clippy::too_many_arguments)]
fn check_units(
    units: &[FileUnit],
    index: &Index,
    folder: &mut dyn Folder,
    warning_handler_abort: bool,
    final_keyword: FinalKeyword,
    layout: &ProjectLayout,
    plugins: &PluginFacts,
    policy: &EffectsPolicy,
) -> Vec<Diagnostic> {
    let mut out = Vec::new();

    // The whole-universe dam fact (ADR-0049 §2): one query answer per run, shared by
    // every file's context. Consumed by the absence family's conditional-decl leg.
    let dam = dam_facts(units, layout);

    // The analysis PHP view (issue #28): the TARGET the project declares
    // (`config.platform.php` / `require.php`, via the layout) is what
    // version-sensitive decisions key on; the sidecar's runtime minor is the
    // fallback when the project declares nothing. One computation per run,
    // shared by every file's context — ADR-0052 A11 (catalog skew) and
    // ADR-0049 A12 (the next-int rule, through `normalize_array`) both follow
    // this one seam.
    let runtime_minor = folder.php_minor();
    let view = effective_php_view(runtime_minor, layout.php_target());
    let (php_minor, catalog_skew) = (view.effective_minor, view.catalog_skew);
    // The PHP_VERSION_ID guard fold (issue #29) is disabled project-wide the
    // moment any file declares a userland constant of that name — constant
    // resolution is otherwise unmodeled, so the conservative reading is the
    // only sound one.
    let version_id = if units.iter().any(|u| u.tree.php_version_id_declared()) {
        None
    } else {
        view.version_id
    };

    // The callable-purity oracle (ADR-0063 P3): one whole-project effect fixpoint per
    // run, shared by every file's context, and built only when some docblock actually
    // spells a purity-bearing callable.
    let purity = PurityOracle::build(units, index, plugins, policy);

    // parse failure (ADR-0079, issue #180): `parse_errors()`'s first real consumer.
    // One finding per broken file at its first error, and then NOTHING else from
    // that file — its recovered tree may misattribute anything locally, and a
    // finding built on a misparse is the manufactured-FP shape ADR-0002 forbids
    // (§2.4). The declarations the recovery kept still sit in the index, where they
    // can only *silence* an absence claim, never fire one.
    //
    // Vendor is NOT special here, only in the dam (§2.3): a broken vendor file
    // emits the finding too and it rides the CLI's ordinary vendor filter, exactly
    // as the ADR-0046 §2 presumption prescribes.
    for u in units {
        emit_parse_failure(u, dam.file_is_unparsable(u.path), &mut out);
    }
    let unparsable: HashSet<&str> =
        units.iter().filter(|u| !u.tree.parse_errors().is_empty()).map(|u| u.path).collect();
    // end parse failure (ADR-0079, issue #180)

    // return missing (ADR-0078, issue #199): the whole-run veto set, computed once
    // because a never-returning helper is routinely declared in a different file
    // from the body that calls it.
    let never_returning = never_returning_names(units);
    // end return missing (ADR-0078, issue #199)

    // ADR-0088 §5 (issue #433): the dataflow walk's own verdict on which
    // default-less `match` statements do NOT cover their subject's Verified
    // domain, keyed by (file, span-start) — the same key the structural throw
    // scan's `ThrowKind::New` origin for the same construct carries (both trace
    // back to the same CST `Match` node). Populated below, read by
    // `throw_diagnostics` at the end.
    let mut uncovered_matches: HashMap<usize, HashSet<u32>> = HashMap::new();

    for fi in 0..units.len() {
        // parse failure (ADR-0079, issue #180): the broken file's own passes do not
        // run at all. The project-wide passes below (effects, throws) are filtered
        // by path instead — they walk the whole universe in one go, so there is no
        // per-file switch to turn off there.
        if unparsable.contains(units[fi].path) {
            continue;
        }
        let cx = Cx::new_with(
            units,
            index,
            fi,
            &dam,
            warning_handler_abort,
            final_keyword,
            php_minor,
            catalog_skew,
            version_id,
            purity.as_ref(),
            layout.php_target(),
        );

        // --- Propagation pass FIRST: it walks every scope and, as a side
        // product, proves dead regions (decided branches, unreachable tails) —
        // the env-free direct pass below must not report inside them
        // (live-path discipline, ADR-0002/0031). Binding descents contribute
        // nothing here: their deadness is per-binding, not universal. ---------
        let mut dead_spans: Vec<Span> = Vec::new();
        let mut uncovered_spans: Vec<Span> = Vec::new();
        for scope in cx.tree().scopes() {
            analyze_scope(
                &cx,
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
                &mut out,
            );
        }
        uncovered_matches.insert(fi, uncovered_spans.iter().map(|s| s.start).collect());

        // --- The `class.undefined` pass (ADR-0049 §5 / S4): the file's hard-error
        // class references, judged once each. A reference in a proven-dead region is
        // skipped — which IS this id's guard leg (a `class_exists('X')` whose class
        // meets the firing conditions folds its branch dead under the same closure).
        for r in cx.tree().hard_class_refs() {
            if in_dead(&dead_spans, r.offset) {
                continue;
            }
            check_undefined_class(&cx, folder, r, &mut out);
        }

        // --- The `constant.undefined` pass (ADR-0078, issue #198): the file's bare
        // constant fetches, judged once each, with the same dead-region skip — which
        // IS this id's guard leg, exactly as it is for `class.undefined` above.
        for r in cx.tree().const_refs() {
            if in_dead(&dead_spans, r.offset) {
                continue;
            }
            check_undefined_constant(&cx, folder, r, &mut out);
        }

        // --- `array.duplicate-key` (ADR-0078, issue #187): every literal array
        // in the file, judged once each. No dead-region gate — unlike the
        // passes above, this is a mechanics finding about how the literal is
        // WRITTEN, not a proof of a live runtime path, so it fires the same
        // whether or not the array is ever reached. -----------------------
        check_array_duplicate_keys(&cx, &mut out);

        // --- The declaration-fatal pass (ADR-0078 / issue #183): the file's own
        // class-like declarations, judged against the enumerated declaration graph.
        // Sidecar-free (a positive claim about resolved declarations, not an absence
        // of a symbol) and dam-free (the immunity asymmetry — no runtime construct
        // adds a method to a declared class), so it runs beside the pass above
        // without borrowing its ladder. -------------------------------------------
        check_declaration_fatals(&cx, &dead_spans, &mut out);

        // --- Docblock hygiene (ADR-0078 / issue #186): the mechanics-layer
        // anti-rot family. Textual premises only — no env, no folder, no dead-region
        // filter: an annotation that names a subject the code no longer has is rot
        // wherever it sits, including in a branch that never runs.
        docblock_hygiene(&cx, &mut out);

        // --- The untyped surface (ADR-0078 / issue #200): the contract-layer
        // `untyped.*` family. Declaration reading only — no env, no folder, no
        // dead-region filter, no sidecar: a declaration that withholds its type
        // withholds it wherever it sits. -----------------------------------------
        untyped_surface(&cx, &mut out);

        // --- `type.return-missing` (ADR-0078 / issue #199): the reachability
        // foundation's tracer. Declaration premise plus a structural terminality
        // verdict, so — like the two passes above — no env, no folder, no
        // dead-region filter: a body that runs off its end does so wherever it
        // sits, and the judgement is about the body's own shape.
        check_return_missing(&cx, &never_returning, &mut out);

        // --- `variable.undefined` (ADR-0078 / issue #194): every read of a name
        // its scope never binds. A per-scope textual/structural pass over the
        // lowering-computed firing set, plus the warning-handler posture and the
        // out-parameter subtraction. No dead-region filter and no folder: the
        // premise is that the scope's own text holds no binding form, which is
        // true wherever the read sits. -------------------------------------------
        check_undefined_variables(&cx, &mut out);
        check_phpdoc_maybe_undefined(&cx, &mut out);

        // --- Direct pass: literal / array / `new` arguments at every function
        // call site (env-free; propagation adds `$var`/folded resolution). Native
        // scalar checks and the phpdoc declared-contract check both run here; a
        // site where the native check fired is skipped by the phpdoc check (no
        // double-report; ADR-0030). Calls in proven-dead regions are skipped. ---
        let empty_env: HashMap<String, Known> = HashMap::new();
        let empty_classes: Store = Store::default();
        for call in cx.tree().calls() {
            if in_dead(&dead_spans, call.span.start) {
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
                    && is_type_error(&cx, ty, &checkable)
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
                        ArgValue::Array(_) | ArgValue::New(..) | ArgValue::EnumCase(..) | ArgValue::ClassConst(..)
                    );
                if !native_fired
                    && env_free
                    && let Some(env) = &envelopes
                {
                    check_phpdoc_param(
                        &cx,
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
                        &mut out,
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
                    check_callable_arg(&cx, env, param, &decl.name, arg.span.start, closure, &mut out);
                }
            }
        }

    }

    // --- Effects pass (ADR-0005), computed once over the whole project. ------
    out.extend(effect_diagnostics(units, index, plugins, policy));

    // --- Throw system (ADR-0040/0007): `@throws` envelope + Liskov. ----------
    out.extend(throw_diagnostics(units, index, &uncovered_matches));

    // parse failure (ADR-0079, issue #180): drop whatever the two project-wide
    // passes above attributed to a broken file. §2.4 is about the file, not about
    // which pass produced the finding.
    if !unparsable.is_empty() {
        out.retain(|d| d.id == SYNTAX_UNPARSABLE_ID || !unparsable.contains(d.path.as_str()));
    }

    dedup(&mut out);
    out
}

/// Drop exact-duplicate diagnostics, preserving first-occurrence order.
fn dedup(out: &mut Vec<Diagnostic>) {
    let mut seen: HashSet<Diagnostic> = HashSet::new();
    out.retain(|d| seen.insert(d.clone()));
}

/// A node in the unified project effect call graph — a free function (keyed by
/// FQN) or a class method (keyed by class FQN + method name).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Sym {
    Func(String),
    Method(String, String),
    /// A closure/arrow body (ADR-0033), keyed by file path + definition-site
    /// offset (closures are same-file, so this key is stable within a project).
    Closure(String, u32),
}

/// Join `f` into an accumulator that may still be empty; `None` propagates the
/// unrepresentable join as the unknown floor.
fn join_into(acc: Option<Fact>, f: &Fact) -> Option<Option<Fact>> {
    match acc {
        None => Some(Some(f.clone())),
        Some(a) => a.join(f).map(Some),
    }
}

/// The domain value a shape key denotes (`Key::Int(5)` is the value `5`).
fn val_of_key(k: &VKey) -> Val {
    match k {
        VKey::Int(i) => Val::Int(*i),
        VKey::Str(s) => Val::Str(s.clone()),
    }
}

/// Is every value this fact admits an `int`? (`null` is immaterial to
/// [`project_flip`]'s question — a null value is skipped by the flip, not turned
/// into a key.)
fn fact_is_int(f: &Fact) -> bool {
    match f.finite_members() {
        Some(vals) => vals.iter().all(|v| matches!(v, Val::Int(_) | Val::Null)),
        None => matches!(
            f,
            Fact::General { base: Base::Int, .. } | Fact::Refined { base: Base::Int, .. }
        ),
    }
}

/// Add `null` to a fact's denotation — the finite layers by value, the abstract
/// ones through their own `nullable` flag. `None` when the result is not
/// representable (a shape fact, or an over-cap finite widening).
fn fact_admitting_null(f: &Fact) -> Option<Fact> {
    match f.finite_members() {
        Some(vals) => {
            let mut vals = vals.to_vec();
            vals.push(Val::Null);
            Fact::from_vals(vals)
        }
        None => fact_with_null(f),
    }
}

/// The abstract fact an argument resolves to: a bare `$var` whose env fact is an
/// abstract layer (no finite members). Finite/proven values go through
/// `resolve_cval` instead, so this is the disjoint "abstract" arm of Feature E.
fn arg_abstract_fact<'e>(
    value: &ArgValue,
    env: &'e HashMap<String, Known>,
    poisoned: bool,
) -> Option<&'e Fact> {
    if poisoned {
        return None;
    }
    let ArgValue::Var(name) = value else { return None };
    let f = env.get(name)?.fact.as_ref()?;
    f.finite_members().is_none().then_some(f)
}

/// Whether a lowered contract type contains a class-name node — a bare identifier
/// that may actually be a template or a type-alias. The abstract-fact check stays
/// silent on these (see [`check_phpdoc_param`]).
fn contract_touches_class(ty: &steins_contract::ContractTy) -> bool {
    use steins_contract::ContractTy as C;
    match ty {
        C::Class(_) => true,
        C::Union(m) | C::Inter(m) => m.iter().any(contract_touches_class),
        C::ListOf { elem, .. } => contract_touches_class(elem),
        C::MapOf { key, val, .. } | C::IterableOf { key, val } => {
            contract_touches_class(key) || contract_touches_class(val)
        }
        C::Shape { fields, unsealed, .. } => {
            fields.iter().any(|f| contract_touches_class(&f.ty))
                || unsealed.as_ref().is_some_and(|(k, v)| {
                    k.as_ref().is_some_and(|k| contract_touches_class(k))
                        || contract_touches_class(v)
                })
        }
        _ => false,
    }
}

/// ADR-0043 stage 4 — the phpdoc-side analogue of [`object_world_guard_blind`]. A
/// class-touching phpdoc verdict is unsound inside a binding descent: the callee's
/// in-body type guards on the rebound value are unmodeled. "Touches a class"
/// means the proven value is an object, or the contract references a class name.
/// Scalar-vs-scalar phpdoc checks are unaffected. Always `false` outside a descent.
fn phpdoc_object_guard_blind(in_descent: bool, ty: &PType, cv: Option<&CVal>) -> bool {
    in_descent
        && (matches!(cv, Some(CVal::Object(..)))
            || contract_touches_class(&steins_contract::lower(ty)))
}

/// ADR-0043 stage 4 — is `ty` a **pure class contract**: a known class name, or a
/// union/nullable built only from known class names and `null` (e.g. `Foo`,
/// `Foo|null`, `?Foo`, `A|B`)? Only such a contract may let a definite scalar fact
/// open the [`contract_touches_class`] valve. `is_known_class` is the safety
/// valve — an unresolved bare identifier may be a `@template`/`@phpstan-type`
/// alias denoting a scalar, disqualifying the whole contract. A contract touching
/// array/generic/shape/intersection/callable, or any scalar/pseudo-type keyword,
/// is *not* pure-class.
fn is_pure_class_contract(cx: &Cx, cfile: usize, coff: u32, ty: &PType) -> bool {
    fn walk(cx: &Cx, cfile: usize, coff: u32, ty: &PType, saw_class: &mut bool) -> bool {
        match &ty.kind {
            PKind::Identifier(name) => {
                // A `null` companion (the `class|null` shape) is allowed but is not
                // itself the class that satisfies the "at least one class" rule.
                if name.eq_ignore_ascii_case("null") {
                    return true;
                }
                let target = cx.resolve_pclass(cfile, coff, name);
                if cx.is_known_class(&target) {
                    *saw_class = true;
                    true
                } else {
                    false
                }
            }
            PKind::Nullable(inner) => walk(cx, cfile, coff, inner, saw_class),
            PKind::Union { types, .. } => {
                types.iter().all(|t| walk(cx, cfile, coff, t, saw_class))
            }
            _ => false,
        }
    }
    let mut saw_class = false;
    walk(cx, cfile, coff, ty, &mut saw_class) && saw_class
}

/// A short, phpdoc-flavored description of an abstract fact for a diagnostic
/// message (`a value of type int`, `a non-empty-string value`, `an int|null
/// value`). Finite facts never reach here (they render as concrete values).
fn describe_fact(f: &Fact) -> String {
    let base_kw = |b: Base| match b {
        Base::Int => "int",
        Base::Float => "float",
        Base::String => "string",
        Base::Bool => "bool",
    };
    let (name, nullable) = match f {
        Fact::General { base, nullable } => (base_kw(*base).to_owned(), *nullable),
        Fact::Refined { base: Base::Int, refinement: Refinement::Int(r), nullable } => {
            let n = if *r == IntRange::POSITIVE {
                "positive-int".to_owned()
            } else if *r == IntRange::NEGATIVE {
                "negative-int".to_owned()
            } else if *r == IntRange::NON_NEGATIVE {
                "non-negative-int".to_owned()
            } else {
                format!("int<{}, {}>", r.lo(), r.hi())
            };
            (n, *nullable)
        }
        Fact::Refined { base: Base::String, refinement: Refinement::Str(p), nullable } => {
            let casing = match (
                p.contains_all(StrPreds::LOWERCASE),
                p.contains_all(StrPreds::UPPERCASE),
            ) {
                (true, false) => Some("lowercase"),
                (false, true) => Some("uppercase"),
                // Neither, or both (nothing cased to change): no single keyword.
                _ => None,
            };
            let n = if p.contains_all(StrPreds::NON_FALSY) {
                "non-falsy-string".to_owned()
            } else if p.contains_all(StrPreds::NUMERIC) {
                "numeric-string".to_owned()
            } else if let Some(c) = casing {
                if p.contains_all(StrPreds::NON_EMPTY) {
                    format!("non-empty-{c}-string")
                } else {
                    format!("{c}-string")
                }
            } else if p.contains_all(StrPreds::NON_EMPTY) {
                "non-empty-string".to_owned()
            } else {
                "string".to_owned()
            };
            (n, *nullable)
        }
        Fact::Refined { base, nullable, .. } => (base_kw(*base).to_owned(), *nullable),
        // A union spells arm by arm through this same speller, joined by `|`
        // (issue #339). The arms carry no `null` of their own — the union's
        // flag does — so each is rendered non-nullable and the null half is
        // added once, below, exactly as it is for a single base.
        Fact::Union { arms, nullable } => {
            let spelled: Vec<String> = arms
                .iter()
                .map(|(base, refinement)| {
                    let arm = match refinement {
                        Some(r) => Fact::refined(*base, *r, false),
                        None => Fact::General { base: *base, nullable: false },
                    };
                    describe_fact(&arm)
                        .trim_start_matches("a value of type ")
                        .to_owned()
                })
                .collect();
            (spelled.join("|"), *nullable)
        }
        // The array stratum reaches this surface as of ADR-0072 (a shape fact is
        // now judged against a contract, so it can be the thing a
        // `phpdoc.*-mismatch` names). It spells through the ONE speller the dump
        // surface uses — `render_shape_fact` already carries the null half, so
        // the `nullable` flag stays `false` here rather than doubling it.
        Fact::Shape { shape, nullable } => (render_shape_fact(shape, *nullable), false),
        // Finite facts do not reach here: the callers gate on `finite_members`.
        Fact::Singleton(_) | Fact::OneOf(_) => ("value".to_owned(), false),
    };
    if nullable {
        format!("a value of type {name}|null")
    } else {
        format!("a value of type {name}")
    }
}

/// Render a proven [`CVal`] for a diagnostic message (delegates arrays/scalars to
/// [`ArgValue::render`]; objects show `new Class()`).
fn rendered_cval(v: &CVal) -> String {
    match v {
        CVal::Scalar(s) => s.render(),
        CVal::Object(class, _) => format!("new {}()", class.rsplit('\\').next().unwrap_or(class)),
        CVal::Resource => "a resource".to_owned(),
        CVal::Array(entries) => {
            // Rebuild an `ArgValue::Array` with explicit keys so the shared compact
            // renderer applies (it re-normalizes; explicit keys round-trip).
            let items: Vec<(ArrayKey, ArgValue)> = entries
                .iter()
                .map(|(k, cv)| {
                    let key = match k {
                        NormKey::Int(i) => ArrayKey::Int(*i),
                        NormKey::Str(s) => ArrayKey::Str(s.clone()),
                    };
                    (key, cval_to_argvalue(cv))
                })
                .collect();
            ArgValue::Array(items).render()
        }
    }
}

/// A best-effort [`ArgValue`] reconstruction of a [`CVal`], for rendering only.
fn cval_to_argvalue(v: &CVal) -> ArgValue {
    match v {
        CVal::Scalar(s) => s.clone(),
        CVal::Object(..) | CVal::Resource => ArgValue::Other,
        CVal::Array(entries) => ArgValue::Array(
            entries
                .iter()
                .map(|(k, cv)| {
                    let key = match k {
                        NormKey::Int(i) => ArrayKey::Int(*i),
                        NormKey::Str(s) => ArrayKey::Str(s.clone()),
                    };
                    (key, cval_to_argvalue(cv))
                })
                .collect(),
        ),
    }
}

#[cfg(test)]
mod domain_tests {
    //! Unit tests for the ADR-0031/0035 domain skeleton: the unified [`Certainty`]
    //! algebra, [`Fact`] joins (agree / OneOf / cap overflow), and the empirically
    //! settled PHP comparison primitives.
    use super::*;
    use steins_domain::PhpStr;
    use crate::compare::{php_identical, php_loose_eq, php_truthy};
    use crate::env::singleton_fact;
    use steins_syntax::ArgValue;

    fn sing(v: ArgValue) -> Fact {
        // Scalars only here — no array literal, so the minor is immaterial.
        singleton_fact(&v, None).expect("literal converts")
    }

    #[test]
    fn certainty_algebra() {
        use Certainty::{Maybe, No, Yes};
        // not swaps the poles, fixes Maybe.
        assert_eq!(Yes.not(), No);
        assert_eq!(No.not(), Yes);
        assert_eq!(Maybe.not(), Maybe);
        // and: No dominates, then Maybe.
        assert_eq!(Yes.and(Yes), Yes);
        assert_eq!(Yes.and(No), No);
        assert_eq!(Yes.and(Maybe), Maybe);
        assert_eq!(No.and(Maybe), No);
        // or: Yes dominates, then Maybe.
        assert_eq!(No.or(No), No);
        assert_eq!(No.or(Yes), Yes);
        assert_eq!(No.or(Maybe), Maybe);
        assert_eq!(Yes.or(Maybe), Yes);
    }

    #[test]
    fn fact_join_agree_keeps_singleton() {
        // The env now stores `steins_domain::Fact`; joins go through the domain
        // algebra. Equal singletons stay a Singleton and resolve to the value.
        let j = sing(ArgValue::Int(5)).join(&sing(ArgValue::Int(5))).unwrap();
        assert!(matches!(j, Fact::Singleton(Val::Int(5))));
        let k = Known::value(j, 0, None);
        assert_eq!(k.singleton(), Some(ArgValue::Int(5)));
    }

    #[test]
    fn fact_join_differ_forms_oneof_and_dedups() {
        let j = sing(ArgValue::Int(5)).join(&sing(ArgValue::Int(6))).unwrap();
        assert!(matches!(&j, Fact::OneOf(vs) if vs.len() == 2));
        // A OneOf never resolves to a single proven value.
        assert_eq!(Known::value(j.clone(), 0, None).singleton(), None);
        // Re-joining an already-present value dedups.
        let j2 = j.join(&sing(ArgValue::Int(6))).unwrap();
        assert!(matches!(&j2, Fact::OneOf(vs) if vs.len() == 2));
    }

    #[test]
    fn fact_join_overflow_widens_to_refined() {
        // Beyond the OneOf cap the domain widens to a *computed* Refined summary
        // (an int interval), rather than dropping — abstract facts now flow
        // through the env (ADR-0035 stage 2). The widened fact resolves no value.
        let full = Fact::from_vals((0..steins_domain::CAP as i64).map(Val::Int).collect()).unwrap();
        assert!(matches!(full, Fact::OneOf(_)));
        let widened = full.join(&sing(ArgValue::Int(999))).unwrap();
        assert!(matches!(widened, Fact::Refined { base: Base::Int, .. }));
        assert_eq!(Known::value(widened, 0, None).singleton(), None);
    }

    #[test]
    fn loose_eq_measured_cells_php_8_5_8() {
        use ArgValue::{Bool, Int, Null, Str};
        let s = |x: &str| Str(x.into());
        // A representative slice of the recorded PHP 8.5.8 table.
        assert_eq!(php_loose_eq(&Null, &Null, Some((8, 5))), Some(true));
        assert_eq!(php_loose_eq(&Null, &Int(0), Some((8, 5))), Some(true));
        assert_eq!(php_loose_eq(&Null, &s(""), Some((8, 5))), Some(true));
        assert_eq!(php_loose_eq(&Null, &s("0"), Some((8, 5))), Some(false)); // the PHP 8 trap
        assert_eq!(php_loose_eq(&Null, &Bool(false), Some((8, 5))), Some(true));
        assert_eq!(php_loose_eq(&Bool(false), &s("0"), Some((8, 5))), Some(true));
        assert_eq!(php_loose_eq(&Bool(false), &s("abc"), Some((8, 5))), Some(false));
        assert_eq!(php_loose_eq(&Bool(true), &s("abc"), Some((8, 5))), Some(true));
        assert_eq!(php_loose_eq(&Int(0), &s("abc"), Some((8, 5))), Some(false)); // PHP 8, not PHP 7
        assert_eq!(php_loose_eq(&Int(0), &s("0"), Some((8, 5))), Some(true));
        assert_eq!(php_loose_eq(&Int(0), &s(""), Some((8, 5))), Some(false));
        assert_eq!(php_loose_eq(&s("0"), &s(""), Some((8, 5))), Some(false));
        assert_eq!(php_loose_eq(&s("5"), &s("5"), Some((8, 5))), Some(true));
        assert_eq!(php_loose_eq(&s("5"), &Int(5), Some((8, 5))), Some(true));
    }

    #[test]
    fn truthiness_edge_cells() {
        use ArgValue::{Array, Float, Int, Null, Str};
        assert_eq!(php_truthy(&Str("0".into())), Some(false)); // "0" is falsy
        assert_eq!(php_truthy(&Str("0.0".into())), Some(true)); // but "0.0" is truthy
        assert_eq!(php_truthy(&Str(PhpStr::new())), Some(false));
        assert_eq!(php_truthy(&Int(0)), Some(false));
        assert_eq!(php_truthy(&Float(0.0)), Some(false));
        assert_eq!(php_truthy(&Null), Some(false));
        assert_eq!(php_truthy(&Array(vec![])), Some(false)); // [] is falsy
    }

    #[test]
    fn identical_is_type_strict() {
        use ArgValue::{Float, Int};
        assert_eq!(php_identical(&Int(5), &Int(5), Some((8, 5))), Some(true));
        assert_eq!(php_identical(&Int(5), &Float(5.0), Some((8, 5))), Some(false)); // 5 === 5.0 is false
    }

    /// ADR-0049 A12: the next-auto-index rule for negative keys changed in PHP
    /// 8.3, so an array `===` verdict is a function of the *project's* minor —
    /// and is unproven when no minor was reported.
    #[test]
    fn negative_key_arrays_compare_per_the_project_minor() {
        use steins_syntax::ArrayKey;
        let s = |x: &str| ArgValue::Str(x.into());
        let arr = |items: Vec<(ArrayKey, ArgValue)>| ArgValue::Array(items);

        // `[-5 => 'a', 'b']` — the omitted key is where the two rules disagree.
        let auto = arr(vec![(ArrayKey::Int(-5), s("a")), (ArrayKey::Auto, s("b"))]);
        // `[-5 => 'a', -4 => 'b']` (the 8.3+ landing) and `[-5 => 'a', 0 => 'b']`
        // (the pre-8.3 landing), both written with explicit keys.
        let at_minus_4 = arr(vec![(ArrayKey::Int(-5), s("a")), (ArrayKey::Int(-4), s("b"))]);
        let at_zero = arr(vec![(ArrayKey::Int(-5), s("a")), (ArrayKey::Int(0), s("b"))]);

        // Witnessed on PHP 8.5.8:
        //   php -r 'var_export([-5=>"a","b"] === [-5=>"a",-4=>"b"]);' → true
        //   php -r 'var_export([-5=>"a","b"] === [-5=>"a",0=>"b"]);'  → false
        assert_eq!(php_identical(&auto, &at_minus_4, Some((8, 5))), Some(true));
        assert_eq!(php_identical(&auto, &at_zero, Some((8, 5))), Some(false));

        // A project on 8.1/8.2 floors the auto index at 0 — the verdicts invert.
        for minor in [(8, 1), (8, 2)] {
            assert_eq!(php_identical(&auto, &at_minus_4, Some(minor)), Some(false), "{minor:?}");
            assert_eq!(php_identical(&auto, &at_zero, Some(minor)), Some(true), "{minor:?}");
        }

        // No reported minor: unproven, not guessed. This is the leg that keeps a
        // wrong key out of the proof layer.
        assert_eq!(php_identical(&auto, &at_minus_4, None), None);
        assert_eq!(php_identical(&auto, &at_zero, None), None);
        assert_eq!(php_loose_eq(&auto, &at_minus_4, None), None);

        // A version-independent literal still decides under an unknown minor —
        // the widening stays narrow.
        let list = arr(vec![(ArrayKey::Auto, s("a"))]);
        let list_explicit = arr(vec![(ArrayKey::Int(0), s("a"))]);
        assert_eq!(php_identical(&list, &list_explicit, None), Some(true));
    }

    /// The same premise on the fact side: an unresolvable key drops the
    /// `Val::Array` singleton rather than recording a guessed one.
    #[test]
    fn unproven_negative_key_drops_the_singleton_fact() {
        use steins_syntax::ArrayKey;
        let arr = ArgValue::Array(vec![
            (ArrayKey::Int(-5), ArgValue::Str("a".into())),
            (ArrayKey::Auto, ArgValue::Str("b".into())),
        ]);
        assert!(singleton_fact(&arr, None).is_none());
        assert!(singleton_fact(&arr, Some((8, 5))).is_some());
        assert!(singleton_fact(&arr, Some((8, 1))).is_some());
    }
}

#[cfg(test)]
mod oracle_tests {
    //! Unit tests for the ADR-0043 §3 trinary is-a oracle ([`Cx::is_a`]): the
    //! parent chain, the transitive `implements` closure, interface-extends, the
    //! builtin exception tree, the enum interface roots, the closed-set `No`, and
    //! every `Unknown` condition. The oracle is exercised directly against a
    //! one-file project so its verdicts are asserted without routing through
    //! instanceof branch analysis (integration tests cover that path separately).
    use super::*;
    use crate::contract::IsA;

    fn is_a(src: &str, sub: &str, sup: &str) -> IsA {
        let tree = SourceTree::parse(src);
        let units = [FileUnit { path: "t.php", tree: &tree }];
        let index = Index::from_units(&units);
        Cx::new(&units, &index, 0).is_a(sub, sup)
    }

    #[test]
    fn reflexive_and_parent_chain() {
        let src = "<?php class A {} class B extends A {} class C extends B {}";
        assert_eq!(is_a(src, "c", "c"), IsA::Yes, "reflexive");
        assert_eq!(is_a(src, "c", "a"), IsA::Yes, "grandparent via chain");
        assert_eq!(is_a(src, "b", "a"), IsA::Yes);
        // Fully enumerated, unrelated direction → No.
        assert_eq!(is_a(src, "a", "c"), IsA::No, "a is not a c (closed set)");
    }

    #[test]
    fn transitive_implements_and_interface_extends() {
        let src = "<?php
interface I {}
interface J extends I {}
class Base implements J {}
class Foo extends Base {}";
        assert_eq!(is_a(src, "foo", "j"), IsA::Yes, "class implements via parent");
        assert_eq!(is_a(src, "foo", "i"), IsA::Yes, "transitive interface-extends");
        assert_eq!(is_a(src, "base", "i"), IsA::Yes);
        assert_eq!(is_a(src, "j", "i"), IsA::Yes, "interface extends interface");
        // A class with no relation to K, fully enumerated → No.
        let src2 = "<?php interface I {} interface K {} class Foo implements I {}";
        assert_eq!(is_a(src2, "foo", "k"), IsA::No);
    }

    #[test]
    fn builtin_exception_tree_closed() {
        let src = "<?php class MyEx extends \\RuntimeException {}";
        // Chain leaves the project into the catalogued exception tree — enumerated.
        assert_eq!(is_a(src, "myex", "runtimeexception"), IsA::Yes);
        assert_eq!(is_a(src, "myex", "exception"), IsA::Yes);
        assert_eq!(is_a(src, "myex", "throwable"), IsA::Yes);
        // A catalogued exception is provably NOT a LogicException (both under the
        // fully-known SPL tree).
        assert_eq!(is_a(src, "myex", "logicexception"), IsA::No);
        // PHP 8.0+: `Throwable extends Stringable`, so every Throwable IS-A
        // Stringable (verified against PHP 8.5). A `No` here would be unsound.
        assert_eq!(is_a(src, "myex", "stringable"), IsA::Yes);
    }

    #[test]
    fn enum_is_a_its_interfaces_and_roots() {
        let src = "<?php
interface HasLabel {}
enum Suit: string implements HasLabel { case H = 'h'; }
enum Dir { case Up; }";
        // A backed enum is-a UnitEnum, BackedEnum, and its explicit interface.
        assert_eq!(is_a(src, "suit", "unitenum"), IsA::Yes);
        assert_eq!(is_a(src, "suit", "backedenum"), IsA::Yes);
        assert_eq!(is_a(src, "suit", "haslabel"), IsA::Yes);
        // A pure enum is-a UnitEnum but NOT BackedEnum (closed enumeration).
        assert_eq!(is_a(src, "dir", "unitenum"), IsA::Yes);
        assert_eq!(is_a(src, "dir", "backedenum"), IsA::No);
        assert_eq!(is_a(src, "dir", "haslabel"), IsA::No);
    }

    #[test]
    fn unknown_when_chain_leaves_project() {
        // Parent is an uncatalogued external → enumeration incomplete → Unknown.
        let src = "<?php class Foo extends \\Vendor\\Base {}";
        assert_eq!(is_a(src, "foo", "vendor\\base"), IsA::Yes, "the named parent is still Yes");
        assert_eq!(is_a(src, "foo", "somethingelse"), IsA::Unknown, "beyond the unknown parent");
    }

    #[test]
    fn unknown_when_sub_or_super_unknown() {
        let src = "<?php class A {}";
        // Sub is an unknown external → Unknown (unless reflexively equal).
        assert_eq!(is_a(src, "ghost", "a"), IsA::Unknown);
        assert_eq!(is_a(src, "ghost", "ghost"), IsA::Yes, "reflexive even when unknown");
        // Sub known+enumerated, super an unknown name absent from the closed set → No.
        assert_eq!(is_a(src, "a", "ghost"), IsA::No);
    }

    #[test]
    fn ambiguous_sub_is_unknown() {
        // Two definitions of the same FQN → ambiguous → not Unique → Unknown.
        let src = "<?php class Dup {} class Dup {}";
        assert_eq!(is_a(src, "dup", "whatever"), IsA::Unknown);
    }

    #[test]
    fn trait_use_does_not_force_unknown() {
        // A `use`d trait adds no type; the class is still fully enumerated (its real
        // parent/interfaces), so a `No` verdict stands.
        let src = "<?php trait T {} class A {} class Foo extends A { use T; }";
        assert_eq!(is_a(src, "foo", "a"), IsA::Yes);
        assert_eq!(is_a(src, "foo", "unrelated"), IsA::No, "trait use keeps closure complete");
    }
}

#[cfg(test)]
mod template_type_rewrite_tests {
    //! Unit tests for [`Cx::resolve_template_types`] (issue #361) at the level the
    //! rewrite actually operates on: the phpdoc AST, before any lowering.
    //!
    //! The integration tests pin what the resolved envelopes *judge*; these pin
    //! what the node *becomes*, which is a distinct claim in two places the
    //! judgement cannot see. A declined node must be `Unsupported` and still read
    //! back as what was written; a node whose subject is a template name must be
    //! left byte-identical, because the carry readers (#362/#363) intercept exactly
    //! that spelling and a rewrite would erase it.
    use super::*;
    use steins_phpdoc::parse_type;

    /// `passes` applications of the rewrite to `spelling`, read in file `file` at
    /// offset `off` of a project made of `srcs`.
    fn rewritten_n(srcs: &[&str], file: usize, off: u32, spelling: &str, passes: usize) -> PType {
        let trees: Vec<SourceTree> = srcs.iter().map(|s| SourceTree::parse(s)).collect();
        let paths: Vec<String> = (0..srcs.len()).map(|i| format!("t{i}.php")).collect();
        let units: Vec<FileUnit> = trees
            .iter()
            .zip(&paths)
            .map(|(tree, path)| FileUnit { path: path.as_str(), tree })
            .collect();
        let index = Index::from_units(&units);
        let cx = Cx::new(&units, &index, file);
        let mut ty = parse_type(spelling).expect("the spelling parses").ty;
        for _ in 0..passes {
            cx.resolve_template_types(&mut ty, file, off);
        }
        ty
    }

    /// The rewrite of `spelling`, read in file `file` at offset `off`.
    fn rewritten_in(srcs: &[&str], file: usize, off: u32, spelling: &str) -> PType {
        rewritten_n(srcs, file, off, spelling, 1)
    }

    /// The rewrite of `spelling` against a single global-namespace file.
    fn rewritten(src: &str, spelling: &str) -> PType {
        rewritten_in(&[src], 0, 0, spelling)
    }

    /// The `@return` envelope of the **last** function declared in `src`, built the
    /// way every consumer builds one — [`Cx::envelopes_of`], so the declaration's
    /// own `@template` shadow has run before the rewrite, exactly as in production.
    fn envelope_return(src: &str) -> PType {
        let tree = SourceTree::parse(src);
        let units = [FileUnit { path: "t.php", tree: &tree }];
        let index = Index::from_units(&units);
        let cx = Cx::new(&units, &index, 0);
        let f = tree.functions().last().expect("a function is declared");
        cx.envelopes_of(f.docblock.as_deref(), 0, f.span.start)
            .expect("the docblock carries envelopes")
            .ret
            .expect("the docblock carries a @return")
    }

    const BOX: &str = "<?php\n/** @template T */\nclass Box {}\n";

    #[test]
    fn a_spelled_parameterization_becomes_the_argument_itself() {
        let ty = rewritten(BOX, "template-type<Box<int>, Box, 'T'>");
        assert!(matches!(&ty.kind, PKind::Identifier(n) if n == "int"), "{ty}");
        // Inside-out: the outer `list` survives, the inner node resolves.
        let nested = rewritten(BOX, "list<template-type<Box<int>, Box, 'T'>>");
        assert_eq!(nested.to_string(), "list<int>");
    }

    #[test]
    fn a_template_subject_is_left_exactly_as_written() {
        // The orchestrating rule for the follow-ups: a subject that names a
        // template is not this slice's to decide, and it must survive the rewrite
        // as the node the carry readers will match on. `Opaque` either way today
        // (issue #360), so nothing observable changes — which is the point.
        // Both spellings that reach the rewrite as a bare identifier: a template
        // name, and any other name no class answers to.
        for spelling in ["template-type<T, Box, 'T'>", "template-type<Unresolvable, Box, 'T'>"] {
            let ty = rewritten(BOX, spelling);
            let PKind::Generic { base, args } = &ty.kind else {
                panic!("{spelling} was rewritten to {ty}");
            };
            assert_eq!(base, "template-type");
            assert_eq!(args.len(), 3);
            assert_eq!(steins_contract::lower(&ty), steins_contract::ContractTy::Opaque);
        }
    }

    #[test]
    fn the_shadowed_spelling_of_a_template_subject_survives_too() {
        // The path production actually takes. `parse_envelopes` applies the
        // declaration's own `@template` shadow *before* the rewrite, so a
        // function-level `T` subject is no longer an identifier by the time the
        // projection sees it — it is an `Unsupported` node, and the previous test's
        // identifier arm never covers it. Declining here would rewrite the node and
        // erase the spelling #363 intercepts.
        let src = "<?php\n/** @template T */\nclass Box {}\n\
                   /**\n * @template T\n * @param Box<T> $b\n\
                   \x20* @return template-type<T, Box, 'T'>\n */\n\
                   function f(Box $b) {}\n";
        let ret = envelope_return(src);
        let PKind::Generic { base, args } = &ret.kind else { panic!("rewritten to {ret}") };
        assert_eq!(base, "template-type");
        assert_eq!(args.len(), 3);
        assert!(
            matches!(&args[0].ty.kind, PKind::Unsupported(_)),
            "the shadow left {}, not an opaque node",
            args[0].ty,
        );
        assert_eq!(steins_contract::lower(&ret), steins_contract::ContractTy::Opaque);

        // Its sibling through case (a): the owner parameterized by the same
        // shadowed template resolves, and what it resolves *to* is that node — so
        // the envelope reads as `@return T` reads, which is the acceptance
        // criterion. One spelling defers, the other projects; neither invents.
        let spelled = src.replace("template-type<T, Box, 'T'>", "template-type<Box<T>, Box, 'T'>");
        let resolved = envelope_return(&spelled);
        assert!(matches!(&resolved.kind, PKind::Unsupported(_)), "{resolved}");
        assert_eq!(steins_contract::lower(&resolved), steins_contract::ContractTy::Opaque);
    }

    #[test]
    fn a_decline_becomes_an_opaque_node_that_still_says_what_it_was() {
        // `Box` is the owner itself, unparameterized — nothing to project.
        let ty = rewritten(BOX, "template-type<Box, Box, 'T'>");
        assert!(
            matches!(&ty.kind, PKind::Unsupported(raw) if raw == "template-type<Box, Box, 'T'>"),
            "{ty}",
        );
        assert_eq!(steins_contract::lower(&ty), steins_contract::ContractTy::Opaque);
    }

    #[test]
    fn the_rewrite_is_idempotent() {
        // Load-bearing: the member sites apply a second shadow stage after
        // `envelopes_of` has run, and every stage over an envelope is written to be
        // safe to re-apply.
        for spelling in [
            "template-type<Box<int>, Box, 'T'>",
            "template-type<Box, Box, 'T'>",
            "template-type<T, Box, 'T'>",
        ] {
            assert_eq!(
                rewritten_n(&[BOX], 0, 0, spelling, 1),
                rewritten_n(&[BOX], 0, 0, spelling, 2),
                "{spelling}",
            );
        }
    }

    #[test]
    fn an_edge_argument_keeps_naming_the_class_it_named_where_it_was_written() {
        // The projection lifts `@extends Box<Dog>` out of `App`'s file into a
        // declaration written in `Other`, where a bare `Dog` would name a
        // different class. It arrives fully qualified instead.
        let app = "<?php\nnamespace App;\n/** @template T */\nclass Box {}\nclass Dog {}\n\
                   /** @extends Box<Dog> */\nfinal class DogBox extends Box {}\n";
        let other = "<?php\nnamespace Other;\nclass Dog {}\n";
        let off = other.len() as u32;
        let ty = rewritten_in(
            &[app, other],
            1,
            off,
            "template-type<\\App\\DogBox, \\App\\Box, 'T'>",
        );
        assert_eq!(ty.to_string(), "\\App\\Dog");
    }
}

#[cfg(test)]
mod phpdoc_walk_tests {
    //! Unit tests for the one traversal every phpdoc-type walk in this crate goes
    //! through ([`for_each_child_type`] and its mutable twin, issue #374), pinned
    //! through the walk whose reach is observable node by node: the `@template`
    //! shadow ([`neutralize_templates`]).
    //!
    //! One test per node kind, and every one reads the position out of the AST **by
    //! hand**. A test that recursed the way the subject recurses would agree with it
    //! about a position neither reaches, and so would pass vacuously on exactly the
    //! drift it exists to catch — which is how `\Closure(): T` stayed unshadowed
    //! through four issues.
    use super::*;
    use crate::contract::{neutralize_templates, template_names_of};
    use crate::return_arms::mentioned_templates;
    use steins_phpdoc::ast::ConditionalSubject;
    use steins_phpdoc::parse_type;
    use crate::contract::parse_envelopes;
    use crate::contract::type_has_unsupported;

    /// `spelling` after the shadow of a lone `@template T` has run over it.
    fn shadowed(spelling: &str) -> PType {
        let shadow = template_names_of(Some("/** @template T */"));
        let mut ty = parse_type(spelling).expect("the spelling parses").ty;
        neutralize_templates(&mut ty, &shadow);
        ty
    }

    /// Whether the shadow reached this position: the template name has become the
    /// opaque node that lowers to `Opaque` and judges nothing, keeping its spelling.
    fn neutral(ty: &PType) -> bool {
        matches!(&ty.kind, PKind::Unsupported(raw) if raw == "T")
    }

    #[test]
    fn a_callables_parameters_and_return_are_shadowed() {
        // The leak this walk was unified to close: `T` here lowered as a class
        // named `T`, so a project declaring one judged closure arguments against it.
        let ty = shadowed("callable(T, int): T");
        let PKind::Callable(c) = &ty.kind else { panic!("parsed as {ty}") };
        assert!(neutral(&c.params[0].ty), "parameter: {}", c.params[0].ty);
        assert!(neutral(&c.return_type), "return: {}", c.return_type);
        // What the contract lane makes of it: the signature survives, and the two
        // shadowed positions lower to the silent arm rather than to a class named
        // `T`. The untouched `int` parameter is the control.
        let lowered = steins_contract::lower(&ty);
        let steins_contract::ContractTy::CallableTy { sig: Some(sig), .. } = lowered else {
            panic!("lowered to {lowered:?}");
        };
        assert_eq!(sig.params[0].ty, steins_contract::ContractTy::Opaque);
        assert_eq!(sig.ret, steins_contract::ContractTy::Opaque);
        assert_ne!(sig.params[1].ty, steins_contract::ContractTy::Opaque, "int is untouched");
    }

    #[test]
    fn a_conditionals_subject_target_and_branches_are_shadowed() {
        let ty = shadowed("(T is T ? T : T)");
        let PKind::Conditional(c) = &ty.kind else { panic!("parsed as {ty}") };
        let ConditionalSubject::Type(subject) = &c.subject else { panic!("parsed as {ty}") };
        assert!(neutral(subject), "subject: {subject}");
        assert!(neutral(&c.target), "target: {}", c.target);
        assert!(neutral(&c.if_type), "if branch: {}", c.if_type);
        assert!(neutral(&c.else_type), "else branch: {}", c.else_type);
    }

    #[test]
    fn an_offset_accesss_base_and_offset_are_shadowed() {
        let ty = shadowed("T[T]");
        let PKind::OffsetAccess { base, offset } = &ty.kind else { panic!("parsed as {ty}") };
        assert!(neutral(base), "base: {base}");
        assert!(neutral(offset), "offset: {offset}");
    }

    #[test]
    fn a_shape_value_and_its_unsealed_tail_are_shadowed() {
        let ty = shadowed("array{a: T, ...<T, T>}");
        let PKind::ArrayShape(s) = &ty.kind else { panic!("parsed as {ty}") };
        assert!(neutral(&s.items[0].value), "value: {}", s.items[0].value);
        let tail = s.unsealed.as_ref().expect("the tail parsed");
        assert!(neutral(&tail.value), "tail value: {}", tail.value);
        assert!(neutral(tail.key.as_ref().expect("the tail key parsed")), "tail key");
        // The object-shape twin of the value position.
        let obj = shadowed("object{a: T}");
        let PKind::ObjectShape(items) = &obj.kind else { panic!("parsed as {obj}") };
        assert!(neutral(&items[0].value), "object value: {}", items[0].value);
    }

    #[test]
    fn the_positions_that_were_never_in_doubt_still_are_shadowed() {
        // The composites the hand-rolled walk already covered, kept under the pin so
        // the unification is measurably behaviour-preserving where it should be.
        let ty = shadowed("?list<T>");
        let PKind::Nullable(inner) = &ty.kind else { panic!("parsed as {ty}") };
        let PKind::Generic { args, .. } = &inner.kind else { panic!("parsed as {ty}") };
        assert!(neutral(&args[0].ty), "generic argument: {}", args[0].ty);
        let arr = shadowed("(T|int)[]");
        let PKind::Array(elem) = &arr.kind else { panic!("parsed as {arr}") };
        let PKind::Union { types, .. } = &elem.kind else { panic!("parsed as {arr}") };
        assert!(neutral(&types[0]), "union member: {}", types[0]);
    }

    #[test]
    fn the_names_a_node_carries_are_each_walks_own_business() {
        // A `\`-qualified reference opts out of the template namespace (issue #5's
        // own rule), and a generic *base* is a string no rewrite touches — the
        // mention scan is what reads it, and it still does.
        let ty = shadowed("list<\\T>");
        let PKind::Generic { args, .. } = &ty.kind else { panic!("parsed as {ty}") };
        assert!(matches!(&args[0].ty.kind, PKind::Identifier(n) if n == "\\T"), "{ty}");
        let base = shadowed("T<int>");
        assert!(matches!(&base.kind, PKind::Generic { base, .. } if base == "T"), "{base}");
        let shadow = template_names_of(Some("/** @template T */"));
        let mut names = Vec::new();
        mentioned_templates(&base, &shadow, &mut names);
        assert_eq!(names, vec!["T".to_owned()], "the base is a mention the read cannot index");
    }

    #[test]
    fn the_shadow_runs_after_the_opaque_test_and_the_envelope_survives() {
        // The order inside `parse_envelopes` is load-bearing now that the shadow
        // reaches inside a signature. `parse_tag_type` refuses a type carrying an
        // opaque node; the shadow plants one. Were the two the other way round,
        // every `\Closure(): T` envelope would vanish instead of merely going quiet
        // about the class named `T`.
        let env = parse_envelopes(Some("/** @template T\n * @param \\Closure(): T $f */"))
            .expect("the docblock carries an envelope");
        let ty = env.param("f").expect("the @param survived the shadow");
        let PKind::Callable(c) = &ty.kind else { panic!("parsed as {ty}") };
        assert!(neutral(&c.return_type), "return: {}", c.return_type);
        assert!(type_has_unsupported(ty), "the opaque test reaches inside a signature too");
    }
}

#[cfg(test)]
mod n4_carrier_tests {
    //! ADR-0052 N4 — contract facts, class facts, and instanceof subtraction at the
    //! carrier level (the walk-integration path is covered by the `narrowing_n4`
    //! integration test). Each adversarial drift direction of the slice prompt has a
    //! test: argument-order (`is_a(M,T)`), positive-branch non-final survival,
    //! Unknown-keeps-both, emptied-lane-is-no-fact, Asserted-never-launders, and the
    //! A11 catalog-skew demotion scoped to arm deletion.
    use super::*;
    use crate::fold_args::parse_php_minor;
    use crate::contract::ProjectIsa;
    use steins_contract::{ContractTy, normalize};
    use steins_syntax::ScopeOwner;
    use crate::cond::member_instanceof;
    use crate::cx::EMPTY_DAM;
    use crate::env::{ContractArm, Member, Stratum, dedup_contract_arms, join_stores};
    use crate::refine::{seed_contract_arms, subtract_contract_lane};

    /// Build a `Cx` over a one-file project and run `f` against it. `php_minor` seeds
    /// the A11 version input; the skew flag is derived from it exactly as
    /// [`effective_php_view`] does with no declared target.
    fn with_cx<R>(src: &str, php_minor: Option<(u16, u16)>, f: impl FnOnce(&Cx) -> R) -> R {
        let tree = SourceTree::parse(src);
        let units = [FileUnit { path: "t.php", tree: &tree }];
        let index = Index::from_units(&units);
        let view = effective_php_view(php_minor, None);
        let cx = Cx::new_with(
            &units,
            &index,
            0,
            &EMPTY_DAM,
            true,
            FinalKeyword::Enforced,
            view.effective_minor,
            view.catalog_skew,
            view.version_id,
            None,
            None,
        );
        f(&cx)
    }

    fn cls(s: &str) -> ContractTy {
        ContractTy::Class(s.to_owned())
    }
    fn arm(ty: ContractTy, stratum: Stratum) -> ContractArm {
        ContractArm { ty, stratum }
    }
    fn oracle<'c, 'a>(cx: &'c Cx<'a>) -> ProjectIsa<'c, 'a> {
        ProjectIsa { cx, demote_catalog: cx.a11_demote_catalog() }
    }

    /// The identity class resolver for the global-namespace seeding tests: the
    /// lowered phpdoc names are already the normalized FQNs there.
    fn id_resolve(n: &str) -> String {
        n.to_ascii_lowercase()
    }

    // ---- native_arms / flatten_arms / seeding -------------------------------

    #[test]
    fn native_arms_lowers_scalars_instances_and_null() {
        let src = "<?php function f(?int $a, User|Guest $b): void {}";
        with_cx(src, None, |cx| {
            let scope = cx.tree().scopes().iter().find(|s| matches!(&s.owner, ScopeOwner::Function(n) if n == "f")).unwrap();
            let params = cx.scope_params(scope).unwrap();
            // `?int` → [int, null] Verified.
            assert_eq!(
                seed_contract_arms(&params[0], None, &id_resolve),
                Some(vec![arm(ContractTy::Base(Base::Int), Stratum::Verified), arm(ContractTy::Null, Stratum::Verified)])
            );
            // `User|Guest` native (object instances) → [User, Guest] Verified.
            assert_eq!(
                seed_contract_arms(&params[1], None, &id_resolve),
                Some(vec![arm(cls("user"), Stratum::Verified), arm(cls("guest"), Stratum::Verified)])
            );
        });
    }

    #[test]
    fn seed_phpdoc_refines_at_asserted_stratum() {
        // `object $value` (native None) + `@param User|Guest` → phpdoc arms, Asserted.
        let src = "<?php /** @param User|Guest $value */ function f(object $value): void {}";
        with_cx(src, None, |cx| {
            let scope = cx.tree().scopes().iter().find(|s| matches!(&s.owner, ScopeOwner::Function(n) if n == "f")).unwrap();
            let p = &cx.scope_params(scope).unwrap()[0];
            let env = cx.scope_envelopes(scope).unwrap();
            let seeded = seed_contract_arms(p, env.param("value"), &id_resolve).unwrap();
            assert_eq!(
                seeded,
                vec![arm(cls("user"), Stratum::Asserted), arm(cls("guest"), Stratum::Asserted)]
            );
        });
    }

    #[test]
    fn seed_phpdoc_arm_backed_by_native_stays_verified() {
        // `int $x` + `@param int $x`: the `int` arm the native ALSO proves keeps the
        // Verified stratum (no needless downgrade); a phpdoc-only refinement would be
        // Asserted.
        let src = "<?php /** @param int $x */ function f(int $x): void {}";
        with_cx(src, None, |cx| {
            let scope = cx.tree().scopes().iter().find(|s| matches!(&s.owner, ScopeOwner::Function(n) if n == "f")).unwrap();
            let p = &cx.scope_params(scope).unwrap()[0];
            let env = cx.scope_envelopes(scope).unwrap();
            assert_eq!(
                seed_contract_arms(p, env.param("x"), &id_resolve),
                Some(vec![arm(ContractTy::Base(Base::Int), Stratum::Verified)])
            );
        });
    }

    #[test]
    fn dedup_contract_arms_ties_keep_min_stratum() {
        // Two arm_eq arms (a Verified `int` and an Asserted `int`, as a join would
        // produce): the survivor keeps the WEAKER (Asserted) stratum — no laundering.
        let mut arms = vec![
            arm(ContractTy::Base(Base::Int), Stratum::Verified),
            arm(ContractTy::Base(Base::Int), Stratum::Asserted),
        ];
        dedup_contract_arms(&mut arms);
        assert_eq!(arms, vec![arm(ContractTy::Base(Base::Int), Stratum::Asserted)]);
    }

    #[test]
    fn dedup_collapses_identical_opaque_arms() {
        // Survey non-termination regression (nextcloud `core/Migrations`): the
        // non-extensional arms (`CallableTy`/`StrOpaque`/`Opaque`) have
        // `subsumes(x, x) == Maybe`, so `arm_eq` alone could NOT collapse two
        // identical copies — a branch-union then doubled the pile at every join,
        // reaching 2^depth. Structural equality must collapse them.
        // (`Mixed`/`ObjectAny` are arm_eq-reflexive already, unaffected.)
        //
        // The survey's OTHER exploding arm, `array $options`, is deliberately no
        // longer in this list: ADR-0071 made every array arm arm_eq-reflexive
        // (pinned in steins-contract's `array_arms_are_arm_eq_reflexive`), so the
        // structural-equality collapse still catches it first.
        for ty in [
            ContractTy::CallableTy { sig: None, obl: steins_contract::CallableObl::default() },
            ContractTy::StrOpaque,
            ContractTy::Opaque,
        ] {
            assert!(!normalize::arm_eq(&ty, &ty), "{ty:?} is expectedly non-arm_eq-reflexive");
            let mut arms: Vec<ContractArm> =
                (0..64).map(|_| arm(ty.clone(), Stratum::Verified)).collect();
            dedup_contract_arms(&mut arms);
            assert_eq!(arms, vec![arm(ty.clone(), Stratum::Verified)], "{ty:?} pile must collapse to one");
        }
        // The array arm collapses too, now for the stronger reason (ADR-0071).
        let array = ContractTy::ArrayAny { non_empty: false };
        assert!(normalize::arm_eq(&array, &array), "array arms are arm_eq-reflexive since ADR-0071");
        let mut arms: Vec<ContractArm> =
            (0..64).map(|_| arm(array.clone(), Stratum::Verified)).collect();
        dedup_contract_arms(&mut arms);
        assert_eq!(arms, vec![arm(array, Stratum::Verified)], "array pile must collapse to one");
    }

    #[test]
    fn dedup_identical_opaque_keeps_min_stratum() {
        // The structural-equality collapse still honors the derivation clause: a
        // Verified + Asserted pair of the SAME opaque arm survives at Asserted.
        let mut arms = vec![
            arm(ContractTy::CallableTy { sig: None, obl: steins_contract::CallableObl::default() }, Stratum::Verified),
            arm(ContractTy::CallableTy { sig: None, obl: steins_contract::CallableObl::default() }, Stratum::Asserted),
            arm(ContractTy::CallableTy { sig: None, obl: steins_contract::CallableObl::default() }, Stratum::Verified),
        ];
        dedup_contract_arms(&mut arms);
        assert_eq!(arms, vec![arm(ContractTy::CallableTy { sig: None, obl: steins_contract::CallableObl::default() }, Stratum::Asserted)]);
    }

    // ---- the deliverable: else-of-instanceof leaves {Guest} -----------------

    const FIXTURE: &str = "<?php interface Named { public function name(): string; } \
        final class User implements Named { public function name(): string { return 'u'; } } \
        final class Guest { public function guestId(): int { return 1; } }";

    #[test]
    fn negative_branch_leaves_guest_arm_asserted() {
        // The conformance deliverable, at the carrier level: a `User|Guest` lane, the
        // else of `instanceof User` subtracts User (is_a(User,User)=Yes), leaving
        // {Guest} — and Guest keeps its Asserted stratum (came from `@param`).
        with_cx(FIXTURE, None, |cx| {
            let mut store = Store::default();
            store.contract.insert(
                "value".into(),
                vec![arm(cls("user"), Stratum::Asserted), arm(cls("guest"), Stratum::Asserted)],
            );
            subtract_contract_lane(
                &mut store,
                "value",
                &normalize::Subtrahend::Class { fqn: "user".into(), polarity: false },
                &oracle(cx),
            );
            assert_eq!(store.contract_arms("value"), Some([arm(cls("guest"), Stratum::Asserted)].as_slice()));
        });
    }

    #[test]
    fn negative_branch_argument_order_is_m_then_t() {
        // `Named` is a supertype of `User`. else of `instanceof User` over a lane
        // holding `Named` asks is_a(Named, User) = No (a Named need not be a User) →
        // the arm SURVIVES. A reversed is_a(User, Named)=Yes would wrongly delete it.
        with_cx(FIXTURE, None, |cx| {
            let mut store = Store::default();
            store.contract.insert("v".into(), vec![arm(cls("named"), Stratum::Verified)]);
            subtract_contract_lane(
                &mut store,
                "v",
                &normalize::Subtrahend::Class { fqn: "user".into(), polarity: false },
                &oracle(cx),
            );
            assert_eq!(store.contract_arms("v"), Some([arm(cls("named"), Stratum::Verified)].as_slice()));
        });
    }

    #[test]
    fn positive_branch_deletes_final_nonmember_keeps_open() {
        // then of `instanceof User` over `Guest|Named`: Guest is final and
        // is_a(Guest,User)=No → deleted; Named is NOT final → survives (an unseen
        // Named subclass could be a User). Guards both positive-branch drifts.
        with_cx(FIXTURE, None, |cx| {
            let mut store = Store::default();
            store.contract.insert("v".into(), vec![arm(cls("guest"), Stratum::Verified), arm(cls("named"), Stratum::Verified)]);
            subtract_contract_lane(
                &mut store,
                "v",
                &normalize::Subtrahend::Class { fqn: "user".into(), polarity: true },
                &oracle(cx),
            );
            assert_eq!(store.contract_arms("v"), Some([arm(cls("named"), Stratum::Verified)].as_slice()));
        });
    }

    #[test]
    fn emptied_lane_drops_to_no_fact() {
        // A `!== null` on a `null`-only lane empties it → the lane is REMOVED (no
        // key), never a death signal (§2: the verdict owns death).
        with_cx(FIXTURE, None, |cx| {
            let mut store = Store::default();
            store.contract.insert("v".into(), vec![arm(ContractTy::Null, Stratum::Verified)]);
            subtract_contract_lane(&mut store, "v", &normalize::Subtrahend::Null, &oracle(cx));
            assert_eq!(store.contract_arms("v"), None, "emptied lane is no-fact, not present-and-empty");
        });
    }

    // ---- Member fact + eval_instanceof implication (§3b) --------------------

    #[test]
    fn member_implication_yes_no_maybe() {
        with_cx(FIXTURE, None, |cx| {
            // yes:[User], test `instanceof Named`: is_a(User,Named)=Yes → Yes.
            let m = Member { yes: vec!["user".into()], no: vec![] };
            assert_eq!(member_instanceof(cx, Some(&m), "named"), Certainty::Yes);
            // no:[Named], test `instanceof User`: is_a(User,Named)=Yes so a User would
            // be a Named, which the guard excluded → No.
            let m2 = Member { yes: vec![], no: vec!["named".into()] };
            assert_eq!(member_instanceof(cx, Some(&m2), "user"), Certainty::No);
            // yes:[Guest], test `instanceof Named`: is_a(Guest,Named)=No, no exclusion
            // matches → Maybe.
            let m3 = Member { yes: vec!["guest".into()], no: vec![] };
            assert_eq!(member_instanceof(cx, Some(&m3), "named"), Certainty::Maybe);
            // No fact → Maybe.
            assert_eq!(member_instanceof(cx, None, "named"), Certainty::Maybe);
        });
    }

    // ---- A11 catalog version-skew demotion ----------------------------------

    #[test]
    fn a11_catalog_backed_deletion_demoted_only_on_skew() {
        // Empty project: `ArrayObject`/`Traversable` resolve through the builtin
        // CATALOG. else of `instanceof Traversable` over an `ArrayObject` arm asks
        // is_a(ArrayObject, Traversable) = Yes (catalog-backed).
        let sub = normalize::Subtrahend::Class { fqn: "traversable".into(), polarity: false };
        // Pinned minor (matches catalog) → verdict stands → arm deleted.
        with_cx("<?php", Some(steins_catalog::PINNED_PHP), |cx| {
            let mut store = Store::default();
            store.contract.insert("v".into(), vec![arm(cls("arrayobject"), Stratum::Verified)]);
            subtract_contract_lane(&mut store, "v", &sub, &oracle(cx));
            assert_eq!(store.contract_arms("v"), None, "matching minor: catalog verdict stands, arm deleted");
        });
        // Skewed minor → catalog-backed verdict demotes to Unknown → arm KEPT.
        with_cx("<?php", Some((steins_catalog::PINNED_PHP.0, steins_catalog::PINNED_PHP.1 - 1)), |cx| {
            let mut store = Store::default();
            store.contract.insert("v".into(), vec![arm(cls("arrayobject"), Stratum::Verified)]);
            subtract_contract_lane(&mut store, "v", &sub, &oracle(cx));
            assert_eq!(
                store.contract_arms("v"),
                Some([arm(cls("arrayobject"), Stratum::Verified)].as_slice()),
                "skewed minor: catalog-backed deletion demoted, arm kept (FP-safe)"
            );
        });
    }

    #[test]
    fn a11_in_project_deletion_not_demoted_on_skew() {
        // A purely in-project `User|Guest` union narrows the SAME under a skewed minor
        // — A11 touches only catalog-backed edges, never in-project source.
        with_cx(FIXTURE, Some((steins_catalog::PINNED_PHP.0, steins_catalog::PINNED_PHP.1 - 1)), |cx| {
            assert!(cx.a11_demote_catalog(), "skew is active");
            let mut store = Store::default();
            store.contract.insert("v".into(), vec![arm(cls("user"), Stratum::Verified), arm(cls("guest"), Stratum::Verified)]);
            subtract_contract_lane(
                &mut store,
                "v",
                &normalize::Subtrahend::Class { fqn: "user".into(), polarity: false },
                &oracle(cx),
            );
            assert_eq!(
                store.contract_arms("v"),
                Some([arm(cls("guest"), Stratum::Verified)].as_slice()),
                "in-project is_a(User,User)=Yes not catalog-backed → deletion stands under skew"
            );
        });
    }

    #[test]
    fn parse_php_minor_reads_major_minor() {
        assert_eq!(parse_php_minor("8.5.8"), Some((8, 5)));
        assert_eq!(parse_php_minor("8.4.10-dev"), Some((8, 4)));
        assert_eq!(parse_php_minor("nonsense"), None);
    }

    // ---- join semantics -----------------------------------------------------

    #[test]
    fn join_unions_contract_arms_and_intersects_members() {
        // A branch with lane {User} and Member{yes:[User]} joined with a branch with
        // lane {Guest} and Member{yes:[Guest]}: the merged lane is {User,Guest} (a
        // value live on EITHER path is possible), and the Member intersection is empty
        // (no bound holds on both) → dropped.
        let mut a = Store::default();
        a.contract.insert("v".into(), vec![arm(cls("user"), Stratum::Asserted)]);
        a.members.insert("v".into(), Member { yes: vec!["user".into()], no: vec![] });
        let mut b = Store::default();
        b.contract.insert("v".into(), vec![arm(cls("guest"), Stratum::Asserted)]);
        b.members.insert("v".into(), Member { yes: vec!["guest".into()], no: vec![] });
        let j = join_stores(&a, &[&b]);
        let mut got = j.contract.get("v").cloned().unwrap();
        got.sort_by(|x, y| format!("{:?}", x.ty).cmp(&format!("{:?}", y.ty)));
        assert_eq!(got, vec![arm(cls("guest"), Stratum::Asserted), arm(cls("user"), Stratum::Asserted)]);
        assert_eq!(j.members.get("v"), None, "disjoint members intersect to empty → dropped");
    }

    #[test]
    fn unbind_forgets_narrowing_carriers() {
        // Reassignment (`store.unbind`) voids both new carriers for the var.
        let mut store = Store::default();
        store.contract.insert("v".into(), vec![arm(cls("user"), Stratum::Verified)]);
        store.members.insert("v".into(), Member { yes: vec!["user".into()], no: vec![] });
        store.unbind("v");
        assert_eq!(store.contract_arms("v"), None);
        assert_eq!(store.member_of("v"), None);
    }
}

#[cfg(test)]
mod dump_render_tests {
    //! ADR-0053 §7 — the dump fact renderer. Finite facts render **value-precisely**
    //! (the literal itself: `5`, `false`, `'abc'`); the abstract layers render the
    //! honest keyword ladder. Rendering, not the walk, is under test here; the
    //! end-to-end emitter is covered by the `dump_surface` integration test.
    //!
    //! The earlier ADR-0053 §9 pin — collapse a finite fact to its base type on the
    //! dump path — is reversed FOR THE DUMP PATH by the rendering-fidelity fix:
    //! value-precision is what the dump surface exists to show, and PHPStan itself
    //! renders constant types (`5`, `false`, `'a'`). The `annotate`/docblock
    //! renderer keeps the base-collapsed spelling unchanged (untouched byte-parity
    //! suite in `steins-edit`). §9's message *frame* is non-contractual; only the
    //! rendered fact changed.
    use super::*;
    use crate::dump::render_dump_fact;

    fn i(n: i64) -> Val {
        Val::Int(n)
    }
    fn s(v: &str) -> Val {
        Val::Str(v.into())
    }

    #[test]
    fn finite_facts_render_value_precisely() {
        let r = |vals: &[Val]| render_dump_fact(&Fact::from_vals(vals.to_vec()).expect("nonempty"));
        // Singleton int / string, OneOf int / string-enum, dedup, nullable, bool.
        assert_eq!(r(&[i(5)]), "5");
        assert_eq!(r(&[s("abc")]), "'abc'");
        assert_eq!(r(&[i(1), i(2), i(3)]), "1|2|3");
        assert_eq!(r(&[s("GET"), s("POST")]), "'GET'|'POST'");
        assert_eq!(r(&[i(1), i(2), i(1)]), "1|2"); // dedup
        assert_eq!(r(&[i(1), Val::Null]), "1|null");
        // Both bool literals ⟺ the whole `bool` type (PHPStan renders `bool`, not
        // `false|true`); a lone literal stays precise.
        assert_eq!(r(&[Val::Bool(true), Val::Bool(false)]), "bool");
        assert_eq!(r(&[Val::Bool(true)]), "true");
        // `bool` joins other precise members (e.g. an int literal alongside).
        assert_eq!(r(&[i(1), Val::Bool(true), Val::Bool(false)]), "1|bool");
        // Floats keep a visible fractional part; strings stay precise alongside ints.
        assert_eq!(r(&[Val::Float(123.0)]), "123.0");
        assert_eq!(r(&[Val::Float(2.5)]), "2.5");
        assert_eq!(r(&[i(-5)]), "-5");
    }

    #[test]
    fn singleton_renders_the_php_literal_not_the_base_type() {
        // The dump surface renders the literal itself (value-precision — the ADR-0053
        // §9 collapse is reversed for this path), NOT the collapsed base `int`. A
        // string literal is single-quoted; `null` is `null`.
        assert_eq!(render_dump_fact(&Fact::Singleton(i(5))), "5");
        assert_eq!(render_dump_fact(&Fact::Singleton(s("abc"))), "'abc'");
        assert_eq!(render_dump_fact(&Fact::Singleton(Val::Bool(false))), "false");
        assert_eq!(render_dump_fact(&Fact::Singleton(Val::Null)), "null");
    }

    #[test]
    fn abstract_layers_render_the_honest_keyword_ladder() {
        // General: bare base, with nullability.
        assert_eq!(render_dump_fact(&Fact::General { base: Base::Int, nullable: false }), "int");
        assert_eq!(
            render_dump_fact(&Fact::General { base: Base::String, nullable: true }),
            "string|null"
        );
        // Refined int range: the interval, PHPStan's own spelling for every range
        // (issue #90 — `positive-int` is phpdoc input sugar, never dump output).
        assert_eq!(
            render_dump_fact(&Fact::refined(Base::Int, Refinement::Int(IntRange::POSITIVE), false)),
            "int<1, max>"
        );
        // Refined string: reuse the speller's own preds_keyword so a refined-string
        // dump and its spell_arms rendering agree.
        let numeric = Fact::refined(Base::String, Refinement::Str(StrPreds::NUMERIC.close()), false);
        assert_eq!(
            render_dump_fact(&numeric),
            steins_contract::spell::preds_keyword(StrPreds::NUMERIC.close())
        );
    }

    #[test]
    fn array_bearing_fact_spells_through_the_d4_array_vocabulary() {
        // ADR-0062 §6 flip: the array-vocabulary slice teaches the speller, so an
        // array-bearing fact is no longer an honest-unknown refusal. The empty
        // array is denotationally a Yes-list (array_is_list([]) is vacuously
        // true, §3), and it is the one shape issue #163 does not print the word
        // for: `array{}` already says "no keys at all" and re-parses to the very
        // same `Yes`.
        let fact = Fact::Singleton(Val::Array(vec![]));
        assert_eq!(render_dump_fact(&fact), "array{}");
    }

    #[test]
    fn concrete_array_values_spell_value_precisely() {
        // Point 3 of the S1 mission fixtures: a keyed non-list value spells its
        // keys under `array{…}`; a sequential value IS a key sequence (a concrete
        // array is order-witnessed) and spells positionally under `list{…}`,
        // stating the fact rather than the oracle's spelling of it (issue #163).
        let map = Fact::Singleton(Val::Array(vec![(VKey::Str("a".into()), s("v"))]));
        assert_eq!(render_dump_fact(&map), "array{a: 'v'}");
        let list = Fact::Singleton(Val::Array(vec![(VKey::Int(0), s("x")), (VKey::Int(1), s("y"))]));
        assert_eq!(render_dump_fact(&list), "list{'x', 'y'}");
    }

    #[test]
    fn shape_fact_spells_through_the_shared_speller() {
        // ADR-0062 S1 point 4: `Fact::Shape` spells through the shared speller, so
        // every consumer inherits the rendering for free.
        use steins_domain::{Presence, ShapeFact, Tail};
        let shape = ShapeFact::normalize(
            vec![(VKey::Str("a".into()), Presence::Required { witnessed: false }, None)],
            Tail::Sealed,
            steins_domain::Certainty::Maybe,
            false,
            Vec::new(),
        );
        let fact = Fact::Shape { shape: Box::new(shape), nullable: false };
        // `ShapeFact::normalize` sets `non_empty` from the `Required` field
        // itself (shape.rs), but a sealed shape's required key already proves
        // non-emptiness, so issue #159 stops printing the modifier twice.
        assert_eq!(render_dump_fact(&fact), "array{a: mixed}");
    }
}

#[cfg(test)]
mod return_fact_admission_tests {
    //! ADR-0056 §1–2 — the pure admission-gate core ([`admit_return_fact`] and its
    //! helpers), tested without a sidecar. Covers: the reflected envelope alone for
    //! each single representable base; the un-representable cases (multi-base union,
    //! non-scalar, `mixed`/`void`); and the three curated-refinement legs — admitted
    //! (subset ∧ pinned), rejected by a failed subset check, and rejected by a minor
    //! mismatch. The R1 generated table is empty, so curation is exercised with
    //! hand-passed refinement strings here.
    use super::*;
    use crate::builtin_returns::admit_return_fact;
    use steins_contract::ContractTy;
    use crate::builtin_returns::{envelope_fact, floor_target_admits};

    #[test]
    fn envelope_alone_for_each_representable_base() {
        // A curated-less row (the R1 reality) seeds exactly the reflected base.
        assert_eq!(admit_return_fact("bool", None, true), Some(Fact::General { base: Base::Bool, nullable: false }));
        assert_eq!(admit_return_fact("int", None, true), Some(Fact::General { base: Base::Int, nullable: false }));
        assert_eq!(admit_return_fact("string", None, true), Some(Fact::General { base: Base::String, nullable: false }));
        assert_eq!(admit_return_fact("float", None, true), Some(Fact::General { base: Base::Float, nullable: false }));
        // A `?T` nullable envelope carries nullability.
        assert_eq!(admit_return_fact("?string", None, true), Some(Fact::General { base: Base::String, nullable: true }));
    }

    #[test]
    fn unrepresentable_envelopes_seed_nothing() {
        // A multi-base union (`int|false`) is not a single value-domain fact — the
        // union case is deferred (§4), so R1 seeds nothing rather than a wrong arm.
        assert_eq!(admit_return_fact("int|false", None, true), None);
        assert_eq!(admit_return_fact("string|int|false", None, true), None);
        // Non-scalars and the top/void keywords never seed.
        assert_eq!(admit_return_fact("array", None, true), None);
        assert_eq!(admit_return_fact("object", None, true), None);
        assert_eq!(admit_return_fact("mixed", None, true), None);
        assert_eq!(admit_return_fact("void", None, true), None);
        assert_eq!(admit_return_fact("DateTime", None, true), None);
    }

    #[test]
    fn curated_refinement_admitted_when_subset_and_pinned() {
        // `count(): int` envelope refined to `int<0, max>` — a subset of `int` — is
        // admitted at the pinned minor, yielding a Refined (narrower) int fact.
        let got = admit_return_fact("int", Some("int<0, max>"), true).expect("some fact");
        assert!(
            matches!(got, Fact::Refined { base: Base::Int, .. }),
            "the admitted refinement must be a Refined int, got {got:?}"
        );
        assert_ne!(got, Fact::General { base: Base::Int, nullable: false }, "must be narrower than the envelope");
    }

    #[test]
    fn curated_string_refinement_admitted_within_string_envelope() {
        // R4 shape: `sha1(): string` envelope refined to `non-falsy-string` — a
        // subset of `string`, same base — is admitted at the pinned minor as a
        // Refined string fact (narrower than the bare string envelope).
        let got = admit_return_fact("string", Some("non-falsy-string"), true).expect("some fact");
        assert!(
            matches!(got, Fact::Refined { base: Base::String, .. }),
            "the admitted refinement must be a Refined string, got {got:?}"
        );
        assert_ne!(
            got,
            Fact::General { base: Base::String, nullable: false },
            "must be narrower than the string envelope"
        );
    }

    #[test]
    fn curated_refinement_rejected_when_not_a_subset() {
        // `non-empty-string` is NOT a subset of an `int` envelope (base mismatch):
        // the row is discarded and the envelope stands alone (never a wrong premise).
        assert_eq!(
            admit_return_fact("int", Some("non-empty-string"), true),
            Some(Fact::General { base: Base::Int, nullable: false })
        );
    }

    #[test]
    fn curated_refinement_rejected_on_minor_mismatch() {
        // A perfectly valid subset refinement is still NOT admitted when the project
        // PHP minor differs from PINNED_PHP (the A11 narrowing-direction guard, §2):
        // the envelope stands alone.
        assert_eq!(
            admit_return_fact("int", Some("int<0, max>"), false),
            Some(Fact::General { base: Base::Int, nullable: false })
        );
    }

    #[test]
    fn envelope_fact_shapes() {
        assert_eq!(envelope_fact(&ContractTy::Base(Base::Bool)), Some(Fact::General { base: Base::Bool, nullable: false }));
        // A non-nullable multi-base union → None.
        assert_eq!(
            envelope_fact(&ContractTy::Union(vec![ContractTy::Base(Base::Int), ContractTy::LitBool(false)])),
            None
        );
    }

    /// The ADR-0069 floor's version gate, against the real change oracle.
    ///
    /// The gate's own law, unit-tested against the mined data: `str_split` is the
    /// witness whose declared return type moved at 8.2. (Whether that particular
    /// name also carries an admitted row is a property of the mining, not of this
    /// gate — `declared_return_floor.rs` pins the end-to-end decline on a name that
    /// does.)
    #[test]
    fn floor_target_gate_declines_below_a_names_change_boundary() {
        use steins_db::{PhpTarget, PhpTargetSource};
        let target = |floor: (u16, u16), ceiling: Option<(u16, u16)>| PhpTarget {
            floor,
            ceiling,
            source: PhpTargetSource::Require,
            raw: "test".to_owned(),
        };
        // `str_split`'s declared return type moved at 8.2.
        assert_eq!(steins_catalog::declared_return_changed_at("str_split"), Some((8, 2)));

        // A STRADDLING target has no single answer — decline (the A11 shape).
        assert!(!floor_target_admits("str_split", Some(&target((8, 1), Some((8, 5))))));
        assert!(!floor_target_admits("str_split", Some(&target((8, 1), None))));
        // A target lying entirely BELOW the boundary is just as wrong: the mined row
        // states the type at the pin, which that project never runs.
        assert!(!floor_target_admits("str_split", Some(&target((8, 1), Some((8, 1))))));
        // Wholly at or above the boundary: the row is exactly what that range runs.
        assert!(floor_target_admits("str_split", Some(&target((8, 2), Some((8, 2))))));
        assert!(floor_target_admits("str_split", Some(&target((8, 3), None))));
        // An UNDECLARED target admits — the row is Asserted anyway, and its
        // consumers tolerate that grade (ADR-0069 §3).
        assert!(floor_target_admits("str_split", None));
        // A name the oracle does not list is admitted for every target: its declared
        // return type never moved across the supported line.
        assert!(floor_target_admits("str_repeat", Some(&target((8, 1), None))));
        assert!(floor_target_admits("str_repeat", None));
    }
}

#[cfg(test)]
mod shape_projection_tests {
    //! ADR-0062 S7 — the positional projections over the order-DECLARED lane
    //! ([`shape_projection_fact`]'s helpers), tested as pure algebra.
    //!
    //! The headline is [`every_projection_admits_the_real_result`]: for every
    //! (shape, array) pair in the universe where the shape admits the array, the
    //! projected shape admits the array the real builtin produces. The reference
    //! results are the measured PHP semantics (`array_reverse` renumbers integer
    //! keys and keeps string ones; `array_flip` skips a non-`int|string` value),
    //! written out here rather than derived from the transfer under test.
    //!
    //! The second discipline is §2's rule: no transfer may read field
    //! declaration order. [`array_key_first_is_never_the_declared_first_key`] is
    //! its negative pin.
    use super::*;
    use crate::shape_projection::{
        project_flip, project_keys, project_reverse, project_values, shape_key_union,
    };
    use steins_domain::{Certainty, KeyClass, Presence, ShapeFact, Tail};

    fn ik(i: i64) -> VKey {
        VKey::Int(i)
    }

    fn sk(s: &str) -> VKey {
        VKey::Str(s.into())
    }

    fn req() -> Presence {
        Presence::Required { witnessed: false }
    }

    fn slot(f: Fact) -> Option<Box<Fact>> {
        Some(Box::new(f))
    }

    fn base_fact(base: Base) -> Fact {
        Fact::General { base, nullable: false }
    }

    /// `array{a: int, b?: string}` — the ADR's own fixture shape.
    fn declared_shape() -> ShapeFact {
        ShapeFact::normalize(
            vec![
                (sk("a"), req(), slot(base_fact(Base::Int))),
                (sk("b"), Presence::Optional, slot(base_fact(Base::String))),
            ],
            Tail::Sealed,
            Certainty::Maybe,
            false,
            Vec::new(),
        )
    }

    /// `list<int>`: an int-classed unsealed tail, denotationally a list.
    fn list_of_int() -> ShapeFact {
        ShapeFact::normalize(
            Vec::new(),
            Tail::Unsealed { key: KeyClass::Int, value: slot(base_fact(Base::Int)) },
            Certainty::Yes,
            false,
            Vec::new(),
        )
    }

    /// `array<string, int>`.
    fn map_str_int() -> ShapeFact {
        ShapeFact::normalize(
            Vec::new(),
            Tail::Unsealed { key: KeyClass::Str, value: slot(base_fact(Base::Int)) },
            Certainty::Maybe,
            false,
            Vec::new(),
        )
    }

    /// `list{string, int}` — issue #165's measured-table subject: sealed,
    /// all-required, `is_list == Yes` surviving `normalize`'s sharpening.
    fn sealed_list_str_int() -> ShapeFact {
        ShapeFact::normalize(
            vec![
                (ik(0), req(), slot(base_fact(Base::String))),
                (ik(1), req(), slot(base_fact(Base::Int))),
            ],
            Tail::Sealed,
            Certainty::Yes,
            false,
            Vec::new(),
        )
    }

    /// `list{int, 1?: string}` — the trailing-optional sequence form.
    fn sealed_list_trailing_optional() -> ShapeFact {
        ShapeFact::normalize(
            vec![
                (ik(0), req(), slot(base_fact(Base::Int))),
                (ik(1), Presence::Optional, slot(base_fact(Base::String))),
            ],
            Tail::Sealed,
            Certainty::Yes,
            false,
            Vec::new(),
        )
    }

    /// The concrete arrays the soundness sweep runs over, in *witnessed* order.
    fn arrays() -> Vec<Vec<(VKey, Val)>> {
        vec![
            vec![],
            vec![(ik(0), Val::Int(7))],
            vec![(ik(0), Val::Str("x".into())), (ik(1), Val::Str("y".into()))],
            vec![(ik(5), Val::Int(1)), (ik(9), Val::Int(2))],
            vec![(ik(1), Val::Int(2)), (ik(0), Val::Int(3))],
            vec![(sk("a"), Val::Int(1)), (sk("b"), Val::Str("x".into()))],
            vec![(sk("a"), Val::Int(1))],
            vec![(sk("b"), Val::Str("zz".into())), (sk("a"), Val::Int(4))],
            vec![(ik(0), Val::Int(1)), (sk("a"), Val::Int(2)), (ik(3), Val::Int(3))],
            vec![(ik(0), Val::Str("x".into())), (ik(1), Val::Int(1))],
        ]
    }

    fn shapes() -> Vec<ShapeFact> {
        let mut out = vec![
            declared_shape(),
            list_of_int(),
            map_str_int(),
            ShapeFact::plain_array(),
            sealed_list_str_int(),
            sealed_list_trailing_optional(),
        ];
        out.extend(arrays().iter().map(|a| ShapeFact::lift(a)));
        out
    }

    // ---- The reference results (measured PHP semantics) --------------------

    fn php_array_values(a: &[(VKey, Val)]) -> Vec<(VKey, Val)> {
        a.iter()
            .enumerate()
            .map(|(i, (_, v))| (ik(i64::try_from(i).expect("small")), v.clone()))
            .collect()
    }

    fn php_array_keys(a: &[(VKey, Val)]) -> Vec<(VKey, Val)> {
        a.iter()
            .enumerate()
            .map(|(i, (k, _))| (ik(i64::try_from(i).expect("small")), val_of_key(k)))
            .collect()
    }

    /// `array_flip`: values become keys (an `int` value gives an `int` key, a
    /// non-numeric `string` value a string key), and anything else is skipped.
    /// The universe carries no duplicate flipped key, so last-wins never arises.
    fn php_array_flip(a: &[(VKey, Val)]) -> Vec<(VKey, Val)> {
        a.iter()
            .filter_map(|(k, v)| {
                let nk = match v {
                    Val::Int(i) => VKey::Int(*i),
                    Val::Str(s) => VKey::Str(s.clone()),
                    _ => return None,
                };
                Some((nk, val_of_key(k)))
            })
            .collect()
    }

    /// `array_reverse($a)` with the default `$preserve_keys = false`: walk the
    /// entries backwards, keep string keys, renumber integer ones from 0.
    fn php_array_reverse(a: &[(VKey, Val)]) -> Vec<(VKey, Val)> {
        let mut next = 0i64;
        let mut out = Vec::with_capacity(a.len());
        for (k, v) in a.iter().rev() {
            match k {
                VKey::Str(_) => out.push((k.clone(), v.clone())),
                VKey::Int(_) => {
                    out.push((ik(next), v.clone()));
                    next += 1;
                }
            }
        }
        out
    }

    #[test]
    fn every_projection_admits_the_real_result() {
        let mut checked = 0usize;
        for shape in shapes() {
            for a in arrays() {
                if !shape.admits(&a) {
                    continue;
                }
                checked += 1;
                assert!(
                    project_values(&shape).admits(&php_array_values(&a)),
                    "array_values: {shape:?} on {a:?}"
                );
                assert!(
                    project_keys(&shape).admits(&php_array_keys(&a)),
                    "array_keys: {shape:?} on {a:?}"
                );
                assert!(
                    project_flip(&shape).admits(&php_array_flip(&a)),
                    "array_flip: {shape:?} on {a:?}"
                );
                assert!(
                    project_reverse(&shape).admits(&php_array_reverse(&a)),
                    "array_reverse: {shape:?} on {a:?}"
                );
                // The key-member transfer: `array_key_first`/`_last` return SOME
                // key, or `null` on the empty array — every one of which the
                // transfer's fact must admit.
                if let Some(keys) = shape_key_union(&shape) {
                    let member = if shape.non_empty {
                        keys
                    } else {
                        fact_admitting_null(&keys).expect("representable")
                    };
                    match (a.first(), a.last()) {
                        (Some((f, _)), Some((l, _))) => {
                            assert!(member.admits(&val_of_key(f)), "first: {shape:?} on {a:?}");
                            assert!(member.admits(&val_of_key(l)), "last: {shape:?} on {a:?}");
                        }
                        _ => assert!(member.admits(&Val::Null), "empty: {shape:?}"),
                    }
                }
            }
        }
        // The sweep is only evidence if the pairs exist.
        assert!(checked >= 20, "universe too small: {checked} admitted pairs");
    }

    // ---- §2's rule: declaration order is never read ------------------------

    #[test]
    fn array_key_first_is_never_the_declared_first_key() {
        // Negative soundness test: `array{a: int, b: int}` is a key SET;
        // PHPStan answers `'a'` here (phpstan/phpstan#14940) and is wrong on
        // `['b' => 1, 'a' => 2]`, which the shape admits just as well.
        let shape = ShapeFact::normalize(
            vec![(sk("a"), req(), slot(base_fact(Base::Int))), (sk("b"), req(), slot(base_fact(Base::Int)))],
            Tail::Sealed,
            Certainty::Maybe,
            false,
            Vec::new(),
        );
        let keys = shape_key_union(&shape).expect("enumerable");
        assert_eq!(
            keys,
            Fact::OneOf(vec![Val::Str("a".into()), Val::Str("b".into())])
        );
        assert!(keys.admits(&Val::Str("b".into())));
        // Both fields are Required, so the array cannot be empty and no `null`
        // joins in.
        assert!(shape.non_empty);
    }

    #[test]
    fn a_possibly_empty_shape_admits_null_as_its_key_member() {
        let keys = shape_key_union(&map_str_int()).expect("string class");
        let member = fact_admitting_null(&keys).expect("representable");
        assert_eq!(member, Fact::General { base: Base::String, nullable: true });
    }

    // ---- Per-projection structure -----------------------------------------

    #[test]
    fn array_values_is_a_list_of_the_value_union() {
        // `int ⊔ string` IS one fact now (issue #339), so the value slot carries
        // the union where it used to widen to the unknown floor.
        let p = project_values(&declared_shape());
        assert_eq!(p.is_list, Certainty::Yes);
        assert!(p.non_empty);
        assert!(p.fields.is_empty());
        assert_eq!(
            p.tail,
            Tail::Unsealed {
                key: KeyClass::Int,
                value: Fact::union(vec![(Base::Int, None), (Base::String, None)], false)
                    .map(Box::new),
            }
        );

        // A homogeneous shape keeps its value bound.
        let same = ShapeFact::normalize(
            vec![
                (sk("a"), req(), slot(base_fact(Base::Int))),
                (sk("b"), Presence::Optional, slot(base_fact(Base::Int))),
            ],
            Tail::Sealed,
            Certainty::Maybe,
            false,
            Vec::new(),
        );
        assert_eq!(
            project_values(&same).tail,
            Tail::Unsealed { key: KeyClass::Int, value: slot(base_fact(Base::Int)) }
        );
    }

    #[test]
    fn array_keys_enumerates_a_sealed_shapes_keys_and_widens_an_unsealed_one() {
        assert_eq!(
            project_keys(&declared_shape()).tail,
            Tail::Unsealed {
                key: KeyClass::Int,
                value: slot(Fact::OneOf(vec![
                    Val::Str("a".into()),
                    Val::Str("b".into())
                ])),
            }
        );
        // An `array-key`-classed tail is `int|string`, which IS one fact now
        // (issue #339) — the element slot carries it instead of widening to the
        // unknown floor.
        assert_eq!(
            project_keys(&ShapeFact::plain_array()).tail,
            Tail::Unsealed {
                key: KeyClass::Int,
                value: Fact::union(vec![(Base::Int, None), (Base::String, None)], false)
                    .map(Box::new),
            }
        );
        // An unsealed Yes-list's keys are `0..n-1` — never negative, so the
        // element bound sharpens past the bare `int` class (issue #165).
        assert_eq!(
            project_keys(&list_of_int()).tail,
            Tail::Unsealed {
                key: KeyClass::Int,
                value: slot(Fact::refined(
                    Base::Int,
                    Refinement::Int(IntRange::NON_NEGATIVE),
                    false
                )),
            }
        );
    }

    #[test]
    fn array_flip_drops_non_empty_and_only_claims_int_keys_for_int_values() {
        let p = project_flip(&declared_shape());
        // Values are `int|string`; a string value can still produce an INT key
        // (PHP's array-key cast), so the class is `array-key`.
        assert!(matches!(p.tail, Tail::Unsealed { key: KeyClass::ArrayKey, .. }));
        // A non-`int|string` value is skipped by the flip, so the result may be
        // empty even though the input is not.
        assert!(!p.non_empty);

        let ints = ShapeFact::normalize(
            vec![(sk("a"), req(), slot(base_fact(Base::Int)))],
            Tail::Sealed,
            Certainty::Maybe,
            false,
            Vec::new(),
        );
        assert!(matches!(
            project_flip(&ints).tail,
            Tail::Unsealed { key: KeyClass::Int, .. }
        ));
    }

    #[test]
    fn array_reverse_reads_the_key_structure_three_ways() {
        // All-int keys: everything is renumbered, so the result IS a list.
        assert_eq!(project_reverse(&list_of_int()).is_list, Certainty::Yes);
        // A required string key survives the reversal — never a list.
        assert_eq!(project_reverse(&declared_shape()).is_list, Certainty::No);
        // A string key that may or may not be there: the honest widening.
        let optional_str = ShapeFact::normalize(
            vec![(sk("a"), Presence::Optional, slot(base_fact(Base::Int)))],
            Tail::Sealed,
            Certainty::Maybe,
            false,
            Vec::new(),
        );
        assert_eq!(project_reverse(&optional_str).is_list, Certainty::Maybe);
        // The entry count is preserved, so `non_empty` carries.
        assert!(project_reverse(&declared_shape()).non_empty);
    }

    // ---- The SEQUENCE lane (issue #165): isList == Yes is realizable order --

    #[test]
    fn array_values_is_the_identity_on_a_proven_list() {
        // A Yes-list's keys are already `0..n-1` in realizable order (probed:
        // `array_values(["x", 1]) === ["x", 1]`), so the projection returns
        // the subject's own shape — element types, optionality and
        // non-emptiness intact — where the set widening drops the
        // heterogeneous element types to the unknown floor.
        assert_eq!(project_values(&sealed_list_str_int()), sealed_list_str_int());
        assert_eq!(
            project_values(&sealed_list_trailing_optional()),
            sealed_list_trailing_optional()
        );
        // The unsealed forms: `list<T>` and `non-empty-list<T>`.
        assert_eq!(project_values(&list_of_int()), list_of_int());
        let non_empty = ShapeFact::normalize(
            Vec::new(),
            Tail::Unsealed { key: KeyClass::Int, value: slot(base_fact(Base::Int)) },
            Certainty::Yes,
            true,
            Vec::new(),
        );
        assert_eq!(project_values(&non_empty), non_empty);
    }

    #[test]
    fn array_keys_of_a_proven_sequence_is_the_literal_key_list() {
        // Probed: `array_keys(["x", 1, 2.5]) === [0, 1, 2]` — a list's keys
        // ARE the sequence `0..n-1`, so the sealed all-required answer is the
        // literal `list{0, 1}`.
        let expected = ShapeFact::normalize(
            vec![
                (ik(0), req(), slot(Fact::Singleton(Val::Int(0)))),
                (ik(1), req(), slot(Fact::Singleton(Val::Int(1)))),
            ],
            Tail::Sealed,
            Certainty::Yes,
            false,
            Vec::new(),
        );
        assert_eq!(project_keys(&sealed_list_str_int()), expected);
        // A trailing optional carries per position: `list{A, 1?: B}` realizes
        // as `[A]` or `[A, B]`, whose key arrays are `[0]` and `[0, 1]` (both
        // probed) — exactly `list{0, 1?: 1}`.
        let expected = ShapeFact::normalize(
            vec![
                (ik(0), req(), slot(Fact::Singleton(Val::Int(0)))),
                (ik(1), Presence::Optional, slot(Fact::Singleton(Val::Int(1)))),
            ],
            Tail::Sealed,
            Certainty::Yes,
            false,
            Vec::new(),
        );
        assert_eq!(project_keys(&sealed_list_trailing_optional()), expected);
    }

    #[test]
    fn array_reverse_of_a_sealed_all_required_sequence_reverses_it() {
        // Probed at lengths 1, 2 and 3: `array_reverse(["a", "b", "c"]) ===
        // ["c", "b", "a"]` — position `i` takes the subject's position
        // `n-1-i`, so `list{string, int}` reverses to `list{int, string}`.
        let expected = ShapeFact::normalize(
            vec![
                (ik(0), req(), slot(base_fact(Base::Int))),
                (ik(1), req(), slot(base_fact(Base::String))),
            ],
            Tail::Sealed,
            Certainty::Yes,
            false,
            Vec::new(),
        );
        assert_eq!(project_reverse(&sealed_list_str_int()), expected);
    }

    #[test]
    fn array_reverse_declines_the_positional_claim_on_an_optional_key() {
        // Probed: `"a"` sits at index 0 in `array_reverse(["a"])` but at
        // index 1 in `array_reverse(["a", "b"])` — a variable-length reversal
        // smears every position, so an optional key keeps today's widening
        // exactly (the value union under an int-classed list tail, `non_empty`
        // carried). The union that value slot carries is `int|string`, which
        // issue #339 made expressible — the widening is the same one, said
        // more precisely.
        let expected = ShapeFact::normalize(
            Vec::new(),
            Tail::Unsealed {
                key: KeyClass::Int,
                value: Fact::union(vec![(Base::Int, None), (Base::String, None)], false)
                    .map(Box::new),
            },
            Certainty::Yes,
            true,
            Vec::new(),
        );
        assert_eq!(project_reverse(&sealed_list_trailing_optional()), expected);
    }

    #[test]
    fn a_set_subject_keeps_todays_widenings_exactly() {
        // The doctrinal pin (issue #165): `array{a: 1, b: 2}` is a key SET —
        // `['b' => 2, 'a' => 1]` is admitted just as well — so no projection
        // may consume an order from it. `array_values` still answers the
        // value union as a non-empty list: the issue's pinned
        // `non-empty-list<1|2>`.
        let subject = ShapeFact::normalize(
            vec![
                (sk("a"), req(), slot(Fact::Singleton(Val::Int(1)))),
                (sk("b"), req(), slot(Fact::Singleton(Val::Int(2)))),
            ],
            Tail::Sealed,
            Certainty::Maybe,
            false,
            Vec::new(),
        );
        assert_eq!(subject.is_list, Certainty::No, "a required string key is never a list");
        let values = project_values(&subject);
        assert_eq!(
            values.tail,
            Tail::Unsealed {
                key: KeyClass::Int,
                value: slot(Fact::OneOf(vec![Val::Int(1), Val::Int(2)])),
            }
        );
        assert!(values.non_empty);
        assert_eq!(values.is_list, Certainty::Yes);
        assert_eq!(
            project_keys(&subject).tail,
            Tail::Unsealed {
                key: KeyClass::Int,
                value: slot(Fact::OneOf(vec![
                    Val::Str("a".into()),
                    Val::Str("b".into())
                ])),
            }
        );
    }

    #[test]
    fn a_guard_flagged_sequence_with_incoherent_fields_declines_the_positional_claims() {
        // `array{0: int, 2?: int}` narrowed by an `array_is_list` guard: the
        // flag is `Yes` (key `2` can then never actually be present), but the
        // FIELDS do not spell the sequence the flag claims. The positional
        // claims decline — `array_keys` answers from the flag alone (a list's
        // keys are never negative), `array_reverse` keeps the widening — while
        // `array_values` stays the identity, exact for every admitted value
        // whatever the fields say.
        let subject = ShapeFact::normalize(
            vec![
                (ik(0), req(), slot(base_fact(Base::Int))),
                (ik(2), Presence::Optional, slot(base_fact(Base::Int))),
            ],
            Tail::Sealed,
            Certainty::Yes,
            false,
            Vec::new(),
        );
        assert_eq!(subject.is_list, Certainty::Yes, "the guard flag survives normalize");
        let keys = project_keys(&subject);
        assert!(keys.fields.is_empty(), "no literal key list from incoherent fields");
        assert_eq!(
            keys.tail,
            Tail::Unsealed {
                key: KeyClass::Int,
                value: slot(Fact::refined(
                    Base::Int,
                    Refinement::Int(IntRange::NON_NEGATIVE),
                    false
                )),
            }
        );
        assert!(
            project_reverse(&subject).fields.is_empty(),
            "no reversed sequence from incoherent fields"
        );
        assert_eq!(project_values(&subject), subject);
    }
}

#[cfg(test)]
mod php_view_tests {
    use super::*;
    use steins_db::{PhpTarget, PhpTargetSource};

    fn target(floor: (u16, u16), ceiling: Option<(u16, u16)>) -> PhpTarget {
        PhpTarget { floor, ceiling, source: PhpTargetSource::Require, raw: String::new() }
    }

    /// Issue #28: the one seam both A11 and A12 follow (and, since #29, the
    /// PHP_VERSION_ID guard interval).
    #[test]
    fn a_declared_target_overrides_the_runtime() {
        // A range straddling the A12 boundary declines the effective minor
        // (boundary-sensitive literals must decline) and skews the catalog; the
        // version-id interval spans the declared range [8.1.00, 8.99.99].
        let caret81 = target((8, 1), Some((8, u16::MAX)));
        let v = effective_php_view(Some((8, 5)), Some(&caret81));
        assert_eq!((v.effective_minor, v.catalog_skew), (None, true));
        assert_eq!(v.version_id, Some((80100, Some(89999))));
        // A range entirely below the boundary answers with its floor.
        let old = target((8, 1), Some((8, 2)));
        let v = effective_php_view(Some((8, 5)), Some(&old));
        assert_eq!((v.effective_minor, v.catalog_skew), (Some((8, 1)), true));
        assert_eq!(v.version_id, Some((80100, Some(80299))));
        // A range entirely at/above the boundary answers with its floor too; an
        // open ceiling is an open interval.
        let new = target((8, 3), None);
        let v = effective_php_view(Some((8, 1)), Some(&new));
        assert_eq!((v.effective_minor, v.catalog_skew), (Some((8, 3)), true));
        assert_eq!(v.version_id, Some((80300, None)));
        // A target pinned exactly to the catalog pin carries no skew.
        let pinned = target(steins_catalog::PINNED_PHP, Some(steins_catalog::PINNED_PHP));
        let v = effective_php_view(None, Some(&pinned));
        assert_eq!((v.effective_minor, v.catalog_skew), (Some(steins_catalog::PINNED_PHP), false));
    }

    /// No declaration: the pre-#28 posture, verbatim — runtime minor passthrough,
    /// skew iff the runtime differs from the pin; the version-id interval spans
    /// the runtime's minor (the exact patch is unknown).
    #[test]
    fn no_target_falls_back_to_the_runtime() {
        let v = effective_php_view(Some(steins_catalog::PINNED_PHP), None);
        assert_eq!((v.effective_minor, v.catalog_skew), (Some(steins_catalog::PINNED_PHP), false));
        let v = effective_php_view(Some((8, 1)), None);
        assert_eq!((v.effective_minor, v.catalog_skew), (Some((8, 1)), true));
        assert_eq!(v.version_id, Some((80100, Some(80199))));
        let v = effective_php_view(None, None);
        assert_eq!((v.effective_minor, v.catalog_skew, v.version_id), (None, false, None));
    }
}

#[cfg(test)]
mod fold_wire_tests {
    //! The fold seam's admission, checked where it is now decided *once*.
    //!
    //! [`fits_fold_budget`]'s gate and [`arg_to_fold_within`]'s encoder run over
    //! the same argument at different moments — the gate before `folder.fold` so
    //! an inadmissible literal is never cloned into the memo, the encoder inside
    //! it — and the seam's standing invariant is that they never disagree. A gate
    //! that admits what the encoder refuses asks the engine a question it cannot
    //! be given; a gate that refuses what the encoder would send loses a fold for
    //! no reason. Both now read [`scalar_to_fold`] and [`array_key_to_fold`], so
    //! this asserts the property rather than two transcriptions of it.
    use steins_sidecar::FoldKey;
    use crate::fold_args::{arg_to_fold, array_key_to_fold, is_fold_arg};
    use steins_domain::PhpStr;
    use steins_syntax::{ArgValue, ArrayKey};

    /// A byte string with no UTF-8 reading — `as_str()` is `None`, ADR-0080.
    fn raw_byte_string() -> PhpStr {
        PhpStr::from_bytes(&[0xC0])
    }

    /// Every value the seam has an opinion about, sendable or not.
    fn every_shape() -> Vec<ArgValue> {
        let inner = ArgValue::Array(vec![(ArrayKey::Auto, ArgValue::Int(1))]);
        vec![
            ArgValue::Int(1),
            ArgValue::Float(1.5),
            ArgValue::Float(-0.0),
            ArgValue::Float(f64::MAX),
            ArgValue::Float(f64::INFINITY),
            ArgValue::Float(f64::NEG_INFINITY),
            ArgValue::Float(f64::NAN),
            ArgValue::Str(PhpStr::from("ab")),
            ArgValue::Str(raw_byte_string()),
            ArgValue::Bool(true),
            ArgValue::Null,
            ArgValue::Other,
            ArgValue::Array(vec![]),
            ArgValue::Array(vec![(ArrayKey::Auto, ArgValue::Int(1))]),
            // The hazards, each buried one level down: the array is admissible
            // in every other respect, and one entry has to take it down.
            ArgValue::Array(vec![(ArrayKey::Auto, ArgValue::Float(f64::INFINITY))]),
            ArgValue::Array(vec![(ArrayKey::Auto, ArgValue::Str(raw_byte_string()))]),
            ArgValue::Array(vec![(ArrayKey::Str(raw_byte_string()), ArgValue::Int(1))]),
            ArgValue::Array(vec![(ArrayKey::Expr(Box::new(ArgValue::Other)), ArgValue::Int(1))]),
            ArgValue::Array(vec![(ArrayKey::Int(-3), ArgValue::Null)]),
            ArgValue::Array(vec![(ArrayKey::Auto, ArgValue::Other)]),
            // …and nested twice, since the walk is where the two recursions
            // could still drift apart.
            ArgValue::Array(vec![(ArrayKey::Auto, inner)]),
            ArgValue::Array(vec![(
                ArrayKey::Auto,
                ArgValue::Array(vec![(ArrayKey::Auto, ArgValue::Float(f64::NAN))]),
            )]),
        ]
    }

    #[test]
    fn the_gate_admits_exactly_what_the_encoder_sends() {
        for v in every_shape() {
            assert_eq!(
                is_fold_arg(&v),
                arg_to_fold(&v).is_some(),
                "the gate and the encoder disagree about {v:?}"
            );
        }
    }

    /// The three values that have no JSON spelling, named so a future reader
    /// sees WHICH shapes the agreement above is really about.
    #[test]
    fn a_value_with_no_wire_spelling_is_refused_by_both() {
        for v in [
            ArgValue::Float(f64::INFINITY),
            ArgValue::Str(raw_byte_string()),
            ArgValue::Array(vec![(ArrayKey::Str(raw_byte_string()), ArgValue::Int(1))]),
            ArgValue::Array(vec![(ArrayKey::Expr(Box::new(ArgValue::Other)), ArgValue::Int(1))]),
        ] {
            assert!(!is_fold_arg(&v), "the gate admits {v:?}");
            assert_eq!(arg_to_fold(&v), None, "the encoder sends {v:?}");
        }
        // The neighbours still travel: this refuses spellings, not types.
        assert!(arg_to_fold(&ArgValue::Float(f64::MAX)).is_some());
        assert!(arg_to_fold(&ArgValue::Str(PhpStr::from("ab"))).is_some());
    }

    /// An absent key is a key the wire carries (`null`, for PHP's next-int
    /// rule); it is not the "no spelling" answer, and the nesting in
    /// [`array_key_to_fold`]'s return type is what keeps them apart.
    #[test]
    fn an_absent_key_is_not_a_refused_key() {
        assert_eq!(array_key_to_fold(&ArrayKey::Auto), Some(None));
        assert_eq!(array_key_to_fold(&ArrayKey::Int(7)), Some(Some(FoldKey::Int(7))));
        assert_eq!(array_key_to_fold(&ArrayKey::Expr(Box::new(ArgValue::Other))), None);
        assert_eq!(array_key_to_fold(&ArrayKey::Str(raw_byte_string())), None);
    }
}
