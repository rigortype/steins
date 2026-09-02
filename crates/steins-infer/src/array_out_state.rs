//! **What an array builtin left in its by-ref argument 0** (issue #635): the
//! *fact* half of ADR-0077 §3 for the names whose written-when witness
//! [`steins_catalog::out_param_written_when`] now states.
//!
//! The witness says the write happened; nothing here re-argues that. Each rule
//! below answers only the second question — given what the caller's variable
//! held *before* the call, what does it hold after — and **declines** (`None`)
//! on any premise it cannot prove, which leaves ADR-0063 §2.3's by-ref
//! invalidation standing as the FP-safe floor.
//!
//! Every row is transcribed from a `php -r` at `PINNED_PHP` (8.5.9), quoted in
//! the rule's own doc (ADR-0061 §4). Two of those probes contradict a plain
//! reading of the php-src stub, and the rules are shaped by the probe:
//!
//! * `array_unshift` and `array_splice` **preserve string keys** — only the
//!   *integer* keys renumber. `array_unshift(['a' => 1, 7 => 2], 0)` measures
//!   `[0 => 0, 'a' => 1, 1 => 2]`, which is not a list. So list-ness is a
//!   claim about the input, never an unconditional rewrite.
//! * a comparator that writes to the array under `usort`/`uasort` has its
//!   writes **discarded**, so the result rests on the input alone and a
//!   callback-invoking sort needs no callback analysis to state its out-state.

use steins_domain::{Certainty, Fact, Presence, ShapeFact, Tail, Val};

use crate::shape_projection::{shape_fact, shape_value_union};
use crate::transfers::list_transfer_fact;

/// How a sort rearranges its argument — the only axis the twelve names differ
/// on, since none of them adds, removes or rewrites a *value*.
///
/// Measured (PHP 8.5.9), one call per row:
///
/// ```text
/// sort(['b'=>2,'a'=>1,0=>9])   => [0=>1, 1=>2, 2=>9]      keys discarded
/// rsort([4,1])                 => [0=>4, 1=>1]
/// usort([2,1], $cmp)           => [0=>1, 1=>2]
/// shuffle(['b'=>2,'a'=>1])     => [0=>1, 1=>2]            keys discarded
/// asort([2,1])                 => [1=>1, 0=>2]            keys ride along
/// arsort([1,2])                => [1=>2, 0=>1]
/// uasort(['b'=>2,'a'=>1],$cmp) => ['a'=>1, 'b'=>2]
/// natsort(['b','a'])           => [1=>'a', 0=>'b']
/// natcasesort(['B','a'])       => [1=>'a', 0=>'B']
/// ksort([1=>'b',0=>'a'])       => [0=>'a', 1=>'b']        keys ride along
/// krsort([0=>'a',1=>'b'])      => [1=>'b', 0=>'a']
/// uksort(['b'=>2,'a'=>1],$cmp) => ['a'=>1, 'b'=>2]
/// ```
///
/// The `asort` row is the one that decides the whole
/// [`SortKind::KeyPreserving`] rule: `asort([2, 1])` measures `[1 => 1, 0 => 2]`,
/// so a **list input is not a list afterwards** — the rebuilt shape must state
/// [`Certainty::Maybe`] and let [`ShapeFact::normalize`] recompute, which it
/// does correctly because an order-agnostic `array{0: T, 1: U}` is already
/// `Maybe` in this domain (ADR-0062 §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SortKind {
    /// The keys are discarded and the values renumbered from `0`: the result is
    /// a list of the input's value union.
    Renumbering,
    /// Each value keeps its key; only the iteration order moves.
    KeyPreserving,
}

