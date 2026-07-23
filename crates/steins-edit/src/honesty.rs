//! Transform #2 — phpdoc-honesty repair (ADR-0037 point 4 / ADR-0041 point 4).
//!
//! The inverse of promotion. Where a `@param`/`@return` tag **lies** — call-site
//! or return propagation proves values the declared type does not admit — this
//! transform rewrites the tag's *type text* to the proven truth, turning a lying
//! docblock into machine-fixed debt (ADR-0037: widen a lying `@param int $id` to
//! the observed proven union `int|numeric-string`).
//!
//! ## Enumeration domain
//! Exactly the sites where `phpdoc.param-mismatch` / `phpdoc.return-mismatch`
//! fire, restricted to v1 scope:
//! - **Free functions and methods** (ADR-0043 §6). Free-function candidates prove
//!   against [`sweep_free_functions`]; method candidates key on the `Sym::Method`
//!   identity `(class_fqn, method)` and prove against [`sweep_methods`], subject to
//!   the same ADR-0041 §1 eligibility split as method promotion (a non-eligible
//!   method refuses `method-inheritance`; a magic method refuses `magic-method`).
//!   Free-function planning is byte-identical to before this extension — the two
//!   sweeps are independent, and every existing transform test passes unchanged.
//! - **Literal-only proofs**: a non-literal call-site argument (`@param`) or a
//!   non-literal return (`@return`) refuses rather than guesses. The
//!   abstract-fact portion of a mismatch (a typed `$var` with no literal value)
//!   is outside v1's literal scope — the same soundness-not-completeness posture
//!   promotion takes.
//!
//! ## The widened type is the join of proven facts ONLY
//! The declared type is never gratuitously unioned in (ADR-0041): if callers pass
//! `int` and numeric-string literals, the honest type is `int|numeric-string`; if
//! no caller passes `int` where `@param int` is declared, the honest type is what
//! is proven, not `int|…`. Proven beats declared (ADR-0037 iron rule). The join
//! is rendered via [`crate::common::render_value_domain`]; a set with no faithful
//! phpdoc spelling refuses `type-not-renderable` rather than over-widening.
//!
//! ## `@param` vs `@return`
//! - `@param`: all call sites must be enumerable and every relevant argument
//!   literal-proven (the same obstacles → same refusals as promotion). The join
//!   is over **all** observed args (not only the violating ones — the honest type
//!   must admit every observed value).
//! - `@return`: the join is over the function's **own** return-site facts; every
//!   return path must be literal-proven or refuse `return-not-proven`. No caller
//!   sweep is needed. A body whose returns v1 cannot fully enumerate (a loop /
//!   `try` / `switch` that may hide a return, or a fall-through that implicitly
//!   returns `null`) refuses `return-not-proven` rather than under-widen.
//!
//! ## Planner-level safety (ADR-0041, binding)
//! Docblock-only edits cannot compile-fatal, but two disciplines are enforced by
//! construction, not by the post-check:
//! - Never contradict an existing **native** hint: if a native hint exists and
//!   the proven join is not admitted by it, that is a different disease — refuse
//!   `native-contradicts-proven`, it needs human eyes.
//! - `@phpstan-`/`@psalm-` precedence: rewrite the **governing** tag; leave a
//!   plain sibling untouched only when it still admits the proven join, else
//!   rewrite both.

use std::collections::HashMap;

use steins_contract::{ContractTy, admits_val, lower};
use steins_db::{Db, Project, SourceFile, parse};
use steins_domain::{Certainty, Val};
use steins_infer::is_vendor_path;
use steins_infer::promote::{
    FreeFnSweep, MethodEligibility, MethodSweep, TargetSweep, sweep_free_functions, sweep_methods,
};
use steins_phpdoc::docblock::DocTag;
use steins_phpdoc::{TagKind, parse_type, scan_docblock};
use steins_syntax::{
    ArgValue, ClassDecl, FunctionDecl, MethodDecl, NativeType, Param, Scope, ScopeOwner, SourceTree,
    Stmt, StmtKind,
};

use crate::common::{
    admits_all, arg_to_val, check_caller_enumerability, check_method_caller_enumerability,
    count_fqns, method_param_site, method_return_site, native_contract, param_site,
    render_value_domain, return_site, REASON_ARG_NOT_PROVEN,
};
use crate::obstacles::{self, VouchSet};
use crate::plan::{ByteSpan, Edit, EditPlan};
use crate::transform::{CompletenessOracle, Refusal, SiteRef, Transform, TransformReport};

