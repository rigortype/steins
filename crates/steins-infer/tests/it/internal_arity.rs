//! Issue #937 (slice S5 of #1051): arity of calls to INTERNAL functions, to engine
//! class methods and constructors, and of callbacks handed to invoking builtins.
//!
//! One test per row of the tables in `crates/steins-infer/src/arity/internal.rs` and
//! `arity/callbacks.rs`. Every PHP verdict was run on 8.5.11. Each case runs against a
//! **mock** folder whose signatures were read off `ReflectionFunction` on that
//! engine, and — where a `php` answers — against the **live** sidecar, so the table
//! cannot drift from the engine. Each case is also run on the sound subset
//! (`NoFold`), where it must be silent: the finding exists only because the engine
//! was asked.

use steins_infer::{
    CALL_TOO_FEW_ARGUMENTS_ID as TOO_FEW, CALL_TOO_MANY_ARGUMENTS_ID as TOO_MANY,
    CALL_UNKNOWN_NAMED_ARGUMENT_ID as UNKNOWN_NAMED, Diagnostic, Folder, NoFold, SidecarFolder,
    check_with,
};
use steins_sidecar::{
    BuiltinParam, ClassReflection, ReflectedClass, ReflectedClassKind, ReflectedMethod,
    Visibility,
};
use steins_syntax::{ArgValue, SourceTree};

// ---------------------------------------------------------------------------
// The mock engine
// ---------------------------------------------------------------------------

/// `"r:string o:flags v:rest"` as the wire carries a parameter list.
fn sig(spec: &str) -> Vec<BuiltinParam> {
    spec.split_whitespace()
        .map(|p| {
            let (kind, name) = p.split_once(':').expect("kind:name");
            BuiltinParam {
                name: name.to_owned(),
                ty: None,
                by_ref: false,
                variadic: kind == "v",
                optional: kind != "r",
            }
        })
        .collect()
}

/// The functions the fixtures touch, signatures as `ReflectionFunction` reports them.
fn functions() -> Vec<(&'static str, Vec<BuiltinParam>)> {
    [
        ("strlen", "r:string"),
        ("intdiv", "r:num1 r:num2"),
        ("str_replace", "r:search r:replace r:subject o:count"),
        ("max", "r:value v:values"),
        ("compact", "r:var_name v:var_names"),
        ("mktime", "r:hour o:minute o:second o:month o:day o:year"),
        ("var_dump", "r:value v:values"),
        ("array_keys", "r:array o:filter_value o:strict"),
        ("session_id", "o:id"),
        ("date_create", "o:datetime o:timezone"),
        ("array_merge", "v:arrays"),
        ("sprintf", "r:format v:values"),
        ("rand", "o:min o:max"),
        ("array_map", "r:callback r:array v:arrays"),
        ("array_filter", "r:array o:callback o:mode"),
        ("usort", "r:array r:callback"),
        ("uasort", "r:array r:callback"),
        ("uksort", "r:array r:callback"),
        ("array_walk", "r:array r:callback o:arg"),
        ("array_reduce", "r:array r:callback o:initial"),
        ("call_user_func", "r:callback v:args"),
        ("str_repeat", "r:string r:times"),
        ("str_pad", "r:string r:length o:pad_string o:pad_type"),
        ("preg_replace_callback", "r:pattern r:callback r:subject o:limit o:count o:flags"),
        ("strtoupper", "r:string"),
        ("gnupg_getprotocol", ""),
    ]
    .into_iter()
    .map(|(n, s)| (n, sig(s)))
    .collect()
}

fn method(name: &str, total: u32, required: u32, is_static: bool) -> ReflectedMethod {
    ReflectedMethod {
        name: name.to_owned(),
        is_static,
        is_abstract: false,
        is_final: false,
        visibility: Visibility::Public,
        params_total: total,
        params_required: required,
        return_type: None,
        return_type_tentative: false,
    }
}

