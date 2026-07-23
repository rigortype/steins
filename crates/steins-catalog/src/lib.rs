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

/// The hierarchical **label registry** (ADR-0018): the set of known effect
/// labels. A declared envelope label outside this set (and not an ancestor of
/// any entry — see [`is_known_label`]) earns an `effect.unknown-label`
/// diagnostic; typo safety is Steins' own job.
///
/// It is the union of every label the catalog can color a builtin with
/// ([`effect_labels`]) and the core taxonomy roots/parents of ADR-0018. Ecosystem
/// and private labels (`io.redis`, `email.send`) are **not** here — they become
/// known only once the ADR-0012 plugin channel can register them, which this
/// slice does not implement, so they are (correctly) unknown for now.
#[must_use]
pub fn known_labels() -> &'static [&'static str] {
    // Kept sorted for readability; the taxonomy of ADR-0018 plus every label used
    // in `effect_labels` coloring (all of which are already taxonomy nodes).
    &[
        "exit",
        "global.read",
        "global.write",
        "io",
        "io.db",
        "io.fs",
        "io.fs.read",
        "io.fs.write",
        "io.net",
        "io.net.http",
        "io.process",
        "mutate",
        "nondet",
        "nondet.random",
        "nondet.time",
        "output",
    ]
}

/// Whether `envelope_label` **subsumes** `effect_label` under ADR-0018 prefix
/// subsumption: true iff they are equal, or `effect_label` extends
/// `envelope_label` by a dot-path segment (a declared `io` admits an inferred
/// `io.net.http`). Segment-aware, so `io` does **not** subsume `iota`.
#[must_use]
pub fn subsumes(envelope_label: &str, effect_label: &str) -> bool {
    effect_label == envelope_label
        || effect_label
            .strip_prefix(envelope_label)
            .is_some_and(|rest| rest.starts_with('.'))
}

/// Whether a declared envelope `label` is **known** to the registry: it is a
/// registry entry, or an ancestor of one (an internal taxonomy path). Since the
/// registry already lists every internal node, the ancestor clause matters only
/// for labels finer than the shipped taxonomy — `io.netw` is neither a node nor
/// an ancestor of one, so it stays unknown (→ `effect.unknown-label`), while
/// every registry root is accepted.
#[must_use]
pub fn is_known_label(label: &str) -> bool {
    known_labels().iter().any(|&k| k == label || subsumes(label, k))
}

/// The registry label nearest to an unknown `label`, for a typo suggestion
/// (`io.netw` → `io.net`). Returns `None` when nothing is close. The metric is a
/// simple Levenshtein distance capped so only genuinely near names suggest.
#[must_use]
pub fn nearest_label(label: &str) -> Option<&'static str> {
    known_labels()
        .iter()
        .map(|&k| (levenshtein(label, k), k))
        .filter(|&(d, _)| d <= 2)
        .min_by_key(|&(d, _)| d)
        .map(|(_, k)| k)
}

/// The **builtin SPL/engine exception hierarchy** (ADR-0040): the parent of a
/// standard PHP `Throwable` class not defined in any project, keyed by its global
/// simple name (no namespace, case-insensitive). Project classes chain into this
/// table through their `extends` once their own chain leaves the project index.
///
/// The tree is the standard SPL/engine one: `Throwable` is the root interface;
/// `Exception` and `Error` implement it; the SPL logic/runtime families and the
/// engine `Error` family descend as PHP defines them. A name absent here (and not
/// a project class) has an **unknown** parent — the caller keeps the chain result
/// at `Maybe`, never `No` (the FP-safe side per ADR-0040).
///
/// Names are returned without a leading backslash; matching is case-insensitive.
/// A name carrying a namespace separator is never a builtin (returns `None`).
#[must_use]
pub fn builtin_exception_parent(name: &str) -> Option<&'static str> {
    let bare = name.trim_start_matches('\\');
    if bare.contains('\\') {
        return None; // namespaced — not a global engine/SPL class
    }
    Some(match bare.to_ascii_lowercase().as_str() {
        // Root interface.
        "throwable" => return None,
        // The two roots implement Throwable.
        "exception" | "error" => "Throwable",
        // ── Exception family ──────────────────────────────────────────────
        "errorexception" => "Exception",
        "jsonexception" => "Exception",
        "runtimeexception" => "Exception",
        "logicexception" => "Exception",
        // RuntimeException descendants.
        "outofboundsexception" | "overflowexception" | "rangeexception"
        | "underflowexception" | "unexpectedvalueexception" => "RuntimeException",
        // LogicException descendants.
        "badfunctioncallexception" | "domainexception" | "invalidargumentexception"
        | "lengthexception" | "outofrangeexception" => "LogicException",
        "badmethodcallexception" => "BadFunctionCallException",
        // ── Error family ──────────────────────────────────────────────────
        "typeerror" | "valueerror" | "arithmeticerror" | "unhandledmatcherror"
        | "assertionerror" | "compileerror" | "fibererror" => "Error",
        "divisionbyzeroerror" => "ArithmeticError",
        "parseerror" => "CompileError",
        _ => return None,
    })
}

