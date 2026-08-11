# Interop envelopes: effect labels on the PHPStan purity tags

**Status: designed, not implemented** (ADR-0082; slices tracked in issue
#303). This document is deliberately self-contained — motivation, grammar,
semantics, and compatibility — so it can be handed to an upstream (PHPStan)
discussion as-is. Steins-specific machinery is confined to the
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
io   io.db   io.fs   io.fs.read   io.fs.write   io.ipc
     io.net  io.net.http   io.process   io.signal
mutate   mutate.local
nondet   nondet.random   nondet.time
output   output.header
```

Because a bound is checked by subsumption, a checker never needs the full leaf
vocabulary to be useful: verifying `@phpstan-impure io` requires only "is every
impure point I found subsumed by `io`?" — a segment-wise string prefix test.

### Bare tags

- A bare `@phpstan-impure` means what it means today: the function is impure,
  nothing said about how — the ⊤ bound. Adding labels only ever *narrows* an
  existing tag's meaning; no existing docblock changes meaning.
- `@phpstan-pure` is the empty envelope — with one deliberate exception:
  writes through by-ref out-parameters whose target is a binding of the
  *calling* frame (`preg_match($p, $s, $matches)`, `sort($rows)`) are
  admissible under pure, because nothing escapes the frame and no caller can
  observe a difference. In Steins vocabulary: pure is the `{mutate.local}`
  envelope. This is the answer to the long-standing by-ref question that
  `hasSideEffects` flags could not express.
- `@phpstan-pure` takes no labels: "pure, except it performs effects" is a
  contradiction. A partially-effectful function is spelled as an impure bound.

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

### Reserved: complement bounds

A future `-except` form ("anything but `io`") is sound — the check inverts to
"is the inferred effect subsumed by an excluded label?" — but is deliberately
not part of v1: the class/method override rule covers the motivating cases,
and the minimal adoption surface stays a string prefix test.

## Backward compatibility

Verified against `phpstan/phpdoc-parser` 2.3.3 and phpstan-src 2.2.x:

- `@phpstan-impure io` parses as the `@phpstan-impure` tag with a
  `GenericTagValueNode("io")` value. PHPStan reads only the tag name, so the
  tag functions exactly as today; the labels are ignored. No
  `phpDoc.parseError`, no `phpDoc.phpstanTag`, no behavioral change for any
  existing or new docblock under current PHPStan.
- The one syntactic hazard is the Doctrine path noted under
  [Grammar](#grammar); the grammar forbids the only spelling that triggers it.
- Every construct here **narrows** an existing tag's meaning or adopts an
  existing tag's implemented semantics. Nothing widens, nothing is redefined,
  no new tag name is introduced.

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
  facts without ever claiming exhaustiveness.
- The declaring function is contract-checked against its interop envelope
  (`effect.envelope-exceeded`) — reading the tag is not believing it, it is
  verifying it.
- A bare `@phpstan-impure` stays a non-tag in Steins (⊤ adds no information);
  a bare `@phpstan-pure` is read as the `{mutate.local}` envelope.
- Within the interop stratum the class/method precedence is upstream's
  nearest-wins rule above; checked (attribute) envelopes continue to conjoin.
- Until `mutate.self` narrows Steins' conservative `mutate` coloring of
  property writes (ADR-0055 E2), a pure-declared constructor's own-property
  initialization is admissible rather than a finding — matching the upstream
  fixtures that bless it.
- `steins transform effects-envelope` emits these tags conservatively:
  class-level pure only when every declared method is provenly, exhaustively
  pure; per-method impure bounds only from exhaustive inference;
  non-exhaustive functions get no tag at all. Emission never writes a bare
  tag and never writes per-method `@phpstan-pure`.

## Open questions for upstream

1. Should the label vocabulary be fixed in PHPStan, or open the way
   `@phpstan-ignore` identifiers are (with a typo-distance diagnostic against
   a known set)?
2. Is interface-to-implementation propagation of class-level purity tags
   desirable? (PHPStan's `reportMethodPurityOverride` rule already points in
   this direction; today the tags do not propagate.)
3. Does `@phpstan-all-methods-pure`'s void-method exclusion want to carry
   over to a labeled world, where a void method could still usefully declare
   `@phpstan-impure output`?
