//! `mine-constants`: build the committed ENGINE-CONSTANT table (ADR-0094 §2).
//!
//! # Why a table and not a sidecar call
//!
//! `ArgValue::GlobalConst` is unproven by construction (issue #168), so
//! `PHP_INT_MAX` and `JSON_THROW_ON_ERROR` both dump `unknown`. The obvious fix
//! — asking the sidecar for `constant('PHP_INT_MAX')` — extends the fold seam's
//! recognizer surface, which ADR-0060/ADR-0066 fence: the analyzed source would
//! name a symbol and the engine would evaluate it. ADR-0094 §2 rules that
//! engine constants come from a **generated, committed table** instead, in the
//! shape of the functionMap declared-return floor (ADR-0069) and the
//! per-parameter facts (issue #382). This is that table's miner.
//!
//! # Two sources, because one of them cannot answer the question
//!
//! * **Values** come from the resident engine's `get_defined_constants(true)`
//!   (`mine_constants.php`), which is also what says *which extension*
//!   registered each name. The TOP build answers, and `[meta]` records which —
//!   but it is not the only build asked. ADR-0094 §2 says the generator "runs
//!   over the PHP minors the corpus harness already scopes", and a value mined
//!   from one engine is a claim about every one of them: any name whose value
//!   DISAGREES across the engines the run was given is refused outright
//!   ([`value_moves`]), because a row is a spec-fixed literal or it is nothing.
//! * **Version ranges** cannot come from one build: `since` is the question
//!   "did this name exist at 8.1", and an 8.5 engine has no opinion. They come
//!   from a php-src checkout instead — the per-minor `PHP-8.x` branches, read
//!   for the two places a constant is declared (a `.stub.php` global `const`,
//!   or a `REGISTER_*_CONSTANT("NAME", …)` in C).
//!
//! The presence scan is deliberately **fallible in one direction only**. Macro
//! token-pasting hides some registrations from any grep, so a name the scan
//! cannot see at the TOP mined minor is a name the scan is blind to — and such
//! a row gets **no range at all** rather than a guessed one. A row is gated only
//! when the scan saw the name appear and stay ([`arrival`]).
//!
//! # What is admitted, and what is refused
//!
//! ADR-0094 §3 splits constants into classes, and only one of them belongs in a
//! mined table: the **spec-fixed** roster, whose value is the same on every host
//! that has the constant at all. The other classes are refused here and answered
//! by the resolver's own hand-written roster (`steins-infer`'s `global_consts`),
//! because their answer is a union or a derivation, not a mined literal:
//!
//! * `PLATFORM_RULED` — the host/width/version constants ADR-0094 §3 tabulates.
//! * `BUILD_DEPENDENT` — a value that encodes the *build*: the linked library's
//!   version, a compile flag, a platform limit. These have no Steins answer at
//!   all today; refusing them is what keeps the table's rows `Verified`.
//! * [`BUILD_DEPENDENT_FAMILIES`] — the same property held by a whole family
//!   whose numbers are a C library's (`glob.h`, ICU's `utypes.h`, `libpq-fe.h`),
//!   refused by prefix so the next member the library gains is refused too.
//! * `REFUSED_EXTENSIONS` — whole families whose numbers come from the C library
//!   (`sockets`, `pcntl`, `posix`) or from the parser generator (`tokenizer`),
//!   so the NAME is stable while the value is not.
//! * Non-scalar values (`STDIN` and friends are resources; `INF`/`NAN` are
//!   floats the value domain cannot compare).
//!
//! A **path tripwire** backs the roster up: an admitted string value that
//! contains one of the mining machine's own directories fails the whole run.
//! A roster is a list somebody maintains; the tripwire is what makes a *missed*
//! entry loud instead of a committed absolute path.
//!
//! # Usage
//!
//! ```text
//! cargo xtask mine-constants [PHP_SRC_DIR] [--php PATH]…
//! ```
//!
//! `PHP_SRC_DIR` (or `$STEINS_PHP_SRC`) is a php-src checkout with the
//! `origin/PHP-8.x` branches fetched; without one the run mines values only and
//! every row is rangeless. `--php PATH` (repeatable, or `$STEINS_MINE_PHP` as a
//! `:`-separated list) names the engines to mine; with none given the run asks
//! the `php` on PATH alone, and then **no name can be refused as a value move** —
//! a single engine agrees with itself. The engines' EXTENSION sets need not
//! agree: a name the lower build does not have is simply not compared. Output:
//! `docs/research/phpsrc-mining/constants.toml` (source of record).
//! `cargo xtask gen-catalog` turns it into the shipped Rust table.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::process::Command;

use crate::corpus::repo_root;

/// The PHP minors the presence scan reads, low to high. The floor is the
/// workspace's own declared floor (ADR-0011) and the ceiling is the branch the
/// mining engine belongs to: a range wider at the bottom would claim knowledge
/// of a minor Steins does not analyze for, and one wider at the top would read a
/// branch no released engine matches.
const MINED_MINORS: &[(u16, u16)] = &[(8, 1), (8, 2), (8, 3), (8, 4), (8, 5)];

/// Constants ADR-0094 §3 answers by CLASS rather than by mined value — the host
/// sets, the 64-bit integer width, and the `PhpTarget`-derived engine version.
/// The resolver owns every one of them; a mined row would be this machine's
/// answer to a question about the deployment target.
const PLATFORM_RULED: &[&str] = &[
    // Closed host sets and the open one (§3, "closed host sets" / "open host sets").
    "PHP_EOL",
    "DIRECTORY_SEPARATOR",
    "PATH_SEPARATOR",
    "PHP_OS_FAMILY",
    "PHP_OS",
    // The engine version, derived from `PhpTarget` (§3, "engine version").
    "PHP_VERSION",
    "PHP_MAJOR_VERSION",
    "PHP_MINOR_VERSION",
    "PHP_RELEASE_VERSION",
    "PHP_VERSION_ID",
    "PHP_EXTRA_VERSION",
    // Integer width and float limits — the 64-bit literals, always (§3.1).
    "PHP_INT_MAX",
    "PHP_INT_MIN",
    "PHP_INT_SIZE",
    "PHP_FLOAT_DIG",
    "PHP_FLOAT_EPSILON",
    "PHP_FLOAT_MAX",
    "PHP_FLOAT_MIN",
];

