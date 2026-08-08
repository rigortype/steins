//! The declared-receiver lane (ADR-0049 §8 / S6), routed by minimum stratum
//! (ADR-0049 A13).
//!
//! One ladder, two ids. The lane fires when a receiver's narrowed declared type
//! provably lacks the method; its ladder adds descendant closure, because a
//! subclass — including one minted by `eval` — could satisfy the contract and
//! define the method. "Conditional is not enough" (§8).
//!
//! Which id carries the result is decided by the lane's arms and nothing else: a
//! native declaration is `Verified` (PHP enforces it at the call boundary), so the
//! finding is `call.undefined-method` on the proof layer; a `@param`/`@var` claim is
//! `Asserted`, so it stays `phpdoc.undefined-method` on the contract layer.
//!
//! This file pins the **ladder**: each leg has a firing fixture and a silence
//! fixture, and every silence leg is asserted over BOTH ids — a leg that only
//! silenced the contract id would look green while the promotion leaked past it.
//! The routing itself, the profile surfaces and the copy-propagation widening live
//! in `s6_routing.rs`.

use steins_infer::{
    CALL_UNDEFINED_METHOD_ID, Diagnostic, Folder, PHPDOC_UNDEFINED_METHOD_ID, check_with,
};
use steins_syntax::SourceTree;

/// A boot-surface mock mirroring the S2 test harness: `available` is the A9
/// family-availability gate; `builtins` are the lowercased names the boot surface
/// reports as resident class-likes (A2ii homonyms); `minor` overrides the reported
/// PHP `(major, minor)` for the A11 skew leg.
struct Boot {
    available: bool,
    builtins: Vec<String>,
    minor: Option<(u16, u16)>,
}

impl Boot {
    fn ready() -> Self {
        Boot { available: true, builtins: Vec::new(), minor: None }
    }
    fn with_builtins(names: &[&str]) -> Self {
        Boot {
            available: true,
            builtins: names.iter().map(|n| n.to_ascii_lowercase()).collect(),
            minor: None,
        }
    }
    fn with_minor(m: (u16, u16)) -> Self {
        Boot { available: true, builtins: Vec::new(), minor: Some(m) }
    }
    fn unavailable() -> Self {
        Boot { available: false, builtins: Vec::new(), minor: None }
    }
}

impl Folder for Boot {
    fn fold(&mut self, _name: &str, _args: &[steins_syntax::ArgValue]) -> Option<steins_syntax::ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        self.available
    }
    fn boot_surface_class_like(&mut self, fqn: &str) -> Option<bool> {
        Some(self.builtins.iter().any(|b| b.eq_ignore_ascii_case(fqn)))
    }
    fn php_minor(&mut self) -> Option<(u16, u16)> {
        self.minor
    }
}

fn run_id(src: &str, folder: &mut dyn Folder, id: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "test.php", folder).into_iter().filter(|d| d.id == id).collect()
}

/// The contract-layer (Asserted-premise) findings with a ready boot surface.
fn fires(src: &str) -> Vec<Diagnostic> {
    run_id(src, &mut Boot::ready(), PHPDOC_UNDEFINED_METHOD_ID)
}

/// The proof-layer (all-`Verified`-premise) findings with a ready boot surface —
/// the A13-promoted half, told apart from S2's own emissions by the
/// declared-receiver evidence phrasing.
fn promoted(src: &str) -> Vec<Diagnostic> {
    run_id(src, &mut Boot::ready(), CALL_UNDEFINED_METHOD_ID)
        .into_iter()
        .filter(|d| d.message.contains("declared receiver"))
        .collect()
}

/// Every declared-receiver finding, whichever id A13 routed it to, under `folder`.
/// The silence legs assert over this: a ladder leg must silence the lane, not merely
/// move it between the two ids.
fn family_with(src: &str, folder: &mut dyn Folder) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "test.php", folder)
        .into_iter()
        .filter(|d| {
            (d.id == PHPDOC_UNDEFINED_METHOD_ID || d.id == CALL_UNDEFINED_METHOD_ID)
                && d.message.contains("declared receiver")
        })
        .collect()
}

