//! What a loop body is worth to the walk — the four structured loop forms (issues
//! #649, #653 and #650).
//!
//! Each lowers to a `StmtKind` of its own carrying its body as a sub-trace. The
//! construct's effect on the code after it is unchanged — the same write/read sets
//! an `Opaque` applies — and its body's **entry** env is a separate answer to a
//! separate question: what the loop provably cannot change. That drops `writes`,
//! keeps `reads`, and sweeps the mutable state of every object a kept name refers
//! to. What the header then does to that env is the one thing the forms disagree
//! about: a `while` and a `for` narrow by a condition evaluated before every entry,
//! a `foreach` header binds rather than tests, and a `do`-`while` narrows **nothing**
//! — its first iteration runs before its condition is ever evaluated.
//!
//! The shape that motivated the slice types its subject entirely from the loop
//! header, which is what a body-less construct could not use:
//!
//! ```php
//! $parent = $node->getAttribute('parent');   // untyped return -> unknown
//! while ($parent instanceof Node) {          // the ONLY typing of $parent
//!     if ($parent instanceof ClassMethod) { return false; }
//!     $parent = $parent->getAttribute('parent');
//! }
//! ```
//!
//! Two properties carry the entry env's soundness, and both are pinned below: the
//! env is iteration-count-agnostic (every name the loop can rebind is forgotten
//! and every object it can mutate is swept before the header applies, so a body
//! that reassigns the subject it was narrowed on is no obstacle), and nothing the
//! body computes escapes it.

use steins_infer::{
    CALL_ON_NULL_ID, CALL_UNDEFINED_METHOD_ID, DEBUG_TYPE_ID, Diagnostic, Folder, check, check_with,
};
use steins_syntax::SourceTree;

/// Every `debug.type` dump a source produces, as `line: rendered fact`.
fn dumps(src: &str) -> Vec<String> {
    let tree = SourceTree::parse(src);
    check(&tree, &[], "t.php")
        .into_iter()
        .filter(|d: &Diagnostic| d.id == DEBUG_TYPE_ID)
        .map(|d| format!("{}: {}", d.line, d.message))
        .collect()
}

/// A boot surface that answers every absence-ladder leg, so
/// `call.undefined-method` is free to speak (its own tests carry the leg matrix).
struct Boot;

impl Folder for Boot {
    fn fold(
        &mut self,
        _name: &str,
        _args: &[steins_syntax::ArgValue],
        _strict: bool,
    ) -> Option<steins_syntax::ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        true
    }
    fn boot_surface_class_like(&mut self, _fqn: &str) -> Option<bool> {
        Some(false)
    }
}

/// The lines carrying a `call.undefined-method` finding.
fn undefined_method_lines(src: &str) -> Vec<u32> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "t.php", &mut Boot)
        .into_iter()
        .filter(|d| d.id == CALL_UNDEFINED_METHOD_ID)
        .map(|d| d.line)
        .collect()
}

/// A parent-pointer class world: `getAttribute()` is deliberately untyped, so the
/// only fact about what it returns is the one a guard establishes.
const NODES: &str = "\
class Node { public function getAttribute(string $key) { return null; } }
class Stmt_ extends Node {}
class Expr_ extends Node {}
class Return_ extends Stmt_ {}
class ClassMethod extends Stmt_ {}
class Closure_ extends Expr_ {}
";

// ---- The header types the body ---------------------------------------------

#[test]
fn the_header_guard_types_an_untyped_seed_inside_the_body() {
    // The motivating shape, verbatim. `$parent` enters the loop as `unknown` — the
    // return type `getAttribute()` does not have — and is `Node` at the top of the
    // body on the strength of the header alone. The body's last statement reassigns
    // it through the same untyped call, which is precisely the case a fixpoint
    // would be needed for if the entry env were the previous iteration's exit. It
    // is not: it is the post-forget env, so the header re-derives the fact every
    // time.
    let src = format!(
        "<?php
{NODES}
function traverse(Return_ $node): bool {{
    $parent = $node->getAttribute('parent');
    \\PHPStan\\dumpType($parent);
    while ($parent instanceof Node) {{
        \\PHPStan\\dumpType($parent);
        if ($parent instanceof ClassMethod) {{ return false; }}
        if ($parent instanceof Closure_) {{ return true; }}
        $parent = $parent->getAttribute('parent');
    }}
    \\PHPStan\\dumpType($parent);
    return false;
}}
"
    );
    assert_eq!(
        dumps(&src),
        vec![
            "11: dumped type: unknown".to_owned(),
            "13: dumped type: Node".to_owned(),
            "18: dumped type: unknown".to_owned(),
        ],
        "line 13 is inside the body; line 18 shows the fall-through unchanged \
         (carrying the negated condition out is issue #651)"
    );
}

