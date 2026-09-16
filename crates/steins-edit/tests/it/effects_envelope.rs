//! Integration tests for Transform #5 — interop-envelope emission (issue #303 /
//! ADR-0082 §7). Asserts plan edits and refusal reasons; output is byte-exact (ADR-0003).

use steins_db::{Project, SourceFile, SteinsDatabase};
use steins_edit::TransformReport;
use steins_edit::effects_envelope::{
    REASON_ALREADY_DECLARED, REASON_ATTRIBUTE_ENVELOPE, REASON_DECLARATION_MID_LINE,
    REASON_BOUND_LABEL_UNKNOWN, REASON_DOCBLOCK_NOT_ROUND_TRIPPABLE,
    REASON_EFFECTS_NOT_EXHAUSTIVE, REASON_EXISTING_TAG_UNREADABLE,
};
use steins_edit::plan_effects_envelope;

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
    plan_effects_envelope(&db, project, None)
}

fn assert_oracle_complete(report: &TransformReport) {
    assert!(report.oracle.is_complete(), "oracle incomplete: {:?}", report.oracle);
}

fn only_reason(report: &TransformReport) -> &str {
    assert_eq!(report.refusals.len(), 1, "expected one refusal, got: {:#?}", report.refusals);
    &report.refusals[0].reason
}

/// Plan over one file and return the applied text.
fn applied(path: &str, src: &str) -> String {
    plan(&[(path, src)]).plan.apply_file(path, src)
}

// 1. The flagship emission: an exhaustive impure bound

#[test]
fn no_docblock_creates_one_with_the_impure_bound() {
    let lib = "<?php\nfunction f(): void { file_put_contents(\"/x\", \"y\"); }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.enumerated, 1);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);

    assert_eq!(
        report.plan.apply_file("lib.php", lib),
        "<?php\n/**\n * @phpstan-impure io.fs.write\n */\nfunction f(): void { file_put_contents(\"/x\", \"y\"); }\n"
    );
}

#[test]
fn existing_docblock_is_extended_losslessly() {
    let lib = "<?php\n/**\n * Summary line.\n *\n * @param int $x  the count\n */\nfunction f($x): void { file_put_contents(\"/x\", \"y\"); }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);

    // Byte-exact, tag line inserted directly before the closing `*/` (ADR-0003).
    assert_eq!(
        report.plan.apply_file("lib.php", lib),
        "<?php\n/**\n * Summary line.\n *\n * @param int $x  the count\n * @phpstan-impure io.fs.write\n */\nfunction f($x): void { file_put_contents(\"/x\", \"y\"); }\n"
    );
}

/// Sort order matches what `annotate` prints.
#[test]
fn several_labels_are_one_comma_space_list_sorted() {
    let lib = "<?php\nfunction f(): void {\n    file_put_contents(\"/x\", \"y\");\n    echo \"hi\";\n    $t = time();\n}\n";
    let out = applied("lib.php", lib);
    assert!(
        out.contains(" * @phpstan-impure io.fs.write, io.output.buffer, nondet.time\n"),
        "one sorted comma-space list:\n{out}"
    );
}

#[test]
fn method_emission_creates_an_indented_docblock() {
    let lib = "<?php\nclass W {\n    public function run(): void { file_put_contents(\"/x\", \"y\"); }\n    public function pure(): int { return 1; }\n}\n";
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    // Pure siblings get nothing (ADR-0082 §7 never writes a per-declaration pure tag).
    assert_eq!(report.oracle.enumerated, 1, "{:#?}", report.refusals);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);

    let out = report.plan.apply_file("lib.php", lib);
    assert!(
        out.contains("    /**\n     * @phpstan-impure io.fs.write\n     */\n    public function run()"),
        "indented docblock:\n{out}"
    );
    assert_eq!(out.matches("@phpstan-").count(), 1, "the pure sibling stays untagged:\n{out}");
}

/// Covers both the create and the extend path.
#[test]
fn written_lines_match_the_files_own_line_terminator() {
    let created = "<?php\r\nfunction f(): void { file_put_contents(\"/x\", \"y\"); }\r\n";
    assert_eq!(
        applied("lib.php", created),
        "<?php\r\n/**\r\n * @phpstan-impure io.fs.write\r\n */\r\nfunction f(): void { file_put_contents(\"/x\", \"y\"); }\r\n"
    );

    let extended = "<?php\r\n/**\r\n * Summary line.\r\n */\r\nfunction f(): void { file_put_contents(\"/x\", \"y\"); }\r\n";
    assert_eq!(
        applied("lib.php", extended),
        "<?php\r\n/**\r\n * Summary line.\r\n * @phpstan-impure io.fs.write\r\n */\r\nfunction f(): void { file_put_contents(\"/x\", \"y\"); }\r\n"
    );
}

