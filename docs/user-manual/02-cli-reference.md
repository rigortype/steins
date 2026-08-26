# CLI reference

Every subcommand, every flag it accepts, and what each exit code means.
Verified against **steins 0.1.2** (`steins version`); the transcripts below
are real output from that binary, except that every **id count** and the
built-in profile list have been re-measured on the current build. For getting
the binary in the first place, see
[installation and quickstart](01-installation-and-quickstart.md).

## How to read this page

A synopsis line spells one invocation:

```
steins check [--format text|json|github|sarif] [--profile <name>] <paths...>
```

Square brackets mark optional flags. `<name>` is a value you supply.
`a|b` is a closed choice. `<paths...>` takes one or more files or
directories; a directory is walked recursively for `.php` files, and every
path in one invocation forms a single project, so cross-file calls and class
chains resolve.

The walk stays inside the paths you named. A directory **symlink** found
during that walk is followed only when its real target is under one of those
paths and has not been walked already: a link out of your project is code you
did not ask about, and a link back into it would analyze the same files twice
and report every finding twice. A path you name yourself is always walked,
symlink or not, and a symlinked *file* is analyzed — with a file reachable
under two names analyzed once, reported under its real path. `steins doctor`
prints how many paths a run skipped this way, and names them; `steins check`
says nothing about them, because its output is findings.

There is **no `--help`**. Run the binary with no arguments and it prints the
whole surface to stderr and exits `2`:

```
$ steins
usage: steins check [--format text|json|github|sarif] [--profile <name>] [--no-php] [--no-cache] [--no-tolerated-effects] [--vendor-diagnostics] [--fix] [--set-baseline] [--baseline <path>] [--ignore-baseline] <paths...>
       steins annotate [--no-php] [--format text|json] <file.php>
       steins transform <phpdoc-to-native|phpdoc-honesty|throws-envelope|effects-envelope|loop-to-array-map> [--apply] [--format text|json] <paths...>
       steins effect-diff [--baseline <path>] [--set-baseline] [--format text|json] <paths...>
       steins doctor [--no-php] [--baseline <path>] [--format text|json] [path]
       steins mcp
       steins version | -v | --version
       steins license
$ echo $?
2
```

An unrecognized subcommand names the eight that exist:

```
$ steins lsp
steins: unknown command `lsp` (available: check, annotate, transform, effect-diff, doctor, mcp, version, license)
$ echo $?
2
```

The command set is ADR-0020's. `mcp` ships as of this release; `lsp` is
designed and not yet shipped.

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
| `mcp` | the client closed the connection | stdin could not be read | an argument was given (it takes none) |
| `version` | always | — | — |
| `license` | always | — | — |

Two rules cut across all eight. A path argument that names nothing is a
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
steins check [--format text|json|github|sarif] [--profile <name>] [--no-php]
             [--no-cache] [--vendor-diagnostics] [--fix] [--set-baseline]
             [--baseline <path>] [--ignore-baseline]
             [--no-tolerated-effects] <paths...>
