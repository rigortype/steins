//! Acceptance tests for the **method-shaped catalog rows** (issue #67): the
//! class-world twin of `effect_labels`, and `io.db`'s producer. `PDO::query` is
//! a row keyed by a class rather than a function name.
//!
//! The tracer is deliberately narrow, and the tests focus on the *edges* of that
//! narrowness, where a zero-FP claim is kept or lost: a receiver the analyzer
//! cannot name is silent and tainted, a namespaced `PDO` is somebody else's
//! class, and a project class named `PDO` shadows the catalog — its body is the
//! truth, not a hand-written row.

use steins_infer::{Diagnostic, EFFECT_ID, EffectSummary, check, effect_summary};
use steins_syntax::SourceTree;

/// Parse + check inline PHP, returning only the effect-envelope findings.
fn effects(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php").into_iter().filter(|d| d.id == EFFECT_ID).collect()
}

fn one(src: &str) -> Diagnostic {
    let f = effects(src);
    assert_eq!(f.len(), 1, "expected exactly one effect finding, got: {f:#?}");
    f.into_iter().next().unwrap()
}

fn summary(src: &str, symbol: &str) -> EffectSummary {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let classes = tree.classes().to_vec();
    effect_summary(&tree, &functions, &classes)
        .into_iter()
        .find(|s| s.symbol == symbol)
        .unwrap_or_else(|| panic!("no summary for {symbol}"))
}

// The headline: PDO::query on a Pure function

