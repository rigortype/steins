//! Integration tests for ADR-0043 §6 — phpdoc→native promotion extended to
//! **methods**. Each test builds a real multi-file salsa project and asserts on
//! the plan's edits AND the named refusal reasons, exercising the ADR-0041 §1
//! eligibility split, the six receiver forms, and the method-call reverse sweep.

use steins_db::{Project, SourceFile, SteinsDatabase};
use steins_edit::plan_phpdoc_to_native;
use steins_edit::promote::{
    REASON_AMBIGUOUS, REASON_ARG_NOT_PROVEN, REASON_MAGIC_METHOD, REASON_METHOD_INHERITANCE,
    REASON_REFERENCED_AS_VALUE,
};
use steins_edit::{TransformReport, VouchSet};

fn plan(files: &[(&str, &str)]) -> TransformReport {
    let db = SteinsDatabase::default();
    let inputs: Vec<SourceFile> = files
        .iter()
        .map(|(p, t)| SourceFile::new(&db, (*p).to_owned(), (*t).to_owned()))
        .collect();
    let project = Project::new(&db, inputs);
    plan_phpdoc_to_native(&db, project, &VouchSet::empty())
}

fn assert_oracle_complete(report: &TransformReport) {
    assert!(report.oracle.is_complete(), "oracle incomplete: {:?}", report.oracle);
}

fn only_reason(report: &TransformReport) -> &str {
    assert_eq!(report.refusals.len(), 1, "expected one refusal, got: {:#?}", report.refusals);
    &report.refusals[0].reason
}

// ---- 1. Eligible promotion (private / final / final-class) -----------------

#[test]
fn private_method_promotes_via_this_call() {
    // A private method is Liskov-safe by construction; its only callers are
    // `$this->`/`self::`/`Self::` from inside the class, all resolvable.
    let src = "<?php\nclass C {\n/** @param int $x */\nprivate function m($x) { return $x; }\npublic function run() { return $this->m(1); }\n}\n";
    let report = plan(&[("c.php", src)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.enumerated, 1);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);
    let out = report.plan.apply_file("c.php", src);
    assert!(out.contains("private function m(int $x)"), "got:\n{out}");
    assert!(!out.contains("@param"), "tag removed:\n{out}");
}

#[test]
fn final_class_public_method_promotes_via_new_receiver() {
    let src = "<?php\nfinal class C {\n/** @param int $x */\npublic function m($x) { return $x; }\n}\n";
    let main = "<?php\n(new C())->m(1);\n";
    let report = plan(&[("c.php", src), ("main.php", main)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);
    let out = report.plan.apply_file("c.php", src);
    assert!(out.contains("public function m(int $x)"), "got:\n{out}");
}

#[test]
fn final_method_on_nonfinal_class_promotes() {
    let src = "<?php\nclass C {\n/** @param int $x */\nfinal public function m($x) { return $x; }\n}\n";
    let main = "<?php\n(new C())->m(1);\n";
    let report = plan(&[("c.php", src), ("main.php", main)]);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);
    let out = report.plan.apply_file("c.php", src);
    assert!(out.contains("final public function m(int $x)"), "got:\n{out}");
}

