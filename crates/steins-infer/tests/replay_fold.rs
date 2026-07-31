//! The replay transport (ADR-0066, issue #64): a [`TableFolder`] answers from a
//! supplied table and records what it could not answer, so the sidecar surface is
//! reachable where no process can be spawned.
//!
//! Three things are pinned here.
//!
//! 1. **Table semantics** — a hit parses through the shared wire parsers; a miss
//!    declines and lands on `pending`, deduped and in first-occurrence order. A
//!    miss never fabricates: an unanswered fold is exactly a dead sidecar's widen.
//! 2. **The gates are the SHARED gates.** `EngineFolder` holds the policy once,
//!    over any transport, so these tests drive the real ADR-0056 admission
//!    sequence through a fake engine — the version pin (§2) and the issue-#64
//!    integer-width pin — and get the same verdicts the native folder would give.
//! 3. **The differential oracle.** A replay run driven to its fixpoint by a REAL
//!    [`Sidecar`] answering each pending request verbatim produces the same
//!    findings as a direct `SidecarFolder` run. That is the acceptance property:
//!    fold answers come from one dispatch core, and the replay path is a transport
//!    change, not a semantics change.

use std::collections::HashMap;

use steins_domain::{Base, Fact};
use steins_infer::{
    DEBUG_TYPE_ID, Diagnostic, EngineFolder, FoldEngine, Folder, LineFact, SidecarFolder,
    TableFolder, annotate_project, check_with, request_key,
};
use steins_sidecar::{
    EnvInfo, FoldArg, FoldResult, Reflection, Sidecar, env_params, fold_params, reflect_params,
};
use steins_syntax::{ArgValue, SourceTree};

// ---------------------------------------------------------------------------
// Table builders
// ---------------------------------------------------------------------------

type Table = HashMap<String, serde_json::Value>;

/// An `env` answer: `version`, no extensions, and an integer width in bytes.
fn env_answer(version: &str, int_size: u32) -> serde_json::Value {
    serde_json::json!({
        "php_version": version,
        "extensions": ["Core", "standard"],
        "sapi": "cli",
        "int_size": int_size,
    })
}

/// A `reflection` answer for a resident function with a declared return type.
fn fn_reflection(target: &str, return_type: &str) -> serde_json::Value {
    serde_json::json!({
        "kind": "reflection",
        "target": target,
        "function": true,
        "class_like": false,
        "return_type": return_type,
    })
}

fn with_env(table: &mut Table, version: &str, int_size: u32) {
    table.insert(request_key("env", &env_params()), env_answer(version, int_size));
}

fn with_reflect(table: &mut Table, target: &str, answer: serde_json::Value) {
    table.insert(request_key("reflect", &reflect_params(target)), answer);
}

fn with_fold(table: &mut Table, name: &str, args: &[FoldArg], answer: serde_json::Value) {
    table.insert(request_key("fold", &fold_params(name, args)), answer);
}

/// A 64-bit engine at the pinned minor — the "everything admitted" baseline.
fn pinned_env_table() -> Table {
    let mut t = Table::new();
    with_env(&mut t, "8.5.8", 8);
    t
}

// ---------------------------------------------------------------------------
// (i) Table semantics
// ---------------------------------------------------------------------------

#[test]
fn a_table_hit_answers_the_fold() {
    let mut t = pinned_env_table();
    with_fold(
        &mut t,
        "strtoupper",
        &[FoldArg::Str("ab".to_owned())],
        serde_json::json!({ "kind": "value", "type": "string", "value": "AB" }),
    );
    let mut folder = TableFolder::with_table(t);
    assert_eq!(
        folder.fold("strtoupper", &[ArgValue::Str("ab".to_owned())]),
        Some(ArgValue::Str("AB".to_owned()))
    );
    assert!(folder.pending().is_empty(), "a fully-answered run has nothing pending");
}

