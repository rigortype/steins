//! Issue #328 wave 2 — the rest of the array family that reads keys and not
//! values, executed on the order-witnessed lane.
//!
//! Wave 1 took the four restructuring projections. This takes the names that
//! qualify under the same test — *restructures the argument, reads no value
//! semantics beyond key normalization* — and were still answering the key-set
//! widening: position readers (`array_key_first`/`array_key_last`/`array_first`/
//! `array_last`), `array_slice` over a witnessed shape (previously needed every
//! value proven though it reads none), `array_fill_keys`/`array_combine` (keys
//! come from values through a measured cast), and `array_diff_key`/
//! `array_intersect_key` (pure key-set work).
//!
//! Two disciplines pinned throughout, as in wave 1:
//! * **The declared lane does not move.** A shape with no order witness is a
//!   key set, and every position reader over one keeps its `'a'|'b'` answer —
//!   phpstan/phpstan#14940's FP class, declined by ADR-0062 §7.
//! * **The pointer family stays out.** `key`/`current`/`reset`/`end` read the
//!   unmodeled internal array pointer; the existing arm tolerates that only
//!   because a shape-derived fact never premises a proof-layer finding, while a
//!   witnessed literal is `Verified` and would carry the assumption into a proof.
//!
//! Every expectation was probed at PHP 8.5.9 and cross-checked against PHPStan
//! 2.2.2; rows *sharper* than that oracle are noted where it happens.
//!
//! NB: a variable handed to a call is invalidated after that statement
//! (pre-existing by-ref conservatism), so each fixture uses its parameters once.

use std::collections::HashMap;

use steins_domain::Fact;
use steins_infer::{DEBUG_TYPE_ID, Diagnostic, Folder, check_with};
use steins_syntax::{ArgValue, SourceTree};

/// A mock sidecar: the reflected return type per name, plus the read-position
/// family's arity second leg.
#[derive(Default)]
struct Mock {
    types: HashMap<String, String>,
}

impl Mock {
    fn sidecar() -> Mock {
        let mut types = HashMap::new();
        for f in [
            "array_values",
            "array_keys",
            "array_flip",
            "array_reverse",
            "array_slice",
            "array_fill_keys",
            "array_combine",
            "array_diff_key",
            "array_intersect_key",
        ] {
            types.insert(f.to_owned(), "array".to_owned());
        }
        for f in ["array_key_first", "array_key_last"] {
            types.insert(f.to_owned(), "string|int|null".to_owned());
        }
        for f in ["array_first", "array_last"] {
            types.insert(f.to_owned(), "mixed".to_owned());
        }
        // `key` declared so its test exercises the *widening*, not the floor.
        types.insert("key".to_owned(), "string|int|null".to_owned());
        Mock { types }
    }
}

impl Folder for Mock {
    fn fold(&mut self, _name: &str, _args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        true
    }
    fn builtin_return_fact(&mut self, _name: &str) -> Option<Fact> {
        None
    }
    fn builtin_return_type(&mut self, name: &str) -> Option<String> {
        self.types.get(&name.to_ascii_lowercase()).cloned()
    }
    /// Measured at `PINNED_PHP`: read-position takes 1 required param;
    /// `array_slice` takes 4 with 2 required.
    fn builtin_param_counts(&mut self, name: &str) -> Option<(u32, u32)> {
        let n = name.to_ascii_lowercase();
        if n == "array_slice" {
            return Some((4, 2));
        }
        matches!(n.as_str(), "array_first" | "array_last").then_some((1, 1))
    }
}

fn one_type(src: &str) -> String {
    let tree = SourceTree::parse(src);
    let ds: Vec<Diagnostic> = check_with(&tree, &[], "t.php", &mut Mock::sidecar())
        .into_iter()
        .filter(|d| !d.id.starts_with("untyped."))
        .collect();
    let other: Vec<&Diagnostic> = ds.iter().filter(|d| !d.id.starts_with("debug.")).collect();
    assert!(other.is_empty(), "a projection emitted a finding: {other:?}");
    let ty: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ty.len(), 1, "expected exactly one debug.type dump, got {ds:?}");
    ty[0].message.clone()
}

