//! A guard over a `OneOf`, decided member by member (ADR-0043's 2026-10-11 amendment on
//! #950, the #1071 review).
//!
//! A `Singleton`'s guard is folded and its dead side pruned. A `OneOf` has no single
//! value to fold, so its guard evaluated `Maybe`, both sides were walked with the
//! whole set, and a verdict that needs every member to reach the point (the definite
//! `OneOf` finding of `finite_verdict`) fired on a point no member reaches:
//! `$x = $c ? 'a' : 'b'; if (ctype_alpha($x)) { return 1; } return $x;` into `: int`.
//!
//! The condition is therefore evaluated once per member, with the member bound as a
//! `Singleton` (the very fold a `Singleton` takes), and the verdicts are combined:
//!
//! ```text
//! members' verdicts                      then side          else side         after the join
//! -------------------------------------- ------------------ ----------------- ----------------
//! every member decided, all Yes          the set            dead (pruned)     the set
//! every member decided, all No           dead (pruned)      the set           the set
//! every member decided, mixed Yes / No   Yes members only   No members only    the union again
//! some member undecidable (Maybe)        forgotten          forgotten         restored when
//!                                                                             every kept side
//!                                                                             falls through,
//!                                                                             forgotten when
//!                                                                             only one does
//! two or more OneOf variables read       forgotten (all)    forgotten (all)    as above
//! ```
//!
//! The per-member decision is the precise half. It cannot see a guard on a *copy*
//! (`$y = $x; if (ctype_alpha($y)) { return 1; }`), on a value *derived* from the set
//! (`$ok = ctype_alpha($x)`) or on an array holding it: the walk binds nothing it can
//! decide those on, and teaching it each shape is the patching the #1071 re-review
//! stopped. The backstop is a **path rule**, tracked on the binding (`Known::thinned`):
//!
//! ```text
//! walk event after the OneOf is bound                         the binding
//! ----------------------------------------------------------- ------------------------
//! an `if` with a live side that does not fall through         thinned, unless this guard
//! (return, throw, break, continue, exit, goto)                 decided that variable
//! a loop, a `try`, or a `match` (any other subject)           thinned
//! an `if` whose live sides all fall through                   unchanged
//! a rebinding of the variable                                 a new, unthinned binding
//! a join of bindings                                          thinned if either was
//!
//! verdict at the judged site                  not thinned          thinned
//! ------------------------------------------- -------------------- -------------------
//! every member refused                        definite (D6)        possibly grade
//! some members refused                        possibly grade       possibly grade
//! no member refused                           silent               silent
//! ```
//!
//! The flag travels with the value. Every binding is marked at the event above, not
//! only a `OneOf`, and an assignment inherits the flag of every variable its right-hand
//! side mentions (`inherits_thinned`): a copy, a shape element read back, a closure
//! called, a concatenation, a fold, a call's arguments. A binding built from nothing
//! thinned is fresh. So a set stored in a shape or captured by a closure *before* the
//! exit is thinned when it comes back out *after* it, with no per-site plumbing.
//!
//! ```text
//! a ternary `$r = <cond> ? a : b` whose arm is a OneOf variable the condition reads
//! ------------------------------------------------------------------------------------
//! condition decided per member   the members that reach each arm, plus the other arm's
//!                                literal if some member reaches its side
//! undecided, every arm one base  that base, `General` (still a proven string, so
//!                                `$r->foo()` stays `call.on-non-object`)
//! undecided, mixed bases         nothing bound (a type-mismatch needs a value that
//!                                reaches the site)
//! a ternary that reads no set    the evaluator's fact; the result inherits the flag
//! ```
//!
//! (A ternary in a `return`, a `match (true)` and a `switch` are already silent.)
//!
//! "Forgotten" is the `unmodelled_tests` treatment a type test over a `Singleton`
//! already gets: no verdict is premised on a binding inside a side that may be dead.
//! A narrowed set is exact, so a verdict over it holds for every member that reaches
//! the point. A set that is narrowed to one member is a `Singleton` again.

use std::collections::HashMap;

use steins_domain::{Certainty, Fact, Val};
use steins_syntax::{ArgValue, CallExpr, Callee, CondExpr, CondOperand, Receiver};

use crate::branch::atom_verdict;
use crate::cond::eval_cond;
use crate::predicates::in_array_literals;
use crate::env::{Known, Store};
use crate::fold::Folder;
use crate::resource::mentioned_vars;
use crate::walk::WalkCx;

