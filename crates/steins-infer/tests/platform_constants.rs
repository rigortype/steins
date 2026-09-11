//! ADR-0094 / issue #598 — what a bare global constant is worth.
//!
//! `ArgValue::GlobalConst` is unproven by construction (issue #168), so every
//! one of these dumped `unknown` before this slice. The three lanes ADR-0094
//! rules are exercised here in the order the ADR writes them: §2's mined table
//! (a spec-fixed literal, `Verified`, gated on the declared `PhpTarget`), §3's
//! classes (the host unions, the 64-bit width, the version derivation).
//!
//! The `(asserted)` marker on the dump surface (ADR-0053 §2) is what says which
//! stratum answered, and it is the whole of §3.2: a default union is `Verified`
//! because it is true of every host the project can run on, while a value fixed
//! by a `[runtime] os` pin is the user's claim about the host and is `Asserted`.

use std::path::PathBuf;

use steins_db::{
    GoverningRoot, PhpTarget, PhpTargetSource, Project, ProjectLayout, SourceFile, SteinsDatabase,
};
use steins_infer::{
    DEBUG_TYPE_ID, FinalKeyword, NoFold, OsFamily, check_project_with_os, check_with,
};
use steins_syntax::SourceTree;

/// Every `dumpType` message in `src`, single-file and without an engine — the
/// resolver needs neither.
fn dumps(src: &str) -> Vec<String> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "t.php", &mut NoFold)
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message)
        .collect()
}

/// The dumped type of one expression, at top level.
fn dump(expr: &str) -> String {
    let src = format!("<?php\n\\PHPStan\\dumpType({expr});\n");
    dumps(&src).pop().expect("one dump")
}

fn require_target(raw: &str, floor: (u16, u16), ceiling: Option<(u16, u16)>) -> PhpTarget {
    PhpTarget { floor, ceiling, source: PhpTargetSource::Require, raw: raw.to_owned() }
}

/// Every diagnostic a whole source raises under a project that declares `target`
/// and pins `os`, as `(id, message)`. The `dumpType` surface answers what a
/// constant IS; this one answers what the analyzer still PROVES with it, which is
/// the only way to see a finding that a pin withdrew.
fn diagnostics_under(
    src: &str,
    target: Option<PhpTarget>,
    os: Option<OsFamily>,
) -> Vec<(String, String)> {
    let root = GoverningRoot::new(
        PathBuf::from("/proj/composer.json"),
        PathBuf::from("/proj"),
        vec![PathBuf::from("/proj/vendor")],
        vec![],
    )
    .with_php_target(target);
    let layout = ProjectLayout::new(PathBuf::from("/proj"), vec![root]);
    let db = SteinsDatabase::default();
    let file = SourceFile::new(&db, "/proj/t.php".to_owned(), src.to_owned());
    let project = Project::new(&db, vec![file], layout, steins_db::PluginFacts::none());
    check_project_with_os(&db, project, &mut NoFold, true, FinalKeyword::Enforced, os)
        .into_iter()
        .map(|d| (d.id.to_owned(), d.message.clone()))
        .collect()
}

/// Every `dumpType` message a whole source raises under `target` and `os`.
fn dumps_under(src: &str, target: Option<PhpTarget>, os: Option<OsFamily>) -> Vec<String> {
    diagnostics_under(src, target, os)
        .into_iter()
        .filter(|(id, _)| id == DEBUG_TYPE_ID)
        .map(|(_, message)| message)
        .collect()
}

