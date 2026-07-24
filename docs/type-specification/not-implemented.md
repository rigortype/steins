# Not Implemented

This document exists so no other document has to be vague. Everything here is
either **designed with no code**, or **known imprecision** that costs true
positives. Nothing here costs *false* positives: an unknown widens to silence,
which is the whole shape of the zero-FP bar.

Sequencing and exit criteria live in [`docs/ROADMAP.md`](../ROADMAP.md); this is
the semantic inventory.

## Designed, no code

### Type and inference machinery

| Surface | ADR | Note |
| --- | --- | --- |
| Generic type-argument carry through a variable binding | 0032 | A heap object records no type arguments; `$x = new Box('x'); f($x)` judges only the class half. Stage 1 (the direct-`new` argument position) landed. |
| Narrowing N5/N6 — property-chain guards, static-prop channel, structured loops | 0052 | Deferred out of v0.1.0 by owner decision; designed in full in ADR-0052 §7–8. |
| Template scope transfer | 0051 | Templates as functions, render sites as call sites. Out of v0.1.0 scope by owner decision; promoted only if dogfooding demands it. |
| Callable signatures beyond the closure-variance arm | 0033 | A declared `callable(P): R` is checked against a *closure argument*; nothing else consumes it. |
| `resource` type / resource-value tracking | 0030 reg. suite 4 | Needs `fopen()`-style values modeled through `=== false` narrowing. |
| Value-provenance labels | 0038 | Reserved as the general mechanism in place of taint analysis. |
| Ecosystem packs — PSL, Serde, Valinor, PSR | 0044, 0045 | Dependent shapes, witness refs, mapper returns as runtime truth. The mapper-boundary types are exactly where legacy modernization needs truth. |
| Plugin contract | 0012, 0039 | Composer-distributed declaration suppliers with pattern subscriptions. The sidecar's `plugin` method is a documented stub returning `widen`. Consequence: ecosystem effect labels (`io.redis`, `email.send`) cannot be registered, so they are correctly unknown. |

### Diagnostics and CLI

| Surface | ADR | Note |
| --- | --- | --- |
| `call.undefined-function`, `class.undefined` | 0049 S4 | Registered with layers; no emitter. Scoped into v0.1.0, not landed. |
| `call.too-many-arguments` | 0049 §6 | Internal targets only — userland too-many runs clean and is never a finding. Waits on the sidecar reflect slice. |
| `debug.type` / `debug.phpdoc-type` / `debug.var-dump` emission | 0053 D3/D4 | The lane, ids, and shared rendering landed (D1/D2); the emit slices were in flight at verification time. |
| Scoped policy — `[paths.sets]`, `[[policy]]` | 0023 | Designed in full, including semantic `where` matchers. The pipeline stage exists as a no-op with a seam. |
| `sarif` / `github` formats | 0054 | With CI auto-detection and format invariance as the binding rule. Decided out of v0.1.0 by owner. |
| `doctor` | 0054 | The posture report: coverage, sidecar health, catalog audit, baseline capture surface. A minimal `doctor` is scoped into v0.1.0, not landed. |
| `check --fix` fix-its | 0010 | Autofix as a first-class diagnostic payload. |
| `lsp` | 0048, roadmap M6 | Position queries are *constrained* today (replay over retention, canonical entry states, no global-ordering dependence) but not built. The flagship capability is type-directed member completion. |
| `mcp` | 0010, roadmap M7 | The agent-driven dry-run → diff → approve → apply loop. |
| `init` / config generators | 0020 | **Refused**, not deferred — zero-config is the banner. |

### Runtime knowledge

| Surface | Note |
| --- | --- |
| Extension-class reflection | Classes from unloaded PHP extensions are `Unknown`-silent. The sidecar's `reflect()` exists and is unused for class resolution. |
| The full effect catalog | What ships is a frequency-seeded starter set; ADR-0014's php-src stub sourcing is not built. |
| Computed folding purity | Folding permission is a hand-picked allowlist, not a derived property. |
| Locale/timezone pseudo-constants | The ADR-0008 opt-in that would let `mb_*` and locale-sensitive functions fold. |

## Known imprecision

Places where Steins is quieter than it could be.

**Control flow** ([narrowing.md](narrowing.md)):

- Loops are `Opaque` — write/read-set invalidation only, no loop-carried facts
  (ADR-0052 N6, deferred out of v0.1.0 by owner decision).
- `try`/`catch`/`finally` is `Opaque` for value flow (catch *matching* works).
- No reachability analysis: a construct that early-returns on every branch makes
  fall-through code dead, and a fact about a variable it never reads could
  describe an unreachable path.
- Static properties are not a fact lane; property chains (`$a->b->c`) are a
  `Barrier` (ADR-0052 N5, same owner deferral).
- `??` in guard position does not refine; it yields a value fact only.
- Array elements do not narrow — an array is a fact only when *fully* known.

**Objects** ([object-model.md](object-model.md)):

- `__get`/`__set` are not modeled; `__call` is an absence-proof obstacle.
- Traits are an obstacle, not a modeled method source.
- A `Member` fact on a `final` class is not treated as exactness in v1.
- `Closure::bind`/`bindTo` rebinding drops the binding.

**Propagation**:

- Binding descent is capped at 8 frames (`MAX_BINDING_DEPTH`), plus on-stack
  recursion detection. Past the cap: silence.
- Vendor propagation is budgeted per package (ADR-0015).

**Docblock tags not read** ([phpdoc-grammar.md](phpdoc-grammar.md)):
`@method`, `@property`, `@mixin`, `@phpstan-type` aliases,
`@phpstan-import-type`, `@phpstan-pure`, `@phpstan-impure`.

## Engine and performance

- **No cross-run persistence and no warm path.** salsa memoizes `parse`,
  `function_index`, and a monolithic `project_index`; the check pass itself runs
  *outside* the query graph because folding is impure (ADR-0028). Nothing of
  inference survives a run.
- **`project_index` is monolithic** — any file edit invalidates it and
  everything downstream. Acceptable for a batch CLI; the recorded plan is
  per-symbol interning, which the LSP needs (ADR-0009).
- **No perf harness.** Full batch over the ~99.3k-file corpus is CI-viable on
  dev hardware; there is no measured cold/warm baseline under `xtask`.

## Deliberate refusals

Not gaps. Recorded here so a reader does not file them as such: numeric
strictness levels, worst-case `maybe`-reporting, message-regex suppression,
benevolent-union semantics, a call-site template solver, a
`TypeCombinator`/`TypeUtils` layer, lint and format rules, Rector integration,
tool-specific docblock tags beyond `@phpstan-*`/`@psalm-*`, `init`, and a
PHP-version emulation matrix. Each is anchored in an ADR; see
[overview.md](overview.md) and `docs/ROADMAP.md`'s "Won't build".
