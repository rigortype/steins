//! Issue #871: every class the mined hierarchy lists resolves through
//! `ReflectionClass` on the project's own PHP **under the key it is stored as**.
//!
//! The miner once dropped the namespace of a class declared in a
//! `namespace X` whose brace sits on the next line, so `Random\RandomException`
//! was stored as `randomexception`. The table passed every other test, because
//! nothing asked the engine whether a key named what it said.
//!
//! This asks. Each row of `hierarchy.toml` is offered to the live engine by its
//! stored key; the engine's own `ReflectionClass::getName()` must lowercase to that
//! key and carry the casing php-src declares. A row the engine lacks has to **say
//! why**, and there is no floor that lets an unexplained absence through:
//!
//! * the row is marked `absent_on_pinned`, which the miner writes for a row the
//!   PHP it cross-checked against did not declare (a php-src stub newer than that
//!   minor, an extension it was built without); or
//! * the extension its stub belongs to is not loaded here; or
//! * this PHP is an older minor than the cross-check's, which may lack a class of
//!   a loaded extension that the newer one added.
//!
//! A mark is the miner's word for the same name it is being asked about, so it
//! cannot catch a key the miner got wrong (a `Dom\Text` mined as `Text` would be
//! marked absent as well). Two things do. A row marked although its extension is
//! loaded must be one of [`NEWER_THAN_THE_CROSS_CHECK`], an explicit list a re-mine
//! has to revisit. And the catalog holds the converse statically: every namespaced
//! class PHP 8.5.11 declares is a row under its FQN
//! (`every_namespaced_class_php_src_declares_is_keyed_by_fqn`).
//!
//! Skipped with a marker when no `php` answers.

use steins_sidecar::Sidecar;

/// Marked rows whose extension is loaded on a PHP that has them, because the stubs are
/// a later php-src than the cross-check PHP: a new `Io` and `Stream*` API, `ext/uri`'s
/// builder, `ext/openssl`'s session classes, `SortDirection`, `ext/intl`'s number range
/// formatter (needs a newer ICU than the cross-check build's), and two stub-only PDO
/// names (`PDO_PGSql_Ext`, `PDO_SQLite_Ext`) no build registers. Lowercased.
const NEWER_THAN_THE_CROSS_CHECK: &[&str] = &[
    "intlnumberrangeformatter",
    "io\\ioexception",
    "io\\poll\\backend",
    "io\\poll\\backendunavailableexception",
    "io\\poll\\context",
    "io\\poll\\event",
    "io\\poll\\failedcontextinitializationexception",
    "io\\poll\\failedhandleaddexception",
    "io\\poll\\failedpolloperationexception",
    "io\\poll\\failedpollwaitexception",
    "io\\poll\\failedwatchermodificationexception",
    "io\\poll\\handle",
    "io\\poll\\handlealreadywatchedexception",
    "io\\poll\\inactivewatcherexception",
    "io\\poll\\invalidhandleexception",
    "io\\poll\\pollexception",
    "io\\poll\\watcher",
    "openssl\\opensslexception",
    "openssl\\psk",
    "openssl\\session",
    "pdo_pgsql_ext",
    "pdo_sqlite_ext",
    "sortdirection",
    "streamerror",
    "streamerrorcode",
    "streamerrormode",
    "streamerrorstore",
    "streamexception",
    "streampollhandle",
    "uri\\rfc3986\\uribuilder",
    "uri\\rfc3986\\urihosttype",
    "uri\\rfc3986\\uritype",
    "uri\\whatwg\\urlhosttype",
];

/// One `[[class]]` row of `hierarchy.toml`, as far as this test reads it.
struct Row {
    name: String,
    source: String,
    marked: bool,
}

/// The committed hierarchy's rows. The file is the generator's input, written in one
/// fixed shape (`key = 'value'` lines under each `[[class]]`), so a line scan reads it.
fn rows() -> Vec<Row> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/research/phpsrc-mining/hierarchy.toml"
    );
    let text = std::fs::read_to_string(path).expect("the committed hierarchy.toml");
    let mut rows: Vec<Row> = Vec::new();
    for line in text.lines() {
        if line == "[[class]]" {
            rows.push(Row { name: String::new(), source: String::new(), marked: false });
        } else if let Some(row) = rows.last_mut() {
            if let Some(v) = line.strip_prefix("name = '") {
                row.name = v.trim_end_matches('\'').to_owned();
            } else if let Some(v) = line.strip_prefix("source = '") {
                row.source = v.trim_end_matches('\'').to_owned();
            } else if line == "absent_on_pinned = true" {
                row.marked = true;
            }
        }
    }
    rows
}

