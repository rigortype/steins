# Deep expression nesting and the stack budget of each entry point

Measured 2026-08-08 against `4900ed9`, on `aarch64-apple-darwin` (`ulimit -s`
8176 KiB), toolchain 1.97.0. Issue #246 established that `scan_effect_origins`
recurses one frame per CST node and that phpstan-src's benchmark fixture
`tests/bench/data/nullsafe-chain-walk.php` (property-fetch chains up to 1,000
`->next` deep) overflows a debug build's stack; PR #253 gave the nsrt harness a
256 MiB worker thread and left the engine alone, on the reasoning that the
library may not assume headroom it has not been given.

This note asks whether the engine itself needed protection. **It did.** The
harness was not the only thing walking that fixture, and on one surface — the
wasm playground — headroom turns out not to be purchasable at all.

## 1. What recurses, and from where

The recursion is not one function. `steins-syntax` holds **34 walkers of the
same shape** — `for child in node.children() { walk(&child, …) }` with no depth
guard — and all of them run inside `SourceTree::parse`, the lowering pass, not
inside inference. `scan_effect_origins` (`crates/steins-syntax/src/lib.rs:5876`,
self-call at `:5973`) is merely the one issue #246 happened to witness; its
siblings include `scan_throw_origins` (`:6048`), `scan_var_usage` (`:7725`),
`collect_scopes` (`:7112`) and `scan_opaque` (`:9962`). Because they hang off
`parse`, **every** entry point that reads a `.php` file reaches them. There is no
subcommand that is safe by construction.

The Mago parser is not the first thing to break. It carries a real guard —
`MAX_RECURSION_DEPTH = 512` in `crates/syntax/src/parser/mod.rs` — but
`parse_expression_continuation` consumes left-associative and postfix chains
**iteratively**, rewriting `left` in one stack frame, so `$a->b->c->…` parses at
recursion depth 1 while producing a 1,000-deep tree. The guard fires only for
right-nested growth (`(((…)))`, `f(g(h(…)))`, `??`, `**`). Steins' own walkers
are what a `->` chain blows.

The CST itself is safe to drop: `parse` allocates into a `LocalArena` whose
nodes are `Box::leak`ed, so no recursive `Drop` glue runs. The lowered Steins IR
is not — `ArgValue` and `CondExpr` are `Box`-based, and their derived
`Hash`/`PartialEq`/`Clone` (which salsa runs on every revision to backdate
`parse`) recurse over `$a[0][0][…]`, long `.` concatenations, and long `&&`
chains. A `->` chain terminates early in `lower_arg_value`, so issue #246's
specific shape does not reach that family — but `[0][0][…]` and `.` chains do,
and they overflow at the same order of depth (measured below).

## 2. Measured ceilings, before the fix

`steins check --no-php` over a single `$n->next->…->next` chain of depth N,
bisected. "Ceiling" is the largest depth that completed.

| Surface | Stack | Ceiling (`->` levels) |
| --- | --- | --- |
| `steins check`, debug | 8 MiB (OS main thread) | between 520 and 530 |
| `steins check`, release | 8 MiB (OS main thread) | between 2,500 and 3,000 |
| `steins check`, release, 2 MiB | 2 MiB (rayon / libtest default) | between 600 and 1,000 |
| `steins check`, release, 1 MiB | 1 MiB | between 300 and 600 |
| wasm playground | ~1 MiB shadow stack | between 300 and 600 |

That works out to roughly 16 KiB of stack per nesting level in debug and
roughly 2.7 KiB in release — a 6× difference in frame size between profiles,
which is the reason a single depth constant cannot be calibrated for both.

`[0][0][…]`, `'a' . 'a' . …` and `1 + 1 + …` chains sit in the same release band
(between 2,000 and 5,000). Nested array literals (`[[[…]]]`, `['k' => …]`) and
nested `if` statements survive 10,000, so the exposure is expression chains, not
nesting in general.

Two consequences worth stating plainly:

- **A real file in a real repository was past the release ceiling on two of
  these surfaces and past the debug ceiling on all of them.** phpstan-src's
  fixture is 1,000 levels, i.e. ~40% of the way to the 8 MiB release ceiling and
  roughly double the debug one. `steins check` on it aborted with
  `fatal runtime error: stack overflow`.
