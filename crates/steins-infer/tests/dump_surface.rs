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

// Recognition matrix (ADR-0053 §5)

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
    assert_eq!(one_type("<?php $x = 5; \\PHPStan\\DUMPTYPE($x);\n"), "dumped type: 5");
    assert_eq!(one_type("<?php $x = 5; \\phpstan\\DumpType($x);\n"), "dumped type: 5");
}

#[test]
fn recognized_when_the_current_namespace_is_phpstan() {
    let src = "<?php\nnamespace PHPStan;\nfunction g($v) { $x = 5; dumpType($x); }\n";
    assert_eq!(one_type(src), "dumped type: 5");
}

#[test]
fn userland_definition_does_not_stand_recognition_down() {
    // Definition-insensitive (§5).
    let src = "<?php\nnamespace PHPStan;\nfunction dumpType($v) { return 1; }\n\
               function g() { $x = 5; dumpType($x); }\n";
    assert_eq!(one_type(src), "dumped type: 5");
}

#[test]
fn a_different_namespace_homonym_is_not_recognized() {
    assert!(dumps("<?php $x = 5; \\Foo\\dumpType($x);\n").is_empty());
    assert!(dumps("<?php $x = 5; dumpType($x);\n").is_empty());
}

// Fact layers (ADR-0053 §2 / §7)

#[test]
fn singleton_value_fact() {
    assert_eq!(one_type("<?php $x = 5; \\PHPStan\\dumpType($x);\n"), "dumped type: 5");
    assert_eq!(one_type("<?php $x = 'GET'; \\PHPStan\\dumpType($x);\n"), "dumped type: 'GET'");
}

#[test]
fn oneof_value_fact_renders_a_literal_union() {
    // `$c ? 'GET' : 'POST'` over an undecided bool guard is a OneOf of two literals.
    let src = "<?php\nfunction f(bool $c) { $x = $c ? 'GET' : 'POST'; \\PHPStan\\dumpType($x); }\n";
    assert_eq!(one_type(src), "dumped type: 'GET'|'POST'");
}

#[test]
fn general_value_fact_from_a_native_param() {
    // A native-typed param seeds the General layer (the runtime-enforced base fact).
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
    // Renders the source-cased, namespace-qualified FQN (no leading `\`), never the
    // lowercase-normalized segment — spans both a heap object and a declared param.
    let obj = "<?php\nnamespace App\\Models;\nclass User {}\n\
               function f() { $x = new User(); \\PHPStan\\dumpType($x); }\n";
    assert_eq!(one_type(obj), "dumped type: App\\Models\\User");
    let param = "<?php\nnamespace App\\Models;\nclass User {}\n\
                 function f(User $x) { \\PHPStan\\dumpType($x); }\n";
    assert_eq!(one_type(param), "dumped type: App\\Models\\User");
}

#[test]
fn instanceof_member_carrier_renders_the_narrowed_class() {
    // N4 `Member{yes:[…]}` gap: a coarse-supertype var narrowed by a live `instanceof`
    // carries the tighter class only on the Member carrier, no heap object;
    // `best_dump_type` now consults it between the exact-class and contract arms.
    let src = "<?php\ninterface CallLike {}\nfinal class Handler implements CallLike {}\n\
               function f(CallLike $x) { if ($x instanceof Handler) { \\PHPStan\\dumpType($x); } }\n";
    assert_eq!(one_type(src), "dumped type: Handler");
}

#[test]
fn instanceof_multi_member_falls_through_to_contract_carrier() {
    // A MULTI-member yes-set has no single faithful class spelling, so the Member
    // arm falls through to the declared contract carrier.
    let src = "<?php\ninterface CallLike {}\ninterface A {}\ninterface B {}\n\
               function f(CallLike $x) { if ($x instanceof A) { if ($x instanceof B) { \\PHPStan\\dumpType($x); } } }\n";
    assert_eq!(one_type(src), "dumped type: CallLike");
}

#[test]
fn unknown_is_honest() {
    assert_eq!(one_type("<?php \\PHPStan\\dumpType($undefined);\n"), "dumped type: unknown");
}

