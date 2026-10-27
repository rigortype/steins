//! The path driver (ADR-0106 §2.1 and §2.2, issue #1083): a condition narrows
//! along the paths PHP takes through it.
//!
//! Every narrowing vocabulary distributes `&&` on the true side and `||` on the
//! false side, and contributes nothing for the other two. A branch entered through
//! `a || b` is the join of the path entered through `a` and the path entered
//! through `!a && b`, and the false side of `a && b` is the join of `!a` and
//! `a && !b`. [`narrow_paths`] expands a condition into those paths at the
//! requested polarity, runs the site's own vocabulary once per path, each path
//! rebuilt as a conjunction, on a clone of `(env, store)`, and merges the clones
//! with [`join_envs`], the merge every `if` ends with.
//!
//! A path is applied whole rather than atom by atom, so the order inside it is the
//! order the vocabularies already depend on (`is_string($v) && $v !== ''` mints
//! `string`, then `non-empty-string`). A condition with no `||` at then and no
//! `&&` at else is one path and goes to the vocabulary unchanged. Around the join
//! the driver leaves out a path the walk refutes, applies the whole condition once
//! more, and keeps the join from knowing less than the site did ([`narrow_paths`]).
//!
//! The expansion is capped at [`MAX_PATHS`]. A node whose paths would pass the
//! cap stays one atom, so the vocabularies read it as they did before paths
//! existed: its own `||` at then contributes nothing. The cap is structural, never
//! a clock, and it names itself: every variable the capped node reads is recorded
//! in [`Store::paths_capped`], and a dump of it says `(paths capped)`.
//!
//! A test of a variable that holds a stored condition (ADR-0106 §2.3) is expanded
//! first, into the test and the condition together, so the condition's paths are
//! the variable's ([`with_stored_conditions`]).

use std::collections::HashMap;

use steins_domain::{Certainty, Fact};
use steins_phpdoc::AssertKind;
use steins_syntax::{CondExpr, OperandSpan};

use crate::asserts::apply_guard_asserts;
use crate::cond::eval_cond;
use crate::env::{Known, Store, Stratum, join_envs};
use crate::existence::{existence_vouch, with_stored_conditions};
use crate::fold::Folder;
use crate::oneof_guard::collect_reads;
use crate::out_params::{seed_out_params, seed_produced_places};
use crate::predicates::apply_type_narrowing;
use crate::refine::{
    apply_class_narrowing, apply_refinements, collect_guard_calls, collect_guard_vars,
    collect_same_expr_call_guards, else_refinements, then_refinements,
};
use crate::remembered;
use crate::shapes::apply_shape_narrowing;
use crate::walk::WalkCx;

/// The most paths one condition expands into (ADR-0106 §2.1, D2).
pub(crate) const MAX_PATHS: usize = 8;

/// One step of a path: a leaf of the condition, or a node left whole at the cap,
/// and the polarity it holds at on that path.
type Atom<'c> = (&'c CondExpr, bool);

/// A site's vocabulary: everything it applies for `cond` holding at `then`.
pub(crate) type Vocabulary<'v> =
    dyn FnMut(&mut dyn Folder, &CondExpr, bool, &mut HashMap<String, Known>, &mut Store) + 'v;

