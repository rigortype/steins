//! The argument-dispatched transfers (ADR-0064 seam (iii)): per-builtin rungs that
//! carry a fact through `explode`, `range`, `preg_replace`, `min` / `max`,
//! `abs`, `pow`, `var_export`, the string predicates, `sprintf` and the list
//! transfers.

use std::collections::HashMap;

use steins_domain::{
    Base, Certainty, Fact, IntRange, Presence, Refinement, ShapeFact, StrPreds, Tail, Val,
};
use steins_syntax::{ArgValue, ArrayKey, RefKind};

use crate::coerce::{CastTarget, php_cast_fact};
use crate::cx::Cx;
use crate::env::{
    ContractArm, Known, Store, Stratum, array_literal_fact, singleton_fact, val_of,
};
use crate::walk::value_stratum;
use crate::fold::Folder;
use crate::fact_is_int;
use crate::builtin_returns::transfer_envelope_admits;
use crate::shape_projection::{shape_fact, shape_value_union};

/// **The argument-dispatched transfers** (ADR-0064 seam ii, the DR3 batch).
///
/// [`shape_builtin_return_fact`]'s own rung reads exactly one argument, and that
/// argument has to be an array — `count($x)`, `array_values($x)`. The transfers
/// here need strictly more: which argument decides the answer varies by function
/// (`explode`'s separator, `var_export`'s flag, `preg_replace`'s subject), and
/// the deciding fact is a scalar, not a shape. So the seam gains ONE thing — a
/// per-argument fact reader ([`transfer_arg_fact`]) — and every rule stays a
/// plain `&[ArgValue] -> Option<Fact>` function behind the same gate.
///
/// **The admission gate is [`transfer_envelope_admits`]** — ADR-0061 §2 in both
/// its legs. The running engine's own reflected *declaration* must be the one the
/// rule was written against (carrying the sidecar-presence and A9 monkey-patch
/// legs: a run with no PHP, a monkey-patch extension, a project function
/// shadowing the name, or an engine whose declaration has moved withholds the
/// rule), AND — where that declaration lowers to a value-domain fact — the rule's
/// output must be extensionally inside it. The array/nullable-union results below
/// have no `Fact` form to be inside, so for those the declaration stands alone,
/// exactly as it did before the arithmetic family arrived with declarations
/// (`int|float`) that do.
///
/// **Stratum is ADR-0061 §3's derivation clause**: `min` over every argument the
/// call passes — a transfer premised on a docblock-claimed separator is
/// `Asserted` and can never premise a proof-layer finding.
///
/// Every rule below is **independently implemented** (ADR-0061 §4): authored from
/// `php -r` probes against `PINNED_PHP` and php.net's documented semantics, not
/// from phpstan-src text.
///
/// [`shape_builtin_return_fact`]: crate::builtin_returns::shape_builtin_return_fact
/// [`shape_projection_fact`]: crate::shape_projection::shape_projection_fact
pub(crate) fn arg_dispatch_return_fact(
    cx: &Cx,
    folder: &mut dyn Folder,
    name: &str,
    args: &[ArgValue],
    env: &HashMap<String, Known>,
    store: Option<&Store>,
) -> Option<(Fact, Stratum)> {
    /// `explode`/`range`: `array` (PHP 8.5.8 `ReflectionFunction::getReturnType`).
    const ARRAY: &[&str] = &["array"];
    /// `var_export`: `?string` — the flag is what strips the null arm.
    const NULLABLE_STRING: &[&str] = &["?string", "string|null"];
    /// `preg_replace`: the three-member union, either rendering order.
    const PREG_REPLACE: &[&str] = &["array|string|null", "string|array|null"];
    /// `min`/`max`: a bare **`mixed`**, which pins nothing on its own — the arm
    /// declaring it MUST carry [`ARITY_MIN_MAX`] (ADR-0064 Amendment B, and the
    /// `debug_assert!` at the gate below).
    const MIXED: &[&str] = &["mixed"];
    /// `min`/`max`'s live signature at `PINNED_PHP` (8.5.8): variadic, `(total,
    /// required) = (2, 1)` — `min(mixed $value, mixed ...$values)`. Measured, not
    /// assumed; a variadic reports the *declared* parameters, not a call's.
    const ARITY_MIN_MAX: Option<(u32, u32)> = Some((2, 1));

    /// `curl_getinfo`'s live signature at `PINNED_PHP` (8.5.9): two parameters,
    /// one required — `curl_getinfo(CurlHandle $handle, ?int $option = null)`.
    /// Measured, not assumed (issue #594), the same discipline [`ARITY_MIN_MAX`]
    /// documents.
    const ARITY_CURL_GETINFO: Option<(u32, u32)> = Some((2, 1));

    /// `filter_var`'s live signature at `PINNED_PHP` (8.5.9): three parameters,
    /// one required — `filter_var(mixed $value, int $filter = FILTER_DEFAULT,
    /// array|int $options = 0)`. Measured (issue #597), and load-bearing twice
    /// over: the rule reads all three POSITIONALLY, and the declaration it rides
    /// on is a bare `mixed`, so Amendment B's second leg is the only thing
    /// countersigning it.
    const ARITY_FILTER_VAR: Option<(u32, u32)> = Some((3, 1));

    /// `array_key_exists`/`key_exists`: `bool`.
    const BOOL: &[&str] = &["bool"];
    /// The pair's live signature at `PINNED_PHP` (8.5.8): two parameters, both
    /// required — `array_key_exists(mixed $key, array $array)`. Measured, not
    /// assumed. The arm reads its arguments POSITIONALLY, with the subject at
    /// index 1, so a php-src signature that grew a parameter in front of the
    /// array would make the read stale while the `bool` declaration still held.
    const ARITY_KEY_EXISTS: Option<(u32, u32)> = Some((2, 2));

    /// `abs`: the two-member scalar union, either rendering order. Unlike the
    /// `mixed` pins above this one is a real bound — [`transfer_envelope_admits`]
    /// checks the rule's output against it extensionally.
    const INT_OR_FLOAT: &[&str] = &["int|float", "float|int"];
    /// `abs`'s live signature at `PINNED_PHP` (8.5.9): one parameter, required —
    /// `abs(int|float $num)`. Measured, and load-bearing: the rule reads argument
    /// **0** positionally, so a php-src signature that grew a parameter in front
    /// of `$num` would leave the read stale while `int|float` still held.
    const ARITY_ABS: Option<(u32, u32)> = Some((1, 1));
    /// `pow`: `object|int|float`, in the order the engine renders it. The
    /// `object` arm is `GMP` (and any extension overloading `**`), which is
    /// exactly why [`pow_transfer`] must prove both operands are NOT objects
    /// before it answers.
    const POW: &[&str] = &["object|int|float", "int|float|object"];
    /// `pow`'s live signature at `PINNED_PHP` (8.5.9): two parameters, both
    /// required — `pow(mixed $num, mixed $exponent)`.
    const ARITY_POW: Option<(u32, u32)> = Some((2, 2));

    let lower = name.to_ascii_lowercase();
    let (out, declared, arity): (Fact, &[&str], Option<(u32, u32)>) = match lower.as_str() {
        // `array_key_exists($key, $array)` in VALUE position (issue #343). The
        // pair has narrowed a shape's presence as a GUARD since ADR-0062 §4, and
        // answered nothing sharper than `bool` when its result was read — against
        // a fact that carries the answer.
        //
        // The subject is argument **1**, which is why this lives here rather than
        // with the shape-projection family: that rung binds a single subject at
        // argument 0 by construction.
        "array_key_exists" | "key_exists" => {
            (key_exists_verdict(cx, folder, args, env, store)?, BOOL, ARITY_KEY_EXISTS)
        }
        "explode" => (explode_transfer(cx, folder, args, env, store)?, ARRAY, None),
        "range" => (range_transfer(cx, folder, args, env, store)?, ARRAY, None),
        "preg_replace" => {
            (preg_replace_transfer(cx, folder, args, env, store)?, PREG_REPLACE, None)
        }
        "var_export" => {
            (var_export_transfer(cx, folder, args, env, store)?, NULLABLE_STRING, None)
        }
        "min" | "max" => {
            (min_max_transfer(cx, folder, &lower, args, env, store)?, MIXED, ARITY_MIN_MAX)
        }
        "curl_getinfo" => (curl_getinfo_transfer(args)?, MIXED, ARITY_CURL_GETINFO),
        // `filter_var` ONLY (issue #597) — not `filter_var_array`, not
        // `filter_input*`: those answer arrays, and an array result is a
        // different rule's shape, not this scalar rung's fact.
        "filter_var" => {
            (filter_var_transfer(cx, folder, args, env, store)?, MIXED, ARITY_FILTER_VAR)
        }
        "abs" => (abs_transfer(cx, folder, args, env, store)?, INT_OR_FLOAT, ARITY_ABS),
        "pow" => (pow_transfer(cx, folder, args, env, store)?, POW, ARITY_POW),
        // `json_decode` is the batch's recorded DECLINE, not an omission: its
        // reflected declaration is bare `mixed`, and the soundest envelope any
        // flag combination admits — `$assoc = true` still allows
        // `array|int|float|string|bool|null` — is a six-base union the four-layer
        // domain has no single `Fact` for (`envelope_fact`'s multi-base `None`).
        // A rule that cannot state its own answer declines (ADR-0061 §1).
        //
        // The string-predicate transfer family (issue #77) is keyed inside its own
        // table rather than spelled out here — ~25 names sharing one `string` pin
        // (`strlen`: `int`), so a per-name arm would be a transcription of that
        // table with nothing added.
        other => {
            let (fact, declared) = str_pred_transfer(cx, folder, other, args, env, store)?;
            (fact, declared, None)
        }
    };
    // ADR-0064 Amendment B, enforced structurally at this rung too. `min`/`max`
    // declare a bare `mixed`, so the rung grew the same arity second leg the S7
    // rung carries: name `mixed` and you must pin the signature the rule was
    // written against.
    debug_assert!(
        !declared.iter().any(|d| d.eq_ignore_ascii_case("mixed")) || arity.is_some(),
        "{lower}: a `mixed` declaration pin requires the arity second leg"
    );
    if !transfer_envelope_admits(cx, folder, name, declared, arity, &out) {
        return None;
    }
    // ADR-0061 §3's derivation clause, over the facts the rules actually read: an
    // argument answered from the declared arm lane contributes that lane's own
    // (`Asserted`) stratum, and everything else contributes what it always did.
    let stratum = args.iter().fold(Stratum::Verified, |acc, v| {
        acc.min(
            transfer_arg_known(cx, folder, v, env, store)
                .map_or_else(|| value_stratum(v, env, store), |(_, s)| s),
        )
    });
    Some((out, stratum))
}

