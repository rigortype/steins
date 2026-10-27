//! Issue #1112 (slice S11 of run #1114): the `method_exists` fold answers a builtin node
//! of the class chain (the start class, a parent, an interface) from the sidecar's
//! reflected class instead of staying `Maybe` (ADR-0049 A28, ADR-0106 §2.7).
//!
//! Each case runs against a **mock** folder answering a hand-built table of reflected
//! classes (shaped as PHP 8.3 reports them) and, where a `php` answers, against the
//! **live** sidecar, so the table cannot drift from the engine. Every runtime claim is
//! `php -r`-witnessed. The same case on the sound subset (`NoFold`) keeps the body live.

use std::path::PathBuf;

use steins_db::{
    GoverningRoot, PhpTarget, PhpTargetSource, PluginFacts, Project, ProjectLayout, SourceFile,
    SteinsDatabase,
};
use steins_infer::{DEBUG_TYPE_ID, Folder, NoFold, SidecarFolder, check_project, check_with};
use steins_sidecar::{
    ClassReflection, ReflectedClass, ReflectedClassKind, ReflectedMethod, Visibility,
};
use steins_syntax::{ArgValue, SourceTree};

fn method(name: &str) -> ReflectedMethod {
    ReflectedMethod {
        name: name.to_owned(),
        is_static: false,
        is_abstract: false,
        is_final: false,
        visibility: Visibility::Public,
        params_total: 0,
        params_required: 0,
        return_type: None,
        return_type_tentative: false,
    }
}

fn reflected(name: &str, kind: ReflectedClassKind, ms: &[&str]) -> ReflectedClass {
    ReflectedClass {
        name: name.to_owned(),
        kind,
        internal: true,
        extension: Some("Core".to_owned()),
        is_final: false,
        is_abstract: false,
        parent: None,
        interfaces: vec![],
        methods: ms.iter().map(|m| method(m)).collect(),
        constants: vec![],
        properties: vec![],
    }
}

trait WithPrivate {
    fn with_private(self, ms: &[&str]) -> Self;
}

impl WithPrivate for ReflectedClass {
    fn with_private(mut self, ms: &[&str]) -> Self {
        let private = |m: &&str| ReflectedMethod { visibility: Visibility::Private, ..method(m) };
        self.methods.extend(ms.iter().map(private));
        self
    }
}

/// The slice of the engine the fixtures touch; `Resident` is a class that is not internal.
fn table() -> Vec<ReflectedClass> {
    use ReflectedClassKind::{Class, Interface};
    let mut resident = reflected("Resident", Class, &["foo"]);
    resident.internal = false;
    vec![
        reflected("Countable", Interface, &["count"]),
        reflected("ArrayObject", Class, &["count", "getArrayCopy", "offsetGet"]),
        reflected("Stringable", Interface, &["__toString"]),
        reflected("ReflectionMethod", Class, &["getName", "createFromMethodName"]),
        resident,
        reflected("Exception", Class, &["getMessage"]).with_private(&["__clone"]),
        reflected("IntlCalendar", Class, &["get"]).with_private(&["__construct"]),
    ]
}

struct Mock;

impl Folder for Mock {
    fn fold(&mut self, _: &str, _: &[ArgValue], _strict: bool) -> Option<ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        true
    }
    fn boot_surface_class_like(&mut self, fqn: &str) -> Option<bool> {
        Some(table().iter().any(|c| c.name.eq_ignore_ascii_case(fqn)))
    }
    fn boot_surface_function(&mut self, _: &str) -> Option<bool> {
        Some(false)
    }
    fn php_minor(&mut self) -> Option<(u16, u16)> {
        Some((8, 3))
    }
    fn reflected_class(&mut self, fqn: &str) -> Option<ClassReflection> {
        let declaration = table().into_iter().find(|c| c.name.eq_ignore_ascii_case(fqn));
        Some(ClassReflection { target: fqn.to_owned(), declaration })
    }
}

/// Whether the body `method_exists(<class>::class, '<m>')` guards is live, under `folder`.
fn live(folder: &mut dyn Folder, fixture: &str, class: &str, m: &str) -> usize {
    let src = format!(
        "<?php\n{fixture}\nfunction f(?string $n): int {{ if (method_exists({class}::class, '{m}')) \
         {{ \\PHPStan\\dumpType($n); return 1; }} return 0; }}\n"
    );
    let tree = SourceTree::parse(&src);
    check_with(&tree, &[], "t.php", folder).iter().filter(|d| d.id == DEBUG_TYPE_ID).count()
}

/// Run `case` over the mock, and over the live sidecar when a `php` answers.
fn engines(case: impl Fn(&mut dyn Folder, &str)) {
    case(&mut Mock, "mock");
    let mut sidecar = SidecarFolder::enabled();
    if sidecar.reflected_class("Countable").is_some_and(|r| r.exists()) {
        case(&mut sidecar, "live");
    }
}

