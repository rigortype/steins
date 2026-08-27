//! The effect tables of builtins (ADR-0018 labels, ADR-0021 argument-blind
//! upper bounds): [`effect_labels`] and its method-shaped twin
//! [`method_effect_labels`], the call-site narrowing of the wrapper-capable
//! stream rows ([`narrowed_stream_labels`], issue #318), the two callback
//! shapes no parameter table can see ([`callables_in_array_param`],
//! [`variadic_tail_is_data`], issue #382), and the by-ref out-parameter rows
//! ([`out_params`], ADR-0063) with their written-when witnesses
//! ([`out_param_written_when`], ADR-0077) and the by-value certification
//! ([`by_value_arg`], ADR-0070).
//!
//! Two seams reach into the fold allowlist, and both read only [`foldable`]: a
//! foldable name is catalogued pure (`Some(&[])`), and a foldable name's
//! arguments are all by value.

use crate::fold::foldable;

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
    // Runs a child and hands its output BACK — captured, returned, or piped —
    // so the parent's own output channel is untouched.
    const IO_PROCESS: &[&str] = &["io.process"];
    // Talks to a database server. Named apart from the method-side `IO_DB` so
    // the two tables can be read side by side without one shadowing the other.
    const IO_DB_LABELS: &[&str] = &["io.db"];
    // `curl_exec`: response body to output unless `CURLOPT_RETURNTRANSFER`.
    const NET_TO_OUTPUT: &[&str] = &["io.net", "io.output"];

    let colored: Option<&'static [&'static str]> = match name.to_ascii_lowercase().as_str() {
        "rand" | "mt_rand" | "random_int" | "random_bytes" | "uniqid" | "shuffle" => {
            Some(NONDET_RANDOM)
        }
        // The time family, argument-blind (ADR-0021). `date("Y-m-d", 0)` with an
        // explicit timestamp still reads the ambient timezone, and the same
        // name with the timestamp omitted reads the clock, so the row is the
        // upper bound over both — which is why `date` has carried this label
        // since the first seeding pass. The names below are that row's
        // siblings, added when a coverage survey found the module doc claiming
        // `strtotime`/`idate` were `nondet.time` while `effect_labels` answered
        // `None` for both. The `gm*` spellings read UTC rather than the ambient
        // zone, but omitting their timestamp still reads the clock.
        "time" | "microtime" | "hrtime" | "date" | "mktime" => Some(NONDET_TIME),
        "strtotime" | "idate" | "gmdate" | "gmmktime" | "getdate" | "localtime" => {
            Some(NONDET_TIME)
        }
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
        // Shell out and DO NOT relay: `exec` captures into its by-ref array and
        // returns the last line, `shell_exec` returns the whole output as a
        // string, and `popen`/`proc_open` hand back pipes for the caller to read
        // (effects_gaps.md's seeding gap — the label existed, the rows did not).
        // So the parent's own output is untouched and `io.process` stands alone:
        // the child still runs, which is the effect a purity envelope is about.
        // A relayed child is `system`/`passthru` above, and that difference is
        // exactly why these are not simply added to that row.
        "exec" | "shell_exec" | "popen" | "proc_open" => Some(IO_PROCESS),
        "curl_exec" => Some(NET_TO_OUTPUT),
        "error_log" | "syslog" | "sleep" | "usleep" => Some(IO),
        "date_default_timezone_set" | "mb_regex_encoding" | "setlocale" | "ini_set" | "putenv" => {
            Some(GLOBAL_WRITE)
        }
        // Process-global state, no channel: seeding pair replaces RNG state;
        // `clearstatcache` empties the stat cache. Drawing stays `nondet.random`.
        "srand" | "mt_srand" | "clearstatcache" => Some(GLOBAL_WRITE),
        // Handler and wrapper REGISTRATION (effects_gaps.md §5): each writes a
        // slot of the engine's own dispatch table, which every later call in the
        // process reads. `global.write` is the honest coarse colour — a finer
        // node would claim a channel these do not touch by themselves.
        //
        // The write is the effect, not the eventual call: `register_shutdown_function`
        // additionally carries the callback into shutdown (ADR-0033's deferred
        // invoker), and `stream_wrapper_register` re-points a SCHEME, so a later
        // `file_get_contents('foo://x')` runs user code — which is why `io` is
        // the arg-blind colour on the stream family and why this row is a write
        // rather than an `io` of its own.
        "set_error_handler" | "set_exception_handler" | "spl_autoload_register"
        | "spl_autoload_unregister" | "stream_wrapper_register" | "stream_wrapper_unregister"
        | "stream_wrapper_restore" | "register_shutdown_function" | "register_tick_function"
        | "unregister_tick_function" => Some(GLOBAL_WRITE),
        // The procedural database families (effects_gaps.md's last seeding gap).
        // `io.db` has existed since ADR-0018 and `PDO`'s methods return it; the
        // procedural spellings returned nothing, so `mysqli_query($c, $sql)` in
        // a declared-pure function said nothing while `$pdo->query($sql)` did.
        //
        // The rule is **talks to the server**: opening or closing a connection,
        // sending a statement, and the transaction control that sends `COMMIT`
        // or `ROLLBACK`. Async sends are the same wire traffic under a different
        // name, and `mysqli_poll`/`pg_get_result` read it back.
        "mysqli_connect" | "mysqli_real_connect" | "mysqli_close" | "mysqli_ping"
        | "mysqli_query" | "mysqli_real_query" | "mysqli_multi_query" | "mysqli_execute_query"
        | "mysqli_prepare" | "mysqli_stmt_prepare" | "mysqli_execute" | "mysqli_stmt_execute"
        | "mysqli_stmt_send_long_data" | "mysqli_reap_async_query" | "mysqli_poll"
        | "mysqli_commit" | "mysqli_rollback" | "mysqli_begin_transaction" | "mysqli_autocommit"
        | "pg_connect" | "pg_pconnect" | "pg_close" | "pg_ping" | "pg_connection_reset"
        | "pg_query" | "pg_query_params" | "pg_exec" | "pg_prepare" | "pg_execute"
        | "pg_send_query" | "pg_send_query_params" | "pg_send_prepare" | "pg_send_execute"
        | "pg_get_result" | "pg_cancel_query" | "pg_flush"
        | "pg_copy_from" | "pg_copy_to" | "pg_put_line" | "pg_end_copy"
        | "odbc_connect" | "odbc_pconnect" | "odbc_close" | "odbc_exec" | "odbc_do"
        | "odbc_prepare" | "odbc_execute" | "odbc_commit" | "odbc_rollback"
        | "odbc_autocommit" => Some(IO_DB_LABELS),
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

/// The position of an `array` parameter whose **values are callables**, or
/// `None` (issue #382).
///
/// The last callback shape no mechanical source can see. `preg_replace_callback_array`
/// takes `[pattern => callback, …]` at position 0: arginfo says `array` and
/// stops, because "array of callables" is not a type PHP declares. The
/// `callable` column cannot see it, `invocation_shape` names a positional
/// argument and these are array *values*, and the untyped-variadic-tail rule
/// does not apply because the parameter is neither untyped nor variadic.
///
/// So this is **curated on purpose**, and the curation is the claim: a row says
/// the engine reaches into that array and calls what it finds. The fold seam
/// refuses to fold a call that puts anything in it, which is the same posture
/// the other two shapes get — the difference is only where the knowledge comes
/// from.
///
/// One row today. A second belongs here the moment a builtin is found that
/// invokes array values, and finding one is a reading exercise, not a query:
/// nothing in a signature distinguishes `[$k => $callback]` from `[$k => $v]`.
#[must_use]
pub fn callables_in_array_param(name: &str) -> Option<usize> {
    match name.to_ascii_lowercase().as_str() {
        // `preg_replace_callback_array([$pattern => $callback, …], $subject)`.
        "preg_replace_callback_array" => Some(0),
        _ => None,
    }
}

/// Whether a foldable name's **untyped variadic tail carries data** rather than
/// a callee (issue #382).
///
/// 33 builtins declare a `mixed ...$rest`, and the declared type says nothing
/// about what goes in it. The `array_udiff`/`array_uintersect` family puts its
/// **comparator** there — a callable the engine invokes, invisible to
/// [`param_facts`]'s `callable` column because nothing declares it callable and
/// invisible to [`invocation_shape`] because that table names one fixed index.
/// It is the one callback shape neither table can express, and the fold seam
/// refuses an argument in such a tail unless the name is listed here.
///
/// A row is an argument, not a note: it says the tail is values, and it has to
/// be true for every call, since the seam consults the name and not the site.
///
/// The list is deliberately short. A name that merely *looks* safe does not
/// belong — `array_multisort` takes sort flags AND arrays by reference in the
/// same tail, `call_user_func` takes the callee's own arguments after a callee.
/// Both are excluded by other rules already; neither needs to be argued here,
/// and arguing one would be claiming something about a name the seam never
/// reaches.
///
/// [`param_facts`]: crate::param_facts
/// [`invocation_shape`]: crate::invocation_shape
#[must_use]
pub fn variadic_tail_is_data(name: &str) -> bool {
    match name.to_ascii_lowercase().as_str() {
        // `sprintf`/`printf`'s tail is rendered BY the format string. Each value
        // is cast and substituted; nothing in it is called. (`sprintf` is
        // `REFUSED` for the machine word, not for this, so it folds on a 64-bit
        // engine and the gate has to let it.)
        "sprintf" | "printf" | "vsprintf" | "vprintf" => true,
        _ => false,
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
        // `is_callable(..., string &$callable_name = null)` — the one type
        // predicate with a reference parameter (issue #559).
        "is_callable" => Some(P2),
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
/// * the **array presence/list predicates** (issue #536): `array_key_exists`,
///   its `key_exists` alias, and `array_is_list`. Not [`foldable`] — the
///   allowlist refuses them for their *result* — but every parameter is by
///   value, so an uncertified name was costing the KEY its declared arms at
///   every `array_key_exists($key, $a);` site. `array_all`/`array_any` are
///   deliberately absent: their second parameter is a callback, and what a
///   callback does to the caller's variables is not a by-value question.
/// * the **type predicates** (issue #559): `is_string`, `is_int`, `is_array`,
///   … with their aliases — the very family the DR2 exemption doc names as
///   all-by-value and exempts in guard position, so leaving them uncertified
///   made an unconsumed `is_string($key);` STATEMENT cost `$key` what the
///   same call in an `if` never did. `is_callable` is the family's rowed
///   member: `&$callable_name` puts it in [`out_params`] instead.
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
        // Array presence/list predicates (issue #536): both parameters by value.
        "array_key_exists",
        "key_exists",
        "array_is_list",
        // Type predicates (issue #559): the DR2 family asserts.rs already
        // exempts in guard position, certified for the statement position too.
        // `is_callable` is deliberately ABSENT — `&$callable_name` is by
        // reference at position 2, so it is rowed in `out_params` instead.
        "is_string",
        "is_int",
        "is_integer", // = is_int
        "is_long",    // = is_int
        "is_float",
        "is_double",  // = is_float
        "is_bool",
        "is_array",
        "is_null",
        "is_object",
        "is_scalar",
        "is_numeric",
        "is_iterable",
        "is_countable",
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

#[cfg(test)]
mod tests {
    use crate::fold::{PORTABLE, REFUSED, UNVERIFIED};
    use crate::{
        FailureArms, FailureCause, failure_arms, foldable, invocation_shape, param_facts,
        param_facts_generated, param_facts_mined,
    };
    use super::{
        WrittenWhen, by_value_arg, callables_in_array_param, effect_labels, out_param_written_when,
        out_params, variadic_tail_is_data,
    };

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

    /// Uncatalogued is a real answer and not a leftover.
    ///
    /// Both of this test's previous examples stopped being ones — `proc_open`
    /// when the process family was coloured, `mysqli_query` when the database
    /// families were. What is left is an unknown name and the parts of those
    /// families deliberately left alone, which
    /// `the_database_families_are_coloured_where_they_talk_to_the_server`
    /// argues for by name.
    #[test]
    fn uncatalogued_builtins_are_none() {
        for name in ["some_unknown_fn", "mysqli_error", "mysqli_real_escape_string", "pg_last_error"] {
            assert_eq!(effect_labels(name), None, "{name} must be uncatalogued");
        }
    }

    /// The procedural database families, coloured by one rule: **talks to the
    /// server**.
    ///
    /// `io.db` has existed since ADR-0018 and `PDO`'s methods return it, so
    /// `$pdo->query($sql)` in a declared-pure function was reported while
    /// `mysqli_query($c, $sql)` was silent — the audit's last seeding gap.
    ///
    /// What is deliberately NOT coloured, and why it is a separate question:
    ///
    /// * **error and metadata accessors** (`mysqli_error`, `mysqli_num_rows`,
    ///   `pg_last_error`) read state the extension already holds for a buffered
    ///   result. On an UNBUFFERED one some of them do reach the wire, and
    ///   telling those apart is a property of the call site's earlier
    ///   `MYSQLI_USE_RESULT`, which a name-keyed table cannot see — the same
    ///   shape as `fwrite`'s `STDOUT` destination, deferred for the same reason.
    /// * **`mysqli_real_escape_string`** consults the connection's charset and
    ///   sends nothing.
    /// * **the `*_fetch_*` families**, for the buffered/unbuffered reason above.
    #[test]
    fn the_database_families_are_coloured_where_they_talk_to_the_server() {
        for name in [
            "mysqli_connect", "mysqli_query", "mysqli_multi_query", "mysqli_prepare",
            "mysqli_stmt_execute", "mysqli_commit", "mysqli_rollback", "mysqli_close",
            "pg_connect", "pg_query", "pg_query_params", "pg_send_query", "pg_get_result",
            "pg_copy_from", "pg_close",
            "odbc_connect", "odbc_exec", "odbc_execute", "odbc_commit", "odbc_close",
        ] {
            assert_eq!(effect_labels(name), Some(&["io.db"][..]), "{name} reaches the server");
        }
        // Case-insensitive like every other row. The tail is upper-cased rather
        // than the head: a capitalised-word-underscore-capitalised-word shape
        // reads as a private class name to the leak tripwire, and it is right to.
        assert_eq!(effect_labels("mysqli_QUERY"), Some(&["io.db"][..]));
        // And the deliberate exclusions, so the boundary is asserted rather than
        // implied by absence.
        for name in [
            "mysqli_error",
            "mysqli_num_rows",
            "mysqli_real_escape_string",
            "mysqli_fetch_assoc",
            "pg_last_error",
            "pg_fetch_assoc",
        ] {
            assert_eq!(effect_labels(name), None, "{name} is the buffered/local half");
        }
    }

    /// The process family, whole (effects_gaps.md's seeding gap): every builtin
    /// that starts a child carries `io.process`, and the ones that RELAY the
    /// child's output to the parent's carry `io.output` beside it.
    ///
    /// The split is the whole content of the rows. `exec` captures into its
    /// by-ref array and returns the last line, `shell_exec` returns the output
    /// as a string, `popen`/`proc_open` hand back pipes — none of them writes to
    /// the parent's output, so claiming `io.output` there would convict a
    /// declared-`io.process` function of an effect it does not have.
    #[test]
    fn every_child_process_builtin_is_coloured() {
        for name in ["exec", "shell_exec", "popen", "proc_open"] {
            assert_eq!(effect_labels(name), Some(&["io.process"][..]), "{name} runs a child");
        }
        for name in ["system", "passthru"] {
            assert_eq!(
                effect_labels(name),
                Some(&["io.process", "io.output"][..]),
                "{name} runs a child AND relays its output"
            );
        }
    }

    /// Handler and wrapper registration (effects_gaps.md §5): a write to the
    /// engine's own dispatch table, which every later call in the process reads.
    ///
    /// Paired with the read side of the same table, so the test says what the
    /// colour means rather than repeating the list: registering is
    /// `global.write`, and the eventual invocation is somebody else's effect —
    /// `invocation_shape` is what carries it (ADR-0033).
    #[test]
    fn registering_a_handler_writes_global_state() {
        for name in [
            "set_error_handler",
            "set_exception_handler",
            "spl_autoload_register",
            "spl_autoload_unregister",
            "stream_wrapper_register",
            "stream_wrapper_unregister",
            "stream_wrapper_restore",
            "register_shutdown_function",
            "register_tick_function",
            "unregister_tick_function",
        ] {
            assert_eq!(effect_labels(name), Some(&["global.write"][..]), "{name} writes dispatch state");
        }
        // The seeding pair sits on the same colour for the same reason, and the
        // DRAW stays nondeterministic — writing the RNG state is not reading it.
        assert_eq!(effect_labels("mt_srand"), Some(&["global.write"][..]));
        assert_eq!(effect_labels("mt_rand"), Some(&["nondet.random"][..]));
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

    // ---- The engine countersigns the two hand-transcribed parameter tables ----
    //
    // `param_facts` is `ReflectionFunction` over the resident engine, mined by
    // `cargo xtask mine-param-facts`. Everything below is a claim these tables
    // make that the engine can contradict — which is the property the previous
    // by-ref check did not have: `by_value_arg` falls back to `out_params`, so a
    // name with no row answered "by value" at every position and the loop
    // skipped exactly the omission it was hunting (issue #382).

    /// The anti-vacuity guard, and the reason every test below can be trusted:
    /// a name nobody mined has no facts to disagree with, so an unmined
    /// foldable name is a FAILURE rather than a quiet pass.
    #[test]
    fn every_foldable_name_was_mined() {
        for name in PORTABLE.iter().chain(REFUSED).chain(UNVERIFIED) {
            assert!(
                param_facts_mined(name),
                "{name} is foldable but absent from param_facts.toml — rerun \
                 `cargo xtask mine-param-facts && cargo xtask gen-catalog`; until then \
                 nothing below says anything about it"
            );
        }
    }

    /// **The by-ref precondition, made real.** The fold seam passes arguments by
    /// value, so a callee's by-ref write is lost. That is sound only because
    /// ADR-0077's `out_params` seeding invalidates the argument independently —
    /// `$n = 'x'; str_replace('a', 'b', 'aa', $n)` folds the result and widens
    /// `$n`, which is coarser than PHP's `2` and never wrong. The rule that
    /// makes it sound is therefore: **every by-ref position of a foldable name
    /// is declared**, and here the engine says which positions those are.
    #[test]
    fn every_foldable_names_by_ref_positions_are_declared() {
        for name in PORTABLE.iter().chain(REFUSED).chain(UNVERIFIED) {
            let Some(facts) = param_facts(name) else { continue };
            let declared: &[usize] = out_params(name).unwrap_or(&[]);
            assert_eq!(
                facts.by_ref, declared,
                "{name} folds, and the engine's arginfo disagrees with its `out_params` row: \
                 arginfo says {:?}, the catalog says {declared:?}. A by-ref position with no \
                 row is written by the real call and never invalidated here.",
                facts.by_ref
            );
        }
    }

    /// A row that names a position the engine does not have by-ref would
    /// invalidate a variable PHP never writes — wrong in the other direction,
    /// and just as much a defect. Checked over every mined name, so it also
    /// covers rows for names that are not foldable.
    #[test]
    fn no_out_param_row_claims_a_position_the_engine_denies() {
        // A row, where present, must match. ABSENCE is legal and deliberate:
        // ADR-0077 §3 restricts the table to the fixed positional refs the
        // analysis needs, and 98 by-ref-bearing builtins carry no row on
        // purpose. Requiring one for each would be 98 new claims about names
        // nothing asks about — a different decision, and not this test's.
        //
        // A row on a name the engine gives no by-ref parameter at all is still
        // caught, because that name now has a row too, with an empty `by_ref`.
        for (name, facts) in param_facts_generated::PARAM_FACTS {
            let Some(declared) = out_params(name) else { continue };
            assert_eq!(
                declared, facts.by_ref,
                "{name}'s `out_params` row is {declared:?}, the engine's arginfo is {:?}",
                facts.by_ref
            );
        }

    }

    /// **Every by-value claim, at every position, countersigned by the engine.**
    ///
    /// `by_value_arg` is the predicate consumers ask before keeping a
    /// variable's fact across a call: `Some(true)` says the callee cannot write
    /// through that argument. It answers from two hand-maintained sources — a
    /// certified-extra list, and `out_params` for everything foldable — and
    /// until this table existed nothing could contradict either. A `Some(true)`
    /// at a position the engine declares `&$` means the call writes through an
    /// argument while the analysis carries the old value forward, which is a
    /// wrong fact rather than a missing one.
    ///
    /// Positional, because the predicate is: `preg_match_all` is by value at 0
    /// and 1 and by reference at 2, and a whole-name reading of it would be
    /// both wrong and vacuous.
    #[test]
    fn every_by_value_claim_matches_the_engine_at_that_position() {
        let mut claims = 0usize;
        for (name, facts) in param_facts_generated::PARAM_FACTS {
            for position in 0..facts.params.len() {
                let by_ref = facts.by_ref.contains(&position);
                match by_value_arg(name, position) {
                    // No claim at all: nothing to contradict.
                    None => {}
                    Some(true) => {
                        claims += 1;
                        assert!(
                            !by_ref,
                            "{name} is certified by value at {position} and the engine declares \
                             it `&$`: a caller would keep a fact the call overwrites"
                        );
                    }
                    Some(false) => {
                        claims += 1;
                        assert!(
                            by_ref,
                            "{name} is claimed by REFERENCE at {position} and the engine declares \
                             it by value: the caller drops a fact the call cannot touch"
                        );
                    }
                }
            }
        }
        assert!(claims > 200, "the predicate should answer widely, saw {claims} claims");
    }

    /// A foldable name with a **variadic tail the engine types `mixed`** is the
    /// one shape neither parameter table can rule on: `array_udiff` hides its
    /// comparator exactly there, and no declared type gives it away.
    ///
    /// Such a name may fold only if [`variadic_tail_is_data`] argues the tail
    /// carries values — the same predicate the seam's shape gate consults, so
    /// the catalog and the seam cannot disagree about which names are argued.
    /// (An earlier revision kept the list here, privately, which made two
    /// sources of truth for one claim: the disease this whole slice is about.)
    #[test]
    fn a_variadic_mixed_tail_on_a_foldable_name_is_argued_for() {
        for name in PORTABLE.iter().chain(REFUSED).chain(UNVERIFIED) {
            let Some(facts) = param_facts(name) else { continue };
            let untyped_tail = facts
                .variadic
                .iter()
                .any(|&i| facts.params.get(i).is_some_and(|t| *t == "mixed"));
            if !untyped_tail {
                continue;
            }
            assert!(
                variadic_tail_is_data(name),
                "{name} folds and takes an untyped variadic tail, which is where the \
                 `array_udiff` family hides its comparator. Say why this one is data."
            );
        }
        // Not vacuous in either direction: one foldable name has such a tail and
        // is argued, and the family the rule exists for is neither argued nor
        // foldable.
        assert!(variadic_tail_is_data("sprintf") && foldable("sprintf"));
        for name in ["array_udiff", "array_uintersect", "array_diff_ukey"] {
            assert!(!variadic_tail_is_data(name), "{name} hides a comparator in its tail");
            assert!(!foldable(name), "{name} is not on the allowlist");
        }
    }

    /// The third callback shape, and the only one that is a **list**.
    ///
    /// `preg_replace_callback_array` takes `[pattern => callback, …]`: arginfo
    /// says `array` and stops, because "array of callables" is not a type PHP
    /// declares. So no mechanical rule can find it — not the `callable` column,
    /// not `invocation_shape` (these are array *values*, not a positional
    /// argument), not the untyped-variadic-tail rule (the parameter is neither).
    ///
    /// The row is the claim, and this pins both halves of it: the name is
    /// curated, and it is not on the folding allowlist — so the seam's refusal
    /// is defence for a future admission rather than something load-bearing
    /// today, which is exactly the posture the other two shapes have.
    #[test]
    fn the_array_of_callables_is_curated_and_not_admitted() {
        assert_eq!(callables_in_array_param("preg_replace_callback_array"), Some(0));
        assert_eq!(callables_in_array_param("PREG_REPLACE_CALLBACK_ARRAY"), Some(0));
        assert!(!foldable("preg_replace_callback_array"));
        // Not a blanket rule about array parameters: the names that take an
        // array of VALUES are untouched, or `count`/`implode` would stop folding.
        for name in ["count", "implode", "array_merge", "array_filter"] {
            assert_eq!(callables_in_array_param(name), None, "{name} takes values, not callees");
        }
        // …and every foldable name is clear of it, which is what lets the seam's
        // rule cost nothing today.
        for name in PORTABLE.iter().chain(REFUSED).chain(UNVERIFIED) {
            assert_eq!(callables_in_array_param(name), None, "{name} folds and carries callees");
        }
    }

    /// Every `invocation_shape` row names a position the engine declares
    /// callable. A row pointing at the wrong index would make the effects and
    /// throws passes read the wrong argument as the callback.
    #[test]
    fn every_invocation_shape_row_is_a_declared_callable_position() {
        for (name, facts) in param_facts_generated::PARAM_FACTS {
            let Some(shape) = invocation_shape(name) else { continue };
            assert!(
                facts.callable.contains(&shape.callback_param),
                "{name}'s invocation_shape names position {}, and the engine declares callables \
                 at {:?}",
                shape.callback_param,
                facts.callable
            );
        }
    }

    /// …and the other direction: a name the engine says takes a callable is
    /// either rowed or **named here as not invoking during the call**. The list
    /// is closed, so a new callable-bearing builtin cannot arrive unexamined —
    /// which is the completeness `no_foldable_name_invokes_a_callback` could
    /// never claim on its own.
    #[test]
    fn every_declared_callable_builtin_is_rowed_or_excluded() {
        /// Names that take a callable and get no `invocation_shape` row,
        /// grouped by why. ADR-0033's "deliberate exclusions" paragraph is the
        /// prose version; this is the enforced list.
        const NOT_INVOKED_HERE: &[&str] = &[
            // Registration: the callable is stored and invoked later, by the
            // engine and not by this call, so there is no call-site effect to
            // attribute (ADR-0033).
            "set_error_handler",
            "set_exception_handler",
            "spl_autoload_register",
            "spl_autoload_unregister",
            "register_tick_function",
            "unregister_tick_function",
            "header_register_callback",
            "readline_callback_handler_install",
            "readline_completion_function",
            "libxml_set_external_entity_loader",
            "ldap_set_rebind_proc",
            "session_set_save_handler",
            "opcache_jit_blacklist",
            "xml_set_character_data_handler",
            "xml_set_default_handler",
            "xml_set_element_handler",
            "xml_set_end_namespace_decl_handler",
            "xml_set_external_entity_ref_handler",
            "xml_set_notation_decl_handler",
            "xml_set_processing_instruction_handler",
            "xml_set_start_namespace_decl_handler",
            "xml_set_unparsed_entity_decl_handler",
            // Immediate, and unrowed only because no consumer needs the shape
            // yet — the callback's arguments are the forwarded arguments, which
            // `ArgSource` has no spelling for.
            "forward_static_call",
            "forward_static_call_array",
            // Immediate, extension-scoped: the replacement callback runs during
            // the call. Rowing it is a `mbstring` slice of its own.
            "mb_ereg_replace_callback",
        ];
        for (name, facts) in param_facts_generated::PARAM_FACTS {
            if facts.callable.is_empty() || invocation_shape(name).is_some() {
                continue;
            }
            assert!(
                NOT_INVOKED_HERE.contains(name),
                "{name} declares a callable at {:?} with no `invocation_shape` row and no entry \
                 in NOT_INVOKED_HERE — say which it is",
                facts.callable
            );
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

    /// Issue #536: the presence/list predicates are certified per NAME, so the
    /// KEY position answers `true` as much as the array does — an uncertified
    /// name made `array_key_exists($key, $a);` forget `$key`. The callback
    /// family stays uncertified, deliberately.
    #[test]
    fn by_value_arg_certifies_the_array_presence_predicates() {
        for f in ["array_key_exists", "key_exists"] {
            for p in 0..2 {
                assert_eq!(by_value_arg(f, p), Some(true), "{f} position {p} is by value");
            }
        }
        assert_eq!(by_value_arg("array_is_list", 0), Some(true));
        for f in ["array_all", "array_any"] {
            assert_eq!(by_value_arg(f, 1), None, "{f} takes a callback; nothing is certified");
        }
    }

    /// Issue #559: the DR2 family, certified per NAME so the statement
    /// position answers what guard position always assumed. None becomes
    /// foldable — the fold allowlist is about the RESULT, and stays as it was.
    #[test]
    fn by_value_arg_certifies_the_type_predicates() {
        for f in ["is_string", "is_int", "is_integer", "is_long", "is_float", "is_double",
                  "is_bool", "is_array", "is_null", "is_object", "is_scalar", "is_numeric",
                  "is_iterable", "is_countable"] {
            assert_eq!(by_value_arg(f, 0), Some(true), "{f} is by value");
            assert!(!foldable(f), "{f} must NOT become foldable");
        }
        // `is_callable` is the family's rowed member: `&$callable_name` sits at
        // position 2, so the row answers positionally like `preg_match`'s.
        assert_eq!(by_value_arg("is_callable", 0), Some(true));
        assert_eq!(by_value_arg("is_callable", 1), Some(true), "$syntax_only is by value");
        assert_eq!(by_value_arg("is_callable", 2), Some(false), "$callable_name is by ref");
        assert!(!foldable("is_callable"));
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
        // A by-ref row is not an effect color: `similar_text` writes argument 2
        // and touches nothing global. (`preg_match` used to be the example here
        // and stopped being one when issue #382 admitted it — a foldable name is
        // catalogued-PURE, `Some(&[])`, not uncatalogued.)
        assert_eq!(out_params("similar_text"), Some(&[2][..]));
        assert_eq!(effect_labels("similar_text"), None);
        assert_eq!(effect_labels("preg_match"), Some(&[][..]), "foldable is catalogued-pure");
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
}
