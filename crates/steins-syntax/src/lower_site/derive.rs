//! The effect lane's and the throw lane's origin lists, derived from a body's
//! sites. Each is exactly what the lane's legacy scan records: `site_oracle`
//! (`tests/it/site_oracle.rs`) holds them equal over every file it reads.

use crate::ast::{
    ConstructKind, EffectOrigin, EffectRecv, NameRef, RefKind, SiteKind, SiteOrigin, ThrowKind,
    ThrowOrigin, ThrownKind,
};

/// The [`EffectOrigin`]s of a body: one per site the effect lane records, in
/// site order. A throw, and a `match` with no `default`, are the throw lane's.
#[must_use]
pub fn derive_effect_origins(sites: &[SiteOrigin]) -> Vec<EffectOrigin> {
    sites.iter().filter_map(effect_origin).collect()
}

/// The [`ThrowOrigin`]s of a body: one per site the throw lane records, in site
/// order. Output, exit, `eval`, inclusion and state constructs are the effect
/// lane's.
#[must_use]
pub fn derive_throw_origins(sites: &[SiteOrigin]) -> Vec<ThrowOrigin> {
    sites.iter().filter_map(throw_origin).collect()
}

/// The effect lane's origin for one site, if it records one.
fn effect_origin(site: &SiteOrigin) -> Option<EffectOrigin> {
    let span = site.span;
    let origin = match &site.kind {
        SiteKind::Call { name, callbacks } if callbacks.is_empty() => EffectOrigin::Call {
            name: name.clone(),
            span,
            arg_targets: site.ref_targets.clone(),
            const_args: site.const_args.clone(),
            arg_shapes: site.operands.clone(),
        },
        // The scan forms a higher-order call only from an all-positional argument
        // list, so `ref_targets` is `Some` there, one entry per argument.
        SiteKind::Call { name, callbacks } => EffectOrigin::HigherOrder {
            callee: name.clone(),
            callbacks: callbacks.clone(),
            arg_count: site.ref_targets.as_ref().map_or(0, Vec::len),
            arg_targets: site.ref_targets.clone().unwrap_or_default(),
            const_args: site.const_args.clone(),
            span,
            arg_shapes: site.operands.clone().unwrap_or_default(),
        },
        SiteKind::MethodCall { receiver, method } => EffectOrigin::MethodCall {
            receiver: receiver.clone(),
            method: method.clone(),
            span,
            arg_shapes: site.operands.clone(),
        },
        SiteKind::New { class } => {
            EffectOrigin::New { class: class.clone(), span, arg_shapes: site.operands.clone() }
        }
        SiteKind::Callback { cbref } => EffectOrigin::Callback { cbref: cbref.clone(), span },
        SiteKind::Dynamic(_) => EffectOrigin::Opaque { span },
        SiteKind::Throw(_) => return None,
        SiteKind::Construct(construct) => return effect_construct(construct, span),
    };
    Some(origin)
}

/// The effect lane's origin for a construct site, if it records one.
fn effect_construct(construct: &ConstructKind, span: crate::ast::Span) -> Option<EffectOrigin> {
    Some(match construct {
        ConstructKind::Output(keyword) => EffectOrigin::Output { keyword: *keyword, span },
        ConstructKind::Exit(keyword) => EffectOrigin::Exit { keyword: *keyword, span },
        ConstructKind::Eval => EffectOrigin::Eval { span },
        ConstructKind::Include(keyword) => EffectOrigin::Include { keyword: *keyword, span },
        ConstructKind::State(construct) => EffectOrigin::State { construct: *construct, span },
        ConstructKind::MatchNoDefault => return None,
    })
}

/// The throw lane's origin for one site, if it records one.
fn throw_origin(site: &SiteOrigin) -> Option<ThrowOrigin> {
    let kind = match &site.kind {
        SiteKind::Call { name, callbacks } if callbacks.is_empty() => ThrowKind::Call(name.clone()),
        SiteKind::Call { name, callbacks } => ThrowKind::HigherOrder {
            callee: name.clone(),
            callbacks: callbacks.clone(),
            arg_count: site.ref_targets.as_ref().map_or(0, Vec::len),
        },
        // The throw lane has no flow-free way to name a declared receiver's
        // class, so the two declared forms are as unresolvable to it as a
        // dynamic one.
        SiteKind::MethodCall { receiver: EffectRecv::Var(_) | EffectRecv::PropRead(_), .. } => {
            ThrowKind::Taint
        }
        SiteKind::MethodCall { receiver, method } => {
            ThrowKind::MethodCall { receiver: receiver.clone(), method: method.clone() }
        }
        SiteKind::New { class } => ThrowKind::Construct { class: class.clone() },
        SiteKind::Callback { cbref } => ThrowKind::Callback { cbref: cbref.clone() },
        SiteKind::Dynamic(_) => ThrowKind::Taint,
        SiteKind::Throw(thrown) => thrown_kind(thrown),
        // An `UnhandledMatchError` is an `Error` (unchecked), so it never
        // enters `throw.undeclared`; it surfaces only in the annotate margin.
        SiteKind::Construct(ConstructKind::MatchNoDefault) => ThrowKind::New(NameRef {
            raw: "UnhandledMatchError".to_owned(),
            kind: RefKind::FullyQualified,
            offset: site.span.start,
        }),
        SiteKind::Construct(_) => return None,
    };
    Some(ThrowOrigin { kind, span: site.span, guards: site.guards.clone() })
}

/// The throw lane's kind for a `throw` site.
fn thrown_kind(thrown: &ThrownKind) -> ThrowKind {
    match thrown {
        ThrownKind::New(class) => ThrowKind::New(class.clone()),
        ThrownKind::Rethrow { caught, has_unresolvable } => {
            ThrowKind::Rethrow { caught: caught.clone(), has_unresolvable: *has_unresolvable }
        }
        ThrownKind::Unresolved => ThrowKind::Taint,
    }
}