fn dump(expr: &str) -> String {
    one_type(&format!(
        "<?php\nfunction f(int $x, string $s): void {{ \\PHPStan\\dumpType({expr}); }}\n"
    ))
}

fn declared(decl: &str, expr: &str) -> String {
    one_type(&format!(
        "<?php\n/** @param {decl} $v */\nfunction f(array $v): void {{ \\PHPStan\\dumpType({expr}); }}\n"
    ))
}

// Position readers

/// Exact once the position is witnessed. **Sharper than PHPStan 2.2.2**, which
/// answers `'a'|'b'` and `int` here — it holds a constant array but does not
/// consume its order for these names.
#[test]
fn the_position_readers_are_exact_on_a_witnessed_order() {
    assert_eq!(dump("array_key_first(['b' => 1, 'a' => $x])"), "dumped type: 'b'");
    assert_eq!(dump("array_key_last(['b' => 1, 'a' => $x])"), "dumped type: 'a'");
    assert_eq!(dump("array_first(['b' => 1, 'a' => $x])"), "dumped type: 1");
    assert_eq!(dump("array_last(['b' => 1, 'a' => $x])"), "dumped type: int");
}

/// An empty witnessed sequence is exactly the proof the array is empty.
#[test]
fn the_empty_array_answers_null() {
    for f in ["array_key_first", "array_key_last", "array_first", "array_last"] {
        assert_eq!(dump(&format!("{f}([])")), "dumped type: null", "{f} on []");
    }
}

/// `array_first` declines rather than claiming `mixed` when nothing proved the
/// slot (this mock has no catalog floor, so the decline surfaces as `unknown`;
/// a real run shows ADR-0069's floor instead — either way the *rule* said nothing).
#[test]
fn a_value_reader_declines_on_an_unknown_slot() {
    assert_eq!(
        dump("array_first([strlen($s) > 2 ? [] : new \\stdClass(), 'z'])"),
        "dumped type: unknown"
    );
    // The key reader is unaffected: keys are known however little the values are.
    assert_eq!(
        dump("array_key_first([strlen($s) > 2 ? [] : new \\stdClass(), 'z'])"),
        "dumped type: 0"
    );
}

/// **The negative pin.** ADR-0062 §2 at its sharpest: PHPStan answers `'a'` for
/// `array_key_first(array{a: int, b: int})` and is wrong on `['b' => 2, 'a' => 1]`,
/// which the declaration admits just as well.
#[test]
fn a_declared_shape_still_answers_some_key_of_the_set() {
    assert_eq!(
        declared("array{a: int, b: int}", "array_key_first($v)"),
        "dumped type: 'a'|'b' (asserted)"
    );
    assert_eq!(
        declared("array{a: int, b: int}", "array_key_last($v)"),
        "dumped type: 'a'|'b' (asserted)"
    );
    // A possibly-empty shape keeps its `null` arm.
    assert_eq!(
        declared("array{a?: int, b?: int}", "array_key_first($v)"),
        "dumped type: 'a'|'b'|null (asserted)"
    );
}

/// `key`/`current`/`reset`/`end` stay deliberately outside the witnessed rung
/// (see module header: unmodeled pointer, unsound at `Verified`).
#[test]
fn the_pointer_family_keeps_its_widening() {
    assert_eq!(dump("key(['b' => 1, 'a' => 2])"), "dumped type: 'a'|'b'");
    assert_eq!(
        declared("array{a: int, b: int}", "key($v)"),
        "dumped type: 'a'|'b' (asserted)"
    );
}

// array_slice over a witnessed shape

