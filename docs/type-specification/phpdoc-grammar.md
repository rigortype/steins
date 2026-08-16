# The PHPDoc Grammar

**Status: implemented** (`steins-phpdoc`; ADR-0029).

## Why the grammar is normative

A PHPDoc type becomes an authoritative envelope (ADR-0001). A **misparsed
docblock is a wrong contract**, and a wrong contract is a false-positive vector.
So the grammar is not "a reasonable subset of what PHPStan accepts" — it is a
faithful, hand-written port of `phpstan/phpdoc-parser`, and compatibility is
enforced mechanically rather than by inspection.

## The oracle

`harness/phpdoc-oracle` (`cargo xtask phpdoc-oracle`) runs the same inputs
through the *real* `phpstan/phpdoc-parser` and diffs the result against this
crate. The comparison key is the **canonical form**: `Display` on the AST
reproduces phpdoc-parser's node `__toString()`, so a structural divergence shows
up as a string mismatch.

Two consequences worth knowing when reading canonical output:

```php
parse_type("int|string")->ty->to_string()   // "(int | string)" — always parenthesized
parse_type("Foo $bar the description")      // parses "Foo", at_end = false
```

A `@param` type followed by a variable name and prose is a *partial* parse —
`at_end` records it — which is exactly how the tag scanner isolates the type.

## The type grammar

The whole reference grammar is modelled. `TypeKind` covers:

| Form | Example |
| --- | --- |
| Identifier | `int`, `\App\User`, `self`, `true` |
| `$this` | `$this` |
| Nullable | `?T` |
| Union / Intersection | `A\|B`, `A&B` |
| Array shorthand | `T[]` |
| Generic | `array<K, V>`, `list<T>`, `int<0, max>`, `Collection<T>` |
| Callable | `callable(int, string=): bool`, `\Closure<T>(T): R` |
| Array shape | `array{a: int, b?: string}`, `list{int, string}`, `non-empty-array{…}` |
| Object shape | `object{a: int}` |
| Offset access | `T[K]` |
| Const type | `'x'`, `123`, `Foo::BAR`, `Foo::*` |
| Conditional | `(T is U ? A : B)`, `($param is U ? A : B)` |

Generic arguments carry declared variance (invariant, covariant `covariant T`,
contravariant, and the `*` bivariant wildcard).

