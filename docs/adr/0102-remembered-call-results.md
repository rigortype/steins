# A call's result is remembered on its key until a site invalidates it: the value-lane half of "remembering and forgetting"

**Status: proposed (2026-10-03), PENDING ratification.** Designed by the
architect under the owner's standing delegation, as the separate ADR that
ADR-0101 §5.5 scopes; the owner's framing (2026-10-03): "`is_dir($dir)`'s
result stays deterministic until `clearstatcache()`, so it can be treated as a
literal type". ADR-0101 §5.4 supplies the invalidation set; this ADR owns the
key, the subject kind, the scope rules, the parity decision and the ADR-0002
calibration. Tracking issue to be filed with slice 1; D5 of ADR-0101's slice
list places it after S1–S3, which have landed (#1001, #1004, #1007).

**Owner decisions (2026-10-03).** D1 the stat family (`is_dir`, `file_exists`, `filesize`,
…) is never remembered, and the divergence from PHPStan is recorded (no configuration key
either way): PHP caches only a successful stat of the last path and `file_exists` never reads
the cache, so a remembered stat result would disagree with the engine. D2 nested calls are not
keyed in v1; a call key is built from local variables and literals only. D3 v2 adds project
callees first, then methods with receiver places. D4 the terms are **remembered call result**
and **call key**.

## 1. Context

### 1.1 What PHPStan does, read in its source

PHPStan's "Remembering and forgetting returned values" (2021) describes a
mechanism over *values*: a narrowed call expression is remembered, keyed by the
expression's printed text, and a later identical expression reads the
remembered type until something forgets it. Verified in phpstan-src 2.3.x
(local checkout at 0fe65c2b8):

- **The remember gate** is a function's `hasSideEffects` bit: a call is
  narrowable unless the bit is `yes`, and under
  `rememberPossiblyImpureFunctionValues: false` only when it is `no`
  (`src/Analyser/ExprHandler/FuncCallHandler.php:907-912`,
  `src/Analyser/TypeSpecifier.php:361-365`; the method twin at
  `ExprHandler/MethodCallHandler.php:638`).
- **Forgetting** happens at an assignment or `unset` of a variable the
  expression mentions (`MutatingScope.php:3485-3510`), and at an impure
  method call on the receiver (`MethodCallHandler.php:321, 356`); the post
  adds passing the object to an impure function. The path by which a void
  `clearstatcache()` forgets `is_dir($dir)` was not traced here (inferred
  from the post, not verified in the 2.3.x source).
- **The metadata** is hand-curated: `is_dir` and `file_exists` are
  `hasSideEffects: false` and are remembered; `getenv` is `true` and never is
  (`resources/functionMetadata.php:1283, 943, 1035`); `clearstatcache` returns
  `void` and is impure by the void rule.

### 1.2 What Steins has today

Narrowing acts on variables, offsets and properties (ADR-0052's three
carriers, env-carried as `Known { fact, stratum }` per name; ADR-0070's
by-value survival; `branch.rs`'s branch-scoped forgetting). A call already
reaches the condition lowering, twice: `CondExpr::Call` for a call in guard
position, retained so `@phpstan-assert-if-true` and the existence predicates
can answer, and `CondOperand::Other { call: Some(…) }` for a call that *is*
a comparison operand (`crates/steins-syntax/src/ast.rs`, the `CondExpr` and
`CondOperand` enums). Both are "unrepresentable for the verdict": a guard on
`strpos($h, '=') !== false` evaluates `Maybe` and narrows nothing, and a
second `strpos($h, '=')` inside the branch is answered by the transfer alone,
`int|false` again. The operand-kind reader files "a call result" under
silence (`operands.rs`). So nothing remembers a call's result, and the
concept is new machinery in the value lane.

What the effect lane now supplies, after ADR-0101 S1–S3: every site has one
resolved effect answer (ADR-0099) — a label set and a gap set — and that
answer already distinguishes a call whose result is a function of its
arguments (`{}`), one that also reads an ambient setting
(`global.read.setting.<cell>`), one whose trigger the site cannot see
(`value-dependent-read`, `project.rs`'s `Site`, `site.rs:233`), and one that
reads the world (`io*`) or a generator (`nondet.*`). That is the gate this
ADR reads instead of a purity bit.

### 1.3 Witnesses

`witnesses/remembered-results.php`, PHP 8.5.11. "same" means the first
result may stand for the second across the span; "DIFFERS" is a must-stay.

| row | span | verdict |
| --- | --- | --- |
| R1 | `setlocale(LC_ALL, '0')` twice, no write | same |
| R2 | the same across `setlocale(LC_NUMERIC, 'de_DE.UTF-8')` | DIFFERS |
| R3 | `strlen($s)` twice, `$s` untouched | same |
| R4 | `strlen($s)` across `$s .= '!'` | DIFFERS |
| R5 | `strlen($s)` across `byref($s)` (`string &$x`) | DIFFERS |
| R6 | `localeconv()` across `opaque($cb)` whose callback sets the locale | DIFFERS |
| R7 | `sprintf('%.2f', $x)` across `setlocale(LC_NUMERIC, …)` | DIFFERS |
| R8 | `sprintf('%05d', $n)` across the same write: no read to invalidate | same |
| R9 | `strpos($h, '=')` twice (the guard-then-use idiom) | same |
| R10 | `mt_rand()` twice after one `mt_srand(7)` | DIFFERS |
| R11 | `$o->count()` across `$o->append(3)` | DIFFERS |
| R12 | `getenv('X')` across `putenv('X=2')` | DIFFERS |
| R13a | `is_dir($d)` **false**, a child `mkdir`s it, `is_dir($d)` | DIFFERS |
| R13b | `is_dir($d)` **true**, a child `rmdir`s it, `is_dir($d)` | same (served from the stat cache) |
| R13c | `file_exists($d)` right after R13b's cached `true` | DIFFERS (`access(2)`, never cached) |

R13a is the row to read twice: PHPStan's own example — `if (is_dir($dir))
return; dumpType(is_dir($dir)) // false` — is wrong about the engine. php-src
drops a stat into the one-entry cache only on success
(`ext/standard/filestat.c`, "Drop into cache" after a successful `url_stat`),
so a remembered `false` is never served, and a remembered `true` is served
only until a stat of another path evicts it or any plain-stream read, write or
flush clears it (`main/streams/plain_wrapper.c`, ADR-0101 §2.3).

## 2. Decision

### 2.1 The concept and the key

A **remembered call result** is a value-domain fact held on a **call key**,
so that a later call with the same key, in the same frame, with no
invalidating site between the two, answers the remembered fact rather than
the row's return type alone.

The key is structural, never textual:

- the **callee**, as ADR-0099's site resolved it: the exact builtin spelling
  (v1), or a project declaration and, for a method, its receiver place (v2);
- the **argument places**, one per positional argument, each a place in
  ADR-0098's sense — a local variable, a typed `$this->p`, an element
  `$a[k]` with a literal key — or a **literal** (`ArgValue`). A named or
  spread argument, a nested call, an operator expression, a `new` or a
  closure gives the call no key in v1;
- the **receiver place**, for a method (v2).

`strlen($s)` and `strlen( $s )` are one key; `strlen(trim($s))` has none in
v1 (PHPStan remembers it by its text; D2).

### 2.2 The subject kind, and the four layers

A call key is a fourth subject kind beside the variable, the offset and the
property: it is carried in the frame's env as a `Known { fact, stratum }`
like a variable's, under a spelling no variable can take (the key contains
`(`), and it is joined at branch joins, widened at loop back-edges and
snapshotted and restored by the branch-scoped forgetting exactly as a
variable fact is (ADR-0031, `branch.rs`). Nothing new is built for join,
widen or scope.

The fact lives in ADR-0035's four layers and is produced by **guard
survival** on the call, through the two existing lowerings (`CondExpr::Call`,
`CondOperand::Other { call }`): `=== literal` pins a `Singleton`, `!== false`
and `!== null` subtract on the transfer's answer (`int|false` minus `false`
is `int`), an ordering guard intersects an `IntRange`, truthiness sets
`NON_FALSY`. Widening is layer descent. The second call's answer is the
**meet** of the transfer's answer with the remembered fact: the remembered
fact can only refine what the row already says.

The stratum is **Verified** — a runtime-executed test on the live branch,
ADR-0052 §1 — on one stated condition: the result is a function of the key
and of settings no site between the two calls rewrote. §2.5 says which rows
meet it; §4 says why the condition is the whole of the ADR-0002 argument.

### 2.3 Scope: within a frame, never across a call

A key lives in the frame that produced it. It does not flow into a callee —
ADR-0001 propagates argument *facts*, and a callee's own `strlen($x)` keys
on its own parameter — and a callee's keys do not survive its return. A
closure body is its own frame; a by-value `use` binding is its own place. A
key produced inside a branch dies with the branch's env unless the join
keeps it (both arms agree, or the arms join to a `OneOf`). A loop widens it
like any fact; a site inside the loop body that invalidates it is walked on
the back edge, so a key invalidated anywhere in the body is not remembered
across iterations.

### 2.4 Invalidation, read off the sites in program order

A key is forgotten at the first later site (in the linear trace, ADR-0027;
sites per ADR-0099) that does one of the following. The three halves reuse
three existing mechanisms and add no tracking.

1. **A place the key names is forgotten.** This is ADR-0070's statement-end
   rule, unchanged: an assignment, a compound assignment, `++`/`--`, `unset`,
   a `list` or `foreach` binding, a by-reference pass that the five survival
   conditions do not rule out, a poisoned scope. A key is dropped whenever
   any place it names is dropped — a reverse index from place to keys is the
   one addition. A `$this->p` place is dropped by ADR-0036's sweep (an
   overridable call on `$this`); an object receiver by any sweep of its id.
   R4, R5, R11.