/// `array_slice` reads offsets and keys and never a value, so an unproven slot
/// can travel through it unread.
#[test]
fn the_exact_slice_no_longer_needs_every_value_proven() {
    assert_eq!(dump("array_slice(['x', $s, 'z'], 1)"), "dumped type: list{string, 'z'}");
    // Key rule: string keys survive, integer keys renumber `0..` in the surviving order.
    assert_eq!(
        dump("array_slice(['a' => 1, 5 => 2, 'b' => $x, 9 => 4], 1)"),
        "dumped type: array{0: 2, b: int, 1: 4}"
    );
}

/// The fully-proven path is untouched, and the two rules agree wherever both apply.
#[test]
fn the_value_only_slice_still_answers_what_it_did() {
    assert_eq!(dump("array_slice(['a', 'b', 'c'], 1)"), "dumped type: list{'b', 'c'}");
    assert_eq!(dump("array_slice(['a', 'b', 'c'], -1)"), "dumped type: list{'c'}");
    assert_eq!(dump("array_slice(['a', 'b', 'c'], 1, 1)"), "dumped type: list{'b'}");
}

/// The offset is not a proven int, so there is no window to take — the
/// shape-level widening answers instead of a guess.
#[test]
fn an_unproven_window_falls_to_the_widening() {
    assert_eq!(dump("array_slice(['x', $s, 'z'], $x)"), "dumped type: list<string>");
}

// array_fill_keys / array_combine

/// Numeric strings normalize to the integer key (`'01'` is not numeric-canonical
/// and stays a string key); duplicates collapse; the filled value travels at
/// whatever layer it was proven.
#[test]
fn values_become_keys_through_the_measured_cast() {
    assert_eq!(dump("array_fill_keys(['a', 'b'], 1)"), "dumped type: array{a: 1, b: 1}");
    assert_eq!(dump("array_fill_keys(['1', 2], 'v')"), "dumped type: array{1: 'v', 2: 'v'}");
    assert_eq!(dump("array_fill_keys(['01'], 'v')"), "dumped type: array{'01': 'v'}");
    assert_eq!(dump("array_fill_keys(['a', 'a'], 1)"), "dumped type: array{a: 1}");
    assert_eq!(dump("array_fill_keys(['a'], $x)"), "dumped type: array{a: int}");
    assert_eq!(dump("array_fill_keys([true, null], 'v')"), "dumped type: array{1: 'v', '': 'v'}");
}

/// The KEY declines (not the value): PHP renders a float to string under the
/// `precision` ini directive, so `array_fill_keys([1.5], 'v')`'s key depends on
/// runtime configuration (same reason `concat_cast` excludes floats — unlike
/// `$a[1.5]`'s int cast or `array_flip`'s skip). Issue #336 piece 2: the shape
/// lane still answers non-empty and filled with `'v'`; only the key goes unnamed.
#[test]
fn a_float_key_declines_because_its_spelling_is_a_setting() {
    assert_eq!(dump("array_fill_keys([1.5], 'v')"), "dumped type: non-empty-array<'v'>");
}

#[test]
fn combine_zips_positionally_and_resolves_duplicates_last_wins() {
    assert_eq!(dump("array_combine(['a', 'b'], [1, $x])"), "dumped type: array{a: 1, b: int}");
    assert_eq!(dump("array_combine(['1', 'b'], [1, 2])"), "dumped type: array{1: 1, b: 2}");
    assert_eq!(dump("array_combine(['a', 'a'], [1, 2])"), "dumped type: array{a: 2}");
}

/// A length mismatch raises `ValueError` — no return value to state, so the
/// rule declines rather than inventing one.
#[test]
fn a_length_mismatch_is_a_call_that_does_not_return() {
    assert_eq!(
        dump("array_combine(['a'], [1, 2])"),
        "dumped type: associative-array<mixed> (asserted)"
    );
}

// array_diff_key / array_intersect_key

