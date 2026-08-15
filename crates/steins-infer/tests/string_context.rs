//! String context (ADR-0078, issue #193): `string.non-stringable` and
//! `string.array-conversion`.
//!
//! Putting a value where PHP wants a string — `"x $v"`, `echo $v`, `print $v`,
//! `(string) $v`, `'a' . $v` — is total and silent for every scalar and `null`.
//! Two values break it, split into two ids because the ADR-0049 §7
//! warning-handler gate cuts between them (ADR-0078 §1.4): an **object with no
//! reachable `__toString`** is a fatal, witnessed on PHP 8.5.9 as `Error: Object
//! of class A could not be converted to string` in all five contexts; an
//! **array** is `Warning: Array to string conversion` plus the literal string
//! `"Array"` (`(string) [1,2,3]` is `"Array"`, `'x' . [1,2,3]` is `"xArray"`).
//!
//! The object leg is an absence proof riding the whole ADR-0049 ladder (needs a
//! boot-surface mock, as `call.undefined-method` does); the array leg is a
//! value-domain fact alone and needs nothing. Every ladder leg ships a silence
//! fixture (the §10 discipline).

use steins_infer::{
    Diagnostic, Folder, STRING_ARRAY_CONVERSION_ID, STRING_NON_STRINGABLE_ID, check_full,
    check_with,
};
use steins_syntax::{ArgValue, SourceTree};

/// The boot-surface mock `call.undefined-method`'s tests use (`available`: A9
/// family gate; `builtins`: names the runtime reports as resident class-likes,
/// the A2ii homonyms).
struct Boot {
    available: bool,
    builtins: Vec<String>,
}

impl Boot {
    fn ready() -> Self {
        Boot { available: true, builtins: Vec::new() }
    }
}

impl Folder for Boot {
    fn fold(&mut self, _name: &str, _args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        self.available
    }
    fn boot_surface_class_like(&mut self, fqn: &str) -> Option<bool> {
        Some(self.builtins.iter().any(|b| b.eq_ignore_ascii_case(fqn)))
    }
}

fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "test.php", &mut Boot::ready())
        .into_iter()
        .filter(|d| d.id == STRING_NON_STRINGABLE_ID || d.id == STRING_ARRAY_CONVERSION_ID)
        .collect()
}

fn objects(src: &str) -> Vec<Diagnostic> {
    findings(src).into_iter().filter(|d| d.id == STRING_NON_STRINGABLE_ID).collect()
}

fn arrays(src: &str) -> Vec<Diagnostic> {
    findings(src).into_iter().filter(|d| d.id == STRING_ARRAY_CONVERSION_ID).collect()
}

/// A plain class with no `__toString` and no ancestors — the fully enumerable case.
const PLAIN: &str = "<?php\nclass A {}\n";

// `string.non-stringable`: one firing fixture per context.

#[test]
fn non_stringable_in_interpolation_fires() {
    let d = objects(&format!("{PLAIN}$a = new A();\necho \"x $a\";\n"));
    assert_eq!(d.len(), 1, "an interpolated non-stringable object is a proven fatal: {d:#?}");
    assert!(d[0].message.contains("string interpolation"), "{}", d[0].message);
    assert!(
        d[0].message.contains("could not be converted to string"),
        "the message quotes the runtime Error: {}",
        d[0].message
    );
}

#[test]
fn non_stringable_in_braced_interpolation_fires() {
    let d = objects(&format!("{PLAIN}$a = new A();\necho \"x {{$a}}\";\n"));
    assert_eq!(d.len(), 1, "`{{$a}}` is the same conversion as `$a`: {d:#?}");
}

#[test]
fn non_stringable_in_heredoc_fires() {
    let d = objects(&format!("{PLAIN}$a = new A();\n$s = <<<TXT\n  x $a\n  TXT;\n"));
    assert_eq!(d.len(), 1, "a heredoc interpolates exactly as a double-quoted string: {d:#?}");
}

#[test]
fn nowdoc_does_not_interpolate_and_is_silent() {
    let d = findings(&format!("{PLAIN}$a = new A();\n$s = <<<'TXT'\n  x $a\n  TXT;\n"));
    assert!(d.is_empty(), "a nowdoc has no embedded expressions: {d:#?}");
}

