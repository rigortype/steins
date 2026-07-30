# Effects

**Status: implemented** for the labels, envelopes, propagation, and checks
described below. The plugin channel that opens the registry is **partly
implemented**: a Composer package's manifest registers labels and colors plain
functions; the sidecar half that would boot the framework does not exist.
ADR-0005, ADR-0006, ADR-0008, ADR-0018, ADR-0019, ADR-0033, ADR-0067, ADR-0068.

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

The **builtin** label set is the union of every label the catalog can color a
builtin with, plus the ADR-0018 taxonomy roots:

```text
exit
ffi
global.read   global.write
io   io.db   io.fs   io.fs.read   io.fs.write   io.ipc
     io.net  io.net.http   io.process   io.signal
mutate   mutate.local
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

`mutate.local` is the degenerate member of the `mutate` family and the one label
**every** envelope tolerates, `#[\Steins\Pure]` included (ADR-0063 §2.3). It
names a by-ref out-parameter write whose target is a binding of the *calling*
frame — `preg_match($p, $s, $matches)`, `sort($localRows)`. Nothing escapes the
frame, so no caller can observe it, and an envelope constrains only what a caller
can observe. The tolerance is implemented for every envelope rather than for
`Pure` alone because `Pure` is the tightest one: tolerating a label there and
rejecting it under a wider declaration would make the check non-monotone.

The same builtin call *does* exceed `Pure` when its by-ref argument points
somewhere else. The color is decided per call site by the argument's lvalue root:
a frame-private binding earns `mutate.local`, a superglobal earns `global.write`,
and a property, static property, by-ref parameter, or unclassifiable target earns
the conservative parent `mutate`. Refining that parent into ADR-0055's
`mutate.self` / `mutate.instance` / `mutate.static` is that ADR's slice E2; until
it exists, the coarse-but-true label is preferred to a precise guess.

`ffi` is a deliberate top-level escape hatch beside `exit`: FFI runs arbitrary C,
so the catalog can prove nothing about it. No plain builtin is colored `ffi`
(FFI is OO-only); the label exists so `#[\Steins\Effect('ffi')]` is valid.

**Ecosystem and private labels** (`io.redis`, `email.send`) are not builtin, and
before issue #68 they were *correctly* unknown, because nothing could open the
registry. A Composer package of `type: steins-plugin` now can, through the
manifest channel.

A plugin ships a `steins-plugin.json` at its own package root:

```json
{
    "steins-plugin-api": 1,
    "labels": ["acme.cache"],
    "effects": { "acme_cache_get": ["acme.cache"] }
}
```

Steins reads it directly from `vendor/<name>/steins-plugin.json` after finding
the package in `vendor/composer/installed.json` — no PHP runs, so discovery is
deterministic and `--no-php` loses nothing. A `steins.toml` `[plugins] allow =
[…]` list **replaces** discovery with exactly the named packages (ADR-0039: the
explicit listing wins) and vouches for their identity.

Two rules govern what a plugin may say, both from ADR-0068:

- **Root ownership (§2).** A registered label must descend from a core taxonomy
  root (`io.redis`) or open a new root equal to the plugin's Composer *vendor*
  name (`acme/steins-plugin` may register `acme.*`). Anything else is rejected
  and reported by name on stderr, while the rest of the plugin loads. An
  explicitly listed plugin is exempt — the owner's listing is the vouching act.
- **Lane and taint (§1).** A plugin's function coloring enters the **declared**
  lane and does *not* discharge the call's exhaustiveness taint. Nothing checks a
  plugin's assertion the way `effect.liskov-widened` checks an interface
  envelope, so a plugin-covered call reads "declared `acme.cache`, and possibly
  more". Plugin facts therefore never reach the proven lane, and never
  manufacture a finding. Builtin catalog rows and project bodies are consulted
  first; a plugin recolors neither.

