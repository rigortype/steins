//! Unresolved aliases of heap objects (ADR-0036, issue #1048): what keeps a property
//! fact honest when the same object is reachable through a name the store does not
//! bind.
//!
//! The store follows an object through one route only, the plain copy `$b = $a`,
//! which shares the allocation id. Every other route hands the handle to a name, an
//! array slot or a callee's parameter that the store cannot resolve, and a write
//! through that name lands on the same object. Objects are handles, so
//! `$arr = [$o]; foreach ($arr as $q) { $q->p = 'x'; }` changes `$o->p`.
//!
//! The rule has two halves, and neither is sound without the other:
//!
//! 1. **Reach.** Every route that lets an object reach a name the store does not
//!    bind marks the object **escaped**. The escape set is then exactly the set of
//!    objects an unresolved name may hold.
//! 2. **Write.** A write through an unresolved name sweeps the escape set: the
//!    non-readonly props of every escaped object go, which is the sweep an unknown
//!    call already performs. Readonly props and the class survive, as they do there.
//!
//! An object no route has reached stays unescaped and keeps its facts across every
//! such write. That is the precision the rule keeps: `$arr = [new O()];` followed
//! by a write through `$q` touches nothing a name of this frame holds.
//!
//! # Reach: how an object comes to a second, unresolved name
//!
//! | route | where it escapes |
//! | --- | --- |
//! | plain copy `$q = $o` | no escape: the store shares the id (the precise route) |
//! | array literal `[$o]`, `['k' => $o]` | the rvalue ([`escape_stored_objects`]) |
//! | ternary, `??`, cast, array union `+` | the rvalue ([`escape_stored_objects`]) |
//! | a value the lowering cannot spell: `match`, `(object)`, a computed key, a chain | the statement ([`escape_unspelled_reads`]) |
//! | `$arr[] = $o`, `$arr[$k] = $o` | the write clears the heap (`open_offset_barrier`) |
//! | property `$h->p = $o`, closure `use ($o)`, `return $o` | already escaped (ADR-0036) |
//! | call argument `f($o)`, `f([$o])`, receiver `$o->m()` | the call's sweep (`escape_and_sweep_calls`) |
//! | foreach value or by-ref value over `$arr` | when the element was stored (rows above) |
//! | foreach over an object `foreach ($o as $v)` | the subject escapes |
//! | `list()` / `[$q] = …` destructuring | a barrier: the heap is cleared |
//! | `array_map`/`array_walk` callbacks, `current`, `reset`, `end`, `array_pop`, `array_shift`, `array_values`, `array_filter`, `iterator_to_array` | the element escaped when stored; each is a builtin, an unresolved call that sweeps |
//! | `$arr[$k]` (dynamic or constant key) read into `$q` | the element escaped when stored |
//! | spread `f(...$arr)` | the element escaped when stored; a resolved callee with a spread sweeps |
//! | a function returning its argument, `$q = id($o)` | the argument escaped at the call |
//! | generator `foreach (gen($o) as $q)` | the argument escaped at the call |
//! | a join whose branches bind a name to different objects | the dropped name's object ([`join_stores`]) |
//! | a loop or unwalked construct forgetting a written name | the forgotten name's object |
//!
//! # Write: how a write reaches the object through the unresolved name
//!
//! | write | effect |
//! | --- | --- |
//! | `$q->p = v`, `$q->p .= v` with `$q` unbound | sweep the escape set ([`sweep_for_unresolved_write`]) |
//! | `$q->m(…)` with `$q` unbound | an unresolved call: the call's sweep |
//! | `unset($q->p)`, `$q->r->p = v`, `$a[0]->p = v`, `$q->$name = v` | a barrier: the heap is cleared |
//! | `f($q->p)` where `f` may take the argument by reference | `$q` bound: its `p` goes; unbound: sweep the escape set |
//! | `f($q)` to a resolved callee, `$q` possibly an object the store cannot name | sweep the escape set ([`may_carry_unnamed_object`]) |
//! | any of the above in a loop body | the fall-through sweeps when the body walk did (`WalkCx::heap_sweeps`) |
//! | any of the above in an unwalked (`Opaque`) construct | the construct sweeps the escape set |
//!
//! Each row was witnessed on PHP 8.5.11: the write through the alias is visible
//! through the original binding in every reach row, and in no row of the control
//! where the stored object is a different one.
//!
//! The escaped-set sweep is the conservative choice over a per-class one: an
//! unresolved receiver has no class the walk can trust, and a write to a declared
//! property can still run a `__set` or a hook on a class the walk did not pick, so
//! narrowing the sweep by the written name or a class would trust what nothing
//! proved. An object the rule keeps is one no unresolved name can hold, whatever
//! the write does.
//!
//! [`join_stores`]: crate::env::join_stores