#[test]
fn non_stringable_in_echo_fires() {
    let d = objects(&format!("{PLAIN}$a = new A();\necho $a;\n"));
    assert_eq!(d.len(), 1, "an echoed non-stringable object is a proven fatal: {d:#?}");
    assert!(d[0].message.contains("`echo`"), "{}", d[0].message);
}

#[test]
fn non_stringable_in_echo_tag_fires() {
    let d = objects(&format!("{PLAIN}$a = new A();\n?>\n<?= $a ?>\n"));
    assert_eq!(d.len(), 1, "`<?= … ?>` is an echo: {d:#?}");
}

#[test]
fn non_stringable_in_print_fires() {
    let d = objects(&format!("{PLAIN}$a = new A();\nprint $a;\n"));
    assert_eq!(d.len(), 1, "a printed non-stringable object is a proven fatal: {d:#?}");
    assert!(d[0].message.contains("`print`"), "{}", d[0].message);
}

#[test]
fn non_stringable_in_string_cast_fires() {
    let d = objects(&format!("{PLAIN}$a = new A();\n$s = (string) $a;\n"));
    assert_eq!(d.len(), 1, "a `(string)` cast of a non-stringable object fatals: {d:#?}");
    assert!(d[0].message.contains("(string)"), "{}", d[0].message);
}

#[test]
fn non_stringable_in_concat_fires() {
    let d = objects(&format!("{PLAIN}$a = new A();\n$s = 'x' . $a;\n"));
    assert_eq!(d.len(), 1, "a concatenated non-stringable object fatals: {d:#?}");
    assert!(d[0].message.contains("concatenation"), "{}", d[0].message);
}

#[test]
fn non_stringable_in_compound_concat_fires() {
    let d = objects(&format!("{PLAIN}$a = new A();\n$s = 'x';\n$s .= $a;\n"));
    assert_eq!(d.len(), 1, "`.=` converts its right-hand side exactly as `.` does: {d:#?}");
}

#[test]
fn non_stringable_in_a_return_operand_fires() {
    let src = format!("{PLAIN}function f(): string {{\n    $a = new A();\n    return \"v: $a\";\n}}\n");
    let d = objects(&src);
    assert_eq!(d.len(), 1, "a `return` operand is a collected position: {d:#?}");
}

#[test]
fn non_stringable_at_a_new_expression_fires() {
    // A `new C(...)` operand is exact by construction — no variable binding needed.
    let d = objects(&format!("{PLAIN}echo new A();\n"));
    assert_eq!(d.len(), 1, "`new A()` is an exactly-known class: {d:#?}");
}

#[test]
fn the_other_casts_are_not_this_id() {
    let d = findings(&format!("{PLAIN}$a = new A();\n$s = (array) $a;\n$b = (bool) $a;\n"));
    assert!(d.is_empty(), "only `(string)` is a string context: {d:#?}");
}

// `string.non-stringable`: the silence matrix.

#[test]
fn a_class_with_tostring_is_silent() {
    let src = "<?php\nclass A { public function __toString(): string { return 'a'; } }\n$a = new A();\necho $a;\n";
    assert!(findings(src).is_empty(), "the conversion is legal");
}

#[test]
fn a_stringable_implementor_is_silent() {
    let src = "<?php\nclass A implements Stringable { public function __toString(): string { return 'a'; } }\n$a = new A();\necho \"x $a\";\n";
    assert!(findings(src).is_empty(), "a `Stringable` implementor converts legally");
}

#[test]
fn tostring_inherited_from_a_parent_is_silent() {
    let src = "<?php\nclass B { public function __toString(): string { return 'b'; } }\nclass A extends B {}\n$a = new A();\necho $a;\n";
    assert!(findings(src).is_empty(), "the chain walk finds the parent's `__toString`");
}

#[test]
fn an_unenumerable_chain_is_silent() {
    // `Vendor` is out of the project index, so `__toString` could be declared out
    // of view and the chain never closes (also PHPStan's own unresolvable-parent leg).
    let src = "<?php\nclass A extends \\Vendor\\Base {}\n$a = new A();\necho $a;\n";
    assert!(findings(src).is_empty(), "an unresolvable ancestor is silence");
}

