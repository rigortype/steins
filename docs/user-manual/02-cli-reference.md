# CLI reference

Every subcommand, every flag it accepts, and what each exit code means.
Verified against **steins 0.1.2** (`steins version`); the transcripts below
are real output from that binary. For getting the binary in the first place,
see [installation and quickstart](01-installation-and-quickstart.md).

## How to read this page

A synopsis line spells one invocation:

```
steins check [--format text|json] [--profile <name>] <paths...>
```

Square brackets mark optional flags. `<name>` is a value you supply.
`a|b` is a closed choice. `<paths...>` takes one or more files or
directories; a directory is walked recursively for `.php` files, and every
path in one invocation forms a single project, so cross-file calls and class
chains resolve.

There is **no `--help`**. Run the binary with no arguments and it prints the
whole surface to stderr and exits `2`:

```
$ steins
usage: steins check [--format text|json] [--profile <name>] [--no-php] [--vendor-diagnostics] [--fix] [--set-baseline] [--baseline <path>] [--ignore-baseline] <paths...>
       steins annotate [--no-php] [--format text|json] <file.php>
       steins transform <phpdoc-to-native|phpdoc-honesty> [--apply] [--format text|json] <paths...>
       steins effect-diff [--baseline <path>] [--set-baseline] [--format text|json] <paths...>
       steins doctor [--no-php] [--baseline <path>] [path]
       steins version | -v | --version
       steins license
$ echo $?
2
```

An unrecognized subcommand names the seven that exist:

```
$ steins lsp
steins: unknown command `lsp` (available: check, annotate, transform, effect-diff, doctor, version, license)
$ echo $?
2
```

The command set is ADR-0020's; `lsp` and `mcp` are designed and not yet
shipped.

### Exit codes

`2` always means "the invocation or the configuration is wrong" — a bad
flag, a missing flag argument, an unknown profile, an unparseable
`steins.toml`, a path that names nothing. It never means "your code has a
problem". The other two codes vary by subcommand:

| Subcommand | `0` | `1` | `2` |
| --- | --- | --- | --- |
| `check` | nothing fail-level displayed | a fail-level finding displayed | usage or config error |
| `annotate` | file printed | — | usage or read error |
| `transform` | plan produced (and written, under `--apply`) | post-check found new diagnostics, or a write failed | usage error |
| `effect-diff` | report produced, deltas or none | — | usage error, or an unreadable/unparseable baseline |
| `doctor` | posture reported, degraded ones included | configuration contradiction | usage error |
| `version` | always | — | — |
| `license` | always | — | — |

Two rules cut across all seven. A path argument that names nothing is a
usage error everywhere, checked before any output, so a renamed directory
reds the build instead of producing a clean empty report (ADR-0050 §7,
ADR-0054 §10). And a hard stdout write failure forces `1` regardless of the
command's own verdict, while a closed reader — `steins license | head` —
leaves the exit code alone.

### stdout and stderr

Findings, reports, diffs, and JSON documents go to **stdout**. Notices,
warnings, usage errors, and the confirmation lines from `--set-baseline` and
`--apply` go to **stderr**. Piping stdout to a file therefore captures the
report and leaves the commentary on your terminal.

### The examples

Transcripts on this page run against one demo project — a `composer.json`
declaring `"php": "^8.3"` and PSR-4 `Acme\` → `src/`, five source files, and
an installed package under `vendor/`:

- `src/Greeter.php` — `Greeter::greet(string $name)` called with `null`, a
  proven `TypeError`.
- `src/Invoice.php` — `Invoice::total()` declares `@throws \LogicException`
  and throws `RuntimeException`.
- `src/Counter.php` — `Counter::bump()` carries `#[\Steins\Pure]` and
  `echo`es.
- `src/Formatter.php` — a PHPDoc-typed `pad($label, $width)` with one caller.
- `src/Ids.php` — `@param int $id` on a function one caller passes `'42'`.
- `vendor/acme/lib/src/Legacy.php` — a second proven `TypeError`, in
  somebody else's code.

