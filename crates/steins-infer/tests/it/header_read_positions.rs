//! The reads a branch condition, a loop header or a `switch` subject evaluates (ADR-0104's
//! S4 amendment, issue #1050): one case per row of the table in `header_reads.rs`'s module
//! doc, per family class, plus the silences every row keeps.
//!
//! A finding is named by its id and the source text it points at, so each case also pins
//! the column (D1: the read's own span). Every firing row warns or throws on PHP 8.5 with a
//! handler installed; every silent row is silent there, or is a recorded false negative
//! (named so).

use steins_infer::{Diagnostic, Folder, check_with};
use steins_syntax::SourceTree;

/// A boot surface that has only the guard builtins: every other function and class is
/// absent, so an undefined name is a proof.
struct Boot;

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
        true
    }
    fn boot_surface_class_like(&mut self, _: &str) -> Option<bool> {
        Some(false)
    }
    fn boot_surface_function(&mut self, name: &str) -> Option<bool> {
        Some(["function_exists", "class_exists", "defined", "strlen"].contains(&name))
    }
    fn boot_surface_constant(&mut self, _: &str) -> Option<bool> {
        Some(false)
    }
    fn boot_surface_label(&mut self) -> Option<String> {
        Some("PHP 8.5.8 (32 extensions)".to_owned())
    }
}

const PRELUDE: &str = "<?php
class A { const K = 1; private const P = 2; }
class C { public int $x = 1; private int $priv = 2; public function m(): int { return 1; } }
function two(int $a, int $b): int { return $a + $b; }
function byref(&$a) { $a['nope'] = 1; return ''; }
";

const IDS: &[&str] = &[
    "call.",
    "class-const.",
    "offset.missing",
    "property.",
    "constant.undefined",
    "class.undefined",
];

/// Every finding of `body` as `"<id> @ <source text at its position>"`.
fn found(body: &str) -> Vec<String> {
    let src = format!(
        "{PRELUDE}function t($x) {{ $a = ['k' => 1]; $c = new C(); $z = null; {body} }}\n"
    );
    let tree = SourceTree::parse(&src);
    let lines: Vec<&str> = src.lines().collect();
    let mut out: Vec<Diagnostic> = check_with(&tree, &[], "test.php", &mut Boot);
    out.retain(|d| IDS.iter().any(|p| d.id.starts_with(p)));
    out.sort_by_key(|d| (d.line, d.column));
    out.iter()
        .map(|d| {
            let line = lines[d.line as usize - 1];
            let text: String = line.chars().skip(d.column as usize - 1).take(12).collect();
            format!("{} @ {}", d.id, text)
        })
        .collect()
}

macro_rules! row {
    ($name:ident, $body:expr, [$($expect:expr),* $(,)?]) => {
        #[test]
        fn $name() {
            let expect: Vec<&str> = vec![$($expect),*];
            let got = found($body);
            assert_eq!(got.len(), expect.len(), "{got:#?}");
            for (g, e) in got.iter().zip(&expect) {
                assert!(g.starts_with(e), "got {g:?}, wanted a prefix of {e:?}\n{got:#?}");
            }
        }
    };
}

const UNDEF_FN: &str = "call.undefined-function @ nope_fn()";
const UNDEF_M: &str = "call.undefined-method @ $c->gone()";
const UNDEF_CC: &str = "class-const.undefined @ A::NOPE";
const OFFSET: &str = "offset.missing @ $a['nope']";
const PROP: &str = "property.undefined @ $c->nope";

// ---- `if`: the condition, in the env the `if` is entered with ----

row!(if_call, "if (nope_fn()) { return 1; }", [UNDEF_FN]);
row!(if_method, "if ($c->gone()) { return 1; }", [UNDEF_M]);
row!(if_class_const, "if (A::NOPE) { return 1; }", [UNDEF_CC]);
row!(if_private_class_const, "if (A::P) { return 1; }", ["class-const.inaccessible @ A::P"]);
row!(if_offset, "if ($a['nope']) { return 1; }", [OFFSET]);
row!(if_property, "if ($c->nope) { return 1; }", [PROP]);
row!(if_private_property, "if ($c->priv) { return 1; }", ["property.inaccessible @ $c->priv"]);
row!(if_arity, "if (two(1)) { return 1; }", ["call.too-few-arguments @ two(1)"]);
row!(if_call_on_null, "if ($z->m()) { return 1; }", ["call.on-null @ $z->m()"]);
row!(if_argument, "if (strlen(nope_fn())) { return 1; }", [UNDEF_FN]);
row!(if_assignment_value, "if ($y = nope_fn()) { return $y; }", [UNDEF_FN]);

