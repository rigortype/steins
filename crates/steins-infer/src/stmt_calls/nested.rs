//! The calls a statement writes anywhere but the position its kind spells (ADR-0104,
//! issue #935): an argument, an operand, an array element, a `new` argument, a `throw` or
//! `exit` operand, a `static` initializer, a first-class callable. The rule table is on
//! [`super`]; this is its implementation, one function per family class.

use std::collections::{HashMap, HashSet};

use steins_syntax::{
    ArgValue, CallExpr, Callee, Carrier, ReadKind, ReadPosition, Receiver, Span, Stmt,
};

use crate::absence::{check_undefined_function, check_undefined_method};
use crate::arity::{check_arity, check_printf_arity};
use crate::by_value::by_value_survivors;
use crate::descent::checkable_calls;
use crate::declared_receiver::check_phpdoc_undefined_method;
use crate::env::{Known, Store};
use crate::existence::is_member_guard_call;
use crate::fold::Folder;
use crate::inaccessible::check_inaccessible_method;
use crate::non_object::{check_call_on_non_object, check_call_on_null};
use crate::out_params::check_preg_pattern;
use crate::project::Diagnostic;
use crate::walk::{WalkCx, in_dead};

/// How far a family reaches at a position, decided by its carrier alone.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reach {
    /// Every family: the read is evaluated unconditionally, so the entry env is the env it
    /// is judged in (modulo [`Blocked`]).
    Value,
    /// The environment-free families only (existence, arity): the read may not run, and a
    /// test the statement spells may have established what an env-dependent family reads.
    Existence,
}

/// The reach a carrier gives a call position, `None` where the position is not judged.
fn reach_of(carrier: Carrier) -> Option<Reach> {
    match carrier {
        Carrier::Plain | Carrier::Argument { .. } => Some(Reach::Value),
        Carrier::Conditional => Some(Reach::Existence),
        // `@` is silent at the default floor (D2); the others are not carriers of a call.
        Carrier::Suppressed | Carrier::IssetLike | Carrier::CompoundRead => None,
    }
}

/// The names this statement may have rebound before the position runs: an embedded
/// assignment, or a name handed to a call that may take it by reference. The entry env holds
/// the pre-statement value of each, so an env-dependent judgement about one is stale.
struct Blocked<'s>(HashSet<&'s str>);

impl<'s> Blocked<'s> {
    fn of(
        w: &WalkCx,
        stmt: &'s Stmt,
        env: &HashMap<String, Known>,
        store: &Store,
    ) -> Self {
        if stmt.invalidated.is_empty() {
            return Blocked(HashSet::new());
        }
        let (kept, _) =
            by_value_survivors(w.cx, w.scope.poisoned, &stmt.invalidated, env, store);
        Blocked(
            stmt.invalidated
                .iter()
                .map(|e| e.name.as_str())
                .filter(|n| !kept.contains(n))
                .collect(),
        )
    }

    /// Whether `call` reads a name this statement may have rebound: its receiver variable.
    fn receiver_of(&self, call: &CallExpr) -> bool {
        let Callee::Method { receiver: Receiver::Var(v) | Receiver::Prop { var: v, .. }, .. } =
            &call.receiver
        else {
            return false;
        };
        self.0.contains(v.as_str())
    }

    /// Whether the first two arguments (where a format or a pattern sits) fold off the entry
    /// env: a literal does not, a bare name does unless it may be rebound, anything else
    /// does unless the statement rebinds nothing.
    fn args_fold(&self, call: &CallExpr) -> bool {
        call.args.iter().take(2).any(|a| match &a.value {
            ArgValue::Str(_) | ArgValue::Int(_) | ArgValue::Float(_) | ArgValue::Bool(_)
            | ArgValue::Null => false,
            ArgValue::Var(v) => self.0.contains(v.as_str()),
            _ => !self.0.is_empty(),
        })
    }
}

