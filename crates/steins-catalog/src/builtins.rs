//! The per-builtin fact tables that are neither effects nor the fold
//! allowlist: the class hierarchy and display casing mined from php-src
//! ([`builtin_class_supers`], [`builtin_class_display`], ADR-0043) with the
//! frozen `Throwable` projection ([`builtin_exception_parent`], ADR-0040), the
//! curated throw facts ([`builtin_throws`]) and failure-arm causes
//! ([`failure_arms`], ADR-0042), the callback invocation shapes
//! ([`invocation_shape`], ADR-0033), the return ladder's catalog rungs
//! ([`return_fact`], [`resource_return`], [`declared_return`], ADR-0056 and
//! ADR-0069), and the engine's own per-parameter facts ([`param_facts`],
//! issue #382).
//!
//! The generated tables (`*_generated.rs`) are declared in `lib.rs` and read
//! only from here.

use crate::{
    declared_returns_generated, display_names_generated, hierarchy_generated,
    param_facts_generated, resource_returns_generated, return_facts_generated,
};

/// The **builtin SPL/engine exception hierarchy** (ADR-0040): the parent of a
/// standard PHP `Throwable` class not defined in any project, keyed by its
/// global simple name (no namespace, case-insensitive). Project classes chain
/// in through their `extends`.
///
/// `Throwable` is the root interface; `Exception`/`Error` implement it; SPL and
/// engine families descend as PHP defines them. An absent name (and not a
/// project class) has an **unknown** parent — the caller keeps `Maybe`, never
/// `No` (FP-safe). Leading backslash stripped; a namespaced name is never a
/// builtin.
///
/// **Frozen throw-system projection**, deliberately *not* widened to the full
/// mined hierarchy ([`builtin_class_supers`]) per ADR-0043 §5. The test
/// `exception_parent_agrees_with_generated_hierarchy` proves the two never
/// conflict.
#[must_use]
pub fn builtin_exception_parent(name: &str) -> Option<&'static str> {
    let bare = name.trim_start_matches('\\');
    if bare.contains('\\') {
        return None; // namespaced — not a global engine/SPL class
    }
    Some(match bare.to_ascii_lowercase().as_str() {
        "throwable" => return None,
        "exception" | "error" => "Throwable",
        "errorexception" => "Exception",
        "jsonexception" => "Exception",
        "runtimeexception" => "Exception",
        "logicexception" => "Exception",
        "outofboundsexception" | "overflowexception" | "rangeexception"
        | "underflowexception" | "unexpectedvalueexception" => "RuntimeException",
        "badfunctioncallexception" | "domainexception" | "invalidargumentexception"
        | "lengthexception" | "outofrangeexception" => "LogicException",
        "badmethodcallexception" => "BadFunctionCallException",
        "typeerror" | "valueerror" | "arithmeticerror" | "unhandledmatcherror"
        | "assertionerror" | "compileerror" | "fibererror" => "Error",
        "divisionbyzeroerror" => "ArithmeticError",
        "parseerror" => "CompileError",
        _ => return None,
    })
}

/// The **direct supertypes** of a builtin class / interface, for the trinary
/// is-a oracle (ADR-0043): `Some(list)` of immediate parents/interfaces (a
/// root returns empty), `None` for an *unknown* external (→ `Unknown`, never
/// `No`; FP-safe).
///
/// The **single source of truth** for the builtin hierarchy: 352 production
/// classes + interfaces mined from php-src (pin `6bc7c26cf6…`, cross-checked
/// vs PHP 8.5.8), generated into `hierarchy_generated::HIERARCHY`. Subsumes the
/// SPL/engine `Throwable` tree (also projected by [`builtin_exception_parent`])
/// and the enum interface roots.
///
/// Matching is case-insensitive; namespaced builtins (`Random\…`, `FFI\…`)
/// **are** resolved. **Builtin enums are deliberately absent** (→ `Unknown`):
/// the mining data omits an enum's implicit `UnitEnum`/`BackedEnum`
/// interfaces, so a `No` verdict would be unsound (ADR-0043 §3).
#[must_use]
pub fn builtin_class_supers(name: &str) -> Option<Vec<&'static str>> {
    let key = name.trim_start_matches('\\').to_ascii_lowercase();
    hierarchy_generated::HIERARCHY
        .binary_search_by(|(n, _)| (*n).cmp(key.as_str()))
        .ok()
        .map(|i| hierarchy_generated::HIERARCHY[i].1.to_vec())
}

/// The number of rows in the generated hierarchy table (ADR-0054 §9.6
/// freshness context). A named accessor keeps the generated module private.
#[must_use]
pub fn hierarchy_entry_count() -> usize {
    hierarchy_generated::HIERARCHY.len()
}

/// The casing php-src **declares** a builtin class/interface/enum with (`gmp` →
/// `GMP`), or `None` when the mining data doesn't declare it — mined from the
/// same `hierarchy.toml` pin as [`builtin_class_supers`].
///
/// **Display fidelity only.** `ContractTy::Class` case-folds on the way in, so
/// a class name reaching a rendering surface has lost its source casing; this
/// closes that gap (ADR-0069 third-amendment residual). No judgment may
/// consult it — everything downstream compares case-insensitively.
///
/// Matching is case-insensitive, backslash stripped, namespaced builtins
/// resolved as in [`builtin_class_supers`]. **Enums are present here** even
/// though the hierarchy table skips them, since a display name has no
/// soundness gate to guard.
#[must_use]
pub fn builtin_class_display(name: &str) -> Option<&'static str> {
    let key = name.trim_start_matches('\\').to_ascii_lowercase();
    display_names_generated::DISPLAY_NAMES
        .binary_search_by(|(n, _)| (*n).cmp(key.as_str()))
        .ok()
        .map(|i| display_names_generated::DISPLAY_NAMES[i].1)
}

