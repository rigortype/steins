//! Per-parse memoization of the pure whole-subtree scans (issue #484).
//!
//! Several lowering scans are pure functions of the subtree they are launched
//! on, yet run once per *enclosing* construct: `stmt_end` re-judges every
//! nested statement of an `if` chain from each level that contains it, the
//! presence pass re-walks a loop body once per fixpoint round, and a function
//! body's opaque/call-var inventories are collected once for the effects
//! context and again for the scope build. This module caches those results on
//! subtree identity so each subtree is walked once per scan family and per
//! parse — `docs/agents/profiling.md` puts the family at 45–57% of worker CPU.
//!
//! # Scope and lifetime
//!
//! The tables live for exactly one [`SourceTree::parse`] call: [`Scope::enter`]
//! activates them and its `Drop` clears them, the same per-parse thread-local
//! discipline [`crate::stack_guard`] uses (the lowering has no context struct
//! threaded through its walkers to hang the tables on, and threading one would
//! touch every recursive walker in the crate). No state crosses runs or files
//! — cross-run persistence is ADR-0092's layer, not this one's — and no Mago
//! type leaks: everything here is `pub(crate)` at most (the ADR-0003 hard rule).
//!
//! A **nested** parse (nothing does this today) clears the tables and leaves
//! memoization off for the remainder of the outer parse rather than sharing
//! them. Two live arenas cannot alias an address, but once the inner parse
//! returns and its arena frees, the allocator can hand that memory to the
//! outer arena's next chunk — and a stale key with the right shape tag would
//! then answer for the wrong subtree. Deactivating on nesting closes that
//! class outright; the cost is cache misses, never a wrong hit.
//!
//! # Key identity, and why it cannot collide
//!
//! A cached entry is keyed by [`NodeKey`]: the CST node's address in the parse
//! arena plus a tag for the entry's shape (statement or expression). Within one
//! parse this is collision-free:
//!
//! * the arena allocates monotonically and frees nothing until the parse
//!   returns, and the tables are cleared inside that lifetime, so an address is
//!   never reused while an entry citing it exists;
//! * two distinct live nodes of the *same* type occupy disjoint storage — a
//!   `Statement` cannot contain another `Statement` inline (that type would be
//!   infinitely sized; statement nesting goes through arena references), and
//!   the same holds for `Expression`;
//! * a `Statement` and the `Expression` it holds *can* start at the same
//!   address (an enum payload may sit at offset zero), which is exactly what
//!   the shape tag disambiguates.
//!
//! # When the memo is inert
//!
//! The walks descend through [`crate::children`], which returns an empty child
//! list once a [`crate::stack_guard`] budget is spent — under a budget a scan's
//! result depends on the stack depth it was launched from, not on the subtree
//! alone, so [`Scope::enter`] refuses to activate while a floor is installed
//! (the wasm playground). Every wrapper then computes directly, unchanged.
//!
//! [`SourceTree::parse`]: crate::SourceTree::parse

use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::HashSet;
use std::rc::Rc;

use mago_syntax::cst::{Node, Statement, Try};

use crate::ast::{BodyEnd, OpaqueSite, Runs, UndefinedRead};
use crate::stack_guard;

/// A CST node's identity within one parse: its arena address, tagged with the
/// entry shape. See the module doc for the collision-freedom argument.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct NodeKey(usize, u8);

/// The key for a node a scan can be entered on — a statement or an expression
/// — or `None` for any other shape, which is never cached.
pub(crate) fn node_key(node: &Node<'_, '_>) -> Option<NodeKey> {
    match node {
        Node::Statement(s) => Some(stmt_key(s)),
        Node::Expression(e) => Some(NodeKey(std::ptr::from_ref(*e) as usize, 1)),
        _ => None,
    }
}

pub(crate) fn stmt_key(s: &Statement<'_>) -> NodeKey {
    NodeKey(std::ptr::from_ref(s) as usize, 0)
}

/// The key of a `try` node. A `Try` sits inside its `Statement` and can share its
/// address, so it takes a shape tag of its own; two distinct `Try` nodes occupy
/// disjoint storage, as two statements do.
pub(crate) fn try_key(t: &Try<'_>) -> NodeKey {
    NodeKey(std::ptr::from_ref(t) as usize, 2)
}