use std::collections::HashMap;

use steins_contract::ContractTy;
use steins_domain::Fact;
use steins_syntax::{ArgValue, Callee, ClosureRef, Receiver};

use crate::env::{Known, Store, Stratum};
use crate::walk::WalkCx;

/// Mark escaped every heap object `value` may hand on **as itself** — an array
/// element, an arm of a ternary or `??`, a cast operand, an offset base, a nested
/// call's argument or receiver, a closure's capture. Answers whether any bound
/// object was named there.
///
/// An operator that yields a scalar (`==`, `.`, `!`, `isset`) hands no handle on,
/// and a property fetch hands on the property's value, not the receiver; neither
/// escapes anything.
pub(crate) fn escape_stored_objects(value: &ArgValue, store: &mut Store) -> bool {
    let mut vars = Vec::new();
    flowing_vars(value, &mut vars);
    let mut any = false;
    for v in vars {
        if store.is_object(v) {
            store.mark_escaped(v);
            any = true;
        }
    }
    any
}

/// Mark escaped every bound object a statement names inside a value its lowering
/// could not spell ([`steins_syntax::Stmt::unspelled_reads`]): a `match`, an
/// `(object)` cast or an array with a computed key may store the handle as surely
/// as an array literal does.
pub(crate) fn escape_unspelled_reads(names: &[String], store: &mut Store) {
    for v in names {
        if store.is_object(v) {
            store.mark_escaped(v);
        }
    }
}

/// The variables whose handle `value` may carry into its result or into a callee.
fn flowing_vars<'a>(value: &'a ArgValue, out: &mut Vec<&'a str>) {
    match value {
        ArgValue::Var(v) => out.push(v),
        ArgValue::Array(items) => items.iter().for_each(|(_, v)| flowing_vars(v, out)),
        ArgValue::Ternary { then_val, else_val, .. } => {
            flowing_vars(then_val, out);
            flowing_vars(else_val, out);
        }
        ArgValue::Coalesce(a, b, _) => {
            flowing_vars(a, out);
            flowing_vars(b, out);
        }
        ArgValue::Cast { operand, .. } => flowing_vars(operand, out),
        // `+` on arrays is a union that keeps the operands' elements.
        ArgValue::Binary { lhs, rhs, .. } => {
            flowing_vars(lhs, out);
            flowing_vars(rhs, out);
        }
        // A string, a bool: the operand's handle stays behind, but a call nested
        // in it is handed its arguments all the same.
        ArgValue::Concat(a, b) | ArgValue::Logical { lhs: a, rhs: b, .. } => {
            nested_call_vars(a, out);
            nested_call_vars(b, out);
        }
        ArgValue::Not(v) => nested_call_vars(v, out),
        ArgValue::OffsetRead { base, .. } => flowing_vars(base, out),
        ArgValue::Call(_, args) => args.iter().for_each(|a| flowing_vars(a, out)),
        ArgValue::MethodCall { callee, args, named } => {
            if let Callee::Method { receiver: Receiver::Var(v), .. } = callee {
                out.push(v);
            }
            if let Callee::Method { receiver: Receiver::New { args, named, .. }, .. } = callee {
                args.iter().chain(named.iter().map(|n| &n.value)).for_each(|a| flowing_vars(a, out));
            }
            args.iter().chain(named.iter().map(|n| &n.value)).for_each(|a| flowing_vars(a, out));
        }
        ArgValue::New(_, args, named) => {
            args.iter().chain(named.iter().map(|n| &n.value)).for_each(|a| flowing_vars(a, out));
        }
        ArgValue::Closure(ClosureRef::Anonymous { captures, .. }) => {
            out.extend(captures.iter().map(String::as_str));
        }
        _ => {}
    }
}

/// The handles the calls nested in a scalar-valued operand are handed: the
/// operand itself is converted, but `'a' . f($o)` still passes `$o` to `f`.
fn nested_call_vars<'a>(value: &'a ArgValue, out: &mut Vec<&'a str>) {
    match value {
        ArgValue::Var(_) => {}
        other => flowing_vars(other, out),
    }
}

