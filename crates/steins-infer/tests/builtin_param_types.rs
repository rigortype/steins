//! Issue #423 / ADR-0056 §9 — a builtin's arguments are judged against the
//! engine's own reflected parameter types, by the relation that has judged
//! project parameters since ADR-0043.
//!
//! The mock below is the sidecar's answer written down: every parameter list in
//! it was read off PHP 8.5.9 with `ReflectionFunction::getParameters()`, and
//! `steins-sidecar`'s `reflect_reports_the_parameter_list` pins the same
//! signatures against a live engine, so a signature that moves fails there
//! rather than quietly re-labelling these fixtures.
//!
//! ```text
//! strlen(string $string): int
//! dechex(int $num): string
//! abs(int|float $num): int|float
//! file_get_contents(string $filename, bool $use_include_path = false, …)
//! preg_match(string $pattern, string $subject, mixed &$matches = null, …)
//! sprintf(string $format, mixed ...$values): string
//! var_dump(mixed $value, mixed ...$values): void
//! str_replace(array|string $search, array|string $replace, string|array $subject, mixed &$count = null)
//! ```
//!
//! The coercion cells are not asserted from memory either: the `php`-measured
//! grid in `harness/coercion-grid/witness-internal-{strict,coercive}.tsv` is
//! replayed cell for cell at the bottom of this file, on the same rule the
//! userland grid carries — a Steins finding where PHP accepts fails outright.

use std::collections::HashMap;
use std::path::PathBuf;

use steins_infer::{
    Diagnostic, Folder, ID, PHPDOC_MAYBE_ARGUMENT_MISMATCH_ID, SidecarFolder,
    TYPE_MAYBE_ARGUMENT_MISMATCH_ID, check_with,
};
use steins_sidecar::BuiltinParam;
use steins_syntax::{ArgValue, SourceTree};

// ---------------------------------------------------------------------------
// The mock engine
// ---------------------------------------------------------------------------

/// A parameter position, spelled the way the wire carries it.
fn p(name: &str, ty: Option<&str>) -> BuiltinParam {
    BuiltinParam {
        name: name.to_owned(),
        ty: ty.map(ToOwned::to_owned),
        by_ref: false,
        variadic: false,
        optional: false,
    }
}

fn by_ref(name: &str, ty: Option<&str>) -> BuiltinParam {
    BuiltinParam { by_ref: true, optional: true, ..p(name, ty) }
}

fn variadic(name: &str, ty: Option<&str>) -> BuiltinParam {
    BuiltinParam { variadic: true, optional: true, ..p(name, ty) }
}

fn optional(name: &str, ty: Option<&str>) -> BuiltinParam {
    BuiltinParam { optional: true, ..p(name, ty) }
}

/// A PHP that answers the ADR-0056 §9 parameter surface for the names below and
/// nothing else — which is also the "a name the folder does not answer" case.
///
/// `boot` additionally opens the ADR-0049 absence family, which the arity check
/// needs for its A2ii homonym leg. It is off by default so these fixtures never
/// have to reason about `call.undefined-function` beside the id under test.
struct Mock {
    params: HashMap<String, Vec<BuiltinParam>>,
    boot: bool,
}

impl Mock {
    fn sidecar() -> Mock {
        let mut params = HashMap::new();
        params.insert("strlen".to_owned(), vec![p("string", Some("string"))]);
        params.insert("dechex".to_owned(), vec![p("num", Some("int"))]);
        params.insert("abs".to_owned(), vec![p("num", Some("int|float"))]);
        params.insert(
            "file_get_contents".to_owned(),
            vec![p("filename", Some("string")), optional("use_include_path", Some("bool"))],
        );
        params.insert(
            "preg_match".to_owned(),
            vec![
                p("pattern", Some("string")),
                p("subject", Some("string")),
                by_ref("matches", None),
            ],
        );
        params.insert(
            "sprintf".to_owned(),
            vec![p("format", Some("string")), variadic("values", Some("mixed"))],
        );
        params.insert(
            "var_dump".to_owned(),
            vec![p("value", Some("mixed")), variadic("values", Some("mixed"))],
        );
        params.insert(
            "str_replace".to_owned(),
            vec![
                p("search", Some("array|string")),
                p("replace", Some("array|string")),
                p("subject", Some("string|array")),
                by_ref("count", Some("mixed")),
            ],
        );
        params.insert("substr".to_owned(), vec![
            p("string", Some("string")),
            p("offset", Some("int")),
            optional("length", Some("?int")),
        ]);
        params.insert("is_int".to_owned(), vec![p("value", Some("mixed"))]);
        Mock { params, boot: false }
    }

