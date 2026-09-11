# Discardable effects: a dead statement read off the label lattice, not off a curated boolean

**Status: proposed (2026-09-12), PENDING ratification.** Drafted under the
owner's standing delegation alongside the slice that implements it (issue
#320). The load-bearing choices — the membership of the discardable set, the
layer, and the floor — are recorded here so a later reading knows which lines
are policy and which are derivation.

## 1. Context: PHPStan spends a boolean where Steins has a lattice

PHPStan's `CallToFunctionStatementWithoutSideEffectsRule` and its siblings
report a statement-position call whose result is unused, gated on a curated
`hasSideEffects` bit carried per function in the reflection layer. The bit is
maintained by hand, it is one bit wide, and it cannot express the two things
the corpus actually contains:

- **A call that is impure and still dead.** `rand();` has an effect — it
  advances the RNG state — so the boolean says "side effects" and the rule
  stays silent, though discarding the result is exactly the mistake the rule
  exists to catch.
- **A call whose deadness depends on its arguments.** `file_get_contents('/c');`
  reads a file; `file_get_contents($url, false, $ctx);` may reach the network.
  One name, one bit, two programs
  ([phpstan#8440](https://github.com/phpstan/phpstan/issues/8440)).

Steins already proves something finer than the bit: the ADR-0018 effect labels,
with ADR-0021's argument-blind upper bound narrowed at a call site that proves
its arguments (issue #318, and #352 for the output family). The rule therefore
does not need a boolean at all. It needs a **set membership** — read, as §3
says, at a call whose arguments are the literals the row was calibrated on.
The `file_get_contents` split above is the shape the effects pass narrows
today; this family does not read that narrowing yet, and the name's arg-blind
`io` row and its throw row keep both spellings silent in this slice.

## 2. Decision: the discardable set, and it is policy

A proven effect label is **discardable** when one of these four roots subsumes
it:

```
Discardable = { global.read, nondet.random, nondet.time, io.fs.read }
```

This is a judgment call, not a derivation, and each member and each exclusion
is one:

- `global.read`, `nondet.random`, `nondet.time` — the call reads ambient state
  and hands it back. Throwing the answer away leaves the world as it was. This
  is where `rand();` lands, the case the boolean structurally could not reach.
- `io.fs.read` — **declines to count atime.** A filesystem read does update the
  access timestamp, and a program written to update it is a program nobody has
  shipped. The label is reached only through a call site that proved a
  filesystem target, so an unprovable target stays `io` and stays silent.
- `io.input` is **excluded** although it too is a read: consuming a stream
  advances its position, so `fgets($h);` is how a program skips a line.
- `mutate.local` is **excluded**: `sort($rows);` is *called* for the
  caller-visible mutation, and its return value is the part nobody wants.
- Everything else is excluded by not being listed. A label the catalog grows
  later is silent until this set is amended, which is the right default for a
  proof-layer id.

ADR-0084's `tolerated` policy does **not** feed this set. Tolerance discharges
an effect at *envelope judgment* time — "this impurity is understood and
accepted" — and says nothing about whether the call is dead. A tolerated
logging call still writes to the log.

## 3. The predicate: the name, the arguments, and what the seam cannot see

A statement-position call (`StmtKind::Call`, so the result is discarded by
construction) is reported when **all** hold. Each is a proof, and each missing
one is silence — that is what puts the id on the proof layer.

**The name.** The callee resolves to one catalogued builtin (a namespaced
shadow or an ambiguous name is the `…?` of the function world), its effect row
lies wholly inside §2's set, it has no by-ref out-parameter row (`shuffle`'s
colour is discardable and the write through `$rows` is why it was called), and
it has no throw row. The throw row is PHPStan's exemption made precise: a bare
`random_int(1, 10);` can be the validity check whose *result* is the throw. The
row is read **per name**: `str_repeat` carries a `ValueError` arm for a negative
`$times`, so `str_repeat('a', 3);` is silent although that call cannot throw.
The review of 2026-09-12 found eight fold-allowlist names with unmined
`ValueError` arms (`str_repeat`, `array_fill`, `range`, `str_pad`, `str_split`,
`explode`, `str_increment`, `round`) and every one reported; those arms and
their siblings (`strpos` family, `sprintf`, `version_compare`, `count`,
`mt_rand`, `str_decrement`) are mined now, each reproduced by probe on PHP
8.5.10 and recorded in `throws.toml`, so the row answers rather than a hand
list in the rule.

**The arguments.** For most names the colour row is the fold allowlist's:
`strlen` is catalogued pure *because it folds*, and a name folds because it is
pure **given literal arguments**. That calibration is the whole of the
evidence, and the same review showed what reading it argument-blind costs —
`array_filter($paths, 'unlink');` reported while the callback unlinked,
`strlen($o);` reported while `__toString` ran. So the call is held to the
calibration: every argument is a literal the fold gate would admit — a scalar,
or an array literal of scalars, never a variable, a call, a constant fetch, a
concatenation or an object — spelled positionally with no name and no
unproven spread; the count lies inside the arity the engine's own arginfo
reports (`param_facts`), and inside the rule's own set where the engine's
interval is wider than the call's (`rand()` takes exactly zero or exactly two
arguments, `rand(1);` is an `ArgumentCountError` raised from inside the call,
and reflection can only spell that as "0 required of 2"; the arity checker is
silent on it too, so the co-fire rule below does not catch it); no position
passed is one the engine declares `callable`; and each literal is **exactly**
of a type its parameter declares, under strict-mode acceptance with the one
widening strict mode itself performs (`int` into `float`), `null` only into a
nullable slot. Exactness rules out both halves of a coercion at once — the
`TypeError` `strict_types=1` raises and the deprecation the coercive mode
raises instead — which is what makes `strlen(null);` and
`array_merge([], 'string');` silent in either mode. A nested array literal is
refused whole, because the string family renders an element and warns on an
array, and an array literal is admitted only at a slot declared `array` or
`iterable`, never at `mixed`: a `mixed` slot is one the engine does not check,
so what the builtin does with an array there is its own business, and
`strval([1]);` is an "Array to string conversion" warning. The rule is uniform
rather than a row per name, so `intval([1]);`, `boolval([1]);` and
`gettype([1]);` — true positives the first draft reported — are silent with
it. An object cannot be spelled as a literal, so `__toString`, `Countable`,
`JsonSerializable` and `ArrayAccess` are out by construction, and a string
literal at a `callable` position names user code.

**No proven throw on the same call.** A checker that ran on this statement and
proved it throws — the argument checker's `TypeError`, an arity error, a printf
format demanding arguments it was not given, a pattern the project's PCRE
refuses, an undefined callee — has said what the statement is for. The two
ids never co-fire; the proof wins.

**What the seam cannot see, and the refusals that stand in for it.** The bar
above was designed around the fold seam: fold the literal call, and a value
with no engine diagnostic is the proof, a throw or a warning the refusal. The
runner cannot supply the second half. It routes every warning, notice and
deprecation to a stream the parent discards (`log_errors` to `php://stderr`,
the same routing that keeps a notice out of the NDJSON channel), and answers a
value as if nothing had been said, so it cannot tell `strlen(null)`'s value
from the same value with the deprecation the phpunit fixture exists to raise.
Teaching it to would change the wire form, the persisted fold cache and the
wasm engine's twin, which is not this slice. Two consequences follow. First,
the fold seam is **not consulted** by this family at all — a verdict that
changed with `--no-php` would be a different id — and the argument bar is
what stands where the fold's answer would have. Second, a small refusal list
(`REFUSED_ON_LITERALS`) names the catalogued-pure names whose exactly-typed
literal call can still raise a diagnostic or write an engine slot: `json_encode`
(the `json_last_error` slot, and a `JsonException` under a literal flag bit the
catalog carries only as a synthetic key), `preg_split` (a compile warning, the
`preg_last_error` slot), `bindec`/`hexdec` (a deprecation on a stray
character), the `trim` family (a warning on a decreasing mask range), `idate`
(a warning on an unknown token), and three names whose `TypeError` the
declared parameter types cannot show: `strtr` (the two-argument string shape),
`implode`/`join` (the legacy `array|string $separator, ?array $array`
signature couples the shapes it accepts across the two positions, so
`implode('a');`, `implode('x', null);` and `implode([1], [2]);` are each
admitted by type and each a throw) and `substr_replace` (an array `$offset` or
`$length` on a string subject). A refusal is per name, so the well-formed
`implode(',', [1, 2]);` is silent with them. Each row carries its witness, and
a test holds every row to a name the rest of the predicate would otherwise
judge. An engine warning or deprecation is therefore a **refusal** under this
bar, never a "non-effect": a diagnostic reaches `set_error_handler`, and a
call that can raise one may run user code.

**The callee returns something.** A `: void` or `: never` callee hands back no
result, so "the result is unused" states nothing. No catalogued name reaches
the end of the predicate without one: a name is either catalogued pure — which
for a `void` builtin would be a no-op nobody shipped — or carries a §2 read
colour, and every such row returns what it read.

## 4. Layer and floor

`Layer::Proof`, `Floor::Default`.

The issue that raised this family proposed the mechanics layer on the reading
that dead code is anti-rot rather than a claim about correctness. That reading
is declined. The premise here is the same proven lane
`effect.envelope-exceeded` is made of — not a style preference and not a
declaration — and the verdict is definite: this statement does nothing. A
mechanics id would also be always-on regardless of profile (ADR-0050), which is
a stronger commitment than the proof layer's, not a weaker one.

`Floor::Default` follows from the layer rather than from a count. The
calibrated-defaults posture (the zero-FP bar is a calibrated floor, not an
omitted check) is served here by the *predicate's* strictness: four conjuncts,
each of which must be established, is the calibration. Raising the floor
instead would hide a definite proof behind an opt-in rung and answer a question
about frequency that the predicate has already answered about certainty.

## 5. Scope: where the judgment is made, and what the first slice answers

The judgment is made **inside the analysis walk**, at the statement, and not in
a reporting pass of its own. That is forced by ADR-0092: a reporting pass would
have to scan every file's trace for statement-position calls, and a warm run
that changed nothing is required to decode no tree at all. Riding the walk puts
the finding in the per-file block a warm run replays, at no tree cost.

The first slice answers **catalogued builtins**: the rows it reads
(`effect_labels`, `out_params`, `builtin_throws`, `param_facts`) are a property
of the binary and already covered by the artifact's analyzer-version field.
`strlen('x');`, `time();` and `rand();` report; a project callee is silent.

**The project half is not blocked by the affected set.** An earlier draft of
this section argued that a project callee's summary — a whole-project fixpoint
result that can move without the callee's own file moving — has only the
whole-universe verdict to be priced under, and that pricing it there
invalidates every replayed block on any function added or removed anywhere.
That was misattributed. `affected.rs` leg 2 already intersects each file's
footprint with the name delta, and a file's footprint carries an `f:`/`s:` key
per named call it makes; a persisted per-symbol does-nothing list,
symmetric-differenced against the previous generation's, is a delta in exactly
that key space, and a caller whose footprint names a symbol that entered or
left the list is affected by the existing leg with nothing new built. The real
cost is elsewhere: the effect and throw fixpoints are today forced only under
`Gate::Envelope` / `Gate::Throws` — a project that declares no envelope and no
`@throws` never runs them — and the project half would force both on every
run to have a summary to read. That is the trade a follow-up has to price, and
it is a cost question, not a soundness one.

- **`io.fs.read` is policy-reserved and inert in this slice.** The label is
  minted only by `narrowed_stream_labels`, which the effects pass calls at a
  call site that proves its target and this judgment does not call at all. It
  stays in §2 so the policy is stated whole; no statement reaches the set
  through it today.
- **Never methods.** A statement-position *method* call needs the effect lane's
  own receiver resolution, and the receiver forms that lane resolves
  (`$this->`, `self::`, `parent::`, `Foo::`) are not where a discarded call
  gets written. The everyday `$config->getTimeout();` dispatches on a variable,
  which draws no edge in the effect graph at all, so the method half buys
  little and is deferred rather than guessed at.
- **A `match` arm body is not a statement.** `lower_expr_position` lowers an
  arm's body to a `StmtKind::Call` so the walk reaches it, and `echo match (…)`
  consumes that call's result — so `Stmt` carries a `value_position` bit and
  this family is the only reader of it. Without it the family would make a
  false claim about every value-position `match` arm that calls a pure builtin.
- **A `switch` arm reports only when the lowering can structure it; a `try`
  body is opaque and never does.** `lower_switch` lowers an arm to a statement
  list when the arm ends in a plain `break;` or terminates (`return`, `throw`,
  `exit`); a trailing arm without a `break`, a `default:` without one, and a
  braced `case 1: { … break; }` leave the whole `switch` opaque, so a
  discarded call inside any of those is silence — a missed true positive, not
  a wrong claim, and pinned so the next change to the lowering moves it
  deliberately. A `try` body carries no statements in the lowered trace
  (`StmtKind::Opaque`), so a discarded call inside one is silence for the same
  reason — a property of the lowering, not a rule of this ADR.
- **`@` is silence by the lowering.** An error-suppressed call lowers to no
  `StmtKind::Call`; the operator is a request about diagnostics, which is the
  thing this family cannot see.
- **PHP 8.5's `#[\NoDiscard]` is the other quadrant** — an *effectful* call
  whose result is the point — and needs no effect machinery at all. It is a
  separate, smaller rule for whenever 8.5 support lands.

## 6. Consequences

- A new proof-layer id fires on ordinary code, which is a **breaking** change
  for a green CI. The remedy is the ordinary one: delete the statement, or
  suppress the id.
- **It costs nothing to run.** Riding the walk, the catalogued half reads four
  catalog rows per statement-position call and forces neither fixpoint.
- **The mined `ValueError` rows reach the throws lane too.** They are `Error`
  subclasses, unchecked under ADR-0007, so no `throw.*` finding moves; what
  moves is that `count`, `sprintf` and their siblings now carry the arm they
  have always had. The seventeen rows do move the `throws-envelope`
  transform's denominator on the public corpus: enumerated 3346 → 5058 and
  refused 861 → 2573 (`escape-not-proven (ArgumentCountError|ValueError might
  escape)`), with edits 2485 → 2485 unchanged; `loop-to-array-map` 0 → 0 and
  `check` findings unmoved.
- **`Stmt` grows a field**, so the cache schema moves. A `value_position` bit
  appended to a positionally-decoded struct is a misdecode for an older
  artifact, which the bump refuses to read.
- **Two shapes the first draft ledgered as true positives are silent now.**
  Composer's `array_merge([], 'string');` under `strict_types=1` is a provable
  `TypeError` and the test's subject; phpunit's `strlen(null);` exists to raise
  the deprecation its recorder observes. Under the argument bar neither is a
  no-op line, and the public fp-gate produces no `statement.no-effect` row.
- The set in §2 and the refusal list in §3 are amendment surfaces. Growing the
  set is a policy change and wants a measured corpus pass behind it, exactly as
  ADR-0084's `tolerated` does; shrinking the refusal list wants the runner to
  report diagnostics first.