/// What a condition says about one `OneOf` variable it reads.
pub(crate) struct OneOfGuard {
    pub(crate) var: String,
    /// The condition's verdict over the whole set.
    pub(crate) verdict: Certainty,
    /// The members that reach the then side / the else side, when every member's
    /// verdict was decided; `None` when some member was undecidable.
    pub(crate) sides: Option<(Vec<Val>, Vec<Val>)>,
}

/// Every `OneOf` variable the condition reads, judged per member. Empty when the
/// condition reads none, which is the common case and costs one pass over the atoms.
pub(crate) fn oneof_guards(
    w: &WalkCx,
    folder: &mut dyn Folder,
    cond: &CondExpr,
    env: &HashMap<String, Known>,
    store: &Store,
) -> Vec<OneOfGuard> {
    let mut reads = Vec::new();
    collect_reads(cond, &mut reads);
    reads.sort();
    reads.dedup();
    let oneofs: Vec<String> = reads
        .into_iter()
        .filter(|v| matches!(env.get(v).and_then(|k| k.fact.as_ref()), Some(Fact::OneOf(_))))
        .collect();
    let several = oneofs.len() > 1;
    oneofs
        .into_iter()
        .map(|var| {
            let verdict_sides = if several { None } else { per_member(w, folder, cond, &var, env, store) };
            match verdict_sides {
                Some((verdict, then_set, else_set)) => OneOfGuard {
                    var,
                    verdict,
                    sides: Some((then_set, else_set)),
                },
                None => OneOfGuard { var, verdict: Certainty::Maybe, sides: None },
            }
        })
        .collect()
}

/// The condition evaluated with each member of `var` bound as a `Singleton`.
fn per_member(
    w: &WalkCx,
    folder: &mut dyn Folder,
    cond: &CondExpr,
    var: &str,
    env: &HashMap<String, Known>,
    store: &Store,
) -> Option<(Certainty, Vec<Val>, Vec<Val>)> {
    let known = env.get(var)?;
    let Some(Fact::OneOf(members)) = known.fact.as_ref() else { return None };
    let (mut then_set, mut else_set) = (Vec::new(), Vec::new());
    let mut trial = env.clone();
    for m in members {
        let mut k = known.clone();
        k.fact = Some(Fact::Singleton(m.clone()));
        trial.insert(var.to_owned(), k);
        match member_verdict(w, folder, cond, var, m, &trial, store) {
            Certainty::Yes => then_set.push(m.clone()),
            Certainty::No => else_set.push(m.clone()),
            Certainty::Maybe => return None,
        }
    }
    let verdict = match (then_set.is_empty(), else_set.is_empty()) {
        (false, true) => Certainty::Yes,
        (true, false) => Certainty::No,
        _ => Certainty::Maybe,
    };
    Some((verdict, then_set, else_set))
}

/// The condition's verdict with `var` bound to the single member `m` (already in
/// `trial`). The atoms the fact-level narrowers model are decided the way they decide
/// them: a type predicate on the fact, and a strict `in_array` over literals as an
/// identity test; every other atom is [`eval_cond`]'s, and an atom it cannot decide
/// leaves the member undecided.
fn member_verdict(
    w: &WalkCx,
    folder: &mut dyn Folder,
    cond: &CondExpr,
    var: &str,
    m: &Val,
    trial: &HashMap<String, Known>,
    store: &Store,
) -> Certainty {
    match cond {
        CondExpr::Not(c) => member_verdict(w, folder, c, var, m, trial, store).not(),
        CondExpr::And(a, b, _) => {
            let (l, r) = (
                member_verdict(w, folder, a, var, m, trial, store),
                member_verdict(w, folder, b, var, m, trial, store),
            );
            match (l, r) {
                (Certainty::No, _) | (_, Certainty::No) => Certainty::No,
                (Certainty::Yes, Certainty::Yes) => Certainty::Yes,
                _ => Certainty::Maybe,
            }
        }
        CondExpr::Or(a, b, _) => {
            let (l, r) = (
                member_verdict(w, folder, a, var, m, trial, store),
                member_verdict(w, folder, b, var, m, trial, store),
            );
            match (l, r) {
                (Certainty::Yes, _) | (_, Certainty::Yes) => Certainty::Yes,
                (Certainty::No, Certainty::No) => Certainty::No,
                _ => Certainty::Maybe,
            }
        }
        CondExpr::Call { call, .. } => {
            if let Some((v, lits)) = in_array_literals(w.cx, call)
                && v == var
            {
                return if lits.contains(m) { Certainty::Yes } else { Certainty::No };
            }
            atom_verdict(w, folder, cond, trial, store)
        }
        atom => eval_cond(w, folder, atom, trial, store, w.scope.poisoned),
    }
}

