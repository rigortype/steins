use std::collections::HashMap;

use steins_domain::{Base, Certainty, Fact, Refinement, ShapeFact, StrPreds, Tail, Val};
use steins_syntax::{ArgValue, ArrayKey, CastTarget, RefKind, ValueOp};

use crate::coerce::php_cast_fact;
use crate::cx::Cx;
use crate::env::{Known, Store};
use crate::fold::Folder;
use crate::transfers::transfer_arg_fact;

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
/// * **A flag outside the accepted list** — `FILTER_REQUIRE_SCALAR` refuses an
///   array input outright and is not read here (it is a *validity* claim about the
///   input, which is a different question from the answer's type — and measurably
///   not a no-op: `filter_var(17, …, FILTER_REQUIRE_SCALAR|FILTER_FORCE_ARRAY)` is
///   `[17]`, so it does not even dominate its neighbours);
///   `FILTER_FLAG_STRIP_LOW` / `_STRIP_HIGH` / `_STRIP_BACKTICK` /
///   `_ENCODE_LOW` / `_ENCODE_HIGH` / `_ENCODE_AMP` / `_NO_ENCODE_QUOTES` rewrite
///   the string, so `FILTER_DEFAULT` stops being the identity;
///   `FILTER_FLAG_EMPTY_STRING_NULL` turns `''` into `null` on the SUCCESS path
///   (measured), which no cell above accounts for; and `FILTER_THROW_ON_FAILURE`
///   is a PHP 8.5 constant whose whole point is to delete the failure arm — a
///   sharper answer than anything here, but one that needs a PHP-minor gate this
///   rung does not carry.
/// * **A flags argument whose value is not PROVEN** — a declared `int $flags`
///   parameter has no bits to decompose. A flags argument held in a
///   const-valued local is no longer among these: ADR-0094 §2 gives
///   `$nullFilter = \FILTER_NULL_ON_FAILURE` a value, and
///   [`filter_flag_alternatives`] reads the integer when no flag NAME is spelled
///   — which is what `filterVar.php` spends two rows per filter block on. A `|`
///   combination and a `?:` ternary over recognized constants are read too.
///
/// # The array flags (issue #615 leg (a))
///
/// `FILTER_FORCE_ARRAY` and `FILTER_REQUIRE_ARRAY` are read, and both answer
/// through [`Fact::Shape`] — `ShapeFact::plain_array` with a typed tail IS plain
/// `array<T>` (ADR-0062 §3, no array-`General` variant), so the scalar outcome
/// [`filter_success`] already computes is exactly the element fact. The grid,
/// every cell probed at `PINNED_PHP` (8.5.9):
///
/// | flags | input | answer | witness |
/// | --- | --- | --- | --- |
/// | `FORCE_ARRAY` | proven non-array | `array<outcome>` | `filter_var(17, INT, FORCE_ARRAY)` → `[0 => 17]` |
/// | `FORCE_ARRAY` | may be an array | **decline** | the map recurses — see below |
/// | `REQUIRE_ARRAY` | proven non-array | `false`, or `null` under `NULL_ON_FAILURE` | `filter_var(17, INT, REQUIRE_ARRAY)` → `false` |
/// | `REQUIRE_ARRAY` | may be an array | **decline** | the element, as above; and without `NULL_ON_FAILURE` the outer arm is `array\|false` too (issue #600 + no array arm in `Fact::Union`) |
/// | `REQUIRE_ARRAY\|FORCE_ARRAY` | proven non-array | as `REQUIRE_ARRAY` alone | `REQUIRE_ARRAY` dominates: `filter_var(17, INT, RA\|FA)` → `false` |
///
/// **The decline on an input that may be an array is the load-bearing cell, and it
/// refutes the reference implementation.** Under either array flag `filter_var`
/// does not map the scalar filter over the input's slots — it walks the input
/// *recursively*, and a slot that is itself an array stays an array:
///
/// ```text
/// filter_var([[1]],        FILTER_VALIDATE_INT, ['flags' => FORCE_ARRAY]) === [0 => [0 => 1]]
/// filter_var(['a'=>['b'=>'z']], FILTER_VALIDATE_INT, ['flags' => REQUIRE_ARRAY]) === ['a' => ['b' => false]]
/// filter_var([[[[[1]]]]],  FILTER_VALIDATE_INT, ['flags' => FORCE_ARRAY]) === [[[[[1]]]]]
/// ```
///
/// So for an input whose slots may be arrays — `mixed`, or an `array<string, mixed>`
/// map — the true element fact is `int|false|array<…>` at unbounded depth, which no
/// [`Fact`] spells. Upstream PHPStan asserts a flat `array<string, int|false>` for
/// exactly that input and is unsound there; those rows stay `unknown` (the issue
/// #40 / #594 precedent — when the fixture and the measurement disagree, the
/// measurement wins). A shape whose slots are themselves proven non-array would map
/// soundly, but no fixture row spells one, so the rung asks the simpler question.
///
/// **Not taken, deliberately.** Over a proven non-array input `FORCE_ARRAY` yields
/// exactly ONE slot at key `0` (probed across every filter and both failure modes),
/// so `list{outcome}` would be sound and strictly sharper than `array<outcome>`.
/// That is a second claim — about the result's *cardinality* rather than its
/// element type — and this rung's business is the element type; the sharpening is
/// recorded here rather than made.
///
/// [`php_cast_fact`]: crate::coerce::php_cast_fact
pub(super) fn filter_var_transfer(
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
    let kinds = match filter {
        None => vec![FilterKind::Raw],
        Some(v) => filter_kinds(cx, folder, v, env)?,
    };
    let flag_sets = filter_var_flags(cx, folder, options, env)?;
    let input = transfer_arg_fact(cx, folder, value, env, store);
    // A ternary in either position contributes its arms as ALTERNATIVES, so the
    // answer is the join over the cross product. The join declines whole, never
    // per-arm: `?` picking between two flag sets whose answers the domain cannot
    // unite is a call this rung has nothing to say about.
    let mut acc: Option<Fact> = None;
    for kind in &kinds {
        for flags in &flag_sets {
            let arm = filter_var_answer(*kind, *flags, input.as_ref())?;
            acc = Some(match acc {
                None => arm,
                Some(prev) => prev.join(&arm)?,
            });
        }
    }
    acc
}

