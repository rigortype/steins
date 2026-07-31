//! Sidecar protocol tests: spawn a real `php` and exercise the request loop.
//!
//! These require `php` on `PATH`. When it is absent they skip with an explicit
//! stderr marker rather than failing (the runner is PHP; there is nothing to
//! test without it). In this repo's environment `php` IS present, so they run.

use std::time::Duration;

use steins_sidecar::{FoldArg, FoldKey, FoldResult, FoldValue, Sidecar};

/// An unkeyed (`ArrayKey::Auto`) array argument of `values`.
fn list(values: Vec<FoldArg>) -> FoldArg {
    FoldArg::Array(values.into_iter().map(|v| (None, v)).collect())
}

fn int(i: i64) -> FoldArg {
    FoldArg::Int(i)
}

fn s(v: &str) -> FoldArg {
    FoldArg::Str(v.to_owned())
}

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

/// Regression tripwire for the temp-dir leak this crate used to have: the
/// runner source now travels as a `php -r` argv element, so `spawn` must
/// touch no file or dir under `$TMPDIR` at all — in particular none named
/// `steins-sidecar-<ourpid>*`, the pattern the old per-instance/per-process
/// temp dir used. Scoped to our own pid so this does not trip over an
/// unrelated leftover from a different process's run.
#[test]
fn spawn_leaves_no_temp_dir_behind() {
    let Some(sc) = spawn_or_skip("spawn_leaves_no_temp_dir_behind") else { return };
    let prefix = format!("steins-sidecar-{}", std::process::id());
    let leaked: Vec<_> = std::fs::read_dir(std::env::temp_dir())
        .expect("read temp dir")
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
        .map(|entry| entry.path())
        .collect();
    assert!(leaked.is_empty(), "spawn must create no steins-sidecar-<pid>* temp entry, found: {leaked:?}");
    drop(sc);
}

/// The runner source is passed whole as a single `-r` argv element (no temp
/// file to hold it), so its size is now bounded by the OS argv limit rather
/// than disk space. Linux's `MAX_ARG_STRLEN` caps a single argv element at
/// 128 KiB (macOS's `ARG_MAX`, ~1 MiB shared across all argv+envp, is far
/// more generous). 100,000 bytes leaves comfortable headroom under the
/// tighter Linux ceiling while still catching runaway growth early.
#[test]
fn runner_size_stays_under_the_argv_limit() {
    const RUNNER_SRC: &str = include_str!("../runner.php");
    assert!(
        RUNNER_SRC.len() < 100_000,
        "runner.php is {} bytes, approaching Linux's 128 KiB MAX_ARG_STRLEN argv limit",
        RUNNER_SRC.len()
    );
}

#[test]
fn env_round_trips() {
    let Some(mut sc) = spawn_or_skip("env_round_trips") else { return };
    let env = sc.env().expect("env reply");
    assert!(env.php_version.starts_with('8'), "PHP 8.x expected, got {}", env.php_version);
    assert!(env.extensions.iter().any(|e| e == "Core" || e == "standard"), "core ext present");
    assert!(!env.sapi.is_empty());
    // The integer machine, not just the version (issue #64). A local `php` is a
    // 64-bit build; php-wasm at the same minor is not, which is the whole reason
    // this field exists.
    assert_eq!(env.int_size, Some(8), "a native php build is 64-bit");
}

