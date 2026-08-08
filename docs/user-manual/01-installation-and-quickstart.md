# Installation and quickstart

A static analyzer reads your PHP without running it (or, for Steins, without
running *your* code — see below) and reports what looks wrong. Steins is a
**value-precise** one: instead of stopping at declared types, it folds
literal values through your code — `null`, `"5"`, `["a" => 1]` — and reports
only what it can prove breaks at runtime. This page gets you from an install
to reading that first run.

## Install

Four channels. They differ in *where the version lives*, which is the only
question worth thinking about.

**Composer** — when the analyzer should be pinned beside the code it
analyzes. The version goes in `composer.lock`, so CI and every developer
resolve the same one and `composer update` moves it deliberately. This is
the right default for a project.

```
composer require --dev typedduck/steins
```

What Composer installs is a small PHP shim, not the analyzer: on first use
it downloads the release binary matching the installed version, checks it
against the sha256 the release published, and runs it. Later runs use the
cached binary and touch the network not at all. Requires PHP 8.1+.

You end up with two packages. `typedduck/steins` is the shim, **Apache-2.0**,
and it requires `typedduck/steins-attributes` — the `#[\Steins\Pure]` and
`#[\Steins\Effect]` classes you write in your own source, **MIT** and inert
at runtime. They are separate packages because they are different kinds of
thing: one is tooling you run, the other is vocabulary meant to spread
(ADR-0025). The requirement is there so one command leaves you able to write
an envelope.

**Homebrew** — when you want `steins` on `PATH` for any project on the
machine. Requires Homebrew itself; no PHP prerequisite, because the binary
is self-contained and the PHP sidecar (below) is a separate, optional piece
your project supplies at analysis time.

```
brew install rigortype/tap/steins
```