#[test]
fn a_table_miss_declines_and_records_the_request() {
    let mut folder = TableFolder::with_table(pinned_env_table());
    assert_eq!(folder.fold("strtoupper", &[ArgValue::Str("ab".to_owned())]), None);
    let pending = folder.take_pending();
    assert_eq!(pending.len(), 1, "one miss: {pending:?}");
    // The pending key IS the request, minus its framing id — everything the
    // answering side needs, and byte-identical to what the table wants back.
    let req: serde_json::Value = serde_json::from_str(&pending[0]).expect("key parses as JSON");
    assert_eq!(req["method"], "fold");
    assert_eq!(req["params"]["function"], "strtoupper");
    assert_eq!(req["params"]["args"], serde_json::json!(["ab"]));
    assert_eq!(pending[0], request_key("fold", &fold_params("strtoupper", &[FoldArg::Str("ab".to_owned())])));
}

#[test]
fn an_empty_table_asks_about_the_engine_before_it_asks_for_a_fold() {
    // The integer-width gate (issue #64) is consulted first, so the very first
    // iteration of a replay loop reports the `env` question and nothing else.
    // This is not a wasted round trip: it is the gate refusing to dispatch a
    // value question to an engine whose arithmetic it has not established.
    let mut folder = TableFolder::with_table(Table::new());
    assert_eq!(folder.fold("strtoupper", &[ArgValue::Str("ab".to_owned())]), None);
    assert_eq!(folder.take_pending(), vec![request_key("env", &env_params())]);
}

#[test]
fn repeated_misses_are_deduped_in_first_occurrence_order() {
    let mut folder = TableFolder::with_table(pinned_env_table());
    // A memoized folder would hide a duplicate anyway, so ask three DISTINCT
    // questions and one of them twice through a distinct-args call.
    let _ = folder.fold("strtoupper", &[ArgValue::Str("ab".to_owned())]);
    let _ = folder.fold("strtolower", &[ArgValue::Str("AB".to_owned())]);
    let _ = folder.boot_surface_function("strlen");
    let _ = folder.boot_surface_class_like("strlen"); // same `reflect` request
    let pending = folder.take_pending();
    assert_eq!(pending.len(), 3, "deduped: {pending:?}");
    assert_eq!(pending[0], request_key("fold", &fold_params("strtoupper", &[FoldArg::Str("ab".to_owned())])));
    assert_eq!(pending[1], request_key("fold", &fold_params("strtolower", &[FoldArg::Str("AB".to_owned())])));
    assert_eq!(pending[2], request_key("reflect", &reflect_params("strlen")));
}

#[test]
fn a_widen_answer_is_an_answer_not_a_miss() {
    let mut t = pinned_env_table();
    with_fold(
        &mut t,
        "nope",
        &[],
        serde_json::json!({ "kind": "widen", "reason": "unknown function" }),
    );
    let mut folder = TableFolder::with_table(t);
    assert_eq!(folder.fold("nope", &[]), None);
    assert!(folder.pending().is_empty(), "answered-with-widen is settled, not pending");
}

#[test]
fn env_drives_the_php_minor_and_the_boot_surface_label() {
    let mut folder = TableFolder::with_table(pinned_env_table());
    assert_eq!(folder.php_minor(), Some((8, 5)));
    assert_eq!(folder.boot_surface_label().as_deref(), Some("PHP 8.5.8 (2 extensions)"));
    assert!(folder.absence_family_available(), "a clean env with no target admits the family");
    assert!(folder.pending().is_empty());
}

#[test]
fn reflect_drives_the_builtin_return_fact() {
    let mut t = pinned_env_table();
    with_reflect(&mut t, "strlen", fn_reflection("strlen", "int"));
    let mut folder = TableFolder::with_table(t);
    let fact = folder.builtin_return_fact("strlen").expect("a seeded fact");
    assert_eq!(fact_base_of(&fact), Some(Base::Int));
    // The curated row refines within the reflected envelope at the pin: `strlen`
    // is `int<0, max>`, strictly narrower than the declared `int`.
    assert!(matches!(fact, Fact::Refined { .. }), "curated refinement admitted at the pin: {fact:?}");
    assert_eq!(folder.builtin_return_type("strlen").as_deref(), Some("int"));
}

