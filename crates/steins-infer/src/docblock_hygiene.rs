//! Docblock hygiene (ADR-0078 / issue #186): the mechanics-layer anti-rot family —
//! `phpdoc.unparsable`, stale `@param` / `@var`, misplaced `@var`, non-throwable
//! `@throws`, unused closure `use`. Every check is textual: it asks whether the
//! subject a tag names still exists, never what it means.

use steins_phpdoc::{DocTag, TagKind, parse_type, scan_docblock};
use steins_syntax::{
    Comment, CommentKind, OpaqueConstruct, Param, Scope, ScopeOwner, Stmt, StmtKind,
};

use crate::{
    CLOSURE_UNUSED_USE_ID, Cx, Diagnostic, IsA, PHPDOC_MISPLACED_VAR_ID, PHPDOC_STALE_PARAM_ID,
    PHPDOC_STALE_VAR_ID, PHPDOC_THROWS_NOT_THROWABLE_ID, PHPDOC_UNPARSABLE_ID, parse_tag_type,
};
use crate::throws::{collect_class_names, resolve_class_name};

// ---------------------------------------------------------------------------
// Docblock hygiene (ADR-0078 / issue #186): the mechanics-layer anti-rot family.
//
// Every check below is TEXTUAL — it asks whether the subject a tag names still
// exists, never what it means — which is what earns the mechanics layer's
// red-on-sight posture. The bounded-tag-set discipline holds throughout: an
// unrecognized/vendor tag never reaches `scan_docblock`'s output at all, so it
// can never be a finding; only rot inside the read set is.
// ---------------------------------------------------------------------------

/// The file's docblock-hygiene findings, run once per file from `check_units`.
pub(crate) fn docblock_hygiene(cx: &Cx, out: &mut Vec<Diagnostic>) {
    for comment in cx.tree().comments() {
        if comment.kind != CommentKind::DocBlock {
            continue;
        }
        let tags = scan_docblock(&comment.text);
        for tag in &tags {
            check_unparsable_tag(cx, comment, tag, out);
            check_throws_not_throwable(cx, comment, tag, out);
        }
        check_misplaced_var(cx, comment, &tags, out);
    }
    check_stale_params(cx, out);
    check_stale_vars(cx, out);
    check_unused_uses(cx, out);
}

/// One docblock-hygiene diagnostic, positioned at a docblock-relative tag offset.
pub(crate) fn hygiene_diag(cx: &Cx, id: &'static str, offset: u32, message: String) -> Diagnostic {
    let pos = cx.tree().position(offset);
    Diagnostic {
        id,
        path: cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        message,
        facet: None,
        fix: None,
    }
}

/// The tag's bare spelling (`param`, `return`, `var`, `throws`) for a message.
/// `None` for every other member of the read set — the hygiene family judges only
/// the four type-carrying tags.
fn hygiene_tag_name(kind: TagKind) -> Option<&'static str> {
    match kind {
        TagKind::Param => Some("param"),
        TagKind::Return => Some("return"),
        TagKind::Var => Some("var"),
        TagKind::Throws => Some("throws"),
        _ => None,
    }
}

/// Whether every bracket pair in a type payload closes. An **unbalanced** payload
/// is the line-wrapped array-shape spelling (`@param array{` continued on the next
/// physical line), which the line-based docblock scanner truncates — a parser
/// limitation, never rot, so `phpdoc.unparsable` stays silent on it.
fn brackets_balanced(s: &str) -> bool {
    let mut depth = [0i32; 4];
    for ch in s.chars() {
        let (slot, delta) = match ch {
            '{' => (0, 1),
            '}' => (0, -1),
            '<' => (1, 1),
            '>' => (1, -1),
            '(' => (2, 1),
            ')' => (2, -1),
            '[' => (3, 1),
            ']' => (3, -1),
            _ => continue,
        };
        depth[slot] += delta;
        if depth[slot] < 0 {
            return false;
        }
    }
    depth.iter().all(|&d| d == 0)
}

/// A tag's **whole payload** — everything after the tag name, exactly as written.
///
/// `DocTag::type_text` is not that: the docblock scanner cuts the type region at
/// the first `$name`, and for a `callable(CallbackInput $input): bool $callback`
/// spelling that `$name` sits *inside the type*, leaving `type_text` truncated to
/// an unbalanced `"callable(CallbackInput"`. Every hygiene check therefore reads
/// the payload and lets `parse_type` — which parses a prefix and reports how far
/// it got — decide where the type ends.
fn tag_payload<'a>(doc_text: &'a str, tag: &DocTag) -> Option<&'a str> {
    doc_text.get(tag.type_span.start as usize..tag.tag_span.end as usize)
}

