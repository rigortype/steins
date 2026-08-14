//! Curated signatures and effect entries for PHP builtins and extensions.
//!
//! # Folding gate
//!
//! [`foldable`] is the hand-curated ADR-0008 allowlist. A function is admitted
//! only when it is pure and deterministic on the concrete path; unlisted names
//! widen. Locale-, timezone-, encoding-, global-, and nondeterminism-sensitive
//! functions remain excluded.
//!
//! `WIDTH_REFUSED` differs from exclusion: its names are foldable on a proven
//! 64-bit engine but decline on 32-bit. The following excluded names have
//! separate portability or semantic evidence:
//!
//! * `strtotime`, `date`, `idate` are `nondet.time` and timezone-coupled even
//!   with explicit timestamps. Probes gave `idate("Y", 0)` as `1970` under UTC
//!   and `1969` under Pacific/Kiritimati; `strtotime("2020-01-01")` differed by
//!   the engine timezone offset (`1577804400` versus `1577836800`).
//! * `mb_*` depends on `mbstring.internal_encoding`; php-wasm 0.1.0 also lacks
//!   mbstring, and all eleven probes widened as unknown functions.
//! * `strcmp` and `strcasecmp` promise a sign, not `memcmp`'s
//!   implementation-defined magnitude. Both engines agreed on all 36 tuples,
//!   including `strcmp("A", "a") == -32` and `strcmp("zzz", "a") == 25`, but
//!   folding those literals would promise more than PHP does. Sign-normalizing
//!   would instead diverge from the executing engine.
//! * `number_format` remains conservatively excluded. Width probes found no
//!   divergence, and at `PINNED_PHP` both `de_DE.UTF-8` and `C` rendered
//!   `number_format(1234.5678, 2)` identically, unaffected by `precision`.
//! * `bin2hex` remains excluded because ADR-0056 records its empty-in/empty-out
//!   return-fact refusal in `docs/research/phpsrc-mining/return_facts.toml`.
//!   Width probes found no divergence.

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

/// The builtin class **display-name** table, generated from the same mining
/// data by the same command — lowercased key → the casing php-src declares.
/// Consulted only by [`builtin_class_display`].
mod display_names_generated;

/// The builtin return-fact refinement table (ADR-0056), generated from
/// `docs/research/phpsrc-mining/return_facts.toml` by `cargo xtask gen-catalog`.
/// Consulted only by [`return_fact`]. The table may be empty.
mod return_facts_generated;

/// The **resource-return** table (ADR-0056 §8), generated from
/// `docs/research/phpsrc-mining/resource_returns.toml` by the same command.
/// Consulted only by [`resource_return`].
mod resource_returns_generated;

/// The builtin declared-return floor (ADR-0069), generated from
/// `docs/research/phpstan-mining/declared_returns.toml` by `cargo xtask
/// gen-catalog`. Consulted only by [`declared_return`] and
/// [`declared_return_changed_at`].
mod declared_returns_generated;

// The capture-group structure of a literal PCRE pattern (issue #149). Carries
// its own module documentation, so this is a plain comment: an outer doc here
// would merge with that header and resolve its intra-doc links in *this* scope.
pub mod preg;

/// Whether `name` is on the folding allowlist (case-insensitive).
///
/// A `true` here is a *permission to fold*, not a promise the call folds: the
/// inference engine still requires the callee to be a non-user function and all
/// arguments to be literals the IR carries before it asks the sidecar.
///
/// Arguments may be scalar literals or recursively concrete array literals.
/// This permits array-taking entries such as `sprintf`, `str_replace`, `in_array`,
/// `count`, and `implode` (issue #39).
///
/// A folded *result* is still scalar-only: a builtin that returns an array (say
/// `str_replace` over an array subject) widens, because carrying an array back
/// would seed synthesized array facts rather than read written ones (#41/#42).
///
/// # Where the list lives
///
/// The allowlist is the union of the integer-width classes `WIDTH_SAFE` and
/// `WIDTH_REFUSED` (issue #64). A name without a width verdict is not foldable.
/// Entries are limited to portable ASCII-cased string operations and other
/// deterministic functions; `mb_*`, locale-sensitive, and `nondet` functions are
/// excluded.
#[must_use]
pub fn foldable(name: &str) -> bool {
    width_safe(name) || width_refused(name)
}

/// The number of names on the folding allowlist (ADR-0054 §9.6's Catalog section
/// "freshness context", the [`foldable`] twin of [`hierarchy_entry_count`]): the
/// union of `WIDTH_SAFE` and `WIDTH_REFUSED`, which is exactly what
/// [`foldable`] tests — the two lists are disjoint by construction (a name has one
/// width verdict), so counting both and summing is the same set [`foldable`]
/// answers `true` for.
#[must_use]
pub fn foldable_entry_count() -> usize {
    WIDTH_SAFE.len() + WIDTH_REFUSED.len()
}

/// Whether folding `name` is **safe on a 32-bit engine** (case-insensitive), given
/// that the caller has already applied the argument range guard.
///
/// # The rule
///
/// A `foldable` name is width-safe when, for every argument tuple in which every
/// integer occurring anywhere in the arguments (values *and* explicit array keys,
/// recursively) lies within `[-(2^31 - 1), 2^31 - 1]`, a 32-bit engine either
/// returns the **identical value and type tag** a 64-bit engine returns, or
/// **declines** (throws or widens). A decline loses precision without producing a
/// wrong literal (ADR-0066 §4).
///
/// The guard's lower bound is `-(2^31 - 1)` and **not** `-2^31`: excluding
/// `PHP_INT_MIN`-on-32-bit is what makes the `abs`-shaped boundary flip
/// unreachable, because no in-range integer has an out-of-range magnitude.
///
/// The subset is verified by differential probes against php-wasm 0.1.0
/// (PHP 8.5.2, `PHP_INT_SIZE = 4`) and `php` 8.5.8 (`PHP_INT_SIZE = 8`) through the
/// same `steins_handle` dispatch core: **661 adversarial tuples** covering
/// boundary integers, oversized numeric strings, oversized floats, negative inputs,
/// integer array keys at `PHP_INT_MAX`, engine-minted binary strings, out-of-alphabet
/// string arithmetic and both `strtr` arities. See the ADR-0066 amendments for the
/// per-name tables.
///
/// A `false` here is not a claim that the name is width-*sensitive* in general —
/// it is a refusal to certify it. Default-deny: an unclassified or newly added
/// name folds only on a provably 64-bit engine.
#[must_use]
pub fn width_safe(name: &str) -> bool {
    WIDTH_SAFE.iter().any(|&f| name.eq_ignore_ascii_case(f))
}

/// The complement of [`width_safe`] *within the folding allowlist* — a name that
/// is foldable and whose 32-bit behaviour is refused. Not the same as
/// `!width_safe(name)`, which is also true of every name that is not foldable at
/// all; see `WIDTH_REFUSED` for the refusals and their probes.
fn width_refused(name: &str) -> bool {
    WIDTH_REFUSED.iter().any(|&f| name.eq_ignore_ascii_case(f))
}

/// The verified width-safe names, in catalog order — the *extension* of
/// [`width_safe`], for a caller that must **name** the subset rather than test a
/// membership.
///
/// The playground boundary widget uses this catalog-backed list so its displayed
/// subset cannot drift from the folding gate (issue #64).
#[must_use]
pub fn width_safe_names() -> &'static [&'static str] {
    WIDTH_SAFE
}

/// The refused names, in catalog order — the complement of [`width_safe_names`]
/// *within* the folding allowlist ([`foldable`] is the union of the two by
/// construction, so this is that complement and not a third list to keep in step).
///
/// These are the folds a 32-bit engine does not get, by name. See `WIDTH_REFUSED`
/// for each refusal's probe evidence.
#[must_use]
pub fn width_refused_names() -> &'static [&'static str] {
    WIDTH_REFUSED
}

/// The verified width-safe half of the folding allowlist (issue #64).
///
/// Grouped by *why* the width cannot reach the result:
///
/// * **string in, string out.** The result is a byte transform of the subject.
///   The only integer in sight is a coerced subject (`strtoupper(2147483647)`),
///   and an in-range integer has the same decimal spelling on both machines.
/// * **result bounded by the input.** `strlen`/`count` return a length or an
///   element count, bounded by a string that fits in the engine's own memory and
///   by the fold seam's 256-entry array budget — neither can reach 2^31.
/// * **int parameters, in-range results.** `substr`/`str_repeat`/`intdiv` take
///   `int` parameters, but an in-range argument yields an in-range result
///   (`|intdiv(a, b)| <= |a|`). The one divergence class they have is a *decline*:
///   an oversized numeric string or float landing on an `int` parameter
///   (`substr("abcdef", "3000000000")`) is a `TypeError` on the 32-bit engine
///   where the 64-bit engine answers — the sound direction.
/// * **no integer in the result at all.** `floatval` returns a double (64-bit on
///   both machines), `boolval` returns a bool, `strval` renders under the same
///   `precision` ini (14 on both builds, verified), and `in_array` returns a
///   bool from php-src's own `zendi_smart_strcmp`, whose overflow guard makes two
///   oversized numeric strings compare as strings on BOTH machines
///   (`in_array("9007199254740993", ["9007199254740992"])` is `false` on each).
///
/// The following names use the same verified categories:
///
/// * **byte transform of the subject.** `ucwords`, `strtr` (both arities),
///   `preg_quote`, `addslashes`, `urlencode`/`urldecode`,
///   `rawurlencode`/`rawurldecode`, `base64_encode`/`base64_decode`. No integer
///   enters the result, and a coerced integer subject has one decimal spelling in
///   range. The case-touching member is **locale-free at `PINNED_PHP`**: `ucwords`
///   has been ASCII-only since PHP 8.2's locale-independent case conversion, which
///   is what lets it sit beside `strtoupper` here.
/// * **string arithmetic that never becomes machine arithmetic.**
///   `str_increment`/`str_decrement` (8.3+, present on both builds) carry the
///   digits in the *string*, so `str_increment("9223372036854775807")` is
///   `"9223372036854775808"` on both machines where any integer path would have
///   overflowed. Out-of-alphabet input is a `ValueError` on both.
/// * **int parameters, in-range results.** `str_pad`, `substr_replace` (scalar
///   subject). Their `int` parameters are an offset, a length or a target width, so
///   an in-range argument clamps against the subject identically; the divergence
///   class is again a *decline* — `str_pad("abc", "-3000000000")` answers `"abc"`
///   on 64-bit and is a `TypeError` on 32-bit.
/// * **no integer in the result at all.** `str_starts_with`, `str_contains` and
///   `str_ends_with` return a bool from a byte comparison, and `gettype` returns
///   one word from a fixed vocabulary.
///
/// `substr_replace` is listed for its **scalar** subject. Handed an array subject
/// it returns an array, and an array *result* widens on the Rust side exactly as
/// `str_replace`'s does — the same documented #41/#42 boundary, reached by the same
/// path, and identical on both engines (`substr_replace(["aa","bb"], "X", 0)` is
/// `["X","X"]` on each, so there is nothing for the width classification to catch).
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
];

