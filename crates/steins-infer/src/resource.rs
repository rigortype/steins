//! The resource family (ADR-0097, ADR-0098): what the walk knows about a PHP
//! resource handle, from the call that produces it to the call that closes it.
//!
//! This file is the lock every consumer reads first: whether a binding provably
//! holds a resource ([`store_holds_resource`]), and in which state
//! ([`proven_resource_state`]). The rest is split by stage, and what other modules
//! call is re-exported here:
//!
//! * [`producers`] — the arms and the heap resource a producer's return binds
//!   (`fopen` and the other catalog rows), and the element places
//!   `stream_socket_pair` and `proc_open` mint;
//! * [`closers`] — what a statement's calls do to the handles they are handed:
//!   close, keep, or let escape;
//! * [`folds`] — `gettype`, `get_debug_type`, `get_resource_type` and
//!   `get_resource_id` over a proven handle (§2.7).

mod closers;
mod folds;
mod producers;

pub(crate) use closers::{
    ResourceEffects, apply_resource_effects, escape_mentioned_resources, resource_call_effects,
};
pub(crate) use folds::resource_fold_return_fact;
pub(crate) use producers::{
    apply_produced_places, bind_produced_places, builtin_resource_arms, seed_produced_places,
    socket_pair_places, stmt_produced_places,
};

use steins_contract::ContractTy;

use crate::env::{ContractArm, HandleState, Store, Stratum};

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
/// [`site_verdict`]: crate::resource::closers
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