- **The parser's own guard is unreachable in a debug build.** A 480-level
  parenthesis nest — *below* Mago's 512 limit — overflows debug before the guard
  can fire. In release the same input produces a clean
  `error[syntax.unparsable]: … Maximum recursion depth exceeded`. A depth
  constant calibrated against release frames is simply wrong under debug frames,
  which is direct evidence against answering this with one number in Steins.

## 3. Why an abort is worse than either alternative

The Certainty discipline (ADR-0009) governs the choice between a finding and a
silence. A stack overflow is neither. It is a process death that reports nothing
about the file that caused it **and nothing about the other files in the
project** — the whole run is lost, with no diagnostic, no exit-code contract, and
nothing a baseline could record. Whatever the right answer is, the current
behaviour is below the floor the discipline sets.

Issue #246 ruled that a depth cutoff over a finite input manufactures a silence
nothing calls for. That ruling is correct **where headroom can be bought**, and
that is what the fix below does: it buys headroom and the engine keeps answering
the whole question. The ruling's premise is that a bigger stack is available.
Section 5 records the one surface where it is not.

## 4. What landed

- `crates/steins-cli/src/main.rs`: `dispatch` now runs on a worker thread sized
  at 256 MiB (`WORKER_STACK_SIZE`), matching the nsrt harness's constant. One
  spawn covers every subcommand, because every subcommand parses. Measured
  after: the debug ceiling moves from ~520 to between 10,000 and 15,000 levels,
  the release build clears a synthesized 50,000-level chain (from ~2,700), and
  phpstan-src's fixture completes in a plain debug build.
- `xtask/src/main.rs`: the global rayon pool is now built with a 256 MiB
  `stack_size`. `fp-gate` and `freq` fan out with `par_iter`, so their parsing
  ran on rayon's **2 MiB** default — a quarter of the stack issue #246 already
  found fatal, in a debug-built CI job, over a corpus that includes an unpinned
  local checkout.
- `crates/steins-cli/tests/deep_nesting.rs`: drives the real binary over a
  3,000-level chain, past both the debug and the release ceiling, and asserts it
  neither prints an overflow nor dies by signal. It fails on the pre-fix binary
  (verified by reverting the `main.rs` change) — a stack overflow is not a
  catchable panic, so running the binary as a subprocess is what makes the
  failure assertable.

256 MiB is virtual address space the OS commits page by page as frames are
touched, so a run that never nests deeply pays nothing for it.

## 5. The wasm playground: headroom is not purchasable there

`sw_check` reaches the same walkers. Probed in Node against the real release
module:

| Shadow stack (`-z stack-size`) | Host | Ceiling | Failure mode |
| --- | --- | --- | --- |
| 1 MiB (link default) | V8 default | 300–600 | `RuntimeError: memory access out of bounds` |
| 16 MiB | V8 default | 600–1,000 | `RangeError: Maximum call stack size exceeded` |
| 16 MiB | `node --stack-size=8000` | ≥2,000 | — |

Raising the module's shadow stack moves the ceiling by a factor of about two and
then stops mattering: the binding constraint becomes **the host VM's own call
stack**, which a wasm module cannot raise and a web page cannot configure. Both
failures are unrecoverable traps that surface as a JavaScript error naming
neither PHP nor a line number.

So the playground overflows on a file that exists in the wild, and no link flag
fixes it. This is the case where #246's ruling does not reach: the alternative to
a named cutoff is not a complete answer, it is a dead module. A refusal that says
*this expression nests deeper than the analyzer can walk* is strictly more
informative — and it already has a precedent one layer down, in Mago's
`RecursionLimitExceeded` surfacing as `syntax.unparsable`.

Also worth recording: `apps/playground/smoke.mjs` describes itself as "the CI
gate before any artifact upload", but no workflow invokes it — `smoke.mjs`,
`wasm32` and `steins-wasm` appear nowhere in `.github/workflows/`. Nothing would
have caught this.

### 5.1 What landed for it (issue #264, 2026-08-09)

A **headroom** guard, not a depth counter: `crates/steins-syntax/src/stack_guard.rs`
records the stack address at the top of `SourceTree::parse` and refuses once the
walk has consumed a budget of bytes below it. Bytes, because §2's 6× frame-size
gap between profiles is exactly what a node count cannot straddle; the same
budget refuses under debug frames and release frames alike, and every input the
machine can walk is still walked in full.