#[test]
fn assert_construct_is_verified_no_marker() {
    // FLIPPED 2026-07-25 (ADR-0052 amendment): `assert($x === 5)` reads as
    // `if (!$expr) throw`, Verified unconditionally, ignoring `zend.assertions` — so
    // no `(asserted)` marker (pre-ruling it printed one).
    let src = "<?php\nfunction f($x) { assert($x === 5); \\PHPStan\\dumpType($x); }\n";
    assert_eq!(one_type(src), "dumped type: 5");
}

#[test]
fn asserted_tag_stratum_carries_a_marker() {
    // `@phpstan-assert` stays Asserted (moved off `assert()`): a docblock claim never
    // launders as a proof.
    let src = "<?php\n/** @phpstan-assert null $x */\nfunction claimNull($x): void {}\n\
               function f($x) { claimNull($x); \\PHPStan\\dumpType($x); }\n";
    assert_eq!(one_type(src), "dumped type: null (asserted)");
}

// Multi-arg / zero-arg (ADR-0053 §7)

#[test]
fn multi_argument_dumps_one_report_per_argument() {
    let src = "<?php $a = 5; $b = 'x'; \\PHPStan\\dumpType($a, $b);\n";
    let ds = dumps(src);
    assert_eq!(ds.len(), 2, "one report per argument: {ds:?}");
    assert_eq!(ds[0].message, "dumped type: 5");
    assert_eq!(ds[1].message, "dumped type: 'x'");
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

// dumpPhpDocType — the declared-side view (ADR-0053 §2)

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
    // A `@param` with no matching native type is a docblock claim (Asserted, ADR-0052
    // §5) — the marker keeps the surface from laundering a claim as a proof.
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

// ADR-0062 — the array vocabulary (D4): a seeded `array{…}` @param no longer renders
// "no declared contract" (#51 L1) — the ONE speller (`spell_arms`) now spells the
// array vocabulary on the declared side; concrete `dumpType` arrays spell
// value-precisely through the same speller's value-side counterpart.

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
fn phpdoc_type_spells_a_sealed_shape_canonically() {
    // Issue #159: the phpdoc surface prints the canonical head, not source text —
    // `a` is required, so `non-empty-` says nothing the key does not.
    let src = "<?php\n/** @param non-empty-array{a: int} $x */\n\
               function f($x) { \\PHPStan\\dumpPhpDocType($x); }\n";
    assert_eq!(one_phpdoc(src), "dumped phpdoc type: array{a: int} (asserted)");
}

#[test]
fn dump_type_spells_a_keyed_concrete_array() {
    let src = "<?php\n$arr = ['a' => 'v'];\n\\PHPStan\\dumpType($arr);\n";
    assert_eq!(one_type(src), "dumped type: array{a: 'v'}");
}

#[test]
fn dump_type_spells_a_sequential_concrete_array_as_a_positional_list() {
    // D4-native divergence (ADR-0062 §6), restored by issue #163: a concrete array
    // is order-witnessed, so it spells `list{…}` positionally (issue #159's rule).
    // PHPStan stable writes `array{…}` here (its `ConstantArrayType` conflates the
    // two); we spell the fact.
    let src = "<?php\n$l = ['x', 'y'];\n\\PHPStan\\dumpType($l);\n";
    assert_eq!(one_type(src), "dumped type: list{'x', 'y'}");
}

// Transparency (ADR-0053 §10 §3)

#[test]
fn a_dump_reads_facts_and_binds_nothing() {
    // `emit_dumps` reads, never binds: a dump site is exempt from the blanket
    // call-argument drop (ADR-0070 gate's dump-read exception), so repeat dumps of
    // the same variable agree, while a genuinely unknown call still drops the fact.
    let ds = dumps("<?php $x = 5; \\PHPStan\\dumpType($x); \\PHPStan\\dumpType($x);\n");
    assert_eq!(ds.len(), 2, "{ds:?}");
    assert_eq!(ds[0].message, "dumped type: 5");
    assert_eq!(ds[1].message, "dumped type: 5", "a dump must not perturb the env");
    assert_eq!(one_type("<?php $x = 5; foo($x); \\PHPStan\\dumpType($x);\n"), "dumped type: unknown");
}

#[test]
fn a_second_dump_of_a_docblock_param_keeps_the_contract() {
    // Regression (2026-08-02): the second dump degraded to `unknown` — the blanket
    // call-argument drop ate the contract lane after the first dump rendered.
    let src = "<?php\n/** @param non-empty-string $method */\nfunction c($method) {\n\
               \\PHPStan\\dumpType($method);\n\\PHPStan\\dumpType($method);\n}\n";
    let ds = dumps(src);
    assert_eq!(ds.len(), 2, "{ds:?}");
    assert_eq!(ds[0].message, "dumped type: non-empty-string (asserted)");
    assert_eq!(ds[1].message, "dumped type: non-empty-string (asserted)");
}

#[test]
fn dump_type_then_var_dump_of_one_variable_agree_across_ids() {
    // Cross-id shape of the same regression: D3 and D4 share the fact source (§2),
    // so `var_dump` must not read a state the earlier `dumpType` statement degraded.
    let src = "<?php $verb = 'POST'; \\PHPStan\\dumpType($verb); var_dump($verb);\n";
    assert_eq!(one_type(src), "dumped type: 'POST'");
    let vd = var_dumps(src);
    assert_eq!(vd.len(), 1, "{vd:?}");
    assert_eq!(vd[0].message, "dumped type: 'POST'");
}

#[test]
fn var_dump_is_a_transparent_read_too() {
    let vd = var_dumps("<?php $x = 5; var_dump($x); var_dump($x);\n");
    assert_eq!(vd.len(), 2, "{vd:?}");
    assert_eq!(vd[0].message, "dumped type: 5");
    assert_eq!(vd[1].message, "dumped type: 5");
    assert_eq!(one_type("<?php $x = 5; var_dump($x); \\PHPStan\\dumpType($x);\n"), "dumped type: 5");
}

#[test]
fn a_second_dump_of_an_object_holder_keeps_the_class() {
    // The exception spans the object gate too (ADR-0070 condition 3): a dump hands
    // the handle nowhere that could mutate the referent, so the binding survives.
    let src = "<?php\nclass Foo {}\n$x = new Foo();\n\\PHPStan\\dumpType($x);\n\\PHPStan\\dumpType($x);\n";
    let ds = dumps(src);
    assert_eq!(ds.len(), 2, "{ds:?}");
    assert_eq!(ds[0].message, "dumped type: Foo");
    assert_eq!(ds[1].message, "dumped type: Foo");
}

// ADR-0053 D4 — `var_dump` default-on. The six resolution legs of §5.

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
    // Clean universe, no same-namespace homonym: the runtime falls back to global
    // var_dump, so the dump fires.
    let src = "<?php\nnamespace App;\nfunction g(int $v) { var_dump($v); }\n";
    let ds = var_dumps(src);
    assert_eq!(ds.len(), 1, "{ds:?}");
    assert_eq!(ds[0].message, "dumped type: int");
}

