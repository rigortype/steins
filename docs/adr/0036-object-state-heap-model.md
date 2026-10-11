# Object state: store-based heap, escape sets, readonly immunity

Property facts are keyed by **allocation identity, not variable**: env maps
variables to `ObjRef(allocation-id)`; a heap maps ids to
`{ class, props: {name → Fact}, escaped }`. Aliasing (`$b = $a`) shares the
id, so writes through any alias are visible through all — PHP's handle
semantics becomes the design's input rather than its enemy.

- **Escape sets bound the sweeps**: an object escapes when passed to an
  unresolved call, stored into an unknown structure, or returned. Unknown
  calls sweep the props of **escaped ids only** — a purely local object's
  facts survive (the Rigor fact-bucket discipline, narrowed further by
  escape). Stage-1 conservatism: passing anywhere escapes; descent-time
  precision for resolved callees comes later.
- **`$this` is a pre-escaped id**: `$this->p` facts survive own
  private/final method calls (descend), and are swept by any overridable
  call on `$this` — the dispatch guard rules reused.
- **A class-typed parameter is a second pre-escaped, lower-bound id**
  (2026-08-16, ADR-0032's declared-parameter-seed amendment, issue #388):
  `class_exact` is `false` for it exactly as for `$this` — audit G1's rule, that
  membership is not exactness, licenses only Yes-side conclusions — and it holds
  no props, a declaration stating that a parameter is a `Box` and never what
  that `Box` holds. Everything a lower bound cannot license (arity, member
  absence, definite-No acceptance, the exact-receiver dispatch) keys on the
  `class_exact` bit and stays silent, save where `final` forecloses the override
  the refusal was written against.
- **`readonly` is sweep-immune**: constructor-established readonly facts
  persist through escapes and unknown calls — the language guarantees the
  immutability, so the analyzer honors it permanently. Deliberate product
  stance: adopting readonly is rewarded with precision (the modernization
  incentive).
- **New proof-layer checks**: `type.property-mismatch` (native property
  types are runtime-enforced; strict/coercive per the assigning file) and
  `readonly.reassigned` (a proven second write on one path).
  Uninitialized-typed-property reads are an explicit TODO (needs
  initialization analysis). `@var` on properties joins the phpdoc contract
  family as `phpdoc.property-mismatch`.
- Out of stage 1: objects inside arrays, static properties (global-state
  family), descent-time escape precision.

## Amendment (2026-10-11): unresolved aliases — PENDING ratification

Issue #1048. The store follows an object through one route only, the plain
copy `$b = $a`. Every other route hands the handle to a name the store does not
bind, and a write through that name lands on the same object:
`$arr = [$o]; foreach ($arr as $q) { $q->p = 'x'; }` changes `$o->p` (PHP
8.5.11). The walk used to drop such a write (an unbound receiver recorded
nothing), so `$o->p` kept its old fact. The rule now has two halves, and the
table in `crates/steins-infer/src/heap_alias.rs` lists every route and every
write it covers, each witnessed on PHP 8.5.11.

- **Reach.** Every route that lets an object reach a name the store does not
  bind marks it escaped: storing it in an array literal, a ternary or `??` arm,
  a cast, an array union, or any value the lowering spells as `Other` (a
  `match`, an `(object)` cast, a computed key — the statement now names the
  variables inside such a value, `Stmt::unspelled`); handing it to a call inside
  an array or a nested call; iterating over it; a join whose branches bind one
  name to different objects; a loop or an unwalked construct forgetting a name
  that held it.
- **Write.** A write through a name the store does not resolve sweeps the escape
  set, exactly as an unknown call does: a property write or a by-reference pass
  of a property through an unbound receiver, an argument a resolved callee may
  write through (an unbound name with no `Verified` scalar fact, a call's
  result, a general spread). A by-reference pass of a bound object's property
  forgets that property.
- **Constructs the walk does not see whole.** A loop's fall-through is built
  from the write sets before its body is walked, so a sweep the body makes
  (on any path, one that `break`s included) is applied to the fall-through too
  (`WalkCx::heap_sweeps`). An `Opaque` construct sweeps the escape set and the
  objects its names refer to. An expression statement that `yield`s is a
  barrier: the consumer runs before the expression resumes.

The sweep is the escape set, not a class or a property name: an unresolved
receiver has no class the walk can trust, and a write can run `__set` or a hook.
An object no route reached keeps every fact across such a write, so
`$arr = [new O()]` followed by a write through `$q` still leaves `$o->p` known.
The public corpus moves by nothing at `strict` or `default`.

Not covered: a resolved callee that writes an escaped object through a static
property or a global it reaches itself (the callee is trusted with the heap
today), an object converted by `.` whose `__toString` writes itself, and a
`yield` in a condition or a `return`.
