//! A catalogued builtin can run user code through its arguments (issue #856):
//! a coercive `string` parameter converts an object through `__toString`,
//! `count()` calls `Countable::count`, `in_array()` compares loosely,
//! `is_callable()` autoloads. A call is pure only where the call site rules
//! out every reaching argument, by what the argument is shown to hold or, for
//! a coerced `string` parameter, by `declare(strict_types=1)`. The string
//! family is certified under the same rule.

use steins_infer::{EffectSummary, effect_summary};
use steins_syntax::SourceTree;

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
    assert!(s.labels.is_empty() && s.exhaustive, "{symbol} should be pure: {s:?}\n{src}");
}

fn unknown(src: &str, symbol: &str) {
    let s = summary(src, symbol);
    assert!(!s.exhaustive, "{symbol} should be `…?`: {s:?}\n{src}");
}

/// A file declaring a `Stringable` and a `Countable` class, then `f` with the
/// given signature and body, in coercive mode or under `strict_types=1`.
fn file(strict: bool, signature: &str, body: &str) -> String {
    let declare = if strict { "declare(strict_types=1);\n" } else { "" };
    format!(
        "<?php\n{declare}\
         final class Name {{ public function __toString(): string {{ echo 'x'; return 'x'; }} }}\n\
         final class Bag implements \\Countable {{\n\
             public function count(): int {{ echo 'n'; return 0; }}\n\
         }}\n\
         function f({signature}): mixed {{ {body} }}\n"
    )
}

#[test]
fn a_string_parameter_reaches_to_string_only_under_coercive_typing() {
    unknown(&file(false, "Name $o", "return strlen($o);"), "f");
    proven_pure(&file(true, "Name $o", "return strlen($o);"), "f");
    proven_pure(&file(false, "string $s", "return strlen($s);"), "f");
    proven_pure(&file(false, "?string $s = null", "return strlen($s);"), "f");
    unknown(&file(false, "$s", "return strlen($s);"), "f");
}

#[test]
fn count_reaches_a_countable_and_never_an_arrays_elements() {
    unknown(&file(false, "Bag $c", "return count($c);"), "f");
    unknown(&file(true, "Bag $c", "return count($c);"), "f");
    proven_pure(&file(false, "array $a", "return count($a);"), "f");
    proven_pure(&file(false, "array $a", "return count($a, COUNT_RECURSIVE);"), "f");
    proven_pure(&file(false, "", "return count([new Name()]);"), "f");
}

/// `in_array` compares the needle loosely with every element, so both must
/// hold no object anywhere, in either mode.
#[test]
fn in_array_reaches_through_the_needle_and_the_haystack() {
    unknown(&file(false, "Name $o", "return in_array($o, ['a', 'b']);"), "f");
    unknown(&file(true, "Name $o", "return in_array($o, ['a', 'b']);"), "f");
    unknown(&file(false, "string $s, array $a", "return in_array($s, $a);"), "f");
    proven_pure(&file(false, "string $s", "return in_array($s, ['a', 'b'], true);"), "f");
    proven_pure(&file(false, "int $i", "return in_array($i, [1, 2]);"), "f");
}

/// `is_callable` resolves a class named in a string, which runs the
/// autoloader; no argument shape rules that out.
#[test]
fn is_callable_autoloads() {
    unknown(&file(false, "", "return is_callable('Foo::bar');"), "f");
    unknown(&file(true, "string $s", "return is_callable($s);"), "f");
}

/// The string family is pure exactly where its `string` parameters are ruled
/// out, and keeps its `…?` otherwise.
#[test]
fn the_string_family_is_certified_under_the_same_rule() {
    for call in ["strcmp($o, 'x')", "ord($o)", "dirname($o)", "substr_count($o, 'x')"] {
        unknown(&file(false, "Name $o", &format!("return {call};")), "f");
        proven_pure(&file(true, "Name $o", &format!("return {call};")), "f");
    }
    for call in ["strcmp($s, 'x')", "strncasecmp($s, 'x', 1)", "bin2hex($s)", "unpack('N', $s)"] {
        proven_pure(&file(false, "string $s", &format!("return {call};")), "f");
    }
    proven_pure(&file(false, "int $i", "return chr($i);"), "f");
    // A name that reads the locale is not pure in either mode: since ADR-0101 §3.9 it is
    // catalogued with the read, and its string parameters are held to the reach rule like
    // any other coloured row.
    for call in ["basename($s)", "strnatcasecmp($s, 'x')"] {
        for strict in [false, true] {
            let s = summary(&file(strict, "string $s", &format!("return {call};")), "f");
            assert_eq!(s.labels, ["global.read.setting.locale"], "{call}: {s:?}");
            assert!(s.exhaustive, "{call}: {s:?}");
        }
        let s = summary(&file(false, "Name $o", &format!("return {}", call.replace("$s", "$o") + ";")), "f");
        assert!(!s.exhaustive, "an object argument runs __toString: {call}: {s:?}");
    }
}

