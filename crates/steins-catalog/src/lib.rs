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

/// The builtin return-fact refinement table (ADR-0056), generated from
/// `docs/research/phpsrc-mining/return_facts.toml` by `cargo xtask gen-catalog`.
/// Consulted only by [`return_fact`]. May be empty (R1 lands zero rows).
mod return_facts_generated;

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
/// *same* `steins_handle` dispatch core, 310 adversarial tuples over boundary
/// integers, oversized numeric strings, oversized floats, negative inputs and
/// integer array keys at `PHP_INT_MAX`. See the ADR-0066 amendment for the table.
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
const WIDTH_REFUSED: &[&str] = &["abs", "intval", "sprintf"];

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
    /// a 32-bit engine while the refused table claims otherwise) and that the size
    /// is the 22 the ADR-0066 amendment tabulates.
    #[test]
    fn the_width_classes_partition_the_allowlist() {
        for name in WIDTH_SAFE {
            assert!(!WIDTH_REFUSED.contains(name), "{name} is classified twice");
            assert!(foldable(name), "{name} is classified but not foldable");
        }
        for name in WIDTH_REFUSED {
            assert!(foldable(name), "{name} is classified but not foldable");
        }
        assert_eq!(WIDTH_SAFE.len(), 19, "the verified width-safe subset");
        assert_eq!(WIDTH_REFUSED.len(), 3, "the refused rows");
        assert_eq!(
            WIDTH_SAFE.len() + WIDTH_REFUSED.len(),
            22,
            "the allowlist size the ADR-0066 amendment tabulates"
        );
    }

    /// The three refused rows, named. Each is a *silent* value divergence on a
    /// 32-bit engine — see `WIDTH_REFUSED` for the verbatim probes.
    #[test]
    fn the_width_sensitive_builtins_are_refused() {
        for name in ["abs", "intval", "sprintf", "ABS", "IntVal", "SPRINTF"] {
            assert!(!width_safe(name), "{name} must not be certified width-safe");
        }
        // …and the certification is real, not vacuous.
        for name in ["strtoupper", "substr", "str_repeat", "count", "in_array", "STRLEN"] {
            assert!(width_safe(name), "{name} is a verified width-safe fold");
        }
    }

    /// Default-deny: a name nobody classified is not width-safe, foldable or not.
    #[test]
    fn an_unclassified_name_is_not_width_safe() {
        for name in ["some_unknown_fn", "ip2long", "crc32", "hexdec", "dechex", "strtotime"] {
            assert!(!width_safe(name), "{name} must not be certified width-safe");
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

    use super::{is_known_label, nearest_label, out_params, subsumes};

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
