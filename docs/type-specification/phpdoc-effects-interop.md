# Interop envelopes: effect labels on the PHPStan purity tags

**Status: implemented** (ADR-0082; issue #303). Steins reads these tags as
interop envelopes: a covered call contributes to the caller's **declared
lane** at abstraction-typed call sites (role A, ADR-0067), and the
declaring function is contract-checked against its own bound —
`effect.envelope-exceeded` (role B). `steins transform effects-envelope`
writes the tags from proven effects. This document is now the description
of that live behavior; it remains deliberately self-contained — motivation,
grammar, semantics, and compatibility — so it can be handed to an upstream
(PHPStan) discussion as-is. Steins-specific machinery is confined to the
[Steins semantics](#steins-semantics) section.

## Proposal in one line

Allow a list of **effect labels** as a parameter of the existing
`@phpstan-impure` tag — `@phpstan-impure io` — turning a boolean impurity flag
into a declared upper bound on *what kind* of impurity a function may perform.

```php
/** @phpstan-impure io.db, nondet.time (reads the clock for cache TTL) */
function refreshCache(string $key): CacheEntry { … }
```

The idea and the parameter position are Ondřej Mirtes' own suggestion
(2026-08-09): *"The effect could just be a parameter after @phpstan-impure
PHPDoc tag. Like @phpstan-impure io."*

## Grammar

```ebnf
impure-tag      = "@phpstan-impure" [ label-list [ comment ] ] ;
pure-tag        = "@phpstan-pure" ;
class-pure-tag  = "@phpstan-all-methods-pure" ;
class-impure-tag= "@phpstan-all-methods-impure" [ label-list [ comment ] ] ;

label-list      = label { "," label } ;
label           = segment { "." segment } ;
segment         = lowercase-letter { lowercase-letter | digit } ;
comment         = "(" text-without-close-paren ")" ;
```

- The shape is `@phpstan-ignore`'s: a comma-separated identifier list, one
  optional parenthesized trailing comment. `@phpstan-ignore`'s identifiers are
  already dot-paths, so the two tags read alike.
- The comment is only legal **after at least one label**. A `(` directly after
  the tag name is parsed by `phpstan/phpdoc-parser` as a Doctrine annotation
  and can produce `phpDoc.parseError` — `@phpstan-impure (why)` is not a valid
  spelling of anything.
- Accepted tag spellings mirror PHPStan's implementation exactly: the impure
  family is `@impure` / `@phpstan-impure`; the pure family is `@pure`,
  `@phan-pure`, `@phan-side-effect-free`, `@psalm-pure`, `@phpstan-pure`; the
  class-level pair exists only as `@phpstan-all-methods-pure` /
  `@phpstan-all-methods-impure`, no aliases.

## Semantics

### Labels are hierarchical; the declaration is a bound

A label is a dot-path in a known vocabulary, checked by **segment-aware prefix
subsumption**: a declared `io` admits an inferred `io.net.http`; it does not
admit `iota`. The label list on a tag is an **upper bound** (an *envelope*) on
the function's effects — a claim that every effect the body performs is
subsumed by some listed label. It is not an exhaustive description: declaring
`io` says nothing about *which* I/O happens, only that nothing *outside* `io`
does.

The v1 vocabulary (Steins' builtin registry, minus its `failure.*` family,
which names value provenance rather than an effect and is out of scope here):

```text
exit
ffi
global.read   global.write
io   io.db   io.fs   io.fs.read   io.fs.write   io.input   io.ipc
     io.net  io.net.http   io.process   io.signal
     io.output   io.output.buffer   io.output.header
                 io.output.stderr   io.output.stdout
mutate   mutate.local
nondet   nondet.random   nondet.time
```

Because a bound is checked by subsumption, a checker never needs the full leaf
vocabulary to be useful: the *subsumption half* of verifying `@phpstan-impure
io` is one question — "is every impure point I found subsumed by `io`?" — a
segment-wise string prefix test. (The full checking surface is of course
larger: the list grammar, a three-state reading — absent / unbounded /
bounded — effect attribution for language constructs and catalogued builtins,
call propagation, precedence, and diagnostics. The prefix test is the core
relation, not the whole implementation.)

### Bare tags

- A bare `@phpstan-impure` means what it means today: the function is impure,
  nothing said about how — the ⊤ bound. Adding labels only ever *narrows* an
  existing tag's meaning; under a checker that has not adopted this proposal,
  no existing docblock changes meaning. (What adoption itself changes is a
  separate, honest claim — see [Backward
  compatibility](#backward-compatibility).)
- `@phpstan-pure` is the empty envelope — with one deliberate, precisely
  bounded exception, spelled `mutate.local`: mutation of a local binding that
  the enclosing function **owns**, that is **not aliased**, and that does
  **not escape** the frame — `preg_match($p, $s, $matches)` into a local,
  `sort($rows)` on a local copy. Writes reaching by-ref formal parameters,
  properties, statics, superglobals, `global` aliases, reference aliases, or
  escaping captures are *not* `mutate.local` and stay outside the pure bound.
  Note the call boundary: `sort()` itself is not pure — it is the *enclosing
  function*, having confined the mutation, whose envelope discharges it, and
  only that enclosing function is a candidate for the pure-call optimizations
  below. This is the answer to the long-standing by-ref question that
  `hasSideEffects` flags could not express.
- `@phpstan-pure` takes no labels: "pure, except it performs effects" is a
  contradiction. A partially-effectful function is spelled as an impure bound.
- `@phpstan-pure` does **not** claim termination. The vocabulary has no
  divergence label (Koka separates `div` from purity for exactly this
  reason), so "pure" means "no observable effects", not "total": a checker
  may deduplicate or memoize repeated pure calls even though that changes
  behavior for a diverging program. Tracking divergence is out of scope.

### Unknown labels

A label a checker's vocabulary does not recognize does not narrow the bound
to the labels it does recognize — it makes the **whole tag** unspecified (the
⊤ bound), the same reading a bare `@phpstan-impure` already has. Current
PHPStan discards everything after `@phpstan-impure`, so a docblock may
legitimately carry a human's one-word note in that position —
`@phpstan-impure database` — syntactically indistinguishable from a typo of a
real label (`@phpstan-impure io.netw`). A checker cannot tell those apart, so
it must not choose the reading that manufactures a violation: judging the
body against only the recognized subset of the list would hold the function
to a narrower claim than its author wrote, which is not what `@phpstan-impure
io.db, io.netw` says. Widening the whole tag to ⊤ can only lose a finding,
never invent one.

An unspecified tag still **wins** whatever precedence rule applies to it (the
class-level override of the next section, in particular): a method whose own
tag is unspecified does not fall through to its class's tag, because the
method did write something, however uninterpretable — falling through would
check it against a bound its author never reached for.

This rule is deliberately **fail-open**, and the trade should be stated
plainly: it creates no new false positives, at the price of silently losing
enforcement — a typo'd `@phpstan-impure io.dbb` checks nothing, and (by the
precedence rule above) also suppresses the class-level bound that would
otherwise have covered the method. "Degrades safely" means "invents no
finding", not "loses nothing". A checker that *enforces* envelopes SHOULD
therefore pair the bound-reading rule with a vocabulary-conformance
diagnostic (unknown label, with a typo-distance suggestion), so the
degradation is at least visible at the declaration; the diagnostic is a
separable concern from bounding, but enforcement without it leaves the
fail-open path invisible.

### Class-level tags

`@phpstan-all-methods-pure` / `@phpstan-all-methods-impure` (PHPStan 2.1.39)
distribute the claim over the class's methods. Their semantics here are
PHPStan's implemented semantics, verbatim:

- A method-level purity tag always **overrides** the class-level tag (the
  class tag is a fallback for methods that say nothing themselves). This is
  also the exception mechanism: a class-wide claim with one deviant method is
  spelled class tag + method tag, which is why no `-except` syntax is needed
  in v1.
- `all-methods-pure` covers the constructor (a constructor initializing its
  own properties is still pure) and does **not** cover void-returning methods.
  `all-methods-impure` covers every method unconditionally.
- The tags apply to methods *declared* in the annotated class (static
  included). They do not propagate to methods a subclass declares or
  overrides, nor from an interface to its implementations.
- `@phpstan-all-methods-impure` accepts a label list with the same
  bound-over-all-covered-methods meaning: `/** @phpstan-all-methods-impure
  io.net */ class RedisClient` bounds every method at once.
  `@phpstan-all-methods-pure` takes no labels, like `@phpstan-pure`.

### Checking

A checker that understands the labels verifies the bound: an effect inferred
in the body that no declared label subsumes is a diagnostic on the
declaration ("declared `io`, performs `nondet.time`"). A checker that does not
understand the labels loses nothing: the tag still carries its current
boolean meaning.

### Trust models for bounds on abstractions

An envelope on an interface method invites a second use beyond checking the
declaration: trusting it as an upper bound at call sites typed against the
interface, where the concrete callee is unknown. That trust is only coherent
under one of two disciplines, and an adopting checker must pick one:

- **Contract with substitutability.** The bound is trusted at call sites,
  *and* an overriding method's envelope must be subsumed by the overridden
  one (Liskov inclusion; with `pure = ∅` and bare impure = ⊤, the existing
  impure-cannot-override-pure rule is the degenerate case). This is the
  natural single-spelling choice for a checker that already trusts
  `@phpstan-pure` declarations in its purity checks.
- **Hint without proof.** The bound contributes what it claims but is never
  treated as exhaustive — a covered call reads as "≤ these labels, and
  possibly more" — and no substitutability rule is imposed on overrides.
  This is the right posture when a checked spelling exists *above* the
  docblock (Steins' native attributes) and the docblock is the unchecked
  stratum.

Trusting the bound at call sites while neither checking overrides nor
capping the trust is not a coherent option: an impure implementation behind
a pure-declared interface would then be invisible by construction.

### Beyond envelope checks: labels as invalidation keys (informative)

Envelope checking is not the only machinery inside a checker that can consume
effect *kinds*. A checker that remembers returned values across statements —
PHPStan documents its own such machinery in
[Remembering and forgetting returned
values](https://phpstan.org/blog/remembering-and-forgetting-returned-values) —
must forget them when an intervening call could have changed what they
describe, and with only a boolean impurity flag the trigger is
all-or-nothing: *any* side-effecting call forces forgetting. Labels let the
invalidation be scoped to what the memory actually depends on. Memory
derived from the filesystem's stat cache (`is_dir($x)`, `file_exists($x)`)
is invalidated by a call whose labels reach `io.fs` or `global.write` —
`clearstatcache()` is precisely a `global.write` — while an intervening
`rand()` (`nondet.random`) provably cannot have touched it and need not
forget anything. This is informative, not part of the tag semantics: it
records that the label vocabulary pays for itself inside an engine even
before any user writes an envelope.

### Vocabulary evolution

The dot-path vocabulary is **open**, and the openness has consequences a
consumer should be able to rely on:

- **Adding a leaf never changes a *recognized* bound.** A coarse bound is a
  predicate, not an enumeration: a declared `io` admits a future `io.xyz` by
  construction, and a fine bound (`io.db`) is unaffected by new siblings.
  (Contrast Koka, whose `io` is an *alias* expanding to a closed effect row —
  there, growing the alias changes the meaning of every `io` annotation.
  A prefix buys the evolution property an enumeration cannot have; the price
  is that a coarse bound's extension grows silently with the registry.)
  The claim is scoped deliberately: a docblock that already carried the new
  spelling *as an unknown label* flips from unspecified (⊤) to bounded the
  moment the vocabulary grows — for such docblocks a vocabulary addition is a
  semantic event, and it belongs in release notes.
- **Moving or removing a node is a breaking change**, and it degrades along
  two paths by design: in a docblock tag the retired spelling becomes an
  unrecognized label, so the whole tag reads as unspecified and no finding is
  invented; a checked native annotation carrying it earns the
  vocabulary-conformance diagnostic. The `output` → `io.output` migration
  (ADR-0083) exercised both paths.

### Reserved: complement bounds

A future `-except` form ("anything but `io`") is sound — the check inverts to
"is the inferred effect subsumed by an excluded label?" — but is deliberately
not part of v1: the class/method override rule covers the motivating cases,
and the core subsumption relation stays one string prefix test. One caution the
evolution rules above imply: a positive bound is *stable* under vocabulary
growth, while a complement bound is not — every leaf added later silently
joins what "`io -except io.db`" admits, so an exclusion is always read
against the vocabulary at checking time, not at writing time.

## Backward compatibility

Two claims, deliberately separated — conflating them was the honest mistake
an adversarial review caught, and the distinction is load-bearing:

**Parser compatibility (unconditional).** Verified against
`phpstan/phpdoc-parser` 2.3.3 and phpstan-src 2.2.x: `@phpstan-impure io`
parses as the `@phpstan-impure` tag with a `GenericTagValueNode("io")` value.
PHPStan reads only the tag name, so the tag functions exactly as today; the
labels are ignored. No `phpDoc.parseError`, no `phpDoc.phpstanTag`. The one
syntactic hazard is the Doctrine path noted under [Grammar](#grammar), and
the grammar forbids the only spelling that triggers it. No new tag name is
introduced, and a label list only ever narrows what its own tag claims.
Anyone can start writing labels today, under any checker.

**Semantic migration (at adoption).** A checker that *adopts* this proposal
reinterprets every already-written recognized suffix: `@phpstan-impure io`
goes from ⊤ (boolean impure, suffix ignored) to the bounded claim `io`, and
a body performing `nondet.time` earns a new finding. That is the feature
working as intended — the docblock was lying — but it is a semantic change
for pre-existing docblocks, and an adopter should treat it as one: ship the
checking behind an opt-in (or a migration release), and provide the
vocabulary-conformance diagnostic so existing suffixes can be audited before
enforcement is enabled. The same discipline applies to later vocabulary
additions (see [Vocabulary evolution](#vocabulary-evolution)): they are
semantic events for docblocks that carried the new spelling as an unknown
label.

## Steins semantics

This section is Steins-internal; an adopting checker needs nothing in it.

Steins spells checked effect declarations as native attributes
(`#[\Steins\Effect('io')]`, `#[\Steins\Pure]` — ADR-0006), verified with full
contract checking and Liskov conjunction across the hierarchy. The docblock
forms above are the **interop envelope** (ADR-0082): the *unchecked* spelling
of the same envelope concept, one trust stratum below the attribute.

- An interop envelope enters the **declared lane** (ADR-0067) with an
  unchecked stratum tag and **never discharges taint** (ADR-0068's plugin
  discipline): a call covered only by an interop envelope contributes `≤label`
  facts without ever claiming exhaustiveness. Of the two [trust
  models](#trust-models-for-bounds-on-abstractions) this is "hint without
  proof" — coherent for Steins precisely because the checked attribute
  spelling exists above the docblock; a docblock-only checker choosing
  "contract with substitutability" is making the other coherent choice, not
  a conflicting one.
- The declaring function is contract-checked against its interop envelope
  (`effect.envelope-exceeded`) — reading the tag is not believing it, it is
  verifying it.
- A bare `@phpstan-impure` stays a non-tag in Steins (⊤ adds no information);
  a bare `@phpstan-pure` is read as the `{mutate.local}` envelope.
- Within the interop stratum the class/method precedence is upstream's
  nearest-wins rule above; checked (attribute) envelopes continue to conjoin.
- **Unknown labels never reach `effect.unknown-label`** (owner ruling,
  2026-08-12): that id stays attribute-only. An interop tag naming a label
  Steins' registry does not know is read per [Unknown
  labels](#unknown-labels) above — the whole tag is unspecified, and the
  declaration is checked (or, at a call site, contributes) as if it carried
  no tag's *bound*, though the tag still wins its precedence contest. Typo
  reporting for the docblock spelling is deferred to a future, separate,
  opt-in rule.
- Until `mutate.self` narrows Steins' conservative `mutate` coloring of
  property writes (ADR-0055 E2), a pure-declared constructor's own-property
  initialization is admissible rather than a finding — matching the upstream
  fixtures that bless it.
- `steins transform effects-envelope` emits these tags conservatively:
  class-level pure only when every declared method is provenly, exhaustively
  pure; per-method impure bounds only from exhaustive inference;
  non-exhaustive functions get no tag at all. Emission never writes a bare
  tag and never writes per-method `@phpstan-pure`, and it refuses rather than
  touch a site it cannot read faithfully: an existing tag carrying an unknown
  label is left byte-untouched, and a computed bound is never written if it
  would itself contain one.

## Open questions for upstream

1. Should the label vocabulary be fixed in PHPStan, or open the way
   `@phpstan-ignore` identifiers are (with a typo-distance diagnostic against
   a known set)?
2. Is interface-to-implementation propagation of class-level purity tags
   desirable? (PHPStan's `reportMethodPurityOverride` rule already points in
   this direction; today the tags do not propagate.)
3. Does `@phpstan-all-methods-pure`'s void-method exclusion want to carry
   over to a labeled world, where a void method could still usefully declare
   `@phpstan-impure io.output`?
