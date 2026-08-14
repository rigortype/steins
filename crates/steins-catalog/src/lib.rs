//! Curated signatures and effect entries for PHP builtins and extensions.
//!
//! # Folding gate
//!
//! [`foldable`] is the hand-curated ADR-0008 allowlist: admitted only when pure
//! and deterministic on the concrete path, else it widens (locale-, timezone-,
//! encoding-, global-, nondeterminism-sensitive functions stay excluded).
//!
//! `WIDTH_REFUSED` and `WIDTH_UNVERIFIED` fold on a proven 64-bit engine but
//! decline on 32-bit; a refused row has a recorded divergence, an unverified
//! row has none (see [`WidthClass`]). Other exclusions and their evidence:
//!
//! * `strtotime`/`date`/`idate` are `nondet.time`, timezone-coupled even with
//!   explicit timestamps.
//! * `mb_*` depends on `mbstring.internal_encoding`; php-wasm 0.1.0 lacks it.
//! * `strcmp`/`strcasecmp` promise only a sign, not `memcmp`'s
//!   implementation-defined magnitude.
//! * `number_format` stays conservatively excluded despite no probed divergence.
//! * `bin2hex` is excluded per its ADR-0056 empty-in/empty-out return-fact
//!   refusal (`docs/research/phpsrc-mining/return_facts.toml`).

/// The PHP minor version the builtin catalog is pinned to (`major`, `minor`):
/// mining data (`docs/research/phpsrc-mining/hierarchy.toml`, pin
/// `6bc7c26cf6…`) is cross-checked against **PHP 8.5.8**, so reported
/// class-hierarchy edges are those of the `8.5` line.
///
/// ADR-0052 amendment A11: a catalog-backed is-a verdict used for **arm
/// deletion** is trustworthy only when the project's own PHP is on this same
/// minor line. On a skew, the narrowing engine demotes such a verdict to
/// `Unknown` (FP-safe). Only `(major, minor)` is pinned — builtin type edges
/// are stable within a minor line.
pub const PINNED_PHP: (u16, u16) = (8, 5);

/// Builtin class-hierarchy table, from `docs/research/phpsrc-mining/hierarchy.toml`
/// via `cargo xtask gen-catalog`. Consulted only by [`builtin_class_supers`].
mod hierarchy_generated;

/// Builtin class **display-name** table, from the same mining data — lowercased
/// key → the casing php-src declares. Consulted only by [`builtin_class_display`].
mod display_names_generated;

/// Builtin return-fact refinement table (ADR-0056), from
/// `docs/research/phpsrc-mining/return_facts.toml`. Consulted only by
/// [`return_fact`]. May be empty.
mod return_facts_generated;

/// **Resource-return** table (ADR-0056 §8), from
/// `docs/research/phpsrc-mining/resource_returns.toml`. Consulted only by
/// [`resource_return`].
mod resource_returns_generated;

/// Builtin declared-return floor (ADR-0069, issues #73/#79), from
/// `docs/research/phpstan-mining/declared_returns.toml`. Consulted only by
/// [`declared_return`] and [`declared_return_changed_at`].
mod declared_returns_generated;

// Capture-group structure of a literal PCRE pattern (issue #149). Carries its
// own module doc, so this stays a plain comment to avoid merging headers.
pub mod preg;

/// Whether `name` is on the folding allowlist (case-insensitive).
///
/// A `true` here is *permission to fold*, not a promise the call folds: the
/// engine still requires a non-user callee and all-literal arguments (scalar
/// or recursively concrete arrays, permitting `sprintf`/`str_replace`/
/// `in_array`/`count`/`implode`, issue #39). A folded result may likewise be
/// an array (ADR-0028's 2026-08-14 amendment, issue #330).
///
/// The allowlist is the union of `WIDTH_SAFE`, `WIDTH_REFUSED`,
/// `WIDTH_UNVERIFIED` (issue #64; amendment §4 added the third), so
/// [`foldable`] is a *derived* predicate and [`width_class`] the primitive.
/// `mb_*`, locale-sensitive, and `nondet` functions are excluded.
#[must_use]
pub fn foldable(name: &str) -> bool {
    width_class(name).is_some()
}

/// The number of names on the folding allowlist (ADR-0054 §9.6 freshness
/// context, the [`foldable`] twin of [`hierarchy_entry_count`]): the union of
/// the three width classes, which are disjoint by construction.
#[must_use]
pub fn foldable_entry_count() -> usize {
    WIDTH_SAFE.len() + WIDTH_REFUSED.len() + WIDTH_UNVERIFIED.len()
}

/// Which integer-width class a foldable name sits in — the **primitive** the
/// folding allowlist is derived from (ADR-0028's 2026-08-14 amendment §4).
///
/// `None` means not on the allowlist. The three `Some` arms classify
/// *evidence*, not behaviour: only [`WidthClass::Safe`] changes what the fold
/// gate admits; the other two are mechanically identical but kept apart to
/// keep the refused rows' one-divergence-per-row discipline auditable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WidthClass {
    /// **Measured, and it agrees.** Differential probes (php-wasm 0.1.0,
    /// `PHP_INT_SIZE = 4`, vs `php` 8.5.8 at 8) found identical value and type
    /// tag, or a decline, for every argument the range guard admits.
    Safe,
    /// **Measured, and it disagrees.** At least one probe found both engines
    /// silently differing. Folds only on a provably 64-bit engine.
    Refused,
    /// **Not measured.** Folds only on a provably 64-bit engine, the same gate
    /// [`WidthClass::Refused`] rides. See `WIDTH_UNVERIFIED`.
    Unverified,
}

/// The width class of `name` (case-insensitive), or `None` when not on the
/// allowlist. The lists are disjoint, so the search order below is a cost
/// decision, not a precedence rule.
#[must_use]
pub fn width_class(name: &str) -> Option<WidthClass> {
    let listed = |list: &[&str]| list.iter().any(|&f| name.eq_ignore_ascii_case(f));
    if listed(WIDTH_SAFE) {
        Some(WidthClass::Safe)
    } else if listed(WIDTH_REFUSED) {
        Some(WidthClass::Refused)
    } else if listed(WIDTH_UNVERIFIED) {
        Some(WidthClass::Unverified)
    } else {
        None
    }
}

/// Whether folding `name` is **safe on a 32-bit engine** (case-insensitive),
/// given the caller already applied the argument range guard.
///
/// # The rule
///
/// Width-safe means: for every argument tuple where every integer (values and
/// array keys, recursively) lies within `[-(2^31 - 1), 2^31 - 1]`, a 32-bit
/// engine returns the **identical value and type tag** a 64-bit engine
/// returns, or **declines** (ADR-0066 §4). The lower bound is `-(2^31 - 1)`,
/// not `-2^31`, to keep the `abs`-shaped boundary flip unreachable.
///
/// Verified by **661 adversarial-tuple** differential probes against php-wasm
/// 0.1.0 (`PHP_INT_SIZE = 4`) and `php` 8.5.8 (`PHP_INT_SIZE = 8`). See the
/// ADR-0066 amendments for per-name tables.
///
/// A `false` here is a refusal to certify, not a claim of width-sensitivity.
/// Default-deny: unclassified names fold only on a provably 64-bit engine.
#[must_use]
pub fn width_safe(name: &str) -> bool {
    WIDTH_SAFE.iter().any(|&f| name.eq_ignore_ascii_case(f))
}

// A private `width_refused` predicate (complement of `width_safe`) lived here;
// `width_class(name) == Some(Refused)` replaced it since "not safe" and
// "refused" stopped being the same question once unverified rows existed.

/// The verified width-safe names, in catalog order — the *extension* of
/// [`width_safe`]. The playground boundary widget uses this so its displayed
/// subset cannot drift from the folding gate (issue #64).
#[must_use]
pub fn width_safe_names() -> &'static [&'static str] {
    WIDTH_SAFE
}

/// The refused names, in catalog order — [`WidthClass::Refused`] rows: folds a
/// 32-bit engine loses **because a divergence is on record**. See
/// `WIDTH_REFUSED` for each refusal's probe evidence.
#[must_use]
pub fn width_refused_names() -> &'static [&'static str] {
    WIDTH_REFUSED
}

/// The unverified names, in catalog order — [`WidthClass::Unverified`] rows,
/// sibling to [`width_refused_names`]. See `WIDTH_UNVERIFIED` for what
/// "unverified" commits the catalog to (deliberately nothing).
#[must_use]
pub fn width_unverified_names() -> &'static [&'static str] {
    WIDTH_UNVERIFIED
}

/// The verified width-safe half of the folding allowlist (issue #64), grouped
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
/// `array_unique` carries one exposure worth naming: its default `SORT_STRING`
/// compares string casts, so `precision` decides how many elements survive.
/// That is the same ini `strval` and `implode` already fold under (both engines
/// report `precision = 14`), escalated from *how a float is spelled* to *how
/// long the array is*. Closing that seam is a decision about all three names at
/// once, not about this one.
const WIDTH_SAFE: &[&str] = &[
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
];

/// The **refused rows** of the width classification, with the divergence that
/// refused each (ADR-0061 refused-row discipline applied to the integer
/// machine). Every probe passes the range guard; only refusing the name can
/// exclude it. Each row is a *silent* divergence — nothing throws, widens, or
/// warns (ADR-0066 §4). Probes: 64-bit `php` 8.5.8 vs 32-bit php-wasm 0.1.0.
///
/// * `abs` — **type tag** flips: a numeric string coerces to `int|float` by
///   the engine's own width (`abs("3000000000")` = `int` / `float`).
/// * `intval` — saturation and wraparound by definition of the cast (10/17
///   probes diverged — the width-sensitive builtin).
/// * `sprintf` — `%b`/`%x`/`%o`/`%u` render the machine word from an in-range
///   argument; `%d` re-imports `intval`'s saturation. Format-aware
///   sub-classification is deliberately not attempted (the line would live
///   inside a string literal).
/// * `dechex`/`decbin`/`decoct` — render the machine word for a negative,
///   in-range argument.
/// * `bindec`/`hexdec` — type tag flips at the width boundary from a plain
///   string argument.
/// * `version_compare` — the *surprise* of issue #78: php-src compares each
///   numeric run through a C `long`, so two oversized runs both saturate and
///   compare **equal** on 32-bit. The three-argument (bool) form is refused
///   with it.
/// * `range` — the *surprise* of issue #354, and the only one of that slice's
///   five: its bounds are declared `string|int|float`, so an oversized numeric
///   string is typed by the engine's own width and the **element type tag**
///   flips inside the result. `range("3000000000", "3000000000")` is
///   `[int(3000000000)]` / `[float(3000000000.0)]`, and the flip starts one
///   past the narrow `PHP_INT_MAX` (`"2147483648"`). Reachable from strings
///   only — 52 probes found no int- or float-argument route.
/// * `preg_split` — refused on a divergence that is **not** the integer width,
///   the one row here where that is true. The two builds differ in their PCRE:
///   64-bit `php` has `pcre.jit = 1`, php-wasm 0.1.0 has none, and PCRE2's JIT
///   does not honour the inline limit verbs the interpreter does.
///   `preg_split('/(*LIMIT_MATCH=1)a/', "aaa")` splits on one engine and is
///   `false` on the other; adding `(*NO_JIT)` makes them agree, which is what
///   identifies the cause. `LIMIT_RECURSION`/`LIMIT_DEPTH` repeat it. Refusing
///   the narrow engine also keeps this seam and ADR-0078's pattern lane
///   (`preg_refusal_memo`) from answering the same question separately in the
///   browser; on a native run both ride the project's own PCRE, where a folded
///   `false` is that engine's own answer and the lane's diagnostic is about the
///   pattern, not the value.
const WIDTH_REFUSED: &[&str] = &[
    "abs",
    "intval",
    "sprintf",
    // issue #78 — machine-word rendering and its inverse
    "dechex",
    "decbin",
    "decoct",
    "bindec",
    "hexdec",
    // issue #78 — a `long` hiding inside string work
    "version_compare",
    // issue #354 — a width-typed numeric string, and a PCRE build option
    "range",
    "preg_split",
];

