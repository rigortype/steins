//! Steins' syntax-tree contract and its Mago parser backend (ADR-0003).
//!
//! # Encapsulation (hard rule)
//!
//! The pinned Mago fork is a dependency of *this crate only* — **no Mago type
//! appears in this crate's public API**. Everything the analyzer sees is the
//! owned, lowered representation here ([`SourceTree`] and its plain-data structs),
//! the seam ADR-0003 requires so parser backends can be swapped freely. Spans are
//! byte offsets, convertible to 1-based line/column via [`SourceTree::position`].

use mago_span::HasSpan;
use mago_syntax::cst::{
    Access, Argument, ArrayElement, AssignmentOperator, BinaryOperator, Call, Construct, Expression,
    Identifier, Node, Program, Statement, StringPart, Trivia, TriviaKind, UnaryPrefixOperator,
    UseItems, Variable,
};

use lower_expr::{
    assert_stmt_cond, const_key_offset, const_key_offset_path, destructure_reads, lower_arg_value,
    lower_array_key, lower_call, lower_cond, lower_cond_operand, lower_construct_call,
    lower_method_call, lower_opaque, lower_static_call, opaque_sets, prop_fetch_of,
};

mod ast;
mod lower_decl;
mod lower_effect;
mod lower_expr;
mod lower_presence;
mod lower_scope;
pub mod stack_guard;
mod tree;

pub use ast::*;
pub use lower_expr::php_canonical_int_string;
pub use tree::SourceTree;

/// Append the lowered [`Stmt`] for one source statement (or nothing, for benign
/// statements that neither define values nor disturb them).
fn lower_stmt(s: &Statement<'_>, out: &mut Vec<Stmt>) {
    // A brace block creates no PHP scope: flatten it into the enclosing trace so a
    // branch body `{ return; … }` is lowered statement-by-statement (its `return`
    // is a real terminator, not hidden inside an `Opaque`). This is what makes the
    // structured-`if` branches see their terminators (ADR-0031).
    if let Statement::Block(b) = s {
        for inner in b.statements.iter() {
            lower_stmt(inner, out);
        }
        return;
    }
    // A `match` in value position gets its arms walked, as an entry of its own
    // placed ahead of the statement that consumes its result (issue #430). The
    // consuming statement is lowered below exactly as before — this adds a walk,
    // never a value.
    value_position_matches(s, out);
    let stmt_span = to_span(s.span());
    let stmt = match s {
        // Benign: no effect on local values — keep known values flowing across.
        Statement::OpeningTag(_)
        | Statement::ClosingTag(_)
        | Statement::Inline(_)
        | Statement::Noop(_)
        | Statement::Use(_) => return,
        Statement::Expression(es) => lower_expr_stmt(es.expression),
        Statement::Return(r) => {
            let value = r.value.map_or(ArgValue::Other, lower_arg_value);
            let mut invalidated = Vec::new();
            let mut call = None;
            // Point the diagnostic at the returned value, else the `return` word.
            let span = r.value.map_or_else(|| to_span(r.span()), |e| to_span(e.span()));
            if let Some(e) = r.value {
                invalidated = call_invalidation(&Node::Expression(e));
                // `return f($s);` — carry the call so propagation/descent reach it.
                call = named_call(e);
            }
            Stmt::lowered(StmtKind::Return { value, call, span }, invalidated)
        }
        // `echo e1, e2, …;` — collect the statically-named calls among the
        // operands so propagation/descent check them; env stays conservative.
        Statement::Echo(e) => {
            let mut calls = Vec::new();
            // The ADR-0070 evidence is accumulated over the WHOLE echo: every
            // operand feeds the same per-name entries, so `echo trim($x), $o->m($x);`
            // turns `$x` opaque and discards its `trim` site too — the verdict is
            // statement-scoped, never per operand.
            let mut invalidated = Vec::new();
            for v in e.values.iter() {
                scan_invalidated(&Node::Expression(v), &mut invalidated, false);
                // Echo invalidates variables written by embedded assignments
                // (`echo $x = 5;`) or mutable calls (ADR-0031) — and a name this
                // echo WRITES is not a by-value-argument question at all: the
                // write is the reason it is invalidated, no signature can excuse
                // it, so its entry is opaque.
                let mut writes = Vec::new();
                collect_assign_writes(&Node::Expression(v), &mut writes);
                for name in writes {
                    note_occurrence(&mut invalidated, name, None);
                }
                if let Some(c) = named_call(v) {
                    calls.push(c);
                }
            }
            Stmt::lowered(StmtKind::Echo(calls), invalidated)
        }
        // `if`/`elseif`/`else` is structured (ADR-0031): its control flow
        // is modeled, not erased.
        Statement::If(if_stmt) => lower_if(if_stmt),
        // A `switch` is structured (ADR-0031 Part B) when its subject and every
        // case condition lower to a variable/literal AND every non-empty case
        // ends in break/return/throw/exit (no fall-through); else it stays
        // `Opaque` like the loop constructs below.
        Statement::Switch(sw) => lower_switch(sw).unwrap_or_else(|| lower_opaque(s)),
        // Every OTHER control-flow construct stays `Opaque` (ADR-0027 ratchet) —
        // the walk forgets only its write/read set, not the whole env.
        Statement::While(_)
        | Statement::For(_)
        | Statement::Foreach(_)
        | Statement::DoWhile(_)
        | Statement::Try(_) => lower_opaque(s),
        // `unset($var[<lit>]);` — a constant-key offset unset (ADR-0062 A-G8).
        // Barrier semantics plus the base and key, exactly as `OffsetWrite`; a
        // multi-target unset, `unset($var)` itself, and a dynamic key all fall
        // through to the plain barrier below.
        Statement::Unset(u)
            if u.values.len() == 1
                && u.values.iter().next().is_some_and(|v| const_key_offset(v).is_some()) =>
        {
            let (base, key) = u
                .values
                .iter()
                .next()
                .and_then(|v| const_key_offset(v))
                .expect("guarded above");
            Stmt::lowered(StmtKind::OffsetUnset { base, key }, Vec::new())
        }
        // Everything else (declarations, `goto`, labels, `declare`, other unsets,
        // `__halt_compiler`, …) stays a full Barrier: the sound floor for
        // anything whose write set the lowering cannot bound.
        _ => Stmt::lowered(StmtKind::Barrier, Vec::new()),
    };
    out.push(Stmt {
        span: stmt_span,
        string_contexts: string_context_sites(s),
        // The reachability foundation's central fill (ADR-0078, issue #199) — read
        // off the CST statement, never off the lowered `kind`. See `stmt_end`.
        end: stmt_end(s),
        has_terminator: subtree_has_function_exit(&Node::Statement(s)),
        ..stmt
    });
}

// reachability foundation (ADR-0078, issue #199)

/// Where one CST statement leaves control — the per-statement half of the
/// terminality judgment ([`BodyEnd`]), computed here because this is the last
/// place the full construct is in hand.
///
/// # The per-construct table, and why each row answers what it does
///
/// | construct | answer | why |
/// | --- | --- | --- |
/// | `return`, `throw` (expression-statement), `exit`/`die`, `__halt_compiler` | `Terminates` | no edge to the successor at all |
/// | `break`, `continue` | `Terminates` | control leaves *this* statement list; where it lands is the enclosing construct's business, not this list's |
/// | `if` | join of every arm, with a missing `else` joined in as `FallsThrough`; a literal-true condition ends the chain at its own arm and a literal-false one drops it | the implicit empty else IS a terminator-free path to the successor — unless the condition is a literal, where there is no branch at all |
/// | `match` (statement position) | join of every arm, with a missing `default` joined in as `Terminates` | PHP throws `\UnhandledMatchError` on no match — witnessed 8.5.9 |
/// | `switch` | join of every case body, with a missing `default` joined in as `FallsThrough`; `Unknown` when the subtree holds any `break`/`continue`/`goto`, or when a case body runs into the next | a `break` exits the *switch* rather than the list it sits in, and resolving which is which is not this judgment's job; case-to-case fall-through is a real edge it does not model |
/// | `foreach` | `FallsThrough` | the iteration exhausts; see the recorded obstacle below |
/// | `while` / `for` / `do-while` with a provably-true condition and no `break`/`goto` in the subtree | `Terminates` | there is no exit edge to take |
/// | the same with a `break`/`goto` somewhere inside | `Unknown` | the jump's target is not resolved here, so whether *this* loop has an exit edge is undecided |
/// | every other loop | `FallsThrough` | the condition can be false, which is an exit edge |
/// | `try` | `Unknown` | recorded exclusion — see below |
/// | `goto`, a `label:` | `Unknown` | an unbounded jump; a label is an unbounded *incoming* edge, so the tail may be re-entered |
/// | everything else (assignments, calls, `echo`, `global`, `static`, `unset`, declarations, `use`, `declare`, `namespace`) | `FallsThrough` | straight-line |
///
/// # Recorded obstacles — silences this judgment names rather than hides
///
/// * **`try`/`catch`/`finally` is `Unknown`, full stop.** `finally` *overwrites the
///   exit point*: witnessed on 8.5.9, `try { return 1; } finally { return 2; }`
///   returns `2`, and a returning `finally` also swallows an in-flight exception
///   from the `try`. So a `try` whose block and every `catch` terminate can still
///   fall through, and vice versa — undecided until a later slice models `finally`.
/// * **A call to a function proven never to return is not judged here.** A
///   statement-position call answers `FallsThrough` — deciding otherwise needs the
///   project index (does the callee declare `: never`?), and this judgment is
///   deliberately index-free and env-free; `type.return-missing` applies that
///   refinement itself, at the emitter.
/// * **An infinite `Traversable`.** `foreach ($generator as $v)` over a
///   never-ending generator has no exit edge, yet this judgment says
///   `FallsThrough` anyway — bounding it needs the iterator's value, whole-program
///   reasoning the syntactic CFG reading rules out.
fn stmt_end(s: &Statement<'_>) -> BodyEnd {
    match s {
        Statement::Return(_) => BodyEnd::Terminates,
        // `break` / `continue` leave the enclosing statement list. A `switch`
        // case's trailing `break` is stripped before its body is lowered
        // (`strip_trailing_break`), so this row never mis-reads an arm as
        // terminating when it only ends the arm.
        Statement::Break(_) | Statement::Continue(_) => BodyEnd::Terminates,
        Statement::HaltCompiler(_) => BodyEnd::Terminates,
        Statement::Expression(es) => expr_end(es.expression),
        Statement::Block(b) => block_end(b.statements.as_slice()),
        Statement::If(i) => if_end(i),
        Statement::Switch(sw) => switch_end(sw),
        Statement::Foreach(_) => BodyEnd::FallsThrough,
        Statement::While(w) => loop_end(expr_is_true(w.condition), &Node::Statement(s)),
        Statement::DoWhile(d) => loop_end(expr_is_true(d.condition), &Node::Statement(s)),
        // `for (;;)` — no condition at all — is the canonical infinite `for`; a
        // written condition list is infinite when its LAST expression (the one PHP
        // actually tests) is a true literal.
        Statement::For(f) => {
            let infinite = f.conditions.iter().next_back().is_none_or(|c| expr_is_true(c));
            loop_end(infinite, &Node::Statement(s))
        }
        Statement::Try(_) => BodyEnd::Unknown,
        Statement::Goto(_) | Statement::Label(_) => BodyEnd::Unknown,
        _ => BodyEnd::FallsThrough,
    }
}