/// The **measured/curated** throw facts of a builtin call (ADR-0040 source
/// #2): the global class names a builtin provably raises. Deliberately tiny
/// and hand-verified; uncatalogued contributes no throw fact (widen, never a
/// false positive). An empty list means catalogued-but-throwless.
#[must_use]
pub fn builtin_throws(name: &str) -> Option<&'static [&'static str]> {
    // intdiv has TWO input-determined arms (math.c:1502/1507): `divisor == 0`
    // → DivisionByZeroError, `PHP_INT_MIN / -1` overflow → ArithmeticError.
    // Both is-a `Error` → unchecked (ADR-0007).
    const INTDIV: &[&str] = &["DivisionByZeroError", "ArithmeticError"];
    const JSON: &[&str] = &["JsonException"];
    // Input-determined `ValueError` throws mined from php-src C (throws.toml):
    // PHP 8 turned argument-value misuses from `false`-returns into
    // `ValueError`. Method-shaped constructor throws are deferred.
    const VALUE_ERROR: &[&str] = &["ValueError"];
    match name.to_ascii_lowercase().as_str() {
        "intdiv" => Some(INTDIV),
        "preg_match" | "file_get_contents" | "fread" | "fgets" | "file" | "scandir"
        | "stream_get_contents" | "stream_socket_client" | "unserialize" | "json_decode"
        | "iconv" | "mb_convert_encoding" | "hash" | "hash_hmac" | "hash_init" | "hash_file"
        | "random_int" | "random_bytes" | "proc_open" | "shmop_open" | "socket_create" => {
            Some(VALUE_ERROR)
        }
        // `json_decode`/`json_encode` throw JsonException only under
        // JSON_THROW_ON_ERROR; without flag inspection this key stays synthetic.
        "json_decode_throwing" | "json_encode_throwing" => Some(JSON),
        _ => None,
    }
}

/// The **cause** of a builtin's `false`/`null` failure arm (ADR-0042): a fact
/// the catalog can state, never a probability. Maps to a `failure.*`
/// value-provenance label ([`known_labels`]) for boundary-profile must-check
/// policy (default exempts [`Resource`], includes [`Environment`]; strict
/// includes both), replacing ADR-0030's erased benevolent union.
///
/// [`Resource`]: FailureCause::Resource
/// [`Environment`]: FailureCause::Environment
///
/// [`known_labels`]: crate::known_labels
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureCause {
    /// Allocation/handle exhaustion (`curl_init`, `imagecreate*`): statically
    /// irrefutable, unrecoverable in practice. Default profile exempts it.
    Resource,
    /// Filesystem/network/external-state failure (`fopen`, `fsockopen`): a
    /// normal outcome; both profiles require the check.
    Environment,
    /// Argument-value-determined failure (`preg_match` malformed pattern):
    /// statically refutable with proven args, the fallback for unproven ones.
    Input,
}

impl FailureCause {
    /// The `failure.*` registry dot-path this cause attaches to the arm's value.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            FailureCause::Resource => "failure.resource",
            FailureCause::Environment => "failure.environment",
            FailureCause::Input => "failure.input",
        }
    }
}

/// The failure-arm classification of a builtin (ADR-0042), mined from php-src
/// C (`docs/research/phpsrc-mining/failure_arms.toml`):
///
/// * `Some(Causes(&[…]))` — the `false`/`null` arm is a real failure, carrying
///   the [`FailureCause`]s traced (`curl_init` is `[Resource, Input]`).
/// * `Some(Sentinel)` — the `false`/`null` return is a **legitimate
///   non-failure result** (`strpos` "not present"): must NOT be labeled.
/// * `None` — **unclassified**: the catalog states nothing.
///
/// Behavior-neutral until consumed by ADR-0037 boundary profiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureArms {
    /// The distinct failure causes the arm(s) were traced to, in recorded order.
    Causes(&'static [FailureCause]),
    /// A legitimate result, never `failure.*`-labeled.
    Sentinel,
}

