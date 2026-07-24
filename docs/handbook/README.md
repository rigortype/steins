# The Steins Handbook

A walkthrough of what Steins actually proves about your PHP,
written for PHP programmers — no prior static-analysis
background assumed. Read top to bottom for the first pass; come
back to individual chapters for reference once you know what
you are looking for.

## The guarantee, in three sentences

Steins reports only what it can **prove** breaks at runtime: if
a finding prints on the default surface, your program provably
throws or fatals on a live path — there are no "probably" and
no "just in case." Everything it cannot prove is **silent** —
not guessed, not worst-cased; silence means "Steins could not
decide," never "Steins decided it is fine." Everything stricter
than a proven runtime break — declared-contract debt, style,
policy — is off by default and reached only through named,
opt-in surfaces you choose.

That is the whole posture: a quiet default you can trust
literally, and louder surfaces you turn on deliberately.

## Who this is for

You write PHP for a living, you have shipped a
`TypeError: Argument #1 must be of type int` to production more
than once, and you want to know:

- What does `steins check` actually look at?
- Why did it flag this call — or, far more often, why did it
  stay quiet about the one you expected?
- When does silence mean "safe" and when does it mean "I don't
  know"? (In Steins those are the same word on purpose, and
  this handbook teaches you to tell them apart.)

The handbook answers those questions. It does **not** replace
the [normative type specification](../type-specification/README.md),
which is the binding source when this handbook disagrees, nor
the [operational guide](../guide/quickstart.md) that lists
every flag and config key.

> **If you know PHPStan or Psalm:** you already have the reflexes
> for most of this — refinement types, `@phpstan-assert`,
> baselines. What is genuinely different is the *stance*: no
> numeric levels, no benevolent unions, no `treatPhpDocTypesAsCertain`
> toggle, and a proof layer held to **zero false positives** on a
> ~99k-file corpus. Asides like this one throughout the handbook
> map the vocabulary you know onto what Steins does differently.

## Table of contents

1. [**Getting started**](01-getting-started.md) — installing
   from source, your first `steins check .`, reading the default
   surface, the PHP sidecar and the `--no-php` sound subset, and
   exit codes.
2. [**The type system**](02-the-type-system.md) — the core
   chapter. What Steins infers: values before types, the
   four-layer value domain, declared types as envelopes judged
   accept / reject / silent, what "proven" means (the `TypeError`
   findings), intersections, generics, and what the checker
   deliberately does not claim.
3. [**Narrowing and trust**](03-narrowing-and-trust.md) — how
   guards (`instanceof`, `=== null`, `is_int`, `&&`/`||`,
   ternary) sharpen facts along a branch, why dead branches stay
   silent, and the trust stratum that stops a lying
   `@phpstan-assert` from forging a proof.
4. [**Effects**](04-effects.md) — the second inferred dimension.
   What an effect is, `#[\Steins\Pure]` and `#[\Steins\Effect]`
   envelopes, the findings they enable, and reading effects in
   the `steins annotate` margin.

### Planned chapters

Written after tonight's release; listed here so you know the map:

5. **Declared types and contracts** — the two acceptance
   relations (runtime vs contract), PHPDoc `@param`/`@return`/`@var`
   as unnormalized arm lists, why `"5"` satisfies `int` at a
   coercive call boundary but never satisfies a `@param int`
   contract. Grounded in
   [contract-types.md](../type-specification/contract-types.md).
6. **Throws and exceptions** — the checked / unchecked split,
   `@throws` envelopes, damming through `try`/`catch`, and the
   `throws-direct` stage. Grounded in
   [throws.md](../type-specification/throws.md).
7. **Profiles, baselines, and suppression** — the named stages
   (`default` → `throws-direct` → `contracts`), the JSONL
   baseline ratchet, inline `@steins-ignore`, and why there is no
   message-regex suppression. Grounded in
   [profiles-and-baseline](../guide/profiles-and-baseline.md) and
   [diagnostic-policy.md](../type-specification/diagnostic-policy.md).
8. **Understanding findings** — the finding-id catalogue by
   layer (`type.*`, `call.*`, `offset.*`, `phpdoc.*`, `throw.*`,
   `effect.*`), how to read a message, and the difference between
   a `proof`, `contract`, and `mechanics` finding.
9. **Transforms** — `steins transform phpdoc-to-native` and
   `phpdoc-honesty`: what the value-propagation engine can
   rewrite safely, and the vouch valve for dynamic-code sites.
10. **Coming from PHPStan / Psalm** — a concept-by-concept map:
    levels vs layers, `ignoreErrors` vs the id registry, version
    emulation vs asking the real PHP, and the intentional
    divergences. A narrative companion to
    [phpstan-divergences.md](../phpstan-divergences.md).

## How to read this handbook

Each chapter is short on theory and long on examples. Every PHP
snippet is real code you can drop into a file and run, and every
Steins output shown was produced by the v0.1.0 binary — trimmed
where noted, never invented.

Most examples use `\PHPStan\dumpType($x);` to pin what Steins
inferred at a point. It is an **introspection call**, not a
runtime function — PHP has no such function, so a committed
`dumpType()` is a guaranteed fatal, and Steins emits it at
fail level to remind you to delete it before you ship. Steins
prints the inferred type on that line:

```php
$s = "hello";
\PHPStan\dumpType($s);   // dumped type: 'hello'
```

The trailing `// dumped type: …` comment in the examples is the
exact tail of the line Steins prints
(`error[debug.type]: dumped type: 'hello'`). Chapter 2 explains
the display grammar in full.

When a chapter references a more formal document, the link takes
you out of the handbook into the binding spec corpus or the ADRs:

- [`docs/type-specification/`](../type-specification/README.md)
  — the normative specification of what the analysis *means*.
- [`docs/guide/`](../guide/quickstart.md) — the operational guide
  (install, flags, profiles, baseline).
- [`docs/adr/`](../adr/) — architecture decision records, the
  binding source on any conflict.

## Non-goals

The handbook is meant to be readable cover-to-cover in an
afternoon. To keep it short:

- It does **not** teach PHP. Classes, `declare(strict_types=1)`,
  attributes, namespaces, PHPDoc basics — all assumed.
- It does **not** enumerate every finding id or edge case. Those
  live in the spec corpus.
- It does **not** cover analyzer internals (the trace IR, the
  sidecar protocol, crate topology). Those live in
  [`docs/internal-spec/`](../internal-spec/README.md).

If a topic comes up that the handbook does not explain, the
relevant spec document is one click away.