fn class(
    name: &str,
    kind: ReflectedClassKind,
    is_abstract: bool,
    methods: Vec<ReflectedMethod>,
) -> ReflectedClass {
    ReflectedClass {
        name: name.to_owned(),
        kind,
        internal: true,
        extension: Some("date".to_owned()),
        is_final: false,
        is_abstract,
        parent: None,
        interfaces: vec![],
        methods,
        constants: vec![],
        properties: vec![],
    }
}

fn classes() -> Vec<ReflectedClass> {
    use ReflectedClassKind::{Class, Interface};
    let mut private_ctor = method("__construct", 0, 0, false);
    private_ctor.visibility = Visibility::Private;
    vec![
        class(
            "DateTime",
            Class,
            false,
            vec![
                method("__construct", 2, 0, false),
                method("format", 1, 1, false),
                method("createFromFormat", 3, 2, true),
            ],
        ),
        class("DateTimeZone", Class, false, vec![method("__construct", 1, 1, false)]),
        class("DatePeriod", Class, false, vec![method("__construct", 4, 1, false)]),
        class("Exception", Class, false, vec![method("__construct", 3, 0, false)]),
        class("Sealed", Class, false, vec![private_ctor]),
        class("AbstractThing", Class, true, vec![method("__construct", 1, 1, false)]),
        class("Countable", Interface, false, vec![]),
    ]
}

/// A folder answering reflection from the tables above; every other query is the
/// sound default. `available` is the absence-family gate: off, no reflection answers.
struct Mock {
    available: bool,
}

