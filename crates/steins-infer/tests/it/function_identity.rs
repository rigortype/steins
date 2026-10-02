//! A free function is identified by its FQN, not its simple name (issue #925): a file of
//! several namespaces may declare `One\f` and `Two\f`, and every lookup of "the declaration
//! this scope belongs to" used to answer the first one in the file for both.
//!
//! One section per lookup that read the simple name — the native return check, the native
//! parameter seed, the docblock `@param` seed, the docblock `@return` check, the
//! never-returning veto and the binding descent — each in the braced and the unbraced
//! spelling, since the namespace of a function comes from two different syntax shapes.
//! Then the controls: a real mismatch in the *second* namespace still reports, so the fix
//! is not a silencing.

use steins_infer::{DEBUG_TYPE_ID, Diagnostic, RETURN_ID, RETURN_MISMATCH_ID, check};
use steins_syntax::SourceTree;

fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php")
        .into_iter()
        .filter(|d| !d.id.starts_with("untyped."))
        .collect()
}

/// Every finding but the dumps, rendered `line:id`.
fn reports(src: &str) -> Vec<String> {
    findings(src)
        .into_iter()
        .filter(|d| d.id != DEBUG_TYPE_ID)
        .map(|d| format!("{}:{}", d.line, d.id))
        .collect()
}

/// The single dump a fixture asks for.
fn dumped(src: &str) -> String {
    let ds: Vec<Diagnostic> = findings(src).into_iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ds.len(), 1, "expected exactly one dump, got {ds:?}");
    ds[0].message.clone()
}

/// Two namespaces of one file, spelled both ways: the body of each is placed by `wrap`.
fn both_spellings(one: &str, two: &str) -> [String; 2] {
    [
        format!("<?php\nnamespace One {{\n{one}\n}}\nnamespace Two {{\n{two}\n}}\n"),
        format!("<?php\nnamespace One;\n{one}\nnamespace Two;\n{two}\n"),
    ]
}

// the native return check

#[test]
fn class_return_types_are_checked_against_their_own_namespace() {
    // The issue's witness: `Two\make(): A` returns a `Two\A`, not `One\A`.
    let [braced, unbraced] = both_spellings(
        "class A {}\nfunction make(): A { return new A(); }",
        "class A {}\nfunction make(): A { return new A(); }",
    );
    assert_eq!(reports(&braced), Vec::<String>::new());
    assert_eq!(reports(&unbraced), Vec::<String>::new());
}

#[test]
fn scalar_return_types_are_checked_against_their_own_namespace() {
    let [braced, unbraced] = both_spellings(
        "function foo(): int { return 1; }",
        "function foo(): string { return \"x\"; }",
    );
    assert_eq!(reports(&braced), Vec::<String>::new());
    assert_eq!(reports(&unbraced), Vec::<String>::new());
}

#[test]
fn a_real_mismatch_in_the_second_namespace_still_reports() {
    // `Two\foo()` is declared `: int` and returns `"x"`: the finding stays — and it is
    // judged against `int` (the second declaration). Against `One\foo(): string` it would
    // have passed silently, a missed finding the same lookup caused.
    let [braced, unbraced] = both_spellings(
        "function foo(): string { return \"y\"; }",
        "function foo(): int { return \"x\"; }",
    );
    for src in [braced, unbraced] {
        let ds = findings(&src);
        let mismatches: Vec<&Diagnostic> =
            ds.iter().filter(|d| d.id == RETURN_MISMATCH_ID || d.id == RETURN_ID).collect();
        assert_eq!(mismatches.len(), 1, "{ds:?}");
        assert!(mismatches[0].message.contains("int"), "{}", mismatches[0].message);
        assert!(mismatches[0].message.contains("\"x\""), "{}", mismatches[0].message);
    }
}

#[test]
fn a_real_mismatch_in_the_first_namespace_reports_once() {
    // The mirror image: the first declaration is the wrong one, the second is honest. The
    // old lookup read the first for both, so it reported twice.
    let [braced, unbraced] = both_spellings(
        "function foo(): int { return \"x\"; }",
        "function foo(): string { return \"y\"; }",
    );
    for src in [braced, unbraced] {
        let ds = findings(&src);
        let mismatches: Vec<&Diagnostic> =
            ds.iter().filter(|d| d.id == RETURN_MISMATCH_ID || d.id == RETURN_ID).collect();
        assert_eq!(mismatches.len(), 1, "{ds:?}");
        assert_eq!(mismatches[0].line, 3, "the first namespace's function is the wrong one");
    }
}

// the native parameter seed

#[test]
fn native_parameter_seed_is_the_functions_own() {
    // Without a docblock the second `foo`'s `$s` seeded from nothing at all (`unknown`):
    // its own declaration was never found.
    let [braced, unbraced] = both_spellings(
        "function foo(int $s): void {}",
        "function foo(string $s): void { \\PHPStan\\dumpType($s); }",
    );
    assert_eq!(dumped(&braced), "dumped type: string");
    assert_eq!(dumped(&unbraced), "dumped type: string");
}

