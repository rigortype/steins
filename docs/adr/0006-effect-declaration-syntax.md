# Effect envelopes are spelled as native attributes; @throws stays Throwable-only

**Amended by ADR-0082**: the refusal of a docblock spelling is narrowed —
upstream's own purity tags, parameterized with effect labels at upstream's
suggestion, are admitted as the *unchecked* interop spelling of an envelope
(one trust stratum below the attribute). No `@steins-*` docblock tag exists
either way; the attribute remains the checked canonical spelling.

Effect envelopes (ADR-0005) are declared with native PHP attributes
(`#[\Steins\Effect(...)]`, sugar like `#[\Steins\Pure]`), not a docblock tag.
Attributes are real, parser-checked syntax referencing autoloadable classes,
and the declarations themselves are refactorable — matching Steins'
native-declaration-over-docblock philosophy. (The original enum-case sketch
for colors is superseded by ADR-0018: effects are hierarchical dot-path
labels with class-constant sugar.) No `@steins-effect` docblock variant: dual spellings are the
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

- ~~The relationship between the effect system and exceptions / non-local
  exits / `exit` / `never`~~ — resolved: checked/unchecked split in
  ADR-0007, type/effect division of labor in ADR-0019.
