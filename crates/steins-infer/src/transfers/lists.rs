//! The list transfers: `explode` and `range`, which always answer a non-empty list,
//! and [`list_transfer_fact`], the `list<T>` constructor the array out-state rows and
//! the out-parameter seeds build with too.

use std::collections::HashMap;

use steins_domain::{Base, Certainty, Fact, Refinement, ShapeFact, StrPreds, Val};
use steins_syntax::ArgValue;

use crate::cx::Cx;
use crate::env::{Known, Store};
use crate::fact_is_int;
use crate::fold::Folder;
use crate::shape_projection::shape_fact;
use crate::transfers::transfer_arg_fact;

/// `explode($separator, $string)` → **`non-empty-list<string>`**.
///
/// PHP 8 removed `explode`'s `false` arm (the empty separator became a
/// `ValueError`), and the split of *any* string on a non-empty separator has at
/// least one piece — `explode(',', '')` is `['']`, not `[]`. Witnesses at
/// `PINNED_PHP` (8.5.8): `explode(',', '')` → `array(1){ [0]=> "" }`;
/// `explode('', 'abc')` → `ValueError: explode(): Argument #1 ($separator) must
/// not be empty`.
///
/// **Two declines, both load-bearing.** An empty (or not-known-non-empty)
/// separator declines — the `ValueError` form has no return value to describe.
/// The three-argument form declines **because `$limit` breaks non-emptiness
/// outright**: `explode(',', 'a,b,c', -5)` returns `array(0){}` at 8.5.8.
pub(super) fn explode_transfer(
    cx: &Cx,
    folder: &mut dyn Folder,
    args: &[ArgValue],
    env: &HashMap<String, Known>,
    store: Option<&Store>,
) -> Option<Fact> {
    // Exactly two arguments — see the `$limit` witness above.
    let [sep, _string] = args else { return None };
    let sep = transfer_arg_fact(cx, folder, sep, env, store)?;
    // The `$string` argument is deliberately unread: it is declared `string`, so
    // anything reaching the body is one (or was coerced to one), and every string
    // splits to at least one string piece.
    fact_is_non_empty_string(&sep)
        .then(|| list_transfer_fact(true, Some(Fact::General { base: Base::String, nullable: false })))
}

/// `range($start, $end [, $step])` → **`non-empty-list<int>`** for integral
/// bounds and step, **`non-empty-list<mixed>`** otherwise.
///
/// The unconditional half is the stronger claim: PHP's `range` always returns a
/// *packed* array (a list) with at least one entry, since equal bounds still
/// produce one (`range(1, 1)` → `[1]`, witnessed at 8.5.8). No argument shape
/// changes that: `range(3, 1)` is the three-element descending list, and
/// `range('a', 'c')` is `['a', 'b', 'c']`. Every input PHP 8.3+ refuses (`$step`
/// of `0`, too large, or negative on an increasing range — all `ValueError` at
/// 8.5.8) produces no value at all, so non-emptiness survives vacuously.
///
/// The element bound is **narrower than "any float involved"** on purpose: PHP
/// 8.3's saner-`range` makes `range(1, 3, 1.0)` an *int* array, while
/// `range(1, 2, 0.5)` and `range(1.0, 3.0)` are float arrays. Rather than encode
/// that fractional-part rule, the transfer claims `int` only when every bound
/// and step are known integral and leaves the element unknown otherwise.
///
/// The integrality test is [`fact_is_int`], which also passes a *nullable* int —
/// sound vacuously, since `range(null, 3)` is a `TypeError` at 8.3+.
pub(super) fn range_transfer(
    cx: &Cx,
    folder: &mut dyn Folder,
    args: &[ArgValue],
    env: &HashMap<String, Known>,
    store: Option<&Store>,
) -> Option<Fact> {
    // `range` accepts two or three arguments; any other arity is an
    // `ArgumentCountError`, and the seam refuses to describe a call PHP rejects.
    if !(2..=3).contains(&args.len()) {
        return None;
    }
    let mut integral = true;
    for a in args {
        // No short-circuit: every argument's fact is read, so the decision is a
        // function of the whole call rather than of evaluation order.
        integral &= transfer_arg_fact(cx, folder, a, env, store).as_ref().is_some_and(fact_is_int);
    }
    Some(list_transfer_fact(
        true,
        integral.then_some(Fact::General { base: Base::Int, nullable: false }),
    ))
}

/// A `list<T>` / `non-empty-list<T>` fact, through the canonical constructor —
/// `None` element is the unknown floor (`list<mixed>`).
pub(crate) fn list_transfer_fact(non_empty: bool, elem: Option<Fact>) -> Fact {
    use steins_domain::{KeyClass, Tail};
    shape_fact(ShapeFact::normalize(
        Vec::new(),
        Tail::Unsealed { key: KeyClass::Int, value: elem.map(Box::new) },
        Certainty::Yes,
        non_empty,
        Vec::new(),
    ))
}

/// Is every value this fact admits a non-empty string? A literal (or literal set)
/// answers by inspection; an abstract string answers through the predicate the
/// domain already models. Anything else — including a nullable string — is `false`.
fn fact_is_non_empty_string(f: &Fact) -> bool {
    match f {
        Fact::Singleton(Val::Str(s)) => !s.is_empty(),
        Fact::OneOf(vals) => vals.iter().all(|v| matches!(v, Val::Str(s) if !s.is_empty())),
        Fact::Refined { base: Base::String, refinement: Refinement::Str(p), nullable: false } => {
            p.contains_all(StrPreds::NON_EMPTY)
        }
        _ => false,
    }
}
