# Quickstart

Steins is a value-precise static analyzer for PHP. A bare `steins check`
reports only what provably breaks at runtime, and stays quiet about
everything else. This page gets you from an install to reading that first
run.

## Install

Four channels. They differ in *where the version lives*, which is the only
question worth thinking about:

**Composer** — when the analyzer should be pinned beside the code it analyzes.
The version goes in `composer.lock`, so CI and every developer resolve the same
one and `composer update` moves it deliberately. This is the right default for a
project.

```
composer require --dev typedduck/steins
```

What Composer installs is a small PHP shim, not the analyzer: on first use it
downloads the release binary matching the installed version, checks it against
the sha256 the release published, and runs it. Later runs use the cached binary
and touch the network not at all. Requires PHP 8.1+.

You end up with two packages. `typedduck/steins` is the shim, **Apache-2.0**,
and it requires `typedduck/steins-attributes` — the `#[\Steins\Pure]` and
`#[\Steins\Effect]` classes you write in your own source, **MIT** and inert at
runtime. They are separate packages because they are different kinds of thing:
one is tooling you run, the other is vocabulary meant to spread (ADR-0025). The
requirement is there so one command leaves you able to write an envelope.

**Homebrew** — when you want `steins` on `PATH` for any project on the machine.

```
brew install rigortype/tap/steins
```

