# A read is judged at every position it is written, through its carrier

**Status: proposed (2026-10-11), PENDING ratification.** Designed by the
architect for the read-position run (issue #1050, decision D1, adopted under
the owner's policy "proceed with the recommendations"). This ADR lands with
slice S1, which adds the IR and no consumer: `check` output is unchanged.
Slices S2 (calls), S3 (offset, property and class-constant reads) and S4
(headers) read it.

## 1. Context

The checks that judge a call or a member read see only the positions a
statement *kind* spells.

- **Calls.** The existence, arity, inaccessible-method and on-null checks
  iterate `checkable_calls`, which returns the `CallExpr` the statement kind
  names: a `Call`, an `Assign`'s or `Return`'s call, a `PropAssign` value, an
  `Echo` operand. A call nested anywhere else (an argument, an operand, an
  array element, a `new` argument, a `throw` or `exit` operand, a condition, a
  `static` initializer) is never judged, and what survives of it is an
  `ArgValue::Call(String, Vec<ArgValue>)` that carries no span (#935).
- **Member and offset reads.** These are judged by pattern-matching an
  `Assign` or `Return` whose value is the read (#936, #949). `echo $a['k']`,
  `strlen($o->p)`, `$a['k'] + 1` and `A::NOPE === 1` are silent, and PHP warns
  or throws on each.
- **The data exists at lowering.** The same lowering already scans every
  expression of the four entry-env statement kinds for string contexts, guard
  regions, invalidated names and `runs`, and drops what a read position would
  need: its span and the construct around it.

The positions differ in what PHP does with them, so "judge every read" is not
one rule. `isset($a['k'])` and `$a['k'] ?? 1` are silent for an offset and a
property and throw for a class constant; `f($a['k'])` creates the key when `f`
takes a reference; `@$a['k']` warns and still runs the error handler; a
ternary arm may not run. A consumer that discovered these from the lowered
kind would re-derive the same table at every site.

## 2. Decision

### 2.1 D1: a finding about a read is reported at the read's own span

A finding about a read carries the span of the read (`$a['k']`, `f()`,
`$o->p`, `A::B`), not the span of the statement or of the enclosing
expression. `offset.missing` for `echo 'x' . $a['k'];` points at `$a['k']`,
the text to fix, as the family already does for an `Assign` value.

Existing findings move with it: a finding judged at the statement today moves
to its read's column. The fp-gate key includes the column, so the change
costs one conscious reseed when the consumers land (S2 and later); S1 itself
moves nothing. A column move is classified in an A/B as "same finding, moved
to the read".

### 2.2 The IR: `Stmt.read_positions`

Each statement carries `read_positions: Option<Box<Vec<ReadPosition>>>` (the
accessor `Stmt::read_positions()` returns a slice). A `ReadPosition` is

- a **kind**: `Call` (identified by its span alone), `Offset(place, key)`,
  `Prop(var, prop)` or `ClassConst(class, name)`. A `Place` is a variable or a
  literal-keyed offset of one, a `Key` an integer or string literal; neither
  holds a lowered `ArgValue`. A `?->` fetch, `Class::class` and a property
  chain's outer levels are not positions;
- its **span**, the read's own;
- its **carrier**, a `Copy` enum.

Every level of an access chain is a position of its own (`$a['n']['zz']` is
two), and so is every call, nested or not, in source order with the outer
read first. A `Carrier::Argument` names the call it is an argument of by that
call's index in the same list.

Two kinds of read are not recorded: a write target, and an offset or property
under `isset`, `empty`, `??` or `??=`. Every family is silent on them (the run's
decisions D1-D5 judge neither), so a consumer would skip them; a class constant
under `isset`-like constructs is recorded, because PHP throws there. A call
carries no lowered `CallExpr` in the position: slice S2 resolves the span to the call
the lowering already holds (`SourceTree::call_at` for a function call,
`Scope::method_call_at` for a method or static call or callable, `Stmt::read_calls` for a
`new` and a function first-class callable), which keeps the IR free of a second copy of
every call.

### 2.3 Why carriers are classified at lowering time

The carrier is a syntactic fact about the construct around the read, and the
syntax layer is the only place that holds the CST. The inference layer sees
`ArgValue`s, which erase most of the constructs that decide the answer: a
ternary arm, an `isset`, an `@`, a `&` operand, a destructuring target and a
first-class callable are all `Other` or a different kind by the time a walker
asks. Classifying once, next to the scan, gives the consumers one table to
read and the tests one place to pin it.

The carrier says what construct sits around the read, not whether a finding is
due. The syntax layer knows no signatures and no environments, so it decides
nothing: by-reference parameters, decided guards and the strict floor stay the
consumers' questions, answered per family. The families split into those whose
answer does not depend on the environment (an undefined function, an
undeclared constant, an inaccessible member, an arity mismatch) and those
whose does (a missing offset, an undefined property, a call on null); S2 and
S3 own that table.

| Carrier | The construct |
| --- | --- |
| `Plain` | an operand PHP evaluates unconditionally and reads |
| `Argument { call, position }` | an argument; `call` is the index of its call position, `position` is `None` for a named argument or one at or after a spread |
| `Conditional` | an arm of a ternary, the right of `&&` `\|\|` `and` `or` `??` `??=` |
| `IssetLike` | a class constant under `isset`, `empty`, `??` or `??=` (an offset or property there is not recorded) |
| `Suppressed` | under `@` |
| `CompoundRead` | the target of `++`, `--` or a compound assignment |
| (kind `CallRef`) | `f(...)`, `$o->m(...)`, `C::m(...)`: its own kind, not a carrier, with the carrier folded like a call's (`$f && missing(...)` is `Conditional`); the run's S2 row "FCC existence" consumes `kind == CallRef` |

A read can sit under several constructs at once. The lowering keeps the
**weakest claim**, in the order write (not recorded), `IssetLike`, `Conditional`,
`Suppressed`, `CompoundRead`, `Argument`, `Plain`: each carrier before `Plain`
is a reason for a value-dependent family to decline, so the order is the one a
consumer that declines on doubt wants. Two couplings are recorded here for the
consumers. First, `Suppressed` outranks `Argument`; D2 (silence under `@` at
the default floor) is what makes that lossless, and a decision to judge under
`@` has to re-derive the argument's by-reference evidence. Second, the base of
a chain inherits the level above's carrier, except that the base of a
`CompoundRead` is a `Write`, because the level above is fetched for write.

The full rule table, one row per construct that writes, silences or
short-circuits a read, is in the module doc of `lower_read.rs`, with a test per
row. Beyond the order above:

- **`?->` short-circuits the rest of the chain.** Once a receiver is null PHP
  evaluates neither the member, nor its arguments and keys, nor any later link
  (`$n?->m(missing(), A::NOPE, $a['k'])` evaluates nothing). Everything past a
  `?->` is recorded `Conditional`; the receiver itself keeps the inherited
  claim.
- **The class operand of a static call is part of the chain.**
  `$n?->s::m($a['k'])` and `($n?->p)::m($a['k'])` short-circuit their arguments
  like a method call on `$n?->s` does.
- **Known IR gap, not a false positive.** A key that sits under a write target
  and past a `?->` (`$a[$n?->p]['k'] = 1`) is recorded as an ordinary read of
  its operands without the short-circuit; PHP also raises a deprecation there
  that no position models.
- **An append writes its base.** `$a['k'][] = 1` creates `$a['k']`; no level of
  its base is recorded.

Two obligations fall on a consumer:

- A by-reference argument also silences the base levels of its chain:
  `f($a['x']['y'])` records both `$a['x']['y']` and `$a['x']` as `Argument`,
  because a by-reference parameter creates the whole path. A consumer that
  declines the argument declines the levels with it.
- `position: None` (a named argument, or one at or after a spread) means the
  by-reference parameter is unknowable. A value-dependent family must decline
  there, not judge.

### 2.4 The boundary

The scan reads the statements whose entry env is the env PHP evaluates the
expression in: an expression statement, `return`, the two `echo` forms, and a
`static` initializer. It also fills the entries that bypass `lower_stmt` and
evaluate an expression in the same way: an arrow function's body, a `get` or
`set =>` hook's, and a hoisted `match` arm's body. It stops where the string
contexts scan stops, and one construct further:

- a function-like (function, method, closure, arrow function, property hook)
  and a class-like is its own scope, lowered and judged separately; an
  anonymous class contributes its constructor arguments, which the enclosing
  statement evaluates, and none of its body;
- a `match` is not descended. A structured one is hoisted and each arm is a
  statement carrying its own positions; an unstructured one is not a position
  the walk can claim is reached. A `match` subject, and every arm condition
  of a `match` (`echo match ($a['subj']) { $a['arm'] => 1 }`), are never
  recorded here: S4 gives the arm conditions the same header record as the
  subject.

Branch conditions, loop headers and `switch` subjects are not read here: they
are evaluated in an env this pass does not hold, and S4 gives them a header
record of their own. Declaration constant expressions (a parameter default, a
property or constant initializer) and attribute arguments are out of this run.

### 2.5 Memory

The field costs one word per statement when empty, which is almost every
statement: `Option<Box<Vec<_>>>`, as `unspelled` and `writes_through` are
(#1048). A non-empty statement pays the position records, shrunk to fit.
The first cut stored a clone of each `CallExpr` and two lowered `ArgValue`s per
offset read, in an enum as wide as its largest variant: peak RSS grew 25% to
31% (PHPUnit 437 MB to 574 MB), which issue #890 cannot take. The record is
now 40 bytes (`size_of::<ReadPosition>()`: a span, a 16-byte `Copy` carrier
and a 16-byte kind whose heap parts are boxed), a call is its span, an offset
is a place and a literal key, and writes and `isset`-silent reads are not
recorded. Nothing is lowered twice: the scan builds no `CallExpr`, so a nested
call costs no more at depth than at the top. Measured (release build,
`--profile strict --no-php --vendor-diagnostics --no-cache`, median of 3 runs
of peak RSS): PHPUnit 439 MB to 450 MB (+2.5%), Carbon 411 MB to 412 MB
(+0.1%), PHP-Parser 137 MB to 141 MB (+2.6%); wall time is within noise.

### 2.6 What this ADR does not decide

- D2, D3 and D5 of the run (`@`, conditional positions, the by-reference
  oracle for method and constructor arguments) are consumer policy.
- Persistence: trace payloads are decoded past the analyzer gate, and the
  analyzer fingerprint refuses a generation written by another build, so the
  new field needs no `SCHEMA_VERSION` bump (`steins-db`'s wire notes).

Slice S2 measured the call lookup: peak RSS on PHPUnit (release, strict `--no-php
--vendor-diagnostics --no-cache`, median of 5) 454.8 MB to 461.3 MB, +1.4%. A first cut
that kept a `CallExpr` per nested call cost +4% to +7%.

### 2.7 Decisions and known gaps recorded by S2

- **`@` plus a conditional is `Conditional`.** The weakest-claim order puts `Conditional`
  before `Suppressed`, so a call under both is judged for existence. That is sound, since
  an `Error` is not suppressible, and differs from D2 (silence under `@`), which holds for
  a call under `@` alone.
- **Known gap:** calls in an anonymous class's constructor arguments have positions but no
  lowered call, so they are not judged.
- **Known gap:** `call.inaccessible-method` is treated as value-dependent, so it declines
  at a conditional position; a static `C::priv()` there is a false negative.

## 3. Consequences

- Positive: one scan, one table, and every read position the lowering already
  walked is recorded at the cost of a boxed vector.
- Negative: the IR grows; the cost is measured, not assumed.
- Neutral: S1 changes no finding. The first column moves arrive with S2.

## 4. Verification

`crates/steins-syntax/tests/it/read_positions.rs` pins every carrier and
every boundary. The public corpus A/B for S1 is byte-identical under the strict
and default postures.

## Amendment (2026-10-11): the S3 consumer for offset, property and class-constant reads — PENDING ratification

Slice S3 (issues #936, #949) is the first consumer of the offset, property and
class-constant positions. It changes the IR in one place and fixes the
consumer rules below.

**`Key::Var`.** §2.2 says a key is an integer or string literal. A key is
now also a bare variable, `Key::Var(name)`, which the consumer resolves against
the env it judges the read in, as the offset family always did for
`$x = $a[$k]`. Without it, removing the `Assign`/`Return` arms would have
silenced that read. Any other key is `Key::Other`, including a literal float,
bool or null (`$a[true]`, `$a[null]`). Those were judged before, as the whole
value of `=` or `return`, and are now silent: a known, accepted loss.

**The carrier × family table** (`stmt_checks.rs`'s module doc holds it with
the PHP behaviour per cell):

| Carrier | offset, property value | `property.inaccessible` | class constant |
| --- | --- | --- | --- |
| `Plain` | judge | judge | judge |
| `Argument`, by-value parameter of a resolved function | judge | judge | judge |
| `Argument`, by-reference, method, constructor, unresolved, `position: None` | decline | judge | judge |
| `Conditional` | decline (D3) | decline | judge (D3) |
| `IssetLike` | not recorded | not recorded | judge |
| `Suppressed` | decline (D2) | judge | judge |
| `CompoundRead` | offsets: judge `offset.missing` and the strict leg, not `offset.on-unsupported`; properties: decline | decline | not a target |

`property.inaccessible` judges an `@` and a by-reference argument position:
the fatal is not suppressible, and PHP routes a by-reference fetch of an
inaccessible property through the same `__get` lookup as a read. Witnessed on
PHP 8.5: `byref($c->priv)` throws `Cannot access private property`, and with
only `__get` declared it calls `__get`. It declines at `Conditional`. Its
subject is the receiver, and a test on the receiver decides whether the arm
runs: `isset($c->priv) ? $c->priv : 0` is silent in PHP, because `isset` on an
inaccessible property without `__isset` is false, and so are
`$c instanceof X ? $c->priv : 0` and `is_null($c) ? 0 : $c->priv` when the test
is not decided. Every family whose subject is the receiver or its value
declines at `Conditional`; only the class-constant families, whose subject is
a named class that no test on a value decides, judge there (D3), and a decided
guard still makes their arm dead. A compound write takes `__set` on its write
half, which the check does not model, so it declines.

**Two rules across the table.** First, a position inside an extent the guard
sweep proves dead is not judged; the sweep now runs before the reads. Second,
a read of a variable (offset base or key, property receiver) declines when
the same statement may rebind that variable before the read runs. That covers
a name handed to a by-reference or unresolved call, or written by an embedded
assignment (an entry of `Stmt::invalidated` that `by_value_survivors` does not
keep), and every name in the top-level frame when the statement may run
userland. It also covers a by-reference parameter when the statement may run
userland, which can write through the caller's alias. The userland question
is `rebind::may_run_userland`, the frame-free half of
`top_level_rebind_risk`. A finding the position pass would raise at an
`(id, line, column)` that the statement's other checks (the `??` final arm, the
destructure source) already raised is not raised twice.

**Argument callee lookup.** An argument's call is found by its span in
`SourceTree::calls()`, by binary search on the start offset. This assumes the
file's calls are collected in source order, as the pre-order collector does.
If that ever breaks, the lookup misses, and the argument declines as one
whose parameter is unknowable: silence, never a false positive.

**`defined()` over a concatenated name.** A `defined()` argument that
concatenates string literals and `X::class` fetches of a named class
(`defined(K::class . '::NOPE')`) folds as the name it spells, so the guard
decides its arm and its `if` body as a literal name does.

**A base false positive removed.** A guard that proves present a key that a
declared shape cannot carry (a sealed shape without the field, or an unsealed
shape whose tail key class rejects the key; `@param array{type: string} $s`, then
`isset($s['url'])`) left the shape unchanged. The guarded read was then
reported as `offset.undeclared`, a read PHP makes without a warning. This
false positive was on the base already; S3 would have raised it at more
positions (two on the public corpus). The guard now widens the shape to carry
the key, with no value and no order, count or cover claim (`shapes.rs`).

Measured: on the ten public packages, under strict with vendor diagnostics,
default, and strict without PHP, the only difference is one strict finding
that moves to its read's column. Peak RSS on PHPUnit (strict, no PHP) grows
1.0%, 455.9 MB to 460.4 MB, by the review's measurement.