// Carriers inside a condition, as in a statement.
row!(
    if_isset_then_read_is_conditional,
    "if (isset($a['nope']) && $a['nope'] > 0) { return 1; }",
    []
);
// Recorded false negative: the right of `||` is `Conditional`, which a value family declines.
row!(if_right_of_or_declines_a_value_read, "if ($a['k'] > 5 || $a['nope']) { return 1; }", []);
row!(if_right_of_or_judges_existence, "if ($x || nope_fn()) { return 1; }", [UNDEF_FN]);
row!(if_suppressed_offset, "if (@$a['nope']) { return 1; }", []);
row!(if_guarded_call, "if (function_exists('nope_fn') && nope_fn()) { return 1; }", []);
row!(if_guarded_ternary, "if (function_exists('nope_fn') ? nope_fn() : 0) { return 1; }", []);
row!(
    if_stored_guard,
    "$has = function_exists('nope_fn'); if ($has && nope_fn()) { return 1; }",
    []
);
row!(if_short_circuited_call, "if (true || nope_fn()) { return 1; }", []);
// PHP: `byref` creates the key before the concatenation reads it.
row!(if_by_ref_before_the_read, "if (byref($a) . $a['nope']) { return 1; }", []);

// ---- `elseif`: in the env the earlier conditions' false sides leave ----

row!(elseif_call, "if ($x) { return 1; } elseif (nope_fn()) { return 2; }", [UNDEF_FN]);
row!(elseif_offset, "if ($x) { return 1; } elseif ($a['nope']) { return 2; }", [OFFSET]);
row!(
    elseif_after_a_decided_false_one,
    "if ($x) { return 1; } elseif (false) { return 2; } elseif (nope_fn()) { return 3; }",
    [UNDEF_FN]
);
row!(
    elseif_after_a_decided_true_call,
    "if (true) { return 1; } elseif (nope_fn()) { return 2; }",
    []
);
row!(
    elseif_after_a_decided_true_const,
    "if (true) { return 1; } elseif (NOPE_C) { return 2; }",
    []
);
row!(
    elseif_after_a_decided_true_class_const,
    "if (true) { return 1; } elseif (A::NOPE) { return 2; }",
    []
);
row!(
    elseif_after_a_decided_true_new,
    "if (true) { return 1; } elseif (new Nope()) { return 2; }",
    []
);
row!(elseif_reads_the_narrowed_env, "if ($x) { $a = []; } elseif ($a['k']) { return 2; }", []);

// ---- `switch (true)` labels and `match (true)` arms are an `elseif` chain ----

row!(
    switch_true_label,
    "switch (true) { case $x > 1: return 1; case nope_fn(): return 2; }",
    [UNDEF_FN]
);
row!(
    switch_true_label_after_a_decided_one,
    "switch (true) { case true: return 1; case nope_fn(): return 2; }",
    []
);
row!(match_true_arm, "match (true) { $x > 1 => 1, nope_fn() => 2, default => 3 };", [UNDEF_FN]);
row!(
    match_true_arm_offset,
    "match (true) { $x > 1 => 1, $a['nope'] === 1 => 2, default => 3 };",
    [OFFSET]
);
row!(match_true_arm_after_a_decided_one, "match (true) { true => 1, NOPE_C => 2 };", []);
// A hoisted `match (true)` is walked ahead of its statement: `Conditional`.
row!(
    hoisted_match_true_arm_call,
    "return match (true) { $x > 1 => 1, nope_fn() => 2, default => 3 };",
    [UNDEF_FN]
);
// Recorded false negative: a value family declines there.
row!(
    hoisted_match_true_arm_offset,
    "return match (true) { $x > 1 => 1, $a['nope'] === 1 => 2, default => 3 };",
    []
);

// ---- `while`: the loop-head env, which holds at the first evaluation ----

row!(while_call, "while (nope_fn()) { break; }", [UNDEF_FN]);
row!(while_offset, "while ($a['nope']) { break; }", [OFFSET]);
row!(while_offset_body_returns, "while ($a['nope']) { return; }", [OFFSET]);
row!(
    while_class_const_after_a_dead_loop,
    "while (false) { } while (A::NOPE) { break; }",
    [UNDEF_CC]
);
row!(while_coalesce, "while ($a['nope'] ?? false) { break; }", []);
// Recorded false negative: the body rewrites `$a`, which the loop-head env forgets.
row!(while_subject_written_in_the_body, "while ($a['k']) { $a = []; }", []);
row!(
    while_stored_guard_ternary,
    "$has = function_exists('nope_fn'); while ($has ? nope_fn() : false) { break; }",
    []
);

// ---- `for`: conditions after `init`; the step after an iteration completes ----

row!(for_condition_call, "for ($i = 0; nope_fn(); $i++) { }", [UNDEF_FN]);
row!(for_condition_offset, "for ($i = 0; $a['nope']; $i++) { }", [OFFSET]);
row!(
    for_condition_reads_init,
    "for ($b = ['k' => 1]; $b['nope']; ) { break; }",
    ["offset.missing @ $b['nope']"]
);
row!(for_step_call, "for ($i = 0; $i < 1; nope_fn()) { }", [UNDEF_FN]);
row!(for_step_never_reached, "for ($i = 0; $i < 1; nope_fn()) { return; }", []);
// Recorded false negative: the step is `Conditional`.
row!(for_step_offset, "for ($i = 0; $i < 1; $a['nope']) { $i++; }", []);

// ---- `do`-`while`: after an iteration completes ----

