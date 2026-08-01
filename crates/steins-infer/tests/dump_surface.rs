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
    assert_eq!(one_type("<?php $x = 5; \\PHPStan\\dumpType($x);\n"), "dumped type: 5");
}

#[test]
fn recognized_through_use_function_import() {
    let src = "<?php\nuse function PHPStan\\dumpType;\n$x = 'abc';\ndumpType($x);\n";
    assert_eq!(one_type(src), "dumped type: 'abc'");
}

#[test]
fn recognition_is_case_insensitive() {
    // PHP function names are case-insensitive; recognition folds case.
    assert_eq!(one_type("<?php $x = 5; \\PHPStan\\DUMPTYPE($x);\n"), "dumped type: 5");
    assert_eq!(one_type("<?php $x = 5; \\phpstan\\DumpType($x);\n"), "dumped type: 5");
}

#[test]
fn recognized_when_the_current_namespace_is_phpstan() {
    // An unqualified `dumpType()` inside `namespace PHPStan;` resolves to
    // `PHPStan\dumpType` — a resolution path reaching the reserved FQN.
    let src = "<?php\nnamespace PHPStan;\nfunction g($v) { $x = 5; dumpType($x); }\n";
    assert_eq!(one_type(src), "dumped type: 5");
}

#[test]
fn userland_definition_does_not_stand_recognition_down() {
    // Definition-insensitive (§5): a userland `PHPStan\dumpType` definition does not
    // suppress recognition — the dump still fires.
    let src = "<?php\nnamespace PHPStan;\nfunction dumpType($v) { return 1; }\n\
               function g() { $x = 5; dumpType($x); }\n";
    assert_eq!(one_type(src), "dumped type: 5");
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
    assert_eq!(one_type("<?php $x = 5; \\PHPStan\\dumpType($x);\n"), "dumped type: 5");
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
fn class_renders_source_cased_and_namespace_qualified() {
    // The rendering-fidelity fix: a class dump renders the source-cased,
    // namespace-qualified FQN (no leading `\`, matching PHPStan) — never the
    // lowercase-normalized last segment. An object holder (heap class) …
    let obj = "<?php\nnamespace App\\Models;\nclass User {}\n\
               function f() { $x = new User(); \\PHPStan\\dumpType($x); }\n";
    assert_eq!(one_type(obj), "dumped type: App\\Models\\User");
    // … and a declared-param contract arm (the enum-param case the harness flagged:
    // `AllowedSubtypesEnum\\Foo` had rendered as `foo`).
    let param = "<?php\nnamespace App\\Models;\nclass User {}\n\
                 function f(User $x) { \\PHPStan\\dumpType($x); }\n";
    assert_eq!(one_type(param), "dumped type: App\\Models\\User");
}

#[test]
fn instanceof_member_carrier_renders_the_narrowed_class() {
    // The N4 `Member{yes:[…]}` dump-selection gap: a var typed as a coarse
    // supertype (contract lane `CallLike`) and narrowed by a live `instanceof`
    // guard carries the tighter class only on the `Member` carrier — no heap
    // object. `best_dump_type` now consults it (single yes-member) between the
    // exact-heap-class and contract arms, so the dump shows the narrowed class,
    // not the declared supertype it would otherwise fall through to.
    let src = "<?php\ninterface CallLike {}\nfinal class Handler implements CallLike {}\n\
               function f(CallLike $x) { if ($x instanceof Handler) { \\PHPStan\\dumpType($x); } }\n";
    assert_eq!(one_type(src), "dumped type: Handler");
}

#[test]
fn instanceof_multi_member_falls_through_to_contract_carrier() {
    // Two positive `instanceof` guards on the same var bind a MULTI-member yes-set
    // (`[a, b]`) — no single faithful class spelling — so the Member arm falls
    // through to the contract carrier, which still holds the declared `CallLike`.
    let src = "<?php\ninterface CallLike {}\ninterface A {}\ninterface B {}\n\
               function f(CallLike $x) { if ($x instanceof A) { if ($x instanceof B) { \\PHPStan\\dumpType($x); } } }\n";
    assert_eq!(one_type(src), "dumped type: CallLike");
}

#[test]
fn unknown_is_honest() {
    // An unbound variable / unresolvable expression yields no fact — honest `unknown`,
    // never a guess.
    assert_eq!(one_type("<?php \\PHPStan\\dumpType($undefined);\n"), "dumped type: unknown");
}

#[test]
fn assert_construct_is_verified_no_marker() {
    // FLIPPED by the 2026-07-25 owner ruling (ADR-0052 amendment "assert() reads as a
    // throw-guard", slice I0): `assert($x === 5)` narrows at the Verified stratum
    // unconditionally — the ruling reads assert() as `if (!$expr) throw` and never
    // consults `zend.assertions`. So the dump carries NO `(asserted)` marker (pre-
    // ruling it printed `5 (asserted)`). The flip IS the record — see the amendment.
    let src = "<?php\nfunction f($x) { assert($x === 5); \\PHPStan\\dumpType($x); }\n";
    assert_eq!(one_type(src), "dumped type: 5");
}

#[test]
fn asserted_tag_stratum_carries_a_marker() {
    // The `(asserted)` marker path is still live for the `@phpstan-assert` TAG family
    // (Asserted — the ruling boundary, item 4): a docblock claim never launders as a
    // proof, so the dump carries the marker. (Moved off `assert()`, now Verified.)
    let src = "<?php\n/** @phpstan-assert null $x */\nfunction claimNull($x): void {}\n\
               function f($x) { claimNull($x); \\PHPStan\\dumpType($x); }\n";
    assert_eq!(one_type(src), "dumped type: null (asserted)");
}

// ---- Multi-arg / zero-arg (ADR-0053 §7) ------------------------------------

#[test]
fn multi_argument_dumps_one_report_per_argument() {
    let src = "<?php $a = 5; $b = 'x'; \\PHPStan\\dumpType($a, $b);\n";
    let ds = dumps(src);
    assert_eq!(ds.len(), 2, "one report per argument: {ds:?}");
    assert_eq!(ds[0].message, "dumped type: 5");
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

// ---- ADR-0062 S1 — the array vocabulary (D4) -------------------------------
//
// A seeded `array{…}` @param no longer renders "no declared contract" (#51
// L1) — the ONE speller (`spell_arms`) now spells the array vocabulary, so
// the declared-side view shows the spelled arm list. Concrete `dumpType`
// arrays spell value-precisely through the same speller's value-side
// counterpart.

#[test]
fn phpdoc_type_spells_a_seeded_optional_shape_instead_of_no_contract() {
    let src = "<?php\n/** @param array{a?: string, b?: string} $data */\n\
               function f($data) { \\PHPStan\\dumpPhpDocType($data); }\n";
    assert_eq!(one_phpdoc(src), "dumped phpdoc type: array{a?: string, b?: string} (asserted)");
}

#[test]
fn phpdoc_type_spells_list_generic() {
    let src = "<?php\n/** @param list<string> $l */\nfunction f($l) { \\PHPStan\\dumpPhpDocType($l); }\n";
    assert_eq!(one_phpdoc(src), "dumped phpdoc type: list<string> (asserted)");
}

#[test]
fn phpdoc_type_spells_map_generic() {
    let src = "<?php\n/** @param array<string, int> $m */\nfunction f($m) { \\PHPStan\\dumpPhpDocType($m); }\n";
    assert_eq!(one_phpdoc(src), "dumped phpdoc type: array<string, int> (asserted)");
}

#[test]
fn phpdoc_type_spells_non_empty_shape() {
    let src = "<?php\n/** @param non-empty-array{a: int} $x */\n\
               function f($x) { \\PHPStan\\dumpPhpDocType($x); }\n";
    assert_eq!(one_phpdoc(src), "dumped phpdoc type: non-empty-array{a: int} (asserted)");
}

#[test]
fn dump_type_spells_a_keyed_concrete_array() {
    let src = "<?php\n$arr = ['a' => 'v'];\n\\PHPStan\\dumpType($arr);\n";
    assert_eq!(one_type(src), "dumped type: array{a: 'v'}");
}

#[test]
fn dump_type_spells_a_sequential_concrete_array_as_a_list() {
    // The D4-native divergence (ADR-0062 §6): a Yes-list value spells `list{…}`,
    // never PHPStan stable's own `array{…}` for the same value.
    let src = "<?php\n$l = ['x', 'y'];\n\\PHPStan\\dumpType($l);\n";
    assert_eq!(one_type(src), "dumped type: list{'x', 'y'}");
}

// ---- Transparency (ADR-0053 §10 §3) ----------------------------------------

#[test]
fn a_dump_reads_facts_and_binds_nothing() {
    // Transparency (§10 §3): `emit_dumps` reads, never binds — "facts before and
    // after the call are identical". A dump site is exempt from the blanket
    // call-argument drop (the ADR-0070 gate's dump-read exception), so a second
    // dump of the same variable answers exactly what the first did, while a
    // genuinely unknown call still invalidates conservatively.
    let ds = dumps("<?php $x = 5; \\PHPStan\\dumpType($x); \\PHPStan\\dumpType($x);\n");
    assert_eq!(ds.len(), 2, "{ds:?}");
    assert_eq!(ds[0].message, "dumped type: 5");
    assert_eq!(ds[1].message, "dumped type: 5", "a dump must not perturb the env");
    // The contrast pin: an unknown `foo($x)` between assignment and dump still
    // drops the fact (§6 keeps the conservative unresolved-call treatment there).
    assert_eq!(one_type("<?php $x = 5; foo($x); \\PHPStan\\dumpType($x);\n"), "dumped type: unknown");
}

#[test]
fn a_second_dump_of_a_docblock_param_keeps_the_contract() {
    // Regression (2026-08-02): the second dump of the same variable inside one
    // function body degraded to `unknown` — the blanket call-argument drop ate
    // the contract lane after the first dump rendered. A dump is a read: every
    // later dump must answer what the first one did.
    let src = "<?php\n/** @param non-empty-string $method */\nfunction c($method) {\n\
               \\PHPStan\\dumpType($method);\n\\PHPStan\\dumpType($method);\n}\n";
    let ds = dumps(src);
    assert_eq!(ds.len(), 2, "{ds:?}");
    assert_eq!(ds[0].message, "dumped type: non-empty-string (asserted)");
    assert_eq!(ds[1].message, "dumped type: non-empty-string (asserted)");
}

#[test]
fn dump_type_then_var_dump_of_one_variable_agree_across_ids() {
    // The cross-id shape of the same regression: `dumpType($v)` then `var_dump($v)`
    // — the D4 report shares the fact source (§2) and must not read a state the
    // D3 site's own statement degraded.
    let src = "<?php $verb = 'POST'; \\PHPStan\\dumpType($verb); var_dump($verb);\n";
    assert_eq!(one_type(src), "dumped type: 'POST'");
    let vd = var_dumps(src);
    assert_eq!(vd.len(), 1, "{vd:?}");
    assert_eq!(vd[0].message, "dumped type: 'POST'");
}

#[test]
fn var_dump_is_a_transparent_read_too() {
    // D4 is the same read surface: two `var_dump`s of one variable agree, and a
    // `var_dump` before a `dumpType` does not degrade the explicit dump either.
    let vd = var_dumps("<?php $x = 5; var_dump($x); var_dump($x);\n");
    assert_eq!(vd.len(), 2, "{vd:?}");
    assert_eq!(vd[0].message, "dumped type: 5");
    assert_eq!(vd[1].message, "dumped type: 5");
    assert_eq!(one_type("<?php $x = 5; var_dump($x); \\PHPStan\\dumpType($x);\n"), "dumped type: 5");
}

#[test]
fn a_second_dump_of_an_object_holder_keeps_the_class() {
    // The exception spans the object gate too (ADR-0070 condition 3): a dump
    // hands the handle nowhere that could mutate the referent, so the heap
    // binding survives and the second dump still renders the exact class.
    let src = "<?php\nclass Foo {}\n$x = new Foo();\n\\PHPStan\\dumpType($x);\n\\PHPStan\\dumpType($x);\n";
    let ds = dumps(src);
    assert_eq!(ds.len(), 2, "{ds:?}");
    assert_eq!(ds[0].message, "dumped type: Foo");
    assert_eq!(ds[1].message, "dumped type: Foo");
}

// ============================================================================
// ADR-0053 D4 — `var_dump` default-on. The six resolution legs of §5.
// ============================================================================

#[test]
fn leg_a_fully_qualified_global_var_dump_dumps() {
    let ds = var_dumps("<?php $x = 5; \\var_dump($x);\n");
    assert_eq!(ds.len(), 1, "{ds:?}");
    assert_eq!(ds[0].message, "dumped type: 5");
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
    assert_eq!(ds[0].message, "dumped type: 5");
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
    assert_eq!(ds[0].message, "dumped type: 5");
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

// ---- Depth-1 property-fetch dump reach (ADR-0052 §7, Gap B) -----------------

#[test]
fn dump_of_heap_bound_prop_renders_the_value() {
    // A written property fact reaches a direct `dumpType($h->p)` (previously unknown).
    let src = "<?php class H { public ?int $p = null; } \
        $h = new H(); $h->p = 7; \\PHPStan\\dumpType($h->p);";
    assert_eq!(one_type(src), "dumped type: 7");
}

#[test]
fn dump_of_promoted_prop_renders_the_value() {
    // A promoted-constructor prop, bound positionally and by name, both reach the dump
    // (the named form is the value-binding side of Gap A).
    let pos = "<?php class Cfg { public function __construct(public int $n) {} } \
        $c = new Cfg(30); \\PHPStan\\dumpType($c->n);";
    assert_eq!(one_type(pos), "dumped type: 30");
    let named = "<?php class Cfg { public function __construct(public int $n) {} } \
        $c = new Cfg(n: 30); \\PHPStan\\dumpType($c->n);";
    assert_eq!(one_type(named), "dumped type: 30");
}

#[test]
fn dump_of_prop_after_escape_is_unknown() {
    // The object escapes to an unknown call, sweeping its non-readonly props; the dump
    // (read through an alias that keeps the binding) honestly renders unknown. Passing
    // `$h` itself would also drop `$h`'s binding, so the alias isolates the sweep.
    let src = "<?php class H { public ?int $p = null; } \
        $h = new H(); $h->p = 7; $a = $h; sink($h); unknownFn(); \\PHPStan\\dumpType($a->p);";
    assert_eq!(one_type(src), "dumped type: unknown");
}

#[test]
fn dump_of_readonly_prop_survives_escape() {
    // A readonly prop is sweep-immune, so its fact still reaches the dump after the
    // same escape shape (read through the surviving alias).
    let src = "<?php class H { public function __construct(public readonly int $p) {} } \
        $h = new H(5); $a = $h; sink($h); unknownFn(); \\PHPStan\\dumpType($a->p);";
    assert_eq!(one_type(src), "dumped type: 5");
}

#[test]
fn dump_of_depth_2_chain_stays_unknown() {
    // Depth stays exactly 1: `$h->p->q` lowers to `Other`, never a prop fetch.
    let src = "<?php class H { public ?int $p = null; } \
        $h = new H(); $h->p = 7; \\PHPStan\\dumpType($h->p->q);";
    assert_eq!(one_type(src), "dumped type: unknown");
}


// ---- Scalar `@param` envelope seeding (ADR-0052 §9, contract-arm completion) ----
//
// A scalar `@param` envelope lowers to a contract-arm lane the introspection surface
// renders: `positive-int`/`int<lo, hi>`/literal/`StrWith` arms now spell (they were
// seeded before, but the shared speller punted on them). The subset discipline (the
// trust order's refine-within) drops an arm the native base cannot cover.

/// The single `debug.phpdoc-type` message body a one-dump source produces.
fn one_phpdoc(src: &str) -> String {
    let ds = dumps(src);
    let pd: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_PHPDOC_TYPE_ID).collect();
    assert_eq!(pd.len(), 1, "expected exactly one debug.phpdoc-type dump, got {ds:?}");
    pd[0].message.clone()
}

#[test]
fn scalar_param_positive_int_seeds_an_asserted_arm() {
    // No native base: the `@param positive-int` envelope alone seeds an Asserted arm,
    // rendered on both surfaces. `dumpType` shows the arm (no value fact exists).
    let pd = "<?php\n/** @param positive-int $n */\nfunction f($n) { \\PHPStan\\dumpPhpDocType($n); }\n";
    assert_eq!(one_phpdoc(pd), "dumped phpdoc type: positive-int (asserted)");
    let ty = "<?php\n/** @param positive-int $n */\nfunction f($n) { \\PHPStan\\dumpType($n); }\n";
    assert_eq!(one_type(ty), "dumped type: positive-int (asserted)");
}

#[test]
fn scalar_param_int_interval_renders() {
    let src = "<?php\n/** @param int<1, 5> $n */\nfunction f($n) { \\PHPStan\\dumpPhpDocType($n); }\n";
    assert_eq!(one_phpdoc(src), "dumped phpdoc type: int<1, 5> (asserted)");
}

#[test]
fn scalar_param_refines_within_a_native_base() {
    // `@param positive-int` on a native `int` refines within it — Asserted (a strict
    // subset, not an exact match), and the native value seed still wins on dumpType.
    let pd = "<?php\n/** @param positive-int $m */\nfunction f(int $m) { \\PHPStan\\dumpPhpDocType($m); }\n";
    assert_eq!(one_phpdoc(pd), "dumped phpdoc type: positive-int (asserted)");
    let ty = "<?php\n/** @param positive-int $m */\nfunction f(int $m) { \\PHPStan\\dumpType($m); }\n";
    assert_eq!(one_type(ty), "dumped type: int");
}

#[test]
fn scalar_param_contradicting_the_native_type_seeds_nothing() {
    // Subset discipline: `@param string` on `int $x` is a contradiction — the docblock
    // never widens past the runtime-enforced native type, so it seeds NO arm. The
    // native `int` value seed still flows to dumpType.
    let pd = "<?php\n/** @param string $x */\nfunction f(int $x) { \\PHPStan\\dumpPhpDocType($x); }\n";
    assert_eq!(one_phpdoc(pd), "dumped phpdoc type: no declared contract");
    let ty = "<?php\n/** @param string $x */\nfunction f(int $x) { \\PHPStan\\dumpType($x); }\n";
    assert_eq!(one_type(ty), "dumped type: int");
}

// ---- Declared-return call-site seeding (ADR-0052 §9, the return direction) ------

#[test]
fn declared_object_return_makes_the_value_visible() {
    // A `: Foo` native return seeds a Verified Instance-MEMBERSHIP arm at the call
    // site — the object, previously invisible, now dumps as `Foo` (no exactness: no
    // `(asserted)` since Verified, but membership, not an exact-class fact).
    let src = "<?php\nclass Foo {}\nfunction createFoo(int $n): Foo { return new Foo(); }\n\
               function g() { $foo = createFoo(123); \\PHPStan\\dumpType($foo); }\n";
    assert_eq!(one_type(src), "dumped type: Foo");
}

#[test]
fn declared_union_return_narrows_under_instanceof() {
    // A `: User|Guest` return seeds both membership arms; an `instanceof User` guard
    // subtracts `User` on the else-branch (N4 narrowing over the return arms), leaving
    // exactly `Guest` — the assertions_instanceof_narrowing shape at a CALL SITE.
    let src = "<?php\nclass User {}\nclass Guest {}\n\
               function who(): User|Guest { return new User(); }\n\
               function g() { $u = who(); if ($u instanceof User) {} else { \\PHPStan\\dumpType($u); } }\n";
    assert_eq!(one_type(src), "dumped type: Guest");
}

#[test]
fn declared_return_phpdoc_refines_asserted() {
    // `@return positive-int` refines within the native `int` return — an Asserted arm
    // seeded at the call site (the body here is not foldable, so no value fact beats
    // the arm floor).
    let src = "<?php\n/** @return positive-int */\nfunction mk(int $s): int { return $s + 1; }\n\
               function g() { $x = mk(5); \\PHPStan\\dumpPhpDocType($x); }\n";
    assert_eq!(one_phpdoc(src), "dumped phpdoc type: positive-int (asserted)");
}

#[test]
fn a_folded_return_value_beats_the_return_arm() {
    // Precedence (ADR-0052 §9): a proven value fact is the floor's ceiling. A trivially
    // foldable `: int` return resolves to the literal `1`, which wins over the `int`
    // membership arm on dumpType.
    let src = "<?php\nfunction mkInt(): int { return 1; }\n\
               function g() { $x = mkInt(); \\PHPStan\\dumpType($x); }\n";
    assert_eq!(one_type(src), "dumped type: 1");
}