/// The **unverified rows** of the width classification (ADR-0028's 2026-08-14
/// amendment §4, issue #330) — the third class, which claims nothing. Unlike
/// `WIDTH_SAFE`/`WIDTH_REFUSED`, the correct probe count behind a row here is
/// **zero**: not measured, so the name folds only on a provably 64-bit engine.
/// A probe finding agreement moves a row to `WIDTH_SAFE`, a divergence to
/// `WIDTH_REFUSED`.
///
/// Both names are admitted by the amendment's §5 strictly-stronger rule (their
/// Rust rungs are type-level only) rather than re-derived in Rust per ADR-0004:
///
/// * `explode` — rung is `non-empty-list<string>` (`explode_transfer`). Not
///   reimplemented since an empty `$separator` is a `ValueError` since PHP 8.0
///   and a negative `$limit` can empty the result outright, so the rung
///   declines the three-argument form.
/// * `array_merge` — no value rung at all: duplicate **string** keys resolve
///   last-wins and integer keys **renumber** from zero, so result keys are a
///   function of the engine's own construction, not the arguments as written.
///
/// The five names this class deferred — `range`, `preg_split`, `str_split`,
/// `array_unique`, `array_fill` — were probed in issue #354 and left it: three
/// to `WIDTH_SAFE`, two to `WIDTH_REFUSED`. Nothing was promoted *into* here,
/// which is the class working as defined: a probe moves a row out, and a row
/// only enters by being admitted unmeasured.
const WIDTH_UNVERIFIED: &[&str] = &["array_merge", "explode"];

/// The effect labels (ADR-0018 hierarchical dot-paths) a builtin carries, or
/// `None` when **uncatalogued** (unknown effects, ADR-0005): `Some(&[])` is
/// catalogued-pure ([`foldable`] builtins), `Some(&[label, …])` is a proven
/// `effect.envelope-exceeded` violation from `Pure`, `None` is no finding.
///
/// Matching is case-insensitive. Labels follow ADR-0018's taxonomy;
/// argument-dependent effects use the safe, argument-insensitive upper bound
/// (ADR-0021):
///
/// * Every **wrapper-capable** stream API (every filesystem row here) is
///   colored `io`, the parent of every channel a registered wrapper can reach
///   (issue #318): `file_get_contents('https://…')` is a network read, not a
///   filesystem one. A call site that *proves* its target narrows back down;
///   see [`narrowed_stream_labels`]. `session_start` is the one composite
///   exception.
/// * `print_r`/`var_export`/`var_dump` are `io.output.buffer` even though the
///   first two are pure in return-mode — the arg-blind safe choice.
/// * `sleep`/`usleep` are `io`: an observable timing side effect.
/// * `curl_exec` keeps `io.output` arg-blind (only `CURLOPT_RETURNTRANSFER`
///   suppresses it); `system`/`passthru` take parent `io.output` since
///   OB-capturability evidence for a relayed child's output is split
///   (ADR-0083 over-approximates toward unmaskable).
///
/// `exit`/`die` are **language constructs**, never reach this table; the
/// effects pass detects them structurally (ADR-0019 rule 4).
#[must_use]
pub fn effect_labels(name: &str) -> Option<&'static [&'static str]> {
    const EMPTY: &[&str] = &[];
    const NONDET_RANDOM: &[&str] = &["nondet.random"];
    const NONDET_TIME: &[&str] = &["nondet.time"];
    const IO_OUTPUT_BUFFER: &[&str] = &["io.output.buffer"];
    const IO: &[&str] = &["io"];
    const GLOBAL_WRITE: &[&str] = &["global.write"];
    const GLOBAL_READ: &[&str] = &["global.read"];
    const IO_SIGNAL: &[&str] = &["io.signal"];
    const IO_OUTPUT_HEADER: &[&str] = &["io.output.header"];
    const IO_IPC: &[&str] = &["io.ipc"];
    // `session_start` is composite (effects_gaps.md): file handler
    // (`io.fs.write`), `Set-Cookie` header (`io.output.header`), `$_SESSION`/ini
    // mutation (`global.write`).
    const SESSION: &[&str] = &["io.fs.write", "io.output.header", "global.write"];
    // Runs a child process and relays its output; unsettled OB-capturability
    // keeps parent `io.output` rather than `.buffer` (ADR-0083).
    const PROCESS_TO_OUTPUT: &[&str] = &["io.process", "io.output"];
    // `curl_exec`: response body to output unless `CURLOPT_RETURNTRANSFER`.
    const NET_TO_OUTPUT: &[&str] = &["io.net", "io.output"];

    let colored: Option<&'static [&'static str]> = match name.to_ascii_lowercase().as_str() {
        "rand" | "mt_rand" | "random_int" | "random_bytes" | "uniqid" | "shuffle" => {
            Some(NONDET_RANDOM)
        }
        "time" | "microtime" | "hrtime" | "date" | "mktime" => Some(NONDET_TIME),
        // The **wrapper-capable** family (issue #318): every filesystem row.
        // Each reaches whatever the stream layer resolves its target to, so the
        // argument-blind row can only be the `io` parent (a stricter row would
        // hide a network read under `io.fs.read`). [`narrowed_stream_labels`]
        // gives back the precise label once a call site proves its target.
        "file_get_contents" | "file_put_contents" | "fopen" | "copy" | "rename" | "readfile"
        | "fpassthru" | "fread" | "fgets" | "fwrite" | "fputs" | "unlink" | "mkdir" | "rmdir"
        | "touch" | "scandir" | "file_exists" | "is_file" | "is_dir" => Some(IO),
        "print_r" | "var_dump" | "var_export" | "printf" | "vprintf" | "flush" | "ob_flush" => {
            Some(IO_OUTPUT_BUFFER)
        }
        // Shell out and relay the child's output (ADR-0083).
        "system" | "passthru" => Some(PROCESS_TO_OUTPUT),
        "curl_exec" => Some(NET_TO_OUTPUT),
        "error_log" | "syslog" | "sleep" | "usleep" => Some(IO),
        "date_default_timezone_set" | "mb_regex_encoding" | "setlocale" | "ini_set" | "putenv" => {
            Some(GLOBAL_WRITE)
        }
        // Process-global state, no channel: seeding pair replaces RNG state;
        // `clearstatcache` empties the stat cache. Drawing stays `nondet.random`.
        "srand" | "mt_srand" | "clearstatcache" => Some(GLOBAL_WRITE),
        "getenv" | "ini_get" | "date_default_timezone_get" => Some(GLOBAL_READ),
        // Signal delivery/handling (effects_gaps.md §1); pcntl/posix functions.
        "pcntl_signal" | "pcntl_signal_dispatch" | "pcntl_alarm" | "pcntl_async_signals"
        | "pcntl_sigprocmask" | "pcntl_sigwaitinfo" | "posix_kill" => Some(IO_SIGNAL),
        // HTTP response-header mutation (effects_gaps.md §2).
        "header" | "header_remove" | "setcookie" | "setrawcookie" | "http_response_code" => {
            Some(IO_OUTPUT_HEADER)
        }
        // System-V / shared-memory IPC (effects_gaps.md §4).
        "shmop_write" | "shmop_read" | "sem_acquire" | "sem_release" | "msg_send"
        | "msg_receive" => Some(IO_IPC),
        "session_start" => Some(SESSION),
        _ => None,
    };

    colored.or_else(|| foldable(name).then_some(EMPTY))
}

/// A call argument a **call site** proved constant (issue #318) — the evidence
/// [`narrowed_stream_labels`] narrows a wrapper-capable row on. Both forms are
/// *syntactic* proof, never dataflow: a variable or interpolated string is no
/// target, so the caller keeps the `io` default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamTarget<'a> {
    /// A quoted string literal with no interpolation, by its decoded value.
    Literal(&'a str),
    /// A bare constant fetch, by its unqualified spelling (`STDOUT`, `STDERR`,
    /// `STDIN`) — the only open-stream *resource* spelling a structural scan
    /// can read.
    Constant(&'a str),
}

/// The **narrowed** effect labels a wrapper-capable stream call earns at a call
/// site that proves its target (issue #318), or `None` — the caller then keeps
/// [`effect_labels`]' sound `io` default. Costs no precision on ordinary code:
/// a constant target reaches exactly one channel
/// (`file_get_contents('/etc/hosts')` is `io.fs.read`, `file_get_contents('https://…')`
/// is `io.net.http`).
///
/// `first`/`second` are the call's first two positional arguments in
/// proven-constant form; the second's meaning is the row's business (`fopen`'s
/// mode string, `copy`/`rename`'s destination). Each target reads through
/// **its own role's** direction: `copy('/a', '/b')` earns
/// `["io.fs.read", "io.fs.write"]`; `rename` writes on both sides.
///
/// # The scheme table
///
/// | target | narrowed to |
/// | --- | --- |
/// | no scheme (a plain path), `file://`, `zlib://`, `phar://`, `glob://`, `compress.*://`, `php://temp` | that target's own `io.fs.*` direction (`fopen` composes it from a literal mode) |
/// | `http://`, `https://` | `io.net.http` |
/// | `ftp://`, `ftps://`, `ssh2.*://`, `tcp://`, `udp://`, `ssl://`, `tls://` | `io.net` |
/// | `unix://`, `udg://` | `io.ipc` |
/// | `expect://` | `io.process` |
/// | `php://output` | `io.output.buffer` |
/// | `php://stdout` / `php://stderr` | `io.output.stdout` / `io.output.stderr` |
/// | `php://input` / `php://stdin` | `io.input` |
/// | `php://memory`, `data://` | `mutate.local` |
/// | `php://filter/…/resource=<target>` | the trailing target, resolved **one** step |
/// | `STDIN` / `STDOUT` / `STDERR` (a resource row) | `io.input` / `io.output.stdout` / `io.output.stderr` |
/// | anything else (`php://fd/3`, an unknown or userland scheme) | `None` — the `io` default stands |
///
/// A `php://` special stream names a *channel*, not a call direction (a write
/// and a hypothetical read of the same target both color `io.output.stdout`);
/// the stat-and-unlink rows decline the whole `php://` column since they open
/// no stream.
///
/// # What it declines
///
/// A userland wrapper is an unknown scheme → `None` (ruling D-W1; nothing here
/// reads the registration); `copy`/`rename` need **both** sides constant (an
/// unprovable side's `io` default unions to `io` — no narrowing); a `php://`
/// target on a stat-and-unlink row, same reason as above; and a form mismatch
/// (a path row handed a constant, a resource row handed a string literal).
#[must_use]
pub fn narrowed_stream_labels(
    name: &str,
    first: Option<StreamTarget<'_>>,
    second: Option<StreamTarget<'_>>,
) -> Option<Vec<&'static str>> {
    // The target leads: a call with no constant first argument (the common
    // case) answers before paying for a lowercase copy of the name.
    let first = first?;
    let row = stream_row(&name.to_ascii_lowercase())?;
    let mut labels = target_labels(row, row.direction, first, second)?;
    // A second target narrows through **its own** role's direction: `copy`
    // reads its source and writes its destination, so both sides can differ.
    if let SecondArg::Target(direction) = row.second {
        for label in target_labels(row, direction, second?, None)? {
            if !labels.contains(&label) {
                labels.push(label);
            }
        }
    }
    // The read-and-relay pair: narrowing restores the output component the `io`
    // default folded away (`ob_start()` + `readfile()` is a documented capture
    // pattern, ADR-0083).
    if row.relays_to_output && !labels.contains(&"io.output.buffer") {
        labels.push("io.output.buffer");
    }
    Some(labels)
}

