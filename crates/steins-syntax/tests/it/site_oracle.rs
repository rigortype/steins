//! What the lowered sites guarantee their consumers (issues #862, #863).
//!
//! The lanes read a body's `sites` and nothing else, so a site that no lane
//! records is a silent hole, and a lane that selects sites by span (region
//! purity, ADR-0076) reads the span a site carries. For every owner the lowering
//! scans:
//!
//! * every site but an operator site (#859, resolved by the resolver only) is
//!   recorded by at least one lane: the effect origin list or the
//!   throw origin list derived from it holds a view of it ([`derive_effect_origins`],
//!   [`derive_throw_origins`]), and each view sits at the site's span.
//!
//! Until #863 this file held the lowering against the two scans it replaced, the
//! effect and the throw one, and compared the derived lists with theirs element
//! by element over every PHP file of the repository and of the public corpora. The
//! scans are gone; the byte-identity A/B of #863 (`check`, `annotate`,
//! `effect-diff`, and the three transform dry-runs over the ten public packages)
//! is what that comparison became, and the always-on checks below keep the
//! structural half.
//!
//! * `over_repo_files` is the always-on half, over the PHP files the repository
//!   keeps outside `corpus/`.
//! * `over_corpus` is `#[ignore]`d: it walks every `.php` file under
//!   `STEINS_CORPUS_DIR`, vendor directories included, and under
//!   `STEINS_NSRT_DIR` when that is set too.
//!
//!   ```text
//!   STEINS_CORPUS_DIR=/path/to/corpus cargo test --release -p steins-syntax \
//!       --test it site_oracle -- --ignored
//!   ```

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use steins_syntax::{
    ConstructKind, SiteKind, SiteOrigin, SourceTree, derive_effect_origins, derive_throw_origins,
};

/// What a run read.
#[derive(Default)]
struct Report {
    files: usize,
    owners: usize,
    sites: usize,
    divergences: Vec<String>,
    /// Operator sites by `family/construct`, which no lane records (#859).
    operators: BTreeMap<String, usize>,
}

/// Check every site of one owner: at least one lane records it, at its span.
fn check_owner(file: &Path, name: &str, sites: &[SiteOrigin], report: &mut Report) {
    report.owners += 1;
    report.sites += sites.len();
    for site in sites {
        if let SiteKind::Operator { family, construct, .. } = &site.kind {
            *report.operators.entry(format!("{family:?}/{construct:?}")).or_default() += 1;
            continue;
        }
        let one = std::slice::from_ref(site);
        let (effect, throw) = (derive_effect_origins(one), derive_throw_origins(one));
        let spans_ok = effect.iter().all(|o| o.span() == site.span)
            && throw.iter().all(|o| o.span == site.span);
        if (effect.is_empty() && throw.is_empty()) || !spans_ok {
            report.divergences.push(format!(
                "{} owner {name}: site {:?} has {} effect view(s) and {} throw view(s), spans ok: {spans_ok}",
                file.display(),
                site.kind.tag(),
                effect.len(),
                throw.len(),
            ));
        }
    }
}

/// Check every owner of one parsed file.
fn check_tree(file: &Path, tree: &SourceTree, report: &mut Report) {
    report.files += 1;
    for f in tree.functions() {
        check_owner(file, &format!("function {}", f.name), &f.sites, report);
    }
    for c in tree.classes() {
        for m in &c.methods {
            check_owner(file, &format!("method {}::{}", c.name, m.name), &m.sites, report);
        }
    }
    for s in tree.scopes() {
        check_owner(file, &format!("scope {:?}", s.owner), &s.sites, report);
    }
}

/// Every `.php` file under `root`, in path order, skipping the directories `skip`
/// refuses and symlinked directories.
fn php_files(root: &Path, skip: &dyn Fn(&Path) -> bool, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else { return };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(fs::DirEntry::path);
    for entry in entries {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            if !skip(&path) {
                php_files(&path, skip, out);
            }
        } else if path.extension().is_some_and(|e| e == "php") && path.is_file() {
            out.push(path);
        }
    }
}

