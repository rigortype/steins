//! Issue #327 — an array literal keeps its fact when its elements do not.
//!
//! `val_of` needs a proven value per element and answers `None` on the first one
//! it cannot build, so before this slice a single unproven element dropped the
//! fact for the **whole** array: keys, entry count, sealing and every proven
//! sibling, all at once. `['p' => 1, 'q' => $s]` knew nothing.
//!
//! What survives an unknown element is everything that was never about the
//! values. `normalize_array` resolves auto indices, last-wins duplicates and the
//! version-dependent next-int rule without inspecting one, so the key sequence is
//! computable whatever the elements are — and a literal, by being a literal,
//! seals its own key universe.
//!
//! Two disciplines are pinned here beside the answers:
//!
//! * **The concrete path is untouched.** A fully-literal array is still a
//!   `Singleton`, still `Verified`, still spelled as it always was. Every
//!   assertion that says so is a *negative* test — the slice is not allowed to
//!   pay for the abstract case with the concrete one.
//! * **Order is provenance.** A literal-seeded shape prints and projects the
//!   order it was *built* in; a declared shape keeps the canonical key order,
//!   because trusting a docblock's field order in a positional projection is
//!   phpstan/phpstan#14940's false-positive class (ADR-0062 §2, §7).
//!
//! Every expected type here was measured against PHPStan 2.2.2 — the comment on
//! each is that oracle's answer, not a recollection.

use std::collections::HashMap;

use steins_domain::{Base, Fact, IntRange, Refinement};
use steins_infer::{DEBUG_TYPE_ID, Diagnostic, Folder, check_with};
use steins_syntax::{ArgValue, SourceTree};

/// A mock sidecar answering the reflected envelopes the `count` transfer
/// consults. Without an envelope the type rung is withheld — the gate working —
/// so the transfer tests need this to observe anything at all.
#[derive(Default)]
struct Mock {
    facts: HashMap<String, Fact>,
}

impl Mock {
    fn sidecar() -> Mock {
        let mut facts = HashMap::new();
        facts.insert(
            "count".to_owned(),
            Fact::refined(Base::Int, Refinement::Int(IntRange::NON_NEGATIVE), false),
        );
        facts
            .insert("array_is_list".to_owned(), Fact::General { base: Base::Bool, nullable: false });
        Mock { facts }
    }
}

impl Folder for Mock {
    fn fold(&mut self, _name: &str, _args: &[ArgValue]) -> Option<ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        true
    }
    fn builtin_return_fact(&mut self, name: &str) -> Option<Fact> {
        self.facts.get(&name.to_ascii_lowercase()).cloned()
    }
}

fn diagnostics(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "t.php", &mut Mock::sidecar())
        .into_iter()
        .filter(|d| !d.id.starts_with("untyped."))
        .collect()
}

/// The single `debug.type` body a one-dump source produces, asserting on the way
/// that the source produced no other finding.
fn one_type(src: &str) -> String {
    let ds = diagnostics(src);
    let other: Vec<&Diagnostic> = ds.iter().filter(|d| !d.id.starts_with("debug.")).collect();
    assert!(other.is_empty(), "a seeded literal emitted a finding: {other:?}");
    let ty: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ty.len(), 1, "expected exactly one debug.type dump, got {ds:?}");
    ty[0].message.clone()
}

/// A body with two natively-typed parameters to draw abstract elements from,
/// and one dump.
fn dump(body: &str) -> String {
    one_type(&format!("<?php\nfunction f(int $x, string $s): void {{ {body} }}\n"))
}

// ---- The cliff, closed -----------------------------------------------------

#[test]
fn one_unknown_element_costs_that_element_and_nothing_else() {
    // Was `unknown` — the whole fact, for one unproven value.
    assert_eq!(
        dump("$c = ['p' => 1, 'q' => $s]; \\PHPStan\\dumpType($c);"),
        "dumped type: array{p: 1, q: string}"
    );
}

#[test]
fn the_proven_siblings_keep_their_exact_values() {
    assert_eq!(dump("$c = ['p' => 1, 'q' => $s]; \\PHPStan\\dumpType($c['p']);"), "dumped type: 1");
}

#[test]
fn the_entry_count_is_exact_because_the_keys_are() {
    // `count` reads the key structure, which never depended on the values.
    assert_eq!(dump("\\PHPStan\\dumpType(count(['a' => $x, 'b' => $x]));"), "dumped type: 2");
    assert_eq!(
        dump("$c = ['a' => $x, 'b' => $x]; \\PHPStan\\dumpType(count($c));"),
        "dumped type: 2"
    );
}

