//! A fold that would kill the PHP child never reaches it.
//!
//! `str_repeat("x", 2000000000)` is an ordinary literal call on the folding
//! allowlist, and its result does not fit the runner's `memory_limit`. Memory
//! exhaustion is a PHP *fatal*, not a `Throwable`, so no `catch` in the runner
//! could turn it into a widen: the child died mid-NDJSON, the transport replaced
//! it, and the run carried a degradation notice it had not earned. phpstan-src
//! ships `str_repeat('abcdefghij', 1000000000)` as its own regression fixture,
//! so that was reachable by ordinary analysed code rather than by an attacker.
//!
//! The seam refuses it before dispatch now (`fold_within_allocation_budget`):
//! the size-shaped parameters are read from the mined `param_facts`, the budget
//! is charged on the PRODUCT (a 256-byte literal repeated 2^20 times is 256 MB
//! with an innocent-looking count), and the answer widens exactly as a decline
//! always has. This file pins that — the same snippet, and the engine intact
//! afterwards.
//!
//! **The transport's recovery discipline is still tested, in
//! `steins-sidecar/tests/protocol.rs`** (`timeout_poisons_and_the_lost_request_widens`,
//! `the_respawn_cap_bounds_recovery_and_then_poisons_permanently`), where it
//! belongs: it is a property of the transport, and it should not depend on
//! analysed source being able to kill a process. What this file keeps is the
//! layer above — that a refused fold widens to the floor and the next call in
//! the same run still folds.
//!
//! Requires `php` on `PATH`; without it the test skips with an explicit marker.

use steins_infer::{DEBUG_TYPE_ID, Folder, SidecarFolder, check_with};
use steins_syntax::{ArgValue, SourceTree};

/// A snippet that folds a memory bomb and then an ordinary call, dumping both.
const BOMB_THEN_FOLDABLE: &str = "<?php\n\
     $bomb = str_repeat(\"x\", 2000000000);\n\
     \\PHPStan\\dumpType($bomb);\n\
     $ok = strtoupper(\"ab\");\n\
     \\PHPStan\\dumpType($ok);\n";

/// The `dumpType` outputs for `src`, in source order.
fn dumps(src: &str, folder: &mut dyn Folder) -> Vec<String> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check_with(&tree, &functions, "test.php", folder)
        .iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message.replace("dumped type: ", ""))
        .collect()
}

/// The whole point in one run: the bomb widens to the declared-return floor, and
/// the very next call in the SAME analysis still folds to its value.
///
/// Unchanged in what it asserts, and changed in why it passes — the widen used
/// to be the wreckage of a dead child and is now a decline before dispatch.
#[test]
fn a_bomb_fold_does_not_disable_the_folder_for_the_rest_of_the_run() {
    let mut folder = SidecarFolder::enabled();
    // Probe with an unused argument first and skip loudly if `php` is unreachable
    // — `EngineFolder` memoizes, so probing `strtoupper("ab")` would answer the
    // snippet's second fold from cache and hide the death this test is about.
    if folder.fold("strtoupper", &[ArgValue::Str("probe".into())], true).is_none() {
        eprintln!(
            "SKIP a_bomb_fold_does_not_disable_the_folder_for_the_rest_of_the_run: \
             no folding engine — is `php` on PATH?"
        );
        return;
    }
    // The bomb's dump is the rung BELOW the fold (issue #77's string-predicate
    // transfer, casing spelled since #240), not the two-billion-character
    // literal — proof the fold did NOT happen and the analysis carried on.
    assert_eq!(dumps(BOMB_THEN_FOLDABLE, &mut folder), vec!["non-falsy-lowercase-string", "'AB'"]);
}