/// [`family_with`] over a ready boot surface.
fn family(src: &str) -> Vec<Diagnostic> {
    family_with(src, &mut Boot::ready())
}

/// The conformance shape: `@param User|Guest`, `object` native, an `instanceof
/// User` branch, the else-branch `$value->name()` on the narrowed `{Guest}`. The
/// docblock arms are `Asserted`, so this shape stays on the contract id.
const CONFORMANCE_SHAPE: &str = "<?php
interface Named { public function name(): string; }
final class User implements Named { public function name(): string { return 'u'; } }
final class Guest { public function guestId(): int { return 1; } }
/** @param User|Guest $value */
function f(object $value): void {
    if ($value instanceof User) { echo $value->name(); return; }
    $value->name();
}
";


// Firing fixtures.


#[test]
fn fires_on_the_conformance_narrowing_shape() {
    // The `assertions_instanceof_narrowing` closure: the else branch narrows the
    // `@param User|Guest` down to `{Guest}` (final, no `name()`). Asserted premises
    // are coherent at the contract layer — this fires, and is the correct verdict
    // even though the runtime object might be neither (contract-conditional, §8).
    let d = fires(CONFORMANCE_SHAPE);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].line, 8, "fires on the else-branch call, not the guarded one");
    assert!(d[0].message.contains("Guest::name()"), "{}", d[0].message);
    assert!(d[0].message.contains("no __call"), "{}", d[0].message);
}