/// A namespaced function of the same name is what PHP calls, so the
/// certification does not reach it.
#[test]
fn resolution_decides_before_the_string_family_certification() {
    let shadowed = "<?php\ndeclare(strict_types=1);\nnamespace App;\n\
                    function strcmp(string $a, string $b): int { echo 'x'; return 0; }\n\
                    function f(string $s): int { return strcmp($s, 'x'); }\n";
    let s = summary(shadowed, "f");
    assert_eq!(s.labels, ["io.output.buffer"], "{s:?}");
}

/// The fold allowlist's other reaching rows, each against an argument that
/// rules it out and one that does not.
#[test]
fn the_fold_allowlist_rows_answer_by_their_arguments() {
    for (signature, call) in [
        ("array $a", "implode(',', $a)"),
        ("mixed $v", "strval($v)"),
        ("mixed $v", "sprintf('%s', $v)"),
        ("mixed $v", "json_encode($v)"),
        ("array $a", "str_replace('a', 'b', $a)"),
        ("array $a", "array_unique($a)"),
    ] {
        unknown(&file(true, signature, &format!("return {call};")), "f");
    }
    for (signature, call) in [
        ("", "implode(',', ['a', 'b'])"),
        ("int $i", "strval($i)"),
        ("array $a", "array_merge($a, [1])"),
        ("mixed $v", "gettype($v)"),
        ("mixed $v", "intval($v)"),
        ("mixed $v", "array_fill(0, 2, $v)"),
        ("string $s", "str_replace(['a', 'b'], '', $s)"),
    ] {
        proven_pure(&file(false, signature, &format!("return {call};")), "f");
    }
}

/// A parameter proves its type only while the frame keeps the binding it was
/// called with: a write, or a callee that can take it by reference, rebinds it.
#[test]
fn a_rebound_parameter_proves_nothing() {
    unknown(&file(false, "string $s", "$s = new Name(); return strlen($s);"), "f");
    unknown(&file(false, "string $s", "foreach ([new Name()] as $s) {} return strlen($s);"), "f");
    let by_ref = "<?php\nfunction rebind(&$x): void { $x = new Name(); }\n\
                  function f(string $s): int { rebind($s); return strlen($s); }\n";
    unknown(by_ref, "f");
    let method = "<?php\nfunction f(string $s, object $o): int { $o->m($s); return strlen($s); }\n";
    unknown(method, "f");
    // A by-value builtin keeps it, and so does a project function.
    proven_pure(&file(false, "string $s", "trim($s); return strlen($s);"), "f");
    let by_value = "<?php\nfunction keep(string $x): void {}\n\
                    function f(string $s): int { keep($s); return strlen($s); }\n";
    proven_pure(by_value, "f");
}

/// A typed property holds a value of its type on every read; an untyped one, a
/// private one of an ancestor and an undeclared one hold anything.
#[test]
fn a_typed_property_proves_its_type() {
    let class = |decl: &str, body: &str| {
        format!(
            "<?php\nclass Base {{ private array $hidden = []; protected array $items = []; }}\n\
             final class Box extends Base {{ {decl}\n public function m(): int {{ {body} }} }}\n"
        )
    };
    proven_pure(&class("private string $name = '';", "return strlen($this->name);"), "Box::m");
    proven_pure(&class("", "return count($this->items);"), "Box::m");
    unknown(&class("private $name = '';", "return strlen($this->name);"), "Box::m");
    unknown(&class("", "return count($this->hidden);"), "Box::m");
    unknown(&class("", "return count($this->missing);"), "Box::m");
}

/// A builtin handed to another as a callback is called with arguments of the
/// invoker's choosing, in coercive mode whatever the file declares.
#[test]
fn a_builtin_callback_reaches_what_its_invoker_hands_it() {
    unknown(&file(true, "array $a", "return array_map('strlen', $a);"), "f");
    unknown(&file(true, "array $a", "return array_filter($a, 'trim');"), "f");
    proven_pure(&file(true, "array $a", "return array_map('is_int', $a);"), "f");
}