/// Run the checks over `files`.
fn run(files: &[PathBuf]) -> Report {
    let mut report = Report::default();
    for file in files {
        let source = fs::read(file).expect("a PHP file reads");
        let tree = SourceTree::parse(&String::from_utf8_lossy(&source));
        check_tree(file, &tree, &mut report);
    }
    report
}

/// Fail on any divergence, naming the first few. A run that read no files or no
/// sites fails too: it would otherwise pass having checked nothing.
fn assert_clean(label: &str, report: &Report) {
    assert!(report.files > 0, "{label}: the checks read no files");
    assert!(report.sites > 0, "{label}: the checks read no sites in {} files", report.files);
    assert!(
        report.divergences.is_empty(),
        "{label}: {} divergences over {} files, {} owners, {} sites; first {}:\n{}",
        report.divergences.len(),
        report.files,
        report.owners,
        report.sites,
        report.divergences.len().min(10),
        report.divergences.iter().take(10).cloned().collect::<Vec<_>>().join("\n"),
    );
    eprintln!(
        "{label}: {} files, {} owners, {} sites, no divergence",
        report.files, report.owners, report.sites
    );
    for (form, count) in &report.operators {
        eprintln!("{label}: operator {form}: {count}");
    }
}

/// The directories of this repository that hold PHP of its own, relative to the
/// workspace root: a fixed list, so a run reads the same files wherever the
/// checkout sits and never reaches `corpus/` or a build directory.
const REPO_PHP_ROOTS: [&str; 4] = ["composer", "crates", "docs", "harness"];

#[test]
fn over_repo_files() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let skip = |dir: &Path| dir.file_name().is_some_and(|n| n == "target" || n == "vendor");
    let mut files = Vec::new();
    for dir in REPO_PHP_ROOTS {
        php_files(&root.join(dir), &skip, &mut files);
    }
    assert_clean("repo files", &run(&files));
}

#[test]
#[ignore = "reads STEINS_CORPUS_DIR (and STEINS_NSRT_DIR when set); run explicitly"]
fn over_corpus() {
    let dir = std::env::var_os("STEINS_CORPUS_DIR")
        .expect("set STEINS_CORPUS_DIR to the directory holding the public corpora");
    let mut roots = vec![PathBuf::from(dir)];
    roots.extend(std::env::var_os("STEINS_NSRT_DIR").map(PathBuf::from));
    for root in roots {
        let mut files = Vec::new();
        php_files(&root, &|_| false, &mut files);
        assert_clean(&root.display().to_string(), &run(&files));
    }
}

/// Constructs the corpora may carry rarely or never, each in a shape the two
/// lanes treat in their own way: read here so a regression on one does not wait
/// for a corpus run.
const EDGE_SHAPES: &str = r#"<?php
namespace N;

class K {
    private $repo;
    public function __construct(private object $dep) { $this->dep->boot(); $this->x = 1; }
    public function run(array $a, $o, $cb, ...$rest) {
        $this->repo->save($a);
        $o->save(...$rest);
        $o?->save($a);
        $o?->$cb();
        $cb();
        $fn = fn($x) => strlen($x);
        $fn();
        array_map('strtolower', $a);
        array_map(strtolower(...), $a, ...$rest);
        str_replace(search: 'a', replace: 'b', subject: $a);
        fopen('php://stdout', 'w');
        print_r($a, true);
        new self($o);
        new static;
        new $cb($a);
        new class($a) extends K { public function __construct($a) { f($a); } };
        new class(g()) extends K {};
        new class { use T; };
        new class { };
        parent::run($a, $o, $cb);
        $cb::run();
        static::run();
        K::$cache['k'] = h();
        K::$$o;
        $_GET['x'] = $GLOBALS['y'];
        global $g;
        static $s = 0;
        $o->p = 1; $o->q[] = 2; $o->r++; unset($o->s);
        foreach ($a as $o->t) {}
        echo 'a', f();
        print 'b';
        ?>inline<?= $a ?>
        <?php
        eval($a);
        include 'x.php';
        require_once __DIR__ . '/y.php';
        exit(f());
        die;
        return match ($o) { 1 => throw new \LogicException(), 2 => f() };
    }
    public function guards($e) {
        try {
            try {
                throw new \RuntimeException(f());
            } catch (\InvalidArgumentException | \LogicException $e) {
                throw $e;
            } catch (UnknownThing $e) {
                $e = new \Exception();
                throw $e;
            } finally {
                cleanup();
            }
        } catch (\Throwable $t) {
            g($t);
            throw $t;
        }
        throw $e;
        throw $this->make();
        throw new $e();
        $f = function () use ($e) { try { h(); } catch (\Exception $x) { throw $x; } };
        $g = fn() => throw new \Exception();
    }
}
function top($p) {
    $c = 'strlen';
    $c($p);
    $h = static function () { return f(); };
    $h();
    $h();
    echo match (true) { $p > 1 => 'a', default => 'b' };
    echo match (true) { $p > 1 => 'a' };
}
"#;