/// The answer for ONE `(filter, flags, input)` triple — the scalar rung
/// [`filter_success`] computes, plus issue #615 leg (a)'s array wrapping.
fn filter_var_answer(kind: FilterKind, flags: FilterFlags, input: Option<&Fact>) -> Option<Fact> {
    let scalar = filter_scalar_answer(kind, flags.null_on_failure, input);
    if !flags.force_array && !flags.require_array {
        return scalar;
    }
    // Both array flags read the input's array-ness first, and only a PROVEN
    // non-array answers: the recursive map over an input that may be an array has
    // no element fact (see [`filter_var_transfer`]).
    if !input.is_some_and(fact_denotes_no_array) {
        return None;
    }
    if flags.require_array {
        // A proven non-array input can never satisfy `REQUIRE_ARRAY`, so the call
        // has no success arm at all — the failure value stands alone, and both of
        // its spellings are plain Singletons. `FORCE_ARRAY` riding along changes
        // nothing (measured: `REQUIRE_ARRAY` dominates).
        return Some(Fact::Singleton(if flags.null_on_failure { Val::Null } else { Val::Bool(false) }));
    }
    // `FORCE_ARRAY` over a proven non-array input wraps whatever the scalar rung
    // answered; the wrapping itself never fails, so there is no outer arm.
    Some(Fact::Shape { shape: Box::new(plain_array_of(scalar?)), nullable: false })
}

/// Plain `array<T>`: the degenerate shape (`ShapeFact::plain_array`) with `T` on
/// its tail. ADR-0062 §3's A-G1 — no array-`General` variant, so this IS the
/// abstract "array of `T`" and spells as `array<T>`.
fn plain_array_of(elem: Fact) -> ShapeFact {
    ShapeFact::normalize(
        Vec::new(),
        Tail::Unsealed { key: steins_domain::KeyClass::ArrayKey, value: Some(Box::new(elem)) },
        Certainty::Maybe,
        false,
        Vec::new(),
    )
}

