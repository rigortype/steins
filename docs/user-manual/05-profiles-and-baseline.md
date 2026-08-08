# Profiles, baseline, and suppression

You pointed `check` at a real codebase, it came back clean, and you know
that codebase is not clean. The default surface is doing its job: it reports
what provably breaks and holds everything else back. This chapter is how you
ask for the rest, and how you ask for it over a repo with years of debt in it
without a first run that buries you.

Three mechanisms do the work. **Profiles** name how much Steins reports.
The **baseline** freezes the debt you already have so only new debt fails CI.
**`@steins-ignore`** exempts one site. `steins effect-diff` is a fourth
surface that looks like a baseline and belongs to a different loop entirely;
it gets its own section near the end.

## The examples

Transcripts on this page run against one small demo project — a
`composer.json` declaring PSR-4 `Acme\` → `src/`, and two source files.

`src/Importer.php` carries a wrong `@throws` envelope twice over:

```php
<?php

declare(strict_types=1);

namespace Acme;

final class Importer
{
    /** @throws \LogicException */
    public function run(): void
    {
        throw new \RuntimeException('disk full');
    }

    /** @throws \LogicException */
    public function runAll(): void
    {
        $this->run();
    }
}
```

`run()` throws the undeclared exception in its own body. `runAll()` declares
the same envelope and lets the same exception escape through a call. Those
two shapes are what separate the first two stages below.

`src/Report.php` reads an optional key out of a declared shape:

```php
<?php

declare(strict_types=1);

namespace Acme;

final class Report
{
    /** @param array{host: string, port?: int} $dsn */
    public function port(array $dsn): int
    {
        return $dsn['port'];
    }
}
```

The `check` transcripts below came from the v0.1.2 binary against PHP 8.5.8.
Every **id count** on this page was re-measured on the current build, so the
findings a transcript prints and the surface sizes quoted around it come from
different binaries — the counts are the ones to trust.

## The lenient-default principle

Defaults are lenient. Strictness is opt-in, and a project declares it by name
in config (ADR-0050). Steins never guesses how much debt reporting you want,
because a project's appetite for it tracks its modernization stage, and that
is a fact about your team rather than about your source. The repo declares it,
reviewably, in a file that shows up in a diff.

## The five named profiles

A profile selects over the diagnostic *layers* —
[chapter 4](04-findings.md) defines `proof`, `contract`, `mechanics`, and
`debug`, and what kind of claim each one makes. `mechanics` prints in every
profile. `debug` sits outside the ladder entirely. The stages differ in how
much of the **contract** layer they put on the surface.

Every diagnostic id carries the lowest **rung** that admits it, and three of
the five built-ins are that ladder: `default ⊂ contracts ⊂ strict`. One layer
can hold ids at two rungs, which is what the contract layer does today
(ADR-0062). The other two sit beside the ladder rather than on it:
`throws-direct` is `default` plus one faceted id, and `pedantic` is
`contracts` plus the house-style asks.

- **`default`** — proof and mechanics. Only what provably breaks, plus
  anti-rot. This is a bare `check`.
- **`throws-direct`** — default plus `throw.undeclared` findings whose escape
  starts in the annotated declaration's *own body* (`origin = direct`). The
  high-signal subset: a `@throws` that is wrong about the method you are
  reading.
- **`contracts`** — default plus the contract layer's main body. Every
  `throw.undeclared`, direct and propagated; `phpdoc.*` mismatches;
  effect-envelope violations; and `offset.undeclared`, a read of a key the
  declared array shape proves is not there.
- **`strict`** — contracts plus the strict rung: the ids that make a
  *weaker* claim than their default-surface siblings, because they hold on
  some paths rather than all. `offset.maybe-missing` is the shape — a read
  of a key the declared shape marks *optional*, on a path where no guard
  discharges it. This stage asks you to prove presence.
- **`pedantic`** — contracts plus the **house-style** asks: rules about how
  code should be written, where Steins itself has no finding to make. Today
  that is `untyped.class-constant`, a class constant with no native type and
  no `@var`. Steins does not ask for that declaration on its own account —
  a constant's initializer is a constant expression, so its type is pinned
  either way — but a team that wants every constant annotated can say so
  here.

`pedantic` is a **branch off `contracts`, not a step above `strict`**, and
the two are independent: `strict` never reports `untyped.class-constant`,
`pedantic` never reports `offset.maybe-missing`. Wanting your constants
annotated and wanting the some-paths-only claims are separate decisions, so
neither profile forces the other. If you want both, extend one and name the
rest:

```toml
[profile.everything]
extends = "strict"
enable = ["untyped.class-constant"]
```

The same shape takes **one** pedantic rule without the profile — `enable` is
independent of the rung, so it works from any base:

```toml
[profile.house-style]
extends = "contracts"
enable = ["untyped.class-constant"]
```

The counts climb as the surface widens. Same code, the ladder in order:

```
$ steins check src/                          # default
$ echo $?
0
```

```
$ steins check --profile throws-direct src/
src/Importer.php:12:9: error[throw.undeclared]: RuntimeException can escape Importer::run() but is not declared (@throws LogicException) — proven escape
$ echo $?
1
```

```
$ steins check --profile contracts src/
src/Importer.php:12:9: error[throw.undeclared]: RuntimeException can escape Importer::run() but is not declared (@throws LogicException) — proven escape
src/Importer.php:12:9: error[throw.undeclared]: RuntimeException can escape Importer::runAll() but is not declared (@throws LogicException) — proven escape
```

```
$ steins check --profile strict src/
src/Importer.php:12:9: error[throw.undeclared]: RuntimeException can escape Importer::run() but is not declared (@throws LogicException) — proven escape
src/Importer.php:12:9: error[throw.undeclared]: RuntimeException can escape Importer::runAll() but is not declared (@throws LogicException) — proven escape
src/Report.php:12:16: error[offset.maybe-missing]: offset 'port' may be missing — $dsn is non-empty-array{host: string, port?: int}, which declares the key optional, and no guard on this path discharges it; reads null with "Undefined array key "port""
```

Zero findings, then one, then two, then three. Both `throw.undeclared` lines
point at the same `throw` statement on line 12; the escaping function named in
the message tells them apart. `contracts` added `runAll()`, the propagated
escape that `throws-direct` holds back.

`steins doctor` prints the resolved id count for the profile your config
selects, which is the authoritative answer for your build. Two lines out of
its `Config + active surface` section:

```
$ steins doctor --no-php .
  active profile: `default` (from built-in default)
  surface: layers [mechanics, proof], 47 checked id(s)
