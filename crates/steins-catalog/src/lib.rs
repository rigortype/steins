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

/// The effect labels (ADR-0018 hierarchical dot-paths) a builtin carries, or
/// `None` when the function is **uncatalogued** (unknown effects — the safe,
/// silent side of proven-only checking).
///
/// The three-valued return is the heart of ADR-0005 envelope checking:
///
/// * `Some(&[])` — **catalogued and pure**: no effect colors. Every
///   [`foldable`] builtin is pure by construction, so the pure allowlist is
///   reused verbatim as the empty-effect set. A `Pure`-declared function may
///   call these freely.
/// * `Some(&[label, …])` — **catalogued with effects**: calling it from a
///   `Pure` envelope is a proven `effect.envelope-exceeded` violation.
/// * `None` — **uncatalogued**: the effect is unknown. Proven-only checking
///   stays silent here (the design's "cannot-verify" maybe-diagnostic, ADR-0005,
///   is deliberately deferred to a later slice).
///
/// Matching is case-insensitive (PHP function names are).
///
/// # Provisional hand list (ADR-0021)
///
/// This coloring is a small, hand-curated seed drawn from the same
/// frequency-driven sourcing as [`foldable`]; it is **not** the eventual
/// generated catalog (ADR-0014/0021). Labels follow ADR-0018's taxonomy; where a
/// function's effect is argument-dependent the entry takes the *no-arg-analysis
/// upper bound* (the safe, coarser reading):
///
/// * `fopen` stays at the parent `io.fs` label — its read/write split is
///   mode-string-dependent, which this slice does not inspect.
/// * `print_r`/`var_export`/`var_dump` are colored `output` even though the
///   first two are pure when their second argument is `true` (return-mode); the
///   upper bound is the arg-blind safe choice.
/// * `sleep`/`usleep` are `io`: an observable timing side effect on the running
///   process, closest to the io root among the initial colors.
///
/// `exit`/`die` are **language constructs**, not functions — they never reach
/// this table; the effects pass detects them structurally (ADR-0019 rule 4).
#[must_use]
pub fn effect_labels(name: &str) -> Option<&'static [&'static str]> {
    const EMPTY: &[&str] = &[];
    const NONDET_RANDOM: &[&str] = &["nondet.random"];
    const NONDET_TIME: &[&str] = &["nondet.time"];
    const IO_FS_READ: &[&str] = &["io.fs.read"];
    const IO_FS_WRITE: &[&str] = &["io.fs.write"];
    const IO_FS: &[&str] = &["io.fs"];
    const OUTPUT: &[&str] = &["output"];
    const IO: &[&str] = &["io"];
    const GLOBAL_WRITE: &[&str] = &["global.write"];
    const GLOBAL_READ: &[&str] = &["global.read"];

    // A per-call lowercase copy keeps the arms readable; PHP names are ASCII.
    let colored: Option<&'static [&'static str]> = match name.to_ascii_lowercase().as_str() {
        "rand" | "mt_rand" | "random_int" | "random_bytes" | "uniqid" | "shuffle" => {
            Some(NONDET_RANDOM)
        }
        "time" | "microtime" | "hrtime" | "date" | "mktime" => Some(NONDET_TIME),
        "file_get_contents" | "scandir" | "file_exists" | "is_file" | "is_dir" | "fread" => {
            Some(IO_FS_READ)
        }
        "file_put_contents" | "fwrite" | "unlink" | "mkdir" | "rmdir" | "touch" | "copy"
        | "rename" => Some(IO_FS_WRITE),
        "fopen" => Some(IO_FS),
        "print_r" | "var_dump" | "var_export" | "printf" | "vprintf" => Some(OUTPUT),
        "error_log" | "syslog" | "sleep" | "usleep" => Some(IO),
        "date_default_timezone_set" | "mb_regex_encoding" | "setlocale" | "ini_set" | "putenv" => {
            Some(GLOBAL_WRITE)
        }
        "getenv" | "ini_get" | "date_default_timezone_get" => Some(GLOBAL_READ),
        _ => None,
    };

    // A colored entry wins; otherwise a pure/foldable builtin is catalogued with
    // the empty effect set, and everything else stays uncatalogued (`None`).
    colored.or_else(|| foldable(name).then_some(EMPTY))
}

#[cfg(test)]
mod tests {
    use super::{effect_labels, foldable};

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

    #[test]
    fn colored_builtins_carry_their_label() {
        assert_eq!(effect_labels("rand"), Some(&["nondet.random"][..]));
        assert_eq!(effect_labels("time"), Some(&["nondet.time"][..]));
        assert_eq!(effect_labels("file_get_contents"), Some(&["io.fs.read"][..]));
        assert_eq!(effect_labels("file_put_contents"), Some(&["io.fs.write"][..]));
        assert_eq!(effect_labels("fopen"), Some(&["io.fs"][..]));
        assert_eq!(effect_labels("printf"), Some(&["output"][..]));
        assert_eq!(effect_labels("error_log"), Some(&["io"][..]));
        assert_eq!(effect_labels("setlocale"), Some(&["global.write"][..]));
        assert_eq!(effect_labels("getenv"), Some(&["global.read"][..]));
    }

    #[test]
    fn foldable_builtins_are_catalogued_pure() {
        // Every foldable builtin is catalogued with the empty effect set.
        for name in ["strtolower", "strlen", "abs", "trim", "count"] {
            assert_eq!(effect_labels(name), Some(&[][..]), "{name} should be pure");
            assert!(foldable(name));
        }
    }

    #[test]
    fn uncatalogued_builtins_are_none() {
        for name in ["some_unknown_fn", "curl_exec", "mysqli_query"] {
            assert_eq!(effect_labels(name), None, "{name} must be uncatalogued");
        }
    }

    #[test]
    fn effect_labels_are_case_insensitive() {
        assert_eq!(effect_labels("RAND"), Some(&["nondet.random"][..]));
        assert_eq!(effect_labels("File_Put_Contents"), Some(&["io.fs.write"][..]));
        assert_eq!(effect_labels("STRTOLOWER"), Some(&[][..]));
    }
}
