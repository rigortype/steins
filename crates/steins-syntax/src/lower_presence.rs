//! Binding presence (ADR-0081, issue #267): the flow-sensitive bound / maybe-bound
//! state of locals across a body, the `variable.maybe-undefined` reads it yields,
//! and the `unset` pseudo-type seed facts (ADR-0087 §4).

use std::collections::HashMap;
use std::collections::HashSet;

use mago_span::HasSpan;
use mago_syntax::cst::{
    Access, Argument, BinaryOperator, Call, Construct, Expression, Node, Statement,
    UnaryPrefixOperator, Variable,
};

use crate::ast::{Comment, CommentKind, UndefinedRead, UnsetSeedFacts, UnsetSeedRead};
use crate::lower_scope::{Shield, VarUsage, bind_lvalue_roots, scan_var_usage};
use crate::lower_jump::parked_level;
use crate::lower_stmt::{body_has_nested_jumps_only, expr_is_false, expr_is_true, stmt_end};
use crate::lower_try::{stmt_cannot_throw, try_body_cannot_throw};
use crate::memo::{self, TryScan};
use crate::{bytes_to_string, strip_dollar, to_span};

mod loop_memo;

use loop_memo::{LoopMemo, presence_loop_body};

// binding presence (ADR-0081, issue #267)

/// Whether a name carries a binding at a program point, over the three-valued
/// lattice ADR-0081 §2 fixes: `Bound ⊔ Unbound = Maybe`.
///
/// This **mirrors** [`steins_domain::Presence`]'s documented join rather than
/// reusing the type: that enum's `Required` arm carries a `witnessed` provenance
/// bit (the Verified/Asserted split of the array-shape engine) that a local binding
/// has no stratum for — a parameter and an assignment bind identically. The join
/// is the same relation with the same three outcomes: agreement survives,
/// disagreement degrades to the middle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum BindingPresence {
    /// Every path from scope entry to this point carries a binding.
    Bound,
    /// Some paths carry a binding and some do not.
    Maybe,
    /// No path carries a binding.
    Unbound,
}

impl BindingPresence {
    /// The join of two paths meeting at a control-flow merge.
    fn join(self, other: Self) -> Self {
        match (self, other) {
            (Self::Bound, Self::Bound) => Self::Bound,
            (Self::Unbound, Self::Unbound) => Self::Unbound,
            _ => Self::Maybe,
        }
    }
}

/// The presence state at one program point. A name absent from the map is
/// [`BindingPresence::Unbound`] — scope entry is the empty map plus the
/// parameters and captures, which is exactly PHP's own frame.
type PresenceState = HashMap<String, BindingPresence>;

/// Join two states at a control-flow merge, over the union of their names.
fn join_states(a: &PresenceState, b: &PresenceState) -> PresenceState {
    let mut out = PresenceState::with_capacity(a.len().max(b.len()));
    for (name, pa) in a {
        let pb = b.get(name).copied().unwrap_or(BindingPresence::Unbound);
        out.insert(name.clone(), pa.join(pb));
    }
    for (name, pb) in b {
        if !a.contains_key(name) {
            out.insert(name.clone(), pb.join(BindingPresence::Unbound));
        }
    }
    out
}

/// Refine a state by a boundness guard: every listed name is [`BindingPresence::Bound`]
/// on this continuation. Guards only ever refine *toward* boundness (ADR-0081 §5) —
/// `isset` is false on a bound null, so no polarity can prove absence.
fn refine_bound(state: &mut PresenceState, names: &[String]) {
    for name in names {
        state.insert(name.clone(), BindingPresence::Bound);
    }
}

/// Where control leaves a statement list, at the granularity the presence pass
/// needs. Deliberately finer than [`BodyEnd`]: the three ways out land in three
/// different places, and only one is the enclosing statement's successor:
///
/// * a `break` leaves for the enclosing **loop or switch**'s successor — a `switch`
///   arm ending in `break` stays in the switch's arm join (`switch { case 1: $x =
///   1; break; default: $x = 2; break; }` binds on every arm), while an `if` arm
///   ending in `break` does **not** reach the `if`'s successor;
/// * a `continue` leaves for the enclosing loop's **back edge**;
/// * `return`/`throw`/`exit` leave the scope entirely.
///
/// The first two are not lost: [`PresenceCx`] collects them, and the enclosing
/// loop or switch joins them where they actually arrive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PresenceFlow {
    /// Control reached the end of the list.
    Fell,
    /// A `break` left for the enclosing loop or switch's successor.
    Broke,
    /// A `continue` left for the enclosing loop's back edge.
    Continued,
    /// `return`/`throw`/`exit`: the list's state reaches no successor at all, and
    /// this is the arm ADR-0081 §3 subtracts from a branch join.
    Terminated,
}

/// The accumulator of the presence pass: the reads it judged, and the premises it
/// judges them against.
struct PresenceCx<'a> {
    /// The names this run may report a read of, and the only premise that differs
    /// between the pass's two consumers.
    ///
    /// For `variable.maybe-undefined` it is the scope's whole binding set — the
    /// definite pass's `bound` — so that a read of a name the scope binds nowhere is
    /// `variable.undefined`'s and never this id's (ADR-0081 §6): what makes the pair
    /// disjoint by construction. For the `unset` pseudo-type (ADR-0087 §4) it is the
    /// declared names, whose *declaration* is the premise instead.
    reportable: &'a HashSet<String>,
    /// Where the `unset` pseudo-type's declarations sit, or `None` for the
    /// ADR-0081 run, which seeds nothing beyond scope entry.
    seeds: Option<&'a SeedIndex<'a>>,
    /// The statement whose docblock last seeded each name, so a candidate read can
    /// name the declaration the confirming reader must lower.
    seeded_at: HashMap<String, u32>,
    /// Reads whose presence was `Maybe` or `Unbound`, each with the seeding statement
    /// [`Self::seeded_at`] held for it — `None` on the ADR-0081 run, which has no
    /// declaration behind it.
    out: Vec<(UndefinedRead, Option<u32>)>,
    /// Set while a loop body is being walked for its fixpoint rather than for its
    /// findings: the state is not yet stable, so nothing may be reported from it.
    silent: bool,
    /// Read spans already reported — a loop body is walked more than once.
    seen: HashSet<u32>,
    /// The states carried out by `break`, each with the number of loops and
    /// `switch`es it still has to leave (ADR-0103), waiting for the construct it
    /// targets to join it into its successor. Saved and cleared around each such
    /// construct, which takes the level-1 entries as its own and re-parks the rest
    /// one level lower ([`credit_level`]), so a jump is never credited to the wrong
    /// one.
    breaks: Vec<Parked>,
    /// The states carried out by `continue`, levelled the same way, waiting for the
    /// targeted loop's back edge — and, for a loop that can exit by its condition,
    /// its successor too. A `switch` a `continue` targets takes it as a `break`.
    continues: Vec<Parked>,
    /// The loop-body answers this run has already computed (issue #793), or `None`
    /// when memoization is off. See [`LoopMemo`].
    loop_memo: Option<LoopMemo>,
}

