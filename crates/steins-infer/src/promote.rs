//! The reverse call-site sweep for phpdoc→native parameter promotion
//! (ADR-0034 point 4 / ADR-0037): the precondition *all callers proven*, which
//! is structurally unavailable to modular tools.
//!
//! This is the narrow seam the transform engine (`steins-edit`) reaches into: it
//! reuses the inference engine's own name resolution (`Cx::resolve_function`,
//! the project [`Index`]) rather than forking it, and returns plain data. The
//! transform crate owns candidate enumeration, native-type mapping, the
//! acceptance judgment (`steins-contract::admits_*`), refusal assembly, and the
//! edit mechanics — none of which need the inference internals.
//!
//! Only **free-function** targets are swept: v1 promotion scope is free functions
//! (method call-site resolution across receivers is a materially larger surface,
//! deferred with design). A candidate is safe to promote only when *every* call
//! that could reach it is accounted for; the sweep therefore also records the
//! project-wide obstacles that make "all callers" unknowable — dynamic calls,
//! first-class/string references, and unresolved same-name calls.

use std::collections::{HashMap, HashSet};

use steins_db::{Db, Project, SourceFile, parse, project_index};
use steins_syntax::{
    ArgValue, Callee, ClassDecl, ClosureRef, MethodDecl, Scope, ScopeOwner, SourceTree, Stmt,
    StmtKind, Visibility,
};

use crate::{Cx, FileUnit, FnResolution, Index, Store, resolve_call_target};

/// One positional argument observed flowing into a target free function at a call
/// site that resolved uniquely to it.
#[derive(Debug, Clone)]
pub struct ObservedArg {
    /// The zero-based positional parameter index this argument fills.
    pub param_index: usize,
    /// The caller's file path (for the refusal/audit site).
    pub caller_path: String,
    pub line: u32,
    pub column: u32,
    /// The lowered argument value — the transform proves/admits it.
    pub value: ArgValue,
}

/// The reverse-sweep facts for one free-function target (keyed by lowercased FQN).
#[derive(Debug, Clone, Default)]
pub struct TargetSweep {
    /// Every positional argument at every uniquely-resolving call site.
    pub observed: Vec<ObservedArg>,
    /// A call resolving to this target used named or spread arguments (positional
    /// mapping is unreliable) — the `named-or-spread-args` refusal trigger.
    pub named_or_spread: bool,
}

/// The whole-project reverse sweep the promotion planner consumes.
#[derive(Debug, Clone, Default)]
pub struct FreeFnSweep {
    /// Target lowercased FQN → observed args + flags.
    pub targets: HashMap<String, TargetSweep>,
    /// A dynamic (`$fn()`) or otherwise unrepresentable call exists anywhere. Such
    /// a call could target *any* free function, so every candidate is tainted
    /// (`dynamic-call-present`). Conservative and sound.
    pub any_dynamic_call: bool,
    /// Lowercased names (every qualified spelling seen, plus its last segment)
    /// that appear as string or first-class-callable *values* anywhere — the
    /// `function-referenced-as-value` trigger. A candidate whose FQN or simple
    /// name is present here cannot be promoted (a `call_user_func`-style caller is
    /// invisible to call resolution).
    pub value_referenced_names: HashSet<String>,
    /// Lowercased simple names of function-callee calls that did **not** resolve
    /// to a unique user function (ambiguous / builtin-shadowed / unknown). A
    /// candidate whose simple name is here can't be proven to see all of its
    /// callers (`resolution-ambiguous`).
    pub unresolved_simple_names: HashSet<String>,
}