/// The dumped type of `expr` under a project that declares `target` and pins
/// `os` — the two seams ADR-0094 §2 and §3 read.
fn dump_under(expr: &str, target: Option<PhpTarget>, os: Option<OsFamily>) -> String {
    let root = GoverningRoot::new(
        PathBuf::from("/proj/composer.json"),
        PathBuf::from("/proj"),
        vec![PathBuf::from("/proj/vendor")],
        vec![],
    )
    .with_php_target(target);
    let layout = ProjectLayout::new(PathBuf::from("/proj"), vec![root]);
    let db = SteinsDatabase::default();
    let src = format!("<?php\n\\PHPStan\\dumpType({expr});\n");
    let file = SourceFile::new(&db, "/proj/t.php".to_owned(), src);
    let project = Project::new(&db, vec![file], layout, steins_db::PluginFacts::none());
    check_project_with_os(&db, project, &mut NoFold, true, FinalKeyword::Enforced, os)
        .into_iter()
        .find(|d| d.id == DEBUG_TYPE_ID)
        .expect("one dump")
        .message
}

// ---------------------------------------------------------------- §2, the table

#[test]
fn a_spec_fixed_constant_is_its_literal_and_verified() {
    // The mined roster: one value on every host that has the constant at all,
    // which is exactly what lets it be Verified (ADR-0094 §3.2) — no
    // `(asserted)` marker.
    assert_eq!(dump("SORT_REGULAR"), "dumped type: 0");
    assert_eq!(dump("JSON_THROW_ON_ERROR"), "dumped type: 4194304");
    assert_eq!(dump("PREG_SPLIT_NO_EMPTY"), "dumped type: 1");
    assert_eq!(dump("DATE_ATOM"), r"dumped type: 'Y-m-d\\TH:i:sP'");
    assert_eq!(dump("M_PI"), "dumped type: 3.141592653589793");
}

#[test]
fn a_leading_backslash_is_not_part_of_the_name() {
    // `\SORT_REGULAR` and `SORT_REGULAR` are the same constant to PHP.
    assert_eq!(dump("\\SORT_REGULAR"), "dumped type: 0");
}

#[test]
fn a_constant_with_no_row_still_answers_nothing() {
    // Absence of a row is silence, never a claim. `E_ALL` is deliberately
    // refused at mining time — its value lost `E_STRICT`'s bit inside the mined
    // window, which `since`/`until` cannot express — and an unknown name has no
    // row at all.
    assert_eq!(dump("E_ALL"), "dumped type: unknown");
    assert_eq!(dump("SOME_CONSTANT_NOBODY_DEFINED"), "dumped type: unknown");
    // A family whose numbers are the C library's, refused whole.
    assert_eq!(dump("SIGCHLD"), "dumped type: unknown");
}

#[test]
fn a_constant_outside_the_targets_minor_range_answers_nothing() {
    // `FILTER_THROW_ON_FAILURE` arrived in 8.5. A project that supports 8.1
    // supports a minor where the name does not exist, so a value for it is not a
    // fact about that project — the row declines, exactly as the ADR-0069
    // declared-return floor's version gate does.
    let below = require_target(">=8.1", (8, 1), None);
    assert_eq!(dump_under("FILTER_THROW_ON_FAILURE", Some(below), None), "dumped type: unknown");
    // At or above the arrival, the row speaks.
    let at = require_target(">=8.5", (8, 5), None);
    assert_eq!(dump_under("FILTER_THROW_ON_FAILURE", Some(at), None), "dumped type: 268435456");
    // An UNDECLARED target admits, as the floor's gate does: a project that says
    // nothing about its PHP has not said it runs on the minor the row excludes.
    assert_eq!(dump_under("FILTER_THROW_ON_FAILURE", None, None), "dumped type: 268435456");
    // A rangeless row is unaffected by the target either way.
    let below = require_target(">=8.1", (8, 1), None);
    assert_eq!(dump_under("SORT_REGULAR", Some(below), None), "dumped type: 0");
}

// ------------------------------------------------------- §3, the host classes

