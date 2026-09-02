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

use steins_domain::{
    Certainty, Fact, IntRange, Key, Presence, ShapeFact, Tail, Val, keys_are_a_list,
};

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

/// **The key sequence a rule may treat as the array's real iteration order**,
/// or `None` when this shape's order is a guess.
///
/// Two sources answer, and a *declared* order is neither. `@var array{a: 0,
/// b: 1, c: 2}` states no order at all in this domain (ADR-0062 §7 — trusting a
/// docblock's field order is phpstan/phpstan#14940's false-positive class), so
/// `array-shift.php:36`'s `array{b: 1, c: 2}` is out of reach here on purpose.
///
/// 1. **An observed build** ([`ShapeFact::witnessed_order`], issue #327): the
///    literal was seen being constructed, so the sequence is the real one.
/// 2. **A proven list**: `is_list == Yes` under a sealed tail with every field
///    required and keys exactly `0..n-1` — list-ness *is* the order claim, so
///    no separate witness is needed. This is the form a `count($items) === 3`
///    narrowing leaves on a `list<int>` (`list-count.php`).
fn determined_order(shape: &ShapeFact) -> Option<Vec<Key>> {
    if let Some(order) = shape.witnessed_order() {
        return Some(order.to_vec());
    }
    let keys: Vec<Key> = shape.fields.iter().map(|(k, _, _)| k.clone()).collect();
    let all_required = shape.fields.iter().all(|(_, p, _)| p.is_required());
    (shape.is_list == Certainty::Yes
        && matches!(shape.tail, Tail::Sealed)
        && all_required
        && keys_are_a_list(keys.iter()))
    .then_some(keys)
}

/// **What `array_shift($a)` wrote into `$a`.**
///
/// The ordered leg, when [`determined_order`] answers: drop the first key, keep
/// the rest, and **renumber only the integer keys** — string keys stay exactly
/// where they were. Measured at PHP 8.5.9:
///
/// ```text
/// array_shift(['a'=>0, 'b'=>1, 'c'=>2])  => ['b'=>1, 'c'=>2]
/// array_shift(['a'=>1, 5=>2, 6=>3])      => [0=>2, 1=>3]
/// array_shift([5=>1, 'a'=>2, 9=>3])      => ['a'=>2, 0=>3]
/// array_shift([3=>'a', 1=>'b', 2=>'c'])  => [0=>'b', 1=>'c']
/// array_shift([0=>'a'])                  => []
/// ```
///
/// The renumbering runs over the *sequence*, so the surviving integer keys take
/// `0, 1, …` in iteration order and can never collide with a surviving string
/// key. It is index bookkeeping, not arithmetic on an operand (ADR-0028 §3).
///
/// Otherwise the general leg ([`general_removal`]), which needs no order because
/// it has no declared key to lose.
pub(crate) fn array_shift_written_fact(shape: &ShapeFact) -> Option<Fact> {
    if let Some(order) = determined_order(shape)
        && let Some((_first, rest)) = order.split_first()
    {
        let mut next = 0i64;
        let mut fields = Vec::with_capacity(rest.len());
        let mut new_order = Vec::with_capacity(rest.len());
        for key in rest {
            let (_, presence, slot) = shape.field(key)?;
            let renumbered = match key {
                Key::Int(_) => {
                    let k = Key::Int(next);
                    next += 1;
                    k
                }
                Key::Str(_) => key.clone(),
            };
            fields.push((renumbered.clone(), *presence, slot.clone()));
            new_order.push(renumbered);
        }
        return Some(sealed_with_order(fields, new_order));
    }
    general_removal(shape)
}