// 2. Silence: non-exhaustive, and pure

#[test]
fn non_exhaustive_inference_is_refused_and_writes_nothing() {
    // Uncatalogued builtin: no upper bound; bare ⊤ never written (ADR-0082 §3/§7).
    let lib = "<?php\nfunction f(): void { file_put_contents(\"/x\", \"y\"); some_unknown_fn(); }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.enumerated, 1);
    assert_eq!(only_reason(&report), REASON_EFFECTS_NOT_EXHAUSTIVE);
    assert!(report.plan.is_empty(), "a non-exhaustive summary must never be written");
}

#[test]
fn a_pure_free_function_is_not_even_a_candidate() {
    // No per-declaration `@phpstan-pure`, and no class-level tag reaches a free function.
    let lib = "<?php\nfunction f(string $s): string { return strtolower($s); }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.enumerated, 0, "{:#?}", report.refusals);
    assert!(report.plan.is_empty());
}

// 3. Normalization: the two lanes, and prefix subsumption

/// Concretely: `io.fs` subsumes `io.fs.read`.
#[test]
fn declared_lane_joins_the_proven_one_and_subsumption_dedupes() {
    let lib = concat!(
        "<?php\n",
        "interface Repo {\n",
        "    #[\\Steins\\Effect('io.fs')]\n",
        "    public function load(): string;\n",
        "}\n",
        "function f(Repo $r): string {\n",
        "    file_get_contents(\"/x\");\n",
        "    return $r->load();\n",
        "}\n",
    );
    let out = applied("lib.php", lib);
    assert!(out.contains(" * @phpstan-impure io.fs\n"), "the coarse bound alone:\n{out}");
    assert!(!out.contains("io.fs.read"), "the subsumed proven label drops out:\n{out}");
}

#[test]
fn both_lanes_show_up_when_neither_subsumes_the_other() {
    let lib = concat!(
        "<?php\n",
        "interface Clock {\n",
        "    #[\\Steins\\Effect('nondet.time')]\n",
        "    public function now(): int;\n",
        "}\n",
        "function f(Clock $c): int {\n",
        "    file_put_contents(\"/x\", \"y\");\n",
        "    return $c->now();\n",
        "}\n",
    );
    let out = applied("lib.php", lib);
    assert!(
        out.contains(" * @phpstan-impure io.fs.write, nondet.time\n"),
        "the union of both lanes:\n{out}"
    );
}

// 4. The class-level tag (ADR-0082 §5/§7)

/// A constructor and a void-returning method both count as "pure" here.
#[test]
fn all_pure_class_gets_the_class_tag_and_no_method_tags() {
    let lib = concat!(
        "<?php\n",
        "class C {\n",
        "    private int $n;\n",
        "    public function __construct(int $n) { $this->n = $n; }\n",
        "    public function get(): int { return $this->n; }\n",
        "    public function nothing(): void {}\n",
        "}\n",
    );
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.enumerated, 1, "one class-level candidate: {:#?}", report.refusals);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);

    let out = report.plan.apply_file("lib.php", lib);
    assert_eq!(
        out,
        concat!(
            "<?php\n",
            "/**\n",
            " * @phpstan-all-methods-pure\n",
            " */\n",
            "class C {\n",
            "    private int $n;\n",
            "    public function __construct(int $n) { $this->n = $n; }\n",
            "    public function get(): int { return $this->n; }\n",
            "    public function nothing(): void {}\n",
            "}\n",
        )
    );
}

#[test]
fn class_docblock_is_extended_when_present() {
    let lib = concat!(
        "<?php\n",
        "/**\n",
        " * A value holder.\n",
        " */\n",
        "class C {\n",
        "    public function get(): int { return 1; }\n",
        "}\n",
    );
    let out = applied("lib.php", lib);
    assert!(
        out.contains(" * A value holder.\n * @phpstan-all-methods-pure\n */\nclass C {"),
        "extended in place:\n{out}"
    );
}

/// It gets its own bound; pure siblings get nothing.
#[test]
fn one_impure_method_disqualifies_the_class() {
    let lib = concat!(
        "<?php\n",
        "class C {\n",
        "    public function get(): int { return 1; }\n",
        "    public function save(): void { file_put_contents(\"/x\", \"y\"); }\n",
        "}\n",
    );
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.enumerated, 1, "only the impure method: {:#?}", report.refusals);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);

    let out = report.plan.apply_file("lib.php", lib);
    assert!(!out.contains("all-methods-pure"), "no class tag:\n{out}");
    assert_eq!(out.matches("@phpstan-impure io.fs.write").count(), 1, "one method tag:\n{out}");
    assert!(
        out.contains("     */\n    public function save()"),
        "the tag goes on the impure method:\n{out}"
    );
}

