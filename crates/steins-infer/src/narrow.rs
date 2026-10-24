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
//! `&&` at else is one path and goes to the vocabulary unchanged.
//!
//! The expansion is capped at [`MAX_PATHS`]. A node whose paths would pass the
//! cap stays one atom, so the vocabularies read it as they did before paths
//! existed: its own `||` at then contributes nothing. The cap is structural, never
//! a clock, and it names itself: every variable the capped node reads is recorded
//! in [`Store::paths_capped`], and a dump of it says `(paths capped)`.

use std::collections::HashMap;

use steins_domain::Certainty;
use steins_syntax::{CondExpr, OperandSpan};

use crate::cond::eval_cond;
use crate::env::{Known, Store, join_envs};
use crate::fold::Folder;
use crate::oneof_guard::collect_reads;
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
    let mut capped = Vec::new();
    let paths = expand(cond, then, &mut capped);
    if paths.len() > 1 {
        let mut clones = Vec::with_capacity(paths.len());
        for path in &paths {
            if refuted(w, folder, path, env, store, poisoned) {
                continue;
            }
            let (mut penv, mut pstore) = (env.clone(), store.clone());
            apply(folder, &conjunction(path), true, &mut penv, &mut pstore);
            clones.push((penv, pstore));
        }
        if !clones.is_empty() {
            (*env, *store) = join_envs(clones);
        }
    }
    apply(folder, cond, then, env, store);
    for node in capped {
        let mut reads = Vec::new();
        collect_reads(node, &mut reads);
        store.paths_capped.extend(reads);
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