#[test]
fn a_positional_literal_keeps_its_list_ness() {
    assert_eq!(
        dump("$e = ['x', $s, 'z']; \\PHPStan\\dumpType($e);"),
        "dumped type: list{'x', string, 'z'}"
    );
    assert_eq!(dump("$e = ['x', $s, 'z']; \\PHPStan\\dumpType(count($e));"), "dumped type: 3");
}

#[test]
fn an_element_nobody_proved_is_present_with_an_unknown_value() {
    // The key is proven there; only its value is not. `mixed` is the slot, not
    // the array.
    assert_eq!(
        dump("$c = ['k' => strlen($s) > 2 ? [] : new \\stdClass()]; \\PHPStan\\dumpType($c);"),
        "dumped type: array{k: mixed}"
    );
}

#[test]
fn nesting_recurses_rather_than_flattening_to_unknown() {
    assert_eq!(
        dump("\\PHPStan\\dumpType(['n' => ['deep' => $x]]);"),
        "dumped type: array{n: array{deep: int}}"
    );
}

// ---- Order is provenance ---------------------------------------------------

#[test]
fn a_literal_prints_the_order_it_was_built_in() {
    // The witnessed order is the REVERSE of the canonical key order the fields
    // are sorted into, so this is the case that distinguishes the two.
    assert_eq!(
        dump("$d = ['b' => 1, 'a' => $x]; \\PHPStan\\dumpType($d);"),
        "dumped type: array{b: 1, a: int}"
    );
}

#[test]
fn a_declared_shape_keeps_the_canonical_order() {
    // **Negative test.** A docblock's field order is not runtime order
    // (phpstan/phpstan#14940), and the two provenances must not print alike.
    let src = "<?php\n/** @param array{b: int, a: string} $v */\n\
               function f(array $v): void { \\PHPStan\\dumpType($v); }\n";
    assert_eq!(one_type(src), "dumped type: array{a: string, b: int} (asserted)");
}

#[test]
fn the_key_sequence_decides_list_ness_not_the_key_set() {
    // `[1 => $x, 0 => $x]` has the key SET of a two-element list and is not one:
    // `array_is_list` reads the sequence. The canonically sorted fields cannot
    // tell it from `[0 => …, 1 => …]`, which is why the witness exists.
    assert_eq!(
        dump("$a = [1 => $x, 0 => $x]; \\PHPStan\\dumpType($a);"),
        "dumped type: array{1: int, 0: int}"
    );
    assert_eq!(
        dump("$a = [0 => $x, 1 => $x]; \\PHPStan\\dumpType($a);"),
        "dumped type: list{int, int}"
    );
}

// ---- The concrete path is untouched ---------------------------------------

#[test]
fn a_fully_literal_array_is_still_a_proven_value() {
    assert_eq!(dump("$b = ['p' => 1]; \\PHPStan\\dumpType($b);"), "dumped type: array{p: 1}");
    assert_eq!(
        dump("$b = ['b' => 1, 'a' => 2]; \\PHPStan\\dumpType($b);"),
        "dumped type: array{b: 1, a: 2}"
    );
    assert_eq!(dump("\\PHPStan\\dumpType(count(['x', 'y']));"), "dumped type: 2");
    // Still a `Singleton`, so it still decides an identity.
    assert_eq!(
        dump("$b = ['a', 'b']; \\PHPStan\\dumpType($b === ['a', 'b']);"),
        "dumped type: true"
    );
}

#[test]
fn an_unresolvable_key_set_declines_the_whole_literal() {
    // ADR-0049 A12: with no pinned minor, a literal straddling the 8.3 next-int
    // change has unproven KEYS, and a guessed key set is wrong rather than
    // wide. Neither path may seed anything.
    assert_eq!(
        dump("$a = [-5 => 'a', 'b', 'c']; \\PHPStan\\dumpType($a);"),
        "dumped type: unknown"
    );
    assert_eq!(
        dump("$a = [-5 => 'a', $s, 'c']; \\PHPStan\\dumpType($a);"),
        "dumped type: unknown"
    );
}

// ---- The write path (ADR-0062 §4's write row) ------------------------------