```

| Flag | Default | Effect |
| --- | --- | --- |
| `--format text\|json\|github\|sarif` | `text`, or `github` under GitHub Actions | Output mode — see [`--format github`](#--format-github) for the auto-detection rule. |
| `--profile <name>` | `[check] profile`, else `default` | Select the display surface — a built-in stage or one named in `steins.toml`. |
| `--no-php` | off | Skip the PHP sidecar and run the sound subset. |
| `--no-cache` | off | Analyze from source, ignoring and not writing `.steins/` — see [the analysis cache](#the-analysis-cache). |
| `--vendor-diagnostics` | off | Report findings inside vendor trees too. |
| `--fix` | off | Apply the fixes findings carry, post-check-gated — see [`--fix`](#--fix). |
| `--baseline <path>` | `.steins-baseline.jsonl` when it exists | Locate the baseline file. |
| `--set-baseline` | off | Write the baseline instead of reporting; exits `0`. Cannot combine with `--fix`. |
| `--ignore-baseline` | off | Report the full surface, consulting no baseline file. |
| `--no-tolerated-effects` | off | Judge effect envelopes with an empty tolerance set, bringing back everything `[effects] tolerated` discharges — see [`[effects]`](03-configuration.md#effects). |

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
./src/Counter.php:12:9: error[effect.envelope-exceeded]: echo has effect io.output.buffer, but Counter::bump() is declared #[\Steins\Pure]
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

### The analysis cache

`steins check` caches its analysis in `.steins/` beside the project — the
outermost directory a `composer.json` governs, or the analyzed tree when
none does. A second run over an unchanged tree reuses it; a run after an
edit reuses everything the edit could not have reached. It is on by
default, and there is nothing to configure.

Four things follow from that, and all four are deliberate:

- **It never changes a finding.** A cache miss costs time and nothing else.
  If you ever see `--no-cache` report something the default run did not,
  that is a bug worth an issue, not a workaround.
- **It never says anything.** No per-run note, cold or warm, and none when
  something goes wrong either: an unwritable project, a corrupt artifact, a
  half-written cache all fall back to analyzing from source, silently,
  reporting exactly what a machine without a cache reports. To see what is
  actually cached, ask [`doctor`](#doctor) — its **Generation store**
  section shows the published generation, its package count, its size on
  disk, and the persistent reasons a run could not use it.
- **It does not belong in git.** Creating the store writes
  `.steins/.gitignore` holding `*`, the way Cargo does for `target/`, so
  this is already handled unless you delete that file.
- **It does not grow.** The store keeps one generation — the current one.
  Each run's publish removes the generation it replaced, so editing all day
  costs the size of one cached analysis, not one per edit. Nothing under
  `.steins/gen/` is removed unless steins wrote it.

`--no-cache` analyzes from source and neither reads nor writes `.steins/`.
It is worth reaching for in exactly two situations: a sandbox where writing
beside the project is unwelcome, and confirming that a finding you doubt is
not the cache's fault. It is *not* needed in CI — a fresh runner has no
store to reuse, so the cached and uncached runs do the same work.

Deleting `.steins/` at any time is safe; the next run rebuilds it.

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
the surface-widening notice.

### `--format github`

GitHub Actions workflow commands — one per displayed finding, so the run
annotates the pull request diff inline:

```
$ steins check --format github .
::error file=src/Greeter.php,line=16,col=22,title=type.argument-mismatch::argument null to Greeter::greet() cannot become string $name — proven TypeError (strict mode)
::notice file=src/Dump.php,line=4,col=19,title=debug.var-dump::dumped type: 'POST'
1 findings in baseline
```

The command name follows the finding's exit level: fail-level is `::error`,
a profile-demoted `warn` is `::warning`. The one carve-out is the debug
lane — a `var_dump` (or a `@steins-trace` docblock) is an *answer to a
question the code asked*, not a claim about the program, so it takes
`::notice`. It is never dropped: a fail-level dump reds the job in every
format, and a rendering that hid the annotation would leave a red run with
nothing explaining it. `title` carries the id, so an annotation is
triageable from the diff view without opening the job log.

Nothing is truncated on Steins' side. GitHub renders a bounded number of
annotations per step, per type; a run displaying hundreds of findings is
drowning by definition, and the answer to that is the baseline round-trip,
not a quieter log. After the commands come the same plain accounting lines
`text` prints — inert in a workflow log, and identical across formats.

**Auto-detection.** With no `--format` flag, `check` emits `github` when
`GITHUB_ACTIONS=true` is in the environment, and `text` everywhere else. An
explicit `--format` always wins, in both directions. Detection changes only
the *spelling*: the surface, the profile, the suppression pipeline and the
exit code are identical either way. Nothing else is detected — a generic
`CI=true` names no particular rendering, and `text` is already right there.

Paths pass through as given: relative stays relative, absolute stays
absolute. GitHub matches annotations against repo-root-relative paths, so
invoke `check` from the repo root with relative paths.

### `--format sarif`

A SARIF 2.1.0 log, for upload to a code-scanning service. One `run`, and a
deliberately minimal committed shape:

```
$ steins check --format sarif . > steins.sarif
$ jq '.runs[0] | {rules: [.tool.driver.rules[].id], results: (.results | length), properties}' steins.sarif
{
  "rules": [
    "type.argument-mismatch"
  ],
  "results": 1,
  "properties": {
    "profile": "default",
    "vendorSuppressed": 1,
    "suppressed": 0,
    "baselined": 0
  }
}
```

- `tool.driver.rules` carries **one entry per id present in the results**,
  deduped and sorted — not the whole registry. Each entry has the id, its
  layer under `properties`, and its default level. Prose descriptions and
  `helpUri` are not there yet; the id is the description.
- Each result carries `ruleId`, `ruleIndex`, `level` (the same mapping
  `--format github` uses), the message verbatim, one physical location with
  the same 1-based line and column the other formats print, and any
  registry-declared facet under `properties`.
- `partialFingerprints` carries `steinsFindingHash/v1` — the *same* hash the
  baseline uses to recognize a finding across unrelated edits. Alert
  tracking across runs therefore gets the stability the baseline already
  has, from one identity function rather than two.
- `run.automationDetails.id` is `steins/<profile>`, so a `default` gate and
  a `contracts` debt dashboard can upload in parallel without clobbering
  each other's alert categories.
- `run.properties` carries the accounting envelope — **counts, never
  entries**.

There is no `suppressions` section and there never will be. A baselined or
`@steins-ignore`d finding is a count and nothing more: re-emitting it as a
SARIF "suppressed result" would make the format a fourth suppression
channel beside the three that exist, and would publish the contents of your
baseline into every upload.

Output goes to stdout like every other format — redirect it. `sarif` is
never auto-selected: it is a file artifact you ask for in an upload step,
not a log rendering. And the exit code is the usual one, so a workflow that
wants to upload the log from a failing run wants `continue-on-error` on the
check step; there is no flag that makes `check` lie about what it found.

Paths pass through as given, with backslashes normalized to forward
slashes. Code-scanning upload matches repo-root-relative paths, so invoke
`check` from the repo root with relative paths.

### Errors

```
$ steins check src/Typo
steins: path does not exist: src/Typo
$ steins check --format xml src/
steins: unknown format `xml` (text|json|github|sarif)
$ steins check --profile nope src/
steins: unknown profile `nope` (built-ins: default, contracts, throws-direct, strict, pedantic; or define [profile.nope])
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
read "at most" (ADR-0067). A `~` prefix marks a label the project's
`[effects] tolerated` policy discharges wholly at that unit — the label is
still proven and still printed, it is only no longer judged (ADR-0084).

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
        "io.output.buffer"
      ],
      "declared": [],
      "exhaustive": true
    }
  ]
}
```

`effects` holds proven labels, `declared` holds envelope-imported upper
bounds, and `exhaustive` is the bit the `…?` renders. Nothing is flattened.
A `tolerated` array joins them where a policy discharges something; the
tolerated labels stay listed in `effects` too, so a consumer reading only
`effects` sees the same set it always did.

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
steins transform <phpdoc-to-native|phpdoc-honesty|throws-envelope|effects-envelope|loop-to-array-map>
                 [--apply] [--config <path>] [--format text|json] <paths...>
```