/// Sweep every call in `project`, attributing positional arguments to the free
/// functions they uniquely resolve to and recording the obstacles that would make
/// "all callers proven" unknowable.
#[must_use]
pub fn sweep_free_functions(db: &dyn Db, project: Project) -> FreeFnSweep {
    let handles: Vec<SourceFile> = project.files(db).to_vec();
    let units: Vec<FileUnit> =
        handles.iter().map(|&f| FileUnit { path: f.path(db), tree: parse(db, f) }).collect();
    let db_index = project_index(db, project);
    let pos: HashMap<SourceFile, usize> =
        handles.iter().enumerate().map(|(i, &f)| (f, i)).collect();
    let index = Index::from_db(db_index, &pos);

    let mut out = FreeFnSweep::default();
    for fi in 0..units.len() {
        let cx = Cx::new(&units, &index, fi);
        let tree = cx.tree();
        let path = cx.path();
        for call in tree.calls() {
            // Value-reference scan across every argument, regardless of callee
            // kind: a function name flowing as a string/callable value is a caller
            // invisible to resolution.
            for arg in &call.args {
                collect_value_names(&arg.value, &mut out.value_referenced_names);
            }

            // `call_user_func`/`call_user_func_array` whose callable argument is an
            // opaque runtime value (a bare variable, a call result, …) carries no
            // name the scan above can see — it could hold ANY free function at
            // runtime. Taint broadly, mirroring a direct dynamic `$fn()` call,
            // rather than silently seeing nothing (same family as issue #6's
            // callable-array gap).
            if let Callee::Function(name) = &call.receiver
                && is_generic_invoker(name)
                && let Some(callable_arg) = call.args.first()
                && callable_arg_is_opaque(&callable_arg.value)
            {
                out.any_dynamic_call = true;
            }

            match &call.receiver {
                Callee::DynamicVar(_) | Callee::Dynamic => {
                    out.any_dynamic_call = true;
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
                        FnResolution::Builtin | FnResolution::Unknown => {
                            out.unresolved_simple_names.insert(cref.simple().to_ascii_lowercase());
                        }
                    }
                }
                // Method / static / constructor calls are not free-function calls;
                // their arguments were already scanned for value-references above.
                Callee::Method { .. } | Callee::Static { .. } | Callee::Construct { .. } => {}
            }
        }

        // A first-class callable / function-name string can also flow through a
        // non-call value position (`$g = f(...);`, `return 'f';`), invisible to
        // `calls()`. Scan the scope traces too.
        scan_scope_values(tree, &mut out.value_referenced_names);
    }
    out
}

/// Scan every scope's linear trace for function-name-shaped values that escape
/// through a non-call position (assignment / property-assignment / return rhs,
/// recursing into structured `if`/`match` sub-traces).
fn scan_scope_values(tree: &SourceTree, set: &mut HashSet<String>) {
    for scope in tree.scopes() {
        scan_stmts(&scope.stmts, set);
    }
}

fn scan_stmts(stmts: &[Stmt], set: &mut HashSet<String>) {
    for s in stmts {
        match &s.kind {
            StmtKind::Assign { value, .. }
            | StmtKind::PropAssign { value, .. }
            | StmtKind::Return { value, .. } => collect_value_names(value, set),
            StmtKind::If { then_trace, elseifs, else_trace, .. } => {
                scan_stmts(then_trace, set);
                for (_, branch) in elseifs {
                    scan_stmts(branch, set);
                }
                if let Some(e) = else_trace {
                    scan_stmts(e, set);
                }
            }
            StmtKind::Match { arms, default, .. } => {
                for arm in arms {
                    scan_stmts(&arm.trace, set);
                }
                if let Some(d) = default {
                    scan_stmts(d, set);
                }
            }
            _ => {}
        }
    }
}

/// Recursively collect function-name-shaped string and first-class-callable
/// *values* into `set` (lowercased; both the full spelling with a leading `\`
/// stripped and its last segment).
fn collect_value_names(v: &ArgValue, set: &mut HashSet<String>) {
    match v {
        ArgValue::Str(s) => insert_name_forms(s, set),
        ArgValue::Closure(ClosureRef::FunctionName(name)) => {
            insert_name_forms(&name.raw, set);
            set.insert(name.simple().to_ascii_lowercase());
        }
        ArgValue::Array(items) => {
            for (_, e) in items {
                collect_value_names(e, set);
            }
        }
        _ => {}
    }
}

fn insert_name_forms(raw: &str, set: &mut HashSet<String>) {
    let norm = raw.trim_start_matches('\\').to_ascii_lowercase();
    if let Some(pos) = norm.rfind('\\') {
        set.insert(norm[pos + 1..].to_owned());
    }
    set.insert(norm);
}

/// Whether `name` (a `Callee::Function` simple name, as written) is one of PHP's
/// generic first-class-callable invokers — the `call_user_func*` family named
/// explicitly in the `function-referenced-as-value` taxonomy (ADR-0041 §3):
/// their first argument is *itself* the callable to invoke, so an opaque value
/// there is invisible to ordinary call resolution.
fn is_generic_invoker(name: &str) -> bool {
    name.eq_ignore_ascii_case("call_user_func") || name.eq_ignore_ascii_case("call_user_func_array")
}

/// Whether `v` is a runtime value the literal-value scan (`collect_value_names`)
/// cannot already account for, and which could hold an arbitrary callable at
/// runtime: a bare variable, a call result, a `new`, a ternary, a property fetch,
/// … — anything that is neither a name-shaped literal (a string, a first-class-
/// callable reference, a callable array — all already scanned) nor a scalar shape
/// PHP would reject as a callable outright (`int`/`float`/`bool`/`null`).
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

