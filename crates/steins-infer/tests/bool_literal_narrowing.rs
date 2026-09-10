//! ADR-0093 §2 — `!== false` / `!== true` in the **value lane**, the mirror of the
//! arm-lane rule of issue #443.
//!
//! `bool` has two inhabitants, so the guard is a set difference over a two-point
//! domain and the domain can now spell the result: `bool` minus `false` is `true`
//! on a single base, and one union arm's member set on a mixed one. What the issue
//! (#600) asked for is the first test; the rest are the shapes around it —
//! polarity, the arm that disappears rather than widening back, the positive branch
//! that stays keep-only, and the truthiness the literal decides.
//!
//! NB: a call invalidates its argument after the statement (by-ref conservatism),
//! so each fixture dumps its binding once per branch, never before the guard.

use steins_infer::{DEBUG_TYPE_ID, check};
use steins_syntax::SourceTree;

/// Every `debug.type` message body a source produces, in source order.
fn dumps(src: &str) -> Vec<String> {
    let tree = SourceTree::parse(src);
    check(&tree, &[], "t.php")
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message.replace("dumped type: ", ""))
        .collect()
}

/// What `$v` dumps after `assert(<guard>)`, declared as `decl` in a docblock.
fn after_assert(decl: &str, guard: &str) -> String {
    let src = format!(
        "<?php\n/** @param {decl} $v */\nfunction f($v): void {{\n\
         assert({guard});\n\\PHPStan\\dumpType($v);\n}}\n"
    );
    let got = dumps(&src);
    assert_eq!(got.len(), 1, "one dump per fixture: {got:?}");
    got.into_iter().next().expect("len checked")
}

#[test]
fn the_issue_fixture_dumps_string_or_true() {
    // Issue #600 verbatim, on a native union: the bool arm keeps `true` instead of
    // surviving whole, which is the whole of what the value lane could not spell.
    let src = "<?php\nfunction f(string|bool $v): void {\n\
               assert($v !== false);\n\\PHPStan\\dumpType($v);\n}\n";
    assert_eq!(dumps(src), vec!["string|true".to_owned()]);
}

#[test]
fn both_polarities_subtract_their_own_literal() {
    assert_eq!(after_assert("string|bool", "$v !== false"), "string|true (asserted)");
    assert_eq!(after_assert("string|bool", "$v !== true"), "string|false (asserted)");
    // One base, and the finite layer says the survivor exactly.
    assert_eq!(after_assert("bool", "$v !== false"), "true (asserted)");
    assert_eq!(after_assert("bool", "$v !== true"), "false (asserted)");
}

#[test]
fn subtracting_an_arms_own_literal_removes_the_arm() {
    // The measured fp-gate shape (`assert($stdout !== false)` on a `T|false`): the
    // arm is already one inhabitant, so the guard empties its member set and the
    // arm is gone — never widened back to `bool`.
    assert_eq!(after_assert("string|false", "$v !== false"), "string (asserted)");
    // The literal the guard does not name is untouched.
    assert_eq!(after_assert("string|false", "$v !== true"), "string|false (asserted)");
    assert_eq!(after_assert("int|false", "$v !== false"), "int (asserted)");
}

#[test]
fn the_positive_branch_stays_keep_only() {
    // `=== false` proves the value on the true path and leaves the residue on the
    // false one: `Refine::Exact` as before, and the false path is the subtraction.
    let src = "<?php\n/** @param string|bool $v */\nfunction f($v): void {\n\
               if ($v === false) { \\PHPStan\\dumpType($v); } else { \\PHPStan\\dumpType($v); }\n}\n";
    assert_eq!(dumps(src), vec!["false".to_owned(), "string|true (asserted)".to_owned()]);
}

#[test]
fn a_surviving_literal_arm_is_carried_through_the_next_guard() {
    // The result is a fact like any other, not a one-shot spelling: a truthiness
    // test after the subtraction keeps `string|true` (its bool half cannot be the
    // falsy inhabitant any more), where the same test on `string|false` leaves
    // the string alone.
    let kept = "<?php\n/** @param string|true $v */\nfunction f($v): void {\n\
                if ($v) { \\PHPStan\\dumpType($v); }\n}\n";
    assert_eq!(dumps(kept), vec!["string|true (asserted)".to_owned()]);
    let dropped = "<?php\n/** @param string|false $v */\nfunction f($v): void {\n\
                   if ($v) { \\PHPStan\\dumpType($v); }\n}\n";
    assert_eq!(dumps(dropped), vec!["string (asserted)".to_owned()]);
}

#[test]
fn both_literals_subtracted_leave_the_emptied_lane() {
    // The empty member set is an arm that is not there, and a position with
    // nothing left is the emptied lane (ADR-0052's emptied-`Verified` rule, which
    // needs the native declaration — a docblock lane is `Asserted` and floors to
    // `unknown` instead) — never a silent return to `bool`.
    let src = "<?php\nfunction f(bool $v): void {\n\
               assert($v !== false && $v !== true);\n\\PHPStan\\dumpType($v);\n}\n";
    assert_eq!(dumps(src), vec!["*NEVER*".to_owned()]);
}