```

| Profile | Base | What it adds | Checked ids |
| --- | --- | --- | --- |
| `default` | — | — | 47 |
| `throws-direct` | `default` | `throw.undeclared`, direct escapes only | 48 |
| `contracts` | `default` | the contract layer, except the strict and pedantic rungs | 61 |
| `strict` | `contracts` | the some-paths-only claims — `offset.maybe-missing`, `variable.maybe-undefined`, `property.maybe-undefined`, `type.return-maybe-missing` | 65 |
| `pedantic` | `contracts` | the house-style asks — `untyped.class-constant` | 62 |

Only the `default` / `contracts` / `strict` rows nest. `throws-direct` and
`pedantic` branch off their base, so neither contains nor is contained by
`strict` — 62 and 65 are not steps on one scale, they are two different
supersets of the same 61.

`boundary` is a reserved name (ADR-0050 §5, deferred to ADR-0042). Selecting
it or defining `[profile.boundary]` is a config error until its design lands.

## The baseline ratchet

Named stages are only usable if raising one does not bury you. The baseline is
what makes a stage change survivable: capture today's findings once, and only
findings that appear *afterwards* fail CI.

> **If you know PHPStan or Psalm:** you would raise `level` and regenerate
> `phpstan-baseline.neon`. Same shape, two differences worth knowing. The
> ratchet file is machine-managed JSONL keyed by a content hash rather than a
> line number, so unrelated edits do not rot it. And it records the surface it
> was captured under, so widening past that surface produces a notice instead
> of a silently-under-covering baseline.

### Where the file lives

The conventional filename is **`.steins-baseline.jsonl`**, with the leading
dot, in the directory you run `check` from. `--set-baseline` writes there when
you pass no `--baseline`, and a later `check` from the same directory picks it
up with no flag at all:

```
$ steins check --profile throws-direct --set-baseline src/
steins: wrote 1 baseline entries to .steins-baseline.jsonl (profile `throws-direct`)
$ steins check --profile throws-direct src/
1 findings in baseline
$ echo $?
0
```

That second command loaded the baseline because the file was sitting in the
current working directory. Pass `--baseline <path>` when it is not: a
non-conventional filename, a monorepo where the file lives beside a
subproject's `composer.json`, or any CI job whose working directory differs
from the repo root.

```
$ steins check --profile throws-direct packages/api/src
packages/api/src/Importer.php:12:9: error[throw.undeclared]: RuntimeException can escape Importer::run() but is not declared (@throws LogicException) — proven escape
$ steins check --profile throws-direct --baseline packages/api/.steins-baseline.jsonl packages/api/src
1 findings in baseline
```

Auto-loading keys on the working directory alone, so the first run above found
no baseline and reported the finding it should have frozen. A `--baseline`
path that names nothing behaves the same way: `check` reports the full surface
as if no baseline existed, with no error. Both failure modes look identical
from the log, and `steins doctor` is what catches them, because it names the
file it found and what state it is in:

```
$ steins doctor --no-php .
Baseline
  file: .steins-baseline.jsonl (1 entry)
  capture surface: profile `throws-direct`, 48 id(s)
  active surface: profile `default`, 47 id(s)
  1 dormant entry (id outside the active surface — kept, not stale)