#[test]
fn multi_receiver_form_sweep_all_proven() {
    // The same eligible method reached through `$this->`, `self::`, `C::`, and
    // `(new C())->` — every form must resolve and its literal argument prove.
    let c = "<?php\nfinal class C {\n/** @param int $x */\npublic function m($x) { return $x; }\npublic function a() { return $this->m(1); }\npublic function b() { return self::m(2); }\npublic function d() { return C::m(3); }\n}\n";
    let main = "<?php\n(new C())->m(4);\n";
    let report = plan(&[("c.php", c), ("main.php", main)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);
    let out = report.plan.apply_file("c.php", c);
    assert!(out.contains("public function m(int $x)"), "got:\n{out}");
}

#[test]
fn zero_callers_method_promotes_vacuously() {
    let src = "<?php\nfinal class C {\n/** @param string $s */\npublic function m($s) { return $s; }\n}\n";
    let report = plan(&[("c.php", src)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);
    let out = report.plan.apply_file("c.php", src);
    assert!(out.contains("public function m(string $s)"), "got:\n{out}");
}

// ---- 2. The eligibility split → method-inheritance -------------------------

#[test]
fn overridable_public_method_refuses_inheritance() {
    // Non-final public method on a non-final class: a subclass could override it.
    let src = "<?php\nclass C {\n/** @param int $x */\npublic function m($x) { return $x; }\n}\n";
    let report = plan(&[("c.php", src)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.enumerated, 1);
    assert_eq!(only_reason(&report), REASON_METHOD_INHERITANCE);
}

#[test]
fn interface_method_refuses_inheritance() {
    let src = "<?php\ninterface I {\n/** @param int $x */\npublic function m($x);\n}\n";
    let report = plan(&[("i.php", src)]);
    assert_eq!(only_reason(&report), REASON_METHOD_INHERITANCE);
}

#[test]
fn abstract_method_refuses_inheritance() {
    let src = "<?php\nabstract class C {\n/** @param int $x */\nabstract public function m($x);\n}\n";
    let report = plan(&[("c.php", src)]);
    assert_eq!(only_reason(&report), REASON_METHOD_INHERITANCE);
}

#[test]
fn trait_using_class_refuses_inheritance() {
    // A trait `use` merges methods whose bodies live elsewhere — override analysis
    // is incomplete, so even a final method refuses.
    let src = "<?php\nfinal class C {\nuse T;\n/** @param int $x */\npublic function m($x) { return $x; }\n}\n";
    let report = plan(&[("c.php", src)]);
    assert_eq!(only_reason(&report), REASON_METHOD_INHERITANCE);
}

#[test]
fn overriding_method_refuses_inheritance() {
    // Final class, but the method overrides a parent method of the same name.
    let p = "<?php\nclass P {\npublic function m($x) { return $x; }\n}\n";
    let c = "<?php\nfinal class C extends P {\n/** @param int $x */\npublic function m($x) { return $x; }\n}\n";
    let report = plan(&[("p.php", p), ("c.php", c)]);
    assert_eq!(only_reason(&report), REASON_METHOD_INHERITANCE);
}

#[test]
fn unresolvable_parent_refuses_inheritance() {
    // Parent leaves the project → hierarchy incomplete → cannot prove not-overriding.
    let c = "<?php\nfinal class C extends \\Vendor\\Base {\n/** @param int $x */\npublic function m($x) { return $x; }\n}\n";
    let report = plan(&[("c.php", c)]);
    assert_eq!(only_reason(&report), REASON_METHOD_INHERITANCE);
}

// ---- 3. Magic methods are never candidates ---------------------------------

#[test]
fn magic_method_refuses_magic() {
    let src = "<?php\nfinal class C {\n/** @param int $x */\npublic function __set($x) {}\n}\n";
    let report = plan(&[("c.php", src)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.enumerated, 1);
    assert_eq!(only_reason(&report), REASON_MAGIC_METHOD);
}

// ---- 4. Call-sweep obstacles → refusals ------------------------------------

#[test]
fn unresolved_var_receiver_taints_method_name() {
    // A `$o->m()` whose receiver class is unknown opens the target set for every
    // `m` — the eligible C::m must refuse resolution-ambiguous.
    let c = "<?php\nfinal class C {\n/** @param int $x */\npublic function m($x) { return $x; }\n}\n";
    let f = "<?php\nfunction f($o) { $o->m(1); }\n";
    let report = plan(&[("c.php", c), ("f.php", f)]);
    assert_oracle_complete(&report);
    assert_eq!(only_reason(&report), REASON_AMBIGUOUS);
    assert!(report.plan.is_empty());
}

#[test]
fn dynamic_method_name_taints_all_methods() {
    let c = "<?php\nfinal class C {\n/** @param int $x */\npublic function m($x) { return $x; }\n}\n";
    let f = "<?php\nfunction f($o, $name) { $o->$name(1); }\n";
    let report = plan(&[("c.php", c), ("f.php", f)]);
    assert_oracle_complete(&report);
    // `dynamic-call-present` — a dynamic method selector could target any method.
    assert_eq!(report.refusals.len(), 1);
    assert_eq!(report.refusals[0].reason, "dynamic-call-present");
}

#[test]
fn callable_array_reference_refuses() {
    let c = "<?php\nfinal class C {\n/** @param int $x */\npublic function m($x) { return $x; }\n}\n";
    let main = "<?php\n$cb = [new C(), 'm'];\n";
    let report = plan(&[("c.php", c), ("main.php", main)]);
    assert_oracle_complete(&report);
    assert_eq!(only_reason(&report), REASON_REFERENCED_AS_VALUE);
}

#[test]
fn callable_string_reference_refuses() {
    let c = "<?php\nfinal class C {\n/** @param int $x */\npublic function m($x) { return $x; }\n}\n";
    let main = "<?php\n$cb = 'C::m';\n";
    let report = plan(&[("c.php", c), ("main.php", main)]);
    assert_eq!(only_reason(&report), REASON_REFERENCED_AS_VALUE);
}

#[test]
fn wrong_literal_at_method_call_refuses_arg_not_proven() {
    let c = "<?php\nfinal class C {\n/** @param int $x */\npublic function m($x) { return $x; }\n}\n";
    let main = "<?php\n(new C())->m('nope');\n";
    let report = plan(&[("c.php", c), ("main.php", main)]);
    assert_eq!(only_reason(&report), REASON_ARG_NOT_PROVEN);
}

// ---- 5. By-ref / variadic method params ------------------------------------

#[test]
fn variadic_method_param_promotes_when_all_trailing_proven() {
    let c = "<?php\nfinal class C {\n/** @param int $xs */\npublic function m(...$xs) { return $xs; }\n}\n";
    let main = "<?php\n(new C())->m(1, 2, 3);\n";
    let report = plan(&[("c.php", c), ("main.php", main)]);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);
    let out = report.plan.apply_file("c.php", c);
    assert!(out.contains("public function m(int ...$xs)"), "got:\n{out}");
}

#[test]
fn variadic_method_param_refuses_on_bad_trailing_arg() {
    let c = "<?php\nfinal class C {\n/** @param int $xs */\npublic function m(...$xs) { return $xs; }\n}\n";
    let main = "<?php\n(new C())->m(1, 'str');\n";
    let report = plan(&[("c.php", c), ("main.php", main)]);
    assert_eq!(only_reason(&report), REASON_ARG_NOT_PROVEN);
}

#[test]
fn by_ref_method_param_promotes_vacuously_without_callers() {
    let c = "<?php\nfinal class C {\n/** @param int $x */\npublic function m(&$x) { $x = 1; }\n}\n";
    let report = plan(&[("c.php", c)]);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);
    let out = report.plan.apply_file("c.php", c);
    assert!(out.contains("public function m(int &$x)"), "got:\n{out}");
}

// ---- 6. Edit mechanics (docblock, multibyte) -------------------------------

#[test]
fn method_tag_deletion_leaves_valid_multiline_docblock() {
    let c = "<?php\nfinal class C {\n/**\n * @param int $x\n * @return int\n */\npublic function m($x) { return $x; }\n}\n";
    let main = "<?php\n(new C())->m(1);\n";
    let report = plan(&[("c.php", c), ("main.php", main)]);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);
    let out = report.plan.apply_file("c.php", c);
    assert!(out.contains("public function m(int $x)"), "got:\n{out}");
    assert!(out.contains("@return int"), "sibling tag kept:\n{out}");
    assert!(!out.contains("@param"), "promoted tag removed:\n{out}");
}

#[test]
fn method_multibyte_body_preserved() {
    let c = "<?php\nfinal class C {\n/** @param string $s */\npublic function greet($s) { return \"caf\u{e9} \" . $s; }\n}\n";
    let main = "<?php\n(new C())->greet(\"x\");\n";
    let report = plan(&[("c.php", c), ("main.php", main)]);
    let out = report.plan.apply_file("c.php", c);
    assert!(out.contains("public function greet(string $s)"), "got:\n{out}");
    assert!(out.contains("caf\u{e9}"), "multibyte body preserved:\n{out}");
}

// ---- 7. Free-function behavior is unchanged when methods are present --------

#[test]
fn free_function_and_method_coexist() {
    // A free function promotes while an eligible method also promotes — the two
    // sweeps do not interfere.
    let src = "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\nfinal class C {\n/** @param int $y */\npublic function m($y) { return $y; }\n}\n";
    let main = "<?php\nf(1);\n(new C())->m(2);\n";
    let report = plan(&[("lib.php", src), ("main.php", main)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.enumerated, 2);
    assert_eq!(report.oracle.transformed, 2, "{:#?}", report.refusals);
}

// ---- 8. Vendor files are never CANDIDATES (still callers/definitions) -------

#[test]
fn vendor_function_and_method_are_never_candidates() {
    // A promotable free function AND an eligible method both DEFINED in vendor/,
    // each with a valid in-project caller. Vendor code is out of the transform's
    // write contract (composer overwrites it, diagnostics are vendor-filtered), so
    // neither may be enumerated as a candidate and no edit may target vendor/.
    let vendor = "<?php\n/** @param int $x */\nfunction pxxxx_v($x) { return $x; }\nfinal class V {\n/** @param int $y */\npublic function m($y) { return $y; }\n}\n";
    let app = "<?php\npxxxx_v(1);\n(new V())->m(2);\n";
    let report = plan(&[("vendor/acme/lib.php", vendor), ("src/app.php", app)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.enumerated, 0, "vendor sites must not be enumerated: {:#?}", report);
    assert!(report.plan.is_empty(), "no edit may target vendor: {:?}", report.plan.edits);
}

#[test]
fn vendor_caller_still_blocks_a_project_candidate() {
    // The fix keeps vendor in CALLER enumeration: a vendor file calling a project
    // function with a bad literal must still refuse the project candidate.
    let app = "<?php\n/** @param int $x */\nfunction pxxxx_p($x) { return $x; }\n";
    let vendor = "<?php\npxxxx_p('bad');\n";
    let report = plan(&[("src/app.php", app), ("vendor/acme/caller.php", vendor)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.enumerated, 1);
    assert_eq!(only_reason(&report), REASON_ARG_NOT_PROVEN);
    assert!(report.plan.is_empty());
}

// ---- 9. First-class-callable method references taint the method ------------

#[test]
fn instance_first_class_callable_refuses() {
    // `$o->m(...)` (PHP 8.1 first-class callable) references m as a value: the
    // resulting Closure can be invoked with any argument later, so m's callers are
    // not enumerable and it must not promote. (Unknown receiver → resolution-ambiguous.)
    let c = "<?php\nfinal class C {\n/** @param int $x */\npublic function m($x) { return $x; }\n}\n";
    let main = "<?php\n$o = new C();\n$cb = $o->m(...);\n";
    let report = plan(&[("c.php", c), ("main.php", main)]);
    assert_oracle_complete(&report);
    assert_eq!(only_reason(&report), REASON_AMBIGUOUS);
    assert!(report.plan.is_empty());
}

#[test]
fn static_first_class_callable_refuses() {
    // `C::m(...)` — the static form of the same hole. Resolved receiver → the
    // reference is a non-positional "call" → named-or-spread refusal (never promote).
    let c = "<?php\nfinal class C {\n/** @param int $x */\npublic static function m($x) { return $x; }\n}\n";
    let main = "<?php\n$cb = C::m(...);\n";
    let report = plan(&[("c.php", c), ("main.php", main)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.transformed, 0, "must not promote a referenced method: {:#?}", report);
    assert!(report.plan.is_empty());
}
