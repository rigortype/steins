//! The folding allowlist (ADR-0008, ADR-0028, ADR-0066): which builtins the
//! constant-folding seam may evaluate, and the **portability class** each one
//! sits in.
//!
//! [`portability_class`] is the primitive; [`foldable`], [`portable`] and the
//! three name accessors are derived from it. The three lists (`PORTABLE`,
//! `REFUSED`, `UNVERIFIED`) and the refused rows' witnesses live here and
//! nowhere else — the tests at the bottom own the counts. The crate doc
//! records the exclusions and the evidence behind them.

/// Whether `name` is on the folding allowlist (case-insensitive).
///
/// A `true` here is *permission to fold*, not a promise the call folds: the
/// engine still requires a non-user callee and all-literal arguments (scalar
/// or recursively concrete arrays, permitting `sprintf`/`str_replace`/
/// `in_array`/`count`/`implode`, issue #39). A folded result may likewise be
/// an array (ADR-0028's 2026-08-14 amendment, issue #330).
///
/// The allowlist is the union of `PORTABLE`, `REFUSED`,
/// `UNVERIFIED` (issue #64; amendment §4 added the third), so
/// [`foldable`] is a *derived* predicate and [`portability_class`] the primitive.
/// `mb_*`, locale-sensitive, and `nondet` functions are excluded.
#[must_use]
pub fn foldable(name: &str) -> bool {
    portability_class(name).is_some()
}

/// The number of names on the folding allowlist (ADR-0054 §9.6 freshness
/// context, the [`foldable`] twin of [`hierarchy_entry_count`]): the union of
/// the three portability classes, which are disjoint by construction.
///
/// [`hierarchy_entry_count`]: crate::hierarchy_entry_count
#[must_use]
pub fn foldable_entry_count() -> usize {
    PORTABLE.len() + REFUSED.len() + UNVERIFIED.len()
}

/// Which **portability class** a foldable name sits in — the **primitive** the
/// folding allowlist is derived from (ADR-0028's 2026-08-14 amendment §4).
///
/// `None` means not on the allowlist. The three `Some` arms classify
/// *evidence*, not behaviour: only [`PortabilityClass::Portable`] changes what
/// the fold gate admits; the other two are mechanically identical but kept
/// apart to keep the refused rows' one-divergence-per-row discipline auditable.
///
/// The class was called `WidthClass` while every row in it was about the
/// engine's integer width. `preg_split` ended that: it is refused because one
/// build's PCRE has a JIT and the other's does not, which is a property of the
/// engine and not of its word size. The question the gate actually asks has
/// always been *may an engine other than the project's own fold this name* —
/// see [`RefusalAxis`] for what a refusal can be about, and for the axes this
/// instrument cannot see.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PortabilityClass {
    /// **Measured, and it agrees.** Differential probes (php-wasm 0.1.0,
    /// `PHP_INT_SIZE = 4`, vs `php` 8.5.x at 8) found identical value and type
    /// tag, or a decline, for every argument the range guard admits.
    Portable,
    /// **Measured, and it disagrees.** At least one probe found both engines
    /// silently differing; [`refusal`] names the axis and the witness. Folds
    /// only on a provably 64-bit engine.
    Refused,
    /// **Not measured.** Folds only on a provably 64-bit engine, the same gate
    /// [`PortabilityClass::Refused`] rides. See `UNVERIFIED`.
    Unverified,
}

/// What a [`PortabilityClass::Refused`] row is refused *about* — the typed half
/// of the one-witness-per-row discipline, so a reader can tell an arithmetic
/// hazard from a build-configuration one without parsing prose.
///
/// # The axes this instrument cannot see
///
/// The differential is two engines, and they are alike in more ways than a
/// user's two runtimes are:
///
/// * **The operating system.** Both are POSIX — `DIRECTORY_SEPARATOR` and
///   `escapeshellarg("a b'c")` agree byte for byte, and `PHP_OS_FAMILY` differs
///   only as `Darwin` against `Unknown`. Windows is a third machine nobody
///   probes, so a name whose value is OS-shaped cannot be *refused by
///   measurement* — it is excluded from the allowlist by argument, the way
///   `strcmp` is excluded for promising only a sign.
/// * **An ini both builds happen to share.** Both report `precision = 14` and
///   `serialize_precision = -1`, so a name that renders floats agrees here and
///   would not on a project that sets either differently. The catalog names
///   that exposure per row rather than pretending the probe covers it.
///
/// A missing variant is therefore not an oversight: this enum lists what a
/// refusal *has been* about, and grows when a probe finds a new kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefusalAxis {
    /// The engine's `PHP_INT_SIZE`, directly or through a coercion it decides.
    IntegerWidth,
    /// A build option: something the two engines were *compiled* differently
    /// for, at the same version and the same ini.
    BuildOption,
}

