//! What a loop body is worth to the walk — the structured `while` (issues #649
//! and #653) and the three forms still behind the ADR-0027 ratchet.
//!
//! A `while` lowers to `StmtKind::While`, which carries its condition and its body
//! as a sub-trace. The construct's effect on the code after it is unchanged — the
//! same write/read sets an `Opaque` applies — and its body's **entry** env is a
//! separate answer to a separate question: what the loop provably cannot change.
//! That drops `writes`, keeps `reads`, sweeps the mutable state of every object a
//! kept name refers to, and is then narrowed by the header. `for`, `foreach` and
//! `do`/`while` still lower to `StmtKind::Opaque`, whose body no statement of the
//! walk ever reaches (issue #650), so this file is where the two states are told
//! apart.
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

use steins_infer::{CALL_UNDEFINED_METHOD_ID, DEBUG_TYPE_ID, Diagnostic, Folder, check, check_with};
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
    assert!(
        placed("foreach ($xs as $x) { (new Order())->tyop(); }").is_empty(),
        "`foreach` body: still never seen"
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

// ---- The forms still behind the ratchet ------------------------------------

#[test]
fn for_foreach_and_do_while_bodies_are_still_dark() {
    // Issue #650. Each still lowers to `StmtKind::Opaque`, so a dump inside one
    // produces no diagnostic at all — not `unknown`, nothing.
    for (label, body) in [
        ("for", "for ($i = 0; $i < 3; $i++) { \\PHPStan\\dumpType($n); }"),
        ("foreach", "foreach ([1, 2] as $i) { \\PHPStan\\dumpType($n); }"),
        ("do-while", "do { \\PHPStan\\dumpType($n); } while (false);"),
    ] {
        let src = format!("<?php\nfunction f(): void {{\n    $n = 5;\n    {body}\n}}\n");
        assert!(dumps(&src).is_empty(), "{label}: a dump inside the body answered");
    }
}
