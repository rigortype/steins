//! End-to-end tests for the suppression channels (ADR-0022/0023): inline
//! `@steins-ignore` and the `.steins-baseline.jsonl` baseline.
//!
//! Each test runs the real `steins` binary in a private temp directory so the
//! default baseline file and relative paths are fully isolated.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_steins")
}

/// A fresh, unique working directory under the system temp dir.
fn workdir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("steins-suppress-{}-{tag}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create workdir");
    dir
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

/// Run `steins <args>` with CWD set to `dir`.
fn run_in(dir: &Path, args: &[&str]) -> Run {
    let out = Command::new(bin()).args(args).current_dir(dir).output().expect("run steins");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn write(dir: &Path, name: &str, contents: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, contents).expect("write fixture");
    p
}

/// A single coercive int-argument finding: `width("abc")` on the given line.
const WIDTH_DEF: &str = "<?php\nfunction width(int $w): int { return $w; }\n";

// ------------------------------------------------------------------ inline ---

#[test]
fn same_line_trailing_ignore_suppresses_its_own_line() {
    let dir = workdir("same-line");
    write(&dir, "a.php", &format!("{WIDTH_DEF}width(\"abc\"); // @steins-ignore type.argument-mismatch\n"));
    let r = run_in(&dir, &["check", "a.php"]);
    assert_eq!(r.code, 0, "suppressed → exit 0; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("1 diagnostics suppressed by inline ignores"), "got:\n{}", r.stdout);
    assert!(!r.stdout.contains("error["), "no finding printed, got:\n{}", r.stdout);
}

#[test]
fn own_line_ignore_suppresses_next_line() {
    let dir = workdir("next-line");
    write(&dir, "a.php", &format!("{WIDTH_DEF}// @steins-ignore type.argument-mismatch\nwidth(\"abc\");\n"));
    let r = run_in(&dir, &["check", "a.php"]);
    assert_eq!(r.code, 0, "next-line suppression; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("1 diagnostics suppressed by inline ignores"));
}

#[test]
fn prefix_and_bare_family_match() {
    for pat in ["type.*", "type"] {
        let dir = workdir("prefix");
        write(&dir, "a.php", &format!("{WIDTH_DEF}width(\"abc\"); // @steins-ignore {pat}\n"));
        let r = run_in(&dir, &["check", "a.php"]);
        assert_eq!(r.code, 0, "pattern {pat} should match; stdout:\n{}", r.stdout);
        assert!(r.stdout.contains("1 diagnostics suppressed"));
    }
}

#[test]
fn parenthesized_reason_is_allowed() {
    let dir = workdir("reason");
    write(&dir, "a.php", &format!("{WIDTH_DEF}width(\"abc\"); // @steins-ignore type.argument-mismatch (known bad, tracked elsewhere)\n"));
    let r = run_in(&dir, &["check", "a.php"]);
    assert_eq!(r.code, 0, "reason form suppresses; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("1 diagnostics suppressed"));
}

#[test]
fn hash_and_block_comment_forms_work() {
    let dir = workdir("hash");
    write(&dir, "h.php", &format!("{WIDTH_DEF}width(\"abc\"); # @steins-ignore type.argument-mismatch\n"));
    let r = run_in(&dir, &["check", "h.php"]);
    assert_eq!(r.code, 0, "hash comment; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("1 diagnostics suppressed"));

    let dir = workdir("block");
    write(&dir, "b.php", &format!("{WIDTH_DEF}width(\"abc\"); /* @steins-ignore type.argument-mismatch */\n"));
    let r = run_in(&dir, &["check", "b.php"]);
    assert_eq!(r.code, 0, "block comment; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("1 diagnostics suppressed"));
}

#[test]
fn comma_list_suppresses_and_reports_the_unmatched_member() {
    let dir = workdir("comma");
    write(&dir, "a.php", &format!("{WIDTH_DEF}width(\"abc\"); // @steins-ignore type.argument-mismatch, effect.envelope-exceeded\n"));
    let r = run_in(&dir, &["check", "a.php"]);
    // The type finding is suppressed; the effect id matched nothing → unmatched.
    assert_eq!(r.code, 1, "unmatched meta present → exit 1; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("1 diagnostics suppressed"), "got:\n{}", r.stdout);
    assert!(r.stdout.contains("error[suppress.unmatched]"), "got:\n{}", r.stdout);
    assert!(r.stdout.contains("@steins-ignore of effect.envelope-exceeded matches no diagnostic on line 3"), "got:\n{}", r.stdout);
}

#[test]
fn unmatched_ignore_is_reported_at_the_comment() {
    let dir = workdir("unmatched");
    // width(5) is fine → the ignore matches nothing on line 3.
    write(&dir, "a.php", &format!("{WIDTH_DEF}width(5); // @steins-ignore type.argument-mismatch\n"));
    let r = run_in(&dir, &["check", "a.php"]);
    assert_eq!(r.code, 1, "unmatched → exit 1; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("error[suppress.unmatched]"), "got:\n{}", r.stdout);
    assert!(r.stdout.contains("matches no diagnostic on line 3"), "got:\n{}", r.stdout);
    // The meta-diagnostic is reported at the comment's own line (line 3).
    assert!(r.stdout.contains("a.php:3:"), "reported at comment location, got:\n{}", r.stdout);
}

#[test]
fn unknown_id_is_reported_and_does_not_suppress() {
    let dir = workdir("unknown");
    write(&dir, "a.php", &format!("{WIDTH_DEF}width(\"abc\"); // @steins-ignore type.bogus\n"));
    let r = run_in(&dir, &["check", "a.php"]);
    assert_eq!(r.code, 1, "unknown id doesn't suppress; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("error[suppress.unknown-id]"), "got:\n{}", r.stdout);
    assert!(r.stdout.contains("unknown diagnostic id 'type.bogus'"), "got:\n{}", r.stdout);
    // The original finding still fires (bogus never matched it).
    assert!(r.stdout.contains("error[type.argument-mismatch]"), "got:\n{}", r.stdout);
    assert!(!r.stdout.contains("suppressed by inline"), "nothing suppressed, got:\n{}", r.stdout);
}

// ---------------------------------------------------------------- baseline ---

/// The base file with exactly one `type.argument-mismatch` finding.
fn one_finding() -> String {
    format!("{WIDTH_DEF}width(\"abc\");\n")
}

#[test]
fn set_baseline_writes_header_and_sorted_forward_slash_entries() {
    let dir = workdir("set");
    // Two files so path sorting and forward slashes are observable. Distinct
    // function names — a duplicated definition across files would be ambiguous
    // project-wide and produce no finding.
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    write(&dir, "sub/z.php", "<?php\nfunction height(int $h): int { return $h; }\nheight(\"abc\");\n");
    write(&dir, "a.php", &one_finding());
    let r = run_in(&dir, &["check", "--set-baseline", "a.php", "sub/z.php"]);
    assert_eq!(r.code, 0, "set-baseline exits 0; stderr:\n{}", r.stderr);

    let text = std::fs::read_to_string(dir.join(".steins-baseline.jsonl")).expect("baseline written");
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].contains(r#""steins-baseline":1"#), "header first, got:\n{text}");
    assert!(lines[0].contains(r#""note""#), "header carries note, got:\n{text}");
    assert_eq!(lines.len(), 3, "header + 2 entries, got:\n{text}");
    // Sorted by path: a.php before sub/z.php; forward slashes on the nested one.
    assert!(lines[1].contains(r#""path":"a.php""#), "got:\n{text}");
    assert!(lines[2].contains(r#""path":"sub/z.php""#), "forward slash + sorted, got:\n{text}");
    assert!(lines[1].contains(r#""id":"type.argument-mismatch""#), "got:\n{text}");
    assert!(lines[1].contains(r#""hash":"#), "entry carries a hash, got:\n{text}");
}

#[test]
fn rerun_after_set_baseline_is_all_baselined_exit_zero() {
    let dir = workdir("rerun");
    write(&dir, "a.php", &one_finding());
    assert_eq!(run_in(&dir, &["check", "--set-baseline", "a.php"]).code, 0);

    let r = run_in(&dir, &["check", "a.php"]); // auto-loads .steins-baseline.jsonl
    assert_eq!(r.code, 0, "all baselined → exit 0; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("1 findings in baseline"), "got:\n{}", r.stdout);
    assert!(!r.stdout.contains("error["), "no finding printed, got:\n{}", r.stdout);
}

#[test]
fn inserting_lines_above_keeps_the_finding_baselined() {
    let dir = workdir("lineshift");
    write(&dir, "a.php", &one_finding());
    assert_eq!(run_in(&dir, &["check", "--set-baseline", "a.php"]).code, 0);

    // Insert unrelated lines ABOVE the finding's neighborhood — the line number
    // shifts but the flagged line's text and its neighbors do not.
    write(&dir, "a.php", "<?php\n\n// an unrelated note added later\n\nfunction width(int $w): int { return $w; }\nwidth(\"abc\");\n");
    let r = run_in(&dir, &["check", "a.php"]);
    assert_eq!(r.code, 0, "line-shift immunity — still baselined; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("1 findings in baseline"), "got:\n{}", r.stdout);
}

#[test]
fn editing_the_flagged_line_resurfaces_and_marks_stale() {
    let dir = workdir("edit");
    write(&dir, "a.php", &one_finding());
    assert_eq!(run_in(&dir, &["check", "--set-baseline", "a.php"]).code, 0);

    // Change the flagged line itself: still a finding, but a new neighborhood → a
    // new hash, so the baseline entry no longer matches.
    write(&dir, "a.php", &format!("{WIDTH_DEF}width(\"xyz\");\n"));
    let r = run_in(&dir, &["check", "a.php"]);
    assert_eq!(r.code, 1, "resurfaced; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("error[type.argument-mismatch]"), "finding resurfaces, got:\n{}", r.stdout);
    assert!(r.stdout.contains("1 baseline entries no longer match (stale"), "stale count, got:\n{}", r.stdout);
}

#[test]
fn duplicate_findings_and_entries_match_one_for_one() {
    let dir = workdir("dup");
    // Two `width("abc")` calls with identical neighborhoods (a silent builtin filler
    // `strlen("x");` above and below each) → identical (id, path, hash) → two
    // identical baseline lines. (A bare undefined call would now itself be an S4
    // `call.undefined-function` finding, so the filler must be a resident builtin.)
    let src = "<?php\nfunction width(int $w): int { return $w; }\nstrlen(\"x\");\nwidth(\"abc\");\nstrlen(\"x\");\nwidth(\"abc\");\nstrlen(\"x\");\n";
    write(&dir, "a.php", src);
    assert_eq!(run_in(&dir, &["check", "--set-baseline", "a.php"]).code, 0);

    let text = std::fs::read_to_string(dir.join(".steins-baseline.jsonl")).unwrap();
    let entry_lines: Vec<&str> = text.lines().skip(1).collect();
    assert_eq!(entry_lines.len(), 2, "two duplicate entries, got:\n{text}");
    assert_eq!(entry_lines[0], entry_lines[1], "the two entries are identical, got:\n{text}");

    // Both baselined on rerun.
    let r = run_in(&dir, &["check", "a.php"]);
    assert_eq!(r.code, 0, "both baselined; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("2 findings in baseline"), "got:\n{}", r.stdout);

    // Drop ONE duplicate baseline line → one finding consumes it, the other
    // reports (one-for-one). No stale (the surviving entry is consumed).
    let trimmed = format!("{}\n{}\n", text.lines().next().unwrap(), entry_lines[0]);
    std::fs::write(dir.join(".steins-baseline.jsonl"), trimmed).unwrap();
    let r = run_in(&dir, &["check", "a.php"]);
    assert_eq!(r.code, 1, "one reported; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("1 findings in baseline"), "one consumed, got:\n{}", r.stdout);
    assert_eq!(
        r.stdout.matches("error[type.argument-mismatch]").count(),
        1,
        "exactly one reported, got:\n{}",
        r.stdout
    );
    assert!(!r.stdout.contains("stale"), "consumed entry is not stale, got:\n{}", r.stdout);
}

// ------------------------------------------------ debug lane exemption (#108) ---
//
// ADR-0053 §4/§8: the debug lane is exempt from the baseline on BOTH sides —
// never captured (`write_baseline`) and never matched (`match_baseline`). Before
// issue #108's fix, `write_baseline` captured a `debug.type` entry (the surface
// stage deliberately keeps the whole debug lane displayed, ADR-0053 §4, so a
// dump reached `inline.kept` uncaptured-but-present) and a rerun then baselined
// it, silently downgrading a guaranteed runtime fatal to "N findings in
// baseline" at exit 0.

#[test]
fn set_baseline_never_captures_a_debug_dump_entry() {
    let dir = workdir("debug-no-capture");
    // One ordinary finding plus one dumpType() call: only the ordinary finding
    // may reach the baseline.
    write(&dir, "a.php", &format!("{WIDTH_DEF}width(\"abc\");\n$y = 1; \\PHPStan\\dumpType($y);\n"));
    let r = run_in(&dir, &["check", "--set-baseline", "a.php"]);
    assert_eq!(r.code, 0, "set-baseline exits 0; stderr:\n{}", r.stderr);
    assert!(
        r.stderr.contains("wrote 1 baseline entries"),
        "only the ordinary finding is captured, not the dump; stderr:\n{}",
        r.stderr
    );

    let text = std::fs::read_to_string(dir.join(".steins-baseline.jsonl")).expect("baseline written");
    assert!(!text.contains("debug.type"), "no debug entry written, got:\n{text}");
    assert!(text.contains("type.argument-mismatch"), "the ordinary finding IS captured, got:\n{text}");
}

#[test]
fn debug_dump_still_fails_after_set_baseline_rerun() {
    // A dump-only fixture: `--set-baseline` has nothing to capture, and the dump
    // must keep reporting — and keep failing — on the very next run.
    let dir = workdir("debug-still-fails");
    write(&dir, "a.php", "<?php\n$x = 1;\n\\PHPStan\\dumpType($x);\n");
    let set = run_in(&dir, &["check", "--set-baseline", "a.php"]);
    assert_eq!(set.code, 0, "set-baseline exits 0; stderr:\n{}", set.stderr);
    assert!(set.stderr.contains("wrote 0 baseline entries"), "stderr:\n{}", set.stderr);

    let r = run_in(&dir, &["check", "a.php"]);
    assert_eq!(r.code, 1, "the dump still reds CI after --set-baseline; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("error[debug.type]"), "got:\n{}", r.stdout);
    assert!(!r.stdout.contains("in baseline"), "nothing was baselined, got:\n{}", r.stdout);
}

#[test]
fn a_leftover_debug_baseline_entry_never_suppresses_and_is_reported_stale() {
    // A pre-fix baseline (or a hand-edit) may still carry a `debug.*` entry.
    // ADR-0053's exemption is symmetric, so `match_baseline` must refuse it on the
    // read side exactly as `write_baseline` now refuses to write one. A debug
    // finding bypasses the matcher unconditionally (main.rs), so the outcome does
    // not depend on the forged entry's hash matching anything real — the fixture
    // uses an arbitrary one on purpose, to pin that the hash is irrelevant.
    //
    // The ruling this pins (defect 2's open question): a leftover debug entry is
    // NOT silently "dormant" (ADR-0050 §8's reading for an id outside the active
    // surface, which would keep it forever unreported) — it can never become
    // valid again, so it is reported the same way any other stale entry is:
    // counted in "N baseline entries no longer match (stale — rerun
    // --set-baseline)", which cleans it out on the next capture.
    let dir = workdir("debug-leftover-entry");
    write(&dir, "a.php", "<?php\n$x = 1;\n\\PHPStan\\dumpType($x);\n");
    assert_eq!(run_in(&dir, &["check", "--set-baseline", "a.php"]).code, 0);

    let header = std::fs::read_to_string(dir.join(".steins-baseline.jsonl")).unwrap();
    let header_line = header.lines().next().expect("header line");
    let forged = format!(
        "{header_line}\n{{\"id\":\"debug.type\",\"path\":\"a.php\",\"hash\":\"deadbeefdeadbeef\"}}\n"
    );
    std::fs::write(dir.join(".steins-baseline.jsonl"), forged).unwrap();

    let r = run_in(&dir, &["check", "a.php"]);
    assert_eq!(r.code, 1, "the dump still fails despite the leftover entry; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("error[debug.type]"), "not suppressed, got:\n{}", r.stdout);
    assert!(!r.stdout.contains("in baseline"), "not consumed as a baselined finding, got:\n{}", r.stdout);
    assert!(
        r.stdout.contains("1 baseline entries no longer match (stale"),
        "the leftover entry surfaces as stale, not silently dormant; got:\n{}",
        r.stdout
    );
}

#[test]
fn a_strict_captured_debug_entry_is_stale_on_a_default_run_too() {
    // Review finding on issue #108 (PR #133): the first cut of the stale fix kept
    // ADR-0062 A-G10's `captured <= rung` rung gate alongside the debug carve-out,
    // so a `debug.type` entry tagged `"surface":"strict"` and consulted on a
    // `default` run read as `Strict <= Default` = false and was silently kept —
    // dormant forever, exactly the bug the ruling above was meant to close. A
    // debug entry is dead weight at every rung (a debug finding is checked on
    // every profile, and it can never be matched — see the leftover-entry test
    // above), so the rung it happened to be captured at must not matter.
    let dir = workdir("debug-leftover-entry-strict-rung");
    write(&dir, "a.php", "<?php\n$x = 1;\n\\PHPStan\\dumpType($x);\n");
    assert_eq!(run_in(&dir, &["check", "--set-baseline", "--profile", "strict", "a.php"]).code, 0);

    let header = std::fs::read_to_string(dir.join(".steins-baseline.jsonl")).unwrap();
    let header_line = header.lines().next().expect("header line");
    let forged = format!(
        "{header_line}\n{{\"id\":\"debug.type\",\"path\":\"a.php\",\"hash\":\"deadbeefdeadbeef\",\"surface\":\"strict\"}}\n"
    );
    std::fs::write(dir.join(".steins-baseline.jsonl"), forged).unwrap();

    // Run under the DEFAULT profile — lower rung than the entry's own capture tag.
    let r = run_in(&dir, &["check", "a.php"]);
    assert_eq!(r.code, 1, "the dump still fails on a lower-rung run; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("error[debug.type]"), "not suppressed, got:\n{}", r.stdout);
    assert!(
        r.stdout.contains("1 baseline entries no longer match (stale"),
        "a strict-captured debug entry is stale even on a default run, got:\n{}",
        r.stdout
    );
}

#[test]
fn ignore_baseline_bypasses() {
    let dir = workdir("bypass");
    write(&dir, "a.php", &one_finding());
    assert_eq!(run_in(&dir, &["check", "--set-baseline", "a.php"]).code, 0);

    let r = run_in(&dir, &["check", "--ignore-baseline", "a.php"]);
    assert_eq!(r.code, 1, "baseline bypassed → finding fires; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("error[type.argument-mismatch]"), "got:\n{}", r.stdout);
    assert!(!r.stdout.contains("in baseline"), "no baseline accounting, got:\n{}", r.stdout);
}

#[test]
fn custom_baseline_path_round_trips() {
    let dir = workdir("custom");
    write(&dir, "a.php", &one_finding());
    assert_eq!(run_in(&dir, &["check", "--baseline", "custom.jsonl", "--set-baseline", "a.php"]).code, 0);
    assert!(dir.join("custom.jsonl").exists(), "custom file written");
    // The default file was NOT created.
    assert!(!dir.join(".steins-baseline.jsonl").exists(), "default untouched");

    let r = run_in(&dir, &["check", "--baseline", "custom.jsonl", "a.php"]);
    assert_eq!(r.code, 0, "custom baseline read; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("1 findings in baseline"), "got:\n{}", r.stdout);
}

#[test]
fn json_carries_suppressed_and_baselined_fields() {
    // Inline-suppressed: suppressed=1.
    let dir = workdir("json-inline");
    write(&dir, "a.php", &format!("{WIDTH_DEF}width(\"abc\"); // @steins-ignore type.argument-mismatch\n"));
    let r = run_in(&dir, &["check", "--format", "json", "a.php"]);
    let v: serde_json::Value = serde_json::from_str(&r.stdout).expect("json object");
    assert_eq!(v["suppressed"], 1, "got:\n{}", r.stdout);
    assert_eq!(v["baselined"], 0);
    assert_eq!(v["findings"].as_array().unwrap().len(), 0);

    // Baselined: baselined=1.
    let dir = workdir("json-base");
    write(&dir, "a.php", &one_finding());
    assert_eq!(run_in(&dir, &["check", "--set-baseline", "a.php"]).code, 0);
    let r = run_in(&dir, &["check", "--format", "json", "a.php"]);
    let v: serde_json::Value = serde_json::from_str(&r.stdout).expect("json object");
    assert_eq!(v["baselined"], 1, "got:\n{}", r.stdout);
    assert_eq!(v["findings"].as_array().unwrap().len(), 0);
}

// -------------------------------------------- return-mismatch integration ---

/// A file whose only finding is a `type.return-mismatch` on line 3.
const RETURN_FINDING: &str =
    "<?php\ndeclare(strict_types=1);\nfunction f(): int { return \"abc\"; }\n";

#[test]
fn return_mismatch_is_inline_ignorable() {
    // The new `type.return-mismatch` id flows through the inline-ignore channel
    // exactly like `type.argument-mismatch` (registry-governed, ADR-0022/0023).
    let dir = workdir("return-inline");
    write(
        &dir,
        "a.php",
        "<?php\ndeclare(strict_types=1);\nfunction f(): int { return \"abc\"; } // @steins-ignore type.return-mismatch\n",
    );
    let r = run_in(&dir, &["check", "a.php"]);
    assert_eq!(r.code, 0, "suppressed → exit 0; stdout:\n{}", r.stdout);
    assert!(
        r.stdout.contains("1 diagnostics suppressed by inline ignores"),
        "got:\n{}",
        r.stdout
    );
    assert!(!r.stdout.contains("error["), "no finding printed, got:\n{}", r.stdout);
}

#[test]
fn return_mismatch_family_ignore_matches() {
    // A `type.*` family ignore also covers `type.return-mismatch`.
    let dir = workdir("return-family");
    write(
        &dir,
        "a.php",
        "<?php\ndeclare(strict_types=1);\n// @steins-ignore type.*\nfunction f(): int { return \"abc\"; }\n",
    );
    let r = run_in(&dir, &["check", "a.php"]);
    assert_eq!(r.code, 0, "family-suppressed; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("1 diagnostics suppressed by inline ignores"), "got:\n{}", r.stdout);
}

#[test]
fn return_mismatch_is_baselineable() {
    let dir = workdir("return-base");
    write(&dir, "a.php", RETURN_FINDING);
    assert_eq!(run_in(&dir, &["check", "--set-baseline", "a.php"]).code, 0);

    let r = run_in(&dir, &["check", "a.php"]);
    assert_eq!(r.code, 0, "baselined → exit 0; stdout:\n{}", r.stdout);
    assert!(r.stdout.contains("1 findings in baseline"), "got:\n{}", r.stdout);
    assert!(!r.stdout.contains("error["), "no finding printed, got:\n{}", r.stdout);
}