/// The same run, read as a coverage posture (issue #245).
///
/// It used to assert that a recovered death stays SAYABLE: one loss, one
/// restart, and a posture no longer comparable with a run that lost nothing.
/// The seam refuses the bomb before dispatch now, so there is no death to say
/// anything about — and the posture claim inverts. That is the stronger
/// property and the honest one to pin: a run whose folds were all answered or
/// all declined *is* comparable, and saying otherwise would be reporting damage
/// that did not happen.
///
/// The recovery machinery this used to exercise is covered in
/// `steins-sidecar/tests/protocol.rs`, at the transport layer where a death can
/// be induced without asking analysed source to do it.
#[test]
fn a_refused_bomb_leaves_the_run_posture_intact() {
    let mut folder = SidecarFolder::enabled();
    if folder.fold("strtoupper", &[ArgValue::Str("probe".into())], true).is_none() {
        eprintln!(
            "SKIP a_recovered_death_still_shows_in_the_run_posture: \
             no folding engine — is `php` on PATH?"
        );
        return;
    }
    // A live engine that has lost nothing yet is the comparable posture.
    let before = folder.posture();
    assert!(before.engaged, "the probe fold above proves an engine was reached");
    assert!(
        before.sidecar_backed_throughout(),
        "nothing has died yet, got {before:?}"
    );

    let _ = dumps(BOMB_THEN_FOLDABLE, &mut folder);

    let after = folder.posture();
    assert_eq!(after.losses, 0, "the bomb was refused, not survived, got {after:?}");
    assert_eq!(after.restarts, 0, "so no child was replaced, got {after:?}");
    assert!(!after.abandoned, "and nothing was abandoned, got {after:?}");
    assert!(
        after.sidecar_backed_throughout(),
        "every fold in this run was answered or declined by a live engine, got {after:?}"
    );
}

/// The shapes issue #783 measured killing the child, none of which carries its
/// size in a parameter named for it, then the controls. The file is strict on
/// purpose: `range('a', 100000000)` still reaches the allocation there, since
/// `range` declares `string|int|float` and PHP 8.3 coerces a non-numeric string
/// that faces a number to `0` with only a warning.
const PRICED_BOMBS_THEN_FOLDABLE: &str = "<?php\n\
     declare(strict_types=1);\n\
     \\PHPStan\\dumpType(range(0, 2000000000));\n\
     \\PHPStan\\dumpType(range(0, 100000000));\n\
     \\PHPStan\\dumpType(range(0, -100000000));\n\
     \\PHPStan\\dumpType(range(0, 2000000000, 3));\n\
     \\PHPStan\\dumpType(range(0, 100000000, 0.5));\n\
     \\PHPStan\\dumpType(range('0', '100000000'));\n\
     \\PHPStan\\dumpType(range('a', 100000000));\n\
     \\PHPStan\\dumpType(sprintf('%2000000000d', 1));\n\
     \\PHPStan\\dumpType(sprintf('%100000000d', 1));\n\
     \\PHPStan\\dumpType(range(1, 10));\n\
     \\PHPStan\\dumpType(range('a', 'e'));\n\
     \\PHPStan\\dumpType(sprintf('%05d', 42));\n\
     \\PHPStan\\dumpType(strtoupper('ab'));\n";