/// The **refused rows** of the width classification, with the divergence that
/// refused each — the ADR-0061 refused-row discipline (the `bin2hex` trap style)
/// applied to the integer machine. Every probe below passes the argument range
/// guard, so the guard cannot exclude it; only refusing the name can.
///
/// Each row is a *silent* divergence: both engines return a value, and the values
/// (or their type tags) differ. Nothing throws, nothing widens, nothing warns —
/// which is precisely the ADR-0066 §4 hazard, and why these three cannot be
/// certified. Probes are verbatim `fold` results, 64-bit `php` 8.5.8 vs 32-bit
/// php-wasm 0.1.0 (PHP 8.5.2).
///
/// * `abs`      — REFUSED: the **type tag** flips. A numeric string is coerced to
///   the `int|float` parameter by the *engine's* width, so an argument the range
///   guard never sees as an integer re-enters as one.
///   `abs("3000000000")` = `int(3000000000)` / `float(3000000000)`;
///   `abs("-2147483648")` = `int(2147483648)` / `float(2147483648)`.
///   The guard's exclusion of `-2^31` closes the *integer* path to this flip; it
///   cannot close the numeric-string path, so the name goes.
/// * `intval`   — REFUSED: saturation and wraparound, by definition of the cast.
///   `intval("3000000000")` = `3000000000` / `2147483647` (saturated);
///   `intval("-3000000000")` = `-3000000000` / `-2147483648`;
///   `intval("FFFFFFFFF", 16)` = `68719476735` / `2147483647`;
///   `intval(4.2e9)` = `4200000000` / `-94967296` (wrapped);
///   `intval(1.0e30)` = `5076964154930102000` / `0`.
///   Ten of seventeen probes diverged — this is the width-sensitive builtin.
/// * `sprintf`  — REFUSED: the integer conversion specifiers render the machine
///   word, so an **in-range** argument suffices.
///   `sprintf("%b", -1)` = 64 ones / 32 ones;
///   `sprintf("%x", -1)` = `"ffffffffffffffff"` / `"ffffffff"`;
///   `sprintf("%x", -2147483647)` = `"ffffffff80000001"` / `"80000001"`;
///   `sprintf("%o", -1)` = `"1777777777777777777777"` / `"37777777777"`;
///   `sprintf("%u", -1)` = `"18446744073709551615"` / `"4294967295"`;
///   and `%d` re-imports the `intval` saturation for a numeric-string or float
///   argument: `sprintf("%d", 3.0e9)` = `"3000000000"` / `"-1294967296"`.
///   A format-string-aware sub-classification is possible in principle and is
///   deliberately not attempted: the safe/unsafe line would live inside a string
///   literal, which is the wrong place for a soundness gate.
///
/// Six further rows read or write an integer in the machine's own width:
///
/// * `dechex`   — REFUSED: renders the machine word for a negative argument, and
///   the argument is **in range**, so no guard can exclude it.
///   `dechex(-1)` = `"ffffffffffffffff"` / `"ffffffff"`;
///   `dechex(-2147483647)` = `"ffffffff80000001"` / `"80000001"`.
/// * `decbin`   — REFUSED: same shape, 64 ones versus 32.
///   `decbin(-1)` = 64 × `1` / 32 × `1`;
///   `decbin(-2147483647)` = `"…110000000000000000000000000000001"` (64 digits) /
///   `"10000000000000000000000000000001"` (32).
/// * `decoct`   — REFUSED: same shape in base 8.
///   `decoct(-1)` = `"1777777777777777777777"` / `"37777777777"`;
///   `decoct(-2147483647)` = `"1777777777760000000001"` / `"20000000001"`.
/// * `bindec`   — REFUSED: the **type tag** flips at the width boundary, the
///   `abs` failure mode reached from a plain string argument.
///   `bindec("11111111111111111111111111111111")` = `int(4294967295)` /
///   `float(4294967295)`.
/// * `hexdec`   — REFUSED: as `bindec`. `hexdec("FFFFFFFF")` = `int(4294967295)` /
///   `float(4294967295)`; `hexdec("FFFFFFFFF")` = `int(68719476735)` /
///   `float(68719476735)`; `hexdec("7FFFFFFFFFFFFFFF")` = `int` / `float`.
/// * `version_compare` — REFUSED, and the *surprise* of issue #78. It looks like
///   pure string work and its documented return is `-1|0|1` (or a bool), but
///   php-src compares each numeric run of a canonicalized version through a C
///   `long`, so on a 32-bit engine two oversized runs both saturate and compare
///   **equal**. The arguments are strings, so the range guard never sees an
///   integer to reject:
///   `version_compare("2147483647", "2147483648")` = `-1` / `0`;
///   `version_compare("3000000000", "4000000000")` = `-1` / `0`;
///   `version_compare("1.3000000000", "1.4000000000")` = `-1` / `0`;
///   `version_compare("9223372036854775807", "9223372036854775806")` = `1` / `0`.
///   The three-argument (bool) form inherits the same comparison, so it is refused
///   with it rather than split.
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
];

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
/// * `None` — **uncatalogued**: the effect is unknown, so proven-only checking
///   emits no finding (ADR-0005).
///
/// Matching is case-insensitive (PHP function names are).
///
/// # Curated labels (ADR-0021)
///
/// Labels follow ADR-0018's taxonomy. Argument-dependent effects use the safe,
/// argument-insensitive upper bound:
///
/// * Every **wrapper-capable** stream API is `io`, the parent of every channel a
///   registered stream wrapper can reach (issue #318) — which is every
///   filesystem row here, so no argument-blind row in this table produces
///   `io.fs.*` any more (`session_start`'s composite is the one exception, and
///   its handler writes an actual session file). `file_get_contents` is not a
///   filesystem read — `file_get_contents('https://…')` is a network read — nor
///   is `unlink('ssh2.sftp://…')` a filesystem write, nor `fread` on a resource
///   whose provenance this table cannot see. A call site that *proves* its
///   target narrows the row back down; see [`narrowed_stream_labels`], which is
///   where the precise family now comes from.
/// * `print_r`/`var_export`/`var_dump` are colored `io.output.buffer` even
///   though the first two are pure when their second argument is `true`
///   (return-mode); the upper bound is the arg-blind safe choice.
/// * `sleep`/`usleep` are `io`: an observable timing side effect on the running
///   process, closest to the io root among the initial colors.
/// * `curl_exec` keeps its `io.output` component arg-blind (only
///   `CURLOPT_RETURNTRANSFER` suppresses the echo), and `system`/`passthru` take
///   the parent `io.output` rather than `io.output.buffer` because the evidence
///   for OB capturability of a relayed child's output is split — ADR-0083 puts
///   split evidence on the unmaskable side. None of the three is
///   wrapper-capable, so all three keep their precise transport component.
///
/// `exit`/`die` are **language constructs**, not functions — they never reach
/// this table; the effects pass detects them structurally (ADR-0019 rule 4).
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
    // `session_start` is genuinely composite (effects_gaps.md): the default file
    // handler writes session files (`io.fs.write`), the session cookie is sent as
    // a `Set-Cookie` header (`io.output.header`), and `$_SESSION`/ini are mutated
    // (`global.write`). The upper-bound set is all three.
    const SESSION: &[&str] = &["io.fs.write", "io.output.header", "global.write"];
    // Runs a child process *and* relays its output. The relay's OB-capturability
    // is not settled, so the row takes the parent `io.output` rather than
    // `.buffer` — over-approximating toward "cannot be masked" is the sound side
    // (ADR-0083).
    const PROCESS_TO_OUTPUT: &[&str] = &["io.process", "io.output"];
    // `curl_exec` writes the response body to output unless `CURLOPT_RETURNTRANSFER`
    // is set; arg-blind, the upper bound keeps the output component, at the parent
    // for the same capturability reason as `PROCESS_TO_OUTPUT`.
    const NET_TO_OUTPUT: &[&str] = &["io.net", "io.output"];

    // A per-call lowercase copy keeps the arms readable; PHP names are ASCII.
    let colored: Option<&'static [&'static str]> = match name.to_ascii_lowercase().as_str() {
        "rand" | "mt_rand" | "random_int" | "random_bytes" | "uniqid" | "shuffle" => {
            Some(NONDET_RANDOM)
        }
        "time" | "microtime" | "hrtime" | "date" | "mktime" => Some(NONDET_TIME),
        // The **wrapper-capable** family (issue #318), which is every filesystem
        // row this catalog has. Each of these reaches whatever the stream layer
        // resolves its target to — a URL wrapper, a socket, a process pipe, the
        // output channel — so the argument-blind row can only be the `io` parent;
        // a stricter row would hide a network read under an `io.fs.read`
        // envelope, which is the upper-bound contract's exact failure mode. The
        // path-taking rows are wrapper-capable by their target string, the
        // stat-and-unlink family included (`unlink('ssh2.sftp://…')` deletes over
        // the network, `file_exists('ftp://…')` stats over it); the
        // resource-taking half (`fread`/`fgets`/`fwrite`/`fputs`) by the
        // provenance of a resource this table cannot see. The relay component of
        // `readfile`/`fpassthru` folds into the same `io` (which subsumes it).
        // [`narrowed_stream_labels`] is what gives the precise labels back at a
        // call site that proves its target.
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
        // Process-global state with no channel behind it: the seeding pair
        // replaces the RNG's generator state, `clearstatcache` empties the
        // engine's stat cache (a write to a cache every later `is_file`/`stat`
        // reads, not a filesystem access of its own). Drawing from the RNG stays
        // `nondet.random` above — seeding *writes* the state a draw reads, and
        // the two are not the same effect.
        "srand" | "mt_srand" | "clearstatcache" => Some(GLOBAL_WRITE),
        "getenv" | "ini_get" | "date_default_timezone_get" => Some(GLOBAL_READ),
        // Signal delivery/handling (effects_gaps.md §1). pcntl/posix procedural
        // functions; a daemon/worker envelope declares `@effects io.signal`.
        "pcntl_signal" | "pcntl_signal_dispatch" | "pcntl_alarm" | "pcntl_async_signals"
        | "pcntl_sigprocmask" | "pcntl_sigwaitinfo" | "posix_kill" => Some(IO_SIGNAL),
        // HTTP response-header mutation (effects_gaps.md §2).
        "header" | "header_remove" | "setcookie" | "setrawcookie" | "http_response_code" => {
            Some(IO_OUTPUT_HEADER)
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

/// A call argument a **call site** proved constant (issue #318) — the evidence
/// [`narrowed_stream_labels`] narrows a wrapper-capable row on.
///
/// Both forms are *syntactic* proof, never dataflow: a variable, a concatenation
/// or an interpolated string is no target at all, and the caller keeps the `io`
/// default rather than guessing what the variable holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamTarget<'a> {
    /// A quoted string literal with no interpolation, by its decoded value: a
    /// path, a URL, or a `php://` pseudo-stream.
    Literal(&'a str),
    /// A bare constant fetch, by its unqualified spelling (`STDOUT`, `STDERR`,
    /// `STDIN`) — the only spelling of an open stream *resource* a structural
    /// scan can read.
    Constant(&'a str),
}

/// The **narrowed** effect labels a wrapper-capable stream call earns at a call
/// site that proves its target (issue #318), or `None` when nothing here proves
/// anything — in which case the caller keeps [`effect_labels`]' sound `io`
/// default.
///
/// This is the other half of the widening above, and the reason it costs no
/// precision on ordinary code: the argument-blind row must cover every channel a
/// stream wrapper can reach, but a call whose target is a *constant* reaches
/// exactly one of them, and the constant says which. `file_get_contents('/etc/hosts')`
/// is still `io.fs.read`; `file_get_contents('https://…')` is `io.net.http`; and
/// `file_get_contents($url)` is `io`, because it is.
///
/// `first` and `second` are the call's first two positional arguments in their
/// proven-constant form (`None` for an argument that is not a constant). What
/// the second one means is the row's business: `fopen`'s mode string,
/// `copy`/`rename`'s destination, and nothing at all for the rest.
///
/// Each target is read through **its own role's** direction, which is what makes
/// a two-target row honest: `copy('/a', '/b')` reads one path and writes the
/// other, so it earns `["io.fs.read", "io.fs.write"]`, and
/// `copy('https://…', '/b')` earns `["io.net.http", "io.fs.write"]`. `rename`
/// writes on both sides — it moves a directory entry and reads no contents.
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
/// A `php://` special stream names a *channel*, and the label names the channel
/// it names, not the direction of the call: `file_put_contents('php://stdout', …)`
/// and a hypothetical read of the same target both color `io.output.stdout`. The
/// whole `php://` column is declined by the stat-and-unlink rows, which open no
/// stream (`is_file('php://stdout')` is not a question about a channel).
///
/// # What it declines
///
/// * **A userland wrapper** (`stream_wrapper_register('acme', …)`) is an unknown
///   scheme, so it falls through to `None` and the call keeps `io` — ruling D-W1
///   of the soundness proposal, which is an approximation and not a mechanism:
///   nothing here reads the registration.
/// * **`copy`/`rename` with one provable side.** The row is the union of the two
///   targets' narrowings, and the unprovable side contributes the `io` default,
///   whose union with anything is `io` — no narrowing at all. Both sides must be
///   constant or the answer is `None`.
/// * **A `php://` target on a stat-and-unlink row** (`unlink`, `mkdir`, `rmdir`,
///   `touch`, `scandir`, `file_exists`, `is_file`, `is_dir`), for the reason
///   above.
/// * **A form mismatch**: a path row handed a constant, or a resource row handed
///   a string literal (`fwrite('/tmp/x', …)` passes no resource). Neither is a
///   target this table can read.
#[must_use]
pub fn narrowed_stream_labels(
    name: &str,
    first: Option<StreamTarget<'_>>,
    second: Option<StreamTarget<'_>>,
) -> Option<Vec<&'static str>> {
    // The target leads: a call with no constant first argument — the common case,
    // every builtin call in the project reaches this — answers before the row
    // lookup pays for a lowercase copy of the name.
    let first = first?;
    let row = stream_row(&name.to_ascii_lowercase())?;
    let mut labels = target_labels(row, row.direction, first, second)?;
    // A second target is narrowed through **its own** role's direction: `copy`
    // reads its source and writes its destination, so the two sides can land on
    // different labels and both are true of the call.
    if let SecondArg::Target(direction) = row.second {
        for label in target_labels(row, direction, second?, None)? {
            if !labels.contains(&label) {
                labels.push(label);
            }
        }
    }
    // The read-and-relay pair: narrowing restores the output component the `io`
    // default folded away (ADR-0083 — `ob_start()` + `readfile()` is a documented
    // capture pattern, so the relay is OB-visible).
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
    /// stat-and-unlink family opens no stream — `is_file('php://stdout')` names
    /// no channel anyone writes on purpose — so those rows decline it rather than
    /// color a call after a target that says nothing about what it did.
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
        // Two paths, one role each: `copy($from, $to)` reads the source and
        // writes the destination, so a proven pair earns both labels — the
        // argument-blind row could never say that, and neither could a union
        // taken through one direction.
        "copy" => Some(row(Path, Read, SecondArg::Target(Write), false, true)),
        // `rename` moves a directory entry: both sides are metadata writes, and
        // neither reads the file's contents.
        "rename" => Some(row(Path, Write, SecondArg::Target(Write), false, true)),
        "fread" | "fgets" => Some(simple(Resource, Read)),
        "fwrite" | "fputs" => Some(simple(Resource, Write)),
        // Reads a resource and relays it to the output channel.
        "fpassthru" => Some(row(Resource, Read, SecondArg::Ignored, true, true)),
        // The stat-and-unlink family: wrapper-capable all the same — `unlink` and
        // `mkdir` go over `ssh2.sftp://`, `file_exists` stats over `ftp://` — but
        // they open no stream, so the `php://` pseudo-streams are not targets they
        // can meaningfully be handed.
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
        // No scheme at all: an ordinary path, relative or absolute.
        return Some(fs_labels(direction, mode));
    };
    let scheme = scheme.to_ascii_lowercase();
    match scheme.as_str() {
        "http" | "https" => Some(vec!["io.net.http"]),
        "ftp" | "ftps" | "tcp" | "udp" | "ssl" | "tls" => Some(vec!["io.net"]),
        // The socket wrappers that are not sockets on a network: a filesystem
        // (`unix://`) or abstract (`udg://`) domain socket is cross-process
        // state, which is `io.ipc` and NOT under `io.net`.
        "unix" | "udg" => Some(vec!["io.ipc"]),
        // `expect://` runs a program through a PTY.
        "expect" => Some(vec!["io.process"]),
        // A `data:` URI is its own content — nothing is read from anywhere.
        "data" => Some(vec!["mutate.local"]),
        // Wrappers layered over the filesystem: the bytes still come from (or go
        // to) a file, so the target's own direction stands.
        "file" | "zlib" | "phar" | "glob" => Some(fs_labels(direction, mode)),
        "php" => php_labels(target, row, direction, mode, allow_filter),
        // `ssh2.sftp://`, `ssh2.exec://`, … — all of them a network round trip.
        _ if scheme.starts_with("ssh2.") => Some(vec!["io.net"]),
        // `compress.zlib://`, `compress.bzip2://` — the filesystem family again.
        _ if scheme.starts_with("compress.") => Some(fs_labels(direction, mode)),
        // An unknown scheme is a wrapper this catalog knows nothing about,
        // registered userland ones included (D-W1): no narrowing.
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
        // The two spellings of the script's inbound stream (ADR-0083).
        "input" | "stdin" => return Some(vec!["io.input"]),
        // A memory stream is a buffer with a stream API over it.
        "memory" => return Some(vec!["mutate.local"]),
        _ => {}
    }
    // `php://temp` spills to a temporary file past its memory threshold
    // (`php://temp/maxmemory:1024`), so it is the filesystem family, not
    // `php://memory`.
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
/// instance of the *builtin* class `class`, or `None` for uncatalogued.
///
/// The class-world twin of [`effect_labels`], with the same three-valued
/// contract: `Some(labels)` is a colored row, `Some(&[])` is a catalogued-pure
/// row, and `None` says the catalog knows nothing — which widens to
/// unknown-effect (exhaustiveness taint, no finding), never a guess.
///
/// Both keys match **case-insensitively**: PHP class *and* method names fold
/// case, so `new pdo(...)->QUERY()` is the same row as `PDO::query`.
///
/// The key is the **global** class name, no namespace — these are engine classes.
/// A consumer must resolve the receiver's name to an FQN first and only then key
/// this table, so a namespaced `App\PDO` never collides with the engine's `PDO`;
/// and a class the *project* defines shadows this table entirely (the project's
/// own method→method edge is the better answer, and its body is the truth).
///
/// # Membership (issue #67)
///
/// Rows cover `PDO`/`PDOStatement` with coarse label `io.db`. Because runtime
/// configuration controls whether emulated `prepare` contacts the server,
/// `prepare` takes the argument-insensitive upper bound.
#[must_use]
pub fn method_effect_labels(class: &str, method: &str) -> Option<&'static [&'static str]> {
    const IO_DB: &[&str] = &["io.db"];

    // Per-call lowercase copies keep the arms readable; PHP names are ASCII.
    match (class.to_ascii_lowercase().as_str(), method.to_ascii_lowercase().as_str()) {
        ("pdo", "query" | "exec" | "prepare") => Some(IO_DB),
        ("pdostatement", "execute" | "fetch" | "fetchall") => Some(IO_DB),
        _ => None,
    }
}

/// The **by-ref out-parameter rows** (ADR-0063 §2.3): the 0-based positional
/// indices a builtin writes through a reference parameter.
///
/// Out-parameter writes are call-dependent, unlike the unconditional function
/// labels from [`effect_labels`]: `preg_match($p, $s)` writes nothing, while
/// `preg_match($p, $s, $m)` writes `$m`. Rows therefore carry positions and the
/// consumer resolves them at the call site (ADR-0063; php-src #11884):
///
/// * a position `p` contributes **nothing** unless the call actually supplies
///   `p` (`arg_count > p`) — the arity leg;
/// * what it contributes depends on the *lvalue root* of argument `p`
///   (`steins_syntax::RefTarget`) — the target leg: a binding of the calling
///   frame earns `mutate.local`, a superglobal earns `global.write`, anything
///   that escapes or cannot be classified earns the conservative parent
///   `mutate`.
///
/// A builtin may carry both an unconditional color and an out-param row: the two
/// axes join (`shuffle` is `nondet.random` *and* writes argument 0).
///
/// ## Membership
///
/// Rows are transcribed from the php-src stubs at `PINNED_PHP`, restricted to
/// **fixed positional** reference parameters, which is what a positional index
/// can express faithfully. The variadic-by-ref family (`sscanf`, `fscanf`,
/// `array_multisort`) is deliberately absent: its reference positions are
/// open-ended, so a positions row could only under-approximate, and an
/// under-approximated *target* leg would silently downgrade a property write to
/// `mutate.local`. Silence beats a wrong color.
///
/// `extract()` is likewise absent — it writes the caller's symbol *table*, not a
/// named argument, which is the ADR-0046 dynamism world, not this one.
#[must_use]
pub fn out_params(name: &str) -> Option<&'static [usize]> {
    const P0: &[usize] = &[0];
    const P2: &[usize] = &[2];
    const P3: &[usize] = &[3];
    const P4: &[usize] = &[4];

    match name.to_ascii_lowercase().as_str() {
        // Array sort / rearrangement / stack-and-queue: the array itself is
        // argument 0 and is always by-ref, so the arity leg is satisfied by any
        // well-formed call. `usort`/`uasort`/`uksort`/`array_walk` are also
        // callback invokers (`invocation_shape`) — the two rows compose.
        "sort" | "rsort" | "asort" | "arsort" | "ksort" | "krsort" | "usort" | "uasort"
        | "uksort" | "natsort" | "natcasesort" | "shuffle" | "array_splice" | "array_push"
        | "array_pop" | "array_shift" | "array_unshift" | "array_walk"
        | "array_walk_recursive" => Some(P0),
        // Internal array-pointer moves: `array|object &$array` in the stubs.
        "reset" | "end" | "next" | "prev" => Some(P0),
        // `settype(mixed &$var, string $type)`.
        "settype" => Some(P0),
        // `preg_match(string $pattern, string $subject, array &$matches = null, …)`
        // — the ADR's headline case: optional, so the arity leg does real work.
        "preg_match" | "preg_match_all" => Some(P2),
        // `similar_text(string $string1, string $string2, float &$percent = null)`.
        "similar_text" => Some(P2),
        // `str_replace(…, …, $subject, int &$count = null)`.
        "str_replace" | "str_ireplace" => Some(P3),
        // `preg_replace_callback_array(array $pattern, $subject, int $limit = -1,
        // int &$count = null, int $flags = 0)`.
        "preg_replace_callback_array" => Some(P3),
        // `preg_replace($pattern, $replacement, $subject, int $limit = -1,
        // int &$count = null, int $flags = 0)` — `$count` is position **4**, not
        // 3: the optional `$limit` sits between subject and count. Same shape for
        // `preg_replace_callback`, whose callback is argument 1.
        "preg_replace" | "preg_replace_callback" => Some(P4),
        _ => None,
    }
}