#[test]
fn leg_c_namespaced_homonym_is_silent() {
    // A same-namespace `App\var_dump` shadows the global — the call never reaches
    // it, so silence is the free safe side.
    let src = "<?php\nnamespace App;\nfunction var_dump($x) {}\n\
               function g(int $v) { var_dump($v); }\n";
    assert!(var_dumps(src).is_empty(), "a namespaced homonym stands the dump down");
}

#[test]
fn leg_c_dam_leaves_existence_unknown_and_is_silent() {
    // A dam site (eval) means dynamic code could mint `App\var_dump` at runtime, so
    // fallback is Unknown, not provable — no dump.
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
    // No argument expression to dump either way.
    assert!(var_dumps("<?php $f = var_dump(...);\n").is_empty());
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
    // Arity is S5's business, not a dump (§2).
    assert!(var_dumps("<?php var_dump();\n").is_empty());
}

#[test]
fn var_dump_shares_the_type_rendering() {
    // Same fact source and rendering as `debug.type` (§2).
    let obj = var_dumps("<?php\nclass Foo {}\n$x = new Foo();\nvar_dump($x);\n");
    assert_eq!(obj[0].message, "dumped type: Foo");
    let unknown = var_dumps("<?php var_dump($undefined);\n");
    assert_eq!(unknown[0].message, "dumped type: unknown");
}

// Depth-1 property-fetch dump reach (ADR-0052 §7, Gap B)

