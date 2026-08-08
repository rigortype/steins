# The Steins User Manual

How to run Steins and shape what it reports. This is the operational half of
the documentation: install it, run it, configure it, wire it into CI, fix it
when it misbehaves. The [handbook](../handbook/README.md) explains what the
analysis *means*, and the [type specification](../type-specification/README.md)
is the binding record when either disagrees with it.

Two things worth knowing before you start:

1. **A bare `check` is proof-only.** It reports what provably breaks at
   runtime and nothing else. Debt reporting — undeclared `@throws`, PHPDoc
   mismatches, effect-envelope violations — is real and abundant in released
   code, so it lives behind a named profile you opt into, never in a first
   run.
2. **Silence is not a safety claim.** Steins printing nothing about a call
   means it could not decide, not that it decided the call is fine.

## Chapters

1. [**Installation and quickstart**](01-installation-and-quickstart.md) —
   the four install channels, your first `steins check .`, reading the
   default surface, the PHP sidecar and `--no-php`, exit codes, and the
   honest limits of the current release.
2. [**CLI reference**](02-cli-reference.md) — every subcommand and every
   flag: `check`, `annotate`, `transform`, `effect-diff`, `doctor`, `mcp`,
   `version`, `license`, the `text`/`json` output modes, and the exit-code
   contract per command.
3. [**Configuration**](03-configuration.md) — the `steins.toml` key-by-key
   reference: discovery, which sections parse strictly and which leniently,
   and how config keys interact with command-line flags.
4. [**Findings**](04-findings.md) — the finding-id catalogue by family,
   the anatomy of a message line, and the proof / contract / mechanics /
   debug layers with the profile that surfaces each.
5. [**Profiles, baseline, and suppression**](05-profiles-and-baseline.md) —
   the five named profiles — the `default ⊂ contracts ⊂ strict` ladder plus
   the `throws-direct` and `pedantic` branches — the
   `.steins-baseline.jsonl` ratchet,
   user profiles in `steins.toml`, inline `@steins-ignore`, `effect-diff`'s
   separate capture loop, and why mechanics ids can never be switched off.
6. [**CI integration**](06-ci.md) — the exit-code contract in CI, install
   channels and the PHP sidecar on runners, the baseline loop and why
   `--set-baseline` never runs in CI, `--format github` for inline
   annotations, `steins doctor` as a preflight, and copy-pasteable
   workflow templates in [`ci-templates/`](ci-templates/README.md).
7. [**Troubleshooting**](07-troubleshooting.md) — `steins doctor` section
   by section, every sidecar failure mode with its fix, and a
   symptom-indexed list of common first-run problems.

## Where the manual ends

- [`docs/handbook/`](../handbook/README.md) — a cover-to-cover walkthrough
  of what Steins proves and why, written for PHP programmers with no
  static-analysis background.
- [`docs/type-specification/`](../type-specification/README.md) — the
  normative specification of what the analysis means.
- [`docs/adr/`](../adr/) — architecture decision records, the binding
  source on any conflict.