impl RefusalAxis {
    /// The stable machine-readable spelling, for an envelope that carries the
    /// axis as data (the playground's boot object). Owned by the type so no
    /// consumer re-derives it — a second `match` at a crate boundary is how a
    /// typed concept turns back into a string nobody checks.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::IntegerWidth => "integer_width",
            Self::BuildOption => "build_option",
        }
    }
}

/// The recorded divergence behind a [`PortabilityClass::Refused`] row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Refusal {
    /// What kind of engine difference produced it.
    pub axis: RefusalAxis,
    /// The witness, in one line: the call, and what each engine answered. Every
    /// refused row has one — `every_refused_row_carries_its_witness` is the
    /// discipline made mechanical rather than editorial.
    pub witness: &'static str,
}

/// The refused rows, declared once: the name, the axis, and the witness.
///
/// The list and the lookup are generated from the SAME entries. They were two
/// hand-maintained tables for one afternoon, and that is one afternoon too
/// long — a refused row added to the names list without a witness would have
/// compiled, and the test that catches it only runs after the fact.
macro_rules! refused_rows {
    ($($name:literal => ($axis:expr, $witness:literal)),* $(,)?) => {
        /// The **refused rows** of the portability classification: a name folds
        /// on a provably 64-bit engine and declines anywhere else, because a
        /// divergence is on record. Every probe passes the range guard; only
        /// refusing the name can exclude it. Each divergence is *silent* —
        /// nothing throws, widens, or warns (ADR-0066 §4). Probes: 64-bit `php`
        /// 8.5.x against 32-bit php-wasm 0.1.0. See [`refusal`] for each row's
        /// axis and witness, which is where the reasons live now that they are
        /// data rather than prose.
        pub(crate) const REFUSED: &[&str] = &[$($name),*];

        /// The refusal behind `name` (case-insensitive), or `None` when `name`
        /// is not a refused row — including when it is portable, unverified, or
        /// off the allowlist entirely.
        #[must_use]
        pub fn refusal(name: &str) -> Option<Refusal> {
            match name.to_ascii_lowercase().as_str() {
                $($name => Some(Refusal { axis: $axis, witness: $witness }),)*
                _ => None,
            }
        }
    };
}

refused_rows! {
    // issue #64 — the first arithmetic rows
    "abs" => (RefusalAxis::IntegerWidth, "abs(\"3000000000\") is int(3000000000) / float(3000000000) — a numeric string is coerced by the engine's own width, and the type tag flips"),
    "intval" => (RefusalAxis::IntegerWidth, "intval(\"3000000000\") is 3000000000 / the saturated 2147483647"),
    "sprintf" => (RefusalAxis::IntegerWidth, "sprintf(\"%x\", -1) is \"ffffffffffffffff\" / \"ffffffff\" — %b/%x/%o/%u render the machine word, and %d re-imports intval's saturation"),
    // issue #78 — machine-word rendering and its inverse
    "dechex" => (RefusalAxis::IntegerWidth, "dechex(-1) is \"ffffffffffffffff\" / \"ffffffff\", and dechex(-2147483647) diverges too — an in-range argument suffices"),
    "decbin" => (RefusalAxis::IntegerWidth, "decbin(-1) is 64 ones / 32 ones"),
    "decoct" => (RefusalAxis::IntegerWidth, "decoct(-1) is \"1777777777777777777777\" / \"37777777777\""),
    "bindec" => (RefusalAxis::IntegerWidth, "bindec(\"11111111111111111111111111111111\") is int(4294967295) / float(4294967295) — the type tag flips from a plain string"),
    "hexdec" => (RefusalAxis::IntegerWidth, "hexdec(\"FFFFFFFF\") is int(4294967295) / float(4294967295)"),
    // issue #78 — a `long` hiding inside string work
    "version_compare" => (RefusalAxis::IntegerWidth, "version_compare(\"2147483647\", \"2147483648\") is -1 / 0 — php-src compares each numeric run through a C long, so two oversized runs both saturate and compare equal"),
    // issue #354 — a width-typed numeric string, and a PCRE build option
    "range" => (RefusalAxis::IntegerWidth, "range(\"3000000000\", \"3000000000\") is [int(3000000000)] / [float(3000000000.0)] — its bounds are declared string|int|float, so the machine types the numeric string"),
    "preg_split" => (RefusalAxis::BuildOption, "preg_split(\"/(*LIMIT_MATCH=1)a/\", \"aaa\") splits / is false — PCRE2's JIT ignores the inline limit verbs its interpreter honours, and adding (*NO_JIT) makes both engines agree"),
    // issue #382 — the same matcher as preg_split, so the same build option
    "preg_match" => (RefusalAxis::BuildOption, "preg_match(\"/(*LIMIT_MATCH=1)a/\", \"aaa\") is 1 / false, and (*LIMIT_RECURSION=1) diverges the same way — one PCRE2 build JITs and ignores the inline limit verbs the other honours"),
    // wave 3 — measured by the generated probe, not by a hand-written family
    "preg_match_all" => (RefusalAxis::BuildOption, "preg_match_all(\"/(*LIMIT_MATCH=1)a/\", \"aaa\") is 3 / false — the third name on the axis preg_split opened, and for the same reason: one PCRE2 build JITs past the inline limit verbs the other honours"),
    "json_decode" => (RefusalAxis::IntegerWidth, "json_decode(\"3000000000\") is int(3000000000) / float(3000000000.0) — the document is the same text and the value's TYPE TAG is the parsing engine's word size, with no flag, option or unusual input involved"),
    "json_encode" => (RefusalAxis::IntegerWidth, "json_encode(\"3000000000\", JSON_NUMERIC_CHECK|JSON_PRESERVE_ZERO_FRACTION) is \"3000000000\" / \"3000000000.0\" — NUMERIC_CHECK retypes the numeric string, the narrow engine has no int that wide so it becomes a float, and PRESERVE_ZERO_FRACTION renders the fraction; NEITHER FLAG ALONE DIVERGES"),
}

