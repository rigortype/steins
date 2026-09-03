//! String concatenation as a value (issue #59).
//!
//! `.` is the one binary operator lowered to an [`ArgValue`], because for the
//! operand types admitted it is *total and environment-independent* — byte
//! concatenation consults no locale, encoding or ini setting. That lets the
//! result be derived in Rust rather than asked of the sidecar, and is why
//! these fixtures run on the pure `check` path (== `NoFold`): concatenation
//! is proven in the browser too, unlike anything on the `foldable` allowlist.
//!
//! The float exclusion is the load-bearing negative: PHP's float-to-string
//! follows the `precision` ini directive, so a folded `"" . 0.1` would depend
//! on runtime configuration — exactly what this crate must not invent.
//! `oracle_agrees_on_every_admitted_cast` pins the admitted cells against the
//! real engine; `float_operand_widens` pins the refusal — **through
//! [`strlen_of`], not through the dump surface**. See below for why the
//! distinction is the whole point.
//!
//! # What "widens" means since issue #627, and which lane still asks `concat_cast`
//!
//! Every refusal below used to render `unknown`, because the literal seam was
//! the only reader of a `Concat`. It is no longer: `eval_concat_fact` answers
//! the predicate table and, failing that, the `string` floor. The refusals hold
//! in the only sense that matters — **no declined value is ever stated** — so
//! each one asserts the widened *fact* as well.
//!
//! **But a dump of a `Concat` no longer reaches `concat_cast` at all.**
//! `dump.rs` matches `ArgValue::Concat` *above* `Cx::resolve_literal_strat`, and
//! `eval_concat_fact` projects each operand through `php_cast_fact`, which
//! declines a float and an array by itself. So an assertion like
//! `dumped("\"f=\" . 1.5") == "non-falsy-string"` pins the **cast grid's** float
//! row and stays green even if `concat_cast` starts spelling floats. That was
//! measured, not reasoned: adding `ArgValue::Float(f) => Some(format!("{f}"))`
//! to `concat_cast` left this whole file green while
//! `dumpType(strlen("f=" . 1e100))` rendered `103` against PHP's `10`.
//!
//! `concat_cast` now lives only in the **literal lane** — argument position,
//! `switch`/`match` subjects, `in_array` — which still resolves through
//! `Cx::resolve_literal`. [`strlen_of`] is how these fixtures observe it, and
//! every refusal carries one.

use std::process::Command;

use steins_infer::{DEBUG_TYPE_ID, Diagnostic, Folder, check, check_with};
use steins_syntax::{ArgValue, SourceTree};

/// A canned folder for the allowlisted builtins these fixtures need. The real
/// fold runs on the project's PHP; the mock only has to prove a concatenation
/// **reaches** the gate as a resolved literal.
///
/// `strlen` is the observer the refusals below need. It folds only when its
/// argument arrives as an `ArgValue::Str`, so a concatenation that `concat_cast`
/// declined never reaches it and the call falls back to `strlen`'s own contract
/// (`int<0, max>`) — which makes that refinement versus a bare integer the
/// visible difference between the refusal holding and leaking.
struct Mock;

impl Folder for Mock {
    fn fold(&mut self, name: &str, args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
        match (name, args) {
            ("strtoupper", [ArgValue::Str(s)]) => Some(ArgValue::Str(s.as_str()?.to_uppercase().into())),
            ("str_repeat", [ArgValue::Str(s), ArgValue::Int(n)]) => {
                Some(ArgValue::Str(s.as_str()?.repeat(usize::try_from(*n).ok()?).into()))
            }
            ("strlen", [ArgValue::Str(s)]) => {
                Some(ArgValue::Int(i64::try_from(s.as_bytes().len()).ok()?))
            }
            _ => None,
        }
    }
    fn builtin_return_type(&mut self, name: &str) -> Option<String> {
        match name.to_ascii_lowercase().as_str() {
            "strlen" => Some("int".to_owned()),
            "strtoupper" | "str_repeat" => Some("string".to_owned()),
            _ => None,
        }
    }
    fn builtin_param_counts(&mut self, name: &str) -> Option<(u32, u32)> {
        // `(total, required)` — the order that is a silent decline when reversed.
        match name.to_ascii_lowercase().as_str() {
            "strlen" | "strtoupper" => Some((1, 1)),
            "str_repeat" => Some((2, 2)),
            _ => None,
        }
    }
}

/// `dumpType(strlen(<expr>));` — the **literal lane**, which is the only lane
/// `concat_cast` still sits in (issue #627 review).
///
/// The dump surface stopped reaching `concat_cast` when `eval_concat_fact` was
/// wired above `resolve_literal_strat` in `dump.rs`: the fact seam declines a
/// float or array operand by itself, through `php_cast_fact`. So a refusal
/// asserted on the dump surface pins the *cast grid's* row and says nothing
/// about `concat_cast`. Argument position still resolves through
/// `Cx::resolve_literal`, so `strlen` observes the fold that did or did not
/// happen: `int<0, max> (asserted)` — the declared contract, unfolded — is the
/// refusal holding, and a bare integer is it leaking.
fn strlen_of(expr: &str) -> String {
    one_folded(&format!("<?php\n\\PHPStan\\dumpType(strlen({expr}));\n"))
}

