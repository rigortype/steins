# Troubleshooting

Something is wrong and you want to know what, fast. This chapter has three
parts: `steins doctor`, the command that answers "what does my setup look
like" before you go digging; the sidecar failure modes, one entry per
distinct thing the binary can tell you went wrong with PHP; and a
symptom-indexed list of the problems people hit on a first run.

Every transcript below is real output from the binary described in
[installation and quickstart](01-installation-and-quickstart.md), run
against PHP 8.5.8. Long sections are trimmed with `…`; nothing is invented.

## Start with `steins doctor`

When something looks wrong — a finding you expected is missing, a run is
slower than you remember, `check` behaves differently in CI than on your
laptop — run `steins doctor` before anything else. It reads your
configuration, your environment, and your project's index, and prints what
it finds. It runs no checks and never touches your code's correctness; a
clean `doctor` report and a red `check` are not in tension, and a degraded
`doctor` report does not mean your code is broken (ADR-0054 §8).

For the full flag list, the exit-code table, and the JSON output that does
not exist yet, see [the CLI reference's `doctor` entry](02-cli-reference.md#doctor).
This section is about reading what it prints.

### The six sections

**Runtime.** Which `php` answered, its version and SAPI, how many
extensions it loaded, and the PHP version range your project declares (from
`composer.json`'s `require.php` or `config.platform.php`). If the runtime's
version sits above or outside that declared range, a `version skew` line
says so — see [sidecar failure modes](#sidecar-failure-modes) below.

**Config + active surface.** Whether `steins.toml` was found and parsed,
which profile is active and why (a `--profile` flag, `[check] profile`, or
the built-in default), and how many diagnostic ids that profile surfaces.
This is the fastest way to confirm a config change took effect —
see ["profile in steins.toml ignored"](#profile-in-steinstoml-ignored)
below for the most common way it silently does not.

**Layout.** Which `composer.json` manifests govern the tree you pointed at,
and which directories under each one count as vendor versus first-party.
A dependency's findings hiding when you expected them, or your own code
being treated as vendor, traces back to this section.

**Coverage posture.** What doctor's index scan found and what it could not
reason about: how many scopes are "poisoned" (a construct like `extract()`
made every local in that scope unknowable), how many dam sites exist
(`eval`, a dynamic `include`, a runtime-named `class_alias` — places where
Steins cannot enumerate every symbol that might exist), and how much
reflection-driven invocation the syntactic recognizer spotted. None of this
is a finding; it is the honest account of what a quiet run is quiet
*about*. See
["check prints nothing — is it working?"](#check-prints-nothing--is-it-working)
below.

**Envelopes.** How many declarations carry a written `@throws` tag, and
whether the active profile checks them. `default` does not; a
written envelope with silence around it is the single most common "wait,
why didn't that fire" moment, and this line is the direct answer.

**Baseline.** Which baseline file resolved (an explicit `--baseline`, or
the conventional default when present), how many entries it holds, and —
when the file carries a capture-surface header — whether the active
profile's surface has grown past what was captured. See
["baseline not applying"](#baseline-not-applying) below for the usual cause
of "none" here when you expected a file.

### A healthy run

A small project — one `composer.json` declaring `"php": "^8.3"`, one
`Greeter` class:

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
  1 file(s), 3 scope(s), 0 poisoned (0.0%) — a poisoned scope knows no local's value (ADR-0001, ADR-0046 §1)
  opaque constructs: none — no scope is on the give-up list
  dam sites: none — no runtime-definition construct stands, so existence-absence claims are undammed (ADR-0049 §2)
  reflection-driven invocation: none recognized
    (this list is a guess until measured: the recognizer is syntactic, it names no receiver type, and it is not exhaustive)

Envelopes
  0 written throw envelope(s); the active profile `default` does not check them — the `contracts` (or `throws-direct`) profile does

Baseline
  none (no baseline file; `check --set-baseline` writes one)
$ echo $?
0
```

Nothing here is wrong, and most of it is unremarkable — that is the point.
`version skew` fires even on a clean run: it is a posture fact, not a
problem, because PHP 8.5 running code declared for 8.3+ is completely
normal and the line only ever says what that means for reflection-sourced
proofs (below).

### An unhealthy run

The same project, with a `steins.toml` that has a broken table header —
`[profile.migration` with a missing closing bracket:

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
  steins.toml: PARSE ERROR — steins.toml: parse error (TOML parse error at line 4, column 19
  |
4 | [profile.migration
  |                   ^
invalid table header
expected `.`, `]`
)
  (configuration contradiction — doctor exits 1, ADR-0054 §10)
  active profile: `default` (from built-in default)
  surface: layers [mechanics, proof], 16 checked id(s)

Layout
  1 manifest(s) govern this tree:
    composer.json
      vendor: vendor
      ours:   src

Coverage posture
  1 file(s), 3 scope(s), 0 poisoned (0.0%) — a poisoned scope knows no local's value (ADR-0001, ADR-0046 §1)
  opaque constructs: none — no scope is on the give-up list
  dam sites: none — no runtime-definition construct stands, so existence-absence claims are undammed (ADR-0049 §2)
  reflection-driven invocation: none recognized
    (this list is a guess until measured: the recognizer is syntactic, it names no receiver type, and it is not exhaustive)

Envelopes
  0 written throw envelope(s); the active profile `default` does not check them — the `contracts` (or `throws-direct`) profile does

Baseline
  none (no baseline file; `check --set-baseline` writes one)
$ echo $?
1
```

Two things worth noticing. First, only the "Config + active surface"
section changed — Runtime, Layout, Coverage, Envelopes, and Baseline still
render in full, because doctor parses the config once and every later
section falls back to the built-in `default` surface rather than aborting
(ADR-0054 §9.3). Second, the exit code: `1`, not `2`. A broken
`steins.toml` is a *configuration contradiction* — the repo asserts a
profile that cannot resolve — and that is different from a usage error like
a typo'd path or an unknown flag, which exits `2`. The distinction is
argv versus committed intent: `steins doctor nope/` (a path that is not
there) and `steins doctor --format json` (a flag doctor does not have) both
exit `2` and are covered in
[the CLI reference](02-cli-reference.md#doctors-three-exits). A malformed
`steins.toml`, an unresolvable profile, or an unparseable baseline header
are the only three things that make doctor exit `1`; everything else it can
report — no PHP on `PATH`, a monkey-patching extension loaded, a dormant
baseline entry — reports at exit `0`, because a fact about your environment
is not a problem doctor is entitled to fail the run over (ADR-0054 §10, the
crying-wolf prohibition also argued in ADR-0004).

## Sidecar failure modes

Steins types literal values by executing your project's own PHP over IPC
(the sidecar — see
[installation and quickstart](01-installation-and-quickstart.md#requirements)
for what it is and why it exists). Five distinct outcomes are worth telling
apart, because `steins doctor`'s Runtime section names each one differently
and the fix is different for each.

### No `php` on `PATH`

**What you see.** Nothing dramatic — `check` degrades quietly. It prints
the sound-subset notice on stderr and keeps going:

```
$ env PATH=/usr/bin:/bin steins check .
note: running as sound subset (no PHP sidecar) — findings that require executing PHP are omitted, and builtin return types come from the catalog's declarations, unverified
./src/Greeter.php:16:22: error[type.argument-mismatch]: argument null to Greeter::greet() cannot become string $name — proven TypeError (coercive mode)
$ echo $?
1
```

`doctor` names the cause directly:

```
$ env PATH=/usr/bin:/bin steins doctor
…
Runtime
  PHP sidecar: not spawnable (no `php` on PATH)
  note: running as sound subset (no PHP sidecar) — findings that require executing PHP are omitted, and builtin return types come from the catalog's declarations, unverified
  (a degraded environment is not a failure — exit stays 0, ADR-0004)
…
$ echo $?
0
```

**Cause.** `php` is not resolvable on `PATH` at spawn time — a bare CI
image, a container that only ships the analyzer binary, a `PATH` trimmed by
a wrapper script.

**Fix.** Install PHP (any recent version — the sidecar speaks a small,
stable introspection protocol) and confirm it is on `PATH` with `which
php`, or run `steins check --no-php` deliberately and accept the narrower
proof set below.

### `--no-php` chosen deliberately

**What you see.** The same sound-subset notice, but doctor's Runtime
section reads differently — `disabled (--no-php)`, not
`not spawnable`:

```
$ steins doctor --no-php
…
Runtime
  PHP sidecar: disabled (--no-php)
  analysis target: PHP 8.3 (8.x) (from require.php "^8.3")
  posture: sound subset — findings that require executing PHP are omitted
  (a degraded environment is not a failure — exit stays 0, ADR-0004)
…
```

**Cause.** You asked for this. `--no-php` skips the sidecar outright, for a
faster run, a sandbox with no PHP install, or reproducing a CI box that
never has one.

**Fix.** None needed — this is a mode, not a failure. Know what it costs:
the zero-false-positive bar still holds (nothing false is ever added), but
the **absence family** — `call.undefined-function`, `class.undefined`, and
`call.undefined-method`'s builtin/extension check — goes quiet, because
those ids need a live PHP to rule out every candidate name. See
["findings vanished after adding `--no-php`"](#findings-vanished-after-adding---no-php)
below for what that looks like on real code.

### The sidecar spawns but never answers

**What you see.** `steins doctor` reports a third, distinct Runtime line —
not "not spawnable", but "spawned, but the env() query failed":

```
$ env PATH=/path/to/broken-php-wrapper steins doctor
…
Runtime
  PHP sidecar: spawned, but the env() query failed
  posture: sound subset (degraded) — findings that require executing PHP are omitted (exit 0, ADR-0004)
…
```

**Cause.** `php` exists on `PATH` and the process starts, but it never
answers the sidecar's opening handshake — a `php` wrapper script that never
execs real PHP, a broken `php.ini` that hangs on startup, an
`auto_prepend_file` that never returns, a `php` built without the pieces
the sidecar's inline script needs. This is the "protocol mismatch" case:
the process is alive, but it is not speaking the sidecar's JSON-RPC
framing, so the request times out and the whole run poisons and continues
sound-subset for the rest of the invocation.

**This is the one failure mode `check` never mentions.** Unlike the
"no `php` on `PATH`" case, a spawn that succeeds but then fails to answer
prints **no notice at all** on `check` — the request silently widens to
`FoldResult::Widen` per call, which is correct (nothing false is ever
reported) but invisible. If a run feels thinner than it should and `which
php` finds something, `steins doctor` is the only place that says why:

```
$ env PATH=/path/to/broken-php-wrapper steins check .
./src/Greeter.php:16:22: error[type.argument-mismatch]: argument null to Greeter::greet() cannot become string $name — proven TypeError (coercive mode)
$ echo $?
1
```

Same finding, same exit code, zero indication anything degraded — `check`
still proves what it can prove from source alone. Run `doctor` whenever a
run's yield looks suspiciously thin. Issue #110 tracks surfacing this on
`check` itself.

**Fix.** Confirm `php -v` runs a working interpreter from a plain
shell (not through whatever wrapper `PATH` resolves inside your CI runner
or `direnv` shim), and that nothing in `php.ini` — an `auto_prepend_file`,
a broken extension `zend_extension` line — hangs or fatals on the bare
`php -r '...'` invocation the sidecar uses.

### Version skew: runtime above the declared floor

**What you see.** Doctor's Runtime section, on an otherwise clean run:

```
Runtime
  …
  analysis target: PHP 8.3 (8.x) (from require.php "^8.3")
  version skew: runtime 8.5 sits above the 8.3 floor — reflection describes the runtime, so symbols newer than the floor are not proven absent for it (silence, never a false claim)
```

**Cause.** Your `composer.json` declares `"php": "^8.3"` (or similar), and
the `php` the sidecar found is 8.5 — newer than the floor, still inside the
open-ended range. This is completely normal: most projects declare a floor
and run whatever is newest on the box.

**Fix.** None — this is informational, not a problem. It exists so an
absence claim about a symbol that exists on 8.5 but not on 8.3 is never
silently assumed false: Steins would rather say nothing than assert a
symbol is absent for a floor version it cannot reflect against.

### Version skew: runtime outside the declared range

**What you see.** The stronger version of the same line, when the runtime
falls entirely outside the declared range instead of merely above the
floor — a project pinned to `"php": "^7.4"` analyzed with PHP 8.5 on
`PATH`:

```
Runtime
  …
  analysis target: PHP 7.4 (7.x) (from require.php "^7.4")
  version skew: runtime 8.5 is OUTSIDE the declared range — the absence family and reflection-seeded facts are disabled this run (the boot surface is not a version this project ships on)
```

**Cause.** The `php` available to the sidecar cannot run the code you
declared support for — a project's CI image moved to a newer PHP, a
developer's local PHP moved on, or a Docker base image bumped past what
`composer.json` still claims.

**Fix.** Either install a `php` inside the declared range for the most
faithful run, or accept the degradation: the absence family and
reflection-seeded facts go quiet for this run specifically (not a crash,
not a false claim — just a narrower proof set), because a fact reflected
from PHP 8.5 is not a fact about PHP 7.4. Update `composer.json`'s
`require.php` once the project's real floor has moved, so doctor stops
reporting skew against a version nobody runs anymore.

## Common first-run problems

Symptom first, because that is what you searched for.

### "check prints nothing — is it working?"

**Symptom.** `steins check .` exits `0` and prints no findings on a project
you know has bugs.

**Cause.** Two things explain this, in order of likelihood:

1. **A bare `check` is proof-only.** It reports exactly what provably
   breaks at runtime — nothing that merely looks risky. If your bug is a
   contract violation (an undeclared `@throws`, a PHPDoc type mismatch) it
   lives behind a profile you have not asked for yet; see
   ["it missed an obvious bug"](#it-missed-an-obvious-bug) below and
   [profiles, baseline, and suppression](05-profiles-and-baseline.md).
2. **Some of your code may be outside what Steins can reason about.** Run
   `steins doctor` and read the Coverage posture section: poisoned scopes,
   dam sites, and reflection-driven invocation are all places the analyzer
   is honestly silent rather than guessing. A project with heavy dynamic
   dispatch can have a large share of its code in this state, and a clean
   `check` over it is not the same claim as a clean `check` over
   straight-line code.

**Fix.** Run `steins doctor .` first and read Coverage posture's numbers.
Widen the profile (`--profile contracts` or `--profile strict`) for the
debt layer. Silence is never a safety claim: it means "not proven".

> **If you know PHPStan or Psalm:** a clean run at their highest level
> reads as "this code is type-safe end to end". A clean bare `steins check`
> reads narrower — "nothing here provably explodes at runtime" — because
> the type-coherence checking those tools do at every level lives behind
> Steins' `contracts`/`strict` profiles instead of the default surface.

### "it missed an obvious bug"

**Symptom.** Code that looks wrong to you — a nullable value used
without a guard, an array key that might not exist — produces no finding.

**Cause.** Steins is value-precise, not shape-precise: it needs to *prove*
what a value is, from a literal, a declared type, or a chain of folds back
to one. A `mixed`/nullable parameter fed from something Steins cannot fold
— `$_GET['name']`, a database row, an untyped return from code it cannot
see into — is honestly unknown, and an honestly unknown value produces no
finding at any layer. This is deliberate: guessing "probably null" and
reporting it would violate the zero-false-positive bar the whole tool is
built on.

```php
<?php

function loud(mixed $name): string
{
    return strtoupper($name);
}

function run(): string
{
    return loud($_GET['name'] ?? null);
}
```

```
$ steins check src/Maybe.php
$ echo $?
0
```

Nothing fires — not even under `--profile strict` — because nothing here
is *proven*, only plausible.

**Fix.** This usually is not a Steins bug to fix; it is a mismatch of
expectations. If the risk is real, the fix is in the code: narrow the type
at the boundary (`(string) ($_GET['name'] ?? '')`, or a real validation
layer) so the value Steins sees downstream is one it can reason about. A
tool that flags "possibly risky" code is answering a different question
than Steins does — see the handbook for what "proven" means here.

### "exit 2 on a path that used to exist"

**Symptom.** A `check`, `annotate`, `transform`, or `doctor` invocation
that worked yesterday now exits `2` with `steins: path does not exist:
<path>`.

**Cause.** The path argument you passed does not resolve — a directory got
renamed, a file moved, a CI step changed working directory. Every
subcommand that takes a path checks this before any analysis runs, on
purpose: a renamed directory should red the build loudly, not produce a
quietly empty, misleadingly clean report.

**Fix.** Fix the path. There is no flag to make a missing path a warning
instead of a hard `2` — see
[the CLI reference's exit-code table](02-cli-reference.md#exit-codes) for
the two cross-cutting rules every subcommand follows.

### "findings vanished after adding `--no-php`"

**Symptom.** A finding that fires with a normal `steins check` disappears
entirely under `--no-php`, and it is not the finding you were expecting to
lose.

**Cause.** The **absence family** — `call.undefined-function`,
`class.undefined`, and `call.undefined-method`'s builtin/extension check —
needs the sidecar to enumerate what exists on the running PHP.
Without it, Steins cannot tell "this function does not exist anywhere"
from "this function exists somewhere I cannot see", so it says nothing:

```php
<?php

function callIt(): void
{
    totally_not_a_real_function();
}
```

```
$ steins check src/Undef.php
src/Undef.php:5:5: error[call.undefined-function]: call to undefined function totally_not_a_real_function() — not defined in the project, not on PHP 8.5.8 (70 extensions)
$ echo $?
1

$ steins check --no-php src/Undef.php
note: running as sound subset (no PHP sidecar) — findings that require executing PHP are omitted, and builtin return types come from the catalog's declarations, unverified
$ echo $?
0
```

Value-precise mismatches that fold statically are unaffected — passing
`null` to a declared `string` parameter still fires under `--no-php`,
because that proof needs no runtime.

**Fix.** This is the documented cost of `--no-php`
([installation and quickstart](01-installation-and-quickstart.md#requirements)),
not a bug. If the absence family matters to you, get PHP back on `PATH`
instead — see the sidecar failure modes above.

### "baseline not applying"

**Symptom.** You ran `check --set-baseline`, the file was written, but a
plain `steins check` (no flags) still reports the baselined findings as if
nothing was captured.

**Cause.** The baseline auto-loads only under its exact conventional name,
**`.steins-baseline.jsonl`** — leading dot included. A file written under
any other name, including a plausible-looking `steins-baseline.jsonl` or
`baseline.jsonl`, is never picked up automatically:

```
$ steins check --set-baseline --baseline my-baseline.jsonl src/
steins: wrote 1 baseline entries to my-baseline.jsonl (profile `default`)
$ steins check src/
src/Greeter.php:16:22: error[type.argument-mismatch]: … — proven TypeError (coercive mode)
$ echo $?
1
```

**Fix.** Either write to the conventional name (drop `--baseline` entirely
— `check --set-baseline` alone writes `.steins-baseline.jsonl`), or pass
`--baseline <path>` on every subsequent invocation, including in CI. Run
`steins doctor` to confirm which file, if any, resolved — its
Baseline section names the file it found or says `none`. Full workflow in
[profiles, baseline, and suppression](05-profiles-and-baseline.md).

> **If you know PHPStan or Psalm:** both tools require the baseline file
> named explicitly, every time, in `phpstan.neon`'s `includes:` or
> `psalm.xml`. Steins autodiscovers instead, which is one flag fewer to
> wire up — but only under the one filename, so a rename that would be
> silent in an explicit `includes:` list is silent here too, just for a
> different reason: the file is never found at all.

### "unknown profile"

**Symptom.** ``steins: unknown profile `nope` (built-ins: default,
contracts, throws-direct, strict; or define [profile.nope])``, exit `2`.

**Cause.** A `--profile` flag or `[check] profile` names something that is
neither a built-in stage nor a `[profile.<name>]` table in `steins.toml` —
a typo, or a profile defined in a `steins.toml` that was not found (see the
next entry).

**Fix.** Check the spelling against the four built-ins
(`default`/`throws-direct`/`contracts`/`strict`), or define
`[profile.<name>]` if you meant a custom one. Full syntax in
[profiles, baseline, and suppression](05-profiles-and-baseline.md#user-profiles-in-steinstoml).

### "profile in steins.toml ignored"

**Symptom.** `steins.toml` sets `[check] profile = "..."`, but `steins
check` or `steins doctor` behaves as if it were not there — no error, just
the built-in `default` surface.

**Cause.** Config discovery is **current-working-directory only**: `check`
and `doctor` look for exactly `./steins.toml`, relative to wherever you ran
the command from. There is no walk-up to a parent directory and no search
for a project root. Run either command from a subdirectory of the repo and
a `steins.toml` sitting at the repo root is invisible — not an error,
because a missing file is a legitimate zero-config state, so it falls back
to the built-in default silently:

```
$ cd app/ && steins check src/
$ echo $?
0
```

That project's root `steins.toml` sets `profile = "migration"` with
`throw.*` demoted to warn — none of which applied, because `app/` has no
`steins.toml` of its own and the command never looked one level up.

**Fix.** Run `check`/`doctor` from the directory that holds `steins.toml`
— normally the repo root — or confirm what resolved with `steins
doctor`, whose "Config + active surface" section names the file status and
active profile plainly. Full discovery rules in
[the configuration chapter](03-configuration.md#discovery).

### "`@steins-ignore` fails the build"

**Symptom.** A line carrying `// @steins-ignore <id>` makes `check` exit
`1` with a `suppress.unmatched` error, instead of quietly suppressing
anything.

**Cause.** This is working as designed, not a bug. `@steins-ignore` is
**anti-rot**: if the id it names does not match any diagnostic on the line
it is attached to — because the code was fixed, or the id was mistyped —
the ignore itself becomes a fail-level finding rather than silently doing
nothing:

```php
<?php

function takesInt(int $x): int
{
    return $x;
}

function ignoreDemo(): int
{
    // @steins-ignore call.on-null
    return takesInt(5);
}
```

```
$ steins check src/Ignore.php
src/Ignore.php:10:5: error[suppress.unmatched]: @steins-ignore of call.on-null matches no diagnostic on line 11
$ echo $?
1
```

**Fix.** Delete the stale ignore comment (the finding it once suppressed is
gone) or correct the id if it was mistyped. `suppress.unmatched` is a
**mechanics**-layer id: it prints on every profile and cannot be disabled,
because a suppression channel that can silently rot is worse than no
suppression channel. See
[profiles, baseline, and suppression](05-profiles-and-baseline.md#inline-steins-ignore).

### "committed `dumpType` reds the build"

**Symptom.** CI goes red on a `\PHPStan\dumpType($x)` call somebody left in
during debugging, with a `debug.type` finding.

**Cause.** This is deliberate, not a bug. `dumpType()` and
`dumpPhpDocType()` are introspection calls — "what does Steins believe this
value is right now" — and they report at **fail level unconditionally**,
on every profile, out of reach of `@steins-ignore` and of the baseline by
design. A committed one is also a runtime fatal in real PHP if the shim is
not stripped; failing the build is the CI feedback that the debug call was
left in, the same posture PHPStan takes on its own `dumpType()`.

```php
<?php

function dumpDemo(int $x): void
{
    \PHPStan\dumpType($x);
}
```

```
$ steins check src/Dump.php
src/Dump.php:5:23: error[debug.type]: dumped type: int
$ echo $?
1
```

**Fix.** Delete the call before committing. There is no flag or profile
setting to soften this one — `debug.type`/`debug.phpdoc-type` are fixed at
fail level on purpose. If you want the answer to *survive* in committed
code, ask with the docblock spelling instead: a `/** @psalm-trace $x */`
above a statement reports the same rendering as a `debug.trace` at **warn**
level, exit-neutral, against the statement's exit facts — a comment is
runtime-inert and legal to commit (ADR-0074). A leftover `var_dump()`, by
contrast, reports its arguments' inferred facts at **warn** level by default
(exit-neutral) and *can* be silenced project-wide with
`disable = ["debug.var-dump"]` in a named profile, because `var_dump()` is
legal working PHP and `dumpType()` is not. See
[profiles, baseline, and suppression](05-profiles-and-baseline.md#the-dump-ids).

## Where to go next

- **Every flag, every exit code:** [the CLI reference](02-cli-reference.md).
- **`steins.toml` key by key:** [configuration](03-configuration.md).
- **Reading a finding, and the four layers:** [findings](04-findings.md).
- **Ratcheting strictness without a first run that drowns you:**
  [profiles, baseline, and suppression](05-profiles-and-baseline.md).