/// The **direct supertypes** of a builtin class / interface, for the trinary is-a
/// oracle (ADR-0043): `Some(list)` when `name` is a class Steins knows in full —
/// a possibly-empty list of its immediate parents/interfaces (a root returns an
/// empty list) — and `None` when the name is an *unknown* external, which keeps
/// the oracle's enumeration incomplete (→ `Unknown`, never `No`; the FP-safe
/// side). This is the catalog side of the "completely enumerated hierarchy"
/// closure: only names present here (or resolvable in-project) let a `No` verdict
/// stand.
///
/// The tree unifies the SPL/engine `Throwable` hierarchy ([`builtin_exception_parent`])
/// with the enum interface roots (`UnitEnum`; `BackedEnum` extends `UnitEnum`).
/// Matching is case-insensitive; a namespaced name is never a builtin.
#[must_use]
pub fn builtin_class_supers(name: &str) -> Option<Vec<&'static str>> {
    let bare = name.trim_start_matches('\\');
    if bare.contains('\\') {
        return None; // namespaced — not a global engine/SPL class
    }
    match bare.to_ascii_lowercase().as_str() {
        // `Throwable extends Stringable` since PHP 8.0 (the interface gained a
        // `__toString(): string` contract), so *every* Throwable IS-A Stringable
        // — verified against PHP 8.5 (`Reflection`/`is_subclass_of`). Omitting
        // this edge makes `Exception instanceof \Stringable` a spurious `No`
        // (dead live branch — unsound). The whole SPL/engine tree roots here, so
        // this single edge carries Stringable to the entire exception family.
        "throwable" => Some(vec!["Stringable"]),
        // Known root interfaces: fully enumerated, no further supertypes.
        "unitenum" | "stringable" => Some(Vec::new()),
        // A backed enum's implicit interface extends the unit-enum interface.
        "backedenum" => Some(vec!["UnitEnum"]),
        // The SPL/engine exception tree: a single catalogued parent edge.
        other => match builtin_exception_parent(other) {
            Some(parent) => Some(vec![parent]),
            None => None, // unknown external — chain incomplete
        },
    }
}

/// The **measured/curated** throw facts of a builtin call (ADR-0040 source #2):
/// the global class names a builtin provably raises. Deliberately tiny and
/// hand-verified — an uncatalogued builtin simply contributes no throw fact
/// (widen, never a false positive). Empty slice = catalogued-but-throwless.
#[must_use]
pub fn builtin_throws(name: &str) -> Option<&'static [&'static str]> {
    const DIV0: &[&str] = &["DivisionByZeroError"];
    const JSON: &[&str] = &["JsonException"];
    match name.to_ascii_lowercase().as_str() {
        "intdiv" => Some(DIV0),
        // `json_decode`/`json_encode` throw JsonException only under
        // JSON_THROW_ON_ERROR; without flag inspection this stays uncatalogued
        // (widen) rather than manufacture a throw — listed for when flag
        // inspection lands.
        "json_decode_throwing" | "json_encode_throwing" => Some(JSON),
        _ => None,
    }
}