/// `phpdoc.unparsable`: a read-set tag whose type payload the parser rejects, so
/// the annotation declares nothing at all.
///
/// The parser reads a prefix, so a trailing description is never the reason for a
/// rejection — only a payload whose *opening* cannot start a type is. An
/// unbalanced payload is skipped ([`brackets_balanced`]): that is the line-wrapped
/// array-shape spelling the line-based scanner truncates, a parser limitation
/// rather than rot.
fn check_unparsable_tag(cx: &Cx, comment: &Comment, tag: &DocTag, out: &mut Vec<Diagnostic>) {
    let Some(name) = hygiene_tag_name(tag.kind) else { return };
    let Some(payload) = tag_payload(&comment.text, tag).map(str::trim) else { return };
    if payload.is_empty() || !brackets_balanced(payload) {
        return;
    }
    let Err(err) = parse_type(payload) else { return };
    out.push(hygiene_diag(
        cx,
        PHPDOC_UNPARSABLE_ID,
        comment.span.start + tag.tag_span.start,
        format!("`@{name} {payload}` does not parse ({}) — the tag declares nothing", err.message),
    ));
}

/// `phpdoc.throws-not-throwable`: a `@throws` naming a class-like whose hierarchy
/// is fully enumerable and holds no `Throwable`. An unresolvable or incompletely
/// enumerable name is `IsA::Unknown` — silence, the absence-family condition.
fn check_throws_not_throwable(cx: &Cx, comment: &Comment, tag: &DocTag, out: &mut Vec<Diagnostic>) {
    if tag.kind != TagKind::Throws {
        return;
    }
    let Some(ty) = parse_tag_type(&tag.type_text) else { return };
    let mut names: Vec<String> = Vec::new();
    collect_class_names(&ty, &mut |n| names.push(n.to_owned()));
    let offset = comment.span.start + tag.tag_span.start;
    for name in names {
        let fqn = resolve_class_name(cx, offset, &name);
        if cx.is_a(&fqn, "Throwable") != IsA::No {
            continue;
        }
        out.push(hygiene_diag(
            cx,
            PHPDOC_THROWS_NOT_THROWABLE_ID,
            offset,
            format!("`@throws {name}` names {fqn}, which is not a Throwable — it can never be thrown"),
        ));
    }
}

/// `phpdoc.misplaced-var`: a `@var` docblock nothing can adopt (ADR-0073's rule
/// has no eligible follower at all). Reported once per docblock, at its first
/// `@var` tag. The property-`@var` position always has its declaration following
/// it, so it never reaches here.
fn check_misplaced_var(cx: &Cx, comment: &Comment, tags: &[DocTag], out: &mut Vec<Diagnostic>) {
    let Some(tag) = tags.iter().find(|t| t.kind == TagKind::Var) else { return };
    if !cx.tree().docblock_adopts_nothing(comment.span.end) {
        return;
    }
    out.push(hygiene_diag(
        cx,
        PHPDOC_MISPLACED_VAR_ID,
        comment.span.start + tag.tag_span.start,
        "`@var` sits where nothing adopts it — no declaration or statement follows".to_owned(),
    ));
}

/// `phpdoc.stale-param`: every function-like's `@param $name` checked against its
/// own parameter list. Variadic (`...$args`) and by-ref (`&$x`) spellings name a
/// real parameter, so they are matched by name like any other.
fn check_stale_params(cx: &Cx, out: &mut Vec<Diagnostic>) {
    let tree = cx.tree();
    for f in tree.functions() {
        stale_params_of(cx, f.docblock.as_deref(), f.docblock_span.map(|s| s.start), f.span.start, &f.params, &format!("{}()", f.name), out);
    }
    for c in tree.classes() {
        for m in &c.methods {
            stale_params_of(cx, m.docblock.as_deref(), m.docblock_span.map(|s| s.start), m.span.start, &m.params, &format!("{}::{}()", c.name, m.name), out);
        }
    }
    for scope in tree.scopes() {
        let ScopeOwner::Closure { def_offset } = scope.owner else { continue };
        // A closure's adopted docblock carries no span (it is looked up by
        // definition offset), so the finding is positioned at the closure head.
        stale_params_of(cx, scope.docblock.as_deref(), None, def_offset, &scope.params, "the closure", out);
    }
}

