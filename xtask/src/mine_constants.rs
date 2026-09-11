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
//! # One source, asked once per minor
//!
//! Every answer this miner writes comes from the **engines the run was given**,
//! one per PHP minor (`--php PATH`, repeatable). ADR-0094 §2 says the generator
//! "runs over the PHP minors the corpus harness already scopes", and each engine
//! answers two different questions about itself:
//!
//! * **What a name is worth.** The TOP build supplies the values and the
//!   extension set, and `[meta]` records which — but it is not the only build
//!   asked. A value mined from one engine is a claim about every minor the
//!   target may span, so any name the engines DISAGREE about is refused outright
//!   ([`value_moves`]), because a row is a spec-fixed literal or it is nothing.
//! * **Which minors have the name at all.** `since` and `until` are read off the
//!   same engines' presence ([`range`]): a name absent at 8.2 and present from
//!   8.3 up arrived at 8.3, and one present through 8.4 and gone at 8.5 left
//!   after 8.4. Issue #718 retired the php-src branch scan this used to be —
//!   that scan read a `.stub.php` `const` or a `REGISTER_*_CONSTANT("NAME", …)`
//!   in C, was blind to every registration built by macro token-pasting, and
//!   needed a per-extension coverage floor to keep its blind spots from minting
//!   wrong gates. An engine either has the name or it does not.
//!
//! The engines' builds differ, and that is the one way presence can lie: the nix
//! 8.4 build has no `brotli`, so every `BROTLI_*` would read as "arrived in 8.5".
//! [`range`] therefore judges a name only against the engines that **loaded its
//! extension** — the same clause the value diff uses for the same reason, since
//! a build without the extension is not a minor without the constant.
//!
//! # A boundary needs two comparable builds
//!
//! Presence answers "which minors have the name" only if the engines at a
//! boundary differ in their MINOR and in nothing else that decides whether the
//! name is registered. They do not: the 8.2/8.3/8.4 engines here are nix
//! `php-with-extensions` and the 8.5 one is Homebrew's, and php-src guards whole
//! blocks of registrations on the linked library's version and on configure-time
//! features. `LIBXML_NO_XXE` sits behind `LIBXML_VERSION >= 21300`, and the two
//! packagers link libxml2 2.15.3 and 2.9.13 — so "arrived at 8.4, left at 8.5"
//! was a fact about libxml2, not about PHP. `MHASH_*` (39 names) needs
//! `--with-mhash`, `LDAP_OPT_X_SASL_*` needs `HAVE_LDAP_SASL`, and
//! `IMAGETYPE_SWC` needs a zlib compiled into the binary rather than loaded as a
//! shared module.
//!
//! So each engine also reports a **build fingerprint** (`mine_constants.php`'s
//! `build` map, plus the linkage probe [`static_extensions`] runs),
//! [`BUILD_PARITY`] says which of those facts an extension's registrations are
//! guarded on, and [`range`] mints a boundary only when the two engines AT that
//! boundary agree on every one of them. A declined boundary is recorded by name
//! with the fact that differed, the way a refused value is. Two further rules:
//!
//! * an `until` additionally needs the same **packager** on both sides. A name
//!   gone at the next minor and a next minor somebody else built are
//!   indistinguishable, and only one of them is a fact about PHP.
//! * an extension [`BUILD_PARITY`] says nothing about is compared on nothing,
//!   which is the residual risk a roster always carries — the same one
//!   [`BUILD_DEPENDENT`] carries. The packager rule is what backs the roster up
//!   on the departure boundary, which is the one a row goes on to ASSERT.
//!
//! # A row with no value
//!
//! A name present in the older engines and gone from the top one has no value to
//! mine and still has something to say, so it gets a **value-less row** carrying
//! `until` and nothing else (owner ruling 2026-09-11). The value resolver ignores
//! it — there is no literal to answer with — and no other lane reads it: the
//! absence family's own version-skew gate (issue #28) already declines every
//! claim made from a runtime outside the project's `PhpTarget`, so a runtime past
//! the departure reports the name absent on its own. The `until` is what the
//! table RECORDS, for a reader and for the next tool; it is not a second absence
//! oracle. The table still never says a constant IS defined.
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
//! cargo xtask mine-constants [--php PATH]…
//! ```
//!
//! `--php PATH` (repeatable, or `$STEINS_MINE_PHP` as a `:`-separated list) names
//! the engines to mine, one per minor; with none given the run asks the `php` on
//! PATH alone, and then **nothing is diffed and no row carries a range** — a
//! single engine agrees with itself and knows only its own minor. Two engines of
//! the SAME minor are refused: presence is read per minor, and two answers for
//! one minor is not a table. Output:
//! `docs/research/phpsrc-mining/constants.toml` (source of record).
//! `cargo xtask gen-catalog` turns it into the shipped Rust table.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::process::Command;

use crate::corpus::repo_root;

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

