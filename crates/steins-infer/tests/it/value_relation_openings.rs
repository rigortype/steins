//! ADR-0043's 2026-10-11 amendment (issues #951, #941 a/b, #952 l1): the value
//! relation's openings, one test per row of the rule tables on
//! `arg_check::is_type_error` and `value_tops::top_is_type_error`.
//!
//! Every verdict below was run on PHP 8.5.11 at the argument, return and property
//! seams in both modes (the three seams agree on every cell, and only the
//! scalar-into-scalar cells differ between the modes). A firing test is a row PHP
//! raises a `TypeError` on; a silent test is either a row PHP accepts (an accept
//! control) or a row the relation declines because it is not certain (a pinned
//! decline, named as such).
//!
//! The [`Boot`] mock stands in for the sidecar: the absence family is open, the
//! boot surface holds exactly the names listed, and the builtin parameter lists are
//! the engine's own (`ReflectionFunction::getParameters()` at 8.5.11).

use std::collections::HashMap;

use steins_infer::{Diagnostic, Folder, check_with};
use steins_sidecar::BuiltinParam;
use steins_syntax::{ArgValue, SourceTree};

struct Boot {
    available: bool,
    functions: Vec<&'static str>,
    params: HashMap<String, Vec<BuiltinParam>>,
}

fn p(name: &str, ty: &str) -> BuiltinParam {
    BuiltinParam {
        name: name.to_owned(),
        ty: Some(ty.to_owned()),
        by_ref: false,
        variadic: false,
        optional: false,
    }
}

impl Boot {
    fn ready() -> Self {
        let mut params = HashMap::new();
        params.insert("spl_object_id".to_owned(), vec![p("object", "object")]);
        params.insert("iterator_to_array".to_owned(), vec![p("iterator", "Traversable|array")]);
        params.insert("array_keys".to_owned(), vec![p("array", "array")]);
        params.insert("usort".to_owned(), vec![p("array", "array"), p("callback", "callable")]);
        params.insert("array_filter".to_owned(), vec![p("array", "array"), p("callback", "?callable")]);
        params.insert("str_replace".to_owned(), vec![p("search", "array|string")]);
        params.insert("strlen".to_owned(), vec![p("string", "string")]);
        params.insert("var_dump".to_owned(), vec![p("value", "mixed")]);
        Boot { available: true, functions: vec!["strlen", "trim"], params }
    }

    /// The sound subset: no sidecar answers, so no absence claim holds.
    fn closed() -> Self {
        Boot { available: false, ..Boot::ready() }
    }

    /// A boot surface on which `trime` is a resident extension function.
    fn with_function(name: &'static str) -> Self {
        let mut b = Boot::ready();
        b.functions.push(name);
        b
    }
}

impl Folder for Boot {
    fn fold(&mut self, _n: &str, _a: &[ArgValue], _strict: bool) -> Option<ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        self.available
    }
    fn boot_surface_function(&mut self, fqn: &str) -> Option<bool> {
        self.available.then(|| self.functions.iter().any(|f| f.eq_ignore_ascii_case(fqn)))
    }
    fn boot_surface_class_like(&mut self, _fqn: &str) -> Option<bool> {
        self.available.then_some(false)
    }
    fn builtin_param_types(&mut self, name: &str) -> Option<Vec<BuiltinParam>> {
        self.params.get(&name.to_ascii_lowercase()).cloned()
    }
}

fn findings_with(src: &str, folder: &mut dyn Folder) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "t.php", folder)
        .into_iter()
        .filter(|d| d.id.starts_with("type."))
        .collect()
}

/// The `type.*` ids a fixture emits, in order, under a live boot surface.
fn ids(src: &str) -> Vec<String> {
    ids_with(src, &mut Boot::ready())
}

fn ids_with(src: &str, folder: &mut dyn Folder) -> Vec<String> {
    findings_with(src, folder).into_iter().map(|d| d.id.to_owned()).collect()
}

fn strict(body: &str) -> String {
    format!("<?php\ndeclare(strict_types=1);\n{body}")
}

fn coercive(body: &str) -> String {
    format!("<?php\n{body}")
}

