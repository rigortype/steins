//! Steins' native-parameter verdict, pinned cell for cell against **PHP's own**
//! (issue #391).
//!
//! `harness/coercion-grid/witness-{strict,coercive}.tsv` were produced by running
//! the calls on PHP 8.5.9 (`harness/coercion-grid/witness.php`); this file
//! rebuilds each of the 72 cells per mode as a one-line source and asserts that
//! `type.argument-mismatch` fires exactly where PHP raised a `TypeError`.
//!
//! The grid is the oracle for the base-level judgment `type.maybe-argument-mismatch`
//! is built on: an abstract fact's arm is rejected iff **every** witness of its
//! base is, so the witness set has to cover one value per equivalence class of
//! PHP's coercion behaviour (see the harness README for why `bool` and `string`
//! each need two).
//!
//! The two known divergences are named exceptions rather than a weakened
//! assertion, so a new one in either direction fails this test.

use std::collections::HashSet;
use std::path::PathBuf;

use steins_infer::{ID, check};
use steins_syntax::SourceTree;

/// One row of a witness `.tsv`.
struct Cell {
    /// The parameter type as spelled in the signature (`?int`, `string|false`, …).
    param: String,
    /// The equivalence-class name of the value (`string(numeric)`, `bool(false)`, …).
    class: String,
    /// The value's literal spelling, pasted into the call.
    literal: String,
    /// PHP's answer: `true` when the call raised a `TypeError`.
    php_errors: bool,
}

/// The cells PHP rejects and Steins deliberately does not, as
/// `(param, value-class)` pairs. Both are silence-direction and documented in
/// `harness/coercion-grid/README.md`:
///
/// * every `array` cell — `is_type_error` answers `false` for an array argument
///   by construction (the phpdoc contract relation owns that mismatch);
/// * `null` into a class-typed parameter — the native check stays silent there,
///   which is what keeps `f(\DateTime $d = null)` from convicting.
fn is_known_divergence(param: &str, class: &str) -> bool {
    class == "array" || (param == "DateTime" && class == "null")
}

fn grid(mode: &str) -> Vec<Cell> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../harness/coercion-grid")
        .join(format!("witness-{mode}.tsv"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("witness grid unreadable at {}: {e}", path.display()));
    let cells: Vec<Cell> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let c: Vec<&str> = line.split('\t').collect();
            assert!(c.len() >= 5, "malformed witness row: {line}");
            assert_eq!(c[0], mode, "row belongs to another mode: {line}");
            Cell {
                param: c[1].to_owned(),
                class: c[2].to_owned(),
                literal: c[3].to_owned(),
                php_errors: match c[4] {
                    "TypeError" => true,
                    "accept" => false,
                    other => panic!("unknown verdict {other:?} in {line}"),
                },
            }
        })
        .collect();
    assert_eq!(cells.len(), 72, "the grid is 9 witnesses x 8 parameter types");
    cells
}

/// Whether Steins convicts `f(<literal>)` for `function f(<param> $v)` in `mode`.
fn steins_errors(mode: &str, param: &str, literal: &str) -> bool {
    let decl = if mode == "strict" { "declare(strict_types=1);\n" } else { "" };
    let src = format!("<?php\n{decl}function f({param} $v): void {{}}\nf({literal});\n");
    let tree = SourceTree::parse(&src);
    let functions = tree.functions().to_vec();
    let ds = check(&tree, &functions, "t.php");
    let other: Vec<&steins_infer::Diagnostic> =
        ds.iter().filter(|d| d.id != ID && !d.id.starts_with("untyped.")).collect();
    assert!(other.is_empty(), "the grid cell emitted an unrelated finding: {other:?}");
    ds.iter().any(|d| d.id == ID)
}

fn agrees(mode: &str) -> (usize, HashSet<String>) {
    let mut divergent = HashSet::new();
    let cells = grid(mode);
    let n = cells.len();
    for c in &cells {
        let steins = steins_errors(mode, &c.param, &c.literal);
        if steins != c.php_errors {
            assert!(
                !steins,
                "{mode}: Steins convicts {}({}) where PHP accepts it — a false positive, \
                 never an admissible divergence",
                c.param, c.literal
            );
            divergent.insert(format!("{}/{}", c.param, c.class));
        }
    }
    (n, divergent)
}

#[test]
fn the_strict_grid_agrees_with_php_cell_for_cell() {
    let (n, divergent) = agrees("strict");
    assert_eq!(n, 72);
    for d in &divergent {
        let (param, class) = d.split_once('/').expect("formatted above");
        assert!(is_known_divergence(param, class), "new divergence in strict mode: {d}");
    }
    assert_eq!(divergent.len(), 9, "the known silences are 7 array cells + 1 class/null: {divergent:?}");
}

#[test]
fn the_coercive_grid_agrees_with_php_cell_for_cell() {
    let (n, divergent) = agrees("coercive");
    assert_eq!(n, 72);
    for d in &divergent {
        let (param, class) = d.split_once('/').expect("formatted above");
        assert!(is_known_divergence(param, class), "new divergence in coercive mode: {d}");
    }
    assert_eq!(divergent.len(), 9, "the known silences are 7 array cells + 1 class/null: {divergent:?}");
}

#[test]
fn the_two_modes_disagree_where_php_does() {
    // The grid earns its keep only if it is not the same table twice. Coercive
    // mode accepts a numeric string as an `int` and refuses a non-numeric one:
    // the split that makes `string` need two witnesses.
    assert!(steins_errors("strict", "int", "'5'"));
    assert!(!steins_errors("coercive", "int", "'5'"));
    assert!(steins_errors("coercive", "int", "'abc'"));
    // …and the `false` member of `string|false` accepts exactly one of the two
    // bools in strict mode: the split that makes `bool` need two witnesses.
    assert!(!steins_errors("strict", "string|false", "false"));
    assert!(steins_errors("strict", "string|false", "true"));
}
