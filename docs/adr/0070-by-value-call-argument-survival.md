# A fact survives a call that takes its variable by value

Issue #80, measured during #76. Status: PENDING ratification (autonomous
design under the owner's post-hoc-ratification mode).

## 1. Context: a sound rule paying for what PHP already guarantees

The linear walk forgets every variable a statement hands to **any** call, at
that statement's end (`Stmt::invalidated`, fed by `collect_call_vars`). The
rule is sound and it is one line: a callee's `&$x` parameter is an alias of
the caller's lvalue, and the lowering cannot see the callee's signature, so
the walk assumes the worst about every argument of every call.

What it costs was measured, not guessed. Three rows of the nsrt oracle stood
blocked on exactly this, each with a working first line and a dead second one:

* **#76** — `array_first_last.php:21-23`. Lines 15-17 match; 21-23 cannot,
  because `array_first($arrayShape)` on line 15 forgot `$arrayShape` and
  `array_last($arrayShape)` on line 21 has nothing left to read. `array_first`
  does not touch its argument.
* **#77** — `lowercase-string-trim.php`. The same parameter is read through
  `trim`, `ltrim`, `rtrim`, `chop`, once per line. `trim` cannot write its
  argument either.
* **#74** — `lowercase-string-sprintf.php:29/30/36/37`. `$constant` is a
  two-value union consumed by one `sprintf` per line; every line after the
  first needs it still to be a union.

None of those is a soundness question. PHP passes scalars, strings and arrays
**by value**, with copy-on-write: the callee's parameter is a separate zval,
and writing it cannot reach the caller's binding. The blanket drop is paying a
precision price for a hazard the language has already excluded.

Measured at `PINNED_PHP` (8.5.8) rather than recited:

```text
function f(string $x) { $x = 'z'; }  $s = 'abc'; f($s); $s === 'abc'  → true
function g(string &$x) { $x = 'z'; } $t = 'abc'; g($t); $t === 'z'    → true
$a = [1, 2, 3]; array_first($a); count($a) === 3                      → true
$b = [1, 2, 3]; array_pop($b);   count($b) === 2                      → true
$s = 'aaa'; preg_match('/a/', $s, $m); $s === 'aaa'                   → true
```

## 2. Decision

The blanket drop stays the **default and the floor**. A variable's fact
survives a statement's calls only when all five conditions hold; anything
uncertain, unrecognized or unlisted keeps the old behavior unchanged.

1. **The callee resolves with a known signature.** Either a project function
   the index holds — its declared `Param::by_ref` answers the question
   directly — or a builtin whose argument semantics the catalog states.
2. **The argument is by value at that position.** Call-time pass-by-reference
   was removed in PHP 8, so this is a property of the declaration alone.
3. **The variable denotes a value-semantic thing.** Scalar, string or array.
   An object binding always drops: the handle is copied, the object is not,
   and the callee may write its properties.
4. **Neither scope is poisoned** — not the caller's, and (for a project
   callee) not the callee's body either.
5. **Language constructs never route through this path.** `isset`, `empty`,
   `unset`, `list` are not call nodes; they never reached the blanket
   collector and they do not reach the precise one.

### 2.1 The catalog states argument semantics three-valued

`steins_catalog::by_value_arg(name, position) -> Option<bool>`:

* `Some(false)` — a certified by-reference position;
* `Some(true)` — a certified by-value position;
* `None` — **the catalog does not know this name**. Silence is not a promise.

Two tables compose to answer it, and the composition is the load-bearing part.
An `out_params` row (ADR-0063 §2.3) is transcribed from the php-src stubs *per
name* and lists every fixed positional reference parameter that name has, so
for a name carrying a row the row is complete and every other position is by
value. That is what lets one call give two opposite answers:
`preg_match($re, $s, $m)` keeps `$s` and drops `$m`.