/// **A `range` or a format that would spend the engine's memory is priced and
/// refused before dispatch** (issue #783).
///
/// Four `range()` literals in one phpstan-src fixture each killed the child,
/// which spent the run's whole respawn budget inside one file and left every
/// later file on the sound subset. The named-parameter budget could not see
/// them: `range`'s size is arithmetic over three arguments, and `sprintf`'s is
/// inside its format string. As with the `str_repeat` bomb above, the claim is
/// about the RUN as much as the dumps: nothing lost, nothing restarted, and the
/// controls after the bombs still fold.
#[test]
fn a_priced_range_or_format_never_reaches_the_runner() {
    let mut folder = SidecarFolder::enabled();
    if folder.fold("strtoupper", &[ArgValue::Str("probe".into())], true).is_none() {
        eprintln!(
            "SKIP a_priced_range_or_format_never_reaches_the_runner: \
             no folding engine — is `php` on PATH?"
        );
        return;
    }
    let d = dumps(PRICED_BOMBS_THEN_FOLDABLE, &mut folder);
    assert_eq!(d.len(), 13, "one dump per line, got {d:?}");
    for (i, got) in d.iter().take(9).enumerate() {
        assert!(
            !got.starts_with('\'') && !got.starts_with("array{") && !got.starts_with("list{"),
            "bomb {i} folded to a value: {got}"
        );
    }
    // The controls still fold, which is what makes the price a budget and not a
    // ban on the two names.
    assert!(d[9].starts_with("list{1, 2, 3"), "range(1, 10) did not fold: {}", d[9]);
    assert_eq!(d[10], "list{'a', 'b', 'c', 'd', 'e'}");
    assert_eq!(d[11], "'00042'");
    assert_eq!(d[12], "'AB'");

    let after = folder.posture();
    assert_eq!(after.losses, 0, "a reply was lost, so a bomb was dispatched: {after:?}");
    assert_eq!(after.restarts, 0, "the child was replaced: {after:?}");
    assert!(after.sidecar_backed_throughout(), "got {after:?}");
}

/// A snippet whose every fold names a SECOND callee, and then asks an ordinary
/// question.
///
/// `var_dump` is the one that desyncs: it writes to stdout, which is the NDJSON
/// stream's own channel, so its output lands ahead of the JSON-RPC reply and
/// every subsequent read is off by one frame. `getenv` is the quieter half of
/// the same hazard — no output, and the analysis carrying the environment of
/// the machine it runs on into the value domain.
const CARRIERS_THEN_FOLDABLE: &str = "<?php\n\
     $a = array_filter([\"a\", \"b\"], \"var_dump\");\n\
     \\PHPStan\\dumpType($a);\n\
     $b = array_filter([\"PATH\"], \"getenv\");\n\
     \\PHPStan\\dumpType($b);\n\
     $ok = strtoupper(\"ab\");\n\
     \\PHPStan\\dumpType($ok);\n";

/// **The stream survives a would-be callback fold** (issue #382).
///
/// The incident this pins is not a wrong type; it is a dead transport. On a
/// branch that admitted `array_filter` with no shape gate,
/// `array_filter(["a", "b"], "var_dump")` folded — which is to say the sidecar
/// ran `var_dump`, whose stdout went into the NDJSON stream ahead of the reply.
/// The frame after that was unreadable, the child was replaced, and the whole
/// run carried the degradation notice: every later fold in every later file
/// answered from the sound subset instead of from the engine.
///
/// So the assertion is about the RUN and not about the two dumps. A test that
/// only checked the types would pass on the branch that desynced — the widened
/// answer is what a poisoned transport gives you too. What separates "refused
/// before dispatch" from "dispatched and survived" is that nothing was lost,
/// nothing was restarted, and the fold *after* them still answers from the
/// engine.
#[test]
fn a_callback_carrier_never_reaches_the_runner() {
    let mut folder = SidecarFolder::enabled();
    if folder.fold("strtoupper", &[ArgValue::Str("probe".into())], true).is_none() {
        eprintln!(
            "SKIP a_callback_carrier_never_reaches_the_runner: \
             no folding engine — is `php` on PATH?"
        );
        return;
    }
    let before = folder.posture();
    assert!(before.engaged && before.sidecar_backed_throughout(), "got {before:?}");

    let d = dumps(CARRIERS_THEN_FOLDABLE, &mut folder);
    // Neither call folded: each refused one answers exactly its declared floor.
    // The floor is asserted by equality on purpose — a fold of `["PATH"]` renders
    // `list{'PATH'}`, which a `starts_with("array{")` test would wave through.
    assert_eq!(d[0], "array (asserted)", "a `var_dump` argument reached the runner");
    assert_eq!(d[1], "array (asserted)", "`getenv` ran inside the analysis");
    // …and the engine is intact, which is the claim the dumps alone cannot make.
    assert_eq!(d[2], "'AB'", "the next fold still answers from the engine");

    let after = folder.posture();
    assert_eq!(after.losses, 0, "a reply was lost, so something was dispatched: {after:?}");
    assert_eq!(after.restarts, 0, "the child was replaced: {after:?}");
    assert!(!after.abandoned, "the transport gave up: {after:?}");
    assert!(
        after.sidecar_backed_throughout(),
        "the run degraded to the sound subset — the desync this gate exists for, got {after:?}"
    );
}

