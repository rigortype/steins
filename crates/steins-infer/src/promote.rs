//! The reverse call-site sweep for phpdoc→native parameter promotion
//! (ADR-0034 point 4 / ADR-0037): promotion requires *all callers proven*,
//! structurally unavailable to modular tools. `steins-edit` reuses this
//! engine's name resolution (`Cx::resolve_function`, `Index`) through this
//! seam and owns enumeration, native-type mapping, acceptance
//! (`steins-contract::admits_*`), refusals, and edits.
//!
//! Covers **free-function** targets ([`sweep_methods`] handles methods),
//! recording what makes "all callers reached" unknowable: dynamic calls,
//! value references, unresolved same-name calls.

use std::collections::{HashMap, HashSet};

use steins_db::{Db, Project, SourceFile, parse, project_index};
use steins_syntax::{
    ArgValue, Callee, ClassDecl, ClosureRef, MethodDecl, Scope, ScopeOwner, SourceTree, Stmt,
    StmtKind, Visibility,
};

use crate::{Cx, FileUnit, FnResolution, Index, Store, resolve_call_target};

/// One positional argument observed at a call site resolving uniquely to a target.
#[derive(Debug, Clone)]
pub struct ObservedArg {
    /// The zero-based positional parameter index this argument fills.
    pub param_index: usize,
    pub caller_path: String,
    pub line: u32,
    pub column: u32,
    /// The lowered argument value — the transform proves/admits it.
    pub value: ArgValue,
}

/// A recorded obstacle *site* (ADR-0047 §6), so the partition planner can
/// attribute each obstacle to its region (§2 keys on `path`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepSite {
    pub path: String,
    pub line: u32,
    pub column: u32,
}

impl SweepSite {
    #[must_use]
    pub fn new(path: impl Into<String>, line: u32, column: u32) -> Self {
        Self { path: path.into(), line, column }
    }
}

/// The reverse-sweep facts for one free-function target (keyed by lowercased FQN).
#[derive(Debug, Clone, Default)]
pub struct TargetSweep {
    pub observed: Vec<ObservedArg>,
    /// A call used named/spread args (positional mapping unreliable) —
    /// `named-or-spread-args` refusal trigger.
    pub named_or_spread: bool,
}

/// The whole-project reverse sweep the promotion planner consumes.
#[derive(Debug, Clone, Default)]
pub struct FreeFnSweep {
    pub targets: HashMap<String, TargetSweep>,
    /// Dynamic (`$fn()`) call sites — taint every candidate (`dynamic-call-present`).
    pub dynamic_call_sites: Vec<SweepSite>,
    /// Names seen as callable *values* → sites (`function-referenced-as-value`).
    pub value_referenced_names: HashMap<String, Vec<SweepSite>>,
    /// Names unresolved to a unique function → sites (`resolution-ambiguous`).
    pub unresolved_simple_names: HashMap<String, Vec<SweepSite>>,
}

/// Sweep every call, attributing args to free functions and recording what
/// makes "all callers proven" unknowable.
#[must_use]
pub fn sweep_free_functions(db: &dyn Db, project: Project) -> FreeFnSweep {
    let handles: Vec<SourceFile> = project.files(db).to_vec();
    let units: Vec<FileUnit> =
        handles.iter().map(|&f| FileUnit { path: f.path(db), tree: parse(db, f) }).collect();
    let db_index = project_index(db, project);
    let pos: HashMap<SourceFile, usize> =
        handles.iter().enumerate().map(|(i, &f)| (f, i)).collect();
    let index = Index::from_db(db_index, &pos, &units);

    let mut out = FreeFnSweep::default();
    for fi in 0..units.len() {
        let cx = Cx::new(&units, &index, fi);
        let tree = cx.tree();
        let path = cx.path();
        for call in tree.calls() {
            let cp = tree.position(call.span.start);
            let call_site = SweepSite::new(path, cp.line, cp.column);

            for arg in &call.args {
                collect_value_names(&arg.value, &call_site, &mut out.value_referenced_names);
            }

            // An opaque `call_user_func*` argument could hold ANY free function
            // (issue #6's callable-array gap) — taint broadly.
            if let Callee::Function(name) = &call.receiver
                && is_generic_invoker(name)
                && let Some(callable_arg) = call.args.first()
                && callable_arg_is_opaque(&callable_arg.value)
            {
                out.dynamic_call_sites.push(call_site.clone());
            }

            match &call.receiver {
                Callee::DynamicVar(_) | Callee::Dynamic => {
                    out.dynamic_call_sites.push(call_site.clone());
                }
                Callee::Function(_) => {
                    let Some(cref) = &call.callee_ref else { continue };
                    match cx.resolve_function(cref) {
                        FnResolution::User(site) => {
                            let fqn = cx.fn_decl(site).fqn.clone();
                            let entry = out.targets.entry(fqn).or_default();
                            if call.positional_only {
                                for (i, arg) in call.args.iter().enumerate() {
                                    let p = tree.position(arg.span.start);
                                    entry.observed.push(ObservedArg {
                                        param_index: i,
                                        caller_path: path.to_owned(),
                                        line: p.line,
                                        column: p.column,
                                        value: arg.value.clone(),
                                    });
                                }
                            } else {
                                entry.named_or_spread = true;
                            }
                        }
                        FnResolution::Builtin(_) | FnResolution::Unknown => {
                            out.unresolved_simple_names
                                .entry(cref.simple().to_ascii_lowercase())
                                .or_default()
                                .push(call_site.clone());
                        }
                    }
                }
                Callee::Method { .. } | Callee::Static { .. } | Callee::Construct { .. } => {}
            }
        }

        scan_scope_values(tree, path, &mut out.value_referenced_names);
    }
    out
}

