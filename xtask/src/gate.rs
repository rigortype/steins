//! `fp-gate`: run the full proof-layer pipeline over the pinned corpus.
//!
//! ADR-0013: one proof-layer diagnostic on working code is a release blocker,
//! so this gate exits nonzero the moment any diagnostic fires on a clean-parsing
//! file — that is exactly the triage material we want surfaced, never hidden.
//!
//! Whole-project mode (ADR-0009/0015): each corpus package is analyzed as ONE
//! project (a single salsa DB holding all its `.php` files), so cross-file
//! calls, class chains, and effects resolve. Packages run in parallel (rayon);
//! within a package the analysis is one project run. Files that fail to parse
//! are still included in the project (so resolution stays complete — a partial
//! tree can only *silence*, never add a false positive), but any diagnostic that
//! lands in a parse-error file is excluded from the gate count.

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};

use rayon::prelude::*;
use steins_db::{Project, SourceFile, SteinsDatabase, parse};
use steins_infer::{Diagnostic, SidecarFolder, check_project, is_vendor_path};

use crate::corpus::{PACKAGES, checkout_dir, collect_php_files, read_lock, repo_root};
use crate::corpus_local::{self, LocalProject};

/// Per-project result of the gate run (a pinned corpus package or an unpinned
/// local project). `diagnostics` holds only the findings that count against the
/// gate; for local projects, vendor findings are excluded (ADR-0015) and tallied
/// separately in `vendor_suppressed`.
struct PackageReport {
    name: String,
    /// The pinned release tag, or empty for a local (unpinned) project.
    tag: String,
    /// A live working tree injected via `corpus.local.toml` (ADR-0013 §4).
    local: bool,
    file_count: usize,
    parse_error_files: Vec<String>,
    diagnostics: Vec<Diagnostic>,
    /// NEW `phpdoc.*` declared-contract findings, held separately: in this run
    /// they are **measurement mode** (ADR-0030 relation #1 landing) — reported and
    /// counted per package but excluded from the red/green verdict.
    phpdoc: Vec<Diagnostic>,
    /// `throw.*` findings (ADR-0040/0007), held in the same **measurement mode**
    /// as `phpdoc.*`: they are contract-layer claims about the code's own
    /// `@throws` documentation (an undeclared checked throw, a Liskov-widened
    /// override), never runtime-breakage — TRUE ones abound in working code
    /// (the checked-exception volume ADR-0007 keeps quiet by default), so they
    /// gate only as a per-package increase tripwire.
    throws: Vec<Diagnostic>,
    /// Triaged TRUE runtime-layer positives (real broken corpus code Steins
    /// correctly proves; see [`EXPECTED_PROOF_FINDINGS`]). Reported prominently
    /// but excluded from the red/green verdict — matched at finding precision so
    /// any drift falls back into `diagnostics` and reds the gate.
    expected_true: Vec<Diagnostic>,
    /// Vendor findings suppressed from the gate count (local projects only).
    vendor_suppressed: usize,
    elapsed: Duration,
}

/// Whether a diagnostic is one of the measurement-mode `phpdoc.*` ids.
fn is_phpdoc(d: &Diagnostic) -> bool {
    d.id.starts_with("phpdoc.")
}

/// Whether a diagnostic is one of the measurement-mode `throw.*` ids (ADR-0040).
fn is_throw(d: &Diagnostic) -> bool {
    d.id.starts_with("throw.")
}

