# Pure-function typing: semantic effect propagation first, conditional-purity contracts second

Owner directive (2026-07-29): organize and advance pure-function typing.
Status: PENDING ratification (autonomous design under the owner's
post-hoc-ratification mode). Context sources: the effect system (ADR-0018
labels, ADR-0055 class-level purity and the `Impure = ⊤` meet rule), and the
maintainer-side study note
[20260703-effect-system-design.md](https://github.com/zonuexe/phpstan-notes/blob/master/generated-report/20260703-effect-system-design.md),
whose upstream conclusion governs the import: metadata-only purity flags are
a *lie* (rejected twice in phpstan-src, #5580/#5912); the endorsed direction
is **declarative conditional purity** (`@pure-unless-callable-is-impure`,
phpdoc-parser #253 syntax MERGED; sister `@pure-unless-parameter-passed`,
phpdoc-parser #259 open).

## 1. The two problems, in Steins terms

1. **Higher-order purity is polymorphic**: `array_map`'s effect is its
   callback's effect. PHPStan needs an annotation because its analysis is
   modular; Steins descends into visible callbacks (closure wave,
   invocation-shape callbacks) by call-site value propagation — the
   polymorphism is *observable*, not declarable.
2. **By-ref out-params poison purity**: `preg_match($p, $s, $matches)` into
   a local is pure in every useful sense; a flat impure-call verdict is the
   crying-wolf pattern 狼少年撲滅 exists to kill.

## 2. Decisions

1. **Semantic first.** The effect of a higher-order builtin call is
   `builtin's own color ⊔ (join of its immediately-invoked callback
   arguments' envelopes)`. A **callback-position catalog** (which argument
   positions of which builtins are immediately invoked — array_map/filter/
   reduce/walk, usort family, preg_replace_callback, …) drives the join
   through the existing via-provenance fixpoint. No annotation is consulted
   when the callback body is visible. This is the differentiator: where
   PHPStan's endorsed fix is a contract, Steins' first answer is inference.
2. **Contracts for opaque callables.** Where the callback is an opaque
   `callable` parameter, the *declared* conditional purity carries:
   recognize `@pure-unless-callable-is-impure` (upstream-merged syntax) and
   lower it to "this function's envelope = join of the flagged callable
   params' envelopes". Recognize the by-ref sister the same way when
   spelled. BC-free proving ground (ADR-0016): we honor the spelling before
   PHPStan ships its own consumer.
3. **`mutate.local` color.** New effect label for by-ref writes that land
   in caller-local bindings (`preg_match` `$matches`, `str_replace` count).
   Colored-builtin rows for out-param builtins become conditional: the
   label attaches only when the by-ref argument is actually passed, and it
   is **tolerated by Pure envelopes** (a pure function may scribble on its
   own locals; the same call writing a property/static is the existing
   `mutate.instance`/`global` world and stays forbidden). This encodes
   upstream #11884's wish — "conditional on the argument, not a per-function
   lie" — as a color, not a flag.
4. **`pure-callable` / `pure-closure` are enforceable spellings.** Lower to
   `CallableTy` + a pure-envelope obligation on the bound argument: a
   closure argument judged against `pure-callable` must have inferred
   envelope ⊑ Pure (with the `mutate.local` tolerance above; `static
   closure`'s binding constraint is a separate mechanical check). This
   closes the conformance rows (`fallback_pure_callable`, `pure_closure`,
   `static_closure`, `static_pure_closure`) with real enforcement, not
   curation.

## 3. Declined imports

- `hasSideEffects=false` metadata blanket for higher-order builtins — the
  rejected-upstream lie; our catalog rows stay conditional.
- PHPStan's dead `ImpurePointIdentifier` color taxonomy as-is — Steins'
  hierarchical dot-path labels (ADR-0018) already subsume it; no second
  vocabulary.

## 4. Slices

| Slice | Content | Instrument |
| --- | --- | --- |
| P1 | Callback-position catalog + higher-order effect join (semantic leg) | effect fixtures; corpus effect counts |
| P2 | `mutate.local` label + conditional out-param rows + Pure tolerance | preg_match/str_replace fixtures |
| P3 | `pure-callable`/`pure-closure`/`static closure` enforcement | conformance T-rows flip to enforced |
| P4 | `@pure-unless-callable-is-impure` (+ by-ref sister) lowering | nsrt; conformance |

P3 overlaps the conformance C-phase (it *is* four of its rows); sequence P3
inside whichever phase opens first.

## Amendment (2026-07-30): P2 + P4 outcomes

**The sister tag is merged, not open.** §1 cites phpdoc-parser #259 as still
open. The copy vendored at `harness/phpdoc-oracle` is 2.3.3 (2026-07-08) and
ships `PureUnlessParameterIsPassedTagValueNode` alongside its callable sibling,
registered under `@pure-unless-parameter-passed` and
`@phpstan-pure-unless-parameter-passed`. Both tags are therefore implemented
from the merged grammar (`parseRequiredVariableName` + optional description, no
type, no `@psalm-` alias) — no ADR-0016 lead was needed, and no spelling was
guessed.

**`preg_replace`'s count is argument 4.** The P2 brief listed it at 3 alongside
`str_replace`. The optional `$limit` sits between subject and count, so
`str_replace`/`str_ireplace` are 3 and `preg_replace`/`preg_replace_callback`
are 4. Rows are transcribed from the stubs, not from the brief.

**Conditionality needed a second catalog axis, not a wider label set.**
`effect_labels` answers per *function*; an out-parameter write is a property of
the *call*. `out_params(name) -> Option<&[usize]>` is the new row, resolved at
each call site against two legs: arity (`arg_count > position`) and target (the
argument's lvalue root, classified in `steins-syntax` as `RefTarget`). The
variadic-by-ref family (`sscanf`, `array_multisort`) is deliberately absent —
its positions are open-ended, and an under-approximated target leg would
downgrade an escaping write to `mutate.local`.

**Target distinction: Steins has it.** Property, static-property, by-ref
parameter, superglobal, and aliased-frame targets are each recognized and never
claim `mutate.local`. What Steins declines is naming *which* escape it is: those
all land on the conservative parent `mutate`, because ADR-0055's
`mutate.self`/`.instance`/`.static` inference (slice E2) does not exist. The
frame-locality claim is additionally gated on the frame carrying no ADR-0001
give-up-list construct (`global`, `static`, `$$v`, `extract`, `$a = &$b`,
`use (&$x)`) — a coarse gate, but each member genuinely defeats "this name is a
frame-private binding".

**The tolerance is universal, not `Pure`-only.** §2.3 states it for `Pure`.
Implemented for every envelope: `Pure` is the tightest one, so tolerating a
label there while rejecting it under a wider declaration would be non-monotone.

**P1's own-color leg is now live.** It was inert because the catalog had no
color for `usort`/`array_walk` to contribute. Their by-ref row supplies one, so
`usort($localRows, $pureCmp)` under `Pure` is clean (tolerated) while
`usort($this->rows, …)` is not.

**P4 lowers to a taint discharge, not a purity override.** A tagged callee's
proven findings still propagate (ADR-0037). What the declaration buys is the
*unknown*: a tagged function's body calls its callable parameter dynamically and
is therefore permanently non-exhaustive, and when every flagged condition is
decided at a call site the contract discharges that taint there. An opaque
callable in the flagged slot decides nothing, so the taint stands.

**Declined this slice.** The tag is honored on free functions only —
`EffectOrigin::MethodCall` records neither arguments nor callbacks, so a method
carrying the tag falls back to the plain edge. Widening the effects pass's
notion of a catalogued builtin (so `preg_match`/`sort` resolve as builtins at
all) was scoped to that pass: the same widening would change how the *throws*
pass classifies those names, which is a real gap and a different baseline.

## Amendment (2026-09-11): the exposure leg, and a second consumer of the same classification — PENDING ratification

Issue #641. The 2026-07-30 amendment above recorded that conditionality "needed
a second catalog axis, not a wider label set", and resolved `out_params(name)`
per call site against two legs: **arity** and **target**, the target leg being
`RefTarget` with the frame-locality claim "additionally gated on the frame
carrying no ADR-0001 give-up-list construct". This amendment carries the same
pair from *colouring a write* to **bounding a write's observable extent**, and
records that the value lane now reads the classification the effects pass ships.

Nothing in the effect vocabulary changes. What changes is who consults it.

### The two legs, restated as extent

- **The target leg** — what the write names. `RefTarget` unchanged: a
  superglobal root is interpreter-global surface, a by-ref parameter's cell
  belongs to the caller (and two by-ref parameters can be one cell, `f($x, $x)`
  into `function f(&$p, &$q)`), and only a plain local continues.
- **The exposure leg** — which of the frame's *other* bindings could observe the
  write. Today that is the single whole-frame bit `frame_aliased`, and it stays
  whole-frame: "which names survive an aliased frame" is a dataflow question the
  structural scan does not ask, exactly as the earlier amendment says.

The consumer is `apply_offset_write`/`apply_offset_append`, which used to clear
the whole environment and store on every `$a[$k] = v`, `$a[] = v` and
`unset($a['k'])` and put back the one base it wrote through. Both legs proving
`Local` now narrows that clear to the target alone; either leg declining leaves
the total clear that stood before, so the barrier remains the floor everywhere.

The soundness argument is PHP's, probed at `PINNED_PHP` 8.5.10 (ADR-0061 §4): an
offset write is observable through exactly one channel, a reference.

```
$ php -r '$a = [1,2,3]; $b = $a; $a[0] = 9; var_dump($b[0]);'
int(1)                                          # a copy is not aliased
$ php -r '$s = "foo"; $t = $s; $s[0] = "X"; var_dump($t);'
string(3) "foo"                                 # nor is a string copy
$ php -r '$a = 1; $b = ["key" => &$a]; $b["key"] = 42; var_dump($a);'
int(42)                                         # a reference is
```

### The hole the exposure leg had, and its closure

`frame_aliased` is documented as "exactly the ADR-0001 give-up list", and it was
not. `scan_opaque_walk` recognised a reference only as
`Node::Assignment(a) if a.rhs.is_reference()`, and `Expression::is_reference()`
is a top-level test, so two spellings produced no `OpaqueSite` at all:

```
$ php -r '$a = 1; $c = "test"; $b = [&$a, "normal", &$c]; $b[0] = 2; $b[2] = "bar"; var_dump($a, $c);'
int(2)   string(3) "bar"                        # `['k' => &$a]` has an Array rhs
$ php -r '$r = [[1],[2]]; foreach ($r as &$v) {} $v[] = 9; var_dump($r[1]);'
array(2) { [0]=> int(2) [1]=> int(9) }          # and the alias outlives the loop
```

Both are now `OpaqueConstruct::ReferenceBinding`, recorded by one arm — since
PHP 8 removed call-time pass-by-reference, an array-literal element and a
`foreach` value binding are the only two places a `&` stands in expression
position that the closure arm does not already take.

This is a correction to `mutate.local` as much as to the value lane:
`function f(&$p) { foreach ($p as &$r) { sort($r); } }` classified `$r` as
`RefTarget::Local` and coloured the `sort` `mutate.local`, while the write lands
in the caller's array. It costs precision — a frame with a by-ref `foreach` is
now poisoned outright, and by-ref `foreach` is common — and it wins nothing on
its own. It lands first because the extent rule above is unsound without it.

`SCHEMA_VERSION` moves 17 → 18 for it: `Scope::opaque` is persisted, so a
schema-17 artifact records as unaliased a frame this binary records as aliased,
and replaying one would narrow a barrier that must stay total. That is a miss
that changes *meaning*, which ADR-0092 §2 forbids.

### What the extent rule still refuses

- **`store.refs`, `heap`, `members` and `narrowed` are cleared either way.**
  `refs` is the alias map the test is reasoning about; `heap` and `members`
  reach objects whose identity the value lane does not track, and `$b['k'] = $v`
  on an `ArrayAccess` receiver runs `offsetSet`, which nothing here bounds.
  Retaining them is a second soundness claim needing its own reachability
  argument, and it is not made here.
- **A plain `StmtKind::Barrier` and `StmtKind::Destructure` still clear
  totally.** `Barrier` is a unit variant: the lowering throws the lvalue away on
  the way in, so there is no target to classify — and it is also the arm for
  constructs the lowering could not classify at all, which must stay total
  whatever else happens. `Destructure` names an unbounded target set the
  lowering deliberately does not model (issue #288).
- **The gate stays whole-frame**, per the earlier amendment, and per-variable
  exposure is not attempted.

## Amendment (2026-09-28): the top-level scope is a frame whose locals are globals — PENDING ratification

Issue #762, under the owner's ruling on it (option 2), and the first point of
issue #696. The two legs above, and the value lane's statement invalidation
before them, read every scope as frame-private: a callee reaches a caller's
binding only through an argument, a receiver or a reference, so a statement
that names nothing loses nothing. That holds inside a function body and not at
file scope, where every local is a global and any userland body that runs can
rebind one through `global $s` or `$GLOBALS['s']`. Probed at `PINNED_PHP`
8.5.10, both of these exit 0 and Steins convicted the stale `"abc"` on the
default surface:

```
function bump(): void { global $s; $s = 5; }
$s = 'abc'; bump(); intdiv($s, 1);
```

```
final class AA implements ArrayAccess { /* offsetSet: $GLOBALS['s'] = 5; */ }
$a = new AA(); $s = 'abc'; $a['k'] = 1; intdiv($s, 1);
```

### The rule

**In the top-level frame, a statement that runs a userland body forgets every
name the frame holds.** The answer is one predicate
(`rebind::top_level_rebind_risk`) and one clear, the one a `Barrier` applies,
so the value lane, the heap objects and the heap resources drop together. It
replaces ADR-0097 §2.4's top-level row, which forgot the resource *state* alone
and kept the binding and the type lane that the same rebind invalidates.

What runs a userland body is read off the CST (`Stmt::runs`), not the trace IR,
which drops a call nested in an unmodelled expression, the body of a `try` and
a method-call `echo` operand:

- a method or static call, a call through a value, a pipe, and
  `include`/`require`/`eval`;
- a `new` of anything but an engine class: a project class, a project subclass
  of an engine class, an unknown name, `self`/`static`/`parent`, a variable or
  an anonymous class. `new \DateTimeImmutable()` and `new \Exception('…')` run
  the engine's own constructor and are let through;
- a named call that does not denote an engine builtin: a project function, a
  namespaced twin, an unknown name. The builtin test reads the mined arginfo
  table first and the live engine's reflection second, so `--no-php` does not
  turn every `strlen()` into a forgetting;
- an engine builtin handed a userland callee at a position of
  `steins_catalog::callback_carriers`, the carrier rule the by-value lane and
  the fold seam share: `array_map('bump', …)`, `usort($a, 'cmp')`,
  `call_user_func('bump')`, a closure argument. `null`, a carrier-free
  builtin's name (`'intval'`, `'strcmp'`) and an absent optional callback are
  let through. The Deferred route (`ob_start`, `pcntl_signal`, `assert`) is
  left out: a deferred callback runs later, not during the call that stores
  it.

The dump surface and the harness `assertType` are let through by the
recognizers the by-value lane exempts them with.

**When.** A simple statement forgets after its own checks, which judge the
arguments as they were passed, and before its own binding, so `$x = f();`
still binds `$x`. An `if` (and a `match (true)` guard chain, which is one)
forgets once its condition is judged and before any branch is walked:
forgetting before the condition would un-decide it, and a dead branch the walk
can no longer prove dead is checked as if it ran. A loop counts its whole
subtree, since the body's calls run before the header's next evaluation.

**The offset-write leg.** In the top-level frame the narrow barrier of the
2026-09-11 amendment additionally needs the base to hold a `Verified`
value-lane fact, which describes a scalar or an array and never an object;
anything else may be an `ArrayAccess` whose `offsetSet`/`offsetUnset` runs, and
takes the total clear. That settles #696's first point for the offset-write
leg: at top level a write is exposed unless its base is proven not to be an
object.

### What it does not see, by name

- **Userland the engine runs behind a signature or an operator** read as data:
  `__toString` under a conversion, `__clone` under `clone`, `offsetGet` under a
  read, `__get`/`__set`, a generator or `Iterator` under `foreach`,
  `JsonSerializable` under `json_encode`, an engine constructor reaching an
  argument's userland (`IteratorIterator` over an `IteratorAggregate`), a
  destructor, an autoloader, a registered error, output or shutdown handler, a
  user stream wrapper. ADR-0070's top-level refusal and ADR-0097 §2.4's wrapper
  note accept the same calibration.
- **Ordering inside one statement.** The checks judge every argument against
  the facts the statement was entered with, so in `need2(bump2(), $s)` the
  `$s` PHP reads after `bump2()` is judged on its old value; likewise a
  condition read after its own userland call.
- **The routes that do not go through a call.** `include`/`require` of a file
  that assigns, `extract()`, `$$name` and a `&` reference are on ADR-0001's
  give-up list: the top-level scope is poisoned and holds no fact to convict
  with, before the route as much as after it. `$GLOBALS['s'] = 5` written at
  top level is a superglobal target, which the 2026-09-11 target leg already
  sends to the total clear.

### What it costs

The rule gives up the true positives a script earns after its first project
call: in script-shaped code every variable used after one is no longer checked
against what it held before it. On phpstan-src's nsrt corpus (18,153 rows,
mostly function-scoped) 99 rows in 10 files lost precision, 23 of them
`match` → `differ` and 10 `subsumed` → `differ`; the other 66 were already
`differ` or `unsupported` and now answer `unknown` or a wider type. Every moved
row sits at file scope after a call the rule counts: an undefined helper
(`doFoo()`), a project function, a closure handed to `array_map` or
`array_filter`, a static call. Precision for a resolved callee
is the follow-up the ruling names (option 1): forget only the names the callee's
transitive globals-written set names, keeping this rule as the floor for every
call such a summary cannot read.
