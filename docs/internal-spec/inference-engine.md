# The Inference Engine

**Status: implemented** (`steins-infer`). This is the largest crate and the one
that holds the zero-FP bar; everything here exists to turn `Maybe` into silence.

## Entry points

| Function | Scope |
| --- | --- |
| `check_project(db, project, folder)` | the whole-project check — the CLI path |
| `check_project_with_runtime(...)` | the same, with `[runtime]` pseudo-constants |
| `annotate_project(db, project, folder)` | per-line proven facts for the margin |
| `check_file` / `diagnostics` / `check` / `check_with` | single-file entries, implemented as a one-file project |
| `effect_summary(tree, functions, classes)` | per-file effect/throw margin |
| `dam_facts(units)` | the whole-universe dynamism dam |

The single-file entries run over a one-file project, so every same-file
soundness guard keeps working unchanged. There is no separate single-file
analysis path to drift.

## The project view

```text
FileUnit { path: &str, tree: &SourceTree }     // one file in the analyzed project
Cx { … }                                       // read-only analysis context
```

`Cx` is the whole-project view plus the file currently being analyzed. It is
cheap to copy (all borrows), and interprocedural descent rebuilds it at the
callee's file via `Cx::at`.

## Name resolution

Conservative PHP semantics (ADR-0001). A `NameRef` records how the name was
written — fully-qualified, qualified, unqualified, or `namespace\`-relative
(ADR-0049 A8: the leading `namespace\` is stripped and the remainder resolves
against the enclosing namespace only, no `use` imports and — for functions — no
global fallback) — and resolution applies
`use` imports, the current namespace, and the global fallback against
[`project_index`](query-graph.md) plus the builtin catalog.

Never resolved, therefore always silent:

- an FQN with two or more definitions in the project (`Resolve::Ambiguous`);
- a userland definition shadowing a builtin;
- a dynamic callee or an unresolvable receiver.

## The walk

Per scope: a recursive branch walk over the [trace IR](trace-ir.md), threading
an environment and an object store.

```text
env:   HashMap<String, Known>
Known { fact: Option<Fact>, closure: Option<ClosureVal>,
        stratum: Stratum, line: u32, bound: Option<String> }

Store { refs: var -> AllocId, heap: AllocId -> HeapObj,
        contract: var -> Vec<ContractArm>, members: var -> Member }
