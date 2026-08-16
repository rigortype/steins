//! Sidecar protocol tests: spawn a real `php` and exercise the request loop.
//! Require `php` on `PATH`; when absent they skip with a stderr marker rather
//! than fail. `php` IS present in this repo's environment, so they run.

use std::time::Duration;

use steins_sidecar::{
    ConstantDefined, FoldArg, FoldKey, FoldResult, FoldValue, PregCompile, ReflectedClassKind,
    Sidecar,
};

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

/// Regression tripwire: `spawn` touches no file/dir under `$TMPDIR`, scoped to our pid.
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

/// OS argv limit: Linux caps at 128 KiB, macOS `ARG_MAX` ~1 MiB; 100,000 bytes is headroom.
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
    // Pins the actual machine (issue #64): local `php` is 64-bit, php-wasm isn't.
    assert_eq!(env.int_size, Some(8), "a native php build is 64-bit");
}

/// stdout stays pure NDJSON via `log_errors` routing, load-bearing for php-wasm (issue #64).
#[test]
fn a_warning_emitting_fold_does_not_corrupt_the_stream() {
    let Some(mut sc) = spawn_or_skip("a_warning_emitting_fold_does_not_corrupt_the_stream") else {
        return;
    };
    // `str_repeat` with a negative count is a ValueError; must leave the stream usable.
    assert!(matches!(
        sc.fold("str_repeat", &[s("x"), int(-1)], true),
        FoldResult::Throw { .. } | FoldResult::Widen { .. }
    ));
    assert_eq!(
        sc.fold("strtolower", &[s("ABC")], true),
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
    let is_int = sc.reflect("is_int").expect("reflection reply");
    assert_eq!(is_int.return_type.as_deref(), Some("bool"), "is_int returns bool, {is_int:?}");
    assert!(!is_int.return_type_tentative, "is_int has a real (non-tentative) return type");
    assert_eq!(sc.reflect("strlen").expect("reply").return_type.as_deref(), Some("int"));
    assert_eq!(sc.reflect("sha1").expect("reply").return_type.as_deref(), Some("string"));
    // A multi-base union — surfaced faithfully as a string (not single-fact-representable).
    assert_eq!(sc.reflect("strpos").expect("reply").return_type.as_deref(), Some("int|false"));
}

#[test]
fn reflect_reports_the_parameter_counts() {
    // ADR-0064: the reply carries the live arity, answers below at PINNED_PHP.
    let Some(mut sc) = spawn_or_skip("reflect_reports_the_parameter_counts") else { return };
    let strlen = sc.reflect("strlen").expect("reflection reply");
    assert_eq!(strlen.params_total, Some(1), "strlen(string $string), {strlen:?}");
    assert_eq!(strlen.params_required, Some(1));
    let substr = sc.reflect("substr").expect("reflection reply");
    assert_eq!(substr.params_total, Some(3), "substr has three parameters, {substr:?}");
    assert_eq!(substr.params_required, Some(2), "only $string and $offset are required");
    // Array read-position family: one required param each; 8/10 always resident (ADR-0064).
    for name in ["current", "reset", "end", "next", "prev", "key", "array_pop", "array_shift"] {
        let r = sc.reflect(name).expect("reflection reply");
        assert!(r.function_exists, "{name} is resident on this PHP: {r:?}");
        assert_eq!(r.params_total, Some(1), "{name} takes one parameter: {r:?}");
        assert_eq!(r.params_required, Some(1), "{name}'s parameter is required: {r:?}");
    }
    // `array_first`/`array_last`: PHP 8.5 additions (CI 8.4, local 8.5) — residency
    // asserted against the LIVE minor (ADR-0069); absent asserts not-found.
    let env = sc.env().expect("env reply");
    let minor = php_minor(&env.php_version);
    for name in ["array_first", "array_last"] {
        let r = sc.reflect(name).expect("reflection reply");
        if minor >= (8, 5) {
            assert!(r.function_exists, "{name} is resident on PHP {} (>=8.5): {r:?}", env.php_version);
            assert_eq!(r.params_total, Some(1), "{name} takes one parameter: {r:?}");
            assert_eq!(r.params_required, Some(1), "{name}'s parameter is required: {r:?}");
        } else {
            assert!(
                !r.function_exists && !r.class_like_exists,
                "{name} does not exist on PHP {} (added in 8.5): {r:?}",
                env.php_version
            );
            assert!(!r.exists(), "{name} is a structured not-found on PHP {}: {r:?}", env.php_version);
            assert_eq!(r.params_total, None, "no arity for a name that is not resident: {r:?}");
            assert_eq!(r.params_required, None, "no arity for a name that is not resident: {r:?}");
        }
    }
}

#[test]
fn reflect_reports_the_parameter_list() {
    // ADR-0056 §9: the reply carries `getParameters()` per position — the source
    // the builtin-argument judgment reads. Every answer below is the live engine's.
    let Some(mut sc) = spawn_or_skip("reflect_reports_the_parameter_list") else { return };
    let strlen = sc.reflect("strlen").expect("reflection reply");
    let p = strlen.params.clone().expect("strlen has a parameter list");
    assert_eq!(p.len(), 1, "strlen(string $string): {strlen:?}");
    assert_eq!(p[0].name, "string");
    assert_eq!(p[0].ty.as_deref(), Some("string"));
    assert!(!p[0].by_ref && !p[0].variadic && !p[0].optional);

    // The by-ref out-parameter (`preg_match`'s `$matches`) and the variadic tail
    // (`sprintf`'s `$values`) — the two shapes the judgment declines on.
    let matches = &sc.reflect("preg_match").expect("reply").params.expect("params")[2];
    assert_eq!(matches.name, "matches", "preg_match's third parameter: {matches:?}");
    assert!(matches.by_ref, "$matches is an out-parameter: {matches:?}");
    let sprintf = sc.reflect("sprintf").expect("reply").params.expect("params");
    assert_eq!(sprintf[0].ty.as_deref(), Some("string"), "sprintf(string $format, …)");
    assert!(sprintf.last().expect("a tail").variadic, "sprintf's tail is variadic: {sprintf:?}");

    // `mixed` travels verbatim; the consumer is what declines on it.
    assert_eq!(
        sc.reflect("var_dump").expect("reply").params.expect("params")[0].ty.as_deref(),
        Some("mixed"),
    );
    // An optional position with a default, and a union spelling.
    let substr = sc.reflect("substr").expect("reply").params.expect("params");
    assert_eq!(substr[2].ty.as_deref(), Some("?int"), "substr's $length: {substr:?}");
    assert!(substr[2].optional, "substr's $length has a default: {substr:?}");
    assert_eq!(
        sc.reflect("str_replace").expect("reply").params.expect("params")[0].ty.as_deref(),
        Some("array|string"),
    );

    // A name that is not resident is a structured not-found: no list, never an
    // empty one — the same rule the counts follow.
    let missing = sc.reflect("steins_no_such_function").expect("reflection reply");
    assert!(!missing.exists(), "{missing:?}");
    assert_eq!(missing.params, None, "no list for a name that is not resident: {missing:?}");
    // A class-like carries none either.
    assert_eq!(sc.reflect("Exception").expect("reply").params, None);
}

/// `Random\Randomizer` (ext-random, 8.2+) has no catalog row (issue #269, CI 8.4/local 8.5).
#[test]
fn reflect_class_reads_an_extension_class_declaration() {
    let Some(mut sc) = spawn_or_skip("reflect_class_reads_an_extension_class_declaration") else {
        return;
    };
    let r = sc.reflect_class("Random\\Randomizer").expect("class reflection reply");
    let Some(d) = r.declaration else {
        eprintln!("SKIP reflect_class_reads_an_extension_class_declaration: ext-random absent");
        return;
    };
    assert_eq!(d.name, "Random\\Randomizer");
    assert_eq!(d.kind, ReflectedClassKind::Class);
    assert!(d.internal, "an extension class is internal: {d:?}");
    assert_eq!(d.extension.as_deref(), Some("random"), "the origin travels: {d:?}");
    // Member surfaces the catalog can't supply (no rows at all): a method + a property.
    let get_int = d.methods.iter().find(|m| m.name == "getInt").expect("getInt is declared");
    assert_eq!(get_int.params_required, 2, "getInt(int $min, int $max): {get_int:?}");
    assert_eq!(get_int.return_type.as_deref(), Some("int"), "{get_int:?}");
    assert!(d.properties.iter().any(|p| p.name == "engine"), "the engine property: {d:?}");
}

/// `ArrayObject` is SPL and always resident — carries both constants and hierarchy edges.
#[test]
fn reflect_class_reads_constants_and_hierarchy_edges() {
    let Some(mut sc) = spawn_or_skip("reflect_class_reads_constants_and_hierarchy_edges") else {
        return;
    };
    let d = sc
        .reflect_class("ArrayObject")
        .expect("class reflection reply")
        .declaration
        .expect("ArrayObject is resident on every supported PHP");
    assert!(d.constants.iter().any(|c| c.name == "ARRAY_AS_PROPS"), "class constants: {d:?}");
    assert!(d.interfaces.iter().any(|i| i == "Countable"), "hierarchy edges: {d:?}");
    assert!(d.methods.iter().any(|m| m.name == "count"), "methods: {d:?}");
    // A leading backslash resolves to the same symbol, as it does for `reflect`.
    assert!(sc.reflect_class("\\ArrayObject").expect("reply").exists());
}

/// A **structured not-found**, never a decline — same distinction `reflect` draws.
#[test]
fn reflect_class_reports_an_absent_class_as_a_structured_not_found() {
    let Some(mut sc) = spawn_or_skip("reflect_class_reports_an_absent_class") else { return };
    let r = sc.reflect_class("Steins\\NoSuchClass269").expect("an answer, not a decline");
    assert!(!r.exists(), "{r:?}");
    assert_eq!(r.declaration, None);
}

/// The runner never autoloads — a non-resident class is a not-found, not a load.
#[test]
fn reflect_class_never_autoloads() {
    let Some(mut sc) = spawn_or_skip("reflect_class_never_autoloads") else { return };
    let r = sc.reflect_class("App\\Kernel").expect("an answer");
    assert!(!r.exists(), "no project class is reachable from the sidecar: {r:?}");
    assert!(!sc.is_poisoned(), "asking about a userland name is not a transport failure");
}

/// **Fault injection**: sidecar dies mid-run during class queries, degrading to
/// `None` (never empty-members) — one answer per death, decline past the cap.
/// Killed via *timeout*, not memory: no allocation cost (four 256 MB bombs already run).
#[test]
fn a_dead_sidecar_declines_a_class_query_and_the_next_one_answers() {
    let Some(mut sc) = spawn_or_skip("a_dead_sidecar_declines_a_class_query") else { return };
    let quick = Duration::from_millis(20);
    let generous = Duration::from_secs(2);

    for i in 0..3 {
        sc.set_timeout(quick);
        let r = sc.fold("usleep", &[int(1_000_000)], true); // 1s > 20ms
        assert!(matches!(r, FoldResult::Widen { .. }), "death {i} widens, got {r:?}");
        assert!(sc.is_poisoned(), "death {i} poisoned the instance");
        // The next request (a class query) revives the transport, as a fold's would.
        sc.set_timeout(generous);
        let revived = sc.reflect_class("ArrayObject").expect("the revived child answers");
        assert!(revived.exists(), "respawn {i} answered: {revived:?}");
    }

    sc.set_timeout(quick);
    assert!(matches!(sc.fold("usleep", &[int(1_000_000)], true), FoldResult::Widen { .. }));
    sc.set_timeout(generous);
    assert!(sc.is_poisoned(), "the respawn budget is spent");
    assert_eq!(
        sc.reflect_class("ArrayObject"),
        None,
        "a spent transport declines — Unknown, never an empty declaration"
    );
}

/// Same story as a dead child: `None`, and the instance is poisoned, not trusted.
#[test]
fn a_timed_out_class_query_declines() {
    let Some(mut sc) = spawn_or_skip("a_timed_out_class_query_declines") else { return };
    sc.set_timeout(Duration::from_millis(1));
    // A 1ms deadline: whichever side loses, decline is the only admissible outcome.
    match sc.reflect_class("ArrayObject") {
        None => assert!(sc.is_poisoned(), "a lost reply poisons the instance"),
        Some(r) => assert!(r.exists(), "if it did answer in 1ms, it answered correctly: {r:?}"),
    }
    sc.set_timeout(Duration::from_secs(2));
    assert!(
        sc.reflect_class("ArrayObject").expect("the next request revives").exists(),
        "recovery is transparent"
    );
}

/// Parse `major.minor` off `EnvInfo::php_version` — local, not `PINNED_PHP`.
fn php_minor(version: &str) -> (u16, u16) {
    let mut parts = version.split('.');
    let major = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let minor = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    (major, minor)
}

#[test]
fn reflect_return_type_is_none_for_a_class_like() {
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
    // A structured not-found (Some, exists()==false), never None (a failed query).
    let r = sc.reflect("steins_no_such_symbol_xyzzy").expect("reflection reply");
    assert!(!r.exists(), "nonsense name must not exist: {r:?}");
    assert!(!r.function_exists && !r.class_like_exists);
}

// `preg_compile` (issue #189 / ADR-0078) — the project's own PCRE answers.

/// PCRE refuses, and its OWN words come back with the probe's prefix stripped.
#[test]
fn preg_compile_reports_a_refusal_with_pcres_own_message() {
    let Some(mut sc) = spawn_or_skip("preg_compile_reports_a_refusal") else { return };
    let PregCompile::Refuses { message } =
        sc.preg_compile("/(unclosed/").expect("a preg_compile verdict")
    else {
        panic!("PCRE must refuse an unclosed group");
    };
    assert!(
        message.starts_with("Compilation failed:"),
        "PCRE's own words, unprefixed: {message}"
    );
    assert!(message.contains("missing closing parenthesis"), "{message}");
    assert!(!message.contains("preg_match()"), "the probe's prefix is stripped: {message}");
}

/// The three refusal families PHP words differently, all of them compile-time.
#[test]
fn preg_compile_reports_delimiter_and_modifier_refusals() {
    let Some(mut sc) = spawn_or_skip("preg_compile_reports_delimiter_and_modifier_refusals") else {
        return;
    };
    for (pattern, needle) in [
        ("nodelim", "Delimiter must not be alphanumeric"),
        ("/a/Z", "Unknown modifier"),
        ("/a", "No ending delimiter"),
        ("", "Empty regular expression"),
    ] {
        let PregCompile::Refuses { message } =
            sc.preg_compile(pattern).unwrap_or_else(|| panic!("verdict for {pattern:?}"))
        else {
            panic!("PCRE must refuse {pattern:?}");
        };
        assert!(message.contains(needle), "{pattern:?} → {message}");
    }
}

/// The silence half: everything the reader's delimiter/modifier handling admits compiles.
#[test]
fn preg_compile_accepts_the_patterns_the_reader_handles() {
    let Some(mut sc) = spawn_or_skip("preg_compile_accepts_valid_patterns") else { return };
    for pattern in ["/valid/", "~ok~iu", "#(a)(b)?#", "/\\d+/u", "//", "/(?|(a)|(b))(c)/"] {
        assert_eq!(
            sc.preg_compile(pattern),
            Some(PregCompile::Compiles),
            "{pattern:?} compiles on this PHP"
        );
    }
}

/// `/(?R)/` COMPILES then hits a runtime limit: widen, not a compile refusal.
#[test]
fn a_runtime_limit_is_not_a_compile_refusal() {
    let Some(mut sc) = spawn_or_skip("a_runtime_limit_is_not_a_compile_refusal") else { return };
    assert_eq!(
        sc.preg_compile("/(?R)/"),
        None,
        "a `false` from a runtime limit is unanswerable, never a refusal"
    );
}

/// Subject is `''` — the catastrophic pattern answers instantly, no backtracking.
#[test]
fn a_catastrophic_pattern_compiles_without_running_away() {
    let Some(mut sc) = spawn_or_skip("a_catastrophic_pattern_compiles_without_running_away") else {
        return;
    };
    let start = std::time::Instant::now();
    assert_eq!(sc.preg_compile("/(a+)+$/"), Some(PregCompile::Compiles));
    // Loose on purpose ("no runaway", not perf) — else the 2s transport timeout bounds it.
    assert!(start.elapsed() < Duration::from_secs(1), "the empty-subject probe returns at once");
}

/// `error_get_last` is cleared per request, so no stale diagnostic leaks.
#[test]
fn a_refusal_does_not_corrupt_the_stream_or_leak_into_the_next_query() {
    let Some(mut sc) = spawn_or_skip("a_refusal_does_not_corrupt_the_stream") else { return };
    assert!(matches!(sc.preg_compile("/(unclosed/"), Some(PregCompile::Refuses { .. })));
    assert_eq!(sc.preg_compile("/ok/"), Some(PregCompile::Compiles), "no stale diagnostic");
    assert_eq!(
        sc.fold("strtoupper", &[s("ab")], true),
        FoldResult::Value(FoldValue::Str("AB".to_owned())),
        "the NDJSON stream survives a provoked warning"
    );
}

// `defined` (issue #198 / ADR-0078) — the constant-existence oracle.

/// `PHP_EOL`/`JSON_THROW_ON_ERROR` pin core vs ext-json visibility (ADR-0049 §1).
#[test]
fn defined_answers_for_engine_and_extension_constants() {
    let Some(mut sc) = spawn_or_skip("defined_answers_for_engine_and_extension_constants") else {
        return;
    };
    assert_eq!(sc.constant_defined("PHP_EOL"), Some(ConstantDefined::Defined));
    assert_eq!(sc.constant_defined("JSON_THROW_ON_ERROR"), Some(ConstantDefined::Defined));
    assert_eq!(
        sc.constant_defined("STEINS_NO_SUCH_CONSTANT_XYZZY"),
        Some(ConstantDefined::NotDefined),
        "a nonsense name is a definitive not-defined, never an unanswerable"
    );
}

/// Constants are case-sensitive, and the wire must not launder that away: PHP's own
/// `define('Foo', 1); var_dump(defined('FOO'));` prints `bool(false)`.
#[test]
fn defined_is_case_sensitive() {
    let Some(mut sc) = spawn_or_skip("defined_is_case_sensitive") else { return };
    assert_eq!(sc.constant_defined("PHP_EOL"), Some(ConstantDefined::Defined));
    assert_eq!(sc.constant_defined("php_eol"), Some(ConstantDefined::NotDefined));
}

/// A leading `\` is a *spelling*, not the name — the runner trims it like `reflect`.
#[test]
fn defined_ignores_a_leading_backslash() {
    let Some(mut sc) = spawn_or_skip("defined_ignores_a_leading_backslash") else { return };
    assert_eq!(sc.constant_defined("\\PHP_EOL"), Some(ConstantDefined::Defined));
}

/// Refused, not asked: `defined('C::K')` would **autoload** `C`, running project code.
#[test]
fn defined_refuses_a_class_constant_name() {
    let Some(mut sc) = spawn_or_skip("defined_refuses_a_class_constant_name") else { return };
    assert_eq!(sc.constant_defined("DateTime::ATOM"), None, "a `::` name widens, never answers");
    assert_eq!(sc.constant_defined("PHP_EOL"), Some(ConstantDefined::Defined));
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
    let r = sc.fold("strtolower", &[FoldArg::Str("ABC".to_owned())], true);
    assert_eq!(r, FoldResult::Value(FoldValue::Str("abc".to_owned())));
}

#[test]
fn fold_preserves_float_and_int_types() {
    let Some(mut sc) = spawn_or_skip("fold_preserves_float_and_int_types") else { return };
    assert_eq!(
        sc.fold("strlen", &[FoldArg::Str("hello".to_owned())], true),
        FoldResult::Value(FoldValue::Int(5))
    );
    // abs(-3.5) → float 3.5 (stays a float, JSON_PRESERVE_ZERO_FRACTION path)
    assert_eq!(
        sc.fold("abs", &[FoldArg::Float(-3.5)], true),
        FoldResult::Value(FoldValue::Float(3.5))
    );
    assert_eq!(sc.fold("abs", &[FoldArg::Float(-2.0)], true), FoldResult::Value(FoldValue::Float(2.0)));
}

#[test]
fn fold_divide_by_zero_is_a_throw() {
    let Some(mut sc) = spawn_or_skip("fold_divide_by_zero_is_a_throw") else { return };
    let r = sc.fold("intdiv", &[FoldArg::Int(1), FoldArg::Int(0)], true);
    assert_eq!(r, FoldResult::Throw { class: "DivisionByZeroError".to_owned() });
}

/// `explode('', 'x')` is a `ValueError` at `PINNED_PHP` — a throw, not an
/// invented return: PHP 8.0 replaced the pre-8 `false` with this throw, so
/// `explode` sits in `WIDTH_UNVERIFIED` — asking the engine is cheaper (ADR-0004).
#[test]
fn fold_explode_with_an_empty_separator_is_a_throw() {
    let Some(mut sc) = spawn_or_skip("fold_explode_with_an_empty_separator_is_a_throw") else {
        return;
    };
    assert_eq!(
        sc.fold("explode", &[s(""), s("x")], true),
        FoldResult::Throw { class: "ValueError".to_owned() }
    );
    // The same process answers the non-empty call next (array result via the
    // 2026-08-14 amendment) — the throw is the argument's, not the name's.
    assert_eq!(
        sc.fold("explode", &[s(","), s("a,b")], true),
        FoldResult::Value(FoldValue::Array(vec![
            (FoldKey::Int(0), FoldValue::Str("a".to_owned())),
            (FoldKey::Int(1), FoldValue::Str("b".to_owned())),
        ]))
    );
    assert!(!sc.is_poisoned());
}

#[test]
fn fold_unknown_function_widens() {
    let Some(mut sc) = spawn_or_skip("fold_unknown_function_widens") else { return };
    let r = sc.fold("steins_no_such_function_xyz", &[], true);
    assert!(matches!(r, FoldResult::Widen { .. }), "unknown fn widens, got {r:?}");
}

#[test]
fn fold_wrong_arity_widens() {
    let Some(mut sc) = spawn_or_skip("fold_wrong_arity_widens") else { return };
    // strlen() with no args → ArgumentCountError → widen (structural misuse).
    let r = sc.fold("strlen", &[], true);
    assert!(matches!(r, FoldResult::Widen { .. }), "wrong arity widens, got {r:?}");
}

// -- array-literal fold arguments (issue #39): wire is entries; PHP's key rules apply. --

#[test]
fn fold_count_over_a_literal_array() {
    let Some(mut sc) = spawn_or_skip("fold_count_over_a_literal_array") else { return };
    assert_eq!(sc.fold("count", &[list(vec![int(1), int(2), int(3)])], true), FoldResult::Value(FoldValue::Int(3)));
    // The empty array is a value, and its count is 0 — not a widen.
    assert_eq!(sc.fold("count", &[list(vec![])], true), FoldResult::Value(FoldValue::Int(0)));
}

#[test]
fn fold_in_array_and_implode_over_literal_arrays() {
    let Some(mut sc) = spawn_or_skip("fold_in_array_and_implode") else { return };
    let haystack = list(vec![int(1), int(2), int(3)]);
    assert_eq!(sc.fold("in_array", &[int(2), haystack.clone()], true), FoldResult::Value(FoldValue::Bool(true)));
    assert_eq!(sc.fold("in_array", &[int(9), haystack], true), FoldResult::Value(FoldValue::Bool(false)));
    assert_eq!(
        sc.fold("implode", &[s(","), list(vec![s("a"), s("b")])], true),
        FoldResult::Value(FoldValue::Str("a,b".to_owned()))
    );
}

#[test]
fn fold_nested_array_arguments_round_trip() {
    let Some(mut sc) = spawn_or_skip("fold_nested_array_arguments_round_trip") else { return };
    // count() is shallow: [[1,2],[3]] has two entries.
    let nested = list(vec![list(vec![int(1), int(2)]), list(vec![int(3)])]);
    assert_eq!(sc.fold("count", std::slice::from_ref(&nested), true), FoldResult::Value(FoldValue::Int(2)));
    // in_array compares the inner array by value — proof nesting survived the wire intact.
    assert_eq!(
        sc.fold("in_array", &[list(vec![int(1), int(2)]), nested], true),
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
    assert_eq!(sc.fold("count", std::slice::from_ref(&dup), true), FoldResult::Value(FoldValue::Int(1)));
    assert_eq!(sc.fold("implode", &[s(""), dup], true), FoldResult::Value(FoldValue::Str("b".to_owned())));

    // Mixed explicit and absent keys: the runtime's next-int rule places 'c'.
    let mixed = FoldArg::Array(vec![
        (Some(FoldKey::Str("x".into())), s("a")),
        (Some(FoldKey::Int(5)), s("b")),
        (None, s("c")),
    ]);
    assert_eq!(sc.fold("count", std::slice::from_ref(&mixed), true), FoldResult::Value(FoldValue::Int(3)));
    assert_eq!(
        sc.fold("implode", &[s(","), mixed], true),
        FoldResult::Value(FoldValue::Str("a,b,c".to_owned()))
    );
}

/// Rebuilding an array literal can THROW PHP's own key-rule error:
/// `[PHP_INT_MAX => 'a', 'b']` → "Cannot add element...". Before issue #64 S1.5
/// this escaped as an uncaught FATAL, killing the runner mid-NDJSON — must widen.
/// Threshold: engine's `PHP_INT_MAX` (2147483647 on php-wasm's 32-bit build).
#[test]
fn an_overflowing_next_int_key_widens_and_leaves_the_runner_alive() {
    let Some(mut sc) = spawn_or_skip("an_overflowing_next_int_key_widens_and_leaves_the_runner_alive")
    else {
        return;
    };
    let overflowing =
        FoldArg::Array(vec![(Some(FoldKey::Int(i64::MAX)), s("a")), (None, s("b"))]);
    let r = sc.fold("count", std::slice::from_ref(&overflowing), true);
    assert!(matches!(r, FoldResult::Widen { .. }), "an unassignable next key widens, got {r:?}");
    assert!(!sc.is_poisoned(), "widening is not a protocol failure");
    // The same process answers the next question — the fatal is gone.
    assert_eq!(
        sc.fold("strtoupper", &[s("still alive")], true),
        FoldResult::Value(FoldValue::Str("STILL ALIVE".to_owned()))
    );
    // And the boundary below it is an ordinary, answerable array.
    let ok = FoldArg::Array(vec![(Some(FoldKey::Int(i64::MAX - 1)), s("a")), (None, s("b"))]);
    assert_eq!(sc.fold("count", &[ok], true), FoldResult::Value(FoldValue::Int(2)));
}

/// Array results cross the seam (ADR-0028, 2026-08-14, issue #330) — keys **materialized**.
#[test]
fn an_array_returning_fold_comes_back_in_the_envelope() {
    let Some(mut sc) = spawn_or_skip("an_array_returning_fold_comes_back_in_the_envelope") else {
        return;
    };
    let r = sc.fold("str_replace", &[s("a"), s("b"), list(vec![s("a"), s("aa")])], true);
    assert_eq!(
        r,
        FoldResult::Value(FoldValue::Array(vec![
            (FoldKey::Int(0), FoldValue::Str("b".to_owned())),
            (FoldKey::Int(1), FoldValue::Str("bb".to_owned())),
        ])),
        "the engine's array, with the keys it assigned"
    );
    assert!(!sc.is_poisoned());
}

/// Keys keep their own kinds, never flattened; `'5'`-vs-`5` is pinned at the decoder.
#[test]
fn an_array_result_keeps_its_key_kinds() {
    let Some(mut sc) = spawn_or_skip("an_array_result_keeps_its_key_kinds") else { return };
    let subject = FoldArg::Array(vec![
        (Some(FoldKey::Str("a".to_owned())), s("foo")),
        (Some(FoldKey::Int(5)), s("boo")),
    ]);
    assert_eq!(
        sc.fold("str_replace", &[s("o"), s("0"), subject], true),
        FoldResult::Value(FoldValue::Array(vec![
            (FoldKey::Str("a".to_owned()), FoldValue::Str("f00".to_owned())),
            (FoldKey::Int(5), FoldValue::Str("b00".to_owned())),
        ]))
    );
}

/// Budget (256 entries, 8 levels) charged before the envelope. The runner does
/// not consult the allowlist at all, so these probes are unaffected by issue
/// #354 putting `range` on it; the analyzer-level twin is
/// `an_over_budget_array_fill_widens_rather_than_truncating`.
#[test]
fn an_over_budget_array_result_widens_at_the_runner() {
    let Some(mut sc) = spawn_or_skip("an_over_budget_array_result_widens_at_the_runner") else {
        return;
    };
    // 256 entries admissible, 257 not.
    assert!(matches!(
        sc.fold("range", &[int(1), int(256)], true),
        FoldResult::Value(FoldValue::Array(_))
    ));
    assert_eq!(
        sc.fold("range", &[int(1), int(257)], true),
        FoldResult::widen("array result over entry budget")
    );
    // 8 levels admissible, 9 not.
    assert!(matches!(
        sc.fold("json_decode", &[s("[[[[[[[[\"x\"]]]]]]]]"), FoldArg::Bool(true)], true),
        FoldResult::Value(FoldValue::Array(_))
    ));
    assert_eq!(
        sc.fold("json_decode", &[s("[[[[[[[[[\"x\"]]]]]]]]]"), FoldArg::Bool(true)], true),
        FoldResult::widen("array result over depth budget")
    );
    assert!(!sc.is_poisoned(), "a widened result is not a protocol failure");
}

/// A non-UTF-8 string *anywhere* widens the **whole** result (ADR-0080 §2.6):
/// `str_split("À")` (`C3 80`) splits into two non-UTF-8 bytes; ADR-0080 §3.1 lifts this.
#[test]
fn a_binary_string_inside_an_array_result_widens_the_whole_result() {
    let Some(mut sc) = spawn_or_skip("a_binary_string_inside_an_array_result_widens_the_whole_result")
    else {
        return;
    };
    assert_eq!(sc.fold("str_split", &[s("À")], true), FoldResult::widen("non-utf8 string"));
    // The *scalar* form of the same refusal, which had a runner branch and no test.
    assert_eq!(sc.fold("base64_decode", &[s("wA==")], true), FoldResult::widen("non-utf8 string"));
    assert!(!sc.is_poisoned());
    // The same process answers an ordinary array next — the refusal is the value's.
    assert!(matches!(
        sc.fold("str_split", &[s("ab")], true),
        FoldResult::Value(FoldValue::Array(_))
    ));
}

#[test]
fn process_is_reused_across_many_folds() {
    let Some(mut sc) = spawn_or_skip("process_is_reused_across_many_folds") else { return };
    // Same resident process answers request after request (incremental ids).
    for i in 0..50 {
        let s = format!("VALUE{i}");
        let r = sc.fold("strtolower", &[FoldArg::Str(s.clone())], true);
        assert_eq!(r, FoldResult::Value(FoldValue::Str(s.to_lowercase())));
    }
    assert!(!sc.is_poisoned());
}

#[test]
fn timeout_poisons_and_the_lost_request_widens() {
    let Some(mut sc) = spawn_or_skip("timeout_poisons") else { return };
    // Tiny deadline against a slow call; `usleep` isn't on the fold allowlist, but
    // the runner doesn't gate (Rust does) — still exercises the protocol.
    sc.set_timeout(Duration::from_millis(20));
    let r = sc.fold("usleep", &[FoldArg::Int(1_000_000)], true); // 1s > 20ms
    assert!(matches!(r, FoldResult::Widen { .. }), "timeout widens, got {r:?}");
    assert!(sc.is_poisoned(), "timeout poisons the instance");
    // Lost for good (never re-sent — it misbehaved); the next request revives
    // the instance via respawn (full PHP startup, tens of ms, past the 20ms forced).
    sc.set_timeout(Duration::from_secs(2));
    assert_eq!(
        sc.fold("strtolower", &[FoldArg::Str("ABC".to_owned())], true),
        FoldResult::Value(FoldValue::Str("abc".to_owned())),
        "the next request respawns and answers"
    );
    assert!(!sc.is_poisoned(), "a revived instance is not poisoned");
}

/// A `str_repeat` past `memory_limit` dies as an *uncatchable* fatal (no
/// `Throwable`) — child stops mid-NDJSON; ordinary source, must widen, next answers.
#[test]
fn a_memory_exhausting_fold_widens_and_the_next_request_still_answers() {
    let Some(mut sc) = spawn_or_skip("a_memory_exhausting_fold_widens_and_the_next_request_answers")
    else {
        return;
    };
    let r = sc.fold("str_repeat", &[s("x"), int(2_000_000_000)], true);
    assert!(matches!(r, FoldResult::Widen { .. }), "a memory bomb widens, got {r:?}");
    assert!(sc.is_poisoned(), "the child died, so the transport is poisoned");
    // The lost answer costs one request, not the rest of the run.
    assert_eq!(
        sc.fold("strtoupper", &[s("still alive")], true),
        FoldResult::Value(FoldValue::Str("STILL ALIVE".to_owned()))
    );
    assert!(!sc.is_poisoned(), "the respawned child is healthy");
}

/// The storm brake: recovery bounded at three respawns per `Sidecar` — past
/// the cap, the instance is permanently poisoned, widening immediately.
#[test]
fn the_respawn_cap_bounds_recovery_and_then_poisons_permanently() {
    let Some(mut sc) = spawn_or_skip("the_respawn_cap_bounds_recovery") else { return };
    let bomb = [s("x"), int(2_000_000_000)];

    for i in 0..3 {
        assert!(matches!(sc.fold("str_repeat", &bomb, true), FoldResult::Widen { .. }), "bomb {i} widens");
        assert_eq!(
            sc.fold("strtoupper", &[s("alive")], true),
            FoldResult::Value(FoldValue::Str("ALIVE".to_owned())),
            "respawn {i} answered"
        );
    }

    // The fourth bomb kills the third replacement, and there is no fourth.
    assert!(matches!(sc.fold("str_repeat", &bomb, true), FoldResult::Widen { .. }), "the last bomb widens");
    assert!(sc.is_poisoned());
    let start = std::time::Instant::now();
    for _ in 0..5 {
        let r = sc.fold("strtoupper", &[s("alive")], true);
        assert!(matches!(r, FoldResult::Widen { .. }), "past the cap every fold widens, got {r:?}");
    }
    // Widening past the cap touches no process, so it can't cost a timeout;
    // loose on purpose — asserts "no hang", not a performance figure.
    assert!(start.elapsed() < Duration::from_secs(1), "a capped sidecar widens without waiting");
    assert!(sc.is_poisoned(), "the poison is permanent now");
}

/// The runner evaluates every fold in **strict mode**, because it declares
/// `strict_types=1` itself and `strict_types` binds to the file a call is
/// written in.
///
/// Without that declaration the seam lost the call site's own calling
/// convention: a call written inside `declare(strict_types=1)` was evaluated
/// weakly, so `substr("abcdef", "1")` folded to `'bcdef'` where the program it
/// came from throws. A folded value is `Verified`, the strongest stratum, so
/// the analysis carried a value the runtime cannot produce.
///
/// Strict is the sound direction whichever mode the call site is in: where the
/// argument types match the declaration both modes agree, and where they do not
/// strict throws — which this seam reports as `kind: throw`, the fold declines,
/// and the answer widens. The cost is precision in a *weak* file, never a wrong
/// value in a strict one, and recovering that precision means carrying the call
/// site's real strictness (issue #383).
#[test]
fn a_type_mismatched_argument_throws_rather_than_being_coerced() {
    let Some(mut sc) = spawn_or_skip("a_type_mismatched_argument_throws_rather_than_being_coerced")
    else {
        return;
    };
    // Each pair is the same call twice: the declared type, then a literal PHP's
    // weak mode would have coerced into it.
    for (name, ok, coercible) in [
        ("substr", vec![s("abcdef"), int(1)], vec![s("abcdef"), s("1")]),
        ("str_repeat", vec![s("ab"), int(2)], vec![s("ab"), s("2")]),
        ("intdiv", vec![int(6), int(2)], vec![s("6"), int(2)]),
        ("str_pad", vec![s("a"), int(3)], vec![s("a"), s("3")]),
    ] {
        assert!(
            matches!(sc.fold(name, &ok, true), FoldResult::Value(_)),
            "{name} still folds when the argument types match"
        );
        assert_eq!(
            sc.fold(name, &coercible, true),
            FoldResult::Throw { class: "TypeError".to_owned() },
            "{name} must not coerce: the call site may be strict, and this one answer serves both"
        );
    }
    assert!(!sc.is_poisoned(), "a TypeError is a result, not a protocol failure");
}
