//! The call-site read of a literal printf format and of a literal strict flag
//! (ADR-0101 §3.2 and D4, ADR-0021's call-site refinements, issues #860 and #991):
//! the S7 witness table of the Run 2 re-design (rows 7.1 to 7.16, PHP 8.5.11) in
//! both lanes, and the locale and `precision` reads a call carries: a proven label where the
//! read is unconditional for the call as written, nothing where the site shows there is none,
//! and a `value-dependent-read` gap where it depends on a value or a format the site cannot see
//! (the calibration of ADR-0101 §3.2).
//!
//! `S` has a `__toString` that echoes, so a value that reaches it is a gap in both
//! lanes. Each row is a function `f` over the given signature.

use steins_infer::{EffectSummary, effect_summary};
use steins_syntax::SourceTree;

const LOCALE: &str = "global.read.setting.locale";
const PRECISION: &str = "global.read.setting.precision";
const REACH: &str = "user-code-reach";
const DEPENDS: &str = "value-dependent-read";

fn summary(src: &str, symbol: &str) -> EffectSummary {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let classes = tree.classes().to_vec();
    effect_summary(&tree, &functions, &classes)
        .into_iter()
        .find(|s| s.symbol == symbol)
        .unwrap_or_else(|| panic!("no summary for {symbol}"))
}

/// Whether the summary carries `label`.
fn has(s: &EffectSummary, label: &str) -> bool {
    s.labels.iter().any(|l| l == label)
}

/// `function f(<signature>) { <body> }` after a `S` class whose `__toString` runs code.
fn row(signature: &str, body: &str) -> EffectSummary {
    let src = format!(
        "<?php\nclass S {{ public function __toString(): string {{ echo 'x'; return 'x'; }} }}\n\
         function f({signature}) {{ {body} }}\n"
    );
    summary(&src, "f")
}

/// A row that stays exhaustive in the effect lane and the throw lane, with `labels`.
#[track_caller]
fn exhaustive(signature: &str, body: &str, labels: &[&str]) {
    let s = row(signature, body);
    assert!(s.exhaustive && s.gaps.is_empty(), "{body}: effect lane {s:?}");
    assert!(s.throws_exhaustive && s.throws_gaps.is_empty(), "{body}: throw lane {s:?}");
    assert_eq!(s.labels, labels, "{body}: {s:?}");
}

/// A row that is `user-code-reach` in both lanes, with `labels` and no other gap.
#[track_caller]
fn reaches(signature: &str, body: &str, labels: &[&str]) {
    let s = row(signature, body);
    assert_eq!(s.gaps, [REACH], "{body}: effect lane {s:?}");
    assert!(s.throws_gaps.contains(&REACH), "{body}: throw lane {s:?}");
    assert_eq!(s.labels, labels, "{body}: {s:?}");
}

/// A row whose setting read depends on a value or a format the site cannot see: the effect
/// lane names `value-dependent-read`, with `labels` proven beside it, and the throw lane has
/// no such kind. `with_reach` says whether `user-code-reach` stands beside it.
#[track_caller]
fn depends(signature: &str, body: &str, labels: &[&str], with_reach: bool) {
    let s = row(signature, body);
    let expected: &[&str] = if with_reach { &[REACH, DEPENDS] } else { &[DEPENDS] };
    assert_eq!(s.gaps, expected, "{body}: effect lane {s:?}");
    assert!(!s.exhaustive, "{body}: {s:?}");
    assert!(!s.throws_gaps.contains(&DEPENDS), "{body}: throw lane {s:?}");
    assert_eq!(s.labels, labels, "{body}: {s:?}");
}

/// What a row says of the `precision` read: `"proven"` (the label, no gap of its own),
/// `"gap"` (no label, `value-dependent-read`) or `"none"` (neither).
#[track_caller]
fn precision(signature: &str, body: &str) -> &'static str {
    let s = row(signature, body);
    let label = has(&s, PRECISION);
    let gap = s.gaps.contains(&DEPENDS);
    assert!(!(label && gap), "{body}: a label and its gap together: {s:?}");
    if label {
        "proven"
    } else if gap {
        "gap"
    } else {
        "none"
    }
}

