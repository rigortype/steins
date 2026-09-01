//! ADR-0064 DR3 — the argument-DISPATCHED symbolic transfers (seam ii), at
//! fixture level.
//!
//! Rules whose answer depends on an argument the S3/S7 rung cannot bind:
//! `explode` separator, `range` bounds, `preg_replace` subject, `var_export`
//! literal flag, `min`/`max` argument list, and the arithmetic scalar-union
//! family's `abs`/`pow` operands (issue #40). Each asserted both refined and
//! declined — decline is first-class (ADR-0061 §1).
//!
//! `json_decode` only declines (bare `mixed`, six-base per-flag envelope, no
//! single `Fact`). `min`/`max` (issue #118) declare the same `mixed` but ARE
//! admitted via the ADR-0064 Amendment B arity second leg, which
//! `json_decode`'s envelope still fails.
//!
//! Zero emission is asserted on every fixture (as `shape_projections.rs`).

use std::collections::HashMap;

use steins_domain::{Base, Fact};
use steins_infer::{DEBUG_TYPE_ID, Diagnostic, Folder, check_with};
use steins_syntax::{ArgValue, SourceTree};

/// Mock reflected declarations (`ReflectionFunction::getReturnType()` at 8.5.8)
/// the DR3 admission gate consults.
#[derive(Default)]
struct Mock {
    types: HashMap<String, String>,
    facts: HashMap<String, Fact>,
    arities: HashMap<String, (u32, u32)>,
    absence: bool,
}

impl Mock {
    fn sidecar() -> Mock {
        let mut types = HashMap::new();
        for (f, t) in [
            ("explode", "array"),
            ("range", "array"),
            ("preg_replace", "array|string|null"),
            ("var_export", "?string"),
            ("json_decode", "mixed"),
            ("min", "mixed"),
            ("max", "mixed"),
            ("array_key_exists", "bool"),
            ("key_exists", "bool"),
            ("curl_getinfo", "mixed"),
            ("filter_var", "mixed"),
            // The arithmetic scalar-union family (issue #40). `abs` declares a
            // union the value domain CAN spell, so its rule is the one the
            // extensional half of the gate actually bites on; `pow` carries an
            // `object` arm (`GMP`) that no `Fact` names, so its declaration
            // pins the rule without bounding it.
            ("abs", "int|float"),
            ("pow", "object|int|float"),
        ] {
            types.insert(f.to_owned(), t.to_owned());
        }
        // `min(mixed $value, mixed ...$values)` at 8.5.8: variadic, 2 declared /
        // 1 required, via `ReflectionFunction::getNumberOfParameters()`.
        let arities = HashMap::from([
            ("min".to_owned(), (2, 1)),
            ("max".to_owned(), (2, 1)),
            // `array_key_exists(mixed $key, array $array)` at 8.5.8: two
            // declared, two required. The arm reads its arguments positionally
            // with the SUBJECT at index 1, so the arity leg is what keeps the
            // read honest if php-src ever grows a parameter in front of it.
            ("array_key_exists".to_owned(), (2, 2)),
            ("key_exists".to_owned(), (2, 2)),
            // `curl_getinfo(CurlHandle $handle, ?int $option = null)` at 8.5.9:
            // two declared, one required (issue #594).
            ("curl_getinfo".to_owned(), (2, 1)),
            // `filter_var(mixed $value, int $filter = FILTER_DEFAULT,
            // array|int $options = 0)` at 8.5.9: three declared, one required
            // (issue #597). A bare `mixed` declaration, so this arity pin is
            // the whole of what countersigns the rule.
            ("filter_var".to_owned(), (3, 1)),
            // `abs(int|float $num)` and `pow(mixed $num, mixed $exponent)` at
            // 8.5.9. Both rules read their arguments positionally, so both pin
            // the signature they were written against (issue #40).
            ("abs".to_owned(), (1, 1)),
            ("pow".to_owned(), (2, 2)),
        ]);
        // `var_export`'s `?string` envelope sits one rung below the transfer, so
        // the null-strip reads as a refinement, not an answer from nowhere.
        let mut facts = HashMap::new();
        facts.insert(
            "var_export".to_owned(),
            Fact::General { base: Base::String, nullable: true },
        );
        Mock { types, facts, arities, absence: true }
    }
}

impl Folder for Mock {
    fn fold(&mut self, _name: &str, _args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        self.absence
    }
    fn builtin_return_fact(&mut self, name: &str) -> Option<Fact> {
        self.facts.get(&name.to_ascii_lowercase()).cloned()
    }
    fn builtin_return_type(&mut self, name: &str) -> Option<String> {
        self.types.get(&name.to_ascii_lowercase()).cloned()
    }
    fn builtin_param_counts(&mut self, name: &str) -> Option<(u32, u32)> {
        self.arities.get(&name.to_ascii_lowercase()).copied()
    }
}

fn diagnostics_with(src: &str, folder: &mut dyn Folder) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "t.php", folder)
}

/// The single `debug.type` body a one-dump source produces, asserting on the way
/// that the source produced NO other finding.
fn one_type_with(src: &str, folder: &mut dyn Folder) -> String {
    let ds = diagnostics_with(src, folder);
    // `untyped.*` is contract-layer claim-absence (issue #200), orthogonal here.
    let other: Vec<&Diagnostic> =
        ds.iter().filter(|d| !d.id.starts_with("debug.") && !d.id.starts_with("untyped.")).collect();
    assert!(other.is_empty(), "a transfer emitted a finding: {other:?}");
    let ty: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ty.len(), 1, "expected exactly one debug.type dump, got {ds:?}");
    ty[0].message.clone()
}

fn one_type(src: &str) -> String {
    one_type_with(src, &mut Mock::sidecar())
}

/// A fixture with a declared signature and one dump in the body.
fn dump(sig: &str, expr: &str) -> String {
    one_type(&format!("<?php\nfunction f({sig}): void {{ \\PHPStan\\dumpType({expr}); }}\n"))
}

/// A fixture whose parameter carries a phpdoc declaration (an `Asserted` premise).
fn dump_doc(doc: &str, sig: &str, expr: &str) -> String {
    one_type(&format!(
        "<?php\n/** {doc} */\nfunction f({sig}): void {{ \\PHPStan\\dumpType({expr}); }}\n"
    ))
}

// explode: non-empty separator ⇒ non-empty-list<string>

#[test]
fn explode_on_a_literal_separator_is_a_non_empty_list_of_strings() {
    // PHP 8 drops the `false` arm; splitting on a non-empty separator always
    // yields at least one piece (`explode(',', '') === ['']`).
    assert_eq!(
        dump("string $s", "explode(',', $s)"),
        "dumped type: non-empty-list<string>"
    );
    assert_eq!(
        dump("string $s", "explode('::', $s)"),
        "dumped type: non-empty-list<string>"
    );
}

#[test]
fn explode_takes_the_separators_own_predicate_not_just_a_literal() {
    // Separator need not be literal: `non-empty-string` is the premise the rule
    // needs; `Refine::Truthy` adds `NON_FALSY`, closing over `NON_EMPTY`.
    let src = "<?php\nfunction f(string $sep, string $s): void {\n\
               if ($sep) { \\PHPStan\\dumpType(explode($sep, $s)); }\n}\n";
    assert_eq!(one_type(src), "dumped type: non-empty-list<string>");
}

#[test]
fn explode_declines_an_empty_or_unknown_separator() {
    // `explode('', 'abc')` throws `ValueError` at 8.5.8 — no return to describe.
    // The floor answer here is the ADR-0069 FLOOR (ADR-0071's `explode` row):
    // coarse `list<string> (asserted)`. The marker proves the decline held —
    // the rule's own answer (`non-empty-list<string>`) carries none.
    assert_eq!(dump("string $s", "explode('', $s)"), "dumped type: list<string> (asserted)");
    assert_eq!(
        dump("string $sep, string $s", "explode($sep, $s)"),
        "dumped type: list<string> (asserted)"
    );
}

#[test]
fn explode_declines_the_limit_form_because_a_limit_can_empty_the_result() {
    // Load-bearing decline: `explode(',', 'a,b,c', -5)` returns `[]` at 8.5.8,
    // so carrying `non-empty` across a limit arg would be a false premise. The
    // floor's `list<string>` stands instead, correct on the empty case.
    assert_eq!(dump("string $s", "explode(',', $s, 2)"), "dumped type: list<string> (asserted)");
    assert_eq!(dump("string $s", "explode(',', $s, -5)"), "dumped type: list<string> (asserted)");
}