// ---- Stable refusal reason names (ADR-0034 point 2, honesty-specific) ------
//
// The caller-enumerability reasons (`dynamic-call-present`,
// `function-referenced-as-value`, `resolution-ambiguous`, `named-or-spread-args`,
// `argument-not-proven`) are shared with promotion and re-exported here.
pub use crate::common::{
    REASON_AMBIGUOUS, REASON_DYNAMIC_CALL, REASON_DYNAMIC_INCLUDE, REASON_EVAL_PRESENT,
    REASON_MAGIC_METHOD, REASON_METHOD_INHERITANCE, REASON_NAMED_OR_SPREAD,
    REASON_REFERENCED_AS_VALUE,
};
pub use crate::common::REASON_ARG_NOT_PROVEN as REASON_ARGUMENT_NOT_PROVEN;

/// A `@return` site whose return paths v1 cannot fully prove as literals: a
/// non-literal return, a body that may fall through (implicitly returning `null`),
/// or one whose returns hide inside a loop/`try`/`switch` the trace does not model.
pub const REASON_RETURN_NOT_PROVEN: &str = "return-not-proven";
/// The proven value set has no faithful ADR-0029 phpdoc spelling (e.g. it mixes in
/// an array). Refusing is honest; over-widening to make rendering easy is a new lie.
pub const REASON_TYPE_NOT_RENDERABLE: &str = "type-not-renderable";
/// An existing **native** hint on the same param/return does not admit the proven
/// join — a different disease (the native contract is being violated at runtime),
/// which needs human eyes, not a docblock rewrite.
pub const REASON_NATIVE_CONTRADICTS: &str = "native-contradicts-proven";

/// The phpdoc-honesty transform (ADR-0037 point 4).
#[derive(Debug, Clone, Copy, Default)]
pub struct PhpdocHonesty;

impl Transform for PhpdocHonesty {
    fn id(&self) -> &'static str {
        "phpdoc-honesty"
    }
}

/// Plan the phpdoc-honesty repair over `project`. Pure planning: no files are
/// written and no diagnostics are re-checked here — the caller (CLI) drives the
/// dry-run diff, the dual-verification post-check, and any `--apply` write
/// (ADR-0034 point 3).
///
/// `vouches` are the user-vouched dynamic-code sites (`steins.toml`); pass
/// [`VouchSet::empty`] when none. A standing (unvouched) `eval` / dynamic-include
/// obstacle (ADR-0046 §2) makes "all callers proven" unknowable project-wide, so
/// *every* candidate refuses while one remains.
///
/// `partitions` is the region map (ADR-0047 §6), `None` for the single-region
/// identity. **Slice A wires it through but does not consume it**: no honesty
/// decision reads the map yet, so the plan is byte-identical whether it is `None`,
/// an identity [`single_region`](crate::PartitionMap::single_region) map, or a
/// fully-declared map.
#[must_use]
pub fn plan_phpdoc_honesty(
    db: &dyn Db,
    project: Project,
    vouches: &VouchSet,
    partitions: Option<&crate::regions::PartitionMap>,
) -> TransformReport {
    // ADR-0047 Slice A: received, deliberately not consumed (zero behavior change).
    let _ = partitions;
    let sweep = sweep_free_functions(db, project);
    // The class-world reverse sweep (ADR-0043 §6): method targets, taints, and the
    // ADR-0041 §1 eligibility verdicts (honesty applies the same split).
    let msweep = sweep_methods(db, project);
    let files: Vec<SourceFile> = project.files(db).to_vec();
    let fqn_counts = count_fqns(db, &files);

    // Project-global dynamic-code obstacles (ADR-0046 §2): recorded once, and — if
    // any stands unvouched — every candidate refuses with its reason.
    let dynamism = obstacles::detect(db, project, vouches);
    let blocking = dynamism.blocking_reason();

    let mut plan = EditPlan::new();
    let mut refusals: Vec<Refusal> = Vec::new();
    let mut oracle = CompletenessOracle::default();

    for &file in &files {
        let path = file.path(db);
        // Vendor files participate in the reverse SWEEP but are never transform
        // CANDIDATES: a docblock rewrite into `vendor/` is outside the tool's write
        // contract (composer overwrites it; vendor diagnostics are off, ADR-0015).
        // Candidate enumeration is project-only; the sweeps above still span vendor.
        if is_vendor_path(path) {
            continue;
        }
        let tree = parse(db, file);
        // Free-function bodies by written name, for `@return` return-site scans.
        let scopes = scopes_by_function(tree);
        for func in tree.functions() {
            let tags = func.docblock.as_deref().map(scan_docblock).unwrap_or_default();
            plan_params(
                func, &tags, path, tree, &sweep, &fqn_counts, blocking.as_ref(), &mut plan,
                &mut refusals, &mut oracle,
            );
            plan_return(
                func, &tags, path, tree, &scopes, blocking.as_ref(), &mut plan, &mut refusals,
                &mut oracle,
            );
        }
        // Method bodies keyed by `(class_fqn, method)`, for method `@return` scans.
        let method_scopes = scopes_by_method(tree);
        for class in tree.classes() {
            for method in &class.methods {
                let tags = method.docblock.as_deref().map(scan_docblock).unwrap_or_default();
                plan_method_params(
                    class, method, &tags, path, tree, &msweep, blocking.as_ref(), &mut plan,
                    &mut refusals, &mut oracle,
                );
                plan_method_return(
                    class, method, &tags, path, tree, &method_scopes, &msweep, blocking.as_ref(),
                    &mut plan, &mut refusals, &mut oracle,
                );
            }
        }
    }

    TransformReport {
        plan,
        refusals,
        oracle,
        obstacles: dynamism.obstacles,
        vouched_exemptions: dynamism.vouched_exemptions,
    }
}

