//! The throw lane reads the catalog's knowledge, not a default (ADR-0099 §3, §4,
//! issue #864): a known builtin with no throw row is a coverage gap unless the
//! audit evidences it throwless, an operand that may reach user code is a gap in
//! this lane as in the effect lane, and unseen code (`eval`, an inclusion) leaves
//! no lane exhaustive.

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

/// A file declaring a class whose `__toString` throws, then `f` with the given
/// signature and body, in coercive mode or under `strict_types=1`.
fn file(strict: bool, signature: &str, body: &str) -> String {
    let declare = if strict { "declare(strict_types=1);\n" } else { "" };
    format!(
        "<?php\n{declare}\
         final class Name {{ public function __toString(): string {{ throw new \\RuntimeException('x'); }} }}\n\
         /** @throws void */\n\
         function f({signature}): mixed {{ {body} }}\n"
    )
}

/// The throw lane's verdict on `f`: its gap kinds, empty when exhaustive.
fn throw_gaps(src: &str) -> Vec<&'static str> {
    let s = summary(src, "f");
    assert_eq!(s.throws_exhaustive, s.throws_gaps.is_empty(), "the kinds name the bit: {s:?}");
    s.throws_gaps
}

// ---- A known builtin is not throwless by default (issue #858 item 2) -------

#[test]
fn strlen_of_an_object_is_throw_non_exhaustive_because_to_string_may_throw() {
    // `strlen($o)` converts `$o` through `__toString` under coercive typing, which
    // throws: the empty, exhaustive set the legacy default gave was a false claim.
    assert_eq!(throw_gaps(&file(false, "Name $o", "return strlen($o);")), ["user-code-reach"]);
    assert_eq!(throw_gaps(&file(false, "$o", "return strlen($o);")), ["user-code-reach"]);
}

#[test]
fn strlen_of_a_string_is_throw_exhaustive_in_both_modes() {
    assert!(throw_gaps(&file(false, "string $s", "return strlen($s);")).is_empty());
    assert!(throw_gaps(&file(true, "string $s", "return strlen($s);")).is_empty());
    // Under `strict_types=1` an object at a `string` parameter is a `TypeError`,
    // argument checking, which the lane does not model: nothing reaches user code.
    assert!(throw_gaps(&file(true, "Name $o", "return strlen($o);")).is_empty());
    assert!(throw_gaps(&file(false, "", "return strlen('abc');")).is_empty());
}

#[test]
fn a_known_builtin_nobody_audited_is_a_gap_never_throwless() {
    // `mb_strlen` has an effect-free reputation and a mined signature, and raises a
    // `ValueError` for an unknown encoding: no row, no audit, so a gap.
    let src = file(true, "string $s", "return mb_strlen($s);");
    assert_eq!(throw_gaps(&src), ["no-throw-row"]);
    // The effect lane has no row either, and says so under its own name.
    assert_eq!(summary(&src, "f").gaps, ["no-effect-row"]);
    // A truly unknown name is still the old kind.
    assert_eq!(throw_gaps(&file(true, "", "return no_such_function();")), ["unknown-function"]);
}

#[test]
fn the_audited_table_makes_the_corpus_names_exhaustive() {
    for body in [
        "return array_keys($a);",
        "return array_values($a);",
        "return array_key_exists('k', $a);",
        "return implode(',', [1, 2]);",
        "return str_replace('a', 'b', 'abc');",
        "return strtolower('ABC');",
        "return trim(' x ');",
        "return is_array($a);",
    ] {
        let src = file(true, "array $a", body);
        assert!(throw_gaps(&src).is_empty(), "{body}");
    }
}

#[test]
fn a_name_with_a_value_error_row_is_exhaustive_with_the_error_in_its_set() {
    // `dirname($p, 0)` is a `ValueError`: the name has a row, not a place on the
    // throwless table, and the row keeps the body exhaustive.
    let src = file(true, "string $p", "return dirname($p);");
    let s = summary(&src, "f");
    assert!(s.throws_exhaustive, "{s:?}");
    assert_eq!(s.throws, ["ValueError"]);
    let src = file(true, "array $a", "return max($a);");
    assert_eq!(summary(&src, "f").throws, ["ValueError"]);
}

