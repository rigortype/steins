# Verification

Use this guide when changing code, CI, documentation gates, generated
artifacts, or CLI dispatch and exit codes.

## Default

Choose checks that cover the changed behavior, then review the diff. The current
CI surface and commands are defined by `.github/workflows/ci.yml`; the Composer
surface is defined by `.github/workflows/composer.yml`.

## Checks that need extra context

- **`fp-gate`:** a checkout without the private, machine-local corpus silently
  exercises only the public half. Treat that run as partial and run the private
  half alongside CI when the task needs the full corpus claim.
- **`nsrt`:** `cargo xtask nsrt [DIR]` needs a local `phpstan-src` checkout. Run it
  against that checkout when the task changes inference compatibility.
- **Rustdoc:** the docs job rejects public intra-doc links to private items. A
  broken link is a gate failure, not a reason to add a warning suppression.
- **Composer:** `composer.yml` is path-filtered. After changing command dispatch
  or exit codes, dispatch or run the Composer workflow explicitly so that its
  channel is exercised even when the path filter would skip it.

The mining-specific Linux engine and generated-table rules live in
`docs/agents/mining.md`; do not duplicate them here.
