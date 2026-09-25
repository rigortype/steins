use std::collections::HashMap;

use steins_domain::{Base, Fact, IntRange, Refinement, StrPreds, Val};
use steins_syntax::ArgValue;

use crate::cx::Cx;
use crate::env::{Known, Store};
use crate::fact_is_int;
use crate::fold::Folder;
use crate::shape_projection::shape_value_union;
use crate::transfers::transfer_arg_fact;

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
pub(super) fn str_pred_transfer(
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
