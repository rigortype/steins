//! The reads a branch condition, a loop header or a `switch` subject evaluates
//! (ADR-0104's S4 amendment, issue #1050), judged by the construct's walker at the point
//! where PHP evaluates them, in the env that reaches that point.
//!
//! A [`Header`] carries its positions as a statement does ([`Header::eval`]), so the
//! consumers are the statement's: the guard sweep, the call positions of
//! [`crate::stmt_calls`] and the member and offset reads of [`crate::stmt_checks`], with
//! their carrier tables unchanged. What this module adds is where and under which env a
//! header is judged, and when it is not judged at all:
//!
//! | Header | Evaluated | Judged under | Carriers |
//! | --- | --- | --- | --- |
//! | `if` condition | once, at the construct | the env the `if` is entered with | as written |
//! | `elseif` condition, `switch (true)` label, `match (true)` arm | when every earlier condition failed | the env the earlier conditions' false sides leave | as written |
//! | the same, after a condition decided true | never | not judged; its extent is dead | |
//! | `while` condition | before every iteration, the first included | the loop-head env, which holds at the first evaluation | as written |
//! | `for` conditions | after `init`, before every iteration | the loop-head env after `init` | as written |
//! | `for` step | after an iteration completes | the loop-head env; not judged when no iteration completes | `Conditional` |
//! | `do`-`while` condition | after an iteration completes | the loop-head env; not judged when the body never reaches it | `Conditional` |
//! | `foreach` subject | once, at the construct | the env the loop is entered with | as written |
//! | `switch` subject | once, at the construct | the env the `switch` is entered with | as written |
//! | structured `match` or `switch` arm conditions | when no earlier arm matched | the env the construct is entered with; dead after an arm decided taken | as written |
//! | a hoisted value-position `match (true)` arm | ahead of the operands to its left | the hoisted entry's env | `Conditional` |
//! | a `match` subject | as an operand | the enclosing statement's read positions | as the operand |
//!
//! `Conditional` (recorded by the lowering) keeps the environment-free families and
//! declines the value-dependent ones: the env the walker holds there is not one PHP
//! evaluates the header in. The loop-head env is the loop's entry env with every name the
//! loop writes forgotten, so a value-dependent finding read off it holds at the header's
//! first evaluation, which is where PHP warns or throws.
//!
//! Headers are judged in the plain per-scope pass only, like a statement's positions.
//!
//! [`Header::eval`]: steins_syntax::Header::eval

use std::collections::HashMap;

use steins_syntax::Header;

use crate::env::{Descent, Known, Store};
use crate::fold::Folder;
use crate::guard_sweep::sweep_guard_regions;
use crate::project::Diagnostic;
use crate::stmt_calls::check_position_calls;
use crate::stmt_checks::check_read_positions;
use crate::walk::{WalkCx, in_dead};

/// Judge `head`'s read positions under `env` and `store`, the pair PHP evaluates it in.
/// `raised` is where the findings already raised about this header begin in `out` (a
/// guard call's `preg` pattern): a finding at the same place is not raised twice.
#[allow(clippy::too_many_arguments)]
pub(crate) fn check_header(
    w: &WalkCx,
    folder: &mut dyn Folder,
    head: &Header,
    env: &HashMap<String, Known>,
    store: &Store,
    descent: &Option<Descent<'_>>,
    raised: usize,
    out: &mut Vec<Diagnostic>,
) {
    if descent.is_some() {
        return;
    }
    let Some(stmt) = head.eval.as_deref() else { return };
    if in_dead(&w.dead.borrow(), head.span.start) {
        return;
    }
    sweep_guard_regions(w, folder, stmt, store);
    let mut found = Vec::new();
    check_position_calls(w, folder, stmt, env, store, &mut found);
    check_read_positions(w, folder, stmt, env, store, &mut found);
    push_unseen(out, raised, found);
}

/// Push each of `found` onto `out` unless a finding with the same id sits at the same
/// place in `out[since..]`.
pub(crate) fn push_unseen(out: &mut Vec<Diagnostic>, since: usize, found: Vec<Diagnostic>) {
    let since = since.min(out.len());
    for d in found {
        let seen = out[since..]
            .iter()
            .any(|o| o.id == d.id && o.line == d.line && o.column == d.column);
        if !seen {
            out.push(d);
        }
    }
}
