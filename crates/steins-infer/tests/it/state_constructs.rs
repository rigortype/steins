//! Structural state constructs (ADR-0055 amendment, 2026-09-26): a `global`
//! import, a `static` declaration, a superglobal access, a static property
//! access and an instance property write each reach state that outlives the
//! call, and none has its label (`global.*`, `mutate.*`) inferred yet. Until
//! it is, each marks its body **non-exhaustive** — `{…?}`, never the `{}` a
//! proven-pure body earns — and does nothing else: no label, no finding.

use steins_db::{PluginFacts, Project, ProjectLayout, SourceFile, SteinsDatabase};
use steins_infer::{
    Diagnostic, EFFECT_ID, EFFECT_LISKOV_ID, EffectSummary, FactKind, NoFold, RegionPurity,
    STATEMENT_NO_EFFECT_ID, annotate_facts, check, effect_summary, region_purity_project,
};
use steins_syntax::SourceTree;

fn summary(src: &str, symbol: &str) -> EffectSummary {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let classes = tree.classes().to_vec();
    effect_summary(&tree, &functions, &classes)
        .into_iter()
        .find(|s| s.symbol == symbol)
        .unwrap_or_else(|| panic!("no summary for {symbol}"))
}

/// Every `annotate` effect-margin body in a source, in line order.
fn effect_margins(src: &str) -> Vec<String> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let classes = tree.classes().to_vec();
    annotate_facts(&tree, &functions, &classes, "test.php", &mut NoFold)
        .into_iter()
        .filter(|f| matches!(f.kind, FactKind::Effects { .. }))
        .map(|f| f.body())
        .collect()
}

/// The findings of `ids` a source earns.
fn findings(src: &str, ids: &[&str]) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php").into_iter().filter(|d| ids.contains(&d.id)).collect()
}

/// The purity of the region between the two `// region` markers of `source`.
fn region(source: &str) -> RegionPurity {
    let start = source.find("// region-start").expect("no start marker") as u32;
    let end = source.find("// region-end").expect("no end marker") as u32;
    let db = SteinsDatabase::default();
    let file = SourceFile::new(&db, "lib.php".to_owned(), source.to_owned());
    let project = Project::new(&db, vec![file], ProjectLayout::fallback(), PluginFacts::none());
    let mut answers = region_purity_project(&db, project, &[("lib.php".to_owned(), start, end)]);
    answers.pop().expect("one answer per region")
}

/// A body's summary: no proven label, and not exhaustive.
fn assert_unknown(body: &str) {
    let src = format!(
        "<?php\nclass Svc {{\n    public static $hits = 0;\n    public $x = 0;\n    public $items = [];\n    public function m($o) {{ {body} }}\n}}\n"
    );
    let s = summary(&src, "Svc::m");
    assert!(s.labels.is_empty(), "`{body}` proves no label, got {:?}", s.labels);
    assert!(!s.exhaustive, "`{body}` must mark the body `…?`");
}

// ---- The three bodies that read `{}` before ---------------------------------

/// The issue's own reproduction: each summary read `effects: {}`, the one a
/// proven-pure body earns, and `effects-envelope` wrote `@phpstan-all-methods-pure`
/// over the class from it.
#[test]
fn the_three_state_bodies_are_not_proven_pure() {
    let src = concat!(
        "<?php\n",
        "/** @pure */\n",
        "function readGet(): mixed { return $_GET['x']; }\n",
        "/** @pure */\n",
        "function counter(): int { static $n = 0; return ++$n; }\n",
        "final class Svc\n",
        "{\n",
        "    public static int $hits = 0;\n",
        "    public int $x = 0;\n",
        "    public function bump(): void { self::$hits++; $this->x = 1; }\n",
        "}\n",
    );
    assert_eq!(effect_margins(src), ["effects: {…?}", "effects: {…?}", "effects: {…?}"]);
}

// ---- Each construct on its own ---------------------------------------------

#[test]
fn global_and_static_declarations_are_not_exhaustive() {
    assert_unknown("global $config;");
    assert_unknown("static $cache = [];");
}

#[test]
fn every_superglobal_read_or_write_is_not_exhaustive() {
    for sg in steins_syntax::SUPERGLOBALS {
        assert_unknown(&format!("return ${sg}['k'] ?? null;"));
        assert_unknown(&format!("${sg}['k'] = 1;"));
    }
    assert_unknown("return isset($_SESSION['user']);");
    assert_unknown("unset($_SESSION['user']);");
}

#[test]
fn static_property_reads_and_writes_are_not_exhaustive() {
    for access in ["self::$hits", "static::$hits", "Svc::$hits", "$o::$hits"] {
        assert_unknown(&format!("return {access};"));
        assert_unknown(&format!("{access} = 1;"));
    }
    assert_unknown("self::$hits++;");
}