/// The whole-subtree scans memoized at a `try` node (issue #943's review). Nested
/// `try` statements are lowered one level at a time, and each level runs these
/// scans over its whole subtree, so without a cut at every `try` the work grows
/// with the square of the nesting depth.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum TryScan {
    CallVars,
    AssignWrites,
    Mentioned,
    MayReturn,
    Goto,
}

/// The per-leaf result the presence pass re-derives on every fixpoint round
/// (ADR-0081): the reads and bound names of one leaf unit, both pure functions
/// of the unit's subtree. The judgment against the flowing state stays in
/// `presence_leaf`; only the subtree scan is cached.
pub(crate) struct PresenceLeaf {
    pub(crate) reads: Vec<UndefinedRead>,
    pub(crate) bound: HashSet<String>,
}

#[derive(Default)]
struct Tables {
    active: bool,
    stmt_end: HashMap<NodeKey, BodyEnd>,
    function_exit: HashMap<NodeKey, bool>,
    opaque: HashMap<NodeKey, Rc<Vec<OpaqueSite>>>,
    call_vars: HashMap<NodeKey, Rc<Vec<String>>>,
    presence_leaf: HashMap<NodeKey, Rc<PresenceLeaf>>,
    try_names: HashMap<(NodeKey, TryScan), Rc<Vec<String>>>,
    try_flags: HashMap<(NodeKey, TryScan), bool>,
    try_runs: HashMap<NodeKey, Rc<Runs>>,
}

thread_local! {
    static TABLES: RefCell<Tables> = RefCell::new(Tables::default());
}

/// Activates the memo for one parse, clears it on drop. Refuses to activate
/// under a stack-guard floor (see the module doc); entered while another scope
/// is already active, it clears the tables and leaves memoization off for the
/// remainder of the outer parse — see the module doc for why sharing tables
/// across two arenas would be unsound.
pub(crate) struct Scope {
    activated: bool,
}

impl Scope {
    pub(crate) fn enter() -> Self {
        let activated = TABLES.with_borrow_mut(|t| {
            if t.active {
                // Nested parse: drop the outer entries and deactivate (see the
                // module doc). Keeping the outer tables live would let the two
                // arenas' addresses cross once the inner one frees.
                *t = Tables::default();
                return false;
            }
            if stack_guard::guarded() {
                return false;
            }
            t.active = true;
            true
        });
        Self { activated }
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        if self.activated {
            // Dropping the whole table set (not just the flag) is what keeps a
            // later parse from reading entries keyed by a freed arena's
            // addresses.
            TABLES.with_borrow_mut(|t| *t = Tables::default());
        }
    }
}

/// Whether lookups can hit at all — wrappers that would otherwise pay for a
/// cacheable-shape detour (an extra `Vec`, a full walk on a predicate path)
/// check this first and fall through to the plain walk when the memo is inert.
pub(crate) fn enabled() -> bool {
    TABLES.with_borrow(|t| t.active)
}

pub(crate) fn stmt_end_lookup(s: &Statement<'_>) -> Option<BodyEnd> {
    TABLES.with_borrow(|t| t.stmt_end.get(&stmt_key(s)).copied())
}

pub(crate) fn stmt_end_store(s: &Statement<'_>, end: BodyEnd) {
    TABLES.with_borrow_mut(|t| {
        if t.active {
            t.stmt_end.insert(stmt_key(s), end);
        }
    });
}

pub(crate) fn function_exit_lookup(s: &Statement<'_>) -> Option<bool> {
    TABLES.with_borrow(|t| t.function_exit.get(&stmt_key(s)).copied())
}

pub(crate) fn function_exit_store(s: &Statement<'_>, hit: bool) {
    TABLES.with_borrow_mut(|t| {
        if t.active {
            t.function_exit.insert(stmt_key(s), hit);
        }
    });
}

pub(crate) fn opaque_lookup(key: NodeKey) -> Option<Rc<Vec<OpaqueSite>>> {
    TABLES.with_borrow(|t| t.opaque.get(&key).cloned())
}

pub(crate) fn opaque_store(key: NodeKey, sites: Rc<Vec<OpaqueSite>>) {
    TABLES.with_borrow_mut(|t| {
        if t.active {
            t.opaque.insert(key, sites);
        }
    });
}

pub(crate) fn call_vars_lookup(key: NodeKey) -> Option<Rc<Vec<String>>> {
    TABLES.with_borrow(|t| t.call_vars.get(&key).cloned())
}