#[test]
fn dump_of_heap_bound_prop_renders_the_value() {
    // A written property fact reaches a direct `dumpType($h->p)` (previously unknown).
    let src = "<?php class H { public ?int $p = null; } \
        $h = new H(); $h->p = 7; \\PHPStan\\dumpType($h->p);";
    assert_eq!(one_type(src), "dumped type: 7");
}

#[test]
fn dump_of_promoted_prop_renders_the_value() {
    // A promoted-constructor prop reaches the dump bound both positionally and by
    // name (named form is the value-binding side of Gap A).
    let pos = "<?php class Cfg { public function __construct(public int $n) {} } \
        $c = new Cfg(30); \\PHPStan\\dumpType($c->n);";
    assert_eq!(one_type(pos), "dumped type: 30");
    let named = "<?php class Cfg { public function __construct(public int $n) {} } \
        $c = new Cfg(n: 30); \\PHPStan\\dumpType($c->n);";
    assert_eq!(one_type(named), "dumped type: 30");
}

#[test]
fn dump_of_prop_after_escape_is_unknown() {
    // The object escapes to an unknown call, sweeping its non-readonly props; the
    // dump (read through an alias that keeps the binding) honestly renders unknown.
    // Passing `$h` itself would also drop `$h`'s binding, so the alias isolates it.
    let src = "<?php class H { public ?int $p = null; } \
        $h = new H(); $h->p = 7; $a = $h; sink($h); unknownFn(); \\PHPStan\\dumpType($a->p);";
    assert_eq!(one_type(src), "dumped type: unknown");
}

#[test]
fn dump_of_readonly_prop_survives_escape() {
    // A readonly prop is sweep-immune, so its fact still reaches the dump after the
    // same escape shape.
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

// Scalar `@param` envelope seeding (ADR-0052 §9, contract-arm completion): a scalar
// `@param` with no matching native type lowers to a contract-arm lane the shared
// speller now spells (`positive-int`/`int<lo, hi>`/literal/`StrWith`), subject to
// the trust order's refine-within (an arm the native base cannot cover is dropped).

/// The single `debug.phpdoc-type` message body a one-dump source produces.
fn one_phpdoc(src: &str) -> String {
    let ds = dumps(src);
    let pd: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_PHPDOC_TYPE_ID).collect();
    assert_eq!(pd.len(), 1, "expected exactly one debug.phpdoc-type dump, got {ds:?}");
    pd[0].message.clone()
}

#[test]
fn scalar_param_positive_int_seeds_an_asserted_arm() {
    // No native base: the envelope alone seeds an Asserted arm on both surfaces.
    // The keyword is accepted on the way in and spelled as PHPStan spells it on the
    // way out (issue #90).
    let pd = "<?php\n/** @param positive-int $n */\nfunction f($n) { \\PHPStan\\dumpPhpDocType($n); }\n";
    assert_eq!(one_phpdoc(pd), "dumped phpdoc type: int<1, max> (asserted)");
    let ty = "<?php\n/** @param positive-int $n */\nfunction f($n) { \\PHPStan\\dumpType($n); }\n";
    assert_eq!(one_type(ty), "dumped type: int<1, max> (asserted)");
}

#[test]
fn scalar_param_int_interval_renders() {
    let src = "<?php\n/** @param int<1, 5> $n */\nfunction f($n) { \\PHPStan\\dumpPhpDocType($n); }\n";
    assert_eq!(one_phpdoc(src), "dumped phpdoc type: int<1, 5> (asserted)");
}

#[test]
fn scalar_param_refines_within_a_native_base() {
    // `@param positive-int` on a native `int` refines within it — Asserted (a strict
    // subset), and the refinement reaches the value lane too (issue #242): the
    // native pass' coarser `int` seed no longer shadows it just by being planted
    // first (the array vocabulary, which seeds its value lane the same way, is what
    // exposed the asymmetry).
    let pd = "<?php\n/** @param positive-int $m */\nfunction f(int $m) { \\PHPStan\\dumpPhpDocType($m); }\n";
    assert_eq!(one_phpdoc(pd), "dumped phpdoc type: int<1, max> (asserted)");
    let ty = "<?php\n/** @param positive-int $m */\nfunction f(int $m) { \\PHPStan\\dumpType($m); }\n";
    assert_eq!(one_type(ty), "dumped type: int<1, max> (asserted)");
}

