//! Ask-the-real-thing at the unit level: our `php_is_numeric` must agree
//! with the engine's `is_numeric()` cell for cell. Skips (loudly) when no
//! `php` binary is available.

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

    // One process for all cases: read JSON list on stdin, print 0/1 per line.
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