#[test]
fn a_name_that_compares_its_elements_carries_the_error_of_a_recursive_array() {
    // Two distinct arrays that contain themselves by reference compare to an
    // `Error` ("Nesting level too deep"), and the parameter type admits them: these
    // names left the throwless table for a row (issue #881), and the row keeps the
    // body exhaustive with the `Error` in its set.
    for body in [
        "return in_array(1, [1, 2]);",
        "return array_search(1, [1, 2]);",
        "return array_unique([1, 1]);",
        "return array_replace_recursive([1], [2]);",
        "return boolval(1);",
        "return array_filter([1, 0]);",
    ] {
        let s = summary(&file(true, "", body), "f");
        assert!(s.throws_exhaustive, "{body}: {s:?}");
        assert_eq!(s.throws, ["Error"], "{body}");
    }
    // A sort takes its array by reference, so a local operand is a gap of its own
    // kind; the row is still what it says.
    for sort in ["sort", "rsort", "asort", "arsort"] {
        let s = summary(&file(true, "", &format!("$a = [2, 1]; {sort}($a); return $a;")), "f");
        assert_eq!(s.throws, ["Error"], "{sort}: {s:?}");
    }
    // The `Error` of `date_create` is an uninitialised `DateTimeZone` passed as its
    // second argument: with one argument, nothing is read and nothing is raised.
    let no_throws: &[&str] = &[];
    for (call, throws) in [
        ("date_create('now')", no_throws),
        ("date_create_immutable('now')", no_throws),
        ("date_create('now', $z)", &["Error"][..]),
        ("date_create_immutable('now', $z)", &["Error"][..]),
        ("date_create_from_format('Y', '2020')", &["ValueError"][..]),
        ("date_create_from_format('Y', '2020', $z)", &["Error", "ValueError"][..]),
    ] {
        // A `DateTimeZone` operand is an object, a reach of its own: the gap says so,
        // and the set is what the row says.
        let s = summary(&file(true, "\\DateTimeZone $z", &format!("return {call};")), "f");
        assert_eq!(s.throws, throws, "{call}");
    }
    // `ksort` compares keys, which are never arrays: still on the table.
    let s = summary(&file(true, "", "$a = [2, 1]; ksort($a); return $a;"), "f");
    assert!(s.throws_exhaustive && s.throws.is_empty(), "{s:?}");
}

// ---- array_keys: the certification gives the throw lane its arity too -------

#[test]
fn array_keys_is_throw_exhaustive_at_one_argument_only() {
    // Comparing `$filter` loosely with every element may run an object's
    // `__toString`: at two arguments the operand reaches user code.
    assert!(throw_gaps(&file(true, "array $a", "return array_keys($a);")).is_empty());
    // At one argument nothing is compared, so the set is empty: the `Error` of the
    // search form, which compares two recursive arrays, is the other arities'.
    let one = summary(&file(true, "array $a", "return array_keys($a);"), "f");
    assert!(one.throws_exhaustive && one.throws.is_empty(), "{one:?}");
    let two = summary(&file(true, "", "return array_keys([1, 2], 1);"), "f");
    assert!(two.throws_exhaustive && two.throws == ["Error"], "{two:?}");
    assert_eq!(throw_gaps(&file(true, "array $a, $v", "return array_keys($a, $v);")), ["user-code-reach"]);
    // A spread list has no arity to certify. The spread of a variable is also an
    // iteration site, and the callee may take the variable by reference.
    assert_eq!(
        throw_gaps(&file(true, "array $a", "return array_keys(...$a);")),
        ["user-code-reach", "operator-iteration"]
    );
    assert_eq!(throw_gaps(&file(true, "", "return array_keys(...[[1]]);")), ["user-code-reach"]);
    // An array that holds no object and a scalar to find rule the comparison out.
    assert!(throw_gaps(&file(true, "", "return array_keys([1, 2], 1);")).is_empty());
    assert_eq!(throw_gaps(&file(true, "array $a", "return array_keys($a, 1);")), ["user-code-reach"]);
}

