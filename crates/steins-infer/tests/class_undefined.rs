//! ADR-0049 §5 / S4: `class.undefined` — a DAMMED absence proof at the positions
//! verified to break at run time. Four groups, one id, ONE ladder:
//!
//! 1. the hard-error expressions `new X`, `X::m()`, `X::CONST`, `X::$prop`
//!    (fatal `Error: Class "X" not found`);
//! 2. inheritance — `extends`, `implements`, `use <Trait>` (fatal at class load);
//! 3. `catch (X $e)` (never matches: the handler is silently dead);
//! 4. parameter / return / property native type declarations (`TypeError` on the
//!    first typed use).
//!
//! Groups 2–4 landed with issue #182 and add NO firing licence — the ladder they
//! run is byte-identical to group 1's, so every silence leg below covers them too.
//!
//! The verified NON-findings (`instanceof`, `X::class`, `self`/`static`/`parent`,
//! trait-name static calls, the built-in type keywords, a `catch` clause the
//! lowering cannot name) each ship a silence fixture. A [`Boot`] mock stands in for
//! the runtime boot surface.

use steins_infer::{CLASS_UNDEFINED_ID, Diagnostic, Folder, check_with};
use steins_syntax::SourceTree;

struct Boot {
    available: bool,
    classes: Vec<String>,
    reflect_fails: bool,
}

impl Boot {
    fn ready() -> Self {
        Boot { available: true, classes: Vec::new(), reflect_fails: false }
    }
    fn with_classes(names: &[&str]) -> Self {
        Boot {
            available: true,
            classes: names.iter().map(|n| n.to_ascii_lowercase()).collect(),
            reflect_fails: false,
        }
    }
}

impl Folder for Boot {
    fn fold(
        &mut self,
        _n: &str,
        _a: &[steins_syntax::ArgValue],
        _strict: bool,
    ) -> Option<steins_syntax::ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        self.available
    }
    fn boot_surface_class_like(&mut self, fqn: &str) -> Option<bool> {
        if self.reflect_fails {
            return None;
        }
        Some(self.classes.iter().any(|b| b.eq_ignore_ascii_case(fqn)))
    }
    fn boot_surface_label(&mut self) -> Option<String> {
        Some("PHP 8.5.8 (32 extensions)".to_owned())
    }
}

fn run(src: &str, folder: &mut dyn Folder) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "test.php", folder)
        .into_iter()
        .filter(|d| d.id == CLASS_UNDEFINED_ID)
        .collect()
}

fn fires(src: &str) -> Vec<Diagnostic> {
    run(src, &mut Boot::ready())
}

// Firing fixtures, group 1: each of the four hard-error expression positions.