/// What a proven target *means* for the wrapper-capable function that takes it —
/// one row of [`narrowed_stream_labels`]' table.
#[derive(Debug, Clone, Copy)]
struct StreamRow {
    /// The form argument 0 must have for this row to narrow at all.
    form: TargetForm,
    /// The `io.fs.*` label argument 0 earns when its target has no scheme (or a
    /// filesystem-family one).
    direction: FsDirection,
    /// What argument 1 is.
    second: SecondArg,
    /// Whether the call also relays what it moves to the output channel
    /// (`readfile`, `fpassthru`).
    relays_to_output: bool,
    /// Whether a `php://` pseudo-stream is a meaningful target for this row. The
    /// stat-and-unlink family opens no stream, so those rows decline it.
    php_streams: bool,
}

/// Which argument form carries the stream target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetForm {
    /// A path or URL string: `file_get_contents($path)`, `fopen($path, $mode)`.
    Path,
    /// An already-open stream resource, provable only as one of PHP's three
    /// predefined CLI constants: `fwrite($handle, …)`.
    Resource,
}

/// The filesystem direction one target of a row takes when it is an ordinary
/// file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FsDirection {
    Read,
    Write,
    /// `fopen`: composed from the mode string when that is a literal too, and
    /// the parent `io.fs` when it is not.
    FromMode,
}

/// What argument 1 of a row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SecondArg {
    /// Nothing this table reads: a length, a flags int, a sort order, a mtime.
    Ignored,
    /// `fopen`'s mode string, which composes argument 0's direction.
    Mode,
    /// A **second target**, narrowed through the direction its own role names —
    /// `copy($from, $to)` reads the first and writes the second.
    Target(FsDirection),
}

/// The [`StreamRow`] for a wrapper-capable builtin, `None` for every other name.
fn stream_row(name_lc: &str) -> Option<StreamRow> {
    use FsDirection::{FromMode, Read, Write};
    use TargetForm::{Path, Resource};
    let row = |form, direction, second, relays_to_output, php_streams| StreamRow {
        form,
        direction,
        second,
        relays_to_output,
        php_streams,
    };
    let simple = |form, direction| row(form, direction, SecondArg::Ignored, false, true);
    match name_lc {
        "file_get_contents" => Some(simple(Path, Read)),
        "file_put_contents" => Some(simple(Path, Write)),
        // Reads a path and relays it to the output channel.
        "readfile" => Some(row(Path, Read, SecondArg::Ignored, true, true)),
        // `fopen($path, $mode)` — the one row whose second argument is a mode.
        "fopen" => Some(row(Path, FromMode, SecondArg::Mode, false, true)),
        // `copy($from, $to)` reads the source and writes the destination — a
        // proven pair earns both labels, which no single-direction union could.
        "copy" => Some(row(Path, Read, SecondArg::Target(Write), false, true)),
        // `rename` moves a directory entry: both sides are metadata writes.
        "rename" => Some(row(Path, Write, SecondArg::Target(Write), false, true)),
        "fread" | "fgets" => Some(simple(Resource, Read)),
        "fwrite" | "fputs" => Some(simple(Resource, Write)),
        // Reads a resource and relays it to the output channel.
        "fpassthru" => Some(row(Resource, Read, SecondArg::Ignored, true, true)),
        // Stat-and-unlink family: wrapper-capable too (`unlink`/`mkdir` go over
        // `ssh2.sftp://`), but open no stream, so `php://` targets don't apply.
        "unlink" | "mkdir" | "rmdir" | "touch" => {
            Some(row(Path, Write, SecondArg::Ignored, false, false))
        }
        "scandir" | "file_exists" | "is_file" | "is_dir" => {
            Some(row(Path, Read, SecondArg::Ignored, false, false))
        }
        _ => None,
    }
}

/// The labels one proven target earns under `row`, read through `direction` —
/// which is the row's own for argument 0 and the second target's role for
/// argument 1. `mode` is argument 1 where a [`FsDirection::FromMode`] target
/// reads it.
fn target_labels(
    row: StreamRow,
    direction: FsDirection,
    target: StreamTarget<'_>,
    mode: Option<StreamTarget<'_>>,
) -> Option<Vec<&'static str>> {
    match (row.form, target) {
        (TargetForm::Path, StreamTarget::Literal(s)) => path_labels(s, row, direction, mode, true),
        (TargetForm::Resource, StreamTarget::Constant(c)) => constant_labels(c),
        _ => None,
    }
}

/// The channel one of PHP's three predefined stream constants names. Matched
/// case-**sensitively**: PHP constant names are.
fn constant_labels(name: &str) -> Option<Vec<&'static str>> {
    match name {
        "STDIN" => Some(vec!["io.input"]),
        "STDOUT" => Some(vec!["io.output.stdout"]),
        "STDERR" => Some(vec!["io.output.stderr"]),
        _ => None,
    }
}

/// The labels a literal path or URL earns under `row`. `allow_filter` is the
/// one-step recursion budget `php://filter/…/resource=` spends: a filter naming
/// another filter proves nothing and stops at `None`.
fn path_labels(
    target: &str,
    row: StreamRow,
    direction: FsDirection,
    mode: Option<StreamTarget<'_>>,
    allow_filter: bool,
) -> Option<Vec<&'static str>> {
    let Some(scheme) = scheme_of(target) else {
        return Some(fs_labels(direction, mode));
    };
    let scheme = scheme.to_ascii_lowercase();
    match scheme.as_str() {
        "http" | "https" => Some(vec!["io.net.http"]),
        "ftp" | "ftps" | "tcp" | "udp" | "ssl" | "tls" => Some(vec!["io.net"]),
        // Filesystem (`unix://`) / abstract (`udg://`) domain sockets are
        // cross-process state: `io.ipc`, NOT `io.net`.
        "unix" | "udg" => Some(vec!["io.ipc"]),
        "expect" => Some(vec!["io.process"]),
        // A `data:` URI is its own content — nothing read from anywhere.
        "data" => Some(vec!["mutate.local"]),
        "file" | "zlib" | "phar" | "glob" => Some(fs_labels(direction, mode)),
        "php" => php_labels(target, row, direction, mode, allow_filter),
        _ if scheme.starts_with("ssh2.") => Some(vec!["io.net"]),
        _ if scheme.starts_with("compress.") => Some(fs_labels(direction, mode)),
        // Unknown scheme, registered userland ones included (D-W1): no narrowing.
        _ => None,
    }
}

/// The labels a `php://` pseudo-stream earns. `target` is the whole literal, so
/// the `resource=` tail keeps its own casing for the recursion.
fn php_labels(
    target: &str,
    row: StreamRow,
    direction: FsDirection,
    mode: Option<StreamTarget<'_>>,
    allow_filter: bool,
) -> Option<Vec<&'static str>> {
    if !row.php_streams {
        return None;
    }
    let rest = target.get("php://".len()..)?;
    let rest_lc = rest.to_ascii_lowercase();
    match rest_lc.as_str() {
        "output" => return Some(vec!["io.output.buffer"]),
        "stdout" => return Some(vec!["io.output.stdout"]),
        "stderr" => return Some(vec!["io.output.stderr"]),
        // Two spellings of the script's inbound stream (ADR-0083).
        "input" | "stdin" => return Some(vec!["io.input"]),
        "memory" => return Some(vec!["mutate.local"]),
        _ => {}
    }
    // `php://temp` spills to a temporary file past its memory threshold.
    if rest_lc == "temp" || rest_lc.starts_with("temp/") {
        return Some(fs_labels(direction, mode));
    }
    if rest_lc.starts_with("filter/") {
        if !allow_filter {
            return None;
        }
        // php-src reads the filter spec up to the first `/resource=` and takes
        // everything after it as the stream actually opened; the filters
        // themselves are transforms, not channels.
        let inner = rest.split_once("/resource=")?.1;
        return path_labels(inner, row, direction, mode, false);
    }
    // `php://fd/3` and anything else: the target is a number this table cannot
    // resolve to a channel.
    None
}

/// The filesystem label a target earns, in the direction its role names, when it
/// is an ordinary file.
fn fs_labels(direction: FsDirection, mode: Option<StreamTarget<'_>>) -> Vec<&'static str> {
    match direction {
        FsDirection::Read => vec!["io.fs.read"],
        FsDirection::Write => vec!["io.fs.write"],
        FsDirection::FromMode => match mode {
            Some(StreamTarget::Literal(m)) => mode_labels(m),
            // An unprovable mode leaves the direction unknown — the parent
            // `io.fs`, which is exactly what the row said before issue #318.
            _ => vec!["io.fs"],
        },
    }
}

/// `fopen`'s mode string, read for its direction: `r` reads, `w`/`a`/`x`/`c`
/// write, and a `+` anywhere opens both, which is the parent `io.fs`. The
/// `b`/`t`/`e` suffixes decide line endings and `close-on-exec`, not direction.
/// Modes are lowercase in PHP; anything else is not a mode and stays `io.fs`.
fn mode_labels(mode: &str) -> Vec<&'static str> {
    if mode.contains('+') {
        return vec!["io.fs"];
    }
    match mode.as_bytes().first() {
        Some(b'r') => vec!["io.fs.read"],
        Some(b'w' | b'a' | b'x' | b'c') => vec!["io.fs.write"],
        _ => vec!["io.fs"],
    }
}

/// The wrapper scheme of a target string — the `scheme` of `scheme://rest` —
/// `None` when the string is a plain path.
///
/// Deliberately strict about the shape: the scheme must be an RFC-3986-flavored
/// name (ASCII alphanumerics plus `+`, `-`, `.`, first character a letter), so a
/// path that merely *contains* `://` (`/var/log/http://weird`) is a path, and a
/// Windows drive letter (`C:\dir`) never looks like a scheme at all.
fn scheme_of(target: &str) -> Option<&str> {
    let (scheme, _) = target.split_once("://")?;
    let mut chars = scheme.chars();
    if !chars.next()?.is_ascii_alphabetic() {
        return None;
    }
    chars
        .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.')
        .then_some(scheme)
}

/// **Method-shaped effect rows**: the effect labels of a call to `method` on an
/// instance of the *builtin* class `class`, or `None` for uncatalogued. The
/// class-world twin of [`effect_labels`], same three-valued contract. Both
/// keys match case-insensitively, so `new pdo(...)->QUERY()` is `PDO::query`.
///
/// The key is the **global** class name, no namespace — a consumer must
/// resolve the receiver to an FQN first, so a namespaced `App\PDO` never
/// collides with the engine's `PDO`; a project-defined class shadows this
/// table entirely.
///
/// # Membership (issue #67)
///
/// Rows cover `PDO`/`PDOStatement` with coarse label `io.db`. Runtime
/// configuration controls whether emulated `prepare` contacts the server, so
/// `prepare` takes the argument-insensitive upper bound.
#[must_use]
pub fn method_effect_labels(class: &str, method: &str) -> Option<&'static [&'static str]> {
    const IO_DB: &[&str] = &["io.db"];

    match (class.to_ascii_lowercase().as_str(), method.to_ascii_lowercase().as_str()) {
        ("pdo", "query" | "exec" | "prepare") => Some(IO_DB),
        ("pdostatement", "execute" | "fetch" | "fetchall") => Some(IO_DB),
        _ => None,
    }
}

