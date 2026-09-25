//! Builtin return facts (ADR-0056 R1): the reflected envelope seeds the value
//! domain and a curated row refines strictly within it. The admission gate is
//! factored into pure functions so every leg is unit-testable without a sidecar;
//! the declared-return floor (ADR-0069) and the shape-builtin rows live here too.

use std::collections::{HashMap, HashSet};

use steins_catalog::ResourceKind;
use steins_contract::{ContractTy, ResourceState, normalize};
use steins_domain::{Base, Certainty, Fact, Refinement, ShapeFact, Key as VKey, Val};
use steins_syntax::{
    ArgValue, ArrayKey, CallExpr, Callee, ClosureRef, InvalidatedVar, Receiver,
};

use crate::by_value::value_stratum;
use crate::cx::Cx;
use crate::dispatch::BuiltinCallee;
use crate::env::{
    AllocId, ContractArm, HandleState, HeapRes, Known, Store, Stratum, array_literal_fact,
    singleton_fact,
};
use crate::existence::{denotes_global_function, global_function_callee};
use crate::refine::{flatten_arms, refine_declared_arms, seed_shape_fact};
use crate::resource_folds::resource_fold_return_fact;
use crate::walk::WalkCx;
use crate::fold::Folder;
use crate::shape_projection::{
    shape_projection_fact, witnessed_family_fact, witnessed_projection_fact,
};
use crate::transfers::{arg_dispatch_return_fact, declared_arm_known, transfer_arg_known};

// ---------------------------------------------------------------------------
// Builtin return facts (ADR-0056 R1): the reflected envelope seeds the value
// domain; a curated row refines strictly within it. The admission gate of §2 is
// factored into pure functions so every leg is unit-testable without a sidecar.
// ---------------------------------------------------------------------------

/// Combine a reflected return-type string with an optional curated refinement
/// into the value-domain [`Fact`] to seed (ADR-0056 §1–2), or `None` when nothing
/// representable can be seeded.
///
/// * The **reflected envelope** is `return_type` lowered to a single-base fact
///   (`bool`, `int`, `string`, `float`, or their `?T` nullable form). A multi-base
///   union, a non-scalar, or `mixed` is not representable as one [`Fact`] and
///   yields `None` — the union case belongs to the contract-lane arms (§4).
/// * A **curated refinement** (a phpdoc type string like `int<0, max>`) is
///   admitted only when `minor_matches_pin` holds (the A11 pin, §2), it lowers to
///   the SAME base as the envelope, AND the envelope extensionally subsumes it
///   ([`normalize::subsumes`] `== Yes`). Otherwise the envelope stands alone.
///   Curation may narrow within the envelope; it may never widen or cross bases.
pub(crate) fn admit_return_fact(return_type: &str, curated: Option<&str>, minor_matches_pin: bool) -> Option<Fact> {
    let envelope_ty = steins_contract::lower_str(return_type)?;
    let envelope = envelope_fact(&envelope_ty)?;
    // No curated row, or the minor pin fails: the envelope stands alone.
    let Some(curated) = curated.filter(|_| minor_matches_pin) else {
        return Some(envelope);
    };
    // The curated refinement must lower, be extensionally subsumed by the
    // envelope (curated ⊆ reflected, the §1.2 subset check), and share the
    // envelope's base. Any failure keeps the envelope alone (never widens).
    let refined = steins_contract::lower_str(curated).and_then(|cty| {
        if normalize::subsumes(&envelope_ty, &cty).is_yes() {
            contractty_to_fact(&cty).filter(|f| fact_base(f) == fact_base(&envelope))
        } else {
            None
        }
    });
    Some(refined.unwrap_or(envelope))
}

/// The scalar base of a [`General`]/[`Refined`] fact (`None` for the finite
/// layers, which the return-fact path never produces).
///
/// [`General`]: Fact::General
/// [`Refined`]: Fact::Refined
fn fact_base(f: &Fact) -> Option<Base> {
    match f {
        Fact::General { base, .. } | Fact::Refined { base, .. } => Some(*base),
        // A union has no single base — that is what it is for.
        Fact::Union { .. } => None,
        // The array stratum has no scalar base.
        Fact::Singleton(_) | Fact::OneOf(_) | Fact::Shape { .. } => None,
    }
}

/// Lower a reflected envelope [`ContractTy`] to the single-base value-domain
/// [`Fact`] it seeds, or `None` when not a single representable scalar base: a
/// bare `Base(b)` → `General{b}`, a two-member `?T` union → `General{b, nullable}`.
/// Everything else (multi-base unions, non-scalars, `mixed`) yields `None`.
pub(crate) fn envelope_fact(ty: &ContractTy) -> Option<Fact> {
    match ty {
        ContractTy::Base(b) => Some(Fact::General { base: *b, nullable: false }),
        // **Still the nullable pair only, and now that is a decision** (issue
        // #339). Generalising `Fact::Union` here was tried and reverted: the
        // reflected declaration is coarse by construction (`abs` declares
        // `int|float`), while ADR-0069's curated floor carries the sharp row
        // (`int<1, max>|0|float`). The envelope rung sits ABOVE the floor, so a
        // wider envelope *shadows* the sharper row — 13 nsrt rows regressed from
        // `int<0, max>|float` to `int|float` on exactly that path. Widening waits
        // on whether the floor may refine *within* a union envelope (ADR-0061 §2's
        // question for the type rung), its own decision.
        ContractTy::Union(members) if members.len() == 2 && members.iter().any(|m| matches!(m, ContractTy::Null)) => {
            let base = members.iter().find_map(|m| match m {
                ContractTy::Base(b) => Some(*b),
                _ => None,
            })?;
            Some(Fact::General { base, nullable: true })
        }
        _ => None,
    }
}

/// Fold a declared union's members into one [`Fact`] through the domain's join
/// (issue #339), or `None` if any member does not lift.
///
/// `null` is not a member here but a flag: it lowers to `Fact::Singleton(Null)`
/// and the join folds it into `nullable` on the way, which is the same thing the
/// old two-member special case did by hand.
fn union_envelope(members: &[ContractTy]) -> Option<Fact> {
    let mut acc: Option<Fact> = None;
    for m in members {
        let f = match m {
            ContractTy::Null => Fact::Singleton(Val::Null),
            ContractTy::Base(b) => Fact::General { base: *b, nullable: false },
            ContractTy::IntIn(r) => Fact::refined(Base::Int, Refinement::Int(*r), false),
            ContractTy::StrWith(p) => Fact::refined(Base::String, Refinement::Str(*p), false),
            // Anything else — a class, a shape, a callable, a nested union —
            // is not a scalar arm, so the union has no fact form.
            _ => return None,
        };
        acc = Some(match acc {
            None => f,
            Some(prev) => prev.join(&f)?,
        });
    }
    acc
}

/// Lower a curated refinement [`ContractTy`] to a value-domain [`Fact`] (ADR-0056
/// §1.2), or `None` when not a scalar refinement the domain carries: the base
/// layer, the two Refined refinements (`int<lo, hi>`, string predicates), and a
/// two-member `?T` nullable wrapper. Unions past the nullable pair, and
/// non-scalars, yield `None` — the envelope stands alone.
fn contractty_to_fact(ty: &ContractTy) -> Option<Fact> {
    match ty {
        ContractTy::Base(b) => Some(Fact::General { base: *b, nullable: false }),
        ContractTy::IntIn(r) => Some(Fact::refined(Base::Int, Refinement::Int(*r), false)),
        ContractTy::StrWith(p) => Some(Fact::refined(Base::String, Refinement::Str(*p), false)),
        // An all-`StrWith` intersection is one predicate set (issue #240), folded
        // by `steins_contract::inter_str_preds` — the same fold
        // `steins_contract::to_fact` and the arm speller read, never a second one
        // here. Every other `Inter` still returns `None`: the honest floor.
        ContractTy::Inter(members) => steins_contract::inter_str_preds(members)
            .map(|p| Fact::refined(Base::String, Refinement::Str(p), false)),
        // Any scalar union (issue #339), by the same fold the envelope path uses
        // — the nullable pair is now just its two-member case.
        ContractTy::Union(members) => union_envelope(members),
        _ => None,
    }
}

/// Add null admissibility to a single-base fact (the `?T` curated wrapper). `None`
/// for a finite fact (never produced here).
pub(crate) fn fact_with_null(f: &Fact) -> Option<Fact> {
    match f {
        Fact::General { base, .. } => Some(Fact::General { base: *base, nullable: true }),
        Fact::Refined { base, refinement, .. } => Some(Fact::refined(*base, *refinement, true)),
        Fact::Union { arms, .. } => Fact::union(arms.clone(), true),
        // The curated `?T` wrapper is a scalar path; a shape fact refuses
        // rather than acquiring nullability here.
        Fact::Singleton(_) | Fact::OneOf(_) | Fact::Shape { .. } => None,
    }
}

/// The value-domain fact to seed for a call to builtin `name` at a call site
/// (ADR-0056 R1), or `None` when no fact may be seeded. The call must resolve
/// **uniquely to the builtin**: any project user function sharing the simple name
/// shadows (or makes ambiguous) the builtin, so — exactly as [`Cx::try_fold`]
/// does — a simple-name collision refuses (conservative, never an FP). The fact
/// itself, and the sidecar/monkey-patch/pin gating, come from
/// [`Folder::builtin_return_fact`].
pub(crate) fn builtin_call_return_fact(cx: &Cx, folder: &mut dyn Folder, name: &str) -> Option<Fact> {
    if cx.index.has_simple_function(name) {
        return None;
    }
    folder.builtin_return_fact(name)
}

/// The `Known::bound` provenance a floor-seeded fact carries. A constant rather
/// than a literal at each site because two rungs stamp it and a reader comparing
/// them must see one string.
pub(crate) const CATALOG_FLOOR: &str = "declared in the builtin catalog, unverified";

/// The **declared-return floor** (ADR-0069, issues #73/#79): the bottom rung of the
/// return ladder, seeded from `steins_catalog::declared_return` as a declared-contract
/// **arm list**, every arm `Asserted`.
///
/// It fires exactly where [`builtin_call_return_fact`] yielded `None` for this
/// name — which is *per name*, not per run. `--no-php` (and the browser before
/// php-wasm loads) is only the total case; with a live engine the floor still
/// speaks where that engine is **silent** about a name: an extension the analyzing
/// PHP does not load, a builtin with no declared return type. Where the engine
/// answers, the caller never reaches here, so a static row can never outvote the
/// real thing — the consuming engine may not be the pinned one.
///
/// Three gates, and each is the same gate an existing rung already applies:
///
/// 1. **Project shadow wins** — `has_simple_function` refuses exactly as
///    [`builtin_call_return_fact`] does: a project function of the same simple name
///    shadows (or makes ambiguous) the builtin, and the project's own definition is
///    the better answer.
/// 2. **Version discipline** ([`floor_target_admits`]) — the A11-shaped target gate.
/// 3. **The lowering is the declared-return lowering** — `lower_str` →
///    [`flatten_arms`] → [`refine_declared_arms`] against an empty native list,
///    which is byte for byte the path a project function's `@return` takes at a call
///    site ([`fn_return_arms`], issue #60). One lowering, two provenances
///    (ADR-0069 §2). Issue #79 replaced the #73 `envelope_fact` rung with this one
///    rather than stacking a second: a bare base is a trivial arm set, so the
///    envelope case is subsumed, and a `string|false` row now seeds the same way.
///
/// The stratum is not returned: `refine_declared_arms` over an empty native list
/// marks every arm `Asserted`, and the proof layer's all-Verified premise rule then
/// keeps the fact out of every finding by construction. The absence family never
/// comes here at all — existence is a boot-surface fact, and this table answers only
/// about return types.
/// Whether `var`'s contract lane says it holds a **resource and nothing else**
/// (ADR-0056 §8) — the single condition under which the argument families may
/// read that lane.
///
/// Three requirements, each ruling out a specific way of being wrong:
///
/// * **exactly one arm.** `resource|false` straight out of `fopen()` is not a
///   proven resource until the `=== false` guard kills that arm.
/// * **that arm is [`ContractTy::Resource`].** Not a supertype, not an `Opaque`
///   that might contain one. Any declared state: a closed handle is still a
///   resource to every native parameter (`fclose($h); strlen($h)` is the same
///   `TypeError`, probed at 8.5.10).
/// * **`Verified`.** ADR-0052 §3 keeps the contract lane away from the proof
///   layer — a lane arm reaching `Asserted` by any route, including a
///   `@return resource` docblock, does not qualify.
///
/// The lane answers "is this a resource" and nothing about its **state**: that
/// lives on the heap entry the same binding refers to (ADR-0097 §2.3), read by
/// [`proven_resource_state`] — the lock's second clause.
///
/// [`fn_return_arms`]: crate::fn_return_arms
pub(crate) fn store_holds_resource(store: &Store, var: &str) -> bool {
    matches!(
        store.contract_arms(var),
        Some([ContractArm { ty: ContractTy::Resource { .. }, stratum: Stratum::Verified }])
    )
}