/// Issue #76: the arity second leg reaches the **replay** transport through the
/// same `reflect` table entry the declaration comes from — no new wire method, so
/// the browser lane needs nothing beyond the runner it already ships.
#[test]
fn reflect_drives_the_parameter_counts_over_the_replay_transport() {
    let mut t = pinned_env_table();
    with_reflect(
        &mut t,
        "substr",
        serde_json::json!({
            "kind": "reflection",
            "target": "substr",
            "function": true,
            "class_like": false,
            "return_type": "string",
            "params_total": 3,
            "params_required": 2,
        }),
    );
    // A row recorded BEFORE the arity surface existed: it still answers the
    // declaration, and reports no arity — which withholds an arity-pinned rule
    // instead of admitting it un-countersigned.
    with_reflect(&mut t, "strlen", fn_reflection("strlen", "int"));
    let mut folder = TableFolder::with_table(t);
    assert_eq!(folder.builtin_param_counts("substr"), Some((3, 2)));
    assert_eq!(folder.builtin_return_type("substr").as_deref(), Some("string"));
    assert_eq!(folder.builtin_param_counts("strlen"), None, "a pre-arity row answers no arity");
    assert_eq!(folder.builtin_return_type("strlen").as_deref(), Some("int"));
    // Case-insensitive, memoized per name, and no extra traffic for either.
    assert_eq!(folder.builtin_param_counts("SUBSTR"), Some((3, 2)));
    assert!(folder.pending().is_empty());
}

/// The gate the arity surface inherits: without a live engine (`env` unanswered)
/// nothing is reflected at all, so the counts are silent too.
#[test]
fn the_parameter_counts_are_withheld_without_a_live_engine() {
    let engine = FakeEngine::new("8.5.8", Some(8)).with_arity("substr", "string", 3, 2);
    let mut folder = EngineFolder::with_engine(engine);
    assert_eq!(folder.builtin_param_counts("substr"), Some((3, 2)));
    // A monkey-patch extension voids the whole surface (ADR-0049 A9) — a redefined
    // builtin disowns its signature exactly as it disowns its declared type.
    let mut engine = FakeEngine::new("8.5.8", Some(8)).with_arity("substr", "string", 3, 2);
    engine.env.as_mut().expect("env").extensions.push("uopz".to_owned());
    let mut folder = EngineFolder::with_engine(engine);
    assert_eq!(folder.builtin_param_counts("substr"), None);
    assert_eq!(folder.builtin_return_type("substr"), None);
}

// ---------------------------------------------------------------------------
// (ii) The shared gates, driven through a fake engine
// ---------------------------------------------------------------------------

/// A [`FoldEngine`] with canned answers — the transport-level Spy. It exists to
/// drive the SHARED policy in `EngineFolder`, which is the only copy of it.
struct FakeEngine {
    env: Option<EnvInfo>,
    reflections: HashMap<String, Reflection>,
    folds: HashMap<String, FoldResult>,
    /// Every fold the policy actually dispatched — the observable for the gate
    /// that must refuse *before* the engine is touched.
    dispatched: Vec<String>,
}

impl FakeEngine {
    fn new(version: &str, int_size: Option<u32>) -> Self {
        Self {
            env: Some(EnvInfo {
                php_version: version.to_owned(),
                extensions: vec!["Core".to_owned()],
                sapi: "cli".to_owned(),
                int_size,
            }),
            reflections: HashMap::new(),
            folds: HashMap::new(),
            dispatched: Vec::new(),
        }
    }

