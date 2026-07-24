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
written — fully-qualified, qualified, or unqualified — and resolution applies
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

## The folding seam

```rust
trait Folder {
    fn fold(&mut self, name: &str, args: &[ArgValue]) -> Option<ArgValue>;
    fn absence_family_available(&mut self) -> bool { false }
    fn boot_surface_class_like(&mut self, fqn: &str) -> Option<bool> { None }
    fn boot_surface_function(&mut self, fqn: &str) -> Option<bool> { None }
    fn php_minor(&mut self) -> Option<(u16, u16)> { None }
}
```

Two implementations: `NoFold` (the sound subset) and `SidecarFolder`. Every
default is the conservative answer — no fold, absence family unavailable,
existence unanswerable, no detectable version skew (`php_minor` feeds the
ADR-0052 A11 catalog-skew demotion) — so the sound subset is what you get by
*not* implementing anything. See
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
the universe; and every **non-literal** `class_alias`. It exists to gate the
existence-absence ids only — which have no emitter yet, so today the fact is
carried and tested but consumed by nothing. Method-absence needs no dam (PHP
cannot reopen a defined class).

An empty shared dam is used by the auxiliary passes, which never emit an absence
id and so never read it.

## The finding-breadth emitters

The ADR-0049 family, landed stage by stage (each stage's silence legs are
tabulated at its emitter):

| Emitter | Ids | Gate |
| --- | --- | --- |
| `check_undefined_method` (S2) | `call.undefined-method` | exact-class receivers only; hierarchy fully enumerated; `absence_family_available` (A9) plus the boot-surface class homonym leg (A2ii) |
| `check_offset_read` (S3) | `offset.missing`, `offset.on-unsupported` | proven container values under the read-context whitelist; warning-grade findings obey the `warning-handler` pseudo-constant |
| `check_arity` (S5) | `call.too-few-arguments`, `call.unknown-named-argument` | uniquely-resolved userland functions or proven-exact receivers; the boot-surface *function* homonym leg |
| `check_phpdoc_undefined_method` (S6) | `phpdoc.undefined-method` (contract layer) | the declared-receiver lane over narrowed contract-arm lists, under per-arm descendant closure |

Every doubt leg in every table is **silence** — the family widens the finding
surface, never the proof standard. The dump surface's `emit_dumps` (ADR-0053
D3) sits beside them: a recognized `PHPStan\dumpType()` /
`PHPStan\dumpPhpDocType()` call emits its fact rendering as a debug-layer
answer.

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
