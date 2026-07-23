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

/// The PHP minor version the builtin catalog is pinned to (`major`, `minor`) —
/// the php-src mining data (`docs/research/phpsrc-mining/hierarchy.toml`, pin
/// `6bc7c26cf6…`) is cross-checked against **PHP 8.5.8**, so the builtin
/// class-hierarchy edges this crate reports are those of the `8.5` line.
///
/// ADR-0052 amendment A11: a catalog-backed is-a verdict used for **arm deletion**
/// is only trustworthy when the project's own PHP is on this same minor line — a
/// different minor may add/remove a builtin supertype edge the catalog does not
/// reflect. The narrowing engine compares the sidecar-reported minor against this
/// pin and, on a skew, demotes such a verdict to `Unknown` (keeping the arm, the
/// FP-safe side). The patch component (`8` in `8.5.8`) is irrelevant — builtin
/// type edges are stable within a minor line — so only `(major, minor)` is pinned.
pub const PINNED_PHP: (u16, u16) = (8, 5);

/// The builtin class-hierarchy table, generated from the pinned php-src mining
/// data (`docs/research/phpsrc-mining/hierarchy.toml`) by `cargo xtask
/// gen-catalog`. Consulted only by [`builtin_class_supers`]; see that function
/// and `xtask/src/gen_catalog.rs` for the generation contract.
mod hierarchy_generated;

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
    const IO_SIGNAL: &[&str] = &["io.signal"];
    const OUTPUT_HEADER: &[&str] = &["output.header"];
    const IO_IPC: &[&str] = &["io.ipc"];
    // `session_start` is genuinely composite (effects_gaps.md): the default file
    // handler writes session files (`io.fs.write`), the session cookie is sent as
    // a `Set-Cookie` header (`output.header`), and `$_SESSION`/ini are mutated
    // (`global.write`). The upper-bound set is all three.
    const SESSION: &[&str] = &["io.fs.write", "output.header", "global.write"];

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
        // Signal delivery/handling (effects_gaps.md §1). pcntl/posix procedural
        // functions; a daemon/worker envelope declares `@effects io.signal`.
        "pcntl_signal" | "pcntl_signal_dispatch" | "pcntl_alarm" | "pcntl_async_signals"
        | "pcntl_sigprocmask" | "pcntl_sigwaitinfo" | "posix_kill" => Some(IO_SIGNAL),
        // HTTP response-header mutation (effects_gaps.md §2).
        "header" | "header_remove" | "setcookie" | "setrawcookie" | "http_response_code" => {
            Some(OUTPUT_HEADER)
        }
        // System-V / shared-memory IPC (effects_gaps.md §4).
        "shmop_write" | "shmop_read" | "sem_acquire" | "sem_release" | "msg_send"
        | "msg_receive" => Some(IO_IPC),
        // Composite session bootstrap (effects_gaps.md).
        "session_start" => Some(SESSION),
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
        // Failure-cause provenance family (ADR-0042): the benevolent-union
        // replacement. These label a `false`/`null` failure arm's *value
        // provenance* (why the arm exists), not an effect; they share the ADR-0018
        // registry so prefix subsumption (`failure` admits `failure.environment`)
        // works, and so a future boundary profile can name them. See
        // [`failure_arms`].
        "failure",
        "failure.environment",
        "failure.input",
        "failure.resource",
        // Opaque native boundary (php-src FFI): runs arbitrary C, so the catalog
        // can prove nothing about it — a deliberately top-level escape hatch
        // beside `exit`/`mutate` (effects_gaps.md §3). FFI is OO-only, so no plain
        // builtin is colored `ffi` yet; the label exists so an `@effects ffi`
        // envelope declaration is valid.
        "ffi",
        "global.read",
        "global.write",
        "io",
        "io.db",
        "io.fs",
        "io.fs.read",
        "io.fs.write",
        // System-V / shared-memory IPC (effects_gaps.md §4): cross-process shared
        // state, neither filesystem nor network.
        "io.ipc",
        "io.net",
        "io.net.http",
        "io.process",
        // Signal delivery/handling (pcntl/posix; effects_gaps.md §1): an
        // observable OS interaction, parallel to `io.process`.
        "io.signal",
        "mutate",
        "nondet",
        "nondet.random",
        "nondet.time",
        "output",
        // HTTP response-header mutation (effects_gaps.md §2): a response-side
        // sibling of stdout `output`; a coarse `output` subsumes it, a policy can
        // name it precisely.
        "output.header",
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
///
/// This is the **frozen throw-system projection** of the builtin hierarchy: it
/// covers exactly the core SPL/engine `Throwable` tree the throw accounting
/// (ADR-0040) reasons over, and is deliberately *not* widened to the full mined
/// hierarchy ([`builtin_class_supers`]) — expanding the throw world is the job of
/// the throw-catalog slices (ADR-0043 §5 gate discipline), not the is-a
/// ingestion. A test (`exception_parent_agrees_with_generated_hierarchy`) proves
/// this projection never conflicts with the generated table, so there is still a
/// single source of truth for every edge both know.
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
/// The data is the **single source of truth** for the builtin hierarchy: the 352
/// production classes + interfaces mined from php-src (pin
/// `6bc7c26cf6…`, cross-checked vs PHP 8.5.8), generated into
/// [`hierarchy_generated::HIERARCHY`] by `cargo xtask gen-catalog` from
/// `docs/research/phpsrc-mining/hierarchy.toml`. It subsumes the SPL/engine
/// `Throwable` tree (also projected, frozen, by [`builtin_exception_parent`] for
/// the throw system — a test verifies the two agree on their overlap) and the
/// enum interface roots (`UnitEnum`; `BackedEnum extends UnitEnum`;
/// `Throwable extends Stringable`).
///
/// Matching is case-insensitive. **Namespaced** builtin classes (`Random\…`,
/// `FFI\…`) *are* resolved here — the key preserves the backslash, and an unknown
/// namespaced name simply misses the table (→ `None`). **Builtin enums are
/// deliberately absent** (→ `None` → `Unknown`): the mining data omits an enum's
/// implicit `UnitEnum`/`BackedEnum` interfaces and its backing, so its edge set is
/// incomplete and a `No` against those interfaces would be unsound (ADR-0043 §3).
#[must_use]
pub fn builtin_class_supers(name: &str) -> Option<Vec<&'static str>> {
    let key = name.trim_start_matches('\\').to_ascii_lowercase();
    hierarchy_generated::HIERARCHY
        .binary_search_by(|(n, _)| (*n).cmp(key.as_str()))
        .ok()
        .map(|i| hierarchy_generated::HIERARCHY[i].1.to_vec())
}