#[test]
fn a_host_dependent_constant_defaults_to_the_union_of_its_values() {
    // A library cannot assume its deployment host, so the default is the union —
    // sound on every host, and Verified for that reason (no `(asserted)`).
    //
    // The spelling is the reference implementation's on both counts: a control
    // character forces PHP's double-quoted form, everything else stays
    // single-quoted, which is why `PHP_EOL` and `DIRECTORY_SEPARATOR` are written
    // differently for the same kind of union.
    assert_eq!(dump("PHP_EOL"), r#"dumped type: "\n"|"\r\n""#);
    assert_eq!(dump("DIRECTORY_SEPARATOR"), r"dumped type: '/'|'\\'");
    assert_eq!(dump("PATH_SEPARATOR"), "dumped type: ':'|';'");
    assert_eq!(
        dump("PHP_OS_FAMILY"),
        "dumped type: 'BSD'|'Darwin'|'Linux'|'Solaris'|'Unknown'|'Windows'"
    );
    // php-src does not close `PHP_OS`'s value set, so the strongest sound claim
    // is that it is a non-empty string.
    assert_eq!(dump("PHP_OS"), "dumped type: non-empty-string");
}

#[test]
fn the_os_pin_fixes_the_four_host_constants_together_at_asserted() {
    // ADR-0094 §3: pinning one and leaving the others as unions would let
    // `if (PHP_OS_FAMILY === 'Windows')` stay alive while `PHP_EOL` inside it is
    // already `"\n"`. And §3.2: a pinned value is the USER's claim about the
    // host, so it is Asserted and premises no proof-layer finding.
    let linux = Some(OsFamily::Linux);
    assert_eq!(dump_under("PHP_OS_FAMILY", None, linux), "dumped type: 'Linux' (asserted)");
    assert_eq!(dump_under("PHP_EOL", None, linux), r#"dumped type: "\n" (asserted)"#);
    assert_eq!(dump_under("DIRECTORY_SEPARATOR", None, linux), "dumped type: '/' (asserted)");
    assert_eq!(dump_under("PATH_SEPARATOR", None, linux), "dumped type: ':' (asserted)");

    let windows = Some(OsFamily::Windows);
    assert_eq!(dump_under("PHP_OS_FAMILY", None, windows), "dumped type: 'Windows' (asserted)");
    assert_eq!(dump_under("PHP_EOL", None, windows), r#"dumped type: "\r\n" (asserted)"#);
    assert_eq!(dump_under("DIRECTORY_SEPARATOR", None, windows), r"dumped type: '\\' (asserted)");
    assert_eq!(dump_under("PATH_SEPARATOR", None, windows), "dumped type: ';' (asserted)");

    // `PHP_OS` is NOT narrowed by a pin: php-src does not close its set per
    // family, so a pin has nothing to pick from.
    assert_eq!(dump_under("PHP_OS", None, linux), "dumped type: non-empty-string");
}

#[test]
fn integer_width_is_the_sixty_four_bit_literal_under_every_configuration() {
    // ADR-0094 §3.1. No `4|8` union and no knob: a union no runtime check can
    // narrow away is the contradiction phpstan#14948 §1 and §3 document.
    assert_eq!(dump("PHP_INT_MAX"), "dumped type: 9223372036854775807");
    assert_eq!(dump("PHP_INT_MIN"), "dumped type: -9223372036854775808");
    assert_eq!(dump("PHP_INT_SIZE"), "dumped type: 8");
    // Verified, and unmoved by a pin — the pin is about the OS, not the word size.
    assert_eq!(dump_under("PHP_INT_MAX", None, Some(OsFamily::Windows)), "dumped type: 9223372036854775807");
}

#[test]
fn php_version_id_is_the_range_the_declared_target_spans() {
    // ADR-0094 §3, "engine version": derived from the target and from nothing
    // else — the analyzing engine's own version never appears, because the
    // project is not analyzed for the machine running the analyzer.
    let exact = require_target("8.3.*", (8, 3), Some((8, 3)));
    assert_eq!(dump_under("PHP_VERSION_ID", Some(exact), None), "dumped type: int<80300, 80399>");
    let spread = require_target(">=8.1 <8.4", (8, 1), Some((8, 3)));
    assert_eq!(dump_under("PHP_VERSION_ID", Some(spread), None), "dumped type: int<80100, 80399>");
    // An open ceiling has no top, so the honest floor is the base.
    let open = require_target(">=8.1", (8, 1), None);
    assert_eq!(dump_under("PHP_VERSION_ID", Some(open), None), "dumped type: int");
    // No target at all: nothing to derive from.
    assert_eq!(dump_under("PHP_VERSION_ID", None, None), "dumped type: int");
}

#[test]
fn the_version_components_take_the_sharpest_form_the_target_supports() {
    let exact = require_target("8.3.*", (8, 3), Some((8, 3)));
    assert_eq!(dump_under("PHP_MAJOR_VERSION", Some(exact.clone()), None), "dumped type: 8");
    assert_eq!(dump_under("PHP_MINOR_VERSION", Some(exact.clone()), None), "dumped type: 3");
    // The patch level is what `PhpTarget` drops by design, so nothing narrows it.
    assert_eq!(dump_under("PHP_RELEASE_VERSION", Some(exact), None), "dumped type: int");
    // A range spanning minors agrees on the major and not on the minor.
    let spread = require_target(">=8.1 <8.4", (8, 1), Some((8, 3)));
    assert_eq!(dump_under("PHP_MAJOR_VERSION", Some(spread.clone()), None), "dumped type: 8");
    assert_eq!(dump_under("PHP_MINOR_VERSION", Some(spread), None), "dumped type: int");
    // No target: the base floor for both.
    assert_eq!(dump_under("PHP_MAJOR_VERSION", None, None), "dumped type: int");
    // A patch level is never derivable, so the version strings stay strings.
    assert_eq!(dump("PHP_VERSION"), "dumped type: non-empty-string");
    // A release build has no extra suffix, so the empty string is in the set.
    assert_eq!(dump("PHP_EXTRA_VERSION"), "dumped type: string");
}

// ------------------------------------------- the value lane, not just the dump

#[test]
fn a_constant_binds_a_variable_and_folds_as_an_argument() {
    // The resolver sits in the LITERAL seam as well as the fact seam, which is
    // what makes a constant behave like the value it is everywhere at once.
    let src = "<?php\n\
               $n = PHP_INT_SIZE;\n\
               \\PHPStan\\dumpType($n);\n\
               \\PHPStan\\dumpType(PHP_INT_SIZE === 8);\n\
               \\PHPStan\\dumpType('at ' . DATE_ATOM);\n";
    assert_eq!(
        dumps(src),
        vec![
            "dumped type: 8".to_owned(),
            "dumped type: true".to_owned(),
            r"dumped type: 'at Y-m-d\\TH:i:sP'".to_owned(),
        ]
    );
}

#[test]
fn a_union_valued_constant_is_not_a_literal() {
    // `PHP_EOL` without a pin is two values, and a literal seam that picked one
    // of them would be stating something false. The comparison is therefore
    // undecided and takes the `bool` floor rather than a wrong `true`.
    assert_eq!(dump("PHP_EOL === \"\\n\""), "dumped type: bool");
    // Pinned, it IS a literal, and the comparison decides — carrying the pin's
    // Asserted stratum out with it (ADR-0094 §3.2).
    assert_eq!(
        dump_under("PHP_EOL === \"\\n\"", None, Some(OsFamily::Linux)),
        "dumped type: true (asserted)"
    );
}


// -------------------------------------------------- §4, same-file user constants

#[test]
fn a_same_file_const_declaration_binds_its_literal() {
    // ADR-0094 §4: bound from the declaration the walk already parsed — no
    // sidecar, no generation input. Verified: the declaration IS the source.
    let src = "<?php\n\
               const GREETING = 'hi';\n\
               define('ANSWER', 42);\n\
               \\PHPStan\\dumpType(GREETING);\n\
               \\PHPStan\\dumpType(ANSWER);\n";
    assert_eq!(dumps(src), vec!["dumped type: 'hi'".to_owned(), "dumped type: 42".to_owned()]);
}

#[test]
fn a_namespaced_const_resolves_the_way_php_resolves_it() {
    // An unqualified reference inside a namespace tries `App\NAME` first and
    // falls back to global — which is what lets namespaced code write `PHP_EOL`
    // and mean the engine's. `define()` always declares the GLOBAL name, even
    // inside a namespace.
    let src = "<?php\n\
               namespace App;\n\
               const LOCAL = 7;\n\
               \\define('GLOBAL_ONE', 'g');\n\
               \\PHPStan\\dumpType(LOCAL);\n\
               \\PHPStan\\dumpType(GLOBAL_ONE);\n\
               \\PHPStan\\dumpType(SORT_REGULAR);\n";
    assert_eq!(
        dumps(src),
        vec![
            "dumped type: 7".to_owned(),
            "dumped type: 'g'".to_owned(),
            "dumped type: 0".to_owned(),
        ]
    );
}

#[test]
fn a_conditional_or_non_literal_definition_declines() {
    // Three declines, each for its own reason (ADR-0094 §4). A conditional
    // `define` states what the value is *when that branch runs*; a non-literal
    // initializer would need an evaluation this lowering does not do; a second
    // declaration of the same name is a fork without the `if`.
    let src = "<?php\n\
               if (\\PHP_INT_SIZE === 8) { define('GUARDED', 1); }\n\
               const COMPUTED = 2 * 3;\n\
               define('TWICE', 1);\n\
               define('TWICE', 2);\n\
               \\PHPStan\\dumpType(GUARDED);\n\
               \\PHPStan\\dumpType(COMPUTED);\n\
               \\PHPStan\\dumpType(TWICE);\n";
    assert_eq!(
        dumps(src),
        vec![
            "dumped type: unknown".to_owned(),
            "dumped type: unknown".to_owned(),
            "dumped type: unknown".to_owned(),
        ]
    );
}

#[test]
fn a_project_declaration_does_not_take_a_name_the_engine_holds() {
    // PHP refuses the redefinition and keeps its own. `define('SORT_REGULAR', 99)`
    // raises `Constant SORT_REGULAR already defined` and the constant is still
    // `0`; `define('PHP_EOL', 'x')` leaves `"\n"`. ADR-0094 §4's same-file binding
    // is for names the ENGINE does not have — reading it over an engine row
    // reported the project's number for a constant PHP never let it take.
    let src = "<?php\n\
               define('SORT_REGULAR', 99);\n\
               \\PHPStan\\dumpType(SORT_REGULAR);\n";
    assert_eq!(dumps(src), vec!["dumped type: 0".to_owned()]);
    // And a declaration that states no value does not fall through to a decline
    // either: the engine's row is still the answer.
    let src = "<?php\n\
               define('SORT_REGULAR', \\some_call());\n\
               \\PHPStan\\dumpType(SORT_REGULAR);\n";
    assert_eq!(dumps(src), vec!["dumped type: 0".to_owned()]);

    // §3's classes are held at EVERY minor Steins analyzes for, so the rule
    // reaches them too — and `PHP_VERSION_ID` is the one that mattered: declaring
    // it WAS a way around issue #29's discipline. The polyfill still takes the
    // whole version family out of §3, which is that discipline, but what it buys
    // is silence and not the polyfill's own number.
    let src = "<?php\n\
               define('PHP_INT_MAX', 5);\n\
               define('PHP_EOL', 'x');\n\
               define('PHP_VERSION_ID', 70400);\n\
               \\PHPStan\\dumpType(PHP_INT_MAX);\n\
               \\PHPStan\\dumpType(PHP_EOL);\n\
               \\PHPStan\\dumpType(PHP_VERSION_ID);\n";
    assert_eq!(
        dumps(src),
        vec![
            "dumped type: 9223372036854775807".to_owned(),
            r#"dumped type: "\n"|"\r\n""#.to_owned(),
            "dumped type: unknown".to_owned(),
        ]
    );
}

#[test]
fn below_the_rows_arrival_the_projects_own_declaration_is_what_runs() {
    // The one place a project DOES take an engine name: a minor where the engine
    // has no such constant. `FILTER_THROW_ON_FAILURE` arrives at 8.5, so a project
    // whose whole range sits below that runs on a PHP where the `define` is not
    // the dead code 8.5 makes it — it is the value the reader reaches.
    let src = "<?php\n\
               define('FILTER_THROW_ON_FAILURE', 7);\n\
               \\PHPStan\\dumpType(FILTER_THROW_ON_FAILURE);\n";
    let below = require_target(">=8.1 <8.5", (8, 1), Some((8, 4)));
    assert_eq!(dumps_under(src, Some(below), None), vec!["dumped type: 7".to_owned()]);

    // At or above the arrival, the engine holds the name over the whole range and
    // wins as it does everywhere else.
    let at = require_target(">=8.5", (8, 5), None);
    assert_eq!(dumps_under(src, Some(at), None), vec!["dumped type: 268435456".to_owned()]);

    // A range that STRADDLES the arrival declines. The answer there is the
    // engine's on one minor and the project's on another, and a union of the two
    // is not something a project that defines a name PHP is about to take away
    // has earned.
    let straddle = require_target(">=8.4", (8, 4), None);
    assert_eq!(dumps_under(src, Some(straddle), None), vec!["dumped type: unknown".to_owned()]);
}

#[test]
fn a_namespaced_twin_is_a_different_constant_and_keeps_its_own_value() {
    // `namespace App; const PREG_UNMATCHED_AS_NULL = 0;` declares
    // `App\PREG_UNMATCHED_AS_NULL`, which the engine has never heard of — so the
    // rule above has nothing to say and §4 binds the project's literal. The
    // engine-name rule must not reach a name that merely ENDS the same way.
    let src = "<?php\n\
               namespace App;\n\
               const PREG_UNMATCHED_AS_NULL = 0;\n\
               \\PHPStan\\dumpType(PREG_UNMATCHED_AS_NULL);\n";
    assert_eq!(dumps(src), vec!["dumped type: 0".to_owned()]);
    // The same file's fully-qualified spelling is the ENGINE's constant, since it
    // resolves to the global name and skips the namespace entirely.
    let src = "<?php\n\
               namespace App;\n\
               const PREG_UNMATCHED_AS_NULL = 0;\n\
               \\PHPStan\\dumpType(\\PREG_UNMATCHED_AS_NULL);\n";
    assert_eq!(dumps(src), vec!["dumped type: 512".to_owned()]);
}


#[test]
fn a_cross_file_project_constant_stops_the_walk() {
    // ADR-0094 §4 defers cross-file constants to their own slice, and the decline
    // is not caution: inside `namespace App;` an unqualified `SORT_REGULAR` means
    // `App\SORT_REGULAR`, so falling through to the global fallback would report
    // the engine's `0` for a name PHP reads as the project's own value.
    //
    // The same name in a file that does NOT declare it — and is not in that
    // namespace — still reads the engine row, so the guard costs nothing where it
    // has nothing to protect.
    let db = SteinsDatabase::default();
    let decl = SourceFile::new(
        &db,
        "/proj/a.php".to_owned(),
        "<?php\nnamespace App;\nconst SORT_REGULAR = 99;\n".to_owned(),
    );
    let read = SourceFile::new(
        &db,
        "/proj/b.php".to_owned(),
        "<?php\nnamespace App;\n\\PHPStan\\dumpType(SORT_REGULAR);\n".to_owned(),
    );
    let elsewhere = SourceFile::new(
        &db,
        "/proj/c.php".to_owned(),
        "<?php\nnamespace Other;\n\\PHPStan\\dumpType(SORT_REGULAR);\n".to_owned(),
    );
    let layout = ProjectLayout::new(PathBuf::from("/proj"), vec![]);
    let project = Project::new(
        &db,
        vec![decl, read, elsewhere],
        layout,
        steins_db::PluginFacts::none(),
    );
    let mut dumps: Vec<(String, String)> =
        check_project_with_os(&db, project, &mut NoFold, true, FinalKeyword::Enforced, None)
            .into_iter()
            .filter(|d| d.id == DEBUG_TYPE_ID)
            .map(|d| (d.path.clone(), d.message.clone()))
            .collect();
    dumps.sort();
    assert_eq!(
        dumps,
        vec![
            ("/proj/b.php".to_owned(), "dumped type: unknown".to_owned()),
            ("/proj/c.php".to_owned(), "dumped type: 0".to_owned()),
        ]
    );
}

// -------------------------------------- §3.2, what an Asserted value may decide

/// The `type.return-mismatch` messages a source raises under an `os` pin.
fn return_mismatches(src: &str, os: Option<OsFamily>) -> Vec<String> {
    diagnostics_under(src, None, os)
        .into_iter()
        .filter(|(id, _)| id == "type.return-mismatch")
        .map(|(_, message)| message)
        .collect()
}

#[test]
fn an_os_pinned_value_decides_no_reachability_through_a_variable() {
    // ADR-0094 §3.2: a pinned value premises no proof-layer finding. Marking a
    // branch dead IS a proof-layer act — it withdraws the findings inside it, and
    // a decided `if` whose arm returns makes the code after it unreachable too —
    // so under `os = "windows"` this function's `return "not-int"` simply stopped
    // being checked, and a `type.return-mismatch` present without the pin
    // disappeared because the user claimed a host.
    let through_a_variable = "<?php\n\
        function d3(): int { $v = PHP_EOL; if ($v === \"\\r\\n\") { return 1; } return \"not-int\"; }\n";
    // The same laundering one derivation further out: the comparison's own
    // `bool` is bound first and the guard reads THAT.
    let through_a_bool = "<?php\n\
        function d4(): int { $x = PHP_EOL === \"\\r\\n\"; if ($x) { return 1; } return \"not-int\"; }\n";
    let windows = Some(OsFamily::Windows);
    for src in [through_a_variable, through_a_bool] {
        assert_eq!(return_mismatches(src, None).len(), 1, "without a pin: {src}");
        assert_eq!(return_mismatches(src, windows).len(), 1, "under a pin: {src}");
    }

    // A VERIFIED binding still decides, which is what makes the gate a stratum
    // check and not a retreat: `PHP_INT_SIZE` is §3.1's 64-bit literal, true of
    // every target, so the guard is proven and the tail really is unreachable.
    let verified = "<?php\n\
        function d5(): int { $v = PHP_INT_SIZE; if ($v === 8) { return 1; } return \"not-int\"; }\n";
    assert!(return_mismatches(verified, windows).is_empty());
}

#[test]
fn an_aliased_pin_is_still_the_pin() {
    // `use const PHP_EOL as EOL;` spells `EOL`, and the stratum used to be keyed
    // on that spelling — so the alias carried a pinned value at `Verified` and
    // `EOL === "\r\n"` dumped `true` with no `(asserted)` on it. The pin is a
    // property of the constant PHP resolves to, never of the word the file wrote.
    let src = "<?php\n\
               use const PHP_EOL as EOL;\n\
               \\PHPStan\\dumpType(EOL);\n\
               \\PHPStan\\dumpType(EOL === \"\\r\\n\");\n";
    assert_eq!(
        dumps_under(src, None, Some(OsFamily::Windows)),
        vec![
            r#"dumped type: "\r\n" (asserted)"#.to_owned(),
            "dumped type: true (asserted)".to_owned(),
        ]
    );
    // And the alias decides no reachability either, for the same reason the
    // spelled-out name does not.
    let src = "<?php\n\
        use const PHP_EOL as EOL;\n\
        function d6(): int { $v = EOL; if ($v === \"\\r\\n\") { return 1; } return \"not-int\"; }\n";
    assert_eq!(return_mismatches(src, Some(OsFamily::Windows)).len(), 1);
}