/// Apply `cond` at `then` to `(env, store)` through `apply`, path by path.
///
/// One path (the common case) calls `apply` once on `cond` itself. Several call it
/// once per live path, each on a clone and on a conjunction of the path's atoms at
/// true polarity, and leave the join of the clones in `(env, store)`.
///
/// Two steps keep the join from losing what a single application knew:
///
/// - **A refuted path is left out.** A path one of whose atoms the walk decides
///   false under `env` (`$x = 'abc'` and the `!is_string($x)` path of `is_string($x)
///   && …` at else) is never taken. Applied, its refuted atom would drop the
///   binding the way a refuted guard does (the verdict owns death), and the join
///   would drop it with it. A node left whole at the cap is not evaluated.
/// - **A path its own narrowing empties is left out** ([`path_fate`]): over `int
///   $n`, the path `!is_int($n) && is_string($n)` of `is_int($n) || is_string($n)`
///   deletes the last arm of `$n`, so the `string` the next atom would mint is a
///   value no run holds. A docblock's claim emptied is not a proof of that, so
///   such a path is kept with the name as the site held it.
/// - **`cond` itself is applied once more over the join.** What a vocabulary proves
///   of the whole condition at once, the presence cover of `isset($a['x']) ||
///   isset($a['y'])` (ADR-0062 A-G11), is a fact no single path holds, so the join
///   cannot rebuild it. Every such fact holds wherever `cond` does, so applying
///   `cond` to the join is sound, and it is the application a site made before
///   paths existed.
#[allow(clippy::too_many_arguments)]
pub(crate) fn narrow_paths(
    w: &WalkCx,
    folder: &mut dyn Folder,
    cond: &CondExpr,
    then: bool,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
    poisoned: bool,
    apply: &mut Vocabulary<'_>,
) {
    // A tested variable that holds a stored condition narrows by that condition too
    // (ADR-0106 §2.3): `$ok = is_int($n) || is_float($n); if ($ok)` takes both paths.
    let stored = with_stored_conditions(cond, then, store);
    let cond = stored.as_ref().unwrap_or(cond);
    let mut capped = Vec::new();
    let paths = expand(cond, then, &mut capped);
    if paths.len() > 1 {
        let mut clones = Vec::with_capacity(paths.len());
        for path in &paths {
            if refuted(w, folder, path, env, store, poisoned) {
                continue;
            }
            let Some(reset) = path_fate(folder, path, env, store, apply) else { continue };
            let (mut penv, mut pstore) = (env.clone(), store.clone());
            apply(folder, &conjunction(path), true, &mut penv, &mut pstore);
            for name in reset {
                restore_site_state(&name, env, store, &mut penv, &mut pstore);
            }
            clones.push((penv, pstore));
        }
        if !clones.is_empty() {
            let (mut jenv, mut jstore) = join_envs(clones);
            settle_join(env, store, &mut jenv, &mut jstore);
            (*env, *store) = (jenv, jstore);
        }
    }
    apply(folder, cond, then, env, store);
    for node in capped {
        let mut reads = Vec::new();
        collect_reads(node, &mut reads);
        store.paths_capped.extend(reads);
    }
}

/// What one carrier of a name holds before an atom: the value fact's stratum, and
/// whether a non-empty declared-type lane is bound.
#[derive(Clone, Copy)]
struct Carriers {
    fact: Option<Stratum>,
    lane: bool,
}

fn carriers(env: &HashMap<String, Known>, store: &Store, name: &str) -> Carriers {
    Carriers {
        fact: env.get(name).and_then(|k| k.fact.as_ref().map(|_| k.stratum)),
        lane: store.contract.get(name).is_some_and(|arms| !arms.is_empty()),
    }
}

/// Whether the site's own carriers of `name` are all runtime evidence: a value fact
/// or a declared-type lane is bound, the fact is `Verified`, and every arm is.
/// Judged at the site, before any atom: a `===` or a type test the path applies
/// mints a `Verified` value over a docblock's lane, and that must not turn the
/// docblock's claim into the proof that drops a path.
fn site_verified(env: &HashMap<String, Known>, store: &Store, name: &str) -> bool {
    let fact = env.get(name).and_then(|k| k.fact.as_ref().map(|_| k.stratum));
    let lane = store.contract.get(name);
    (fact.is_some() || lane.is_some_and(|arms| !arms.is_empty()))
        && fact.is_none_or(|s| s == Stratum::Verified)
        && lane.is_none_or(|arms| arms.iter().all(|a| a.stratum == Stratum::Verified))
}

