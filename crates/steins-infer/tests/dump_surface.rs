//! ADR-0053 D3 — the explicit dump pair (`PHPStan\dumpType` / `PHPStan\dumpPhpDocType`).
//!
//! A recognized call emits `debug.type` / `debug.phpdoc-type` carrying the engine's
//! best fact for the argument, rendered through the ONE shared speller (the D2
//! extraction). Recognition is by **resolved FQN**, definition-insensitive and
//! case-insensitive (ADR-0053 §5). The rendered fact is pinned; the message frame
//! wording is not (§7). `var_dump` (D4) has its own test module.

use steins_infer::{DEBUG_PHPDOC_TYPE_ID, DEBUG_TYPE_ID, DEBUG_VAR_DUMP_ID, Diagnostic, check};
use steins_syntax::SourceTree;

/// The dump diagnostics a source file produces (both explicit ids).
fn dumps(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check(&tree, &[], "t.php")
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID || d.id == DEBUG_PHPDOC_TYPE_ID)
        .collect()
}

/// The `debug.var-dump` diagnostics a source file produces (ADR-0053 D4).
fn var_dumps(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check(&tree, &[], "t.php").into_iter().filter(|d| d.id == DEBUG_VAR_DUMP_ID).collect()
}

/// The single `debug.type` message body a one-dump source produces.
fn one_type(src: &str) -> String {
    let ds = dumps(src);
    let ty: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ty.len(), 1, "expected exactly one debug.type dump, got {ds:?}");
    ty[0].message.clone()
}

// ---- Recognition matrix (ADR-0053 §5) --------------------------------------

#[test]
fn recognized_by_fully_qualified_fqn() {
    assert_eq!(one_type("<?php $x = 5; \\PHPStan\\dumpType($x);\n"), "dumped type: int");
}

#[test]
fn recognized_through_use_function_import() {
    let src = "<?php\nuse function PHPStan\\dumpType;\n$x = 'abc';\ndumpType($x);\n";
    assert_eq!(one_type(src), "dumped type: 'abc'");
}

#[test]
fn recognition_is_case_insensitive() {
    // PHP function names are case-insensitive; recognition folds case.
    assert_eq!(one_type("<?php $x = 5; \\PHPStan\\DUMPTYPE($x);\n"), "dumped type: int");
    assert_eq!(one_type("<?php $x = 5; \\phpstan\\DumpType($x);\n"), "dumped type: int");
}

#[test]
fn recognized_when_the_current_namespace_is_phpstan() {
    // An unqualified `dumpType()` inside `namespace PHPStan;` resolves to
    // `PHPStan\dumpType` — a resolution path reaching the reserved FQN.
    let src = "<?php\nnamespace PHPStan;\nfunction g($v) { $x = 5; dumpType($x); }\n";
    assert_eq!(one_type(src), "dumped type: int");
}

#[test]
fn userland_definition_does_not_stand_recognition_down() {
    // Definition-insensitive (§5): a userland `PHPStan\dumpType` definition does not
    // suppress recognition — the dump still fires.
    let src = "<?php\nnamespace PHPStan;\nfunction dumpType($v) { return 1; }\n\
               function g() { $x = 5; dumpType($x); }\n";
    assert_eq!(one_type(src), "dumped type: int");
}

#[test]
fn a_different_namespace_homonym_is_not_recognized() {
    // `Foo\dumpType` (qualified) resolves elsewhere — never the reserved pair.
    assert!(dumps("<?php $x = 5; \\Foo\\dumpType($x);\n").is_empty());
    // A bare unqualified `dumpType()` in the global namespace resolves to the global
    // `dumpType`, not `PHPStan\dumpType`.
    assert!(dumps("<?php $x = 5; dumpType($x);\n").is_empty());
}

// ---- Fact layers (ADR-0053 §2 / §7) ----------------------------------------

#[test]
fn singleton_value_fact() {
    assert_eq!(one_type("<?php $x = 5; \\PHPStan\\dumpType($x);\n"), "dumped type: int");
    assert_eq!(one_type("<?php $x = 'GET'; \\PHPStan\\dumpType($x);\n"), "dumped type: 'GET'");
}

#[test]
fn oneof_value_fact_renders_a_literal_union() {
    // A `$c ? 'GET' : 'POST'` over an undecided bool guard is a OneOf of two literals.
    let src = "<?php\nfunction f(bool $c) { $x = $c ? 'GET' : 'POST'; \\PHPStan\\dumpType($x); }\n";
    assert_eq!(one_type(src), "dumped type: 'GET'|'POST'");
}

#[test]
fn general_value_fact_from_a_native_param() {
    // A native-typed param seeds the General layer (its runtime-enforced base fact).
    let src = "<?php\nfunction f(int $x) { \\PHPStan\\dumpType($x); }\n";
    assert_eq!(one_type(src), "dumped type: int");
    let nullable = "<?php\nfunction f(?string $x) { \\PHPStan\\dumpType($x); }\n";
    assert_eq!(one_type(nullable), "dumped type: string|null");
}