/// Constants whose value encodes the BUILD, not the language: the version of a
/// linked library, a compile-time flag, an installation path, a platform limit.
///
/// Every one of these is a value no target-independent claim can be made about,
/// and none has a Steins answer today — the resolver declines them the same way
/// it declines a name with no row. The roster is by NAME and not by pattern on
/// purpose: `CURL_VERSION_HTTP2` is a spec-fixed bit flag and `LIBXML_VERSION`
/// is the linked libxml2's version, and no regex over `VERSION` tells them
/// apart.
const BUILD_DEPENDENT: &[&str] = &[
    // Installation layout — the names that would put this machine's directories
    // in a committed file. The path tripwire below is the second line of defence.
    "DEFAULT_INCLUDE_PATH",
    "PEAR_INSTALL_DIR",
    "PEAR_EXTENSION_DIR",
    "PHP_EXTENSION_DIR",
    "PHP_PREFIX",
    "PHP_BINDIR",
    "PHP_SBINDIR",
    "PHP_MANDIR",
    "PHP_LIBDIR",
    "PHP_DATADIR",
    "PHP_SYSCONFDIR",
    "PHP_LOCALSTATEDIR",
    "PHP_CONFIG_FILE_PATH",
    "PHP_CONFIG_FILE_SCAN_DIR",
    "PHP_BINARY",
    // Build identity and compile-time flags.
    "PHP_BUILD_DATE",
    "PHP_BUILD_PROVIDER",
    "PHP_SAPI",
    "PHP_SHLIB_SUFFIX",
    "PHP_DEBUG",
    "PHP_ZTS",
    "PHP_CLI_PROCESS_TITLE",
    "PHP_MAXPATHLEN",
    "PHP_FD_SETSIZE",
    "ZEND_THREAD_SAFE",
    "ZEND_DEBUG_BUILD",
    "ZEND_VM_KIND",
    // Linked-library versions and capabilities.
    "OPENSSL_VERSION_NUMBER",
    "OPENSSL_VERSION_TEXT",
    "OPENSSL_DEFAULT_STREAM_CIPHERS",
    "PCRE_VERSION",
    "PCRE_VERSION_MAJOR",
    "PCRE_VERSION_MINOR",
    "PCRE_JIT_SUPPORT",
    "ZLIB_VERSION",
    "ZLIB_VERNUM",
    "MB_ONIGURUMA_VERSION",
    "GD_VERSION",
    "GD_EXTRA_VERSION",
    "GD_MAJOR_VERSION",
    "GD_MINOR_VERSION",
    "GD_RELEASE_VERSION",
    "GMP_VERSION",
    "ICONV_IMPL",
    "ICONV_VERSION",
    "INTL_ICU_VERSION",
    "INTL_ICU_DATA_VERSION",
    "LIBXML_VERSION",
    "LIBXML_DOTTED_VERSION",
    "LIBXML_LOADED_VERSION",
    "LIBXSLT_DOTTED_VERSION",
    "LIBEXSLT_DOTTED_VERSION",
    "CURLVERSION_NOW",
    "ODBC_TYPE",
    "PDO_ODBC_TYPE",
    "PGSQL_LIBPQ_VERSION",
    "PGSQL_LIBPQ_VERSION_STR",
    "READLINE_LIB",
    "XML_SAX_IMPL",
    "MYSQLI_IS_MARIADB",
    "SODIUM_LIBRARY_VERSION",
    "SODIUM_LIBRARY_MAJOR_VERSION",
    "SODIUM_LIBRARY_MINOR_VERSION",
    "BROTLI_VERSION_TEXT",
    "BROTLI_DICTIONARY_SUPPORT",
    "GNUPG_GPGME_VERSION",
    "PASSWORD_ARGON2_PROVIDER",
    "pcov\\version",
    // libpq's `ExecStatusType`, the one libpq enum whose members PHP spells
    // without a shared prefix ([`BUILD_DEPENDENT_FAMILIES`] catches the other
    // five). By name and not by pattern because `PGSQL_COMMAND_OK` and
    // `PGSQL_CONNECT_ASYNC` are adjacent spellings of different things, and only
    // the first is PostgreSQL's number. The residual risk is the one a roster
    // always carries: the next `PGRES_*` PostgreSQL adds needs a line here.
    "PGSQL_EMPTY_QUERY",
    "PGSQL_COMMAND_OK",
    "PGSQL_TUPLES_OK",
    "PGSQL_TUPLES_CHUNK",
    "PGSQL_COPY_OUT",
    "PGSQL_COPY_IN",
    "PGSQL_BAD_RESPONSE",
    "PGSQL_NONFATAL_ERROR",
    "PGSQL_FATAL_ERROR",
    // A value that is the OR of a set php-src has changed inside the mined
    // window: `E_ALL` lost `E_STRICT`'s bit when `E_STRICT` left. `since`/`until`
    // cannot express this — they say when a NAME exists, and this name exists
    // throughout with two different values — so the honest answer is no answer.
    "E_ALL",
];

/// **A C library's numbering, held by a whole FAMILY**: `(extension, prefix,
/// the header the numbers come from)`.
///
/// [`REFUSED_EXTENSIONS`] makes this argument about an extension and
/// [`BUILD_DEPENDENT`] makes it about a name; these are the cases where the
/// property belongs to neither. `standard` is mostly PHP's own numbers and
/// `GLOB_*` is not; `intl` spells ICU's error enum and its own formatter flags
/// with equally ordinary names. A prefix is what makes the refusal survive the
/// library's next release: a name-by-name roster admits whatever member gets
/// added next, which is precisely how these rows were admitted in the first
/// place.
///
/// The extension is part of the key so a prefix stays as narrow as the claim:
/// `U_` is ICU's only inside `intl`.
const BUILD_DEPENDENT_FAMILIES: &[(&str, &str, &str)] = &[
    // `glob.h`. The flag bits are the C library's own and the libraries
    // disagree: glibc's `GLOB_BRACE` is 1024 and Darwin's is 128, and
    // `GLOB_NOESCAPE`, `GLOB_ONLYDIR` and `GLOB_MARK` diverge the same way.
    // `GLOB_AVAILABLE_FLAGS` is their OR, so it moves whenever any member does —
    // which is how the multi-engine diff first noticed the family.
    ("standard", "GLOB_", "the C library's glob.h"),
    // ICU's `UErrorCode`. Every `U_*` is an ordinal ICU assigns, so adding one
    // code renumbers the `_LIMIT` sentinel that closes its section and every
    // section above it — `U_FMT_PARSE_ERROR_LIMIT` is 65812 against one ICU and
    // 65825 against another. The NAME is stable while the value tracks the
    // linked ICU, which is `tokenizer`'s property in a family's shape.
    ("intl", "U_", "the linked ICU's utypes.h"),
    // libpq's enums, five of the six by prefix. `PGSQL_ERRORS_SQLSTATE` is 3
    // against one libpq and 0 against an older one, because `PQERRORS_SQLSTATE`
    // arrived in PostgreSQL 12 and the enum had a different shape before it.
    ("pgsql", "PGSQL_CONNECTION_", "libpq's ConnStatusType"),
    ("pgsql", "PGSQL_POLLING_", "libpq's PostgresPollingStatusType"),
    ("pgsql", "PGSQL_TRANSACTION_", "libpq's PGTransactionStatusType"),
    ("pgsql", "PGSQL_ERRORS_", "libpq's PGVerbosity"),
    ("pgsql", "PGSQL_SHOW_CONTEXT_", "libpq's PGContextVisibility"),
    ("pgsql", "PGSQL_DIAG_", "libpq-fe.h's PG_DIAG_* field codes"),
];

