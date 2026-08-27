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
    ALL_EMITTABLE_IDS, ARRAY_DUPLICATE_KEY_ID, CALL_ON_NULL_ID, CALL_TOO_FEW_ARGUMENTS_ID,
    CALL_TOO_MANY_ARGUMENTS_ID,
    CALL_UNDEFINED_FUNCTION_ID, CALL_UNDEFINED_METHOD_ID, CALL_UNKNOWN_NAMED_ARGUMENT_ID,
    CLASS_UNDEFINED_ID, DEBUG_PHPDOC_TYPE_ID, DEBUG_TRACE_ID, DEBUG_TYPE_ID, DEBUG_VAR_DUMP_ID,
    DIAGNOSTIC_IDS,
    DIAGNOSTIC_REGISTRY, EFFECT_ID, EFFECT_LISKOV_ID, FACET_ORIGIN, Facet, Floor, ID,
    INTEROP_UNKNOWN_LABEL_ID, Layer,
    OFFSET_MAYBE_MISSING_ID, OFFSET_UNDECLARED_ID, surface_floor,
    OFFSET_MISSING_ID, OFFSET_ON_UNSUPPORTED_ID, Origin, PARAM_MISMATCH_ID, PHPDOC_PROP_MISMATCH_ID,
    PHPDOC_UNDEFINED_METHOD_ID, PROP_MISMATCH_ID, READONLY_REASSIGNED_ID, REGISTERED_NOT_YET_EMITTED,
    RETURN_ID, RETURN_MISMATCH_ID, SUPPRESS_UNKNOWN_ID, SUPPRESS_UNMATCHED_ID, THROW_LISKOV_ID,
    THROW_UNDECLARED_ID, TYPE_RETURN_MAYBE_MISSING_ID, UNKNOWN_LABEL_ID, declared_facet, layer,
    // undefined variables (ADR-0078, issue #194)
    VARIABLE_MAYBE_UNDEFINED_ID, VARIABLE_UNDEFINED_ID,
};
// the argument side's possibly pair (ADR-0081 amendment, issue #391)
use steins_infer::{PHPDOC_MAYBE_ARGUMENT_MISMATCH_ID, TYPE_MAYBE_ARGUMENT_MISMATCH_ID};
// the return seam's possibly pair (ADR-0081 amendment, issue #537)
use steins_infer::{PHPDOC_MAYBE_RETURN_MISMATCH_ID, TYPE_MAYBE_RETURN_MISMATCH_ID};
// unset pseudo-type (ADR-0087 §4, issue #396)
use steins_infer::PHPDOC_MAYBE_UNDEFINED_ID;
// member absence (ADR-0078, issue #197)
use steins_infer::{CLASS_CONST_UNDEFINED_ID, PROPERTY_MAYBE_UNDEFINED_ID, PROPERTY_UNDEFINED_ID};
// global constants (ADR-0078, issue #198)
use steins_infer::CONSTANT_UNDEFINED_ID;
// untyped surface (ADR-0078, issue #200)
use steins_infer::UNTYPED_CLASS_CONSTANT_ID;
// the hyphen reservation's diagnostic (ADR-0091 §6, issue #479)
use steins_infer::PHPDOC_UNKNOWN_VOCABULARY_ID;

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
/// either one an emitter actually produces (`ALL_EMITTABLE_IDS`) or registered
/// **ahead of emission** (`REGISTERED_NOT_YET_EMITTED`, ADR-0049 S1), the two
/// carve-outs disjoint, so a newly-emitted id must leave the not-yet-emitted list.
#[test]
fn registry_has_no_unemittable_ids() {
    let emittable: HashSet<&str> = ALL_EMITTABLE_IDS.iter().copied().collect();
    let pending: HashSet<&str> = REGISTERED_NOT_YET_EMITTED.iter().copied().collect();

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
    // The interop stratum's vocabulary check (issue #311) is contract, not the
    // mechanics its attribute-side twin below carries: suppressable and off the
    // default surface by design — the whole reason it's a separate id.
    assert_eq!(layer(INTEROP_UNKNOWN_LABEL_ID), Some(Layer::Contract));
    // mechanics
    assert_eq!(layer(SUPPRESS_UNMATCHED_ID), Some(Layer::Mechanics));
    assert_eq!(layer(SUPPRESS_UNKNOWN_ID), Some(Layer::Mechanics));
    assert_eq!(layer(UNKNOWN_LABEL_ID), Some(Layer::Mechanics));
    // the member-kind port wave's first id (ADR-0078, issue #187).
    assert_eq!(layer(ARRAY_DUPLICATE_KEY_ID), Some(Layer::Mechanics));
    // finding-breadth family (ADR-0049): proof layer, except the declared-receiver
    // lane's Asserted half (contract, paired-id precedent ADR-0049 §8) — its
    // all-Verified half rides `call.undefined-method` under A13, so two rows cover
    // both halves without a third id.
    assert_eq!(layer(CALL_UNDEFINED_FUNCTION_ID), Some(Layer::Proof));
    assert_eq!(layer(CALL_UNDEFINED_METHOD_ID), Some(Layer::Proof));
    assert_eq!(layer(CLASS_UNDEFINED_ID), Some(Layer::Proof));
    // global constants (ADR-0078, issue #198): the family's third existence id,
    // registered at the same layer and floor as the two above.
    assert_eq!(layer(CONSTANT_UNDEFINED_ID), Some(Layer::Proof));
    assert_eq!(surface_floor(CONSTANT_UNDEFINED_ID), Some(Floor::Default));
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

/// The two unknown-label ids are one defect on two strata, enforced by the registry
/// (issue #311): same question — "this label is not in the vocabulary" — asked of a
/// Steins attribute (apparatus rot: unsuppressable, red on every profile) and of an
/// upstream docblock tag (declared debt on an unchecked claim: suppressable, opt-in).
/// Reusing the mechanics id would fail every project with a pre-existing
/// `@phpstan-impure` note on a bare `steins check` — exactly what ADR-0082 refused.
#[test]
fn the_two_unknown_label_ids_sit_on_different_strata() {
    let emittable: HashSet<&str> = ALL_EMITTABLE_IDS.iter().copied().collect();
    assert!(emittable.contains(INTEROP_UNKNOWN_LABEL_ID), "the interop half emits (#311)");
    assert_ne!(UNKNOWN_LABEL_ID, INTEROP_UNKNOWN_LABEL_ID);

    assert_eq!(layer(UNKNOWN_LABEL_ID), Some(Layer::Mechanics));
    assert_eq!(surface_floor(UNKNOWN_LABEL_ID), Some(Floor::Default));
    assert_eq!(layer(INTEROP_UNKNOWN_LABEL_ID), Some(Layer::Contract));
    assert_eq!(
        surface_floor(INTEROP_UNKNOWN_LABEL_ID),
        Some(Floor::Contracts),
        "it rides with the envelope family it keeps honest"
    );
    // The ADR-0022 kebab-case spelling is pinned: it reaches users' baselines.
    assert_eq!(INTEROP_UNKNOWN_LABEL_ID, "effect.interop-unknown-label");
}

/// Finding-breadth registry coverage by ADR-0049 stage.
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

    // S4: emittable proof-layer ids following a zero-FP measurement run and field survey.
    for id in [CALL_UNDEFINED_FUNCTION_ID, CLASS_UNDEFINED_ID] {
        assert!(emittable.contains(id), "`{id}` must be emittable from S4");
        assert!(!pending.contains(id), "`{id}` must have left REGISTERED_NOT_YET_EMITTED");
        assert_eq!(layer(id), Some(Layer::Proof));
    }

    // S5: the userland arity arms (too-few / unknown-named), emittable and off the pending list.
    for id in [CALL_TOO_FEW_ARGUMENTS_ID, CALL_UNKNOWN_NAMED_ARGUMENT_ID] {
        assert!(emittable.contains(id), "`{id}` must be emittable from S5");
        assert!(!pending.contains(id), "`{id}` must have left REGISTERED_NOT_YET_EMITTED");
        assert_eq!(layer(id), Some(Layer::Proof));
    }

    // S6: the declared-receiver lane, emittable and off the pending list. Under
    // ADR-0049 A13 it routes by minimum stratum across TWO already-registered ids —
    // `phpdoc.undefined-method` (Asserted premise), `call.undefined-method` (S2's,
    // all-Verified) — so the registry sees no change: nothing added, renamed or relayered.
    assert!(emittable.contains(PHPDOC_UNDEFINED_METHOD_ID), "S6 must be emittable");
    assert!(!pending.contains(PHPDOC_UNDEFINED_METHOD_ID), "S6 must have left REGISTERED_NOT_YET_EMITTED");
    assert_eq!(layer(PHPDOC_UNDEFINED_METHOD_ID), Some(Layer::Contract));
    // A13's disjointness invariant (§8) is over SITES, not ids: one id may carry two
    // emitters, but one site is never judged by both — pinned in
    // `tests/s6_routing.rs` and `tests/phpdoc_undefined_method.rs`. Registry-side:
    // both ids stay emittable and neither duplicates the other.
    assert!(emittable.contains(CALL_UNDEFINED_METHOD_ID), "the promoted half's id must be emittable");
    assert_ne!(PHPDOC_UNDEFINED_METHOD_ID, CALL_UNDEFINED_METHOD_ID);

    // Only the internal-target too-many arm remains pending; userland too-many
    // measured clean, and internal targets require reflection (M2).
    let too_many = CALL_TOO_MANY_ARGUMENTS_ID;
    assert!(pending.contains(too_many), "`{too_many}` should be registered-not-yet-emitted");
    assert!(!emittable.contains(too_many), "`{too_many}` must not be emittable before its stage");
    assert!(layer(too_many).is_some(), "`{too_many}` must be registered with a layer");

    // member absence (ADR-0078, issue #197)
    // The `maybe-` sibling convention (ADR-0078 §1.3) mechanized: `property.undefined`
    // ships, so its possibly-grade twin is REGISTERED with it and emitted by nothing —
    // the enforcement of "the possibly-leg is named, never scoped out of existence".
    for id in [PROPERTY_UNDEFINED_ID, CLASS_CONST_UNDEFINED_ID] {
        assert!(emittable.contains(id), "`{id}` must be emittable from its slice (#197)");
        assert!(!pending.contains(id), "`{id}` must not be registered-not-yet-emitted");
        assert_eq!(layer(id), Some(Layer::Proof));
        assert_eq!(surface_floor(id), Some(Floor::Default));
    }
    assert!(
        emittable.contains(PROPERTY_MAYBE_UNDEFINED_ID),
        "the maybe- sibling emits since the declared-shape possibly leg (#267)"
    );
    assert!(
        !pending.contains(PROPERTY_MAYBE_UNDEFINED_ID),
        "the maybe- sibling left the registered-ahead-of-emission list"
    );
    assert_eq!(layer(PROPERTY_MAYBE_UNDEFINED_ID), Some(Layer::Proof));
    assert_eq!(surface_floor(PROPERTY_MAYBE_UNDEFINED_ID), Some(Floor::Strict));

    assert_eq!(REGISTERED_NOT_YET_EMITTED.len(), 1);
}

