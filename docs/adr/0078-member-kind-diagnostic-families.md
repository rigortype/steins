# Member-kind diagnostic families and the port-wave floor table

Issues #182–#200. Status: PENDING ratification (autonomous design under the
owner's post-hoc-ratification mode, per the ADR-0063/0067/0076/0077
precedent). Context: ADR-0022 (id registry), ADR-0050 (layers/profiles),
ADR-0062 A-G10 (surface floor), ADR-0049 §7 (warning-handler gate) and its
2026-08-08 amendment (stratum routing, dischargeable obstacles), ADR-0079
(parse-failure dam), the owner's zero-FP policy restatement of 2026-08-08,
and the measurement in `docs/notes/20260808-phpstan-rule-port-map.md`: of
PHPStan's ~500 identifiers, 86 ever fire over fourteen real applications
and nine cover 95% of the volume — the port is a ~twenty-id project, and
this ADR names the twenty.

## 1. The naming decision

The registry's families follow two axes today: the **premise** axis
(`type.*` = Verified native evidence, `phpdoc.*` = Asserted docblock
evidence — the paired-id precedent) and the **syntactic** axis (`call.*`,
`class.*`, `offset.*`, `throw.*`, `effect.*`). The port wave adds ids that
fit neither cleanly, and the decision is a third axis rather than a
stretch: **member-kind families**, where the first segment names what kind
of member or construct the finding is about — `property.*`, `constant.*`,
`variable.*`, `class-const.*`, `override.*`, `string.*`, `untyped.*`.

Bound conventions:

1. **No PHPStan identifier mirroring.** `property.notFound` is PHPStan
   vocabulary; Steins ids are `family.kebab-rule` and their meanings are
   Steins contracts (ADR-0022). The port map records the correspondence;
   the registry does not.
2. **Visibility is a rule name, not a family.** `call.inaccessible-method`,
   `property.inaccessible`, `class-const.inaccessible` — a `visibility.*`
   family would cut by *why* while every other family cuts by *what*.
3. **The `maybe-` sibling convention generalizes.** The possibly-grade twin
   of a definite id is spelled with a `maybe-` prefix on the rule name and
   floored at `strict` (`offset.missing` / `offset.maybe-missing` is the
   precedent pair). A `maybe-` sibling is **registered ahead of emission**
   (`REGISTERED_NOT_YET_EMITTED`, the `call.too-many-arguments` precedent)
   whenever its definite leg ships — registration is the mechanical
   enforcement of "the possibly-leg is named, never scoped out of
   existence."
4. **Gate boundaries and id boundaries coincide.** A warning-grade
   consequence and a fatal consequence never share an id, because the
   ADR-0049 §7 warning-handler gate demotes warning-grade findings under a
   declared `"null"` posture and an id must demote whole or not at all.
   This is why the string-context check below is two ids.
5. **A family prefix may span layers.** The layer is a registry attribute,
   not a prefix property (ADR-0050), so `phpdoc.*` carries both contract
   ids and the new mechanics hygiene ids. One consequence is user-visible
   and recorded here: a prefix pattern in `@steins-ignore` or a profile
   (`phpdoc.*`) matches ids across layers, but mechanics ids remain
   `disable`-proof regardless — the prefix match does not override the
   anti-rot channel.

## 2. The floor table

Layer and floor per id. "gate" marks warning-grade ids behind
`warning_handler_abort` (proof under the default `"abort"` posture,
demoted under `"null"`), per the `offset.missing` precedent. Every fatal
claim ships `php -r`-witnessed per the ADR-0049 point-10 discipline.