**A prebuilt binary**, from the
[releases page](https://github.com/rigortype/steins/releases) — `x86_64`/`aarch64`
Linux (glibc), `x86_64` Linux (musl, static), and `x86_64`/`aarch64` macOS. Each
archive holds the bare `steins` binary and carries a `.sha256` sidecar beside it.
Windows is not shipped yet.

**From source**, with a Rust toolchain at 1.97 or newer. This is the fallback
for a platform with no prebuilt binary — notably arm64 Alpine, where the musl
build is x86_64-only.

```
cargo install --git https://github.com/rigortype/steins steins-cli
```

There is no crates.io package: the parser backend is a rev-pinned fork, and
crates.io does not accept crates with git dependencies. From a checkout of this
workspace, `cargo install --path crates/steins-cli` works too.

The binary has six subcommands and no `--help`; run it with no arguments
to see the surface:

```
usage: steins check [--format text|json] [--profile <name>] [--no-php] [--vendor-diagnostics] [--set-baseline] [--baseline <path>] [--ignore-baseline] <paths...>
       steins annotate [--no-php] <file.php>
       steins transform <phpdoc-to-native|phpdoc-honesty> [--apply] [--format text|json] <paths...>
       steins doctor [--no-php] [--baseline <path>] [path]
       steins version | -v | --version
       steins license
```

`doctor` reports posture rather than findings — which `php` was resolved, what the
active profile checks, which trees count as vendor, baseline health — and runs no
checks at all. It is also the quickest way to confirm an install works:

```
steins doctor --no-php
```

## Requirements

Steins types literals by executing the **project's own PHP** over IPC — its
version, its extensions, its `composer` autoload — so that a folded value is
"what this code produces on the runtime it actually runs on" (ADR-0004).
The sidecar is default-on and lazily spawned; discovery is `php` on `PATH`.

If PHP is absent, or you pass `--no-php`, the run degrades to a **sound
subset** and says so on the first line:

```
note: running as sound subset (no PHP sidecar) — findings that require executing PHP are omitted
```

The zero-FP bar still holds in the sound subset — nothing false is added —
but findings that need the runtime *widen away*. The **absence family** goes
quiet here: `call.undefined-function` and `class.undefined` need the sidecar
to answer "not defined on this PHP" for every candidate name, and
`call.undefined-method` needs it to rule out a builtin/extension homonym
(ADR-0049 §1, A2). Value-precise mismatches that fold statically still fire.
Incompleteness is never silent — the coverage posture is surfaced, not
assumed.

## First run

Point `check` at a project (or any subtree); zero config, everything is
inferred from `composer.json` and autoload:

```
steins check .
```

Nothing to report exits `0` with no output. On a real tree you see the
proof surface. Here is a trimmed run over Nextcloud's server tree — two
deliberate test-fixture breaks, and a count of vendor findings held back:

```
…/nextcloud-server/tests/lib/BackgroundJob/JobTest.php:51:4: error[call.on-null]: method call $test->someMethod() — $test is proven null on this path — proven Error (Call to a member function on null)
…/nextcloud-server/tests/lib/Files/ViewTest.php:1314:12: error[type.argument-mismatch]: argument null to View::__construct() cannot become string $root — proven TypeError (coercive mode)
492 findings in vendor suppressed (--vendor-diagnostics to show)
```

Each line is `path:line:col: error[id]: message — proven <consequence>`.
The `id` (`call.on-null`, `type.argument-mismatch`) names the *finding*,
not the rule that found it. Vendor code is analyzed for propagation but its
own findings are suppressed by default; `--vendor-diagnostics` shows them.

Which trees are vendor is read from your `composer.json` — `config.vendor-dir`
and the `autoload` roots — so a project that installs into `3rdparty/` is
classified correctly, and a first-party `src/vendor/` is not disowned. In a
monorepo each subproject's own manifest governs its own subtree. With no
manifest, a `vendor` directory component is the fallback. `steins doctor` prints
what resolved.

## Reading the default surface

What a bare `check` prints is exactly the set held to the
**proven-runtime-break** bar (ADR-0002/0050): report only what breaks on a
live path — "the program works" outranks the worst-case static reading. So
`View::__construct(null)` where `null` cannot become `string` is a finding
(a proven `TypeError`), while a value that merely *looks* risky but works at
runtime is silent by construction. This is the lenient-default principle:
defaults are lenient, strictness is opt-in and named (see
[profiles-and-baseline](profiles-and-baseline.md)). Debt reporting —
true-but-not-breaking findings such as undeclared `@throws` — is reached
through profiles, deliberately, never dumped on you by a first run.

`--format json` emits the same findings structured, each carrying its
`layer` and `level`, with run-level suppression counts:

```json
{
  "findings": [
    {
      "id": "type.argument-mismatch",
      "layer": "proof",
      "level": "fail",
      "path": "…/dump.php",
      "line": 10,
      "column": 10,
      "message": "argument \"abc\" to takesInt() cannot become int $x — proven TypeError (coercive mode)"
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
  `steins.toml`). For example `--profile nope` prints
  `steins: unknown profile 'nope' (built-ins: default, contracts, throws-direct; …)`
  and exits `2`.

## Known limitations (v0.1.0, honest)

- **No warm or incremental runs.** Every `check` is a cold batch analysis.
- **No LSP or editor server yet.** `annotate` gives a one-shot margin view
  of inferred types and effects; a resident `lsp` server is later work.
- **The dump surface is live.** `PHPStan\dumpType($e)` prints the inferred
  fact and **reds the build** (fail-level — remove it before committing, as
  with PHPStan); `var_dump()` reports its arguments' inferred facts by
  default at warn level (exit-neutral; disable with a profile's
  `disable = ["debug.var-dump"]`). See the handbook's type-system chapter
  for a tour built on `dumpType()`.
- **Conformance posture, not a scoreboard.** Steins tracks the
  php-typing-conformance suite but does not claim a headline pass fraction
  in this doc — the default surface deliberately hides contract-layer
  expectations, so a bare `check` measures lower than a `--profile
  contracts` run over the same suite (ADR-0050 §6). The intentional
  divergences are each registered, one line each (ADR-0030):
  - Tool-specific phpdoc tags beyond `@phpstan-*`/`@psalm-*` (e.g.
    `@phan-param`) are erased — a standing refusal.
  - Declaration-coherence lints (native `?string` wider than
    `@param string`) are not reported — type-safe code, not a proof-layer
    concern; a standing refusal PHPStan itself shares by design.
  - `resource`-typed hints and resource-value tracking are unmodeled — an
    honest deferral, not a refusal.
  - Conditional late-static-binding return shapes (`new self()` under
    `: static` in an open class) stay silent — refused worst-casing.
