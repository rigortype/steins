# A by-reference out-parameter carries a fact only where the call proves it was written

Issue #148. Status: PENDING ratification (autonomous design under the owner's
post-hoc-ratification mode, per the ADR-0063/0067/0076 precedent). Context:
ADR-0063 §2.3 (by-ref coloring), ADR-0070 (`by_value_survivors` — the existing
call-argument survival machinery), ADR-0031 (guard refinement), ADR-0037 (trust
strata), and the measured PCRE semantics recorded on issue #148.

## 1. What is missing, and what is not

The catalog already knows which argument positions a builtin writes through
(`out_params`; `preg_match` → `P2`). That knowledge does exactly one thing
today: it makes the walk *forget* the name after the call. Forgetting is sound
and it is also the whole story — no builtin has ever taught the engine what it
wrote. Every `$matches[1]` in every analyzed project is therefore `unknown`.

The missing seam is not "stop forgetting". It is a **write with a known
value**: the call invalidates the name, and then, on the paths where the write
is proven to have happened, rebinds it to a fact the callee's contract
determines.

## 2. The soundness pivot: an out-parameter write is conditional

The naive design seeds the fact unconditionally at the call. Measured PCRE
behavior (issue #148) refutes it:

* a match returns `1` and assigns the success shape;
* no match returns `0` and assigns `[]`;
* **a compile failure returns `false` and assigns nothing at all** — the
  variable keeps its prior value, or stays undefined.

The third case is not a value the fact could widen to include. It is the
*absence of an assignment*, and no value-shaped fact can express "this variable
still holds whatever it held". An unconditional seed would therefore have the
engine manufacture a fact that is false on a reachable path — the one thing
ADR-0002's zero-FP bar forbids at its foundation.

The same measurement supplies the fix. The return value is the witness:
**truthy ⟺ the callee compiled its inputs and performed the write**. So the
fact belongs where the truthiness is known, and nowhere else.

## 3. Decisions

1. **A seed is a refinement, not a transfer.** The fact enters through the
   guard machinery (ADR-0031), on the branch where the call's result is proven
   truthy — not at the call statement. On the falsy branch and on every
   unguarded path the name stays invalidated, exactly as today. This is the
   whole ADR in one line, and it is a soundness decision before it is a
   precision one.
2. **The catalog states the witness, per name and position.** An out-parameter
   row grows an optional *written-when* condition naming which return values
   prove the write. Absent means "no seed" — every existing row is unchanged
   and stays unchanged until someone measures its contract. The engine never
   guesses a witness.
3. **The fact is computed from proven arguments only** (ADR-0037): for
   `preg_match`, a `Singleton` pattern read by the slice-A group reader
   (#149), which declines on anything it cannot fully establish. A widened
   argument, or a declining reader, yields no seed. A seed is `Asserted`
   grade — derived from a declared contract plus proven inputs, never from
   observing a run.
4. **The seed replaces the invalidation on that branch; it does not race it.**
   Order is fixed: the call invalidates the name, the branch refinement then
   binds the seeded fact. A reader of the code should not have to reason about
   which won.
5. **Nothing here changes `by_value_survivors`** (ADR-0070). That machinery
   answers "may this name keep the fact it already had?"; this one answers
   "does the callee give it a new one?". They are different questions about
   different facts and must not be folded together — a name can be both
   invalidated by ADR-0070's rules and re-seeded here, which is precisely the
   `preg_match` case.
6. **Aliasing refuses.** A seed applies only to a plain local variable at the
   out-parameter position. `$this->m`, `$arr['k']`, a variable-variable, or any
   argument the lowering cannot name refuses — the same discipline
   `out_params` already applies for coloring, for the same reason (ADR-0063
   §2.3: the write may be visible to callers this scope cannot see).

## 4. What this admits, and what it postpones

Admitted by this ADR: the seam, the witness vocabulary, and the
`preg_match`/`preg_match_all` rows that motivate it. Postponed, each a
decline until measured: the flag-dependent shapes (`PREG_OFFSET_CAPTURE`,
`PREG_UNMATCHED_AS_NULL`, `PREG_SET_ORDER`), and every other `out_params` row
— `sort` and friends write their argument too, and their contracts are worth
the same treatment, but none of them has been measured and none is seeded here.

## 5. Considered and rejected

- **Seeding unconditionally at the call.** Unsound on the compile-failure path
  (§2), and the unsoundness is invisible in ordinary testing because the
  failure needs a malformed pattern to surface.
- **A weak unconditional floor (`array<…>|null`) plus a sharp guarded fact.**
  The floor is still wrong — the variable may hold an unrelated prior value,
  not null — and it buys nothing the guarded fact does not already give.
- **Teaching `by_value_survivors` to keep the name and mutate it.** Conflates
  survival with assignment (§3.5), and would put a "the callee wrote this"
  claim inside machinery whose entire contract is about facts the callee did
  *not* disturb.
- **Deriving the witness from the declared return type.** `int|false` does not
  say which member proves the write; only the function's documented contract
  does. The catalog states it explicitly or the row does not seed.
- **Treating the seed as `Verified`.** It rests on a declared contract, not on
  execution. `Asserted` is the honest stratum, and it keeps the proof layer
  free of any fact this ADR introduces.

## Amendment (2026-08-06, issue #158): the witness is truthiness, not the truthiness *spelling*

§2 derives the witness from measured return values and states it as
**truthy ⟺ the write happened**. The implementation, however, could only see
the witness in one syntactic place: a call tested directly in guard position.
`preg_match($re, $s, $m) === 1` proves the same thing — measured, the return
values are `1` on match, `0` on no match and `false` on a compile failure, so
`1` is reachable only through the branch that wrote — and the seed declined
there, answering `unknown` where PHPStan types the success shape. Eight rows of
the reference `preg_match_shapes.php` fixture are exactly this spelling.

The amendment adds no new witness vocabulary and changes no catalog row.
`WrittenWhen::ReturnTruthy` stays the only condition a row may state, and it
stays a claim about the *value* the callee returned. What is added is the set of
condition shapes from which the walk can prove that value truthy:

**A comparison against a literal witnesses truthiness when every value that
satisfies it is truthy.** PHP's falsy values are a finite, enumerable set
(`null`, `false`, `0`, `0.0`, `''`, `'0'`, `[]`), so the question is decided by
running each of them through the engine's own comparison semantics rather than
by a table of blessed operators. `=== 1`, `1 === …` and `== 1` qualify on the
true branch; `!== 1` and `!= 1` qualify on the *false* branch, which is what
carries the fact past an early-return guard. `=== 0`, `!== false`, a comparison
against a non-literal, and the else branch of `=== 1` all fail the test and
decline silently, as does every ordering comparison (`> 0` and its siblings
lower to `Opaque` before a witness could be asked; admitting them is a separate
precision change, not a soundness one).

Two boundaries are worth stating because they are easy to erode:

* **This does not widen `@phpstan-assert-if-true`.** That envelope is stated
  about the callee returning `true`, and `f($x) === 1` does not witness that —
  `1` is truthy but is not `true`. The two consumers therefore read the
  condition through *separate* collectors, so a comparison cannot silently
  widen every envelope in the project.
* **Decision §3.4 is unchanged and now load-bearing in a second place.** The
  same slice fixed the soundness defect issue #158 reports: a call in a
  comparison operand was invalidating nothing at all, so a pre-call `$m = []`
  survived into the branch as a false `list{}`. The order stays "invalidate,
  then seed" — the seed replaces what the invalidation forgot, and on every
  comparison shape that fails the witness test above, the invalidation is the
  whole answer.
