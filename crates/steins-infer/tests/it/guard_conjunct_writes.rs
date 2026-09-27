//! Issue #654 — a conjunct's refinement does not outlive a later conjunct's write.
//!
//! PHP evaluates `a && b` left to right. When `b` rebinds a name `a` narrowed,
//! the branch sees the value `b` wrote, and `a`'s refinement describes the one
//! that is gone:
//!
//! ```php
//! $x = null;
//! if ($x === null && ($x = fetch()) !== null) {
//!     $x->foo();   // entered only when fetch() returned a Node
//! }
//! ```
//!
//! The walk used to enter that branch with `$x` "proven null" and report
//! `call.on-null` there. Every spelling that applies a condition to what follows
//! it is pinned below — `if`, `while`, `for`, the `||`/else mirror, `assert()` —
//! and so is the other half of the rule: a conjunct that writes nothing still
//! narrows, in the order the vocabularies apply.

use steins_infer::{CALL_ON_NULL_ID, DEBUG_TYPE_ID, Diagnostic, check};
use steins_syntax::SourceTree;

/// Every `debug.type` dump and `call.on-null` finding a source produces, as
/// `line: id: message`.
fn report(src: &str) -> Vec<String> {
    let tree = SourceTree::parse(src);
    check(&tree, &[], "t.php")
        .into_iter()
        .filter(|d: &Diagnostic| d.id == DEBUG_TYPE_ID || d.id == CALL_ON_NULL_ID)
        .map(|d| format!("{}: {}: {}", d.line, d.id, d.message))
        .collect()
}

/// A function body over the issue's class world: `fetch()` returns `?Node`.
fn body(stmts: &str) -> String {
    format!(
        "<?php\nfinal class Node {{ public function foo(): void {{}} }}\n\
         function fetch(): ?Node {{ return null; }}\n\
         function g(mixed $v): bool {{ return true; }}\n\
         function t(): void {{\n{stmts}\n}}\n"
    )
}

/// The dumped type, when the source's whole output is one dump.
fn one_dump(src: &str) -> String {
    let lines = report(src);
    assert_eq!(lines.len(), 1, "expected exactly one line of output, got {lines:?}");
    lines[0].split_once("dumped type: ").map(|(_, t)| t.to_owned()).expect("a dump")
}

#[test]
fn the_issues_if_enters_its_branch_unrefined_and_reports_nothing() {
    let src = body(
        "$x = null;\n\
         if ($x === null && ($x = fetch()) !== null) {\n\
         \\PHPStan\\dumpType($x);\n\
         $x->foo();\n\
         }",
    );
    assert_eq!(report(&src), vec![format!("8: {DEBUG_TYPE_ID}: dumped type: unknown")]);
}

