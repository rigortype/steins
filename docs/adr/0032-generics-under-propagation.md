# Generics under call-site propagation: no solver where values flow

`@template` handling has three tiers, inverting PHPStan's solver-centric
model (a registered divergence, ADR-0030):

1. **Where propagation reaches, templates are transparent.** Bound
   arguments carry actual values/exact classes through binding descent;
   `T` *is* whatever flowed in — a value, which holds strictly more
   information than a solved type variable. No call-site template solving
   runs (running a solver beside propagation would create dual-inference
   disagreement, a bug factory). A declared bound (`@template T of Foo`)
   still participates as an upper-bound contract: whatever binds to `T`
   inhabits `T`, and `T` is at most its bound, so a bounded template
   reads as its bound wherever the template itself would be opaque.
   **Implemented for vocabulary bounds only** (`of array`, `of int`,
   `of int|list<int>`, …; issue #293). A **class** bound declines and
   leaves the template opaque — reading class bounds is a follow-up, and
   half-checking one would put a class contract on every templated
   parameter in a codebase at once.
2. **Where propagation cannot reach** (public-API entry points, opaque
   callers), templates act as **contracts**: signature-internal
   consistency and bound checking only, imported from the PHPStan
   denotational core.
3. **Class-level generics (`Collection<int>`) are state, not solving** —
   an extension of the exact-class fact. Stage 1 reads *declared* type
   parameters (phpdoc/`@var`) as envelopes on element operations; growing
   parameters from observed element flow is deferred to the same machinery
   as shape evolution.

Accepted cost, stated honestly: library-author diagnostics about
internally-inconsistent generic signatures (a PHPStan strength) stay thin.
Steins' battlefield is application/monorepo defect-finding, where
propagation is the stronger weapon.

## Amendment (2026-08-09): inheritance edges carry type arguments, keyed to the declaring ancestor (issue #294)

Tier 3 above says class-level generics are *state*, and stage 1 read that
state off the one place a value proves it: a `new Class(args)` site, where
the class's own `@template` list aligns positionally with values that
flowed in. `@extends Box<int>` breaks that alignment in the only way that
matters. The object is an `IntBox`, the templates belong to `Box`, and
`IntBox` declares none of its own — so a positional vector attached to the
object's own class has nothing to align to, and the original tier-3 text
has nothing to say about it. This amendment says it.

**The type arguments are a phpdoc fact, not a syntax one.** `ClassDecl`'s
`parent`/`implements` stay bare `NameRef`s. Nothing in PHP source carries
`<int>`; it is written in the class docblock, next to `@template`, and it
is scanned by the same scanner in the same pass — `@extends`,
`@implements`, their `@template-` spellings, and the `@phpstan-`/`@psalm-`
prefixes ADR-0029 already governs. Widening the syntax tree for a comment
would put a phpdoc obligation on every consumer of `ClassDecl` and buy
nothing: the resolution question (`Box` in whose namespace?) is answered
from the declaring class's file context either way.

**A carry names the class that declares the templates it aligns to.** The
object-level carry stops being one positional vector and becomes a small
set of *edges*, each an owner FQN plus its arguments. A `new Box('x')`
records one edge owned by `Box` — the stage-1 shape, unchanged, now
explicit about what it was always implicitly assuming. A `new IntBox(1)`
records one owned by `Box`, from `@extends Box<int>`. Acceptance of a
declared `Class<A, …>` then asks for the edge whose owner *is* that class,
and answers `Maybe` when no edge matches. That is stricter than the old
positional read, which would have judged any arity-matched vector against
any generic spelling of a class the object happened to satisfy; the
strictness is the point, because with two owners in play the old rule
would silently compare `Box`'s arguments against `Producer`'s parameters.

**Own templates win, and the walk does not recurse.** A class that
declares its own `@template` keeps the stage-1 value carry and never reads
its own inheritance edges: the two would disagree exactly when the edge
mentions a template (`@extends Box<T>`), and a value is the stronger fact
whenever there is one. Edges are read from the instantiated class's own
docblock only, one level. Following `A extends IntBox` up to `Box` is a
*substitution* problem the moment any intermediate class is generic, and a
one-level walk that pretends otherwise would be wrong rather than merely
incomplete.

**An edge argument is a type, and types meet types through `subsumes`.**
Stage 1's carry held proven *values*, judged by the acceptance relation.
An edge holds what the author wrote — `int`, `Dog`, `array<string>` — so
the argument half becomes a type-vs-type question, and it is answered by
the relation that already exists for that (`steins_contract::subsumes`,
ADR-0071 §2.1), not by a second one. Its class arms carry no hierarchy, so
a cross-class position answers `Maybe`; that is the honest floor and it is
where generic *class-half* mismatch reporting still sits (tier 3 stage 1's
deferral, unchanged). Template names inside an edge are neutralized
through issue #5's shadow before lowering, or `@extends Box<T>` would lower
`T` to a class named `T` and manufacture a `No` against every spelling.

**Variance gates the verdict, and it gates it before the comparison.**
`@template-covariant T` and `@template-contravariant T` state that the
declaring author expects substitution in one direction; Steins models
neither direction. Reading such a position invariantly would not be a
miss, it would be a **false positive** — `@implements Producer<Dog>` under
`@param Producer<Animal>` is correct code, and so is a contravariant
consumer standing in for a narrower one. So a non-invariant position
contributes `Maybe` unconditionally, whatever the arguments say. Only an
invariant position may reach a verdict, and only its `No` is claimed. This
is what makes the variance marker load-bearing rather than decorative: it
is scanned (issue #293) precisely so that this slice can decline.

Reconsideration precondition for the variance gate: a modeled variance
relation, which needs the is-a oracle inside the type-vs-type relation —
the same missing piece that keeps the class half deferred. Until then, a
covariant parameter is a capability Steins declines to judge, stated, not a
capability it silently gets wrong.

**Status: PENDING ratification.** Designed autonomously under the owner's
standing delegation; recorded before the implementation so the decision,
not the diff, is what gets reviewed.
