//! What the ADR-0027 ratchet costs inside a loop body, measured.
//!
//! `while`/`for`/`foreach`/`do-while` lower to [`StmtKind::Opaque`], which models
//! the construct's write/read sets and nothing else: the body is never lowered
//! into a sub-trace, so no statement inside one ever reaches the walk. The price
//! is not paid in narrowing alone — **every** trace-walk finding family is dark
//! in there, including ones that fire on the identical statement one line outside
//! or inside an `if`.
//!
//! The shape that motivated the measurement is the parent-pointer traversal that
//! AST consumers write, where the loop condition is the only thing that types the
//! subject at all:
//!
//! ```php
//! $parent = $node->getAttribute('parent');   // untyped return → unknown
//! while ($parent instanceof Node) {          // the ONLY typing of $parent
//!     if ($parent instanceof ClassMethod) { return false; }
//!     $parent = $parent->getAttribute('parent');
//! }
//! ```
//!
//! Two halves of the ratchet, and this file pins both: the write-set half already
//! works (a fact about a binding the loop cannot touch survives the construct),
//! and the condition half does not exist (the guard in the loop header narrows
//! nothing, because there is no body trace for it to narrow *into*).
//!
//! Nothing here is a false positive — the cost is silence, which is why it was
//! affordable (ADR-0013 zero-FP). It is the missed-true-positive side of the same
//! trade, and the remaining slice of issue #266 ("loops beyond write-sets").
//!
//! [`StmtKind::Opaque`]: steins_syntax::StmtKind::Opaque

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

// ---- The machinery itself is not the gap ----------------------------------

#[test]
fn a_guard_types_an_untyped_return_when_the_guard_is_an_if() {
    // The traversal above, hand-unrolled to one iteration. `unknown` in, `Node`
    // inside the guard, `unknown` again once the untyped call re-seeds it: the
    // narrowing carrier has no trouble with a subject whose seed is untyped.
    let src = format!(
        "<?php
{NODES}
function unrolled(Return_ $node): bool {{
    $parent = $node->getAttribute('parent');
    \\PHPStan\\dumpType($parent);
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
        vec![
            "11: dumped type: unknown".to_owned(),
            "13: dumped type: Node".to_owned(),
            "15: dumped type: unknown".to_owned(),
        ],
        "an `instanceof` guard types an untyped seed — the gap below is not this"
    );
}

#[test]
fn a_fact_survives_a_loop_that_cannot_touch_its_subject() {
    // The write-set half of the ADR-0027 ratchet, working: `$x` is neither
    // written nor read by the loop, so the construct forgets `$c` and leaves the
    // `Node` fact standing across it.
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

// ---- The gap: a loop body has no trace ------------------------------------

#[test]
fn the_loop_condition_types_nothing_inside_the_body() {
    // The motivating shape, verbatim. Every dump OUTSIDE the loop answers; every
    // dump INSIDE it is absent entirely — not `unknown`, not a widened fact, no
    // diagnostic at all, because the body was never lowered into a trace.
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
        \\PHPStan\\dumpType($parent);
    }}
    \\PHPStan\\dumpType($parent);
    return false;
}}
"
    );
    assert_eq!(
        dumps(&src),
        vec!["11: dumped type: unknown".to_owned(), "19: dumped type: unknown".to_owned()],
        "lines 13 and 17 sit inside the body and produce nothing; \
         line 19 shows the fall-through does not carry the negated condition either"
    );
}

#[test]
fn every_loop_form_is_equally_dark() {
    // `for`, `foreach` and `do`/`while` take the same `Opaque` arm as `while`.
    for (label, body) in [
        ("for", "for ($i = 0; $i < 3; $i++) { \\PHPStan\\dumpType($n); }"),
        ("foreach", "foreach ([1, 2] as $i) { \\PHPStan\\dumpType($n); }"),
        ("do-while", "do { \\PHPStan\\dumpType($n); } while (false);"),
    ] {
        let src = format!("<?php\nfunction f(): void {{\n    $n = 5;\n    {body}\n}}\n");
        assert!(dumps(&src).is_empty(), "{label}: a dump inside the body answered");
    }
}

/// One `Order` receiver and one placement slot for the same undefined call, so
/// the four placements differ in nothing but the construct wrapped around it.
/// The call always lands on line 5.
const PLACEMENT: &str = "<?php
class Order {}
function f(int $n, array $xs): void {
    $o = new Order();
    STMT
}
";

#[test]
fn a_true_positive_fires_outside_a_loop_and_is_lost_inside_one() {
    // The same call, in four placements. It is judged at the top level and inside
    // an `if`; inside a `while` or a `foreach` it is never seen. The cost is
    // silence-only — no placement turns it into a wrong answer — but a real
    // defect in loop-carried code goes unreported.
    let placed = |stmt: &str| undefined_method_lines(&PLACEMENT.replace("STMT", stmt));

    assert_eq!(placed("$o->tyop();"), vec![5], "top level: judged");
    assert_eq!(placed("if ($n > 0) { $o->tyop(); }"), vec![5], "`if` body: judged (ADR-0031)");
    assert!(placed("while ($n > 0) { $o->tyop(); $n--; }").is_empty(), "`while` body: never seen");
    assert!(placed("foreach ($xs as $x) { $o->tyop(); }").is_empty(), "`foreach` body: never seen");
}
