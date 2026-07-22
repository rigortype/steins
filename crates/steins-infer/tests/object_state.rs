//! Acceptance tests for the object-state milestone (ADR-0036): the allocation-keyed
//! heap, aliasing, `clone` isolation, escape sets and sweeps, `readonly` immunity,
//! and the three new property checks (`type.property-mismatch`,
//! `phpdoc.property-mismatch`, `readonly.reassigned`).
//!
//! Property facts are observed *through diagnostics*: a proven bad property value
//! read into a native-typed argument (`needInt($o->p)`) fires
//! `type.argument-mismatch`; its presence or absence witnesses whether the heap
//! kept the fact.

use steins_db::{Project, SourceFile, SteinsDatabase};
use steins_infer::{
    Diagnostic, ID, NoFold, PHPDOC_PROP_MISMATCH_ID, PROP_MISMATCH_ID, READONLY_REASSIGNED_ID,
    check_project,
};

fn findings(src: &str) -> Vec<Diagnostic> {
    let db = SteinsDatabase::default();
    let input = SourceFile::new(&db, "main.php".to_owned(), src.to_owned());
    let project = Project::new(&db, vec![input]);
    check_project(&db, project, &mut NoFold)
}

fn count(src: &str) -> usize {
    findings(src).len()
}

/// A prelude shared by most tests: an `int`-typed sink and an untyped-property box.
const PRELUDE: &str = "<?php\nfunction needInt(int $x): int { return $x; }\nclass Box { public $p; }\n";

// ---- Aliasing: writes visible through every alias --------------------------

#[test]
fn alias_write_visible_via_original() {
    // `$b = $a` shares the object; a write via `$a` is seen through `$b`.
    let src = format!("{PRELUDE}$a = new Box();\n$a->p = \"abc\";\n$b = $a;\nneedInt($b->p);\n");
    let f = findings(&src);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, ID);
}

#[test]
fn alias_write_visible_via_alias() {
    // A write via the alias `$b` is seen through the original `$a`.
    let src = format!("{PRELUDE}$a = new Box();\n$b = $a;\n$b->p = \"abc\";\nneedInt($a->p);\n");
    assert_eq!(count(&src), 1);
}

// ---- clone isolation (adversarial #1), both directions ---------------------

#[test]
fn clone_isolates_both_directions() {
    // `$a->p = 5` (good int); clone; corrupt only the clone. The original stays a
    // clean int (no finding for `$a->p`); the clone carries the bad value (one
    // finding for `$c->p`). A naive id-sharing model would misfire on both.
    let src = format!(
        "{PRELUDE}$a = new Box();\n$a->p = 5;\n$c = clone $a;\n$c->p = \"abc\";\nneedInt($a->p);\nneedInt($c->p);\n"
    );
    let f = findings(&src);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, ID);
    assert_eq!(f[0].line, 9, "finding is on the $c->p read, not $a->p: {f:#?}");
}

#[test]
fn clone_write_does_not_leak_to_original() {
    // Writing the clone never changes the original (the other direction).
    let src = format!(
        "{PRELUDE}$a = new Box();\n$a->p = 5;\n$c = clone $a;\n$c->p = \"abc\";\nneedInt($a->p);\n"
    );
    assert_eq!(count(&src), 0);
}

// ---- Escape sweep on pass-to-unknown; non-escaped survival (the payoff) -----

#[test]
fn escape_sweep_on_pass_to_unknown() {
    // `$a` (aliased by `$b`) is passed to an unknown call: its props are swept —
    // observed through the surviving alias `$b`, so this is a real heap sweep, not
    // merely the passed variable losing its binding.
    let src = format!(
        "{PRELUDE}$a = new Box();\n$a->p = \"abc\";\n$b = $a;\nsink($a);\nneedInt($b->p);\n"
    );
    assert_eq!(count(&src), 0, "escaped object's props must die");
}

#[test]
fn non_escaped_survives_unknown_call() {
    // The payoff: a purely-local object never passed anywhere keeps its facts across
    // an unrelated unknown call.
    let src = format!("{PRELUDE}$a = new Box();\n$a->p = \"abc\";\nunknownFn();\nneedInt($a->p);\n");
    assert_eq!(count(&src), 1, "non-escaped object's props must survive");
}

// ---- Other escape triggers: prop-store, closure capture --------------------

#[test]
fn store_into_property_escapes() {
    let src = format!(
        "{PRELUDE}class Holder {{ public $item; }}\n$h = new Holder();\n$a = new Box();\n$a->p = \"abc\";\n$b = $a;\n$h->item = $a;\nunknownFn();\nneedInt($b->p);\n"
    );
    assert_eq!(count(&src), 0, "an object stored into a property escapes and is swept");
}

#[test]
fn closure_capture_escapes() {
    let src = format!(
        "{PRELUDE}$a = new Box();\n$a->p = \"abc\";\n$b = $a;\n$f = function() use ($a) {{ return $a; }};\nunknownFn();\nneedInt($b->p);\n"
    );
    assert_eq!(count(&src), 0, "a captured object escapes and is swept");
}

// ---- $this: overridable-call sweep vs private/final descent survival --------

