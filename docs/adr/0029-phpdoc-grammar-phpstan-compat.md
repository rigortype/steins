# PHPDoc type grammar: phpstan/phpdoc-parser compatibility, own Rust implementation

PHPDoc types become authoritative envelopes (ADR-0001), so a misparsed
docblock is a wrong contract and therefore a false-positive vector — grammar
fidelity is a soundness surface, not a convenience. The grammar users
actually write today is phpstan/phpdoc-parser's; Steins adopts it as
**normative** and implements it in an own `steins-phpdoc` crate.

- **Mago's `phpdoc-syntax` is rejected** for this role: its grammar deviates
  from the de-facto standard (e.g. callable types requiring outer
  parentheses), and one known deviation predicts others. This is independent
  of the ADR-0003 parser adoption — a **two-parser arrangement**: Mago for
  PHP syntax, steins-phpdoc for docblock types, joined at the raw comment
  text Mago's trivia already carries.
- **Compatibility is enforced mechanically, two ways**: the reference
  implementation's test suite is ported as fixtures (compatibility = passing
  the reference's tests — the rigor-rs parity discipline applied to a
  parser), and the PHP sidecar runs the *real* phpstan/phpdoc-parser as a
  differential **oracle in CI** over the fixtures, the corpus, and private
  corpus docblocks. The oracle is test-harness-only: at runtime the Rust
  parser stands alone (the runner stays zero-dep per ADR-0024, and `--no-php`
  keeps phpdoc envelopes).
- **Subset-first**: constructs are implemented in envelope-checking priority
  order (scalars, class names, nullable, unions, `list<T>`, `array<K,V>`,
  shapes, callables, …); anything unparsed yields **no envelope** — silence,
  always the safe side. Upstream grammar evolution is tracked by re-syncing
  the ported test suite, which also keeps the flow-back channel (ADR-0016)
  aligned.