/// The proven state of the resource `var` holds (ADR-0097 §2.3): `None` where
/// [`store_holds_resource`]'s lock fails, else the state of the heap resource
/// the binding refers to — the lock's second clause — and
/// [`HandleState::Unknown`] where the lane holds but no heap resource is bound
/// (a lane that reached here by a route other than a producer call, or one
/// whose binding a join or a forgetting dropped).
///
/// Only a producer call allocates a heap resource and only a closing call, an
/// `is_resource` guard or an escape moves its state, so a docblock's
/// `open-resource`/`closed-resource` — `Asserted`, and heap-less — never
/// reaches a state here.
///
/// # `Closed` is a proof; `Open` is not, whoever established it
///
/// `Closed` is durable: nothing reopens a handle, so a `Closed` established
/// anywhere on the path still holds at every later read. Every finding this
/// judgment carries stands on that half — `fclose($h); fread($h, 1);`, a closed
/// handle at `@param open-resource`, the dead branch over `gettype()`.
///
/// `Open` is not durable, and its origin does not change that. What breaks it
/// is not who minted the handle but **what ran afterwards**: a call that closes
/// it while naming nothing, which the walk's escape table
/// ([`resource_call_effects`]) is never even consulted about. So `Open` is
/// answered as `Unknown`, which convicts nothing.
///
/// Three such routes, each probed at 8.5.10 and each a `phpdoc.param-mismatch`
/// on CORRECT php before this answer was widened:
///
/// ```php
/// $d = opendir('/tmp'); closedir();                  // no argument: closes $d
/// $h = fsockopen(…); $s = socket_import_stream($h); socket_close($s);
/// $m = fopen($p, 'r'); $bz = bzopen($m, 'r'); bzclose($bz);
/// ```
///
/// The first closes the most recently opened directory handle — with two open,
/// it takes the later one and leaves the earlier open. The other two hand
/// ownership to a second handle, which the closer then names instead. None of
/// the three mentions the subject, so no site verdict applies and the state
/// stays `Open`.
///
/// A runtime-read `Open` fares no better one call later, which is what forces
/// the rule rather than merely suggesting it — `if (is_resource($d)) {
/// closedir(); … }` convicted on the same channel. `Open` would be a proof only
/// under "no call has run since the state was established", and this walk does
/// not track that.
///
/// # How the absence was probed, and what the probe cannot reach
///
/// A whitelist of producers stood here and vouched for fourteen rows; all three
/// routes above were on it. The probe behind it read each handle after every
/// **other handle in the script** was closed or dropped — sibling drop and
/// close only, which structurally cannot reach a closer that takes no argument
/// or a call that adopts the handle. Two axes were added before retiring it,
/// over `get_defined_functions()['internal']` on the pinned build:
///
/// * every closer-shaped name whose FIRST parameter is optional. One:
///   `closedir()`. That axis is bounded and re-runnable.
/// * every function with a `resource`/`mixed`/untyped parameter that returns a
///   handle or an object — 98 names, the adopters among them being `bzopen`,
///   `socket_import_stream`, `stream_bucket_new`, `dir` and `zip_read`. Probed:
///   `bzopen` and `socket_import_stream` adopt; a stream-context handed to
///   `fopen`/`dir` is not adopted, nor is a stream handed to
///   `stream_bucket_new` or a `proc_open` descriptor spec.
///
/// Both axes see only the extensions THIS build loads, and neither sees a
/// userland stream wrapper — whose methods run under `ftell($h)` and can reach
/// the handle through `global` ([`site_verdict`] records that calibration).
///
/// Shrinking to the survivors rather than retiring was weighed and refused.
/// `stream-context` is the one KIND left standing — php has no operation that
/// closes one — and the only conviction it would buy is a handle at
/// `@param closed-resource`, a contract **no** context can ever satisfy. The
/// three `stream` producers left standing stand on nothing: `tmpfile`,
/// `gzopen` and `bzopen` survive `bzopen` only because it rejects their
/// stream's MODE (`r+b`, and a gz stream), a runtime detail one level below
/// anything this walk models, while `fopen` and `popen` in the same kind fall.
/// A proof resting on that is a false positive waiting for a mode to change.
/// The measurement is recorded here all the same, so that reinstating any
/// `Open` proof starts from it rather than from scratch, and answers all three
/// axes.
///
/// [`resource_call_effects`]: crate::builtin_returns::resource_call_effects
pub(crate) fn proven_resource_state(store: &Store, var: &str) -> Option<HandleState> {
    if !store_holds_resource(store, var) {
        return None;
    }
    Some(match store.res_of(var) {
        Some(r) if r.state == HandleState::Open => HandleState::Unknown,
        Some(r) => r.state,
        None => HandleState::Unknown,
    })
}

/// The **resource-return arms** of a builtin call (ADR-0056 §8): `resource` plus,
/// where the stub declares one, the `false` failure arm — both `Verified` — and
/// the **heap resource** the assignment binds beside them (ADR-0097 §2.3):
/// the row's kind, in the `Open` state, with the producer's name.
///
/// # Why these arms are `Verified` when the declared floor's are not
///
/// ADR-0069's floor is `Asserted` because a `functionMap` row is an unconfirmed
/// claim about the analyzing PHP. These rows cannot disagree with the engine in
/// that way: [`Folder::builtin_resource_return`] admits the row only while this
/// engine declares NO return type for the name. A migrated function
/// (`curl_init` → `CurlHandle|false`) declares one and is refused; a genuine
/// resource producer declares none because the language has no syntax for it.
///
/// The resource arm's declared state is [`ResourceState::Any`]: the lane
/// carries the type, and `Open` is the heap entry's claim — a proof, since the
/// producer returned and nothing has touched the handle yet; the `false` arm,
/// where present, is what the ordinary `=== false` guard subtracts before
/// [`store_holds_resource`] admits the lane at all.
///
/// The project-shadowing check comes first, as for the floor.
pub(crate) fn builtin_resource_arms(
    cx: &Cx,
    folder: &mut dyn Folder,
    name: &str,
) -> Option<(Vec<ContractArm>, HeapRes)> {
    if cx.index.has_simple_function(name) {
        return None;
    }
    let row = folder.builtin_resource_return(name)?;
    let mut arms = vec![ContractArm {
        ty: ContractTy::Resource { state: ResourceState::Any },
        stratum: Stratum::Verified,
    }];
    if row.may_be_false {
        arms.push(ContractArm { ty: ContractTy::LitBool(false), stratum: Stratum::Verified });
    }
    let res = HeapRes {
        kind: row.kind,
        state: HandleState::Open,
        producer: name.trim_start_matches('\\').to_ascii_lowercase(),
    };
    Some((arms, res))
}

// ---------------------------------------------------------------------------
// The array-of-handles producers (ADR-0098 §4 slice 2)
// ---------------------------------------------------------------------------

/// The contract arms an **element place** a producer minted carries
/// (ADR-0098 §2.2): one `resource` arm, `Verified`, and no failure arm.
///
/// The failure arm belongs to the *container*, never to an element: PHP either
/// hands back the array of handles or hands back nothing at all, and when it
/// hands one back every entry in it is an open handle. So the lane that
/// ADR-0056 §8.6's lock reads is already narrowed at the moment the place is
/// bound, which is the difference between a minted place and one
/// [`bind_handle_elements`] copied off a variable that still had to be guarded.
///
/// [`bind_handle_elements`]: crate::assign::bind_handle_elements
pub(crate) fn produced_place_arms() -> Vec<ContractArm> {
    vec![ContractArm {
        ty: ContractTy::Resource { state: ResourceState::Any },
        stratum: Stratum::Verified,
    }]
}

/// The heap resource one produced element holds: a **fresh** allocation, `Open`,
/// of the producer's kind. Fresh and not shared — `$pair[0]` and `$pair[1]` are
/// two different handles with two different ids (probed at 8.5.10:
/// `get_resource_id()` answers 4 and 5 for one pair, and `fclose($pair[0])`
/// leaves `is_resource($pair[1])` true), so one id for both would make closing
/// either close the other.
fn produced_handle(producer: &str, kind: ResourceKind) -> HeapRes {
    HeapRes { kind, state: HandleState::Open, producer: producer.to_ascii_lowercase() }
}

/// `stream_socket_pair`'s name, spelled once.
const SOCKET_PAIR: &str = "stream_socket_pair";

/// The return declaration `stream_socket_pair` must still carry for its row to
/// be admitted — the ADR-0056 §8.2 tripwire in the one shape it can take for a
/// producer whose return type PHP *can* spell.
///
/// `fopen`'s tripwire is silence: the engine declaring anything at all disowns
/// the row. That reading is unavailable here, because `array|false` is exactly
/// what this engine declares and always has. So the tripwire is the declaration
/// itself: the day a PHP hands back `Socket[]|false`, an `iterable`, or a pair
/// object, the string below stops matching and the row switches itself off with
/// no denylist and no release of Steins.
const SOCKET_PAIR_RETURN: &str = "array|false";

/// The places `$pair = stream_socket_pair(…)` binds (ADR-0098 §2.2): **exactly
/// two**, keyed `0` and `1`, each a fresh `Open` `stream` handle.
///
/// Probed at 8.5.10 — the count is the contract and not an observation of one
/// call:
///
/// ```text
/// $p = stream_socket_pair(STREAM_PF_UNIX, STREAM_SOCK_STREAM, 0);
///   gettype($p) === 'array'   array_keys($p) === [0, 1]   count($p) === 2
///   get_debug_type($p[0]) === get_debug_type($p[1]) === 'resource (stream)'
///   get_resource_id($p[0]) === 4   get_resource_id($p[1]) === 5   ($p[0] !== $p[1])
///   array_is_list($p) === true
/// $p = stream_socket_pair(STREAM_PF_UNIX, STREAM_SOCK_DGRAM, 0);   keys [0, 1] too
/// $p = @stream_socket_pair(-1, -1, -1);   === false   (the whole failure shape)
/// fclose($p[0]);   is_resource($p[0]) === false   is_resource($p[1]) === true
/// ```
///
/// The failure arm is `false`, never a shorter array: there is no call that
/// answers a one-element list, so "the array exists" and "both places hold an
/// open handle" are the same statement, and `pair[0]` needs no guard of its own
/// beyond the ordinary `=== false` on `$pair`.
///
/// Two gates, both the ones the `fopen` rung applies: a project function of the
/// same simple name shadows the builtin and answers instead, and the engine's
/// own declaration must still be [`SOCKET_PAIR_RETURN`]. Without a live sidecar
/// there is no declaration to read and nothing is bound — the sound subset
/// (ADR-0004), same as every other resource rung.
pub(crate) fn socket_pair_places(
    cx: &Cx,
    folder: &mut dyn Folder,
    name: &str,
) -> Option<Vec<(VKey, HeapRes)>> {
    if !name.eq_ignore_ascii_case(SOCKET_PAIR) || cx.index.has_simple_function(name) {
        return None;
    }
    let declared = folder.builtin_return_type(name)?;
    if !declared.eq_ignore_ascii_case(SOCKET_PAIR_RETURN) {
        return None;
    }
    Some(vec![
        (VKey::Int(0), produced_handle(SOCKET_PAIR, ResourceKind::Stream)),
        (VKey::Int(1), produced_handle(SOCKET_PAIR, ResourceKind::Stream)),
    ])
}

