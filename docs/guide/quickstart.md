# Quickstart

Steins is a value-precise static analyzer for PHP. A bare `steins check`
reports only what provably breaks at runtime, and stays quiet about
everything else. This page gets you from an install to reading that first
run.

## Install

From a checkout of this workspace:

```
cargo install --path .
```

Or run the workspace binary directly out of `target/release/steins` after a
release build. Prebuilt binaries ship with the release.

The binary has three subcommands and no `--help`; run it with no arguments
to see the surface:

```
usage: steins check [--format text|json] [--profile <name>] [--no-php] [--vendor-diagnostics] [--set-baseline] [--baseline <path>] [--ignore-baseline] <paths...>
       steins annotate [--no-php] <file.php>
       steins transform <phpdoc-to-native|phpdoc-honesty> [--apply] [--format text|json] <paths...>
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