/// The [`FailureArms`] classification of a builtin `name` (ADR-0042), or `None`
/// when unclassified. Matching is case-insensitive. Method-shaped rows
/// (`DateTime::createFromFormat`) are deferred — the API is function-keyed.
/// See `docs/research/phpsrc-mining/failure_arms.toml` for per-arm C evidence.
#[must_use]
pub fn failure_arms(name: &str) -> Option<FailureArms> {
    use FailureCause::{Environment, Input, Resource};
    const RESOURCE: &[FailureCause] = &[Resource];
    const ENVIRONMENT: &[FailureCause] = &[Environment];
    const INPUT: &[FailureCause] = &[Input];
    const RESOURCE_INPUT: &[FailureCause] = &[Resource, Input];
    const INPUT_ENVIRONMENT: &[FailureCause] = &[Input, Environment];

    let arms = |c| Some(FailureArms::Causes(c));
    match name.to_ascii_lowercase().as_str() {
        "curl_init" => arms(RESOURCE_INPUT),
        "curl_exec" => arms(ENVIRONMENT),
        "curl_setopt" => arms(INPUT),
        "fopen" | "file_get_contents" | "file_put_contents" | "file" | "readfile" | "fread"
        | "fwrite" | "fgets" | "fscanf" | "tmpfile" | "mkdir" | "unlink" | "rename" | "copy"
        | "scandir" => arms(ENVIRONMENT),
        "fsockopen" | "pfsockopen" | "stream_socket_client" | "stream_get_contents" => {
            arms(ENVIRONMENT)
        }
        "preg_match" | "preg_match_all" | "preg_replace" | "preg_split" => arms(INPUT),
        "json_decode" | "json_encode" | "unserialize" | "strtotime" | "date_create" | "iconv"
        | "mb_convert_encoding" => arms(INPUT),
        // hash_file straddles but reads primarily environmental.
        "hash_file" => arms(ENVIRONMENT),
        "getenv" => arms(ENVIRONMENT),
        "proc_open" => arms(INPUT_ENVIRONMENT),
        "sem_get" | "shmop_open" => arms(ENVIRONMENT),
        "socket_create" => arms(RESOURCE),
        // NOT-A-FAILURE SENTINELS: `false`/`null` is legitimate, must stay
        // distinct from unclassified (`None`). The failure_arms.toml
        // `[[sentinel]]` set.
        "array_search" | "strpos" | "array_key_first" | "next" | "current" | "prev" | "end"
        | "reset" => Some(FailureArms::Sentinel),
        _ => None,
    }
}

/// When a higher-order builtin invokes its callback (ADR-0033 point 3). Both
/// arms join the callback's effect/throw sets into the caller's; the
/// distinction is only *when* — a `Deferred` invoker claims nothing about
/// timing, so no value-level fold is attempted through it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invocation {
    /// Runs during the call (`array_map`, `usort`, …); fold may be attempted.
    Immediate,
    /// Runs at some unspecified later point (`register_shutdown_function`); no
    /// timing or value is claimed.
    Deferred,
}

/// Where a higher-order builtin draws the callback's arguments from (ADR-0033),
/// reserved for value-level folding; effects/throws joining uses only
/// [`InvocationShape::callback_param`].
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
/// taint, as required by ADR-0005.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvocationShape {
    /// The positional index (0-based) of the callback argument.
    pub callback_param: usize,
    /// Immediate vs. deferred invocation.
    pub invocation: Invocation,
    /// Where the callback's arguments are drawn from (fold path only).
    pub arg_source: ArgSource,
}

/// The [`InvocationShape`] of a higher-order builtin, or `None` when not a
/// known higher-order invoker (its callback argument, if any, stays an opaque
/// taint — FP-safe). Matching is case-insensitive; rows follow ADR-0033.
///
/// Argument-order quirks make this a table rather than a rule: `array_filter`
/// is **reversed** (array first, callback at 1); `array_walk`'s callback's
/// first parameter is by-ref (modeled as `ElementsOf(0)`, by-ref handling
/// lives in the consumer); comparator-style callbacks (`usort`, `array_reduce`)
/// have non-element-shaped args, so `arg_source` is `None`.
///
/// # Immediately invoked rows (ADR-0063 P1)
///
/// A row asserts the named position is invoked *during* the call, because PHP
/// evaluates the callback before returning. `array_find`/`array_find_key`/
/// `array_any`/`array_all` (PHP 8.4) and `array_walk_recursive` (whose
/// callback sees nested *leaves*, not param 0's elements, so `arg_source` is
/// `None`) and `iterator_apply` are all immediate.
///
/// # Deliberate exclusions
///
/// A builtin taking a callable but **not** given a row contributes no callback
/// effects: `set_error_handler`/`set_exception_handler`/
/// `spl_autoload_register`/`register_tick_function`/
/// `header_register_callback`/`ob_start` store their callable for later
/// invocation, without even a `Deferred` row; `preg_replace_callback_array`'s
/// callables sit *inside* an associative array, not a positional argument; the
/// `array_u*diff`/`array_u*intersect` family's comparator(s) sit in the
/// **last** variadic position, which a fixed `callback_param` index cannot
/// express.
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
        "array_find" | "array_find_key" | "array_any" | "array_all" => {
            shape(1, Immediate, ElementsOf(0))
        }
        "array_walk_recursive" => shape(1, Immediate, NoSrc),
        "iterator_apply" => shape(1, Immediate, NoSrc),
        _ => None,
    }
}

/// The **curated return-fact refinement** of a builtin `name` (ADR-0056 §1.2):
/// a phpdoc type string (`"int<0, max>"`, `"non-empty-string"`) narrowing
/// strictly within the reflected return envelope, or `None` when no row
/// curates it (the common case).
///
/// Only a *refinement proposal*: steins-infer admits it only after confirming
/// it is an extensional subset of the reflected envelope AND the project PHP
/// minor equals [`PINNED_PHP`] (ADR-0056 §2). A stale row loses precision,
/// never manufactures a wrong premise.
///
/// Generated from `return_facts.toml`. The bool-predicate family has no rows
/// since its reflected envelope is already `bool`. Case-insensitive.
///
/// [`PINNED_PHP`]: crate::PINNED_PHP
#[must_use]
pub fn return_fact(name: &str) -> Option<&'static str> {
    let key = name.trim_start_matches('\\').to_ascii_lowercase();
    return_facts_generated::RETURN_FACTS
        .binary_search_by(|(n, _)| (*n).cmp(key.as_str()))
        .ok()
        .map(|i| return_facts_generated::RETURN_FACTS[i].1)
}