/// `proc_open`'s name, spelled once.
pub(crate) const PROC_OPEN: &str = "proc_open";

/// One descriptor word `proc_open` accepts, spelled the way php-src compares it
/// — **byte for byte**. `'PIPE'` and `'Pipe'` are not this word (probed at
/// 8.5.10: `proc_open(): PIPE is not a valid descriptor spec/mode`, the call
/// answers `false` and leaves `$pipes` untouched), so a case-insensitive
/// comparison here would mint a place for a call that never writes one — on
/// EVERY execution, not on a failure path.
struct DescriptorWord {
    /// The word itself, lowercase, as php-src's `zend_string_equals_literal`
    /// sees it.
    word: &'static str,
    /// Whether `proc_open` hands back an entry in `$pipes` at this descriptor's
    /// key — and, for the two words where it does but this rung stays out, see
    /// [`proc_open_places`]' "two descriptor words" section.
    binds_place: bool,
    /// The cells **after** `0` the engine demands beside this word. Each absent
    /// one is a `ValueError` raised before `proc_open` returns anything, so a
    /// descriptor missing one never reaches a statement that could read a place.
    /// Probed at 8.5.10, one message per cell: `Missing mode parameter for
    /// 'pipe'`, `Missing file name parameter for 'file'`, `Missing mode
    /// parameter for 'file'`, `Missing redirection target`.
    cells: &'static [i64],
}

/// **Every** descriptor word this rung will read a spec through, and no others.
///
/// A word outside this table refuses the **whole** spec, for the reason `PIPE`
/// does: php-src warns `… is not a valid descriptor spec/mode`, the call answers
/// `false`, and `$pipes` is left exactly as it was found — so no key of that
/// spec is ever written, the readable ones included. `pty` is deliberately
/// outside: it was probed to produce an entry *here*, but it needs a build whose
/// `proc_open` has pseudo-terminal support, and a build without it cannot both
/// refuse the word and write the other keys. Admitting it is a probe on such a
/// build, not a reading of this comment.
const DESCRIPTOR_WORDS: &[DescriptorWord] = &[
    DescriptorWord { word: "pipe", binds_place: true, cells: &[1] },
    DescriptorWord { word: "file", binds_place: false, cells: &[1, 2] },
    DescriptorWord { word: "null", binds_place: false, cells: &[] },
    DescriptorWord { word: "redirect", binds_place: false, cells: &[1] },
    DescriptorWord { word: "socket", binds_place: false, cells: &[] },
];

/// The row for `word`, matched **case-sensitively**, or `None` for a word
/// php-src does not accept — which is a refusal of the whole spec.
fn descriptor_word(word: &[u8]) -> Option<&'static DescriptorWord> {
    DESCRIPTOR_WORDS.iter().find(|d| d.word.as_bytes() == word)
}

/// The places `proc_open($cmd, $spec, $pipes, …)` binds (ADR-0098 §2.2): **one
/// per `pipe` descriptor of `$spec`, at that descriptor's own key** — never one
/// per position and never one per argument.
///
/// The key set is a function of the spec, which is the whole reason this cannot
/// be a fixed row. Probed at 8.5.10:
///
/// ```text
/// [0 => ['pipe','r'], 2 => ['pipe','w']]            array_keys($pipes) === [0, 2]
/// [0 => ['file','/dev/null','r'], 1 => ['pipe','w']]                    === [1]
/// [0 => ['pipe','r'], 1 => ['file','/dev/null','w']]                    === [0]
/// [0 => ['pipe','r'], 5 => ['pipe','w']]                                === [0, 5]
/// [3 => ['pipe','r']]                                                   === [3]
/// [['pipe','r'], ['pipe','w']]                                          === [0, 1]
/// [100 => ['pipe','r']]                                                 === [100]
/// [1 => ['null']]                                                       === []
/// [1 => ['pipe','w'], 2 => ['redirect', 1]]                             === [1]
/// [1 => $fh]           (a stream resource as the descriptor)            === []
/// []                                                                    === []
/// ```
///
/// Every entry is `resource (stream)` — `process` is the *return's* kind, not
/// the pipes' — and each is closed by `fclose`, after which the element keeps
/// holding the closed handle (`gettype($pipes[1])` reads `resource (closed)`).
///
/// # Two descriptor words that also produce an entry, and stay out anyway
///
/// `['socket']` and `['pty']` were probed here too, and both yield an entry
/// (`[1 => ['socket']]` → key `1`; `[0 => ['pty'], 1 => ['pty']]` → keys `0` and
/// `1`). Neither is admitted, and they are held back differently because PHP
/// treats them differently. `socket` is a word php-src accepts on any build, so
/// a spec that uses one stays readable and the key it fills is simply not
/// claimed — a missed finding, which is where the family already is. `pty`
/// needs a build whose `proc_open` has pseudo-terminal support, which this probe
/// cannot speak for, so it is outside [`DESCRIPTOR_WORDS`] and refuses the whole
/// spec: on a build that lacks the support the call cannot succeed, and the
/// other keys of that same spec would be places nothing ever wrote.
///
/// # What is checked, in the order it is checked
///
/// 1. the spec resolves to a **literal array** — else nothing is bound;
/// 2. every key is an **integer**. A string key is `ValueError: proc_open():
///    Argument #2 ($descriptor_spec) must be an integer indexed array` (probed),
///    so the call raises before it returns and no statement after it runs;
/// 3. every key is **non-negative**. Probed at 8.5.10, for every descriptor word
///    alike: `[-1 => ['pipe','r']]` warns `Unable to copy file descriptor 5 (for
///    pipe) into file descriptor -1: Bad file descriptor`, answers `false` and
///    leaves `$pipes` untouched — on every execution, so a place minted from
///    such a spec could never be read;
/// 4. every descriptor is a **literal array** whose cell `0` is a **literal
///    string** — a word this walk cannot name is not a non-`pipe`;
/// 5. that word is one [`DESCRIPTOR_WORDS`] carries, compared **byte for
///    byte** — the one php-src compares. Anything else is the `PIPE` case: a
///    warning, `false`, and an untouched `$pipes`;
/// 6. the cells php-src demands beside the word are **present**
///    ([`DescriptorWord::cells`]) — `['pipe']` with no mode is `ValueError:
///    Missing mode parameter for 'pipe'` (probed), which returns nothing at all.
///    Only presence is checked: the mode's *value* is not validated by the
///    engine either (probed: `['pipe','zzz']` and `['pipe', 5]` both open a
///    pipe).
///
/// # And every refusal is refused whole
///
/// A spec the walk cannot prove is a spec whose key set is unknown, and an
/// unknown key set cannot be bound *in part*: the unprovable entry may itself be
/// a `pipe`, so binding the provable ones would claim `$pipes` has exactly the
/// keys this returned — a claim about the entries as a set, which is the thing
/// that was not proven. Legs 2, 3, 5 and 6 refuse whole for a second reason on
/// top of that one: each of them is a spec on which `proc_open` writes
/// **nothing**, every time it runs, so a place bound beside it would be a
/// finding on a line that is never reached.
pub(crate) fn proc_open_places(
    cx: &Cx,
    folder: &mut dyn Folder,
    name: &str,
    spec: &ArgValue,
    env: &HashMap<String, Known>,
    poisoned: bool,
) -> Option<Vec<(VKey, HeapRes)>> {
    if !name.eq_ignore_ascii_case(PROC_OPEN) || cx.index.has_simple_function(name) {
        return None;
    }
    // The producer row's own gate (ADR-0056 §8.2), which `proc_open` is already
    // in: the engine has the name, declares no return type for it, and the
    // project minor is the catalog pin. It answers about the *return*, and it
    // is the right gate for the pipes too — a `proc_open` migrated to an object
    // is a `proc_open` whose `$pipes` this probe no longer speaks for.
    folder.builtin_resource_return(name)?;
    // The by-reference tripwire: position 2 is where the handles land, and a
    // position the engine reports by value is one this call cannot write.
    let params = folder.builtin_param_types(name)?;
    if !params.get(PROC_OPEN_PIPES).is_some_and(|p| p.by_ref) {
        return None;
    }
    let Some(ArgValue::Array(items)) = cx.resolve_literal(spec, env, poisoned, folder) else {
        return None;
    };
    let normalized = steins_syntax::normalize_array(&items, cx.php_minor)?;
    let mut places = Vec::new();
    for (key, descriptor) in normalized {
        // Leg 2: a string key is the `ValueError` above; an integer key can be a
        // place. Leg 3: a negative one never is — the call fails before writing.
        let steins_syntax::NormKey::Int(index) = key else { return None };
        if index < 0 {
            return None;
        }
        // Leg 4. The descriptor's word is its element `0`. A descriptor whose
        // first cell is not a literal string is one whose word is unknown — not
        // a non-`pipe`, which is why it refuses the whole spec rather than
        // contributing nothing.
        let ArgValue::Array(cells) = descriptor else { return None };
        let cells = steins_syntax::normalize_array(&cells, cx.php_minor)?;
        let word = match cells.iter().find(|(k, _)| *k == steins_syntax::NormKey::Int(0)) {
            Some((_, ArgValue::Str(s))) => s.clone(),
            // No cell `0` at all is a `ValueError` out of the engine
            // (`Missing handle qualifier in array`), so it never reaches a
            // statement that could read a place; refused here regardless.
            _ => return None,
        };
        // Leg 5, byte for byte, and leg 6 beside it.
        let row = descriptor_word(word.as_bytes())?;
        let present =
            |cell: &i64| cells.iter().any(|(k, _)| *k == steins_syntax::NormKey::Int(*cell));
        if !row.cells.iter().all(present) {
            return None;
        }
        if row.binds_place {
            places.push((VKey::Int(index), produced_handle(PROC_OPEN, ResourceKind::Stream)));
        }
    }
    Some(places)
}

/// The 0-based position of `proc_open`'s `&$pipes` — the same index the
/// catalog's [`steins_catalog::out_params`] row carries, spelled here because
/// the two tripwires above read it directly.
pub(crate) const PROC_OPEN_PIPES: usize = 2;

/// Bind `places` as element places of `var` (ADR-0098 §2.2), each to an
/// allocation **this walk mints**.
///
/// The producer twin of [`bind_handle_elements`], and the one line of it that
/// differs is the id: a literal shares the id the source variable holds, a
/// producer has no source to share with and every element is its own handle. So
/// each place gets a [`WalkCx::fresh_id`] of its own, and closing one leaves the
/// others exactly where they were — which is what PHP does (probed at 8.5.10:
/// `fclose($pair[0])` leaves `is_resource($pair[1])` true).
///
/// The caller has already dropped `var`'s previous places; this only adds.
///
/// [`bind_handle_elements`]: crate::assign::bind_handle_elements
pub(crate) fn bind_produced_places(
    w: &WalkCx,
    var: &str,
    places: Vec<(VKey, HeapRes)>,
    store: &mut Store,
) {
    for (key, res) in places {
        let place = crate::env::elem_place(var, &key);
        store.bind_resource(&place, w.fresh_id(), res);
        store.contract.insert(place, produced_place_arms());
    }
}

