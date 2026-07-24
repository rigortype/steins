# Profiles and baseline

The default `check` surface is proof-only on purpose. Strictness in Steins
is not a numeric dial — it is a set of **named stages** a project declares
for itself and ratchets through as it modernizes. This page covers the three
built-in stages, the baseline round-trip that makes raising a stage
survivable, and the config that ties them to a repo.

## The lenient-default principle

Defaults are lenient; strictness is opt-in, expressed as named stages a
project declares in config (ADR-0050). A project's appetite for debt
reporting tracks its modernization stage, so the tool never guesses it — the
repo declares it, reviewably.

## The three named stages

A profile is a named selection over diagnostic *layers* — `proof` (provable
runtime break), `contract` (a proven behavior violates something the code
*declares* about itself), and `mechanics` (the analyzer's own hygiene).
`mechanics` prints in every profile; the stages differ in how much of the
contract layer they surface.

- **`default`** — proof + mechanics. Only what provably breaks, plus
  anti-rot. This is a bare `check`.
- **`throws-direct`** — default plus `throw.undeclared` findings whose escape
  originates in the annotated declaration's *own body* (`origin = direct`).
  The high-signal subset: a `@throws` that is wrong about the method you are
  reading.
- **`contracts`** — default plus the whole contract layer: every
  `throw.undeclared` (direct and propagated), `phpdoc.*` mismatches, and
  effect-envelope violations.

The counts climb as the surface widens. Kimai's `src/` tree, same code, the
three stages in order:

```
$ steins check src/                         # default → exit 0, clean
$ steins check --profile throws-direct src/ # exit 1
src/Invoice/Renderer/AbstractSpreadsheetRenderer.php:148:13: error[throw.undeclared]: Exception can escape AbstractSpreadsheetRenderer::addTemplateRows() but is not declared (@throws Exception) — proven escape
src/Timesheet/TimesheetService.php:149:13: error[throw.undeclared]: Exception can escape TimesheetService::saveNewTimesheet() but is not declared (@throws ValidationFailedException|InvalidArgumentException|AccessDeniedException) — proven escape
$ steins check --profile contracts src/     # exit 1
… the two above, plus:
src/Timesheet/TimesheetService.php:149:13: error[throw.undeclared]: Exception can escape TimesheetService::restartTimesheet() but is not declared (…) — proven escape
```

`default` 0 findings, `throws-direct` 2, `contracts` 3 — the third is a
*propagated* escape (`restartTimesheet` re-throws through a call), which
`throws-direct` holds back and `contracts` surfaces.

## The ratchet workflow

Named stages are only usable if raising one does not bury you. The pattern:

1. **Adopt on `default`.** Get a clean, proof-only run.
2. **Capture a baseline** at the stage you want to move to, freezing today's
   debt so only *new* debt fails CI:

   ```
   $ steins check --profile throws-direct --set-baseline --baseline steins-baseline.jsonl src/
   steins: wrote 2 baseline entries to steins-baseline.jsonl (profile `throws-direct`)
   ```

   The flag is `--set-baseline` (writes) paired with `--baseline <path>`
   (locates the file). The file is machine-managed JSONL — a header line
   recording the capture surface, then one `{"id","path","hash"}` entry per
   finding; no line numbers, so it does not rot on unrelated edits
   (ADR-0022). Do not hand-edit it.

3. **Raise the profile** in config (below) and re-run with the baseline. A
   fully-baselined run is clean:

   ```
   $ steins check --profile throws-direct --baseline steins-baseline.jsonl src/
   2 findings in baseline
   ```

4. **It drowns loudly, never silently.** If you raise the profile *past* the
   surface the baseline was captured under, the new findings are unbaselined
   and the run says exactly that:

   ```
   $ steins check --profile contracts --baseline steins-baseline.jsonl src/
   src/Timesheet/TimesheetService.php:149:13: error[throw.undeclared]: … saveNewTimesheet() … — proven escape
   2 findings in baseline
   active profile `contracts` surfaces 7 id(s) the baseline (captured under `throws-direct`) did not — those findings are unbaselined (rerun --set-baseline to capture them)
   ```

5. **Burn down, then shrink.** Fix the frozen findings, re-run
   `--set-baseline` to recapture. The baseline only ever shrinks; a zero-entry
   baseline means the stage is fully paid off. `--ignore-baseline` shows the
   full unfiltered surface without touching the file.

## User profiles in steins.toml

Built-in stages cover the common ladder; a repo composes its own named
surfaces in `steins.toml` at the project root. Config carries intent —
ad-hoc `--enable id,id` flags are refused because an unnamed surface is
unreviewable in CI history. A worked example, a migration stage that
surfaces the whole contract layer but keeps `throw.*` warn-only:

```toml
[check]
profile = "migration"

[profile.migration]
extends = "contracts"
warn    = ["throw.*"]
```

`extends` names a built-in or another user profile; `warn`/`disable`/`enable`
take prefix id-arrays (`throw.*`). Running under this config, a
`throw.undeclared` escape surfaces as `warning`, and the run exits `0`:

```
$ steins check src/
src/App.php:7:9: warning[throw.undeclared]: RuntimeException can escape Svc::run() but is not declared (@throws LogicException) — proven escape
$ echo $?
0
```

An explicit `--profile default` on the command line overrides the config's
`profile = "migration"` (invocation intent beats repo default).

## Exit-level semantics

Every surfaced finding carries a level, `fail` by default in every layer: if
a profile put it on the surface, it was asked for, and CI must see it. A
profile's `warn = [...]` demotes matching ids to report-without-fail.

- `0` — nothing fail-level displayed. **A warn-only run exits `0`** — that is
  what `warn` means.
- `1` — a fail-level finding was displayed.
- `2` — usage or config error (unknown profile, `extends` cycle, unknown
  name).

## Mechanics ids always print

`suppress.unmatched`, `suppress.unknown-id`, `effect.unknown-label` are the
**mechanics** layer: findings whose *absence* would silently rot another
channel. They print in every profile, are exempt from every suppression
channel, and default to `fail` — a stale suppression must bite CI or it
never gets cleaned up.

## Inline `@steins-ignore`

Suppress a single finding at its site with a comment naming the id:

```php
// @steins-ignore type.argument-mismatch
takesInt("abc");
```

The ignore is **anti-rot**: if it matches nothing (the code was fixed, or the
id was wrong), it does not fail quietly — it reports `suppress.unmatched` and
fails the run, so dead ignores get removed:

```
a.php:7:1: error[suppress.unmatched]: @steins-ignore of call.on-null matches no diagnostic on line 8
1 diagnostics suppressed by inline ignores
```

## The dump ids (landing in v0.1.0)

ADR-0053 adds a fourth `debug` layer for *requested introspection* —
`debug.type` from an explicit `PHPStan\dumpType()` (fail-level: the call is a
runtime fatal, so a committed one reds CI), `debug.phpdoc-type` from
`dumpPhpDocType()`, and `debug.var-dump` reporting the engine's inferred
facts at every default-on `var_dump()` call (warn-level, structurally
exit-neutral — a leftover `var_dump` is legal working PHP, and going red on
it would invert the quiet-default identity). Dumps are exempt from all three
suppression channels: the question is in the source, and the remedy is
deleting the call.

This lane is specified but **not yet emitting in the v0.1.0 binary**: as of
this build, `dumpType()` and `var_dump()` produce no dump output from
`check`. The exit postures above (fail for the explicit pair, exit-neutral
warn for `var_dump`) are what the D3/D4 slices deliver when they land; treat
this section as the contract, not yet the behavior.