#[test]
fn exact_class_of_an_object_holder() {
    let src = "<?php\nclass Foo {}\n$x = new Foo();\n\\PHPStan\\dumpType($x);\n";
    assert_eq!(one_type(src), "dumped type: Foo");
}

#[test]
fn unknown_is_honest() {
    // An unbound variable / unresolvable expression yields no fact — honest `unknown`,
    // never a guess.
    assert_eq!(one_type("<?php \\PHPStan\\dumpType($undefined);\n"), "dumped type: unknown");
}

#[test]
fn asserted_stratum_carries_a_marker() {
    // An `assert($x === 5)` narrowing is Asserted (assertions off by default), so the
    // dump prints the marker — a docblock/assert claim never launders as a proof.
    let src = "<?php\nfunction f($x) { assert($x === 5); \\PHPStan\\dumpType($x); }\n";
    assert_eq!(one_type(src), "dumped type: int (asserted)");
}

// ---- Multi-arg / zero-arg (ADR-0053 §7) ------------------------------------

#[test]
fn multi_argument_dumps_one_report_per_argument() {
    let src = "<?php $a = 5; $b = 'x'; \\PHPStan\\dumpType($a, $b);\n";
    let ds = dumps(src);
    assert_eq!(ds.len(), 2, "one report per argument: {ds:?}");
    assert_eq!(ds[0].message, "dumped type: int");
    assert_eq!(ds[1].message, "dumped type: 'x'");
    // Argument order → column order.
    assert!(ds[0].column < ds[1].column);
}

#[test]
fn zero_argument_dump_still_reports_fail_level() {
    let src = "<?php \\PHPStan\\dumpType();\n";
    let ds = dumps(src);
    assert_eq!(ds.len(), 1);
    assert_eq!(ds[0].id, DEBUG_TYPE_ID);
    assert!(ds[0].message.contains("no argument"), "{}", ds[0].message);
}

#[test]
fn first_class_callable_is_not_a_dumping_call() {
    // `dumpType(...)` creates a Closure — no argument expression to dump (§5 leg f).
    assert!(dumps("<?php $f = \\PHPStan\\dumpType(...);\n").is_empty());
}

// ---- dumpPhpDocType — the declared-side view (ADR-0053 §2) ------------------

#[test]
fn phpdoc_type_renders_the_declared_arm_list() {
    // A native union type is the declared envelope, seeded Verified — no marker.
    let src = "<?php\nfunction f(int|string $x) { \\PHPStan\\dumpPhpDocType($x); }\n";
    let ds = dumps(src);
    let pd: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_PHPDOC_TYPE_ID).collect();
    assert_eq!(pd.len(), 1, "{ds:?}");
    assert_eq!(pd[0].message, "dumped phpdoc type: int|string");
}

#[test]
fn phpdoc_type_marks_a_pure_docblock_declaration_asserted() {
    // A `@param` refinement with no matching native type is a docblock claim
    // (Asserted stratum, ADR-0052 §5) — the dump carries the marker so the
    // introspection surface never launders a claim as a proof.
    let src = "<?php\n/** @param int|string $x */\nfunction f($x) { \\PHPStan\\dumpPhpDocType($x); }\n";
    let ds = dumps(src);
    let pd: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_PHPDOC_TYPE_ID).collect();
    assert_eq!(pd.len(), 1, "{ds:?}");
    assert_eq!(pd[0].message, "dumped phpdoc type: int|string (asserted)");
}

#[test]
fn phpdoc_type_is_honest_when_no_contract_is_declared() {
    let src = "<?php\nfunction f($x) { \\PHPStan\\dumpPhpDocType($x); }\n";
    let ds = dumps(src);
    let pd: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_PHPDOC_TYPE_ID).collect();
    assert_eq!(pd.len(), 1, "{ds:?}");
    assert_eq!(pd[0].message, "dumped phpdoc type: no declared contract");
}

// ---- Transparency (ADR-0053 §10 §3) ----------------------------------------

#[test]
fn a_dump_reads_facts_and_binds_nothing() {
    // Transparency (§10 §3): `emit_dumps` reads, never binds. A recognized dump
    // perturbs the env EXACTLY as an equivalent unresolved call does (§6 keeps the
    // conservative unresolved-call treatment) — no more (it adds no binding), no
    // less. So after either an unknown `foo($x)` or a `dumpType($x)`, a following
    // dump reads the identical (conservatively invalidated) state.
    let after_unknown = one_type("<?php $x = 5; foo($x); \\PHPStan\\dumpType($x);\n");
    let after_dump = {
        let ds = dumps("<?php $x = 5; \\PHPStan\\dumpType($x); \\PHPStan\\dumpType($x);\n");
        ds.last().expect("two dumps").message.clone()
    };
    assert_eq!(after_unknown, after_dump, "a dump perturbs the env exactly as any unknown call");
    assert_eq!(after_unknown, "dumped type: unknown");
}

// ============================================================================
// ADR-0053 D4 — `var_dump` default-on. The six resolution legs of §5.
// ============================================================================