/// The **closing calls** (ADR-0097 §2.4; CONTEXT.md "Closing call") and, per
/// call, the kinds of handle it leaves **closed** when it returns — each cell
/// probed at 8.5.10 by `gettype()` reading `"resource (closed)"` after the call.
///
/// A return is the whole premise: every argument a closing call rejects — a
/// non-resource, an already-closed handle, a handle of a kind the call has no
/// row for — is a `TypeError`, so the statement after it runs only when the
/// close happened. That is why the table lists what a *normal* return closes
/// and nothing else, and why the one kind-sensitive cell is a keeper rather
/// than an error: `fclose`, `gzclose` and `bzclose` over an `opendir()` handle
/// **warn, return `false` and leave it open** (`cannot close the provided
/// stream, as it must not be manually closed`), so a `dir` is absent from their
/// rows and the handle keeps its state. `pclose` closes a `dir` (and any plain
/// stream); `closedir` and `proc_close` reject everything but their own kind.
///
/// A persistent stream (`pfsockopen`) is closed by `fclose`, `gzclose`,
/// `bzclose` and `pclose` alike: the handle reads `resource (closed)` and
/// `is_resource` says `false`, while the connection lives on for the next
/// `pfsockopen()` to hand out under a new id. A `stream-filter` has one closer
/// of its own, `stream_filter_remove` (probed at 8.5.10: `gettype()` reads
/// `resource (closed)` after it, and the call throws on a stream, a context
/// and an already-removed filter alike); the `stream-context` kind appears in
/// no row, since every closer throws on it.
const CLOSERS: &[(&str, &[ResourceKind])] = &[
    ("fclose", &[ResourceKind::Stream, ResourceKind::PersistentStream]),
    ("gzclose", &[ResourceKind::Stream, ResourceKind::PersistentStream]),
    ("bzclose", &[ResourceKind::Stream, ResourceKind::PersistentStream]),
    ("pclose", &[ResourceKind::Stream, ResourceKind::PersistentStream, ResourceKind::Dir]),
    ("closedir", &[ResourceKind::Dir]),
    ("proc_close", &[ResourceKind::Process]),
    ("stream_filter_remove", &[ResourceKind::StreamFilter]),
];

/// What one **direct argument position** of a global builtin does to the heap
/// resource handed to it (ADR-0097 §2.4's table, one row per verdict).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SiteVerdict {
    /// A closing call whose row closes this kind: the state becomes `Closed`.
    Close,
    /// A **keeper**: a closing call that returns from this kind without closing
    /// it, or a builtin whose reflected parameter here is by value and that is
    /// not a closing call — a builtin cannot close a handle it merely reads.
    /// The state is unchanged and the binding survives the call.
    Keep,
    /// Everything else: the handle escapes and its state goes `Unknown`.
    Escape,
}

/// [`SiteVerdict`] for the global builtin `name` (already resolved through
/// [`denotes_global_function`], so a project shadow is excluded) receiving a
/// handle of `kind` at `position`.
///
/// The closing table is consulted **first** and by name alone: its rows were
/// probed by hand and every one takes its handle by value, so it needs no
/// reflection — which is also what keeps the proof of `Closed` available on an
/// engine whose replay table predates the parameter reply. A name outside the
/// table is a keeper only on the engine's own word: the reflected parameter at
/// `position` exists, is not variadic, and is not by reference. A position past
/// the declared list, a variadic one, a by-reference one, and a name the engine
/// reflects no signature for are all escapes — the silent direction.
///
/// The one userland route a by-value stream position leaves open is a user
/// stream wrapper (`stream_wrapper_register`), whose `stream_tell()` runs on
/// `ftell($h)`; that its body could reach `$h` through `global` and close it is
/// accepted as this slice's calibration, exactly as ADR-0070's by-value rule
/// accepts a callback's `global` inside a function frame.
fn site_verdict(
    folder: &mut dyn Folder,
    name: &str,
    position: usize,
    kind: ResourceKind,
) -> SiteVerdict {
    if let Some((_, kinds)) = CLOSERS.iter().find(|(n, _)| name.eq_ignore_ascii_case(n)) {
        return if kinds.contains(&kind) { SiteVerdict::Close } else { SiteVerdict::Keep };
    }
    let Some(params) = folder.builtin_param_types(name) else { return SiteVerdict::Escape };
    match params.get(position) {
        Some(p) if !p.variadic && !p.by_ref => SiteVerdict::Keep,
        _ => SiteVerdict::Escape,
    }
}

/// Whether `call`, in the frame being walked, could **rebind a global name** to
/// a different handle — in which case the state this walk proved belongs to the
/// old handle and says nothing about what the name holds next.
///
/// [`HandleState::escaped`] keeps `Closed` across an escape on the ground that
/// nothing reopens a handle. That is true of the *handle* and false of the
/// *name*, and [`Store::res_of`] is keyed on the name. In the **top-level
/// frame** the locals are the globals, so any userland body that runs can
/// rebind one through `global $h` or `$GLOBALS['h']` while the call site
/// mentions nothing — no argument, no receiver, so §2.4's escape table is never
/// even consulted. Probed at 8.5.10, this exits 0:
///
/// ```php
/// function bump(): void { global $h; $h = fopen('php://memory', 'r'); }
/// $h = fopen('php://memory', 'r');
/// if ($h === false) { throw new \RuntimeException('x'); }
/// fclose($h);
/// bump();
/// fread($h, 1); // a fresh OPEN handle: no TypeError
/// ```
///
/// So at top level such a call **forgets the state** of every heap resource the
/// frame already held — `Unknown`, which convicts nothing (§2.4) — rather than
/// leaving a `Closed` the name no longer answers for. Forgetting is strictly
/// weaker than an escape: it drops a proof, never adds one.
///
/// Narrow in three ways, so the ordinary conviction survives:
///
/// * **Top-level frame only** ([`frame_is_top_level`], the carrier issue #637's
///   review already installed for this exact hazard). Inside a function body
///   `$h` is a local the callee cannot see; the one route in is the analyzed
///   scope's own `global $h`, which voids the binding on its own.
/// * **Calls the walk cannot resolve to an engine builtin only.** A global
///   builtin the engine reflects has no `global` statement in it, so a closing
///   call and a keeper are both left alone and `fclose($h); fread($h, 1);` at
///   file scope still convicts. A project function, a method, a constructor, a
///   dynamic callee and a global name the engine does not know are all opaque.
/// * **The state only.** The binding, the type lane and the identity stay; only
///   the one fact a rebind would invalidate is dropped.
///
/// What this does not close is a builtin that runs userland behind its own
/// signature — a callback under `usort`, a user stream wrapper's `stream_tell`
/// under `ftell` — which could `global $h` from there. That is the calibration
/// [`site_verdict`] already records for the wrapper, unchanged here. The same
/// blind spot on the *value* lane (`$s = 'abc'; bump(); intdiv($s, 1);`)
/// predates this slice and is not this function's business.
///
/// [`frame_is_top_level`]: crate::walk::frame_is_top_level
fn top_level_rebind_risk(cx: &Cx, folder: &mut dyn Folder, call: &CallExpr) -> bool {
    if !crate::walk::frame_is_top_level() {
        return false;
    }
    let Some(name) = global_function_callee(cx, call) else { return true };
    // The closing table needs no reflection (its rows were probed by hand), so
    // it answers first, exactly as `site_verdict` reads it.
    !CLOSERS.iter().any(|(n, _)| name.eq_ignore_ascii_case(n))
        && folder.builtin_param_types(name).is_none()
}

/// What a statement's calls do to the heap resources their arguments name
/// (ADR-0097 §2.4), read on the pre-call store — where every binding the
/// statement is about to forget still resolves — and applied by
/// [`apply_resource_effects`] after the statement's own forgetting, so that
/// a closed handle stays closed for every name that shares it, `$h =
/// fclose($h)` included.
pub(crate) struct ResourceEffects {
    /// The heap resources this statement closed (`true`) or let escape
    /// (`false`), by allocation id — a binding the statement drops cannot be
    /// read back by name, and the entry outlives the name for its aliases.
    transitions: Vec<(AllocId, bool)>,
    /// The heap resources whose **state** this statement forgot without
    /// escaping them: at top level, everything the frame already held when a
    /// call that could rebind a global ran ([`top_level_rebind_risk`]).
    /// Pre-existing by construction — an entry the statement itself allocated
    /// is a handle no call before it could have been handed.
    forgotten: Vec<AllocId>,
    /// The variables whose **binding survives** the statement's conservative
    /// forgetting: every occurrence of the name in the statement's call
    /// arguments is a keeper or a closing call, so nothing the statement did
    /// could have rebound the variable — only the heap state may have changed.
    pub(crate) kept: HashSet<String>,
}

/// Compute [`ResourceEffects`] for `calls` — a statement's
/// [`checkable_calls`] or a guard's retained calls — on the pre-call `store`.
///
/// Per call: each **direct** positional `$v` argument bound to a heap resource
/// takes [`site_verdict`] for the call's global builtin, and escapes for any
/// other callee (a project function, a method, a constructor, a dynamic or
/// unresolvable name, a call with named or spread arguments). A handle named
/// anywhere **inside** an argument — an array literal, a nested call's
/// argument, a closure's capture — and one in receiver position escapes
/// unconditionally: nothing here reads through a nested position, and the
/// silent direction is the only sound one there.
///
/// `invalidated` is the statement's own completeness oracle for `kept`
/// ([`Stmt::invalidated`], every occurrence recorded or the entry marked
/// opaque); a guard position passes none and keeps nothing here — its lane
/// survival stays with the guard machinery ([`by_value_survivors`],
/// [`type_predicate`]) as before.
///
/// [`checkable_calls`]: crate::descent::checkable_calls
/// [`Stmt::invalidated`]: steins_syntax::Stmt::invalidated
/// [`by_value_survivors`]: crate::walk::by_value_survivors
/// [`type_predicate`]: crate::predicates::type_predicate
pub(crate) fn resource_call_effects(
    cx: &Cx,
    folder: &mut dyn Folder,
    poisoned: bool,
    calls: &[&CallExpr],
    invalidated: Option<&[InvalidatedVar]>,
    store: &Store,
) -> ResourceEffects {
    let mut effects =
        ResourceEffects { transitions: Vec::new(), forgotten: Vec::new(), kept: HashSet::new() };
    if poisoned || calls.is_empty() {
        return effects;
    }
    // Nothing to forget where the frame holds no handle — and asking is not free
    // (it reflects the callee), so the emptiness check comes first.
    if !store.resources.is_empty()
        && calls.iter().any(|call| top_level_rebind_risk(cx, folder, call))
    {
        effects.forgotten.extend(store.resources.keys().copied());
    }
    let mut escaped_mentions: Vec<&str> = Vec::new();
    for call in calls {
        let builtin = global_function_callee(cx, call).filter(|_| call.positional_only);
        for (position, arg) in call.args.iter().enumerate() {
            match &arg.value {
                // A place, not only a variable (ADR-0098 §2.4): `fclose($pipes[0])`
                // closes the entry the element names, by the same id transition a
                // bare `$h` takes. An offset whose key is not literal names no
                // place and falls to the escape leg below, as it does today.
                value if crate::offsets::place_of_static(cx, value)
                    .is_some_and(|p| store.res_of(&p).is_some()) =>
                {
                    let place = crate::offsets::place_of_static(cx, value)
                        .expect("the guard just resolved one");
                    let res = store.res_of(&place).expect("the guard just found one");
                    let id = store.id_of(&place).expect("a bound resource has an id");
                    let verdict = match builtin {
                        Some(name) => site_verdict(folder, name, position, res.kind),
                        None => SiteVerdict::Escape,
                    };
                    match verdict {
                        SiteVerdict::Close => effects.transitions.push((id, true)),
                        SiteVerdict::Keep => {}
                        SiteVerdict::Escape => effects.transitions.push((id, false)),
                    }
                }
                other => mentioned_vars(other, &mut escaped_mentions),
            }
        }
        for named in &call.named_args {
            mentioned_vars(&named.value, &mut escaped_mentions);
        }
        match &call.receiver {
            Callee::Method { receiver: Receiver::Var(v), .. } => escaped_mentions.push(v),
            Callee::Method { receiver: Receiver::New { args, named, .. }, .. } => {
                for value in args.iter().chain(named.iter().map(|n| &n.value)) {
                    mentioned_vars(value, &mut escaped_mentions);
                }
            }
            _ => {}
        }
    }
    for v in escaped_mentions {
        if let Some(id) = store.id_of(v).filter(|id| store.resources.contains_key(id)) {
            effects.transitions.push((id, false));
        }
    }
    // The survivors: a resource-bound name every recorded site of which is a
    // keeper or a closing call of a global builtin, with no opaque occurrence.
    for entry in invalidated.unwrap_or(&[]) {
        if entry.opaque || entry.sites.is_empty() {
            continue;
        }
        // The kinds this name's sites must all be keepers for: the handle it
        // holds itself, or — ADR-0098 §2.3 — the handles its element places
        // hold. `fclose($pipes[0])` records `pipes` as an occurrence, and the
        // array is no more rebound by it than `$h` is by `fclose($h)`; without
        // this leg the base is swept and its places die with it, which is the
        // whole `proc_open` idiom lost one statement in.
        let kinds: Vec<ResourceKind> = match store.res_of(&entry.name) {
            Some(res) => vec![res.kind],
            None => store
                .places_under(&entry.name)
                .iter()
                .filter_map(|(_, id)| store.resources.get(id).map(|r| r.kind))
                .collect(),
        };
        if kinds.is_empty() {
            continue;
        }
        let survives = entry.sites.iter().all(|(r, position)| {
            denotes_global_function(cx, r)
                && kinds.iter().all(|kind| {
                    site_verdict(folder, &r.raw, *position as usize, *kind) != SiteVerdict::Escape
                })
        });
        if survives {
            effects.kept.insert(entry.name.clone());
        }
    }
    effects
}

