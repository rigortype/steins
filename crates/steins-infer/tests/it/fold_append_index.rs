//! A folded array is a literal only when PHP's next append index for it is the
//! one its keys state.
//!
//! A literal lifts with an order witness, and `$a[] = v` / `array_push` read the
//! landing key off that witness as the largest integer key plus one. Every
//! array-returning name on the fold allowlist builds its result fresh, which
//! makes that true, except `array_unique` with a flag other than `SORT_STRING`:
//! php-src duplicates the input and deletes the repeats, so the input's index
//! carries over. Measured with `php -r` on 8.1.32, 8.2.33, 8.3.33, 8.4.25 and
//! 8.5.10 alike:
//!
//! ```text
//! $a = array_unique(['a', 'a'], SORT_REGULAR); $a[] = 9;           => 0, 2
//! $a = array_unique(['a', 'a']); $a[] = 9;                         => 0, 1
//! $a = array_unique(['a', 'a', 'b'], SORT_REGULAR); $a[] = 9;      => 0, 2, 3
//! $a = array_unique([5 => 'x'], SORT_REGULAR); $a[] = 9;           => 5, 6
//! $a = array_unique(['k' => 'a', 0 => 'a'], SORT_REGULAR);
//! $a[-5] = 1; $a[] = 9;                                            => 'k', -5, 1
//! ```
//!
//! The engine here is a mock, so the rows run without PHP;
//! `fold_allowlist_growth.rs` runs the first two through the real sidecar.

use std::collections::HashMap;

use steins_domain::{Base, Fact, IntRange, Refinement};
use steins_infer::{DEBUG_TYPE_ID, Diagnostic, Folder, check_with};
use steins_syntax::{ArgValue, ArrayKey, NormKey, SourceTree, normalize_array};

/// An engine that folds `array_unique` over literals whose repeats are spelled
/// alike, which is what every flag answers for them: the first entry of each
/// value stays, under its own key. It also answers `count`'s envelope, which the
/// shape rung needs before it states a count.
struct Engine {
    facts: HashMap<String, Fact>,
}

impl Engine {
    fn new() -> Engine {
        let mut facts = HashMap::new();
        facts.insert(
            "count".to_owned(),
            Fact::refined(Base::Int, Refinement::Int(IntRange::NON_NEGATIVE), false),
        );
        Engine { facts }
    }
}

impl Folder for Engine {
    fn fold(&mut self, name: &str, args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
        let ("array_unique", [ArgValue::Array(items), ..]) = (name, args) else { return None };
        let mut kept: Vec<(ArrayKey, ArgValue)> = Vec::new();
        for (key, value) in normalize_array(items)? {
            if kept.iter().all(|(_, seen)| *seen != value) {
                let key = match key {
                    NormKey::Int(i) => ArrayKey::Int(i),
                    NormKey::Str(s) => ArrayKey::Str(s),
                };
                kept.push((key, value));
            }
        }
        Some(ArgValue::Array(kept))
    }
    fn absence_family_available(&mut self) -> bool {
        true
    }
    fn builtin_return_fact(&mut self, name: &str) -> Option<Fact> {
        self.facts.get(name).cloned()
    }
}

/// The `debug.type` bodies of `body` run inside a function, asserting that it
/// produced no other finding.
fn dumps(body: &str) -> Vec<String> {
    let tree = SourceTree::parse(&format!("<?php\nfunction f(): void {{ {body} }}\n"));
    let ds = check_with(&tree, &[], "t.php", &mut Engine::new());
    let other: Vec<&Diagnostic> = ds.iter().filter(|d| !d.id.starts_with("debug.")).collect();
    assert!(other.is_empty(), "unexpected findings: {other:?}");
    ds.iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message.replace("dumped type: ", ""))
        .collect()
}

#[test]
fn a_non_default_flag_leaves_the_append_key_unknown() {
    // PHP puts the 9 at key 2. `list{'a', 9}` said key 1.
    for flag in ["SORT_REGULAR", "SORT_NUMERIC", "SORT_LOCALE_STRING", "0"] {
        assert_eq!(
            dumps(&format!(
                "$a = array_unique(['a', 'a'], {flag}); $a[] = 9; \\PHPStan\\dumpType($a);"
            )),
            vec!["non-empty-array{9|'a', ...<int, 9>}"],
            "{flag}"
        );
    }
}

#[test]
fn the_folded_value_keeps_its_type_without_the_witness() {
    assert_eq!(
        dumps(
            "$a = array_unique(['a', 'a'], SORT_REGULAR); \\PHPStan\\dumpType($a); \
             \\PHPStan\\dumpType(count($a)); \
             \\PHPStan\\dumpType(array_unique(['a', 'a'], SORT_REGULAR)); \
             \\PHPStan\\dumpType(count(array_unique(['a', 'a'], SORT_REGULAR)));"
        ),
        vec!["list{'a'}", "1", "list{'a'}", "1"]
    );
}

#[test]
fn the_default_flag_is_a_fresh_build_and_stays_exact() {
    for flags in ["", ", SORT_STRING", ", 2"] {
        assert_eq!(
            dumps(&format!(
                "$a = array_unique(['a', 'a']{flags}); $a[] = 9; \\PHPStan\\dumpType($a);"
            )),
            vec!["list{'a', 9}"],
            "{flags:?}"
        );
    }
}

#[test]
fn a_surviving_largest_integer_key_keeps_the_exact_append() {
    // The input's index is its largest key plus one, and that key is still there.
    assert_eq!(
        dumps(
            "$a = array_unique(['a', 'a', 'b'], SORT_REGULAR); $a[] = 9; \
             \\PHPStan\\dumpType($a); \
             $b = array_unique([5 => 'x'], SORT_REGULAR); $b[] = 9; \\PHPStan\\dumpType($b);"
        ),
        vec!["array{0: 'a', 2: 'b', 3: 9}", "array{5: 'x', 6: 9}"]
    );
}

#[test]
fn a_removed_integer_key_is_not_read_as_no_index_set() {
    // PHP's index is 1, so a negative write does not move it and the 9 lands on
    // 1. A witness with no integer key would have put it on -4.
    assert_eq!(
        dumps(
            "$a = array_unique(['k' => 'a', 0 => 'a'], SORT_REGULAR); \\PHPStan\\dumpType($a); \
             $a[-5] = 1; $a[] = 9; \\PHPStan\\dumpType($a);"
        ),
        vec!["array{k: 'a'}", "non-empty-array{-5: 1|9, k: 'a', ...}"]
    );
}