#[test]
fn scalar_param_contradicting_the_native_type_seeds_nothing() {
    // Subset discipline: `@param string` on `int $x` contradicts the native type, so
    // it seeds NO arm; the native `int` value seed still flows to dumpType.
    let pd = "<?php\n/** @param string $x */\nfunction f(int $x) { \\PHPStan\\dumpPhpDocType($x); }\n";
    assert_eq!(one_phpdoc(pd), "dumped phpdoc type: no declared contract");
    let ty = "<?php\n/** @param string $x */\nfunction f(int $x) { \\PHPStan\\dumpType($x); }\n";
    assert_eq!(one_type(ty), "dumped type: int");
}

// Declared-return call-site seeding (ADR-0052 §9, the return direction)

#[test]
fn declared_object_return_makes_the_value_visible() {
    // A `: Foo` native return seeds a Verified Instance-MEMBERSHIP arm at the call
    // site — no `(asserted)` marker (Verified), but membership, not exact class.
    let src = "<?php\nclass Foo {}\nfunction createFoo(int $n): Foo { return new Foo(); }\n\
               function g() { $foo = createFoo(123); \\PHPStan\\dumpType($foo); }\n";
    assert_eq!(one_type(src), "dumped type: Foo");
}

#[test]
fn declared_union_return_narrows_under_instanceof() {
    // A `: User|Guest` return seeds both membership arms; an `instanceof User` guard
    // subtracts `User` on the else-branch (N4 narrowing over return arms), leaving
    // `Guest` — the narrowing shape, but at a CALL SITE.
    let src = "<?php\nclass User {}\nclass Guest {}\n\
               function who(): User|Guest { return new User(); }\n\
               function g() { $u = who(); if ($u instanceof User) {} else { \\PHPStan\\dumpType($u); } }\n";
    assert_eq!(one_type(src), "dumped type: Guest");
}

#[test]
fn declared_return_phpdoc_refines_asserted() {
    // `@return positive-int` refines within the native `int` return — an Asserted arm
    // at the call site (body not foldable, so no value fact beats the arm floor).
    let src = "<?php\n/** @return positive-int */\nfunction mk(int $s): int { return $s + 1; }\n\
               function g() { $x = mk(5); \\PHPStan\\dumpPhpDocType($x); }\n";
    assert_eq!(one_phpdoc(src), "dumped phpdoc type: int<1, max> (asserted)");
}

#[test]
fn a_template_type_return_dumps_the_template_argument_it_names() {
    // Issues #360 and #361. `template-type<…>` used to lower to
    // `Class("template-type")`, which the speller printed straight back —
    // `template-type (asserted)` read like a resolved class. #360 made it
    // vocabulary with an `Opaque` floor (`unknown`); #361 resolves the declared
    // side, so a spelled `Box<int>` subject now seeds exactly the arms
    // `@return int` seeds.
    let src = "<?php\n/** @template T */\nclass Box { public function __construct(public mixed $v) {} }\n\
               /**\n * @param Box<int> $b\n * @return template-type<Box<int>, Box, 'T'>\n */\n\
               function unwrap(Box $b): mixed { return $b; }\n\
               function g() { \\PHPStan\\dumpType(unwrap(new Box(1))); }\n";
    // The body returns the object, which the value domain cannot carry (ADR-0086
    // §4), so no summary crosses and the DECLARED read is what these dumps show —
    // the rung the two spellings are a claim about.
    assert_eq!(one_type(src), "dumped type: int (asserted)");
    // The same declaration with the type written out: identical surface, which is
    // the whole claim the resolution makes.
    let spelled = src.replace("template-type<Box<int>, Box, 'T'>", "int");
    assert_eq!(one_type(&spelled), "dumped type: int (asserted)");
}