/// Scan every scope for function-name-shaped values escaping through
/// assignment/return, recursing into `if`/`match`.
fn scan_scope_values(tree: &SourceTree, path: &str, map: &mut HashMap<String, Vec<SweepSite>>) {
    for scope in tree.scopes() {
        scan_stmts(&scope.stmts, tree, path, map);
    }
}

fn scan_stmts(
    stmts: &[Stmt],
    tree: &SourceTree,
    path: &str,
    map: &mut HashMap<String, Vec<SweepSite>>,
) {
    for s in stmts {
        match &s.kind {
            StmtKind::Assign { value, .. }
            | StmtKind::PropAssign { value, .. }
            | StmtKind::Return { value, .. } => {
                let p = tree.position(s.span.start);
                collect_value_names(value, &SweepSite::new(path, p.line, p.column), map);
            }
            StmtKind::If { then_trace, elseifs, else_trace, .. } => {
                scan_stmts(then_trace, tree, path, map);
                for (_, branch) in elseifs {
                    scan_stmts(branch, tree, path, map);
                }
                if let Some(e) = else_trace {
                    scan_stmts(e, tree, path, map);
                }
            }
            StmtKind::Match { arms, default, .. } => {
                for arm in arms {
                    scan_stmts(&arm.trace, tree, path, map);
                }
                if let Some(d) = default {
                    scan_stmts(d, tree, path, map);
                }
            }
            _ => {}
        }
    }
}

/// Recursively collect function-name-shaped values into `map` (full spelling +
/// last segment, lowercased), keyed by `site`.
fn collect_value_names(v: &ArgValue, site: &SweepSite, map: &mut HashMap<String, Vec<SweepSite>>) {
    match v {
        // A byte string is no PHP symbol name (ADR-0080 §2.5).
        ArgValue::Str(s) => {
            if let Some(s) = s.as_str() {
                insert_name_forms(s, site, map);
            }
        }
        ArgValue::Closure(ClosureRef::FunctionName(name)) => {
            insert_name_forms(&name.raw, site, map);
            push_name(map, name.simple().to_ascii_lowercase(), site);
        }
        ArgValue::Array(items) => {
            for (_, e) in items {
                collect_value_names(e, site, map);
            }
        }
        _ => {}
    }
}

fn insert_name_forms(raw: &str, site: &SweepSite, map: &mut HashMap<String, Vec<SweepSite>>) {
    let norm = raw.trim_start_matches('\\').to_ascii_lowercase();
    if let Some(pos) = norm.rfind('\\') {
        push_name(map, norm[pos + 1..].to_owned(), site);
    }
    push_name(map, norm, site);
}

/// Record `site` under `name` in a name→sites taint map (ADR-0047 §6).
fn push_name(map: &mut HashMap<String, Vec<SweepSite>>, name: String, site: &SweepSite) {
    map.entry(name).or_default().push(site.clone());
}

/// A `call_user_func*` invoker (ADR-0041 §3): its first arg is itself the callable.
fn is_generic_invoker(name: &str) -> bool {
    name.eq_ignore_ascii_case("call_user_func") || name.eq_ignore_ascii_case("call_user_func_array")
}