/// The **by-ref out-parameter rows** (ADR-0063 §2.3): 0-based positional
/// indices a builtin writes through a reference parameter.
///
/// Call-dependent, unlike unconditional [`effect_labels`]: `preg_match($p,
/// $s)` writes nothing, `preg_match($p, $s, $m)` writes `$m`. A position
/// contributes only if the call supplies it (arity leg); what it contributes
/// depends on argument `p`'s *lvalue root* (target leg: a calling-frame
/// binding earns `mutate.local`, a superglobal earns `global.write`, anything
/// else earns the conservative parent `mutate`). A builtin may carry both an
/// unconditional color and an out-param row (`shuffle` is `nondet.random`
/// *and* writes argument 0).
///
/// Rows are transcribed from the php-src stubs at `PINNED_PHP`, restricted to
/// **fixed positional** reference parameters — the variadic-by-ref family
/// (`sscanf`, `fscanf`, `array_multisort`) and `extract()` (writes the symbol
/// *table*, the ADR-0046 world) are deliberately absent: silence beats a wrong
/// color.
#[must_use]
pub fn out_params(name: &str) -> Option<&'static [usize]> {
    const P0: &[usize] = &[0];
    const P2: &[usize] = &[2];
    const P3: &[usize] = &[3];
    const P4: &[usize] = &[4];

    match name.to_ascii_lowercase().as_str() {
        // Array sort/rearrangement/stack-and-queue: argument 0, always by-ref.
        // `usort`/`uasort`/`uksort`/`array_walk` also compose with
        // `invocation_shape` as callback invokers.
        "sort" | "rsort" | "asort" | "arsort" | "ksort" | "krsort" | "usort" | "uasort"
        | "uksort" | "natsort" | "natcasesort" | "shuffle" | "array_splice" | "array_push"
        | "array_pop" | "array_shift" | "array_unshift" | "array_walk"
        | "array_walk_recursive" => Some(P0),
        // Internal array-pointer moves: `array|object &$array` in the stubs.
        "reset" | "end" | "next" | "prev" => Some(P0),
        "settype" => Some(P0),
        // `preg_match(..., array &$matches = null, …)` — the ADR's headline
        // case: optional, so the arity leg does real work.
        "preg_match" | "preg_match_all" => Some(P2),
        "similar_text" => Some(P2),
        "str_replace" | "str_ireplace" => Some(P3),
        "preg_replace_callback_array" => Some(P3),
        // `$count` is position **4**, not 3: the optional `$limit` sits between
        // subject and count.
        "preg_replace" | "preg_replace_callback" => Some(P4),
        _ => None,
    }
}

/// **When** a by-ref out-parameter write is proven to have happened (ADR-0077
/// §3.2) — the *written-when* witness an [`out_params`] row may carry.
///
/// Conditional on the callee's contract: `preg_match` measures (PHP 8.5.9) as
/// three outcomes, only two of which write (`1` the success shape, `0` `[]`,
/// a PCRE compile failure `false` and writes **nothing**), which is why the
/// witness names a *return value* rather than an unconditional write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WrittenWhen {
    /// The write happened on exactly the paths where the call's return value is
    /// **truthy**. Every falsy return — including one that means "the callee
    /// refused its inputs" — proves nothing about the argument.
    ReturnTruthy,
}

/// The *written-when* witness for position `position` of `name`, or `None`
/// when the catalog states none (ADR-0077 §3.2). `None` means "no seed" for
/// every position but the two below: a row is added only once the callee's
/// contract is read *and* measured — a wrong witness would manufacture a fact
/// on a path the callee never wrote.
///
/// `preg_match_all` position 2 (issue #168) — measured (PHP 8.5.9): `int >= 1`
/// on the truthy branch, `0` still writes empty columns, `false` writes
/// nothing; the zero-match write is indistinguishable from `false` on the
/// falsy branch, so that side stays unseeded.
///
/// Every other [`out_params`] row's contract deserves the same treatment but
/// stays a decline until measured (ADR-0077 §4). A witness is not by itself a
/// fact: it says *where* a seed would be sound.
#[must_use]
pub fn out_param_written_when(name: &str, position: usize) -> Option<WrittenWhen> {
    match (name.to_ascii_lowercase().as_str(), position) {
        ("preg_match", 2) => Some(WrittenWhen::ReturnTruthy),
        ("preg_match_all", 2) => Some(WrittenWhen::ReturnTruthy),
        _ => None,
    }
}

/// Whether argument `position` of the builtin `name` is passed **by value**
/// (ADR-0070), three-valued: `Some(true)` certified by value (PHP copies into
/// the parameter), `Some(false)` certified **by-reference** (aliases the
/// caller's lvalue), `None` unknown (consumer assumes the worst).
///
/// An [`out_params`] row lists every fixed positional reference parameter for
/// a name, so every other position is by value — one table answers both
/// `true` for `preg_match`'s `$s` and `false` for its `$m`.
///
/// Absence of a row is **not** a by-value statement, so a rowless name must be
/// *positively certified* below — every parameter declared by value in the
/// `PINNED_PHP` stub (everything else answers `None`). The set covers:
///
/// * the folding allowlist ([`foldable`]), pure by construction;
/// * the ADR-0062/0064 array read-position/shape-projection family lacking an
///   out-param row (`array_first`, `array_values`, …; `current`/`key` are
///   by-value, their pointer-moving siblings `reset`/`end`/`next`/`prev` are
///   rowed);
/// * alias spellings of foldable names (`chop`, `join`, `sizeof`);
/// * the **string-producer family's non-foldable members** (issue #41):
///   `addcslashes`, `escapeshellarg`, `escapeshellcmd`, `htmlspecialchars`,
///   `htmlentities`, `vsprintf` — leaving these uncertified was measured as
///   the wave's dominant precision loss (an uncertified name also drops the
///   declared-arm lane, silencing ~70 later assertions in one phpstan-src
///   fixture);
/// * the **`mb_*` string family** (issue #41): excluded from [`foldable`] for
///   its encoding-dependent *result*, but all-by-value in its *arguments* —
///   independent questions that cost the same ~70 assertions when conflated.
///
/// Widening this set is a separate, measured act: every added name is a new
/// premise for every kept fact downstream.
#[must_use]
pub fn by_value_arg(name: &str, position: usize) -> Option<bool> {
    /// Certified all-by-value names outside the folding allowlist, each
    /// transcribed from the `PINNED_PHP` stub. See the membership rules above.
    const CERTIFIED_EXTRA: &[&str] = &[
        "chop",     // = rtrim
        "join",     // = implode
        "sizeof",   // = count
        "array_first",
        "array_last",
        "array_key_first",
        "array_key_last",
        // By value since PHP 8.0; `&$array` siblings are rowed in `out_params`.
        "current",
        "key",
        // Shape-projection family (ADR-0062): array by value, returns new array.
        "array_values",
        "array_keys",
        "array_flip",
        "array_reverse",
        // Sibling `array_splice` takes `&$array` and has an `out_params` row.
        "array_slice",
        // String-producer family's non-foldable members (issue #41).
        // `escapeshellcmd` is here despite the transfer table refusing its
        // RESULT — that says nothing about ARGUMENT reachability.
        "addcslashes",
        "escapeshellarg",
        "escapeshellcmd",
        "htmlspecialchars",
        "htmlentities",
        "vsprintf",
        // `mb_*` family (issue #41): encoding-dependent RESULT excludes it from
        // `foldable`, but every ARGUMENT is by value. `mb_internal_encoding` is
        // deliberately ABSENT — it writes process-global state.
        "mb_strtolower",
        "mb_strtoupper",
        "mb_substr",
        "mb_strlen",
        "mb_strwidth",
        "mb_convert_case",
        "mb_convert_kana",
        "mb_str_split",
        "mb_str_pad",
        "mb_strpos",
        "mb_substr_count",
        "mb_convert_encoding",
        "mb_check_encoding",
        "mb_detect_encoding",
        "mb_ucfirst",
        "mb_lcfirst",
        "mb_trim",
        "mb_ltrim",
        "mb_rtrim",
    ];
    match out_params(name) {
        Some(positions) => Some(!positions.contains(&position)),
        None => {
            let certified = foldable(name)
                || CERTIFIED_EXTRA.iter().any(|&f| name.eq_ignore_ascii_case(f));
            certified.then_some(true)
        }
    }
}

/// The hierarchical **label registry** (ADR-0018): the set of known effect
/// labels. A declared envelope label outside this set (and not an ancestor of
/// any entry — see [`is_known_label`]) earns an `effect.unknown-label`
/// diagnostic.
///
/// The union of every label [`effect_labels`] can color a builtin with and the
/// core taxonomy roots/parents of ADR-0018. Ecosystem/private labels
/// (`io.redis`, `email.send`) are **not** here — a plugin opens the registry
/// beside it; see [`LabelRegistry`], what inference actually asks.
#[must_use]
pub fn known_labels() -> &'static [&'static str] {
    BUILTIN_LABELS
}

/// The **core taxonomy roots** of ADR-0018 — the label roots Steins itself owns.
///
/// A plugin may register *descendants* of these (`io.redis`, `io.db.dynamo`),
/// so subsumption works with no new machinery; a **new root** must instead
/// equal the plugin's composer vendor name (ADR-0068 §2), which is what the
/// vendor-root rule checks against this list.
///
/// `global` is a root even though only `global.read`/`global.write` are
/// registry entries — root ownership applies to the namespace.
#[must_use]
pub fn core_roots() -> &'static [&'static str] {
    &["exit", "failure", "ffi", "global", "io", "mutate", "nondet"]
}

/// Whether `label` lies under some [`core_roots`] entry — equal to a root, or a
/// dot-path descendant of one. The ADR-0068 §2 predicate a plugin registration
/// passes when it refines Steins' own taxonomy instead of opening a new root.
#[must_use]
pub fn is_core_label(label: &str) -> bool {
    core_roots().iter().any(|&r| r == label || subsumes(r, label))
}

/// The builtin label table [`known_labels`] returns, shared with [`LabelRegistry`]
/// so the builtin-only and extended views cannot drift.
const BUILTIN_LABELS: &[&str] = {
    // Sorted; ADR-0018's taxonomy plus every label `effect_labels` uses.
    &[
        "exit",
        // Failure-cause provenance family (ADR-0042): labels value provenance,
        // not an effect. See [`failure_arms`].
        "failure",
        "failure.environment",
        "failure.input",
        "failure.resource",
        // Opaque native boundary (FFI, effects_gaps.md §3): OO-only.
        "ffi",
        "global.read",
        "global.write",
        "io",
        "io.db",
        "io.fs",
        "io.fs.read",
        "io.fs.write",
        // Ambient input channel (ADR-0083, issue #318), from
        // [`narrowed_stream_labels`]; `$_GET` reads stay `global.read`.
        "io.input",
        "io.ipc", // System-V / shared-memory IPC (effects_gaps.md §4).
        "io.net",
        "io.net.http",
        // Ambient output channel (ADR-0083); children split on `ob_start()` capture.
        "io.output",
        "io.output.buffer", // OB-layer output — the only `ob_start()`-deductible.
        "io.output.header", // HTTP header mutation (effects_gaps.md §2), outside OB.
        "io.output.stderr", // Process-fd writes, which OB cannot touch.
        "io.output.stdout",
        "io.process",
        "io.signal", // Signal delivery/handling (pcntl/posix; effects_gaps.md §1).
        "mutate",
        // By-ref out-parameter write into the calling frame's own binding
        // (ADR-0063 §2.3); non-local targets stop at parent `mutate` (ADR-0055 §1).
        "mutate.local",
        "nondet",
        "nondet.random",
        "nondet.time",
    ]
};

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
/// for labels finer than the registry taxonomy — `io.netw` is neither a node nor
/// an ancestor of one, so it stays unknown (→ `effect.unknown-label`), while
/// every registry root is accepted.
#[must_use]
pub fn is_known_label(label: &str) -> bool {
    known_labels().iter().any(|&k| admits(label, k))
}

/// The registry label nearest to an unknown `label`, for a typo suggestion
/// (`io.netw` → `io.net`). Returns `None` when nothing is close. The metric is a
/// simple Levenshtein distance capped so only genuinely near names suggest.
#[must_use]
pub fn nearest_label(label: &str) -> Option<&'static str> {
    nearest_of(label, known_labels().iter().copied())
}

/// Whether a registry entry `entry` makes declared `label` known: `label` is the
/// entry itself, or an ancestor path of it. The one rule [`is_known_label`] and
/// [`LabelRegistry::is_known`] share, so the builtin-only and extended views
/// cannot answer differently for the same entry.
fn admits(label: &str, entry: &str) -> bool {
    entry == label || subsumes(label, entry)
}