#[test]
fn the_same_guard_as_an_if_answers_the_same_thing() {
    // The traversal hand-unrolled to one iteration. The `while` above must agree
    // with this, because it is the same carrier applied for the same reason — the
    // condition held immediately before the code that reads it.
    let src = format!(
        "<?php
{NODES}
function unrolled(Return_ $node): bool {{
    $parent = $node->getAttribute('parent');
    if ($parent instanceof Node) {{
        \\PHPStan\\dumpType($parent);
        $parent = $parent->getAttribute('parent');
        \\PHPStan\\dumpType($parent);
    }}
    return false;
}}
"
    );
    assert_eq!(
        dumps(&src),
        vec!["12: dumped type: Node".to_owned(), "14: dumped type: unknown".to_owned()],
        "the `if` twin of the shape above"
    );
}

#[test]
fn a_guarded_break_or_continue_does_not_erase_the_body() {
    // `break`/`continue` end the block they sit in, so the branch holding one
    // contributes nothing to its `if`'s join — the same thing `return` means, and
    // the reason they lower to `StmtKind::LoopJump` rather than `Barrier`. As a
    // `Barrier` they fell through with a *cleared* env, so a guarded jump handed
    // the join an empty env and the rest of the body knew nothing. Cost-free while
    // loop bodies were unwalked; the first thing a real body hits now.
    let after_a_guarded = |jump: &str| {
        let src = format!(
            "<?php
{NODES}
function f(Node $node): void {{
    $x = $node->getAttribute('parent');
    while ($x instanceof Node) {{
        if ($x instanceof ClassMethod) {{ {jump} }}
        \\PHPStan\\dumpType($x);
        $x = $x->getAttribute('parent');
    }}
}}
"
        );
        dumps(&src)
    };
    for jump in ["break;", "continue;", "return;"] {
        assert_eq!(
            after_a_guarded(jump),
            vec!["13: dumped type: Node".to_owned()],
            "`{jump}` must leave the header's narrowing standing for the rest of the body"
        );
    }
}

#[test]
fn the_statements_after_a_break_are_unreachable() {
    // The other half of the same terminator: a `break` ends the block, so what
    // follows it there is not walked at all.
    let src = "<?php
function f(int $n): void {
    while ($n > 0) {
        break;
        \\PHPStan\\dumpType($n);
    }
}
";
    assert!(dumps(src).is_empty(), "a statement after `break` was walked: {:#?}", dumps(src));
}

#[test]
fn a_switch_case_still_ends_on_its_own_break() {
    // The regression guard for the terminator change: a trailing `break` in a
    // `switch` case is stripped before the arm body is lowered, so nothing here
    // reaches `LoopJump` and the arms join exactly as they did.
    let src = "<?php
function f(string $k): void {
    $v = 'abc';
    switch ($k) {
        case 'a': \\PHPStan\\dumpType($v); break;
        default: \\PHPStan\\dumpType($v); break;
    }
    \\PHPStan\\dumpType($v);
}
";
    assert_eq!(
        dumps(src),
        vec![
            "5: dumped type: 'abc'".to_owned(),
            "6: dumped type: 'abc'".to_owned(),
            "8: dumped type: 'abc'".to_owned(),
        ],
        "the switch arms and their join are untouched"
    );
}

