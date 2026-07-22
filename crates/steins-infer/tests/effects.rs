//! Acceptance tests for the effect-envelope check (`effect.envelope-exceeded`,
//! ADR-0005): a function declared `#[\Steins\Pure]` whose inferred effects
//! exceed the empty envelope. Proven violations only — unknown effects stay
//! silent (the deferred "cannot-verify" maybe-diagnostic).

use steins_infer::{Diagnostic, EFFECT_ID, check};
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

// ---- Direct builtin effect at a Pure call site ---------------------------

#[test]
fn pure_calling_rand_is_flagged_with_exact_message() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction withRng(): int { return rand(); }\n";
    let d = one(src);
    assert_eq!(d.id, EFFECT_ID);
    assert_eq!(
        d.message,
        "rand() has effect nondet.random, but withRng() is declared #[\\Steins\\Pure]"
    );
    // Points at the `rand` call (line 3).
    assert_eq!(d.line, 3);
}

#[test]
fn pure_builtin_and_arithmetic_are_silent() {
    // strtolower is catalogued-pure; arithmetic has no effect.
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(string $s): string { $x = 1 + 2; return strtolower($s); }\n";
    assert_eq!(effects(src).len(), 0, "pure builtin + arithmetic → silent");
}

#[test]
fn pure_calling_uncatalogued_builtin_is_silent() {
    // Unknown builtin widens to unknown-effect → deferred maybe → silent.
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(): void { some_unknown_fn(); }\n";
    assert_eq!(effects(src).len(), 0, "uncatalogued builtin → silent (deferred)");
}

// ---- echo (CST-scan case: nested in control flow) ------------------------

#[test]
fn echo_inside_if_inside_pure_is_flagged() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(bool $c): void { if ($c) { echo \"hi\"; } }\n";
    let d = one(src);
    assert_eq!(d.message, "echo has effect output, but f() is declared #[\\Steins\\Pure]");
}

// ---- exit (ADR-0019 rule 4) ----------------------------------------------

#[test]
fn exit_inside_pure_is_flagged() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(): void { exit(1); }\n";
    let d = one(src);
    assert_eq!(d.message, "exit has effect exit, but f() is declared #[\\Steins\\Pure]");
}

// ---- throw is permitted by Pure (ADR-0006) -------------------------------

#[test]
fn throw_inside_pure_is_silent() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(): void { throw new \\RuntimeException(\"x\"); }\n";
    assert_eq!(effects(src).len(), 0, "Pure permits throw");
}

// ---- Transitive: pure → helper → file_put_contents, with via-provenance --

#[test]
fn transitive_effect_reports_via_origin() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(): void { helper(); }\nfunction helper(): void { file_put_contents(\"/tmp/x\", \"y\"); }\n";
    let d = one(src);
    assert_eq!(
        d.message,
        "helper() has effect io.fs.write (via file_put_contents at line 4), but f() is declared #[\\Steins\\Pure]"
    );
    // Reported at the outer `helper()` call site (line 3).
    assert_eq!(d.line, 3);
}

#[test]
fn transitive_through_two_hops() {
    // f → g → h → file_put_contents; the via still names the ultimate origin.
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(): void { g(); }\nfunction g(): void { h(); }\nfunction h(): void { file_put_contents(\"/tmp/x\", \"y\"); }\n";
    let d = one(src);
    assert!(
        d.message.contains("g() has effect io.fs.write (via file_put_contents at line 5)"),
        "got: {}",
        d.message
    );
}

#[test]
fn transitive_pure_helper_is_silent() {
    // helper only calls a pure builtin → no effect propagates.
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(string $s): void { helper($s); }\nfunction helper(string $s): string { return strtolower($s); }\n";
    assert_eq!(effects(src).len(), 0, "pure helper → silent");
}

// ---- Recursion must not hang ---------------------------------------------

#[test]
fn mutual_recursion_terminates() {
    // a ⇄ b with no real effects: converges, reports nothing, does not loop.
    let src = "<?php\n#[\\Steins\\Pure]\nfunction a(): void { b(); }\nfunction b(): void { a(); }\n";
    assert_eq!(effects(src).len(), 0);
    // And with a real effect reachable through the cycle it is still found.
    let effectful = "<?php\n#[\\Steins\\Pure]\nfunction a(): void { b(); }\nfunction b(): void { a(); rand(); }\n";
    let f = effects(effectful);
    assert!(
        f.iter().any(|d| d.message.contains("b() has effect nondet.random")),
        "effect through the cycle is found: {f:#?}"
    );
}

// ---- Attribute recognition guards (end-to-end through the check) ---------

#[test]
fn bare_pure_without_use_is_not_checked() {
    // JetBrains collision guard: #[Pure] without `use Steins\Pure` → not an
    // envelope → the rand() call is not a violation.
    let src = "<?php\n#[Pure]\nfunction f(): int { return rand(); }\n";
    assert_eq!(effects(src).len(), 0, "#[Pure] without use is not the Steins envelope");
}

#[test]
fn bare_pure_with_use_is_checked() {
    let src = "<?php\nuse Steins\\Pure;\n#[Pure]\nfunction f(): int { return rand(); }\n";
    let d = one(src);
    assert!(d.message.contains("rand() has effect nondet.random"), "got: {}", d.message);
}

#[test]
fn jetbrains_qualified_pure_is_not_checked() {
    let src = "<?php\n#[JetBrains\\PhpStorm\\Pure]\nfunction f(): int { return rand(); }\n";
    assert_eq!(effects(src).len(), 0, "#[JetBrains\\PhpStorm\\Pure] is not the Steins envelope");
}

// ---- Coexistence with type.argument-mismatch -----------------------------

#[test]
fn effect_and_type_findings_coexist() {
    // A Pure function that both calls rand() (effect) and is called with a bad
    // literal (type mismatch) yields both diagnostics.
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(int $w): int { return rand() + $w; }\nf(\"abc\");\n";
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let all = check(&tree, &functions, "test.php");
    assert!(
        all.iter().any(|d| d.id == "type.argument-mismatch"),
        "type finding present: {all:#?}"
    );
    assert!(all.iter().any(|d| d.id == EFFECT_ID), "effect finding present: {all:#?}");
}

// ---- Non-Pure functions are never effect-checked -------------------------

#[test]
fn unannotated_function_with_effects_is_silent() {
    let src = "<?php\nfunction f(): int { echo \"hi\"; return rand(); }\n";
    assert_eq!(effects(src).len(), 0, "no envelope → no effect check");
}
