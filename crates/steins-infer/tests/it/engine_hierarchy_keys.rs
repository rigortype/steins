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
//! why**, and there is no floor that lets an unexplained absence through. Exactly
//! these explain one:
//!
//! * its extension is not loaded here (`extension_loaded` of the stub's extension);
//! * this PHP is an older minor than the pin (`steins_catalog::PINNED_PHP`), which may
//!   lack a class of a loaded extension that the pinned release added; or
//! * the row is one of [`STUB_ONLY`].
//!
//! The table is the pinned release's (the stubs at tag `php-8.5.11`), so a row is a class
//! that release declares when its extension is built in. A row mined under a wrong key
//! (`Dom\Text` as `Text`) is declared at the tag under that same wrong key, so it is
//! absent with its extension loaded on a PHP of the pinned minor: unexplained. The
//! catalog also holds the converse statically: every namespaced class PHP 8.5.11 declares
//! is a row under its FQN (`every_namespaced_class_php_src_declares_is_keyed_by_fqn`).
//!
//! Skipped with a marker when no `php` answers.

use steins_sidecar::Sidecar;

/// Rows declared in a stub that is explicit about them not being classes: `ext/pdo_pgsql`
/// and `ext/pdo_sqlite` hang their driver methods on `PDO_PGSql_Ext` and `PDO_SQLite_Ext`
/// ("These are extension methods for PDO. This is not a real class."), and no build
/// registers either. Lowercased.
const STUB_ONLY: &[&str] = &["pdo_pgsql_ext", "pdo_sqlite_ext"];

/// One `[[class]]` row of `hierarchy.toml`, as far as this test reads it.
struct Row {
    name: String,
    source: String,
    /// The stub's class-level `@alias` names (issue #917).
    aliases: Vec<String>,
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
            rows.push(Row { name: String::new(), source: String::new(), aliases: Vec::new() });
        } else if let Some(row) = rows.last_mut() {
            if let Some(v) = line.strip_prefix("name = '") {
                row.name = v.trim_end_matches('\'').to_owned();
            } else if let Some(v) = line.strip_prefix("source = '") {
                row.source = v.trim_end_matches('\'').to_owned();
            } else if let Some(v) = line.strip_prefix("aliases = [") {
                row.aliases = v
                    .trim_end_matches(']')
                    .split(", ")
                    .map(|a| a.trim_matches('\'').to_owned())
                    .collect();
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
    let older = {
        let (major, minor) = minor_of(&env.php_version);
        (major as u16, minor as u16) < steins_catalog::PINNED_PHP
    };

    let rows = rows();
    assert!(rows.len() >= 300, "hierarchy.toml lists only {} rows", rows.len());
    let mut resolved = 0;
    let mut unexplained: Vec<String> = Vec::new();
    for row in &rows {
        let key = row.name.to_ascii_lowercase();
        let answer = sidecar.reflect_class(&key).expect("a live engine answers");
        let Some(class) = answer.declaration else {
            let ext = extension_of(&row.source);
            let explained =
                !loaded.contains(&ext) || older || STUB_ONLY.contains(&key.as_str());
            if !explained {
                unexplained.push(format!("{} (extension `{ext}` is loaded)", row.name));
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

/// Issue #917: a stub's class-level `@alias` is a second name of the same class, so the
/// engine answers the **declared** name for either spelling, and the catalog's table says
/// exactly that. The pin lists one (`Dom\DOMException`); a PHP without `ext/dom`, or older
/// than the pin, has no class to compare and the engine half is skipped.
#[test]
fn a_mined_second_name_resolves_to_the_declared_class() {
    let with_second_names: Vec<Row> =
        rows().into_iter().filter(|row| !row.aliases.is_empty()).collect();
    assert!(!with_second_names.is_empty(), "the pin mines at least one class-level alias");
    for row in &with_second_names {
        for second in &row.aliases {
            assert_eq!(
                steins_catalog::builtin_class_alias(second),
                Some(row.name.as_str()),
                "`{second}`: the catalog's table disagrees with the mined row"
            );
        }
    }
    let Ok(mut sidecar) = Sidecar::spawn() else {
        eprintln!("SKIP a_mined_second_name_resolves… (engine half): no PHP engine");
        return;
    };
    let env = sidecar.env().expect("a live engine answers `env`");
    let loaded: Vec<String> = env.extensions.iter().map(|e| e.to_ascii_lowercase()).collect();
    let (major, minor) = minor_of(&env.php_version);
    if (major as u16, minor as u16) < steins_catalog::PINNED_PHP {
        return;
    }
    for row in &with_second_names {
        if !loaded.contains(&extension_of(&row.source)) {
            continue;
        }
        for second in &row.aliases {
            let answer = sidecar.reflect_class(&second.to_ascii_lowercase()).expect("answers");
            let class = answer.declaration.expect("a second name of a loaded class resolves");
            assert_eq!(class.name, row.name, "`{second}` is a second name of `{}`", row.name);
        }
    }
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