#[test]
fn a_trait_in_the_chain_is_silent() {
    // Trait members are not flattened into the class (object-model), so a `use`d
    // trait obstructs the absence proof even though it would supply `__toString`.
    let src = "<?php\ntrait T { public function __toString(): string { return 't'; } }\nclass A { use T; }\n$a = new A();\necho $a;\n";
    assert!(findings(src).is_empty(), "a trait anywhere in the chain is silence");
}

#[test]
fn a_trait_without_tostring_is_still_silent() {
    // The obstacle is the trait itself, not what it declares.
    let src = "<?php\ntrait T { public function x(): int { return 1; } }\nclass A { use T; }\n$a = new A();\necho $a;\n";
    assert!(findings(src).is_empty(), "the trait obstacle does not depend on its members");
}

#[test]
fn a_magic_tag_carrying_class_is_silent() {
    // The A14 obstacle record (issue #195): a `@method` tag anywhere in the
    // resolved reach means members live where the index cannot enumerate them.
    let src = "<?php\n/**\n * @method string __toString()\n */\nclass A {}\n$a = new A();\necho $a;\n";
    assert!(findings(src).is_empty(), "a `@method` tag is an enumerability obstacle");
}

#[test]
fn a_mixin_tag_on_an_ancestor_is_silent() {
    let src = "<?php\n/**\n * @mixin \\Other\n */\nclass B {}\nclass A extends B {}\n$a = new A();\necho $a;\n";
    assert!(findings(src).is_empty(), "the obstacle is transitive through the parent chain");
}

#[test]
fn an_enum_case_is_silent() {
    // Measured: `echo E::A;` IS a fatal on 8.5.9, but enum members are not lowered,
    // so the chain walk cannot enumerate them — silence, not a finding.
    let src = "<?php\nenum E: string { case A = 'a'; }\necho E::A;\n";
    assert!(findings(src).is_empty(), "an enum chain is not enumerable here");
}

#[test]
fn an_inexact_receiver_is_silent() {
    // A parameter typed `A` is a LOWER bound: a subclass may declare `__toString`.
    let src = "<?php\nclass A {}\nfunction f(A $a): void { echo $a; }\n";
    assert!(findings(src).is_empty(), "a lower-bound receiver proves nothing");
}

#[test]
fn this_in_a_non_final_class_is_silent() {
    // Membership is not exactness: a subclass may declare `__toString`.
    let src = "<?php\nclass A {\n    public int $n = 1;\n    public function f(): void { echo $this; }\n}\n";
    assert!(findings(src).is_empty(), "`$this` is a lower bound in a non-final class");
}

#[test]
fn this_in_a_final_class_fires() {
    // A `final` class has no subclass, so its `$this` IS the exact runtime class.
    let src = "<?php\nfinal class A {\n    public int $n = 1;\n    public function f(): void { echo $this; }\n}\n";
    let d = objects(src);
    assert_eq!(d.len(), 1, "`final` makes `$this` exact: {d:#?}");
}

#[test]
fn a_property_receiver_is_silent() {
    let src = "<?php\nclass A {}\nclass B { public A $a; public function f(): void { echo $this->a; } }\n";
    assert!(findings(src).is_empty(), "a property fetch carries no proven object here");
}

#[test]
fn an_anonymous_class_is_silent() {
    let src = "<?php\necho new class {};\n";
    assert!(findings(src).is_empty(), "an anonymous class's body is not read");
}

#[test]
fn without_a_boot_surface_the_object_leg_is_silent() {
    struct Unavailable;
    impl Folder for Unavailable {
        fn fold(&mut self, _n: &str, _a: &[ArgValue], _strict: bool) -> Option<ArgValue> {
            None
        }
    }
    let tree = SourceTree::parse(&format!("{PLAIN}$a = new A();\necho $a;\n"));
    let d: Vec<Diagnostic> = check_with(&tree, &[], "test.php", &mut Unavailable)
        .into_iter()
        .filter(|d| d.id == STRING_NON_STRINGABLE_ID)
        .collect();
    assert!(d.is_empty(), "the absence family is silent without a live sidecar (A9): {d:#?}");
}