/// Rows 7.1, 7.4, 7.6, 7.9 and 7.11: an object handed to a numeric conversion becomes
/// a number with a warning and runs no `__toString`, and a `%s` over a literal reaches
/// nothing, so the call is exhaustive in both lanes. The positional forms and the
/// escaped `%%` are read as the engine reads them.
#[test]
fn s7_a_value_no_percent_s_names_reaches_nothing() {
    // 7.1
    exhaustive("S $o", "return sprintf('%d', $o);", &[]);
    // 7.4: `'a'` lands on `%s`, `$o` on `%d`.
    exhaustive("S $o", "return sprintf('%s %d', 'a', $o);", &[]);
    // 7.6
    exhaustive("S $o", "return sprintf('%2$d %1$s', 'a', $o);", &[]);
    // 7.9: flags, width, precision, `'c` padding and `%%`.
    exhaustive("S $o", "return sprintf('%.2f', $o);", &[LOCALE]);
    exhaustive("S $o", "return sprintf('100%% %d', $o);", &[]);
    exhaustive(
        "S $o",
        "return sprintf(\"%'*10d|%-5s|%+.1e|%x|%c|%u|%b\", $o, 'x', $o, $o, $o, $o, $o);",
        &[],
    );
    let s = row("S $o", "return printf(\"%05d\\n\", $o);");
    assert!(s.exhaustive && s.gaps.is_empty(), "{s:?}");
    assert_eq!(s.labels, ["io.output.buffer"], "{s:?}");
    // 7.11: too few arguments is `ArgumentCountError`, argument checking and not user code.
    exhaustive("S $o", "return sprintf('%d %d', $o);", &[]);
}

/// Rows 7.2, 7.3, 7.5, 7.7, 7.8 and 7.10 must stay: a position some `%s` names reaches
/// `__toString`, the strongest conversion of a value wins, and a format the call does not
/// show or the parser cannot read keeps the reach.
#[test]
fn s7_a_value_a_percent_s_names_still_reaches_to_string() {
    // 7.2, 7.3, 7.5. An `S` renders through its `__toString`, which returns a string, so
    // the `%s` reads no `precision` here.
    reaches("S $o", "return sprintf('%s', $o);", &[]);
    reaches("S $o", "return sprintf('%d %s', 1, $o);", &[]);
    reaches("S $o", "return sprintf('%2$s %1$d', 1, $o);", &[]);
    // 7.7: one argument, two conversions, and the `%s` wins.
    reaches("S $o", "return sprintf('%1$d|%1$s', $o);", &[]);
    // The same `%s` over a value that may be a float or an object depends on it.
    depends("mixed $o", "return sprintf('%s', $o);", &[], true);
    // 7.8: no literal format, so both reads depend on it.
    depends("string $fmt, S $o", "return sprintf($fmt, $o);", &[], true);
    // 7.10: a malformed format may have run an earlier `%s`, so the whole format is unreadable.
    depends("S $o", "return sprintf('%q', $o);", &[], true);
    depends("S $o", "return sprintf('%s %q', $o);", &[], true);
    depends("S $o", "return sprintf('%*d', 3, $o);", &[], true);
    // A named or spread list hides the format and the positions.
    depends("S $o", "return sprintf(format: '%d', values: $o);", &[], true);
    let s = row("array $a", "return sprintf('%d', ...$a);");
    assert!(s.gaps.contains(&REACH) && s.gaps.contains(&DEPENDS), "{s:?}");
    assert!(s.labels.is_empty(), "{s:?}");
}

/// Rows 7.12, 7.13 and 7.14: a literal `true` strict flag compares by identity and runs
/// no `__toString`; no flag, a literal `false` and a flag that is not a literal keep
/// the loose comparison. `array_search` has the same rule.
#[test]
fn s7_a_literal_true_strict_flag_reaches_nothing() {
    // 7.12
    exhaustive("S $o, array $h", "return in_array($o, $h, true);", &[]);
    exhaustive("S $o, array $h", "return in_array($o, $h, TRUE);", &[]);
    exhaustive("S $o, array $h", "return array_search($o, $h, true);", &[]);
    exhaustive("S $o", "return in_array($o, ['7'], true);", &[]);
    // 7.13
    for body in [
        "return in_array($o, $h);",
        "return in_array($o, $h, false);",
        "return array_search($o, $h);",
        "return array_search($o, $h, false);",
    ] {
        reaches("S $o, array $h", body, &[]);
    }
    // 7.14: not a literal `true`.
    for body in [
        "return in_array($o, $h, $f);",
        "return in_array($o, $h, 1);",
        "return in_array($o, $h, !false);",
        "return in_array($o, $h, strict: true);",
    ] {
        let s = row("S $o, array $h, bool $f", body);
        assert!(s.gaps.contains(&REACH), "{body}: {s:?}");
    }
    // The flag rules out its own call only: the loose call beside it still reaches.
    reaches("S $o, array $h", "return in_array(1, $h) && in_array($o, $h, true);", &[]);
}

