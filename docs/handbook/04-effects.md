# Effects

Types are the first dimension Steins infers. **Effects** are the
second: what an expression *does* beyond computing a value —
throw, output, touch the filesystem, hit the network, read the
clock. Steins infers and propagates effects the same way it
infers types, and this is the design feature that sets it apart
from every other PHP checker.

By the end of this chapter you will be able to:

- read an effect **label** and understand prefix subsumption;
- declare an **envelope** with `#[\Steins\Pure]` /
  `#[\Steins\Effect]` and know what it promises;
- read the effect and throw margins that `steins annotate`
  prints;
- tell the three effect findings apart, and know which surface
  each lives on.

## The second dimension

An effect is what a function does that a return value cannot
capture. Steins cares about it for a concrete reason: the
project's end goal is to **structurally separate effectful code
from testable code**. A function you can prove is pure is a
function you can test without mocking the world; a function whose
effects are declared is one whose surprises are on the label.

Where PHPStan collects a flat list of a body's "impure spots" to
check `@phpstan-pure`, Steins *types* the side effects — the
difference between noting that something is impure and saying
exactly *how*.

> **If you know PHPStan:** this grew out of the `ImpurePoint`
> mechanism. PHPStan gathers evidence of impurity; Steins turns
> impurity into an inferred, propagated, hierarchical type with
> declared upper bounds. The full comparison is in
> [phpstan-divergences.md](../phpstan-divergences.md#impurepoint-vs-effect-system).

## Labels

An effect's identity is a **hierarchical dot-path string**, and
checking is by **prefix subsumption**, segment-aware:

```text
subsumes("io", "io.fs.read")   = true
subsumes("io", "iota")         = false     // segments, not raw string prefix
```

A declared `io` therefore admits an inferred `io.fs.read`: the
general permission covers the specific act. The known-label set
is closed today — the taxonomy roots plus whatever the builtin
catalog can color a function with:

```text
exit   ffi
global.read   global.write
io   io.db   io.fs   io.fs.read   io.fs.write   io.ipc
     io.net   io.net.http   io.process   io.signal
mutate
nondet   nondet.random   nondet.time
output   output.header
failure.environment   failure.input   failure.resource
```

You will not memorise these. You will read one off an `annotate`
margin, decide whether it belongs on the function, and put it in
the envelope if it does.

## Envelopes

An **effect envelope** is a declared upper bound on what a
function may do. Its mere presence opts the declaration into
always-on checking; with no envelope, nothing about effects is
checked. Envelopes are spelled as native PHP attributes, not
docblock tags:

```php
#[\Steins\Pure]                          // the empty set — the tightest bound
function slug(string $s): string {
    return strtolower($s);
}

#[\Steins\Effect('io.fs')]               // an upper bound: filesystem, nothing wider
function readIt(string $path): string {
    return file_get_contents($path);
}
```

`#[\Steins\Pure]` is the empty envelope: this function computes a
value and does nothing else. `#[\Steins\Effect('io.fs')]` says
"filesystem effects are allowed here" — and by prefix subsumption
a read like `file_get_contents` falls under `io.fs`, so it raises
no finding. Both the fully-qualified spelling and a `use`-imported
`#[Pure]` / `#[Effect(...)]` are recognised.

## Reading effects with `steins annotate`

You do not have to guess what Steins inferred. `steins annotate`
reprints a file with an effect (and throw) margin on each
function. Given this file:

```php
<?php
declare(strict_types=1);

#[\Steins\Pure]
function slug(string $s): string {
    return strtolower($s);
}

#[\Steins\Pure]
function leaky(string $s): string {
    echo $s;
    return $s;
}

function noisy(): void {
    echo "hi";
    $r = rand();
}

#[\Steins\Effect('io.netw')]
function typo(): void {}
```

`steins annotate` prints:

```text
<?php
declare(strict_types=1);

#[\Steins\Pure]
function slug(string $s): string {  //=> effects: {}
    return strtolower($s);
}

#[\Steins\Pure]
function leaky(string $s): string { //=> effects: {}
    echo $s;                        //=> ✗ effect.envelope-exceeded
    return $s;
}

function noisy(): void {            //=> effects: {nondet.random, output}
    echo "hi";
    $r = rand();
}

#[\Steins\Effect('io.netw')]        //=> ✗ effect.unknown-label
function typo(): void {}            //=> effects: {}
```

Read it top to bottom. `slug` is genuinely pure (`{}`). `leaky`
declared `Pure`, so its margin shows the empty set it is *allowed*
— and the `echo` that breaks that promise is pinpointed inline
with `✗ effect.envelope-exceeded`. `noisy` has **no** envelope, so
there is nothing to measure it against and the margin simply lists
its full inferred set: `echo` contributes `output`, `rand()`
contributes `nondet.random`. `typo` misspelled its label, flagged
on the attribute line. The margin is the fastest way to see both
dimensions at once — and note the `throws:` clause appears only
when a function has a proven throw set to report.

## The exhaustiveness marker

Effects propagate to a fixpoint over the call graph, joined with
an **exhaustiveness bit** that a dynamic or unresolved call
taints. When Steins cannot see everything a function reaches, the
margin says so with a `…?`:

```text
#[\Steins\Effect('nondet.random')]
function maybeMore(callable $cb): void {   //=> effects: {…?}; throws: {…?}
    rand();
    $cb();          // an opaque callback — could do anything
}
```

`maybeMore` declares `nondet.random` and does exactly that with
`rand()` — but `$cb()` is an opaque callback that could do
anything. Steins cannot bound what lies beyond the envelope, so
the residual is `{…?}`: "and possibly more." The `throws: {…?}`
says the same about exceptions. The `…?` never becomes a finding
on its own — an over-approximation of the unknown is not a proof
of anything. It is honesty in the margin: Steins is telling you
where its view ends.

## The three findings

Effects produce three findings, and they live on different
surfaces — which is the whole point of the layered design.

**`effect.envelope-exceeded`** (contract layer) fires when a
function's *proven* effects exceed its declared envelope. It is a
contract-layer finding — the program still *works*; it just does
more than it promised — so a bare `steins check` does **not**
print it:

```text
$ steins check leaky.php
(nothing about the envelope — contract layer is off by default)
```

Reach it with a named profile:

```text
$ steins check --profile contracts leaky.php
leaky.php:11:5: error[effect.envelope-exceeded]: echo has effect output, but leaky() is declared #[\Steins\Pure]
```

**`effect.liskov-widened`** (contract layer) applies the same
proven-only rule across an override: an implementation whose
proven effects exceed the envelope on the method it overrides is
a finding. Implementations may be *purer* than their abstraction,
never less pure.

```php
interface Store {
    #[\Steins\Pure]
    public function get(string $k): string;
}
class LoudStore implements Store {
    #[\Steins\Effect('output')]
    public function get(string $k): string { echo $k; return $k; }
}
```

```text
$ steins check --profile contracts store.php
store.php:9:21: error[effect.liskov-widened]: LoudStore::get() has proven effect output but Store::get() (its abstraction) is declared #[\Steins\Pure] — Liskov effect widening
```

**`effect.unknown-label`** (mechanics layer) is the odd one out —
and it fires on the **default** surface. A misspelled label is
not a contract claim about your program; it is rot in the
apparatus, a declaration that can no longer mean what it says. So
it is red on sight, with a suggestion:

```text
$ steins check typo.php
typo.php:20:3: error[effect.unknown-label]: unknown effect label 'io.netw' in #[\Steins\Effect] on typo() — did you mean 'io.net'?
```

Mechanics findings cannot be disabled — the anti-rot channel is
the one thing you do not get to turn off. Typo safety is Steins'
job, not yours.

> **If you know PHPStan:** the layer split is the mechanism. The
> two contract findings sit behind `--profile contracts` exactly
> as `throw.undeclared` does — declared-debt findings that are
> true and abundant in real code, so never dumped on you by a
> first run. The one mechanics finding is always on because a
> typo'd envelope silently checks the wrong thing.

## Where effects come from

Effects have exactly two origins: **catalogued builtin/extension
functions** and **language constructs** (`echo`/`print` →
`output`, `exit`/`die` → `exit`). Nothing else *creates* an
effect — user code only propagates what it calls. An uncatalogued
function widens to *unknown effect*, which taints the `…?`
exhaustiveness bit but never produces a finding. That seeding
order — color what you know, widen the rest — is the only one
compatible with the zero-false-positive bar.

This also connects back to Chapter 1's sidecar: Steins may fold a
value by executing it in the sidecar **only when its effect set
is empty**. Purity is what licenses constant folding, so the two
dimensions are not independent — the effect system is part of why
a folded value is trustworthy.

## What is not implemented yet

Recorded honestly:

- **The plugin channel** that would open the label registry to
  ecosystem effects (`io.redis`, `email.send`) and library effect
  signatures. The registry is *designed* to be open; it is closed
  in practice because nothing can open it yet — so those labels
  are correctly `unknown` today.
- **PSR-mediated envelopes as an out-of-the-box story.** The
  mechanism works (an interface method's envelope binds its
  implementations, as the Liskov example shows), but no framework
  knowledge ships to make dependency-injected effects checkable
  without your own annotations.
- **Effect-driven transforms.** The `transform` engine exists,
  but no transform consumes effects yet.

## Where to go from here

You now have the two dimensions Steins infers — values and
effects — and the trust model that keeps both honest. The
[planned chapters](README.md#planned-chapters) build outward from
here: declared-type contracts and the two acceptance relations
(5), the throw accounting that mirrors this effect chapter (6),
and the profiles and baseline that decide which of these surfaces
you see (7). Until those land, the
[type specification](../type-specification/README.md) is the
binding source for every one of them.
