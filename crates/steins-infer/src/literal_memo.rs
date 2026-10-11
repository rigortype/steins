//! **The literal seam's memo** (issue #1063): [`Cx::resolve_literal_under`]
//! answers each nested argument once per outermost resolution, however many
//! consumers inside it ask again.
//!
//! # The recursion it collapses
//!
//! Resolving a call value `c(a)` asks for the value of its argument `a` along
//! more than one road, and each road resolves `a` from scratch:
//!
//! - the fold ([`Cx::try_fold_under`]) resolves `a` to fold over it;
//! - when the fold declines (no runtime under `--no-php`, a name off the
//!   allowlist, a project function), the transfer rung
//!   ([`shape_builtin_return_fact`]) resolves `a` again as its subject when
//!   `a` is itself a call;
//! - the rung's string-predicate and argument-dispatch legs read `a`'s fact
//!   through [`transfer_arg_known`], which resolves `a` a third time.
//!
//! So `c(c(…c(x)…))` nested `n` deep costs `k^n` with `k` between 2 (a project
//! function: the subject and the fact road) and 3 (a builtin: the fold as well).
//! Every road is a pure function of the same inputs, so one answer per argument
//! serves them all.
//!
//! # When an answer is reused
//!
//! The memo lives for one **outermost** resolution — the root, the first call
//! made while no memo is open — and is dropped when the root returns (or
//! unwinds). Inside it, an answer is stored and reused only when every input
//! that answer depends on is provably the root's:
//!
//! - **The value** is identified by address, and only for values inside the
//!   root's own value tree ([`Memo::stable`]): the root is borrowed for the whole
//!   extent, so its descendants live at addresses nothing else can occupy while
//!   the memo is open. A temporary built along the way (a resolved argument
//!   list, a lifted literal) is never stored, since a later temporary could reuse
//!   its address.
//! - **The env, the folder and the context** are the root's. The env is
//!   immutably borrowed for the extent, so equal addresses are equal contents; a
//!   callee body walked by a descent binds a fresh env and does not match.
//! - **No live descent.** A [`Descent`] carries a memo of its own that a second
//!   resolution would hit, emitting no findings the second time; a stored answer
//!   replayed there could emit findings the recomputation would not. A call
//!   resolved under a descent is computed as before. The roads that multiply
//!   (the transfer rung and its fact reads) pass no descent, so this costs the
//!   collapse nothing.
//!
//! The findings a resolution emits are part of its answer: a stored entry keeps
//! the diagnostics the computation produced, and a reuse replays them into the
//! caller's sink, exactly what recomputing would have emitted (with no descent,
//! each nested binding descent starts from an empty memo and re-emits). The
//! computation always runs with a sink, which no resolution road treats
//! differently from none: a sinkless fold writes to a scratch it discards.
//!
//! [`Cx::resolve_literal_under`]: crate::cx::Cx::resolve_literal_under
//! [`Cx::try_fold_under`]: crate::cx::Cx::try_fold_under
//! [`shape_builtin_return_fact`]: crate::builtin_returns::shape_builtin_return_fact
//! [`transfer_arg_known`]: crate::transfers::transfer_arg_known

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use steins_syntax::ArgValue;

use crate::cx::Cx;
use crate::env::{Descent, Known, Stratum};
use crate::fold::Folder;
use crate::project::Diagnostic;

/// A resolution's answer: the literal and its stratum, or nothing proven.
pub(crate) type Resolved = Option<(ArgValue, Stratum)>;

/// Everything a resolution reads besides the value and the descent, by identity.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Inputs {
    units: usize,
    index: usize,
    cur: usize,
    dam: usize,
    purity: usize,
    php_target: usize,
    postures: crate::RuntimePostures,
    php_minor: Option<(u16, u16)>,
    catalog_skew: bool,
    version_id: Option<(u32, Option<u32>)>,
    env: usize,
    folder: usize,
}

impl Inputs {
    fn of(cx: &Cx<'_>, env: &HashMap<String, Known>, folder: &mut dyn Folder) -> Self {
        Self {
            units: cx.units.as_ptr().addr(),
            index: std::ptr::from_ref(cx.index).addr(),
            cur: cx.cur,
            dam: std::ptr::from_ref(cx.dam).addr(),
            purity: cx.purity.map_or(0, |p| std::ptr::from_ref(p).addr()),
            php_target: cx.php_target.map_or(0, |p| std::ptr::from_ref(p).addr()),
            postures: cx.postures,
            php_minor: cx.php_minor,
            catalog_skew: cx.catalog_skew,
            version_id: cx.version_id,
            env: std::ptr::from_ref(env).addr(),
            folder: std::ptr::from_mut(folder).cast::<()>().addr(),
        }
    }
}

/// One root's memo.
struct Memo {
    root: Inputs,
    /// Addresses of the values inside the root's value tree, grown as the
    /// resolution reaches them (a stored value's children join on its way in).
    stable: HashSet<usize>,
    /// The answer and the findings of each stored resolution, by value address.
    entries: HashMap<usize, (Resolved, Vec<Diagnostic>)>,
}