/// Whether `v` could hold an arbitrary callable: not a name-shaped literal nor
/// a scalar PHP rejects outright as callable.
fn callable_arg_is_opaque(v: &ArgValue) -> bool {
    !matches!(
        v,
        ArgValue::Str(_)
            | ArgValue::Closure(_)
            | ArgValue::Array(_)
            | ArgValue::Int(_)
            | ArgValue::Float(_)
            | ArgValue::Bool(_)
            | ArgValue::Null
    )
}

// The method-call reverse sweep (ADR-0043 §6), keyed by `(class_fqn, method_name)`.
// An unresolved call taints the *method name* project-wide. Precision limit: no
// per-scope object heap, so `$var->m()` always taints (can't prove its exact class).

/// A method target key: `(class_fqn, method_name)`, lowercased.
pub type MethodKey = (String, String);

/// Whether a class method may host a phpdoc→native rewrite (ADR-0041 §1 split),
/// from the hierarchy alone; non-`Eligible` → `magic-method`/`method-inheritance`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MethodEligibility {
    /// Private, final, or on a final class — narrowing cannot break Liskov.
    Eligible,
    /// A magic method (`__*`, e.g. `__construct`): never a candidate (ADR-0046 §3).
    Magic,
    /// Inheritance-involved, so a partial rewrite would risk Liskov. Carries detail.
    Inheritance(String),
}

/// The whole-project method-call reverse sweep the method-transform planners
/// consume (ADR-0043 §6). Parallel to [`FreeFnSweep`] but keyed on [`MethodKey`].
#[derive(Debug, Clone, Default)]
pub struct MethodSweep {
    pub targets: HashMap<MethodKey, TargetSweep>,
    /// Dynamic method call sites — taint every candidate (`dynamic-call-present`).
    pub dynamic_method_sites: Vec<SweepSite>,
    /// Unresolved method names → sites (`resolution-ambiguous`); first is the
    /// refusal's representative.
    pub unresolved_method_names: HashMap<String, Vec<SweepSite>>,
    /// Method names referenced as a *value* → sites (`function-referenced-as-value`).
    pub value_referenced_methods: HashMap<String, Vec<SweepSite>>,
    /// The ADR-0041 §1 eligibility verdict, every declared method.
    pub eligibility: HashMap<MethodKey, MethodEligibility>,
}

/// Sweep every method call in `project`: attribute args, record what makes "all
/// callers proven" unknowable, and compute each declared method's eligibility.
#[must_use]
pub fn sweep_methods(db: &dyn Db, project: Project) -> MethodSweep {
    let handles: Vec<SourceFile> = project.files(db).to_vec();
    let units: Vec<FileUnit> =
        handles.iter().map(|&f| FileUnit { path: f.path(db), tree: parse(db, f) }).collect();
    let db_index = project_index(db, project);
    let pos: HashMap<SourceFile, usize> =
        handles.iter().enumerate().map(|(i, &f)| (f, i)).collect();
    let index = Index::from_db(db_index, &pos, &units);

    let mut out = MethodSweep::default();
    let empty_store = Store::default();

    for fi in 0..units.len() {
        let cx = Cx::new(&units, &index, fi);
        let tree = cx.tree();
        let path = cx.path();

        // (1) Resolve method calls per scope (owner gives enclosing class).
        for scope in tree.scopes() {
            let enclosing = match &scope.owner {
                ScopeOwner::Method { class, .. } => Some(class.as_str()),
                _ => None,
            };
            for call in &scope.method_calls {
                let cp = tree.position(call.span.start);
                let call_site = SweepSite::new(path, cp.line, cp.column);
                for arg in &call.args {
                    collect_method_value_names(
                        &arg.value,
                        &call_site,
                        &mut out.value_referenced_methods,
                        &mut out.dynamic_method_sites,
                    );
                }
                resolve_one_method_call(
                    &cx, tree, path, scope, enclosing, &empty_store, call, &mut out,
                );
            }
        }

        // (2) A callable can also flow through a free-function call arg, e.g.
        // `usort($x, [$o, 'm'])`.
        for call in tree.calls() {
            let cp = tree.position(call.span.start);
            let call_site = SweepSite::new(path, cp.line, cp.column);
            for arg in &call.args {
                collect_method_value_names(
                    &arg.value,
                    &call_site,
                    &mut out.value_referenced_methods,
                    &mut out.dynamic_method_sites,
                );
            }
            if matches!(call.receiver, Callee::Dynamic) {
                // `$arr['x']()` and friends could invoke a method.
                out.dynamic_method_sites.push(call_site);
            }
        }
        for scope in tree.scopes() {
            scan_scope_method_values(
                scope,
                tree,
                path,
                &mut out.value_referenced_methods,
                &mut out.dynamic_method_sites,
            );
        }

        // (3) Eligibility for every declared method (hierarchy-only; ADR-0041 §1).
        for class in tree.classes() {
            for m in &class.methods {
                let key = (class.fqn.to_ascii_lowercase(), m.name.to_ascii_lowercase());
                out.eligibility.entry(key).or_insert_with(|| method_eligibility(&cx, class, m));
            }
        }
    }
    out
}