#[test]
fn a_true_positive_inside_a_while_body_is_reported() {
    // The finding that was lost inside a loop. It fires at the top level, inside an
    // `if`, and now inside a `while` body; a `foreach` body is still out of reach
    // (issue #650). One report per site, never two — the env-free direct pass owns
    // its own families in there and must not double up behind the walk.
    let placed = |stmt: &str| undefined_method_lines(&PLACEMENT.replace("STMT", stmt));

    assert_eq!(placed("(new Order())->tyop();"), vec![4], "top level");
    assert_eq!(placed("if ($n > 0) { (new Order())->tyop(); }"), vec![4], "`if` body (ADR-0031)");
    assert_eq!(placed("while ($n > 0) { (new Order())->tyop(); $n--; }"), vec![4], "`while` body");
    assert_eq!(
        placed("for ($i = 0; $i < $n; $i++) { (new Order())->tyop(); }"),
        vec![4],
        "`for` body (issue #650)"
    );
    assert_eq!(
        placed("foreach ($xs as $x) { (new Order())->tyop(); }"),
        vec![4],
        "`foreach` body (issue #650)"
    );
    assert_eq!(
        placed("do { (new Order())->tyop(); } while ($n > 0);"),
        vec![4],
        "`do`-`while` body (issue #650)"
    );
}

/// One placement slot for the same undefined call, so the placements differ in
/// nothing but the construct wrapped around the statement. The call is on line 4.
const PLACEMENT: &str = "<?php
final class Order {}
function f(Order $o, int $n, array $xs): void {
    STMT
}
";

// ---- The body's env is entered, never left ---------------------------------

#[test]
fn the_entry_env_is_not_the_previous_iteration_s_exit() {
    // What makes the entry env sound without a fixpoint: the body's own assignment
    // is invisible at the top of the body, because the loop's write set was
    // forgotten before the header applied. If the entry env were carried from the
    // body's exit, `$s` would read `'abc'` here — on every iteration but the first.
    let src = "<?php
function f(int $n): void {
    while ($n > 0) {
        \\PHPStan\\dumpType($s);
        $s = 'abc';
        $n--;
    }
}
";
    assert_eq!(
        dumps(src),
        vec!["4: dumped type: unknown".to_owned()],
        "the body's own write is not visible at its entry"
    );
}

#[test]
fn nothing_the_body_computes_escapes_the_construct() {
    // The construct's effect on the code after it is what the sets alone leave
    // standing — a `while` is not a way to publish a fact to its successor.
    let src = "<?php
function f(int $n): void {
    while ($n > 0) {
        $s = 'abc';
        $n--;
    }
    \\PHPStan\\dumpType($s);
    \\PHPStan\\dumpType($n);
}
";
    assert_eq!(
        dumps(src),
        vec!["7: dumped type: unknown".to_owned(), "8: dumped type: unknown".to_owned()],
        "the body's exit env is discarded; the loop's own writes stay forgotten"
    );
}

#[test]
fn a_fact_survives_a_loop_that_cannot_touch_its_subject() {
    // The write-set half of the ADR-0027 ratchet, unchanged by any of this: `$x` is
    // neither written nor read by the loop, so the construct forgets `$c` and
    // leaves the `Node` fact standing across it.
    let src = format!(
        "<?php
{NODES}
function survives(Node $node, int $c): void {{
    $x = $node->getAttribute('parent');
    if ($x instanceof Node) {{
        \\PHPStan\\dumpType($x);
        while ($c > 0) {{ $c--; }}
        \\PHPStan\\dumpType($x);
    }}
}}
"
    );
    assert_eq!(
        dumps(&src),
        vec!["12: dumped type: Node".to_owned(), "14: dumped type: Node".to_owned()],
        "a binding the loop cannot touch keeps its fact across the construct"
    );
}

#[test]
fn a_header_decided_false_leaves_its_body_unwalked() {
    // A body that runs zero times contributes no findings. The region is not marked
    // dead — withdrawing what the env-free direct pass already reports in there is
    // a separate judgment from adding what the walk now reports.
    //
    // The `while (true)` sibling is the positive control: the two differ in the
    // header alone, so a silent `while (false)` is the skip and not some other
    // reason the body went quiet.
    let body = |header: &str| {
        format!(
            "<?php
function f(): void {{
    $s = 'abc';
    while ({header}) {{
        \\PHPStan\\dumpType($s);
    }}
}}
"
        )
    };
    assert!(dumps(&body("false")).is_empty(), "a zero-iteration body is not walked");
    assert_eq!(
        dumps(&body("true")),
        vec!["5: dumped type: unknown".to_owned()],
        "the same body under a live header IS walked — the dump answers"
    );
}