#[test]
fn a_template_argument_return_dumps_what_that_template_dumps() {
    // The other half of #361's equivalence claim: where the utility names a
    // template rather than a spelled type, the surface is the template's own, and
    // the reader cannot tell which spelling produced it.
    let with = "<?php\n/** @template T */\nclass Box { public function __construct(public mixed $v) {} }\n\
                /**\n * @template T\n * @param Box<T> $b\n\
                \x20* @return template-type<Box<T>, Box, 'T'>\n */\n\
                function unwrap(Box $b): mixed { return $b; }\n\
                function g() { \\PHPStan\\dumpType(unwrap(new Box(1))); }\n";
    let plain = with.replace("template-type<Box<T>, Box, 'T'>", "T");
    assert_eq!(one_type(with), one_type(&plain));
    // Still `unknown` on both sides after issue #363, and for a reason that has
    // nothing to do with the utility: this `Box`'s constructor carries no
    // `@param T $v`, so `new Box(1)` proves no carry for the argument read to index.
    assert_eq!(one_type(with), "dumped type: unknown");
}

#[test]
fn a_receiver_carry_return_dumps_what_the_class_it_names_dumps() {
    // Issue #362, the same equivalence claim one layer further out: the subject is a
    // class-level template of the receiver, so the answer comes off the carry
    // `new Helper(new Model())` proved — and the surface is the one a hand-written
    // `@return Child` produces, down to the stratum. The carry read is where the
    // type comes from; it is not what the type is trusted as.
    let with = "<?php\n\
        /** @template TChild */\ninterface ModelInterface {}\n\
        /** @implements ModelInterface<Child> */\nclass Model implements ModelInterface {}\n\
        interface ChildInterface {}\nclass Child implements ChildInterface {}\n\
        /** @template T of ModelInterface */\n\
        class Helper {\n\
        \x20 /** @param T $model */\n  public function __construct(private ModelInterface $model) {}\n\
        \x20 /** @return template-type<T, ModelInterface, 'TChild'> */\n\
        \x20 public function first(): ChildInterface { return new Child(); }\n}\n\
        function g() { $h = new Helper(new Model()); $c = $h->first(); \\PHPStan\\dumpType($c); }\n";
    let plain = with.replace("template-type<T, ModelInterface, 'TChild'>", "Child");
    assert_eq!(one_type(with), one_type(&plain));
    assert_eq!(one_type(with), "dumped type: Child (asserted)");
}

#[test]
fn an_argument_carry_return_dumps_the_same_at_both_call_forms() {
    // Issue #363, and the parity claim that matters to the surface: the value
    // position and the assignment form read one seam, so `dumpType(unwrap($b))` and
    // `$v = unwrap($b); dumpType($v)` cannot disagree. Since ADR-0086 §2 the seam
    // that answers is the PROVEN one — `return $b->v` reads the property the
    // argument's object carried into the descent — so both forms are `1`, not the
    // docblock's Asserted claim about the return. Parity is the claim; the rung it
    // holds at is whichever rung is highest.
    let base = "<?php\n/** @template T */\n\
        class Box { /** @param T $v */ public function __construct(public mixed $v) {} }\n\
        /**\n * @template T\n * @param Box<T> $b\n * @return T\n */\n\
        function unwrap(Box $b): mixed { return $b->v; }\n";
    let nested = format!("{base}function g() {{ $b = new Box(1); \\PHPStan\\dumpType(unwrap($b)); }}\n");
    let assigned =
        format!("{base}function g() {{ $b = new Box(1); $v = unwrap($b); \\PHPStan\\dumpType($v); }}\n");
    assert_eq!(one_type(&nested), one_type(&assigned));
    assert_eq!(one_type(&nested), "dumped type: 1");
}

#[test]
fn a_folded_return_value_beats_the_return_arm() {
    // Precedence (ADR-0052 §9): a proven value fact is the floor's ceiling. A
    // trivially foldable `: int` return resolves to `1`, beating the `int` arm.
    let src = "<?php\nfunction mkInt(): int { return 1; }\n\
               function g() { $x = mkInt(); \\PHPStan\\dumpType($x); }\n";
    assert_eq!(one_type(src), "dumped type: 1");
}

// The fix payload (ADR-0010, issue #114): a statement-position explicit dump — the
// call IS the whole expression-statement — carries its remedy as a first-class
// payload of byte-span edits mirroring steins-edit's `Edit` shape, so `check --fix`
// (and any JSON consumer) can apply it by splicing. Embedded dumps and
// `debug.var-dump` carry none: deleting an enclosing binding, or a legal
// `var_dump()`, is a judgment call, not a mechanical remedy.

