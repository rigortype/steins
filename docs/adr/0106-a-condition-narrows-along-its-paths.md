# A condition narrows along its paths, and a stored condition is a condition

**Status: proposed (2026-10-23), PENDING ratification.** Designed by the
architect for the guard-and-narrowing run (issue #1086; decisions D1 to D5 taken
under the owner's standing delegation). Lands slice by slice: A1 to A5, B1, B2,
C and D, each named in the section it implements. This ADR is docs only and
lands first; the slices do not edit `docs/adr/`. It retracts one sentence of
ADR-0052 §6, states ADR-0052 §2's positive class rule for the arms
`class_covers` left out, and generalizes the stored guards of ADR-0104; the
pointer notes of the same date are on those ADRs, on ADR-0062 (Amendment N) and
on ADR-0088.

**Amended (2026-10-27) by run #1114**, the twelve follow-ups this run filed. The
paragraphs headed "Designed in run #1114" below record the design before the code
lands; each names its slice and issue, and the slice that lands it does not edit
`docs/adr/`. They are PENDING ratification, like the rest of this ADR.

## 1. Context

PHP honours a family of guard spellings that Steins does not narrow on. Most of
them are strict-floor (possibly-grade) false positives; one (#908) is a false
`never` in the dump. They share three seams and four local holes.

**S1, path polarity.** Nine collectors turn a condition into narrowings: type
predicates (`collect_type_guards`), null and literal comparisons
(`collect_refine`), `instanceof` (`collect_instanceof`), enum identity
(`collect_enum_identity`), shape presence (`collect_shape_guards`), same-expression
call guards (`collect_same_expr_call_guards`), out-parameter seeds
(`collect_guard_calls`), stored guard variables (`collect_guard_vars`) and
remembered call results. All nine distribute `And` on the then side and `Or` on
the else side, and flip at `Not`. None contributes anything for `Or` on the then
side or `And` on the else side. `is_int($n) || is_float($n)` therefore narrows
nothing inside its branch, and neither does `if (!is_int($n) && !is_float($n))
return;` on the fall-through (#1083). ADR-0052 §6 says refinement collection "is
already polarity-correct and needs no change". The collectors are polarity-safe
(they never narrow wrongly), not polarity-complete (they omit what a disjunction
proves), and the premise of §6 is retracted here.

**S2, four application sites with drifted vocabularies.** The collected
narrowings are applied at `apply_cond_side` (`if`, `elseif`, `while`: the full
set), `threaded_operand_env` (the right operand of `&&` and `||` and the ternary
arms: type and scalar refinements only), the `assert` statement (type, scalar,
class, shape and remembered), and `remembered::produce` (scalar and class, on a
synthetic store). Each re-lists the vocabularies by hand. The operand site omits
the arm-lane and shape narrowings, so `$n !== null && method_exists($n, 'gone')`
keeps the null arm, the member-guard fold sees a non-class arm and answers
`Maybe`, and the dead body reports (#984).

**S3, a stored condition is not yet a condition.** ADR-0104 stores a guard in
`Store::guard_vars`, but `guard_cond_of` admits only existence calls, member
guards, their negation and conjunctions, and the consumer only re-vouches; it
never narrows. `$has = $v !== null; if ($has)` stores nothing. `isset($v)` has a
`CondExpr::IssetVar` form that refines nothing, and `($a['k'] ?? null) !== null`
never becomes the `isset($a['k'])` it denotes (#933, #1079).

**The local holes.** A loose `switch` returns before refining its subject
(`refine_match_arm`, #1041). `class_covers` answers `Yes` under a positive guard
for an arm it knows nothing about, so a native intersection dumps `*NEVER*`
(#908). The dump ranks the guard's class above a more specific carrier (#909).
`method_exists` narrows no arm of a union receiver (#981).

The facts the rules below rest on were measured on PHP 8.5.11:

| fact | PHP 8.5.11 |
| --- | --- |
| `1 == "1"`, `1 == "1.0"`, `1 == " 1"`, `"1" == "01"`, `"0.0" == 0`, `" 0" == 0` | true |
| `1 == "1abc"`, `"abc" == 0` | false |
| `null == 0`, `null == false`, `false == 0`, `true == 1`, `1.0 == 1` | true |
| `null == 1`, `NAN == NAN` | false |
| `switch (1.0) { case 1: }`, `switch (false) { case 0: }`, `switch (1) { case "1": }`, `switch (null) { case 0: }` | case taken |
| `switch (1.5) { case 1: }` | default |
| `null == []`, `false == []`, `[] == []` | true |
| `0 == []`, `'' == []`, `[0] == []`, `[null] == []` | false |
| `empty("0")`, `empty("0.0")`, `empty([0])` | true, false, false |
| `method_exists($o, 'priv')` for a private method | true |
| `method_exists($o, 'anything')` with `__call` | false |
| `method_exists(null, 'x')` | TypeError |
| `is_callable([$o, 'anything'])` with `__call` | true |
| `is_callable([$o, 'priv'])` from outside the class | false |

The `*NEVER*` witness: `function f(Foo&Bar $x) { if ($x instanceof Foo) {
dumpType($x); } }` dumps `*NEVER*`, and so does `(Foo&Bar)|int $x` declared
natively; PHP reaches the body with a `Foo&Bar` object. The phpdoc spellings and
`callable $x` do not show it, because the Asserted lane is dropped and the
`Member` rung answers `Foo`; for them the deleted arm is a missed narrowing, not
a false `never`.

## 2. Decision

### 2.1 Path semantics (A3, #1083)

A branch entered through `a || b` is the join of the path entered through `a` and
the path entered through `!a && b`. PHP sequences it exactly so. Symmetrically, a
branch entered on the false side of `a && b` is the join of the path through
`!a` and the path through `a && !b`. The rule is:

- At the requested polarity, a condition expands into **paths**. `Or(a, b)` at
  then gives `[a@then]` and `[a@else, b@then]`; `And(a, b)` at else gives
  `[a@else]` and `[a@then, b@else]`; `Not` flips the polarity; `And` at then and
  `Or` at else stay one path, as today.
- Each path is rebuilt as a synthetic `And` chain of its atoms, `Not`-wrapped to
  carry polarity, and the site's existing vocabulary runs once over that chain on
  a clone of `(env, store)`. The clones are merged with `join_envs`, the merge
  every `if` already ends with.
- Applying a path as one synthetic conjunction, not atom by atom, keeps the
  within-path order the vocabularies depend on (`is_string($v) && $v !== ''`
  mints `string` first and `non-empty-string` second) and adds only the join
  across paths.
- A condition with no `Or` at then and no `And` at else is passed through
  **unchanged**, so the overwhelmingly common case is byte-identical.
- Paths are capped at **8** (D2). Past the cap the node contributes nothing, and
  the cutoff names itself in the dump provenance ("paths capped"). The cap is a
  structural bound on a widening, never a clock (invariants). How often it binds
  is measured on the public corpus.

The join is the operation, and ADR-0052 §4's refusal of a `union(A, B)` stays
upheld: there is no per-carrier union, joins stay the value domain's job. A fact
that one path leaves unconstrained joins away on the other, so `if ($c || $x ===
"abc") { width($x); }` over an untyped `$x` still narrows nothing.

**Soundness.** `if (is_int($n) || is_string($n)) return number_format($n);` keeps
the `string` arm through the join and keeps reporting; `if (is_int($n) && $n > 0)
return ''; return $n;` over `int|string|null` joins `string|null` with
`int<min, 0>` and keeps the `null` finding.

**Measured residue, accepted (D5).** `if ($x === null || $x->m())` over `?Foo`
dumps `unknown` in the then branch, where it dumped the declaration `Foo|null`.
Path 1 (`Exact null`) unbinds the lane, `join_stores` drops a lane absent on one
side, and `lane_claim` cannot spell a class arm as a fact. No finding moves, and
the same weakness already governs every post-`if` join (`if ($x === null) {}`
followed by a dump). The proper fix is in the join, not the driver: a value fact
the arm vocabulary can spell joins into the lane as a literal arm. It is its own
classified-diff slice and is filed as a follow-up unless A3's public diff shows
more than about five dump or finding losses (D5).

### 2.2 One driver, every site (A2 #984, A3 #1083)

The path driver, `narrow_paths`, is the one entry for every application site:
`if`, `elseif` and `while` through `apply_cond_side`; the `&&` and `||` operand
and the ternary arms through `threaded_operand_env`; and `assert`. Each site
passes its own vocabulary closure and the driver owns the expansion and the join.

A2 comes first and closes the vocabulary drift on its own: the operand
environment gains `apply_class_narrowing` and `apply_shape_narrowing`, so the
operand lane sees the same arm-lane and shape narrowings the branch lane does.
After it, `$n !== null && method_exists($n, 'gone')` folds `No` (the surviving
arms are all classes without `gone`), the body is dead, and the finding goes.
`$n !== null && method_exists($n, 'go')` followed by `$n->gone()` over a class
declaring `go` but not `gone` keeps reporting. The operand and ternary positions
can now mark an operand dead where the arm lane proves it; each such sharpening is
classified in the slice's diff.

**Designed in run #1114, slice S8 (#1096); superseded by #1115, not landed.** The
design below was built and found to change no verdict. A call or an offset inside a
`&&`/`||` operand or a ternary arm sits at a `Conditional` read position, which the
method and offset families decline (D3, the carrier table in `stmt_checks.rs`), and it
is not one of the statement's own calls, so it never reads the operand environment the
unified vocabulary would build. The rows below are therefore #1115's acceptance rows,
and the per-position operand environments of #1115 must decide how they meet the D3
decline (decision 8 of #1114). The original design, kept for #1115: A2
closed the drift for two vocabulary entries, and the operand site still runs only
four (type, scalar refinement, class, shape) while the branch site runs the full
list of `apply_branch_vocabularies`: the same-expression guards, the
`-if-true`/`-if-false` assert envelopes, the existence and stored-guard vouches,
`seed_out_params`, `seed_produced_places` and `remembered::produce`. So `isFoo($o)
&& $o->other()` and `isFoo($o) ? $o->other() : 0` stay silent where `if (isFoo($o))
{ return $o->other(); }` reports, and `preg_match('/(x)?y/', $s, $m) === 1 &&
strlen($m[1]) > 0` misses the `offset.maybe-missing` the `if` spelling reports.
The decision is one vocabulary function, `apply_vocabularies`, owned by
`narrow.rs`: the branch site, the operand and ternary site (through
`threaded_operand_env`) and the `assert` statement all pass it to the path driver.
The envelopes apply at `Asserted` whatever the site. The operand site keeps
`cond_invalidations` after the driver (the branch site masks stale conjuncts
instead) and drops its own `remembered::produce` call, which the list now
contains. Cost: the full list on every right operand, cheap scans over the two
`(env, store)` clones that already exist. The rows that must stay silent are
`isFoo($o) && $o->fooOnly()` and the required-group `preg_match` operand.

**Designed in run #1114, slice S9 (#1098); landed when that slice merges.** A
value-position `&&` or `||` (`$r = $n !== null && method_exists($n, 'gone') &&
f();`, a `return`, a ternary condition) is not a condition today:
`eval_logical_fact` evaluates the right operand under the entry env, so no left
operand narrows it and no existence guard folds, and a dead mark is written after
the positions were judged. The decision (option B): the value-position `&&`/`||`
is lowered as the condition the `if` would lower (`ArgValue::Logical` carries a
`CondExpr`, as `ArgValue::Ternary` already does) and evaluated through
`eval_cond`, which threads the operand env, folds the existence guards and marks
dead operands. `xor` keeps the truthiness path. Because positions are judged
before values are evaluated, a pre-step evaluates the statement's value-position
conditions for dead-marking only; `eval_cond` is read-only, so the double
evaluation is safe. The trace payload gains a codec arm for `Logical`; no schema
bump. Per-position environments (option C: findings inside an operand read the
operand environment, which touches the lowering, `stmt_checks`, `nested` and the
D3 table) are **deferred** to #1115 and listed in §5.

### 2.3 Stored conditions (A5, #933 leg 2)

A variable assigned a condition holds that condition in `Store::guard_vars`, and
a test of the variable applies it. This generalizes ADR-0104's existence-guard
store; its invalidation is unchanged.

- **What is stored.** The `ArgValue` of the assigned expression converts to the
  `CondExpr` it denotes: a `Binary` comparison, a call of a type predicate or
  existence guard, `isset`, `Not`, `Logical` (`&&`, `||`) and a bare variable. The
  dependencies are every bare variable the condition reads.
- **What is declined.** A property fetch, an offset read on an object, and any
  other call: none is a pure function of local variables.
- **Invalidation.** Rebinding the stored name or any dependency drops it
  (`release`), as does a barrier; a join keeps it only where every joined path
  holds the same guard; a poisoned scope (`global`, a reference, `extract`, a
  by-reference capture) never stores it. All as ADR-0104 §2.7.
- **The verdict is taken under the current env**, not under an empty one.
  `$has = $v !== null; if ($v === null) return; if ($has) …` decides `Yes` and
  prunes the else side. This is monotone: the stored condition is the same
  function of the same values, and the current env only knows more about them.
- **Test.** A tested stored condition applies its narrowing through the path
  driver at the tested polarity, in addition to vouching what its existence
  guards vouch. A stored disjunction therefore narrows by §2.1 the day both land:
  `$ok = is_int($n) || is_float($n); if ($ok) return number_format($n);`.

True positives stay: `$has = $v !== null; $v = null; if ($has) return $v;` (the
dependency was rebound), `$has = $v !== null || $z; if ($has) return $v;` (path 2
leaves `$v` nullable and the join keeps `float|null`), and `$has = …; $has =
true; if ($has)` (`release($has)`) all throw in PHP and keep reporting.

**Designed in run #1114, slice S3 (#1110); landed when that slice merges.** The
two-sided fall-through rule restores each untested name through
`restore_branch_scoped`, whose "rebound itself" test is survival in the side's env
or contract. A by-reference call forgets the name, which passes the test, so the
pre-test value (`'abc'`) comes back after both sides rewrote it. The hand-back
rule reads a **syntactic write set per side** instead, computed in steins-infer
over the trace (`side_writes.rs`): assignments, offset writes, appends and
unsets, destructure targets, loop and `try` write sets, opaque writes (a poisoning
construct writes everything), recursion into `if`/`match`/`try`, and every
invalidated name that is not a provable by-value site (the predicate the
conjunct-writes pass already uses). At the join a name goes back only if neither
side's set holds it. The test is not the survival of the suspect. The same rule
may serve the presence-guard put-back, which has the same weakness
(`if (isset($a['k'])) { fill($a); }`), with its own witness. Reads and by-value
calls keep the hand-back, so the read-only sides keep their findings.

### 2.4 The isset equivalences (B1, C; #933 legs 1 and 3, #1079)

- `isset($v)` at then is `$v !== null`: the value lane gets `Refine::NotNull` and
  the arm lane subtracts `Null`. At else it narrows nothing, since an unset or a
  null binding both falsify it. `if (isset($v)) return ''; return $v;` keeps
  reporting.
- `(X ?? null)` compared with `null` by `===` or `!==`, in either operand order,
  is `isset(X)` at lowering (`lower_binary_cond`): `X` a constant-key offset gives
  `Isset{var, key}`, a bare variable gives `IssetVar`, `===` wraps in `Not`. Only
  a `null` default lowers: `($a['k'] ?? 0) !== null` proves nothing and keeps its
  finding.
- The isset flavour of presence strips `null` from the key's value, so `array{k?:
  ?int}` read after `($a['k'] ?? null) === null` returns early is closed too. This
  was a master false positive, not only a gap.
- A guard-position `isset` still decides nothing except over a proven value
  (ADR-0062 Amendment H1 is kept): deciding reachability from an Asserted shape
  would let a docblock silence a live path.

### 2.5 Emptiness on shapes (C, #1079)

- `$a !== []` and `$a != []` at then, and `$a === []` and `$a == []` at else,
  mean non-empty: `Exclude(v, [])`, which `exclude_member` implements for
  `Fact::Shape` as `set_non_empty`. Truthiness (`if ($a)`) already reaches the
  same call.
- The loose forms also add `NotNull(v)`: `null == []` is true, so `$a != []`
  proves a nullable array is not null. `false == []` is true as well, and the
  rule adds nothing for it: `false` stays in the fact, which is sound (the branch
  keeps a value PHP may still hold there) and only less precise. `0 == []` and
  `[0] == []` are false, so nothing else needs excluding.
- A shape that can never be non-empty (`array{}`) is left as declared, since
  `set_non_empty` would build an empty `non-empty-array{}`. The branch is one
  the `Asserted` declaration says PHP never enters, and a dead region on that
  premise would silence a live path, so reads there are judged against
  `array{}` as before (the loose form's `NotNull` still drops a `null` arm).
- In the shape normalizer, **a sealed shape that is non-empty counts as having
  a count floor of one** for the existing exact-count pin, which then promotes a
  single optional key to required. This is the rule `count($a) > 0` already
  satisfies, which is why it is silent today. `normalize_counted` reads that
  floor without storing it in `count_bound`: storing it gave one shape two
  forms (with and without the floor, depending on how its non-emptiness was
  established), which broke the canonical form equality relies on.
- Not promoted: an open tail (`array{0?: int, ...}`) and two or more optional keys
  (`array{0?: int, 1?: int}` is non-empty for `[1 => 2]`, which has no key 0).
- **A non-empty list holds key 0** (the list-head pin, added with B2): a list's
  keys are `0..n-1`, so a non-empty shape whose `is_list` is `Yes` has its
  declared optional key 0 made required, open tail or not. It closes a false
  positive B2 exposed: filling an empty list (`if (empty($xs)) { $xs[] = 'd'; }`)
  and joining it back left `non-empty-list{0?: string, ...<string>}`, and the
  read of `$xs[0]` after the `if` reported (the same held for `!$xs`,
  `$xs === []`, and a literal joined into a declared `non-empty-list<T>`). Only a
  declared field moves: a key 0 the tail alone supplies is not added as one, so
  `non-empty-list<T>` keeps its tail-only form and the rule maps
  `non-empty-list{0?: T, ...<T>}` onto the one form `non-empty-list{0: T,
  ...<T>}` without giving any shape a second spelling. Like the floor above, a
  count the flag contradicts keeps its own floor and promotes nothing. The pin
  trusts `is_list` as given, so a write at a literal key drops the verdict before
  it promotes the key, unless the write overwrites a required key or lands where
  a list cannot gap: `$a[5] = 9` on `list{0?: int}` may leave `[5 => 9]`.

### 2.6 Loose `switch` refinement (A1, #1041)

In a loose `switch`, the subject is refined inside a case when its fact is
int-based (`General` or `Refined` int, or a finite set of ints) and every label
is an int literal. `==` then coincides with `===`, and the arm's fact is the
labels (`Fact::from_vals`) intersected with the subject's, so a refuted label
leaves the empty domain. A bool subject with bool labels refines the same way.

Two additions keep it sound against the table in §1. A **nullable** subject with
a `0` label adds `Null` to the set (`null == 0`); any other label excludes null.
A `?bool` subject with a `false` label adds `Null` (`null == false`). So `?int`
under `case 0:` dumps `0|null`, and `if ($k === 0)` inside it stays undecided and
keeps its true positive.

Declined: string subjects (`"1" == "01"`), float subjects (`1.0 == 1`, NaN),
mixed-kind labels (`case 1: case "a":`), and string labels on an int subject
(sound, but `1 == " 1"` makes the gain not worth the table). The no-match path
rule (ADR-0052's note on #439) is unchanged: a `switch` residue still proves
silence and may not buy a finding.

**Designed in run #1114, slice S1 (#1090); landed in #1117.** The
arm rebinding of a `match` follows the same table. The loose arm's fact is the
labels intersected with the subject's fact, so it depends on the subject and
inherits its `thinned` flag, line and closure (#1088). The strict arm's fact is
exactly the labels, established by the runtime `===`, so it stays `Verified`
whatever the subject's stratum (decision 7 of #1114, a deliberate deviation from
the filed checkbox). What the strict arm used to drop is `thinned`: it was built
as `Known::value`, so a subject whose fact had been thinned by a guard came back
un-thinned and the arm's argument check proved a mismatch the guard's exit had
made unreachable. The arm now keeps `thinned` and `line`, replaces `fact` and
`bound`, and resets `closure`, and `check_oneof_argument` demotes to the possibly
grade as it does for the loose arm. The closure is reset because a closure is
never `===` a literal label, so an arm reached through one is dead at runtime;
keeping it let `match ($f) { 1 => $f("a"), default => 0 }` over
`$f = fn(int $x): int => $x` prove a mismatch PHP never reaches (#1117's review).
A strict `match` over an Asserted `@param 1|2 $k` with `1 => takes_string($k)`
keeps its proven `type.argument-mismatch`.

### 2.7 `method_exists` narrows the receiver lane (A4, #981; D3)

On the then side, an arm whose class hierarchy provably lacks the method is
deleted: the hierarchy is fully enumerated, no ancestor declares it, and the
traits composed per ADR-0105 do not. On the else side, an arm whose class, an
ancestor, or a composed trait declares the method is deleted, since every
descendant then has it. Both are visibility-blind and ignore `__call`, which is
what `method_exists` does (§1). A non-final class arm with unknown descendants
without the method is kept on the then side: it is not provably lacking.

`is_callable([$v, 'm'])` is **declined** (D3). With `__call` it is true for any
name, and from outside the class it is false for a private method, so it cannot
share the rule. It is left open (§5).

The else side turns a currently silent call into a true positive: `if
(method_exists($x, 'go')) return ''; return $x->go();` over `N|M`, where `M` lacks
`go`, narrows to `M` and reports `call.undefined-method`. PHP throws `Error`.
That is an expected addition, listed in the slice's diff.

**Landed (2026-10-27, #981) on the declared-receiver lane's per-arm verdict.**
The rule above is the arm verdict of the guard-aware lane (`judge_arm`, A4a/A4b),
asked with no magic fallback: on the then side an arm judged `Absent` or `Dead` is
deleted, on the else side one judged `Declared` or `Dead`. `Unknown` (an open
hierarchy, a descendant that declares the method, an obstacle on a chain), an arm
the lane cannot read (`null`, a scalar, a class-string), and a verdict that rests
on a class only a `@phpstan-assert` excluded keep the arm on both sides. Strata
are kept, and an all-`Verified` lane emptied this way is ADR-0052 §2's emptied
carrier. The narrowing is one more entry of the arm vocabulary, so the branch, the
`&&`/`||` operand, the path driver, `assert()` and stored conditions all apply it.
The fold stays No/Maybe; it never answers Yes.

The walk counts an interface's methods for an abstract class or an interface,
its own or reached through `implements` and every parent of an interface (#1103):
before, the literal fold `method_exists(Abs::class, 'm')` answered a wrong No
there. An interface the walk cannot enumerate (a builtin, a vendor name) leaves
such a node `Unknown`. A concrete class implements every interface method on its
own chain, so a walk from one, and the call lanes, keep walking `extends` alone.
An abstract class or an interface below the anchor is no candidate of the
`method_exists` ladder: it has no instances, and its concrete descendants are
candidates in their own right.

**Designed in run #1114, slice S11 (#1112); landed when that slice merges.** The
chain walk returns `Unknown` at a node `find_class` cannot resolve (the start
class, a parent, an interface), with "awaiting the reflect method surface" as the
reason. With the sidecar that surface exists: `Folder::reflected_class` returns the
class with every method, inherited included. The decision (owner decision 2 of
#1114): **every** unresolved builtin node is answered by reflection, not only
builtin interfaces. A reflected node that has the method is `Declared`; one that
lacks it ends the walk absent, since a reflected class already lists what it
inherits. The node is recorded apart from the project chain (the verdict still
requires every project chain FQN to be a non-boot-surface class), and a node
whose reflection is unavailable, or no sidecar, stays `Unknown` as today. So
`method_exists(AbsB::class, 'nope')` over an `AbsB implements \Countable` folds
No (the body is dead and the strict-floor finding in it goes), `'count'` folds
Yes, and a version guard on a builtin class folds on the project's PHP. The
change is invisible under `--no-php` (byte-identical) and can add and remove
findings in sidecar runs, which is why the slice measures with a sidecar A/B.

### 2.8 Class arms under a positive guard (D, #908, #909; D4)

ADR-0052 §2's positive class rule says an arm dies only when it is final, or an
enum, and is-a is `No`. `class_covers` implemented it for `Class`, `EnumCase`,
`ObjectAny`, `Opaque` and `Mixed`, and answered `Yes` for everything else, which
deleted arms the rule never allowed it to. The corrected positive answers are:

| arm | answer | reason |
| --- | --- | --- |
| intersection | `Yes` only if some member is a final class (or an enum case) with is-a `No`; otherwise `Maybe` | a subclass of `Foo&Bar` can be a `Foo` |
| `callable` | `Maybe` | a `Closure` or an invokable `Foo` |
| non-empty-mixed | `Maybe` | any object that is truthy |
| union | `Maybe` | decided member-wise by the caller |

The dump side (D4, kept off the source so the `Member` fact stays true and the
slice stays parallel): rung 2b, a single-`yes` `Member`, **yields** when the heap
class or every surviving lane arm is-a that member, and rungs 2c (the heap lower
bound) and 3 (the arm list) render the most specific carrier. `Suit $s` under
`instanceof \UnitEnum` dumps `Suit` inside and after; `Dog $d` under
`instanceof Animal` dumps `Dog` in all three places, including the dead else;
`Dog|int $x` dumps `Dog`. When nothing is more specific, `mixed $x` under
`instanceof Foo` still renders the `Member`, `Foo`. This is a rendering rule
inside this ADR; ADR-0053 is not amended.

An intersection arm renders `unknown` even unnarrowed. After the fix the
then-branch of `Foo&Bar $x` dumps `Foo` (the `Member` rung), and the acceptance
is "not `*NEVER*`".

### 2.9 Bare `empty($v)` is `!$v` (B2, #1079; D1)

**Decided into the run, last on its lane, deferred if its classified diff is
large.** `empty($v)` over a bare variable lowers today to an opaque test that
reads `$v`, so both sides forget `$v`: silence by forgetting, a precision gap and
not a false positive. The rule is to lower it as `Not(Truthy(Var))`, which:

- stops forgetting `$v` on both sides, so a finding inside an `if (empty($row))`
  body reads a variable that is now proven;
- narrows `!empty($s)` over a string to `non-falsy-string`, and `!empty($a)` over
  `array{0?: int}` through §2.5 (so B2 lands after C);
- on the side where `empty($v)` holds narrows as `!$v` does and no further: an
  array becomes `array{}` (a falsy array is `[]`), and a scalar keeps its domain,
  since "falsy" is not representable (`empty("0")` is true, `empty([0])` false).

**Landed (2026-10-27, #1079).** The public-corpus diff is empty in both the
strict and the default profile, though about 39 bare `empty($var)` guards in the
corpus now lower this way. The expected true-positive class (`if (empty($row))
{ return $row['x']; }` over `array{x?: int}`) does not appear: a read on an
`array{}` reached by narrowing is silent, for `!$row` too, while a declared
`array{}` reports `offset.undeclared`. That gap predates the run and is left
open in §5.

**Designed in run #1114, slice S7 (#1105); landed when that slice merges.** The
falsy side of a guard on a non-nullable shape bound
`Fact::Singleton(Val::Array([]))` at the shape's `Asserted` stratum. The proof
emitter reads a variable base only at `Verified`, and the strict leg resolves
only a `Fact::Shape`, so the read stayed silent while the declared `array{}`
reported. The decision: that side binds the sealed shape with no fields and
`non_empty: false`, the `array{}` this ADR already names, at the shape's stratum,
instead of the value-lane singleton. A read of an absent key then answers
`DeclaredAbsent` and reports **`offset.undeclared`** at the contracts floor, the
id the declared `array{}` reports (owner decision 3 of #1114). It does not report
`offset.missing`: that is the proof-grade id, and a native-only `array $row` has
no fact at all, because native array-ness is not in the value lane. The grade
stays open (§5). A nullable base (`?array` under `!$row`) still binds nothing, and
a write to the key promotes the shape as before; the list-head and `set_non_empty`
pins of §2.5 are unchanged. `$row === []` keeps reporting `offset.missing` with
the sidecar.

## 3. Consequences

On the fp-gate, the Carbon possibly row for `Traits/Timestamp.php:172` (an
untyped `$numbers` under `if (\is_int($numbers) || \is_float($numbers))`) drops,
760 to 759, with A3, and the known-false-positive sentence in its note goes.
Everything else is classified per slice. A narrowing change is never accepted as
byte-identical: removals are the expected direction (strict-floor false positives
leave), and every addition must be a true positive, with these named as expected:
the else-side `call.undefined-method` of §2.7 and dead-operand marks the operand
site now earns (§2.2); B2 added none (see §2.9). D's finding
level is the exception that is expected to be byte-identical, with only
`debug.type` lines moving.

The path driver costs two clones of `(env, store)` and one join per disjunctive
node at the branch's polarity, bounded by the cap of 8. Its wall time is measured
against the fp-gate.

The residue of §2.1 (a dump, not a finding) is accepted and filed.

## 4. Verification

Each slice turns the witness table of its issue into tests: the silenced shapes
go quiet, and the true positives and the declined shapes stay as regression pins.
The kept rows that matter are: `if (is_int($n) || is_string($n))` and `is_int($n)
&& $n > 0` keep their findings (§2.1); the operand and ternary spellings of
`number_format` stay silent as they are now; `?int` under `case 0:` keeps the
`=== 0` true positive, and `string`, `float` and mixed-label switches stay
unrefined (§2.6); `tp_stored_rebound`, `tp_stored_or` and `tp_stored_reassigned`
(§2.3); `isset` else-side (§2.4); two optional keys and an open tail under
`!== []` (§2.5); the callable and `is_callable` declines and the non-final
unknown-descendant keep (§2.7); a final member refuting an intersection arm
(§2.8). The `short_circuit.rs` file and the `type_predicate_guards.rs` `$c ||
is_string($v)` else-side case pass unchanged.

Each slice runs the public-corpus A/B (strict, `--no-php`, vendor diagnostics, no
cache) against a base built from its merge base, classifying every difference.
The private corpus runs under the lock discipline before merge.

## 5. Left open

- **The join.** A value fact the arm vocabulary can spell should join into the
  lane as a literal arm (`val_contract`). It fixes the §2.1 residue and every
  post-`if` merge; its own slice, filed on D5.
- **`is_callable` narrowing** (§2.7, D3): declined with the measurement above.
- **`method_exists` folding Yes** (§2.7): a lane every live arm of which is
  `Declared` would prove the else side dead; it is narrowed to an empty lane
  instead, and the region stays walked.
- **Loop conditions keep no `method_exists` narrowing** (§2.7): `while
  (!method_exists($x, 'go'))` forgets `$x` on both sides, on master too, while the
  `instanceof` twin narrows after the loop.
- **`non-empty-list<T>` key 0**: closed for a declared key 0 by the list-head pin
  (§2.5). A tail-only `non-empty-list<T>` gains no field, and a read of its key 0
  is judged against the tail as before.
- **Intersection arm rendering**: an unnarrowed `Foo&Bar` renders `unknown`.
- **Path-cap tuning** (D2): 8 is proposed; the measurement of how often it binds
  decides whether it moves.
- **Operand and ternary positions do not judge argument mismatch**: observed
  silent on master (`(is_int($n) || is_float($n)) ? number_format($n) : $n`), for a
  reason separate from the narrowing here, and kept silent.
- **A read on a narrowed `array{}`**: an `array{}` reached by narrowing (`!$v`,
  the true side of `empty($v)`) reports nothing for `$v['x']`, while a declared
  `array{}` reports `offset.undeclared` and `$v === []` reports `offset.missing`
  (§2.9). It predates this run.
- **Per-position environments** (run #1114, deferred to #1115): findings inside
  an operand or a ternary arm read the entry store, because a conditional
  position is judged at `Reach::Existence` and the value-dependent families
  decline it (ADR-0104). Judging a position under the operand environment of its
  guard region means linking each position to its region and polarity at
  lowering, and touches `stmt_checks`, `nested` and the D3 table. S8 and S9 fix
  the verdict of the operand and the dead marks, not the findings inside it.
- **Proof-grade `offset.missing` for a native `array` parameter** (run #1114,
  decision 3): S7 lands `offset.undeclared` at the contracts floor. The proof id
  needs native array-ness in the value lane, which does not exist (a native-only
  `array $row` dumps `unknown`).
- **PHPStan's behaviour** on these spellings was not re-run for this ADR.