/// The fact one call argument carries: a bound variable's env fact, else the
/// literal (or fold-resolved) value's own Singleton. The seam's whole extension
/// beyond the single-shape-argument pattern.
pub(crate) fn transfer_arg_fact(
    cx: &Cx,
    folder: &mut dyn Folder,
    value: &ArgValue,
    env: &HashMap<String, Known>,
    store: Option<&Store>,
) -> Option<Fact> {
    transfer_arg_known(cx, folder, value, env, store).map(|(fact, _)| fact)
}

/// The same fact **with the stratum it enters at** — the two are computed together
/// because the second leg below can change both at once.
///
/// The env fact is the first answer. Where it is only the *envelope*
/// (`Fact::General`, what a native `string $s` parameter seeds), the **declared
/// contract arm lane** is consulted instead: `@param non-empty-string $s` on a
/// natively-typed `string` parameter lives there and nowhere else, since
/// ADR-0052 §9's entry-state seeding puts only *array* arms into the value lane
/// (A-G9's corollary).
///
/// This is the narrowest possible widening of that seam: nothing about entry
/// state moves, only a rule that asked for this argument sees it. The arm's own
/// stratum comes with it, so a docblock-*claimed* refinement enters `Asserted`
/// and can never premise a proof-layer finding (ADR-0061 §3). An arm lane
/// lowering to no better than the envelope contributes nothing.
pub(crate) fn transfer_arg_known(
    cx: &Cx,
    folder: &mut dyn Folder,
    value: &ArgValue,
    env: &HashMap<String, Known>,
    store: Option<&Store>,
) -> Option<(Fact, Stratum)> {
    if let ArgValue::Var(v) = value {
        let known = env.get(v);
        let env_fact = known.and_then(|k| k.fact.clone());
        let env_stratum = known.map_or(Stratum::Verified, |k| k.stratum);
        if let Some(fact) = env_fact.clone()
            && !matches!(fact, Fact::General { .. })
        {
            return Some((fact, env_stratum));
        }
        if let Some(arms) = store.and_then(|s| s.contract_arms(v))
            && let Some((fact, stratum)) = declared_arm_known(arms)
            && !matches!(fact, Fact::General { .. })
        {
            return Some((fact, stratum));
        }
        return env_fact.map(|f| (f, env_stratum));
    }
    // An array literal the value path cannot prove whole still denotes a fact
    // (issue #327), and a rule reading it as an ARGUMENT should see the same one
    // an assignment would bind — otherwise a sibling argument like
    // `array_combine(['a', 'b'], [1, $x])` reads nothing at all.
    if let ArgValue::Array(items) = value
        && let Some((lit, strat)) = cx
            .resolve_literal_strat(value, env, false, folder)
            .and_then(|(l, s)| Some((singleton_fact(&l, cx.php_minor)?, s)))
            .or_else(|| array_literal_fact(cx, folder, items, env, false, store))
    {
        return Some((lit, strat.min(value_stratum(value, env, store))));
    }
    let lit = cx.resolve_literal(value, env, false, folder)?;
    Some((singleton_fact(&lit, cx.php_minor)?, value_stratum(value, env, store)))
}

/// The declared contract lane as ONE fact, with the weakest stratum any arm of it
/// carries. Every arm must lower ([`steins_contract::to_fact`]) and the domain must
/// be able to join them — `'foo'|'bar'` becomes a `OneOf`, `int|string` declines,
/// which is the same honest floor the value-slot lowering takes everywhere else.
fn declared_arm_known(arms: &[ContractArm]) -> Option<(Fact, Stratum)> {
    let mut acc: Option<Fact> = None;
    let mut stratum = Stratum::Verified;
    for arm in arms {
        let f = steins_contract::to_fact(&arm.ty)?;
        stratum = stratum.min(arm.stratum);
        acc = Some(match acc {
            None => f,
            Some(prev) => prev.join(&f)?,
        });
    }
    Some((acc?, stratum))
}

/// `explode($separator, $string)` → **`non-empty-list<string>`**.
///
/// PHP 8 removed `explode`'s `false` arm (the empty separator became a
/// `ValueError`), and the split of *any* string on a non-empty separator has at
/// least one piece — `explode(',', '')` is `['']`, not `[]`. Witnesses at
/// `PINNED_PHP` (8.5.8): `explode(',', '')` → `array(1){ [0]=> "" }`;
/// `explode('', 'abc')` → `ValueError: explode(): Argument #1 ($separator) must
/// not be empty`.
///
/// **Two declines, both load-bearing.** An empty (or not-known-non-empty)
/// separator declines — the `ValueError` form has no return value to describe.
/// The three-argument form declines **because `$limit` breaks non-emptiness
/// `array_key_exists($key, $array)` read as a VALUE (issue #343): the verdict the
/// subject's shape already carries, or `None` to keep today's `bool`.
///
/// The rule, and the reason each leg is sound:
///
/// * a **required** field is present in every realization the shape admits, so
///   the answer is `true` — whether the presence was declared or witnessed;
/// * a field the shape proves **absent** (post-`unset`, the false branch of a
///   guard) answers `false` for the same reason in reverse;
/// * an undeclared key under a **sealed** tail answers `false`: sealed is
///   exactly the claim that no undeclared key may be present;
/// * an **optional** field and an undeclared key under an **unsealed** tail keep
///   `bool`. Both are genuinely undecided, and `Maybe` is the honest answer the
///   arm lane already gives elsewhere.
///
/// The key must be a concrete literal. A key that is itself a variable names no
/// field to look up, and guessing from its type is a different rung.
///
/// **Stratum is the caller's business and it is already handled**: a shape read
/// out of a `@param array{…}` enters `Asserted`, and `derivation_stratum` carries
/// that through, so ADR-0062 A-G9's corollary keeps this fact out of every
/// proof-layer premise exactly as it keeps every other shape-derived fact out.
///
/// `isset($array[$key])` is the same question one step stronger — it additionally
/// needs the field's value provably non-null — and is NOT answered here: `isset`
/// is a construct whose value lowering is `ArgValue::Other`, so it never reaches
/// a call seam at all. That is its own slice.
fn key_exists_verdict(
    cx: &Cx,
    folder: &mut dyn Folder,
    args: &[ArgValue],
    env: &HashMap<String, Known>,
    store: Option<&Store>,
) -> Option<Fact> {
    let [key_arg, subject] = args else { return None };
    // The array-key cast is PHP's, not ours: `$a[5]` and `$a["5"]` are one key,
    // and `offset_key_of` is the same primitive the read and write sides use.
    let key = crate::offsets::offset_key_of(&val_of(key_arg, cx.php_minor)?)?;
    let Fact::Shape { shape, nullable: false } =
        transfer_arg_fact(cx, folder, subject, env, store)?
    else {
        return None;
    };
    let verdict = match shape.field(&key).map(|(_, presence, _)| *presence) {
        Some(Presence::Required { .. }) => true,
        Some(Presence::Absent) => false,
        Some(Presence::Optional) => return None,
        None => match shape.tail {
            Tail::Sealed => false,
            Tail::Unsealed { .. } => return None,
        },
    };
    Some(Fact::Singleton(Val::Bool(verdict)))
}

/// outright**: `explode(',', 'a,b,c', -5)` returns `array(0){}` at 8.5.8.
fn explode_transfer(
    cx: &Cx,
    folder: &mut dyn Folder,
    args: &[ArgValue],
    env: &HashMap<String, Known>,
    store: Option<&Store>,
) -> Option<Fact> {
    // Exactly two arguments — see the `$limit` witness above.
    let [sep, _string] = args else { return None };
    let sep = transfer_arg_fact(cx, folder, sep, env, store)?;
    // The `$string` argument is deliberately unread: it is declared `string`, so
    // anything reaching the body is one (or was coerced to one), and every string
    // splits to at least one string piece.
    fact_is_non_empty_string(&sep)
        .then(|| list_transfer_fact(true, Some(Fact::General { base: Base::String, nullable: false })))
}

/// `range($start, $end [, $step])` → **`non-empty-list<int>`** for integral
/// bounds and step, **`non-empty-list<mixed>`** otherwise.
///
/// The unconditional half is the stronger claim: PHP's `range` always returns a
/// *packed* array (a list) with at least one entry, since equal bounds still
/// produce one (`range(1, 1)` → `[1]`, witnessed at 8.5.8). No argument shape
/// changes that: `range(3, 1)` is the three-element descending list, and
/// `range('a', 'c')` is `['a', 'b', 'c']`. Every input PHP 8.3+ refuses (`$step`
/// of `0`, too large, or negative on an increasing range — all `ValueError` at
/// 8.5.8) produces no value at all, so non-emptiness survives vacuously.
///
/// The element bound is **narrower than "any float involved"** on purpose: PHP
/// 8.3's saner-`range` makes `range(1, 3, 1.0)` an *int* array, while
/// `range(1, 2, 0.5)` and `range(1.0, 3.0)` are float arrays. Rather than encode
/// that fractional-part rule, the transfer claims `int` only when every bound
/// and step are known integral and leaves the element unknown otherwise.
///
/// The integrality test is [`fact_is_int`], which also passes a *nullable* int —
/// sound vacuously, since `range(null, 3)` is a `TypeError` at 8.3+.
fn range_transfer(
    cx: &Cx,
    folder: &mut dyn Folder,
    args: &[ArgValue],
    env: &HashMap<String, Known>,
    store: Option<&Store>,
) -> Option<Fact> {
    // `range` accepts two or three arguments; any other arity is an
    // `ArgumentCountError`, and the seam refuses to describe a call PHP rejects.
    if !(2..=3).contains(&args.len()) {
        return None;
    }
    let mut integral = true;
    for a in args {
        // No short-circuit: every argument's fact is read, so the decision is a
        // function of the whole call rather than of evaluation order.
        integral &= transfer_arg_fact(cx, folder, a, env, store).as_ref().is_some_and(fact_is_int);
    }
    Some(list_transfer_fact(
        true,
        integral.then_some(Fact::General { base: Base::Int, nullable: false }),
    ))
}