#[test]
fn a_boot_surface_homonym_is_silent() {
    let mut folder = Boot { available: true, builtins: vec!["a".to_owned()] };
    let tree = SourceTree::parse(&format!("{PLAIN}$a = new A();\necho $a;\n"));
    let d: Vec<Diagnostic> = check_with(&tree, &[], "test.php", &mut folder)
        .into_iter()
        .filter(|d| d.id == STRING_NON_STRINGABLE_ID)
        .collect();
    assert!(d.is_empty(), "a resident builtin of the same name shadows the claim (A2ii): {d:#?}");
}

// `string.array-conversion`: one firing fixture per context.

#[test]
fn array_in_interpolation_fires() {
    let d = arrays("<?php\n$a = [1, 2, 3];\necho \"x $a\";\n");
    assert_eq!(d.len(), 1, "an interpolated array warns: {d:#?}");
    assert!(d[0].message.contains("string interpolation"), "{}", d[0].message);
    assert!(d[0].message.contains("Array to string conversion"), "{}", d[0].message);
}

#[test]
fn array_in_echo_fires() {
    let d = arrays("<?php\n$a = [1, 2, 3];\necho $a;\n");
    assert_eq!(d.len(), 1, "an echoed array warns: {d:#?}");
}

#[test]
fn array_in_print_fires() {
    let d = arrays("<?php\n$a = [1, 2, 3];\nprint $a;\n");
    assert_eq!(d.len(), 1, "a printed array warns: {d:#?}");
}

#[test]
fn array_in_string_cast_fires() {
    let d = arrays("<?php\n$a = [1, 2, 3];\n$s = (string) $a;\n");
    assert_eq!(d.len(), 1, "`(string) $a` is the literal \"Array\": {d:#?}");
}

#[test]
fn array_in_concat_fires() {
    let d = arrays("<?php\n$a = [1, 2, 3];\n$s = 'x' . $a;\n");
    assert_eq!(d.len(), 1, "`'x' . $a` is \"xArray\": {d:#?}");
}

#[test]
fn a_variable_bound_to_an_array_fires_through_the_env() {
    let d = arrays("<?php\n$a = [1, 2, 3];\n$b = $a;\necho $b;\n");
    assert_eq!(d.len(), 1, "the env carries the proven array: {d:#?}");
}

#[test]
fn both_concat_operands_are_judged() {
    // PHP warns once per array operand.
    let d = arrays("<?php\n$a = [1];\n$b = [2];\n$s = $a . $b;\n");
    assert_eq!(d.len(), 2, "each operand converts on its own: {d:#?}");
}

#[test]
fn compound_concat_judges_its_target() {
    let d = arrays("<?php\n$a = [1];\n$a .= 'x';\n");
    assert_eq!(d.len(), 1, "`$a .= 'x'` reads `$a` in string context: {d:#?}");
}

// `string.array-conversion`: the silence matrix and both gate postures.

#[test]
fn scalars_and_null_are_silent() {
    // Every one of these is LEGAL PHP: `int`, `float`, `bool` and `null` convert
    // totally and silently (`null` interpolates to the empty string).
    let src = "<?php\n$i = 42;\n$f = 1.5;\n$b = true;\n$n = null;\n$s = 'x';\necho \"$i $f $b $n $s\";\necho $i;\nprint $n;\n$t = (string) $b;\n$u = 'a' . $f;\n";
    assert!(findings(src).is_empty(), "a scalar in string context is never a finding");
}

#[test]
fn a_maybe_array_is_silent() {
    // `array|string` proves nothing about this call.
    let src = "<?php\nfunction f(array|string $a): void { echo $a; }\n";
    assert!(findings(src).is_empty(), "a union that may not be an array is silence");
}

#[test]
fn a_declared_array_parameter_is_recorded_silence() {
    // `array $a` IS runtime-enforced, but `TypeMember` has no array member, so an
    // `array` hint lowers the native type away — pinned as a known IR gap, not a surprise.
    let d = findings("<?php\nfunction f(array $a): void { echo $a; }\n");
    assert!(d.is_empty(), "a bare native `array` declaration is not in the IR yet: {d:#?}");
}