/// Whether `path` can be taken, judged under its own narrowing: its atoms are
/// applied one by one to a scratch clone, and each name an atom reads is compared
/// before and after it. `None` drops the path; `Some(names)` keeps it, with the
/// names whose site state the kept path must carry instead of its own.
///
/// | the site's carriers | before the atom | after the atom | path |
/// | --- | --- | --- | --- |
/// | all `Verified` | a non-empty lane | the lane kept empty | dropped |
/// | all `Verified` | a `Verified` value fact | no value fact | dropped |
/// | any `Asserted` | a carrier | emptied, or no carrier | kept, name reset |
/// | any | a carrier | no carrier (a shape lane removed) | kept, name reset |
/// | none | anything | anything | kept, nothing joined |
/// | any | anything | a carrier left | kept |
///
/// The stratum is the site's, never the scratch clone's: over `@param 'a'|'b'`, the
/// path `$s !== 'a' && $s === 'c'` pins a `Verified` value at its first atom, but
/// the lane its second atom empties is a docblock's claim, PHP takes the path for
/// `'c'`, and dropping it would let the join prove `'a'`. Only an all-`Verified`
/// lane is kept empty (by a type predicate, a null or literal comparison, an
/// `instanceof` or an enum case); a lane holding an `Asserted` arm, and a shape
/// lane, are removed instead. A value fact goes when a predicate refutes it, a
/// comparison excludes its last member or a range empties. "Nothing joined" is
/// [`settle_join`]'s rule for a name the site held no carrier for.
///
/// A remembered call result or a stored guard binds no carrier of a name, so an
/// atom over one never drops a path. An atom the vocabularies cannot model applies
/// nothing and changes no row.
fn path_fate(
    folder: &mut dyn Folder,
    path: &[Atom<'_>],
    env: &HashMap<String, Known>,
    store: &Store,
    apply: &mut Vocabulary<'_>,
) -> Option<Vec<String>> {
    let (mut senv, mut sstore) = (env.clone(), store.clone());
    let mut reset = Vec::new();
    for atom in path {
        let mut names = Vec::new();
        collect_reads(atom.0, &mut names);
        names.sort_unstable();
        names.dedup();
        let before: Vec<Carriers> = names.iter().map(|n| carriers(&senv, &sstore, n)).collect();
        apply(folder, &conjunction(std::slice::from_ref(atom)), true, &mut senv, &mut sstore);
        for (name, was) in names.into_iter().zip(before) {
            let now = carriers(&senv, &sstore, &name);
            let emptied = (was.lane && sstore.contract_emptied(&name))
                || (was.fact == Some(Stratum::Verified) && now.fact.is_none());
            if emptied && site_verified(env, store, &name) {
                return None;
            }
            if (was.fact.is_some() || was.lane) && now.fact.is_none() && !now.lane {
                reset.push(name);
            }
        }
    }
    Some(reset)
}

/// Put `name` back as the site held it in a path's clone: its value fact, its
/// declared-type lane and its narrowed mark.
fn restore_site_state(
    name: &str,
    env: &HashMap<String, Known>,
    store: &Store,
    penv: &mut HashMap<String, Known>,
    pstore: &mut Store,
) {
    match env.get(name) {
        Some(k) => penv.insert(name.to_owned(), k.clone()),
        None => penv.remove(name),
    };
    match store.contract.get(name) {
        Some(arms) => pstore.contract.insert(name.to_owned(), arms.clone()),
        None => pstore.contract.remove(name),
    };
    if store.narrowed.contains(name) {
        pstore.narrowed.insert(name.to_owned());
    } else {
        pstore.narrowed.remove(name);
    }
}

/// Keep the join of the paths from knowing less than the site did, or knowing it in a
/// form the next guard cannot narrow.
///
/// - **A nullable cross-base union the paths minted is dropped.** Over a name the
///   site held no value fact for, `is_int($t)`, `is_string($t)` and `$t === null`
///   mint `int`, `string` and `null` on their paths, and their join is the value
///   lane's nullable `int|string` union. A `!== null` does not clear that union's
///   `null` (`clear_null` leaves a union unchanged), so the `null` arm would outlive
///   a `$t === null` early return and reach the code after it, which PHP never
///   does. Without it the name falls back to its declared-type lane, as it did
///   before paths existed.
/// - **A name the site held no carrier for gains none.** Over an undeclared `$n`,
///   or one a loop's entry forgot, `is_int($n) || is_string($n)` mints `int` and
///   `string` on its paths, and their join reads as a value that may be either. The
///   path that mints `string` may be one no run takes (the loop forgot that `$n` is
///   an `int`), and nothing the site knew rules it out, so a possibly-grade finding
///   over that arm would be a claim the walk cannot back. The name keeps what the
///   site had, which is nothing; the whole condition applied over the join still
///   adds what it adds alone.
/// - **A declared-type lane the join dropped comes back unnarrowed** where the joined
///   name holds no value fact either. The join drops a lane one path lacks (`$x ===
///   null` unbinds it and pins the value instead, and a value one path pins joins
///   away), so `if ($x === null || $x->m())` over `?Foo` would know nothing of `$x`.
///   The site's lane holds of every path, so it is the sound fallback.
fn settle_join(
    env: &HashMap<String, Known>,
    store: &Store,
    jenv: &mut HashMap<String, Known>,
    jstore: &mut Store,
) {
    let held = |name: &str| env.contains_key(name) || store.contract.contains_key(name);
    jenv.retain(|name, k| {
        held(name)
            && (env.contains_key(name)
                || !matches!(k.fact, Some(Fact::Union { nullable: true, .. })))
    });
    jstore.contract.retain(|name, _| held(name));
    for (name, arms) in &store.contract {
        if jstore.contract.contains_key(name) || jenv.contains_key(name) {
            continue;
        }
        jstore.contract.insert(name.clone(), arms.clone());
        if store.narrowed.contains(name) {
            jstore.narrowed.insert(name.clone());
        }
    }
}

/// Whether the walk decides one of `path`'s atoms false under `(env, store)`. Only
/// a leaf is evaluated: a node left whole at the cap is a connective, whose
/// evaluation records the operand it short-circuits as dead.
fn refuted(
    w: &WalkCx,
    folder: &mut dyn Folder,
    path: &[Atom<'_>],
    env: &HashMap<String, Known>,
    store: &Store,
    poisoned: bool,
) -> bool {
    path.iter().any(|&(atom, holds)| {
        if matches!(atom, CondExpr::And(..) | CondExpr::Or(..) | CondExpr::Not(_)) {
            return false;
        }
        let verdict = eval_cond(w, folder, atom, env, store, poisoned);
        verdict == if holds { Certainty::No } else { Certainty::Yes }
    })
}

/// The paths `cond` holds at `then` along, each a sequence of atoms in source
/// order. A node past the cap is pushed to `capped` and stays one atom.
fn expand<'c>(
    cond: &'c CondExpr,
    then: bool,
    capped: &mut Vec<&'c CondExpr>,
) -> Vec<Vec<Atom<'c>>> {
    let paths = match cond {
        CondExpr::Not(inner) => return expand(inner, !then, capped),
        // `a && b` holds along `a` then `b`; `a || b` fails along `!a` then `!b`.
        CondExpr::And(a, b, _) if then => {
            product(expand(a, true, capped), &expand(b, true, capped))
        }
        CondExpr::Or(a, b, _) if !then => {
            product(expand(a, false, capped), &expand(b, false, capped))
        }
        // `a || b` holds along `a`, or along `!a` then `b`.
        CondExpr::Or(a, b, _) => {
            let mut paths = expand(a, true, capped);
            paths.extend(product(expand(a, false, capped), &expand(b, true, capped)));
            paths
        }
        // `a && b` fails along `!a`, or along `a` then `!b`.
        CondExpr::And(a, b, _) => {
            let mut paths = expand(a, false, capped);
            paths.extend(product(expand(a, true, capped), &expand(b, false, capped)));
            paths
        }
        _ => return vec![vec![(cond, then)]],
    };
    if paths.len() > MAX_PATHS {
        capped.push(cond);
        return vec![vec![(cond, then)]];
    }
    paths
}