const ARG: &str = "type.argument-mismatch";
const RET: &str = "type.return-mismatch";
const PROP: &str = "type.property-mismatch";

fn fires(src: &str, id: &str) {
    assert_eq!(ids(src), vec![id.to_owned()], "expected one {id} for:\n{src}");
}

fn silent(src: &str) {
    let got = ids(src);
    assert!(got.is_empty(), "expected silence, got {got:?} for:\n{src}");
}

/// Both modes give the same verdict for every row this amendment opens.
fn fires_both_modes(body: &str, id: &str) {
    fires(&strict(body), id);
    fires(&coercive(body), id);
}

fn silent_both_modes(body: &str) {
    silent(&strict(body));
    silent(&coercive(body));
}

const K: &str = "class K {}\ninterface I {}\ninterface A {}\ninterface B {}\n";

// ---------------------------------------------------------------------------
// null against a NativeType (#951 a, b, f)
// ---------------------------------------------------------------------------

#[test]
fn null_to_a_class_parameter_is_a_type_error() {
    // #951 a.
    fires_both_modes(&format!("{K}function tk(K $k): void {{}}\ntk(null);\n"), ARG);
}

#[test]
fn null_to_an_int_parameter_still_fires() {
    // Kept shape: the scalar row reported before the amendment.
    fires_both_modes("function ti(int $i): void {}\nti(null);\n", ARG);
}

#[test]
fn null_to_an_interface_an_intersection_and_a_dnf_type_is_a_type_error() {
    fires_both_modes(&format!("{K}function f(I $x): void {{}}\nf(null);\n"), ARG);
    fires_both_modes(&format!("{K}function f(A&B $x): void {{}}\nf(null);\n"), ARG);
    fires_both_modes(&format!("{K}function f((A&B)|K $x): void {{}}\nf(null);\n"), ARG);
}

#[test]
fn null_to_a_nullable_class_parameter_is_accepted() {
    silent_both_modes(&format!("{K}function f(?K $x): void {{}}\nf(null);\n"));
    silent_both_modes(&format!("{K}function f(K|null $x): void {{}}\nf(null);\n"));
    silent_both_modes(&format!("{K}function f((A&B)|null $x): void {{}}\nf(null);\n"));
}

#[test]
fn null_to_a_class_parameter_with_a_null_default_is_accepted() {
    // `implicit_null_accepted`: PHP widens the parameter (an 8.4 deprecation).
    silent_both_modes(&format!("{K}function f(K $x = null): void {{}}\nf(null);\n"));
    silent_both_modes(&format!("{K}function f(A&B $x = null): void {{}}\nf(null);\n"));
}

#[test]
fn null_returned_from_a_class_return_type_is_a_type_error() {
    // #951 b.
    fires_both_modes(&format!("{K}function b(): K {{ return null; }}\n"), RET);
    silent_both_modes(&format!("{K}function b(): ?K {{ return null; }}\n"));
}

#[test]
fn null_written_to_a_class_typed_property_is_a_type_error() {
    // #951 f.
    let body = format!("{K}final class P {{ public K $k; }}\nfunction f(): void {{ $p = new P(); $p->k = null; }}\n");
    fires_both_modes(&body, PROP);
    silent_both_modes(&format!(
        "{K}final class P {{ public ?K $k = null; }}\nfunction f(): void {{ $p = new P(); $p->k = null; }}\n"
    ));
}

#[test]
fn null_at_a_method_a_static_method_and_a_constructor_is_a_type_error() {
    let decl = format!(
        "{K}final class C {{\n    public function __construct(K $k) {{}}\n    public function m(K $k): void {{}}\n    public static function s(K $k): void {{}}\n}}\n"
    );
    fires_both_modes(&format!("{decl}new C(null);\n"), ARG);
    fires_both_modes(&format!("{decl}function f(C $c): void {{ $c->m(null); }}\n"), ARG);
    fires_both_modes(&format!("{decl}C::s(null);\n"), ARG);
}

#[test]
fn a_variable_proven_null_to_a_class_parameter_is_a_type_error() {
    fires_both_modes(&format!("{K}function tk(K $k): void {{}}\nfunction g(): void {{ $x = null; tk($x); }}\n"), ARG);
}

