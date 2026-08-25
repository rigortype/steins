//! The generation fingerprint: what moves it, what cannot.

use steins_gen::{EnginePosture, Fingerprint, GenerationInputs, PackageName};

fn pkg(name: &str) -> PackageName {
    PackageName::new(name).unwrap()
}

fn fp(seed: &str) -> Fingerprint {
    Fingerprint::of_bytes("steins-gen/test-seed", seed.as_bytes())
}

fn baseline() -> GenerationInputs {
    GenerationInputs {
        analyzer_version: "0.1.6".to_owned(),
        packages: vec![(pkg("__first_party__"), fp("fp")), (pkg("vendor/lib"), fp("lib"))],
        composer_lock: Some(fp("lock")),
        catalog_pin: "php-8.4:2026-08".to_owned(),
        plugins: vec!["a-plugin".to_owned(), "b-plugin".to_owned()],
        engine: EnginePosture::On {
            php_version: "8.4.11".to_owned(),
            int_size: 8,
            extensions: vec!["json".to_owned(), "mbstring".to_owned()],
            fold_lane: "process".to_owned(),
        },
        config: vec![("level".to_owned(), "strict".to_owned())],
    }
}

/// Collections are identity, not order: the tags and internal sorting make
/// the supplied order immaterial.
#[test]
fn reordering_collections_leaves_the_id_fixed() {
    let a = baseline();
    let mut b = baseline();
    b.packages.reverse();
    b.plugins.reverse();
    if let EnginePosture::On { extensions, .. } = &mut b.engine {
        extensions.reverse();
    }
    assert_eq!(a.generation_id(), b.generation_id());

    let mut c = baseline();
    c.config.push(("mode".to_owned(), "ci".to_owned()));
    let mut d = baseline();
    d.config.insert(0, ("mode".to_owned(), "ci".to_owned()));
    assert_eq!(c.generation_id(), d.generation_id());
    assert_ne!(a.generation_id(), c.generation_id());
}

/// Every covered field moves the id when it moves.
#[test]
fn each_covered_field_moves_the_id() {
    let base = baseline().generation_id();
    let mut ids = vec![base];

    let mut m = baseline();
    m.analyzer_version = "0.1.7".to_owned();
    ids.push(m.generation_id());

    let mut m = baseline();
    m.packages[1].1 = fp("lib-edited");
    ids.push(m.generation_id());

    let mut m = baseline();
    m.packages.push((pkg("vendor/extra"), fp("extra")));
    ids.push(m.generation_id());

    let mut m = baseline();
    m.composer_lock = Some(fp("other-lock"));
    ids.push(m.generation_id());

    let mut m = baseline();
    m.composer_lock = None;
    ids.push(m.generation_id());

    let mut m = baseline();
    m.catalog_pin = "php-8.4:2026-09".to_owned();
    ids.push(m.generation_id());

    let mut m = baseline();
    m.plugins.push("c-plugin".to_owned());
    ids.push(m.generation_id());

    let mut m = baseline();
    m.engine = EnginePosture::Off;
    ids.push(m.generation_id());

    let mut m = baseline();
    if let EnginePosture::On { php_version, .. } = &mut m.engine {
        *php_version = "8.3.20".to_owned();
    }
    ids.push(m.generation_id());

    let mut m = baseline();
    if let EnginePosture::On { int_size, .. } = &mut m.engine {
        *int_size = 4;
    }
    ids.push(m.generation_id());

    let mut m = baseline();
    if let EnginePosture::On { extensions, .. } = &mut m.engine {
        extensions.push("intl".to_owned());
    }
    ids.push(m.generation_id());

    let mut m = baseline();
    if let EnginePosture::On { fold_lane, .. } = &mut m.engine {
        *fold_lane = "browser".to_owned();
    }
    ids.push(m.generation_id());

    let mut m = baseline();
    m.config[0].1 = "lax".to_owned();
    ids.push(m.generation_id());

    let count = ids.len();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), count, "two distinct input sets fingerprinted alike");
}

/// The length prefix keeps adjacent values from colliding by concatenation:
/// two plugin sets with the same concatenated spelling still differ.
#[test]
fn adjacent_values_cannot_collide_by_concatenation() {
    let mut a = baseline();
    a.plugins = vec!["ab".to_owned(), "c".to_owned()];
    let mut b = baseline();
    b.plugins = vec!["a".to_owned(), "bc".to_owned()];
    assert_ne!(a.generation_id(), b.generation_id());
}

/// The id round-trips through its hex spelling — the store's directory name.
#[test]
fn id_round_trips_through_hex() {
    let id = baseline().generation_id();
    let hex = id.to_hex();
    assert_eq!(hex.len(), Fingerprint::HEX_LEN);
    assert_eq!(steins_gen::GenerationId::from_hex(&hex), Some(id));
    assert_eq!(steins_gen::GenerationId::from_hex("not-hex"), None);
}