/// Resolve one method/static call: attribute its args, or taint its method name.
#[allow(clippy::too_many_arguments)]
fn resolve_one_method_call(
    cx: &Cx,
    tree: &SourceTree,
    path: &str,
    scope: &Scope,
    enclosing: Option<&str>,
    store: &Store,
    call: &steins_syntax::CallExpr,
    out: &mut MethodSweep,
) {
    // A dynamic method selector taints every method (any name could be the target).
    let method_name = match &call.receiver {
        Callee::Method { method, .. } | Callee::Static { method, .. } => method.as_str(),
        Callee::Dynamic => {
            let p = tree.position(call.span.start);
            out.dynamic_method_sites.push(SweepSite::new(path, p.line, p.column));
            return;
        }
        // scan_method_calls only emits Method/Static/Dynamic receivers.
        _ => return,
    };

    match resolve_call_target(cx, &call.receiver, store, None, enclosing, scope.poisoned) {
        Some(target) => {
            let key = (
                target.declaring_class.fqn.to_ascii_lowercase(),
                target.method.name.to_ascii_lowercase(),
            );
            let entry = out.targets.entry(key).or_default();
            if call.positional_only {
                for (i, arg) in call.args.iter().enumerate() {
                    let p = tree.position(arg.span.start);
                    entry.observed.push(ObservedArg {
                        param_index: i,
                        caller_path: path.to_owned(),
                        line: p.line,
                        column: p.column,
                        value: arg.value.clone(),
                    });
                }
            } else {
                entry.named_or_spread = true;
            }
        }
        None => {
            // Taint project-wide; first recorded site is the refusal's representative.
            let p = tree.position(call.span.start);
            out.unresolved_method_names
                .entry(method_name.to_ascii_lowercase())
                .or_default()
                .push(SweepSite::new(path, p.line, p.column));
        }
    }
}

/// The ADR-0041 §1 eligibility split, computed from the class hierarchy alone.
fn method_eligibility(cx: &Cx, class: &ClassDecl, m: &MethodDecl) -> MethodEligibility {
    // Magic methods are never candidates (ADR-0046 §3).
    if m.is_constructor || m.name.starts_with("__") {
        return MethodEligibility::Magic;
    }
    if class.is_interface {
        return MethodEligibility::Inheritance(
            "an interface method is an inherited contract point".to_owned(),
        );
    }
    if m.is_abstract {
        return MethodEligibility::Inheritance(
            "an abstract method is implemented (overridden) by every subclass".to_owned(),
        );
    }
    if class.uses_traits {
        return MethodEligibility::Inheritance(
            "the class `use`s a trait; trait methods merge in, so override analysis is incomplete"
                .to_owned(),
        );
    }
    let promotable = m.is_final || m.visibility == Visibility::Private || class.is_final;
    if !promotable {
        return MethodEligibility::Inheritance(
            "a non-final public/protected method on a non-final class may be overridden by a subclass (Liskov)"
                .to_owned(),
        );
    }
    // Private dispatches by the calling scope's class, never a subclass — always
    // Liskov-safe, no ancestor walk needed.
    if m.visibility == Visibility::Private {
        return MethodEligibility::Eligible;
    }
    // Must not *override* an ancestor method of the same name (a supertype
    // caller could break Liskov).
    match overrides_ancestor(cx, &class.fqn, &m.name) {
        AncestorVerdict::Clean => MethodEligibility::Eligible,
        AncestorVerdict::Overrides => MethodEligibility::Inheritance(
            "overrides a parent/interface method of the same name (narrowing would break Liskov substitution)"
                .to_owned(),
        ),
        AncestorVerdict::Incomplete => MethodEligibility::Inheritance(
            "the class hierarchy is not fully resolvable, so `does not override an ancestor` cannot be proven"
                .to_owned(),
        ),
    }
}

/// The result of the strict-ancestor override walk.
enum AncestorVerdict {
    /// Fully enumerated; no ancestor declares the method.
    Clean,
    /// Some ancestor declares a method of that name.
    Overrides,
    /// Not fully enumerable (unresolved parent/interface, trait, opaque builtin).
    Incomplete,
}

