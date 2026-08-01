//! ADR-0070 — a fact survives a call that takes its variable **by value**.
//!
//! The walk's call-argument invalidation was blanket: every variable handed to
//! any call was forgotten at the statement's end. Sound, and far coarser than
//! PHP needs. Scalars, strings and arrays are value-semantic (copy-on-write), so
//! a callee handed one by value receives a copy and cannot reach the caller's
//! binding at all — `array_first($a)` leaves `$a` exactly as it was.
//!
//! This file is the gate's behavior from both sides. The positive half is the
//! three measured blockers in miniature (issues #76/#77/#74); the negative half
//! is every reason the gate refuses, and it is the larger half on purpose — a
//! kept fact is a new premise for everything downstream, so each refusal is
//! pinned rather than assumed.
//!
//! # Behavioral witnesses at `PINNED_PHP` (8.5.8, `php -r`)
//!
//! The value semantics the whole design rests on, measured rather than recited:
//!
//! ```text
//! function f(string $x) { $x = 'z'; }  $s = 'abc'; f($s); $s === 'abc'  → true
//! function g(string &$x) { $x = 'z'; } $t = 'abc'; g($t); $t === 'z'    → true
//! $a = [1, 2, 3]; array_first($a); count($a) === 3                      → true
//! $b = [1, 2, 3]; array_pop($b);   count($b) === 2                      → true
//! $s = 'aaa'; preg_match('/a/', $s, $m); $s === 'aaa'                   → true
//! class C { public int $p = 1; } $o = new C; h($o); // h may write $o->p
//! ```

use steins_domain::Fact;
use steins_infer::{DEBUG_TYPE_ID, Folder, check, check_with};
use steins_syntax::{ArgValue, SourceTree};

/// The `debug.type` message bodies a source produces, in source order.
fn types(src: &str) -> Vec<String> {
    let tree = SourceTree::parse(src);
    check(&tree, &[], "t.php")
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message)
        .collect()
}

/// The single `debug.type` body a one-dump source produces.
fn one_type(src: &str) -> String {
    let ds = types(src);
    assert_eq!(ds.len(), 1, "expected exactly one debug.type dump, got {ds:?}");
    ds[0].clone()
}

/// A statement body wrapped in a function scope, dumping `$s` at the end. The
/// scope wrapper matters: a top-level script and a function body are different
/// [`steins_syntax::ScopeOwner`]s, and the blockers all live in function bodies.
fn dump_after(body: &str) -> String {
    one_type(&format!("<?php\nfunction f(bool $b): void {{ {body} \\PHPStan\\dumpType($s); }}\n"))
}

// ---------------------------------------------------------------------------
// The positive half: the three measured blockers
// ---------------------------------------------------------------------------

#[test]
fn a_string_fact_survives_a_by_value_builtin() {
    // Issue #77's `lowercase-string-trim.php` shape: the same binding read
    // through `trim`, then again, then again. Under the blanket rule only the
    // first line could see anything.
    assert_eq!(dump_after("$s = 'abc'; trim($s);"), "dumped type: 'abc'");
    assert_eq!(dump_after("$s = 'abc'; trim($s); ltrim($s); rtrim($s);"), "dumped type: 'abc'");
    // `chop` is `rtrim` under a second name and is certified as such.
    assert_eq!(dump_after("$s = 'abc'; chop($s);"), "dumped type: 'abc'");
}

#[test]
fn a_bounded_union_survives_a_by_value_builtin() {
    // Issue #74's `lowercase-string-sprintf.php` shape: `$constant` is a two-value
    // union consumed by one `sprintf` per line, and every line after the first
    // needs it still to be a union.
    assert_eq!(dump_after("$s = $b ? 'A' : 'B'; sprintf('%s', $s);"), "dumped type: 'A'|'B'");
    assert_eq!(
        dump_after("$s = $b ? 'A' : 'B'; sprintf('%s', $s); sprintf('%d', $s);"),
        "dumped type: 'A'|'B'"
    );
}

#[test]
fn an_array_shape_survives_the_non_mutating_read_position_family() {
    // Issue #76's `array_first_last.php:21-23`, at the shape lane: `array_first`
    // does not touch its argument, so `array_last` four lines later still has a
    // shape to read.
    assert_eq!(
        dump_after("$s = [1, 2]; array_first($s); array_last($s);"),
        "dumped type: list{1, 2}"
    );
    // The projection family and the two `array|object $array` pointer *readers*
    // (`current`/`key` — their pointer-*moving* siblings are by-ref, below).
    for f in ["array_values", "array_keys", "array_flip", "array_reverse", "current", "key",
              "array_key_first", "array_key_last", "count", "in_array"] {
        assert_eq!(
            dump_after(&format!("$s = [1, 2]; {f}($s);")),
            "dumped type: list{1, 2}",
            "{f} takes its array by value"
        );
    }
}

