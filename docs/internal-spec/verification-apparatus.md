# The Verification Apparatus

**Status: implemented** (`xtask`, `harness/phpdoc-oracle`; ADR-0013, ADR-0021,
ADR-0026, ADR-0029).

The zero-FP bar is a claim, and a claim without an instrument is a slogan. This
is the instrument.

## `cargo xtask` commands

| Command | Role |
| --- | --- |
| `fp-gate` | run the proof layer over the pinned corpus; **red on any finding** |
| `corpus-sync` | clone/refresh the pinned corpus (`--update` re-resolves to latest stable) |
| `phpdoc-oracle` | differential the PHPDoc parser against the real `phpstan/phpdoc-parser` |
| `gen-catalog` | regenerate the builtin class hierarchy from the mining TOML |
| `freq` | builtin frequency mining (catalog seeding input) |

## `fp-gate`

**One proof-layer diagnostic on working code is a release blocker** (ADR-0013),
so the gate exits nonzero the moment any proof-layer finding fires on a
clean-parsing corpus file. That is exactly the triage material worth surfacing —
never hidden behind a threshold.

**Whole-project mode.** Each corpus package is analyzed as *one* project — a
single salsa DB holding all its `.php` files — so cross-file calls, class
chains, and effects resolve. Packages run in parallel (rayon); within a package
the analysis is one project run.

**Parse errors.** Files that fail to parse are still *included in the project*,
so resolution stays complete — a partial tree can only silence, never add a false
positive. But any diagnostic landing *in* a parse-error file is excluded from the
gate count.

**Layer-driven partitioning.** The counter partition is decided in exactly one
place: `gate_bucket` routes each finding by its registry **layer** through a
`GateBucket` match that is *exhaustive on `Layer`* — proof and mechanics (and
any unregistered id, conservatively) are `RedOnSight`; contract is
`Measurement`; debug is `Excluded` from every counter (a dump is not a
finding). A new `Layer` variant is a compile error here until its gate posture
is stated.

**Measurement mode.** Contract-layer families (`phpdoc.*`, `throw.*`,
`effect.*`) are held separately: they are true findings that legitimately abound
in released code, so they gate as **per-package increase tripwires**, not
red-on-sight (ADR-0050 §9). The seeded expectations are hand-maintained tables
in `gate.rs`: `PHPDOC_EXPECTED` (488 findings across seven entries),
`THROW_EXPECTED` (44,184 — dominated by the legacy monorepo's 43,964, and
including the 20 `throw.undeclared` TRUEs seeded for phpstan-src at its
registration), and `EFFECT_EXPECTED`, seeded **empty**: an all-zero tripwire
that is vacuous until an envelope-annotated package lands, and correct the day
one does. Moving a count is a conscious, comment-triaged act, never a drive-by.

Triaged true positives in the proof layer are **fingerprint-pinned**
(`EXPECTED_PROOF_FINDINGS`), matched at finding precision — package + id +
path suffix + line + a message substring — so a known-good finding does not
re-block, and *any* drift does. Currently **13 pins**: the monolog
`stdClass`-into-`MongoDBHandler` TypeError the package's own test expects, ten
S2 `call.undefined-method` findings on the legacy monorepo, and two S5
`call.too-few-arguments` findings there (path suffixes deliberately shortened
past the private-corpus directory names). The discipline is staged opening:
a new family lands in measurement, its findings are triaged verbatim, and only
then are TRUEs pinned or counts seeded.

**Vendor.** Vendor findings are excluded from local projects' verdicts
(ADR-0015) and tallied separately.

## The corpus

`corpus.lock.toml` pins ten OSS packages by tag **and commit** — a shallow clone
at exactly that revision, so the gate is reproducible. Current entries include
`composer/composer`, `sebastianbergmann/phpunit`, `guzzle/guzzle`, and others
chosen for style diversity rather than size.

`corpus.local.toml` injects **live working trees** that are deliberately not
pinned and not committed: a private legacy monorepo, and — registered
2026-07-24 at the v0.1.0 run — `phpstan/phpstan-src` (curated, pathological,
modern PHP; `tests/` and `e2e/` excluded as deliberately-broken fixtures, so
`src/` is the clean FP-hunting surface). Its first run: 0 proof-layer, 0
`phpdoc.*`, 20 `throw.undeclared` — all triaged TRUE and seeded into
`THROW_EXPECTED`. Total scale at the last recorded run: ~99,280 files.

Held-out projects used for adoption drills are never used for tuning; that
separation is what makes an adoption-drill number mean anything. See
`docs/notes/20260724-adoption-drill-record.md`.

## `phpdoc-oracle`

The differential harness for grammar compatibility. The same inputs run through
the *real* `phpstan/phpdoc-parser` (in `harness/phpdoc-oracle`, a small PHP
project) and through `steins-phpdoc`, and the **canonical forms** are diffed.

This is why the grammar can be called normatively compatible rather than
"close": compatibility is measured, not asserted. See
[`phpdoc-grammar.md`](../type-specification/phpdoc-grammar.md).

## `gen-catalog`

Regenerates `steins-catalog::hierarchy_generated` from
`docs/research/phpsrc-mining/hierarchy.toml`. The TOML is the **source of
record**; the Rust file is `@generated` and carries the php-src commit pin and
the PHP version it was cross-checked against. Editing the Rust by hand is a
defect.

The mining directory also holds `throws.toml`, `failure_arms.toml`,
`effects_gaps.md`, and a `crosscheck.txt` — the per-arm C evidence behind the
catalog's claims.

## Conformance

Steins runs the external `php-typing-conformance` suite. Standing at the last
recorded triage: **85/98**, with every remaining non-#14939 failure registered
in the divergence registry as a standing refusal or an honest deferral, and zero
absent-machinery failures among them at that time.

The suite adapter (`SteinsChecker` plus a `--tool` filter) exists in the
maintainer's working tree and is not committed — roadmap gate G4. It affects
measurement convenience only.

## Test discipline

~1,200 `#[test]` functions across the workspace, weighted toward
`steins-infer/tests/` (35 integration files: arity, branch analysis, effects,
throws, offsets, object acceptance, truth tables, short-circuit, match/switch,
phpdoc contracts, …).

Two structural tests deserve naming because they enforce invariants rather than
behavior:

- **`tests/registry.rs`** — the diagnostic id totality reconciliation. See
  [diagnostic-shape.md](diagnostic-shape.md).
- **the domain's property tests** — `γ(a) ∪ γ(b) ⊆ γ(join(a, b))` over generated
  facts.

The standing rule recorded in the roadmap: **zero conformance regressions,
ever.**

## Not implemented

- **A performance harness.** No cold/warm baselines are measured under `xtask`;
  the ~200s full-batch figure is an observation, not a tracked metric
  (roadmap M5).
- **Mutation testing** of the checker itself.
- **CI wiring** for the gate beyond running it locally.