```

Commit the file. It is machine-managed: a header line recording the capture
surface, then one `{"id","path","hash"}` entry per finding. The hash covers
the id, the relative path, and the flagged line plus its nearest non-empty
neighbors, which is why there are no line numbers in it and why it survives
edits elsewhere in the file (ADR-0022). Do not hand-edit it.

### The loop

1. **Adopt on `default`.** Get a clean, proof-only run first. Proof-layer
   findings are runtime breaks; fix them rather than freezing them.
2. **Capture at the stage you want to move to**, with `--set-baseline`. The
   capture run reports nothing and exits `0`.
3. **Raise the profile** in `steins.toml` (below) and re-run. A fully
   baselined run is quiet, and prints one count so the freeze stays visible.
4. **Burn down.** Fix a frozen finding and the entry stops matching:

   ```
   $ steins check --profile throws-direct src/
   1 baseline entries no longer match (stale — rerun --set-baseline)
   $ echo $?
   0
   ```

   Recapture to collect the win. The file only ever shrinks, and a zero-entry
   baseline means the stage is paid off:

   ```
   $ steins check --profile throws-direct --set-baseline src/
   steins: wrote 0 baseline entries to .steins-baseline.jsonl (profile `throws-direct`)
   ```

`--ignore-baseline` shows the full unfiltered surface without touching the
file, which is how you see the whole debt again.

### It drowns loudly

Raise the profile past the surface the baseline was captured under and the new
findings are unbaselined. The run says so, on its own line, and fails:

```
$ steins check --profile contracts src/
src/Importer.php:12:9: error[throw.undeclared]: RuntimeException can escape Importer::runAll() but is not declared (@throws LogicException) — proven escape
1 findings in baseline
active profile `contracts` surfaces 13 id(s) the baseline (captured under `throws-direct`) did not — those findings are unbaselined (rerun --set-baseline to capture them)
$ echo $?
1
```

`contracts` checks 61 ids and the `throws-direct` capture covered 48, so 13
ids on today's surface have no frozen past. The propagated escape is one of
them, and it fails the build. Recapture under `contracts` and it goes quiet.

The header also draws the line between **stale** and **dormant**. A stale
entry is one whose id is on the current surface and no longer matches, so the
debt is paid and the entry should go. A dormant entry names an id the current
profile does not check at all; it is kept, not counted against you, and comes
back into force when you raise the stage again.

Flag-by-flag detail for `--baseline`, `--set-baseline`, and
`--ignore-baseline` is in [the CLI reference](02-cli-reference.md). The
CI-side loop — where the capture runs, what the job does with a stale count —
is in [CI integration](06-ci.md).

## User profiles in steins.toml

Built-in stages cover the common ladder. A repo composes its own named
surfaces in `steins.toml` at the project root, and there is deliberately no
ad-hoc `--enable id,id` flag: an unnamed surface is unreviewable in CI history
(ADR-0023). Config carries intent.

A worked example, a migration stage that surfaces the whole contract layer
while keeping `throw.*` from failing the build:

```toml
[check]
profile = "migration"

[profile.migration]
extends = "contracts"
warn    = ["throw.*"]
```

`extends` names a built-in or another user profile. `enable`, `disable`, and
`warn` take prefix id-arrays. [The configuration
chapter](03-configuration.md) owns the key-by-key semantics, including which
patterns are a config error.

Under that config, both `throw.undeclared` escapes surface as `warning` and
the run exits `0`:

```
$ steins check src/
src/Importer.php:12:9: warning[throw.undeclared]: RuntimeException can escape Importer::run() but is not declared (@throws LogicException) — proven escape
src/Importer.php:12:9: warning[throw.undeclared]: RuntimeException can escape Importer::runAll() but is not declared (@throws LogicException) — proven escape
$ echo $?
0
```

`doctor` confirms which surface resolved and where it came from:

```
$ steins doctor --no-php .
Config + active surface
  steins.toml: found
  active profile: `migration` (from [check] profile)
  surface: layers [contract, mechanics, proof], 25 checked id(s)
