# CI templates

Copy-pasteable CI configuration for running Steins in your project's
pipeline. See [chapter 6, "CI integration"](../06-ci.md) for the
exit-code contract, the baseline loop, and the annotation format
these templates use — this README only lists the files and their
assumptions.

| File | Copy it to | What it does |
| --- | --- | --- |
| [`github-actions.yml`](github-actions.yml) | `.github/workflows/steins.yml` | **The minimal template.** Checkout, `setup-php`, install Steins via Composer, `steins check .`. Plain log output; no annotations. |
| [`github-actions-annotations.yml`](github-actions-annotations.yml) | `.github/workflows/steins.yml` | Same install, plus `--format github` — GitHub Actions workflow commands, so findings render as inline `::error`/`::warning`/`::notice` annotations on the diff. |

Both assume:

- **GitHub Actions**, `ubuntu-latest`.
- **The Composer install channel** — your project has a `composer.json`
  Steins can resolve autoload roots from. If your project has no
  `composer.json` at all, use the prebuilt-binary or Homebrew channel
  instead ([installation and quickstart](../01-installation-and-quickstart.md)
  covers all four).
- **A `.steins-baseline.jsonl` committed at the repo root, or none yet.**
  Neither template runs `--set-baseline` — that command is a human,
  local action (see [chapter 6](../06-ci.md#the-baseline-loop-in-ci)). If
  your project has adopted a baseline, commit it before this workflow
  runs; if not, every pre-existing finding fails the first run, which is
  the expected first-adoption experience.
- **PHP on the runner, matching your project's declared floor.** The
  `php-version` input in both templates is a placeholder (`"8.3"`) —
  set it to what your `composer.json` requires.

Pick the annotations variant when you want findings visible inline on the
PR diff without a code-scanning upload step. Pick the minimal one when
plain job-log output is enough, or as the smaller starting point to build
your own variant from — a matrix over multiple PHP versions, a
`--no-php` job for a container without PHP, a `steins doctor` preflight
step (all discussed in [chapter 6](../06-ci.md)).

## Other CI systems

Not yet templated here — planned. The four steps are the same on any
system: put a `php` matching your project's floor on `PATH`, install
Steins (Composer, the prebuilt binary, or Homebrew), run `steins check
.`, and let the shell's own nonzero-exit-fails-the-job behavior gate the
pipeline — no Steins-specific glue is needed for that part on GitLab CI,
CircleCI, Jenkins, or anywhere else that runs shell steps. What a
GitHub-specific template adds over that generic recipe is exactly the
annotation rendering (`--format github`, which `check` also selects on
its own inside Actions); every other CI system gets the same value from
the minimal recipe with `steins check .` as its own step.

## Pinning Steins' version

The Composer channel pins automatically through `composer.lock` — commit
it, and `composer require --dev typedduck/steins` in CI installs the
exact version every developer resolves locally. `composer update
typedduck/steins` moves it deliberately. The prebuilt-binary and Homebrew
channels have no per-project lock file; pin the release tag in the
download URL, or the Homebrew formula version, the same way you would pin
any other CI dependency installed outside a project manifest.