/// Whether the builtin `name` returns a legacy PHP **resource**, and whether
/// its return carries a `false` failure arm (ADR-0056 §8). `Some(true)` is
/// `resource|false`, `Some(false)` is a bare `resource`, `None` otherwise.
///
/// `resource` is the one type PHP cannot spell in a declaration, so the
/// reflected envelope anchoring every other return fact is structurally
/// unavailable; this row is condition 1 of §7's gate. steins-infer supplies
/// two more before seeding: **the tripwire** (the analyzing engine must
/// declare NO return type — PHP 8 migrated most resources to objects, and an
/// engine answering `CurlHandle|false` has disowned the row, self-switching
/// it off) and **the minor pin** ([`PINNED_PHP`]).
///
/// [`PINNED_PHP`]: crate::PINNED_PHP
#[must_use]
pub fn resource_return(name: &str) -> Option<bool> {
    let key = name.trim_start_matches('\\').to_ascii_lowercase();
    resource_returns_generated::RESOURCE_RETURNS
        .binary_search_by(|(n, _)| (*n).cmp(key.as_str()))
        .ok()
        .map(|i| resource_returns_generated::RESOURCE_RETURNS[i].1)
}

/// The **declared return type** of a builtin `name` (ADR-0069, issues #73/#79):
/// the canonical phpdoc spelling the builtin declares (`"string"`,
/// `"string|false"`), or `None` when no row covers it — the bottom rung of the
/// return ladder, for runs where every other rung is engine-gated.
///
/// Three load-bearing properties, each enforced elsewhere: **Asserted, never
/// Verified** (seeded at the `Asserted` stratum, so a wrong row can mislead a
/// dump but never mint a finding); **any engine answer wins, per name** (fires
/// only where the sidecar-backed reflected envelope is `None`); **never an
/// existence answer** (the absence family reads the boot surface, not this
/// table).
///
/// Rows are mined from PHPStan's `resources/functionMap.php` at a pinned
/// commit (inherited from Phan; see the root `NOTICE`), countersigned arm-wise
/// against the pinned engine's own reflection. Case-insensitive, leading `\`
/// stripped. Values may use the full scalar-arm vocabulary; every value stays
/// Asserted, never a proof premise.
#[must_use]
pub fn declared_return(name: &str) -> Option<&'static str> {
    let key = name.trim_start_matches('\\').to_ascii_lowercase();
    declared_returns_generated::DECLARED_RETURNS
        .binary_search_by(|(n, _)| (*n).cmp(key.as_str()))
        .ok()
        .map(|i| declared_returns_generated::DECLARED_RETURNS[i].1)
}

pub use param_facts_generated::ParamFacts;

/// One builtin's per-parameter facts as the **engine's arginfo** reports them
/// (issue #382), or `None` when the mining build had no such internal function.
///
/// This is a second, independent witness, and that is its whole point.
/// [`out_params`] and [`invocation_shape`] were transcribed from php-src's stubs
/// by hand and nothing checked them; the check that was attempted could not
/// work, because [`by_value_arg`] falls back to `out_params`, so a name with no
/// row answers "by value" everywhere and a loop keyed on it skips exactly the
/// omission it is hunting. Reading arginfo instead of the stubs a second time is
/// what makes disagreement possible at all.
///
/// **A `None` is not "no parameters".** It means the mining build did not have
/// the name — an extension it was not built with, or a name that does not exist.
/// Use [`param_facts_mined`] to tell those apart: a name that was mined and
/// carries nothing answers `true` there and `None` here only if it is absent.
///
/// [`out_params`]: crate::out_params
/// [`by_value_arg`]: crate::by_value_arg
#[must_use]
pub fn param_facts(name: &str) -> Option<&'static ParamFacts> {
    let key = name.trim_start_matches('\\').to_ascii_lowercase();
    param_facts_generated::PARAM_FACTS
        .binary_search_by(|(n, _)| (*n).cmp(key.as_str()))
        .ok()
        .map(|i| &param_facts_generated::PARAM_FACTS[i].1)
}

/// Whether the mining build had this internal function at all.
///
/// The negative is the useful half: a completeness test that reads an absent
/// name as agreement is the vacuity issue #382 was opened about, so every such
/// test asks this first and fails on `false` rather than passing quietly. Every
/// mined name has a row — an EMPTY row is the recorded fact "carries nothing" —
/// so this is exactly "[`param_facts`] answers".
#[must_use]
pub fn param_facts_mined(name: &str) -> bool {
    param_facts(name).is_some()
}

