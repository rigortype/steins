//! What one returning exit hands back (ADR-0057 T0/T1): its value fact
//! ([`return_value_fact`]) and its allocation ([`return_heap_object`]); and whether
//! a summary's value fact binds at the caller ([`summary_binds`]).

use std::collections::HashMap;

use steins_domain::Fact;
use steins_syntax::ArgValue;

use crate::fold::Folder;
use crate::assign::eval_coalesce_fact;
use crate::cond::{eval_ternary_fact_strat, total_op_fact};
use crate::env::{
    HeapObj, HeapSummary, Known, ReturnSummary, Store, Stratum, array_literal_fact,
    class_const_class_fact, singleton_fact,
};
use crate::heap::{CtorDefaults, new_heap_object};
use crate::offsets::shape_read_at;
use crate::walk::WalkCx;

/// The returned expression's best value-domain fact and stratum at a returning exit
/// (ADR-0057 amendment T0): a bare variable's env fact (covering the assert-narrowed
/// `positive-int` case), else a constant-key shape read, a `??` chain's join, or a
/// value-position comparison, else a literal/const/foldable `Singleton`, else an
/// array literal's shape, a `::class` fact, or a depth-1 property fact. `None` for
/// any exit the value domain cannot spell (an object, an unresolved call) — a
/// factless exit (A3).
///
/// Every rung is the assignment ladder's own function under the assignment ladder's
/// gates (issue #590): the same rvalue crosses a `return` with the same fact it
/// binds under `$x = <rvalue>`, so this reader adds no resolution logic of its own.
pub(crate) fn return_value_fact(
    w: &WalkCx,
    folder: &mut dyn Folder,
    value: &ArgValue,
    env: &HashMap<String, Known>,
    store: &Store,
) -> Option<(Fact, Stratum)> {
    let poisoned = w.scope.poisoned;
    if let ArgValue::Var(name) = value
        && let Some(known) = env.get(name)
        && let Some(fact) = known.fact.clone()
    {
        return Some((fact, known.stratum));
    }
    // A constant-key read against an abstract shape (ADR-0062 §4, S3): the declared
    // field's value slot. Every no-fact outcome (optional field, unknown slot,
    // declared absence) is a factless exit.
    if let ArgValue::OffsetRead { base, key } = value
        && let Some((read, strat)) = shape_read_at(base, key, env, poisoned, w.cx.php_minor)
        && let Some(fact) = read.into_fact()
    {
        return Some((fact, strat));
    }
    // A ternary `return $c ? A : B;` (ADR-0031, issue #625): the guard's verdict
    // picks the taken arm's fact, an undecided guard joins both. Above the literal
    // rung for the reason the `??` below it is — a ternary is never a literal the
    // folder can reach — and present here because a returned rvalue crosses a
    // `return` with the fact `$x = <rvalue>` binds (issue #590's rule), which for
    // a ternary it did not: the evaluator was wired into the assignment seam
    // alone.
    //
    // The stratum is the assignment seam's: both read `eval_ternary_fact_strat`.
    //
    // This rung's `mark_dead_span` side effect is discarded, and correctly so:
    // `return_value_fact` runs under a binding descent whose `WalkCx` owns its own
    // `dead` vec and whose `dead_out` sink is `None` (only the plain per-scope
    // walk's regions are universal truths — a descent's are dead for that binding
    // only). So the marking discipline needs nothing from this call site.
    if let ArgValue::Ternary { cond, then_val, then_span, else_val, else_span } = value
        && let Some((fact, strat)) = eval_ternary_fact_strat(
            w,
            folder,
            cond,
            then_val,
            else_val,
            (*then_span, *else_span),
            env,
            store,
        )
    {
        return Some((fact, strat));
    }
    // A `??` chain (ADR-0052 §6 + ADR-0062 A-G11, S5): the spine's join under the
    // left-to-right `¬isset` premise ladder. Above the literal rung since a `??`
    // is never a literal the folder can reach.
    if let ArgValue::Coalesce(a, b, rhs_span) = value
        && let Some((fact, strat)) =
            eval_coalesce_fact(w, folder, a, b, *rhs_span, env, Some(store))
    {
        return Some((fact, strat));
    }
    // A value-position operator — a comparison, `<=>`, a connective, `!`, a cast,
    // a concatenation or `isset(…)`, through `total_op_fact`: total, so lower rungs
    // never see one, and an exit crossing one carries the operator's floor at worst
    // rather than no fact. An undecided comparison's `General` still degrades to
    // the arm floor at the caller's binding, exactly as a factless exit does (A3).
    if let Some((fact, strat)) = total_op_fact(w, folder, value, env, Some(store), poisoned) {
        return Some((fact, strat));
    }
    // Stratum from the resolution itself (issue #127 review): a fold over an
    // Asserted project-call summary stays Asserted — never re-read from the
    // syntactic tree. Scratch sink: findings for nested fold args are owned by the
    // return-check / assignment paths that resolve with a real `out`.
    if let Some((lit, strat)) = w.cx.resolve_literal_strat(value, env, poisoned, folder)
        && let Some(fact) = singleton_fact(&lit, w.cx.php_minor)
    {
        return Some((fact, strat));
    }
    // An array literal the rung above could not prove whole (issue #327): keys,
    // count, and sealing are known even when an element's value is not, so the exit
    // crosses a `Fact::Shape` — an untyped callee has no arm floor for A3 to
    // degrade to, so without this rung one such exit killed the whole summary.
    if let ArgValue::Array(items) = value
        && let Some((fact, strat)) =
            array_literal_fact(w.cx, folder, items, env, poisoned, Some(store))
    {
        return Some((fact, strat));
    }
    // The `::class` magic constant (issue #236): the FQN literal when written, the
    // `class-string` refinement when relative. Verified — PHP's own guarantee, not
    // a declaration's.
    if let ArgValue::ClassConst(sc, name) = value
        && let Some(fact) = class_const_class_fact(w.cx, w.scope, sc, name)
    {
        return Some((fact, Stratum::Verified));
    }
    if let ArgValue::PropFetch { var, prop } = value
        && !poisoned
        && let Some(fact) = store.prop_fact(var, prop).cloned()
    {
        return Some((fact, store.prop_stratum(var, prop)));
    }
    None
}

