//! The float renderers in the effect lane (ADR-0101 §3.15, issue #1000, S6a): `strval`,
//! `settype`, `implode`, `join`, `print_r`, `var_export`, `json_encode`, `serialize`, `var_dump`
//! and `debug_zval_dump` read the precision cell when the value they render is a float, and only
//! then. A float, or an array literal holding one at the depth the function walks, is the proven
//! read; a value shown to hold none is no read; any other is the `value-dependent-read` gap and
//! never a label.
//!
//! The witness rows of `steins-catalog`'s `setting_precision_oracle` decide each verdict against
//! PHP; these tests pin that the call site reaches it through every call shape.

use steins_infer::{EffectSummary, effect_summary};
use steins_syntax::SourceTree;

const PRECISION: &str = "global.read.setting.precision";
const OUTPUT: &str = "io.output.buffer";
const DEPENDS: &str = "value-dependent-read";

fn summary(params: &str, body: &str) -> EffectSummary {
    let src = format!("<?php\nfunction f({params}) {{ {body} }}\n");
    let tree = SourceTree::parse(&src);
    let functions = tree.functions().to_vec();
    let classes = tree.classes().to_vec();
    effect_summary(&tree, &functions, &classes)
        .into_iter()
        .find(|s| s.symbol == "f")
        .unwrap_or_else(|| panic!("no summary for f in {src}"))
}

fn has(s: &EffectSummary, label: &str) -> bool {
    s.labels.iter().any(|l| l == label)
}

/// The call carries exactly `labels` and the body stays exhaustive, with no gap.
fn proves(params: &str, call: &str, labels: &[&str]) {
    let s = summary(params, &format!("return {call};"));
    assert_eq!(s.labels, labels, "{params} / {call}: {s:?}");
    assert!(s.exhaustive && s.gaps.is_empty(), "{params} / {call}: {s:?}");
}

/// The call carries exactly `labels` and no value-dependent-read gap. An array literal with a
/// variable in it may still reach user code (the reach gap, which is not this slice's), so the
/// body need not be exhaustive.
fn reads_only(params: &str, call: &str, labels: &[&str]) {
    let s = summary(params, &format!("return {call};"));
    assert_eq!(s.labels, labels, "{params} / {call}: {s:?}");
    assert!(!s.gaps.contains(&DEPENDS), "{params} / {call}: {s:?}");
}

/// The call is the value-dependent-read gap and carries no precision label.
fn depends(params: &str, call: &str) {
    let s = summary(params, &format!("return {call};"));
    assert!(s.gaps.contains(&DEPENDS) && !s.exhaustive, "{params} / {call}: {s:?}");
    assert!(!has(&s, PRECISION), "{params} / {call}: {s:?}");
}

/// A float literal, a `float` parameter the frame never writes, a float constant and a cast to
/// float are the proven read at every renderer that converts its value whole.
#[test]
fn a_float_is_the_proven_read() {
    for call in [
        "strval(1.5)",
        "strval($f)",
        "strval(M_PI)",
        "strval((float) $n)",
        "strval($flag ? 1.5 : 2.5)",
        "json_encode(1.5)",
        "serialize($f)",
        "implode(',', [1.5])",
        "join(',', [1.5])",
        "implode([1, 1.5])",
        "implode($f, ['a', 'b'])",
    ] {
        proves("float $f, int $n, bool $flag", call, &[PRECISION]);
    }
    reads_only("float $f, int $n", "implode(',', [$n, $f])", &[PRECISION]);
}

/// The dumpers write to the output channel and read the cell; return mode removes the output
/// label and not the read.
#[test]
fn the_dumpers_read_the_cell_beside_the_output() {
    for call in ["print_r(1.5)", "var_export(1.5)", "var_dump(1.5)", "debug_zval_dump(1.5)"] {
        let s = summary("", &format!("{call}; return 1;"));
        assert_eq!(s.labels, [PRECISION, OUTPUT], "{call}: {s:?}");
        assert!(s.exhaustive && s.gaps.is_empty(), "{call}: {s:?}");
    }
    proves("", "print_r(1.5, true)", &[PRECISION]);
    proves("", "var_export(1.5, true)", &[PRECISION]);
    proves("", "print_r([1, 'a'], true)", &[]);
    proves("", "var_export([1, 'a'], true)", &[]);
    let s = summary("", "var_dump(1, 'a'); return 1;");
    assert_eq!(s.labels, [OUTPUT], "{s:?}");
    assert!(s.exhaustive && s.gaps.is_empty(), "{s:?}");
    let s = summary("", "var_dump(1, 'a', [2.5]); return 1;");
    assert_eq!(s.labels, [PRECISION, OUTPUT], "every argument is rendered: {s:?}");
}

