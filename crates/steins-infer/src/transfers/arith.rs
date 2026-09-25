//! The arithmetic transfers: `min` / `max` (issue #118), `abs` and `pow` (issue #40).
//! Each answers from what its arguments' facts already say — an interval, a base, a
//! union of the arguments — and computes no value; a value is the fold lane's.

use std::collections::HashMap;

use steins_domain::{ArmKnown, Base, Fact, IntRange, Refinement, ShapeFact, UnionArm, Val};
use steins_syntax::ArgValue;

use crate::cx::Cx;
use crate::env::{Known, Store};
use crate::fold::Folder;
use crate::shape_projection::shape_value_union;
use crate::transfers::transfer_arg_fact;

/// `min(…)` / `max(…)` → **the union of what the arguments already say** (issue
/// #118, ADR-0061's rung).
///
/// # The load-bearing PHP fact
///
/// **`min`/`max` RETURN ONE OF THEIR ARGUMENTS**, not a coerced copy. Witnessed
/// at `PINNED_PHP` (8.5.8): `min('a', 1)` is `int(1)`, the second argument
/// verbatim; `min([3, '1', 2])` is `string(1) "1"`, an element verbatim. So the
/// union of the argument facts admits the result **unconditionally** — the rule
/// needs no premise about comparability, ordering, or type juggling.
///
/// # The ladder
///
/// 1. **Two or more arguments, all int-ranged** → the *composed interval*,
///    strictly sharper than the union: for `a ∈ [l₁, h₁]`, `b ∈ [l₂, h₂]`,
///    `min(a, b) ∈ [min(l₁, l₂), min(h₁, h₂)]`, `max` dually. Interval
///    arithmetic over declared knowledge, never a re-derivation of what PHP
///    compared. A composition collapsing to a point spells the point (`min(1,
///    2)` is `1`), since `min`/`max` are not on the folding allowlist.
/// 2. **Two or more arguments otherwise** → the plain domain join of the facts.
/// 3. **One argument** → the unary ARRAY form: the shape's own value union
///    ([`shape_value_union`]), because the result is one of the array's *elements*.
///    A witnessed array lifts first, so `min([1, 2, 3])` is `1|2|3`.
///
/// # The declines, each for a stated reason
///
/// * **Any argument without a usable fact declines the whole rule** — the missing
///   one could hold the winner, so no partial answer.
/// * **A join the four-layer domain cannot spell declines** — `min($int, $string)`
///   is a two-base union with no single [`Fact`], like `json_decode`'s. ADR-0062
///   Amendment B called for these to enter the *arm* lane, which has no
///   argument-dependent channel here, so the honest floor stands.
/// * **A nullable int leaves the interval path** and takes the union: `min(null,
///   5)` is `NULL` at 8.5.8, so an `?int` argument must not yield a bare `int`.
/// * **A one-argument call whose fact is not an array declines** — `min(5)` is a
///   `TypeError`.
/// * **A zero-argument call declines** — `min()` is an `ArgumentCountError`.
///
/// `min([])` throwing a `ValueError` costs the rule nothing: a throw is the
/// *absence* of a return, so there is no value for the claim to be wrong about
/// (the same vacuity [`range_transfer`] leans on).
///
/// [`range_transfer`]: crate::transfers::lists::range_transfer
pub(super) fn min_max_transfer(
    cx: &Cx,
    folder: &mut dyn Folder,
    lower: &str,
    args: &[ArgValue],
    env: &HashMap<String, Known>,
    store: Option<&Store>,
) -> Option<Fact> {
    let [first, rest @ ..] = args else { return None };
    if rest.is_empty() {
        // The unary array form. A `Singleton(Val::Array)` lifts (A-G5) rather than
        // being read positionally: which element wins is a comparison question, and
        // the union is the claim this rule makes on either lane.
        return match transfer_arg_fact(cx, folder, first, env, store)? {
            Fact::Shape { shape, nullable: false } => shape_value_union(&shape),
            Fact::Singleton(Val::Array(entries)) => shape_value_union(&ShapeFact::lift(&entries)),
            _ => None,
        };
    }
    let mut facts = Vec::with_capacity(args.len());
    for a in args {
        // No short-circuit and no skipping: an argument with no fact declines the
        // whole rule, so the answer is a function of the entire call.
        facts.push(transfer_arg_fact(cx, folder, a, env, store)?);
    }
    min_max_interval(lower == "min", &facts).or_else(|| {
        facts.iter().skip(1).try_fold(facts[0].clone(), |acc, f| acc.join(f))
    })
}

