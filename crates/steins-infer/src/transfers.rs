//! The argument-dispatched transfers (ADR-0064 seam (ii)): per-builtin rungs that
//! read a call's arguments and answer a sharper fact than the declared return.
//!
//! This file is the dispatch ([`arg_dispatch_return_fact`]), the argument readers
//! every rule shares ([`transfer_arg_fact`], [`transfer_arg_known`]), and the three
//! rules that are one function each: `array_key_exists`, `preg_replace` and
//! `var_export`. Each larger family is a module that reads the readers here and
//! nothing of another family:
//!
//! * [`arith`] — `min` / `max`, `abs` and `pow`;
//! * [`curl`] — `curl_getinfo`;
//! * [`filter_var`];
//! * [`lists`] — `explode`, `range`, and the list fact other modules build too;
//! * [`scanf`] — `sscanf`;
//! * [`str_preds`] — the string predicates, `strlen` and `sprintf`.

mod arith;
mod curl;
mod filter_var;
mod lists;
mod scanf;
mod str_preds;

pub(crate) use lists::list_transfer_fact;

use std::collections::HashMap;

use steins_domain::{Base, Fact, Presence, ShapeFact, Tail, Val};
use steins_syntax::ArgValue;

use crate::by_value::value_stratum;
use crate::cx::Cx;
use crate::env::{
    ContractArm, Known, Store, Stratum, array_literal_fact, singleton_fact, val_of,
};
use crate::fold::Folder;
use crate::global_consts::global_const_fact;
use crate::builtin_returns::transfer_envelope_admits;

use arith::{abs_transfer, min_max_transfer, pow_transfer};
use curl::curl_getinfo_transfer;
use filter_var::filter_var_transfer;
use lists::{explode_transfer, range_transfer};
use scanf::sscanf_transfer;
use str_preds::str_pred_transfer;

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

    /// `sscanf`: `array|int|null`, in the order the engine renders it. Not a bare
    /// `mixed`, so Amendment B does not FORCE an arity leg — [`ARITY_SSCANF`] is
    /// carried anyway, because the rule reads argument **1** positionally and
    /// dispatches on the argument COUNT, which a signature change would silently
    /// invalidate while `array|int|null` still held.
    const SSCANF: &[&str] = &["array|int|null"];
    /// `sscanf`'s live signature at `PINNED_PHP` (8.5.9): three parameters, two
    /// required, variadic — `sscanf(string $string, string $format, mixed
    /// &...$vars)`. Measured against the live reflection, not read out of
    /// `param_facts_generated.rs`.
    const ARITY_SSCANF: Option<(u32, u32)> = Some((3, 2));

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
        // `sscanf` ONLY (issue #617) — not `fscanf`, whose `array|int|false|null`
        // envelope carries a `false` arm no `Fact` spells; see [`sscanf_transfer`].
        "sscanf" => (sscanf_transfer(cx, folder, args, env, store)?, SSCANF, ARITY_SSCANF),
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
                .map_or_else(|| value_stratum(cx, v, env, store), |(_, s)| s),
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
            .and_then(|(l, s)| Some((singleton_fact(&l)?, s)))
            .or_else(|| array_literal_fact(cx, folder, items, env, false, store))
    {
        return Some((lit, strat.min(value_stratum(cx, value, env, store))));
    }
    // A bare global constant (ADR-0094, issue #598). Above the literal seam
    // because the literal seam cannot spell what most of these constants ARE: a
    // host-dependent one defaults to the UNION of its values (`PHP_EOL` is
    // `"\n"|"\r\n"`), and `PHP_VERSION_ID` is the range the declared target spans.
    // The single-valued ones would answer through the literal seam below too, and
    // answer identically here — the resolver is one function.
    //
    // The stratum comes from the resolver, not from `value_stratum`: a value fixed
    // by the `[runtime] os` pin is `Asserted` (the user's claim about the host,
    // ADR-0094 §3.2), and laundering it to `Verified` here would let it premise a
    // proof-layer finding.
    if let ArgValue::GlobalConst(r) = value {
        return global_const_fact(cx, r);
    }
    let lit = cx.resolve_literal(value, env, false, folder)?;
    Some((singleton_fact(&lit)?, value_stratum(cx, value, env, store)))
}

/// The declared contract lane as ONE fact, with the weakest stratum any arm of it
/// carries. Every arm must lower ([`steins_contract::to_fact`]) and the domain must
/// be able to join them — `'foo'|'bar'` becomes a `OneOf`, `int|string` declines,
/// which is the same honest floor the value-slot lowering takes everywhere else.
pub(crate) fn declared_arm_known(arms: &[ContractArm]) -> Option<(Fact, Stratum)> {
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
    let key = crate::offsets::offset_key_of(&val_of(key_arg)?)?;
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