/// A parked jump state: the number of breakable constructs (loops and `switch`es)
/// the jump still has to leave, counting the one it is in, and the state it carries.
type Parked = (u32, PresenceState);

/// Split the jumps parked inside one breakable construct into the states it takes
/// as its own (level 1) and those that leave it for an enclosing construct, one
/// level lower — the module table of `crate::lower_jump`, applied as each
/// construct is left.
fn credit_level(parked: Vec<Parked>) -> (Vec<PresenceState>, Vec<Parked>) {
    let mut own = Vec::new();
    let mut outer = Vec::new();
    for (level, state) in parked {
        if level <= 1 {
            own.push(state);
        } else {
            outer.push((level - 1, state));
        }
    }
    (own, outer)
}

impl PresenceCx<'_> {
    fn record(&mut self, read: &UndefinedRead, state: &PresenceState) {
        if self.silent || !self.reportable.contains(&read.name) {
            return;
        }
        let presence =
            state.get(&read.name).copied().unwrap_or(BindingPresence::Unbound);
        if presence == BindingPresence::Bound {
            return;
        }
        if self.seen.insert(read.span.start) {
            let seed = self.seeded_at.get(&read.name).copied();
            self.out.push((read.clone(), seed));
        }
    }
}

/// Judge one **leaf unit** — a statement with no control-flow structure of its
/// own, or a branch condition — against `state`, then apply its bindings.
///
/// Reads are judged against the *pre-unit* state and bindings applied after, which
/// is what makes `$y = $x; $x = 1;` report at the first statement. **Within** a
/// unit the definite pass's ordering-blindness is kept: a name the unit binds
/// anywhere has its reads in that unit withheld, so `$a = ($b = 1) + $b;` and
/// `$x .= 'a'` cannot manufacture a finding out of an intra-expression evaluation
/// order this pass does not model.
fn presence_leaf(node: &Node<'_, '_>, state: &mut PresenceState, cx: &mut PresenceCx) {
    // The shield-and-scan pair is a pure function of the unit's subtree, and a
    // loop body re-walks its units once per fixpoint round (up to two silent
    // rounds plus the reporting one, compounding under nesting), so the pair is
    // cached per node (issue #484). Only the judgment against the flowing
    // state — which the rounds exist to change — runs per visit. The loop
    // walk above it is cached too, on body and entry state (issue #793).
    let leaf = memo::presence_leaf(node, || {
        let mut acc = VarUsage::default();
        let mut names = Vec::new();
        collect_presence_shield(node, &mut names);
        let shield: Vec<Shield> = names.into_iter().map(Shield::plain).collect();
        scan_var_usage(node, false, &shield, &mut acc);
        // The unit-wide shield above is plain for every name an `isset`/`empty` in the
        // unit tests, which subsumes any disjunctive shield the scan raised, so nothing is
        // parked; settling keeps that true if the two ever diverge.
        acc.settle();
        memo::PresenceLeaf { reads: acc.reads, bound: acc.bound }
    });
    for read in &leaf.reads {
        if leaf.bound.contains(&read.name) {
            continue;
        }
        cx.record(read, state);
    }
    for name in &leaf.bound {
        state.insert(name.clone(), BindingPresence::Bound);
    }
}

/// Every name an `isset`/`empty` construct anywhere in this unit tests — the shield
/// the presence pass casts over the *whole* unit.
///
/// [`guard_tested_names`] is deliberately shallow (it reads a condition's top-level
/// spelling only), which is right for the definite pass: there, a read of a name the
/// scope never binds is a finding wherever it sits. Here it is not enough —
/// `if (isset($x) && $x > 1)` reaches the `$x > 1` read only when `$x` is bound, and
/// judging the condition as one unit against the pre-`if` state would report it.
/// Over-shielding costs recall and never manufactures a finding, which is the same
/// trade `guard_tested_names` already documents.
fn collect_presence_shield(node: &Node<'_, '_>, out: &mut Vec<String>) {
    match node {
        Node::Function(_)
        | Node::Closure(_)
        | Node::ArrowFunction(_)
        | Node::AnonymousClass(_)
        | Node::Class(_)
        | Node::Interface(_)
        | Node::Trait(_)
        | Node::Enum(_) => return,
        Node::IssetConstruct(_) | Node::EmptyConstruct(_) => {
            collect_direct_variable_names(node, out);
            return;
        }
        _ => {}
    }
    for child in node.children() {
        collect_presence_shield(&child, out);
    }
}

/// Every `$name` token in a subtree, without descending into a nested scope.
fn collect_direct_variable_names(node: &Node<'_, '_>, out: &mut Vec<String>) {
    match node {
        Node::Function(_)
        | Node::Closure(_)
        | Node::ArrowFunction(_)
        | Node::AnonymousClass(_)
        | Node::Class(_)
        | Node::Interface(_)
        | Node::Trait(_)
        | Node::Enum(_) => return,
        Node::DirectVariable(dv) => out.push(strip_dollar(bytes_to_string(dv.name))),
        _ => {}
    }
    for child in node.children() {
        collect_direct_variable_names(&child, out);
    }
}

