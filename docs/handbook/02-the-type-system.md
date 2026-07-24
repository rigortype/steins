# The type system

This is the most important chapter. Once you have a feel for how
Steins thinks about a value, the rest of the handbook is rules
operating on that model.

By the end you will be able to:

- explain why Steins reasons about **values** before **types**;
- read the four layers Steins uses to describe one value, and
  predict what `dumpType` will print;
- say precisely what the word **proven** rests on in a finding;
- recognise the handful of things the type system deliberately
  refuses to claim.

## Values before types

A vanilla checker asks "what *class* is this object?" Steins
asks a narrower question first: "what *values* can this
expression actually produce here?"

The engine works by carrying **actual argument values from each
call site into the function body**, flow-sensitively. A literal,
a shape, a proven value crosses a function boundary by inference,
not by annotation — so code with no docblock at all is still
analyzed precisely, as long as its values are traceable.

```php
function scale(int $n): int { return $n * 2; }
function run(): void { scale("x"); }   // the "x" is carried into scale()
```

Steins knows `scale` receives `"x"` here because it followed the
call, not because anyone wrote a type. The cost is deliberate:
propagation is bounded (eight frames of interprocedural descent),
and where it cannot reach, Steins **widens to silence** rather
than guessing. A budget cutoff is silence, never a manufactured
finding.

> **If you know PHPStan:** this is the opposite of the modular
> model, where each function is analyzed once against its own
> signature and callers are checked against that signature alone.
> Steins is whole-value-flow: the call site is where precision
> comes from.

## Seeing what Steins infers — `dumpType`

Drop `\PHPStan\dumpType($x);` on any line and Steins prints the
type it inferred for `$x` there. Every example below shows the
call and, in a trailing comment, the exact tail of the line
Steins emits.

```php
$s = "hello";
\PHPStan\dumpType($s);   // dumped type: 'hello'
$b = true;
\PHPStan\dumpType($b);   // dumped type: true
$n = null;
\PHPStan\dumpType($n);   // dumped type: null
```

The display is PHPStan's grammar: a single known string prints as
`'hello'`, booleans and null print literally, class and scalar
base types print as their name. Keep two honest facts in mind as
you read the rest of the chapter:

1. `dumpType` is a **compact view** of a richer internal fact. It
   prints a single known *string* as a literal (`'hello'`), but a
   single known *integer* as its base:

   ```php
   $i = 42;
   \PHPStan\dumpType($i);   // dumped type: int
   ```

   Steins still *reasons* with the exact value `42` — that is how
   it proves the findings later in this chapter — but the dump
   surface renders it as `int`. The dump is a debugging
   convenience, not a full serialization of the value.

2. `dumpType` names a function PHP does not have. A committed call
   is a fatal, so Steins prints it at fail level to nag you into
   deleting it. Use it while exploring; strip it before you ship.

## The four-layer value domain

Steins describes what it knows about one value in four layers,
from most precise to least. The layers name the *shape of the
knowledge*, and losing precision is a descent between them.

### 1. Singleton — exactly one value

The tightest fact: Steins knows the concrete value.

```php
function greet(string $s): void {
    if ($s === 'ready') {
        \PHPStan\dumpType($s);   // dumped type: 'ready'
    }
}
```

Inside the branch, `$s` can only be `'ready'` — the guard proved
it. (For integers and floats the dump prints the base, per the
note above; the singleton fact is still there.)

### 2. OneOf — a small finite set

When a value is one of a few known possibilities (up to eight),
Steins keeps the whole set. This is the closest PHP gets to a
sum type. On the dump surface a finite set of scalars renders as
its base type — `dumpType` does not spell the members out — but
the set is what the engine narrows against branch by branch (see
[Chapter 3](03-narrowing-and-trust.md)).

### 3. Refined — a base plus a predicate

Between "exactly this value" and "just a string" sits the
refinement: a base type restricted by a proven predicate. These
have names you already know from PHPStan, and `dumpType` prints
them precisely.

