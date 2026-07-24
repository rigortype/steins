//! Sidecar protocol tests: spawn a real `php` and exercise the request loop.
//!
//! These require `php` on `PATH`. When it is absent they skip with an explicit
//! stderr marker rather than failing (the runner is PHP; there is nothing to
//! test without it). In this repo's environment `php` IS present, so they run.

use std::time::Duration;

use steins_sidecar::{FoldArg, FoldResult, FoldValue, Sidecar};

/// Spawn a sidecar, or print a skip marker and return `None` if `php` is absent.
fn spawn_or_skip(test: &str) -> Option<Sidecar> {
    match Sidecar::spawn() {
        Ok(s) => Some(s),
        Err(e) => {
            eprintln!("SKIP {test}: could not spawn php sidecar ({e}) — is `php` on PATH?");
            None
        }
    }
}

#[test]
fn env_round_trips() {
    let Some(mut sc) = spawn_or_skip("env_round_trips") else { return };
    let env = sc.env().expect("env reply");
    assert!(env.php_version.starts_with('8'), "PHP 8.x expected, got {}", env.php_version);
    assert!(env.extensions.iter().any(|e| e == "Core" || e == "standard"), "core ext present");
    assert!(!env.sapi.is_empty());
}

#[test]
fn reflect_finds_a_builtin_function() {
    let Some(mut sc) = spawn_or_skip("reflect_finds_a_builtin_function") else { return };
    let r = sc.reflect("strlen").expect("reflection reply");
    assert!(r.function_exists, "strlen is a builtin function");
    assert!(!r.class_like_exists, "strlen is not a class-like");
    assert!(r.exists());
}

#[test]
fn reflect_finds_a_builtin_class_like() {
    let Some(mut sc) = spawn_or_skip("reflect_finds_a_builtin_class_like") else { return };
    // A builtin class and a builtin interface both count as class-like.
    let ex = sc.reflect("Exception").expect("reflection reply");
    assert!(ex.class_like_exists && !ex.function_exists, "Exception is a class, {ex:?}");
    let iface = sc.reflect("Countable").expect("reflection reply");
    assert!(iface.class_like_exists, "Countable is an interface, {iface:?}");
    // A leading backslash resolves to the same symbol.
    assert!(sc.reflect("\\Throwable").expect("reply").class_like_exists);
}

#[test]
fn reflect_reports_the_native_return_type() {
    // ADR-0056 R1: the reflection reply carries the builtin's native return type.
    let Some(mut sc) = spawn_or_skip("reflect_reports_the_native_return_type") else { return };
    // A bool predicate — the R1 family. getReturnType() is `bool`, non-tentative.
    let is_int = sc.reflect("is_int").expect("reflection reply");
    assert_eq!(is_int.return_type.as_deref(), Some("bool"), "is_int returns bool, {is_int:?}");
    assert!(!is_int.return_type_tentative, "is_int has a real (non-tentative) return type");
    // A single-base int producer and a string producer — the envelope-seeding cases.
    assert_eq!(sc.reflect("strlen").expect("reply").return_type.as_deref(), Some("int"));
    assert_eq!(sc.reflect("sha1").expect("reply").return_type.as_deref(), Some("string"));
    // A multi-base union return — surfaced faithfully as a string (the consumer
    // decides it is not single-fact-representable).
    assert_eq!(sc.reflect("strpos").expect("reply").return_type.as_deref(), Some("int|false"));
}

#[test]
fn reflect_return_type_is_none_for_a_class_like() {
    // A class-like name is not a function — no return type surface.
    let Some(mut sc) = spawn_or_skip("reflect_return_type_is_none_for_a_class_like") else { return };
    let ex = sc.reflect("Exception").expect("reflection reply");
    assert!(ex.class_like_exists && !ex.function_exists);
    assert_eq!(ex.return_type, None, "a class-like carries no return type: {ex:?}");
}

#[test]
fn reflect_reports_a_nonsense_name_as_not_found() {
    let Some(mut sc) = spawn_or_skip("reflect_reports_a_nonsense_name_as_not_found") else {
        return;
    };
    // A structured not-found — Some, exists() == false — never None (None is a
    // failed query, this is a definitive answer).
    let r = sc.reflect("steins_no_such_symbol_xyzzy").expect("reflection reply");
    assert!(!r.exists(), "nonsense name must not exist: {r:?}");
    assert!(!r.function_exists && !r.class_like_exists);
}

