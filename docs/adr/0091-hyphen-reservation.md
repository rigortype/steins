# The hyphen reservation: a phpdoc type identifier containing `-` is vocabulary, never a class

**Status: proposed (2026-08-24), PENDING ratification.** Drafted under the
owner's standing delegation, ahead of the slices it governs. No lowering
changes with this ADR. It generalizes a rule three earlier ADRs each state
locally for their own case — ADR-0087 §2.2 for `unset`, ADR-0089 §2 for the
derived operators, and `is_shadowable_pseudo_type` for shadowing — into the one
rule they are all instances of.

## 1. Context: half the rule is already here, and the other half is wrong

Steins already acts on this insight in exactly one place.
`is_shadowable_pseudo_type` opens with

```rust
if norm.contains('-') { return false; }
```

because a hyphenated spelling is not a legal PHP identifier, so no class can
carry the name and nothing can shadow the keyword. That is the reservation,
stated for one question.

Everywhere else the opposite happens. `lower_identifier`'s catch-all lowers an
unrecognized name to `ContractTy::Class(norm)`, and acceptance's class leg
answers a **definite `No`** for every non-object value. Measured 2026-08-23:

```
non-empy-string  => Class("non-empy-string")   admits Val::Str("x") = No
positive-integer => Class("positive-integer")  admits Val::Int(1)   = No
```

A one-letter typo in a pseudo-type does not degrade to silence. It becomes a
contract that **rejects every value it was written to accept**, and a wrong
`No` is a false positive rather than lost precision — the closure-argument
variance check raises findings on `No` (ADR-0071 §1).

ADR-0089's `DERIVED_OPERATORS` closed this hole for seven names, and that is
the shape of every fix so far: a hand-maintained list, one entry per spelling
someone remembered. `KNOWN_UNENFORCED` is the same shape and says so — its doc
comment names the nonexistent-class hazard as the reason it exists. **The
safety is a maintenance property.** This ADR makes it a structural one.

## 2. This is not a Steins defect. It is the ecosystem's.

The cross-tool conformance suite already measures the failure, and Steins is
not in the safe column by design — it is there by coincidence of coverage.

On `phpdoc_advanced_int_range_keyword`, a namespaced fixture whose parameter is
declared `@param int-range<0, 255>`, **5 of 16 analyzer configurations reject
`acceptsByte(200)`** — a call the fixture marks valid — because they resolved
the keyword through the namespace into a class that does not and cannot exist:

```
expects Conformance\Tests\…\int-range<0, 255>, but 200 provided   (Psalm)
expected `unknown-ref(Conformance\Tests\…\int-range)`             (Mago)
```

Across the **69** namespaced fixtures that use a hyphenated keyword, counting
only over-rejections whose output carries the fingerprint of this mechanism (a
backslash-qualified name whose last segment contains a hyphen):

| Tool | over-rejected lines | fixtures |
| --- | ---: | ---: |
| mir | 34 | 18 |
| Mago | 28 | 10 |
| Intelephense | 9 | 9 |
| PHPantom | 6 | 4 |
| Psalm / psalm-next | 3 / 3 | 3 / 3 |
| pzoom | 1 | 1 |
| **total** | **84** | **48** |

**84 is a floor, not a total.** Qodana exhibits the same defect
(`Undefined class 'int-range'`, plus the over-rejection) but prints no
qualified name, so the fingerprint misses it.

Two controls are worth stating, because they are what make the mechanism
identifiable rather than merely correlated. **Phan** implements `int-range`,
passes the fixture cleanly, and its 72 over-rejections elsewhere in the family
drop out of the fingerprint entirely — they have a different cause. And
**PHPStan** does not over-reject at all: it reports
`parameter.unresolvableType` and declines to manufacture a contract, which is
the behavior this ADR adopts.

Steins passes that fixture because `lower_generic` happens to hold
`("int" | "int-range", 2)`. For a spelling it does not implement, §1 is what it
does.

## 3. Decision

**A phpdoc identifier in a type position that contains `-` is type vocabulary.
It is never a class reference.**

1. **No namespace resolution.** It is never resolved against the file's
   namespace or `use` imports. There is no name to find, and the search is
   guaranteed to fail.
2. **No shadowing.** Already true; this ADR only records that
   `is_shadowable_pseudo_type`'s hyphen short-circuit is an instance of the
   rule rather than a local trick.
