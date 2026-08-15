//! Statement-level inline `/** @var T $x */` casts (ADR-0073).
//!
//! The tag replaces the variable's contract lane with `Asserted` arms (ADR-0037).
//! A single array arm also seeds the value lane with its canonical shape fact
//! (ADR-0052 §9, ADR-0062 S3), matching `@param` entry seeding.
//!
//! Cast-derived facts never premise a finding; every fixture asserts zero
//! non-debug emission.

use std::collections::HashMap;

use steins_domain::{Base, Fact, IntRange, Refinement};
use steins_infer::{DEBUG_TYPE_ID, Diagnostic, Folder, check_with};
use steins_syntax::{ArgValue, SourceTree};

/// A mock PHP answering the reflected declarations the S7 admission gates
/// consult (ADR-0061 §2 / ADR-0062 S7) — no folding; the cast lane is
/// order-declared, so nothing here may execute over a witnessed order.
#[derive(Default)]
struct Mock {
    facts: HashMap<String, Fact>,
    types: HashMap<String, String>,
}

impl Mock {
    fn sidecar() -> Mock {
        let mut facts = HashMap::new();
        facts.insert(
            "count".to_owned(),
            Fact::refined(Base::Int, Refinement::Int(IntRange::NON_NEGATIVE), false),
        );
        let mut types = HashMap::new();
        for f in ["array_values", "array_keys"] {
            types.insert(f.to_owned(), "array".to_owned());
        }
        types.insert("count".to_owned(), "int".to_owned());
        Mock { facts, types }
    }
}

impl Folder for Mock {
    /// No folding: the cast lane is order-declared, so no fixture here may ever
    /// reach the sidecar with a witnessed array.
    fn fold(&mut self, _name: &str, _args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        true
    }
    fn builtin_return_fact(&mut self, name: &str) -> Option<Fact> {
        self.facts.get(&name.to_ascii_lowercase()).cloned()
    }
    fn builtin_return_type(&mut self, name: &str) -> Option<String> {
        self.types.get(&name.to_ascii_lowercase()).cloned()
    }
}

fn diagnostics(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    // `untyped.*` (ADR-0078, #200) reports on the fixtures' own deliberately-untyped
    // declarations, not the behaviour under test; dropped to keep assertions meaningful.
    check_with(&tree, &[], "t.php", &mut Mock::sidecar())
        .into_iter()
        .filter(|d| !d.id.starts_with("untyped."))
        .collect()
}

/// All `debug.type` bodies of `src`, in order, asserting the source produced NO
/// other finding (a cast may only silence, never emit).
fn dump_types(src: &str) -> Vec<String> {
    let ds = diagnostics(src);
    let other: Vec<&Diagnostic> = ds.iter().filter(|d| !d.id.starts_with("debug.")).collect();
    assert!(other.is_empty(), "a cast fixture emitted a finding: {other:?}");
    ds.iter().filter(|d| d.id == DEBUG_TYPE_ID).map(|d| d.message.clone()).collect()
}

/// The single `debug.type` body of a one-dump source.
fn one_type(src: &str) -> String {
    let mut types = dump_types(src);
    assert_eq!(types.len(), 1, "expected exactly one debug.type dump");
    types.remove(0)
}

/// A one-statement cast fixture: cast `$arr` (declared `array $arr`) to `decl`,
/// dump `expr`.
fn cast_dump(decl: &str, expr: &str) -> String {
    one_type(&format!(
        "<?php\nfunction f(array $arr): void {{\n  /** @var {decl} $arr */\n  \\PHPStan\\dumpType({expr});\n}}\n"
    ))
}


// The cast seeds what a `@param` would (the measured gap)


#[test]
fn a_cast_shape_feeds_the_projection_family() {
    // The issue's own two probes — before ADR-0073 the tag seeded nothing here.
    assert_eq!(cast_dump("array{a: int, b: int}", "count($arr)"), "dumped type: 2 (asserted)");
    assert_eq!(
        cast_dump("array{a: int, b: int}", "array_values($arr)"),
        "dumped type: non-empty-list<int> (asserted)"
    );
}