// ---- What the entry env keeps ----------------------------------------------

#[test]
fn a_name_the_loop_cannot_change_keeps_its_value_at_the_body_entry() {
    // The `reads` half of the ADR-0027 sets (issue #653). Nothing in the loop
    // assigns `$s` and no call in it can rebind it, so its binding is the same on
    // iteration 40 as on iteration 1 and the entry env keeps it. The construct's
    // FALL-THROUGH still forgets it, which is the separate judgment: a construct
    // that reads and branches may have early-returned, so the tail must exclude
    // the value even where the body may not.
    //
    // The dump is spelled through a cast because `dumpType($s)` would hand `$s` to
    // a call and put it in `writes` — the one thing this test must not do.
    let src = "<?php
function f(int $n): void {
    $s = 'abc';
    while ($n > 0) {
        \\PHPStan\\dumpType((string) $s);
        $n--;
    }
    \\PHPStan\\dumpType((string) $s);
}
";
    assert_eq!(
        dumps(src),
        vec!["5: dumped type: 'abc'".to_owned(), "8: dumped type: string".to_owned()],
        "line 5 is the body entry, line 8 the unchanged fall-through — `string` there \
         is the cast's own total floor, all that is left once the sets have run"
    );
}

#[test]
fn a_receiver_answers_inside_the_body_as_it_does_inside_an_if() {
    // The measured cost of the old rule, and the acceptance criterion of the new
    // one. A receiver is not an argument, so `$o` lands in `reads` — and forgetting
    // `reads` meant the same statement, with the same proof available, was named
    // inside an `if` and silent inside a `while`.
    let receiver = |wrap: &str| PLACEMENT.replace("STMT", wrap);
    assert_eq!(
        undefined_method_lines(&receiver("if ($n > 0) { $o->tyop(); }")),
        vec![4],
        "`if` body: the parameter's own class answers"
    );
    assert_eq!(
        undefined_method_lines(&receiver("while ($n > 0) { $o->tyop(); $n--; }")),
        vec![4],
        "`while` body: the same receiver, the same answer"
    );
}

#[test]
fn a_subtractive_header_narrows_a_read_only_subject() {
    // What the old entry env cost the guard vocabulary. `instanceof` and the `is_*`
    // family MINT a fact and so narrowed a loop header even from nothing; `!== null`
    // and truthiness SUBTRACT from a declared lane, and a forgotten lane leaves them
    // nothing to subtract from. Both spellings must now read the same inside the
    // loop as inside the `if` twin.
    let subject = |stmt: &str| {
        let src = format!(
            "<?php
final class Node {{}}
function f(?Node $x): void {{
    {stmt}
}}
"
        );
        undefined_method_lines(&src)
    };
    assert_eq!(subject("if ($x !== null) { $x->tyop(); }"), vec![4], "`if`, `!== null`");
    assert_eq!(subject("while ($x !== null) { $x->tyop(); }"), vec![4], "`while`, `!== null`");
    assert_eq!(subject("if ($x) { $x->tyop(); }"), vec![4], "`if`, truthiness");
    assert_eq!(subject("while ($x) { $x->tyop(); }"), vec![4], "`while`, truthiness");
}