/// Permanent gate policy for `phpdoc.*` findings (ADR-0030 relation #1).
///
/// `phpdoc.*` findings are **contract-layer** claims: they say a proven value does
/// not inhabit a *declared* `@param`/`@return` type under the no-coercion contract
/// relation. That is a statement about the code's own documentation, **not** a
/// runtime-breakage claim (`type.*`/`effect.*`, which gate red on sight per
/// ADR-0013). TRUE `phpdoc.*` findings legitimately exist in released, working
/// corpus code — a `@param int` that a test calls with the numeric string `"5"` is
/// a real declared-contract violation even though it runs fine — so they must
/// never flip the gate red merely by existing.
///
/// Instead the gate tracks their **count per package** against this deliberately
/// hand-maintained expected-count table and acts as a **regression tripwire**: a
/// package goes red only if its `phpdoc.*` count *increases* beyond the seeded
/// expectation (a genuine new finding, or a real regression in the checker),
/// while a *decrease* is a welcome improvement that never blocks. Update an entry
/// here consciously when a change to the checker legitimately moves a count.
///
/// Seeded with the post-assertion-exemption counts (the assertion-helper exemption
/// removed ~19 monorepo findings vs. the pre-exemption 352). Packages absent from
/// this table expect **zero** `phpdoc.*` findings.
const PHPDOC_EXPECTED: &[(&str, usize)] = &[
    ("composer/composer", 19),
    ("sebastianbergmann/phpunit", 8),
    ("Seldaek/monolog", 4),
    // 1 → 2 (+1) with ADR-0043 stage 4 (phpdoc-side class contracts). The new
    // finding is a class-value contract: `new MountManager(['valid' => 'something
    // else'])` — a plain string in the `array<string, FilesystemOperator>` value
    // position — inside a `guarding_against_mounting_invalid_filesystems` test that
    // wraps it in `expectException(UnableToMountFilesystem::class)` and carries
    // `@phpstan-ignore-next-line`. A TRUE no-coercion violation the test documents.
    ("thephpleague/flysystem", 2),
    // 0 → 1 (+1) with ADR-0043 stage 4. `ChoiceQuestionTest` passes a literal array
    // `[..., null]` to `ChoiceQuestion::__construct(@param array<string|bool|int|
    // float|\Stringable> $choices)`; `null` is a member of none of the union arms —
    // a TRUE no-coercion contract violation (the docblock omits null). The sibling
    // `StringChoice` (a `__toString` object, implicit `\Stringable`) is correctly
    // *accepted*, not a finding — the is-a oracle honors the implicit interface.
    ("symfony/console", 1),
    // 0 → 15 (+15) with ADR-0043 stage 4. Every finding is a deliberate
    // negative-test call site (`expectException(\LogicException::class)` /
    // `\PhpParser\...`) passing a wrong-typed argument to a class-typed `@param`:
    // `new Name()` vs `(string|Identifier|Expr)` (Name is-a-No either), scalar `1`
    // /`"test"` vs `(Node|Builder)` / `(string|Identifier)`, `new stdClass()` vs a
    // `\UnitEnum`-bearing union. All in `test/PhpParser/Builder*Test.php` and
    // `NodeDumperTest.php`; each asserts the runtime `LogicException` that the
    // phpdoc contract predicts — TRUE, released, working test code.
    ("nikic/PHP-Parser", 15),
    // The private monorepo (corpus.local.toml); matched by its local project name.
    // 333 → 357 (+24) with ADR-0031 branch-sensitive analysis: the structured `if`
    // walk, ternary values, and positive refinement now reach proven values that
    // were previously buried inside `Opaque` control-flow blocks, so the phpdoc
    // contract layer sees more of them.
    //
    // 357 → 404 (+47) with the ADR-0035 "refined layer goes live" milestone: the
    // env now stores the four-layer `steins_domain::Fact`, and three new sound
    // inference sources feed the contract layer — native-type parameter *seeding*
    // (`int $x` ⇒ `General{Int}`), guard *refinements* that produce Refined/General
    // facts (`$n > 0` ⇒ positive-int, `$s !== ''` ⇒ non-empty-string), and
    // `@phpstan-assert` *application* — checked via `steins_contract::admits_fact`
    // (only a definite `No` reports). 8 of the increase are the new abstract-fact
    // findings (a seeded/refined scalar flowing into an incompatible `@param`, e.g.
    // positive-int → `@param string`, non-empty-string → `@param int`, int →
    // `@param string`); the rest are concrete values the richer propagation now
    // reaches. All sampled increases are TRUE no-coercion contract violations in
    // released test code, never runtime findings — the runtime gate stays GREEN.
    // Class-shaped `@param`s are held silent against scalar facts (template safety),
    // so no template FPs. Baseline moved deliberately per ADR-0030/0035.
    //
    // 404 → 405 (+1) with the ADR-0036 object-state milestone: the new
    // `phpdoc.property-mismatch` check (a proven/abstract value assigned to a
    // property whose `@var` contract definitely rejects it). The single pxxxx
    // increase is a TRUE finding — a model class's `$id` property is `@var
    // numeric-string`, and a test assigns an int literal to it (a value that
    // is not a numeric *string*); PHPStan flags the identical `assign.propertyType`.
    // Property checks run only in the plain per-scope pass (never under a binding
    // descent, whose caller values in-body guards would narrow), so the descent-bound
    // guard-blind candidates seen mid-development do not reach the gate.
    //
    // 405 → 439 (+34) with ADR-0043 stage 4 (phpdoc-side class contracts + the
    // enum-case/class-const value resolution that feeds them). The delta was
    // baseline-diffed (a HEAD worktree) and triaged verbatim; all 34 net-new (36
    // added, 2 pre-existing FPs removed) are TRUE:
    //   - class-const string args vs `@param int`/`int[]` (a DAO's `TYPE_*`
    //     consts holding `"3"`-style numeric strings into `int`; a const list of
    //     numeric-string ids into `int[]`) — the stringly-typed DB-illusion
    //     pattern (ADR-0037), now that class-const args resolve to their literals.
    //   - proven scalars/objects vs a class-typed contract: a service-name string
    //     vs an enum param, an int literal vs a `SomeInterface|false` union,
    //     a float literal (an `@phpstan-ignore` intentional wrong type) vs a
    //     scalar|`BackedEnum` union, a prose string literal vs a `list<Model>`
    //     param, `null` assigned to a `@var PxxxxPDOCore` property on
    //     `disconnect()`.
    //   - sealed array-shape violations surfaced once a value became provable (its
    //     class-const/`::class`/enum elements now resolve): two finder methods'
    //     options arrays carrying a key their `@param array{…}` omits; a
    //     data-provider `expected => SomeException::class` (a *string*) where
    //     the `@return array<…, array<untyped>>` wants an array; a
    //     metadata-defaults const carrying an extra key vs its `@return array{…}`.
    // The 2 removed are pre-existing FPs the stage cleared: an unresolved const-fetch
    // *type* (`SomeClass::LIST_*`) no longer manufactures a No against an array
    // value (const-fetch types are silent), and a `[]`-vs-`non-empty-list` finding a
    // `count()===0`-guarded value could never actually reach. Runtime layer GREEN.
    ("pxxxx-monorepo", 439),
];