Five transforms:

- **`phpdoc-to-native`** promotes a PHPDoc `@param`/`@return` type to a
  native declaration when every call site proves the native hint cannot
  change behavior.
- **`phpdoc-honesty`** rewrites a `@param`/`@return` tag that *lies* to the
  type the call sites and return sites actually prove.
- **`throws-envelope`** seeds `@throws` tags from proven escapes: for every
  declaration the engine proves throws (the machinery behind
  `throw.undeclared`), it writes the missing tags — creating the docblock
  when absent, appending to it losslessly when present — so a repo can adopt
  the `throws-direct` and `contracts` profiles by running one command
  instead of hand-writing envelopes.
- **`effects-envelope`** seeds the interop envelopes of ADR-0082 from proven
  effects — the effect-world sister of `throws-envelope`. A declaration whose
  inferred effects are *exhaustive* gets `@phpstan-impure <labels>` (the
  declared lane folded in, prefix-subsumed labels dropped, comma-space
  separated); a class whose every declared method is provenly and exhaustively
  pure gets `@phpstan-all-methods-pure` and no method tags. It writes nothing
  where a tag would be a lie or a no-op: a non-exhaustive declaration is
  refused, no bare tag is ever written, no per-declaration `@phpstan-pure` is
  ever written, and a declaration carrying the checked spelling
  (`#[\Steins\Effect]` / `#[\Steins\Pure]`) is skipped. An envelope already
  stating the same bound is left alone; one stating a different bound is
  corrected in place — unless the tag carries a label the run's registry does
  not know, in which case it is not a bound at all but most likely a human's
  note (`@phpstan-impure database`), and those bytes are never touched.