/// The composed interval of a `min`/`max` call whose every argument is a
/// non-nullable int — see [`min_max_transfer`] for the arithmetic and why it is
/// tight. `None` as soon as one argument is anything else, which routes the call
/// to the union.
fn min_max_interval(is_min: bool, facts: &[Fact]) -> Option<Fact> {
    let mut acc: Option<IntRange> = None;
    for f in facts {
        let r = fact_int_range(f)?;
        acc = Some(match acc {
            None => r,
            Some(a) => {
                let (lo, hi) = if is_min {
                    (a.lo().min(r.lo()), a.hi().min(r.hi()))
                } else {
                    (a.lo().max(r.lo()), a.hi().max(r.hi()))
                };
                // `lo <= hi` holds for both arms (a pointwise min/max of two
                // ordered pairs stays ordered); the fallback keeps this total.
                IntRange::new(lo, hi)?
            }
        });
    }
    let r = acc?;
    Some(if r.lo() == r.hi() {
        Fact::Singleton(Val::Int(r.lo()))
    } else {
        Fact::refined(Base::Int, Refinement::Int(r), false)
    })
}

/// The interval a fact pins on a **non-nullable int**, or `None` for anything else.
/// A `OneOf` is deliberately excluded: its finite member set is what the union path
/// carries exactly, and hulling it here would trade a gap-free answer for an
/// interval.
fn fact_int_range(f: &Fact) -> Option<IntRange> {
    match f {
        Fact::Singleton(Val::Int(i)) => Some(IntRange::point(*i)),
        Fact::Refined { base: Base::Int, refinement: Refinement::Int(r), nullable: false } => {
            Some(*r)
        }
        Fact::General { base: Base::Int, nullable: false } => Some(IntRange::FULL),
        _ => None,
    }
}

