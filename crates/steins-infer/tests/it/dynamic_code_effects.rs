//! Dynamic code in the effect lane (ADR-0046 amendment, owner ruling
//! 2026-09-26): `eval` is the `eval` label, and `include`/`include_once`/
//! `require`/`require_once` are a proven `io.fs.read`. Both run code no scan
//! sees, so either also leaves the body **non-exhaustive** (`…?`).
//!
//! Before the ruling neither construct contributed anything, so a body holding
//! one was summarized `{}` — exhaustive and effect-free, which is the summary a
//! proven-pure body earns. The tests below pin the three halves separately: the
//! proven label, the `…?` beside it, and the envelope judgment that now moves.

use steins_infer::{
    Diagnostic, EFFECT_ID, EffectSummary, FactKind, NoFold, STATEMENT_NO_EFFECT_ID,
    UNKNOWN_LABEL_ID, annotate_facts, check, effect_summary,
};
use steins_syntax::SourceTree;

/// Parse + check inline PHP, keeping only the diagnostics with `id`.
fn of_id(src: &str, id: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php").into_iter().filter(|d| d.id == id).collect()
}

fn exceeded(src: &str) -> Vec<Diagnostic> {
    of_id(src, EFFECT_ID)
}

fn one(src: &str) -> Diagnostic {
    let f = exceeded(src);
    assert_eq!(f.len(), 1, "expected exactly one envelope finding, got: {f:#?}");
    f.into_iter().next().unwrap()
}

