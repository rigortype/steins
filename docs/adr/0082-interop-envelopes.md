# Interop envelopes: parameterized purity tags as the unchecked docblock spelling of effect envelopes

Issue #303. Status: **accepted 2026-08-11, owner-ratified** (settled in a live
grilling session with the owner; every decision below is an owner ruling, not a
delegated draft). **Amends ADR-0006** — the refusal of a docblock effect syntax
is narrowed, not repealed. **Consumes ADR-0067/0068** — the declared lane and
the trust-stratum vocabulary are used as designed there, not extended.

## Problem

PHPStan's author proposed, verbatim (2026-08-09, in the orbit of
[phpstan#14220](https://github.com/phpstan/phpstan/issues/14220), where he
asked for concrete code samples):

> The effect could just be a parameter after @phpstan-impure PHPDoc tag.
> Like @phpstan-impure io

Steins has an effect system — hierarchical dot-path labels, envelopes, prefix
subsumption — and PHPStan has a flat purity model (`ImpurePoint`s and a
tri-state `isPure`). The project's standing goal is to prototype in Steins what
PHPStan later adopts. A parameterized `@phpstan-impure` is the first concrete
bridge: the tag already exists, the parameter position is upstream's own
suggestion, and current `phpstan/phpdoc-parser` (verified against 2.3.3) parses
`@phpstan-impure io` as a `GenericTagValueNode` — the trailing text is
harmlessly discarded, the tag keeps its current meaning, no diagnostic fires.

Two standing Steins decisions block the naive path:

- **ADR-0006**: effect envelopes are spelled as native attributes; a docblock
  variant was rejected as "the phpdoc sprawl we exist to prevent" — one color,
  one spelling.
- The unconditional `@phpstan-pure` / `@phpstan-impure` flags are deliberately
  unread (a negative test enforces it): a bare metadata-only purity flag is the
  lie upstream rejected twice.

Separately, the owner ruled (2026-08-08) that Steins' refined-string vocabulary
does not bend toward PHPStan. That ruling stands untouched: this ADR is about
effect tags co-designed with upstream, a different layer entirely.

## Decision

1. **The interop envelope.** `@phpstan-impure <label-list>` (and
   `@phpstan-pure`, and the class-level tags of §5) is the **unchecked docblock
   spelling of an effect envelope** — an interop surface, not a second
   canonical spelling. `#[\Steins\Effect]` / `#[\Steins\Pure]` remain the
   checked stratum. ADR-0006's one-spelling principle is preserved by
   *stratifying* the spellings rather than duplicating them: the two forms
   differ in trust, and the analyzer treats them differently everywhere it
   matters. Interop envelopes take exactly the landing spot ADR-0067's
   rejected-alternatives section predicted for phpdoc bounds: they enter the
   **declared lane** carrying an unchecked stratum tag, and they **never
   discharge taint** — the same discipline ADR-0068 applies to plugin facts.

