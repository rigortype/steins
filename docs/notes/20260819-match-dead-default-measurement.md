# `match.dead-default`: the measurement, and why the id is not registered

Issue #432. ADR-0088 §7 left `match.dead-default`'s floor **to measurement**
rather than to taste, on the #35 precedent: measurement mode first, emitting
nothing; every hit triaged verbatim; the floor chosen from the result; and the id
registered only if the yield justifies it. This note is that measurement. Its
answer is that the id has **nothing to report**, so it is not registered, and
ADR-0088 §7's floor question stays open rather than being settled on no evidence.

## The verdict being measured

A `default` arm is **provably dead** when the arms above it subtracted the
subject's declared domain to nothing. That is read off the same carrier ADR-0088
§4's sentinel and §5's throw gate read, in the opposite direction: those two ask
whether a *residue* survives, this one asks whether the lane **emptied**.

Two grades, told apart by which of `subtract_contract_lane`'s two endings the
lane took — kept-empty (all arms `Verified`) or dropped (an `Asserted` arm was in
it). §2 admits only the first to a `match.`-prefixed id.

Three body classes, per §3's defensive terminator:

| class | body |
|---|---|
| `FallsThrough` | produces a value, or has an effect and continues — the only class §3 does not carve out |
| `Terminator` | exactly one `throw` / `exit` / call to a `: never` callee — §3's letter |
| `TerminatesAndMore` | terminates, but does something first (`log(…); throw …;`) — measured apart because §3's letter and its stated rationale disagree about this cell |

## The numbers

Both corpora, one run, **100,359 files** (public 11,670 across ten pinned
packages plus phpstan-src; the private corpus 85,150 + 3 parse-error files).

**Denominator.** `default` arms written at all: **1,066** — 924 in the private
corpus (378 `default =>`, 546 `default:`), 142 in the public one (92 and 50).

**Provably-dead `default` arms found: 3.** 0.28% of the arms written.

| axis | result |
|---|---|
| by shape | `switch` **3**, by-value `match` **0**, `match (true)` guard chain **0** |
| by grade | `Asserted` **3**, `Verified` **0** |
| by body | `Terminator` **3**, `TerminatesAndMore` **0**, `FallsThrough` **0** |

**Reaching `match.dead-default`: 0.** Each of the id's three gates removes the
entire population on its own — the shape gate (§9 defers `switch`), the grade
gate (§2 requires Verified), and §3's terminator carve-out. Nothing survives any
one of them, let alone all three.

**The open question §7 named — how much of the population does the terminator
carve-out remove? — answers 3 of 3, 100%.** And it does so on §3's *letter*, the
sole-terminator reading: every hit is a bare `throw`, so the `TerminatesAndMore`
cell that the letter and the rationale disagree about did not occur once in a
thousand `default` arms. The disagreement is real but, measured, it is not
load-bearing.

## The triage, verbatim

All three, TRUE. No false positives.

**1.** `<private>/…/Controller/Signup.php:116` — `switch`, Asserted, Terminator.

```php
/**
 * @param string $type
 * @phpstan-param 'login'|'signup' $type
 */
private static function _redirectToAccounts($return_to, $type, $signup_key): void
{
    switch ($type) {
        case 'login':  …  break;
        case 'signup': …  break;
        default:
            throw new PxvException('有り得ないケース', ['type' => $type]);
    }
```

TRUE dead on the docblock's premises, and the arm is a defensive terminator. The
grade is `Asserted` because the native parameter is untyped and the two-literal
union is a `@phpstan-param` alone — the engine enforces nothing, so a third value
genuinely arrives at runtime and the `default` is genuinely live. This is
ADR-0088 §8's second row exactly, and both of that row's answers are silence.
Note also what the author wrote in the arm: *"有り得ないケース"* — "impossible
case". §3's rationale is that scolding this teaches the author to delete their
own safety net, and here the author has annotated the safety net as such.

**2, 3.** `nette/utils/src/Utils/Helpers.php:111` and `:113` — the same file
vendored twice (under `rector/rector` and under `symplify/easy-coding-standard`),
so two hits, one site. `switch`, Asserted, Terminator.

```php
/**
 * @param  '>'|'>='|'<'|'<='|'='|'=='|'==='|'!='|'!=='|'<>'  $operator
 */
public static function compare($left, string $operator, $right): bool
{
    switch ($operator) {
        case '>':  return $left > $right;
        …
        default:
            throw new Nette\InvalidArgumentException("Unknown operator '{$operator}'");
    }
```

TRUE dead on the docblock's premises, defensive terminator. `Asserted` for
ADR-0088 §8's *third* row's reason: the engine enforces `string` and the docblock
refines within it, so covering the ten literals exhausts the declared domain and
says nothing about `string`. The gate reads that correctly with no clause about
docblocks anywhere in it — `subtract_contract_lane` drops a lane that empties
with any surviving-`Asserted` history, and the drop is the whole mechanism.

## Why the number is 3 and not 300

Three reasons, and only the third is a limitation worth closing.

**Exhaustive case analyses mostly do not write a `default` at all.** ADR-0052's
2026-08-18 note already observed this for enums — "the idiomatic exhaustive enum
`match` has **no** `default`" — and the measurement bears it out across both
corpora. A `default`-less exhaustive `match` is not this id's business; it is
§5's, and it lands as silence there too.

