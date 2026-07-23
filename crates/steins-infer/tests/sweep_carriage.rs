//! ADR-0047 Slice B carriage tests: the reverse sweeps now record a *site* for
//! every caller-enumeration obstacle (`dynamic_call_sites`, the `name → sites`
//! taint maps) instead of a bare boolean / bare-name set. This slice is
//! behavior-preserving — key-presence still means "tainted" and a non-empty site
//! list still means "an obstacle stands" — so these tests assert the two things
//! Slice B adds: the *right site* is recorded by each producer, and sites
//! *accumulate* across multiple occurrences (which a set/boolean could not carry).

use steins_db::{Project, SourceFile, SteinsDatabase};
use steins_infer::promote::{FreeFnSweep, MethodSweep, sweep_free_functions, sweep_methods};

fn free_sweep(files: &[(&str, &str)]) -> FreeFnSweep {
    let db = SteinsDatabase::default();
    let inputs: Vec<SourceFile> =
        files.iter().map(|(p, t)| SourceFile::new(&db, (*p).to_owned(), (*t).to_owned())).collect();
    let project = Project::new(&db, inputs);
    sweep_free_functions(&db, project)
}

fn method_sweep(files: &[(&str, &str)]) -> MethodSweep {
    let db = SteinsDatabase::default();
    let inputs: Vec<SourceFile> =
        files.iter().map(|(p, t)| SourceFile::new(&db, (*p).to_owned(), (*t).to_owned())).collect();
    let project = Project::new(&db, inputs);
    sweep_methods(&db, project)
}

// ---- Free-function producers ----------------------------------------------

#[test]
fn dynamic_free_call_records_its_site() {
    // `$fn()` on line 3 → one dynamic-call site at that location.
    let src = "<?php\nfunction run($fn) {\n  return $fn(1);\n}\n";
    let sweep = free_sweep(&[("app.php", src)]);
    assert_eq!(sweep.dynamic_call_sites.len(), 1, "one `$fn()` call");
    let site = &sweep.dynamic_call_sites[0];
    assert_eq!(site.path, "app.php");
    assert_eq!(site.line, 3, "the `$fn(1)` line");
}

#[test]
fn generic_invoker_with_opaque_callable_records_a_dynamic_site() {
    // `call_user_func($cb)` where $cb is an opaque runtime value → a dynamic site
    // (it could invoke any free function), exactly as a bare `$fn()` would.
    let src = "<?php\nfunction run($cb) {\n  return call_user_func($cb, 1);\n}\n";
    let sweep = free_sweep(&[("inv.php", src)]);
    assert_eq!(sweep.dynamic_call_sites.len(), 1, "opaque call_user_func taints");
    assert_eq!(sweep.dynamic_call_sites[0].line, 3);
}

#[test]
fn value_referenced_name_records_its_site_keyed_by_name() {
    // A function name flowing as a string value (`'strlen'`) is a caller invisible
    // to resolution; the name is keyed and the reference site recorded.
    let src = "<?php\nfunction run($a) {\n  return array_map('strlen', $a);\n}\n";
    let sweep = free_sweep(&[("map.php", src)]);
    let sites = sweep.value_referenced_names.get("strlen").expect("`strlen` keyed as a value");
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].line, 3);
    assert_eq!(sites[0].path, "map.php");
}

#[test]
fn value_referenced_name_through_a_scope_assignment_records_the_stmt_site() {
    // `$g = 'foo';` escapes through a non-call value position — the scope-trace scan
    // records it at the assignment statement.
    let src = "<?php\nfunction foo($x) { return $x; }\nfunction run() {\n  $g = 'foo';\n  return $g;\n}\n";
    let sweep = free_sweep(&[("esc.php", src)]);
    let sites = sweep.value_referenced_names.get("foo").expect("`foo` keyed as a value");
    assert_eq!(sites[0].line, 4, "the `$g = 'foo';` statement line");
}

#[test]
fn unresolved_simple_name_records_its_call_site() {
    // A call to a function that resolves to no unique user function taints its
    // simple name with the call site.
    let src = "<?php\nfunction run() {\n  return totally_unknown_fn();\n}\n";
    let sweep = free_sweep(&[("u.php", src)]);
    let sites =
        sweep.unresolved_simple_names.get("totally_unknown_fn").expect("unresolved name keyed");
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].line, 3);
}

