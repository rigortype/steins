# G1 measurement: direct vs propagated `throw.undeclared` split

Feeds the owner decision on whether a **direct-origin middle-stage profile**
for `throw.undeclared` is worth building. ADR-0050 G1's amendment made this a
measurement, not a taste call: *"decided by the pending direct-vs-propagated
measurement, not by taste."* This note supplies the numbers. It does **not**
decide.

## What "direct" vs "propagated" means (ADR-0050 §4)

- **direct**: the escaping throw's ORIGIN site — the point where the finding is
  reported — lies inside the body of the `@throws`-annotated declaration
  itself.
- **propagated**: the origin lies elsewhere, reached through calls made from the
  annotated declaration (one or more call hops into the effect graph).

## Classification rule used (and why it matches the ADR)

Each `throw.undeclared` finding carries a `ThrowFact` with the origin site
`(origin_file, offset)` — the file and byte offset of the throwing/calling
construct that produced the escaping throw (`crates/steins-infer/src/lib.rs`,
`emit_undeclared`). The annotated declaration being checked is analyzed in file
`cx.cur` and owns a list of `throw_origins` (the throw-relevant constructs
scanned out of *its own body*; ADR-0040 damming).

A finding was classified **direct** iff:

```
fact.origin_file == cx.cur                              // same file as the declaration
&& decl.throw_origins.any(|o| o.span.start == fact.offset)  // and matches one of the
                                                            // declaration's OWN body constructs
```

Otherwise **propagated**.

This is exactly the ADR's spatial definition. The origin offset is a unique
file byte position, and `throw_origins` is scoped to a single declaration's
body, so the same-file-plus-own-origin test distinguishes a throw arising in the
declaration's body (`throw new X`, or a builtin call the catalog knows throws)
from one that arrived up a call edge — even when the callee happens to live in
the *same file* as the annotated declaration (same file alone is not enough,
which is why the origin-offset membership test is required). It mirrors the
`compute_throws` fixpoint's own internal split: a fact enters a symbol's `direct`
seed set from its own `throw_origins`, versus arriving through an `edges` call
hop.

## Method

Temporary instrumentation on `master` at HEAD `82b7262` (reverted after the
run — this note is the only artifact that lands):

1. `emit_undeclared` was given the annotated declaration's `throw_origins` and
   tagged each emitted message with a control-char marker (`\x01D` + the
   declaration key for direct, `\x01P` for propagated).
2. The xtask fp-gate throw summary parsed the marker and printed, per package,
   `direct N (D distinct decls) / propagated M`.
3. `cargo xtask fp-gate` run once, foreground.
4. Instrumentation reverted (`git checkout`); tree returned to a clean
   `82b7262`; `cargo test --workspace` = **1073 passed** post-revert.

Corpus pinned per `corpus.lock.toml`; the private legacy monorepo
("pxxxx-monorepo") injected as a local project per `corpus.local.toml`.

## The table

| Package | `throw.undeclared` | direct | propagated | distinct decls (direct) |
|---|--:|--:|--:|--:|
| pxxxx-monorepo (legacy monorepo) | 43,963 | 158 | 43,805 | 134 |
| composer/composer | 91 | 46 | 45 | 12 |
| sebastianbergmann/phpunit | 79 | 13 | 66 | 8 |
| phpstan/phpstan-src | 20 | 7 | 13 | 4 |
| symfony/console | 10 | 0 | 10 | 0 |
| thephpleague/flysystem | 3 | 0 | 3 | 0 |
| guzzle/guzzle | 2 | 1 | 1 | 1 |
| Seldaek/monolog | 1 | 0 | 1 | 0 |
| nikic/PHP-Parser | 1 | 0 | 1 | 0 |
| **TOTAL** | **44,170** | **225** | **43,945** | — |

(Corpus `throw.*` grand total 44,184 = 44,170 `throw.undeclared` + 14
`throw.liskov-widened`. This note concerns only `throw.undeclared`.)

## Distinct-declaration spread of the monorepo's direct findings

The legacy monorepo's **158 direct** findings span **134 distinct annotated
declarations** (≈1.18 direct findings per declaration). A direct-only default in
the monorepo would therefore surface a wide, thin population of annotated
methods/functions — not a handful of hot methods each reported many times.

## Neutral reading (what each option costs, numerically)

Default-check line counts for `throw.undeclared`, corpus-wide:

- **keep-on** (every proven undeclared escape emits in the default profile):
  **44,170** lines — of which 43,805 are propagated escapes in the monorepo
  alone.
- **direct-only middle stage** (default emits only findings whose origin is in
  the annotated declaration's own body): **225** lines corpus-wide; **158** in
  the legacy monorepo, across 134 distinct declarations; the propagated 43,945
  move to an opt-up stage.
- **demote** (nothing in the default profile; the whole family opts up):
  **0** lines in default.

Propagated findings are the overwhelming mass (43,945 / 44,170 ≈ 99.5%), and
they are the ones whose origin is an unannotated callee rather than the
documented declaration itself. The direct slice is small and, in the monorepo,
spread across many declarations rather than concentrated.