/// The minor at which a builtin's declared **return type** last moved across
/// the supported 8.x line, or `None` when it never did (ADR-0069 §3, A11-shaped
/// version discipline). A `Some((8, 2))` means the row is only known good at or
/// above 8.2; an undeclared target still admits it since the row is Asserted.
///
/// Deliberately **independent** of [`declared_return`]: a name can be
/// version-sensitive without an admitted row.
#[must_use]
pub fn declared_return_changed_at(name: &str) -> Option<(u16, u16)> {
    let key = name.trim_start_matches('\\').to_ascii_lowercase();
    declared_returns_generated::RETURN_VERSION_SENSITIVE
        .binary_search_by(|(n, _)| (*n).cmp(key.as_str()))
        .ok()
        .map(|i| declared_returns_generated::RETURN_VERSION_SENSITIVE[i].1)
}

#[cfg(test)]
mod tests {
    use crate::{is_known_label, subsumes};

    #[test]
    fn return_facts_r3_r4_rows() {
        assert_eq!(super::return_fact("is_int"), None);
        assert_eq!(super::return_fact("some_unknown_fn"), None);
        for name in ["count", "sizeof", "strlen", "mb_strlen", "substr_count", "func_num_args", "array_push", "array_unshift"] {
            assert_eq!(super::return_fact(name), Some("int<0, max>"), "{name} must curate int<0, max>");
        }
        for name in ["sha1", "md5", "uniqid", "get_debug_type", "spl_object_hash"] {
            assert_eq!(super::return_fact(name), Some("non-falsy-string"), "{name} must curate non-falsy-string");
        }
        for name in
            ["abs", "bin2hex", "trim", "strtoupper", "preg_match_all", "str_word_count", "sha1_file", "dirname"]
        {
            assert_eq!(super::return_fact(name), None, "{name} is a refused row — no curated fact");
        }
        assert_eq!(super::return_fact("COUNT"), Some("int<0, max>"));
        assert_eq!(super::return_fact("\\sha1"), Some("non-falsy-string"));
        let t = super::return_facts_generated::RETURN_FACTS;
        assert!(t.windows(2).all(|w| w[0].0 < w[1].0), "RETURN_FACTS must be strictly sorted by key");
    }

    #[test]
    fn return_facts_dr4_refined_string_rows() {
        // Two `non-falsy-string` rows that passed the three-leg probe gate at PHP
        // 8.5.8, each with a single `string` reflected envelope.
        //
        // `spl_object_hash` — a fixed 32-character lowercase hex digest
        // (5000-object sweep: none falsy). Its `object` parameter makes the
        // bin2hex empty-in/empty-out trap structurally unreachable.
        assert_eq!(super::return_fact("spl_object_hash"), Some("non-falsy-string"));
        // `get_debug_type` — every return is a type keyword (>= 3 chars) or a
        // class/enum name; PHP's label grammar forbids a leading digit, so no
        // class can be named "0".
        assert_eq!(super::return_fact("get_debug_type"), Some("non-falsy-string"));
        // Both honour the shared lookup contract.
        assert_eq!(super::return_fact("SPL_OBJECT_HASH"), Some("non-falsy-string"));
        assert_eq!(super::return_fact("\\get_debug_type"), Some("non-falsy-string"));
    }

    #[test]
    fn return_facts_dirname_stays_refused() {
        // Probes refute `dirname(): non-falsy-string` twice: (a) NOT non-falsy —
        // `dirname("0/x") === "0"`, a FALSY string; (b) NOT non-empty either —
        // `dirname("") === ""`, the bin2hex empty-in/empty-out shape. Neither
        // refinement holds for all arguments, so the reflected `string` envelope
        // stands alone.
        assert_eq!(super::return_fact("dirname"), None);
        assert_eq!(super::return_fact("DIRNAME"), None);
        assert_eq!(super::return_fact("\\dirname"), None);
    }

    #[test]
    fn resource_returns_carry_the_stub_reading_and_nothing_else() {
        assert_eq!(super::resource_return("fopen"), Some(true));
        assert_eq!(super::resource_return("tmpfile"), Some(true));
        assert_eq!(super::resource_return("stream_context_create"), Some(false));
        assert_eq!(super::resource_return("stream_context_get_default"), Some(false));
        assert_eq!(super::resource_return("stream_context_set_default"), Some(false));
        for migrated in ["curl_init", "imagecreate", "finfo_open", "ldap_connect", "odbc_connect"] {
            assert_eq!(
                super::resource_return(migrated),
                None,
                "{migrated} returns an object on PHP 8 — it must not be a resource row",
            );
        }
        assert_eq!(super::resource_return("stream_socket_pair"), None);
        assert_eq!(super::resource_return("get_resources"), None);
        assert_eq!(super::resource_return("FOPEN"), Some(true));
        assert_eq!(super::resource_return("\\fopen"), Some(true));
        let t = super::resource_returns_generated::RESOURCE_RETURNS;
        assert!(t.windows(2).all(|w| w[0].0 < w[1].0), "RESOURCE_RETURNS must be sorted by key");
        assert!(!t.is_empty(), "the table is the whole point; an empty one is a generation bug");
    }

    /// Spellings treated as single-base envelopes when partitioning generated rows.
    const ENVELOPE_SPELLINGS: &[&str] = &[
        "bool",
        "int",
        "float",
        "string",
        "bool|null",
        "int|null",
        "float|null",
        "string|null",
    ];

