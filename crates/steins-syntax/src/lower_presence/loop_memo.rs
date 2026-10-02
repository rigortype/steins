//! Memoizing the loop-body walk (issue #793).
//!
//! [`presence_loop_walk`] runs a body up to twice silently, to find the fixpoint, and
//! once more to report. Nested, the inner body is walked from every one of those
//! three walks of the outer one, so a depth-`d` nest costs `2^d` to `3^d` body walks
//! (issue #484 cached the leaf scans, not this traversal).
//!
//! The walk is a pure function of four things, and the cache is keyed on all four:
//!
//! * **the body** — its slice address and length. A slice's memory is its content,
//!   so equal keys within one run are the same statements; the run borrows the CST
//!   throughout, so no address is reused for another body. An empty body is never
//!   cached: its slice is a dangling pointer shared by every empty body, and it is
//!   too cheap to be worth an entry;
//! * **the entry state**, in the canonical sorted form of [`StateKey`];
//! * **the `silent` flag**: a silent walk records nothing, a reporting one does;
//! * **the `unset` run's `seeded_at` map** at entry (empty on the ADR-0081 run): a
//!   recorded read names the declaring statement it reads from there, and the walk
//!   rewrites it on the way.
//!
//! Everything else the walk consults is fixed for the run (`reportable`, `seeds`) or
//! is saved, cleared and restored around the body (`breaks`, `continues`, `silent`).
//! The outputs are the two exit sets, the reads the walk recorded and the
//! `seeded_at` entries it rewrote; a hit replays all three.
//!
//! The one input a hit does not key on is `seen`, and it needs no key: `seen` only
//! grows, so a read the walk skipped as already seen when the answer was cached is
//! still seen at every later hit, and a read the cached walk did record is replayed
//! through the same `seen` dedup. The reads a hit pushes are exactly the reads an
//! uncached walk would push.
//!
//! The cache lives in the [`PresenceCx`], so it is exactly as long as one
//! [`maybe_undefined_reads`](super::maybe_undefined_reads) or
//! [`unset_seed_facts`](super::unset_seed_facts) run: no address outlives its CST,
//! and nothing crosses a function or a file. It is off under a stack-guard floor
//! (the wasm playground), where a scan the guard truncates is no longer a pure
//! function of its subtree — the same refusal as [`crate::memo`].

use std::collections::HashMap;
use std::rc::Rc;

use mago_syntax::cst::Statement;

use super::{BindingPresence, LoopExits, PresenceCx, PresenceState, presence_loop_walk};
use crate::ast::UndefinedRead;
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
    silent: bool,
    entry: StateKey,
    seeded: SeedKey,
}

/// What one uncached walk produced.
struct LoopRecord {
    exits: LoopExits,
    /// The reads it pushed, in order, each with the seed it named.
    reads: Vec<(UndefinedRead, Option<u32>)>,
    /// The `seeded_at` entries it left different from how it found them.
    reseeded: Vec<(String, u32)>,
}

/// The per-run table of walked loop bodies.
#[derive(Default)]
pub(super) struct LoopMemo {
    records: HashMap<LoopKey, Rc<LoopRecord>>,
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

/// Walk a loop body, or replay the answer of an earlier walk of the same body from
/// the same entry.
pub(super) fn presence_loop_body(
    body: &[Statement<'_>],
    entry: &PresenceState,
    cx: &mut PresenceCx,
) -> LoopExits {
    if cx.loop_memo.is_none() || body.is_empty() {
        return presence_loop_walk(body, entry, cx);
    }
    let key = LoopKey {
        body: (body.as_ptr() as usize, body.len()),
        silent: cx.silent,
        entry: state_key(entry),
        seeded: seed_key(&cx.seeded_at),
    };
    let hit = cx.loop_memo.as_ref().and_then(|m| m.records.get(&key)).map(Rc::clone);
    if let Some(record) = hit {
        #[cfg(test)]
        tests::count_hit();
        for (read, seed) in &record.reads {
            if cx.seen.insert(read.span.start) {
                cx.out.push((read.clone(), *seed));
            }
        }
        for (name, at) in &record.reseeded {
            cx.seeded_at.insert(name.clone(), *at);
        }
        return record.exits.clone();
    }

    let pushed_from = cx.out.len();
    let exits = presence_loop_walk(body, entry, cx);
    let reseeded = cx
        .seeded_at
        .iter()
        .filter(|(name, at)| rewritten(&key.seeded, name, **at))
        .map(|(name, at)| (name.clone(), *at))
        .collect();
    let record =
        LoopRecord { exits: exits.clone(), reads: cx.out[pushed_from..].to_vec(), reseeded };
    if let Some(memo) = cx.loop_memo.as_mut() {
        memo.records.insert(key, Rc::new(record));
    }
    exits
}

#[cfg(test)]
mod tests;