**A prebuilt binary**, from the
[releases page](https://github.com/rigortype/steins/releases) —
`x86_64`/`aarch64` Linux (glibc), `x86_64` Linux (musl, static), and
`x86_64`/`aarch64` macOS. Each archive holds the bare `steins` binary and
carries a `.sha256` sidecar beside it. Windows is not shipped yet.

**From source**, with a Rust toolchain at 1.97 or newer. This is the
fallback for a platform with no prebuilt binary — notably arm64 Alpine,
where the musl build is x86_64-only.

```
cargo install --git https://github.com/rigortype/steins steins-cli
```

There is no crates.io package: the parser backend is a rev-pinned fork, and
crates.io does not accept crates with git dependencies. From a checkout of
this workspace, `cargo build --release -p steins-cli` (binary lands at
`target/release/steins`) or `cargo install --path crates/steins-cli` both
work.

The binary has eight subcommands and no `--help`; run it with no arguments
to see the surface:

```
$ steins
usage: steins check [--format text|json] [--profile <name>] [--no-php] [--vendor-diagnostics] [--fix] [--set-baseline] [--baseline <path>] [--ignore-baseline] <paths...>
       steins annotate [--no-php] [--format text|json] <file.php>
       steins transform <phpdoc-to-native|phpdoc-honesty|throws-envelope|loop-to-array-map> [--apply] [--asserted-subjects] [--format text|json] <paths...>
       steins effect-diff [--baseline <path>] [--set-baseline] [--format text|json] <paths...>
       steins doctor [--no-php] [--baseline <path>] [path]
       steins mcp
       steins version | -v | --version
       steins license
```

`check` is the one this chapter covers: it walks `.php` files and prints
findings. The rest — `annotate`'s margin view of inferred types and effects,
`transform`'s automated PHPDoc rewrites, `effect-diff`'s baseline for the
effects surface — get their own entries in
[the CLI reference](02-cli-reference.md).

`doctor` reports posture rather than findings — which `php` was resolved,
what the active profile checks, which trees count as vendor, baseline health
— and runs no checks at all. It is also the quickest way to confirm an
install works:

```
$ steins doctor --no-php
steins doctor — posture report (index-bound; runs no checks)

Runtime
  PHP sidecar: disabled (--no-php)
  analysis target: none declared — the runtime PHP is the target
  posture: sound subset — findings that require executing PHP are omitted
  (a degraded environment is not a failure — exit stays 0, ADR-0004)

Config + active surface
  steins.toml: not found (built-in defaults govern)
  active profile: `default` (from built-in default)
  surface: layers [mechanics, proof], 16 checked id(s)

Layout
  no composer.json governs . — vendor is the `vendor` directory-name floor, not a declared fact

Coverage posture
  no .php files under . — nothing to inventory

Envelopes
  0 written throw envelope(s); the active profile `default` does not check them — the `contracts` (or `throws-direct`) profile does

Baseline
  none (no baseline file; `check --set-baseline` writes one)
```

Doctor's own exits are deliberately narrow: `0` for any posture it can
report, including degraded ones (no PHP on `PATH` is a mode, not a failure);
`1` when the configuration asserts something the world refutes (an
unparseable `steins.toml`, an unknown profile, a bad baseline file); `2` for
doctor's own usage errors — including a `path` argument that does not
exist, which errors rather than reporting on some other directory
(ADR-0054 §10). Full flag list and section-by-section reading in
[the troubleshooting chapter](07-troubleshooting.md).

## Requirements

Steins types literals by executing the **project's own PHP** over IPC — its
version, its extensions, its `composer` autoload — so that a folded value is
"what this code produces on the runtime it actually runs on" (ADR-0004).
This is the sidecar. It runs `php`, not any file of yours: Steins never
executes your application code, only small introspection calls (constant
folding, function signatures) against the PHP binary itself. The sidecar is
default-on and lazily spawned; discovery is `php` on `PATH`.

> **If you know PHPStan or Psalm:** neither tool shells out to a live PHP
> during analysis — they emulate a target version from static tables. The
> sidecar is Steins' most consequential design choice: it trades "works with
> no PHP install" for "agrees with the PHP you actually run", including
> extension-dependent behavior static emulation cannot see.

If PHP is absent, or you pass `--no-php`, the run degrades to a **sound
subset** and says so on the first line:

```
note: running as sound subset (no PHP sidecar) — findings that require executing PHP are omitted, and builtin return types come from the catalog's declarations, unverified
```

The zero-false-positive bar still holds in the sound subset — nothing false
is added — but findings that need the runtime *widen away*. The **absence
family** goes quiet here: `call.undefined-function` and `class.undefined`
need the sidecar to answer "not defined on this PHP" for every candidate
name, and `call.undefined-method` needs it to rule out a builtin/extension
homonym (ADR-0049 §1, A2). Value-precise mismatches that fold statically
still fire — passing `null` where a declared `string` parameter sits is
provable from source alone, sidecar or not. Incompleteness is never silent:
the coverage posture is surfaced, not assumed, and `steins doctor` is where
you read it (see above).

## First run

Point `check` at a project (or any subtree); zero config, everything is
inferred from `composer.json` and autoload. Here is a minimal project —
`composer.json` plus a `Greeter` class — with one deliberate bug: `$name` is
provably `null`, and `greet()` declares a `string` parameter under
`strict_types`.

`src/Greeter.php`:

```php
<?php

declare(strict_types=1);

namespace Acme\Greeter;

final class Greeter
{
    public function greet(string $name): string
    {
        return "Hello, {$name}!";
    }
}
```

`src/main.php`:

```php
<?php

declare(strict_types=1);

namespace Acme\Greeter;

$greeter = new Greeter();
$name = null;

echo $greeter->greet($name);
```

```
$ steins check .
./src/main.php:10:22: error[type.argument-mismatch]: argument null (from $name, assigned at line 8) to Greeter::greet() cannot become string $name — proven TypeError (strict mode)
```

The run exits `1` because a fail-level finding was displayed. Each line is
`path:line:col: error[id]: message — proven <consequence>`. The `id`
(`type.argument-mismatch`) names the *finding*, not the rule that found it.
Fix `main.php` — pass a real string — and the same command exits `0` with no
output. Nothing to report is silent by design; that is the norm, not an
edge case.

`--vendor-diagnostics` is worth knowing about even though it belongs to the
CLI reference: vendor code is analyzed for propagation but its own findings
are suppressed by default. A dependency with the same class of bug prints
as a count instead of a finding:

```
$ steins check .
./src/main.php:10:22: error[type.argument-mismatch]: argument null (from $name, assigned at line 8) to Greeter::greet() cannot become string $name — proven TypeError (strict mode)
1 findings in vendor suppressed (--vendor-diagnostics to show)
```

Which trees are vendor is read from your `composer.json` —
`config.vendor-dir` and the `autoload` roots — so a project that installs
into `3rdparty/` is classified correctly, and a first-party `src/vendor/` is
not disowned. In a monorepo each subproject's own manifest governs its own
subtree. With no manifest, a `vendor` directory-name component is the
fallback. `steins doctor` prints what resolved.

## Reading the default surface

What a bare `check` prints is exactly the set held to the
**proven-runtime-break** bar (ADR-0002/0050): report only what breaks on a
live path — "the program works" outranks the worst-case static reading. A
value that merely *looks* risky but works at runtime is silent by
construction. This is the lenient-default principle: defaults are lenient,
strictness is opt-in and named — see
[profiles, baseline, and suppression](05-profiles-and-baseline.md) for the
`throws-direct` / `contracts` / `strict` / `pedantic` profiles, and
[the configuration chapter](03-configuration.md) for wiring one into
`steins.toml`. Debt reporting — true-but-not-breaking findings such as
undeclared `@throws` — is reached through profiles, deliberately, never
dumped on you by a first run.

> **If you know PHPStan or Psalm:** there is no numeric level here. A bare
> `check` is closer to PHPStan level 0 crossed with "coercive-mode runtime
> survivability" than to any single level number — the axis it moves along
> is *what kind of claim gets checked*, not *how aggressively*.

`--format json` emits the same findings structured, each carrying its
`layer` and `level`, with run-level suppression counts:

```
$ steins check . --format json
{
  "findings": [
    {
      "id": "type.argument-mismatch",
      "layer": "proof",
      "level": "fail",
      "path": "./src/main.php",
      "line": 10,
      "column": 22,
      "message": "argument null (from $name, assigned at line 8) to Greeter::greet() cannot become string $name — proven TypeError (strict mode)"
    }
  ],
  "profile": "default",
  "vendor_suppressed": 0,
  "suppressed": 0,
  "baselined": 0
}
```

## Exit codes

- `0` — nothing fail-level was displayed (a clean run, or a warn-only run).
- `1` — at least one fail-level finding was displayed.
- `2` — usage or config error (unknown flag, unknown profile, bad
  `steins.toml`, **a path argument that does not exist**). For example
  `--profile nope` prints

  ```
  $ steins check . --profile nope
  steins: unknown profile `nope` (built-ins: default, contracts, throws-direct, strict, pedantic; or define [profile.nope])
  ```

  and exits `2`.

A path you pass that names nothing is a usage error, not a clean run —

```
$ steins check src/Typo
steins: path does not exist: src/Typo
```

exits `2`, and under `--format json` emits no document at all — only the
error line above on stderr — so a renamed directory reds the build instead
of keeping it green (ADR-0050 §7). A path that *exists* and happens to
contain no `.php` files is a genuine no-op and still exits `0`.

## Known limitations (v0.1.4, honest)

- **No warm or incremental runs.** Every `check` is a cold batch analysis.
- **No LSP or editor server yet.** `annotate` gives a one-shot margin view
  of inferred types and effects; a resident `lsp` server is later work.
- **The dump surface is live.** `PHPStan\dumpType($e)` prints the inferred
  fact and **reds the build** (fail-level — remove it before committing, as
  with PHPStan); a `/** @psalm-trace $e */` docblock above a statement asks
  the same question committably — the trigger is a comment, so `debug.trace`
  reports at warn level and never moves the exit code, and the answer is the
  statement's *exit* fact (what `$e` is after that statement runs);
  `var_dump()` reports its arguments' inferred facts by default at warn
  level (exit-neutral; disable with a profile's
  `disable = ["debug.var-dump"]`). See the handbook's type-system chapter
  for a tour built on `dumpType()`.
- **Conformance posture, not a scoreboard.** Steins tracks the
  php-typing-conformance suite but does not claim a headline pass fraction
  in this doc — the default surface deliberately hides contract-layer
  expectations, so a bare `check` measures lower than a `--profile
  contracts` run over the same suite (ADR-0050 §6). The intentional
  divergences are each registered (ADR-0030):
  - Tool-specific phpdoc tags beyond `@phpstan-*`/`@psalm-*` (e.g.
    `@phan-param`) are erased — a standing refusal.
  - Declaration-coherence lints (native `?string` wider than
    `@param string`) are not reported — type-safe code, not a proof-layer
    concern; a standing refusal PHPStan itself shares by design.
  - `resource`-typed hints and resource-value tracking are unmodeled — an
    honest deferral, not a refusal.
  - Conditional late-static-binding return shapes (`new self()` under
    `: static` in an open class) stay silent — refused worst-casing.

## Where to go next

- **Every flag, every subcommand:** [the CLI reference](02-cli-reference.md)
  — `check`, `annotate`, `transform`, `effect-diff`, `doctor`, `mcp`,
  `version`, `license`, in full.
- **`steins.toml`:** [the configuration chapter](03-configuration.md) —
  discovery, key-by-key reference, and how config interacts with flags.
- **A project with real debt:** [profiles, baseline, and
  suppression](05-profiles-and-baseline.md) — the named strictness stages,
  the JSONL baseline ratchet, and how to onboard a large or noisy codebase
  without a first run that dumps hundreds of findings on you.
- **Something not working:** [troubleshooting](07-troubleshooting.md) —
  `steins doctor` in depth, sidecar failures, and symptom-indexed fixes.