/// Splice a finding's fix edits into `src` (single-file fixtures: every edit
/// targets the one path).
fn apply_fix(src: &str, d: &Diagnostic) -> String {
    let fix = d.fix.as_ref().expect("finding carries a fix");
    let mut edits: Vec<&steins_infer::FixEdit> = fix.edits.iter().collect();
    edits.sort_by_key(|e| e.start);
    let mut out = String::new();
    let mut cursor = 0usize;
    for e in edits {
        out.push_str(&src[cursor..e.start as usize]);
        out.push_str(&e.replacement);
        cursor = e.end as usize;
    }
    out.push_str(&src[cursor..]);
    out
}

#[test]
fn statement_position_dump_carries_the_statement_deletion_fix() {
    let src = "<?php\n$x = 5;\n\\PHPStan\\dumpType($x);\n";
    let ds = dumps(src);
    assert_eq!(ds.len(), 1);
    let fix = ds[0].fix.as_ref().expect("statement-position dump carries a fix");
    assert_eq!(fix.title, "remove the dump statement");
    // Bytes 14..37 are `\PHPStan\dumpType($x);` plus the trailing newline, so no
    // blank gutter line is left.
    assert_eq!(fix.edits.len(), 1);
    let e = &fix.edits[0];
    assert_eq!((e.path.as_str(), e.start, e.end, e.replacement.as_str()), ("t.php", 14, 37, ""));
    let after = apply_fix(src, &ds[0]);
    assert_eq!(after, "<?php\n$x = 5;\n");
    let tree = SourceTree::parse(&after);
    assert!(check(&tree, &[], "t.php").is_empty(), "rerun on the fixed source must be clean");
}

#[test]
fn indented_dump_statement_fix_swallows_the_whole_line() {
    let src = "<?php\nfunction f(int $x): int {\n    \\PHPStan\\dumpType($x);\n    return $x;\n}\n";
    let ds = dumps(src);
    assert_eq!(ds.len(), 1);
    let after = apply_fix(src, &ds[0]);
    assert_eq!(after, "<?php\nfunction f(int $x): int {\n    return $x;\n}\n");
}

#[test]
fn dump_with_a_trailing_comment_deletes_only_the_statement() {
    // Something else shares the line — the deletion span stays exactly the
    // statement, and the comment survives.
    let src = "<?php\n$x = 1;\n\\PHPStan\\dumpType($x); // check me\n";
    let ds = dumps(src);
    assert_eq!(ds.len(), 1);
    let after = apply_fix(src, &ds[0]);
    assert_eq!(after, "<?php\n$x = 1;\n // check me\n");
}

#[test]
fn phpdoc_dump_carries_the_fix_too() {
    let src = "<?php\n/** @param int $x */\nfunction f($x) { \\PHPStan\\dumpPhpDocType($x); }\n";
    let ds = dumps(src);
    assert_eq!(ds.len(), 1);
    assert_eq!(ds[0].id, DEBUG_PHPDOC_TYPE_ID);
    let fix = ds[0].fix.as_ref().expect("statement-position phpdoc dump carries a fix");
    assert_eq!(fix.title, "remove the dump statement");
}

#[test]
fn zero_argument_dump_carries_the_fix_too() {
    // A runtime fatal either way, but still the whole statement, so the deletion
    // remedy applies unchanged.
    let src = "<?php\n\\PHPStan\\dumpType();\n";
    let ds = dumps(src);
    assert_eq!(ds.len(), 1);
    let after = apply_fix(src, &ds[0]);
    assert_eq!(after, "<?php\n");
}

#[test]
fn multi_argument_dump_findings_share_one_statement_edit() {
    // One statement, one deletion: each per-argument finding carries the SAME edit,
    // so a consumer applying both (with identical-edit dedupe) deletes it once.
    let src = "<?php\n$x = 1;\n$y = 2;\n\\PHPStan\\dumpType($x, $y);\n";
    let ds = dumps(src);
    assert_eq!(ds.len(), 2);
    let a = ds[0].fix.as_ref().expect("first finding carries the fix");
    let b = ds[1].fix.as_ref().expect("second finding carries the fix");
    assert_eq!(a, b);
    assert_eq!(a.edits.len(), 1);
}

