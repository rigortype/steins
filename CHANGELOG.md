# Changelog

All notable changes to Steins are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

**This file is load-bearing, not decorative.** `.github/workflows/release.yml`
extracts a version's section verbatim as the body of its GitHub Release, so what
is written here is what users read. Two consequences:

- **Do not hard-wrap entries.** Each bullet and each summary paragraph is one
  physical line, however long. Wrapping renders badly on the Release page.
- **Write release notes, not commit messages.** The audience is someone deciding
  whether to upgrade, not someone reading the diff. Internal refactors, test
  additions, and doc churn do not belong here.

What counts as notable for an analyzer, concretely: a change to which findings are
reported or suppressed, to the surface of a profile, to the exit-code contract, to
the config schema, to the CLI, or to the baseline format. A new true positive is a
feature; a removed false positive is a fix; a finding that starts firing where it
did not before is a **breaking** change for anyone with a green CI, and says so.

Diagnostic **ids** are the contract (ADR-0023) — message wording is not. Reference
findings by id (`call.undefined-function`), never by the sentence they print.

## [Unreleased]

Entries accumulate under this heading as work lands; the `steins-release-prep`
skill seals them into a version section at release time, reconstructing from
`git log` if the discipline slipped.

### Added

- **`steins doctor` now says what a quiet run was silent about** — a Coverage posture section inventories the code the analyzer parses and then declines to reason about, so a clean run is a measured claim rather than an unexplained silence.
  - Poisoned scopes as a share of all scopes, with the constructs that caused them broken down by kind: `eval`, `include`/`require`, `extract`, `compact`, variable variables, reference assignment, `global`, `static`, and by-ref capture. Every local in such a scope is unknown by design — that is why Steins does not report false positives there, and now it says so.
  - Dam sites broken down by `eval` / unproven include / runtime-name `class_alias`: the sites where an absence claim about a function or class stays silent because runtime code could mint the name.
  - Reflection-driven invocation sites (`->invoke*()`, `->newInstance*()`, `Closure::bind` with a computed scope, `func_get_args()` under a typed signature), reported as an explicitly incomplete guess: these silence nothing on their own, and the list exists to be corrected against real code.
  - The section reports; it never fails. Nothing here is a diagnostic id, nothing enters a baseline, and `doctor` still exits 0 on every environment fact.
- **A Composer channel** — `composer require --dev typedduck/steins` pins the analyzer in `composer.lock` beside the code it analyzes, so CI and every developer resolve the same version.
  - What Composer installs is a PHP shim, not the analyzer: on first use it downloads the release binary matching the installed version, checks it against the sha256 published with that release, and runs it. Later runs use the cached binary and touch no network.
  - Requires PHP 8.1 or newer. A platform with no prebuilt binary — notably arm64 musl — is refused by name and pointed at a source build, rather than handed an archive that cannot run.
- **The effect vocabulary as its own package** — `typedduck/steins-attributes` supplies `#[\Steins\Pure]` and `#[\Steins\Effect]`, and the Composer package requires it, so one install leaves you able to declare an envelope.
  - MIT and inert at runtime, separate from the analyzer because it is vocabulary rather than tooling (ADR-0025). The Homebrew and release-binary channels are unchanged; the attributes were always yours to install there.

### Fixed

- **`class_alias(X::class, 'Name')` no longer silences the absence family across your whole project.** `X::class` is resolved by the PHP compiler — it is a plain string constant, it autoloads nothing, and `X` need not even exist — but Steins was reading it as a name minted at run time and raising the runtime-definition dam on it. That dam is a single project-wide switch, so **one** such call anywhere in the analyzed universe made `call.undefined-function`, `class.undefined`, and the guarded legs of `call.undefined-method` go quiet in every file. Vendoring one package that writes `class_alias(Thing::class, 'Legacy_Thing')` once per class was enough to do it; on one 85,000-file codebase this accounted for 32,749 of 32,914 dam sites. The call now contributes a class-alias edge to the index, exactly as the two-string-literal form already did, and the alias name resolves.
  - **This can surface findings that were being suppressed** — that is the point of the fix. If your project's dam is now clear where it was not, absence findings you have never seen may appear; they are claims that were always true and always withheld. Everything else about the dam is unchanged: a genuinely computed name (a variable, a concatenation, a function call, a constant) still dams, as do `self::class`, `static::class`, and `parent::class`.
  - The `X::class` spelling is resolved against the file's `use` imports (including grouped `use A\{B, C}`), its namespace, and the `namespace\X` relative form — not taken as written — so the alias points at the class PHP would point it at. A string-literal argument keeps its existing meaning as a runtime FQN spelled out in full.

## [0.1.0] - 2026-07-25

The first public release. Steins is a static analyzer for PHP built on one commitment: **a bare `steins check` reports only what provably breaks at runtime, and stays quiet about everything else.** That is enforced, not aspirational — the release gate runs the analyzer over roughly 100,000 files of real, clean PHP and fails if it emits a single proof-layer finding. This release ships that gate green.