#[test]
fn this_survives_private_call_but_swept_by_overridable() {
    let src = "<?php\nfunction needInt(int $x): int { return $x; }\n\
class Widget {\n\
  public $p;\n\
  private function helper(): void {}\n\
  public function pub(): void {}\n\
  public function run(): void { $this->p = \"abc\"; $this->helper(); needInt($this->p); }\n\
  public function run2(): void { $this->p = \"abc\"; $this->pub(); needInt($this->p); }\n\
}\n";
    let f = findings(src);
    // run(): private call leaves $this->p intact → one finding.
    // run2(): overridable call sweeps $this->p → no finding.
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, ID);
}

// ---- readonly immunity: persists through escape + unknown calls ------------

#[test]
fn readonly_persists_through_escape_and_unknown_call() {
    let src = "<?php\nfunction needInt(int $x): int { return $x; }\n\
class Ro { public function __construct(public readonly string $name) {} }\n\
$r = new Ro(\"abc\");\n$alias = $r;\nsink($r);\nunknownFn();\nneedInt($alias->name);\n";
    // The readonly `$name` survives the escape + sweep, so the bad string still
    // flows into needInt() → one finding.
    let f = findings(src);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, ID);
}

// ---- readonly.reassigned ---------------------------------------------------

#[test]
fn readonly_reassigned_fires_on_proven_double_write() {
    // Promoted param is the (proven) first write; the method write is the second.
    let src = "<?php\n\
class Acct {\n\
  public function __construct(public readonly int $balance) {}\n\
  public function reset(): void { $this->balance = 0; }\n\
}\n";
    let f = findings(src);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, READONLY_REASSIGNED_ID);
    assert!(f[0].message.contains("readonly property"), "{}", f[0].message);
}

#[test]
fn readonly_reassigned_silent_when_second_write_conditional() {
    let src = "<?php\n\
class Acct {\n\
  public function __construct(public readonly int $balance) {}\n\
  public function reset(bool $c): void { if ($c) { $this->balance = 0; } }\n\
}\n";
    assert_eq!(count(src), 0, "a conditional second write is not a proven reassignment");
}

// ---- type.property-mismatch (strict / coercive / silent-on-unknown) --------

#[test]
fn property_mismatch_coercive_nonnumeric_string() {
    let src = "<?php\nclass T { public int $n = 0; }\n$t = new T();\n$t->n = \"abc\";\n";
    let f = findings(src);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, PROP_MISMATCH_ID);
    assert!(f[0].message.contains("property"), "{}", f[0].message);
}

#[test]
fn property_mismatch_coercive_silent_on_numeric_string() {
    // Coercive mode: "5" coerces into int — no finding.
    let src = "<?php\nclass T { public int $n = 0; }\n$t = new T();\n$t->n = \"5\";\n";
    assert_eq!(count(src), 0);
}

#[test]
fn property_mismatch_strict_fires_on_numeric_string() {
    let src = "<?php\ndeclare(strict_types=1);\nclass T { public int $n = 0; }\n$t = new T();\n$t->n = \"5\";\n";
    let f = findings(src);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, PROP_MISMATCH_ID);
}

#[test]
fn property_mismatch_silent_on_unknown_value() {
    let src = "<?php\nclass T { public int $n = 0; }\n$t = new T();\n$t->n = someUnknown();\n";
    assert_eq!(count(src), 0);
}

// ---- phpdoc.property-mismatch (@var contract, incl. abstract fact) ----------

#[test]
fn phpdoc_property_mismatch_proven_value() {
    let src = "<?php\nclass P {\n  /** @var non-empty-string */\n  public $tag = \"x\";\n}\n$p = new P();\n$p->tag = \"\";\n";
    let f = findings(src);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, PHPDOC_PROP_MISMATCH_ID);
}

#[test]
fn phpdoc_property_mismatch_abstract_fact() {
    // `$s` is only known to be a string (abstract fact from its native param type);
    // the property's `@var int` contract definitely rejects it.
    let src = "<?php\nclass P2 {\n  /** @var int */\n  public $num = 0;\n}\n\
function set(string $s): void { $p = new P2(); $p->num = $s; }\n";
    let f = findings(src);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, PHPDOC_PROP_MISMATCH_ID);
}

// ---- Property read flows into a width()-style argument check ----------------

#[test]
fn prop_read_flows_into_arg_check() {
    let src = format!("{PRELUDE}$b = new Box();\n$b->p = \"abc\";\nneedInt($b->p);\n");
    let f = findings(&src);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, ID);
}

// ---- Promoted-param: no double-report --------------------------------------

#[test]
fn promoted_param_no_double_report() {
    // `new Pt("abc")` is a single ctor-argument violation, not also a property one.
    let src = "<?php\nclass Pt { public function __construct(public int $x) {} }\n$p = new Pt(\"abc\");\n";
    let f = findings(src);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, ID, "the single finding is the ctor-arg check, not a property one");
}

// ---- Adversarial #2: by-ref property alias must not keep a stale fact -------

#[test]
fn by_ref_property_alias_poisons_and_avoids_stale_fact() {
    // `$r = &$b->p` aliases the property by reference; `changeIt($r)` may rewrite it
    // invisibly. If the analyzer kept the stale `"abc"` fact it would wrongly flag
    // `needInt($b->p)`. Poison (the reference-assignment family) drops all facts →
    // no false positive.
    let src = format!(
        "{PRELUDE}function changeIt(&$x): void {{ $x = 1; }}\n$b = new Box();\n$b->p = \"abc\";\n$r = &$b->p;\nchangeIt($r);\nneedInt($b->p);\n"
    );
    assert_eq!(count(&src), 0, "poison must prevent a stale property fact from firing");
}