/// Judge every call position of `stmt` that its kind does not spell, against the entry env
/// and store, in the plain per-scope pass only.
pub(super) fn check_nested_calls(
    w: &WalkCx,
    folder: &mut dyn Folder,
    stmt: &Stmt,
    env: &HashMap<String, Known>,
    store: &Store,
    out: &mut Vec<Diagnostic>,
) {
    let positions = stmt.read_positions();
    if !positions.iter().any(|p| matches!(p.kind, ReadKind::Call | ReadKind::CallRef)) {
        return;
    }
    let own: Vec<Span> = checkable_calls(&stmt.kind).iter().map(|c| c.span).collect();
    let blocked = Blocked::of(w, stmt, env, store);
    let guarded = positions
        .iter()
        .filter_map(|p| call_at(w, stmt, p))
        .any(|c| is_member_guard_call(w.cx, c));
    for pos in positions {
        let Some(reach) = reach_of(pos.carrier) else { continue };
        // The statement's own calls are judged by the kind's loop, so a call is judged once
        // however many positions name it.
        if own.contains(&pos.span) {
            continue;
        }
        let Some(call) = call_at(w, stmt, pos) else { continue };
        if in_dead(&w.dead.borrow(), pos.span.start) {
            continue;
        }
        let site = Site { w, folder, call, env, store, blocked: &blocked, reach, guarded };
        match pos.kind {
            ReadKind::Call => site.judge_call(out),
            ReadKind::CallRef => site.judge_reference(out),
            _ => {}
        }
    }
}

/// The lowered call behind a call position: a `new` and a function first-class callable are
/// in the statement, a function call in the tree, any method or static call (and a method
/// first-class callable) in the scope; `None` when the lowering kept none (a dynamic callee).
fn call_at<'a>(w: &'a WalkCx, stmt: &'a Stmt, pos: &ReadPosition) -> Option<&'a CallExpr> {
    if !matches!(pos.kind, ReadKind::Call | ReadKind::CallRef) {
        return None;
    }
    stmt.read_calls()
        .iter()
        .find(|c| c.span == pos.span)
        .or_else(|| w.cx.tree().call_at(pos.span))
        .or_else(|| w.scope.method_call_at(pos.span))
}

/// One call position and what judging it reads.
struct Site<'a, 'b, 'c> {
    w: &'a WalkCx<'b, 'c>,
    folder: &'a mut dyn Folder,
    call: &'a CallExpr,
    env: &'a HashMap<String, Known>,
    store: &'a Store,
    blocked: &'a Blocked<'a>,
    reach: Reach,
    /// The statement carries a member-existence test (`method_exists` and kin).
    guarded: bool,
}

impl Site<'_, '_, '_> {
    /// A real call: existence, arity, and under [`Reach::Value`] the env-dependent families.
    fn judge_call(mut self, out: &mut Vec<Diagnostic>) {
        let (cx, call, store) = (self.w.cx, self.call, self.store);
        let poisoned = self.w.scope.poisoned;
        match &call.receiver {
            Callee::Function(_) => {
                check_undefined_function(cx, self.folder, call, store, out);
                check_arity(cx, self.folder, call, store, poisoned, out);
                if self.reach == Reach::Value && !self.blocked.args_fold(call) {
                    check_printf_arity(cx, self.folder, call, self.env, poisoned, out);
                    check_preg_pattern(self.w, self.folder, call, self.env, out);
                }
            }
            Callee::Method { .. } | Callee::Static { .. } | Callee::Construct { .. } => {
                if self.blocked.receiver_of(call) {
                    return;
                }
                if self.reach == Reach::Value {
                    check_call_on_null(self.w, call, self.env, store, out);
                    check_call_on_non_object(self.w, call, self.env, store, out);
                    check_inaccessible_method(self.w, call, store, out);
                    check_phpdoc_undefined_method(cx, self.folder, call, store, poisoned, out);
                }
                self.member_exists(out);
                check_arity(cx, self.folder, call, store, poisoned, out);
            }
            Callee::DynamicVar(_) | Callee::Dynamic => {}
        }
    }

    /// A first-class callable: PHP throws at creation when the callee is missing, and runs
    /// no call, so only existence applies. Arity, type, descent and no-effect never see it.
    fn judge_reference(mut self, out: &mut Vec<Diagnostic>) {
        let (cx, call, store) = (self.w.cx, self.call, self.store);
        match &call.receiver {
            Callee::Function(_) => check_undefined_function(cx, self.folder, call, store, out),
            Callee::Method { .. } | Callee::Static { .. } if !self.blocked.receiver_of(call) => {
                self.member_exists(out);
            }
            _ => {}
        }
    }

    /// `call.undefined-method`, declined where a member test of the same statement may have
    /// vouched the member for the one binding it names (`method_exists($o, 'm') && $o->m()`):
    /// the vouch lives in a branch store, and an expression builds none.
    fn member_exists(&mut self, out: &mut Vec<Diagnostic>) {
        if self.reach == Reach::Existence && self.guarded {
            return;
        }
        let poisoned = self.w.scope.poisoned;
        check_undefined_method(self.w.cx, self.folder, self.call, self.store, poisoned, out);
    }
}
