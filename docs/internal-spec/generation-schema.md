# The Generation Schema

**Status: implemented** (`steins_gen::SCHEMA_VERSION`; ADR-0092 §2).

Every artifact in a generation carries one schema number, and a reader that
finds any other number misses and rebuilds. The rule for when that number moves
is the doc comment on `SCHEMA_VERSION` in `crates/steins-gen/src/container.rs`:
what a bump buys, and the four kinds of bump. This page is the record of every
bump since schema 1, the store as it first landed (issue #485). The record used
to be that doc comment, one paragraph per bump, until it outgrew the rule it
sat under (issue #774).

## Adding a bump

Change the constant, add a row to the table and a note under [Notes](#notes),
in the same PR. Anywhere else, cite a bump by its issue, not its number: when
two bumps race, the one that merges second renumbers on rebase, and a number
cited in another file renumbers with it. The conflict on the constant's own
line is what forces the renumber, so it stays a conflict; do not union-merge
it.

## History

The kinds are the constant's: **layout** (the codec, or where the bytes live,
changed), **misdecode** (an old payload decodes as something never written),
**under-answer** (it decodes, and a replay answers less than this binary
decides) and **meaning** (it decodes, and a replay answers something wrong).
Where a bump is several, the column lists each, the one its note leads with
first. A kind marked † is one the note does not state; [the next
section](#where-the-record-and-the-code-disagree) says why it is there.

| Schema | Issue | Kind | Summary |
| --- | --- | --- | --- |
| [2](#schemas-2-and-3) | #504 | layout | The payload codec swaps from serde_json to `steins_db::wire`. |
| [3](#schemas-2-and-3) | #519 | layout | The run-dependent walk blocks move out of the package artifacts into a sidecar. |
| [4](#schemas-4-and-5) | #414 | under-answer, misdecode† | `CondExpr::IssetVar`: a bare `isset($x)` is no longer `Opaque`. |
| [5](#schemas-4-and-5) | #571 | under-answer, misdecode† | `CondExpr::InstanceofDyn`: an `instanceof` of a dynamic class is no longer `Opaque`. |
| [6](#schema-6) | #609 | meaning | `Stmt::invalidated` names the variable under an offset argument, as in `sort($a[0])`. |
| [7](#schema-7) | #579 | under-answer, misdecode† | `ArgValue::Isset`: an `isset(…)` in value position is no longer `Other`. |
| [8](#schema-8) | #615 | under-answer | `ValueOp::BitOr`: a bitwise or is no longer `Other`. |
| [9](#schema-9) | #616 | under-answer | A spread of an array literal lowers to the arguments it flattens to. |
| [10](#schema-10) | #625 | under-answer, misdecode† | `ArgValue::Logical`, `ArgValue::Not`, `ValueOp::Spaceship`: the logical family and `<=>` are no longer `Other`. |
| [11](#schema-11) | #636 | under-answer, misdecode† | `StmtKind::OffsetAppend`: `$a[] = v` is no longer a `Barrier`. |
| [12](#schema-12) | #626 | misdecode, under-answer | `ArgValue::Cast`: a value-position cast is no longer `Other`. |
| [13](#schema-13) | #649 | misdecode, under-answer | `StmtKind::While` carries its condition and body; a `while` is no longer `Opaque`. |
| [14](#schema-14) | #627 | under-answer | An interpolated string lowers to the `ArgValue::Concat` chain it desugars to. |
| [15](#schema-15) | #650 | misdecode, under-answer | `StmtKind::For`, `Foreach` and `DoWhile` carry their bodies; none is `Opaque` any more. |
| [16](#schema-16) | #652 | misdecode | `StmtKind::Foreach` grows its header: subject, key, value, by-reference flag. |
| [17](#schema-17) | #651 | misdecode, under-answer | `While` and `For` grow `break_free`; `DoWhile` grows `break_free` and `cond`. |
| [18](#schema-18) | #641 | meaning, misdecode† | `OpaqueConstruct::ReferenceBinding`: a `&` binding anywhere is a give-up site. |
| [19](#schema-19) | #598 (ADR-0094) | misdecode, under-answer | `GlobalConstDecl` grows the declared value and the `conditional` flag. |
| [20](#schema-20) | #603 (ADR-0057 note) | misdecode, under-answer | `FunctionDecl` and `MethodDecl` grow `ret_top`; `RetHintKind` grows `Top`. |
| [21](#schema-21) | #320 (ADR-0096), #352 | misdecode, meaning | `Stmt` grows `value_position` (#320); `CallTarget` grows `Bool` (#352). |
| [22](#schema-22) | #803 (ADR-0046 amendment) | meaning | `EffectOrigin` grows `Eval` and `Include`: `eval` and an inclusion are effect origins. |

## Where the record and the code disagree

The notes below are kept as they were written, so what they get wrong is
recorded here instead, and the kind column follows this section.

- **Six bumps misdecode without saying so.** Each inserted a variant ahead of
  an existing one, and the wire codec carries a variant by index, so that one's
  index moved: `CondExpr::IssetVar` (schema 4, e4a3aba6) and
  `CondExpr::InstanceofDyn` (5, 2f4b1e8f) ahead of `CondExpr::Opaque`;
  `ArgValue::Isset` (7, 427de0ec), and `ArgValue::Logical` and `ArgValue::Not`
  (10, e8845925), ahead of `ArgValue::Other`; `StmtKind::OffsetAppend` (11,
  d903bca4) ahead of `OffsetUnset`; and `OpaqueConstruct::ReferenceBinding`
  (18, d8b964a2) ahead of `Global`, inside the persisted `Scope::opaque`. The
  notes for 11 and 18 say outright that no index moved; the round-trip test for
  11 in `crates/steins-db/src/persist.rs` says it did. Nor is 12 the first
  misdecode bump, as its note implies.
- **Schema 6 is already a meaning bump.** Schema 18's note calls itself the
  first bump made for soundness, and says every earlier one replays at worst as
  an under-answer. Schema 6's note gives its reason as a replay that keeps an
  array shape this binary forgets, which is a wrong answer, not a weaker one.
- **The notes for 19 and 20 say "changes meaning" of an under-answer.** What
  each describes, a replay answering `unknown` or the arm floor where this
  binary binds or bounds, is a weaker answer rather than a wrong one, so the
  column calls it under-answer.

## Notes

Each note is the paragraph that recorded the bump in `SCHEMA_VERSION`'s doc
comment, moved word for word and re-wrapped. Two pairs of bumps shared one
sentence there, and share one note here.

### Schemas 2 and 3

`2` was the swap of the payload codec from serde_json to `steins_db::wire`
(issue #504), and `3` is the split of the run-dependent walk blocks out of the
artifact into a sidecar container (issue #519), which is what leaves an unmoved
package's artifact byte-identical between generations and therefore shareable.

### Schemas 4 and 5

`4` is the `CondExpr::IssetVar` variant (issue #414): the stored trace IR spells
a bare `isset($x)` differently now, and a reader that disagrees with its writer
about what that condition IS would answer from a forgetting an artifact recorded
and this binary no longer performs, and `5` is `CondExpr::InstanceofDyn` (issue
#571) for the same reason one spelling over.

### Schema 6

`6` is the offset-argument entry in `Stmt::invalidated` (issue #609): a stored
trace of `sort($a[0])` from schema 5 carries no entry for `$a`, so replaying it
would keep the stale array shape this binary now forgets.

### Schema 7

`7` is the `ArgValue::Isset` value carrier (issue #579), the value-position twin
of `4`'s reason: a schema-6 trace spells `$b = isset($a['k'])` as
`ArgValue::Other`, so replaying it would answer `unknown` for a value this
binary decides.

### Schema 8

`8` is `ValueOp::BitOr` (issue #615), the same reason once more and with a wider
blast radius than the operator itself: a `|` used to lower to `ArgValue::Other`,
and an `Other` ELEMENT collapses its whole enclosing array literal to `Other`,
so a schema-7 trace spells `['flags' => FILTER_A | FILTER_B]` as no array at
all.

### Schema 9

`9` is the literal-spread flattening (issue #616): `f(1, ...[2, 3])` used to
lower to `ArgValue::Other` with `has_spread` raised and now lowers to the
three-argument call it names, so a schema-8 trace both spells the value
differently and reports an argument count this binary no longer believes
unproven.

### Schema 10

`10` is the logical family (issue #625) — `ArgValue::Logical`, `ArgValue::Not`
and `ValueOp::Spaceship`, bumped ONCE for all three because they land together —
and it is the same reason a fourth time: a schema-9 trace spells `$a && $b`,
`!$x` and `$a <=> $b` as `ArgValue::Other`, so replaying it would answer
`unknown` for three values this binary now decides, and would miss the dead
right operand of a decided `&&`/`||` that only the new carrier's span records.

### Schema 11

`11` is the auto-index append (issue #636): `StmtKind::OffsetAppend`. A
schema-10 trace spells `$a[] = 1` as `StmtKind::Barrier` — a statement that
clears the whole environment — so replaying one would answer `unknown` for every
local from that line on, where this binary answers the extended array. The
variant is new, not re-encoded, so an old trace cannot be misread as the new
form; it simply states something weaker than the source does.

### Schema 12

`12` is the value-position cast carrier (issue #626): `ArgValue::Cast`. A
schema-11 trace spells `(int) $x` as `ArgValue::Other` — every cast expression
did — so replaying one would answer `unknown` where this binary answers the
conversion grid, or at worst the base type the cast operator guarantees. **And
unlike `11`, this one would MISDECODE rather than merely under-answer**: the
wire codec carries an enum variant by INDEX, and `Cast` is inserted *before*
`Other` in `ArgValue`, so every schema-11 `Other` now names `Cast` — a variant
whose target and operand were never written. That is the sharp half of the bump,
and the reason it is not optional.

### Schema 13

`13` is the structured `while` (issue #649): `StmtKind::While` carries the
loop's condition and its body as a sub-trace, and it sits *before* `Opaque` in
the enum, so — the wire codec carrying a variant by index — every later
variant's index moved. A schema-12 artifact must therefore miss rather than
decode, which is what the bump buys; on top of that it spells every `while` as
an `Opaque` and every `break`/`continue` as a `Barrier`, replaying silence for a
body this binary judges and an erased env where it now keeps one.

### Schema 14

`14` is interpolation lowering (issue #627): `"a $v"` used to lower to
`ArgValue::Other` and now lowers to the left-nested `ArgValue::Concat` chain it
desugars to, so a schema-13 trace answers `unknown` for a value this binary
decides — the string PHP guarantees at worst, the folded literal at best. This
is the **under-answer** kind of bump, like `11` and unlike either of the two
entries above it: no `ArgValue` variant is added or re-ordered — the change is a
lowering and a doc — so a schema-13 payload still decodes correctly and merely
states something weaker than the source does. It neither misdecodes the way `12`
does nor shifts a variant index the way `13` does. The reach is wider than the
row count suggests, because an `Other` element collapses its enclosing array
literal: a schema-13 trace spells `['k' => "$a/$b"]` as no array at all.

### Schema 15

`15` is the other three loop forms (issue #650): `for`, `foreach` and
`do`-`while` lower to `StmtKind::For`/`Foreach`/`DoWhile` instead of `Opaque`,
each carrying its body as a sub-trace. This is the **misdecode** kind, like `12`
and `13` and unlike `11` and `14`: the three variants are inserted after `While`
and the wire codec carries a variant by INDEX, so every later variant's index
moved and a schema-14 payload would decode `Opaque` as one of the new forms —
reading a write set as a statement list. On top of that it spells all three
constructs as `Opaque`, whose body is not carried at all, replaying silence for
bodies this binary judges.

### Schema 16

`16` is the `foreach` header (issue #652): `StmtKind::Foreach` grows the four
fields the binding is about — `subject`, `key_var`, `value_var`, `by_ref` — so a
walker can type `$k`/`$v` from the subject's element type instead of leaving
them untyped. This is the **misdecode** kind again, for the plainest reason: the
variant's payload gains fields, and the wire codec reads a struct variant's
fields positionally, so a schema-15 payload would read the old `body` where the
new `subject` is.

### Schema 17

`17` is the post-loop condition (issue #651): `StmtKind::While` and
`StmtKind::For` grow `break_free` — whether any jump can leave the body, which
is what licenses narrowing the fall-through by the negated header — and
`StmtKind::DoWhile` grows both `break_free` and the `cond` the previous slice
deliberately withheld. The **misdecode** kind, for schema 16's reason exactly:
three struct variants gain fields and the wire codec reads a struct variant's
fields positionally, so a schema-16 `DoWhile` would read its old `body` where
the new `cond` is. On top of that it spells every loop as one nothing is known
after, replaying `unknown` where this binary answers the negated condition.

### Schema 18

`18` is the reference-binding give-up site (issue #641), and it is the first
entry in this history bumped for **soundness** rather than precision.
`scan_opaque_walk` used to recognise a reference only as an assignment's
right-hand side, so `['key' => &$a]` and `foreach ($rows as &$row)` produced no
`OpaqueSite` and left `Scope::poisoned` false — `Scope::opaque` is a persisted
`Vec<OpaqueSite>`, so a schema-17 artifact records a frame as unaliased that
this binary now records as aliased. Every earlier entry could be replayed at
worst as an under-answer; this one replays a *wrong* one, because the walk now
trusts that verdict to bound how wide an offset write's barrier opens (ADR-0063
§2.3's exposure leg). ADR-0092 §2 forbids a miss that changes meaning, so the
bump is not optional. `OpaqueConstruct::ReferenceBinding` is appended after the
last variant, so no existing index moved — the bump buys the refusal to read the
old file, not a decode fix.

### Schema 19

`19` is the platform-constant slice (ADR-0094, issue #598), and it is a
**misdecode** bump of schema 16's kind: `GlobalConstDecl` grows two fields — the
declared literal value and the ADR-0049 A2i `conditional` flag — and the wire
codec reads a struct's fields positionally, so a schema-18 artifact's `span`
would be read where the new `value` is. It also changes meaning even where it
decodes: a schema-18 file records no constant value at all, so replaying one
would answer `unknown` for every same-file `const` this binary now binds.

### Schema 20

`20` is the enforced-top return hint (ADR-0057 note, issue #603), and it is a
**misdecode** bump of schema 16's kind on two counts. `FunctionDecl` and
`MethodDecl` each grow a `ret_top` field and the wire codec reads a struct's
fields positionally, so a schema-19 `FunctionDecl`'s `ret_span` would be read
where the new `ret_top` is; and `RetHintKind` grows a `Top` variant ahead of
`Other`, which moves `Other`'s index. It also changes meaning even where it
decodes: a schema-19 file records a bare `: array` as an unrepresentable hint
that refuses the return summary outright, so replaying one would answer the arm
floor for every call this binary now bounds by `array`.

### Schema 21

`21` carries two slices, and the stricter of the two decides its kind. The
discarded-call family (ADR-0096, issue #320) grows `Stmt` a `value_position`
field, appended last, and the wire codec reads a struct's fields positionally —
so a schema-20 `Stmt`'s trailing bytes would be read where the new field is.
That makes it a **misdecode** bump of schema 16's kind on its own.

The other half is the return-mode output narrowing (issue #352), and it is a
**meaning** bump of schema 18's kind rather than a misdecode one. `CallTarget`
grows a `Bool` variant appended after the last, so no existing index moves and
every schema-20 `ConstArgs` still decodes exactly as it was written. What
changed is what the absence of a second argument *means*: `print_r($x, true)`
was lowered with `second: None` because the parser lexes `true` as a literal and
the old enum could not carry one, so replaying a schema-20 artifact would answer
`io.output.buffer` for every return-mode dumper this binary now proves writes
nothing. ADR-0092 §2 forbids a miss that changes meaning, so the bump buys the
refusal to read the old file, not a decode fix.

### Schema 22

`22` is dynamic code in the effect lane (ADR-0046 amendment, PR #803), and it
is a **meaning** bump of schema 18's kind. `EffectOrigin` grows `Eval` and
`Include`, both appended after `Callback`, so no existing index moves and every
schema-21 payload still decodes exactly as it was written. What changed is what
the absence of an origin *means*: a schema-21 body holding `eval` or an
`include` was lowered with no origin for it, so replaying one would answer `{}`
— exhaustive and effect-free — for a body this binary colors `eval` or
`io.fs.read` and marks `…?`. ADR-0092 §2 forbids a miss that changes meaning,
so the bump buys the refusal to read the old file, not a decode fix.