- **`loop-to-array-map`** rewrites an append loop to `array_map` when the
  engine *proves* the loop body has no effects and cannot throw.

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

`throws-envelope` writes the proven escape set as `@throws` tags — one tag
per exception class, fully qualified, in the proven set's source order:

```
$ steins transform throws-envelope src/
--- a/src/Loader.php
+++ b/src/Loader.php
@@ -1,4 +1,7 @@
 <?php
+/**
+ * @throws \RuntimeException
+ */
 function load(string $path): string
 {
     throw new \RuntimeException("cannot read $path");

1 enumerated: 1 seeded, 0 refused
Post-check OK — no new diagnostics.
```

Only **proven** escapes are written — a Maybe escape refuses with
`escape-not-proven`, because a seeded tag is a contract the repo then owns
(written-by-tool is declared, not proven). A declaration whose proven
escapes are all covered already refuses `already-declared`, which is also
why running the transform twice is a no-op. An existing docblock is
extended by inserting whole lines before its closing `*/` — every existing
line is byte-preserved — and a docblock with no such insertion point (a
single-line `/** … */`, or content sharing the closing line) refuses
`docblock-not-round-trippable`. A declaration that does not start its own
line, so that no docblock can go above it without rewriting bytes that are
not its own, refuses `declaration-mid-line`. Those four names are the whole
refusal taxonomy for this transform.

Neither envelope-seeding transform consults the vouch valve: a proven escape
(or a proven effect) is a forward fact of the declaration's own body and
callees, so the dynamic-code obstacles that make "all callers proven"
unknowable have no bearing on them. A `[transform.vouch]` section is simply
inert for `throws-envelope` and `effects-envelope`, and no per-entry no-op
warning is printed for either.

The post-check for `throws-envelope` and `effects-envelope` is measured on the
**default** display surface — proof and mechanics, what a bare `check` reports.
They are the only two transforms for which that is true. `phpdoc-to-native`,
`phpdoc-honesty`, and `loop-to-array-map` are measured against every layer,
contract included.

The asymmetry is deliberate, and pinned by a test. Seeding an envelope is
*supposed* to move the contract surface: writing `@throws` onto an override
is exactly what gives its parent's narrower envelope something to be widened
against, so `throw.liskov-widened` appears where there was none. Measured
against the contract layer, a correct seed would veto itself and refuse to
write. That finding is existing debt the envelope makes visible — run
`check --profile contracts` after seeding to see it — not a regression.

An emitted interop envelope is the same story one system over: it is exactly
what gives `effect.envelope-exceeded` something to check.

The other three have no such property: a promotion or an honesty repair is
not meant to change what a docblock promises, and a loop rewrite does not
touch a docblock at all, so a new `phpdoc.*` finding after their edit is a
regression and still blocks the write.

`effects-envelope` refuses by name too: `effects-not-exhaustive` (inference
could not close the effect set, so no label list is an upper bound),
`attribute-envelope` (the declaration already carries the checked spelling),
`already-declared` (the same bound is written — the second run of the
transform), `existing-tag-unreadable` (a tag is already written whose labels
the registry cannot read, so it may be prose rather than a bound),
`bound-label-unknown` (the computed bound names a label the registry does not
know, so the tag would read back as prose), plus the two shared
docblock-mechanics names above. A pure declaration is not a candidate at all —
no per-declaration `@phpstan-pure` is ever written — with one exception: a pure
declaration carrying an unreadable tag is reported, because "your docblock was
left alone" is an answer worth having.

`loop-to-array-map` is the first transform whose precondition is an
*effect* judgment rather than a type one. It rewrites

```php
$out = [];
foreach ($xs as $x) {
    $out[] = f($x);
}
```

into `$out = array_map(fn ($x) => f($x), $xs);` — but only when the engine
proves all of the following, and refuses by name otherwise:

- the loop body's **proven** effect lane is empty on every label, and every
  call in it resolved (a declared `≤` bound is a cap, not a proof, and does
  not qualify);