#[test]
fn a_null_variable_under_an_instanceof_guard_stays_silent() {
    // The guard folds the branch dead: PHP never makes the call.
    silent_both_modes(&format!(
        "{K}function tk(K $k): void {{}}\nfunction g(): void {{ $x = null; if ($x instanceof K) {{ tk($x); }} }}\n"
    ));
}

#[test]
fn null_to_self_declines() {
    // Pinned decline: `self`/`static`/`parent` lower to no NativeType.
    silent_both_modes("final class C { public function m(self $o): void {} }\nfunction f(C $c): void { $c->m(null); }\n");
}

#[test]
fn the_possibly_grade_keeps_its_object_bearing_null_reading() {
    // Kept shape: a `?string` fact against `K` still reports its string arm; the
    // new null row does not turn it into an all-arms verdict (#291 ruling).
    let src = strict(&format!("{K}function tk(K $k): void {{}}\nfunction g(?string $s): void {{ tk($s); }}\n"));
    assert_eq!(ids(&src), vec!["type.maybe-argument-mismatch".to_owned()]);
}

// ---------------------------------------------------------------------------
// array literals against a NativeType (#951 d, e)
// ---------------------------------------------------------------------------

#[test]
fn an_array_to_an_int_parameter_is_a_type_error() {
    // #951 d.
    fires_both_modes("function ti(int $i): void {}\nti([]);\n", ARG);
}

#[test]
fn an_array_to_every_representable_native_type_is_a_type_error() {
    for ty in ["bool", "string", "int|string", "float", "false", "?int", "K", "I", "A&B", "(A&B)|K"] {
        fires_both_modes(&format!("{K}function f({ty} $x): void {{}}\nf([1, 2]);\n"), ARG);
    }
}

#[test]
fn an_array_holding_unknown_values_is_still_an_array() {
    fires_both_modes("function ti(int $i): void {}\nfunction g($v): void { ti([$v]); }\n", ARG);
}

#[test]
fn an_array_returned_from_an_int_return_type_is_a_type_error() {
    // #951 e.
    fires_both_modes("function e(): int { return []; }\n", RET);
}

#[test]
fn an_array_written_to_an_int_typed_property_is_a_type_error() {
    fires_both_modes(
        "final class P { public int $i = 0; }\nfunction f(): void { $p = new P(); $p->i = []; }\n",
        PROP,
    );
}

#[test]
fn an_array_to_mixed_array_iterable_and_nullable_array_is_accepted() {
    for ty in ["mixed", "array", "?array", "iterable", "array|null"] {
        silent_both_modes(&format!("function f({ty} $x): void {{}}\nf([1]);\n"));
    }
}

// ---------------------------------------------------------------------------
// closures against a NativeType (#951 c)
// ---------------------------------------------------------------------------

#[test]
fn a_closure_returned_from_a_class_return_type_is_a_type_error() {
    // #951 c.
    fires_both_modes(&format!("{K}function c(): K {{ return fn () => 1; }}\n"), RET);
}

#[test]
fn every_closure_spelling_to_a_class_or_interface_parameter_is_a_type_error() {
    for value in ["function () {}", "fn () => 1", "strlen(...)"] {
        fires_both_modes(&format!("{K}function f(K $x): void {{}}\nf({value});\n"), ARG);
        fires_both_modes(&format!("{K}function f(I $x): void {{}}\nf({value});\n"), ARG);
    }
}

#[test]
fn a_closure_to_int_bool_and_float_is_a_type_error() {
    for ty in ["int", "bool", "float", "int|float"] {
        fires_both_modes(&format!("function f({ty} $x): void {{}}\nf(fn () => 1);\n"), ARG);
    }
}

#[test]
fn a_closure_to_string_fires_strict_and_declines_coercive() {
    // Pinned decline: the coercive `__toString` rule is not specialised to Closure.
    fires(&strict("function f(string $x): void {}\nf(fn () => 1);\n"), ARG);
    silent(&coercive("function f(string $x): void {}\nf(fn () => 1);\n"));
}