    fn with_function(mut self, name: &str, return_type: &str) -> Self {
        self.reflections.insert(
            name.to_owned(),
            Reflection {
                target: name.to_owned(),
                function_exists: true,
                class_like_exists: false,
                return_type: Some(return_type.to_owned()),
                return_type_tentative: false,
                // No arity: an engine that answers a declaration but no parameter
                // counts is exactly the old-runner case, and the mixed-pinned rules
                // must withhold on it.
                params_total: None,
                params_required: None,
            },
        );
        self
    }

    /// [`Self::with_function`] plus the reflected arity — ADR-0064's second leg.
    fn with_arity(mut self, name: &str, return_type: &str, total: u32, required: u32) -> Self {
        self = self.with_function(name, return_type);
        let r = self.reflections.get_mut(name).expect("just inserted");
        r.params_total = Some(total);
        r.params_required = Some(required);
        self
    }

    fn with_fold(mut self, name: &str, result: FoldResult) -> Self {
        self.folds.insert(name.to_owned(), result);
        self
    }
}

impl FoldEngine for FakeEngine {
    fn env(&mut self) -> Option<EnvInfo> {
        self.env.clone()
    }
    fn reflect(&mut self, target: &str) -> Option<Reflection> {
        self.reflections.get(target).cloned()
    }
    fn fold(&mut self, name: &str, _args: &[FoldArg]) -> FoldResult {
        self.dispatched.push(name.to_owned());
        self.folds.get(name).cloned().unwrap_or_else(|| FoldResult::widen("unknown"))
    }
}

fn fact_base_of(f: &Fact) -> Option<Base> {
    match f {
        Fact::General { base, .. } | Fact::Refined { base, .. } => Some(*base),
        _ => None,
    }
}

/// ADR-0056 Gate 2, version leg: a non-pinned minor declines the CURATED row and
/// still seeds the reflected envelope. The engine's own declaration is
/// version-correct by construction; only the curated refinement is pinned.
#[test]
fn a_non_pinned_minor_declines_the_curated_row_and_still_seeds_the_envelope() {
    let engine = FakeEngine::new("8.3.7", Some(8)).with_function("strlen", "int");
    let mut folder = EngineFolder::with_engine(engine);
    let fact = folder.builtin_return_fact("strlen").expect("the envelope still seeds");
    assert_eq!(fact, Fact::General { base: Base::Int, nullable: false }, "envelope alone: {fact:?}");
    // And the pin admits at the pinned minor, so this is a real gate, not a
    // fact that never refines.
    let engine = FakeEngine::new("8.5.2", Some(8)).with_function("strlen", "int");
    let mut folder = EngineFolder::with_engine(engine);
    let pinned = folder.builtin_return_fact("strlen").expect("a seeded fact");
    assert!(matches!(pinned, Fact::Refined { .. }), "refined at the pin: {pinned:?}");
}

/// ADR-0056 Gate 2, machine leg (issue #64): php-wasm 0.1.0 is PHP **8.5.2** —
/// the pinned minor, which the version leg above admits — built **32-bit**. The
/// curated rows are verified against the 64-bit engine at that minor, and a
/// narrow one can violate them (`hexdec` promoting to float where the row says
/// int), so the width declines the refinement. The envelope still seeds: a
/// declared return type is a platform-independent claim.
///
/// S1.5 deliberately does NOT relax this leg. The fold lane gained a width-safe
/// subset because a fold is a claim about one argument tuple, which a range guard
/// can bound; a curated row is a claim about a builtin's whole return domain, and
/// there is no per-call tuple to bound it with. `strlen` is on the width-safe fold
/// subset and its curated row still declines here — that contrast IS the pin.
#[test]
fn a_32_bit_engine_at_the_pinned_minor_declines_the_curated_row_and_still_seeds() {
    let engine = FakeEngine::new("8.5.2", Some(4)).with_function("strlen", "int");
    let mut folder = EngineFolder::with_engine(engine);
    let fact = folder.builtin_return_fact("strlen").expect("the envelope still seeds");
    assert_eq!(fact, Fact::General { base: Base::Int, nullable: false }, "envelope alone: {fact:?}");
    // The raw reflected declaration is unaffected by the width, too.
    assert_eq!(folder.builtin_return_type("strlen").as_deref(), Some("int"));
    // …and `strlen` IS width-safe for the fold lane, on the same folder, at the
    // same width. The two gates are genuinely independent.
    assert!(steins_catalog::width_safe("strlen"));
}