Absence of a row is emphatically **not** a by-value statement. The row set is
deliberately restricted — the variadic-by-ref family (`sscanf`, `fscanf`,
`array_multisort`) is absent by design, and the table only ever aimed at the
names the effect layer colors, so `parse_str` and `exec` are absent too. A
rowless name must therefore be *positively certified*: every parameter by
value at `PINNED_PHP`. The certified set is closed and motivated rather than
open — it is exactly the names Steins' own rules already reason about:

* the folding allowlist (`foldable`), pure by construction;
* the ADR-0062/0064 array read-position and shape-projection family that
  carries no out-param row (`array_first`, `array_last`, `array_values`,
  `array_keys`, `array_flip`, `array_reverse`, `array_key_first`,
  `array_key_last`, and the two `array|object $array` pointer *readers*
  `current`/`key` — whose pointer-*moving* siblings `reset`/`end`/`next`/`prev`
  are `out_params` rows, so the two tables corroborate each other);
* the alias spellings of foldable names (`chop`, `join`, `sizeof`).

Widening that set is a separate act with its own measurement run. Every name
added to it is a new premise for every fact kept downstream of it.

### 2.2 The syntax layer records sites; it decides nothing

`Stmt::call_args` carries one `CallArgSite { var, callee, position }` per
occurrence of a variable as a plain positional argument of a statically named
call. The lowering knows no signatures and takes no decision — what it owns is
the **completeness invariant** the walk relies on:

> a variable appears in `call_args` only when EVERY occurrence of it in that
> statement's call arguments is describable as such a site.

One indescribable occurrence removes the name from the list entirely. So a
consumer that finds a name there knows it has seen all of that name's uses in
the statement, and `str_replace('a', 'b', $s, $s)` — by value at position 2,
the out-parameter at position 3 — cannot launder the write.
`Stmt::invalidated` is untouched, still complete, and still the answer whenever
`call_args` is silent.

### 2.3 The callee's own poison flag closes the non-argument route

A by-value parameter is not the only route into a caller's frame. A callee
doing `global $s` writes the *global* binding of that name, which at top-level
scope is a caller local; `extract`, `$$v` and `eval` are the same hazard by
other spellings. Argument passing never described that route, so no reasoning
about arguments can exclude it.

The veto is the callee's own `Scope::poisoned` flag — the ADR-0001 give-up
list, reused rather than restated. A project callee whose body carries any of
those constructs refuses. Builtins need no such gate: PHP's `global` is a
userland construct.

The caller's side of condition 4 is the same flag on the enclosing scope, and
it is why this design needs no reference-liveness analysis of its own: `$x = &$y`,
`global`, `static $x`, `$$v`, `extract`/`compact`, `eval`, `include` and a
by-ref `use (&$x)` capture all poison the whole scope already, so a live
reference into a local cannot coexist with a surviving fact.

## 3. v1 exclusions — kept on the blanket drop, deliberately

* **Method, nullsafe-method, static-method and constructor calls**, receiver
  and arguments alike. The receiver's own mutability is a separate question
  (ADR-0036/0043) and no `NameRef` names the target from the trace.
* **Dynamic callees** — `$f($a)`, `($o->cb)($a)`, first-class callables.
* **Named arguments and spread**. Positional mapping is defeated, so a
  position index would be a guess; the whole argument list is withheld rather
  than partially indexed.
* **Variadic parameter positions** of a project function, and any argument past
  the declared arity (`func_get_args` territory).
* **Terminator statements** (`return`, `throw`, `exit`). Their invalidation runs
  and the trace then stops, so precision there buys nothing; the blanket drop
  is retained rather than generalized for symmetry's sake.
* **`Opaque` constructs** (loop / switch / try bodies). Those forget their whole
  write ∪ read set for control-flow reasons this ADR does not address.

### 3.1 One thing this ADR deliberately does NOT claim to fix