#[test]
fn a_kept_object_s_mutable_state_is_swept_at_every_entry() {
    // The part of #653 that is NOT a simple keep. The binding survives, because the
    // loop cannot rebind it; the object's non-readonly properties do not, because a
    // method call the body makes writes through a receiver that never enters
    // `writes` — so iteration 2 would otherwise read iteration 1's `$seen`.
    //
    // One fixture pins both halves. The `call.undefined-method` says `$c` is bound
    // and classed at the body entry, so the silence beside it is a sweep and not a
    // forgotten binding; the dump says the property is gone. The `if` twin, whose
    // properties are not swept, answers `7` at the same position.
    let placed = |construct: &str| {
        format!(
            "<?php
final class Counter {{
    public int $seen = 0;
    public function bump(): void {{ $this->seen = $this->seen + 1; }}
}}
function f(int $n): void {{
    $c = new Counter();
    $c->seen = 7;
    {construct}
}}
"
        )
    };
    let in_if = placed("if ($n > 0) { \\PHPStan\\dumpType($c->seen); $c->bump(); }");
    let in_while =
        placed("while ($n > 0) { \\PHPStan\\dumpType($c->seen); $c->tyop(); $c->bump(); $n--; }");

    assert_eq!(
        dumps(&in_if),
        vec!["9: dumped type: 7".to_owned()],
        "the `if` reads the property the statement before it wrote"
    );
    assert_eq!(
        dumps(&in_while),
        vec!["9: dumped type: unknown".to_owned()],
        "the loop body reads nothing about it — `bump()` runs before the next entry"
    );
    assert_eq!(
        undefined_method_lines(&in_while),
        vec![9],
        "…and the receiver whose property was swept is still bound and classed"
    );
}

#[test]
fn a_poisoned_loop_keeps_nothing_at_its_body_entry() {
    // `poisons` is untouched by any of this: a subtree that `extract`s, `eval`s,
    // aliases or captures by reference has no binding worth carrying anywhere, and
    // the entry env clears exactly as the fall-through does. The same loop without
    // the `extract` is the positive control, two tests above.
    let src = "<?php
function f(int $n, array $a): void {
    $s = 'abc';
    while ($n > 0) {
        extract($a);
        \\PHPStan\\dumpType((string) $s);
        $n--;
    }
}
";
    assert_eq!(
        dumps(src),
        vec!["6: dumped type: string".to_owned()],
        "a poisoned subtree leaves the entry env nothing but the cast's own floor"
    );
}

// ---- What the entry env still costs ----------------------------------------

#[test]
fn a_name_the_body_hands_to_a_call_still_arrives_unknown() {
    // The `writes` half stays exactly as strong as it was: every variable handed to
    // any call in the subtree is in it, whether or not the callee takes it by
    // reference, so `$s` arrives unknown here even though nothing can have changed
    // it. Recovering these is ADR-0070's by-value survivor rule applied to a
    // construct's sets rather than a statement's, which moves every `Opaque`'s
    // fall-through too.
    let src = "<?php
function f(int $n): void {
    $s = 'abc';
    while ($n > 0) {
        \\PHPStan\\dumpType($s);
        $n--;
    }
}
";
    assert_eq!(
        dumps(src),
        vec!["5: dumped type: unknown".to_owned()],
        "a call argument inside the body: the `writes` half took it"
    );
}

// ---- The other three forms (issue #650) ------------------------------------

#[test]
fn every_loop_form_s_body_is_walked() {
    // The row this file used to pin as silence. A dump inside one of these bodies
    // produced no diagnostic at all — not `unknown`, nothing — because the walk
    // never entered the construct. Each now answers.
    for (label, body) in [
        ("for", "for ($i = 0; $i < 3; $i++) { \\PHPStan\\dumpType($n); }"),
        ("foreach", "foreach ([1, 2] as $i) { \\PHPStan\\dumpType($n); }"),
        ("do-while", "do { \\PHPStan\\dumpType($n); } while (false);"),
    ] {
        let src = format!("<?php\nfunction f(): void {{\n    $n = 5;\n    {body}\n}}\n");
        assert_eq!(
            dumps(&src),
            vec!["4: dumped type: unknown".to_owned()],
            "{label}: the body's dump must answer (`unknown` — the dump hands `$n` to a \
             call, so the loop's own `writes` took it)"
        );
    }
}

