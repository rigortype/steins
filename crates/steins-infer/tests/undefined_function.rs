//! ADR-0049 §3 / S4: `call.undefined-function` — a DAMMED absence proof.
//!
//! Fires only under complete closure with a CLEAR dam: every candidate FQN (under
//! PHP resolution order) index-Absent, not a catalog builtin, the boot surface
//! reports not-found, no dominating `function_exists` vouch, no dump/SAPI carve-out.
//! A [`Boot`] mock stands in for the runtime boot surface (there is no live sidecar
//! in tests). Every ladder leg ships with a silence fixture (the §10 discipline).

use steins_infer::{CALL_UNDEFINED_FUNCTION_ID, Diagnostic, Folder, check_with};
use steins_syntax::SourceTree;

/// A boot-surface mock: `available` is the A9/no-sidecar gate; `fns` are the
/// lowercased names the boot surface reports as resident functions (A2ii homonyms);
/// `reflect_fails` simulates a mid-run sidecar failure (Unknown for every query).
struct Boot {
    available: bool,
    fns: Vec<String>,
    reflect_fails: bool,
}

impl Boot {
    fn ready() -> Self {
        Boot { available: true, fns: Vec::new(), reflect_fails: false }
    }
    fn with_fns(names: &[&str]) -> Self {
        Boot {
            available: true,
            fns: names.iter().map(|n| n.to_ascii_lowercase()).collect(),
            reflect_fails: false,
        }
    }
}

impl Folder for Boot {
    fn fold(&mut self, _n: &str, _a: &[steins_syntax::ArgValue]) -> Option<steins_syntax::ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        self.available
    }
    fn boot_surface_function(&mut self, fqn: &str) -> Option<bool> {
        if self.reflect_fails {
            return None;
        }
        Some(self.fns.iter().any(|b| b.eq_ignore_ascii_case(fqn)))
    }
    fn boot_surface_label(&mut self) -> Option<String> {
        Some("PHP 8.5.8 (32 extensions)".to_owned())
    }
}

fn run(src: &str, folder: &mut dyn Folder) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "test.php", folder)
        .into_iter()
        .filter(|d| d.id == CALL_UNDEFINED_FUNCTION_ID)
        .collect()
}

fn fires(src: &str) -> Vec<Diagnostic> {
    run(src, &mut Boot::ready())
}

// ---------------------------------------------------------------------------
// Firing fixtures.
// ---------------------------------------------------------------------------

#[test]
fn fires_on_a_bare_undefined_global_call() {
    let d = fires("<?php\ntyop();\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("call to undefined function tyop()"), "{}", d[0].message);
    assert!(d[0].message.contains("PHP 8.5.8 (32 extensions)"), "{}", d[0].message);
}

#[test]
fn fires_on_a_namespaced_unqualified_call_both_candidates_absent() {
    // `App\tyop` and the global `tyop` both absent ⇒ fatal. Message names the
    // current-ns candidate (PHP's own phrasing).
    let d = fires("<?php\nnamespace App;\ntyop();\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("undefined function App\\tyop()"), "{}", d[0].message);
}

#[test]
fn fires_on_a_fully_qualified_absent_name() {
    let d = fires("<?php\nnamespace App;\n\\tyop();\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("undefined function tyop()"), "{}", d[0].message);
}

#[test]
fn fires_on_a_relative_namespace_call_a8() {
    // A8: `namespace\tyop()` in `App` resolves to `App\tyop` (not the doubled prefix).
    let d = fires("<?php\nnamespace App;\nnamespace\\tyop();\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("undefined function App\\tyop()"), "{}", d[0].message);
}

#[test]
fn fires_with_arguments_present() {
    // Existence is argument-shape-independent.
    let d = fires("<?php\ntyop(1, 2);\n");
    assert_eq!(d.len(), 1, "{d:?}");
}

// ---------------------------------------------------------------------------
// Silence matrix — one fixture per leg.
// ---------------------------------------------------------------------------

#[test]
fn silent_when_family_unavailable() {
    let mut b = Boot { available: false, fns: Vec::new(), reflect_fails: false };
    assert!(run("<?php\ntyop();\n", &mut b).is_empty());
}

#[test]
fn silent_when_reflect_unanswerable() {
    let mut b = Boot { available: true, fns: Vec::new(), reflect_fails: true };
    assert!(run("<?php\ntyop();\n", &mut b).is_empty());
}

#[test]
fn silent_on_defined_user_function() {
    assert!(fires("<?php\nfunction tyop() {}\ntyop();\n").is_empty());
}

#[test]
fn silent_on_a_catalog_builtin() {
    // `strlen` is a catalog builtin — never absent.
    assert!(fires("<?php\nstrlen('x');\n").is_empty());
}

#[test]
fn silent_on_boot_surface_homonym() {
    // The name is index-absent but the boot surface has it (a loaded extension fn).
    let mut b = Boot::with_fns(&["fictional_ext_fn"]);
    assert!(run("<?php\nfictional_ext_fn();\n", &mut b).is_empty());
}

#[test]
fn silent_on_polyfill_conditional_decl_with_dam() {
    // The polyfill idiom: a conditionally-declared function IS in the index (Unique),
    // so the name is never Absent ⇒ never a finding, regardless of the dam.
    let d = fires(
        "<?php\nif (!function_exists('tyop')) {\n  function tyop() {}\n}\ntyop();\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_under_a_standing_dam_eval() {
    // A5: function existence is dammed — an `eval` could mint the name.
    let d = fires("<?php\neval('function tyop(){}');\ntyop();\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_under_a_standing_dam_bare_relative_include() {
    // A5: a bare-relative include is an unproven dam site.
    let d = fires("<?php\ninclude 'config.php';\ntyop();\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_when_function_exists_vouches_on_this_branch() {
    // FP-15 guard leg: a positive `function_exists('tyop')` guard vouches the name on
    // the branch it dominates (a Maybe verdict walks both branches live).
    let d = fires("<?php\nif (function_exists('tyop')) {\n  tyop();\n}\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_first_class_callable() {
    // `tyop(...)` builds a Closure — it does not invoke.
    let d = fires("<?php\n$f = tyop(...);\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_dump_family_fqn() {
    // D3 carve-out: `PHPStan\dumpType` already reds CI with a fail-level dump.
    let d = fires("<?php\nnamespace PHPStan;\nfunction x($v){ dumpType($v); }\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_sapi_provided_function() {
    // A6: `fastcgi_finish_request` is never Absent under an undeclared SAPI.
    let d = fires("<?php\nfastcgi_finish_request();\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_apache_family_sapi_function() {
    let d = fires("<?php\napache_setenv('k', 'v');\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_ambiguous_duplicate_decl() {
    // Two decls of one FQN ⇒ Ambiguous ⇒ not Absent ⇒ silence.
    let d = fires("<?php\nfunction tyop() {}\nfunction tyop() {}\ntyop();\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_method_and_var_calls() {
    // `$o->m()` / `$fn()` are other Callee variants — never this id.
    let d = fires("<?php\n$o = new stdClass();\n$fn = 'x';\n$fn();\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_in_a_dead_branch() {
    // A provably-dead region is pruned before this check runs.
    let d = fires("<?php\nif (false) {\n  tyop();\n}\n");
    assert!(d.is_empty(), "{d:?}");
}