// range: always a non-empty list; integral arguments sharpen the element

#[test]
fn range_of_integral_bounds_is_a_non_empty_list_of_ints() {
    assert_eq!(dump("", "range(1, 3)"), "dumped type: non-empty-list<int>");
    // Equal (`range(1,1)===[1]`) and descending bounds still produce a
    // non-empty list; an integral step keeps the element bound; non-literal
    // bounds work the same since the rule reads facts, not text.
    assert_eq!(dump("", "range(5, 5)"), "dumped type: non-empty-list<int>");
    assert_eq!(dump("", "range(3, 1)"), "dumped type: non-empty-list<int>");
    assert_eq!(dump("", "range(1, 9, 2)"), "dumped type: non-empty-list<int>");
    assert_eq!(dump("int $a, int $b", "range($a, $b)"), "dumped type: non-empty-list<int>");
}

#[test]
fn range_keeps_the_list_and_the_non_emptiness_when_the_element_is_unknown() {
    // Fractional step (float array) or string bounds (`range('a','c')===
    // ['a','b','c']`) drop the element bound, but never emptiness or list-ness.
    assert_eq!(dump("", "range(1, 2, 0.5)"), "dumped type: non-empty-list<mixed>");
    assert_eq!(dump("", "range('a', 'c')"), "dumped type: non-empty-list<mixed>");
    assert_eq!(dump("float $a, float $b", "range($a, $b)"), "dumped type: non-empty-list<mixed>");
}

#[test]
fn range_declines_an_arity_php_itself_rejects() {
    // 1 or 4 args is `ArgumentCountError` — PHP won't make the call, so the
    // seam declines. ADR-0069 §2's floor is arity-blind, answering bare
    // `array` (asserted) instead of the withheld `non-empty-list<int>`.
    assert_eq!(dump("", "range(1)"), "dumped type: array (asserted)");
    assert_eq!(dump("", "range(1, 2, 3, 4)"), "dumped type: array (asserted)");
}

// preg_replace: the subject's base splits the multi-base declaration

#[test]
fn preg_replace_of_a_string_subject_is_string_or_null() {
    // Reflected `array|string|null` is multi-base and seeds no fact; the
    // subject's own base splits it — `$pattern` shape and trailing $limit/
    // $count don't change the answer, only `$subject` governs.
    assert_eq!(
        dump("string $s", "preg_replace('/a/', 'b', $s)"),
        "dumped type: string|null"
    );
    assert_eq!(
        dump("string $s", "preg_replace(['/a/', '/b/'], 'z', $s)"),
        "dumped type: string|null"
    );
    assert_eq!(
        dump("string $s", "preg_replace('/a/', 'b', $s, 1)"),
        "dumped type: string|null"
    );
}

#[test]
fn preg_replace_of_an_array_subject_is_array_or_null() {
    assert_eq!(
        dump_doc("@param array{a: string} $v", "array $v", "preg_replace('/a/', 'b', $v)"),
        "dumped type: array|null (asserted)"
    );
}

#[test]
fn preg_replace_declines_a_subject_it_cannot_place() {
    // Unknown or nullable subject base declines the split: the floor's
    // unsplit `string|null|array (asserted)` states both arms instead of
    // choosing one.
    assert_eq!(
        // `$u`: bound but fact-less, i.e. the unknown base this declines on.
        dump("$u", "preg_replace('/a/', 'b', $u)"),
        "dumped type: string|null|array (asserted)"
    );
    assert_eq!(
        dump("?string $s", "preg_replace('/a/', 'b', $s)"),
        "dumped type: string|null|array (asserted)"
    );
}

// var_export: the literal `true` flag strips the envelope's null arm

#[test]
fn var_export_with_a_literal_true_flag_is_a_string() {
    // `$value` doesn't matter: `var_export(null, true) === 'NULL'` (a string).
    assert_eq!(dump("int $v", "var_export($v, true)"), "dumped type: string");
    assert_eq!(dump("", "var_export(null, true)"), "dumped type: string");
}

#[test]
fn var_export_without_the_flag_falls_back_to_its_own_envelope() {
    // One-arg / literal-`false` forms decline to the reflected `?string` envelope.
    assert_eq!(dump("int $v", "var_export($v)"), "dumped type: string|null");
    assert_eq!(dump("int $v", "var_export($v, false)"), "dumped type: string|null");
    assert_eq!(dump("int $v, bool $b", "var_export($v, $b)"), "dumped type: string|null");
}

// min / max: the argument-fact union, and the interval that sharpens it

#[test]
fn min_and_max_compose_the_intervals_of_int_arguments() {
    // `min(a,b) ∈ [min(lo),min(hi)]`, `max` dually — interval arithmetic,
    // either argument order.
    assert_eq!(
        dump_doc("@param int<0, max> $r", "int $r", "min($r, 100)"),
        "dumped type: int<0, 100> (asserted)"
    );
    assert_eq!(
        dump_doc("@param int<0, max> $r", "int $r", "min(100, $r)"),
        "dumped type: int<0, 100> (asserted)"
    );
    assert_eq!(
        dump_doc("@param int<min, 100> $r", "int $r", "max($r, 0)"),
        "dumped type: int<0, 100> (asserted)"
    );
    assert_eq!(
        dump_doc("@param int<1, 6> $r", "int $r", "min($r, 4)"),
        "dumped type: int<1, 4> (asserted)"
    );
    assert_eq!(
        dump_doc("@param int<1, 6> $r", "int $r", "max(4, $r)"),
        "dumped type: int<4, 6> (asserted)"
    );
    // A bare `int` param is the full interval; composition still bounds one side.
    assert_eq!(dump("int $i", "min($i, 5)"), "dumped type: int<min, 5>");
    assert_eq!(dump("int $i", "max($i, 5)"), "dumped type: int<5, max>");
    // 3 args compose left-to-right; a collapse to a point spells the point —
    // `min`/`max` aren't on the folding allowlist, so nothing else would answer.
    assert_eq!(dump("int $i", "min(3, 1, 2)"), "dumped type: 1");
    assert_eq!(dump("int $i", "max(3, 1, 2)"), "dumped type: 3");
}

#[test]
fn the_union_is_the_answer_where_the_interval_is_not() {
    // Load-bearing: min/max return one of their ARGUMENTS, so the union of
    // argument facts is sound with no comparison-semantics premise. Witnessed
    // at 8.5.8: `min('a', 1) === 1`, the 2nd argument verbatim.
    assert_eq!(dump_doc("@param 'a'|'b' $s", "string $s", "min($s, 'c')"), "dumped type: 'a'|'b'|'c' (asserted)");
    // Two facts of the same base with no interval between them join in the domain.
    assert_eq!(dump("string $s", "max($s, 'c')"), "dumped type: string");
}

#[test]
fn the_unary_array_form_answers_from_the_shape() {
    // 1-arg ARRAY form: result is one of the array's elements, so the shape's
    // value union is the claim. `min([])` throws — no `non_empty` premise needed.
    assert_eq!(
        dump_doc("@param array{a: int, b: int} $v", "array $v", "max($v)"),
        "dumped type: int (asserted)"
    );
    assert_eq!(
        dump_doc("@param list<string> $v", "array $v", "min($v)"),
        "dumped type: string (asserted)"
    );
    // A witnessed array lifts first; which element wins is declined, union claims.
    assert_eq!(
        one_type("<?php\nfunction f(): void { \\PHPStan\\dumpType(min([1, 2, 3])); }\n"),
        "dumped type: 1|2|3"
    );
}