/// The **allocation** a returning exit hands back, snapshotted at the return point
/// (ADR-0057 T1, §2's source list). `None` for every other exit — a scalar, `null`, an
/// unresolved expression — and a `None` on any path kills the whole heap summary
/// (§2.5), there being no heap shape that truthfully covers a non-allocation exit.
///
/// The three sources, and the fourth by composition:
///
/// * **`return $local`** — the object the callee's store holds, verbatim. Its origin
///   does not matter (§2.3): a local `new`, an alias, or the copy ADR-0086 seeded for
///   a parameter are all just "what the walk knows about this value", and the walk's
///   knowledge is sound however the value arrived. Exactness is whatever the object
///   carries, never promoted (§6.4).
/// * **`return $this`** — the same arm; `$this` is `refs["this"]`, pre-escaped by
///   construction and membership-only unless the receiver leg proved exactness, so a
///   fluent chain gets class continuity and no forged exactness (§6's probe).
/// * **`return new Foo(...)`** — the SAME object the assignment form binds, which is
///   what makes §4's new-vs-factory equivalence a consequence rather than a
///   coincidence: the statement's own `Callee::Construct` rung already walked the
///   constructor and left its snapshot in `ctor_heap` (ADR-0057 C7), so this arm
///   consumes it exactly as `apply_assign`'s does and never walks a second time.
///   Where the walk declined, the declaration-only object under the ADR-0086 §4
///   lexical gate stands (C6). It is minted, never stored, so it consumes no
///   allocation id.
/// * **`return g(...)` / `return $o->m(...)`** — the composition arm: the inner call's
///   own heap summary is this exit's snapshot, which is how a chained factory keeps
///   its exactness across two boundaries (§2.3's "chaining composes correctly").
#[allow(clippy::too_many_arguments)]
pub(crate) fn return_heap_object(
    w: &WalkCx,
    folder: &mut dyn Folder,
    value: &ArgValue,
    env: &HashMap<String, Known>,
    store: &Store,
    stmt_summary: Option<&ReturnSummary>,
    ctor_heap: Option<&HeapSummary>,
) -> Option<HeapObj> {
    if w.scope.poisoned {
        return None;
    }
    match value {
        ArgValue::Var(name) => store.obj_of(name).cloned(),
        ArgValue::New(class_ref, args, named) => {
            let class = w.cx.class_fqn(class_ref);
            Some(match ctor_heap {
                Some(h) => {
                    debug_assert!(
                        h.obj.class == class && h.obj.class_exact,
                        "a constructor snapshot must be the exact allocation its `new` site minted",
                    );
                    h.obj.clone()
                }
                None => new_heap_object(
                    w.cx,
                    folder,
                    &class,
                    args,
                    named,
                    env,
                    store,
                    false,
                    CtorDefaults::Lexical,
                ),
            })
        }
        _ => stmt_summary.and_then(|s| s.heap.as_ref()).map(|h| h.obj.clone()),
    }
}

