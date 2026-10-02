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
const PROPS: &[u8] = b"<?php\nclass Bag { public int $field\xC9 = 1; public string $field\xFF = \"a\"; }\n\
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
fn transform_refuses_a_byte_lossy_file_before_planning_it() {
    let proj = TempProject::new("transform");
    let lib = b"<?php\n// \x82\xA0\n/** @param int $x */\nfunction f($x) { return $x; }\n";
    proj.write("lib.php", lib);
    proj.write("main.php", b"<?php\nf(1);\n");
    for args in [&["transform", "phpdoc-to-native"][..], &["transform", "phpdoc-to-native", "--apply"]]
    {
        let mut args = args.to_vec();
        args.push(proj.path());
        let r = run(&args);
        assert_eq!(r.code, 2, "{args:?}: stderr:\n{}", r.stderr);
        assert!(r.stderr.contains("not valid UTF-8"), "{args:?}: stderr:\n{}", r.stderr);
        assert_eq!(proj.read("lib.php"), lib, "{args:?}: not one byte written");
    }
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
