# Profiling

How to get a trustworthy CPU profile of `steins`, and what the current one says.

## Getting a profile at all

`[profile.release]` sets `strip = "symbols"`, so a stock release binary profiles as a wall of `???`. Rebuild with debug info into a scratch target dir so the normal `target/` stays as CI builds it:

```
CARGO_PROFILE_RELEASE_DEBUG=1 CARGO_PROFILE_RELEASE_STRIP=none \
CARGO_TARGET_DIR=/tmp/steins-prof \
cargo build --release -p steins-cli
```

`steins-cli` has no features, so `--features cli` fails the build rather than doing nothing. A full rebuild is about a minute (thin LTO, `codegen-units = 1`).

On macOS, sample the run rather than instrumenting it:

```
/tmp/steins-prof/release/steins check corpus >/dev/null 2>&1 &
sample $! 12 1 -file /tmp/prof.txt -mayDie
```

## Three ways the numbers lie

**`sample` reports inclusive counts, not self time.** Its output is a call tree where each node's count includes its whole subtree. Self time is the node's count minus the sum of its *immediate* children — post-process for it, or every leaf's cost shows up attributed to `main`.

**Restrict to the worker thread.** Every subcommand runs on one worker thread sized by `WORKER_STACK_SIZE`, and the main thread sits in `pthread_join` for the entire run. Totalling all threads puts about a third of the samples in `__ulock_wait` and rescales everything real by the length of the run. Find the thread subtree containing the `steins_*` frames and profile that alone.

**Inlining moves cost across crate boundaries.** With thin LTO and `codegen-units = 1`, small callees are inlined into their callers and attributed to the caller's crate. Rust's mangled names carry the *instantiating* crate as a trailing token, which recovers attribution for generic instantiations (`RawVec` growth, hash lookups) but not for inlined-away functions. Good for an order-of-magnitude judgment; useless for arguing about one percent.

## What the profile says today

Worker-thread self time, master `842f710`, 2026-08-25. Two workloads: the public `corpus/` packages, and synthetic code written to saturate the array-shape stratum (nested shapes joined across branches, dumped after each step).

| | `corpus/` | shape-saturated |
| --- | --- | --- |
| steins-syntax (CST lowering) | **44.7%** (57% of it in the allocator) | **56.7%** (72%) |
| steins-infer | 39.3% (40%) | 12.4% (69%) |
| mago (parser) | 7.8% | 9.1% |
| steins-phpdoc | 5.2% | 0.1% |
| steins-domain (the `Fact` algebra) | 0.7% (32%) | 13.6% (60%) |
| steins-contract (`ContractTy`) | 0.2% (24%) | 6.6% (68%) |

Peak RSS is 1.4 GB on `corpus/` and 1.1 GB on the shape-saturated run — *inversely* correlated with shape density, so the resident set is CST arenas and reflection state, not type values.

The cost centre is a family of whole-subtree scans in `steins-syntax` that re-run for each enclosing statement: `scan_var_usage`, `scan_invalidated`, `subtree_has_goto`, `collect_presence_shield`, `scan_effect_origins`, `scan_opaque`, `collect_call_vars`, `collect_read_vars`, `collect_scopes`. Each is a pure function of its subtree, so each is memoizable on subtree identity. Riding on top of them, Mago's `Node::children()` returns a `Vec<Node>` — one heap allocation per node visited, which is where the third-largest self-time entry (`RawVec::finish_grow`) comes from.

`corpus/` under-represents array-shape density. The workload that would show the shape stratum honestly is the private half of `cargo xtask fp-gate`, so re-measure there before acting on the second column.

## The large private project (#658, 2026-10)

The profile above is of the public corpus. The largest private fp-gate project was a different story until #884 (bounded shapes) landed: it did not finish, because one generated file with tens of thousands of straight-line `$x[] = <literal>;` appends cost O(N³) in `walk_trace` → `apply_offset_append` → `array_push_written_fact` → `sealed_with_order`, and nothing crosses file boundaries in that cost.

With #884 in (2026-10-02, release build, `--no-cache --no-php --profile strict`, tens of thousands of files), the project completes in about 186 s wall with peak RSS about 21 GiB (22.7 GB), and the per-file walk is no longer the bottleneck. The split, from `--progress`:

| phase | time |
| --- | --- |
| discover | 3.6 s |
| parse | 90.6 s |
| universe + purity oracle | 15.4 s |
| walk (1 worker) | 28.4 s |
| report (fixpoints in all: effects 15.2 s, throws 35.5 s) | 36.3 s |
| suppress + output | 6.5 s |

- **The cost is spread across phases.** Parsing is about half of the wall, and the report phase costs more than the walk. The "fixpoints in all" figures are not additive with the report's span: the effects fixpoint is forced early by the purity oracle, so its time is inside the universe + purity-oracle row, and only the rest of it lands in the report. Six files took 250 ms or more to walk and none took a second, so a walk that dominates again will show as one or a few slow-file lines, not a flat tail.
- **Do not read its profile as the shape-saturated column above.** That column is a synthetic workload; this project's cost was one input pathology, now bounded.
- **Peak RSS is the parsed universe, not the walk.** The 10 GB and the 65 MB readings in the #658 diagnosis came from one process at two stages: the whole project's parsed trees held in the salsa database, which is the resident set while the walk runs, and what is left once the process is paged out while stuck on one file. Nothing grew in the hot loop. So an RSS number says how large the parsed universe is, scaled by file count and size; it is not evidence of a leak, and it does not move when a walk-time fix lands.

To see where a run is, use `steins check --progress` (phases and slow files) and, in the gate, the start and end lines and `--deadline` (see `docs/agents/verification.md`).

## Ruled out on this evidence