#[test]
fn leg_a_fully_qualified_global_var_dump_dumps() {
    let ds = var_dumps("<?php $x = 5; \\var_dump($x);\n");
    assert_eq!(ds.len(), 1, "{ds:?}");
    assert_eq!(ds[0].message, "dumped type: int");
}

#[test]
fn leg_b_unqualified_root_namespace_dumps() {
    let ds = var_dumps("<?php $x = 'GET'; var_dump($x);\n");
    assert_eq!(ds.len(), 1, "{ds:?}");
    assert_eq!(ds[0].message, "dumped type: 'GET'");
}

#[test]
fn leg_c_namespaced_falls_back_to_global_when_provably_undefined() {
    // A clean universe (dam clear) with no same-namespace homonym: the runtime falls
    // back to the global var_dump, so the dump fires.
    let src = "<?php\nnamespace App;\nfunction g(int $v) { var_dump($v); }\n";
    let ds = var_dumps(src);
    assert_eq!(ds.len(), 1, "{ds:?}");
    assert_eq!(ds[0].message, "dumped type: int");
}

#[test]
fn leg_c_namespaced_homonym_is_silent() {
    // A same-namespace `App\var_dump` shadows the global — the call resolves to it,
    // never the global, so NO dump (silence is the free safe side).
    let src = "<?php\nnamespace App;\nfunction var_dump($x) {}\n\
               function g(int $v) { var_dump($v); }\n";
    assert!(var_dumps(src).is_empty(), "a namespaced homonym stands the dump down");
}

#[test]
fn leg_c_dam_leaves_existence_unknown_and_is_silent() {
    // A dam site (eval) means dynamic code could mint `App\var_dump` at runtime, so
    // its existence is Unknown — the call might not fall back to global. No dump.
    let src = "<?php\nnamespace App;\nfunction g(int $v) { eval('return 1;'); var_dump($v); }\n";
    assert!(var_dumps(src).is_empty(), "dam-Unknown existence stands the dump down");
}

#[test]
fn leg_d_qualified_var_dump_resolves_elsewhere() {
    assert!(var_dumps("<?php $x = 5; \\App\\var_dump($x);\n").is_empty());
    assert!(var_dumps("<?php\nnamespace N;\n$x = 5;\nApp\\var_dump($x);\n").is_empty());
}

#[test]
fn leg_d_use_function_import_of_a_namespaced_var_dump_is_silent() {
    // `use function App\var_dump;` resolves the name to `App\var_dump`, never global.
    let src = "<?php\nuse function App\\var_dump;\n$x = 5;\nvar_dump($x);\n";
    assert!(var_dumps(src).is_empty());
}

#[test]
fn leg_d_use_function_import_of_the_global_still_dumps() {
    // `use function var_dump;` explicitly imports the global — still the trigger.
    let src = "<?php\nnamespace App;\nuse function var_dump;\n$x = 5;\nvar_dump($x);\n";
    let ds = var_dumps(src);
    assert_eq!(ds.len(), 1, "{ds:?}");
    assert_eq!(ds[0].message, "dumped type: int");
}

#[test]
fn leg_e_a_method_named_var_dump_is_never_a_dump() {
    let src = "<?php\nclass C { function m() { $this->var_dump(5); } }\n";
    assert!(var_dumps(src).is_empty(), "a method var_dump is a different symbol space");
}

#[test]
fn leg_f_first_class_callable_and_string_callable_are_silent() {
    // First-class callable: no argument expression to dump.
    assert!(var_dumps("<?php $f = var_dump(...);\n").is_empty());
    // String callable: the call is to array_map, not var_dump.
    assert!(var_dumps("<?php $a = [1]; array_map('var_dump', $a);\n").is_empty());
}

#[test]
fn var_dump_multi_argument_dumps_one_report_per_argument() {
    let src = "<?php $a = 5; $b = 'x'; var_dump($a, $b);\n";
    let ds = var_dumps(src);
    assert_eq!(ds.len(), 2, "one report per argument: {ds:?}");
    assert_eq!(ds[0].message, "dumped type: int");
    assert_eq!(ds[1].message, "dumped type: 'x'");
    assert!(ds[0].column < ds[1].column, "argument order → column order");
}

#[test]
fn zero_argument_var_dump_dumps_nothing() {
    // Arity is S5's business, not a dump (§2): a bare `var_dump()` emits nothing.
    assert!(var_dumps("<?php var_dump();\n").is_empty());
}

#[test]
fn var_dump_shares_the_type_rendering() {
    // Same fact source and rendering as the explicit `debug.type` (§2): an object
    // holder renders its exact class; an unknown is honest.
    let obj = var_dumps("<?php\nclass Foo {}\n$x = new Foo();\nvar_dump($x);\n");
    assert_eq!(obj[0].message, "dumped type: Foo");
    let unknown = var_dumps("<?php var_dump($undefined);\n");
    assert_eq!(unknown[0].message, "dumped type: unknown");
}