/// The nearest of `entries` to `label` under the capped Levenshtein metric.
fn nearest_of<'a>(label: &str, entries: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    entries
        .map(|k| (levenshtein(label, k), k))
        .filter(|&(d, _)| d <= 2)
        .min_by_key(|&(d, _)| d)
        .map(|(_, k)| k)
}

/// A label spelling this project has **retired**, paired with what to write in
/// its place. Lives beside the registry, read by both the attribute check and
/// the interop one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetiredLabel {
    /// The retired spelling, as unmigrated code still writes it.
    pub spelling: &'static str,
    /// What to write instead — prose, since one retirement can fan out to
    /// several new labels.
    pub guidance: &'static str,
}

/// Every label spelling Steins has retired, with replacement guidance — the
/// table [`retired_label`] looks up.
///
/// **A row is appended whenever a taxonomy node moves or is renamed**: the
/// Levenshtein suggestion of [`nearest_label`] cannot reach a rename more than
/// two edits away. The first two rows are ADR-0083's move of the ambient
/// output channel under `io` (`output` → `io.output.*`, distance 3).
const RETIRED_LABELS: &[RetiredLabel] = &[
    // ADR-0083 split `output` over three children on one question (can
    // `ob_start()` capture this?), so there is no single replacement to name.
    RetiredLabel {
        spelling: "output",
        guidance: "io.output.buffer for echo-shaped code, io.output.header for \
                   header()/setcookie(), or the umbrella io.output",
    },
    RetiredLabel { spelling: "output.header", guidance: "io.output.header" },
];

/// The retirement row for `label`, if this project retired that spelling.
#[must_use]
pub fn retired_label(label: &str) -> Option<&'static RetiredLabel> {
    RETIRED_LABELS.iter().find(|r| r.spelling == label)
}

/// Why an unrecognized label reads as an **attempt at a label** rather than as
/// human prose ([`LabelRegistry::label_intent`]). Variants are the evidence, in
/// weighing order: the first two carry something to suggest, the last two are
/// evidence of intent with nothing to name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelIntent<'a> {
    /// A spelling this project retired — the strongest signal, since Steins
    /// itself once printed that name.
    Retired(&'static RetiredLabel),
    /// Within [`LabelRegistry::nearest`]'s edit cap of a known label, which is
    /// also the suggestion to print.
    Near(&'a str),
    /// Some *other* member of the same tag's label list is a recognized label —
    /// prose does not usually sit in a comma list beside a real effect label.
    KnownSibling,
    /// Two or more dot-path segments, a shape a one-word English note can't take.
    DotPath,
}

/// The label registry **as one run sees it**: the builtin table ([`known_labels`])
/// plus whatever the ADR-0012/0039 plugin channel registered (ADR-0068).
/// Inference asks this, not the free functions, so a plugin-registered label
/// stops earning `effect.unknown-label` without the builtin table growing.
///
/// [`LabelRegistry::builtin`] is the closed default view for a caller with no
/// project in hand. Extension labels are validated *before* they arrive here —
/// the ADR-0068 §2 vendor-root rule is a load-time gate in the discovery layer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LabelRegistry {
    /// Registered extension labels, sorted and deduplicated so two runs that
    /// discovered the same plugins compare equal (a salsa input requirement).
    extensions: Vec<String>,
}

impl LabelRegistry {
    /// The builtin-only registry — what every caller without a plugin channel
    /// wants.
    #[must_use]
    pub fn builtin() -> Self {
        Self { extensions: Vec::new() }
    }

    /// The builtin registry extended with `labels` (already vendor-root checked).
    #[must_use]
    pub fn with_extensions<I: IntoIterator<Item = String>>(labels: I) -> Self {
        let mut extensions: Vec<String> = labels.into_iter().collect();
        extensions.sort();
        extensions.dedup();
        Self { extensions }
    }

    /// The registered extension labels, sorted.
    #[must_use]
    pub fn extensions(&self) -> &[String] {
        &self.extensions
    }

    /// Whether this registry has no extensions.
    #[must_use]
    pub fn is_builtin_only(&self) -> bool {
        self.extensions.is_empty()
    }

    /// [`is_known_label`] over builtins **and** extensions.
    #[must_use]
    pub fn is_known(&self, label: &str) -> bool {
        is_known_label(label) || self.extensions.iter().any(|k| admits(label, k))
    }

    /// [`nearest_label`] over builtins **and** extensions — so a typo of a
    /// registered ecosystem label suggests that label, not a core one.
    #[must_use]
    pub fn nearest(&self, label: &str) -> Option<&str> {
        let builtin = known_labels().iter().copied();
        nearest_of(label, builtin.chain(self.extensions.iter().map(String::as_str)))
    }

