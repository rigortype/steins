//! Issue #544 — a property hook's `get`/`set` body is an ordinary function body.
//!
//! Every finding here belongs to a family that already existed; what changed is
//! that the walk reaches the body at all. So the fixtures come in two kinds: ones
//! that show an ordinary check firing inside a hook, and dumps that show what the
//! hook's own scope binds — the hook parameter and `$this` — because that binding
//! is the part a wrong lowering would get wrong silently.
//!
//! Each PHP semantic the lowering leans on was witnessed on 8.5.9 and is named at
//! the test that depends on it.

use steins_infer::{
    DEBUG_TYPE_ID, Diagnostic, ID as ARG_MISMATCH_ID, RETURN_ID, TYPE_RETURN_MISSING_ID, check,
};
use steins_syntax::{ScopeOwner, SourceTree};

fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php")
        .into_iter()
        .filter(|d| !d.id.starts_with("untyped."))
        .collect()
}

fn count(src: &str, id: &str) -> usize {
    findings(src).iter().filter(|d| d.id == id).count()
}

/// The single dump a fixture asks for, rendered.
fn dumped(src: &str) -> String {
    let ds: Vec<Diagnostic> = findings(src).into_iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ds.len(), 1, "expected exactly one dump, got {ds:#?}");
    ds[0].message.replace("dumped type: ", "")
}

/// A fixture with the one-argument helper every argument check here points at.
fn src(class: &str) -> String {
    format!(
        "<?php\ndeclare(strict_types=1);\nfunction takesString(string $s): void {{}}\n{class}"
    )
}

// --- the issue's own repro: the body is walked at all ----------------------

#[test]
fn a_set_hook_body_is_walked_on_a_plain_property() {
    let fixture = src("final class Regular {
            public int $value = 0 {
                set(int $value) {
                    takesString(1);
                    $this->value = $value;
                }
            }
        }");
    assert_eq!(count(&fixture, ARG_MISMATCH_ID), 1, "{:#?}", findings(&fixture));
}

#[test]
fn a_set_hook_body_is_walked_on_a_promoted_property() {
    let fixture = src("final class Promoted {
            public function __construct(
                public int $value = 0 {
                    set(int $value) {
                        takesString(1);
                        $this->value = $value;
                    }
                },
            ) {}
        }");
    assert_eq!(count(&fixture, ARG_MISMATCH_ID), 1, "{:#?}", findings(&fixture));
}

#[test]
fn a_get_hook_body_is_walked() {
    let fixture = src("final class Getter {
            private int $backing = 0;
            public int $value {
                get {
                    takesString(1);
                    return $this->backing;
                }
            }
        }");
    assert_eq!(count(&fixture, ARG_MISMATCH_ID), 1, "{:#?}", findings(&fixture));
}

#[test]
fn a_clean_hook_body_stays_silent() {
    let fixture = src("final class Clean {
            private string $backing = '';
            public string $value {
                set(string $value) {
                    takesString($value);
                    $this->backing = $value;
                }
                get {
                    return $this->backing;
                }
            }
        }");
    assert!(findings(&fixture).is_empty(), "{:#?}", findings(&fixture));
}

#[test]
fn each_concrete_hook_body_owns_exactly_one_scope() {
    // A hook is not also a method: nothing else enumerates these bodies, so no
    // finding inside one can be reported twice.
    let fixture = src("final class Two {
            private int $backing = 0;
            public int $value {
                get { return $this->backing; }
                set { $this->backing = $value; }
            }
        }");
    let tree = SourceTree::parse(&fixture);
    let hooks: Vec<_> = tree
        .scopes()
        .iter()
        .filter(|s| matches!(&s.owner, ScopeOwner::PropertyHook { .. }))
        .collect();
    assert_eq!(hooks.len(), 2, "{:#?}", hooks.iter().map(|s| &s.owner).collect::<Vec<_>>());
}

#[test]
fn an_abstract_hook_declares_no_body_and_owns_no_scope() {
    let fixture = src("abstract class Abstracted {
            abstract public int $value { get; set; }
        }");
    let tree = SourceTree::parse(&fixture);
    assert!(
        !tree.scopes().iter().any(|s| matches!(&s.owner, ScopeOwner::PropertyHook { .. })),
        "an abstract hook has no body to walk"
    );
}

// --- what a hook scope binds ----------------------------------------------

#[test]
fn a_short_form_set_binds_value_at_the_property_type() {
    // Witnessed 8.5.9: `ReflectionProperty::getHooks()` reports the implicit
    // parameter as `value:int` for an `int` property, and a bad assignment raises
    // `A::$v::set(): Argument #1 ($value) must be of type int, string given`.
    let fixture = src("final class Short {
            private int $backing = 0;
            public int $value {
                set {
                    \\PHPStan\\dumpType($value);
                    $this->backing = $value;
                }
            }
        }");
    assert_eq!(dumped(&fixture), "int");
}