The walkers reach it through one function. All 36 descents in `lib.rs` went from
`node.children()` to a `children(node)` helper that hands back an empty child
list when the headroom is gone, so a walker out of stack returns exactly as it
would at a leaf — no walker's control flow changed. The five expression
recursions that descend through typed sub-nodes instead of `children()`
(`lower_arg_value`, `lower_cond`, `lower_guard_arg`, `lower_concat`,
`bind_lvalue_roots`) check the guard at their entry and return the unproven
answer they already give an unmodelled shape.

The budget is **off by default everywhere except wasm**, where it is half the
1 MiB shadow stack. A library may not refuse on a stack that could have answered,
and on native nothing has to: §4 bought the headroom. `stack_guard::set_budget`
is there for an embedder that cannot.

`-z stack-size` stays at wasm-ld's default, decided rather than defaulted: the
table above shows 16 MiB buys about 2× and then changes the failure from a
shadow-stack overrun — a linear-memory address this module can *read*, and
therefore preempt — into the host VM's `RangeError`, which it cannot. It also
costs that much initial linear memory outright. A bigger stack would trade a
preemptible failure for an unpreemptible one and pay for the privilege.

Measured after, in Node against the release module (fresh instance per depth):

| Depth (`->` levels) | Before | After |
| --- | --- | --- |
| 100–200 | analyzed | analyzed |
| 300–400 | `RuntimeError: memory access out of bounds` | `syntax.unparsable`, module alive |
| 600–1,600 | trap | `syntax.unparsable`, module alive |
| 1,800+ | trap | `RangeError: Maximum call stack size exceeded` |

The residual ceiling is **not** the guard's to move, and it is worth stating
precisely because the next person will otherwise re-derive it: `HasSpan::span`
on a Mago CST node recurses down that node's spine. `lower_stmt` asks a statement
for its span at walker depth 1 — above any guard — and the parser joins spans the
same way while building the chain, so a 1,800-level chain pushes ~1,800 small
frames that never touch the shadow stack (hence V8's `RangeError` rather than an
out-of-bounds access, and hence no budget preempts them). Making the fork's span
accessors iterative is what moves that number; it is a change to the parser, not
to Steins.

Two smaller consequences, both landed: the deep-chain case is now asserted by
`apps/playground/smoke.mjs`, and that file is finally *invoked* — by the `wasm`
job in `ci.yml`, which builds the module for `wasm32-unknown-unknown` and runs
the suite over it.

## 6. What is left open

Filed for decision rather than answered here, because it needs a ruling and
probably an ADR amendment:

1. ~~**A depth guard for the surfaces that cannot buy stack**~~ — answered by
   §5.1: a headroom guard, on by default only where headroom cannot be bought,
   naming its silence as `syntax.unparsable`.
2. **Whether the guard belongs in `steins-syntax` at all**, or whether the 34
   walkers should be rewritten onto an explicit worklist. One shared visitor
   would fix all 34 at once and remove the question permanently; it is a large,
   mechanical, and risky change to the lowering pass. §5.1 narrowed rather than
   answered this: the descents now go through one function, so a worklist has a
   single seam to replace — but it would also retire the CST half of the guard
   entirely, and it would *not* retire the span recursion §5.1 measured.
3. ~~**`cargo test` runs on libtest's 2 MiB threads**~~ — written down as a
   convention in
   [the verification apparatus](../internal-spec/verification-apparatus.md), and
   demonstrated both ways (`crates/steins-syntax/tests/deep_nesting.rs` sets a
   budget; `crates/steins-cli/tests/deep_nesting.rs` uses a subprocess). Not
   *enforced*: nothing stops a new in-process deep parse from aborting the suite.
4. ~~**Wiring `apps/playground/smoke.mjs` into CI**~~ — done, §5.1.
5. **Mago's recursive `HasSpan`** (new, from §5.1): the parser fork computes a
   node's span by walking its spine, which is now the binding constraint on the
   playground and a hidden multiplier on every surface's stack cost. Making it
   iterative is a fork change.
6. **The Steins IR's derived `Hash`/`PartialEq`/`Clone`** (§1) are still
   unguarded: salsa runs them on every revision to backdate `parse`, and they
   recurse over `ArgValue`/`CondExpr` shapes this slice did not touch. The guard
   protects the lowering, not the comparison of what it produced.