/// The names a condition **proves bound** on one of its continuations
/// (ADR-0081 §5).
///
/// `want_true` asks for the true-continuation. `isset($x)` proves `$x` bound when
/// true; `empty($x)` and `!isset($x)` prove it bound when **false** — which is the
/// whole reason the defaulting idiom `if (!isset($x)) { $x = f(); } use($x);` is
/// silent: the then-arm binds and the implicit else-arm holds `isset($x)`.
///
/// `&&` conjoins on the true side (both operands hold), `||` on the false side
/// (neither holds). No polarity ever refines toward absence: `isset` is false on a
/// bound null, so a false `isset($x)` proves nothing about the binding.
fn guard_bound_names(cond: &Expression<'_>, want_true: bool, out: &mut Vec<String>) {
    match cond.unparenthesized() {
        Expression::Construct(Construct::Isset(i)) if want_true => {
            for value in i.values.iter() {
                push_guard_root(value, out);
            }
        }
        Expression::Construct(Construct::Empty(e)) if !want_true => {
            push_guard_root(e.value, out);
        }
        Expression::UnaryPrefix(up) if matches!(up.operator, UnaryPrefixOperator::Not(_)) => {
            guard_bound_names(up.operand, !want_true, out);
        }
        Expression::Binary(b)
            if want_true
                && matches!(b.operator, BinaryOperator::And(_) | BinaryOperator::LowAnd(_)) =>
        {
            guard_bound_names(b.lhs, true, out);
            guard_bound_names(b.rhs, true, out);
        }
        Expression::Binary(b)
            if !want_true
                && matches!(b.operator, BinaryOperator::Or(_) | BinaryOperator::LowOr(_)) =>
        {
            guard_bound_names(b.lhs, false, out);
            guard_bound_names(b.rhs, false, out);
        }
        _ => {}
    }
}

/// The **root local** a guarded expression reads through: `$x`, `$x['a']['b']`,
/// `$x->p` and `$x->p['k']` all reach `x`, and nothing else does.
///
/// `isset($info['subject']['commonName'])` cannot be true unless `$info` is bound
/// (witnessed: PHP evaluates the whole chain and answers false at the first missing
/// link, without warning), so the root is exactly as guarded as a bare `isset($x)`
/// would make it. This is the shape the corpus produces far more often than the bare
/// one — reading only the bare spelling reported every read after the
/// `if (!isset($info['subject']['commonName'])) { return null; }` prologue.
pub(crate) fn push_guard_root(expr: &Expression<'_>, out: &mut Vec<String>) {
    match expr.unparenthesized() {
        Expression::Variable(Variable::Direct(dv)) => {
            out.push(strip_dollar(bytes_to_string(dv.name)));
        }
        Expression::ArrayAccess(aa) => push_guard_root(aa.array, out),
        Expression::Access(Access::Property(pa)) => push_guard_root(pa.object, out),
        Expression::Access(Access::NullSafeProperty(pa)) => push_guard_root(pa.object, out),
        _ => {}
    }
}

fn guarded_names(cond: &Expression<'_>, want_true: bool) -> Vec<String> {
    let mut out = Vec::new();
    guard_bound_names(cond, want_true, &mut out);
    out
}

/// Walk an ordered statement list, threading `state` through it, and answer where
/// control left it. The scan stops at the first statement that does not fall
/// through — everything after it is unreachable, and judging unreachable reads
/// against a state no execution has would be a claim about nothing.
fn presence_seq<'ast, 'arena>(
    stmts: impl IntoIterator<Item = &'ast Statement<'arena>>,
    state: &mut PresenceState,
    cx: &mut PresenceCx,
) -> PresenceFlow
where
    'arena: 'ast,
{
    for s in stmts {
        match presence_stmt(s, state, cx) {
            PresenceFlow::Fell => {}
            other => return other,
        }
    }
    PresenceFlow::Fell
}

/// The presence transfer for a `do`-`while`. The body runs at least once, so there
/// is no zero-iteration path to join in — but the condition can still be false, so
/// the back edge does reach the successor.
///
/// Unless nothing reaches the back edge (issue #679): a body whose every path ends
/// in `return`/`throw`/`exit`, with no `break` and no `continue` of this loop,
/// never evaluates the condition and never reaches the successor, so the construct
/// answers [`PresenceFlow::Terminated`] and a branch join subtracts it like a
/// `return`. The lowering's [`body_has_nested_jumps_only`] gates it as it gates the
/// walker's `do_while_flow`; it still refuses multi-level jumps, which this pass has
/// credited by level since ADR-0103 (relaxing it is #1033 S2b).
fn presence_do_while(
    d: &mago_syntax::cst::DoWhile<'_>,
    state: &mut PresenceState,
    cx: &mut PresenceCx,
) -> PresenceFlow {
    let body = std::slice::from_ref(d.statement);
    let entry = state.clone();
    let exits = presence_loop_body(body, &entry, cx);
    let terminates =
        exits.broke.is_empty() && !exits.reaches_back_edge && body_has_nested_jumps_only(body);
    *state = join_loop_exit(&entry, exits, true);
    if terminates {
        return PresenceFlow::Terminated;
    }
    presence_leaf(&Node::Expression(d.condition), state, cx);
    PresenceFlow::Fell
}

