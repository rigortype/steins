//! Transform #1 — phpdoc→native parameter promotion (ADR-0034 point 4 /
//! ADR-0037).
//!
//! Where `@param int $x` documents a parameter that carries **no native hint in
//! source**, and call-site value propagation proves *every* project call site
//! flows a value the native type admits, this transform adds the native
//! declaration (`int $x`) and deletes the now-redundant tag. The precondition —
//! *all callers proven* — is structurally unavailable to modular tools (PHPStan,
//! Rector), which is exactly why it belongs here (ADR-0034).
//!
//! ## Scope
//! - **Free functions and methods** (ADR-0043 §6). Free-function candidates are
//!   proven against [`sweep_free_functions`]; method candidates against
//!   [`sweep_methods`], keyed on the `Sym::Method` identity `(class_fqn, method)`.
//!   A method is a candidate only when the ADR-0041 §1 **eligibility split**
//!   admits it — private, or final, or a method of a final class, with no
//!   inheritance involvement (not overriding an ancestor, not abstract, not an
//!   interface method, its class not trait-using). Every non-eligible method
//!   refuses `method-inheritance`; a magic method refuses `magic-method` (ADR-0046
//!   §3). By construction of the split, narrowing an eligible method's parameter
//!   cannot break Liskov substitution: a private method is invisible to subtype
//!   dispatch, and a final method / final-class method cannot be overridden — so
//!   no supertype caller can reach a narrowed body it would violate.
//! - Promotable phpdoc types are those representable as a
//!   [`steins_syntax::NativeType`]: the four scalars, `true`/`false` literals,
//!   `?T`, and unions of those (plus a `null` member). A finer phpdoc type
//!   (`positive-int`, `non-empty-string`, `int<0, max>`, a class, an array, …) is
//!   not representable and is refused `type-not-natively-representable`.
//! - Only **literal** arguments prove a call site in v1 (folding-backed and
//!   `$var`-flow proofs are deferred): a non-literal argument at any call site is
//!   refused `argument-not-proven`.
//! - A candidate whose enumerated caller set is **empty** — no call site anywhere
//!   resolved to it — refuses `no-observed-callers` rather than promote on a
//!   vacuous "all callers proven" (ADR-0047 §4 / ADR-0037; amends ADR-0041 §3):
//!   zero callers is zero evidence, and is exactly the shape a framework's
//!   convention-reflection dispatch hides behind (a test runner invoking a
//!   data-provider method with no visible call site).
//!
//! Every enumerated site is accounted for as transformed-or-refused (the
//! completeness oracle, ADR-0034 point 3b). Refusals carry a stable named reason
//! and human detail so an agent can read them and continue (ADR-0034 point 2).

use steins_contract::{ContractTy, admits_val};
use steins_db::{Db, Project, SourceFile, parse};
use steins_domain::Certainty;
use steins_infer::is_vendor_path;
use steins_infer::promote::{
    FreeFnSweep, MethodEligibility, MethodSweep, TargetSweep, sweep_free_functions, sweep_methods,
};
use steins_phpdoc::ast::{ConstExpr, Type as PType, TypeKind};
use steins_phpdoc::docblock::DocTag;
use steins_phpdoc::{TagKind, parse_type, scan_docblock};
use steins_syntax::{ClassDecl, FunctionDecl, MethodDecl, NativeType, Param, ScalarType, SourceTree, TypeMember};

use crate::common::{
    arg_to_val, check_caller_enumerability, check_method_caller_enumerability, count_fqns,
    has_source_hint, method_param_site, native_contract, param_site,
};
use crate::obstacles::{self, VouchSet};
use crate::plan::{ByteSpan, Edit, EditPlan};
use crate::transform::{CompletenessOracle, Refusal, Transform, TransformReport};

// ---- Stable refusal reason names (ADR-0034 point 2) -----------------------