// undefined variables (ADR-0078, issue #194)

/// The `variable.*` pair: `variable.undefined` proves the binding absent from the
/// whole scope and emits at `default`; `variable.maybe-undefined` claims only that
/// *a* path reaches the read unbound (issue #199, answered by the binding-presence
/// pass ADR-0081/#267) and emits at `strict` — the weaker claim never reaches default.
#[test]
fn the_variable_pair_splits_across_the_two_floors() {
    let emittable: HashSet<&str> = ALL_EMITTABLE_IDS.iter().copied().collect();
    let pending: HashSet<&str> = REGISTERED_NOT_YET_EMITTED.iter().copied().collect();

    assert!(emittable.contains(VARIABLE_UNDEFINED_ID), "the proven arm emits");
    assert!(
        !pending.contains(VARIABLE_UNDEFINED_ID),
        "`{VARIABLE_UNDEFINED_ID}` must not be registered-ahead-of-emission"
    );
    assert_eq!(layer(VARIABLE_UNDEFINED_ID), Some(Layer::Proof));
    assert_eq!(surface_floor(VARIABLE_UNDEFINED_ID), Some(Floor::Default));

    assert!(
        emittable.contains(VARIABLE_MAYBE_UNDEFINED_ID),
        "`{VARIABLE_MAYBE_UNDEFINED_ID}` emits since the binding-presence pass (#267)"
    );
    assert!(
        !pending.contains(VARIABLE_MAYBE_UNDEFINED_ID),
        "`{VARIABLE_MAYBE_UNDEFINED_ID}` left the registered-ahead-of-emission list"
    );
    assert_eq!(layer(VARIABLE_MAYBE_UNDEFINED_ID), Some(Layer::Proof));
    assert_eq!(
        surface_floor(VARIABLE_MAYBE_UNDEFINED_ID),
        Some(Floor::Strict),
        "the weaker claim is opt-in"
    );

    // Disjointness for this pair specifically: same defect, different strengths —
    // exactly the shape where a double-registration would go unnoticed.
    assert!(emittable.is_disjoint(&pending));
    assert_ne!(VARIABLE_UNDEFINED_ID, VARIABLE_MAYBE_UNDEFINED_ID);

    // The ADR-0022 kebab-case spellings are pinned: they reach users' baselines.
    assert_eq!(VARIABLE_UNDEFINED_ID, "variable.undefined");
    assert_eq!(VARIABLE_MAYBE_UNDEFINED_ID, "variable.maybe-undefined");
}

