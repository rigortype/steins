//! A structured `try` (the ADR-0027 `try` amendment, issues #943 and #905): the
//! env each part of the construct starts in, the caught variable, and whether the
//! code after it is reachable.
//!
//! The control-flow table the rule comes from is on [`StmtKind::Try`]; the lowering
//! applies the same rule to `BodyEnd`, and the presence pass to its own flow.

use std::collections::HashMap;

use steins_syntax::{CatchClause, NativeType, Param, Span, Stmt, StmtKind, TypeMember};

use crate::annotate::LineFact;
use crate::assert_harness::{MuteObservers, observers_muted};
use crate::contract::IsA;
use crate::env::{Descent, Known, Store, join_envs};
use crate::fold::Folder;
use crate::heap::seed_declared_param_object;
use crate::loops::loop_entry_forget;
use crate::project::Diagnostic;
use crate::refine::seed_contract_arms;
use crate::walk::{Flow, WalkCx, forget_construct_sets, push_hidden_exit_floor, walk_trace};

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
///   entry forgets both parts' sets. That walk is the one that judges the
///   `finally`'s own statements.
/// * The **successor** is reached only through a *normal exit* — the body's end,
///   or the end of a live `catch`, each reached by falling through — and then
///   through the `finally`, when there is one, falling through as well. A path
///   through the body's end ran the whole body; a path through a `catch` ran it from
///   its forgotten entry. So the successor env is the join of those exits' envs,
///   carried through the `finally` (S1b of #1033, issue #943's `t6`):
///
/// | normal exits that fall through | `finally` | successor |
/// | --- | --- | --- |
/// | none | any | dead |
/// | some | none | the join of their envs |
/// | some | terminates on its walk over every entry | dead |
/// | some | falls through on that walk | the `finally` walked from the join: its end env, or dead where that walk terminates |
/// | a `goto` or label inside | any | reachable; the construct's sets forgotten as an `Opaque`'s |
///
/// The second walk of a `finally` ([`walk_for_env`]) holds on the normal paths
/// only, so it reports nothing and records nothing; the walk over every entry has
/// already answered for the same statements.
///
/// That second walk is taken at the outermost `try` only. A `try` with a `finally`
/// met *inside* a second walk takes the end env of its first `finally` walk as its
/// successor instead: that walk started from the entry every path into the
/// `finally` shares, so its end covers the normal paths as well, and the env is one
/// this call already holds. Walking the inner `finally` twice too would double the
/// work at every level of `finally` nested in a `finally` (2^depth walks); with the
/// cut, depth n costs O(n²). The inner successor forgets what the block and the
/// catches wrote, which costs precision only, and only to the outer second walk,
/// which reports nothing.
///
/// A `return` the parts record is a summary exit, and the hidden-exit floor
/// ([`push_hidden_exit_floor`]) still goes with it whenever the construct may
/// return: a `finally` that runs after a `return` in the body or a `catch` can
/// replace the value, or mutate the returned object and `$this`, after the
/// return's snapshot was taken. The floor is about the exits, not the successor,
/// so the join leaves it as it was.
///
/// Where the top-level frame's rebind rule fires (`rebinds`, issue #762), a body
/// statement may have run user code that rebound any global before the body was
/// left, so the handler entries start from nothing; the body itself applies the
/// rule statement by statement, so its end env is already the rule's. A construct
/// that `poisons` leaves nothing behind.
///
/// The construct terminates when its `finally` does, or when the body and every
/// live `catch` do. A `goto` or a label inside keeps the successor reachable, with
/// S1's conservative env (ADR-0103: control may arrive at a label with anything
/// bound).
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

    // The normal exits: every part that falls through, with the env it ends in.
    let mut exits: Vec<(HashMap<String, Known>, Store)> = Vec::new();
    // A `return` in the body or a `catch` of a `try` whose `finally` can itself exit is
    // replaced by that exit: the return seam's mismatch ids decline it (issue #1099).
    let outer_exits = store.finally_exits;
    let finally_exits = outer_exits
        || finally.as_ref().is_some_and(|trace| trace.iter().any(|s| s.has_terminator));
    let mut benv = env.clone();
    let mut bstore = store.clone();
    bstore.finally_exits = finally_exits;
    let body_flow =
        walk_trace(w, folder, body, &mut benv, &mut bstore, descent, facts, guarded, out);
    if body_flow == Flow::FellThrough {
        exits.push((benv, bstore));
    }

    if let Some((cenv, cstore)) = catch_entry {
        for arm in catches {
            let mut henv = cenv.clone();
            let mut hstore = cstore.clone();
            hstore.finally_exits = finally_exits;
            bind_caught(w, &arm.clause, &mut henv, &mut hstore);
            let trace = &arm.trace;
            let flow =
                walk_trace(w, folder, trace, &mut henv, &mut hstore, descent, facts, true, out);
            if flow == Flow::FellThrough {
                exits.push((henv, hstore));
            }
        }
    }

    let mut finally_terminates = false;
    let mut finally_end = None;
    if let (Some(trace), Some((mut fenv, mut fstore))) = (finally, finally_entry) {
        let flow =
            walk_trace(w, folder, trace, &mut fenv, &mut fstore, descent, facts, guarded, out);
        finally_terminates = flow == Flow::Terminated;
        finally_end = Some((fenv, fstore));
    }

    if *has_goto {
        if rebinds {
            env.clear();
            store.clear();
        }
        forget_construct_sets(w, writes, reads, *poisons, *may_return, env, store);
        return Flow::FellThrough;
    }
    push_hidden_exit_floor(w, *may_return);
    if finally_terminates || exits.is_empty() {
        return Flow::Terminated;
    }
    // Inside a walk that is itself carrying an outer `finally` to its successor, the
    // end of the walk over every entry stands in for the second walk: it covers the
    // normal paths too, and walking again here would double the work at every level.
    if let Some((fenv, fstore)) = finally_end
        && observers_muted()
    {
        *env = fenv;
        *store = fstore;
        store.finally_exits = outer_exits;
        return Flow::FellThrough;
    }
    let (mut jenv, mut jstore) = join_envs(exits);
    if *poisons {
        jenv.clear();
        jstore.clear();
    }
    if let Some(trace) = finally
        && walk_for_env(w, folder, trace, &mut jenv, &mut jstore, descent, guarded)
            == Flow::Terminated
    {
        return Flow::Terminated;
    }
    *env = jenv;
    *store = jstore;
    store.finally_exits = outer_exits;
    Flow::FellThrough
}