/// Apply [`resource_call_effects`]' transitions to `store`: a closed handle's
/// entry goes `Closed`; an escaped one takes [`HandleState::escaped`]
/// (`Closed` stays — nothing reopens a handle). By id, so a name the statement
/// rebound or forgot still reaches the entry its aliases share.
///
/// Then the `forgotten` entries go `Unknown` outright — the top-level rebind
/// rule ([`top_level_rebind_risk`]), which reaches every entry the frame held
/// because at file scope every name is a global. Last, because it is the
/// weakest claim of the three: a `Closed` proved about the handle the name used
/// to hold must not outlive the call that may have pointed the name elsewhere.
pub(crate) fn apply_resource_effects(effects: &ResourceEffects, store: &mut Store) {
    for (id, closed) in &effects.transitions {
        if let Some(r) = store.resources.get_mut(id) {
            r.state = if *closed { HandleState::Closed } else { r.state.escaped() };
        }
    }
    for id in &effects.forgotten {
        if let Some(r) = store.resources.get_mut(id) {
            r.state = HandleState::Unknown;
        }
    }
}

/// Every local variable a value **mentions**, at any depth the value IR spells
/// (ADR-0097 §2.4's "stored, captured, nested" escapes): array elements and
/// expression keys, nested call/method/constructor arguments and receivers, a
/// closure's by-value captures, both arms of a ternary and both operands of
/// `??`, a clone's source, the operands of a cast, a negation, a
/// concatenation and a binary operator, an offset read's base and key. A
/// property fetch, an `isset` and the literal/constant leaves mention no
/// binding a handle could ride.
pub(crate) fn mentioned_vars<'a>(value: &'a ArgValue, out: &mut Vec<&'a str>) {
    match value {
        ArgValue::Var(v) | ArgValue::Clone(v) => out.push(v),
        ArgValue::Call(_, args) => args.iter().for_each(|a| mentioned_vars(a, out)),
        ArgValue::MethodCall { callee, args, named } => {
            match callee {
                Callee::Method { receiver: Receiver::Var(v), .. } => out.push(v),
                Callee::Method { receiver: Receiver::New { args, named, .. }, .. } => {
                    for value in args.iter().chain(named.iter().map(|n| &n.value)) {
                        mentioned_vars(value, out);
                    }
                }
                _ => {}
            }
            for value in args.iter().chain(named.iter().map(|n| &n.value)) {
                mentioned_vars(value, out);
            }
        }
        ArgValue::New(_, args, named) => {
            for value in args.iter().chain(named.iter().map(|n| &n.value)) {
                mentioned_vars(value, out);
            }
        }
        ArgValue::Array(items) => {
            for (key, value) in items {
                if let ArrayKey::Expr(k) = key {
                    mentioned_vars(k, out);
                }
                mentioned_vars(value, out);
            }
        }
        ArgValue::Ternary { then_val, else_val, .. } => {
            mentioned_vars(then_val, out);
            mentioned_vars(else_val, out);
        }
        ArgValue::Closure(ClosureRef::Anonymous { captures, .. }) => {
            out.extend(captures.iter().map(String::as_str));
        }
        ArgValue::Coalesce(a, b, _) | ArgValue::Concat(a, b) => {
            mentioned_vars(a, out);
            mentioned_vars(b, out);
        }
        ArgValue::Binary { lhs, rhs, .. } | ArgValue::Logical { lhs, rhs, .. } => {
            mentioned_vars(lhs, out);
            mentioned_vars(rhs, out);
        }
        ArgValue::OffsetRead { base, key } => {
            mentioned_vars(base, out);
            mentioned_vars(key, out);
        }
        ArgValue::Not(v) | ArgValue::Cast { operand: v, .. } => mentioned_vars(v, out),
        ArgValue::Int(_)
        | ArgValue::Float(_)
        | ArgValue::Str(_)
        | ArgValue::Bool(_)
        | ArgValue::Null
        | ArgValue::Closure(ClosureRef::FunctionName(_))
        | ArgValue::PropFetch { .. }
        | ArgValue::ClassConst(..)
        | ArgValue::EnumCase(..)
        | ArgValue::GlobalConst(_)
        | ArgValue::Isset(_)
        | ArgValue::Other => {}
    }
}

/// Let every heap resource `value` mentions escape (ADR-0097 §2.4's "stored
/// into an array or a property, captured by a closure, returned" rows): the
/// statement-effect twin of [`resource_call_effects`], for the rvalue positions
/// that are not calls — an assignment's right-hand side, an offset write's
/// value, a `return` operand.
pub(crate) fn escape_mentioned_resources(value: &ArgValue, store: &mut Store) {
    let mut vars = Vec::new();
    mentioned_vars(value, &mut vars);
    for v in vars {
        store.escape_resource(v);
    }
}

pub(crate) fn builtin_return_floor(cx: &Cx, name: &str) -> Option<Vec<ContractArm>> {
    if cx.index.has_simple_function(name) {
        return None;
    }
    if !floor_target_admits(name, cx.php_target) {
        return None;
    }
    let declared = steins_catalog::declared_return(name)?;
    let arms = flatten_arms(steins_contract::lower_str(declared)?);
    // The resolver is the **identity**, a claim worth stating now that mining
    // admits class rows (`imageloadfont` = `GdFont`).
    //
    // `refine_declared_arms`' resolver exists to turn a *relative* class name in a
    // project docblock into an FQN against the declaring namespace. A functionMap
    // row has no declaring namespace: every class it names is a global builtin FQN
    // as PHP resolves it (`GdFont`, `CurlHandle`, already-qualified `ast\Node`).
    // Running a project namespace resolver over those would MANGLE them (`GdFont`
    // inside `namespace App;` would become `App\GdFont`), so identity is the only
    // correct resolver — and it preserves `ContractTy::Class`'s own normalization
    // (`lower_identifier` strips a leading `\`, case-folds), matching the
    // generation-time countersign. Same argument for a class inside an array row's
    // element type.
    refine_declared_arms(&[], arms, &|n: &str| n.to_owned())
}

/// The declared-return floor's **method** half (issue #673): the contract arms a
/// builtin `class::method` call seeds, every arm `Asserted`.
///
/// It is the last rung of the method return ladder, reached only where the project
/// chain answered nothing (`resolve_builtin_callee` is that reading, and refuses if
/// any project class on the receiver's chain declares the name). The lowering is
/// [`builtin_return_floor`]'s, verbatim and for the same reasons — the same
/// `lower_str` → [`flatten_arms`] → [`refine_declared_arms`] path against an empty
/// native list, and the same **identity** class resolver, since a functionMap row
/// has no declaring namespace and every class it names is a global builtin FQN.
///
/// Three gates, and the first two are this rung's own:
///
/// 1. **Inheritance, walked here rather than stored in the table.** A row is keyed
///    where functionMap puts it, so a `SplFileObject` receiver finds
///    `SplFileInfo::getPath` only by walking upward. That walk is sound because PHP
///    enforces return covariance at class-declaration time — a child cannot widen
///    the parent's promise — so the declaring class's **native, non-`mixed`**
///    engine envelope is an upper bound on every override (ADR-0049 A16), which is
///    a membership-direction claim about the *result* and needs no exactness about
///    the receiver. The miner is what makes the envelope native: it admits no row
///    the engine declared nothing or `mixed` for, precisely because those bound no
///    descendant. The walk is [`builtin_class_supers`]' transitive closure,
///    breadth-first so the nearest row wins; an unknown class contributes no supers
///    and simply ends its branch (ADR-0043's FP-safe absence), and a **shadow key**
///    on the way up ends the walk with no answer at all — see
///    [`builtin_method_row`].
/// 2. **The static form constrains, the instance form does not.** PHP lets `$o->m()`
///    call a `static` method, so an instance call accepts either kind of row. `C::m()`
///    on an instance method is a PHP 8 `Error` unless it is forwarding `$this` from
///    inside a class — the same parity `resolve_static_named` and
///    [`resolve_declaration_target`] already keep, so a call that never returns is
///    handed no envelope.
/// 3. **Version discipline** ([`builtin_method_target_admits`]) — the A11-shaped
///    target gate, keyed on the class the row was *found* on, since that is the row
///    whose minor moved.
///
/// The stratum is not returned, exactly as for the function half:
/// `refine_declared_arms` over an empty native list marks every arm `Asserted`, so
/// the proof layer's all-Verified premise rule keeps these arms out of every finding
/// by construction. An object-returning row is doubly excluded — the value domain has
/// no object inhabitant, so [`floor_value_fact`] finds no fact to seed either.
///
/// [`builtin_class_supers`]: steins_catalog::builtin_class_supers
/// [`resolve_declaration_target`]: crate::dispatch::resolve_declaration_target
pub(crate) fn builtin_method_return_floor(
    cx: &Cx,
    callee: &BuiltinCallee,
) -> Option<Vec<ContractArm>> {
    let (declared, is_static) =
        builtin_method_row(&callee.class, &callee.method, cx.php_target)?;
    if callee.static_call && !is_static && !callee.inside_class {
        return None;
    }
    let arms = flatten_arms(steins_contract::lower_str(declared)?);
    refine_declared_arms(&[], arms, &|n: &str| n.to_owned())
}

/// The row a builtin `class::method` resolves to, walking the builtin hierarchy
/// upward from `class` — the inheritance half of [`builtin_method_return_floor`],
/// with the version gate applied at the class the row was found on.
///
/// Breadth-first over [`builtin_class_supers`], so the nearest declaration wins
/// where two ancestors both carry a row (`SplFileObject::key` shadows
/// `SplFileInfo`'s were there one). The frontier is deduplicated, which both
/// terminates on the diamond every SPL interface makes and bounds the walk.
///
/// **A shadow key ends the walk with no answer.** Climbing past a class reads "no
/// row here" as "inherit", and that reading is only sound where functionMap was
/// *silent* about the class. Where it stated a row the miner could not carry or
/// the engine refused, the map is saying the child's declaration differs — so
/// [`declared_method_return_blocked`] is asked at every class on the way up, ahead
/// of the row lookup, and a hit answers nothing at all. It is checked ahead
/// because it is the stronger fact; the two tables are disjoint by construction,
/// so the order cannot change which one answers, only which one is read first.
///
/// [`builtin_class_supers`]: steins_catalog::builtin_class_supers
/// [`declared_method_return_blocked`]: steins_catalog::declared_method_return_blocked
fn builtin_method_row(
    class: &str,
    method: &str,
    target: Option<&steins_db::PhpTarget>,
) -> Option<(&'static str, bool)> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut frontier = vec![class.to_owned()];
    while !frontier.is_empty() {
        let mut next = Vec::new();
        for name in &frontier {
            if !seen.insert(name.to_ascii_lowercase()) {
                continue;
            }
            if steins_catalog::declared_method_return_blocked(name, method) {
                return None;
            }
            if let Some(row) = steins_catalog::declared_method_return(name, method) {
                // A row found and then declined by the version gate ENDS the walk
                // rather than resuming it up the chain: the nearest declaration is
                // the one this call reaches, and a grandparent's row for the same
                // name is a different row, not a fallback for this one.
                return method_target_admits(name, method, target).then_some(row);
            }
            next.extend(
                steins_catalog::builtin_class_supers(name)
                    .unwrap_or_default()
                    .into_iter()
                    .map(str::to_owned),
            );
        }
        frontier = next;
    }
    None
}