/// Whether `value`, handed to a callee, may carry an object the store cannot name —
/// one an unresolved name holds, so possibly any escaped object. A callee that
/// writes through it writes that object, and a resolved callee is otherwise trusted
/// to leave the escape set alone.
///
/// A bound object is not this question: [`escape_stored_objects`] already escaped
/// it and the call sweeps for it. A literal, an operator's scalar result and a name
/// whose `Verified` fact or declared lane is scalar carry none. A call's result, an
/// unbound name the walk knows nothing about, an element of an array it cannot
/// spell, and a callable that may run a body all may.
pub(crate) fn may_carry_unnamed_object(
    value: &ArgValue,
    env: &HashMap<String, Known>,
    store: &Store,
) -> bool {
    match value {
        ArgValue::Var(v) => !store.is_bound(v) && !holds_no_object(v, env, store),
        ArgValue::Array(items) => items.iter().any(|(_, v)| may_carry_unnamed_object(v, env, store)),
        ArgValue::Ternary { then_val, else_val, .. } => {
            may_carry_unnamed_object(then_val, env, store)
                || may_carry_unnamed_object(else_val, env, store)
        }
        ArgValue::Coalesce(a, b, _) => {
            may_carry_unnamed_object(a, env, store) || may_carry_unnamed_object(b, env, store)
        }
        ArgValue::Cast { operand, .. } => may_carry_unnamed_object(operand, env, store),
        ArgValue::New(_, args, named) => args
            .iter()
            .chain(named.iter().map(|n| &n.value))
            .any(|a| may_carry_unnamed_object(a, env, store)),
        // A property's value: known and object-free only through a recorded fact.
        ArgValue::PropFetch { var, prop } => {
            !store.prop_fact(var, prop).is_some_and(|f| !matches!(f, Fact::Shape { .. }))
        }
        // An element of an array whose whole value is known holds no object.
        ArgValue::OffsetRead { base, .. } => match base.as_ref() {
            ArgValue::Var(b) => !holds_no_object(b, env, store),
            _ => true,
        },
        ArgValue::Int(_)
        | ArgValue::Float(_)
        | ArgValue::Str(_)
        | ArgValue::Bool(_)
        | ArgValue::Null
        | ArgValue::ClassConst(..)
        | ArgValue::EnumCase(..)
        | ArgValue::GlobalConst(_)
        | ArgValue::Isset(_)
        | ArgValue::Not(_)
        | ArgValue::Binary { .. }
        | ArgValue::Logical { .. }
        | ArgValue::Concat(..) => false,
        // A call's result, a clone (its props are the source's handles), a callable
        // that may run a body, and an expression the IR does not spell.
        ArgValue::Call(..)
        | ArgValue::MethodCall { .. }
        | ArgValue::Clone(_)
        | ArgValue::Closure(_)
        | ArgValue::Other => true,
    }
}

/// Whether the unbound name `var` provably holds no object: a `Verified` scalar or
/// fully-known array fact (an abstract array shape may hold objects), or a declared
/// lane whose every arm is a `Verified` scalar.
fn holds_no_object(var: &str, env: &HashMap<String, Known>, store: &Store) -> bool {
    if let Some(k) = env.get(var)
        && k.closure.is_none()
        && k.stratum == Stratum::Verified
        && let Some(fact) = &k.fact
    {
        return !matches!(fact, Fact::Shape { .. });
    }
    store.contract_arms(var).is_some_and(|arms| {
        arms.iter().all(|a| a.stratum == Stratum::Verified && is_scalar_arm(&a.ty))
    })
}

/// Whether a declared arm admits only scalars and `null`.
fn is_scalar_arm(ty: &ContractTy) -> bool {
    matches!(
        ty,
        ContractTy::Null
            | ContractTy::Base(_)
            | ContractTy::IntIn(_)
            | ContractTy::StrWith(_)
            | ContractTy::StrOpaque
            | ContractTy::LitInt(_)
            | ContractTy::LitFloat(_)
            | ContractTy::LitStr(_)
            | ContractTy::LitBool(_)
    )
}

/// A write through a name the store does not resolve (the write table's first row):
/// it may land on any escaped object, so the escape set is swept.
pub(crate) fn sweep_for_unresolved_write(w: &WalkCx, store: &mut Store) {
    store.sweep_escaped();
    w.note_heap_sweep();
}