// ---- `@param` honesty ------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn plan_params(
    func: &FunctionDecl,
    tags: &[DocTag],
    path: &str,
    tree: &SourceTree,
    sweep: &FreeFnSweep,
    fqn_counts: &HashMap<String, usize>,
    blocking: Option<&(&'static str, String)>,
    plan: &mut EditPlan,
    refusals: &mut Vec<Refusal>,
    oracle: &mut CompletenessOracle,
) {
    let target = sweep.targets.get(&func.fqn);
    for (idx, param) in func.params.iter().enumerate() {
        // Governing `@param` for this parameter (`@phpstan-`/`@psalm-` wins).
        let (Some(gov), plain) = param_tags(tags, TagKind::Param, Some(&param.name)) else {
            continue;
        };
        // Assertion-helper exemption (ADR-0030): a `@…-assert T $x` makes `@param`
        // a post-condition, so the checker never fires param-mismatch here.
        if assert_targets(tags, &param.name) {
            continue;
        }
        // The declared contract must parse; otherwise there is no envelope and no
        // mismatch fires — out of domain (not a candidate).
        let Some((gov_contract, _consumed)) = tag_contract(gov) else { continue };

        // Gather the observed literal values at this position, and detect whether
        // any of them proves the tag lies (a param-mismatch would fire). A `null`
        // value that the parameter accepts by native-nullable / `= null` default
        // is not a violation (mirrors `check_phpdoc_param`).
        let matches = |p: usize| if param.variadic { p >= idx } else { p == idx };
        // ADR-0043 stage 1: an object-bearing native type gives no native-nullable
        // signal (it lowered to `None` before ADR-0043); only scalar-value types do.
        let null_ok = param.has_null_default
            || param.ty.as_ref().is_some_and(|t| t.nullable && !t.has_instance());
        let mut lie = false;
        if let Some(t) = target {
            for obs in t.observed.iter().filter(|o| matches(o.param_index)) {
                if let Some(v) = arg_to_val(&obs.value) {
                    if matches!(v, Val::Null) && null_ok {
                        continue;
                    }
                    if admits_val(&gov_contract, &v) == Certainty::No {
                        lie = true;
                    }
                }
            }
        }
        if !lie {
            continue; // the tag is honest (or unprovably so) — nothing to repair.
        }

        // Enumerated site (ADR-0034 point 3b): must end transformed-or-refused.
        oracle.enumerated += 1;
        let site = param_site(path, tree, func, param);
        // A standing project-global obstacle (ADR-0046 §2) refuses every candidate.
        if let Some((reason, detail)) = blocking {
            oracle.refused += 1;
            refusals.push(Refusal::new(site, *reason, detail.clone()));
            continue;
        }
        let doc_start = func.docblock_span.map_or(0, |s| s.start);
        match decide_param(
            func, param, idx, path, doc_start, gov, plain, target, sweep, fqn_counts,
        ) {
            Ok(edits) => account(plan, oracle, refusals, &site, edits),
            Err((reason, detail)) => {
                oracle.refused += 1;
                refusals.push(Refusal::new(site, reason, detail));
            }
        }
    }
}