`TypeKind::Unsupported` exists for forward compatibility with upstream
additions; the **parser never produces it** — it models the whole reference
grammar. Two rewrites over a parsed type produce it: the template shadow (issue
#5), where a bare identifier shadowed by an in-scope `@template` name becomes
one, and the `template-type` resolution below, where a node nothing decides
becomes one. Both lower `Opaque` — the same silence a template floor already
gets.

### `unset` — the possibly-undefined pseudo-type

`unset` is **vocabulary**, not a class name (ADR-0087, issue #395). The parser
has no keyword table, so the spelling arrives as a plain
`TypeKind::Identifier("unset")`; the identifier table lowers it to
`ContractTy::Unset` rather than letting the class catch-all invent a class named
`unset` in the current namespace.

`/** @var \DateTime|unset $x */` is the Blade-view and included-partial idiom:
`$x` holds a `\DateTime`, or the variable is not defined at all. The member
speaks about the *binding*, so it carries **no value** — it is not `null`, not
`void`, not `never`, not `mixed` — and `\DateTime|unset` accepts exactly what
`\DateTime` accepts. Because `unset` is a reserved PHP language construct
(`class unset {}` does not parse), it is **non-shadowable**: a same-named class
in scope can never win, unlike the pseudo-types that are also legal class names
(`integer`, `number`, `closure`, …). The word round-trips — lowering
`\DateTime|unset` and spelling it back yields `\DateTime|unset`.

A bare `@var unset $x` parses, lowers to an empty value-arm list, and is treated
as "no envelope" (ADR-0029) — nothing is seeded and nothing is reported. **No
diagnostic reads the state yet**: the semantics (`isset`/`empty`/`??`/`??=`/
assignment discharge, the guard never redundant, and the
`phpdoc.maybe-undefined` id) are issue #396, and positions other than an inline
top-level `@var` are issue #397. PHPStan resolves the spelling as a class — see
[divergence-registry.md](divergence-registry.md), core entry 15.

### `template-type<Subject, Owner, 'TName'>`

PHPStan's "read a `@template` argument out of an object type" utility is
**recognized vocabulary** (issue #360), is **resolved wherever declarations
decide it** (issue #361), and where the subject is a class-level template of the
receiver, is **read off that receiver's generics carry at the call site** (issue
#362) — as is a subject naming a **function- or method-level `@template`**, off
the carry of the *argument* that bound it (issue #363). The declaration half is
one rewrite over the parsed type, run where
envelopes are built and before anything is lowered, so a resolved node is judged,
dumped and stored exactly as the type it names is — there is no second evaluator
and no `ContractTy` variant (ADR-0030's one-relation discipline).

Its arguments are read in three different ways, which is the thing to keep
straight:

- **Argument 1, the subject**, is an ordinary type position: it reports
  `untyped.generics` like any other (`template-type<Box, Box, 'T'>` names `Box`
  bare where a `Box<T>` belongs).
- **Argument 2, the owner**, is a class **reference**. The class whose
  `@template` list is being indexed is named without type arguments by design,
  so it is exempt from `untyped.generics` — writing `Box<T>` there would be the
  wrong docblock.
- **Argument 3** is a quoted template name, not a type at all.

Three subject shapes resolve:

- **The owner, parameterized here.** `template-type<Box<int>, Box, 'T'>` is
  `int`, indexed positionally by the owner's own `@template` order — so
  `template-type<Pair<int, string>, Pair, 'V'>` is the `string`. A nested
  argument carries through whole (`template-type<Box<list<int>>, Box, 'T'>` is
  `list<int>`).
- **A one-level inheritance edge to the owner.** `IntBox` declaring
  `@extends Box<int>` makes `template-type<IntBox, Box, 'T'>` an `int`.
- **The owner parameterized by a template.** `template-type<Box<T>, Box, 'T'>`
  under `@template T` is `T` itself — opaque, or the vocabulary bound of
  `@template T of int` (issue #293).

Two more shapes resolve at a **call site** rather than in a declaration, because
that is the only place their answer exists:

- **A class-level template of the receiver's class**, on a `@return`.
  `@return template-type<T, ModelInterface, 'TChild'>` on a `Helper<T>` method is
  read off the carry the *receiver object* holds: `T`'s position in `Helper`'s
  own `@template` list picks what flowed into the constructor, and `'TChild'`
  indexes that value's own `@implements ModelInterface<Child>` edge. So
  `(new Helper(new Model()))->getFirstChildren()` reads as `Child` — the same
  arms, at the same stratum, as if the docblock had said `@return Child`. Two
  lookups, one level each; nothing recurses.
- **A function- or method-level template**, bound from an **argument's** carry
  (issue #363). A declaration's own `@template T` binds where a `@param` spells
  `Owner<…, T, …>` at the top level and the argument carries an edge owned by
  exactly that `Owner`; `@param T $p` binds to the argument's proven value.
  `@return T` then reads that binding, and
  `@return template-type<T, Box, 'T'>` reads one hop past it — the binding's own
  carry, indexed by `'T'` on `Box`. So under `@template T`, `@param Box<T> $box`
  and either `@return` spelling, `unwrap(new Box(1))` reads `1`. Since #361
  rewrites `template-type<Box<T>, Box, 'T'>` to `T` on the declared side, that
  spelling and the bare `T` are the same read.

The binding rule is deliberately narrow and states its own refusals: top-level
positions only (not `list<Box<T>>`, `Box<T>|null`, `?T`, `array<T>`,
`\Closure():T` or any nested position), the owner's own `@template` list indexed
positionally with no hierarchy walk, and **all-or-nothing per name, over every
occurrence** — `T` binds only when every place the `@param` envelopes mention it
is a binding position the read performed, and all of them agree. So two
`@param Box<T>` parameters handed `Box<1>` and `Box<'s'>` decline, and so does a
readable `@param T $t2` standing beside an unreadable `@param \Closure():T $t1`:
an occurrence the rule cannot read **contests** the name rather than being
skipped, because answering from the legible position alone would be narrower
than the declaration supports. A named or spread argument list, a by-ref or
variadic parameter, and an argument past the declared arity decline the whole
call. A **bounded** template never binds, because the shadow already replaced it
with its bound: under `@template T of int`, `@return T` reads `int`. No
unification, no fixpoint, no flow back into the argument — this is a read of
what tier 1 already calls "whatever flowed in", not the call-site solver
ADR-0032 refuses.

Where the callee's body proves a value, that **summary wins** and the read is
only the floor beneath it: `function id(int $x): int { return 2; }` under
`@template T @param T $x @return T` reads `2` at `id(1)`, not `1`.

Everything else keeps the `Opaque` floor and never manufactures a `No`: an
unknown owner, a template name the owner does not declare, an arity
disagreement between the spelled arguments and the owner's list, an unrelated
subject, a non-class subject, a union or intersection subject, and a subject
that reaches the owner only through a generic intermediate (one level, no
walk — ADR-0032's amendment). Any arity but three is not this utility and floors
the same way, silently, where PHPStan yields an error type. The call-site read
floors the same way wherever there is nothing to read — a `$this`, static or
non-exact receiver, a receiver whose value carry an earlier method call swept, a
declared `@param Helper<Model> $h` receiver (which seeds no object today), an
argument that proves no value or carries no matching edge, and every spelling
outside the binding rule above. See [not-implemented.md](not-implemented.md) for
the row and [divergence-registry.md](divergence-registry.md) for the registered
differences from PHPStan.

Variance markers do not gate a projection. `@template-covariant T` states what
the author expects of *substitution*, which is why acceptance is gated on it
(issue #294); reading an argument out by position asks nothing about
substitution.

### Accepted syntactically, erased semantically

`__benevolent<A|B>` parses and is recorded as a union with a `benevolent`
provenance flag. The flag is **not** read by any semantic rule: a benevolent
union is a plain union to Steins. Benevolent unions compensate for worst-case
false positives that a proof layer does not emit in the first place, so the
compensation has nothing to compensate for (ADR-0030 registry entry 3,
ADR-0042). See [divergence-registry.md](divergence-registry.md).

### Failure modes

- A construct the parser cannot accept yields a `ParseError`.
- A construct deliberately kept opaque yields `TypeKind::Unsupported` (today
  the template-shadow rewrite and the declined `template-type` above).

Callers treat **both** as "no envelope" — silence, always the safe side. The
parser never panics on input.

Beyond the parser, lowering to [`ContractTy`](contract-types.md) is total, with
`Opaque` (always `Maybe`) as the floor for conditionals, offset access, const
fetches, `$this`/`self`/`static`, and templates.

## The tag surface

`scan_docblock` extracts typed tags with positions. The recognized set is small
and closed:

| Tag | Read as |
| --- | --- |
| `@param` | parameter envelope (contract layer) |
| `@return` | return envelope |
| `@var` | property/variable envelope |
| `@throws` | throw envelope ([throws.md](throws.md)) |
| `@phpstan-assert` / `@psalm-assert` | unconditional assertion |
| `@phpstan-assert-if-true` / `-if-false` | conditional assertion, guard position only |

Precedence prefixes `@phpstan-` and `@psalm-` are accepted on all of these.
Assertion tags exist **only** in prefixed form — PHPStan has no bare `@assert`
tag, so an unprefixed `@assert` is not a tag at all. The negated form
(`@phpstan-assert !T $x`) is recorded on the tag.

`@phpstan-impure` / `@phpstan-pure` and the class-level
`@phpstan-all-methods-pure` / `@phpstan-all-methods-impure` pair are read too,
as **interop effect envelopes** rather than types — a separate grammar with
its own alias rules, detailed in
[phpdoc-effects-interop.md](phpdoc-effects-interop.md).

**Tool-specific tags beyond `@phpstan-*` / `@psalm-*` are refused by design**
(ADR-0029). There is no `@steins-` type tag: Steins' own annotations are PHP
attributes ([effects.md](effects.md)), not docblock tags — with the single
exception of the `@steins-ignore` suppression comment
([diagnostic-policy.md](diagnostic-policy.md)).

**Not read today:** `@template` and friends are scanned for names
(`scan_template_names` — the plain, `@phpstan-`/`@psalm-`-prefixed, and
`-covariant`/`-contravariant` variants), and those names **shadow the class
universe** in the declaring docblock's own types (issue #5; see
[contract-types.md](contract-types.md)) — but no call-site template solver
exists (ADR-0032), and template scope transfer (ADR-0051) is designed and
unimplemented. `@method`,
`@property`, `@mixin`, `@phpstan-type` aliases, and `@phpstan-import-type`
are not recognized. See [not-implemented.md](not-implemented.md).

The two **conditional-purity** tags *are* read (ADR-0063 §2 decision 2), in the
spelling merged upstream in `phpstan/phpdoc-parser` 2.3.3 — bare or
`@phpstan-`-prefixed, no `@psalm-` alias, a required `$parameter` followed by an
optional description:

```php
/** @pure-unless-callable-is-impure $callback */
/** @pure-unless-parameter-passed $matches */
```

The first makes a call's effect the join of the callables bound at the flagged
positions; the second makes the flagged parameter a userland by-ref
out-parameter row, colored by its argument exactly as a catalog row is.

The unconditional `@phpstan-pure` / `@phpstan-impure` flags stayed unread for
a long time: they were refused as the metadata lie upstream rejected twice,
and Steins spells unconditional purity `#[\Steins\Pure]`, where inference can
check it. ADR-0082 supersedes that refusal with a design that keeps its
spirit rather than repealing it — the parameterized forms (`@phpstan-impure
<labels>`, and bare `@phpstan-pure` as the `{mutate.local}` envelope) are
read as *checkable* unchecked bounds, the **interop envelope** row above.
A bare `@phpstan-impure` is the one spelling still unread, and by the same
logic as before: it is ⊤ (every effect possible), which is exactly what the
absence of the tag already means, so reading it would add no information.
See [phpdoc-effects-interop.md](phpdoc-effects-interop.md).

## Annotation restraint

A design stance rather than a mechanism, stated here because it explains what
the grammar is *for*: complex structural types — `array{foo: int}` shapes,
scattered `@var` — should not be hand-written. Steins infers them from values,
and its transforms steer code toward runtime-enforced **native** declarations
instead (`steins transform phpdoc-to-native`). PHPDoc is where a project records
what native syntax cannot express, not where it re-states what the analyzer
already knows.