/// The presence transfer function for one statement.
fn presence_stmt(
    s: &Statement<'_>,
    state: &mut PresenceState,
    cx: &mut PresenceCx,
) -> PresenceFlow {
    apply_presence_seeds(s, state, cx);
    match s {
        Statement::Break(b) => {
            cx.breaks.push((parked_level(b.level), state.clone()));
            PresenceFlow::Broke
        }
        Statement::Continue(c) => {
            cx.continues.push((parked_level(c.level), state.clone()));
            PresenceFlow::Continued
        }
        Statement::Block(b) => presence_seq(b.statements.iter(), state, cx),
        Statement::If(i) => presence_if(i, state, cx),
        Statement::Switch(sw) => presence_switch(sw, state, cx),
        Statement::While(w) => {
            presence_leaf(&Node::Expression(w.condition), state, cx);
            let mut entry = state.clone();
            refine_bound(&mut entry, &guarded_names(w.condition, true));
            let exits = presence_loop_body(w.body.statements(), &entry, cx);
            if expr_is_true(w.condition) {
                // No false-condition exit edge: zero iterations is impossible and
                // the back edge never leaves, so only a `break` reaches the
                // successor.
                *state = join_loop_exit(&entry, exits, false);
            } else {
                let after = join_loop_exit(&entry, exits, true);
                *state = join_states(state, &after);
                refine_bound(state, &guarded_names(w.condition, false));
            }
            PresenceFlow::Fell
        }
        Statement::DoWhile(d) => presence_do_while(d, state, cx),
        Statement::For(f) => {
            for init in f.initializations.iter() {
                presence_leaf(&Node::Expression(init), state, cx);
            }
            for cond in f.conditions.iter() {
                presence_leaf(&Node::Expression(cond), state, cx);
            }
            let last = f.conditions.iter().next_back();
            let mut entry = state.clone();
            if let Some(cond) = last {
                refine_bound(&mut entry, &guarded_names(cond, true));
            }
            let mut exits = presence_loop_body(f.body.statements(), &entry, cx);
            // The increments run on the back edge, not on a `break`.
            for inc in f.increments.iter() {
                presence_leaf(&Node::Expression(inc), &mut exits.looped, cx);
            }
            if last.is_none_or(|c| expr_is_true(c)) {
                *state = join_loop_exit(&entry, exits, false);
            } else {
                let after = join_loop_exit(&entry, exits, true);
                *state = join_states(state, &after);
            }
            PresenceFlow::Fell
        }
        Statement::Foreach(fe) => {
            presence_leaf(&Node::Expression(fe.expression), state, cx);
            let mut entry = state.clone();
            let mut targets = VarUsage::default();
            match &fe.target {
                mago_syntax::cst::ForeachTarget::Value(t) => {
                    bind_lvalue_roots(t.value, &mut targets);
                }
                mago_syntax::cst::ForeachTarget::KeyValue(t) => {
                    bind_lvalue_roots(t.key, &mut targets);
                    bind_lvalue_roots(t.value, &mut targets);
                }
            }
            for name in targets.bound {
                entry.insert(name, BindingPresence::Bound);
            }
            let exits = presence_loop_body(fe.body.statements(), &entry, cx);
            let after = join_loop_exit(&entry, exits, true);
            // Zero iterations is always possible over an empty subject — which is
            // exactly why `foreach ($xs as $v) {} echo $v;` is this id and not
            // silence.
            *state = join_states(state, &after);
            PresenceFlow::Fell
        }
        Statement::Try(t) => presence_try(s, t, state, cx),
        _ => {
            presence_leaf(&Node::Statement(s), state, cx);
            refine_bound(state, &assert_bound_names(s));
            if stmt_end(s).provably_terminates() {
                PresenceFlow::Terminated
            } else {
                PresenceFlow::Fell
            }
        }
    }
}

/// The names a statement-position `assert()` proves bound in everything after it.
///
/// `assert(isset($x));` is a boundness guard whose *only* continuation is the
/// true-polarity one: ADR-0052 slice I0 reads `assert()` as Verified evidence, and
/// with assertions enabled a failed one throws `AssertionError` (witnessed at 8.5.9
/// under `zend.assertions=1`), so control reaches the next statement exactly when
/// the condition held. With assertions compiled out the call doesn't run, so the
/// refinement cannot manufacture a claim either way.
///
/// The polarity is [`guard_bound_names`]'s own: `assert(isset($x) && $x > 1)`
/// refines through the conjunction, `assert(!isset($x))` refines nothing.
fn assert_bound_names(s: &Statement<'_>) -> Vec<String> {
    let Statement::Expression(es) = s else {
        return Vec::new();
    };
    let Expression::Call(Call::Function(fc)) = es.expression.unparenthesized() else {
        return Vec::new();
    };
    let Expression::Identifier(id) = fc.function else {
        return Vec::new();
    };
    if !bytes_to_string(id.last_segment()).eq_ignore_ascii_case("assert") {
        return Vec::new();
    }
    // The first argument is the condition; a second is the description, which
    // asserts nothing. A named or spread argument is not this shape.
    let Some(Argument::Positional(first)) = fc.argument_list.arguments.iter().next() else {
        return Vec::new();
    };
    if first.ellipsis.is_some() {
        return Vec::new();
    }
    guarded_names(first.value, true)
}

/// The `if`/`elseif`/`else` chain: each arm is evaluated from the pre-branch state
/// refined by its own guard polarity, and the arms that reach the successor are
/// joined. **An arm that provably terminates drops out of the join** — ADR-0081 §3,
/// and [`BodyEnd::provably_terminates`]'s first production consumer, reached here
/// through [`PresenceFlow`] so that a `break` is not mistaken for a terminator.
///
/// Conditions are evaluated in written order against the running state, so an
/// `elseif` reads what the preceding conditions bound, and every preceding
/// condition's *false* polarity is in force by the time it is judged.
fn presence_if(
    i: &mago_syntax::cst::If<'_>,
    state: &mut PresenceState,
    cx: &mut PresenceCx,
) -> PresenceFlow {
    let body = &i.body;
    let mut chain: Vec<(&Expression<'_>, &[Statement<'_>])> =
        vec![(i.condition, body.statements())];
    chain.extend(body.else_if_clauses());

    let mut arms: Vec<PresenceState> = Vec::new();
    let mut open = true;
    for (cond, stmts) in chain {
        presence_leaf(&Node::Expression(cond), state, cx);
        if expr_is_false(cond) {
            // The arm can never be taken; it contributes no path (`if_end`'s rule).
            continue;
        }
        let mut arm = state.clone();
        refine_bound(&mut arm, &guarded_names(cond, true));
        // Only a fall-through arm reaches THIS statement's successor. A `break` or
        // `continue` arm leaves for an enclosing construct, which joins its state
        // where it actually arrives — keeping it here is what would report
        // `foreach (…) { if (A) { $p = …; } elseif (B) { $p = …; } else { continue; }
        // use($p); }`, the shape the corpus produces most.
        if presence_seq(stmts.iter(), &mut arm, cx) == PresenceFlow::Fell {
            arms.push(arm);
        }
        if expr_is_true(cond) {
            // Always taken: no later arm and no no-branch path exist.
            open = false;
            break;
        }
        refine_bound(state, &guarded_names(cond, false));
    }
    if open {
        match body.else_statements() {
            Some(stmts) => {
                let mut arm = state.clone();
                if presence_seq(stmts.iter(), &mut arm, cx) == PresenceFlow::Fell {
                    arms.push(arm);
                }
            }
            // No `else`: the no-branch-taken path runs straight to the successor,
            // carrying whatever the conditions' false polarities established.
            None => arms.push(state.clone()),
        }
    }
    let Some(joined) = arms.into_iter().reduce(|a, b| join_states(&a, &b)) else {
        // No arm falls through. Whatever each one did — terminate or jump — nothing
        // after this `if` in this list runs, and the jump states are already parked
        // on the enclosing construct.
        return PresenceFlow::Terminated;
    };
    *state = joined;
    PresenceFlow::Fell
}

