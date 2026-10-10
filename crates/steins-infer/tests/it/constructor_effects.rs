//! `new C(...)` runs `C`'s constructor, so its effects are the caller's (issue
//! #804). A `new` is an effect edge to the constructor it resolves to, an
//! inherited one included; a class with no constructor contributes nothing; a
//! class the analyzer cannot name or resolve leaves the caller non-exhaustive
//! (`…?`), never silently pure. An engine class answers from the catalog's
//! `__construct` row, and one without a row taints like any unknown class.

use steins_infer::{Diagnostic, EFFECT_ID, EffectSummary, check, effect_summary};
use steins_syntax::SourceTree;

fn exceeded(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php").into_iter().filter(|d| d.id == EFFECT_ID).collect()
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

/// The issue's own example, with `Clock` declared ahead of `make`.
const CLOCK: &str = "<?php
final class Clock
{
    public int $at;

    public function __construct()
    {
        $this->at = time();
    }
}
";

// ---------------------------------------------------------------------------
// A project constructor is an edge
// ---------------------------------------------------------------------------

#[test]
fn the_issue_example_reaches_the_pure_caller() {
    let src = format!("{CLOCK}\n/** @pure */\nfunction make(): Clock\n{{\n    return new Clock();\n}}\n");
    let d = one(&src);
    assert_eq!(
        d.message,
        "Clock::__construct() has effect nondet.time (via time at line 8), \
         but make() is declared @phpstan-pure"
    );
    assert_eq!(d.line, 15, "reported at the `new`");
    let s = summary(&src, "make");
    assert_eq!(s.labels, ["nondet.time"]);
    assert!(s.exhaustive, "the constructor resolved, so nothing is unknown");
}

#[test]
fn an_inherited_constructor_is_the_one_new_runs() {
    let src = "<?php\nclass Base { public function __construct() { echo 'hi'; } }\n\
               final class Child extends Base {}\n\
               #[\\Steins\\Pure]\nfunction make(): Child { return new Child(); }\n";
    let d = one(src);
    let expected = "Base::__construct() has effect io.output.buffer";
    assert!(d.message.starts_with(expected), "{}", d.message);
}

#[test]
fn an_initializing_constructor_keeps_the_caller_pure_and_exhaustive() {
    // ADR-0055's creation exemption: writes to `$this` in `__construct` are
    // initialization, so building a value object is pure all the way out.
    let src = "<?php\nfinal class Money { public function __construct(public int $a) { $this->a = $a; } }\n\
               #[\\Steins\\Pure]\nfunction make(int $a): Money { return new Money($a); }\n";
    silent(src);
    let s = summary(src, "make");
    assert!(s.labels.is_empty() && s.exhaustive, "{s:?}");
}

#[test]
fn a_class_without_a_constructor_contributes_nothing() {
    let src = "<?php\nclass Base {}\nclass Point extends Base { public int $x = 0; }\n\
               function make(): Point { return new Point; }\n";
    let s = summary(src, "make");
    assert!(s.labels.is_empty() && s.exhaustive, "{s:?}");
}

#[test]
fn a_constructor_under_its_own_envelope_still_carries_its_effect_out() {
    // The constructor's envelope admits its effect, and a caller's wider one
    // admits it too; a pure caller's does not.
    let src = "<?php\nfinal class Clock {\n    #[\\Steins\\Effect('nondet.time')]\n    \
               public function __construct() { $this->at = time(); }\n    public int $at;\n}\n\
               #[\\Steins\\Effect('nondet')]\nfunction wide(): Clock { return new Clock(); }\n\
               #[\\Steins\\Pure]\nfunction narrow(): Clock { return new Clock(); }\n";
    let d = one(src);
    assert!(d.message.contains("narrow() is declared"), "{}", d.message);
}

#[test]
fn the_arguments_are_still_the_callers_own() {
    let src = "<?php\nfinal class Box { public function __construct(public int $v) {} }\n\
               #[\\Steins\\Pure]\nfunction make(): Box { return new Box(rand()); }\n";
    let d = one(src);
    assert!(d.message.starts_with("rand() has effect nondet.random"), "{}", d.message);
}

// ---------------------------------------------------------------------------
// `self`, `static`, `parent`
// ---------------------------------------------------------------------------

#[test]
fn new_self_and_new_parent_resolve_exactly() {
    let src = "<?php\nclass Base { public function __construct() { echo 'x'; } }\n\
               class Factory extends Base {\n    public function __construct() {}\n    \
               #[\\Steins\\Pure]\n    public static function me(): self { return new self(); }\n    \
               #[\\Steins\\Pure]\n    public static function up(): Base { return new parent(); }\n}\n";
    let d = one(src);
    assert!(d.message.starts_with("Base::__construct() has effect io.output.buffer"), "{}", d.message);
    assert!(d.message.contains("up() is declared"), "{}", d.message);
    assert!(summary(src, "Factory::me").exhaustive, "`self` is exactly `Factory`");
    assert!(summary(src, "Factory::up").exhaustive, "`parent` is exactly `Base`");
}

#[test]
fn new_static_resolves_only_where_no_subclass_can_replace_the_constructor() {
    let open = "<?php\nclass Node {\n    public function __construct() {}\n    \
                public static function make(): static { return new static(); }\n}\n";
    assert!(!summary(open, "Node::make").exhaustive, "a subclass may bring its own constructor");
    let sealed = "<?php\nfinal class Node {\n    public function __construct() {}\n    \
                  public static function make(): static { return new static(); }\n}\n";
    assert!(summary(sealed, "Node::make").exhaustive, "a final class has no subclass");
    let pinned = "<?php\nclass Node {\n    final public function __construct() {}\n    \
                  public static function make(): static { return new static(); }\n}\n";
    assert!(summary(pinned, "Node::make").exhaustive, "a final constructor is every subclass's");
}

// ---------------------------------------------------------------------------
// What cannot be resolved is `…?`, never `{}`
// ---------------------------------------------------------------------------

#[test]
fn a_dynamic_class_marks_the_caller_non_exhaustive() {
    let src = "<?php\nfunction make(string $cls): object { return new $cls(); }\n";
    let s = summary(src, "make");
    assert!(s.labels.is_empty() && !s.exhaustive, "{s:?}");
}

#[test]
fn an_undeclared_class_marks_the_caller_non_exhaustive_and_reports_nothing() {
    let src = "<?php\nnamespace App;\n#[\\Steins\\Pure]\nfunction make(): object { return new Missing(); }\n";
    silent(src);
    assert!(!summary(src, "make").exhaustive);
}

#[test]
fn a_chain_leaving_the_project_at_an_unknown_class_is_non_exhaustive() {
    let src = "<?php\nnamespace App;\nclass Local extends \\Vendor\\Base {}\n\
               function make(): Local { return new Local(); }\n";
    assert!(!summary(src, "make").exhaustive);
}

#[test]
fn a_trait_using_class_may_get_its_constructor_from_the_trait() {
    let src = "<?php\ntrait T {}\nclass C { use T; }\nfunction make(): C { return new C(); }\n";
    assert!(!summary(src, "make").exhaustive);
}

// ---------------------------------------------------------------------------
// Engine classes answer from the catalog
// ---------------------------------------------------------------------------

#[test]
fn an_engine_exception_constructs_purely() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction fail(): never { throw new \\InvalidArgumentException('no'); }\n";
    silent(src);
    assert!(summary(src, "fail").exhaustive);
}