impl Folder for Mock {
    fn fold(&mut self, _name: &str, _args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        self.available
    }
    fn boot_surface_class_like(&mut self, _fqn: &str) -> Option<bool> {
        Some(false)
    }
    fn boot_surface_function(&mut self, _fqn: &str) -> Option<bool> {
        Some(false)
    }
    fn builtin_param_types(&mut self, name: &str) -> Option<Vec<BuiltinParam>> {
        if !self.available {
            return None;
        }
        functions().into_iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, p)| p)
    }
    fn reflected_class(&mut self, fqn: &str) -> Option<ClassReflection> {
        let declaration = classes().into_iter().find(|c| c.name.eq_ignore_ascii_case(fqn));
        Some(ClassReflection { target: fqn.to_owned(), declaration })
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

fn run_with(src: &str, folder: &mut dyn Folder) -> Vec<Diagnostic> {
    check_with(&SourceTree::parse(src), &[], "t.php", folder)
        .into_iter()
        .filter(|d| [TOO_FEW, TOO_MANY, UNKNOWN_NAMED].contains(&d.id))
        .collect()
}

/// The mock run, plus the live run when a `php` answers; both must agree.
fn engines(src: &str) -> Vec<(&'static str, Vec<Diagnostic>)> {
    let mut out = vec![("mock", run_with(src, &mut Mock { available: true }))];
    let mut live = SidecarFolder::enabled();
    if live.builtin_param_types("strlen").is_some() && live.absence_family_available() {
        out.push(("live", run_with(src, &mut live)));
    }
    out
}

/// The fixture fires exactly `id` once on every engine, with `needle` in the message,
/// and is silent on the sound subset and on a mock whose family gate is closed.
fn fires(src: &str, id: &str, needle: &str) {
    let src = format!("<?php\n{src}\n");
    assert!(run_with(&src, &mut NoFold).is_empty(), "silent without an engine:\n{src}");
    assert!(run_with(&src, &mut Mock { available: false }).is_empty(), "gate closed:\n{src}");
    for (which, ds) in engines(&src) {
        assert_eq!(ds.len(), 1, "{which}: expected one finding for:\n{src}\ngot {ds:?}");
        assert_eq!(ds[0].id, id, "{which}: {ds:?}");
        assert!(ds[0].message.contains(needle), "{which}: {:?} lacks `{needle}`", ds[0].message);
    }
}

fn too_few(src: &str, needle: &str) {
    fires(src, TOO_FEW, needle);
}

fn silent(src: &str) {
    let src = format!("<?php\n{src}\n");
    for (which, ds) in engines(&src) {
        assert!(ds.is_empty(), "{which}: expected silence for:\n{src}\ngot {ds:?}");
    }
}

// ---------------------------------------------------------------------------
// Internal functions: too few
// ---------------------------------------------------------------------------

#[test]
fn too_few_to_internal_functions() {
    too_few("strlen();", "strlen(): 0 passed, 1 required");
    too_few("intdiv(1);", "intdiv(): 1 passed, 2 required");
    too_few("str_replace('a', 'b');", "str_replace(): 2 passed, 3 required");
    too_few("max();", "max(): 0 passed, 1 required");
    too_few("compact();", "compact(): 0 passed, 1 required");
    too_few("mktime();", "mktime(): 0 passed, 1 required");
    too_few("var_dump();", "var_dump(): 0 passed, 1 required");
}

#[test]
fn a_named_argument_covers_a_required_parameter() {
    silent("intdiv(num1: 1, num2: 2);");
    too_few("intdiv(num1: 1);", "intdiv(): 1 passed, 2 required");
}

#[test]
fn the_global_function_is_judged_from_a_namespace_and_by_its_root_spelling() {
    too_few("namespace App;\nstrlen();", "strlen()");
    too_few("namespace App;\n\\strlen();", "strlen()");
}

// ---------------------------------------------------------------------------
// Internal functions: too many
// ---------------------------------------------------------------------------

#[test]
fn too_many_to_non_variadic_internal_functions() {
    fires("strlen('a', 'b');", TOO_MANY, "strlen(): 2 passed, at most 1 accepted");
    fires("array_keys([1], 1, true, 4);", TOO_MANY, "array_keys(): 4 passed, at most 3");
    fires("session_id(1, 2);", TOO_MANY, "session_id(): 2 passed, at most 1");
    fires("date_create('now', null, 1);", TOO_MANY, "date_create(): 3 passed, at most 2");
}

#[test]
fn a_variadic_takes_any_surplus_and_a_full_list_is_fine() {
    silent("array_merge();");
    silent("array_merge([1], [2], [3]);");
    silent("sprintf('x', 1, 2, 3);");
    silent("str_pad('a', 3);");
    silent("str_pad('a', 3, ' ', 1);");
}

#[test]
fn a_userland_function_keeps_the_userland_rule() {
    // Extras to a project function run clean, even one named like a builtin.
    silent("function strlen($a) {}\nstrlen('a', 'b');");
    silent("function two($a) {}\ntwo(1, 2, 3);");
}

#[test]
fn a_function_whose_reflected_arity_is_not_the_enforced_one_declines() {
    // The `gnupg` procedural wrappers reflect a maximum below what they take.
    silent("gnupg_getprotocol(1, 2);");
}

#[test]
fn rand_with_one_argument_is_a_known_false_negative() {
    // PHP throws; reflection says 0 required of 2, so the engine cannot say so.
    silent("rand(1);");
}

#[test]
fn argument_unpacking_and_a_first_class_callable_are_not_counted() {
    silent("$xs = [];\nstrlen(...$xs);");
    silent("$f = strlen(...);");
}

// ---------------------------------------------------------------------------
// Internal functions: named arguments
// ---------------------------------------------------------------------------

#[test]
fn an_unknown_named_argument_to_a_non_variadic_internal_function_is_an_error() {
    fires("strlen(str: 'a');", UNKNOWN_NAMED, "unknown named argument $str to strlen()");
}

#[test]
fn an_unknown_named_argument_to_a_variadic_declines() {
    // `call_user_func` forwards named extras; the family cannot tell them apart.
    silent("sprintf(format: 'x', foo: 1);");
    silent("call_user_func('strlen', string: 'a');");
}

#[test]
fn a_named_argument_over_a_positional_declines() {
    silent("strlen('a', string: 'b');");
}

// ---------------------------------------------------------------------------
// Engine methods and constructors
// ---------------------------------------------------------------------------

#[test]
fn too_few_to_an_engine_method() {
    too_few("$d = new DateTime();\n$d->format();", "DateTime::format(): 0 passed, 1 required");
    too_few("(new DateTime())->format();", "DateTime::format()");
    too_few("DateTime::createFromFormat('Y');", "DateTime::createFromFormat(): 1 passed, 2");
}

#[test]
fn too_few_to_an_engine_constructor() {
    too_few("new DateTimeZone();", "DateTimeZone::__construct(): 0 passed, 1 required");
}

#[test]
fn a_surplus_argument_to_an_engine_method_is_not_judged() {
    // `ReflectedMethod` carries no variadic flag, so the maximum is unknown.
    silent("new DateTime('now', null, 3);");
    silent("$d = new DateTime();\n$d->format('Y', 1);");
}

#[test]
fn an_engine_method_that_asks_nothing_is_fine() {
    silent("new DateTime();");
    silent("new Exception();");
    silent("$d = new DateTime();\n$d->format('Y');");
}

#[test]
fn engine_method_declines() {
    // A static call to a non-static method is its own Error, ahead of the count.
    silent("DateTime::format();");
    // A class the engine marks abstract or an interface cannot be instantiated.
    silent("new AbstractThing();");
    silent("new Countable();");
    // A non-public constructor is a visibility Error.
    silent("new Sealed(1);");
    // A project subclass belongs to the project arm; a declared parameter is a lower
    // bound that a subclass may widen.
    silent("class P extends DateTime {}\n$p = new P();\n$p->format();");
    silent("function f(DateTime $d) { $d->format(); }");
    // `DatePeriod` parses its overloaded constructor by hand: a `TypeError`, not the count.
    silent("new DatePeriod();");
    // A class the analysing PHP does not have.
    silent("new NoSuchEngineClass();");
}

// ---------------------------------------------------------------------------
// Callbacks
// ---------------------------------------------------------------------------

#[test]
fn array_map_passes_one_argument_per_array() {
    too_few("array_map(fn($a, $b) => 1, [1]);", "closure() as a callback: array_map() passes 1, 2");
    too_few(
        "array_map(function ($a, $b, $c) { return 1; }, [1], [2]);",
        "array_map() passes 2, 3 required",
    );
    silent("array_map(fn($a, $b) => 1, [1], [2]);");
}

#[test]
fn a_callback_with_fewer_parameters_defaults_or_a_variadic_is_fine() {
    silent("array_map(fn() => 1, [1]);");
    silent("array_map(fn($a) => 1, [1]);");
    silent("array_map(fn($a, $b = 1) => 1, [1]);");
    silent("array_map(fn($a, ...$rest) => 1, [1]);");
}

#[test]
fn a_callback_that_may_never_run_is_not_judged() {
    silent("array_map(fn($a, $b) => 1, []);");
    silent("array_filter([], fn($a, $b) => 1);");
    silent("$xs = unknown();\narray_map(fn($a, $b) => 1, $xs);");
    silent("$xs = [1];\n$xs[] = unknown();\n$xs = unknown();\narray_map(fn($a, $b) => 1, $xs);");
}

#[test]
fn array_filter_passes_by_its_mode() {
    too_few("array_filter([1], fn($a, $b) => 1);", "array_filter() passes 1, 2");
    too_few("array_filter([1], fn($a, $b) => 1, 2);", "array_filter() passes 1, 2");
    too_few("array_filter([1], fn($a, $b) => 1, ARRAY_FILTER_USE_KEY);", "passes 1, 2");
    silent("array_filter([1], fn($a, $b) => 1, 1);");
    silent("array_filter([1], fn($a, $b) => 1, ARRAY_FILTER_USE_BOTH);");
    // The mode is not a literal: the invoker argument count is unknown.
    silent("function mode() { return 1; }\narray_filter([1], fn($a, $b) => 1, mode());");
}

#[test]
fn call_user_func_always_runs_its_callback() {
    too_few("call_user_func(fn($a, $b) => 1, 1);", "call_user_func() passes 1, 2");
    too_few("call_user_func(fn($a) => 1);", "call_user_func() passes 0, 1");
    silent("call_user_func(fn($a, $b) => 1, 1, 2);");
    silent("call_user_func(fn($a, $b = 2) => 1, 1);");
}

#[test]
fn sorts_pass_two_arguments_and_need_two_elements() {
    too_few("$xs = [3, 1, 2];\nusort($xs, fn($a, $b, $c) => 0);", "usort() passes 2, 3");
    too_few("$xs = [3, 1];\nuasort($xs, fn($a, $b, $c) => 0);", "uasort() passes 2, 3");
    too_few("$xs = [3, 1];\nuksort($xs, fn($a, $b, $c) => 0);", "uksort() passes 2, 3");
    silent("$xs = [3];\nusort($xs, fn($a, $b, $c) => 0);");
    silent("$xs = [3, 1];\nusort($xs, fn($a, $b) => 0);");
}

#[test]
fn array_walk_passes_the_value_and_key_and_the_extra_argument() {
    too_few("$xs = [1];\narray_walk($xs, fn($v, $k, $extra) => 1);", "array_walk() passes 2, 3");
    silent("$xs = [1];\narray_walk($xs, fn($v, $k, $extra) => 1, 'x');");
    silent("$xs = [];\narray_walk($xs, fn($v, $k, $extra) => 1);");
}

#[test]
fn array_reduce_passes_two_arguments() {
    too_few("array_reduce([1], fn($c, $i, $x) => 1, 0);", "array_reduce() passes 2, 3");
    silent("array_reduce([1], fn($c, $i) => 1, 0);");
}

#[test]
fn preg_replace_callback_runs_the_callback_only_on_a_match() {
    // Whether the pattern matches is the project's own PCRE's to say: declined.
    silent("preg_replace_callback('/a/', fn($m, $n) => 'x', 'a');");
}

#[test]
fn a_callback_held_by_a_variable_is_judged_like_one_written_in_place() {
    too_few("$f = fn($a, $b) => 1;\narray_map($f, [1]);", "closure() as a callback");
    silent("$f = fn($a, $b) => 1;\n$f = unknown();\narray_map($f, [1]);");
}

#[test]
fn a_string_or_first_class_callable_names_a_function() {
    let add = "function add(int $a, int $b) { return $a + $b; }\n";
    too_few(&format!("{add}array_map('add', [1, 2, 3]);"), "add() as a callback");
    too_few(&format!("{add}array_map(add(...), [1]);"), "add() as a callback");
    too_few(&format!("{add}$f = 'add';\narray_map($f, [1]);"), "add() as a callback");
    silent(&format!("{add}array_map('add', [1], [2]);"));
    silent("function one(int $a) {}\narray_map('one', [1]);");
}

#[test]
fn an_internal_function_as_a_callback_is_judged_by_its_reflected_signature() {
    too_few("array_map('str_repeat', ['a']);", "str_repeat() as a callback: array_map() passes 1, 2");
    silent("array_map('strtoupper', ['a']);");
    // Too MANY arguments to an internal callee are not judged here.
    silent("$xs = ['a', 'b'];\narray_walk($xs, 'strtoupper');");
}

#[test]
fn callables_the_family_cannot_prove_decline() {
    silent("array_map([new DateTime(), 'format'], [1]);");
    silent("array_map('Foo::bar', [1]);");
    silent("array_map('no_such_function', [1]);");
    silent("array_map(null, [1], [2]);");
    silent("$cb = unknown();\narray_map($cb, [1]);");
}

#[test]
fn a_project_twin_of_the_invoker_is_not_the_engines_function() {
    silent("function array_map($cb, $xs) {}\narray_map(fn($a, $b) => 1, [1]);");
}