2. **A setting the row reads is rewritten.** A site carrying
   `global.write.setting.X` for an `X` in the row, or the coarser
   `global.write.setting` or `global.write`, forgets every key whose row reads
   `X` (ADR-0101 §5.4 rule 1). V1 forgets every setting-read key at any
   `global.write`-subsumed site; per-cell precision is a later refinement
   with no change of principle. R2, R7, R12.
3. **A site the lane cannot see.** A site with an effect-lane gap (`…?`: a
   dynamic callee, an unresolved callback, unseen code, `value-dependent-read`,
   …) or an escape-hatch label (`eval`, `ffi`) forgets every setting-read key:
   ⊤ to a consumer asking what a call could touch (ADR-0046). R6. A `{}`-row
   key survives such a site: its result is a function of its places, and the
   places are protected by rule 1 (an unknown call cannot write a local it is
   not handed, absent the poisons rule 1 already honours). R3 across
   `opaque()` is the witness in prose.

And three rows are never remembered at all (ADR-0101 §5.4 rule 5, sharpened
by R13): a `nondet.*` row (every draw writes, R10); an `io.*` row, the stat
family included (§4); and, in v1, a row with an out-parameter at a filled
position (the written place is part of the key and is rewritten by the call
itself — `preg_match($p, $s, $m)` is remembered in v2 with `$m` excluded from
the key).