/// **What an extension's registrations are guarded on**: `(extension, the build
/// facts a minor boundary must agree about)`.
///
/// php-src does not register a constant unconditionally. Whole blocks sit behind
/// the linked library's version (`#if LIBXML_VERSION >= 21300`, `#if
/// LIBCURL_VERSION_NUM >= …`) or behind a configure-time feature
/// (`#ifdef PHP_MHASH_BC`, `#ifdef HAVE_LDAP_SASL`, `#ifdef HAVE_ZLIB`), so two
/// engines that differ THERE disagree about which names exist for a reason that
/// is not their PHP minor. [`range`] refuses to mint a boundary between two such
/// engines.
///
/// The facts are named the way `mine_constants.php`'s `build` map and
/// [`static_extensions`] spell them. An extension absent from this table is
/// compared on nothing, which is the roster's residual risk: the next library
/// guard php-src adds needs a line here, exactly as the next `PGRES_*` needs one
/// in [`BUILD_DEPENDENT`]. The packager rule in [`range`] is what backs it up on
/// the departure boundary, the one a value-less row goes on to assert.
const BUILD_PARITY: &[(&str, &[&str])] = &[
    // libxml2's version, read by every extension that links it. `LIBXML_NO_XXE`
    // is the row this table was written for: it is registered only above libxml2
    // 2.13, and the nix builds link 2.15.3 against Homebrew's 2.9.13.
    ("libxml", &["libxml"]),
    ("dom", &["libxml"]),
    ("SimpleXML", &["libxml"]),
    ("xml", &["libxml"]),
    ("xmlreader", &["libxml"]),
    ("xmlwriter", &["libxml"]),
    ("soap", &["libxml"]),
    ("xsl", &["libxml", "libxslt"]),
    // `MHASH_*` is the `--with-mhash` BC layer, not a hash algorithm roster:
    // 39 names that exist or do not exist by configure flag.
    ("hash", &["mhash_bc"]),
    // `LDAP_OPT_X_SASL_*` is `#ifdef HAVE_LDAP_SASL`.
    ("ldap", &["ldap_sasl"]),
    ("curl", &["curl"]),
    ("openssl", &["openssl"]),
    ("intl", &["icu"]),
    ("mbstring", &["oniguruma"]),
    ("pcre", &["pcre"]),
    ("gd", &["gd", "zlib", "zlib_linkage"]),
    ("sodium", &["sodium"]),
    ("gmp", &["gmp"]),
    ("iconv", &["iconv"]),
    ("pgsql", &["libpq"]),
    ("pdo_pgsql", &["libpq"]),
    ("zlib", &["zlib", "zlib_linkage"]),
    ("zip", &["libzip", "zlib", "zlib_linkage"]),
    ("readline", &["readline"]),
    ("sqlite3", &["sqlite"]),
    ("pdo_sqlite", &["sqlite"]),
    // `ext/standard` is PHP's own, with one library guard inside it:
    // `IMAGETYPE_SWC` is `#ifdef HAVE_ZLIB`, which follows the zlib the binary
    // was CONFIGURED with — the nix builds load zlib as a shared module and
    // Homebrew's compiles it in, and `extension_loaded('zlib')` is true either
    // way, so the linkage is the fact and the mere presence is not.
    ("standard", &["zlib_linkage"]),
];

/// Extensions whose STATIC linkage is a build fact some other extension's
/// registrations are guarded on, probed by running the engine with `-n` — no
/// `php.ini`, so only what is compiled into the binary answers.
const LINKAGE_PROBES: &[&str] = &["zlib"];

/// The miner's JSON shape.
#[derive(serde::Deserialize)]
struct Mined {
    php: String,
    /// The build fingerprint: `fact => spelling`, as `mine_constants.php` reads
    /// it off the linked libraries and the configure-time features php-src guards
    /// registrations on. [`BUILD_PARITY`] says which facts matter per extension.
    #[serde(default)]
    build: BTreeMap<String, String>,
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

/// A row's minor range: `(since, until)`, each absent when the engines proved no
/// boundary there. Named because the pair is what [`range`] answers with and a
/// bare tuple of two optional tuples reads as noise at the call site.
type MinorRange = (Option<(u16, u16)>, Option<(u16, u16)>);

/// **A boundary the presence run found and the build fingerprints took back**:
/// which boundary, the two engines that form it, and the fact they spell
/// differently ([`BUILD_PARITY`], or `packager` for the `until` rule).
///
/// Recorded and not merely counted, for the reason [`Refusal`] is: a reviewer who
/// wants to know whether `MHASH_ADLER32` really arrived in 8.5 reads that the two
/// builds disagree about `mhash_bc`, and a smaller `gated` number tells them
/// nothing.
#[derive(PartialEq, Eq, Debug)]
struct Decline {
    boundary: &'static str,
    below: String,
    above: String,
    fact: &'static str,
}

impl Decline {
    fn new(boundary: &'static str, below: &Engine, above: &Engine, fact: &'static str) -> Self {
        Decline {
            boundary,
            below: below.mined.php.clone(),
            above: above.mined.php.clone(),
            fact,
        }
    }
}

/// One admitted row, as the source of record spells it.
struct Row {
    ext: String,
    /// The value's type and spelling, or `None` for a **value-less row**: the
    /// name is gone from the top engine, so there is no value to mine and the row
    /// carries only its departure. Both halves move together — a type without a
    /// value describes nothing.
    value: Option<(String, String)>,
    /// The lowest engine minor that has the name, when a lower one that loaded
    /// its extension does not. `None` = no lower gate.
    since: Option<(u16, u16)>,
    /// The highest engine minor that has the name, when a higher one that loaded
    /// its extension does not. `None` = the name is still there at the top.
    until: Option<(u16, u16)>,
}

/// One engine the run asked, and what it answered. The TOP engine supplies the
/// table's values and extension set; every engine takes part in [`value_moves`]
/// and in [`range`].
struct Engine {
    bin: String,
    minor: (u16, u16),
    mined: Mined,
    /// The extensions this build loaded. A build WITHOUT an extension has no
    /// opinion about that extension's constants, which is what keeps a packaging
    /// difference from reading as a language change ([`range`]).
    exts: BTreeSet<String>,
    /// This build's names, keyed the way the table keys its rows.
    names: BTreeSet<String>,
    /// The build fingerprint: the engine's own `build` map plus the linkage
    /// probes. Two engines that differ in a fact [`BUILD_PARITY`] charges to an
    /// extension cannot range that extension's names between them.
    build: BTreeMap<String, String>,
}

impl Engine {
    /// Whether this build can be asked about `ext`'s constants at all.
    fn judges(&self, ext: &str) -> bool {
        self.exts.contains(ext)
    }

