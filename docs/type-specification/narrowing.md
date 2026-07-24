# Control Flow and Narrowing

**Status: partial** — the parts that are absent are marked inline and collected
in [not-implemented.md](not-implemented.md). ADR-0031 (branch analysis),
ADR-0052 (narrowing and subtraction).

## The trace model

Propagation runs over a **linear trace IR** per scope (ADR-0027): a flat list of
statements the analyzer understands, with everything else lowered to an explicit
unknown. Control flow is added to the trace **one construct at a time** — the
ADR-0027 ratchet — so that what the trace does not model is visible in the IR
rather than hidden in the walk.

Modeled today:

| Construct | Trace form |
| --- | --- |
| `if` / `elseif` / `else` (statement form) | structured sub-traces per branch |
| statement-position `match` / `switch` | arms + optional default, with strict/loose comparison distinguished |
| `assert($expr)` | a guard applied to the fall-through env at the `Asserted` stratum |
| `throw`, `exit` / `die` | trace terminators |
| loops, `try`, nested blocks, expression-position `match` | `Opaque { writes, reads, poisons }` |
| `goto`, labels, `declare`, `__halt_compiler`, anything unsure | `Barrier` |

`match`/`switch` structuring is **all-or-nothing**: the subject and every arm
condition must lower to a bare variable or a literal, and every non-empty
`switch` case must terminate without fall-through. A single unrepresentable arm
makes the whole construct `Opaque` — partial structuring would be unsound for
`match`'s first-match rule and its `\UnhandledMatchError` on no match.

### What `Opaque` costs, precisely

An `Opaque` construct does not erase the environment. It forgets:

- **`writes`** — every variable the subtree may assign, over-approximated
  (assignment lvalues, compound assigns, `++`/`--`, `foreach` bindings, `catch`
  parameters, `list()` destructuring), *plus* every variable handed to any call
  inside it (by-reference conservatism).
- **`reads`** — every *other* variable the subtree merely mentions. This is not
  paranoia: a construct that reads a variable may branch on it and early-return,
  so the fall-through path can *exclude* the currently-known value. Keeping the
  binding would assert an unreachable path.
- everything, when **`poisons`** is set — the subtree contains an ADR-0001
  poison marker.

Nested function and closure bodies are separate scopes and are not descended.
Over-collection is always sound: it only forgets more.

A known residual gap, recorded rather than hidden: a construct that early-returns
on *every* branch makes all fall-through code dead, so even a fact about a
variable it never reads could describe an unreachable path. Closing that needs
real reachability analysis and is deferred until the trace models control flow.

### Scope poisoning

A scope containing `extract`/`compact`, `global`, `static $x`,
variable-variables, reference assignment, by-ref closure capture, or
`include`/`require`/`eval` is **poisoned**: no variable value is ever considered
known anywhere in it (ADR-0001 give-up list).

## Three fact lanes

A variable can carry three independent kinds of knowledge. Keeping them separate
is what lets each be consumed only where it is sound (ADR-0052 §1).

**1. Value facts** — the four-layer [`Fact`](value-domain.md), in the walk's
environment, with a [trust stratum](trust-stratification.md) and a provenance
line. This is the lane that answers "what value is this".

**2. Contract facts** — the variable's *declared* type as a lowered, syntactic
**arm list**, seeded at scope entry and narrowed arm-wise by guards. A native
member list seeds `Verified`; a `@param` PHPDoc refinement seeds `Asserted`.
Consumed by exactly four things: arm filtering, `instanceof` implication, catch
matching, and the declared-receiver lane (`phpdoc.undefined-method`). It is
**never** consumed by `call.on-null` proofs, arity checks,
`call.undefined-method`, or binding descent.

**3. Class facts (`Member`)** — guard-derived is-a bounds on an object variable:
`instanceof T` on the positive branch binds `T` into `yes`, the negative branch
into `no`. `Member` is deliberately **weaker than exactness** and is not fed to
the exactness-gated consumers. A `Member` on a `final` class is *not* treated as
exactness in v1 — a deliberate, recorded conservatism.

Objects themselves live in the heap store ([object-model.md](object-model.md)).

## Guards

A guard narrows a variable on the branch where it holds.

**Positive facts.** `$x === v` binds a `Singleton`. `is_int($x)` and kin bind the
base. A native-type seed at scope entry binds the declared type's fact.

**Negative facts** (ADR-0031 stage 2):

| Guard | Effect on the branch where it holds |
| --- | --- |
| `!== null` | clears the nullable flag |
| `!== v` | removes `v` from a finite layer |
| `!== ''` | adds `NON_EMPTY` |
| `<`, `<=`, `>`, `>=` against a literal | intersects the int interval |
| truthiness (`if ($x)`) | adds `NON_FALSY`, clears null |
| `instanceof T` | binds into the `Member` lane — **not** exactness |

`instanceof` binding nothing exact is the important one: membership is not
exactness, and treating it as such would make an exact-class fact *wrong* for a
subclass instance.

**Condition evaluation** is trinary. Over a finite candidate set, all member
pairs must agree for a verdict; any disagreement or undecidable pair yields
`Maybe`. `===` and truthiness are exact; `==` is settled **empirically against
PHP 8.5.8** rather than from the manual's table; ordering comparisons decide only
for concrete numeric operands, and any other pairing is `Maybe`.

