# The Object and Method World

**Status: implemented** (ADR-0036 object state, ADR-0043 object/method world).

## Objects are not values

An object has no [`Fact`](value-domain.md). It lives in a **store**: a variable
binds to an allocation id, and the id maps to a heap object. Aliasing copies the
reference, so `$b = $a` shares the id and a write through either alias is visible
through both.

```text
Store {
  refs: var  -> AllocId          // which object a variable refers to
  heap: AllocId -> HeapObj       // the object itself
}
```

A `HeapObj` carries:

| Field | Meaning |
| --- | --- |
| `class` | the class FQN (lowercase-normalized) |
| `class_exact` | whether `class` is the *exact* runtime class or only a lower bound |
| `props` | per-property value-domain facts, each with its [trust stratum](trust-stratification.md) |
| `readonly` | properties declared `readonly` — sweep-immune once established |
| `ro_written` | readonly properties provably written on this path |
| `escaped` | whether the object has left local control |

## Exactness

`class_exact` is the object-world half of the trust discipline.

- **Exact** — allocation-proven: `new Foo`, an enum case, a clone of an exact
  object. `class` *is* the runtime class. Two `$this` shapes are also exact:
  a `$this` whose enclosing class is `final` or an enum (no subclass can
  exist), and a `$this` seeded by a binding descent that proved the exact
  receiver at the call site.
- **Lower bound** — any other `$this` seed. The runtime object may be any
  descendant that inherited the method being analyzed.

The rule this exists for: a **`No`-side conclusion requires exactness.**
"`is_a(class, T) = No`, therefore this object is not a `T`" is sound only for an
exact class; with a lower bound the actual instance may be a descendant that *is*
a `T`. `Yes`-side conclusions hold for a lower bound too, since every descendant
is a `T`.

Fresh heap objects default to `class_exact = false` — the safe side.
Construction sites that can prove exactness set it explicitly.

## Escape and sweeping

The precision payoff of the store model. An object **escapes** when it is passed
to a call, returned, stored into an array or property, or captured by a closure.

- An **escaped** object has its non-`readonly` properties swept by an unknown or
  overridable call — the callee may have mutated them.
- A purely **local** object's properties survive such calls untouched.
- `readonly` properties are **sweep-immune** once established, and the class is
  never swept.

`readonly.reassigned` (proof layer) fires on a second *proven* write to a
readonly property on one path — a guaranteed runtime `Error`.

## Property facts

`$o->p = <value>` and `$x = $o->p` are modeled for a **static property name on a
receiver variable** (`$this` included). A dynamic property name (`$o->$p`) or a
chained/complex lvalue (`$a->b->c`, `Foo::$s`) stays a `Barrier` — it is not
modeled, and it erases known values.

Property facts propagate their stratum in both directions (write and read), so
an `Asserted` value cannot launder into a proof-layer premise through the heap.

Two ids consume them: `type.property-mismatch` (proof — a proven value assigned
to a native-typed property that provably raises a `TypeError` under the assigning
file's strict mode) and `phpdoc.property-mismatch` (contract — the same against a
`@var` envelope, definite `No` only, and never double-reporting where the native
check already fired).

## The trinary is-a oracle

`is_a` returns three values, and the third is the important one (ADR-0043 §3):

| Verdict | Condition |
| --- | --- |
| `Yes` | a supertype path exists — membership proven |
| `No` | the hierarchy is **completely enumerated** and the target is absent — non-membership proven under closure |
| `Unknown` | the hierarchy is incomplete — no verdict, the FP-safe silence |

`No` requires closure over the whole chain. A single link Steins cannot resolve
— an external class, an unloaded extension, a conditional declaration — makes the
answer `Unknown`, never `No`.

The chain walks project classes first (from the whole-project symbol index), then
falls through to the **builtin hierarchy table**: 352 generated rows of
`(lowercased name, direct supertypes)`, mined from php-src at a pinned commit and
cross-checked against **PHP 8.5.8**. A name absent from both is an unknown
external → `Unknown`.

