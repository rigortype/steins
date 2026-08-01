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
    CLASS_UNDEFINED_ID, DEBUG_PHPDOC_TYPE_ID, DEBUG_TRACE_ID, DEBUG_TYPE_ID, DEBUG_VAR_DUMP_ID,
    DIAGNOSTIC_IDS,
    DIAGNOSTIC_REGISTRY, EFFECT_ID, EFFECT_LISKOV_ID, FACET_ORIGIN, Facet, Floor, ID, Layer,
    OFFSET_MAYBE_MISSING_ID, OFFSET_UNDECLARED_ID, surface_floor,
    OFFSET_MISSING_ID, OFFSET_ON_UNSUPPORTED_ID, Origin, PARAM_MISMATCH_ID, PHPDOC_PROP_MISMATCH_ID,
    PHPDOC_UNDEFINED_METHOD_ID, PROP_MISMATCH_ID, READONLY_REASSIGNED_ID, REGISTERED_NOT_YET_EMITTED,
    RETURN_ID, RETURN_MISMATCH_ID, SUPPRESS_UNKNOWN_ID, SUPPRESS_UNMATCHED_ID, THROW_LISKOV_ID,
    THROW_UNDECLARED_ID, UNKNOWN_LABEL_ID, declared_facet, layer,
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
    for &(id, ..) in DIAGNOSTIC_REGISTRY {
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
    let regset: HashSet<&str> = DIAGNOSTIC_REGISTRY.iter().map(|(i, ..)| *i).collect();
    assert_eq!(regset.len(), DIAGNOSTIC_REGISTRY.len(), "duplicate id in DIAGNOSTIC_REGISTRY");
    assert_eq!(emittable.len(), ALL_EMITTABLE_IDS.len(), "duplicate id in ALL_EMITTABLE_IDS");
    assert_eq!(pending.len(), REGISTERED_NOT_YET_EMITTED.len(), "duplicate id in REGISTERED_NOT_YET_EMITTED");
}

/// `DIAGNOSTIC_IDS` is a faithful projection of the registry (single source of
/// truth): same ids, same order.
#[test]
fn diagnostic_ids_is_derived_from_registry() {
    let derived: Vec<&str> = DIAGNOSTIC_REGISTRY.iter().map(|(i, ..)| *i).collect();
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
    // dump surface (ADR-0053 §1): the three debug ids carry the debug layer.
    assert_eq!(layer(DEBUG_TYPE_ID), Some(Layer::Debug));
    assert_eq!(layer(DEBUG_PHPDOC_TYPE_ID), Some(Layer::Debug));
    assert_eq!(layer(DEBUG_VAR_DUMP_ID), Some(Layer::Debug));
    // trace annotation (ADR-0074 §4): the docblock spelling of the same question,
    // same layer.
    assert_eq!(layer(DEBUG_TRACE_ID), Some(Layer::Debug));
}

/// The finding-breadth family lights up stage by stage (ADR-0049). At S2 the
/// flagship `call.undefined-method` is **emitted**; S3 adds the offset pair, S4 the
/// two existence ids, S5 the userland arity arms, S6 the declared-receiver lane —
/// leaving only the internal-target too-many arm registered ahead of emission.
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

    // S4: the two existence ids are emittable proof-layer ids and left the pending
    // list (promoted after the zero-FP measurement run + field survey).
    for id in [CALL_UNDEFINED_FUNCTION_ID, CLASS_UNDEFINED_ID] {
        assert!(emittable.contains(id), "`{id}` must be emittable from S4");
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

    // Only ONE finding-breadth id stays registered ahead of emission: the too-many
    // arm (internal targets only — userland too-many runs clean, never a finding —
    // waiting on the reflect slice, M2).
    let too_many = CALL_TOO_MANY_ARGUMENTS_ID;
    assert!(pending.contains(too_many), "`{too_many}` should be registered-not-yet-emitted");
    assert!(!emittable.contains(too_many), "`{too_many}` must not be emittable before its stage");
    assert!(layer(too_many).is_some(), "`{too_many}` must be registered with a layer");
    // One id is registered-not-yet-emitted: the too-many arm. The debug lane's
    // four ids — the three ADR-0053 dump-surface ids and the ADR-0074 trace
    // annotation `debug.trace` — have all lit up (checked in
    // `all_four_debug_ids_emit`).
    assert_eq!(REGISTERED_NOT_YET_EMITTED.len(), 1);
}