    /// Which packager built this engine (`nix`, `homebrew`, `system`, …), as
    /// `mine_constants.php` classifies its `PHP_PREFIX`. The label and never the
    /// path: an install root is exactly what [`leak_tripwire`] keeps out of a
    /// committed file.
    fn packager(&self) -> &str {
        self.build.get("packager").map_or("other", String::as_str)
    }

    /// The first fact `other` spells differently among those [`BUILD_PARITY`]
    /// charges to `ext`, or `None` when the two builds are comparable there.
    ///
    /// A fact neither build reports is not a difference; a fact one reports and
    /// the other does not IS one, and the `absent` spelling makes that the same
    /// comparison as any other.
    fn parity_gap(&self, other: &Engine, ext: &str) -> Option<&'static str> {
        BUILD_PARITY
            .iter()
            .find(|(e, _)| *e == ext)
            .into_iter()
            .flat_map(|(_, facts)| facts.iter())
            .find(|fact| self.build.get(**fact) != other.build.get(**fact))
            .copied()
    }
}

/// One mined name, as the union across engines sees it: the spelling the miner
/// reported, the extension and type the NEWEST engine that has it reported, and
/// which engine that was.
struct Candidate {
    raw: String,
    ext: String,
    ty: String,
    newest: usize,
}

/// Entry point for `cargo xtask mine-constants`.
pub fn run(php_bins: &[String]) -> Result<(), String> {
    let extensions = catalog_extensions()?;
    let mut engines = Vec::new();
    for bin in php_binaries(php_bins) {
        let mined = run_miner(&bin, &extensions)?;
        let minor = php_minor(&mined.php)?;
        let build = build_fingerprint(&bin, &mined)?;
        println!(
            "mine-constants: PHP {} — {} constants over {} extensions, built by {}",
            mined.php,
            mined.constants_total,
            mined.extensions.len(),
            build.get("packager").map_or("other", String::as_str),
        );
        let exts = mined.extensions.iter().cloned().collect();
        let names = mined.rows.keys().map(|n| normalize_const_fqn(n)).collect();
        engines.push(Engine { bin, minor, mined, exts, names, build });
    }
    // The TOP minor is the one whose values the table carries, and the order is
    // also the presence table's axis, so it is established before anything reads
    // it. Two engines of one minor would give that axis two answers for one
    // column: refuse rather than let the later one silently win.
    engines.sort_by_key(|e| e.minor);
    if let Some(w) = engines.windows(2).find(|w| w[0].minor == w[1].minor) {
        return Err(format!(
            "two engines report PHP {}.{} ({} and {}) — presence is read one engine per minor",
            w[0].minor.0, w[0].minor.1, w[0].mined.php, w[1].mined.php
        ));
    }
    let top = engines.last().ok_or("no PHP engine to mine")?;

    // The union of every engine's names, not the top engine's alone: a name the
    // top build no longer has is exactly the `until` case, and iterating one
    // engine would never see it. The ext and type come from the NEWEST engine
    // that has the name, which is the one whose value the row would carry.
    let mut candidates: BTreeMap<String, Candidate> = BTreeMap::new();
    for (i, e) in engines.iter().enumerate() {
        for (name, m) in &e.mined.rows {
            candidates.insert(
                normalize_const_fqn(name),
                Candidate { raw: name.clone(), ext: m.ext.clone(), ty: m.ty.clone(), newest: i },
            );
        }
    }

    let dirs = machine_dirs(&top.bin);
    let mut rows: BTreeMap<String, Row> = BTreeMap::new();
    let mut refused: BTreeMap<String, Refusal> = BTreeMap::new();
    // The boundaries the presence run found and the build fingerprints took back
    // ([`BUILD_PARITY`]). Recorded rather than counted: "this name's arrival was
    // a packaging difference" is reviewable and a smaller `gated` number is not.
    let mut declined: BTreeMap<String, Vec<Decline>> = BTreeMap::new();
    for (key, c) in &candidates {
        if let Some(r) = refuse(&c.raw, &c.ty, &c.ext) {
            refused.insert(c.raw.clone(), r);
            continue;
        }
        let ((since, until), gaps) = range(&engines, key, &c.ext);
        if !gaps.is_empty() {
            declined.insert(c.raw.clone(), gaps);
        }
        let value = if c.newest + 1 == engines.len() {
            let m = &engines[c.newest].mined.rows[&c.raw];
            let value = decode_value(&c.raw, &m.ty, &m.value)?;
            if m.ty == "string" {
                leak_tripwire(&format!("constant `{}`", c.raw), &value, &dirs)?;
            }
            Some((m.ty.clone(), value))
        } else if until.is_some() {
            // Present below the top and gone at it, with the engines that loaded
            // the extension agreeing on where: a value-less row.
            None
        } else {
            // Absent from the top build with no proven departure — the top build
            // simply lacks the extension. Nothing is known and nothing is said.
            continue;
        };
        rows.insert(key.clone(), Row { ext: c.ext.clone(), value, since, until });
    }

    // The multi-engine diff, LAST: it judges the rows as they would be written,
    // so a name the rosters already refused is not diffed, and the `since` the
    // presence range found is what decides which engines a disagreement counts at.
    let moves = value_moves(&engines, &rows)?;
    for name in moves.keys() {
        rows.remove(name);
        refused.insert(name.clone(), Refusal::ValueMoves);
    }

    let out = render(&engines, &rows, &refused, &moves, &declined);
    let dst = repo_root().join("docs/research/phpsrc-mining/constants.toml");
    std::fs::write(&dst, &out).map_err(|e| format!("write {}: {e}", dst.display()))?;
    let gated = rows.values().filter(|r| r.since.is_some()).count();
    let value_less = rows.values().filter(|r| r.value.is_none()).count();
    println!(
        "mine-constants: {} rows ({gated} version-gated, {value_less} value-less), {} refused, \
         {} boundaries declined for build parity → {}",
        rows.len(),
        refused.len(),
        declined.len(),
        dst.display()
    );
    Ok(())
}