#[test]
fn the_variable_itself_spells_the_declared_type() {
    assert_eq!(
        cast_dump("array{a: int, b: int}", "$arr"),
        "dumped type: array{a: int, b: int} (asserted)"
    );
}

#[test]
fn a_scalar_cast_seeds_the_contract_lane() {
    // No shape fact (nothing for the array stratum), but the lane spells — the
    // same split the `@param` seeding makes for a scalar declaration.
    let src = "<?php\nfunction f($x): void {\n  /** @var positive-int $x */\n  \\PHPStan\\dumpType($x);\n}\n";
    assert_eq!(one_type(src), "dumped type: int<1, max> (asserted)");
}

#[test]
fn a_nullable_array_cast_declines_the_count_transfer() {
    // `count(null)` is a TypeError, so the S7 gate takes `nullable: false` facts
    // only — the cast stands in the lane, and count falls back to its envelope.
    assert_eq!(cast_dump("array{a: int}|null", "count($arr)"), "dumped type: int<0, max>");
    assert_eq!(
        cast_dump("array{a: int}|null", "$arr"),
        "dumped type: null|array{a: int} (asserted)"
    );
}


// Flow: where the cast is (and is not) in force


#[test]
fn the_cast_is_in_force_from_its_statement_on_and_a_re_cast_replaces_it() {
    let src = "<?php\nfunction f(array $arr): void {\n  \\PHPStan\\dumpType(count($arr));\n  /** @var array{a: int, b: int} $arr */\n  \\PHPStan\\dumpType(count($arr));\n  /** @var array{a: int} $arr */\n  \\PHPStan\\dumpType(count($arr));\n}\n";
    assert_eq!(
        dump_types(src),
        [
            "dumped type: int<0, max>",
            "dumped type: 2 (asserted)",
            "dumped type: 1 (asserted)",
        ]
    );
}

#[test]
fn an_assignment_to_the_variable_erases_the_cast() {
    // Assignment-form `@var` (PHPStan casts the RHS there) is unsupported: the rebind
    // forgets the lane, so the tail sees plain `array` again, never a stale claim.
    let src = "<?php\nfunction f(array $arr, $u): void {\n  /** @var array{a: int} $arr */\n  $arr = $u;\n  \\PHPStan\\dumpType(count($arr));\n}\n";
    assert_eq!(one_type(src), "dumped type: int<0, max>");
}

#[test]
fn a_comment_in_the_gap_breaks_the_adjacency() {
    let src = "<?php\nfunction f(array $arr): void {\n  /** @var array{a: int} $arr */\n  // note\n  \\PHPStan\\dumpType(count($arr));\n}\n";
    assert_eq!(one_type(src), "dumped type: int<0, max>");
}


// The guards: property targets, templates, precedence


#[test]
fn a_property_target_never_casts_the_receiver() {
    // `@var int $obj->prop` speaks about the property; treating it as a cast of
    // `$obj` could manufacture declared-receiver findings out of thin air.
    let src = "<?php\nfunction f(object $obj, array $arr): void {\n  /** @var int $obj->prop */\n  \\PHPStan\\dumpType(count($arr));\n}\n";
    assert_eq!(one_type(src), "dumped type: int<0, max>");
}

#[test]
fn a_template_name_stays_opaque() {
    // The issue #5 shadow discipline, applied to the body: `T` is a template
    // parameter, not a class — the cast lane must not spell (or judge) it.
    let src = "<?php\n/** @template T */\nfunction f($x): void {\n  /** @var T $x */\n  \\PHPStan\\dumpType($x);\n}\n";
    assert_eq!(one_type(src), "dumped type: unknown");
}

#[test]
fn a_prefixed_var_displaces_the_plain_one() {
    let src = "<?php\nfunction f(array $arr): void {\n  /**\n   * @phpstan-var array{a: int} $arr\n   * @var array{a: int, b: int} $arr\n   */\n  \\PHPStan\\dumpType(count($arr));\n}\n";
    assert_eq!(one_type(src), "dumped type: 1 (asserted)");
}