/// The **measured/curated** throw facts of a builtin call (ADR-0040 source #2):
/// the global class names a builtin provably raises. Deliberately tiny and
/// hand-verified — an uncatalogued builtin simply contributes no throw fact
/// (widen, never a false positive). Empty slice = catalogued-but-throwless.
#[must_use]
pub fn builtin_throws(name: &str) -> Option<&'static [&'static str]> {
    // intdiv has TWO input-determined arms (php-src `ext/standard/math.c`,
    // throws.toml): `divisor == 0` → DivisionByZeroError (math.c:1502), and the
    // `PHP_INT_MIN / -1` overflow → ArithmeticError (math.c:1507). The complete
    // set is both; DivisionByZeroError extends ArithmeticError, so a coarse
    // `@throws ArithmeticError` subsumes both. Both are is-a `Error` → unchecked
    // (ADR-0007), so they enrich the throw envelope without adding
    // `throw.undeclared` noise.
    const INTDIV: &[&str] = &["DivisionByZeroError", "ArithmeticError"];
    const JSON: &[&str] = &["JsonException"];
    // Input-determined `ValueError` throws mined from php-src C (throws.toml,
    // ADR-0040 source #2): PHP-8 migration turned a family of argument-value
    // misuses (bad flags/offset/length, unknown hash algo, `$min > $max`, malformed
    // descriptor spec, …) from `false`-returns into `ValueError`. Each row is
    // C-evidenced and statically refutable with proven args. `ValueError` is-a
    // `Error` → unchecked (ADR-0007). Flag-gated JSON throws are deliberately NOT
    // here (see below). Method-shaped constructor throws (DateTime::__construct →
    // DateMalformedStringException) are deferred — they need the Date* exception
    // family wired into the frozen throw tree first.
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
        // JSON_THROW_ON_ERROR; without flag inspection this stays uncatalogued
        // (widen) rather than manufacture a throw — listed for when flag
        // inspection lands. (The plain `json_decode` key above carries its
        // *unconditional* `$depth`-misuse ValueError, a separate arm.)
        "json_decode_throwing" | "json_encode_throwing" => Some(JSON),
        _ => None,
    }
}

