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
