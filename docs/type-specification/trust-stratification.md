# Trust Stratification

**Status: implemented** (ADR-0037, ADR-0052 §5).

## The standing rule

**A proven value never loses to a declared type.**

Where a call-site-propagated value and a declared type disagree, the value wins,
because the value is what the runtime will see. A declaration is an
[authoritative envelope](overview.md) — an upper bound to refine *within* — not
a ceiling on what the analyzer is allowed to know.

The consequence users notice: there is no `treatPhpDocTypesAsCertain` toggle, no
"trust the docblock" mode, and no way to configure the trust order. The order is
fixed (ADR-0002, ADR-0009, ADR-0037). Making it configurable would mean shipping
a mode in which findings are unsound, which the zero-FP bar does not permit.

## The two strata

Every bound fact carries a **trust stratum** — a checked attribute, not a
display string:

| Stratum | Origin | Fit to premise a proof-layer finding |
| --- | --- | --- |
| `Verified` | A runtime-executed test on the live branch (`===`, `is_int()`, `instanceof`, ordering comparisons, truthiness), or a native declaration seed. | yes |
| `Asserted` | A docblock claim (`@phpstan-assert` family) or `assert($expr)` narrowing. | **no** |

The distinction is operational, not philosophical. A `Verified` fact holds
because *the branch only runs if the test passed*. An `Asserted` fact holds
because someone said so — and in the `assert()` case, because someone said so in
code that PHP does not execute at all under `zend.assertions=-1`, the standard
production setting.

The consumption rule: **a proof-layer id requires all-`Verified` premises.**
Contract-layer ids may consume `Asserted` facts — they are claims about
declarations, and a declaration-derived premise is appropriate there.

## The derivation clause

A derived fact's stratum is the **minimum over every fact consumed in its
derivation**, where `Asserted` dominates:

```text
min(Verified, Verified) = Verified
min(_, Asserted)        = Asserted
```

This is applied at every derivation site: folds, array composition, heap
property writes and reads, branch joins, and binding-descent seeding. The point
is that `Asserted` **cannot launder into `Verified` across a derivation step**:

```php
/** @phpstan-assert int $x */
function assertInt(mixed $x): void {}

assertInt($v);          // $v: Asserted int
$o->prop = $v;          // the property fact is Asserted (heap write)
$w = $o->prop;          // still Asserted (heap read)
takesString($w);        // NOT a proof-layer finding — the premise is Asserted
```

`min` is commutative and associative, so the rule is order-independent — which
is what keeps it compatible with ADR-0048's "no global-ordering dependence"
constraint for future position queries.

## `assert()` and `zend.assertions`

`assert($expr)` narrows like a guard, but binds at the `Asserted` stratum,
because PHP compiles the call away under `zend.assertions=-1`.

A project that genuinely runs with assertions on can say so:

```toml
# steins.toml
[runtime]
zend-assertions = "enabled"    # default: disabled
```

This promotes `assert($expr)` narrowing to `Verified`. It is intent the repo
declares about its own runtime, reviewably — the config-carries-intent principle
of ADR-0023 — not a strictness knob.

## The exactness dimension

Trust stratification is about *how a fact was learned*. A second, orthogonal
axis governs *how strong a class fact is*: an object's class may be **exact**
(allocation-proven: `new`, an enum case, a clone of an exact object) or a **lower
bound** (a `$this` seed — the runtime object may be any descendant that
inherited the method).

`No`-side conclusions require exactness: "`is_a(class, T) = No`, therefore this
object is not a `T`" is only sound when `class` is exact, since a lower bound's
actual instance may be a descendant that *is* a `T`. `Yes`-side conclusions hold
for a lower bound too (every descendant is a `T`). See
[object-model.md](object-model.md).
