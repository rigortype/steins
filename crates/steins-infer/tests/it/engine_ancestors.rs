//! Issue #939 (slice S6 of #1051): the declaration-fatal family follows ENGINE
//! ancestors through `Folder::reflected_class`.
//!
//! Every runtime claim is `php -r`-witnessed on PHP 8.5.11. Each case runs twice:
//! against a **mock** folder answering a hand-built table of reflected classes
//! (shaped as `ReflectionClass` reports them on 8.5), and — where a `php` answers —
//! against the **live** sidecar, so the table cannot drift from the engine. A case
//! is also run on the sound subset (`NoFold`), where it must be silent: the finding
//! exists only because the engine was asked.

use steins_infer::{Diagnostic, Folder, NoFold, SidecarFolder, check_with};
use steins_sidecar::{
    ClassReflection, ReflectedClass, ReflectedClassKind, ReflectedMethod, Visibility,
};
use steins_syntax::{ArgValue, SourceTree};

const ABSTRACT_ID: &str = "class.abstract-unimplemented";
const FINAL_CLASS_ID: &str = "class.extends-final";
const FINAL_METHOD_ID: &str = "override.final";
const VISIBILITY_ID: &str = "override.visibility-weakened";
const STATIC_ID: &str = "override.static-mismatch";
const PARAM_ID: &str = "override.parameter-variance";
const RETURN_ID: &str = "override.return-variance";

// ---------------------------------------------------------------------------
// The mock engine
// ---------------------------------------------------------------------------

fn method(name: &str, total: u32, required: u32) -> ReflectedMethod {
    ReflectedMethod {
        name: name.to_owned(),
        is_static: false,
        is_abstract: false,
        is_final: false,
        visibility: Visibility::Public,
        params_total: total,
        params_required: required,
        return_type: None,
        return_type_tentative: false,
    }
}

fn abstract_method(name: &str, total: u32, required: u32) -> ReflectedMethod {
    ReflectedMethod { is_abstract: true, ..method(name, total, required) }
}

fn tentative(mut m: ReflectedMethod, ty: &str) -> ReflectedMethod {
    m.return_type = Some(ty.to_owned());
    m.return_type_tentative = true;
    m
}

fn class(
    name: &str,
    kind: ReflectedClassKind,
    parent: Option<&str>,
    interfaces: &[&str],
    methods: Vec<ReflectedMethod>,
) -> ReflectedClass {
    ReflectedClass {
        name: name.to_owned(),
        kind,
        internal: true,
        extension: Some("Core".to_owned()),
        is_final: false,
        is_abstract: false,
        parent: parent.map(str::to_owned),
        interfaces: interfaces.iter().map(|s| (*s).to_owned()).collect(),
        methods,
        constants: vec![],
        properties: vec![],
    }
}

fn iface(name: &str, extends: &[&str], methods: Vec<ReflectedMethod>) -> ReflectedClass {
    class(name, ReflectedClassKind::Interface, None, extends, methods)
}