#[test]
fn min_and_max_decline_where_they_cannot_state_an_answer() {
    // An arg with no usable fact declines the whole rule — the missing one
    // could hold the winner. `$u` carries nothing.
    let src = "<?php\nfunction f(int $i): void { $u = frobnicate(); \\PHPStan\\dumpType(min($i, $u)); }\n";
    assert_eq!(one_type(src), "dumped type: unknown");
    // No longer declines (issue #339): `min($int, $string)` returns one of its
    // args, so `int|string` is the answer. Previously declined for lack of a
    // two-base form (ADR-0062 Amendment B deviation); `Fact::Union` discharges it.
    assert_eq!(dump("int $i, string $s", "min($i, $s)"), "dumped type: int|string");
    // Nullable int leaves the INTERVAL path (`min(null,5) === NULL` at 8.5.8)
    // for the union, which carries the null side correctly.
    assert_eq!(dump("?int $i", "min($i, 5)"), "dumped type: int|null");
    // 1-arg call whose fact isn't an array declines — `min(5)` is a `TypeError`.
    assert_eq!(dump("int $i", "min($i)"), "dumped type: unknown");
    assert_eq!(dump("int $i", "min()"), "dumped type: unknown");
}

#[test]
fn an_engine_that_answers_no_arity_withholds_min_and_max() {
    // ADR-0064 Amendment B: bare `mixed` pins nothing, so the arity signature
    // is what countersigns min/max. A runner unable to state it withholds it.
    let mut mock = Mock::sidecar();
    mock.arities.clear();
    let src = "<?php\nfunction f(int $i): void { \\PHPStan\\dumpType(min($i, 5)); }\n";
    assert_eq!(one_type_with(src, &mut mock), "dumped type: unknown");
    // A signature that has MOVED withholds it too — the rule would be stale.
    let mut mock = Mock::sidecar();
    mock.arities.insert("min".to_owned(), (3, 2));
    assert_eq!(one_type_with(src, &mut mock), "dumped type: unknown");
}

// curl_getinfo: a fixed per-constant table (issue #594)

#[test]
fn curl_getinfo_of_a_recognized_int_constant_is_int() {
    // The issue's own witness.
    assert_eq!(dump("$h", "curl_getinfo($h, CURLINFO_HTTP_CODE)"), "dumped type: int");
    assert_eq!(dump("$h", "curl_getinfo($h, CURLINFO_FILETIME)"), "dumped type: int");
}

#[test]
fn curl_getinfo_of_a_recognized_float_constant_is_float() {
    assert_eq!(dump("$h", "curl_getinfo($h, CURLINFO_TOTAL_TIME)"), "dumped type: float");
}

#[test]
fn curl_getinfo_of_a_recognized_string_constant_is_string() {
    // Verified apart from the `T|false` family: the C-level field coalesces an
    // unset value to `''`, never `false` (module doc on `curl_getinfo_transfer`).
    assert_eq!(dump("$h", "curl_getinfo($h, CURLINFO_EFFECTIVE_URL)"), "dumped type: string");
}

#[test]
fn curl_getinfo_declines_a_true_false_constant() {
    // `CURLINFO_CONTENT_TYPE` is `string|false` — no `Fact` spells a two-base
    // union, the same floor `min`/`json_decode` stand on.
    assert_eq!(dump("$h", "curl_getinfo($h, CURLINFO_CONTENT_TYPE)"), "dumped type: unknown");
    // `CURLINFO_PRIVATE` echoes the caller's own `CURLOPT_PRIVATE` — `mixed`.
    assert_eq!(dump("$h", "curl_getinfo($h, CURLINFO_PRIVATE)"), "dumped type: unknown");
}

#[test]
fn curl_getinfo_declines_the_zero_option_whole_array_form() {
    // `array{…}|false` — a shape-typed result outside this scalar table.
    assert_eq!(dump("$h", "curl_getinfo($h)"), "dumped type: unknown");
}

#[test]
fn curl_getinfo_declines_a_non_constant_option() {
    assert_eq!(dump("$h", "curl_getinfo($h, 'CURLINFO_HTTP_CODE')"), "dumped type: unknown");
    assert_eq!(dump("$h, int $opt", "curl_getinfo($h, $opt)"), "dumped type: unknown");
    assert_eq!(dump("$h", "curl_getinfo($h, null)"), "dumped type: unknown");
}

#[test]
fn curl_getinfo_declines_an_unrecognized_constant() {
    // A real constant, but not a `CURLINFO_*` one at all.
    assert_eq!(dump("$h", "curl_getinfo($h, PHP_EOL)"), "dumped type: unknown");
    // A `CURLINFO_*`-shaped name this table does not carry: `CURLINFO_SCHEME`
    // measures `bool(false)` on an untouched handle exactly like the confirmed
    // `T|false` rows (module doc), so it stays out rather than trusting
    // php.net's plain `string` word against that measurement; the 8.4-only
    // `CURLINFO_POSTTRANSFER_TIME_T` is excluded for the matching reason —
    // its value is additionally gated on a libcurl version no PHP-minor pin
    // can see.
    assert_eq!(dump("$h", "curl_getinfo($h, CURLINFO_SCHEME)"), "dumped type: unknown");
    assert_eq!(
        dump("$h", "curl_getinfo($h, CURLINFO_POSTTRANSFER_TIME_T)"),
        "dumped type: unknown"
    );
}

#[test]
fn a_qualified_or_relative_constant_name_is_not_the_global_one() {
    // `Foo\CURLINFO_HTTP_CODE` and `namespace\CURLINFO_HTTP_CODE` both denote a
    // constant OTHER than the global `\CURLINFO_HTTP_CODE` — the same
    // `FullyQualified`/`Unqualified`-only admission `cond.rs`'s
    // `PHP_VERSION_ID` check applies.
    assert_eq!(
        dump("$h", "curl_getinfo($h, Foo\\CURLINFO_HTTP_CODE)"),
        "dumped type: unknown"
    );
    let src = "<?php\nnamespace App;\nfunction f($h): void {\n\
               \\PHPStan\\dumpType(curl_getinfo($h, namespace\\CURLINFO_HTTP_CODE));\n}\n";
    assert_eq!(one_type(src), "dumped type: unknown");
}

#[test]
fn an_engine_that_answers_no_arity_withholds_curl_getinfo() {
    let mut mock = Mock::sidecar();
    mock.arities.clear();
    let src = "<?php\nfunction f($h): void { \\PHPStan\\dumpType(curl_getinfo($h, CURLINFO_HTTP_CODE)); }\n";
    assert_eq!(one_type_with(src, &mut mock), "dumped type: unknown");
    let mut mock = Mock::sidecar();
    mock.arities.insert("curl_getinfo".to_owned(), (3, 1));
    assert_eq!(one_type_with(src, &mut mock), "dumped type: unknown");
}

#[test]
fn a_project_function_shadowing_curl_getinfo_declines() {
    let src = "<?php\nfunction curl_getinfo($h, $opt = null): mixed { return 'shadowed'; }\n\
               function f($h): void { \\PHPStan\\dumpType(curl_getinfo($h, CURLINFO_HTTP_CODE)); }\n";
    let ds = diagnostics_with(src, &mut Mock::sidecar());
    let ty: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ty.len(), 1);
    assert_eq!(ty[0].message, "dumped type: unknown");
}

// abs: the argument's base, folded onto the non-negative half (issue #40)

/// A bounded int interval is EXACT under `abs`: non-negative intervals pass
/// through, all-negative ones come back reversed, straddling ones are floored at
/// zero and capped by whichever end is further from it.
#[test]
fn abs_reflects_a_bounded_int_interval() {
    assert_eq!(dump_doc("@param int<0, 123> $i", "int $i", "abs($i)"), "dumped type: int<0, 123> (asserted)");
    assert_eq!(dump_doc("@param int<-456, -123> $i", "int $i", "abs($i)"), "dumped type: int<123, 456> (asserted)");
    assert_eq!(dump_doc("@param int<-123, 123> $i", "int $i", "abs($i)"), "dumped type: int<0, 123> (asserted)");
    assert_eq!(dump_doc("@param int<-20, 25> $i", "int $i", "abs($i)"), "dumped type: int<0, 25> (asserted)");
    assert_eq!(dump_doc("@param positive-int $i", "int $i", "abs($i)"), "dumped type: int<1, max> (asserted)");
    assert_eq!(dump_doc("@param non-negative-int $i", "int $i", "abs($i)"), "dumped type: int<0, max> (asserted)");
    // A one-point interval spells the point.
    assert_eq!(dump_doc("@param -1 $i", "int $i", "abs($i)"), "dumped type: 1 (asserted)");
    assert_eq!(dump_doc("@param 0 $i", "int $i", "abs($i)"), "dumped type: 0 (asserted)");
    // A finite union collapses through the same per-value rule.
    assert_eq!(dump_doc("@param -2|2 $i", "int $i", "abs($i)"), "dumped type: 2 (asserted)");
}