/// The extension a stub's `source` (`ext/dom/php_dom.stub.php:8`) belongs to, as
/// `extension_loaded` names it (lowercased).
fn extension_of(source: &str) -> String {
    let mut parts = source.split('/');
    match (parts.next(), parts.next()) {
        (Some("ext"), Some("opcache")) => "zend opcache".to_owned(),
        (Some("ext"), Some(dir)) => dir.to_owned(),
        _ => "core".to_owned(),
    }
}

/// `(major, minor)` of a `8.5.11` version string.
fn minor_of(version: &str) -> (u32, u32) {
    let mut it = version.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    (it.next().unwrap_or(0), it.next().unwrap_or(0))
}

#[test]
fn every_hierarchy_key_resolves_under_the_key_it_is_stored_as() {
    let Ok(mut sidecar) = Sidecar::spawn() else {
        eprintln!("SKIP every_hierarchy_key_resolves…: no PHP engine — is `php` on PATH?");
        return;
    };
    let env = sidecar.env().expect("a live engine answers `env`");
    let loaded: Vec<String> = env.extensions.iter().map(|e| e.to_ascii_lowercase()).collect();
    let checked_against = minor_of(steins_catalog::hierarchy_cross_checked_php());
    let older = minor_of(&env.php_version) < checked_against;

    let rows = rows();
    assert!(rows.len() >= 300, "hierarchy.toml lists only {} rows", rows.len());
    let mut resolved = 0;
    let mut unexplained: Vec<String> = Vec::new();
    for row in &rows {
        let key = row.name.to_ascii_lowercase();
        // The generated table and the TOML it came from agree on the mark.
        assert_eq!(
            steins_catalog::builtin_class_absent_on_pinned(&key),
            row.marked,
            "`{}`: the generated mark disagrees with hierarchy.toml",
            row.name
        );
        let answer = sidecar.reflect_class(&key).expect("a live engine answers");
        let Some(class) = answer.declaration else {
            let ext = extension_of(&row.source);
            let ext_missing = !loaded.contains(&ext);
            if !(row.marked || ext_missing || older) {
                unexplained.push(row.name.clone());
            }
            if row.marked && !ext_missing && !NEWER_THAN_THE_CROSS_CHECK.contains(&key.as_str()) {
                let why = "marked, its extension is loaded, and it is not a listed newer row";
                unexplained.push(format!("{} ({why}; `{ext}`)", row.name));
            }
            continue;
        };
        assert_eq!(
            class.name.to_ascii_lowercase(),
            key,
            "`{key}` resolves to `{}`: the key does not name the class it is stored for",
            class.name
        );
        assert_eq!(class.name, row.name, "`{key}`: the engine's casing is not the stored one");
        assert!(class.internal, "`{key}` is a class of the engine, not the project's: {class:?}");
        assert!(!row.marked, "`{key}` is marked absent_on_pinned and this PHP declares it");
        resolved += 1;
    }
    assert!(
        unexplained.is_empty(),
        "{} rows the engine lacks explain no absence (resolved {resolved} of {}): {unexplained:?}",
        unexplained.len(),
        rows.len()
    );
    assert!(resolved > 0, "no row resolved");
}

/// Namespaced classes every supported PHP has (`ext/random` is built in since 8.2):
/// their absence from a live engine is a wrong key, not a missing extension.
#[test]
fn the_always_present_namespaced_rows_resolve() {
    let Ok(mut sidecar) = Sidecar::spawn() else {
        eprintln!("SKIP the_always_present_namespaced_rows_resolve: no PHP engine");
        return;
    };
    for class in [
        "Random\\RandomException",
        "Random\\BrokenRandomEngineError",
        "Random\\RandomError",
        "Random\\Randomizer",
        "Random\\Engine",
        "Random\\Engine\\Mt19937",
    ] {
        let key = class.to_ascii_lowercase();
        let found = sidecar.reflect_class(&key).expect("a live engine answers");
        assert!(found.exists(), "`{class}` must resolve on every supported PHP");
        assert!(!steins_catalog::builtin_class_absent_on_pinned(&key), "{class}");
    }
}

/// The hierarchy's parents are keys of the same table: a parent spelled relative
/// to the declaring namespace and left unqualified would name no row.
#[test]
fn every_parent_a_hierarchy_row_names_is_itself_a_row() {
    for (key, _) in steins_catalog::engine_class_declarations() {
        let Some(supers) = steins_catalog::builtin_class_supers(key) else { continue };
        for parent in supers {
            assert!(
                steins_catalog::builtin_class_display(parent).is_some(),
                "`{key}` extends or implements `{parent}`, which no hierarchy row declares"
            );
        }
    }
}