/// [`body_end`] over a borrowed CST statement list — the same fold, one level
/// earlier. Kept separate from [`body_end`] (which reads lowered [`Stmt`]s) because
/// a branch body is judged here *before* it is lowered, and the two must agree by
/// sharing this shape rather than by coincidence.
fn block_end(statements: &[Statement<'_>]) -> BodyEnd {
    let mut undecided = false;
    for s in statements {
        match stmt_end(s) {
            BodyEnd::Terminates => return BodyEnd::Terminates,
            BodyEnd::Unknown => undecided = true,
            BodyEnd::FallsThrough => {}
        }
    }
    if undecided { BodyEnd::Unknown } else { BodyEnd::FallsThrough }
}

/// An `if`'s terminality: the join over its arms, with the **implicit empty
/// `else`** joined in as [`BodyEnd::FallsThrough`] when no `else` is written —
/// why `if ($c) { return 1; }` is reported by `type.return-missing` and
/// `if ($c) { return 1; } else { return 2; }` is not.
///
/// # The one place a condition is read
///
/// Branch conditions are otherwise non-deterministic here (see [`stmt_end`]), but a
/// **literal** one is not a branch at all: `if (true) { return 1; }` has no
/// no-branch path to add, and reading it as one would accuse a function that
/// demonstrably returns. A provably-true condition ends the chain at its own arm
/// (no later `elseif`/`else`/implicit arm); a provably-false one contributes none.
///
/// **Recorded obstacle:** only *literals* are read. A constant-folded guard —
/// `if (PHP_VERSION_ID >= 80000) { return 1; }`, `if (self::ENABLED) { … }` — still
/// contributes the implicit fall-through arm, since folding needs the project index
/// this judgment does without. A guard of that shape with no `else` is
/// `type.return-missing`'s second named over-report risk, alongside the undeclared
/// never-returning callee.
fn if_end(i: &mago_syntax::cst::If<'_>) -> BodyEnd {
    let body = &i.body;
    let mut chain: Vec<(&Expression<'_>, &[Statement<'_>])> =
        vec![(i.condition, body.statements())];
    chain.extend(body.else_if_clauses());

    let mut arms = Vec::new();
    for (cond, stmts) in chain {
        if expr_is_false(cond) {
            // The arm can never be taken; it contributes no path.
            continue;
        }
        arms.push(block_end(stmts));
        if expr_is_true(cond) {
            // Always taken: no later arm and no implicit no-branch path exist.
            return BodyEnd::join_arms(arms);
        }
    }
    match body.else_statements() {
        Some(stmts) => arms.push(block_end(stmts)),
        // No `else`: the no-branch-taken path runs straight to the successor.
        None => arms.push(BodyEnd::FallsThrough),
    }
    BodyEnd::join_arms(arms)
}

/// A `switch`'s terminality: the join over its case bodies, with the **implicit
/// no-match arm** joined in as [`BodyEnd::FallsThrough`] when there is no
/// `default`.
///
/// Two shapes make the whole construct [`BodyEnd::Unknown`], both honest answers
/// rather than shortcuts:
///
/// * **any `break` / `continue` / `goto` in the subtree.** A `break` in a case
///   body exits the *switch* and lands on its successor — the exact opposite of
///   what [`stmt_end`] says about a `break` in isolation, where it terminates the
///   list it sits in. Telling the two apart means resolving the jump's target
///   through nested `if`s, loops and inner switches, which this judgment does not
///   do. A `switch` whose every case `break`s would otherwise be read as
///   *terminating*, and a dead-code consumer would call everything after it
///   unreachable — the single worst mistake available here.
/// * **a non-empty case body that runs off its end** into the *next* case. PHP's
///   case-to-case fall-through is a real control-flow edge, not modelled; an empty
///   case label (`case 1: case 2: body`) is that shape used deliberately, and
///   contributes no arm of its own.
fn switch_end(sw: &mago_syntax::cst::Switch<'_>) -> BodyEnd {
    if subtree_has_switch_jump(&Node::Switch(sw)) {
        return BodyEnd::Unknown;
    }
    let mut arms = Vec::new();
    let mut has_default = false;
    for case in sw.body.cases() {
        if case.expression().is_none() {
            has_default = true;
        }
        if case.is_empty() {
            continue;
        }
        let end = block_end(case.statements());
        if !end.provably_terminates() {
            // The body runs off its end — into the next case, not past the switch.
            return BodyEnd::Unknown;
        }
        arms.push(end);
    }
    if !has_default {
        arms.push(BodyEnd::FallsThrough);
    }
    BodyEnd::join_arms(arms)
}

/// Whether `node`'s subtree contains a jump whose target could be this `switch`:
/// a `break`, a `continue` (which PHP accepts inside a `switch`, where it acts on
/// the enclosing loop) or a `goto`. Nested function-likes are not descended.
fn subtree_has_switch_jump(node: &Node<'_, '_>) -> bool {
    match node {
        Node::Break(_) | Node::Continue(_) | Node::Goto(_) => true,
        Node::Function(_) | Node::Method(_) | Node::Closure(_) | Node::ArrowFunction(_) => false,
        _ => children(node).iter().any(subtree_has_switch_jump),
    }
}

/// A loop's terminality from the two facts that decide it: whether its condition
/// is a proven-true literal, and whether its subtree contains a jump that could
/// leave it.
///
/// * not provably infinite → [`BodyEnd::FallsThrough`]: the false-condition exit
///   edge exists (a `while ($x)` whose `$x` happens to always be true is a hang,
///   not a fall-through, but proving that is path feasibility — outside this
///   judgment by design, same as `if ($c) { return 1; }`);
/// * infinite with no `break`/`goto` anywhere inside → [`BodyEnd::Terminates`]:
///   there is no exit edge at all;
/// * infinite *with* one → [`BodyEnd::Unknown`]: a `break` may belong to a nested
///   `switch` or loop rather than to this one, and resolving jump targets is not
///   this judgment's job.
///
/// `continue` is deliberately not a jump here: it re-enters the loop, it never
/// leaves it — not even `continue 2` from a nested loop, which targets *this*
/// loop's next iteration.
fn loop_end(infinite: bool, node: &Node<'_, '_>) -> BodyEnd {
    if !infinite {
        return BodyEnd::FallsThrough;
    }
    if subtree_has_exit_jump(node) { BodyEnd::Unknown } else { BodyEnd::Terminates }
}

/// Whether `node`'s subtree contains a **function exit** — a `return`, a `throw`
/// or an `exit`/`die` — at any depth (ADR-0078 §5). Nested function-likes are their
/// own scopes and are not descended: a `return` inside a closure exits the closure,
/// not the body that defines it.
///
/// Deliberately **not** counting `break`/`continue`: those leave a construct, never
/// the function, and a `switch` full of `break`s is no evidence at all that the
/// author meant to return something.
fn subtree_has_function_exit(node: &Node<'_, '_>) -> bool {
    match node {
        Node::Return(_)
        | Node::Throw(_)
        | Node::ExitConstruct(_)
        | Node::DieConstruct(_)
        | Node::HaltCompiler(_) => true,
        Node::Function(_) | Node::Method(_) | Node::Closure(_) | Node::ArrowFunction(_) => false,
        _ => children(node).iter().any(subtree_has_function_exit),
    }
}

/// Whether `node`'s subtree contains a `break` or a `goto` — a jump that could
/// leave an enclosing loop. Nested function-likes are their own scopes and are not
/// descended (their jumps cannot leave this loop).
fn subtree_has_exit_jump(node: &Node<'_, '_>) -> bool {
    match node {
        Node::Break(_) | Node::Goto(_) => true,
        Node::Function(_) | Node::Method(_) | Node::Closure(_) | Node::ArrowFunction(_) => false,
        _ => children(node).iter().any(subtree_has_exit_jump),
    }
}

/// Whether an expression is a **proven-true literal** — the only conditions this
/// judgment reads as always-taken. `while (true)`, `while (1)` and `for (;;)` are
/// the idioms; anything else (a variable, a call, a comparison) is left to the
/// non-deterministic reading, which is the safe side for a *loop* condition
/// because it produces [`BodyEnd::FallsThrough`], never a claim of termination.
fn expr_is_true(expr: &Expression<'_>) -> bool {
    match lower_arg_value(expr.unparenthesized()) {
        ArgValue::Bool(b) => b,
        ArgValue::Int(i) => i != 0,
        _ => false,
    }
}

/// Whether an expression is a **proven-false literal** — the mirror of
/// [`expr_is_true`], read only for an `if`/`elseif` condition (see [`if_end`]).
/// `false`, `0` and `null` are the spellings that appear; anything non-literal is
/// left to the non-deterministic reading.
fn expr_is_false(expr: &Expression<'_>) -> bool {
    match lower_arg_value(expr.unparenthesized()) {
        ArgValue::Bool(b) => !b,
        ArgValue::Int(i) => i == 0,
        ArgValue::Null => true,
        _ => false,
    }
}

/// Where an expression in **statement position** leaves control. The expression
/// forms that terminate are exactly the three the trace IR already models as
/// terminators — `throw`, `exit`, `die` — plus a statement-position `match`,
/// whose arms are themselves expressions.
///
/// A plain call answers [`BodyEnd::FallsThrough`]; see `stmt_end`'s recorded
/// obstacle on never-returning callees for why, and where that refinement lives.
fn expr_end(expr: &Expression<'_>) -> BodyEnd {
    match expr.unparenthesized() {
        Expression::Throw(_) => BodyEnd::Terminates,
        Expression::Construct(Construct::Exit(_) | Construct::Die(_)) => BodyEnd::Terminates,
        Expression::Match(m) => match_end(m),
        _ => BodyEnd::FallsThrough,
    }
}

/// A `match`'s terminality: the join over its arm bodies, with the **implicit
/// no-match arm** joined in as [`BodyEnd::Terminates`] when there is no
/// `default` — PHP throws `\UnhandledMatchError` there (witnessed 8.5.9), and a
/// throw is a terminator.
///
/// This is the one place where a missing default makes a construct *more*
/// terminal rather than less, and it is the exact opposite of `switch`'s rule
/// above. The two are different constructs with different semantics; sharing one
/// rule between them would be wrong for one of them.
fn match_end(m: &mago_syntax::cst::Match<'_>) -> BodyEnd {
    let mut arms = Vec::new();
    let mut has_default = false;
    for arm in m.arms.iter() {
        match arm {
            mago_syntax::cst::MatchArm::Expression(a) => arms.push(expr_end(a.expression)),
            mago_syntax::cst::MatchArm::Default(a) => {
                has_default = true;
                arms.push(expr_end(a.expression));
            }
        }
    }
    if !has_default {
        arms.push(BodyEnd::Terminates);
    }
    BodyEnd::join_arms(arms)
}

// end reachability foundation (ADR-0078, issue #199)

/// Every [`StringContextSite`] a statement's **own** expressions carry (ADR-0078,
/// issue #193).
///
/// # The position boundary, and why it is here
///
/// Four statement kinds are read: an expression statement, `return`, and the two
/// `echo` forms — `$s = "x $v";`, `f((string) $v)`, `return 'a' . $v;`, `echo $v;`,
/// `print $v;`, `<?= $v ?>` — each a position where the walk's ENTRY env is exactly
/// the env PHP evaluates the expression in.
///
/// Everything else is recorded silence: a branch condition, loop header, `match`
/// subject or `switch` case is evaluated in an env this pass does not hold (an
/// `elseif` condition runs only after the previous branch is refuted; a loop
/// header runs once per iteration), and unstructured construct bodies are not
/// lowered as statements at all. Same position boundary every other value-reading
/// check carries, minus the `if`-guard the preg pattern check adds.
///
/// Nested statements are never descended: an `if` branch's body is lowered by
/// [`lower_stmt`] itself and collects its own sites, so nothing double-counts.
fn string_context_sites(s: &Statement<'_>) -> Vec<StringContextSite> {
    let mut out = Vec::new();
    match s {
        Statement::Expression(es) => {
            scan_string_contexts(&Node::Expression(es.expression), &mut out);
        }
        Statement::Return(r) => {
            if let Some(e) = r.value {
                scan_string_contexts(&Node::Expression(e), &mut out);
            }
        }
        // Each `echo` operand is itself a conversion. An operand that is a composite
        // string or a cast lowers to `Other` here (proving nothing) and is collected
        // again, precisely, by the scan — so a value is reported once, at the
        // innermost construct that names it.
        Statement::Echo(e) => {
            for v in e.values.iter() {
                out.push(echo_site(v));
                scan_string_contexts(&Node::Expression(v), &mut out);
            }
        }
        Statement::EchoTag(e) => {
            for v in e.values.iter() {
                out.push(echo_site(v));
                scan_string_contexts(&Node::Expression(v), &mut out);
            }
        }
        _ => {}
    }
    out
}

/// One `echo` / `<?= ?>` operand as a site.
fn echo_site(v: &Expression<'_>) -> StringContextSite {
    StringContextSite {
        value: lower_arg_value(v),
        span: to_span(v.span()),
        kind: StringContextKind::Echo,
    }
}