// unset pseudo-type (ADR-0087 §4, issue #396)

/// The declared possibly-undefined read is a **third** id beside the `variable.*`
/// pair, and its registry row is where the reason is pinned: it asks the same
/// question about the same defect from a different premise, and a premise is what
/// the layer answers to.
#[test]
fn the_declared_possibly_undefined_read_is_a_contract_id() {
    let emittable: HashSet<&str> = ALL_EMITTABLE_IDS.iter().copied().collect();
    let pending: HashSet<&str> = REGISTERED_NOT_YET_EMITTED.iter().copied().collect();

    assert!(emittable.contains(PHPDOC_MAYBE_UNDEFINED_ID), "the emitter landed with issue #396");
    assert!(!pending.contains(PHPDOC_MAYBE_UNDEFINED_ID));

    // The premise is the author's own `@var T|unset`, unverifiable by definition, so
    // ADR-0052 §5 forbids it behind a `type.*`/proof-layer id — the whole reason this
    // is not `variable.maybe-undefined`, which is `Layer::Proof`.
    assert_eq!(layer(PHPDOC_MAYBE_UNDEFINED_ID), Some(Layer::Contract));
    // …and the `phpdoc.*` family floor, not the possibly grade's `Strict`. `Strict`
    // answers uncertainty about the premise (`offset.maybe-missing`,
    // `phpdoc.maybe-argument-mismatch`); a declaration has none, and the read
    // contradicts it exactly as `phpdoc.param-mismatch`'s subject does.
    assert_eq!(surface_floor(PHPDOC_MAYBE_UNDEFINED_ID), Some(Floor::Contracts));
    assert_eq!(surface_floor(PARAM_MISMATCH_ID), Some(Floor::Contracts));

    // Three ids, one defect, three premises — all distinct spellings, since every one
    // of them reaches a user's baseline file.
    assert_ne!(PHPDOC_MAYBE_UNDEFINED_ID, VARIABLE_MAYBE_UNDEFINED_ID);
    assert_ne!(PHPDOC_MAYBE_UNDEFINED_ID, VARIABLE_UNDEFINED_ID);
    assert_eq!(PHPDOC_MAYBE_UNDEFINED_ID, "phpdoc.maybe-undefined");
}

