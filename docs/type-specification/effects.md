# Effects

**Status: implemented** for the labels, envelopes, propagation, and checks
described below. The plugin channel that would open the registry is **designed,
not implemented**. ADR-0005, ADR-0006, ADR-0008, ADR-0018, ADR-0019, ADR-0033.

## The second dimension

An **effect** is what an expression does beyond computing its value: throw,
output, filesystem, network, global state, nondeterminism. Steins infers and
propagates effects exactly as it does types — the second inferred dimension
(ADR-0005), and the design differentiator against every other PHP checker.

## Labels

An effect's canonical identity is a **hierarchical dot-path string**
(ADR-0018). Checking is by **prefix subsumption**, segment-aware:

```text
subsumes("io", "io.net.http")  = true
subsumes("io", "iota")         = false      // segment-aware, not string prefix
```

A declared `io` therefore admits an inferred `io.net.http`.

### The registry

The known-label set is closed today. It is the union of every label the catalog
can color a builtin with, plus the ADR-0018 taxonomy roots:

```text
exit
ffi
global.read   global.write
io   io.db   io.fs   io.fs.read   io.fs.write   io.ipc
     io.net  io.net.http   io.process   io.signal
mutate
nondet   nondet.random   nondet.time
output   output.header
failure   failure.environment   failure.input   failure.resource
```

A declared label outside this set — and not an ancestor of an entry — earns
`effect.unknown-label`, with a Levenshtein-based suggestion (`io.netw` → did you
mean `io.net`). Typo safety is Steins' own job, not the user's.

`failure.*` is the odd family: those labels name a `false`/`null` failure arm's
*value provenance* — why the arm exists — rather than an effect. They share the
registry so prefix subsumption works and a future boundary profile can name them
(ADR-0042). See [divergence-registry.md](divergence-registry.md).

`ffi` is a deliberate top-level escape hatch beside `exit`: FFI runs arbitrary C,
so the catalog can prove nothing about it. No plain builtin is colored `ffi`
(FFI is OO-only); the label exists so `#[\Steins\Effect('ffi')]` is valid.

**Ecosystem and private labels** (`io.redis`, `email.send`) are *correctly*
unknown today: they become known only through the ADR-0012 plugin channel, which
is not implemented. The registry is designed to be open; it is closed in practice
because nothing can open it yet.

## Envelopes

An **effect envelope** is a declared upper bound. Its presence opts the
declaration into always-on contract checking; absent an envelope, nothing is
checked.

Envelopes are spelled as **native PHP attributes**, not docblock tags
(ADR-0006):

```php
#[\Steins\Pure]                          // the empty set — the tightest bound
function slug(string $s): string { … }

#[\Steins\Effect('io', 'nondet.time')]   // an upper bound of two labels
function log(string $m): void { … }
```

Both the fully-qualified spelling and a `use`-imported bare `#[Pure]` /
`#[Effect(...)]` are recognized. When both attributes decorate one declaration
they are contradictory (`Pure` is the tighter bound): `Pure` wins, and this slice
emits no diagnostic about the contradiction.

**`@throws` is not the effect syntax.** It stays Throwable-only
([throws.md](throws.md)); the analogy to declarative effects is as far as the
relationship goes.

## Origin closure

Effects have exactly two origins (ADR-0005): **catalogued builtin/extension
functions**, and **language constructs**. Nothing else creates an effect; user
code only propagates. An uncatalogued function widens to *unknown effect*, which
taints exhaustiveness but produces no finding.

Recognized origins in a body:

| Origin | Effect |
| --- | --- |
| a statically-named function call | the catalog's labels for it, or a propagation edge to a project function |
| `echo` / `print` / `<?=` | `output` |
| `exit` / `die` | `exit` (ADR-0019 rule 4 — `Pure` forbids exit) |
| a resolvable method call (`$this->`, `self::`, `parent::`, `Foo::`, `new Foo()->`) | a method→method propagation edge |
| a higher-order builtin with a resolvable callback | the callback's effects, per the [invocation shape](closures.md) |
| a `$fn()` call resolved to a known callback | the callback's effects |
| anything else dynamic | **no** effect, but exhaustiveness is tainted |

The `$this->`/`self::` edges are drawn under a **final/private guard**: a
non-final public method may be overridden, so its resolved body is not
authoritative. `parent::` and `Foo::` are exact.

The origin scan is **structural, not reachability-aware**: an `echo` in provably
dead code is still an origin. This is deliberate — an envelope is a contract
about the function's *code*, not about one execution path, so `Pure` forbids the
mere presence of an effectful construct.

## Propagation

Effects propagate to a fixpoint over the resolved call graph, joined with an
**exhaustiveness bit** that is tainted by any unresolved or dynamic call. The
consequences are asymmetric on purpose:

- The **envelope check** (`effect.envelope-exceeded`) reads only the *proven*
  effect set. A proven effect outside the declared envelope is a finding.
- The **exhaustiveness bit** never produces a finding. It surfaces in
  `annotate` as a `…?` marker: "these effects, and possibly more".

`effect.liskov-widened` applies the same proven-only rule across an override:
an implementation whose proven effects exceed the envelope declared on the class
or interface method it overrides is a finding. Implementations may be purer,
never less pure ([closures.md](closures.md)).

## Folding is gated on effects

The connection between the effect system and value precision (ADR-0008): an
expression may be folded by executing it in the [sidecar](overview.md) only when
its effect set is empty and `nondet` is absent on the concrete path.

In this slice that rule is applied as a **hand-picked allowlist** rather than a
computed property. Uncoloured functions widen — a miss, never a false positive —
which is the only seeding order compatible with the zero-FP bar. Locale- and
timezone-sensitive functions (`mb_*`, anything under `setlocale`) are excluded
even when frequent, because their value is not portable without the opt-in
pseudo-constant configuration this slice does not implement. See
[`docs/internal-spec/catalog.md`](../internal-spec/catalog.md).

## Not implemented

- **The plugin channel** (ADR-0012 / ADR-0039) that registers ecosystem labels
  and library effect signatures.
- **Envelope carrier interfaces as an ecosystem story** — the mechanism works
  (an interface method's envelope binds implementations), but no PSR knowledge
  ships to make DI-mediated effects checkable out of the box (ADR-0045).
- **The full effect catalog.** What exists is the frequency-seeded starter set
  above; ADR-0014's php-src stub sourcing is not built.
- **A computed purity property.** Folding permission stays an allowlist.
- **`fopen` mode-string discrimination** — it stays at the parent `io.fs` label.
- **Effect-precondition-driven transforms** (loop→map requires purity) — the
  transform engine exists, but no transform consumes effects yet (ADR-0034).
