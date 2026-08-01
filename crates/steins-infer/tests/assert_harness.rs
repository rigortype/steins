//! The assertType harness seam (oracle idea B) — `collect_assert_types`.
//!
//! The harness consumes `PHPStan\Testing\assertType('T', $e)` and measures Steins'
//! rendering of `$e` against the asserted string. Recognition is gated on the
//! thread-local sink, so a normal check never sees `assertType` as anything but an
//! ordinary call (the byte-identity pin, tested last). Inside the harness universe
//! an `assertType` site is a transparent read (the ADR-0070 gate's assertType
//! exception): repeated assertions on one variable observe the same env.

use steins_db::{PluginFacts, Project, ProjectLayout, SourceFile, SteinsDatabase};
use steins_infer::{AssertObservation, DEBUG_TYPE_ID, NoFold, check, collect_assert_types};
use steins_syntax::SourceTree;

/// Run the harness over one source file (a single-file project, the nsrt shape).
fn observations(src: &str) -> Vec<AssertObservation> {
    let db = SteinsDatabase::default();
    let input = SourceFile::new(&db, "t.php".to_owned(), src.to_owned());
    let project = Project::new(&db, vec![input], ProjectLayout::fallback(), PluginFacts::none());
    collect_assert_types(&db, project, &mut NoFold)
}

const USE_ASSERT: &str = "use function PHPStan\\Testing\\assertType;";

#[test]
fn a_second_assert_of_one_variable_observes_the_same_env() {
    // Regression (2026-08-02): each assertType call invalidated its own argument
    // (FnResolution::Unknown → blanket drop), so every assertion after the first
    // measured a degraded env. An assertType site is a read in the harness
    // universe: the second observation must equal the first.
    let src = format!(
        "<?php\n{USE_ASSERT}\n$x = 5;\nassertType('5', $x);\nassertType('5', $x);\n"
    );
    let obs = observations(&src);
    assert_eq!(obs.len(), 2, "{obs:?}");
    assert_eq!(obs[0].got, "5");
    assert_eq!(obs[1].got, "5", "the second assertion must not read its own scaffolding's drop");
}

#[test]
fn a_second_assert_of_a_docblock_param_keeps_the_contract() {
    // The contract-lane shape of the same regression, phpstan-src's most common
    // nsrt pattern: a parameter whose only fact is the docblock contract, asserted
    // twice inside one function body.
    let src = format!(
        "<?php\n{USE_ASSERT}\n/** @param non-empty-string $m */\nfunction f($m) {{\n\
         assertType('non-empty-string', $m);\nassertType('non-empty-string', $m);\n}}\n"
    );
    let obs = observations(&src);
    assert_eq!(obs.len(), 2, "{obs:?}");
    assert_eq!(obs[0].got, obs[1].got, "{obs:?}");
    assert_eq!(obs[1].got, "non-empty-string");
}

#[test]
fn an_unknown_call_between_asserts_still_degrades() {
    // The contrast pin: the exception is the assertType site itself, nothing
    // wider. A genuinely unknown call between two assertions keeps the ADR-0070
    // conservative drop, and the second assertion honestly reads `unknown`.
    let src = format!(
        "<?php\n{USE_ASSERT}\n$x = 5;\nassertType('5', $x);\nmystery($x);\nassertType('5', $x);\n"
    );
    let obs = observations(&src);
    assert_eq!(obs.len(), 2, "{obs:?}");
    assert_eq!(obs[0].got, "5");
    assert_eq!(obs[1].got, "unknown");
}

#[test]
fn without_the_sink_assert_type_stays_an_ordinary_call() {
    // The byte-identity pin (the emit_asserts doc): in a normal check the sink is
    // absent, so `assertType` is an ordinary unresolved call — it invalidates its
    // argument exactly as any unknown callee does. Observed through the dump
    // surface: the dump after the assertion reads the conservatively dropped env.
    let src = "<?php\nuse function PHPStan\\Testing\\assertType;\n$x = 5;\n\
               assertType('5', $x);\n\\PHPStan\\dumpType($x);\n";
    let tree = SourceTree::parse(src);
    let dumps: Vec<_> =
        check(&tree, &[], "t.php").into_iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(dumps.len(), 1, "{dumps:?}");
    assert_eq!(
        dumps[0].message, "dumped type: unknown",
        "a normal check must treat assertType as an ordinary call"
    );
}