// end unset pseudo-type (ADR-0087 §4, issue #396)

/// The hyphen reservation's diagnostic (ADR-0091 §6, issue #479) registers with
/// an emitter behind it, on the contract family's **own** floor.
///
/// §6 refused to fix the floor in advance — the FP source is precise
/// (vocabulary from tools Steins does not model) and only a measurement can
/// place it. The measurement came back with that source **absent everywhere it
/// is measurable**: zero over the pinned public corpus across 2,903 hyphenated
/// type-position sites, and one hit on the private corpus that was a
/// misspelling of known vocabulary — a true positive of the class the id exists
/// for. This slice's review (2026-08-27) therefore put it at `Contracts` rather than
/// behind a rung nobody reaches, which is what `floors_reproduce_the_pre_s6_
/// layer_selection` now covers with no exception row.
#[test]
fn the_unknown_vocabulary_id_sits_on_the_measured_family_floor() {
    assert_eq!(PHPDOC_UNKNOWN_VOCABULARY_ID, "phpdoc.unknown-vocabulary");
    assert_eq!(layer(PHPDOC_UNKNOWN_VOCABULARY_ID), Some(Layer::Contract));
    assert_eq!(
        surface_floor(PHPDOC_UNKNOWN_VOCABULARY_ID),
        Some(Floor::Contracts),
        "the ruled floor: `contracts`, from measurement, not `pedantic`",
    );
    // The definite `phpdoc.*` siblings' floor, exactly — the question this id
    // asks is definite too, so it must not drift onto the possibly grade.
    assert_eq!(
        surface_floor(PHPDOC_UNKNOWN_VOCABULARY_ID),
        surface_floor(PARAM_MISMATCH_ID),
    );

    let emittable: HashSet<&str> = ALL_EMITTABLE_IDS.iter().copied().collect();
    let pending: HashSet<&str> = REGISTERED_NOT_YET_EMITTED.iter().copied().collect();
    assert!(emittable.contains(PHPDOC_UNKNOWN_VOCABULARY_ID), "the emitter landed with the id");
    assert!(!pending.contains(PHPDOC_UNKNOWN_VOCABULARY_ID));

    // `phpdoc.*` spans two layers (ADR-0078 §1.5). This one is contract, not
    // the hygiene family's mechanics: the premise is a docblock's own spelling
    // and nothing about the run is rotten, so it must never be red-on-sight.
    assert_ne!(layer(PHPDOC_UNKNOWN_VOCABULARY_ID), Some(Layer::Mechanics));
}