/// **When** a by-ref out-parameter write is proven to have happened (ADR-0077
/// §3.2) — the *written-when* witness an [`out_params`] row may carry.
///
/// An out-parameter write is conditional, and the condition is part of the
/// callee's contract rather than something a caller can see. `preg_match`
/// measures (PHP 8.5.9) as three outcomes and only two of them write: `1`
/// assigns the success shape, `0` assigns `[]`, and a pattern PCRE refuses to
/// compile returns `false` and assigns **nothing at all**, leaving the caller's
/// variable holding whatever it held. The third case is not a value a fact could
/// widen to include, which is why the witness names a *return value* rather than
/// promising an unconditional write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WrittenWhen {
    /// The write happened on exactly the paths where the call's return value is
    /// **truthy**. Every falsy return — including one that means "the callee
    /// refused its inputs" — proves nothing about the argument.
    ReturnTruthy,
}

/// The *written-when* witness for position `position` of `name`, or `None` when
/// the catalog states none (ADR-0077 §3.2).
///
/// `None` is the answer for every position of every name but the one row below,
/// and it means "no seed": the consumer keeps forgetting the argument exactly as
/// it does today. The engine never guesses a witness — a row is added only when
/// the callee's documented contract has been read *and* the behavior measured,
/// because a wrong witness manufactures a fact on a path where the callee never
/// wrote.
///
/// # Membership
///
/// * `preg_match` position 2 — measured above.
/// * `preg_match_all` position 2 (issue #168) — measured (PHP 8.5.9): the return
///   is the number of full matches, an `int >= 1` on the truthy branch, `0` on
///   zero matches (which still writes — empty columns), and `false` on a compile
///   failure, which writes nothing at all. Truthy therefore proves both that the
///   pattern compiled and that at least one match landed, exactly the
///   `ReturnTruthy` discipline; the zero-match write is real but
///   indistinguishable from `false` on the falsy branch, so the falsy side
///   stays unseeded.
///
/// Deliberately absent, each a decline until measured (ADR-0077 §4): every other
/// [`out_params`] row — `sort` and friends write their argument too, and their
/// contracts deserve the same treatment, but none has been measured here.
///
/// A witness is not by itself a fact: it says *where* a seed would be sound, and
/// the consumer still has to know what was written.
#[must_use]
pub fn out_param_written_when(name: &str, position: usize) -> Option<WrittenWhen> {
    match (name.to_ascii_lowercase().as_str(), position) {
        // `preg_match(string $pattern, string $subject, array &$matches = null, …)`:
        // `1` writes the success shape, `0` writes `[]`, `false` writes nothing.
        ("preg_match", 2) => Some(WrittenWhen::ReturnTruthy),
        // `preg_match_all(string $pattern, string $subject, array &$matches = null, …)`:
        // an int >= 1 writes every column non-empty, `0` writes empty columns,
        // `false` writes nothing (issue #168; measured, see the doc above).
        ("preg_match_all", 2) => Some(WrittenWhen::ReturnTruthy),
        _ => None,
    }
}

