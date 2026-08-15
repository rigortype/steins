//! `probe291`: the issue #291 measurement harness. **Not a shipping surface.**
//!
//! Counts the four scratch ids `steins-infer` emits for the probe
//! (`phpdoc.probe291-*`) across the three sources #291 names:
//!
//! * the pinned public corpus (whole-project mode, as `fp-gate` analyzes it);
//! * phpstan-src's nsrt fixtures (each file its own single-file universe, as
//!   `cargo xtask nsrt` analyzes them);
//! * `php-typing-conformance`'s test files (same single-file treatment).
//!
//! Output is one TSV line per firing plus a per-source, per-id summary, so the
//! corpus lines can be triaged against the real source by hand.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use steins_db::{Project, SourceFile, SteinsDatabase, composer, parse};
use steins_infer::{Diagnostic, SidecarFolder, check_project};

use crate::corpus::{PACKAGES, checkout_dir, collect_php_files, read_lock, repo_root};

/// The probe's four ids, in report order.
const IDS: &[&str] = &[
    "phpdoc.probe291-native-verified", // cell A: Verified abstract fact vs native param
    "phpdoc.probe291-native-asserted", // cell B(i): Asserted fact vs native param
    "phpdoc.probe291-param-asserted",  // cell B(ii): arm-lane fact vs @param envelope
    "phpdoc.probe291-param-verified",  // control: what Feature E already banks
    "phpdoc.probe291-native-partial",  // some-arm-rejected (the `maybe-` shape)
    "phpdoc.probe291-census",          // the denominator (STEINS_PROBE291_CENSUS=1)
];

/// The census id, aggregated rather than listed line by line.
const CENSUS_ID: &str = "phpdoc.probe291-census";

thread_local! {
    static FOLDER: RefCell<SidecarFolder> = RefCell::new(SidecarFolder::enabled());
}

/// 256 MiB, the same reservation `fp-gate`/`nsrt` make (issue #246).
const WORKER_STACK_SIZE: usize = 256 * 1024 * 1024;

pub fn run(args: &[String]) -> Result<(), String> {
    let args: Vec<String> = args.to_vec();
    std::thread::Builder::new()
        .stack_size(WORKER_STACK_SIZE)
        .spawn(move || run_on_worker(&args))
        .expect("failed to spawn the probe291 worker thread")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

fn run_on_worker(args: &[String]) -> Result<(), String> {
    // The probe is off unless asked for (see `steins_infer::probe291_on`), so this
    // command is the only thing in the tree that turns it on.
    // SAFETY: single-threaded, before any analysis thread reads the variable.
    unsafe { std::env::set_var("STEINS_PROBE291", "1") };
    let want = |k: &str| args.iter().any(|a| a == k);
    let all = !want("--corpus") && !want("--nsrt") && !want("--conformance");
    let arg_after = |k: &str| {
        args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).map(String::as_str)
    };

    let mut totals: Vec<(String, Vec<usize>)> = Vec::new();

    if all || want("--corpus") {
        let root = repo_root();
        let lock = read_lock();
        let mut counts = vec![0usize; IDS.len()];
        for p in PACKAGES {
            let dir = checkout_dir(p.name);
            if !dir.is_dir() {
                eprintln!("probe291: corpus package not synced: {}", p.name);
                continue;
            }
            let tag = lock
                .packages
                .iter()
                .find(|e| e.name == p.name)
                .map_or("", |e| e.tag.as_str());
            let diags = analyze_project(&dir, &root, true);
            for d in &diags {
                report("corpus", &format!("{}@{tag}", p.name), d);
                bump(&mut counts, d);
            }
        }
        drain_census("corpus");
        totals.push(("corpus".to_owned(), counts));
    }

    if all || want("--nsrt") {
        let dir = arg_after("--nsrt")
            .filter(|s| !s.starts_with("--"))
            .map_or_else(
                || sibling_dir("phpstan-src/tests/PHPStan/Analyser/nsrt"),
                PathBuf::from,
            );
        totals.push(("nsrt".to_owned(), per_file_source("nsrt", &dir)?));
    }

    if all || want("--conformance") {
        let dir = arg_after("--conformance")
            .filter(|s| !s.starts_with("--"))
            .map_or_else(
                || sibling_dir("php-typing-conformance/conformance/tests"),
                PathBuf::from,
            );
        totals.push(("conformance".to_owned(), per_file_source("conformance", &dir)?));
    }

    println!("\n# summary");
    print!("source");
    for id in IDS {
        print!("\t{id}");
    }
    println!();
    for (name, counts) in &totals {
        print!("{name}");
        for c in counts {
            print!("\t{c}");
        }
        println!();
    }
    Ok(())
}

