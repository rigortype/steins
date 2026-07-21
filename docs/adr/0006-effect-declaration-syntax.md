# Effect envelopes are spelled as native attributes; @throws stays Throwable-only

Effect envelopes (ADR-0005) are declared with native PHP attributes
(`#[\Steins\Effect(...)]`, sugar like `#[\Steins\Pure]`), not a docblock tag.
Attributes are real, parser-checked syntax referencing autoloadable classes —
colors can be enum cases rather than strings, and the declarations themselves
are refactorable — matching Steins' native-declaration-over-docblock
philosophy. No `@steins-effect` docblock variant: dual spellings are the
phpdoc sprawl we exist to prevent. Third-party code needs no in-source
spelling — its envelopes come from the effect catalog / stubs.

The `throw` effect keeps its existing spelling: `@throws` (Throwable classes /
interfaces only) is read as the envelope for the throw color, and writing
throw inside `#[\Steins\Effect]` is **rejected** — one color, one spelling.
`#[\Steins\Pure]` does not forbid throwing (pure computations throw — division,
`JsonException`); it means "all colors empty except throw," Koka's
total-vs-exn split.

## Considered options

- **Docblock tag (`@steins-effect`)** — would interoperate better if a
  hypothetical "PSR-Effect" standard ever emerged; rejected as speculative
  (no consensus path visible) and as reintroducing docblock sprawl.

## Open questions

- The relationship between the effect system and exceptions / non-local
  exits / `exit` / `never` (type-and-effect interplay) needs its own design
  round, including a checked/unchecked-style split (cf. PHP's `LogicException`
  documented as "errors that should be detected at compile time").
