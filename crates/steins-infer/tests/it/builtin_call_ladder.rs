//! Issue #767 — the builtin-call ladder is walked by one function
//! (`builtin_call_rung`), and its rungs 3–5 (the reflected envelope, the
//! resource arms, the declared floor) read nothing of the scope, so the ladder
//! does not refuse them in a poisoned scope (ADR-0046). Whether a poisoned scope
//! silences them is each seam's own posture, and the three seams differ. This
//! pin holds each to its own, so walking the ladder once can never quietly fold
//! the seams into one.
//!
//! Each call site of the ladder is pinned by its own family elsewhere
//! (`declared_return_floor`, `resource_folds`, `operand_builtin_return`); what
//! is pinned here is only where the seams part.

use steins_infer::{DEBUG_TYPE_ID, check};
use steins_syntax::SourceTree;

/// The `\PHPStan\dumpType()` renderings of a function body under `NoFold`, in
/// source order: no engine answers, so `rand()` reaches the ADR-0069 floor.
fn dumps(body: &str) -> Vec<String> {
    let src = format!("<?php\nfunction f(array $a): void {{\n{body}}}\n");
    let tree = SourceTree::parse(&src);
    check(&tree, &[], "t.php")
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message.trim_start_matches("dumped type: ").to_owned())
        .collect()
}

#[test]
fn a_poisoned_scope_silences_the_assignment_and_the_operand_but_not_the_dump() {
    // The assignment binds nothing in a poisoned scope and asks nothing; the
    // operand answers no more than the assignment does, so the cast takes its
    // own floor; the dump asks, and renders what the name declares.
    assert_eq!(
        dumps(
            "extract($a);\n$r = rand();\n\\PHPStan\\dumpType($r);\n\
             \\PHPStan\\dumpType((string) rand());\n\\PHPStan\\dumpType(rand());\n"
        ),
        vec!["unknown".to_owned(), "string".to_owned(), "int (asserted)".to_owned()],
    );
}