/// The method floor's version gate (ADR-0069 §3, A11-shaped): the twin of
/// [`floor_target_admits`], keyed on `class::method`.
///
/// A `Some(m)` from the change oracle says the method's declared return type last
/// moved at minor `m`, so the mined row is known good only for a target lying wholly
/// at or above it. An **undeclared target admits**, the row being Asserted anyway; a
/// key the oracle does not list admits unconditionally.
pub(crate) fn method_target_admits(
    class: &str,
    method: &str,
    target: Option<&steins_db::PhpTarget>,
) -> bool {
    let Some(boundary) = steins_catalog::declared_method_return_changed_at(class, method) else {
        return true;
    };
    match target {
        Some(t) => t.floor >= boundary,
        None => true,
    }
}

/// The value-lane seed a floor arm list contributes: the single value-domain
/// [`Fact`] its arms denote, or `None` when they denote more than one.
///
/// Both abstract layers are reachable from here, through the lowering each already
/// owns — [`seed_shape_fact`] for an array arm (ADR-0062 S3), [`contractty_to_fact`]
/// for a scalar one. A builtin row and a project function's `@return array{…}` are
/// the same arm list by the time they arrive here, so the array vocabulary needed
/// no new seam.
///
/// The single-fact rule is why #73's pins survive every widening unchanged. A
/// one-arm row binds `$r = f(...)` to one fact, premising the contract-layer
/// return check. A genuinely multi-arm row (`string|false`, `false|array`) has no
/// single fact — the value domain carries no union layer over either vocabulary —
/// so it stays in the arm lane alone.
///
/// A `?T` **scalar** pair is one fact (`nullable` is a side flag), which is how
/// `?string` rows keep their #73 rendering. A `?array{…}` row is **not**:
/// [`fact_with_null`] refuses a shape, so it lives in the arm lane alone — a
/// designed refusal, the FP-safe side (the arms still carry the null).
///
/// A **class** row (`GdFont`, `?GdFont`, bare `object`) declines for a stronger
/// reason: the value domain has no object inhabitant at all (ADR-0035/0038), so
/// there is no fact to seed. Both lowerings say so independently
/// ([`contractty_to_fact`] has no `Class`/`ObjectAny` arm, `to_shape_fact` has
/// none either) — a class row is **arm-lane only**, unconditionally.
pub(crate) fn floor_value_fact(arms: &[ContractArm]) -> Option<Fact> {
    let (nulls, rest): (Vec<ContractArm>, Vec<ContractArm>) =
        arms.iter().cloned().partition(|a| matches!(a.ty, ContractTy::Null));
    let [only] = rest.as_slice() else { return None };
    let fact = match seed_shape_fact(&rest) {
        Some(shape) => shape,
        None => contractty_to_fact(&only.ty)?,
    };
    if nulls.is_empty() { Some(fact) } else { fact_with_null(&fact) }
}

/// Which rung of the builtin-call ladder answered ([`builtin_call_rung`]), with
/// its answer.
pub(crate) enum BuiltinRung {
    /// A §2.7 fold over a proven handle (ADR-0097), at its stratum.
    ResourceFold(Fact, Stratum),
    /// The argument-dependent rung (ADR-0061 §1), at the argument's stratum.
    Shape(Fact, Stratum),
    /// The engine's reflected return envelope (ADR-0056 R1): `Verified`, read off
    /// the running engine's own arginfo (§2).
    Envelope(Fact),
    /// The resource-return arms and the heap resource the binding takes beside
    /// them (ADR-0056 §8, ADR-0097 §2.3).
    ResourceArms(Vec<ContractArm>, HeapRes),
    /// The declared-return floor (ADR-0069), every arm `Asserted`.
    Floor(Vec<ContractArm>),
}

/// Which of the ladder's two seam-dependent rungs a seam asks
/// ([`builtin_call_rung`]).
#[derive(Clone, Copy)]
pub(crate) struct OptionalRungs {
    /// The §2.7 resource folds: only where no other call of the statement can
    /// have moved the handle's state first (`resource_folds`' module doc).
    pub(crate) resource_folds: bool,
    /// The resource-return arms: only at a seam that binds, since they come with
    /// a heap resource and only a binding has somewhere to put it.
    pub(crate) resource_arms: bool,
}

/// **The builtin-call ladder**, walked once: the first rung that answers for
/// `name(args)`, and which rung that was, or `None` when every rung declines.
///
/// 1. the §2.7 **resource folds** ([`resource_fold_return_fact`], ADR-0097),
///    above the shape rung because the two cannot both answer;
/// 2. the **argument-dependent** rung ([`shape_builtin_return_fact`], ADR-0061
///    §1), carrying the argument's stratum;
/// 3. the engine's **reflected envelope** ([`builtin_call_return_fact`]);
/// 4. the **resource-return arms** ([`builtin_resource_arms`], ADR-0056 §8),
///    below the envelope because they fire only where it structurally cannot
///    (PHP has no `resource` return-type syntax), and above the floor;
/// 5. the **declared-return floor** ([`builtin_return_floor`], ADR-0069),
///    reached only where the engine said nothing about the name.
///
/// Three seams climb it and each keeps its own sink: the assignment binds
/// (`apply_assign`), the dump renders, the operand position reads a fact
/// ([`builtin_operand_fact`]). A new rung goes here, once. [`OptionalRungs`]
/// says which of rungs 1 and 4 a seam asks.
///
/// **Poison.** Rungs 1 and 2 read the env and refuse a poisoned scope
/// themselves. Rungs 3–5 read nothing of the scope, and the ladder does not
/// refuse them: the assignment and the operand refuse a poisoned scope before
/// they ask (they bind and answer nothing there), while the dump asks in one
/// too and renders what the name declares.
#[allow(clippy::too_many_arguments)]
pub(crate) fn builtin_call_rung(
    cx: &Cx,
    folder: &mut dyn Folder,
    name: &str,
    args: &[ArgValue],
    env: &HashMap<String, Known>,
    store: Option<&Store>,
    poisoned: bool,
    rungs: OptionalRungs,
) -> Option<BuiltinRung> {
    if rungs.resource_folds
        && let Some(store) = store
        && let Some((fact, stratum)) =
            resource_fold_return_fact(cx, folder, name, args, env, store, poisoned)
    {
        return Some(BuiltinRung::ResourceFold(fact, stratum));
    }
    if let Some((fact, stratum)) =
        shape_builtin_return_fact(cx, folder, name, args, env, store, poisoned)
    {
        return Some(BuiltinRung::Shape(fact, stratum));
    }
    if let Some(fact) = builtin_call_return_fact(cx, folder, name) {
        return Some(BuiltinRung::Envelope(fact));
    }
    if rungs.resource_arms
        && let Some((arms, res)) = builtin_resource_arms(cx, folder, name)
    {
        return Some(BuiltinRung::ResourceArms(arms, res));
    }
    builtin_return_floor(cx, name).map(BuiltinRung::Floor)
}

/// **The fact a builtin call in OPERAND position denotes** (issue #646) — the
/// same ladder the dump seam ([`crate::dump`]) and the assignment seam
/// ([`crate::assign`]) already climb, so one value has one answer however it is
/// spelled.
///
/// `value_operand_fact` fell through to [`transfer_arg_known`], which has a
/// `Var` rung, an array-literal rung and a literal rung and **no builtin-return
/// rung at all**. So `(string) rand()` reached the cast with nothing and took the
/// operator's floor, while `$r = rand(); (string) $r` reached it with the
/// catalog's `int` — two spellings of one value, two answers. This is the missing
/// rung, and it is the assignment seam's own ladder ([`builtin_call_rung`]), in
/// its order (ADR-0056 §9 made exactly this argument one seam earlier, for
/// arguments): the shape rung at the argument's stratum, the reflected envelope
/// at `Verified`, and the declared-return floor at `Asserted` — a catalog row is
/// a declaration, not a runtime answer (ADR-0069), so `(string) rand()` prints
/// `(asserted)` exactly as its hoisted twin does and can never premise a
/// proof-layer finding (ADR-0061 §3).
///
/// Two rungs are not asked. The §2.7 resource folds read the handle's state as
/// the statement started, and an operand is a composed spelling in which another
/// call may have run first (`resource_folds`' module doc). The resource-return
/// arms come with a heap resource, and an operand binds nothing.
///
/// **It answers no more than those two seams do.** A name the engine is silent
/// about and the catalog cannot describe declines, totally, and the operator
/// takes its own floor — which is what every operand of a builtin did before this
/// rung existed. A rung answering *more* would be a second source of truth and
/// would reintroduce the drift it closes. So a poisoned scope, in which the
/// assignment binds nothing, answers nothing here either.
pub(crate) fn builtin_operand_fact(
    cx: &Cx,
    folder: &mut dyn Folder,
    name: &str,
    args: &[ArgValue],
    env: &HashMap<String, Known>,
    store: Option<&Store>,
    poisoned: bool,
) -> Option<(Fact, Stratum)> {
    if poisoned {
        return None;
    }
    let rungs = OptionalRungs { resource_folds: false, resource_arms: false };
    match builtin_call_rung(cx, folder, name, args, env, store, poisoned, rungs)? {
        BuiltinRung::ResourceFold(fact, stratum) | BuiltinRung::Shape(fact, stratum) => {
            Some((fact, stratum))
        }
        BuiltinRung::Envelope(fact) => Some((fact, Stratum::Verified)),
        BuiltinRung::Floor(arms) => floor_operand_known(&arms),
        // Not asked.
        BuiltinRung::ResourceArms(..) => None,
    }
}

/// The floor's **two** carriers, read the way a variable bound to them is read.
///
/// The assignment seam seeds both — the value lane through [`floor_value_fact`],
/// the arm lane with the arms themselves — and [`transfer_arg_known`]'s `Var`
/// rung then prefers whichever says more than the bare envelope. Reproducing that
/// preference here is what makes the agreement hold for a **multi-arm** row
/// (`realpath`'s `string|false`, which the value lane cannot seed and the arm
/// lane lowers through ADR-0085's union) and not only for `rand`'s single one.
fn floor_operand_known(arms: &[ContractArm]) -> Option<(Fact, Stratum)> {
    let value_lane = floor_value_fact(arms);
    if let Some(fact) = value_lane.clone()
        && !matches!(fact, Fact::General { .. })
    {
        return Some((fact, Stratum::Asserted));
    }
    if let Some((fact, stratum)) = declared_arm_known(arms)
        && !matches!(fact, Fact::General { .. })
    {
        return Some((fact, stratum));
    }
    value_lane.map(|f| (f, Stratum::Asserted))
}

/// The floor's version gate (ADR-0069 §3, A11-shaped): whether the project's
/// declared PHP target agrees with the minor the mined row was stated at.
///
/// `steins_catalog::declared_return_changed_at` is the change oracle — a
/// `Some(m)` says the builtin's declared return type last moved at minor `m`, so
/// the mined row is only known good for a target lying **wholly at or above** `m`
/// (stricter than "does not straddle": a target entirely below the boundary is
/// just as wrong).
///
/// An **undeclared target admits**: the row is Asserted anyway, and its consumers
/// tolerate that grade. A name the oracle does not list admits unconditionally.
pub(crate) fn floor_target_admits(name: &str, target: Option<&steins_db::PhpTarget>) -> bool {
    let Some(boundary) = steins_catalog::declared_return_changed_at(name) else {
        return true;
    };
    match target {
        Some(t) => t.floor >= boundary,
        None => true,
    }
}

