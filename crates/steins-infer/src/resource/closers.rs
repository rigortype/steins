//! What a statement's calls do to the heap resources they are handed (ADR-0097 §2.4):
//! the closing table and the per-position verdict (close, keep or escape), and the
//! escape of every handle a value mentions outside a call. The top-level rebind
//! rule that used to live here is the frame's now, shared by every carrier
//! (`crate::rebind`, issue #762).

use std::collections::HashSet;

use steins_catalog::ResourceKind;
use steins_syntax::{ArgValue, ArrayKey, CallExpr, Callee, ClosureRef, InvalidatedVar, Receiver};

use crate::cx::Cx;
use crate::env::{AllocId, HandleState, Store};
use crate::existence::{denotes_global_function, global_function_callee};
use crate::fold::Folder;

/// The **closing calls** (ADR-0097 §2.4; CONTEXT.md "Closing call") and, per
/// call, the kinds of handle it leaves **closed** when it returns — each cell
/// probed at 8.5.10 by `gettype()` reading `"resource (closed)"` after the call.
///
/// A return is the whole premise: every argument a closing call rejects — a
/// non-resource, an already-closed handle, a handle of a kind the call has no
/// row for — is a `TypeError`, so the statement after it runs only when the
/// close happened. That is why the table lists what a *normal* return closes
/// and nothing else, and why the one kind-sensitive cell is a keeper rather
/// than an error: `fclose`, `gzclose` and `bzclose` over an `opendir()` handle
/// **warn, return `false` and leave it open** (`cannot close the provided
/// stream, as it must not be manually closed`), so a `dir` is absent from their
/// rows and the handle keeps its state. `pclose` closes a `dir` (and any plain
/// stream); `closedir` and `proc_close` reject everything but their own kind.
///
/// A persistent stream (`pfsockopen`) is closed by `fclose`, `gzclose`,
/// `bzclose` and `pclose` alike: the handle reads `resource (closed)` and
/// `is_resource` says `false`, while the connection lives on for the next
/// `pfsockopen()` to hand out under a new id. A `stream-filter` has one closer
/// of its own, `stream_filter_remove` (probed at 8.5.10: `gettype()` reads
/// `resource (closed)` after it, and the call throws on a stream, a context
/// and an already-removed filter alike); the `stream-context` kind appears in
/// no row, since every closer throws on it.
const CLOSERS: &[(&str, &[ResourceKind])] = &[
    ("fclose", &[ResourceKind::Stream, ResourceKind::PersistentStream]),
    ("gzclose", &[ResourceKind::Stream, ResourceKind::PersistentStream]),
    ("bzclose", &[ResourceKind::Stream, ResourceKind::PersistentStream]),
    ("pclose", &[ResourceKind::Stream, ResourceKind::PersistentStream, ResourceKind::Dir]),
    ("closedir", &[ResourceKind::Dir]),
    ("proc_close", &[ResourceKind::Process]),
    ("stream_filter_remove", &[ResourceKind::StreamFilter]),
];

/// What one **direct argument position** of a global builtin does to the heap
/// resource handed to it (ADR-0097 §2.4's table, one row per verdict).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SiteVerdict {
    /// A closing call whose row closes this kind: the state becomes `Closed`.
    Close,
    /// A **keeper**: a closing call that returns from this kind without closing
    /// it, or a builtin whose reflected parameter here is by value and that is
    /// not a closing call — a builtin cannot close a handle it merely reads.
    /// The state is unchanged and the binding survives the call.
    Keep,
    /// Everything else: the handle escapes and its state goes `Unknown`.
    Escape,
}