/// A mock PHP answering the two reflection surfaces the ADR-0064 Amendment B
/// read-position rung consults — the same shape `array_read_position.rs` uses,
/// so this file's end-to-end blocker fixture runs the real rule.
struct Mock;

impl Folder for Mock {
    fn fold(&mut self, _name: &str, _args: &[ArgValue]) -> Option<ArgValue> {
        None
    }
    fn builtin_return_fact(&mut self, _name: &str) -> Option<Fact> {
        None
    }
    fn builtin_return_type(&mut self, name: &str) -> Option<String> {
        match name.to_ascii_lowercase().as_str() {
            "array_first" | "array_last" => Some("mixed".to_owned()),
            "array_slice" => Some("array".to_owned()),
            _ => None,
        }
    }
    fn builtin_param_counts(&mut self, name: &str) -> Option<(u32, u32)> {
        match name.to_ascii_lowercase().as_str() {
            "array_first" | "array_last" => Some((1, 1)),
            "array_slice" => Some((4, 2)),
            _ => None,
        }
    }
}

#[test]
fn the_issue_76_blocker_reproduced_end_to_end() {
    // `array_first_last.php` lines 15 and 21 verbatim in miniature: the same
    // declared shape read by `array_first` and then by `array_last`. Line 21 is
    // exactly the row the blanket drop cost.
    let src = "<?php\n/** @param array{a: 'bar', b: 'foo'} $v */\n\
               function f(array $v): void {\n\
               \\PHPStan\\dumpType(array_first($v));\n\
               \\PHPStan\\dumpType(array_last($v));\n}\n";
    let tree = SourceTree::parse(src);
    let got: Vec<String> = check_with(&tree, &[], "t.php", &mut Mock)
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message)
        .collect();
    assert_eq!(
        got,
        vec![
            "dumped type: 'bar'|'foo' (asserted)".to_owned(),
            "dumped type: 'bar'|'foo' (asserted)".to_owned()
        ],
        "the second read must see the same shape as the first"
    );
}

#[test]
fn the_array_slice_stack_reproduced_end_to_end() {
    // phpstan-src's `array-slice.php` shape in miniature: several consecutive
    // reads of ONE declared variable, each through `array_slice` — the variable's
    // occurrence is a *nested* call position under the dump, so the dump-read
    // exception never sees it and survival rides entirely on `array_slice`'s
    // certification (the ADR-0062 Amendment B member of the certified set).
    // Before that certification the second row answered the bare `array` floor
    // and the variable itself was gone.
    let src = "<?php\n/** @param array<int, bool> $arr */\n\
               function f(array $arr): void {\n\
               \\PHPStan\\dumpType(array_slice($arr, 1, 2));\n\
               \\PHPStan\\dumpType(array_slice($arr, 1, 2));\n\
               \\PHPStan\\dumpType($arr);\n}\n";
    let tree = SourceTree::parse(src);
    let got: Vec<String> = check_with(&tree, &[], "t.php", &mut Mock)
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message)
        .collect();
    assert_eq!(got.len(), 3, "expected three debug.type dumps, got {got:?}");
    assert_eq!(
        got[0], got[1],
        "the second slice must see the same declared shape as the first"
    );
    assert_eq!(got[0], "dumped type: list<bool> (asserted)");
    assert_eq!(
        got[2], "dumped type: array<int, bool> (asserted)",
        "the declared fact itself must outlive both slices"
    );
}

// ---------------------------------------------------------------------------
// The negative half: every reason the gate refuses
// ---------------------------------------------------------------------------

#[test]
fn a_by_ref_builtin_position_still_invalidates() {
    // The issue #76 pin, kept: six of the ten read-position names take argument 0
    // by reference and move or shorten it. `steins_catalog::out_params` carries
    // all six and the gate reads the row positionally.
    for f in ["array_pop", "array_shift", "next", "prev", "reset", "end", "sort", "shuffle"] {
        assert_eq!(
            dump_after(&format!("$s = [1, 2]; {f}($s);")),
            "dumped type: unknown",
            "{f} writes argument 0 by reference"
        );
    }
    // `settype(mixed &$var, string $type)` — the scalar member of the same row.
    assert_eq!(dump_after("$s = 'abc'; settype($s, 'int');"), "dumped type: unknown");
}

