//! A statement-position call to a resolved `: never` callee terminates its trace
//! (issue #599, ADR-0081 §9's leg 1).
//!
//! The gap these fixtures close: a branch ending in a plain `throw` contributed
//! nothing to the join, while a branch ending in `boom()` — `function boom():
//! never` — fell through, so the arm the guard had just excluded survived and
//! every downstream consumer read a value the program cannot hold there. The
//! silences below come in PAIRS with the plain-`throw` twin or with the
//! unguarded shape that must still report, because "equal to the terminator that
//! already worked" is the claim worth making, and "quiet" on its own is not.
//!
//! The premise is the callee's own NATIVE `: never` and nothing weaker. PHP
//! fatals when a `never` function returns, so the declaration is a runtime
//! contract; a docblock's `@return never` is a comment's claim, and pruning on it
//! would let a wrong comment delete real code from the analysis. That refusal,
//! the dynamic callee, the unresolved receiver and the conditional declaration
//! are pinned here too — this change SILENCES, so every decline is load-bearing.

use steins_db::{Project, SourceFile, SteinsDatabase};
use steins_infer::{
    Diagnostic, ID, NoFold, TYPE_MAYBE_ARGUMENT_MISMATCH_ID, TYPE_MAYBE_RETURN_MISMATCH_ID, check,
    check_project,
};
use steins_syntax::SourceTree;

/// Every diagnostic `body` produces, under `strict_types` — the mode in which a
/// `false` arm reaching a `string` parameter is a `TypeError` rather than a
/// coercion, and so the mode this family is visible in at all.
fn run(body: &str) -> Vec<Diagnostic> {
    let src = format!("<?php\ndeclare(strict_types=1);\n{body}");
    let tree = SourceTree::parse(&src);
    check(&tree, &[], "t.php")
}

/// The possibly-grade findings of `body`, whichever seam they came from.
fn maybes(body: &str) -> Vec<Diagnostic> {
    run(body)
        .into_iter()
        .filter(|d| {
            d.id == TYPE_MAYBE_RETURN_MISMATCH_ID || d.id == TYPE_MAYBE_ARGUMENT_MISMATCH_ID
        })
        .collect()
}

/// The possibly-grade findings of a real multi-file project — the shape the
/// namespace-shadow rule needs, since that hazard only exists across files.
fn project_maybes(files: &[(&str, &str)]) -> Vec<Diagnostic> {
    let db = SteinsDatabase::default();
    let inputs: Vec<SourceFile> = files
        .iter()
        .map(|(p, t)| SourceFile::new(&db, (*p).to_owned(), (*t).to_owned()))
        .collect();
    let project = Project::new(
        &db,
        inputs,
        steins_db::ProjectLayout::fallback(),
        steins_db::PluginFacts::none(),
    );
    check_project(&db, project, &mut NoFold)
        .into_iter()
        .filter(|d| {
            d.id == TYPE_MAYBE_RETURN_MISMATCH_ID || d.id == TYPE_MAYBE_ARGUMENT_MISMATCH_ID
        })
        .collect()
}

/// A `: never` free function and a `string` sink, the two helpers most fixtures
/// need.
const HELPERS: &str = "function boom(): never { throw new \\RuntimeException('x'); }\n\
                       function needString(string $s): int { return strlen($s); }\n";

// ---------------------------------------------------------------------------
// The witness, and its plain-`throw` twin
// ---------------------------------------------------------------------------

#[test]
fn a_never_callee_prunes_the_guarded_arm() {
    // Issue #599's own repro. The `false` arm is excluded by the guard and the
    // branch that excluded it never reaches the join, so nothing is left to
    // report at the `return`.
    let d = maybes(&format!(
        "{HELPERS}function f(string|false $v): string {{\n\
         \x20   if ($v === false) {{ boom(); }}\n\
         \x20   return $v;\n\
         }}\n"
    ));
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn the_plain_throw_twin_is_the_same_silence() {
    let d = maybes(
        "function g(string|false $v): string {\n\
         \x20   if ($v === false) { throw new \\RuntimeException('x'); }\n\
         \x20   return $v;\n\
         }\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn an_unguarded_return_still_reports() {
    // The soundness end of the pair: pruning a branch must not prune the
    // judgment. With no guard at all the `false` arm really does reach the
    // `return`, and the id fires exactly as it did before this slice.
    let d = maybes(
        "function f(string|false $v): string {\n\
         \x20   return $v;\n\
         }\n",
    );
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].id, TYPE_MAYBE_RETURN_MISMATCH_ID);
}