/// [`SiteVerdict`] for the global builtin `name` (already resolved through
/// [`denotes_global_function`], so a project shadow is excluded) receiving a
/// handle of `kind` at `position`.
///
/// The closing table is consulted **first** and by name alone: its rows were
/// probed by hand and every one takes its handle by value, so it needs no
/// reflection — which is also what keeps the proof of `Closed` available on an
/// engine whose replay table predates the parameter reply. A name outside the
/// table is a keeper only on the engine's own word: the reflected parameter at
/// `position` exists, is not variadic, and is not by reference. A position past
/// the declared list, a variadic one, a by-reference one, and a name the engine
/// reflects no signature for are all escapes — the silent direction.
///
/// The one userland route a by-value stream position leaves open is a user
/// stream wrapper (`stream_wrapper_register`), whose `stream_tell()` runs on
/// `ftell($h)`; that its body could reach `$h` through `global` and close it is
/// accepted as this slice's calibration, exactly as ADR-0070's by-value rule
/// accepts a callback's `global` inside a function frame.
fn site_verdict(
    folder: &mut dyn Folder,
    name: &str,
    position: usize,
    kind: ResourceKind,
) -> SiteVerdict {
    if let Some((_, kinds)) = CLOSERS.iter().find(|(n, _)| name.eq_ignore_ascii_case(n)) {
        return if kinds.contains(&kind) { SiteVerdict::Close } else { SiteVerdict::Keep };
    }
    let Some(params) = folder.builtin_param_types(name) else { return SiteVerdict::Escape };
    match params.get(position) {
        Some(p) if !p.variadic && !p.by_ref => SiteVerdict::Keep,
        _ => SiteVerdict::Escape,
    }
}

/// What a statement's calls do to the heap resources their arguments name
/// (ADR-0097 §2.4), read on the pre-call store — where every binding the
/// statement is about to forget still resolves — and applied by
/// [`apply_resource_effects`] after the statement's own forgetting, so that
/// a closed handle stays closed for every name that shares it, `$h =
/// fclose($h)` included.
pub(crate) struct ResourceEffects {
    /// The heap resources this statement closed (`true`) or let escape
    /// (`false`), by allocation id — a binding the statement drops cannot be
    /// read back by name, and the entry outlives the name for its aliases.
    transitions: Vec<(AllocId, bool)>,
    /// The variables whose **binding survives** the statement's conservative
    /// forgetting: every occurrence of the name in the statement's call
    /// arguments is a keeper or a closing call, so nothing the statement did
    /// could have rebound the variable — only the heap state may have changed.
    pub(crate) kept: HashSet<String>,
}

