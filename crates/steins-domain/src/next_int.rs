//! PHP's next integer key: the key an omitted array key (`[5 => 'a', 'b']`) or an
//! append (`$a[] = v`, `array_push`) takes (ADR-0049 A22, ADR-0062 Amendment K).
//!
//! PHP keeps it per array as `nNextFreeElement`: one past the largest integer key
//! the array has held, and no next key at all past `PHP_INT_MAX`. This module is
//! the rule's one home:
//!
//! - [`NextInt`] walks a key sequence in order. An array literal and a phpdoc
//!   shape's positional items both walk it from [`NextInt::new`], so
//!   `array{-5: T, U}` declares `U` at `-4`, where `[-5 => 'a', 'b']` puts `'b'`.
//! - [`append_index`] reads the next key off a witnessed key sequence, and is
//!   where PHP's version enters: the 8.3 line ([`NEXT_INT_BOUNDARY`]) belongs to
//!   the append, never to a literal.
//!
//! Each caller keeps its own answer to "no next key": an array literal declines,
//! a phpdoc shape is undecided, a shape's spelling prints the key, and an append
//! falls to a write at an unnamed integer key. The next version-dependent array
//! rule takes the minor through an entry point here, not as an `Option<(u16,
//! u16)>` threaded into value constructors.

use crate::Key;

/// The ADR-0049 A22 boundary: the minor from which PHP's next append index
/// counts a negative key on *every* array. Before it, an array that began as the
/// shared empty array floored its next index at `0` (php-src GH-11154); an array
/// literal never did on any supported minor. The one version boundary any value
/// rule keys on today, and the one a declared target range must not straddle for
/// [`append_index`] to name a negative index.
pub const NEXT_INT_BOUNDARY: (u16, u16) = (8, 3);

/// PHP's next integer key while a key sequence is walked in order: the key an
/// omitted key takes at this point, moved by every integer key the walk
/// [observes](Self::observe), omitted or written.
///
/// [`next`](Self::next) is `None` once a `PHP_INT_MAX` key has been seen, and
/// stays `None`. PHP has no next key there and throws "Cannot add element to the
/// array as the next element is already occupied" (`php -r` on 8.5.10 and
/// 8.2.33), so a clamped key would claim an overwrite PHP never performs and a
/// wrapped one a key PHP never picks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NextInt(Cursor);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Cursor {
    /// No integer key seen yet: the next key is `0`, and the first integer key
    /// moves it past itself however negative.
    #[default]
    Unset,
    /// The next key.
    At(i64),
    /// Past a `PHP_INT_MAX` key: there is no next key.
    Exhausted,
}

impl NextInt {
    /// The next key of an array PHP builds from nothing: a literal, or a first
    /// write to an unset or `null` variable. It is `0` until an integer key is
    /// seen, then one past the largest seen, negative or not: `[-5 => 'a', 'b']`
    /// puts `'b'` at `-4`, and `[3 => 'a', -5 => 'b', 'c']` puts `'c'` at `4`.
    ///
    /// Every minor from ADR-0011's 8.1 floor up builds a literal this way
    /// (ADR-0049 A22). The negative-index RFC landed in PHP 8.0 (php-src
    /// `6732028`), and `var_export([-5 => 'a', 'b'])` prints `-5, -4` on 8.0.28,
    /// 8.1.32, 8.2.33, 8.3.33, 8.4.25 and 8.5.10; only 7.4.33 prints `-5, 0`.
    ///
    /// A phpdoc shape's positional item takes the key the same literal would
    /// give it, so the shape grammar and a shape's keyless spelling walk this
    /// cursor too (#832). PHPStan's constant-array builder floors the index at
    /// `0` instead, and reads the `U` of `array{-5: T, U}` at `0`.
    #[must_use]
    pub const fn new() -> Self {
        NextInt(Cursor::Unset)
    }

    /// Move past an integer key at the walk's position, written or omitted. A key
    /// below the next one leaves it where it is: the index tracks the running
    /// largest key, never the last one, and a duplicate key still counts.
    pub fn observe(&mut self, key: i64) {
        let moves = match self.0 {
            Cursor::Unset => true,
            Cursor::At(next) => key >= next,
            Cursor::Exhausted => false,
        };
        if moves {
            self.0 = key.checked_add(1).map_or(Cursor::Exhausted, Cursor::At);
        }
    }

    /// The key an omitted key takes here, or `None` past a `PHP_INT_MAX` key.
    #[must_use]
    pub const fn next(&self) -> Option<i64> {
        match self.0 {
            Cursor::Unset => Some(0),
            Cursor::At(next) => Some(next),
            Cursor::Exhausted => None,
        }
    }
}