fn findings(src: &str, folder: Option<&mut dyn Folder>) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    match folder {
        Some(f) => check_with(&tree, &functions, "test.php", f),
        None => check(&tree, &functions, "test.php"),
    }
}

/// Every `debug.type` body in `src`, in source order, without a folder (`NoFold`).
fn types(src: &str) -> Vec<String> {
    findings(src, None)
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message.replace("dumped type: ", ""))
        .collect()
}

/// The single `debug.type` body, with the mock folder in place.
fn one_folded(src: &str) -> String {
    let ds: Vec<Diagnostic> = findings(src, Some(&mut Mock))
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .collect();
    assert_eq!(ds.len(), 1, "expected exactly one debug.type dump, got {ds:?}");
    ds[0].message.replace("dumped type: ", "")
}

/// `$x = <expr>; dumpType($x);` — the assignment shape, which is the only position a
/// project call currently resolves in (issue #60); irrelevant for a bare concat, but
/// keeps every fixture here on one shape.
fn dumped(expr: &str) -> String {
    let src = format!("<?php\n$x = {expr};\n\\PHPStan\\dumpType($x);\n");
    let ts = types(&src);
    assert_eq!(ts.len(), 1, "expected one dump for `{expr}`, got {ts:?}");
    ts.into_iter().next().expect("one dump")
}

// (i) The flagship — the reported case, end to end.

#[test]
fn flagship_greet_inlines_to_its_value() {
    // The reported snippet: every link participates — literal args seed
    // `greet`'s params, the concat resolves against that env, the result
    // passes the fold gate as `str_repeat`'s subject, and crosses the return.
    let src = "<?php\n\
        function greet(int $times, string $name): string {\n\
            return str_repeat(\"Hello, \" . $name . \"! \", $times);\n\
        }\n\
        $x = greet(2, \"World\");\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_folded(src), "'Hello, World! Hello, World! '");
}

#[test]
fn a_concat_argument_reaches_the_fold_gate() {
    // The gate admits via `is_concrete_value` over the RESOLVED argument — the
    // missing link: before, `"ab" . "cd"` was `Other` and closed the gate.
    let src = "<?php\n$x = strtoupper(\"ab\" . \"cd\");\n\\PHPStan\\dumpType($x);\n";
    assert_eq!(one_folded(src), "'ABCD'");
}

// (ii) The admitted casts — proven WITHOUT a folder, i.e. in the playground too.