#[test]
fn an_asserted_array_fact_is_silent() {
    // A docblock claim is `Asserted`, not `Verified` — not proof-layer evidence.
    let src = "<?php\nfunction f($a): void {\n    /** @var array $a */\n    echo $a;\n}\n";
    assert!(findings(src).is_empty(), "an Asserted fact carries no proof-layer claim");
}

#[test]
fn an_unproven_operand_is_silent() {
    let src = "<?php\nfunction f($a): void { echo $a; echo \"x $a\"; $s = (string) $a; }\n";
    assert!(findings(src).is_empty(), "an untyped parameter proves nothing");
}

#[test]
fn warning_handler_null_demotes_the_array_id() {
    let tree = SourceTree::parse("<?php\n$a = [1, 2, 3];\necho $a;\n");
    let d: Vec<Diagnostic> = check_full(&tree, "test.php", &mut Boot::ready(), false)
        .into_iter()
        .filter(|d| d.id == STRING_ARRAY_CONVERSION_ID)
        .collect();
    assert!(d.is_empty(), "\"null\" posture demotes the warning-grade id: {d:#?}");
}

#[test]
fn warning_handler_abort_emits_the_array_id() {
    let tree = SourceTree::parse("<?php\n$a = [1, 2, 3];\necho $a;\n");
    let d: Vec<Diagnostic> = check_full(&tree, "test.php", &mut Boot::ready(), true)
        .into_iter()
        .filter(|d| d.id == STRING_ARRAY_CONVERSION_ID)
        .collect();
    assert_eq!(d.len(), 1, "the default \"abort\" posture emits: {d:#?}");
}

#[test]
fn the_fatal_id_does_not_demote() {
    // The gate boundary is the whole reason these are two ids (ADR-0078 §1.4): a
    // fatal is not a warning the application can declare it tolerates.
    let tree = SourceTree::parse(&format!("{PLAIN}$a = new A();\necho $a;\n"));
    let d: Vec<Diagnostic> = check_full(&tree, "test.php", &mut Boot::ready(), false)
        .into_iter()
        .filter(|d| d.id == STRING_NON_STRINGABLE_ID)
        .collect();
    assert_eq!(d.len(), 1, "the fatal id survives a \"null\" warning-handler posture: {d:#?}");
}

// Positions: what is collected, and the recorded boundary.

#[test]
fn a_site_is_reported_once() {
    // `echo (string) $a;` is both an echo operand and a cast; only the innermost
    // construct naming the value — the cast — reports.
    let d = objects(&format!("{PLAIN}$a = new A();\necho (string) $a;\n"));
    assert_eq!(d.len(), 1, "one conversion, one finding: {d:#?}");
}

#[test]
fn a_left_nested_concat_chain_reports_each_leaf_once() {
    let d = arrays("<?php\n$a = [1];\n$s = 'x' . $a . 'y';\n");
    assert_eq!(d.len(), 1, "the chain's one array leaf reports once: {d:#?}");
}

#[test]
fn each_echo_operand_is_judged() {
    let d = arrays("<?php\n$a = [1];\n$b = [2];\necho $a, $b;\n");
    assert_eq!(d.len(), 2, "a comma-separated echo converts each operand: {d:#?}");
}

#[test]
fn a_branch_condition_is_the_recorded_omission() {
    // Conditions/loop headers/`match` subjects are evaluated in an env the
    // statement-position pass doesn't hold, so they carry no sites — pinned, not an accident.
    let d = findings("<?php\n$a = [1];\nif ((string) $a) { $x = 1; }\nwhile ((string) $a) { break; }\n");
    assert!(d.is_empty(), "guard and loop-header positions are silence: {d:#?}");
}

#[test]
fn an_arrow_function_body_is_judged() {
    // An arrow body is a `return` position lowered as its own one-statement trace,
    // collected there rather than by `lower_stmt`.
    let d = arrays("<?php\n$f = fn() => 'x' . [1, 2];\n");
    assert_eq!(d.len(), 1, "an arrow body carries its own sites: {d:#?}");
}