/// The portability class of `name` (case-insensitive), or `None` when not on
/// the allowlist. The lists are disjoint, so the search order below is a cost
/// decision, not a precedence rule.
#[must_use]
pub fn portability_class(name: &str) -> Option<PortabilityClass> {
    let listed = |list: &[&str]| list.iter().any(|&f| name.eq_ignore_ascii_case(f));
    if listed(PORTABLE) {
        Some(PortabilityClass::Portable)
    } else if listed(REFUSED) {
        Some(PortabilityClass::Refused)
    } else if listed(UNVERIFIED) {
        Some(PortabilityClass::Unverified)
    } else {
        None
    }
}

/// Whether folding `name` is **safe on a 32-bit engine** (case-insensitive),
/// given the caller already applied the argument range guard.
///
/// # The rule
///
/// Portable means: for every argument tuple where every integer (values and
/// array keys, recursively) lies within `[-(2^31 - 1), 2^31 - 1]`, a 32-bit
/// engine returns the **identical value and type tag** a 64-bit engine
/// returns, or **declines** (ADR-0066 §4). The lower bound is `-(2^31 - 1)`,
/// not `-2^31`, to keep the `abs`-shaped boundary flip unreachable.
///
/// Earned by differential probes against php-wasm 0.1.0 (`PHP_INT_SIZE = 4`)
/// and `php` 8.5.x (`PHP_INT_SIZE = 8`) — **1073 adversarial tuples** behind the
/// classification as a whole, summed and defined by ADR-0066's probe ledger.
/// This row's own evidence is its line in its round's disposition table, and
/// that is the thing to read: the total says how hard the instrument was used,
/// not what it found about any one name.
///
/// A `false` here is a refusal to certify, not a claim of width-sensitivity.
/// Default-deny: unclassified names fold only on a provably 64-bit engine.
#[must_use]
pub fn portable(name: &str) -> bool {
    PORTABLE.iter().any(|&f| name.eq_ignore_ascii_case(f))
}

// A private `width_refused` predicate (complement of `portable`) lived here;
// `portability_class(name) == Some(Refused)` replaced it since "not portable"
// and "refused" stopped being the same question once unverified rows existed.

/// The verified portable names, in catalog order — the *extension* of
/// [`portable`]. The playground boundary widget uses this so its displayed
/// subset cannot drift from the folding gate (issue #64).
#[must_use]
pub fn portable_names() -> &'static [&'static str] {
    PORTABLE
}

/// The refused names, in catalog order — [`PortabilityClass::Refused`] rows: folds a
/// 32-bit engine loses **because a divergence is on record**. See
/// `REFUSED` for each refusal's probe evidence.
#[must_use]
pub fn refused_names() -> &'static [&'static str] {
    REFUSED
}

/// The unverified names, in catalog order — [`PortabilityClass::Unverified`] rows,
/// sibling to [`refused_names`]. See `UNVERIFIED` for what
/// "unverified" commits the catalog to (deliberately nothing).
#[must_use]
pub fn unverified_names() -> &'static [&'static str] {
    UNVERIFIED
}