    /// The sound subset: `--no-php`, a spawn failure, a replay table recorded
    /// before the field. Every gate below it answers `None`.
    fn silent() -> Mock {
        Mock { params: HashMap::new(), boot: false }
    }

    /// The same engine with the boot surface open, for the arity fixture.
    fn booted() -> Mock {
        Mock { boot: true, ..Mock::sidecar() }
    }
}

impl Folder for Mock {
    fn fold(&mut self, _name: &str, _args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        self.boot
    }
    fn boot_surface_function(&mut self, fqn: &str) -> Option<bool> {
        // Exactly the names this engine has: a project function is not a homonym
        // of one, and a builtin under test is resident.
        self.boot.then(|| self.params.contains_key(&fqn.to_ascii_lowercase()))
    }
    fn builtin_param_types(&mut self, name: &str) -> Option<Vec<BuiltinParam>> {
        self.params.get(&name.to_ascii_lowercase()).cloned()
    }
}

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

fn findings_with(src: &str, folder: &mut dyn Folder) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "t.php", folder)
}

fn findings(src: &str) -> Vec<Diagnostic> {
    findings_with(src, &mut Mock::sidecar())
}

/// The ids a fixture emits, `untyped.*` excluded — these sources declare bare
/// signatures on purpose and a contract-layer id on a missing type is not this
/// judgment speaking.
fn ids(src: &str) -> Vec<String> {
    findings(src)
        .into_iter()
        .filter(|d| !d.id.starts_with("untyped."))
        .map(|d| d.id.to_owned())
        .collect()
}

/// A file in coercive mode (no `declare`), the default calling convention.
fn coercive(body: &str) -> String {
    format!("<?php\n{body}")
}

fn strict(body: &str) -> String {
    format!("<?php\ndeclare(strict_types=1);\n{body}")
}

/// The single `type.argument-mismatch` message a fixture produces.
fn one_message(src: &str) -> String {
    let ds: Vec<Diagnostic> = findings(src).into_iter().filter(|d| d.id == ID).collect();
    assert_eq!(ds.len(), 1, "expected exactly one {ID}, got {ds:?}");
    ds[0].message.clone()
}

fn silent(src: &str) {
    let ds = ids(src);
    assert!(ds.is_empty(), "expected silence, got {ds:?} for:\n{src}");
}

// ---------------------------------------------------------------------------
// The flagship: `strlen`, both modes, three values
// ---------------------------------------------------------------------------

#[test]
fn strict_strlen_of_an_int_is_a_proven_type_error() {
    let src = strict("strlen(1);\n");
    assert_eq!(
        one_message(&src),
        "argument 1 to strlen() cannot become string $string — proven TypeError (strict mode)",
    );
}

#[test]
fn strlen_of_a_string_is_silent_in_both_modes() {
    silent(&strict("strlen('a');\n"));
    silent(&coercive("strlen('a');\n"));
}

#[test]
fn coercive_strlen_of_an_int_is_silent_because_php_coerces_it() {
    silent(&coercive("strlen(1);\n"));
}

#[test]
fn strict_strlen_of_null_reports_and_coercive_strlen_of_null_does_not() {
    // ADR-0056 §9.3, the one internal/userland difference in the table: coercive
    // mode deprecates rather than fatals, and a deprecation is not a finding here.
    let src = strict("strlen(null);\n");
    assert_eq!(
        one_message(&src),
        "argument null to strlen() cannot become string $string — proven TypeError (strict mode)",
    );
    silent(&coercive("strlen(null);\n"));
}