/// When a higher-order builtin invokes its callback (ADR-0033 point 3).
///
/// The distinction never changes *what* effects/throws propagate — both
/// `Immediate` and `Deferred` join the callback's effect and throw sets into the
/// caller's — it only records the honesty of *when*: a `Deferred` invoker
/// (`register_shutdown_function`) claims nothing about timing (ADR-0033), so a
/// value-level fold through it is never attempted, while its effects still count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invocation {
    /// The callback runs during the call (`array_map`, `usort`, …). Effects join,
    /// and a value-level fold may be attempted when trivially composable.
    Immediate,
    /// The callback runs at some unspecified later point (`register_shutdown_function`).
    /// Effects still join the caller's set; no timing or value is claimed.
    Deferred,
}

/// Where a higher-order builtin draws the callback's arguments from (ADR-0033).
/// Consumed only by the value-level fold path (deferred this milestone); the
/// effects/throws join needs only [`InvocationShape::callback_param`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgSource {
    /// The callback receives the *elements* of the array at this positional index
    /// (`array_map`'s cb over param 1's elements, `array_filter`'s over param 0).
    ElementsOf(usize),
    /// The argument source is not modeled (variadic following args, an array of
    /// call args, by-ref accumulation, …). Effects still join; no fold.
    None,
}

/// How a higher-order builtin *calls* its callback (ADR-0033 point 3): the
/// positional index of the callback parameter, whether the invocation is
/// immediate or deferred, and where the callback's arguments come from. This is
/// the invocation-shape metadata that lets the effects/throws passes treat
/// `array_map($cb, $xs)` as *callback-effects ∪ own-effects* instead of an opaque
/// taint — the redemption of ADR-0005's array_map claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvocationShape {
    /// The positional index (0-based) of the callback argument.
    pub callback_param: usize,
    /// Immediate vs. deferred invocation.
    pub invocation: Invocation,
    /// Where the callback's arguments are drawn from (fold path only).
    pub arg_source: ArgSource,
}

/// The [`InvocationShape`] of a higher-order builtin, or `None` when the function
/// is not a known higher-order invoker (its callback argument, if any, stays an
/// opaque taint — the FP-safe side).
///
/// Matching is case-insensitive (PHP function names are). The starter set follows
/// ADR-0033's list. Notes on the argument-order quirks that make this a table and
/// not a rule:
///
/// * `array_map($cb, $arr)` — callback first, elements of param 1. (The
///   multi-array form `array_map($cb, $a, $b)` still has cb at 0; the element
///   source degrades to `None` — effects still join, fold does not apply.)
/// * `array_filter($arr, $cb)` — **reversed**: array first, callback at 1, over
///   the elements of param 0. The 1-argument form `array_filter($arr)` has no
///   callback, so a call with fewer than 2 args simply carries no callback to join.
/// * `array_walk($arr, $cb)` — callback at 1 over param 0's elements, but the
///   callback's first parameter is **by-ref** (it mutates in place): the binding
///   descent skips (a by-ref param cannot be soundly value-bound), yet the
///   callback's effects/throws still join. Modeled as `ElementsOf(0)`; the by-ref
///   handling lives in the consumer.
/// * `usort`/`uasort`/`uksort`/`array_reduce` — callback at 1, immediate; the
///   callback args are not element-shaped (a comparator gets two elements, reduce
///   gets carry+item), so `arg_source` is `None` (effects join, no fold).
/// * `call_user_func($cb, …)` / `call_user_func_array($cb, $args)` — callback at
///   0, immediate; args follow / are an array → `None`.
/// * `register_shutdown_function($cb, …)` — callback at 0, **deferred**.
/// * `preg_replace_callback($pat, $cb, $subj)` — callback at 1, immediate; the
///   callback receives match arrays, not elements of an argument → `None`.
#[must_use]
pub fn invocation_shape(name: &str) -> Option<InvocationShape> {
    use ArgSource::{ElementsOf, None as NoSrc};
    use Invocation::{Deferred, Immediate};
    let shape = |callback_param, invocation, arg_source| {
        Some(InvocationShape { callback_param, invocation, arg_source })
    };
    match name.to_ascii_lowercase().as_str() {
        "array_map" => shape(0, Immediate, ElementsOf(1)),
        "array_filter" => shape(1, Immediate, ElementsOf(0)),
        "array_walk" => shape(1, Immediate, ElementsOf(0)),
        "usort" | "uasort" | "uksort" => shape(1, Immediate, NoSrc),
        "array_reduce" => shape(1, Immediate, NoSrc),
        "call_user_func" | "call_user_func_array" => shape(0, Immediate, NoSrc),
        "register_shutdown_function" => shape(0, Deferred, NoSrc),
        "preg_replace_callback" => shape(1, Immediate, NoSrc),
        _ => None,
    }
}

