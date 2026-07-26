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

## [0.1.1] - 2026-07-26

The first release after `v0.1.0`, and the first one that can change what your CI reports. Three entries below move findings — folding now reaches array arguments, `class_alias` no longer silences absence claims project-wide, and array literals with a negative key are read by your PHP's version rules — so a green run may go red on claims that were always true and merely withheld. Steins is still a preview with plenty unimplemented, so the `0.1.x` series continues rather than jumping a minor; read the entries rather than the version number to know what changes.

Alongside those, the binary now carries its own legal notices and can say what version it is, `doctor` reports the code it declines to reason about instead of leaving a quiet run unexplained, and Steins is installable through Composer.

### Added

- **`steins version` and `steins license`** — the binary now tells you what it is and carries its own legal notices. `steins version` (also `-v`, `--version`) prints the version, the date and commit it was built from, the copyright, and where to read the licenses. `steins license` prints Steins' full Apache-2.0 terms followed by every bundled dependency's notice.
  - Both texts are compiled into the executable, which matters because nothing downstream keeps them beside it: `brew install` puts the binary on your `PATH` without `LICENSE`, and so does `cargo install --git`. Apache-2.0 §4(a) entitles you to a copy of the licence and the bundled MIT/BSD/ISC dependencies require their notices to accompany a binary — now the binary carries both itself, whatever your packager installed.
  - One licence reads as one entry, not one per copyright holder: the MIT permission notice is printed once with every dependency's copyright line above it — which is what "the above copyright notice and this permission notice shall be included in all copies" asks for — and bodies differing only in typography (centred versus flush-left) count as the same licence. That is 877 lines rather than the roughly 1,900 a per-crate listing would print, with no attribution lost.
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
- **`count`, `in_array` and `implode` now compute their value when you write the array out.** Steins evaluates a small set of pure builtins on your own PHP and carries the answer forward as a known value; until now it could only pass scalars to them, so these three — the ones whose whole job is to take an array — never actually ran. `count([1, 2, 3])` is `3`, `implode(",", ["a", "b"])` is `"a,b"`, `in_array(2, [1, 2, 3])` is `true`, and any check downstream of that value now has something to check. Nested literals count too: `count([[1, 2], [3]])` is `2`.
  - **This can surface findings that were not firing before**, in the ordinary way that knowing a value does: a folded result flows into argument, return and contract checks like any other proven value.
  - The array has to be written out in full. One element Steins cannot prove — a variable, a call, an offset read — and the whole array is unknown, because `count([1, $x])` is not `2` when `$x` might be an array. A literal of more than 256 entries or nested more than 8 deep is left alone as well.
  - The keys are your PHP's, not an imitation of them: the array is built by the same engine that evaluates the call, so a repeated key, an omitted key after an explicit one, and the negative-key rule PHP 8.3 changed all behave exactly as they do when you run the code.

### Changed

- **`THIRD-PARTY-LICENSES.md` now reads as one entry per licence, not one per copyright holder.** The file ships inside every release archive, and in 0.1.0 it repeated the MIT permission notice once for each of the 39 dependencies that ship it, because the copyright line above each one differs. Those sections are now grouped by the permission notice, with every crate's copyright notice listed above it — which is exactly what "the above copyright notice and this permission notice shall be included in all copies" describes. The file goes from 1,897 lines and 49 sections to 671 and 9, with **no holder dropped**: the regrouping is checked by a test comparing the before and after sets of copyright notices rather than trusting the eye. The same treatment applies to any licence family whose text carries a per-crate copyright line; MIT is simply the only one in this tree with more than one holder.

### Fixed

- **`class_alias(X::class, 'Name')` no longer silences the absence family across your whole project.** `X::class` is resolved by the PHP compiler — it is a plain string constant, it autoloads nothing, and `X` need not even exist — but Steins was reading it as a name minted at run time and raising the runtime-definition dam on it. That dam is a single project-wide switch, so **one** such call anywhere in the analyzed universe made `call.undefined-function`, `class.undefined`, and the guarded legs of `call.undefined-method` go quiet in every file. Vendoring one package that writes `class_alias(Thing::class, 'Legacy_Thing')` once per class was enough to do it; on one 85,000-file codebase this accounted for 32,749 of 32,914 dam sites. The call now contributes a class-alias edge to the index, exactly as the two-string-literal form already did, and the alias name resolves.
  - **This can surface findings that were being suppressed** — that is the point of the fix. If your project's dam is now clear where it was not, absence findings you have never seen may appear; they are claims that were always true and always withheld. Everything else about the dam is unchanged: a genuinely computed name (a variable, a concatenation, a function call, a constant) still dams, as do `self::class`, `static::class`, and `parent::class`.
  - The `X::class` spelling is resolved against the file's `use` imports (including grouped `use A\{B, C}`), its namespace, and the `namespace\X` relative form — not taken as written — so the alias points at the class PHP would point it at. A string-literal argument keeps its existing meaning as a runtime FQN spelled out in full.
- **A long report piped to `head` or `less` no longer crashes.** `steins check` on a large tree, `annotate` on a long file, `transform`'s diff and `doctor`'s report could all outrun a pipe buffer, and when the reader went away — which is exactly what `| head`, `| grep -m1` and quitting `less` early do — the command died with `failed printing to stdout: Broken pipe (os error 32)`. Reading a long report through a pager is ordinary use, not abuse. All user-facing output now goes through one writer that treats a closed reader as the reader's decision rather than an error.
  - The command's own verdict is untouched: `steins check | head` still exits 1 when the tree has findings and 0 when it does not, so a pipeline under `set -o pipefail` reports what the analysis found rather than what the pager did. What a closed pipe can no longer do is *invent* a failure — no panic, no exit 101, and no failure exit on a run that succeeded.
- **Array literals with a negative key are read by your PHP's rules, not one fixed version's.** PHP 8.3 changed where an omitted key lands after a negative one: `[-5 => 'a', 'b']` puts `'b'` at `0` before 8.3 and at `-4` from 8.3 on. Steins applied the pre-8.3 rule unconditionally, so on any supported PHP it could hold the wrong key for such a literal — and a key is what `===` and `==` compare, so a wrong key is a wrong verdict about whether two arrays are the same. The rule is now chosen from the PHP that runs your project, and both sides of the 8.3 boundary are served, because the supported floor is 8.1.
  - **This can change findings either way** on code with such a literal: a comparison Steins got wrong now decides correctly, in whichever direction correct happens to be for your PHP.
  - When Steins cannot see your PHP's version — no sidecar answered — it does not guess. A literal whose keys depend on the 8.3 change is treated as unknown and nothing is concluded from it, while every other literal (anything without a negative key) is unaffected. Values folded by your own PHP were always right and are untouched.

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

[Unreleased]: https://github.com/rigortype/steins/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/rigortype/steins/releases/tag/v0.1.1
[0.1.0]: https://github.com/rigortype/steins/releases/tag/v0.1.0