/// **`abs(PHP_INT_MIN)` is a float** (`float(9.223372036854776E+18)` at 8.5.9),
/// so an interval admitting it is not an int interval on the other side. The
/// rule declines and the ADR-0069 floor's honest `int<0, max>|float` stands —
/// which is exactly the `int<0, max>` that phpstan-src's own fixture asserts
/// for `@var int` and cannot have.
#[test]
fn an_int_interval_admitting_php_int_min_declines() {
    let floor = "dumped type: int<0, max>|float (asserted)";
    assert_eq!(dump("int $i", "abs($i)"), floor);
    assert_eq!(dump_doc("@param negative-int $i", "int $i", "abs($i)"), floor);
    assert_eq!(dump_doc("@param int<min, 0> $i", "int $i", "abs($i)"), floor);
    assert_eq!(dump_doc("@param int<min, -123> $i", "int $i", "abs($i)"), floor);
}

/// The float half is total, and the string half is the fold lane's
/// engine-width question (ADR-0064 seam (i)), never this rung's.
#[test]
fn abs_answers_float_and_declines_a_string() {
    assert_eq!(dump("float $f", "abs($f)"), "dumped type: float");
    assert_eq!(dump("string $s", "abs($s)"), "dumped type: int<0, max>|float (asserted)");
    assert_eq!(
        dump_doc("@param numeric-string $s", "string $s", "abs($s)"),
        "dumped type: int<0, max>|float (asserted)"
    );
    // Arity: `abs` takes exactly one argument, and a second one is not a call
    // this rule was written against.
    assert_eq!(dump_doc("@param int<1, 5> $i", "int $i", "abs($i, 2)"), "dumped type: int<0, max>|float (asserted)");
}

/// An int/float union keeps both arms, each mapped by its own rule — and where
/// the int arm admits `PHP_INT_MIN` the overflow lands in the float arm the
/// union already carries, so the whole answer stays expressible.
#[test]
fn abs_maps_an_int_float_union_arm_by_arm() {
    let src = "<?php\n/** @param int<-20, -10> $i */\nfunction f(int $i, float $g, bool $c): void {\n\
               $x = $c ? $i : $g;\n\\PHPStan\\dumpType(abs($x));\n}\n";
    assert_eq!(one_type(src), "dumped type: int<10, 20>|float (asserted)");
}

// pow: `int|float`, sharpened where one operand pins the answer (issue #40)

#[test]
fn pow_of_two_numeric_operands_is_int_or_float() {
    // The int/int case is an int until it overflows the word (`pow(2, 63)` is a
    // float) — which one is a VALUE question, so the rung states the union.
    assert_eq!(dump("int $i, int $j", "pow($i, $j)"), "dumped type: int|float");
    assert_eq!(dump("int $i, string $s", "pow($i, $s)"), "dumped type: int|float");
    assert_eq!(dump("bool $b, int $i", "pow($b, $i)"), "dumped type: int|float");
}

/// An exponent that numerifies to the integer 0 answers `1` — `1.0` for a float
/// base (`pow(2.0, 0)` is `float(1)`), and `1|1.0` for a string base, since
/// `pow("5.5", 0)` is `float(1)` and `pow("5", 0)` is `int(1)`.
#[test]
fn a_zero_exponent_answers_one_in_the_bases_own_shape() {
    assert_eq!(dump("int $i", "pow($i, 0)"), "dumped type: 1");
    assert_eq!(dump("int $i", "pow($i, false)"), "dumped type: 1");
    assert_eq!(dump("float $f", "pow($f, 0)"), "dumped type: 1.0");
    assert_eq!(dump("string $s", "pow($s, 0)"), "dumped type: 1|1.0");
    assert_eq!(dump("bool $b", "pow($b, 0)"), "dumped type: 1");
}

/// An exponent that numerifies to the integer 1 answers the base numerified:
/// `pow(2, true)` is `int(2)`, `pow(2.0, true)` is `float(2)`.
#[test]
fn a_one_exponent_answers_the_base_numerified() {
    assert_eq!(dump("int $i", "pow($i, 1)"), "dumped type: int");
    assert_eq!(dump("int $i", "pow($i, true)"), "dumped type: int");
    assert_eq!(dump("float $f", "pow($f, 1)"), "dumped type: float");
    assert_eq!(dump("bool $b", "pow($b, 1)"), "dumped type: int");
    // A string base numerifies to an int OR a float, so the union stands.
    assert_eq!(dump("string $s", "pow($s, 1)"), "dumped type: int|float");
}

/// php-src promotes the whole operation to a double as soon as one operand is
/// one, so a float exponent takes an int base with it (`pow(2, 0.0)` is
/// `float(1)` — which is why a float spelling is NOT read as the zero exponent).
#[test]
fn either_operand_being_a_float_makes_the_answer_a_float() {
    assert_eq!(dump("int $i, float $f", "pow($i, $f)"), "dumped type: float");
    assert_eq!(dump("float $f, int $i", "pow($f, $i)"), "dumped type: float");
    assert_eq!(dump("int $i", "pow($i, 0.0)"), "dumped type: float");
}

/// A **nullable float** is the one operand nullability decides, and it decides
/// it by declining: `pow(null, 2)` is `int(0)`, not a float, so a `?float`
/// falls to the `int|float` that admits both halves — where a `?int` keeps
/// every sharpening, since `null` numerifies to an int too.
#[test]
fn a_nullable_float_operand_pins_no_base() {
    assert_eq!(dump("?float $f, int $i", "pow($f, $i)"), "dumped type: int|float");
    assert_eq!(dump("int $i, ?float $f", "pow($i, $f)"), "dumped type: int|float");
    // …including the exponent shortcuts, which would otherwise claim `1.0` and
    // `float` for a call that answers `int(1)` and `int(0)` on the null half.
    assert_eq!(dump("?float $f", "pow($f, 0)"), "dumped type: int|float");
    assert_eq!(dump("?float $f", "pow($f, 1)"), "dumped type: int|float");
    // The nullable int/bool/string halves keep theirs.
    assert_eq!(dump("?int $i", "pow($i, 0)"), "dumped type: 1");
    assert_eq!(dump("?int $i", "pow($i, 1)"), "dumped type: int");
    assert_eq!(dump("?string $s", "pow($s, 0)"), "dumped type: 1|1.0");
}

/// An array operand is a `TypeError`, not a number, and an operand with no fact
/// at all — an object, the `GMP` the `object` arm of `pow`'s declaration is
/// about — never reaches the rule.
#[test]
fn pow_declines_an_array_or_factless_operand() {
    assert_eq!(dump("int $i, array $a", "pow($i, $a)"), "dumped type: unknown");
    assert_eq!(dump("int $i, array $a", "pow($a, $i)"), "dumped type: unknown");
    assert_eq!(dump("int $i, \\DateTime $d", "pow($i, $d)"), "dumped type: unknown");
    // Wrong arity: `pow($num, $exponent)` is exactly two operands.
    assert_eq!(dump("int $i", "pow($i)"), "dumped type: unknown");
    assert_eq!(dump("int $i", "pow($i, 2, 3)"), "dumped type: unknown");
}

/// The gate, on the family's two names: an engine whose declaration has moved
/// withholds the rule, and what is left is what the lower rungs already said.
#[test]
fn a_moved_arithmetic_declaration_withholds_the_rule() {
    let mut mock = Mock::sidecar();
    // `int|false` is not `int|float`: the rule is stale and discarded, and the
    // ADR-0069 floor speaks instead.
    mock.types.insert("abs".to_owned(), "int|false".to_owned());
    let src = "<?php\n/** @param int<0, 9> $i */\nfunction f(int $i): void { \\PHPStan\\dumpType(abs($i)); }\n";
    assert_eq!(one_type_with(src, &mut mock), "dumped type: int<0, max>|float (asserted)");

    // The arity leg, independently: both rules read their operands
    // positionally, so a signature that grew a parameter withholds them.
    let mut mock = Mock::sidecar();
    mock.arities.insert("pow".to_owned(), (3, 3));
    let src = "<?php\nfunction f(int $i, int $j): void { \\PHPStan\\dumpType(pow($i, $j)); }\n";
    assert_eq!(one_type_with(src, &mut mock), "dumped type: unknown");
}