// ---------------------------------------------------------------------------
// The if-branch join, at the corpus shape that motivated the slice
// ---------------------------------------------------------------------------

#[test]
fn the_argument_seam_reads_the_pruned_join() {
    // phpunit's `TextUI/Application.php:472`, reduced: `realpath(string $path)`
    // is reached with `$configurationFile` still carrying its `false` arm only
    // because the `never` call three lines up did not prune. The seed row this
    // fixture stands for is `type.maybe-argument-mismatch`, so the fix has to
    // reach the join and not merely the return statement it was found at.
    let d = maybes(&format!(
        "{HELPERS}final class App {{\n\
         \x20   public function migrate(string|false $configurationFile): int\n\
         \x20   {{\n\
         \x20       if ($configurationFile === false) {{\n\
         \x20           $this->bail('No configuration file found to migrate');\n\
         \x20       }}\n\
         \x20\n\
         \x20       return needString($configurationFile);\n\
         \x20   }}\n\
         \x20\n\
         \x20   private function bail(string $message): never\n\
         \x20   {{\n\
         \x20       throw new \\RuntimeException($message);\n\
         \x20   }}\n\
         }}\n"
    ));
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn a_self_dispatched_never_prunes() {
    // monolog's `Utils.php:140`: `self::throwEncodeError(…): never` guarding the
    // `false` arm of a `json_encode()` result.
    let d = maybes(
        "final class Utils {\n\
         \x20   public static function encode(string|false $json): string\n\
         \x20   {\n\
         \x20       if ($json === false) { self::throwEncodeError(); }\n\
         \x20\n\
         \x20       return $json;\n\
         \x20   }\n\
         \x20\n\
         \x20   private static function throwEncodeError(): never\n\
         \x20   {\n\
         \x20       throw new \\RuntimeException('x');\n\
         \x20   }\n\
         }\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn a_class_named_static_call_prunes() {
    let d = maybes(
        "final class Utils {\n\
         \x20   public static function encode(string|false $json): string\n\
         \x20   {\n\
         \x20       if ($json === false) { Utils::throwEncodeError(); }\n\
         \x20\n\
         \x20       return $json;\n\
         \x20   }\n\
         \x20\n\
         \x20   private static function throwEncodeError(): never\n\
         \x20   {\n\
         \x20       throw new \\RuntimeException('x');\n\
         \x20   }\n\
         }\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

// ---------------------------------------------------------------------------
// The declines — every one of them a finding this must NOT silence
// ---------------------------------------------------------------------------

#[test]
fn a_phpdoc_only_never_does_not_prune() {
    // `@return never` is `Asserted` (ADR-0069): a comment, not a contract PHP
    // enforces. Pruning on it would let a wrong docblock delete the rest of the
    // function from the analysis, which is the opposite of what a docblock is
    // allowed to do.
    let d = maybes(
        "/** @return never */\n\
         function pdBoom() { throw new \\RuntimeException('x'); }\n\
         function f(string|false $v): string {\n\
         \x20   if ($v === false) { pdBoom(); }\n\
         \x20   return $v;\n\
         }\n",
    );
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].id, TYPE_MAYBE_RETURN_MISMATCH_ID);
}

#[test]
fn a_dynamic_callee_does_not_prune() {
    let d = maybes(
        "function f(string|false $v, callable $c): string {\n\
         \x20   if ($v === false) { $c(); }\n\
         \x20   return $v;\n\
         }\n",
    );
    assert_eq!(d.len(), 1, "{d:?}");
}

#[test]
fn an_off_project_receiver_does_not_prune() {
    // The chain leaves the project, so dispatch answers nothing and the branch
    // falls through exactly as it does today.
    let d = maybes(
        "function f(string|false $v, \\DateTimeInterface $d): string {\n\
         \x20   if ($v === false) { $d->format('c'); }\n\
         \x20   return $v;\n\
         }\n",
    );
    assert_eq!(d.len(), 1, "{d:?}");
}

#[test]
fn a_conditionally_declared_never_does_not_prune() {
    // ADR-0049 A2i, the `function_exists`-guarded polyfill shape: which body
    // binds is a load-order fact, so the `never` on the declaration this index
    // happens to hold proves nothing about the call.
    let d = maybes(
        "if (!function_exists('boom')) {\n\
         \x20   function boom(): never { throw new \\RuntimeException('x'); }\n\
         }\n\
         function f(string|false $v): string {\n\
         \x20   if ($v === false) { boom(); }\n\
         \x20   return $v;\n\
         }\n",
    );
    assert_eq!(d.len(), 1, "{d:?}");
}

#[test]
fn a_namespaced_call_falling_back_to_the_global_never_does_not_prune() {
    // `namespace App; boom();` binds `App\boom` the moment anything defines it
    // and `\boom` only until then, so a global match is the answer only for as
    // long as it happens to be. The same call written fully qualified is settled
    // and prunes — the pair below is what makes this a resolution rule rather
    // than a blanket refusal. A two-file project, because the shadow hazard only
    // exists where the caller's namespace and the declaration's differ.
    let caller = |call: &str| {
        format!(
            "<?php\ndeclare(strict_types=1);\nnamespace App;\n\
             function f(string|false $v): string {{\n\
             \x20   if ($v === false) {{ {call}; }}\n\
             \x20   return $v;\n\
             }}\n"
        )
    };
    const GLOBAL_BOOM: &str =
        "<?php\ndeclare(strict_types=1);\nfunction boom(): never { throw new \\RuntimeException('x'); }\n";

    let shadowable = project_maybes(&[("main.php", &caller("boom()")), ("lib.php", GLOBAL_BOOM)]);
    assert_eq!(shadowable.len(), 1, "{shadowable:?}");

    let settled = project_maybes(&[("main.php", &caller("\\boom()")), ("lib.php", GLOBAL_BOOM)]);
    assert!(settled.is_empty(), "{settled:?}");
}

#[test]
fn a_call_bound_within_its_own_namespace_prunes() {
    // The other half of the resolution rule: nothing can shadow `App\bang` for a
    // caller already in `App`, so the bare spelling is settled and prunes.
    let d = maybes(
        "namespace App;\n\
         function bang(): never { throw new \\RuntimeException('x'); }\n\
         function f(string|false $v): string {\n\
         \x20   if ($v === false) { bang(); }\n\
         \x20   return $v;\n\
         }\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn a_never_callee_in_argument_position_does_not_prune() {
    // The claim is about a STATEMENT-position call, which is where "the trace
    // stops here" is a statement about the trace. An argument position is an
    // expression the enclosing statement evaluates, and this walk does not model
    // its evaluation order — declining is the same silence as before.
    let d = maybes(&format!(
        "{HELPERS}function f(string|false $v): string {{\n\
         \x20   if ($v === false) {{ $x = needString(boom()); }}\n\
         \x20   return $v;\n\
         }}\n"
    ));
    assert_eq!(d.len(), 1, "{d:?}");
}

// ---------------------------------------------------------------------------
// Dead-code marking parity (ADR-0002/0031 live-path discipline)
// ---------------------------------------------------------------------------

#[test]
fn code_after_a_never_call_is_read_exactly_as_code_after_a_throw() {
    // The pruned call takes the `Throw`/`Exit` arm verbatim, so what happens to
    // the statements behind it is whatever already happened behind a `throw`:
    // the walk stops, and the env-free direct pass — which does not follow
    // control flow — still judges them. Pinned as an equality rather than as a
    // count, because the count is the plain-`throw` behaviour's to change.
    let after_never = run(&format!(
        "{HELPERS}function f(bool $c): void {{\n\
         \x20   if ($c) {{\n\
         \x20       boom();\n\
         \x20       needString(1);\n\
         \x20   }}\n\
         }}\n"
    ));
    let after_throw = run(&format!(
        "{HELPERS}function f(bool $c): void {{\n\
         \x20   if ($c) {{\n\
         \x20       throw new \\RuntimeException('x');\n\
         \x20       needString(1);\n\
         \x20   }}\n\
         }}\n"
    ));
    let ids = |ds: &[Diagnostic]| -> Vec<String> {
        ds.iter().map(|d| format!("{}@{}", d.id, d.line)).collect()
    };
    assert_eq!(ids(&after_never), ids(&after_throw), "{after_never:?} vs {after_throw:?}");
    assert!(
        after_never.iter().any(|d| d.id == ID),
        "the fixture must actually carry a judgeable statement behind the terminator: {after_never:?}"
    );
}