/// The registered-ahead-of-emission list holds exactly the one id argued above and
/// nothing else — the cardinality guard that makes a forgotten emitter visible.
#[test]
fn exactly_one_id_is_registered_ahead_of_emission() {
    let pending: HashSet<&str> = REGISTERED_NOT_YET_EMITTED.iter().copied().collect();
    assert_eq!(
        pending,
        HashSet::from([CALL_TOO_MANY_ARGUMENTS_ID]),
        "REGISTERED_NOT_YET_EMITTED drifted — every entry needs an argued reason"
    );
}

/// All four debug ids emit at `Layer::Debug`, appear in `ALL_EMITTABLE_IDS`, and
/// retain their ADR-0053/ADR-0074 kebab-case spellings.
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

// The `surface_floor` attribute (ADR-0062 A-G10)

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
/// is a faithful unification of what the built-ins already selected: proof/mechanics/
/// debug at `default`, contract at `contracts` — checked here for EVERY id rather
/// than argued once in a comment.
///
/// The admitted exceptions are ids whose floor was set by a **measurement** rather
/// than by their layer (the `promoted` table below); each is argued at its own row
/// in the test body, including the two straddling directions: a `maybe-` sibling
/// that stays proof-layer but opts UP to `strict` (ADR-0078 §1.3), and the offset
/// family's strict leg, which sits in the contract layer instead of `default`.
#[test]
fn floors_reproduce_the_pre_s6_layer_selection() {
    // The S6 pair, post-triage (2026-07-29 sweep): `offset.undeclared` measured ZERO
    // corpus findings and took A-G10's promotion to `Contracts`; `offset.maybe-missing`
    // stays `Strict` until the assertion-helper discharge lands (3 sweep findings were
    // the whole gap). Plus the member-absence `maybe-` sibling (ADR-0078, issue #197).
    // `type.return-maybe-missing` (2026-08-08 triage): the SAME fatal as its definite
    // sibling, so the layer can't differ. The corpus shows the conditional class (a
    // body returning on every taken arm, leaving an uncovered escape edge) dominated
    // by code correct by construction and unprovable — phpstan-src's own `src/` has
    // two such cases and passes its own missing-return rule. Hence `Strict`.
    // `untyped.class-constant` (2026-08-09 ruling): the one untyped-family arm whose
    // missing declaration withholds NO information (a constant's type is pinned either
    // way), so not `Strict` (which asks about a weaker some-paths claim) but
    // `Pedantic` — the rung no built-in reaches, named in `enable`.
    let promoted = [
        (OFFSET_UNDECLARED_ID, Layer::Contract, Floor::Contracts),
        (OFFSET_MAYBE_MISSING_ID, Layer::Contract, Floor::Strict),
        (PROPERTY_MAYBE_UNDEFINED_ID, Layer::Proof, Floor::Strict),
        (VARIABLE_MAYBE_UNDEFINED_ID, Layer::Proof, Floor::Strict),
        (TYPE_RETURN_MAYBE_MISSING_ID, Layer::Proof, Floor::Strict),
        (UNTYPED_CLASS_CONSTANT_ID, Layer::Contract, Floor::Pedantic),
        // The argument side's possibly pair (ADR-0081's 2026-08-16 amendment,
        // issue #391). Both `Strict`, for two different halves of one reason: the
        // claim is partial-path on either premise (some arm of the argument's own
        // type would not bind, never that this call breaks), and the `phpdoc.*`
        // half additionally rides an `Asserted` premise. The contract half takes
        // the `offset.maybe-missing` split rather than the `phpdoc.*` family's
        // `Contracts` — its definite sibling `phpdoc.param-mismatch` keeps
        // `Contracts`, so a `contracts` run keeps its meaning.
        (TYPE_MAYBE_ARGUMENT_MISMATCH_ID, Layer::Proof, Floor::Strict),
        (PHPDOC_MAYBE_ARGUMENT_MISMATCH_ID, Layer::Contract, Floor::Strict),
        // The same judgment at the return seam (issue #537), on the same split for
        // the same two halves of the same reason. `type.return-mismatch` /
        // `phpdoc.return-mismatch` are the definite siblings and keep their floors.
        (TYPE_MAYBE_RETURN_MISMATCH_ID, Layer::Proof, Floor::Strict),
        (PHPDOC_MAYBE_RETURN_MISMATCH_ID, Layer::Contract, Floor::Strict),
        // `phpdoc.unknown-vocabulary` (ADR-0091 §6, issue #479) is deliberately
        // NOT listed here. §6 made its floor a measurement rather than a
        // decision, and this slice's review (2026-08-27) read that measurement
        // and put it on the contract family's own floor — so it is covered by
        // the default expectation below, as an ordinary member of the family,
        // and needs no exception. Its own row is
        // `the_unknown_vocabulary_id_sits_on_the_measured_family_floor`.
    ];
    for &(id, layer_of, floor) in DIAGNOSTIC_REGISTRY {
        if let Some(&(_, expected_layer, expected_floor)) =
            promoted.iter().find(|(p, _, _)| *p == id)
        {
            assert_eq!(layer_of, expected_layer, "`{id}` layer is fixed by its consequence");
            assert_eq!(floor, expected_floor, "`{id}` floor per its triage ruling");
        // The proof-layer opt-in (ADR-0078, issue #194) — the one row where a
        // proof id does NOT sit at `default`, and deliberately so.
        if id == VARIABLE_MAYBE_UNDEFINED_ID {
            assert_eq!(layer_of, Layer::Proof, "the some-paths sibling stays proof-layer");
            assert_eq!(floor, Floor::Strict, "…but opts in at `strict` (issue #199)");
            continue;
        }
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
    assert!(Floor::Strict < Floor::Pedantic);
    for f in [Floor::Default, Floor::Contracts, Floor::Strict, Floor::Pedantic] {
        assert_eq!(Floor::parse(f.as_str()), Some(f), "rung spelling round-trips");
    }
    assert_eq!(Floor::parse("nope"), None);
}