/// **What `array_pop($a)` wrote into `$a`.**
///
/// The ordered leg drops the *last* key and renumbers nothing — the surviving
/// keys keep their own identities. Measured at PHP 8.5.9:
///
/// ```text
/// array_pop(['a'=>1, 5=>2, 6=>3])  => ['a'=>1, 5=>2]
/// array_pop([3=>'a', 1=>'b'])      => [3=>'a']
/// array_pop(['a'=>1, 'b'=>2])      => ['a'=>1]
/// ```
///
/// (The *next* append index does move — `$a = ['x','y']; array_pop($a); $a[] =
/// 'z';` measures `[0=>'x', 1=>'z']` — but that is a fact about a later write,
/// not about the value this call left behind.)
pub(crate) fn array_pop_written_fact(shape: &ShapeFact) -> Option<Fact> {
    if let Some(order) = determined_order(shape)
        && let Some((_last, rest)) = order.split_last()
    {
        let fields = rest
            .iter()
            .map(|k| shape.field(k).map(|(k, p, s)| (k.clone(), *p, s.clone())))
            .collect::<Option<Vec<_>>>()?;
        return Some(sealed_with_order(fields, rest.to_vec()));
    }
    general_removal(shape)
}

/// The sealed shape a removal leaves, with its surviving order reattached.
/// `is_list` is read off the surviving sequence rather than left to
/// [`ShapeFact::normalize`]'s order-agnostic verdict: the sequence is known
/// here, and `[0 => 'b', 1 => 'c']` in that order really is a list.
fn sealed_with_order(fields: Vec<(Key, Presence, Option<Box<Fact>>)>, order: Vec<Key>) -> Fact {
    let is_list = Certainty::from_bool(keys_are_a_list(order.iter()));
    shape_fact(
        ShapeFact::normalize(fields, Tail::Sealed, is_list, !order.is_empty(), Vec::new())
            .with_order(order),
    )
}

/// **A removal from an array with no declared key**: `non-empty-array<string>`
/// becomes `array<string>`, `list<T>` stays `list<T>`, `array{}` stays
/// `array{}`.
///
/// Everything structural rides along — the tail, its key class, the `is_list`
/// verdict — because removing one entry changes none of them. Two things move:
/// `non_empty` is cleared (the array may have held exactly one entry), and the
/// count bound shifts down by one, floored at zero, which is also correct for
/// the empty input the call leaves alone.
///
/// Declines the moment a key is declared: which key a removal takes is
/// [`determined_order`]'s question, and if that declined then so must this.
/// Covers are dropped rather than carried — a disjunctive-presence claim can
/// name the very key that left.
fn general_removal(shape: &ShapeFact) -> Option<Fact> {
    if !shape.fields.iter().all(|(_, p, _)| matches!(p, Presence::Absent)) {
        return None;
    }
    let count = shape.count_range();
    let lo = count.lo().saturating_sub(1).max(0);
    let hi = if count.hi() == i64::MAX { i64::MAX } else { (count.hi() - 1).max(0) };
    let bound = IntRange::new(lo, hi)?;
    Some(shape_fact(ShapeFact::normalize_counted(
        shape.fields.clone(),
        shape.tail.clone(),
        shape.is_list,
        false,
        Vec::new(),
        bound,
    )))
}

/// **One array builtin's out-state contract**: what the running engine must
/// still declare for the rule to apply, and which rewrite it performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ArrayOutRule {
    /// The reflected return spellings ADR-0061 §2's pin accepts.
    pub(crate) declared: &'static [&'static str],
    /// `(total, required)` at `PINNED_PHP` — the order
    /// [`crate::fold::Folder::builtin_param_counts`] answers in, and the leg
    /// that carries the pin when the return spelling pins nothing.
    pub(crate) arity: (u32, u32),
    kind: RuleKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuleKind {
    Sort(SortKind),
    /// `reset` / `end`: the internal pointer moves and the array does not.
    PointerMove,
    /// `array_shift`: the first entry leaves and the integer keys renumber.
    Shift,
    /// `array_pop`: the last entry leaves and nothing renumbers.
    Pop,
}

