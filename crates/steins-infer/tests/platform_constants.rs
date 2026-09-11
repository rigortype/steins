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
fn a_project_constant_shadows_the_engine_table_for_this_file() {
    // A file that declares the name is the file whose reader reaches it, so the
    // mined row must not answer over it — and a declaration that states no
    // value must not fall THROUGH to the mined row either, which would report
    // the engine's number for a name the project has taken.
    let src = "<?php\n\
               define('SORT_REGULAR', 99);\n\
               \\PHPStan\\dumpType(SORT_REGULAR);\n";
    assert_eq!(dumps(src), vec!["dumped type: 99".to_owned()]);
    let src = "<?php\n\
               define('SORT_REGULAR', \\some_call());\n\
               \\PHPStan\\dumpType(SORT_REGULAR);\n";
    assert_eq!(dumps(src), vec!["dumped type: unknown".to_owned()]);
}