/// The scalar answer — `filter_var`'s result with neither array flag set. The rule
/// #608 landed, unchanged; leg (a) wraps its output rather than rewriting it.
fn filter_scalar_answer(kind: FilterKind, null_on_failure: bool, input: Option<&Fact>) -> Option<Fact> {
    let (success, proven) = filter_success(kind, input);
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
        // The `(string)` cast, through the domain's own grid. An ARRAY input
        // declines there — in both of its spellings, since the grid decomposes a
        // fact into the alternatives PHP converts one at a time — because PHP
        // writes `'Array'` with an `E_WARNING`; and that is exactly the input
        // class `filter_var` answers `false` for anyway.
        FilterKind::Raw => match input.and_then(|f| php_cast_fact(f, CastTarget::String)) {
            Some(cast) => (cast, true),
            None => general(Base::String),
        },
        // A sanitizer rewrites its input, so only the *totality* survives: every
        // scalar and `null` sanitizes to a string, and an array is the one input
        // class the fact has to rule out (`filter_var([1], FILTER_SANITIZE_EMAIL)`
        // is `false`).
        FilterKind::Sanitize => match input {
            Some(f) if fact_denotes_no_array(f) => {
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

/// Does this fact denote only scalars and `null`, at every alternative it admits?
///
/// The premise the sanitizer row on [`filter_var_transfer`] needs, and the one
/// place the ARRAY stratum has two spellings that both matter: a fully-known
/// array is a `Fact::Singleton(Val::Array(…))`, not only a `Fact::Shape`, so this
/// asks the values rather than the layer. The [`FilterKind::Raw`] row needs no
/// such test of its own — [`php_cast_fact`] already refuses an array to `string`
/// (PHP writes `'Array'` with an `E_WARNING`), which is the same refusal.
///
/// [`php_cast_fact`]: crate::coerce::php_cast_fact
fn fact_denotes_no_array(f: &Fact) -> bool {
    match f {
        Fact::Singleton(v) => !matches!(v, Val::Array(_)),
        Fact::OneOf(vals) => !vals.iter().any(|v| matches!(v, Val::Array(_))),
        // The abstract layers are scalar strata by construction: `Fact::Union`'s
        // own doc records that an array arm has no place in them.
        Fact::Refined { .. } | Fact::General { .. } | Fact::Union { .. } => true,
        Fact::Shape { .. } => false,
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

/// The [`FilterKind`]s a filter-argument expression may name — one for a constant,
/// both arms for a ternary over two recognized ones (issue #615 leg (b)), `None`
/// for anything this rung cannot key on.
///
/// A `|` is deliberately NOT walked here: filter ids are an enumeration, not a bit
/// field, and `FILTER_VALIDATE_INT | FILTER_VALIDATE_IP` names no filter.
fn filter_kinds(
    cx: &Cx,
    folder: &mut dyn Folder,
    value: &ArgValue,
    env: &HashMap<String, Known>,
) -> Option<Vec<FilterKind>> {
    if let ArgValue::Ternary { then_val, else_val, .. } = value {
        let mut out = filter_kinds(cx, folder, then_val, env)?;
        out.extend(filter_kinds(cx, folder, else_val, env)?);
        return Some(out);
    }
    // The NAME first, then the VALUE (ADR-0094 §2) — the same two readings the
    // flags leg takes, and for the same reason: a filter id held in a local, or
    // reached through a `use const` alias, is the id it is.
    if let Some(kind) = filter_kind(value) {
        return Some(vec![kind]);
    }
    Some(vec![filter_kind_of_id(int_of(cx, folder, value, env)?)?])
}

/// The names [`filter_kind_by_name`] answers for — the roster
/// [`filter_kind_of_id`] resolves an integer id through.
const FILTER_ID_NAMES: &[&str] = &[
    "FILTER_DEFAULT",
    "FILTER_SANITIZE_EMAIL",
    "FILTER_SANITIZE_URL",
    "FILTER_SANITIZE_ENCODED",
    "FILTER_SANITIZE_SPECIAL_CHARS",
    "FILTER_SANITIZE_FULL_SPECIAL_CHARS",
    "FILTER_SANITIZE_NUMBER_INT",
    "FILTER_SANITIZE_NUMBER_FLOAT",
    "FILTER_SANITIZE_ADD_SLASHES",
    "FILTER_VALIDATE_INT",
    "FILTER_VALIDATE_FLOAT",
    "FILTER_VALIDATE_BOOL",
    "FILTER_VALIDATE_EMAIL",
    "FILTER_VALIDATE_URL",
    "FILTER_VALIDATE_IP",
    "FILTER_VALIDATE_MAC",
    "FILTER_VALIDATE_DOMAIN",
];

/// The [`FilterKind`] an integer filter id names, resolved through the mined
/// table so the id roster has ONE spelling: the names [`filter_kind`] matches.
///
/// `FILTER_DEFAULT` and `FILTER_UNSAFE_RAW` share an engine value and a kind, so
/// the collision is not one. Every other modeled id is distinct.
fn filter_kind_of_id(id: i64) -> Option<FilterKind> {
    FILTER_ID_NAMES.iter().find_map(|name| {
        (engine_int(name)? == id).then(|| filter_kind_by_name(name))?
    })
}

/// The [`FilterKind`] a filter-argument CONSTANT names, or `None` for anything this
/// rung cannot key on.
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
    filter_kind_by_name(&r.raw)
}

/// The id roster, by name. Split out of [`filter_kind`] so the value-keyed
/// reading ([`filter_kind_of_id`]) resolves through the SAME roster rather than a
/// second transcription of it.
fn filter_kind_by_name(name: &str) -> Option<FilterKind> {
    match name {
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

/// The flags [`filter_var_transfer`] reads out of the third argument — the three
/// that change the ANSWER's shape. Every other accepted flag is a measured no-op
/// here and contributes nothing but permission to proceed.
///
/// A set of booleans rather than the engine's flag integer, deliberately: the
/// name-keyed roster ([`filter_flag_set`]) can tell `FILTER_FLAG_HOSTNAME`,
/// `_IPV4` and `_EMAIL_UNICODE` apart where a value-keyed one cannot — they share
/// one engine value. Since ADR-0094 §2 the rung ALSO reads a flags integer when
/// no name is spelled ([`filter_flags_of_bits`]); all three of those names are
/// modeled no-ops, so collapsing them there costs nothing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct FilterFlags {
    /// `FILTER_NULL_ON_FAILURE` — the failure value is `null`, not `false`.
    null_on_failure: bool,
    /// `FILTER_FORCE_ARRAY` — a non-array input is wrapped, an array walked.
    force_array: bool,
    /// `FILTER_REQUIRE_ARRAY` — a non-array input is a plain failure.
    require_array: bool,
}

impl FilterFlags {
    /// The `|` of two flag sets, which is what PHP's own `|` does to the bits.
    const fn union(self, other: FilterFlags) -> FilterFlags {
        FilterFlags {
            null_on_failure: self.null_on_failure || other.null_on_failure,
            force_array: self.force_array || other.force_array,
            require_array: self.require_array || other.require_array,
        }
    }
}

/// The flag sets the third argument may set — one entry per alternative a ternary
/// introduces — or `None` when it is a spelling this rung refuses (which declines
/// the whole rule — a flag it cannot read may be `FILTER_FLAG_STRIP_LOW`, which
/// rewrites the string and makes `FILTER_DEFAULT` stop being the identity).
///
/// Three accepted shapes, and only three: **absent**, a flag expression
/// ([`filter_flag_alternatives`]) written directly, and an **array literal** whose
/// only key is a literal `'flags'` holding one. See [`filter_var_transfer`] for why
/// every other spelling — a variable, a non-zero int literal, an `'options'` key —
/// is refused.
fn filter_var_flags(
    cx: &Cx,
    folder: &mut dyn Folder,
    value: Option<&ArgValue>,
    env: &HashMap<String, Known>,
) -> Option<Vec<FilterFlags>> {
    let Some(value) = value else { return Some(vec![FilterFlags::default()]) };
    if let ArgValue::Array(items) = value {
        let mut flags = vec![FilterFlags::default()];
        for (key, item) in items {
            let ArrayKey::Str(k) = key else { return None };
            if k.as_str() != Some("flags") {
                return None;
            }
            flags = filter_flag_alternatives(cx, folder, item, env)?;
        }
        return Some(flags);
    }
    filter_flag_alternatives(cx, folder, value, env)
}

/// How many alternative flag sets one flags expression may resolve to before the
/// rung stops walking it. Two nested ternaries already exceed anything a fixture
/// spells; the bound is here so a pathological expression cannot make the cross
/// product in [`filter_var_transfer`] grow with the source.
const FILTER_FLAG_ALTERNATIVE_CAP: usize = 8;

/// One flags EXPRESSION as the alternatives it may take, resolved from the syntax
/// (issue #615 leg (b)).
///
/// Two composers, and each is a different kind of combination:
///
/// * a **`|` chain** combines flags into ONE set — PHP's own `|` over the bits, and
///   [`FilterFlags::union`] over the roster's booleans;
/// * a **`?:` ternary** offers two sets as ALTERNATIVES, which the caller answers
///   separately and joins.
///
/// **The roster resolves by constant NAME first, and by VALUE when no name is
/// spelled.** The name reading comes first because it keeps `FILTER_FLAG_HOSTNAME`,
/// `_IPV4` and `_EMAIL_UNICODE` — which share one engine value — distinguishable,
/// and because it carries the constant's own shadow discipline. The value reading
/// is what ADR-0094 §2 made possible: `$nullFilter = \FILTER_NULL_ON_FAILURE`
/// binds a fact now, where it bound none under issue #168, so a const-valued local
/// is read instead of declining — and so is the bare integer PHP itself sees.
fn filter_flag_alternatives(
    cx: &Cx,
    folder: &mut dyn Folder,
    value: &ArgValue,
    env: &HashMap<String, Known>,
) -> Option<Vec<FilterFlags>> {
    match value {
        ArgValue::Ternary { then_val, else_val, .. } => {
            let mut out = filter_flag_alternatives(cx, folder, then_val, env)?;
            out.extend(filter_flag_alternatives(cx, folder, else_val, env)?);
            (out.len() <= FILTER_FLAG_ALTERNATIVE_CAP).then_some(out)
        }
        ArgValue::Binary { op: ValueOp::BitOr, lhs, rhs } => {
            let (ls, rs) = (
                filter_flag_alternatives(cx, folder, lhs, env)?,
                filter_flag_alternatives(cx, folder, rhs, env)?,
            );
            if ls.len() * rs.len() > FILTER_FLAG_ALTERNATIVE_CAP {
                return None;
            }
            Some(ls.iter().flat_map(|l| rs.iter().map(|r| l.union(*r))).collect())
        }
        // A recognized NAME first — it keeps `FILTER_FLAG_HOSTNAME`, `_IPV4` and
        // `_EMAIL_UNICODE` (one engine value, three names) distinguishable, and it
        // is the reading that carries the constant's own shadow discipline.
        // Otherwise the VALUE, which is what ADR-0094 §2 made available: a local
        // holding `\FILTER_NULL_ON_FAILURE`, a `use const` alias, a same-file
        // `const` of the project's own. The bits are decomposed against the same
        // roster, so an unmodeled bit declines exactly as an unmodeled name does.
        _ => match filter_flag_set(value) {
            Some(set) => Some(vec![set]),
            None => Some(vec![filter_flags_of_bits(
                int_of(cx, folder, value, env)?,
            )?]),
        },
    }
}

/// A flags expression's proven integer value, or `None`.
fn int_of(
    cx: &Cx,
    folder: &mut dyn Folder,
    value: &ArgValue,
    env: &HashMap<String, Known>,
) -> Option<i64> {
    match transfer_arg_fact(cx, folder, value, env, None)? {
        Fact::Singleton(Val::Int(n)) => Some(n),
        _ => None,
    }
}

/// **A `FILTER_*` flag bit field, decomposed against the modeled roster.**
///
/// The bit values are not transcribed: each comes from the mined engine-constant
/// table (ADR-0094 §2), so this reading and the name-keyed one above cannot
/// disagree about what a flag is worth. A bit outside the roster declines the
/// whole call — the same invariant [`filter_flag_set`] holds, and for the same
/// reason: an unread flag may be `FILTER_FLAG_STRIP_LOW`, which rewrites the
/// string and makes `FILTER_DEFAULT` stop being the identity.
fn filter_flags_of_bits(bits: i64) -> Option<FilterFlags> {
    let mut out = FilterFlags::default();
    let mut rest = bits;
    let take = |name: &str, rest: &mut i64| -> bool {
        // A roster name the TABLE has no row for subtracts nothing. That is not a
        // hole: its bit, if the caller set it, is then still in `rest` when the
        // `rest == 0` check below runs, and the call declines — the same refusal
        // by a shorter route. Declining on the name instead would let ONE
        // unreadable member blind the whole roster, and one is unreadable by
        // design: `FILTER_FLAG_GLOBAL_RANGE`'s value MOVED across the supported
        // minors (268435456 at 8.2–8.4, 536870912 at 8.5), so ADR-0094 §2 refuses
        // it a row — which is exactly a bit no decomposition may claim to know.
        let Some(bit) = engine_int(name) else { return false };
        if bit == 0 || *rest & bit != bit {
            return false;
        }
        *rest &= !bit;
        true
    };
    out.null_on_failure = take("FILTER_NULL_ON_FAILURE", &mut rest);
    out.force_array = take("FILTER_FORCE_ARRAY", &mut rest);
    out.require_array = take("FILTER_REQUIRE_ARRAY", &mut rest);
    for name in FILTER_FLAG_NO_OPS {
        take(name, &mut rest);
    }
    (rest == 0).then_some(out)
}

/// The flags that restrict which inputs *validate* without touching the result's
/// type — measured no-ops for every cell of [`filter_var_transfer`]'s grid, and
/// the same roster [`filter_flag_set`] accepts by name.
const FILTER_FLAG_NO_OPS: &[&str] = &[
    "FILTER_FLAG_ALLOW_OCTAL",
    "FILTER_FLAG_ALLOW_HEX",
    "FILTER_FLAG_ALLOW_FRACTION",
    "FILTER_FLAG_ALLOW_THOUSAND",
    "FILTER_FLAG_ALLOW_SCIENTIFIC",
    "FILTER_FLAG_IPV4",
    "FILTER_FLAG_IPV6",
    "FILTER_FLAG_HOSTNAME",
    "FILTER_FLAG_EMAIL_UNICODE",
    "FILTER_FLAG_NO_PRIV_RANGE",
    "FILTER_FLAG_NO_RES_RANGE",
    "FILTER_FLAG_GLOBAL_RANGE",
    "FILTER_FLAG_PATH_REQUIRED",
    "FILTER_FLAG_QUERY_REQUIRED",
];

/// One engine constant's integer value from the mined table (ADR-0094 §2), or
/// `None` when the table has no int row for it — an extension the mining build
/// lacked, a name outside its minor range, or a value-less row (issue #718),
/// which records a departure and carries no literal at all.
fn engine_int(name: &str) -> Option<i64> {
    match steins_catalog::engine_constant(name)?.value? {
        steins_catalog::ConstValue::Int(n) => Some(n),
        _ => None,
    }
}

/// One flag CONSTANT as a [`FilterFlags`], or `None` for a spelling outside the
/// roster.
///
/// The accepted list is the three answer-shaping flags plus the flags that restrict
/// which inputs *validate* without touching the result's type — measured no-ops for
/// every cell of the grid on [`filter_var_transfer`]. An unrecognized name declines
/// the whole call rather than being ignored, and that invariant is load-bearing.
fn filter_flag_set(value: &ArgValue) -> Option<FilterFlags> {
    let none = FilterFlags::default();
    if let ArgValue::Int(0) = value {
        return Some(none);
    }
    let ArgValue::GlobalConst(r) = value else { return None };
    if !matches!(r.kind, RefKind::FullyQualified | RefKind::Unqualified) {
        return None;
    }
    match r.raw.as_str() {
        "FILTER_NULL_ON_FAILURE" => Some(FilterFlags { null_on_failure: true, ..none }),
        "FILTER_FORCE_ARRAY" => Some(FilterFlags { force_array: true, ..none }),
        "FILTER_REQUIRE_ARRAY" => Some(FilterFlags { require_array: true, ..none }),
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
        | "FILTER_FLAG_QUERY_REQUIRED" => Some(none),
        _ => None,
    }
}
