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
| Plugin contract | 0012, 0039, 0068 | **Partial.** The manifest channel has **landed**: a `type: steins-plugin` Composer package's `steins-plugin.json` registers effect labels (vendor-root checked) and colors plain functions, whose labels enter the *declared* lane with the taint kept. Deferred: everything the sidecar half was for — synthetic declarations, pattern subscriptions, booting the real framework (the `plugin` JSON-RPC method is still the stub returning `widen`), method colorings, value-provenance registrations, response caching by environment fingerprint, and the ADR-0044/0045 packs. |
| Per-package vendor budgets | 0015 | Descent into `vendor/` bodies is implemented (diagnostics off); the budget cap that would bound it, naming its cutoff per the Certainty discipline, has no code. Vendor propagation runs uncapped today. |

### Diagnostics and CLI

| Surface | ADR | Note |
| --- | --- | --- |
| `call.too-many-arguments` | 0049 §6 | Internal targets only — userland too-many runs clean and is never a finding. Waits on the sidecar reflect slice. The only registered id with no emitter. |
| Scoped policy — `[paths.sets]`, `[[policy]]` | 0023 | Designed in full, including semantic `where` matchers. The pipeline stage exists as a no-op with a seam. |
| `sarif` / `github` formats | 0054 | With CI auto-detection and format invariance as the binding rule. Decided out of v0.1.0 by owner. |
| `doctor` (full report) | 0054 | The **minimal** `doctor` (ADR-0054 C3 scope — index-bound posture report, runs no emitter) has **landed**. Deferred: `--format json`, the richer audits (deeper catalog audit, full baseline capture-surface report). |
| `check --fix` fix-its | 0010 | Autofix as a first-class diagnostic payload has **landed**: a finding may carry a `Fix` (a title plus byte-span `FixEdit`s), `--format json` shows it as an additive key, and `check --fix` pours a run's fixes into one atomic plan that writes only past the ADR-0034 dual-verification post-check — a refusal is named and nothing touches disk. One family ships: deleting a committed `\PHPStan\dumpType()` / `\PHPStan\dumpPhpDocType()` statement (`debug.type`, `debug.phpdoc-type`), the remedy ADR-0053 names. Deferred: every further fix family. `debug.var-dump` carries no fix by decision, not by deferral — deleting legal working PHP is the author's call. |
| `lsp` | 0048, roadmap M6 | Position queries are *constrained* today (replay over retention, canonical entry states, no global-ordering dependence) but not built. The flagship capability is type-directed member completion. |
| `mcp` | 0010, roadmap M7 | The agent-driven dry-run → diff → approve → apply loop has **landed** as `steins mcp`: an MCP server on stdio with four tools (`list_transforms`, `plan_transform`, `apply_plan`, `check`), plan and apply deliberately separate, and a plan handle scoped to the serving process. Deferred: an `annotate` tool, MCP resources and prompts, and a tool that applies a finding's `fix` payload (the payload is returned; the agent applies it). |
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
- Reachability is decided **structurally only** (ADR-0078 §5, issue #199). Every
  statement carries a `BodyEnd` — `Terminates` / `FallsThrough` / `Unknown` —
  computed from the CST, and `body_end` folds a statement list to the same
  verdict. What it does not do is feed *value* flow: a construct that early-
  returns on every branch is now provably terminal, but a fact about a variable
  the dead tail never reads is still carried as if that tail ran. The judgment's
  own silences are `try`/`catch`/`finally` (excluded whole — `finally` overwrites
  the exit point), `goto`/labels, a `switch` with case-to-case fall-through, and
  a provably-infinite loop containing a `break` whose target is unresolved.
  `type.return-missing` is its only consumer today; the level-4 dead-code family
  is the deferred one, and reads `Unknown` the opposite way round.
- Static properties are not a fact lane; property chains (`$a->b->c`) are a
  `Barrier` (ADR-0052 N5, same owner deferral).
- `??` refines an *array offset* in guard position (ADR-0062 S5); over any other
  operand it yields a value fact only.
- Array shapes carry key presence, optionality and list-ness, and the
  `isset`/`array_key_exists`/`empty`/`??` family narrows them (ADR-0062) — but a
  write at a key Steins cannot prove widens the whole shape rather than refining
  it, and the value side of `in_array`/`array_search` declines to project through
  a shape at all (its answer is a multi-base union the value domain cannot spell).
- `array_slice` projects through a shape (ADR-0062 Amendment B), but only as far
  as the element union, the key class and list-ness carry: it claims no size
  bound, and it never projects *positionally* from a declared shape — a key set
  has no runtime order (§2). An order-witnessed array is where the exact slice
  comes from, and only there.

**Objects** ([object-model.md](object-model.md)):

- `__get`/`__set` are not modeled; `__call` is an absence-proof obstacle.
- Traits are an obstacle, not a modeled method source.
- `@method`/`@property`/`@mixin` are absence-proof obstacles too, not member
  sources: `$obj->scopeActive()` still resolves to nothing.
- A `Member` fact on a `final` class is not treated as exactness in v1.
- `Closure::bind`/`bindTo` rebinding drops the binding.

**Propagation**:

- Binding descent is capped at 8 frames (`MAX_BINDING_DEPTH`), plus on-stack
  recursion detection. Past the cap: silence.

**Docblock tags not read as types** ([phpdoc-grammar.md](phpdoc-grammar.md)):
`@phpstan-pure`, `@phpstan-impure`.

**Docblock tags read as obstacles only** (ADR-0049 A14): `@method`,
`@property`, `@property-read`, `@property-write`, `@mixin`, `@phpstan-type`
aliases, `@phpstan-import-type`. Steins recognizes each tag's presence and its
subject — the method name, the property name, the `@mixin` target — and records
one `(class-like, kind, subject)` obstacle per tag site. A class-like carrying
any of them anywhere in its resolved reach (parents, interfaces, `@mixin`
targets transitively) is not enumerable, so the absence family is silent on it,
exactly as for `__call`. Reading them as **member sources** — resolving
`$model->scopeActive()` or `$model->created_at` to a type — remains deferred,
as does the subject-granular discharge channel that would re-enable the
absence proof for a class-like's *undeclared* remainder (ADR-0039's to design).
Their types are never parsed: only the subject is.

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