/// Extensions whose constant VALUES are not a property of PHP, so no mined
/// number is a target-independent fact:
///
/// * `sockets`, `pcntl`, `posix` take their numbers from the C library of the
///   machine PHP was built on. `SIGCHLD` is 17 on Linux and 20 on Darwin;
///   `SO_REUSEPORT` is 15 and 0x200. A mined row would be this machine's
///   `errno.h`, which is the very substitution ADR-0094 §3 exists to refuse.
/// * `tokenizer`'s `T_*` are ordinals the parser generator assigns, so adding
///   one token renumbers its neighbours between minors — a value that changes
///   while the name stays, which `since`/`until` cannot express.
///
/// Refused as extensions rather than as names because the property belongs to
/// the whole family, and a name-by-name roster would silently admit the next
/// constant the extension gains.
const REFUSED_EXTENSIONS: &[&str] = &["pcntl", "posix", "sockets", "tokenizer"];

/// The miner's JSON shape.
#[derive(serde::Deserialize)]
struct Mined {
    php: String,
    extensions: Vec<String>,
    constants_total: usize,
    rows: BTreeMap<String, MinedRow>,
}

#[derive(serde::Deserialize)]
struct MinedRow {
    ext: String,
    #[serde(rename = "type")]
    ty: String,
    value: String,
}

/// Why a mined name is not in the table.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Refusal {
    /// ADR-0094 §3 answers it by class; the resolver owns it.
    Platform,
    /// The value is a property of the build, not of PHP.
    Build,
    /// Not a scalar — `STDIN` and its two siblings are resources, `INF`/`NAN`
    /// are floats the value domain cannot compare.
    Unrepresentable,
    /// The engines the run was given do not agree on the value, inside the very
    /// minor range the row would claim. Found rather than rostered, which is the
    /// point: [`BUILD_DEPENDENT`] is a list somebody maintains, and this is the
    /// check that makes a missing entry a refused row instead of a wrong one.
    ValueMoves,
}

/// One admitted row, as the source of record spells it.
struct Row {
    ext: String,
    ty: String,
    value: String,
    /// The lowest mined minor from which the presence scan saw the name, when it
    /// saw it arrive strictly above the mined floor. `None` = no lower gate.
    since: Option<(u16, u16)>,
}

/// One engine the run asked, and what it answered. The TOP engine supplies the
/// table's values and extension set; every engine takes part in [`value_moves`].
struct Engine {
    bin: String,
    minor: (u16, u16),
    mined: Mined,
}

/// Entry point for `cargo xtask mine-constants`.
pub fn run(php_src: Option<&str>, php_bins: &[String]) -> Result<(), String> {
    let extensions = catalog_extensions()?;
    let mut engines = Vec::new();
    for bin in php_binaries(php_bins) {
        let mined = run_miner(&bin, &extensions)?;
        let minor = php_minor(&mined.php)?;
        println!(
            "mine-constants: PHP {} — {} constants over {} extensions",
            mined.php,
            mined.constants_total,
            mined.extensions.len()
        );
        engines.push(Engine { bin, minor, mined });
    }
    // The TOP minor is the one whose values the table carries: `since` asks
    // whether a name existed at the floor and the engine that HAS the most names
    // is the one that can be asked about the most rows. Ties keep the order given.
    engines.sort_by_key(|e| e.minor);
    let top = engines.pop().ok_or("no PHP engine to mine")?;
    let mined = &top.mined;

    let php_src = php_src
        .map(str::to_owned)
        .or_else(|| std::env::var("STEINS_PHP_SRC").ok())
        .filter(|d| !d.is_empty());
    // The scan's own reliability is measured per extension, so it needs the
    // extension each mined name belongs to before it can judge an absence.
    //
    // Over every MINED name, not only the admitted ones: coverage asks whether
    // the scan can see this extension's registrations at all, and a roster entry
    // is an answer to a different question. Counting only the admitted names
    // would let a refusal RAISE coverage — refusing `intl`'s 141 `U_*`, which the
    // scan cannot see, lifts the extension over the floor and manufactures a
    // `since = "8.2"` for `ULOC_ACTUAL_LOCALE`, which 8.1 registers through
    // `COLLATOR_EXPOSE_CONST` and the scan is blind to. A blind spot must not
    // become invisible because the names that revealed it stopped being mined.
    let mut by_ext: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, m) in &mined.rows {
        if m.ty != "unrepresentable" {
            by_ext.entry(m.ext.clone()).or_default().push(normalize_const_fqn(name));
        }
    }
    let presence = match &php_src {
        Some(dir) => Some(scan_presence(dir, &by_ext)?),
        None => {
            println!(
                "mine-constants: no php-src checkout given (argument or $STEINS_PHP_SRC) — \
                 every row will be rangeless"
            );
            None
        }
    };

    let dirs = machine_dirs(&top.bin);
    let mut rows: BTreeMap<String, Row> = BTreeMap::new();
    let mut refused: BTreeMap<String, Refusal> = BTreeMap::new();
    for (name, m) in &mined.rows {
        if let Some(r) = refuse(name, &m.ty, &m.ext) {
            refused.insert(name.clone(), r);
            continue;
        }
        let value = decode_value(name, &m.ty, &m.value)?;
        if m.ty == "string" {
            leak_tripwire(name, &value, &dirs)?;
        }
        let since = presence.as_ref().and_then(|p| arrival(p, &normalize_const_fqn(name), &m.ext));
        rows.insert(
            normalize_const_fqn(name),
            Row { ext: m.ext.clone(), ty: m.ty.clone(), value, since },
        );
    }

    // The multi-engine diff, LAST: it judges the rows as they would be written,
    // so a name the rosters already refused is not diffed, and a `since` the
    // presence scan found is what decides which engines a disagreement counts at.
    let moves = value_moves(&top, &engines, &rows)?;
    for name in moves.keys() {
        rows.remove(name);
        refused.insert(name.clone(), Refusal::ValueMoves);
    }

    let out = render(&top, &engines, presence.as_ref(), &rows, &refused, &moves);
    let dst = repo_root().join("docs/research/phpsrc-mining/constants.toml");
    std::fs::write(&dst, &out).map_err(|e| format!("write {}: {e}", dst.display()))?;
    let gated = rows.values().filter(|r| r.since.is_some()).count();
    println!(
        "mine-constants: {} rows ({gated} version-gated), {} refused → {}",
        rows.len(),
        refused.len(),
        dst.display()
    );
    Ok(())
}

