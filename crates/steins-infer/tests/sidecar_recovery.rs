//! A fold that kills the PHP child costs one answer, not the rest of the run.
//!
//! `str_repeat("x", 2000000000)` is an ordinary literal call on the folding
//! allowlist, and its result does not fit the runner's `memory_limit`. Memory
//! exhaustion is a PHP *fatal*, not a `Throwable`, so no `catch` in the runner can
//! turn it into a widen — the child dies mid-NDJSON. The transport recovers by
//! replacing it (`Sidecar`'s respawn discipline); what this file pins is that the
//! recovery survives the layers above, which is where a run-long disable would
//! actually be introduced: `ProcessEngine` latches "no engine" for a *spawn*
//! failure and for `--no-php`, and neither must be reached by a dead child.
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
    // A folder that cannot reach `php` folds nothing at all, which would make the
    // assertion below vacuous. Probe first and skip loudly instead — with an
    // argument the snippet does not use, because `EngineFolder` memoizes answers
    // and a probe of `strtoupper("ab")` would answer the snippet's second fold
    // from cache, hiding the very death this test is about.
    if folder.fold("strtoupper", &[ArgValue::Str("probe".to_owned())]).is_none() {
        eprintln!(
            "SKIP a_bomb_fold_does_not_disable_the_folder_for_the_rest_of_the_run: \
             no folding engine — is `php` on PATH?"
        );
        return;
    }
    assert_eq!(dumps(BOMB_THEN_FOLDABLE, &mut folder), vec!["string", "'AB'"]);
}