// json_decode: the batch's measured decline

#[test]
fn json_decode_declines_in_every_form() {
    // Bare `mixed` declaration; `$assoc=true` form admits 6 bases
    // (`array|int|float|string|bool|null`), no single `Fact` — declines
    // rather than guess an arm.
    assert_eq!(dump("string $s", "json_decode($s)"), "dumped type: unknown");
    assert_eq!(dump("string $s", "json_decode($s, true)"), "dumped type: unknown");
    assert_eq!(dump("string $s", "json_decode($s, false)"), "dumped type: unknown");
}

// The admission gate (ADR-0061 §2), on the new rules

#[test]
fn without_the_reflected_declaration_every_transfer_is_withheld() {
    struct NoPhp;
    impl Folder for NoPhp {
        fn fold(&mut self, _name: &str, _args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
            None
        }
    }
    // These used to read `unknown`; now ADR-0069's declared-return FLOOR
    // answers underneath with the catalog declaration, marked `(asserted)`.
    // The ADR-0061 §2 gate is untouched — the *transfer* is still withheld,
    // only the coarse declaration reaches the dump (never a refined
    // `non-empty-*` or split answer).
    for (expr, floor) in [
        ("explode(',', $s)", "list<string>"),
        ("range(1, 3)", "array"),
        ("preg_replace('/a/', 'b', $s)", "string|null|array"),
        ("var_export($s, true)", "string|null"),
        // The arithmetic family (issue #40). `abs` has an ADR-0069 floor row to
        // fall to; `pow` has none, so a silent engine leaves it `unknown` — the
        // sound subset, and what every `pow` call said before this wave.
        ("abs(strlen($s))", "int<0, max>|float"),
    ] {
        let src =
            format!("<?php\nfunction f(string $s): void {{ \\PHPStan\\dumpType({expr}); }}\n");
        assert_eq!(
            one_type_with(&src, &mut NoPhp),
            format!("dumped type: {floor} (asserted)"),
            "no-PHP run must withhold the transfer for {expr} and fall to the floor"
        );
    }
    let src = "<?php\nfunction f(int $i, int $j): void { \\PHPStan\\dumpType(pow($i, $j)); }\n";
    assert_eq!(one_type_with(src, &mut NoPhp), "dumped type: unknown");
}

#[test]
fn a_declaration_the_rule_was_not_written_against_withholds_it() {
    // Widening staleness (ADR-0061 §2): engine declares `array|false` for
    // `explode` (not what the rule expects), so the claim is discarded.
    // `array|false` also seeds no fact (multi-base), so the ADR-0069 floor
    // speaks: coarse `list<string>`, never the discarded `non-empty-list<string>`.
    let mut mock = Mock::sidecar();
    mock.types.insert("explode".to_owned(), "array|false".to_owned());
    let src = "<?php\nfunction f(string $s): void { \\PHPStan\\dumpType(explode(',', $s)); }\n";
    assert_eq!(one_type_with(src, &mut mock), "dumped type: list<string> (asserted)");
}

#[test]
fn a_project_function_shadowing_the_name_declines() {
    let src = "<?php\nfunction explode(string $a, string $b): array { return []; }\n\
               function f(string $s): void { \\PHPStan\\dumpType(explode(',', $s)); }\n";
    let ds = diagnostics_with(src, &mut Mock::sidecar());
    let ty: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ty.len(), 1);
    assert_eq!(ty[0].message, "dumped type: unknown");
}


// `array_key_exists` read as a VALUE (issue #343)


/// The pair has narrowed a shape's presence as a GUARD since ADR-0062 §4 and
/// answered nothing sharper than `bool` when its result was READ — against a fact
/// that carries the answer. The subject is argument **1**, which is why this is a
/// DR3 arm and not a shape-projection one.
#[test]
fn a_declared_shape_decides_the_key_question() {
    let decl = "/** @param array{p: 1, q: string} $z */\n";
    // A required field is present in every realization the shape admits.
    assert_eq!(
        one_type_with(
            &format!("<?php\n{decl}function f(array $z): void {{ \\PHPStan\\dumpType(\\array_key_exists('p', $z)); }}\n"),
            &mut Mock::sidecar()
        ),
        "dumped type: true (asserted)"
    );
    // An undeclared key under a SEALED tail: sealed is exactly the claim that no
    // undeclared key may be present.
    assert_eq!(
        one_type_with(
            &format!("<?php\n{decl}function f(array $z): void {{ \\PHPStan\\dumpType(\\array_key_exists('zz', $z)); }}\n"),
            &mut Mock::sidecar()
        ),
        "dumped type: false (asserted)"
    );
}

/// A witnessed literal carries the same answer at the `Verified` stratum — the
/// absent `(asserted)` marker is the whole difference, and it is what keeps
/// ADR-0062 A-G9's corollary honest about which of the two may premise a
/// proof-layer finding.
#[test]
fn a_witnessed_literal_decides_it_too_and_at_its_own_stratum() {
    let src = "<?php\nfunction g(int $x): void {\n  $c = ['p' => 1, 'q' => $x];\n  \\PHPStan\\dumpType(\\array_key_exists('p', $c));\n}\n";
    assert_eq!(one_type_with(src, &mut Mock::sidecar()), "dumped type: true");
    let absent = "<?php\nfunction g(int $x): void {\n  $c = ['p' => 1, 'q' => $x];\n  \\PHPStan\\dumpType(\\array_key_exists('zz', $c));\n}\n";
    assert_eq!(one_type_with(absent, &mut Mock::sidecar()), "dumped type: false");
}

/// The two genuinely undecided shapes keep `bool`. An optional field may or may
/// not be there by definition, and an unsealed tail is the claim that undeclared
/// keys are admitted — neither supports a verdict, and `Maybe` is the honest
/// answer the arm lane gives everywhere else.
#[test]
fn an_optional_field_and_an_unsealed_tail_keep_bool() {
    for (decl, key) in [("array{p?: int}", "p"), ("array{p: int, ...}", "zz")] {
        let src = format!(
            "<?php\n/** @param {decl} $o */\nfunction f(array $o): void {{ \\PHPStan\\dumpType(\\array_key_exists('{key}', $o)); }}\n"
        );
        assert_eq!(
            one_type_with(&src, &mut Mock::sidecar()),
            "dumped type: bool (asserted)",
            "{decl}"
        );
    }
}

/// `array_key_exists` asks about PRESENCE, not about the value — a `null` value
/// is still a present key. This is the half `isset` would answer differently, and
/// the pin that says the two questions are not the same one.
#[test]
fn a_null_valued_field_is_still_present() {
    let src = "<?php\n/** @param array{p: ?int} $n */\nfunction f(array $n): void { \\PHPStan\\dumpType(\\array_key_exists('p', $n)); }\n";
    assert_eq!(one_type_with(src, &mut Mock::sidecar()), "dumped type: true (asserted)");
}

/// A key that is not a concrete literal names no field to look up, and guessing
/// one from its type is a different rung.
#[test]
fn a_non_literal_key_decides_nothing() {
    let src = "<?php\n/** @param array{p: int} $z */\nfunction f(array $z, string $k): void { \\PHPStan\\dumpType(\\array_key_exists($k, $z)); }\n";
    assert_eq!(one_type_with(src, &mut Mock::sidecar()), "dumped type: bool (asserted)");
}

/// The declaration gate applies here as everywhere: a sidecar that does not
/// report `bool` for the name declines the arm, and the verdict is not produced.
/// What is left is the answer every other rung was already giving — which is the
/// point of the gate, not a second answer from nowhere.
#[test]
fn the_declaration_gate_still_governs() {
    let mut mock = Mock::sidecar();
    mock.types.insert("array_key_exists".to_owned(), "int".to_owned());
    let src = "<?php\n/** @param array{p: int} $z */\nfunction f(array $z): void { \\PHPStan\\dumpType(\\array_key_exists('p', $z)); }\n";
    assert_eq!(one_type_with(src, &mut mock), "dumped type: bool (asserted)");
}