/// The expected `phpdoc.*` count for a package/local-project name (0 if untabled).
fn phpdoc_expected(name: &str) -> usize {
    PHPDOC_EXPECTED.iter().find(|(n, _)| *n == name).map_or(0, |(_, c)| *c)
}

/// Permanent gate policy for `throw.*` findings (ADR-0040/0007), identical in
/// spirit to [`PHPDOC_EXPECTED`]: an undeclared **checked** throw escaping a
/// written `@throws`, or a Liskov-widened override, is a real contract-layer
/// claim about the code's own documentation — not a runtime-breakage proof. Such
/// findings legitimately saturate working code (the very checked-exception volume
/// ADR-0007 keeps quiet by default), so they are held in measurement mode and
/// gate only as a per-package **increase** tripwire.
///
/// Seeded from the first landing run of the throw system (ADR-0040). The
/// monorepo count is dominated by two pervasive base exceptions
/// (an assertion-failure base and the app-wide base exception) thrown far below `@throws`-
/// annotated controllers — all TRUE undeclared-checked-throw findings, none
/// runtime breakage. Update an entry consciously when a checker change moves a
/// count. Packages absent expect **zero**.
///
/// The pxxxx count rose 35614 → 43963 with the closure wave (ADR-0033): throws
/// now propagate through higher-order-builtin callbacks (`array_map(closure, …)`)
/// and body-local `$fn()` closures that were previously opaque taints. Triaged
/// (5-sample, verbatim): every new finding is a TRUE undeclared-checked-throw —
/// exclusively the two pervasive base exceptions reached through a real
/// callback edge (e.g. a controller method with `@throws ErrorException`
/// calling `array_map` over a closure whose callee throws the app-wide base
/// exception). No FP: the by-ref-invalidation guard keeps the
/// local `$fn()` resolution sound, and the public corpus packages are unmoved.
// Reconciled to actual after the closure-wave Stage D (interface/parent @throws
// Liskov + `implements` lowering). The moves were triaged and are deterministic:
// the increases are new `throw.liskov-widened` findings (phpunit +4, pxxxx +1 —
// e.g. JsonMatches::fail declares InvalidJsonException while the abstraction
// Constraint::fail declares only ExpectationFailedException: a true widening),
// and the decreases (symfony/console 12→10, nikic 2→1) are `undeclared` counts
// that dropped because lowering `implements` enriched the class chain, letting
// throw subtype/absorption checks resolve where they previously widened.
const THROW_EXPECTED: &[(&str, usize)] = &[
    ("composer/composer", 93),
    ("sebastianbergmann/phpunit", 84),
    ("guzzle/guzzle", 2),
    ("Seldaek/monolog", 7),
    ("symfony/console", 10),
    ("thephpleague/flysystem", 3),
    ("nikic/PHP-Parser", 1),
    ("pxxxx-monorepo", 43964),
];

/// The expected `throw.*` count for a package/local-project name (0 if untabled).
fn throw_expected(name: &str) -> usize {
    THROW_EXPECTED.iter().find(|(n, _)| *n == name).map_or(0, |(_, c)| *c)
}

