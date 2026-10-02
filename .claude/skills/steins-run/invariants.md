# Invariants every slice keeps

The checklist implementers build against and reviewers audit against. A slice
that changes one of these says so in its PR body.

## Repository rules

- Rust is hand-formatted to `rustfmt.toml` (width 100); match the surrounding
  code by hand. `cargo fmt` rewrites the tree, so it stays unused.
- Functions stay under `too-many-lines-threshold` in `clippy.toml`.
- New module families use the 2018 layout: `foo.rs` beside `foo/*.rs`.
- A user-visible change gets a fragment under `changelog.d/` per its README.
- Vocabulary follows `CONTEXT.md` (`docs/agents/domain.md`); a rename updates
  the glossary first.
- Gates: `cargo clippy --workspace --all-targets -- -D warnings`,
  `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`,
  `cargo test -p xtask changelog`, and the targeted tests of every touched
  crate. Redirect test output to a file and read `FAILED` and `test result`
  lines from it; a pipe into `tail` reports `tail`'s exit status.

## Persisted formats

- The `symbols` shard (`PackageShard`) and the `summaries` rows are decoded
  before the analyzer gate: changing their types bumps `SCHEMA_VERSION` in
  `crates/steins-gen/src/container.rs`, with a row and a note in
  `docs/internal-spec/generation-schema.md`.
- Trace and facts payloads are decoded past the gate: no bump
  (`crates/steins-db/src/wire.rs`).
- The wire codec reads enum variants by index and struct fields by position.

## Analysis

- Zero-FP means calibrated defaults: every cause of unsoundness is detected;
  tolerance is absorbed by plugins, ignores and strict-tier floors.
- An engine-side wall-clock budget is never added; cutoffs are structural and
  widen, and they name themselves.
- Findings stay deterministic: cold equals warm, and output order is stable.

## Measurement hygiene

- A comparison asserts the binaries differ and the B side is non-empty.
- One `steins check` per path at a time; `--no-cache` for A/B; `timeout` on
  long runs; a separate `CARGO_TARGET_DIR` per worktree.
- Public corpora live under `corpus/` (read-only); never symlink
  `corpus.lock.toml`.

## Privacy

- The private corpus is the local project in `corpus.local.toml`, read-only.
  Report aggregates only: counts, wall time, exit code, peak RSS.
- Fixtures are synthetic minimal reproductions with synthetic names.
- Code, comments, fixtures, commit messages, issues and PR bodies carry no
  local absolute path, private project or symbol name, or employer identifier.
- Commit messages are English, in the `git log` style
  (`<crate>: <claim sentence> (#issue)`), with no attribution trailers.