/// Plain Levenshtein edit distance (small strings, so the quadratic DP is fine).
fn levenshtein(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
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
    fn builtin_exception_tree_shape() {
        use super::builtin_exception_parent as p;
        assert_eq!(p("Throwable"), None);
        assert_eq!(p("Exception"), Some("Throwable"));
        assert_eq!(p("Error"), Some("Throwable"));
        assert_eq!(p("RuntimeException"), Some("Exception"));
        assert_eq!(p("LogicException"), Some("Exception"));
        assert_eq!(p("JsonException"), Some("Exception"));
        assert_eq!(p("ErrorException"), Some("Exception"));
        assert_eq!(p("InvalidArgumentException"), Some("LogicException"));
        assert_eq!(p("OutOfRangeException"), Some("LogicException"));
        assert_eq!(p("OutOfBoundsException"), Some("RuntimeException"));
        assert_eq!(p("TypeError"), Some("Error"));
        assert_eq!(p("DivisionByZeroError"), Some("ArithmeticError"));
        assert_eq!(p("ArithmeticError"), Some("Error"));
        assert_eq!(p("UnhandledMatchError"), Some("Error"));
        // Leading backslash tolerated; case-insensitive.
        assert_eq!(p("\\runtimeexception"), Some("Exception"));
        // Namespaced names are never the builtin.
        assert_eq!(p("App\\Exception"), None);
        // Unknown class → unknown parent.
        assert_eq!(p("MyCustomThing"), None);
    }

    #[test]
    fn builtin_throws_curated() {
        assert_eq!(super::builtin_throws("intdiv"), Some(&["DivisionByZeroError"][..]));
        assert_eq!(super::builtin_throws("strlen"), None);
    }

    #[test]
    fn builtin_class_supers_tree() {
        use super::builtin_class_supers as s;
        // `Throwable extends Stringable` since PHP 8.0 (verified vs PHP 8.5).
        assert_eq!(s("Throwable"), Some(vec!["Stringable"]));
        // Known roots: fully enumerated, no supertypes.
        assert_eq!(s("UnitEnum"), Some(vec![]));
        assert_eq!(s("Stringable"), Some(vec![]));
        // A backed enum's interface extends the unit-enum interface.
        assert_eq!(s("BackedEnum"), Some(vec!["UnitEnum"]));
        // The SPL/engine exception tree (a single catalogued parent edge).
        assert_eq!(s("Exception"), Some(vec!["Throwable"]));
        assert_eq!(s("RuntimeException"), Some(vec!["Exception"]));
        assert_eq!(s("TypeError"), Some(vec!["Error"]));
        // Case-insensitive, leading backslash tolerated.
        assert_eq!(s("\\backedenum"), Some(vec!["UnitEnum"]));
        // Unknown external / namespaced → None (chain incomplete → oracle Unknown).
        assert_eq!(s("MyCustomThing"), None);
        assert_eq!(s("App\\Suit"), None);
    }

    #[test]
    fn effect_labels_are_case_insensitive() {
        assert_eq!(effect_labels("RAND"), Some(&["nondet.random"][..]));
        assert_eq!(effect_labels("File_Put_Contents"), Some(&["io.fs.write"][..]));
        assert_eq!(effect_labels("STRTOLOWER"), Some(&[][..]));
    }

    use super::{is_known_label, nearest_label, subsumes};

    #[test]
    fn subsumption_is_prefix_and_segment_aware() {
        assert!(subsumes("io", "io"), "equal labels subsume");
        assert!(subsumes("io", "io.fs.write"), "coarse admits fine");
        assert!(subsumes("nondet", "nondet.random"));
        assert!(subsumes("io.fs.read", "io.fs.read"));
        // Not subsumption: sibling, ancestor-of-envelope, and non-segment prefix.
        assert!(!subsumes("io.fs.read", "io.fs.write"), "siblings do not subsume");
        assert!(!subsumes("io.net", "io"), "fine does not admit coarse");
        assert!(!subsumes("io", "iota"), "non-segment prefix is not subsumption");
        assert!(!subsumes("io.net", "io.netw"), "io.net does not subsume io.netw");
    }

    #[test]
    fn registry_roots_are_known() {
        for label in [
            "output", "io", "io.fs", "io.fs.read", "io.fs.write", "io.net", "io.net.http",
            "io.db", "io.process", "global.read", "global.write", "nondet", "nondet.random",
            "nondet.time", "exit", "mutate",
        ] {
            assert!(is_known_label(label), "{label} should be a known registry label");
        }
    }

    #[test]
    fn typos_and_private_labels_are_unknown() {
        assert!(!is_known_label("io.netw"), "typo is unknown");
        assert!(!is_known_label("email.send"), "private/plugin label is unknown for now");
        assert!(!is_known_label("nondet.rand"), "close typo still unknown");
    }

    #[test]
    fn nearest_label_suggests_the_obvious_typo() {
        assert_eq!(nearest_label("io.netw"), Some("io.net"));
        assert_eq!(nearest_label("outputt"), Some("output"));
        // Something wildly off has no near suggestion.
        assert_eq!(nearest_label("completely-different"), None);
    }

    use super::{invocation_shape, ArgSource, Invocation};

    #[test]
    fn invocation_shapes_of_the_starter_set() {
        let s = |n| invocation_shape(n).expect("known invoker");
        // array_map: cb first, elements of the array at 1.
        assert_eq!(s("array_map").callback_param, 0);
        assert_eq!(s("array_map").invocation, Invocation::Immediate);
        assert_eq!(s("array_map").arg_source, ArgSource::ElementsOf(1));
        // array_filter: REVERSED — array first, cb at 1, over param 0's elements.
        assert_eq!(s("array_filter").callback_param, 1);
        assert_eq!(s("array_filter").arg_source, ArgSource::ElementsOf(0));
        // array_walk: cb at 1 over param 0 (by-ref handled by the consumer).
        assert_eq!(s("array_walk").callback_param, 1);
        assert_eq!(s("array_walk").arg_source, ArgSource::ElementsOf(0));
        // usort/uasort/uksort/array_reduce: cb at 1, no element source.
        for n in ["usort", "uasort", "uksort", "array_reduce"] {
            assert_eq!(s(n).callback_param, 1, "{n}");
            assert_eq!(s(n).arg_source, ArgSource::None, "{n}");
            assert_eq!(s(n).invocation, Invocation::Immediate, "{n}");
        }
        // call_user_func family: cb at 0, immediate.
        assert_eq!(s("call_user_func").callback_param, 0);
        assert_eq!(s("call_user_func_array").callback_param, 0);
        // register_shutdown_function: cb at 0, DEFERRED.
        assert_eq!(s("register_shutdown_function").callback_param, 0);
        assert_eq!(s("register_shutdown_function").invocation, Invocation::Deferred);
        // preg_replace_callback: cb at 1, immediate.
        assert_eq!(s("preg_replace_callback").callback_param, 1);
    }

    #[test]
    fn invocation_shape_is_case_insensitive_and_none_for_others() {
        assert!(invocation_shape("ARRAY_MAP").is_some());
        assert!(invocation_shape("Array_Filter").is_some());
        // Non-invokers and plain builtins carry no shape.
        for n in ["strtolower", "count", "array_merge", "some_unknown_fn"] {
            assert_eq!(invocation_shape(n), None, "{n}");
        }
    }
}