// ---- Flag-dependent throws (ADR-0099 §3.3) ----------------------------------

#[test]
fn json_encode_throws_only_under_its_flag_and_only_a_readable_flag_clears_it() {
    let encode = |args: &str| throw_gaps(&file(true, "", &format!("return json_encode({args});")));
    assert!(encode("[1, 2]").is_empty(), "flags omitted: the default is 0");
    assert!(encode("[1, 2], 0").is_empty());
    assert!(encode("[1, 2], JSON_PRETTY_PRINT").is_empty());
    assert!(encode("[1, 2], JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES").is_empty());
    assert_eq!(encode("[1, 2], JSON_THROW_ON_ERROR"), ["flag-dependent-throw"]);
    assert_eq!(encode("[1, 2], \\JSON_THROW_ON_ERROR"), ["flag-dependent-throw"]);
    assert_eq!(encode("[1, 2], JSON_PRETTY_PRINT | JSON_THROW_ON_ERROR"), ["flag-dependent-throw"]);
    assert_eq!(encode("[1, 2], 4194304"), ["flag-dependent-throw"], "the bit, spelled as a number");
    // Flags nobody can read at the call: a variable, a user constant, a call.
    assert_eq!(encode("[1, 2], $flags"), ["flag-dependent-throw"]);
    assert_eq!(encode("[1, 2], MY_FLAGS"), ["flag-dependent-throw"]);
    // A named or spread list hides which argument is the flags (and which arguments
    // reach what), so both gaps are there. Kinds are listed in codec order.
    assert_eq!(
        encode("[1, 2], flags: JSON_PRETTY_PRINT"),
        ["user-code-reach", "flag-dependent-throw"]
    );
    assert_eq!(
        encode("...$args"),
        ["user-code-reach", "flag-dependent-throw", "operator-iteration"]
    );
}

#[test]
fn json_decode_keeps_its_value_error_and_gates_jsonexception_on_the_fourth_argument() {
    let decode = |args: &str| {
        let s = summary(&file(true, "string $j", &format!("return json_decode({args});")), "f");
        (s.throws_gaps, s.throws)
    };
    // Without the flag the depth check is the one thing it raises.
    assert_eq!(decode("$j, true"), (vec![], vec!["ValueError".to_owned()]));
    assert_eq!(decode("$j, true, 512, 0").0, Vec::<&str>::new());
    assert_eq!(decode("$j, true, 512, JSON_BIGINT_AS_STRING").0, Vec::<&str>::new());
    assert_eq!(decode("$j, true, 512, JSON_THROW_ON_ERROR").0, ["flag-dependent-throw"]);
    assert_eq!(decode("$j, true, 512, $flags").0, ["flag-dependent-throw"]);
    // The depth is not the flags: a constant at position 2 clears nothing.
    assert_eq!(decode("$j, true, JSON_THROW_ON_ERROR").0, Vec::<&str>::new());
}

#[test]
fn a_flag_gated_builtin_handed_over_as_a_callback_is_a_gap() {
    // The invoker chooses the arguments, flags included.
    let src = file(true, "array $xs", "return array_map('json_encode', $xs);");
    assert!(throw_gaps(&src).contains(&"flag-dependent-throw"));
}

// ---- Unseen code and reach (ADR-0099 §4, ADR-0046) --------------------------

#[test]
fn eval_and_include_leave_the_throw_lane_non_exhaustive() {
    assert_eq!(throw_gaps(&file(true, "string $c", "return eval($c);")), ["unseen-code"]);
    assert_eq!(throw_gaps(&file(true, "string $p", "return include $p;")), ["unseen-code"]);
    assert_eq!(throw_gaps(&file(true, "string $p", "return require_once $p;")), ["unseen-code"]);
    // Both lanes agree.
    let s = summary(&file(true, "string $c", "return eval($c);"), "f");
    assert_eq!((s.gaps.as_slice(), s.throws_gaps.as_slice()), (&["unseen-code"][..], &["unseen-code"][..]));
}

