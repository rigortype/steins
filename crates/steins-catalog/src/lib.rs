//! Builtin / extension catalog — the curated signatures and effect entries for
//! PHP builtins and extension functions.
//!
//! # Folding gate (this milestone)
//!
//! The full effect catalog (ADR-0014 sourcing, ADR-0021 seeding) is not built
//! yet. What exists here is the **folding gate of ADR-0008 applied as an
//! allowlist**: [`foldable`] names a small set of builtins that are pure and
//! deterministic under ADR-0008's rule — an expression folds only when all
//! effect colors are empty and `nondet` is absent on the concrete path — so a
//! sidecar fold of them yields a value that is portable to the source.
//!
//! This is deliberately a *hand-picked allowlist*, not a computed property:
//! uncolored functions widen (a miss, never a false positive), the only seeding
//! order compatible with the zero-FP bar (ADR-0002). The names are drawn from
//! the top of `docs/notes/20260722-builtin-frequency.md` where safely pure.
//!
//! # Deliberate exclusions
//!
//! Locale- or global-sensitive functions are **not** here, even when frequent:
//! `mb_*` (encoding-dependent), anything affected by `setlocale`, the current
//! timezone, or `mb_regex_encoding`-class settings. Their value is not portable
//! without ADR-0008's opt-in "pseudo-constant settings" config, which this slice
//! does not implement. `nondet` builtins (`time`, `rand`, `microtime`, …) are
//! excluded by definition.

/// Whether `name` is on the folding allowlist (case-insensitive).
///
/// A `true` here is a *permission to fold*, not a promise the call folds: the
/// inference engine still requires the callee to be a non-user function and all
/// arguments to be literals the IR carries before it asks the sidecar.
///
/// Several allowlisted functions (`sprintf`, `str_replace`, `in_array`, `count`,
/// `implode`) commonly take **array** arguments. The trace IR has no array
/// literal yet (ADR-0027), so those calls simply will not qualify — every arg
/// must be an `int`/`float`/`string`/`bool`/`null` literal. They stay on the
/// list so they light up automatically once array literals arrive.
#[must_use]
pub fn foldable(name: &str) -> bool {
    // Sorted for readability; matched case-insensitively (PHP function names are
    // case-insensitive).
    const ALLOWLIST: &[&str] = &[
        // String transforms — pure, locale-independent (ASCII-cased builtins;
        // the `mb_*` and locale-sensitive variants are deliberately excluded).
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
        "sprintf",
        "strlen",
        // Numeric / conversion — pure and deterministic.
        "abs",
        "intdiv",
        "intval",
        "floatval",
        "strval",
        "boolval",
        // Array/collection predicates — pure (qualify only once array literals
        // exist in the IR).
        "in_array",
        "count",
    ];

    ALLOWLIST.iter().any(|&f| name.eq_ignore_ascii_case(f))
}

#[cfg(test)]
mod tests {
    use super::foldable;

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

    #[test]
    fn impure_and_locale_sensitive_are_excluded() {
        for name in [
            "mb_strtolower", // encoding-dependent
            "time",          // nondet
            "rand",          // nondet
            "setlocale",     // global-write
            "file_get_contents", // io
            "printf",        // output
            "date",          // global-read (timezone) + nondet
        ] {
            assert!(!foldable(name), "{name} must not be foldable");
        }
    }
}
