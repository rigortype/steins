//! End-to-end CLI tests for `steins transform loop-to-array-map` (ADR-0076).
//!
//! The differential fixture is the point of this file. ADR-0076 §5 asks for
//! behaviour identity to be **measured, not argued**: the flagship rewrite is
//! applied by the real binary and then *both* spellings are executed under the
//! real `php`, with their outputs compared. An equivalence claim that is only
//! reasoned about in a doc comment is a claim; an equivalence claim that runs is
//! evidence.
//!
//! `php` is present in this environment and in CI (the sidecar needs it anyway);
//! if it were missing these tests would fail loudly, which is the correct signal
//! for a fixture whose whole job is to execute PHP.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_steins")
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run(args: &[&str]) -> Run {
    let out = Command::new(bin()).args(args).output().expect("run steins");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// A throwaway project directory under the OS temp dir, cleaned on drop.
struct TempProject {
    dir: PathBuf,
}

impl TempProject {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "steins-loops-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }
    fn write(&self, name: &str, contents: &str) -> PathBuf {
        let p = self.dir.join(name);
        std::fs::write(&p, contents).unwrap();
        p
    }
    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.join(name)).unwrap()
    }
    fn path(&self) -> &str {
        self.dir.to_str().unwrap()
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Execute a PHP file under the project's own `php` and return its stdout. A
/// non-zero exit (a fatal, a TypeError) fails the test with the interpreter's own
/// message — a rewrite that does not even run is the loudest possible failure.
fn php_output(path: &std::path::Path) -> String {
    let out = Command::new("php").arg(path).output().expect("run php");
    assert!(
        out.status.success(),
        "php exited {:?} on {}:\n{}\n{}",
        out.status.code(),
        path.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The flagship fixture: an eligible loop plus a `var_export` of the result, so
/// the program's observable output *is* the accumulated array — keys included,
/// which is what the list-ness precondition exists to protect.
const FIXTURE: &str = "\
<?php

function label(int $n): string
{
    return 'n=' . ($n * 2);
}

function run(): array
{
    $rows = [3, 1, 4];
    $out = [];
    foreach ($rows as $row) {
        $out[] = label($row);
    }

    return $out;
}

$rows = [3, 1, 4, 1, 5];
$out = [];
foreach ($rows as $row) {
    $out[] = label($row);
}

var_export($out);
echo \"\\n\";
var_export(run());
echo \"\\n\";
";

// ---- The differential fixture (ADR-0076 §5) --------------------------------

#[test]
fn both_spellings_produce_identical_output_under_the_real_php() {
    let proj = TempProject::new("differential");
    let before = proj.write("fixture.php", FIXTURE);
    let before_out = php_output(&before);

    let r = run(&["transform", "loop-to-array-map", "--apply", proj.path()]);
    assert_eq!(r.code, 0, "stderr:\n{}", r.stderr);

    let after_src = proj.read("fixture.php");
    // Both loops — the top-level one and the one inside a function — are
    // rewritten, so the comparison below covers both scopes.
    assert_eq!(
        after_src.matches("array_map(fn ($row) => label($row), $rows);").count(),
        2,
        "expected both loops rewritten:\n{after_src}"
    );
    assert!(!after_src.contains("foreach"), "a loop survived:\n{after_src}");
    assert!(r.stdout.contains("2 enumerated: 2 rewritten, 0 refused"), "oracle:\n{}", r.stdout);

    let after_out = php_output(&before);
    assert_eq!(
        before_out, after_out,
        "the two spellings disagree under php:\nbefore:\n{before_out}\nafter:\n{after_out}"
    );
    // Not vacuous: the fixture really printed the accumulated arrays.
    assert!(before_out.contains("'n=6'"), "fixture produced nothing to compare:\n{before_out}");
}

#[test]
fn a_non_list_subject_would_have_changed_the_keys_and_is_refused() {
    // The measurement behind the `subject-not-proven-list` gate: with a
    // string-keyed subject the two spellings genuinely differ, so the refusal is
    // not conservatism for its own sake.
    let proj = TempProject::new("keys");
    let loop_form = "<?php\n$xs = ['a' => 1, 'b' => 2];\n$out = [];\nforeach ($xs as $x) {\n    $out[] = $x * 2;\n}\nvar_export($out);\n";
    let map_form = "<?php\n$xs = ['a' => 1, 'b' => 2];\n$out = array_map(fn ($x) => $x * 2, $xs);\nvar_export($out);\n";
    let a = proj.write("loop.php", loop_form);
    let b = proj.write("map.php", map_form);
    assert_ne!(
        php_output(&a),
        php_output(&b),
        "array_map was expected to preserve the string keys the append renumbers"
    );

    // And Steins refuses exactly that shape rather than laundering it.
    let single = TempProject::new("keys-refusal");
    single.write("loop.php", loop_form);
    let r = run(&["transform", "loop-to-array-map", single.path()]);
    assert_eq!(r.code, 0, "a refusal is not a failure; stderr:\n{}", r.stderr);
    assert!(r.stdout.contains("subject-not-proven-list"), "stdout:\n{}", r.stdout);
}

// ---- CLI surface -----------------------------------------------------------

#[test]
fn dry_run_prints_a_diff_and_writes_nothing() {
    let proj = TempProject::new("dryrun");
    let before = "<?php\n$xs = [1, 2, 3];\n$out = [];\nforeach ($xs as $x) {\n    $out[] = strlen((string) $x);\n}\nvar_export($out);\n";
    proj.write("lib.php", before);

    let r = run(&["transform", "loop-to-array-map", proj.path()]);
    assert_eq!(r.code, 0, "stderr:\n{}", r.stderr);
    assert!(
        r.stdout.contains("+$out = array_map(fn ($x) => strlen((string) $x), $xs);"),
        "diff:\n{}",
        r.stdout
    );
    assert!(r.stdout.contains("1 enumerated: 1 rewritten, 0 refused"), "oracle:\n{}", r.stdout);
    assert!(r.stdout.contains("Post-check OK"), "postcheck:\n{}", r.stdout);
    assert_eq!(proj.read("lib.php"), before, "a dry run wrote to disk");
}

#[test]
fn refusals_are_reported_with_their_stable_reason() {
    let proj = TempProject::new("refuse");
    let before = "<?php\nfunction f(array $xs): array {\n    $out = [];\n    foreach ($xs as $k => $v) {\n        $out[] = $v;\n    }\n    return $out;\n}\n";
    proj.write("lib.php", before);

    let r = run(&["transform", "loop-to-array-map", "--apply", proj.path()]);
    assert_eq!(r.code, 0, "a refusal is not a failure; stderr:\n{}", r.stderr);
    assert!(r.stdout.contains("key-binding"), "reason:\n{}", r.stdout);
    assert!(r.stdout.contains("0 rewritten, 1 refused"), "oracle:\n{}", r.stdout);
    assert_eq!(proj.read("lib.php"), before, "a refused site was written anyway");
}

#[test]
fn json_format_carries_the_plan_the_oracle_and_the_postcheck() {
    let proj = TempProject::new("json");
    proj.write(
        "lib.php",
        "<?php\n$xs = [1, 2];\n$out = [];\nforeach ($xs as $x) {\n    $out[] = $x;\n}\nvar_export($out);\n",
    );

    let r = run(&["transform", "loop-to-array-map", "--format", "json", proj.path()]);
    assert_eq!(r.code, 0, "stderr:\n{}", r.stderr);
    let v: serde_json::Value = serde_json::from_str(&r.stdout).expect("valid json");
    assert_eq!(v["report"]["oracle"]["enumerated"], 1);
    assert_eq!(v["report"]["oracle"]["transformed"], 1);
    assert_eq!(v["postcheck"]["ok"], true);
    assert_eq!(v["applied"], false);
    assert_eq!(v["report"]["plan"]["edits"].as_array().unwrap().len(), 1);
}

#[test]
fn apply_writes_only_after_the_post_check_reports_ok() {
    // ADR-0034 point 3a on this transform's own surface: the post-check runs in
    // BOTH dry-run and `--apply`, and the write happens only behind it.
    //
    // The failing branch is deliberately not fixture-driven here, because no
    // regression is constructible for this rewrite: everything the checker can
    // see in the loop — literal arguments, arity, class references, effect
    // origins — it still sees in the `array_map` form (the arrow function is a
    // recognized invocation shape, so its origins edge straight back), while the
    // arrow function's own scope is analyzed with an empty environment and can
    // therefore prove strictly less, never more. A synthetic failure would be a
    // test of the shared `post_check`, which is not this transform's contract.
    let proj = TempProject::new("postcheck");
    let before = "<?php\nfunction w(int $n): int { return $n; }\n$xs = [1, 2];\n$out = [];\nforeach ($xs as $x) {\n    $out[] = w($x);\n}\nvar_export($out);\n";
    proj.write("lib.php", before);

    let dry = run(&["transform", "loop-to-array-map", proj.path()]);
    assert_eq!(dry.code, 0, "stderr:\n{}", dry.stderr);
    assert!(dry.stdout.contains("Post-check OK"), "dry run must post-check:\n{}", dry.stdout);
    assert_eq!(proj.read("lib.php"), before, "a dry run wrote to disk");

    let applied = run(&["transform", "loop-to-array-map", "--apply", proj.path()]);
    assert_eq!(applied.code, 0, "stderr:\n{}", applied.stderr);
    assert!(applied.stdout.contains("Post-check OK"), "apply must post-check:\n{}", applied.stdout);
    assert!(applied.stderr.contains("applied 1 file edit"), "stderr:\n{}", applied.stderr);
    assert!(proj.read("lib.php").contains("array_map"), "not written:\n{}", proj.read("lib.php"));
}

#[test]
fn a_run_with_nothing_to_do_claims_no_post_check() {
    // An empty plan short-circuits the post-check, so a refusal-only run must not
    // print a verification claim it never made.
    let proj = TempProject::new("nothing");
    proj.write("lib.php", "<?php\nfunction f(array $xs): void {\n    foreach ($xs as $x) {\n        echo $x;\n    }\n}\n");
    let r = run(&["transform", "loop-to-array-map", proj.path()]);
    assert_eq!(r.code, 0, "stderr:\n{}", r.stderr);
    assert!(!r.stdout.contains("Post-check"), "claimed a post-check it skipped:\n{}", r.stdout);
    assert!(r.stdout.contains("0 rewritten, 1 refused"), "oracle:\n{}", r.stdout);
}

#[test]
fn unknown_and_missing_transform_names_still_list_this_one() {
    let bad = run(&["transform", "bogus-transform", "."]);
    assert_eq!(bad.code, 2);
    assert!(bad.stderr.contains("loop-to-array-map"), "stderr:\n{}", bad.stderr);

    let none = run(&["transform"]);
    assert_eq!(none.code, 2);
    assert!(none.stderr.contains("loop-to-array-map"), "stderr:\n{}", none.stderr);
}