// ===========================================================================
// The method-call reverse sweep (ADR-0043 §6): the class-world analogue of
// `sweep_free_functions`. Where a free function is keyed by its FQN, a method is
// keyed by `(class_fqn, method_name)` — the `Sym::Method` shape — and its callers
// arrive through the six receiver forms (`$this->m()`, `self::`/`parent::m()`,
// `Foo::m()`, `(new Foo)->m()`, `$var->m()`). The sweep resolves each call to a
// unique target method (the checker's own `resolve_call_target`) and attributes
// its positional arguments; a call it cannot resolve to a unique target taints the
// *method name* project-wide — the conservative soundness rule (a method M is
// enumerable only if EVERY call that could reach M is resolved).
//
// Precision deliberately deferred (soundness-first, ADR-0043 §6): a `$var->m()`
// receiver is resolved only when the sweep can prove `$var`'s exact class, which
// (having no per-scope object heap here) it never can — so every `$var->m()`
// taints its method name. Enclosing-class-aware `$this->`/`self::`/`parent::` and
// explicit `Foo::`/`new Foo()->` resolution IS performed, so private/final methods
// reachable only through those forms still enumerate precisely.
// ===========================================================================

/// A method target key: `(class_fqn, method_name)`, both ASCII-lowercased — the
/// [`steins_syntax`]/`Sym::Method` identity the checker keys method dispatch on.
pub type MethodKey = (String, String);

/// A call site whose method the sweep could not resolve to a unique target, kept
/// so a `resolution-ambiguous` refusal can name the offending location.
#[derive(Debug, Clone)]
pub struct MethodCallSite {
    pub path: String,
    pub line: u32,
    pub column: u32,
}

/// Whether a class method may host a phpdoc→native rewrite (the ADR-0041 §1
/// eligibility split), computed from the class hierarchy alone (independent of any
/// docblock). The transform crate turns a non-`Eligible` verdict into the reserved
/// `magic-method` / `method-inheritance` refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MethodEligibility {
    /// Private, or final, or a method of a final class, with no inheritance
    /// involvement — narrowing its parameter cannot break Liskov substitution.
    Eligible,
    /// A magic method (`__construct`, `__wakeup`, `__toString`, any `__*`): never a
    /// candidate (ADR-0046 §3). Carries no detail — the reason name says it all.
    Magic,
    /// The method is inheritance-involved (overridable, overriding, abstract, an
    /// interface method, or in a class whose hierarchy is not fully resolvable), so
    /// a partial rewrite would risk Liskov. Carries the human detail.
    Inheritance(String),
}

/// The whole-project method-call reverse sweep the method-transform planners
/// consume (ADR-0043 §6). Parallel to [`FreeFnSweep`] but keyed on [`MethodKey`].
#[derive(Debug, Clone, Default)]
pub struct MethodSweep {
    /// `(class_fqn, method)` → observed positional args + the named/spread flag,
    /// for every call that resolved *uniquely and exactly* to that method.
    pub targets: HashMap<MethodKey, TargetSweep>,
    /// A dynamic method call (`$o->$m()`, `$o::{$m}()`, or any [`Callee::Dynamic`])
    /// exists somewhere — it could target *any* method, so every candidate is
    /// tainted (the `dynamic-call-present` refusal). Conservative and sound.
    pub any_dynamic_method: bool,
    /// Lowercased method names that appear in a method call the sweep could NOT
    /// resolve to a unique target (an unknown-class `$var->m()`, a non-final
    /// overridable `self::m()`, a chain that leaves the project, …). A candidate
    /// whose name is here cannot prove it sees all its callers
    /// (`resolution-ambiguous`); the value is a representative offending site.
    pub unresolved_method_names: HashMap<String, MethodCallSite>,
    /// Lowercased method names referenced as a *value* — a callable string
    /// `'Foo::m'` or a callable array `[$o, 'm']` — a caller invisible to call
    /// resolution (`function-referenced-as-value`).
    pub value_referenced_methods: HashSet<String>,
    /// `(class_fqn, method)` → the ADR-0041 §1 eligibility verdict for *every*
    /// method declared in the project (independent of its docblock).
    pub eligibility: HashMap<MethodKey, MethodEligibility>,
}