/// Compute [`ResourceEffects`] for `calls` — a statement's
/// [`checkable_calls`] or a guard's retained calls — on the pre-call `store`.
///
/// Per call: each **direct** positional `$v` argument bound to a heap resource
/// takes [`site_verdict`] for the call's global builtin, and escapes for any
/// other callee (a project function, a method, a constructor, a dynamic or
/// unresolvable name, a call with named or spread arguments). A handle named
/// anywhere **inside** an argument — an array literal, a nested call's
/// argument, a closure's capture — and one in receiver position escapes
/// unconditionally: nothing here reads through a nested position, and the
/// silent direction is the only sound one there.
///
/// `invalidated` is the statement's own completeness oracle for `kept`
/// ([`Stmt::invalidated`], every occurrence recorded or the entry marked
/// opaque); a guard position passes none and keeps nothing here — its lane
/// survival stays with the guard machinery ([`by_value_survivors`],
/// [`type_predicate`]) as before.
///
/// [`checkable_calls`]: crate::descent::checkable_calls
/// [`Stmt::invalidated`]: steins_syntax::Stmt::invalidated
/// [`by_value_survivors`]: crate::by_value::by_value_survivors
/// [`type_predicate`]: crate::predicates::type_predicate
pub(crate) fn resource_call_effects(
    cx: &Cx,
    folder: &mut dyn Folder,
    poisoned: bool,
    calls: &[&CallExpr],
    invalidated: Option<&[InvalidatedVar]>,
    store: &Store,
) -> ResourceEffects {
    let mut effects = ResourceEffects { transitions: Vec::new(), kept: HashSet::new() };
    if poisoned || calls.is_empty() {
        return effects;
    }
    let mut escaped_mentions: Vec<&str> = Vec::new();
    for call in calls {
        let builtin = global_function_callee(cx, call).filter(|_| call.positional_only);
        for (position, arg) in call.args.iter().enumerate() {
            match &arg.value {
                // A place, not only a variable (ADR-0098 §2.4): `fclose($pipes[0])`
                // closes the entry the element names, by the same id transition a
                // bare `$h` takes. An offset whose key is not literal names no
                // place and falls to the escape leg below, as it does today.
                value if crate::offsets::place_of_static(value)
                    .is_some_and(|p| store.res_of(&p).is_some()) =>
                {
                    let place = crate::offsets::place_of_static(value)
                        .expect("the guard just resolved one");
                    let res = store.res_of(&place).expect("the guard just found one");
                    let id = store.id_of(&place).expect("a bound resource has an id");
                    let verdict = match builtin {
                        Some(name) => site_verdict(folder, name, position, res.kind),
                        None => SiteVerdict::Escape,
                    };
                    match verdict {
                        SiteVerdict::Close => effects.transitions.push((id, true)),
                        SiteVerdict::Keep => {}
                        SiteVerdict::Escape => effects.transitions.push((id, false)),
                    }
                }
                other => mentioned_vars(other, &mut escaped_mentions),
            }
        }
        for named in &call.named_args {
            mentioned_vars(&named.value, &mut escaped_mentions);
        }
        match &call.receiver {
            Callee::Method { receiver: Receiver::Var(v), .. } => escaped_mentions.push(v),
            Callee::Method { receiver: Receiver::New { args, named, .. }, .. } => {
                for value in args.iter().chain(named.iter().map(|n| &n.value)) {
                    mentioned_vars(value, &mut escaped_mentions);
                }
            }
            _ => {}
        }
    }
    for v in escaped_mentions {
        if let Some(id) = store.id_of(v).filter(|id| store.resources.contains_key(id)) {
            effects.transitions.push((id, false));
        }
    }
    // The survivors: a resource-bound name every recorded site of which is a
    // keeper or a closing call of a global builtin, with no opaque occurrence.
    for entry in invalidated.unwrap_or(&[]) {
        if entry.opaque || entry.sites.is_empty() {
            continue;
        }
        // The kinds this name's sites must all be keepers for: the handle it
        // holds itself, or — ADR-0098 §2.3 — the handles its element places
        // hold. `fclose($pipes[0])` records `pipes` as an occurrence, and the
        // array is no more rebound by it than `$h` is by `fclose($h)`; without
        // this leg the base is swept and its places die with it, which is the
        // whole `proc_open` idiom lost one statement in.
        let kinds: Vec<ResourceKind> = match store.res_of(&entry.name) {
            Some(res) => vec![res.kind],
            None => store
                .places_under(&entry.name)
                .iter()
                .filter_map(|(_, id)| store.resources.get(id).map(|r| r.kind))
                .collect(),
        };
        if kinds.is_empty() {
            continue;
        }
        let survives = entry.sites.iter().all(|(r, position)| {
            denotes_global_function(cx, r)
                && kinds.iter().all(|kind| {
                    site_verdict(folder, &r.raw, *position as usize, *kind) != SiteVerdict::Escape
                })
        });
        if survives {
            effects.kept.insert(entry.name.clone());
        }
    }
    effects
}

/// Apply [`resource_call_effects`]' transitions to `store`: a closed handle's
/// entry goes `Closed`; an escaped one takes [`HandleState::escaped`]
/// (`Closed` stays — nothing reopens a handle). By id, so a name the statement
/// rebound or forgot still reaches the entry its aliases share.
///
/// At top level a statement that may run a userland body has already cleared
/// the store before this runs (issue #762, [`top_level_rebind_risk`]), so an id
/// read on the pre-call store reaches nothing there: a `Closed` proved about
/// the handle a name used to hold does not outlive the call that may have
/// pointed the name elsewhere.
///
/// [`top_level_rebind_risk`]: crate::rebind::top_level_rebind_risk
pub(crate) fn apply_resource_effects(effects: &ResourceEffects, store: &mut Store) {
    for (id, closed) in &effects.transitions {
        if let Some(r) = store.resources.get_mut(id) {
            r.state = if *closed { HandleState::Closed } else { r.state.escaped() };
        }
    }
}