/// A single **triaged TRUE proof-layer positive** the corpus legitimately
/// contains: real broken code that Steins correctly proves. Unlike the
/// measurement-mode `phpdoc.*`/`throw.*` families this is a *runtime-layer*
/// finding, where the standing bar is a strict **zero** (ADR-0013). An entry here
/// is not a weakening of that bar but a recorded, verbatim-triaged exception,
/// matched at **finding precision** (package + id + path + line + a message
/// fingerprint): any drift — a different line, a different message, a second
/// finding — no longer matches and re-reds the gate, so this can never mask a
/// future regression the way a bare count could.
struct ExpectedProofFinding {
    /// Package / local-project name the finding belongs to.
    package: &'static str,
    /// The diagnostic id (e.g. `type.argument-mismatch`).
    id: &'static str,
    /// A suffix of the finding's project-relative path.
    path_suffix: &'static str,
    /// The 1-based line.
    line: u32,
    /// A stable substring of the message (the acceptance fingerprint).
    message_contains: &'static str,
}

/// Triaged TRUE proof-layer positives (ADR-0043 §5 gate discipline). Each is a
/// place where real corpus code is genuinely wrong and Steins now proves it; the
/// triage lives in the comment beside the row. Adding a row is a conscious,
/// orchestrator-visible act — never a silent suppression.
const EXPECTED_PROOF_FINDINGS: &[ExpectedProofFinding] = &[
    // Surfaced by the ADR-0043 builtin-hierarchy ingestion (php-src mining): once
    // `stdClass` entered the closed hierarchy as a mined root (supers = []), the
    // is-a oracle can prove `stdClass` is-a-NO against every member of the
    // external union `MongoDB\Client|MongoDB\Driver\Manager`, so the definite-No
    // acceptance arm fires. The finding is in monolog's OWN test, which
    // deliberately constructs the invalid argument and asserts the resulting
    // TypeError:
    //   public function testConstructorShouldThrowExceptionForInvalidMongo() {
    //       $this->expectException(\TypeError::class);
    //       new MongoDBHandler(new \stdClass, 'db', 'collection');   // ← here
    //   }
    // against `__construct(Client|Manager $mongodb, …)` under `declare(strict_types=1)`.
    // Steins proves exactly the TypeError the test expects — a TRUE positive, not
    // an FP. (Verbatim triage in the ingestion session; sound because `stdClass`
    // has a fully-enumerated empty ancestor set and cannot be a subtype of either
    // external class.)
    ExpectedProofFinding {
        package: "Seldaek/monolog",
        id: "type.argument-mismatch",
        path_suffix: "tests/Monolog/Handler/MongoDBHandlerTest.php",
        line: 27,
        message_contains: "cannot become mongodb\\client|mongodb\\driver\\manager",
    },
];

/// Whether `d` is a recorded, triaged TRUE proof-layer positive for `package`
/// (see [`EXPECTED_PROOF_FINDINGS`]) — reported but excluded from the red/green
/// verdict. Matched at finding precision so any drift re-reds the gate.
fn is_expected_true_positive(package: &str, d: &Diagnostic) -> bool {
    EXPECTED_PROOF_FINDINGS.iter().any(|e| {
        e.package == package
            && e.id == d.id
            && e.line == d.line
            && d.path.ends_with(e.path_suffix)
            && d.message.contains(e.message_contains)
    })
}

/// Entry point for `cargo xtask fp-gate`. Returns `true` if the gate is GREEN
/// (no diagnostics on clean code).
pub fn run() -> Result<bool, String> {
    let lock = read_lock();
    if lock.packages.is_empty() {
        return Err("corpus.lock.toml is empty — run `cargo xtask corpus-sync` first".to_owned());
    }
    let root = repo_root();

    // One project per package; packages analyzed in parallel.
    let reports: Result<Vec<PackageReport>, String> = PACKAGES
        .par_iter()
        .map(|pkg| {
            let dir = checkout_dir(pkg.name);
            if !dir.is_dir() {
                return Err(format!(
                    "{} not checked out at {} — run `cargo xtask corpus-sync`",
                    pkg.name,
                    dir.display()
                ));
            }
            let tag = lock.get(pkg.name).map(|e| e.tag.clone()).unwrap_or_default();
            Ok(analyze_package(pkg.name, &tag, &dir, &root))
        })
        .collect();
    let mut reports = reports?;
    // Keep a stable (canonical corpus) order for the report.
    reports.sort_by_key(|r| PACKAGES.iter().position(|p| p.name == r.name).unwrap_or(usize::MAX));

    // Private-corpus injection point (ADR-0013 §4): each `[[project]]` in the
    // optional (gitignored) `corpus.local.toml` is analyzed as one project, in
    // parallel like the packages, applying the CLI's vendor default — vendor
    // files are indexed for inference but their findings don't count.
    let locals = corpus_local::read_local()?;
    let mut local_reports: Vec<PackageReport> =
        locals.par_iter().map(analyze_local).collect();
    local_reports.sort_by(|a, b| a.name.cmp(&b.name));

    // Measurement-mode regression tripwires (see `PHPDOC_EXPECTED` /
    // `THROW_EXPECTED`): a package regresses iff a count *exceeds* its seeded
    // expectation. Both `phpdoc.*` and `throw.*` are contract-layer.
    let regressions = phpdoc_regressions(&reports, &local_reports);
    let throw_regressions = measurement_regressions(&reports, &local_reports, "throw", |r| r.throws.len(), throw_expected);

    print_report(&reports, &local_reports, &regressions, &throw_regressions);

    // RED on any counted proof-layer finding — package diagnostics plus local
    // *non-vendor* diagnostics (vendor findings never gate; ADR-0015) — OR on any
    // measurement-mode count that has regressed past its expected baseline.
    let total_diags: usize = reports.iter().map(|r| r.diagnostics.len()).sum::<usize>()
        + local_reports.iter().map(|r| r.diagnostics.len()).sum::<usize>();
    Ok(total_diags == 0 && regressions.is_empty() && throw_regressions.is_empty())
}