/// **The integer key PHP hands the next appended value** (`$a[] = v`,
/// `array_push`), read off the array's witnessed key sequence, or `None` when
/// `minor` does not decide it or there is no next key.
///
/// The caller vouches that PHP's counter is the sequence's `max + 1`, which an
/// order witness means (ADR-0062 Amendment K). The index is then the literal
/// rule ([`NextInt::new`]) over the sequence. Measured with `php -r` on 8.1.32
/// and 8.5.10:
///
/// ```text
/// array_push(['a'=>1], 9)   => ['a'=>1, 0=>9]    no integer key at all: 0
/// array_push([5=>1], 9)     => [5=>1, 6=>9]      max + 1
/// array_push([-3=>1], 9)    => [-3=>1, -2=>9]    max + 1, negatives included
/// array_push([], 9)         => [0=>9]
/// ```
///
/// **A negative landing index is only provable from PHP 8.3** (ADR-0049 A22).
/// Before it, an array that began as PHP's shared empty array (`[]`, `array()`)
/// kept a next index of `0` through a negative write, so `$a = []; $a[-3] = 1;
/// $a[] = 9;` lands `9` on `0` on 8.1.32 and 8.2.33, and on `-2` from 8.3.33
/// (php-src GH-11154, commit `e2f477c`). A literal-built `[-3 => 1]` lands on
/// `-2` on every one of those. Both witness the sequence `[-3]`, so a negative
/// `max + 1` declines when `minor` is below [`NEXT_INT_BOUNDARY`] or `None`
/// (unknown, or a target range that straddles the boundary). From `0` up both
/// arrays agree, and so does every minor.
///
/// Index bookkeeping, not folded arithmetic on an operand (ADR-0028 §3): the
/// keys are the shape's own, and `max + 1` at `i64::MAX` declines rather than
/// wrapping.
#[must_use]
pub fn append_index(keys: &[Key], minor: Option<(u16, u16)>) -> Option<i64> {
    let mut index = NextInt::new();
    for key in keys {
        if let Key::Int(n) = key {
            index.observe(*n);
        }
    }
    let next = index.next()?;
    (next >= 0 || minor.is_some_and(|m| m >= NEXT_INT_BOUNDARY)).then_some(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The keys a literal's entries take, `None` standing for an omitted key; `None`
    /// overall where PHP throws instead of building the array.
    fn literal_keys(entries: &[Option<i64>]) -> Option<Vec<i64>> {
        let mut index = NextInt::new();
        let mut keys = Vec::with_capacity(entries.len());
        for entry in entries {
            let key = match entry {
                Some(k) => *k,
                None => index.next()?,
            };
            index.observe(key);
            keys.push(key);
        }
        Some(keys)
    }

    fn after(mut index: NextInt, keys: &[i64]) -> Option<i64> {
        for k in keys {
            index.observe(*k);
        }
        index.next()
    }

    #[test]
    fn an_array_with_no_integer_key_takes_zero() {
        assert_eq!(NextInt::new().next(), Some(0));
        assert_eq!(NextInt::default(), NextInt::new());
        // A string key never reaches the cursor.
        assert_eq!(append_index(&[], None), Some(0));
        assert_eq!(append_index(&[Key::Str("a".into()), Key::Str("b".into())], None), Some(0));
    }

    #[test]
    fn the_next_key_is_one_past_the_largest_integer_key() {
        // `[5 => 'a', 'b']` → 5, 6.
        assert_eq!(literal_keys(&[Some(5), None]), Some(vec![5, 6]));
        // `[5 => 'a', 5 => 'b', 'c']` → 5, 5, 6: a duplicate key still counts.
        assert_eq!(literal_keys(&[Some(5), Some(5), None]), Some(vec![5, 5, 6]));
        assert_eq!(after(NextInt::new(), &[0, 7, 2]), Some(8));
    }

    // The literal rows of `steins-syntax`'s `array_lowering.rs`, each `php -r`
    // -witnessed on 8.1.32, 8.2.33 and 8.5.10 (ADR-0049 A22).
    #[test]
    fn a_literal_counts_negative_keys() {
        // `[-5 => 'a', 'b']` → -5, -4.
        assert_eq!(literal_keys(&[Some(-5), None]), Some(vec![-5, -4]));
        // `[3 => 'a', -5 => 'b', 'c']` → 3, -5, 4: max, not last.
        assert_eq!(literal_keys(&[Some(3), Some(-5), None]), Some(vec![3, -5, 4]));
        // `[-5 => 'a', -10 => 'b', 'c']` → -5, -10, -4.
        assert_eq!(literal_keys(&[Some(-5), Some(-10), None]), Some(vec![-5, -10, -4]));
        // `[-5 => 'a', -5 => 'b', 'c']` → the duplicate still advances to -4.
        assert_eq!(literal_keys(&[Some(-5), Some(-5), None]), Some(vec![-5, -5, -4]));
        // `[-5 => 'a', 'b', -1 => 'z', 'c']` → -5, -4, -1, 0.
        assert_eq!(literal_keys(&[Some(-5), None, Some(-1), None]), Some(vec![-5, -4, -1, 0]));
    }

    #[test]
    fn a_literal_mixes_signs_by_the_running_max() {
        // `[-5 => a, 3 => b, c]` → -5, 3, 4.
        assert_eq!(literal_keys(&[Some(-5), Some(3), None]), Some(vec![-5, 3, 4]));
        // `['a', -5 => b, c]` → 0, -5, 1: the auto key already pushed the max to 0.
        assert_eq!(literal_keys(&[None, Some(-5), None]), Some(vec![0, -5, 1]));
        // `[-5 => a, b, 10 => c, d, -1 => e, f]` → -5, -4, 10, 11, -1, 12.
        assert_eq!(
            literal_keys(&[Some(-5), None, Some(10), None, Some(-1), None]),
            Some(vec![-5, -4, 10, 11, -1, 12])
        );
        // One past `-1` is `0` and one past `-2` is `-1`: no special case at zero.
        assert_eq!(literal_keys(&[Some(-1), None, None]), Some(vec![-1, 0, 1]));
        assert_eq!(literal_keys(&[Some(-2), None]), Some(vec![-2, -1]));
    }

    #[test]
    fn there_is_no_next_key_past_php_int_max() {
        // `[9223372036854775807 => 1, 2]` throws on 8.5.10 and 8.2.33.
        assert_eq!(literal_keys(&[Some(i64::MAX), None]), None);
        // `PHP_INT_MAX` is still a key an omitted one can take, and a written key
        // never needs a next one. `[PHP_INT_MAX - 1 => 1, 2, 3]` throws on 8.5.10.
        assert_eq!(literal_keys(&[Some(i64::MAX - 1), None]), Some(vec![i64::MAX - 1, i64::MAX]));
        assert_eq!(literal_keys(&[Some(i64::MAX - 1), None, None]), None);
        assert_eq!(literal_keys(&[None, Some(i64::MAX), Some(3)]), Some(vec![0, i64::MAX, 3]));
        // Once gone, no smaller key brings it back.
        assert_eq!(after(NextInt::new(), &[i64::MAX, 3, -3]), None);
    }

    /// Every sequence of up to four keys over the edges, against the closed form:
    /// `max + 1` (`0` with no key), and `None` past `PHP_INT_MAX`.
    #[test]
    fn the_cursor_matches_its_closed_form() {
        const EDGES: [i64; 7] = [i64::MIN, -2, -1, 0, 1, i64::MAX - 1, i64::MAX];
        let mut seqs: Vec<Vec<i64>> = vec![Vec::new()];
        let mut frontier: Vec<Vec<i64>> = vec![Vec::new()];
        for _ in 0..4 {
            frontier = frontier
                .iter()
                .flat_map(|s| EDGES.iter().map(move |k| [s.as_slice(), &[*k]].concat()))
                .collect();
            seqs.extend(frontier.iter().cloned());
        }
        assert_eq!(seqs.len(), 1 + 7 + 49 + 343 + 2401);
        for seq in &seqs {
            let closed = seq.iter().max().map_or(Some(0), |m| m.checked_add(1));
            assert_eq!(after(NextInt::new(), seq), closed, "after {seq:?}");
        }
    }

    // The append cases `steins-infer`'s `array_out_state.rs` pinned before the rule
    // moved here.
    #[test]
    fn the_append_index_is_the_largest_integer_key_plus_one() {
        // Probed on 8.1.32 and 8.5.10. From `0` up every minor agrees, so an
        // unknown one answers too.
        for minor in [None, Some((8, 1)), Some((8, 5))] {
            assert_eq!(append_index(&[], minor), Some(0));
            assert_eq!(append_index(&[Key::Str("a".into())], minor), Some(0), "no integer key");
            assert_eq!(append_index(&[Key::Int(5)], minor), Some(6));
            assert_eq!(append_index(&[Key::Int(0), Key::Int(7), Key::Int(2)], minor), Some(8));
            // `$a = []; $a[-3] = 1; $a[-1] = 2; $a[] = 9;` lands on `0` everywhere.
            assert_eq!(append_index(&[Key::Int(-3), Key::Int(-1)], minor), Some(0));
            assert_eq!(append_index(&[Key::Int(i64::MAX)], minor), None, "no wrap, a decline");
        }
    }

    #[test]
    fn a_negative_append_index_needs_php_8_3() {
        // `$a = [-3 => 1]; $a[] = 9;` lands on `-2` on 8.1.32 through 8.5.10, but
        // `$a = []; $a[-3] = 1; $a[] = 9;` lands on `0` on 8.1.32 and 8.2.33
        // (php-src GH-11154). Both arrays witness the sequence `[-3]`.
        assert_eq!(append_index(&[Key::Int(-3)], Some((8, 3))), Some(-2));
        assert_eq!(append_index(&[Key::Int(-3)], Some((8, 5))), Some(-2));
        assert_eq!(append_index(&[Key::Int(-3)], Some((8, 2))), None);
        assert_eq!(append_index(&[Key::Int(-3)], Some((8, 1))), None);
        assert_eq!(append_index(&[Key::Int(-3)], None), None, "an unknown minor may be 8.2");
        // A string key between the negatives changes nothing: `$a = [-5 => 1, 'k'
        // => 2, -3 => 3]; $a[] = 9;` lands on `-2` on 8.5.10.
        let mixed = [Key::Int(-5), Key::Str("k".into()), Key::Int(-3)];
        assert_eq!(append_index(&mixed, Some((8, 5))), Some(-2));
        assert_eq!(append_index(&mixed, Some((8, 2))), None);
    }
}