2. **Two roles, both on.** An interop envelope contributes at call sites
   exactly as a checked envelope does (an opaque call covered by one feeds the
   caller's declared lane), *and* the declaring function is contract-checked
   against it: inference exceeding the declared bound is
   `effect.envelope-exceeded`. Reading the tag is not believing it — it is
   taking it as a checkable claim. This keeps the spirit of the negative test:
   what was refused was an unverifiable metadata flag, and what is admitted is
   a bound the analyzer verifies.

3. **Bare tags.** A bare `@phpstan-impure` is ⊤ — every effect possible — which
   is exactly what the absence of information already means, so it stays a
   **non-tag**: the existing negative test survives unchanged for it. A bare
   `@phpstan-pure` does carry information — it is the `{mutate.local}` envelope
   (the degenerate member every envelope tolerates, ADR-0063 §2.3) — and **is
   read**. The pure side of the negative test evolves accordingly; the impure
   side does not.

4. **Grammar and vocabulary.** The label list reuses `@phpstan-ignore`'s
   list-and-comment shape — comma-separated dot-path identifiers, one optional
   parenthesized trailing comment — with one constraint learned from the
   parser: the comment is only legal **after at least one label**, because a
   `(` directly after the tag name sends phpdoc-parser down its Doctrine
   annotation path, which can produce `phpDoc.parseError`. Dot paths and
   prefix subsumption are part of v1 — they are the substance of the proposal,
   the difference from a flat impure-point enumeration. The v1 vocabulary is
   the builtin registry **minus `failure.*`**, which names value provenance,
   not an effect, and would only confuse the interop surface.

5. **Class-level tags are adopted from upstream, semantics and all.**
   `@phpstan-all-methods-pure` / `@phpstan-all-methods-impure` exist in PHPStan
   (since 2.1.39, phpstan-src#4422). Steins adopts them as interop envelopes
   with **upstream-faithful semantics**, verified against phpstan-src 2.2.x:
   a method-level tag always wins (the class tag is a fallback, not a
   conjunct); the constructor is covered by `all-methods-pure` (upstream
   fixtures accept property-initializing pure constructors — see Deferral
   below); `all-methods-pure` does not cover void-returning methods while
   `all-methods-impure` covers everything; the tag does not propagate to
   subclasses' own declarations or from interfaces to implementations.
   Accepted spellings mirror upstream's exactly — no `@psalm-impure`, no
   aliases for the class-level pair. Within the *checked* stratum, envelopes
   continue to conjoin (Liskov, ADR-0033); the nearest-wins rule is a property
   of the interop stratum alone, because it is upstream's documented contract
   for upstream's own tags, and rewriting the semantics of someone else's
   implemented tag is not "interop".

6. **`-except` is reserved, not implemented.** A complement bound ("anything
   but `io`") is sound and checkable, but upstream's method-over-class override
   already covers the motivating use case (a class-wide claim with explicit
   per-method exceptions), and a positive-set-only v1 keeps PHPStan's minimal
   adoption surface small. The spec names the idea and reserves the syntax.

7. **Emission: read faithfully, write conservatively.** A new transform,
   `effects-envelope` (sister of `throws-envelope`), writes interop envelopes.
   The reading side mirrors upstream semantics quirks included; the writing
   side is stricter than the tags require: a class-level pure tag only when
   *every* declared method (void ones included, constructor under the
   admissibility rule) is provenly and exhaustively pure; a per-method
   `@phpstan-impure <labels>` only when inference is exhaustive; a
   non-exhaustive function gets **nothing** — a bare ⊤ tag is docblock litter,
   and PHPStan's default assumption already covers it. No per-method
   `@phpstan-pure` is ever written. This is the owner's docblock-restraint
   ruling applied: Steins stays silent exactly where writing would be a lie or
   a no-op.

## Considered and rejected

- **Export-only interop** (write the tags, never read them) — keeps ADR-0006
  and the negative test byte-identical, but a proof of concept that cannot
  consume its own output is not a design PHPStan can adopt, and ADR-0067 had
  already priced in the read path.
- **Conjunction semantics for the class-level tags** — more principled
  (Liskov everywhere), but it contradicts upstream's documented and
  implemented override rule for upstream's own tags. Recorded as the one place
  the two strata deliberately differ.
- **Constructor exemption from `all-methods-pure`** — cleaner given Steins'
  current conservative `mutate` coloring of property writes, but upstream
  includes the constructor and its fixtures bless property-initializing pure
  constructors. Fidelity wins; the gap becomes a deferral, not a divergence.
- **A third lane or a new tag family** — both were pre-rejected by
  ADR-0067/0068; nothing here required revisiting them.

## Consequences

- ADR-0006 gains an amended-by pointer; its rejection of `@steins-effect`
  stands — no Steins-named docblock tag exists or will. What changed is that
  *upstream's* tags, parameterized at upstream's own suggestion, are now an
  admitted unchecked spelling.
- The `docblock.rs` negative test is split by §3: bare `@phpstan-impure`
  remains a non-tag; bare `@phpstan-pure` and parameterized forms become
  tags. `not-implemented.md` and `phpdoc-grammar.md` are reconciled in this
  slice; the spec lands as designed-not-implemented until the slices close.
- **Deferral (tracked in #303):** until `mutate.self` lands (ADR-0055 slice
  E2), a pure-declared constructor's writes to its own properties are treated
  as admissible rather than as `mutate` — otherwise every property-initializing
  constructor under `all-methods-pure` would be a false
  `effect.envelope-exceeded` against semantics upstream explicitly accepts.
- The full grammar, semantics, and upstream-facing rationale live in
  [phpdoc-effects-interop.md](../type-specification/phpdoc-effects-interop.md),
  written as a standalone document pasteable into an upstream discussion.
