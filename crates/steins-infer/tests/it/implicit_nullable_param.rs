//! The implicitly-nullable parameter at an argument position (issue #391).
//!
//! `function f(string $s = null)` declares a non-nullable hint whose `= null`
//! default makes PHP widen the parameter to `?string`. `f(null)` runs — on PHP
//! 8.5.9 it emits only the 8.4 deprecation "Implicitly marking parameter $s as
//! nullable is deprecated" — so a definite `type.argument-mismatch` there is a
//! proof-layer false positive, which is what the pinned corpus was carrying.
//!
//! The declaration side has read the bit since the declared-parameter seeding
//! landed (`seed_fact`: `ty.nullable || p.has_null_default`); these fixtures pin
//! the argument side, both directions, plus the join shape that reaches the same
//! callee through a branch.

use steins_infer::{DEBUG_TYPE_ID, Diagnostic, ID, check};
use steins_syntax::SourceTree;

/// Every `type.argument-mismatch` a source produces.
fn mismatches(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "t.php").into_iter().filter(|d| d.id == ID).collect()
}

/// The single `debug.type` body a one-dump source produces.
fn one_type(src: &str) -> String {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let ds: Vec<Diagnostic> =
        check(&tree, &functions, "t.php").into_iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ds.len(), 1, "expected exactly one debug.type dump, got {ds:?}");
    ds[0].message.clone()
}

#[test]
fn a_literal_null_into_an_implicitly_nullable_parameter_is_silent() {
    let src = "<?php
declare(strict_types=1);
function f(string $s = null): void {}
f(null);
";
    assert!(mismatches(src).is_empty(), "{:?}", mismatches(src));
}

#[test]
fn a_proven_null_variable_into_an_implicitly_nullable_parameter_is_silent() {
    // The env-dependent half: the direct pass sees the literal, the propagation
    // pass sees a variable the walk proved `null`. Both consult the default.
    let src = "<?php
declare(strict_types=1);
function f(string $s = null): void {}
function g(): void { $v = null; f($v); }
";
    assert!(mismatches(src).is_empty(), "{:?}", mismatches(src));
}

#[test]
fn a_method_parameter_reads_the_default_too() {
    let src = "<?php
declare(strict_types=1);
class C { public function m(string $s = null): void {} }
function g(C $c): void { $c->m(null); }
";
    assert!(mismatches(src).is_empty(), "{:?}", mismatches(src));
}

#[test]
fn the_control_a_genuinely_non_nullable_parameter_still_convicts() {
    // Same call, no `= null` default: PHP raises the `TypeError` and so does
    // Steins. Tells the repair apart from "null arguments stopped being checked".
    let src = "<?php
declare(strict_types=1);
function f(string $s): void {}
f(null);
";
    let d = mismatches(src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("cannot become string $s"), "{}", d[0].message);
}

#[test]
fn the_other_control_a_non_null_mismatch_still_convicts() {
    // The default widens the parameter by `null` and by nothing else.
    let src = "<?php
declare(strict_types=1);
function f(int $n = null): void {}
f('abc');
";
    assert_eq!(mismatches(src).len(), 1);
}

#[test]
fn the_declaration_side_still_seeds_the_null() {
    // The bit this repair mirrors: inside the callee, `$s` really may be `null`.
    let src = "<?php
declare(strict_types=1);
function f(string $s = null): void { \\PHPStan\\dumpType($s); }
";
    assert_eq!(one_type(src), "dumped type: string|null");
}

#[test]
fn a_local_assigned_on_every_arm_of_a_nested_if_else_is_non_nullable_at_the_join() {
    // The join shape issue #391 names, pinned as a regression: every arm of the
    // nested `if`/`else` assigns a string, so nothing nullable survives the join
    // and the value reaches a native `string` parameter clean.
    let src = "<?php
declare(strict_types=1);
function needString(string $s): void {}
function f(bool $a, bool $b): void {
    $message = null;
    if ($a) {
        if ($b) { $message = 's1'; } else { $message = 's2'; }
    } else {
        $message = 's3';
    }
    \\PHPStan\\dumpType($message);
    needString($message);
}
";
    assert_eq!(one_type(src), "dumped type: 's1'|'s2'|'s3'");
    assert!(mismatches(src).is_empty(), "{:?}", mismatches(src));
}

#[test]
fn the_guarded_default_shape_the_corpus_carries_is_silent() {
    // The corpus line the repair was found on, minimized: an implicitly-nullable
    // parameter defaulted inside a guard and forwarded to another one. The `null`
    // that survives the guard's fall-through path is the one the callee's own
    // default admits.
    let src = "<?php
class IOException extends \\RuntimeException {
    public function __construct(string $message, int $code = 0, string $path = null) {
        parent::__construct($message, $code);
    }
}
class FileNotFoundException extends IOException {
    public function __construct(string $message = null, int $code = 0, string $path = null) {
        if (null === $message) {
            if (null === $path) { $message = 'File could not be found.'; }
            else { $message = sprintf('File \"%s\" could not be found.', $path); }
        }
        parent::__construct($message, $code, $path);
    }
}
";
    assert!(mismatches(src).is_empty(), "{:?}", mismatches(src));
}