/// A `switch`: every non-empty case body is evaluated from the pre-switch state and
/// the surviving arms are joined, with the implicit no-match arm joined in when
/// there is no `default`.
///
/// Case entry is deliberately the **pre-switch** state, not the previous case's
/// exit: PHP enters a case directly on a match, so a name the previous case bound
/// is genuinely absent there. Fall-through only ever *adds* a path.
///
/// The jumps that target this switch (ADR-0103) are joined in: every `break` and
/// `continue` whose level runs out here — a `continue` that targets a `switch` is
/// its `break` — and the jumps that target an enclosing construct are re-parked
/// one level lower. The last non-empty case running off its end leaves the switch
/// as a `break` would, so its state joins too.
fn presence_switch(
    sw: &mago_syntax::cst::Switch<'_>,
    state: &mut PresenceState,
    cx: &mut PresenceCx,
) -> PresenceFlow {
    presence_leaf(&Node::Expression(sw.expression), state, cx);
    let pre = state.clone();
    let outer_breaks = std::mem::take(&mut cx.breaks);
    let outer_continues = std::mem::take(&mut cx.continues);
    let mut arms: Vec<PresenceState> = Vec::new();
    let mut has_default = false;
    let cases = sw.body.cases();
    let last_body = cases.iter().rposition(|c| !c.is_empty());
    for (position, case) in cases.iter().enumerate() {
        match case.expression() {
            Some(e) => {
                let mut probe = pre.clone();
                presence_leaf(&Node::Expression(e), &mut probe, cx);
            }
            None => has_default = true,
        }
        if case.is_empty() {
            // `case 1: case 2: body` — an empty label contributes no arm of its own.
            continue;
        }
        let mut arm = pre.clone();
        // A case body that falls off its end runs into the NEXT case rather than
        // past the switch, so only its `break` state — already parked — reaches the
        // successor. Keeping the fall-off state here would be the fall-through edge
        // this pass does not model. The last body has no next case to run into.
        let flow = presence_seq(case.statements().iter(), &mut arm, cx);
        if flow == PresenceFlow::Fell && Some(position) == last_body {
            arms.push(arm);
        }
    }
    if !has_default {
        arms.push(pre);
    }
    let (own_breaks, escaped_breaks) =
        credit_level(std::mem::replace(&mut cx.breaks, outer_breaks));
    let (own_continues, escaped_continues) =
        credit_level(std::mem::replace(&mut cx.continues, outer_continues));
    cx.breaks.extend(escaped_breaks);
    cx.continues.extend(escaped_continues);
    arms.extend(own_breaks);
    arms.extend(own_continues);
    let Some(joined) = arms.into_iter().reduce(|a, b| join_states(&a, &b)) else {
        return PresenceFlow::Terminated;
    };
    *state = joined;
    PresenceFlow::Fell
}

/// `try`/`catch`/`finally`, conservative in the one direction that matters
/// (ADR-0081 §4).
///
/// The `try` block may throw **at any point**, so a `catch` arm is entered with the
/// pre-`try` state joined with the block's exit state: a name the block binds is
/// `Maybe` there, never `Bound`. The normal-completion path keeps the block's own
/// exit state — weakening that too would report `try { $x = f(); } catch (E $e) {
/// $x = 0; } echo $x;`, where every path does bind. `finally` runs on every path,
/// so its bindings apply unconditionally while its reads are judged against the
/// weakened state.
///
/// Where control leaves is `try_end`'s rule (the ADR-0027 `try` amendment, issue
/// #943): a block no statement of which can throw has no live `catch`
/// ([`try_body_cannot_throw`]), and the construct is [`PresenceFlow::Terminated`]
/// when its `finally` terminates or no live arm falls through. A `goto` or a label
/// in it keeps the successor reachable, as it keeps `try_end` undecided.
///
/// A `break` or `continue` out of the block or a `catch` runs the `finally` before
/// it lands (ADR-0103), so the states parked since the construct began take its
/// bindings as the normal-completion path does.
fn presence_try(
    s: &Statement<'_>,
    t: &mago_syntax::cst::Try<'_>,
    state: &mut PresenceState,
    cx: &mut PresenceCx,
) -> PresenceFlow {
    let parked_from = (cx.breaks.len(), cx.continues.len());
    let mut certain = state.clone();
    let mut block_flow = PresenceFlow::Fell;
    let mut rest = t.block.statements.iter().peekable();
    // The prologue that cannot throw. `$count = 0;` at the head of a `try` runs
    // before anything can go wrong, so a `catch` arm must not be entered with it
    // undone — the shape that reported `try { $count = 0; foreach (…) {…} } catch
    // (…) {…} if (1 < $count)`, where every path does bind.
    while let Some(s) = rest.peek() {
        if !stmt_cannot_throw(s) {
            break;
        }
        let s = rest.next().expect("peeked");
        block_flow = presence_stmt(s, &mut certain, cx);
        if block_flow != PresenceFlow::Fell {
            break;
        }
    }
    let pre = certain;
    let mut block = pre.clone();
    if block_flow == PresenceFlow::Fell {
        block_flow = presence_seq(rest, &mut block, cx);
    }
    let thrown = join_states(&pre, &block);

    let mut arms: Vec<PresenceState> = Vec::new();
    if block_flow == PresenceFlow::Fell {
        arms.push(block);
    }
    let catches_live = !try_body_cannot_throw(t.block.statements.as_slice());
    for clause in t.catch_clauses.iter().filter(|_| catches_live) {
        let mut arm = thrown.clone();
        if let Some(v) = clause.variable.as_ref() {
            arm.insert(strip_dollar(bytes_to_string(v.name)), BindingPresence::Bound);
        }
        if presence_seq(clause.block.statements.iter(), &mut arm, cx) == PresenceFlow::Fell {
            arms.push(arm);
        }
    }
    let mut terminates = arms.is_empty();
    let mut result = arms
        .into_iter()
        .reduce(|a, b| join_states(&a, &b))
        .unwrap_or_else(|| thrown.clone());

    if let Some(f) = t.finally_clause.as_ref() {
        let mut fin = join_states(&thrown, &result);
        if presence_seq(f.block.statements.iter(), &mut fin, cx) == PresenceFlow::Terminated {
            terminates = true;
        }
        let jumped =
            cx.breaks[parked_from.0..].iter_mut().chain(&mut cx.continues[parked_from.1..]);
        for parked in std::iter::once(&mut result).chain(jumped.map(|(_, state)| state)) {
            for (name, presence) in &fin {
                if *presence == BindingPresence::Bound {
                    parked.insert(name.clone(), BindingPresence::Bound);
                }
            }
        }
    }
    *state = result;
    if terminates && !subtree_has_goto(&Node::Statement(s)) {
        return PresenceFlow::Terminated;
    }
    PresenceFlow::Fell
}

