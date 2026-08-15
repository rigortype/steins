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
mutable props.

**Argument passing sweeps too, and the direction of failure is why.**
Every other thing deferred around this carry fails toward *silence*: an
empty carry, an unmatched owner, an arity disagreement, a non-invariant
position — all of them answer `Maybe` and say nothing. A callee that
mutates the object it was handed fails the other way. `$box = new
MutableBox(1); mutate($box); takesStringBox($box);`, where `mutate()`
calls `$b->replace('s')` internally, is **correct code**, and a retained
carry reports `phpdoc.param-mismatch` on it. That it lands in the contract
layer rather than the proof layer decides which gate absorbs it, not
whether it is acceptable: a finding the reader has to argue with is the one
output this analyzer promises not to produce. The fp-gate's silence over
100k files says the shape is rare, not impossible. So the carry does not
survive an argument pass by default.

**What it survives on is reachability, not non-mutation — because
non-mutation is not knowable here.** The natural gate would be "this callee
does not mutate that argument", and no such judgment exists in the tree, in
any form. ADR-0055's mutation family (`mutate.arg`/`.self`/`.instance`/
`.static`) is taxonomy without inference — `EffectOrigin` has no
property-write arm at all, so `$b->value = 's'` colors *nothing*, and the
only `mutate*` carriers are `mutate.local` and a coarse `mutate` from
builtin by-ref out-parameters (ADR-0063 §2.3). The purity judgment cannot
stand in for it either, and leaning on it would be worse than leaning on
nothing: `PurityOracle` answers `provably_impure`, whose negative means
"not proven impure", and a body that only writes properties has an *empty*
proven finding set — so a purity gate would keep the carry across precisely
the call that invalidates it. That is ADR-0055's own opening complaint, that
`Pure` does not today mean what it says, which disqualifies the declared
envelope for the same reason.

A different question *is* decidable with what exists: not whether the
callee mutates the object, but whether it can **refer to it at all**. PHP
locals are lexical, so a parameter a body never spells cannot be read,
written, captured, passed onward, or used as a receiver by it; and every
construct that reaches a binding non-lexically (`$$v`, `extract`/`compact`,
`eval`, `include`, `global`, a by-ref `use`) is on the ADR-0001 give-up
list that sets `Scope::poisoned`, which the gate refuses. The test is
therefore a token scan of the callee's **body text**, not of the linear
trace: the trace drops nested sub-expressions to `ArgValue::Other` and
unrecognized statements to `Barrier`, so `helper($b)` inside `$x =
strlen($b->p) + helper($b);` is invisible to it — and a gate that misses one
use is a gate that keeps a stale carry, which is the failure this rule
exists to prevent. Every uncertainty sweeps: an unresolved, dynamic,
builtin, method or static callee; a named or spread argument list (position
no longer maps to a parameter); a by-ref or variadic position; an argument
past the declared arity; a poisoned callee body. Unknown is never proof of
non-mutation.

This is deliberately narrow: in practice it admits the callee that ignores
the parameter — which is exactly what the conformance case's
`takesIntBox(MutableBox $box): void {}` does, so line 53 keeps its carry
while the mutating shape drops it. **The follow-up is the wider gate**, a
real per-parameter non-mutation judgment, and its precondition is named:
ADR-0055 Part II's inference. Once a property write colors
`mutate.self`/`mutate.instance` and those labels reach the fixpoint,
"nothing this callee mutates is reachable from its parameters" becomes a
propagation question, and the lexical test retires into being its cheap
fast path.

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
a function of the statement sequence the scope walk already replays. The
argument-pass gate adds the project index and the callee's own source text
to that input set, both of which are replay inputs already (the index keys
every descent; the text is the file being analyzed). It asks the engine
nothing — no sidecar, no fold, no effect fixpoint — so no per-name state
and no global ordering can enter the verdict.

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

## Amendment (2026-08-15): `template-type` reads the carry as a type expression (issue #362)

