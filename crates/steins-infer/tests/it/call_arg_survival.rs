//! ADR-0070 — a fact survives a call that takes its variable **by value**.
//!
//! The walk's invalidation was blanket: every variable handed to a call was
//! forgotten at the statement's end — sound, but coarser than PHP needs.
//! Scalars, strings and arrays are value-semantic (copy-on-write): a callee
//! handed one by value gets a copy and can't reach the caller's binding —
//! `array_first($a)` leaves `$a` unchanged.
//!
//! This file pins the gate's behavior from both sides: the positive half is
//! the three measured blockers in miniature (issues #76/#77/#74); the negative
//! half — deliberately the larger one — is every reason the gate refuses,
//! since a kept fact is a premise for everything downstream.
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

/// A statement body wrapped in a function scope, dumping `$s` at the end.
/// Scope matters: a script and a function body are different
/// [`steins_syntax::ScopeOwner`]s, and the blockers all live in function bodies.
fn dump_after(body: &str) -> String {
    one_type(&format!("<?php\nfunction f(bool $b): void {{ {body} \\PHPStan\\dumpType($s); }}\n"))
}

// The positive half: the three measured blockers

#[test]
fn a_string_fact_survives_a_by_value_builtin() {
    // Issue #77's `lowercase-string-trim.php` shape: the same binding read via
    // `trim` repeatedly; under the blanket rule only the first read survived.
    assert_eq!(dump_after("$s = 'abc'; trim($s);"), "dumped type: 'abc'");
    assert_eq!(dump_after("$s = 'abc'; trim($s); ltrim($s); rtrim($s);"), "dumped type: 'abc'");
    // `chop` is `rtrim` under a second name and is certified as such.
    assert_eq!(dump_after("$s = 'abc'; chop($s);"), "dumped type: 'abc'");
}

#[test]
fn a_bounded_union_survives_a_by_value_builtin() {
    // Issue #74's `lowercase-string-sprintf.php` shape: a two-value union
    // consumed by `sprintf` per line must still be a union past the first.
    assert_eq!(dump_after("$s = $b ? 'A' : 'B'; sprintf('%s', $s);"), "dumped type: 'A'|'B'");
    assert_eq!(
        dump_after("$s = $b ? 'A' : 'B'; sprintf('%s', $s); sprintf('%d', $s);"),
        "dumped type: 'A'|'B'"
    );
}

#[test]
fn an_array_shape_survives_the_non_mutating_read_position_family() {
    // Issue #76's `array_first_last.php:21-23`: `array_first` doesn't touch
    // its argument, so `array_last` four lines later still has a shape to read.
    assert_eq!(
        dump_after("$s = [1, 2]; array_first($s); array_last($s);"),
        "dumped type: list{1, 2}"
    );
    // Projection family plus the two pointer *readers* (`current`/`key`) —
    // their pointer-*moving* siblings are by-ref, below.
    for f in ["array_values", "array_keys", "array_flip", "array_reverse", "current", "key",
              "array_key_first", "array_key_last", "count", "in_array"] {
        assert_eq!(
            dump_after(&format!("$s = [1, 2]; {f}($s);")),
            "dumped type: list{1, 2}",
            "{f} takes its array by value"
        );
    }
}

/// A mock PHP answering the two reflection surfaces ADR-0064 Amendment B's
/// read-position rung consults (same shape as `array_read_position.rs`), so
/// this fixture runs the real rule.
struct Mock;