/// The verified portable half of the folding allowlist (issue #64), grouped
/// by *why* the width cannot reach the result:
///
/// * **string in, string out**: byte transforms of the subject, plus
///   `ucwords`/`strtr`/`preg_quote`/`addslashes`/`urlencode`/`urldecode`/
///   `rawurlencode`/`rawurldecode`/`base64_encode`/`base64_decode` (`ucwords`
///   is ASCII-only since PHP 8.2) and `str_increment`/`str_decrement` (8.3+,
///   digits carried in the *string*, no integer path to overflow).
/// * **result bounded by the input**: `strlen`/`count`, bounded by a string
///   fitting in memory and the fold seam's 256-entry budget, never 2^31.
/// * **int parameters, in-range results**: `substr`/`str_repeat`/`intdiv`/
///   `str_pad`/`substr_replace` (scalar subject) — in-range in, in-range out;
///   the only divergence is a *decline* (`TypeError` on 32-bit where 64-bit
///   answers).
/// * **no integer in the result at all**: `floatval`, `boolval`, `strval`
///   (same `precision` ini on both), `in_array` (php-src's
///   `zendi_smart_strcmp` compares oversized numeric strings as strings on
///   both machines), `str_starts_with`/`str_contains`/`str_ends_with`/`gettype`.
/// * **array results the width cannot reach** (issue #354): `str_split` and
///   `array_fill` take an `int` parameter but never coerce a *value* by it —
///   an oversized argument is a `TypeError` on the narrow engine, which is a
///   decline; and `array_unique` compares string casts without retyping what
///   it keeps. None of the three declares a `string|int|float` parameter,
///   which is the one place the engine's own width picks a numeric string's
///   type, and is why `range` is refused while these three are not.
///
/// `substr_replace` above is scalar-subject only; handed an array it returns
/// an array, which **folds** since ADR-0028's 2026-08-14 amendment (issue
/// #330) with no re-verification, since the array form is identical on both
/// engines. Issue #354 re-probed both array-returning rows *bytewise* — array
/// elements cross the seam with no per-element type tag, so an `int`/`float`
/// flip inside a result is legible only in the response bytes — and found
/// them unchanged.
///
/// * **a second spelling of a name already here**: `join`, `chop`, `sizeof` and
///   `doubleval` are PHP's own aliases for `implode`, `rtrim`, `count` and
///   `floatval` — one C handler reached by two names, so an alias cannot
///   diverge from its target on any machine. They are listed anyway rather than
///   resolved through an alias table, because `foldable` matches a spelling and
///   a row that claims a width owes probes; each earned its own, by running its
///   target's recorded probe family against the alias spelling. The four
///   reproduced their targets' ADR-0066 counts exactly, and replied
///   byte-identically to the target on both engines across all 45 tuples. A
///   scan of every internal function's arginfo against the allowlist found no
///   fifth pair: `key_exists`, `is_integer`/`is_long` and `is_double` alias
///   names that are not admitted, so they enter with their targets or not at all.
///
/// `array_unique` carries one exposure worth naming: its default `SORT_STRING`
/// compares string casts, so `precision` decides how many elements survive.
/// That is the same ini `strval` and `implode` already fold under (both engines
/// report `precision = 14`), escalated from *how a float is spelled* to *how
/// long the array is*. Closing that seam is a decision about all three names at
/// once, not about this one.
pub(crate) const PORTABLE: &[&str] = &[
    "strtolower",
    "strtoupper",
    "ucfirst",
    "lcfirst",
    "trim",
    "ltrim",
    "rtrim",
    "strrev",
    "substr",
    "str_replace",
    "str_repeat",
    "implode",
    "strlen",
    "intdiv",
    "floatval",
    "strval",
    "boolval",
    "in_array",
    "count",
    // issue #78 — byte transforms of the subject
    "ucwords",
    "strtr",
    "preg_quote",
    "addslashes",
    "urlencode",
    "urldecode",
    "rawurlencode",
    "rawurldecode",
    "base64_encode",
    "base64_decode",
    // issue #78 — string arithmetic (8.3+)
    "str_increment",
    "str_decrement",
    // issue #78 — int parameters, in-range results
    "str_pad",
    "substr_replace",
    // issue #78 — no integer in the result at all
    "str_starts_with",
    "str_contains",
    "str_ends_with",
    "gettype",
    // issue #354 — array results with no width-sensitive path to a value
    "str_split",
    "array_fill",
    "array_unique",
    // the alias slice — one C handler under a second spelling
    "join",
    "chop",
    "sizeof",
    "doubleval",
    // wave 2 — an `int` parameter whose oversized argument is a TypeError, not
    // a value: the offset family and the float roundings
    "strpos",
    "stripos",
    "strrpos",
    "round",
    "floor",
    "ceil",
    // issue #382 — a callable-taking name, admitted only once the seam could
    // refuse the callback argument. `array_filter` retypes nothing: it selects
    // entries by PHP's own falsiness and preserves the keys it keeps, so no
    // integer in the result was computed by the machine. The one decline is the
    // narrow engine having no key after its own `PHP_INT_MAX`.
    "array_filter",
    // issue #330's two UNVERIFIED rows, measured at last (ADR-0066's 2026-08-16
    // amendment) — the first names admitted by the generated probe rather than
    // by a hand-written tuple list. `array_merge` renumbers integer keys and
    // keeps string ones under PHP's own last-wins, and neither rule consults the
    // machine word; `explode`'s `int $limit` is the shape wave 2 admitted six
    // times over, where an oversized argument is a `TypeError` and therefore a
    // decline.
    "array_merge",
    "explode",
];