Because the table is pinned to a PHP *minor* line (`PINNED_PHP = (8, 5)`), a
catalog-backed verdict used for contract-arm deletion is demoted to `Unknown`
when the project's own PHP reports a different minor (ADR-0052 amendment A11).
Builtin type edges are stable within a minor line, so only `(major, minor)` is
pinned — the patch component is irrelevant.

**Builtin enums are deliberately omitted** from the generated table: the mining
data for their implicit interfaces and backing is incomplete, and an incomplete
row would produce a wrong `No`.

## Method resolution and dispatch

Method calls resolve through the project inheritance chain (`extends`,
`implements`, and — where the chain leaves the project — the builtin table).
`$this->`, `self::`, `parent::`, and `static::` resolve against the declaring
class of the enclosing method scope.

Sound dispatch means a call is only *checked* when the target is uniquely
determined. Anything ambiguous is silent:

- An FQN with **two definitions** in the project index is never resolved — PHP
  would fatal on a real double-definition, and Steins cannot know which body
  runs.
- A **builtin-shadowing** userland definition is never resolved.
- A receiver whose class is not proven is not dispatched.

Native and PHPDoc **object acceptance** is judged over the same oracle: a class
argument satisfies a declared class/interface parameter when `is_a` says `Yes`,
fails on `No`, and is silent on `Unknown`. A native intersection type (`A&B`)
lowers to a conjunctive member (`InstanceInter`): every conjunct must answer
`Yes`, any `No` fails, any `Unknown` is silence — Kleene `and` over the same
oracle.

**Return-position `static`/`self`/`parent`** is lowered by the minimum-bound
lemma (ADR-0043 amendment, landed): every late-bound class `T` of a `: static`
method of class `C` satisfies `is_a(T, C) = Yes`, so an exact returned class
`V` with `is_a(V, C) = No` fails *every* possible `T` — an unconditional
runtime `TypeError`, reported under `type.return-mismatch` with no worst-case
reasoning. The bound is the enclosing class for `self`/`static` and the
resolved parent for `parent`; `return $this`, `new self()` under `: static` in
an open class, and sibling-subclass returns stay silent (see the divergence
registry, conformance entry 3).

## Enums and `::class`

Enum cases are objects with an exact class (the enum), so they participate in the
is-a oracle and in acceptance like any allocation-proven object. Backed enums
carry their backing scalar type. `Foo::class` resolves to the class-string of the
named class in the reference's namespace/`use` context.

## Absence proofs over the class world

`call.undefined-method` (proof layer) is the flagship of the finding-breadth
family and the strictest consumer of everything above. It fires only under
**complete closure**: a proven-exact receiver, a fully-enumerated hierarchy that
defines no such method, no `__call`/`__callStatic` anywhere in the chain, no
trait obstacle, and — through the sidecar — no builtin/extension homonym that
could mean the textual class is dead code shadowed by a loaded one. Any doubt is
silence. See [dynamism.md](dynamism.md).

`phpdoc.undefined-method` is its contract-layer twin over *declared* receivers,
under an additional per-arm **descendant closure** requirement: a declared type is
satisfied by subclasses, so absence on the declared class is not absence on the
value.

## Not implemented

- **Generic type-argument carry through a variable binding.** A heap object
  records no type arguments: `$x = new Box('x'); f($x);` judges only the class
  half. What *has* landed (ADR-0032 stage 1, issue #10) is scoped to the
  direct-`new` argument position: `f(new Box('x'))` infers the class-level
  type-argument values at the `new` site and judges them against the declared
  generic, with an empty carry — silence on the argument half — as the honest
  floor everywhere else.
- **Extension classes from unloaded PHP extensions** are `Unknown`-silent. The
  sidecar's `reflect()` is the designed answer and is not wired into class
  resolution.
- **Traits** are recognized as an obstacle to absence proofs, not modeled as a
  method source.
- **`__get`/`__set`** property magic is not modeled (and would, like `__call`,
  be an obstacle rather than a feature).