/// Order comes from the FIRST array, not the second and not the canonical one.
#[test]
fn the_key_set_operations_keep_the_first_arrays_order() {
    assert_eq!(
        dump("array_intersect_key(['b' => 2, 'a' => 1], ['a' => 9, 'b' => 8])"),
        "dumped type: array{b: 2, a: 1}"
    );
    assert_eq!(
        dump("array_diff_key(['a' => 1, 'b' => $x], ['a' => 9])"),
        "dumped type: array{b: int}"
    );
}

/// `'5'` and `5` are one key, so the `5` entry is removed. **Sharper than
/// PHPStan 2.2.2**, which answers `array<5|'5x', 1|2>` here.
#[test]
fn key_identity_is_the_normalized_key() {
    assert_eq!(dump("array_diff_key([5 => 1, '5x' => 2], ['5' => 9])"), "dumped type: array{'5x': 2}");
}

#[test]
fn values_are_never_read_so_unknown_slots_cost_nothing() {
    assert_eq!(
        dump("array_intersect_key(['a' => $x, 'b' => $s], ['a' => 9])"),
        "dumped type: array{a: int}"
    );
}

/// A set has no order, so reading a *declaration's* key set is not the §7
/// declined import — only its key ORDER is declined. Result order still comes
/// from the first (witnessed) array.
#[test]
fn a_declared_second_argument_contributes_its_key_set() {
    let src = "<?php\n/** @param array{a: int, b: int} $v */\n\
               function f(array $v): void { \\PHPStan\\dumpType(array_intersect_key(['a' => 1, 'b' => 2], $v)); }\n";
    assert_eq!(one_type(src), "dumped type: array{a: 1, b: 2} (asserted)");
}

/// Neither an OPTIONAL key nor an unsealed tail decides the set.
#[test]
fn an_uncertain_key_set_declines() {
    let optional = "<?php\n/** @param array{a: int, b?: int} $v */\n\
                    function f(array $v): void { \\PHPStan\\dumpType(array_intersect_key(['a' => 1, 'b' => 2], $v)); }\n";
    assert_eq!(one_type(optional), "dumped type: array (asserted)");
    let unsealed = "<?php\n/** @param array<string, int> $v */\n\
                    function f(array $v): void { \\PHPStan\\dumpType(array_diff_key(['a' => 1], $v)); }\n";
    assert_eq!(one_type(unsealed), "dumped type: array (asserted)");
}

/// Unlike the key-set pair, `array_combine` zips POSITIONALLY, so its second
/// argument needs a realizable order, which a declaration has none of (§7
/// declined import applied to the sibling argument).
#[test]
fn combine_will_not_read_a_declared_shape_positionally() {
    let src = "<?php\n/** @param array{a: int, b: int} $v */\n\
               function f(array $v): void { \\PHPStan\\dumpType(array_combine(['x', 'y'], $v)); }\n";
    assert_eq!(one_type(src), "dumped type: associative-array<mixed> (asserted)");
}

// The gate

#[test]
fn a_silent_engine_withholds_every_name_in_the_wave() {
    struct Silent;
    impl Folder for Silent {
        fn fold(&mut self, _n: &str, _a: &[ArgValue], _strict: bool) -> Option<ArgValue> {
            None
        }
    }
    for expr in [
        "array_key_first(['a' => 1])",
        "array_first(['a' => 1])",
        "array_fill_keys(['a'], 1)",
        "array_combine(['a'], [1])",
        "array_diff_key(['a' => 1], ['b' => 2])",
        "array_intersect_key(['a' => 1], ['a' => 2])",
    ] {
        let src = format!("<?php\nfunction f(): void {{ \\PHPStan\\dumpType({expr}); }}\n");
        let tree = SourceTree::parse(&src);
        let ds: Vec<Diagnostic> = check_with(&tree, &[], "t.php", &mut Silent)
            .into_iter()
            .filter(|d| d.id == DEBUG_TYPE_ID)
            .collect();
        assert_eq!(ds.len(), 1, "{expr}");
        assert!(
            !ds[0].message.contains('{'),
            "the rule fired without an engine to gate it: {expr} → {}",
            ds[0].message
        );
    }
}

// The abstract array-key cast (issue #336, first rung)