`instanceof` additionally decides on the **value side, before any class
reasoning**: an operand whose value-domain fact proves it holds a non-object
value (a `null` or scalar `Singleton`/`OneOf`, any `Refined`/`General` — all
four layers denote non-objects) answers `No` — nothing non-object is an
instance of anything. The `No`-side heap conclusion stays exactness-gated as
above.

## Short-circuit threading

Implemented (ADR-0052 §6 / N3). The operands of `&&`/`||` no longer all
evaluate in the pre-branch env: `And(a, b)` evaluates `b` under the entry env
plus `a`'s then-refinements; `Or(a, b)` under the entry env plus `a`'s
else-refinements (De Morgan — `b` runs only when `a` was falsy). The composed
verdict stays the trinary `and`/`or`; only the operand-evaluation env threads
left to right, so a contradiction (`$x === 5 && $x === 6`) proves its branch
dead and a `||` tautology over a finite fact proves its else dead.

**Guard calls are retained**, not opaqued. A resolvable call in guard position
keeps three payoffs: the receiver check runs inside the threaded env (the
`$x !== null && $x->foo()` shape sees a non-null receiver), the callee's
`@phpstan-assert-if-true`/`-if-false` envelopes consume on the matching
polarity — including in nested `&&`/`||` positions, always at the `Asserted`
stratum — and foldable predicate calls evaluate to verdicts where the catalog
licenses it. The obligation is **sequenced, not blanket, invalidation**: a
method call does not rebind its receiver variable (the receiver fact survives
while the escaped object's properties are swept), but by-ref arguments and
other mentions are forgotten at the call's position in left-to-right order.

Ternary arms resolve under the guard's then/else refinements. `$a ?? $b`
lowers to a coalesce value (`ArgValue::Coalesce`) and yields
`clear_null(fact($a)) join fact($b)` — a fact only when *both* operands are
visible, so `??` never manufactures certainty for a value it cannot spell
(an unseen array offset yields no fact); the stratum is the min over both
operands. `??` in *guard position* does not yet refine like
`$a !== null ? $a : $b` — see the gap list below.

## Arm-wise subtraction

Negative guard information removes arms from a contract-fact arm list
(ADR-0052 §2). The subtrahend vocabulary is closed:

| Subtrahend | Guard |
| --- | --- |
| `Null` | `!== null` |
| `Value(v)` | `!== v` |
| `Base(b)` | `!is_int($x)` and kin — deletes the base's arm and every literal arm it covers |
| `Class { fqn, polarity }` | `instanceof` narrowing over class arms |

**An arm dies iff the subtrahend subsumes it with `Yes`.** `Maybe` keeps the arm
— the silence side. Surviving arms keep their own stratum, so an `Asserted` arm
can never launder to `Verified` through subtraction.

The `Class` subtrahend is **polarity-asymmetric**, and the asymmetry is a
soundness rule, not an optimization: on the negative branch (`!($v instanceof
T)`) is-a is inherited, so every arm that is provably a `T` dies; on the positive
branch, deleting the non-instances requires knowing no descendant of the arm is a
`T`, which is finality-gated. A catalog-backed is-a verdict used for arm deletion
is additionally demoted to `Unknown` when the project's PHP minor line differs
from the catalog's pin (ADR-0052 amendment A11).

## Branch joins

At a merge point, per-variable facts are joined by the value domain's `join`
(precision may be lost, members never are; an unrepresentable join drops the
fact). Contract arm lists are concatenated and then **deduped by structural
equality** before subsumption collapse.

The dedup is not a micro-optimization. Structural equality on canonical facts is
cheap and total, and without it a deeply nested `if` over opaque arms doubles the
arm list at every join — a measured non-termination on real code (a single
migration method with ~40 sequential `if` blocks grew one variable's lane to
65,280 arms, all of one distinct shape). Canonical forms are what make the fix
correct rather than a heuristic cap.

## `@phpstan-assert` application

After a call to an assertion helper, the asserted type narrows the **caller's**
environment for the variable passed at the asserted position:

- `@phpstan-assert T $x` applies on the fall-through (statement position).
- `@phpstan-assert-if-true` / `-if-false` apply only in guard position.

All of these bind at the `Asserted` stratum
([trust-stratification.md](trust-stratification.md)), so they narrow contract-
layer reasoning but never premise a proof-layer finding.

## Not implemented

Recorded honestly; each costs true positives, never false positives, because an
unknown widens to silence. The first three are ADR-0052's N5/N6 slices,
**deferred out of v0.1.0 by owner decision** — designed in full, no code.

- **Loops as anything but `Opaque`** (N6). No structured loop walk, no
  loop-carried facts; only the write/read-set invalidation above.
- **Property chains as guard operands and static properties as a fact lane**
  (N5). One level of property access is modeled
  ([object-model.md](object-model.md)); chained lvalues stay `Barrier`.
- **`??` in guard position** — refining like `$a !== null ? $a : $b`; today
  `??` yields a value fact only.
- **`try`/`catch`/`finally` control flow.** Catch *matching* consumes contract
  facts, but the construct itself is `Opaque` for value flow.
- **Array element narrowing.** An array is a fact only when *fully* known.
- **Reachability analysis** — the all-branches-early-return gap above.