/// The slice of PHP 8.5's engine the fixtures touch.
fn table() -> Vec<ReflectedClass> {
    use ReflectedClassKind::Class;
    let count = tentative(method("count", 0, 0), "int");
    let mut closure = class("Closure", Class, None, &[], vec![method("bind", 3, 2)]);
    closure.is_final = true;
    let mut get_message = method("getMessage", 0, 0);
    get_message.is_final = true;
    let mut filter_accept = abstract_method("accept", 0, 0);
    filter_accept.return_type = Some("bool".to_owned());
    vec![
        iface("Traversable", &[], vec![]),
        iface("Throwable", &["Stringable", "Traversable"], vec![]),
        iface("Stringable", &[], vec![abstract_method("__toString", 0, 0)]),
        iface("DateTimeInterface", &[], vec![]),
        iface("UnitEnum", &[], vec![]),
        iface("Countable", &[], vec![ReflectedMethod { is_abstract: true, ..count.clone() }]),
        iface("JsonSerializable", &[], vec![abstract_method("jsonSerialize", 0, 0)]),
        iface(
            "IteratorAggregate",
            &["Traversable"],
            vec![tentative(abstract_method("getIterator", 0, 0), "Traversable")],
        ),
        iface(
            "Iterator",
            &["Traversable"],
            ["current", "next", "key", "valid", "rewind"]
                .iter()
                .map(|n| abstract_method(n, 0, 0))
                .collect(),
        ),
        iface(
            "ArrayAccess",
            &[],
            vec![
                tentative(abstract_method("offsetExists", 1, 1), "bool"),
                tentative(abstract_method("offsetGet", 1, 1), "mixed"),
                tentative(abstract_method("offsetSet", 2, 2), "void"),
                tentative(abstract_method("offsetUnset", 1, 1), "void"),
            ],
        ),
        class(
            "ArrayIterator",
            Class,
            None,
            &["Iterator", "Traversable", "ArrayAccess", "Countable"],
            vec![count.clone(), tentative(method("seek", 1, 1), "void")],
        ),
        class("ArrayObject", Class, None, &["IteratorAggregate", "Countable"], vec![count.clone()]),
        class("SplObjectStorage", Class, None, &["Countable"], vec![count.clone()]),
        class(
            "Exception",
            Class,
            None,
            &["Throwable", "Stringable", "Traversable"],
            vec![get_message, method("__construct", 3, 0), method("__toString", 0, 0)],
        ),
        class(
            "IteratorIterator",
            Class,
            None,
            &["Iterator", "Traversable"],
            vec![method("current", 0, 0)],
        ),
        class(
            "FilterIterator",
            Class,
            Some("IteratorIterator"),
            &["Iterator", "Traversable"],
            vec![filter_accept.clone(), method("current", 0, 0)],
        ),
        class(
            "RecursiveFilterIterator",
            Class,
            Some("FilterIterator"),
            &["Iterator", "Traversable", "RecursiveIterator"],
            vec![filter_accept, method("current", 0, 0)],
        ),
        closure,
    ]
}

/// A folder answering reflection from [`table`]; every other query is the sound default.
struct Mock {
    available: bool,
    classes: Vec<ReflectedClass>,
    asked: Vec<String>,
}

impl Mock {
    fn new() -> Mock {
        Mock { available: true, classes: table(), asked: vec![] }
    }
}