/// `abs($num)` → **the argument's own base, folded onto the non-negative half of
/// the axis** (issue #40 — the head of the arithmetic scalar-union family, and
/// the family's whole ADR-0064 seam (ii) shape: the argument's TYPE decides the
/// return, no value is computed by the analyzer).
///
/// # The load-bearing PHP fact, which is the one PHPStan's fixture gets wrong
///
/// `abs(int)` is *almost* `int<0, max>`, and the exception is why this rule
/// declines where it does: `PHP_INT_MIN` has no positive counterpart in a 64-bit
/// int, so the engine hands back a **float**. Witnessed at `PINNED_PHP` (8.5.9):
///
/// ```text
/// abs(PHP_INT_MIN)      → float(9.223372036854776E+18)
/// abs(PHP_INT_MIN + 1)  → int(9223372036854775807)
/// abs(-1.0)             → float(1)
/// abs(true) abs(false)  → int(1) int(0)
/// abs(null)             → int(0)
/// ```
///
/// `abs.php`'s unbounded rows (`@var int`, `@var negative-int`, `@var int<min,
/// 0>`) assert `int<0, max>`, and that assertion is false at exactly one
/// argument. So **an int interval admitting `PHP_INT_MIN` declines**, and the
/// ADR-0069 floor's honest `int<0, max>|float` stands. A bounded interval — every
/// interval a docblock writes with a finite lower bound — is exact.
///
/// # The declines, each for a stated reason
///
/// * **`PHP_INT_MIN` in the interval** — above. `Fact::General { Int }` is the
///   full interval, so a plain `int $x` declines too.
/// * **A string argument** — `abs('123')` is `int(123)` and
///   `abs('3000000000')` is an int or a float *by the engine's own word size*.
///   That is the recorded [`RefusalAxis::IntegerWidth`] row behind `abs`'s
///   folding-allowlist refusal, and it is a VALUE question (ADR-0064 seam (i))
///   the sidecar owns; a type rung that answered it would be reimplementing the
///   fold in Rust.
/// * **A nullable abstract argument** — `null` is a `TypeError` under
///   `strict_types=1` and a deprecated coercion without it, and the two modes
///   answer different things about the same call. The `null` VALUE is read
///   (`abs(null)` is `int(0)` in every mode that returns at all), the nullable
///   *envelope* is not.
/// * **A union carrying a string or bool arm** — the string arm is the width
///   question again; a `bool` arm would have to become an int arm, which is a
///   base change this rule has no witness for beyond the finite layer.
///
/// One more decline is **not this rule's**, and is recorded here because it
/// looks like one: a *declared* float (`@var 1.0 $x`) never reaches the rule at
/// all. [`steins_contract::to_fact`] refuses `float` and float literals by
/// design — `Base(Float)` admits ints under PHPStan's own semantics while
/// `Fact::General { base: Float }` does not, so lowering would reject values the
/// declaration admits. A NATIVE `float $x` parameter still seeds the value lane
/// and is read here; only the declared spelling is silent.
///
/// [`RefusalAxis::IntegerWidth`]: steins_catalog::RefusalAxis::IntegerWidth
pub(super) fn abs_transfer(
    cx: &Cx,
    folder: &mut dyn Folder,
    args: &[ArgValue],
    env: &HashMap<String, Known>,
    store: Option<&Store>,
) -> Option<Fact> {
    let [only] = args else { return None };
    match transfer_arg_fact(cx, folder, only, env, store)? {
        Fact::Singleton(v) => abs_val(&v).map(Fact::Singleton),
        Fact::OneOf(ref vals) => {
            let mapped = vals.iter().map(abs_val).collect::<Option<Vec<Val>>>()?;
            Fact::from_vals(mapped)
        }
        Fact::Refined { base: Base::Int, refinement: Refinement::Int(r), nullable: false } => {
            abs_range(r).map(|out| {
                if out.lo() == out.hi() {
                    Fact::Singleton(Val::Int(out.lo()))
                } else {
                    Fact::refined(Base::Int, Refinement::Int(out), false)
                }
            })
        }
        // The float half is total: `abs` maps every float to a float, including
        // the infinities and `NAN`.
        Fact::General { base: Base::Float, nullable: false } => {
            Some(Fact::General { base: Base::Float, nullable: false })
        }
        Fact::Union { ref arms, nullable: false }
            if arms.iter().all(|(b, _)| matches!(b, Base::Int | Base::Float)) =>
        {
            abs_union(arms)
        }
        _ => None,
    }
}

/// `abs` of one concrete value, or `None` where the four-layer domain would have
/// to guess which base comes back — see [`abs_transfer`] for each witness.
fn abs_val(v: &Val) -> Option<Val> {
    match v {
        // `checked_abs` is `None` at exactly `PHP_INT_MIN`, which is exactly
        // where the engine stops answering with an int.
        Val::Int(i) => i.checked_abs().map(Val::Int),
        Val::Float(f) => Some(Val::Float(f.abs())),
        Val::Bool(b) => Some(Val::Int(i64::from(*b))),
        // `abs(null)` is `int(0)` in weak mode and a `TypeError` in strict mode,
        // and a throw is the ABSENCE of a return — so `0` is never wrong about a
        // value this call produced.
        Val::Null => Some(Val::Int(0)),
        Val::Str(_) | Val::Array(_) => None,
    }
}

/// The interval `abs` maps `r` onto, or `None` when `r` admits `PHP_INT_MIN` and
/// the answer is therefore not an int interval at all.
///
/// Three arms, and the middle one is the reflection: an all-negative interval
/// comes back *reversed* (`int<-456, -123>` → `int<123, 456>`), a straddling one
/// is floored at zero and capped by whichever end is further from it.
fn abs_range(r: IntRange) -> Option<IntRange> {
    if r.lo() == i64::MIN {
        return None;
    }
    let (lo, hi) = if r.lo() >= 0 {
        (r.lo(), r.hi())
    } else if r.hi() <= 0 {
        (-r.hi(), -r.lo())
    } else {
        (0, r.hi().max(-r.lo()))
    };
    IntRange::new(lo, hi)
}