/// The **argument-dependent** return rung (ADR-0061 §1) for the two ADR-0062 §4
/// transfers that read the abstract array stratum: `count($x)` and
/// `array_is_list($x)`. `None` — decline — is a first-class outcome, and the
/// caller falls through to the argument-insensitive envelope rung.
///
/// The rule fires only on a single-argument call whose one argument is a bare
/// variable or a depth-1 property fetch carrying a non-nullable [`Fact::Shape`]
/// — or a [`Fact::Singleton`] array, lifted to a shape ([`ShapeFact::lift`])
/// once the value lane's own order-dependent projections (issue #118) have
/// first refused the name, so a literal array is never worse off than a
/// declared one. A nullable base declines, a second argument declines
/// (`count($x, COUNT_RECURSIVE)` counts something else), and a project function
/// shadowing the name declines through [`builtin_call_return_fact`]'s own check.
///
/// **The admission gate is ADR-0061 §2's, unweakened**: seeded only when the
/// sidecar-backed envelope for this name exists AND the fact is extensionally
/// inside it (`envelope ⊔ out == envelope`). A rule claiming something the
/// running engine's own declaration disowns is discarded, never demoted.
///
/// **Stratum is ADR-0061 §3's derivation clause**: the output carries the
/// argument fact's stratum, `Asserted` for a declared shape — so
/// `count($declaredShape)` can never premise a proof-layer finding (A-G9's
/// corollary), while `count()` of a *proven* array folds to a Singleton unchanged.
pub(crate) fn shape_builtin_return_fact(
    cx: &Cx,
    folder: &mut dyn Folder,
    name: &str,
    args: &[ArgValue],
    env: &HashMap<String, Known>,
    store: Option<&Store>,
    poisoned: bool,
) -> Option<(Fact, Stratum)> {
    if poisoned {
        return None;
    }
    // The argument-DISPATCHED family (ADR-0064 seam ii, DR3) sits at the same
    // seam, one step earlier: its rules read arguments this rung's single-shape
    // pattern cannot even bind. Declining falls straight through to the shape
    // rung below, unchanged.
    if let Some(out) = arg_dispatch_return_fact(cx, folder, name, args, env, store) {
        return Some(out);
    }
    // **The subject binds by what it resolves to, not by how it was spelled**
    // (issue #328 L1). A bare variable reads the env; a property fetch reads the
    // allocation-keyed heap (ADR-0036), the same single lookup the assignment
    // form performs, so `count($o->p)` and `$v = $o->p; count($v)` cannot
    // disagree (issue #610) — an unbound receiver, an unknown or swept prop each
    // carry no fact and decline; an array written *at the call site* resolves
    // through the seeding ladder, so `count(['a' => $x, 'b' => $x])` is no
    // worse than the two-statement spelling.
    //
    // Deliberately only these four forms — every other spelling would need
    // resolving to find out it is not an array, and most calls reaching this
    // rung are not in the family at all.
    //
    // The `Call` form is what makes a projection *of* a projection compose
    // (`array_values(array_keys([…]))`, issue #329). It terminates because each
    // level strips one call from a finite expression.
    let seeded;
    let (subject_fact, subject_stratum) = match args {
        [ArgValue::Var(var), ..] => {
            let known = env.get(var)?;
            (known.fact.as_ref()?, known.stratum)
        }
        [ArgValue::PropFetch { var, prop }, ..] => {
            let store = store?;
            (store.prop_fact(var, prop)?, store.prop_stratum(var, prop))
        }
        [ArgValue::Array(items), ..] => {
            seeded = cx
                .resolve_literal(&args[0], env, poisoned, folder)
                .and_then(|lit| singleton_fact(&lit, cx.php_minor))
                .map(|f| (f, value_stratum(cx, &args[0], env, store)))
                .or_else(|| array_literal_fact(cx, folder, items, env, poisoned, store))?;
            (&seeded.0, seeded.1)
        }
        [call @ ArgValue::Call(..), ..] => {
            let (lit, strat) = cx.resolve_literal_strat(call, env, poisoned, folder)?;
            seeded = (singleton_fact(&lit, cx.php_minor)?, strat);
            (&seeded.0, seeded.1)
        }
        _ => return None,
    };
    let [_, rest @ ..] = args else { return None };

    // **The value lane's own privilege** (ADR-0062 §2): a subject whose fact is a
    // witnessed `Val::Array` carries true insertion order, so the order-dependent
    // projection may be *executed* rather than widened. Taken before the shape
    // binding below, since a `Singleton` is not a `Fact::Shape`.
    //
    // A name that projection declines is not a dead end: the same entries LIFT to
    // a `ShapeFact` (issue #262) and fall through to the rung below exactly as a
    // seeded `Fact::Shape` would — a literal array only sharpens what that rung
    // can answer.
    let lifted;
    let shape: &ShapeFact = match subject_fact {
        Fact::Singleton(Val::Array(entries)) => {
            if let Some(out) = witnessed_projection_fact(cx, folder, name, entries, args, env, store) {
                return Some((out, derivation_stratum(cx, folder, args, env, store, subject_stratum)));
            }
            lifted = ShapeFact::lift(entries);
            &lifted
        }
        Fact::Shape { shape, nullable: false } => shape.as_ref(),
        _ => return None,
    };

    // **The positional projections, executed** (issue #328). A shape that
    // witnessed its own construction carries a realizable key sequence, so the
    // family may run over it instead of taking the key-set widening below. A
    // shape that witnessed nothing falls straight through (ADR-0062 §7's
    // declined import, stays declined).
    if let Some(order) = shape.witnessed_order() {
        let entries: Vec<(VKey, Option<Fact>)> = order
            .iter()
            .filter_map(|k| shape.field(k).map(|(_, _, slot)| (k.clone(), slot.clone().map(|f| *f))))
            .collect();
        if entries.len() == order.len()
            && let Some(out) =
                witnessed_family_fact(cx, folder, name, &entries, args, env, store)
        {
            return Some((out, derivation_stratum(cx, folder, args, env, store, subject_stratum)));
        }
    }

    let out = if rest.is_empty()
        && (name.eq_ignore_ascii_case("count") || name.eq_ignore_ascii_case("sizeof"))
    {
        let range = shape.count_range();
        if range.lo() == range.hi() {
            // The one place a shape has an exact size: a sealed, all-required
            // shape (ADR-0062 §4, mirroring PHPStan's own exactness).
            Fact::Singleton(Val::Int(range.lo()))
        } else {
            Fact::refined(Base::Int, Refinement::Int(range), false)
        }
    } else if rest.is_empty() && name.eq_ignore_ascii_case("array_is_list") {
        match shape.is_list {
            // The answer IS the denotational flag (§4's row) — no structural
            // inspection, and `Maybe` answers nothing.
            Certainty::Yes => Fact::Singleton(Val::Bool(true)),
            Certainty::No => Fact::Singleton(Val::Bool(false)),
            Certainty::Maybe => return None,
        }
    } else {
        // The positional-projection family (ADR-0062 S7) carries its own
        // admission gate — the reflected *declaration*, since its results are not
        // facts the scalar envelope path can name.
        let fact = shape_projection_fact(cx, folder, name, shape, args, env, store)?;
        return Some((fact, derivation_stratum(cx, folder, args, env, store, subject_stratum)));
    };

    let envelope = builtin_call_return_fact(cx, folder, name)?;
    (envelope.join(&out).as_ref() == Some(&envelope)).then_some((out, subject_stratum))
}

/// **ADR-0061 §3's derivation clause over every argument the call passes**: `min`
/// of the subject's own stratum and each other argument's.
///
/// For single-argument arms this is the subject's stratum unchanged; the
/// argument-reading arm (`array_slice`, issue #118) is where an offset read out
/// of a docblock-claimed binding can lower it. Computed **after** a rule has
/// produced a fact and never before, since most calls arriving here are not in
/// this family at all.
fn derivation_stratum(
    cx: &Cx,
    folder: &mut dyn Folder,
    args: &[ArgValue],
    env: &HashMap<String, Known>,
    store: Option<&Store>,
    subject: Stratum,
) -> Stratum {
    args.iter().fold(subject, |acc, v| {
        acc.min(
            transfer_arg_known(cx, folder, v, env, store)
                .map_or_else(|| value_stratum(cx, v, env, store), |(_, s)| s),
        )
    })
}

/// **The admission gate both symbolic-transfer rungs share** (ADR-0061 §2): the
/// running engine's own reflected *declaration* must be the one the rule was
/// written against, and — where the declaration pins too little on its own —
/// its arity must be too (ADR-0064 Amendment B).
///
/// Three refusals, each an existing rung already applied by hand before issue
/// #118 gave them one home:
///
/// 1. **A project function shadowing the simple name** is not the builtin.
/// 2. **A silent engine withholds.** No sidecar, an A9 monkey-patch, or a name
///    the engine declares nothing about: withheld rather than trusted.
/// 3. **A moved signature withholds.** An engine answering no arity withholds
///    exactly as a silent declaration does; a non-pinned arity means the
///    signature has moved and the rule is stale.
pub(crate) fn transfer_declaration_admits(
    cx: &Cx,
    folder: &mut dyn Folder,
    name: &str,
    declared: &[&str],
    arity: Option<(u32, u32)>,
) -> bool {
    if cx.index.has_simple_function(name) {
        return false;
    }
    let Some(reflected) = folder.builtin_return_type(name) else { return false };
    if !declared.iter().any(|d| d.eq_ignore_ascii_case(&reflected)) {
        return false;
    }
    match arity {
        None => true,
        Some(pin) => folder.builtin_param_counts(name) == Some(pin),
    }
}

/// **The extensional leg of ADR-0061 §2's gate**, which the declaration pin
/// above cannot carry on its own.
///
/// The pin says the engine still declares what the rule was written against. It
/// says nothing about the rule's OUTPUT being *inside* that declaration — a
/// second question, and the one ADR-0061 §2 actually names ("the output is an
/// extensional subset of the reflected envelope"). Where the reflected
/// declaration lowers to a value-domain [`Fact`], this asks it, through the same
/// domain join ADR-0056 §1.2 runs over curated rows rather than a second copy of
/// that machinery: `envelope ⊔ out == envelope`.
///
/// The scalar UNIONS are why this exists. [`envelope_fact`] refuses them for
/// *seeding* — a coarse `int|float` envelope would shadow the sharper ADR-0069
/// floor row — but refusing to seed a union is not refusing to *check against*
/// one, and `abs`, `array_sum` and `array_product` declare exactly `int|float`.
/// The refusal that made the arithmetic family's envelope unusable as a seed is
/// what makes it usable as a bound.
///
/// A declaration with no `Fact` form leaves the pin standing alone, unchanged:
/// `array` and `array|string|null` for the shape transfers, bare `mixed` for
/// `min`/`max`, and — inside this very family — `pow`'s `object|int|float`,
/// whose `object` arm no `Fact` names. That is ADR-0061 §2's recorded cost — "a
/// builtin reflecting no representable envelope hosts no rung to refine
/// within" — and not a bypass: the declaration and its arity still countersign
/// the rule, which is the whole authority those rules ever had.
pub(crate) fn transfer_envelope_admits(
    cx: &Cx,
    folder: &mut dyn Folder,
    name: &str,
    declared: &[&str],
    arity: Option<(u32, u32)>,
    out: &Fact,
) -> bool {
    if !transfer_declaration_admits(cx, folder, name, declared, arity) {
        return false;
    }
    let Some(reflected) = folder.builtin_return_type(name) else { return false };
    declared_envelope_admits(&reflected, out)
}

/// [`transfer_envelope_admits`]'s pure core — the subset check itself, over a
/// reflected declaration STRING, so every leg of it is unit-testable without a
/// sidecar (this module's standing discipline).
///
/// `true` when the declaration has no `Fact` form: nothing to be inside is not
/// the same as being outside, and the declaration pin is what carries such a
/// rule. See [`transfer_envelope_admits`] for why that is a recorded cost and
/// not a bypass.
pub(crate) fn declared_envelope_admits(reflected: &str, out: &Fact) -> bool {
    let Some(envelope) =
        steins_contract::lower_str(reflected).as_ref().and_then(contractty_to_fact)
    else {
        return true;
    };
    envelope.join(out).as_ref() == Some(&envelope)
}