/// The `$name` a `@param` tag actually takes as its **subject**: the variable
/// token that follows the type *expression*.
///
/// This is why the check reads the payload rather than `DocTag::var_name`. The
/// docblock scanner records the first `$name` it sees, and in
/// `@param callable(CallbackInput $input): bool $callback` that is `$input` — a
/// parameter of the callable *type*, not of the annotated signature (measured on
/// phpunit's `Framework/Constraint/Callback.php`). Reading the first variable
/// **past the parse's extent** puts the boundary where the type grammar puts
/// it, so a `$name` inside `callable(…)` / `\Closure(…)` parens can never be
/// the subject.
///
/// Two guards keep a *multiline* type from ever convicting, because the
/// line-based scanner hands this a truncated payload and `parse_type`'s
/// tolerance can hide the truncation:
///
/// 1. **The payload must be bracket-balanced** — the same guard
///    `phpdoc.unparsable` applies, applied symmetrically. An unbalanced payload is
///    a type continued on the next physical line
///    (`@phpstan-param callable(array<string,string> $params): array{` …), which
///    the scanner never reassembles.
/// 2. **The subject must sit at bracket depth 0** past the parse's extent. The
///    `callable(…)` / `\Closure(…)` signature form is all-or-nothing: when the
///    whole `(params): ret` cannot parse — a missing return type, or one truncated
///    mid-shape — the parser BACKTRACKS to the bare identifier and reports
///    `consumed = 8`, leaving the entire parameter list unconsumed. Trusting
///    `consumed` alone then reads the type's own `$params` as the subject.
///
/// A payload the parser rejects yields `None` too — silence here, because that
/// case belongs to `phpdoc.unparsable` (and only within its balanced narrowing).
pub(crate) fn param_subject(doc_text: &str, tag: &DocTag) -> Option<String> {
    let payload = tag_payload(doc_text, tag)?;
    if !brackets_balanced(payload) {
        return None;
    }
    let parsed = parse_type(payload).ok()?;
    top_level_variable_token(payload.get(parsed.consumed as usize..)?)
}