**Interning type values** — one canonical instance per distinct type, so an identity check stands in for structural comparison ([phpstan/phpstan-src#6261](https://github.com/phpstan/phpstan-src/pull/6261), which measured −3.1% upstream). It does not transfer. That win comes from identity fast paths already sitting on PHPStan's hot paths, over `Type` objects that are the allocation-heavy graphs ADR-0035 explicitly declined; here the scalar layers are canonical by construction and compare in a few words, `Fact` and `ContractTy` are plain values with no identity to exploit, and the measured target is under 1% of real-code CPU. Introducing one would mean a handle representation threaded through roughly 40 files, and Salsa's `#[salsa::interned]` is not the route — it wants a database handle at every construction site, which inverts `steins-domain`'s zero-dependency layering.

If the profile ever moves — a shape-heavy workload where `steins-domain` clears, say, 15% — the thing to attack first is its allocator share (cheap clones of `ShapeFact`), not its comparison share. And note the precondition upstream had to fix first: interning is only sound if every operation returns the same result for *equal* operands as for *identical* ones, which for us also means no provenance bit may sit outside the hashed key (`Stratum` lives beside the fact in `Known`; `Presence::Required { witnessed }` lives inside `ShapeFact` and is hashed — moving either would let interning launder ADR-0037's trust order).

## Perf harness

`cargo xtask perf <target-dir>... [--runs N] [--bless] [--no-php] [--warm] [--paranoid] [--edits] [--check-rss] [--baseline PATH]` measures a cold library-path run per target tree — the same load → parse → `check_project` pipeline `fp-gate` drives, never a shelled-out binary, so process startup is not in the numbers — and reports load+parse, analyze, total wall clock, and peak RSS as the median over N runs (default 3). Each run gets a fresh salsa DB and a fresh sidecar, so every run is cold by construction; the OS file cache is the one warmth the harness does not control. The corpus checkouts make good targets (`cargo xtask perf corpus/nikic__PHP-Parser`).

### Peak RSS (#913)

Each cold run executes in its own **child process**: the xtask re-invokes itself as the hidden `perf-child <dir> [--no-php]` subcommand (absent from the usage text; the protocol is in `xtask/src/perf/child.rs`), which does one cold run and prints a header line (file count, timings, findings hash, byte length) followed by the canonical findings text. The parent drains that, reaps the child itself with `wait4`, and reads `ru_maxrss` from the returned `rusage`, normalised to bytes (macOS reports bytes, Linux KiB) and printed as MB (10⁶ bytes). Timing is taken inside the child around the same load and analyze phases as before, so process startup is still not in the numbers, and the determinism oracle and the findings hash read the same text they always did. A stream that is truncated, padded or hashes differently from its header is an error, not a hash mismatch.

Why a child: in one process `ru_maxrss` is a lifetime high-water mark, so it stops moving after run 1 and includes the harness. In the child it is that run's own peak. It folds in the PHP sidecar as a maximum, not a sum, and the sidecar is far smaller, so the number is the analyzer's.

- **The number is one build profile's.** `cargo xtask` is a debug build; a release build has a different resident set. Bless and check under the same profile (and the same OS: a blessed macOS peak says nothing about Linux).
- **`--warm` stays in-process** and records no RSS; only the cold runs have one.
- **`--bless`** stores `peak_rss_mb` (the median over the runs, rounded to 0.1 MB) beside the timings. An entry blessed before this field existed still loads; it has no ceiling until re-blessed.
- **The ceiling.** `--check-rss` fails the run (exit 1) when the median peak is above the blessed value × 1.10 (`RSS_CEILING_FRACTION`; exactly at it holds). It is an error (exit 2), not a pass, when there is nothing to compare against: no entry for the target, an entry without `peak_rss_mb`, or a platform without `wait4`. It cannot be combined with `--bless`. Without the flag the same comparison prints as an advisory line. A reduction never fails; ratchet the ceiling down by re-blessing, which stays a conscious act.
- **`--baseline PATH`** reads and writes that file (relative to the repo root, or absolute) instead of `perf.local.toml`. `perf.local.toml` is untracked, so a CI job keeps its blessed ceilings in a tracked file of its own.

A CI job over the public corpus runs, with the ceilings blessed on the runner's own OS and kept in the tracked file:

```
cargo xtask perf corpus/nikic__PHP-Parser --runs 3 --no-php --check-rss --baseline perf.ci.toml
```

and `--bless --baseline perf.ci.toml` (without `--check-rss`) re-records it. `--no-php` keeps the findings hash, which the same run also checks, independent of the runner's PHP.

`--warm` adds the generation lifecycle: a cold build into a scratch store, then N warm rebuilds, with the analyze phase split into merge / whole-universe facts / each fixpoint / the walk loop / the reporting passes (issue #516), and the per-run counters — files loaded, parsed, and **decoded** (a loaded file's tree is only decoded where a walk reaches it, so a no-change rebuild should report zero). The cold build reports the same split, because it is the one run that walks the whole universe. `--paranoid` turns the walk verifier on: every file is walked anyway and every would-be skip is compared against its fresh walk.

The walk line also says how many workers it fanned out over (issue #490). The width is trimmed to the *walk*, not to the universe, so a rebuild that walks a handful of files reports `1` however wide the machine is. `STEINS_WALK_WORKERS` names the width instead — `=1` is the walk in place, which is the sequential code path itself and therefore the honest "before" for any measurement of a walk-loop change.

`--edits` seeds five edit shapes over a *copy* of the target — leaf, core, the declarer most class-likes live in, a file addition, a file removal — and grades each twice: a cost run in one store (what a warm rebuild of that edit actually does) and a paranoid run in another, with a fresh cold build of the same edited tree as the oracle. The shapes are chosen by a property of the universe rather than by a file name, so the same five land on any target: a leaf is the file whose declared names the corpus spells least. It implies `--warm --paranoid`, never writes to the target, and is the evidence a warm-path change is asked for.

Baselines live in `perf.local.toml` at the repo root — machine-local and untracked, the `corpus.local.toml` precedent. `--bless` records, per target: file count, findings count, a SHA-256 of the serialized findings, the median timings, and the engine posture (`php` or `no-php`).

What fails vs what only warns:

- **Determinism fails the run.** Every invocation runs the full cold analysis at least twice on identical inputs and asserts the findings serialize byte-identically (sorted the way `steins check` sorts its output). A mismatch prints a per-diagnostic-id count diff and exits red. This is the **cold half of ADR-0092 §5's warm ≡ cold oracle**; issue #489 extends the same comparison to warm-vs-cold when the generation layer lands.
- **A findings-hash mismatch against the baseline fails the run.** Either the target tree moved or the analyzer changed what it finds — triage, then re-bless consciously.
- **A posture mismatch is an error, not a number.** A baseline blessed under the other engine posture is refused before any hash comparison, exit 2.
- **Peak RSS gates only under `--check-rss`.** Above the blessed value × 1.10 fails the run; see Peak RSS above.
- **Timing only warns.** Deltas against the blessed medians print but never gate — machine variance. The provisional M5 targets live in the harness, not the roadmap: cold within 10% of the pre-persistence baseline (printed as an advisory when crossed), warm re-check ≤ 2s p95 at the ~30k-file scale (unenforceable until the warm path exists).