#[test]
fn the_implicit_parameter_follows_the_property_not_the_hook() {
    // The mirror of the test above: the same short-form `set` on a `string`
    // property binds `$value` as a string.
    let fixture = src("final class ShortString {
            private string $backing = '';
            public string $value {
                set {
                    \\PHPStan\\dumpType($value);
                    $this->backing = $value;
                }
            }
        }");
    assert_eq!(dumped(&fixture), "string");
}

#[test]
fn an_explicit_set_parameter_is_the_one_that_is_bound() {
    // `set(string $raw)` names its own parameter, and PHP lets it widen past the
    // property's type (witnessed 8.5.9: `set(string|int $raw)` on an `int` property
    // is legal), so the body must see the WRITTEN type, not the property's.
    let fixture = src("final class Renamed {
            private int $backing = 0;
            public int $value {
                set(string $raw) {
                    \\PHPStan\\dumpType($raw);
                    $this->backing = (int) $raw;
                }
            }
        }");
    assert_eq!(dumped(&fixture), "string");
}

#[test]
fn a_hook_body_sees_this_at_the_declaring_class() {
    // `$this->take(…)` resolving at all is the proof the hook body runs in the
    // declaring class's scope.
    let fixture = src("final class Reaches {
            private int $backing = 0;
            public int $value {
                set {
                    $this->take(1);
                    $this->backing = $value;
                }
            }
            private function take(string $s): void {}
        }");
    assert_eq!(count(&fixture, ARG_MISMATCH_ID), 1, "{:#?}", findings(&fixture));
}

// --- the `get` hook's return obligation ------------------------------------

#[test]
fn a_get_hook_returns_the_property_type() {
    let fixture = src("final class BadReturn {
            public int $value {
                get {
                    return 'not an int';
                }
            }
        }");
    assert_eq!(count(&fixture, RETURN_ID), 1, "{:#?}", findings(&fixture));
}

#[test]
fn a_get_hook_that_falls_off_its_end_is_the_ordinary_return_missing() {
    // Witnessed 8.5.9: reading `public int $v { get { } }` raises
    // `C::$v::get(): Return value must be of type int, none returned`.
    let fixture = src("final class Fallthrough {
            public int $value {
                get {
                }
            }
        }");
    let ds: Vec<_> =
        findings(&fixture).into_iter().filter(|d| d.id == TYPE_RETURN_MISSING_ID).collect();
    assert_eq!(ds.len(), 1, "{ds:#?}");
    assert!(
        ds[0].message.contains("Fallthrough::$value::get"),
        "PHP's own subject spelling: {}",
        ds[0].message
    );
}

#[test]
fn a_set_hook_carries_no_return_obligation() {
    // A `set` hook returns nothing — `return <value>;` inside one is a PHP compile
    // error — so an empty body is not a `type.return-missing` site.
    let fixture = src("final class SetOnly {
            private int $backing = 0;
            public int $value {
                set {
                }
            }
        }");
    assert_eq!(count(&fixture, TYPE_RETURN_MISSING_ID), 0, "{:#?}", findings(&fixture));
}

#[test]
fn an_untyped_hooked_property_gives_its_get_hook_no_return_obligation() {
    let fixture = src("final class Untyped {
            public $value {
                get {
                }
            }
        }");
    assert_eq!(count(&fixture, TYPE_RETURN_MISSING_ID), 0, "{:#?}", findings(&fixture));
}

// --- the two arrow bodies, which are not the same construct ----------------

#[test]
fn an_arrow_get_body_is_a_return() {
    let fixture = src("final class ArrowGet {
            public int $value {
                get => 'not an int';
            }
        }");
    assert_eq!(count(&fixture, RETURN_ID), 1, "{:#?}", findings(&fixture));
}

#[test]
fn an_arrow_set_body_is_an_expression_in_statement_position() {
    // Witnessed 8.5.9: `set => e` ASSIGNS `e` to the backing property
    // (`public int $n { set => "nope"; }` raises `Cannot assign string to property
    // G::$n of type int`), so the expression is not a return — but the calls inside
    // it are checked like any other statement-position call.
    let fixture = src("final class ArrowSet {
            public int $value = 0 {
                set => takesString(1);
            }
        }");
    assert_eq!(count(&fixture, ARG_MISMATCH_ID), 1, "{:#?}", findings(&fixture));
    assert_eq!(count(&fixture, RETURN_ID), 0, "an arrow `set` is no return position");
}