/// Decide a single `@param` honesty candidate: `Ok(edits)` to rewrite the
/// governing (and possibly the plain) tag, `Err((reason, detail))` to refuse.
#[allow(clippy::too_many_arguments)]
fn decide_param(
    func: &FunctionDecl,
    param: &Param,
    idx: usize,
    path: &str,
    doc_start: u32,
    gov: &DocTag,
    plain: Option<&DocTag>,
    target: Option<&TargetSweep>,
    sweep: &FreeFnSweep,
    fqn_counts: &HashMap<String, usize>,
) -> Result<Vec<Edit>, (&'static str, String)> {
    // All callers must be enumerable (shared obstacles), and the reaching calls
    // must be positional.
    check_caller_enumerability(func, sweep, fqn_counts)?;
    if target.is_some_and(|t| t.named_or_spread) {
        return Err((
            REASON_NAMED_OR_SPREAD,
            "a call reaching this function used named or spread arguments".to_owned(),
        ));
    }

    // Every observed argument at this position must be a proven literal — the join
    // must admit *every* observed value, not only the violating ones.
    let matches = |p: usize| if param.variadic { p >= idx } else { p == idx };
    let mut vals: Vec<Val> = Vec::new();
    if let Some(t) = target {
        for obs in t.observed.iter().filter(|o| matches(o.param_index)) {
            let Some(v) = arg_to_val(&obs.value) else {
                return Err((
                    REASON_ARG_NOT_PROVEN,
                    format!(
                        "call at {}:{}:{} passes `{}`, not a proven literal",
                        obs.caller_path,
                        obs.line,
                        obs.column,
                        obs.value.render()
                    ),
                ));
            };
            vals.push(v);
        }
    }
    dedup(&mut vals);

    // Never contradict an existing native hint (ADR-0041): if the native type does
    // not admit the proven join, that is a different disease (human eyes).
    // ADR-0043 stage 1: an object-bearing native type is out of the native-guard's
    // scalar domain (it lowered to `None` before ADR-0043), so skip it — reproducing
    // the pre-ADR-0043 `None`-typed behavior exactly.
    if let Some(nt) = param.ty.as_ref().filter(|t| !t.has_instance()) {
        native_guard(nt, &vals, &param.name)?;
    }

    build_tag_edits(path, doc_start, gov, plain, &vals)
}