/// **A death costs its callee, not the run** (issue #783).
///
/// The seam's budget prices the bombs it knows, and the transport has to
/// survive the ones it does not. This asks the process engine directly, below
/// every gate, with four callees that each kill the child: one past the
/// three-respawn lifetime budget that used to abandon the fold surface. Each
/// death quarantines its callee — the next call of that name is declined
/// without dispatch, even a harmless one — and every other callee still
/// answers from a live child at the end.
///
/// The posture counts every death where it happened. The old edge detector
/// missed one whenever a request revived a dead child that then died again.
#[test]
fn a_callee_that_kills_the_child_is_quarantined_and_the_run_keeps_its_engine() {
    use steins_infer::FoldEngine;
    use steins_sidecar::{FoldArg, FoldResult, FoldValue};

    let s = |v: &str| FoldArg::Str(v.to_owned());
    let mut engine = steins_infer::ProcessEngine::enabled();
    if !matches!(engine.fold("strtoupper", &[s("probe")], true), FoldResult::Value(_)) {
        eprintln!(
            "SKIP a_callee_that_kills_the_child_is_quarantined_and_the_run_keeps_its_engine: \
             no folding engine — is `php` on PATH?"
        );
        return;
    }
    let bombs = [
        ("str_repeat", vec![s("x"), FoldArg::Int(2_000_000_000)], vec![s("ab"), FoldArg::Int(3)]),
        ("str_pad", vec![s("x"), FoldArg::Int(2_000_000_000)], vec![s("a"), FoldArg::Int(3)]),
        ("range", vec![FoldArg::Int(0), FoldArg::Int(100_000_000)], vec![FoldArg::Int(1), FoldArg::Int(3)]),
        ("sprintf", vec![s("%2000000000d"), FoldArg::Int(1)], vec![s("%d"), FoldArg::Int(1)]),
    ];
    for (i, (name, bomb, harmless)) in bombs.iter().enumerate() {
        assert!(!engine.is_quarantined(name), "{name} is not quarantined before it kills");
        let r = engine.fold(name, bomb, true);
        assert!(matches!(r, FoldResult::Widen { .. }), "{name}'s bomb widened, got {r:?}");
        assert!(engine.is_quarantined(name), "{name} killed a child and was not quarantined");
        assert_eq!(
            engine.fold(name, harmless, true),
            FoldResult::widen("callee quarantined"),
            "a quarantined {name} reached the engine again"
        );
        assert_eq!(
            engine.fold("strtoupper", &[s("alive")], true),
            FoldResult::Value(FoldValue::Str("ALIVE".to_owned())),
            "bomb {i} ({name}) cost an unrelated callee its answer"
        );
    }
    // The replay path's dispatch is quarantined the same way: the persisted
    // table engine sends every miss through `call_raw`.
    let params = steins_sidecar::fold_params("STR_REPEAT", &[s("ab"), FoldArg::Int(3)], true)
        .expect("askable");
    assert_eq!(engine.call_raw("fold", params), None, "the raw path ignored the quarantine");

    let posture = engine.posture();
    assert_eq!(posture.losses, 4, "one death per bomb: {posture:?}");
    assert_eq!(posture.restarts, 4, "and one replacement for each: {posture:?}");
    assert!(!posture.abandoned, "four deaths between answers are not a storm: {posture:?}");
}