#[test]
fn over_edge_shapes() {
    let mut report = Report::default();
    let tree = SourceTree::parse(EDGE_SHAPES);
    assert!(tree.parse_errors().is_empty(), "the fixture parses: {:?}", tree.parse_errors());
    check_tree(Path::new("<edge shapes>"), &tree, &mut report);
    assert_clean("edge shapes", &report);
    assert!(report.sites > 60, "the fixture lowers to many sites, not {}", report.sites);
}

/// Every site carries the guards of the `try` blocks it sits in, whatever its
/// kind (an `eval`, an `include` and an `echo` are guarded as a call is), and
/// the body of a `catch` or `finally` carries only the guards outside its own
/// `try`. Guards read innermost first.
#[test]
fn guards_follow_the_try_structure_for_every_kind() {
    let src = r"<?php
function g($x) {
    try {
        eval($x);
        include $x;
        echo $x;
        f();
        try {
            f();
        } catch (A $a) {
            echo 1; eval($x); f();
        } finally {
            echo 2; f();
        }
    } catch (B $b) {
        echo 3; f();
    }
}
";
    let tree = SourceTree::parse(src);
    let sites = &tree.functions()[0].sites;
    let guards = |site: &SiteOrigin| {
        let level = |clauses: &Vec<steins_syntax::CatchClause>| {
            let names = clauses.iter().flat_map(|c| c.classes.iter().map(|n| n.raw.as_str()));
            names.collect::<Vec<_>>().join("|")
        };
        match site.guards.iter().map(level).collect::<Vec<_>>().join(">").as_str() {
            "" => "-".to_owned(),
            guards => guards.to_owned(),
        }
    };
    let label = |site: &SiteOrigin| match &site.kind {
        SiteKind::Construct(ConstructKind::Eval) => "eval",
        SiteKind::Construct(ConstructKind::Include(_)) => "include",
        SiteKind::Construct(ConstructKind::Output(_)) => "echo",
        SiteKind::Call { .. } => "f",
        // `echo $x` also converts its parameter operand (#859); `echo 1` converts nothing.
        SiteKind::Operator { .. } => "echo operator",
        other => panic!("a site this source should not lower to: {other:?}"),
    };
    let got: Vec<(&str, String)> = sites.iter().map(|s| (label(s), guards(s))).collect();
    let expected: Vec<(&str, &str)> = vec![
        ("eval", "B"),
        ("include", "B"),
        ("echo", "B"),
        ("echo operator", "B"),
        ("f", "B"),
        ("f", "A>B"),
        ("echo", "B"),
        ("eval", "B"),
        ("f", "B"),
        ("echo", "B"),
        ("f", "B"),
        ("echo", "-"),
        ("f", "-"),
    ];
    let expected: Vec<(&str, String)> =
        expected.into_iter().map(|(l, g)| (l, g.to_owned())).collect();
    assert_eq!(got, expected);
}
