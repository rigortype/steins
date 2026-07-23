//! Diagnostic-registry totality (ADR-0050 §2).
//!
//! The registry ([`DIAGNOSTIC_REGISTRY`]) carries every diagnostic id together
//! with its [`Layer`], and is the single source of truth: `DIAGNOSTIC_IDS` is
//! derived from it and `layer()` reads it. These tests bind the registry to the
//! emitters' canonical id list ([`ALL_EMITTABLE_IDS`]) both directions, so an id
//! that any emitter can produce but that is not registered *with a layer* — the
//! exact defect ADR-0050 §2 forbids — cannot pass CI.

use std::collections::HashSet;

use steins_infer::{
    ALL_EMITTABLE_IDS, CALL_ON_NULL_ID, DIAGNOSTIC_IDS, DIAGNOSTIC_REGISTRY, EFFECT_ID,
    EFFECT_LISKOV_ID, ID, Layer, PARAM_MISMATCH_ID, PHPDOC_PROP_MISMATCH_ID, PROP_MISMATCH_ID,
    READONLY_REASSIGNED_ID, RETURN_ID, RETURN_MISMATCH_ID, SUPPRESS_UNKNOWN_ID,
    SUPPRESS_UNMATCHED_ID, THROW_LISKOV_ID, THROW_UNDECLARED_ID, UNKNOWN_LABEL_ID, layer,
};

/// Totality, forward: every id an emitter can produce is registered *with* a layer.
#[test]
fn every_emittable_id_is_registered_with_a_layer() {
    for &id in ALL_EMITTABLE_IDS {
        assert!(
            layer(id).is_some(),
            "emittable id `{id}` has no registry entry — ADR-0050 §2 totality violated \
             (add it to DIAGNOSTIC_REGISTRY with its layer)"
        );
    }
}

/// Totality, backward: the registry has no phantom ids — every registered id is one
/// an emitter actually produces, so the two lists are a bijection.
#[test]
fn registry_has_no_unemittable_ids() {
    let emittable: HashSet<&str> = ALL_EMITTABLE_IDS.iter().copied().collect();
    for &(id, _) in DIAGNOSTIC_REGISTRY {
        assert!(
            emittable.contains(id),
            "registered id `{id}` is not in ALL_EMITTABLE_IDS — either it is dead \
             (drop it) or an emit site was added without listing it"
        );
    }
    // Same cardinality both ways ⇒ set equality (no duplicates in either list).
    assert_eq!(DIAGNOSTIC_REGISTRY.len(), ALL_EMITTABLE_IDS.len());
    let regset: HashSet<&str> = DIAGNOSTIC_REGISTRY.iter().map(|(i, _)| *i).collect();
    assert_eq!(regset.len(), DIAGNOSTIC_REGISTRY.len(), "duplicate id in DIAGNOSTIC_REGISTRY");
    assert_eq!(emittable.len(), ALL_EMITTABLE_IDS.len(), "duplicate id in ALL_EMITTABLE_IDS");
}

/// `DIAGNOSTIC_IDS` is a faithful projection of the registry (single source of
/// truth): same ids, same order.
#[test]
fn diagnostic_ids_is_derived_from_registry() {
    let derived: Vec<&str> = DIAGNOSTIC_REGISTRY.iter().map(|(i, _)| *i).collect();
    assert_eq!(DIAGNOSTIC_IDS, derived.as_slice());
}

/// The classification is exactly ADR-0050 §1, verbatim — pinned so a silent
/// re-layering of any id (which *is* allowed, but only by ADR) trips the test.
#[test]
fn classification_matches_adr_0050_section_1() {
    // proof
    assert_eq!(layer(ID), Some(Layer::Proof));
    assert_eq!(layer(RETURN_ID), Some(Layer::Proof));
    assert_eq!(layer(PROP_MISMATCH_ID), Some(Layer::Proof));
    assert_eq!(layer(CALL_ON_NULL_ID), Some(Layer::Proof));
    assert_eq!(layer(READONLY_REASSIGNED_ID), Some(Layer::Proof));
    // contract
    assert_eq!(layer(PARAM_MISMATCH_ID), Some(Layer::Contract));
    assert_eq!(layer(RETURN_MISMATCH_ID), Some(Layer::Contract));
    assert_eq!(layer(PHPDOC_PROP_MISMATCH_ID), Some(Layer::Contract));
    assert_eq!(layer(THROW_UNDECLARED_ID), Some(Layer::Contract));
    assert_eq!(layer(THROW_LISKOV_ID), Some(Layer::Contract));
    assert_eq!(layer(EFFECT_ID), Some(Layer::Contract));
    assert_eq!(layer(EFFECT_LISKOV_ID), Some(Layer::Contract));
    // mechanics
    assert_eq!(layer(SUPPRESS_UNMATCHED_ID), Some(Layer::Mechanics));
    assert_eq!(layer(SUPPRESS_UNKNOWN_ID), Some(Layer::Mechanics));
    assert_eq!(layer(UNKNOWN_LABEL_ID), Some(Layer::Mechanics));
}

/// An unregistered id has no layer (the lookup is exact, not prefix-based).
#[test]
fn unregistered_id_has_no_layer() {
    assert_eq!(layer("type.bogus"), None);
    assert_eq!(layer("nope"), None);
    assert_eq!(layer(""), None);
    // A family prefix is not itself an id.
    assert_eq!(layer("type"), None);
}

/// The wire spellings for the `--format json` `layer` field (ADR-0050 §2).
#[test]
fn layer_wire_spellings() {
    assert_eq!(Layer::Proof.as_str(), "proof");
    assert_eq!(Layer::Contract.as_str(), "contract");
    assert_eq!(Layer::Mechanics.as_str(), "mechanics");
}