/// Sweep every method call in `project`, attributing positional arguments to the
/// methods they uniquely resolve to, recording the obstacles that make "all
/// callers proven" unknowable, and computing each declared method's eligibility.
#[must_use]
pub fn sweep_methods(db: &dyn Db, project: Project) -> MethodSweep {
    let handles: Vec<SourceFile> = project.files(db).to_vec();
    let units: Vec<FileUnit> =
        handles.iter().map(|&f| FileUnit { path: f.path(db), tree: parse(db, f) }).collect();
    let db_index = project_index(db, project);
    let pos: HashMap<SourceFile, usize> =
        handles.iter().enumerate().map(|(i, &f)| (f, i)).collect();
    let index = Index::from_db(db_index, &pos);

    let mut out = MethodSweep::default();
    let empty_store = Store::default();

    for fi in 0..units.len() {
        let cx = Cx::new(&units, &index, fi);
        let tree = cx.tree();
        let path = cx.path();

        // (1) Resolve method calls per scope (the scope owner supplies the enclosing
        // class for `$this->`/`self::`/`parent::`).
        for scope in tree.scopes() {
            let enclosing = match &scope.owner {
                ScopeOwner::Method { class, .. } => Some(class.as_str()),
                _ => None,
            };
            for call in &scope.method_calls {
                // Value-reference scan across every argument (a method name flowing
                // as a callable string/array is a caller invisible to resolution).
                for arg in &call.args {
                    collect_method_value_names(
                        &arg.value,
                        &mut out.value_referenced_methods,
                        &mut out.any_dynamic_method,
                    );
                }
                resolve_one_method_call(
                    &cx, tree, path, scope, enclosing, &empty_store, call, &mut out,
                );
            }
        }

        // (2) A callable string/array can also flow through a value position
        // (a free-function call arg like `usort($x, [$o, 'm'])`, an assignment, or a
        // return) — scan free-function call args and scope traces too.
        for call in tree.calls() {
            for arg in &call.args {
                collect_method_value_names(
                    &arg.value,
                    &mut out.value_referenced_methods,
                    &mut out.any_dynamic_method,
                );
            }
            if matches!(call.receiver, Callee::Dynamic) {
                // `$arr['x']()` and friends could invoke a method via a callable.
                out.any_dynamic_method = true;
            }
        }
        for scope in tree.scopes() {
            scan_scope_method_values(
                scope,
                &mut out.value_referenced_methods,
                &mut out.any_dynamic_method,
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

/// Resolve one method/static call, attributing its args on a unique resolution or
/// tainting its method name otherwise.
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
            out.any_dynamic_method = true;
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
            // Unresolved to a unique target → taint the method name project-wide.
            let p = tree.position(call.span.start);
            out.unresolved_method_names.entry(method_name.to_ascii_lowercase()).or_insert(
                MethodCallSite { path: path.to_owned(), line: p.line, column: p.column },
            );
        }
    }
}

/// The ADR-0041 §1 eligibility split, computed from the class hierarchy alone.
fn method_eligibility(cx: &Cx, class: &ClassDecl, m: &MethodDecl) -> MethodEligibility {
    // Magic methods are never candidates (ADR-0046 §3): `__construct`, `__wakeup`,
    // `__toString`, and every other `__`-prefixed reserved name.
    if m.is_constructor || m.name.starts_with("__") {
        return MethodEligibility::Magic;
    }
    // Interface methods are inherited contract points; abstract methods are
    // implemented by subclasses — both are inherently override sites.
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
    // A trait-using class merges methods whose bodies live elsewhere, so override
    // analysis is incomplete — refuse rather than misclassify.
    if class.uses_traits {
        return MethodEligibility::Inheritance(
            "the class `use`s a trait; trait methods merge in, so override analysis is incomplete"
                .to_owned(),
        );
    }
    // The promotable kind: private, or final, or a method of a final class. A
    // non-final public/protected method on a non-final class may be overridden by a
    // subclass, so narrowing its parameter could break Liskov substitution.
    let promotable = m.is_final || m.visibility == Visibility::Private || class.is_final;
    if !promotable {
        return MethodEligibility::Inheritance(
            "a non-final public/protected method on a non-final class may be overridden by a subclass (Liskov)"
                .to_owned(),
        );
    }
    // A private method is not part of dispatch inheritance (PHP resolves it by the
    // calling scope's class, never a subclass override), so narrowing it is always
    // Liskov-safe — no ancestor analysis needed. "Not overridden by a subclass" is
    // guaranteed for the other promotable kinds too: a final method cannot be
    // overridden, and a final class cannot be subclassed.
    if m.visibility == Visibility::Private {
        return MethodEligibility::Eligible;
    }
    // A final method, or a method of a final class, that is non-private: it must not
    // *override* an ancestor's method of the same name (a caller holding the
    // supertype could reach this body through dispatch, so narrowing would break
    // Liskov). Prove the ancestor set is fully enumerated and free of that name.
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
    /// The ancestor set is fully enumerated and no ancestor declares the method.
    Clean,
    /// Some ancestor declares a method of that name — the candidate overrides it.
    Overrides,
    /// The ancestor set is not fully enumerable (an unresolved parent/interface, a
    /// trait-using ancestor, or a builtin whose methods are opaque).
    Incomplete,
}

/// Walk `class_fqn`'s strict ancestors (parent + `implements`, transitively) for a
/// declaration of `method`. `Overrides` on the first hit; `Incomplete` if any
/// ancestor edge cannot be enumerated (an unknown external, a trait-using class, or
/// a builtin whose method surface is opaque to us).
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
                // A trait-using ancestor may merge in a method of that name we cannot
                // see — cannot prove "does not override".
                if cd.uses_traits {
                    incomplete = true;
                }
                match cx.ancestors_of(&cur) {
                    Some(supers) => queue.extend(supers),
                    None => incomplete = true,
                }
            }
            // A catalogued builtin (or unknown external): its method surface is
            // opaque, so we cannot prove the candidate does not override one of them.
            None => incomplete = true,
        }
    }
    if incomplete { AncestorVerdict::Incomplete } else { AncestorVerdict::Clean }
}