#[test]
fn the_carve_out_is_the_internal_boundary_and_not_the_value() {
    // The same `null` into a USERLAND `string` parameter fatals in coercive mode
    // too, and still reports — nothing about the carve-out reaches that arm.
    let src = coercive("function f(string $s): void {}\nf(null);\n");
    assert_eq!(
        findings(&src).iter().filter(|d| d.id == ID).count(),
        1,
        "a userland null is a TypeError in coercive mode: {:?}",
        findings(&src),
    );
}

#[test]
fn a_propagated_variable_reaches_the_builtin_arm_too() {
    // Not just literals: the builtin arm consumes the SAME resolution the project
    // arm does, so a proven binding crosses.
    let src = strict("$n = 1;\nstrlen($n);\n");
    assert_eq!(
        one_message(&src),
        "argument 1 (from $n, assigned at line 3) to strlen() cannot become string $string \
         — proven TypeError (strict mode)",
    );
}

// ---------------------------------------------------------------------------
// The declines
// ---------------------------------------------------------------------------

#[test]
fn a_by_reference_position_declines() {
    // `preg_match`'s `$matches` is an out-parameter: what PHP wants there is a
    // variable, not a value of a type. The two typed positions before it are
    // still judged, which is what makes this a decline and not a dead arm.
    silent(&strict("$m = 1;\npreg_match('/a/', 'a', $m);\n"));
    assert_eq!(ids(&strict("preg_match(1, 'a', $m);\n")), vec![ID]);
}

#[test]
fn a_variadic_position_and_everything_after_it_declines() {
    silent(&strict("sprintf('%d', 1, 2);\n"));
    // …while the format position in front of it is judged.
    assert_eq!(ids(&strict("sprintf(1, 2);\n")), vec![ID]);
}

#[test]
fn a_mixed_position_declines() {
    // `mixed` is the total envelope: it refuses nothing, so there is nothing to
    // judge against.
    silent(&strict("is_int(1);\n"));
    silent(&strict("is_int(null);\n"));
    // `var_dump`'s first position is `mixed` too; it is asserted separately
    // because Steins also reads `var_dump` as a dump surface (ADR-0053), so the
    // fixture has a `debug.*` finding that is not this judgment speaking.
    let ds = findings(&strict("var_dump(1);\nvar_dump(null);\n"));
    assert!(
        ds.iter().all(|d| d.id.starts_with("debug.")),
        "a mixed position judges nothing: {ds:?}",
    );
}

#[test]
fn an_untyped_position_declines() {
    // `preg_match`'s `$matches` carries no type at all — the by-ref decline above
    // is reached first, and this pins the type-side reason independently.
    let mut folder = Mock::sidecar();
    let src = strict("preg_match('/a/', 'a', 1);\n");
    let ds = findings_with(&src, &mut folder);
    assert!(ds.iter().all(|d| d.id != ID), "an untyped position judges nothing: {ds:?}");
}

#[test]
fn a_position_whose_type_the_native_relation_does_not_model_declines() {
    // `str_replace`'s three `array|string` positions decline exactly as
    // `array|string $x` written in a project signature does — `NativeType` has no
    // array member, and inventing one here would be a second coercion table.
    silent(&strict("str_replace([], 1, 1);\n"));
    silent(&coercive("str_replace([], 1, 1);\n"));
}

#[test]
fn a_name_the_folder_does_not_answer_declines() {
    // `strtoupper` is not in the mock: an unloaded extension, a name this engine
    // does not have, or the whole sound subset.
    silent(&strict("strtoupper(1);\n"));
    let src = strict("strlen(1);\n");
    let ds = findings_with(&src, &mut Mock::silent());
    assert!(ds.iter().all(|d| d.id != ID), "the sound subset judges nothing: {ds:?}");
}

#[test]
fn named_arguments_to_a_builtin_decline() {
    // v1: name→position binding for an internal target is its own slice, and a
    // named argument breaks the map this judgment is indexed by.
    silent(&strict("strlen(string: 1);\n"));
    // The positional prefix of a mixed call declines with it — one refusal, whole
    // call, so no half-mapped position is ever judged.
    silent(&strict("substr(1, offset: 0);\n"));
}

#[test]
fn argument_unpacking_declines() {
    silent(&strict("$a = [1];\nstrlen(...$a);\n"));
}