// filter_var: the (filter × flags × input) grid (issue #597)
//
// The winnable set is the one the four-layer domain can spell: every
// `FILTER_NULL_ON_FAILURE` combination (`T|null`), the plain `bool` of
// `FILTER_VALIDATE_BOOL`, and the success-proven inputs whose failure arm
// vanishes. `T|false` with `T != bool` has no `Fact` and declines.

#[test]
fn filter_var_null_on_failure_is_the_issues_witness() {
    // The issue's witness, exactly: `int|null`, where master answered `unknown`.
    assert_eq!(
        dump("string $s", "filter_var($s, FILTER_VALIDATE_INT, FILTER_NULL_ON_FAILURE)"),
        "dumped type: int|null"
    );
}

#[test]
fn filter_var_null_on_failure_spells_every_filters_success_type() {
    for (filter, want) in [
        ("FILTER_VALIDATE_INT", "int|null"),
        ("FILTER_VALIDATE_FLOAT", "float|null"),
        ("FILTER_VALIDATE_BOOL", "bool|null"),
        ("FILTER_VALIDATE_BOOLEAN", "bool|null"),
        ("FILTER_VALIDATE_EMAIL", "non-falsy-string|null"),
        ("FILTER_VALIDATE_URL", "non-falsy-string|null"),
        ("FILTER_VALIDATE_IP", "non-falsy-string|null"),
        ("FILTER_VALIDATE_MAC", "non-falsy-string|null"),
        // `''` and `'0'` both validate as domains — measured, against upstream's
        // `non-empty-string`. See the rule's own doc.
        ("FILTER_VALIDATE_DOMAIN", "string|null"),
        ("FILTER_SANITIZE_EMAIL", "string|null"),
        ("FILTER_SANITIZE_URL", "string|null"),
        ("FILTER_SANITIZE_NUMBER_INT", "string|null"),
        ("FILTER_SANITIZE_FULL_SPECIAL_CHARS", "string|null"),
        ("FILTER_DEFAULT", "string|null"),
        ("FILTER_UNSAFE_RAW", "string|null"),
    ] {
        // A `mixed` subject: nothing is proven about the input, so the failure
        // arm stands and `null` is what it spells.
        assert_eq!(
            dump("$m", &format!("filter_var($m, {filter}, FILTER_NULL_ON_FAILURE)")),
            format!("dumped type: {want}"),
            "{filter}"
        );
    }
}

#[test]
fn filter_var_reads_the_flag_out_of_an_options_array_literal() {
    // `['flags' => FILTER_NULL_ON_FAILURE]` is the documented spelling and the
    // one `filterVar.php` uses once per filter block.
    assert_eq!(
        dump("$m", "filter_var($m, FILTER_VALIDATE_INT, ['flags' => FILTER_NULL_ON_FAILURE])"),
        "dumped type: int|null"
    );
    // An empty literal carries no flags at all — same as an absent argument.
    assert_eq!(dump("$m", "filter_var($m, FILTER_VALIDATE_BOOL, [])"), "dumped type: bool");
}

#[test]
fn filter_var_validate_bool_is_plain_bool_without_the_null_flag() {
    // `false` is BOTH the failure value and a valid parse of `'false'`/`'off'`,
    // so `bool|false` IS `bool` — the one base whose `T|false` has a `Fact`.
    assert_eq!(dump("$m", "filter_var($m, FILTER_VALIDATE_BOOL)"), "dumped type: bool");
    assert_eq!(dump("$m", "filter_var($m, FILTER_VALIDATE_BOOLEAN)"), "dumped type: bool");
    assert_eq!(dump("string $s", "filter_var($s, FILTER_VALIDATE_BOOL)"), "dumped type: bool");
}

#[test]
fn filter_var_success_proven_inputs_lose_the_failure_arm() {
    // `filter_var($int, FILTER_VALIDATE_INT)` is the identity over the whole int
    // range, both edges included — so the input's own refinement rides through.
    assert_eq!(dump("int $i", "filter_var($i, FILTER_VALIDATE_INT)"), "dumped type: int");
    assert_eq!(
        dump("int $i", "filter_var($i, FILTER_VALIDATE_INT, FILTER_NULL_ON_FAILURE)"),
        "dumped type: int"
    );
    assert_eq!(dump("bool $b", "filter_var($b, FILTER_VALIDATE_BOOL)"), "dumped type: bool");
    assert_eq!(
        dump("bool $b", "filter_var($b, FILTER_VALIDATE_BOOL, FILTER_NULL_ON_FAILURE)"),
        "dumped type: bool"
    );
    // An `int` always validates as a float, and the value is the `(float)` cast.
    assert_eq!(dump("int $i", "filter_var($i, FILTER_VALIDATE_FLOAT)"), "dumped type: float");
    // A sanitizer cannot fail on a value the domain denotes (all scalars or null).
    assert_eq!(dump("string $s", "filter_var($s, FILTER_SANITIZE_EMAIL)"), "dumped type: string");
}

#[test]
fn filter_var_success_proven_inputs_keep_their_own_refinement() {
    // `int<0, 9>` in, `int<0, 9>` out: the identity is the identity.
    assert_eq!(
        dump_doc("@param int<0, 9> $i", "int $i", "filter_var($i, FILTER_VALIDATE_INT)"),
        "dumped type: int<0, 9> (asserted)"
    );
    // `FILTER_DEFAULT` is the `(string)` cast, so a `non-empty-string` survives it.
    assert_eq!(
        dump_doc("@param non-empty-string $s", "string $s", "filter_var($s, FILTER_DEFAULT)"),
        "dumped type: non-empty-string (asserted)"
    );
}

#[test]
fn filter_var_default_is_the_string_cast_of_whatever_it_is_given() {
    assert_eq!(dump("string $s", "filter_var($s)"), "dumped type: string");
    assert_eq!(dump("string $s", "filter_var($s, FILTER_UNSAFE_RAW)"), "dumped type: string");
    // The cast grid's own rows, reached through it rather than restated here.
    assert_eq!(dump("int $i", "filter_var($i)"), "dumped type: numeric-uncased-string");
    assert_eq!(dump("bool $b", "filter_var($b)"), "dumped type: ''|'1'");
}

#[test]
fn filter_var_declines_a_true_false_outcome_rather_than_widening() {
    // `int|false` has no `Fact`; a widened `int|bool` would claim `true` is
    // possible. Every one of these is the `T|false` half, and every one stays
    // `unknown` until issue #600 gives the domain a spelling for it.
    for filter in [
        "FILTER_VALIDATE_INT",
        "FILTER_VALIDATE_FLOAT",
        "FILTER_VALIDATE_EMAIL",
        "FILTER_VALIDATE_URL",
        "FILTER_VALIDATE_IP",
        "FILTER_VALIDATE_MAC",
        "FILTER_VALIDATE_DOMAIN",
    ] {
        assert_eq!(
            dump("$m", &format!("filter_var($m, {filter})")),
            "dumped type: unknown",
            "{filter}"
        );
    }
}

#[test]
fn filter_var_declines_a_float_input_under_validate_float() {
    // NOT an omission: `filter_var(NAN, FILTER_VALIDATE_FLOAT)` is `false` (so is
    // `INF`, so is `-INF`), and `-0.0` comes back `+0.0` — the value is coerced
    // to a string first. Upstream's fixture asserts a flat `float` here; the
    // probe refutes it, so the row is deliberately not won.
    assert_eq!(dump("float $f", "filter_var($f, FILTER_VALIDATE_FLOAT)"), "dumped type: unknown");
    assert_eq!(
        dump("float $f", "filter_var($f, FILTER_VALIDATE_FLOAT, FILTER_NULL_ON_FAILURE)"),
        "dumped type: float|null"
    );
}