/// Collect the string conversions inside one expression subtree.
///
/// Function-like bodies are not descended — a closure, an arrow function and a
/// nested declaration are their own scopes, lowered (and judged) separately, and
/// their free variables are not this statement's env.
fn scan_string_contexts(node: &Node<'_, '_>, out: &mut Vec<StringContextSite>) {
    let mut site = |e: &Expression<'_>, kind| {
        out.push(StringContextSite { value: lower_arg_value(e), span: to_span(e.span()), kind });
    };
    match node {
        Node::Function(_) | Node::Method(_) | Node::Closure(_) | Node::ArrowFunction(_) => return,
        // `"a $v"`, `"{$v}"`, a heredoc body, and a backtick string: every embedded
        // expression is converted. A nowdoc and a single-quoted string carry only
        // literal parts and so contribute nothing.
        Node::CompositeString(cs) => {
            for part in cs.parts().iter() {
                match part {
                    StringPart::Literal(_) => {}
                    StringPart::Expression(e) => site(e, StringContextKind::Interpolation),
                    StringPart::BracedExpression(b) => {
                        site(b.expression, StringContextKind::Interpolation);
                    }
                }
            }
        }
        // `(string) $v`. Every other cast converts to something else entirely and is
        // not these ids' business.
        Node::UnaryPrefix(u) if matches!(u.operator, UnaryPrefixOperator::StringCast(..)) => {
            site(u.operand, StringContextKind::Cast);
        }
        // `$a . $b` — BOTH operands convert, and PHP warns once per array operand, so
        // both are sites. A left-nested chain `'a' . $x . $y` visits each inner
        // `Binary` in turn, so every leaf is collected exactly once (the nested
        // operand lowers to `ArgValue::Concat`, which proves no value unless both
        // its own operands do — never a second report).
        Node::Binary(b) if b.operator.is_concatenation() => {
            site(b.lhs, StringContextKind::Concat);
            site(b.rhs, StringContextKind::Concat);
        }
        // `$a .= $b` reads `$a` in string context too — `$arr .= 'x'` warns on the
        // left-hand side exactly as `$arr . 'x'` does.
        Node::Assignment(a) if matches!(a.operator, AssignmentOperator::Concat(_)) => {
            site(a.lhs, StringContextKind::Concat);
            site(a.rhs, StringContextKind::Concat);
        }
        Node::PrintConstruct(p) => site(p.value, StringContextKind::Print),
        _ => {}
    }
    for child in children(node) {
        scan_string_contexts(&child, out);
    }
}

/// The full [`CallExpr`] when `expr` (unparenthesized) is a resolvable call —
/// a statically-named function, an instance/static method call, or a `new`
/// construction — else `None` (dynamic receivers carry nothing the checker can
/// resolve, so they are dropped rather than tracked).
fn named_call(expr: &Expression<'_>) -> Option<CallExpr> {
    match expr.unparenthesized() {
        Expression::Call(Call::Function(fc)) => {
            let call = lower_call(fc);
            // A named function (`f(...)`) or a variable call (`$fn(...)`) is
            // resolvable by the propagation walk; a fully dynamic callee is not.
            (call.receiver != Callee::Dynamic).then_some(call)
        }
        Expression::Call(Call::Method(mc)) => {
            let call = lower_method_call(mc.object, &mc.method, &mc.argument_list, to_span(mc.span()), false);
            (call.receiver != Callee::Dynamic).then_some(call)
        }
        Expression::Call(Call::NullSafeMethod(mc)) => {
            let call = lower_method_call(mc.object, &mc.method, &mc.argument_list, to_span(mc.span()), true);
            (call.receiver != Callee::Dynamic).then_some(call)
        }
        Expression::Call(Call::StaticMethod(sc)) => {
            let call = lower_static_call(sc.class, &sc.method, &sc.argument_list, to_span(sc.span()));
            (call.receiver != Callee::Dynamic).then_some(call)
        }
        Expression::Instantiation(inst) => lower_construct_call(inst),
        _ => None,
    }
}

/// Lower a structured `if`/`elseif`/`else` statement (ADR-0031) to
/// [`StmtKind::If`]. Each branch body is lowered by the same statement rules as
/// the enclosing scope (so nested ifs recurse and unstructured constructs inside
/// a branch appear as `Opaque`/`Barrier` within the sub-trace). Both the brace
/// body and the colon-delimited (`if: … endif;`) form are handled via the CST's
/// body accessors.
fn lower_if(if_stmt: &mago_syntax::cst::If<'_>) -> Stmt {
    let body = &if_stmt.body;
    let cond = lower_cond(if_stmt.condition);
    let then_trace = lower_trace(body.statements());
    let elseifs = body
        .else_if_clauses()
        .into_iter()
        .map(|(c, stmts)| (lower_cond(c), lower_trace(stmts)))
        .collect();
    let else_trace = body.else_statements().map(lower_trace);
    Stmt::lowered(StmtKind::If { cond, then_trace, elseifs, else_trace }, Vec::new())
}