What the manifest channel does **not** do yet: boot the sidecar to ask the real
framework (ADR-0039's `plugin` JSON-RPC method is still the stub returning
`widen`), supply synthetic declarations, color *methods* rather than plain
functions, register value-provenance labels, or cache anything by environment
fingerprint. The framework packs of ADR-0044/0045 sit downstream of the parts
that are still missing.

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
functions and methods**, and **language constructs**. Nothing else creates an
effect; user code only propagates. An uncatalogued function or method widens to
*unknown effect*, which taints exhaustiveness but produces no finding.

A declared envelope is a third kind of source, and a different kind: it does not
create a *proven* effect but a **declared bound** (ADR-0067) — see "The declared
lane" below. The two-origin closure is a statement about the proven lane, and
stays exactly true of it.

Recognized origins in a body:

| Origin | Effect |
| --- | --- |
| a statically-named function call | the catalog's labels for it, or a propagation edge to a project function |
| `echo` / `print` / `<?=` | `output` |
| `exit` / `die` | `exit` (ADR-0019 rule 4 — `Pure` forbids exit) |
| a resolvable method call (`$this->`, `self::`, `parent::`, `Foo::`, `new Foo()->`) | a method→method propagation edge into the project class, else the catalog's labels for the *builtin* class's method |
| a higher-order builtin with a resolvable callback | the callback's effects, per the [invocation shape](closures.md) |
| a `$fn()` call resolved to a known callback | the callback's effects |
| a method call on a receiver whose declared type is a project **interface** | the interface method's envelope labels, in the **declared** lane (ADR-0067) |
| anything else dynamic | **no** effect, but exhaustiveness is tainted |

The `$this->`/`self::` edges are drawn under a **final/private guard**: a
non-final public method may be overridden, so its resolved body is not
authoritative. `parent::` and `Foo::` are exact.