/// The CAST decides the key class, not the value's base: a `decimal-int-string`
/// is a string whose key is `int`, because PHP rewrites a string that spells an
/// integer the way it writes one back — invisible to the old all-int test.
#[test]
fn a_flipped_decimal_int_string_is_keyed_by_int() {
    assert_eq!(
        declared("list<decimal-int-string>", "array_flip($v)"),
        "dumped type: array<int, int> (asserted)"
    );
}

/// The complement: every string that keeps its identity as an array key.
#[test]
fn a_flipped_non_decimal_int_string_stays_string_keyed() {
    assert_eq!(
        declared("list<non-decimal-int-string>", "array_flip($v)"),
        "dumped type: array<string, int> (asserted)"
    );
}

/// `string` casts to `int | non-decimal-int-string` and `numeric-string` to
/// `int | numeric-string&non-decimal-int-string` — two-base unions neither a
/// `Fact` nor a `KeyClass` can hold, so the key falls to `array-key` knowingly.
#[test]
fn the_two_base_unions_take_the_array_key_floor() {
    assert_eq!(declared("list<string>", "array_flip($v)"), "dumped type: array<int> (asserted)");
    assert_eq!(
        declared("list<numeric-string>", "array_flip($v)"),
        "dumped type: array<int> (asserted)"
    );
    assert_eq!(declared("list<array-key>", "array_flip($v)"), "dumped type: array<int> (asserted)");
}

#[test]
fn a_witnessed_flip_reads_the_cast_too() {
    let src = |decl: &str| {
        format!(
            "<?php\n/** @param {decl} $v */\nfunction f(string $v): void {{ \\PHPStan\\dumpType(array_flip([$v, $v])); }}\n"
        )
    };
    assert_eq!(one_type(&src("decimal-int-string")), "dumped type: array<int, 0|1> (asserted)");
    assert_eq!(
        one_type(&src("non-decimal-int-string")),
        "dumped type: array<string, 0|1> (asserted)"
    );
}

/// Exact where the abstract rung declines: each proven value has an exact key,
/// so a mixed set is only `array-key` when the keys really differ in class.
#[test]
fn a_finite_value_set_casts_key_by_key() {
    assert_eq!(dump("array_flip(['1', '2'])"), "dumped type: array{1: 0, 2: 1}");
    assert_eq!(dump("array_flip(['a', 'b'])"), "dumped type: array{a: 0, b: 1}");
}

// array_fill_keys on the declared lane (#336 piece 2)

/// The witnessed lane computes entry by entry; here only the key CLASS is
/// knowable, from the array-key CAST of the value union — which is what lets a
/// `string`-based value key an `int` array.
#[test]
fn fill_keys_over_a_declared_subject_answers_the_key_class() {
    assert_eq!(
        declared("list<decimal-int-string>", "array_fill_keys($v, null)"),
        "dumped type: array<int, null> (asserted)"
    );
    assert_eq!(
        declared("list<non-decimal-int-string>", "array_fill_keys($v, null)"),
        "dumped type: array<string, null> (asserted)"
    );
    // A two-base union has no `KeyClass`, so the key falls to `array-key`.
    assert_eq!(
        declared("list<string>", "array_fill_keys($v, null)"),
        "dumped type: array<null> (asserted)"
    );
}

/// Every value becomes a key — none is skipped — so a non-empty subject fills a
/// non-empty array; `array_flip` drops non-`int|string` values instead
/// (`array_fill_keys` keeps even an array value, as string key `'Array'`).
#[test]
fn fill_keys_keeps_non_emptiness_where_flip_drops_it() {
    assert_eq!(
        declared("non-empty-list<decimal-int-string>", "array_fill_keys($v, null)"),
        "dumped type: non-empty-array<int, null> (asserted)"
    );
    assert_eq!(
        declared("non-empty-list<decimal-int-string>", "array_flip($v)"),
        "dumped type: array<int, int> (asserted)"
    );
}