```

An explicit `--profile default` on the command line overrides the config's
`profile = "migration"`. Invocation intent beats the repo default, so a
developer can narrow the surface for one run without editing config.

## Exit-level semantics

Every surfaced finding carries a level, `fail` by default in every layer. If a
profile put a finding on the surface, somebody asked for it, and CI should see
it. A profile's `warn = [...]` demotes matching ids to report-without-fail.

- `0` — nothing fail-level displayed. **A warn-only run exits `0`**, which is
  what `warn` means.
- `1` — a fail-level finding was displayed.
- `2` — usage or config error: an unknown profile, an `extends` cycle, a
  pattern naming no registered id, a path that does not exist.

`warn` is how you introduce a stage to a noisy repo without a baseline at all.
The findings print in every CI log, the build stays green, and you delete the
`warn` entry when the count reaches zero.

## Mechanics ids always print

`suppress.unmatched`, `suppress.unknown-id`, and `effect.unknown-label` are
the **mechanics** layer: findings whose *absence* would silently rot another
channel. A stale `@steins-ignore` that nothing removes is an ignore nobody
ever cleans up. A typo'd effect label silently disables the envelope that
contains it.

So they print in every profile, they default to `fail`, and neither `disable`
nor `warn` reaches them. A profile that tries either changes nothing:

```toml
[profile.quiet]
extends = "default"
disable = ["effect.*", "suppress.*"]
warn     = ["effect.*", "suppress.*"]
```

```
$ steins check src/Fetcher.php
src/Fetcher.php:9:7: error[effect.unknown-label]: unknown effect label 'io.netwrok' in #[\Steins\Effect] on Fetcher::fetch()
$ echo $?
1
```

`warn` matters here as much as `disable` does: a mechanics id that could be
demoted to a report-without-fail would let a stale `@steins-ignore` render as
a harmless `warning[suppress.unmatched]` and exit `0` — the exact rot the
mechanics layer exists to catch, reopened through a different door. Neither
channel gets one.

They are also exempt from the baseline. `--set-baseline` writes zero entries
for a run whose only finding is a `suppress.unmatched`, so a stale ignore
cannot be frozen into the file and forgotten. And an `@steins-ignore` naming
`suppress.unmatched` reports `suppress.unmatched` — suppressing the
suppressor would close a loop. [Configuration](03-configuration.md) lists this
alongside the two other things `steins.toml` will never grow.

## Inline `@steins-ignore`

Suppress one finding at its site with a comment naming the id. Placement
follows `@phpstan-ignore` exactly: a comment **trailing code on a line**
suppresses matching findings on *that* line, and a comment **alone on its own
line** suppresses findings on the *next* line. An optional reason in
parentheses is ignored by the parser and read by your reviewers.

```php
<?php

declare(strict_types=1);

namespace Acme;

function charge(int $cents): void {}

// @steins-ignore type.argument-mismatch (legacy caller, tracked in #412)
charge("1200");

charge("2400"); // @steins-ignore type.argument-mismatch
```

```
$ steins check src/Legacy.php
2 diagnostics suppressed by inline ignores
$ echo $?
0
```

The scope is exactly one line and the ids you name. `// @steins-ignore a, b`
takes a comma-separated list, and prefix patterns work, so
`// @steins-ignore type.*` covers the family. Nothing reaches a second
statement, a whole function, or a whole file.

The ignore is **anti-rot**. Fix the code and the ignore stops matching, which
fails the run rather than passing quietly. Change that first call to
`charge(1200)` and leave the comment behind:

```
$ steins check src/Legacy.php
src/Legacy.php:9:1: error[suppress.unmatched]: @steins-ignore of type.argument-mismatch matches no diagnostic on line 10
1 diagnostics suppressed by inline ignores
$ echo $?
1
```

The dead ignore reds the build until somebody deletes it, and the live one on
line 12 keeps working. A misspelled id behaves the same way through a
different id: `suppress.unknown-id` fires *and* the finding it meant to
suppress prints underneath. Both are in the catalogue in
[chapter 4](04-findings.md).

> **If you know PHPStan or Psalm:** the notation is `@phpstan-ignore`'s,
> deliberately, down to the placement rule. What is missing is the escape
> hatch: no `@psalm-suppress all`, no message matching, no file-level or
> block-level form. And `suppress.unmatched` is always on, where PHPStan's
> `reportUnmatchedIgnoredErrors` is a setting you can switch off.

## Which mechanism, and when

Three tools, three jobs. Reaching for the wrong one is how a suppression file
starts growing.