thread_local! {
    /// The open memo, `None` outside any resolution. Per thread, since every walk
    /// worker resolves on its own.
    static MEMO: RefCell<Option<Memo>> = const { RefCell::new(None) };
}

/// Closes the root's memo on return and on unwind, so a panic isolated to one
/// file's walk cannot leave the next file reading this one's answers.
struct RootGuard;

impl Drop for RootGuard {
    fn drop(&mut self) {
        MEMO.with_borrow_mut(|m| *m = None);
    }
}

/// Whether a value's resolution is worth the memo's bookkeeping: the variants
/// [`Cx::resolve_literal_under`] recurses through. A literal, a variable and
/// every variant the seam declines answer in constant time.
///
/// [`Cx::resolve_literal_under`]: crate::cx::Cx::resolve_literal_under
pub(crate) fn worth_memoising(value: &ArgValue) -> bool {
    matches!(
        value,
        ArgValue::Call(..)
            | ArgValue::Concat(..)
            | ArgValue::Binary { .. }
            | ArgValue::Cast { .. }
            | ArgValue::Array(_)
    )
}

/// The values directly inside `value` that the literal seam or its callers
/// resolve. A child left out is only never stored, never answered wrongly.
fn children(value: &ArgValue) -> Vec<&ArgValue> {
    match value {
        ArgValue::Call(_, args) => args.iter().collect(),
        ArgValue::MethodCall { args, named, .. } | ArgValue::New(_, args, named) => {
            args.iter().chain(named.iter().map(|n| &n.value)).collect()
        }
        ArgValue::Array(items) => items.iter().map(|(_, v)| v).collect(),
        ArgValue::Ternary { then_val, else_val, .. } => vec![then_val, else_val],
        ArgValue::Coalesce(a, b, _)
        | ArgValue::Concat(a, b)
        | ArgValue::OffsetRead { base: a, key: b }
        | ArgValue::Binary { lhs: a, rhs: b, .. }
        | ArgValue::Logical { lhs: a, rhs: b, .. } => vec![a, b],
        ArgValue::Not(a) | ArgValue::Cast { operand: a, .. } => vec![a],
        _ => Vec::new(),
    }
}

/// What the open memo says about one resolution.
enum Role {
    /// No memo was open: this resolution opens it.
    Root,
    /// A stored answer, to replay.
    Hit(Resolved, Vec<Diagnostic>),
    /// A storable resolution not yet stored.
    Miss,
    /// An input differs from the root's, or the value is not the root's: compute.
    Bypass,
}

/// Resolve `value` through the memo, computing it with `compute` when no stored
/// answer applies. The caller has already answered a poisoned scope and a value
/// not [`worth_memoising`].
pub(crate) fn resolve<'d>(
    cx: &Cx<'_>,
    value: &ArgValue,
    env: &HashMap<String, Known>,
    folder: &mut dyn Folder,
    descent: Option<&mut Descent<'d>>,
    out: Option<&mut Vec<Diagnostic>>,
    compute: impl FnOnce(
        &mut dyn Folder,
        Option<&mut Descent<'d>>,
        Option<&mut Vec<Diagnostic>>,
    ) -> Resolved,
) -> Resolved {
    let addr = std::ptr::from_ref(value).addr();
    let inputs = Inputs::of(cx, env, folder);
    let role = MEMO.with_borrow_mut(|open| match open {
        None => {
            *open = Some(Memo {
                root: inputs,
                stable: HashSet::from([addr]),
                entries: HashMap::new(),
            });
            Role::Root
        }
        Some(memo) => {
            if descent.is_some() || memo.root != inputs || !memo.stable.contains(&addr) {
                Role::Bypass
            } else if let Some((r, diags)) = memo.entries.get(&addr) {
                Role::Hit(r.clone(), diags.clone())
            } else {
                Role::Miss
            }
        }
    });
    let store = match role {
        Role::Hit(r, diags) => {
            if let Some(out) = out {
                out.extend(diags);
            }
            return r;
        }
        Role::Bypass => return compute(folder, descent, out),
        Role::Root => {
            let _guard = RootGuard;
            register_children(value);
            return compute(folder, descent, out);
        }
        Role::Miss => addr,
    };
    register_children(value);
    let mut emitted = Vec::new();
    let r = compute(folder, None, Some(&mut emitted));
    if let Some(out) = out {
        out.extend(emitted.iter().cloned());
    }
    MEMO.with_borrow_mut(|open| {
        if let Some(memo) = open {
            memo.entries.insert(store, (r.clone(), emitted));
        }
    });
    r
}

/// Mark `value`'s children as inside the root's tree; `value` is.
fn register_children(value: &ArgValue) {
    MEMO.with_borrow_mut(|open| {
        if let Some(memo) = open {
            memo.stable.extend(children(value).into_iter().map(|c| std::ptr::from_ref(c).addr()));
        }
    });
}
