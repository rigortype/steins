//! Memoizing the loop-body walk (issue #793).
//!
//! [`presence_loop_walk`] runs a body up to twice silently, to find the fixpoint, and
//! once more to report. Nested, the inner body is walked from every one of those
//! three walks of the outer one, so a depth-`d` nest costs `2^d` to `3^d` body walks
//! (issue #484 cached the leaf scans, not this traversal).
//!
//! # What is cached
//!
//! Only **silent** walks. A reporting walk of a body happens once per run — the
//! reporting walk of the loop around it is itself unique, by induction up to the
//! outermost loop — so its answer is never asked for again, and recording it would
//! only cost memory. A silent walk records no read (`PresenceCx::record` returns
//! first), so its answer is the two exit sets and the `seeded_at` entries it
//! rewrote; a debug assertion checks that `out` did not grow.
//!
//! The silent walk is a pure function of three things, and the cache is keyed on all
//! three:
//!
//! * **the body** — its slice address and length. A slice's memory is its content,
//!   so equal keys within one run are the same statements; the run borrows the CST
//!   throughout, so no address is reused for another body. An empty body is never
//!   cached: its slice is a dangling pointer shared by every empty body, and it is
//!   too cheap to be worth an entry;
//! * **the entry state**, in the canonical sorted form of [`StateKey`];
//! * **the `unset` run's `seeded_at` map** at entry (empty on the ADR-0081 run): the
//!   walk rewrites it on the way, and what a later statement reads from it depends on
//!   it.
//!
//! Everything else the walk consults is fixed for the run (`reportable`, `seeds`) or
//! is saved, cleared and restored around the body (`breaks`, `continues`, `silent`).
//! `seen` is consulted only by a recording walk, so it is not an input. A hit replays
//! the exits and the rewritten `seeded_at` entries.
//!
//! # How long it lives
//!
//! The table is a field of the [`PresenceCx`], so it is at most as long as one
//! [`maybe_undefined_reads`](super::maybe_undefined_reads) or
//! [`unset_seed_facts`](super::unset_seed_facts) run: no address outlives its CST,
//! and nothing crosses a function or a file. Within a run it is dropped early: a
//! `depth` counter follows the reporting walks, and when the outermost one returns
//! nothing inside that loop is walked again — a silent walk happens only under an
//! enclosing loop's walk — so the table is cleared. Clearing can only cause misses.
//! That bounds the table by one top-level loop's silent walks rather than by every
//! loop in the function, which is what keeps a function of many sequential loops
//! over many locals from holding a state per loop for the whole run.
//!
//! It is off under a stack-guard floor (the wasm playground), where a scan the guard
//! truncates is no longer a pure function of its subtree — the same refusal as
//! [`crate::memo`].

use std::collections::HashMap;
use std::rc::Rc;

use mago_syntax::cst::Statement;

use super::{BindingPresence, LoopExits, PresenceCx, PresenceState, presence_loop_walk};
use crate::stack_guard;

/// A presence state in canonical form: its entries sorted by name, so two equal maps
/// key alike whatever their iteration order.
type StateKey = Vec<(String, BindingPresence)>;

/// A `seeded_at` map in canonical form.
type SeedKey = Vec<(String, u32)>;

fn state_key(state: &PresenceState) -> StateKey {
    let mut key: StateKey = state.iter().map(|(name, p)| (name.clone(), *p)).collect();
    key.sort_unstable();
    key
}

fn seed_key(seeded_at: &HashMap<String, u32>) -> SeedKey {
    let mut key: SeedKey = seeded_at.iter().map(|(name, at)| (name.clone(), *at)).collect();
    key.sort_unstable();
    key
}

#[derive(PartialEq, Eq, Hash)]
struct LoopKey {
    /// The body slice's address and length.
    body: (usize, usize),
    entry: StateKey,
    seeded: SeedKey,
}

/// What one uncached silent walk produced.
struct LoopRecord {
    exits: LoopExits,
    /// The `seeded_at` entries it left different from how it found them.
    reseeded: Vec<(String, u32)>,
}

/// The per-run table of silently walked loop bodies.
#[derive(Default)]
pub(super) struct LoopMemo {
    records: HashMap<LoopKey, Rc<LoopRecord>>,
    /// How many reporting loop walks are open: the table is cleared when the
    /// outermost one returns.
    depth: usize,
}

impl LoopMemo {
    /// The table for one run, or `None` where a stack-guard floor makes the walk
    /// impure.
    pub(super) fn for_run() -> Option<Self> {
        #[cfg(test)]
        if tests::forced_off() {
            return None;
        }
        (!stack_guard::guarded()).then(Self::default)
    }
}

/// Whether `seeded` (sorted by name) lacks `name` or holds a different statement for it.
fn rewritten(seeded: &SeedKey, name: &str, at: u32) -> bool {
    match seeded.binary_search_by(|(n, _)| n.as_str().cmp(name)) {
        Ok(i) => seeded[i].1 != at,
        Err(_) => true,
    }
}

/// Walk a loop body, or replay the answer of an earlier silent walk of the same body
/// from the same entry.
pub(super) fn presence_loop_body(
    body: &[Statement<'_>],
    entry: &PresenceState,
    cx: &mut PresenceCx,
) -> LoopExits {
    if cx.loop_memo.is_none() || body.is_empty() {
        return presence_loop_walk(body, entry, cx);
    }
    if !cx.silent {
        return reporting_walk(body, entry, cx);
    }
    let key = LoopKey {
        body: (body.as_ptr() as usize, body.len()),
        entry: state_key(entry),
        seeded: seed_key(&cx.seeded_at),
    };
    let hit = cx.loop_memo.as_ref().and_then(|m| m.records.get(&key)).map(Rc::clone);
    if let Some(record) = hit {
        #[cfg(test)]
        tests::count_hit();
        for (name, at) in &record.reseeded {
            cx.seeded_at.insert(name.clone(), *at);
        }
        return record.exits.clone();
    }

    let pushed_from = cx.out.len();
    let exits = presence_loop_walk(body, entry, cx);
    debug_assert_eq!(cx.out.len(), pushed_from, "a silent walk records no read");
    let reseeded = cx
        .seeded_at
        .iter()
        .filter(|(name, at)| rewritten(&key.seeded, name, **at))
        .map(|(name, at)| (name.clone(), *at))
        .collect();
    let record = LoopRecord { exits: exits.clone(), reseeded };
    if let Some(memo) = cx.loop_memo.as_mut() {
        memo.records.insert(key, Rc::new(record));
        #[cfg(test)]
        tests::note_records(memo.records.len());
    }
    exits
}

/// A reporting walk is never repeated, so it is neither looked up nor stored; it only
/// keeps the nesting count that says when the table has outlived its use.
fn reporting_walk(
    body: &[Statement<'_>],
    entry: &PresenceState,
    cx: &mut PresenceCx,
) -> LoopExits {
    if let Some(memo) = cx.loop_memo.as_mut() {
        memo.depth += 1;
    }
    let exits = presence_loop_walk(body, entry, cx);
    if let Some(memo) = cx.loop_memo.as_mut() {
        memo.depth -= 1;
        if memo.depth == 0 {
            memo.records.clear();
        }
    }
    exits
}

#[cfg(test)]
mod tests;
