# The parameter-coercion witness grid

What **PHP itself** does with one value of each base handed to each native
parameter type, in both coercion modes — measured by running the calls, not by
reading the spec. The committed `.tsv` files are the oracle
`crates/steins-infer/tests/coercion_witness_grid.rs` pins Steins against, cell
for cell, in both modes.

The grid answers the base-level question issue #391's judgment is built on:
"does a native parameter of type `T` reject *every* value of base `B`?" — which
is not the same question as "does a representative value pass". A base whose
acceptance is not uniform across its own values needs one witness per
equivalence class of PHP's coercion behaviour, and two of the four bases split:

* `int`, `float` — uniform in both modes, one witness each;
* `bool` — a `false` literal union member (`string|false`) accepts exactly one
  of `true`/`false`, so both are needed;
* `string` — coercive mode decides on `is_numeric`, so a numeric and a
  non-numeric witness are both needed. This is the whole reason a `string` base
  is not a coercive-mode definite No against an `int` parameter.

Plus `null` and `array`, which are side-flags rather than bases. Nine values ×
eight parameter types = **72 cells per mode**.

## The internal grid

The same nine values against **real internal functions** rather than a declared
userland `f` — because PHP's coercion table is not quite the same on both sides
of that line. From 8.1 on, `null` into a non-nullable scalar parameter of an
internal function is a *deprecation* in coercive mode and a `TypeError` only
under `strict_types` (ADR-0056 §9.3):

```
coercive  strlen  string  null  null  accept  deprecated
strict    strlen  string  null  null  TypeError
```

That one cell is why this second grid exists; the other rows are here so a
second divergence cannot hide behind it. Three targets — `strlen` (`string`),
`dechex` (`int`), `abs` (`int|float`) — cover a single scalar and a scalar
union, and the parameter type is **read off reflection at run time**, not
written down, so a signature change shows up as a diff. Rows carry one extra
column (the function name):
`mode <TAB> function <TAB> param <TAB> value-class <TAB> literal <TAB> accept|TypeError <TAB> [deprecated]`.

`crates/steins-infer/tests/builtin_param_types.rs` pins Steins against these
files cell for cell, with the same rule: a Steins finding where PHP accepts is
a false positive and fails the test outright; a silence where PHP raises is
admissible only as a named divergence.

## Regenerating

```
php witness.php strict   > witness-strict.tsv
php witness.php coercive > witness-coercive.tsv
php witness.php strict   internal > witness-internal-strict.tsv
php witness.php coercive internal > witness-internal-coercive.tsv
```

The committed files were produced on **PHP 8.5.9** — the version `steins doctor`
reports for the pinned sidecar. Each userland row is
`mode <TAB> param <TAB> value-class <TAB> literal <TAB> accept|TypeError <TAB> [deprecated]`,
and the literal column makes every row a reproducible one-liner.

`gen_grid.php <mode>` emits the same cells as a Steins fixture — one call site
per line, each carrying PHP's verdict in a trailing comment — for the human
loop:

```
php gen_grid.php strict > /tmp/grid-strict.php
steins check --profile strict /tmp/grid-strict.php
```

## The two divergences, both silences

63 of the 72 cells agree per mode. The nine that do not are all Steins staying
silent where PHP raises, never the other direction, and both classes are
deliberate:

1. **The whole `array` row** (7 cells per mode, every non-`array` parameter).
   `is_type_error` answers `false` for an `ArgValue::Array` by construction:
   an array argument's mismatch is the phpdoc contract relation's to report, not
   the native check's. Recorded in the divergence registry.
2. **`null` into a class-typed parameter** (1 cell per mode). The native check
   stays silent on `null` against an object-bearing type, which is what keeps
   the implicitly-nullable `f(\DateTime $d = null)` idiom from convicting.

The test encodes both as a named exception list rather than as a weaker
assertion, so a *new* divergence — in either direction — fails it.