### 2.5 Which calls are remembered: the site's effect answer is the gate

A call is remembered iff its ADR-0099 site answers **exhaustive** with every
label under `global.read.setting`. That one predicate replaces PHPStan's
hand bit and covers what the catalog already decides per call: a certified
pure name (`is_int`, `array_key_exists`), a call-site certified name whose
arguments are ruled out (`strcmp($a, $b)` under `strict_types`), a foldable
name on non-literal places (`strlen($s)`, `strpos($h, '=')`), a printf call
whose literal format drops every read (`'%05d'`, R8) or keeps only the locale
read (`'%.2f'`, R7, forgotten at rule 2). A `%s` of a value not shown
no-float is `value-dependent-read` at the site and is not remembered — the
gap is the answer, not a weaker memory. A `setlocale(LC_x, '0')` is
remembered once ADR-0101's D6 narrows the literal `'0'` to the read (R1 is
witnessed ahead of that slice).

## 3. Parity with PHPStan, and the recorded divergences

The intent is PHPStan's: `if ($x->getName() !== null) { echo $x->getName(); }`
works without a temporary. Five departures, each for the divergence registry
(ADR-0030):

1. **The gate is the effect lane, not a bit.** "Possibly impure" is a site
   with a gap and is never remembered; Steins therefore behaves as PHPStan's
   `rememberPossiblyImpureFunctionValues: false` by construction, with the
   catalog as the source of every `no`. No configuration key is added (D1).
2. **The stat family is never remembered.** PHPStan remembers `is_dir` and
   `file_exists` (`hasSideEffects: false`); R13a shows its `false` example is
   engine-wrong, R13b that only a success is served and only for the last
   path, R13c that `file_exists` is never served at all. §4.
3. **A setting read is remembered where the cell is known.** PHPStan never
   remembers `getenv` (`hasSideEffects: true`); Steins remembers it once the
   env cell lands (ADR-0101 S6) and forgets it at `putenv` (R12), because the
   invalidation is derived from the cell rather than guessed per name.