    /// Whether an unrecognized `label`, written in a tag whose whole label list
    /// is `siblings`, carries evidence of **label intent** — and if so, which
    /// (issue #311).
    ///
    /// `None` matters: a bare word far from every known label, alone in its
    /// list, is indistinguishable from the one-word note PHPStan lets a
    /// docblock carry, and guessing "it is a label" is what ADR-0082 refuses —
    /// `None` means *stay silent*, permanently.
    ///
    /// Callers filter out already-known labels first; this checks siblings
    /// only, not `label` itself.
    #[must_use]
    pub fn label_intent<'a>(&'a self, label: &str, siblings: &[String]) -> Option<LabelIntent<'a>> {
        if let Some(r) = retired_label(label) {
            return Some(LabelIntent::Retired(r));
        }
        if let Some(near) = self.nearest(label) {
            return Some(LabelIntent::Near(near));
        }
        if siblings.iter().any(|s| s != label && self.is_known(s)) {
            return Some(LabelIntent::KnownSibling);
        }
        let mut segments = label.split('.');
        if segments.clone().count() >= 2 && segments.all(|s| !s.is_empty()) {
            return Some(LabelIntent::DotPath);
        }
        None
    }
}

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
    use super::{
        WIDTH_REFUSED, WIDTH_SAFE, WIDTH_UNVERIFIED, WidthClass, effect_labels, foldable,
        foldable_entry_count, width_class, width_safe,
    };

    /// Classes are pairwise DISJOINT, no name listed twice, size is 53
    /// (ADR-0066, plus ADR-0028's 2026-08-14 wave 1, plus issue #354).
    #[test]
    fn the_width_classes_partition_the_allowlist() {
        for (list, class, label) in [
            (WIDTH_SAFE, WidthClass::Safe, "WIDTH_SAFE"),
            (WIDTH_REFUSED, WidthClass::Refused, "WIDTH_REFUSED"),
            (WIDTH_UNVERIFIED, WidthClass::Unverified, "WIDTH_UNVERIFIED"),
        ] {
            for name in list {
                assert!(foldable(name), "{name} is classified but not foldable");
                assert_eq!(
                    width_class(name),
                    Some(class),
                    "{name} is listed in {label} but classifies elsewhere"
                );
                assert_eq!(
                    list.iter().filter(|&n| n == name).count(),
                    1,
                    "{name} is listed twice in {label}"
                );
                for (other, other_label) in [
                    (WIDTH_SAFE, "WIDTH_SAFE"),
                    (WIDTH_REFUSED, "WIDTH_REFUSED"),
                    (WIDTH_UNVERIFIED, "WIDTH_UNVERIFIED"),
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
        assert_eq!(WIDTH_SAFE.len(), 40, "the verified width-safe subset");
        assert_eq!(WIDTH_REFUSED.len(), 11, "the refused rows");
        assert_eq!(WIDTH_UNVERIFIED.len(), 2, "the unverified rows (ADR-0028 wave 1)");
        assert_eq!(
            foldable_entry_count(),
            53,
            "the allowlist size the ADR-0066 amendments tabulate, plus wave 1 and issue #354"
        );
        assert_eq!(
            WIDTH_SAFE.len() + WIDTH_REFUSED.len() + WIDTH_UNVERIFIED.len(),
            foldable_entry_count(),
            "the count is the three lists and nothing else"
        );
    }

    /// An unverified name declines exactly where a refused one does, without
    /// being one.
    #[test]
    fn the_unverified_rows_decline_like_refused_ones_without_being_them() {
        for name in ["array_merge", "explode", "Array_Merge", "EXPLODE"] {
            assert_eq!(width_class(name), Some(WidthClass::Unverified));
            assert!(foldable(name), "{name} folds on a 64-bit engine");
            assert!(!width_safe(name), "{name} declines on anything narrower");
            assert_eq!(effect_labels(name), Some(&[][..]), "{name} is catalogued pure");
        }
        assert!(!WIDTH_REFUSED.contains(&"explode"));
        assert!(!WIDTH_REFUSED.contains(&"array_merge"));
        // The five this class deferred are no longer deferred: issue #354
        // probed each and landed it in the class its evidence chose. None of
        // them passed through here, and the class did not grow.
        for name in ["str_split", "array_unique", "array_fill"] {
            assert_eq!(width_class(name), Some(WidthClass::Safe), "{name} probed clean");
        }
        for name in ["range", "preg_split"] {
            assert_eq!(
                width_class(name),
                Some(WidthClass::Refused),
                "{name} has a recorded divergence"
            );
        }
    }

    /// The eleven refused rows, named; see `WIDTH_REFUSED` for probes.
    #[test]
    fn the_width_sensitive_builtins_are_refused() {
        for name in [
            "abs",
            "intval",
            "sprintf",
            "dechex",
            "decbin",
            "decoct",
            "bindec",
            "hexdec",
            "version_compare",
            "range",
            "preg_split",
            "ABS",
            "IntVal",
            "SPRINTF",
            "DecHex",
            "Version_Compare",
            "Range",
            "PREG_SPLIT",
        ] {
            assert!(!width_safe(name), "{name} must not be certified width-safe");
            assert!(foldable(name), "{name} is refused on width, not off the allowlist");
        }
        for name in [
            "strtoupper",
            "substr",
            "str_repeat",
            "count",
            "in_array",
            "STRLEN",
            "str_contains",
            "base64_decode",
            "strtr",
            "substr_replace",
            "str_increment",
            "GetType",
            "str_split",
            "array_fill",
            "array_UNIQUE",
        ] {
            assert!(width_safe(name), "{name} is a verified width-safe fold");
        }
    }

    /// The issue #354 slice: three names probed clean and three declines that
    /// are declines, not divergences. The two refusals are pinned beside their
    /// causes in `the_width_sensitive_builtins_are_refused`.
    #[test]
    fn the_deferred_fold_names_landed_where_their_evidence_put_them() {
        for name in ["str_split", "array_fill", "array_unique"] {
            assert!(width_safe(name), "{name} folds on a 32-bit engine too");
            assert!(foldable(name), "{name} is on the folding allowlist");
            assert_eq!(effect_labels(name), Some(&[][..]), "{name} is catalogued pure");
        }
        for name in ["range", "preg_split"] {
            assert!(foldable(name), "{name} folds on a 64-bit engine");
            assert!(!width_safe(name), "{name} is refused on a 32-bit engine");
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
            assert!(width_safe(name), "{name} is an admitted width-safe fold");
            assert!(foldable(name), "{name} is on the folding allowlist");
            assert_eq!(effect_labels(name), Some(&[][..]), "{name} is catalogued pure");
        }
        for name in ["dechex", "decbin", "decoct", "bindec", "hexdec", "version_compare"] {
            assert!(foldable(name), "{name} folds on a 64-bit engine");
            assert!(!width_safe(name), "{name} is refused on a 32-bit engine");
            assert_eq!(effect_labels(name), Some(&[][..]), "{name} is catalogued pure");
        }
    }

    /// The name accessors equal the predicate extensions (issue #64).
    #[test]
    fn the_name_accessors_agree_with_the_predicates() {
        use super::{width_refused_names, width_safe_names, width_unverified_names};
        assert_eq!(width_safe_names(), WIDTH_SAFE);
        assert_eq!(width_refused_names(), WIDTH_REFUSED);
        assert_eq!(width_unverified_names(), WIDTH_UNVERIFIED);
        for name in width_safe_names() {
            assert!(width_safe(name), "{name} is listed safe but the predicate declines it");
        }
        for name in width_refused_names() {
            assert!(!width_safe(name), "{name} is listed refused but the predicate admits it");
            assert_eq!(width_class(name), Some(WidthClass::Refused), "{name} classifies elsewhere");
            assert!(foldable(name), "a refused name is still on the folding allowlist");
        }
        for name in width_unverified_names() {
            assert!(!width_safe(name), "{name} is listed unverified but the predicate admits it");
            assert_eq!(
                width_class(name),
                Some(WidthClass::Unverified),
                "{name} is unverified, which is not refused"
            );
            assert!(foldable(name), "an unverified name is still on the folding allowlist");
        }
        assert_eq!(
            width_refused_names().len() + width_unverified_names().len(),
            foldable_entry_count() - width_safe_names().len(),
            "refused ∪ unverified is exactly what a 32-bit engine does not fold"
        );
        assert_eq!(width_safe_names().len(), 40);
        assert_eq!(width_refused_names().len(), 11);
        assert_eq!(width_unverified_names().len(), 2);
    }

    /// Default-deny: a name without a width classification is not width-safe.
    #[test]
    fn an_unclassified_name_is_not_width_safe() {
        for name in
            ["some_unknown_fn", "ip2long", "crc32", "strtotime", "str_word_count", "strcmp"]
        {
            assert!(!width_safe(name), "{name} must not be certified width-safe");
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
            assert!(!width_safe(name), "{name} must not be certified width-safe");
        }
    }

    #[test]
    fn colored_builtins_carry_their_label() {
        assert_eq!(effect_labels("rand"), Some(&["nondet.random"][..]));
        assert_eq!(effect_labels("time"), Some(&["nondet.time"][..]));
        assert_eq!(effect_labels("file_get_contents"), Some(&["io"][..]));
        assert_eq!(effect_labels("file_put_contents"), Some(&["io"][..]));
        assert_eq!(effect_labels("fopen"), Some(&["io"][..]));
        assert_eq!(effect_labels("scandir"), Some(&["io"][..]));
        assert_eq!(effect_labels("unlink"), Some(&["io"][..]));
        assert_eq!(effect_labels("file_exists"), Some(&["io"][..]));
        assert_eq!(effect_labels("mkdir"), Some(&["io"][..]));
        assert_eq!(
            super::narrowed_stream_labels("unlink", Some(Literal("/tmp/x")), None),
            Some(vec!["io.fs.write"])
        );
        assert_eq!(
            super::narrowed_stream_labels("file_exists", Some(Literal("/tmp/x")), None),
            Some(vec!["io.fs.read"])
        );
        assert_eq!(effect_labels("printf"), Some(&["io.output.buffer"][..]));
        assert_eq!(effect_labels("error_log"), Some(&["io"][..]));
        assert_eq!(effect_labels("setlocale"), Some(&["global.write"][..]));
        assert_eq!(effect_labels("getenv"), Some(&["global.read"][..]));
        assert_eq!(effect_labels("srand"), Some(&["global.write"][..]));
        assert_eq!(effect_labels("mt_srand"), Some(&["global.write"][..]));
        assert_eq!(effect_labels("clearstatcache"), Some(&["global.write"][..]));
    }

    #[test]
    fn foldable_builtins_are_catalogued_pure() {
        for name in ["strtolower", "strlen", "abs", "trim", "count"] {
            assert_eq!(effect_labels(name), Some(&[][..]), "{name} should be pure");
            assert!(foldable(name));
        }
    }

    #[test]
    fn uncatalogued_builtins_are_none() {
        for name in ["some_unknown_fn", "mysqli_query", "proc_open"] {
            assert_eq!(effect_labels(name), None, "{name} must be uncatalogued");
        }
    }

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

    #[test]
    fn effect_labels_are_case_insensitive() {
        assert_eq!(effect_labels("RAND"), Some(&["nondet.random"][..]));
        assert_eq!(effect_labels("File_Put_Contents"), Some(&["io"][..]));
        assert_eq!(effect_labels("STRTOLOWER"), Some(&[][..]));
        assert_eq!(
            super::narrowed_stream_labels("UnLink", Some(Literal("/tmp/x")), None),
            Some(vec!["io.fs.write"])
        );
    }

    use super::method_effect_labels;

    #[test]
    fn pdo_methods_are_colored_io_db() {
        for method in ["query", "exec", "prepare"] {
            assert_eq!(
                method_effect_labels("PDO", method),
                Some(&["io.db"][..]),
                "PDO::{method} is io.db"
            );
        }
        for method in ["execute", "fetch", "fetchAll"] {
            assert_eq!(
                method_effect_labels("PDOStatement", method),
                Some(&["io.db"][..]),
                "PDOStatement::{method} is io.db"
            );
        }
    }

    #[test]
    fn method_rows_match_both_keys_case_insensitively() {
        assert_eq!(method_effect_labels("pdo", "QUERY"), Some(&["io.db"][..]));
        assert_eq!(method_effect_labels("PdoStatement", "FetchAll"), Some(&["io.db"][..]));
    }

    #[test]
    fn uncatalogued_methods_stay_none() {
        assert_eq!(method_effect_labels("PDO", "getAttribute"), None);
        assert_eq!(method_effect_labels("PDO", "beginTransaction"), None);
        assert_eq!(method_effect_labels("mysqli", "query"), None);
        assert_eq!(method_effect_labels("Foo", "query"), None);
    }

    #[test]
    fn io_db_is_a_registered_label() {
        assert!(is_known_label("io.db"));
        assert!(subsumes("io", "io.db"), "coarse io admits io.db");
        assert!(!subsumes("io.db", "io"), "and not the other way round");
        assert!(!subsumes("io.fs", "io.db"), "siblings do not subsume");
    }

    use super::{
        LabelIntent, WrittenWhen, by_value_arg, is_core_label, is_known_label, nearest_label,
        out_param_written_when, out_params, retired_label, subsumes,
    };

    #[test]
    fn subsumption_is_prefix_and_segment_aware() {
        assert!(subsumes("io", "io"), "equal labels subsume");
        assert!(subsumes("io", "io.fs.write"), "coarse admits fine");
        assert!(subsumes("nondet", "nondet.random"));
        assert!(subsumes("io.fs.read", "io.fs.read"));
        assert!(!subsumes("io.fs.read", "io.fs.write"), "siblings do not subsume");
        assert!(!subsumes("io.net", "io"), "fine does not admit coarse");
        assert!(!subsumes("io", "iota"), "non-segment prefix is not subsumption");
        assert!(!subsumes("io.net", "io.netw"), "io.net does not subsume io.netw");
    }

    #[test]
    fn registry_roots_are_known() {
        for label in [
            "io.output", "io", "io.fs", "io.fs.read", "io.fs.write", "io.net", "io.net.http",
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
        assert_eq!(nearest_label("io.outpt"), Some("io.output"));
        assert_eq!(nearest_label("completely-different"), None);
        // The retired ADR-0083 spelling is NOT a near miss (distance 3, past
        // the cap) — why `RETIRED_LABELS` exists (issue #311).
        assert_eq!(nearest_label("output"), None);
        assert_eq!(nearest_label("output.header"), None);
    }

    #[test]
    fn the_retired_table_carries_the_adr_0083_migration() {
        let out = retired_label("output").expect("the retired output root");
        assert_eq!(out.spelling, "output");
        assert_eq!(
            out.guidance,
            "io.output.buffer for echo-shaped code, io.output.header for \
             header()/setcookie(), or the umbrella io.output"
        );
        assert_eq!(retired_label("output.header").map(|r| r.guidance), Some("io.output.header"));
        assert_eq!(retired_label("io.output"), None);
        assert_eq!(retired_label("io.netw"), None);
        assert_eq!(retired_label("database"), None);
        for label in ["io.output", "io.output.buffer", "io.output.header"] {
            assert!(is_known_label(label), "{label} is named as a replacement");
        }
    }

    #[test]
    fn label_intent_tells_a_typo_from_a_humans_prose() {
        let r = super::LabelRegistry::builtin();
        let alone: Vec<String> = Vec::new();

        // THE GUARANTEE (issue #311): a bare word, far from everything, alone in
        // its list, is prose as far as this predicate is concerned — forever.
        assert_eq!(r.label_intent("database", &alone), None);
        assert_eq!(r.label_intent("todo", &alone), None);
        // (a) near a known label.
        assert_eq!(r.label_intent("io.netw", &alone), Some(LabelIntent::Near("io.net")));
        assert_eq!(r.label_intent("nondet.tyme", &alone), Some(LabelIntent::Near("nondet.time")));
        // (b) a recognized sibling in the same list turns even prose-shaped
        // `database` into evidence — the deliberately aggressive signal.
        let beside = vec!["io.db".to_owned(), "database".to_owned()];
        assert_eq!(r.label_intent("database", &beside), Some(LabelIntent::KnownSibling));
        // The sibling must be a *different* token: a list of one unknown label
        // repeated is not evidence of anything.
        let itself = vec!["database".to_owned(), "database".to_owned()];
        assert_eq!(r.label_intent("database", &itself), None);
        // (c) dot-path shape, with nothing near and no known sibling.
        assert_eq!(r.label_intent("cache.warmup", &alone), Some(LabelIntent::DotPath));
        // A trailing dot is not a second segment.
        assert_eq!(r.label_intent("database.", &alone), None);
        // (d) a retirement outranks the rest, and reaches where the metric cannot.
        let retired = r.label_intent("output", &alone).expect("the retired spelling reports");
        assert!(matches!(retired, LabelIntent::Retired(row) if row.spelling == "output"));
        // An extension label a plugin registered makes its own typos near misses.
        let plugged = super::LabelRegistry::with_extensions(["acme.cache".to_owned()]);
        assert_eq!(plugged.label_intent("acme.cach", &alone), Some(LabelIntent::Near("acme.cache")));
    }

    #[test]
    fn the_builtin_registry_answers_exactly_as_the_free_functions_do() {
        let r = super::LabelRegistry::builtin();
        assert!(r.is_builtin_only());
        for label in ["io", "io.db", "nondet.time", "exit", "mutate.local"] {
            assert_eq!(r.is_known(label), is_known_label(label), "{label}");
        }
        for label in ["io.netw", "email.send", "acme.cache"] {
            assert!(!r.is_known(label), "{label} is not in the closed set");
        }
        assert_eq!(r.nearest("io.netw"), Some("io.net"));
    }

    #[test]
    fn an_extension_label_becomes_known_without_the_builtin_table_growing() {
        let r = super::LabelRegistry::with_extensions(["acme.cache".to_owned()]);
        assert!(r.is_known("acme.cache"));
        assert!(!is_known_label("acme.cache"));
        assert!(!r.is_known("acme.cach"));
        assert_eq!(r.nearest("acme.cach"), Some("acme.cache"));
        assert!(r.is_known("acme"));
        assert!(!r.is_known("acme.cache.hit"));
    }

    #[test]
    fn core_roots_are_the_ones_a_plugin_may_only_refine() {
        // ADR-0068 §2: descendants of these are open to any plugin.
        assert!(is_core_label("io.redis"));
        assert!(is_core_label("io"));
        assert!(is_core_label("global.write"));
        assert!(!is_core_label("acme.cache"));
        assert!(!is_core_label("output"));
        assert!(is_core_label("io.output.buffer"));
        assert!(!is_core_label("email.send"));
        assert!(!is_core_label("iota.thing"));
    }

    #[test]
    fn new_effect_labels_are_registered_and_subsume() {
        for label in ["ffi", "io.signal", "io.ipc", "io.output.header", "io.input"] {
            assert!(is_known_label(label), "{label} should be a known registry label");
        }
        assert!(subsumes("io", "io.signal"), "coarse io admits io.signal");
        assert!(subsumes("io", "io.ipc"), "coarse io admits io.ipc");
        assert!(
            subsumes("io.output", "io.output.buffer"),
            "coarse io.output admits io.output.buffer"
        );
        // ADR-0083: bare `io` is the ambient channels' ancestor too.
        assert!(subsumes("io", "io.output.buffer"), "io admits the ambient output channel");
        assert!(subsumes("io", "io.input"));
        assert!(
            !subsumes("io.output.buffer", "io.output.header"),
            "headers are outside the OB-capturable family"
        );
        assert!(!subsumes("io.signal", "io.ipc"), "siblings do not subsume");
        assert!(!subsumes("io", "ffi"));
    }

    #[test]
    fn mutate_local_is_registered_under_mutate() {
        assert!(is_known_label("mutate.local"));
        assert!(subsumes("mutate", "mutate.local"), "a coarse `mutate` admits it");
        assert!(!subsumes("mutate.local", "mutate"), "and not the other way round");
    }

    #[test]
    fn out_param_rows_carry_the_stub_positions() {
        assert_eq!(out_params("preg_match"), Some(&[2][..]));
        assert_eq!(out_params("preg_match_all"), Some(&[2][..]));
        assert_eq!(out_params("similar_text"), Some(&[2][..]));
        assert_eq!(out_params("str_replace"), Some(&[3][..]));
        assert_eq!(out_params("str_ireplace"), Some(&[3][..]));
        // `preg_replace(..., $subject, $limit, &$count)` — count is 4, not 3.
        assert_eq!(out_params("preg_replace"), Some(&[4][..]));
        assert_eq!(out_params("preg_replace_callback"), Some(&[4][..]));
        assert_eq!(out_params("preg_replace_callback_array"), Some(&[3][..]));
        for f in ["sort", "usort", "shuffle", "array_push", "array_pop", "reset", "settype"] {
            assert_eq!(out_params(f), Some(&[0][..]), "{f} writes argument 0");
        }
        assert_eq!(out_params("PREG_MATCH"), Some(&[2][..]));
    }

    #[test]
    fn the_written_when_witness_is_stated_for_the_measured_rows_only() {
        assert_eq!(out_param_written_when("preg_match", 2), Some(WrittenWhen::ReturnTruthy));
        assert_eq!(out_param_written_when("PREG_MATCH", 2), Some(WrittenWhen::ReturnTruthy));
        assert_eq!(out_param_written_when("preg_match_all", 2), Some(WrittenWhen::ReturnTruthy));
        for p in [0, 1, 3, 4] {
            assert_eq!(out_param_written_when("preg_match", p), None, "position {p} is by value");
            assert_eq!(out_param_written_when("preg_match_all", p), None, "position {p} is by value");
        }
        for f in ["similar_text", "str_replace", "sort", "array_pop"] {
            for p in 0..5 {
                assert_eq!(out_param_written_when(f, p), None, "{f} states no witness yet");
            }
        }
    }

    #[test]
    fn a_witness_never_appears_at_a_by_value_position() {
        for f in ["preg_match", "preg_match_all", "sort", "str_replace", "similar_text"] {
            for p in 0..6 {
                if out_param_written_when(f, p).is_some() {
                    assert_eq!(by_value_arg(f, p), Some(false), "{f} argument {p}");
                }
            }
        }
    }

    #[test]
    fn variadic_by_ref_builtins_are_deliberately_absent() {
        for f in ["sscanf", "fscanf", "array_multisort", "extract"] {
            assert_eq!(out_params(f), None, "{f} has no positional out-param row");
        }
    }

    #[test]
    fn by_value_arg_reads_the_out_param_row_positionally() {
        assert_eq!(by_value_arg("preg_match", 0), Some(true));
        assert_eq!(by_value_arg("preg_match", 1), Some(true), "$subject is by value");
        assert_eq!(by_value_arg("preg_match", 2), Some(false), "$matches is by ref");
        // `str_replace(..., $subject, int &$count = null)` — 3, not 2.
        assert_eq!(by_value_arg("str_replace", 2), Some(true));
        assert_eq!(by_value_arg("str_replace", 3), Some(false));
        assert_eq!(by_value_arg("array_pop", 0), Some(false));
        assert_eq!(by_value_arg("sort", 0), Some(false));
        assert_eq!(by_value_arg("usort", 1), Some(true), "the comparator is by value");
        assert_eq!(by_value_arg("PREG_MATCH", 2), Some(false));
    }

    #[test]
    fn by_value_arg_certifies_the_rowless_names_positively() {
        for f in ["trim", "ltrim", "rtrim", "sprintf", "implode", "strlen", "in_array"] {
            assert_eq!(by_value_arg(f, 0), Some(true), "{f} is certified by value");
            assert_eq!(by_value_arg(f, 1), Some(true), "{f} argument 1 too");
        }
        for f in ["chop", "join", "sizeof", "array_first", "array_last", "current", "key",
                  "array_values", "array_keys", "array_flip", "array_reverse",
                  "array_key_first", "array_key_last", "array_slice"] {
            assert_eq!(by_value_arg(f, 0), Some(true), "{f} is certified by value");
        }
        // `array_slice` is by value at every position, `array_splice` writes.
        for p in 0..4 {
            assert_eq!(by_value_arg("array_slice", p), Some(true), "array_slice position {p}");
        }
        assert_eq!(by_value_arg("array_splice", 0), Some(false), "array_splice is by ref");
    }

    /// Issue #41 string-producer family: certified per NAME, so every
    /// position answers `true`, including optional ones like `vsprintf`'s
    /// `$values`.
    #[test]
    fn by_value_arg_certifies_the_string_producer_family() {
        for f in ["addcslashes", "escapeshellarg", "escapeshellcmd", "htmlspecialchars",
                  "htmlentities", "vsprintf"] {
            for p in 0..4 {
                assert_eq!(by_value_arg(f, p), Some(true), "{f} position {p} is by value");
            }
            assert_eq!(by_value_arg(&f.to_uppercase(), 0), Some(true), "{f} folds case");
        }
        // `str_replace` stays the family's rowed member at position 3.
        assert_eq!(by_value_arg("str_replace", 2), Some(true));
        assert_eq!(by_value_arg("str_replace", 3), Some(false));
    }

    /// `mb_*`: certified for **argument** semantics while staying outside the
    /// fold allowlist, which is about the *result* — the two answers disagree.
    #[test]
    fn the_mb_family_is_by_value_without_becoming_foldable() {
        for f in ["mb_strtolower", "mb_strtoupper", "mb_substr", "mb_strlen", "mb_convert_case",
                  "mb_str_split", "mb_str_pad", "mb_strpos", "mb_convert_encoding", "mb_trim"] {
            assert_eq!(by_value_arg(f, 0), Some(true), "{f} is by value");
            assert!(!foldable(f), "{f} must NOT become foldable");
        }
        assert_eq!(by_value_arg("mb_internal_encoding", 0), None);
    }

    #[test]
    fn by_value_arg_declines_every_name_it_has_not_certified() {
        for f in ["sscanf", "fscanf", "array_multisort", "extract", "parse_str", "exec",
                  "my_helper", "some_unknown_function"] {
            assert_eq!(by_value_arg(f, 0), None, "{f} is not certified");
            assert_eq!(by_value_arg(f, 1), None, "{f} is not certified at any position");
        }
    }

    #[test]
    fn the_two_catalog_axes_are_independent() {
        assert_eq!(effect_labels("shuffle"), Some(&["nondet.random"][..]));
        assert_eq!(out_params("shuffle"), Some(&[0][..]));
        assert_eq!(out_params("rand"), None);
        assert_eq!(effect_labels("preg_match"), None);
    }

    #[test]
    fn new_effect_labels_color_the_mined_functions() {
        assert_eq!(effect_labels("pcntl_signal"), Some(&["io.signal"][..]));
        assert_eq!(effect_labels("posix_kill"), Some(&["io.signal"][..]));
        assert_eq!(effect_labels("header"), Some(&["io.output.header"][..]));
        assert_eq!(effect_labels("setcookie"), Some(&["io.output.header"][..]));
        assert_eq!(effect_labels("shmop_write"), Some(&["io.ipc"][..]));
        assert_eq!(
            effect_labels("session_start"),
            Some(&["io.fs.write", "io.output.header", "global.write"][..])
        );
    }

    /// ADR-0083 rows closing the read-and-relay false-negative gap: before, a
    /// body whose only statement was `readfile($p)`/`system($cmd)` carried no
    /// output component.
    #[test]
    fn relaying_builtins_carry_their_output_component() {
        assert_eq!(effect_labels("readfile"), Some(&["io"][..]));
        assert_eq!(effect_labels("fpassthru"), Some(&["io"][..]));
        assert_eq!(
            super::narrowed_stream_labels("readfile", Some(Literal("/tmp/x")), None),
            Some(vec!["io.fs.read", "io.output.buffer"])
        );
        assert_eq!(effect_labels("system"), Some(&["io.process", "io.output"][..]));
        assert_eq!(effect_labels("passthru"), Some(&["io.process", "io.output"][..]));
        assert_eq!(effect_labels("curl_exec"), Some(&["io.net", "io.output"][..]));
        assert_eq!(
            failure_arms("curl_exec"),
            Some(FailureArms::Causes(&[FailureCause::Environment]))
        );
        // The OB flush pair writes through the buffer like `echo` does.
        assert_eq!(effect_labels("flush"), Some(&["io.output.buffer"][..]));
        assert_eq!(effect_labels("ob_flush"), Some(&["io.output.buffer"][..]));
        // `ob_start`/`ob_get_clean` stay uncatalogued: unknown-effect widening is
        // the sound default until masking exists (ADR-0083, deferred).
        assert_eq!(effect_labels("ob_start"), None);
        assert_eq!(effect_labels("ob_get_clean"), None);
        // `fwrite`'s destination narrowing is no longer deferred (issue #318):
        // arg-blind it is `io`, and a `STDOUT` argument proves the OB-unmaskable
        // process fd ADR-0083 named the label for.
        assert_eq!(effect_labels("fwrite"), Some(&["io"][..]));
        assert_eq!(
            super::narrowed_stream_labels("fwrite", Some(Constant("STDOUT")), None),
            Some(vec!["io.output.stdout"])
        );
    }

    // ---- issue #318: argument-dependent narrowing of the stream rows ---------

    use super::StreamTarget::{Constant, Literal};
    use super::narrowed_stream_labels as narrowed;

    #[test]
    fn a_literal_local_path_narrows_to_the_rows_own_direction() {
        // The positive control the whole widening rests on: ordinary code keeps
        // the precise label it had before the row moved to `io`.
        assert_eq!(narrowed("file_get_contents", Some(Literal("/etc/passwd")), None), Some(vec!["io.fs.read"]));
        assert_eq!(narrowed("file_put_contents", Some(Literal("out.txt")), None), Some(vec!["io.fs.write"]));
        // Relative, dot-prefixed and Windows-flavored spellings are all paths.
        assert_eq!(narrowed("file_get_contents", Some(Literal("./cfg/app.ini")), None), Some(vec!["io.fs.read"]));
        assert_eq!(narrowed("file_get_contents", Some(Literal("C:\\tmp\\x")), None), Some(vec!["io.fs.read"]));
        // A path that merely CONTAINS `://` is still a path — the scheme grammar
        // rejects the slashes before it.
        assert_eq!(narrowed("file_get_contents", Some(Literal("/var/log/http://odd")), None), Some(vec!["io.fs.read"]));
        assert_eq!(narrowed("File_Get_Contents", Some(Literal("/x")), None), Some(vec!["io.fs.read"]));
    }

    #[test]
    fn a_url_scheme_narrows_off_the_filesystem_entirely() {
        assert_eq!(narrowed("file_get_contents", Some(Literal("https://example.com/r")), None), Some(vec!["io.net.http"]));
        assert_eq!(narrowed("file_get_contents", Some(Literal("HTTP://example.com")), None), Some(vec!["io.net.http"]));
        assert_eq!(narrowed("file_put_contents", Some(Literal("ftp://h/f")), None), Some(vec!["io.net"]));
        assert_eq!(narrowed("file_get_contents", Some(Literal("ssh2.sftp://h/f")), None), Some(vec!["io.net"]));
        assert_eq!(narrowed("fopen", Some(Literal("tcp://h:9000")), Some(Literal("r"))), Some(vec!["io.net"]));
        // Domain sockets are cross-process state, not network transport.
        assert_eq!(narrowed("fopen", Some(Literal("unix:///tmp/s.sock")), Some(Literal("r"))), Some(vec!["io.ipc"]));
        assert_eq!(narrowed("fopen", Some(Literal("udg:///tmp/s.sock")), Some(Literal("r"))), Some(vec!["io.ipc"]));
        assert_eq!(narrowed("fopen", Some(Literal("expect://ls")), Some(Literal("r"))), Some(vec!["io.process"]));
        assert_eq!(narrowed("file_get_contents", Some(Literal("compress.zlib:///tmp/a.gz")), None), Some(vec!["io.fs.read"]));
        assert_eq!(narrowed("file_get_contents", Some(Literal("phar:///app.phar/x")), None), Some(vec!["io.fs.read"]));
        assert_eq!(narrowed("file_get_contents", Some(Literal("file:///etc/hosts")), None), Some(vec!["io.fs.read"]));
    }

    #[test]
    fn the_php_pseudo_streams_name_their_channel() {
        assert_eq!(narrowed("file_put_contents", Some(Literal("php://output")), None), Some(vec!["io.output.buffer"]));
        assert_eq!(narrowed("file_put_contents", Some(Literal("php://stdout")), None), Some(vec!["io.output.stdout"]));
        assert_eq!(narrowed("file_put_contents", Some(Literal("php://stderr")), None), Some(vec!["io.output.stderr"]));
        assert_eq!(narrowed("file_get_contents", Some(Literal("php://input")), None), Some(vec!["io.input"]));
        assert_eq!(narrowed("file_get_contents", Some(Literal("php://stdin")), None), Some(vec!["io.input"]));
        assert_eq!(narrowed("file_get_contents", Some(Literal("php://memory")), None), Some(vec!["mutate.local"]));
        assert_eq!(narrowed("file_get_contents", Some(Literal("data://text/plain,hi")), None), Some(vec!["mutate.local"]));
        // `php://temp` spills to a real file past its threshold.
        assert_eq!(narrowed("fopen", Some(Literal("php://temp")), Some(Literal("w"))), Some(vec!["io.fs.write"]));
        assert_eq!(narrowed("fopen", Some(Literal("php://temp/maxmemory:1024")), Some(Literal("w"))), Some(vec!["io.fs.write"]));
        assert_eq!(narrowed("file_put_contents", Some(Literal("PHP://StdOut")), None), Some(vec!["io.output.stdout"]));
        assert_eq!(narrowed("fopen", Some(Literal("php://fd/3")), Some(Literal("r"))), None);
    }

    #[test]
    fn a_filter_chain_resolves_its_resource_exactly_one_step() {
        assert_eq!(
            narrowed("file_get_contents", Some(Literal("php://filter/read=convert.base64-encode/resource=https://example.com/r")), None),
            Some(vec!["io.net.http"])
        );
        assert_eq!(
            narrowed("file_get_contents", Some(Literal("php://filter/resource=/etc/hosts")), None),
            Some(vec!["io.fs.read"])
        );
        // One step, no more: a filter naming another filter stops at `None`.
        assert_eq!(
            narrowed("file_get_contents", Some(Literal("php://filter/resource=php://filter/resource=/etc/hosts")), None),
            None
        );
        assert_eq!(narrowed("file_get_contents", Some(Literal("php://filter/read=x")), None), None);
    }

    #[test]
    fn an_unknown_scheme_keeps_the_io_default() {
        // A userland `stream_wrapper_register('acme', …)`: ruling D-W1.
        assert_eq!(narrowed("file_get_contents", Some(Literal("acme://bucket/key")), None), None);
        assert_eq!(narrowed("file_get_contents", Some(Literal("foo://x")), None), None);
        assert_eq!(narrowed("file_get_contents", None, None), None);
        assert_eq!(narrowed("strlen", Some(Literal("/tmp/x")), None), None);
        assert_eq!(narrowed("error_log", Some(Literal("/tmp/x")), None), None);
        assert_eq!(narrowed("session_start", Some(Literal("/tmp/x")), None), None);
    }

    #[test]
    fn fopen_composes_its_direction_from_a_literal_mode() {
        assert_eq!(narrowed("fopen", Some(Literal("/tmp/x")), Some(Literal("r"))), Some(vec!["io.fs.read"]));
        assert_eq!(narrowed("fopen", Some(Literal("/tmp/x")), Some(Literal("rb"))), Some(vec!["io.fs.read"]));
        assert_eq!(narrowed("fopen", Some(Literal("/tmp/x")), Some(Literal("w"))), Some(vec!["io.fs.write"]));
        assert_eq!(narrowed("fopen", Some(Literal("/tmp/x")), Some(Literal("a"))), Some(vec!["io.fs.write"]));
        assert_eq!(narrowed("fopen", Some(Literal("/tmp/x")), Some(Literal("xb"))), Some(vec!["io.fs.write"]));
        // A `+` opens both directions: the parent.
        assert_eq!(narrowed("fopen", Some(Literal("/tmp/x")), Some(Literal("r+"))), Some(vec!["io.fs"]));
        assert_eq!(narrowed("fopen", Some(Literal("/tmp/x")), Some(Literal("w+b"))), Some(vec!["io.fs"]));
        assert_eq!(narrowed("fopen", Some(Literal("/tmp/x")), None), Some(vec!["io.fs"]));
        assert_eq!(narrowed("fopen", Some(Literal("/tmp/x")), Some(Constant("SOME_MODE"))), Some(vec!["io.fs"]));
        assert_eq!(narrowed("fopen", Some(Literal("https://h/r")), None), Some(vec!["io.net.http"]));
    }

    #[test]
    fn the_resource_rows_narrow_only_on_the_predefined_constants() {
        assert_eq!(narrowed("fwrite", Some(Constant("STDOUT")), None), Some(vec!["io.output.stdout"]));
        assert_eq!(narrowed("fputs", Some(Constant("STDERR")), None), Some(vec!["io.output.stderr"]));
        assert_eq!(narrowed("fread", Some(Constant("STDIN")), None), Some(vec!["io.input"]));
        assert_eq!(narrowed("fgets", Some(Constant("STDIN")), None), Some(vec!["io.input"]));
        assert_eq!(narrowed("fpassthru", Some(Constant("STDIN")), None), Some(vec!["io.input", "io.output.buffer"]));
        assert_eq!(narrowed("fwrite", Some(Constant("SOCKET")), None), None);
        // Constants are case-sensitive in PHP, so `stdout` is a different name.
        assert_eq!(narrowed("fwrite", Some(Constant("stdout")), None), None);
        assert_eq!(narrowed("fwrite", Some(Literal("/tmp/x")), None), None);
        assert_eq!(narrowed("file_get_contents", Some(Constant("STDIN")), None), None);
    }

    #[test]
    fn the_two_target_rows_read_each_side_in_its_own_role() {
        assert_eq!(
            narrowed("copy", Some(Literal("/a")), Some(Literal("/b"))),
            Some(vec!["io.fs.read", "io.fs.write"])
        );
        assert_eq!(narrowed("rename", Some(Literal("/a")), Some(Literal("/b"))), Some(vec!["io.fs.write"]));
        assert_eq!(
            narrowed("copy", Some(Literal("https://h/a")), Some(Literal("/b"))),
            Some(vec!["io.net.http", "io.fs.write"])
        );
        assert_eq!(
            narrowed("copy", Some(Literal("/a")), Some(Literal("ssh2.sftp://h/b"))),
            Some(vec!["io.fs.read", "io.net"])
        );
        assert_eq!(
            narrowed("rename", Some(Literal("ftp://h/a")), Some(Literal("/b"))),
            Some(vec!["io.net", "io.fs.write"])
        );
        // One side unprovable: `io` default, whose union with anything is `io`.
        assert_eq!(narrowed("copy", Some(Literal("/a")), None), None);
        assert_eq!(narrowed("copy", None, Some(Literal("/b"))), None);
        assert_eq!(narrowed("copy", Some(Literal("acme://a")), Some(Literal("/b"))), None);
        assert_eq!(narrowed("copy", Some(Literal("/a")), Some(Literal("acme://b"))), None);
    }

    #[test]
    fn the_stat_and_unlink_family_narrows_by_scheme_but_not_by_pseudo_stream() {
        for name in ["unlink", "mkdir", "rmdir", "touch"] {
            assert_eq!(narrowed(name, Some(Literal("/tmp/x")), None), Some(vec!["io.fs.write"]), "{name}");
        }
        for name in ["scandir", "file_exists", "is_file", "is_dir"] {
            assert_eq!(narrowed(name, Some(Literal("/tmp/x")), None), Some(vec!["io.fs.read"]), "{name}");
        }
        assert_eq!(narrowed("unlink", Some(Literal("ssh2.sftp://h/x")), None), Some(vec!["io.net"]));
        assert_eq!(narrowed("mkdir", Some(Literal("ftp://h/d")), None), Some(vec!["io.net"]));
        assert_eq!(narrowed("file_exists", Some(Literal("ssh2.sftp://h/x")), None), Some(vec!["io.net"]));
        assert_eq!(narrowed("is_dir", Some(Literal("ftp://h/d")), None), Some(vec!["io.net"]));
        // A `php://` target is not a question these functions ask.
        assert_eq!(narrowed("unlink", Some(Literal("php://stdout")), None), None);
        assert_eq!(narrowed("is_file", Some(Literal("php://input")), None), None);
        assert_eq!(narrowed("file_exists", Some(Literal("php://filter/resource=/x")), None), None);
        assert_eq!(narrowed("mkdir", Some(Literal("/tmp/d")), Some(Literal("0777"))), Some(vec!["io.fs.write"]));
        assert_eq!(narrowed("scandir", Some(Literal("/tmp")), Some(Literal("1"))), Some(vec!["io.fs.read"]));
        assert_eq!(narrowed("unlink", Some(Constant("STDOUT")), None), None);
    }

    #[test]
    fn every_narrowed_label_is_a_registry_entry() {
        let targets = [
            Literal("/tmp/x"), Literal("https://h/x"), Literal("ftp://h/x"), Literal("ssh2.exec://h/x"),
            Literal("unix:///s"), Literal("expect://ls"), Literal("data://text/plain,x"),
            Literal("php://output"), Literal("php://stdout"), Literal("php://stderr"),
            Literal("php://input"), Literal("php://memory"), Literal("php://temp"),
            Literal("phar:///a.phar/x"), Literal("php://filter/resource=https://h/x"),
            Constant("STDIN"), Constant("STDOUT"), Constant("STDERR"),
        ];
        let modes = [None, Some(Literal("r")), Some(Literal("w")), Some(Literal("r+"))];
        for name in ["file_get_contents", "file_put_contents", "fopen", "copy", "rename", "readfile",
                     "fpassthru", "fread", "fgets", "fwrite", "fputs", "unlink", "mkdir", "rmdir",
                     "touch", "scandir", "file_exists", "is_file", "is_dir"] {
            assert!(effect_labels(name).is_some(), "{name} must be catalogued");
            for &t in &targets {
                for &m in &modes {
                    let Some(labels) = narrowed(name, Some(t), m.or(Some(t))) else { continue };
                    for label in labels {
                        assert!(
                            super::is_known_label(label),
                            "{name}({t:?}) narrowed to unregistered label {label}"
                        );
                    }
                }
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
