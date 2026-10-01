//! Builtins certified pure without being foldable (issue #851): the `is_*` type
//! predicates with `get_debug_type`, and ADR-0070's array readers. "Catalogued
//! pure" used to mean "on the fold allowlist", so a body calling `is_int()` was
//! `…?` however plainly it did nothing. `array_keys` is pure only with one
//! argument; its search form compares loosely and can run `__toString`.

use steins_infer::{Diagnostic, EFFECT_ID, EffectSummary, check, effect_summary};
use steins_syntax::SourceTree;

fn exceeded(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php").into_iter().filter(|d| d.id == EFFECT_ID).collect()
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

fn proven_pure(src: &str, symbol: &str) {
    let s = summary(src, symbol);
    assert!(s.labels.is_empty() && s.exhaustive, "{symbol}: {s:?}");
}

fn unknown(src: &str, symbol: &str) {
    let s = summary(src, symbol);
    assert!(s.labels.is_empty() && !s.exhaustive, "{symbol}: {s:?}");
}

/// `return <call>;` as the body of `f`, with `$a`, `$b` and `$c` in scope.
fn body(call: &str) -> String {
    format!("<?php\nfunction f(array $a, mixed $b, mixed $c): mixed {{ return {call}; }}\n")
}

/// A test framework's base exception: a non-`int` code is folded into the
/// message, the engine's constructor runs, and the trace is copied with its
/// arguments stripped. Every call in it is now catalogued.
const FRAMEWORK_EXCEPTION: &str = "<?php
namespace Acme\\Testing;

use function array_keys;
use function is_int;
use function sprintf;

class Failure extends \\RuntimeException
{
    protected array $frames;

    public function __construct(string $message = '', int|string $code = 0, ?\\Throwable $previous = null)
    {
        if (!is_int($code)) {
            $message .= sprintf(' (code: %s)', $code);
            $code = 0;
        }
        parent::__construct($message, $code, $previous);
        $this->frames = $this->getTrace();
        foreach (array_keys($this->frames) as $key) {
            unset($this->frames[$key]['args']);
        }
    }
}

class Skipped extends Failure {}

#[\\Steins\\Pure]
function skip(string $why): never
{
    throw new Skipped($why);
}
";

#[test]
fn a_framework_exception_constructor_is_exhaustive() {
    // Every call in it is catalogued, so no call is a gap. What remains is the
    // operators on operands nothing is shown about (a `sprintf` result appended,
    // an element of an array, the keys of one): ADR-0099 §4.3 has no proof for
    // them yet, and they are the operator resolver's, not the catalog's.
    let operator_only = |symbol: &str| {
        let s = summary(FRAMEWORK_EXCEPTION, symbol);
        assert!(s.labels.is_empty(), "{symbol}: {s:?}");
        assert!(s.gaps.iter().all(|kind| kind.starts_with("operator-")), "{symbol}: {s:?}");
    };
    operator_only("Failure::__construct");
    // `new` follows the inherited constructor, so the `#[Pure]` caller inherits its
    // gaps, and the envelope has nothing to report.
    operator_only("skip");
    assert!(exceeded(FRAMEWORK_EXCEPTION).is_empty(), "{:#?}", exceeded(FRAMEWORK_EXCEPTION));
}

#[test]
fn every_type_question_is_pure_and_exhaustive() {
    for name in [
        "is_string", "is_int", "is_integer", "is_long", "is_float", "is_double", "is_bool",
        "is_array", "is_null", "is_object", "is_scalar", "is_numeric", "is_iterable",
        "is_countable", "is_resource", "get_debug_type",
    ] {
        proven_pure(&body(&format!("{name}($c)")), "f");
    }
}

#[test]
fn every_array_reader_is_pure_and_exhaustive() {
    for call in [
        "array_first($a)",
        "array_last($a)",
        "array_key_first($a)",
        "array_key_last($a)",
        "array_values($a)",
        "array_flip($a)",
        "array_reverse($a, true)",
        "array_slice($a, 1, 2, true)",
        "array_key_exists($b, $a)",
        "key_exists($b, $a)",
        "array_is_list($a)",
        "array_keys($a)",
        "\\array_keys($a)",
    ] {
        proven_pure(&body(call), "f");
    }
}

/// Only the one-argument `array_keys` is certified. A `$filter_value` is
/// compared loosely with every element, which runs an object's `__toString`,
/// so the search form keeps the `…?` with or without the strict flag, and so
/// does any call whose arity cannot be read off the call site.
#[test]
fn array_keys_is_pure_only_at_one_positional_argument() {
    for call in [
        "array_keys($a, $b)",
        "array_keys($a, $b, true)",
        "array_keys(...$c)",
        "array_keys(array: $a)",
        "array_map('array_keys', $a)",
    ] {
        unknown(&body(call), "f");
    }
}

/// A name imported or written fully qualified still reaches the builtin, and a
/// namespaced function of the same name is what PHP calls instead.
#[test]
fn resolution_decides_before_the_certification_does() {
    let imported = "<?php\nnamespace App;\nuse function array_keys;\nuse function is_int;\n\
                    function f(array $a, mixed $c): bool { return is_int($c) && array_keys($a) === []; }\n";
    proven_pure(imported, "f");
    let shadowed = "<?php\nnamespace App;\n\
                    function is_int(mixed $c): bool { echo 'x'; return true; }\n\
                    function array_keys(array $a): array { echo 'y'; return []; }\n\
                    function f(array $a, mixed $c): bool { return is_int($c) && array_keys($a) === []; }\n";
    let s = summary(shadowed, "f");
    assert_eq!(s.labels, ["io.output.buffer"], "the project's own functions run: {s:?}");
    assert!(s.exhaustive, "{s:?}");
}

/// A certified name passed as a callback answers as a callback: the invoker's
/// row joins the callee's, and the callee's is empty now.
#[test]
fn a_certified_name_as_a_callback_is_pure() {
    proven_pure(&body("array_filter($a, 'is_int')"), "f");
    proven_pure(&body("array_map('array_values', $a)"), "f");
}

/// The exclusions keep the `…?` they had: each can run userland or reads more
/// than its arguments. `is_callable` is excluded too, but its out-parameter
/// row already made it a known builtin to the effects pass, so it is not a
/// `…?` to keep.
#[test]
fn the_excluded_neighbours_stay_unknown() {
    for call in [
        "current($a)",
        "key($a)",
        "get_class($c)",
        "array_search($b, $a)",
        "array_combine($a, $a)",
        "strcmp($b, $c)",
        "is_a($c, 'Countable', true)",
        "class_exists($b)",
        "defined('X')",
    ] {
        unknown(&body(call), "f");
    }
}

/// A certified name is a catalogued builtin to the throws pass as well: it has
/// no throw row, so it no longer leaves the body's throw set open.
#[test]
fn the_throws_pass_reads_a_certified_name_as_throwless() {
    let s = summary(&body("is_int($c) ? array_values($a) : []"), "f");
    assert!(s.throws.is_empty() && s.throws_exhaustive, "{s:?}");
    let s = summary(&body("current($a)"), "f");
    assert!(!s.throws_exhaustive, "an uncatalogued name still leaves it open: {s:?}");
}
