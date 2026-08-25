//! Headroom guard for the lowering walkers (issue #264).
//!
//! `SourceTree::parse` recurses once per CST node across dozens of walkers, so
//! deeply nested expressions cost one stack frame per nesting level. Native
//! entry points buy headroom instead (issue #246: `steins-cli`'s 256 MiB
//! worker thread), because a depth cutoff over a finite input is an
//! unforced silence.
//!
//! The wasm playground cannot buy headroom: its shadow stack is fixed at
//! link time, raising it only moves the ceiling ~2x before the host VM's own
//! call stack binds, and both failures are unrecoverable traps naming
//! neither PHP nor a line. This guard exists so the playground fails by
//! name instead of by trap.
//!
//! # Why headroom and not a depth count
//!
//! One nesting level costs ~16 KiB of stack in debug and ~2.7 KiB in
//! release (6x difference, measured in
//! `docs/notes/20260808-deep-nesting-stack-budget.md`). A node-count constant
//! calibrated for release overflows debug first: Mago's own
//! `MAX_RECURSION_DEPTH = 512` does (a 480-level parenthesis nest, below its
//! limit, overflows debug). So this guard counts bytes of stack consumed,
//! not levels, and fires only on inputs the process genuinely cannot walk.
//!
//! # Contract
//!
//! - Budget is per-thread, off by default on every non-wasm target — native
//!   entry points buy stack instead; an embedder that cannot should call
//!   [`set_budget`].
//! - On `wasm32` the default is [`WASM_BUDGET`]: there is no bigger stack to
//!   buy at any price.
//! - Tripping the guard is recorded as a recovered parse error, surfaced as
//!   `syntax.unparsable` (same vocabulary as Mago's own recursion limit), a
//!   named silence per ADR-0009.
//!
//! # What this does not fix, measured
//!
//! `HasSpan::span` on a Mago CST node walks the node's spine, so a
//! 6,000-level chain's statement pushes 6,000 frames from `lower_stmt` at
//! walker depth 1 and from inside the parser itself — on the host VM's call
//! stack (V8's `RangeError`), not the module's shadow stack, so no budget
//! here preempts it.
//!
//! Measured in Node against the release module: the playground's ceiling
//! moves from 300-600 levels (trap) to ~1,700 (trap), covering
//! phpstan-src's real 1,000-level fixture. Raising it further requires
//! making the fork's span accessors iterative — a parser change, not this
//! crate's.
//!
//! `cargo test` runs each test on a 2 MiB thread, smaller than the 8 MiB
//! stack issue #246 found fatal at 520 levels. A test parsing a deep
//! fixture in-process must set a budget (as this module's own tests do) or
//! drive the binary as a subprocess (as
//! `crates/steins-cli/tests/deep_nesting.rs` does).

use std::cell::Cell;

/// Default `wasm32` budget, in bytes: half of wasm-ld's 1 MiB shadow stack.
///
/// Half, not nearly all: the budget bounds the walk, but `HasSpan::span`
/// below the check costs one frame per remaining level, proportional to
/// nesting depth, so the other half is reserve for that. At ~2.7 KiB/level
/// in release this yields ~190 levels before refusal, against a 300-600
/// level hard ceiling where the module traps — leaving 512 KiB, thousands
/// of span frames, underneath.
///
/// Raising `-z stack-size` is deliberately not the answer: at 16 MiB the
/// measured ceiling only doubles, and the failure mode changes from a
/// preemptible shadow-stack overrun to the host VM's unpreemptible
/// `RangeError`, while paying for the larger stack in the module's initial
/// linear memory.
pub const WASM_BUDGET: usize = 512 * 1024;

/// Default budget on every other target: none. See the module contract.
const NATIVE_BUDGET: usize = 0;

const DEFAULT_BUDGET: usize = if cfg!(target_family = "wasm") { WASM_BUDGET } else { NATIVE_BUDGET };

/// Refusal message surfaced into `parse_errors` as the one `syntax.unparsable`
/// finding: names what was not walked and why.
///
/// Names the file, not a line: getting a span for the node where the guard
/// fires means recursing down that node's spine — the depth just refused.
pub(crate) const REFUSAL: &str =
    "Maximum expression nesting exceeded: an expression in this file nests deeper than the \
     analyzer can walk on this stack, so the file was analyzed only in part";

