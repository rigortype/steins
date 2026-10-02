//! The per-statement guard-region sweep (issue #928): a decided ternary or short-circuit
//! expression proves the arm or operand PHP never evaluates dead, in whatever position
//! the statement spells it.
//!
//! The condition lane already did this for an `if` ([`walk_if`]) and for the operands of
//! its own `&&`/`||`, and the assignment seam did it for a ternary on the right of `$x =`.
//! What was missing was every other position: a ternary in `return`, in an `echo`, in an
//! argument, a bare `a && b;` statement. A [`GuardRegion`] is the lowering's record of
//! each one ([`Stmt::guards`]), so one sweep serves them all and a guard discharges the
//! same way wherever it is written.
//!
//! [`walk_if`]: crate::branch::walk_if

use std::collections::HashMap;

use steins_domain::Certainty;
use steins_syntax::Stmt;

use crate::cond::eval_cond;
use crate::env::Store;
use crate::fold::Folder;
use crate::walk::{WalkCx, mark_dead_span};

/// Judge every guard region of `stmt` and record the extents its verdict proves
/// unevaluated. Plain per-scope walk only — the caller's `descent.is_none()` gate —
/// because a dead region is a universal truth only there.
///
/// Each region is evaluated against **no environment**: a statement's own bindings
/// (an embedded assignment, a by-reference call earlier in the same expression) are not
/// yet in the entry env this runs under, so a test that names a variable could be read
/// stale. The lowering carries only the regions that can decide without one — a guard
/// on a function, class, constant or extension, a version comparison — and those are
/// the same wherever the statement runs.
///
/// Evaluating a logical expression records, as a side effect, the right operand of
/// every connective in it that its left operand decides ([`mark_dead_operand`]), which
/// is the whole of what such a region is for.
///
/// [`mark_dead_operand`]: crate::walk::mark_dead_operand
pub(crate) fn sweep_guard_regions(w: &WalkCx, folder: &mut dyn Folder, stmt: &Stmt) {
    if stmt.guards.is_empty() {
        return;
    }
    let env = HashMap::new();
    let store = Store::default();
    for region in &stmt.guards {
        let verdict = eval_cond(w, folder, &region.cond, &env, &store, w.scope.poisoned);
        let dead = match verdict {
            Certainty::Yes => region.dead_if_true,
            Certainty::No => region.dead_if_false,
            Certainty::Maybe => None,
        };
        if let Some(span) = dead {
            mark_dead_span(w, span);
        }
    }
}
