//! Oracle tests: `php_is_numeric`, casing predicates, and `php_str_is_decimal_int` agree
//! with the engine's `is_numeric()`, case-identity, and array-key cast. Skips without `php`.

use std::process::Command;

const CASES: &[&str] = &[
    "", "0", "5", "-5", "+5", "5.", ".5", "5.5", "1e3", "1E+3", "5.e3", ".5e2", "007", "00",
    " 5", "5 ", " 5 ", "\t5\n", "abc", "0x1A", "0b101", "0o17", "1_000", "5,5", "++5", "--5",
    "5e", "e5", "5e+", ".", "-", "+", "-.", "1.2.3", "NAN", "INF", "-INF", "nan", "inf",
    "0.0", "-0", "-0.0", "1e308", "1e-308", "9223372036854775807", "9223372036854775808",
];

#[test]
fn is_numeric_matches_the_engine() {
    let probe = Command::new("php").arg("--version").output();
    if probe.is_err() {
        eprintln!("SKIP: php not on PATH; oracle comparison not run");
        return;
    }

    let script = r#"
        $cases = json_decode(stream_get_contents(STDIN), true, 512, JSON_THROW_ON_ERROR);
        foreach ($cases as $c) { echo is_numeric($c) ? "1\n" : "0\n"; }
    "#;
    let mut child = Command::new("php")
        .args(["-d", "display_errors=stderr", "-r", script])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn php");
    {
        use std::io::Write;
        let json: Vec<String> = CASES.iter().map(|c| {
            let escaped: String = c.chars().flat_map(char::escape_default).collect();
            format!("\"{escaped}\"")
        }).collect();
        let payload = format!("[{}]", json.join(","));
        child.stdin.take().expect("stdin").write_all(payload.as_bytes()).expect("write");
    }
    let out = child.wait_with_output().expect("php run");
    assert!(out.status.success(), "php failed: {}", String::from_utf8_lossy(&out.stderr));
    let answers: Vec<&str> = std::str::from_utf8(&out.stdout).expect("utf8").lines().collect();
    assert_eq!(answers.len(), CASES.len(), "answer count mismatch");

    for (case, answer) in CASES.iter().zip(answers) {
        let engine = answer == "1";
        let ours = steins_domain::php_is_numeric(case);
        assert_eq!(
            ours, engine,
            "is_numeric disagreement on {case:?}: engine={engine}, ours={ours}"
        );
    }
}

/// Includes multibyte cases: UTF-8 `"Ä"` has no `A-Z` byte, so `strtolower()`
/// leaves it alone — it *is* a `lowercase-string` under the byte-oriented rule.
const CASING_CASES: &[&str] = &[
    "", "abc", "ABC", "abC", "ABc", "123", "1e5", "1E5", "snake_case", "SCREAMING_CASE",
    "camelCase", "-", " ", "a1", "A1", "0", "Ä", "ä", "Ärger", "ärger", "日本語", "ABCä",
];

/// JSON-escape a PHP string (Rust's `char::escape_default` isn't JSON; need `\uXXXX`).
fn json_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_ascii_graphic() || c == ' ' => out.push(c),
            c => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    out.push('"');
    out
}

#[test]
fn casing_predicates_match_the_engine() {
    let probe = Command::new("php").arg("--version").output();
    if probe.is_err() {
        eprintln!("SKIP: php not on PATH; oracle comparison not run");
        return;
    }

    let script = r#"
        $cases = json_decode(stream_get_contents(STDIN), true, 512, JSON_THROW_ON_ERROR);
        foreach ($cases as $c) {
            echo (strtolower($c) === $c ? "1" : "0"), (strtoupper($c) === $c ? "1" : "0"), "\n";
        }
    "#;
    let mut child = Command::new("php")
        .args(["-d", "display_errors=stderr", "-r", script])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn php");
    {
        use std::io::Write;
        let json: Vec<String> = CASING_CASES.iter().map(|c| json_string(c)).collect();
        let payload = format!("[{}]", json.join(","));
        child.stdin.take().expect("stdin").write_all(payload.as_bytes()).expect("write");
    }
    let out = child.wait_with_output().expect("php run");
    assert!(out.status.success(), "php failed: {}", String::from_utf8_lossy(&out.stderr));
    let answers: Vec<&str> = std::str::from_utf8(&out.stdout).expect("utf8").lines().collect();
    assert_eq!(answers.len(), CASING_CASES.len(), "answer count mismatch");

    for (case, answer) in CASING_CASES.iter().zip(answers) {
        let engine_lower = answer.starts_with('1');
        let engine_upper = answer.ends_with('1');
        assert_eq!(
            steins_domain::php_str_is_lowercase(case),
            engine_lower,
            "strtolower identity disagreement on {case:?}"
        );
        assert_eq!(
            steins_domain::php_str_is_uppercase(case),
            engine_upper,
            "strtoupper identity disagreement on {case:?}"
        );
    }
}

/// `decimal-int-string` cases: numeric strings that aren't canonical (`"007"`,
/// `"+1"`, `"00"`, `"-0"`), plus int-range edges — `PHP_INT_MAX` casts, one past
/// it doesn't, `PHP_INT_MIN` does.
const DECIMAL_INT_CASES: &[&str] = &[
    "", "0", "-0", "1", "-1", "007", "-007", "00", "+1", "+0", "1234", "-1234", "1.2", "0.0",
    "18E+3", "1e5", "1E5", " 1", "1 ", " 1 ", "\t1", "1,3", "foo", "abc", "-", "--1", "0x1A",
    "0b1", "1_000", "9223372036854775807", "9223372036854775808", "-9223372036854775808",
    "-9223372036854775809", "10000000000000000000", "01", "0777",
];

/// Definition, not a proxy: insert as an array key and check whether it came
/// back an `int` — that *is* `decimal-int-string`.
#[test]
fn decimal_int_string_matches_the_array_key_cast() {
    let probe = Command::new("php").arg("--version").output();
    if probe.is_err() {
        eprintln!("SKIP: php not on PATH; oracle comparison not run");
        return;
    }

    let script = r#"
        $cases = json_decode(stream_get_contents(STDIN), true, 512, JSON_THROW_ON_ERROR);
        foreach ($cases as $c) {
            $a = [];
            $a[$c] = 1;
            echo is_int(array_key_first($a)) ? "1\n" : "0\n";
        }
    "#;
    let mut child = Command::new("php")
        .args(["-d", "display_errors=stderr", "-r", script])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn php");
    {
        use std::io::Write;
        let json: Vec<String> = DECIMAL_INT_CASES.iter().map(|c| json_string(c)).collect();
        let payload = format!("[{}]", json.join(","));
        child.stdin.take().expect("stdin").write_all(payload.as_bytes()).expect("write");
    }
    let out = child.wait_with_output().expect("php run");
    assert!(out.status.success(), "php failed: {}", String::from_utf8_lossy(&out.stderr));
    let answers: Vec<&str> = std::str::from_utf8(&out.stdout).expect("utf8").lines().collect();
    assert_eq!(answers.len(), DECIMAL_INT_CASES.len(), "answer count mismatch");

    for (case, answer) in DECIMAL_INT_CASES.iter().zip(answers) {
        let engine = answer == "1";
        let ours = steins_domain::php_str_is_decimal_int(case);
        assert_eq!(
            ours, engine,
            "array-key cast disagreement on {case:?}: engine={engine}, ours={ours}"
        );
    }
}
