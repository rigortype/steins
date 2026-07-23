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
use steins_syntax::{ArgValue, Callee, ClosureRef, SourceTree, Stmt, StmtKind};

use crate::{Cx, FileUnit, FnResolution, Index};

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