// The caller-enumerability reasons (`dynamic-call-present`,
// `function-referenced-as-value`, `resolution-ambiguous`, `named-or-spread-args`,
// `argument-not-proven`) are shared with honesty and live in `crate::common`;
// re-exported here so the public `steins_edit::promote::REASON_*` paths still
// resolve.
pub use crate::common::{
    REASON_AMBIGUOUS, REASON_ARG_NOT_PROVEN, REASON_DYNAMIC_CALL, REASON_DYNAMIC_INCLUDE,
    REASON_EVAL_PRESENT, REASON_MAGIC_METHOD, REASON_METHOD_INHERITANCE, REASON_NAMED_OR_SPREAD,
    REASON_NO_OBSERVED_CALLERS, REASON_REFERENCED_AS_VALUE,
};

/// The phpdoc type has no [`NativeType`] rendering and is not a scalar
/// refinement either (arrays, generics, class names, callables, shapes).
pub const REASON_NOT_REPRESENTABLE: &str = "type-not-natively-representable";
/// The phpdoc type is a *refinement* of a native scalar (`positive-int`,
/// `non-empty-string`, `int<0, max>`, a literal `5`): strictly finer than its
/// native rendering, so v1 refuses rather than promote-and-keep (ADR-0041 pt 2).
pub const REASON_FINER_THAN_NATIVE: &str = "phpdoc-finer-than-native";
/// A `$x = null` default makes the parameter implicitly nullable, but the native
/// type is not nullable — promoting would emit PHP-8.4-deprecated code.
pub const REASON_IMPLICIT_NULLABLE: &str = "implicit-nullable-default";
/// The parameter has a non-null default value that the native type does not
/// provably admit (`int $x = 'str'` is a compile-time fatal; `int $x = PHP_INT_MAX`
/// is valid but unprovable in v1). Refusing keeps the emitted declaration legal.
pub const REASON_DEFAULT_INCOMPATIBLE: &str = "default-not-admitted-by-native";

/// The phpdoc→native promotion transform (ADR-0034 point 4).
#[derive(Debug, Clone, Copy, Default)]
pub struct PhpdocToNative;

impl Transform for PhpdocToNative {
    fn id(&self) -> &'static str {
        "phpdoc-to-native"
    }
}