#[test]
fn embedded_dumps_carry_no_fix() {
    // The dump is part of a larger statement — deleting the whole statement would
    // delete the enclosing binding too, so no fix rides along.
    for src in [
        "<?php\n$x = 1;\n$y = \\PHPStan\\dumpType($x);\n",
        "<?php\nfunction f($x) { return \\PHPStan\\dumpType($x); }\n",
        "<?php\n$x = 1;\necho \\PHPStan\\dumpType($x);\n",
    ] {
        let ds = dumps(src);
        assert!(!ds.is_empty(), "dump still reports in {src:?}");
        for d in &ds {
            assert_eq!(d.fix, None, "embedded dump must carry no fix in {src:?}");
        }
    }
}

#[test]
fn var_dump_carries_no_fix() {
    // Scope guard (issue #114): a `var_dump()` is legal working PHP — deleting it is
    // a judgment call, so `debug.var-dump` ships no fix payload.
    let ds = var_dumps("<?php\n$x = 1;\nvar_dump($x);\n");
    assert_eq!(ds.len(), 1);
    assert_eq!(ds[0].fix, None);
}

// `class-string` (issue #236)

#[test]
fn written_class_const_still_dumps_its_literal() {
    // ADR-0043 resolves a WRITTEN `Foo::class` to its FQN string, strictly more
    // precise than the refinement — the class-string rung sits below the literal
    // rung exactly so this cannot regress. The namespace separator is escaped the
    // way single-quoted PHP escapes it (and PHPStan prints it).
    let src = "<?php namespace N;\nclass Foo {}\n\\PHPStan\\dumpType(Foo::class);\n";
    assert_eq!(one_type(src), "dumped type: 'N\\\\Foo'");
}

#[test]
fn relative_class_consts_dump_as_class_string() {
    // `self`/`parent`/`static::class` name a class-like the index knows but whose
    // declared CASING it does not carry, so only the refinement (not a literal) may
    // be emitted.
    for kw in ["self", "parent", "static"] {
        let src = format!(
            "<?php namespace N;\nclass Base {{}}\n\
             class Child extends Base {{ function go(): void {{ \\PHPStan\\dumpType({kw}::class); }} }}\n"
        );
        assert_eq!(one_type(&src), "dumped type: class-string", "{kw}::class");
    }
}

#[test]
fn relative_class_const_binds_the_fact_through_an_assignment() {
    // The producer lives in the value lane, not only on the dump surface.
    let src = "<?php namespace N;\nclass Base {}\n\
        class Child extends Base { function go(): void { $c = static::class; \\PHPStan\\dumpType($c); } }\n";
    assert_eq!(one_type(src), "dumped type: class-string");
}

#[test]
fn relative_class_const_outside_a_class_produces_nothing() {
    // `self::class` at file scope is a compile error, not a class-string — no
    // class-like to name, so the surface stays honestly silent.
    let src = "<?php \\PHPStan\\dumpType(self::class);\n";
    assert_eq!(one_type(src), "dumped type: unknown");
}

#[test]
fn declared_class_string_param_dumps_as_class_string() {
    // The declaration-flow producer: `interface-string`/`trait-string`/`enum-string`
    // name the same predicate as `class-string`, and PHPStan renders all four back
    // as `class-string` too.
    for spelling in ["class-string", "interface-string", "trait-string", "enum-string"] {
        let src = format!(
            "<?php /** @param {spelling} $c */ function f($c): void {{ \\PHPStan\\dumpType($c); }}\n"
        );
        assert_eq!(one_type(&src), "dumped type: class-string (asserted)", "{spelling}");
    }
}

#[test]
fn a_parameterized_class_string_param_widens_to_the_bare_form() {
    // The generics vocabulary owns `T` (ADR-0032's carry, issue #10); until it
    // lands, `class-string<Foo>` widens to the bare predicate, satisfied by every
    // member of the parameterized set anyway.
    let src = "<?php class Foo {}\n\
        /** @param class-string<Foo> $c */ function f($c): void { \\PHPStan\\dumpType($c); }\n";
    assert_eq!(one_type(src), "dumped type: class-string (asserted)");
}