/// A walked renderer finds a float at any depth of an array literal.
#[test]
fn the_walkers_find_a_float_at_any_depth() {
    for call in [
        "json_encode([1, [2.5]])",
        "json_encode(['a' => ['b' => [2.5]]])",
        "serialize([[2.5]])",
        "print_r([[[2.5]]], true)",
        "var_export([[2.5]], true)",
        "json_encode([1, $flag ? 2.5 : 3.5])",
    ] {
        proves("bool $flag", call, &[PRECISION]);
    }
    reads_only("", "json_encode((array) 2.5)", &[PRECISION]);
    let s = summary("", "var_dump([[2.5]]); debug_zval_dump([2.5]); return 1;");
    assert_eq!(s.labels, [PRECISION, OUTPUT], "{s:?}");
}

/// `implode` converts each element of its array whole, one level down: an element that is an
/// array is `"Array"` and holds nothing read, even where it holds a float.
#[test]
fn implode_reads_one_level() {
    proves("", "implode(',', [[1.5]])", &[]);
    proves("", "implode(',', [1, 'a', [2.5]])", &[]);
    proves("", "implode(',', [1, 2.5, [3.5]])", &[PRECISION]);
    proves("", "implode(',', [])", &[]);
    proves("", "implode([1, 'a'])", &[]);
    // The element that is an array is `"Array"` whatever it holds, so the read is dropped; the
    // array the call hands the walk may reach user code, which is the reach gap and not this one.
    let s = summary("$x", "return implode(',', [[$x]]);");
    assert!(!has(&s, PRECISION) && !s.gaps.contains(&DEPENDS), "{s:?}");
}

/// A value shown to hold no float drops the read, and the call is exhaustive.
#[test]
fn a_value_free_of_floats_reads_nothing() {
    for call in [
        "strval(42)",
        "strval('a')",
        "strval(true)",
        "strval(null)",
        "strval($n)",
        "strval($s)",
        "strval(PHP_INT_MAX)",
        "strval([1.5])",
        "json_encode(1)",
        "json_encode([1, 'a', null, true])",
        "json_encode(['a' => ['b' => 1]])",
        "json_encode([1], JSON_PRETTY_PRINT | JSON_PRESERVE_ZERO_FRACTION)",
        "serialize([1, 'a'])",
        "implode(',', [1, 'a'])",
        "implode(',', explode(',', $s))",
        "join(',', array_keys([1, 2]))",
        "number_format(1.5, 2)",
        "round(1.5)",
        "intval(1.5)",
        "floor(1.5)",
    ] {
        proves("int $n, string $s", call, &[]);
    }
    reads_only("int $n, string $s", "implode($s, [$n, $s])", &[]);
}

/// The operators that convert a float to a string are no calls and stay unlabelled (D4).
#[test]
fn the_operator_sites_stay_unlabelled() {
    for call in ["(string) $f", "\"$f\"", "'x' . $f", "$f . ''"] {
        proves("float $f", call, &[]);
    }
    let s = summary("float $f", "echo $f; return 1;");
    assert!(!has(&s, PRECISION), "{s:?}");
}

/// A value the scan shows nothing of is the gap and no label: an untyped parameter, an array the
/// declaration does not describe, a local, an element read, a call with no declared return, a
/// property, an object, a `float|int` union and a nullable float.
#[test]
fn any_other_value_is_the_gap() {
    for call in [
        "strval($x)",
        "strval($a['k'])",
        "strval($n + 1)",
        "strval(g())",
        "json_encode($x)",
        "json_encode($arr)",
        "json_encode([$x])",
        "json_encode(['a' => [$x]])",
        "json_encode($o)",
        "serialize($o)",
        "implode(',', $arr)",
        "implode(',', [$x])",
        "implode($x, [1])",
        "implode(',', array_map('trim', $arr))",
        "print_r($arr, true)",
        "var_export($x, true)",
        "strval($u)",
        "strval($nf)",
    ] {
        depends("$x, array $arr, object $o, int|float $u, ?float $nf, int $n, array $a", call);
    }
    depends("array $arr", "var_dump(1, $arr)");
}