    #[test]
    fn declared_return_rows_and_their_shape() {
        assert_eq!(super::declared_return("str_repeat"), Some("string"));
        assert_eq!(super::declared_return("str_pad"), Some("string"));
        assert_eq!(super::declared_return("array_key_exists"), Some("bool"));
        assert_eq!(super::declared_return("acos"), Some("float"));
        assert_eq!(super::declared_return("curl_multi_getcontent"), Some("string|null"));
        // Rows may preserve functionMap types richer than a base envelope.
        assert_eq!(super::declared_return("strstr"), Some("string|false"));
        assert_eq!(super::declared_return("strrchr"), Some("string|false"));
        assert_eq!(super::declared_return("file_get_contents"), Some("string|false"));
        assert_eq!(super::declared_return("array_search"), Some("int|string|false"));
        assert_eq!(super::declared_return("preg_match"), Some("0|1|false"));
        assert_eq!(super::declared_return("ctype_alpha"), Some("bool"));
        // Scalar refinement reflection cannot state.
        assert_eq!(super::declared_return("mb_strtoupper"), Some("uppercase-string"));
        // ADR-0071 permits a bare array, list, keyed map, and full shape.
        assert_eq!(super::declared_return("array_merge"), Some("array"));
        assert_eq!(super::declared_return("str_split"), Some("list<string>"));
        assert_eq!(super::declared_return("array_count_values"), Some("array<int<1, max>>"));
        assert_eq!(
            super::declared_return("imagecolorsforindex"),
            Some("array{alpha: int<0, 127>, blue: int<0, 255>, green: int<0, 255>, red: int<0, 255>}")
        );
        assert_eq!(super::declared_return("scandir"), Some("false|list<string>"));
        // Class rows keep functionMap's own casing (`ContractTy::Class` case-folds).
        assert_eq!(super::declared_return("gmp_init"), Some("GMP"));
        assert_eq!(super::declared_return("date_diff"), Some("DateInterval"));
        assert_eq!(super::declared_return("hash_init"), Some("HashContext"));
        assert_eq!(super::declared_return("collator_create"), Some("?Collator"));
        assert_eq!(super::declared_return("simplexml_load_string"), Some("SimpleXMLElement|false"));
        // Namespaced builtin FQN: the consuming resolver must be the identity.
        assert_eq!(super::declared_return("ast\\parse_code"), Some("ast\\Node"));
        assert_eq!(super::declared_return("curl_init"), Some("__benevolent<CurlHandle|false>"));
        assert_eq!(super::declared_return("STRSTR"), Some("string|false"));
        assert_eq!(super::declared_return("\\str_repeat"), Some("string"));
        assert_eq!(super::declared_return("some_unknown_fn"), None);

        let t = super::declared_returns_generated::DECLARED_RETURNS;
        assert!(t.windows(2).all(|w| w[0].0 < w[1].0), "DECLARED_RETURNS must be strictly sorted by key");
        for (name, ty) in t {
            assert!(!ty.is_empty(), "{name} carries an empty spelling");
        }
        let rich = t.iter().filter(|(_, ty)| !ENVELOPE_SPELLINGS.contains(ty)).count();
        assert_eq!(t.len(), 1711, "admitted rows at this pin");
        assert_eq!(t.len() - rich, 919, "the #73 envelope population must be preserved exactly");
        assert_eq!(rich, 792, "the #79, ADR-0071, object-slice and class-string (#236) rich admissions");
    }

    #[test]
    fn declared_return_excludes_what_the_engine_disowns() {
        // ADR-0069 §3 reflection cross-check: functionMap says `string`, the
        // engine says `void`/`?string`/`int`.
        for name in ["sodium_add", "sodium_increment", "xml_error_string", "pg_port", "imageinterlace"] {
            assert_eq!(super::declared_return(name), None, "{name} must stay excluded");
        }
        for name in ["intlcal_get", "socket_cmsg_space", "ldap_compare", "pg_last_notice"] {
            assert_eq!(super::declared_return(name), None, "{name}: the row drops an engine arm");
        }
        for name in ["imageloadfont", "pow", "rewinddir", "substr_compare", "fpassthru"] {
            assert_eq!(super::declared_return(name), None, "{name}: an #79 candidate the engine disowns");
        }
        for name in [
            "ftp_raw",
            "mysqli_fetch_row",
            "locale_get_keywords",
            "odbc_data_source",
            "str_word_count",
            "fscanf",
            "ob_list_handlers",
            "socket_addrinfo_lookup",
        ] {
            assert_eq!(super::declared_return(name), None, "{name}: an ADR-0071 candidate the engine disowns");
        }
        for name in [
            "stream_bucket_make_writeable",
            "intlcal_create_instance",
            "intltz_create_time_zone",
            "msgfmt_create",
            "numfmt_create",
            "tidy_get_root",
            "tidy_get_body",
            "datefmt_create",
            "dom_import_simplexml",
            "xmlwriter_open_uri",
            "mysqli_get_charset",
        ] {
            assert_eq!(super::declared_return(name), None, "{name}: a class candidate the engine disowns");
        }
        for name in ["json_last_error", "session_status"] {
            assert_eq!(super::declared_return(name), None, "{name}: constants are not class names");
        }
        for name in ["base64_decode", "phpversion", "getenv"] {
            assert_eq!(super::declared_return(name), None, "{name} has disagreeing alternates");
        }
    }