---

## `check`

Analyze a tree and report findings. This is the command you run in CI.

```
steins check [--format text|json] [--profile <name>] [--no-php]
             [--vendor-diagnostics] [--fix] [--set-baseline]
             [--baseline <path>] [--ignore-baseline] <paths...>
```

| Flag | Default | Effect |
| --- | --- | --- |
| `--format text\|json` | `text` | Output mode. |
| `--profile <name>` | `[check] profile`, else `default` | Select the display surface — a built-in stage or one named in `steins.toml`. |
| `--no-php` | off | Skip the PHP sidecar and run the sound subset. |
| `--vendor-diagnostics` | off | Report findings inside vendor trees too. |
| `--fix` | off | Apply the fixes findings carry, post-check-gated — see [`--fix`](#--fix). |
| `--baseline <path>` | `.steins-baseline.jsonl` when it exists | Locate the baseline file. |
| `--set-baseline` | off | Write the baseline instead of reporting; exits `0`. Cannot combine with `--fix`. |
| `--ignore-baseline` | off | Report the full surface, consulting no baseline file. |

`<paths...>` is required — `steins check` with none prints
`steins: no paths given` and exits `2`.

Suppression composes in a fixed order: vendor, then the profile surface,
then inline `@steins-ignore`, then the baseline. A finding removed by an
earlier channel never reaches — nor consumes — a later one. Profile
semantics and the baseline round-trip belong to
[profiles, baseline, and suppression](05-profiles-and-baseline.md); this
page covers only the flags.

A default run over the demo project. One proof-layer finding, and the vendor
package's own break counted and withheld:

```
$ steins check .
./src/Greeter.php:16:22: error[type.argument-mismatch]: argument null to Greeter::greet() cannot become string $name — proven TypeError (coercive mode)
1 findings in vendor suppressed (--vendor-diagnostics to show)
$ echo $?
1
```

`--vendor-diagnostics` sends vendor findings through the normal channels:

```
$ steins check --vendor-diagnostics .
./src/Greeter.php:16:22: error[type.argument-mismatch]: argument null to Greeter::greet() cannot become string $name — proven TypeError (coercive mode)
./vendor/acme/lib/src/Legacy.php:15:33: error[type.argument-mismatch]: argument null to Legacy::name() cannot become string $s — proven TypeError (coercive mode)
```

`--profile contracts` widens the surface to the contract layer:

```
$ steins check --profile contracts .
./src/Counter.php:12:9: error[effect.envelope-exceeded]: echo has effect output, but Counter::bump() is declared #[\Steins\Pure]
./src/Greeter.php:16:22: error[type.argument-mismatch]: argument null to Greeter::greet() cannot become string $name — proven TypeError (coercive mode)
./src/Ids.php:16:11: error[phpdoc.param-mismatch]: argument "42" to label() violates declared @param int $id — declared contract violation
./src/Invoice.php:15:13: error[throw.undeclared]: RuntimeException can escape Invoice::total() but is not declared (@throws LogicException) — proven escape
1 findings in vendor suppressed (--vendor-diagnostics to show)
```

`--no-php` prints the sound-subset notice on stderr first, then analyzes
without executing any PHP:

```
$ steins check --no-php .
note: running as sound subset (no PHP sidecar) — findings that require executing PHP are omitted, and builtin return types come from the catalog's declarations, unverified
./src/Greeter.php:16:22: error[type.argument-mismatch]: argument null to Greeter::greet() cannot become string $name — proven TypeError (coercive mode)
1 findings in vendor suppressed (--vendor-diagnostics to show)
```

> **If you know PHPStan or Psalm:** `--set-baseline` writes what
> `phpstan-baseline.neon` holds, and `--baseline <path>` is
> `includes: [phpstan-baseline.neon]`. Two differences matter. The file is
> machine-managed JSONL keyed by a content hash rather than a line number,
> so it does not rot on unrelated edits, and there is no `--generate-baseline`
> spelling — the same `--baseline` flag locates the file for reading and for
> writing, and `--set-baseline` decides which.

### `--fix`

Some findings carry their remedy as a first-class payload (ADR-0010), and
`--fix` applies it. One fix family exists today: a committed
`\PHPStan\dumpType()` / `\PHPStan\dumpPhpDocType()` statement — `debug.type`
and `debug.phpdoc-type`, whose only remedy is deleting the call — is removed
whole (the entire expression-statement, its line when nothing else shares
it). `debug.var-dump` is deliberately not fixable: a `var_dump()` is legal
working PHP, and deleting it is your call, not the tool's.

```
$ steins check src/Dump.php
src/Dump.php:4:19: error[debug.type]: dumped type: 'POST'
$ steins check --fix src/Dump.php
src/Dump.php:4:19: fixed[debug.type]: dumped type: 'POST'
steins: fixed 1 finding(s) (1 file(s) written)
$ echo $?
0
$ steins check src/Dump.php
$ echo $?
0
```

The write is gated by the same dual-verification post-check `transform
--apply` runs (ADR-0034): the edited project is re-analyzed, and unless
every diagnostic id's count is unchanged or lower, the whole write is
refused by name and nothing touches disk. Today's one family cannot
actually trip that gate, and that is why it went first — a recognized dump
is transparent (ADR-0053: it reads facts and binds nothing), so deleting
its statement cannot change what the rest of the file proves. The gate is
what will let the families that follow be less obviously riskless. When one
does refuse, the named reason and the diagnostics the edits would have
surfaced print on stdout, and the reason again on stderr:

```
fix refused (postcheck-new-diagnostics): applying the fixes would surface 1 new diagnostic(s)
  src/Example.php:4:1: [call.on-null] method call $x->m() — $x is proven null on this path — proven Error (Call to a member function on null)
steins: fix refused (postcheck-new-diagnostics): applying the fixes would surface 1 new diagnostic(s)
```

A fixed finding leaves the exit computation — it no longer exists on disk,
so it cannot double as a surviving finding in the same run. A refused (or
fixless) `--fix` run reports and exits exactly like a plain run. Without
`--fix`, `check` behaves byte-identically to before; the one additive
surface is the JSON payload below.

### `--format json`

The document is one object. `findings` is an array, sorted by path, line,
column, and id; the run-level counts sit beside it:

```
$ steins check --format json .
{
  "findings": [
    {
      "id": "type.argument-mismatch",
      "layer": "proof",
      "level": "fail",
      "path": "./src/Greeter.php",
      "line": 16,
      "column": 22,
      "message": "argument null to Greeter::greet() cannot become string $name — proven TypeError (coercive mode)"
    }
  ],
  "profile": "default",
  "vendor_suppressed": 1,
  "suppressed": 0,
  "baselined": 0
}
```

Per finding:

| Field | Type | Meaning |
| --- | --- | --- |
| `id` | string | The finding id, e.g. `type.argument-mismatch`. |
| `layer` | string | `proof`, `contract`, `mechanics`, or `debug`. |
| `level` | string | `fail` or `warn` — whether it counts toward exit `1`. |
| `path` | string | The path as given on the command line, not canonicalized. |
| `line`, `column` | number | 1-based position. |
| `message` | string | The same text the `text` mode prints after the id. |
| `origin` | string | Present only on `throw.undeclared`: `direct` or `propagated`. |
| `fix` | object | Present only on findings that carry a fix payload (today: `debug.type`, `debug.phpdoc-type`). |

`origin` is the one facet v1 defines (ADR-0050 §4); a finding whose id
declares no facet carries no such key at all. Under `--profile contracts`
the same run's `throw.undeclared` entry carries it:

```json
    {
      "id": "throw.undeclared",
      "layer": "contract",
      "level": "fail",
      "path": "./src/Invoice.php",
      "line": 15,
      "column": 13,
      "message": "RuntimeException can escape Invoice::total() but is not declared (@throws LogicException) — proven escape",
      "origin": "direct"
    }
```

Top-level, `profile` names the active surface and `vendor_suppressed` /
`suppressed` / `baselined` count what each channel held back.

A fix-carrying finding's `fix` object is the remedy in machine form: a
`title` and an `edits` array of byte-span splices, the same shape
`transform`'s `EditPlan` serializes, so an agent or editor applies them
without reinventing the diff. Byte offsets index the file's current
contents; `end` is exclusive, and an empty `replacement` is a deletion:

```json
    {
      "id": "debug.type",
      "layer": "debug",
      "level": "fail",
      "path": "./src/Dump.php",
      "line": 4,
      "column": 19,
      "message": "dumped type: 'POST'",
      "fix": {
        "title": "remove the dump statement",
        "edits": [
          { "path": "./src/Dump.php", "span": { "start": 24, "end": 47 }, "replacement": "" }
        ]
      }
    }
```

Under `--fix` the document additionally carries a top-level `fix` object —
`applied` (whether the edits were written), `fixed` (the findings the run
resolved, absent from `findings`), and `refusal` (`null`, or the named
reason with the diagnostics the edits would have surfaced). A run without
`--fix` has no such key.

Two text-mode lines have no JSON counterpart: the stale-baseline count and
the surface-widening notice. SARIF and GitHub-annotation formats are
designed and deferred (ADR-0054 §5); until they land, `json` is the
machine-readable surface — see [CI integration](06-ci.md).

### Errors

```
$ steins check src/Typo
steins: path does not exist: src/Typo
$ steins check --format xml src/
steins: unknown format `xml` (text|json)
$ steins check --profile nope src/
steins: unknown profile `nope` (built-ins: default, contracts, throws-direct, strict; or define [profile.nope])
```

All three exit `2`, and under `--format json` no document is emitted at all.
`check` treats an unknown `--flag` as a path, so a typo surfaces as
``steins: path does not exist: --bogus`` — still exit `2`. A malformed
`steins.toml` is a hard config error, reported before any analysis; see
[configuration](03-configuration.md).

---

## `annotate`

Reprint one file with a right-margin column of proven inferred facts. Reads
only; never writes.

```
steins annotate [--no-php] [--format text|json] [--project <dir>] <file.php>
```

| Flag | Default | Effect |
| --- | --- | --- |
| `--no-php` | off | Skip the PHP sidecar and annotate the sound subset. |
| `--format text\|json` | `text` | `text` is the margin view; `json` emits per-function effect summaries. |
| `--project <dir>` | the file's own directory | The tree parsed for cross-file resolution. |

Exactly one file argument. A directory, a second file, or an unknown flag
exits `2`.

```
$ steins annotate src/Greeter.php
<?php

namespace Acme;

class Greeter
{
    public function greet(string $name): string //=> effects: {}
    {
        return "Hello, {$name}";
    }
}

function run(): string                          //=> effects: {…?}; throws: {…?}
{
    $g = new Greeter();                         //=> $g: Acme\Greeter (exact)
    return $g->greet(null);                     //=> ✗ type.argument-mismatch
}
```

`{…?}` marks a non-exhaustive summary — "these, and possibly more". A `≤`
prefix marks a declared bound: an upper limit imported from an envelope,
read "at most" (ADR-0067).

By default `annotate` parses the target file's own directory as the project.
Point `--project` at the project root when the callers live elsewhere, so the
margin sees the whole call graph — `steins annotate --project . src/Greeter.php`.

`--format json` emits the same effect facts as a document, one entry per
analyzed function, with the two lanes kept apart:

```
$ steins annotate --format json src/Counter.php
{
  "functions": [
    {
      "name": "Counter::bump",
      "line": 10,
      "effects": [
        "output"
      ],
      "declared": [],
      "exhaustive": true
    }
  ]
}
```

`effects` holds proven labels, `declared` holds envelope-imported upper
bounds, and `exhaustive` is the bit the `…?` renders. Nothing is flattened.

> **If you know PHPStan or Psalm:** this is the batch answer to what you get
> from sprinkling `\PHPStan\dumpType()` and rerunning — a whole file's
> inferred facts at once, with no edit to the source. The dump functions
> work too, and report as `debug.type` findings in `check`, as does the
> committable `/** @psalm-trace $x */` docblock (`debug.trace`, warn-level);
> `annotate` is for reading, the dumps are for asking one pointed question.

`annotate` never reports a verdict: a file full of proven breaks still exits
`0`. Run `check` for that.

---

## `transform`

Plan — and optionally apply — a source-to-source rewrite. Dry-run by
default.

```
steins transform <phpdoc-to-native|phpdoc-honesty> [--apply] [--config <path>]
                 [--format text|json] <paths...>
```

Two transforms:

- **`phpdoc-to-native`** promotes a PHPDoc `@param`/`@return` type to a
  native declaration when every call site proves the native hint cannot
  change behavior.
- **`phpdoc-honesty`** rewrites a `@param`/`@return` tag that *lies* to the
  type the call sites and return sites actually prove.

| Flag | Default | Effect |
| --- | --- | --- |
| `--apply` | off | Write the edits, after the post-check passes. |
| `--config <path>` | `./steins.toml` when present | Read `[transform.vouch]` and `[transform.partitions]` from here. |
| `--format text\|json` | `text` | `text` prints unified diffs; `json` carries the whole plan. |

The transform name is the first positional argument and is required.
`<paths...>` is required too.

Every run — dry or applied — re-analyzes the edited project and requires
**zero new diagnostics** before anything is written (ADR-0034). A failing
post-check exits `1` and, under `--apply`, refuses to write.

```
$ steins transform phpdoc-to-native src/
--- a/src/Formatter.php
+++ b/src/Formatter.php
@@ -3,11 +3,9 @@
 namespace Acme;
 
 /**
- * @param string $label
- * @param int $width
  * @return string
  */
-function pad($label, $width)
+function pad(string $label, int $width)
 {
     return str_pad($label, $width);
 }

Refusals (1):
  src/Ids.php:8:16: function label() param $id [argument-not-proven] — call at src/Ids.php:16:11 passes `"42"`, which `int` does not admit

3 enumerated: 2 promoted, 1 refused
Post-check OK — no new diagnostics.
```

The last count is the completeness oracle: every candidate site is
enumerated, and each is either transformed or refused with a named,
stable reason. Nothing is skipped silently.

`phpdoc-honesty` takes the site `phpdoc-to-native` refused and repairs the
tag instead:

```
$ steins transform phpdoc-honesty src/
--- a/src/Ids.php
+++ b/src/Ids.php
@@ -3,7 +3,7 @@
 namespace Acme;
 
 /**
- * @param int $id
+ * @param int|'42' $id
  */
 function label($id): string
 {

1 enumerated: 1 rewritten, 0 refused
Post-check OK — no new diagnostics.
```

`--apply` writes and says how many files it touched, on stderr:

```
$ steins transform phpdoc-to-native --apply src/
…
3 enumerated: 2 promoted, 1 refused
Post-check OK — no new diagnostics.
steins: applied 1 file edit(s)
```

`--format json` carries the plan's byte spans, the refusal list, the oracle
counts, the post-check verdict, and an `applied` boolean — plus every
dynamic-code obstacle site, which the text mode caps at five per obstacle.

Errors exit `2`: an unknown transform name, a missing name, no paths, a
`--config` with no argument. The name is positional, so forgetting it makes
your first path the name — `steins transform src/` reports
``steins: unknown transform `src/` (available: phpdoc-to-native, phpdoc-honesty)``.
Like `check`, `transform` treats an unknown `--flag` as a path, which then
fails the existence check. A `--config` path that cannot be read warns and
proceeds with no vouches, since a vouch typo must not stop the run.

---

## `effect-diff`

Capture the project's per-function effect summaries, or report how today's
differ from a captured past. The review story for an effect-neutral
refactor: capture, refactor, run again, read one line per changed function.

```
steins effect-diff [--baseline <path>] [--set-baseline] [--format text|json] <paths...>
```

| Flag | Default | Effect |
| --- | --- | --- |
| `--baseline <path>` | `steins-effects-baseline.json` | Locate the capture file. |
| `--set-baseline` | off | Write the capture instead of diffing. |
| `--format text\|json` | `text` | Output mode. |

This surface touches neither `check` nor the diagnostic baseline — its own
file, its own format, no suppression, no verdict. It needs no PHP and takes
no `--no-php`.

```
$ steins effect-diff --set-baseline --baseline effects.json src/
steins: wrote 8 effect summaries to effects.json
```

Delete the `echo` from `Counter::bump()` and rerun:

```
$ steins effect-diff --baseline effects.json src/
src/Counter.php Acme\Counter::bump: - output
$ echo $?
0
```

Event lines read `<file> <symbol>: <change>`: `+ label` for a newly proven
effect, `- label` for one gone, `≤→ label` for a declared bound that became
proven, `+ ≤label (declared)` for a new bound, and one-line coverage notes
when the exhaustiveness bit flips. A footer counts functions added and
removed since the capture, printed only when either is nonzero — deleting
`src/Ids.php` and its two functions gives:

```
0 functions not in baseline, 2 no longer present
```

`--format json` gives the same events plus the footer counts:

```
$ steins effect-diff --baseline effects.json --format json src/
{
  "events": [
    {
      "file": "src/Counter.php",
      "symbol": "Acme\\Counter::bump",
      "category": "proven-removed",
      "label": "output"
    }
  ],
  "compared": 8,
  "not_in_baseline": 0,
  "no_longer_present": 0
}
```

`category` is one of `proven-added`, `proven-removed`,
`proven-removed-maybe`, `declared-materialized`, `declared-added`,
`declared-removed`, `coverage-narrowed`, `coverage-completed`.

The diff is informational, so a run that finds changes still exits `0`.
Gating a build on an effect delta is a policy decision this surface does not
make. Only a usage error exits `2` — including a missing capture file:

```
$ steins effect-diff src/
steins: cannot read effect baseline steins-effects-baseline.json: No such file or directory (os error 2) (run --set-baseline to capture one)
$ echo $?
2
```

---

## `doctor`

Report posture: which `php` answered, what the active profile checks, which
trees count as vendor, how much of the code the analysis declined to reason
about, baseline health. Doctor reads configuration, the environment, and
index-level facts. It runs **no checks**, and its exit never depends on what
`check` would find (ADR-0054 §8).

```
steins doctor [--no-php] [--baseline <path>] [path]
```

| Flag | Default | Effect |
| --- | --- | --- |
| `--no-php` | off | Report the sound-subset posture without spawning the sidecar. |
| `--baseline <path>` | the conventional default file when it exists | Which baseline to report on. |

`path` is optional and defaults to `.`. A second path exits `2`. There is no
`--format` — machine-readable doctor output is deferred with design
(ADR-0054 §14).

```
$ steins doctor
steins doctor — posture report (index-bound; runs no checks)

Runtime
  PHP sidecar: spawned ok
  PHP version: 8.5.8
  SAPI: cli
  loaded extensions: 70
  analysis target: PHP 8.3 (8.x) (from require.php "^8.3")
  version skew: runtime 8.5 sits above the 8.3 floor — reflection describes the runtime, so symbols newer than the floor are not proven absent for it (silence, never a false claim)

Config + active surface
  steins.toml: not found (built-in defaults govern)
  active profile: `default` (from built-in default)
  surface: layers [mechanics, proof], 16 checked id(s)

Layout
  1 manifest(s) govern this tree:
    composer.json
      vendor: vendor
      ours:   src

Coverage posture
  6 file(s), 16 scope(s), 0 poisoned (0.0%) — a poisoned scope knows no local's value (ADR-0001, ADR-0046 §1)
  opaque constructs: none — no scope is on the give-up list
  dam sites: none — no runtime-definition construct stands, so existence-absence claims are undammed (ADR-0049 §2)
  reflection-driven invocation: none recognized
    (this list is a guess until measured: the recognizer is syntactic, it names no receiver type, and it is not exhaustive)

Envelopes
  1 written throw envelope(s); the active profile `default` does not check them — the `contracts` (or `throws-direct`) profile does

Baseline
  none (no baseline file; `check --set-baseline` writes one)
$ echo $?
0
```

With `--baseline`, the Baseline section compares the surface the file was
captured under against the active one:

```
$ steins doctor --baseline b.jsonl
…
Baseline
  file: b.jsonl (1 entry)
  capture surface: profile `default`, 16 id(s)
  active surface: profile `default`, 16 id(s)
```

A `--baseline` path that does not exist is reported as absent, not failed.

> **If you know PHPStan or Psalm:** this is `phpstan diagnose` with a wider
> brief. Beyond the environment, it prints how much of your code the
> analysis declined to reason about — poisoned scopes, `eval` sites, dynamic
> includes — so a quiet run comes with numbers behind its quiet.

### Doctor's three exits

`0` covers every posture doctor can report, degraded ones included. No
`php` on `PATH` is a mode, not a failure:

```
$ steins doctor --no-php
steins doctor — posture report (index-bound; runs no checks)

Runtime
  PHP sidecar: disabled (--no-php)
  analysis target: PHP 8.3 (8.x) (from require.php "^8.3")
  posture: sound subset — findings that require executing PHP are omitted
  (a degraded environment is not a failure — exit stays 0, ADR-0004)
…
```

`1` is a configuration contradiction — the configuration asserts something
the world refutes. An unparseable `steins.toml`, an unresolvable profile, or
a baseline file whose header is not valid JSON. The report still renders in
full:

```
$ steins doctor
…
Config + active surface
  steins.toml: PARSE ERROR — steins.toml: parse error (TOML parse error at line 3, column 19
…
  (configuration contradiction — doctor exits 1, ADR-0054 §10)
$ echo $?
1
```

`2` is doctor's own usage error: an unknown flag, a second path, a
`--baseline` with no argument, or a `path` that names nothing (ADR-0054 §10):

```
$ steins doctor nope/
steins: path does not exist: nope/
$ steins doctor --format json
steins: unknown flag `--format` for doctor
$ steins doctor src/ .
steins: doctor takes at most one path (usage: steins doctor [--no-php] [--baseline <path>] [path])
```

Reporting on `/typo`'s parent directory would answer a different question,
so it reds instead. For symptom-indexed use of this report, see
[troubleshooting](07-troubleshooting.md).

---

## `version`

Print the build banner. Spelled `version`, `-v`, or `--version`; takes no
flags and always exits `0`.

```
$ steins version
steins 0.1.2 (2026-08-01 revision 8a901ed) - https://github.com/rigortype/steins
Copyright (c) TypedDuck, USAMI Kenta <tadsan@zonu.me>
    Built with the help of many third-party libraries.
    Run `steins license` to see all dependencies and their licenses.
```

The date and revision come from the build and read `unknown` when the binary
was built outside a git working tree.

---

## `license`

Print Steins' own Apache-2.0 terms, then every bundled dependency's notice.
Aliased as `licenses`; takes no flags and always exits `0`.

```
$ steins license
steins 0.1.2 — open source licenses
https://github.com/rigortype/steins

Steins is licensed under the Apache License 2.0:

                                 Apache License
                           Version 2.0, January 2004
                        http://www.apache.org/licenses/
…
```

Both documents are embedded in the executable, so this works for a bare
binary with no files beside it — a Homebrew install or a
`cargo install --git` build carries its own terms. The full output runs to
several hundred lines; pipe it to a pager. Apache-2.0's text appears twice,
once as Steins' terms and once among the dependencies that use it, because
the third-party notice also ships as a standalone file and has to stand on
its own.