4. **An unseen call forgets setting reads.** PHPStan keeps `localeconv()`
   across `opaque($cb)` — the two share no variable — and R6 shows the
   second call differs. Steins forgets at every gap site (rule 3).
5. **The key is places, not text.** Nested calls are not remembered in v1;
   `strlen($a[0])` keys on the element place; spacing and parenthesisation
   do not matter.

## 4. ADR-0002: a remembered fact decides a verdict, so it must be a proof

A remembered `Singleton` feeds `eval_cond`; a `Certainty::No` marks the taken
branch dead (`crates/steins-infer/src/branch.rs:87`, `mark_dead(then_trace)`)
and the file passes and the direct pass never report inside a dead span
(`file_walk.rs:237-239`); the env after the join is the surviving arm's. A
wrong `No` therefore does two things: it hides true findings inside the
pruned branch, and it can manufacture a finding after the join from an env
that the pruned arm would have changed. ADR-0052 §2's law — "facts never
signal death; the verdict owns death" — makes the remembered fact's stratum
the whole question.

For a `{}` row the result is a function of the key, and the key's places are
guarded by rule 1: the second call returns what the first did on every path
the walk admits. For a setting-read row the same holds on every path with no
rule-2 or rule-3 site between. Both are proofs **about the engine**, which is
what a proof-layer finding is about; they say nothing about the world and
need not.

The stat family fails the condition on the engine's own terms. The first
call's result depends on the disk at that moment; the second call's depends
on whether the first succeeded (only then is it cached), on whether any other
path was stat'd between (eviction), on whether any plain stream was read or
written (clear), and on `clearstatcache`. None of that is a function of the
key, so a remembered `is_dir($d)` is not Verified, and it is not Asserted
either — no declaration made it — so there is no stratum in ADR-0037's
ladder to hold it. It is not remembered, and a PHPStan-parity opt-in
("remember possibly-stale filesystem answers") is declined: the owner's line
is lexical and without convenience, and the opt-in would be the convenience
that ADR-0101 refused for the effect lane, moved to the value lane. A project
wanting the idiom writes `$isDir = is_dir($d);`, which is what PHPStan's own
strict option asks for.

## 5. Consequences

- A new subject kind in the env, a reverse index from places to keys, and
  one gate read off the resolved site. No persisted format changes in v1: a
  remembered fact is a walk-time env entry, not trace payload.
- `dumpType(strpos($h, '='))` inside `if (strpos($h, '=') !== false)` shows
  `int<0, max>`; the nsrt rows blocked on the guard-then-use idiom become
  reachable. The harness trap stands: `assertType($x, …)` drops its subject
  to the writes set, so a fixture asserts the second call through `dumpType`
  or a value position, not through `assertType` on the key.
- Findings can move: a remembered fact narrows an offset read, a `match`
  subject or a call operand that the transfer alone did not. Each new
  proof-layer finding on the ten public packages is ledgered with its
  witness before the slice lands; none is expected to be a false positive by
  §4, and the measurement is what shows it.
- The effect lane is untouched: this ADR reads sites and writes no label.
- Deferred, with the reason: methods and receiver places (ADR-0036's sweep
  is the invalidation and is in place, but the exact-receiver resolution the
  key needs is the dispatch question ADR-0099 §7 still carries); project
  callees (the fixpoint supplies the summary before the walk, ADR-0099 §7.3,
  and ADR-0070 the by-ref flags; it is held back only to keep v1 catalog-only
  and measurable); nested calls as key members.

## 6. Considered and rejected

- **Keying on printed text, as PHPStan does.** Cheap and familiar, and it
  remembers `strlen(trim($s))` for free; but it cannot say which places a
  key names, so rule 1 would fall back to substring matching, and `$a[0]`
  would be invalidated by any write to `$a` even where ADR-0098 can place the
  element. Places are the project's vocabulary.
- **A `rememberPossiblyImpureFunctionValues` twin.** The lane already answers
  the question per call; a global knob would let a project remember what the
  catalog cannot vouch for, which is the metadata lie ADR-0063 refuses.
- **Remembering the stat family under an opt-in or at a weaker stratum.** §4.
- **Folding this into ADR-0101.** ADR-0101 §5.5: a subject kind, a key, scope
  rules and a calibration are each larger than any decision there.