    #[test]
    fn declared_return_version_sensitivity_is_recorded() {
        assert_eq!(super::declared_return_changed_at("str_split"), Some((8, 2)));
        assert_eq!(super::declared_return_changed_at("gc_status"), Some((8, 3)));
        assert_eq!(super::declared_return_changed_at("session_get_cookie_params"), Some((8, 5)));
        assert_eq!(super::declared_return_changed_at("STR_SPLIT"), Some((8, 2)));
        assert_eq!(super::declared_return_changed_at("str_repeat"), None);
        assert_eq!(super::declared_return_changed_at("some_unknown_fn"), None);
        let t = super::declared_returns_generated::RETURN_VERSION_SENSITIVE;
        assert!(!t.is_empty(), "the change oracle must not be silently empty");
        assert!(
            t.windows(2).all(|w| w[0].0 < w[1].0),
            "RETURN_VERSION_SENSITIVE must be strictly sorted by key"
        );
        // ADR-0071's array widening now makes these tables INTERSECT; the
        // end-to-end fixture lives in steins-infer's `declared_return_floor.rs`.
        for (name, _) in t {
            assert!(
                super::declared_return(name).is_some(),
                "{name}: a version-sensitive name must carry a row for the gate to decide"
            );
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
        assert_eq!(p("App\\Exception"), None);
        assert_eq!(p("MyCustomThing"), None);
    }

    #[test]
    fn builtin_throws_curated() {
        assert_eq!(
            super::builtin_throws("intdiv"),
            Some(&["DivisionByZeroError", "ArithmeticError"][..])
        );
        assert_eq!(super::builtin_throws("preg_match"), Some(&["ValueError"][..]));
        assert_eq!(super::builtin_throws("random_int"), Some(&["ValueError"][..]));
        assert_eq!(super::builtin_throws("HASH"), Some(&["ValueError"][..]));
        assert_eq!(super::builtin_throws("json_decode_throwing"), Some(&["JsonException"][..]));
        assert_eq!(super::builtin_throws("strlen"), None);
    }

    #[test]
    fn builtin_class_supers_tree() {
        use super::builtin_class_supers as s;
        assert_eq!(s("Throwable"), Some(vec!["Stringable"]));
        assert_eq!(s("UnitEnum"), Some(vec![]));
        assert_eq!(s("Stringable"), Some(vec![]));
        assert_eq!(s("BackedEnum"), Some(vec!["UnitEnum"]));
        assert_eq!(s("Exception"), Some(vec!["Throwable"]));
        assert_eq!(s("RuntimeException"), Some(vec!["Exception"]));
        assert_eq!(s("TypeError"), Some(vec!["Error"]));
        assert_eq!(s("\\backedenum"), Some(vec!["UnitEnum"]));
        assert_eq!(s("MyCustomThing"), None);
        assert_eq!(s("App\\Suit"), None);
    }

    #[test]
    fn builtin_class_supers_from_mined_hierarchy() {
        use super::builtin_class_supers as s;
        assert_eq!(
            s("ArrayObject"),
            Some(vec!["IteratorAggregate", "ArrayAccess", "Serializable", "Countable"])
        );
        assert_eq!(s("IteratorAggregate"), Some(vec!["Traversable"]));
        assert_eq!(s("FFI\\Exception"), Some(vec!["Error"]));
        assert_eq!(s("\\FFI\\ParserException"), Some(vec!["Exception"]));
        // Builtin enums are deliberately ABSENT: incomplete implicit-interface /
        // backing data → Unknown, never a spurious No.
        assert_eq!(s("RoundingMode"), None);
        assert_eq!(s("IntervalBoundary"), None);
    }

    #[test]
    fn hierarchy_table_is_sorted_for_binary_search() {
        let t = super::hierarchy_generated::HIERARCHY;
        assert!(t.windows(2).all(|w| w[0].0 < w[1].0), "HIERARCHY must be strictly sorted by key");
    }

    #[test]
    fn display_names_answer_the_declared_casing() {
        use super::builtin_class_display as d;
        assert_eq!(d("gmp"), Some("GMP"));
        assert_eq!(d("hashcontext"), Some("HashContext"));
        assert_eq!(d("xmlparser"), Some("XMLParser"));
        assert_eq!(d("dateinterval"), Some("DateInterval"));
        assert_eq!(d("GMP"), Some("GMP"));
        assert_eq!(d("\\DateInterval"), Some("DateInterval"));
        assert_eq!(d("ffi\\cdata"), Some("FFI\\CData"));
        assert_eq!(d("com"), Some("com"));
        // Enums ARE here, even though `builtin_class_supers` skips them: that
        // exclusion guards the is-a oracle, not the display surface.
        assert_eq!(d("roundingmode"), Some("RoundingMode"));
        assert_eq!(super::builtin_class_supers("roundingmode"), None);
        assert_eq!(d("App\\GMP"), None);
        assert_eq!(d("nosuchclass"), None);
    }

    #[test]
    fn display_name_table_is_sorted_and_self_consistent() {
        let t = super::display_names_generated::DISPLAY_NAMES;
        assert!(t.windows(2).all(|w| w[0].0 < w[1].0), "DISPLAY_NAMES must be strictly sorted");
        for &(key, name) in t {
            assert_eq!(key, name.to_ascii_lowercase(), "key must be the lowercased value");
        }
        for &(key, _) in super::hierarchy_generated::HIERARCHY {
            assert!(
                super::builtin_class_display(key).is_some(),
                "hierarchy key `{key}` has no display row"
            );
        }
    }

    #[test]
    fn exception_parent_agrees_with_generated_hierarchy() {
        for &(name, supers) in super::hierarchy_generated::HIERARCHY {
            if let Some(parent) = super::builtin_exception_parent(name) {
                assert_eq!(
                    Some(&parent),
                    supers.first(),
                    "throw-tree parent of `{name}` disagrees with generated hierarchy"
                );
            }
        }
    }

    use super::{failure_arms, FailureArms, FailureCause};

    #[test]
    fn failure_arms_classifies_by_cause() {
        use FailureCause::{Environment, Input, Resource};
        assert_eq!(failure_arms("curl_init"), Some(FailureArms::Causes(&[Resource, Input])));
        assert_eq!(failure_arms("proc_open"), Some(FailureArms::Causes(&[Input, Environment])));
        assert_eq!(failure_arms("fopen"), Some(FailureArms::Causes(&[Environment])));
        assert_eq!(failure_arms("preg_match"), Some(FailureArms::Causes(&[Input])));
        assert_eq!(failure_arms("socket_create"), Some(FailureArms::Causes(&[Resource])));
        assert_eq!(failure_arms("FOPEN"), Some(FailureArms::Causes(&[Environment])));
    }

    #[test]
    fn failure_arms_sentinels_are_not_failures() {
        for name in ["array_search", "strpos", "array_key_first", "next", "current", "reset"] {
            assert_eq!(failure_arms(name), Some(FailureArms::Sentinel), "{name} is a sentinel");
        }
        assert_eq!(failure_arms("strlen"), None);
        assert_eq!(failure_arms("some_unknown_fn"), None);
    }

    #[test]
    fn failure_cause_labels_are_registered_dot_paths() {
        assert_eq!(FailureCause::Resource.label(), "failure.resource");
        assert_eq!(FailureCause::Environment.label(), "failure.environment");
        assert_eq!(FailureCause::Input.label(), "failure.input");
        for c in [FailureCause::Resource, FailureCause::Environment, FailureCause::Input] {
            assert!(is_known_label(c.label()), "{} should be known", c.label());
            assert!(subsumes("failure", c.label()), "failure.* subsumes {}", c.label());
        }
    }

    use super::{invocation_shape, ArgSource, Invocation};

    #[test]
    fn invocation_shapes_of_the_starter_set() {
        let s = |n| invocation_shape(n).expect("known invoker");
        assert_eq!(s("array_map").callback_param, 0);
        assert_eq!(s("array_map").invocation, Invocation::Immediate);
        assert_eq!(s("array_map").arg_source, ArgSource::ElementsOf(1));
        // array_filter: REVERSED — array first, cb at 1.
        assert_eq!(s("array_filter").callback_param, 1);
        assert_eq!(s("array_filter").arg_source, ArgSource::ElementsOf(0));
        assert_eq!(s("array_walk").callback_param, 1);
        assert_eq!(s("array_walk").arg_source, ArgSource::ElementsOf(0));
        for n in ["usort", "uasort", "uksort", "array_reduce"] {
            assert_eq!(s(n).callback_param, 1, "{n}");
            assert_eq!(s(n).arg_source, ArgSource::None, "{n}");
            assert_eq!(s(n).invocation, Invocation::Immediate, "{n}");
        }
        assert_eq!(s("call_user_func").callback_param, 0);
        assert_eq!(s("call_user_func_array").callback_param, 0);
        assert_eq!(s("register_shutdown_function").callback_param, 0);
        assert_eq!(s("register_shutdown_function").invocation, Invocation::Deferred);
        assert_eq!(s("preg_replace_callback").callback_param, 1);
    }

    #[test]
    fn adr0063_p1_immediately_invoked_rows() {
        let s = |n| invocation_shape(n).expect("known invoker");
        for n in ["array_find", "array_find_key", "array_any", "array_all"] {
            assert_eq!(s(n).callback_param, 1, "{n}");
            assert_eq!(s(n).invocation, Invocation::Immediate, "{n}");
            assert_eq!(s(n).arg_source, ArgSource::ElementsOf(0), "{n}");
        }
        // array_walk_recursive's callback sees leaves, so unmodeled.
        assert_eq!(s("array_walk_recursive").callback_param, 1);
        assert_eq!(s("array_walk_recursive").invocation, Invocation::Immediate);
        assert_eq!(s("array_walk_recursive").arg_source, ArgSource::None);
        assert_eq!(s("iterator_apply").callback_param, 1);
        assert_eq!(s("iterator_apply").invocation, Invocation::Immediate);
    }

    #[test]
    fn adr0063_p1_exclusions_carry_no_shape() {
        for n in [
            "set_error_handler",
            "set_exception_handler",
            "spl_autoload_register",
            "register_tick_function",
            "header_register_callback",
            "ob_start",
            "preg_replace_callback_array",
            "array_udiff",
            "array_uintersect",
            "array_udiff_assoc",
            "array_diff_ukey",
            "array_intersect_ukey",
            "array_udiff_uassoc",
            "array_uintersect_uassoc",
        ] {
            assert_eq!(invocation_shape(n), None, "{n} must stay excluded");
        }
        assert_eq!(
            invocation_shape("register_shutdown_function").map(|s| s.invocation),
            Some(Invocation::Deferred)
        );
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
