//! Calls nested 200 deep walk in polynomial time (issue #1063).
//!
//! A call's argument was resolved once by each road that reads it — the fold,
//! the transfer rung and its fact reads — and each of those resolved the
//! argument's own argument the same way, so `strlen(strlen(…))` cost `3^n` and
//! a project call `2^n`. In value position, a method call or `new` also lowered
//! its argument list twice per level. At depth 200 either is out of reach, so
//! each test runs the check on its own thread and fails on a deadline instead of
//! hanging the suite.

use std::sync::mpsc;
use std::time::Duration;

use steins_infer::{NoFold, check_with};
use steins_syntax::SourceTree;

const DEPTH: usize = 200;

/// Far above what a polynomial walk of these files costs in a debug build, and
/// far below a walk exponential in [`DEPTH`].
const DEADLINE: Duration = Duration::from_secs(120);

/// `open` × [`DEPTH`], then `inner`, then `close` × [`DEPTH`].
fn nest(open: &str, inner: &str, close: &str) -> String {
    format!("{}{inner}{}", open.repeat(DEPTH), close.repeat(DEPTH))
}

/// The `debug.type` answers of `src` as `line: type`, computed on a thread of
/// its own with the binary's stack and failed on [`DEADLINE`].
fn dumps_within_deadline(src: String) -> Vec<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(move || {
            let tree = SourceTree::parse(&src);
            let dumps: Vec<String> = check_with(&tree, &[], "t.php", &mut NoFold)
                .into_iter()
                .filter(|d| d.id == "debug.type")
                .map(|d| {
                    let answer = d.message.split_once("dumped type: ").map_or("?", |(_, a)| a);
                    format!("{}: {answer}", d.line)
                })
                .collect();
            let _ = tx.send(dumps);
        })
        .expect("spawn the check thread");
    rx.recv_timeout(DEADLINE).expect("a call nested 200 deep must walk in polynomial time")
}

#[test]
fn a_builtin_call_nested_200_deep_walks_in_polynomial_time() {
    let src = format!("<?php\n\\PHPStan\\dumpType({});\n", nest("strlen(", "'x'", ")"));
    assert_eq!(dumps_within_deadline(src), vec!["2: int<0, max> (asserted)"]);
}

#[test]
fn a_project_function_call_nested_200_deep_walks_in_polynomial_time() {
    let src = format!(
        "<?php\nfunction g($a) {{ return $a; }}\n\\PHPStan\\dumpType({});\n",
        nest("g(", "1", ")")
    );
    assert_eq!(dumps_within_deadline(src), vec!["3: 1"]);
}

#[test]
fn a_method_call_nested_200_deep_walks_in_polynomial_time() {
    let src = format!(
        "<?php\nclass C {{ public function m($a) {{ return $a; }} }}\n$o = new C();\n\\PHPStan\\dumpType({});\n",
        nest("$o->m(", "1", ")")
    );
    assert_eq!(dumps_within_deadline(src), vec!["4: 1"]);
}

#[test]
fn a_static_call_nested_200_deep_walks_in_polynomial_time() {
    let src = format!(
        "<?php\nclass C {{ public static function m($a) {{ return $a; }} }}\n\\PHPStan\\dumpType({});\n",
        nest("C::m(", "1", ")")
    );
    assert_eq!(dumps_within_deadline(src), vec!["3: 1"]);
}
