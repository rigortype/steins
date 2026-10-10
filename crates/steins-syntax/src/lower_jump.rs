//! Jumps credited by level (ADR-0103, issues #904 and #944): where a `break`, a
//! `continue` or a `goto` lands, read off the syntax by counting the breakable
//! constructs it is written in.
//!
//! Every pass that asks where a jump goes asks it here, so the lowering of loops
//! (`break_free`, `nested_jumps_only`), the structured `switch` arm
//! ([`ArmLanding`]) and the binding-presence pass's parked jump states count the
//! same levels.
//!
//! # The rule
//!
//! A jump inside a `switch` case body is read against `d`, the number of loops and
//! `switch`es between the jump and the case body's own top level. A `try`, an `if`
//! or a block is not breakable and adds nothing to `d`; a function-like or a
//! class-like is another scope and is not descended.
//!
//! | jump | where it lands, for this `switch` |
//! | --- | --- |
//! | `break N` / `continue N`, `N <= d` | absorbed by a construct inside the case; not this switch's edge |
//! | `break N` / `continue N`, `N == d + 1` | this switch's **successor**: the arm *lands* |
//! | `break N` / `continue N`, `N > d + 1` | a construct outside this switch: it **ends the arm** and the enclosing loop's own count owns it |
//! | `break` / `continue` with a level that is not a positive integer literal | a compile error in PHP (`break $n` since 5.4, `break 0` always); read as the worst case, landing **and** ending the arm |
//! | `goto` | its label is not bounded here, so the arm **lands**; the edge carries no env, since every path from it to the successor passes the label, which lowers as a barrier — it only keeps the successor reachable when nothing else does |
//! | `return` / `throw` / `exit` | leaves the function: **ends the arm**, never lands |
//!
//! `continue` counts a `switch` as a level exactly as `break` does, and a `continue
//! N` whose `N`th level is a `switch` *is* `break N` (PHP 7.3+ says so in a
//! compile-time warning, "\"continue\" targeting switch is equivalent to
//! \"break\""): it lands on that switch's successor.
//!
//! A jump out of a `try` block or a `catch` runs the `finally` before it lands
//! (witnessed on PHP 8.5.11); a `finally` cannot itself hold a jump that leaves
//! it, which PHP rejects at compile time. The landing edge therefore forgets what
//! the **whole** case body writes, `finally` included, and never only what runs
//! before the jump. A `finally` that terminates replaces the jump: `try { break; }
//! finally { return; }` returns, so such a jump lands nowhere, for a loop's count
//! as for a switch's ([`body_has_jump`]).
//!
//! # A `goto` and the list it ends
//!
//! An unconditional `goto` ends the list it is written in, wherever that list is (a
//! case body, an `if` arm, a loop body, a `try` part, a scope's top level): it
//! lowers as a [`StmtKind::LoopJump`], and the walk stops there. Where its label is
//! decides only where the walk resumes. Every row is witnessed on PHP 8.5.11.
//!
//! | label | lowering and walk |
//! | --- | --- |
//! | none after it in its list | the rest of the list is dead (`if ($c) { goto out; $o = null; }` never assigns) |
//! | later in the same list | dead up to the label; the label ([`StmtKind::Label`]) resumes the walk with nothing known, today's barrier reading |
//! | inside a later `if` or `try` in the list | that statement holds the label ([`Stmt::holds_label`]). PHP lets a `goto` enter an `if` arm or a `try` block, even one whose condition is false, so what precedes the label in it is dead: after a terminator the statement is marked dead whole and the walk resumes after it with nothing known; reached by falling through, it is walked as usual |
//! | before it (a backward `goto`) | the rest of its list is dead; the label, walked earlier, is a barrier, so the loop it forms carries no fact into its second pass |
//! | inside a loop or a `switch` | PHP refuses it at compile time ("'goto' into loop or switch statement is disallowed"), so such a label resumes nothing outside its construct |
//! | in an enclosing list | the statement after the construct holds it, or follows it: resumed there as above |
//!
//! A `switch` arm's `goto` also lands on the successor (the table above), with no
//! env: every path from it to the successor passes its label. `block_end` and
//! `body_end` read a terminator followed by a label as undecided, as the walk does.
//!
//! Known false negatives (ADR-0103 §2.2b): a label in an `if` arm the walk decides is
//! dead is not walked; an `Opaque` that holds a label (a `declare` block) does not
//! resume the walk; a `goto` path into a `No` arm drops out of a `match` join. A
//! `try` holding a `goto` still falls through as a whole (`walk_try`), so code after
//! it is walked even where the `goto` makes it dead.
//!
//! The same counting answers the loop questions: a loop body is the case body
//! with the loop itself as the construct, and a `continue N` with `N == d + 1`
//! re-enters that loop rather than leaving it.

use mago_syntax::cst::{Expression, Node, Statement};

use crate::ast::{ArgValue, ArmLanding, BodyEnd, Runs};
#[cfg(doc)]
use crate::ast::{Stmt, StmtKind};
use crate::children;
use crate::lower_expr::lower_arg_value;
use crate::lower_stmt::{block_end, node_poisons, scan_runs};
use crate::lower_try::list_sets;

