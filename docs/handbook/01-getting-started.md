# Getting started

By the end of this chapter you will be able to:

- build `steins` from source and get it on your `PATH`;
- run `steins check` and read the line it prints;
- understand the PHP **sidecar** and the `--no-php` **sound
  subset** — and why one run can safely see more than the other;
- read exit codes in a CI script.

It is the only chapter you must read top to bottom. The rest of
the handbook is reference you can dip into later.

## Installing Steins

Steins is a tool, not a library — like a linter or a compiler,
it analyzes your project but is never loaded by its runtime.
Four channels install it; they differ only in *where the version
lives*. [The user manual's install
chapter](../user-manual/01-installation-and-quickstart.md) covers
the trade-off in full.

**Composer**, when the analyzer should be pinned beside the code
it analyzes — the version goes in `composer.lock`, so CI and
every developer resolve the same one. This is the right default
for a project:

```sh
composer require --dev typedduck/steins
```

What that installs is a small PHP shim, not the analyzer: on
first use it downloads the release binary matching the installed
version, verifies it against the published sha256, and runs it.
Requires PHP 8.1+.

**Homebrew**, when you want `steins` on `PATH` for any project
on the machine:

```sh
brew install rigortype/tap/steins
```

Or take a prebuilt binary from the [releases
page](https://github.com/rigortype/steins/releases) — `x86_64`/`aarch64`
Linux (glibc), `x86_64` Linux (musl, static), and
`x86_64`/`aarch64` macOS, each with a `.sha256` sidecar. Windows is
not shipped yet.

From source, with a Rust toolchain at 1.97 or newer:

```sh
cargo install --git https://github.com/rigortype/steins steins-cli
```

That puts a `steins` binary on your Cargo `PATH`. There is no
crates.io package — the parser backend is a rev-pinned fork, and
crates.io rejects crates with git dependencies. From a checkout,
`cargo install --path crates/steins-cli` works, or run
`target/release/steins` directly after a release build.

The binary has eight subcommands — `check`, `annotate`,
`transform`, `effect-diff`, `doctor`, `mcp`, `version`,
`license` — and no `--help`. Run it with no arguments to see the
surface:

```text
usage: steins check [--format text|json|github|sarif] [--profile <name>] [--no-php] [--vendor-diagnostics] [--fix] [--set-baseline] [--baseline <path>] [--ignore-baseline] <paths...>
       steins annotate [--no-php] [--format text|json] <file.php>
       steins transform <phpdoc-to-native|phpdoc-honesty|throws-envelope|effects-envelope|loop-to-array-map> [--apply] [--asserted-subjects] [--format text|json] <paths...>
       steins effect-diff [--baseline <path>] [--set-baseline] [--format text|json] <paths...>
       steins doctor [--no-php] [--baseline <path>] [--format text|json] [path]
       steins mcp
       steins version | -v | --version
       steins license
```

`doctor` answers "what is my setup?" rather than "what is wrong
with my code?" — it reports the resolved `php`, the active
profile's surface, and baseline health, and runs no checks.

## Your first run

Point `check` at a project, or any subtree, or a single file.
There is no config to write first — everything is inferred from
`composer.json` and the autoloader:

```sh
steins check .
```

A clean run prints nothing and exits `0`. When Steins can prove
a break, each finding is one line. Given this file:

```php
<?php
declare(strict_types=1);

function takesInt(int $x): int { return $x; }

takesInt("abc");
```

`steins check` prints:

```text
demo.php:6:1: error[type.argument-mismatch]: argument "abc" to takesInt() cannot become int $x — proven TypeError (strict mode)
```

## Reading a finding

Every line has the same shape:

```text
demo.php:6:1: error[type.argument-mismatch]: argument "abc" to takesInt() cannot become int $x — proven TypeError (strict mode)
```

| Slice | Meaning |
| --- | --- |
| `demo.php:6:1` | File, 1-indexed line, 1-indexed column |
| `error` | Severity |
| `type.argument-mismatch` | The **finding id** — what broke, not the rule that found it |
| `argument "abc" … $x` | The human-readable message |
| `proven TypeError (strict mode)` | The **consequence** — the exact runtime failure Steins proved |

The last clause is the point of the whole tool: `steins check`
does not say "this looks risky," it says "this program throws a
`TypeError` here, on a real path." Everything it cannot say that
firmly about, it does not print. Chapter 2 is about how it earns
that word *proven*; Chapter 8 (planned) is the id catalogue.

## The sidecar, and the sound subset

Steins types literal values by **executing your project's own
PHP** over a resident sidecar process — its version, its
extensions, its autoload. A folded value is then "what this code
produces on the runtime it actually runs on," not a guess from a
signature map. The sidecar is on by default and discovered as
`php` on your `PATH`.

Two whole families of finding depend on it. The **absence family**
— "this function does not exist," "this class is undefined" —
needs the runtime to answer *authoritatively* whether a name is
resident as a builtin or in a loaded extension. Without that, an
absence claim would be a guess, and Steins does not guess.

So when PHP is absent, or you pass `--no-php`, the run degrades
to a **sound subset** and says so on the first line:

```text
note: running as sound subset (no PHP sidecar) — findings that require executing PHP are omitted, and builtin return types come from the catalog's declarations, unverified
```

The zero-false-positive bar still holds — nothing false is ever
added — but findings that need the runtime **widen away into
silence**. A value mismatch that folds statically still fires:

```text
note: running as sound subset (no PHP sidecar) — findings that require executing PHP are omitted, and builtin return types come from the catalog's declarations, unverified
demo.php:6:1: error[type.argument-mismatch]: argument "abc" to takesInt() cannot become int $x — proven TypeError (strict mode)
```

while a call to an undefined function goes quiet, because only
the live PHP could have confirmed the name is truly absent. The
key property: **incompleteness is never silent about itself.**
The run tells you it saw less; it never pretends the quieter
surface is the whole story.

> **If you know PHPStan:** this replaces the version-emulation
> matrix. Instead of a bundled signature map for each PHP
> version, Steins asks the PHP your project actually runs. The
> trade is that offline runs (`--no-php`) can answer fewer
> questions — and they tell you which ones.

## Exit codes

For CI, only the exit code matters:

| Exit | Meaning |
| --- | --- |
| `0` | No fail-level finding was displayed — a clean run, or warn-only. |
| `1` | At least one fail-level finding was displayed. |
| `2` | Usage or config error — an unknown flag, an unknown profile, a bad `steins.toml`. |

For example, an unknown profile is a config error, not a finding:

```text
steins: unknown profile `nope` (built-ins: default, contracts, throws-direct, strict, pedantic; or define [profile.nope])
```

and exits `2`, so a misconfigured CI job fails loudly rather than
passing on an empty analysis.

## What's next

Chapter 2 is the core of the handbook: what Steins infers, why
it thinks in *values* before *types*, and exactly what a
`proven` finding rests on.
