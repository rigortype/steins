//! Assignment into the env: `$var = <value>` (extracted from the walk), and the
//! [`AssignLhs`] its arms bind through. The rungs below the literal rung are in
//! `call`; the fact of a `??` chain is in `coalesce`.

mod call;
mod coalesce;

use std::collections::HashMap;

use steins_domain::Fact;
use steins_syntax::{ArgValue, CallExpr};

use crate::fold::Folder;
use crate::annotate::{FactKind, LineFact};
use crate::builtin_returns::escape_mentioned_resources;
use crate::cond::{eval_ternary_fact_strat, total_op_fact};
use crate::env::{
    ContractArm, HeapSummary, Known, ReturnSummary, Store, Stratum, render_val, singleton_fact,
};
use crate::heap::{build_closure_val, build_new_object};
use crate::offsets::shape_read_at;
use crate::project::Diagnostic;
use crate::walk::WalkCx;

use call::{bind_call_places, bind_unfolded};
pub(crate) use coalesce::{
    coalesce_projection, cover_discharges, eval_coalesce_fact, flatten_coalesce,
};

/// The target of `$var = <value>;`: the variable, the statement's line, and the
/// line-fact sink the margin display reads (ADR-0020).
///
/// The arms of [`apply_assign`] land their answers through these methods, so a
/// rebinding drops the old binding's heap, place and arm lanes ([`Store::unbind`])
/// together with its value lane. [`bind`](Self::bind) also shows a literal on the
/// line and [`bind_quiet`](Self::bind_quiet) does not. The arms that bind quietly —
/// an offset read, `??`, an array literal, `::class`, the shape rung and the
/// envelope, and beside them a property read and the declared floor — have never
/// shown one; that asymmetry is older than this struct and kept as it is.
struct AssignLhs<'a> {
    var: &'a str,
    line: u32,
    facts: Option<&'a mut Vec<LineFact>>,
}

impl AssignLhs<'_> {
    /// Bind the value lane to `fact` at `strat`, and show it on the line when it
    /// is one literal.
    fn bind(
        &mut self,
        env: &mut HashMap<String, Known>,
        store: &mut Store,
        fact: Fact,
        strat: Stratum,
    ) {
        self.note_literal(&fact);
        self.bind_quiet(env, store, fact, strat);
    }

    /// Bind the value lane to `fact` at `strat`, showing nothing on the line.
    fn bind_quiet(
        &self,
        env: &mut HashMap<String, Known>,
        store: &mut Store,
        fact: Fact,
        strat: Stratum,
    ) {
        self.bind_known(env, store, Known::value_strat(fact, self.line, None, strat));
    }

    /// Bind the value lane to `known` as built, for the two arms that name where
    /// the fact came from.
    fn bind_known(&self, env: &mut HashMap<String, Known>, store: &mut Store, known: Known) {
        env.insert(self.var.to_owned(), known);
        store.unbind(self.var);
    }

    /// Drop the binding from every lane: the arm binds no value.
    fn clear(&self, env: &mut HashMap<String, Known>, store: &mut Store) {
        env.remove(self.var);
        store.unbind(self.var);
    }

    /// Show `fact` on the line when it is one literal.
    fn note_literal(&mut self, fact: &Fact) {
        if let Fact::Singleton(v) = fact {
            self.note(FactKind::Value { var: self.var.to_owned(), rendered: render_val(v) });
        }
    }

    fn note(&mut self, kind: FactKind) {
        if let Some(facts) = self.facts.as_deref_mut() {
            facts.push(LineFact { line: self.line, kind });
        }
    }
}