- the body's **proven throw set is empty** — a stricter bar than
  `#[\Steins\Pure]`, which admits `throw`. A body that throws on element
  *k* leaves `$out` holding the first *k* results, which every enclosing
  `catch` can see; the rewritten form leaves `$out` unassigned;
- the subject proves `array` **and** `is_list = Yes` (`array_map` preserves
  keys, the append renumbers `0..n-1`);
- the iteration variable is not used after the loop, the accumulator is not
  read inside it, and `$out = [];` is the statement immediately before it.

Every `foreach` in the analyzed set is a candidate, so the oracle counts
show exactly how narrow this first version is:

```
$ steins transform loop-to-array-map src/
--- a/src/Report.php
+++ b/src/Report.php
@@ -6,10 +6,7 @@
 function labels(): array
 {
     $rows = [3, 1, 4];
-    $out = [];
-    foreach ($rows as $row) {
-        $out[] = label($row);
-    }
+    $out = array_map(fn ($row) => label($row), $rows);
 
     return $out;
 }

Refusals (2):
  src/Report.php:21:5: foreach [body-effects] — the body's proven effect lane is non-empty: {io}
  src/Report.php:34:5: foreach [subject-not-proven-list] — `$rows` is not proven `is_list = Yes`; array_map preserves keys while the append renumbers 0..n-1

3 enumerated: 1 rewritten, 2 refused
Post-check OK — no new diagnostics.
```

The refusal reasons are stable names: `key-binding`, `reference-binding`,
`value-binding-not-variable`, `subject-not-variable`,
`subject-not-proven-array`, `subject-not-proven-list`,
`accumulator-init-not-adjacent`, `accumulator-not-empty`,
`accumulator-read-in-body`, `iteration-var-live-after`, `early-exit`,
`body-not-single-append`, `body-effects`, `body-throws`,
`body-call-unresolved`.

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
``steins: unknown transform `src/` (available: phpdoc-to-native, phpdoc-honesty,
throws-envelope, loop-to-array-map)``.
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
src/Counter.php Acme\Counter::bump: - io.output.buffer
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
      "label": "io.output.buffer"
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
about, baseline health, the builtin catalog's version pin, and the
diagnostic registry's own self-consistency. Doctor reads configuration, the
environment, and index-level facts. It runs **no checks**, and its exit
never depends on what `check` would find (ADR-0054 §8).

```
steins doctor [--no-php] [--baseline <path>] [--format text|json] [path]
```

| Flag | Default | Effect |
| --- | --- | --- |
| `--no-php` | off | Report the sound-subset posture without spawning the sidecar. |
| `--baseline <path>` | the conventional default file when it exists | Which baseline to report on. |
| `--format text\|json` | `text` | Render the same section structure as JSON (ADR-0054 §14). |

`path` is optional and defaults to `.`. A second path exits `2`.

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
  [runtime] sapi: undeclared (deferred-with-design, ADR-0049 A6) — apache_*, fastcgi_finish_request, getallheaders, litespeed_*, virtual are never reported Absent this run

Config + active surface
  steins.toml: not found (built-in defaults govern)
  active profile: `default` (from built-in default)
  surface: layers [mechanics, proof], 47 checked id(s)

Layout
  1 manifest(s) govern this tree:
    composer.json
      vendor: vendor
      ours:   src

Generation store
  store: .steins/gen
  current generation: 7fed4cf2dc714492f2256affc1ddf71219696b66e019cb7c3346da3a6ee4ac1e
  packages: 2
  on disk: 1.4 MB across 1 generation(s)

