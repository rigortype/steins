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

## Ruled out on this evidence

**Interning type values** — one canonical instance per distinct type, so an identity check stands in for structural comparison ([phpstan/phpstan-src#6261](https://github.com/phpstan/phpstan-src/pull/6261), which measured −3.1% upstream). It does not transfer. That win comes from identity fast paths already sitting on PHPStan's hot paths, over `Type` objects that are the allocation-heavy graphs ADR-0035 explicitly declined; here the scalar layers are canonical by construction and compare in a few words, `Fact` and `ContractTy` are plain values with no identity to exploit, and the measured target is under 1% of real-code CPU. Introducing one would mean a handle representation threaded through roughly 40 files, and Salsa's `#[salsa::interned]` is not the route — it wants a database handle at every construction site, which inverts `steins-domain`'s zero-dependency layering.

If the profile ever moves — a shape-heavy workload where `steins-domain` clears, say, 15% — the thing to attack first is its allocator share (cheap clones of `ShapeFact`), not its comparison share. And note the precondition upstream had to fix first: interning is only sound if every operation returns the same result for *equal* operands as for *identical* ones, which for us also means no provenance bit may sit outside the hashed key (`Stratum` lives beside the fact in `Known`; `Presence::Required { witnessed }` lives inside `ShapeFact` and is hashed — moving either would let interning launder ADR-0037's trust order).

## Perf harness

`cargo xtask perf <target-dir>... [--runs N] [--bless] [--no-php] [--warm] [--paranoid] [--edits]` measures a cold library-path run per target tree — the same load → parse → `check_project` pipeline `fp-gate` drives, never a shelled-out binary, so process startup is not in the numbers — and reports load+parse, analyze, and total wall clock as the median over N runs (default 3). Each run gets a fresh salsa DB and a fresh sidecar, so every run is cold by construction; the OS file cache is the one warmth the harness does not control. The corpus checkouts make good targets (`cargo xtask perf corpus/nikic__PHP-Parser`).

`--warm` adds the generation lifecycle: a cold build into a scratch store, then N warm rebuilds, with the analyze phase split into merge / whole-universe facts / each fixpoint / the walk loop / the reporting passes (issue #516), and the per-run counters — files loaded, parsed, and **decoded** (a loaded file's tree is only decoded where a walk reaches it, so a no-change rebuild should report zero). `--paranoid` turns the walk verifier on: every file is walked anyway and every would-be skip is compared against its fresh walk.

`--edits` seeds five edit shapes over a *copy* of the target — leaf, core, the declarer most class-likes live in, a file addition, a file removal — and grades each twice: a cost run in one store (what a warm rebuild of that edit actually does) and a paranoid run in another, with a fresh cold build of the same edited tree as the oracle. The shapes are chosen by a property of the universe rather than by a file name, so the same five land on any target: a leaf is the file whose declared names the corpus spells least. It implies `--warm --paranoid`, never writes to the target, and is the evidence a warm-path change is asked for.

Baselines live in `perf.local.toml` at the repo root — machine-local and untracked, the `corpus.local.toml` precedent. `--bless` records, per target: file count, findings count, a SHA-256 of the serialized findings, the median timings, and the engine posture (`php` or `no-php`).

What fails vs what only warns:

- **Determinism fails the run.** Every invocation runs the full cold analysis at least twice on identical inputs and asserts the findings serialize byte-identically (sorted the way `steins check` sorts its output). A mismatch prints a per-diagnostic-id count diff and exits red. This is the **cold half of ADR-0092 §5's warm ≡ cold oracle**; issue #489 extends the same comparison to warm-vs-cold when the generation layer lands.
- **A findings-hash mismatch against the baseline fails the run.** Either the target tree moved or the analyzer changed what it finds — triage, then re-bless consciously.
- **A posture mismatch is an error, not a number.** A baseline blessed under the other engine posture is refused before any hash comparison, exit 2.
- **Timing only warns.** Deltas against the blessed medians print but never gate — machine variance. The provisional M5 targets live in the harness, not the roadmap: cold within 10% of the pre-persistence baseline (printed as an advisory when crossed), warm re-check ≤ 2s p95 at the ~30k-file scale (unenforceable until the warm path exists).
