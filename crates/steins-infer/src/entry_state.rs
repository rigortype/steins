//! The state a scope's walk enters with (ADR-0048 §3): the parameters' facts and
//! contract lanes, and the `$this` and declared-parameter objects on the heap.

use std::collections::HashMap;

use steins_syntax::Scope;

use crate::cx::Cx;
use crate::descent::scope_class;
use crate::env::{Known, Store, Stratum};
use crate::heap::{seed_declared_param_object, seed_this_object};
use crate::refine::{
    expand_enum_case_arms, seed_contract_arms, seed_fact, seed_refined_scalar_fact, seed_shape_fact,
};

/// Seed the state one scope's walk enters with: the parameters' native facts, their
/// contract-arm lanes (the plain per-scope pass only, `plain`), and the `$this` and
/// declared parameter objects on the heap, each only where a descent has not already
/// bound the name.
///
/// The order is load-bearing. The declared scalar refinement replaces only the seed
/// the native pass planted (its `== native` test), so the native pass runs first; and
/// every heap id is minted here, before [`analyze_scope`] starts the walk's allocation
/// counter past them.
///
/// [`analyze_scope`]: crate::walk::analyze_scope
pub(crate) fn seed_entry_state(
    cx: &Cx,
    scope: &Scope,
    plain: bool,
    this_exact: Option<&str>,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
) {
    let enclosing_class = scope_class(scope);

    // Native-type parameter seeding (Feature B): sound in both strict and coercive
    // modes since the engine coerces or throws at entry, so inside the body an
    // `int $x` param IS an int post-coercion. A descent already binds params the
    // caller supplied; only params still absent from the env get seeded here.
    let scope_params = cx.scope_params(scope);
    if let Some(params) = scope_params {
        for p in params {
            if env.contains_key(&p.name) || store.is_bound(&p.name) {
                continue;
            }
            if let Some(fact) = seed_fact(p) {
                env.insert(p.name.clone(), Known::value(fact, 0, Some("native parameter type".to_owned())));
            }
        }
    }

    // One parse of the owning declaration's docblock serves both parameter-seed
    // lanes below — the arm lane and the declared object — so a documented scope is
    // never read twice for the same envelopes.
    let param_envelopes = scope_params.and_then(|_| cx.scope_envelopes(scope));

    // Contract-fact seeding (ADR-0052 §9), the canonical entry-state contribution
    // (ADR-0048 §3): per declared parameter, the native member list (`Verified`)
    // refined by the declared `@param` phpdoc envelope (`Asserted`, ADR-0037 trust
    // order). The arm lane lives in the walk-local `Store`; a descent that already
    // bound a param's value gets no lane. No other narrowing carrier (guard facts,
    // members, static-prop channels) contributes to entry state.
    if plain
        && let Some(params) = scope_params
    {
        let envelopes = param_envelopes.as_ref();
        for p in params {
            if store.contract.contains_key(&p.name) {
                continue;
            }
            let phpdoc = envelopes.and_then(|e| {
                // An assertion-target `@param` states a post-condition, not the
                // parameter's declared type — never seed a lane from it.
                if e.is_assert_target(&p.name) { None } else { e.param(&p.name) }
            });
            // Resolve phpdoc class arms in the param's namespace context (its offset
            // falls in the same region as the `@param` docblock), matching the FQNs
            // the `instanceof` subtrahend and S6's `find_class` use.
            let resolve = |n: &str| {
                cx.resolve_pclass(cx.cur, p.span.start, n).trim_start_matches('\\').to_ascii_lowercase()
            };
            if let Some(mut arms) = seed_contract_arms(p, phpdoc, &resolve)
                && !arms.is_empty()
            {
                // The finite enum domain (issue #429), planted here rather than
                // inside the shared refinement so the trust question is already
                // settled when it is asked: only a `Verified` class arm expands.
                expand_enum_case_arms(cx, &mut arms);
                // Abstract array stratum's entry state (ADR-0062 S3): a lane whose
                // array vocabulary collapsed to ONE arm also seeds the value lane
                // with that arm's shape fact. Multi-arm lanes seed nothing — the
                // shape∪shape union lives in the arm lane until a guard subtracts it
                // to one (A-G3, S4's job).
                if !env.contains_key(&p.name)
                    && let Some(fact) = seed_shape_fact(&arms)
                {
                    env.insert(
                        p.name.clone(),
                        // ALWAYS `Asserted` even where the arm itself is `Verified`
                        // (a native `array $x`): A-G9's corollary is normative —
                        // shape-derived facts never feed proof-layer findings.
                        Known::value_strat(
                            fact,
                            0,
                            Some("declared array shape".to_owned()),
                            Stratum::Asserted,
                        ),
                    );
                }
                // The scalar half (issue #242): the native pass already planted the
                // coarse `Fact::General`, which outranks the arm lane, so a declared
                // refinement must REPLACE it, not yield. The `== native` test
                // overwrites only the seed this same entry pass planted, never a
                // descent-bound value or guard fact.
                else if let Some(native) = seed_fact(p)
                    && env.get(&p.name).is_some_and(|k| k.fact.as_ref() == Some(&native))
                    && let Some((fact, stratum)) = seed_refined_scalar_fact(p, &native, &arms)
                {
                    env.insert(
                        p.name.clone(),
                        Known::value_strat(
                            fact,
                            0,
                            Some("declared parameter refinement".to_owned()),
                            stratum,
                        ),
                    );
                }
                store.contract.insert(p.name.clone(), arms);
            }
        }
    }

    // Seed the `$this` object in a method scope (ADR-0036): props/readonly from the
    // class surface, only when the class declares tracked properties (otherwise
    // `$this` stays unbound). A descent that already bound `this` is left untouched —
    // that is the receiver leg's seam (ADR-0086 §3): a method call on an exact
    // `Receiver::Var` hands the callee a copy of the receiver's own object, props and
    // carries included, and re-seeding the class shell over it would throw exactly the
    // knowledge the crossing bought away. Every other receiver arrives here with `this`
    // unbound and is seeded below, as before.
    if let Some(class) = enclosing_class
        && !store.is_bound("this")
    {
        // G1: `$this`'s heap class is a lower bound (any subclass instance may be
        // running this method) unless exactness is locally provable — a descent
        // that proved the exact receiver (`this_exact`), or the enclosing class
        // itself when `final` or an enum.
        let (this_class, exact): (&str, bool) = match this_exact {
            Some(exact) => (exact, true),
            None => (class, cx.this_class_exact(class)),
        };
        if let Some(obj) = seed_this_object(cx, this_class, exact) {
            let id = store.heap.keys().copied().max().map_or(0, |m| m + 1);
            store.heap.insert(id, obj);
            store.refs.insert("this".to_owned(), id);
        }
    }

    // Seed the **declared** parameter objects (ADR-0032's 2026-08-16 amendment,
    // issue #388): a parameter that is an object by declaration enters its scope on
    // the heap wherever no ADR-0086 copy landed — the plain per-scope pass, which
    // has never given a parameter a `HeapObj` at all, and a descent whose argument
    // resolved to no object. A parameter the caller already bound is left alone in
    // both lanes: a copied object in `refs` and a proven value in `env` are each
    // stronger than the declaration that would have stood in for them.
    if let Some(params) = scope_params {
        let shadow = cx.scope_template_shadow(scope);
        for p in params {
            if env.contains_key(&p.name) || store.is_bound(&p.name) {
                continue;
            }
            // An assertion-target `@param` states a post-condition, not the
            // parameter's declared type — read as absent here exactly as the arm
            // lane reads it.
            let phpdoc = param_envelopes.as_ref().and_then(|e| {
                if e.is_assert_target(&p.name) { None } else { e.param(&p.name) }
            });
            if let Some(obj) = seed_declared_param_object(cx, p, phpdoc, &shadow) {
                let id = store.heap.keys().copied().max().map_or(0, |m| m + 1);
                store.heap.insert(id, obj);
                store.refs.insert(p.name.clone(), id);
            }
        }
    }
}