Coverage posture
  6 file(s), 16 scope(s), 0 poisoned (0.0%) — a poisoned scope knows no local's value (ADR-0001, ADR-0046 §1)
  opaque constructs: none — no scope is on the give-up list
  dam sites: none — no runtime-definition construct stands, so existence-absence claims are undammed (ADR-0049 §2)
  reflection-driven invocation: none recognized
    (this list is a guess until measured: the recognizer is syntactic, it names no receiver type, and it is not exhaustive)
  reflected class world: 1 of 3 unanswered class-like name(s) resolved off the project's own PHP — Redis (redis)
    (a reflected declaration restores coverage only: it is the runtime's own claim, and no absence finding is premised on it — issue #269)
  vendor posture: findings under a vendor root are suppressed by default (ADR-0015)
  vouched dynamic-code exemptions: none declared ([transform.vouch] in steins.toml)

Envelopes
  1 written throw envelope(s); the active profile `default` does not check them — the `contracts` (or `throws-direct`) profile does

Baseline
  none (no baseline file; `check --set-baseline` writes one)

Catalog
  builtin catalog pinned to php-src PHP 8.5
  analysis target: PHP 8.3 (8.x) — SKEWED against the pin
  A11 consequence: catalog-backed is-a demoted to Unknown for arm deletion and descendant closure (ADR-0052 amendment A11)
  hierarchy table: 352 row(s); foldable allowlist: 46 name(s) (freshness context, not a per-project fact)

Registry totality
  70 registered id(s): 67 emittable, 3 registered-not-yet-emitted
  partition consistent — every registered id is emittable XOR pending (ADR-0050 §2 totality)

Require
  not configured — no posture assertions declared ([doctor] require = [...] opts in, ADR-0054 §14)
$ echo $?
0
```

The **reflected class world** line answers "which classes here does Steins
know about only because your PHP told it". A class an installed extension
provides — `Redis`, `Random\Randomizer`, `Dom\Element` — is in no source
file and in no bundled list, so Steins asks the same `php` the sidecar
already runs and prints what it resolved, with the extension each class came
from. The line appears only when a live sidecar answered: under `--no-php`,
with no `php` on `PATH`, or after a failed handshake there is nothing to
report and the section is exactly what it was.

A resolved class buys coverage, not findings. It is the runtime's own claim
about its own class, so nothing that reports a *missing* member — the
`call.undefined-method` / `property.undefined` / `class-const.undefined`
family — is ever premised on it. Enabling PHP can therefore make Steins
know more about `Redis`, and cannot make it report a method call on one.

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

### `--format json`

The point-9 section list **is** the schema (ADR-0054 §14): `text` and
`json` render the identical internal `Vec<Section>`, so the two can never
drift apart into two different stories about one run.

```
$ steins doctor --no-php --format json .
{
  "schema": "steins.doctor/v1",
  "banner": "steins doctor — posture report (index-bound; runs no checks)",
  "exit_code": 0,
  "sections": [
    {"name": "Runtime", "lines": ["PHP sidecar: disabled (--no-php)", "…"]},
    {"name": "Config + active surface", "lines": ["…"]},
    {"name": "Layout", "lines": ["…"]},
    {"name": "Coverage posture", "lines": ["…"]},
    {"name": "Envelopes", "lines": ["…"]},
    {"name": "Baseline", "lines": ["…"]},
    {"name": "Catalog", "lines": ["…"]},
    {"name": "Registry totality", "lines": ["…"]},
    {"name": "Require", "lines": ["…"]}
  ]
}
```

`sections` is always exactly these nine, in this order, whatever their
content — the *structure* is fixed the way the four `check` formats' finding
multiset is fixed (ADR-0054 point 1), only the *content* of a `lines` array
varies with the project. Each line is the same sentence `text` prints, with
the terminal-facing leading-space indentation trimmed; nesting is not
otherwise represented in this first schema version. `exit_code` mirrors the
process's own exit code, so a consumer that only reads the JSON document
still learns whether the run was 0 or 1.

### `[doctor] require`

Every posture line above reports at exit `0`, degradations included — that
is point 10's whole design. A project that wants a specific fact to fail the
run opts in by name:

```toml
[doctor]
require = ["sidecar"]
```

```
$ steins doctor --no-php
…
Require
  FAIL `sidecar` — no PHP sidecar answered this run (Runtime section)
  1 of 1 declared assertion(s) FAILED (sidecar) — doctor exits 1, ADR-0054 §14
$ echo $?
1
```

The known assertion names:

| Name | Passes when |
| --- | --- |
| `sidecar` | The PHP sidecar spawned and answered `env()` this run. |
| `catalog-pin-match` | The analysis version is **confirmed** to match the catalog's php-src pin (Catalog section). A confirmed skew fails; so does an unconfirmable comparison (no target declared and no PHP sidecar) — `require` is the strictness opt-in, so a guarantee doctor cannot even attempt is a violation, not a free pass (the Catalog section's own text still reports "unskewed" by default — this is stricter only under `require`). |
| `no-monkey-patch` | No `uopz`/`runkit7`/`Componere` extension is loaded (Runtime section, ADR-0049 A9). |
| `no-dormant-baseline` | The baseline carries no dormant entries (Baseline section); vacuously true with no baseline at all. |

A name outside this list is a configuration contradiction — the same lane
as an unparseable `steins.toml` — and so is a misspelled key under
`[doctor]` itself (`requries`, say): both are hard config errors, exit `1`,
never a silently-ignored typo.

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
the world refutes. An unparseable `steins.toml`, an unresolvable profile, a
baseline file whose header is not valid JSON, or a violated (or unknown)
`[doctor] require` assertion. The report still renders in full:

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
`--baseline` with no argument, an unrecognized `--format` value, or a
`path` that names nothing (ADR-0054 §10):

```
$ steins doctor nope/
steins: path does not exist: nope/
$ steins doctor --format yaml
steins: unknown format `yaml` (text|json)
$ steins doctor src/ .
steins: doctor takes at most one path (usage: steins doctor [--no-php] [--baseline <path>] [--format text|json] [path])
```

Reporting on `/typo`'s parent directory would answer a different question,
so it reds instead. For symptom-indexed use of this report, see
[troubleshooting](07-troubleshooting.md).

---

## `mcp`

Serve the transform loop to an AI agent over
[MCP](https://modelcontextprotocol.io), on stdio.

```
steins mcp
```

Takes no arguments: *what* to analyze is a tool argument, because one server
answers about many paths over its lifetime. It speaks JSON-RPC 2.0 messages
delimited by newlines on stdin/stdout — the transport an MCP host starts a
server with — and logs to stderr. It is not meant to be typed at a terminal;
point your agent's MCP configuration at the binary with the single argument
`mcp`.

Four tools:

| Tool | Arguments | Answers | Writes? |
| --- | --- | --- | --- |
| `list_transforms` | none | every transform this build can plan, with what it rewrites | no |
| `plan_transform` | `transform`, `paths`, optional `config` | the edit plan, a unified diff per file, the completeness oracle, every refusal with its named reason, the post-check verdict, and a `plan_handle` | no |
| `apply_plan` | `plan_handle` | the files written | **yes — only this one** |
| `check` | `paths`, optional `profile`, `no_php`, `vendor_diagnostics` | the findings `check` reports, each with its `fix` payload where one exists | no |

The loop is ADR-0010's, and the pause in the middle of it is the point:
**plan and apply are separate calls, and there is no call that does both.**
`plan_transform` writes nothing and hands back a handle; you show the human
the diff and the refusals; `apply_plan` on that handle writes. An agent that
wants to skip the approval step has no tool to do it with.

A plan handle is **valid only inside the server process that produced it**.
There is no daemon and no plan file: the plan is memory belonging to that one
connection. A handle from a restarted server, from a second connection, or one
that a previous `apply_plan` already consumed comes back as a named error and
never as a write — so nothing can splice byte offsets into a tree that nobody
re-verified. Applying re-reads every target and refuses `tree-changed-since-plan`
if the bytes moved since planning, then re-runs the same zero-new-diagnostics
post-check `transform` runs, on the same surface that transform names.

Failures an agent is meant to read come back as tool results carrying
`isError` and a stable `reason` — `plan-handle-foreign-process`,
`plan-handle-unknown`, `tree-changed-since-plan`,
`postcheck-new-diagnostics`, `path-does-not-exist`, … — with a human
`detail` beside it, the same discipline transform refusals follow. A path
argument that names nothing is refused here too, for the reason it is
refused everywhere else: a renamed directory must not come back as a clean
empty report.

What the tools report is what the command line reports, structured — the same
plan, the same oracle counts, the same refusal names, the same findings and
fix payloads — with one deliberate difference: `check` does not consult the
baseline file. The baseline is a CI ratchet, and an agent asking what is true
about the code should not be answered through it.

`mcp` exits `2` if given any argument, `1` if stdin cannot be read, and `0`
when the client closes the connection.

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