#[test]
fn pure_calling_pdo_query_is_flagged_with_exact_message() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(): void { (new \\PDO(\"sqlite::memory:\"))->query(\"SELECT 1\"); }\n";
    let d = one(src);
    assert_eq!(d.id, EFFECT_ID);
    assert_eq!(
        d.message,
        "PDO::query() has effect io.db, but f() is declared #[\\Steins\\Pure]"
    );
    assert_eq!(d.line, 3);
}

#[test]
fn io_db_shows_up_in_the_effect_summary() {
    let src = "<?php\nfunction f(): void { (new \\PDO(\"sqlite::memory:\"))->query(\"SELECT 1\"); }\n";
    let s = summary(src, "f");
    assert_eq!(s.labels, vec!["io.db".to_owned()], "the summary names the label");
    assert!(s.exhaustive, "a catalogued row is a complete answer — no `…?`");
}

// Subsumption: the envelope that passes and the one that does not

#[test]
fn effect_io_subsumes_io_db() {
    let src = "<?php\n#[\\Steins\\Effect('io')]\nfunction f(): void { (new \\PDO(\"sqlite::memory:\"))->query(\"SELECT 1\"); }\n";
    assert_eq!(effects(src).len(), 0, "coarse io admits io.db → silent");
}

#[test]
fn exact_io_db_envelope_admits_it() {
    let src = "<?php\n#[\\Steins\\Effect('io.db')]\nfunction f(): void { (new \\PDO(\"sqlite::memory:\"))->exec(\"DELETE FROM t\"); }\n";
    assert_eq!(effects(src).len(), 0, "io.db admits io.db → silent");
}

#[test]
fn sibling_io_fs_envelope_does_not_admit_io_db() {
    let src = "<?php\n#[\\Steins\\Effect('io.fs')]\nfunction f(): void { (new \\PDO(\"sqlite::memory:\"))->query(\"SELECT 1\"); }\n";
    let d = one(src);
    assert_eq!(
        d.message,
        "PDO::query() has effect io.db, but f() is declared #[\\Steins\\Effect('io.fs')] — io.db exceeds the envelope"
    );
}

// Every row in the tracer, through the one receiver form that resolves

#[test]
fn all_six_rows_reach_a_pure_envelope() {
    // `PDOStatement` has a private constructor, but a direct `new` is the only
    // receiver form that exercises its rows until `->prepare()` return types flow.
    for (class, method) in [
        ("PDO", "query"),
        ("PDO", "exec"),
        ("PDO", "prepare"),
        ("PDOStatement", "execute"),
        ("PDOStatement", "fetch"),
        ("PDOStatement", "fetchAll"),
    ] {
        let src = format!(
            "<?php\n#[\\Steins\\Pure]\nfunction f(): void {{ (new \\{class}())->{method}(); }}\n"
        );
        let d = one(&src);
        assert_eq!(
            d.message,
            format!("{class}::{method}() has effect io.db, but f() is declared #[\\Steins\\Pure]"),
            "{class}::{method} colors io.db end to end"
        );
    }
}

#[test]
fn class_and_method_names_fold_case() {
    // PHP is case-insensitive on both, so `new pdo(...)->QUERY()` is the same row.
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(): void { (new \\pdo(\"x\"))->QUERY(\"SELECT 1\"); }\n";
    let d = one(src);
    assert!(d.message.contains("has effect io.db"), "got: {}", d.message);
}

// Transitive propagation through a project helper

#[test]
fn io_db_propagates_through_a_helper_with_via_provenance() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(): void { rows(); }\nfunction rows(): void { (new \\PDO(\"sqlite::memory:\"))->query(\"SELECT 1\"); }\n";
    let d = one(src);
    assert_eq!(
        d.message,
        "rows() has effect io.db (via PDO::query at line 4), but f() is declared #[\\Steins\\Pure]"
    );
    assert_eq!(d.line, 3, "reported at the outer rows() call site");
}

// An unresolvable receiver is exactly as silent as it was

#[test]
fn variable_receiver_stays_silent_and_taints() {
    // `$pdo->query()` is not covered: the tracer adds no value tracking, so the
    // receiver's class is not proven, and an unproven effect is silence plus
    // taint — never a guess.
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(\\PDO $pdo): void { $pdo->query(\"SELECT 1\"); }\n";
    assert_eq!(effects(src).len(), 0, "unproven receiver → no finding");
    let s = summary(src, "f");
    assert!(s.labels.is_empty(), "nothing proven");
    assert!(!s.exhaustive, "and the taint says so: `…?`");
}

#[test]
fn local_binding_of_a_new_pdo_is_not_tracked_either() {
    // The origin scan cannot name a local binding without a flow environment.
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(): void { $pdo = new \\PDO(\"sqlite::memory:\"); $pdo->query(\"SELECT 1\"); }\n";
    assert_eq!(effects(src).len(), 0, "a bound receiver is still unproven here");
    assert!(!summary(src, "f").exhaustive, "tainted, not colored");
}

#[test]
fn a_chained_statement_receiver_is_unproven() {
    // `->prepare()` return types do not flow, so the `->execute()` receiver has
    // no proven class and the PDOStatement rows stay dormant.
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(): void { (new \\PDO(\"x\"))->prepare(\"SELECT 1\")->execute(); }\n";
    let f = effects(src);
    assert_eq!(f.len(), 1, "only the prepare() row fires: {f:#?}");
    assert!(f[0].message.starts_with("PDO::prepare() has effect io.db"), "got: {}", f[0].message);
    assert!(!summary(src, "f").exhaustive, "the unproven ->execute() receiver taints");
}

#[test]
fn an_uncatalogued_pdo_method_taints_rather_than_going_pure() {
    // `Some(&[])` would mean catalogued-pure; the table says `None`, which must
    // widen exactly like an uncatalogued builtin function does.
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(): void { (new \\PDO(\"x\"))->beginTransaction(); }\n";
    assert_eq!(effects(src).len(), 0, "no row → no finding");
    assert!(!summary(src, "f").exhaustive, "no row → taint");
}

// Precedence: a project class shadows the catalog

#[test]
fn a_project_class_named_pdo_shadows_the_catalog() {
    // The project's own `PDO::query` is the truth here, not the catalog: its
    // body's real effect (nondet.random) surfaces, not io.db.
    let src = "<?php\nfinal class PDO {\n  public function query(string $q): int { return rand(); }\n}\n#[\\Steins\\Pure]\nfunction f(): void { (new PDO())->query(\"SELECT 1\"); }\n";
    let d = one(src);
    assert_eq!(
        d.message,
        "PDO::query() has effect nondet.random (via rand at line 3), but f() is declared #[\\Steins\\Pure]"
    );
}

#[test]
fn a_pure_project_pdo_is_silent_under_pure() {
    // A genuinely pure shadowing class is silent — the catalog row would have
    // made this a false positive.
    let src = "<?php\nfinal class PDO {\n  public function query(string $q): string { return strtolower($q); }\n}\n#[\\Steins\\Pure]\nfunction f(): void { (new PDO())->query(\"SELECT 1\"); }\n";
    assert_eq!(effects(src).len(), 0, "the project class decides, and it is pure");
    assert!(summary(src, "f").exhaustive, "a resolved project edge is exhaustive");
}

// The key is the global class name, resolved, not spelled

#[test]
fn a_namespaced_pdo_is_not_the_engines_pdo() {
    // `namespace App; new PDO()` is `App\PDO` — PHP does not fall back to the
    // global namespace for class names. Some class Steins has not indexed, so:
    // silence and taint.
    let src = "<?php\nnamespace App;\n#[\\Steins\\Pure]\nfunction f(): void { (new PDO(\"x\"))->query(\"SELECT 1\"); }\n";
    assert_eq!(effects(src).len(), 0, "App\\PDO is not PDO");
    assert!(!summary(src, "f").exhaustive, "unknown external class → taint");
}

#[test]
fn an_imported_pdo_inside_a_namespace_is_the_engines_pdo() {
    // `use PDO;` resolves back to the global name, so the row applies — the
    // lookup keys the resolved FQN, not the spelling.
    let src = "<?php\nnamespace App;\nuse PDO;\n#[\\Steins\\Pure]\nfunction f(): void { (new PDO(\"x\"))->query(\"SELECT 1\"); }\n";
    let d = one(src);
    assert!(d.message.contains("PDO::query() has effect io.db"), "got: {}", d.message);
}

// The rows never leak into the function-keyed world

#[test]
fn no_plain_function_is_colored_io_db() {
    // `io.db`'s only producer is the method table. A same-named free function is
    // uncatalogued, and stays that way.
    assert_eq!(steins_catalog::effect_labels("query"), None);
    assert_eq!(steins_catalog::effect_labels("pdo_query"), None);
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(): void { query(\"SELECT 1\"); }\n";
    assert_eq!(effects(src).len(), 0, "a free query() proves nothing");
}
