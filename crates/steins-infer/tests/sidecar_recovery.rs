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
    // Neither call folded: the refused ones widen to their declared floor, and
    // in particular neither answers the value its callback would have produced.
    assert!(!d[0].starts_with("array{"), "a `var_dump` argument reached the runner: {}", d[0]);
    assert!(!d[1].starts_with("array{"), "`getenv` ran inside the analysis: {}", d[1]);
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
/// by the respawn cap.
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