/// The width-safe subset over the REPLAY transport, end to end: a 32-bit table
/// answers a `strtoupper` fold, and the browser's engine is the one that answered
/// it. This is what S2 will see in the playground.
#[test]
fn the_replay_transport_folds_the_width_safe_subset_on_a_32_bit_table() {
    let mut t = Table::new();
    with_env(&mut t, "8.5.2", 4); // php-wasm 0.1.0
    with_fold(
        &mut t,
        "strtoupper",
        &[FoldArg::Str("ab".to_owned())],
        serde_json::json!({ "kind": "value", "type": "string", "value": "AB" }),
    );
    with_fold(
        &mut t,
        "intval",
        &[FoldArg::Str("3000000000".to_owned())],
        serde_json::json!({ "kind": "value", "type": "int", "value": 2_147_483_647_i64 }),
    );
    let mut folder = TableFolder::with_table(t);
    assert_eq!(
        folder.fold("strtoupper", &[ArgValue::Str("ab".to_owned())]),
        Some(ArgValue::Str("AB".to_owned()))
    );
    // The refused name declines even though the table HAS the (wrong) answer —
    // the gate is upstream of the table, so a saturated 32-bit literal cannot
    // reach a proof by being pre-answered.
    assert_eq!(folder.fold("intval", &[ArgValue::Str("3000000000".to_owned())]), None);
    assert!(folder.pending().is_empty(), "a refused fold asks nothing: {:?}", folder.pending());
}

/// The fold lane's width gate, refused leg (issue #64 S1.5): a builtin the
/// catalog does NOT certify width-safe folds nothing on a 32-bit engine, and is
/// not even dispatched to — not even with innocent arguments. `intval` is the
/// refusal's own worst case: `intval("3000000000")` is `3000000000` on a 64-bit
/// engine and the saturated `2147483647` on a 32-bit one, silently.
#[test]
fn a_width_refused_name_folds_nothing_on_a_32_bit_engine() {
    let engine = FakeEngine::new("8.5.2", Some(4))
        .with_fold("intval", FoldResult::Value(steins_sidecar::FoldValue::Int(7)));
    let mut folder = EngineFolder::with_engine(engine);
    // Innocent arguments — every integer in range, nothing suspicious about the
    // call. The refusal is per-NAME and the arguments cannot buy it back.
    assert_eq!(folder.fold("intval", &[ArgValue::Str("7".to_owned())]), None);
    assert!(
        folder.engine_mut().dispatched.is_empty(),
        "the gate refuses before the engine is touched: {:?}",
        folder.engine_mut().dispatched
    );
    // The absence family is NOT width-gated: existence is not arithmetic.
    assert!(folder.absence_family_available(), "a 32-bit engine still witnesses absence");
}

/// The fold lane's width gate, ADMITTED leg (issue #64 S1.5): a verified
/// width-safe builtin over in-range arguments folds on a 32-bit engine and IS
/// dispatched to. Without this the width tests would all be satisfied by a lane
/// that folds nothing at all.
#[test]
fn a_32_bit_engine_folds_the_verified_width_safe_subset() {
    let engine = FakeEngine::new("8.5.2", Some(4))
        .with_fold("strtoupper", FoldResult::Value(steins_sidecar::FoldValue::Str("AB".to_owned())));
    let mut folder = EngineFolder::with_engine(engine);
    assert_eq!(
        folder.fold("strtoupper", &[ArgValue::Str("ab".to_owned())]),
        Some(ArgValue::Str("AB".to_owned()))
    );
    assert_eq!(folder.engine_mut().dispatched, vec!["strtoupper".to_owned()]);
}