#[test]
fn a_project_function_shadowing_the_name_is_not_the_builtin() {
    // What runs is the project's own function, whose (untyped) parameter accepts
    // an int — judging it against the engine's `strlen` would convict working code.
    silent(&strict("function strlen($x) { return 1; }\nstrlen(1);\n"));
}

// ---------------------------------------------------------------------------
// The possibly pair on a builtin (issues #391/#418)
// ---------------------------------------------------------------------------

#[test]
fn the_possibly_pair_fires_on_a_builtin_argument() {
    // `realpath()` is `string|false` (the ADR-0069 declared-return floor), and
    // `file_get_contents` wants a `string`: the `false` arm raises.
    let src = strict("function f(string $p): void {\n  file_get_contents(realpath($p));\n}\n");
    let ds: Vec<Diagnostic> = findings(&src)
        .into_iter()
        .filter(|d| {
            d.id == PHPDOC_MAYBE_ARGUMENT_MISMATCH_ID || d.id == TYPE_MAYBE_ARGUMENT_MISMATCH_ID
        })
        .collect();
    assert_eq!(ds.len(), 1, "expected the possibly pair on the builtin, got {ds:?}");
    // `Asserted`, because the premise came off the ADR-0069 catalog floor: the
    // contract-layer spelling of the pair, never the proof-layer one.
    assert_eq!(ds[0].id, PHPDOC_MAYBE_ARGUMENT_MISMATCH_ID);
    assert_eq!(
        ds[0].message,
        "argument realpath() to file_get_contents() may not become string $filename \
         — realpath() is non-empty-string|false, and its false arm raises a TypeError \
         (strict mode)",
    );
}

#[test]
fn the_guarded_twin_is_silent() {
    let src = strict(
        "function f(string $p): void {\n\
         \x20 $r = realpath($p);\n\
         \x20 if ($r !== false) {\n\
         \x20   file_get_contents($r);\n\
         \x20 }\n\
         }\n",
    );
    silent(&src);
}

#[test]
fn the_possibly_pair_null_arm_takes_the_carve_out_too() {
    // `?string` into a coercive builtin `string`: the `null` arm is the ONLY
    // rejected one, so with the carve-out nothing is rejected and the pair is
    // silent. Strict mode keeps it.
    let body = "function f(?string $p): void {\n  strlen($p);\n}\n";
    silent(&coercive(body));
    let ds = ids(&strict(body));
    assert_eq!(ds, vec![TYPE_MAYBE_ARGUMENT_MISMATCH_ID], "strict keeps the null arm");
}

// ---------------------------------------------------------------------------
// Arity is untouched
// ---------------------------------------------------------------------------

#[test]
fn userland_arity_still_reports_beside_the_new_arm() {
    // `call.too-few-arguments` is the userland arity check, and nothing here moved
    // it: the fixture calls a project function short and a builtin correctly.
    // Needs the boot surface open (the A2ii homonym leg), which is what the arity
    // check has always needed — the new arm changed nothing about that.
    let src = strict("function g(int $a, int $b): void {}\ng(1);\nstrlen('a');\n");
    let ds: Vec<String> = findings_with(&src, &mut Mock::booted())
        .into_iter()
        .filter(|d| !d.id.starts_with("untyped."))
        .map(|d| d.id.to_owned())
        .collect();
    assert_eq!(ds, vec!["call.too-few-arguments"]);
}

// ---------------------------------------------------------------------------
// The measured grid: Steins against `php`, cell for cell (ADR-0056 §9.3)
// ---------------------------------------------------------------------------

struct Cell {
    function: String,
    class: String,
    literal: String,
    php_errors: bool,
}

fn internal_grid(mode: &str) -> Vec<Cell> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../harness/coercion-grid")
        .join(format!("witness-internal-{mode}.tsv"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("internal witness grid unreadable at {}: {e}", path.display()));
    let cells: Vec<Cell> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let c: Vec<&str> = line.split('\t').collect();
            assert!(c.len() >= 6, "malformed witness row: {line}");
            assert_eq!(c[0], mode, "row belongs to another mode: {line}");
            Cell {
                function: c[1].to_owned(),
                class: c[3].to_owned(),
                literal: c[4].to_owned(),
                php_errors: match c[5] {
                    "TypeError" => true,
                    "accept" => false,
                    other => panic!("unknown verdict {other:?} in {line}"),
                },
            }
        })
        .collect();
    assert_eq!(cells.len(), 27, "the internal grid is 9 witnesses x 3 functions");
    cells
}