#[test]
fn a_project_exception_inherits_the_engine_constructor() {
    let bare = "<?php\nnamespace App;\nclass Oops extends \\RuntimeException {}\n\
                function fail(): never { throw new Oops('no'); }\n";
    assert!(summary(bare, "fail").exhaustive);
    // Its own constructor forwarding to the engine's is resolved the same way.
    let forwarding = "<?php\nnamespace App;\nclass Oops extends \\RuntimeException {\n    \
                      public function __construct(string $m) { parent::__construct($m, 7); }\n}\n\
                      function fail(): never { throw new Oops('no'); }\n";
    assert!(summary(forwarding, "Oops::__construct").exhaustive);
    assert!(summary(forwarding, "fail").exhaustive);
}

#[test]
fn a_datetime_reads_the_zone_and_the_clock() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction now(): \\DateTimeImmutable { return new \\DateTimeImmutable(); }\n";
    let f = exceeded(src);
    let messages: Vec<&str> = f.iter().map(|d| d.message.as_str()).collect();
    let pure = "but now() is declared #[\\Steins\\Pure]";
    assert_eq!(
        messages,
        [
            format!("new DateTimeImmutable has effect global.read.setting.timezone, {pure}"),
            format!("new DateTimeImmutable has effect nondet.time, {pure}"),
        ]
    );
    let s = summary(src, "now");
    assert_eq!(s.labels, ["global.read.setting.timezone", "nondet.time"]);
    assert!(s.exhaustive);
}

