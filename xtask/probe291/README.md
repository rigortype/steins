# issue #291 probe — measurement scaffolding

**Not for merge.** This directory and the `phpdoc.probe291-*` ids in
`steins-infer` exist to answer issue #291 with numbers; they are a probe's
apparatus, not a shipping surface.

## The PHP witness grid

`witness.php` runs a real call per cell — a parameter of type `T` in a file whose
`declare(strict_types=1)` is the mode under test, handed a value of base `B` —
and records accept / `TypeError`. Nine value witnesses × eight parameter types ×
two modes = 144 cells.

```
php witness.php strict   > witness-strict.tsv
php witness.php coercive > witness-coercive.tsv
python3 derive.py                     # the base-level NO / partial / ok table
```

The committed `.tsv` files were produced on **PHP 8.5.9** (the pinned sidecar
version `steins doctor` reports).

`gen_grid.php` emits the same grid as a Steins fixture (`grid-<mode>.php`), so
`steins check --profile contracts` can be diffed against the `php -r` answers.
The two agreed cell for cell when this probe ran.

## The analyzer side

`cargo xtask probe291 [--corpus] [--nsrt [DIR]] [--conformance [DIR]]` counts the
scratch ids over the three sources. The `DIR` defaults follow `cargo xtask nsrt`'s
sibling-checkout convention and therefore **do not resolve inside an agent
worktree** — pass the path explicitly there. `STEINS_PROBE291_CENSUS=1`
additionally emits the denominator (`phpdoc.probe291-census`), aggregated by
shape rather than listed per site.

The probe emits nothing unless `STEINS_PROBE291=1` is set; `cargo xtask probe291`
sets it, and nothing else in the tree does.
