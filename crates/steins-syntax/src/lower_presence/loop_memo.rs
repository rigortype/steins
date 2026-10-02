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
//! * **the entry state**;
//! * **the `unset` run's `seeded_at` map** at entry (empty on the ADR-0081 run): the
//!   walk rewrites it on the way, and what a later statement reads from it depends on
//!   it.
//!
//! Everything else the walk consults is fixed for the run (`reportable`, `seeds`) or
//! is saved, cleared and restored around the body (`breaks`, `continues`, `silent`).
//! `seen` is consulted only by a recording walk, so it is not an input. A hit replays
//! the exits and the rewritten `seeded_at` entries.
//!
//! The table is looked up by the body and an order-independent hash of the two maps,
//! which costs one pass and no allocation; a candidate record is then compared with
//! the real entry state and `seeded_at` map, so a hash collision costs a comparison,
//! never a wrong answer.
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
use std::hash::{DefaultHasher, Hash, Hasher};
use std::rc::Rc;

use mago_syntax::cst::Statement;

use super::{LoopExits, PresenceCx, PresenceState, presence_loop_walk};
use crate::stack_guard;

/// An order-independent hash of a map's entries: each entry hashes on its own (with the
/// fixed-key hasher, so it is deterministic) and the results are summed, so two equal
/// maps hash alike whatever their iteration order, with no allocation and no sort.
fn map_hash<V: Hash>(map: &HashMap<String, V>) -> u64 {
    map.iter()
        .map(|entry| {
            let mut hasher = DefaultHasher::new();
            entry.hash(&mut hasher);
            hasher.finish()
        })
        .fold(0, u64::wrapping_add)
}

/// The cheap, lossy half of a lookup: records that agree here are then compared in
/// full, so a hash collision costs a comparison and never a wrong answer.
#[derive(PartialEq, Eq, Hash)]
struct LoopKey {
    /// The body slice's address and length.
    body: (usize, usize),
    entry: u64,
    seeded: u64,
}

/// What one uncached silent walk produced, with the inputs it was keyed on.
struct LoopRecord {
    entry: PresenceState,
    seeded: HashMap<String, u32>,
    exits: LoopExits,
    /// The `seeded_at` entries it left different from how it found them.
    reseeded: Vec<(String, u32)>,
}

/// The per-run table of silently walked loop bodies.
#[derive(Default)]
pub(super) struct LoopMemo {
    records: HashMap<LoopKey, Vec<Rc<LoopRecord>>>,
    /// How many records `records` holds in all.
    len: usize,
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

impl LoopMemo {
    fn lookup(
        &self,
        key: &LoopKey,
        entry: &PresenceState,
        seeded: &HashMap<String, u32>,
    ) -> Option<Rc<LoopRecord>> {
        let bucket = self.records.get(key)?;
        bucket.iter().find(|r| r.entry == *entry && r.seeded == *seeded).map(Rc::clone)
    }

    fn store(&mut self, key: LoopKey, record: LoopRecord) {
        self.records.entry(key).or_default().push(Rc::new(record));
        self.len += 1;
        #[cfg(test)]
        tests::note_records(self.len);
    }

    fn clear(&mut self) {
        self.records.clear();
        self.len = 0;
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
        entry: map_hash(entry),
        seeded: map_hash(&cx.seeded_at),
    };
    let hit = cx.loop_memo.as_ref().and_then(|m| m.lookup(&key, entry, &cx.seeded_at));
    if let Some(record) = hit {
        #[cfg(test)]
        tests::count_hit();
        for (name, at) in &record.reseeded {
            cx.seeded_at.insert(name.clone(), *at);
        }
        return record.exits.clone();
    }

    let pushed_from = cx.out.len();
    let seeded = cx.seeded_at.clone();
    let exits = presence_loop_walk(body, entry, cx);
    debug_assert_eq!(cx.out.len(), pushed_from, "a silent walk records no read");
    let reseeded = cx
        .seeded_at
        .iter()
        .filter(|(name, at)| seeded.get(*name) != Some(*at))
        .map(|(name, at)| (name.clone(), *at))
        .collect();
    let record = LoopRecord { entry: entry.clone(), seeded, exits: exits.clone(), reseeded };
    if let Some(memo) = cx.loop_memo.as_mut() {
        memo.store(key, record);
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
            memo.clear();
        }
    }
    exits
}

#[cfg(test)]
mod tests;