impl Folder for Mock {
    fn fold(&mut self, _name: &str, _args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
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
    // `array_first_last.php` lines 15 & 21 in miniature: declared shape read
    // by `array_first` then `array_last` — line 21 is what the blanket drop cost.
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
    // phpstan-src's `array-slice.php` shape: consecutive reads of ONE declared
    // var via `array_slice`, nested under the dump so the dump-read exception
    // never sees it — survival rides entirely on `array_slice`'s ADR-0062
    // Amendment B certification (before it: bare `array` floor, variable gone).
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

#[test]
fn an_inline_var_cast_outlives_the_slice_like_a_param_would() {
    // Same stack, seeded by ADR-0073's inline `@var` cast instead of `@param`
    // (phpstan-src's `array-slice.php` verbatim) — same contract lane, so the
    // gate must not distinguish the two seedings.
    let src = "<?php\nfunction f(array $arr): void {\n\
               /** @var array<int, bool> $arr */\n\
               \\PHPStan\\dumpType(array_slice($arr, 1, 2));\n\
               \\PHPStan\\dumpType(array_slice($arr, 1, 2));\n}\n";
    let tree = SourceTree::parse(src);
    let got: Vec<String> = check_with(&tree, &[], "t.php", &mut Mock)
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message)
        .collect();
    assert_eq!(
        got,
        vec![
            "dumped type: list<bool> (asserted)".to_owned(),
            "dumped type: list<bool> (asserted)".to_owned()
        ],
        "the cast-seeded fact must survive the first slice"
    );
}

// The negative half: every reason the gate refuses

#[test]
fn a_by_ref_builtin_position_still_invalidates() {
    // Issue #76 pin, kept: 6 of the 10 read-position names take argument 0 by
    // reference and move/shorten it; `steins_catalog::out_params` carries all
    // six, read positionally.
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
    // Sharpest single pin of the design: ONE call, two opposite verdicts.
    // `preg_match(string $pattern, string $subject, array &$matches = null)` —
    // `$subject` is by value and survives; `$matches` is the out-param and can't.
    let src = "<?php\nfunction f(): void { $s = 'aaa'; $m = [1]; preg_match('/a/', $s, $m);\n\
               \\PHPStan\\dumpType($s); \\PHPStan\\dumpType($m); }\n";
    assert_eq!(
        types(src),
        vec!["dumped type: 'aaa'".to_owned(), "dumped type: unknown".to_owned()]
    );
}

#[test]
fn one_by_ref_occurrence_condemns_the_name_for_the_whole_statement() {
    // `str_replace(…, …, $subject, int &$count = null)`: same variable at a
    // by-value position AND the out-param position — position 2's read must
    // not launder position 3's write.
    assert_eq!(dump_after("$s = 'abc'; str_replace('a', 'b', $s, $s);"), "dumped type: unknown");
    // Same across two callees in one statement: one known by value, one unknown.
    assert_eq!(dump_after("$s = 'abc'; $x = trim($s) . my_helper($s);"), "dumped type: unknown");
}

#[test]
fn an_unknown_callee_invalidates() {
    // A name the catalog can't describe and the index doesn't hold is silence,
    // not a by-value promise — the blanket drop stays.
    assert_eq!(dump_after("$s = 'abc'; my_helper($s);"), "dumped type: unknown");
    // Includes the variadic-by-ref family, deliberately absent from the
    // out-parameter table (its reference positions are open-ended).
    for f in ["sscanf", "array_multisort", "parse_str", "exec"] {
        assert_eq!(
            dump_after(&format!("$s = 'abc'; {f}($s);")),
            "dumped type: unknown",
            "{f} is not certified by the catalog"
        );
    }
}

// The mined arginfo arm (issue #637): past the hand-transcribed lists, a
// rowless internal name is certified from the generated `param_facts` table —
// `ReflectionFunction::isPassedByReference()` at `PINNED_PHP`, one row per
// internal function. What that buys is measured in the nsrt oracle; what it
// must not cost is pinned here.

/// The issue's own witness, `non-empty-string-file-functions.php:172-175`.
/// `rmdir(string $directory, $context = null): bool` writes nothing through
/// either parameter, so the guard reads `$s` and both arms — and the join
/// after them — still hold the parameter's declared type. The join is the
/// half this issue buys: it asks for `string`, which was there before the call
/// and which nothing narrowed.
#[test]
fn a_mined_by_value_builtin_keeps_its_argument_on_both_arms_and_at_the_join() {
    let src = "<?php\nfunction f(string $s): void {\n\
               if (rmdir($s)) { \\PHPStan\\dumpType($s); } else { \\PHPStan\\dumpType($s); }\n\
               \\PHPStan\\dumpType($s); }\n";
    assert_eq!(
        types(src),
        vec![
            "dumped type: string".to_owned(),
            "dumped type: string".to_owned(),
            "dumped type: string".to_owned(),
        ]
    );
    // The string-search family, the issue's largest slice: `strstr` reads a
    // haystack and a needle and writes neither, so a second read of either
    // still has something to read.
    assert_eq!(dump_after("$s = 'abc'; strstr($s, 'b'); strstr($s, 'c');"), "dumped type: 'abc'");
    let two = "<?php\nfunction f(): void { $h = 'abc'; $n = 'b'; strstr($h, $n);\n\
               \\PHPStan\\dumpType($h); \\PHPStan\\dumpType($n); }\n";
    assert_eq!(types(two), vec!["dumped type: 'abc'".to_owned(), "dumped type: 'b'".to_owned()]);
}

/// The guard-position exemption's own refusals. The read set a condition
/// forgets is the whole condition's, so the exemption may only fire when the
/// certified callee is the ONLY thing in the call that could have written.
#[test]
fn the_guard_position_exemption_refuses_what_it_cannot_read() {
    // A nested call inside the argument is what actually touched `$s`, and
    // nothing here knows its parameter modes — `rmdir`'s promise is about
    // `rmdir`'s parameter.
    let nested = "<?php\nfunction f(string $s): void { if (rmdir(my_helper($s))) {\n\
                  \\PHPStan\\dumpType($s); } }\n";
    assert_eq!(one_type(nested), "dumped type: unknown");
    // A by-reference position condemns the whole call, not just its own
    // argument: `similar_text(string $string1, string $string2, float
    // &$percent)` is certified at 0 and 1 and refused at 2, so the guard keeps
    // the blanket drop and even `$s` — a by-value position — is forgotten.
    let by_ref = "<?php\nfunction f(string $s): void { $p = 0.0;\n\
                  if (similar_text($s, 'x', $p)) { \\PHPStan\\dumpType($s); } }\n";
    assert_eq!(one_type(by_ref), "dumped type: unknown");
    // A project declaration beside the call makes the spelling ambiguous
    // (issue #279), and an ambiguous name is exempted from nothing.
    let shadowed = "<?php\nfunction rmdir($d) { return true; }\n\
                    function f(string $s): void { if (rmdir($s)) { \\PHPStan\\dumpType($s); } }\n";
    assert_eq!(one_type(shadowed), "dumped type: unknown");
    // A method call names no catalog row at all.
    let method = "<?php\nclass C { public function m(string $x): bool { return true; } }\n\
                  function f(string $s): void { $c = new C();\n\
                  if ($c->m($s)) { \\PHPStan\\dumpType($s); } }\n";
    assert_eq!(one_type(method), "dumped type: unknown");
}

/// The other direction, and the reason the arm reads `by_ref` rather than
/// assuming: a reference parameter must still condemn the binding. A rowed
/// name answers positionally (its row is the complete list); an **unrowed**
/// by-ref name is declined wholesale, because certifying its other positions
/// would mean inventing the row this arm deliberately does not invent.
#[test]
fn a_by_reference_position_still_invalidates_under_the_mined_arm() {
    // Rowed, positional: `str_replace`'s `$subject` survives and its `$count`
    // does not — one call, two verdicts, unchanged by the widening.
    let mixed = "<?php\nfunction f(): void { $s = 'abc'; $c = 1; str_replace('a', 'b', $s, $c);\n\
                 \\PHPStan\\dumpType($s); \\PHPStan\\dumpType($c); }\n";
    assert_eq!(
        types(mixed),
        vec!["dumped type: 'abc'".to_owned(), "dumped type: unknown".to_owned()]
    );
    // Unrowed by-ref: every position keeps the forgetting floor, including the
    // by-value ones. `exec(string $command, array &$output, int &$result_code)`
    // and `similar_text`'s unrowed cousins are exactly this shape.
    for f in ["exec", "parse_str", "settype", "sscanf"] {
        assert_eq!(
            dump_after(&format!("$s = 'abc'; {f}($s, $q);")),
            "dumped type: unknown",
            "{f} declares a reference parameter"
        );
    }
}

/// Issue #279's rule, over a name the catalog only learned in #637: a project
/// declaration beside the call makes the spelling **ambiguous**, and an
/// ambiguous name certifies nothing. Widening the catalog widens the set of
/// spellings this applies to, which is the direction that costs silence — never
/// a fact kept on a body the analysis has not read.
#[test]
fn a_project_declaration_shadowing_a_mined_name_certifies_nothing() {
    let shadowed = "<?php\nfunction strpos($h, $n) { return 0; }\n\
                    function f(): void { $s = 'abc'; strpos($s, 'b'); \\PHPStan\\dumpType($s); }\n";
    assert_eq!(one_type(shadowed), "dumped type: unknown");
    // Without the shadow the same call keeps the fact — so the pin above is
    // measuring the shadow and not the name.
    assert_eq!(dump_after("$s = 'abc'; strpos($s, 'b');"), "dumped type: 'abc'");
}

/// The mined arm changes nothing about an argument written by an **embedded
/// assignment** in the same statement (`f($s = 'x')`). That shape is not a
/// recorded site — the lowering can only describe a bare variable or an offset
/// chain — so the assignment channel is what owns it, and the pin is that a
/// certified callee and an unknown one answer identically.
///
/// They answer identically at the WRONG value: a nested assignment expression
/// is not applied by the walk, so `$s` reads back as its pre-statement value
/// under either callee. That gap predates this issue and is orthogonal to it
/// (`$q = ($s = 'zz');` with no call at all does the same); what matters here
/// is that certification does not launder it — a by-value promise about the
/// PARAMETER never becomes a promise about the caller's statement.
#[test]
fn an_embedded_assignment_reads_the_same_under_a_certified_callee() {
    let certified = dump_after("$s = 'abc'; strstr($s = 'zz', 'z');");
    let unknown = dump_after("$s = 'abc'; my_helper($s = 'zz');");
    let no_call = dump_after("$s = 'abc'; $q = ($s = 'zz');");
    assert_eq!(certified, unknown, "certification did not move this shape");
    assert_eq!(certified, no_call, "…and the shape does not need a call at all");
    // The offset spelling of the by-value rule itself is unaffected (issue
    // #609/#641): a certified position copies the element out, so the root
    // survives with its whole shape.
    assert_eq!(dump_after("$s = ['abc']; strstr($s[0], 'b');"), "dumped type: list{'abc'}");
}

/// Issue #41 — `use function trim;` names the **global builtin**, and the gate
/// must read the catalog through the import exactly as it does through `\trim`.
///
/// This was the string family's second measured blocker: an imported call
/// resolved to *nothing*, condemning every argument's fact — a file that
/// imports its string functions (phpstan-src's nsrt fixtures do) lost every
/// refinement at the first such call.
#[test]
fn an_imported_builtin_resolves_through_the_import() {
    let src = |uses: &str, call: &str| {
        format!(
            "<?php\nnamespace App;\n{uses}\n\
             function f(): void {{ $s = 'abc'; {call}; \\PHPStan\\dumpType($s); }}\n"
        )
    };
    // The plain and leading-backslash import forms both name `trim`/`strtolower`.
    assert_eq!(one_type(&src("use function trim;", "trim($s)")), "dumped type: 'abc'");
    assert_eq!(
        one_type(&src("use function \\strtolower;", "strtolower($s)")),
        "dumped type: 'abc'"
    );
    // Aliased form (issue #279): `FnResolution::Builtin` carries the resolved
    // catalog name (`trim`) beside the import target, so catalog-keyed
    // consumers read that, not the call's own spelling (`t`).
    assert_eq!(one_type(&src("use function trim as t;", "t($s)")), "dumped type: 'abc'");
    // A namespaced import isn't the builtin: no project function defines it,
    // so the call stays unresolved and condemns.
    assert_eq!(one_type(&src("use function Other\\trim;", "trim($s)")), "dumped type: unknown");
    // Same through an ALIAS of a namespaced import: target is neither project
    // function nor global builtin, so aliasing gains nothing.
    assert_eq!(
        one_type(&src("use function Other\\trim as t;", "t($s)")),
        "dumped type: unknown"
    );
    // An import of an uncertified builtin is still not a promise.
    assert_eq!(one_type(&src("use function sscanf;", "sscanf($s)")), "dumped type: unknown");
    // An import a project function DOES define answers from that declaration,
    // not the same-named builtin.
    let shadowed = "<?php\nnamespace Other;\nfunction trim(string &$x): void {}\n\
                    namespace App;\nuse function Other\\trim;\n\
                    function f(): void { $s = 'abc'; trim($s); \\PHPStan\\dumpType($s); }\n";
    assert_eq!(one_type(shadowed), "dumped type: unknown");
    // Same shadowing through an ALIAS: resolving to a project fn (not a
    // builtin) keeps today's behavior — the by-ref param condemns, same as
    // unaliased; aliasing can't manufacture a builtin promise the target never had.
    let shadowed_aliased = "<?php\nnamespace Other;\nfunction trim(string &$x): void {}\n\
                    namespace App;\nuse function Other\\trim as t;\n\
                    function f(): void { $s = 'abc'; t($s); \\PHPStan\\dumpType($s); }\n";
    assert_eq!(one_type(shadowed_aliased), "dumped type: unknown");
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

    // A variadic position refuses too: spread binding is not modeled.
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
    // A by-value param isn't the only route into the caller's frame: a callee
    // doing `global $s` writes the top-level binding — the callee's own
    // poison flag is the veto.
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
    // Handle semantics: the callee copies the *handle*, shares the object —
    // its heap facts never survive a call, by value or not.
    let src = "<?php\nclass C { public int $p = 1; }\n\
               function f(): void { $o = new C(); $o->p = 5; \\PHPStan\\dumpType($o->p); }\n";
    assert_eq!(one_type(src), "dumped type: 5", "the baseline the next case moves");

    // The written fact `5` does not survive the call — that is this test's claim.
    // What is left is the property's DECLARED type (issue #620): the sweep cleared
    // the heap, and the floor below it is what the declaration says the slot may
    // ever hold, which no call can invalidate.
    let touched = "<?php\nclass C { public int $p = 1; }\n\
                   function f(): void { $o = new C(); $o->p = 5; in_array($o, [1]);\n\
                   \\PHPStan\\dumpType($o->p); }\n";
    assert_eq!(one_type(touched), "dumped type: int");

    // The same call over an UNDECLARED property, where there is no floor to fall
    // to — the sweep's own answer, unchanged.
    let undeclared = "<?php\nclass C { public $p = 1; }\n\
                      function f(): void { $o = new C(); $o->p = 5; in_array($o, [1]);\n\
                      \\PHPStan\\dumpType($o->p); }\n";
    assert_eq!(one_type(undeclared), "dumped type: unknown");
}

#[test]
fn a_reference_binding_in_the_scope_invalidates_everything() {
    // `$r = &$s` is on the ADR-0001 give-up list and poisons the WHOLE scope
    // (condition 4's scope half) — a live reference can never coexist with a
    // surviving fact, so no separate reference-liveness analysis is needed.
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
    // Method/static/constructor call: receiver mutability is a separate
    // question and no name resolves the target here.
    let method = "<?php\nclass C { public function m(string $x): void {} }\n\
                  function f(): void { $s = 'abc'; $c = new C(); $c->m($s);\n\
                  \\PHPStan\\dumpType($s); }\n";
    assert_eq!(one_type(method), "dumped type: unknown");

    let static_call = "<?php\nclass C { public static function m(string $x): void {} }\n\
                       function f(): void { $s = 'abc'; C::m($s); \\PHPStan\\dumpType($s); }\n";
    assert_eq!(one_type(static_call), "dumped type: unknown");

    // A dynamic callee resolves to nothing.
    assert_eq!(dump_after("$s = 'abc'; $fn = 'trim'; $fn($s);"), "dumped type: unknown");

    // Named arguments and spread defeat positional mapping, so the whole list
    // is withheld rather than indexed by guess.
    let named = "<?php\nfunction g(string $x, string $y): void {}\n\
                 function f(): void { $s = 'abc'; g(y: 'b', x: $s); \\PHPStan\\dumpType($s); }\n";
    assert_eq!(one_type(named), "dumped type: unknown");

    let spread = "<?php\nfunction g(string ...$x): void {}\n\
                  function f(array $r): void { $s = 'abc'; g($s, ...$r); \\PHPStan\\dumpType($s); }\n";
    assert_eq!(one_type(spread), "dumped type: unknown");
}

#[test]
fn a_closure_body_occurrence_keeps_the_blanket_drop() {
    // A bare `$s` inside a closure/arrow body is a DIFFERENT scope's variable;
    // attributing it to the enclosing `$s` would be manufactured evidence, so
    // the entry is opaque (issue #135) even though `trim` is certified by value.
    assert_eq!(
        dump_after("$s = 'abc'; $c = function () use ($s) { trim($s); };"),
        "dumped type: unknown"
    );
    assert_eq!(dump_after("$s = 'abc'; $c = fn() => trim($s);"), "dumped type: unknown");
}

// The lowering's own invariant

#[test]
fn language_constructs_are_untouched_by_this_path() {
    // `isset`/`empty`/`unset`/`list` are constructs, not calls — neither
    // collector ever sees them, and `unset($s)` must still erase the binding.
    assert_eq!(dump_after("$s = 'abc'; $q = isset($s);"), "dumped type: 'abc'");
    assert_eq!(dump_after("$s = 'abc'; $q = empty($s);"), "dumped type: 'abc'");
    assert_eq!(dump_after("$s = 'abc'; unset($s);"), "dumped type: unknown");
}

// The offset-argument spelling (issue #609): `f($a[0])` hands the ELEMENT to
// the callee, so a by-ref parameter writes through the reference into `$a` —
// the enclosing array's binding is what the statement may change. The chain's
// root takes the same site/opaque rules a bare `$v` argument gets, and
// `by_value_survivors` does the rest unchanged.

#[test]
fn a_by_ref_offset_argument_invalidates_the_enclosing_array() {
    // The issue's own repro: `sort($s[0])` sorts the element in place; the
    // pre-call shape is a false fact afterwards.
    assert_eq!(dump_after("$s = ['x']; sort($s[0]);"), "dumped type: unknown");
    // `settype`'s statement-position cast seed (issue #595) refuses offset
    // targets, so nothing rebinds over the drop either.
    assert_eq!(dump_after("$s = ['y']; settype($s[0], 'int');"), "dumped type: unknown");
    // The entry records the chain's ROOT, whatever the depth.
    assert_eq!(dump_after("$s = [['x']]; sort($s[0][0]);"), "dumped type: unknown");
    // An out-param position spelled as a slot: `$s[0]` is `preg_match`'s
    // `&$matches`, so the enclosing array drops while the subject survives.
    assert_eq!(
        dump_after("$s = [1]; preg_match('/a/', 'aaa', $s[0]);"),
        "dumped type: unknown"
    );
    // A project declaration's by-ref bit is read exactly as for a bare `$v`.
    let by_ref = "<?php\nfunction g(string &$x): void {}\n\
                  function f(): void { $s = ['abc']; g($s[0]); \\PHPStan\\dumpType($s); }\n";
    assert_eq!(one_type(by_ref), "dumped type: unknown");
}

#[test]
fn a_by_value_offset_argument_keeps_the_enclosing_array() {
    // The callee copies the element out; the array is never reachable from it,
    // so the ADR-0070 survival applies to the root with no further evidence.
    assert_eq!(dump_after("$s = ['ab']; strlen($s[0]);"), "dumped type: list{'ab'}");
    assert_eq!(dump_after("$s = ['ab']; trim($s[0]);"), "dumped type: list{'ab'}");
    let by_value = "<?php\nfunction g(string $x): void {}\n\
                    function f(): void { $s = ['abc']; g($s[0]); \\PHPStan\\dumpType($s); }\n";
    assert_eq!(one_type(by_value), "dumped type: list{'abc'}");
}

#[test]
fn an_unprovable_offset_occurrence_drops_the_enclosing_array() {
    // Unknown callee: silence is not a by-value promise — same as a bare `$s`.
    assert_eq!(dump_after("$s = ['ab']; my_helper($s[0]);"), "dumped type: unknown");
    // Method call: no site may vouch, the entry is opaque, the blanket drop stays.
    let method = "<?php\nclass C { public function m(string $x): void {} }\n\
                  function f(): void { $s = ['ab']; $c = new C(); $c->m($s[0]);\n\
                  \\PHPStan\\dumpType($s); }\n";
    assert_eq!(one_type(method), "dumped type: unknown");
}

#[test]
fn the_offset_spelling_shares_the_bare_variable_exceptions() {
    // The key expression is a read, not an argument: `$i` survives untouched
    // while the root drops.
    let key = "<?php\nfunction f(): void { $i = 0; $s = ['x']; sort($s[$i]);\n\
               \\PHPStan\\dumpType($i); \\PHPStan\\dumpType($s); }\n";
    assert_eq!(
        types(key),
        vec!["dumped type: 0".to_owned(), "dumped type: unknown".to_owned()]
    );
    // A dump-family read binds nothing, offset spelling included — `var_dump`'s
    // variadic position could never promise by-value, but a read needs no promise.
    assert_eq!(dump_after("$s = ['x']; var_dump($s[0]);"), "dumped type: list{'x'}");
    // A chain that leaves pure offsets on its way to the variable roots in a
    // different carrier (the heap's), and records nothing here: the OBJECT
    // handle in the element is shared either way, so its fact was never the
    // array binding's to keep. The dump's element spelling is not the point —
    // the pin is that it reads the same PAST the unknown callee.
    let through_prop = "<?php\nclass C { public array $p = ['x']; }\n\
                        function f(): void { $s = [new C()]; my_helper($s[0]->p[0]);\n\
                        \\PHPStan\\dumpType($s); }\n";
    assert_eq!(one_type(through_prop), "dumped type: list{mixed}");
}

#[test]
fn the_site_list_is_complete_or_absent_per_name() {
    // Syntax layer's invariant, observed through behavior: a name with even
    // one indescribable occurrence is absent from the precise list entirely —
    // the blanket drop applies to ALL its occurrences.
    let mixed = "<?php\nclass C { public function m(string $x): void {} }\n\
                 function f(): void { $s = 'abc'; $c = new C(); trim($s); $c->m($s);\n\
                 \\PHPStan\\dumpType($s); }\n";
    assert_eq!(one_type(mixed), "dumped type: unknown");
    // Two statements, though, are two verdicts — the method call is what drops it.
    let split = "<?php\nclass C { public function m(string $x): void {} }\n\
                 function f(): void { $s = 'abc'; trim($s); \\PHPStan\\dumpType($s); }\n";
    assert_eq!(one_type(split), "dumped type: 'abc'");
}

// The two gates the adversarial review of #637 added: an `ArrayAccess` offset
// read runs userland, and the top-level frame's locals are globals.

/// `$a['k']` on an `ArrayAccess` receiver is `offsetGet`, a userland body that may
/// write anything — here the receiver's own property. Certifying `strstr` by value
/// must not keep `$a->p` across it: in guard position the shape gate refuses the
/// offset read over an object-bound base, in statement position the kept handle
/// takes the ADR-0036 sweep. Either way no `type.argument-mismatch` is
/// manufactured on `want($a->p)`, and the property loses the `1` it was written —
/// falling to its declared `int|string` (issue #620), which is a floor `offsetGet`
/// cannot violate either, since PHP type-checks its write to the slot too.
#[test]
fn an_offset_read_on_an_array_access_receiver_is_not_a_by_value_shape() {
    let src = "<?php
declare(strict_types=1);
final class AA implements ArrayAccess {
    public int|string $p = 1;
    public function offsetGet(mixed $o): mixed { $this->p = 'w'; return 'v'; }
    public function offsetExists(mixed $o): bool { return true; }
    public function offsetSet(mixed $o, mixed $v): void {}
    public function offsetUnset(mixed $o): void {}
}
function want(string $s): void {}
function guard(): void { $a = new AA(); $a->p = 1; if (strstr($a['k'], 'x')) { \\PHPStan\\dumpType($a->p); want($a->p); } }
function stmt(): void { $a = new AA(); $a->p = 1; strstr($a['k'], 'x'); \\PHPStan\\dumpType($a->p); want($a->p); }
";
    let tree = SourceTree::parse(src);
    let ds = check(&tree, &[], "t.php");
    assert!(
        !ds.iter().any(|d| d.id == "type.argument-mismatch"),
        "offsetGet rewrote the property: {ds:?}"
    );
    assert_eq!(
        types(src),
        vec!["dumped type: unknown".to_owned(), "dumped type: int|string".to_owned()]
    );
}

/// The top-level frame is the one whose locals are globals, so a builtin that runs
/// userland through a non-`callable` argument — a generator body under
/// `iterator_to_array` — can rebind them by `global`. The mined certification is
/// refused there and admitted in a function body, where a callback's `global $g`
/// reaches a different `$g`: the function keeps the handle (swept), the script
/// does not.
#[test]
fn the_mined_certification_is_refused_in_the_top_level_frame() {
    let generator = "function gen(): Generator { global $g; $g = 'rebound'; yield 1; }\n";
    let script = format!("<?php\n{generator}$g = gen();\niterator_to_array($g);\n\\PHPStan\\dumpType($g);\n");
    assert_eq!(one_type(&script), "dumped type: unknown", "a script local is a global the body rebinds");
    let func = format!(
        "<?php\n{generator}function f(): void {{ $g = gen(); iterator_to_array($g); \\PHPStan\\dumpType($g); }}\n"
    );
    assert_ne!(one_type(&func), "dumped type: unknown", "a function local is out of the body's reach");
}

/// Named arguments and a spread are not positional, so the guard exemption never
/// looks at the table for them: the whole read set takes the floor.
#[test]
fn the_guard_exemption_refuses_named_arguments_and_a_spread() {
    let named = "<?php\nfunction f(string $s, int $c): void {\n\
                 if (str_replace(subject: $s, search: 'a', replace: 'b', count: $c)) { \\PHPStan\\dumpType($c); \\PHPStan\\dumpType($s); } }\n";
    assert_eq!(types(named), vec!["dumped type: unknown".to_owned(), "dumped type: unknown".to_owned()]);
    let spread = "<?php\nfunction f(array $a): void { if (strstr(...$a)) { \\PHPStan\\dumpType($a); } }\n";
    assert_eq!(one_type(spread), "dumped type: unknown");
}