The two amendments above built the carry and made it survive a binding.
Both were written for one consumer, acceptance: a declared `Box<int>` meets
a carried argument and the answer is a `Tri`. PHPStan's
`template-type<Subject, Owner, 'TName'>` asks the same state a different
question — *what* is carried there, not whether something inhabits it — and
that question has no answer in the text above. This amendment gives it one.

**The utility is a reader, not a solver.** `getTemplateType(owner, name)`
on the PHPStan side is a lookup: find the ancestor parameterization owned
by `owner`, index the position `name` holds in that owner's own
`@template` list, return what sits there. Steins already keeps exactly that
shape — [`GenericCarry`] is an owner FQN plus one argument per declared
template — so the reader is a projection out of tier-3 state and introduces
no inference. Tier 1 is untouched: nothing about a call-site template
solver changes, and the divergence ADR-0030 registers stands. What the
declared side of the utility resolves from declarations alone (issue #361)
this reads from the receiver's carry at the call site, which is the only
place the answer exists when the subject is a class-level template.

**A value carry holding an object contributes that object's own carries,
one hop.** The discussion-9053 shape needs two lookups, not one:
`template-type<T, ModelInterface, 'TChild'>` on `Helper<T>` first reads `T`
off the receiver's `Helper` carry, and what sits there is a *value* — the
`Model` object that flowed into the constructor. The second lookup is
`ModelInterface`'s `TChild` on that object, and the carries to index are
the object's own (`@implements ModelInterface<Child>`, minted by
`infer_generic_carry` when the value was proven). A `CArg::Ty` naming a
declared class resolves the same way, through the index rather than through
a heap object. **Each hop is one level**, the same rule the inheritance-edge
amendment already states and for the same reason: the moment following a
second edge would mean substituting through a generic intermediate, the
walk is wrong rather than incomplete. The subject asks for exactly two
lookups and gets exactly two; nothing recurses.

**The read is Asserted, and never anything stronger.** What the reader
produces is a *docblock's claim about a return*, resolved against a carry
that happens to be proven — the proof is about what flowed into a
constructor, not about what this method returns. So the projected type
enters the call site through the same refinement a hand-written
`@return Child` goes through, at the same stratum, and a reader cannot tell
which spelling produced it. That is the whole soundness argument: a proven
carry may not launder a declared return into the proof layer, and routing
the result through the ordinary declared-return refinement is what
guarantees it does not.

**Silence everywhere the lookup does not land.** An empty carry, a carry
swept by an earlier receiver call (the previous amendment), a non-exact
receiver, a `$this` receiver, no carry edge owned by the declaring class, a
template name the owner does not declare, an arity that disagrees with that
list, a hop whose object carries no edge owned by the owner, a carried
argument the contract lane has no type for — every one of them declines and
leaves today's floor (the class-level shadow, and `Opaque`). PHPStan
substitutes an unresolved template's declared bound; Steins declines class
bounds on tier 1's own terms (issue #293), so that path is opaque here too.

**The sweep is visible through this reader, and that is correct.** A value
carry does not survive a receiver method call, so `$helper->reset();
$helper->getFirstChildren()` reads nothing where `$helper->getFirstChildren()`
alone reads `Child` — and a *second* read on the same receiver declines,
because the first call swept it. The alternative is carrying a stale
parameterization past a method that may have rewritten it, which the
previous amendment already rejected as a false-positive source. A declared
edge carry is sweep-immune and reads identically before and after.

### ADR-0048 obligations