/// `preg_replace($pattern, $replacement, $subject, …)` → **`string|null`** for a
/// string subject, **`array|null`** for an array one.
///
/// The reflected declaration is the three-member `array|string|null`, which
/// `envelope_fact` cannot represent (multi-base), so `$subject`'s own base is
/// what splits it: `preg_replace(['/a/', '/b/'], 'z', 'ab')` is `'zz'`, a string,
/// despite the array `$pattern` (witnessed at 8.5.8).
///
/// The `null` arm is **kept on both sides**, deliberately. A string subject
/// genuinely returns `null` on a PCRE error (witnessed). Array-subject probes
/// returned `array(0){}` on the same errors, but "no probe produced null" is not
/// proof none can — ADR-0061 §2's ledger only balances one way: a
/// kept-but-impossible `null` costs one arm of precision, a dropped-but-possible
/// one is a false premise.
fn preg_replace_transfer(
    cx: &Cx,
    folder: &mut dyn Folder,
    args: &[ArgValue],
    env: &HashMap<String, Known>,
    store: Option<&Store>,
) -> Option<Fact> {
    if !(3..=5).contains(&args.len()) {
        return None;
    }
    let string_or_null = || Fact::General { base: Base::String, nullable: true };
    let array_or_null =
        || Fact::Shape { shape: Box::new(ShapeFact::plain_array()), nullable: true };
    match transfer_arg_fact(cx, folder, &args[2], env, store)? {
        Fact::Singleton(Val::Str(_)) => Some(string_or_null()),
        Fact::Singleton(Val::Array(_)) => Some(array_or_null()),
        Fact::OneOf(ref vals) if vals.iter().all(|v| matches!(v, Val::Str(_))) => {
            Some(string_or_null())
        }
        Fact::OneOf(ref vals) if vals.iter().all(|v| matches!(v, Val::Array(_))) => {
            Some(array_or_null())
        }
        // A nullable subject declines: `null` is a deprecation-plus-coercion in
        // weak mode and a `TypeError` in strict mode, and neither is a case this
        // rule was probed against.
        Fact::Refined { base: Base::String, nullable: false, .. }
        | Fact::General { base: Base::String, nullable: false } => Some(string_or_null()),
        Fact::Shape { nullable: false, .. } => Some(array_or_null()),
        _ => None,
    }
}

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
fn min_max_transfer(
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
fn abs_transfer(
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
fn abs_union(arms: &[(Base, Option<Refinement>)]) -> Option<Fact> {
    let mut out: Vec<(Base, Option<Refinement>)> = Vec::with_capacity(arms.len() + 1);
    let overflow = |out: &mut Vec<(Base, Option<Refinement>)>| {
        out.push((Base::Int, Some(Refinement::Int(IntRange::NON_NEGATIVE))));
        out.push((Base::Float, None));
    };
    for (base, refinement) in arms {
        match (base, refinement) {
            (Base::Float, _) => out.push((Base::Float, None)),
            (Base::Int, Some(Refinement::Int(r))) => match abs_range(*r) {
                Some(a) => out.push((Base::Int, Some(Refinement::Int(a)))),
                None => overflow(&mut out),
            },
            (Base::Int, None) => overflow(&mut out),
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
fn pow_transfer(
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
    let int_or_float = || Fact::union(vec![(Base::Int, None), (Base::Float, None)], false);
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

/// `var_export($value, true)` → **`string`**.
///
/// The reflected declaration is `?string`, and the `null` half is precisely the
/// `$return = false` behavior: the export is *printed* and nothing is returned.
/// A literal `true` flag strips the null arm, and nothing else about `$value`
/// matters (`var_export(null, true)` is the four-character string `'NULL'`, not
/// `null`; witnessed at 8.5.8).
///
/// The one-argument and literal-`false` forms decline: the reflected `?string`
/// envelope already describes them exactly (ADR-0061 §3's replace-if-weaker
/// corollary).
fn var_export_transfer(
    cx: &Cx,
    folder: &mut dyn Folder,
    args: &[ArgValue],
    env: &HashMap<String, Known>,
    store: Option<&Store>,
) -> Option<Fact> {
    let [_value, flag] = args else { return None };
    let flag = transfer_arg_fact(cx, folder, flag, env, store)?;
    (flag == Fact::Singleton(Val::Bool(true)))
        .then_some(Fact::General { base: Base::String, nullable: false })
}

/// `curl_getinfo($handle, CURLINFO_X)` → **the fixed type php.net documents for
/// `CURLINFO_X`**, on the `$option` argument being a recognized `CURLINFO_*`
/// global constant NAME (issue #594). The constant's VALUE is never read: by
/// design (issue #168) an `ArgValue::GlobalConst` carries no proven value, and
/// this table has no use for one anyway — the name alone is the whole key.
///
/// # Method
///
/// Every row is the `gettype()` PHP itself reports for
/// `curl_getinfo($h, CURLINFO_X)` on a `curl_init($url)` handle that is NEVER
/// `curl_exec`'d — no network is reached (ADR-0061 §4) — cross-checked against
/// php.net's `curl_getinfo` return-value table. Witnessed at `PINNED_PHP`
/// (8.5.9, linked libcurl 8.7.1).
///
/// This works because an **int- or float-typed** option coerces libcurl's
/// unset-field sentinel (`0`, or `-1` for the handful documented that way) at
/// the C level rather than ever answering PHP `false` — so a virgin handle
/// already carries the same `integer`/`double` `gettype()` the table below
/// claims for a live one. A **string-typed** option is not uniform: most
/// coalesce an unset C `NULL` to PHP `false` (the confirmed `T|false` rows,
/// below), but four coalesce it to `''` instead — verified because the probe
/// answers a real (non-`false`) string on the same untouched handle. Those four
/// are the `STRING` list's entire membership.
///
/// # The declines, each measured or reasoned from a primary source
///
/// * **Every `string|false`/`mixed`/array-shaped documented option**
///   (`CURLINFO_CONTENT_TYPE`, `CURLINFO_REDIRECT_URL`, `CURLINFO_HEADER_OUT`,
///   `CURLINFO_PRIVATE`, `CURLINFO_FTP_ENTRY_PATH`, `CURLINFO_REFERER`,
///   `CURLINFO_CAINFO`, `CURLINFO_CAPATH`, `CURLINFO_RTSP_SESSION_ID`, …) — no
///   `Fact` spells a two-base union, the same floor `min`/`json_decode` already
///   stand on. `CURLINFO_PRIVATE` specifically echoes back whatever
///   `CURLOPT_PRIVATE` was set to — `mixed` by construction, not by measurement.
/// * **`CURLINFO_SCHEME`** — php.net's own return-value table calls this
///   `string`, but the probe answers `bool(false)` on the untouched handle
///   exactly like the confirmed `T|false` rows above, unlike `STRING`'s four
///   members (which default to `''`). Trusting the documented word against the
///   grain of its own measurement is exactly the transcription ADR-0061 §4
///   forbids, so this stays unrecognized pending a stronger signal.
/// * **`CURLINFO_POSTTRANSFER_TIME_T`** — PHP 8.4.0 gates the *constant*, but
///   php.net's changelog additionally gates the *value* on cURL ≥ 8.10.0 (this
///   probe's libcurl 8.7.1 is older, yet still answers a plain `int` — an
///   artifact of this one binary, not a portable fact). A PHP-minor pin cannot
///   see the linked libcurl version a deployed project will run against, so
///   `int|false` — the nsrt-measured shape — is the honest floor.
/// * **Every list/array-shaped option** (`CURLINFO_CERTINFO`,
///   `CURLINFO_SSL_ENGINES`, `CURLINFO_COOKIELIST`) and **the zero- or
///   one-argument whole-array form** — out of this rung's scope; a shape-typed
///   result is a different rule (`shape_projection_fact`'s family), not this
///   scalar table's.
/// * **A non-constant, or unrecognized-name, `$option`** — the table has
///   nothing to key on.
fn curl_getinfo_transfer(args: &[ArgValue]) -> Option<Fact> {
    let [_handle, option] = args else { return None };
    let ArgValue::GlobalConst(r) = option else { return None };
    // Constants are case-sensitive (unlike PHP's function/class names), so the
    // match below is exact. A `Qualified`/`Relative` spelling never denotes the
    // global `CURLINFO_*` constant — the same `FullyQualified`/`Unqualified`
    // split `cond.rs`'s `PHP_VERSION_ID` identity check applies (issue #29).
    if !matches!(r.kind, RefKind::FullyQualified | RefKind::Unqualified) {
        return None;
    }

    /// The 37 constants documented (and probed) as a plain `int`.
    const INT: &[&str] = &[
        "CURLINFO_FILETIME",
        "CURLINFO_REDIRECT_COUNT",
        "CURLINFO_PRIMARY_PORT",
        "CURLINFO_LOCAL_PORT",
        "CURLINFO_HEADER_SIZE",
        "CURLINFO_REQUEST_SIZE",
        "CURLINFO_SSL_VERIFYRESULT",
        "CURLINFO_RESPONSE_CODE",
        "CURLINFO_HTTP_CODE",
        "CURLINFO_HTTP_CONNECTCODE",
        "CURLINFO_HTTPAUTH_AVAIL",
        "CURLINFO_PROXYAUTH_AVAIL",
        "CURLINFO_OS_ERRNO",
        "CURLINFO_NUM_CONNECTS",
        "CURLINFO_CONDITION_UNMET",
        "CURLINFO_RTSP_CLIENT_CSEQ",
        "CURLINFO_RTSP_CSEQ_RECV",
        "CURLINFO_RTSP_SERVER_CSEQ",
        // The PHP 7.3+ `_T` (microsecond-precision) family.
        "CURLINFO_CONTENT_LENGTH_DOWNLOAD_T",
        "CURLINFO_CONTENT_LENGTH_UPLOAD_T",
        "CURLINFO_HTTP_VERSION",
        "CURLINFO_PROTOCOL",
        "CURLINFO_PROXY_SSL_VERIFYRESULT",
        "CURLINFO_SIZE_DOWNLOAD_T",
        "CURLINFO_SIZE_UPLOAD_T",
        "CURLINFO_SPEED_DOWNLOAD_T",
        "CURLINFO_SPEED_UPLOAD_T",
        "CURLINFO_APPCONNECT_TIME_T",
        "CURLINFO_CONNECT_TIME_T",
        "CURLINFO_FILETIME_T",
        "CURLINFO_NAMELOOKUP_TIME_T",
        "CURLINFO_PRETRANSFER_TIME_T",
        "CURLINFO_REDIRECT_TIME_T",
        "CURLINFO_STARTTRANSFER_TIME_T",
        "CURLINFO_TOTAL_TIME_T",
        // PHP 8.2+.
        "CURLINFO_PROXY_ERROR",
        "CURLINFO_RETRY_AFTER",
    ];
    /// The 13 constants documented (and probed) as a plain `float`.
    const FLOAT: &[&str] = &[
        "CURLINFO_TOTAL_TIME",
        "CURLINFO_NAMELOOKUP_TIME",
        "CURLINFO_CONNECT_TIME",
        "CURLINFO_PRETRANSFER_TIME",
        "CURLINFO_STARTTRANSFER_TIME",
        "CURLINFO_REDIRECT_TIME",
        "CURLINFO_SIZE_UPLOAD",
        "CURLINFO_SIZE_DOWNLOAD",
        "CURLINFO_SPEED_DOWNLOAD",
        "CURLINFO_SPEED_UPLOAD",
        "CURLINFO_CONTENT_LENGTH_DOWNLOAD",
        "CURLINFO_CONTENT_LENGTH_UPLOAD",
        "CURLINFO_APPCONNECT_TIME",
    ];
    /// The 4 constants probed as an unconditional (never-`false`) `string`: each
    /// coalesces an unset field to `''`, not `false` — see the module doc above
    /// for the measurement that sets these apart from `CURLINFO_SCHEME`.
    const STRING: &[&str] = &[
        "CURLINFO_EFFECTIVE_URL",
        "CURLINFO_PRIMARY_IP",
        "CURLINFO_LOCAL_IP",
        "CURLINFO_EFFECTIVE_METHOD",
    ];

    let name = r.raw.as_str();
    if INT.contains(&name) {
        Some(Fact::General { base: Base::Int, nullable: false })
    } else if FLOAT.contains(&name) {
        Some(Fact::General { base: Base::Float, nullable: false })
    } else if STRING.contains(&name) {
        Some(Fact::General { base: Base::String, nullable: false })
    } else {
        None
    }
}

/// What one recognized `FILTER_*` filter constant does to a value, as the grid on
/// [`filter_var_transfer`] measures it. The constant NAME is the key; its value is
/// never read (issue #168), exactly as in [`curl_getinfo_transfer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FilterKind {
    /// `FILTER_DEFAULT` / `FILTER_UNSAFE_RAW` (one engine value, two names) under
    /// no string-modifying flag: the plain `(string)` cast, and it cannot fail
    /// for any value the four-layer domain denotes.
    Raw,
    /// Every `FILTER_SANITIZE_*`: a `string`, always — the output is rewritten, so
    /// nothing of the input's own predicates survives.
    Sanitize,
    /// `FILTER_VALIDATE_INT`.
    Int,
    /// `FILTER_VALIDATE_FLOAT`.
    Float,
    /// `FILTER_VALIDATE_BOOL` / `FILTER_VALIDATE_BOOLEAN`.
    Bool,
    /// The four validators whose every success value is a `non-falsy-string`.
    NonFalsyString,
    /// `FILTER_VALIDATE_DOMAIN`, whose success value is a plain `string` — `''`
    /// and `'0'` both validate (measured; see [`filter_var_transfer`]).
    PlainString,
}

/// `filter_var($value, FILTER_X, $flags)` → **the fact the (filter × flags × input)
/// grid below proves**, for the combinations the four-layer domain can spell
/// (issue #597). Every other combination declines.
///
/// # The expressibility rule, which is what shapes the whole rung
///
/// `filter_var` answers `success | failure`, and the failure value is `false` by
/// default, `null` under `FILTER_NULL_ON_FAILURE`. `T|null` is a nullable fact and
/// spells fine. **`T|false` has no `Fact` spelling unless `T` is `bool`** — the
/// domain's [`Refinement`] axis is `Str`/`Int` only, so a `Fact::Union` of an
/// `int` arm and a `bool` arm would be `int|bool`, a widening that claims `true`
/// is possible when it is not. Those outcomes therefore decline outright rather
/// than widen (ADR-0061 §1). The `T|false` half of this callee waits on issue
/// #600's domain work; nothing here anticipates it.
///
/// So exactly three things are winnable, and the rung wins all three:
///
/// 1. **Every `FILTER_NULL_ON_FAILURE` combination** — `T|null` for whatever `T`
///    the filter's success type is.
/// 2. **`FILTER_VALIDATE_BOOL` with the default failure value** — `bool|false` IS
///    `bool`, because `false` is *both* the failure value and a valid parse of
///    `'false'`/`'off'`/`'no'`/`''`. Measured, not reasoned: the probe below shows
///    `filter_var('off', FILTER_VALIDATE_BOOL)` and
///    `filter_var('maybe', FILTER_VALIDATE_BOOL)` both answering `false`.
/// 3. **Success-proven inputs** — where the input fact's WHOLE domain validates,
///    the failure arm vanishes and the plain success type binds, with or without
///    `FILTER_NULL_ON_FAILURE`.
///
/// # The grid, every cell `php -r`-measured at `PINNED_PHP` (8.5.9)
///
/// | filter (constant names) | success type | success-proven inputs |
/// | --- | --- | --- |
/// | `FILTER_DEFAULT` `FILTER_UNSAFE_RAW` | the `(string)` cast of the input | **every** fact-denoted input |
/// | `FILTER_SANITIZE_*` (8 names) | `string` | every fact-denoted input |
/// | `FILTER_VALIDATE_INT` | `int` | an `int` input — the identity |
/// | `FILTER_VALIDATE_FLOAT` | `float` | an `int` input — the `(float)` cast |
/// | `FILTER_VALIDATE_BOOL` `FILTER_VALIDATE_BOOLEAN` | `bool` | a `bool` input — the identity |
/// | `FILTER_VALIDATE_EMAIL` `_URL` `_IP` `_MAC` | `non-falsy-string` | — |
/// | `FILTER_VALIDATE_DOMAIN` | `string` | — |
///
/// The witnesses behind the cells that are not the obvious ones:
///
/// * **`FILTER_DEFAULT` is the `(string)` cast, exactly.** Over the cross product
///   of 15 values (`''`, `'0'`, `"a\x01b`&"`, the `int` edges, both bools, `null`,
///   `17.0`, `1e-50`) and all 14 accepted flags, both names, `filter_var` and
///   `strval` never disagree. So the rung answers through the domain's own cast
///   grid ([`php_cast_fact`]) rather than a second copy of it, which is where the
///   `int` → `decimal-int-string` and `bool` → `''|'1'` rows come from.
/// * **A `float` input under `FILTER_VALIDATE_FLOAT` is NOT proven, and NOT the
///   identity.** `filter_var(NAN, …)`, `INF` and `-INF` all answer `false` (the
///   value is coerced to a string first — the engine even emits "unexpected NAN
///   value was coerced to string"), and `-0.0` comes back as `+0.0`. Upstream
///   PHPStan's `filter-var.php` asserts a flat `float` for a `float` input; the
///   probe refutes it, so that row is deliberately NOT won (the issue #40 / #594
///   precedent — when the fixture and the measurement disagree, the measurement
///   wins and the row stays `unknown`).
/// * **`FILTER_VALIDATE_DOMAIN`'s success value is a plain `string`.**
///   `filter_var('', FILTER_VALIDATE_DOMAIN)` is `''` and
///   `filter_var('0', …)` is `'0'` — under every accepted flag. Upstream calls
///   this `non-empty-string`; claiming that here would be unsound, so the rung
///   states `string` and the fixture rows stay unwon.
/// * **The four `non-falsy-string` validators.** `''` and `'0'` are PHP's only
///   falsy strings, and neither validates as an email, URL, IP or MAC under any
///   accepted flag (measured over the whole flag list). The shortest successes
///   are `'::'`, `'a://b'`, `'a@b.c'` and a 17-byte MAC.
/// * **The sanitizers never fail on a scalar.** Over the same 15 values and 14
///   flags, all eight `FILTER_SANITIZE_*` names answer a `string` every time —
///   `false` appears only for an `array` or `object` input, neither of which the
///   value domain denotes. Their success type is nonetheless flat `string`:
///   `filter_var('ä', FILTER_SANITIZE_EMAIL)` is `''`, so no input predicate
///   survives.
///
/// # The declines, each for a stated reason
///
/// * **A dynamic filter argument, or an unrecognized constant name** — the table
///   has nothing to key on. `FILTER_CALLBACK` is unrecognized by construction: its
///   result is whatever the userland callback returns.
/// * **`FILTER_VALIDATE_REGEXP`** — it needs a `'regexp'` entry in the options
///   array, and an options array is itself a decline (below), so every call this
///   rung could otherwise answer raises `ValueError: filter_var(): "regexp" option
///   is missing` at 8.5.9 and returns nothing at all. A rule whose every reachable
///   call throws states nothing.
/// * **Any options ARRAY carrying a key other than `'flags'`** — `'options' =>
///   ['default' => $x]` REPLACES the failure value with an arbitrary one, which
///   moves the answer clean off this grid; `'min_range'`/`'max_range'` narrow the
///   success arm this rung does not read. One unrecognized key declines the whole
///   literal rather than being ignored.
/// * **A flag outside the accepted list** — `FILTER_REQUIRE_ARRAY` /
///   `FILTER_FORCE_ARRAY` make the result an array (the `filter_var_array` slice's
///   business, not this scalar rung's) and `FILTER_REQUIRE_SCALAR` rides with
///   them; `FILTER_FLAG_STRIP_LOW` / `_STRIP_HIGH` / `_STRIP_BACKTICK` /
///   `_ENCODE_LOW` / `_ENCODE_HIGH` / `_ENCODE_AMP` / `_NO_ENCODE_QUOTES` rewrite
///   the string, so `FILTER_DEFAULT` stops being the identity;
///   `FILTER_FLAG_EMPTY_STRING_NULL` turns `''` into `null` on the SUCCESS path
///   (measured), which no cell above accounts for; and `FILTER_THROW_ON_FAILURE`
///   is a PHP 8.5 constant whose whole point is to delete the failure arm — a
///   sharper answer than anything here, but one that needs a PHP-minor gate this
///   rung does not carry.
/// * **A flags argument that is not a literal** — a variable (`$nullFilter =
///   \FILTER_NULL_ON_FAILURE`) carries no proven value (issue #168), and a `|`
///   combination of two constants lowers to [`ArgValue::Other`] because
///   [`steins_syntax::ValueOp`] represents comparisons only. Both are recorded
///   declines, not oversights: `filterVar.php` spends two rows per filter block on
///   exactly these spellings.
/// * **A bare non-zero int literal in the flags position** — the rung keys on
///   constant NAMES, so `filter_var($x, FILTER_VALIDATE_INT, 134217728)` is not
///   recognized as `FILTER_NULL_ON_FAILURE`. A literal `0` is the documented
///   "no flags" and is accepted.
///
/// [`php_cast_fact`]: crate::coerce::php_cast_fact
fn filter_var_transfer(
    cx: &Cx,
    folder: &mut dyn Folder,
    args: &[ArgValue],
    env: &HashMap<String, Known>,
    store: Option<&Store>,
) -> Option<Fact> {
    let (value, filter, options) = match args {
        [value] => (value, None, None),
        [value, filter] => (value, Some(filter), None),
        [value, filter, options] => (value, Some(filter), Some(options)),
        _ => return None,
    };
    // An absent filter argument is `FILTER_DEFAULT` (php.net's own default).
    let kind = match filter {
        None => FilterKind::Raw,
        Some(v) => filter_kind(v)?,
    };
    let null_on_failure = filter_var_flags(options)?;
    let input = transfer_arg_fact(cx, folder, value, env, store);
    let (success, proven) = filter_success(kind, input.as_ref());
    if proven {
        return Some(success);
    }
    if null_on_failure {
        return crate::fact_admitting_null(&success);
    }
    // The failure value is `false`, and `bool|false` IS `bool` — the one base for
    // which the union has a `Fact`. Everything else declines (the doc's
    // expressibility rule).
    (kind == FilterKind::Bool).then_some(success)
}

/// The success type a filter produces, and whether the input fact PROVES the call
/// takes that path — the two halves [`filter_var_transfer`] combines.
///
/// A `None` input is "nothing known", which is never proven and always falls to
/// the filter's own general success type.
fn filter_success(kind: FilterKind, input: Option<&Fact>) -> (Fact, bool) {
    let general = |base| (Fact::General { base, nullable: false }, false);
    match kind {
        // The `(string)` cast, through the domain's own grid. A `Fact::Shape`
        // input declines there (PHP writes `'Array'` with an `E_WARNING`), which
        // is exactly the input class `filter_var` answers `false` for anyway.
        FilterKind::Raw => match input.and_then(|f| php_cast_fact(f, CastTarget::String)) {
            Some(cast) => (cast, true),
            None => general(Base::String),
        },
        // A sanitizer rewrites its input, so only the *totality* survives: any
        // value the domain denotes is a scalar or `null`, and every one of those
        // sanitizes to a string.
        FilterKind::Sanitize => match input {
            Some(f) if !matches!(f, Fact::Shape { .. }) => {
                (Fact::General { base: Base::String, nullable: false }, true)
            }
            _ => general(Base::String),
        },
        // `filter_var($int, FILTER_VALIDATE_INT)` is the identity, over the whole
        // int range including both edges — so the input's own refinement rides
        // through (`int<0, 9>` stays `int<0, 9>`).
        FilterKind::Int => match input {
            Some(f) if fact_only_base(f, Base::Int) => (f.clone(), true),
            _ => general(Base::Int),
        },
        // An `int` input always validates as a float, and the value is the plain
        // `(float)` cast. A `float` input is NOT proven — see the `NAN` row on
        // [`filter_var_transfer`].
        FilterKind::Float => match input {
            Some(f) if fact_only_base(f, Base::Int) => {
                php_cast_fact(f, CastTarget::Float).map_or_else(|| general(Base::Float), |c| (c, true))
            }
            _ => general(Base::Float),
        },
        FilterKind::Bool => match input {
            Some(f) if fact_only_base(f, Base::Bool) => (f.clone(), true),
            _ => general(Base::Bool),
        },
        FilterKind::NonFalsyString => (
            Fact::refined(Base::String, Refinement::Str(StrPreds::NON_FALSY.close()), false),
            false,
        ),
        FilterKind::PlainString => general(Base::String),
    }
}

/// Does this fact admit ONLY values of `base` — no `null`, no second base, no
/// array? The premise every success-proven row on [`filter_var_transfer`] needs:
/// a `?int` is not an `int` input, because `filter_var(null, FILTER_VALIDATE_INT)`
/// is `false`.
fn fact_only_base(f: &Fact, base: Base) -> bool {
    match f {
        Fact::Singleton(v) => v.base() == Some(base),
        Fact::OneOf(vals) => vals.iter().all(|v| v.base() == Some(base)),
        Fact::Refined { base: b, nullable: false, .. } | Fact::General { base: b, nullable: false } => {
            *b == base
        }
        _ => false,
    }
}

/// The [`FilterKind`] a filter-argument expression names, or `None` for anything
/// this rung cannot key on.
///
/// Constants are case-sensitive (unlike PHP's function and class names), so the
/// match is exact. A `Qualified`/`Relative` spelling never denotes the global
/// `FILTER_*` constant — the same `FullyQualified`/`Unqualified` split
/// [`curl_getinfo_transfer`] applies.
fn filter_kind(value: &ArgValue) -> Option<FilterKind> {
    let ArgValue::GlobalConst(r) = value else { return None };
    if !matches!(r.kind, RefKind::FullyQualified | RefKind::Unqualified) {
        return None;
    }
    match r.raw.as_str() {
        "FILTER_DEFAULT" | "FILTER_UNSAFE_RAW" => Some(FilterKind::Raw),
        "FILTER_SANITIZE_EMAIL"
        | "FILTER_SANITIZE_URL"
        | "FILTER_SANITIZE_ENCODED"
        | "FILTER_SANITIZE_SPECIAL_CHARS"
        | "FILTER_SANITIZE_FULL_SPECIAL_CHARS"
        | "FILTER_SANITIZE_NUMBER_INT"
        | "FILTER_SANITIZE_NUMBER_FLOAT"
        | "FILTER_SANITIZE_ADD_SLASHES"
        // `FILTER_SANITIZE_STRING`/`FILTER_SANITIZE_STRIPPED` are deprecated
        // since 8.1 and still behave: the deprecation is on the CONSTANT, and
        // the measured answer is a `string` like the rest of the family.
        | "FILTER_SANITIZE_STRING"
        | "FILTER_SANITIZE_STRIPPED" => Some(FilterKind::Sanitize),
        "FILTER_VALIDATE_INT" => Some(FilterKind::Int),
        "FILTER_VALIDATE_FLOAT" => Some(FilterKind::Float),
        "FILTER_VALIDATE_BOOL" | "FILTER_VALIDATE_BOOLEAN" => Some(FilterKind::Bool),
        "FILTER_VALIDATE_EMAIL" | "FILTER_VALIDATE_URL" | "FILTER_VALIDATE_IP"
        | "FILTER_VALIDATE_MAC" => Some(FilterKind::NonFalsyString),
        "FILTER_VALIDATE_DOMAIN" => Some(FilterKind::PlainString),
        _ => None,
    }
}

/// Whether the third argument sets `FILTER_NULL_ON_FAILURE`, or `None` when it is
/// a spelling this rung refuses (which declines the whole rule — a flag it cannot
/// read may be `FILTER_FORCE_ARRAY`).
///
/// Three accepted shapes, and only three: **absent**, a **literal `0`** or a
/// single recognized flag **constant**, and an **array literal** whose only key is
/// a literal `'flags'` holding one of those. See [`filter_var_transfer`] for why
/// every other spelling — a variable, a `|` combination, a non-zero int literal,
/// an `'options'` key — is refused.
fn filter_var_flags(value: Option<&ArgValue>) -> Option<bool> {
    let Some(value) = value else { return Some(false) };
    if let ArgValue::Array(items) = value {
        let mut flags = false;
        for (key, item) in items {
            let ArrayKey::Str(k) = key else { return None };
            if k.as_str() != Some("flags") {
                return None;
            }
            flags = filter_flag_bit(item)?;
        }
        return Some(flags);
    }
    filter_flag_bit(value)
}

/// One flag EXPRESSION: `true` for `FILTER_NULL_ON_FAILURE`, `false` for a literal
/// `0` or an accepted type-neutral flag, `None` for everything else.
///
/// The accepted list is the flags that restrict which inputs *validate* without
/// touching the result's type — measured no-ops for every cell of the grid on
/// [`filter_var_transfer`]. `FILTER_FLAG_HOSTNAME`, `_IPV4` and `_EMAIL_UNICODE`
/// share one engine value; keying on names rather than values costs nothing here,
/// since all three are type-neutral.
fn filter_flag_bit(value: &ArgValue) -> Option<bool> {
    if let ArgValue::Int(0) = value {
        return Some(false);
    }
    let ArgValue::GlobalConst(r) = value else { return None };
    if !matches!(r.kind, RefKind::FullyQualified | RefKind::Unqualified) {
        return None;
    }
    match r.raw.as_str() {
        "FILTER_NULL_ON_FAILURE" => Some(true),
        "FILTER_FLAG_NONE"
        | "FILTER_FLAG_ALLOW_OCTAL"
        | "FILTER_FLAG_ALLOW_HEX"
        | "FILTER_FLAG_ALLOW_FRACTION"
        | "FILTER_FLAG_ALLOW_THOUSAND"
        | "FILTER_FLAG_ALLOW_SCIENTIFIC"
        | "FILTER_FLAG_IPV4"
        | "FILTER_FLAG_IPV6"
        | "FILTER_FLAG_HOSTNAME"
        | "FILTER_FLAG_EMAIL_UNICODE"
        | "FILTER_FLAG_NO_PRIV_RANGE"
        | "FILTER_FLAG_NO_RES_RANGE"
        | "FILTER_FLAG_GLOBAL_RANGE"
        | "FILTER_FLAG_PATH_REQUIRED"
        | "FILTER_FLAG_QUERY_REQUIRED" => Some(false),
        _ => None,
    }
}

/// **The string-predicate transfers** (issue #77) — the residual half of the names
/// whose constant half the fold lane already owns.
///
/// A string builtin on a *known constant* folds to a Singleton one rung up
/// (ADR-0028). On a string the walk knows only *predicates* about, this table
/// answers one question per name: which [`StrPreds`](steins_domain::StrPreds) bits
/// survive the call, and which does the call establish on its own?
///
/// ```text
/// out = (preds(arg0) ∩ KEEP(name, args)) ∪ FORCE(name, args)
/// ```
///
/// `out == ∅` means **decline**: an empty summary is exactly the reflected `string`
/// envelope, and restating it would put a stratum-carrying fact where a `Verified`
/// one already stands (ADR-0061 §3's replace-if-weaker corollary). `strlen` is the
/// one member whose output is not a string.
///
/// Casing, probed at `PINNED_PHP` (8.5.8): `LOWERCASE` is "no ASCII uppercase
/// byte", since PHP 8.2+ made both case functions locale-insensitive and byte-wise.
/// `strtolower`/`strtoupper` **force** their bit for *any* input including a
/// factless one (`strtolower('ÄB') === 'Äb'`); a transfer that only removes bytes
/// or inserts uncased ones preserves both bits (`trim`, `substr`, `strrev`,
/// `str_repeat`, `implode`), so an explicit `trim` charlist changes nothing.
///
/// | name(s) | keeps | forces | declines |
/// | --- | --- | --- | --- |
/// | `trim ltrim rtrim chop` | casing | — | length (`trim('  ') === ''`) |
/// | `substr` | casing always; length at a provably zero offset (whole axis with no length argument or a length ≥ 2, `NON_EMPTY` at ≥ 1) | — | length at any other offset (`substr('abc', 5) === ''`) |
/// | `strrev` | casing + length | — | — |
/// | `strtr` | `NON_EMPTY` (3-arg always; 2-arg when every replacement value is non-empty) | — | casing (`strtr('a', 'a', 'A')`) and `NON_FALSY` (`strtr('a', 'ax', '0x') === '0'`) |
/// | `str_repeat` | casing always; length at a provable multiplier ≥ 1 | — | length at `str_repeat('a', 0) === ''` |
/// | `str_pad` | length; casing when the pad argument carries it | `NON_EMPTY` at a provable length ≥ 1 | casing under an unknown pad |
/// | `strtolower` / `strtoupper` | length | `LOWERCASE` / `UPPERCASE` | the opposite casing |
/// | `ucfirst` / `ucwords` | length + `UPPERCASE` | — | `LOWERCASE` (`ucfirst('abc') === 'Abc'`) |
/// | `lcfirst` | length + `LOWERCASE` | — | `UPPERCASE` |
/// | `implode` / `join` | casing, over the glue **and** every element | — | length (an empty array implodes to `''`) |
/// | `addslashes addcslashes escapeshellarg urlencode rawurlencode preg_quote` | length | — | casing |
/// | `htmlspecialchars` / `htmlentities` | length, **only** under `ENT_SUBSTITUTE` | — | casing; everything under a non-constant flags argument |
/// | `urldecode` / `rawurldecode` | `NON_EMPTY` only | — | `NON_FALSY` (`urldecode('%30') === '0'`) |
/// | `sprintf` `vsprintf` | — | `NON_EMPTY` at a constant format with a literal byte; `NUMERIC` at a whole-format `%[flags][width][.precision]{b,d,o,e,f,g}` conversion (`e`/`f`/`g` gated on a proven `int` value — `sprintf` only, issue #41) | everything else |
/// | `strlen` | — | `int<1, max>` at a non-empty subject | — |
///
/// Only those five bits move (`NON_EMPTY`, `NON_FALSY`, `NUMERIC`, `LOWERCASE`,
/// `UPPERCASE`). `DECIMAL_INT`/`NON_DECIMAL_INT` are never propagated even where
/// they would survive: dropping a bit is a widening, and keeping the table to the
/// measured axis is worth more than the rows it would buy.
///
/// # The declines, each for a stated reason
///
/// * **`escapeshellcmd`** — upstream PHPStan's non-empty set is wrong here:
///   `escapeshellcmd("\x80") === ''` at 8.5.8 (ADR-0061 §2).
/// * **`urldecode`/`rawurldecode` keep `NON_EMPTY` only** — upstream propagates
///   non-falsiness too, refuted by `urldecode('%30') === '0'`.
/// * **Any `mb_*` name** — encoding- and locale-dependent, the catalog's standing
///   exclusion.
/// * **`substr` non-emptiness away from offset zero** — needs the subject's own
///   length, which a predicate summary lacks (`substr('a', 1, 1) === ''`); issue
///   #41 takes only the offset-zero sliver.
/// * **`strtr` casing and non-falsiness** — both refuted at the pin.
/// * **`sprintf` casing, and every non-constant format** — a conversion may emit
///   uppercase (`'%X'` → `FF`) or nothing (`'%.0s'` → `''`); only a literal byte
///   in a constant format is claimed.
/// * **`sprintf`'s `%x`/`%X`** — excluded from NUMERIC: `sprintf('%14x', 255)
///   === 'ff'` is not a numeric string, though upstream's `bug-7387.php` fixture
///   claims it is (ADR-0061 §2 again).
/// * **`sprintf`'s `%e`/`%f`/`%g` away from a proven `int` value** — the float
///   formatter renders `NAN`/`INF` verbatim and a numeric-string argument can
///   overflow to `INF`; `b`/`d`/`o` need no gate since PHP's int cast clamps.
/// * **`sprintf`'s `%c %s %u %h %H %E %F %G`** — unmeasured for this slice.
/// * **`vsprintf`'s `%e`/`%f`/`%g`** — the value sits inside the values array
///   rather than a fixed position; opening it is more machinery than needed.
/// * **`str_replace`/`substr_replace`/`parse_str`** — need a second subject's
///   predicates or an out-parameter, not this table's single-subject shape.
///
/// A `Fact::OneOf` of constant strings answers by **intersecting** each member's
/// summary — `'foo'|'bar'` is `LOWERCASE`.
fn str_pred_transfer(
    cx: &Cx,
    folder: &mut dyn Folder,
    lower: &str,
    args: &[ArgValue],
    env: &HashMap<String, Known>,
    store: Option<&Store>,
) -> Option<(Fact, &'static [&'static str])> {
    /// Every member of the family declares a real `string` (PHP 8.5.8
    /// `ReflectionFunction::getReturnType`, non-tentative) — a pin that moves when
    /// php-src adds an arm, so ADR-0064 Amendment B's `mixed` hole never opens here.
    const STRING: &[&str] = &["string"];
    /// `strlen` is the one member returning an `int`.
    const INT: &[&str] = &["int"];

    if lower == "strlen" {
        // `strlen($nonEmpty)` is `int<1, max>`: one byte in, one byte counted. The
        // curated `int<0, max>` row (ADR-0056 R3) stays the floor for every other
        // subject, and this narrows strictly inside it.
        let [subject] = args else { return None };
        let preds = arg_str_preds(cx, folder, subject, env, store)?;
        if !preds.contains_all(StrPreds::NON_EMPTY) {
            return None;
        }
        return Some((Fact::refined(Base::Int, Refinement::Int(IntRange::POSITIVE), false), INT));
    }
    let out = str_pred_out(cx, folder, lower, args, env, store)?;
    if out.is_empty() {
        return None;
    }
    Some((Fact::refined(Base::String, Refinement::Str(out), false), STRING))
}

/// The table itself: the output predicate summary for one call, or `None` where the
/// name is not a member (or its arity is not one this rule was written against).
/// Every arm is authored from `php -r` probes at `PINNED_PHP` and php.net's
/// documented semantics — see [`str_pred_transfer`] for the reasoning per row.
fn str_pred_out(
    cx: &Cx,
    folder: &mut dyn Folder,
    lower: &str,
    args: &[ArgValue],
    env: &HashMap<String, Known>,
    store: Option<&Store>,
) -> Option<StrPreds> {
    /// The length axis: `non-empty-string` and its `non-falsy-string` refinement.
    const LENGTH: StrPreds = StrPreds::NON_EMPTY.union(StrPreds::NON_FALSY);
    /// The casing pair. Both bits together is "no cased character at all" (`''`,
    /// `'123'`) and survives every transfer that keeps either one.
    const CASING: StrPreds = StrPreds::LOWERCASE.union(StrPreds::UPPERCASE);
    /// Everything this table ever propagates.
    const BOTH_AXES: StrPreds = LENGTH.union(CASING);
    /// `ENT_SUBSTITUTE`. Without it, `htmlspecialchars` answers `''` for a
    /// non-empty subject holding an invalid encoding sequence — witnessed at 8.5.8:
    /// `htmlspecialchars("\x80", ENT_QUOTES) === ''`, while
    /// `htmlspecialchars("\x80", ENT_SUBSTITUTE)` is the three-byte U+FFFD.
    const ENT_SUBSTITUTE: i64 = 8;

    // Argument 0 is the subject for every member but `implode`, whose own arm
    // ignores this. A zero-argument call is not one of these builtins at all.
    let subject = arg_str_preds(cx, folder, args.first()?, env, store).unwrap_or_default();

    match lower {
        // ---- Removal only: the output is a SUBSTRING of the subject ----------
        //
        // `trim('  ') === ''` kills the length axis outright; an explicit charlist
        // changes nothing (`ltrim('0abc', '0') === 'abc'` is still a substring).
        // `chop` is `rtrim` (same reflected `string`, same 1..=2).
        "trim" | "ltrim" | "rtrim" | "chop" => {
            (1..=2).contains(&args.len()).then(|| subject.intersect(CASING))
        }
        // `substr('abc', 0, 0) === ''`, so casing is all a bare `substr` claims.
        // The length axis needs the offset AND the length, and issue #41 buys
        // exactly the sliver that needs no knowledge of the subject's own length:
        // an offset **provably zero** anchors the window at the first byte, and
        // then the output's first `min(strlen($s), $length)` bytes are the
        // subject's own. Witnesses at `PINNED_PHP` (8.5.9):
        //
        // * `substr('abc', 0) === 'abc'` — the two-argument form at offset 0 is the
        //   IDENTITY, so both length bits survive it.
        // * `substr('a', 0, 2) === 'a'` — a length past the end clamps; it never
        //   pads, so a non-empty subject cannot come back empty.
        // * `substr('abc', 0, 0) === ''` and `substr('abc', 0, -5) === ''` — which
        //   is why the length must be provably `>= 1`, negatives included.
        // * `substr('0x', 0, 2) === '0x'` — at a length `>= 2` the output is either
        //   two bytes (so not `'0'`) or the whole subject (so non-falsy if the
        //   subject was), which is the whole non-falsy leg.
        //
        // A non-zero (or unknown) offset declines the length axis outright:
        // `substr('abc', 5) === ''`, and this rung does not carry `strlen($s)`.
        "substr" => {
            if !(2..=3).contains(&args.len()) {
                return None;
            }
            let anchored = transfer_arg_fact(cx, folder, &args[1], env, store)
                .is_some_and(|f| f == Fact::Singleton(Val::Int(0)));
            let length = match (anchored, args.get(2)) {
                (false, _) => StrPreds::empty(),
                // The identity form.
                (true, None) => LENGTH,
                (true, Some(len)) => {
                    let len = transfer_arg_fact(cx, folder, len, env, store);
                    let at_least = |n: i64| len.as_ref().is_some_and(|f| fact_int_at_least(f, n));
                    if at_least(2) {
                        LENGTH
                    } else if at_least(1) {
                        StrPreds::NON_EMPTY
                    } else {
                        StrPreds::empty()
                    }
                }
            };
            Some(subject.intersect(CASING.union(length)))
        }
        // ---- Byte MAPPING: the length is preserved, the bytes are not --------
        //
        // `strtr($s, $from, $to)` maps single bytes 1:1 over the common prefix of
        // `$from`/`$to`, so the output length EQUALS the subject's and
        // non-emptiness survives (`strtr('ab', 'ab', 'xy') === 'xy'`). Both
        // refusals measured at 8.5.9:
        //
        // * **Casing** — `strtr('a', 'a', 'A') === 'A'`: the map's target byte is
        //   arbitrary.
        // * **`NON_FALSY`** — `strtr('a', 'ax', '0x') === '0'`, refuting upstream
        //   PHPStan's claim for both arities (ADR-0061 §2). Same shape for the
        //   array form: `strtr('a', ['a' => '0']) === '0'`.
        //
        // The array form replaces whole substrings, so length is not preserved and
        // `''` DELETES an entry (`strtr('a', ['a' => '']) === ''`). Non-emptiness
        // survives only when every replacement value is known non-empty, read off
        // by [`array_value_preds`].
        "strtr" => match args {
            [_subject, pairs] => {
                let vals = array_value_preds(cx, folder, pairs, env, store)?;
                vals.contains_all(StrPreds::NON_EMPTY)
                    .then(|| subject.intersect(StrPreds::NON_EMPTY))
            }
            [_subject, _from, _to] => Some(subject.intersect(StrPreds::NON_EMPTY)),
            _ => None,
        },
        // ---- Permutation: the byte MULTISET is preserved -------------------
        //
        // `strrev` keeps the length exactly, so `''`/`'0'` can only come from
        // `''`/`'0'`: both length bits survive alongside both casing bits.
        "strrev" => {
            let [_] = args else { return None };
            Some(subject.intersect(BOTH_AXES))
        }
        // ---- Repetition: length survives only at a provable multiplier ≥ 1 ----
        //
        // `str_repeat('a', 0) === ''`. Casing survives regardless — `''` carries
        // both casing bits.
        "str_repeat" => {
            let [_subject, times] = args else { return None };
            let once = transfer_arg_fact(cx, folder, times, env, store)
                .as_ref()
                .is_some_and(fact_is_positive_int);
            Some(subject.intersect(if once { BOTH_AXES } else { CASING }))
        }
        // ---- Padding: the subject is a SUBSEQUENCE of the output --------------
        //
        // The length axis always survives (`str_pad` never shortens). Output
        // length is `max(strlen($subject), $length)`, so a provable `$length >= 1`
        // FORCES non-emptiness whatever the subject was — `str_pad('', 1) === ' '`,
        // while `str_pad('', 0) === ''`.
        //
        // Casing needs the pad string too. An absent pad argument is `' '`, no
        // cased character, carrying both bits; an unknown one drops casing.
        "str_pad" => {
            if !(2..=4).contains(&args.len()) {
                return None;
            }
            let pad = match args.get(2) {
                None => CASING,
                Some(p) => arg_str_preds(cx, folder, p, env, store).unwrap_or_default().intersect(CASING),
            };
            let forced = if transfer_arg_fact(cx, folder, &args[1], env, store)
                .as_ref()
                .is_some_and(fact_is_positive_int)
            {
                StrPreds::NON_EMPTY
            } else {
                StrPreds::empty()
            };
            Some(subject.intersect(LENGTH.union(pad)).union(forced))
        }
        // ---- The FORCED casing pair ------------------------------------------
        //
        // Byte-wise ASCII case mapping (locale-insensitive since 8.2): length is
        // preserved, so both length bits survive, and the target casing holds for
        // ANY input. The OPPOSITE casing is dropped: `strtolower('AB')` is `'ab'`.
        "strtolower" => {
            let [_] = args else { return None };
            Some(subject.intersect(LENGTH).union(StrPreds::LOWERCASE))
        }
        "strtoupper" => {
            let [_] = args else { return None };
            Some(subject.intersect(LENGTH).union(StrPreds::UPPERCASE))
        }
        // ---- Selective casing: length survives, one casing bit does ----------
        //
        // `ucfirst('abc') === 'Abc'` breaks `LOWERCASE` but cannot break
        // `UPPERCASE` (it only ever uppercases). `lcfirst` is the mirror. `ucwords`
        // is `ucfirst` at every word boundary, same asymmetry.
        "ucfirst" => {
            let [_] = args else { return None };
            Some(subject.intersect(LENGTH.union(StrPreds::UPPERCASE)))
        }
        "lcfirst" => {
            let [_] = args else { return None };
            Some(subject.intersect(LENGTH.union(StrPreds::LOWERCASE)))
        }
        "ucwords" => {
            (1..=2).contains(&args.len()).then(|| subject.intersect(LENGTH.union(StrPreds::UPPERCASE)))
        }
        // ---- Concatenation: EVERY contributor must carry the claim -----------
        //
        // The output's bytes are the elements' bytes plus the glue's, so a casing
        // bit holds exactly when the glue and every admitted element hold it. The
        // one-argument form's glue is `''`, which carries both. The length axis is
        // NOT claimed: `implode(',', [])` is `''`, and proving otherwise needs the
        // array's non-emptiness *and* an element's.
        "implode" | "join" => {
            let (glue, array) = match args {
                [array] => (CASING, array),
                [glue, array] => {
                    (arg_str_preds(cx, folder, glue, env, store).unwrap_or_default().intersect(CASING), array)
                }
                _ => return None,
            };
            Some(glue.intersect(implode_element_preds(cx, folder, array, env, store)?))
        }
        // ---- The escaping family: insertion only ------------------------------
        //
        // Each of these only ever *inserts* bytes or copies them, so a non-empty
        // subject stays non-empty and a non-falsy one cannot collapse to `'0'`.
        // Casing is NOT claimed: `htmlspecialchars('<')` is `'&lt;'` and
        // `urlencode('ä')` is `'%C3%A4'` (uppercase hex) — both bits break.
        "addslashes" | "escapeshellarg" | "urlencode" | "rawurlencode" => {
            let [_] = args else { return None };
            Some(subject.intersect(LENGTH))
        }
        "addcslashes" => {
            let [_, _] = args else { return None };
            Some(subject.intersect(LENGTH))
        }
        "preg_quote" => (1..=2).contains(&args.len()).then(|| subject.intersect(LENGTH)),
        // `urldecode('%30') === '0'`: decoding SHRINKS, so a non-falsy subject can
        // decode to the falsy `'0'`. Non-emptiness still holds — every `%XX` triple
        // yields one byte and every other byte yields itself.
        "urldecode" | "rawurldecode" => {
            let [_] = args else { return None };
            Some(subject.intersect(StrPreds::NON_EMPTY))
        }
        // The one gated pair: without `ENT_SUBSTITUTE` an invalid encoding sequence
        // makes the whole call answer `''`. A missing flags argument is the 8.1+
        // default (`ENT_QUOTES | ENT_SUBSTITUTE | ENT_HTML401` == 11), which
        // carries the bit; a non-constant flags argument declines everything. The
        // `$encoding`/`$double_encode` arguments do not move the boundary — probed
        // at 8.5.8 across UTF-8, ISO-8859-1, SJIS and two unsupported charsets.
        "htmlspecialchars" | "htmlentities" => {
            if !(1..=4).contains(&args.len()) {
                return None;
            }
            let substitutes = match args.get(1) {
                None => true,
                Some(flags) => fact_int_bits_all_set(
                    &transfer_arg_fact(cx, folder, flags, env, store)?,
                    ENT_SUBSTITUTE,
                ),
            };
            substitutes.then(|| subject.intersect(LENGTH))
        }
        // ---- `sprintf`/`vsprintf`: a literal byte in a CONSTANT format, and ----
        // ---- issue #41's NUMERIC slice on top of it ----------------------------
        //
        // Both names carry their format at argument 0 (ADR-0078's `PrintfShape`),
        // reading [`sprintf_emits_a_literal`] and
        // [`sprintf_whole_numeric_conversion`] the same way — the difference is
        // only WHERE the converted value lives: `sprintf`'s own second positional
        // argument, or inside `vsprintf`'s values array.
        //
        // Every conversion can produce nothing (`sprintf('%.0s', 'abc') === ''`),
        // so a format only proves the literal text between conversions. `'%%'`
        // counts — it emits one `'%'`.
        //
        // NUMERIC is a SEPARATE, stricter question — [`sprintf_whole_numeric_conversion`]
        // answers it — forcing `StrPreds::NUMERIC` (closes to `NON_EMPTY` too) when
        // the WHOLE format is one admitted conversion. `b`/`d`/`o` are forced
        // unconditionally (PHP's int cast cannot render anything but digits) for
        // EITHER name — issue #41's `vsprintf('%d', $array)` row (`bug-7387.php`)
        // needs no look inside `$array` at all. `e`/`f`/`g` are forced only when
        // the paired value argument is provably an `int` — a float value could BE
        // `NAN`/`INF` — and only `sprintf` exposes that argument positionally;
        // `vsprintf`'s stays declined there.
        "sprintf" | "vsprintf" => {
            let Some(Fact::Singleton(Val::Str(fmt))) = transfer_arg_fact(cx, folder, &args[0], env, store)
            else {
                return None;
            };
            let mut out = if sprintf_emits_a_literal(fmt.as_bytes())? {
                StrPreds::NON_EMPTY
            } else {
                StrPreds::empty()
            };
            if let Some(ty) = sprintf_whole_numeric_conversion(fmt.as_bytes()) {
                let numeric_safe = match ty {
                    b'b' | b'd' | b'o' => true,
                    b'e' | b'f' | b'g' if lower == "sprintf" => args
                        .get(1)
                        .and_then(|v| transfer_arg_fact(cx, folder, v, env, store))
                        .is_some_and(|f| fact_is_int(&f)),
                    _ => false,
                };
                if numeric_safe {
                    out = out.union(StrPreds::NUMERIC);
                }
            }
            Some(out)
        }
        _ => None,
    }
}

/// The predicate summary an argument's fact carries, or `None` when the fact is not
/// a string one at all. `General { String }` answers the *empty* summary — it is a
/// string, just one nothing is known about — so a caller intersecting against it
/// declines for the right reason.
fn arg_str_preds(
    cx: &Cx,
    folder: &mut dyn Folder,
    value: &ArgValue,
    env: &HashMap<String, Known>,
    store: Option<&Store>,
) -> Option<StrPreds> {
    fact_str_preds(&transfer_arg_fact(cx, folder, value, env, store)?)
}

/// The predicate summary EVERY value a fact admits satisfies.
///
/// The finite layers are read directly: a `OneOf` of constant strings intersects
/// its members' summaries, so `'foo'|'bar'` is `LOWERCASE` and `'foo'|'BAR'` is
/// neither casing.
fn fact_str_preds(f: &Fact) -> Option<StrPreds> {
    match f {
        Fact::Singleton(Val::Str(s)) => Some(StrPreds::of(s)),
        Fact::OneOf(vals) => {
            let mut acc: Option<StrPreds> = None;
            for v in vals {
                let Val::Str(s) = v else { return None };
                let p = StrPreds::of(s);
                acc = Some(acc.map_or(p, |a| a.intersect(p)));
            }
            acc
        }
        Fact::Refined { base: Base::String, refinement: Refinement::Str(p), nullable: false } => {
            Some(*p)
        }
        Fact::General { base: Base::String, nullable: false } => Some(StrPreds::empty()),
        _ => None,
    }
}

/// The predicate summary every element of `implode`'s array argument satisfies.
///
/// An abstract shape answers through [`shape_value_union`] — one unknown slot
/// already yields `None`, the decline this rule wants. A fully known array
/// answers by intersecting its values; the empty array answers the summary of
/// `''` (both casing bits), what it implodes to. A non-string element declines —
/// `implode` casts it, and the cast is the element's business, not this rule's.
fn implode_element_preds(
    cx: &Cx,
    folder: &mut dyn Folder,
    value: &ArgValue,
    env: &HashMap<String, Known>,
    store: Option<&Store>,
) -> Option<StrPreds> {
    array_value_preds_seeded(cx, folder, value, env, store, Some(StrPreds::of("")))
}

/// The predicate summary every VALUE of an array argument satisfies, with **no
/// seed** — unlike [`implode_element_preds`], whose `''` seed is `implode`'s own
/// answer for the empty array. `strtr`'s array form needs "every replacement
/// value is non-empty", and the empty array declines here rather than pretending.
fn array_value_preds(
    cx: &Cx,
    folder: &mut dyn Folder,
    value: &ArgValue,
    env: &HashMap<String, Known>,
    store: Option<&Store>,
) -> Option<StrPreds> {
    array_value_preds_seeded(cx, folder, value, env, store, None)
}

/// The shared body of the two accessors above: the intersection over an array
/// argument's values, starting from `seed`.
fn array_value_preds_seeded(
    cx: &Cx,
    folder: &mut dyn Folder,
    value: &ArgValue,
    env: &HashMap<String, Known>,
    store: Option<&Store>,
    seed: Option<StrPreds>,
) -> Option<StrPreds> {
    match transfer_arg_fact(cx, folder, value, env, store)? {
        Fact::Shape { shape, nullable: false } => fact_str_preds(&shape_value_union(&shape)?),
        Fact::Singleton(Val::Array(entries)) => {
            let mut acc = seed;
            for (_, v) in &entries {
                let Val::Str(s) = v else { return None };
                let p = StrPreds::of(s);
                acc = Some(acc.map_or(p, |a| a.intersect(p)));
            }
            acc
        }
        _ => None,
    }
}

/// Is every value this fact admits an integer of at least 1? The multiplier gate
/// for `str_repeat` and the length gate for `str_pad` — both of which turn on
/// exactly that boundary (`str_repeat('a', 0) === ''`, `str_pad('', 0) === ''`).
fn fact_is_positive_int(f: &Fact) -> bool {
    fact_int_at_least(f, 1)
}

/// Is every value this fact admits an integer of at least `bound`? The general
/// form of [`fact_is_positive_int`], which `substr`'s length axis needs at two
/// different bounds at once (`>= 1` for non-emptiness, `>= 2` for non-falsiness).
///
/// Everything the finite layers admit is read directly; anything else answers
/// `false`, which is the decline every caller wants from an integer it cannot see
/// through.
fn fact_int_at_least(f: &Fact, bound: i64) -> bool {
    match f {
        Fact::Singleton(Val::Int(n)) => *n >= bound,
        // `all` over an empty set is vacuously true; a `OneOf` is never empty in
        // practice, and the guard says so rather than relying on it.
        Fact::OneOf(vals) => {
            !vals.is_empty() && vals.iter().all(|v| matches!(v, Val::Int(n) if *n >= bound))
        }
        Fact::Refined { base: Base::Int, refinement: Refinement::Int(r), nullable: false } => {
            r.lo() >= bound
        }
        _ => false,
    }
}

/// Does every integer this fact admits have all of `bits` set? A flags argument the
/// rule cannot see through (a variable, an abstract int) answers `false`, which is
/// the decline `htmlspecialchars` needs.
fn fact_int_bits_all_set(f: &Fact, bits: i64) -> bool {
    match f {
        Fact::Singleton(Val::Int(n)) => n & bits == bits,
        Fact::OneOf(vals) => {
            !vals.is_empty() && vals.iter().all(|v| matches!(v, Val::Int(n) if n & bits == bits))
        }
        _ => false,
    }
}

/// Does this `sprintf` format guarantee at least one output byte?
///
/// `Some(true)` when a byte is emitted no matter what the arguments are — a literal
/// outside any conversion, or a `'%%'` escape. `Some(false)` when the format is all
/// conversions (`'%s'`, `'%.0s'`), each of which may emit nothing. **`None` when the
/// scanner does not recognize the format**, which is a decline, not a `false`: a
/// mis-parsed conversion specifier read as a literal would be a false premise, so
/// anything outside the documented `%[argnum$][flags][width][.precision]specifier`
/// grammar refuses the rule outright. A trailing `%` is a `ValueError` at 8.5.8 and
/// lands in the same refusal — there is no return value to describe.
fn sprintf_emits_a_literal(fmt: &[u8]) -> Option<bool> {
    /// php-src's `php_formatted_print` conversion characters.
    const SPECIFIERS: &[u8] = b"bcdeEfFgGosuxX";

    let mut i = 0;
    let mut literal = false;
    while i < fmt.len() {
        if fmt[i] != b'%' {
            literal = true;
            i += 1;
            continue;
        }
        i += 1;
        if *fmt.get(i)? == b'%' {
            // `sprintf('%%') === '%'` — a guaranteed byte.
            literal = true;
            i += 1;
            continue;
        }
        loop {
            match *fmt.get(i)? {
                // `%'x5s`: the byte after the quote is the custom padding char.
                b'\'' => {
                    i += 2;
                    if i > fmt.len() {
                        return None;
                    }
                }
                b'-' | b'+' | b' ' | b'.' | b'$' | b'0'..=b'9' => i += 1,
                _ => break,
            }
        }
        if !SPECIFIERS.contains(fmt.get(i)?) {
            return None;
        }
        i += 1;
    }
    Some(literal)
}

/// Issue #41's sprintf `NUMERIC` slice: is this `sprintf`/`vsprintf` format EXACTLY
/// one conversion — no literal byte anywhere, not even a `%%` escape, no second
/// specifier, no explicit `%N$` position — built from admitted flags/width/
/// precision and one of the six type characters this rule has probed sound?
/// Returns that type character on a match.
///
/// # Why a stricter grammar than [`sprintf_emits_a_literal`]
///
/// That scanner answers "does a byte definitely come out"; this one answers "is
/// the WHOLE output determined by one conversion" — a single unrecognized byte
/// anywhere is a decline here even where the general scanner would walk past it.
///
/// # The admitted flags, probed at `PINNED_PHP` (8.5.9)
///
/// ```text
/// php -r 'var_dump(sprintf("%05d", 5));'      // "00005"      (zero-pad: NUMERIC)
/// php -r 'var_dump(sprintf("%+d", 5));'       // "+5"         (leading +: NUMERIC)
/// php -r 'var_dump(sprintf("%+d", -5));'      // "-5"
/// php -r 'var_dump(sprintf("%-10d", 5));'     // "5         " (trailing spaces:
///                                              //   PHP 8+ numeric strings allow
///                                              //   trailing whitespace)
/// php -r 'var_dump(sprintf("% 5d", 5));'      // "    5"      (default space-pad:
///                                              //   leading whitespace is ALWAYS
///                                              //   allowed in a numeric string)
/// php -r 'var_dump(sprintf("% d", 5));'       // "5"          (the space FLAG
///                                              //   itself is a documented no-op
///                                              //   at this pin — admitted for
///                                              //   the same reason as '0'/'+'/'-')
/// ```
///
/// A custom pad (the `'` flag) is refused outright — its pad byte is arbitrary and
/// not always whitespace-or-digit:
///
/// ```text
/// php -r 'var_dump(sprintf("%\x27*10d", 5));' // "*********5" — NOT numeric, even
///                                              //   though "%'010d" (pad '0') would be
/// ```
///
/// # The type-character split: int-cast vs. float-format
///
/// `b`/`d`/`o` go through PHP's int cast (`zend_dval_to_lval`), which clamps any
/// input to a definite, in-range integer, rendering only ASCII digits (and an
/// optional leading `-`), so these three are admitted UNCONDITIONALLY:
///
/// ```text
/// php -r 'var_dump(sprintf("%d", NAN));'        // "0"   (int-cast clamps; NUMERIC)
/// php -r 'var_dump(sprintf("%b", 1.0e300));'    // "0"   (same clamp; NUMERIC)
/// php -r 'var_dump(sprintf("%o", "1e400"));'    // "0"   (string->int; NUMERIC)
/// ```
///
/// `e`/`f`/`g` go through PHP's float FORMATTER instead, which renders a
/// non-finite float verbatim — and that rendering is not a numeric string:
///
/// ```text
/// php -r 'var_dump(sprintf("%f", NAN));'        // "NaN"  — NOT numeric
/// php -r 'var_dump(sprintf("%f", INF));'        // "INF"  — NOT numeric
/// php -r 'var_dump(sprintf("%f", "1e400"));'    // "INF"  — a numeric STRING can
///                                                //   overflow its (float) cast too
/// ```
///
/// A native PHP `int` cannot hold `NAN`/`INF` at all (and `null`'s `(float)` cast
/// is the finite `0.0`), so the call site admits `e`/`f`/`g` only when the value
/// argument's own fact is provably `int` (via [`fact_is_int`], `null`-immaterial)
/// — the same boundary PHPStan's own `bug-7387.php` fixture draws by typing that
/// argument `int $i` rather than `float`.
///
/// # What stays out of this slice
///
/// `%x`/`%X` are the excluded hex pair (`sprintf('%14x', 255) === 'ff'`, not a
/// numeric string — upstream PHPStan's `bug-7387.php` claims `numeric-string` for
/// both and is wrong at this pin). `%c`/`%s`/`%u`/`%h`/`%H`/`%E`/`%F`/`%G` are
/// unmeasured for this slice, left to a future slice rather than an unwitnessed
/// guess.
fn sprintf_whole_numeric_conversion(fmt: &[u8]) -> Option<u8> {
    let n = fmt.len();
    if n < 2 || fmt[0] != b'%' {
        return None;
    }
    let mut i = 1;
    // Flags: only the four bytes probed above as numeric-safe. A custom pad
    // (`'`) is not in this set, so it falls through to the "leftover bytes"
    // decline below rather than being specially recognized and refused.
    while i < n && matches!(fmt[i], b'-' | b'+' | b'0' | b' ') {
        i += 1;
    }
    // Width.
    while i < n && fmt[i].is_ascii_digit() {
        i += 1;
    }
    // Precision: `.` optionally followed by digits.
    if i < n && fmt[i] == b'.' {
        i += 1;
        while i < n && fmt[i].is_ascii_digit() {
            i += 1;
        }
    }
    // Exactly one byte must remain: the type character. Anything else — a
    // literal byte before/after, a second specifier, a dangling `%N$` position
    // (whose `$` is never consumed by the loops above), a custom-pad quote — is
    // leftover content this whole-format claim cannot admit.
    if i + 1 != n {
        return None;
    }
    matches!(fmt[i], b'b' | b'd' | b'o' | b'e' | b'f' | b'g').then_some(fmt[i])
}

/// A `list<T>` / `non-empty-list<T>` fact, through the canonical constructor —
/// `None` element is the unknown floor (`list<mixed>`).
pub(crate) fn list_transfer_fact(non_empty: bool, elem: Option<Fact>) -> Fact {
    use steins_domain::{KeyClass, Tail};
    shape_fact(ShapeFact::normalize(
        Vec::new(),
        Tail::Unsealed { key: KeyClass::Int, value: elem.map(Box::new) },
        Certainty::Yes,
        non_empty,
        Vec::new(),
    ))
}

/// Is every value this fact admits a non-empty string? A literal (or literal set)
/// answers by inspection; an abstract string answers through the predicate the
/// domain already models. Anything else — including a nullable string — is `false`.
fn fact_is_non_empty_string(f: &Fact) -> bool {
    match f {
        Fact::Singleton(Val::Str(s)) => !s.is_empty(),
        Fact::OneOf(vals) => vals.iter().all(|v| matches!(v, Val::Str(s) if !s.is_empty())),
        Fact::Refined { base: Base::String, refinement: Refinement::Str(p), nullable: false } => {
            p.contains_all(StrPreds::NON_EMPTY)
        }
        _ => false,
    }
}
