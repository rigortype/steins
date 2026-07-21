# Corpus harness as xtask: pinned lock, red-on-any-finding gate

The verification apparatus (ADR-0013) materializes as an `xtask` workspace
member — dev tooling invoked as `cargo xtask <cmd>`, consuming the analysis
crates as libraries (full access to parse errors and call data; no CLI
shelling).

- **`corpus-sync`** — the ADR-0021 packages as shallow clones pinned by a
  committed `corpus.lock.toml` (url/tag/commit per package; `corpus/` itself
  gitignored). Measurements are reproducible against recorded commits;
  re-resolution is explicit (`--update`), never implicit.
- **`fp-gate`** — runs the pipeline over the whole corpus and exits nonzero
  on **any** diagnostic: one proof-layer finding on working code is triage
  material by definition, so the gate goes red and a human looks. Every
  finding is printed verbatim — the gate never summarizes findings away.
  Parse-error files are statistics, not failures (corpora contain
  intentionally-broken fixtures), but they are reported: the corpus doubles
  as a standing parser-regression check on top of the spike.
- **`freq`** — builtin-call frequency over the corpus, written to
  `docs/notes/` as the measured seeding order for catalog coloring
  (ADR-0021). Counts are an upper bound (unresolved cross-file userland
  calls included) until real symbol resolution lands, and say so in the
  report header.

The discipline this buys: from now on, **every inference extension merges
against a green fp-gate** — the Rigor no-regression claim ("zero findings on
the corpus") becomes a one-command check before any commit that touches
steins-infer.