/// A callable a plain call cannot resolve runs, and the invoker's own
/// arguments reach user code as any call's do.
#[test]
fn an_unresolved_callback_and_an_invokers_arguments_reach_user_code() {
    unknown(&file(false, "array $a, callable $cb", "return array_filter($a, $cb);"), "f");
    unknown(&file(false, "array $a, callable $cb", "usort($a, $cb); return $a;"), "f");
    unknown(
        &file(false, "mixed $v", "return preg_replace_callback('/x/', fn ($m) => 'y', $v);"),
        "f",
    );
    proven_pure(
        &file(false, "string $s", "return preg_replace_callback('/x/', fn ($m) => 'y', $s);"),
        "f",
    );
}

/// `array_search` is certified at a call site (issue #860, row 7.12): pure where
/// every reaching argument is shown object-free, `user-code-reach` where the
/// haystack is an unknown array, and no longer `no-effect-row` either way.
#[test]
fn s7_array_search_is_held_to_the_reach_rule() {
    let src = file(false, "", "return array_search(3, [1, 2, 3]);");
    proven_pure(&src, "f");
    let src = file(false, "string $s, array $h", "return array_search($s, $h);");
    let s = summary(&src, "f");
    assert!(!s.exhaustive && s.gaps == ["user-code-reach"], "{s:?}");
}

/// A call result is an argument shape (issue #877, rows 8.3, 8.4 and 8.14): what
/// the callee's declared return holds rules out the reach of the call it is handed
/// to, and an unproven result keeps it.
#[test]
fn s8_a_call_result_is_an_argument_shape() {
    // The engine constructor's `Coerced` message position, handed a `string` result.
    let message = "return new \\RuntimeException(strtoupper($s));";
    proven_pure(&file(false, "string $s", message), "f");
    // `sprintf`'s result is just as much a `string`: the call is ruled out, and the
    // literal format reads no locale and renders a `string` (ADR-0101 §3.2, D4).
    let message = "return new \\RuntimeException(sprintf('%s!', $s));";
    proven_pure(&file(false, "string $s", message), "f");
    // `count()` reaches `Countable::count` through an object, never through a list.
    proven_pure(&file(false, "string $s", "return count(explode(',', $s));"), "f");
    proven_pure(&file(false, "string $s", "return strlen(strtoupper($s));"), "f");
    // `current()` is `mixed`, and a project function with no native return may be anything.
    unknown(&file(false, "array $a", "return count(current($a));"), "f");
    let untyped = "<?php\nfunction pass($x) { return $x; }\n\
                   function f(string $s) { return new \\RuntimeException(pass($s)); }\n";
    unknown(untyped, "f");
    // Row 8.14: `strlen($s)` of an untyped `$s` keeps the call's own reach to `__toString`
    // under coercive typing, and the string conversion of its `int` result runs nothing.
    let s = summary(&file(false, "$s", "return 'x' . strlen($s);"), "f");
    assert!(!s.exhaustive && s.gaps == ["user-code-reach"], "{s:?}");
}

/// `sprintf` and `vsprintf` are held to the reach rule like the rest of the
/// fold allowlist's rows, and carry the setting reads beside it (issue #991,
/// ADR-0101): a literal format with no `f`, `g` or `G` reads no locale, and a `%s`
/// reads `precision` only for a float, so a value not shown either way is the
/// `value-dependent-read` gap. An object-free call over values shown no float is exhaustive
/// with no gap; a `%s` over a value that may be an object is `user-code-reach`. The per-row
/// table is `printf_call_site.rs`.
#[test]
fn s7_the_printf_family_carries_its_setting_reads_beside_the_reach_rule() {
    proven_pure(&file(false, "string $s, int $i", "return sprintf('%s-%d', $s, $i);"), "f");
    // A vector's elements are not read, so a `%s` over one depends on them.
    let s = summary(&file(false, "", "return vsprintf('%s-%s', ['a', 'b']);"), "f");
    assert!(!s.exhaustive && s.gaps == ["value-dependent-read"], "{s:?}");
    assert!(s.labels.is_empty(), "{s:?}");
    let s = summary(&file(false, "mixed $v", "return sprintf('%s', $v);"), "f");
    assert!(!s.exhaustive && s.gaps == ["user-code-reach", "value-dependent-read"], "{s:?}");
    assert!(s.labels.is_empty(), "{s:?}");
}
