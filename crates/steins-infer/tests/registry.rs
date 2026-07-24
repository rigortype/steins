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
    ALL_EMITTABLE_IDS, CALL_ON_NULL_ID, CALL_TOO_FEW_ARGUMENTS_ID, CALL_TOO_MANY_ARGUMENTS_ID,
    CALL_UNDEFINED_FUNCTION_ID, CALL_UNDEFINED_METHOD_ID, CALL_UNKNOWN_NAMED_ARGUMENT_ID,
    CLASS_UNDEFINED_ID, DIAGNOSTIC_IDS, DIAGNOSTIC_REGISTRY, EFFECT_ID, EFFECT_LISKOV_ID,
    FACET_ORIGIN, Facet, ID, Layer, OFFSET_MISSING_ID, OFFSET_ON_UNSUPPORTED_ID, Origin,
    PARAM_MISMATCH_ID, PHPDOC_PROP_MISMATCH_ID, PHPDOC_UNDEFINED_METHOD_ID, PROP_MISMATCH_ID,
    READONLY_REASSIGNED_ID, REGISTERED_NOT_YET_EMITTED, RETURN_ID, RETURN_MISMATCH_ID,
    SUPPRESS_UNKNOWN_ID, SUPPRESS_UNMATCHED_ID, THROW_LISKOV_ID, THROW_UNDECLARED_ID,
    UNKNOWN_LABEL_ID, declared_facet, layer,
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

/// Totality, backward: the registry has no phantom ids — every registered id is
/// either one an emitter actually produces (`ALL_EMITTABLE_IDS`) or one registered
/// **ahead of emission** (`REGISTERED_NOT_YET_EMITTED`, ADR-0049 S1). The two
/// carve-outs are disjoint, so an id emitted for the first time must leave the
/// not-yet-emitted list — the reconciliation cannot rot silently.
#[test]
fn registry_has_no_unemittable_ids() {
    let emittable: HashSet<&str> = ALL_EMITTABLE_IDS.iter().copied().collect();
    let pending: HashSet<&str> = REGISTERED_NOT_YET_EMITTED.iter().copied().collect();

    // Disjointness: no id may be both emitted and "not yet emitted". When a stage
    // lights up a pending id (adds it to ALL_EMITTABLE_IDS), this forces its
    // removal from REGISTERED_NOT_YET_EMITTED.
    for id in &emittable {
        assert!(
            !pending.contains(id),
            "id `{id}` is in both ALL_EMITTABLE_IDS and REGISTERED_NOT_YET_EMITTED — \
             remove it from the not-yet-emitted list now that it is emitted"
        );
    }

    // Every registered id is accounted for by exactly one carve-out.
    for &(id, _) in DIAGNOSTIC_REGISTRY {
        assert!(
            emittable.contains(id) || pending.contains(id),
            "registered id `{id}` is neither emittable nor registered-ahead-of-emission — \
             either it is dead (drop it), an emit site was added without listing it in \
             ALL_EMITTABLE_IDS, or it should join REGISTERED_NOT_YET_EMITTED"
        );
    }

    // Every not-yet-emitted id must actually be registered (else the list rots).
    for &id in REGISTERED_NOT_YET_EMITTED {
        assert!(
            layer(id).is_some(),
            "REGISTERED_NOT_YET_EMITTED names `{id}`, which is not in DIAGNOSTIC_REGISTRY"
        );
    }

    // Cardinality: registry == emittable + pending (disjoint), so set equality.
    assert_eq!(DIAGNOSTIC_REGISTRY.len(), ALL_EMITTABLE_IDS.len() + REGISTERED_NOT_YET_EMITTED.len());
    let regset: HashSet<&str> = DIAGNOSTIC_REGISTRY.iter().map(|(i, _)| *i).collect();
    assert_eq!(regset.len(), DIAGNOSTIC_REGISTRY.len(), "duplicate id in DIAGNOSTIC_REGISTRY");
    assert_eq!(emittable.len(), ALL_EMITTABLE_IDS.len(), "duplicate id in ALL_EMITTABLE_IDS");
    assert_eq!(pending.len(), REGISTERED_NOT_YET_EMITTED.len(), "duplicate id in REGISTERED_NOT_YET_EMITTED");
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
    // finding-breadth family (ADR-0049): proof layer, except the declared-receiver
    // lane which is contract (the paired-id precedent, ADR-0049 §8).
    assert_eq!(layer(CALL_UNDEFINED_FUNCTION_ID), Some(Layer::Proof));
    assert_eq!(layer(CALL_UNDEFINED_METHOD_ID), Some(Layer::Proof));
    assert_eq!(layer(CLASS_UNDEFINED_ID), Some(Layer::Proof));
    assert_eq!(layer(CALL_TOO_FEW_ARGUMENTS_ID), Some(Layer::Proof));
    assert_eq!(layer(CALL_TOO_MANY_ARGUMENTS_ID), Some(Layer::Proof));
    assert_eq!(layer(CALL_UNKNOWN_NAMED_ARGUMENT_ID), Some(Layer::Proof));
    assert_eq!(layer(OFFSET_MISSING_ID), Some(Layer::Proof));
    assert_eq!(layer(OFFSET_ON_UNSUPPORTED_ID), Some(Layer::Proof));
    assert_eq!(layer(PHPDOC_UNDEFINED_METHOD_ID), Some(Layer::Contract));
}