/// `abs` over an int/float union: each arm mapped by its own rule, and the
/// `PHP_INT_MIN` overflow expressed rather than declined — an int arm that
/// admits it widens to `int<0, max>` **and** contributes the float arm the
/// overflow lands in, which the union can carry where a single-base fact could
/// not.
fn abs_union(arms: &[UnionArm]) -> Option<Fact> {
    let mut out: Vec<UnionArm> = Vec::with_capacity(arms.len() + 1);
    let overflow = |out: &mut Vec<UnionArm>| {
        out.push((Base::Int, ArmKnown::Refined(Refinement::Int(IntRange::NON_NEGATIVE))));
        out.push((Base::Float, ArmKnown::Whole));
    };
    for (base, known) in arms {
        match (base, known) {
            (Base::Float, _) => out.push((Base::Float, ArmKnown::Whole)),
            (Base::Int, ArmKnown::Refined(Refinement::Int(r))) => match abs_range(*r) {
                Some(a) => out.push((Base::Int, ArmKnown::Refined(Refinement::Int(a)))),
                None => overflow(&mut out),
            },
            (Base::Int, ArmKnown::Whole) => overflow(&mut out),
            _ => return None,
        }
    }
    Fact::union(out, false)
}

/// `pow($num, $exponent)` → **`int|float`, sharpened where one operand pins the
/// answer** (issue #40).
///
/// # Why the rule may speak at all
///
/// `pow`'s reflected declaration is `object|int|float` — the `object` arm is
/// `GMP` and anything else overloading `**`. A [`Fact`] describes scalars, `null`
/// and arrays and *nothing else*, so an operand carrying a fact is provably not
/// an object, and the `object` arm is discharged by the fact's own existence. An
/// object-typed variable carries no fact, so `pow($gmpA, $gmpB)` declines one
/// rung up without this rule ever seeing it.
///
/// # The grid, probed at `PINNED_PHP` (8.5.9)
///
/// ```text
/// pow(2, 0)     int(1)    pow(2.0, 0)     float(1)  pow("5.5", 0)   float(1)
/// pow(2, true)  int(2)    pow(2.0, true)  float(2)  pow("5", true)  int(5)
/// pow(2, 0.0)   float(1)  pow(2, 62)      int(…)    pow(2, 63)      float(…)
/// pow(null, 0)  int(1)    pow(null, 1)    int(0)    pow(-1, 5.5)    float(NAN)
/// ```
///
/// Four readings fall out, in the order the rule takes them:
///
/// 1. **An exponent that numerifies to the integer 0** answers `1` — `1.0` for a
///    float base, and `1|1.0` for a *string* base, since a string numerifies to
///    an int or a float and the exponent-0 result follows it (`pow("5.5", 0)` is
///    `float(1)`, not `int(1)`). That last row is why `pow.php`'s own assertion
///    that `pow($s, 0)` is `1` is not winnable here.
/// 2. **An exponent that numerifies to the integer 1** answers the base
///    numerified: `int` for an int/bool/null base, `float` for a float one.
/// 3. **Either operand certainly a float** answers `float` — php-src promotes
///    the whole operation to a double, so a float exponent takes an int base with
///    it (`pow(2, 0.0)` is `float(1)`).
/// 4. **Otherwise** `int|float`: the int-base/int-exponent case is an int until
///    it overflows the word, at which point it is a float (`pow(2, 63)`), and
///    which one it is is a VALUE question — the sidecar's, not this rung's.
///
/// A non-integral float exponent (`pow(-1, 5.5)`) is `NAN`, which is a float and
/// so already inside every answer above.
///
/// # The declines
///
/// * **Either operand an array** — `1 ** []` is a `TypeError`. Vacuously the
///   rule could claim anything; it claims nothing, because an array operand
///   means the call was written by mistake and silence is the honest report.
/// * **An operand with no fact** — the object case above, and `mixed`.
/// * **A string EXPONENT spelling 0 or 1** is not read as one. `'0'` and `'1'`
///   are exact, so the shortcut would be sound for those two spellings — and
///   admitting a numeric string here at all is the engine-width question the fold
///   lane refuses for `abs`, so the whole base stays out and the call takes the
///   `int|float` below.
pub(super) fn pow_transfer(
    cx: &Cx,
    folder: &mut dyn Folder,
    args: &[ArgValue],
    env: &HashMap<String, Known>,
    store: Option<&Store>,
) -> Option<Fact> {
    let [num, exponent] = args else { return None };
    let base = transfer_arg_fact(cx, folder, num, env, store)?;
    let exp = transfer_arg_fact(cx, folder, exponent, env, store)?;
    if !pow_numeric_operand(&base) || !pow_numeric_operand(&exp) {
        return None;
    }
    let int_or_float = || Fact::union(vec![(Base::Int, ArmKnown::Whole), (Base::Float, ArmKnown::Whole)], false);
    let base_kind = pow_operand_base(&base);
    if pow_exponent_is(&exp, 0) {
        return match base_kind {
            Some(Base::Float) => Some(Fact::Singleton(Val::Float(1.0))),
            Some(Base::Int | Base::Bool) => Some(Fact::Singleton(Val::Int(1))),
            Some(Base::String) => Fact::from_vals(vec![Val::Int(1), Val::Float(1.0)]),
            None => int_or_float(),
        };
    }
    if pow_exponent_is(&exp, 1) {
        return match base_kind {
            Some(Base::Float) => Some(Fact::General { base: Base::Float, nullable: false }),
            Some(Base::Int | Base::Bool) => {
                Some(Fact::General { base: Base::Int, nullable: false })
            }
            Some(Base::String) | None => int_or_float(),
        };
    }
    if base_kind == Some(Base::Float) || pow_operand_base(&exp) == Some(Base::Float) {
        return Some(Fact::General { base: Base::Float, nullable: false });
    }
    int_or_float()
}

