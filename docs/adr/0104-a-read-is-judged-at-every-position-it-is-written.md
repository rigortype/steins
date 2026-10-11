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

- a **kind**: `Call(CallExpr)`, `Offset { base, key }`, `Prop { var, prop }`
  or `ClassConst(class, name)`, each lowered as its `ArgValue` twin is, so
  the unresolvable shapes (a dynamic receiver, a `?->` fetch, `Class::class`,
  a chain's outer levels over a non-variable) are not positions;
- its **span**, the read's own;
- its **carrier**.

Every level of an access chain is a position of its own (`$a['n']['zz']` is
two), and so is every call nested in an argument.

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
| `Argument { callee, position }` | an argument; `callee` names a plain function, `position` is `None` for a named argument or one at or after a spread |
| `Conditional` | an arm of a ternary, the right of `&&` `\|\|` `and` `or` `??` `??=` |
| `IssetLike` | the operand of `isset` or `empty`, the left of `??` and `??=` |
| `Suppressed` | under `@` |
| `Write` | an assignment, `&` or destructuring target |
| `CompoundRead` | the target of `++`, `--` or a compound assignment |
| `FirstClassCallable` | `f(...)`, `$o->m(...)`, `C::m(...)` |

A read can sit under several constructs at once. The lowering keeps the
**weakest claim**, in the order `Write`, `IssetLike`, `Conditional`,
`Suppressed`, `CompoundRead`, `Argument`, `Plain`: each carrier before `Plain`
is a reason for a value-dependent family to decline, so the order is the one a
consumer that declines on doubt wants. Two couplings are recorded here for the
consumers. First, `Suppressed` outranks `Argument`; D2 (silence under `@` at
the default floor) is what makes that lossless, and a decision to judge under
`@` has to re-derive the argument's by-reference evidence. Second, the base of
a chain inherits the level above's carrier, except that the base of a
`CompoundRead` is a `Write`, because the level above is fetched for write.

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
  the walk can claim is reached. A `match` subject belongs to the headers.

Branch conditions, loop headers and `switch` subjects are not read here: they
are evaluated in an env this pass does not hold, and S4 gives them a header
record of their own. Declaration constant expressions (a parameter default, a
property or constant initializer) and attribute arguments are out of this run.

### 2.5 Memory

The field costs one word per statement when empty, which is almost every
statement: `Option<Box<Vec<_>>>`, as `unspelled` and `writes_through` are
(#1048). A non-empty statement pays the position records. A `ReadPosition`
owns its `CallExpr` (a clone of the lowering the call would get anyway) or two
lowered `ArgValue`s, so a deeply nested call is lowered once per enclosing
level, quadratic in the nesting depth, which is bounded by the stack guard
every lowering shares. Measured at S1 on the public corpus (release build,
`--no-cache`): peak RSS grows by 25% to 31% (PHPUnit 437 MB to 574 MB, Carbon
414 MB to 514 MB), wall time is within noise. That is the price of cloning
every call and lowering both operands of every offset read; a consumer slice
that needs it back can store a call's span and index instead of its clone.

### 2.6 What this ADR does not decide

- D2, D3 and D5 of the run (`@`, conditional positions, the by-reference
  oracle for method and constructor arguments) are consumer policy.
- Persistence: trace payloads are decoded past the analyzer gate, and the
  analyzer fingerprint refuses a generation written by another build, so the
  new field needs no `SCHEMA_VERSION` bump (`steins-db`'s wire notes).

## 3. Consequences

- Positive: one scan, one table, and every read position the lowering already
  walked is recorded at the cost of a boxed vector.
- Negative: the IR grows; the cost is measured, not assumed.
- Neutral: S1 changes no finding. The first column moves arrive with S2.

## 4. Verification

`crates/steins-syntax/tests/it/read_positions.rs` pins every carrier and
every boundary. The public corpus A/B for S1 is byte-identical under the strict
and default postures.
