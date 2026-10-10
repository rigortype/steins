# Propagation is staged through a linear trace IR; unknown lowers to Barrier

ADR-0001's engine is grown in provable stages, not built whole. The first
stage lowers each scope (function body, top-level script) to a **linear
trace IR** — an ordered statement list of `Assign`/`Call`/`Return`/`Barrier`
— where **everything not exactly recognized lowers to `Barrier`**.
Over-lowering to Barrier is always sound: it can only cause silence, never a
wrong finding. Control flow of any kind is a Barrier in this stage; a
variable's value is *known* at a use only along a barrier-free straight line
from its last literal assignment, and a scope containing aliasing machinery
(references, by-ref closure captures, variable-variables, `extract`,
`compact`, `global`, `include`, `eval`, `static`) is **poisoned** — nothing
in it is ever known. A constant function (body = exactly `return <literal>`)
propagates its value to zero-argument call sites in the same file.

The point of recording this: the IR's ratchet direction. Every future
precision gain (branch joins, interprocedural argument binding, shapes) is a
*refinement that removes Barriers or narrows poisoning* — each step lands
against a green fp-gate (ADR-0026), so precision only ever grows from a
sound floor. This is how "the program works outranks the worst-case
reading" becomes an implementation strategy rather than a slogan: we never
guess and then patch false positives away; we widen first and prove our way
narrower.

## Amendment (2026-09-03): a `while` body is a sub-trace, entered under its own header — PENDING ratification

Issue #649. The ratchet above forgets a construct's write and read sets and
keeps the rest of the env; that is what it does *to the code after* the
construct. What went unstated is what it does to the code *inside* one:
nothing at all, because the body never lowered. `while`, `for`, `foreach`,
`do`/`while` and `try` all became `StmtKind::Opaque`, a variant with sets
and no statements, so the walk had no body to enter and every trace-borne
finding inside a loop was silence. The same `call.undefined-method` fixture
fires at the top level and inside an `if` and said nothing inside a `while`;
scope-level families reading the CST (`variable.undefined`) fired in there
regardless, so the surface was inconsistent as well as incomplete.

`while` now lowers to `StmtKind::While`, carrying its condition and its body
as a sub-trace. The sets are unchanged and land exactly as before, so the
construct's effect on its successor is byte-identical. What they leave
standing is then also the body's **entry env**, narrowed by the header's
true-side refinements — the same application an `if`'s then-branch takes,
for the same reason: PHP evaluates the header immediately before every entry
to the body.

**No fixpoint, and this is the load-bearing part.** The entry env is not the
previous iteration's exit; it is an env in which every name the loop can
rebind is already ⊤ and every object it can mutate has been swept. Nothing
in it is specific to an iteration, so it is valid for all of them, the first
included — and a body whose last statement reassigns the very subject the
header narrowed on re-derives the fact at the next entry rather than losing
it. That shape is the motivating one: an AST traversal walking a parent
pointer types its subject from the loop header and from nowhere else,
because the accessor it calls is untyped.