#[test]
fn preg_match_keeps_its_subject_and_drops_its_matches() {
    // The sharpest single pin of the design: ONE call, two opposite verdicts.
    // `preg_match(string $pattern, string $subject, array &$matches = null)` —
    // `$subject` is by value and survives, `$matches` is the out-parameter and
    // cannot.
    let src = "<?php\nfunction f(): void { $s = 'aaa'; $m = [1]; preg_match('/a/', $s, $m);\n\
               \\PHPStan\\dumpType($s); \\PHPStan\\dumpType($m); }\n";
    assert_eq!(
        types(src),
        vec!["dumped type: 'aaa'".to_owned(), "dumped type: unknown".to_owned()]
    );
}

#[test]
fn one_by_ref_occurrence_condemns_the_name_for_the_whole_statement() {
    // `str_replace(…, …, $subject, int &$count = null)`: the same variable at a
    // by-value position AND at the out-parameter position. The by-value reading
    // of position 2 must not launder the write at position 3.
    assert_eq!(dump_after("$s = 'abc'; str_replace('a', 'b', $s, $s);"), "dumped type: unknown");
    // …and the same across two different callees in one statement: one known and
    // by value, one nobody knows.
    assert_eq!(dump_after("$s = 'abc'; $x = trim($s) . my_helper($s);"), "dumped type: unknown");
}

#[test]
fn an_unknown_callee_invalidates() {
    // A name the catalog cannot describe and the index does not hold is not a
    // by-value promise — it is silence, and silence keeps the blanket drop.
    assert_eq!(dump_after("$s = 'abc'; my_helper($s);"), "dumped type: unknown");
    // Including the variadic-by-ref family, which is deliberately absent from the
    // out-parameter table (its reference positions are open-ended).
    for f in ["sscanf", "array_multisort", "parse_str", "exec"] {
        assert_eq!(
            dump_after(&format!("$s = 'abc'; {f}($s);")),
            "dumped type: unknown",
            "{f} is not certified by the catalog"
        );
    }
}

#[test]
fn a_project_function_answers_from_its_declared_parameter() {
    // The index knows the declaration, so the by-ref bit is read, not guessed.
    let by_value = "<?php\nfunction g(string $x): void {}\n\
                    function f(): void { $s = 'abc'; g($s); \\PHPStan\\dumpType($s); }\n";
    assert_eq!(one_type(by_value), "dumped type: 'abc'");

    let by_ref = "<?php\nfunction g(string &$x): void {}\n\
                  function f(): void { $s = 'abc'; g($s); \\PHPStan\\dumpType($s); }\n";
    assert_eq!(one_type(by_ref), "dumped type: unknown");

    // A variadic position refuses too: this slice does not model spread binding.
    let variadic = "<?php\nfunction g(string ...$x): void {}\n\
                    function f(): void { $s = 'abc'; g($s); \\PHPStan\\dumpType($s); }\n";
    assert_eq!(one_type(variadic), "dumped type: unknown");

    // An argument past the declared arity has no declaration to read.
    let over = "<?php\nfunction g(string $x): void {}\n\
                function f(): void { $s = 'abc'; g('a', $s); \\PHPStan\\dumpType($s); }\n";
    assert_eq!(one_type(over), "dumped type: unknown");
}

#[test]
fn a_callee_that_defeats_value_tracking_invalidates() {
    // A by-value parameter is not the only route into the caller's frame: a
    // callee doing `global $s` writes the top-level binding of that name, which
    // argument passing never described. The callee's own poison flag is the veto.
    for body in ["global $z;", "extract(['a' => 1]);", "eval('1;');", "$$x = 1;"] {
        let src = format!(
            "<?php\nfunction g(string $x): void {{ {body} }}\n\
             function f(): void {{ $s = 'abc'; g($s); \\PHPStan\\dumpType($s); }}\n"
        );
        assert_eq!(one_type(&src), "dumped type: unknown", "a callee doing `{body}` vetoes");
    }
}

#[test]
fn an_object_binding_always_invalidates() {
    // Handle semantics: the callee gets a copy of the *handle*, and the object
    // behind it is shared. Its heap facts cannot survive a call, by value or not.
    let src = "<?php\nclass C { public int $p = 1; }\n\
               function f(): void { $o = new C(); $o->p = 5; \\PHPStan\\dumpType($o->p); }\n";
    assert_eq!(one_type(src), "dumped type: 5", "the baseline the next case moves");

    let touched = "<?php\nclass C { public int $p = 1; }\n\
                   function f(): void { $o = new C(); $o->p = 5; in_array($o, [1]);\n\
                   \\PHPStan\\dumpType($o->p); }\n";
    assert_eq!(one_type(touched), "dumped type: unknown");
}

