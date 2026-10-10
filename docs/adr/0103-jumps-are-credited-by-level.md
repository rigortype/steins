# Jumps are credited by level

**Status: proposed (2026-10-11), PENDING ratification.** Designed by the
architect for the walker coverage run (issue #1033, decisions D3 and D4,
adopted under the owner's policy "proceed with the recommendations"). Lands
with slice S2, which fixes #904 and #944. ADR-0027's 2026-10-02 amendment
(#679) refuses multi-level jumps until this ADR holds; relaxing that gate is
slice S2b.

## 1. Context

Three passes ask where a `break`, a `continue` or a `goto` lands, and before
this ADR they gave three answers.

- **The loop lowering counted levels.** `break_free` and `nested_jumps_only`
  count the loops and `switch`es between a jump and the loop body
  (ADR-0027's 2026-09-11 and 2026-10-02 amendments), so `break 2` inside a
  nested `switch` is the loop's.
- **The structured `switch` did not.** Its stray-jump scan stopped at nested
  loops and `switch`es, so `case 1: foreach (…) { break 2; } return;` read as
  an arm ending in its `return`. With every other arm terminating too, the
  walker called the code after the switch dead, and PHP runs it (#904,
  `w904c`: PHP raises "Call to a member function bar() on null" on the line
  Steins skipped). Any jump the scan *did* see made the whole switch
  `Opaque`, and so did a `goto` anywhere in a case.
- **The presence pass credited the innermost construct.** It parked every
  `break` and `continue` state on the nearest loop or `switch`, so a
  `break 2` out of a `foreach` inside a case joined the `foreach`'s
  successor, and the read after the switch saw a binding that the jumping
  path never made (`w904b` `p`: PHP warns "Undefined variable $y"; Steins was
  silent at `strict`).

Separately, a `switch ($x)` whose last case had no `break` stayed `Opaque`
(#944), although that case has no next case to fall into. Only the
`switch (true)` chain (#928) let its last case end.

## 2. Decision

### 2.1 One rule, one helper

Every pass resolves a jump's target through `crates/steins-syntax/src/lower_jump.rs`.
Let `d` be the number of loops and `switch`es between the jump and the list the
question is about (a `try`, an `if` or a block adds nothing; a function-like or
class-like is another scope and is not descended). For a `switch` case body:

| jump | lands |
| --- | --- |
| `break N` / `continue N`, `N <= d` | inside the case, on a nested construct |
| `break N` / `continue N`, `N == d + 1` | on this switch's **successor**: the arm *lands* |
| `break N` / `continue N`, `N > d + 1` | on a construct outside the switch: it **ends the arm**, and that loop's own count owns it |
| level not a positive integer literal | PHP refuses to compile it (`break $n` since 5.4, `break 0` always); read as the worst case, landing and ending the arm |
| `goto` | its label is unbounded: the arm lands, carrying no fact |
| `return` / `throw` / `exit` | ends the arm, never lands |

A `continue` counts a `switch` as a level, and a `continue N` whose `N`th
level is a `switch` is `break N`. PHP 8.5.11 says so at compile time:
`"continue" targeting switch is equivalent to "break"`, and
`"continue 2" targeting switch is equivalent to "break 2"`. A jump out of a
`try` block or a `catch` runs the `finally` first (witnessed on 8.5.11).
PHP rejects a jump out of a `finally` at compile time.

### 2.2 The walker: `MatchArmT::lands`

A structured `switch` arm whose case body holds a landing jump carries an
`ArmLanding`: the whole case body's `writes`, `reads`, `clears` and `runs`.
The `default` body carries one in `StmtKind::Match::default_lands`.
`walk_match` adds one more fall-through edge for a live arm with a landing,
whatever the arm's own walk answered. The edge carries the env the arm was
entered with, forgotten by `loop_entry_forget`'s rule: `writes` dropped, the
objects named by `reads` swept, and everything cleared when `clears` is set.
`clears` is set by an ADR-0001 poison or by any `goto`, whose label lowers as
a barrier wherever it sits. The env is also cleared where the top-level rebind
rule fires on `runs` (issue #762). The sets are the whole body's, so a
`finally` the jump runs is forgotten too.

The design note spelled the field `Option<Vec<String>>`, the arm's writes.
Writes alone would keep an object property a method call in the case body
changed before the jump, and would keep facts at file scope that a userland
call rebound. The struct carries the three further inputs that close both.

A jump that ends the arm lowers as `StmtKind::LoopJump` and terminates the
arm's walk. A trailing `break;`/`break 1;`, and now also `continue;`/
`continue 1;`, is stripped as the switch's own end. A trailing `break 2` is
no longer a refusal: it stays in the body as a `LoopJump`. A `goto` no longer
refuses the construct. A `switch (true)` chain is an `if`, which has no
landing edge, so a chain with a landing arm stays `Opaque`.

`switch_end`, the `BodyEnd` row, is unchanged. It still answers `Unknown`
for a switch with any jump in it, which costs recall only.

### 2.3 The presence pass: parked jumps carry their level

`breaks` and `continues` become `(level, state)` pairs. Each loop and
`switch`, as it is left, takes the level-1 entries as its own and re-parks
the rest one level lower. A `switch` takes a level-1 `continue` as its
`break`. A loop's answer (`LoopExits`) carries the jumps that escape it, so
the loop-body memo (#793) replays them on a hit. A `try` gives the states
parked since it began the bindings its `finally` makes, as it already gave
them to its normal-completion path.

### 2.4 The last case may end (D4)

A non-empty case followed only by empty labels may run off its end. It
leaves the switch as a `break` would, for the by-value `switch` as for the
chain. Trailing empty labels become an empty arm, or an empty `default`. In
the presence pass, only the last case's fall-off state joins the successor.
A non-last case that runs off its end runs into the next case, and its state
used to join the successor too. That join made `switch ($c) { case 1: $y = 0;
case 2: $x = 1; break; default: $x = 2; } echo $x;` report `$x` at `strict`,
although every PHP path binds it.

A case that runs into a later non-empty case still keeps the construct
`Opaque`. That fall-through edge is not modelled.

## 3. Consequences

- **Findings can appear:** findings after a switch whose case jumps past it
  (`call.on-null` in `w904c`); every walked finding inside arms of a switch
  that used to be `Opaque` because of a `break 2`, a `continue`, a `goto`, a
  braced `break`, or a last case without `break` (#944);
  `variable.maybe-undefined` at `strict` after a switch whose case jumps past
  it, or whose `continue` is its `break`.
- **Findings can disappear:** findings on code after a `break 2` from a case,
  where every other arm terminates, because the walk used to keep that code
  live through the `Opaque`; `variable.maybe-undefined` where a non-last case
  ran into a binding case, or where a `finally` binds before a jump lands.
- `phpdoc-honesty` refuses a body with a landing arm. The landing edge is not
  in the arm's trace, so its returns are not the function's whole set.
- The trace payload changes shape. Under the 2026-09-27 narrowing
  (`docs/internal-spec/generation-schema.md`), a trace-IR change moves the
  analyzer version, and that refuses every stored trace. `SCHEMA_VERSION`
  does not move.

## 4. Measurement

Public corpus (the ten packages under `corpus/`, 85 `switch`, 43 multi-level
jumps, 7 `goto`), base cd4455cc against the S2 head, `--no-cache`: `check` at
`strict`, at `default` and under the fp-gate posture (`--no-php
--vendor-diagnostics`), the five `transform` dry-runs and `effect-diff` are
identical on every package. The probes of the run's design (`w904`, `w904b`,
`w904c`, `w944`) move exactly as its witness table says, each row matching PHP
8.5.11.

## 5. Fixtures

`crates/steins-infer/tests/it/switch_jumps.rs` (the #904 and #944 witnesses,
`break 2` out of a switch in a loop, `continue 2` out of a nested loop, a mid-arm
`break`, `continue` in a switch, `goto` in a case, a jump through `try`/`finally`,
a last case ending in a call, trailing empty labels, the fall-through control);
`crates/steins-syntax/tests/it/binding_presence.rs` (levels, `continue` as
`break`, the last case, `finally` on a jump);
`crates/steins-syntax/tests/it/trace_stmt_lowering.rs` (`lands` per arm, the
last case and trailing labels); `crates/steins-syntax/tests/it/guard_lowering.rs`
(the by-value last case, a chain with a landing arm);
`crates/steins-infer/tests/it/no_effect.rs` (the shapes that flipped from
opaque); the wire round-trip fixture in `crates/steins-db/src/persist.rs`.