/// Whether argument `position` of the builtin `name` is passed **by value** —
/// the ADR-0070 argument-semantics question, three-valued.
///
/// * `Some(true)` — certified by value. PHP copies the argument into the
///   parameter (copy-on-write for strings and arrays), so the callee cannot
///   reach the caller's binding through it, whatever it does to the parameter.
/// * `Some(false)` — a certified **by-reference** position: the parameter is an
///   alias of the caller's lvalue and the call may rewrite it.
/// * `None` — the catalog does not know this name's argument semantics. The
///   consumer must assume the worst; a name nobody knows is not a by-value
///   promise.
///
/// # How the two legs compose
///
/// An [`out_params`] row is transcribed from the php-src stubs *per name* and
/// lists every fixed positional reference parameter that name has, so for a name
/// carrying a row the row is complete and every other position is by value —
/// that is what makes `preg_match($re, $s, $m)` answer `true` for `$s` and
/// `false` for `$m` off a single table.
///
/// Absence of a row is **not** a by-value statement: the row set is deliberately
/// restricted (the variadic-by-ref family `sscanf`/`fscanf`/`array_multisort`
/// is absent by design, and the table only ever aimed to cover the names the
/// effect layer colors). So a name with no row must be *positively certified*
/// below, and everything else answers `None`.
///
/// # Membership of the certified set
///
/// Certification means that at `PINNED_PHP` every parameter is declared by
/// value in the php-src stub. The set covers names used by inference:
///
/// * the folding allowlist ([`foldable`]), which is pure by construction, plus
/// * the ADR-0062/0064 array read-position and shape-projection family that does
///   **not** carry an out-param row (`array_first`/`array_last`/`array_values`/…,
///   including `array_slice`; `current` and `key` take `array|object $array`,
///   while their pointer-moving siblings
///   `reset`/`end`/`next`/`prev` take `&$array` and are rowed above — the two
///   tables corroborate each other, as do `array_slice` and its rowed splicing
///   sibling `array_splice`), plus
/// * the alias spellings of foldable names (`chop`, `join`, `sizeof`), which are
///   the same C function under a second name, plus
/// * the **string-producer family's non-foldable members** (issue #41):
///   `addcslashes`, `escapeshellarg`, `escapeshellcmd`, `htmlspecialchars`,
///   `htmlentities`, `vsprintf`. Each is a member of the string-predicate
///   transfer table (or its recorded refusal) whose absence here was measured
///   as the family's dominant precision loss — see the note on the constant,
///   plus
/// * the **`mb_*` string family** (issue #41), which is excluded from
///   [`foldable`] for the determinism of its *result* and is nevertheless
///   all-by-value in its *arguments* — two independent questions, and only the
///   second one is this table's.
///
/// Widening this set is deliberately a separate act with its own measurement
/// run: every added name is a new premise for every kept fact downstream.
#[must_use]
pub fn by_value_arg(name: &str, position: usize) -> Option<bool> {
    /// Certified all-by-value names outside the folding allowlist. See the
    /// membership rules above; each is transcribed from the `PINNED_PHP` stub.
    const CERTIFIED_EXTRA: &[&str] = &[
        // Alias spellings of foldable names.
        "chop",     // = rtrim
        "join",     // = implode
        "sizeof",   // = count
        // The read-position family's non-mutating members (PHP 8.5 for the
        // first pair): `array_first(array $array)`, `array_last(array $array)`.
        "array_first",
        "array_last",
        "array_key_first",
        "array_key_last",
        // `current(array|object $array)` / `key(array|object $array)` — by value
        // since PHP 8.0; their `&$array`-taking siblings are the `out_params`
        // rows `reset`/`end`/`next`/`prev`.
        "current",
        "key",
        // The shape-projection family (ADR-0062): all take `array $array` by
        // value and return a new array.
        "array_values",
        "array_keys",
        "array_flip",
        "array_reverse",
        // `array_slice(array $array, int $offset, ?int $length = null,
        // bool $preserve_keys = false)` is entirely by value at `PINNED_PHP`;
        // sibling `array_splice` takes `&$array` and has an `out_params` row.
        "array_slice",
        // ---- The string-producer family's non-foldable members (issue #41) ----
        //
        // Every other member of the string-predicate transfer table is already
        // certified through [`foldable`]; these six are the family's whole
        // uncertified remainder, and leaving them uncertified was measured as
        // the wave's dominant precision loss rather than a theoretical one. An
        // uncertified name makes ADR-0070's survival gate condemn every variable
        // the call is handed, and that drop takes the **declared-arm lane** with
        // it (`Store::unbind`) — so a single `escapeshellarg($s)` erased the
        // `@param non-empty-string` premise of *every later statement in the
        // scope*, and the transfers below it declined for want of a subject
        // fact. In phpstan-src's `non-empty-string.php` one such call at line 319
        // silenced the ~70 assertions that follow it.
        //
        // Certification is the reflected declaration at `PINNED_PHP`
        // (`ReflectionFunction::getParameters`, 8.5.9, no parameter reports
        // `isPassedByReference`), verbatim:
        //
        //   addcslashes(string $string, string $characters): string
        //   escapeshellarg(string $arg): string
        //   escapeshellcmd(string $command): string
        //   htmlspecialchars(string $string, int $flags = …, ?string $encoding = null,
        //                    bool $double_encode = true): string
        //   htmlentities(string $string, int $flags = …, ?string $encoding = null,
        //                bool $double_encode = true): string
        //   vsprintf(string $format, array $values): string
        //
        // `escapeshellcmd` is here despite the transfer table *refusing* it
        // (`escapeshellcmd("\x80") === ''`): the two questions are independent —
        // refusing to describe a name's RESULT says nothing about whether the
        // call can reach the caller's binding, and it cannot.
        "addcslashes",
        "escapeshellarg",
        "escapeshellcmd",
        "htmlspecialchars",
        "htmlentities",
        "vsprintf",
        // ---- The `mb_*` string family (issue #41) ----------------------------
        //
        // These are the catalog's standing **fold** exclusion (see the "Deliberate
        // exclusions" note: their RESULT depends on the internal encoding and, for
        // the case pair, on a Unicode table that is not the byte-wise ASCII
        // mapping Steins' predicates describe). That exclusion says nothing about
        // their ARGUMENT semantics, which is this table's only question — and
        // conflating the two was measured as costly: one `mb_strtolower($s)` in a
        // string-heavy scope condemned every refinement that followed it
        // (phpstan-src's `non-empty-string.php` lines 327-330 silenced the ~70
        // assertions below them).
        //
        // Reflected at `PINNED_PHP` (8.5.9), every parameter by value; the last
        // five are the 8.4+ additions, absent on older engines and harmless here
        // (a name the engine does not have is never called):
        //
        //   mb_strtolower/mb_strtoupper(string $string, ?string $encoding = null)
        //   mb_substr(string $string, int $start, ?int $length = null, ?string $encoding = null)
        //   mb_strlen/mb_strwidth(string $string, ?string $encoding = null)
        //   mb_convert_case(string $string, int $mode, ?string $encoding = null)
        //   mb_convert_kana(string $string, string $mode = "KV", ?string $encoding = null)
        //   mb_str_split(string $string, int $length = 1, ?string $encoding = null)
        //   mb_str_pad(string $string, int $length, string $pad_string = " ",
        //              int $pad_type = STR_PAD_RIGHT, ?string $encoding = null)
        //   mb_strpos(string $haystack, string $needle, int $offset = 0, ?string $encoding = null)
        //   mb_substr_count(string $haystack, string $needle, ?string $encoding = null)
        //   mb_convert_encoding(array|string $string, string $to_encoding,
        //                       array|string|null $from_encoding = null)
        //   mb_check_encoding(array|string|null $value = null, ?string $encoding = null)
        //   mb_detect_encoding(string $string, array|string|null $encodings = null,
        //                      bool $strict = false)
        //   mb_ucfirst/mb_lcfirst(string $string, ?string $encoding = null)
        //   mb_trim/mb_ltrim/mb_rtrim(string $string, ?string $characters = null,
        //                             ?string $encoding = null)
        //
        // `mb_internal_encoding` is deliberately ABSENT: its argument is by value
        // too, but it is the one member that writes process-global state, and the
        // certification is read by a gate about *keeping facts across a call* —
        // leaving it uncertified costs nothing and states the asymmetry.
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
        // A transcribed row states this name's by-ref positions exhaustively.
        Some(positions) => Some(!positions.contains(&position)),
        // No row: the name itself must be certified.
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
/// diagnostic; typo safety is Steins' own job.
///
/// It is the union of every label the catalog can color a builtin with
/// ([`effect_labels`]) and the core taxonomy roots/parents of ADR-0018. Ecosystem
/// and private labels (`io.redis`, `email.send`) are **not** here: they are the
/// *builtin* set, and a plugin opens the registry beside it rather than inside it
/// — see [`LabelRegistry`], which is what inference actually asks.
#[must_use]
pub fn known_labels() -> &'static [&'static str] {
    BUILTIN_LABELS
}

/// The **core taxonomy roots** of ADR-0018 — the label roots Steins itself owns.
///
/// A plugin may register *descendants* of these (`io.redis`, `io.db.dynamo`), which
/// is why descendants are the recommended spelling for anything transport-like:
/// subsumption then works with no new machinery. A **new root** must instead equal
/// the plugin's composer vendor name (ADR-0068 §2); this list is what the
/// vendor-root rule checks the other side of.
///
/// `global` is a root even though only its `global.read` and `global.write`
/// children are registry entries; root ownership applies to the namespace.
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
    // Kept sorted for readability; the taxonomy of ADR-0018 plus every label used
    // in `effect_labels` coloring (all of which are already taxonomy nodes).
    &[
        "exit",
        // Failure-cause provenance family (ADR-0042): the benevolent-union
        // replacement. These label a `false`/`null` failure arm's *value
        // provenance* (why the arm exists), not an effect; they share the ADR-0018
        // registry so prefix subsumption (`failure` admits `failure.environment`)
        // works. See [`failure_arms`].
        "failure",
        "failure.environment",
        "failure.input",
        "failure.resource",
        // Opaque native boundary (php-src FFI): runs arbitrary C, so the catalog
        // can prove nothing about it — a deliberately top-level escape hatch
        // beside `exit`/`mutate` (effects_gaps.md §3). FFI is OO-only, so no plain
        // builtin is colored `ffi`; the label permits an `@effects ffi`
        // envelope declaration.
        "ffi",
        "global.read",
        "global.write",
        "io",
        "io.db",
        "io.fs",
        "io.fs.read",
        "io.fs.write",
        // Ambient *input* channel (ADR-0083): `php://input`, `php://stdin` — the
        // script's inbound stream, symmetric with `io.output`. Produced by
        // [`narrowed_stream_labels`] at a call site that names one of those
        // targets (issue #318); no argument-blind row carries it, because
        // recognizing this channel is exactly a question about the argument.
        // `$_GET`-style parsed-memory reads stay `global.read`.
        "io.input",
        // System-V / shared-memory IPC (effects_gaps.md §4): cross-process shared
        // state, neither filesystem nor network.
        "io.ipc",
        "io.net",
        "io.net.http",
        // Ambient *output* channel (ADR-0083), an `io` child like the opened
        // resources beside it. Its own children split on the one question a future
        // effect masking has to answer — can `ob_start()` capture this? — so the
        // masking rule stays a single prefix test against `io.output.buffer`.
        "io.output",
        // OB-layer output: `echo`, `print`, `printf`, inline HTML, `php://output`,
        // `flush`, `ob_flush`. The only family an `ob_start()` guard could ever
        // deduct.
        "io.output.buffer",
        // HTTP response-header mutation (effects_gaps.md §2): response metadata,
        // outside OB's reach (the old `output.header`).
        "io.output.header",
        // Process-fd writes, which OB cannot touch: `php://stderr`, `STDERR`.
        "io.output.stderr",
        // As `io.output.stderr`: `php://stdout`, `fwrite(STDOUT, …)`.
        "io.output.stdout",
        "io.process",
        // Signal delivery/handling (pcntl/posix; effects_gaps.md §1): an
        // observable OS interaction, parallel to `io.process`.
        "io.signal",
        "mutate",
        // By-ref out-parameter write landing in a binding of the *calling* frame
        // (ADR-0063 §2.3): `preg_match($p, $s, $matches)`, `sort($localArray)`.
        // The degenerate member of the `mutate` family — nothing escapes the
        // caller, so no observer outside the frame can tell it happened, which is
        // why every envelope tolerates it (see `steins-infer`'s `exceeds`). It
        // still earns a label for annotate/summary output. Non-local targets
        // stop at parent `mutate` rather than guessing a caller-observable child
        // (`mutate.arg`/`.self`/`.instance`/`.static`; ADR-0055 point 1).
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

/// A label spelling this project has **retired**, paired with what to write in its
/// place. Vocabulary knowledge, so it lives beside the registry rather than in a
/// diagnostic: both the attribute check and the interop one read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetiredLabel {
    /// The retired spelling, exactly as code that has not migrated still writes it.
    pub spelling: &'static str,
    /// What to write instead, as a diagnostic spells it after "write …" — prose,
    /// because a retirement can fan a single old name out over several new ones.
    pub guidance: &'static str,
}

/// Every label spelling Steins has retired, with its replacement guidance — the
/// table [`retired_label`] looks up.
///
/// **A row is appended here whenever a taxonomy node moves or is renamed** — that
/// is the table's contract, and the reason it exists at all: the Levenshtein
/// suggestion of [`nearest_label`] cannot reach a renaming that moved a label more
/// than two edits, and a migration is exactly where a project's docblocks are most
/// likely to still name the old node. The first two rows are ADR-0083's, which
/// moved the ambient output channel under `io` (`output` → `io.output.*`): those
/// are distance 3, past the cap, so without this table a project on the old
/// vocabulary is told nothing at all.
const RETIRED_LABELS: &[RetiredLabel] = &[
    // ADR-0083 split the old `output` root over three children on the one question
    // an `ob_start()` guard has to answer, so there is no single replacement to
    // name — the guidance walks the reader through the choice instead.
    RetiredLabel {
        spelling: "output",
        guidance: "io.output.buffer for echo-shaped code, io.output.header for \
                   header()/setcookie(), or the umbrella io.output",
    },
    // The one old spelling that does have an exact replacement (ADR-0083).
    RetiredLabel { spelling: "output.header", guidance: "io.output.header" },
];

/// The retirement row for `label`, if this project retired that spelling. Exact
/// match, like the registry's own lookups.
#[must_use]
pub fn retired_label(label: &str) -> Option<&'static RetiredLabel> {
    RETIRED_LABELS.iter().find(|r| r.spelling == label)
}

/// Why an unrecognized label reads as an **attempt at a label** rather than as a
/// human's prose ([`LabelRegistry::label_intent`]).
///
/// The variants are the evidence, in the order it is weighed. The first two carry
/// something to suggest; the last two are evidence of intent with no replacement to
/// name, so a diagnostic built on them says what happened and stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelIntent<'a> {
    /// The token is a spelling this project retired — the strongest signal there
    /// is, since Steins itself once printed that name.
    Retired(&'static RetiredLabel),
    /// The token is within [`LabelRegistry::nearest`]'s edit cap of a known label,
    /// which is also the suggestion to print.
    Near(&'a str),
    /// Some *other* member of the same tag's label list is a recognized label.
    /// Prose does not usually sit in a comma list beside a real effect label.
    KnownSibling,
    /// The token has two or more dot-path segments, the shape a one-word English
    /// note cannot take.
    DotPath,
}

/// The label registry **as one run sees it**: the builtin table ([`known_labels`])
/// plus whatever the ADR-0012/0039 plugin channel registered for this project
/// (ADR-0068). Inference asks this, not the free functions, so an ecosystem label
/// a plugin registered stops earning `effect.unknown-label` without the builtin
/// table growing a single ecosystem row.
///
/// [`LabelRegistry::builtin`] is the closed view, and it is the default: every
/// caller that has no project in hand (a single-file check, a unit test, the
/// browser) gets the builtin-only answers. Extension labels are validated *before*
/// they arrive here — the vendor-root rule of ADR-0068 §2 is a load-time gate in
/// the discovery layer, not a property this type re-derives.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LabelRegistry {
    /// Registered extension labels, sorted and deduplicated so two runs that
    /// discovered the same plugins compare equal (a salsa input's requirement).
    extensions: Vec<String>,
}

impl LabelRegistry {
    /// The builtin-only registry — the closed set, and what every caller without a
    /// plugin channel wants.
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

    /// Whether this registry has no extensions — i.e. it answers exactly as the
    /// free functions do.
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

    /// Whether an unrecognized `label`, written in a tag whose whole label list is
    /// `siblings`, carries evidence of **label intent** — and if so, which
    /// (issue #311).
    ///
    /// `None` is the answer that matters: a bare word far from every known label,
    /// alone in its list, is indistinguishable from the one-word note current
    /// PHPStan lets a docblock carry after `@phpstan-impure`, and guessing "it is a
    /// label" is exactly what the ADR-0082 amendment refuses. `None` therefore
    /// means *stay silent*, permanently and on every surface — not "report it
    /// somewhere quieter".
    ///
    /// Answering `Some` for a *known* label would be meaningless, so callers filter
    /// those out first; this method does not re-derive [`Self::is_known`] for the
    /// label itself, only for its siblings.
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
/// hierarchy ([`builtin_class_supers`]); ADR-0043 §5 keeps throw-catalog scope
/// separate from is-a ingestion. A test (`exception_parent_agrees_with_generated_hierarchy`) proves
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
/// `6bc7c26cf6…`, cross-checked vs PHP 8.5.8), generated into the private
/// `hierarchy_generated::HIERARCHY` table by `cargo xtask gen-catalog` from
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

/// The number of rows in the generated hierarchy table (ADR-0054 §9.6's Catalog
/// section "freshness context" — a plain count, not a fact about any project).
/// Reading `hierarchy_generated::HIERARCHY.len()` through a named accessor rather
/// than exposing the table itself keeps the generated module private, per its own
/// doc comment ("consulted only by `builtin_class_supers`").
#[must_use]
pub fn hierarchy_entry_count() -> usize {
    hierarchy_generated::HIERARCHY.len()
}

/// The casing php-src **declares** a builtin class/interface/enum with (`gmp` →
/// `GMP`, `hashcontext` → `HashContext`), or `None` for a name the mining data
/// does not declare — mined from the same `hierarchy.toml` pin as
/// [`builtin_class_supers`], so the two tables cannot drift apart.
///
/// **Display fidelity only.** `ContractTy::Class` case-folds on the way in —
/// that is what makes the countersign's `class_eq` comparison work — so by the
/// time a class name reaches a rendering surface its source casing is gone, and
/// the project index cannot recover it for a class no project file declares.
/// This table closes exactly that gap (the ADR-0069 third-amendment residual:
/// `dumpType(gmp_init($x))` read `gmp` where PHPStan reads `GMP`). No judgment
/// may consult it: everything downstream compares case-insensitively, and a
/// consumer that decided on casing would be deciding on nothing.
///
/// Matching is case-insensitive and a leading backslash is stripped, as in
/// [`builtin_class_supers`]; namespaced builtins are resolved the same way
/// (`ffi\cdata` → `FFI\CData`). **Enums are present here** even though the
/// hierarchy table skips them: that exclusion guards the is-a oracle against an
/// incomplete super-edge set, and a display name has no such soundness gate.
#[must_use]
pub fn builtin_class_display(name: &str) -> Option<&'static str> {
    let key = name.trim_start_matches('\\').to_ascii_lowercase();
    display_names_generated::DISPLAY_NAMES
        .binary_search_by(|(n, _)| (*n).cmp(key.as_str()))
        .ok()
        .map(|i| display_names_generated::DISPLAY_NAMES[i].1)
}

