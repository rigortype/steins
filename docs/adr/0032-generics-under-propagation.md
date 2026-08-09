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

## Amendment (2026-08-09): the carry survives a variable binding, on the heap, and is swept by receiver calls (issue #295)

Tier 3 reads class-level generics off the one place a value proves them, a
`new Class(args)` site. Until now that reading died at the next semicolon:
`resolve_cval` minted the carry inside the `New` arm, and a later
`ArgValue::Var` re-derived the object from `Store::class_of`, which knows a
class and nothing else. `$box = new MutableBox(1); takesStringBox($box);`
therefore judged only the class half — the exact gap this amendment closes.

**The carry is an extension of the exact-class fact, so it lives where that
fact lives.** `HeapObj` already holds the one allocation-keyed record of
what an object *is* (ADR-0036); `targs` joins `class`/`class_exact` there.
The two rejected homes are rejected for stated reasons, not for taste. The
**value lattice** is object-free by ADR-0035/0043 §4 — `cval_as_val`
returns `None` for `CVal::Object`, and putting a class parameterization in
`Fact` would be the first object in it. A **`ContractTy` variant** would
oblige every existing `Class` consumer to answer what a parameterized class
means, for zero movement: the declared side already works, because
`accepts_class_generic` reads the phpdoc AST directly and never goes
through `lower_generic`. A `ContractTy` variant becomes necessary only when
a declared parameterized class must survive into `Store::contract` arms for
the S6 declared-receiver lane, which is a different issue.

**A carry can go stale, and the sweep is what makes it sound.** A class
parameterization proven from constructor values is a fact about the values
the object holds, so a call that may write those values invalidates it —
`@phpstan-self-out self<U>` is the annotation that says so out loud, and
Steins models no such re-parameterization. Carrying `int` past
`$box->replace($next)` would not be a miss, it would be a **false
positive** at the next `takesStringBox($box)`, convicting correct code. So
a **receiver method call sweeps its receiver's value carries**, in the same
step and for the same reason `sweep_nonreadonly` sweeps that receiver's
mutable props. Argument passing deliberately does not: a callee that
mutates a parameter it received is a hole this slice leaves open, stated,
because closing it there would sweep the very carry the case under test
needs one line later, and because the honest closure is `@param-out`
modelling rather than a blanket erase.

**What survives a sweep is what mutation cannot reach.** An
inheritance-edge carry (`@extends Box<int>`, the previous amendment)
records what the author *declared* about the class, not what flowed into
one object; no method call can change it, so it is sweep-immune exactly as
a `readonly` prop is. Value carries (`CArg::Val`) are swept; type carries
(`CArg::Ty`) survive. That is the whole rule, and it needs no new
machinery: the two provenances the previous amendment introduced already
distinguish the mutable fact from the declared one.

### ADR-0048 obligations

**§2 (replayable).** The carry is a pure function of the `new` trace — the
same arguments, the same class docblock, the same result — and the sweep is
a function of the statement sequence the scope walk already replays. No
site consults anything outside the trace it is replayed from.

**§3 (entry-state contribution), the load-bearing one.** `targs` is a new
fact kind on `HeapObj`, so its value at scope entry is defined here, when
it lands, and not retroactively:

- A **`$this` seed** contributes **empty**. `$this` is a lower bound on the
  runtime class (audit G1), the enclosing method's docblock states no
  parameterization of the instance, and a class's own `@template` binds to
  constructor values this scope never saw.
- **Any non-exact object** contributes **empty**, for the same reason the
  No-side consumers gate on `class_exact`: without exactness there is no
  single class whose template list the arguments could align to.
- A **parameter seeded from a declared `@param MutableBox<string> $box`**
  contributes its **declared** arguments — owner-keyed to the class that
  declares the templates, as `CArg::Ty`, resolved in the *declaring* file's
  namespace scope. Declared-authoritative is ADR-0037's trust order: at an
  entry point the docblock is the strongest fact available, and it is
  precisely the fact the callee is entitled to assume. Being `CArg::Ty`,
  such a seed is sweep-immune, which is right: a declaration does not stop
  being true because the body called a method.

  Today this clause has no site to fire at — a parameter receives a
  `Store::contract` arm lane and **no `HeapObj` at all**, so there is no
  object for `targs` to hang on, and the clause is a contract on the
  parameter-seed when one lands rather than code shipping in this slice.
  It is stated now because §3 requires the contribution defined at the
  moment the fact kind is introduced, not at the moment a consumer appears.

**§4 (no global ordering).** Nothing in the carry or the sweep depends on
analysis order across files or scopes. The branch join intersects: a carry
survives a merge only when every joined branch carried it identically,
which is order-independent because it is an intersection.

**Status: PENDING ratification.** Designed autonomously under the owner's
standing delegation, recorded ahead of the implementation.