/// Lower a borrowed statement list to a sub-trace (a branch body). Shares the
/// per-statement lowering with the top-level scope walk.
fn lower_trace(statements: &[Statement<'_>]) -> Vec<Stmt> {
    let mut out = Vec::new();
    for s in statements {
        lower_stmt(s, &mut out);
    }
    out
}

/// Lower a match-arm body expression (`… => <expr>`) to a sub-trace. The body is
/// an expression, so it reuses [`lower_expr_stmt`] (an arm body that is `throw …`
/// therefore lowers to a real [`StmtKind::Throw`] terminator), preceded by the
/// entries a `match` in value position inside it contributes (issue #430) — an
/// arm body is a statement position by any other name, so it gets the same
/// treatment [`lower_stmt`] gives one.
fn lower_arm_body(expr: &Expression<'_>) -> Vec<Stmt> {
    let mut out = Vec::new();
    // A `match` that IS the arm body is a statement position: `lower_expr_stmt`
    // structures it below, and hoisting it here too would walk its arms twice.
    if !matches!(expr.unparenthesized(), Expression::Match(_)) {
        scan_value_matches(&Node::Expression(expr), &mut out);
    }
    let st = lower_expr_stmt(expr);
    // This path bypasses `lower_stmt`, so it owns its own terminality fill
    // (ADR-0078, issue #199) — from the arm's expression, the same `expr_end` a
    // statement-position expression gets.
    out.push(Stmt {
        span: to_span(expr.span()),
        end: expr_end(expr),
        has_terminator: subtree_has_function_exit(&Node::Expression(expr)),
        ..st
    });
    out
}

/// The trace entries a statement's **value-position** `match` expressions
/// contribute, pushed ahead of the statement that consumes them (issue #430).
///
/// A `match` whose result is consumed — `$r = match (…)`, `return match (…)`,
/// `echo match (…)`, `f(match (…))` — is the form nearly all real code uses, and
/// until it lowered here its arms were never walked at all: only the
/// statement-position path reached [`lower_match_stmt`], so every arm body was
/// invisible to the walk. The hoisted entry restores exactly what statement
/// position already had — per-arm first-match certainty, dead-arm marking, and
/// the diagnostics an arm body emits — and nothing else. The **value** the
/// `match` produces stays what it was: `lower_arg_value` still answers
/// [`ArgValue::Other`] for a `match` and `named_call` still answers `None`, so
/// the consuming statement's own value lane is untouched by this.
///
/// Only the positions whose expressions PHP evaluates in the statement's own
/// entry env are read — an expression statement, `return`, and the two `echo`
/// forms — the same boundary [`string_context_sites`] draws and for the same
/// reason. A `match` in an `if` condition or a loop header is evaluated in an env
/// this pass does not hold, and stays unstructured.
fn value_position_matches(s: &Statement<'_>, out: &mut Vec<Stmt>) {
    match s {
        Statement::Expression(es) => {
            // A `match` that IS the statement is a statement position, already
            // structured by `lower_expr_stmt`; hoisting it would double the walk.
            if matches!(es.expression.unparenthesized(), Expression::Match(_)) {
                return;
            }
            scan_value_matches(&Node::Expression(es.expression), out);
        }
        Statement::Return(r) => {
            if let Some(e) = r.value {
                scan_value_matches(&Node::Expression(e), out);
            }
        }
        Statement::Echo(e) => {
            for v in e.values.iter() {
                scan_value_matches(&Node::Expression(v), out);
            }
        }
        Statement::EchoTag(e) => {
            for v in e.values.iter() {
                scan_value_matches(&Node::Expression(v), out);
            }
        }
        _ => {}
    }
}

/// Collect the structured entries for every value-position `match` in one
/// expression subtree, in source order.
///
/// Two subtrees are never descended, each for its own reason:
///
/// * a nested function-like or class — a separate scope, lowered separately, and
///   its free variables are not this statement's env;
/// * the arms of a `match` this scan has already taken — [`lower_arm_body`] runs
///   the same hoist inside each arm, so descending here would walk them twice.
///
/// A `match` [`lower_match_stmt`] refuses contributes nothing and is not
/// descended either: all-or-nothing structuring is what makes the first-match and
/// no-`default`-throws rules sound, and an arm of an unstructured outer `match`
/// is not a position this walk can claim is reached.
fn scan_value_matches(node: &Node<'_, '_>, out: &mut Vec<Stmt>) {
    match node {
        Node::Function(_)
        | Node::Method(_)
        | Node::Closure(_)
        | Node::ArrowFunction(_)
        | Node::AnonymousClass(_)
        | Node::Class(_)
        | Node::Interface(_)
        | Node::Trait(_)
        | Node::Enum(_) => return,
        Node::Match(m) => {
            if let Some(st) = lower_match_stmt(m) {
                out.push(Stmt {
                    span: to_span(m.span()),
                    // The same terminality a statement-position `match` gets, off
                    // the same CST node (ADR-0078): a construct every arm of which
                    // throws does not fall through just because its result was
                    // about to be assigned.
                    end: match_end(m),
                    has_terminator: subtree_has_function_exit(node),
                    ..st
                });
            }
            return;
        }
        _ => {}
    }
    for child in children(node) {
        scan_value_matches(&child, out);
    }
}

/// Structure a statement-position `match ($subject) { … }` (ADR-0031 Part B).
/// Returns `None` — falling back to `Opaque` — when neither shape fits: the
/// **by-value** shape ([`lower_match_by_value`], subject and every arm condition a
/// variable/literal) or the **guard-chain** shape ([`lower_match_guard_chain`],
/// `match (true)`/`match (false)` over conditions). Both are all-or-nothing:
/// partial structuring is unsound for the first-match and no-`default`-throws
/// rules.
///
/// The by-value shape is tried first, so nothing it already structures changes
/// meaning — `match (true) { true => …, false => … }` stays a by-value `match` on
/// a boolean subject, and the guard chain is reached only where the answer used to
/// be `Opaque`.
fn lower_match_stmt(m: &mago_syntax::cst::Match<'_>) -> Option<Stmt> {
    lower_match_by_value(m).or_else(|| lower_match_guard_chain(m))
}

/// The by-value `match`: subject and every arm condition lower to a
/// variable/literal, and the arms are compared against the subject with `===`.
/// `None` when any of them does not lower, or when more than one `default` arm is
/// present.
fn lower_match_by_value(m: &mago_syntax::cst::Match<'_>) -> Option<Stmt> {
    let subject = usable_operand(m.expression)?;
    let mut arms = Vec::new();
    let mut default: Option<Vec<Stmt>> = None;
    for arm in m.arms.iter() {
        match arm {
            mago_syntax::cst::MatchArm::Expression(a) => {
                let mut conditions = Vec::new();
                for c in a.conditions.iter() {
                    conditions.push(usable_operand(c)?);
                }
                arms.push(MatchArmT { conditions, trace: lower_arm_body(a.expression) });
            }
            mago_syntax::cst::MatchArm::Default(a) => {
                if default.is_some() {
                    return None; // two defaults — give up (unreachable in valid PHP)
                }
                default = Some(lower_arm_body(a.expression));
            }
        }
    }
    Some(Stmt::lowered(StmtKind::Match { subject, arms, default, loose: false }, Vec::new()))
}

/// Structure `match (true) { <guard> => …, … }` — an `if`/`elseif` chain written
/// in `match` syntax (issue #431) — as exactly that: a [`StmtKind::If`] whose
/// links are the arms in source order and whose `else` is the `default`.
///
/// The desugaring is the whole point. First-match order *is* `elseif` order, so
/// the arm walk, the accumulated subtraction every later arm and the `default`
/// inherit (ADR-0052's arm-wise negation), the guard vocabulary and the dead-branch
/// marking all arrive as the `if` path's, not as a second implementation of them.
/// `default` becomes the `else` wherever it is written, since PHP consults it only
/// when nothing else matched.
///
/// Three refusals, each all-or-nothing (`None` → the whole construct is `Opaque`):
///
/// * a subject that is not the literal `true`/`false`. `match ($x) { is_int($y) => … }`
///   is a *comparison* against `$x`, not a guard chain, and `match (1) { … }` likewise;
/// * an arm condition [`arm_cond_is_bool_valued`] refuses — `match` compares with
///   `===`, so reading the arm as its condition's truth is only sound where the two
///   agree;
/// * a second `default`.
///
/// `match (false)` is the same chain with every arm's sense inverted: the arm runs
/// when its condition is `false`, which is `!cond` for the conditions this accepts.
fn lower_match_guard_chain(m: &mago_syntax::cst::Match<'_>) -> Option<Stmt> {
    let sense = bool_literal_subject(m.expression)?;
    let mut links: Vec<(CondExpr, Vec<Stmt>)> = Vec::new();
    let mut default: Option<Vec<Stmt>> = None;
    for arm in m.arms.iter() {
        match arm {
            mago_syntax::cst::MatchArm::Expression(a) => {
                // `cond1, cond2 => …` takes the arm when EITHER holds, so the
                // conditions fold with `||` — after the per-condition inversion, so
                // `match (false) { a, b => … }` reads `!a || !b`.
                let mut cond: Option<CondExpr> = None;
                for c in a.conditions.iter() {
                    let one = guard_arm_cond(c, sense)?;
                    cond = Some(match cond {
                        None => one,
                        Some(acc) => CondExpr::Or(Box::new(acc), Box::new(one)),
                    });
                }
                links.push((cond?, lower_arm_body(a.expression)));
            }
            mago_syntax::cst::MatchArm::Default(a) => {
                if default.is_some() {
                    return None; // two defaults — give up (unreachable in valid PHP)
                }
                default = Some(lower_arm_body(a.expression));
            }
        }
    }
    let mut links = links.into_iter();
    let (cond, then_trace) = links.next()?; // `match (true) { default => … }` is by-value
    Some(Stmt::lowered(
        StmtKind::If { cond, then_trace, elseifs: links.collect(), else_trace: default },
        Vec::new(),
    ))
}

/// Structural scan (issue #448, ADR-0088 §5's note) for `match (true)`/`match
/// (false)` guard chains with no `default` arm — the shape [`lower_match_guard_chain`]
/// desugars to [`StmtKind::If`] with `else_trace: None`. Populates
/// [`Scope::guard_chain_no_default`]; see that field's doc for why this lives
/// outside the trace IR rather than as a bit on [`StmtKind::If`] itself.
///
/// Authoritative rather than re-deriving the three refusals by hand: a `Match`
/// node's span is recorded exactly when [`lower_match_stmt`] — the same function
/// [`scan_value_matches`] and the statement-position lowering both call — answers
/// with an `If` carrying no `else`. Cheap: the re-lowering this calls is pure and
/// runs only on an actual `match` construct, and default-less `match (true)`
/// chains are rare.
///
/// Nested scopes are skipped, matching every sibling structural scan
/// ([`scan_throw_origins`], [`scan_effect_origins`]); a `match` inside a live arm
/// is still reached, since only the outer construct's own arms are skipped by
/// [`lower_match_stmt`] itself, not this walk.
fn scan_guard_chain_no_default(node: &Node<'_, '_>, out: &mut Vec<Span>) {
    if let Node::Match(m) = node
        && !m.arms.iter().any(mago_syntax::cst::MatchArm::is_default)
        && matches!(
            lower_match_stmt(m),
            Some(Stmt { kind: StmtKind::If { else_trace: None, .. }, .. })
        )
    {
        out.push(to_span(m.span()));
        // Fall through (below) to descend into the arms too — they may hold
        // matches of their own.
    }
    if matches!(
        node,
        Node::Function(_)
            | Node::Closure(_)
            | Node::ArrowFunction(_)
            | Node::AnonymousClass(_)
            | Node::Class(_)
            | Node::Interface(_)
            | Node::Trait(_)
            | Node::Enum(_)
    ) {
        return;
    }
    for child in children(node) {
        scan_guard_chain_no_default(&child, out);
    }
}

/// `Some(true)` / `Some(false)` when the `match` subject is written as the literal
/// `true` / `false`, else `None`. Read off [`lower_cond_operand`] so a parenthesized
/// or case-varied spelling (`match (TRUE)`) answers the same as the bare one.
fn bool_literal_subject(expr: &Expression<'_>) -> Option<bool> {
    match lower_cond_operand(expr) {
        CondOperand::Literal(ArgValue::Bool(b)) => Some(b),
        _ => None,
    }
}

/// One arm condition of a guard chain, lowered by [`lower_cond`] — the very
/// lowering the `if` path uses — and inverted for a `match (false)` subject.
fn guard_arm_cond(expr: &Expression<'_>, sense: bool) -> Option<CondExpr> {
    let cond = lower_cond(expr);
    if !arm_cond_is_bool_valued(&cond) {
        return None;
    }
    Some(if sense { cond } else { CondExpr::Not(Box::new(cond)) })
}

/// May a `match (true)` arm be read as "its condition holds"?
///
/// `match` compares with `===`, so the arm runs on `<cond> === true` and the later
/// arms inherit `<cond> !== true` — which is the condition's negation **only where
/// the condition is boolean-valued**. `match (true) { $n => … }` is the shape that
/// makes the difference bite: `$n = 5` takes no arm, and reading the residue as
/// "`$n` is falsy" would hand every later arm and the `default` a narrowing PHP
/// never proved. So [`CondExpr::Truthy`] — the one lowered form whose truth set is
/// wider than `{true}` — is refused, and with it the whole construct.
///
/// `!`, `&&` and `||` yield `bool` in PHP whatever their operands are, comparisons
/// and `instanceof` and `isset` likewise, so those are unconditionally fine.
/// [`CondExpr::Opaque`] is fine for the opposite reason: it narrows nothing on
/// either side, so no reading of it can claim anything.
///
/// [`CondExpr::Call`] is the judgment call. A call in `match (true)` arm position
/// is a predicate in every idiom that works — a callee returning anything but
/// `bool` matches *no* arm at all, so the code would not be written — and refusing
/// calls would refuse `is_string($foo)`, the form the feature exists for. The
/// residual exposure is a non-`bool` callee that also carries
/// `@phpstan-assert-if-false` or an out-parameter (`preg_match(…) => …`), where the
/// no-match path would read the tag at a polarity PHP did not prove; measured at
/// zero occurrences across the public corpus.
fn arm_cond_is_bool_valued(cond: &CondExpr) -> bool {
    match cond {
        CondExpr::Cmp { .. }
        | CondExpr::Instanceof { .. }
        | CondExpr::Not(_)
        | CondExpr::And(..)
        | CondExpr::Or(..)
        | CondExpr::Isset { .. }
        | CondExpr::Call { .. }
        | CondExpr::Opaque { .. } => true,
        CondExpr::Truthy(_) => false,
    }
}

/// Structure a `switch ($subject) { … }` (ADR-0031 Part B) into the same
/// [`StmtKind::Match`] node with `loose: true`. Returns `None` — falling back to
/// `Opaque` — unless the subject and every case condition lower to a
/// variable/literal AND every non-empty case ends in `break`/`return`/`throw`/
/// `exit` with no fall-through. Empty case labels stack onto the following
/// non-empty case as extra conditions (`case 1: case 2: body`), matching PHP
/// fall-through-to-the-body semantics; a trailing `break` is stripped (end-of-arm,
/// not a trace terminator). A stray `break`/`continue`/`goto` inside a case body
/// makes the whole construct opaque — modeling it as an arm would be unsound.
fn lower_switch(sw: &mago_syntax::cst::Switch<'_>) -> Option<Stmt> {
    let subject = usable_operand(sw.expression)?;
    let mut arms: Vec<MatchArmT> = Vec::new();
    let mut default: Option<Vec<Stmt>> = None;
    // Conditions of consecutive empty case labels, waiting to stack onto the next
    // non-empty case body; `pending_default` records an empty `default:` label.
    let mut pending: Vec<CondOperand> = Vec::new();
    let mut pending_default = false;

    for case in sw.body.cases() {
        // The case's own comparison operand (None for `default`), rejected early
        // if it does not lower to a variable/literal.
        let cond = match case.expression() {
            Some(e) => Some(usable_operand(e)?),
            None => None,
        };
        if case.is_empty() {
            // An empty label falls through to the next case body: remember it.
            match cond {
                Some(c) => pending.push(c),
                None => {
                    if default.is_some() {
                        return None;
                    }
                    pending_default = true;
                }
            }
            continue;
        }
        // A non-empty case must end cleanly: strip a trailing plain `break;`, else
        // require a terminator; a stray jump anywhere in the body is unsound.
        let raw = case.statements();
        let (body, ends_break) = strip_trailing_break(raw)?;
        if case_has_stray_jump(body) {
            return None;
        }
        let trace = lower_trace(body);
        if !ends_break {
            // No break: the body must terminate, or it would fall through to the
            // next case (which structuring cannot model).
            let terminates = matches!(
                trace.last().map(|s| &s.kind),
                Some(StmtKind::Return { .. } | StmtKind::Throw { .. } | StmtKind::Exit { .. })
            );
            if !terminates {
                return None;
            }
        }
        // Build this arm, stacking any pending empty-label conditions in front.
        match cond {
            Some(c) if !pending_default => {
                let mut conditions = std::mem::take(&mut pending);
                conditions.push(c);
                arms.push(MatchArmT { conditions, trace });
            }
            // This body is (or is reached by fall-through from) `default:`; a
            // default subsumes any stacked case conditions (it catches all).
            _ => {
                if default.is_some() {
                    return None;
                }
                default = Some(trace);
            }
        }
        pending.clear();
        pending_default = false;
    }
    // Trailing empty labels with no following body do nothing at runtime, but
    // structuring them as no-op arms is fiddly; bail to Opaque (sound).
    if !pending.is_empty() || pending_default {
        return None;
    }
    Some(Stmt::lowered(StmtKind::Match { subject, arms, default, loose: true }, Vec::new()))
}

/// Lower an operand to a *usable* [`CondOperand`] — a bare variable, a literal, or
/// a class-constant/enum-case fetch — or `None` for anything else (a call,
/// property fetch, arithmetic). Used to gate whether the **by-value** shape of a
/// `match`/`switch` can be structured at all; a `match` this refuses is offered to
/// [`lower_match_guard_chain`] before it is given up as `Opaque`.
///
/// [`CondOperand::ClassConst`] used to refuse here too (issue #429), keeping every
/// enum `match`/`switch` opaque until the no-match path could subtract a case
/// (#439) and the throw-origin gate could tell an exhaustive chain from one
/// missing a case (issue #433, ADR-0088 §5). Both landed, so structuring
/// `case Suit::Hearts:` is sound now: measured on the public corpus at 184
/// `case X::C:` labels / 463 `X::C =>` arms with a 0-line A/B diff.
fn usable_operand(expr: &Expression<'_>) -> Option<CondOperand> {
    match lower_cond_operand(expr) {
        CondOperand::Other { .. } => None,
        operand => Some(operand),
    }
}

/// Split a case body into (body-without-terminating-break, ended-in-break). A
/// trailing `break;` / `break 1;` is stripped; a `break N` (N > 1) or a
/// non-literal level targets an outer construct — unrepresentable, so `None`.
fn strip_trailing_break<'a, 'arena>(
    raw: &'a [Statement<'arena>],
) -> Option<(&'a [Statement<'arena>], bool)> {
    match raw.last() {
        Some(Statement::Break(b)) => {
            if break_is_plain(b) { Some((&raw[..raw.len() - 1], true)) } else { None }
        }
        _ => Some((raw, false)),
    }
}

/// Whether a `break` targets its immediately-enclosing construct (`break;` or
/// `break 1;`) as opposed to an outer one (`break 2;`, `break $n;`).
fn break_is_plain(b: &mago_syntax::cst::Break<'_>) -> bool {
    match b.level {
        None => true,
        Some(e) => matches!(lower_arg_value(e), ArgValue::Int(1)),
    }
}