// the docblock parameter seed

#[test]
fn docblock_parameter_seed_is_the_functions_own() {
    // The second namespace's `@param` narrows its untyped `$s`; the dump was the first
    // namespace's `1|2|3`.
    let [braced, unbraced] = both_spellings(
        "/** @param 1|2|3 $s */\nfunction foo($s): void {}",
        "/** @param \"foo\"|\"bar\" $s */\nfunction foo($s): void { \\PHPStan\\dumpType($s); }",
    );
    assert_eq!(dumped(&braced), "dumped type: 'bar'|'foo' (asserted)");
    assert_eq!(dumped(&unbraced), "dumped type: 'bar'|'foo' (asserted)");
}

#[test]
fn docblock_parameter_seed_under_a_native_hint_is_the_functions_own() {
    // The issue's `param.php`: the second `foo` is natively `string`, and that is what its
    // body sees. It dumped the first namespace's `@param` (`1|2|3`).
    let [braced, unbraced] = both_spellings(
        "/** @param 1|2|3 $s */\nfunction foo($s): void {}",
        "/** @param \"foo\"|\"bar\" $s */\nfunction foo(string $s): void { \\PHPStan\\dumpType($s); }",
    );
    assert_eq!(dumped(&braced), "dumped type: string");
    assert_eq!(dumped(&unbraced), "dumped type: string");
}

#[test]
fn docblock_parameter_seed_reads_the_first_namespace_too() {
    // The first namespace's own seed is not lost to the second.
    let [braced, unbraced] = both_spellings(
        "/** @param 1|2|3 $s */\nfunction foo($s): void { \\PHPStan\\dumpType($s); }",
        "/** @param \"foo\"|\"bar\" $s */\nfunction foo($s): void {}",
    );
    assert_eq!(dumped(&braced), "dumped type: 1|2|3 (asserted)");
    assert_eq!(dumped(&unbraced), "dumped type: 1|2|3 (asserted)");
}

// the docblock return check

#[test]
fn docblock_return_is_checked_against_its_own_namespace() {
    let [braced, unbraced] = both_spellings(
        "/** @return int */\nfunction foo() { return 1; }",
        "/** @return string */\nfunction foo() { return \"x\"; }",
    );
    assert_eq!(reports(&braced), Vec::<String>::new());
    assert_eq!(reports(&unbraced), Vec::<String>::new());
}

// the never-returning veto

#[test]
fn a_never_function_does_not_veto_the_same_named_function_of_another_namespace() {
    // `One\stop()` never returns, so `One\run()` may fall off its end only after it; `Two\stop()`
    // returns, so `Two\run()` provably falls through. The simple name vetoed both.
    let [braced, unbraced] = both_spellings(
        "function stop(): never { exit(1); }\nfunction run(): int { stop(); }",
        "function stop(): int { return 1; }\nfunction run(): int { stop(); }",
    );
    for src in [braced, unbraced] {
        let ds = findings(&src);
        let missing: Vec<&Diagnostic> =
            ds.iter().filter(|d| d.id == steins_infer::TYPE_RETURN_MISSING_ID).collect();
        assert_eq!(missing.len(), 1, "{ds:?}");
        assert!(missing[0].message.contains("function run"), "{}", missing[0].message);
        // The finding is the second namespace's (the file's last `run`).
        assert!(missing[0].line > 6, "{}", missing[0].line);
    }
}

#[test]
fn a_never_function_still_vetoes_its_own_namespace() {
    let [braced, unbraced] = both_spellings(
        "function stop(): never { exit(1); }\nfunction run(): int { stop(); }",
        "function stop(): never { exit(1); }\nfunction run(): int { stop(); }",
    );
    assert_eq!(reports(&braced), Vec::<String>::new());
    assert_eq!(reports(&unbraced), Vec::<String>::new());
}

#[test]
fn a_never_function_of_the_global_namespace_vetoes_an_unqualified_call_that_falls_back_to_it() {
    // `\stop` is `: never`; `namespace Two` declares none of its own, so `stop()` there is the
    // global one — the call resolves by PHP's fallback, and the veto holds.
    let src = "<?php\nnamespace {\nfunction stop(): never { exit(1); }\n}\n\
        namespace Two {\nfunction run(): int { stop(); }\n}\n";
    assert_eq!(reports(src), Vec::<String>::new());
}

// the binding descent

#[test]
fn descent_reads_the_callees_own_body() {
    // Two `pick`s of one simple name: `fn_scope` declined on two scopes, so the call's value
    // was lost. Each call now descends into its own namespace's body.
    let [braced, unbraced] = both_spellings(
        "function pick(int $t): int { return 1; }",
        "function pick(int $t): int { return 2; }\n\
         $x = \\Two\\pick(1);\n\\PHPStan\\dumpType($x);",
    );
    for src in [braced, unbraced] {
        assert_eq!(dumped(&src), "dumped type: 2");
    }
}