/// Every local variable a value **mentions**, at any depth the value IR spells
/// (ADR-0097 §2.4's "stored, captured, nested" escapes): array elements and
/// expression keys, nested call/method/constructor arguments and receivers, a
/// closure's by-value captures, both arms of a ternary and both operands of
/// `??`, a clone's source, the operands of a cast, a negation, a
/// concatenation and a binary operator, an offset read's base and key. A
/// property fetch, an `isset` and the literal/constant leaves mention no
/// binding a handle could ride.
fn mentioned_vars<'a>(value: &'a ArgValue, out: &mut Vec<&'a str>) {
    match value {
        ArgValue::Var(v) | ArgValue::Clone(v) => out.push(v),
        ArgValue::Call(_, args) => args.iter().for_each(|a| mentioned_vars(a, out)),
        ArgValue::MethodCall { callee, args, named } => {
            match callee {
                Callee::Method { receiver: Receiver::Var(v), .. } => out.push(v),
                Callee::Method { receiver: Receiver::New { args, named, .. }, .. } => {
                    for value in args.iter().chain(named.iter().map(|n| &n.value)) {
                        mentioned_vars(value, out);
                    }
                }
                _ => {}
            }
            for value in args.iter().chain(named.iter().map(|n| &n.value)) {
                mentioned_vars(value, out);
            }
        }
        ArgValue::New(_, args, named) => {
            for value in args.iter().chain(named.iter().map(|n| &n.value)) {
                mentioned_vars(value, out);
            }
        }
        ArgValue::Array(items) => {
            for (key, value) in items {
                if let ArrayKey::Expr(k) = key {
                    mentioned_vars(k, out);
                }
                mentioned_vars(value, out);
            }
        }
        ArgValue::Ternary { then_val, else_val, .. } => {
            mentioned_vars(then_val, out);
            mentioned_vars(else_val, out);
        }
        ArgValue::Closure(ClosureRef::Anonymous { captures, .. }) => {
            out.extend(captures.iter().map(String::as_str));
        }
        ArgValue::Coalesce(a, b, _) | ArgValue::Concat(a, b) => {
            mentioned_vars(a, out);
            mentioned_vars(b, out);
        }
        ArgValue::Binary { lhs, rhs, .. } | ArgValue::Logical { lhs, rhs, .. } => {
            mentioned_vars(lhs, out);
            mentioned_vars(rhs, out);
        }
        ArgValue::OffsetRead { base, key } => {
            mentioned_vars(base, out);
            mentioned_vars(key, out);
        }
        ArgValue::Not(v) | ArgValue::Cast { operand: v, .. } => mentioned_vars(v, out),
        ArgValue::Int(_)
        | ArgValue::Float(_)
        | ArgValue::Str(_)
        | ArgValue::Bool(_)
        | ArgValue::Null
        | ArgValue::Closure(ClosureRef::FunctionName(_))
        | ArgValue::PropFetch { .. }
        | ArgValue::ClassConst(..)
        | ArgValue::EnumCase(..)
        | ArgValue::GlobalConst(_)
        | ArgValue::Isset(_)
        | ArgValue::Other => {}
    }
}

/// Let every heap resource `value` mentions escape (ADR-0097 §2.4's "stored
/// into an array or a property, captured by a closure, returned" rows): the
/// statement-effect twin of [`resource_call_effects`], for the rvalue positions
/// that are not calls — an assignment's right-hand side, an offset write's
/// value, a `return` operand.
pub(crate) fn escape_mentioned_resources(value: &ArgValue, store: &mut Store) {
    let mut vars = Vec::new();
    mentioned_vars(value, &mut vars);
    for v in vars {
        store.escape_resource(v);
    }
}