/// Whether a switch-case body contains a `break`/`continue`/`goto` that would
/// target the switch from inside the case (making arm modeling unsound). Nested
/// loops and switches consume their own `break`/`continue`, so the scan does not
/// descend into them; nested function-likes are separate scopes. Any `goto` at
/// all disqualifies (its target is unbounded).
fn case_has_stray_jump(body: &[Statement<'_>]) -> bool {
    body.iter().any(|s| stmt_has_stray_jump(s))
}

fn stmt_has_stray_jump(s: &Statement<'_>) -> bool {
    match s {
        Statement::Break(_) | Statement::Continue(_) | Statement::Goto(_) => true,
        // Nested loops/switch absorb their own break/continue — do not descend.
        Statement::While(_)
        | Statement::For(_)
        | Statement::Foreach(_)
        | Statement::DoWhile(_)
        | Statement::Switch(_) => false,
        _ => node_has_stray_jump(&Node::Statement(s)),
    }
}

/// Recurse through a node's children looking for a stray jump, stopping at nested
/// loops/switches (which consume their own) and nested function-like scopes.
fn node_has_stray_jump(node: &Node<'_, '_>) -> bool {
    children(node).iter().any(|child| match child {
        Node::Break(_) | Node::Continue(_) | Node::Goto(_) => true,
        Node::While(_)
        | Node::For(_)
        | Node::Foreach(_)
        | Node::DoWhile(_)
        | Node::Switch(_)
        | Node::Function(_)
        | Node::Closure(_)
        | Node::ArrowFunction(_)
        | Node::AnonymousClass(_)
        | Node::Class(_)
        | Node::Interface(_)
        | Node::Trait(_)
        | Node::Enum(_) => false,
        other => node_has_stray_jump(other),
    })
}

/// Lower an expression-statement to a trace entry.
fn lower_expr_stmt(expr: &Expression<'_>) -> Stmt {
    match expr.unparenthesized() {
        Expression::Assignment(a) => {
            if let Expression::Variable(Variable::Direct(dv)) = a.lhs.unparenthesized() {
                let var = strip_dollar(bytes_to_string(dv.name));
                // Only a plain `=` yields a value; compound ops (`+=`, `.=`, …)
                // make the variable unknown.
                let value = if a.operator.is_assign() { lower_arg_value(a.rhs) } else { ArgValue::Other };
                let invalidated = call_invalidation(&Node::Expression(a.rhs));
                // `$x = f($s);` — carry the RHS call for propagation/descent.
                let call = if a.operator.is_assign() { named_call(a.rhs) } else { None };
                let span = to_span(a.lhs.span());
                Stmt::lowered(StmtKind::Assign { var, value, span, call }, invalidated)
            } else if let Expression::Access(Access::Property(pa)) = a.lhs.unparenthesized()
                && let Some((target_var, prop)) = prop_fetch_of(pa.object, &pa.property)
            {
                // `$var->prop = <rvalue>` / `$this->prop = <rvalue>` (ADR-0036). A
                // compound op (`+=`, `.=`, …) makes the property value unknown.
                let value = if a.operator.is_assign() { lower_arg_value(a.rhs) } else { ArgValue::Other };
                let value_call = if a.operator.is_assign() { named_call(a.rhs) } else { None };
                let invalidated = call_invalidation(&Node::Expression(a.rhs));
                let span = to_span(a.lhs.span());
                let kind = StmtKind::PropAssign { target_var, prop, value, value_call, span };
                Stmt::lowered(kind, invalidated)
            } else if a.operator.is_assign()
                && let Some((base, keys)) = const_key_offset_path(a.lhs)
            {
                // `$var[<lit>] = …` / `$var[<lit>][<lit>] = …` (ADR-0062 A-G8).
                // Still a barrier in the walk — see `StmtKind::OffsetWrite` — but
                // one that names the base and key so the shape lane survives it.
                let invalidated = call_invalidation(&Node::Expression(a.rhs));
                let value = lower_arg_value(a.rhs);
                Stmt::lowered(StmtKind::OffsetWrite { base, keys, value }, invalidated)
            } else if a.operator.is_assign()
                && let Some(reads) = destructure_reads(a.lhs)
            {
                // `[$a, $b] = <source>;` / `list($a, $b) = <source>;` (issue #288).
                // Barrier semantics for the targets, plus the source's own reads —
                // see `StmtKind::Destructure`.
                let invalidated = call_invalidation(&Node::Expression(a.rhs));
                let source = lower_arg_value(a.rhs);
                let call = named_call(a.rhs);
                let span = to_span(a.lhs.span());
                Stmt::lowered(StmtKind::Destructure { source, call, reads, span }, invalidated)
            } else {
                // Assignment to a non-simple lvalue (`$a[] = …`, `$a[$i] = …`,
                // `$o->$p = …`, `$a->b->c = …`, `Foo::$s = …`). Barrier (the sound
                // floor); a by-ref property alias `$r = &$x->p` is caught by the
                // poison family above.
                Stmt::lowered(StmtKind::Barrier, Vec::new())
            }
        }
        Expression::Call(Call::Function(fc)) => {
            // `assert(<expr>)` — a statement-position assert whose argument lowers to
            // a condition (ADR-0052 §5). `assert` is a pure by-value builtin (it never
            // mutates its argument by reference), so the narrowed variables carry no
            // invalidation; a non-lowerable argument falls back to a plain `Call`.
            if let Some(cond) = assert_stmt_cond(fc) {
                Stmt::lowered(StmtKind::Assert { cond }, Vec::new())
            } else {
                let invalidated = call_invalidation(&Node::Expression(expr));
                Stmt::lowered(StmtKind::Call(lower_call(fc)), invalidated)
            }
        }
        // Statement-level method / static / constructor calls. A resolvable
        // receiver becomes a `Call`; a dynamic one is a `Barrier` (but its
        // call-var invalidation is still collected below via the fallthrough).
        Expression::Call(Call::Method(_) | Call::NullSafeMethod(_) | Call::StaticMethod(_))
        | Expression::Instantiation(_) => match named_call(expr) {
            Some(call) => {
                let invalidated = call_invalidation(&Node::Expression(expr));
                Stmt::lowered(StmtKind::Call(call), invalidated)
            }
            None => {
                let invalidated = call_invalidation(&Node::Expression(expr));
                Stmt::lowered(StmtKind::Barrier, invalidated)
            }
        },
        // A statement-position `match` (ADR-0031 Part B): structure its arms when
        // the subject and every arm condition lower to a variable/literal, or when
        // it is a `match (true)`/`match (false)` guard chain; else fall back to
        // `Opaque` over the whole subtree (partial structuring is unsound for the
        // first-match / no-default-throws rules).
        Expression::Match(m) => lower_match_stmt(m).unwrap_or_else(|| {
            let node = Node::Expression(expr);
            let (writes, reads, poisons, may_return) = opaque_sets(&node);
            Stmt::lowered(StmtKind::Opaque { writes, reads, poisons, may_return }, Vec::new())
        }),
        // `throw <expr>;` — a trace terminator (ADR-0031). Variables the thrown
        // expression hands to a call are still invalidated (by-ref conservatism),
        // though the terminator makes anything after it unreachable.
        Expression::Throw(t) => {
            let invalidated = call_invalidation(&Node::Expression(t.exception));
            Stmt::lowered(StmtKind::Throw { span: to_span(expr.span()) }, invalidated)
        }
        // `exit;` / `die;` — a trace terminator (ADR-0019 never-returns).
        Expression::Construct(Construct::Exit(_) | Construct::Die(_)) => {
            Stmt::lowered(StmtKind::Exit { span: to_span(expr.span()) }, Vec::new())
        }
        _ => Stmt::lowered(StmtKind::Barrier, Vec::new()),
    }
}

/// Collect the names of bare local variables passed as an argument to any call
/// within `node`. Used to invalidate those variables after the statement.
fn collect_call_vars(node: &Node<'_, '_>, out: &mut Vec<String>) {
    let arguments = match node {
        Node::FunctionCall(c) => Some(&c.argument_list),
        Node::MethodCall(c) => Some(&c.argument_list),
        Node::NullSafeMethodCall(c) => Some(&c.argument_list),
        Node::StaticMethodCall(c) => Some(&c.argument_list),
        _ => None,
    };
    if let Some(list) = arguments {
        for arg in list.arguments.iter() {
            if let Expression::Variable(Variable::Direct(dv)) = arg.value().unparenthesized() {
                let name = strip_dollar(bytes_to_string(dv.name));
                if !out.contains(&name) {
                    out.push(name);
                }
            }
        }
    }
    for child in children(node) {
        collect_call_vars(&child, out);
    }
}

/// The [`InvalidatedVar`] entries a trace entry carries: every variable the
/// subtree hands to a call, one entry per name in first-occurrence order, each
/// carrying its provable sites or the opaque verdict (ADR-0070). Every
/// construction site takes the whole answer from this one walk, so a name and
/// its evidence can never be computed over different subtrees.
fn call_invalidation(node: &Node<'_, '_>) -> Vec<InvalidatedVar> {
    let mut invalidated = Vec::new();
    scan_invalidated(node, &mut invalidated, false);
    invalidated
}

/// The one walk behind [`Stmt::invalidated`]: exactly [`collect_call_vars`]'s
/// shape — same four call nodes, same "bare `$v` argument" recognition, same
/// descent — but recording each occurrence's evidence on the name's entry as it
/// collects it, so the name set and its evidence are one answer by construction. A
/// describable occurrence appends a `(callee, position)` site; an unprovable one
/// marks the entry opaque, discarding every site it has and refusing it any future
/// one.
///
/// Unprovable, and therefore opaque (kept on the blanket drop):
///
/// * a method/nullsafe-method/static-method call — receiver mutability is a
///   separate question (ADR-0070 §4) and no `NameRef` names the target anyway;
/// * a dynamic function callee (`$f($a)`, `($o->cb)($a)`) — nothing to resolve;
/// * an argument list carrying a **named** or **spread** argument, or a
///   first-class callable (`f(...)`) — positional mapping is defeated;
/// * an occurrence inside a nested function-like body (`nested`) — a different
///   variable scope, still collected (blanket-drop conservatism) but no site may
///   vouch for it.
///
/// Language constructs (`isset`, `empty`, `unset`, `list`, `eval`, `exit`) are not
/// call nodes and never reach this walk.
fn scan_invalidated(node: &Node<'_, '_>, out: &mut Vec<InvalidatedVar>, nested: bool) {
    let nested = nested
        || matches!(
            node,
            Node::Function(_)
                | Node::Closure(_)
                | Node::ArrowFunction(_)
                | Node::AnonymousClass(_)
                | Node::Class(_)
                | Node::Interface(_)
                | Node::Trait(_)
                | Node::Enum(_)
        );
    let arguments = match node {
        Node::FunctionCall(c) => {
            let callee = match c.function {
                Expression::Identifier(id) => Some(name_ref(id)),
                _ => None,
            };
            Some((&c.argument_list, callee))
        }
        Node::MethodCall(c) => Some((&c.argument_list, None)),
        Node::NullSafeMethodCall(c) => Some((&c.argument_list, None)),
        Node::StaticMethodCall(c) => Some((&c.argument_list, None)),
        _ => None,
    };
    if let Some((list, callee)) = arguments {
        // One named or spread argument anywhere makes every index in the list
        // unreliable, so the verdict is taken over the whole list, not per
        // argument.
        let all_positional = list
            .arguments
            .iter()
            .all(|a| matches!(a, Argument::Positional(p) if p.ellipsis.is_none()));
        for (position, arg) in list.arguments.iter().enumerate() {
            if let Expression::Variable(Variable::Direct(dv)) = arg.value().unparenthesized() {
                let var = strip_dollar(bytes_to_string(dv.name));
                let site = match &callee {
                    Some(c) if all_positional && !nested => Some((c.clone(), position as u32)),
                    _ => None,
                };
                note_occurrence(out, var, site);
            }
        }
    }
    for child in children(node) {
        scan_invalidated(&child, out, nested);
    }
}

/// Record one occurrence of `name` on its [`InvalidatedVar`] entry (created on
/// first sight, so entries keep first-occurrence order): a provable occurrence
/// carries its `(callee, position)` site, an unprovable one (`None`) marks the
/// entry opaque. Maintained here and nowhere else — turning opaque discards
/// sites already gathered, and a site arriving after the verdict is dropped.
fn note_occurrence(out: &mut Vec<InvalidatedVar>, name: String, site: Option<(NameRef, u32)>) {
    let entry = match out.iter().position(|e| e.name == name) {
        Some(i) => &mut out[i],
        None => {
            out.push(InvalidatedVar { name, opaque: false, sites: Vec::new() });
            out.last_mut().expect("just pushed")
        }
    };
    match site {
        Some(s) if !entry.opaque => entry.sites.push(s),
        Some(_) => {}
        None => {
            entry.opaque = true;
            entry.sites.clear();
        }
    }
}

/// Collect the names of variables a subtree may **write** — over-approximated,
/// which is always sound (it only makes the walk forget more). Covers every
/// assignment lvalue, compound assignment, increment/decrement, `foreach`
/// value/key binding, `catch` parameter, and `list()`/array destructuring
/// target. Does **not** descend into nested function-like bodies (separate
/// scopes); their internal writes are not the enclosing construct's concern.
fn collect_assign_writes(node: &Node<'_, '_>, out: &mut Vec<String>) {
    match node {
        // Any direct variable in an assignment lvalue is a write target
        // (`$a[$i] = …` over-collects `$i` too — sound). Recurse into the rhs
        // for nested writes/increments; the lhs is handled here in full.
        Node::Assignment(a) => {
            collect_direct_vars(&Node::Expression(a.lhs), out);
            collect_assign_writes(&Node::Expression(a.rhs), out);
            return;
        }
        // `++$x` / `--$x` write their operand; other prefix operators do not.
        Node::UnaryPrefix(u) => {
            if matches!(
                u.operator,
                UnaryPrefixOperator::PreIncrement(_) | UnaryPrefixOperator::PreDecrement(_)
            ) {
                collect_direct_vars(&Node::Expression(u.operand), out);
            }
        }
        // `$x++` / `$x--` (the only postfix operators) write their operand.
        Node::UnaryPostfix(u) => collect_direct_vars(&Node::Expression(u.operand), out),
        // `foreach ($it as $v)` / `foreach ($it as $k => $v)` bind their targets.
        Node::ForeachValueTarget(t) => {
            collect_direct_vars(&Node::Expression(t.value), out);
            return;
        }
        Node::ForeachKeyValueTarget(t) => {
            collect_direct_vars(&Node::Expression(t.key), out);
            collect_direct_vars(&Node::Expression(t.value), out);
            return;
        }
        // `catch (T $e)` binds the exception variable; recurse into the block.
        Node::TryCatchClause(c) => {
            if let Some(v) = &c.variable {
                let name = strip_dollar(bytes_to_string(v.name));
                if !out.contains(&name) {
                    out.push(name);
                }
            }
        }
        // Nested scopes are their own concern — do not count their writes.
        Node::Function(_)
        | Node::Closure(_)
        | Node::ArrowFunction(_)
        | Node::AnonymousClass(_)
        | Node::Class(_)
        | Node::Interface(_)
        | Node::Trait(_)
        | Node::Enum(_) => return,
        _ => {}
    }
    for child in children(node) {
        collect_assign_writes(&child, out);
    }
}

/// Collect every direct variable name (`$x` → `x`) anywhere in a subtree. Used
/// for assignment-lvalue / binding positions where over-collection is intended.
fn collect_direct_vars(node: &Node<'_, '_>, out: &mut Vec<String>) {
    if let Node::DirectVariable(dv) = node {
        let name = strip_dollar(bytes_to_string(dv.name));
        if !out.contains(&name) {
            out.push(name);
        }
    }
    for child in children(node) {
        collect_direct_vars(&child, out);
    }
}

/// Collect the **read set** of an `Opaque` construct: every direct variable
/// mentioned anywhere in the subtree (conditions, call arguments, expressions)
/// that is not already a `write`. Over-collection is sound (it only forgets
/// more). Nested function-like bodies are their own scopes and are **not**
/// descended, exactly as [`collect_assign_writes`] treats them.
fn collect_read_vars(node: &Node<'_, '_>, writes: &[String], out: &mut Vec<String>) {
    match node {
        Node::DirectVariable(dv) => {
            let name = strip_dollar(bytes_to_string(dv.name));
            if !writes.contains(&name) && !out.contains(&name) {
                out.push(name);
            }
        }
        // Nested scopes are their own concern — do not read their internals.
        Node::Function(_)
        | Node::Closure(_)
        | Node::ArrowFunction(_)
        | Node::AnonymousClass(_)
        | Node::Class(_)
        | Node::Interface(_)
        | Node::Trait(_)
        | Node::Enum(_) => return,
        _ => {}
    }
    for child in children(node) {
        collect_read_vars(&child, writes, out);
    }
}

/// Collect one [`ForeachSite`] per `foreach` statement in the subtree, in source
/// order (ADR-0076 §4: **every** `foreach` is a candidate, so the transform's
/// refusal distribution measures its own narrowness).
///
/// `scope_end` is the end offset of the enclosing **variable** scope, refreshed
/// whenever the walk enters a function-like body — PHP's variable scope is the
/// function, the region an iteration variable can outlive the loop in.
///
/// Sibling order comes straight from [`Node::children`]: every statement-sequence
/// container emits its statements as consecutive `Node::Statement` children, so
/// the statement preceding a `foreach` is whichever came before it here.
fn collect_foreach_sites(node: &Node<'_, '_>, scope_end: u32, out: &mut Vec<ForeachSite>) {
    let scope_end = match node {
        Node::Function(_)
        | Node::Method(_)
        | Node::Closure(_)
        | Node::ArrowFunction(_)
        | Node::PropertyHook(_) => to_span(node.span()).end,
        _ => scope_end,
    };
    let mut prev: Option<&Statement<'_>> = None;
    for child in children(node) {
        if let Node::Statement(s) = child {
            if let Statement::Foreach(fe) = s {
                out.push(lower_foreach_site(fe, to_span(s.span()), prev, scope_end));
            }
            prev = Some(s);
        }
        collect_foreach_sites(&child, scope_end, out);
    }
}

// invalid operands (ADR-0078, issue #191)

/// Collect every arithmetic/bitwise/shift operator application in the file, in
/// pre-order (ADR-0078, issue #191). Recursion is unconditional, matching
/// [`collect_array_literal_sites`]: a site nested in a call argument, an array
/// element or a closure body is still found — `enclosing_body` on each site is
/// what keeps a closure's site from being judged against the enclosing scope's
/// env, not a truncated walk.
///
/// Pre-order plus source-ordered children means the output is sorted by span
/// start, the ordering [`SourceTree::operand_sites`] promises.
fn collect_operand_sites(node: &Node<'_, '_>, body: Option<Span>, out: &mut Vec<OperandSite>) {
    // The innermost enclosing function-like body, refreshed on entry exactly as
    // `collect_foreach_sites` refreshes `scope_end` — PHP's variable scope is
    // the function, and this field's whole job is to name that scope.
    let body = match node {
        Node::Function(_)
        | Node::Method(_)
        | Node::Closure(_)
        | Node::ArrowFunction(_)
        | Node::PropertyHook(_) => Some(to_span(node.span())),
        _ => body,
    };
    match node {
        Node::Binary(b) => {
            if let Some(op) = binary_operand_op(&b.operator) {
                out.push(OperandSite {
                    span: to_span(node.span()),
                    kind: OperandSiteKind::Binary {
                        op,
                        lhs: lower_arg_value(b.lhs),
                        rhs: lower_arg_value(b.rhs),
                    },
                    enclosing_body: body,
                });
            }
        }
        Node::UnaryPrefix(u) => {
            if let Some(op) = unary_operand_op(&u.operator) {
                out.push(OperandSite {
                    span: to_span(node.span()),
                    kind: OperandSiteKind::Unary { op, operand: lower_arg_value(u.operand) },
                    enclosing_body: body,
                });
            }
        }
        _ => {}
    }
    for child in children(node) {
        collect_operand_sites(&child, body, out);
    }
}

/// The [`BinaryOperandOp`] of a CST binary operator, or `None` for an operator
/// whose operand types PHP never refuses fatally — see [`OperandSite`] for why
/// concatenation, the comparisons and the logical operators are all `None`.
fn binary_operand_op(op: &BinaryOperator<'_>) -> Option<BinaryOperandOp> {
    match op {
        BinaryOperator::Addition(_) => Some(BinaryOperandOp::Add),
        BinaryOperator::Subtraction(_) => Some(BinaryOperandOp::Sub),
        BinaryOperator::Multiplication(_) => Some(BinaryOperandOp::Mul),
        BinaryOperator::Division(_) => Some(BinaryOperandOp::Div),
        BinaryOperator::Modulo(_) => Some(BinaryOperandOp::Mod),
        BinaryOperator::Exponentiation(_) => Some(BinaryOperandOp::Pow),
        BinaryOperator::BitwiseAnd(_) => Some(BinaryOperandOp::BitAnd),
        BinaryOperator::BitwiseOr(_) => Some(BinaryOperandOp::BitOr),
        BinaryOperator::BitwiseXor(_) => Some(BinaryOperandOp::BitXor),
        BinaryOperator::LeftShift(_) => Some(BinaryOperandOp::ShiftLeft),
        BinaryOperator::RightShift(_) => Some(BinaryOperandOp::ShiftRight),
        _ => None,
    }
}

/// The [`UnaryOperandOp`] of a CST unary prefix operator. `!`, the casts, `@`,
/// `&` and `++`/`--` are all `None` (see [`OperandSite`]).
fn unary_operand_op(op: &UnaryPrefixOperator<'_>) -> Option<UnaryOperandOp> {
    match op {
        UnaryPrefixOperator::Negation(_) => Some(UnaryOperandOp::Minus),
        UnaryPrefixOperator::Plus(_) => Some(UnaryOperandOp::Plus),
        UnaryPrefixOperator::BitwiseNot(_) => Some(UnaryOperandOp::BitNot),
        _ => None,
    }
}

// end invalid operands (ADR-0078, issue #191)

/// Collect every literal array expression in the file, file-wide, including
/// nested ones (issue #187) — recursion is unconditional, matching
/// [`collect_foreach_sites`], so an array literal nested inside another
/// array's value, a call argument, a closure body, … is still found.
fn collect_array_literal_sites(node: &Node<'_, '_>, out: &mut Vec<ArrayLiteralSite>) {
    match node {
        Node::Array(a) => out.push(lower_array_literal_site(a.elements.iter())),
        Node::LegacyArray(a) => out.push(lower_array_literal_site(a.elements.iter())),
        _ => {}
    }
    for child in children(node) {
        collect_array_literal_sites(&child, out);
    }
}

/// Lower one array literal's elements to their [`ArrayLiteralSite`] shape.
/// Purely syntactic: only the key side is resolved (`lower_array_key`'s
/// coercion); the value side is never lowered or evaluated.
fn lower_array_literal_site<'a>(
    elements: impl Iterator<Item = &'a ArrayElement<'a>>,
) -> ArrayLiteralSite {
    let elements = elements
        .map(|el| {
            let span = to_span(el.span());
            let key = match el {
                ArrayElement::Value(_) => Some(ArrayKey::Auto),
                ArrayElement::KeyValue(kv) => lower_array_key(kv.key),
                // A spread contributes an unknown number of unknown keys; a
                // destructuring hole (only ever seen in `list()` lvalue position,
                // never a legal literal) contributes none — both `None`, the same
                // "no knowable key here" the fold gate uses for an unresolvable key.
                ArrayElement::Variadic(_) | ArrayElement::Missing(_) => None,
            };
            ArrayLiteralElement { key, span }
        })
        .collect();
    ArrayLiteralSite { elements }
}