#[test]
fn fires_on_a_single_final_declared_arm() {
    // The minimal shape: a native `Guest` param (final, lacking `name()`). The lane
    // is `{Guest}` with no narrowing needed; the receiver is inexact (a param has no
    // heap object), so this is the declared-receiver lane's, not S2's — and the arm
    // is native, so A13 routes it to the proof-layer id.
    let d = promoted("<?php\nfinal class Guest { public function guestId(): int { return 1; } }\nfunction f(Guest $g): void { $g->name(); }\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("Guest::name()"), "{}", d[0].message);
}

#[test]
fn fires_across_a_final_arm_with_an_enumerated_parent_chain() {
    let d = promoted("<?php\nabstract class Person { public function greet(): void {} }\nfinal class Guest extends Person {}\nfunction f(Guest $g): void { $g->name(); }\n");
    assert_eq!(d.len(), 1, "{d:?}");
}

#[test]
fn fires_on_a_nonfinal_arm_with_a_fully_enumerated_clean_descendant_set() {
    // Base is non-final, so descendant closure must enumerate: Sub is-a Base but does
    // not add `tyop()`; the whole universe is resolvable and the dam is clear, so the
    // absence claim closes.
    let d = promoted("<?php\nclass Base {}\nclass Sub extends Base { public function other(): void {} }\nfunction f(Base $b): void { $b->tyop(); }\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("Base::tyop()"), "{}", d[0].message);
}

#[test]
fn fires_on_a_native_union_narrowed_to_one_final_arm() {
    // Native `User|Guest` (no phpdoc): the negative branch of `instanceof User`
    // deletes the User arm, leaving `{Guest}` (final, no `pay()`) — still native, so
    // still `Verified`.
    let d = promoted("<?php\nfinal class User { public function name(): string { return 'u'; } }\nfinal class Guest {}\nfunction f(User|Guest $v): void { if ($v instanceof User) { return; } $v->pay(); }\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("Guest::pay()"), "{}", d[0].message);
}

#[test]
fn fires_when_a_conditional_decl_in_chain_but_dam_is_clear() {
    // A2i: a conditional declaration re-dams only when the dam is *not* clear. With
    // no dynamism site, the dam is clear and the finding stands.
    let d = promoted("<?php\nif (true) { final class Guest { public function guestId(): int { return 1; } } }\nfunction f(Guest $g): void { $g->name(); }\n");
    assert_eq!(d.len(), 1, "{d:?}");
}


// Silence matrix: one fixture per ladder leg, asserted over BOTH routed ids.


#[test]
fn silent_when_a_descendant_defines_the_method() {
    // Base is non-final and Sub (a Base) declares `tyop()`: the runtime object typed
    // Base may be a Sub that answers the call. Silence (descendant introduces it).
    let d = family("<?php\nclass Base {}\nclass Sub extends Base { public function tyop(): void {} }\nfunction f(Base $b): void { $b->tyop(); }\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_an_anonymous_class_extending_the_member() {
    // A4: an anonymous class `extends Base` is invisible to the index — a
    // "completely enumerated" descendant set would miss it. Obstacle ⇒ silence.
    let d = family("<?php\nclass Base {}\nfunction make(): object { return new class extends Base { public function tyop(): void {} }; }\nfunction f(Base $b): void { $b->tyop(); }\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_when_a_descendant_has_an_unresolvable_ancestor() {
    // Sub extends an external, uncatalogued class: whether Sub is-a Base is Unknown,
    // so the descendant set cannot be completely enumerated. Obstacle ⇒ silence.
    let d = family("<?php\nclass Base {}\nclass Sub extends \\Vendor\\External { public function tyop(): void {} }\nfunction f(Base $b): void { $b->tyop(); }\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_when_the_dam_stands_via_a_conditional_decl() {
    // A2i + a dynamism site (`eval`) ⇒ the dam is not clear; a conditional Guest in
    // the chain re-dams the claim. Silence.
    let d = family("<?php\neval('$x = 1;');\nif (true) { final class Guest { public function guestId(): int { return 1; } } }\nfunction f(Guest $g): void { $g->name(); }\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_when_the_dam_stands_for_a_nonfinal_arm() {
    // A non-final arm's enumerated descendant set does not stop `eval` from minting a
    // fresh subclass with the method — the dam must be clear. Silence.
    let d = family("<?php\neval('$x = 1;');\nclass Base {}\nfunction f(Base $b): void { $b->tyop(); }\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_a_trait_in_the_members_chain() {
    // A trait adds methods the is-a oracle ignores — Unknown until flattening. Silence.
    let d = family("<?php\ntrait T { public function name(): string { return 't'; } }\nfinal class Guest { use T; }\nfunction f(Guest $g): void { $g->name(); }\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_a_magic_call_in_the_chain() {
    // `__call` swallows any name — no runtime error. Silence.
    let d = family("<?php\nfinal class Guest { public function __call($n, $a) {} }\nfunction f(Guest $g): void { $g->name(); }\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_an_enum_arm() {
    // A3: enum method bodies are not lowered, so an enum arm would read method-empty.
    // Unknown ⇒ silence.
    let d = family("<?php\nenum Suit { case Hearts; }\nfunction f(Suit $s): void { $s->name(); }\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_a_boot_surface_homonym() {
    // A2ii: the arm FQN is also a resident builtin/extension class-like — the textual
    // twin may be dead code shadowed by the loaded class. Silence.
    let d = family_with(
        "<?php\nfinal class Guest { public function guestId(): int { return 1; } }\nfunction f(Guest $g): void { $g->name(); }\n",
        &mut Boot::with_builtins(&["guest"]),
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_without_a_sidecar() {
    // A9 / A2ii's honest consequence: no live sidecar ⇒ the whole family is silent.
    let d = family_with(CONFORMANCE_SHAPE, &mut Boot::unavailable());
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_under_a_php_minor_skew_for_a_nonfinal_arm() {
    // A11: a PHP-minor skew from the catalog pin demotes descendant closure to
    // Unknown (a skewed catalog edge could fake a No-under-closure). Silence.
    let d = family_with(
        "<?php\nclass Base {}\nfunction f(Base $b): void { $b->tyop(); }\n",
        &mut Boot::with_minor((4, 0)),
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_when_the_method_is_present_on_the_arm() {
    // The declared type *has* the method — not undefined. Silence.
    let d = family("<?php\nfinal class Guest { public function name(): string { return 'g'; } }\nfunction f(Guest $g): void { $g->name(); }\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_when_an_arm_is_a_scalar() {
    // A `int|Guest` narrowed lane still carrying a scalar arm: the runtime receiver
    // may be a non-object, so a method-absence claim does not hold. Silence.
    let d = family("<?php\nfinal class Guest {}\nfunction f(int|Guest $v): void { $v->name(); }\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_a_nullsafe_call() {
    // Leg (l): `?->` is excluded in v1. Silence.
    let d = family("<?php\nfinal class Guest { public function guestId(): int { return 1; } }\nfunction f(Guest $g): void { $g?->name(); }\n");
    assert!(d.is_empty(), "{d:?}");
}


// Adversarial (audit G4): descendant-closure edge kinds.


#[test]
fn silent_on_an_interface_arm_with_an_enum_implementor() {
    // A4: an interface member's descendants include enum implementors. Enum method
    // bodies are unlowered (A3), so the enum is an obstacle ⇒ silence.
    let d = family("<?php\ninterface Shape { public function area(): int; }\nenum Suit: string implements Shape { case H = 'h'; public function area(): int { return 1; } }\n/** @param Shape $s */\nfunction f(Shape $s): void { $s->tyop(); }\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn descendant_via_a_class_alias_edge_is_detected() {
    // A4: parent matching follows a literal `class_alias` edge. A clean descendant
    // reached through the alias still lets the claim close (fire)...
    let clean = promoted("<?php\nclass Base {}\nclass_alias('Base', 'LegacyBase');\nclass Sub extends LegacyBase { public function other(): void {} }\nfunction f(Base $b): void { $b->tyop(); }\n");
    assert_eq!(clean.len(), 1, "clean alias-edge descendant should fire: {clean:?}");
    // ...while a descendant reached through the alias that *defines* the method
    // silences the claim (the descendant is genuinely detected, not blanket-missed).
    let defining = family("<?php\nclass Base {}\nclass_alias('Base', 'LegacyBase');\nclass Sub extends LegacyBase { public function tyop(): void {} }\nfunction f(Base $b): void { $b->tyop(); }\n");
    assert!(defining.is_empty(), "alias-edge descendant defining the method must silence: {defining:?}");
}

#[test]
fn silent_when_an_ambiguous_fqn_descendant_half_defines_the_method() {
    // A4: enumeration is over *declarations*, so both halves of an Ambiguous FQN
    // count as potential descendants — the half that declares the method silences.
    let d = family("<?php\nclass Base {}\nclass Sub extends Base {}\nclass Sub extends Base { public function tyop(): void {} }\nfunction f(Base $b): void { $b->tyop(); }\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn silent_on_a_lying_param_is_not_claimed_here_it_fires_correctly() {
    // A `@param User|Guest` whose runtime object is neither still fires under the
    // narrowed `{Guest}` — that is CORRECT for the contract layer (§8: the claim is
    // conditional on the declared contract, and a declared-contract violation is a
    // real finding even when the runtime value lies). Not an FP. What the lie can
    // never do is reach the PROOF id — that is `s6_routing.rs`'s pin.
    let d = fires(CONFORMANCE_SHAPE);
    assert_eq!(d.len(), 1, "the contract-conditional finding is correct, not an FP: {d:?}");
}


// Disjointness with S2.


#[test]
fn exact_receiver_is_s2s_emitter_never_both() {
    // A13 makes S2 and the promoted declared-receiver half share ONE id, so the
    // disjointness invariant is over SITES, not ids: a receiver is judged by exactly
    // one emitter. A `new Guest()` receiver is `class_exact` ⇒ S2's, and S2's
    // evidence never carries the declared-receiver phrasing.
    let src = "<?php\nfinal class Guest { public function guestId(): int { return 1; } }\nfunction f(): void { $g = new Guest(); $g->name(); }\n";
    let all = run_id(src, &mut Boot::ready(), CALL_UNDEFINED_METHOD_ID);
    assert_eq!(all.len(), 1, "exactly one finding at the site: {all:?}");
    assert!(
        !all[0].message.contains("declared receiver"),
        "the exact receiver is S2's emitter, not the declared-receiver lane's: {}",
        all[0].message
    );
    let s6 = run_id(src, &mut Boot::ready(), PHPDOC_UNDEFINED_METHOD_ID);
    assert!(s6.is_empty(), "the contract id must not fire on an exact receiver: {s6:?}");
}