/// The **measured/curated** throw facts of a builtin call (ADR-0040 source #2):
/// the global class names a builtin provably raises. Deliberately tiny and
/// hand-verified — an uncatalogued builtin simply contributes no throw fact
/// (widen, never a false positive). An empty list means catalogued-but-throwless.
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
        // JSON_THROW_ON_ERROR; without flag inspection this synthetic key stays
        // uncatalogued in real calls. (The plain `json_decode` key above carries its
        // *unconditional* `$depth`-misuse ValueError, a separate arm.)
        "json_decode_throwing" | "json_encode_throwing" => Some(JSON),
        _ => None,
    }
}

/// The **cause** of a builtin's `false`/`null` failure arm (ADR-0042): a fact the
/// catalog can state, never a probability it cannot. Each maps to a `failure.*`
/// value-provenance label ([`known_labels`]) for boundary-profile must-check
/// policy (default exempts [`Resource`] and includes [`Environment`]; strict
/// includes both), replacing ADR-0030's erased benevolent union.
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
/// This is behavior-neutral catalog data until consumed by ADR-0037 boundary
/// profiles. Its shape is a per-call cause set plus the sentinel exclusion.
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
/// Reserved for value-level folding; effects/throws joining uses only
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

/// The [`InvocationShape`] of a higher-order builtin, or `None` when the function
/// is not a known higher-order invoker (its callback argument, if any, stays an
/// opaque taint — the FP-safe side).
///
/// Matching is case-insensitive (PHP function names are). Rows follow ADR-0033.
/// Argument-order quirks make this a table rather than a rule:
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
///
/// # Immediately invoked rows (ADR-0063 P1)
///
/// This callback-position catalog drives the higher-order effect join: a row
/// asserts that the named position is
/// *immediately invoked* during the call, so the callback's inferred envelope is
/// part of this call's effect. Each row below is here because PHP evaluates the
/// callback inside the call, before it returns:
///
/// * `array_find`/`array_find_key`/`array_any`/`array_all` (PHP 8.4) — callback
///   at 1 over param 0's elements; the search predicate runs during the scan.
///   (Short-circuiting does not change *whether* it runs, only how often — the
///   effect join is a may-analysis, so one possible invocation is enough.)
/// * `array_walk_recursive($arr, $cb)` — callback at 1, immediate, like
///   `array_walk`; `arg_source` is `None` rather than `ElementsOf(0)` because the
///   callback sees the *leaves* of the nested array, not param 0's own elements
///   (effects join either way; the fold path must not be lied to).
/// * `iterator_apply($it, $cb, $args)` — callback at 1, immediate; the callback
///   is called once per iteration during the call, with `$args`, so `None`.
///
/// # Deliberate exclusions
///
/// A builtin that takes a callable but is **not** given a row contributes no
/// callback effects; the exclusion is the honest answer, not an oversight:
///
/// * `set_error_handler`, `set_exception_handler`, `spl_autoload_register`,
///   `register_tick_function`, `header_register_callback`, `ob_start` — the
///   callable is *stored* and invoked later by the engine (on an error, an
///   unresolved class, a tick, a flush), not during the call. They are the
///   `register_shutdown_function` family: **not immediately invoked**. The one
///   existing `Deferred` row (`register_shutdown_function`, ADR-0033) remains;
///   other non-immediate positions contribute nothing.
/// * `preg_replace_callback_array($patternsToCallbacks, $subj)` — the callables
///   are *values inside* an associative array at position 0, not a positional
///   callback argument. [`InvocationShape::callback_param`] cannot name them and
///   the consumer's callback resolution is positional, so a row would be a lie.
/// * `array_udiff`/`array_uintersect`/`array_udiff_assoc`/`array_diff_ukey`/
///   `array_intersect_ukey`/`array_udiff_uassoc`/`array_uintersect_uassoc` — the
///   comparator(s) are immediately invoked, but they sit in the **last** (and for
///   the double-`u` forms, last *two*) positions of a variadic argument list.
///   `callback_param` is a fixed index and cannot express "last"; widening the
///   shape type for these is deferred rather than approximated wrongly.
/// * `usleep`-style and every non-callable builtin — no callback at all.
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
        // PHP 8.4 array search predicates — cb at 1 over param 0's elements.
        "array_find" | "array_find_key" | "array_any" | "array_all" => {
            shape(1, Immediate, ElementsOf(0))
        }
        // Leaves, not top-level elements → no element source (see the doc above).
        "array_walk_recursive" => shape(1, Immediate, NoSrc),
        "iterator_apply" => shape(1, Immediate, NoSrc),
        _ => None,
    }
}

/// The **curated return-fact refinement** of a builtin `name` (ADR-0056 §1.2): a
/// phpdoc type string (`"int<0, max>"`, `"non-empty-string"`) that narrows
/// strictly within the builtin's reflected return envelope, or `None` when no row
/// curates it (the common case — the reflected envelope then stands alone).
///
/// This is only a *refinement proposal*: the consumer (steins-infer) admits it at
/// a call site solely after confirming it is an extensional subset of the reflected
/// envelope AND the project PHP minor equals [`PINNED_PHP`] (ADR-0056 §2). A stale
/// row can therefore lose precision, never manufacture a wrong premise.
///
/// The table (`return_facts_generated::RETURN_FACTS`) is generated from
/// `return_facts.toml`. The bool-predicate family has no rows because its
/// reflected envelope is already `bool`, so no refinement adds precision. Matching is
/// case-insensitive; the generated keys are lowercased and sorted for binary search.
#[must_use]
pub fn return_fact(name: &str) -> Option<&'static str> {
    let key = name.trim_start_matches('\\').to_ascii_lowercase();
    return_facts_generated::RETURN_FACTS
        .binary_search_by(|(n, _)| (*n).cmp(key.as_str()))
        .ok()
        .map(|i| return_facts_generated::RETURN_FACTS[i].1)
}

/// Whether the builtin `name` returns a legacy PHP **resource**, and whether its
/// return carries a `false` failure arm (ADR-0056 §8). `Some(true)` is
/// `resource|false`, `Some(false)` is a bare `resource`, `None` is every other
/// builtin.
///
/// # This answer is a proposal, and two of its three conditions are not here
///
/// `resource` is the one type PHP cannot spell in a declaration, so the reflected
/// envelope that anchors every other return fact (ADR-0056 §1) is structurally
/// unavailable — `fopen` declares nothing and never will. The row below is
/// condition 1 of §7's gate, the php-src stub reading at the pin. The consumer
/// (steins-infer) supplies the other two before it seeds anything:
///
/// * **the tripwire** — the analyzing engine must declare NO return type for this
///   name. PHP 8 migrated most of the resource world to objects, and an engine
///   that answers `CurlHandle|false` has *disowned* the row; curation yields to
///   it, exactly as §1 requires. This is what keeps the 89 rotted `functionMap`
///   names (ADR-0069 §5) out without a hand-maintained denylist, and what will
///   switch a row off by itself the day its function migrates.
/// * **the minor pin** — the project PHP minor must equal [`PINNED_PHP`], the
///   same version gate [`return_fact`]'s refinements pass through.
///
/// Matching is case-insensitive and backslash-trimmed, as everywhere else in this
/// crate; the generated keys are lowercased and sorted for binary search.
#[must_use]
pub fn resource_return(name: &str) -> Option<bool> {
    let key = name.trim_start_matches('\\').to_ascii_lowercase();
    resource_returns_generated::RESOURCE_RETURNS
        .binary_search_by(|(n, _)| (*n).cmp(key.as_str()))
        .ok()
        .map(|i| resource_returns_generated::RESOURCE_RETURNS[i].1)
}

/// The **declared return type** of a builtin `name` (ADR-0069, issues #73/#79): the
/// canonical phpdoc spelling of the type the builtin declares (`"string"`,
/// `"string|false"`, `"non-empty-string"`), or `None` when no row covers it.
///
/// This is the bottom rung of the return ladder and nothing more. It exists for the
/// runs where every other rung is engine-gated and a builtin call with variable
/// operands types as `unknown`.
///
/// Three properties are load-bearing, and each is enforced somewhere other than
/// here so it cannot be forgotten at a call site:
///
/// * **Asserted, never Verified.** The consumer seeds the fact at the `Asserted`
///   stratum, so the proof layer's all-Verified premise rule keeps every finding
///   off it by construction. A wrong row can mislead a dump; it cannot mint a
///   finding.
/// * **Any engine answer wins — per name, not per run.** The consuming rung sits
///   strictly below the sidecar-backed reflected envelope and fires exactly where
///   that envelope is `None` for the asked name. `--no-php` (and the browser before
///   php-wasm loads) is only the total case; with a live engine the floor still
///   speaks where the engine is *silent* — a name whose extension the analyzing PHP
///   does not load, or a builtin with no declared return type. Where the engine
///   answers, the floor never overrides it.
/// * **Never an existence answer.** The absence family reads the boot surface, never
///   this table: a static table answering `function_exists` is a false-absence FP
///   factory. An absence finding standing beside a floor fact is complementary —
///   the call fails on the analyzing PHP, and this is the shape it declares where
///   it does exist.
///
/// The rows are mined from PHPStan's `resources/functionMap.php` at a pinned
/// commit — itself inherited from Phan; see the root `NOTICE` — filtered to types
/// whose lowering flattens to an arm list the declared-contract lane carries, and
/// each one countersigned at generation time, arm-wise, against the pinned engine's
/// own reflection. Matching is case-insensitive and a leading `\` is stripped, as
/// everywhere else in this crate.
///
/// Values may use the full scalar-arm vocabulary, including a `T|false` failure
/// union or a refinement (`non-empty-string`, `non-negative-int`) as well as a
/// bare base. Every value remains Asserted and never a proof premise.
#[must_use]
pub fn declared_return(name: &str) -> Option<&'static str> {
    let key = name.trim_start_matches('\\').to_ascii_lowercase();
    declared_returns_generated::DECLARED_RETURNS
        .binary_search_by(|(n, _)| (*n).cmp(key.as_str()))
        .ok()
        .map(|i| declared_returns_generated::DECLARED_RETURNS[i].1)
}