/// Row 7.15: a vector's array position is `Inert` when no conversion is `%s`, and the
/// `%s` form keeps the reach. `vprintf` follows.
#[test]
fn s7_a_vector_reaches_only_through_a_percent_s() {
    exhaustive("array $a", "return vsprintf('%d-%d', $a);", &[]);
    exhaustive("array $a", "return vsprintf('%F', $a);", &[]);
    // A vector's elements are not read, so a `%s` over one depends on them.
    depends("array $a", "return vsprintf('%s', $a);", &[], true);
    depends("array $a", "return vsprintf('%d-%s', $a);", &[], true);
    depends("string $fmt, array $a", "return vsprintf($fmt, $a);", &[], true);
    let s = row("array $a", "return vprintf('%d', $a);");
    assert!(s.exhaustive && s.gaps.is_empty(), "{s:?}");
    assert_eq!(s.labels, ["io.output.buffer"], "{s:?}");
    // `printf` and `vprintf` keep the output label, which is unconditional, beside the gap.
    depends("string $f, array $a", "vprintf($f, $a);", &["io.output.buffer"], true);
    depends("string $f", "printf($f, 1);", &["io.output.buffer"], false);
}

/// Row 7.16 must stay: `fprintf` has no row of any kind, which is a stream-writer
/// catalog hole (#989) and not this slice's.
#[test]
fn s7_fprintf_keeps_no_row() {
    let s = row("S $o", "return fprintf(STDOUT, \"%d\\n\", $o);");
    assert_eq!(s.gaps, ["no-effect-row"], "{s:?}");
    assert!(s.throws_gaps.contains(&"no-throw-row"), "{s:?}");
}

/// ADR-0101 §3.2: the locale rows at the call site. The literal `'%d-%s'` is `{}`, `%.2f`
/// keeps the read, a format the call does not show is the gap and never a label, `%F` drops
/// the read.
#[test]
fn the_locale_read_follows_the_literal_format() {
    exhaustive("", "return sprintf('%d-%s', 1, 'a');", &[]);
    exhaustive("float $x", "return sprintf('%.2f', $x);", &[LOCALE]);
    exhaustive("float $x", "return sprintf('%.2F', $x);", &[]);
    depends("string $fmt, float $x", "return sprintf($fmt, $x);", &[], false);
    exhaustive("float $x", "return vsprintf('%F', [$x]);", &[]);
    exhaustive("float $x", "return vsprintf('%G', [$x]);", &[LOCALE]);
    // `%f` anywhere in the format keeps the read, and `%%f` is no conversion.
    exhaustive("float $x", "return sprintf('%d %f', 1, $x);", &[LOCALE]);
    exhaustive("float $x", "return sprintf('100%%f');", &[]);
    // The `s` and `f` together carry both reads, each unconditional: the value is a float.
    exhaustive("float $x", "return sprintf('%s %f', $x, $x);", &[LOCALE, PRECISION]);
    // A `%f` beside a `%s` of an unproven value: the locale read is proven, the other is not.
    depends("$x, float $y", "return sprintf('%s %f', $x, $y);", &[LOCALE], true);
}