/// One measurement-mode regression: a package whose count exceeds its expectation.
struct PhpdocRegression {
    name: String,
    actual: usize,
    expected: usize,
}

/// Compare every package's `phpdoc.*` count to its expected baseline, returning the
/// ones that have *increased* (the only direction that gates red).
fn phpdoc_regressions(
    reports: &[PackageReport],
    local_reports: &[PackageReport],
) -> Vec<PhpdocRegression> {
    measurement_regressions(reports, local_reports, "phpdoc", |r| r.phpdoc.len(), phpdoc_expected)
}

/// Generic measurement-mode tripwire: report packages whose `count` exceeds their
/// `expected` baseline (the only direction that gates red).
fn measurement_regressions(
    reports: &[PackageReport],
    local_reports: &[PackageReport],
    _family: &str,
    count: impl Fn(&PackageReport) -> usize,
    expected: impl Fn(&str) -> usize,
) -> Vec<PhpdocRegression> {
    reports
        .iter()
        .chain(local_reports.iter())
        .filter_map(|r| {
            let actual = count(r);
            let exp = expected(&r.name);
            (actual > exp).then(|| PhpdocRegression { name: r.name.clone(), actual, expected: exp })
        })
        .collect()
}

// Default posture (ADR-0004): the gate folds via the PHP sidecar. Each rayon
// worker owns one resident `SidecarFolder` (thread-local), reused across the
// packages that worker analyzes.
thread_local! {
    static FOLDER: RefCell<SidecarFolder> = RefCell::new(SidecarFolder::enabled());
}

/// Analyze one package as a single project and time it.
fn analyze_package(name: &str, tag: &str, dir: &Path, root: &Path) -> PackageReport {
    let start = Instant::now();

    let mut files = Vec::new();
    collect_php_files(dir, &mut files);
    files.sort();

    let db = SteinsDatabase::default();
    let mut inputs: Vec<SourceFile> = Vec::with_capacity(files.len());
    for f in &files {
        let rel = f.strip_prefix(root).unwrap_or(f).to_string_lossy().into_owned();
        let text = match std::fs::read(f) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(_) => String::new(), // unreadable → empty (parses clean, contributes nothing)
        };
        inputs.push(SourceFile::new(&db, rel, text));
    }

    // Identify parse-error files (their diagnostics are excluded from the count).
    let mut parse_error_files = Vec::new();
    for &input in &inputs {
        if !parse(&db, input).parse_errors().is_empty() {
            parse_error_files.push(input.path(&db).to_owned());
        }
    }
    let parse_err_set: HashSet<&str> = parse_error_files.iter().map(String::as_str).collect();

    let project = Project::new(&db, inputs);
    let mut diags: Vec<Diagnostic> = FOLDER.with(|f| check_project(&db, project, &mut *f.borrow_mut()));
    diags.retain(|d| !parse_err_set.contains(d.path.as_str()));
    diags.sort_by(|a, b| (&a.path, a.line, a.column).cmp(&(&b.path, b.line, b.column)));
    // Measurement-mode split: `phpdoc.*` and `throw.*` findings are reported +
    // counted but do not gate this run (only their increase tripwire does).
    let phpdoc: Vec<Diagnostic> = diags.iter().filter(|d| is_phpdoc(d)).cloned().collect();
    let throws: Vec<Diagnostic> = diags.iter().filter(|d| is_throw(d)).cloned().collect();
    diags.retain(|d| !is_phpdoc(d) && !is_throw(d));
    // Split off triaged TRUE runtime-layer positives (reported, not gated).
    let expected_true: Vec<Diagnostic> =
        diags.iter().filter(|d| is_expected_true_positive(name, d)).cloned().collect();
    diags.retain(|d| !is_expected_true_positive(name, d));

    PackageReport {
        name: name.to_owned(),
        tag: tag.to_owned(),
        local: false,
        file_count: files.len(),
        parse_error_files,
        diagnostics: diags,
        phpdoc,
        throws,
        expected_true,
        vendor_suppressed: 0,
        elapsed: start.elapsed(),
    }
}