`foreach ($a as &$v)` creates an alias that outlives the loop, and it is **not**
on the ADR-0001 give-up list — so a later mutation of `$a` does not invalidate
`$v`. That hole is real, it is orthogonal, and it is unchanged here: it exists
identically before and after, because it concerns a statement writing `$a`,
never a callee writing a by-value copy of `$v`. Recorded so nobody reads this
ADR as having audited it.

## 4. Replayability (ADR-0048)

The keep/drop verdict is a **pure function** of

```text
(the statement's recorded sites, the project index, the static catalog,
 the walk-local env/store at that point)
```

and of nothing else. It asks the engine nothing — no sidecar reflection, no
boot surface, no fold, no `function_exists` — so:

* there is no per-name engine state to memoize, and the issue #63 discipline
  (any per-name knowledge consulted must be memo-disciplined like the rest of
  `EngineFolder` state) applies **vacuously**, by construction rather than by
  care. The boot surface was considered as a countersignature for "is this a
  real builtin" and rejected for exactly this reason: it would have made a kept
  fact depend on whether PHP was running, and a replay run would then disagree
  with the run it replays;
* no global ordering can enter a kept fact — the verdict at one statement reads
  no other statement's verdict;
* `--no-php`, a live sidecar and a browser replay decide identically.

## 5. Why this is not "trust the catalog more"

The catalog's existing surfaces answer other questions. `effect_labels` says
what a builtin *does*, `declared_return` what it *returns*, `foldable` whether
it may be *executed*. None of them says whether an argument is by reference,
and each would be the wrong widening if borrowed for it: `trim` has no
out-param row and is fully described; `sscanf` has no row, no color, and its
arguments must stay unknown. `by_value_arg` is a new question with its own
membership discipline, so a name entering one table cannot silently change what
another table's consumers conclude — which #617 then demonstrated from the other
side: `sscanf` gained a seam-(ii) **return** rule without gaining a
`by_value_arg` row, a `resolve_arg_function` entry, or one bit of change in what
its by-reference tail's out-state says. A return rule is not an
argument-survival statement.

## 6. Consequences

