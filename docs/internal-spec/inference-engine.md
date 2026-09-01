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

The binding vector also admits pseudo-bindings: `use:{name}` for closure capture
snapshots, `this:` (carrying the exact receiver **class FQN**) for method
descents under `resolve_exact` (ADR-0075 §2.1) so two subclasses sharing an
inherited body never share a memo hit, and `obj:{param}` for a seeded argument
object (below). A memo hit does not re-walk: the first receiver's summary answers
for the key, and the second receiver's walk (and any emissions unique to it) are
suppressed — hence the key components.

**Entry state on the heap** (ADR-0086 §2, the argument leg). The callee is no
longer walked with an empty `Store`. For every positional argument that denotes a
caller heap object — a variable bound in `Store::refs`, or a direct `new` in
argument position — the descent seeds the callee's store with a **copy** of that
`HeapObj` under a fresh callee-local `AllocId` bound to the parameter's name.
`class`, `class_exact` (copied, never promoted), `readonly`, `ro_written` and
`targs` cross verbatim; `escaped` is always `true` on the copy; non-readonly props
cross only from a caller object with `escaped == false`, readonly props always.
There is **one copy per distinct caller allocation**, so aliasing among the
arguments survives (`f($b, $b)` binds both parameters to one callee object) while
aliasing with anything outside the argument list is excluded by the escape rule.
A seeded object counts as a binding, so an object-only argument list descends. The
`obj:{param}` key component is the canonical rendering of that entry state (class,
exactness, readonly bookkeeping, the sorted key-representable props with their
strata, the carries); a prop the rendering cannot name does not cross, so the memo
stays a pure function of the key (ADR-0048 §2).