#[test]
fn filter_var_declines_an_array_input_as_a_success_proof() {
    // `filter_var([1, 2], FILTER_SANITIZE_EMAIL)` is `false` — an array is the one
    // input class the sanitizer row has to rule out, and a fully-known array is a
    // `Singleton(Val::Array(…))`, not only a `Fact::Shape`. Both spellings decline.
    let src = "<?php\nfunction f(): void { $a = [1, 2]; \\PHPStan\\dumpType(filter_var($a, FILTER_SANITIZE_EMAIL)); }\n";
    assert_eq!(one_type(src), "dumped type: unknown");
    assert_eq!(dump("array $a", "filter_var($a, FILTER_SANITIZE_EMAIL)"), "dumped type: unknown");
    assert_eq!(dump("array $a", "filter_var($a, FILTER_DEFAULT)"), "dumped type: unknown");
    // Under the flag the answer is the general one, which covers the `null` an
    // array input really produces.
    let src = "<?php\nfunction f(): void { $a = [1, 2]; \\PHPStan\\dumpType(filter_var($a, FILTER_SANITIZE_EMAIL, FILTER_NULL_ON_FAILURE)); }\n";
    assert_eq!(one_type(src), "dumped type: string|null");
}

#[test]
fn filter_var_declines_a_nullable_input_as_a_success_proof() {
    // `filter_var(null, FILTER_VALIDATE_INT)` is `false`, so a `?int` proves
    // nothing — the failure arm stands and `int|false` has no spelling.
    assert_eq!(dump("?int $i", "filter_var($i, FILTER_VALIDATE_INT)"), "dumped type: unknown");
    assert_eq!(
        dump("?int $i", "filter_var($i, FILTER_VALIDATE_INT, FILTER_NULL_ON_FAILURE)"),
        "dumped type: int|null"
    );
}

#[test]
fn filter_var_declines_the_array_flags_on_an_input_that_may_be_an_array() {
    // Issue #615 leg (a)'s load-bearing decline. Under either array flag the
    // engine walks an array input RECURSIVELY — `filter_var([[1]],
    // FILTER_VALIDATE_INT, ['flags' => FILTER_FORCE_ARRAY])` is `[[1]]`, not
    // `[false]` — so for an input that may itself be an array the element fact
    // would have to admit arrays at unbounded depth. No `Fact` spells that.
    for flag in ["FILTER_REQUIRE_ARRAY", "FILTER_FORCE_ARRAY"] {
        assert_eq!(
            dump("$m", &format!("filter_var($m, FILTER_VALIDATE_INT, {flag})")),
            "dumped type: unknown",
            "{flag}"
        );
        assert_eq!(
            dump("$m", &format!("filter_var($m, FILTER_VALIDATE_BOOL, ['flags' => {flag}])")),
            "dumped type: unknown",
            "{flag}"
        );
        // An `array<string, mixed>` map is the same decline for the same reason:
        // its slots are `mixed`, so they may be arrays. Upstream asserts a flat
        // `array<string, int|false>` here and is unsound (issue #40 / #594
        // precedent — the measurement wins).
        assert_eq!(
            dump_doc(
                "@param array<string, mixed> $map",
                "array $map",
                &format!("filter_var($map, FILTER_VALIDATE_INT, {flag})")
            ),
            "dumped type: unknown",
            "{flag}"
        );
    }
    // `FILTER_REQUIRE_SCALAR` stays outside the roster in every position: it is a
    // validity claim about the input, and measurably not a no-op
    // (`FILTER_REQUIRE_SCALAR|FILTER_FORCE_ARRAY` over `17` is `[17]`).
    assert_eq!(
        dump("$m", "filter_var($m, FILTER_VALIDATE_INT, FILTER_REQUIRE_SCALAR)"),
        "dumped type: unknown"
    );
    assert_eq!(
        dump("int $i", "filter_var($i, FILTER_VALIDATE_INT, FILTER_REQUIRE_SCALAR)"),
        "dumped type: unknown"
    );
}

#[test]
fn filter_var_force_array_wraps_the_scalar_answer_over_a_proven_non_array() {
    // Probed at `PINNED_PHP`: `filter_var(17, FILTER_VALIDATE_INT, ['flags' =>
    // FILTER_FORCE_ARRAY])` is `[0 => 17]`. The wrapping never fails, so there is
    // no outer failure arm — `array<17>`, not `array<17>|false`.
    let src = "<?php\nfunction f(): void { \\PHPStan\\dumpType(filter_var(17, FILTER_VALIDATE_INT, ['flags' => FILTER_FORCE_ARRAY])); }\n";
    assert_eq!(one_type(src), "dumped type: array<17>");
    // A `Fact::General` input proves non-array just as well as a Singleton does.
    assert_eq!(
        dump("int $i", "filter_var($i, FILTER_VALIDATE_INT, FILTER_FORCE_ARRAY)"),
        "dumped type: array<int>"
    );
    assert_eq!(
        dump("bool $b", "filter_var($b, FILTER_VALIDATE_BOOL, FILTER_FORCE_ARRAY)"),
        "dumped type: array<bool>"
    );
    assert_eq!(
        dump("string $s", "filter_var($s, FILTER_SANITIZE_EMAIL, FILTER_FORCE_ARRAY)"),
        "dumped type: array<string>"
    );
}

#[test]
fn filter_var_force_array_element_declines_exactly_where_the_scalar_rung_does() {
    // The wrapping is the ONLY new part: an element outcome the domain cannot
    // spell declines the WHOLE call, never a partial answer and never a widening.
    // `string|false` has no `Fact` (issue #600), so neither has `array<string|false>`.
    assert_eq!(
        dump("string $s", "filter_var($s, FILTER_VALIDATE_INT, FILTER_FORCE_ARRAY)"),
        "dumped type: unknown"
    );
    // …and where the scalar rung answers under `FILTER_NULL_ON_FAILURE`, so does
    // the wrapped one. `bool|false` IS `bool`, the one union with a `Fact`.
    assert_eq!(
        dump("string $s", "filter_var($s, FILTER_VALIDATE_BOOL, FILTER_FORCE_ARRAY)"),
        "dumped type: array<bool>"
    );
}

#[test]
fn filter_var_require_array_on_a_proven_non_array_is_the_failure_value_alone() {
    // `filter_var(17, FILTER_VALIDATE_INT, ['flags' => FILTER_REQUIRE_ARRAY])` is
    // `false` at `PINNED_PHP`: a proven non-array input can never satisfy the
    // flag, so the call has NO success arm and no array is involved at all.
    let src = "<?php\nfunction f(): void { \\PHPStan\\dumpType(filter_var(17, FILTER_VALIDATE_INT, ['flags' => FILTER_REQUIRE_ARRAY])); }\n";
    assert_eq!(one_type(src), "dumped type: false");
    // The filter and the input stop mattering once the flag has decided.
    assert_eq!(
        dump("string $s", "filter_var($s, FILTER_VALIDATE_INT, FILTER_REQUIRE_ARRAY)"),
        "dumped type: false"
    );
    assert_eq!(
        dump("float $f", "filter_var($f, FILTER_VALIDATE_FLOAT, FILTER_REQUIRE_ARRAY)"),
        "dumped type: false"
    );
}

#[test]
fn filter_var_a_fully_literal_array_input_is_a_singleton_not_a_shape() {
    // The trap issue #615 names: a fully-literal array binds
    // `Fact::Singleton(Val::Array(…))`, NOT `Fact::Shape`, so a resolver that
    // matches only `Shape` would silently take the proven-non-array branch and
    // answer `false` for a call that really maps. `fact_denotes_no_array` asks
    // the values, so BOTH spellings decline here.
    let lit = "<?php\nfunction f(): void { $a = [1, 2]; \\PHPStan\\dumpType(filter_var($a, FILTER_VALIDATE_INT, FILTER_REQUIRE_ARRAY)); }\n";
    assert_eq!(one_type(lit), "dumped type: unknown");
    let lit = "<?php\nfunction f(): void { $a = [1, 2]; \\PHPStan\\dumpType(filter_var($a, FILTER_VALIDATE_INT, FILTER_FORCE_ARRAY)); }\n";
    assert_eq!(one_type(lit), "dumped type: unknown");
    // The `Fact::Shape` spelling of the same input, for the same answer.
    assert_eq!(
        dump("array $a", "filter_var($a, FILTER_VALIDATE_INT, FILTER_REQUIRE_ARRAY)"),
        "dumped type: unknown"
    );
    assert_eq!(
        dump("array $a", "filter_var($a, FILTER_VALIDATE_INT, FILTER_FORCE_ARRAY)"),
        "dumped type: unknown"
    );
}

