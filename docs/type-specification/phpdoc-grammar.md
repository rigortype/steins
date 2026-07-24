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

`TypeKind::Unsupported` exists for forward compatibility with upstream additions
and is **currently unused** — the parser models the whole reference grammar.

### Accepted syntactically, erased semantically

`__benevolent<A|B>` parses and is recorded as a union with a `benevolent`
provenance flag. The flag is **not** read by any semantic rule: a benevolent
union is a plain union to Steins. Benevolent unions compensate for worst-case
false positives that a proof layer does not emit in the first place, so the
compensation has nothing to compensate for (ADR-0030 registry entry 3,
ADR-0042). See [divergence-registry.md](divergence-registry.md).

### Failure modes

- A construct the parser cannot accept yields a `ParseError`.
- A construct deliberately kept opaque yields `TypeKind::Unsupported`.

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

**Tool-specific tags beyond `@phpstan-*` / `@psalm-*` are refused by design**
(ADR-0029). There is no `@steins-` type tag: Steins' own annotations are PHP
attributes ([effects.md](effects.md)), not docblock tags — with the single
exception of the `@steins-ignore` suppression comment
([diagnostic-policy.md](diagnostic-policy.md)).

**Not read today:** `@template` and friends are scanned for names
(`scan_template_names`) but no call-site template solver exists (ADR-0032), and
template scope transfer (ADR-0051) is designed and unimplemented. `@method`,
`@property`, `@mixin`, `@phpstan-type` aliases, `@phpstan-import-type`,
`@phpstan-pure`, and `@phpstan-impure` are not recognized. See
[not-implemented.md](not-implemented.md).

## Annotation restraint

A design stance rather than a mechanism, stated here because it explains what
the grammar is *for*: complex structural types — `array{foo: int}` shapes,
scattered `@var` — should not be hand-written. Steins infers them from values,
and its transforms steer code toward runtime-enforced **native** declarations
instead (`steins transform phpdoc-to-native`). PHPDoc is where a project records
what native syntax cannot express, not where it re-states what the analyzer
already knows.