/// The **unverified rows** of the portability classification (ADR-0028's 2026-08-14
/// amendment §4, issue #330) — the third class, which claims nothing. Unlike
/// `PORTABLE`/`REFUSED`, the correct probe count behind a row here is
/// **zero**: not measured, so the name folds only on a provably 64-bit engine.
/// A probe finding agreement moves a row to `PORTABLE`, a divergence to
/// `REFUSED`.
///
/// **The class is empty today, and that is the class working rather than the
/// class being retired.** It held exactly two rows, `array_merge` and `explode`,
/// admitted unmeasured by that amendment because their Rust rungs were
/// type-level and a fold could only be strictly stronger. Both were measured in
/// issue #382 (13 and 25 tuples, both calling conventions, zero silent and zero
/// reverse) and left for `PORTABLE`, which is the only way out this class has.
/// The five names it deferred before them — `range`, `preg_split`, `str_split`,
/// `array_unique`, `array_fill` — left the same way in issue #354, three to
/// `PORTABLE` and two to `REFUSED`.
///
/// Nothing has ever been promoted *into* here, and nothing should be casually:
/// a row enters only by being admitted **unmeasured**, which is a debt the next
/// probe run pays. An empty list is what "no outstanding debt" looks like, and
/// the class stays so the next admission has somewhere honest to sit.
pub(crate) const UNVERIFIED: &[&str] = &[];

#[cfg(test)]
mod tests {
    use crate::{effect_labels, param_facts};
    use super::{
        PORTABLE, PortabilityClass, REFUSED, RefusalAxis, UNVERIFIED, foldable,
        foldable_entry_count, portability_class, portable, portable_names, refusal, refused_names,
        unverified_names,
    };

    /// Classes are pairwise DISJOINT, no name listed twice, size is 63
    /// (ADR-0066, plus ADR-0028's 2026-08-14 wave 1, issue #354, the four
    /// aliases that slice's coverage survey turned up, and wave 2's six).
    ///
    /// This is where the three numbers are OWNED. The playground's smoke
    /// scripts and the WASM boot test check that they travel; only this test
    /// says what they are.
    #[test]
    fn the_portability_classes_partition_the_allowlist() {
        for (list, class, label) in [
            (PORTABLE, PortabilityClass::Portable, "PORTABLE"),
            (REFUSED, PortabilityClass::Refused, "REFUSED"),
            (UNVERIFIED, PortabilityClass::Unverified, "UNVERIFIED"),
        ] {
            for name in list {
                assert!(foldable(name), "{name} is classified but not foldable");
                assert_eq!(
                    portability_class(name),
                    Some(class),
                    "{name} is listed in {label} but classifies elsewhere"
                );
                assert_eq!(
                    list.iter().filter(|&n| n == name).count(),
                    1,
                    "{name} is listed twice in {label}"
                );
                for (other, other_label) in [
                    (PORTABLE, "PORTABLE"),
                    (REFUSED, "REFUSED"),
                    (UNVERIFIED, "UNVERIFIED"),
                ] {
                    if other_label != label {
                        assert!(
                            !other.contains(name),
                            "{name} is classified twice ({label} and {other_label})"
                        );
                    }
                }
            }
        }
        assert_eq!(PORTABLE.len(), 53, "the verified portable subset");
        assert_eq!(REFUSED.len(), 15, "the refused rows");
        assert_eq!(
            UNVERIFIED.len(),
            0,
            "the class is EMPTY, not gone: a row enters only by being admitted unmeasured, \
             and there is no such debt outstanding"
        );
        assert_eq!(
            foldable_entry_count(),
            68,
            "the allowlist size the ADR-0066 amendments tabulate, plus wave 1, issue #354, its \
             aliases, wave 2, and the two names the seam's shape gate unblocked (issue #382)"
        );
        assert_eq!(
            PORTABLE.len() + REFUSED.len() + UNVERIFIED.len(),
            foldable_entry_count(),
            "the count is the three lists and nothing else"
        );
    }

