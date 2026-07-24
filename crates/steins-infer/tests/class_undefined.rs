//! ADR-0049 §5 / S4: `class.undefined` — a DAMMED absence proof at the four
//! verified hard-error positions (`new X`, `X::m()`, `X::CONST`, `X::$prop`).
//!
//! The verified NON-findings (`instanceof`, `catch`, `X::class`, type declarations,
//! `self`/`static`/`parent`, trait-name static calls) each ship a silence fixture.
//! A [`Boot`] mock stands in for the runtime boot surface.

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
    fn fold(&mut self, _n: &str, _a: &[steins_syntax::ArgValue]) -> Option<steins_syntax::ArgValue> {
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

// ---------------------------------------------------------------------------
// Firing fixtures: each of the four hard-error positions.
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Verified NON-findings (ADR-0049 §5 table): each silent.
// ---------------------------------------------------------------------------

#[test]
fn silent_on_instanceof() {
    // `instanceof` an undefined class evaluates false — never a hard error.
    let d = fires("<?php\nfunction f($x) { return $x instanceof Widget; }\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_catch() {
    let d = fires("<?php\ntry { x(); } catch (Widget $e) {}\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_class_magic_constant() {
    // `Widget::class` has been a plain string since PHP 8.0.
    let d = fires("<?php\n$x = Widget::class;\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_type_declaration() {
    let d = fires("<?php\nfunction f(Widget $w): Widget { return $w; }\n");
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
    // A static call through a trait name RUNS (deprecated), never a hard error — the
    // trait is in the class-like index (S1), so it resolves.
    let d = fires("<?php\ntrait T { public static function m() {} }\nT::m();\n");
    assert!(d.is_empty(), "{d:?}");
}

// ---------------------------------------------------------------------------
// Silence matrix — one fixture per ladder leg.
// ---------------------------------------------------------------------------

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
    // The guard leg via dead-region pruning: `class_exists('Widget')` folds to No
    // (Widget absent + boot not-found), so the then-branch is dead and skipped.
    let d = fires("<?php\nif (class_exists('Widget')) {\n  new Widget();\n}\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_in_a_dead_branch() {
    let d = fires("<?php\nif (false) {\n  new Widget();\n}\n");
    assert!(d.is_empty(), "{d:?}");
}