/// **The minors an engine set proves a name over**: `(since, until)`, with the
/// boundaries the build fingerprints took back.
///
/// Only the engines that LOADED the name's extension take part. A build without
/// `brotli` is silent about `BROTLI_*`, and reading its silence as an absence
/// would mint `since = "8.5"` for a constant that has been there all along —
/// a packaging difference dressed as a language change. This is the same clause
/// [`value_moves`] applies for the same reason, and it is the whole of what
/// replaced the php-src scan's per-extension coverage floor (issue #718).
///
/// Having the extension is necessary and not sufficient, which is what
/// [`BUILD_PARITY`] adds: two builds that load `libxml` and link different
/// libxml2s disagree about `LIBXML_NO_XXE` for libxml2's reasons, so the
/// boundary BETWEEN them states nothing about PHP. Each boundary is therefore
/// checked against the two engines that form it, and an `until` — the boundary a
/// value-less row records — additionally needs the same packager on both sides.
///
/// Three declines beyond that, each for its own reason:
///
/// * no judge has the name — nothing to range over (the caller drops it);
/// * a boundary with no engine on its far side: a lone judge knows only its own
///   minor, and the top judge cannot see a departure it is not there to miss;
/// * the presence is not contiguous (present, absent, present) — the builds
///   differ in some way the extension check did not catch, so no range at all
///   rather than a guessed one.
fn range(engines: &[Engine], key: &str, ext: &str) -> (MinorRange, Vec<Decline>) {
    let judges: Vec<(&Engine, bool)> = engines
        .iter()
        .filter(|e| e.judges(ext))
        .map(|e| (e, e.names.contains(key)))
        .collect();
    let (Some(first), Some(last)) = (
        judges.iter().position(|(_, present)| *present),
        judges.iter().rposition(|(_, present)| *present),
    ) else {
        return ((None, None), Vec::new());
    };
    if !judges[first..=last].iter().all(|(_, present)| *present) {
        return ((None, None), Vec::new());
    }
    let mut declined = Vec::new();
    // The arrival: the engine that first has the name, against the one below it
    // that does not.
    let since = (first > 0)
        .then(|| {
            let (below, at) = (judges[first - 1].0, judges[first].0);
            match below.parity_gap(at, ext) {
                None => Some(at.minor),
                Some(fact) => {
                    declined.push(Decline::new("since", below, at, fact));
                    None
                }
            }
        })
        .flatten();
    // The departure, held to the stricter bar: this is the boundary a value-less
    // row is made of, and a packager change alone is enough to explain it.
    let until = (last + 1 < judges.len())
        .then(|| {
            let (at, above) = (judges[last].0, judges[last + 1].0);
            let gap = at
                .parity_gap(above, ext)
                .or_else(|| (at.packager() != above.packager()).then_some("packager"));
            match gap {
                None => Some(at.minor),
                Some(fact) => {
                    declined.push(Decline::new("until", at, above, fact));
                    None
                }
            }
        })
        .flatten();
    ((since, until), declined)
}

/// **The build fingerprint of one engine**: what `mine_constants.php` read off
/// the linked libraries and the configure-time features, plus the linkage probes
/// only a second run of the binary can answer.
fn build_fingerprint(bin: &str, mined: &Mined) -> Result<BTreeMap<String, String>, String> {
    let mut build = mined.build.clone();
    // The fingerprints are committed too, so they go past the same tripwire an
    // admitted value does — against THIS engine's directories, since these are
    // the only paths this engine's facts could be carrying.
    let dirs = machine_dirs(bin);
    for (fact, value) in &build {
        leak_tripwire(&format!("build fact `{fact}`"), value, &dirs)?;
    }
    let statics = static_extensions(bin)?;
    for ext in LINKAGE_PROBES {
        // Three states and not two: a build that does not have the library at
        // all is a third thing, and collapsing it into `shared` would make two
        // different builds compare equal.
        let linkage = if statics.contains(*ext) {
            "static"
        } else if mined.extensions.iter().any(|e| e == ext) {
            "shared"
        } else {
            "absent"
        };
        build.insert(format!("{ext}_linkage"), linkage.to_owned());
    }
    Ok(build)
}