**Where a `default` *is* written under an exhaustive chain, it is written as a
guard.** That is the whole population found: 3 for 3 are `throw`. §3 predicted
this shape and carved it out in advance, and the measurement is the evidence that
the carve-out was aimed at the right thing rather than at a hypothetical.

**The arm lane only exists for declared parameters.** This is the real reach
limit. `Store::contract` is seeded from a parameter's native declaration (refined
by its docblock) and from declared returns; a `match` on a local assigned from a
call, on a property, or on anything the walk poisons has no lane to empty, so no
verdict is reachable however exhaustive the arms are. All three hits are
`switch ($parameter)`, consistent with that. The property leg is queued in
ADR-0052's 2026-08-18 note (it needs ADR-0036's object-graph extension) and the
return-through-summary leg with it; when either lands, this measurement should be
re-run rather than trusted.

Two further silences, both false-negative-safe and both properties of the walk
rather than of this check: a `match` inside a `try` is invisible (the walk never
structures a `try` body — the same scope gap ADR-0088's 2026-08-19 note records
for §5), and a poisoned scope is skipped, which in a 2007-era codebase is not
rare.

## What the measurement proved about the machinery

The machinery is not being kept (see the recommendation below), so this section
is a record of how the number above was established and what it cost to establish
it — not a description of shipped code.

The corpus number is 0, so the corpus is not evidence that the check worked. The
hand-written probes are — 4 files, 30 constructs, 16 recorded verdicts, every one
triaged by hand — and two of them found defects the corpus reported nothing
about, which is this run's recurring lesson holding for a seventh time.

* The predicate vocabulary was **dropping** an emptied all-`Verified` lane where
  the value-subtrahend path **keeps** it, so ADR-0088 §8's entire first row — a
  native `string|int` exhausted by `is_string`/`is_int`, the headline cell — read
  as *absence* rather than emptiness, which is the opposite claim. Fixed; ADR-0052
  has the note.
* The body classifier read the walk's own `Flow`, which answers `Terminated` for
  a `return`. §3's list is `throw` / `exit` / a `: never` call and pointedly not
  `return`, and in a `switch` every arm must `break` or `return`, so
  `default: return 4;` — the ordinary value-producing arm — was being carved out
  as a defensive terminator, swallowing exactly the population the id exists to
  report. Fixed by classifying structurally instead, on §3's own list. This
  defect and its fix both lived inside the machinery, so both go with it; the
  lesson worth keeping is that the walk's reachability answer is not §3's
  question, and a future implementation must not reach for it either.

The direction of the verdict is what made it safe, and the probes confirmed it
held: the check reads **emptiness**, and a lane the arm conditions could not
model stands at its full seeded width and reads non-empty. Ignorance therefore
produces silence, not a finding — structurally, not by a gate that could be
forgotten. That is the opposite exposure from §4's sentinel and §5's throw gate,
which read a non-empty residue and need `Store::contract_narrowed` to tell
evidence from ignorance; this verdict needed no such mark and consulted none.
Anyone rebuilding it should keep that property, and keep the entry-lane rule the
probes were written to check: a lane emptied by guards **above** the construct
makes everything below unreachable, and a `default` underneath it must not be
called dead for a reason that has nothing to do with the `match` it is written
in, so the verdict must require the lane to have held something when the
construct was entered and must drop any variable a guard call invalidated on the
way down.

## Recommendation

**Do not register `match.dead-default`.** The yield is zero and the id has no
triage to choose a floor from, so choosing `Floor::Default` or `Floor::Contracts`
today would be the taste call §7 wrote itself to avoid. `REGISTERED_NOT_YET_
EMITTED` is not the right home either: that list is for ids whose emitter is
coming, and this one's emitter exists and finds nothing.

**And the measurement machinery does not ship either** (owner ruling,
2026-08-19). With no id registered it emits nothing and finds nothing, so
carrying it is carrying dead weight; reaching the guard-chain spelling had cost a
syntactic-provenance bit on `StmtKind::If`, which ADR-0031 deliberately keeps off
that variant and which issue #448 answers off the CST without touching — two
mechanisms for one question being worse than one; and re-measurement is gated on
ADR-0052's queued property/return-summary leg, a distant change that will want to
build on whichever mechanism exists then rather than on this one. This note is
the deliverable, and the method above is written out well enough to redo.

**One repair from the measurement does ship**, because it is independently
correct and closes a residue ADR-0052 had already recorded: the type-predicate
vocabulary's emptied-lane rule, the first bullet above. It is worth its own
keep — see the ADR-0052 note of the same date for the measured neutrality — and
it is also what issue #448 met from the other side, its guard-chain throw gate
having hit the same asymmetry and worked around it at its own call site.

**Two findings for #434 (`phpdoc.dead-default`), which owns the Asserted grade.**
Its entire measured population across 100,359 files is these same three hits —
and §7 gives it the same terminator carve-out, so it would report **0** as well.
The two ids together are silent on every provably-dead `default` arm in a
thousand. That is the design working exactly as ADR-0088 §3 intended, and it is
also the reason neither id has yet earned a floor.
