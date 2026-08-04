//! Integration tests for Transform #3 — `@throws` envelope seeding (issue #115
//! / ADR-0040). Each test builds a real salsa project and asserts on the plan's
//! edits AND the named refusal reasons. The applied output is compared as exact
//! bytes where the lossless guarantee (ADR-0003) is what matters.

use steins_db::{Project, SourceFile, SteinsDatabase};
use steins_edit::TransformReport;
use steins_edit::envelope::{
    REASON_ALREADY_DECLARED, REASON_DECLARATION_MID_LINE, REASON_DOCBLOCK_NOT_ROUND_TRIPPABLE,
    REASON_ESCAPE_NOT_PROVEN,
};
use steins_edit::plan_throws_envelope;

fn plan(files: &[(&str, &str)]) -> TransformReport {
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
    plan_throws_envelope(&db, project, None)
}

fn assert_oracle_complete(report: &TransformReport) {
    assert!(report.oracle.is_complete(), "oracle incomplete: {:?}", report.oracle);
}

fn only_reason(report: &TransformReport) -> &str {
    assert_eq!(report.refusals.len(), 1, "expected one refusal, got: {:#?}", report.refusals);
    &report.refusals[0].reason
}

// ---- 1. The flagship seed: no docblock → created --------------------------

#[test]
fn no_docblock_seeds_a_created_envelope() {
    let lib = "<?php\nfunction f(): void { throw new \\RuntimeException(\"boom\"); }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.enumerated, 1);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);

    let out = report.plan.apply_file("lib.php", lib);
    assert_eq!(
        out,
        "<?php\n/**\n * @throws \\RuntimeException\n */\nfunction f(): void { throw new \\RuntimeException(\"boom\"); }\n"
    );
}

#[test]
fn method_seed_creates_an_indented_docblock() {
    let lib = "<?php\nclass W {\n    public function risky(): void {\n        throw new \\RuntimeException(\"boom\");\n    }\n}\n";
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);

    let out = report.plan.apply_file("lib.php", lib);
    assert!(
        out.contains(
            "    /**\n     * @throws \\RuntimeException\n     */\n    public function risky(): void {"
        ),
        "indented docblock missing:\n{out}"
    );
}

#[test]
fn propagated_escape_seeds_the_caller_too() {
    // g throws directly; f only calls g. Both have proven escapes, both seed.
    let lib = "<?php\nfunction g(): void { throw new \\RuntimeException(\"x\"); }\nfunction f(): void { g(); }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.enumerated, 2);
    assert_eq!(report.oracle.transformed, 2, "{:#?}", report.refusals);

    let out = report.plan.apply_file("lib.php", lib);
    assert_eq!(out.matches(" * @throws \\RuntimeException\n").count(), 2, "got:\n{out}");
}

#[test]
fn multiple_proven_classes_get_one_tag_each_in_source_order() {
    let lib = "<?php\nfunction f(bool $b): void {\n    if ($b) { throw new \\RuntimeException(\"r\"); }\n    throw new \\JsonException(\"j\");\n}\n";
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);

    let out = report.plan.apply_file("lib.php", lib);
    let r = out.find("@throws \\RuntimeException").expect("RuntimeException tag");
    let j = out.find("@throws \\JsonException").expect("JsonException tag");
    assert!(r < j, "source order of the proven set must be preserved:\n{out}");
}

// ---- 2. Lossless extension of an existing docblock ------------------------

#[test]
fn existing_docblock_is_extended_losslessly() {
    let lib = "<?php\n/**\n * Summary line.\n *\n * @param int $x  the count\n */\nfunction f($x): void { throw new \\RuntimeException(\"boom\"); }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);

    // Byte-exact: every pre-existing line preserved, the new tag line inserted
    // directly before the closing `*/` (ADR-0003 lossless-CST guarantee).
    let out = report.plan.apply_file("lib.php", lib);
    assert_eq!(
        out,
        "<?php\n/**\n * Summary line.\n *\n * @param int $x  the count\n * @throws \\RuntimeException\n */\nfunction f($x): void { throw new \\RuntimeException(\"boom\"); }\n"
    );
}

/// A CRLF file stays a CRLF file: the seeded lines carry the terminator the
/// file already uses, in both the create and the extend path. Existing bytes are
/// preserved either way — this is about not leaving mixed endings behind.
#[test]
fn seeded_lines_match_the_files_own_line_terminator() {
    let created = "<?php\r\nfunction f(): void { throw new \\RuntimeException(\"boom\"); }\r\n";
    let out = plan(&[("lib.php", created)]).plan.apply_file("lib.php", created);
    assert_eq!(
        out,
        "<?php\r\n/**\r\n * @throws \\RuntimeException\r\n */\r\nfunction f(): void { throw new \\RuntimeException(\"boom\"); }\r\n"
    );

    let extended = "<?php\r\n/**\r\n * Summary line.\r\n */\r\nfunction f(): void { throw new \\RuntimeException(\"boom\"); }\r\n";
    let out = plan(&[("lib.php", extended)]).plan.apply_file("lib.php", extended);
    assert_eq!(
        out,
        "<?php\r\n/**\r\n * Summary line.\r\n * @throws \\RuntimeException\r\n */\r\nfunction f(): void { throw new \\RuntimeException(\"boom\"); }\r\n"
    );
}