/// The out-state rule for `name` (lowercased), or `None` for a name this slice
/// has not measured.
pub(crate) fn array_out_rule(name: &str) -> Option<ArrayOutRule> {
    if let Some((kind, arity)) = sort_rule(name) {
        // Every sort declares `: true` at `PINNED_PHP`, so there is no falsy
        // return the fact would have to exclude.
        return Some(ArrayOutRule { declared: &["true"], arity, kind: RuleKind::Sort(kind) });
    }
    let kind = match name {
        "reset" | "end" => RuleKind::PointerMove,
        "array_shift" => RuleKind::Shift,
        "array_pop" => RuleKind::Pop,
        _ => return None,
    };
    // All four declare `: mixed` and `(&$array)` — a spelling that excludes
    // nothing, which is why the arity is the whole pin.
    Some(ArrayOutRule { declared: &["mixed"], arity: (1, 1), kind })
}

impl ArrayOutRule {
    /// **What this call left in argument 0**, given whatever the caller had
    /// proven about it — `None` for no array claim at all.
    ///
    /// Never declines. A rule that cannot state a precise result falls back to
    /// [`Self::floor`], which the witness alone establishes: every name here
    /// raises a `TypeError` on a non-array argument (probed at PHP 8.5.9), so
    /// control reaching the next statement is itself the proof that the
    /// argument was an array, and the floor is what an array is left as. That
    /// is strictly more than the `unknown` the by-ref invalidation leaves and
    /// strictly less than any claim the rule could not prove.
    pub(crate) fn written_fact(self, shape: Option<&ShapeFact>) -> Fact {
        let precise = shape.and_then(|s| match self.kind {
            RuleKind::Sort(kind) => sort_written_fact(kind, s),
            // The pointer is not part of the type, so the caller's own claim
            // comes straight back: `$a = [1, 2]; reset($a);` measures
            // `$a === [1, 2]`, and `$a = []; end($a);` measures `$a === []`
            // (returning `false`, which is a value and not a refusal).
            RuleKind::PointerMove => Some(shape_fact(s.clone())),
            RuleKind::Shift => array_shift_written_fact(s),
            RuleKind::Pop => array_pop_written_fact(s),
        });
        precise.unwrap_or_else(|| self.floor())
    }