The trade is deliberate. Steins finds less than a conventional analyzer, and what it does report you should not have to argue with. Where it cannot prove something, it widens and says nothing rather than guessing — and where its own coverage is degraded, it says so out loud instead of quietly reporting less.

### Added

- **`steins check`** — the analyzer. Default output is text; `--format json` emits the same findings machine-readably, with the accounting envelope (vendor-suppressed, suppressed, baselined counts).
- **A two-layer diagnostic model.** The *proof* layer carries the zero-false-positive guarantee and is on by default; the *contract* layer judges what your phpdoc claims against what the code proves (`phpdoc.param-mismatch`, `phpdoc.return-mismatch`, `phpdoc.property-mismatch`, `phpdoc.undefined-method`, the `throw.*` family) and is opted into by profile. Sixteen ids are on the default surface, among them `type.argument-mismatch`, `type.return-mismatch`, `type.property-mismatch`, `readonly.reassigned`, `call.on-null`, `call.too-few-arguments`, `call.unknown-named-argument`, `call.undefined-function`, `call.undefined-method`, `class.undefined`, `offset.missing`, and `offset.on-unsupported`.
- **Diagnostic ids are the contract, not message wording.** Every finding carries a stable `family.rule-name` id you can suppress, baseline, and script against; the sentence it prints may be reworded in any release.
- **Value-precise inference via a PHP sidecar.** Steins types literals by executing *your project's own PHP* over IPC — its version, its extensions, its autoload — so a folded value is what your code actually produces on the runtime it actually runs on, not what a model of PHP guesses.
- **Honest degradation when PHP is absent.** With no reachable `php`, or with `--no-php`, the run drops to a documented *sound subset*, prints that it has done so, and names the findings that go silent (the absence family: `call.undefined-function`, `class.undefined`, `call.undefined-method`). The zero-false-positive bar still holds — nothing false is added, some true things are omitted.
- **Profiles and a baseline ratchet.** Three built-in profiles select which layers and ids are surfaced — `default` (proof + mechanics), `contracts` (adds the contract layer), and `throws-direct` — and you can define your own in `steins.toml`, optionally extending a built-in. `check --set-baseline` captures the current findings so an existing codebase can adopt Steins at zero and ratchet down; the baseline records the profile and id set it was captured under, and says so loudly when the active surface has since grown past it.
- **Three suppression channels and no fourth**: the baseline, inline `@steins-ignore`, and config policy. Vendor code is suppressed by default (`--vendor-diagnostics` to see it).
- **`steins doctor`** — a posture report: which `php` resolved and what it reports, the active profile's surface, written-but-unchecked throw envelopes, baseline health, catalog freshness. It runs no checks and never fails on a merely degraded environment, so it is safe as an install smoke test.
- **`steins annotate`** — reprints a file with a right-margin column of the facts Steins actually proved, for seeing the inference rather than only its complaints.
- **`steins transform`** — two verified codemods: `phpdoc-to-native` promotes phpdoc types to native declarations, and `phpdoc-honesty` corrects phpdoc that the code contradicts. Both are gated on preconditions proven across every call site, and `--apply` is opt-in; without it you get a diff.
- **`steins.toml`** — optional configuration for profiles, path sets, policy, and runtime pseudo-constants. There is no `init`: a zero-config run infers everything from `composer.json` and the autoloader.
- **Prebuilt binaries for five targets** — `x86_64`/`aarch64` Linux (glibc), `x86_64` Linux (musl, static), and `x86_64`/`aarch64` macOS — each with a `.sha256` sidecar, and each archive carrying `LICENSE` and `THIRD-PARTY-LICENSES.md` beside the binary. Also installable with `brew install rigortype/tap/steins`.

### Notes

- **Licensed under Apache-2.0.** You may embed Steins in a proprietary tool, run it as a hosted service, and redistribute it, with no source-disclosure obligation, plus an express patent grant. The separate `steins-attributes` vocabulary package is MIT.
- **Requirements**: PHP 8.x for the sidecar (discovered as `php` on `PATH`); building from source needs Rust 1.97 or newer.
- **There is no crates.io package**, and this is structural rather than an oversight: the parser backend is a rev-pinned fork and crates.io rejects crates with git dependencies. Install from a release archive, from Homebrew, or with `cargo install --git https://github.com/rigortype/steins steins-cli`.
- **Windows is not shipped.** The sidecar spawns PHP through a temp-dir path that is unverified there, and a binary that mis-spawned it would degrade silently to the sound subset — worse than not shipping.
- **What Steins does not do yet** is written down rather than left to discovery: see [`docs/type-specification/not-implemented.md`](docs/type-specification/not-implemented.md). It is also not a linter or a formatter, and will not become one.

[Unreleased]: https://github.com/rigortype/steins/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/rigortype/steins/releases/tag/v0.1.0