#[test]
fn an_abstract_class_over_a_builtin_interface_folds_on_its_methods() {
    let fixture = "abstract class AbsB implements \\Countable {}";
    engines(|f, e| {
        // w1112 f1 / f2: PHP says `count` true and `nope` false.
        assert_eq!(live(f, fixture, "AbsB", "count"), 1, "{e}: count");
        assert_eq!(live(f, fixture, "AbsB", "nope"), 0, "{e}: nope");
        assert_eq!(live(f, fixture, "AbsB", "COUNT"), 1, "{e}: case-insensitive");
    });
    assert_eq!(live(&mut NoFold, fixture, "AbsB", "nope"), 1, "sound subset stays Maybe");
}

#[test]
fn a_project_class_over_a_builtin_parent_folds_on_the_parent_methods() {
    let fixture = "class X extends \\ArrayObject { public function own(): int { return 1; } }
class Y extends X {}
abstract class A2 extends \\ArrayObject implements \\Countable {}";
    engines(|f, e| {
        assert_eq!(live(f, fixture, "X", "count"), 1, "{e}: inherited");
        assert_eq!(live(f, fixture, "X", "nope"), 0, "{e}: absent");
        assert_eq!(live(f, fixture, "X", "own"), 1, "{e}: own");
        assert_eq!(live(f, fixture, "Y", "getArrayCopy"), 1, "{e}: two project hops");
        assert_eq!(live(f, fixture, "Y", "nope"), 0, "{e}: two project hops, absent");
        assert_eq!(live(f, fixture, "A2", "nope"), 0, "{e}: parent and interface");
    });
    assert_eq!(live(&mut NoFold, fixture, "X", "nope"), 1, "sound subset stays Maybe");
}

#[test]
fn a_builtin_start_class_folds_on_its_methods() {
    engines(|f, e| {
        assert_eq!(live(f, "", "\\ArrayObject", "count"), 1, "{e}");
        assert_eq!(live(f, "", "\\ArrayObject", "nope"), 0, "{e}");
        assert_eq!(live(f, "", "\\Countable", "count"), 1, "{e}: interface");
        assert_eq!(live(f, "", "\\Countable", "nope"), 0, "{e}: interface");
    });
}

#[test]
fn a_project_interface_over_a_builtin_interface_walks_both() {
    let fixture = "interface Ci extends \\Countable { public function m(): int; }
abstract class AbsCi implements Ci {}";
    engines(|f, e| {
        assert_eq!(live(f, fixture, "AbsCi", "count"), 1, "{e}");
        assert_eq!(live(f, fixture, "AbsCi", "m"), 1, "{e}");
        assert_eq!(live(f, fixture, "AbsCi", "nope"), 0, "{e}");
        assert_eq!(live(f, fixture, "Ci", "count"), 1, "{e}: the interface itself");
    });
}

#[test]
fn what_the_engine_cannot_answer_stays_maybe() {
    // A vendor parent and interface (not resident: `reflect_class` never autoloads), a
    // trait named by `extends`, and an ambiguous name.
    let vendor = "class V extends \\Vendor\\Base {} abstract class VA implements \\Vendor\\Iface {}";
    engines(|f, e| {
        assert_eq!(live(f, vendor, "V", "nope"), 1, "{e}: vendor parent");
        assert_eq!(live(f, vendor, "VA", "nope"), 1, "{e}: vendor interface");
        assert_eq!(live(f, "", "\\Vendor\\Base", "nope"), 1, "{e}: vendor start");
        let two = "class Dup {} class Dup {} class D2 extends Dup {}";
        assert_eq!(live(f, two, "D2", "nope"), 1, "{e}: ambiguous parent");
        let tr = "trait T0 {} class UsesTraitName extends T0 {}";
        assert_eq!(live(f, tr, "UsesTraitName", "nope"), 1, "{e}: a trait is no chain node");
    });
    // A resident class that is not internal is not trusted (mock only).
    let own = "class R extends \\Resident {}";
    assert_eq!(live(&mut Mock, own, "R", "foo"), 1, "a non-internal reflected class");
    assert_eq!(live(&mut Mock, own, "R", "nope"), 1, "a non-internal reflected class");
}

#[test]
fn a_walk_that_reflects_nothing_is_unchanged() {
    // Project-only chains fold as before, with or without an engine.
    let fixture = "class P1 { public function m(): int { return 1; } } class P2 extends P1 {}";
    engines(|f, e| {
        assert_eq!(live(f, fixture, "P2", "m"), 1, "{e}");
        assert_eq!(live(f, fixture, "P2", "nope"), 0, "{e}");
    });
}

