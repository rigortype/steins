//! End-to-end tests for a source file that is not valid UTF-8 (issue #927, an interim inside
//! ADR-0080 §3.2): the decode replaces each ill-formed sequence with U+FFFD, and no claim may
//! stand on two byte strings, or two names, that the replacement made alike. The writers
//! refuse such a file rather than write the decoding back over its bytes.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_steins")
}

/// Spawns with `GITHUB_ACTIONS` scrubbed so `check`'s CI auto-detection
/// (ADR-0054 §6) doesn't emit workflow commands where a test expects text.
fn steins_cmd() -> Command {
    let mut cmd = Command::new(bin());
    cmd.env_remove("GITHUB_ACTIONS");
    cmd
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run(args: &[&str]) -> Run {
    let out = steins_cmd().args(args).output().expect("run steins");
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
            "steins-bytelossy-{}-{}-{}",
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
    /// Write raw bytes: the point of the file is that they are not UTF-8.
    fn write(&self, name: &str, contents: &[u8]) -> PathBuf {
        let p = self.dir.join(name);
        std::fs::write(&p, contents).unwrap();
        p
    }
    fn read(&self, name: &str) -> Vec<u8> {
        std::fs::read(self.dir.join(name)).unwrap()
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

/// Two Shift-JIS strings that differ in their second byte: distinct PHP values, and the same
/// `"\u{FFFD}\u{FFFD}"` once decoded.
const SJIS: &[u8] = b"<?php\n$a = \"\x82\xA0\";\n$b = \"\x82\xA2\";\n\
var_dump($a === $b, strlen($a));\n$map = [\"\x82\xA0\" => 1, \"\x82\xA2\" => 2];\n\
var_dump(count($map));\n";

/// Two properties whose names differ only in a byte the decode replaces.
const PROPS: &[u8] = b"<?php\n\
class Bag { public int $field\xC9 = 1; public string $field\xFF = \"a\"; }\n\
$b = new Bag();\n$b->field\xFF = \"hello\";\n$b->field\xC9 = 2;\n";

/// The dump lines of a run, `line:col: …: dumped type: T` reduced to `T`, in order.
fn dumped(stdout: &str) -> Vec<&str> {
    stdout.lines().filter_map(|l| l.split_once("dumped type: ").map(|(_, t)| t)).collect()
}

#[test]
fn the_shift_jis_witness_no_longer_collapses_distinct_strings() {
    let proj = TempProject::new("sjis");
    proj.write("sjis.php", SJIS);
    for profile in ["strict", "default"] {
        let r = run(&["check", "--no-cache", "--profile", profile, proj.path()]);
        assert!(!r.stdout.contains("array.duplicate-key"), "{profile}:\n{}", r.stdout);
        // `$a === $b` is `false` in PHP, `strlen` is 2 (not the 6 bytes of two U+FFFD), and
        // the map has two entries. The base said `true`, `6` and `1`.
        let types = dumped(&r.stdout);
        assert_eq!(types.first(), Some(&"false"), "{profile}:\n{}", r.stdout);
        assert_ne!(types.get(1), Some(&"6"), "{profile}:\n{}", r.stdout);
        assert_eq!(types.get(2), Some(&"2"), "{profile}:\n{}", r.stdout);
    }
}

#[test]
fn two_properties_the_decode_made_alike_are_no_claim() {
    let proj = TempProject::new("props");
    proj.write("props.php", PROPS);
    for profile in ["strict", "default"] {
        let r = run(&["check", "--no-cache", "--profile", profile, proj.path()]);
        assert_eq!(r.code, 0, "{profile}: stdout:\n{}\nstderr:\n{}", r.stdout, r.stderr);
        assert!(r.stdout.is_empty(), "{profile}: a collapsed name claims nothing:\n{}", r.stdout);
    }
}

#[test]
fn a_body_whose_names_collapsed_is_not_descended_into_from_another_file() {
    // `h(null)` binds the callee's parameter to `null` and walks its body: the clean twin
    // reports `$x->m()` at the callee's line. When a variable in that body is spelled with a
    // byte the decode replaces, the body's names cannot be told apart and nothing is said.
    let body = |name: &[u8]| {
        [b"<?php\nfunction h($x) { $v".as_slice(), name, b" = 1; return $x->m(); }\n"].concat()
    };
    let clean = TempProject::new("descent-clean");
    clean.write("a.php", &body(b"X"));
    clean.write("b.php", b"<?php\nh(null);\n");
    let r = run(&["check", "--no-cache", clean.path()]);
    assert!(r.stdout.contains("error[call.on-null]"), "the control reports:\n{}", r.stdout);

    let lossy = TempProject::new("descent-lossy");
    lossy.write("a.php", &body(b"\xC9"));
    lossy.write("b.php", b"<?php\nh(null);\n");
    let r = run(&["check", "--no-cache", lossy.path()]);
    assert!(r.stdout.is_empty(), "no claim from a collapsed body:\n{}", r.stdout);
}

#[test]
fn byte_identical_latin_1_keys_still_report() {
    let proj = TempProject::new("latin1");
    proj.write(
        "latin1.php",
        b"<?php\n$m = [\"caf\xE9\" => 1, \"caf\xE9\" => 2];\n$a = \"caf\xE9\";\n\
$b = \"caf\xE9\";\nvar_dump($a === $b);\n",
    );
    let r = run(&["check", "--no-cache", proj.path()]);
    assert!(
        r.stdout.contains("error[array.duplicate-key]: array key \"caf\\xE9\" is declared twice"),
        "the true positive stays, spelled the way PHP spells those bytes:\n{}",
        r.stdout
    );
    // The same bytes are the same value, so `===` still proves `true`.
    assert_eq!(dumped(&r.stdout), ["true"], "{}", r.stdout);
}

#[test]
fn a_duplicated_genuine_replacement_character_key_in_a_utf8_file_still_reports() {
    let proj = TempProject::new("fffd");
    proj.write("fffd.php", "<?php\n$m = [\"\u{FFFD}\" => 1, \"\u{FFFD}\" => 2];\n".as_bytes());
    let r = run(&["check", "--no-cache", proj.path()]);
    assert!(r.stdout.contains("error[array.duplicate-key]"), "{}", r.stdout);
}

#[test]
fn a_warm_run_reads_the_same_bytes_as_a_cold_one() {
    // A manifest makes this a project the generation store caches: the first run parses and
    // publishes, the second serves the trees from the artifact.
    let proj = TempProject::new("warm");
    proj.write("composer.json", br#"{"name":"t/p","autoload":{"psr-4":{"T\\":"src/"}}}"#);
    std::fs::create_dir_all(proj.dir.join("src")).unwrap();
    proj.write("src/sjis.php", SJIS);
    proj.write("src/props.php", PROPS);
    let cold = run(&["check", proj.path()]);
    let warm = run(&["check", proj.path()]);
    assert!(proj.dir.join(".steins").exists(), "the run must have used the generation store");
    assert_eq!(cold.stdout, warm.stdout, "cold equals warm");
    assert!(!warm.stdout.contains("array.duplicate-key"), "{}", warm.stdout);
    assert!(!warm.stdout.contains("type.property-mismatch"), "{}", warm.stdout);
    assert_eq!(dumped(&warm.stdout).first(), Some(&"false"), "{}", warm.stdout);
}

#[test]
fn check_fix_refuses_a_byte_lossy_file_and_leaves_its_bytes() {
    let proj = TempProject::new("fix");
    let src = b"<?php\n// \x82\xA0\n$x = 5;\n\\PHPStan\\dumpType($x);\n";
    proj.write("app.php", src);
    let r = run(&["check", "--fix", proj.path()]);
    assert_eq!(r.code, 1, "stdout:\n{}\nstderr:\n{}", r.stdout, r.stderr);
    assert!(r.stderr.contains("fix refused (byte-lossy-source)"), "stderr:\n{}", r.stderr);
    assert!(r.stderr.contains("not valid UTF-8"), "stderr:\n{}", r.stderr);
    assert_eq!(proj.read("app.php"), src, "not one byte written");
}

#[test]
fn check_fix_writes_the_clean_files_and_leaves_a_byte_lossy_one_with_a_notice() {
    let proj = TempProject::new("fixmixed");
    proj.write("app.php", b"<?php\n$x = 5;\n\\PHPStan\\dumpType($x);\n");
    let legacy = b"<?php\n$s = \"\x82\xA0\";\n$y = 6;\n\\PHPStan\\dumpType($y);\n";
    proj.write("legacy.php", legacy);
    let r = run(&["check", "--fix", proj.path()]);
    assert!(r.stderr.contains("(byte-lossy-source)"), "the notice names it:\n{}", r.stderr);
    assert!(r.stderr.contains("legacy.php"), "and the file:\n{}", r.stderr);
    assert!(r.stderr.contains("fixed 1 finding(s) (1 file(s) written)"), "{}", r.stderr);
    assert_eq!(proj.read("app.php"), b"<?php\n$x = 5;\n");
    assert_eq!(proj.read("legacy.php"), legacy, "not one byte written");
    // The lossy file's dump is still a finding, not reported as fixed.
    let of_legacy = |l: &&str| l.contains("legacy.php");
    let (unfixed, fixed) = ("error[debug.type]", "fixed[");
    assert!(r.stdout.lines().filter(of_legacy).any(|l| l.contains(unfixed)), "{}", r.stdout);
    assert!(!r.stdout.lines().filter(of_legacy).any(|l| l.contains(fixed)), "{}", r.stdout);
    assert_eq!(r.code, 1, "the unfixed finding still fails the run:\n{}", r.stdout);
}

#[test]
fn a_lossy_string_that_nothing_reads_as_a_name_leaves_the_file_analysed() {
    // Each file spells a lossy string where a callable, an argument of a named call or an
    // effect-label position could read it, then a mistake the analysis proves. The base
    // reports the mistake, and so must this: no name token is over a replaced byte.
    let proj = TempProject::new("argmark");
    let tail = |n: &str| format!("function g_{n}(int $i): void {{}}\ng_{n}(\"x\");\n");
    let heads: [(&str, &[u8]); 4] = [
        ("m1.php", b"<?php\nvar_dump(strlen(\"\x82\"));\n"),
        ("m3.php", b"<?php\necho htmlspecialchars(\"\x82\xA0\");\n"),
        ("n1.php", b"<?php\nfunction f(): void { $s = \"\x82\"; echo $s; }\n"),
        (
            "p1.php",
            b"<?php\nclass UC { public function index(): void { $t = \"\x83\x86\"; echo $t; } }\n",
        ),
    ];
    for (name, head) in heads {
        let stem = name.trim_end_matches(".php");
        proj.write(name, &[head, tail(stem).as_bytes()].concat());
    }
    let r = run(&["check", "--no-cache", proj.path()]);
    for name in ["m1", "m3", "n1", "p1"] {
        let file = format!("{name}.php:");
        assert!(
            r.stdout
                .lines()
                .any(|l| l.contains(&file) && l.contains("error[type.argument-mismatch]")),
            "{name}.php stays analysed:\n{}",
            r.stdout
        );
    }
}

#[test]
fn check_fix_in_a_clean_file_is_not_disturbed_by_a_byte_lossy_neighbour() {
    // The post-check re-analyzes every file; the neighbour must keep its loss map there, or
    // its collapsed duplicate would look like a regression the edit caused.
    let proj = TempProject::new("fixneighbour");
    proj.write("app.php", b"<?php\n$x = 5;\n\\PHPStan\\dumpType($x);\n");
    let legacy = b"<?php\n$map = [\"\x82\xA0\" => 1, \"\x82\xA2\" => 2];\n";
    proj.write("legacy.php", legacy);
    let r = run(&["check", "--fix", proj.path()]);
    assert_eq!(r.code, 0, "stdout:\n{}\nstderr:\n{}", r.stdout, r.stderr);
    assert!(r.stderr.contains("fixed 1 finding(s) (1 file(s) written)"), "{}", r.stderr);
    assert_eq!(proj.read("app.php"), b"<?php\n$x = 5;\n");
    assert_eq!(proj.read("legacy.php"), legacy);
}

#[test]
fn transform_leaves_a_byte_lossy_file_out_of_the_plan_and_says_so() {
    let proj = TempProject::new("transform");
    let lib = b"<?php\n// \x82\xA0\n/** @param int $x */\nfunction f($x) { return $x; }\n";
    proj.write("lib.php", lib);
    proj.write("main.php", b"<?php\nf(1);\n");
    for flags in [&[] as &[&str], &["--apply"]] {
        let mut args = vec!["transform", "phpdoc-to-native"];
        args.extend_from_slice(flags);
        args.push(proj.path());
        let r = run(&args);
        assert_eq!(r.code, 0, "{args:?}: stderr:\n{}", r.stderr);
        assert!(r.stderr.contains("(byte-lossy-source)"), "{args:?}: stderr:\n{}", r.stderr);
        assert!(r.stderr.contains("not valid UTF-8"), "{args:?}: stderr:\n{}", r.stderr);
        assert!(!r.stdout.contains("+function"), "{args:?}: no diff for it:\n{}", r.stdout);
        assert_eq!(proj.read("lib.php"), lib, "{args:?}: not one byte written");
    }
}

#[test]
fn transform_still_writes_the_other_files_of_a_plan_with_a_byte_lossy_one() {
    let proj = TempProject::new("transformmixed");
    let lossy = b"<?php\n// \x82\xA0\n/** @param int $x */\nfunction f($x) { return $x; }\n";
    proj.write("lossy.php", lossy);
    proj.write("clean.php", b"<?php\n/** @param int $y */\nfunction h($y) { return $y; }\n");
    proj.write("main.php", b"<?php\nf(1);\nh(2);\n");
    let r = run(&["transform", "phpdoc-to-native", "--apply", proj.path()]);
    assert_eq!(r.code, 0, "stderr:\n{}", r.stderr);
    assert!(r.stderr.contains("(byte-lossy-source)"), "{}", r.stderr);
    let clean = String::from_utf8(proj.read("clean.php")).unwrap();
    assert!(clean.contains("function h(int $y)"), "the clean file is promoted:\n{clean}");
    assert_eq!(proj.read("lossy.php"), lossy, "the lossy file is not one byte different");
}

#[test]
fn transform_accounts_for_a_byte_lossy_file_as_refused_not_as_promoted() {
    // Two candidate sites, one per file; only the clean one is written. The oracle and the
    // refusals say so in both formats, rather than counting the lossy file's site as
    // promoted and leaving the difference to a stderr notice.
    let proj = TempProject::new("transformcounts");
    let lossy = b"<?php\n// \x82\xA0\n/** @param int $x */\nfunction f($x) { return $x; }\n";
    proj.write("lossy.php", lossy);
    proj.write("clean.php", b"<?php\n/** @param int $y */\nfunction h($y) { return $y; }\n");
    proj.write("main.php", b"<?php\nf(1);\nh(2);\n");

    let text = run(&["transform", "phpdoc-to-native", proj.path()]);
    assert!(text.stdout.contains("2 enumerated: 1 promoted, 1 refused"), "{}", text.stdout);
    let refusal = |l: &&str| l.contains("lossy.php") && l.contains("[byte-lossy-source]");
    assert_eq!(text.stdout.lines().filter(refusal).count(), 1, "{}", text.stdout);
    let diffs: Vec<&str> = text.stdout.lines().filter(|l| l.starts_with("+++ ")).collect();
    assert!(diffs.len() == 1 && diffs[0].contains("clean.php"), "only the clean file: {diffs:?}");

    let json = run(&["transform", "phpdoc-to-native", "--format", "json", proj.path()]);
    let doc: serde_json::Value = serde_json::from_str(&json.stdout).expect("json");
    let report = &doc["report"];
    assert_eq!(report["oracle"]["enumerated"], 2, "{}", json.stdout);
    assert_eq!(report["oracle"]["transformed"], 1, "{}", json.stdout);
    assert_eq!(report["oracle"]["refused"], 1, "{}", json.stdout);
    let refusals = report["refusals"].as_array().expect("refusals");
    assert_eq!(refusals.len(), 1, "{}", json.stdout);
    assert_eq!(refusals[0]["reason"], "byte-lossy-source");
    assert_eq!(refusals[0]["site"]["label"], "function f() param $x");
    assert!(refusals[0]["site"]["path"].as_str().unwrap().ends_with("lossy.php"));
    let edits = report["plan"]["edits"].as_array().unwrap();
    let edited: Vec<&str> = edits.iter().map(|e| e["path"].as_str().unwrap()).collect();
    assert!(!edited.is_empty() && edited.iter().all(|p| p.ends_with("clean.php")), "{edited:?}");

    let applied = run(&["transform", "phpdoc-to-native", "--apply", proj.path()]);
    assert!(applied.stdout.contains("2 enumerated: 1 promoted, 1 refused"), "{}", applied.stdout);
    assert_eq!(proj.read("lossy.php"), lossy);
    assert!(String::from_utf8(proj.read("clean.php")).unwrap().contains("function h(int $y)"));
}

#[test]
fn every_transform_refuses_a_byte_lossy_files_sites_by_name() {
    // Each kind enumerates the same sites in a clean file and its byte-lossy twin (the twin
    // differs only in a comment); the twin's all end refused as `byte-lossy-source`, none
    // transformed, and no edit reaches the plan.
    let doc = b"/** @param int $x */\nfunction f($x) { return $x; }\n";
    let cases: [(&str, Vec<u8>, bool); 5] = [
        ("phpdoc-to-native", [doc.as_slice(), b"f(1);\n"].concat(), true),
        ("phpdoc-honesty", [doc.as_slice(), b"f(\"a\");\n"].concat(), true),
        ("throws-envelope", b"function t() { throw new Exception(\"x\"); }\n".to_vec(), true),
        ("effects-envelope", b"function e(): void { echo \"x\"; }\n".to_vec(), true),
        (
            "loop-to-array-map",
            b"function m(array $xs) { $o = []; foreach ($xs as $x) { $o[] = $x; } return $o; }\n"
                .to_vec(),
            false,
        ),
    ];
    for (kind, body, promotes) in cases {
        let report = |comment: &[u8]| {
            let proj = TempProject::new("kinds");
            proj.write("a.php", &[b"<?php\n// ", comment, b"\n", body.as_slice()].concat());
            let r = run(&["transform", kind, "--format", "json", proj.path()]);
            let doc: serde_json::Value = serde_json::from_str(&r.stdout).expect("json");
            doc["report"].clone()
        };
        let clean = report(b"c");
        if promotes {
            let o = &clean["oracle"];
            assert!(o["transformed"].as_u64() > Some(0), "{kind}: the control writes:\n{clean}");
        }
        let lossy = report(b"\x82\xA0");
        let o = &lossy["oracle"];
        assert!(o["enumerated"].as_u64() > Some(0), "{kind}: the site is enumerated:\n{lossy}");
        assert_eq!(o["transformed"], 0, "{kind}:\n{lossy}");
        assert_eq!(o["refused"], o["enumerated"], "{kind}:\n{lossy}");
        assert_eq!(lossy["plan"]["edits"], serde_json::json!([]), "{kind}:\n{lossy}");
        let refusals = lossy["refusals"].as_array().unwrap();
        assert!(
            refusals.iter().all(|r| r["reason"] == "byte-lossy-source"),
            "{kind}:\n{lossy}"
        );
    }
}

#[test]
fn check_fix_json_names_the_files_whose_fixes_were_left_out() {
    let proj = TempProject::new("fixjson");
    proj.write("app.php", b"<?php\n$x = 5;\n\\PHPStan\\dumpType($x);\n");
    let legacy = b"<?php\n$s = \"\x82\xA0\";\n$y = 6;\n\\PHPStan\\dumpType($y);\n";
    proj.write("legacy.php", legacy);
    let r = run(&["check", "--fix", "--format", "json", proj.path()]);
    let doc: serde_json::Value = serde_json::from_str(&r.stdout).expect("json");
    let skipped = doc["fix"]["skipped"].as_array().expect("skipped");
    assert_eq!(skipped.len(), 1, "{}", r.stdout);
    assert_eq!(skipped[0]["reason"], "byte-lossy-source");
    assert!(skipped[0]["path"].as_str().unwrap().ends_with("legacy.php"), "{}", r.stdout);
    assert!(skipped[0]["detail"].as_str().unwrap().contains("not valid UTF-8"), "{}", r.stdout);
    // The lossy file's finding is still a finding, with the fix that was not applied.
    let findings = doc["findings"].as_array().unwrap();
    let unfixed = |f: &&serde_json::Value| f["path"].as_str().unwrap().ends_with("legacy.php");
    assert_eq!(findings.iter().filter(unfixed).count(), 1, "{}", r.stdout);
    assert_eq!(doc["fix"]["fixed"].as_array().unwrap().len(), 1, "{}", r.stdout);
    assert_eq!(proj.read("legacy.php"), legacy);
}

#[test]
fn check_fix_json_has_an_empty_skipped_list_when_nothing_was_left_out() {
    let proj = TempProject::new("fixjsonclean");
    proj.write("app.php", b"<?php\n$x = 5;\n\\PHPStan\\dumpType($x);\n");
    let r = run(&["check", "--fix", "--format", "json", proj.path()]);
    let doc: serde_json::Value = serde_json::from_str(&r.stdout).expect("json");
    assert_eq!(doc["fix"]["skipped"], serde_json::json!([]), "{}", r.stdout);
}

#[test]
fn annotate_refuses_a_byte_lossy_file() {
    let proj = TempProject::new("annotate");
    let file = proj.write("lib.php", b"<?php\n// \x82\xA0\nfunction f() { return 1; }\n");
    let r = run(&["annotate", file.to_str().unwrap()]);
    assert_eq!(r.code, 2, "stdout:\n{}\nstderr:\n{}", r.stdout, r.stderr);
    assert!(r.stderr.contains("not valid UTF-8"), "stderr:\n{}", r.stderr);
    assert!(r.stdout.is_empty(), "no annotated copy is printed:\n{}", r.stdout);
}

/// The finding ids of a run, one per `error[...]` line, in order.
fn ids(stdout: &str) -> Vec<&str> {
    stdout
        .lines()
        .filter_map(|l| l.split_once("error[")?.1.split_once(']').map(|(id, _)| id))
        .collect()
}

#[test]
fn an_effect_label_over_non_utf8_bytes_stays_an_unknown_label_and_a_bound_envelope() {
    // The label's bytes are `io` and `0xC9`, the same bytes whether the file writes the
    // escape (a valid UTF-8 file) or the raw byte (a Latin-1 one). The vocabulary is ASCII
    // plus plugin labels, so it is an unknown label, and the envelope it sits in stays bound:
    // the `echo` it does not cover exceeds it under the strict profile. Both findings are
    // true positives on the base, and dropping the envelope would lose them together.
    let proj = TempProject::new("label");
    let body = b"] function f(): void { echo \"x\"; }\n";
    proj.write("esc.php", &[b"<?php\n#[\\Steins\\Effect(\"io\\xC9\")".as_slice(), body].concat());
    proj.write("raw.php", &[b"<?php\n#[\\Steins\\Effect(\"io\xC9\")".as_slice(), body].concat());
    for file in ["esc.php", "raw.php"] {
        let path = format!("{}/{file}", proj.path());
        let default = run(&["check", "--no-cache", "--profile", "default", &path]);
        assert_eq!(ids(&default.stdout), ["effect.unknown-label"], "{file}:\n{}", default.stdout);
        assert!(
            default.stdout.contains("unknown effect label 'io\\xC9' in #[\\Steins\\Effect]"),
            "{file}: the label is spelled the way PHP spells those bytes:\n{}",
            default.stdout
        );
        assert!(default.stdout.contains(" on f()"), "{file}:\n{}", default.stdout);
        let strict = run(&["check", "--no-cache", "--profile", "strict", &path]);
        assert_eq!(
            ids(&strict.stdout),
            ["effect.unknown-label", "effect.envelope-exceeded"],
            "{file}:\n{}",
            strict.stdout
        );
    }
}