/// The debug lane: all four debug ids emit — the ADR-0053 explicit pair
/// (`debug.type` / `debug.phpdoc-type`) at D3, `debug.var-dump` at D4, and the
/// ADR-0074 trace annotation `debug.trace` at issue #94's emit slice. Each carries
/// `Layer::Debug`, is in `ALL_EMITTABLE_IDS` (left `REGISTERED_NOT_YET_EMITTED`), and
/// keeps its pinned kebab-case `debug.*` spelling.
#[test]
fn all_four_debug_ids_emit() {
    let pending: HashSet<&str> = REGISTERED_NOT_YET_EMITTED.iter().copied().collect();
    let emittable: HashSet<&str> = ALL_EMITTABLE_IDS.iter().copied().collect();

    for id in [DEBUG_TYPE_ID, DEBUG_PHPDOC_TYPE_ID, DEBUG_VAR_DUMP_ID, DEBUG_TRACE_ID] {
        assert_eq!(layer(id), Some(Layer::Debug), "`{id}` must be a Debug-layer id");
        assert!(emittable.contains(id), "`{id}` must be emittable (D3/D4/#94)");
        assert!(!pending.contains(id), "`{id}` must have left REGISTERED_NOT_YET_EMITTED");
    }
    // The kebab-case spellings are pinned (ADR-0053 §2 / ADR-0074 §4 / ADR-0022).
    assert_eq!(DEBUG_TYPE_ID, "debug.type");
    assert_eq!(DEBUG_PHPDOC_TYPE_ID, "debug.phpdoc-type");
    assert_eq!(DEBUG_VAR_DUMP_ID, "debug.var-dump");
    // The one bare-word site: the id string names the user-facing vocabulary
    // (Psalm's issue name); internal symbols never use bare `trace` (§4).
    assert_eq!(DEBUG_TRACE_ID, "debug.trace");
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
    // ADR-0053 §4: the debug layer's `--format json` wire spelling.
    assert_eq!(Layer::Debug.as_str(), "debug");
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

// ---------------------------------------------------------------------------
// The `surface_floor` attribute (ADR-0062 A-G10)
// ---------------------------------------------------------------------------

/// The floor column is **total**: every registered id has one, and the floor lookup
/// agrees with the registry row (the same binding `layer()` gets).
#[test]
fn every_registered_id_has_a_surface_floor() {
    for &(id, _, floor) in DIAGNOSTIC_REGISTRY {
        assert_eq!(
            surface_floor(id),
            Some(floor),
            "`{id}`'s floor lookup must agree with its registry row"
        );
    }
    assert_eq!(surface_floor("not.a.real.id"), None);
}

/// **The byte-identity argument, mechanized.** ADR-0062 A-G10 replaced the profile
/// engine's layer-*set* selection with the floor ladder, on the claim that the floor
/// is a faithful unification of what the built-ins already selected. That claim is
/// exactly this rule — proof/mechanics/debug at `default`, contract at `contracts` —
/// and it is checked here for EVERY id rather than argued once in a comment.
///
/// The only admitted exception is the pair ADR-0062 S6 itself introduces: the offset
/// family's strict leg sits in the contract layer at the `strict` floor, which is
/// the whole reason a floor attribute was needed (a layer can now straddle rungs).
#[test]
fn floors_reproduce_the_pre_s6_layer_selection() {
    // The S6 pair, post-triage (2026-07-29 sweep): `offset.undeclared` measured
    // ZERO corpus findings and took A-G10's END-state promotion to `Contracts`;
    // `offset.maybe-missing` stays `Strict` until the assertion-helper
    // discharge lands (its 3 sweep findings were all that one gap).
    let promoted = [(OFFSET_UNDECLARED_ID, Floor::Contracts), (OFFSET_MAYBE_MISSING_ID, Floor::Strict)];
    for &(id, layer_of, floor) in DIAGNOSTIC_REGISTRY {
        if let Some(&(_, expected_floor)) = promoted.iter().find(|(p, _)| *p == id) {
            assert_eq!(layer_of, Layer::Contract, "the strict leg is contract-layer");
            assert_eq!(floor, expected_floor, "`{id}` floor per the 2026-07-29 triage ruling");
            continue;
        }
        let expected = match layer_of {
            // `default` = proof + mechanics; the debug lane displays everywhere.
            Layer::Proof | Layer::Mechanics | Layer::Debug => Floor::Default,
            // `contracts` was the first built-in whose layer set held these.
            Layer::Contract => Floor::Contracts,
        };
        assert_eq!(
            floor, expected,
            "`{id}` ({layer_of:?}) must keep the floor that reproduces its pre-S6 selection"
        );
    }
}

/// The ladder is a total order, smallest-first — what `floor(id) <= rung` relies on.
#[test]
fn the_floor_ladder_is_cumulative() {
    assert!(Floor::Default < Floor::Contracts);
    assert!(Floor::Contracts < Floor::Strict);
    for f in [Floor::Default, Floor::Contracts, Floor::Strict] {
        assert_eq!(Floor::parse(f.as_str()), Some(f), "rung spelling round-trips");
    }
    assert_eq!(Floor::parse("nope"), None);
}
