# Two-layer diagnostics: zero-FP proof layer, named policy profiles, no numeric levels

Steins splits diagnostics into two classes. The **proof layer** is always on and
held to Rigor's zero-false-positive bar: report only what provably breaks on a
live path ("the program works" outranks the worst-case static reading). The
**policy layer** is opt-in via named profiles: works-but-violates findings such
as coercive-mode implicit conversions, annotation-restraint violations, and
effect-declaration violations. PHPStan's numeric levels 0–9 are deliberately
not adopted — levels make "what will be reported" opaque and would import
PHPStan's level culture wholesale; adoption cost is absorbed by a baseline
mechanism instead.

One scope on the zero-FP sentence, ratified 2026-08-09: it covers the proof
layer's **definite** ids. A possibly-grade id — proof layer, `Strict` floor —
makes a partial-path claim, is off every default run, and is held to
non-increase rather than to zero; see ADR-0081 §8 for the ruling and the
membership rule.

Consequence accepted explicitly: `width("5")` on a coercive-mode `int`
parameter is **silent by default** (it works at runtime), while `width("abc")`
is a proof-layer finding (proven `TypeError`). Value-precise analysis
distinguishing these two is the differentiation from PHPStan in one screen.