/// Walk `class_fqn`'s ancestors for `method`; `Incomplete` if any edge unenumerable.
fn overrides_ancestor(cx: &Cx, class_fqn: &str, method: &str) -> AncestorVerdict {
    let Some(mut queue) = cx.ancestors_of(class_fqn) else {
        return AncestorVerdict::Incomplete;
    };
    let mut seen: HashSet<String> = HashSet::new();
    let mut incomplete = false;
    while let Some(cur) = queue.pop() {
        if !seen.insert(cur.to_ascii_lowercase()) {
            continue;
        }
        match cx.find_class(&cur) {
            Some((_, cd)) => {
                if cd.methods.iter().any(|mm| mm.name.eq_ignore_ascii_case(method)) {
                    return AncestorVerdict::Overrides;
                }
                // A trait ancestor may merge in a same-name method we can't see.
                if cd.uses_traits {
                    incomplete = true;
                }
                match cx.ancestors_of(&cur) {
                    Some(supers) => queue.extend(supers),
                    None => incomplete = true,
                }
            }
            // Builtin/unknown external: method surface is opaque.
            None => incomplete = true,
        }
    }
    if incomplete { AncestorVerdict::Incomplete } else { AncestorVerdict::Clean }
}

/// Extract a method name referenced as a *callable value* into `set`: a callable
/// string `'Foo::method'` or array `[$target, 'method']`, recursing into arrays.
/// A non-literal method-name position (`[$obj, $var]`) names no method, and left
/// undetected would be a caller invisible to any method the value could resolve
/// to at runtime (issue #6); value tracking is out of scope (ADR-0041/0046), so
/// this records a dynamic site instead, tainting every method project-wide.
fn collect_method_value_names(
    v: &ArgValue,
    site: &SweepSite,
    set: &mut HashMap<String, Vec<SweepSite>>,
    dynamic: &mut Vec<SweepSite>,
) {
    match v {
        ArgValue::Str(s) => {
            if let Some((_, m)) = s.as_str().and_then(|s| s.rsplit_once("::"))
                && is_identifier(m)
            {
                push_name(set, m.to_ascii_lowercase(), site);
            }
        }
        ArgValue::Array(items) => {
            // A callable array is exactly two entries; the second is the name.
            if items.len() == 2 {
                match &items[1].1 {
                    ArgValue::Str(name) => {
                        if let Some(name) = name.as_str()
                            && is_identifier(name)
                        {
                            push_name(set, name.to_ascii_lowercase(), site);
                        }
                    }
                    // No name extractable: taint broadly rather than see nothing.
                    _ => dynamic.push(site.clone()),
                }
            }
            for (_, e) in items {
                collect_method_value_names(e, site, set, dynamic);
            }
        }
        _ => {}
    }
}

/// Scan a scope's trace for callable values escaping through assignment or
/// return, recursing into `if`/`match`.
fn scan_scope_method_values(
    scope: &Scope,
    tree: &SourceTree,
    path: &str,
    set: &mut HashMap<String, Vec<SweepSite>>,
    dynamic: &mut Vec<SweepSite>,
) {
    scan_stmts_method_values(&scope.stmts, tree, path, set, dynamic);
}

fn scan_stmts_method_values(
    stmts: &[Stmt],
    tree: &SourceTree,
    path: &str,
    set: &mut HashMap<String, Vec<SweepSite>>,
    dynamic: &mut Vec<SweepSite>,
) {
    for s in stmts {
        match &s.kind {
            StmtKind::Assign { value, .. }
            | StmtKind::PropAssign { value, .. }
            | StmtKind::Return { value, .. } => {
                let p = tree.position(s.span.start);
                collect_method_value_names(
                    value,
                    &SweepSite::new(path, p.line, p.column),
                    set,
                    dynamic,
                );
            }
            StmtKind::If { then_trace, elseifs, else_trace, .. } => {
                scan_stmts_method_values(then_trace, tree, path, set, dynamic);
                for (_, branch) in elseifs {
                    scan_stmts_method_values(branch, tree, path, set, dynamic);
                }
                if let Some(e) = else_trace {
                    scan_stmts_method_values(e, tree, path, set, dynamic);
                }
            }
            StmtKind::Match { arms, default, .. } => {
                for arm in arms {
                    scan_stmts_method_values(&arm.trace, tree, path, set, dynamic);
                }
                if let Some(d) = default {
                    scan_stmts_method_values(d, tree, path, set, dynamic);
                }
            }
            _ => {}
        }
    }
}

/// Whether `s` is a plain PHP identifier, so a random string merely containing
/// `::` isn't mistaken for a callable reference.
fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}