row!(do_while_call, "$i = 0; do { $i++; } while (nope_fn());", [UNDEF_FN]);
row!(do_while_never_reached, "do { return; } while (nope_fn());", []);
row!(
    do_while_reached_by_continue,
    "do { if ($x) { continue; } return; } while (nope_fn());",
    [UNDEF_FN]
);
// Recorded false negative: the condition is `Conditional`.
row!(do_while_offset, "do { } while ($a['nope']);", []);

// ---- `foreach`: the subject, once, at the construct ----

row!(foreach_call, "foreach (nope_fn() as $v) { }", [UNDEF_FN]);
row!(foreach_offset, "foreach ($a['nope'] as $v) { }", [OFFSET]);
row!(foreach_property, "foreach ($c->nope as $v) { }", [PROP]);
row!(foreach_class_const, "foreach (A::NOPE as $v) { }", [UNDEF_CC]);

// ---- `match` and `switch` by value ----

row!(match_subject_class_const, "return match (A::NOPE) { 1 => 'a', default => 'b' };", [UNDEF_CC]);
row!(match_subject_statement, "match (A::NOPE) { 1 => 'a', default => 'b' };", [UNDEF_CC]);
row!(match_subject_offset, "return match ($a['nope']) { null => 1, default => 2 };", [OFFSET]);
row!(match_subject_unstructured, "match ($c->gone()) { default => 1 };", [UNDEF_M]);
row!(match_arm_class_const, "return match ($x) { A::NOPE => 1, default => 2 };", [UNDEF_CC]);
row!(
    match_arm_after_a_decided_one,
    "$y = 1; return match ($y) { 1 => 'a', A::NOPE => 'b', default => 'c' };",
    []
);
row!(switch_subject_class_const, "switch (A::NOPE) { case 1: return 1; }", [UNDEF_CC]);
row!(switch_subject_offset, "switch ($a['nope']) { case 1: return 1; }", [OFFSET]);
row!(switch_label_class_const, "switch ($x) { case A::NOPE: return 1; }", [UNDEF_CC]);
row!(
    switch_label_after_a_decided_one,
    "$y = 1; switch ($y) { case 1: return 1; case A::NOPE: return 2; }",
    []
);

// ---- A statement's own reads are judged once ----

row!(echo_call_once, "echo nope_fn();", [UNDEF_FN]);
row!(if_condition_once, "if (nope_fn() && nope_fn()) { return 1; }", [UNDEF_FN, UNDEF_FN]);

// ---- Alternatives: PHP stops at the first condition of an arm list or a label stack ----
// Witnessed on PHP 8.5: each silent row runs without a warning or an error.

row!(
    match_arm_list_after_a_decided_guard,
    "match (true) { !defined('A::NOPE'), A::NOPE > 1 => 1, default => 2 };",
    []
);
row!(
    hoisted_match_arm_list_after_a_decided_guard,
    "return match (true) { !defined('A::NOPE'), A::NOPE > 1 => 1, default => 2 };",
    []
);
row!(
    assigned_match_arm_list_after_a_decided_guard,
    "$y = match (true) { !defined('A::NOPE'), A::NOPE > 1 => 1, default => 2 }; return $y;",
    []
);
row!(
    match_arm_list_after_a_decided_class_guard,
    "match (true) { !class_exists('Nope'), (new Nope())->x() => 1, default => 2 };",
    []
);
row!(
    match_arm_list_after_an_isset,
    "$b = []; match (true) { !isset($b['k']), $b['k'] > 5 => 1, default => 2 };",
    []
);
row!(
    switch_label_stack_after_an_empty,
    "$b = []; switch (true) { case empty($b): case $b[0] > 5: return 1; }",
    []
);
row!(
    match_arm_list_later_condition_judges_existence,
    "match (true) { $x > 1, nope_fn() => 1, default => 2 };",
    [UNDEF_FN]
);
row!(
    switch_label_stack_later_label_judges_existence,
    "switch (true) { case $x > 1: case nope_fn(): return 1; }",
    [UNDEF_FN]
);
// A `for` header's comma conditions all run: PHP throws on `A::NOPE`.
row!(for_comma_conditions_all_run, "for (; $x, A::NOPE; ) { break; }", [UNDEF_CC]);

/// `body` in a function whose `$s` is declared `array{k?: int}`, at the strict leg.
fn found_shape(body: &str) -> Vec<String> {
    let src = format!("{PRELUDE}/** @param array{{k?: int}} $s */\nfunction t(array $s) {{ {body} }}\n");
    let tree = SourceTree::parse(&src);
    let out: Vec<Diagnostic> = check_with(&tree, &[], "test.php", &mut Boot);
    out.iter().filter(|d| d.id.starts_with("offset.")).map(|d| d.id.to_owned()).collect()
}

#[test]
fn an_optional_key_after_an_isset_alternative_is_not_read() {
    for body in [
        "echo match (true) { !isset($s['k']), $s['k'] > 5 => 'a', default => 'b' };",
        "switch (true) { case !isset($s['k']): case $s['k'] > 5: echo 'a'; }",
        "match (true) { !isset($s['k']), $s['k'] > 5 => print('a'), default => print('b') };",
    ] {
        assert_eq!(found_shape(body), Vec::<String>::new(), "{body}");
    }
}