#[test]
fn a_closure_to_closure_callable_object_and_mixed_is_accepted() {
    for ty in ["Closure", "?Closure", "Closure|int", "callable", "object", "mixed"] {
        silent_both_modes(&format!("function f({ty} $x): void {{}}\nf(fn () => 1);\n"));
    }
}

#[test]
fn a_first_class_callable_of_an_unresolved_name_declines() {
    // Pinned decline: PHP throws an Error for the name before any type check.
    silent_both_modes(&format!("{K}function f(K $x): void {{}}\nf(undefined_function(...));\n"));
}

#[test]
fn a_closure_written_to_a_class_typed_property_is_a_type_error() {
    fires_both_modes(
        &format!("{K}final class P {{ public K $k; }}\nfunction f(): void {{ $p = new P(); $p->k = fn () => 1; }}\n"),
        PROP,
    );
}

// ---------------------------------------------------------------------------
// an object written to a typed property (#952 l1)
// ---------------------------------------------------------------------------

const AB: &str = "class A {}\nclass B {}\nclass A2 extends A {}\n";

#[test]
fn a_new_of_an_unrelated_class_written_to_a_typed_property_is_a_type_error() {
    // #952 l1.
    fires_both_modes(
        &format!("{AB}class P {{ public A $a; }}\nfunction l1(): void {{ $p = new P(); $p->a = new B(); }}\n"),
        PROP,
    );
}

#[test]
fn a_string_written_to_a_class_typed_property_still_fires() {
    // Kept shape: #952 l0.
    fires_both_modes(
        &format!("{AB}class P {{ public A $a; }}\nfunction l0(): void {{ $p = new P(); $p->a = \"x\"; }}\n"),
        PROP,
    );
}

#[test]
fn a_new_of_the_class_or_a_subclass_written_to_a_typed_property_is_accepted() {
    for value in ["new A()", "new A2()"] {
        silent_both_modes(&format!(
            "{AB}class P {{ public A $a; }}\nfunction f(): void {{ $p = new P(); $p->a = {value}; }}\n"
        ));
    }
}

#[test]
fn a_new_whose_hierarchy_is_unresolvable_declines() {
    // Pinned decline: `Missing` may implement anything.
    silent_both_modes(&format!(
        "{AB}class C extends Missing {{}}\nclass P {{ public A $a; }}\nfunction f(): void {{ $p = new P(); $p->a = new C(); }}\n"
    ));
}

#[test]
fn an_enum_case_written_to_a_class_typed_property_is_a_type_error() {
    fires_both_modes(
        &format!("{AB}enum E {{ case X; }}\nclass P {{ public A $a; }}\nfunction f(): void {{ $p = new P(); $p->a = E::X; }}\n"),
        PROP,
    );
}

// ---------------------------------------------------------------------------
// the `array`, `object` and `iterable` tops
// ---------------------------------------------------------------------------

#[test]
fn a_non_array_to_an_array_parameter_is_a_type_error() {
    for value in ["\"x\"", "42", "1.5", "true", "null", "fn () => 1", "new K()"] {
        fires_both_modes(&format!("{K}function f(array $a): void {{}}\nf({value});\n"), ARG);
    }
}

#[test]
fn null_to_a_nullable_or_null_defaulted_array_parameter_is_accepted() {
    silent_both_modes("function f(?array $a): void {}\nf(null);\n");
    silent_both_modes("function f(array|null $a): void {}\nf(null);\n");
    silent_both_modes("function f(array $a = null): void {}\nf(null);\n");
}

#[test]
fn an_array_or_a_scalar_to_an_object_parameter_is_a_type_error() {
    for value in ["[]", "\"x\"", "42", "null"] {
        fires_both_modes(&format!("function f(object $o): void {{}}\nf({value});\n"), ARG);
    }
}

#[test]
fn an_object_a_closure_and_an_enum_case_to_an_object_parameter_are_accepted() {
    for value in ["new K()", "fn () => 1", "E::X", "new stdClass()"] {
        silent_both_modes(&format!("{K}enum E {{ case X; }}\nfunction f(object $o): void {{}}\nf({value});\n"));
    }
}