/// Analyze one local project (ADR-0013 §4) as a single project. Paths are made
/// project-relative so the `vendor/` predicate and the report read cleanly.
/// Vendor findings are split out of the gate count (ADR-0015).
fn analyze_local(proj: &LocalProject) -> PackageReport {
    let start = Instant::now();
    let root = Path::new(&proj.path);

    let files = corpus_local::collect_php_files(root, &proj.exclude);

    let db = SteinsDatabase::default();
    let mut inputs: Vec<SourceFile> = Vec::with_capacity(files.len());
    for f in &files {
        // Project-relative path (falls back to the full path if `f` is not under
        // `root`, which cannot normally happen). Keeps `vendor/` detection and
        // the printed rows readable.
        let rel = f.strip_prefix(root).unwrap_or(f).to_string_lossy().into_owned();
        let text = match std::fs::read(f) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(_) => String::new(),
        };
        inputs.push(SourceFile::new(&db, rel, text));
    }

    let mut parse_error_files = Vec::new();
    for &input in &inputs {
        if !parse(&db, input).parse_errors().is_empty() {
            parse_error_files.push(input.path(&db).to_owned());
        }
    }
    let parse_err_set: HashSet<&str> = parse_error_files.iter().map(String::as_str).collect();

    let project = Project::new(&db, inputs);
    let mut diags: Vec<Diagnostic> = FOLDER.with(|f| check_project(&db, project, &mut *f.borrow_mut()));
    diags.retain(|d| !parse_err_set.contains(d.path.as_str()));

    // Vendor default (ADR-0015): vendor code was fully indexed and inferred, but
    // its findings do not count against the gate. Split them out.
    let before = diags.len();
    diags.retain(|d| !is_vendor_path(&d.path));
    let vendor_suppressed = before - diags.len();
    diags.sort_by(|a, b| (&a.path, a.line, a.column).cmp(&(&b.path, b.line, b.column)));
    // Measurement-mode split (first-party only; vendor already removed above).
    let phpdoc: Vec<Diagnostic> = diags.iter().filter(|d| is_phpdoc(d)).cloned().collect();
    let throws: Vec<Diagnostic> = diags.iter().filter(|d| is_throw(d)).cloned().collect();
    diags.retain(|d| !is_phpdoc(d) && !is_throw(d));
    // Split off triaged TRUE runtime-layer positives (reported, not gated).
    let expected_true: Vec<Diagnostic> =
        diags.iter().filter(|d| is_expected_true_positive(&proj.name, d)).cloned().collect();
    diags.retain(|d| !is_expected_true_positive(&proj.name, d));

    PackageReport {
        name: proj.name.clone(),
        tag: String::new(),
        local: true,
        file_count: files.len(),
        parse_error_files,
        diagnostics: diags,
        phpdoc,
        throws,
        expected_true,
        vendor_suppressed,
        elapsed: start.elapsed(),
    }
}