/// Whether an operand is one PHP's `**` numerifies rather than rejecting: every
/// fact the domain spells EXCEPT an array. See [`pow_transfer`] for why an
/// object never reaches here.
fn pow_numeric_operand(f: &Fact) -> bool {
    match f {
        Fact::Shape { .. } | Fact::Singleton(Val::Array(_)) => false,
        Fact::OneOf(vals) => !vals.iter().any(|v| matches!(v, Val::Array(_))),
        _ => true,
    }
}

/// The single base an operand numerifies as, or `None` when it spans more than
/// one. `null` counts as `Int` — it numerifies to `int(0)` and nothing else.
///
/// **A nullable FLOAT is the one base nullability decides**, and it decides it
/// by declining: `pow(null, 2)` is `int(0)`, so a `?float` operand pins no base
/// and the call falls to the plain `int|float` that admits both halves. `?int`,
/// `?bool` and `?string` keep their base, because every answer those three
/// produce already admits `null`'s `int(0)` — `1` for the zero exponent, `int`
/// for the one exponent, `1|1.0` and `int|float` for the string arms.
fn pow_operand_base(f: &Fact) -> Option<Base> {
    let of = |v: &Val| match v {
        Val::Null => Some(Base::Int),
        other => other.base(),
    };
    match f {
        Fact::Singleton(v) => of(v),
        Fact::OneOf(vals) => {
            let first = of(vals.first()?)?;
            vals.iter().all(|v| of(v) == Some(first)).then_some(first)
        }
        Fact::Refined { base, nullable, .. } | Fact::General { base, nullable } => {
            (!(*nullable && *base == Base::Float)).then_some(*base)
        }
        Fact::Union { .. } | Fact::Shape { .. } => None,
    }
}

/// Whether an exponent is CERTAINLY the integer `want` (0 or 1), across the
/// spellings PHP numerifies to it exactly — the int, the bool, and `null` for 0.
/// A float spelling is excluded because it changes the RESULT's base
/// (`pow(2, 0.0)` is `float(1)`), and a string spelling because reading one is
/// the engine-width question the fold lane owns.
fn pow_exponent_is(f: &Fact, want: i64) -> bool {
    let one = |v: &Val| match v {
        Val::Int(i) => *i == want,
        Val::Bool(b) => i64::from(*b) == want,
        Val::Null => want == 0,
        Val::Float(_) | Val::Str(_) | Val::Array(_) => false,
    };
    match f {
        Fact::Singleton(v) => one(v),
        Fact::OneOf(vals) => vals.iter().all(one),
        _ => false,
    }
}
