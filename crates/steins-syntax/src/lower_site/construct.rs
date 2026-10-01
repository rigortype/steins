//! The construct sites: output, exit, `eval`, file inclusion, the structural
//! state constructs, and a `match` with no `default`.

use mago_span::HasSpan;
use mago_syntax::cst::{Match, MatchArm, Node, Variable};

use super::{SiteScope, scan_sites};
use crate::ast::{
    ConstructKind, ExitKeyword, IncludeKeyword, OutputKeyword, SiteKind, SiteOrigin, StateConstruct,
};
use crate::lower_effect::{is_superglobal, property_write_span};
use crate::to_span;

/// A `match` with no `default` arm, which can raise `\UnhandledMatchError`
/// (ADR-0031 Part B). The arms are walked by the caller.
pub(super) fn match_site(m: &Match<'_>, sx: &SiteScope<'_>, out: &mut Vec<SiteOrigin>) {
    if !m.arms.iter().any(MatchArm::is_default) {
        let kind = SiteKind::Construct(ConstructKind::MatchNoDefault);
        out.push(sx.site(to_span(m.span()), kind));
    }
}

/// Record the construct `node` is, if it is one. Returns `true` when this
/// already walked the node's children, which happens for one shape: a static
/// property's name is a variable token, and `Foo::$_GET` names a property
/// rather than the superglobal, so only the class expression — and a dynamic
/// name's expression — is walked on.
pub(super) fn lower(node: &Node<'_, '_>, sx: &SiteScope<'_>, out: &mut Vec<SiteOrigin>) -> bool {
    if let Some((span, kind)) = statement_construct(node) {
        out.push(sx.site(to_span(span), SiteKind::Construct(kind)));
        return false;
    }
    state(node, sx, out)
}

/// The output, exit, `eval` or inclusion construct `node` is, with its span.
fn statement_construct(node: &Node<'_, '_>) -> Option<(mago_span::Span, ConstructKind)> {
    let include = |keyword, span| Some((span, ConstructKind::Include(keyword)));
    match node {
        Node::Echo(e) => Some((e.span(), ConstructKind::Output(OutputKeyword::Echo))),
        Node::EchoTag(e) => Some((e.span(), ConstructKind::Output(OutputKeyword::Echo))),
        Node::PrintConstruct(p) => Some((p.span(), ConstructKind::Output(OutputKeyword::Print))),
        // Raw text between `?>` and the next `<?php` inside a body: the engine writes
        // it to the output channel exactly as `echo` does (ADR-0083). Whitespace-only
        // inline text is skipped — layout punctuation between tag pairs isn't output
        // anyone writes a function for.
        Node::Inline(i) if i.kind.is_text() && !i.value.iter().all(u8::is_ascii_whitespace) => {
            Some((i.span(), ConstructKind::Output(OutputKeyword::InlineHtml)))
        }
        Node::ExitConstruct(x) => Some((x.span(), ConstructKind::Exit(ExitKeyword::Exit))),
        Node::DieConstruct(d) => Some((d.span(), ConstructKind::Exit(ExitKeyword::Die))),
        // Dynamic code (ADR-0046 amendment): `eval` is its own label, a file
        // inclusion reads a file. Both run code this scan never sees; the operand
        // is still walked — `eval(f())` calls `f`.
        Node::EvalConstruct(ec) => Some((ec.span(), ConstructKind::Eval)),
        Node::IncludeConstruct(ic) => include(IncludeKeyword::Include, ic.span()),
        Node::IncludeOnceConstruct(ic) => include(IncludeKeyword::IncludeOnce, ic.span()),
        Node::RequireConstruct(rq) => include(IncludeKeyword::Require, rq.span()),
        Node::RequireOnceConstruct(rq) => include(IncludeKeyword::RequireOnce, rq.span()),
        _ => None,
    }
}

/// Record the [`StateConstruct`] `node` is, if it is one (ADR-0055 amendment of
/// 2026-09-26). See [`lower`] for the return value.
fn state(node: &Node<'_, '_>, sx: &SiteScope<'_>, out: &mut Vec<SiteOrigin>) -> bool {
    match node {
        Node::Global(g) => push_state(StateConstruct::Global, g.span(), sx, out),
        Node::Static(s) => push_state(StateConstruct::StaticVar, s.span(), sx, out),
        Node::DirectVariable(dv) if is_superglobal(dv.name) => {
            push_state(StateConstruct::Superglobal, dv.span(), sx, out);
        }
        Node::StaticPropertyAccess(spa) => {
            push_state(StateConstruct::StaticProperty, spa.span(), sx, out);
            scan_sites(&Node::Expression(spa.class), sx, out);
            if !matches!(spa.property, Variable::Direct(_)) {
                scan_sites(&Node::Variable(&spa.property), sx, out);
            }
            return true;
        }
        _ => {
            if let Some(span) = property_write_span(node, sx.cx.constructor) {
                push_state(StateConstruct::PropertyWrite, span, sx, out);
            }
        }
    }
    false
}

/// Append a [`StateConstruct`] site at `span`.
fn push_state(
    construct: StateConstruct,
    span: mago_span::Span,
    sx: &SiteScope<'_>,
    out: &mut Vec<SiteOrigin>,
) {
    let kind = SiteKind::Construct(ConstructKind::State(construct));
    out.push(sx.site(to_span(span), kind));
}