    /// The unverified class is **empty**, and every name that ever sat in it
    /// left through a probe.
    ///
    /// The class claims nothing, so its rows cost precision until measured; the
    /// only way out is evidence. `array_merge` and `explode` were the last two
    /// and left in issue #382 (13 and 25 generated tuples, both calling
    /// conventions, zero silent and zero reverse). The five before them left in
    /// issue #354, three to `PORTABLE` and two to `REFUSED`.
    #[test]
    fn the_unverified_class_is_empty_and_everything_left_it_by_probe() {
        assert!(UNVERIFIED.is_empty(), "an unmeasured row is a debt, and there is none");
        assert!(unverified_names().is_empty());
        // The two that left last: portable now, and still catalogued pure.
        for name in ["array_merge", "explode", "Array_Merge", "EXPLODE"] {
            assert_eq!(portability_class(name), Some(PortabilityClass::Portable));
            assert!(foldable(name), "{name} folds on a 64-bit engine");
            assert!(portable(name), "{name} folds in the browser too now");
            assert_eq!(effect_labels(name), Some(&[][..]), "{name} is catalogued pure");
        }
        assert!(!REFUSED.contains(&"explode"));
        assert!(!REFUSED.contains(&"array_merge"));
        // The five this class deferred earlier landed in the class their
        // evidence chose, and none of them passed back through here.
        for name in ["str_split", "array_unique", "array_fill"] {
            assert_eq!(portability_class(name), Some(PortabilityClass::Portable), "{name} probed clean");
        }
        for name in ["range", "preg_split"] {
            assert_eq!(
                portability_class(name),
                Some(PortabilityClass::Refused),
                "{name} probed dirty"
            );
            assert!(refusal(name).is_some(), "{name} carries its witness");
        }
    }

    /// Every refused row carries a witness, and only refused rows do. The
    /// ADR-0061 one-divergence-per-row discipline has been an editorial promise
    /// since the first refused row; this is it made mechanical. A witness names
    /// the call and what each engine answered, so `/` appears in every one.
    #[test]
    fn every_refused_row_carries_its_witness() {
        for name in refused_names() {
            let r = refusal(name).unwrap_or_else(|| panic!("{name} is refused with no witness"));
            assert!(
                r.witness.contains(name),
                "{name}'s witness must show the call it is about: {}",
                r.witness
            );
            assert!(
                r.witness.contains(" / "),
                "{name}'s witness must show BOTH engines' answers: {}",
                r.witness
            );
            assert_eq!(refusal(&name.to_uppercase()), Some(r), "{name} matches case-insensitively");
        }
        // Only a refused row has one. A portable row has nothing to witness, and
        // an unverified row's correct witness count is zero by definition.
        for name in portable_names().iter().chain(unverified_names()) {
            assert_eq!(refusal(name), None, "{name} is not a refused row");
        }
        assert_eq!(refusal("strtolower"), None);
        assert_eq!(refusal("some_unknown_fn"), None);
    }

    /// The axes, and the count per axis. `preg_split` is the whole reason this
    /// enum exists: one row that is not about the integer width at all.
    #[test]
    fn the_refusal_axes_partition_the_refused_rows() {
        let axis_of = |n: &str| refusal(n).expect("refused").axis;
        for name in
            ["abs", "intval", "sprintf", "dechex", "decbin", "decoct", "bindec", "hexdec",
             "version_compare", "range"]
        {
            assert_eq!(axis_of(name), RefusalAxis::IntegerWidth, "{name} is an arithmetic row");
        }
        for name in ["preg_split", "preg_match", "preg_match_all"] {
            assert_eq!(axis_of(name), RefusalAxis::BuildOption, "{name} runs PCRE");
        }
        // …and the JSON pair is the word size, not the build.
        for name in ["json_encode", "json_decode"] {
            assert_eq!(axis_of(name), RefusalAxis::IntegerWidth, "{name} is about the machine word");
        }
        let build = refused_names()
            .iter()
            .filter(|n| refusal(n).expect("refused").axis == RefusalAxis::BuildOption)
            .count();
        assert_eq!(
            build, 3,
            "three rows are about a build option, and all three run the same PCRE"
        );
    }