/// Lower one `foreach` into its [`ForeachSite`] shape. Purely syntactic — every
/// field is a fact about how the loop is *written*.
fn lower_foreach_site(
    fe: &mago_syntax::cst::Foreach<'_>,
    span: Span,
    prev: Option<&Statement<'_>>,
    scope_end: u32,
) -> ForeachSite {
    let target = &fe.target;
    let value = target.value();
    ForeachSite {
        span,
        subject: direct_var_name(fe.expression),
        key_binding: target.key().is_some(),
        by_ref_binding: value.is_reference(),
        // A by-ref target's operand is still a variable; the by-ref flag is the
        // refusal-bearing fact, so the name is reported either way.
        value_var: direct_var_name(strip_reference(value)),
        body: lower_foreach_body(&fe.body),
        prev_stmt: prev.map(lower_prev_stmt),
        scope_end,
    }
}

/// The variable name of an expression that is exactly `$name` (no `$`); `None`
/// for every other expression, including `$$name` and `${…}`.
fn direct_var_name(expr: &Expression<'_>) -> Option<String> {
    match expr {
        Expression::Variable(Variable::Direct(dv)) => {
            Some(strip_dollar(bytes_to_string(dv.name)))
        }
        _ => None,
    }
}

/// Peel a leading `&` off a by-reference binding target, so the bound name is
/// still readable.
fn strip_reference<'a, 'arena: 'a>(expr: &'a Expression<'arena>) -> &'a Expression<'arena> {
    match expr {
        Expression::UnaryPrefix(u) if matches!(u.operator, UnaryPrefixOperator::Reference(_)) => {
            u.operand
        }
        _ => expr,
    }
}

/// Reduce the statement preceding a `foreach` to the adjacency rule's inputs
/// (ADR-0076 §3): is it an assignment, to which variable, and is the right-hand
/// side an empty array literal?
fn lower_prev_stmt(s: &Statement<'_>) -> PrevStmt {
    let span = to_span(s.span());
    let Statement::Expression(es) = s else {
        return PrevStmt { span, assign_target: None, assigns_empty_array: false };
    };
    let Expression::Assignment(a) = es.expression else {
        return PrevStmt { span, assign_target: None, assigns_empty_array: false };
    };
    if !a.operator.is_assign() {
        return PrevStmt { span, assign_target: None, assigns_empty_array: false };
    }
    PrevStmt {
        span,
        assign_target: direct_var_name(a.lhs),
        assigns_empty_array: is_empty_array_literal(a.rhs),
    }
}