#[test]
fn a_closure_a_scalar_and_a_non_traversable_object_to_iterable_are_type_errors() {
    for value in ["fn () => 1", "\"x\"", "42", "null", "new K()"] {
        fires_both_modes(&format!("{K}function f(iterable $i): void {{}}\nf({value});\n"), ARG);
    }
}

#[test]
fn an_array_and_a_traversable_object_to_iterable_are_accepted() {
    for value in ["[1]", "new ArrayIterator([])", "new Agg()"] {
        silent_both_modes(&format!(
            "final class Agg implements IteratorAggregate {{ public function getIterator(): Iterator {{ return new ArrayIterator([]); }} }}\nfunction f(iterable $i): void {{}}\nf({value});\n"
        ));
    }
}

#[test]
fn a_union_of_a_top_with_a_scalar_declines() {
    // Pinned decline: `array|string` lowers to neither a NativeType nor a top.
    silent_both_modes("function f(array|string $a): void {}\nf(42);\n");
    silent_both_modes("function f(callable|string $a): void {}\nf(42);\n");
    silent_both_modes("function f(iterable|int $a): void {}\nf(fn () => 1);\n");
}

#[test]
fn a_variable_proven_a_scalar_to_an_array_parameter_is_a_type_error() {
    fires_both_modes("function f(array $a): void {}\nfunction g(): void { $s = \"x\"; f($s); }\n", ARG);
}

#[test]
fn a_by_reference_and_a_variadic_top_parameter_decline() {
    // Pinned declines: by-reference positions are skipped, a variadic ends the walk.
    silent_both_modes("function f(array &$a): void {}\nfunction g(): void { $s = \"x\"; f($s); }\n");
    silent_both_modes("function f(callable ...$cs): void {}\nf(42);\n");
}

#[test]
fn a_top_is_judged_at_a_method_a_constructor_and_a_closure_call() {
    let decl = "final class C {\n    public function __construct(callable $c) {}\n    public function m(array $a): void {}\n    public static function s(object $o): void {}\n}\n";
    fires_both_modes(&format!("{decl}new C(42);\n"), ARG);
    fires_both_modes(&format!("{decl}function f(C $c): void {{ $c->m(\"x\"); }}\n"), ARG);
    fires_both_modes(&format!("{decl}C::s([]);\n"), ARG);
    fires_both_modes("function g(): void { $f = function (array $a) {}; $f(\"x\"); }\n", ARG);
}

// ---------------------------------------------------------------------------
// the `callable` top (#941 a, b)
// ---------------------------------------------------------------------------

const FOO: &str = "function foo(callable $c): void {}\n";

#[test]
fn an_int_to_a_callable_parameter_is_a_type_error() {
    // #941 a.
    fires_both_modes(&format!("{FOO}foo(42);\n"), ARG);
}

#[test]
fn an_absent_function_name_to_a_callable_parameter_is_a_type_error() {
    // #941 b.
    fires_both_modes(&format!("{FOO}foo(\"trime\");\n"), ARG);
}

#[test]
fn a_bool_a_float_and_null_to_a_callable_parameter_are_type_errors() {
    for value in ["true", "false", "1.5", "null"] {
        fires_both_modes(&format!("{FOO}foo({value});\n"), ARG);
    }
}

#[test]
fn null_to_a_nullable_callable_parameter_is_accepted() {
    silent_both_modes("function foo(?callable $c): void {}\nfoo(null);\n");
    silent_both_modes("function foo(callable $c = null): void {}\nfoo(null);\n");
}

#[test]
fn a_function_name_that_exists_to_a_callable_parameter_is_accepted() {
    // `strlen` is a builtin, `mine` a project function, `\trim` the global builtin.
    for value in ["\"strlen\"", "\"trim\"", "\"\\\\trim\"", "\"mine\"", "\"MINE\""] {
        silent_both_modes(&format!("{FOO}function mine(): void {{}}\nfoo({value});\n"));
    }
}

#[test]
fn an_absent_function_name_declines_without_the_absence_family() {
    // Pinned decline: no sidecar, no absence claim (ADR-0049 A2ii/A9).
    let src = strict(&format!("{FOO}foo(\"trime\");\n"));
    assert!(ids_with(&src, &mut Boot::closed()).is_empty());
}