/// Apply a plain `$var = <value>;` assignment to the env (extracted from the walk).
/// `return_arms` is the declared return floor resolved at the call site **before**
/// this assignment may unbind its own target (self-assign `$o = $o->m(1)`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_assign(
    w: &WalkCx,
    folder: &mut dyn Folder,
    var: &str,
    value: &ArgValue,
    call: Option<&CallExpr>,
    span_start: u32,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
    facts: &mut Option<&mut Vec<LineFact>>,
    summary: Option<&ReturnSummary>,
    // The constructor descent's `$this` snapshot for a `new` right-hand side
    // (ADR-0057 C4): the fresh allocation IS that snapshot, the allocation having had
    // no alias before the constructor ran.
    ctor_heap: Option<&HeapSummary>,
    return_arms: Option<&[ContractArm]>,
    out: &mut Vec<Diagnostic>,
) {
    let cx = w.cx;
    let line = cx.tree().position(span_start).line;
    let mut lhs = AssignLhs { var, line, facts: facts.as_deref_mut() };

    // Any rvalue other than a plain copy that names a handle stores it somewhere
    // this walk cannot follow — an array literal, a ternary arm, a `(array)`
    // cast, a closure's capture (ADR-0097 §2.4): the handle escapes, before any
    // rung below binds. A plain `$b = $h` is the one rvalue that ALIASES instead
    // (the `Var` arm of the match below shares the heap entry), and a call's
    // arguments are the statement's call effects, judged on the pre-call store.
    if !matches!(value, ArgValue::Var(_) | ArgValue::Call(..) | ArgValue::MethodCall { .. }) {
        escape_mentioned_resources(value, store);
    }

    // A ternary rvalue `$x = $c ? A : B` (ADR-0031): the walk evaluates the guard
    // and resolves to the chosen arm, or (undecided) a `OneOf` of both when
    // literal, else unknown.
    if let ArgValue::Ternary { cond, then_val, then_span, else_val, else_span } = value {
        match eval_ternary_fact_strat(
            w,
            folder,
            cond,
            then_val,
            else_val,
            (*then_span, *else_span),
            env,
            store,
        ) {
            Some((fact, strat)) => lhs.bind(env, store, fact, strat),
            None => lhs.clear(env, store),
        }
        return;
    }

    // An operator rvalue `$b = $x > 3;`, `$s = $a . $b;`, `$b = isset($a['k']);`
    // and the rest of `total_op_fact`'s seven: the operator's fact, by the same
    // dispatch the dump surface and a returning exit read, so the three can never
    // disagree. Total, so the binding is the operator's floor at worst and never
    // dropped; a `ValueOp::BitOr` (issue #615) falls through and binds nothing.
    if let Some((fact, strat)) =
        total_op_fact(w, folder, value, env, Some(&*store), w.scope.poisoned)
    {
        lhs.bind(env, store, fact, strat);
        return;
    }

    // A closure value (ADR-0033): record a `ClosureVal` with its by-value capture
    // snapshot from the current (definition-site) env. A poisoned scope drops it.
    if let ArgValue::Closure(cref) = value {
        lhs.clear(env, store);
        if w.scope.poisoned {
            return;
        }
        // A closure that captures an object escapes it (ADR-0036): the closure holds
        // the object handle, so an unknown call may reach and mutate it.
        if let steins_syntax::ClosureRef::Anonymous { captures, .. } = cref {
            for name in captures {
                store.mark_escaped(name);
            }
        }
        if let Some(cv) = build_closure_val(cx, cref, line, env) {
            env.insert(var.to_owned(), Known::closure(cv, line));
        }
        return;
    }

    // The declared-arm lane travels across a plain copy `$c = $o` (ADR-0052 §9,
    // issue #196 piece 2): a copy binds the same value, so declared possibilities
    // carry over at the same stratum. Read before the match (a self-assign `$a =
    // $a` would otherwise unbind `var` and lose the source's own lane); written
    // after, since every match arm for a `Var` rvalue drops `var`'s lane without
    // replacing it.
    let copied_arms: Option<Vec<ContractArm>> = match value {
        ArgValue::Var(src) if !w.scope.poisoned => store.contract.get(src).cloned(),
        _ => None,
    };

    match value {
        // `$x = new Foo(args)` (ADR-0036): a fresh allocation, class from resolution,
        // props populated from promoted ctor params + literal defaults.
        ArgValue::New(class_ref, args, named) => {
            lhs.clear(env, store);
            if !w.scope.poisoned {
                let class = cx.class_fqn(class_ref);
                let id = build_new_object(w, folder, &class, args, named, env, store, ctor_heap);
                store.refs.insert(var.to_owned(), id);
                lhs.note(FactKind::ExactClass { var: var.to_owned(), class });
            }
        }
        // `$b = $a` where `$a` holds an object (ADR-0036 aliasing): copy the ObjRef
        // (shared id), so a later write through either alias is visible via both.
        ArgValue::Var(src) if !w.scope.poisoned && store.is_bound(src) => {
            env.remove(var);
            let id = store.id_of(src).expect("bound var has an id");
            store.refs.insert(var.to_owned(), id);
        }
        // `clone $a` (ADR-0036 adversarial #1): a NEW id with a COPY of the source
        // object's props (PHP shallow clone) — post-clone writes stay isolated.
        ArgValue::Clone(src) if !w.scope.poisoned && store.is_bound(src) => {
            // Read the source id before unbinding `var`. For a self-clone
            // `$a = clone $a`, `var == src`, so unbinding first would drop `src`'s
            // binding too. PHP evaluates the rvalue before assigning, so the
            // pre-assignment id is the correct one to capture.
            let src_id = store.id_of(src).expect("bound var has an id");
            lhs.clear(env, store);
            if let Some(src_obj) = store.heap.get(&src_id) {
                let mut copy = src_obj.clone();
                copy.escaped = false; // a fresh, local clone has not escaped
                let id = w.fresh_id();
                store.heap.insert(id, copy);
                store.refs.insert(var.to_owned(), id);
            }
        }
        // `$x = $o->p` (ADR-0036): a property read flows the prop's fact into `$x`,
        // carrying the prop's stratum (derivation clause — heap reads).
        ArgValue::PropFetch { var: recv, prop } if !w.scope.poisoned => {
            lhs.clear(env, store);
            if let Some(fact) = store.prop_fact(recv, prop).cloned() {
                let strat = store.prop_stratum(recv, prop);
                env.insert(var.to_owned(), Known::value_strat(fact, line, None, strat));
            }
        }
        // `$x = $base[k]` where `$base` carries an abstract shape (ADR-0062 §4's
        // read row, S3): a constant-key read takes the declared field's value slot.
        // A key with no fact behind it binds nothing — never value∪null (A-G9:
        // missing-ness is the strict leg's finding, never type pollution). The
        // whitelisted `offset.missing` judgment for this statement already ran
        // independently (step 1z).
        ArgValue::OffsetRead { base, key } => {
            // Resolve against the PRE-assignment env: PHP evaluates the rvalue
            // first, so a self-read `$a = $a['k']` still reads the old `$a`.
            let read = shape_read_at(base, key, env, w.scope.poisoned)
                .and_then(|(read, strat)| Some((read.into_fact()?, strat)));
            match read {
                Some((fact, strat)) => lhs.bind_quiet(env, store, fact, strat),
                None => lhs.clear(env, store),
            }
        }
        // `$x = $a ?? $b` (ADR-0052 §6): `clear_null(fact($a)) join fact($b)`. A
        // fact only when BOTH operands are visible facts, so `??` never manufactures
        // certainty for a value it cannot spell. The join widens, so it can only
        // lose precision — the FP-safe side.
        ArgValue::Coalesce(a, b, rhs_span) => {
            // `??` gates its right operand like a ternary gates an arm: an arm
            // proven set-and-non-null means PHP never evaluates what follows it.
            // The evaluator owns that record now (issue #630) — it needs the same
            // predicate to pick the value, and one predicate cannot disagree with
            // itself. Stratum is the evaluator's own `min` over the spine's arms.
            match eval_coalesce_fact(w, folder, a, b, *rhs_span, env, Some(&*store)) {
                Some((fact, strat)) => lhs.bind_quiet(env, store, fact, strat),
                None => lhs.clear(env, store),
            }
        }
        _ => match cx
            .resolve_literal_strat_ex(
                value,
                env,
                w.scope.poisoned,
                folder,
                None,
                Some(&mut *out),
            )
            .and_then(|(lit, strat)| singleton_fact(&lit).map(|f| (lit, f, strat)))
        {
            Some((lit, fact, strat)) => {
                lhs.note(FactKind::Value { var: var.to_owned(), rendered: lit.render() });
                // Derivation clause: folds and array composition resolve through
                // `resolve_literal`, consuming env facts and nested project-call
                // summary strata (issue #127) — stamp that min. Nested descents for
                // fold args emit through `out` so findings under `strtoupper(g(1))`
                // aren't discarded.
                lhs.bind_quiet(env, store, fact, strat);
            }
            // `$x = is_int($y)` and kin: the fold could not reach it, so seed the
            // uniquely-resolved builtin's reflected return envelope (ADR-0056 R1).
            // Enters at `Verified` — a native declaration (§2).
            None => bind_unfolded(
                w, folder, &mut lhs, value, call, env, store, summary, return_arms,
            ),
        },
    }

    // The array-of-handles producer rung (ADR-0098 §2.2): `$pair =
    // stream_socket_pair(…)` binds two element PLACES where every rung above
    // binds one variable. It runs after the match rather than inside an arm of
    // it, because which arm answered the value lane is the engine's business —
    // the reflected envelope seeds `array|false` with a live sidecar and the
    // declared floor seeds the same arms without one — while the places are the
    // same answer either way. Every arm above has already run `unbind(var)`,
    // which is what drops the previous binding's places (§2.3's sweep), so the
    // places minted here are this call's and no earlier one's.
    bind_call_places(w, folder, var, value, store);

    if let Some(arms) = copied_arms {
        store.contract.insert(var.to_owned(), arms);
    }
}
