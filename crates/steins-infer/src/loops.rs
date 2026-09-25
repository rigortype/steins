//! Structured loops (ADR-0027 amendment, issues #649 to #653): the env a `while`,
//! `for`, `foreach` or `do`-`while` hands its body and the one it leaves for its
//! fall-through, the `foreach` target binding, the header's negation at the exit,
//! and the body walks.

use std::collections::HashMap;

use steins_domain::Certainty;
use steins_syntax::{CondExpr, Stmt};

use crate::fold::Folder;
use crate::annotate::LineFact;
use crate::branch::apply_cond_side;
use crate::cond::eval_cond;
use crate::env::{Descent, Known, Store};
use crate::foreach_bind::element_binding;
use crate::project::Diagnostic;
use crate::walk::{Flow, WalkCx, push_hidden_exit_floor, walk_trace};

/// What a `while` or `for` does to the code after it, given the header's verdict on
/// the entry env (issue #651). That env holds at every header evaluation, so a
/// `Yes` there is a `Yes` at all of them: no test ever fails, and with no jump able
/// to leave the body either, the successor is unreachable — `while (true) { …;
/// return; }` is the `if (true) { return; }` twin `walk_if` already terminates.
/// Anything less than that pair falls through: an undecided header may fail, and a
/// `break` leaves without failing it. A `do`-`while` is not this question (its body
/// runs before any test; issue #679 owns its reachability).
pub(crate) fn loop_flow(break_free: bool, verdict: Certainty) -> Flow {
    if break_free && verdict == Certainty::Yes { Flow::Terminated } else { Flow::FellThrough }
}

/// The env a **structured loop's fall-through** starts in (issue #651) — the same
/// two-set question `forget_construct_sets` answers for an `Opaque`, with the
/// `reads` half decided the other way.
///
/// Forgetting `writes` stays, and for the reason it always had: the by-ref
/// conservatism names everything the body may assign or hand to a call, and any of
/// it may hold something else once the loop is over. Forgetting `reads` was the
/// `Opaque` rule, and its justification is a construct whose control flow the trace
/// does not model — a subtree that reads and branches may have early-returned, so
/// the tail must exclude the value it branched on. A **loop** does not have that
/// shape: a name in `reads` is assigned by nothing in the body and handed to no call
/// in it, so it holds at the fall-through exactly what it held at the construct,
/// under any iteration count including zero. This is [`loop_entry_forget`]'s
/// argument verbatim, and it is as true after the loop as it is inside it.
///
/// Measured, it is what made a `foreach` subject unusable twice: `foreach ($xs as
/// $v) {}` left `$xs` unknown, so a second `foreach` over the same declared subject
/// bound nothing (issue #652's adversarial review). The subject is a read.
///
/// A kept name takes the same [`Store::sweep_object`] the entry gives it, for the
/// same reason: the loop can mutate the object a read name points at even though it
/// cannot rebind the name. `carried` has no counterpart here — a `for`'s `init` is
/// walked into the body's entry env and never into this one, so there is no
/// post-`init` binding at the fall-through to keep.
pub(crate) fn loop_fallthrough_forget(
    w: &WalkCx,
    writes: &[String],
    reads: &[String],
    poisons: bool,
    may_return: bool,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
) {
    if poisons {
        env.clear();
        store.clear();
    } else {
        for v in writes {
            // The handle a written name held may have been closed by the body
            // through THAT name, and its entry outlives the name for every alias
            // (ADR-0097 §2.4) — the resource half of the read names' sweep below.
            store.escape_resource(v);
            env.remove(v);
            store.unbind(v);
        }
        for v in reads {
            store.sweep_object(v);
        }
    }
    push_hidden_exit_floor(w, may_return);
}

/// The env a structured loop's **body** starts each iteration in (issue #653) —
/// what the loop provably cannot change, which is not what its fall-through keeps.
///
/// The construct's fall-through forgets both ADR-0027 sets, and only the `writes`
/// half of that is right for an entry. `writes` is the by-ref conservatism — every
/// name assigned in the subtree and every name handed to any call in it — so a name
/// in it may hold something else on iteration 2 and has to go. `reads` is everything
/// the subtree merely mentions: nothing in the loop assigns such a name and no call
/// in the loop can rebind it, so its binding is the same on iteration 40 as on
/// iteration 1. Forgetting it is right for the fall-through, where a construct that
/// reads and branches may have early-returned, and wrong here — it is what made a
/// method receiver, which is not an argument, arrive unknown, and what cost the
/// subtractive guards (`!== null`, truthiness) the base they subtract from.
///
/// What a kept name may **not** keep is the mutable state of the object it points
/// at: a method call the body makes writes through a receiver that never enters
/// `writes`, so iteration 2 would otherwise read iteration 1's properties. Every
/// object a kept name refers to therefore takes the sweep an escaping call already
/// performs ([`Store::sweep_object`]) — the class and the readonly props survive it,
/// the rest does not. This is why the rule is not simply "forget less".
///
/// `carried` is the `for` header's one exception (issue #650): a name its `init`
/// writes and its condition, increments and body never write again is written once,
/// before the first iteration, so it holds the same binding at every entry to the
/// body. It is in `writes` — `init` is part of the construct — and is kept anyway,
/// under exactly the argument `reads` is kept under, sweep included. Empty for every
/// other loop form, which has no clause that runs once.
///
/// `poisons` clears everything, exactly as it does for the fall-through: a scope
/// that aliases, `extract`s or `eval`s has no binding worth carrying anywhere.
pub(crate) fn loop_entry_forget(
    writes: &[String],
    reads: &[String],
    carried: &[String],
    poisons: bool,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
) {
    if poisons {
        env.clear();
        store.clear();
        return;
    }
    for v in writes {
        if carried.contains(v) {
            store.sweep_object(v);
            continue;
        }
        // Iteration 2 may see a handle iteration 1 closed through this name, and
        // an alias outside `writes` still refers to the entry (ADR-0097 §2.4).
        store.escape_resource(v);
        env.remove(v);
        store.unbind(v);
    }
    for v in reads {
        store.sweep_object(v);
    }
}