/// The minor at which a builtin's declared **return type** last moved across the
/// supported 8.x line, or `None` when it never did (ADR-0069 §3, the A11-shaped
/// version discipline).
///
/// The functionMap delta files are the change oracle. A `Some((8, 2))` says: the
/// mined row states the type as of the pin, and that statement is only known good
/// for a project whose declared PHP target lies wholly at or above 8.2. The
/// consumer declines the row otherwise; an undeclared target admits it, because the
/// row is Asserted anyway.
///
/// Deliberately **independent** of [`declared_return`]: a name can be
/// version-sensitive without carrying an admitted row, and the gate must stay
/// complete either way. The sets overlap because ADR-0071 permits array return
/// floors; the version gate therefore applies to names carrying admitted rows.
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
    use super::{WIDTH_REFUSED, WIDTH_SAFE, effect_labels, foldable, width_safe};

    /// The allowlist is the union of the two width classes, so "every foldable
    /// name has a width verdict" is structural. What still needs pinning is that
    /// the two classes are DISJOINT (a name in both would be silently admitted on
    /// a 32-bit engine while the refused table claims otherwise), that no name is
    /// listed twice within a class, and that the size is the 46 the ADR-0066
    /// amendments tabulate.
    #[test]
    fn the_width_classes_partition_the_allowlist() {
        for name in WIDTH_SAFE {
            assert!(!WIDTH_REFUSED.contains(name), "{name} is classified twice");
            assert!(foldable(name), "{name} is classified but not foldable");
            assert_eq!(
                WIDTH_SAFE.iter().filter(|&n| n == name).count(),
                1,
                "{name} is listed twice in WIDTH_SAFE"
            );
        }
        for name in WIDTH_REFUSED {
            assert!(foldable(name), "{name} is classified but not foldable");
            assert_eq!(
                WIDTH_REFUSED.iter().filter(|&n| n == name).count(),
                1,
                "{name} is listed twice in WIDTH_REFUSED"
            );
        }
        assert_eq!(WIDTH_SAFE.len(), 37, "the verified width-safe subset");
        assert_eq!(WIDTH_REFUSED.len(), 9, "the refused rows");
        assert_eq!(
            WIDTH_SAFE.len() + WIDTH_REFUSED.len(),
            46,
            "the allowlist size the ADR-0066 amendments tabulate"
        );
    }

    /// The nine refused rows, named. Each is a *silent* value divergence on a
    /// 32-bit engine — see `WIDTH_REFUSED` for the verbatim probes.
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
            "ABS",
            "IntVal",
            "SPRINTF",
            "DecHex",
            "Version_Compare",
        ] {
            assert!(!width_safe(name), "{name} must not be certified width-safe");
            assert!(foldable(name), "{name} is refused on width, not off the allowlist");
        }
        // …and the certification is real, not vacuous.
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
        ] {
            assert!(width_safe(name), "{name} is a verified width-safe fold");
        }
    }

    /// The issue-#78 admissions, spelled out: every new name is on the allowlist
    /// AND carries the empty effect set, which is the `foldable` fallthrough in
    /// [`effect_labels`] doing its job — no second table to keep in step.
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

    /// The name accessors equal the predicate extensions, so the boundary widget's
    /// displayed subsets cannot drift from the gate (issue #64).
    #[test]
    fn the_name_accessors_agree_with_the_predicates() {
        use super::{width_refused, width_refused_names, width_safe_names};
        assert_eq!(width_safe_names(), WIDTH_SAFE);
        assert_eq!(width_refused_names(), WIDTH_REFUSED);
        for name in width_safe_names() {
            assert!(width_safe(name), "{name} is listed safe but the predicate declines it");
        }
        for name in width_refused_names() {
            assert!(!width_safe(name), "{name} is listed refused but the predicate admits it");
            assert!(width_refused(name), "{name} is listed refused but is not in the complement");
            assert!(foldable(name), "a refused name is still on the folding allowlist");
        }
        assert_eq!(width_safe_names().len(), 37);
        assert_eq!(width_refused_names().len(), 9);
    }

    /// Default-deny: a name without a width classification is not width-safe.
    /// This roster remains populated to exercise the unclassified case.
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

    /// The refusals that are **not** width rows (issue #78). A `WIDTH_REFUSED` entry
    /// is still `foldable`, so a name that fails ADR-0008's purity/determinism bar
    /// cannot be written as one — it has to be absent from both tables, and that
    /// absence is what this pins. See the module docs for each name's evidence.
    #[test]
    fn impure_and_locale_sensitive_are_excluded() {
        for name in [
            "mb_strtolower",     // encoding-dependent
            "mb_strlen",         // encoding-dependent (and absent from the wasm build)
            "mb_substr",         // encoding-dependent
            "time",              // nondet
            "rand",              // nondet
            "setlocale",         // global-write
            "file_get_contents", // io
            "printf",            // io.output.buffer
            "date",              // global-read (timezone) + nondet
            "strtotime",         // nondet.time, timezone-coupled
            "idate",             // timezone-coupled even with an explicit timestamp
            "strcmp",            // magnitude is memcmp's, implementation-defined
            "strcasecmp",        // as strcmp
            "number_format",     // held out with the mb_* family (issue #78)
            "bin2hex",           // standing ADR-0056 refused row, not reopened here
        ] {
            assert!(!foldable(name), "{name} must not be foldable");
            assert!(!width_safe(name), "{name} must not be certified width-safe");
        }
    }

    #[test]
    fn colored_builtins_carry_their_label() {
        assert_eq!(effect_labels("rand"), Some(&["nondet.random"][..]));
        assert_eq!(effect_labels("time"), Some(&["nondet.time"][..]));
        // Every filesystem row is wrapper-capable, so all of them are the `io`
        // parent until a call site proves the target (issue #318 — see
        // `narrowed_stream_labels`). The stat-and-unlink family included: a
        // `ssh2.sftp://` path makes `unlink` a network write.
        assert_eq!(effect_labels("file_get_contents"), Some(&["io"][..]));
        assert_eq!(effect_labels("file_put_contents"), Some(&["io"][..]));
        assert_eq!(effect_labels("fopen"), Some(&["io"][..]));
        assert_eq!(effect_labels("scandir"), Some(&["io"][..]));
        assert_eq!(effect_labels("unlink"), Some(&["io"][..]));
        assert_eq!(effect_labels("file_exists"), Some(&["io"][..]));
        assert_eq!(effect_labels("mkdir"), Some(&["io"][..]));
        // The narrowing gives each of them its old precise row back.
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
        // Process-global state the catalog states precisely: the RNG generator
        // (seeding, not drawing) and PHP's stat cache.
        assert_eq!(effect_labels("srand"), Some(&["global.write"][..]));
        assert_eq!(effect_labels("mt_srand"), Some(&["global.write"][..]));
        assert_eq!(effect_labels("clearstatcache"), Some(&["global.write"][..]));
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
        for name in ["some_unknown_fn", "mysqli_query", "proc_open"] {
            assert_eq!(effect_labels(name), None, "{name} must be uncatalogued");
        }
    }

    #[test]
    fn return_facts_r3_r4_rows() {
        // The table contains int-range and refined-string families. Bool predicates
        // have no row because their reflected `bool` envelope cannot be refined.
        assert_eq!(super::return_fact("is_int"), None);
        assert_eq!(super::return_fact("some_unknown_fn"), None);
        // `int<0, max>` refines the reflected `int` envelope.
        for name in ["count", "sizeof", "strlen", "mb_strlen", "substr_count", "func_num_args", "array_push", "array_unshift"] {
            assert_eq!(super::return_fact(name), Some("int<0, max>"), "{name} must curate int<0, max>");
        }
        // `non-falsy-string` refines the reflected `string` envelope. Two
        // probe-verified rows are `get_debug_type`
        // (every return is a type keyword or a class name — PHP's label grammar forbids
        // a leading digit, so "0" is not nameable) and `spl_object_hash` (a fixed
        // 32-char lowercase hex digest; its `object` parameter has no empty-in path).
        for name in ["sha1", "md5", "uniqid", "get_debug_type", "spl_object_hash"] {
            assert_eq!(super::return_fact(name), Some("non-falsy-string"), "{name} must curate non-falsy-string");
        }
        // Refused rows carry no curated fact (argument-sensitive / multi-base).
        // `dirname` is refused: `dirname("0/x")==="0"` is falsy and
        // `dirname("")===""` is empty, so neither NON_FALSY nor NON_EMPTY holds.
        for name in
            ["abs", "bin2hex", "trim", "strtoupper", "preg_match_all", "str_word_count", "sha1_file", "dirname"]
        {
            assert_eq!(super::return_fact(name), None, "{name} is a refused row — no curated fact");
        }
        // Case-insensitive lookup and leading-backslash trimming both hit.
        assert_eq!(super::return_fact("COUNT"), Some("int<0, max>"));
        assert_eq!(super::return_fact("\\sha1"), Some("non-falsy-string"));
        // The generated table is well-formed (sorted for binary search).
        let t = super::return_facts_generated::RETURN_FACTS;
        assert!(t.windows(2).all(|w| w[0].0 < w[1].0), "RETURN_FACTS must be strictly sorted by key");
    }

    #[test]
    fn return_facts_dr4_refined_string_rows() {
        // These two `non-falsy-string` rows passed the three-leg probe gate at PHP
        // 8.5.8. Each has a single `string` reflected envelope, so the refinement
        // narrows strictly within it.
        //
        // `spl_object_hash` — a fixed 32-character lowercase hex digest
        // (5000-object sweep: distinct lengths=32, allhex, alllowercase, none falsy).
        // Its parameter is typed `object`, so the bin2hex empty-in/empty-out trap is
        // structurally unreachable: there is no empty input to produce an empty output.
        assert_eq!(super::return_fact("spl_object_hash"), Some("non-falsy-string"));
        // `get_debug_type` — every return is either a type keyword ('bool','int','float',
        // 'string','array','null','resource (stream)','resource (closed)', all >= 3 chars)
        // or a class/enum name. `get_debug_type("")` is 'string' and `get_debug_type("0")`
        // is 'string' — the value never leaks into the result — and PHP's label grammar
        // forbids a leading digit, so no class can be named "0" (class_exists("0") is false).
        assert_eq!(super::return_fact("get_debug_type"), Some("non-falsy-string"));
        // Both honour the shared lookup contract (case-insensitive, backslash-trimmed).
        assert_eq!(super::return_fact("SPL_OBJECT_HASH"), Some("non-falsy-string"));
        assert_eq!(super::return_fact("\\get_debug_type"), Some("non-falsy-string"));
    }

    #[test]
    fn return_facts_dirname_stays_refused() {
        // Probes refute `dirname(): non-falsy-string` twice, so `dirname` remains
        // refused.
        //
        //   (a) NOT non-falsy: a path segment can itself be "0", returned verbatim —
        //       dirname("0/x") === "0", a FALSY string (the census's contrary
        //       dirname("0") === "." only holds because "0" is there a bare basename).
        //   (b) NOT non-empty either: dirname("") === "" — the exact bin2hex
        //       empty-in/empty-out shape that refused bin2hex.
        //
        // Neither StrPreds refinement holds for all arguments, so the reflected `string`
        // envelope must stand alone. A row here would be a wrong premise — the ADR's
        // named FP channel (a curated fact "disproving" a correct docblock).
        assert_eq!(super::return_fact("dirname"), None);
        assert_eq!(super::return_fact("DIRNAME"), None);
        assert_eq!(super::return_fact("\\dirname"), None);
    }

    #[test]
    fn resource_returns_carry_the_stub_reading_and_nothing_else() {
        // The three bare-`resource` rows and the `resource|false` majority — the
        // `false` arm is READ from the stub, never assumed, because the guard
        // machinery downstream behaves differently for the two.
        assert_eq!(super::resource_return("fopen"), Some(true));
        assert_eq!(super::resource_return("tmpfile"), Some(true));
        assert_eq!(super::resource_return("stream_context_create"), Some(false));
        assert_eq!(super::resource_return("stream_context_get_default"), Some(false));
        assert_eq!(super::resource_return("stream_context_set_default"), Some(false));
        // The migration's other side. These once returned resources and now
        // return objects, so php-src's stubs no longer name `resource` and they
        // were never mined — the row is absent before §8.2's tripwire is even
        // consulted, which is the second belt the TOML header describes.
        for migrated in ["curl_init", "imagecreate", "finfo_open", "ldap_connect", "odbc_connect"] {
            assert_eq!(
                super::resource_return(migrated),
                None,
                "{migrated} returns an object on PHP 8 — it must not be a resource row",
            );
        }
        // Arrays OF resources are not resource rows (ADR-0056 §8.7): the arms are
        // an array and the element type has no carrier.
        assert_eq!(super::resource_return("stream_socket_pair"), None);
        assert_eq!(super::resource_return("get_resources"), None);
        // The shared lookup contract, as for every other table here.
        assert_eq!(super::resource_return("FOPEN"), Some(true));
        assert_eq!(super::resource_return("\\fopen"), Some(true));
        // Well-formed for binary search.
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
        // Asserted-floor rows (ADR-0069). `str_repeat` is the worked example.
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
        // A scalar refinement: functionMap states what
        // reflection cannot: `mb_strtoupper` never returns a lowercase character.
        assert_eq!(super::declared_return("mb_strtoupper"), Some("uppercase-string"));
        // ADR-0071 permits a bare array, list, keyed map, and full shape.
        assert_eq!(super::declared_return("array_merge"), Some("array"));
        assert_eq!(super::declared_return("str_split"), Some("list<string>"));
        // The stored spelling is the speller's, and an int range spells as the
        // interval PHPStan itself states (issue #90) — not the phpdoc keyword sugar.
        assert_eq!(super::declared_return("array_count_values"), Some("array<int<1, max>>"));
        assert_eq!(
            super::declared_return("imagecolorsforindex"),
            Some("array{alpha: int<0, 127>, blue: int<0, 255>, green: int<0, 255>, red: int<0, 255>}")
        );
        // Array arms are also permitted inside unions.
        assert_eq!(super::declared_return("scandir"), Some("false|list<string>"));
        // Class rows are admitted by the reflexive countersign. They keep
        // functionMap's own casing rather than a canonical respelling:
        // `spell_arms` has no faithful spelling for a class arm, so the row stores the
        // source string, which lowers back by construction and is the only place the
        // builtin's casing survives at all (`ContractTy::Class` case-folds).
        assert_eq!(super::declared_return("gmp_init"), Some("GMP"));
        assert_eq!(super::declared_return("date_diff"), Some("DateInterval"));
        assert_eq!(super::declared_return("hash_init"), Some("HashContext"));
        // A class arm paired with `null`, carriable per ARM and so needing no case of
        // its own; and a class arm paired with a scalar one.
        assert_eq!(super::declared_return("collator_create"), Some("?Collator"));
        assert_eq!(super::declared_return("simplexml_load_string"), Some("SimpleXMLElement|false"));
        // A namespaced builtin FQN, which is why the consuming resolver must be the
        // identity: `ast\Node` is already global, and a project-namespace resolver
        // would mangle it.
        assert_eq!(super::declared_return("ast\\parse_code"), Some("ast\\Node"));
        // PHPStan's own spelling of a plain union survives verbatim, because the
        // phpdoc parser expands `__benevolent<T1|T2>` to the union it wraps before
        // anything lowers it.
        assert_eq!(super::declared_return("curl_init"), Some("__benevolent<CurlHandle|false>"));
        // Case-insensitive lookup and leading-backslash trimming, as everywhere else.
        assert_eq!(super::declared_return("STRSTR"), Some("string|false"));
        assert_eq!(super::declared_return("\\str_repeat"), Some("string"));
        // A name nothing covers stays silent.
        assert_eq!(super::declared_return("some_unknown_fn"), None);

        let t = super::declared_returns_generated::DECLARED_RETURNS;
        assert!(t.windows(2).all(|w| w[0].0 < w[1].0), "DECLARED_RETURNS must be strictly sorted by key");
        for (name, ty) in t {
            assert!(!ty.is_empty(), "{name} carries an empty spelling");
        }
        // Pinned mining contract: 1,711 rows comprise 919 single-base envelopes and
        // 792 richer rows. Any count change indicates a generation or lowering change.
        let rich = t.iter().filter(|(_, ty)| !ENVELOPE_SPELLINGS.contains(ty)).count();
        assert_eq!(t.len(), 1711, "admitted rows at this pin");
        assert_eq!(t.len() - rich, 919, "the #73 envelope population must be preserved exactly");
        assert_eq!(rich, 792, "the #79, ADR-0071, object-slice and class-string (#236) rich admissions");
    }

    #[test]
    fn declared_return_excludes_what_the_engine_disowns() {
        // The generation-time reflection cross-check is the ADR-0069 §3 answer to
        // ADR-0014's silent-rot warning, and these are its live catches at the pin:
        // functionMap says `string`, the engine's own declaration says `void`
        // (`sodium_add`) or `?string` (`xml_error_string`, a null the row would have
        // hidden), or `int` against the engine's `string` (`pg_port`). Every one is
        // excluded and listed verbatim in `declared_returns.toml`.
        //
        // The arm-wise clause keeps these excluded: a row cannot drop the engine's
        // own arm merely because the remaining row is a refinement.
        for name in ["sodium_add", "sodium_increment", "xml_error_string", "pg_port", "imageinterlace"] {
            assert_eq!(super::declared_return(name), None, "{name} must stay excluded");
        }
        for name in ["intlcal_get", "socket_cmsg_space", "ldap_compare", "pg_last_notice"] {
            assert_eq!(super::declared_return(name), None, "{name}: the row drops an engine arm");
        }
        // Rich-row countersigning also excludes stale map entries. For example,
        // functionMap still says `int|false` where PHP 8 returns a `GdFont` object.
        for name in ["imageloadfont", "pow", "rewinddir", "substr_compare", "fpassthru"] {
            assert_eq!(super::declared_return(name), None, "{name}: an #79 candidate the engine disowns");
        }
        // Array candidates use the same dropped-arm check: `ftp_raw` says `array`
        // where the
        // engine declares `?array`, so the row hides a null exactly as
        // `xml_error_string` did; `mysqli_fetch_row` and `locale_get_keywords` hide
        // the engine's `false`. `str_word_count` invents one instead — functionMap
        // still carries a `false` arm PHP 8 replaced with a ValueError.
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
        // Class candidates expose stale resource-era rows.
        // `stream_bucket_make_writeable` is the sharpest example:
        // functionMap says the call returns a bare `stdClass`, where PHP 8 declares a
        // real `StreamBucket` — the stand-in outlived the thing it stood in for, and
        // the reflexive countersign refuses it because the two names simply differ.
        // The rest are the familiar dropped-arm shape wearing class names:
        // `intlcal_create_instance` and the four `tidy_get_*` rows hide the engine's
        // `null` exactly as `ftp_raw` hid one; `xmlwriter_open_uri` hides its `false`;
        // `dom_import_simplexml` drops the engine's `DOMAttr` arm AND invents a
        // `false`.
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
        // A constant-union row is refused for the same mechanical reason, and the
        // A CONSTANT name is not vocabulary, so `lower_identifier`'s catch-all
        // lowers it to a `Class` arm. The countersign keeps those rows out: the
        // engine declares `int`, no class name matches, and the row is listed.
        for name in ["json_last_error", "session_status"] {
            assert_eq!(super::declared_return(name), None, "{name}: constants are not class names");
        }
        // Alternate signatures that disagree on the return type exclude the name too:
        // a floor row must state ONE type.
        for name in ["base64_decode", "phpversion", "getenv"] {
            assert_eq!(super::declared_return(name), None, "{name} has disagreeing alternates");
        }
    }

    #[test]
    fn declared_return_version_sensitivity_is_recorded() {
        // The A11-shaped change oracle (ADR-0069 §3): `str_split` returned
        // `non-empty-list<string>` through 8.1 and `list<string>` from 8.2, so a row
        // for it is only known good at or above 8.2.
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
        // The tripwire fired, as ADR-0069's amendment predicted it would. All four
        // version-sensitive names return an array or a list; the arm lane could not
        // carry those through #73 and #79, so the tables were disjoint and the gate
        // had no end-to-end fixture. ADR-0071's array widening admits all four, so
        // this assertion is INVERTED: the tables now INTERSECT, and the gate is a
        // live decision rather than a wired-but-unreached one. The end-to-end fixture
        // the old assertion was designed to demand lives in steins-infer's
        // `declared_return_floor.rs` (`the_version_gate_declines_below_the_boundary`).
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
    fn display_names_answer_the_declared_casing() {
        use super::builtin_class_display as d;
        // The names the residual was pinned on (ADR-0069 third amendment).
        assert_eq!(d("gmp"), Some("GMP"));
        assert_eq!(d("hashcontext"), Some("HashContext"));
        assert_eq!(d("xmlparser"), Some("XMLParser"));
        assert_eq!(d("dateinterval"), Some("DateInterval"));
        // Case-insensitive, leading backslash stripped, namespaced keys resolved
        // — the same key discipline as `builtin_class_supers`.
        assert_eq!(d("GMP"), Some("GMP"));
        assert_eq!(d("\\DateInterval"), Some("DateInterval"));
        assert_eq!(d("ffi\\cdata"), Some("FFI\\CData"));
        // An all-lowercase declaration answers itself — the row states the
        // declared casing, not a beautification.
        assert_eq!(d("com"), Some("com"));
        // Enums ARE here, even though `builtin_class_supers` skips them: the
        // hierarchy exclusion guards the is-a oracle, not the display surface.
        assert_eq!(d("roundingmode"), Some("RoundingMode"));
        assert_eq!(super::builtin_class_supers("roundingmode"), None);
        // An unknown external stays unknown.
        assert_eq!(d("App\\GMP"), None);
        assert_eq!(d("nosuchclass"), None);
    }

    #[test]
    fn display_name_table_is_sorted_and_self_consistent() {
        // Sorted, or `binary_search_by` in `builtin_class_display` silently
        // misses entries; each key the lowercase of its value, or the lookup
        // and the answer describe two different classes. Guards regen drift.
        let t = super::display_names_generated::DISPLAY_NAMES;
        assert!(t.windows(2).all(|w| w[0].0 < w[1].0), "DISPLAY_NAMES must be strictly sorted");
        for &(key, name) in t {
            assert_eq!(key, name.to_ascii_lowercase(), "key must be the lowercased value");
        }
        // One mining source, two projections: every hierarchy key has a display
        // row (the converse cannot hold — enums are display-only by design).
        for &(key, _) in super::hierarchy_generated::HIERARCHY {
            assert!(
                super::builtin_class_display(key).is_some(),
                "hierarchy key `{key}` has no display row"
            );
        }
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
        assert_eq!(effect_labels("File_Put_Contents"), Some(&["io"][..]));
        assert_eq!(effect_labels("STRTOLOWER"), Some(&[][..]));
        // The narrowing table folds the function name's case the same way.
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
        // PHP folds case on class AND method names, so every spelling is one row.
        assert_eq!(method_effect_labels("pdo", "QUERY"), Some(&["io.db"][..]));
        assert_eq!(method_effect_labels("PdoStatement", "FetchAll"), Some(&["io.db"][..]));
    }

    #[test]
    fn uncatalogued_methods_stay_none() {
        // A real PDO method the table does not carry widens, it does not go pure:
        // silence is the only honest answer for a row that was never written.
        assert_eq!(method_effect_labels("PDO", "getAttribute"), None);
        assert_eq!(method_effect_labels("PDO", "beginTransaction"), None);
        // A class with no rows at all, and a same-named method on it.
        assert_eq!(method_effect_labels("mysqli", "query"), None);
        assert_eq!(method_effect_labels("Foo", "query"), None);
    }

    #[test]
    fn io_db_is_a_registered_label() {
        // The row would be unusable otherwise: `#[\Steins\Effect('io.db')]` must be
        // a valid declaration, and a coarse `io` must admit it.
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
        // Not subsumption: sibling, ancestor-of-envelope, and non-segment prefix.
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
        // Something wildly off has no near suggestion.
        assert_eq!(nearest_label("completely-different"), None);
        // The retired ADR-0083 spelling is *not* a near miss of its replacement
        // (`output` → `io.output` is distance 3, past the cap), so this metric can
        // say nothing about it — which is precisely why `RETIRED_LABELS` exists
        // beside it (issue #311) and why both checks consult that table first.
        assert_eq!(nearest_label("output"), None);
        assert_eq!(nearest_label("output.header"), None);
    }

    #[test]
    fn the_retired_table_carries_the_adr_0083_migration() {
        // Distance-3 renames the suggestion metric cannot reach: the table is the
        // only channel that tells a project on the old vocabulary what to write.
        let out = retired_label("output").expect("the retired output root");
        assert_eq!(out.spelling, "output");
        assert_eq!(
            out.guidance,
            "io.output.buffer for echo-shaped code, io.output.header for \
             header()/setcookie(), or the umbrella io.output"
        );
        assert_eq!(retired_label("output.header").map(|r| r.guidance), Some("io.output.header"));
        // Only retired spellings are in it: not a live label, not a typo, not prose.
        assert_eq!(retired_label("io.output"), None);
        assert_eq!(retired_label("io.netw"), None);
        assert_eq!(retired_label("database"), None);
        // Every replacement the table names must itself be a registry label, or the
        // guidance sends a reader from one unknown label to another.
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
        // The free function — the builtin-only view — is unmoved.
        assert!(!is_known_label("acme.cache"));
        // A typo of the extension is still unknown, and now suggests it.
        assert!(!r.is_known("acme.cach"));
        assert_eq!(r.nearest("acme.cach"), Some("acme.cache"));
        // Ancestor-of-an-entry is known (the taxonomy path), finer is not — the
        // same rule the builtin table follows.
        assert!(r.is_known("acme"));
        assert!(!r.is_known("acme.cache.hit"));
    }

    #[test]
    fn core_roots_are_the_ones_a_plugin_may_only_refine() {
        // ADR-0068 §2: descendants of these are open to any plugin.
        assert!(is_core_label("io.redis"));
        assert!(is_core_label("io"));
        assert!(is_core_label("global.write"));
        // A new root is not — that is what the vendor-name rule adjudicates.
        assert!(!is_core_label("acme.cache"));
        // ADR-0083 retired the `output` root; it is now just an unowned name.
        assert!(!is_core_label("output"));
        assert!(is_core_label("io.output.buffer"));
        assert!(!is_core_label("email.send"));
        // Segment-aware, like every other label predicate here.
        assert!(!is_core_label("iota.thing"));
    }

    #[test]
    fn new_effect_labels_are_registered_and_subsume() {
        // Labels from effects_gaps.md are known and prefix-subsume correctly.
        for label in ["ffi", "io.signal", "io.ipc", "io.output.header", "io.input"] {
            assert!(is_known_label(label), "{label} should be a known registry label");
        }
        assert!(subsumes("io", "io.signal"), "coarse io admits io.signal");
        assert!(subsumes("io", "io.ipc"), "coarse io admits io.ipc");
        assert!(
            subsumes("io.output", "io.output.buffer"),
            "coarse io.output admits io.output.buffer"
        );
        // The ADR-0083 meaning change, pinned at the registry: bare `io` is the
        // ambient channels' ancestor too, so an `io` envelope admits output.
        assert!(subsumes("io", "io.output.buffer"), "io admits the ambient output channel");
        assert!(subsumes("io", "io.input"));
        assert!(
            !subsumes("io.output.buffer", "io.output.header"),
            "headers are outside the OB-capturable family"
        );
        assert!(!subsumes("io.signal", "io.ipc"), "siblings do not subsume");
        // ffi is a top-level escape hatch, not under io.
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
        // The headline optional out-parameter, and the `$limit`-shifted sibling
        // that makes reading the stub (rather than guessing) matter.
        assert_eq!(out_params("preg_match"), Some(&[2][..]));
        assert_eq!(out_params("preg_match_all"), Some(&[2][..]));
        assert_eq!(out_params("similar_text"), Some(&[2][..]));
        assert_eq!(out_params("str_replace"), Some(&[3][..]));
        assert_eq!(out_params("str_ireplace"), Some(&[3][..]));
        // `preg_replace(..., $subject, $limit, &$count)` — count is 4, not 3.
        assert_eq!(out_params("preg_replace"), Some(&[4][..]));
        assert_eq!(out_params("preg_replace_callback"), Some(&[4][..]));
        assert_eq!(out_params("preg_replace_callback_array"), Some(&[3][..]));
        // The always-by-ref array family.
        for f in ["sort", "usort", "shuffle", "array_push", "array_pop", "reset", "settype"] {
            assert_eq!(out_params(f), Some(&[0][..]), "{f} writes argument 0");
        }
        // Case-insensitive, like every other row.
        assert_eq!(out_params("PREG_MATCH"), Some(&[2][..]));
    }

    #[test]
    fn the_written_when_witness_is_stated_for_the_measured_rows_only() {
        // The two measured contracts: truthy means the callee performed the write.
        // `preg_match_all` joined with issue #168 — measured, an int >= 1 return is
        // reachable only through the branch that wrote every column.
        assert_eq!(out_param_written_when("preg_match", 2), Some(WrittenWhen::ReturnTruthy));
        assert_eq!(out_param_written_when("PREG_MATCH", 2), Some(WrittenWhen::ReturnTruthy));
        assert_eq!(out_param_written_when("preg_match_all", 2), Some(WrittenWhen::ReturnTruthy));
        // A position the row does not name is not a by-ref position at all.
        for p in [0, 1, 3, 4] {
            assert_eq!(out_param_written_when("preg_match", p), None, "position {p} is by value");
            assert_eq!(out_param_written_when("preg_match_all", p), None, "position {p} is by value");
        }
        // Every other out-param row stays silent: a witness is added only after the
        // callee's contract has been measured, never inferred from `out_params`.
        for f in ["similar_text", "str_replace", "sort", "array_pop"] {
            for p in 0..5 {
                assert_eq!(out_param_written_when(f, p), None, "{f} states no witness yet");
            }
        }
    }

    #[test]
    fn a_witness_never_appears_at_a_by_value_position() {
        // The two tables must not contradict: a stated witness claims the callee
        // writes through that position, which `by_value_arg` must deny.
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
        // Their reference positions are open-ended: a positions row could only
        // under-approximate, and an under-approximated target leg would downgrade
        // an escaping write to `mutate.local`. Silence beats a wrong color.
        for f in ["sscanf", "fscanf", "array_multisort", "extract"] {
            assert_eq!(out_params(f), None, "{f} has no positional out-param row");
        }
    }

    #[test]
    fn by_value_arg_reads_the_out_param_row_positionally() {
        // The ADR-0070 sharpest pin: one call, two opposite answers.
        // `preg_match(string $pattern, string $subject, array &$matches = null)`.
        assert_eq!(by_value_arg("preg_match", 0), Some(true));
        assert_eq!(by_value_arg("preg_match", 1), Some(true), "$subject is by value");
        assert_eq!(by_value_arg("preg_match", 2), Some(false), "$matches is by ref");
        // `str_replace(..., $subject, int &$count = null)` — 3, not 2.
        assert_eq!(by_value_arg("str_replace", 2), Some(true));
        assert_eq!(by_value_arg("str_replace", 3), Some(false));
        // The always-by-ref array family writes argument 0 and nothing else.
        assert_eq!(by_value_arg("array_pop", 0), Some(false));
        assert_eq!(by_value_arg("sort", 0), Some(false));
        assert_eq!(by_value_arg("usort", 1), Some(true), "the comparator is by value");
        // Case-insensitive, like every other row.
        assert_eq!(by_value_arg("PREG_MATCH", 2), Some(false));
    }

    #[test]
    fn by_value_arg_certifies_the_rowless_names_positively() {
        // The folding allowlist is pure by construction.
        for f in ["trim", "ltrim", "rtrim", "sprintf", "implode", "strlen", "in_array"] {
            assert_eq!(by_value_arg(f, 0), Some(true), "{f} is certified by value");
            assert_eq!(by_value_arg(f, 1), Some(true), "{f} argument 1 too");
        }
        // The certified extras: aliases and the non-mutating read-position /
        // projection family.
        for f in ["chop", "join", "sizeof", "array_first", "array_last", "current", "key",
                  "array_values", "array_keys", "array_flip", "array_reverse",
                  "array_key_first", "array_key_last", "array_slice"] {
            assert_eq!(by_value_arg(f, 0), Some(true), "{f} is certified by value");
        }
        // The near-name pair the certification must not blur: `array_slice` is
        // by value at every position, `array_splice` writes its subject.
        for p in 0..4 {
            assert_eq!(by_value_arg("array_slice", p), Some(true), "array_slice position {p}");
        }
        assert_eq!(by_value_arg("array_splice", 0), Some(false), "array_splice is by ref");
    }

    /// Issue #41 — the string-producer family's six non-foldable members.
    ///
    /// Certification is per NAME here (none of them carries an `out_params` row),
    /// so every position answers `true`, including the optional ones the string
    /// rules read: `htmlspecialchars`' `$flags`, `vsprintf`' `$values`.
    #[test]
    fn by_value_arg_certifies_the_string_producer_family() {
        for f in ["addcslashes", "escapeshellarg", "escapeshellcmd", "htmlspecialchars",
                  "htmlentities", "vsprintf"] {
            for p in 0..4 {
                assert_eq!(by_value_arg(f, p), Some(true), "{f} position {p} is by value");
            }
            // Case-insensitive, like every other lookup.
            assert_eq!(by_value_arg(&f.to_uppercase(), 0), Some(true), "{f} folds case");
        }
        // `str_replace` stays the family's rowed member: its `&$count` is position
        // 3 and the certification must not blur that.
        assert_eq!(by_value_arg("str_replace", 2), Some(true));
        assert_eq!(by_value_arg("str_replace", 3), Some(false));
    }

    /// The `mb_*` family: certified for **argument** semantics while staying
    /// outside the fold allowlist, which is about the *result*. The two answers
    /// must be able to disagree, and this pins that they do.
    #[test]
    fn the_mb_family_is_by_value_without_becoming_foldable() {
        for f in ["mb_strtolower", "mb_strtoupper", "mb_substr", "mb_strlen", "mb_convert_case",
                  "mb_str_split", "mb_str_pad", "mb_strpos", "mb_convert_encoding", "mb_trim"] {
            assert_eq!(by_value_arg(f, 0), Some(true), "{f} is by value");
            assert!(!foldable(f), "{f} must NOT become foldable");
        }
        // The one member left out on purpose: it writes process-global state.
        assert_eq!(by_value_arg("mb_internal_encoding", 0), None);
    }

    #[test]
    fn by_value_arg_declines_every_name_it_has_not_certified() {
        // Absence of an `out_params` row is NOT a by-value statement: the
        // variadic-by-ref family and every uncatalogued name answer `None`, so a
        // consumer cannot mistake silence for a promise.
        for f in ["sscanf", "fscanf", "array_multisort", "extract", "parse_str", "exec",
                  "my_helper", "some_unknown_function"] {
            assert_eq!(by_value_arg(f, 0), None, "{f} is not certified");
            assert_eq!(by_value_arg(f, 1), None, "{f} is not certified at any position");
        }
    }

    #[test]
    fn the_two_catalog_axes_are_independent() {
        // `shuffle` is colored AND writes by reference; `sort` only writes;
        // `rand` only has a color. The consumer joins them.
        assert_eq!(effect_labels("shuffle"), Some(&["nondet.random"][..]));
        assert_eq!(out_params("shuffle"), Some(&[0][..]));
        assert_eq!(out_params("rand"), None);
        // `preg_match` has no unconditional color; its effect is conditional.
        assert_eq!(effect_labels("preg_match"), None);
    }

    #[test]
    fn new_effect_labels_color_the_mined_functions() {
        // io.signal (pcntl/posix), io.output.header (header/cookies), io.ipc (sysv),
        // and the composite session bootstrap.
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

    /// The ADR-0083 rows that close the read-and-relay false-negative gap: before
    /// the move, a body whose only statement was `readfile($p)` or `system($cmd)`
    /// carried no output component at all.
    #[test]
    fn relaying_builtins_carry_their_output_component() {
        // The read-and-relay pair is wrapper-capable, so its argument-blind row is
        // the `io` parent (issue #318) — which subsumes the output component the
        // pair used to spell out. A proven target restores both halves; that is
        // `narrowed_stream_labels`' job and its own tests pin it.
        assert_eq!(effect_labels("readfile"), Some(&["io"][..]));
        assert_eq!(effect_labels("fpassthru"), Some(&["io"][..]));
        assert_eq!(
            super::narrowed_stream_labels("readfile", Some(Literal("/tmp/x")), None),
            Some(vec!["io.fs.read", "io.output.buffer"])
        );
        // Split capturability evidence → the parent, which no future masking may
        // deduct.
        assert_eq!(effect_labels("system"), Some(&["io.process", "io.output"][..]));
        assert_eq!(effect_labels("passthru"), Some(&["io.process", "io.output"][..]));
        assert_eq!(effect_labels("curl_exec"), Some(&["io.net", "io.output"][..]));
        // Its failure-arm row is a separate table and is untouched by the coloring.
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
        // Case-insensitive on the function name, like every other row.
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
        // A PTY-driven child process.
        assert_eq!(narrowed("fopen", Some(Literal("expect://ls")), Some(Literal("r"))), Some(vec!["io.process"]));
        // Compression and archive wrappers stay in the filesystem family.
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
        // Memory and data URIs touch no channel at all.
        assert_eq!(narrowed("file_get_contents", Some(Literal("php://memory")), None), Some(vec!["mutate.local"]));
        assert_eq!(narrowed("file_get_contents", Some(Literal("data://text/plain,hi")), None), Some(vec!["mutate.local"]));
        // `php://temp` spills to a real file past its threshold.
        assert_eq!(narrowed("fopen", Some(Literal("php://temp")), Some(Literal("w"))), Some(vec!["io.fs.write"]));
        assert_eq!(narrowed("fopen", Some(Literal("php://temp/maxmemory:1024")), Some(Literal("w"))), Some(vec!["io.fs.write"]));
        // The wrapper name and the stream name both fold case.
        assert_eq!(narrowed("file_put_contents", Some(Literal("PHP://StdOut")), None), Some(vec!["io.output.stdout"]));
        // A file descriptor is a number this table cannot resolve to a channel.
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
        // One step, and no more: a filter naming another filter is where this
        // stops, with the `io` default (`None`) rather than an unbounded walk.
        assert_eq!(
            narrowed("file_get_contents", Some(Literal("php://filter/resource=php://filter/resource=/etc/hosts")), None),
            None
        );
        // A filter spec with no `resource=` opens nothing this table can name.
        assert_eq!(narrowed("file_get_contents", Some(Literal("php://filter/read=x")), None), None);
    }

    #[test]
    fn an_unknown_scheme_keeps_the_io_default() {
        // A userland `stream_wrapper_register('acme', …)` is exactly this case:
        // ruling D-W1 approximates it by the widened default, and nothing here
        // reads the registration.
        assert_eq!(narrowed("file_get_contents", Some(Literal("acme://bucket/key")), None), None);
        assert_eq!(narrowed("file_get_contents", Some(Literal("foo://x")), None), None);
        // No proven target at all — the ordinary `file_get_contents($path)` call.
        assert_eq!(narrowed("file_get_contents", None, None), None);
        // A name with no stream row of its own never narrows, proven or not.
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
        // A `+` opens both directions: the parent, which is what the row said
        // before #318 for every mode.
        assert_eq!(narrowed("fopen", Some(Literal("/tmp/x")), Some(Literal("r+"))), Some(vec!["io.fs"]));
        assert_eq!(narrowed("fopen", Some(Literal("/tmp/x")), Some(Literal("w+b"))), Some(vec!["io.fs"]));
        // An unprovable mode leaves the direction unknown — the same parent.
        assert_eq!(narrowed("fopen", Some(Literal("/tmp/x")), None), Some(vec!["io.fs"]));
        assert_eq!(narrowed("fopen", Some(Literal("/tmp/x")), Some(Constant("SOME_MODE"))), Some(vec!["io.fs"]));
        // Off the filesystem the mode is irrelevant.
        assert_eq!(narrowed("fopen", Some(Literal("https://h/r")), None), Some(vec!["io.net.http"]));
    }

    #[test]
    fn the_resource_rows_narrow_only_on_the_predefined_constants() {
        assert_eq!(narrowed("fwrite", Some(Constant("STDOUT")), None), Some(vec!["io.output.stdout"]));
        assert_eq!(narrowed("fputs", Some(Constant("STDERR")), None), Some(vec!["io.output.stderr"]));
        assert_eq!(narrowed("fread", Some(Constant("STDIN")), None), Some(vec!["io.input"]));
        assert_eq!(narrowed("fgets", Some(Constant("STDIN")), None), Some(vec!["io.input"]));
        assert_eq!(narrowed("fpassthru", Some(Constant("STDIN")), None), Some(vec!["io.input", "io.output.buffer"]));
        // Any other constant is a resource of unknown provenance.
        assert_eq!(narrowed("fwrite", Some(Constant("SOCKET")), None), None);
        // Constants are case-sensitive in PHP, so `stdout` is a different name.
        assert_eq!(narrowed("fwrite", Some(Constant("stdout")), None), None);
        // A form mismatch in either direction proves nothing: a string is not a
        // resource, and a constant is not a path this table can read.
        assert_eq!(narrowed("fwrite", Some(Literal("/tmp/x")), None), None);
        assert_eq!(narrowed("file_get_contents", Some(Constant("STDIN")), None), None);
    }

    #[test]
    fn the_two_target_rows_read_each_side_in_its_own_role() {
        // `copy` reads its source and writes its destination — two roles, two
        // labels, and both of them true of the call.
        assert_eq!(
            narrowed("copy", Some(Literal("/a")), Some(Literal("/b"))),
            Some(vec!["io.fs.read", "io.fs.write"])
        );
        // `rename` moves a directory entry: metadata writes on both sides, and
        // the two collapse to one label.
        assert_eq!(narrowed("rename", Some(Literal("/a")), Some(Literal("/b"))), Some(vec!["io.fs.write"]));
        // A remote source is a genuinely different transport, and the roles keep
        // it apart from the local destination.
        assert_eq!(
            narrowed("copy", Some(Literal("https://h/a")), Some(Literal("/b"))),
            Some(vec!["io.net.http", "io.fs.write"])
        );
        assert_eq!(
            narrowed("copy", Some(Literal("/a")), Some(Literal("ssh2.sftp://h/b"))),
            Some(vec!["io.fs.read", "io.net"])
        );
        // The same pair under `rename`'s both-write reading.
        assert_eq!(
            narrowed("rename", Some(Literal("ftp://h/a")), Some(Literal("/b"))),
            Some(vec!["io.net", "io.fs.write"])
        );
        // One side unprovable: its narrowing is the `io` default, and the union
        // with `io` is `io` — no narrowing, so the row declines outright.
        assert_eq!(narrowed("copy", Some(Literal("/a")), None), None);
        assert_eq!(narrowed("copy", None, Some(Literal("/b"))), None);
        // An unknown scheme on either side declines the same way.
        assert_eq!(narrowed("copy", Some(Literal("acme://a")), Some(Literal("/b"))), None);
        assert_eq!(narrowed("copy", Some(Literal("/a")), Some(Literal("acme://b"))), None);
    }

    #[test]
    fn the_stat_and_unlink_family_narrows_by_scheme_but_not_by_pseudo_stream() {
        // The positive control for all eight: a literal local path gives each of
        // them the precise row it carried before issue #318 widened it.
        for name in ["unlink", "mkdir", "rmdir", "touch"] {
            assert_eq!(narrowed(name, Some(Literal("/tmp/x")), None), Some(vec!["io.fs.write"]), "{name}");
        }
        for name in ["scandir", "file_exists", "is_file", "is_dir"] {
            assert_eq!(narrowed(name, Some(Literal("/tmp/x")), None), Some(vec!["io.fs.read"]), "{name}");
        }
        // …and the reason they were widened: these go over a wrapper too.
        assert_eq!(narrowed("unlink", Some(Literal("ssh2.sftp://h/x")), None), Some(vec!["io.net"]));
        assert_eq!(narrowed("mkdir", Some(Literal("ftp://h/d")), None), Some(vec!["io.net"]));
        assert_eq!(narrowed("file_exists", Some(Literal("ssh2.sftp://h/x")), None), Some(vec!["io.net"]));
        assert_eq!(narrowed("is_dir", Some(Literal("ftp://h/d")), None), Some(vec!["io.net"]));
        // A `php://` target is not a question these functions ask, so they
        // decline it and keep the `io` default rather than name a channel.
        assert_eq!(narrowed("unlink", Some(Literal("php://stdout")), None), None);
        assert_eq!(narrowed("is_file", Some(Literal("php://input")), None), None);
        assert_eq!(narrowed("file_exists", Some(Literal("php://filter/resource=/x")), None), None);
        // Their second argument is never a target or a mode: `mkdir`'s
        // permissions and `scandir`'s sort order change nothing.
        assert_eq!(narrowed("mkdir", Some(Literal("/tmp/d")), Some(Literal("0777"))), Some(vec!["io.fs.write"]));
        assert_eq!(narrowed("scandir", Some(Literal("/tmp")), Some(Literal("1"))), Some(vec!["io.fs.read"]));
        // And a constant is not a path: these rows take no resource.
        assert_eq!(narrowed("unlink", Some(Constant("STDOUT")), None), None);
    }

    #[test]
    fn every_narrowed_label_is_a_registry_entry() {
        // The narrowing table is a second producer of effect labels, so it owes
        // the registry the same debt `effect_labels` does — a label no envelope
        // could name would be unreportable and unmatchable.
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
            // Every narrowing name is also a catalogued row: the narrowing REPLACES
            // a default, it never colors an uncatalogued function.
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
    fn adr0063_p1_immediately_invoked_rows() {
        let s = |n| invocation_shape(n).expect("known invoker");
        // PHP 8.4 search predicates: cb at 1 over param 0's elements, immediate.
        for n in ["array_find", "array_find_key", "array_any", "array_all"] {
            assert_eq!(s(n).callback_param, 1, "{n}");
            assert_eq!(s(n).invocation, Invocation::Immediate, "{n}");
            assert_eq!(s(n).arg_source, ArgSource::ElementsOf(0), "{n}");
        }
        // array_walk_recursive: immediate, but the callback sees leaves, so the
        // element source is deliberately unmodeled.
        assert_eq!(s("array_walk_recursive").callback_param, 1);
        assert_eq!(s("array_walk_recursive").invocation, Invocation::Immediate);
        assert_eq!(s("array_walk_recursive").arg_source, ArgSource::None);
        // iterator_apply: cb at 1, immediate, args from the third parameter.
        assert_eq!(s("iterator_apply").callback_param, 1);
        assert_eq!(s("iterator_apply").invocation, Invocation::Immediate);
    }

    #[test]
    fn adr0063_p1_exclusions_carry_no_shape() {
        // Deferred invokers (the callable is stored, not immediately invoked) and
        // shapes this table cannot express (callables inside an array; comparators
        // in the LAST variadic position) stay uncatalogued on purpose.
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
        // The ADR-0033 deferred row remains represented.
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