#[test]
fn dynamic_free_call_sites_accumulate_across_files() {
    // Two `$fn()` calls in two files → two accumulated sites (a boolean could not).
    let a = "<?php\nfunction ra($fn) { return $fn(1); }\n";
    let b = "<?php\nfunction rb($fn) { return $fn(2); }\n";
    let sweep = free_sweep(&[("a.php", a), ("b.php", b)]);
    assert_eq!(sweep.dynamic_call_sites.len(), 2, "sites accumulate, not a boolean");
    let mut paths: Vec<&str> = sweep.dynamic_call_sites.iter().map(|s| s.path.as_str()).collect();
    paths.sort_unstable();
    assert_eq!(paths, vec!["a.php", "b.php"]);
}

#[test]
fn clean_free_project_records_no_obstacle_sites() {
    // Non-empty == the old `true`; a clean project must stay empty (== the old
    // `false`), so consumers still see "enumerable".
    let src = "<?php\nfunction foo($x) { return $x; }\nfoo(1);\n";
    let sweep = free_sweep(&[("clean.php", src)]);
    assert!(sweep.dynamic_call_sites.is_empty());
    assert!(sweep.value_referenced_names.is_empty());
    assert!(sweep.unresolved_simple_names.is_empty());
}

// ---- Method producers ------------------------------------------------------

#[test]
fn dynamic_method_call_records_its_site() {
    // `$o->$m()` — a dynamic method selector — taints every method with its site.
    let src =
        "<?php\nclass C {\n  public function run($o, $m) {\n    return $o->$m(1);\n  }\n}\n";
    let sweep = method_sweep(&[("c.php", src)]);
    assert_eq!(sweep.dynamic_method_sites.len(), 1, "one `$o->$m()` selector");
    assert_eq!(sweep.dynamic_method_sites[0].line, 4);
    assert_eq!(sweep.dynamic_method_sites[0].path, "c.php");
}

#[test]
fn literal_callable_array_records_the_method_name_site() {
    // `[$o, 'cmp']` names a method as a value → keyed by `cmp` with its site.
    let src = "<?php\nclass C {\n  public function run($o, $a) {\n    return usort($a, [$o, 'cmp']);\n  }\n}\n";
    let sweep = method_sweep(&[("cb.php", src)]);
    let sites = sweep.value_referenced_methods.get("cmp").expect("`cmp` keyed as a value");
    assert_eq!(sites[0].line, 4);
    assert_eq!(sites[0].path, "cb.php");
}

#[test]
fn nonliteral_callable_array_records_a_dynamic_site() {
    // `[$o, $m]` — the method-name position is not a literal → taint broadly, as a
    // dynamic method site (issue-#6 gap), recording where it occurred.
    let src = "<?php\nclass C {\n  public function run($o, $m, $a) {\n    return usort($a, [$o, $m]);\n  }\n}\n";
    let sweep = method_sweep(&[("cb2.php", src)]);
    assert_eq!(sweep.dynamic_method_sites.len(), 1, "non-literal callable array taints");
    assert_eq!(sweep.dynamic_method_sites[0].line, 4);
}

#[test]
fn unresolved_method_name_keeps_the_first_site_as_representative() {
    // Two unknown-receiver `->probe()` calls (lines 4 and 5): both are recorded, but
    // the first (source order) stays the representative the refusal names — the
    // byte-identical pre-Slice-B behavior.
    let src = "<?php\nclass C {\n  public function run($x, $y) {\n    $x->probe();\n    $y->probe();\n  }\n}\n";
    let sweep = method_sweep(&[("m.php", src)]);
    let sites = sweep.unresolved_method_names.get("probe").expect("`probe` tainted");
    assert_eq!(sites.len(), 2, "both unresolved sites accumulate");
    assert_eq!(sites[0].line, 4, "the first call is the representative");
    assert_eq!(sites[1].line, 5);
}

#[test]
fn clean_method_project_records_no_obstacle_sites() {
    let src = "<?php\nfinal class C {\n  private function m($x) { return $x; }\n  public function run() { return $this->m(1); }\n}\n";
    let sweep = method_sweep(&[("clean.php", src)]);
    assert!(sweep.dynamic_method_sites.is_empty());
    assert!(sweep.value_referenced_methods.is_empty());
    assert!(sweep.unresolved_method_names.is_empty());
}