/// Whether Steins convicts `<function>(<literal>)` in `mode`.
fn steins_errors(mode: &str, function: &str, literal: &str) -> bool {
    let src = if mode == "strict" {
        strict(&format!("{function}({literal});\n"))
    } else {
        coercive(&format!("{function}({literal});\n"))
    };
    let ds = findings(&src);
    let other: Vec<&Diagnostic> =
        ds.iter().filter(|d| d.id != ID && !d.id.starts_with("untyped.")).collect();
    assert!(other.is_empty(), "the grid cell emitted an unrelated finding: {other:?}");
    ds.iter().any(|d| d.id == ID)
}

/// The cells PHP rejects and Steins deliberately does not — the same `array` row
/// the userland grid carries (`is_type_error` answers `false` for an array by
/// construction; the phpdoc contract relation owns that mismatch).
fn is_known_divergence(class: &str) -> bool {
    class == "array"
}

fn agrees(mode: &str) {
    let mut divergent = Vec::new();
    for c in internal_grid(mode) {
        let steins = steins_errors(mode, &c.function, &c.literal);
        if steins == c.php_errors {
            continue;
        }
        assert!(
            !steins,
            "{mode}: Steins convicts {}({}) where PHP accepts it — a false positive, \
             never an admissible divergence",
            c.function, c.literal
        );
        assert!(is_known_divergence(&c.class), "new divergence in {mode} mode: {}/{}", c.function, c.class);
        divergent.push(format!("{}/{}", c.function, c.class));
    }
    assert_eq!(divergent.len(), 3, "the known silence is one array cell per function: {divergent:?}");
}

#[test]
fn the_strict_internal_grid_agrees_with_php_cell_for_cell() {
    agrees("strict");
}

// ---------------------------------------------------------------------------
// The live lane: the same verdict off a real `php`, no mock anywhere
// ---------------------------------------------------------------------------

/// A live sidecar folder, or `None` when `php` cannot be reached — in which case
/// the caller skips loudly rather than asserting something vacuous.
fn live(test: &str) -> Option<SidecarFolder> {
    let mut folder = SidecarFolder::enabled();
    if folder.builtin_param_types("strlen").is_none() {
        eprintln!("SKIP {test}: no reflection engine — is `php` on PATH?");
        return None;
    }
    Some(folder)
}

#[test]
fn a_live_php_answers_the_same_verdict_the_mock_does() {
    let Some(mut folder) = live("a_live_php_answers_the_same_verdict_the_mock_does") else {
        return;
    };
    // The signature the whole file is written against, straight off the engine.
    let params = folder.builtin_param_types("STRLEN").expect("the probe answered");
    assert_eq!(params.len(), 1, "strlen(string $string): {params:?}");
    assert_eq!(params[0].name, "string");
    assert_eq!(params[0].ty.as_deref(), Some("string"));

    let reported = |src: &str, folder: &mut SidecarFolder| {
        findings_with(src, folder).into_iter().any(|d| d.id == ID)
    };
    assert!(reported(&strict("strlen(1);\n"), &mut folder));
    assert!(!reported(&coercive("strlen(1);\n"), &mut folder));
    assert!(reported(&strict("strlen(null);\n"), &mut folder));
    assert!(!reported(&coercive("strlen(null);\n"), &mut folder), "the §9.3 carve-out");
    assert!(!reported(&strict("strlen('a');\n"), &mut folder));
    // The declines, off the engine's own list rather than a written-down one.
    assert!(!reported(&strict("sprintf('%d', 1);\n"), &mut folder), "a variadic tail");
    assert!(!reported(&strict("preg_match('/a/', 'a', $m);\n"), &mut folder), "a by-ref position");
    assert!(!reported(&strict("str_replace([], 1, 1);\n"), &mut folder), "an array|string position");
}

#[test]
fn the_coercive_internal_grid_agrees_with_php_cell_for_cell() {
    agrees("coercive");
}