```php
function f(string $s): void {
    if ($s !== '') {
        \PHPStan\dumpType($s);   // dumped type: non-empty-string
    }
}

function g(int $x): void {
    if ($x > 0)  { \PHPStan\dumpType($x); }   // dumped type: positive-int
    if ($x >= 0) { \PHPStan\dumpType($x); }   // dumped type: non-negative-int
    if ($x < 0)  { \PHPStan\dumpType($x); }   // dumped type: negative-int
}

function h(int $x): void {
    if ($x >= 1 && $x <= 9) {
        \PHPStan\dumpType($x);   // dumped type: int<1, 9>
    }
}
```

There are exactly two refinement kinds: a closed bitset of
string predicates (`non-empty-string`, `non-falsy-string`,
`numeric-string`) and an integer interval (`int<lo, hi>`, of
which `positive-int`, `non-negative-int`, and `negative-int` are
named spellings). The set is closed on purpose — every
interaction stays exhaustively checkable.

### 4. General — the bare base

When Steins knows the type but no more, you get the base:

```php
function bare(int $x): void {
    \PHPStan\dumpType($x);   // dumped type: int
}
```

And when it cannot prove even the base — an unresolvable value,
a partially-known array, a computed string it did not fold — you
get the honest `unknown`:

```php
$arr = [1, 2, 3];
\PHPStan\dumpType($arr);   // dumped type: unknown
```

`unknown` is not a type. It is Steins saying "I decided nothing
here" — and a finding *never* fires against it. That silence is
the whole zero-false-positive stance in one word.

Nullability rides alongside these four as a flag, so a declared
nullable prints as a union with `null`:

```php
function n(?string $x): void {
    \PHPStan\dumpType($x);   // dumped type: string|null
}
```

## Declared types are envelopes, not ceilings

A declared type — native or PHPDoc — is an **upper bound Steins
trusts and refines *within***. Call-site precision may tighten
inside the envelope; it never widens past it. And where a
declared type and a proven value disagree, **the proven value
wins**, because the proven value is what the runtime will see.

```php
/** @return non-empty-string */
function nes(): string { return "x"; }

$r = nes();
\PHPStan\dumpType($r);   // dumped type: 'x'
```

The docblock says `non-empty-string`; Steins followed the body,
proved the return is literally `"x"`, and reported the tighter
truth. The declaration was an envelope — `"x"` fits inside it —
and the proof beat it.

> **If you know PHPStan:** there is no `treatPhpDocTypesAsCertain`
> toggle, and there never will be. The trust order is fixed: a
> proven value always beats a declared type. Configuration
> selects which findings you *see*; it never changes what Steins
> *infers*. Chapter 3 covers the trust stratum that makes this
> safe.

## What "proven" means

A `proof`-layer finding fires only when Steins can prove the
program breaks at runtime — a definite Yes. Three of these
findings are the everyday `TypeError` family.

**Argument mismatch.** A value that cannot satisfy a parameter's
type, checked under the *calling file's* strict mode:

```php
<?php
declare(strict_types=1);
function takesInt(int $x): int { return $x; }
takesInt("abc");
// argument "abc" to takesInt() cannot become int $x — proven TypeError (strict mode)
```

The mode is not incidental. In a file *without*
`declare(strict_types=1)`, PHP coerces — so Steins does too, and
reports only what coercion still cannot save:

```php
<?php
function takesInt(int $x): int { return $x; }
takesInt("abc");   // proven TypeError (coercive mode)
takesInt("123");   // silent — "123" coerces to 123 at runtime
```

`"abc"` breaks in both modes; `"123"` breaks in neither under
coercion, so it stays silent. Steins mirrors PHP's own rule
rather than a stricter one.

**Return mismatch — and why a general parameter is silent.** This
is the single most important thing to internalise about the proof
layer:

```php
<?php
declare(strict_types=1);
function prov(): string { return 5; }            // reported
function gen(int $x): string { return $x; }      // SILENT
```

`prov()` returns a **proven** `5`, and `5` cannot become a
`string`, so:

```text
return 5 cannot become string (return type of prov()) — proven TypeError (strict mode)
```