/// The extensions COMPILED INTO an engine, asked with `-n` so no `php.ini` loads
/// a shared module and answers for one that is built in.
///
/// `ext/standard`'s `HAVE_ZLIB` guard follows the zlib the binary was configured
/// with, which `extension_loaded('zlib')` cannot see: the nix builds load zlib as
/// a shared module and lack `IMAGETYPE_SWC`, Homebrew's compiles it in and has
/// it, and both answer `true` to the runtime question.
fn static_extensions(bin: &str) -> Result<BTreeSet<String>, String> {
    let out = Command::new(bin)
        .args(["-n", "-r", "echo implode(\",\", get_loaded_extensions());"])
        .output()
        .map_err(|e| format!("run {bin} -n: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "linkage probe failed on {bin}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).split(',').map(|e| e.trim().to_owned()).collect())
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
///   the "unless the range explains it" clause, and it is the only thing `since`
///   is allowed to excuse: a disagreement at or above `since` is inside the row's
///   own range and nothing explains it away.
///
/// A value-less row takes no part at all: it claims no value, so nothing can
/// disagree with it.
fn value_moves(
    engines: &[Engine],
    rows: &BTreeMap<String, Row>,
) -> Result<BTreeMap<String, Vec<(String, String)>>, String> {
    // The engines key their rows the way the miner reported them and the table's
    // key is normalized, so each engine is re-keyed once rather than per row.
    /// One engine's answers, re-keyed the way the table keys its rows.
    struct Answers {
        minor: (u16, u16),
        version: String,
        values: BTreeMap<String, String>,
    }
    let mut answers: Vec<Answers> = Vec::new();
    for e in engines {
        let mut values = BTreeMap::new();
        for (n, m) in &e.mined.rows {
            let key = normalize_const_fqn(n);
            if rows.contains_key(&key) {
                values.insert(key, decode_value(n, &m.ty, &m.value)?);
            }
        }
        answers.push(Answers { minor: e.minor, version: e.mined.php.clone(), values });
    }

    let mut out: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for (name, row) in rows {
        let Some((_, mined)) = &row.value else { continue };
        let mut seen: Vec<(String, String)> = Vec::new();
        for a in &answers {
            if row.since.is_some_and(|s| a.minor < s) {
                continue;
            }
            let Some(v) = a.values.get(name) else { continue };
            seen.push((a.version.clone(), v.clone()));
        }
        if seen.iter().any(|(_, v)| v != mined) {
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

/// Fail the run if a value bound for the committed file carries one of this
/// machine's own directories — an admitted string constant, or a build fact.
/// The [`BUILD_DEPENDENT`] roster is what *should* catch the first kind; this is
/// what makes a missing roster entry, or a fingerprint probe that answers with a
/// path, a failed mining run rather than an absolute path in a committed file.
fn leak_tripwire(what: &str, value: &str, dirs: &[String]) -> Result<(), String> {
    for d in dirs {
        if value.contains(d.as_str()) {
            return Err(format!(
                "{what} carries a directory of the mining machine — a constant belongs in \
                 BUILD_DEPENDENT in xtask/src/mine_constants.rs, a build fact needs a probe \
                 that answers without a path"
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
    engines: &[Engine],
    rows: &BTreeMap<String, Row>,
    refused: &BTreeMap<String, Refusal>,
    moves: &BTreeMap<String, Vec<(String, String)>>,
    declined: &BTreeMap<String, Vec<Decline>>,
) -> String {
    let top = engines.last().expect("at least one engine");
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
         # ONE SOURCE, ASKED ONCE PER MINOR. Values and extensions come from the TOP\n\
         # engine's `get_defined_constants(true)` (`mine_constants.php`); `since` and\n\
         # `until` come from the SAME engines' presence, one per minor. Issue #718\n\
         # retired the php-src branch scan that used to answer the range question: it\n\
         # read a `.stub.php` `const` or a `REGISTER_*_CONSTANT(\"NAME\", …)` in C, was\n\
         # blind to every registration built by macro token-pasting, and needed a\n\
         # per-extension coverage floor to keep its blind spots from minting wrong\n\
         # gates. An engine either has the name or it does not.\n\
         #\n\
         # PRESENCE IS JUDGED PER EXTENSION. The builds differ — the nix 8.4 build has\n\
         # no `brotli` — so a name is ranged only against the engines that LOADED its\n\
         # extension. A build without the extension is not a minor without the\n\
         # constant, and reading its silence as an absence would dress a packaging\n\
         # difference as a language change.\n\
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
         # AND A BOUNDARY NEEDS TWO COMPARABLE BUILDS. Having the extension is necessary\n\
         # and not sufficient: php-src guards whole blocks of registrations on the\n\
         # linked library's version and on configure-time features, so two engines that\n\
         # differ THERE disagree about which names exist for a reason that is not their\n\
         # PHP minor. `[engines.build]` records each build's fingerprint, and a boundary\n\
         # is minted only when the two engines forming it agree on every fact the\n\
         # extension's registrations are guarded on — `LIBXML_NO_XXE` is registered only\n\
         # above libxml2 2.13, `MHASH_*` only with `--with-mhash`, `LDAP_OPT_X_SASL_*`\n\
         # only with `HAVE_LDAP_SASL`, `IMAGETYPE_SWC` only with a zlib compiled in. An\n\
         # `until` is held higher still: it needs the same PACKAGER on both sides, since\n\
         # `gone at the next minor` and `the next minor was built by somebody else` are\n\
         # otherwise indistinguishable. `[declined.build_parity]` records what this took\n\
         # back and why.\n\
         #\n\
         # A ROW WITH NO VALUE. A name the older engines have and the top one does not\n\
         # has no value to mine and still has something to say, so it gets a row with\n\
         # `until` and no `v` at all (owner ruling 2026-09-11). Nothing reads it as an\n\
         # oracle: the value resolver has no literal to answer with, and the absence\n\
         # family's own version-skew gate (issue #28) already declines a claim made from\n\
         # a runtime outside the project's target, so a runtime past the departure\n\
         # reports the name absent on its own. The `until` is what this file RECORDS.\n\
         # `E_STRICT` is NOT one of these: the row exists (2048) and 8.5 still defines\n\
         # it, deprecated (issue #720). A name deprecated in place keeps its value and\n\
         # its row; only a name actually REMOVED loses the value and keeps the row.\n\n",
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
    let _ = writeln!(s, "# The engines this run diffed, low minor first; `php` above is the top");
    let _ = writeln!(s, "# one, whose values the rows carry. They answer BOTH questions: what a");
    let _ = writeln!(s, "# name is worth, and which minors have it. One entry = nothing was");
    let _ = writeln!(s, "# diffed and no row carries a range.");
    let _ = writeln!(s, "diffed = [");
    for e in engines {
        let _ = writeln!(s, "  {},", toml_str(&e.mined.php));
    }
    let _ = writeln!(s, "]");
    s.push('\n');

    // The fingerprints themselves, so a reader can check a declined boundary
    // against the two builds that declined it without re-running the miner.
    let _ = writeln!(s, "[engines.build]");
    let _ = writeln!(s, "# \"<php version>\" = {{ <fact> = \"<spelling>\", … }} — the linked-library");
    let _ = writeln!(s, "# versions and configure-time features php-src guards constant");
    let _ = writeln!(s, "# registrations on, plus `packager` and the `*_linkage` probes. `absent`");
    let _ = writeln!(s, "# is a value like any other: a build that cannot answer a fact differs");
    let _ = writeln!(s, "# from one that can.");
    for e in engines {
        let _ = write!(s, "{} = {{", toml_str(&e.mined.php));
        for (i, (fact, value)) in e.build.iter().enumerate() {
            let sep = if i == 0 { " " } else { ", " };
            let _ = write!(s, "{sep}{fact} = {}", toml_str(value));
        }
        let _ = writeln!(s, " }}");
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
    let value_less = rows.values().filter(|r| r.value.is_none()).count();
    let _ = writeln!(s, "[counts]");
    let _ = writeln!(s, "# mined      what `get_defined_constants(true)` had on the TOP engine, over");
    let _ = writeln!(s, "#            the catalog's extensions");
    let _ = writeln!(s, "# rows       spec-fixed literals admitted to the table, plus the value-less");
    let _ = writeln!(s, "# gated      of those, carrying a `since` the engines' presence proved");
    let _ = writeln!(s, "# value_less of those, carrying `until` and no value — the removed names");
    let _ = writeln!(s, "# refused_*  the three ADR-0094 §3 classes a mined row cannot carry, plus");
    let _ = writeln!(s, "#            the names the engines themselves disagreed about");
    let _ = writeln!(s, "# declined   names whose boundary the presence run found and the build");
    let _ = writeln!(s, "#            fingerprints took back — a packaging difference, not a minor");
    let _ = writeln!(s, "mined = {}", mined.constants_total);
    let _ = writeln!(s, "rows = {}", rows.len());
    let _ = writeln!(s, "gated = {gated}");
    let _ = writeln!(s, "value_less = {value_less}");
    let _ = writeln!(s, "declined_build_parity = {}", declined.len());
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

    // What the fingerprints took back. The evidence is the FACT the two builds
    // spell differently: `MHASH_ADLER32` does not arrive at 8.5, the 8.5 build is
    // the one configured `--with-mhash`, and that sentence is the whole review.
    let _ = writeln!(s, "[declined.build_parity]");
    let _ = writeln!(s, "# name = [[\"since|until\", \"<lower php>\", \"<upper php>\", \"<fact>\"], …] —");
    let _ = writeln!(s, "# a boundary the presence run found between two builds that are not");
    let _ = writeln!(s, "# comparable there. The row keeps its value and loses the range — and a");
    let _ = writeln!(s, "# name the TOP build lacks loses its row outright, since a departure it");
    let _ = writeln!(s, "# cannot prove is the only thing that row would have said.");
    for (name, gaps) in declined {
        let _ = write!(s, "{} = [", toml_key(name));
        for (i, d) in gaps.iter().enumerate() {
            let sep = if i == 0 { "" } else { ", " };
            let _ = write!(
                s,
                "{sep}[{}, {}, {}, {}]",
                toml_str(d.boundary),
                toml_str(&d.below),
                toml_str(&d.above),
                toml_str(d.fact)
            );
        }
        let _ = writeln!(s, "]");
    }
    s.push('\n');

    let _ = writeln!(s, "[const]");
    let _ = writeln!(s, "# name = {{ ext, t?, v?, since?, until? }} — `v` is the value's SPELLING: a");
    let _ = writeln!(s, "# decimal integer, PHP's own `var_export` float, `true`/`false`/`null`, or");
    let _ = writeln!(s, "# the string itself. Namespace segments are lowercased, the final segment");
    let _ = writeln!(s, "# is not (PHP's own rule for a constant's identity). A row with `until`");
    let _ = writeln!(s, "# and neither `t` nor `v` is a value-less row: the name is gone from the");
    let _ = writeln!(s, "# top engine, so there is no value to carry and only its departure is");
    let _ = writeln!(s, "# recorded.");
    for (name, r) in rows {
        let _ = write!(s, "{} = {{ ext = {}", toml_key(name), toml_str(&r.ext));
        if let Some((ty, value)) = &r.value {
            let _ = write!(s, ", t = {}, v = {}", toml_str(ty), toml_str(value));
        }
        if let Some((maj, min)) = r.since {
            let _ = write!(s, ", since = \"{maj}.{min}\"");
        }
        if let Some((maj, min)) = r.until {
            let _ = write!(s, ", until = \"{maj}.{min}\"");
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

    /// One engine's answer, spelled the way the miner's JSON does. `exts` is the
    /// build's extension set — what decides whether this engine may judge a
    /// name's presence at all.
    fn engine(version: &str, exts: &[&str], rows: &[(&str, &str, &str)]) -> Engine {
        build_of(version, exts, rows, &[])
    }

    /// The same, with a build fingerprint: `facts` is `(fact, spelling)`, and
    /// `packager` defaults to one shared value so a test that says nothing about
    /// packaging is not silently testing the `until` packager rule.
    fn build_of(
        version: &str,
        exts: &[&str],
        rows: &[(&str, &str, &str)],
        facts: &[(&str, &str)],
    ) -> Engine {
        let mined = Mined {
            php: version.to_owned(),
            build: BTreeMap::new(),
            extensions: exts.iter().map(|e| (*e).to_owned()).collect(),
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
        let names = mined.rows.keys().map(|n| normalize_const_fqn(n)).collect();
        let exts = mined.extensions.iter().cloned().collect();
        let mut build = BTreeMap::from([("packager".to_owned(), "nix".to_owned())]);
        build.extend(facts.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())));
        Engine { bin: "php".to_owned(), minor, mined, exts, names, build }
    }

    /// `range`'s pair alone, for the tests that have nothing to say about why a
    /// boundary was declined.
    fn ranged(engines: &[Engine], key: &str, ext: &str) -> MinorRange {
        range(engines, key, ext).0
    }

    fn row(ext: &str, value: &str, since: Option<(u16, u16)>) -> Row {
        Row {
            ext: ext.to_owned(),
            value: Some(("int".to_owned(), value.to_owned())),
            since,
            until: None,
        }
    }

    #[test]
    fn engines_that_disagree_inside_a_rows_range_refuse_it() {
        // The four names this check first caught are all of this shape: the top
        // engine says one number and a supported minor says another, so the row
        // the top engine would write is false at a minor the target may span.
        let engines = vec![
            engine("8.4.25", &["gd"], &[("IMAGETYPE_COUNT", "gd", "20")]),
            engine("8.5.10", &["gd"], &[("IMAGETYPE_COUNT", "gd", "22")]),
        ];
        let rows = BTreeMap::from([("IMAGETYPE_COUNT".to_owned(), row("gd", "22", None))]);
        let moves = value_moves(&engines, &rows).expect("diff");
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
        let engines = vec![
            engine("8.4.25", &["json"], &[("JSON_THROW_ON_ERROR", "json", "4194304")]),
            engine("8.5.10", &["json"], &[("JSON_THROW_ON_ERROR", "json", "4194304")]),
        ];
        let rows =
            BTreeMap::from([("JSON_THROW_ON_ERROR".to_owned(), row("json", "4194304", None))]);
        assert!(value_moves(&engines, &rows).expect("diff").is_empty());
    }

    #[test]
    fn a_disagreement_below_the_rows_since_is_the_ranges_business() {
        // The one thing `since` is allowed to excuse: the row does not speak for
        // 8.2 at all, so what 8.2 calls the name is not a counter-example to it.
        let engines = vec![
            engine("8.2.33", &["standard"], &[("LATE_ARRIVAL", "standard", "3")]),
            engine("8.5.10", &["standard"], &[("LATE_ARRIVAL", "standard", "7")]),
        ];
        let rows =
            BTreeMap::from([("LATE_ARRIVAL".to_owned(), row("standard", "7", Some((8, 3))))]);
        assert!(value_moves(&engines, &rows).expect("diff").is_empty());
    }

    #[test]
    fn an_engine_that_lacks_the_name_says_nothing() {
        // A build without the extension is an absence, which is the version
        // gate's question — never a disagreement about a value.
        let engines = vec![
            engine("8.2.33", &[], &[]),
            engine("8.5.10", &["pgsql"], &[("PGSQL_ASSOC", "pgsql", "1")]),
        ];
        let rows = BTreeMap::from([("PGSQL_ASSOC".to_owned(), row("pgsql", "1", None))]);
        assert!(value_moves(&engines, &rows).expect("diff").is_empty());
    }

    /// **The presence-derived `since`** (issue #718): the engines that loaded the
    /// extension disagree about whether the NAME is there, and the lowest one that
    /// has it is where it arrived. Delete the `first > 0` clause in [`range`] and
    /// this reads `None`, which is the rangeless table the php-src scan produced.
    #[test]
    fn a_name_the_lower_engines_lack_is_gated_at_its_arrival() {
        let engines = vec![
            engine("8.2.33", &["standard"], &[]),
            engine("8.3.33", &["standard"], &[("LATE_ARRIVAL", "standard", "7")]),
            engine("8.5.10", &["standard"], &[("LATE_ARRIVAL", "standard", "7")]),
        ];
        assert_eq!(ranged(&engines, "LATE_ARRIVAL", "standard"), (Some((8, 3)), None));
    }

    /// **The presence-derived `until`**: the name is there through 8.4 and gone at
    /// 8.5, so the row it mints is the value-less one the absence family reads.
    #[test]
    fn a_name_the_top_engine_lost_carries_its_departure() {
        let engines = vec![
            engine("8.4.25", &["standard"], &[("DEPARTED", "standard", "7")]),
            engine("8.5.10", &["standard"], &[]),
        ];
        assert_eq!(ranged(&engines, "DEPARTED", "standard"), (None, Some((8, 4))));
    }

    /// **A build difference is not a language change.** The nix 8.4 build has no
    /// `brotli`, and reading its silence as an absence would gate the whole
    /// extension at 8.5. Delete [`Engine::judges`] from [`range`]'s filter and this
    /// test reports `since = 8.5` for a constant that has been there all along.
    #[test]
    fn an_engine_without_the_extension_judges_nothing() {
        let engines = vec![
            engine("8.4.25", &["standard"], &[]),
            engine("8.5.10", &["standard", "brotli"], &[("BROTLI_GENERIC", "brotli", "0")]),
        ];
        assert_eq!(ranged(&engines, "BROTLI_GENERIC", "brotli"), (None, None));
    }

    /// One engine knows only its own minor, so it proves neither an arrival nor a
    /// departure — which is what makes a single-engine run rangeless rather than a
    /// table claiming every name arrived at that minor.
    ///
    /// The pair is the point, and the clauses it pins are [`range`]'s `first > 0`
    /// and `last + 1 < judges.len()`: a boundary is the gap BETWEEN two engines,
    /// so the second engine is what creates one. Drop either clause and the lone
    /// judge starts answering `since = 8.5` — which is the rangeless run turned
    /// into a table claiming every name arrived at the top minor.
    #[test]
    fn one_judge_ranges_nothing() {
        let alone = vec![engine("8.5.10", &["standard"], &[("SORT_REGULAR", "standard", "0")])];
        assert_eq!(ranged(&alone, "SORT_REGULAR", "standard"), (None, None));
        assert_eq!(ranged(&alone, "NOT_HERE", "standard"), (None, None));
        // The same top engine with a neighbour below it that lacks the name: now
        // there IS a gap, and it is the arrival.
        let paired = vec![
            engine("8.4.25", &["standard"], &[]),
            engine("8.5.10", &["standard"], &[("SORT_REGULAR", "standard", "0")]),
        ];
        assert_eq!(ranged(&paired, "SORT_REGULAR", "standard"), (Some((8, 5)), None));
    }

    /// Absent, present, absent, present: the builds differ in some way the
    /// extension check did not catch, so no range at all rather than a guessed one.
    ///
    /// Four engines and not three, so the clause is load-bearing: with the
    /// contiguity check deleted, the first `present` reads as an arrival and the
    /// row mints `since = 8.3` over a minor that does not have the name.
    #[test]
    fn a_gap_in_the_presence_run_ranges_nothing() {
        let engines = vec![
            engine("8.2.33", &["standard"], &[]),
            engine("8.3.33", &["standard"], &[("PATCHY", "standard", "1")]),
            engine("8.4.25", &["standard"], &[]),
            engine("8.5.10", &["standard"], &[("PATCHY", "standard", "1")]),
        ];
        assert_eq!(ranged(&engines, "PATCHY", "standard"), (None, None));
    }

    /// **The boundary the fingerprints take back** — `LIBXML_NO_XXE` in miniature.
    /// Both builds load `libxml` and they link different libxml2s, and php-src
    /// registers the name only above 2.13: the presence run sees an arrival and
    /// the arrival is libxml2's, not PHP's.
    #[test]
    fn a_boundary_between_two_incomparable_builds_is_declined() {
        let engines = vec![
            build_of("8.4.25", &["libxml"], &[], &[("libxml", "20913")]),
            build_of(
                "8.5.10",
                &["libxml"],
                &[("LIBXML_NO_XXE", "libxml", "1")],
                &[("libxml", "21503")],
            ),
        ];
        let (pair, declined) = range(&engines, "LIBXML_NO_XXE", "libxml");
        assert_eq!(pair, (None, None));
        assert_eq!(
            declined,
            vec![Decline {
                boundary: "since",
                below: "8.4.25".to_owned(),
                above: "8.5.10".to_owned(),
                fact: "libxml",
            }]
        );
        // The same two minors with ONE libxml2 between them: now the arrival is
        // PHP's, and the row is gated at it.
        let agreed = vec![
            build_of("8.4.25", &["libxml"], &[], &[("libxml", "21503")]),
            build_of(
                "8.5.10",
                &["libxml"],
                &[("LIBXML_NO_XXE", "libxml", "1")],
                &[("libxml", "21503")],
            ),
        ];
        assert_eq!(ranged(&agreed, "LIBXML_NO_XXE", "libxml"), (Some((8, 5)), None));
    }

    /// A fact `BUILD_PARITY` charges to some OTHER extension is not this row's
    /// business: the prefixes stay as narrow as the claim, the way the
    /// build-dependent families' do.
    #[test]
    fn a_fact_another_extension_is_guarded_on_declines_nothing() {
        let engines = vec![
            build_of("8.4.25", &["json"], &[], &[("libxml", "20913")]),
            build_of("8.5.10", &["json"], &[("JSON_LATE", "json", "1")], &[("libxml", "21503")]),
        ];
        assert_eq!(ranged(&engines, "JSON_LATE", "json"), (Some((8, 5)), None));
    }

    /// **An `until` is never minted across a packager change.** The two builds
    /// agree on every fact the extension is guarded on and were built by different
    /// people, and "the name is gone at 8.5" and "8.5 was packaged by somebody
    /// else" are the same observation until a second 8.5 build says otherwise.
    ///
    /// The arrival at the same boundary is NOT held to this bar: a wrong `since`
    /// keeps the resolver quiet below it, and a wrong `until` is a row asserting a
    /// departure that never happened.
    #[test]
    fn a_departure_across_a_packager_change_is_declined() {
        let engines = vec![
            build_of("8.4.25", &["mysqli"], &[("GONE", "mysqli", "1")], &[("packager", "nix")]),
            build_of("8.5.10", &["mysqli"], &[], &[("packager", "homebrew")]),
        ];
        let (pair, declined) = range(&engines, "GONE", "mysqli");
        assert_eq!(pair, (None, None));
        assert_eq!(declined.first().map(|d| (d.boundary, d.fact)), Some(("until", "packager")));
        // One packager on both sides and the departure stands.
        let same = vec![
            build_of("8.3.33", &["mysqli"], &[("GONE", "mysqli", "1")], &[]),
            build_of("8.4.25", &["mysqli"], &[], &[]),
        ];
        assert_eq!(ranged(&same, "GONE", "mysqli"), (None, Some((8, 3))));
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