/// Every path of `left` followed by every path of `right`.
fn product<'c>(left: Vec<Vec<Atom<'c>>>, right: &[Vec<Atom<'c>>]) -> Vec<Vec<Atom<'c>>> {
    if let [only] = right {
        return left
            .into_iter()
            .map(|mut p| {
                p.extend_from_slice(only);
                p
            })
            .collect();
    }
    let mut out = Vec::with_capacity(left.len() * right.len());
    for l in &left {
        for r in right {
            let mut p = l.clone();
            p.extend_from_slice(r);
            out.push(p);
        }
    }
    out
}

/// A path as the condition that holds at true along it: its atoms, the false ones
/// under `Not`, chained by `&&` left to right. The connectives the driver
/// synthesizes carry no operand extent ([`OperandSpan::NONE`]).
fn conjunction(path: &[Atom<'_>]) -> CondExpr {
    let atom = |&(c, holds): &Atom<'_>| {
        if holds { c.clone() } else { CondExpr::Not(Box::new(c.clone())) }
    };
    let mut atoms = path.iter();
    let first = atoms
        .next()
        .map_or(CondExpr::Opaque { reads: Vec::new(), writes: Vec::new() }, atom);
    atoms.fold(first, |acc, a| CondExpr::And(Box::new(acc), Box::new(atom(a)), OperandSpan::NONE))
}

/// The vocabularies over one path of a condition, in the order [`apply_cond_side`]
/// documents: the one list every site passes to [`narrow_paths`] (ADR-0106, #1096).
///
/// [`apply_cond_side`]: crate::branch::apply_cond_side
pub(crate) fn apply_vocabularies(
    w: &WalkCx,
    folder: &mut dyn Folder,
    cond: &CondExpr,
    then: bool,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
) {
    apply_type_narrowing(w.cx, cond, then, env, store);
    let refs = if then {
        then_refinements(cond)
    } else {
        else_refinements(cond)
    };
    apply_refinements(&refs, env, store, Stratum::Verified);
    apply_class_narrowing(w, folder, cond, then, store);
    apply_shape_narrowing(w.cx, cond, then, env, store, true);
    // Same-expression call guards (issue #421): a call has no binding to narrow, so
    // this records the guard rather than a fact — only on the side where the "not
    // null"/"not false" reading actually holds, so a sibling branch that never
    // tested the expression does not inherit it through the join (`join_stores`
    // intersects, same as `vouched`). The `false` polarity is the twin reading,
    // `if ($e === null) {} else { <here> }`.
    let mut guards = Vec::new();
    collect_same_expr_call_guards(cond, then, &mut guards);
    for v in guards {
        store.guard_call(v);
    }
    let mut calls = Vec::new();
    collect_guard_calls(cond, then, &mut calls);
    for (call, returns_true) in calls {
        let kind = if returns_true { AssertKind::IfTrue } else { AssertKind::IfFalse };
        apply_guard_asserts(w, call, kind, env, store);
        // Guard-respect leg (ADR-0049 §4): an existence guard vouches its symbol on
        // the side where it holds true — including the negated-guard else-branch,
        // `if (!method_exists(...)) {} else <here>`.
        if returns_true {
            for v in existence_vouch(w.cx, store, call) {
                store.vouch(v);
            }
        }
    }
    // A variable that holds an existence guard vouches it on the side where it is truthy
    // (ADR-0104): `$has = function_exists('f'); if ($has) { f(); }`.
    if !store.guard_vars.is_empty() {
        let mut tested = Vec::new();
        collect_guard_vars(cond, then, &mut tested);
        for (var, truthy) in tested {
            let Some(g) = store.guard_vars.get(var).cloned() else { continue };
            let mut calls = Vec::new();
            collect_guard_calls(&g.cond, truthy, &mut calls);
            for (call, returns_true) in calls {
                if returns_true {
                    for v in existence_vouch(w.cx, store, call) {
                        store.vouch(v);
                    }
                }
            }
        }
    }
    // The out-parameter seed on both sides, for the same reason: `if
    // (!preg_match($re, $s, $m)) { return; }` reaches its else-branch with the call
    // proven truthy (ADR-0077 §3.1) — the polarity the witness survives under decides.
    seed_out_params(w, folder, cond, then, env, store);
    // The out-parameter **places** a guard call filled (ADR-0098 §2.2), on the
    // polarity that proves the call returned truthy — the strongest position
    // the `proc_open` row has, and the one `if (proc_open($c, $s, $pipes))`
    // lands in.
    seed_produced_places(w, folder, cond, then, env, store);
    // Last: what the condition proves about the calls it names, remembered on the
    // key so that the next identical call answers it (ADR-0102). After every
    // vocabulary above, which may have forgotten a name a key stands on.
    remembered::produce(w, folder, cond, then, env, store);
}

#[cfg(test)]
mod tests {
    use super::*;
    use steins_syntax::CondOperand;

    fn var(name: &str) -> CondExpr {
        CondExpr::Truthy(CondOperand::Var(name.to_owned()))
    }

    fn or(a: CondExpr, b: CondExpr) -> CondExpr {
        CondExpr::Or(Box::new(a), Box::new(b), OperandSpan::NONE)
    }

    fn and(a: CondExpr, b: CondExpr) -> CondExpr {
        CondExpr::And(Box::new(a), Box::new(b), OperandSpan::NONE)
    }

    fn not(a: CondExpr) -> CondExpr {
        CondExpr::Not(Box::new(a))
    }

    fn shape(c: &CondExpr, then: bool) -> (Vec<String>, usize) {
        let mut capped = Vec::new();
        let paths = expand(c, then, &mut capped);
        let spelled = paths
            .iter()
            .map(|p| {
                p.iter()
                    .map(|(c, holds)| {
                        let CondExpr::Truthy(CondOperand::Var(v)) = c else {
                            return "*".to_owned();
                        };
                        if *holds { v.clone() } else { format!("!{v}") }
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();
        (spelled, capped.len())
    }

    #[test]
    fn a_disjunction_at_then_and_a_conjunction_at_else_split() {
        assert_eq!(shape(&or(var("a"), var("b")), true).0, ["a", "!a b"]);
        assert_eq!(shape(&and(var("a"), var("b")), false).0, ["!a", "a !b"]);
        // `Not` flips, so `!(a || b)` at else is `a || b` at then.
        assert_eq!(shape(&not(or(var("a"), var("b"))), false).0, ["a", "!a b"]);
    }

    #[test]
    fn a_conjunction_at_then_and_a_disjunction_at_else_stay_one_path() {
        assert_eq!(shape(&and(var("a"), var("b")), true).0, ["a b"]);
        assert_eq!(shape(&or(var("a"), var("b")), false).0, ["!a !b"]);
        // A conjunction over a split operand multiplies.
        let c = and(or(var("a"), var("b")), var("c"));
        assert_eq!(shape(&c, true).0, ["a c", "!a b c"]);
    }

    #[test]
    fn a_node_past_the_cap_stays_one_atom() {
        let nine = (1..9).fold(var("v0"), |acc, i| or(acc, var(&format!("v{i}"))));
        let (paths, capped) = shape(&nine, true);
        assert_eq!((paths, capped), (vec!["*".to_owned()], 1));
        let eight = (1..8).fold(var("v0"), |acc, i| or(acc, var(&format!("v{i}"))));
        let (paths, capped) = shape(&eight, true);
        assert_eq!((paths.len(), capped), (8, 0));
    }
}