#[cfg(test)]
mod return_fact_admission_tests {
    //! ADR-0056 §1–2 — the pure admission-gate core ([`admit_return_fact`] and its
    //! helpers), tested without a sidecar. Covers: the reflected envelope alone for
    //! each single representable base; the un-representable cases (multi-base union,
    //! non-scalar, `mixed`/`void`); and the three curated-refinement legs — admitted
    //! (subset ∧ pinned), rejected by a failed subset check, and rejected by a minor
    //! mismatch. The R1 generated table is empty, so curation is exercised with
    //! hand-passed refinement strings here.
    use super::*;
    use crate::builtin_returns::admit_return_fact;
    use steins_contract::ContractTy;
    use crate::builtin_returns::{envelope_fact, floor_target_admits};

    #[test]
    fn envelope_alone_for_each_representable_base() {
        // A curated-less row (the R1 reality) seeds exactly the reflected base.
        assert_eq!(admit_return_fact("bool", None, true), Some(Fact::General { base: Base::Bool, nullable: false }));
        assert_eq!(admit_return_fact("int", None, true), Some(Fact::General { base: Base::Int, nullable: false }));
        assert_eq!(admit_return_fact("string", None, true), Some(Fact::General { base: Base::String, nullable: false }));
        assert_eq!(admit_return_fact("float", None, true), Some(Fact::General { base: Base::Float, nullable: false }));
        // A `?T` nullable envelope carries nullability.
        assert_eq!(admit_return_fact("?string", None, true), Some(Fact::General { base: Base::String, nullable: true }));
    }

    #[test]
    fn unrepresentable_envelopes_seed_nothing() {
        // A multi-base union (`int|false`) is not a single value-domain fact — the
        // union case is deferred (§4), so R1 seeds nothing rather than a wrong arm.
        assert_eq!(admit_return_fact("int|false", None, true), None);
        assert_eq!(admit_return_fact("string|int|false", None, true), None);
        // Non-scalars and the top/void keywords never seed.
        assert_eq!(admit_return_fact("array", None, true), None);
        assert_eq!(admit_return_fact("object", None, true), None);
        assert_eq!(admit_return_fact("mixed", None, true), None);
        assert_eq!(admit_return_fact("void", None, true), None);
        assert_eq!(admit_return_fact("DateTime", None, true), None);
    }

    #[test]
    fn curated_refinement_admitted_when_subset_and_pinned() {
        // `count(): int` envelope refined to `int<0, max>` — a subset of `int` — is
        // admitted at the pinned minor, yielding a Refined (narrower) int fact.
        let got = admit_return_fact("int", Some("int<0, max>"), true).expect("some fact");
        assert!(
            matches!(got, Fact::Refined { base: Base::Int, .. }),
            "the admitted refinement must be a Refined int, got {got:?}"
        );
        assert_ne!(got, Fact::General { base: Base::Int, nullable: false }, "must be narrower than the envelope");
    }

    #[test]
    fn curated_string_refinement_admitted_within_string_envelope() {
        // R4 shape: `sha1(): string` envelope refined to `non-falsy-string` — a
        // subset of `string`, same base — is admitted at the pinned minor as a
        // Refined string fact (narrower than the bare string envelope).
        let got = admit_return_fact("string", Some("non-falsy-string"), true).expect("some fact");
        assert!(
            matches!(got, Fact::Refined { base: Base::String, .. }),
            "the admitted refinement must be a Refined string, got {got:?}"
        );
        assert_ne!(
            got,
            Fact::General { base: Base::String, nullable: false },
            "must be narrower than the string envelope"
        );
    }

    #[test]
    fn curated_refinement_rejected_when_not_a_subset() {
        // `non-empty-string` is NOT a subset of an `int` envelope (base mismatch):
        // the row is discarded and the envelope stands alone (never a wrong premise).
        assert_eq!(
            admit_return_fact("int", Some("non-empty-string"), true),
            Some(Fact::General { base: Base::Int, nullable: false })
        );
    }

    #[test]
    fn curated_refinement_rejected_on_minor_mismatch() {
        // A perfectly valid subset refinement is still NOT admitted when the project
        // PHP minor differs from PINNED_PHP (the A11 narrowing-direction guard, §2):
        // the envelope stands alone.
        assert_eq!(
            admit_return_fact("int", Some("int<0, max>"), false),
            Some(Fact::General { base: Base::Int, nullable: false })
        );
    }

    /// Issue #40 — the extensional leg of the transfer gate, over the scalar
    /// UNIONS the seeding path (`envelope_fact`) refuses. Refusing to *seed* a
    /// coarse union is not refusing to *check against* one, and `abs`'s
    /// `int|float` is the family's whole bound.
    #[test]
    fn a_transfer_output_is_checked_against_a_union_declaration() {
        use crate::builtin_returns::declared_envelope_admits;
        use steins_domain::{IntRange, Val};
        let non_negative =
            Fact::refined(Base::Int, Refinement::Int(IntRange::NON_NEGATIVE), false);
        // Inside `int|float`: an int refinement, a float, and either finite layer.
        assert!(declared_envelope_admits("int|float", &non_negative));
        assert!(declared_envelope_admits(
            "int|float",
            &Fact::General { base: Base::Float, nullable: false }
        ));
        assert!(declared_envelope_admits("int|float", &Fact::Singleton(Val::Int(0))));
        assert!(declared_envelope_admits("int|float", &Fact::Singleton(Val::Float(1.0))));
        assert!(declared_envelope_admits(
            "int|float",
            &Fact::from_vals(vec![Val::Int(1), Val::Float(1.0)]).expect("two values")
        ));
        // OUTSIDE it: a string, a bool, and a nullable int — each an arm the
        // engine's own declaration disowns, each discarded.
        assert!(!declared_envelope_admits(
            "int|float",
            &Fact::General { base: Base::String, nullable: false }
        ));
        assert!(!declared_envelope_admits("int|float", &Fact::Singleton(Val::Bool(true))));
        assert!(!declared_envelope_admits(
            "int|float",
            &Fact::refined(Base::Int, Refinement::Int(IntRange::NON_NEGATIVE), true)
        ));
        // A declaration with no `Fact` form leaves the pin alone — `pow`'s
        // `object` arm and `min`'s bare `mixed` are the family's two.
        assert!(declared_envelope_admits("object|int|float", &Fact::Singleton(Val::Bool(true))));
        assert!(declared_envelope_admits("mixed", &Fact::Singleton(Val::Bool(true))));
        assert!(declared_envelope_admits("array", &non_negative));
    }

    #[test]
    fn envelope_fact_shapes() {
        assert_eq!(envelope_fact(&ContractTy::Base(Base::Bool)), Some(Fact::General { base: Base::Bool, nullable: false }));
        // A non-nullable multi-base union → None.
        assert_eq!(
            envelope_fact(&ContractTy::Union(vec![ContractTy::Base(Base::Int), ContractTy::LitBool(false)])),
            None
        );
    }

    /// The ADR-0069 floor's version gate, against the real change oracle.
    ///
    /// The gate's own law, unit-tested against the mined data: `str_split` is the
    /// witness whose declared return type moved at 8.2. (Whether that particular
    /// name also carries an admitted row is a property of the mining, not of this
    /// gate — `declared_return_floor.rs` pins the end-to-end decline on a name that
    /// does.)
    #[test]
    fn floor_target_gate_declines_below_a_names_change_boundary() {
        use steins_db::{PhpTarget, PhpTargetSource};
        let target = |floor: (u16, u16), ceiling: Option<(u16, u16)>| PhpTarget {
            floor,
            ceiling,
            source: PhpTargetSource::Require,
            raw: "test".to_owned(),
        };
        // `str_split`'s declared return type moved at 8.2.
        assert_eq!(steins_catalog::declared_return_changed_at("str_split"), Some((8, 2)));

        // A STRADDLING target has no single answer — decline (the A11 shape).
        assert!(!floor_target_admits("str_split", Some(&target((8, 1), Some((8, 5))))));
        assert!(!floor_target_admits("str_split", Some(&target((8, 1), None))));
        // A target lying entirely BELOW the boundary is just as wrong: the mined row
        // states the type at the pin, which that project never runs.
        assert!(!floor_target_admits("str_split", Some(&target((8, 1), Some((8, 1))))));
        // Wholly at or above the boundary: the row is exactly what that range runs.
        assert!(floor_target_admits("str_split", Some(&target((8, 2), Some((8, 2))))));
        assert!(floor_target_admits("str_split", Some(&target((8, 3), None))));
        // An UNDECLARED target admits — the row is Asserted anyway, and its
        // consumers tolerate that grade (ADR-0069 §3).
        assert!(floor_target_admits("str_split", None));
        // A name the oracle does not list is admitted for every target: its declared
        // return type never moved across the supported line.
        assert!(floor_target_admits("str_repeat", Some(&target((8, 1), None))));
        assert!(floor_target_admits("str_repeat", None));
    }

    /// The method floor's version gate (issue #673), the same shape one key
    /// grammar over. It is unit-tested rather than fixtured because the three
    /// version-sensitive method keys all return `static`, so none of them has an
    /// admitted row at this pin — `the_method_table_and_its_version_oracle_are_disjoint_at_this_pin`
    /// is the tripwire that will demand the fixture when one does.
    #[test]
    fn the_method_target_gate_declines_below_a_keys_change_boundary() {
        use steins_db::{PhpTarget, PhpTargetSource};
        let target = |floor: (u16, u16), ceiling: Option<(u16, u16)>| PhpTarget {
            floor,
            ceiling,
            source: PhpTargetSource::Require,
            raw: "test".to_owned(),
        };
        // `DateTime::modify`'s declared return type moved at 8.3.
        let (c, m) = ("DateTime", "modify");
        assert_eq!(steins_catalog::declared_method_return_changed_at(c, m), Some((8, 3)));
        assert!(!method_target_admits(c, m, Some(&target((8, 1), Some((8, 5))))));
        assert!(!method_target_admits(c, m, Some(&target((8, 1), None))));
        assert!(!method_target_admits(c, m, Some(&target((8, 1), Some((8, 1))))));
        assert!(method_target_admits(c, m, Some(&target((8, 3), Some((8, 3))))));
        assert!(method_target_admits(c, m, Some(&target((8, 4), None))));
        // An undeclared target admits, and so does a key the oracle does not list.
        assert!(method_target_admits(c, m, None));
        assert!(method_target_admits("SplFileObject", "fgets", Some(&target((8, 1), None))));
    }

    /// The inheritance walk, exercised on the table rather than through a fixture,
    /// so a hierarchy regression names itself.
    #[test]
    fn the_method_row_walk_climbs_the_builtin_hierarchy_nearest_first() {
        // Declared on the receiver's own class.
        assert_eq!(builtin_method_row("SplFileObject", "fgets", None), Some(("string", false)));
        // Declared on a PARENT: `SplFileObject` has no `getPath` row, `SplFileInfo`
        // does, and the hierarchy table carries the edge (ADR-0043).
        assert_eq!(steins_catalog::declared_method_return("SplFileObject", "getPath"), None);
        assert_eq!(builtin_method_row("SplFileObject", "getPath", None), Some(("string", false)));
        // Nearest-first: `SplFileObject` declares `key` itself, so the walk stops
        // there rather than reaching any ancestor's row for the same name.
        assert_eq!(builtin_method_row("SplFileObject", "key", None), Some(("int", false)));
        // A class the hierarchy does not know contributes no supers and ends its
        // branch — absence is `Unknown`, never a wrong row (ADR-0043 §3).
        assert_eq!(builtin_method_row("NoSuchBuiltin", "getPath", None), None);
        assert_eq!(builtin_method_row("SplFileObject", "noSuchMethodAnywhere", None), None);
    }
}
