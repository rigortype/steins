//! A structured `try` (the ADR-0027 `try` amendment, issues #943 and #905): the
//! env each part of the construct starts in, the caught variable, and whether the
//! code after it is reachable.
//!
//! The control-flow table the rule comes from is on [`StmtKind::Try`]; the lowering
//! applies the same rule to `BodyEnd`, and the presence pass to its own flow.

use std::collections::HashMap;

use steins_syntax::{CatchClause, NativeType, Param, Span, Stmt, StmtKind, TypeMember};

use crate::annotate::LineFact;
use crate::contract::IsA;
use crate::env::{Descent, Known, Store};
use crate::fold::Folder;
use crate::heap::seed_declared_param_object;
use crate::loops::loop_entry_forget;
use crate::project::Diagnostic;
use crate::refine::seed_contract_arms;
use crate::walk::{Flow, WalkCx, forget_construct_sets, walk_trace};

/// Walk one structured `try` statement and answer whether the code after it is
/// reachable.
///
/// * The **body** runs straight-line from the construct's entry, so it is walked on
///   a copy of the env as it stands, exactly as the statements before it were.
/// * A **`catch`** is entered from any point of the body: its entry is the
///   construct's entry with the body's `writes` forgotten and the objects its
///   `reads` name swept — [`loop_entry_forget`]'s argument, for the same reason (a
///   name the body never assigns or hands to a call holds what it held at the
///   entry, but the object it points at may have been mutated through it). The
///   caught variable is then bound to the caught classes ([`bind_caught`]). A
///   `catch` of a body no statement of which can throw is dead and is not walked.
/// * **`finally`** is entered from any point of the body or of one `catch`, so its
///   entry forgets both parts' sets.
/// * The **successor** forgets the whole construct's sets, an `Opaque`'s rule
///   ([`forget_construct_sets`]), hidden-exit floor included: what the parts leave
///   behind is not joined here.
///
/// Where the top-level frame's rebind rule fires (`rebinds`, issue #762), a body
/// statement may have run user code that rebound any global before the body was
/// left, so the handler entries and the successor start from nothing; the body
/// itself applies the rule statement by statement.
///
/// The construct terminates when its `finally` does, or when the body and every
/// live `catch` do. A `goto` or a label inside keeps the successor reachable.
#[allow(clippy::too_many_arguments)]
pub(crate) fn walk_try(
    w: &WalkCx,
    folder: &mut dyn Folder,
    stmt: &Stmt,
    rebinds: bool,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
    descent: &mut Option<Descent<'_>>,
    facts: &mut Option<&mut Vec<LineFact>>,
    guarded: bool,
    out: &mut Vec<Diagnostic>,
) -> Flow {
    let StmtKind::Try {
        body,
        catches,
        finally,
        catches_live,
        has_goto,
        body_writes,
        body_reads,
        catch_writes,
        catch_reads,
        writes,
        reads,
        poisons,
        may_return,
    } = &stmt.kind
    else {
        unreachable!("walk_trace hands walk_try the `try` kind only");
    };
    let catch_entry = (*catches_live && !catches.is_empty())
        .then(|| handler_entry(env, store, rebinds, *poisons, &[(body_writes, body_reads)]));
    let finally_entry = finally.as_ref().map(|_| {
        let parts = [(body_writes, body_reads), (catch_writes, catch_reads)];
        handler_entry(env, store, rebinds, *poisons, &parts)
    });

    let mut benv = env.clone();
    let mut bstore = store.clone();
    let body_flow =
        walk_trace(w, folder, body, &mut benv, &mut bstore, descent, facts, guarded, out);
    let mut falls = body_flow == Flow::FellThrough;

    if let Some((cenv, cstore)) = catch_entry {
        for arm in catches {
            let mut henv = cenv.clone();
            let mut hstore = cstore.clone();
            bind_caught(w, &arm.clause, &mut henv, &mut hstore);
            let flow =
                walk_trace(w, folder, &arm.trace, &mut henv, &mut hstore, descent, facts, true, out);
            falls |= flow == Flow::FellThrough;
        }
    }

    let mut finally_terminates = false;
    if let (Some(trace), Some((mut fenv, mut fstore))) = (finally, finally_entry) {
        let flow = walk_trace(w, folder, trace, &mut fenv, &mut fstore, descent, facts, guarded, out);
        finally_terminates = flow == Flow::Terminated;
    }

    if rebinds {
        env.clear();
        store.clear();
    }
    forget_construct_sets(w, writes, reads, *poisons, *may_return, env, store);
    if !*has_goto && (finally_terminates || !falls) { Flow::Terminated } else { Flow::FellThrough }
}