fn silent(src: &str) {
    let f = exceeded(src);
    assert!(f.is_empty(), "expected silence, got: {f:#?}");
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

/// The rendered `annotate` effect margin on `line`.
fn margin(src: &str, line: u32) -> String {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let classes = tree.classes().to_vec();
    annotate_facts(&tree, &functions, &classes, "test.php", &mut NoFold)
        .into_iter()
        .find(|f| f.line == line && matches!(f.kind, FactKind::Effects { .. }))
        .unwrap_or_else(|| panic!("no effect fact on line {line}"))
        .body()
}

// ---------------------------------------------------------------------------
// eval
// ---------------------------------------------------------------------------

#[test]
fn a_pure_function_that_evals_exceeds_its_envelope() {
    let src = "<?php\n/** @pure */\nfunction nondet(string $s): mixed\n{\n    return eval($s);\n}\n";
    let d = one(src);
    assert_eq!(d.message, "eval has effect eval, but nondet() is declared @phpstan-pure");
    assert_eq!((d.line, d.column), (5, 12), "anchored at the construct");
}

#[test]
fn a_literal_payload_is_no_exception() {
    // The owner's example: a payload whose every arm is a pure literal is still
    // `eval`, because the same thing is expressible without it (`$c ? 1 : 2`).
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(bool $c): int { return eval($c ? 'return 1;' : 'return 2;'); }\n";
    let d = one(src);
    assert_eq!(d.message, "eval has effect eval, but f() is declared #[\\Steins\\Pure]");
}

#[test]
fn an_eval_summary_carries_the_label_and_the_marker() {
    let src = "<?php\nfunction f(string $s): mixed { return eval($s); }\n";
    let s = summary(src, "f");
    assert_eq!(s.labels, ["eval"]);
    assert!(!s.exhaustive, "the payload is unseen, so the body is `…?`");
    assert_eq!(margin(src, 2), "effects: {eval, …?}");
}

#[test]
fn an_eval_envelope_admits_eval() {
    silent("<?php\n#[\\Steins\\Effect('eval')]\nfunction f(string $s): mixed { return eval($s); }\n");
    silent("<?php\n/** @phpstan-impure eval */\nfunction f(string $s): mixed { return eval($s); }\n");
    assert!(
        of_id("<?php\n#[\\Steins\\Effect('eval')]\nfunction f(): void {}\n", UNKNOWN_LABEL_ID)
            .is_empty(),
        "`eval` is a registry label"
    );
}

#[test]
fn eval_is_not_under_io() {
    // A root beside `ffi`: an `io` envelope does not admit running code.
    let src = "<?php\n#[\\Steins\\Effect('io')]\nfunction f(string $s): mixed { return eval($s); }\n";
    let d = one(src);
    assert!(d.message.starts_with("eval has effect eval"), "{}", d.message);
}

#[test]
fn an_eval_reaches_a_pure_caller_transitively() {
    let src = "<?php\nfunction run(string $s): mixed { return eval($s); }\n\
               #[\\Steins\\Pure]\nfunction f(): mixed { return run('return 1;'); }\n";
    let d = one(src);
    assert!(d.message.contains("eval"), "{}", d.message);
    assert_eq!(d.line, 4, "reported at the call in the pure caller");
}

#[test]
fn a_closures_eval_is_the_closures_own() {
    // Defining a closure that evals runs nothing; calling it does.
    let defined = "<?php\n#[\\Steins\\Pure]\nfunction f(string $s): \\Closure { return function () use ($s) { return eval($s); }; }\n";
    silent(defined);
    let s = summary(defined, "f");
    assert!(s.labels.is_empty() && s.exhaustive, "the enclosing body is untouched: {s:?}");

    let called = "<?php\n#[\\Steins\\Pure]\nfunction f(string $s): mixed { $g = function () use ($s) { return eval($s); }; return $g(); }\n";
    let d = one(called);
    assert!(d.message.contains("has effect eval"), "{}", d.message);
    assert!(!summary(called, "f").exhaustive, "the call carries the closure's `…?` too");
}

// ---------------------------------------------------------------------------
// include / require
// ---------------------------------------------------------------------------

#[test]
fn a_pure_function_that_includes_exceeds_its_envelope() {
    let src = "<?php\n/** @pure */\nfunction inc(string $f): mixed\n{\n    return include $f;\n}\n";
    let d = one(src);
    assert_eq!(d.message, "include has effect io.fs.read, but inc() is declared @phpstan-pure");
    assert_eq!((d.line, d.column), (5, 12));
}

#[test]
fn all_four_inclusions_read_a_file_and_name_their_own_keyword() {
    for kw in ["include", "include_once", "require", "require_once"] {
        let src = format!("<?php\n#[\\Steins\\Pure]\nfunction f(string $p): mixed {{ return {kw} $p; }}\n");
        let d = one(&src);
        assert_eq!(
            d.message,
            format!("{kw} has effect io.fs.read, but f() is declared #[\\Steins\\Pure]")
        );
        let s = summary(&src, "f");
        assert_eq!(s.labels, ["io.fs.read"], "{kw}");
        assert!(!s.exhaustive, "{kw}: the included file's code is unseen");
    }
}

#[test]
fn an_include_summary_renders_the_marker() {
    let src = "<?php\nfunction f(string $p): mixed { return require_once $p; }\n";
    assert_eq!(margin(src, 2), "effects: {io.fs.read, …?}");
}

#[test]
fn a_literal_path_is_still_a_read() {
    // Whatever the file holds, the construct reads it; a proven path changes
    // what the dam says, not what the effect lane says.
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(): mixed { return require __DIR__ . '/config.php'; }\n";
    assert!(one(src).message.starts_with("require has effect io.fs.read"));
}

#[test]
fn a_read_envelope_admits_an_include() {
    for bound in ["io.fs.read", "io.fs", "io"] {
        silent(&format!(
            "<?php\n#[\\Steins\\Effect('{bound}')]\nfunction f(string $p): mixed {{ return include $p; }}\n"
        ));
    }
    // …and a write-only one does not.
    let d = one("<?php\n#[\\Steins\\Effect('io.fs.write')]\nfunction f(string $p): mixed { return include $p; }\n");
    assert!(d.message.contains("io.fs.read exceeds the envelope"), "{}", d.message);
}

// ---------------------------------------------------------------------------
// What must stay silent
// ---------------------------------------------------------------------------

#[test]
fn a_bare_eval_or_include_statement_is_never_a_dead_statement() {
    let src = "<?php\nfunction f(string $s, string $p): void { eval($s); include $p; require_once 'x.php'; }\n";
    let f = of_id(src, STATEMENT_NO_EFFECT_ID);
    assert!(f.is_empty(), "{f:#?}");
}

#[test]
fn an_undeclared_function_is_colored_but_not_judged() {
    let src = "<?php\nfunction f(string $s, string $p): void { eval($s); include $p; }\n";
    silent(src);
    let s = summary(src, "f");
    assert_eq!(s.labels, ["eval", "io.fs.read"]);
    assert!(!s.exhaustive);
}