#[test]
fn a_function_name_resident_on_the_boot_surface_declines() {
    // Pinned decline: an extension function the catalog does not know (A2ii homonym).
    let src = strict(&format!("{FOO}foo(\"trime\");\n"));
    assert!(ids_with(&src, &mut Boot::with_function("trime")).is_empty());
}

#[test]
fn a_conditionally_declared_function_name_declines() {
    // The index holds the polyfill: the name is not absent.
    silent_both_modes(&format!(
        "{FOO}if (!function_exists('trime')) {{ function trime(): void {{}} }}\nfoo(\"trime\");\n"
    ));
}

#[test]
fn a_function_name_reaching_the_argument_through_a_variable_declines() {
    // Pinned decline: an `is_callable($s)` guard the walk does not model may stand.
    silent_both_modes(&format!("{FOO}function g(): void {{ $s = \"trime\"; foo($s); }}\n"));
    // The same for a class constant that resolves to the name.
    silent_both_modes(&format!("{FOO}final class C {{ const CB = 'trime'; }}\nfoo(C::CB);\n"));
}

#[test]
fn a_top_is_not_judged_inside_a_binding_descent() {
    // Pinned decline: a rebound value may sit under a guard the walk does not
    // model (`is_callable`, `is_array`, `instanceof`), so only the plain pass judges.
    silent_both_modes(&format!("{FOO}function g($c): void {{ foo($c); }}\ng(42);\n"));
    silent_both_modes(&format!("{K}function tk(K $k): void {{}}\nfunction g($c): void {{ tk($c); }}\ng(fn () => 1);\n"));
}

#[test]
fn a_variable_proven_an_int_to_a_callable_parameter_is_a_type_error() {
    fires_both_modes(&format!("{FOO}function g(): void {{ $x = 42; foo($x); }}\n"), ARG);
}

#[test]
fn a_static_callable_string_naming_an_absent_class_or_method_is_a_type_error() {
    let decl = format!("{FOO}class C {{ public static function s(): void {{}} }}\n");
    fires_both_modes(&format!("{decl}foo(\"Nope::m\");\n"), ARG);
    fires_both_modes(&format!("{decl}foo(\"C::nope\");\n"), ARG);
    silent_both_modes(&format!("{decl}foo(\"C::s\");\n"));
}

#[test]
fn a_callable_string_through_self_or_parent_declines() {
    // Pinned decline: the class half depends on the calling scope.
    silent_both_modes(&format!("{FOO}foo(\"parent::m\");\nfoo(\"self::m\");\n"));
}

#[test]
fn an_array_that_is_no_callable_pair_is_a_type_error() {
    let decl = format!("{FOO}class C {{ public static function s(): void {{}} }}\n");
    for value in ["[]", "[1, 2]", "[\"C\"]", "[\"C\", \"s\", \"x\"]", "[\"a\" => \"C\", \"b\" => \"s\"]", "[\"C\", 1]"] {
        fires_both_modes(&format!("{decl}foo({value});\n"), ARG);
    }
}

#[test]
fn an_array_pair_naming_an_absent_method_or_class_is_a_type_error() {
    let decl = format!("{FOO}class C {{ public static function s(): void {{}} }}\n");
    fires_both_modes(&format!("{decl}foo([\"C\", \"nope\"]);\n"), ARG);
    fires_both_modes(&format!("{decl}foo([new C(), \"nope\"]);\n"), ARG);
    fires_both_modes(&format!("{decl}foo([\"Nope\", \"m\"]);\n"), ARG);
}

#[test]
fn an_array_pair_naming_a_method_that_exists_is_accepted() {
    let decl = format!("{FOO}class C {{ public static function s(): void {{}} }}\n");
    silent_both_modes(&format!("{decl}foo([\"C\", \"s\"]);\n"));
    silent_both_modes(&format!("{decl}foo([1 => \"s\", 0 => \"C\"]);\n"));
    silent_both_modes(&format!("{decl}foo([new C(), \"s\"]);\n"));
}