#[test]
fn a_subclass_forwarding_to_datetime_carries_the_zone_and_the_clock() {
    let src = "<?php\nclass Moment extends \\DateTime {\n    \
               public function __construct() { parent::__construct('now'); }\n}\n\
               #[\\Steins\\Pure]\nfunction now(): Moment { return new Moment(); }\n";
    let f = exceeded(src);
    assert_eq!(f.len(), 2, "{f:#?}");
    for (d, label) in f.iter().zip(["global.read.setting.timezone", "nondet.time"]) {
        let expected = format!("Moment::__construct() has effect {label} (via parent::__construct");
        assert!(d.message.starts_with(&expected), "{}", d.message);
    }
}

#[test]
fn an_uncatalogued_engine_class_is_non_exhaustive_and_silent() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction open(): \\SplFileObject { return new \\SplFileObject('x'); }\n";
    silent(src);
    assert!(!summary(src, "open").exhaustive);
}

#[test]
fn a_project_class_shadows_the_engine_row() {
    let src = "<?php\nclass DateTime { public function __construct() {} }\n\
               #[\\Steins\\Pure]\nfunction now(): DateTime { return new DateTime(); }\n";
    silent(src);
    assert!(summary(src, "now").exhaustive);
}

#[test]
fn a_namespaced_name_is_not_the_engine_class() {
    let src = "<?php\nnamespace App;\n#[\\Steins\\Pure]\nfunction now(): object { return new DateTime(); }\n";
    silent(src);
    assert!(!summary(src, "now").exhaustive);
}

// ---------------------------------------------------------------------------
// Closures and anonymous classes
// ---------------------------------------------------------------------------

#[test]
fn a_new_in_a_closure_is_the_closures_own_and_joins_where_it_runs() {
    let src = format!(
        "{CLOCK}\n#[\\Steins\\Pure]\nfunction lazy(): \\Closure {{ return fn() => new Clock(); }}\n\
         #[\\Steins\\Pure]\nfunction eager(): array {{ return array_map(fn($i) => new Clock(), [1]); }}\n"
    );
    let f = exceeded(&src);
    assert_eq!(f.len(), 1, "only the invoked closure reaches its caller: {f:#?}");
    assert!(f[0].message.contains("eager() is declared"), "{}", f[0].message);
}

#[test]
fn an_anonymous_class_constructor_is_unseen() {
    let src = "<?php\nfunction make(): object { return new class { public function __construct() {} }; }\n";
    assert!(!summary(src, "make").exhaustive);
    let plain = "<?php\nfunction make(): object { return new class { public int $x = 1; }; }\n";
    assert!(summary(plain, "make").exhaustive);
}

#[test]
fn an_anonymous_class_runs_its_parents_constructor_and_its_arguments() {
    let src = "<?php\nclass Open { public function __construct() { echo 'x'; } }\n\
               #[\\Steins\\Pure]\nfunction make(): object { return new class extends Open {}; }\n\
               #[\\Steins\\Pure]\nfunction args(): object {\n    \
               return new class(rand()) { public function __construct(public int $v) {} };\n}\n";
    let f = exceeded(src);
    let messages: Vec<&str> = f.iter().map(|d| d.message.as_str()).collect();
    assert_eq!(messages.len(), 2, "{f:#?}");
    assert!(messages[0].starts_with("Open::__construct() has effect io.output.buffer"), "{messages:?}");
    assert!(messages[1].starts_with("rand() has effect nondet.random"), "{messages:?}");
}