The body contributes **findings, not facts**. Its exit env is discarded, so
what a loop computes cannot reach the code after it, and the negated
condition does not ride the fall-through — a `break` leaves a loop without
falsifying its header, so that is a separate question with a separate gate
(issue #651). A header the walk decides false leaves its body unwalked; the
region is not marked dead, since withdrawing what the env-free direct pass
already reports there is a separate judgment from adding what the walk now
reports.

`break` and `continue` had to stop being `Barrier`s for any of this to be
worth having. A `Barrier` *falls through* with a cleared env, so a guarded
`break` handed its `if`'s join an empty env and erased what the rest of the
body knew — free while bodies were unwalked, and the first thing a real body
hits now. They lower to `StmtKind::LoopJump`, a terminator: the statements
after one are unreachable and the branch holding one contributes nothing to
the join, exactly as a `return` does. Which loop a `break 2;` leaves stays
unmodelled and need not be modelled — the question a walker asks is whether
the code after it in *this* block is reachable, and the answer is no at every
level.

One cost stays, deliberately: `for`, `foreach` and `do`/`while` are still
body-less (issue #650), and `do`/`while` will need the entry narrowing
**withheld** when it arrives, its first iteration running before its
condition is ever evaluated.

`StmtKind::While` and `StmtKind::LoopJump` both sit before `Opaque` in the
enum and the wire codec carries a variant by index, so `SCHEMA_VERSION` moves
11 → 12.

## Amendment (2026-09-04): a loop body's entry env keeps what the loop cannot change — PENDING ratification

Issue #653, found by an adversarial review of the amendment above, which
first claimed the entry env forgot only `writes` and then had to own up: it
forgot **both** sets, and only one of those is right for an entry.

Forgetting `writes` is the by-ref conservatism — every name assigned in the
subtree and every name handed to any call in it — and stays. Forgetting
`reads` is right for the construct's **fall-through**, where a subtree that
reads and branches may have early-returned, so the tail must exclude the
value. It is wrong for a **body entry**: a name in `reads` is assigned by
nothing in the loop and handed to no call in it, so its binding holds on
every iteration. The paragraph two above says exactly this and the code did
the opposite.

Measured, that was most of the feature. A receiver is not an argument, so it
lands in `reads`, and `$o->tyop()` on a declared parameter was named inside
an `if` and silent inside a `while` — same statement, same proof available.
The same forgetting cost the **subtractive** guard vocabulary its base:
`instanceof` and the `is_*` family mint a fact and narrowed a loop header
from nothing, while `!== null` and truthiness subtract from a declared lane
that was no longer there. So inside a `while`, every fact was header-minted,
statement-local or env-free, and a declared parameter used in the loop
contributed nothing.

The construct therefore answers two questions from the same starting env,
neither of which can see the other. Its **fall-through** is what it always
was, sets and all. Its **body entry** drops `writes`, keeps `reads`, and is
then narrowed by the header.

**What a kept name may not keep** is the mutable state of the object it
refers to. A method call the body makes writes through a receiver that never
enters `writes`, so iteration 2 would read iteration 1's properties. Every
object a kept name points at therefore takes the sweep an escaping call
already performs: non-readonly properties and value generic carries go, the
class and the readonly properties stay. This is why the rule is not "forget
less" — the value, declared-arm and class lanes describe a binding the loop
cannot rebind, and the heap describes state it can. The sweep is
unconditional, not conditioned on the body actually calling a mutator: a
walk that proves a particular call harmless proves it for that call, and the
entry env is answering for all of them at once.

`poisons` clears both envs, unchanged: a subtree that aliases, `extract`s or
`eval`s has no binding worth carrying anywhere. The zero-iteration reading
moves to the entry env along with the narrowing, for the reason the
narrowing is sound there — the env holds at every evaluation of the header,
so a `No` there is a `No` at all of them.

The `writes` half is untouched and is a real cost still: recovering those is
ADR-0070's by-value survivor rule applied to a construct's sets rather than
a statement's, which moves every `Opaque`'s fall-through too. `for`,
`foreach` and `do`/`while` inherit this rule when their bodies start walking
(issue #650).

`SCHEMA_VERSION` does not move. Nothing about the trace's spelling changes,
so a stored trace from the amendment above replays into this walk unchanged;
what separates the two readings of it is the analyzer version, which the
generation fingerprint already covers (ADR-0092).

## Amendment (2026-09-10): the other three loop forms, and the one `do`/`while` refuses — PENDING ratification

Issue #650. The two amendments above are stated for `while` and argued for
loops. `for`, `foreach` and `do`/`while` now lower to structured variants of
their own — `StmtKind::For`, `StmtKind::Foreach`, `StmtKind::DoWhile` — each
carrying its body as a sub-trace entered from the same iteration-count-
agnostic env: `writes` forgotten, `reads` kept, the mutable state of every
object a kept name points at swept. Every construct's fall-through is
byte-identical to the `Opaque` it replaces. `try` is the only construct left
without a body, and stays one: its `finally` overwrites the exit point, which
is a control-flow question this IR does not answer yet.

**`for`.** Its header has two clauses a `while`'s does not, and they are on
opposite sides of the entry env. `init` runs **once**, before the condition is
ever evaluated, so the walk enters it in the env as it stands at the construct
— its findings are a top-level statement's, and a name it writes that the
condition, the increments and the body never write again cannot differ between
iterations, so the entry env keeps it (`carried`). The **increments** run after
every iteration, which is the loop-carried kind of write exactly, so their
targets are forgotten with the rest of `writes`; the increment expressions
themselves are not walked, since the env they run in is the body's exit, which
this construct discards. The tested condition is the **last** one PHP evaluates;
a `for (;;)` carries the literal `true` PHP evaluates there, which narrows
nothing, so its body walks unguarded (and, per the 2026-09-11 amendment,
never falls through when no jump can leave it).

**`foreach`.** A header that binds rather than tests, so there is no condition
and nothing to narrow by. `$k` and `$v` are ordinary members of `writes` — the
`foreach`-binding row the write collector has always had — so the entry env
leaves them defined but untyped, which is what they are. Typing them from the
subject's own value type is issue #652.

**`do`/`while` refuses the entry narrowing, and the variant does not carry the
condition at all.** The body's first iteration runs *before* the header is
evaluated, so taking the `while` rule there would be unsound in two directions
at once: it would narrow that iteration by a fact no test has established, and
it would read a `do { … } while (false)` — a loop whose body runs exactly once
— as a zero-iteration loop and walk nothing. A field no reader may consult is
a field that invites being consulted, so it is absent rather than ignored, and
both halves are pinned by fixtures whose expected answers invert under the
`while` rule.

What the bodies contribute is unchanged from #649: **findings, not facts**.
Every exit env is discarded, so the fall-through of each form is what its sets
alone leave standing and the negated condition still does not ride it
(issue #651).

The three variants sit after `While` and the wire codec carries a variant by
index, so `SCHEMA_VERSION` moves 14 → 15: a stored trace of the previous schema
must miss rather than decode, and would in any case spell all three constructs
as `Opaque`, replaying silence for bodies this analyzer judges.

## Amendment (2026-09-11): a `foreach` binds its targets from the subject's element type — PENDING ratification

Issue #652. The amendment above walks a `foreach` body and leaves `$k`/`$v`
defined but untyped. They are now bound from the subject's own element type,
which is a fact the engine already holds: `untyped.iterable-value` exists
precisely to demand it of an author, and the argument and return lanes already
carry it. No new inference happens — every answer is a projection of a
declaration or a witnessed array.

**The header is carried on the variant.** `StmtKind::Foreach` grows `subject`,
`key_var`, `value_var` and `by_ref`, each purely syntactic and each read by the
same two readers `ForeachSite` uses, so the trace variant and the ADR-0076 site
cannot disagree about what the header says. A subject or target that is not a
plain `$var` is `None`: it names nothing the env can be asked about.

**The binding is applied AFTER the entry forgetting, never instead of it.** Both
targets are ordinary members of `writes`, so the forgetting drops them first and
the element fact is put back from the subject *as the entry env holds it*. That
order is the whole of the iteration-count-agnosticism: a body that reassigns
`$v` writes into an env the construct discards, and a body that rebinds the
**subject** removes it from the entry env, so nothing is bound at all rather
than iteration 1's element type being stated as every iteration's. The
fall-through is untouched — `$v` after the loop is what the write set leaves it,
and carrying it out is still issue #651's question.

**Two lanes, in ADR-0037's trust order.** A witnessed array answers first and
exactly, at its own stratum, so `foreach ([1, 2] as $v)` may premise a proof.
Failing that the subject's declared arms answer, at the arms' stratum — a
docblock `@param list<int>` is `Asserted`, so the element fact can never premise
a proof-layer finding (ADR-0052 §5, the A-G9 corollary). `list<T>` gives
`int<0, max>` keys, `array<K, V>`/`T[]`/`iterable<K, V>` give what they state, a
**sealed** shape gives the union of its keys and of its values, and a typed tail
joins as one more alternative. Two array arms decline for A-G3's reason: the
element of a blur is the element of neither arm. A class element (`array<string,
Foo>`) has no `Fact` to be (ADR-0035/0043), so it is bound in the arm lane and
the two lanes are populated together, exactly as a declared parameter's are.

**What states no element type binds nothing.** A bare `array`, an unsealed shape
with an untyped tail, an empty witnessed array, a `mixed` binding: each leaves
both targets defined but untyped. Inventing a fact here is the thing
`untyped.iterable-value` exists to ask an author to fix, and ADR-0002's silence
is the correct answer until they do.

**Two refusals, both silent.** `as &$v` writes *through* the subject, and the
trace models neither that write nor the alias it installs (issue #677) — typing
`$v` would let iteration 2 read an element iteration 1 overwrote, so the by-ref
form binds nothing. A destructuring target (`as [$a, $b]`, `as list(...)`) and a
property/offset target (`as $this->x`) bind names the variant does not resolve,
and stay writes alone; a plain `$k` beside either still binds. `Traversable` and
`Generator` value types are out of scope by the issue's own ruling — they ride
`@implements`/`@extends` template arguments, a lane of their own. A declared
`iterable<K, V>` is not that case and is read like the map it spells.

`StmtKind::Foreach`'s payload gains fields and the wire codec reads a struct
variant's fields positionally, so `SCHEMA_VERSION` moves 15 → 16: a schema-15
payload must miss rather than read the old `body` where the new `subject` is.

## Amendment (2026-09-11): the statement after a break-free loop knows the header is false — PENDING ratification

Issue #651. Every amendment above ends by saying the fall-through is
byte-identical to the `Opaque` the construct replaces, and names this issue
as where that stops being true. It does, in two independent ways.

**The negated header.** A loop is left in exactly two ways: its condition
went false, or a jump left the body. When no jump can, the condition is
false at the statement after the loop, so its **else**-refinements hold
there — the mirror of the entry narrowing, through the same carrier
(`apply_cond_side`, false polarity) at the same strata (ADR-0052 §5: a
`Verified` test refines at `Verified`, an `Asserted` envelope at `Asserted`,
and nothing launders). `while`, `for` and `do`/`while` all qualify. A
`foreach` does not and cannot: it exits on exhaustion, not on a lowered
`CondExpr`, so there is nothing to negate.

**`do`/`while` takes it, and this is not a reversal.** The amendment above
withholds the *entry* narrowing there and withholds the condition field with
it. The exit is the opposite case for the identical reason: the condition is
evaluated **after** the body and **immediately before** the fall-through, so
a fact read off it is untested at the entry and freshly tested at the exit.
`StmtKind::DoWhile` therefore carries `cond` after all, with the one legal
reading written on the field.

**A header that can never fail, with no jump to leave by, never falls
through.** The header's verdict on the entry env is the verdict of every
test the loop makes, since that env holds at each of them. `Yes` plus
`break_free` therefore proves the successor unreachable — `while (true)
{ …; return; }` is the `if (true) { return; }` twin `walk_if` already
terminates — and the `while`/`for` arms answer `Flow::Terminated` rather
than walking dead code the old `Opaque` washed out by forgetting `reads`.
A `do`-`while` is not this question (issue #679, the 2026-10-02 amendment below).

**The order is the soundness, and it is the part worth reading twice.** The
negation is applied to the **post-forget** env — `writes` gone, `reads` kept
— which is the entry env minus the `for`'s `carried`: `init` is walked into
the body env only, so an init-only name is forgotten at the fall-through
like any other write (recoverable, out of scope here). So it never states what a name held *before* the loop; it states
what the failing test proves about what the name holds *now*, and the exit
is reached only through a failing test of exactly that value. Both readings
fall out of the order without a special case:

* a name the loop **rewrote** arrives with its lanes gone, so only a
  refinement that mints its own fact can say anything about it — `while ($x
  !== null) { $x = $x->parent(); }` proves `$x === null` at the exit, which
  is the issue's own shape, while `while ($x instanceof Node) { $x =
  $x->parent(); }` has no base to subtract `Node` from and answers silence.
  That silence is a precision residue, not an unsoundness: `$x` genuinely is
  not a `Node` there, and the domain has no lane to spell it in once the
  binding is gone.
* a name the loop **did not write** arrives with its lanes intact, so a
  subtractive negation has its base — `while ($i < 10)` over a declared
  parameter leaves `int<10, max>`.

**The gate is syntactic, and levels are counted.** `break_free` is computed
on the CST body at lowering, because the lowered trace cannot answer it: a
nested `switch` becomes an `Opaque` whose `break 2` is invisible, and
`StmtKind::LoopJump` deliberately records neither the keyword nor the level
(the #649 amendment's ruling, which stands — the *reachability* question it
answers still needs neither). Counting the loops and `switch`es between a
jump and the body's top level as `depth`: a `break N` targets this loop when
`N > depth`, so a bare `break;` inside a nested `switch` is the switch's and
a `break 2;` in the same place is this loop's; a `continue N` targets this
loop at `N == depth + 1`, which re-tests the header and is harmless, and
anything larger targets a loop outside this one and disqualifies. Any `goto`
disqualifies — its label is unbounded. A non-literal level (`break $n`,
which PHP has rejected since 5.4) is read as the worst case. `return`,
`throw` and `exit` disqualify nothing: they do not reach the fall-through,
so what holds there is not their business.

**The fall-through keeps `reads`.** The second way the fall-through stops
being an `Opaque`'s, and the one that moves every loop rather than only the
break-free ones. Forgetting `reads` is right for an `Opaque`, whose control
flow is unmodelled: a subtree that reads and branches may have early-
returned, so the tail must exclude the value it branched on. A **loop** does
not have that shape — a name in `reads` is assigned by nothing in the body
and handed to no call in it, so it holds at the fall-through exactly what it
held at the construct, under any iteration count including zero. That is the
2026-09-04 amendment's argument for the body's entry, verbatim, and it is as
true after the loop as inside it. A kept name takes the same
`Store::sweep_object` the entry gives it, for the same reason: the loop can
mutate the object it points at even though it cannot rebind the name.

Measured, that is what capped issue #652 at one loop per subject: a `foreach`
subject is a read, so `foreach ($xs as $v) {}` left `$xs` unknown and a second
`foreach` over the same declared subject bound nothing. `writes` is untouched
and stays the real cost it always was.

`StmtKind::While` and `StmtKind::For` gain `break_free`; `StmtKind::DoWhile`
gains `break_free` and `cond`. The wire codec reads a struct variant's fields
positionally, so `SCHEMA_VERSION` moves 16 → 17: a schema-16 payload would
read a `do`/`while`'s old `body` where the new `cond` is, and would in any
case replay `unknown` after every loop.

Fixtures: `crates/steins-infer/tests/it/loop_exit_condition.rs` — the shape and
its `if`/`else` twin, both post-forget readings, the four loop forms, the
`break`/`break 2`/nested-`switch`/nested-loop level matrix, `continue` and
`continue 2`, `goto`, and the `foreach` subject surviving into a second loop.

## Amendment (2026-10-02): a `do`-`while` body that terminates on every path terminates the loop — PENDING ratification

Issue #679. The amendment above answers `Flow::Terminated` for a `while` or
`for` whose header can never fail, and leaves the `do`-`while` out. The body
walk's own `Flow` was discarded for every loop form, on the reasoning that a
loop whose condition is not decided may run its body zero times. That holds
for `while`, `for` and `foreach`. It is false for `do`-`while`, whose body runs
at least once, so `$x = null; do { return; } while (false); $x->bar();`
reported `call.on-null` on a line nothing reaches.

**The body decides the successor, under one jump gate.** The body walk counts
a `StmtKind::LoopJump` as terminating its path, because it leaves the block it
is written in, but two of those jumps come back. A `break` of this loop lands
on the successor, and a `continue` of it lands on the condition, which may
then fail. `StmtKind::DoWhile` gains `nested_jumps_only`, computed on the CST
body at lowering with the depth counting `break_free` uses. It is `true` when
the body holds no `goto` and every `break`/`continue` in it has the literal
level 1 and sits inside a nested loop or `switch`; nested function-likes are
not descended. With it, every jump the body walk stopped at belongs to a
nested construct whose own walk has answered for it. What remains is
`return`, `throw`, `exit` and a `: never` call, and none of those comes back.
A body that terminates on every path only by `continue` is therefore not
terminated. `break_free` stays, for the exit negation; `nested_jumps_only`
implies it.

**Multi-level jumps are refused, though some are harmless.** The exact gate
would be `break_free` plus "no `continue` of this loop": a `break 2` that
leaves only an outer loop still inside the body does not come back. But the
walker's structured `switch` and the presence pass credit a jump to the
innermost breakable, so a multi-level jump out of a nested loop is
mis-credited by both (#904). A `switch` case holding `foreach (…) { break 2; }`
followed by `return` is structured as ending in its `return`, although the
jump lands after the switch. The gate therefore refuses every multi-level
jump until those passes count levels, and then relaxes to the exact one. The
cost is precision only: such a `do`-`while` keeps its successor live.

*Pointer (2026-10-11):* ADR-0103 makes both passes count levels (#904). This
gate is unchanged by it; the relaxation to `break_free && continue_free` is its
own slice (#1033 S2b) and waits on ADR-0103's ratification.

**The header is not read.** The `while (true)` rule above is deliberately not
extended to `do`-`while`, so `do { … } while (true);` still falls through in
the walker. The entry env misses a write that reaches a tested name through
an alias the loop's sets do not name. At file scope `$GLOBALS['go'] = false`
rewrites `$go` without putting `go` in `writes`, so a `Yes` read off `$go`
would silence a successor PHP does reach. The same hole exists for `while`
and is tracked on its own (#902); this amendment does not widen it.

**The syntactic and presence passes agree.** The `stmt_end` row for
`do`-`while` answers `Terminates` when the body passes `nested_jumps_only`
and its `block_end` terminates, and otherwise keeps the infinite-loop row it
had. `type.return-missing` and its `maybe-` sibling stop reporting a function
that ends in such a loop. A body whose end is undecided, such as one holding
a `try`, falls through as before; that over-report is older than this
amendment (#905). The binding-presence pass answers `Terminated` for a
`do`-`while` whose body reaches neither a `break` nor its back edge, under
the same gate. A branch join then drops that arm as it drops a `return`.

The trace payload changes shape, but under the 2026-09-27 narrowing
(`docs/internal-spec/generation-schema.md`) a trace-IR change moves the
analyzer version, and that refuses every stored trace. `SCHEMA_VERSION` does
not move.

Fixtures: `crates/steins-infer/tests/it/loop_exit_condition.rs`. They cover the
two shapes from the issue, nested constructs that own their own jumps, the
negative controls (a conditional return, `break`, `continue`, `break 2` and
`continue 2`, and a body that terminates only by `continue`), the
`$GLOBALS` alias that keeps the header unread, multi-level jumps the gate
refuses, and a terminating loop inside an `if` arm. Also
`crates/steins-syntax/tests/it/terminality.rs`,
`crates/steins-syntax/tests/it/binding_presence.rs`,
`crates/steins-syntax/tests/it/trace_stmt_lowering.rs` for
`nested_jumps_only`, and `crates/steins-infer/tests/it/return_missing.rs`.

## Amendment (2026-10-11): a `try` is a sub-trace, and terminates when its `finally` or every live arm does — PENDING ratification

Issues #943 and #905, slice S1 of the walker coverage run (#1033). After the
loop amendments above, `try` was the last control-flow construct that lowered
to `StmtKind::Opaque`. Nothing in a `try` block, a `catch` or a `finally` was
walked, so every trace-borne finding there was silence, and the walk always
fell through the construct. The second half was a default-surface false
positive: `$x = null; try { return; } finally { echo 1; } $x->bar();`
reported `call.on-null` on a line PHP never runs. Its terminality was
`Unknown` whole, and a `do`-`while` whose body held one fell back to
`FallsThrough`, so `type.return-maybe-missing` reported functions that return
on every path (#905).

`try` now lowers to `StmtKind::Try`, appended after `Barrier`. It carries the
block, each `catch` (its `CatchClause` as the throw system reads it, ADR-0040,
and its body), the `finally`, the whole construct's `Opaque` sets, and the
sets of the block alone and of the catch bodies together.

**Where control leaves each part.** Witnessed on PHP 8.5.11:

| part ends by | then |
| --- | --- |
| the block falls through | `finally` runs, then the successor |
| the block returns, `break`s or `continue`s | `finally` runs, then that exit proceeds |
| the block throws | a matching `catch` runs; with none, `finally` runs and the throw proceeds |
| the block calls `exit` | nothing else runs |
| a `catch` falls through | `finally` runs, then the successor |
| a `catch` exits or jumps | `finally` runs, then that exit proceeds |
| `finally` falls through | whatever was pending proceeds |
| `finally` returns or throws | it replaces whatever was pending, a throw included |
| `finally` jumps out | a compile error ("jump out of a finally block is disallowed") |

So the successor is reachable exactly when `finally`, if there is one, can
fall through, **and** the block or some live `catch` can. A `finally` that
falls through does not rescue a block that terminates. One that terminates
terminates the construct whatever the other parts do. The lowering
(`try_end`), the walker (`walk_try`) and the binding-presence pass each apply
this one rule to their own flow.

**A `catch` of a block that cannot throw is dead.** A `catch` is a
non-deterministic branch, except when every statement of the block is
throw-free, and the whitelist (`try_body_cannot_throw` in `lower_try.rs`) holds
three statements: an empty one, a bare `return;`, and a `return` of a literal
value (`return 1;`, `return [];`, `return ['a' => -1];`). That `return` throws
only when the declared return type rejects the literal's type. That is a proven
`TypeError` the return-type check reports on the statement itself, so reading
the statement as throw-free can only drop a `catch (TypeError)` arm of code
already convicted; a destructor that throws as the locals are released at the
`return` throws in the caller (witnessed). A plain assignment is not admitted,
though the presence pass's prologue reads one: witnessed on 8.5.11, `$conn =
null;` over an object whose `__destruct` throws, and `$ref = "many";` through a
reference to an `int` property, each throw inside the block (#1036's review).
The literal reading is narrow on the same evidence: a sign only over a number
literal (`-[]`, `-$a` with `$a = []` and `-"abc"` throw), an array key only as a
literal (`[$k => 1]` throws when `$k` holds an array). A dead `catch` is neither
walked nor marked dead. This
is what makes `try { return 1; } catch (Exception $e) { echo 1; }` terminate,
which #905's control shape `h` needed: that shape was itself a false
positive, not the correct report the issue took it for.

**The block runs straight-line.** It is walked on a copy of the construct's
entry env, exactly as the statements before it were, so its findings are a
top-level statement's.

**A handler enters with what came before it forgotten.** A `catch` is entered
from any point of the block. Its entry is the construct's entry with the
block's `writes` dropped and the objects its `reads` name swept. This is the
2026-09-04 amendment's argument for a loop body's entry, applied verbatim:
a name the block neither assigns nor hands to a call holds what it held at the
entry, but the object it points at may have been mutated through it. The
caught variable is then forgotten and seeded as a declared receiver of the
caught classes: a `Verified` arm per class on the contract lane, and the
declared heap object a parameter of that one type gets. The seed is withheld
unless every caught class is a known class-like that is provably a
`Throwable`. An interface that does not extend `Throwable` names only part of
what the object is, and a lane of it alone would call `$e->getMessage()`
missing. `finally` is entered from any point of the block or of one `catch`,
so its entry drops both parts' sets.

The remembered call results of ADR-0102 need nothing of their own here. The
handler entry drops a name through `Store::unbind`, which forgets the keys
that name it (§2.4 rule 1), and a condition in the block that rebinds a place
puts it in the block's `writes`. That is the loop entry's mechanism, and it
covers the same cases.

**The successor is an `Opaque`'s.** It forgets the whole construct's sets and
pushes the hidden-exit floor, exactly as before. A precise join of the block
and catch exits (`try { $x = 1; } catch (E $e) { $x = 2; }` leaving `1|2`) is
slice S1b, after this slice's A/B. At the top-level frame a statement in the
block may have run user code that rebinds any global (issue #762), so the
handler entries and the successor start from nothing there, while the block
applies the rule statement by statement.

**`goto` stays unbounded.** A `goto` or a label anywhere in the construct
keeps it `Unknown` in `stmt_end`, keeps the walker's successor live, and keeps
the presence pass falling through.

**Consumers that must not read the block's returns as the function's.**
`phpdoc-honesty`'s `contains_opaque` refuses a body holding a `try`, as it
refuses a loop. A `finally` that returns replaces the block's return value,
and a `catch` may return something else, so the visible returns beside it are
not the whole set. `collect_returns` does not descend into one either. The
summary walk keeps the hidden-exit floor for the same reason: a `return` the
block walk records is joined with the floor and never stands for the exit
set alone.

The trace payload changes shape. Under the 2026-09-27 narrowing
(`docs/internal-spec/generation-schema.md`), a trace-IR change moves the
analyzer version, which refuses every stored trace, so `SCHEMA_VERSION` does
not move.

Fixtures: `crates/steins-infer/tests/it/try_bodies.rs` holds the #943 table
(t1 to t10), the #905 table (f to j), a returning `finally` over a throwing
block, nested `try`s, a `try` in a loop with jumps in the block, the `catch`
and around the `finally`, the caught variable for one class, a multi-catch
and an interface, and a `catch` without a variable. Also
`crates/steins-syntax/tests/it/terminality.rs` for the rule table,
`crates/steins-syntax/tests/it/binding_presence.rs`, and the pins this
amendment flips in `return_missing.rs`, `variable_undefined.rs`,
`trace_annotation.rs` and `match_unhandled_throw.rs` (a `catch
(RuntimeException)` does not absorb an `UnhandledMatchError`, witnessed).