thread_local! {
    /// Bytes of stack the lowering pass may consume, or 0 for "unbounded".
    static BUDGET: Cell<usize> = const { Cell::new(DEFAULT_BUDGET) };
    /// Stack address the walkers must not descend past, or 0 when unguarded.
    static FLOOR: Cell<usize> = const { Cell::new(0) };
    /// Whether the guard fired during the current parse.
    static TRIPPED: Cell<bool> = const { Cell::new(false) };
}

/// Sets this thread's lowering budget, in bytes; `0` disables the guard.
///
/// For an embedder that parses on a stack it cannot grow. Pass comfortably
/// less than what remains below the parse, minus room for the walkers'
/// own calls — the guard measures consumption from the parse entry.
pub fn set_budget(bytes: usize) {
    BUDGET.set(bytes);
}

/// This thread's lowering budget in bytes; `0` when the guard is off.
#[must_use]
pub fn budget() -> usize {
    BUDGET.get()
}

/// Approximates the current stack pointer via a local forced into memory.
/// Stacks grow downwards on every Steins target, wasm included, so a
/// smaller value is a deeper frame.
#[inline]
fn stack_probe() -> usize {
    let probe = 0_u8;
    std::ptr::addr_of!(probe) as usize
}

/// Installs the guard for one parse, removes it on drop.
///
/// A nested parse (nothing does this today) keeps the outer floor: it is
/// running on stack the outer parse already spent.
pub(crate) struct Scope {
    previous_floor: usize,
    previous_trip: bool,
}

impl Scope {
    pub(crate) fn enter() -> Self {
        let previous_floor = FLOOR.get();
        let previous_trip = TRIPPED.replace(false);
        if previous_floor == 0 {
            let budget = BUDGET.get();
            FLOOR.set(if budget == 0 { 0 } else { stack_probe().saturating_sub(budget) });
        }
        Self { previous_floor, previous_trip }
    }

    /// Whether the guard fired during this scope.
    pub(crate) fn tripped(&self) -> bool {
        TRIPPED.get()
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        FLOOR.set(self.previous_floor);
        // Restore the outer scope's trip so a nested run's refusal isn't misattributed.
        TRIPPED.set(self.previous_trip);
    }
}

/// Whether a floor is installed for the current parse. Read by [`crate::memo`],
/// which refuses to cache while one is: a walk the guard may truncate is no
/// longer a pure function of the subtree it was launched on.
#[inline]
pub(crate) fn guarded() -> bool {
    FLOOR.get() != 0
}

/// Whether the walkers must stop descending: one thread-local read on the
/// common path, no control-flow change beyond the descent not taken.
#[inline]
pub(crate) fn exhausted() -> bool {
    let floor = FLOOR.get();
    if floor == 0 || stack_probe() >= floor {
        return false;
    }
    TRIPPED.set(true);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_guard_is_off_by_default_on_native() {
        assert_eq!(budget(), 0, "a native build must not refuse on a stack that could answer");
    }

    #[test]
    fn a_budget_installs_a_floor_and_a_scope_restores_it() {
        set_budget(64 * 1024);
        assert_eq!(FLOOR.get(), 0, "no floor outside a parse");
        {
            let scope = Scope::enter();
            assert!(FLOOR.get() > 0, "the floor is installed for the parse");
            assert!(!scope.tripped(), "an untouched guard does not trip");
        }
        assert_eq!(FLOOR.get(), 0, "the floor is removed again");
        set_budget(0);
    }

    #[test]
    fn a_deep_enough_descent_trips_the_guard() {
        fn descend(depth: u32) -> u32 {
            // 4 KiB per frame, so a 64 KiB budget is spent in ~16 levels.
            let ballast = [0_u8; 4096];
            if exhausted() {
                return depth;
            }
            let reached = descend(depth + 1);
            std::hint::black_box(&ballast);
            reached
        }

        set_budget(64 * 1024);
        let scope = Scope::enter();
        let reached = descend(0);
        assert!(reached > 0, "the guard let some descent happen");
        assert!(reached < 1_000, "the guard fired long before libtest's 2 MiB thread died");
        assert!(scope.tripped(), "and the parse learns that it did");
        drop(scope);
        set_budget(0);
    }
}