impl Folder for Mock {
    fn fold(&mut self, _name: &str, _args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        self.available
    }
    fn reflected_class(&mut self, fqn: &str) -> Option<ClassReflection> {
        self.asked.push(fqn.to_owned());
        let declaration =
            self.classes.iter().find(|c| c.name.eq_ignore_ascii_case(fqn)).cloned();
        Some(ClassReflection { target: fqn.to_owned(), declaration })
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

fn run_with(src: &str, folder: &mut dyn Folder) -> Vec<Diagnostic> {
    let all = check_with(&SourceTree::parse(src), &[], "t.php", folder);
    all.into_iter()
        .filter(|d| {
            [ABSTRACT_ID, FINAL_CLASS_ID, FINAL_METHOD_ID, VISIBILITY_ID, STATIC_ID, PARAM_ID, RETURN_ID]
                .contains(&d.id)
        })
        .collect()
}

/// The mock run, plus the live run when a `php` answers; both must agree.
fn engines(src: &str) -> Vec<(&'static str, Vec<Diagnostic>)> {
    let mut out = vec![("mock", run_with(src, &mut Mock::new()))];
    let mut live = SidecarFolder::enabled();
    if live.reflected_class("Steins\\Probe939").is_some() && live.absence_family_available() {
        out.push(("live", run_with(src, &mut live)));
    }
    out
}

/// The fixture fires exactly `id` once on every engine, with `needle` in the message,
/// and is silent on the sound subset.
fn fires(src: &str, id: &str, needle: &str) {
    let src = format!("<?php\n{src}\n");
    assert!(run_with(&src, &mut NoFold).is_empty(), "silent without an engine:\n{src}");
    for (which, ds) in engines(&src) {
        assert_eq!(ds.len(), 1, "{which}: expected one finding for:\n{src}\ngot {ds:?}");
        assert_eq!(ds[0].id, id, "{which}: {ds:?}");
        assert!(ds[0].message.contains(needle), "{which}: {:?} lacks `{needle}`", ds[0].message);
    }
}

fn silent(src: &str) {
    let src = format!("<?php\n{src}\n");
    for (which, ds) in engines(&src) {
        assert!(ds.is_empty(), "{which}: expected silence for:\n{src}\ngot {ds:?}");
    }
}

// ---------------------------------------------------------------------------
// The #939 witnesses
// ---------------------------------------------------------------------------

#[test]
fn implementing_countable_without_count_is_unimplemented() {
    fires("class Y implements Countable {}", ABSTRACT_ID, "(Countable::count)");
}

#[test]
fn extending_filter_iterator_without_accept_is_unimplemented() {
    fires("class F extends FilterIterator {}", ABSTRACT_ID, "(FilterIterator::accept)");
}

#[test]
fn the_declarer_is_the_class_that_declares_not_the_one_reflected() {
    fires("class R extends RecursiveFilterIterator {}", ABSTRACT_ID, "(FilterIterator::accept)");
}

#[test]
fn overriding_an_engine_final_method_is_final() {
    fires(
        "class E extends Exception { public function getMessage(): string { return 'x'; } }",
        FINAL_METHOD_ID,
        "Exception::getMessage()",
    );
}

#[test]
fn extending_an_engine_final_class_is_extends_final() {
    fires("class C extends Closure {}", FINAL_CLASS_ID, "final class Closure");
    fires("$x = new class extends Closure {};", FINAL_CLASS_ID, "final class Closure");
}

// ---------------------------------------------------------------------------
// The rest of the fatal surface, against real engine classes
// ---------------------------------------------------------------------------

#[test]
fn an_abstract_project_class_passes_an_engine_interface_down() {
    fires(
        "abstract class A implements Countable {}\nclass B extends A {}",
        ABSTRACT_ID,
        "class B leaves 1",
    );
}

#[test]
fn a_project_interface_extending_an_engine_interface_carries_its_requirements() {
    fires("interface I extends Countable {}\nclass C implements I {}", ABSTRACT_ID, "Countable::count");
}

#[test]
fn json_serializable_and_iterator_aggregate_are_requirements() {
    fires("class C implements JsonSerializable {}", ABSTRACT_ID, "JsonSerializable::jsonSerialize");
    fires("class C implements IteratorAggregate {}", ABSTRACT_ID, "IteratorAggregate::getIterator");
}

#[test]
fn a_concrete_reflected_method_discharges_the_requirement() {
    silent("class C extends ArrayIterator implements Countable {}");
    silent("class A extends FilterIterator { public function accept(): bool { return true; } }");
    silent("class C implements Countable { public function count(): int { return 1; } }");
    silent("abstract class C implements Countable {}");
}

#[test]
fn a_final_method_reached_through_a_project_class_is_final() {
    fires(
        "class A extends Exception {}\nclass B extends A { public function getMessage(): string { return ''; } }",
        FINAL_METHOD_ID,
        "Exception::getMessage()",
    );
}

#[test]
fn narrowing_an_engine_methods_visibility_is_a_fatal() {
    fires(
        "class C extends SplObjectStorage { protected function count(): int { return 1; } }",
        VISIBILITY_ID,
        "from public to protected",
    );
}

#[test]
fn making_an_engine_method_static_is_a_fatal() {
    fires(
        "class C extends ArrayIterator { public static function count(): int { return 1; } }",
        STATIC_ID,
        "ArrayIterator::count()",
    );
}

#[test]
fn an_arity_incompatible_override_of_an_engine_method_is_a_fatal() {
    fires(
        "class C extends ArrayIterator { public function count($x): int { return 1; } }",
        PARAM_ID,
        "requires 1 parameters where the engine's declaration requires 0",
    );
    fires(
        "class C extends ArrayIterator { public function seek(): void {} }",
        PARAM_ID,
        "accepts 0 parameters where the engine's declaration accepts 1",
    );
    fires(
        "class C extends ArrayIterator { public function seek(int $a, int $b): void {} }",
        PARAM_ID,
        "requires 2 parameters",
    );
    fires(
        "class C implements JsonSerializable { public function jsonSerialize(int $x) {} }",
        PARAM_ID,
        "JsonSerializable::jsonSerialize()",
    );
}

#[test]
fn compatible_engine_overrides_stay_silent() {
    silent("class C extends ArrayIterator { public function count(int $x = 1): int { return 1; } }");
    silent("class C extends ArrayIterator { public function seek(int ...$a): void {} }");
    silent("class C extends ArrayIterator { public function seek(int $o, int $extra = 0): void {} }");
    // The constructor is exempt from variance while the engine's is concrete.
    silent("class C extends Exception { public function __construct(int $a, int $b, int $c, int $d) {} }");
}

// ---------------------------------------------------------------------------
// Refusals and silences
// ---------------------------------------------------------------------------

#[test]
fn the_refusal_list_is_declined_not_judged() {
    // Each is its own fatal on PHP 8.5 (e1–e4, e9, e10), none this pass's ids.
    silent("class C implements Throwable {}");
    silent("class C implements Traversable {}");
    silent("class C implements DateTimeInterface {}");
    silent("class C implements UnitEnum {}");
    silent("class C implements Iterator, IteratorAggregate {}");
    silent("class C extends Countable {}");
    silent("interface I extends Traversable {}\nclass C implements I {}");
}

#[test]
fn a_tentative_return_mismatch_is_a_deprecation_and_stays_silent() {
    // `count(): string` over `ArrayObject::count(): int`, an untyped `getIterator()`,
    // an untyped `offsetGet()`: Deprecated on 8.5, loads, reaches `set_error_handler`.
    silent("class C extends ArrayObject { public function count(): string { return '1'; } }");
    silent("class C implements IteratorAggregate { public function getIterator() {} }");
    silent(
        "class C implements ArrayAccess { public function offsetExists($o) {} public function offsetGet($o) {}
         public function offsetSet($o, $v) {} public function offsetUnset($o) {} }",
    );
    for (_, ds) in engines(
        "<?php\nclass C extends ArrayObject { public function count(): string { return '1'; } }\n",
    ) {
        assert!(ds.iter().all(|d| d.id != RETURN_ID), "{ds:?}");
    }
}

#[test]
fn an_engine_class_the_running_php_lacks_is_silent() {
    silent("class C extends Steins\\NoSuchEngineClass939 {}");
    silent("class C implements Steins\\NoSuchEngineInterface939 {}");
}

#[test]
fn without_the_absence_family_the_pass_is_silent() {
    let src = "<?php\nclass C extends Closure {}\nclass Y implements Countable {}\n";
    let mut off = Mock::new();
    off.available = false;
    assert!(run_with(src, &mut off).is_empty());
    assert!(off.asked.is_empty(), "a closed gate asks the engine nothing: {:?}", off.asked);
}

#[test]
fn a_user_land_class_the_engine_reports_is_not_an_engine_ancestor() {
    let mut mock = Mock::new();
    for c in &mut mock.classes {
        if c.name == "Closure" {
            c.internal = false;
        }
    }
    assert!(run_with("<?php\nclass C extends Closure {}\n", &mut mock).is_empty());
}

#[test]
fn a_project_class_shadows_the_engine_name() {
    // The project declares its own `Countable`: the project declaration answers, the
    // engine is never asked.
    let src = "<?php\ninterface Countable {}\nclass Y implements Countable {}\n";
    let mut mock = Mock::new();
    assert!(run_with(src, &mut mock).is_empty());
    assert!(mock.asked.iter().all(|a| !a.eq_ignore_ascii_case("Countable")), "{:?}", mock.asked);
}
