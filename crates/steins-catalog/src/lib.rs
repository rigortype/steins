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
//!
//! ## Two kinds of refusal, and why they are not the same list
//!
//! A name in `WIDTH_REFUSED` **is** on the allowlist — it folds on a provably
//! 64-bit engine and declines on a 32-bit one. So a builtin that fails ADR-0008's
//! purity/determinism bar can never be written as a refused row: that would admit
//! it. Those names are refused from the allowlist *entirely*, and are recorded here
//! with the evidence, pinned absent by `impure_and_locale_sensitive_are_excluded`:
//!
//! * `strtotime`, `date`, `idate` — **nondet.time**, and timezone-coupled even when
//!   handed an explicit timestamp: `idate("Y", 0)` is `1970` under `UTC` and `1969`
//!   under `Pacific/Kiritimati` (probed). `strtotime("2020-01-01")` differed between
//!   the two probe engines by exactly their timezone offset (`1577804400` vs
//!   `1577836800`), which is the divergence in its purest form.
//! * `mb_*` — encoding-coupled (`mbstring.internal_encoding`). The browser engine
//!   settles it a second way: php-wasm 0.1.0 has **no mbstring extension**, so all
//!   eleven `mb_*` probes answered `widen: unknown function` there.
//! * `strcmp`, `strcasecmp` — the *contract* is the sign; the *value* is `memcmp`'s,
//!   which C leaves implementation-defined. Both probe engines agreed on all 36
//!   tuples (`strcmp("A","a")` = `-32`, `strcmp("zzz","a")` = `25` on each), so this
//!   is **not** a width verdict — it is an ADR-0008 one. Folding would pin a literal
//!   the language does not promise, and a sign-normalized admission would have the
//!   catalog report `-1` where the engine returns `-32`: forking semantics, which the
//!   fold seam must never do. Declining costs nothing that a two-literal `strcmp`
//!   call was going to buy.
//! * `number_format` — held out with the `mb_*` family by the issue-#78 must-not
//!   list. Recorded honestly: the width probe found no divergence, and the historical
//!   locale coupling of float rendering is gone at `PINNED_PHP` (`de_DE.UTF-8` and
//!   `C` render `number_format(1234.5678, 2)` identically, and `precision` does not
//!   move it). It stays out on the conservative side and may be admitted later on
//!   its own evidence rather than smuggled in on this slice's.
//! * `bin2hex` — carries a standing refused row in the ADR-0056 return-fact table
//!   (`docs/research/phpsrc-mining/return_facts.toml`, the empty-in/empty-out trap).
//!   That row is about a different table and is **not relitigated here**; the width
//!   probe found no divergence, and the name simply does not enter on this slice.

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
/// Consulted only by [`return_fact`]. May be empty (R1 lands zero rows).
mod return_facts_generated;

/// The builtin declared-return floor (ADR-0069), generated from
/// `docs/research/phpstan-mining/declared_returns.toml` by `cargo xtask
/// gen-catalog`. Consulted only by [`declared_return`] and
/// [`declared_return_changed_at`].
mod declared_returns_generated;

