# Why Effects?

> [!NOTE]
> This document narrates why the effect system exists and where the idea
> came from. As of 2026-08-25, the core model it describes — labels,
> envelopes, propagation, interface Liskov widening, and the PHPStan
> interop bridge below — is implemented. A handful of illustrations stay
> forward-looking on purpose (marked "could" or "a future rule"):
> project-specific semantic labels beyond the manifest plugin channel, and
> connection-provenance effects. For the precise current state, see
> [What is not implemented](type-specification/not-implemented.md).

Steins started with a question: can a PHP analyzer tell us not only what value
a method returns, but what calling that method can do to the world?

That question matters in large PHP applications. A controller method may contain
no SQL, HTTP client, clock, or output operation of its own while reaching all of
them through repositories, framework abstractions, and dependency injection.
Changing one call can add a database dependency or a network request without
changing the method's parameter or return types. Tests may still pass. The
operational shape of the method has changed.

Steins treats that shape as an inferred second dimension: **effects**.

## The two dimensions

A type describes the values an expression accepts and produces:

```php
function findUser(UserId $id): User
```

The signature does not say whether `findUser` reads a database, consults the
clock, sends an HTTP request, or writes output. Steins assigns hierarchical
labels to those actions:

```text
io.db
io.net.http
io.output.buffer
nondet.time
```

It then propagates those labels through the call graph. If a controller calls a
repository that reaches a catalogued database primitive, the database effect
reaches the controller's effect summary. The controller does not need to mention
the database directly.

An effect summary is therefore a static description of a method's observable
contacts with the outside world. Comparing summaries before and after a change
can reveal a new dependency even when the ordinary type signatures are
unchanged.

## Where the idea came from