#[test]
fn a_callback_that_is_a_builtin_reaches_user_code_in_the_throw_lane_too() {
    // `array_map('strlen', [$o])` runs `__toString` under `strict_types=1`: the
    // invoker calls the callback coercively.
    let src = file(true, "array $xs", "return array_map('strlen', $xs);");
    assert_eq!(throw_gaps(&src), ["user-code-reach"]);
    let src = file(true, "array $xs", "return array_map('mb_strlen', $xs);");
    assert_eq!(throw_gaps(&src), ["user-code-reach", "no-throw-row"]);
}

#[test]
fn an_invokers_own_throw_row_is_read() {
    // The legacy default never read an invoker's row. `array_map` is audited.
    let src = file(true, "array $xs", "return array_map(fn($x) => $x, $xs);");
    assert!(throw_gaps(&src).is_empty());
    // `iterator_apply` is not: the invoker's own part is a gap.
    let src = file(true, "\\Traversable $t", "return iterator_apply($t, fn() => true);");
    assert!(throw_gaps(&src).contains(&"no-throw-row"));
}

#[test]
fn a_user_stream_wrapper_or_filter_is_a_gap_at_its_registration_not_at_the_io_calls() {
    // The wrapper's methods run inside later `file_exists`, `fwrite`, … : the body
    // that registers one carries the gap (ADR-0099 §4.5), and the I/O is audited.
    for body in [
        "return stream_wrapper_register('acme', 'Wrapper');",
        "return stream_filter_register('acme.*', 'Filter');",
        "return stream_filter_append($h, 'acme.upper');",
    ] {
        let gaps = throw_gaps(&file(true, "$h", body));
        assert!(gaps.contains(&"no-throw-row") && gaps.contains(&"user-code-reach"), "{body}: {gaps:?}");
    }
    assert!(throw_gaps(&file(true, "string $p", "return file_exists($p);")).is_empty());
    assert!(throw_gaps(&file(true, "$h", "return fwrite($h, 'x');")).is_empty());
}

// ---- One predicate decides what is a builtin (ADR-0099 §3.1) ----------------

#[test]
fn a_project_function_shadowing_any_known_name_is_ambiguous_in_every_pass() {
    // `levenshtein` has no colour and no row: the catalog knows it through its
    // mined signature alone, which is exactly the kind of name the effect lane's
    // narrower closure used to let a project body shadow. A shadow is ambiguous
    // for all passes now, so the call is a gap, not an edge to the project body.
    let src = "<?php
function levenshtein(string $a, string $b): int { return 0; }
function f(): int { return levenshtein('a', 'b'); }";
    let s = summary(src, "f");
    assert_eq!(s.gaps, ["unknown-function"]);
    assert_eq!(s.throws_gaps, ["unknown-function"]);
    // A name nothing knows still resolves to the project body.
    let src = "<?php
function my_distance(string $a, string $b): int { return 0; }
function f(): int { return my_distance('a', 'b'); }";
    let s = summary(src, "f");
    assert!(s.exhaustive && s.throws_exhaustive, "{s:?}");
}

/// The throw lane never reads `builtin_throws` itself: every question about what
/// a builtin raises goes through the catalog's one composition,
/// [`steins_catalog::throws_of`], so a missing row cannot be read as "nothing"
/// anywhere (ADR-0099 §3.2). The scan is over `steins-infer`'s sources.
#[test]
fn no_pass_reads_the_throw_table_directly() {
    fn rust_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("readable source dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                rust_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    assert!(files.len() > 50, "the scan found the sources: {}", files.len());
    // Spelled in pieces so this file never matches itself, and the legacy default
    // and its role enum stay gone.
    let forbidden = [
        ["builtin", "_throws"].concat(),
        ["legacy", "_throws"].concat(),
        ["Throws", "Legacy"].concat(),
        ["Throws", "Role"].concat(),
    ];
    for path in files {
        let text = std::fs::read_to_string(&path).expect("readable source");
        for word in &forbidden {
            assert!(!text.contains(word.as_str()), "{} mentions `{word}`", path.display());
        }
    }
}