#[test]
fn fires_on_new() {
    let d = fires("<?php\nnew Widget();\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("undefined class Widget"), "{}", d[0].message);
    assert!(d[0].message.contains("PHP 8.5.8 (32 extensions)"), "{}", d[0].message);
}

#[test]
fn fires_on_static_method_call() {
    let d = fires("<?php\nWidget::make();\n");
    assert_eq!(d.len(), 1, "{d:?}");
}

#[test]
fn fires_on_class_constant_fetch() {
    let d = fires("<?php\n$x = Widget::VERSION;\n");
    assert_eq!(d.len(), 1, "{d:?}");
}

#[test]
fn fires_on_static_property_fetch() {
    let d = fires("<?php\n$x = Widget::$count;\n");
    assert_eq!(d.len(), 1, "{d:?}");
}

#[test]
fn fires_on_namespaced_new_resolved_to_current_ns() {
    let d = fires("<?php\nnamespace App;\nnew Widget();\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("App\\Widget"), "{}", d[0].message);
}

#[test]
fn fires_on_relative_namespace_new_a8() {
    // A8: `new namespace\Widget` in `App` resolves to `App\Widget`.
    let d = fires("<?php\nnamespace App;\nnew namespace\\Widget();\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("App\\Widget"), "{}", d[0].message);
}

// Firing fixtures, group 2 (issue #182): inheritance — fatal at class load.

#[test]
fn fires_on_class_extends() {
    let d = fires("<?php\nclass C extends Widget {}\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("undefined class Widget"), "{}", d[0].message);
}

#[test]
fn fires_on_class_implements_once_per_named_interface() {
    let d = fires("<?php\nclass C implements Alpha, Beta {}\n");
    assert_eq!(d.len(), 2, "{d:?}");
}

#[test]
fn fires_on_interface_extends() {
    // An interface may extend several parents; each is its own load-time fatal.
    let d = fires("<?php\ninterface I extends Alpha, Beta {}\n");
    assert_eq!(d.len(), 2, "{d:?}");
}

#[test]
fn fires_on_enum_implements() {
    let d = fires("<?php\nenum Suit implements Alpha { case Hearts; }\n");
    assert_eq!(d.len(), 1, "{d:?}");
}

#[test]
fn fires_on_trait_use_in_class_body() {
    let d = fires("<?php\nclass C { use Helper; }\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("undefined class Helper"), "{}", d[0].message);
}

#[test]
fn fires_on_anonymous_class_inheritance() {
    // The parent of an anonymous class fatals at the `new` that declares it.
    let d = fires("<?php\n$x = new class extends Widget implements Alpha {};\n");
    assert_eq!(d.len(), 2, "{d:?}");
}

#[test]
fn fires_on_namespaced_extends_resolved_to_current_ns() {
    let d = fires("<?php\nnamespace App;\nclass C extends Widget {}\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("App\\Widget"), "{}", d[0].message);
}

// Firing fixtures, group 3 (issue #182): `catch` — the handler is silently dead.

#[test]
fn fires_on_catch() {
    let d = fires("<?php\ntry { x(); } catch (NoSuchEx $e) {}\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("undefined class NoSuchEx"), "{}", d[0].message);
}

#[test]
fn fires_on_multi_catch_once_per_named_arm() {
    let d = fires("<?php\ntry { x(); } catch (NoSuchEx | AlsoMissing $e) {}\n");
    assert_eq!(d.len(), 2, "{d:?}");
}

#[test]
fn fires_on_catch_arm_beside_a_defined_one() {
    let d = fires("<?php\ntry { x(); } catch (Exception | NoSuchEx $e) {}\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("undefined class NoSuchEx"), "{}", d[0].message);
}

// Firing fixtures, group 4 (issue #182): native type declarations — `TypeError`.

#[test]
fn fires_on_parameter_and_return_type_declarations() {
    let d = fires("<?php\nfunction f(Widget $w): Gadget { return $w; }\n");
    assert_eq!(d.len(), 2, "{d:?}");
}

#[test]
fn fires_on_property_type_declaration() {
    let d = fires("<?php\nclass C { private Widget $w; }\n");
    assert_eq!(d.len(), 1, "{d:?}");
}

#[test]
fn fires_on_hooked_property_type_declaration() {
    let d = fires("<?php\nclass C { public Widget $w { get => $this->w; } }\n");
    assert_eq!(d.len(), 1, "{d:?}");
}

#[test]
fn fires_on_promoted_constructor_property() {
    let d = fires("<?php\nclass C { public function __construct(private Widget $w) {} }\n");
    assert_eq!(d.len(), 1, "{d:?}");
}

#[test]
fn fires_on_closure_signature() {
    let d = fires("<?php\n$f = function (Widget $w): Gadget { return $w; };\n");
    assert_eq!(d.len(), 2, "{d:?}");
}

#[test]
fn fires_once_on_nullable_type_declaration() {
    let d = fires("<?php\nfunction f(?Widget $w) {}\n");
    assert_eq!(d.len(), 1, "{d:?}");
}

#[test]
fn fires_once_per_named_union_arm() {
    // `int` is a keyword arm and contributes nothing; both class arms report.
    let d = fires("<?php\nfunction f(Widget|Gadget|int $w) {}\n");
    assert_eq!(d.len(), 2, "{d:?}");
}

#[test]
fn fires_once_per_intersection_conjunct() {
    let d = fires("<?php\nfunction f(Widget&Gadget $w) {}\n");
    assert_eq!(d.len(), 2, "{d:?}");
}

#[test]
fn fires_on_dnf_type_declaration_arms() {
    let d = fires("<?php\nfunction f((Widget&Gadget)|null $w) {}\n");
    assert_eq!(d.len(), 2, "{d:?}");
}

// Verified NON-findings (ADR-0049 §5 table, as amended by issue #182): each silent.

#[test]
fn silent_on_instanceof() {
    // `instanceof` an undefined class evaluates false, never a hard error (an
    // ADR-0078 contract twin, deferred by name; never a proof-layer finding).
    let d = fires("<?php\nfunction f($x) { return $x instanceof Widget; }\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_class_magic_constant() {
    // `Widget::class` has been a plain string since PHP 8.0.
    let d = fires("<?php\n$x = Widget::class;\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_docblock_class_reference() {
    // The docblock positions error nothing — the other ADR-0078 contract twin.
    let d = fires("<?php\n/** @param Widget $w */\nfunction f($w) {}\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_builtin_class_in_catch() {
    // `Throwable` is a catalog builtin — the catalog leg, at the new position.
    let d = fires("<?php\ntry { x(); } catch (\\Throwable $e) {}\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_builtin_class_in_type_declaration() {
    let d = fires("<?php\nfunction f(\\Stringable $s): \\Throwable { return $s; }\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_self_and_parent_return_types() {
    // `self`/`static`/`parent` are their own CST hint variants, never class names.
    let d = fires(
        "<?php\nclass Base {}\nclass C extends Base {\n  public function a(): self { return $this; }\n  public function b(): static { return $this; }\n  public function c(): parent { return $this; }\n}\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_nullable_scalar_type_declaration() {
    let d = fires("<?php\nfunction f(?int $n): ?string { return null; }\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_every_builtin_type_keyword() {
    // The exhaustive keyword sweep: none of these is a class reference.
    let d = fires(
        "<?php\nclass C {\n  public array $a = [];\n  public iterable $b = [];\n  public mixed $c = null;\n  public object $d;\n  public bool $e = false;\n  public float $f = 0.0;\n  public int|string $g = 0;\n  public ?bool $h = null;\n  public function m(callable $x, true $y, false $z, null $n): void {}\n  public function n(): never { throw new \\Exception('x'); }\n}\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_catch_with_unresolvable_member() {
    // A caught type the lowering cannot name statically contributes NOTHING —
    // not even its resolvable arms (ADR-0040's `has_unresolvable`).
    let d = fires("<?php\ntry { x(); } catch (?NoSuchEx $e) {}\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_defined_names_at_the_new_positions() {
    let d = fires(
        "<?php\ninterface Alpha {}\ntrait Helper {}\nclass Base {}\nclass C extends Base implements Alpha { use Helper; }\nfunction f(C $c): Base { return $c; }\ntry { x(); } catch (\\Exception $e) {}\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_self_static_parent() {
    let d = fires(
        "<?php\nclass Base {}\nclass C extends Base { public function go() { self::x(); static::y(); parent::z(); new self(); } }\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_trait_name_static_call() {
    // A (deprecated) static call through a trait name resolves via the class-like index.
    let d = fires("<?php\ntrait T { public static function m() {} }\nT::m();\n");
    assert!(d.is_empty(), "{d:?}");
}

// Silence matrix — one fixture per ladder leg.

#[test]
fn silent_on_defined_class() {
    assert!(fires("<?php\nclass Widget {}\nnew Widget();\n").is_empty());
}

#[test]
fn silent_on_defined_interface_class_like() {
    // Interfaces are in the class-like index; a static const fetch on one resolves.
    assert!(fires("<?php\ninterface I { const V = 1; }\n$x = I::V;\n").is_empty());
}

#[test]
fn silent_on_defined_enum() {
    assert!(fires("<?php\nenum Suit { case Hearts; }\nSuit::Hearts;\n").is_empty());
}

#[test]
fn silent_on_alias_edge() {
    // A literal `class_alias` edge makes the alias name resolvable.
    let d = fires("<?php\nclass Real {}\nclass_alias('Real', 'Widget');\nnew Widget();\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_ambiguous_duplicate_decl() {
    let d = fires("<?php\nclass Widget {}\nclass Widget {}\nnew Widget();\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_catalog_builtin() {
    // `Exception` is a known builtin class in the catalog hierarchy.
    assert!(fires("<?php\nthrow new Exception('x');\n").is_empty());
}

#[test]
fn silent_on_boot_surface_homonym() {
    // Index-absent but the boot surface knows it (a loaded extension class).
    let mut b = Boot::with_classes(&["Widget"]);
    assert!(run("<?php\nnew Widget();\n", &mut b).is_empty());
}

#[test]
fn silent_when_family_unavailable() {
    let mut b = Boot { available: false, classes: Vec::new(), reflect_fails: false };
    assert!(run("<?php\nnew Widget();\n", &mut b).is_empty());
}

#[test]
fn silent_when_reflect_unanswerable() {
    let mut b = Boot { available: true, classes: Vec::new(), reflect_fails: true };
    assert!(run("<?php\nnew Widget();\n", &mut b).is_empty());
}

#[test]
fn silent_under_standing_dam_eval() {
    let d = fires("<?php\neval('class Widget {}');\nnew Widget();\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_under_standing_dam_bare_relative_include() {
    let d = fires("<?php\ninclude 'classes.php';\nnew Widget();\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_when_class_exists_guard_folds_branch_dead() {
    // Dead-region pruning: `class_exists('Widget')` folds to No (absent + boot
    // not-found), so the then-branch is dead.
    let d = fires("<?php\nif (class_exists('Widget')) {\n  new Widget();\n}\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_in_a_dead_branch() {
    let d = fires("<?php\nif (false) {\n  new Widget();\n}\n");
    assert!(d.is_empty(), "{d:?}");
}
