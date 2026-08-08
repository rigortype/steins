//! A **headroom** guard for the lowering walkers (issue #264).
//!
//! `SourceTree::parse` runs three dozen walkers of the same shape — a function
//! that recurses once per CST node — so a deeply nested expression (`$a->b->c->…`,
//! `$a[0][0][…]`, `'a' . 'a' . …`) costs one stack frame per nesting level. Where
//! headroom can be bought, issue #246 bought it: the CLI analyzes on a 256 MiB
//! worker thread and answers the whole question, because a depth cutoff over a
//! finite input manufactures a silence nothing calls for.
//!
//! **The wasm playground cannot buy it.** Its shadow stack is fixed at link time,
//! raising it moves the ceiling by about 2× before the host VM's own call stack
//! binds instead, and both failures are unrecoverable traps that name neither PHP
//! nor a line. There the alternative to a cutoff is not a complete answer, it is a
//! dead module. This is the guard for that case.
//!
//! # Why headroom and not a depth count
//!
//! One nesting level costs roughly 16 KiB of stack in a debug build and roughly
//! 2.7 KiB in a release one — a 6× difference in frame size, measured in
//! `docs/notes/20260808-deep-nesting-stack-budget.md`. A node-count constant
//! calibrated against release frames therefore overflows a debug build before it
//! can fire; Mago's own `MAX_RECURSION_DEPTH = 512` demonstrably does (a
//! 480-level parenthesis nest, *below* its limit, overflows debug). So this guard
//! counts **bytes of stack actually consumed**, not levels: the same budget is
//! correct under debug frames, release frames and wasm's shadow stack, and it
//! fires only on inputs the process genuinely cannot walk. Every input the
//! machine can answer is still answered in full.
//!
//! # Contract
//!
//! - The budget is **per thread** and, on every target except wasm, **off by
//!   default**: a library may not assume headroom it has not been given, and it
//!   may not refuse to answer on a stack that could have answered. Native entry
//!   points buy stack instead (`steins-cli`'s 256 MiB worker); an embedder that
//!   cannot should call [`set_budget`].
//! - On `wasm32` the default is [`WASM_BUDGET`], because there is no bigger stack
//!   to buy at any price and the module is otherwise one paste away from a trap.
//! - Tripping the guard is **not** a finding. It is recorded as a recovered parse
//!   error, which the checker names once as `syntax.unparsable` — the same
//!   vocabulary Mago's own recursion limit already surfaces through, and a named
//!   silence in the sense of ADR-0009.
//!
//! # What this does not fix, measured
//!
//! The guard bounds *Steins'* recursion. One recursion below it belongs to the
//! parser fork: `HasSpan::span` on a Mago CST node walks the node's spine, so
//! asking a 6,000-level chain's statement for its span pushes 6,000 frames —
//! from `lower_stmt` at walker depth 1, where no headroom guard can be standing,
//! and from inside the parser itself while it builds the chain. Those frames use
//! the host VM's call stack rather than the module's shadow stack (the trap they
//! produce is V8's `RangeError`, not an out-of-bounds linear-memory access), so
//! no shadow-stack budget preempts them.
//!
//! Measured in Node against the release module: the playground's ceiling moves
//! from between 300 and 600 levels (a trap) to about 1,700 (a trap), with
//! everything below it — including phpstan-src's real 1,000-level fixture — now
//! answered or refused by name. Raising it further means making the fork's span
//! accessors iterative, which is a change to the parser, not to this crate.
//!
//! `cargo test` is worth a word: libtest runs each test on a 2 MiB thread, which
//! is *smaller* than the 8 MiB stack issue #246 already found fatal at 520
//! levels. A test that parses a deep fixture in process must therefore set a
//! budget (as this module's own tests do) or drive the binary as a subprocess (as
//! `crates/steins-cli/tests/deep_nesting.rs` does).

use std::cell::Cell;