#[test]
fn partially_declared_envelope_gains_only_the_missing_class() {
    let lib = "<?php\n/**\n * @throws \\RuntimeException\n */\nfunction f(bool $b): void {\n    if ($b) { throw new \\RuntimeException(\"r\"); }\n    throw new \\JsonException(\"j\");\n}\n";
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);

    let out = report.plan.apply_file("lib.php", lib);
    assert_eq!(out.matches("@throws \\RuntimeException").count(), 1, "already covered:\n{out}");
    assert!(out.contains(" * @throws \\JsonException\n */"), "missing class seeded:\n{out}");
}

#[test]
fn declared_parent_class_covers_the_subclass() {
    // OutOfBoundsException <: RuntimeException — the declared parent covers it,
    // so nothing is written and the run refuses `already-declared`.
    let lib = "<?php\n/**\n * @throws \\RuntimeException\n */\nfunction f(): void { throw new \\OutOfBoundsException(\"x\"); }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.enumerated, 1);
    assert_eq!(only_reason(&report), REASON_ALREADY_DECLARED);
    assert!(report.plan.is_empty());
}

// ---- 3. Only proven escapes are written (ADR-0037/0040) --------------------

#[test]
fn maybe_escape_refuses_and_never_annotates() {
    // MyExc extends an external \Vendor\Base, so its ancestry leaves known
    // territory; the catch of \Vendor\Other MIGHT absorb it → Maybe escape.
    // A Maybe never becomes a declared envelope: refuse with a named reason.
    let lib = "<?php\nclass MyExc extends \\Vendor\\Base {}\nfunction f(): void { try { throw new MyExc(); } catch (\\Vendor\\Other $e) {} }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.enumerated, 1);
    assert_eq!(only_reason(&report), REASON_ESCAPE_NOT_PROVEN);
    assert!(report.plan.is_empty(), "a Maybe escape must never be annotated");
}

#[test]
fn mixed_proven_and_maybe_writes_only_the_proven_class() {
    // RuntimeException provably escapes; MyExc (unknown ancestry) only maybe
    // does. The seeded envelope carries the proven class alone.
    let lib = "<?php\nclass MyExc extends \\Vendor\\Base {}\nfunction f(): void {\n    try { throw new MyExc(); } catch (\\Vendor\\Other $e) {}\n    throw new \\RuntimeException(\"r\");\n}\n";
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);

    let out = report.plan.apply_file("lib.php", lib);
    assert!(out.contains("@throws \\RuntimeException"), "proven class seeded:\n{out}");
    assert!(!out.contains("@throws \\MyExc"), "a Maybe escape must not be written:\n{out}");
}

#[test]
fn unchecked_families_are_not_candidates() {
    // Error/LogicException families never count against envelopes (ADR-0007):
    // a declaration whose only escapes are unchecked is not even enumerated.
    let lib = "<?php\nfunction f(): void { throw new \\TypeError(\"t\"); }\nfunction g(): void { throw new \\InvalidArgumentException(\"l\"); }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.enumerated, 0, "{:#?}", report.refusals);
    assert!(report.plan.is_empty());
}

// ---- 4. Idempotence: the second run is a no-op -----------------------------

#[test]
fn second_run_refuses_already_declared_and_writes_nothing() {
    let lib = "<?php\nfunction f(): void { throw new \\RuntimeException(\"boom\"); }\n";
    let first = plan(&[("lib.php", lib)]);
    assert_eq!(first.oracle.transformed, 1, "{:#?}", first.refusals);
    let seeded = first.plan.apply_file("lib.php", lib);

    let second = plan(&[("lib.php", &seeded)]);
    assert_oracle_complete(&second);
    assert_eq!(second.oracle.enumerated, 1, "the candidate is still enumerated");
    assert_eq!(second.oracle.transformed, 0);
    assert_eq!(only_reason(&second), REASON_ALREADY_DECLARED);
    assert!(second.plan.is_empty(), "second run must be a byte-level no-op");
    assert_eq!(second.plan.apply_file("lib.php", &seeded), seeded);
}

// ---- 5. Docblocks the parser cannot round-trip -----------------------------

#[test]
fn single_line_docblock_refuses_round_trip() {
    let lib = "<?php\n/** existing summary */\nfunction f(): void { throw new \\RuntimeException(\"x\"); }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.enumerated, 1);
    assert_eq!(only_reason(&report), REASON_DOCBLOCK_NOT_ROUND_TRIPPABLE);
    assert!(report.plan.is_empty());
}

#[test]
fn closing_delimiter_sharing_a_line_refuses_round_trip() {
    let lib = "<?php\n/**\n * summary\n * @param int $x */\nfunction f($x): void { throw new \\RuntimeException(\"x\"); }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(only_reason(&report), REASON_DOCBLOCK_NOT_ROUND_TRIPPABLE);
    assert!(report.plan.is_empty());
}

#[test]
fn mid_line_declaration_refuses() {
    // The whole file on one line: no line of its own to hold a docblock.
    let lib = "<?php function f(): void { throw new \\RuntimeException(\"x\"); }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(only_reason(&report), REASON_DECLARATION_MID_LINE);
    assert!(report.plan.is_empty());
}

// ---- 6. The seeded spelling is the machinery's FQN -------------------------

#[test]
fn namespaced_project_exception_is_seeded_fully_qualified() {
    let lib = "<?php\nnamespace App;\nclass BoomException extends \\RuntimeException {}\nfunction f(): void { throw new BoomException(\"x\"); }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);

    let out = report.plan.apply_file("lib.php", lib);
    assert!(
        out.contains(" * @throws \\App\\BoomException\n"),
        "the tag must spell the resolved FQN, fully qualified:\n{out}"
    );
}
