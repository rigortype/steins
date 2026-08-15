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
grammar. The one producer today is the template-shadow rewrite (issue #5): a
bare identifier shadowed by an in-scope `@template` name is rewritten to
`Unsupported`, which lowers `Opaque` — the same silence a template floor
already gets.

### Recognized vocabulary, no resolution yet

`template-type<Subject, Owner, 'TName'>` — PHPStan's "read a `@template`
argument out of an object type" utility — is **recognized as vocabulary** and
floors to `Opaque` (issue #360). Recognition alone is what it buys today: the
spelling is no longer read as a class named `template-type`, and its second
argument is a class-**reference** position, so the owner named there without
type arguments is not an `untyped.generics` finding — that is exactly how
PHPStan reads the utility, and writing `Box<T>` there would be the wrong
docblock. Argument 1 is an ordinary type position and still reports; argument 3
is a quoted template name and is not a type at all. Any arity but three floors
the same way, silently, where PHPStan yields an error type — see
[divergence-registry.md](divergence-registry.md). Resolving the utility to the
template argument it names is issue #361;
[not-implemented.md](not-implemented.md) carries the row.

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
  only the template-shadow rewrite above produces it).

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