#[test]
fn a_write_onto_a_witnessed_literal_extends_it() {
    // Was `unknown`: one write undid everything the literal had proven.
    assert_eq!(
        dump("$a = []; $a['k'] = $x; \\PHPStan\\dumpType($a);"),
        "dumped type: array{k: int}"
    );
    assert_eq!(
        dump("$g = ['p' => 1]; $g['q'] = 2; \\PHPStan\\dumpType($g);"),
        "dumped type: array{p: 1, q: 2}"
    );
}

#[test]
fn a_witnessed_write_keeps_the_sealing_a_declared_one_loses() {
    // **The distinction, side by side.** A witnessed base has no docblock to
    // have diverged from — its sealing is a fact about the array the code
    // built, so adding a key extends the shape. A DECLARED base's sealing is a
    // claim the write just falsified, so the tail opens (`...`).
    assert_eq!(
        dump("$g = ['p' => 1]; $g['q'] = 2; \\PHPStan\\dumpType(count($g));"),
        "dumped type: 2"
    );
    let declared = "<?php\n/** @param array{p: int} $v */\n\
                    function f(array $v): void { $v['q'] = 2; \\PHPStan\\dumpType($v); }\n";
    assert_eq!(one_type(declared), "dumped type: non-empty-array{p: int, q: 2, ...} (asserted)");
}

#[test]
fn a_write_appends_the_key_where_php_puts_it() {
    assert_eq!(
        dump("$a = ['b' => 1]; $a['a'] = $x; \\PHPStan\\dumpType($a);"),
        "dumped type: array{b: 1, a: int}"
    );
    // Overwriting an existing key moves nothing.
    assert_eq!(
        dump("$a = ['b' => 1, 'a' => 2]; $a['b'] = $x; \\PHPStan\\dumpType($a);"),
        "dumped type: array{b: int, a: 2}"
    );
}

#[test]
fn a_write_recomputes_list_ness_from_the_new_sequence() {
    assert_eq!(dump("$a = []; $a[0] = $x; \\PHPStan\\dumpType($a);"), "dumped type: list{int}");
    assert_eq!(dump("$a = []; $a[1] = $x; \\PHPStan\\dumpType($a);"), "dumped type: array{1: int}");
}

#[test]
fn unset_takes_the_key_out_of_the_sequence() {
    assert_eq!(
        dump("$a = ['a' => 1, 'b' => $x]; unset($a['a']); \\PHPStan\\dumpType($a);"),
        "dumped type: array{b: int}"
    );
}

// ---- Stratum ---------------------------------------------------------------

#[test]
fn a_literal_over_a_declared_element_is_asserted() {
    // ADR-0061 §3's derivation clause: the shape cannot come out more trusted
    // than the element facts it consumed. A `@param` refinement is `Asserted`,
    // so the literal built from it is too — and A-G9's corollary then keeps it
    // out of every proof-layer premise.
    let src = "<?php\n/** @param non-empty-string $v */\n\
               function f(string $v): void { $a = ['k' => $v]; \\PHPStan\\dumpType($a); }\n";
    assert_eq!(one_type(src), "dumped type: array{k: non-empty-string} (asserted)");
}

#[test]
fn a_literal_over_native_elements_stays_verified() {
    // A native `int $x` is the engine's own guarantee, not a claim, so nothing
    // here is demoted: the keys were observed and the slot is `Verified`.
    let out = dump("$a = ['k' => $x]; \\PHPStan\\dumpType($a);");
    assert_eq!(out, "dumped type: array{k: int}");
    assert!(!out.contains("asserted"));
}

// ---- Zero emission ---------------------------------------------------------

#[test]
fn no_finding_is_premised_on_a_seeded_literal() {
    // The sweep `one_type` runs per fixture, over the whole matrix at once: a
    // shape the walk *derived* must not become a finding's premise (ADR-0062
    // A-G9's corollary).
    for body in [
        "$c = ['p' => 1, 'q' => $s]; $y = $c['p']; $z = $c['q'];",
        "$c = ['p' => 1, 'q' => $s]; $y = count($c);",
        "$a = []; $a['k'] = $x; $y = $a['k'];",
        "$a = ['b' => 1, 'a' => $x]; unset($a['a']); $y = count($a);",
        "$a = ['n' => ['deep' => $x]]; $y = $a['n'];",
    ] {
        let src = format!("<?php\nfunction f(int $x, string $s): void {{ {body} }}\n");
        let ds = diagnostics(&src);
        assert!(ds.is_empty(), "a seeded literal premised a finding in `{body}`: {ds:?}");
    }
}
