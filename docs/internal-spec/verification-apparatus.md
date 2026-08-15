# The Verification Apparatus

**Status: implemented** (`xtask`, `harness/phpdoc-oracle`, `spike/lean-domain`;
ADR-0013, ADR-0021, ADR-0026, ADR-0029, ADR-0059).

The zero-FP bar is a claim, and a claim without an instrument is a slogan. This
is the instrument.

## `cargo xtask` commands

| Command | Role |
| --- | --- |
| `fp-gate` | run the proof layer over the pinned corpus; **red on any finding** |
| `corpus-sync` | clone/refresh the pinned corpus (`--update` re-resolves to latest stable) |
| `phpdoc-oracle` | differential the PHPDoc parser against the real `phpstan/phpdoc-parser` |
| `lean-check` | build the Lean 4 spec of the value domain and verify the committed differential vectors are still what it prints (`--bless` to rewrite) |
| `gen-catalog` | regenerate the builtin class hierarchy **and the return-fact table** from the mining TOML |
| `freq` | builtin frequency mining (catalog seeding input) |
| `nsrt` | the `assertType` harness (oracle idea B): five-verdict measurement of dump renderings against PHPStan's own `nsrt/` fixtures, `assertType` recognized **harness-only** |

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
in `gate.rs`: `PHPDOC_EXPECTED` (526 findings across seven entries — the legacy
monorepo alone at 477 after the ADR-0056 R1 return-fact reseed, +43 from
uniquely-resolved builtin calls now seeding their reflected return envelope),
`THROW_EXPECTED` (44,592 — dominated by the legacy monorepo's 44,372, and
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

`corpus.local.toml` injects **live working trees** that this repo deliberately
neither checks out nor commits: a private legacy monorepo, and — registered
2026-07-24 at the v0.1.0 run — `phpstan/phpstan-src` (curated, pathological,
modern PHP; `tests/` and `e2e/` excluded as deliberately-broken fixtures, so
`src/` is the clean FP-hunting surface). Its first run: 0 proof-layer, 0
`phpdoc.*`, 20 `throw.undeclared` — all triaged TRUE and seeded into
`THROW_EXPECTED`. Total scale at the last recorded run: ~99,490 files (the
unpinned monorepo drifted +210 during the day and its tripwires were reseeded).

That asymmetry has a cost the public half does not pay. A raised count on a
pinned package can only be the analyzer, because the corpus is fixed by the
lock file; a raised count on a live tree is ambiguous between a regression and
corpus drift, and settling it after the fact means archaeology in a repository
this one cannot see. So a `[[project]]` entry may carry an optional `revision`
— the checkout state its seeded baselines were measured at. It does not *pin*
anything (nothing here checks out a private tree); it records, so the gate can
compare. On every run the gate reads what the tree is actually on (`git -C
<path> rev-parse HEAD`, degrading to "unknown" on any failure) and prints it;
when a tripwire trips it says which of three situations obtains — the revision
matches, so the increase is a genuine regression; the revision differs, so the
change may be drift and wants a re-measure before a reseed; or nothing was
recorded, so the question cannot be settled automatically and here is the value
to record. The field lives in the gitignored file and the sha is printed only
to the operator's terminal: a private repository's commit id never enters this
one. Absent is legal, and an entry without it behaves exactly as before.

A matching revision is not by itself a statement about the *files*, and these
trees are somebody's working checkout, where dirty is the normal state rather
than an edge case. So the gate also asks `git -C <path> status --porcelain`
(same degradation: unknown, never assumed clean) and only a **clean** match
issues the confident "this is a genuine regression, stop looking at the corpus"
verdict. A dirty match says the recorded commit agrees while uncommitted or
untracked content sits on top, so the measured files are not exactly that
revision; an undeterminable one says so rather than implying clean. Untracked
content counts as dirty without exception — the gate walks the filesystem, not
the index, so an untracked `.php` file is measured like any other.

What this cannot do is keep the two halves in sync. The counts live in tracked
Rust; the revision lives in an untracked file no check in this repository can
read. A reseed that updates the count and forgets the revision leaves a record
that is not merely stale but actively wrong — it will assert the confident
verdict against a baseline seeded somewhere else. That is why the revision
prints on every run including green ones: putting both halves in front of the
operator at every reseed opportunity is the only available mitigation, and it
is a discipline rather than a guarantee.

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

## `lean-check`

The differential harness for the value domain's *algebra* (ADR-0059).
`spike/lean-domain` is a Lean 4 specification of `steins-domain` that proves what
the crate's doc comments claim — `γ(a) ∪ γ(b) ⊆ γ(join(a, b))` for every value,
not for generated samples — and then prints a deterministic vector file.

Three legs, only the first two of which need Lean:

1. `lake build` — the proofs compile. A spec that does not build proves nothing.
2. `lake exe vectors` — the spec prints 4,154 lines of `admits` / `truthy` /
   `isnull` / `satisfiesstr` / `intin` / `join` over a fixed universe, plus the
   atom tables (where the PHP-classifier assumptions the proofs rest on are
   checked against `StrPreds::of`) and an exhaustive associativity tally.
   `lean-check` verifies `crates/steins-domain/tests/fixtures/lean-vectors.expected`
   is byte-identical to that output; `--bless` rewrites it.
3. `cargo test -p steins-domain --test lean_vectors` — the Rust implementation
   walks the same universe in the same order and diffs the rendered results.

Leg 3 is an ordinary test, so a machine without a Lean toolchain still gets the
full Rust-side check; that is why `lean-check` **skips rather than fails** when no
toolchain is found.

In CI, legs 1–2 run in `.github/workflows/lean.yml` — a separate, path-filtered
workflow (only `spike/lean-domain/**` and `crates/steins-domain/**` trigger it)
using `leanprover/lean-action`; leg 3 runs in the ordinary `test` job on every PR.
Not part of `fp-gate` or the release gates: those are about the analyzer's output,
this is a drift guard on a committed generated artifact, exactly like the
`licenses` job's `THIRD-PARTY-LICENSES.md` check.

`spike/lean-domain/SteinsDomain/Axioms.lean` makes the "no `sorry`, no
`native_decide`" claim a build step: each headline theorem's axiom set is pinned
with `#guard_msgs`, so weakening a proof fails `lake build`.

What is *not* proved, and is checked exhaustively instead: `join` associativity
(110,592 triples, zero mismatches). It matters because `join_envs` folds
multi-branch joins left-to-right. See `spike/lean-domain/REPORT.md`.

## `nsrt`

PHPStan's `assertType` corpus read as an oracle for inference. Each observation is
given one of four verdicts, and the split between the last two is the point of
issue #47.

| verdict | meaning |
| --- | --- |
| `match` | semantically equal to the assertion after normalization |
| `unsupported` | the assertion uses vocabulary Steins deliberately does not model |
| `subsumed` | Steins is strictly **more precise**: what it renders is a proper subtype of what PHPStan asserts |
| `differ` | a genuine divergence, including `unknown` where a concrete type was asserted |

### Invocation

```
cargo xtask nsrt [DIR]
```

`DIR` defaults to a sibling checkout, `../../php/phpstan-src/tests/PHPStan/Analyser/nsrt`
resolved from the workspace root — pass it explicitly whenever `cargo xtask` runs
from anywhere else (a worktree, most obviously: its workspace root is not the
repo root the default assumes, so the default path resolves to nothing there).
**Run it on PHP 8.5.** Since the version gate below, the fixture set the harness
measures depends on the sidecar's PHP minor, so the headline is only comparable
between runs on the same one. 8.5 is the choice because the exclusion is monotonic
— every newer minor skips a strict subset of what an older one skips, so 8.5
measures the most fixtures anyone can (60 files skipped, against 71 at 8.4, 84 at
8.3, 92 at 8.2). Record the minor next to any number quoted from a run; the harness
prints it above the summary for that purpose.

There is deliberately **no `--php-version` override**. Faking the gate would not
move the sidecar: the folds still run on whatever `php` is on PATH, so an override
would score one minor's fixture set with another minor's answers — a number that
looks like a measurement and is not one. To measure another minor, install it.

`DIR` need not be that exact subdirectory; the walk is recursive over whatever
path it is given, so pointing it at the phpstan-src checkout root instead of the
`nsrt/` fixture directory also works — it just measures a much larger, mostly
irrelevant file set (phpstan-src's own `src/`, `vendor/`, `tmp/`, benches, …)
alongside the fixtures that are the actual oracle.

A plain `cargo xtask nsrt` (debug build) used to stack-overflow on that larger
walk (issue #246): phpstan-src ships a benchmark fixture,
`tests/bench/data/nullsafe-chain-walk.php`, built out of `Node` property-fetch
chains up to 1,000 `->next` accesses deep (a deliberate stress test, per that
file's own doc comment) — deeply nested but finite, not a cycle. Walking it
recurses steins-syntax's `scan_effect_origins` roughly 2,500 frames down, which
overflows a debug build's large, uninlined frames on the ~8 MiB default OS
stack while fitting easily in release's optimized ones — release was the only
workaround. `nsrt`'s entry point now runs the analysis on a worker thread with
an explicitly sized stack (256 MiB; see `WORKER_STACK_SIZE` in `xtask/src/nsrt.rs`)
specifically so a measurer does not need `--release` to get past that fixture —
a plain debug-build `cargo xtask nsrt` now completes over the whole phpstan-src
tree, not just the `nsrt/` subdirectory. The fixture is fixed depth, not a
growing one, so this is a one-time harness fix, not a budget that needs
revisiting as the corpus moves.

`nsrt` was not the only walker of that fixture, though, and the sizing is no
longer harness-only: `steins check` aborted on it too, `fp-gate` and `freq`
parse on rayon workers whose default stack is a quarter of the one that
overflowed, and the wasm playground cannot buy headroom at any price. The
per-entry-point measurements, and the ceilings each surface actually has, are in
[the deep-nesting note](../notes/20260808-deep-nesting-stack-budget.md); the
binary's own worker is `WORKER_STACK_SIZE` in `crates/steins-cli/src/main.rs`
and the pool's is `RAYON_STACK_SIZE` in `xtask/src/main.rs`.

The playground got the other answer, because it had no choice (issue #264):
`crates/steins-syntax/src/stack_guard.rs` measures how much stack the lowering
walk has actually consumed and refuses past a budget, which on wasm defaults to
half the module's 1 MiB shadow stack and everywhere else is off unless an
embedder sets it. The refusal is a recovered parse error, so it reaches the
reader as the `syntax.unparsable` a broken file already earns rather than as a
trap. Two conventions follow, and both are load-bearing for anyone adding
coverage here:

- **A test that parses a deep fixture in process must set a budget first.**
  libtest runs tests on 2 MiB threads — a quarter of the stack issue #246 found
  fatal at ~520 levels — so an unguarded in-process deep parse aborts the whole
  test binary, and a stack overflow is not a catchable panic.
  `crates/steins-syntax/tests/deep_nesting.rs` sets one;
  `crates/steins-cli/tests/deep_nesting.rs` takes the other route and drives the
  real binary as a subprocess.
- **The wasm module has a gate now.** `apps/playground/smoke.mjs` called itself
  "the CI gate before any artifact upload" while no workflow invoked it; the
  `wasm` job in `ci.yml` builds the module and runs it, with the deep-chain case
  among its assertions.

Before the fourth verdict existed, `subsumed` rows scored as `differ`. That made the
instrument argue against the analyzer: PHPStan asserts `bool` for
`in_array('foo', ['foo', 'bar'])` because it declines to fold a loose comparison,
Steins proves `true`, and the slice that shipped the improvement booked a
regression. Since nsrt's counts are what every inference slice is ranked by
(ADR-0056 ranked its whole programme off them, and ADR-0061's type rung is measured
the same way), a metric that falls as folding widens is worse than a wrong number —
it is a number that argues against the work.

The subtype test **reuses the acceptance relation the checker enforces**:
`steins_contract::lower_str` on both strings, then `normalize::subsumes` — the same
relation behind param contravariance / return covariance and ADR-0056's envelope
subset check. There is deliberately no harness-local definition of "narrower than";
one would drift from what the analyzer actually does, and the harness would end up
measuring something else. Anything the relation cannot decide (`Maybe`) stays
`differ`, which is the FP-safe direction for a metric.

Two asymmetries are load-bearing and pinned by unit tests:

- **Steins wider than the assertion stays a failure.** `bool` where PHPStan asserts
  `true` is a real gap. Were that laundered into `subsumed`, every widening
  regression would report as precision.
- **PHP's int→float coercion is not membership.** `admits_val(float, Int)` is `Yes`
  because PHP coerces at a declared `float` slot; PHPStan's own hierarchy answers
  `No`. So an `int` under an asserted `float` is a contradiction, not an open
  question — `bug-12393.php:40` is Steins missing a typed-property coercion — and the
  harness declines to ask the relation across that boundary.

  Issue #356 extended that veto to **nested** positions. The original guard scanned
  top-level `|`-split atoms, so a crossing buried inside an array read as one opaque
  `array-shape` atom and reached the relation anyway: `array{2.0, 3.0, 4.0, 5.0}`
  against `list{2, 3, 4, 5}` scored `subsumed`, because `admits_val` answers `Yes` for
  a `LitFloat` against an int value by the same PHP value-equality rule. The veto is
  now judged on the lowered types with *aligned* positions, so a genuine int arm
  elsewhere in a shape cannot excuse a crossing at the position that has one, and an
  undecidable alignment simply yields no pair.

  Alignment gathers `expected`'s candidate contracts at a position across **union
  arms**, which is load-bearing rather than incidental: `?list<float>` is a union
  whose `null` arm answers nothing at the element position, so an arm-blind lookup
  finds the whole expectation unalignable and never vetoes — the exact shape of the
  original bug, one level up. Judging the arms *together* is also what keeps
  `list<float>|list<int>` a membership question: an int arm among the candidates is
  not a coercion. Arm-at-a-time would get both wrong, in opposite directions. The
  relation itself is unchanged:
  `subsumes` answering `Yes` there is correct for what it models (acceptance), and the
  harness's job is to not read acceptance as precision.

### Version-gated fixtures are not measured (issue #356)

448 of the 1,617 nsrt fixtures open with a `// lint <op> <version>` marker written
*on the `<?php` line itself*, not as a standalone comment. It names the PHP range
under which PHPStan's assertions in that file hold. Steins folds through a sidecar
running whatever `php` resolves off `PATH`, so outside that range those assertions
are not an oracle at all — and the harness used to score them anyway.

`range-function-php82.php:5` is the case that surfaced it: `range(2, 5, 1.0)` is
asserted `array{2.0, 3.0, 4.0, 5.0}` behind `// lint < 8.3`, PHP 8.3 changed the
function to return ints, and on an 8.5 sidecar the fold answers `list{2, 3, 4, 5}` —
correct for the engine that ran. Scored against the 8.2 assertion it is a
disagreement about *which PHP is running*, and the nested-crossing hole above then
booked it as precision.

The harness now asks the interpreter for `PHP_MAJOR_VERSION.PHP_MINOR_VERSION` — the
same bare `php` that `steins-sidecar` spawns — and **skips an excluded fixture before
analysis**, counting it on its own report line rather than folding it into any
verdict. At PHP 8.5 that removes 59 files / 619 observations, among them 81 `match`
and 20 `equal`/`subsumed`: the headline had been carrying 81 rows of luck, where an
assertion in a gated file happened not to be version-sensitive. Agreement with a
statement that is not being claimed for your engine is not confirmation.

The gate is per *file* while only some assertions in it are version-sensitive, which
makes whole-file exclusion look blunt. **Owner ruling (2026-08-15, #356): file-level
exclusion stands — do not re-argue per slice.** It is the honest denominator: the
marker is the only statement anyone makes about which rows are sensitive, so a
finer-grained rule would be the harness inventing an oracle for itself. The 81 `match`
rows this costs were never confirmations, and buying them back with a heuristic would
trade a known-honest number for a guessed one.

Because the exclusion moves the headline, **counts are only comparable between runs
on the same PHP minor.** The sidecar version is printed above the summary for that
reason, alongside the fold-surface posture.

### Does `subsumed` count toward the headline? No. (Settled; do not re-argue.)

The headline stays `match`: oracle-**confirmed** agreement. A `subsumed` row is only
*unfalsified* — the corpus says `bool` is admissible, it never says `true` is right,
and a fold bug producing the wrong literal under a correct base type would land here
too. Merging the two would make the headline unfalsifiable and `match` would stop
meaning "we reproduce PHPStan". The 24 rows at the baseline below make the case
concretely: several are Steins narrowing for reasons that are *not* extra
precision — `array-find-key.php:59` and `bug-9293.php:27` render bare `null` where a
real value is reachable, `bug-10122.php:17` misses the string-increment fallout, and
the `bug-2600*.php` rows narrow on an `(asserted)`-stratum phpdoc fact that ignores a
`= null` default. All four are strictly narrower and all four are gaps.

What actually fixes the defect is that these rows **leave `differ`**, not that they
join `match`. A slice that converts ten divergences into subsumptions now reads as
differ falling and subsumed rising, never as a regression. The report prints
`match + equal + subsumed` as an explicit secondary *admissible* figure so that
movement is visible without unverified claims entering the headline. (`equal` —
issue #172, ADR-0062 §6 as amended — is the proven-equal-but-differently-spelled
verdict: the relation answers `Yes` in both directions while the normalized
spellings differ. It is awarded by the relation's own proof, never by a
normalization rule, and it too stays out of the headline: the headline counts
string-level reproduction of the oracle.)

Recorded baseline, post-#47 (superseded below by the S1.5 reseed):

| | count |
| --- | --- |
| match (**headline**) | 734 |
| unsupported | 6,987 |
| subsumed | 24 |
| differ | 7,768 |
| measured | 15,513 |

That baseline is superseded, not wrong: it pre-dates S1 (the speller learning the
full array vocabulary) and S1.5 (2026-07-29, ADR-0062) removing the array-vocabulary
atoms — `{`, `array<…>`/`list<…>`, bare `array`/`list`, and their `non-empty-` forms
— from `unsupported_pattern`. Before S1.5 those atoms were gated *before `got` was
ever read*, so 4,398 array-typed expectations that the speller could already answer
sat in `unsupported` unmeasured. Reseeded baseline (same phpstan-src checkout,
classify-logic change only — no analyzer code moved):

| | count |
| --- | --- |
| match (**headline**) | 845 |
| unsupported | 2,586 |
| subsumed | 25 |
| differ | 12,054 |
| measured | 15,510 |

Of the 4,398 records that left `unsupported`: 111 became `match`, 1 became
`subsumed` (`bug-10834.php:20` — Steins renders bare `null` where PHPStan asserts a
64-field shape union with `null`, the same narrowing pattern as the pre-existing
`array-find-key.php:59`/`bug-9293.php:27` rows), and the remaining 4,286 became
`differ` — the array-vocabulary gap inventory this slice exists to expose,
including the deliberately-*not*-normalized-away D4-native divergence where Steins
spells an empty or sequential array value as `list{…}` and PHPStan stable asserts
`array{…}` (e.g. `array-is-list-unset.php:9`).

Future slices are measured against the S1.5 numbers above, not against the pre-#47
pair (734 / 7,792, in which the 24 subsumptions were counted as losses) or the
post-#47/pre-S1.5 pair (734 / 6,987/24/7,768) superseded here.

### The casing-keyword reseed (2026-07-31, issue #77)

The same defect S1.5 fixed for the array atoms was still open for two scalar ones.
`steins_contract::spell::preds_keyword` has spelled `lowercase-string` and
`uppercase-string` since the casing predicates landed, but neither appeared in
`is_supported_atom`'s keyword list, so every expectation naming one was gated
**before `got` was ever read** — 88 records sitting in `unsupported`, including two
(`more-types.php:50`/`:52`) where Steins already rendered the asserted keyword
exactly. Adding the pair is a classify-logic change only; no analyzer code moves
with it.

The pair is added, their `non-empty-` intersections are not: PHPStan spells that
set `lowercase-string&non-empty-string`, which is an intersection and stays
unsupported on its own terms. A Steins answer of `non-empty-lowercase-string`
against a bare `lowercase-string` assertion therefore scores `subsumed`, not
`match` — the same discipline as everywhere else.

Reseeded baseline (at the issue-#77 slice):

| | count |
| --- | --- |
| match (**headline**) | 961 |
| unsupported | 2,488 |
| subsumed | 33 |
| differ | 12,057 |
| measured | 15,539 |

Of the 33 headline gains, 17 are rows that were already measured and became
`match` when the string-predicate transfers landed (headline 928 → 945 with that
commit alone); 2 are the `more-types.php` pair that needed no analyzer change at
all; the other 14 are transfer answers this reseed made scoreable. **Zero rows
left `match`**, verified as a per-row set diff keyed on file+line.

The phpstan-src checkout is a **live working tree** (`corpus.local.toml`), and it
moved during this slice: the measured total rose from 15,509 to 15,539 as seven
nsrt files gained 31 observations. All 31 are new `differ`/`unsupported` rows and
none is a `match`, so the headline movement above is the slice's alone — but a
future comparison against these numbers should re-measure its own baseline rather
than assume the corpus stood still.

### The foldable-allowlist reseed (2026-08-01, issue #78)

Eighteen builtins joined the portable fold subset and six more joined the
width-refused rows (which still fold on this 64-bit measuring machine). No
analyzer code moved with them — the fold lane reads the catalog — and nsrt is
where that shows up as precision.

Reseeded baseline (at the issue-#78 slice):

| | count |
| --- | --- |
| match (**headline**) | 1,003 |
| unsupported | 2,488 |
| subsumed | 37 |
| differ | 12,011 |
| measured | 15,539 |

The corpus did **not** move this time: the record set is the same 15,849 keys
(15,539 measured + 310 skipped), verified as a symmetric difference of zero on
file+line, so every movement below is the slice's.

All 42 headline gains are `differ → match`, and **zero rows left `match`** —
per-row set diff keyed on file+line. By file: `str_increment.php` 18,
`str_decrement.php` 15, `gettype.php` 3, `functions.php` 2, `str-casing.php` 2,
`version-compare-php7.php` 1, `version-compare-php8.php` 1. The four
`differ → subsumed` rows are all `functions.php:168–171`, where a folded `''`
is a proper subtype of the asserted `string`/`string|false`.

Two of these are worth naming. `version_compare` scores as a *width-refused*
row: it folds on the 64-bit measuring machine and declines in the browser, and
nsrt measures the former. And `gettype.php` moved because `gettype` returns one
word from a fixed vocabulary, which the declared `string` envelope could never
say.

## `gen-catalog`

Regenerates `steins-catalog::hierarchy_generated` from
`docs/research/phpsrc-mining/hierarchy.toml` **and
`steins-catalog::return_facts_generated` from `return_facts.toml`** (ADR-0056 R3+R4,
the eleven curated return rows). The TOML is the **source of record**; the Rust
files are `@generated` and carry the php-src commit pin and the PHP version they
were cross-checked against. Editing the Rust by hand is a defect.

The mining directory also holds `throws.toml`, `failure_arms.toml`,
`return_facts.toml`, `effects_gaps.md`, and a `crosscheck.txt` — the per-arm C
evidence behind the catalog's claims.

## Conformance

Steins runs the external `php-typing-conformance` suite. Standing at the last
recorded triage: **85/98**, with every remaining non-#14939 failure registered
in the divergence registry as a standing refusal or an honest deferral, and zero
absent-machinery failures among them at that time.

The suite adapter (`SteinsChecker` plus a `--tool` filter) exists in the
maintainer's working tree and is not committed — roadmap gate G4. It affects
measurement convenience only.

## Test discipline

~1,350 `#[test]` functions across the workspace, weighted toward
`steins-infer/tests/` (40 integration files: arity, branch analysis, effects,
throws, offsets, object acceptance, truth tables, short-circuit, match/switch,
phpdoc contracts, …).

Two structural tests deserve naming because they enforce invariants rather than
behavior:

- **`tests/registry.rs`** — the diagnostic id totality reconciliation. See
  [diagnostic-shape.md](diagnostic-shape.md).
- **the domain's property tests** — `γ(a) ∪ γ(b) ⊆ γ(join(a, b))` over generated
  facts. The same statement is *proved* for every value by the Lean spec
  (ADR-0059); the property tests stay because they exercise the real
  implementation, which the proofs do not.
- **`crates/steins-domain/tests/lean_vectors.rs`** — the Rust leg of the
  `lean-check` loop above.

The standing rule recorded in the roadmap: **zero conformance regressions,
ever.**

## Not implemented

- **A performance harness.** No cold/warm baselines are measured under `xtask`;
  the ~200s full-batch figure is an observation, not a tracked metric
  (roadmap M5).
- **Mutation testing** of the checker itself.
- **CI wiring** for the gate beyond running it locally.
