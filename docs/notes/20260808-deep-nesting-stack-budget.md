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

## 6. What is left open

Filed for decision rather than answered here, because it needs a ruling and
probably an ADR amendment:

1. **A depth guard for the surfaces that cannot buy stack** (wasm, and any future
   embedder). The shape that respects the Certainty discipline is a *headroom*
   guard rather than a node-count budget — it fires only when the machine is
   about to die, so on every input the engine can answer, it answers fully — and
   it names its silence in the `syntax.*` family.
2. **Whether the guard belongs in `steins-syntax` at all**, or whether the 34
   walkers should be rewritten onto an explicit worklist. One shared visitor
   would fix all 34 at once and remove the question permanently; it is a large,
   mechanical, and risky change to the lowering pass.
3. **`cargo test` runs on libtest's 2 MiB threads**, so any future test that
   parses a deep fixture in-process (rather than as a subprocess, as
   `deep_nesting.rs` does) is on a stack smaller than the one already known
   fatal.
4. **Wiring `apps/playground/smoke.mjs` into CI**, with a deep-chain case.