/// Whether `name` is on the folding allowlist (case-insensitive).
///
/// A `true` here is a *permission to fold*, not a promise the call folds: the
/// inference engine still requires the callee to be a non-user function and all
/// arguments to be literals the IR carries before it asks the sidecar.
///
/// Several allowlisted functions (`sprintf`, `str_replace`, `in_array`, `count`,
/// `implode`) commonly take **array** arguments. Those calls now qualify: an
/// argument may be a scalar literal *or* an array literal that is concrete all the
/// way down (issue #39). `in_array`/`count`/`implode` were parked here waiting for
/// exactly that, and lit up when the fold seam learned to carry an array — this
/// list never changed, which is the point.
///
/// A folded *result* is still scalar-only: a builtin that returns an array (say
/// `str_replace` over an array subject) widens, because carrying an array back
/// would seed synthesized array facts rather than read written ones (#41/#42).
///
/// # Where the list lives
///
/// The allowlist is spelled as the union of the two integer-width classes,
/// `WIDTH_SAFE` and `WIDTH_REFUSED` (issue #64 S1.5), rather than as a third list
/// they are checked against. A name is foldable *by being classified*, so a name
/// added without a width verdict is not foldable at all — the invariant holds by
/// construction rather than by a test that could be forgotten. The two lists keep
/// the allowlist's own composition rules: ASCII-cased string builtins only (the
/// `mb_*` and locale-sensitive variants are deliberately excluded, as are all
/// `nondet` builtins), and the array-taking members (`in_array`, `count`,
/// `implode`, `str_replace`, `sprintf`) which lit up when the fold seam learned
/// to carry an array literal (issue #39) without this list changing at all.
#[must_use]
pub fn foldable(name: &str) -> bool {
    width_safe(name) || width_refused(name)
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
/// **declines** (throws, or widens). A decline is the sound direction — it is
/// exactly what the blanket ADR-0066 §4 gate does for every name today — so the
/// browser loses precision there and never gains a wrong literal.
///
/// The guard's lower bound is `-(2^31 - 1)` and **not** `-2^31`: excluding
/// `PHP_INT_MIN`-on-32-bit is what makes the `abs`-shaped boundary flip
/// unreachable, because no in-range integer has an out-of-range magnitude.
///
/// This is the width-safe subset ADR-0066 §4 deferred, and it is **verified, not
/// reasoned**: every name below was probed differentially against php-wasm 0.1.0
/// (PHP 8.5.2, `PHP_INT_SIZE = 4`) and `php` 8.5.8 (`PHP_INT_SIZE = 8`) through the
/// *same* `steins_handle` dispatch core: **661 adversarial tuples** over two rounds
/// (310 at issue #64 S1.5, 351 more for issue #78's candidate round) covering
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
/// The playground's boundary widget (issue #64 S3) is the caller: it states how
/// much of the folding allowlist is live on the engine the browser actually
/// booted, and the counts have to come from the catalog rather than from a number
/// typed into JS, or the page can drift from the gate it describes.
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

/// The verified width-safe half of the folding allowlist (issue #64 S1.5).
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
/// Issue #78 grew the table by eighteen names, probed the same way and falling
/// into the same groups:
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
/// Issue #78 adds six rows, all of the same shape — a builtin whose *job* is to
/// read or write an integer in the machine's own width:
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
/// One class family, deliberately: `PDO`/`PDOStatement`, the first producer of
/// the `io.db` label, which the registry has carried since ADR-0018 with nothing
/// to emit it. `io.db` is the coarse label for the whole family — statement
/// preparation is as much a round trip to the server as execution is (`PDO`'s
/// emulated-prepares setting decides whether `prepare` talks to the server at
/// all, which is runtime configuration this catalog cannot read, so the row takes
/// the upper bound). Breadth — mysqli, the rest of the mined method rows — comes
/// from the ADR-0014 generator, not from hand-seeding here.
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
/// This is the catalog's one **conditional** row shape, and the conditionality is
/// the point. [`effect_labels`] answers "what color does calling this function
/// have", unconditionally — a per-function flag. An out-parameter write is not a
/// property of the function, it is a property of the *call*: `preg_match($p, $s)`
/// writes nothing, `preg_match($p, $s, $m)` writes `$m`, and the same two calls
/// differ again in *whose* binding `$m` is. Flattening that into an
/// unconditional color is exactly the metadata-only-purity lie ADR-0063 imports
/// the refusal of (php-src #11884: "conditional on the argument, not a
/// per-function lie"). So the row carries positions only, and the consumer
/// resolves it against the call site:
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
/// Certification means: at `PINNED_PHP`, **every** parameter of the name is
/// declared by value in the php-src stub. The set is closed and motivated — it
/// is exactly the names Steins' own inference rules already reason about:
///
/// * the folding allowlist ([`foldable`]), which is pure by construction, plus
/// * the ADR-0062/0064 array read-position and shape-projection family that does
///   **not** carry an out-param row (`array_first`/`array_last`/`array_values`/…;
///   `current` and `key` take `array|object $array`, while their pointer-moving
///   siblings `reset`/`end`/`next`/`prev` take `&$array` and are rowed above —
///   the two tables corroborate each other), plus
/// * the alias spellings of foldable names (`chop`, `join`, `sizeof`), which are
///   the same C function under a second name.
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
/// `global` appears here as a root even though the registry lists only its
/// `global.read` / `global.write` children — root ownership is about the *name
/// space*, not about which nodes happen to be colorable today.
#[must_use]
pub fn core_roots() -> &'static [&'static str] {
    &["exit", "failure", "ffi", "global", "io", "mutate", "nondet", "output"]
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
        // By-ref out-parameter write landing in a binding of the *calling* frame
        // (ADR-0063 §2.3): `preg_match($p, $s, $matches)`, `sort($localArray)`.
        // The degenerate member of the `mutate` family — nothing escapes the
        // caller, so no observer outside the frame can tell it happened, which is
        // why every envelope tolerates it (see `steins-infer`'s `exceeds`). It
        // still earns a label rather than silence because the annotate/summary
        // surface, and a future by-ref out-param fact lane, want to name it. Its
        // caller-*observable* siblings (`mutate.arg`/`.self`/`.instance`/
        // `.static`, ADR-0055 point 1) are not inferred yet; this slice's
        // non-local targets stop at the parent `mutate` rather than guess a child.
        "mutate.local",
        "nondet",
        "nondet.random",
        "nondet.time",
        "output",
        // HTTP response-header mutation (effects_gaps.md §2): a response-side
        // sibling of stdout `output`; a coarse `output` subsumes it, a policy can
        // name it precisely.
        "output.header",
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
/// for labels finer than the shipped taxonomy — `io.netw` is neither a node nor
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

/// The label registry **as one run sees it**: the builtin table ([`known_labels`])
/// plus whatever the ADR-0012/0039 plugin channel registered for this project
/// (ADR-0068). Inference asks this, not the free functions, so an ecosystem label
/// a plugin registered stops earning `effect.unknown-label` without the builtin
/// table growing a single ecosystem row.
///
/// [`LabelRegistry::builtin`] is the closed view, and it is the default: every
/// caller that has no project in hand (a single-file check, a unit test, the
/// browser) gets exactly today's answers. Extension labels are validated *before*
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
///
/// # The immediately-invoked rows (ADR-0063 P1)
///
/// ADR-0063 decision 1 makes this table the **callback-position catalog** that
/// drives the higher-order effect join: a row asserts that the named position is
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
///   grandfathered `Deferred` row (`register_shutdown_function`, ADR-0033) is
///   kept as-is; ADR-0063 P1 adds no new deferred rows, so a non-immediate
///   position contributes nothing new.
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
/// `return_facts.toml`; it is EMPTY in R1 (the bool-predicate family's reflected
/// envelope is already `bool`, so no refinement adds precision). Matching is
/// case-insensitive; the generated keys are lowercased and sorted for binary search.
#[must_use]
pub fn return_fact(name: &str) -> Option<&'static str> {
    let key = name.trim_start_matches('\\').to_ascii_lowercase();
    return_facts_generated::RETURN_FACTS
        .binary_search_by(|(n, _)| (*n).cmp(key.as_str()))
        .ok()
        .map(|i| return_facts_generated::RETURN_FACTS[i].1)
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
/// Issue #79 widened the filter from single-base envelopes to the full scalar arm
/// vocabulary, so a value here may be a `T|false` failure union or a refinement
/// (`non-empty-string`, `non-negative-int`) as well as a bare base. The grade is
/// unchanged: still Asserted, still never a proof premise.
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
/// complete either way. The two sets were disjoint through the #73 and #79 pins —
/// every version-sensitive name returns an array, which the floor could not then
/// carry — and ADR-0071's array widening makes them **overlap**, which is exactly
/// the case this gate was wired ahead of and now decides for real.
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

    /// The two name accessors ARE the two predicates, extensionally — the boundary
    /// widget (issue #64 S3) names the subset through them, and a list that drifted
    /// from the predicate would make the page describe a gate that is not the gate.
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

    /// Default-deny: a name nobody classified is not width-safe, foldable or not.
    ///
    /// `hexdec`/`dechex` used to sit in this roster and have since been *classified*
    /// (refused, issue #78), so they moved to
    /// `the_width_sensitive_builtins_are_refused`; what is pinned here is the
    /// unclassified case, which must stay populated for the test to mean anything.
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
            "printf",            // output
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
    fn return_facts_r3_r4_rows() {
        // ADR-0056 R3+R4 populate the curated table with the int-range and
        // refined-string families. The bool-predicate family (R1) still has NO row —
        // its reflected envelope is already `bool`, nothing to refine.
        assert_eq!(super::return_fact("is_int"), None);
        assert_eq!(super::return_fact("some_unknown_fn"), None);
        // R3 int-range: `int<0, max>` within the reflected `int` envelope.
        for name in ["count", "sizeof", "strlen", "mb_strlen", "substr_count", "func_num_args", "array_push", "array_unshift"] {
            assert_eq!(super::return_fact(name), Some("int<0, max>"), "{name} must curate int<0, max>");
        }
        // R4 refined-string: `non-falsy-string` within the reflected `string` envelope.
        // DR4 extends the same family with two probe-verified rows: `get_debug_type`
        // (every return is a type keyword or a class name — PHP's label grammar forbids
        // a leading digit, so "0" is not nameable) and `spl_object_hash` (a fixed
        // 32-char lowercase hex digest; its `object` parameter has no empty-in path).
        for name in ["sha1", "md5", "uniqid", "get_debug_type", "spl_object_hash"] {
            assert_eq!(super::return_fact(name), Some("non-falsy-string"), "{name} must curate non-falsy-string");
        }
        // Refused rows carry no curated fact (argument-sensitive / multi-base).
        // `dirname` is the DR4 refusal: `dirname("0/x")==="0"` is falsy AND
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
        // ADR-0064 seam iii (DR4) extends the R4 `non-falsy-string` family with the two
        // candidates whose probes survived the three-leg gate at PHP 8.5.8. Both have a
        // single `string` reflected envelope, so the refinement narrows strictly within it.
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
        // The DR4 census proposed `dirname(): non-falsy-string`; the probes REFUTED it
        // twice over, so `dirname` is a refused row and must never gain a curated fact.
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

    /// Every spelling in the shipped table that a *single-base envelope* can state:
    /// the #73 population. Everything else is the issue-#79 reach.
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
        // ADR-0069 / issue #73: the Asserted floor's rows. `str_repeat` is the ADR's
        // own worked example — `string` with the sidecar, `unknown` without it before
        // this table existed. Every #73 row is still here, spelled the same way.
        assert_eq!(super::declared_return("str_repeat"), Some("string"));
        assert_eq!(super::declared_return("str_pad"), Some("string"));
        assert_eq!(super::declared_return("array_key_exists"), Some("bool"));
        assert_eq!(super::declared_return("acos"), Some("float"));
        assert_eq!(super::declared_return("curl_multi_getcontent"), Some("string|null"));
        // Issue #79's reach: the rows functionMap states more richly than any
        // envelope could, which #73 counted and dropped.
        assert_eq!(super::declared_return("strstr"), Some("string|false"));
        assert_eq!(super::declared_return("strrchr"), Some("string|false"));
        assert_eq!(super::declared_return("file_get_contents"), Some("string|false"));
        assert_eq!(super::declared_return("array_search"), Some("int|string|false"));
        assert_eq!(super::declared_return("preg_match"), Some("0|1|false"));
        assert_eq!(super::declared_return("ctype_alpha"), Some("bool"));
        // A scalar refinement — the other #79 bucket. functionMap states what
        // reflection cannot: `mb_strtoupper` never returns a lowercase character.
        assert_eq!(super::declared_return("mb_strtoupper"), Some("uppercase-string"));
        // The ADR-0071 bucket: the array vocabulary, which #73 and #79 both counted
        // and dropped because the countersign could only shrug at it. A bare `array`,
        // a list, a keyed map and a full shape all ship now.
        assert_eq!(super::declared_return("array_merge"), Some("array"));
        assert_eq!(super::declared_return("str_split"), Some("list<string>"));
        assert_eq!(super::declared_return("array_count_values"), Some("array<positive-int>"));
        assert_eq!(
            super::declared_return("imagecolorsforindex"),
            Some("array{alpha: int<0, 127>, blue: int<0, 255>, green: int<0, 255>, red: int<0, 255>}")
        );
        // And an array arm inside a union, which was the other half of the movement:
        // the row was uncarriable only because ONE of its arms was an array.
        assert_eq!(super::declared_return("scandir"), Some("false|list<string>"));
        // The object slice: class rows, admitted by the reflexive countersign alone.
        // These keep functionMap's OWN casing rather than a canonical respelling —
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
        // The mining counts, pinned. The #73 slice admitted 919 rows, every one a
        // single-base envelope; #79 kept all of them (the countersign still admits a
        // row that BOUNDS the engine) and added 439 richer ones; ADR-0071's array
        // widening keeps those 1,358 name for name and adds 248 more; the object
        // slice keeps all 1,606 and adds 102. A drop below 919, or a collapse of the
        // rich population, means a lowering regressed.
        let rich = t.iter().filter(|(_, ty)| !ENVELOPE_SPELLINGS.contains(ty)).count();
        assert_eq!(t.len(), 1708, "admitted rows at this pin");
        assert_eq!(t.len() - rich, 919, "the #73 envelope population must be preserved exactly");
        assert_eq!(rich, 789, "the #79, ADR-0071 and object-slice rich admissions");
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
        // The #79 widening keeps ALL of them. That is the load-bearing property of the
        // arm-wise clause: "the row refines the engine" on its own would have readmitted
        // exactly these — `string` is a perfectly good refinement of `?string` unless
        // dropping the engine's own `null` arm is itself a disagreement.
        for name in ["sodium_add", "sodium_increment", "xml_error_string", "pg_port", "imageinterlace"] {
            assert_eq!(super::declared_return(name), None, "{name} must stay excluded");
        }
        for name in ["intlcal_get", "socket_cmsg_space", "ldap_compare", "pg_last_notice"] {
            assert_eq!(super::declared_return(name), None, "{name}: the row drops an engine arm");
        }
        // The catches the RICH rows brought with them — the map's own rot, now visible
        // because these rows are candidates at all. `imageloadfont` is the sharpest:
        // functionMap still says `int|false` where PHP 8 returns a `GdFont` object.
        for name in ["imageloadfont", "pow", "rewinddir", "substr_compare", "fpassthru"] {
            assert_eq!(super::declared_return(name), None, "{name}: an #79 candidate the engine disowns");
        }
        // And the catches ADR-0071's array candidates brought, which are the SAME
        // dropped-arm shape one vocabulary over: `ftp_raw` says `array` where the
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
        // And the object slice's own catches, which are the resource-era rot made
        // visible. `stream_bucket_make_writeable` is the sharpest of the whole table:
        // functionMap says the call returns a bare `stdClass`, where PHP 8 declares a
        // real `StreamBucket` — the stand-in outlived the thing it stood in for, and
        // the reflexive countersign refuses it because the two names simply differ.
        // The rest are the familiar dropped-arm shape wearing class names:
        // `intlcal_create_instance` and the four `tidy_get_*` rows hide the engine's
        // `null` exactly as `ftp_raw` hid one; `xmlwriter_open_uri` hides its `false`;
        // `dom_import_simplexml` drops the engine's `DOMAttr` arm AND invents a
        // `false`. None of them could have been caught before, because none of them
        // was a candidate.
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
        // refusal matters more than it looks: a CONSTANT name is not vocabulary, so
        // `lower_identifier`'s catch-all lowers it to a `Class` arm, which the object
        // slice made carriable. The countersign is what keeps those rows out — the
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
        assert_eq!(effect_labels("File_Put_Contents"), Some(&["io.fs.write"][..]));
        assert_eq!(effect_labels("STRTOLOWER"), Some(&[][..]));
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

    use super::{by_value_arg, is_core_label, is_known_label, nearest_label, out_params, subsumes};

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
        assert!(!is_core_label("email.send"));
        // Segment-aware, like every other label predicate here.
        assert!(!is_core_label("iota.thing"));
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
                  "array_key_first", "array_key_last"] {
            assert_eq!(by_value_arg(f, 0), Some(true), "{f} is certified by value");
        }
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
        // `preg_match` has no unconditional color at all — its only effect is the
        // conditional one, which is why it was uncatalogued before ADR-0063 P2.
        assert_eq!(effect_labels("preg_match"), None);
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
        // The one grandfathered deferred row is untouched by ADR-0063 P1.
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