`gen()` returns `$x`, a **general** `int` parameter — no call
site here proves a concrete value flowing to that `return`. So
Steins stays silent, even though "int declared, string returned"
looks like an obvious mismatch. It is not a *proven* runtime
break until a real value reaches it. This is the value-first
philosophy in action: Steins reports proven values breaking, not
declarations disagreeing. (That declaration-level disagreement is
a real observation — it is just a *contract*-layer concern, off
by the default surface; see [Chapter 5, planned](README.md).)

**Property mismatch.** Assigning a value a typed property cannot
hold:

```php
<?php
declare(strict_types=1);
class Box { public int $n = 0; }
$b = new Box();
$b->n = "str";
// Cannot assign "str" to property Box::$n of type int — proven TypeError (strict mode)
```

> **If you know PHPStan:** where PHPStan has numeric *levels* that
> turn broad classes of check on together, Steins has semantic
> **layers**. Every finding is tagged `proof` / `contract` /
> `mechanics` / `debug` by identity, and a bare `check` surfaces
> `proof` + `mechanics` only. You do not climb a ladder toward
> more findings; you opt into a named surface when you want the
> contract layer. Chapter 7 (planned) covers the stages.

## Native intersection types

Intersection parameters (`A&B`) are judged exactly. A value that
satisfies only one arm is a proven mismatch:

```php
<?php
declare(strict_types=1);
interface A {}
interface B {}
function needsAB(A&B $x): void {}
class Only implements A {}
needsAB(new Only());
// argument new Only() to needsAB() cannot become A&B $x — proven TypeError (strict mode)
```

`new Only()` is provably an `A` but provably not a `B`, so the
intersection fails and Steins says so.

## Generics carry at the new-site

When you construct a generic class with a proven argument, Steins
infers the type argument at that `new`:

```php
/** @template T */
class Box {
    /** @param T $v */
    public function __construct(public mixed $v) {}
}
$b = new Box(42);   // inferred as Box<int> at the construction site
```

This is a deliberately small slice: the type-argument *value* is
inferred at a direct-`new` argument position. Carrying it through
a later variable binding is not modeled, and there is **no
call-site template solver** — where value propagation reaches,
templates are transparent; where it does not, silence. The
accepted cost is that some library-author generic lints Steins
simply does not attempt.

## Callable signatures

A declared `callable(P): R` carries a parameter and return
contract, and Steins checks a closure passed where one is
expected the way variance requires: the closure may accept
**wider** parameters (contravariance — it must handle at least
what the caller sends) and return **narrower** results
(covariance — it must produce at least what the caller expects).
A closure that demands a narrower parameter than the signature
promises, or returns something wider, is the mismatch.

The bare forms (`callable`, `Closure`) carry no signature and are
checked only for being callable at all. A signature that mentions
a template variable drops to the bare form — Steins does not
solve it.

## What the type system deliberately does not claim

A specification is also a refusal list. The proof layer stays
silent — on purpose — in these places, and each silence costs
true positives, never false ones:

- **Maybe is silence.** Where analysis cannot decide — an
  unresolvable name, dynamic dispatch, a budget cutoff, a missing
  sidecar — the answer widens to `Maybe`, and `Maybe` never
  prints. (Chapter 3 enumerates where `Maybe` comes from so you
  can predict silence rather than discover it.)
- **`unknown` is the honest floor**, not a bug. It means "no fact
  here," and it is always the safe side.
- **Declaration coherence is not a proof concern.** A native
  `?string` that is wider than a `@param string` is type-safe
  code, not a runtime break — Steins does not report it, a
  standing refusal it shares with PHPStan by design.
- **No worst-casing.** Steins reports a definite Yes only. It
  never inflates a "probably" into a finding to be safe, because
  a noisy default is a deleted tool.

That last point is the through-line of the whole type system:
precision where a value is traceable, honest silence everywhere
else, and the word *proven* reserved for exactly what it says.

## What's next

Chapter 3 is the engine that takes these value facts and sharpens
them as control flow passes through guards — and introduces the
trust stratum that keeps a docblock claim from ever masquerading
as a proof.
