# Verification

Use this guide when changing CI workflows, rustdoc, inference compatibility, or
CLI dispatch and exit codes. `.github/workflows/ci.yml` defines the CI jobs and
their commands; `.github/workflows/composer.yml` defines the Composer channel.

## Gates that pass without exercising the change

- **`fp-gate`:** the private corpus is untracked and machine-local, so a
  checkout without it silently measures only the public packages. Treat that
  run as partial, and run the private half alongside CI when the change needs
  the full corpus claim. The private half of the baselines (count rows and
  triage pins for the private-corpus projects) is equally untracked:
  `fp-gate.local.toml` at the repository root. The gate refuses to run when
  `corpus.local.toml` lists a project and that file is missing (an empty file
  runs with no local rows), and the line under the report's header says whether
  it was loaded. Copy both files into a worktree.
- **`nsrt`:** `cargo xtask nsrt [DIR]` needs a local `phpstan-src` checkout and
  does not run in CI. Run it alongside CI when the change affects inference
  compatibility.
- **Composer:** `composer.yml` is path-filtered, so a CLI change that breaks
  the Composer channel stays green until the next `composer/**` PR or the
  weekly scheduled run. After changing command dispatch or exit codes, dispatch
  that workflow explicitly.

## Reading an `fp-gate` run

`cargo xtask fp-gate [--deadline SECS]` keeps its two streams apart: stdout is
the report, the one CI and people compare, and stderr is progress. Do not parse
stderr, and do not add to stdout for the sake of progress.

- **Progress (stderr).** Each project writes `fp-gate: <project>: start (N
  file(s))` when it begins and `fp-gate: <project>: done in <t> (cold <t>:
  <phase> <t>, ...; warm <t>: ...)` when it ends. The phases are the ones
  `steins check --progress` reports, per pass. A file whose walk takes 5 s or
  more is named as it is found (`fp-gate: <project>: cold: slow file: <path>
  walked in <t>`; `STEINS_PROGRESS_SLOW_MS` changes the threshold). Projects run
  in parallel, so lines from different projects interleave, but each line is
  whole.
- **Deadline.** A project that has run longer than `--deadline` (default 1200 s,
  cold and warm pass together; `0` disables) ends the gate with **exit 3**,
  apart from a red verdict (1) and a command that could not run (2). Nothing
  ran to a verdict, so it says nothing about findings.
- **Reading a deadline failure.** The headline names the oldest project past
  the deadline and its elapsed time. Below it, **every project still running**
  is listed, oldest first and starred when past the deadline, each with its
  pass (`cold`, `warm`, or `setup` before the first), the last phase the
  analyzer *finished* (a phase is named as it ends, so the running phase is the
  next one) and every file of that project whose walk had begun and not ended,
  longest first. The oldest project is not always the one running: projects
  share the walk pool, and a thread that waits on the pool can run another
  project's jobs, so read the whole list. Files in flight is the actionable
  part: re-run `steins check --progress --no-cache --profile strict <file>` on
  the longest one and read its slow-file and phase lines. A pinned corpus
  package's paths are relative to the repository root (`corpus/<package>/...`)
  and a local project's to its own root. "No file of this project is walking"
  means it may be waiting on the shared walk pool or on another project: look at
  the projects below it, and at the phase, since the universe and purity-oracle
  fixpoints and the parse are whole-project work outside the per-file walk.
- **The watchdog cannot unwind the run.** The analysis is in-process on rayon
  workers, and a worker in a CPU loop cannot be interrupted, so the watchdog
  thread flushes stdout and calls `std::process::exit(3)`. The scratch stores
  under `target/fp-gate-stores/` stay on disk; the next run wipes them before
  its cold pass.
- **A deadline failure is not a regression verdict.** A slow machine can trip
  it; re-run with a larger `--deadline` before concluding the analyzer stalled,
  and use the files in flight to tell a slow file from a stuck one.

## Gates that fail on something the diff hides

- **Rustdoc:** the `docs` job rejects a public doc item that intra-doc-links a
  private one. Fix the link; a warning suppression hides the break rather than
  fixing it.
- **Generated tables:** see `docs/agents/mining.md`.