#[test]
fn filter_var_declines_the_string_modifying_and_unmodeled_flags() {
    // These rewrite the SUCCESS value (`FILTER_DEFAULT` stops being the
    // identity), turn `''` into `null` on the success path, or delete the
    // failure arm through an exception this rung has no PHP-minor gate for.
    for flag in [
        "FILTER_FLAG_STRIP_LOW",
        "FILTER_FLAG_STRIP_HIGH",
        "FILTER_FLAG_STRIP_BACKTICK",
        "FILTER_FLAG_ENCODE_LOW",
        "FILTER_FLAG_ENCODE_HIGH",
        "FILTER_FLAG_ENCODE_AMP",
        "FILTER_FLAG_NO_ENCODE_QUOTES",
        "FILTER_FLAG_EMPTY_STRING_NULL",
        "FILTER_THROW_ON_FAILURE",
    ] {
        assert_eq!(
            dump("string $s", &format!("filter_var($s, FILTER_DEFAULT, {flag})")),
            "dumped type: unknown",
            "{flag}"
        );
    }
}

#[test]
fn filter_var_accepts_the_type_neutral_restricting_flags() {
    // These only restrict which inputs validate; measured no-ops for every cell
    // of the grid, so the answer is the one the flag-less call gives.
    for flag in [
        "FILTER_FLAG_NONE",
        "FILTER_FLAG_ALLOW_OCTAL",
        "FILTER_FLAG_ALLOW_HEX",
        "FILTER_FLAG_IPV4",
        "FILTER_FLAG_IPV6",
        "FILTER_FLAG_HOSTNAME",
        "FILTER_FLAG_EMAIL_UNICODE",
        "FILTER_FLAG_NO_PRIV_RANGE",
        "FILTER_FLAG_NO_RES_RANGE",
        "FILTER_FLAG_GLOBAL_RANGE",
        "FILTER_FLAG_PATH_REQUIRED",
        "FILTER_FLAG_QUERY_REQUIRED",
    ] {
        assert_eq!(
            dump("$m", &format!("filter_var($m, FILTER_VALIDATE_BOOL, {flag})")),
            "dumped type: bool",
            "{flag}"
        );
        assert_eq!(
            dump("int $i", &format!("filter_var($i, FILTER_VALIDATE_INT, {flag})")),
            "dumped type: int",
            "{flag}"
        );
    }
    // A literal `0` is the documented "no flags".
    assert_eq!(dump("$m", "filter_var($m, FILTER_VALIDATE_BOOL, 0)"), "dumped type: bool");
}

#[test]
fn filter_var_declines_an_unreadable_flags_argument() {
    // A variable carries no proven value (issue #168) — `filterVar.php` spends a
    // row per filter block on `$nullFilter = \FILTER_NULL_ON_FAILURE`.
    assert_eq!(
        dump("$m, int $flags", "filter_var($m, FILTER_VALIDATE_INT, $flags)"),
        "dumped type: unknown"
    );
    // A `|` combination lowers to `Other`: the value lane represents comparisons
    // only, so there is no bit-set to read.
    assert_eq!(
        dump(
            "$m",
            "filter_var($m, FILTER_VALIDATE_INT, FILTER_NULL_ON_FAILURE | FILTER_FLAG_IPV4)"
        ),
        "dumped type: unknown"
    );
    // A bare non-zero int is not a recognized NAME.
    assert_eq!(dump("$m", "filter_var($m, FILTER_VALIDATE_INT, 134217728)"), "dumped type: unknown");
    // An unrecognized flag constant declines like any other unreadable one.
    assert_eq!(dump("$m", "filter_var($m, FILTER_VALIDATE_INT, PHP_EOL)"), "dumped type: unknown");
}

#[test]
fn filter_var_declines_an_options_array_carrying_any_other_key() {
    // `'default'` REPLACES the failure value with an arbitrary one; `min_range`
    // narrows the success arm. One unrecognized key declines the whole literal.
    for options in [
        "['options' => ['default' => 0]]",
        "['options' => ['min_range' => 1], 'flags' => FILTER_NULL_ON_FAILURE]",
        "['flags' => FILTER_NULL_ON_FAILURE, 'options' => ['default' => 0]]",
        "[FILTER_NULL_ON_FAILURE]",
    ] {
        assert_eq!(
            dump("$m", &format!("filter_var($m, FILTER_VALIDATE_INT, {options})")),
            "dumped type: unknown",
            "{options}"
        );
    }
}

#[test]
fn filter_var_declines_an_unreadable_or_unrecognized_filter() {
    // A dynamic filter has no name to key on.
    assert_eq!(dump("$m, int $f", "filter_var($m, $f)"), "dumped type: unknown");
    assert_eq!(dump("$m", "filter_var($m, 257)"), "dumped type: unknown");
    // `FILTER_CALLBACK` returns whatever a userland callback returns.
    assert_eq!(
        dump("$m", "filter_var($m, FILTER_CALLBACK, FILTER_NULL_ON_FAILURE)"),
        "dumped type: unknown"
    );
    // `FILTER_VALIDATE_REGEXP` needs a `'regexp'` option, and an options array is
    // itself a decline — so every call this rung could answer raises
    // `ValueError: filter_var(): "regexp" option is missing` at 8.5.9.
    assert_eq!(
        dump("$m", "filter_var($m, FILTER_VALIDATE_REGEXP, FILTER_NULL_ON_FAILURE)"),
        "dumped type: unknown"
    );
    // A `Qualified`/`Relative` spelling never denotes the global constant.
    assert_eq!(
        dump("$m", "filter_var($m, Foo\\FILTER_VALIDATE_INT, FILTER_NULL_ON_FAILURE)"),
        "dumped type: unknown"
    );
}

#[test]
fn filter_var_is_the_only_name_in_the_family_this_rung_answers() {
    // `filter_var_array` and `filter_input*` answer arrays; they are out of the
    // rung's scope by construction, not by omission. Whatever each name's own
    // floor already said is what it still says — this rung adds nothing, and
    // above all never the `int|null` the `filter_var` spelling earns.
    for call in [
        "filter_var_array($m, FILTER_VALIDATE_INT, FILTER_NULL_ON_FAILURE)",
        "filter_input(INPUT_GET, 'k', FILTER_VALIDATE_INT, FILTER_NULL_ON_FAILURE)",
        "filter_input_array(INPUT_GET, FILTER_VALIDATE_INT, FILTER_NULL_ON_FAILURE)",
    ] {
        assert_ne!(dump("$m", call), "dumped type: int|null", "{call}");
    }
}

#[test]
fn filter_vars_declaration_and_arity_gates_still_govern() {
    let src = "<?php\nfunction f(string $s): void { \\PHPStan\\dumpType(filter_var($s, FILTER_VALIDATE_INT, FILTER_NULL_ON_FAILURE)); }\n";
    assert_eq!(one_type_with(src, &mut Mock::sidecar()), "dumped type: int|null");
    // A declaration that is no longer the bare `mixed` the rule was written
    // against withholds it.
    let mut mock = Mock::sidecar();
    mock.types.insert("filter_var".to_owned(), "string|false".to_owned());
    assert_eq!(one_type_with(src, &mut mock), "dumped type: unknown");
    // So does a MOVED signature: the rule reads all three arguments positionally
    // (ADR-0064 Amendment B's second leg, the only countersignature a `mixed`
    // declaration leaves).
    let mut mock = Mock::sidecar();
    mock.arities.insert("filter_var".to_owned(), (4, 1));
    assert_eq!(one_type_with(src, &mut mock), "dumped type: unknown");
    // And an absent sidecar withholds it outright.
    let mut mock = Mock::sidecar();
    mock.types.remove("filter_var");
    assert_eq!(one_type_with(src, &mut mock), "dumped type: unknown");
}

/// The A9 monkey-patch leg: a project function sharing the simple name is what
/// the call resolves to, so the builtin's grid says nothing about it.
#[test]
fn a_project_function_named_filter_var_shadows_the_rule() {
    let src = "<?php\nfunction filter_var($v, $f = null, $o = null) { return 1; }\n\
               function f(string $s): void { \\PHPStan\\dumpType(filter_var($s, FILTER_VALIDATE_INT, FILTER_NULL_ON_FAILURE)); }\n";
    assert_ne!(one_type_with(src, &mut Mock::sidecar()), "dumped type: int|null");
}