/// The first `$name` token in `s` that is **not nested inside a bracket**, without
/// its `$`. `None` when there is none (a bare `@param T` names no subject), and
/// `None` the moment the scan closes a bracket it never opened — that means the
/// scan began *inside* a bracketed construct, so nothing here is a subject.
fn top_level_variable_token(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut depth = 0i32;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'(' | b'[' | b'{' | b'<' => depth += 1,
            b')' | b']' | b'}' | b'>' => {
                depth -= 1;
                if depth < 0 {
                    return None;
                }
            }
            b'$' if depth == 0 => {
                let mut end = i + 1;
                while bytes
                    .get(end)
                    .is_some_and(|&b| b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80)
                {
                    end += 1;
                }
                if end > i + 1 {
                    return Some(s[i + 1..end].to_owned());
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// The per-declaration leg of [`check_stale_params`]. `doc_start` is the
/// docblock's file offset when one is recorded (the finding then lands on the tag
/// itself); `anchor` is the fallback position.
fn stale_params_of(
    cx: &Cx,
    docblock: Option<&str>,
    doc_start: Option<u32>,
    anchor: u32,
    params: &[Param],
    display: &str,
    out: &mut Vec<Diagnostic>,
) {
    let Some(text) = docblock else { return };
    for tag in scan_docblock(text) {
        if tag.kind != TagKind::Param {
            continue;
        }
        // A `@param` with no subject token names nothing — not this finding.
        let Some(name) = param_subject(text, &tag) else { continue };
        if name == "this" {
            continue;
        }
        if params.iter().any(|p| p.name == name) {
            continue;
        }
        let offset = doc_start.map_or(anchor, |s| s + tag.tag_span.start);
        out.push(hygiene_diag(
            cx,
            PHPDOC_STALE_PARAM_ID,
            offset,
            format!("`@param ${name}` names no parameter of {display}"),
        ));
    }
}

/// `phpdoc.stale-var`: an adopted statement-level `@var` naming a variable that
/// **does not exist** — neither bound by the statement it leads nor occurring
/// anywhere before it. A typo (`@var Echo_ $ecoh`), not a mismatch.
///
/// The narrower claim is ADR-0073's, not PHPStan's. Under §2 the cast re-declares
/// *the variable the tag names*, whatever the adopted statement binds — re-typing
/// an already-bound variable is the feature's whole point — and §4 defers the
/// assignment form as a **silence** (the rebind erases the cast), never as rot.
/// So `/** @var Echo_ $echo */ $dnumber = $echo->exprs[0];` is legal here, and a
/// docblock naming several in-scope variables is too. PHPStan's
/// `varTag.differentVariable` is a different tool's semantics and is not ported.
///
/// Adoption is `SourceTree::stmt_docblock`, the ADR-0073/0074 rule verbatim. The
/// firing shape is the plain assignment, the one statement whose bound name is a
/// syntactic fact; every other statement kind stays silent. A scope that can
/// mint names (`extract`/`compact`/`$$x`/`eval`/`include`) is silent throughout
/// — the same dam `closure.unused-use` applies.
fn check_stale_vars(cx: &Cx, out: &mut Vec<Diagnostic>) {
    for scope in cx.tree().scopes() {
        if scope_mints_names(scope) {
            continue;
        }
        stale_vars_in_trace(cx, &scope.stmts, out);
    }
}

/// Whether a scope holds a construct that can bring a name into being without
/// spelling it — the scope-local dam the `@var` and capture checks share.
fn scope_mints_names(scope: &Scope) -> bool {
    scope.opaque.iter().any(|site| {
        matches!(
            site.construct,
            OpaqueConstruct::Extract
                | OpaqueConstruct::Compact
                | OpaqueConstruct::VariableVariable
                | OpaqueConstruct::Eval
                | OpaqueConstruct::Include
        )
    })
}

/// Walk a trace (descending the structured `if`/`match` sub-traces) and judge each
/// plain assignment's adopted `@var`.
fn stale_vars_in_trace(cx: &Cx, stmts: &[Stmt], out: &mut Vec<Diagnostic>) {
    for stmt in stmts {
        match &stmt.kind {
            StmtKind::Assign { var, .. } => stale_var_on_assign(cx, stmt, var, out),
            StmtKind::If { then_trace, elseifs, else_trace, .. } => {
                stale_vars_in_trace(cx, then_trace, out);
                for (_, branch) in elseifs {
                    stale_vars_in_trace(cx, branch, out);
                }
                if let Some(branch) = else_trace {
                    stale_vars_in_trace(cx, branch, out);
                }
            }
            StmtKind::Match { arms, default, .. } => {
                for arm in arms {
                    stale_vars_in_trace(cx, &arm.trace, out);
                }
                if let Some(branch) = default {
                    stale_vars_in_trace(cx, branch, out);
                }
            }
            _ => {}
        }
    }
}

/// The one firing shape: `/** @var T $y */ $x = …;` where `$y` is neither `$x`
/// nor a spelling that occurs anywhere earlier — a name with no referent at all.
fn stale_var_on_assign(cx: &Cx, stmt: &Stmt, bound: &str, out: &mut Vec<Diagnostic>) {
    let Some(doc) = cx.tree().stmt_docblock(stmt.span.start) else { return };
    for tag in scan_docblock(&doc.text) {
        if tag.kind != TagKind::Var || tag.property_target {
            continue;
        }
        // A bare `@var T` speaks about the statement's own binding — never stale.
        let Some(raw) = tag.var_name.as_deref() else { continue };
        let name = raw.trim_start_matches('$');
        if name.is_empty() || name == "this" || name == bound {
            continue;
        }
        // The ADR-0073 cast re-declares an already-bound variable, so a name that
        // exists before the tag is a legitimate cast however the statement below it
        // assigns. Only a name that appears NOWHERE earlier has no referent.
        if cx.tree().variable_mentioned_before(name, doc.span.start) {
            continue;
        }
        out.push(hygiene_diag(
            cx,
            PHPDOC_STALE_VAR_ID,
            doc.span.start + tag.tag_span.start,
            format!("`@var ${name}` names a variable that appears nowhere before this statement in the scope"),
        ));
    }
}

/// `closure.unused-use`: a by-value `use ($x)` the closure body never mentions.
/// The firing set is computed at lowering (`Scope::unused_captures`), where the
/// by-ref out-channel, the nested-closure mention and the `compact`/`extract`/
/// `$$x`/`eval`/`include` dam are all already accounted for.
fn check_unused_uses(cx: &Cx, out: &mut Vec<Diagnostic>) {
    for scope in cx.tree().scopes() {
        for capture in &scope.unused_captures {
            out.push(hygiene_diag(
                cx,
                CLOSURE_UNUSED_USE_ID,
                capture.span.start,
                format!("`use (${})` is never read in the closure body", capture.name),
            ));
        }
    }
}