/// The range guard reaches array KEYS, recursively. `count` is width-safe, every
/// *value* here is in range, and the one out-of-range integer is a key two levels
/// down — which is exactly where it matters, because the key is what PHP's
/// next-int rule reads to decide whether the sibling is a new element.
#[test]
fn an_out_of_range_key_buried_in_a_nested_array_declines_the_fold() {
    use steins_syntax::ArrayKey;
    let nested = ArgValue::Array(vec![
        (ArrayKey::Auto, ArgValue::Int(1)),
        // 2^31 — one past the guard, and the engine's own PHP_INT_MAX at width 4.
        (ArrayKey::Int(2_147_483_648), ArgValue::Str("a".to_owned())),
    ]);
    let outer = ArgValue::Array(vec![(ArrayKey::Auto, nested)]);
    let engine = FakeEngine::new("8.5.2", Some(4))
        .with_fold("count", FoldResult::Value(steins_sidecar::FoldValue::Int(1)));
    let mut folder = EngineFolder::with_engine(engine);
    assert_eq!(folder.fold("count", std::slice::from_ref(&outer)), None);
    assert!(
        folder.engine_mut().dispatched.is_empty(),
        "an out-of-range key is refused before dispatch: {:?}",
        folder.engine_mut().dispatched
    );
    // The SAME shape one below the boundary folds, so the guard is a boundary and
    // not a blanket refusal of nested arrays.
    let ok_nested = ArgValue::Array(vec![
        (ArrayKey::Auto, ArgValue::Int(1)),
        (ArrayKey::Int(2_147_483_647), ArgValue::Str("a".to_owned())),
    ]);
    let engine = FakeEngine::new("8.5.2", Some(4))
        .with_fold("count", FoldResult::Value(steins_sidecar::FoldValue::Int(1)));
    let mut folder = EngineFolder::with_engine(engine);
    assert_eq!(
        folder.fold("count", &[ArgValue::Array(vec![(ArrayKey::Auto, ok_nested)])]),
        Some(ArgValue::Int(1))
    );
    assert_eq!(folder.engine_mut().dispatched, vec!["count".to_owned()]);
}

/// `-2^31` is excluded even though it is a legal 32-bit integer: it is the one
/// value whose magnitude the machine cannot represent, and excluding it is what
/// makes the `abs`-shaped boundary flip unreachable rather than merely unobserved.
#[test]
fn the_range_guard_excludes_php_int_min_on_a_32_bit_machine() {
    for (arg, expected) in [(-2_147_483_648_i64, None), (-2_147_483_647_i64, Some(0_usize))] {
        let engine = FakeEngine::new("8.5.2", Some(4))
            .with_fold("strval", FoldResult::Value(steins_sidecar::FoldValue::Str("x".to_owned())));
        let mut folder = EngineFolder::with_engine(engine);
        let folded = folder.fold("strval", &[ArgValue::Int(arg)]);
        assert_eq!(folded.is_some(), expected.is_some(), "arg {arg}");
        assert_eq!(folder.engine_mut().dispatched.is_empty(), expected.is_none(), "arg {arg}");
    }
}

/// An engine that does not report its width is not provably 64-bit, so the fold
/// lane declines — INCLUDING the width-safe subset. Default-deny: an old or
/// foreign runner is unknown, not assumed, and the subset is verified against a
/// specific machine rather than against "not 64-bit".
#[test]
fn an_unreported_width_declines_even_the_width_safe_subset() {
    let engine = FakeEngine::new("8.5.2", None)
        .with_fold("strtoupper", FoldResult::Value(steins_sidecar::FoldValue::Str("AB".to_owned())));
    let mut folder = EngineFolder::with_engine(engine);
    assert_eq!(folder.fold("strtoupper", &[ArgValue::Str("ab".to_owned())]), None);
    assert!(folder.engine_mut().dispatched.is_empty());
}