/// The fact a side keeps of a set narrowed to `members`.
pub(crate) fn narrowed_fact(members: &[Val]) -> Option<Fact> {
    match members {
        [] => None,
        [one] => Some(Fact::Singleton(one.clone())),
        many => Some(Fact::OneOf(many.to_vec())),
    }
}

/// Every variable the condition mentions, anywhere (atoms of `&&`/`||`/`!` included).
pub(crate) fn collect_reads(cond: &CondExpr, out: &mut Vec<String>) {
    let operand = |op: &CondOperand, out: &mut Vec<String>| match op {
        CondOperand::Var(v) => out.push(v.clone()),
        CondOperand::Other { reads, .. } => out.extend(reads.iter().cloned()),
        // `$x[0]` reads `$x`, and `$a[$x]` reads `$x` through its key.
        CondOperand::Offset { var, key } => {
            out.push(var.clone());
            if let ArgValue::Var(k) = key.as_ref() {
                out.push(k.clone());
            }
        }
        CondOperand::Literal(_) | CondOperand::Const(_) | CondOperand::ClassConst(..) => {}
    };
    match cond {
        CondExpr::Cmp { lhs, rhs, .. } => {
            operand(lhs, out);
            operand(rhs, out);
        }
        CondExpr::Truthy(op) | CondExpr::Instanceof { operand: op, .. } => operand(op, out),
        CondExpr::Not(c) => collect_reads(c, out),
        CondExpr::And(a, b, _) | CondExpr::Or(a, b, _) => {
            collect_reads(a, out);
            collect_reads(b, out);
        }
        CondExpr::Call { reads, .. }
        | CondExpr::Opaque { reads, .. }
        | CondExpr::InstanceofDyn { reads, .. } => out.extend(reads.iter().cloned()),
        CondExpr::Isset { var, .. } | CondExpr::IssetVar { var } => out.push(var.clone()),
    }
}

/// Narrow the `OneOf` variables of a decided guard to the members that reach one side.
pub(crate) fn narrow_side(guards: &[OneOfGuard], then: bool, benv: &mut HashMap<String, Known>) {
    for g in guards {
        let Some((then_set, else_set)) = &g.sides else { continue };
        let set = if then { then_set } else { else_set };
        if let (Some(fact), Some(k)) = (narrowed_fact(set), benv.get_mut(&g.var)) {
            k.fact = Some(fact);
        }
    }
}

/// Mark every binding of `env` but `except` as thinned: the walk has passed an
/// undecided branch with an exit, so a member of any set a binding holds, or a value
/// derived from one, may not reach the current point (the path rule in the module doc).
/// Every binding, not only a `OneOf`: a shape, a closure or a heap value built from a
/// set before the exit hands the flag on to what is derived from it ([`inherits_thinned`]).
pub(crate) fn mark_thinned(env: &mut HashMap<String, Known>, except: &[String]) {
    for (name, k) in env.iter_mut() {
        if !except.contains(name) {
            k.thinned = true;
        }
    }
}

/// Whether a value being bound mentions a thinned binding: an assignment inherits the
/// flag of every variable its right-hand side reads (a copy, a shape element, a
/// concatenation, a fold, a call's arguments, a closure's captures and the closure
/// called). A binding derived from nothing thinned is a fresh one.
pub(crate) fn inherits_thinned(
    value: &ArgValue,
    call: Option<&CallExpr>,
    env: &HashMap<String, Known>,
) -> bool {
    let mut names: Vec<&str> = Vec::new();
    mentioned_vars(value, &mut names);
    // `$f()` lowers to no value at all; the statement's call carries the closure
    // variable called and the arguments passed.
    if let Some(c) = call {
        match &c.receiver {
            Callee::DynamicVar(v) | Callee::Method { receiver: Receiver::Var(v), .. } => names.push(v),
            _ => {}
        }
        for a in &c.args {
            mentioned_vars(&a.value, &mut names);
        }
    }
    match value {
        ArgValue::MethodCall { callee: Callee::DynamicVar(v), .. } => names.push(v),
        ArgValue::PropFetch { var, .. } => names.push(var),
        _ => {}
    }
    names.iter().any(|n| env.get(*n).is_some_and(|k| k.thinned))
}