/// D4 and the calibration (ADR-0101 §3.2, §3.8): a `%s` reads `precision` only if its value is
/// a float at run time (`ini_set('precision', '3')` turns `1234.5678` into `1.23E+3`). A
/// value shown a float is the proven label, a value shown no float is nothing, and any other
/// is `value-dependent-read`. No other conversion reads it.
#[test]
fn a_percent_s_reads_precision_only_for_a_float() {
    for (signature, body) in [
        ("float $x", "return sprintf('%s', $x);"),
        ("float $x", "return sprintf('%1$d %1$s', $x);"),
        ("float $x", "return sprintf('%d %s', 1, $x);"),
        ("", "return sprintf('%s', 1.5);"),
        ("", "return sprintf('%s', -1.5);"),
        ("", "return sprintf('%s', 9223372036854775808);"),
        ("$x", "return sprintf('%s', (float) $x);"),
        ("$x", "return sprintf('%s', $x ? 1.5 : 2.5);"),
        ("", "return sprintf('%s', M_PI);"),
        ("", "return sprintf('%s', round(1.5));"),
        ("float $x", "$y = $x; return sprintf('%s%s', $x, 'a');"),
    ] {
        assert_eq!(precision(signature, body), "proven", "{body}");
    }
    for (signature, body) in [
        ("mixed $x", "return sprintf('%s', $x);"),
        ("$x", "return sprintf('%s', $x);"),
        ("int|float $x", "return sprintf('%s', $x);"),
        ("?float $x", "return sprintf('%s', $x);"),
        ("$x", "return sprintf('%s', $x + 1);"),
        ("$x", "return sprintf('%s', $x ? 1.5 : 'a');"),
        ("$x", "return sprintf('%s', $x ?: 'a');"),
        ("$x", "return sprintf('%s', $x ?? 'a');"),
        ("int $x", "$x = $x / 2; return sprintf('%s', $x);"),
        ("int $x", "$x++; return sprintf('%s', $x);"),
        ("string $x", "$x = 1.5; return sprintf('%s', $x);"),
        ("string $x", "$x = h(); return sprintf('%s', $x);"),
        ("string $x", "return sprintf('%s', h($x));"),
        ("array $a", "foreach ($a as $x) { return sprintf('%s', $x); }"),
        ("array $a", "return sprintf('%s', $a[0]);"),
        ("$o", "return sprintf('%s', $o->p);"),
        ("$o", "return sprintf('%s', $o->m());"),
        ("", "return sprintf('%s', $this->p);"),
        ("", "return sprintf('%s', FOO);"),
        ("", "return sprintf('%s', Foo::BAR);"),
        ("string $x", "settype($x, 'float'); return sprintf('%s', $x);"),
        ("", "return sprintf('%s', current([1]));"),
        ("float $x", "$x = 'a'; return sprintf('%s', $x);"),
        ("float $x", "$y = 1; $x += $y; return sprintf('%s', $x);"),
        // Implicitly nullable: called with the default it holds `null`, as `?float` may.
        ("float $x = null", "return sprintf('%s', $x);"),
        ("?float $x = 1.5", "return sprintf('%s', $x);"),
    ] {
        assert_eq!(precision(signature, body), "gap", "{body}");
    }
    for (signature, body) in [
        ("string $x", "return sprintf('%s', $x);"),
        ("?string $x = null", "return sprintf('%s', $x);"),
        ("int $x", "return sprintf('%s', $x);"),
        ("bool $x", "return sprintf('%s', $x);"),
        ("array $x", "return sprintf('%s', $x);"),
        ("int|string $x", "return sprintf('%s', $x);"),
        ("S $x", "return sprintf('%s', $x);"),
        ("string $x", "$x = (string) $x; return sprintf('%s', $x);"),
        ("string $x", "$x .= 'y'; return sprintf('%s', $x);"),
        ("int $x", "unset($x); return sprintf('%s', $x);"),
        ("", "return sprintf('%s', 'a');"),
        ("", "return sprintf('%s', 7);"),
        ("", "return sprintf('%s', -7);"),
        ("", "return sprintf('%s', null);"),
        ("", "return sprintf('%s', true);"),
        ("$x", "return sprintf('%s', 'a' . $x);"),
        ("$x", "return sprintf('%s', \"a{$x}\");"),
        ("$x", "return sprintf('%s', $x === 1);"),
        ("$x", "return sprintf('%s', (string) $x);"),
        ("$x", "return sprintf('%s', (int) $x);"),
        ("$x", "return sprintf('%s', $x ? 'a' : 'b');"),
        ("$x", "return sprintf('%s', $x ? 'a' : (string) $x);"),
        ("string $x", "return sprintf('%s', $x ?: 'a');"),
        ("string $x", "return sprintf('%s', $x ?? 'a');"),
        ("string $x", "return sprintf('%s', $x ? 1 : 'a');"),
        ("$x", "return sprintf('%s', strlen('a'));"),
        ("", "return sprintf('%s', PHP_EOL);"),
        ("", "return sprintf('%s', PHP_INT_MAX);"),
        ("$x", "return sprintf('%s', PHP_EOL . $x);"),
        ("", "$a = 'x'; $b = $a . 'y'; return sprintf('%s%s', $a, $b);"),
        ("", "$a = 1; return sprintf('%s', $a);"),
        ("", "$a = strlen('a'); return sprintf('%s', $a);"),
        ("", "$a = 'a'; $a = strtoupper($a); return sprintf('%s', $a);"),
        ("", "return sprintf('%s', basename('a'));"),
        ("string $x", "return sprintf('%s', strtoupper($x));"),
        ("string $x", "return sprintf('%s', strlen($x));"),
        ("string $x", "return sprintf('%s', $x ? strtoupper($x) : 'a');"),
        ("int $x", "return sprintf('%s', $x ? strlen('a') : $x);"),
    ] {
        assert_eq!(precision(signature, body), "none", "{body}");
    }
    // No other conversion reads it, and a `%s` that no value reaches renders nothing.
    for body in [
        "return sprintf('%d', $x);",
        "return sprintf('%5.1F|%e|%E|%h|%H|%u|%c|%o|%x|%X|%b', $x, $x, $x, $x, $x, $x, $x, $x, $x, $x, $x);",
        "return sprintf('%d %s', $x, 'a');",
        "return sprintf('%s');",
        "return sprintf('%2$s', 'a');",
        "return sprintf('%s %s', 'a');",
        "return sprintf('plain');",
    ] {
        assert_eq!(precision("float $x", body), "none", "{body}");
    }
    // Several `%s`: a float among them is the proven read, and the gap needs one the site
    // cannot place with no float among them.
    assert_eq!(precision("float $x, $y", "return sprintf('%s %s', $x, $y);"), "proven");
    assert_eq!(precision("string $x, $y", "return sprintf('%s %s', $x, $y);"), "gap");
}