/// Which rearrangement `name` performs and the **arity it was measured at**, or
/// `None` for a name that is not one of the twelve. Lowercased by the caller.
///
/// The arity rides along because the declaration pin needs it (ADR-0064
/// Amendment B): `: true` says the call has no falsy return, and nothing at all
/// about *which* parameter is the array. A signature that has moved is a stale
/// rule, and the pin is what notices.
pub(crate) fn sort_rule(name: &str) -> Option<(SortKind, (u32, u32))> {
    let kind = match name {
        "sort" | "rsort" | "usort" | "shuffle" => SortKind::Renumbering,
        "asort" | "arsort" | "uasort" | "uksort" | "ksort" | "krsort" | "natsort"
        | "natcasesort" => SortKind::KeyPreserving,
        _ => return None,
    };
    // `(total, required)` — the order [`Folder::builtin_param_counts`] answers
    // in, read off `ReflectionFunction` at `PINNED_PHP`.
    let arity = match name {
        "sort" | "rsort" | "asort" | "arsort" | "ksort" | "krsort" => (2, 1),
        "natsort" | "natcasesort" | "shuffle" => (1, 1),
        _ => (2, 2),
    };
    Some((kind, arity))
}

/// **The array a by-ref array rule reads out of a pre-call claim**, or `None`
/// when the claim does not spell one.
///
/// Two forms answer. A [`Fact::Shape`] is the shape itself; a fully literal
/// array binds [`Fact::Singleton`] of [`Val::Array`] instead (never
/// `Fact::Shape`), so it lifts — the form `sort.php:23`'s
/// `$arr = [4, 'one' => 1, …]` actually arrives in.
///
/// **`nullable` is dropped rather than refused**, and the witness is what makes
/// that sound: every name here raises a `TypeError` on a non-array argument
/// (`asort($null)`, `array_shift($null)`, `reset($string)` all probed at PHP
/// 8.5.9), so on the path where the *next statement runs* the argument was an
/// array. A claim of `array{…}|null` therefore contributes its array arm and
/// nothing else.
pub(crate) fn byref_array_shape(fact: &Fact) -> Option<ShapeFact> {
    match fact {
        Fact::Shape { shape, .. } => Some((**shape).clone()),
        Fact::Singleton(Val::Array(entries)) => Some(ShapeFact::lift(entries)),
        _ => None,
    }
}

/// Is this shape **proven empty** — sealed, with no key it admits? Then every
/// rule here that would otherwise widen to `list<T>` answers `array{}` exactly,
/// which is what `shuffle.php:59` and `array_splice.php:74` assert.
fn proven_empty(shape: &ShapeFact) -> bool {
    matches!(shape.tail, Tail::Sealed)
        && shape.fields.iter().all(|(_, p, _)| matches!(p, Presence::Absent))
}

/// **What a sort wrote into argument 0.**
///
/// [`SortKind::Renumbering`]: a list of the input's value union
/// ([`shape_value_union`]), `non_empty` carried — the entry count is the one
/// thing every sort preserves. An unknown value union is the honest floor
/// (`list<mixed>`), still strictly more than the invalidation it replaces.
///
/// [`SortKind::KeyPreserving`]: the same fields, slots, presence, covers, tail
/// and count bound, rebuilt through [`ShapeFact::normalize_counted`] — whose
/// one job here is to **drop the order witness** (`order: None` is
/// unconditional there, issue #327's drop discipline). Nothing else about the
/// value moves, because nothing else about the array did.
pub(crate) fn sort_written_fact(kind: SortKind, shape: &ShapeFact) -> Option<Fact> {
    if proven_empty(shape) {
        return Some(shape_fact(shape.clone()));
    }
    Some(match kind {
        SortKind::Renumbering => list_transfer_fact(shape.non_empty, shape_value_union(shape)),
        SortKind::KeyPreserving => shape_fact(ShapeFact::normalize_counted(
            shape.fields.clone(),
            shape.tail.clone(),
            Certainty::Maybe,
            shape.non_empty,
            shape.covers.clone(),
            shape.count_bound,
        )),
    })
}