#[test]
fn literal_chain_folds_left_associatively() {
    assert_eq!(dumped(r#""x" . "y""#), "'xy'");
    assert_eq!(dumped(r#""a" . "b" . "c" . "d""#), "'abcd'");
    assert_eq!(dumped(r#""" . """#), "''");
}

#[test]
fn an_env_bound_operand_resolves() {
    // The whole reason `.` lowers structurally: $n is an env fact, known only during the walk.
    let src = "<?php\n\
        $n = \"World\";\n\
        $b = \"Hello, \" . $n . \"!\";\n\
        \\PHPStan\\dumpType($b);\n";
    assert_eq!(types(src), vec!["'Hello, World!'"]);
}

#[test]
fn non_string_scalars_take_their_php_cast() {
    assert_eq!(dumped(r#""n=" . 5"#), "'n=5'");
    assert_eq!(dumped(r#""n=" . -3"#), "'n=-3'");
    assert_eq!(dumped(r#""b=" . true"#), "'b=1'");
    assert_eq!(dumped(r#""b=" . false"#), "'b='");
    assert_eq!(dumped(r#""z=" . null"#), "'z='");
}

// (iii) The refusals — each one a value this crate declines to invent.

#[test]
fn float_operand_widens() {
    // NOT `'f=1.5'`. PHP's float-to-string is `precision`-dependent, so the value is
    // the runtime's to state, not ours. `strval` stays on the allowlist for this.
    // Since #627 the widening is the predicate answer rather than `unknown`; what
    // the refusal forbids is a *value*, and none of these states one.
    for src in [r#""f=" . 1.5"#, r#""f=" . 0.1"#, r#""f=" . 9223372036854775808"#] {
        // The last is a float by *promotion* rather than by spelling (issue #62)
        // and takes the same refusal — the admission rule reads the value, not
        // how it was written.
        let got = dumped(src);
        assert_eq!(got, "non-falsy-string", "{src}");
        assert!(!got.contains('\''), "a float value leaked into `{src}`: {got}");
        // **The lane that actually holds `concat_cast`.** The three assertions
        // above are answered by the cast grid one rung higher up and stay green
        // even if `concat_cast` starts admitting floats; only argument position
        // still reaches it. `int` is `strlen`'s declared return with no fold
        // behind it — any integer here means a float value was invented.
        assert_eq!(strlen_of(src), "int<0, max> (asserted)", "the literal lane folded `{src}`");
    }
    // Why the refusal exists, in one measurement: PHP prints this float under
    // the `precision` ini directive, so `php -r 'var_dump(strlen("f=" . 1e100));'`
    // is `int(10)` for `"f=1.0E+100"`. A Rust `{}` formatter writes all 101
    // digits and `strlen` would fold to 103 — right-looking and wrong.
    assert_eq!(strlen_of(r#""f=" . 1e100"#), "int<0, max> (asserted)");
}

#[test]
fn an_unresolved_operand_widens_the_whole_concat() {
    // No partial strings: one unresolved operand and the result is the operator's
    // widened fact, never the resolved prefix. `rand()` is not foldable and `$u`
    // is never bound, so `'u='` and `'a'` must not survive as values.
    assert_eq!(dumped(r#""u=" . rand()"#), "non-falsy-string");
    let src = "<?php\n$y = \"a\" . $undefined;\n\\PHPStan\\dumpType($y);\n";
    assert_eq!(types(src), vec!["non-falsy-string"]);
}

#[test]
fn array_operand_widens() {
    // PHP yields "Array" plus a warning; that is a diagnosis, not a value to fold.
    // The #627 floor says the result is a `string` — which it is — without ever
    // naming `'Array'`, because the cast grid keeps declining an array input.
    assert_eq!(dumped(r#""a=" . [1, 2]"#), "non-falsy-string");
    // And the same lane split as the float row above: argument position is the
    // only one still asking `concat_cast`, so this is the assertion that fails
    // if it starts admitting arrays.
    assert_eq!(strlen_of(r#""a=" . [1, 2]"#), "int<0, max> (asserted)");
    assert_eq!(strlen_of(r#""a=" . []"#), "int<0, max> (asserted)");
}

#[test]
fn compound_concat_assign_is_still_unproven() {
    // `.=` lowers its rvalue to `Other` (see `StmtKind`) — kept distinct from plain concat.
    let src = "<?php\n$s = \"a\";\n$s .= \"b\";\n\\PHPStan\\dumpType($s);\n";
    assert_eq!(types(src), vec!["unknown"]);
}

#[test]
fn arithmetic_is_not_lowered() {
    // Scope boundary: `.` is the only binary operator lowered as a value. `+` has
    // overflow and int/float promotion questions concatenation does not.
    assert_eq!(dumped("1 + 2"), "unknown");
}

// (iv) The oracle — the admitted casts, checked against the real engine.

/// The operand spellings the cast admits, as PHP source. Each is concatenated onto
/// `"<"` and `">"` so an empty result is still visible in the comparison.
///
/// The int bounds run to `±(2^63 - 1)`. `-9223372036854775808` is a *float* operand
/// in PHP (unary minus over a literal that overflows int, issue #62), so it belongs
/// with the refusals below, not here — `float_operand_widens` covers it.
const ADMITTED: &[&str] = &[
    r#""""#,
    r#""abc""#,
    r#""0""#,
    r#""ﾏﾙﾁﾊﾞｲﾄ""#,
    "0",
    "5",
    "-3",
    "9223372036854775807",
    "-9223372036854775807",
    "true",
    "false",
    "null",
];

#[test]
fn oracle_agrees_on_every_admitted_cast() {
    if Command::new("php").arg("--version").output().is_err() {
        eprintln!("SKIP: php not on PATH; oracle comparison not run");
        return;
    }

    // Ask the engine for `"<" . <operand> . ">"` on each admitted spelling.
    let script = ADMITTED
        .iter()
        .map(|op| format!(r#"echo "<" . {op} . ">", "\n";"#))
        .collect::<Vec<_>>()
        .join("");
    let out = Command::new("php")
        .args(["-d", "display_errors=stderr", "-r", &script])
        .output()
        .expect("run php");
    assert!(out.status.success(), "php failed: {}", String::from_utf8_lossy(&out.stderr));
    let engine: Vec<&str> = std::str::from_utf8(&out.stdout).expect("utf8").lines().collect();
    assert_eq!(engine.len(), ADMITTED.len(), "answer count mismatch");

    for (op, expected) in ADMITTED.iter().zip(engine) {
        let ours = dumped(&format!(r#""<" . {op} . ">""#));
        assert_ne!(ours, "unknown", "admitted operand `{op}` failed to resolve");
        // The dump spells a string as `'…'`; compare the payload.
        let ours = ours.trim_matches('\'');
        assert_eq!(ours, expected, "operand `{op}`: engine said {expected:?}, we said {ours:?}");
    }
}

#[test]
fn flagship_greet_inlines_in_argument_position() {
    // The report's LITERAL form (issue #60 closing the loop #59 opened): the
    // call dumped directly — identical to the other flagship but for position.
    let src = "<?php\n\
        function greet(int $times, string $name): string {\n\
            return str_repeat(\"Hello, \" . $name . \"! \", $times);\n\
        }\n\
        \\PHPStan\\dumpType(greet(2, \"World\"));\n";
    assert_eq!(one_folded(src), "'Hello, World! Hello, World! '");
}