/// The env a handler (a `catch` or the `finally`) starts in: the construct's entry
/// with every part it can be entered from forgotten, each by
/// [`loop_entry_forget`]'s rule — its `writes` dropped, the objects its `reads`
/// name swept. Applying the parts one after the other forgets their union.
fn handler_entry(
    env: &HashMap<String, Known>,
    store: &Store,
    rebinds: bool,
    poisons: bool,
    parts: &[(&Vec<String>, &Vec<String>)],
) -> (HashMap<String, Known>, Store) {
    let mut henv = env.clone();
    let mut hstore = store.clone();
    if rebinds {
        henv.clear();
        hstore.clear();
    }
    for (writes, reads) in parts {
        loop_entry_forget(writes, reads, &[], poisons, &mut henv, &mut hstore);
    }
    (henv, hstore)
}

/// Bind a `catch` clause's variable at its entry: forget whatever the name held,
/// then seed it as a declared receiver of the caught classes — the contract lane
/// with one `Verified` arm per class, and for a single class the declared heap
/// object a parameter of that type gets (ADR-0032's 2026-08-16 amendment).
///
/// PHP binds the variable to the thrown object, which is an instance of one of the
/// caught classes and is always a `Throwable`. The seed is withheld unless every
/// caught class is a known class-like that is provably a `Throwable`: an interface
/// that does not extend it (`catch (Marker $e)`) names only part of what the
/// object is, and a lane of that interface alone would call `$e->getMessage()`
/// missing. A name the lowering could not read (`has_unresolvable`) seeds nothing
/// either; the variable stays bound and unknown.
fn bind_caught(
    w: &WalkCx,
    clause: &CatchClause,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
) {
    let Some(var) = clause.var.as_deref() else { return };
    store.escape_resource(var);
    env.remove(var);
    store.unbind(var);
    if clause.has_unresolvable || clause.classes.is_empty() {
        return;
    }
    let cx = w.cx;
    let mut members = Vec::with_capacity(clause.classes.len());
    for r in &clause.classes {
        let display = cx.class_fqn(r).trim_start_matches('\\').to_owned();
        let fqn = display.to_ascii_lowercase();
        if !cx.is_known_class(&fqn) || cx.is_a(&fqn, "throwable") != IsA::Yes {
            return;
        }
        members.push(TypeMember::Instance { fqn, display });
    }
    let param = Param {
        name: var.to_owned(),
        ty: Some(NativeType { members, nullable: false }),
        hint_span: None,
        variadic: false,
        by_ref: false,
        has_null_default: false,
        has_default: false,
        default: None,
        span: Span { start: 0, end: 0 },
    };
    if let Some(arms) = seed_contract_arms(&param, None, &|n: &str| n.to_ascii_lowercase())
        && !arms.is_empty()
    {
        store.contract.insert(var.to_owned(), arms);
    }
    let shadow = cx.scope_template_shadow(w.scope);
    if let Some(obj) = seed_declared_param_object(cx, &param, None, &shadow) {
        let id = w.fresh_id();
        store.heap.insert(id, obj);
        store.refs.insert(var.to_owned(), id);
    }
}