```

`WalkCx` carries the immutable per-scope context: the scope, the enclosing
class, the exact `$this` class when known, return-type information, a
`RefCell<Vec<Span>>` of proven-dead regions, and a monotone allocation-id
counter.

The allocation counter lives in a `Cell` *shared across branch clones* — branches
clone the `Store`, not the counter — so a `new` in one branch can never collide
with a `new` in another that later joins.

`Flow` records whether a walked sub-trace fell through or terminated
(`return`/`throw`/`exit`, or an `if` where no branch falls through). Proven-dead
regions are recorded only from the plain per-scope walk: a binding descent's dead
branches are dead *for that binding only*, so descents discard theirs.

The three fact lanes (`env`, `Store::contract`, `Store::members`) and their
consumption rules are specified in
[`docs/type-specification/narrowing.md`](../type-specification/narrowing.md).

## Binding descent

The interprocedural half of call-site propagation. When a call's arguments are
proven, the walk descends into the callee's scope with those bindings.

```text
BindingKey = (callee-key, [(param, ArgValue)])
Descent { provenance, depth, stack, memo }
```

Three bounds, all producing **silence** rather than a finding when hit:

1. **`MAX_BINDING_DEPTH = 8`** — a chain of calls propagating a literal is
   followed at most eight frames.
2. **The on-stack binding set** — direct and indirect recursion is caught by
   `stack` before the depth bound.
3. **The memo set** — a `(callee, bindings)` pair already analyzed is not
   re-analyzed.

A budget cutoff **names itself as silence** and never manufactures a finding
(ADR-0009). Closure bodies are descended the same way, using the scope's own
`params`.

The same descent also yields a **return-fact summary** (`ReturnSummary`,
ADR-0057 amendment slice T0): the join, over a callee's returning exits, of the
returned expression's value-domain fact, carried at the `min` trust stratum over
those exits (an `Asserted` exit drags the whole summary to `Asserted`). It rides
the same `BindingKey` memo — now a value map — and is consumed at the call-result
binding as the value **floor** above the declared arms. It is a pure function of
`(callee CST, bound entry state)`, so it is a legitimate replayable query answer.
The struct carries a heap-object component slot (ADR-0057 §1) for slice **T1**;
in T0 that slot is present but always `None` — no returned allocation is
transferred yet.

## The folding seam

```rust
trait Folder {
    fn fold(&mut self, name: &str, args: &[ArgValue]) -> Option<ArgValue>;
    fn absence_family_available(&mut self) -> bool { false }
    fn boot_surface_class_like(&mut self, fqn: &str) -> Option<bool> { None }
    fn boot_surface_function(&mut self, fqn: &str) -> Option<bool> { None }
    fn php_minor(&mut self) -> Option<(u16, u16)> { None }
    fn boot_surface_label(&mut self) -> Option<String> { None }
    fn builtin_return_fact(&mut self, name: &str) -> Option<Fact> { None }
}
```

Two implementations: `NoFold` (the sound subset) and `SidecarFolder`. Every
default is the conservative answer — no fold, absence family unavailable,
existence unanswerable, no detectable version skew (`php_minor` feeds the
ADR-0052 A11 catalog-skew demotion), no boot-surface label, no return fact — so
the sound subset is what you get by *not* implementing anything.
`builtin_return_fact` (ADR-0056 R1) seeds a uniquely-resolved builtin call's
reflected return envelope into the value domain — at an assignment RHS and at a
dump site — always at the `Verified` stratum, refused when the simple name
collides with a project user function. See
[folding-and-sidecar.md](folding-and-sidecar.md).

## The auxiliary passes

Two fixpoints run alongside the walk, over the *resolved call graph* rather than
the trace, because they must see constructs the trace erases.

**Effects** — `effects(f) = own origins ∪ ⋃ effects(callee)`, monotone to a
fixpoint, with an exhaustiveness bit tainted by any dynamic or unresolved call.
Origins come from the structural CST scan, not the trace.

**Throws** — `throws(f) = escaping own-throws(f) ∪ ⋃ filter(throws(callee),
caller-guards)`, the same shape, with catch-guard damming applied per origin and
its own exhaustiveness bit.

The asymmetry that matters in both: the **envelope check reads only the proven
set**; the exhaustiveness bit never produces a finding, only the `…?` marker in
`annotate`.

Semantics: [`effects.md`](../type-specification/effects.md),
[`throws.md`](../type-specification/throws.md).

## The dam

`dam_facts` aggregates whole-universe dynamism sites as a **query answer** —
recomputed per run, no entry state, no ordering dependence: every `eval`; every
**non-vendor** `include`/`require` whose path is not provably in-universe —
`Unproven`, a bare-relative or `./`-prefixed literal (A5 as amended: runtime
resolves those against `include_path` → the script dir → CWD, so
directory-relative belief is unsound; only absolute and `__DIR__`-anchored
literals can prove in-universe), or a provable literal that resolves *outside*
the universe; and every `class_alias` whose class names are **not known at
compile time** (a string literal and the `X::class` constant both are — the
latter is resolved by the compiler, so it mints an index edge instead). It gates the
existence-absence ids: since ADR-0049 S4 its consumers are live — the
`call.undefined-function` and `class.undefined` emitters fire only when the dam
is clear (a single `eval` or out-of-universe include withholds the whole
family). Method-absence needs no dam (PHP cannot reopen a defined class).

An empty shared dam is used by the auxiliary passes, which never emit an absence
id and so never read it.

## The finding-breadth emitters

The ADR-0049 family, landed stage by stage (each stage's silence legs are
tabulated at its emitter):

| Emitter | Ids | Gate |
| --- | --- | --- |
| `check_undefined_method` (S2) | `call.undefined-method` | exact-class receivers only; hierarchy fully enumerated; `absence_family_available` (A9) plus the boot-surface class homonym leg (A2ii) |
| `check_offset_read` (S3) | `offset.missing`, `offset.on-unsupported` | proven container values under the read-context whitelist; warning-grade findings obey the `warning-handler` pseudo-constant |
| `check_undefined_function` / `check_undefined_class` (S4) | `call.undefined-function`, `class.undefined` | a clear dynamism dam (A5); every candidate answered not-a-function/not-a-class-like by the boot surface (A2ii) and `absence_family_available` (A9); `class.undefined` runs the §5 ladder over the file's `hard_class_refs`; the message register is seeded by `boot_surface_label` |
| `check_arity` (S5) | `call.too-few-arguments`, `call.unknown-named-argument` | uniquely-resolved userland functions or proven-exact receivers; the boot-surface *function* homonym leg |
| `check_phpdoc_undefined_method` (S6) | `phpdoc.undefined-method` (contract layer) | the declared-receiver lane over narrowed contract-arm lists, under per-arm descendant closure |

Every doubt leg in every table is **silence** — the family widens the finding
surface, never the proof standard. The dump surface's `emit_dumps` (ADR-0053
D3) sits beside them: a recognized `PHPStan\dumpType()` /
`PHPStan\dumpPhpDocType()` call emits its fact rendering as a debug-layer
answer. `emit_trace_annotations` (ADR-0074) is its docblock twin, in the same
walk: a statement-adopted `/** @psalm-trace $x */` (the shared `stmt_docblock`
query, resolved at the top of the walk's per-statement step) flushes
`debug.trace` at the step's exit — the same rendering, against the statement's
**exit** facts, reported at the tag's own position, in the plain per-scope
pass only, with declaration statements inert.

Two read surfaces reach one level into the heap (ADR-0052 §7): a **depth-1
property fetch** `$var->prop` — allocation-keyed through the object store — reads
a proven member fact both as a dump argument and as a call receiver, so
`check_call_on_null` proves `call.on-null` on a `Receiver::Prop` whose depth-1
member is `Singleton(null)`. Anything deeper (`$a->b->c`) stays unknown and
silent.

## The annotate surface

`LineFact { line, kind }` with:

| Kind | Margin body |
| --- | --- |
| `Effects { labels, exhaustive }` | `effects: {io.fs.read, …?}` |
| `Throws { classes, exhaustive }` | `throws: {RuntimeException}` |
| `Value { var, rendered }` | `$x = 'abc'` |
| `ExactClass { var, class }` | `$u: App\User (exact)` |
| `Finding { id }` | `✗ type.argument-mismatch` |

The `…?` suffix is the non-exhaustiveness marker: "these, and possibly more".
Only **proven** facts appear — the margin never shows a guess.

## Diagnostic emission

Every emitter constructs a `Diagnostic` with a registry id; the registry
totality tests bind emitters to layers. Findings are deduplicated by structural
equality before display. Inline `@steins-ignore` matching runs in
`steins-infer::suppress`; the vendor filter, profiles, and baseline run in the
CLI. See [diagnostic-shape.md](diagnostic-shape.md).

## Not implemented

- **Memoization of anything in this crate.** The check pass runs outside the
  query graph ([query-graph.md](query-graph.md)).
- **Parallelism.** The walk is single-threaded; ADR-0015's per-package vendor
  budgets bound cost instead.
- **Incremental re-check.** A run is a run.