/// The 64-bit control for the tests above: same fake engine, width 8, and
/// everything flows. Without this the width tests could be passing for the wrong
/// reason.
#[test]
fn a_64_bit_engine_folds_normally() {
    let engine = FakeEngine::new("8.5.2", Some(8))
        .with_fold("strtoupper", FoldResult::Value(steins_sidecar::FoldValue::Str("AB".to_owned())));
    let mut folder = EngineFolder::with_engine(engine);
    assert_eq!(
        folder.fold("strtoupper", &[ArgValue::Str("ab".to_owned())]),
        Some(ArgValue::Str("AB".to_owned()))
    );
    assert_eq!(folder.engine_mut().dispatched, vec!["strtoupper".to_owned()]);
}

/// The 64-bit control for S1.5 specifically: at width 8 NEITHER new leg applies —
/// a width-refused name folds, and so does an argument far outside the 32-bit
/// range. The subset is a relaxation of the 32-bit decline, not a new restriction
/// on the machine every native run and every corpus gate uses.
#[test]
fn a_64_bit_engine_is_untouched_by_the_width_safe_subset() {
    let engine = FakeEngine::new("8.5.2", Some(8))
        .with_fold("intval", FoldResult::Value(steins_sidecar::FoldValue::Int(3_000_000_000)))
        .with_fold("strval", FoldResult::Value(steins_sidecar::FoldValue::Str("9e18".to_owned())));
    let mut folder = EngineFolder::with_engine(engine);
    // A width-REFUSED name, with an argument the 32-bit range guard would reject.
    assert_eq!(
        folder.fold("intval", &[ArgValue::Str("3000000000".to_owned())]),
        Some(ArgValue::Int(3_000_000_000))
    );
    // A width-SAFE name, with an argument beyond 2^31.
    assert_eq!(
        folder.fold("strval", &[ArgValue::Int(9_000_000_000_000_000_000)]),
        Some(ArgValue::Str("9e18".to_owned()))
    );
    assert_eq!(folder.engine_mut().dispatched, vec!["intval".to_owned(), "strval".to_owned()]);
}

// ---------------------------------------------------------------------------
// (iii) The differential fixpoint oracle
// ---------------------------------------------------------------------------

/// The flagship of issue #60/#59: a project call in argument position, whose body
/// concatenates and then folds through the engine.
const FLAGSHIP: &str = "<?php\n\
    function greet(int $times, string $name): string {\n\
        return str_repeat(\"Hello, \" . $name . \"! \", $times);\n\
    }\n\
    \\PHPStan\\dumpType(greet(2, \"World\"));\n";

/// The smallest thing that reaches the fold seam at all.
const STRTOUPPER: &str = "<?php\n$x = strtoupper(\"ab\");\n\\PHPStan\\dumpType($x);\n";

/// The replay loop, in test form — the native twin of the JS loop S2 will write.
///
/// Each iteration builds a FRESH folder over the accumulated table, runs the
/// analysis, and answers every pending request by handing it VERBATIM to a real
/// sidecar. The answered set strictly grows, so this terminates; the cap is
/// defensive and its exhaustion is a failure, not a degradation.
fn replay<T>(
    sidecar: &mut Sidecar,
    mut run: impl FnMut(&mut TableFolder) -> T,
) -> (T, HashMap<String, serde_json::Value>) {
    let mut table = Table::new();
    for _ in 0..32 {
        let mut folder = TableFolder::with_table(table.clone());
        let out = run(&mut folder);
        let pending = folder.take_pending();
        if pending.is_empty() {
            return (out, table);
        }
        for key in pending {
            let req: serde_json::Value = serde_json::from_str(&key).expect("a pending key is JSON");
            let method = req["method"].as_str().expect("method").to_owned();
            let result = sidecar
                .call_raw(&method, req["params"].clone())
                .expect("the sidecar answered a replayed request");
            table.insert(key, result);
        }
    }
    panic!("the replay loop did not reach a fixpoint within 32 iterations");
}