/// Walk `trace` for the env it leaves and nothing else: the `finally` carried from
/// the normal exits of its `try` to the successor ([`walk_try`]).
///
/// The env it starts from holds on the normal paths into the `finally` only, not on
/// the paths an exception or a jump takes through it, so nothing this walk reads
/// may stand for the statements it walks. Its findings and line facts go nowhere,
/// the dead regions, uncovered matches and summary exits it adds are cut off again,
/// and the harness observers are muted for its duration. What it keeps is shared
/// with the walk over every entry by construction: allocation ids (one counter for
/// the scope, so a `new` here never collides with one the successor meets later)
/// and the descent memo, which is keyed by the binding it answers.
fn walk_for_env(
    w: &WalkCx,
    folder: &mut dyn Folder,
    trace: &[Stmt],
    env: &mut HashMap<String, Known>,
    store: &mut Store,
    descent: &mut Option<Descent<'_>>,
    guarded: bool,
) -> Flow {
    let dead = w.dead.borrow().len();
    let uncovered = w.uncovered_matches.borrow().len();
    let exits = w.summary.as_ref().map(|sc| {
        (sc.exits.borrow().len(), sc.this_exits.as_ref().map(|te| te.borrow().len()))
    });
    let mut discarded = Vec::new();
    let flow = {
        let _muted = MuteObservers::new();
        walk_trace(w, folder, trace, env, store, descent, &mut None, guarded, &mut discarded)
    };
    w.dead.borrow_mut().truncate(dead);
    w.uncovered_matches.borrow_mut().truncate(uncovered);
    if let (Some(sc), Some((len, this_len))) = (&w.summary, exits) {
        sc.exits.borrow_mut().truncate(len);
        if let (Some(te), Some(this_len)) = (&sc.this_exits, this_len) {
            te.borrow_mut().truncate(this_len);
        }
    }
    flow
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
    store.release(var);
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
        top: None,
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