#[test]
fn a_for_header_condition_narrows_the_body_entry() {
    // The `while` slice's motivating shape, spelled as the `for` nearly every
    // traversal actually is: the seed in `init`, the test in `cond`, the step in the
    // increment. The body entry is the same answer to the same question — `$parent`
    // is written by `init` AND by the increment, so it is forgotten there, and the
    // header alone types it.
    let src = format!(
        "<?php
{NODES}
function traverse(Return_ $node): bool {{
    for ($p = $node->getAttribute('parent'); $p instanceof Node; $p = $p->getAttribute('parent')) {{
        \\PHPStan\\dumpType($p);
        if ($p instanceof ClassMethod) {{ return false; }}
    }}
    \\PHPStan\\dumpType($p);
    return false;
}}
"
    );
    assert_eq!(
        dumps(&src),
        vec!["11: dumped type: Node".to_owned(), "14: dumped type: unknown".to_owned()],
        "line 11 is the body entry, line 14 the unchanged fall-through"
    );
}

#[test]
fn a_for_init_write_reaches_the_condition_and_a_step_write_does_not() {
    // The two halves of the `for` header's own rule, in one pair of fixtures.
    //
    // `init` runs once, before the condition is ever evaluated, so a name it writes
    // that nothing else in the loop writes again holds that value at every entry —
    // and the condition is judged against it. `$x === 2` is decided here, both ways:
    // the body is walked under the header that holds and skipped under the one that
    // cannot. Nothing but the init value differs between the two.
    let decided = |init: &str| {
        let src = format!(
            "<?php
final class Order {{}}
function f(): void {{
    for ($x = {init}; $x === 2; ) {{
        (new Order())->tyop();
    }}
}}
"
        );
        undefined_method_lines(&src)
    };
    assert_eq!(decided("2"), vec![5], "the init value satisfies the header: the body is walked");
    assert!(decided("1").is_empty(), "the init value refutes it: a zero-iteration body");
}

#[test]
fn a_for_step_write_is_forgotten_at_the_body_entry() {
    // The other half. `$i` is written by `init` and by the increment, so it may hold
    // something else on iteration 2 and is forgotten; `$s` is written by `init`
    // alone, so it cannot, and is kept. Both dumps are spelled through a cast, which
    // is what keeps the subject out of the loop's `writes` (see the `while` twin).
    let src = "<?php
function f(): void {
    for ($i = 0, $s = 'abc'; $i < 10; $i++) {
        \\PHPStan\\dumpType((string) $i);
        \\PHPStan\\dumpType((string) $s);
    }
}
";
    assert_eq!(
        dumps(src),
        vec!["4: dumped type: string".to_owned(), "5: dumped type: 'abc'".to_owned()],
        "the increment's target is forgotten (only the cast's own floor is left); the \
         init-only name keeps what init wrote"
    );
}

#[test]
fn a_for_header_tests_its_last_condition_only() {
    // PHP evaluates every comma-separated condition and tests the LAST one. `!$x`
    // over an init-carried `true` is decided false, so the body is skipped; put the
    // undecided `$i < $n` last and the same loop walks. Only the order differs.
    let with_conditions = |conds: &str| {
        let src = format!(
            "<?php
final class Order {{}}
function f(int $n): void {{
    for ($x = true, $i = 0; {conds}; ) {{
        (new Order())->tyop();
    }}
}}
"
        );
        undefined_method_lines(&src)
    };
    assert!(with_conditions("$i < $n, !$x").is_empty(), "the last condition is the tested one, and it refutes");
    assert_eq!(with_conditions("!$x, $i < $n"), vec![5], "reversed, the tested condition is undecided");
}

#[test]
fn an_empty_for_header_walks_its_body_unguarded() {
    // `for (;;)` has no condition to test; it lowers to `CondExpr::Opaque`, which
    // decides nothing and narrows nothing, so the body is walked as-is.
    let src = "<?php
final class Order {}
function f(): void {
    for (;;) {
        (new Order())->tyop();
    }
}
";
    assert_eq!(undefined_method_lines(src), vec![5], "an opaque header is a walked body");
}

#[test]
fn a_write_inside_the_for_condition_is_not_carried() {
    // `carried` is the init-written names nothing else in the loop writes again —
    // and the condition is part of the loop. `$x--` in the header rebinds `$x` on
    // every test, so it is forgotten at the entry and only the cast's floor is left.
    let src = "<?php
function f(): void {
    for ($x = 2; $x-- > 0; ) {
        \\PHPStan\\dumpType((string) $x);
    }
}
";
    assert_eq!(dumps(src), vec!["4: dumped type: string".to_owned()], "a condition write leaves `carried`");
}