| id | layer / floor | notes |
| --- | --- | --- |
| `property.undefined` | proof / Default, gate | undefined property **read**; `__get`/`#[AllowDynamicProperties]`/`stdClass` descent are A14 obstacles |
| `property.maybe-undefined` | proof / Strict | declared-shape possibly leg; emitting since ADR-0081 §7 — the §8 ladder proves the property absent on some union arms and declared on the rest |
| `property.on-non-object` | proof / Default, gate | property fetch on proven non-object |
| `property.inaccessible` | proof / Default | fatal; visibility from the resolver that already computes it |
| `class-const.undefined` | proof / Default | fatal; enum cases and interface constants are member sources |
| `class-const.inaccessible` | proof / Default | fatal |
| `constant.undefined` | proof / Default | fatal since 8.0; global constants; computed `define()` dams |
| `variable.undefined` | proof / Default, gate | never-bound in scope; `extract`/`compact`/`$$` dam the scope |
| `variable.maybe-undefined` | proof / Strict | some-paths-only; emitting since ADR-0081 — the binding-presence pass is the reachability foundation it waited on |
| `phpdoc.maybe-undefined` | contract / Contracts | the same question from a declared premise (ADR-0087 §4): a read of a top-level `@var T\|unset $x`. Not a member of this family's proof-layer ladder — it sits on the contract layer because the premise is the author's assertion, and shares only ADR-0081's presence engine |
| `call.inaccessible-method` | proof / Default | fatal; `__call` is an A14 obstacle |
| `call.on-non-object` | proof / Default | fatal; sibling of `call.on-null`, whose meaning is unchanged |
| `call.printf-too-few-arguments` | proof / Default | format-string-derived arity; distinct from signature-derived `call.too-few-arguments` so the M2 internal-arity slice stays clean |
| `class.abstract-unimplemented` | proof / Default | load-time fatal |
| `class.extends-final` | proof / Default | load-time fatal |
| `override.final` | proof / Default | load-time fatal |
| `override.static-mismatch` | proof / Default | load-time fatal, both directions |
| `override.visibility-weakened` | proof / Default | load-time fatal |
| `override.parameter-variance` | proof / Default | native signatures only in v1; any Asserted premise is silence, twin deferred until ADR-0032 carry settles the generics vocabulary |
| `override.return-variance` | proof / Default | same v1 boundary |
| `foreach.non-iterable` | proof / Default, gate | single-id family, `readonly.reassigned` precedent |
| `string.non-stringable` | proof / Default | fatal: object without `__toString` in string context |
| `string.array-conversion` | proof / Default, gate | warning: array in string context |
| `type.invalid-operand` | proof / Default | binary/unary/comparison in one id; fatal rows only, version-sensitive rows ask the sidecar |
| `type.return-missing` | proof / Default | fall-through past a non-void native return type **with no `return`/`throw`/`exit` anywhere in the body** — a stub, an empty body, pure side effects, where every execution fatals; the reachability tracer — *added here beyond the approved table, flagged for ratification* |
| `type.return-maybe-missing` | proof / **Strict** | the same fatal where the body *does* exit somewhere but not on every path (a no-`default` `switch` whose every case returns, an `if` with no `else`). §1.3's `maybe-` sibling, emitted with its definite leg rather than deferred. The **layer** is the definite leg's — one consequence, one layer — and only the floor differs; the first proof-layer id at `strict`, on the 2026-08-08 gate measurement (phpstan-src's own `src/` carries two and passes its own missing-return rule) |
| `preg.invalid-pattern` | proof / Default, gate | PCRE refusal witnessed by the sidecar; joins the preg-slice vocabulary |
| `array.duplicate-key` | mechanics / Default | works but drops a value silently — intent/behaviour drift, the anti-rot shape |
| `syntax.unparsable` | mechanics / Default | ADR-0079 |
| `phpdoc.unparsable` | mechanics / Default | docblock does not parse (within the read tag set only) |
| `phpdoc.stale-param` | mechanics / Default | `@param` names a parameter the signature lacks |
| `phpdoc.stale-var` | mechanics / Default | `@var` names an absent or different variable (merges PHPStan's variableNotFound/differentVariable) |
| `phpdoc.misplaced-var` | mechanics / Default | `@var` where nothing adopts it |
| `phpdoc.throws-not-throwable` | mechanics / Default | `@throws` names a non-Throwable |
| `closure.unused-use` | mechanics / Default | `use ($x)` never read |
| `untyped.parameter` | contract / Contracts | no native type and no docblock claim |
| `untyped.return` | contract / Contracts | |
| `untyped.property` | contract / Contracts | |
| `untyped.class-constant` | contract / **Pedantic** | moved off the family floor by the 2026-08-09 owner ruling: a constant is inherently static — its initializer is a constant expression, so the type is pinned whether or not one is written — making it the only arm whose silence withholds nothing. Inheritance can still overwrite a constant with a differently shaped value; that risk is accepted knowingly, and unlike a property Steins does not ask for the declaration. Not `Strict` either, which asks a different question (is a weaker some-paths-only claim worth seeing?): demanding the declaration is a house-style ask, so it takes the `Pedantic` rung no built-in reaches by rung, and the `pedantic` branch names it in `enable` |
| `untyped.iterable-value` | contract / Contracts→Strict by measurement | `array` with no value type |
| `untyped.generics` | contract / Contracts→Strict by measurement | generic class used bare |

`untyped.*` is deliberately not `phpdoc.*`: the phpdoc family reports a
claim that *disagrees* with the code; `untyped.*` reports a claim the code
*does not make*. The lint boundary holds because an absent claim is debt,
not style — the contract layer's exact definition.

## 3. Deferred, by name

Recorded so the registry's silence is named (ADR-0049 point 10 shape):

- **The unbound-variable guard trio** (`isset`/`empty`/`??` on a
  never-bound name): legal PHP, constant-false guard. Mechanics would make
  it un-disableable against defensive-coding house styles — a crying-wolf
  risk — and it is neither breakage nor declared debt. No id; triage
  measures the shape first (issues #50/#194).
- **Dynamic property write** (`property.dynamic-write` shape): deprecation
  today, fatal at PHP 9.0. Ask-the-real-thing forbids calling it proof
  while the project's PHP tolerates it; when the sidecar reports ≥ 9.0 it
  becomes a proof id. Designed, not registered.
- **Undeclared static property access** (`C::$prop`, issue #197): a fatal
  `Error` (`Access to undeclared static property C::$nope`, witnessed at
  8.5.9), so §1.4 forbids it riding `property.undefined`'s warning-grade
  id and it would need a row of its own. The trace IR carries no
  static-property *read* site either — `Node::StaticPropertyAccess` is
  collected only as a class reference, for `class.undefined` — so the
  slice is a lowering change, not a ladder change. Named here rather than
  minted.
- **The property family's phpdoc twin.** A13 routes an Asserted
  declared-receiver *method* claim to `phpdoc.undefined-method`; the
  property family has no such id in the table above, so an Asserted arm is
  simply silence for `property.undefined` (issue #197's calibration
  boundary). Adding the twin is a registry addition, and waits on
  measurement asking for one.
- **`class.undefined` contract twins** (`instanceof` on a missing class,
  docblock class references): contract-layer by consequence, named at
  slice time under this ADR's vocabulary (issue #182 follow-up).
- **`override.*` phpdoc twins**: after ADR-0032 generics carry.

## 4. Consequences

- The registry grows by ~30 entries across two waves; the totality test
  and `REGISTERED_NOT_YET_EMITTED` discipline apply unchanged. Each
  family's first PR seeds its fp-gate expectations, and every
  warning-grade id wires the same `warning_handler_abort` gate the offset
  family built — no second mechanism.
- Issues #182–#200 carry these ids in their acceptance criteria; the port
  map note records the PHPStan correspondence for readers arriving from
  that side.
- `CONTEXT.md` gains the session's four terms (member-kind family,
  dischargeable obstacle, maybe- sibling, warning-handler gate).

## 5. The reachability foundation and its seam (issue #199)

`type.return-missing` is the one row in §2 that is not a rule port. It is
the **tracer** of a foundation the port map needed three separate times
and could not scope: a per-scope terminality judgment. The foundation
landed with it, and this section records the seam so the deferred
consumers do not each invent their own.

**The judgment.** `steins_syntax::BodyEnd` — `Terminates` /
`FallsThrough` / `Unknown` — is computed per statement at lowering time,
from the CST, and carried on `Stmt::end`; `body_end(&[Stmt])` folds a
statement list to the same three-valued answer. It is deliberately
computed from the CST rather than from the trace IR, because the IR
erases every loop, `try` and `switch` into one undifferentiated
`StmtKind::Opaque` and `goto` into a `StmtKind::Barrier` — the two
distinctions the judgment lives on. It is env-free, index-free and
project-free: a **syntactic** control-flow reading, where a branch
condition is non-deterministic and only a construct with no exit edge at
all (`return`, `throw`, `exit`, an `unhandled` `match`, a `while (true)`
with no `break`) terminates.

**The asymmetry, which is the point.** `Unknown` is not a defect to be
smoothed away; it is the honest verdict for a construct whose exit edges
the judgment does not bound. Its *safe side differs by consumer*, and
each consumer must name which side it takes:

| consumer | the accusation | safe reading of `Unknown` | predicate |
| --- | --- | --- | --- |
| `type.return-missing` | "this body runs off its end" | **terminating** — silence | `provably_falls_through()` |
| the level-4 dead-code family (`UnreachableStatementRule`, `CatchWithUnthrownExceptionRule`, the unused-private trio) | "the statement after this never runs" | **not terminal** — silence | `provably_terminates()` |
| `variable.maybe-undefined` (#194), some-paths-only | "this path reaches a read with no write" | not terminal — the path stays live, so no claim | `provably_terminates()` |

Both predicates exist precisely so that no consumer writes
`!= Terminates` or `!= FallsThrough`: each negation is correct for one
consumer and inverts the other's safe side, and that is the mistake the
type exists to prevent.

**The second question, and the definite/possibly split.** `body_end`
answers *does control reach the end*; `body_has_terminator` answers *does
the body exit the function anywhere*, reading a `Stmt::has_terminator`
the lowering computed over the whole CST subtree (so a `return` inside a
`foreach` body or a `try` block counts, though the trace IR erased the
construct). The pair splits a falling-through body into the two
populations §1.3's `maybe-` convention exists for: **no exit anywhere** is
unconditional and stays `type.return-missing` at `Default`; **an exit that
does not cover every path** is `type.return-maybe-missing` at `Strict`.
One predicate routes the finding, so the two ids are disjoint by
construction and no site can report both.

**Named silences of the foundation**, so its quiet is measured rather
than assumed: `try`/`catch`/`finally` is excluded whole (`finally`
overwrites the exit point — `try { return 1; } finally { return 2; }`
returns `2` on 8.5.9, and a returning `finally` swallows an in-flight
exception, so neither direction is readable off the block ends);
`goto` and labels are unbounded jumps; a `switch` whose case body runs
into the next case is not modelled; a provably-infinite loop containing a
`break` whose target is unresolved is undecided. A call to a callee
proven never to return is not the judgment's business either — it needs
the project index — so `type.return-missing` applies that refinement
itself, at the emitter, and the undeclared never-returner (a helper that
calls `exit` without declaring `: never`) is its one named over-report
risk. Inferring `never` from a callee's own `BodyEnd` is the obvious next
consumer of this seam.

## 6. The `pedantic` rung and branch (2026-08-09 owner ruling)

`untyped.class-constant` forced a distinction the ladder did not have a
place for, and §2's table now records the result. The finding itself is
unchanged — same id, same layer, same emission conditions — but the rung
that asks for it is new, and so is the built-in that reaches it.

**The ruling.** A constant is inherently static: its initializer is a
constant expression, so the declaration pins the type whether or not one
is written. Every other arm of the `untyped.*` family names information
that is genuinely lost — a parameter, property or return with no type is
`mixed` — while this arm's silence costs nothing. Inheritance can still
overwrite a constant with a differently shaped value, and that risk is
accepted knowingly: unlike a property, Steins does not ask for the
declaration. Teams that *do* want to require it must still be able to,
so the check is calibrated rather than deleted — the standing rule that
zero-FP means calibrated defaults, not omitted checks.

**Why not `Strict`.** The strict rung asks one question: is a weaker,
some-paths-only claim worth seeing? Its whole population is `maybe-`
siblings and shapes that defensive house styles produce on purpose. A
team opting into that has not thereby asked to be told how to write its
constants. Parking a house-style ask there would make every `strict` user
inherit a style opinion the analyzer does not hold — the same objection
that took the id off `contracts`, one rung higher.

**The amendments.** Two prior decisions are narrowed:

- **ADR-0062 A-G10** wrote `surface_floor ∈ {default, contracts, strict}`
  and "profiles are the cumulative ladder `default ⊂ contracts ⊂
  strict`". The floor set gains a fourth member, `pedantic`, and stays a
  total order — `Default < Contracts < Strict < Pedantic` — because
  `floor(id) <= rung` depends on it. What is narrowed is the second
  clause: the *rungs* remain a cumulative chain, but the *built-in
  profiles* are not one, which was already true when A-G10 was written
  (`throws-direct` branches off `default` via `enable`) and is now true
  by design rather than by exception.
- **ADR-0050 §5 / G1 amendment** ships a fifth built-in, `pedantic` =
  `contracts` plus the pedantic-floor ids named in its `enable` list. Its
  rung is `Floor::Contracts`; it reaches above its rung exactly the way
  `throws-direct` does. No built-in carries `Floor::Pedantic` **as a
  rung**, and a test pins that: an id parked at that floor is off every
  built-in surface until something names it.

**What this costs, accepted.** There is no built-in meaning "everything
on" — `pedantic` and `strict` are incomparable, each holding what the
other lacks (measured: 62 ids and 65 ids, over a shared 61). A project
wanting the union writes `extends = "strict"` with the pedantic ids in
its own `enable`. The second cost is that a baseline captured under
`pedantic` tags its entries `contracts`, its rung — the same
under-reporting `throws-direct` already produces, and harmless for the
same reason: the staleness predicate is `captured <= rung && surfaces_id`,
and the second conjunct is what actually gates.

**The per-id path is unchanged and remains the finer one.** `enable` was
always orthogonal to the rung, so a project can take one pedantic id
without the profile:

```toml
[profile.house-style]
extends = "contracts"
enable = ["untyped.class-constant"]
```

## 7. Protected visibility is judged against the member's root class (2026-10-03, issue #942)

Status: PENDING ratification (post-hoc, as above).

`call.inaccessible-method` and `property.inaccessible` ran the protected leg
against the class that *redeclares* the member. PHP asks the member's **root**:
the class that first introduced it in the inheritance line (for a method,
`zend_get_function_root_class` reads the prototype's scope). With `Base {
protected h() }`, `A extends Base` redeclaring `h()`, and `B extends Base`,
`(new A)->h()` from `B` is legal, because `B` is in `Base`'s hierarchy. Judging
against `A` found `B` unrelated and reported a call that runs.

**The version split, witnessed on PHP 7.4 through 8.5.** The root rule holds for
**methods**, instance and `static`, on every minor 7.4 to 8.5. For **properties**
it exists only from PHP 8.4, whose `zend_object_handlers.c` checks
`property_info->prototype->ce`: 7.4 to 8.3 check `property_info->ce`, the
redeclaring class, and fatal on the same read (instance, typed, untyped, promoted,
`readonly` and `static` properties alike; 8.4 and later allow it).

**The rule.** In the already-enumerated member chain, start at the node that
declares the member the site reaches and walk up:

- a node that declares a **non-private** member of that name becomes the new
  root, and a node that declares nothing is walked through (methods match
  case-insensitively, instance and `static`; properties are instance properties
  matched exactly);
- a **private** declaration ends the walk, because a private ancestor member is a
  different member that the nearer one does not override. `class Base { private
  h() } class A extends Base { protected h() }` keeps `A` as the root, and
  `(new A)->h()` from a sibling `B` still raises `Call to protected method A::h()
  from scope B` (witnessed);
- a **constructor's** prototype link is conditional and transitive
  (`zend_inheritance.c`: the parent's prototype, else the parent, is linked only
  when that is abstract). The ordinary walk finds the topmost non-private
  constructor `top`: if it is `abstract` the root is `top`, and otherwise the root is
  the declaring class itself. An abstract constructor can only sit at the top of a
  declaration line, so this is exact. It keys on the method being `__construct`,
  which covers `$a->__construct()` as well as `new A`.

`protected_invisible` then runs, unchanged, against the root. It stays bidirectional
and keeps its definite-`No` discipline.

**The property gate.** A property is judged against the root unless the declared
PHP target puts the whole analysed interval below 8.4 (`ceiling < 8.4`), where it is
judged against the redeclaring class as before. An undeclared, open or straddling
target takes the root, which only reports less (the same direction as ADR-0049
A12/A22's boundary-straddling declines). A method takes the root on every target.

**The exception: class constants.** PHP fatals on a `protected` constant redeclared in
`A` and fetched as `A::K` from a sibling `B` (`Cannot access protected constant A::K`)
on every supported minor 7.4 to 8.5 (witnessed), so `class-const.inaccessible` keeps
judging against the declaring class and its finding stands.

**What this does not touch.** The reach is unchanged: only exact receivers and named
classes are subjects, so no new site is claimed. No static-property fetch (`A::$sp`)
has an inaccessibility id; the rule would apply there from 8.4 if one is added. The
change only removes findings: a protected member whose root is a common ancestor of
the site's scope.

## Amendment (2026-10-03): the never-bound guard reads its own condition, a terminating `if`, and a disjunction, by polarity (issue #929)

**Status: PENDING ratification.** `variable.undefined` (#194) shielded a
read only under a bare `isset`/`empty` (optionally negated) that
*enclosed* it: the arms of `isset($x) ? … : …`, the body of
`if (empty($x)) { … }`. A name that is never bound makes every such test
constant (`isset` false, `empty` true), so a read that only the other
outcome reaches is dead code, and PHP runs it without the warning. Three
shapes stood in front of such a read without enclosing it, and the id
reported all three.

The enclosing-arm rule is unchanged, either polarity and containment
rather than reachability. The three rules below are different: each asks
**which outcome of the condition reaches the read**, and shields a name
only where that outcome proves it bound. `bound_when(cond, outcome)`
(`lower_scope.rs`) is the presence pass's `guard_bound_names` polarity:

- `isset($x)` proves `$x` bound when true, and `empty($x)` when false.
  `!` flips the outcome. An offset or property chain proves its root
  (`isset($x['a'])` proves `$x`).
- `&&` when true and `||` when false hold both operands, so their names
  add.
- `&&` when false and `||` when true hold *either* operand, so one
  operand's names prove nothing alone. A name is then shielded only when
  **every** operand proves something, and only while every name any
  operand proves is never bound (`Shield::joint`, decided by
  `VarUsage::settle` once the scope's bindings are known; a read under
  several disjunctions is discharged by any one of them). "Never bound"
  is the scope's syntactic binding set **widened** by what that set omits:
  the root of every function-call argument (a callee can bind it by
  reference: `preg_match($p, $s, $y)`, `parse_str($q, $y['k'])`) and the
  names PHP supplies (the superglobals, `$GLOBALS`, `$this`). This is the one
  place the disjunction rule survives the move to polarity, and it is
  needed there: `if (isset($x) || isset($y)) { echo $x; }` runs its body
  when `$y` is bound, with `$x` unbound. An operand that proves nothing
  (`$y > 0`, `empty($y)`) can hold on its own and voids the whole.

The rules:

1. **A short-circuit operand.** The right operand of `&&`/`and` takes
   `bound_when(lhs, true)`, and of `||`/`or` takes `bound_when(lhs,
   false)`. `isset($x) && $x > 1`, `!isset($x) || print($x)` and
   `empty($x) || print($x)` are silent.
2. **A compound condition's arms.** The then-arm of an `if` or `?:` takes
   `bound_when(cond, true)` and the `elseif`/`else` clauses and the
   else-arm take `bound_when(cond, false)`, in addition to the bare tests
   above. `isset($x) && $c ? $x : null` and the three disjunction shapes
   of the issue (`isset($x) || isset($y)` as the condition) are silent.
3. **A terminating `if`.** An `if` with no `elseif` and no `else`, whose
   body provably terminates (`BodyEnd::provably_terminates`, so a `try`, a
   `goto` or a `switch` that cannot be structured never counts), shields
   the statements after it in the same statement list with
   `bound_when(cond, false)`, the only outcome that reaches them:
   `if (!isset($x)) { return; } print($x);`. The shield stops at the end
   of the list and does not reach a statement before the `if`.

**The `goto` exception.** "Only this outcome reaches here" holds for a
fall-through and not for a jump: a forward `goto` lands after a guard, or
inside an `if` body, without evaluating the condition. A scope that holds
any `goto` or label therefore takes neither rule 3 nor the sided arms of
rule 2 for an `if` (a `?:` arm cannot be jumped into and keeps its side).
Rule 1 needs no exception: its right operand is reached only by evaluating
the left one.

**What stays reported**, each witnessed to warn on PHP 8.5.11 with `$x`
never bound: `isset($x) || print($x)`, `!isset($x) && print($x)`,
`if (isset($x)) { return 1; } return $x;`,
`if (!isset($x) && $c) { return; } print($x);`, the else-arm of
`isset($x) && $c ? null : $x`, `if (isset($x)) {} echo $x;`, and a
disjunction one of whose names is bound.

**The presence pass, accurately.** `variable.maybe-undefined` judges each
unit against the flowing state, and `presence_leaf` shields a unit by every
name an `isset`/`empty` anywhere in it tests (`collect_presence_shield`).
That subsumes the shields `scan_var_usage` now raises inside a unit, so the
pass's own judgments do not move. What does move is its gate: the pass
runs only when the definite pass's `reads` hold a read of a *bound* name
(`has_presence_candidate`), and a read withheld by these shields is no
longer in `reads`. A scope whose only such candidate sits in a shielded
region therefore no longer runs the pass, which can withhold a
`variable.maybe-undefined` finding the pass would have made (for example
`unset($y); echo $y;` after `if (!isset($y)) { return; }`). The same was
already true of the bare `if`/`?:` shield; the corpus A/B shows no movement.