* Three measured nsrt row families unblock (#76/#77/#74), and the whole
  read-position and shape-driven family stops losing its argument to its own
  first read.
* A kept fact is a **new premise**. The fp-gate is the standing instrument and
  movement in either direction is a triage event, not a win — a fact that
  survives can newly *prove* a finding as easily as it can newly silence one.
* The precise path costs one index resolution per described site, and only for
  names the walk actually holds something for; a statement with no describable
  site short-circuits before any lookup.

## Amendment A (2026-08-02): `array_slice` joins the certified set — PENDING ratification

§2.1 closed the certified set over "the names Steins' own rules already
reason about" and made widening it a separate act with its own measurement
run. This amendment is one such act, with one name.

**The membership case.** ADR-0062 Amendment B grew the shape-projection
seam to `array_slice`: the projection rung executes it on the
order-witnessed lane and answers the widening floor on the order-declared
lane. That made it a member of exactly the family §2.1 already lists
(`array_values`, `array_keys`, `array_flip`, `array_reverse`) while the
certified set still predated the growth — so a *nested* read like
`dumpType(array_slice($arr, 1, 2))` computed a precise answer and then
paid for it by dropping `$arr`: the site's callee is `array_slice`, not
the dump, so the dump-read exception never applies, and an uncertified
name refuses (`by_value_arg` answered `None`). Every row after a group's
first answered the bare envelope floor.

**The certification.** At `PINNED_PHP` (8.5.8) the stub declares
`array_slice(array $array, int $offset, ?int $length = null, bool
$preserve_keys = false)` — every parameter by value. Its splicing sibling
`array_splice(array &$array, …)` stays an `out_params` row, so the two
tables corroborate each other exactly as `current`/`reset` do, and the
near-name pair is pinned in the catalog's own tests.

**Measurement.** On the ADR-0073 base (inline `@var` seeding merged,
which is what makes the `array-slice.php` groups reachable at all), the
nsrt harness over phpstan-src's corpus moves 1602 → 1607 matches — +5
from this certification alone, with zero rows lost
(`array-slice.php` 34/38/46 — each group's second row — and
`bug-10721.php` 90/91); differ 11347 → 11342 and no other verdict class
moves. Three further rows (`array-slice.php` 42/50/54) improve from the
bare `array` floor to a structured answer without reaching match — their
remaining distance is `slice_widening` precision (`$preserve_keys`,
zero-length slices), not survival. Per §6, the kept fact is a new
premise and the fp-gate remains the standing instrument.

## Amendment (2026-08-09): a heap object handle passed by value survives (issue #295)

Condition 3 refused every object binding, and the code that implemented
it said why in one sentence: *class identity is not the reason the heap
lane refuses — a by-value call cannot change what class an object is —
the object's own mutable state is.* The refusal therefore threw away a
fact it had no quarrel with in order to discard one that a different
mechanism was already discarding.

**The mutable state is swept without this drop.** Handing an object to a
call runs the ADR-0036 escape-and-sweep in the same statement, *before*
step 4's invalidation: the object is marked escaped and its non-readonly
props are swept. What condition 3's drop removed on top of that was the
var→id link — the allocation identity, which carries the exact class, the
`readonly` bookkeeping, and (issue #295) the class-level generic carry.
None of those is reachable from a by-value callee.

**The one route that could rebind is already refused.** A `&$x` parameter
is condition 2's job and it condemns the name for the whole statement; a
callee body reaching a caller local sideways (`global`, `extract`, `$$v`,
`eval`) is condition 4's, applied to both sides. So a name every
occurrence of which is a proven by-value argument cannot be rebound, which
is exactly what a surviving handle claims.

**This was a stated deferral, not a new idea.** The regression that pinned
the drop (`by_value_pass_of_object_var_loses_class_fact_conservatively`)
recorded its own precondition verbatim: *"Recovering it requires proving
the resolved callee's parameter at that position is not by-ref (deferred —
not implemented now)."* `arg_is_by_value` is that proof, and it has been
in the tree since this ADR landed. The amendment collects a debt this ADR
itself made collectible; the test is renamed and inverted rather than
deleted, so the by-ref twin beside it still pins the refusing direction.

**What does not follow.** A bare guard-derived class bound (`Member` with
no heap object behind it) keeps refusing. The same argument would carry
it, but its consumer set is different and nothing in issue #295 needs it;
lifting it is a separate measurement.

**Why now.** Issue #295 carries class-level generic type arguments on the
allocation, so the conformance case's shape —
`$box = new MutableBox(1); takesIntBox($box); takesStringBox($box);` —
reads the carry one statement after the object was handed to a call. Under
the old condition 3 the second call saw no object at all, so the carry was
unreachable there for the same reason the exact class was: not because
anything doubted it, but because nobody had spent the deferral.

Per §6, the kept fact is a new premise and the fp-gate remains the
standing instrument.

**Status: PENDING ratification.** Designed autonomously under the owner's
standing delegation, alongside the ADR-0032 binding amendment it unblocks.

## Amendment (2026-09-01): an offset-spelled argument records the chain's root (issue #609)

The occurrence this ADR recognized was a bare `$v` argument. An argument
spelled `$a[0]` recorded nothing — not a site, not an opaque entry — so
`$a` never entered `Stmt::invalidated` at all, and the walk kept the
array's pre-call shape past `sort($a[0])` and `settype($b[0], 'int')`.
That is not the blanket drop being coarse; it is the invalidation channel
being silent about a write PHP performs. The element's stated value was a
false fact on every reachable path after the call — a false-positive
source wherever a later check premised it, and the one soundness class
this ADR's whole design exists to never trade for precision.

**The rule.** A pure offset chain rooted at a direct variable (`$a[0]`,
`$a['k'][1]`, …) records the ROOT under exactly the site/opaque rules a
bare argument gets: a statically named callee at an all-positional call
site yields a `(callee, position)` site, anything else marks the entry
opaque. The gate then works unchanged — a certified by-value position
(`count($a[0])`) copies the element out and the root survives with its
whole shape, a by-ref or unresolvable one condemns the binding. The
chain's keys are reads and record nothing (`sort($s[$i])` drops `$s`,
never `$i`). `preg_match('/…/', $s, $m[0])` falls out of the same rule:
`$m` drops at the out-parameter position while `$s` survives at its
by-value one.