#[test]
fn a_private_method_of_a_builtin_ancestor_is_not_the_subclass_s() {
    // PHP: `method_exists(AppEx::class, '__clone')` is false (the private method of a
    // builtin parent), while `method_exists('Exception', '__clone')` is true.
    let fixture = "abstract class AppEx extends \\Exception {}";
    engines(|f, e| {
        assert_eq!(live(f, fixture, "AppEx", "__clone"), 0, "{e}: parent");
        assert_eq!(live(f, fixture, "AppEx", "getMessage"), 1, "{e}: parent, public");
        assert_eq!(live(f, "", "\\Exception", "__clone"), 1, "{e}: start");
    });
    // The Intl shapes need the extension: mock only.
    let cal = "class MyCal extends \\IntlCalendar {}";
    assert_eq!(live(&mut Mock, cal, "MyCal", "__construct"), 0, "parent");
    assert_eq!(live(&mut Mock, "", "\\IntlCalendar", "__construct"), 1, "start");
}

/// Whether the body `method_exists(<class>::class, '<m>')` guards is live in a project
/// that declares `target` (`None`: no declared target), under the mock engine on PHP 8.3.
fn live_under(target: Option<PhpTarget>, class: &str, m: &str) -> usize {
    live_declaring(target, false, class, m)
}

/// [`live_under`] with `unreadable`: the manifest declares a PHP version no target was read from.
fn live_declaring(target: Option<PhpTarget>, unreadable: bool, class: &str, m: &str) -> usize {
    let (class, not) = (class.trim_start_matches('!'), if class.starts_with('!') { "!" } else { "" });
    let src = format!(
        "<?php\nfunction f(?string $n): int {{ if ({not}method_exists({class}::class, '{m}')) \
         {{ \\PHPStan\\dumpType($n); return 1; }} return 0; }}\n"
    );
    let root = GoverningRoot::new(
        PathBuf::from("/proj/composer.json"),
        PathBuf::from("/proj"),
        vec![PathBuf::from("/proj/vendor")],
        vec![],
    )
    .with_php_target(target)
    .with_php_target_unreadable(unreadable);
    let layout = ProjectLayout::new(PathBuf::from("/proj"), vec![root]);
    let db = SteinsDatabase::default();
    let file = SourceFile::new(&db, "/proj/t.php".to_owned(), src);
    let project = Project::new(&db, vec![file], layout, PluginFacts::none());
    let found = check_project(&db, project, &mut Mock);
    found.iter().filter(|d| d.id == DEBUG_TYPE_ID).count()
}

fn require(raw: &str, floor: (u16, u16), ceiling: Option<(u16, u16)>) -> PhpTarget {
    PhpTarget { floor, ceiling, source: PhpTargetSource::Require, raw: raw.to_owned() }
}

/// The reflection is the sidecar's PHP (8.3 here), so it answers only when the project's
/// declared target is that minor alone: `createFromMethodName` is 8.3+, and a `>=8.1`
/// project may run on 8.1.
#[test]
fn a_wider_declared_target_than_the_sidecar_minor_stays_maybe() {
    let (rm, abs) = ("\\ReflectionMethod", "\\ArrayObject");
    // No declared target and one pinned to the sidecar's minor fold.
    assert_eq!(live_under(None, rm, "createFromMethodName"), 1);
    assert_eq!(live_under(None, "!\\ReflectionMethod", "createFromMethodName"), 0);
    assert_eq!(live_under(None, abs, "nope"), 0);
    let pinned = require("8.3.*", (8, 3), Some((8, 3)));
    assert_eq!(live_under(Some(pinned.clone()), rm, "createFromMethodName"), 1);
    assert_eq!(live_under(Some(pinned), abs, "nope"), 0);
    // A range that includes other minors, below or above, does not.
    for raw in [">=8.1", "^8.1", "8.1 - 8.4"] {
        let (floor, ceiling) = if raw.starts_with(">=") { ((8, 1), None) } else { ((8, 1), Some((8, 4))) };
        let wide = require(raw, floor, ceiling);
        assert_eq!(live_under(Some(wide.clone()), abs, "nope"), 1, "{raw}: absent");
        let negated = "!\\ReflectionMethod";
        assert_eq!(live_under(Some(wide), negated, "createFromMethodName"), 1, "{raw}: present");
    }
}

/// `">= 8.1"` and `"^8.1 | ^8.2"` are valid Composer constraints the target parser cannot
/// read: the project declared a target, so the reflection stays out (the sidecar is 8.3).
#[test]
fn a_declared_but_unreadable_target_stays_maybe() {
    let (rm, not_rm, abs) = ("\\ReflectionMethod", "!\\ReflectionMethod", "\\ArrayObject");
    assert_eq!(live_declaring(None, true, abs, "nope"), 1);
    assert_eq!(live_declaring(None, true, rm, "createFromMethodName"), 1);
    assert_eq!(live_declaring(None, true, not_rm, "createFromMethodName"), 1);
    // Undeclared (a manifest without `php`) still folds.
    assert_eq!(live_declaring(None, false, abs, "nope"), 0);
    assert_eq!(live_declaring(None, false, not_rm, "createFromMethodName"), 0);
}