#[test]
fn a_non_exhaustive_method_refuses_the_class_tag() {
    let lib = concat!(
        "<?php\n",
        "class C {\n",
        "    public function get(): int { return 1; }\n",
        "    public function poke(): void { some_unknown_fn(); }\n",
        "}\n",
    );
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(only_reason(&report), REASON_EFFECTS_NOT_EXHAUSTIVE);
    assert!(report.plan.is_empty(), "an unproven class-wide claim is never written");
}

/// No bodies means nothing is proven; not a class-tag candidate.
#[test]
fn an_interface_is_never_a_class_tag_candidate() {
    let lib = "<?php\ninterface I {\n    public function get(): int;\n}\n";
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.enumerated, 0, "{:#?}", report.refusals);
    assert!(report.plan.is_empty());
}

// 5. The checked spelling shadows this whole stratum (ADR-0082 §1)

#[test]
fn an_attribute_bearing_declaration_is_skipped() {
    let lib = concat!(
        "<?php\n",
        "#[\\Steins\\Effect('io.fs')]\n",
        "function f(): void { file_put_contents(\"/x\", \"y\"); }\n",
    );
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.enumerated, 1);
    assert_eq!(only_reason(&report), REASON_ATTRIBUTE_ENVELOPE);
    assert!(report.plan.is_empty(), "a docblock twin of a checked envelope is duplication");
}

#[test]
fn an_attribute_bearing_method_disqualifies_its_class() {
    let lib = concat!(
        "<?php\n",
        "class C {\n",
        "    #[\\Steins\\Pure]\n",
        "    public function get(): int { return 1; }\n",
        "    public function other(): int { return 2; }\n",
        "}\n",
    );
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.enumerated, 1, "the class is the candidate");
    assert_eq!(only_reason(&report), REASON_ATTRIBUTE_ENVELOPE);
    assert!(report.plan.is_empty());
}

// 6. Idempotence and stale bounds

#[test]
fn the_same_bound_already_declared_refuses_and_writes_nothing() {
    let lib = "<?php\n/**\n * @phpstan-impure io.fs.write\n */\nfunction f(): void { file_put_contents(\"/x\", \"y\"); }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.enumerated, 1);
    assert_eq!(only_reason(&report), REASON_ALREADY_DECLARED);
    assert!(report.plan.is_empty());
}

/// Acceptance property: idempotent on its own output.
#[test]
fn running_twice_is_a_no_op() {
    for src in [
        "<?php\nfunction f(): void { file_put_contents(\"/x\", \"y\"); }\n",
        "<?php\nclass C {\n    public function get(): int { return 1; }\n}\n",
        "<?php\nclass C {\n    public function get(): int { return 1; }\n    public function save(): void { file_put_contents(\"/x\", \"y\"); }\n}\n",
    ] {
        let first = applied("lib.php", src);
        assert_ne!(first, src, "the first run must write something:\n{src}");
        let second = plan(&[("lib.php", &first)]);
        assert_oracle_complete(&second);
        assert_eq!(second.oracle.transformed, 0, "second run: {:#?}", second.refusals);
        assert!(second.plan.is_empty(), "the second run must be a no-op:\n{first}");
        assert!(
            second.refusals.iter().all(|r| r.reason == REASON_ALREADY_DECLARED),
            "second run refusals: {:#?}",
            second.refusals
        );
    }
}

/// A stale bound is corrected in place; every other byte is preserved.
#[test]
fn a_stale_bound_is_replaced_in_place() {
    let lib = "<?php\n/**\n * Writes the cache.\n * @phpstan-impure nondet.time (why it is not pure)\n * @return void\n */\nfunction f(): void { file_put_contents(\"/x\", \"y\"); }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);
    assert_eq!(
        report.plan.apply_file("lib.php", lib),
        "<?php\n/**\n * Writes the cache.\n * @phpstan-impure io.fs.write (why it is not pure)\n * @return void\n */\nfunction f(): void { file_put_contents(\"/x\", \"y\"); }\n"
    );
}

/// A false `@phpstan-pure` claim is corrected in place, tag name included.
#[test]
fn a_false_pure_claim_is_corrected_to_the_proven_bound() {
    let lib = "<?php\n/**\n * @phpstan-pure\n */\nfunction f(): void { file_put_contents(\"/x\", \"y\"); }\n";
    assert_eq!(
        applied("lib.php", lib),
        "<?php\n/**\n * @phpstan-impure io.fs.write\n */\nfunction f(): void { file_put_contents(\"/x\", \"y\"); }\n"
    );
}

