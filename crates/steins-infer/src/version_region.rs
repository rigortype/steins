//! Version-dependent regions: the branches of an `if` whose condition reads the
//! engine's version and that the declared target range does not decide.
//!
//! A branch such as `if (PHP_VERSION_ID < 80000) { mktime(); }` runs on an older PHP
//! only. The analysing PHP (8.5) reflects the signatures of the NEWER engine, so a
//! verdict about a call inside the branch is about a PHP the branch never runs on.
//! The families that judge a call from the analysing PHP's reflection (internal
//! arity, engine methods, callbacks) ask [`in_version_dependent_region`] and decline.
//!
//! A condition reads the version when it mentions `PHP_VERSION_ID`,
//! `PHP_MAJOR_VERSION`, `PHP_MINOR_VERSION`, `PHP_RELEASE_VERSION` or `PHP_VERSION`,
//! or calls `version_compare`. The one comparison the target range can decide, the
//! `PHP_VERSION_ID` against an int literal ([`eval_version_id_cmp`]), is not a region
//! when it is decided: the arm that cannot run on any target is a region (the walk
//! skips its statements but still visits the closures written in it), the other is live
//! on every target. Every other read is undecided by construction (the walk does not fold it).
//! Every arm of an `if`/`elseif`/`else` chain with one such condition is a region,
//! since each arm's liveness depends on every earlier condition.
//!
//! Expression-level constructs are regions too; the table is on
//! [`in_version_dependent_region`].

use steins_domain::Certainty;
use steins_syntax::{ArgValue, CallExpr, CondExpr, CondOperand, Span, Stmt, StmtKind};

use crate::cond::eval_version_id_cmp;
use crate::cx::Cx;

const VERSION_CONSTS: [&str; 5] = [
    "PHP_VERSION_ID",
    "PHP_MAJOR_VERSION",
    "PHP_MINOR_VERSION",
    "PHP_RELEASE_VERSION",
    "PHP_VERSION",
];

fn is_version_const(raw: &str) -> bool {
    VERSION_CONSTS.contains(&raw.trim_start_matches('\\'))
}

fn call_reads_version(call: &CallExpr) -> bool {
    call.callee.as_deref().is_some_and(|c| c.eq_ignore_ascii_case("version_compare"))
        || call
            .args
            .iter()
            .any(|a| matches!(&a.value, ArgValue::GlobalConst(r) if is_version_const(&r.raw)))
}

fn operand_reads_version(op: &CondOperand) -> bool {
    match op {
        CondOperand::Const(r) => is_version_const(&r.raw),
        CondOperand::Other { call: Some(call), .. } => call_reads_version(call),
        _ => false,
    }
}

/// Whether `cond` reads the version in a way the target range leaves undecided.
fn cond_undecided_version(cx: &Cx, cond: &CondExpr) -> bool {
    match cond {
        CondExpr::Cmp { op, lhs, rhs } => {
            if !(operand_reads_version(lhs) || operand_reads_version(rhs)) {
                return false;
            }
            // A decided `PHP_VERSION_ID <op> literal` is not a region.
            !matches!(
                eval_version_id_cmp(cx, *op, lhs, rhs),
                Some(Certainty::Yes | Certainty::No)
            )
        }
        CondExpr::Truthy(op) => operand_reads_version(op),
        CondExpr::Not(c) => cond_undecided_version(cx, c),
        CondExpr::And(a, b, _) | CondExpr::Or(a, b, _) => {
            cond_undecided_version(cx, a) || cond_undecided_version(cx, b)
        }
        CondExpr::Call { call, .. } => call_reads_version(call),
        _ => false,
    }
}

/// The source extent a trace covers, or `None` for an empty one.
fn trace_span(trace: &[Stmt]) -> Option<Span> {
    Some(Span { start: trace.first()?.span.start, end: trace.last()?.span.end })
}

/// The verdict the target range gives a plain `PHP_VERSION_ID <op> literal` condition.
fn decided(cx: &Cx, cond: &CondExpr) -> Option<Certainty> {
    let CondExpr::Cmp { op, lhs, rhs } = cond else { return None };
    eval_version_id_cmp(cx, *op, lhs, rhs).filter(|v| *v != Certainty::Maybe)
}

fn collect(cx: &Cx, stmts: &[Stmt], regions: &mut Vec<Span>) {
    for stmt in stmts {
        if let StmtKind::If { cond, then_trace, elseifs, else_trace } = &stmt.kind {
            let rest = || {
                elseifs.iter().map(|(_, t)| t.as_slice()).chain(else_trace.as_deref())
            };
            match decided(cx, cond) {
                // The range fixes the condition: only the arm that cannot run on any
                // target is a region (its closures are scopes the walk still visits).
                Some(Certainty::Yes) => regions.extend(rest().filter_map(trace_span)),
                Some(_) => regions.extend(trace_span(then_trace)),
                None if cond_undecided_version(cx, cond)
                    || elseifs.iter().any(|(c, _)| cond_undecided_version(cx, c)) =>
                {
                    regions.extend(stmt.kind.sub_traces().into_iter().filter_map(trace_span));
                }
                None => {}
            }
        }
        for sub in stmt.kind.sub_traces() {
            collect(cx, sub, regions);
        }
    }
}