The **builtin-class** leg (issue #67) is consulted only when the named class is
one the project does not define, and only when the receiver's name resolves to a
*global* FQN — a project `PDO` shadows the catalog outright, and an unimported
`PDO` inside `namespace App;` is `App\PDO`, somebody else's class. Everything
else about a method call is unchanged: a variable receiver (`$pdo->query()`) is
not a named class at all, so it contributes no effect and taints exhaustiveness,
exactly as before. Receiver *types* do not flow yet, which is why
`(new PDO(…))->prepare(…)->execute()` colors only the `prepare` half.

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

`steins annotate --format json` (issue #65) is the machine-readable exit for
the same two facts: a `functions` array, one entry per analyzed
function/method, each carrying `name`, `line`, the sorted proven `effects`
labels, and the `exhaustive` bit as distinct fields rather than the margin's
flattened `…?` string. A catalogued-pure function reports `effects: []` and
`exhaustive: true`; an uncatalogued/dynamic-tainted one reports
`exhaustive: false`. The default `annotate` output is still the text margin —
`--format json` is opt-in, mirroring `check --format json`'s posture
(ADR-0053/0054) without sharing that command's document shape.

`steins effect-diff [--baseline <path>] [--set-baseline] [--format text|json]
<paths...>` (issue #69) puts the same summaries to work as a **review** surface.
`--set-baseline` captures every analyzed function's proven labels, declared
bounds and exhaustiveness bit into `steins-effects-baseline.json`; a later run
reports the delta, one line per changed function — `a.php Checkout::confirm: +
io.net.http` when an occurrence appeared. It is a sidecar of its own: it shares
nothing with the diagnostic baseline (ADR-0022) but that file's path handling,
suppresses nothing, and always exits 0, because an effect delta is information,
not a verdict. Three rules keep it honest. Only functions present on **both**
sides are compared, so a rename or a deletion is counted in a one-line footer
and never reported as a lost effect. A proven label that vanished is claimed
confidently only when the *current* summary is exhaustive — "and possibly more"
cannot prove an absence, so the candidate is otherwise reported hedged. And a
label that left the declared lane to appear in the proven one is a
**materialization** (ADR-0067 §2.6), one event rather than a removal plus an
addition; exhaustiveness transitions are their own category, never folded into a
label event. `--format json` carries the same events as an `events` array of
`file`, `symbol`, `category`, `label`, beside the footer counts.

`effect.liskov-widened` applies the same proven-only rule across an override:
an implementation whose proven effects exceed the envelope declared on the class
or interface method it overrides is a finding. Implementations may be purer,
never less pure ([closures.md](closures.md)).

## The declared lane

Dependency injection breaks the call graph on purpose: a controller holding a
repository *interface* has no resolvable callee, so the proven lane can only
shrug and taint. The declaration is still there, though, and it is a bound. So a
summary carries **two** lanes (ADR-0067):

```text
function f(Repo $r) { return $r->find(1); }     //=> effects: {≤io.db}
```

`Repo::find()` declares `#[\Steins\Effect('io.db')]`, so the call *cannot* do
more than `io.db` whichever implementation is injected. That label joins the
caller's **declared** lane — rendered with a `≤` prefix inside the same braces
(`effects: {output, ≤io.db}`) and never conflated with a proven one. Declared
labels travel call edges exactly as proven ones do, monotone to the same
fixpoint.

The rules that make this safe:

- A declared label **never** enters the proven set, so `effect.envelope-exceeded`
  and `effect.liskov-widened` cannot see it. A body whose only `io.db` is a
  declared one satisfies `#[\Steins\Pure]` — the bound describes code Steins did
  not analyze, and a contract about someone else's body is not a violation in
  this one.
- The bound **discharges its own call site's** exhaustiveness taint, and only
  that one: another unresolved call in the same body still marks the summary
  `…?`. Discharge is a property of the *checked* stratum, not of the lane: an
  interface envelope is held to by `effect.liskov-widened`, so importing it
  bounds the call. A **plugin** coloring (ADR-0068 §1) shares the lane and not
  the discharge — nothing checks a third party's assertion, so a plugin-covered
  call keeps its taint and reads "declared this, and possibly more".
- A method with no envelope imports nothing and taints exactly as before. Absence
  of a contract is not a contract.
- At rendering time a declared label already subsumed by a proven label of the
  same summary is dropped: the proven lane says strictly more.

The receiver forms are deliberately narrow — a parameter (`$r->find()`) or a
`$this` property read (`$this->repo->find()`) whose declared type names one
project interface, and which the body **never writes**. Any write to the name,
anywhere in the body, disqualifies it: the binding is no longer provably the one
the declaration typed.

`annotate --format json` carries the lane as a `declared` array beside `effects`
and `exhaustive`, normalized the same way and never flattened into the proven
one — a consumer that only wants occurrences can keep reading `effects` and
ignore the new field.

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

- **The sidecar half of the plugin channel** (ADR-0012 / ADR-0039). The manifest
  half ships (see [The registry](#the-registry)); what does not is booting the
  project's own autoload to ask the real framework, synthetic declarations,
  pattern subscriptions, method colorings, and response caching by environment
  fingerprint.
- **Envelope carrier interfaces as an ecosystem story** — the mechanism works in
  both directions now (an interface method's envelope binds implementations, and
  a call through an interface-typed receiver imports it as a declared bound), but
  no PSR knowledge ships to make DI-mediated effects checkable out of the box
  (ADR-0045).
- **The envelope as an effect source, past the first receiver forms.** What ships
  is the declared lane above (ADR-0067): a call whose receiver
  is a never-written parameter or `$this` property read, declared as one project
  **interface** whose method carries an envelope, imports that envelope and
  discharges its own call site's taint. What does not: **non-final classes as
  carriers** — ADR-0067 §2 admits them, this slice reads interfaces only, because
  a class has a body and its envelope and its inferred effects are two facts the
  proven lane already reasons about — and **broader receiver-type recovery**, so
  a receiver the flow environment could type but the structural scan cannot (a
  local assigned from a factory, an array element, a chained call result) still
  only taints.
- **The full effect catalog.** What exists is the frequency-seeded starter set
  above; ADR-0014's php-src stub sourcing is not built.
- **A computed purity property.** Folding permission stays an allowlist.
- **`fopen` mode-string discrimination** — it stays at the parent `io.fs` label.
- **Effect-precondition-driven transforms** (loop→map requires purity) — the
  transform engine exists, but no transform consumes effects yet (ADR-0034).
