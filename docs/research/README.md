# Research

Mining and analysis work that feeds the builtin catalog and the effect
taxonomy. Everything here is **non-normative** — it is the evidence a decision
was drawn from, not the decision. The decisions live in
[`docs/adr/`](../adr/README.md) and the semantics in
[`docs/type-specification/`](../type-specification/README.md).

## php-src mining

Extraction over the PHP source tree, used to seed the builtin catalog
(ADR-0014) and to audit the effect-label taxonomy (ADR-0008, ADR-0018).

- **[Effect-label taxonomy audit](phpsrc-mining/effects_gaps.md)** — vocabulary
  gaps between what php-src actually does and the labels Steins recognises.

The directory also carries the raw extraction outputs and the scripts that
produced them — `extract_hierarchy.py`, `hierarchy.toml`, `throws.toml`,
`failure_arms.toml`, `return_facts.toml`, and `crosscheck.txt`. Those are data,
not prose; they are read by the catalog generator (`cargo xtask gen-catalog`),
and only the audit above is written to be read directly.

## PHPDoc runtime check

A feasibility spike for a runtime checker of PHPDoc types against values,
built on `phpstan/phpdoc-parser` only — the evidence behind
[the 2026-09-07 note](../notes/20260907-phpdoc-runtime-contract-checker.md).

- **[`phpdoc-runtime-check/spike.php`](phpdoc-runtime-check/spike.php)** —
  the interpreter (trinary verdict), the contract reader, the generator and
  the shrinker in one file. Run with `composer require phpstan/phpdoc-parser`
  next to it, or `PHPDOC_PARSER_AUTOLOAD=<vendor/autoload.php> php spike.php`;
  the output is deterministic (seed 7).