/// Walk a loop body to a fixpoint and answer the states that leave it
/// (ADR-0081 §4).
///
/// The body's entry is its own entry joined with everything that reaches the **back
/// edge** — the fall-through end of the body and every `continue`. A prior iteration
/// may have bound a name the first one did not, so a read *earlier* in the body than
/// the binding is `Maybe` rather than `Unbound`. The lattice has height two, so
/// iteration is bounded at two rounds; the rounds that only compute the fixpoint
/// report nothing, since the state they run against is not yet one any execution has.
///
/// The two exits are answered apart because they are reached differently: `looped`
/// is the state at the back edge, which becomes the loop's successor only when the
/// loop can exit by its condition, while `broke` is every `break` state, which
/// reaches the successor unconditionally — including out of a `while (true)`.
#[derive(Clone)]
struct LoopExits {
    /// The state at the back edge: the body's fall-through end joined with every
    /// `continue`.
    looped: PresenceState,
    /// Every `break` state of this loop, in order.
    broke: Vec<PresenceState>,
    /// Whether any path reaches the back edge at all — the body's end, or a
    /// `continue`. `false` means every path left by `break` or terminated, which
    /// is what lets a `do`-`while` terminate (issue #679).
    reaches_back_edge: bool,
    /// The `break`s and `continue`s that leave this loop for an enclosing construct
    /// (ADR-0103), one level lower, for the caller to re-park
    /// ([`loop_memo::presence_loop_body`] does).
    escaped: (Vec<Parked>, Vec<Parked>),
}