// ---- `@return` honesty -----------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn plan_return(
    func: &FunctionDecl,
    tags: &[DocTag],
    path: &str,
    tree: &SourceTree,
    scopes: &HashMap<&str, &Scope>,
    blocking: Option<&(&'static str, String)>,
    plan: &mut EditPlan,
    refusals: &mut Vec<Refusal>,
    oracle: &mut CompletenessOracle,
) {
    let (Some(gov), plain) = param_tags(tags, TagKind::Return, None) else {
        return;
    };
    let Some((gov_contract, _consumed)) = tag_contract(gov) else { return };

    // The function's own return-site values (structurally-visible returns; the
    // checker models the same set).
    let Some(scope) = scopes.get(func.name.as_str()) else { return };
    let mut returns: Vec<&ArgValue> = Vec::new();
    collect_returns(&scope.stmts, &mut returns);

    // Lie detection: some *literal* return violates the declared `@return`.
    let lie = returns.iter().any(|v| {
        arg_to_val(v).is_some_and(|val| admits_val(&gov_contract, &val) == Certainty::No)
    });
    if !lie {
        return;
    }

    oracle.enumerated += 1;
    let site = return_site(path, tree, func);
    // A standing project-global obstacle (ADR-0046 §2) refuses every candidate.
    if let Some((reason, detail)) = blocking {
        oracle.refused += 1;
        refusals.push(Refusal::new(site, *reason, detail.clone()));
        return;
    }
    let doc_start = func.docblock_span.map_or(0, |s| s.start);
    match decide_return(func.ret.as_ref(), path, doc_start, gov, plain, scope, &returns) {
        Ok(edits) => account(plan, oracle, refusals, &site, edits),
        Err((reason, detail)) => {
            oracle.refused += 1;
            refusals.push(Refusal::new(site, reason, detail));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn decide_return(
    ret: Option<&NativeType>,
    path: &str,
    doc_start: u32,
    gov: &DocTag,
    plain: Option<&DocTag>,
    scope: &Scope,
    returns: &[&ArgValue],
) -> Result<Vec<Edit>, (&'static str, String)> {
    // A body whose returns v1 cannot fully enumerate (a loop / `try` / `switch`
    // may hide a return the trace does not model) cannot be widened soundly.
    if contains_opaque(&scope.stmts) {
        return Err((
            REASON_RETURN_NOT_PROVEN,
            "the body contains a loop/try/switch whose return paths v1 cannot enumerate".to_owned(),
        ));
    }
    // A body that may fall off the end implicitly returns `null`; widening from the
    // explicit returns alone would omit it. Refuse rather than under-widen.
    if !stmts_terminate(&scope.stmts) {
        return Err((
            REASON_RETURN_NOT_PROVEN,
            "not every path returns a value (an implicit `null` return is possible)".to_owned(),
        ));
    }

    let mut vals: Vec<Val> = Vec::with_capacity(returns.len());
    for v in returns {
        let Some(val) = arg_to_val(v) else {
            return Err((
                REASON_RETURN_NOT_PROVEN,
                format!("a `return {}` is not a proven literal", v.render()),
            ));
        };
        vals.push(val);
    }
    dedup(&mut vals);

    // ADR-0043 stage 1: an object-bearing native return type is out of the
    // native-guard's scalar domain — skip it (pre-ADR-0043 `None`-typed behavior).
    if let Some(nt) = ret.filter(|t| !t.has_instance()) {
        native_guard(nt, &vals, "return")?;
    }

    build_tag_edits(path, doc_start, gov, plain, &vals)
}

// ---- Method `@param` / `@return` honesty (ADR-0043 §6) ---------------------

/// The ADR-0041 §1 eligibility gate, shared by method `@param` and `@return`
/// honesty: `Ok(())` when the method may host a rewrite, else the reserved
/// `magic-method` / `method-inheritance` refusal.
fn method_eligibility_gate(
    msweep: &MethodSweep,
    key: &(String, String),
    class: &ClassDecl,
    method: &MethodDecl,
) -> Result<(), (&'static str, String)> {
    match msweep.eligibility.get(key) {
        Some(MethodEligibility::Eligible) => Ok(()),
        Some(MethodEligibility::Magic) => Err((
            REASON_MAGIC_METHOD,
            format!(
                "`{}::{}` is a magic method; it is invoked by the runtime with no ordinary call site, so it is never a candidate",
                class.name, method.name
            ),
        )),
        Some(MethodEligibility::Inheritance(detail)) => {
            Err((REASON_METHOD_INHERITANCE, detail.clone()))
        }
        None => Err((
            REASON_METHOD_INHERITANCE,
            "method eligibility could not be determined".to_owned(),
        )),
    }
}

#[allow(clippy::too_many_arguments)]
fn plan_method_params(
    class: &ClassDecl,
    method: &MethodDecl,
    tags: &[DocTag],
    path: &str,
    tree: &SourceTree,
    msweep: &MethodSweep,
    blocking: Option<&(&'static str, String)>,
    plan: &mut EditPlan,
    refusals: &mut Vec<Refusal>,
    oracle: &mut CompletenessOracle,
) {
    let key = (class.fqn.to_ascii_lowercase(), method.name.to_ascii_lowercase());
    let target = msweep.targets.get(&key);
    for (idx, param) in method.params.iter().enumerate() {
        let (Some(gov), plain) = param_tags(tags, TagKind::Param, Some(&param.name)) else {
            continue;
        };
        if assert_targets(tags, &param.name) {
            continue;
        }
        let Some((gov_contract, _consumed)) = tag_contract(gov) else { continue };

        let matches = |p: usize| if param.variadic { p >= idx } else { p == idx };
        let null_ok = param.has_null_default
            || param.ty.as_ref().is_some_and(|t| t.nullable && !t.has_instance());
        let mut lie = false;
        if let Some(t) = target {
            for obs in t.observed.iter().filter(|o| matches(o.param_index)) {
                if let Some(v) = arg_to_val(&obs.value) {
                    if matches!(v, Val::Null) && null_ok {
                        continue;
                    }
                    if admits_val(&gov_contract, &v) == Certainty::No {
                        lie = true;
                    }
                }
            }
        }
        if !lie {
            continue;
        }

        oracle.enumerated += 1;
        let site = method_param_site(path, tree, class, method, param);
        if let Some((reason, detail)) = blocking {
            oracle.refused += 1;
            refusals.push(Refusal::new(site, *reason, detail.clone()));
            continue;
        }
        if let Err((reason, detail)) = method_eligibility_gate(msweep, &key, class, method) {
            oracle.refused += 1;
            refusals.push(Refusal::new(site, reason, detail));
            continue;
        }
        let doc_start = method.docblock_span.map_or(0, |s| s.start);
        match decide_method_param(method, param, idx, path, doc_start, gov, plain, &key, msweep) {
            Ok(edits) => account(plan, oracle, refusals, &site, edits),
            Err((reason, detail)) => {
                oracle.refused += 1;
                refusals.push(Refusal::new(site, reason, detail));
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn decide_method_param(
    method: &MethodDecl,
    param: &Param,
    idx: usize,
    path: &str,
    doc_start: u32,
    gov: &DocTag,
    plain: Option<&DocTag>,
    key: &(String, String),
    msweep: &MethodSweep,
) -> Result<Vec<Edit>, (&'static str, String)> {
    check_method_caller_enumerability(&method.name, msweep)?;
    let target = msweep.targets.get(key);
    if target.is_some_and(|t| t.named_or_spread) {
        return Err((
            REASON_NAMED_OR_SPREAD,
            "a call reaching this method used named or spread arguments".to_owned(),
        ));
    }

    let matches = |p: usize| if param.variadic { p >= idx } else { p == idx };
    let mut vals: Vec<Val> = Vec::new();
    if let Some(t) = target {
        for obs in t.observed.iter().filter(|o| matches(o.param_index)) {
            let Some(v) = arg_to_val(&obs.value) else {
                return Err((
                    REASON_ARG_NOT_PROVEN,
                    format!(
                        "call at {}:{}:{} passes `{}`, not a proven literal",
                        obs.caller_path,
                        obs.line,
                        obs.column,
                        obs.value.render()
                    ),
                ));
            };
            vals.push(v);
        }
    }
    dedup(&mut vals);

    if let Some(nt) = param.ty.as_ref().filter(|t| !t.has_instance()) {
        native_guard(nt, &vals, &param.name)?;
    }
    build_tag_edits(path, doc_start, gov, plain, &vals)
}

#[allow(clippy::too_many_arguments)]
fn plan_method_return(
    class: &ClassDecl,
    method: &MethodDecl,
    tags: &[DocTag],
    path: &str,
    tree: &SourceTree,
    method_scopes: &HashMap<(String, String), &Scope>,
    msweep: &MethodSweep,
    blocking: Option<&(&'static str, String)>,
    plan: &mut EditPlan,
    refusals: &mut Vec<Refusal>,
    oracle: &mut CompletenessOracle,
) {
    let (Some(gov), plain) = param_tags(tags, TagKind::Return, None) else {
        return;
    };
    let Some((gov_contract, _consumed)) = tag_contract(gov) else { return };

    let key = (class.fqn.to_ascii_lowercase(), method.name.to_ascii_lowercase());
    let Some(scope) = method_scopes.get(&key) else { return };
    let mut returns: Vec<&ArgValue> = Vec::new();
    collect_returns(&scope.stmts, &mut returns);

    let lie = returns.iter().any(|v| {
        arg_to_val(v).is_some_and(|val| admits_val(&gov_contract, &val) == Certainty::No)
    });
    if !lie {
        return;
    }

    oracle.enumerated += 1;
    let site = method_return_site(path, tree, class, method);
    if let Some((reason, detail)) = blocking {
        oracle.refused += 1;
        refusals.push(Refusal::new(site, *reason, detail.clone()));
        return;
    }
    if let Err((reason, detail)) = method_eligibility_gate(msweep, &key, class, method) {
        oracle.refused += 1;
        refusals.push(Refusal::new(site, reason, detail));
        return;
    }
    let doc_start = method.docblock_span.map_or(0, |s| s.start);
    match decide_return(method.ret.as_ref(), path, doc_start, gov, plain, scope, &returns) {
        Ok(edits) => account(plan, oracle, refusals, &site, edits),
        Err((reason, detail)) => {
            oracle.refused += 1;
            refusals.push(Refusal::new(site, reason, detail));
        }
    }
}

/// Map each method to its scope, keyed `(class_fqn, method)` (both lowercased) —
/// for method `@return` return-site scans.
fn scopes_by_method(tree: &SourceTree) -> HashMap<(String, String), &Scope> {
    let mut map = HashMap::new();
    for scope in tree.scopes() {
        if let ScopeOwner::Method { class, method } = &scope.owner {
            map.insert((class.to_ascii_lowercase(), method.to_ascii_lowercase()), scope);
        }
    }
    map
}

// ---- Shared decision mechanics --------------------------------------------

/// Build the tag-type replacement edit(s): rewrite the governing tag's type-text
/// span to the rendered join; also rewrite a plain sibling when it too fails to
/// admit the join (ADR-0041 precedence reconciliation).
fn build_tag_edits(
    path: &str,
    doc_start: u32,
    gov: &DocTag,
    plain: Option<&DocTag>,
    vals: &[Val],
) -> Result<Vec<Edit>, (&'static str, String)> {
    let rendered = render_value_domain(vals).ok_or((
        REASON_TYPE_NOT_RENDERABLE,
        "the proven value set has no faithful phpdoc spelling".to_owned(),
    ))?;

    let mut edits = Vec::with_capacity(2);
    edits.push(type_edit(path, doc_start, gov, &rendered)?);

    if let Some(pl) = plain {
        // Leave the plain sibling untouched only if it still admits the proven
        // join with Certainty::Yes; otherwise it too is lying — rewrite both.
        let plain_admits = tag_contract(pl).is_some_and(|(c, _)| admits_all(&c, vals));
        if !plain_admits {
            edits.push(type_edit(path, doc_start, pl, &rendered)?);
        }
    }
    Ok(edits)
}

/// The file byte span of a tag's *type text* (only the type prefix, never the
/// `@return` description tail), replaced by `rendered`. Absolute span =
/// `docblock_span.start` + `DocTag.type_span.start` + the parsed type's length.
fn type_edit(
    path: &str,
    doc_start: u32,
    tag: &DocTag,
    rendered: &str,
) -> Result<Edit, (&'static str, String)> {
    let (_, consumed) = tag_contract(tag).ok_or((
        REASON_TYPE_NOT_RENDERABLE,
        "internal: tag type no longer parses".to_owned(),
    ))?;
    let start = doc_start + tag.type_span.start;
    let end = start + consumed;
    Ok(Edit {
        path: path.to_owned(),
        span: ByteSpan::new(start, end),
        replacement: rendered.to_owned(),
    })
}

/// Whether the native type admits every proven value with [`Certainty::Yes`];
/// otherwise the native hint contradicts the proof (a different disease).
fn native_guard(nt: &NativeType, vals: &[Val], what: &str) -> Result<(), (&'static str, String)> {
    let contract = native_contract(nt);
    if admits_all(&contract, vals) {
        Ok(())
    } else {
        Err((
            REASON_NATIVE_CONTRADICTS,
            format!(
                "the native hint `{}` on {what} does not admit the proven value(s); this is a runtime contract violation, not a docblock error",
                nt.render()
            ),
        ))
    }
}

/// Add the decided edits to the plan, updating the oracle. An overlap (an internal
/// invariant break, never a panic) is surfaced as a refusal.
fn account(
    plan: &mut EditPlan,
    oracle: &mut CompletenessOracle,
    refusals: &mut Vec<Refusal>,
    site: &SiteRef,
    edits: Vec<Edit>,
) {
    let mut staged = EditPlan::new();
    for e in edits {
        if staged.add_edit(e).is_err() {
            oracle.refused += 1;
            refusals.push(Refusal::new(
                site.clone(),
                REASON_TYPE_NOT_RENDERABLE,
                "internal: honesty edits overlapped; skipped".to_owned(),
            ));
            return;
        }
    }
    let mut ok = true;
    for e in staged.edits {
        if plan.add_edit(e).is_err() {
            ok = false;
            break;
        }
    }
    if ok {
        oracle.transformed += 1;
    } else {
        oracle.refused += 1;
        refusals.push(Refusal::new(
            site.clone(),
            REASON_TYPE_NOT_RENDERABLE,
            "internal: honesty edits overlapped another edit; skipped".to_owned(),
        ));
    }
}

// ---- Tag helpers -----------------------------------------------------------

/// The governing tag of `kind` (and its plain sibling when a `@phpstan-`/`@psalm-`
/// prefixed tag governs). For `@param`, `name` selects the parameter; for
/// `@return`, pass `None`.
///
/// Returns `(governing, other_plain)`: the governing tag the checker enforces, and
/// the plain sibling *only when* the governing one is prefixed (so both may need
/// reconciling). When the plain tag itself governs, `other_plain` is `None`.
fn param_tags<'a>(
    tags: &'a [DocTag],
    kind: TagKind,
    name: Option<&str>,
) -> (Option<&'a DocTag>, Option<&'a DocTag>) {
    let want = name.map(|n| format!("${n}"));
    let matching: Vec<&DocTag> = tags
        .iter()
        .filter(|t| {
            std::mem::discriminant(&t.kind) == std::mem::discriminant(&kind)
                && match &want {
                    Some(w) => t.var_name.as_deref() == Some(w.as_str()),
                    None => t.var_name.is_none(),
                }
        })
        .collect();
    let prefixed = matching.iter().find(|t| t.prefixed).copied();
    let plain = matching.iter().find(|t| !t.prefixed).copied();
    match prefixed {
        Some(p) => (Some(p), plain),
        None => (plain, None),
    }
}

/// Whether any assertion tag targets parameter `name` (its `@param` is then a
/// post-condition, exempt from the mismatch check — ADR-0030).
fn assert_targets(tags: &[DocTag], name: &str) -> bool {
    let want = format!("${name}");
    tags.iter().any(|t| {
        matches!(t.kind, TagKind::Assert { .. })
            && !t.assert_property_target
            && t.var_name.as_deref() == Some(want.as_str())
    })
}

/// Parse a tag's declared type, returning its lowered contract and the byte length
/// of the type prefix within the tag's `type_text` (so the edit replaces only the
/// type, never a `@return` description). `None` when the type does not parse.
fn tag_contract(tag: &DocTag) -> Option<(ContractTy, u32)> {
    let parsed = parse_type(&tag.type_text).ok()?;
    Some((lower(&parsed.ty), parsed.consumed))
}

fn dedup(vals: &mut Vec<Val>) {
    vals.sort();
    vals.dedup();
}

// ---- Return-site trace walking --------------------------------------------

/// Map each free-function written name to its scope (for `@return` return scans).
fn scopes_by_function(tree: &SourceTree) -> HashMap<&str, &Scope> {
    let mut map = HashMap::new();
    for scope in tree.scopes() {
        if let ScopeOwner::Function(name) = &scope.owner {
            map.insert(name.as_str(), scope);
        }
    }
    map
}

/// Collect every structurally-visible `return <value>` in a statement list,
/// recursing into `if`/`match` sub-traces (the same visibility the checker's trace
/// has — returns inside an `Opaque` loop/try are not modeled here or there).
fn collect_returns<'a>(stmts: &'a [Stmt], out: &mut Vec<&'a ArgValue>) {
    for s in stmts {
        match &s.kind {
            StmtKind::Return { value, .. } => out.push(value),
            StmtKind::If { then_trace, elseifs, else_trace, .. } => {
                collect_returns(then_trace, out);
                for (_, branch) in elseifs {
                    collect_returns(branch, out);
                }
                if let Some(e) = else_trace {
                    collect_returns(e, out);
                }
            }
            StmtKind::Match { arms, default, .. } => {
                for arm in arms {
                    collect_returns(&arm.trace, out);
                }
                if let Some(d) = default {
                    collect_returns(d, out);
                }
            }
            _ => {}
        }
    }
}

/// Whether the statement list contains a construct whose internal control flow —
/// and thus any return inside it — the trace does not model (`Opaque`/`Barrier`).
/// Recurses into modeled `if`/`match` sub-traces.
fn contains_opaque(stmts: &[Stmt]) -> bool {
    stmts.iter().any(|s| match &s.kind {
        StmtKind::Opaque { .. } | StmtKind::Barrier => true,
        StmtKind::If { then_trace, elseifs, else_trace, .. } => {
            contains_opaque(then_trace)
                || elseifs.iter().any(|(_, b)| contains_opaque(b))
                || else_trace.as_ref().is_some_and(|e| contains_opaque(e))
        }
        StmtKind::Match { arms, default, .. } => {
            arms.iter().any(|a| contains_opaque(&a.trace))
                || default.as_ref().is_some_and(|d| contains_opaque(d))
        }
        _ => false,
    })
}

/// Whether control provably cannot fall off the end of this statement list: some
/// statement is a guaranteed terminator (`return`/`throw`/`exit`, or an
/// `if`/`match` whose every branch — including a present `else`/`default` —
/// terminates).
fn stmts_terminate(stmts: &[Stmt]) -> bool {
    stmts.iter().any(stmt_terminates)
}

fn stmt_terminates(s: &Stmt) -> bool {
    match &s.kind {
        StmtKind::Return { .. } | StmtKind::Throw { .. } | StmtKind::Exit { .. } => true,
        StmtKind::If { then_trace, elseifs, else_trace, .. } => {
            else_trace.as_ref().is_some_and(|e| stmts_terminate(e))
                && stmts_terminate(then_trace)
                && elseifs.iter().all(|(_, b)| stmts_terminate(b))
        }
        StmtKind::Match { arms, default, .. } => {
            default.as_ref().is_some_and(|d| stmts_terminate(d))
                && arms.iter().all(|a| stmts_terminate(&a.trace))
        }
        _ => false,
    }
}