/// **What a sort wrote when the input was never proven** — the floor the
/// witness alone establishes, with no premise about the caller's variable at
/// all.
///
/// The reasoning is the witness's, taken one step further than
/// [`byref_array_shape`] takes it. A non-array argument raises a `TypeError`
/// (probed for every name), so *control reaching the next statement is itself
/// the proof that the argument was an array* — and a sort leaves an array a
/// list ([`SortKind::Renumbering`]) or an array ([`SortKind::KeyPreserving`]).
/// Both are honest floors, and both are strictly more than the `unknown` the
/// by-ref invalidation leaves.
///
/// It says nothing about emptiness: an unproven input may be `[]`, and every
/// sort leaves `[]` alone.
pub(crate) fn unproven_input_sort_fact(kind: SortKind) -> Fact {
    match kind {
        SortKind::Renumbering => list_transfer_fact(false, None),
        SortKind::KeyPreserving => shape_fact(ShapeFact::plain_array()),
    }
}

/// [`unproven_input_sort_fact`]'s pointer-move twin: `reset`/`end` change
/// nothing, so with no claim to hand back the floor is bare `array` — again
/// the `TypeError` on a non-array argument doing the work.
pub(crate) fn unproven_input_pointer_move_fact() -> Fact {
    shape_fact(ShapeFact::plain_array())
}