**§2 (replayable).** The reader is a pure function of the same inputs the
carry is: the trace that proved the object, the project index (the owner's
`@template` list, the hop class's inheritance edges), and the docblock being
read. It asks the engine nothing and holds no state between call sites, so
re-walking a scope alone reproduces it exactly.

**§3 (entry-state contribution).** No new fact kind is introduced — the
reader consumes `HeapObj::targs`, whose entry-state contribution the
previous amendment already fixed. Its own call-site contributions are
stated here because they are the same kind of commitment: a `$this`
receiver contributes **empty** (a lower bound on the runtime class, and the
enclosing method saw no constructor), a non-exact receiver contributes
**empty** (no single class whose template list the arguments align to), a
static call contributes **empty** (no receiver), and a direct
`new Helper(…)->m()` receiver contributes **empty** in this slice — the
allocation has no heap object yet at the point the target resolves, and
minting a carry out of the `new` arguments here would be a second
implementation of `infer_generic_carry` rather than a reading of it.

**§4 (no global ordering).** The reader depends on statement order within
one scope — which is what the sweep *is* — and on nothing across scopes or
files. Statement order inside a scope is the walk's own semantics, not an
iteration order of a whole-project pass.

**Status: PENDING ratification.** Designed autonomously under the owner's
standing delegation, recorded ahead of the implementation.

## Amendment (2026-08-15): a function-level `@template` binds from an argument's carry, and the read is a floor under the body summary (issue #363)

Tier 1 says templates are transparent where propagation reaches: `T` *is*
whatever flowed in. That sentence is about a **body** — the descent binds
the parameter to the actual value and the return follows. It says nothing
about the caller's own view of `f(...)` when the descent yields nothing,
and that view is what the conformance case asks for: `unwrap(new Box(1))`
declared `@template T`, `@param Box<T> $box`, `@return T`, over a body
whose value the summary cannot prove. This amendment says what the caller
reads there.

**It is a carry read, not the solver tier 1 refuses.** The refusal in tier
1 is of *constraint solving beside propagation* — collecting occurrences,
unifying them, propagating a substitution back through the signature — and
the refusal's reason is dual-inference disagreement, two engines answering
the same question differently. What lands here does none of that. It
performs one **projection**: for a parameter spelled `Owner<…, T, …>` at
the top level, ask the argument's tier-3 carry what sits at `T`'s position
in `Owner`'s own `@template` list, and let the callee's `@return T` name
it. That is `getTemplateType` (the previous amendment's reader) applied to
an *argument* instead of a receiver, and the state it reads is exactly
tier 1's own "whatever flowed in", made legible at the call site because
tier 3 recorded it. No constraint is generated, nothing is unified, no
substitution flows back into the argument, and there is no fixpoint: a
single positional read, once, per name.

**The binding rule, exhaustively.** Against the callee's `@param`
envelopes as the declaration's own `@template` shadow leaves them:

- `@param Owner<…, T, …> $p` at the **top level** — `Owner` resolving to a
  class that declares templates, spelled with exactly as many arguments as
  that class declares, position `j` naming `T` — binds `T` to the argument
  carry's `j`-th argument, taken from the edge whose owner **is** `Owner`.
  No hierarchy walk, the same exact-owner rule acceptance uses.
- `@param T $p` at the **top level** binds `T` to the argument's proven
  value, when it has one.
- **Nothing else binds.** Not `list<Box<T>>`, not `Box<T>|null`, not `?T`,
  not `array<T>`, not `\Closure():T`, not any nested position; not a call
  with a named or spread argument list (position no longer maps to a
  parameter, so the whole call declines — the same list issue #295's sweep
  gate declines on), not a by-ref or variadic parameter list, not an
  argument past the declared arity.
- **All-or-nothing per name, over every occurrence and not merely the
  readable ones.** `T` binds only when *every* place the parameter
  envelopes mention it is a binding position the read actually performed,
  and all of those reads agree. Two `@param Box<T>` parameters handed
  `Box<1>` and `Box<'s'>` say the author's `T` is not one thing here. One
  of them handed an object nobody proved says the same. And an occurrence
  the rule cannot read at all — `@param \Closure():T $t1` beside
  `@param T $t2`, a `list<Box<T>>` beside a `Box<T>`, a `@param` on a
  parameter this call supplied no argument for — **contests the name**
  rather than being skipped.

**Why a non-binding occurrence contests rather than abstains.** A
`@template T` witnessed at two parameters is the docblock stating that one
type stands at both. Reading the position Steins understands and ignoring
the other would answer **narrower than the declaration supports**:
`@param \Closure():T $t1, @param T $t2, @return T` handed a `Closure(): A1`
and an `A2` would come back `A2` where the declaration's own truth is
`A1|A2`. Narrower-than-true is the direction every other decision in this
family refuses — a stale carry is swept, a covariant position declines,
an unproven argument contributes nothing — and the `Asserted` grade does
not excuse it, because contract arms feed narrowing and the dump surface,
not only diagnostics. The two alternatives are both worse: joining the
readable position with a modelled reading of the unreadable one *is* the
solver, and joining it with `mixed` is a claim about nothing. So the read
declines, and this ADR says why rather than leaving the narrowing to be
found in a corpus.

**A bounded template does not bind, and the bound is why.** `@template T
of int` has already become `int` in every envelope by the time this runs
(issue #293's vocabulary bounds, applied by the shadow), so there is no
template spelling left to match and `@return T` reads `int`. That is the
correct outcome rather than a limitation: the bound is what the author
promised, tier 1 already reads it as an upper-bound contract, and reading
the carried value *through* a bound would need the read to be checked
against it — an inhabitation question, and a library-author lint of the
kind this ADR keeps thin. A class bound declines the same way it declines
everywhere else in tier 1.

**Precedence — the dual-inference answer, pinned.** Where a body summary
speaks, it wins; the declared read is the floor beneath it, one rung above
`Opaque`. `function id(int $x): int { return 2; }` under `@template T
@param T $x @return T` reads `2` at `id(1)`, not `1`, because the body
proved `2` and the docblock only claimed `T`. Where the summary is absent
— an opaque body, a return the descent cannot carry — the read supplies
what flowed in. This ordering is what makes the two inferences unable to
disagree: they are not two answers to one question but two rungs of one
ladder, and the proven rung is always above the asserted one. Structurally
it is guaranteed rather than maintained, because the read is installed at
exactly the seam the argument-blind declared floor already occupied
(`fn_return_arms`), which the summary already outranks.

**The read is Asserted, and the conformance line stays unenforced.** What
comes out is a docblock's claim about a return, resolved against a carry
that happens to be proven — so it enters through the same refinement a
hand-written `@return Box` goes through and comes out at the same stratum,
exactly as the receiver reader does. The consequence is stated here rather
than left to be discovered: **`takesString(unwrap(new Box(1)))` reports
nothing**, and line 47 of the conformance case
`phpdoc_advanced_phpstan_template_type` is therefore *not enforced* by
this slice. An Asserted argument fact premises no `type.argument-mismatch`
(a `@return int` flowing into `takesString(string)` is silent today for
the same reason), and the body-proven route that would premise one needs
heap properties to cross a binding descent — ADR-0057 T1's heap component,
out of scope here. The dump surface and the contract store are this
slice's consumers, and the divergence registry records the line.

**Methods bind on the same rule.** A method's own `@template` names read
from its arguments identically, at the same seam and with the receiver's
carries untouched — the two readers are orthogonal, one indexing the
receiver's carry for a class-level subject and one indexing an argument's
for a method-level one, and a docblock may carry both without either
seeing the other's names (the shadow stages separate them).

### ADR-0048 obligations

**§2 (replayable).** The read is a pure function of the call's own
arguments as the walk already resolved them (`resolve_cval`, the same
resolution acceptance uses), the project index (the owner's `@template`
list), and the callee's docblock. It asks the engine nothing beyond the
fold memo `resolve_cval` already consults, holds no state between call
sites, and adds no cross-scope coupling: re-walking the scope alone
reproduces it exactly.

**§3 (entry-state contribution).** **Nothing new is seeded into any
scope.** No fact kind is introduced, no parameter seeding changes, and the
callee's entry state is untouched — the read produces *return arms at the
caller*, which is a call-site consumer of `HeapObj::targs` and nothing
else. The contributions it depends on are the previous amendments' and
stand unchanged: an argument with no proven `CVal` contributes no carry, a
swept value carry contributes none, and a declared edge carry (`CArg::Ty`)
contributes and survives sweeps.

**§4 (no global ordering).** The read depends on statement order within
one scope — an argument's carry is swept by the statements before it,
which is the sweep's own semantics — and on nothing across scopes or
files.

**Status: PENDING ratification.** Designed autonomously under the owner's
standing delegation, recorded ahead of the implementation.