#[test]
fn instance_property_writes_are_not_exhaustive() {
    for write in [
        "$this->x = 1;",
        "$this->x += 1;",
        "$this->x ??= 1;",
        "$this->x++;",
        "--$this->x;",
        "unset($this->x);",
        "$this->items[] = 1;",
        "$this->items['k'] = 1;",
        "$o->p = 1;",
        "$o->p->q = 1;",
        "[$this->x, $y] = [1, 2];",
        "$r = &$this->x;",
        "foreach ([1] as $this->x) {}",
        "foreach ($this->items as &$v) { $v = 0; }",
    ] {
        assert_unknown(write);
    }
}

/// Reading an instance property is not state the call changes (and PHPStan
/// agrees: a pure method may read `$this`), so it stays exhaustive.
#[test]
fn instance_property_reads_stay_exhaustive() {
    let src = "<?php\nclass C {\n    public $x = 0;\n    public function m($o, array $a) { $a[$this->x] = $o->p; foreach ($this->x as $v) {} return $this->x; }\n}\n";
    let s = summary(src, "C::m");
    assert!(s.labels.is_empty() && s.exhaustive, "{s:?}");
}

/// The conservative side of the ADR-0055 constructor carve-out (#313): an
/// initializing constructor is a property write like any other, for now.
#[test]
fn a_constructors_own_property_initialization_is_not_exhaustive_yet() {
    let src = "<?php\nclass Money {\n    private int $n;\n    public function __construct(int $n) { $this->n = $n; }\n}\n";
    assert!(!summary(src, "Money::__construct").exhaustive);
    let promoted = "<?php\nclass Money {\n    public function __construct(private int $n) {}\n}\n";
    assert!(summary(promoted, "Money::__construct").exhaustive, "promotion is no body write");
}

// ---- What it does not do ---------------------------------------------------

/// A proven label beside a state construct is still proven and still printed.
#[test]
fn a_proven_label_survives_beside_the_marker() {
    let src = "<?php\nfunction f(): void { echo $_GET['q']; }\n";
    assert_eq!(effect_margins(src), ["effects: {io.output.buffer, …?}"]);
}

/// The marker taints callers the way an unresolved call does.
#[test]
fn the_marker_propagates_to_callers() {
    let src = "<?php\nfunction counter(): int { static $n = 0; return ++$n; }\nfunction twice(): int { return counter() + counter(); }\n";
    assert!(!summary(src, "twice").exhaustive);
}

/// A construct inside a closure or arrow function is that scope's; defining
/// the closure is not running it.
#[test]
fn a_nested_scope_owns_its_construct() {
    let src = "<?php\nfunction f(): void { $g = function () { global $x; }; $h = fn () => $_GET; }\n";
    let s = summary(src, "f");
    assert!(s.exhaustive, "{s:?}");
}

/// Non-exhaustiveness never produces a finding (ADR-0005): a `Pure` envelope
/// over a state construct is not `effect.envelope-exceeded`, and a pure
/// abstraction implemented by a property writer is not `effect.liskov-widened`.
/// Both wait for the labels.
#[test]
fn no_envelope_or_liskov_finding_fires() {
    let src = concat!(
        "<?php\n",
        "interface Counter {\n",
        "    #[\\Steins\\Pure]\n",
        "    public function bump(): void;\n",
        "}\n",
        "final class Svc implements Counter {\n",
        "    public static int $hits = 0;\n",
        "    public int $x = 0;\n",
        "    public function bump(): void { self::$hits++; $this->x = 1; }\n",
        "}\n",
        "#[\\Steins\\Pure]\n",
        "function readGet(): mixed { return $_GET['x']; }\n",
    );
    let found = findings(src, &[EFFECT_ID, EFFECT_LISKOV_ID]);
    assert!(found.is_empty(), "{found:#?}");
}

/// `statement.no-effect` judges discarded *calls* (ADR-0096); a bare superglobal
/// read is not one, and it was silent before this change too.
#[test]
fn a_bare_superglobal_read_is_not_a_no_effect_statement() {
    let found = findings("<?php\nfunction f(): void { $_GET['x']; self::$p; }\n", &[STATEMENT_NO_EFFECT_ID]);
    assert!(found.is_empty(), "{found:#?}");
}

/// The loop→`array_map` precondition reads the same bit (ADR-0076 §2), so a
/// region holding a state construct is not exhaustive and a property read is.
#[test]
fn region_purity_sees_the_marker() {
    let reads_request = "<?php\nfunction f(array $xs): array {\n    $out = [];\n    // region-start\n    foreach ($xs as $x) { $out[] = $_GET[$x]; }\n    // region-end\n    return $out;\n}\n";
    assert!(!region(reads_request).exhaustive);
    let reads_property = "<?php\nclass C {\n    public $k = 1;\n    public function f(array $xs): array {\n        $out = [];\n        // region-start\n        foreach ($xs as $x) { $out[] = $x + $this->k; }\n        // region-end\n        return $out;\n    }\n}\n";
    assert!(region(reads_property).exhaustive);
}