/// Issue #649 gave a `while` header the same true-side application, and its
/// entry env never runs the `if`'s invalidation step — the reason the fix sits
/// where the two meet, not in `walk_if`.
#[test]
fn the_while_and_for_spellings_report_nothing() {
    let r#while = body(
        "$x = null;\n\
         while ($x === null && ($x = fetch()) !== null) {\n\
         \\PHPStan\\dumpType($x);\n\
         $x->foo();\n\
         }",
    );
    assert_eq!(report(&r#while), vec![format!("8: {DEBUG_TYPE_ID}: dumped type: unknown")]);
    let r#for = body(
        "$x = null;\n\
         for (; $x === null && ($x = fetch()) !== null; ) {\n\
         $x->foo();\n\
         }",
    );
    assert_eq!(report(&r#for), Vec::<String>::new());
}

/// The mirror: `a || b` reaches its else-branch with both false, `b` evaluated
/// after `a`. `$x !== null` failing proved `$x` null — before `b` replaced it.
#[test]
fn the_or_else_mirror_enters_its_else_branch_unrefined() {
    let src = body(
        "$x = null;\n\
         if ($x !== null || ($x = fetch()) === null) {\n\
         } else {\n\
         \\PHPStan\\dumpType($x);\n\
         $x->foo();\n\
         }",
    );
    assert_eq!(report(&src), vec![format!("9: {DEBUG_TYPE_ID}: dumped type: unknown")]);
}

/// The write need not be the operand itself. A bare assignment in truthy
/// position, one nested in a call's argument at any depth, and an increment
/// all rebind.
#[test]
fn a_write_anywhere_in_the_later_conjunct_is_covered() {
    for guard in [
        "$x === null && ($x = fetch())",
        "$x === null && g($x = fetch())",
        "$x === null && g(g($x = fetch()))",
        "$x === null && ($x = fetch()) instanceof Node",
    ] {
        let src = body(&format!("$x = null;\nif ({guard}) {{ \\PHPStan\\dumpType($x); }}"));
        assert_eq!(one_dump(&src), "unknown", "{guard}");
    }
    let src = body("$w = 5;\nif ($w === 5 && $w++ > 1) { \\PHPStan\\dumpType($w); }");
    assert_eq!(one_dump(&src), "unknown");
}

/// A reference parameter is a write the analysis can see when it can resolve
/// the callee: a project function's `&$v`, and a builtin only the mined
/// arginfo table knows takes a reference (`settype` has no out-parameter row).
#[test]
fn a_by_reference_argument_is_a_write() {
    let setr = "<?php\nfunction setr(&$v): bool { $v = 'z'; return true; }\n\
                function t(): void { $x = 5;\n\
                if ($x === 5 && setr($x)) { \\PHPStan\\dumpType($x); } }\n";
    assert_eq!(one_dump(setr), "unknown");
    let settype =
        body("$y = 'a';\nif ($y === 'a' && settype($y, 'int')) { \\PHPStan\\dumpType($y); }");
    assert_eq!(one_dump(&settype), "unknown");
    // `preg_match` writes `$m` and the out-parameter seed then says what it
    // wrote — the refinement from BEFORE the call is what goes, not that one.
    let seeded = body(
        "$m = 5;\nif ($m === 5 && preg_match('/a/', 'a', $m)) { \\PHPStan\\dumpType($m); }",
    );
    assert_eq!(one_dump(&seeded), "list{non-empty-string} (asserted)");
}

/// `assert()` reads as `if (!$expr) throw`, so the code after it is the true
/// side. The statement carries no invalidation set of its own, so the rebound
/// name is forgotten there as well as unrefined.
#[test]
fn the_assert_spelling_forgets_the_rebound_name() {
    let src = body(
        "$z = 'a';\nassert($z === 'a' && ($z = fetch()) !== null);\n\\PHPStan\\dumpType($z);",
    );
    assert_eq!(one_dump(&src), "unknown");
}

/// A `@phpstan-assert-if-true` lane is lifted over the guard's own invalidation
/// and put back afterwards. Over a name the condition rebinds, the lane it
/// would put back is the one the rebinding replaced: `$x` holds a `C` in the
/// branch, and the lane said `A`.
#[test]
fn an_asserted_lane_is_not_put_back_over_a_rebound_name() {
    let src = "<?php\nclass A {} class B extends A {} class C {}\n\
               /** @phpstan-assert-if-true B $v */\n\
               function isB(A $v): bool { return true; }\n\
               function t(A $x, C $y): void {\n\
               if (isB($x) && ($x = $y) instanceof C) { \\PHPStan\\dumpType($x); } }\n";
    assert_eq!(one_dump(src), "unknown");
}

/// The rule's other half: a conjunct that rebinds nothing leaves every earlier
/// refinement in place, and the DR2 type vocabulary still applies before the
/// scalar one (ADR-0052), in either source order.
#[test]
fn a_conjunct_that_writes_nothing_still_narrows() {
    let src = "<?php\nfunction t($s): void {\n\
               if (is_string($s) && $s !== '') { \\PHPStan\\dumpType($s); }\n\
               if ($s !== '' && is_string($s)) { \\PHPStan\\dumpType($s); } }\n";
    assert_eq!(
        report(src),
        vec![
            format!("3: {DEBUG_TYPE_ID}: dumped type: non-empty-string"),
            format!("4: {DEBUG_TYPE_ID}: dumped type: non-empty-string"),
        ]
    );
    // A call handed the name by value writes nothing, and neither does a
    // method call on it — the receiver is not rebound.
    let src = "<?php\nfinal class Node { public function ok(): bool { return true; } }\n\
               function chk(mixed $v): bool { return true; }\n\
               function t($x): void {\n\
               if ($x instanceof Node && $x->ok()) { \\PHPStan\\dumpType($x); }\n\
               if ($x instanceof Node && chk($x)) { \\PHPStan\\dumpType($x); }\n\
               if ($x === 5 && chk($x)) { \\PHPStan\\dumpType($x); } }\n";
    assert_eq!(
        report(src),
        vec![
            format!("5: {DEBUG_TYPE_ID}: dumped type: Node"),
            format!("6: {DEBUG_TYPE_ID}: dumped type: Node"),
            format!("7: {DEBUG_TYPE_ID}: dumped type: 5"),
        ]
    );
    // A write to ANOTHER name masks nothing: `$x`'s refinement stands.
    let src = body(
        "$x = null; $y = 1;\n\
         if ($x === null && ($y = fetch()) !== null) { \\PHPStan\\dumpType($x); }",
    );
    assert_eq!(one_dump(&src), "null");
}