    /// The out-state this rule can state with **no** premise about the input.
    ///
    /// A renumbering sort leaves a list whatever it was given; everything else
    /// leaves an array. Neither says anything about emptiness — an unproven
    /// input may be `[]`, and every name here leaves `[]` alone.
    fn floor(self) -> Fact {
        match self.kind {
            RuleKind::Sort(SortKind::Renumbering) => list_transfer_fact(false, None),
            _ => shape_fact(ShapeFact::plain_array()),
        }
    }
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
        let rule = array_out_rule("reset").expect("a rule");
        let Fact::Shape { shape: out, .. } = rule.written_fact(Some(&shape)) else {
            panic!("a pointer move states a shape");
        };
        assert_eq!(*out, shape, "`reset`/`end` change nothing the type can see");
    }

    #[test]
    fn a_shift_drops_the_first_key_and_renumbers_only_the_integers() {
        // Probed: `array_shift([5=>1, 'a'=>2, 9=>3]) === ['a'=>2, 0=>3]`.
        let shape = lit(&[
            (Key::Int(5), i(1)),
            (Key::Str("a".into()), i(2)),
            (Key::Int(9), i(3)),
        ]);
        let Fact::Shape { shape: out, .. } = array_shift_written_fact(&shape).expect("a shape")
        else {
            panic!("a shift states a shape");
        };
        let keys: Vec<&Key> = out.fields.iter().map(|(k, _, _)| k).collect();
        assert_eq!(keys, vec![&Key::Int(0), &Key::Str("a".into())]);
        assert_eq!(out.field(&Key::Int(0)).and_then(|(_, _, s)| s.clone()).map(|s| *s),
                   Some(Fact::Singleton(i(3))), "key 9's value took index 0");
        assert_eq!(out.is_list, Certainty::No, "a surviving string key is not a list");
    }

    #[test]
    fn a_shift_of_a_witnessed_list_stays_a_witnessed_list() {
        // Probed: `array_shift([3=>'a', 1=>'b', 2=>'c']) === [0=>'b', 1=>'c']`.
        let shape = lit(&[(Key::Int(0), i(1)), (Key::Int(1), i(2)), (Key::Int(2), i(3))]);
        let Fact::Shape { shape: out, .. } = array_shift_written_fact(&shape).expect("a shape")
        else {
            panic!("a shift states a shape");
        };
        assert_eq!(out.is_list, Certainty::Yes);
        assert_eq!(out.fields.len(), 2);
        assert_eq!(out.witnessed_order(), Some(&[Key::Int(0), Key::Int(1)][..]));
    }

    #[test]
    fn a_pop_drops_the_last_key_and_renumbers_nothing() {
        // Probed: `array_pop(['a'=>1, 5=>2, 6=>3]) === ['a'=>1, 5=>2]`.
        let shape = lit(&[
            (Key::Str("a".into()), i(1)),
            (Key::Int(5), i(2)),
            (Key::Int(6), i(3)),
        ]);
        let Fact::Shape { shape: out, .. } = array_pop_written_fact(&shape).expect("a shape")
        else {
            panic!("a pop states a shape");
        };
        let keys: Vec<&Key> = out.fields.iter().map(|(k, _, _)| k).collect();
        assert_eq!(keys, vec![&Key::Int(5), &Key::Str("a".into())], "sorted, but key 5 kept");
    }

    #[test]
    fn a_removal_from_a_keyless_array_only_loses_non_emptiness() {
        // `non-empty-array<string>` -> `array<string>`: the tail, its key class
        // and the `is_list` verdict all ride along.
        let non_empty = ShapeFact::normalize(
            Vec::new(),
            Tail::Unsealed { key: KeyClass::ArrayKey, value: Some(Box::new(Fact::General {
                base: steins_domain::Base::String,
                nullable: false,
            })) },
            Certainty::Maybe,
            true,
            Vec::new(),
        );
        for rule in ["array_shift", "array_pop"] {
            let out = array_out_rule(rule).expect("a rule").written_fact(Some(&non_empty));
            let Fact::Shape { shape, .. } = out else { panic!("{rule} states a shape") };
            assert!(!shape.non_empty, "{rule} may have taken the only entry");
            assert_eq!(shape.tail, non_empty.tail, "{rule} does not touch the tail");
        }
    }

    #[test]
    fn a_declared_field_order_is_not_an_order() {
        // `@var array{a: 0, b: 1, c: 2}` states no iteration order in this
        // domain (ADR-0062 §7), so a removal cannot say which key left and the
        // rule falls to its floor rather than guessing the docblock's order.
        let declared = ShapeFact::normalize(
            vec![
                (Key::Str("a".into()), Presence::Required { witnessed: false }, Some(Box::new(Fact::Singleton(i(0))))),
                (Key::Str("b".into()), Presence::Required { witnessed: false }, Some(Box::new(Fact::Singleton(i(1))))),
            ],
            Tail::Sealed,
            Certainty::Maybe,
            true,
            Vec::new(),
        );
        assert_eq!(determined_order(&declared), None);
        assert!(array_shift_written_fact(&declared).is_none());
        assert!(array_pop_written_fact(&declared).is_none());
        let floored = array_out_rule("array_shift").expect("a rule").written_fact(Some(&declared));
        assert_eq!(floored, shape_fact(ShapeFact::plain_array()));
    }

    #[test]
    fn every_rule_answers_something_with_no_claim_at_all() {
        for (name, want_list) in [
            ("sort", true),
            ("shuffle", true),
            ("asort", false),
            ("reset", false),
            ("array_shift", false),
            ("array_pop", false),
        ] {
            let Fact::Shape { shape, .. } = array_out_rule(name).expect("a rule").written_fact(None)
            else {
                panic!("{name} states a shape");
            };
            assert_eq!(shape.is_list == Certainty::Yes, want_list, "{name}");
            assert!(!shape.non_empty, "{name} says nothing about emptiness");
        }
        assert_eq!(array_out_rule("array_walk"), None, "array_walk stays unmeasured");
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