fn print_report(
    reports: &[PackageReport],
    local_reports: &[PackageReport],
    regressions: &[PhpdocRegression],
    throw_regressions: &[PhpdocRegression],
) {
    println!("\n=== fp-gate: per-package findings ===\n");
    if !local_reports.is_empty() {
        println!(
            "note: {} local project(s) are UNPINNED live working trees (corpus.local.toml, \
             ADR-0013 §4); their vendor findings are indexed for inference but do not gate \
             (ADR-0015).\n",
            local_reports.len()
        );
    }
    // Packages first, then local projects, in the per-project findings section.
    for r in reports.iter().chain(local_reports.iter()) {
        let ident = if r.local {
            format!("{} (local)", r.name)
        } else {
            format!("{} @ {}", r.name, r.tag)
        };
        let vendor_note = if r.local {
            format!(", {} vendor-suppressed", r.vendor_suppressed)
        } else {
            String::new()
        };
        println!(
            "{ident} — {} files, {} parse-error files, {} diagnostics{vendor_note} ({:.2}s)",
            r.file_count,
            r.parse_error_files.len(),
            r.diagnostics.len(),
            r.elapsed.as_secs_f64()
        );
        if !r.parse_error_files.is_empty() {
            for sample in r.parse_error_files.iter().take(5) {
                println!("    parse-error: {sample}");
            }
            if r.parse_error_files.len() > 5 {
                println!("    … and {} more", r.parse_error_files.len() - 5);
            }
        }
        for d in &r.diagnostics {
            println!("    DIAGNOSTIC {}:{}:{} [{}] {}", d.path, d.line, d.column, d.id, d.message);
        }
        if !r.expected_true.is_empty() {
            println!(
                "    [expected TRUE positive] {} triaged real-bug finding(s) (excluded from red/green — see EXPECTED_PROOF_FINDINGS):",
                r.expected_true.len()
            );
            for d in &r.expected_true {
                println!("    TRUE-POSITIVE {}:{}:{} [{}] {}", d.path, d.line, d.column, d.id, d.message);
            }
        }
        if !r.phpdoc.is_empty() {
            println!("    [measurement mode] {} phpdoc.* finding(s) (excluded from red/green):", r.phpdoc.len());
            for d in &r.phpdoc {
                println!("    PHPDOC {}:{}:{} [{}] {}", d.path, d.line, d.column, d.id, d.message);
            }
        }
    }

    // Measurement-mode summary: the `phpdoc.*` declared-contract ids, counted per
    // package against the `PHPDOC_EXPECTED` baseline. These do NOT gate on their
    // own existence (TRUE contract-layer findings live in released code, ADR-0030);
    // a package gates red only if its count *increased* past the baseline.
    let total_phpdoc: usize = reports.iter().chain(local_reports.iter()).map(|r| r.phpdoc.len()).sum();
    let total_expected: usize = PHPDOC_EXPECTED.iter().map(|(_, c)| *c).sum();
    println!("\n=== phpdoc.* measurement mode (contract layer — gates only on INCREASE) ===\n");
    for r in reports.iter().chain(local_reports.iter()) {
        let expected = phpdoc_expected(&r.name);
        if r.phpdoc.is_empty() && expected == 0 {
            continue;
        }
        let label = if r.local { format!("{} (local)", r.name) } else { r.name.clone() };
        let (params, returns) = r
            .phpdoc
            .iter()
            .fold((0usize, 0usize), |(p, ret), d| match d.id {
                "phpdoc.param-mismatch" => (p + 1, ret),
                "phpdoc.return-mismatch" => (p, ret + 1),
                _ => (p, ret),
            });
        let actual = r.phpdoc.len();
        let marker = match actual.cmp(&expected) {
            std::cmp::Ordering::Greater => "  ⬆ REGRESSION (exceeds expected)",
            std::cmp::Ordering::Less => "  ⬇ improved (below expected — update baseline when intentional)",
            std::cmp::Ordering::Equal => "",
        };
        println!(
            "{label} — {actual} phpdoc.* ({params} param, {returns} return) [expected {expected}]{marker}"
        );
    }
    println!("phpdoc.* TOTAL: {total_phpdoc} (expected baseline {total_expected})");
    if regressions.is_empty() {
        println!("phpdoc.* tripwire: OK — no package exceeds its expected baseline.");
    } else {
        println!("phpdoc.* tripwire: TRIPPED — the following packages regressed:");
        for reg in regressions {
            println!("    {} — {} > expected {}", reg.name, reg.actual, reg.expected);
        }
    }

    // Measurement-mode summary for the `throw.*` contract-layer ids (ADR-0040):
    // counted per package against `THROW_EXPECTED`, gating only on INCREASE. The
    // volume is far larger than `phpdoc.*` (checked-exception saturation), so only
    // per-package counts and a small sample print — never every finding.
    let total_throw: usize = reports.iter().chain(local_reports.iter()).map(|r| r.throws.len()).sum();
    let total_throw_expected: usize = THROW_EXPECTED.iter().map(|(_, c)| *c).sum();
    println!("\n=== throw.* measurement mode (contract layer — gates only on INCREASE) ===\n");
    for r in reports.iter().chain(local_reports.iter()) {
        let expected = throw_expected(&r.name);
        if r.throws.is_empty() && expected == 0 {
            continue;
        }
        let label = if r.local { format!("{} (local)", r.name) } else { r.name.clone() };
        let (undecl, liskov) = r.throws.iter().fold((0usize, 0usize), |(u, l), d| match d.id {
            "throw.undeclared" => (u + 1, l),
            "throw.liskov-widened" => (u, l + 1),
            _ => (u, l),
        });
        let actual = r.throws.len();
        let marker = match actual.cmp(&expected) {
            std::cmp::Ordering::Greater => "  ⬆ REGRESSION (exceeds expected)",
            std::cmp::Ordering::Less => "  ⬇ improved (below expected — update baseline when intentional)",
            std::cmp::Ordering::Equal => "",
        };
        println!(
            "{label} — {actual} throw.* ({undecl} undeclared, {liskov} liskov) [expected {expected}]{marker}"
        );
        // A tiny sample so a regression is triageable without a 35k-line dump.
        if actual > expected {
            for d in r.throws.iter().take(3) {
                println!("    THROW {}:{}:{} [{}] {}", d.path, d.line, d.column, d.id, d.message);
            }
        }
    }
    println!("throw.* TOTAL: {total_throw} (expected baseline {total_throw_expected})");
    if throw_regressions.is_empty() {
        println!("throw.* tripwire: OK — no package exceeds its expected baseline.");
    } else {
        println!("throw.* tripwire: TRIPPED — the following packages regressed:");
        for reg in throw_regressions {
            println!("    {} — {} > expected {}", reg.name, reg.actual, reg.expected);
        }
    }

    // Summary table: packages and local projects share one table; local rows are
    // marked `(local)`.
    let rows = || reports.iter().chain(local_reports.iter());
    let name_w = rows()
        .map(|r| r.name.len() + if r.local { " (local)".len() } else { 0 })
        .max()
        .unwrap_or(4)
        .max(7);
    println!("\n=== summary ===\n");
    println!(
        "{:<name_w$}  {:>6}  {:>12}  {:>11}  {:>8}",
        "package", "files", "parse-errors", "diagnostics", "time(s)"
    );
    println!("{}", "-".repeat(name_w + 2 + 6 + 2 + 12 + 2 + 11 + 2 + 8));
    let (mut tf, mut tp, mut td) = (0usize, 0usize, 0usize);
    let mut ttime = 0.0f64;
    for r in rows() {
        let label = if r.local { format!("{} (local)", r.name) } else { r.name.clone() };
        println!(
            "{:<name_w$}  {:>6}  {:>12}  {:>11}  {:>8.2}",
            label,
            r.file_count,
            r.parse_error_files.len(),
            r.diagnostics.len(),
            r.elapsed.as_secs_f64()
        );
        tf += r.file_count;
        tp += r.parse_error_files.len();
        td += r.diagnostics.len();
        ttime += r.elapsed.as_secs_f64();
    }
    println!("{}", "-".repeat(name_w + 2 + 6 + 2 + 12 + 2 + 11 + 2 + 8));
    println!("{:<name_w$}  {:>6}  {:>12}  {:>11}  {:>8.2}", "TOTAL", tf, tp, td, ttime);

    println!();
    let measurement_ok = regressions.is_empty() && throw_regressions.is_empty();
    match (td == 0, measurement_ok) {
        (true, true) => {
            println!(
                "GATE GREEN — no proof-layer diagnostics on clean-parsing corpus code, \
                 and no phpdoc.*/throw.* regression past the expected baselines."
            );
        }
        (false, _) => {
            println!(
                "GATE RED — {td} proof-layer diagnostic(s) on clean code. Human FP triage required (ADR-0013)."
            );
        }
        (true, false) => {
            println!(
                "GATE RED — {} package(s) regressed past their expected phpdoc.*/throw.* baseline \
                 (see the tripwire lists above). Investigate the new finding(s); update \
                 PHPDOC_EXPECTED / THROW_EXPECTED in xtask/src/gate.rs only once the change is \
                 understood and intended.",
                regressions.len() + throw_regressions.len()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use steins_infer::is_vendor_path;

    // The vendor-path predicate (ADR-0015) drives the gate's local-project vendor
    // split; verify its component-boundary behavior here where the gate uses it.
    #[test]
    fn vendor_predicate_matches_directory_components_only() {
        // A `vendor/` component — top-level or nested — is vendor.
        assert!(is_vendor_path("vendor/foo/Bar.php"));
        assert!(is_vendor_path("src/vendor/foo/Bar.php"));
        assert!(is_vendor_path("/abs/mono/vendor/pkg/lib.php"));
        assert!(is_vendor_path("a\\vendor\\b.php")); // Windows separators
        // First-party paths are not vendor — including look-alikes.
        assert!(!is_vendor_path("src/app/Service.php"));
        assert!(!is_vendor_path("vendor_proj/app/Service.php")); // sibling, not a component
        assert!(!is_vendor_path("src/vendored/x.php"));
        assert!(!is_vendor_path("app/vendor.php")); // filename, not a directory
    }
}