/// The **cause** of a builtin's `false`/`null` failure arm (ADR-0042): a fact the
/// catalog can state, never a probability it cannot. Each maps to a `failure.*`
/// value-provenance label ([`known_labels`]) that a future boundary profile
/// consumes to decide must-check policy (default exempts [`Resource`], includes
/// [`Environment`]; strict includes both) — the honest-union + policy-profile
/// replacement for ADR-0030's erased benevolent union.
///
/// [`Resource`]: FailureCause::Resource
/// [`Environment`]: FailureCause::Environment
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureCause {
    /// Allocation/handle exhaustion (`curl_init`, `imagecreate*`, `socket_create`
    /// fd-exhaustion): statically irrefutable, unrecoverable in practice. Label
    /// `failure.resource`. Default profile exempts it from must-check.
    Resource,
    /// Filesystem/network/external-state failure (`fopen`, `fsockopen`): a normal
    /// operational outcome; not checking it is a real bug. Label
    /// `failure.environment`. Both profiles require the check.
    Environment,
    /// Argument-value-determined failure (`preg_match` malformed pattern,
    /// `json_encode` unencodable value): statically refutable with proven args —
    /// the fallback label for sites whose arguments stay unproven. Label
    /// `failure.input`.
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

/// The failure-arm classification of a builtin (ADR-0042), as mined from php-src
/// C (`docs/research/phpsrc-mining/failure_arms.toml`). Distinguishes the three
/// states the boundary profile must tell apart:
///
/// * `Some(FailureArms::Causes(&[…]))` — the `false`/`null` arm is a real failure,
///   carrying the distinct [`FailureCause`]s its arms were traced to (a function
///   may fail for more than one cause: `curl_init` is `[Resource, Input]`,
///   `proc_open` is `[Input, Environment]`).
/// * `Some(FailureArms::Sentinel)` — the `false`/`null` return is a **legitimate
///   non-failure result** (`strpos` "not present", `array_search` "not found",
///   `next()` past end): it must NOT receive any `failure.*` label. This is
///   *explicitly not a failure*, deliberately distinct from…
/// * `None` — **unclassified**: the catalog states nothing about this name.
///
/// Nothing consumes this yet (the boundary profiles of ADR-0037 are future work),
/// so it is behavior-neutral catalog data; the shape is the minimal one those
/// profiles need — a per-call cause set plus the sentinel exclusion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureArms {
    /// The distinct failure causes the arm(s) were traced to (order: as recorded).
    Causes(&'static [FailureCause]),
    /// The `false`/`null` is a legitimate result, never to be `failure.*`-labeled.
    Sentinel,
}

/// The [`FailureArms`] classification of a builtin `name` (ADR-0042), or `None`
/// when the name is unclassified. Matching is case-insensitive.
///
/// This is the queryable catalog side of the failure-cause labels: it states, per
/// builtin, whether its `false`/`null` arm is a failure (and of what cause) or a
/// legitimate sentinel result. Method-shaped rows from the mining data
/// (`DateTime::createFromFormat`) are deferred — the current API is
/// function-keyed. See `docs/research/phpsrc-mining/failure_arms.toml` (the
/// source of record) for per-arm C evidence.
#[must_use]
pub fn failure_arms(name: &str) -> Option<FailureArms> {
    use FailureCause::{Environment, Input, Resource};
    const RESOURCE: &[FailureCause] = &[Resource];
    const ENVIRONMENT: &[FailureCause] = &[Environment];
    const INPUT: &[FailureCause] = &[Input];
    // Multi-cause arms (each distinct cause the mining traced, in recorded order).
    const RESOURCE_INPUT: &[FailureCause] = &[Resource, Input];
    const INPUT_ENVIRONMENT: &[FailureCause] = &[Input, Environment];

    let arms = |c| Some(FailureArms::Causes(c));
    match name.to_ascii_lowercase().as_str() {
        // cURL.
        "curl_init" => arms(RESOURCE_INPUT),
        "curl_exec" => arms(ENVIRONMENT),
        "curl_setopt" => arms(INPUT),
        // Filesystem open/read/write — environmental.
        "fopen" | "file_get_contents" | "file_put_contents" | "file" | "readfile" | "fread"
        | "fwrite" | "fgets" | "fscanf" | "tmpfile" | "mkdir" | "unlink" | "rename" | "copy"
        | "scandir" => arms(ENVIRONMENT),
        // Streams / sockets — network is environmental.
        "fsockopen" | "pfsockopen" | "stream_socket_client" | "stream_get_contents" => {
            arms(ENVIRONMENT)
        }
        // PCRE — input-determined (pattern+subject).
        "preg_match" | "preg_match_all" | "preg_replace" | "preg_split" => arms(INPUT),
        // Serialization / conversion / time — input-determined.
        "json_decode" | "json_encode" | "unserialize" | "strtotime" | "date_create" | "iconv"
        | "mb_convert_encoding" => arms(INPUT),
        // hash_file straddles but reads primarily environmental (file unreadable).
        "hash_file" => arms(ENVIRONMENT),
        // Environment/external process state.
        "getenv" => arms(ENVIRONMENT),
        // IPC / process.
        "proc_open" => arms(INPUT_ENVIRONMENT),
        "sem_get" | "shmop_open" => arms(ENVIRONMENT),
        "socket_create" => arms(RESOURCE),
        // NOT-A-FAILURE SENTINELS — `false`/`null` is a legitimate result. These
        // MUST stay distinct from unclassified (`None`): the boundary profile must
        // know never to label them, not merely lack an opinion. Exactly the
        // failure_arms.toml `[[sentinel]]` set (`next` note names the internal-
        // pointer siblings current/prev/end/reset explicitly).
        "array_search" | "strpos" | "array_key_first" | "next" | "current" | "prev" | "end"
        | "reset" => Some(FailureArms::Sentinel),
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
        // intdiv now carries BOTH input-determined arms (throws.toml, math.c:1502/1507).
        assert_eq!(
            super::builtin_throws("intdiv"),
            Some(&["DivisionByZeroError", "ArithmeticError"][..])
        );
        // Input-determined ValueError rows (php-src throws.toml).
        assert_eq!(super::builtin_throws("preg_match"), Some(&["ValueError"][..]));
        assert_eq!(super::builtin_throws("random_int"), Some(&["ValueError"][..]));
        assert_eq!(super::builtin_throws("HASH"), Some(&["ValueError"][..])); // case-insensitive
        // Flag-gated JSON stays under its placeholder key (widen for plain json_*).
        assert_eq!(super::builtin_throws("json_decode_throwing"), Some(&["JsonException"][..]));
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
    fn builtin_class_supers_from_mined_hierarchy() {
        use super::builtin_class_supers as s;
        // A class with multiple direct supers (extends none, implements many).
        assert_eq!(
            s("ArrayObject"),
            Some(vec!["IteratorAggregate", "ArrayAccess", "Serializable", "Countable"])
        );
        // Interface→interface edge (needed so the closure reaches Traversable).
        assert_eq!(s("IteratorAggregate"), Some(vec!["Traversable"]));
        // Namespaced builtin classes ARE resolved now (backslash kept in key).
        assert_eq!(s("FFI\\Exception"), Some(vec!["Error"]));
        assert_eq!(s("\\FFI\\ParserException"), Some(vec!["Exception"]));
        // Builtin enums are deliberately ABSENT (incomplete implicit-interface /
        // backing data → Unknown, never a spurious No). See gen_catalog.rs.
        assert_eq!(s("RoundingMode"), None);
        assert_eq!(s("IntervalBoundary"), None);
    }

    #[test]
    fn hierarchy_table_is_sorted_for_binary_search() {
        // The generated table MUST be sorted by key or `binary_search_by` in
        // `builtin_class_supers` silently misses entries. Guards regen drift.
        let t = super::hierarchy_generated::HIERARCHY;
        assert!(t.windows(2).all(|w| w[0].0 < w[1].0), "HIERARCHY must be strictly sorted by key");
    }

    #[test]
    fn exception_parent_agrees_with_generated_hierarchy() {
        // One source of truth: the frozen throw-tree projection
        // (`builtin_exception_parent`) must never conflict with the generated
        // hierarchy. For every table entry, if the throw tree names a parent it
        // must be that class's first (single) recorded super — except Throwable,
        // the throw-root, whose only super is the non-Throwable `Stringable`.
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

    #[test]
    fn new_effect_labels_are_registered_and_subsume() {
        // S4 additions (effects_gaps.md) are known and prefix-subsume correctly.
        for label in ["ffi", "io.signal", "io.ipc", "output.header"] {
            assert!(is_known_label(label), "{label} should be a known registry label");
        }
        assert!(subsumes("io", "io.signal"), "coarse io admits io.signal");
        assert!(subsumes("io", "io.ipc"), "coarse io admits io.ipc");
        assert!(subsumes("output", "output.header"), "coarse output admits output.header");
        assert!(!subsumes("io.signal", "io.ipc"), "siblings do not subsume");
        // ffi is a top-level escape hatch, not under io.
        assert!(!subsumes("io", "ffi"));
    }

    #[test]
    fn new_effect_labels_color_the_mined_functions() {
        // io.signal (pcntl/posix), output.header (header/cookies), io.ipc (sysv),
        // and the composite session bootstrap.
        assert_eq!(effect_labels("pcntl_signal"), Some(&["io.signal"][..]));
        assert_eq!(effect_labels("posix_kill"), Some(&["io.signal"][..]));
        assert_eq!(effect_labels("header"), Some(&["output.header"][..]));
        assert_eq!(effect_labels("setcookie"), Some(&["output.header"][..]));
        assert_eq!(effect_labels("shmop_write"), Some(&["io.ipc"][..]));
        assert_eq!(
            effect_labels("session_start"),
            Some(&["io.fs.write", "output.header", "global.write"][..])
        );
    }

    use super::{failure_arms, FailureArms, FailureCause};

    #[test]
    fn failure_arms_classifies_by_cause() {
        use FailureCause::{Environment, Input, Resource};
        // Multi-cause: curl_init is resource ∪ input; proc_open is input ∪ environment.
        assert_eq!(failure_arms("curl_init"), Some(FailureArms::Causes(&[Resource, Input])));
        assert_eq!(failure_arms("proc_open"), Some(FailureArms::Causes(&[Input, Environment])));
        // Single-cause canonical examples (ADR-0042).
        assert_eq!(failure_arms("fopen"), Some(FailureArms::Causes(&[Environment])));
        assert_eq!(failure_arms("preg_match"), Some(FailureArms::Causes(&[Input])));
        assert_eq!(failure_arms("socket_create"), Some(FailureArms::Causes(&[Resource])));
        // Case-insensitive.
        assert_eq!(failure_arms("FOPEN"), Some(FailureArms::Causes(&[Environment])));
    }

    #[test]
    fn failure_arms_sentinels_are_not_failures() {
        // Explicitly NOT-a-failure — distinct from unclassified (None).
        for name in ["array_search", "strpos", "array_key_first", "next", "current", "reset"] {
            assert_eq!(failure_arms(name), Some(FailureArms::Sentinel), "{name} is a sentinel");
        }
        // Unclassified names return None (no opinion), NOT Sentinel.
        assert_eq!(failure_arms("strlen"), None);
        assert_eq!(failure_arms("some_unknown_fn"), None);
    }

    #[test]
    fn failure_cause_labels_are_registered_dot_paths() {
        assert_eq!(FailureCause::Resource.label(), "failure.resource");
        assert_eq!(FailureCause::Environment.label(), "failure.environment");
        assert_eq!(FailureCause::Input.label(), "failure.input");
        // The family is in the ADR-0018 registry with working prefix subsumption.
        for c in [FailureCause::Resource, FailureCause::Environment, FailureCause::Input] {
            assert!(is_known_label(c.label()), "{} should be known", c.label());
            assert!(subsumes("failure", c.label()), "failure.* subsumes {}", c.label());
        }
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