pub(crate) fn call_vars_store(key: NodeKey, vars: Rc<Vec<String>>) {
    TABLES.with_borrow_mut(|t| {
        if t.active {
            t.call_vars.insert(key, vars);
        }
    });
}

/// The cached names `scan` answers for the `try` node `t`, computing (and caching)
/// them on a miss; `None` when the memo is inert.
pub(crate) fn try_names(
    t: &Try<'_>,
    scan: TryScan,
    compute: impl FnOnce() -> Vec<String>,
) -> Option<Rc<Vec<String>>> {
    if !enabled() {
        return None;
    }
    let key = (try_key(t), scan);
    if let Some(hit) = TABLES.with_borrow(|t| t.try_names.get(&key).cloned()) {
        return Some(hit);
    }
    let fresh = Rc::new(compute());
    TABLES.with_borrow_mut(|t| {
        if t.active {
            t.try_names.insert(key, Rc::clone(&fresh));
        }
    });
    Some(fresh)
}

/// [`try_names`] for a yes/no scan.
pub(crate) fn try_flag(t: &Try<'_>, scan: TryScan, compute: impl FnOnce() -> bool) -> Option<bool> {
    if !enabled() {
        return None;
    }
    let key = (try_key(t), scan);
    if let Some(hit) = TABLES.with_borrow(|t| t.try_flags.get(&key).copied()) {
        return Some(hit);
    }
    let fresh = compute();
    TABLES.with_borrow_mut(|t| {
        if t.active {
            t.try_flags.insert(key, fresh);
        }
    });
    Some(fresh)
}

/// [`try_names`] for the [`Runs`] record of a `try`'s subtree.
pub(crate) fn try_runs(t: &Try<'_>, compute: impl FnOnce() -> Runs) -> Option<Rc<Runs>> {
    if !enabled() {
        return None;
    }
    let key = try_key(t);
    if let Some(hit) = TABLES.with_borrow(|t| t.try_runs.get(&key).cloned()) {
        return Some(hit);
    }
    let fresh = Rc::new(compute());
    TABLES.with_borrow_mut(|t| {
        if t.active {
            t.try_runs.insert(key, Rc::clone(&fresh));
        }
    });
    Some(fresh)
}

/// The cached leaf scan for `node`, computing (and caching) it on a miss. The
/// compute closure runs with no table borrow held, so the scans it launches may
/// consult other tables freely.
pub(crate) fn presence_leaf(
    node: &Node<'_, '_>,
    compute: impl FnOnce() -> PresenceLeaf,
) -> Rc<PresenceLeaf> {
    let key = if enabled() { node_key(node) } else { None };
    let Some(key) = key else { return Rc::new(compute()) };
    if let Some(hit) = TABLES.with_borrow(|t| t.presence_leaf.get(&key).cloned()) {
        return hit;
    }
    let fresh = Rc::new(compute());
    TABLES.with_borrow_mut(|t| {
        if t.active {
            t.presence_leaf.insert(key, Rc::clone(&fresh));
        }
    });
    fresh
}

#[cfg(test)]
mod tests {
    use super::*;

    // Synthetic keys stand in for arena addresses: the tables never dereference
    // a key, so no CST needs to exist for the lifecycle to be tested.
    #[test]
    fn a_nested_scope_clears_the_tables_and_deactivates_instead_of_sharing_them() {
        let key = NodeKey(0xdead_beef, 0);
        let outer = Scope::enter();
        opaque_store(key, Rc::new(Vec::new()));
        assert!(opaque_lookup(key).is_some(), "the outer scope caches");
        {
            let inner = Scope::enter();
            assert!(
                opaque_lookup(key).is_none(),
                "the nested enter must drop the outer entries — a freed inner arena's \
                 addresses can be reused by the outer one"
            );
            opaque_store(key, Rc::new(Vec::new()));
            assert!(opaque_lookup(key).is_none(), "stores are inert while deactivated");
            drop(inner);
        }
        opaque_store(key, Rc::new(Vec::new()));
        assert!(
            opaque_lookup(key).is_none(),
            "the outer parse stays deactivated for its remainder — misses, never a stale hit"
        );
        drop(outer);
        // A later parse on this thread starts clean and can activate again.
        let next = Scope::enter();
        opaque_store(key, Rc::new(Vec::new()));
        assert!(opaque_lookup(key).is_some(), "a fresh scope activates normally");
        drop(next);
        assert!(opaque_lookup(key).is_none(), "and its drop clears the tables");
    }
}
