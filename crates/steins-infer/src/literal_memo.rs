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
//! An array literal between the calls (`count([count([…])])`) adds a road: the
//! fact seam reads the literal element by element ([`array_literal_fact`]),
//! each element through [`transfer_arg_known`] again. Every road is a pure
//! function of the same inputs, so one answer per argument serves them all.
//!
//! # The extent
//!
//! The memo lives for one **outermost** read — the root, the first resolution or
//! fact-seam read ([`within`]) made while no memo is open — and is dropped when
//! the root returns (or unwinds). The fact seam opens the extent too, so the
//! literal reads it makes per element share one memo instead of each opening
//! its own.
//!
//! # When an answer is reused
//!
//! An answer is stored and reused only when every input it depends on is
//! provably the root's:
//!
//! - **The value** is identified by address, and only for values inside the
//!   root's own value tree ([`Memo::stable`]): the root is borrowed for the whole
//!   extent, so its descendants live at addresses nothing else can occupy while
//!   the memo is open. A value joins the set when the read reaches it through
//!   its parent. A temporary built along the way (a resolved argument list, a
//!   lifted literal) never joins, since a later temporary could reuse its
//!   address.
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
//! # What is kept
//!
//! Only calls, concatenations, comparisons and casts are stored — the variants
//! whose resolution re-enters the roads above. An array literal resolves by
//! resolving its elements, so it is never stored itself: its answer is a copy of
//! its whole subtree, and storing one per level of a nested literal held
//! `O(n·depth)` for no reuse. Its elements still join the stable set.
//!
//! A stored answer is read by the roads of its parent, which all run while the
//! parent is being computed. So once a stored value's own computation returns,
//! its children's entries are dropped: the memo holds the frontier of the read,
//! not every level of it, and a nested call whose answers are arrays does not
//! keep one copy per level.
//!
//! [`Cx::resolve_literal_under`]: crate::cx::Cx::resolve_literal_under
//! [`Cx::try_fold_under`]: crate::cx::Cx::try_fold_under
//! [`shape_builtin_return_fact`]: crate::builtin_returns::shape_builtin_return_fact
//! [`transfer_arg_known`]: crate::transfers::transfer_arg_known
//! [`array_literal_fact`]: crate::env::array_literal_fact

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
    /// Addresses of the values inside the root's value tree, grown as the read
    /// reaches them (a value's children join when the value is read).
    stable: HashSet<usize>,
    /// The answer and the findings of each stored resolution, by value address.
    entries: HashMap<usize, (Resolved, Vec<Diagnostic>)>,
}

thread_local! {
    /// The open memo, `None` outside any read. Per thread, since every walk
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

/// Whether a value's resolution is stored: the variants whose resolution
/// re-enters the roads above. A literal, a variable and every variant the seam
/// declines answer in constant time; an array literal is read through its
/// elements (see the module docs).
pub(crate) fn worth_memoising(value: &ArgValue) -> bool {
    matches!(
        value,
        ArgValue::Call(..) | ArgValue::Concat(..) | ArgValue::Binary { .. } | ArgValue::Cast { .. }
    )
}

/// The values directly inside `value` that the literal seam and the fact seam
/// read: a call's arguments, an operator's operands, an array literal's
/// elements. A child left out is only never stored, never answered wrongly.
fn children(value: &ArgValue) -> Vec<&ArgValue> {
    match value {
        ArgValue::Call(_, args) => args.iter().collect(),
        ArgValue::Array(items) => items.iter().map(|(_, v)| v).collect(),
        ArgValue::Concat(a, b) | ArgValue::Binary { lhs: a, rhs: b, .. } => vec![a, b],
        ArgValue::Cast { operand: a, .. } => vec![a],
        _ => Vec::new(),
    }
}

fn addr(value: &ArgValue) -> usize {
    std::ptr::from_ref(value).addr()
}

/// Opens the root's memo at `value`, or `None` when one is already open.
fn open_root(value: &ArgValue, inputs: Inputs) -> Option<RootGuard> {
    MEMO.with_borrow_mut(|open| {
        if open.is_some() {
            return None;
        }
        *open = Some(Memo {
            root: inputs,
            stable: HashSet::from([addr(value)]),
            entries: HashMap::new(),
        });
        Some(RootGuard)
    })
}

/// Mark `value`'s children as inside the root's tree, when `value` is.
pub(crate) fn note(value: &ArgValue) {
    MEMO.with_borrow_mut(|open| {
        if let Some(memo) = open
            && memo.stable.contains(&addr(value))
        {
            // A literal or a variable leaf is never stored and has no children.
            let inner = children(value)
                .into_iter()
                .filter(|c| !c.is_literal() && !matches!(c, ArgValue::Var(_)));
            memo.stable.extend(inner.map(addr));
        }
    });
}

/// Run a **fact-seam** read of `value` inside the memo's extent: opening the
/// root at `value` when no memo is open, so every literal read it makes per
/// element shares one memo.
pub(crate) fn within<R>(
    cx: &Cx<'_>,
    value: &ArgValue,
    env: &HashMap<String, Known>,
    folder: &mut dyn Folder,
    body: impl FnOnce(&mut dyn Folder) -> R,
) -> R {
    let _guard = open_root(value, Inputs::of(cx, env, folder));
    note(value);
    body(folder)
}

/// What the open memo says about one resolution.
enum Role {
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
    let key = addr(value);
    let inputs = Inputs::of(cx, env, folder);
    let _guard = open_root(value, inputs);
    let role = MEMO.with_borrow_mut(|open| {
        let Some(memo) = open else { return Role::Bypass };
        if descent.is_some() || memo.root != inputs || !memo.stable.contains(&key) {
            Role::Bypass
        } else if let Some((r, diags)) = memo.entries.get(&key) {
            Role::Hit(r.clone(), diags.clone())
        } else {
            Role::Miss
        }
    });
    note(value);
    match role {
        Role::Hit(r, diags) => {
            if let Some(out) = out {
                out.extend(diags);
            }
            r
        }
        Role::Bypass => compute(folder, descent, out),
        Role::Miss => {
            let mut emitted = Vec::new();
            let r = compute(folder, None, Some(&mut emitted));
            if let Some(out) = out {
                out.extend(emitted.iter().cloned());
            }
            store(value, key, r.clone(), emitted);
            r
        }
    }
}

/// Store `value`'s answer and drop the answers stored beneath it: their readers
/// were the roads of `value`'s own computation, which has returned. Beneath it
/// means down to the nearest stored value on each path, through the array
/// literals between (whose own answers are never stored).
fn store(value: &ArgValue, key: usize, r: Resolved, emitted: Vec<Diagnostic>) {
    MEMO.with_borrow_mut(|open| {
        if let Some(memo) = open {
            let mut below = children(value);
            while let Some(child) = below.pop() {
                if worth_memoising(child) {
                    memo.entries.remove(&addr(child));
                } else {
                    below.extend(children(child));
                }
            }
            memo.entries.insert(key, (r, emitted));
        }
    });
}