#[test]
fn a_reference_binding_in_the_scope_invalidates_everything() {
    // `$r = &$s` is on the ADR-0001 give-up list and poisons the WHOLE scope, so
    // a live reference into a local can never coexist with a surviving fact.
    // (This is condition 4's scope half, and it is why the gate needs no separate
    // reference-liveness analysis of its own.)
    assert_eq!(dump_after("$s = 'abc'; $r = &$s; trim($s);"), "dumped type: unknown");
    for poison in ["global $g;", "static $t;", "$$k = 1;", "extract([]);", "compact('s');"] {
        assert_eq!(
            dump_after(&format!("$s = 'abc'; trim($s); {poison}")),
            "dumped type: unknown",
            "`{poison}` poisons the scope"
        );
    }
    // A by-ref `use (&$s)` capture poisons both sides of the capture.
    assert_eq!(
        dump_after("$s = 'abc'; $c = function () use (&$s) {}; trim($s);"),
        "dumped type: unknown"
    );
}

#[test]
fn the_v1_exclusions_keep_the_blanket_drop() {
    // A method / static / constructor call: the receiver's own mutability is a
    // separate question and no name resolves the target here.
    let method = "<?php\nclass C { public function m(string $x): void {} }\n\
                  function f(): void { $s = 'abc'; $c = new C(); $c->m($s);\n\
                  \\PHPStan\\dumpType($s); }\n";
    assert_eq!(one_type(method), "dumped type: unknown");

    let static_call = "<?php\nclass C { public static function m(string $x): void {} }\n\
                       function f(): void { $s = 'abc'; C::m($s); \\PHPStan\\dumpType($s); }\n";
    assert_eq!(one_type(static_call), "dumped type: unknown");

    // A dynamic callee resolves to nothing.
    assert_eq!(dump_after("$s = 'abc'; $fn = 'trim'; $fn($s);"), "dumped type: unknown");

    // Named arguments and spread defeat positional mapping, so the whole list is
    // withheld rather than indexed by guess.
    let named = "<?php\nfunction g(string $x, string $y): void {}\n\
                 function f(): void { $s = 'abc'; g(y: 'b', x: $s); \\PHPStan\\dumpType($s); }\n";
    assert_eq!(one_type(named), "dumped type: unknown");

    let spread = "<?php\nfunction g(string ...$x): void {}\n\
                  function f(array $r): void { $s = 'abc'; g($s, ...$r); \\PHPStan\\dumpType($s); }\n";
    assert_eq!(one_type(spread), "dumped type: unknown");
}

// ---------------------------------------------------------------------------
// The lowering's own invariant
// ---------------------------------------------------------------------------

#[test]
fn language_constructs_are_untouched_by_this_path() {
    // `isset`/`empty`/`unset`/`list` are constructs, not calls, so neither the
    // blanket collector nor the site collector ever sees them — and `unset($s)`
    // must keep erasing the binding.
    assert_eq!(dump_after("$s = 'abc'; $q = isset($s);"), "dumped type: 'abc'");
    assert_eq!(dump_after("$s = 'abc'; $q = empty($s);"), "dumped type: 'abc'");
    assert_eq!(dump_after("$s = 'abc'; unset($s);"), "dumped type: unknown");
}

#[test]
fn the_site_list_is_complete_or_absent_per_name() {
    // The syntax layer's invariant, observed through behavior: a name with even
    // one indescribable occurrence in the statement is absent from the precise
    // list entirely, so the blanket drop applies to ALL of its occurrences.
    let mixed = "<?php\nclass C { public function m(string $x): void {} }\n\
                 function f(): void { $s = 'abc'; $c = new C(); trim($s); $c->m($s);\n\
                 \\PHPStan\\dumpType($s); }\n";
    assert_eq!(one_type(mixed), "dumped type: unknown");
    // Two statements, though, are two verdicts — the method call is what drops it.
    let split = "<?php\nclass C { public function m(string $x): void {} }\n\
                 function f(): void { $s = 'abc'; trim($s); \\PHPStan\\dumpType($s); }\n";
    assert_eq!(one_type(split), "dumped type: 'abc'");
}