/// A PHP checkout beside this repo, the `default_nsrt_dir` convention: `repo_root`
/// is `…/repo/rust/steins`, the PHP siblings live under `…/repo/php/`. Pass the
/// directory explicitly when the layout differs.
fn sibling_dir(rel: &str) -> PathBuf {
    let guess = repo_root().join("../../php").join(rel);
    guess.canonicalize().unwrap_or(guess)
}

/// Every nsrt / conformance file is a standalone universe, so each is its own
/// single-file project (the `cargo xtask nsrt` treatment).
fn per_file_source(label: &str, dir: &Path) -> Result<Vec<usize>, String> {
    if !dir.is_dir() {
        return Err(format!("probe291: {label} directory not found: {}", dir.display()));
    }
    let mut files = Vec::new();
    collect_php_files(dir, &mut files);
    files.sort();
    let mut counts = vec![0usize; IDS.len()];
    for f in &files {
        let name = f.strip_prefix(dir).unwrap_or(f).to_string_lossy().into_owned();
        let Ok(bytes) = std::fs::read(f) else { continue };
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let db = SteinsDatabase::default();
        let input = SourceFile::new(&db, name.clone(), text);
        if !parse(&db, input).parse_errors().is_empty() {
            continue;
        }
        let project = Project::new(
            &db,
            vec![input],
            steins_db::ProjectLayout::fallback(),
            steins_db::PluginFacts::none(),
        );
        let diags = FOLDER.with(|f| {
            let mut folder = f.borrow_mut();
            check_project(&db, project, &mut *folder)
        });
        for d in diags.iter().filter(|d| is_probe(d)) {
            report(label, &name, d);
            bump(&mut counts, d);
        }
    }
    drain_census(label);
    Ok(counts)
}

/// Analyze a directory as ONE project (the corpus treatment: cross-file
/// resolution matters, and the package's own `composer.json` decides vendor).
fn analyze_project(dir: &Path, root: &Path, drop_parse_errors: bool) -> Vec<Diagnostic> {
    let mut files = Vec::new();
    collect_php_files(dir, &mut files);
    files.sort();
    let db = SteinsDatabase::default();
    let mut inputs: Vec<SourceFile> = Vec::with_capacity(files.len());
    for f in &files {
        let rel = f.strip_prefix(root).unwrap_or(f).to_string_lossy().into_owned();
        let text = std::fs::read(f)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        inputs.push(SourceFile::new(&db, rel, text));
    }
    let mut parse_errs: Vec<String> = Vec::new();
    if drop_parse_errors {
        for &input in &inputs {
            if !parse(&db, input).parse_errors().is_empty() {
                parse_errs.push(input.path(&db).to_owned());
            }
        }
    }
    let layout = composer::discover(&[dir.to_path_buf()], root);
    let php_target = layout.php_target().cloned();
    let plugins = steins_db::PluginFacts::discover(&layout, None);
    let project = Project::new(&db, inputs, layout, plugins);
    let mut diags = FOLDER.with(|f| {
        let mut folder = f.borrow_mut();
        folder.set_php_target(php_target);
        check_project(&db, project, &mut *folder)
    });
    diags.retain(|d| is_probe(d) && !parse_errs.contains(&d.path));
    diags.sort_by(|a, b| (&a.path, a.line, a.column).cmp(&(&b.path, b.line, b.column)));
    diags
}

fn is_probe(d: &Diagnostic) -> bool {
    IDS.contains(&d.id)
}

fn bump(counts: &mut [usize], d: &Diagnostic) {
    if let Some(i) = IDS.iter().position(|i| *i == d.id) {
        counts[i] += 1;
    }
}

fn report(source: &str, unit: &str, d: &Diagnostic) {
    if d.id == CENSUS_ID {
        CENSUS.with(|c| {
            // Aggregate on the judged PAIR, dropping the site: the census answers
            // "how many positions were judged, of which shapes", not "where".
            *c.borrow_mut().entry(d.message.clone()).or_insert(0usize) += 1;
        });
        return;
    }
    println!("{source}\t{unit}\t{}\t{}:{}\t{}", d.id, d.path, d.line, d.message);
}

thread_local! {
    static CENSUS: RefCell<std::collections::BTreeMap<String, usize>> =
        RefCell::new(std::collections::BTreeMap::new());
}

/// Print the aggregated census and reset it, so each source reports its own.
fn drain_census(source: &str) {
    CENSUS.with(|c| {
        let mut m = c.borrow_mut();
        if m.is_empty() {
            return;
        }
        let mut rows: Vec<(String, usize)> = m.iter().map(|(k, v)| (k.clone(), *v)).collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        println!("\n# census ({source}) — judged argument positions by shape");
        for (k, v) in rows {
            println!("{source}\tCENSUS\t{v}\t{k}");
        }
        m.clear();
    });
}