/// Words that make what follows conditional on what precedes them.
const WORD_OPERATORS: [&str; 4] = ["and", "or", "xor", "case"];

/// Whether `word` occurs in `text` as a whole word.
fn has_word(text: &str, word: &str) -> bool {
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    text.match_indices(word).any(|(i, _)| {
        !text[..i].chars().next_back().is_some_and(is_ident)
            && !text[i + word.len()..].chars().next().is_some_and(is_ident)
    })
}

/// Whether `text` holds a conditional operator: `?` (ternary, `?:`), `&&`, `||`, `=>`
/// (a `match` arm), a single `:` (a ternary else, a `switch` label), or `and`/`or`/
/// `xor`/`case`.
fn has_conditional_operator(text: &str) -> bool {
    let b = text.as_bytes();
    let lone_colon = b.iter().enumerate().any(|(i, &c)| {
        c == b':' && b.get(i + 1) != Some(&b':') && (i == 0 || b[i - 1] != b':')
    });
    text.contains('?')
        || text.contains("&&")
        || text.contains("||")
        || text.contains("=>")
        || lone_colon
        || WORD_OPERATORS.iter().any(|w| has_word(text, w))
}

/// Whether `text` reads the version and, after its LAST read, passes a conditional
/// operator: what follows runs only on some versions.
fn text_is_version_conditional(text: &str) -> bool {
    let last_read = ["PHP_VERSION", "PHP_MAJOR_VERSION", "PHP_MINOR_VERSION", "PHP_RELEASE_VERSION"]
        .iter()
        .filter_map(|t| text.rfind(t))
        .chain(text.rfind("version_compare"))
        .max();
    last_read.is_some_and(|i| has_conditional_operator(&text[i..]))
}

/// The expression-level rule, read from the source: a call after a version read and a
/// conditional operator in the same statement (or the same gap between a container's
/// branches). The tree keeps no expression spans, so the text between the read and the
/// call is scanned; the cost is over-silence in an exotic statement, never a finding.
fn lexical_region(cx: &Cx, stmts: &[Stmt], offset: u32) -> bool {
    for stmt in stmts {
        if !(stmt.span.start <= offset && offset < stmt.span.end) {
            continue;
        }
        let subs = stmt.kind.sub_traces();
        let spans: Vec<Span> = subs.iter().filter_map(|t| trace_span(t)).collect();
        let inside = spans.iter().any(|r| r.start <= offset && offset < r.end);
        let from = match &stmt.kind {
            // A leaf statement: its whole text up to the call.
            _ if spans.is_empty() => Some(stmt.span.start),
            // A `match`/`switch`: every earlier arm label decides a later arm.
            StmtKind::Match { .. } => Some(stmt.span.start),
            // An `if`: the branches are the structural rule's; its conditions are the
            // gaps between them.
            StmtKind::If { .. } if !inside => {
                spans.iter().map(|r| r.end).filter(|&e| e <= offset).max().or(Some(stmt.span.start))
            }
            _ => None,
        };
        if let Some(from) = from
            && cx
                .tree()
                .source_slice(Span { start: from.min(offset), end: offset })
                .is_some_and(text_is_version_conditional)
        {
            return true;
        }
        if subs.iter().any(|t| lexical_region(cx, t, offset)) {
            return true;
        }
    }
    false
}

/// Whether the byte `offset` of the current file lies in a part of the code whose
/// liveness depends on the PHP version the target range does not fix.
///
/// | construct                                  | version-dependent part               | rule        |
/// |--------------------------------------------|--------------------------------------|-------------|
/// | `if (V) A elseif (V2) B else C`            | every arm (the dead arm only, when   | structural  |
/// |                                            | the range decides a `PHP_VERSION_ID`)|             |
/// | `V ? A : B`, `V ?: B`                      | A and B                              | lexical     |
/// | `V && X`, `V and X`, `V \|\| X`, `V or X` | X                                    | lexical     |
/// | `match (true) { V => A, default => B }`    | A, and every later arm               | lexical     |
/// | `switch (true) { case V: A }`              | the statements after the label       | lexical     |
/// | a closure written in any of the above      | its whole body                       | both        |
/// | the condition `V` itself, a call before the operator | none (runs on every target) | -           |
///
/// `V` reads `PHP_VERSION_ID`, `PHP_MAJOR_VERSION`, `PHP_MINOR_VERSION`,
/// `PHP_RELEASE_VERSION`, `PHP_VERSION` or `version_compare`. The structural rule
/// consults the target range; the lexical rule does not (a decided ternary stays silent
/// too), because the tree records no expression spans to fold against.
///
/// Computed on demand: the callers ask only when about to report.
pub(crate) fn in_version_dependent_region(cx: &Cx, offset: u32) -> bool {
    let mut regions = Vec::new();
    for scope in cx.tree().scopes() {
        collect(cx, &scope.stmts, &mut regions);
        if lexical_region(cx, &scope.stmts, offset) {
            return true;
        }
    }
    regions.iter().any(|r| r.start <= offset && offset < r.end)
}
