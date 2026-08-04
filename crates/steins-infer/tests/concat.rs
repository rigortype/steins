//! String concatenation as a value (issue #59).
//!
//! `.` is the one binary operator lowered to an [`ArgValue`], because for the
//! operand types admitted it is *total and environment-independent* — byte
//! concatenation consults no locale, encoding or ini setting. That is what lets the
//! result be derived in Rust rather than asked of the sidecar, and it is why these
//! fixtures run on the pure `check` path (== `NoFold`): concatenation is proven in
//! the browser too, unlike anything on the `foldable` allowlist.
//!
//! The float exclusion is the load-bearing negative. PHP's float-to-string follows
//! the `precision` ini directive, so a folded `"" . 0.1` would be a value that
//! depends on the runtime's configuration — exactly what this crate must not
//! invent. `oracle_agrees_on_every_admitted_cast` pins the admitted cells against
//! the real engine, and `float_operand_widens` pins the refusal.

use std::process::Command;

use steins_infer::{DEBUG_TYPE_ID, Diagnostic, Folder, check, check_with};
use steins_syntax::{ArgValue, SourceTree};

/// A canned folder for the two allowlisted builtins the flagship needs. The real
/// fold runs on the project's PHP; the mock only has to prove a concatenation
/// **reaches** the gate as a resolved literal.
struct Mock;

impl Folder for Mock {
    fn fold(&mut self, name: &str, args: &[ArgValue]) -> Option<ArgValue> {
        match (name, args) {
            ("strtoupper", [ArgValue::Str(s)]) => Some(ArgValue::Str(s.to_uppercase())),
            ("str_repeat", [ArgValue::Str(s), ArgValue::Int(n)]) => {
                Some(ArgValue::Str(s.repeat(usize::try_from(*n).ok()?)))
            }
            _ => None,
        }
    }
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
    // The reported snippet. Every link in the chain participates: the literal
    // arguments seed `greet`'s parameters, `"Hello, " . $name . "! "` resolves
    // against that env, the resolved string passes the fold gate as `str_repeat`'s
    // subject, and the folded result crosses the return boundary.
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
    // The gate's admission is `is_concrete_value` over the RESOLVED argument, so a
    // concatenation qualifies exactly when it resolves. This is the link that was
    // missing: before, `"ab" . "cd"` was `Other` and closed the gate.
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
    // The whole reason `.` lowers structurally instead of folding at lowering time:
    // `$n`'s value is an env fact, known only during the walk.
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
    assert_eq!(dumped(r#""f=" . 1.5"#), "unknown");
    assert_eq!(dumped(r#""f=" . 0.1"#), "unknown");
    // A float by *promotion* rather than by spelling (issue #62) takes the same
    // refusal — the admission rule reads the value, not how it was written.
    assert_eq!(dumped(r#""f=" . 9223372036854775808"#), "unknown");
}

#[test]
fn an_unresolved_operand_widens_the_whole_concat() {
    // No partial strings: one unknown operand and the result is unknown, not a
    // prefix. `rand()` is not foldable and `$u` is never bound.
    assert_eq!(dumped(r#""u=" . rand()"#), "unknown");
    let src = "<?php\n$y = \"a\" . $undefined;\n\\PHPStan\\dumpType($y);\n";
    assert_eq!(types(src), vec!["unknown"]);
}

#[test]
fn array_operand_widens() {
    // PHP yields "Array" plus a warning; that is a diagnosis, not a value to fold.
    assert_eq!(dumped(r#""a=" . [1, 2]"#), "unknown");
}

#[test]
fn compound_concat_assign_is_still_unproven() {
    // `.=` lowers its rvalue to `Other` (see `StmtKind`); this negative pin keeps
    // unsupported compound assignment from being treated as plain concatenation.
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
    // The report's LITERAL form (issue #60 closing the loop #59 opened): the call
    // dumped directly, no assignment detour. Identical to
    // `flagship_greet_inlines_to_its_value` in every way but the position.
    let src = "<?php\n\
        function greet(int $times, string $name): string {\n\
            return str_repeat(\"Hello, \" . $name . \"! \", $times);\n\
        }\n\
        \\PHPStan\\dumpType(greet(2, \"World\"));\n";
    assert_eq!(one_folded(src), "'Hello, World! Hello, World! '");
}