/// A class constant, a typed `$this->p` or `self::$p`, a project function or method and a
/// builtin are read by what they declare or hold: a float is proven, a string or an integer is
/// nothing, and `float|int`, `mixed`, an untyped one and an unknown name are the gap.
#[test]
fn a_percent_s_reads_the_declaration_of_a_property_constant_or_call_result() {
    let class = |members: &str, body: &str| {
        let src = format!(
            "<?php\nfinal class C {{ {members} public function f() {{ {body} }} }}\n\
             function g(): string {{ return 'a'; }}\nfunction h(): float {{ return 1.5; }}\n\
             function k() {{ return 1; }}\nfunction n(): ?float {{ return null; }}\n"
        );
        let s = summary(&src, "C::f");
        let label = has(&s, PRECISION);
        let gap = s.gaps.contains(&DEPENDS);
        assert!(!(label && gap), "{members} {body}: {s:?}");
        if label { "proven" } else if gap { "gap" } else { "none" }
    };
    let this = "return sprintf('%s', $this->p);";
    let stat = "return sprintf('%s', self::$p);";
    let konst = "return sprintf('%s', self::K);";
    for (members, body, expected) in [
        ("private int $p = 0;", this, "none"),
        ("private string $p = '';", this, "none"),
        ("private ?array $p = null;", this, "none"),
        ("private float $p = 0.0;", this, "proven"),
        ("private ?float $p = null;", this, "gap"),
        ("private int|float $p = 0;", this, "gap"),
        ("private mixed $p = 0;", this, "gap"),
        ("private $p = 0;", this, "gap"),
        ("", "return sprintf('%s', $this->q);", "gap"),
        ("private static float $p = 0.5;", stat, "proven"),
        ("private static string $p = '';", stat, "none"),
        ("private static int $p = 1;", stat, "none"),
        ("private static $p = 1;", stat, "gap"),
        ("private static ?float $p = null;", stat, "gap"),
        ("private static float $p = 0.5;", "return sprintf('%s', $this->p);", "gap"),
        ("const K = 1.5;", konst, "proven"),
        // A typed constant holds its declared type: an integer literal under `float` is a float.
        ("const float K = 123456789;", konst, "proven"),
        ("const ?float K = 1;", konst, "proven"),
        ("const float K = 1.5;", konst, "proven"),
        ("const ?float K = null;", konst, "gap"),
        ("const int K = 1;", konst, "none"),
        ("const string K = 'a';", konst, "none"),
        ("const int|float K = 1;", konst, "gap"),
        ("const mixed K = 1.5;", konst, "gap"),
        ("const K = 123456789;", konst, "none"),
        ("const K = 'a';", konst, "none"),
        ("const K = 2;", konst, "none"),
        ("const K = true;", konst, "none"),
        ("const K = null;", konst, "none"),
        ("const K = 1 + 1;", konst, "gap"),
        ("const K = [1];", konst, "gap"),
        ("", "return sprintf('%s', self::MISSING);", "gap"),
        ("", "return sprintf('%s', static::K);", "gap"),
        ("const K = 'a';", "return sprintf('%s', C::K);", "none"),
        ("private function m(): string { return ''; }", "return sprintf('%s', $this->m());", "none"),
        ("private function m(): float { return 1.5; }", "return sprintf('%s', $this->m());", "proven"),
        ("private function m(): ?float { return 1.5; }", "return sprintf('%s', $this->m());", "gap"),
        ("private function m() { return 1; }", "return sprintf('%s', $this->m());", "gap"),
        ("", "return sprintf('%s', g());", "none"),
        ("", "return sprintf('%s', h());", "proven"),
        ("", "return sprintf('%s', n());", "gap"),
        ("", "return sprintf('%s', k());", "gap"),
        ("", "$x = g(); return sprintf('%s', $x);", "none"),
        ("", "$x = g(); $x = k(); return sprintf('%s', $x);", "gap"),
        ("", "$x = h(); return sprintf('%s', $x);", "gap"),
        ("", "return sprintf('%s', g() ?: h());", "gap"),
        ("", "return sprintf('%s', strlen('a'));", "none"),
        ("", "return sprintf('%s', round(1.5));", "proven"),
        ("", "return sprintf('%s', floor(1));", "proven"),
        ("", "return sprintf('%s', json_encode(1));", "none"),
        ("", "return sprintf('%s', implode(',', [1]));", "none"),
        ("", "return sprintf('%s', current([1]));", "gap"),
        ("", "return sprintf('%s', array_sum([1]));", "gap"),
        ("", "return sprintf('%s', unknown_fn());", "gap"),
        ("", "return sprintf('%s', numfmt_get_attribute($f, 1));", "gap"),
    ] {
        assert_eq!(class(members, body), expected, "{members} {body}");
    }
}