/// Whether a summary value fact is precise enough to bind as the call-result's value
/// (ADR-0057 A3, amended by A8): a `Singleton`/`OneOf`/`Refined`/`Shape` fact is
/// strictly more than the declared arm floor and binds; a bare `General{base}` (the
/// degraded join) carries nothing beyond the arms — the arm floor stands, observably
/// identical to no summary.
///
/// `Shape` is here because the array stratum (ADR-0062) postdates the T0 vocabulary,
/// not because anything ever decided against it (issue #596). The layer list was
/// written when the value domain ended at `General`, and adding a fifth layer left
/// this predicate spelling "the four layers T0 knew" while meaning "sharper than the
/// arms". A shape is sharper than the arms twice over: an untyped callee has no arms
/// at all, and a declared `array`/`@return array<…>` reaches the caller through
/// `seed_returned_shape` at `Asserted` — coarser in stratum AND in content than a
/// proven `list{1, int}`.
///
/// It is the same crossing the other four layers make, on the same three grounds:
/// PHP arrays are values, so a returned array is a copy and no heap identity crosses
/// (§1's rebind argument, which for an array needs no `AllocId` at all); the summary
/// carries its own stratum and the binding keeps it (A4), so an `Asserted` shape
/// stays out of the proof layer exactly as ADR-0062 A-G9's corollary requires; and
/// the fact is already bounded where it was built (`array_literal_fact`'s
/// `SHAPE_SEED_MAX_DEPTH`, `ShapeFact`'s `SHAPE_WIDTH_LIMIT`), so the boundary adds
/// no growth vector of its own.
///
/// `General` stays out, and issue #596 measured why rather than inheriting it. A
/// summary `General` is a sound claim, so the objection is not honesty — it is that
/// binding it **displaces** the arm lane, which for that layer is the richer carrier:
/// admitting it turned `Bar::__toString(): string` with `@return class-string<T>`
/// from `class-string` into `string` (nsrt `bug-3226.php:70`), the body's `General`
/// evicting the docblock arm it was never sharper than. The three rows it improved
/// (`offset-access.php:34-36`, `unknown` → `int`) stayed in the same bucket, so the
/// measured trade is one strict loss for no bucket gain. The honest generalization —
/// bind whichever of summary and arms is sharper — needs a predicate that can see the
/// *other* lane; this one is handed a fact and nothing else.
///
/// `Union` stays out too: no measured consumer asks for it, and unlike `Shape` it has
/// no lane of its own at the binding to be sharper *than*.
pub(crate) fn summary_binds(fact: &Fact) -> bool {
    matches!(
        fact,
        Fact::Singleton(_) | Fact::OneOf(_) | Fact::Refined { .. } | Fact::Shape { .. }
    )
}
