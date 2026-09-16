# Verification

Use this guide when changing CI workflows, rustdoc, inference compatibility, or
CLI dispatch and exit codes. `.github/workflows/ci.yml` defines the CI jobs and
their commands; `.github/workflows/composer.yml` defines the Composer channel.

## Gates that pass without exercising the change

- **`fp-gate`:** the private corpus is untracked and machine-local, so a
  checkout without it silently measures only the public packages. Treat that
  run as partial, and run the private half alongside CI when the change needs
  the full corpus claim.
- **`nsrt`:** `cargo xtask nsrt [DIR]` needs a local `phpstan-src` checkout and
  does not run in CI. Run it alongside CI when the change affects inference
  compatibility.
- **Composer:** `composer.yml` is path-filtered, so a CLI change that breaks
  the Composer channel stays green until the next `composer/**` PR. After
  changing command dispatch or exit codes, dispatch that workflow explicitly.

## Gates that fail on something the diff hides

- **Rustdoc:** the `docs` job rejects a public doc item that intra-doc-links a
  private one. Fix the link; a warning suppression hides the break rather than
  fixing it.
- **Generated tables:** see `docs/agents/mining.md`.
