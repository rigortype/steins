//! Integration tests for ADR-0043 §6 — phpdoc-honesty repair extended to
//! **methods**. A lying `@param`/`@return` on an *eligible* method is widened to
//! the proven truth from the method-call reverse sweep / the method's own return
//! sites; a non-eligible method refuses through the same ADR-0041 §1 split.

use steins_db::{Project, SourceFile, SteinsDatabase};
use steins_edit::honesty::{REASON_AMBIGUOUS, REASON_MAGIC_METHOD, REASON_METHOD_INHERITANCE};
use steins_edit::plan_phpdoc_honesty;
use steins_edit::{TransformReport, VouchSet};

fn plan(files: &[(&str, &str)]) -> TransformReport {
    let db = SteinsDatabase::default();
    let inputs: Vec<SourceFile> = files
        .iter()
        .map(|(p, t)| SourceFile::new(&db, (*p).to_owned(), (*t).to_owned()))
        .collect();
    let project = Project::new(&db, inputs);
    plan_phpdoc_honesty(&db, project, &VouchSet::empty())
}

fn assert_oracle_complete(report: &TransformReport) {
    assert!(report.oracle.is_complete(), "oracle incomplete: {:?}", report.oracle);
}

fn only_reason(report: &TransformReport) -> &str {
    assert_eq!(report.refusals.len(), 1, "expected one refusal, got: {:#?}", report.refusals);
    &report.refusals[0].reason
}

// ---- 1. Method `@param` honesty --------------------------------------------

#[test]
fn method_param_widens_to_proven_union() {
    // `@param int $id` but callers pass an int and numeric strings — widen the doc.
    let c = "<?php\nfinal class C {\n/** @param int $id */\npublic function m($id) { return $id; }\npublic function a() { return $this->m(1); }\npublic function b() { return $this->m('12'); }\npublic function d() { return $this->m('34'); }\n}\n";
    let report = plan(&[("c.php", c)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);
    let out = report.plan.apply_file("c.php", c);
    assert!(out.contains("@param int|numeric-string $id"), "got:\n{out}");
}

#[test]
fn method_param_honesty_refuses_inheritance() {
    // An overridable public method that lies: an exact-receiver caller makes the
    // lie observable, then the eligibility split refuses the widening (honesty
    // applies the split too).
    let c = "<?php\nclass C {\n/** @param int $id */\npublic function m($id) { return $id; }\n}\n";
    let main = "<?php\n(new C())->m('x');\n";
    let report = plan(&[("c.php", c), ("main.php", main)]);
    assert_eq!(only_reason(&report), REASON_METHOD_INHERITANCE);
}

#[test]
fn method_param_honesty_refuses_magic() {
    let c = "<?php\nfinal class C {\n/** @param int $x */\npublic function __set($x) { return $this->__set('s'); }\n}\n";
    let report = plan(&[("c.php", c)]);
    // The lie is proven (`'s'` violates `int`), then the magic verdict refuses.
    assert!(
        report.refusals.iter().any(|r| r.reason == REASON_MAGIC_METHOD),
        "{:#?}",
        report.refusals
    );
}

#[test]
fn method_param_honesty_refuses_on_unresolved_receiver() {
    // A `$o->m(...)` unknown-receiver call taints `m`; the lie widening cannot
    // claim all callers are enumerable.
    let c = "<?php\nfinal class C {\n/** @param int $id */\npublic function m($id) { return $id; }\npublic function a() { return $this->m('x'); }\n}\n";
    let f = "<?php\nfunction f($o) { $o->m(1); }\n";
    let report = plan(&[("c.php", c), ("f.php", f)]);
    assert_eq!(only_reason(&report), REASON_AMBIGUOUS);
}

// ---- 2. Method `@return` honesty -------------------------------------------

#[test]
fn method_return_widens_to_proven_union() {
    // `@return int` but the body returns int and string literals.
    let c = "<?php\nfinal class C {\n/** @return int */\npublic function m($flag) {\nif ($flag) { return 1; }\nreturn 'zero';\n}\n}\n";
    let report = plan(&[("c.php", c)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);
    let out = report.plan.apply_file("c.php", c);
    // int + a single string literal → `int|'zero'`.
    assert!(out.contains("@return"), "got:\n{out}");
    assert!(out.contains("int") && out.contains("'zero'"), "widened return:\n{out}");
}

#[test]
fn method_return_honesty_refuses_inheritance() {
    let c = "<?php\nclass C {\n/** @return int */\npublic function m() { return 'nope'; }\n}\n";
    let report = plan(&[("c.php", c)]);
    assert_eq!(only_reason(&report), REASON_METHOD_INHERITANCE);
}

#[test]
fn honest_method_is_not_enumerated() {
    // No lie: the `@param` admits every observed literal — nothing to repair.
    let c = "<?php\nfinal class C {\n/** @param int $x */\npublic function m($x) { return $x; }\npublic function a() { return $this->m(1); }\n}\n";
    let report = plan(&[("c.php", c)]);
    assert_eq!(report.oracle.enumerated, 0);
    assert!(report.refusals.is_empty());
}