/// Extract a method name referenced as a *callable value* into `set`: a callable
/// string `'Foo::method'` (name after the last `::`) or a callable array
/// `[$target, 'method']` (a 2-element array whose second entry is a string method
/// name). Recurses into arrays so a callable nested in a value is still seen.
///
/// A callable array whose method-name position (the second entry) is present but
/// **not** a literal string — `[$obj, $var]`, `[$obj, someExpr()]`, … — names no
/// method at all, so it cannot be added to `set`; left undetected, it would be a
/// caller invisible to *every* method of whatever name `$var` resolves to at
/// runtime (issue #6). Context-sensitively tracking what the variable might hold
/// is out of scope (v1 posture, ADR-0041/0046): instead this mirrors the existing
/// `$o->$m()` (`Callee::Dynamic`) handling and sets `any_dynamic` — the broadest,
/// conservative fallback that taints every method project-wide, exactly like an
/// unresolvable dynamic method-call selector.
fn collect_method_value_names(v: &ArgValue, set: &mut HashSet<String>, any_dynamic: &mut bool) {
    match v {
        ArgValue::Str(s) => {
            if let Some((_, m)) = s.rsplit_once("::")
                && is_identifier(m)
            {
                set.insert(m.to_ascii_lowercase());
            }
        }
        ArgValue::Array(items) => {
            // A classic callable array `[$obj, 'method']` / `[Foo::class, 'method']`
            // is exactly two entries; the second is the method-name position.
            if items.len() == 2 {
                match &items[1].1 {
                    ArgValue::Str(name) => {
                        if is_identifier(name) {
                            set.insert(name.to_ascii_lowercase());
                        }
                    }
                    // Non-literal method-name position: no name can be extracted, so
                    // taint broadly rather than silently seeing nothing.
                    _ => *any_dynamic = true,
                }
            }
            for (_, e) in items {
                collect_method_value_names(e, set, any_dynamic);
            }
        }
        _ => {}
    }
}

/// Scan a scope's linear trace for callable values escaping through an assignment /
/// property-assignment / return position, recursing into `if`/`match` sub-traces.
fn scan_scope_method_values(scope: &Scope, set: &mut HashSet<String>, any_dynamic: &mut bool) {
    scan_stmts_method_values(&scope.stmts, set, any_dynamic);
}

fn scan_stmts_method_values(stmts: &[Stmt], set: &mut HashSet<String>, any_dynamic: &mut bool) {
    for s in stmts {
        match &s.kind {
            StmtKind::Assign { value, .. }
            | StmtKind::PropAssign { value, .. }
            | StmtKind::Return { value, .. } => collect_method_value_names(value, set, any_dynamic),
            StmtKind::If { then_trace, elseifs, else_trace, .. } => {
                scan_stmts_method_values(then_trace, set, any_dynamic);
                for (_, branch) in elseifs {
                    scan_stmts_method_values(branch, set, any_dynamic);
                }
                if let Some(e) = else_trace {
                    scan_stmts_method_values(e, set, any_dynamic);
                }
            }
            StmtKind::Match { arms, default, .. } => {
                for arm in arms {
                    scan_stmts_method_values(&arm.trace, set, any_dynamic);
                }
                if let Some(d) = default {
                    scan_stmts_method_values(d, set, any_dynamic);
                }
            }
            _ => {}
        }
    }
}

/// Whether `s` is a plain PHP identifier (a method-name shape) — so a random string
/// literal that merely contains `::` or sits in a 2-element array is not mistaken
/// for a callable reference.
fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}