/// Plan the phpdoc→native promotion over `project`. Pure planning: no files are
/// written and no diagnostics are re-checked here — the caller (CLI) drives the
/// dry-run diff, the dual-verification post-check, and any `--apply` write
/// (ADR-0034 point 3).
///
/// `vouches` are the user-vouched dynamic-code sites (`steins.toml`); pass
/// [`VouchSet::empty`] when none. A standing (unvouched) `eval` / dynamic-include
/// obstacle (ADR-0046 §2) makes "all callers proven" unknowable project-wide, so
/// *every* candidate refuses while one remains.
#[must_use]
pub fn plan_phpdoc_to_native(db: &dyn Db, project: Project, vouches: &VouchSet) -> TransformReport {
    let sweep = sweep_free_functions(db, project);
    // The class-world reverse sweep (ADR-0043 §6): method targets, taints, and the
    // ADR-0041 §1 eligibility verdicts. Free-function behavior is unaffected.
    let msweep = sweep_methods(db, project);

    // Project-global dynamic-code obstacles (ADR-0046 §2): recorded once, and — if
    // any stands unvouched — every candidate refuses with its reason.
    let dynamism = obstacles::detect(db, project, vouches);
    let blocking = dynamism.blocking_reason();

    let files: Vec<SourceFile> = project.files(db).to_vec();

    // Count each FQN across the project so a duplicate definition (which makes
    // resolution ambiguous) refuses rather than promotes on thin evidence.
    let fqn_counts = count_fqns(db, &files);

    let mut plan = EditPlan::new();
    let mut refusals: Vec<Refusal> = Vec::new();
    let mut oracle = CompletenessOracle::default();

    for &file in &files {
        let path = file.path(db);
        // Vendor files participate in the reverse SWEEP (as callers and as
        // definitions) but are never transform CANDIDATES: a rewrite into `vendor/`
        // is outside the tool's write contract — composer overwrites it, and vendor
        // diagnostics are off by default (ADR-0015). Candidate enumeration is
        // project-only; caller/obstacle enumeration (above) still spans vendor.
        if is_vendor_path(path) {
            continue;
        }
        let tree = parse(db, file);
        let source = file.text(db);
        for func in tree.functions() {
            plan_function(
                func, path, source, tree, &sweep, &fqn_counts, blocking.as_ref(), &mut plan,
                &mut refusals, &mut oracle,
            );
        }
        for class in tree.classes() {
            for method in &class.methods {
                plan_method(
                    class, method, path, source, tree, &msweep, blocking.as_ref(), &mut plan,
                    &mut refusals, &mut oracle,
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

/// Plan promotions for one free function, appending edits/refusals and updating
/// the oracle for each *enumerated* parameter.
#[allow(clippy::too_many_arguments)]
fn plan_function(
    func: &FunctionDecl,
    path: &str,
    source: &str,
    tree: &SourceTree,
    sweep: &FreeFnSweep,
    fqn_counts: &std::collections::HashMap<String, usize>,
    blocking: Option<&(&'static str, String)>,
    plan: &mut EditPlan,
    refusals: &mut Vec<Refusal>,
    oracle: &mut CompletenessOracle,
) {
    let tags = func.docblock.as_deref().map(scan_docblock).unwrap_or_default();

    for (idx, param) in func.params.iter().enumerate() {
        // Domain gate 1: the parameter must have no native hint in source.
        if param.ty.is_some() || has_source_hint(source, param) {
            continue;
        }
        // Domain gate 2: there must be a promotable `@param` tag for it.
        let Some(tag) = param_tag(&tags, &param.name) else { continue };

        // This parameter is an *enumerated* site — it must end transformed or
        // refused (the completeness oracle, ADR-0034 point 3b).
        oracle.enumerated += 1;
        let site = param_site(path, tree, func, param);

        // A standing project-global obstacle (ADR-0046 §2) refuses every candidate
        // before any per-site judgment: "all callers proven" is unknowable.
        if let Some((reason, detail)) = blocking {
            oracle.refused += 1;
            refusals.push(Refusal::new(site, *reason, detail.clone()));
            continue;
        }

        match decide(func, param, idx, tag, source, sweep, fqn_counts) {
            Ok(native) => {
                // Insert `"<native> "` at the parameter's start (covers `&`/`...`),
                // and delete the now-redundant `@param` line.
                let insert = Edit {
                    path: path.to_owned(),
                    span: ByteSpan::at(param.span.start),
                    replacement: format!("{} ", native.render()),
                };
                let doc_start = func.docblock_span.map_or(0, |s| s.start);
                let doc_text = func.docblock.as_deref().unwrap_or("");
                let delete = Edit {
                    path: path.to_owned(),
                    span: tag_deletion(source, doc_start, doc_text, tag, tags.len()),
                    replacement: String::new(),
                };
                // Overlap rejection is the plan's job; a rejection here is an
                // internal invariant break, surfaced as a refusal (never a panic).
                if plan.add_edit(insert).and_then(|()| plan.add_edit(delete)).is_ok() {
                    oracle.transformed += 1;
                } else {
                    oracle.refused += 1;
                    refusals.push(Refusal::new(
                        site,
                        REASON_ARG_NOT_PROVEN,
                        "internal: promotion edits overlapped another edit; skipped",
                    ));
                }
            }
            Err((reason, detail)) => {
                oracle.refused += 1;
                refusals.push(Refusal::new(site, reason, detail));
            }
        }
    }
}

/// Decide a single **free-function** candidate: `Ok(native)` to promote,
/// `Err((reason, detail))` to refuse.
fn decide(
    func: &FunctionDecl,
    param: &Param,
    idx: usize,
    tag: &DocTag,
    source: &str,
    sweep: &FreeFnSweep,
    fqn_counts: &std::collections::HashMap<String, usize>,
) -> Result<NativeType, (&'static str, String)> {
    // (a/b/b2) The native type mapping + default-value gates (shared with methods).
    let native = native_of_candidate(param, tag)?;

    // (c) Project-wide obstacles that make "all callers" unknowable (shared with
    // honesty's `@param` widening).
    check_caller_enumerability(func, sweep, fqn_counts)?;

    // (d) Prove every observed call-site argument for this parameter position.
    let contract = native_contract(&native);
    let _ = source; // (source already consulted in the domain gate)
    match sweep.targets.get(&func.fqn) {
        Some(target) => prove_target(target, idx, param.variadic, &native, &contract)?,
        // No target entry means no observed callers anywhere in the enumerated
        // universe. This is NOT vacuous proof (ADR-0047 §4 / ADR-0037): an
        // "all-callers-proven" claim over zero callers is zero evidence, and
        // exactly the shape a framework's convention-reflection dispatch hides
        // behind (a test runner invoking this function with no visible call
        // site). Refuse rather than promote on nothing.
        None => {
            return Err((
                REASON_NO_OBSERVED_CALLERS,
                format!(
                    "no call site was observed for `{}`; a vacuous all-callers proof is no evidence — callers may exist via reflection or dynamic dispatch",
                    func.name
                ),
            ));
        }
    }

    Ok(native)
}

/// The native-type mapping + default-value gates shared by free-function and method
/// candidates (ADR-0041 points 2/3). `Ok(native)` to keep going, `Err` to refuse.
fn native_of_candidate(param: &Param, tag: &DocTag) -> Result<NativeType, (&'static str, String)> {
    // (a) Representability: the phpdoc type must map to a NativeType. A type that
    // is strictly *finer* than a native scalar (`positive-int`, `non-empty-string`,
    // `int<0, max>`) refuses distinctly from a genuinely non-scalar type (ADR-0041
    // point 2).
    let parsed = parse_type(&tag.type_text).map_err(|_| {
        (REASON_NOT_REPRESENTABLE, format!("phpdoc type `{}` did not parse", tag.type_text))
    })?;
    let native = match parsed.at_end.then(|| phpdoc_to_native(&parsed.ty)).flatten() {
        Some(nt) => nt,
        None if parsed.at_end && is_finer_than_native(&parsed.ty) => {
            return Err((
                REASON_FINER_THAN_NATIVE,
                format!(
                    "phpdoc type `{}` is finer than its native rendering; v1 refuses rather than promote-and-keep",
                    tag.type_text
                ),
            ));
        }
        None => {
            return Err((
                REASON_NOT_REPRESENTABLE,
                format!("phpdoc type `{}` has no native scalar/union rendering", tag.type_text),
            ));
        }
    };

    // (b) A literal-null default makes the parameter implicitly nullable; a
    // non-nullable native type would emit PHP-8.4-deprecated code.
    if param.has_null_default && !native.nullable {
        return Err((
            REASON_IMPLICIT_NULLABLE,
            format!(
                "parameter ${} has a `= null` default (implicitly nullable), but `{}` is not nullable",
                param.name,
                native.render()
            ),
        ));
    }

    // (b2) Any other default value must be provably admitted by the native type,
    // or the emitted declaration is a compile-time fatal (`int $x = 'str'`,
    // `int $x = 3.0`, `int $x = []`). A non-representable default (a constant,
    // `self::X`, an expression) is unprovable in v1 and refused conservatively —
    // `int $x = PHP_INT_MAX` is legal but we cannot show it here.
    if param.has_default && !param.has_null_default {
        let contract = native_contract(&native);
        let admitted = param
            .default
            .as_ref()
            .and_then(arg_to_val)
            .is_some_and(|val| admits_val(&contract, &val) == Certainty::Yes);
        if !admitted {
            return Err((
                REASON_DEFAULT_INCOMPATIBLE,
                format!(
                    "parameter ${} has a default value the native type `{}` does not provably admit; promoting would emit a declaration PHP rejects",
                    param.name,
                    native.render()
                ),
            ));
        }
    }

    Ok(native)
}

// ---- Method promotion (ADR-0043 §6) ---------------------------------------

/// Plan promotions for one method (the class-world analogue of [`plan_function`]),
/// applying the ADR-0041 §1 eligibility split before any per-site judgment.
#[allow(clippy::too_many_arguments)]
fn plan_method(
    class: &ClassDecl,
    method: &MethodDecl,
    path: &str,
    source: &str,
    tree: &SourceTree,
    msweep: &MethodSweep,
    blocking: Option<&(&'static str, String)>,
    plan: &mut EditPlan,
    refusals: &mut Vec<Refusal>,
    oracle: &mut CompletenessOracle,
) {
    let tags = method.docblock.as_deref().map(scan_docblock).unwrap_or_default();
    let key = (class.fqn.to_ascii_lowercase(), method.name.to_ascii_lowercase());

    for (idx, param) in method.params.iter().enumerate() {
        // Domain gate 1: the parameter must have no native hint in source.
        if param.ty.is_some() || has_source_hint(source, param) {
            continue;
        }
        // Domain gate 2: there must be a promotable `@param` tag for it.
        let Some(tag) = param_tag(&tags, &param.name) else { continue };

        // An *enumerated* site — it must end transformed or refused (the oracle).
        oracle.enumerated += 1;
        let site = method_param_site(path, tree, class, method, param);

        // A standing project-global obstacle (ADR-0046 §2) refuses every candidate
        // before any per-site judgment (an `eval` can call methods too).
        if let Some((reason, detail)) = blocking {
            oracle.refused += 1;
            refusals.push(Refusal::new(site, *reason, detail.clone()));
            continue;
        }

        // The ADR-0041 §1 eligibility split (magic / inheritance) precedes the
        // type/caller judgment: a non-eligible method is never a candidate.
        match msweep.eligibility.get(&key) {
            Some(MethodEligibility::Magic) => {
                oracle.refused += 1;
                refusals.push(Refusal::new(
                    site,
                    REASON_MAGIC_METHOD,
                    format!(
                        "`{}::{}` is a magic method; it is invoked by the runtime with no ordinary call site, so it is never a candidate",
                        class.name, method.name
                    ),
                ));
                continue;
            }
            Some(MethodEligibility::Inheritance(detail)) => {
                oracle.refused += 1;
                refusals.push(Refusal::new(site, REASON_METHOD_INHERITANCE, detail.clone()));
                continue;
            }
            // `Eligible` (or, defensively, an absent verdict → treat as ineligible).
            Some(MethodEligibility::Eligible) => {}
            None => {
                oracle.refused += 1;
                refusals.push(Refusal::new(
                    site,
                    REASON_METHOD_INHERITANCE,
                    "method eligibility could not be determined".to_owned(),
                ));
                continue;
            }
        }

        match decide_method(method, param, idx, tag, &key, msweep) {
            Ok(native) => {
                let insert = Edit {
                    path: path.to_owned(),
                    span: ByteSpan::at(param.span.start),
                    replacement: format!("{} ", native.render()),
                };
                let doc_start = method.docblock_span.map_or(0, |s| s.start);
                let doc_text = method.docblock.as_deref().unwrap_or("");
                let delete = Edit {
                    path: path.to_owned(),
                    span: tag_deletion(source, doc_start, doc_text, tag, tags.len()),
                    replacement: String::new(),
                };
                if plan.add_edit(insert).and_then(|()| plan.add_edit(delete)).is_ok() {
                    oracle.transformed += 1;
                } else {
                    oracle.refused += 1;
                    refusals.push(Refusal::new(
                        site,
                        REASON_ARG_NOT_PROVEN,
                        "internal: promotion edits overlapped another edit; skipped",
                    ));
                }
            }
            Err((reason, detail)) => {
                oracle.refused += 1;
                refusals.push(Refusal::new(site, reason, detail));
            }
        }
    }
}

/// Decide a single **method** candidate: the type/default gates, then the
/// method-caller-enumerability obstacles and the observed-argument proof.
fn decide_method(
    method: &MethodDecl,
    param: &Param,
    idx: usize,
    tag: &DocTag,
    key: &(String, String),
    msweep: &MethodSweep,
) -> Result<NativeType, (&'static str, String)> {
    let native = native_of_candidate(param, tag)?;

    // Project-wide obstacles that make "all callers" unknowable for this method
    // name (dynamic method call, callable-value reference, unresolved receiver).
    check_method_caller_enumerability(&method.name, msweep)?;

    // Prove every observed call-site argument for this parameter position.
    let contract = native_contract(&native);
    match msweep.targets.get(key) {
        Some(target) => prove_target(target, idx, param.variadic, &native, &contract)?,
        // No target entry → no observed callers anywhere in the enumerated
        // universe (ADR-0047 §4 / ADR-0037): the motivating hole is exactly this
        // shape — a final test class's data-provider method invoked only via
        // framework reflection, invisible to the sweep. Refuse rather than
        // promote on a vacuous zero-caller proof.
        None => {
            return Err((
                REASON_NO_OBSERVED_CALLERS,
                format!(
                    "no call site was observed for `{}::{}`; a vacuous all-callers proof is no evidence — callers may exist via reflection or dynamic dispatch",
                    key.0, method.name
                ),
            ));
        }
    }
    Ok(native)
}

/// Prove that every observed argument at parameter position `idx` admits the
/// native contract with [`Certainty::Yes`]; also honor the named/spread flag.
fn prove_target(
    target: &TargetSweep,
    idx: usize,
    variadic: bool,
    native: &NativeType,
    contract: &ContractTy,
) -> Result<(), (&'static str, String)> {
    if target.named_or_spread {
        return Err((
            REASON_NAMED_OR_SPREAD,
            "a call reaching this function used named or spread arguments".to_owned(),
        ));
    }
    // A variadic parameter at position `idx` collects *every* positional argument
    // from `idx` onward, so each of them must be proven (not just the one at
    // `idx`) — otherwise a bad later argument (`f(1, 'str')`) flows in unchecked.
    let matches = |p: usize| if variadic { p >= idx } else { p == idx };
    for obs in target.observed.iter().filter(|o| matches(o.param_index)) {
        let Some(val) = arg_to_val(&obs.value) else {
            return Err((
                REASON_ARG_NOT_PROVEN,
                format!(
                    "call at {}:{}:{} passes `{}`, not a proven literal admitting `{}`",
                    obs.caller_path,
                    obs.line,
                    obs.column,
                    obs.value.render(),
                    native.render()
                ),
            ));
        };
        if admits_val(contract, &val) != Certainty::Yes {
            return Err((
                REASON_ARG_NOT_PROVEN,
                format!(
                    "call at {}:{}:{} passes `{}`, which `{}` does not admit",
                    obs.caller_path,
                    obs.line,
                    obs.column,
                    obs.value.render(),
                    native.render()
                ),
            ));
        }
    }
    Ok(())
}

// ---- Candidate helpers ----------------------------------------------------

/// The `@param` tag documenting `param_name` (without `$`), if any.
fn param_tag<'a>(tags: &'a [DocTag], param_name: &str) -> Option<&'a DocTag> {
    let want = format!("${param_name}");
    tags.iter().find(|t| {
        matches!(t.kind, TagKind::Param) && t.var_name.as_deref() == Some(want.as_str())
    })
}

/// Compute the file byte span to delete for a promoted `@param` tag, leaving a
/// syntactically valid docblock.
///
/// - A tag on a line with no docblock delimiters → delete the whole physical line
///   (plus its trailing newline).
/// - A tag sharing a line with `/**` or `*/` and it is the docblock's only tag →
///   delete the whole docblock.
/// - Otherwise (delimiter line with sibling tags) → delete just the tag text.
fn tag_deletion(
    source: &str,
    doc_start: u32,
    doc_text: &str,
    tag: &DocTag,
    total_tags: usize,
) -> ByteSpan {
    let line = &doc_text[tag.line_span.start as usize..tag.line_span.end as usize];
    let has_delims = line.contains("/**") || line.contains("*/");

    if !has_delims {
        let start = doc_start + tag.line_span.start;
        let mut end = doc_start + tag.line_span.end;
        // Swallow the line's terminating newline so no blank gutter line is left.
        if source.as_bytes().get(end as usize) == Some(&b'\n') {
            end += 1;
        }
        ByteSpan::new(start, end)
    } else if total_tags == 1 {
        ByteSpan::new(doc_start, doc_start + doc_text.len() as u32)
    } else {
        ByteSpan::new(doc_start + tag.tag_span.start, doc_start + tag.tag_span.end)
    }
}

// ---- Type + value mapping -------------------------------------------------

/// Map a parsed phpdoc type to a [`NativeType`], or `None` when it is not
/// representable as a scalar/bool-literal/nullable/scalar-union native type.
fn phpdoc_to_native(ty: &PType) -> Option<NativeType> {
    match &ty.kind {
        TypeKind::Identifier(name) => {
            member_of(name).map(|m| NativeType { members: vec![m], nullable: false })
        }
        TypeKind::Nullable(inner) => {
            let mut nt = phpdoc_to_native(inner)?;
            nt.nullable = true;
            Some(nt)
        }
        TypeKind::Union { types, .. } => {
            let mut members = Vec::new();
            let mut nullable = false;
            for t in types {
                match &t.kind {
                    TypeKind::Identifier(name) if name.eq_ignore_ascii_case("null") => {
                        nullable = true;
                    }
                    TypeKind::Identifier(name) => members.push(member_of(name)?),
                    TypeKind::Nullable(inner) => {
                        // `?T` inside a union: fold its member in and mark nullable.
                        let nt = phpdoc_to_native(inner)?;
                        members.extend(nt.members);
                        nullable = true;
                    }
                    _ => return None,
                }
            }
            if members.is_empty() {
                None // a `null`-only union has no native rendering
            } else {
                Some(NativeType { members, nullable })
            }
        }
        _ => None,
    }
}

/// Whether a non-representable phpdoc type is a *refinement* of a native scalar
/// (`positive-int`, `non-empty-string`, `int<0, max>`, a literal `5` / `'x'`), as
/// opposed to genuinely non-scalar (arrays, classes, generic collections,
/// callables, shapes). Drives the `phpdoc-finer-than-native` vs
/// `type-not-natively-representable` split (ADR-0041 point 2).
fn is_finer_than_native(ty: &PType) -> bool {
    match &ty.kind {
        TypeKind::Identifier(n) => is_refined_scalar_keyword(&n.to_ascii_lowercase()),
        TypeKind::Nullable(inner) => is_finer_than_native(inner),
        // A literal-value type (`5`, `1.5`, `'x'`) is finer than its scalar base.
        TypeKind::Const(c) => {
            matches!(c, ConstExpr::Int(_) | ConstExpr::Float(_) | ConstExpr::Str(_))
        }
        // `int<a, b>` — a bounded-int refinement (`array<…>` etc. are not).
        TypeKind::Generic { base, .. } => base.eq_ignore_ascii_case("int"),
        // A union is "finer" only if every member is representable-or-finer and at
        // least one member is a refinement — else it is genuinely non-representable.
        TypeKind::Union { types, .. } => {
            let mut any_finer = false;
            for t in types {
                if native_member_repr(t) {
                    continue;
                }
                if is_finer_than_native(t) {
                    any_finer = true;
                } else {
                    return false;
                }
            }
            any_finer
        }
        _ => false,
    }
}

/// Whether a union member is a plainly-representable native member (scalar /
/// bool-literal / `null` / nullable-of-such).
fn native_member_repr(ty: &PType) -> bool {
    match &ty.kind {
        TypeKind::Identifier(n) => {
            let n = n.to_ascii_lowercase();
            n == "null" || member_of(&n).is_some()
        }
        TypeKind::Nullable(inner) => native_member_repr(inner),
        _ => false,
    }
}

/// The refined-scalar phpdoc keywords (subtypes of `int`/`string`) — finer than
/// their native base, so promotion refuses `phpdoc-finer-than-native`.
fn is_refined_scalar_keyword(n: &str) -> bool {
    matches!(
        n,
        "positive-int"
            | "negative-int"
            | "non-negative-int"
            | "non-positive-int"
            | "non-empty-string"
            | "non-falsy-string"
            | "truthy-string"
            | "numeric-string"
            | "lowercase-string"
            | "uppercase-string"
            | "non-empty-lowercase-string"
            | "class-string"
            | "interned-string"
            | "literal-string"
            | "callable-string"
            | "trait-string"
            | "enum-string"
            | "html-escaped-string"
    )
}

/// A single native union member from a phpdoc identifier keyword. Only the
/// canonical scalar keywords and `true`/`false` literals map; everything else
/// (aliases, refined scalars, class names) is not natively representable.
fn member_of(name: &str) -> Option<TypeMember> {
    let n = name.to_ascii_lowercase();
    match n.as_str() {
        "int" => Some(TypeMember::Scalar(ScalarType::Int)),
        "float" => Some(TypeMember::Scalar(ScalarType::Float)),
        "string" => Some(TypeMember::Scalar(ScalarType::String)),
        "bool" => Some(TypeMember::Scalar(ScalarType::Bool)),
        "true" => Some(TypeMember::BoolLiteral(true)),
        "false" => Some(TypeMember::BoolLiteral(false)),
        _ => None,
    }
}

