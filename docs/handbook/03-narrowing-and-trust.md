# Narrowing and trust

A value fact describes a variable at one point. **Narrowing**
describes how that fact sharpens when control flow passes through
a guard. **Trust** describes *how strongly* a fact was learned —
and whether it is strong enough to premise a proof.

By the end of this chapter you will be able to:

- predict how `instanceof`, `=== null`, `is_int`, `&&`/`||`, and
  ternaries sharpen a variable along each branch;
- explain why a dead branch is silent — and why that is a
  feature, not a gap;
- tell a **Verified** fact from an **Asserted** one, and know why
  a lying `@phpstan-assert` can never forge a proof.

## The mental model: two edges per guard

Every guard splits control flow into two edges — one where the
condition held, one where it did not — and sharpens the
variable's fact on each. If Steins does not recognise the guard,
both edges keep the entry fact unchanged. Conservative by
construction: an unrecognised guard costs precision, never
correctness.

## Truthiness and null guards

The everyday case. A `?string` loses its `null` the moment you
prove it is not null:

```php
function shout(?string $name): void {
    if ($name !== null) {
        \PHPStan\dumpType($name);   // dumped type: string
    }
}
```

A bare truthiness check (`if ($name)`) does the same and more —
it also removes the empty and `"0"` strings on the truthy edge,
because those are PHP's falsy strings.

## Type guards

`is_int`, `is_string`, and kin narrow the branch to the tested
base; `instanceof` narrows to the tested class. The proof that
narrowing happened is that the *use* it enables goes through
cleanly while the unguarded version would break:

```php
class Dog { public function bark(): void {} }

function speak(object $o): void {
    if ($o instanceof Dog) {
        $o->bark();     // silent — narrowed to Dog, the method exists
    }
}
```

Outside the guard, `$o` is just `object` and `$o->bark()` could
not be proven safe; inside it, `bark()` is known to exist. The
silence *is* the narrowing working.

One honesty note: a type predicate on a `mixed` value sharpens
the reasoning Steins uses to accept or reject a later call, but
it does not always surface as a printable value fact —
`dumpType` after `is_int($x)` on a `mixed` may still print
`unknown`. The narrowing is real where it matters (it gates the
findings); the *dump* is just a compact view that does not spell
every internal fact. Reach for typed parameters when you want to
watch a value fact sharpen in the margin.

> **If you know PHPStan:** the guard vocabulary is the same one
> you already reach for. `instanceof` deliberately binds a
> *membership* fact, not an *exactness* fact — Steins will not
> treat "is a `Dog`" as "is exactly `Dog`," because a subclass
> instance would make that wrong. That distinction is what keeps
> the absence-family findings (Chapter 8, planned) sound.

## Equality peels a union

`===` against a literal binds a singleton on the matching edge.
When the subject is a small set of possibilities, each branch
peels one member off:

```php
function state(string $s): void {
    if ($s === 'on') {
        \PHPStan\dumpType($s);        // dumped type: 'on'
    } elseif ($s === 'off') {
        \PHPStan\dumpType($s);        // dumped type: 'off'
    }
}
```

`!==` is the mirror: it *removes* the value on the branch where
it holds.

## Comparisons narrow ranges

Ordering comparisons and the integer predicate methods sharpen
integer intervals, exactly as Chapter 2 showed:

```php
function idx(int $n): void {
    if ($n >= 0) {
        \PHPStan\dumpType($n);   // dumped type: non-negative-int
    }
}
```

`<`, `<=`, `>`, `>=` against a literal each intersect the
interval; compose them (`$n >= 1 && $n <= 9`) for a bounded
`int<1, 9>`.

## Short-circuit threading and ternaries

The operands of `&&` and `||` do not all evaluate in the
pre-branch environment. In `$a && $b`, the right operand `$b` is
evaluated under the truthy edge of `$a`; in `$a || $b`, under the
falsy edge. This is what makes the ubiquitous null-guard-then-use
idiom safe to write on one line:

```php
function safe(?Dog $d): void {
    if ($d !== null && $d->bark()) {}   // the receiver is seen non-null
}
```

By the time `$d->bark()` is evaluated, the left operand already
proved `$d` is not null, so the method call is checked against a
non-null receiver.

Ternaries resolve each arm under the matching edge, and the null
coalesce clears null before joining:

```php
function co(?string $s): void {
    $v = $s ?? 'default';
    \PHPStan\dumpType($v);   // dumped type: string
}
```

`$s ?? 'default'` is "`$s` with its null cleared, or `'default'`"
— either way a `string`, and Steins says so.

## Dead branches are silent

Threading has a payoff that matters for the zero-false-positive
bar: when a guard chain is a **contradiction**, the branch it
guards is unreachable, and Steins refuses to report on
unreachable code.