/// Whether an expression is an empty array literal — `[]` or `array()`.
fn is_empty_array_literal(expr: &Expression<'_>) -> bool {
    match expr {
        Expression::Array(a) => a.elements.is_empty(),
        Expression::LegacyArray(a) => a.elements.is_empty(),
        _ => false,
    }
}

/// Lower a `foreach` body to its [`ForeachBodyShape`].
///
/// The braced form `foreach (…) { … }` arrives as a single `Statement::Block`, so
/// the block is unwrapped: `{ $out[] = $x; }` is a **one**-statement body, not a
/// one-block one. A `Noop` (`foreach (…) ;`) is an empty body, not one-statement.
fn lower_foreach_body(body: &mago_syntax::cst::ForeachBody<'_>) -> ForeachBodyShape {
    let raw: &[Statement<'_>] = match body.statements() {
        [Statement::Block(b)] => b.statements.as_slice(),
        other => other,
    };
    let statements: Vec<&Statement<'_>> =
        raw.iter().filter(|s| !matches!(s, Statement::Noop(_))).collect();
    let append = match statements.as_slice() {
        [only] => lower_append_stmt(only),
        _ => None,
    };
    let early_exit =
        statements.iter().copied().any(|s| body_has_early_exit(&Node::Statement(s)));
    ForeachBodyShape { stmt_count: statements.len(), append, early_exit }
}

/// Lower a statement that is exactly `$acc[] = <expr>;` into an [`AppendStmt`];
/// `None` for anything else (a compound `.=`, an offset write `$acc[$k] = …`, a
/// non-variable base, a call, a nested construct).
fn lower_append_stmt(s: &Statement<'_>) -> Option<AppendStmt> {
    let Statement::Expression(es) = s else { return None };
    let Expression::Assignment(a) = es.expression else { return None };
    if !a.operator.is_assign() {
        return None;
    }
    let Expression::ArrayAppend(app) = a.lhs else { return None };
    let acc = direct_var_name(app.array)?;

    let mut value_vars = Vec::new();
    collect_direct_vars(&Node::Expression(a.rhs), &mut value_vars);
    let mut writes = Vec::new();
    collect_assign_writes(&Node::Expression(a.rhs), &mut writes);
    Some(AppendStmt {
        acc,
        value_span: to_span(a.rhs.span()),
        value_vars,
        value_writes: !writes.is_empty(),
        value_unmodelled: expr_is_unmodelled(&Node::Expression(a.rhs)),
    })
}

/// Whether a subtree carries a `break` / `continue` / `return` / `goto` that
/// belongs to the enclosing loop. Nested function-like bodies are skipped: a
/// `return` inside a closure returns from the closure, not from the loop.
fn body_has_early_exit(node: &Node<'_, '_>) -> bool {
    match node {
        Node::Break(_) | Node::Continue(_) | Node::Return(_) | Node::Goto(_) => return true,
        Node::Function(_)
        | Node::Closure(_)
        | Node::ArrowFunction(_)
        | Node::AnonymousClass(_)
        | Node::Class(_)
        | Node::Interface(_)
        | Node::Trait(_)
        | Node::Enum(_) => return false,
        _ => {}
    }
    children(node).iter().any(body_has_early_exit)
}

/// The scope-sensitive builtins whose meaning is defined by the *frame* they are
/// written in, so moving the expression into an arrow function changes what they
/// answer (ADR-0076: read as an unanalyzable call target).
const FRAME_SENSITIVE_BUILTINS: &[&str] =
    &["compact", "get_defined_vars", "func_get_args", "func_num_args"];

/// Whether an expression carries a construct the effect scan does not model as a
/// call, and which therefore cannot be shown effect-free: `new` (constructor
/// effects are not on the fixpoint), `clone` (`__clone`), `yield`, a backtick shell
/// execute, an ADR-0001 poison construct, or a frame-sensitive builtin. Nested
/// function-like bodies are descended deliberately — an arrow function in the
/// appended expression is part of what the rewrite moves.
fn expr_is_unmodelled(node: &Node<'_, '_>) -> bool {
    node_poisons(node) || scan_unmodelled(node)
}

/// The construct half of [`expr_is_unmodelled`] (the poison half runs once, over
/// the whole expression, in the caller).
fn scan_unmodelled(node: &Node<'_, '_>) -> bool {
    match node {
        Node::Instantiation(_)
        | Node::Clone(_)
        | Node::Yield(_)
        | Node::YieldFrom(_)
        | Node::YieldPair(_)
        | Node::YieldValue(_)
        | Node::ShellExecuteString(_)
        | Node::AnonymousClass(_) => return true,
        Node::FunctionCall(fc) => {
            if let Expression::Identifier(id) = fc.function {
                let name = bytes_to_string(id.last_segment()).to_ascii_lowercase();
                if FRAME_SENSITIVE_BUILTINS.contains(&name.as_str()) {
                    return true;
                }
            }
        }
        _ => {}
    }
    children(node).iter().any(scan_unmodelled)
}

/// Whether a node (scanned within a single scope, not descending into nested
/// function-like bodies) contains a construct on the ADR-0001 whole-scope
/// give-up list. Over-detection is always safe — it only silences the scope.
///
/// The predicate is `scan_opaque` asking for the first site only: one walk decides
/// poisoning and enumerates the reasons, so [`Scope::opaque`] cannot disagree with
/// [`Scope::poisoned`].
fn node_poisons(node: &Node<'_, '_>) -> bool {
    // No heap allocation on the (overwhelmingly common) clean path: `Vec::new` does
    // not allocate, and `stop_at_first` pushes at most once.
    let mut first = Vec::new();
    scan_opaque(node, &mut first, true);
    !first.is_empty()
}

/// Collect the ADR-0001 give-up-list constructs in `node`'s subtree, appending one
/// [`OpaqueSite`] per construct in source order. Nested function-like bodies are
/// their own scopes and are not descended (they get their own [`Scope`]) — a
/// closure's `use (&$x)` clause is the one exception: a by-ref capture poisons the
/// *enclosing* scope, so it is recorded here and, separately, on the closure's own
/// scope (ADR-0033).
///
/// `stop_at_first` makes the walk exit as soon as one site exists — the predicate
/// path ([`node_poisons`]), which asks only whether the scope is poisoned; the
/// inventory path passes `false` and gets every site. Both share this control flow
/// exactly, so the predicate cannot recognize a construct the inventory misses.
///
/// A matched construct is not descended into: the outermost construct is the site
/// (`extract(compact($a))` is one `extract`), where the predicate stops too.
fn scan_opaque(node: &Node<'_, '_>, out: &mut Vec<OpaqueSite>, stop_at_first: bool) {
    let direct = match node {
        // Direct markers.
        Node::Global(_) => Some(OpaqueConstruct::Global),
        Node::Static(_) => Some(OpaqueConstruct::StaticVar),
        Node::EvalConstruct(_) => Some(OpaqueConstruct::Eval),
        Node::IncludeConstruct(_)
        | Node::IncludeOnceConstruct(_)
        | Node::RequireConstruct(_)
        | Node::RequireOnceConstruct(_) => Some(OpaqueConstruct::Include),
        Node::NestedVariable(_) | Node::IndirectVariable(_) => {
            Some(OpaqueConstruct::VariableVariable)
        }
        // `extract(...)` / `compact(...)`.
        Node::FunctionCall(fc) => {
            if let Expression::Identifier(id) = fc.function {
                match bytes_to_string(id.last_segment()).as_str() {
                    "extract" => Some(OpaqueConstruct::Extract),
                    "compact" => Some(OpaqueConstruct::Compact),
                    _ => None,
                }
            } else {
                None
            }
        }
        // Reference assignment `$x = &$y`.
        Node::Assignment(a) => a.rhs.is_reference().then_some(OpaqueConstruct::ReferenceAssign),
        // Closure: inspect its `use (&$x)` capture list, but do not descend into
        // its body (a separate scope).
        Node::Closure(c) => {
            push_byref_captures(c, out, stop_at_first);
            return;
        }
        // Other nested scopes — skip entirely (their own give-up list is their
        // own concern).
        Node::Function(_)
        | Node::ArrowFunction(_)
        | Node::AnonymousClass(_)
        | Node::Class(_)
        | Node::Interface(_)
        | Node::Trait(_)
        | Node::Enum(_) => return,
        _ => None,
    };
    if let Some(construct) = direct {
        out.push(OpaqueSite { construct, span: to_span(node.span()) });
        return;
    }
    for child in children(node) {
        scan_opaque(&child, out, stop_at_first);
        if stop_at_first && !out.is_empty() {
            return;
        }
    }
}