**Raise a stage, then baseline** when you are turning on a new class of
reporting over a codebase that predates it. The debt is real, it is large, and
it is not the change you are making today. Capture it, get CI green, burn it
down in its own PRs. This is the only mechanism sized for hundreds of
findings, and it is the one designed to shrink on its own.

**Use `@steins-ignore`** for a single site you looked at and decided to keep.
It lives at the code, in the diff, next to the reason you wrote in
parentheses — which is exactly where the next reader needs it. If you find
yourself adding the fifth one for the same id, you wanted a baseline or a
profile.

**Choose a profile in `steins.toml`** when the question is team policy rather
than any particular finding. `profile = "contracts"` says this repo checks its
declared contracts. A `warn = ["throw.*"]` entry says this repo is working on
its `@throws` envelopes and has not finished. Both statements belong in a file
under review, and both survive the person who made the call.

For a proof-layer finding, none of the three is the right answer. Steins was
held to a zero-false-positive bar before it was allowed to claim your program
breaks; if it is wrong, that is a bug worth reporting rather than a line worth
suppressing.

## A second baseline: the effects surface

`steins effect-diff` has its own capture-and-diff loop, separate from `check`
and separate from the ratchet above. It records what effects each function
*has* and reports how today's differ from a capture. Nothing about it feeds
`check`: different file, different format, no profiles, no suppression, no
verdict.

Its conventional filename is **`steins-effects-baseline.json`**, with no
leading dot, and `--baseline <path>` moves it. Add a third file to the demo
project, an `Acme\Counter` whose `bump()` echoes, then capture before the
refactor:

```
$ steins effect-diff --set-baseline src/
steins: wrote 4 effect summaries to steins-effects-baseline.json
```

Delete the `echo` and diff. One line:

```
$ steins effect-diff src/
src/Counter.php Acme\Counter::bump: - output
$ echo $?
0
```

That exit code is the point. `effect-diff` reports and never fails, whether
the diff is empty or a hundred lines long, because an effect change is
information for a reviewer rather than a verdict on the code. Wire it as a PR
comment; it was never designed to gate a build. The event vocabulary —
`+ label`, `- label`, `≤→ label`, the coverage notes, the added/removed
footer — and the JSON form are in [the CLI reference](02-cli-reference.md).

Reach for it when you are asserting that a change is effect-neutral: a
constructor extraction, a caching layer, a move to a repository class. The
diff either agrees with you or names the function where you were wrong.

## The dump ids

The `debug` layer is the fourth kind of claim, and it answers a question you
asked in the source (ADR-0053). `debug.type` reports what `PHPStan\dumpType()`
saw, `debug.phpdoc-type` the same for `dumpPhpDocType()`, `debug.trace` the
same answer as `debug.type` for a `/** @psalm-trace $x */` (or
`@phpstan-trace`) docblock above a statement — the committable spelling of
the question, answered against that statement's *exit* facts (ADR-0074) —
and `debug.var-dump` reports the engine's inferred facts at every default-on
`var_dump()` call.

The layer sits outside the profile ladder: every stage shows dumps, and
raising or lowering a stage changes nothing about them. The levels differ on
purpose. The explicit pair is fail-level, because `PHPStan\dumpType` is not a
real PHP function and a committed call is a guaranteed fatal. `debug.var-dump`
is warn-level and exit-neutral by construction, since a leftover `var_dump()`
is legal working PHP and reddening a build over it would invert the
quiet-default identity. `debug.trace` is warn-level and exit-neutral for the
mirror reason: its trigger is a runtime-inert comment that is legal to
commit, so the question is answered visibly on every run without holding CI
hostage.

`@steins-ignore` does not reach them — an ignore naming `debug.type` reports
`suppress.unmatched`. The remedy for an unwanted dump is deleting the call
(or, for a trace, the comment). Silence `var_dump()` reporting for a whole
repo with `disable = ["debug.var-dump"]` in a named profile; `debug.trace`
has no such switch — an annotation is always an authored question, never an
incidental call somebody else wrote. [Chapter 4](04-findings.md) shows what
each one prints.

## Where to go next

- **Every flag on `check` and `effect-diff`:** [the CLI
  reference](02-cli-reference.md).
- **Every `steins.toml` key:** [configuration](03-configuration.md) —
  including the `[profile.<name>]` table this chapter selects from.
- **What a finding means:** [findings](04-findings.md) — the id catalogue,
  the anatomy of a message line, and the four layers.
- **Running the ratchet in CI:** [CI integration](06-ci.md) — where the
  capture belongs, and what the job does with a stale count.