/// The runner's stdout stays pure NDJSON even when the folded call emits a
/// diagnostic. `display_errors = 'stderr'` is honored only by cli/cgi, so the
/// routing goes through `log_errors`/`error_log` — a change that must be a no-op
/// here and is load-bearing under php-wasm's `embed` SAPI (issue #64).
#[test]
fn a_warning_emitting_fold_does_not_corrupt_the_stream() {
    let Some(mut sc) = spawn_or_skip("a_warning_emitting_fold_does_not_corrupt_the_stream") else {
        return;
    };
    // `1/0` raises DivisionByZeroError; `str_repeat` with a negative count is a
    // ValueError. Both are results, and both must leave the stream usable.
    assert!(matches!(
        sc.fold("str_repeat", &[s("x"), int(-1)]),
        FoldResult::Throw { .. } | FoldResult::Widen { .. }
    ));
    // The very next request still round-trips, i.e. no stray text desynced us.
    assert_eq!(
        sc.fold("strtolower", &[s("ABC")]),
        FoldResult::Value(FoldValue::Str("abc".to_owned()))
    );
    assert!(!sc.is_poisoned(), "the stream survived a diagnostic-emitting call");
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
fn reflect_reports_the_parameter_counts() {
    // ADR-0064's mixed-pin ruling: the reply carries the live signature's arity, so
    // a rule whose name declares a bare `mixed` has something to countersign
    // against. Every expectation below is the real engine's answer at PINNED_PHP.
    let Some(mut sc) = spawn_or_skip("reflect_reports_the_parameter_counts") else { return };
    let strlen = sc.reflect("strlen").expect("reflection reply");
    assert_eq!(strlen.params_total, Some(1), "strlen(string $string), {strlen:?}");
    assert_eq!(strlen.params_required, Some(1));
    // An optional tail is where total and required diverge:
    // `substr(string $string, int $offset, ?int $length = null)`.
    let substr = sc.reflect("substr").expect("reflection reply");
    assert_eq!(substr.params_total, Some(3), "substr has three parameters, {substr:?}");
    assert_eq!(substr.params_required, Some(2), "only $string and $offset are required");
    // The array read-position family: one required parameter each — the arity the
    // ADR-0064 rules pin against, measured rather than assumed.
    for name in [
        "current", "reset", "end", "next", "prev", "key", "array_pop", "array_shift",
        "array_first", "array_last",
    ] {
        let r = sc.reflect(name).expect("reflection reply");
        assert!(r.function_exists, "{name} is resident on this PHP: {r:?}");
        assert_eq!(r.params_total, Some(1), "{name} takes one parameter: {r:?}");
        assert_eq!(r.params_required, Some(1), "{name}'s parameter is required: {r:?}");
    }
}

#[test]
fn reflect_return_type_is_none_for_a_class_like() {
    // A class-like name is not a function — no return type surface.
    let Some(mut sc) = spawn_or_skip("reflect_return_type_is_none_for_a_class_like") else { return };
    let ex = sc.reflect("Exception").expect("reflection reply");
    assert!(ex.class_like_exists && !ex.function_exists);
    assert_eq!(ex.return_type, None, "a class-like carries no return type: {ex:?}");
    assert_eq!(ex.params_total, None, "a class-like carries no arity: {ex:?}");
    assert_eq!(ex.params_required, None);
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

// ---- array-literal fold arguments (issue #39) -----------------------------
//
// The wire form carries entries, not a JSON map, so PHP's own key rules apply:
// the runtime assigns absent keys and resolves duplicates. These tests run
// against real PHP precisely because that is where the semantics live.

#[test]
fn fold_count_over_a_literal_array() {
    let Some(mut sc) = spawn_or_skip("fold_count_over_a_literal_array") else { return };
    assert_eq!(sc.fold("count", &[list(vec![int(1), int(2), int(3)])]), FoldResult::Value(FoldValue::Int(3)));
    // The empty array is a value, and its count is 0 — not a widen.
    assert_eq!(sc.fold("count", &[list(vec![])]), FoldResult::Value(FoldValue::Int(0)));
}

#[test]
fn fold_in_array_and_implode_over_literal_arrays() {
    let Some(mut sc) = spawn_or_skip("fold_in_array_and_implode") else { return };
    let haystack = list(vec![int(1), int(2), int(3)]);
    assert_eq!(sc.fold("in_array", &[int(2), haystack.clone()]), FoldResult::Value(FoldValue::Bool(true)));
    assert_eq!(sc.fold("in_array", &[int(9), haystack]), FoldResult::Value(FoldValue::Bool(false)));
    assert_eq!(
        sc.fold("implode", &[s(","), list(vec![s("a"), s("b")])]),
        FoldResult::Value(FoldValue::Str("a,b".to_owned()))
    );
}

#[test]
fn fold_nested_array_arguments_round_trip() {
    let Some(mut sc) = spawn_or_skip("fold_nested_array_arguments_round_trip") else { return };
    // count() is shallow: [[1,2],[3]] has two entries.
    let nested = list(vec![list(vec![int(1), int(2)]), list(vec![int(3)])]);
    assert_eq!(sc.fold("count", std::slice::from_ref(&nested)), FoldResult::Value(FoldValue::Int(2)));
    // in_array compares the inner array by value — proof the nesting survived
    // the wire intact rather than arriving as some flattened approximation.
    assert_eq!(
        sc.fold("in_array", &[list(vec![int(1), int(2)]), nested]),
        FoldResult::Value(FoldValue::Bool(true))
    );
}

#[test]
fn php_assigns_absent_keys_and_resolves_duplicates() {
    let Some(mut sc) = spawn_or_skip("php_assigns_absent_keys_and_resolves_duplicates") else {
        return;
    };
    // A duplicate key is one entry after PHP's own last-wins assignment.
    let dup = FoldArg::Array(vec![
        (Some(FoldKey::Int(1)), s("a")),
        (Some(FoldKey::Int(1)), s("b")),
    ]);
    assert_eq!(sc.fold("count", std::slice::from_ref(&dup)), FoldResult::Value(FoldValue::Int(1)));
    assert_eq!(sc.fold("implode", &[s(""), dup]), FoldResult::Value(FoldValue::Str("b".to_owned())));

    // Mixed explicit and absent keys: the runtime's next-int rule places 'c'.
    let mixed = FoldArg::Array(vec![
        (Some(FoldKey::Str("x".to_owned())), s("a")),
        (Some(FoldKey::Int(5)), s("b")),
        (None, s("c")),
    ]);
    assert_eq!(sc.fold("count", std::slice::from_ref(&mixed)), FoldResult::Value(FoldValue::Int(3)));
    assert_eq!(
        sc.fold("implode", &[s(","), mixed]),
        FoldResult::Value(FoldValue::Str("a,b,c".to_owned()))
    );
}

/// Rebuilding an array literal runs PHP's own key rules, and those rules can
/// THROW: `[PHP_INT_MAX => 'a', 'b']` raises "Cannot add element to the array as
/// the next element is already occupied". Before issue #64 S1.5 that Error escaped
/// `steins_fold` as an uncaught FATAL, killing the resident runner mid-NDJSON and
/// widening every later request in the run. It must widen, and the process must
/// survive — the runner's standing contract that any misuse widens.
///
/// The threshold is the engine's OWN `PHP_INT_MAX`, so on php-wasm's 32-bit build
/// it is 2147483647 — a key inside what the fold seam's width guard admits, and
/// one a human plausibly writes.
#[test]
fn an_overflowing_next_int_key_widens_and_leaves_the_runner_alive() {
    let Some(mut sc) = spawn_or_skip("an_overflowing_next_int_key_widens_and_leaves_the_runner_alive")
    else {
        return;
    };
    let overflowing =
        FoldArg::Array(vec![(Some(FoldKey::Int(i64::MAX)), s("a")), (None, s("b"))]);
    let r = sc.fold("count", std::slice::from_ref(&overflowing));
    assert!(matches!(r, FoldResult::Widen { .. }), "an unassignable next key widens, got {r:?}");
    assert!(!sc.is_poisoned(), "widening is not a protocol failure");
    // The same process answers the next question — the fatal is gone.
    assert_eq!(
        sc.fold("strtoupper", &[s("still alive")]),
        FoldResult::Value(FoldValue::Str("STILL ALIVE".to_owned()))
    );
    // And the boundary below it is an ordinary, answerable array.
    let ok = FoldArg::Array(vec![(Some(FoldKey::Int(i64::MAX - 1)), s("a")), (None, s("b"))]);
    assert_eq!(sc.fold("count", &[ok]), FoldResult::Value(FoldValue::Int(2)));
}

#[test]
fn an_array_returning_fold_widens() {
    let Some(mut sc) = spawn_or_skip("an_array_returning_fold_widens") else { return };
    // Array *arguments* exist; an array *result* is a documented boundary
    // (#41/#42) — the runner reports it faithfully, the Rust side widens.
    let r = sc.fold("str_replace", &[s("a"), s("b"), list(vec![s("a")])]);
    assert!(matches!(r, FoldResult::Widen { .. }), "array result widens, got {r:?}");
    assert!(!sc.is_poisoned(), "a widened result is not a protocol failure");
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
fn timeout_poisons_and_the_lost_request_widens() {
    let Some(mut sc) = spawn_or_skip("timeout_poisons") else { return };
    // Force the timeout path with a tiny deadline against a deliberately slow
    // call. `usleep` is not on the fold allowlist, but the runner does not gate
    // — the Rust side does — so this is a valid way to exercise the protocol.
    sc.set_timeout(Duration::from_millis(20));
    let r = sc.fold("usleep", &[FoldArg::Int(1_000_000)]); // 1s > 20ms
    assert!(matches!(r, FoldResult::Widen { .. }), "timeout widens, got {r:?}");
    assert!(sc.is_poisoned(), "timeout poisons the instance");
    // The timed-out request is lost for good: it is never re-sent to the
    // replacement child, because it is the request that misbehaved.
    //
    // The next request is a different matter — it revives the instance. Give it
    // a deadline a fresh `php` can actually meet (a respawn pays a full PHP
    // startup, tens of milliseconds, well past the 20ms this test forced).
    sc.set_timeout(Duration::from_secs(2));
    assert_eq!(
        sc.fold("strtolower", &[FoldArg::Str("ABC".to_owned())]),
        FoldResult::Value(FoldValue::Str("abc".to_owned())),
        "the next request respawns and answers"
    );
    assert!(!sc.is_poisoned(), "a revived instance is not poisoned");
}

/// A `str_repeat` whose result exceeds the runner's `memory_limit` dies as an
/// *uncatchable* fatal: memory exhaustion is not a `Throwable`, so unlike the
/// unassignable-array-key case above, no `catch` in the runner can turn it into
/// a widen. The child simply stops mid-NDJSON.
///
/// The call is ordinary source — `str_repeat("x", 2000000000)` is a literal call
/// on the folding allowlist — so the answer must be a widen and the *next*
/// request must still be answerable. That second half is the respawn: the same
/// `Sidecar` is now talking to a different process.
#[test]
fn a_memory_exhausting_fold_widens_and_the_next_request_still_answers() {
    let Some(mut sc) = spawn_or_skip("a_memory_exhausting_fold_widens_and_the_next_request_answers")
    else {
        return;
    };
    let r = sc.fold("str_repeat", &[s("x"), int(2_000_000_000)]);
    assert!(matches!(r, FoldResult::Widen { .. }), "a memory bomb widens, got {r:?}");
    assert!(sc.is_poisoned(), "the child died, so the transport is poisoned");
    // The lost answer costs one request, not the rest of the run.
    assert_eq!(
        sc.fold("strtoupper", &[s("still alive")]),
        FoldResult::Value(FoldValue::Str("STILL ALIVE".to_owned()))
    );
    assert!(!sc.is_poisoned(), "the respawned child is healthy");
}

/// The storm brake. Recovery is bounded at three respawns per `Sidecar`, so
/// input engineered to kill children cannot buy an unbounded number of PHP
/// startups. Past the cap the instance is what it was before recovery existed:
/// permanently poisoned, every request widening immediately.
#[test]
fn the_respawn_cap_bounds_recovery_and_then_poisons_permanently() {
    let Some(mut sc) = spawn_or_skip("the_respawn_cap_bounds_recovery") else { return };
    let bomb = [s("x"), int(2_000_000_000)];

    // Three bombs, three recoveries: each one widens, and the next fold answers.
    for i in 0..3 {
        assert!(matches!(sc.fold("str_repeat", &bomb), FoldResult::Widen { .. }), "bomb {i} widens");
        assert_eq!(
            sc.fold("strtoupper", &[s("alive")]),
            FoldResult::Value(FoldValue::Str("ALIVE".to_owned())),
            "respawn {i} answered"
        );
    }

    // The fourth bomb kills the third replacement, and there is no fourth.
    assert!(matches!(sc.fold("str_repeat", &bomb), FoldResult::Widen { .. }), "the last bomb widens");
    assert!(sc.is_poisoned());
    let start = std::time::Instant::now();
    for _ in 0..5 {
        let r = sc.fold("strtoupper", &[s("alive")]);
        assert!(matches!(r, FoldResult::Widen { .. }), "past the cap every fold widens, got {r:?}");
    }
    // Widening past the cap touches no process at all, so it cannot cost a
    // timeout. The bound is loose on purpose — this asserts "no hang", not a
    // performance figure.
    assert!(start.elapsed() < Duration::from_secs(1), "a capped sidecar widens without waiting");
    assert!(sc.is_poisoned(), "the poison is permanent now");
}