/// Record one [`OpaqueConstruct::ByRefCapture`] site per `use (&$x)` variable of a
/// closure. Shared by the enclosing-scope walk ([`scan_opaque`]) and the closure's
/// own scope build, which is why the by-ref capture appears on both scopes — it is
/// one aliasing fact that defeats value tracking on either side of the capture.
fn push_byref_captures(
    cl: &mago_syntax::cst::Closure<'_>,
    out: &mut Vec<OpaqueSite>,
    stop_at_first: bool,
) {
    let Some(use_clause) = &cl.use_clause else { return };
    for v in use_clause.variables.iter() {
        if v.ampersand.is_some() {
            out.push(OpaqueSite {
                construct: OpaqueConstruct::ByRefCapture,
                span: to_span(v.variable.span()),
            });
            if stop_at_first {
                return;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Namespace contexts and name resolution helpers.
// ---------------------------------------------------------------------------

/// Build a [`NameRef`] from a Mago identifier: its raw spelling (leading `\`
/// stripped for fully-qualified names), the qualification [`RefKind`], and the
/// reference's byte offset (for context lookup).
fn name_ref(id: &Identifier<'_>) -> NameRef {
    let kind = match id {
        Identifier::Local(_) => RefKind::Unqualified,
        Identifier::Qualified(_) => RefKind::Qualified,
        Identifier::FullyQualified(_) => RefKind::FullyQualified,
    };
    let raw = bytes_to_string(id.value()).trim_start_matches('\\').to_owned();
    let offset = to_span(id.span()).start;
    // ADR-0049 A8: the `namespace\bar` relative form lexes as a `QualifiedIdentifier`
    // whose first segment is the reserved `namespace` keyword (never a real segment
    // name). Rewrite it to the distinct `Relative` kind, dropping the prefix, so the
    // remainder resolves against the enclosing namespace instead of being appended
    // (the doubled-prefix bug). Case-insensitive: PHP keywords fold case.
    if kind == RefKind::Qualified {
        let first_len = raw.find('\\').unwrap_or(raw.len());
        if raw[..first_len].eq_ignore_ascii_case("namespace") {
            let remainder = raw.get(first_len + 1..).unwrap_or("").to_owned();
            return NameRef { raw: remainder, kind: RefKind::Relative, offset };
        }
    }
    NameRef { raw, kind, offset }
}

/// Build the file's namespace contexts (index 0 = global) and the byte regions
/// each namespace declaration covers. Every `namespace` node in the file becomes
/// one context (its name plus the `use` imports at its body's top level);
/// top-level `use` statements outside any namespace populate the global context.
fn build_contexts(program: &Program<'_>) -> (Vec<NsCtx>, Vec<(u32, u32, usize)>) {
    let mut contexts = vec![NsCtx::global()];
    let mut regions: Vec<(u32, u32, usize)> = Vec::new();

    // Global-context imports: top-level `use` statements (a file with a
    // file-scoped `namespace A;` has none — its statements nest under the node).
    for stmt in program.statements.iter() {
        if let Statement::Use(u) = stmt {
            add_use(u, &mut contexts[0]);
        }
    }

    // One context per namespace declaration, anywhere in the tree. Namespaces do
    // not nest semantically, but a second file-scoped `namespace B;` may sit
    // inside the first's implicit body sequence; a byte offset then falls inside
    // both spans and [`ctx_of`] picks the innermost (latest-starting) region.
    collect_namespaces(&Node::Program(program), &mut contexts, &mut regions);
    (contexts, regions)
}

fn collect_namespaces(
    node: &Node<'_, '_>,
    contexts: &mut Vec<NsCtx>,
    regions: &mut Vec<(u32, u32, usize)>,
) {
    if let Node::Namespace(ns) = node {
        let name = ns
            .name
            .as_ref()
            .map(|id| bytes_to_string(id.value()).trim_start_matches('\\').to_owned())
            .unwrap_or_default();
        let mut ctx = NsCtx { namespace: name, ..NsCtx::global() };
        // `use` imports at the namespace body's top level.
        for stmt in ns.statements().iter() {
            if let Statement::Use(u) = stmt {
                add_use(u, &mut ctx);
            }
        }
        let idx = contexts.len();
        contexts.push(ctx);
        let span = to_span(ns.span());
        regions.push((span.start, span.end, idx));
    }
    for child in children(node) {
        collect_namespaces(&child, contexts, regions);
    }
}

/// Fold one `use` statement's items into a context — every import form: the plain
/// sequence (`use A\B, C\D;`), the typed sequences (`use function a\b;`,
/// `use const A\FOO;`), and the **grouped** forms (`use A\{B, C}`,
/// `use function A\{b, c}`, `use const A\{X, Y}`, and the mixed
/// `use A\{B, function c, const D}`).
///
/// Grouped imports must be lowered because an unresolved import falls back through
/// [`resolve_class_ref`] to the enclosing namespace and can collide with a
/// different class, a false positive (ADR-0049 §6). `use const` items joined the
/// same discipline with issue #198: an unlowered const import would make `FOO`
/// read as `Ns\FOO` and manufacture an absence. Their alias keys are exact-case —
/// see [`NsCtx::const_imports`].
fn add_use(u: &mago_syntax::cst::Use<'_>, ctx: &mut NsCtx) {
    match &u.items {
        UseItems::Sequence(seq) => {
            for item in seq.items.iter() {
                let target = bytes_to_string(item.name.value()).trim_start_matches('\\').to_owned();
                ctx.class_imports.insert(use_item_alias(item), target);
            }
        }
        // `use function a\b;` and `use const A\FOO, B\BAR;` (the latter, issue #198,
        // with exact-case alias keys).
        UseItems::TypedSequence(seq) => {
            let is_fn = seq.r#type.is_function();
            for item in seq.items.iter() {
                let target = bytes_to_string(item.name.value()).trim_start_matches('\\').to_owned();
                if is_fn {
                    ctx.fn_imports.insert(use_item_alias(item), target);
                } else {
                    ctx.const_imports.insert(use_item_bound_name(item), target);
                }
            }
        }
        // Grouped `use function A\{b, c}` / `use const A\{X, Y}`: one leading type
        // applies to every item under the `A\` prefix.
        UseItems::TypedList(list) => {
            let prefix = bytes_to_string(list.namespace.value());
            if list.r#type.is_function() {
                for item in list.items.iter() {
                    ctx.fn_imports.insert(use_item_alias(item), group_target(&prefix, item));
                }
            } else if list.r#type.is_const() {
                for item in list.items.iter() {
                    ctx.const_imports
                        .insert(use_item_bound_name(item), group_target(&prefix, item));
                }
            }
        }
        // Grouped `use A\{B, function c, const D}`: each item carries its own
        // optional type (`None` ⇒ class, `Function` ⇒ function, `Const` ⇒ constant).
        UseItems::MixedList(list) => {
            let prefix = bytes_to_string(list.namespace.value());
            for mti in list.items.iter() {
                let target = group_target(&prefix, &mti.item);
                match &mti.r#type {
                    None => {
                        ctx.class_imports.insert(use_item_alias(&mti.item), target);
                    }
                    Some(t) if t.is_function() => {
                        ctx.fn_imports.insert(use_item_alias(&mti.item), target);
                    }
                    Some(_) => {
                        ctx.const_imports.insert(use_item_bound_name(&mti.item), target);
                    }
                }
            }
        }
    }
}

/// The lowercase-normalized import alias for a `use` item: its explicit `as` alias,
/// else the last segment of the imported name (PHP class/function names are
/// case-insensitive, so the map keys on the lowercased form).
/// Whether a `use` statement binds the (case-sensitive) alias `PHP_VERSION_ID`
/// through any of its **const** item forms (issue #29). The exact-case binding
/// name is the explicit `as` alias, else the imported name's last segment.
fn use_binds_php_version_id(u: &mago_syntax::cst::Use<'_>) -> bool {
    use_binds_const_named(u, |bound| bound == "PHP_VERSION_ID")
}

/// The modeled `PREG_*` flag constant names (issue #168) — the four whose values
/// the out-parameter seed resolves. Kept beside the shadow scans that consult it;
/// the values live with the consumer (`steins-infer`), not here.
const PREG_FLAG_CONST_NAMES: &[&str] =
    &["PREG_PATTERN_ORDER", "PREG_SET_ORDER", "PREG_OFFSET_CAPTURE", "PREG_UNMATCHED_AS_NULL"];

/// `use const … as PREG_SET_ORDER` / `use const …\PREG_SET_ORDER` and siblings
/// (issue #168) — see [`use_binds_php_version_id`], whose rules this mirrors for
/// the modeled preg flag constants.
fn use_binds_preg_flag_const(u: &mago_syntax::cst::Use<'_>) -> bool {
    use_binds_const_named(u, |bound| PREG_FLAG_CONST_NAMES.contains(&bound))
}

/// Whether a `use` statement `use const`-imports something whose **bound name**
/// (the alias if present, else the last segment) satisfies `wanted`. Constant
/// names are case-sensitive; the match is exact. Const imports are otherwise
/// unlowered (out of scope), so the flags fed from this are the only thing read
/// from them.
fn use_binds_const_named(u: &mago_syntax::cst::Use<'_>, wanted: impl Fn(&str) -> bool) -> bool {
    let item_binds = |item: &mago_syntax::cst::UseItem<'_>| -> bool {
        let bound = match &item.alias {
            Some(a) => bytes_to_string(a.identifier.value),
            None => bytes_to_string(item.name.last_segment()),
        };
        wanted(&bound)
    };
    match &u.items {
        UseItems::TypedSequence(seq) if seq.r#type.is_const() => seq.items.iter().any(item_binds),
        UseItems::TypedList(list) if list.r#type.is_const() => list.items.iter().any(item_binds),
        UseItems::MixedList(list) => list
            .items
            .iter()
            .any(|mti| mti.r#type.as_ref().is_some_and(|t| t.is_const()) && item_binds(&mti.item)),
        _ => false,
    }
}

fn use_item_alias(item: &mago_syntax::cst::UseItem<'_>) -> String {
    match &item.alias {
        Some(a) => bytes_to_string(a.identifier.value),
        None => bytes_to_string(item.name.last_segment()),
    }
    .to_ascii_lowercase()
}

/// The **exact-case** name a `use` item binds — [`use_item_alias`]'s constant-side
/// twin (issue #198). Same rule (the explicit `as` alias, else the imported name's
/// last segment) with the lowercasing omitted, because constant names are
/// case-sensitive and `use const A\FOO;` binds `FOO`, never `foo`.
fn use_item_bound_name(item: &mago_syntax::cst::UseItem<'_>) -> String {
    match &item.alias {
        Some(a) => bytes_to_string(a.identifier.value),
        None => bytes_to_string(item.name.last_segment()),
    }
}

/// The full target FQN of a grouped-`use` item: `<prefix>\<item name>`, each side
/// trimmed of a stray leading backslash (grouped items are relative to the prefix).
fn group_target(prefix: &str, item: &mago_syntax::cst::UseItem<'_>) -> String {
    let prefix = prefix.trim_start_matches('\\');
    let name = bytes_to_string(item.name.value());
    let name = name.trim_start_matches('\\');
    format!("{prefix}\\{name}")
}

/// The namespace context enclosing `offset`: the innermost (latest-starting)
/// namespace region containing it, else the global context (index 0).
fn ctx_of<'a>(contexts: &'a [NsCtx], regions: &[(u32, u32, usize)], offset: u32) -> &'a NsCtx {
    let mut best: Option<(u32, usize)> = None;
    for &(start, end, idx) in regions {
        if offset >= start && offset < end && best.is_none_or(|(bstart, _)| start >= bstart) {
            best = Some((start, idx));
        }
    }
    &contexts[best.map_or(0, |(_, idx)| idx)]
}

/// The lowercase-normalized FQN of a declaration named `name` in context `ctx`.
fn fqn_of(ctx: &NsCtx, name: &str) -> String {
    if ctx.namespace.is_empty() {
        name.to_ascii_lowercase()
    } else {
        format!("{}\\{}", ctx.namespace, name).to_ascii_lowercase()
    }
}

/// Resolve a **class** reference to its FQN (case preserved, no leading `\`) in
/// namespace context `ctx`, applying PHP class-name resolution: fully-qualified
/// names pass through; qualified/unqualified names apply `use` class imports on
/// the first segment, else prepend the current namespace. Class references have
/// no global fallback (unlike functions), so this is a pure function of the
/// reference and its context. Shared by [`SourceTree::resolve_class_fqn`] (use-time)
/// and [`RefResolver`] (lowering-time); callers needing the normalized matching
/// key lowercase the case-preserved result.
fn resolve_class_ref(ctx: &NsCtx, r: &NameRef) -> String {
    match r.kind {
        RefKind::FullyQualified => r.raw.clone(),
        RefKind::Qualified => {
            // First segment via class/namespace imports, else current ns.
            let first_len = r.raw.find('\\').unwrap_or(r.raw.len());
            let first = &r.raw[..first_len];
            if let Some(target) = ctx.class_imports.get(&first.to_ascii_lowercase()) {
                format!("{target}{}", &r.raw[first_len..])
            } else if ctx.namespace.is_empty() {
                r.raw.clone()
            } else {
                format!("{}\\{}", ctx.namespace, r.raw)
            }
        }
        RefKind::Unqualified => {
            if let Some(target) = ctx.class_imports.get(&r.raw.to_ascii_lowercase()) {
                target.clone()
            } else if ctx.namespace.is_empty() {
                r.raw.clone()
            } else {
                format!("{}\\{}", ctx.namespace, r.raw)
            }
        }
        // ADR-0049 A8: `namespace\Bar` — the remainder resolves against the enclosing
        // namespace only, no imports (`use` never rebinds a `namespace\`-relative
        // name). In the global namespace it is the remainder itself.
        RefKind::Relative => {
            if ctx.namespace.is_empty() {
                r.raw.clone()
            } else {
                format!("{}\\{}", ctx.namespace, r.raw)
            }
        }
    }
}

/// Lowering-time namespace resolver for object type hints (ADR-0043). Carries the
/// file's namespace contexts + regions so a class/interface/enum name in a native
/// hint can be resolved to its FQN (case-preserved; lowercased by the caller into
/// the normalized matching key matching [`ClassDecl::fqn`]) at the point of
/// lowering, exactly like the FQN post-pass does for declaration names.
struct RefResolver<'a> {
    contexts: &'a [NsCtx],
    regions: &'a [(u32, u32, usize)],
}

impl RefResolver<'_> {
    /// The case-preserved (source-cased) FQN a class-name reference resolves to,
    /// in the namespace context enclosing its offset. Lowercase the result to get
    /// the normalized matching key.
    fn class_display_fqn(&self, r: &NameRef) -> String {
        resolve_class_ref(ctx_of(self.contexts, self.regions, r.offset), r)
    }
}

// ---------------------------------------------------------------------------
// Small helpers.
// ---------------------------------------------------------------------------

fn to_span(span: mago_span::Span) -> Span {
    Span { start: span.start.offset, end: span.end.offset }
}

/// The children of `node`, **or none when the stack is spent** (issue #264).
///
/// Every walker in this file descends through here, so this one function is the
/// whole depth guard for the CST walk: when [`stack_guard::exhausted`] says the
/// remaining headroom is gone, a walker is handed an empty child list and returns
/// the way it would at a leaf. No walker's control flow changes or unwinds, and
/// the parse still produces a (partial) tree, which [`SourceTree::parse`] then
/// reports as a recovered parse error rather than letting the process (or the
/// wasm module) die walking it.
///
/// On every native target the guard is off by default and this is
/// `node.children()` behind one thread-local read; see [`stack_guard`].
fn children<'ast, 'arena>(node: &Node<'ast, 'arena>) -> Vec<Node<'ast, 'arena>> {
    if stack_guard::exhausted() {
        return Vec::new();
    }
    node.children()
}

/// Lower one trivium to a [`Comment`], dropping whitespace trivia (`None`).
fn lower_comment(t: &Trivia<'_>) -> Option<Comment> {
    let kind = match t.kind {
        TriviaKind::SingleLineComment => CommentKind::Line,
        TriviaKind::HashComment => CommentKind::Hash,
        TriviaKind::MultiLineComment => CommentKind::Block,
        TriviaKind::DocBlockComment => CommentKind::DocBlock,
        TriviaKind::WhiteSpace => return None,
    };
    Some(Comment { kind, span: to_span(t.span), text: bytes_to_string(t.value) })
}

fn bytes_to_string(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn strip_dollar(name: String) -> String {
    name.strip_prefix('$').map_or(name.clone(), ToOwned::to_owned)
}

fn line_starts(source: &str) -> Vec<u32> {
    let mut starts = vec![0u32];
    for (i, b) in source.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i as u32 + 1);
        }
    }
    starts
}