/// Bind a `foreach`'s `$k`/`$v` at the body entry from the subject's own element
/// type (issue #652), into the env [`loop_entry_forget`] has just built.
///
/// Applied **after** the forgetting, never instead of it. Both targets are in
/// `writes` — the `foreach`-binding row the write collector has always had — so the
/// forgetting drops them first and this puts back only what the subject supports.
/// That is the whole of the iteration-count-agnosticism: the fact comes from the
/// subject as the entry env holds it, so a body that reassigns `$v` writes into an
/// env this construct discards, and the next entry re-derives from the subject.
///
/// The subject is read from that same entry env, which is why a subject the body
/// rebinds contributes nothing: it is in `writes` too, and is gone by the time this
/// looks for it. A `poisons` scope has an empty env here for the same reason, so
/// the lookup fails and nothing binds.
///
/// **Refusals**, both silent — the target keeps the defined-but-untyped binding the
/// forgetting left it with:
///
/// * `by_ref` (`as &$v`) — the loop writes *through* the subject, and the trace
///   models neither that write nor the alias it installs (issue #677). Typing `$v`
///   from the subject would let iteration 2 read an element iteration 1 overwrote.
/// * a target that is not a plain variable — a destructuring (`as [$a, $b]`) or a
///   property/offset target (`as $this->x`) binds names this statement does not
///   name, so there is nothing to bind and it stays a write alone.
#[allow(clippy::too_many_arguments)]
pub(crate) fn bind_foreach_targets(
    w: &WalkCx,
    stmt: &Stmt,
    subject: Option<&str>,
    key_var: Option<&str>,
    value_var: Option<&str>,
    by_ref: bool,
    benv: &mut HashMap<String, Known>,
    bstore: &mut Store,
) {
    if by_ref {
        return;
    }
    let Some(subject) = subject else { return };
    let binding = element_binding(benv.get(subject), bstore.contract_arms(subject));
    let line = w.cx.tree().position(stmt.span.start).line;
    // `foreach ($xs as $x => $x)` assigns the key first and the value last, so the
    // name holds the value; binding the key too would leave the env and the arm
    // lane disagreeing about one variable.
    if let Some(name) = key_var
        && key_var != value_var
        && let Some(known) = binding.key_known(line)
    {
        benv.insert(name.to_owned(), known);
    }
    let Some(name) = value_var else { return };
    if let Some(known) = binding.value_known(line) {
        benv.insert(name.to_owned(), known);
    }
    // The declared lane too, and for the one thing the value lane cannot hold: a
    // class element (`array<string, Foo>`) has no `Fact` to be (ADR-0035/0043), so
    // `$v: Foo` exists only as an arm.
    if let Some(arms) = binding.value_arms {
        bstore.contract.insert(name.to_owned(), arms);
    }
}