/// The walk itself, uncached; [`presence_loop_body`] is the entry the statements
/// call, and answers a repeat walk of the same body from the same entry from a
/// table instead (issue #793).
fn presence_loop_walk(
    body: &[Statement<'_>],
    entry: &PresenceState,
    cx: &mut PresenceCx,
) -> LoopExits {
    // A level-1 jump inside this body targets THIS loop; anything parked by an
    // enclosing one must not be credited to it, and a deeper jump leaves it.
    let outer_breaks = std::mem::take(&mut cx.breaks);
    let outer_continues = std::mem::take(&mut cx.continues);

    let mut body_entry = entry.clone();
    let was_silent = cx.silent;
    cx.silent = true;
    for _ in 0..2 {
        let mut s = body_entry.clone();
        let flow = presence_seq(body.iter(), &mut s, cx);
        let (mut back, _) = credit_level(std::mem::take(&mut cx.continues));
        if flow == PresenceFlow::Fell {
            back.push(s);
        }
        cx.breaks.clear();
        let Some(reached) = back.into_iter().reduce(|a, b| join_states(&a, &b)) else {
            break; // nothing reaches the back edge at all.
        };
        let next = join_states(&body_entry, &reached);
        if next == body_entry {
            break;
        }
        body_entry = next;
    }
    cx.silent = was_silent;
    cx.breaks.clear();
    cx.continues.clear();

    let mut fell = body_entry.clone();
    let flow = presence_seq(body.iter(), &mut fell, cx);
    let (mut back, escaped_continues) =
        credit_level(std::mem::replace(&mut cx.continues, outer_continues));
    if flow == PresenceFlow::Fell {
        back.push(fell);
    }
    let reaches_back_edge = !back.is_empty();
    let looped = back.into_iter().reduce(|a, b| join_states(&a, &b)).unwrap_or(body_entry);
    let (broke, escaped_breaks) = credit_level(std::mem::replace(&mut cx.breaks, outer_breaks));
    LoopExits { looped, broke, reaches_back_edge, escaped: (escaped_breaks, escaped_continues) }
}

/// Join a loop's exits into the state after it. `entry` is folded in for the
/// zero-iteration path and `looped` for the condition-became-false path, both only
/// when the loop can exit that way at all; a `break` always reaches the successor.
fn join_loop_exit(
    entry: &PresenceState,
    exits: LoopExits,
    can_exit_by_condition: bool,
) -> PresenceState {
    let LoopExits { looped, broke, .. } = exits;
    let mut states = broke;
    if can_exit_by_condition {
        states.push(looped);
    } else if states.is_empty() {
        // `while (true)` with no `break`: the successor is unreachable, and the
        // back-edge state is the only honest thing to carry into dead code.
        states.push(looped);
    }
    states.into_iter().reduce(|a, b| join_states(&a, &b)).unwrap_or_else(|| entry.clone())
}

/// The reads whose binding only *some* paths carry (issue #267) — the computation
/// behind [`Scope::maybe_undefined_reads`].
///
/// Every premise of the definite id is inherited: this runs only after
/// [`undefined_variable_reads`] has cleared the scope's name dams, only for
/// function-like scopes (top-level and arrow scopes never reach here), and only
/// over the reads `scan_var_usage` collects — so the guards, the superglobal/`$this`
/// exclusions and the nested-scope boundary are all already settled. `scope_bound`
/// adds the disjointness premise: a name the scope binds nowhere is
/// `variable.undefined`'s, never this id's.
///
/// A `goto` or a label anywhere in the scope dams the pass outright. Every other
/// construct's exit edges are bounded by the traversal above; a jump to an arbitrary
/// label is not, and the honest answer to an unbounded edge is silence.
pub(crate) fn maybe_undefined_reads(
    params: &mago_syntax::cst::FunctionLikeParameterList<'_>,
    use_clause: Option<&mago_syntax::cst::ClosureUseClause<'_>>,
    statements: &[&Statement<'_>],
    scope_bound: &HashSet<String>,
) -> Vec<UndefinedRead> {
    if statements.iter().any(|s| subtree_has_goto(&Node::Statement(s))) {
        return Vec::new();
    }
    let mut state = PresenceState::new();
    for p in params.parameters.iter() {
        state.insert(strip_dollar(bytes_to_string(p.variable.name)), BindingPresence::Bound);
    }
    if let Some(uc) = use_clause {
        for v in uc.variables.iter() {
            state.insert(strip_dollar(bytes_to_string(v.variable.name)), BindingPresence::Bound);
        }
    }
    let mut cx = PresenceCx {
        reportable: scope_bound,
        seeds: None,
        seeded_at: HashMap::new(),
        out: Vec::new(),
        silent: false,
        seen: HashSet::new(),
        breaks: Vec::new(),
        continues: Vec::new(),
        loop_memo: LoopMemo::for_run(),
    };
    for s in statements {
        if presence_stmt(s, &mut state, &mut cx) != PresenceFlow::Fell {
            break;
        }
    }
    let mut out: Vec<UndefinedRead> = cx.out.into_iter().map(|(read, _)| read).collect();
    out.sort_by_key(|r| r.span.start);
    out
}

// unset pseudo-type (ADR-0087 §4, issue #396)

/// Where a declaration re-seeds a name as possibly-unbound, and the text needed to
/// find it: the docblock adoption rule is a byte-offset relation
/// ([`docblock_before`]), so the seeds are keyed by the **docblock's** end offset and
/// looked up from whatever statement adopts it.
struct SeedIndex<'a> {
    comments: &'a [Comment],
    text: &'a str,
    /// Docblock end offset → the names its `@var`-ish tags may declare `T|unset`.
    names: HashMap<u32, Vec<String>>,
}

/// Re-seed every name a statement's adopted docblock declares possibly-unbound.
///
/// The declaration takes effect **at** the adopted statement and regardless of the
/// prior state, because an inline `@var` is a cast: it re-declares what the name
/// holds rather than narrowing it (ADR-0073 §2), and the state it re-declares here
/// is presence. So `$x = new \DateTime(); /** @var \DateTime|unset $x */ echo $x->f();`
/// reports, exactly as the author's own tag asks it to.
///
/// A no-op on the ADR-0081 run, which carries no seeds at all.
fn apply_presence_seeds(s: &Statement<'_>, state: &mut PresenceState, cx: &mut PresenceCx) {
    let Some(seeds) = cx.seeds else { return };
    let start = to_span(s.span()).start;
    let Some(doc) = docblock_before(seeds.comments, seeds.text, start) else { return };
    let Some(names) = seeds.names.get(&doc.span.end) else { return };
    for name in names {
        state.insert(name.clone(), BindingPresence::Maybe);
        cx.seeded_at.insert(name.clone(), start);
    }
}

/// The `/** … */` docblock immediately preceding `stmt_start` — only whitespace
/// between — the free-function core of [`SourceTree::stmt_docblock`], reached before
/// the tree exists by the seed pass below.
pub(crate) fn docblock_before<'a>(comments: &'a [Comment], text: &str, stmt_start: u32) -> Option<&'a Comment> {
    // Comment trivia are recovered in source order and never overlap, so `span.end` is
    // monotone and the nearest preceding one is binary-searchable.
    let idx = comments.partition_point(|c| c.span.end <= stmt_start).checked_sub(1)?;
    let c = &comments[idx];
    if c.kind != CommentKind::DocBlock {
        return None;
    }
    let gap = text.get(c.span.end as usize..stmt_start as usize)?;
    gap.chars().all(char::is_whitespace).then_some(c)
}

