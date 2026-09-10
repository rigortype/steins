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
a `for (;;)` carries `CondExpr::Opaque`, which decides nothing and narrows
nothing, so its body walks unguarded.

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

**The order is the soundness, and it is the part worth reading twice.** The
negation is applied to the **post-forget** env — `writes` gone, `reads` and
the `for`'s `carried` kept — which is the same env the header applies to at
the entry. So it never states what a name held *before* the loop; it states
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

Fixtures: `crates/steins-infer/tests/loop_exit_condition.rs` — the shape and
its `if`/`else` twin, both post-forget readings, the four loop forms, the
`break`/`break 2`/nested-`switch`/nested-loop level matrix, `continue` and
`continue 2`, `goto`, and the `foreach` subject surviving into a second loop.
