# Worktrees

What a `git worktree` costs on this machine, and the one lever that keeps the cost
off the disk. Measured 2026-09-16: the main checkout at master `5db1d1ac`, the
worktree column at `e066f88f`. The shape of the answer transfers; the numbers are
one machine's and one checkout's, so re-measure and re-date them rather than
acting on these.

## A worktree is free; its target dir is not

`git worktree add` writes a `.git` file pointing at `.git/worktrees/<name>`, so
the object store is shared and the only thing a worktree owns outright is its
checkout. Everything else is cargo.

| | |
| --- | --- |
| `target/` | **2.6 GB** — 96% of a 2.7 GB worktree |
| `target/debug/deps` | 1.6 GB — external crates 464 MB, workspace libs 251 MB, the rest test binaries |
| `target/debug/incremental` | 976 MB, one directory per integration-test binary |
| `target/release` | 305 MB |
| the checkout | 17 MB |

The main checkout's `target/` is 9.4 GB across 47,065 files, and the 3.1 GB of
`target/debug/incremental` in it is 19,758 of those files — two fifths of every
file the tree contains. That cache is worth its size only where a tight
edit/rebuild loop lives, which is the main checkout and not an agent's worktree.

## Cloning it back is the lever

On APFS `/bin/cp -c` calls `clonefile(2)`, and the clone shares every block with
its origin: 9.4 GB of `target/` clones in about six seconds for about 46 MB.
Cargo then reuses the part that is genuinely reusable — `cargo build --workspace`
in a fresh worktree compiled 13 crates, every one of them a workspace crate, with
mago, serde and the rest back **fresh**.

**Why the external crates return and the workspace crates do not.** Artifact
fingerprints are path-independent: 499 of the 532 units in one worktree carried
the same hash as the main checkout's, 319 of them byte-identical down to the
`path` field, and `dep-info` records source paths relative to the crate root. A
git or registry dependency therefore holds one unit identity across both trees,
and its sources live in `CARGO_HOME`, where no checkout can touch their mtimes. A
workspace crate's sources are written by `git worktree add` with mtime = now,
newer than the artifact cloned out of another tree — and cargo rebuilds whatever
source is newer than its output.

That asymmetry is the guarantee, not a shortfall of the trick. `clonefile`
preserves mtimes, so the obvious way to win the workspace half back — touching
the artifacts newer — would let a worktree on branch B link artifacts built from
branch A's sources.

**The build command decides the externals.** Cargo unifies features across the
units it is building, so `cargo build -p <one-package>` can want a different
feature set than the `--workspace` build the target was cloned from, and will
rebuild those externals. That is a resolution difference, not a broken clone: the
same `-p` build asks for a target dir that never existed, in any tree.

**Skip `incremental` when cloning.** In the main checkout it is 3.1 GB across
19,758 files, and a worktree turns it off (below), so a cloned copy is dead
weight — and it stops being shared, becoming disk nothing reads, the day the main
checkout cleans its own.

## incremental is not worth its keep in a worktree

A worktree sets `incremental = false` for `dev` and `test`, from a cargo config
committed at the one path that reaches worktrees and nothing else:

```toml
# .claude/worktrees/.cargo/config.toml
[profile.dev]
incremental = false

[profile.test]
incremental = false
```

Cargo merges the config of every parent directory, and the main checkout never
walks through `.claude/worktrees/`, so this costs the main checkout nothing — its
own `target/debug/incremental` is 3.1 GB and it keeps it. Tracing cargo's config
discovery confirms the split: the main checkout loads only its own
`.cargo/config.toml`, a worktree loads both.

Not written the obvious way, as `[env] CARGO_INCREMENTAL = "0"`: cargo applies
`[env]` to the subprocesses it spawns, while it reads `CARGO_INCREMENTAL` for
itself, so that spelling parses cleanly and changes nothing. Verified by A/B —
`-C incremental` survives it and does not survive the profile override.

A worktree created outside `.claude/worktrees/` (the `gw` sibling convention, say)
is out of that file's reach. Putting the config where it would reach both does not
work: the only common ancestor is a directory the main checkout also walks
through.

## What prepares a worktree

The config above is repo content, so a clone keeps its worktrees small without
being told. The clone step is not, and cannot be — git never versions
`.git/hooks` — so re-creating that half is a per-machine job. Ours is a
`.git/hooks/post-checkout` that fires only when the previous HEAD is the null
object (which is what `git worktree add` passes and an ordinary checkout never
does), only inside a linked worktree, and only after a one-file `clonefile` probe
against the *destination's* filesystem, so a fallback to a real multi-gigabyte
copy is impossible rather than merely unlikely. It never fails a checkout: a
clone that dies takes its partial output with it and the worktree arrives without
a `target/`.

For a docs-only or review-only worktree, `STEINS_NO_WORKTREE_TARGET=1` skips the
clone entirely — `target/` is the one thing such a worktree has no use for.

## Ruled out

**A shared `CARGO_TARGET_DIR`.** It saves the whole 2.6 GB where the clone saves
the externals' 730 MB (464 of `debug/deps`, 209 of `release/deps`, 56 of the two
`build/` trees), and the fingerprints make it look free: they are path-independent,
so both trees would address the same units. That is the problem. The same unit
identity means the same output filenames, so a worktree on one branch overwrites
the other branch's workspace artifacts on every build, and each alternation pays
for the workspace crates again. It also takes cargo's build-dir lock, and the
worktrees here are run in parallel. Reflinking keeps each worktree's blocks
private and still hands it the externals.

**`cargo build --release` in a worktree.** 305 MB here, 529 MB of `release/deps`
in the main checkout, for a mode whose artifacts a review never reads.