/// A `break`/`continue` level as written: `Some(n)` for a positive integer literal
/// (`None` written is level 1), `None` for anything else — `break $n`, which PHP has
/// rejected since 5.4, and `break 0`, which it always has. Every caller reads `None`
/// as the worst case its question has.
pub(crate) fn jump_level(level: Option<&Expression<'_>>) -> Option<u32> {
    match level {
        None => Some(1),
        Some(e) => match lower_arg_value(e) {
            ArgValue::Int(n) => u32::try_from(n).ok().filter(|n| *n >= 1),
            _ => None,
        },
    }
}

/// The level of a jump node: [`jump_level`] for a `break` or a `continue`, `None`
/// for anything else (a `goto` has no level).
pub(crate) fn node_jump_level(jump: &Node<'_, '_>) -> Option<u32> {
    match jump {
        Node::Break(b) => jump_level(b.level),
        Node::Continue(c) => jump_level(c.level),
        _ => None,
    }
}

/// The scan every level question shares: whether any `break`, `continue` or `goto`
/// under `node` answers `hit`. `depth` is the number of breakable structures (loops
/// and `switch`es) between this node and the list the question is about.
///
/// A `try` whose `finally` terminates contributes only the jumps in that `finally`:
/// a jump out of its block or a `catch` runs the `finally`, whose `return`, `throw`
/// or `exit` replaces the jump (the module table's `finally` row), so it lands
/// nowhere.
pub(crate) fn body_has_jump(
    node: &Node<'_, '_>,
    depth: u32,
    hit: &dyn Fn(&Node<'_, '_>, u32) -> bool,
) -> bool {
    match node {
        Node::Break(_) | Node::Continue(_) | Node::Goto(_) => hit(node, depth),
        Node::Try(t)
            if t.finally_clause.as_ref().is_some_and(|f| {
                block_end(f.block.statements.as_slice()) == BodyEnd::Terminates
            }) =>
        {
            let f = t.finally_clause.as_ref().expect("guarded above");
            f.block.statements.iter().any(|s| body_has_jump(&Node::Statement(s), depth, hit))
        }
        // A nested breakable structure absorbs one level of every jump beneath it.
        Node::While(_) | Node::For(_) | Node::Foreach(_) | Node::DoWhile(_) | Node::Switch(_) => {
            children(node).iter().any(|c| body_has_jump(c, depth + 1, hit))
        }
        // Separate scopes: their bodies do not run here, and PHP does not let a jump
        // in one target a structure out here.
        Node::Function(_)
        | Node::Closure(_)
        | Node::ArrowFunction(_)
        | Node::AnonymousClass(_)
        | Node::Class(_)
        | Node::Interface(_)
        | Node::Trait(_)
        | Node::Enum(_) => false,
        other => children(other).iter().any(|c| body_has_jump(c, depth, hit)),
    }
}

/// The module table's "lands" rows for one jump `depth` breakable structures below a
/// case body's top level: `break N`/`continue N` with `N == depth + 1`, a level
/// that is not a positive literal, or a `goto`.
fn jump_lands_on_switch(jump: &Node<'_, '_>, depth: u32) -> bool {
    match jump {
        Node::Break(_) | Node::Continue(_) => node_jump_level(jump).is_none_or(|n| n == depth + 1),
        _ => true,
    }
}

/// The `break`/`continue` rows of [`jump_lands_on_switch`] alone: the jumps whose
/// landing carries an env.
fn level_lands_on_switch(jump: &Node<'_, '_>, depth: u32) -> bool {
    !matches!(jump, Node::Goto(_)) && jump_lands_on_switch(jump, depth)
}

/// The landing a case body owes its switch's successor (see [`ArmLanding`]), or
/// `None` when no jump in it lands there.
pub(crate) fn arm_landing(body: &[Statement<'_>]) -> Option<ArmLanding> {
    let nodes: Vec<Node<'_, '_>> = body.iter().map(Node::Statement).collect();
    if !nodes.iter().any(|n| body_has_jump(n, 0, &jump_lands_on_switch)) {
        return None;
    }
    let by_level = nodes.iter().any(|n| body_has_jump(n, 0, &level_lands_on_switch));
    let (writes, reads) = list_sets(&nodes);
    let clears = nodes.iter().any(node_poisons);
    let mut runs = Runs::default();
    for n in &nodes {
        scan_runs(n, &mut runs);
    }
    if runs.other {
        runs.functions = Vec::new();
        runs.constructs = Vec::new();
    }
    Some(ArmLanding { writes, reads, clears, by_level, runs })
}

/// Whether a trailing jump of a case body is the switch's own `break` — `break;`,
/// `break 1;`, `continue;` or `continue 1;` (the last two are `break` there, the
/// module table's `continue` row) — which ends the arm on the successor.
pub(crate) fn ends_own_switch(s: &Statement<'_>) -> bool {
    match s {
        Statement::Break(b) => jump_level(b.level) == Some(1),
        Statement::Continue(c) => jump_level(c.level) == Some(1),
        _ => false,
    }
}

/// The level a statement-position `break`/`continue` is parked at by the presence
/// pass: [`jump_level`], with a level PHP refuses to compile read as `1`. Such a
/// file never runs, so the pass needs only some answer that does not panic; `1` is
/// the reading it had before levels were counted.
pub(crate) fn parked_level(level: Option<&Expression<'_>>) -> u32 {
    jump_level(level).unwrap_or(1)
}