#[test]
fn an_array_pair_naming_an_instance_method_statically_declines() {
    // Pinned decline: `["C", "i"]` is callable from inside an instance of C.
    silent_both_modes(&format!("{FOO}class C {{ public function i(): void {{}} }}\nfoo([\"C\", \"i\"]);\n"));
}

#[test]
fn an_array_pair_on_a_class_with_magic_calls_is_accepted() {
    silent_both_modes(&format!(
        "{FOO}class M {{ public static function __callStatic($n, $a) {{}} public function __call($n, $a) {{}} }}\nfoo([\"M\", \"nope\"]);\n"
    ));
}

#[test]
fn a_builtin_class_pair_and_a_builtin_object_decline() {
    // Pinned declines: a reflected class refutes, it never convicts, so
    // `stdClass` has no provably absent member.
    silent_both_modes(&format!("{FOO}foo([\"stdClass\", \"nope\"]);\n"));
    silent_both_modes(&format!("{FOO}foo(new stdClass());\n"));
}

#[test]
fn an_object_without_invoke_to_a_callable_parameter_is_a_type_error() {
    fires_both_modes(&format!("{FOO}class C {{}}\nfoo(new C());\n"), ARG);
}

#[test]
fn an_invokable_object_a_closure_and_an_enum_case_to_a_callable_parameter() {
    silent_both_modes(&format!("{FOO}class Inv {{ public function __invoke(): void {{}} }}\nfoo(new Inv());\n"));
    silent_both_modes(&format!("{FOO}foo(fn () => 1);\nfoo(strlen(...));\n"));
    // Pinned decline: an enum may declare `__invoke`.
    silent_both_modes(&format!("{FOO}enum E {{ case X; }}\nfoo(E::X);\n"));
}

#[test]
fn an_object_whose_class_has_a_magic_call_to_a_callable_parameter_declines() {
    // Pinned decline: the chain walk treats `__call` as an obstacle.
    silent_both_modes(&format!(
        "{FOO}class C {{ public function __call($n, $a) {{}} }}\nfoo(new C());\n"
    ));
}

// ---------------------------------------------------------------------------
// null at a builtin position the internal-null carve-out does not cover
// ---------------------------------------------------------------------------

#[test]
fn null_to_a_builtin_object_or_array_position_is_a_type_error_even_coercive() {
    fires_both_modes("spl_object_id(null);\n", ARG);
    fires_both_modes("iterator_to_array(null);\n", ARG);
    fires_both_modes("array_keys(null);\n", ARG);
    fires_both_modes("function g(): void { $a = [2, 1]; usort($a, null); }\n", ARG);
}

#[test]
fn null_to_a_builtin_position_mixing_array_and_string_follows_the_scalar_rule() {
    // Coercive: a deprecation (the carve-out); strict: a TypeError.
    silent(&coercive("str_replace(null);\n"));
    fires(&strict("str_replace(null);\n"), ARG);
}

#[test]
fn null_to_a_nullable_or_mixed_builtin_position_is_accepted() {
    silent_both_modes("array_filter([1], null);\n");
    silent_both_modes("var_dump(null);\n");
}

#[test]
fn null_to_a_builtin_scalar_position_keeps_the_carve_out() {
    // Kept shape: coercive deprecation, strict TypeError.
    silent(&coercive("strlen(null);\n"));
    fires(&strict("strlen(null);\n"), ARG);
}

#[test]
fn an_array_to_a_builtin_scalar_position_is_a_type_error() {
    fires_both_modes("strlen([]);\n", ARG);
}

#[test]
fn the_builtin_null_message_spells_the_reflected_type() {
    let src = coercive("iterator_to_array(null);\n");
    let ds = findings_with(&src, &mut Boot::ready());
    assert_eq!(ds.len(), 1);
    assert!(ds[0].message.contains("cannot become Traversable|array $iterator"), "{}", ds[0].message);
}

#[test]
fn the_top_message_spells_the_declared_top() {
    let src = strict(&format!("{FOO}foo(42);\n"));
    let ds = findings_with(&src, &mut Boot::ready());
    assert_eq!(ds.len(), 1);
    assert!(ds[0].message.contains("argument 42 to foo() cannot become callable $c"), "{}", ds[0].message);
}
