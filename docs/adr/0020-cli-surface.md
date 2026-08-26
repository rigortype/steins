# CLI surface: six commands, two deliberate absences

Initial command set: `check` (proof layer by default, `--profile` adds
policy, `--set-baseline`/`--fix` as flags), `annotate` (Rigor-style margin
display of inferred types *and* effect labels — the one-screen proof that
annotation restraint works), `transform` (dry-run by default, `--apply`
writes; the consult-rector loop), `doctor` (coverage posture, sidecar
health, catalog audit), `lsp` (stdio server), `mcp` (agent surface).
Output formats `text|json|sarif|github` with CI auto-detection from the
start.

Two deliberate absences:

- **No `fix` command.** Fix-its are diagnostic payloads (ADR-0010), exits
  via `check --fix` and LSP/MCP — a standalone `fix` would wear a linter's
  face and blur ADR-0017's boundary.
- **No `init` command.** Zero-config operation is the banner: everything is
  inferred from composer.json and autoload. Needing setup is losing.
  Instead, a later step adds **Skill-driven initialization** in the Rigor
  manner — an agent skill walks a project through adoption
  conversationally, rather than a config generator.

**Amendment (2026-08-26, issue #525): the generation cache is default-on,
opt out with `--no-cache`, and says nothing.** ADR-0092's frozen-generation
lifecycle reached `steins check` behind `STEINS_EXPERIMENTAL_GENERATIONS=1`,
deliberately deferring the surface decision to this ADR. The decision:

- **On by default for `check`.** The project is pre-release; there is no
  installed base to stage this for. `annotate`, `transform` and `doctor`
  keep their present paths in this slice, and `mcp` is issue #491's.
- **`--no-cache` is the opt-out**, and the environment variable is gone
  rather than kept as a second spelling. ADR-0092 §2 already calls the
  artifacts a cache in its own standing invariant, so that is the honest
  user-facing word; the flag parallels `--no-php` (both switch off a
  capability the run would otherwise use), and zero-config is this ADR's
  banner — a flag is discoverable where an environment variable is not.
- **Silence.** The per-run stderr ledger is removed. Every disposition a
  cache can have is cost-only by §2's invariant — a miss changes what a run
  pays, never what it finds — so narrating one is noise the reader cannot
  act on, on every invocation, forever. That includes the degradations: an
  unwritable project, a corrupt artifact, a lost publish, a source that
  moved under the seal all fall back to a cold run whose stderr is
  byte-for-byte the stderr of a machine that never had a store.
- **The disposition moves to `doctor`**, which gains a store section: whether
  a store exists, the published generation, its package count, its size on
  disk and generation count, and the persistent reasons a run could not use
  it. That is where a posture question is asked deliberately.
- **`.steins/.gitignore` holding `*`, written at store creation**, the way
  Cargo does for `target/`. A cache the tool writes unasked must not become
  a commit the user did not ask for.

The zero-FP gate follows the product: `cargo xtask fp-gate` now analyzes every
corpus project through the orchestrator — cold, then warm over what the cold
pass published, asserting the two agree — because a release gate guarding a
code path the product no longer takes guards nothing.

**Implementation note (2026-08-05, issue #114).** The `--fix` reservation
is implemented, with its first fix family: dump-statement removal for the
explicit dump pair (`debug.type` / `debug.phpdoc-type`, ADR-0053). The
findings carry the deletion as a first-class payload (ADR-0010), `check
--fix` applies it, and the write is gated by the transform engine's
zero-new-diagnostics post-check (ADR-0034). `debug.var-dump` ships no fix
— deleting legal working PHP is a judgment call, not a mechanical remedy.
Further families ride later slices.