```php
function dead(int $x): void {
    if ($x === 5 && $x === 6) {
        $x->nope();     // silent — this branch cannot run
    }
}
```

`$x` cannot be both `5` and `6`, so the body is dead. A checker
that reasoned syntactically might flag `$x->nope()` (an int has
no methods); Steins proves the branch is unreachable and stays
quiet. It reports what *runs* and breaks, not what is written.

## `class_exists` / `method_exists` verdicts

Existence guards narrow too, and the direction that matters most
is the one that *prevents* a false finding. Steins is willing to
prove a class or function is undefined (Chapter 1's absence
family) — but not across a guard that already checked:

```php
function make(string $cls): void {
    if (class_exists($cls)) {
        $o = new $cls();     // silent — the guard proved the class is present
    }
}
```

Without the guard, a `new` on an unproven dynamic class name is
opaque (silence, not a finding — Steins cannot prove absence of a
runtime-computed name). With the guard, the presence is
established, and the construction is clean. Either way, no false
positive.

## The trust stratum

Now the second axis. Every fact Steins learns carries a **trust
stratum** — a checked attribute recording *how* it was learned:

| Stratum | Where it comes from | May premise a proof? |
| --- | --- | --- |
| **Verified** | A test the runtime actually executes on the live branch — `===`, `is_int()`, `instanceof`, ordering, truthiness — or a native type declaration. | **Yes** |
| **Asserted** | A docblock claim (`@phpstan-assert`) or an `assert($expr)` narrowing. | **No** |

The distinction is operational, not philosophical. A `Verified`
fact holds because *the branch only runs if the test passed*. An
`Asserted` fact holds because someone *said so* — and in the
`assert()` case, because they said so in code PHP does not even
execute under the standard production setting `zend.assertions=-1`.

The rule: **a proof-layer finding requires all-Verified
premises.** A derived fact inherits the *minimum* stratum of
everything that went into it, and `Asserted` dominates — so an
asserted fact can never launder itself into a verified one across
a derivation step.

## The lying `@phpstan-assert`

Here is why the stratum matters. Suppose a helper *claims* to
assert a type but does nothing:

```php
<?php
declare(strict_types=1);

/** @phpstan-assert int $x */
function assertInt(mixed $x): void {}   // the body is empty — the claim is a lie

function takesString(string $s): void {}

function demo(mixed $v): void {
    assertInt($v);          // $v is now Asserted int
    takesString($v);        // passing an "int" where a string is wanted
}
```

Read syntactically, this looks like a provable `TypeError`:
`$v` was asserted `int`, and an `int` cannot become a `string`.
A checker that trusted the docblock would fire — and would be
**wrong**, because the assertion is a lie and `$v` could be
anything at runtime.

Steins stays silent. The `int` fact is `Asserted`, a proof-layer
finding needs `Verified` premises, and so the finding is
withheld. `steins check` prints nothing and exits `0`. The lie
cannot forge a proof — it can only fail to produce one.

> **If you know PHPStan:** this is the structural replacement for
> `treatPhpDocTypesAsCertain`. There is no global toggle for
> "trust the docblock," because trust is a per-fact attribute,
> not a mode. Docblock-derived facts still power the *contract*
> layer (they are, after all, claims about declarations) — they
> simply cannot underwrite a zero-FP proof.

## `assert()` and `zend.assertions`

`assert($expr)` narrows like any guard, but binds `Asserted`,
because PHP compiles the call away in production. A project that
genuinely runs with assertions enabled can declare that intent:

```toml
# steins.toml
[runtime]
zend-assertions = "enabled"   # default: disabled
```

This promotes `assert()` narrowing to `Verified`. It is not a
strictness knob — it is the repository stating a reviewable fact
about the runtime it actually boots on, and Steins takes it at
its word only because *you* declared it about *your* runtime.

## What is not narrowed yet

Recorded honestly — each of these costs true positives, never
false ones, because an unknown widens to silence:

- **Loops** are opaque for flow: no loop-carried facts yet, only
  a conservative "these variables may have changed."
- **`try`/`catch`/`finally`** control flow is opaque for value
  narrowing (catch *matching* against declared types still works).
- **Property chains as guard operands** — one level of property
  access is modeled; chained lvalues (`$a->b->c`) are not.
- **`??` in guard position** does not yet refine the way an
  explicit `$a !== null ? $a : $b` does; it yields a value fact
  only.
- **Array element narrowing** — an array is a fact only when
  *fully* known.

When narrowing is not recognised, both edges keep the entry fact.
Steins stays conservative rather than making a wrong call.

## What's next

Chapter 4 turns to the second dimension Steins infers alongside
types: **effects** — what your code *does* beyond computing a
value, and how declaring an envelope turns that into a checkable
contract.
