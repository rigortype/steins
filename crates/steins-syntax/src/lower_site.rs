//! The one site scan over a function-like body: every call-like and
//! construct-like [`SiteOrigin`] the effect lane and the throw lane read,
//! lowered once.
//!
//! `scan_effect_origins` and `scan_throw_origins` (`lower_effect.rs`) walk the
//! same tree and disagree in what they record; this walk records the union, so a
//! new kind of site is added in one place. Each lane's legacy origin list is a
//! view of the sites ([`derive_effect_origins`], [`derive_throw_origins`]), and
//! the `site_oracle` test holds the views equal to the lists the legacy scans
//! still produce.
//!
//! The traversal is the legacy scans' own: pre-order over [`children`], not
//! descending into nested scopes, with the `try`/`catch` guard stack and the
//! catch-variable scope the throw scan threads through it.

mod call;
mod construct;
mod derive;
mod throw;

use mago_syntax::cst::Node;

use crate::ast::{CatchClause, ConstArgs, NameRef, SiteKind, SiteOrigin, Span};
use crate::children;
use crate::lower_effect::EffectScanCx;

pub use derive::{derive_effect_origins, derive_throw_origins};

/// One catch variable in scope for rethrow precision: its name (no `$`), the
/// classes its clause absorbs, and whether a caught type could not be named.
pub(crate) type CatchVar = (String, Vec<NameRef>, bool);

/// What the walk carries down: the frame, the enclosing `try` guards (outermost
/// first) and the catch variables in scope.
#[derive(Clone, Copy)]
pub(crate) struct SiteScope<'a> {
    pub(crate) cx: &'a EffectScanCx,
    guards: &'a [Vec<CatchClause>],
    catch_scope: &'a [CatchVar],
}

impl SiteScope<'_> {
    /// A site at `span` with no argument metadata, under the guards active here.
    pub(crate) fn site(&self, span: Span, kind: SiteKind) -> SiteOrigin {
        SiteOrigin {
            span,
            kind,
            guards: self.guards.iter().rev().cloned().collect(),
            operands: None,
            ref_targets: None,
            const_args: ConstArgs::default(),
        }
    }
}

/// Append every [`SiteOrigin`] of a function-like body subtree to `out`. `node`
/// is a body statement (or an arrow function's expression); a frame calls this
/// once per root, next to the legacy scans.
pub(crate) fn scan_owner_sites(node: &Node<'_, '_>, cx: &EffectScanCx, out: &mut Vec<SiteOrigin>) {
    scan_sites(node, &SiteScope { cx, guards: &[], catch_scope: &[] }, out);
}

/// Walk a subtree, appending its sites in source order. Does not descend into
/// nested scopes (function, closure, arrow function or class-like bodies),
/// whose sites are their own owner's.
fn scan_sites(node: &Node<'_, '_>, sx: &SiteScope<'_>, out: &mut Vec<SiteOrigin>) {
    match node {
        Node::FunctionCall(fc) => call::function_call(fc, sx, out),
        Node::MethodCall(mc) => call::method_call(mc, sx, out),
        Node::NullSafeMethodCall(mc) => call::nullsafe_method_call(mc, sx, out),
        Node::StaticMethodCall(sc) => call::static_method_call(sc, sx, out),
        Node::Instantiation(inst) => call::instantiation(inst, sx, out),
        // An anonymous class's body is its own scope, but its constructor runs
        // at this `new`, and its arguments are evaluated here.
        Node::AnonymousClass(ac) => {
            call::anonymous_class(ac, sx, out);
            if let Some(list) = &ac.argument_list {
                scan_sites(&Node::PartialArgumentList(list), sx, out);
            }
            return;
        }
        Node::Try(_) => {
            throw::try_node(node, sx, out);
            return;
        }
        Node::Throw(t) => throw::throw_site(t, sx, out),
        Node::Match(m) => construct::match_site(m, sx, out),
        // Nested scopes are scanned independently.
        Node::Function(_)
        | Node::Closure(_)
        | Node::ArrowFunction(_)
        | Node::Class(_)
        | Node::Interface(_)
        | Node::Trait(_)
        | Node::Enum(_) => return,
        _ => {
            if construct::lower(node, sx, out) {
                return;
            }
        }
    }
    for child in children(node) {
        scan_sites(&child, sx, out);
    }
}
