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
| `Verified` | A runtime-executed test on the live branch (`===`, `is_int()`, `instanceof`, ordering comparisons, truthiness), a native declaration seed, or an `assert($expr)` construct (read as a throw-guard — see below). | yes |
| `Asserted` | A docblock claim (`@phpstan-assert` family). | **no** |

The distinction is operational, not philosophical. A `Verified` fact holds
because *the branch only runs if the test passed*. An `Asserted` fact holds
because someone said so in a docblock.

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

## `assert()` reads as a throw-guard

`assert($expr)` narrows the fall-through env at the `Verified` stratum,
**unconditionally**. The 2026-07-25 owner ruling (ADR-0052 amendment "assert()
reads as a throw-guard") reads `assert($expr)` as statically equivalent to
`if (!$expr) throw`: continuing past it means the condition held, so the fact is
fit for the proof layer exactly as a native throw-guard would be. Steins does not
consult `zend.assertions` at all.

The honest epistemic note: under `zend.assertions=-1` (the production default)
PHP never evaluates the expression, so the fall-through carries no *runtime*
guarantee. The ruling assigns that residual risk to the operator who chose to
disable the runtime check — a finding premised on an assert-derived fact is not a
false positive; it is the runtime check the operator turned off, reported
statically (ADR-0002's zero-FP identity, read accordingly). There is **no
`[runtime] zend-assertions` knob** — it was abolished; the key is now an
unknown-key config error.

**Boundary:** this covers the `assert()` *construct* only. The `@phpstan-assert`
tag family stays `Asserted` — a docblock is a claim, and a lying tag must still
be unable to forge a proof.

## The exactness dimension

Trust stratification is about *how a fact was learned*. A second, orthogonal
axis governs *how strong a class fact is*: an object's class may be **exact**
(allocation-proven: `new`, an enum case, a clone of an exact object; a `$this`
in a `final` class or an enum; a descent-proved receiver) or a **lower bound**
(any other `$this` seed — the runtime object may be any descendant that
inherited the method).

`No`-side conclusions require exactness: "`is_a(class, T) = No`, therefore this
object is not a `T`" is only sound when `class` is exact, since a lower bound's
actual instance may be a descendant that *is* a `T`. `Yes`-side conclusions hold
for a lower bound too (every descendant is a `T`). See
[object-model.md](object-model.md).