/// A class-level envelope already present is left alone: wider-than-true isn't
/// false, and this transform never narrows.
#[test]
fn an_existing_class_level_envelope_is_left_alone() {
    let lib = concat!(
        "<?php\n",
        "/**\n",
        " * @phpstan-all-methods-impure io\n",
        " */\n",
        "class C {\n",
        "    public function get(): int { return 1; }\n",
        "}\n",
    );
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(only_reason(&report), REASON_ALREADY_DECLARED);
    assert!(report.plan.is_empty());
}

// 7. Edit mechanics: the round-trip gate

/// No lossless insertion point (same mechanics/reason as the `@throws` sister).
#[test]
fn single_line_docblock_refuses_round_trip() {
    let lib = "<?php\n/** Writes a file. */\nfunction f(): void { file_put_contents(\"/x\", \"y\"); }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(only_reason(&report), REASON_DOCBLOCK_NOT_ROUND_TRIPPABLE);
    assert!(report.plan.is_empty());
}

#[test]
fn closing_delimiter_sharing_a_line_refuses_round_trip() {
    let lib = "<?php\n/**\n * summary\n * @param int $x */\nfunction f($x): void { file_put_contents(\"/x\", \"y\"); }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(only_reason(&report), REASON_DOCBLOCK_NOT_ROUND_TRIPPABLE);
    assert!(report.plan.is_empty());
}

#[test]
fn mid_line_declaration_refuses() {
    // The whole file on one line: no line of its own to hold a docblock.
    let lib = "<?php function f(): void { file_put_contents(\"/x\", \"y\"); }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(only_reason(&report), REASON_DECLARATION_MID_LINE);
    assert!(report.plan.is_empty());
}

/// Round-trip is *verified*, not assumed: an attributed class looks insertable
/// but lands unassociated — caught by re-parse.
#[test]
fn a_docblock_the_reparse_cannot_see_is_refused() {
    let lib = concat!(
        "<?php\n",
        "#[\\Attribute]\n",
        "class C {\n",
        "    public function get(): int { return 1; }\n",
        "}\n",
    );
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.enumerated, 1);
    assert_eq!(only_reason(&report), REASON_DOCBLOCK_NOT_ROUND_TRIPPABLE);
    assert!(report.plan.is_empty(), "an unverifiable write never enters the plan");
}

// 8. Vendor is outside the write contract (ADR-0015)

#[test]
fn vendor_declarations_are_never_written() {
    let report = plan(&[(
        "vendor/acme/lib.php",
        "<?php\nfunction f(): void { file_put_contents(\"/x\", \"y\"); }\n",
    )]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.enumerated, 0, "{:#?}", report.refusals);
    assert!(report.plan.is_empty());
}

// 9. An unreadable existing tag is prose, not a stale bound. Owner ruling
// 2026-08-12: any unknown label makes the tag unspecified, whole (PHPStan
// discards everything after `@phpstan-impure`) — never overwrite a human's note.

#[test]
fn an_existing_prose_tag_is_never_overwritten() {
    let lib = "<?php\n/**\n * @phpstan-impure database\n */\nfunction f(): void { file_put_contents(\"/x\", \"y\"); }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.enumerated, 1);
    assert_eq!(only_reason(&report), REASON_EXISTING_TAG_UNREADABLE);
    assert!(report.plan.is_empty());
    assert_eq!(report.plan.apply_file("lib.php", lib), lib, "the file must be byte-identical");
}

/// One-line spelling: unreadable-tag wins over mechanics — bytes aren't ours to move.
#[test]
fn a_one_line_prose_tag_is_refused_as_prose() {
    let lib = "<?php\n/** @phpstan-impure database */\nfunction f(): void { file_put_contents(\"/x\", \"y\"); }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(only_reason(&report), REASON_EXISTING_TAG_UNREADABLE);
    assert!(report.plan.is_empty());
}

/// A typo is indistinguishable from prose to a registry: leave it alone either way.
#[test]
fn a_typoed_existing_bound_is_prose_not_a_stale_bound() {
    let lib = "<?php\n/**\n * @phpstan-impure io.netw\n */\nfunction f(): void { file_put_contents(\"/x\", \"y\"); }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(only_reason(&report), REASON_EXISTING_TAG_UNREADABLE);
    assert_eq!(report.plan.apply_file("lib.php", lib), lib, "the file must be byte-identical");
}