/// Apply a break-free loop's **negated header** to its fall-through env
/// (issue #651) — the mirror of the entry narrowing, and the same carrier.
///
/// A loop is left in exactly two ways: its condition went false, or a jump left the
/// body. `break_free` is the lowering's answer to whether the second is possible
/// (`StmtKind::While`'s docs carry the rule), and when it is not, the condition was
/// evaluated **false immediately before** the statement that follows — with nothing
/// running in between. That is a stronger position than an `if`'s else-branch, which
/// takes the identical application for the identical reason, so [`apply_cond_side`]
/// is used verbatim at the same strata (ADR-0052 §5: a `Verified` test refines at
/// `Verified`, an `Asserted` envelope at `Asserted`, and nothing launders).
///
/// **It is applied to the post-forget env**, after [`loop_fallthrough_forget`], and
/// that order is the soundness. The loop may have rewritten the very name the header
/// tests, so the negation may not be read as refining what the name held *before*
/// the loop; it refines what it holds *now*, which is precisely the value whose
/// failing test ended the loop. Both halves of that fall out of the order: a name
/// the loop wrote arrives here with its lanes gone, so only a refinement that mints
/// its own fact (`while ($x !== null)` proving `$x === null`) can say anything about
/// it, and a name the loop did not write arrives with its lanes intact, so a
/// subtractive negation (`while ($x instanceof Node)`) has the base it subtracts
/// from. Neither reading can state iteration 1's value as the exit's.
///
/// A `CondExpr::Opaque` — a `for (;;)`, or any header the lowering could not read —
/// refines nothing and needs no gate of its own: it is the same inert value at the
/// exit that it is at the entry.
pub(crate) fn apply_loop_exit_negation(
    w: &WalkCx,
    folder: &mut dyn Folder,
    cond: &CondExpr,
    break_free: bool,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
) {
    if !break_free {
        return;
    }
    apply_cond_side(w, folder, cond, false, env, store);
}

/// Walk a structured `while` body (ADR-0027 amendment, issue #649).
///
/// `benv`/`bstore` are the body's **entry** pair, built by the caller from what the
/// loop provably cannot change (`loop_entry_forget`), and this consumes them: a
/// loop body contributes **findings**, never facts. The body's exit env is
/// discarded, so the code after the loop sees what the construct's own sets left
/// standing, plus the one thing the body did not compute: the negated header, when
/// no jump can leave the body ([`apply_loop_exit_negation`], issue #651).
///
/// The entry env needs no fixpoint. Nothing in it is specific to one iteration —
/// every name the loop can rebind is forgotten in it and the mutable state of every
/// object it still names has been swept — and PHP evaluates the header before
/// **every** entry to the body, the first included, so the true-side application is
/// exactly as sound here as it is on an `if`'s then-branch. A body whose last
/// statement reassigns the subject the header narrowed — the parent-pointer
/// traversal that motivated the slice — is therefore no obstacle: the next
/// iteration's entry re-derives the fact from the header.
///
/// A header the walk decides is false runs its body zero times, so the body is not
/// walked. That reading is taken in the entry env for the same reason the narrowing
/// is: the env holds at every evaluation of the header, so a `No` there is a `No`
/// at all of them. The region is not marked dead: the env-free direct pass reports
/// there today, and withdrawing those findings is a separate judgment from adding
/// these.
#[allow(clippy::too_many_arguments)]
pub(crate) fn walk_while_body(
    w: &WalkCx,
    folder: &mut dyn Folder,
    cond: &CondExpr,
    body: &[Stmt],
    mut benv: HashMap<String, Known>,
    mut bstore: Store,
    descent: &mut Option<Descent<'_>>,
    facts: &mut Option<&mut Vec<LineFact>>,
    out: &mut Vec<Diagnostic>,
) -> Certainty {
    // The verdict is on the ENTRY env, which holds at every header evaluation, so
    // it is the verdict of every test the loop ever makes: `No` skips the body,
    // and `Yes` is what the caller reads as "no failing test can ever leave this
    // loop" (issue #651's reachability half).
    let verdict = eval_cond(w, folder, cond, &benv, &bstore, w.scope.poisoned);
    if verdict == Certainty::No {
        return verdict;
    }
    apply_cond_side(w, folder, cond, true, &mut benv, &mut bstore);
    walk_loop_body(w, folder, body, benv, bstore, descent, facts, out);
    verdict
}

/// Walk a structured loop body from an entry pair the header has already had its
/// say over — the half of [`walk_while_body`] that is not the header.
///
/// It is also the WHOLE of what a `foreach` and a `do`-`while` get (issue #650),
/// and for opposite reasons. A `foreach` header binds rather than tests, so there is
/// no condition to narrow by. A `do`-`while` has one and may not use it: the body's
/// first iteration runs before it is ever evaluated, so narrowing the entry by it
/// would state an untested fact, and reading it as false would skip a body that runs
/// exactly once. Neither may take the `while` treatment, and neither loses anything
/// else by it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn walk_loop_body(
    w: &WalkCx,
    folder: &mut dyn Folder,
    body: &[Stmt],
    mut benv: HashMap<String, Known>,
    mut bstore: Store,
    descent: &mut Option<Descent<'_>>,
    facts: &mut Option<&mut Vec<LineFact>>,
    out: &mut Vec<Diagnostic>,
) {
    // The body's own `Flow` is discarded: a body that terminates on every path
    // terminates an ITERATION, and a `while`, `for` or `foreach` whose condition is
    // not decided may run none at all, so their successor stays reachable either
    // way. A `do`-`while` body runs at least once, so there the discard is a
    // widening — a successor the body provably never reaches is still walked
    // (issue #679); it never under-reports, and it is the one caller for which
    // the reasoning above does not hold.
    let _ = walk_trace(w, folder, body, &mut benv, &mut bstore, descent, facts, true, out);
}