[Flix](https://flix.dev/) demonstrated how much leverage a language can get from
making effects part of its type system. Its effect polymorphism captures a
particularly important relationship: the effect of a higher-order function such
as `map` depends on the effect of the function passed to it. A pure callback
keeps `map` pure; an effectful callback contributes its effects to the call.
Flix also supports sub-effecting, effect exclusion, purity reflection, and
algebraic effect handlers.

The immediate PHP problem came from studying
[PHPStan's pure/impure model and the purity of `map`-like functions](https://github.com/zonuexe/phpstan-notes/blob/master/generated-report/20260703-effect-system-design.md).
Two cases exposed the limits of a flat pure/impure flag:

- `array_map` is pure or effectful according to its callback;
- a by-reference output parameter such as `preg_match(..., $matches)` may mutate
  a local binding without creating a caller-observable effect.

PHPStan already carries useful pieces of this information, but a boolean
`hasSideEffects` flag cannot state the full relationship. Declaring
`array_map` pure is a lie for an effectful callback. Declaring it impure is a lie
for a pure callback.

Steins began as a place to explore the stronger model without PHPStan's
compatibility constraints, and to ask whether that model could make large PHP
changes easier to trust.

## Steins' model

Steins is an external static analyzer. It finds primitive effects at language
constructs and catalogued builtin or extension calls, then propagates them to a
fixpoint over the resolved project call graph.

Effects use hierarchical dot-path labels. A broad label subsumes its
descendants:

```text
io
└── io.net
    └── io.net.http
```

A declaration may place an upper bound, called an **effect envelope**, on a
function:

```php
#[\Steins\Pure]
function slug(string $value): string
{
    return strtolower(trim($value));
}

#[\Steins\Effect('io.db')]
function findUser(UserId $id): User
{
    // ...
}
```

The declaration does not cause or handle an effect. It asks Steins to check that
the proven implementation effects stay within the stated bound.

Visible callbacks do not need an explicit effect variable. Call-site value
propagation lets Steins inspect the callback and join its effects into the
higher-order call. Conditional-purity contracts cover some cases where a
callable remains opaque. This recovers a useful part of effect polymorphism
without adding effect-row syntax to PHP.

Interface methods can carry envelopes too. A call through an interface can
therefore retain effect information after dependency injection breaks the
concrete call graph. Implementations must not widen the interface's envelope.
This is the effect form of Liskov substitutability.

## Back to PHPStan: interop envelopes

Steins began by studying PHPStan's purity model from outside; the interop
envelope closes part of that loop. Ondřej Mirtes, PHPStan's author, suggested
in passing (2026-08-09) that the existing `@phpstan-impure` tag could take a
parameter naming the kind of impurity — `@phpstan-impure io`. Steins reads
that parameterized form, and its `@phpstan-pure` /
`@phpstan-all-methods-pure` / `@phpstan-all-methods-impure` siblings, as an
**interop envelope**: the same envelope concept as `#[\Steins\Effect]`,
spelled in a docblock instead of an attribute, one trust stratum below it. A
call reached only through such a tag contributes to the caller's declared
effect lane without ever discharging its own exhaustiveness taint, and the
declaring function is contract-checked against its own tag exactly as an
attribute is.

The bridge runs in both directions. Steins reads what an upstream project
already carries, and `steins transform effects-envelope` writes the same
tags from a project's own proven effects — a docblock a current PHPStan
install parses and quietly ignores, today, with no behavior change on either
side. See
[phpdoc-effects-interop.md](type-specification/phpdoc-effects-interop.md)
for the grammar and semantics, written to be pasteable into an upstream
discussion, and [ADR-0082](adr/0082-interop-envelopes.md) for the design
record.

## One bit, many questions

The interop work surfaced a second, subtler limit of the flat flag. PHPStan's
`hasSideEffects` bit is consulted by machinery with genuinely different
questions: the no-effect statement rules ask *"did this discarded call change
the world?"*; the
[remembered-returned-values machinery](https://phpstan.org/blog/remembering-and-forgetting-returned-values)
asks *"may two identical calls be treated as one — and does this call
invalidate other memory?"*; and the must-use question (PHP 8.5's
`#[\NoDiscard]`) asks *"is discarding the result a bug?"*. One bit answers
all of them at once, so it can satisfy none of them precisely. `rand()` must
be marked side-effectful so its results are not collapsed — which then stops
the no-effect rule from flagging a bare `rand();`, a statement that provably
changes nothing. `file_get_contents()` wants the bit clear so discarded reads
are caught — until someone posts an HTTP request through it
([phpstan#8440](https://github.com/phpstan/phpstan/issues/8440), patched by
flipping the bit per parameter,
[phpstan-src#2037](https://github.com/phpstan/phpstan-src/pull/2037)).
Resource openers were flipped by hand
([phpstan-src#698](https://github.com/phpstan/phpstan-src/pull/698)), and
must-use arrived as its own request
([phpstan#12738](https://github.com/phpstan/phpstan/issues/12738)) because it
never was an effect question.

Labels decompose the bit. Read-shaped effects — `global.read`, `nondet.*`,
`io.fs.read` — change nothing a caller can observe, so a call whose proven
effects stay inside that set and whose throw set is empty is a dead statement
when its result is unused: derivable, no annotation. `nondet.random` forbids
collapsing two calls into one without making a bare `rand();` meaningful.
`clearstatcache()` is `global.write` — exactly what stat-derived memory
depends on. And `#[\NoDiscard]` shrinks to the one quadrant that genuinely
needs a declaration: effectful calls whose result is still the point
(`fopen()`). The engine-facing halves are recorded as informative sections in
[the interop spec](type-specification/phpdoc-effects-interop.md), and the
argument-dependent narrowing that resolves the `file_get_contents` case is
implemented: a literal `'/config'` is `io.fs.read`; an unprovable target
stays `io`.

## Transport facts and semantic facts

Low-level analysis can identify a transport action such as an HTTP request:

```text
io.net.http
```

A package or project knows more about what that request means. A plugin could
add semantic labels to a SendGrid call:

```text
io.net.http
sendgrid.mail.send
email.send
```

These labels coexist. `io.net.http` records the mechanism;
`sendgrid.mail.send` records the provider operation; `email.send` records the
application meaning. Policies can then ask different questions without forcing
one taxonomy to encode every dimension.

The same distinction applies to database connections. Two objects may both have
the PHP type `PDO` while referring to different database families or connection
roles. A future provenance-aware rule could attach a label to the value returned
by a connection factory:

```text
acme.db.catalog.connection.master
acme.db.catalog.connection.slave
```

When a PDO operation is invoked on that receiver, Steins could turn the
receiver's provenance into an effect:

```text
acme.db.catalog.master
```

The connection route and the SQL operation are separate facts. A `SELECT` sent
through a master connection would carry both the master-route effect and a
query-operation fact. This separation can show which database a controller
reaches, whether a read-only path depends on a master connection, and whether a
change introduced a new database route.

## What Steins is not

Steins does not add algebraic effects to PHP.

In languages with algebraic effects, a program can perform an effect, transfer
control to a handler, and resume the captured computation. That mechanism can
implement user-defined control flow, exceptions, generators, async execution,
backtracking, or runtime dependency substitution.

Steins cannot provide:

- `perform`, `handle`, or `resume` constructs;
- continuation capture or resumable effect handlers;
- user-defined control-flow mechanisms;
- runtime replacement, redirection, recording, or replay of effects;
- handler-based dependency injection;
- compiler optimization based on effect types.

It observes existing PHP behavior; it does not mediate that behavior at runtime.
Its labels classify calls rather than reifying operations that a handler can
interpret.

Steins also has no explicit effect rows or general effect variables. Its
callback propagation recovers effect-polymorphic behavior where the project or a
declaration supplies enough information, but it is not a general row-polymorphic
type system.

Finally, a clean report is not automatically a proof of purity. Dynamic dispatch,
unresolved calls, an incomplete builtin catalog, or a propagation budget can
leave the summary non-exhaustive. Steins records that state as "known effects,
and possibly more" instead of treating the unknown as pure. Envelope diagnostics
use proven effects only.

## The intended value

Steins aims to make the effect footprint of existing PHP code observable:

- which controllers reach a database, network, clock, output, or global state;
- which concrete infrastructure families sit behind those broad effects;
- whether a refactor added or removed an effect;
- whether an implementation exceeds the envelope declared by its abstraction;
- where a pure computational core can be separated from effectful orchestration.

This is narrower than a language-level algebraic effect system. It is also
deployable against PHP code that already exists. The project bets that accurate
effect observation, honest unknowns, and project-specific semantic labels can
make large changes easier to review and safer to ship.

For the implemented semantics, see [Effects](type-specification/effects.md).
For current gaps, see
[What is not implemented](type-specification/not-implemented.md).