/// The candidate reads of the `unset` pseudo-type idiom over the **top-level script
/// scope** (ADR-0087 §4, issue #396) — the computation behind
/// [`SourceTree::unset_seed_facts`].
///
/// The engine is [`maybe_undefined_reads`]', unchanged: the same three-valued
/// lattice, the same polarity-consuming guards, the same terminating-arm subtraction,
/// the same loop fixpoint. Three premises differ, and only three:
///
/// * **Scope entry is `Bound`, not `Unbound`.** ADR-0081 §6 silences a script scope
///   because an included file inherits the includer's symbol table, so the CST cannot
///   claim a name is absent. That silence is kept literally here: every candidate
///   starts bound, and only the author's own `|unset` moves it to `Maybe`. A read
///   *before* the declaration is therefore silent — it has no premise yet.
/// * **The reportable set is the declared names**, not the scope's binding set: the
///   premise is the declaration, so a name nothing declares is nobody's finding.
/// * **A name dam ends the pass rather than blanking it.** `extract`, `compact`,
///   `get_defined_vars`, `$$x`, `eval` and — the one that forces the rule — `include`
///   / `require` are routine in exactly the top-level templates this idiom is written
///   for, so blanking the scope whole (the ADR-0081 §6 rule) would silence the
///   feature outright. Instead every seeded name becomes `Bound` from the dam
///   onwards: reads *before* it are still judged, and after it nothing is claimed,
///   which is the silence direction. A `goto` or label still dams the pass outright,
///   the ADR-0081 non-goal.
pub(crate) fn unset_seed_facts(top: &[&Statement<'_>], source: &str, comments: &[Comment]) -> UnsetSeedFacts {
    let names = seed_candidate_names(comments);
    if names.is_empty() {
        return UnsetSeedFacts::default();
    }
    if top.iter().any(|s| subtree_has_goto(&Node::Statement(s))) {
        return UnsetSeedFacts::default();
    }
    let reportable: HashSet<String> = names.values().flatten().cloned().collect();
    let seeds = SeedIndex { comments, text: source, names };

    let mut state = PresenceState::new();
    for name in &reportable {
        state.insert(name.clone(), BindingPresence::Bound);
    }
    let mut cx = PresenceCx {
        reportable: &reportable,
        seeds: Some(&seeds),
        seeded_at: HashMap::new(),
        out: Vec::new(),
        silent: false,
        seen: HashSet::new(),
        breaks: Vec::new(),
        continues: Vec::new(),
        loop_memo: LoopMemo::for_run(),
    };
    for s in top {
        if presence_stmt(s, &mut state, &mut cx) != PresenceFlow::Fell {
            break;
        }
    }

    let dam = first_name_dam(top);
    let mut reads: Vec<UnsetSeedRead> = cx
        .out
        .into_iter()
        .filter(|(read, _)| dam.is_none_or(|d| read.span.start < d))
        .filter_map(|(read, seed)| {
            seed.map(|seed_stmt| UnsetSeedRead { name: read.name, span: read.span, seed_stmt })
        })
        .collect();
    reads.sort_by_key(|r| r.span.start);
    if reads.is_empty() {
        return UnsetSeedFacts::default();
    }

    // The out-parameter residue (ADR-0077), collected the way `undefined_variable_reads`
    // collects it and restricted to the names actually judged.
    let mut acc = VarUsage::default();
    for s in top {
        scan_var_usage(&Node::Statement(s), false, &[], &mut acc);
    }
    let judged: HashSet<&str> = reads.iter().map(|r| r.name.as_str()).collect();
    let ref_arg_candidates =
        acc.arg_candidates.into_iter().filter(|c| judged.contains(c.name.as_str())).collect();
    UnsetSeedFacts { reads, ref_arg_candidates }
}

/// The syntactic superset of the seeds: for every docblock whose text mentions
/// `unset` at all, every `$name` it spells.
///
/// Deliberately coarse in the one direction that is safe. `steins-syntax` cannot
/// lower a phpdoc type, so it cannot know which tag carries the pseudo-type; what it
/// can know is that the leaf is unreachable without the word, so a docblock without
/// it can be skipped outright — which is every docblock in almost every file. The
/// caller lowers the named tag and drops whatever does not confirm.
fn seed_candidate_names(comments: &[Comment]) -> HashMap<u32, Vec<String>> {
    let mut out = HashMap::new();
    for c in comments {
        if c.kind != CommentKind::DocBlock || !mentions_unset(&c.text) {
            continue;
        }
        let names = docblock_variable_names(&c.text);
        if !names.is_empty() {
            out.insert(c.span.end, names);
        }
    }
    out
}

/// Whether a docblock spells `unset` anywhere, ASCII-case-insensitively — the gate
/// that keeps this whole pass off every file that does not use the idiom.
fn mentions_unset(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.windows(5).any(|w| w.eq_ignore_ascii_case(b"unset"))
}

/// Every `$name` token in a docblock's raw text, deduplicated in first-seen order.
fn docblock_variable_names(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            i += 1;
            continue;
        }
        let start = i + 1;
        let mut end = start;
        while end < bytes.len() && (bytes[end] == b'_' || bytes[end].is_ascii_alphanumeric()) {
            end += 1;
        }
        // A PHP variable name never starts with a digit; `$1` is not one.
        if end > start && !bytes[start].is_ascii_digit() {
            let name = String::from_utf8_lossy(&bytes[start..end]).into_owned();
            if !out.contains(&name) {
                out.push(name);
            }
        }
        i = end.max(start);
    }
    out
}

/// The byte offset of the first name dam in this statement list, or `None`.
///
/// The dam set is [`scan_var_usage`]'s own — `$$x`, `eval`, `include`/`require`,
/// `extract`, `compact`, `get_defined_vars` — asked for a *position* rather than for
/// the scope-wide flag that pass records, because this consumer keeps the reads
/// before the dam (see [`unset_seed_facts`]). Nested scopes are not descended into,
/// matching where the pass itself looks.
fn first_name_dam(top: &[&Statement<'_>]) -> Option<u32> {
    let mut best: Option<u32> = None;
    for s in top {
        collect_name_dam(&Node::Statement(s), &mut best);
    }
    best
}

fn collect_name_dam(node: &Node<'_, '_>, best: &mut Option<u32>) {
    let hit = match node {
        Node::ArrowFunction(_)
        | Node::Closure(_)
        | Node::Function(_)
        | Node::Class(_)
        | Node::Interface(_)
        | Node::Trait(_)
        | Node::Enum(_)
        | Node::AnonymousClass(_) => return,
        Node::NestedVariable(_)
        | Node::IndirectVariable(_)
        | Node::EvalConstruct(_)
        | Node::IncludeConstruct(_)
        | Node::IncludeOnceConstruct(_)
        | Node::RequireConstruct(_)
        | Node::RequireOnceConstruct(_) => true,
        Node::FunctionCall(fc) => matches!(fc.function, Expression::Identifier(id)
            if matches!(
                bytes_to_string(id.last_segment()).as_str(),
                "extract" | "compact" | "get_defined_vars"
            )),
        _ => false,
    };
    if hit {
        let start = to_span(node.span()).start;
        *best = Some(best.map_or(start, |b| b.min(start)));
    }
    for child in node.children() {
        collect_name_dam(&child, best);
    }
}

// end unset pseudo-type (ADR-0087 §4, issue #396)

/// Whether a `goto` or a label stands anywhere in this subtree, without descending
/// into a nested scope.
pub(crate) fn subtree_has_goto(node: &Node<'_, '_>) -> bool {
    // A `try` answers from the per-parse memo (see `memo::TryScan`).
    if let Node::Try(t) = node
        && let Some(hit) =
            memo::try_flag(t, TryScan::Goto, || node.children().iter().any(subtree_has_goto))
    {
        return hit;
    }
    match node {
        Node::Goto(_) | Node::Label(_) => true,
        Node::Function(_)
        | Node::Method(_)
        | Node::Closure(_)
        | Node::ArrowFunction(_)
        | Node::AnonymousClass(_)
        | Node::Class(_)
        | Node::Interface(_)
        | Node::Trait(_)
        | Node::Enum(_) => false,
        _ => node.children().iter().any(subtree_has_goto),
    }
}

// end binding presence (ADR-0081, issue #267)