/// The extension set the FUNCTION catalog mines (ADR-0094 §2: "the extension set
/// is the function catalog's"), read off the parameter-facts source of record so
/// the two tables cannot drift into covering different builds.
fn catalog_extensions() -> Result<Vec<String>, String> {
    let src = repo_root().join("docs/research/phpsrc-mining/param_facts.toml");
    let text = std::fs::read_to_string(&src).map_err(|e| format!("read {}: {e}", src.display()))?;
    #[derive(serde::Deserialize)]
    struct Doc {
        meta: Meta,
    }
    #[derive(serde::Deserialize)]
    struct Meta {
        extensions: Vec<String>,
    }
    let doc: Doc = toml::from_str(&text).map_err(|e| format!("parse {}: {e}", src.display()))?;
    Ok(doc.meta.extensions)
}

/// Run the PHP miner on one engine, with the catalog's extension allowlist.
fn run_miner(bin: &str, extensions: &[String]) -> Result<Mined, String> {
    let script = repo_root().join("docs/research/phpsrc-mining/mine_constants.php");
    let allow = serde_json::to_string(extensions).map_err(|e| format!("encode allowlist: {e}"))?;
    let out = Command::new(bin)
        .arg(&script)
        .arg(&allow)
        .output()
        .map_err(|e| format!("run {bin} {}: {e}", script.display()))?;
    if !out.status.success() {
        return Err(format!("miner failed on {bin}: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("parse miner JSON: {e}"))
}

/// **The engines to ask**: every `--php PATH`, else `$STEINS_MINE_PHP` split on
/// `:`, else the `php` on PATH alone.
///
/// One engine is a working run and not a degraded one — it is what the table has
/// always been mined from — but it refuses nothing as a value move, and the
/// header says so, because a single engine cannot disagree with itself.
fn php_binaries(php_bins: &[String]) -> Vec<String> {
    if !php_bins.is_empty() {
        return php_bins.to_vec();
    }
    match std::env::var("STEINS_MINE_PHP") {
        Ok(list) if !list.is_empty() => {
            list.split(':').filter(|s| !s.is_empty()).map(str::to_owned).collect()
        }
        _ => vec!["php".to_owned()],
    }
}

/// `"8.4.25"` → `(8, 4)`. A version the miner reports and this cannot read is a
/// failed run: the minor is what decides whether a disagreement lies inside the
/// range a row claims, and guessing it would decide that wrongly.
fn php_minor(version: &str) -> Result<(u16, u16), String> {
    let mut parts = version.split('.');
    let maj = parts.next().and_then(|p| p.parse().ok());
    let min = parts.next().and_then(|p| p.parse().ok());
    match (maj, min) {
        (Some(maj), Some(min)) => Ok((maj, min)),
        _ => Err(format!("engine reported PHP version `{version}`, which has no major.minor")),
    }
}

/// **The names the given engines disagree about**, name → the value each engine
/// that has it reported, in ascending minor order.
///
/// A row says its value is the same on every host and every minor the target may
/// span; two engines that both have the name and report different numbers are a
/// direct counter-example, and the row is refused rather than mined from
/// whichever engine happened to answer first. The four names this first caught
/// are exactly the shape the argument predicts: `IMAGETYPE_COUNT` counts a list
/// php-src appended to, `PASSWORD_BCRYPT_DEFAULT_COST` was raised from 10 to 12,
/// `IDNA_DEFAULT` and `FILTER_FLAG_GLOBAL_RANGE` changed bits under names that
/// stayed.
///
/// Two clauses keep the check from refusing rows that are in fact fine:
///
/// * an engine that does NOT have the name says nothing — the extension is
///   absent from that build, and absence is the version gate's question, not
///   this one;
/// * an engine BELOW the row's `since` says nothing either — the row does not
///   speak for that minor, so a disagreement there is outside its claim. This is
///   the "unless the range scan explains it" clause, and it is the only thing
///   `since` is allowed to excuse: a disagreement at or above `since` is inside
///   the row's own range and no scan explains it away.
fn value_moves(
    top: &Engine,
    others: &[Engine],
    rows: &BTreeMap<String, Row>,
) -> Result<BTreeMap<String, Vec<(String, String)>>, String> {
    // The engines key their rows the way the miner reported them and the table's
    // key is normalized, so each engine is re-keyed once rather than per row.
    let mut byminor: Vec<((u16, u16), String, BTreeMap<String, String>)> = Vec::new();
    for e in others.iter().chain(std::iter::once(top)) {
        let mut vals = BTreeMap::new();
        for (n, m) in &e.mined.rows {
            let key = normalize_const_fqn(n);
            if rows.contains_key(&key) {
                vals.insert(key, decode_value(n, &m.ty, &m.value)?);
            }
        }
        byminor.push((e.minor, e.mined.php.clone(), vals));
    }

    let mut out: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for (name, row) in rows {
        let mut seen: Vec<(String, String)> = Vec::new();
        for (minor, version, vals) in &byminor {
            if row.since.is_some_and(|s| *minor < s) {
                continue;
            }
            let Some(v) = vals.get(name) else { continue };
            seen.push((version.clone(), v.clone()));
        }
        if seen.iter().any(|(_, v)| *v != row.value) {
            out.insert(name.clone(), seen);
        }
    }
    Ok(out)
}

/// Whether a mined name is refused, and why.
fn refuse(name: &str, ty: &str, ext: &str) -> Option<Refusal> {
    if PLATFORM_RULED.contains(&name) {
        return Some(Refusal::Platform);
    }
    if BUILD_DEPENDENT.contains(&name) || REFUSED_EXTENSIONS.contains(&ext) {
        return Some(Refusal::Build);
    }
    if BUILD_DEPENDENT_FAMILIES.iter().any(|(e, p, _)| *e == ext && name.starts_with(p)) {
        return Some(Refusal::Build);
    }
    (ty == "unrepresentable").then_some(Refusal::Unrepresentable)
}

/// Turn the miner's wire spelling into the source of record's: a base64 string
/// value becomes the bytes it stands for, everything else passes through.
fn decode_value(name: &str, ty: &str, wire: &str) -> Result<String, String> {
    if ty != "string" {
        return Ok(wire.to_owned());
    }
    let bytes = base64_decode(wire)
        .ok_or_else(|| format!("constant `{name}`: value is not valid base64"))?;
    String::from_utf8(bytes)
        // A non-UTF-8 constant value would need the byte-string spelling ADR-0080
        // gives PHP strings; no mined constant has one, and inventing an escape
        // for a case that does not occur is a guess. Refuse loudly instead.
        .map_err(|_| format!("constant `{name}`: value is not valid UTF-8 — needs a byte spelling"))
}

/// The directories of the mining machine, for [`leak_tripwire`] — asked of the
/// engine whose values the table carries, since those are the only paths a row
/// can be carrying.
fn machine_dirs(bin: &str) -> Vec<String> {
    let mut out = Vec::new();
    for name in ["PHP_PREFIX", "PHP_BINDIR", "PHP_LIBDIR", "PHP_EXTENSION_DIR"] {
        if let Ok(o) = Command::new(bin).arg("-r").arg(format!("echo {name};")).output()
            && o.status.success()
            && let Ok(s) = String::from_utf8(o.stdout)
            && s.len() > 1
        {
            out.push(s);
        }
    }
    if let Ok(home) = std::env::var("HOME")
        && home.len() > 1
    {
        out.push(home);
    }
    out
}

/// Fail the run if an admitted string value carries one of this machine's own
/// directories. The [`BUILD_DEPENDENT`] roster is what *should* catch these; this
/// is what makes a missing roster entry a failed mining run rather than an
/// absolute path in a committed file.
fn leak_tripwire(name: &str, value: &str, dirs: &[String]) -> Result<(), String> {
    for d in dirs {
        if value.contains(d.as_str()) {
            return Err(format!(
                "constant `{name}` carries a directory of the mining machine — \
                 add it to BUILD_DEPENDENT in xtask/src/mine_constants.rs"
            ));
        }
    }
    Ok(())
}

/// PHP's own normalization for a constant's index key (mirrors
/// `steins_syntax::normalize_const_fqn`): leading `\` stripped, namespace
/// segments lowercased, the final segment left exactly as written. The catalog
/// crate has no dependency on the syntax crate, so the rule is applied here, at
/// generation time, and the lookup applies the same one.
fn normalize_const_fqn(name: &str) -> String {
    let name = name.trim_start_matches('\\');
    match name.rfind('\\') {
        Some(pos) => format!("{}{}", name[..=pos].to_ascii_lowercase(), &name[pos + 1..]),
        None => name.to_owned(),
    }
}

/// The presence scan's result: for each mined minor, the php-src branch tip it
/// read and the set of constant names it could see declared there.
struct Presence {
    tips: Vec<((u16, u16), String)>,
    names: Vec<((u16, u16), BTreeSet<String>)>,
    /// Extensions the scan covers well enough for an absence to be evidence.
    covered: BTreeSet<String>,
}

/// The share of an extension's constants the scan must see at a minor before
/// "absent there" is read as evidence rather than as a blind spot.
///
/// The number is calibrated, not chosen: at 8.1 curl still registered its ~700
/// constants through a wrapper macro that pastes the name token
/// (`REGISTER_CURL_CONSTANT(__c)` → `REGISTER_LONG_CONSTANT(#__c, …)`), so the
/// literal never appears anywhere a grep can reach and the scan sees almost none
/// of them. Reading that as "curl gained 700 constants in 8.2" would gate the
/// whole extension off for every project targeting 8.1 — a systematic wrong
/// answer produced by a systematic blind spot. Coverage measured per extension
/// per minor is exactly the signal that tells the two apart, and 0.90 leaves
/// room for the handful of constants an extension genuinely gains in a minor.
const COVERAGE_FLOOR: f64 = 0.90;

/// **When the scan says a name arrived**, or `None` for no gate.
///
/// Four outcomes, and the three that decline are the point:
///
/// * the name is absent from the TOP mined minor — the engine has it, so the
///   scan is blind to this name (macro token-pasting) and every verdict it could
///   give is worthless. No gate.
/// * the scan does not cover the name's EXTENSION well enough at every mined
///   minor ([`COVERAGE_FLOOR`]) — "absent" there means nothing. No gate.
/// * the name is present at every mined minor — it did not arrive inside the
///   mined window, so there is nothing to gate on. No gate.
/// * otherwise the name is present from some minor upward and absent below it:
///   that minor is `since`. A non-monotone pattern (present, absent, present)
///   means the scan is unreliable for this name too, and declines.
fn arrival(p: &Presence, name: &str, ext: &str) -> Option<(u16, u16)> {
    let seen: Vec<bool> = p.names.iter().map(|(_, set)| set.contains(name)).collect();
    if !*seen.last()? {
        return None;
    }
    if !p.covered.contains(ext) {
        return None;
    }
    let first = seen.iter().position(|s| *s)?;
    if !seen[first..].iter().all(|s| *s) {
        return None;
    }
    (first > 0).then(|| p.names[first].0)
}

/// The extensions whose constants the scan sees well enough at EVERY mined minor
/// for an absence to mean something ([`COVERAGE_FLOOR`]).
fn covered_extensions(
    names: &[((u16, u16), BTreeSet<String>)],
    by_ext: &BTreeMap<String, Vec<String>>,
) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (ext, members) in by_ext {
        if members.is_empty() {
            continue;
        }
        let worst = names
            .iter()
            .map(|(_, set)| {
                let hits = members.iter().filter(|n| set.contains(n.as_str())).count();
                hits as f64 / members.len() as f64
            })
            .fold(f64::INFINITY, f64::min);
        if worst >= COVERAGE_FLOOR {
            out.insert(ext.clone());
        } else {
            println!(
                "mine-constants: extension `{ext}` — the range scan sees only {:.0}% of its \
                 {} constants at the worst mined minor; every row of it stays rangeless",
                worst * 100.0,
                members.len()
            );
        }
    }
    out
}

/// Read each mined minor's php-src branch for the two places a global constant
/// is declared.
fn scan_presence(
    dir: &str,
    by_ext: &BTreeMap<String, Vec<String>>,
) -> Result<Presence, String> {
    let mut tips = Vec::new();
    let mut names = Vec::new();
    for &minor in MINED_MINORS {
        let branch = format!("origin/PHP-{}.{}", minor.0, minor.1);
        let tip = git(dir, &["rev-parse", &branch])?.trim().to_owned();
        let mut set = BTreeSet::new();
        stub_constants(dir, &branch, &mut set)?;
        c_constants(dir, &branch, &mut set)?;
        println!(
            "mine-constants: php-src {branch} ({}) declares {} constants the scan can see",
            &tip[..tip.len().min(10)],
            set.len()
        );
        tips.push((minor, tip));
        names.push((minor, set));
    }
    let covered = covered_extensions(&names, by_ext);
    Ok(Presence { tips, names, covered })
}

/// Global `const NAME = …;` in a `.stub.php`, resolved against the file's own
/// `namespace` line — `ext/dom/dom.stub.php` declares `Dom\HTML_NO_DEFAULT_NS`
/// that way, and reading the bare segment would file it under the wrong name.
/// Column 0 is what makes this the GLOBAL form: a class constant in a stub is
/// indented inside its class block.
fn stub_constants(dir: &str, branch: &str, out: &mut BTreeSet<String>) -> Result<(), String> {
    let text = git(
        dir,
        &["grep", "-n", "-E", "^(namespace [A-Za-z_0-9\\\\]+ *;|const [A-Za-z_0-9]+ *=)", branch, "--", "*.stub.php"],
    )?;
    let mut ns_of: BTreeMap<String, String> = BTreeMap::new();
    // `git grep` output is `<branch>:<path>:<line>:<content>`, in path order, so
    // a file's `namespace` line always precedes its constants.
    for line in text.lines() {
        let Some((path, content)) = grep_fields(line) else { continue };
        if let Some(rest) = content.strip_prefix("namespace ") {
            let ns = rest.trim().trim_end_matches(';').trim();
            ns_of.insert(path.to_owned(), format!("{ns}\\"));
        } else if let Some(rest) = content.strip_prefix("const ") {
            let name = rest.split(['=', ' ']).next().unwrap_or("").trim();
            if !name.is_empty() {
                let ns = ns_of.get(path).map_or("", String::as_str);
                out.insert(normalize_const_fqn(&format!("{ns}{name}")));
            }
        }
    }
    Ok(())
}

/// `REGISTER_*_CONSTANT("NAME", …)` and `zend_register_*_constant("NAME", …)` in
/// C — the pre-stub registration form, still how `ext/json` declares its flags on
/// the older branches.
fn c_constants(dir: &str, branch: &str, out: &mut BTreeSet<String>) -> Result<(), String> {
    let text = git(
        dir,
        &[
            "grep",
            "-h",
            "-o",
            "-E",
            "(REGISTER_[A-Z_]*CONSTANT[A-Z_]*|zend_register_[a-z_]*constant[a-z_]*) *\\( *\"[A-Za-z_0-9\\\\]+\"",
            branch,
            "--",
            "*.c",
            "*.h",
        ],
    )?;
    for line in text.lines() {
        if let Some(open) = line.find('"') {
            let rest = &line[open + 1..];
            if let Some(close) = rest.find('"') {
                out.insert(normalize_const_fqn(&rest[..close]));
            }
        }
    }
    Ok(())
}

/// Split a `git grep -n` line into `(path, content)`, dropping the branch and the
/// line number. Paths never contain a colon in php-src, and the content may.
fn grep_fields(line: &str) -> Option<(&str, &str)> {
    let (_branch, rest) = line.split_once(':')?;
    let (path, rest) = rest.split_once(':')?;
    let (_lineno, content) = rest.split_once(':')?;
    Some((path, content.trim()))
}

/// Run `git` inside the php-src checkout. A non-zero exit is the caller's error:
/// a missing branch means the checkout has not fetched what the scan needs, and
/// silently mining a partial range would be worse than refusing.
fn git(dir: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("run git -C {dir} {}: {e}", args.join(" ")))?;
    // `git grep` exits 1 for "no match", which is data, not failure.
    if !out.status.success() && out.stdout.is_empty() && args.first() != Some(&"grep") {
        return Err(format!(
            "git -C {dir} {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Decode standard base64 (the miner's string-value encoding). Small and local:
/// the xtask has no base64 dependency and one decoder is less than one.
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    let mut out = Vec::new();
    for b in s.bytes() {
        if b == b'=' {
            break;
        }
        let v = ALPHABET.iter().position(|c| *c == b)? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((acc >> bits) & 0xff).ok()?);
        }
    }
    Some(out)
}

/// Render the TOML source of record.
fn render(
    top: &Engine,
    others: &[Engine],
    presence: Option<&Presence>,
    rows: &BTreeMap<String, Row>,
    refused: &BTreeMap<String, Refusal>,
    moves: &BTreeMap<String, Vec<(String, String)>>,
) -> String {
    let mined = &top.mined;
    let mut s = String::new();
    s.push_str(
        "# ENGINE CONSTANTS — the generated, committed table ADR-0094 §2 rules in place\n\
         # of a sidecar `constant()` call (issue #598).\n\
         #\n\
         # SOURCE OF RECORD. Generated by `cargo xtask mine-constants`; never by hand.\n\
         # `cargo xtask gen-catalog` turns it into the shipped Rust table. Regenerate\n\
         # alongside a `PINNED_PHP` bump, the way `param_facts.toml` is.\n\
         #\n\
         # TWO SOURCES. Values and extensions come from the TOP engine's\n\
         # `get_defined_constants(true)` (`mine_constants.php`). Version ranges cannot:\n\
         # `since` asks whether a name existed at 8.1, and an 8.5 engine has no opinion,\n\
         # so they come from php-src's per-minor branches instead — a `.stub.php` global\n\
         # `const` or a `REGISTER_*_CONSTANT(\"NAME\", …)` in C. That scan is blind to a\n\
         # registration built by macro token-pasting, so a name it cannot see at the TOP\n\
         # mined minor gets NO range rather than a guessed one.\n\
         #\n\
         # WHAT IS REFUSED (ADR-0094 §3). A mined row is a spec-fixed literal and\n\
         # nothing else. `platform` names are answered by class — the host sets, the\n\
         # 64-bit width, the `PhpTarget`-derived version — and the resolver owns them.\n\
         # `build` names encode the BUILD (a linked library's version, a compile flag,\n\
         # an installation path) and have no target-independent value at all — as do the\n\
         # extensions refused whole, whose numbers come from the C library (`sockets`,\n\
         # `pcntl`, `posix`) or the parser generator (`tokenizer`), so the NAME is stable\n\
         # while the value is not, and the FAMILIES refused the same way for the same\n\
         # reason (`GLOB_*` from `glob.h`, `intl`'s `U_*` from ICU's error enum, the\n\
         # `PGSQL_*` groups from libpq's). `unrepresentable` is `STDIN` and its two\n\
         # siblings (resources) plus `INF`/`NAN`.\n\
         #\n\
         # AND WHAT IS REFUSED BY MEASUREMENT. One engine's value is a claim about every\n\
         # minor the target may span, so the run mines EVERY engine it is given\n\
         # (`--php PATH`, repeatable) and refuses any name they disagree about inside the\n\
         # range its row would claim — `[refused.value_moves]` records the name with what\n\
         # each engine said. This is the check a roster cannot be: a roster is a list\n\
         # somebody maintains, and a missing entry is a WRONG row until something\n\
         # measures it. An engine that lacks the name, or that sits below the row's\n\
         # `since`, takes no part — absence is the version gate's question, not this one.\n\
         #\n\
         # `until` is a schema slot the current mining never fills, and that is a\n\
         # property of the sources rather than of PHP: every mined name exists in the\n\
         # engine that supplied the values, so nothing mined has left yet. A constant\n\
         # that left before that engine (`E_STRICT`, gone in 8.5) has no value to mine\n\
         # and therefore no row — it answers nothing at every target, which is the\n\
         # honest verdict and not a gate.\n\n",
    );
    let _ = writeln!(s, "[meta]");
    let _ = writeln!(s, "php = \"{}\"", mined.php);
    let _ = writeln!(s, "miner = \"docs/research/phpsrc-mining/mine_constants.php\"");
    let _ = writeln!(s, "generator = \"cargo xtask mine-constants\"");
    let _ = writeln!(s, "extensions = [");
    for e in &mined.extensions {
        let _ = writeln!(s, "  \"{e}\",");
    }
    let _ = writeln!(s, "]");
    // The engines' VERSIONS and not their paths: a nix store path or a Homebrew
    // cellar is exactly what `leak_tripwire` exists to keep out of this file, and
    // the version is the whole of what the diff's reader needs.
    let _ = writeln!(s, "# Engines the value diff compared, low minor first; `php` above is the");
    let _ = writeln!(s, "# top one, whose values the rows carry. One entry = nothing was diffed.");
    let _ = writeln!(s, "diffed = [");
    for e in others.iter().chain(std::iter::once(top)) {
        let _ = writeln!(s, "  {},", toml_str(&e.mined.php));
    }
    let _ = writeln!(s, "]");
    match presence {
        Some(p) => {
            let _ = writeln!(s, "# php-src branch tips the presence scan read, low minor first.");
            let _ = writeln!(s, "minors = [");
            for ((maj, min), tip) in &p.tips {
                let _ = writeln!(s, "  [\"{maj}.{min}\", \"{tip}\"],");
            }
            let _ = writeln!(s, "]");
        }
        None => {
            let _ = writeln!(s, "# No php-src checkout was given: every row is rangeless.");
            let _ = writeln!(s, "minors = []");
        }
    }
    s.push('\n');

    let mut by_refusal: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (name, r) in refused {
        let key = match r {
            Refusal::Platform => "platform",
            Refusal::Build => "build",
            Refusal::Unrepresentable => "unrepresentable",
            Refusal::ValueMoves => "value_moves",
        };
        by_refusal.entry(key).or_default().push(name);
    }
    let gated = rows.values().filter(|r| r.since.is_some()).count();
    let _ = writeln!(s, "[counts]");
    let _ = writeln!(s, "# mined      what `get_defined_constants(true)` had, over the catalog's extensions");
    let _ = writeln!(s, "# rows       spec-fixed literals admitted to the table");
    let _ = writeln!(s, "# gated      of those, carrying a `since` above the mined floor");
    let _ = writeln!(s, "# refused_*  the three ADR-0094 §3 classes a mined row cannot carry, plus");
    let _ = writeln!(s, "#            the names the engines themselves disagreed about");
    let _ = writeln!(s, "mined = {}", mined.constants_total);
    let _ = writeln!(s, "rows = {}", rows.len());
    let _ = writeln!(s, "gated = {gated}");
    for (key, names) in &by_refusal {
        let _ = writeln!(s, "refused_{key} = {}", names.len());
    }
    s.push('\n');
    // The refusals are recorded, not merely counted: "this name was looked at and
    // deliberately left out" and "nobody ever mined it" are different facts, and
    // only the first can be reviewed.
    let _ = writeln!(s, "[refused]");
    for (key, names) in by_refusal.iter().filter(|(k, _)| **k != "value_moves") {
        let _ = writeln!(s, "{key} = [");
        for n in names {
            let _ = writeln!(s, "  {},", toml_key(n));
        }
        let _ = writeln!(s, "]");
    }
    s.push('\n');
    // A table and not a list, because the VALUES are the evidence: a reviewer who
    // wants to know whether a refusal was right reads what each engine said, and
    // a bare name would make them re-run the diff to find out.
    let _ = writeln!(s, "[refused.value_moves]");
    let _ = writeln!(s, "# name = [[\"<php version>\", \"<value>\"], …] — every engine that HAS the");
    let _ = writeln!(s, "# name at or above the row's `since`, low minor first. Two spellings here");
    let _ = writeln!(s, "# are what refused the row.");
    for (name, seen) in moves {
        let _ = write!(s, "{} = [", toml_key(name));
        for (i, (version, value)) in seen.iter().enumerate() {
            let sep = if i == 0 { "" } else { ", " };
            let _ = write!(s, "{sep}[{}, {}]", toml_str(version), toml_str(value));
        }
        let _ = writeln!(s, "]");
    }
    s.push('\n');

    let _ = writeln!(s, "[const]");
    let _ = writeln!(s, "# name = {{ ext, t, v, since? }} — `v` is the value's SPELLING: a decimal");
    let _ = writeln!(s, "# integer, PHP's own `var_export` float, `true`/`false`/`null`, or the");
    let _ = writeln!(s, "# string itself. Namespace segments are lowercased, the final segment is");
    let _ = writeln!(s, "# not (PHP's own rule for a constant's identity).");
    for (name, r) in rows {
        let _ = write!(
            s,
            "{} = {{ ext = {}, t = {}, v = {}",
            toml_key(name),
            toml_str(&r.ext),
            toml_str(&r.ty),
            toml_str(&r.value)
        );
        if let Some((maj, min)) = r.since {
            let _ = write!(s, ", since = \"{maj}.{min}\"");
        }
        let _ = writeln!(s, " }}");
    }
    s
}

/// A TOML-safe quoted key: a constant name can carry namespace separators.
fn toml_key(name: &str) -> String {
    toml_str(name)
}

/// A TOML basic string. Not Rust's `{:?}`: the two escape vocabularies overlap
/// but do not agree — Rust writes `\'` and `\u{7f}`, and TOML understands
/// neither — and a `DATE_*` format constant is exactly the kind of value that
/// carries a backslash into the difference.
fn toml_str(v: &str) -> String {
    let mut out = String::with_capacity(v.len() + 2);
    out.push('"');
    for c in v.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\u{:04X}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One engine's answer, spelled the way the miner's JSON does.
    fn engine(version: &str, rows: &[(&str, &str, &str)]) -> Engine {
        let mined = Mined {
            php: version.to_owned(),
            extensions: Vec::new(),
            constants_total: rows.len(),
            rows: rows
                .iter()
                .map(|(name, ext, value)| {
                    (
                        (*name).to_owned(),
                        MinedRow {
                            ext: (*ext).to_owned(),
                            ty: "int".to_owned(),
                            value: (*value).to_owned(),
                        },
                    )
                })
                .collect(),
        };
        let minor = php_minor(version).expect("test version");
        Engine { bin: "php".to_owned(), minor, mined }
    }

    fn row(ext: &str, value: &str, since: Option<(u16, u16)>) -> Row {
        Row { ext: ext.to_owned(), ty: "int".to_owned(), value: value.to_owned(), since }
    }

    #[test]
    fn engines_that_disagree_inside_a_rows_range_refuse_it() {
        // The four names this check first caught are all of this shape: the top
        // engine says one number and a supported minor says another, so the row
        // the top engine would write is false at a minor the target may span.
        let top = engine("8.5.10", &[("IMAGETYPE_COUNT", "gd", "22")]);
        let others = vec![engine("8.4.25", &[("IMAGETYPE_COUNT", "gd", "20")])];
        let rows = BTreeMap::from([("IMAGETYPE_COUNT".to_owned(), row("gd", "22", None))]);
        let moves = value_moves(&top, &others, &rows).expect("diff");
        assert_eq!(
            moves.get("IMAGETYPE_COUNT").map(Vec::as_slice),
            Some(
                [("8.4.25".to_owned(), "20".to_owned()), ("8.5.10".to_owned(), "22".to_owned())]
                    .as_slice()
            )
        );
    }

    #[test]
    fn engines_that_agree_leave_the_row_alone() {
        let top = engine("8.5.10", &[("JSON_THROW_ON_ERROR", "json", "4194304")]);
        let others = vec![engine("8.4.25", &[("JSON_THROW_ON_ERROR", "json", "4194304")])];
        let rows =
            BTreeMap::from([("JSON_THROW_ON_ERROR".to_owned(), row("json", "4194304", None))]);
        assert!(value_moves(&top, &others, &rows).expect("diff").is_empty());
    }

    #[test]
    fn a_disagreement_below_the_rows_since_is_the_range_scans_business() {
        // The one thing `since` is allowed to excuse: the row does not speak for
        // 8.2 at all, so what 8.2 calls the name is not a counter-example to it.
        let top = engine("8.5.10", &[("LATE_ARRIVAL", "standard", "7")]);
        let others = vec![engine("8.2.33", &[("LATE_ARRIVAL", "standard", "3")])];
        let rows =
            BTreeMap::from([("LATE_ARRIVAL".to_owned(), row("standard", "7", Some((8, 3))))]);
        assert!(value_moves(&top, &others, &rows).expect("diff").is_empty());
    }

    #[test]
    fn an_engine_that_lacks_the_name_says_nothing() {
        // A build without the extension is an absence, which is the version
        // gate's question — never a disagreement about a value.
        let top = engine("8.5.10", &[("PGSQL_ASSOC", "pgsql", "1")]);
        let others = vec![engine("8.2.33", &[])];
        let rows = BTreeMap::from([("PGSQL_ASSOC".to_owned(), row("pgsql", "1", None))]);
        assert!(value_moves(&top, &others, &rows).expect("diff").is_empty());
    }

    #[test]
    fn a_family_a_c_library_numbers_is_refused_whole() {
        // Refused by family and not by name, so the next member the library
        // gains is refused with the ones that are here today.
        for (name, ext) in [
            ("GLOB_BRACE", "standard"),
            ("GLOB_SOMETHING_GLIBC_ADDS_NEXT", "standard"),
            ("U_ZERO_ERROR", "intl"),
            ("U_FMT_PARSE_ERROR_LIMIT", "intl"),
            ("PGSQL_ERRORS_SQLSTATE", "pgsql"),
            ("PGSQL_DIAG_SEVERITY", "pgsql"),
            ("PGSQL_COMMAND_OK", "pgsql"),
        ] {
            assert!(
                matches!(refuse(name, "int", ext), Some(Refusal::Build)),
                "`{name}` must be refused as build-dependent"
            );
        }
        // The prefixes stay as narrow as the claim they make: an extension's own
        // numbers keep their rows, and `U_` is ICU's only inside `intl`.
        for (name, ext) in
            [("GLOB_BRACE", "json"), ("PGSQL_ASSOC", "pgsql"), ("U_ZERO_ERROR", "standard")]
        {
            assert!(refuse(name, "int", ext).is_none(), "`{name}` of `{ext}` must stay admitted");
        }
    }
}