3. **Never `ContractTy::Class`.** A hyphenated name that is not recognized
   vocabulary lowers to `ContractTy::Opaque` — `Maybe`, silence — in both the
   identifier table and the generic table. The class catch-all becomes
   unreachable for it.
4. **phpdoc only.** A native PHP declaration cannot contain a hyphen (it is a
   parse error), so there is nothing to decide on that side and the rule sits
   exactly at the phpdoc identifier boundary.

## 4. Why the reservation is airtight

An unrecognized identifier normally forces silence because it could be any of
three things Steins cannot rule out. For a hyphenated one, **all three are
impossible**, and each for a mechanical reason rather than a probabilistic one.

**It cannot be a class.** PHP's compiler rejects `-` in a class, interface,
trait or enum name. Two measurements, neither of which found a counterexample:
the seeded builtin catalog holds **no** hyphenated class-like name (the
hyphenated strings in it are phpdoc vocabulary, a date format, and a crate
name), and **6,670** PHP files in the pinned public corpus declare **zero**
hyphenated class-likes.

**It cannot be a `@template` name or a `@phpstan-type` alias — stated as an
ordering, not as a lexical fact.** The reservation applies to an identifier
that **survives** the two rewrites that resolve those: the `@template` shadow
and (once issue #472 lands) alias expansion. Both already run over the parsed
type before anything is lowered, so the ordering costs nothing and the rule
holds whatever either rewrite decides.

Stating it lexically would have been wrong, and the draft of this ADR did state
it that way. Steins' own two layers do disagree about the hyphen —

| Layer | Predicate | `-` in an identifier |
| --- | --- | --- |
| type lexer (`steins-phpdoc`'s `lexer.rs`) | `is_ident_cont` | **yes** |
| tag scanner (`docblock.rs`) | `is_ident_byte` | **no** |

— so `@phpstan-type foo-bar = int` scans here as an alias named `foo`,
measured. But **phpstan/phpdoc-parser does not agree**, and it is the oracle
(ADR-0029). Its `TOKEN_IDENTIFIER` is
`(?:[\\]?+[a-z_\x80-\xFF][0-9a-z_\x80-\xFF-]*+)++` — hyphen excluded at the
start, **included in the continuation** — and both `parseTemplateTagValue` and
`parseTypeAliasTagValue` read their name with exactly that token. Upstream,
`@template foo-bar` declares a template named `foo-bar` and
`@phpstan-type foo-bar = int` an alias named `foo-bar`.

So Steins' tag scanner **diverges from the oracle on hyphenated tag names**,
today, by accident rather than by decision. That divergence is not this ADR's
to settle — it is issue #472's, which is where alias names acquire meaning —
but it has to be settled deliberately there, and this ADR's recommendation is
that a hyphenated tag name be **rejected** rather than silently truncated,
since the whole point of §3 is that the hyphen space is not the program's to
name. Either way, the ordering above is what carries the reservation, so §3
does not wait on the answer.

The one theoretical escape is a name that reaches the class table as a
**string** rather than through the compiler — `class_alias` with an odd second
argument, or an extension registering one. Such a class cannot be named by a
docblock identifier through any resolution path, and Steins' class registry is
built from declarations it parses, so it does not weaken the rule. It is worth
one sentence and no design.

## 5. What the rule buys: an undecidable case becomes decidable

This is the part worth more than the safety fix.

For a **non**-hyphenated unrecognized identifier, Steins must stay silent, and
the three possibilities of §4 are exactly why: it may be a template, an alias,
or a class the index cannot see. ADR-0049 A14 and `lower_identifier`'s
catch-all are both shaped by that.

For a **hyphenated** one, none of the three survive. What remains is a closed
set of two: a misspelling of vocabulary, or vocabulary from a tool Steins does
not model. Both are things worth saying, and neither can be a false positive
about the *program* — the identifier provably denotes nothing.

**A hyphenated identifier that survives the rewrites and is not recognized
vocabulary is therefore a provable defect in the docblock**, and a diagnostic on
it is zero-FP-clean in the sense ADR-0002 means. That is a rare thing to be able
to say — and the qualifier is load-bearing, per §4: the claim is about what
reaches lowering, not about what the lexer can spell.

## 6. The diagnostic, and the one thing it must be calibrated against

The finding this unlocks is *not* free, and the risk is precise: **vocabulary
from other tools that Steins does not model.** `KNOWN_UNENFORCED` exists
because Psalm, Phan and PHPStan have spellings Steins recognizes without
enforcing; the ones it has never heard of are exactly what a naive
"unrecognized hyphenated name" id would convict.

So, per the calibrated-defaults discipline (ADR-0002, the owner's 2026-08-08
restatement): **detect it, and place the floor honestly.** The id reports, and
the surface floor is not `default`. Whether it sits at `contracts` or
`pedantic` is a measurement, made in its own slice against the fp-gate, not a
decision made here.

One refinement is available and deliberately left as an option rather than a
requirement: an unrecognized name within a small edit distance of a known
spelling (`non-empy-string`) is a typo with much higher confidence than one
that is not (`some-psalm-thing`), and the two could carry different floors.
That is a second slice at most, and possibly never.

## 7. Consequences for the two vocabulary tables

Both tables survive, and both change meaning.

**`KNOWN_UNENFORCED` stops being a safety valve.** Its stated purpose is to
keep recognized-but-unmodeled names away from the class catch-all, which under
this rule nothing can reach. What it becomes is the **allowlist for §6's
diagnostic**: the list of hyphenated names Steins knows about and declines to
report. That is a smaller and more honest job than the one its doc comment
currently describes, and the comment has to change with it.

**`DERIVED_OPERATORS` (ADR-0089, issue #473) is subsumed for flooring.** The
arity-blind `Opaque` floor it exists to provide falls out of §3.3: a
`key-of<A, B>` matches no arm, reaches the catch-all, contains a hyphen, and
floors. What the table still says is "this name carries a relation at arity
N" — which is allowlist information, and folds into the same place. The
implementation shipped one day before this ADR; that is the normal order here,
not a mistake, and the list-shaped fix is what made the rule-shaped one
legible.

**ADR-0087 §2.2 stays as it is, and is *not* an instance of this rule.**
`unset` carries no hyphen. It is safe because PHP **reserves** it — `class
unset {}` is a parse error — which is a sibling reason of the same shape ("the
resolution is guaranteed to fail") reached by a different mechanism. The two
rules together are what `is_shadowable_pseudo_type` already encodes as its two
branches.

## 8. Cross-tool: a sibling of conformance issue #7

zonuexe/php-typing-conformance#7 states this rule for exactly one word. Its
expected semantics open with:

> `unset` is not a class. It must never be resolved as `\Current\Namespace\unset`, and it must not produce an "unknown class" diagnostic.

The hyphen rule is the same claim over a far larger surface — 69 measured
fixtures against that issue's one — reached by the compiler's spelling rules
instead of its reserved words. It belongs in the suite as a **sibling issue**
rather than a comment on #7: #7 is about the *semantics* of definedness, which
is a feature, and this is about *resolution*, which is a bug class.

The proposal to state there is small and costs an implementer almost nothing.
It is not "implement Phan's spellings". It is:

1. Do not namespace-resolve a hyphenated phpdoc type identifier.
2. If you do not implement the keyword, do not manufacture a class contract
   from it — the type is unknown, and an unknown type rejects nothing.
3. If you report it, report unknown **vocabulary**, not an unknown **class**.

The suite already has the axes to express it. `recognition`, `enforcement` and
`over_rejected_lines` separate the two things the current fixture notes
conflate: *not recognizing another tool's spelling is expected and correct;
over-rejecting a valid call because you did not recognize it is the defect.*

## 9. Consequences

**Accepted.** One new diagnostic id, with a floor that has to be measured
before it ships (§6). Two table doc-comments that no longer describe what their
tables are for (§7). A divergence entry: where PHPStan, Psalm and Mago report
an unknown *class*, Steins reports unknown *vocabulary* or stays silent, and
never over-rejects.

**Surfaced, not settled.** §4 found that Steins' tag scanner truncates a
hyphenated `@template` / `@phpstan-type` name where phpstan/phpdoc-parser keeps
it whole — an unregistered divergence from the ADR-0029 oracle, in the one crate
whose premise is faithful porting. It is issue #472's to decide, it wants a
registry entry once decided, and §3 does not depend on the answer.

**Bounded.** No denotation changes for any name Steins already models. Every
spelling in `KNOWN_UNENFORCED`, `DERIVED_OPERATORS`, the refined-string grid
and the array vocabulary lowers exactly as it does today. What changes is the
answer for names Steins does *not* model, and it changes from a manufactured
`No` to silence.

**Sequencing.** §3 is one slice and can land alone; it strictly removes wrong
answers and needs no calibration. §6's diagnostic is a separate slice with its
own fp-gate evidence, and it is the one that can be wrong. Do not bundle them:
the first is a bug fix, the second is a judgement call.