/// A local is read for the value and not for what an array it holds contains: one assigned an
/// array literal is not shown to hold no float.
#[test]
fn a_local_is_not_read_for_what_an_array_holds() {
    for body in [
        "$a = [1.5]; return json_encode($a);",
        "$a = [1, 2]; return json_encode($a);",
        "$a = [1.5]; return implode(',', $a);",
        "$a = 1.5; return strval($a);",
    ] {
        let s = summary("", body);
        assert!(s.gaps.contains(&DEPENDS) && !has(&s, PRECISION), "{body}: {s:?}");
    }
}

/// A parameter the frame writes is not read for what it holds, a walked one included.
#[test]
fn a_parameter_the_frame_writes_is_not_read() {
    let s = summary("string $s", "$s = [1.5]; return json_encode($s);");
    assert!(s.gaps.contains(&DEPENDS) && !has(&s, PRECISION), "{s:?}");
    let s = summary("float $f", "$f = 1; return json_encode($f);");
    assert!(s.gaps.contains(&DEPENDS) && !has(&s, PRECISION), "{s:?}");
}

/// `json_encode`: `JSON_NUMERIC_CHECK`, or flags the scan cannot read, make a value free of
/// floats undecided, since a numeric string becomes a number; a `$depth` makes a float undecided,
/// since a deep value is refused before it is written.
#[test]
fn json_encode_flags_and_depth() {
    proves("", "json_encode([1.5], JSON_PRETTY_PRINT)", &[PRECISION]);
    proves("", "json_encode([1.5], JSON_NUMERIC_CHECK)", &[PRECISION]);
    proves("int $flags", "json_encode([1.5], $flags)", &[PRECISION]);
    depends("", "json_encode([1.5], 0, 1)");
    depends("", "json_encode([1.5], 0, 512)");
    depends("", "json_encode(['1.5'], JSON_NUMERIC_CHECK)");
    depends("", "json_encode([1], JSON_NUMERIC_CHECK)");
    depends("int $flags", "json_encode([1], $flags)");
    proves("", "json_encode([1], JSON_FORCE_OBJECT, 5)", &[]);
}

/// `settype` renders only to a string, and only a type literal says so; its variable is rebound
/// by the call itself, so the value is never shown a float.
#[test]
fn settype_reads_for_the_string_type_only() {
    for ty in ["int", "integer", "float", "bool", "array", "null"] {
        let s = summary("float $f", &format!("settype($f, '{ty}'); return $f;"));
        assert!(!has(&s, PRECISION) && !s.gaps.contains(&DEPENDS), "{ty}: {s:?}");
    }
    for ty in ["'string'", "'STRING'"] {
        let s = summary("float $f", &format!("settype($f, {ty}); return $f;"));
        assert!(s.gaps.contains(&DEPENDS) && !has(&s, PRECISION), "{ty}: {s:?}");
    }
    let s = summary("float $f, string $t", "settype($f, $t); return $f;");
    assert!(s.gaps.contains(&DEPENDS) && !has(&s, PRECISION), "{s:?}");
}

/// A renderer handed over as a callback renders what its invoker chooses, and a call whose
/// arguments the scan cannot place by position is undecided: neither is a label.
#[test]
fn a_callback_and_an_unreadable_argument_list_are_the_gap() {
    depends("array $xs", "array_map('strval', $xs)");
    depends("array $xs", "array_map('json_encode', $xs)");
    depends("float $f", "strval(value: $f)");
    depends("array $xs", "strval(...$xs)");
    depends("array $xs", "implode(',', ...$xs)");
}

/// Both inis share the cell, and `ini_set` of either name is its write.
#[test]
fn both_inis_are_the_one_cell() {
    let s = summary("", "return ini_set('serialize_precision', '3');");
    assert!(has(&s, "global.write.setting.precision") && has(&s, PRECISION));
    let s = summary("", "return ini_set('precision', '3');");
    assert!(has(&s, "global.write.setting.precision") && has(&s, PRECISION));
}

/// Every name the syntax layer collects evidence for reaches the engine's gate: one proven read
/// per renderer, so a name added to one list and not the other fails here.
#[test]
fn every_renderer_is_gated() {
    for call in [
        "strval(1.5)",
        "implode(',', [1.5])",
        "join(',', [1.5])",
        "print_r(1.5, true)",
        "var_export(1.5, true)",
        "json_encode(1.5)",
        "serialize(1.5)",
    ] {
        proves("", call, &[PRECISION]);
    }
    for call in ["var_dump(1.5)", "debug_zval_dump(1.5)"] {
        let s = summary("", &format!("{call}; return 1;"));
        assert_eq!(s.labels, [PRECISION, OUTPUT], "{call}: {s:?}");
    }
}
