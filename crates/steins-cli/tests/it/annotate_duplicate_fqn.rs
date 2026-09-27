//! End-to-end `steins annotate` over a project that declares one function FQN
//! twice (issue #825): the margin and the JSON document read the merged
//! summary, never an emptied one that claims a body calling `time()` is pure.
//!
//! The layouts are the ones the report first blamed. None of them matters on
//! its own — each is pinned alone as a control — and each broke only beside a
//! sibling file declaring the same names, which `annotate` collects as its
//! project (the file's own directory).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_steins")
}

fn workdir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir()
        .join(format!("steins-annotate-dup-{}-{tag}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create workdir");
    dir
}

/// Annotate `f.php` in `dir` with `args` (`--format json` or nothing) twice,
/// with and without the sidecar; the effect summary reads neither, so both
/// runs must print the same thing, which is returned.
fn annotate(dir: &Path, args: &[&str]) -> String {
    let mut outs = Vec::new();
    for no_php in [true, false] {
        let mut cmd = Command::new(bin());
        cmd.env_remove("GITHUB_ACTIONS").arg("annotate");
        if no_php {
            cmd.arg("--no-php");
        }
        let out = cmd.args(args).arg("f.php").current_dir(dir).output().expect("run steins");
        assert_eq!(
            out.status.code(),
            Some(0),
            "annotate never fails on a readable file, got:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        outs.push(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    assert_eq!(outs[0], outs[1], "the effect summary does not depend on the sidecar");
    outs.swap_remove(0)
}

/// A sibling declaring every name the layouts use, each with a pure body.
const SIBLING: &str = "<?php\n\
function a(): int { return 1; }\n\
function stamp(): int { return 1; }\n\
function stampx(): int { return 1; }\n\
function now(): int { return 1; }\n\
function f2(): int { return 1; }\n";

/// Every function in each layout calls `time()`, so every one of them carries
/// `nondet.time`.
struct Layout {
    source: &'static str,
    margin: &'static str,
    /// `(name, declaration line)`, in source order.
    functions: &'static [(&'static str, u32)],
}

const LAYOUTS: &[Layout] = &[
    Layout {
        source: "<?php\nfunction a(): int\n{\n    return time();\n}\n",
        margin: "<?php\nfunction a(): int  //=> effects: {nondet.time}\n{\n    return time();\n}\n",
        functions: &[("a", 2)],
    },
    Layout {
        source: "<?php\n\nfunction a(): int\n{\n    return time();\n}\n",
        margin: "<?php\n\nfunction a(): int  //=> effects: {nondet.time}\n{\n    return time();\n}\n",
        functions: &[("a", 3)],
    },
    Layout {
        source: "<?php\nfunction stamp(): int { return time(); }\n",
        margin: "<?php\nfunction stamp(): int { return time(); } //=> effects: {nondet.time}\n",
        functions: &[("stamp", 2)],
    },
    Layout {
        source: "<?php\n\nfunction stamp(): int { return time(); }\n",
        margin: "<?php\n\nfunction stamp(): int { return time(); } //=> effects: {nondet.time}\n",
        functions: &[("stamp", 3)],
    },
    Layout {
        source: "<?php\nfunction stampx(): int\n{\n    return time();\n}\nfunction now(): int\n{\n    return time();\n}\n",
        margin: "<?php\nfunction stampx(): int //=> effects: {nondet.time}\n{\n    return time();\n}\nfunction now(): int    //=> effects: {nondet.time}\n{\n    return time();\n}\n",
        functions: &[("stampx", 2), ("now", 6)],
    },
    Layout {
        source: "<?php\nfunction f2(): int {\n    return time();\n}\nfunction stampx(): int\n{\n    return time();\n}\n",
        margin: "<?php\nfunction f2(): int {   //=> effects: {nondet.time}\n    return time();\n}\nfunction stampx(): int //=> effects: {nondet.time}\n{\n    return time();\n}\n",
        functions: &[("f2", 2), ("stampx", 5)],
    },
];

fn assert_json(stdout: &str, expected: &[(&str, u32)], layout: &str) {
    let doc: serde_json::Value = serde_json::from_str(stdout).expect("valid json object");
    let want: Vec<serde_json::Value> = expected
        .iter()
        .map(|(name, line)| {
            serde_json::json!({
                "name": name,
                "line": line,
                "effects": ["nondet.time"],
                "declared": [],
                "exhaustive": true,
            })
        })
        .collect();
    assert_eq!(doc, serde_json::json!({ "functions": want }), "layout:\n{layout}");
}

#[test]
fn each_layout_alone_carries_the_time_label() {
    for (i, Layout { source, margin, functions }) in LAYOUTS.iter().enumerate() {
        let dir = workdir(&format!("alone{i}"));
        std::fs::write(dir.join("f.php"), source).expect("write fixture");
        assert_eq!(annotate(&dir, &[]), *margin, "layout:\n{source}");
        assert_json(&annotate(&dir, &["--format", "json"]), functions, source);
    }
}

#[test]
fn a_sibling_declaring_the_same_names_does_not_empty_the_summary() {
    for (i, Layout { source, margin, functions }) in LAYOUTS.iter().enumerate() {
        let dir = workdir(&format!("sibling{i}"));
        std::fs::write(dir.join("f.php"), source).expect("write fixture");
        std::fs::write(dir.join("g.php"), SIBLING).expect("write sibling");
        assert_eq!(annotate(&dir, &[]), *margin, "layout:\n{source}");
        assert_json(&annotate(&dir, &["--format", "json"]), functions, source);
    }
}

#[test]
fn a_conditional_declaration_in_one_file_keeps_the_merged_summary() {
    // Both declarations fold into one row: the merged set is an upper bound
    // for a call to `a()`, and it is what either declaration line shows.
    let dir = workdir("conditional");
    let source = "<?php\n\
if (PHP_OS_FAMILY === 'Windows') {\n\
    function a(): int\n\
    {\n\
        if (rand()) { throw new \\RuntimeException(); }\n\
        return time();\n\
    }\n\
} else {\n\
    function a(): int\n\
    {\n\
        return 1;\n\
    }\n\
}\n";
    std::fs::write(dir.join("f.php"), source).expect("write fixture");
    let text = annotate(&dir, &[]);
    let margins: Vec<&str> = text.lines().filter_map(|l| l.split_once("//=> ").map(|(_, m)| m)).collect();
    assert_eq!(
        margins,
        ["effects: {nondet.random, nondet.time}; throws: {RuntimeException}"; 2],
        "got:\n{text}"
    );
    let doc: serde_json::Value =
        serde_json::from_str(&annotate(&dir, &["--format", "json"])).expect("valid json object");
    for (entry, line) in doc["functions"].as_array().expect("functions array").iter().zip([3, 9]) {
        assert_eq!(entry["line"], serde_json::json!(line), "got:\n{doc}");
        assert_eq!(entry["effects"], serde_json::json!(["nondet.random", "nondet.time"]));
        assert_eq!(entry["exhaustive"], serde_json::json!(true));
    }
}