#[test]
fn a_foreach_binds_its_key_and_value_defined_but_untyped() {
    // `$k` and `$v` are ordinary members of the construct's `writes` — the
    // `foreach`-binding row `collect_assign_writes` has always had — so the entry
    // forgetting leaves them defined but untyped, and a dump on either answers
    // rather than staying absent. Typing them from the subject's own value type is
    // issue #652 and is deliberately not done here.
    let src = "<?php
function f(array $xs): void {
    foreach ($xs as $k => $v) {
        \\PHPStan\\dumpType($k);
        \\PHPStan\\dumpType($v);
    }
}
";
    assert_eq!(
        dumps(src),
        vec!["4: dumped type: unknown".to_owned(), "5: dumped type: unknown".to_owned()],
        "both loop variables answer"
    );
}

#[test]
fn a_do_while_body_runs_even_under_a_header_that_is_false() {
    // The soundness pin, first half. A `do { … } while (false)` runs its body
    // **exactly once**; the `while` rule reads a `false` header as a zero-iteration
    // loop and leaves the body unwalked. Taking it here would silently drop every
    // finding in the one iteration PHP always runs.
    let src = "<?php
final class Order {}
function f(): void {
    do {
        (new Order())->tyop();
    } while (false);
}
";
    assert_eq!(undefined_method_lines(src), vec![5], "the one iteration is judged");
}

#[test]
fn a_do_while_condition_narrows_nothing_at_the_body_entry() {
    // The soundness pin, second half, and the reason `StmtKind::DoWhile` does not
    // carry its condition at all. The first iteration runs BEFORE the header is ever
    // evaluated, so `$x instanceof Order` is not a fact there: `$x` still holds the
    // `null` the statement above it wrote, `$x->ship()` is a guaranteed runtime
    // `Error`, and the `call.on-null` that says so is a true positive the `while`
    // rule would narrow away.
    //
    // The `while` twin is the control, and is silent for its own right reason: there
    // the header IS evaluated first, decides `No` against the same `null`, and the
    // body never runs. The two differ in nothing but the loop form.
    let form = |src: &str| {
        let tree = SourceTree::parse(src);
        check(&tree, &[], "t.php")
            .into_iter()
            .filter(|d: &Diagnostic| d.id == CALL_ON_NULL_ID)
            .map(|d| d.line)
            .collect::<Vec<_>>()
    };
    let do_while = "<?php
final class Order { public function ship(): void {} }
function f(): void {
    $x = null;
    do {
        $x->ship();
    } while ($x instanceof Order);
}
";
    let while_twin = "<?php
final class Order { public function ship(): void {} }
function f(): void {
    $x = null;
    while ($x instanceof Order) {
        $x->ship();
    }
}
";
    assert_eq!(form(do_while), vec![6], "the first iteration's `$x` is still the proven null");
    assert!(form(while_twin).is_empty(), "the `while` twin never enters the body at all");
}

#[test]
fn a_receiver_answers_inside_every_loop_form() {
    // The `reads` half (issue #653) reaches all four forms: a receiver is not an
    // argument, so `$o` stays bound and classed at each body's entry and the same
    // statement is named wherever it is written.
    let placed = |wrap: &str| undefined_method_lines(&PLACEMENT.replace("STMT", wrap));
    assert_eq!(placed("if ($n > 0) { $o->tyop(); }"), vec![4], "`if` body");
    assert_eq!(placed("while ($n > 0) { $o->tyop(); $n--; }"), vec![4], "`while` body");
    assert_eq!(placed("for ($i = 0; $i < $n; $i++) { $o->tyop(); }"), vec![4], "`for` body");
    assert_eq!(placed("foreach ($xs as $x) { $o->tyop(); }"), vec![4], "`foreach` body");
    assert_eq!(placed("do { $o->tyop(); } while ($n > 0);"), vec![4], "`do`-`while` body");
}