    /// **A foldable name that takes a callable is gated at the seam, and the
    /// catalog's job is to keep that gate reachable.**
    ///
    /// The folding allowlist gates the *callee*. A builtin that takes a callable
    /// smuggles a SECOND callee past that gate as an ordinary string argument,
    /// and the fold seam hands string arguments to the runner verbatim, which
    /// calls them. Measured, on a branch that briefly admitted `array_filter`
    /// with no gate at all:
    ///
    /// * `array_filter(["a", "b"], "var_dump")` — the callback's output landed
    ///   on stdout ahead of the JSON-RPC reply, desynced the NDJSON stream and
    ///   poisoned the sidecar, degrading the whole run to the sound subset.
    /// * `array_filter(["PATH"], "getenv")` — folded to `list{'PATH'}`, which
    ///   is to say `getenv` ran inside the analysis and its answer reached the
    ///   value domain. `system`, `unlink` and the rest are the same call.
    ///
    /// Since issue #382 the seam refuses such a call unless every callable
    /// position is **absent or a literal `null`** (`fold_admitted_by_shape`,
    /// reading this crate's mined [`param_facts`] rather than the curated
    /// [`invocation_shape`], which has one position per row and could not
    /// express `session_set_save_handler`'s seven). Two things have to hold on
    /// this side for that gate to be reachable at all, and neither is implied by
    /// the other:
    ///
    /// 1. every callable position of a foldable name is **optional** — a
    ///    required one would mean the gate refuses every call, so the row folds
    ///    nothing and says otherwise;
    /// 2. the name has an [`invocation_shape`] row, so the effects and throws
    ///    passes read the same argument as a callback that the fold seam
    ///    refuses to fill.
    ///
    /// The mined column is *declared* callables, which is sound and not
    /// complete: `array_udiff` takes its comparator at a variadic `mixed` tail
    /// and `preg_replace_callback_array` takes its callables as array values.
    /// `a_variadic_mixed_tail_on_a_foldable_name_is_argued_for` is the tripwire
    /// for the first shape; `out_params` keeps the second off the list. Neither
    /// is this test's claim.
    #[test]
    fn a_foldable_names_callable_positions_are_gateable() {
        use crate::invocation_shape;
        for name in PORTABLE.iter().chain(REFUSED).chain(UNVERIFIED) {
            let Some(facts) = param_facts(name) else { continue };
            for &p in facts.callable {
                assert!(
                    facts.optional.contains(&p),
                    "{name} folds and REQUIRES a callable at position {p}: the seam's shape gate \
                     would refuse every call, so the row folds nothing"
                );
                assert_eq!(
                    invocation_shape(name).map(|s| s.callback_param),
                    Some(p),
                    "{name} folds with a callable at {p} and the effects pass does not know it \
                     is one"
                );
            }
        }
        // Not vacuous: `array_filter` is the name that tried to get in without a
        // gate, and it is on the list now — with its callback position optional
        // and rowed.
        let filter = param_facts("array_filter").expect("array_filter is mined");
        assert_eq!(filter.callable, &[1]);
        assert!(foldable("array_filter"));
        // …and the names that could NOT be gated this way are still off it.
        assert!(!foldable("usort"), "a required callable cannot be gated away");
        assert!(!foldable("array_udiff"), "a comparator at a variadic mixed tail is invisible here");
    }

    /// The alias rows: a second spelling of a name already on the list, and the
    /// pairing itself is the claim being pinned. If PHP ever stopped aliasing
    /// one of these the row would still be *sound* — it was probed on its own
    /// spelling — but the reason written beside it would be wrong, so the pair
    /// is asserted rather than assumed.
    #[test]
    fn the_alias_rows_sit_beside_the_names_they_alias() {
        for (alias, target) in
            [("join", "implode"), ("chop", "rtrim"), ("sizeof", "count"), ("doubleval", "floatval")]
        {
            assert!(portable(alias), "{alias} folds wherever {target} does");
            assert!(portable(target), "{target} is the row {alias} was probed against");
            assert_eq!(
                portability_class(alias),
                portability_class(target),
                "{alias} and {target} are one function; their classes cannot differ"
            );
            assert_eq!(effect_labels(alias), effect_labels(target), "{alias} is {target}");
        }
        // The aliases whose TARGET is not admitted: they enter together or not
        // at all, and until then neither is foldable.
        for (alias, target) in
            [("key_exists", "array_key_exists"), ("is_integer", "is_int"), ("is_double", "is_float")]
        {
            assert!(!foldable(alias), "{alias} is not admitted ahead of {target}");
            assert!(!foldable(target), "{target} is not admitted");
        }
    }