#[test]
fn env_extension_list_is_non_empty() {
    // A9 consults the loaded-extension list; `env` already carries it.
    let Some(mut sc) = spawn_or_skip("env_extension_list_is_non_empty") else { return };
    let env = sc.env().expect("env reply");
    assert!(!env.extensions.is_empty(), "loaded extensions must be reported");
}

#[test]
fn fold_strtolower_returns_value() {
    let Some(mut sc) = spawn_or_skip("fold_strtolower_returns_value") else { return };
    let r = sc.fold("strtolower", &[FoldArg::Str("ABC".to_owned())]);
    assert_eq!(r, FoldResult::Value(FoldValue::Str("abc".to_owned())));
}

#[test]
fn fold_preserves_float_and_int_types() {
    let Some(mut sc) = spawn_or_skip("fold_preserves_float_and_int_types") else { return };
    // strlen → int
    assert_eq!(
        sc.fold("strlen", &[FoldArg::Str("hello".to_owned())]),
        FoldResult::Value(FoldValue::Int(5))
    );
    // abs(-3.5) → float 3.5 (stays a float, JSON_PRESERVE_ZERO_FRACTION path)
    assert_eq!(
        sc.fold("abs", &[FoldArg::Float(-3.5)]),
        FoldResult::Value(FoldValue::Float(3.5))
    );
    // abs(-2.0) → float 2.0, still a float, not an int
    assert_eq!(sc.fold("abs", &[FoldArg::Float(-2.0)]), FoldResult::Value(FoldValue::Float(2.0)));
}

#[test]
fn fold_divide_by_zero_is_a_throw() {
    let Some(mut sc) = spawn_or_skip("fold_divide_by_zero_is_a_throw") else { return };
    let r = sc.fold("intdiv", &[FoldArg::Int(1), FoldArg::Int(0)]);
    assert_eq!(r, FoldResult::Throw { class: "DivisionByZeroError".to_owned() });
}

#[test]
fn fold_unknown_function_widens() {
    let Some(mut sc) = spawn_or_skip("fold_unknown_function_widens") else { return };
    let r = sc.fold("steins_no_such_function_xyz", &[]);
    assert!(matches!(r, FoldResult::Widen { .. }), "unknown fn widens, got {r:?}");
}

#[test]
fn fold_wrong_arity_widens() {
    let Some(mut sc) = spawn_or_skip("fold_wrong_arity_widens") else { return };
    // strlen() with no args → ArgumentCountError → widen (structural misuse).
    let r = sc.fold("strlen", &[]);
    assert!(matches!(r, FoldResult::Widen { .. }), "wrong arity widens, got {r:?}");
}

#[test]
fn process_is_reused_across_many_folds() {
    let Some(mut sc) = spawn_or_skip("process_is_reused_across_many_folds") else { return };
    // Same resident process answers request after request (incremental ids).
    for i in 0..50 {
        let s = format!("VALUE{i}");
        let r = sc.fold("strtolower", &[FoldArg::Str(s.clone())]);
        assert_eq!(r, FoldResult::Value(FoldValue::Str(s.to_lowercase())));
    }
    assert!(!sc.is_poisoned());
}

#[test]
fn timeout_poisons_and_subsequent_calls_widen_fast() {
    let Some(mut sc) = spawn_or_skip("timeout_poisons") else { return };
    // Force the timeout path with a tiny deadline against a deliberately slow
    // call. `usleep` is not on the fold allowlist, but the runner does not gate
    // — the Rust side does — so this is a valid way to exercise the protocol.
    sc.set_timeout(Duration::from_millis(20));
    let r = sc.fold("usleep", &[FoldArg::Int(1_000_000)]); // 1s > 20ms
    assert!(matches!(r, FoldResult::Widen { .. }), "timeout widens, got {r:?}");
    assert!(sc.is_poisoned(), "timeout poisons the instance");
    // A poisoned instance widens immediately without touching the (dead) child.
    let r2 = sc.fold("strtolower", &[FoldArg::Str("ABC".to_owned())]);
    assert!(matches!(r2, FoldResult::Widen { .. }), "poisoned widens, got {r2:?}");
}