**The receiver is the zeroth argument** (ADR-0086 §3, the receiver leg). A
method call on an **exact `Receiver::Var`** — the one receiver form with a heap
object in hand, and the same arm that fills `CallTarget::receiver_carries`
(#362) — seeds the callee's `$this` from a copy of that object, under the field
table above and **sharing the one-copy-per-caller-allocation map with the
argument copies**, so `$b->m($b)` binds `$this` and the parameter to one callee
object. `analyze_scope`'s `$this` seed is the seam: it finds `this` already
bound and leaves it alone. A seeded `$this` counts as a binding, so a
zero-argument `$b->get()` descends. Every other receiver seeds through
`seed_this_object` exactly as before, each for a stated reason: `Receiver::This`
is pre-escaped by construction (its non-readonly props would not cross anyway),
a non-exact `Receiver::Var` resolves through the override guard and proves no
identity, `Receiver::New` has no allocation yet, and a static call has no
receiver. A **constructor** descent seeds `$this` from the fresh allocation
instead (below). The `this:` key component correspondingly carries the object's
canonical rendering where a copy was seeded and the bare exact class FQN where
none was (ADR-0075 §2.1, amended).

**A class-typed parameter is a heap object** (ADR-0032's declared-parameter-seed
amendment, #388), at every entry where neither of the two copies above landed —
the plain per-scope pass above all. The gate is `!store.is_bound(param)` (and no
`env` value), so a copy always wins. What seeds: a parameter whose **native**
hint is exactly one non-nullable class the index knows. What it seeds: `class`
from that hint, `class_exact = false` (audit G1), `escaped = true`, no props,
`readonly`/`ro_written` from the class surface through the same derivation
`seed_this_object` uses, and `targs` from the `@param`'s own type arguments as
`CArg::Ty` — owner-keyed, arity-aligned to the owner's `@template` list, sited at
the declaring file and offset, and dropped whole where an argument names a
template or lowers to `Opaque`. `?Box`, a `= null` default, a union, an
intersection, an unknown class, a by-ref or variadic position, and a `@param`
that disagrees with the native hint or is not a plain (parameterized) class each
seed nothing. The class never comes from the docblock alone: `HeapObj::class`
carries no stratum, and it premises both the guarded dispatch below and the dump
surface's un-`(asserted)` rung.

Three consumers move with it. `resolve_call_target`'s **non-exact**
`Receiver::Var` arm keeps the final/private override guard and now fills
`receiver_carries` from the object's *declared* carries (`receiver_var` stays
`None` — no identity is proven, so no `$this` copy is owed), which is what lets
#362's `template-type` read work on a declared receiver. `check_phpdoc_param`
and `bind_call_templates` read those same carries through `declared_carrier`,
`resolve_cval` declining a lower-bound object on purpose; only the **argument**
half of `Class<A, …>` judges there, the class half staying `Maybe`. And
`resolve_arity_method` admits a lower-bound receiver under the final guard —
ADR-0049 §6's declared-receiver refusal rests on an override adding optional
parameters, which `final` forecloses.

The value surface reads the same summary (ADR-0075 §3 as amended 2026-08-16).
`ArgValue::MethodCall { callee, args, named }` carries a method or static call
in value position — `Callee::Function` stays `ArgValue::Call` — and
`Receiver::New` carries the constructor's own arguments, so `(new C(1))->m()`
dispatches on an object the constructor wrote. `project_method_summary` is the
value-position entry: it resolves through `resolve_call_target` and only
through it, so the `this:` key rendering is the statement walk's own and one
body is walked once however many positions call it. `takesString($b->unwrap())`
now agrees with `$v = $b->unwrap();`, and `dumpType($b->get())` with the dump of
`$v`. Five things stay out, each named at its own layer: an **object** result
(ADR-0057 B5 — the value consumers read `summary.value` and nothing else),
`Receiver::Prop` (never a dispatch target, ADR-0052 §7), `nullsafe` (§3.1
below), the **store-less** roads (`resolve_literal_under` answers `None`, so the
fold and concat lanes see no methods), and `$this`/`self`/`parent` receivers at
the two entries that hold no enclosing class (the nested-argument binding and
`best_dump_phpdoc_type`, which pass `None` and let `resolve_call_target`'s own
arms decline).

A `?->` call rebinds nothing, in value **or** statement position (ADR-0075
§3.1): `resolve_call_target` never read the flag, so `$x = $b?->m()` used to
take `m`'s summary and declared arms as if the receiver were provably non-null.
Both are declined now; the arguments are still checked and the body still
descended.

The caller-side escape-and-sweep after the call is untouched by any of this
(ADR-0086 §2's stated refusal): the copy flows in and the sweep flows out,
independently, until ADR-0055 Part II can prove non-mutation per parameter.

An `Opaque` construct with `may_return` contributes the declared return floor to
the summary join (hidden exits inside `foreach`/`try`/…); untyped fallthrough
contributes `Singleton(null)`. Both keep a visible `return null` from becoming a
false Singleton when other exits were invisible.

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

The same descent also yields a **return summary** (`ReturnSummary`, ADR-0057
amendment slice T0; ADR-0075 for methods/statics) with two independent
components. The **value component** is the join, over a
callee's returning exits, of the returned expression's value-domain fact, carried
at the `min` trust stratum over those exits (an `Asserted` exit drags the whole
summary to `Asserted`). It rides the same `BindingKey` memo — now a value map —
and is consumed at the call-result binding as the value **floor** above the
declared arms, for both free functions and resolved method/static calls. A
constructor has no value component to read — it evaluates to an object, and an
object is not a value — and reads the heap one instead (below). It is a pure
function of
`(callee CST, bound entry state [, exact receiver])`, so it is a legitimate
replayable query answer — and since ADR-0086 §2 the "bound entry state" includes
the argument objects seeded onto the callee's heap, which is what lets
`return $box->value` summarize at all.

The **heap component** (ADR-0057 §1, slice **T1**) is the outbound half: at every
returning exit that hands back a locally-held allocation — `return new Foo(...)`
(minted through the same `new_heap_object` an assignment runs), `return $local`,
`return $this`, or an inner call's own heap summary — the walk snapshots the
object, and `join_heap_exits` joins those snapshots per ADR-0057 §2.4. Classes
must agree, exactness is copied and never promoted, props survive only where
every path has them (facts joined, strata at `min`), `escaped` ORs, readonly and
its write bookkeeping intersect, carries survive only where identical — and
**any** non-allocation exit ends the whole component (a `null`, a scalar, an
untyped fallthrough, an `Opaque` `may_return` floor). The declared return type is
never consulted: A2's native oracle belongs to the value component alone.

The caller **rebinds** the snapshot at the `apply_assign` rung (functions and
methods alike): a fresh walk-local `AllocId`, the object verbatim, `refs[var]`
bound to it. So `$f = createFoo(123);` gives `$f->n` the value `123`, S2 and the
arity family their exactness premise, and `readonly.reassigned` its first write —
and an `escaped = false` rebind survives a later unknown call exactly as a local
`new` does. Value and argument position rebind nothing: they have no store, and
an object is not a value (ADR-0035) — ADR-0057 B5's fence, which is what keeps
the method carrier above from reading the heap component too.

The two components live and die independently, and a summary exists whenever
either does.

**`new C(args)` is the constructor descent's `$this` snapshot** (ADR-0057's
2026-08-16 constructor-summary amendment, the successor to T1). The descent that
already walked `__construct` for its diagnostics is now *read*. Its `$this` is
seeded from the object the site mints — `new_heap_object` with **every** literal
default (the ADR-0086 §4 lexical gate is bypassed, the walk being the body that
gate approximated), every promoted parameter, `class_exact = true`, the readonly
bookkeeping and the carries — under the same field table as a receiver copy with
`escaped` decided the other way: **`false`**, the one copy that is not
pre-escaped, because a `new` site has no caller-side object for the call to
escape. Every exit contributes that `$this`: each `return;`, and the body's
fall-through, which is a constructor's normal exit. A `throw` or `exit` yields no
object and contributes nothing. `join_heap_exits` joins them unchanged, and the
caller's fresh allocation **is** the join — class and exactness asserted rather
than copied, everything else replaced. The replacement (rather than a widening)
is licensed by the allocation having had no alias before the constructor ran.

One walk per site, over three disjoint seams: wherever the lowering builds a
`Callee::Construct` call (assignment, statement, property assignment, `return new
C()`) the walk runs at the call rung and the object build later in the same
statement consumes its snapshot; in **argument** position, where no such call is
lowered at all, the walk runs inside ADR-0086 §2's mint; and in **receiver**
position (`(new C(1))->m()`), where the lowering likewise builds no constructor
call, it runs where the receiver object is minted (issue #386 — the value-IR
limit ADR-0057 C7 deferred this leg to). Each seam is the site's only site.

**A same-`$this` call descends and copies `$this` back** (ADR-0057's 2026-08-17
amendment, the successor to C5). `$this->m(…)`, `self::m(…)`, `parent::m(…)`,
`static::m(…)` and the by-name `Foo::m(…)` of issue #417 hand the callee a copy
of the walk's own `$this` under the ADR-0086 §2 field table with **`escaped`
crossing verbatim** — a same-`$this` call hands nothing over, so the bit is what
it was an instant earlier. Inside a constructor walk that is `false` (C1) and
the non-readonly props cross; in an ordinary method it is `true` and only the
readonly ones and the carries do. The exit snapshot is a **third** summary
component beside `value` and `heap`, filled by any walk whose `$this` came from
a caller object, joined by the same `join_heap_exits`, and copied back into the
caller's object at the call site: `props`, `readonly`, `ro_written` and
`escaped` replaced, `class`/`class_exact` asserted, and `targs` deliberately
**not** restored (a class-level carry is rewritten by `@phpstan-self-out`, which
the walk models not at all, so the #295 sweep stands). The **receiver** leg
travels the same road — an exact `$o->m()` already seeded its `$this` from a
copy of `$o`, so a fluent `$o->setX(1)` reads its own write. A constructor's
snapshot is that same component, read where the `new` site mints its object.

The copy-back runs after the statement's escape-and-sweep pass, so the sweep is
the **decline floor** for free: an unresolvable target, a guarded-refused one, a
poisoned scope, named or general-spread arguments, the budget, a recursion pair, a
generator, a resolved static target (which carries no `$this` at all), an
`Opaque` `may_return` exit and an exit at which `$this` is gone all leave it
standing. Two statement-scoped guards decline a composition that cannot be
ordered: an unresolved call anywhere in the statement, and two snapshots naming
one object. Value position (`f($this->m())`) keeps the sweep too — that road
holds the caller's store by shared reference and has no write-back channel, the
same structural limit ADR-0057 B5 records for the heap component there.

An unescaped `$this` still owes its other sweep: inside a constructor walk it is
swept by the same `object_passed || unknown` condition that sweeps escaped
objects, a non-static closure being able to bind `$this` without naming it. The
condition reads the bit off the object rather than off the walk's flavour, which
is where the rule lives — `seed_this_object` pre-escapes every other `$this`.

Where the descent declines — no constructor, abstract, unresolvable, poisoned on
either side, a named or general-spread argument list, the depth budget, a recursion pair,
every path throwing, or an exit at which `$this` is gone — the site keeps the
object `new_heap_object` builds under the ADR-0086 §4 lexical gate, unchanged.
That gate is now the floor for undescended constructors and nothing else.

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
| `check_undefined_function` / `check_undefined_class` (S4) | `call.undefined-function`, `class.undefined` | a clear dynamism dam (A5); every candidate answered not-a-function/not-a-class-like by the boot surface (A2ii) and `absence_family_available` (A9); `class.undefined` runs the §5 ladder over the file's `hard_class_refs` (the four hard-error expressions plus, per A15, inheritance clauses, `catch` clauses and native type declarations); the message register is seeded by `boot_surface_label` |
| `check_arity` (S5) | `call.too-few-arguments`, `call.unknown-named-argument` | uniquely-resolved userland functions or proven-exact receivers; the boot-surface *function* homonym leg |
| `check_phpdoc_undefined_method` (S6) | `call.undefined-method` (proof layer) when every surviving arm is `Verified`; `phpdoc.undefined-method` (contract layer) when any arm is `Asserted` — the ADR-0049 A13 minimum-stratum routing | the declared-receiver lane over narrowed contract-arm lists, under per-arm descendant closure; the ladder is identical for both ids, only the id moves. The lane also travels across a plain copy `$c = $o` (issue #196), so a declared parameter assigned to another variable keeps its arms and their strata |

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