    /// The issue #354 slice: three names probed clean and three declines that
    /// are declines, not divergences. The two refusals are pinned beside their
    /// causes in `the_width_sensitive_builtins_are_refused`.
    #[test]
    fn the_deferred_fold_names_landed_where_their_evidence_put_them() {
        for name in ["str_split", "array_fill", "array_unique"] {
            assert!(portable(name), "{name} folds on a 32-bit engine too");
            assert!(foldable(name), "{name} is on the folding allowlist");
            assert_eq!(effect_labels(name), Some(&[][..]), "{name} is catalogued pure");
        }
        for name in ["range", "preg_split"] {
            assert!(foldable(name), "{name} folds on a 64-bit engine");
            assert!(!portable(name), "{name} is refused on a 32-bit engine");
            assert_eq!(effect_labels(name), Some(&[][..]), "{name} is catalogued pure");
        }
    }

    /// Issue #78 admissions: on the allowlist AND carry the empty effect set.
    #[test]
    fn the_issue_78_admissions_are_foldable_and_pure() {
        for name in [
            "ucwords",
            "strtr",
            "preg_quote",
            "addslashes",
            "urlencode",
            "urldecode",
            "rawurlencode",
            "rawurldecode",
            "base64_encode",
            "base64_decode",
            "str_increment",
            "str_decrement",
            "str_pad",
            "substr_replace",
            "str_starts_with",
            "str_contains",
            "str_ends_with",
            "gettype",
        ] {
            assert!(portable(name), "{name} is an admitted portable fold");
            assert!(foldable(name), "{name} is on the folding allowlist");
            assert_eq!(effect_labels(name), Some(&[][..]), "{name} is catalogued pure");
        }
        for name in ["dechex", "decbin", "decoct", "bindec", "hexdec", "version_compare"] {
            assert!(foldable(name), "{name} folds on a 64-bit engine");
            assert!(!portable(name), "{name} is refused on a 32-bit engine");
            assert_eq!(effect_labels(name), Some(&[][..]), "{name} is catalogued pure");
        }
    }

    /// The name accessors equal the predicate extensions (issue #64).
    #[test]
    fn the_name_accessors_agree_with_the_predicates() {
        use super::{refused_names, portable_names, unverified_names};
        assert_eq!(portable_names(), PORTABLE);
        assert_eq!(refused_names(), REFUSED);
        assert_eq!(unverified_names(), UNVERIFIED);
        for name in portable_names() {
            assert!(portable(name), "{name} is listed safe but the predicate declines it");
        }
        for name in refused_names() {
            assert!(!portable(name), "{name} is listed refused but the predicate admits it");
            assert_eq!(portability_class(name), Some(PortabilityClass::Refused), "{name} classifies elsewhere");
            assert!(foldable(name), "a refused name is still on the folding allowlist");
        }
        for name in unverified_names() {
            assert!(!portable(name), "{name} is listed unverified but the predicate admits it");
            assert_eq!(
                portability_class(name),
                Some(PortabilityClass::Unverified),
                "{name} is unverified, which is not refused"
            );
            assert!(foldable(name), "an unverified name is still on the folding allowlist");
        }
        assert_eq!(
            refused_names().len() + unverified_names().len(),
            foldable_entry_count() - portable_names().len(),
            "refused ∪ unverified is exactly what a 32-bit engine does not fold"
        );
        assert_eq!(portable_names().len(), 53);
        assert_eq!(refused_names().len(), 15);
        assert_eq!(unverified_names().len(), 0);
    }

    /// Default-deny: a name without a portability classification is not portable.
    #[test]
    fn an_unclassified_name_is_not_portable() {
        for name in
            ["some_unknown_fn", "ip2long", "crc32", "strtotime", "str_word_count", "strcmp"]
        {
            assert!(!portable(name), "{name} must not be certified portable");
            assert!(!foldable(name), "{name} is not on the allowlist at all");
        }
    }

    #[test]
    fn known_pure_builtins_are_foldable() {
        for name in ["strtolower", "strlen", "trim", "abs", "intdiv", "strval", "count"] {
            assert!(foldable(name), "{name} should be foldable");
        }
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert!(foldable("STRTOLOWER"));
        assert!(foldable("StrToLower"));
        assert!(foldable("StrLen"));
    }

    /// Refusals that are **not** width rows (issue #78); see the module docs
    /// for each name's evidence.
    #[test]
    fn impure_and_locale_sensitive_are_excluded() {
        for name in [
            "mb_strtolower", "mb_strlen", "mb_substr", "time", "rand", "setlocale",
            "file_get_contents", "printf", "date", "strtotime", "idate", "strcmp",
            "strcasecmp", "number_format", "bin2hex",
        ] {
            assert!(!foldable(name), "{name} must not be foldable");
            assert!(!portable(name), "{name} must not be certified portable");
        }
    }
}
