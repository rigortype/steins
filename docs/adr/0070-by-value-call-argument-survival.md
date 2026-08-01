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
out-param row and is fully described; `sscanf` has no row, no color, and must
stay unknown. `by_value_arg` is a new question with its own membership
discipline, so a name entering one table cannot silently change what another
table's consumers conclude.

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

**Measurement.** MEASUREMENT-PENDING — nsrt before/after and fp-gate to
be recorded here from the run accompanying this amendment. Per §6, the
kept fact is a new premise and the gate remains the standing instrument.