**What does not follow.** A chain that passes through anything but
offsets on its way to a variable (`$o->p[0]`, `$s[0]->p`, `f()[0]`)
still records nothing here: what such a callee can reach is shared
object state, which the ADR-0036 escape-and-sweep already covers — the
local binding this channel forgets was never the carrier of that fact.
Slot-precise forgetting — dropping only the written key and keeping the
siblings — is deliberately not attempted; the whole-binding drop is the
conservative floor, and sharpening it is a separate measured act.

**The stored-trace consequence.** `Stmt::invalidated` is persisted trace
IR, so a schema-5 artifact of `sort($a[0])` carries no entry for `$a` and
replaying it would keep the stale shape this binary now forgets.
`steins_gen::SCHEMA_VERSION` is 6 (the issue #414/#571 precedent): the
old artifact is a miss and one rebuild.

**Status: PENDING ratification.** Designed autonomously under the owner's
standing delegation; noticed while landing issue #595, which refuses to
write a cast through an offset but did not change what the offset call
forgets.

## Amendment (2026-09-11): the certified set is mined, and a certified guard keeps its subject (issue #637)

§2.1 closed the certified set over "the names Steins' own rules already
reason about" and made each widening a separate measured act. Seven such acts
later (#41, #40, #536, #559, #569, #575, #597, plus Amendment A's one name) the
list had reached a hundred and thirty names and the *rule* behind them had never
changed: every parameter declared by value at `PINNED_PHP`. This amendment
replaces the transcription with the table, and lifts the same argument into
guard position.

### 1. The membership rule is read off arginfo, not off a list

A rowless name is certified when the **mined per-parameter table**
(`param_facts_generated.rs`, issue #382) says every parameter of it is by
value. That table is `ReflectionFunction::getParameters()`'s
`isPassedByReference()` over every internal function of the mining build,
generated by `cargo xtask mine-param-facts` + `cargo xtask gen-catalog`,
committed, and pinned to `PINNED_PHP` — the posture ADR-0094 §2 states for
engine tables and ADR-0069 §4 states for their lineage. **No second table was
mined.** php-src's stubs and the engine's arginfo are one source read twice, and
`param_facts`' own doc names the failure a second transcription would be: it
agrees with the first wherever the first is wrong.

The hand lists stay where they are. Each records *why* a name mattered — which
measurement it unblocked, which sibling is rowed instead — and the table cannot
say that. What the table changes is the answer for the ~2,000 internal names
nobody had reached: at the mining pin, 130 internal functions declare at least
one reference parameter and every other one declares none.

Three exclusions keep the mined arm narrower than "no `&`":

* a name with **any** by-ref parameter certifies nothing new — it keeps
  answering from its `out_params` row, or keeps answering `None`. This arm
  invents no row, which is what leaves `sscanf`, `fscanf`, `array_multisort`,
  `extract`, `exec` and `parse_str` exactly where §2.1 put them;
* a **callback carrier** is refused (a declared `callable` parameter, an
  `invocation_shape` row, an array of callables, or an unargued `mixed`
  variadic tail). §2.3 closes the sideways route into a caller's frame with the
  *callee's* poison flag; a builtin that invokes a userland callback re-opens
  it, and arginfo says nothing about that body;
* a name the mining build did not have answers `None`. The forgetting floor is
  unchanged for every userland, vendor, dynamic and shadowed name — #279's
  refusal of a shadowed spelling now covers ~2,000 more spellings, which is the
  direction that costs silence.

### 2. Guard position takes the same argument

The catalog change alone moves nothing on the issue's own witness. A call in a
**condition** never reached the survival gate: `collect_call_opaque_reads`
forgets a guard call's whole read set unless the callee is one of five
recognized families, and `pure_question_builtin` — the fourth of them — is a
hand list making exactly this amendment's argument name by name
(`class_exists`, `ctype_*`, `strlen`). So `if (rmdir($s))` cost `$s` its fact on
both arms and at the join after them while `if (class_exists($s))` did not,
though the two names differ in nothing that matters here.

A sixth exemption reads the same mined certification, under two conditions
about the **call** rather than the name:

1. every position the call supplies is certified `Some(true)` — one by-reference
   position condemns the guard exactly as §2.2 has it condemn a statement;
2. every argument is a shape that cannot itself write — a literal, a bare
   variable, an offset read of one, or a literal array of those. The read set a
   guard forgets is the whole *condition's*, so `f(g($s))` would otherwise keep
   `$s` on `f`'s promise while `g` is what touched it.

What the exemption does **not** claim is what the guard proves.
`strstr($s, 'a')` keeping `$s` is not `strstr($s, 'a') !== false` narrowing it;
that is the type-specifier half (#575/#266). The join rows move on this
amendment and the guard-body rows wait for the specifier, which is why the
measurement below reports the split.

### 3. What this costs, and where it is measured

A kept fact is a new premise (§6), and this amendment adds ~2,000 names' worth
of them at once. Two surfaces feel it beyond the trace: `variable.undefined`
subtracts the arguments a call might be *writing*
(`out_param_argument_spans`), so a certified callee makes an undefined argument
reportable where an uncertified one silenced it; and every consumer of a
surviving fact may now prove a finding it could not reach before. The fp-gate
over the corpus is the standing instrument and movement in either direction is a
triage event, not a win.

### 4. Measurement

Recorded with the implementation (issue #637), nsrt over phpstan-src at
`ed7ca75` against the same tree with this amendment, 16,700 measured rows:

```text
verdict        before    after
match           3,412    3,497   (+85)
equal             206      207   (+1)
subsumed          477      479   (+2)
unsupported     2,017    2,017
differ         10,588   10,500   (−88)
```

Eighty-eight rows move and every one of them moves **forward**: no row leaves
`match`, and no verdict class regresses. Two of the issue's three named join
witnesses are among them (`non-empty-string-str-containing-fns.php:112`,
`non-empty-string-strstr-specifying.php:100`), each `unknown` → `string`.

The residue is attributed rather than counted:

* **77 rows are the specifier half** — guard-body assertions asking
  `non-empty-string` / `non-falsy-string` where the join beside them now
  answers. Predicted, and #575/#266's to move.
* **29 rows sit downstream of one name the mining build did not have.**
  `non-empty-string-file-functions.php` walks forty guards over one parameter;
  `chroot` is the third, it does not exist on the mining platform at all
  (`function_exists('chroot')` is `false` there), so that one guard forgets `$s`
  and every assertion after it in the function reads `unknown`. The table
  answering `None` for a name its build lacks is the rule, not a defect —
  ADR-0094 §2's extension set — and the fix, if the rows are wanted, is where
  the table is mined, not a hand entry beside it. The third named witness
  (`non-empty-string-file-functions.php:175`) is in this group.
* **5 rows are a comparison shape the operand path does not record sites for**
  (`strchr(...) == ''`, and a comparison in an assignment's right-hand side).
  Not this amendment's question.

The fp-gate's public half stays green over all ten pinned packages, with no
ledger movement in either direction.

**Status: PENDING ratification.** Designed autonomously under the owner's
standing delegation.
