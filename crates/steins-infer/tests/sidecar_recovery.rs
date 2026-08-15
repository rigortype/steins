//! A fold that kills the PHP child costs one answer, not the rest of the run.
//!
//! `str_repeat("x", 2000000000)` is an ordinary literal call on the folding
//! allowlist, and its result does not fit the runner's `memory_limit`. Memory
//! exhaustion is a PHP *fatal*, not a `Throwable`, so no `catch` in the runner
//! can turn it into a widen — the child dies mid-NDJSON. The transport recovers
//! by replacing it (`Sidecar`'s respawn discipline); this file pins that the
//! recovery survives the layers above — `ProcessEngine` latches "no engine" for
//! a *spawn* failure and for `--no-php`, and neither must be reached by a dead
//! child.
//!
//! Since issue #245 this file also pins what the run is allowed to SAY about
//! that recovery (phpstan-src ships `str_repeat('abcdefghij', 1000000000)` as
//! its own regression fixture, so the death is not hypothetical): ADR-0004
//! says incompleteness is never silent, and a posture that changes partway is
//! still a posture.
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

/// The same run, read as a coverage posture (issue #245): recovering from the
/// death must not make it *unsayable*. The transport's recovery is exactly what
/// makes this hard to see — a sidecar that answers, dies, is replaced, and
/// answers again looks from the outside like one that lost nothing; the posture
/// is the only place the difference survives to the end of the run.
#[test]
fn a_recovered_death_still_shows_in_the_run_posture() {
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
    assert_eq!(after.losses, 1, "the bomb cost exactly one answer, got {after:?}");
    assert_eq!(after.restarts, 1, "and exactly one child was replaced, got {after:?}");
    assert!(!after.abandoned, "one death is far inside the respawn budget, got {after:?}");
    assert!(
        !after.sidecar_backed_throughout(),
        "a run that lost an answer is not comparable with one that lost none, got {after:?}"
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