/// **What `reset($a)` / `end($a)` wrote into argument 0: nothing.**
///
/// The internal array pointer is not part of the type, and the array itself is
/// untouched — measured at PHP 8.5.9, `$a = [1, 2]; reset($a);` leaves
/// `[0 => 1, 1 => 2]` and `$a = []; end($a);` leaves `[]` (returning `false`,
/// which is a *value*, not a refusal). So the seed hands back the caller's own
/// claim and the only thing it undoes is the invalidation.
///
/// It still goes through [`byref_array_shape`], for the reason that reader
/// exists: a claim this rule cannot read as an array is one it must decline.
pub(crate) fn pointer_move_written_fact(shape: &ShapeFact) -> Option<Fact> {
    Some(shape_fact(shape.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use steins_domain::{Key, KeyClass};

    fn lit(entries: &[(Key, Val)]) -> ShapeFact {
        ShapeFact::lift(entries)
    }

    fn i(n: i64) -> Val {
        Val::Int(n)
    }

    #[test]
    fn the_sort_family_splits_into_exactly_two_rewrites() {
        for f in ["sort", "rsort", "usort", "shuffle"] {
            assert_eq!(sort_rule(f).map(|(k, _)| k), Some(SortKind::Renumbering), "{f}");
        }
        for f in ["asort", "arsort", "uasort", "uksort", "ksort", "krsort", "natsort", "natcasesort"]
        {
            assert_eq!(sort_rule(f).map(|(k, _)| k), Some(SortKind::KeyPreserving), "{f}");
        }
        assert_eq!(sort_rule("array_shift"), None);
        assert_eq!(sort_rule("reset"), None);
    }

    #[test]
    fn every_sort_pins_the_arity_it_was_measured_at() {
        // `php -r` reflection at PHP 8.5.9, in `builtin_param_counts` order:
        // `(total, required)`. Getting this pair the wrong way round is a silent
        // decline, not a compile error — the pin simply never matches.
        for (f, want) in [
            ("sort", (2, 1)),
            ("rsort", (2, 1)),
            ("asort", (2, 1)),
            ("arsort", (2, 1)),
            ("ksort", (2, 1)),
            ("krsort", (2, 1)),
            ("natsort", (1, 1)),
            ("natcasesort", (1, 1)),
            ("shuffle", (1, 1)),
            ("usort", (2, 2)),
            ("uasort", (2, 2)),
            ("uksort", (2, 2)),
        ] {
            assert_eq!(sort_rule(f).map(|(_, a)| a), Some(want), "{f}");
        }
    }

    #[test]
    fn a_renumbering_sort_answers_a_non_empty_list_of_the_value_union() {
        // `sort.php:23`'s subject, in the form it actually binds: a fully
        // literal array is a `Singleton(Val::Array)`, not a `Fact::Shape`.
        let entries = vec![
            (Key::Int(0), i(4)),
            (Key::Str("one".into()), i(1)),
            (Key::Str("five".into()), i(5)),
            (Key::Str("three".into()), i(3)),
        ];
        let shape = byref_array_shape(&Fact::Singleton(Val::Array(entries))).expect("an array");
        let Some(Fact::Shape { shape: out, .. }) =
            sort_written_fact(SortKind::Renumbering, &shape)
        else {
            panic!("a renumbering sort states a shape");
        };
        assert_eq!(out.is_list, Certainty::Yes);
        assert!(out.non_empty);
        assert!(out.fields.is_empty(), "the keys are discarded");
        let Tail::Unsealed { key: KeyClass::Int, value: Some(v) } = &out.tail else {
            panic!("a list tail keyed by int");
        };
        assert_eq!(**v, Fact::OneOf(vec![i(1), i(3), i(4), i(5)]));
    }

    #[test]
    fn a_key_preserving_sort_keeps_the_keys_and_loses_the_order_witness() {
        // `bug-10627.php`'s subject: `asort` on `['A', 'C', 'B']` keeps every
        // key and slot, so PHPStan spells it `array{'A', 'C', 'B'}` — an
        // `array{…}`, not a `list`, because the order is gone.
        let entries = vec![
            (Key::Int(0), Val::Str("A".into())),
            (Key::Int(1), Val::Str("C".into())),
            (Key::Int(2), Val::Str("B".into())),
        ];
        let shape = lit(&entries);
        assert_eq!(shape.is_list, Certainty::Yes, "the literal was witnessed in order");
        assert!(shape.witnessed_order().is_some());
        let Some(Fact::Shape { shape: out, .. }) =
            sort_written_fact(SortKind::KeyPreserving, &shape)
        else {
            panic!("a key-preserving sort states a shape");
        };
        assert_eq!(out.fields.len(), 3, "every key rides along");
        assert_eq!(out.tail, Tail::Sealed);
        assert!(out.non_empty);
        assert_eq!(out.order, None, "the order witness is exactly what a sort destroys");
        // `asort([2, 1]) === [1 => 1, 0 => 2]` (probed): a list input is not a
        // list afterwards.
        assert_eq!(out.is_list, Certainty::Maybe);
    }

    #[test]
    fn an_empty_array_survives_every_sort_exactly() {
        let shape = lit(&[]);
        for kind in [SortKind::Renumbering, SortKind::KeyPreserving] {
            let Some(Fact::Shape { shape: out, .. }) = sort_written_fact(kind, &shape) else {
                panic!("a sort states a shape");
            };
            assert_eq!(out.tail, Tail::Sealed, "{kind:?} keeps `array{{}}` exact");
            assert!(!out.non_empty);
        }
    }

    #[test]
    fn a_pointer_move_hands_back_the_callers_own_claim() {
        let shape = lit(&[(Key::Int(0), i(1)), (Key::Int(1), i(2))]);
        let Some(Fact::Shape { shape: out, .. }) = pointer_move_written_fact(&shape) else {
            panic!("a pointer move states a shape");
        };
        assert_eq!(*out, shape, "`reset`/`end` change nothing the type can see");
    }

    #[test]
    fn a_claim_that_is_not_an_array_declines() {
        let string = Fact::General { base: steins_domain::Base::String, nullable: false };
        assert!(byref_array_shape(&Fact::Singleton(i(1))).is_none());
        assert!(byref_array_shape(&string).is_none());
        // A nullable array contributes its array arm: a `null` argument is a
        // `TypeError`, which never reaches the statement the seed lands at.
        let shape = lit(&[(Key::Int(0), i(1))]);
        let nullable = Fact::Shape { shape: Box::new(shape.clone()), nullable: true };
        assert_eq!(byref_array_shape(&nullable), Some(shape));
    }
}