// The whole-run `env` answers, across a restart (issue #245). No `php` needed:
// the transport's recovery is modeled directly, the only way to hold the
// decline window open on purpose.

/// A [`FoldEngine`] that declines everything until it has been "restarted" —
/// the mid-run recovery in miniature. A real transport revives itself on the
/// request *after* the one that killed it, so this window is genuinely hard to
/// open by hand; modeling it keeps the test about the policy it exposes (a
/// decline memoized for the whole run), not about reproducing the window.
#[derive(Default)]
struct RestartableEngine {
    /// The generation the folder reads through [`steins_infer::FoldEngine::restarts`].
    restarts: u32,
    /// `env` calls served, so the test can tell a re-ask from a memo hit.
    env_calls: u32,
}

impl steins_infer::FoldEngine for RestartableEngine {
    fn env(&mut self) -> Option<steins_sidecar::EnvInfo> {
        self.env_calls += 1;
        if self.restarts == 0 {
            return None; // the corpse's answer
        }
        Some(steins_sidecar::EnvInfo {
            php_version: "8.5.9".to_owned(),
            extensions: Vec::new(),
            sapi: "cli".to_owned(),
            int_size: Some(8),
        })
    }
    fn reflect(&mut self, _target: &str) -> Option<steins_sidecar::Reflection> {
        None
    }
    fn fold(
        &mut self,
        _name: &str,
        _args: &[steins_sidecar::FoldArg],
        _strict: bool,
    ) -> steins_sidecar::FoldResult {
        steins_sidecar::FoldResult::widen("stub")
    }
    fn preg_compile(&mut self, _pattern: &str) -> Option<steins_sidecar::PregCompile> {
        None
    }
    fn constant_defined(&mut self, _name: &str) -> Option<steins_sidecar::ConstantDefined> {
        None
    }
    fn reflect_class(&mut self, _target: &str) -> Option<steins_sidecar::ClassReflection> {
        None
    }
    fn restarts(&self) -> u32 {
        self.restarts
    }
}

/// A decline taken from a child that has since been replaced is asked again.
/// `php_minor` and its three `env`-derived siblings are memoized for the WHOLE
/// run, so a decline taken while the transport is down must not stay memoized
/// after it recovers — otherwise one badly timed request silently narrows what
/// the checker even asks, for the rest of the run.
#[test]
fn a_whole_run_env_answer_is_retaken_after_the_transport_restarts() {
    let mut folder = steins_infer::EngineFolder::with_engine(RestartableEngine::default());
    assert_eq!(folder.php_minor(), None, "the corpse declines");
    assert!(folder.boot_surface_label().is_none(), "and so does its sibling");

    folder.engine_mut().restarts = 1; // the transport replaced its child

    assert_eq!(
        folder.php_minor(),
        Some((8, 5)),
        "the live child's answer must replace the corpse's decline"
    );
    assert!(folder.boot_surface_label().is_some(), "every env-derived memo, not just one");
}

/// …and only then: within one generation the memo still does its job. Re-asking
/// on "the memo holds a decline" would pay the ADR-0024 timeout at every call
/// site against a merely-hung sidecar (issue #110's failure mode); re-asking on
/// "the engine has been replaced" costs one `env` per respawn instead, bounded
/// by how often the child is replaced.
#[test]
fn a_decline_is_asked_once_per_transport_generation_not_once_per_call_site() {
    let mut folder = steins_infer::EngineFolder::with_engine(RestartableEngine::default());
    assert_eq!(folder.php_minor(), None);
    let after_first = folder.engine_mut().env_calls;
    assert_eq!(after_first, 1, "the first ask reaches the engine");

    for _ in 0..10 {
        assert_eq!(folder.php_minor(), None);
    }
    assert_eq!(
        folder.engine_mut().env_calls,
        after_first,
        "a decline within one generation must be served from the memo"
    );
}