/// The finding-breadth family lights up stage by stage (ADR-0049). At S2 the
/// flagship `call.undefined-method` is **emitted**; at S3 the offset pair
/// (`offset.missing` / `offset.on-unsupported`) joins it in `ALL_EMITTABLE_IDS`;
/// the remaining six stay registered ahead of emission.
#[test]
fn finding_breadth_ids_light_up_stage_by_stage() {
    let pending: HashSet<&str> = REGISTERED_NOT_YET_EMITTED.iter().copied().collect();
    let emittable: HashSet<&str> = ALL_EMITTABLE_IDS.iter().copied().collect();

    // S2/S3: the flagship and the offset pair are emittable proof-layer ids.
    for id in [CALL_UNDEFINED_METHOD_ID, OFFSET_MISSING_ID, OFFSET_ON_UNSUPPORTED_ID] {
        assert!(emittable.contains(id), "`{id}` must be emittable from its stage (S2/S3)");
        assert!(!pending.contains(id), "`{id}` must have left REGISTERED_NOT_YET_EMITTED");
        assert_eq!(layer(id), Some(Layer::Proof));
    }

    // S5: the userland arity arms (too-few / unknown-named) are emittable proof-layer
    // ids and left the pending list.
    for id in [CALL_TOO_FEW_ARGUMENTS_ID, CALL_UNKNOWN_NAMED_ARGUMENT_ID] {
        assert!(emittable.contains(id), "`{id}` must be emittable from S5");
        assert!(!pending.contains(id), "`{id}` must have left REGISTERED_NOT_YET_EMITTED");
        assert_eq!(layer(id), Some(Layer::Proof));
    }

    // S6: the declared-receiver lane is emittable and left the pending list — a
    // **contract**-layer id (the paired-id precedent), not proof.
    assert!(emittable.contains(PHPDOC_UNDEFINED_METHOD_ID), "S6 must be emittable");
    assert!(!pending.contains(PHPDOC_UNDEFINED_METHOD_ID), "S6 must have left REGISTERED_NOT_YET_EMITTED");
    assert_eq!(layer(PHPDOC_UNDEFINED_METHOD_ID), Some(Layer::Contract));

    // The remaining three are still registered ahead of emission: the two S4
    // existence ids, and the too-many arm (internal targets only, reflect slice M2).
    for id in [CALL_UNDEFINED_FUNCTION_ID, CLASS_UNDEFINED_ID, CALL_TOO_MANY_ARGUMENTS_ID] {
        assert!(pending.contains(id), "`{id}` should be registered-not-yet-emitted");
        assert!(!emittable.contains(id), "`{id}` must not be emittable before its stage");
        assert!(layer(id).is_some(), "`{id}` must be registered with a layer");
    }
    assert_eq!(REGISTERED_NOT_YET_EMITTED.len(), 3);
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

/// The `origin` facet (ADR-0050 §4) is declared by exactly one id in v1 —
/// `throw.undeclared`. The facet is a *registry-declared* axis: no other id
/// declares one, so no other id's findings ever carry a facet key.
#[test]
fn only_throw_undeclared_declares_a_facet() {
    assert_eq!(declared_facet(THROW_UNDECLARED_ID), Some("origin"));
    for &id in ALL_EMITTABLE_IDS {
        if id != THROW_UNDECLARED_ID {
            assert_eq!(declared_facet(id), None, "`{id}` must declare no facet in v1");
        }
    }
}

/// The wire spellings for the `origin` facet's additive JSON field (ADR-0050 §4).
#[test]
fn facet_wire_spellings() {
    assert_eq!(FACET_ORIGIN, "origin");
    assert_eq!(Facet::Origin(Origin::Direct).key(), "origin");
    assert_eq!(Facet::Origin(Origin::Direct).value(), "direct");
    assert_eq!(Facet::Origin(Origin::Propagated).value(), "propagated");
}
