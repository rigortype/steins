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
//!   registered each name. One build answers, and `[meta]` records which.
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
//! cargo xtask mine-constants [PHP_SRC_DIR]
//! ```
//!
//! `PHP_SRC_DIR` (or `$STEINS_PHP_SRC`) is a php-src checkout with the
//! `origin/PHP-8.x` branches fetched; without one the run mines values only and
//! every row is rangeless. Output:
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
    // A value that is the OR of a set php-src has changed inside the mined
    // window: `E_ALL` lost `E_STRICT`'s bit when `E_STRICT` left. `since`/`until`
    // cannot express this — they say when a NAME exists, and this name exists
    // throughout with two different values — so the honest answer is no answer.
    "E_ALL",
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

/// Entry point for `cargo xtask mine-constants`.
pub fn run(php_src: Option<&str>) -> Result<(), String> {
    let extensions = catalog_extensions()?;
    let mined = run_miner(&extensions)?;
    println!(
        "mine-constants: PHP {} — {} constants over {} extensions",
        mined.php,
        mined.constants_total,
        mined.extensions.len()
    );

    let php_src = php_src
        .map(str::to_owned)
        .or_else(|| std::env::var("STEINS_PHP_SRC").ok())
        .filter(|d| !d.is_empty());
    // The scan's own reliability is measured per extension, so it needs the
    // extension each mined name belongs to before it can judge an absence.
    let mut by_ext: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, m) in &mined.rows {
        if refuse(name, &m.ty, &m.ext).is_none() {
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

    let dirs = machine_dirs();
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

    let out = render(&mined, presence.as_ref(), &rows, &refused);
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

/// Run the PHP miner with the catalog's extension allowlist.
fn run_miner(extensions: &[String]) -> Result<Mined, String> {
    let script = repo_root().join("docs/research/phpsrc-mining/mine_constants.php");
    let allow = serde_json::to_string(extensions).map_err(|e| format!("encode allowlist: {e}"))?;
    let out = Command::new("php")
        .arg(&script)
        .arg(&allow)
        .output()
        .map_err(|e| format!("run php {}: {e}", script.display()))?;
    if !out.status.success() {
        return Err(format!("miner failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("parse miner JSON: {e}"))
}

/// Whether a mined name is refused, and why.
fn refuse(name: &str, ty: &str, ext: &str) -> Option<Refusal> {
    if PLATFORM_RULED.contains(&name) {
        return Some(Refusal::Platform);
    }
    if BUILD_DEPENDENT.contains(&name) || REFUSED_EXTENSIONS.contains(&ext) {
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

/// The directories of the mining machine, for [`leak_tripwire`].
fn machine_dirs() -> Vec<String> {
    let mut out = Vec::new();
    for name in ["PHP_PREFIX", "PHP_BINDIR", "PHP_LIBDIR", "PHP_EXTENSION_DIR"] {
        if let Ok(o) = Command::new("php").arg("-r").arg(format!("echo {name};")).output()
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
    mined: &Mined,
    presence: Option<&Presence>,
    rows: &BTreeMap<String, Row>,
    refused: &BTreeMap<String, Refusal>,
) -> String {
    let mut s = String::new();
    s.push_str(
        "# ENGINE CONSTANTS — the generated, committed table ADR-0094 §2 rules in place\n\
         # of a sidecar `constant()` call (issue #598).\n\
         #\n\
         # SOURCE OF RECORD. Generated by `cargo xtask mine-constants`; never by hand.\n\
         # `cargo xtask gen-catalog` turns it into the shipped Rust table. Regenerate\n\
         # alongside a `PINNED_PHP` bump, the way `param_facts.toml` is.\n\
         #\n\
         # TWO SOURCES. Values and extensions come from ONE engine's\n\
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
         # while the value is not. `unrepresentable` is `STDIN` and its two siblings\n\
         # (resources) plus `INF`/`NAN`.\n\
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
        };
        by_refusal.entry(key).or_default().push(name);
    }
    let gated = rows.values().filter(|r| r.since.is_some()).count();
    let _ = writeln!(s, "[counts]");
    let _ = writeln!(s, "# mined      what `get_defined_constants(true)` had, over the catalog's extensions");
    let _ = writeln!(s, "# rows       spec-fixed literals admitted to the table");
    let _ = writeln!(s, "# gated      of those, carrying a `since` above the mined floor");
    let _ = writeln!(s, "# refused_*  the three ADR-0094 §3 classes a mined row cannot carry");
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
    for (key, names) in &by_refusal {
        let _ = writeln!(s, "{key} = [");
        for n in names {
            let _ = writeln!(s, "  {},", toml_key(n));
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