/// A parameter proves its type only while the frame keeps the binding it was called with:
/// a callee that can take it by reference rebinds it (`settype`), a project function
/// declaring the parameter by reference does too, and one taking it by value does not.
#[test]
fn a_rebound_parameter_is_no_evidence() {
    let src = "<?php\nfunction byref(&$x) { $x = 1.5; }\nfunction byval($x) { return $x; }\n\
               function a(string $s) { byref($s); return sprintf('%s', $s); }\n\
               function b(string $s) { byval($s); return sprintf('%s', $s); }\n\
               function c(string $s) { $f = fn() => $s; return sprintf('%s', $s); }\n\
               function d(string $s) { $f = function () use (&$s) { $s = 1.5; }; return sprintf('%s', $s); }\n\
               function e(string $s) { $r = &$s; $r = 1.5; return sprintf('%s', $s); }\n\
               function g(float $s) { byref($s); return sprintf('%s', $s); }\n\
               function h(float $s) { byval($s); return sprintf('%s', $s); }\n";
    let kind = |name: &str| {
        let s = summary(src, name);
        if has(&s, PRECISION) {
            "proven"
        } else if s.gaps.contains(&DEPENDS) {
            "gap"
        } else {
            "none"
        }
    };
    assert_eq!(kind("a"), "gap");
    assert_eq!(kind("b"), "none");
    assert_eq!(kind("c"), "none");
    assert_eq!(kind("d"), "gap");
    assert_eq!(kind("e"), "gap");
    assert_eq!(kind("g"), "gap", "a float parameter some call may rebind is not shown a float");
    assert_eq!(kind("h"), "proven");
}

/// A builtin handed over as a callback is called with a format of the invoker's choosing, so
/// its setting reads are the gap and never labels.
#[test]
fn a_printf_name_handed_over_as_a_callback_is_the_gap() {
    for body in ["return array_map('sprintf', ['%s'], [1]);", "return array_map(sprintf(...), [1]);"] {
        let s = row("", body);
        assert!(s.gaps.contains(&DEPENDS), "{body}: {s:?}");
        assert!(!has(&s, LOCALE) && !has(&s, PRECISION), "{body}: {s:?}");
    }
}

/// The `Throwable` accessors are read for their declared return, which holds whatever the
/// property did: `getMessage()` is a `string` and `getLine()` an `int`, so neither is a float,
/// although the same accessors stay out of the object-free reading (ADR-0021's 2026-10-03 note on
/// call results).
#[test]
fn a_throwable_accessor_result_is_no_float() {
    for call in ["$e->getMessage()", "$e->getFile()", "$e->getLine()", "$e->getTraceAsString()"] {
        let body = format!("return sprintf('%s', {call});");
        assert_eq!(precision("\\RuntimeException $e", &body), "none", "{call}");
    }
    assert_eq!(precision("\\RuntimeException $e", "return sprintf('%s', $e->getPrevious());"), "none");
    assert_eq!(precision("$e", "return sprintf('%s', $e->getMessage());"), "gap");
}