/// ADR-0083 retired `output`; the old spelling reads as prose, refused rather than "upgraded".
#[test]
fn a_retired_output_bound_is_prose_and_stays_byte_untouched() {
    let lib = "<?php\n/**\n * @phpstan-impure output\n */\nfunction f(): void { echo \"hi\"; }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.enumerated, 1);
    assert_eq!(only_reason(&report), REASON_EXISTING_TAG_UNREADABLE);
    assert!(report.plan.is_empty());
    assert_eq!(report.plan.apply_file("lib.php", lib), lib, "the file must be byte-identical");
}

/// Writing side of the same migration: never the retired root.
#[test]
fn emission_writes_the_new_output_vocabulary() {
    let lib = "<?php\nfunction f(): void { echo \"hi\"; }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);
    assert_eq!(
        report.plan.apply_file("lib.php", lib),
        "<?php\n/**\n * @phpstan-impure io.output.buffer\n */\nfunction f(): void { echo \"hi\"; }\n"
    );
}

/// One unreadable label makes the **whole** tag unspecified, known labels included.
#[test]
fn one_unknown_label_makes_the_whole_tag_unreadable() {
    let lib = "<?php\n/**\n * @phpstan-impure io.fs.write, database\n */\nfunction f(): void { file_put_contents(\"/x\", \"y\"); }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(only_reason(&report), REASON_EXISTING_TAG_UNREADABLE);
    assert!(report.plan.is_empty());
}

/// Not normally a candidate, but still enumerated and refused: "left alone" is owed.
#[test]
fn a_pure_declaration_with_a_prose_tag_is_reported_not_silently_skipped() {
    let lib = "<?php\n/**\n * @phpstan-impure database\n */\nfunction f(string $s): string { return strtolower($s); }\n";
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.enumerated, 1);
    assert_eq!(only_reason(&report), REASON_EXISTING_TAG_UNREADABLE);
    assert!(report.plan.is_empty());
}

#[test]
fn an_unreadable_class_level_tag_is_never_overwritten() {
    let lib = concat!(
        "<?php\n",
        "/**\n",
        " * @phpstan-all-methods-impure database\n",
        " */\n",
        "class C {\n",
        "    public function get(): int { return 1; }\n",
        "}\n",
    );
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(only_reason(&report), REASON_EXISTING_TAG_UNREADABLE);
    assert_eq!(report.plan.apply_file("lib.php", lib), lib, "the file must be byte-identical");
}

/// Rule 3: an inert method tag doesn't block the class write (nearest-wins on
/// read keeps it truthful); the method site is still refused.
#[test]
fn an_inert_method_tag_does_not_block_the_class_tag() {
    let lib = concat!(
        "<?php\n",
        "class C {\n",
        "    /**\n",
        "     * @phpstan-impure database\n",
        "     */\n",
        "    public function get(): int { return 1; }\n",
        "    public function other(): int { return 2; }\n",
        "}\n",
    );
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    // Two sites: the class (written) and the prose-tagged method (refused).
    assert_eq!(report.oracle.enumerated, 2, "{:#?}", report.refusals);
    assert_eq!(report.oracle.transformed, 1, "{:#?}", report.refusals);
    assert_eq!(only_reason(&report), REASON_EXISTING_TAG_UNREADABLE);

    let out = report.plan.apply_file("lib.php", lib);
    assert!(
        out.starts_with("<?php\n/**\n * @phpstan-all-methods-pure\n */\nclass C {"),
        "the class tag is still written:\n{out}"
    );
    assert_eq!(
        out.matches("@phpstan-impure database").count(),
        1,
        "the method's own note is untouched:\n{out}"
    );
    assert_eq!(out.matches("@phpstan-").count(), 2, "no third tag appears:\n{out}");
}

/// Emission invariant: never write a bound the next run would read as prose.
/// One hole: an unknown label in a **checked** attribute rides the declared
/// lane into a caller (already reported by `effect.unknown-label`); refuse.
#[test]
fn an_unknown_label_in_the_bound_is_never_written() {
    let lib = concat!(
        "<?php\n",
        "interface Repo {\n",
        "    #[\\Steins\\Effect('database')]\n",
        "    public function load(): string;\n",
        "}\n",
        "function f(Repo $r): string { return $r->load(); }\n",
    );
    let report = plan(&[("lib.php", lib)]);
    assert_oracle_complete(&report);
    assert_eq!(report.oracle.enumerated, 1, "{:#?}", report.refusals);
    assert_eq!(only_reason(&report), REASON_BOUND_LABEL_UNKNOWN);
    assert!(report.plan.is_empty());
}