fn spawn_or_skip(test: &str) -> Option<Sidecar> {
    match Sidecar::spawn() {
        Ok(s) => Some(s),
        Err(e) => {
            eprintln!("SKIP {test}: could not spawn php sidecar ({e}) — is `php` on PATH?");
            None
        }
    }
}

fn findings_with(src: &str, folder: &mut dyn Folder) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check_with(&tree, &functions, "test.php", folder)
}

/// The acceptance pin: a replay run driven to its fixpoint by a real sidecar
/// answering each pending request verbatim produces EXACTLY the findings a direct
/// `SidecarFolder` run produces. Fold answers come from the same `steins_handle`
/// dispatch — the replay path is a transport, not a second semantics.
#[test]
fn replay_and_direct_runs_agree_on_the_findings() {
    let Some(mut sidecar) = spawn_or_skip("replay_and_direct_runs_agree_on_the_findings") else {
        return;
    };
    for src in [STRTOUPPER, FLAGSHIP] {
        let (replayed, table) = replay(&mut sidecar, |f| findings_with(src, f));
        let mut direct_folder = SidecarFolder::enabled();
        let direct = findings_with(src, &mut direct_folder);
        assert_eq!(replayed, direct, "replay vs direct disagreed on:\n{src}");
        assert!(!table.is_empty(), "the loop actually asked the engine something");
    }
}

/// The flagship VALUE, not merely agreement: `dumpType(greet(2, "World"))`
/// inlines to `'Hello, World! Hello, World! '` through the replay transport.
/// (Agreement alone would also be satisfied by both paths folding nothing.)
#[test]
fn the_flagship_inlines_through_the_replay_transport() {
    let Some(mut sidecar) = spawn_or_skip("the_flagship_inlines_through_the_replay_transport") else {
        return;
    };
    let (findings, _) = replay(&mut sidecar, |f| findings_with(FLAGSHIP, f));
    let dumps: Vec<String> = findings
        .iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message.replace("dumped type: ", ""))
        .collect();
    assert_eq!(dumps, vec!["'Hello, World! Hello, World! '".to_owned()]);
}

/// Annotate rides the same loop: the margin facts a replay run produces equal the
/// ones a direct sidecar run produces.
#[test]
fn replay_and_direct_runs_agree_on_the_annotations() {
    let Some(mut sidecar) = spawn_or_skip("replay_and_direct_runs_agree_on_the_annotations") else {
        return;
    };
    let annotate = |folder: &mut dyn Folder| -> Vec<(u32, String)> {
        let db = steins_db::SteinsDatabase::default();
        let file = steins_db::SourceFile::new(&db, "test.php".to_owned(), FLAGSHIP.to_owned());
        let project = steins_db::Project::new(
            &db,
            vec![file],
            steins_db::ProjectLayout::fallback(),
            steins_db::PluginFacts::none(),
        );
        annotate_project(&db, project, file, folder)
            .iter()
            .map(|f: &LineFact| (f.line, f.body()))
            .collect()
    };
    let (replayed, _) = replay(&mut sidecar, |f| annotate(f));
    let mut direct_folder = SidecarFolder::enabled();
    let direct = annotate(&mut direct_folder);
    assert_eq!(replayed, direct);
}

/// The native runner reports a 64-bit engine, so the width gate is a no-op on the
/// path every existing test and gate runs — the addition cannot have quietly
/// silenced the native fold lane.
#[test]
fn the_native_engine_reports_a_64_bit_machine() {
    let Some(mut sidecar) = spawn_or_skip("the_native_engine_reports_a_64_bit_machine") else {
        return;
    };
    assert_eq!(sidecar.env().expect("env").int_size, Some(8));
    // And a direct fold through the shared policy still folds.
    let mut folder = SidecarFolder::enabled();
    assert_eq!(
        folder.fold("strtoupper", &[ArgValue::Str("ab".to_owned())]),
        Some(ArgValue::Str("AB".to_owned()))
    );
}