/// The default budget on `wasm32`, in bytes: half of wasm-ld's 1 MiB shadow
/// stack, which is the stack the playground module actually ships with.
///
/// **Half, not nearly all, and the other half is a reserve the guard needs.** The
/// budget bounds the *walk*; work the walker does below the check is not bounded
/// by it, and one piece of that work is itself proportional to the nesting depth:
/// `HasSpan::span` on a Mago expression recurses down the node's left spine, so a
/// walker that records a span at the depth where the guard fires still pushes one
/// (small) frame per remaining level. Spending the whole stack on the walk would
/// leave nothing for that and trade one trap for another. Measured at ~2.7 KiB
/// per level in release, this yields roughly 190 levels of nesting before the
/// refusal, against a hard ceiling between 300 and 600 where the module traps
/// instead — and leaves 512 KiB, thousands of span frames, underneath.
///
/// Raising `-z stack-size` is **deliberately not** the answer here, and the
/// reason is recorded rather than left to rediscovery: at 16 MiB the measured
/// ceiling only doubles and the failure mode changes from a shadow-stack
/// overrun — which this guard can preempt, because it is a linear-memory
/// address this module can read — to the host VM's `RangeError`, which it
/// cannot. A bigger shadow stack would trade a preemptible failure for an
/// unpreemptible one, and pay for it in the module's initial linear memory,
/// which `-z stack-size` adds outright.
pub const WASM_BUDGET: usize = 512 * 1024;

/// The default budget on every other target: none. See the module contract.
const NATIVE_BUDGET: usize = 0;

const DEFAULT_BUDGET: usize = if cfg!(target_family = "wasm") { WASM_BUDGET } else { NATIVE_BUDGET };

/// The message the refusal carries into `parse_errors`, and from there into the
/// one `syntax.unparsable` finding the file earns. It names the silence: what was
/// not walked, and why.
///
/// It names the *file*, not a line, and that is not laziness: asking Mago for the
/// span of the node where the guard fires means recursing down that node's spine,
/// which is the very depth the guard just refused to walk. A position bought that
/// way would cost more stack than the walk it is reporting on. What the reader
/// needs is here anyway — which file, what happened, and that the file's other
/// findings were withheld rather than computed from half a tree.
pub(crate) const REFUSAL: &str =
    "Maximum expression nesting exceeded: an expression in this file nests deeper than the \
     analyzer can walk on this stack, so the file was analyzed only in part";

thread_local! {
    /// Bytes of stack the lowering pass may consume, or 0 for "unbounded".
    static BUDGET: Cell<usize> = const { Cell::new(DEFAULT_BUDGET) };
    /// The stack address the walkers must not descend past, or 0 when no guard is
    /// installed on this thread (either unbounded, or outside a parse).
    static FLOOR: Cell<usize> = const { Cell::new(0) };
    /// Whether the guard fired during the current parse.
    static TRIPPED: Cell<bool> = const { Cell::new(false) };
}

/// Set this thread's lowering budget, in bytes; `0` disables the guard.
///
/// For an embedder that parses on a stack it cannot grow. Pass comfortably less
/// than the thread's real stack — what remains *below* the parse, minus room for
/// whatever the walkers call — since the guard measures consumption from the
/// parse entry, not from the thread's base.
pub fn set_budget(bytes: usize) {
    BUDGET.set(bytes);
}

/// This thread's lowering budget in bytes; `0` when the guard is off.
#[must_use]
pub fn budget() -> usize {
    BUDGET.get()
}

/// An approximation of the current stack pointer: the address of a local this
/// function forces into memory. Stacks grow downwards on every target Steins
/// builds for, wasm's shadow stack included, so a *smaller* value is a *deeper*
/// frame.
#[inline]
fn stack_probe() -> usize {
    let probe = 0_u8;
    std::ptr::addr_of!(probe) as usize
}

/// Installs the guard for one parse, and removes it again on drop.
///
/// Nested parses (a parse reached from inside a walker — nothing does this today)
/// keep the outer floor, which is the conservative reading: the inner parse is
/// running on stack the outer one already spent.
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
        // An inner scope's trip belongs to the inner parse; restore what the outer
        // one had recorded, so its own refusal is not attributed to a nested run.
        TRIPPED.set(self.previous_trip);
    }
}

/// Whether the walkers must stop descending.
///
/// This is the whole guard as the walkers see it: one thread-local read on the
/// common path, and — since a walker that stops descending simply returns — no
/// change to any walker's control flow beyond the descent it does not take. It
/// takes no span and asks for none, for the reason [`REFUSAL`] records.
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
            // 4 KiB per frame, so a 64 KiB budget is spent in ~16 levels however
            // the profile sizes the rest of the frame.
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
