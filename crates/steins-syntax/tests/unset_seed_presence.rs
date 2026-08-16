//! `SourceTree::unset_seed_facts` (ADR-0087 §4/§8, issue #396): the lowering half
//! of `phpdoc.maybe-undefined`.
//!
//! What this crate can decide alone is *presence*, not vocabulary — it has no edge
//! to the phpdoc lowering, so its seeds are a syntactic **superset** and the reads it
//! answers are candidates. These tests pin the superset's two properties: it never
//! misses a declaration (the `unset` spelling is the only route to the leaf), and it
//! costs nothing on a file that does not spell the word. The semantic half — which
//! candidate is really a `T|unset` — is pinned in
//! `crates/steins-infer/tests/unset_maybe_undefined.rs`.

use steins_syntax::SourceTree;

fn candidates(src: &str) -> Vec<(String, u32)> {
    let tree = SourceTree::parse(src);
    tree.unset_seed_facts()
        .reads
        .iter()
        .map(|r| (r.name.clone(), tree.position(r.span.start).line))
        .collect()
}

#[test]
fn a_file_that_never_spells_the_word_produces_no_candidates() {
    // The gate that keeps the pass off nearly every file: no docblock mentions
    // `unset`, so nothing is walked and nothing is answered.
    assert!(candidates("<?php\n/** @var \\DateTime $x */\necho $x->format('c');\n").is_empty());
    assert!(candidates("<?php\n$a = 1;\nif ($a) { $b = 2; }\necho $b;\n").is_empty());
}

#[test]
fn the_candidate_set_is_a_superset_of_the_declarations() {
    // Every `$name` the docblock spells becomes a candidate, whatever the tag around
    // it says — `steins-infer` drops the ones whose lowering carries no `unset`. Here
    // `$other` is named only in prose and still answers, which is the superset
    // working as designed rather than a defect.
    let found = candidates(
        "<?php\n/**\n * @var \\DateTime|unset $x\n * see also $other, unrelated\n */\necho $x->format('c');\necho $other;\n",
    );
    assert_eq!(found, vec![("x".to_owned(), 6), ("other".to_owned(), 7)], "{found:?}");
}

#[test]
fn the_word_is_recognized_case_blind_and_anywhere_in_the_docblock() {
    for text in ["\\DateTime|unset", "\\DateTime|UNSET", "\\DateTime|Unset"] {
        let src = format!("<?php\n/** @var {text} $x */\necho $x->format('c');\n");
        assert_eq!(candidates(&src).len(), 1, "`{text}` must reach the pass");
    }
}

#[test]
fn scope_entry_is_bound_so_a_read_before_the_declaration_answers_nothing() {
    let found = candidates("<?php\necho $x;\n/** @var \\DateTime|unset $x */\necho $x;\n");
    assert_eq!(found, vec![("x".to_owned(), 4)], "{found:?}");
}

#[test]
fn a_name_dam_ends_the_pass_at_its_own_offset() {
    let found = candidates(
        "<?php\n/** @var \\DateTime|unset $x */\necho $x;\ninclude 'p.php';\necho $x;\n",
    );
    assert_eq!(found, vec![("x".to_owned(), 3)], "reads before the dam survive it: {found:?}");
}

#[test]
fn a_goto_dams_the_pass_outright() {
    assert!(
        candidates("<?php\n/** @var \\DateTime|unset $x */\ngoto end;\nend:\necho $x;\n")
            .is_empty()
    );
}

#[test]
fn a_declaration_inside_a_function_body_is_not_the_script_scopes() {
    // The pass walks the top-level statement list only, and `scan_var_usage` never
    // descends into a nested scope — so a function's own `@var` cannot seed here.
    assert!(
        candidates(
            "<?php\nfunction f(): string {\n    /** @var \\DateTime|unset $x */\n    return $x->format('c');\n}\n"
        )
        .is_empty()
    );
}

#[test]
fn the_seed_names_the_statement_whose_docblock_declared_it() {
    let src = "<?php\n/** @var \\DateTime|unset $x */\necho $x->format('c');\n";
    let tree = SourceTree::parse(src);
    let read = &tree.unset_seed_facts().reads[0];
    // The confirming reader reaches the docblock back through this offset.
    let doc = tree.stmt_docblock(read.seed_stmt).expect("the adopted docblock");
    assert!(doc.text.contains("unset"), "{}", doc.text);
}

#[test]
fn a_declared_name_passed_to_a_function_is_kept_as_an_out_parameter_candidate() {
    // The ADR-0077 residue: whether `f($x)` writes `$x` needs the cross-file index,
    // so lowering records the position and the checker decides.
    let tree = SourceTree::parse(
        "<?php\n/** @var string|unset $x */\npreg_match('/a/', 'b', $x);\necho $x;\n",
    );
    let facts = tree.unset_seed_facts();
    assert_eq!(facts.ref_arg_candidates.len(), 1, "{facts:#?}");
    assert_eq!(facts.ref_arg_candidates[0].name, "x");
}
