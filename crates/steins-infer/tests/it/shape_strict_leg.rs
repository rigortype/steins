//! ADR-0062 S6 — the offset family's **strict leg**: `offset.undeclared` and
//! `offset.maybe-missing` (A-G10, issue #51).
//!
//! Three disciplines pinned here, each a separate way the leg could go wrong:
//! **it fires** (an unguarded read of a declared-optional/excluded key produces
//! exactly one finding at the whitelisted read positions); **it stays quiet where a
//! proof exists** (guarded code is clean *by proof, not by skip* — S4 guard
//! promotion / S5 cover discharge delete the finding, so the guarded fixtures below
//! would fire if either regressed; issue #51 §3: a strict tier blind to guards
//! cries wolf); **it stays quiet where PHP protects the read** (every non-final
//! `??` arm, and every non-whitelisted position, is silent everywhere).

use steins_domain::Fact;
use steins_infer::{
    Diagnostic, Floor, Folder, OFFSET_MAYBE_MISSING_ID, OFFSET_UNDECLARED_ID, check_with, layer,
    surface_floor,
};
use steins_syntax::{ArgValue, SourceTree};

/// The absence-family boot surface (no real PHP in a unit test). The strict leg is
/// not gated on it, but the proof leg in the same walk is, so fixtures exercise both.
#[derive(Default)]
struct Mock;

impl Folder for Mock {
    fn fold(&mut self, _name: &str, _args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        true
    }
    fn builtin_return_fact(&mut self, _name: &str) -> Option<Fact> {
        None
    }
}

fn diagnostics(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "t.php", &mut Mock)
}

/// The strict-leg findings a source produces, as `(id, message)`, in emission order.
fn strict(src: &str) -> Vec<(&'static str, String)> {
    diagnostics(src)
        .into_iter()
        .filter(|d| d.id == OFFSET_UNDECLARED_ID || d.id == OFFSET_MAYBE_MISSING_ID)
        .map(|d| (d.id, d.message))
        .collect()
}

/// The strict-leg ids a source produces, in emission order.
fn ids(src: &str) -> Vec<&'static str> {
    strict(src).into_iter().map(|(id, _)| id).collect()
}

/// A one-function fixture: `@param <decl> $d`, body `<body>`.
fn fixture(decl: &str, body: &str) -> String {
    format!("<?php\n/** @param {decl} $d */\nfunction f(array $d): void {{ {body} }}\n")
}

/// The `array{a?: string, b?: string}` shape issue #51's examples are written over.
const AB: &str = "array{a?: string, b?: string}";

// Registry wiring (A-G10).

#[test]
fn the_strict_leg_floors_pin_the_post_triage_ruling() {
    // 2026-07-29 corpus sweep: `offset.undeclared` measured zero findings across
    // 99,522 files, promoted to `contracts`. `offset.maybe-missing` (3 findings, one
    // assertion-helper discharge gap) stayed at `strict` pending that discharge —
    // the discharge landing later does not itself move the floor.
    for (id, floor) in
        [(OFFSET_UNDECLARED_ID, Floor::Contracts), (OFFSET_MAYBE_MISSING_ID, Floor::Strict)]
    {
        assert_eq!(layer(id), Some(steins_infer::Layer::Contract), "{id} is contract-layer");
        assert_eq!(surface_floor(id), Some(floor), "{id} floor per the triage ruling");
    }
}

// It fires.

#[test]
fn an_unguarded_optional_read_fires_once() {
    let src = fixture("array{a?: string}", "$x = $d['a'];");
    assert_eq!(ids(&src), [OFFSET_MAYBE_MISSING_ID]);
}

#[test]
fn the_message_names_the_declared_shape_the_key_and_the_runtime_break() {
    let src = fixture("array{a?: string}", "$x = $d['a'];");
    let (_, msg) = strict(&src).into_iter().next().expect("one finding");
    assert_eq!(
        msg,
        "offset 'a' may be missing — $d is array{a?: string}, which declares the key optional, \
         and no guard on this path discharges it; reads null with \"Undefined array key \"a\"\""
    );
}

#[test]
fn a_key_outside_a_sealed_shape_fires_undeclared() {
    let src = fixture("array{a: string, b: string}", "$x = $d['zzz'];");
    assert_eq!(ids(&src), [OFFSET_UNDECLARED_ID]);
    let (_, msg) = strict(&src).into_iter().next().expect("one finding");
    assert_eq!(
        msg,
        "offset 'zzz' is outside the declared shape — $d is \
         array{a: string, b: string}, which cannot carry the key; reads null with \
         \"Undefined array key \"zzz\"\""
    );
}

#[test]
fn a_key_the_unsealed_tails_key_class_rejects_fires_undeclared() {
    // `array<string, int>` can't hold int key 3 — declared absence through the
    // tail's key class, not a field.
    assert_eq!(ids(&fixture("array<string, int>", "$x = $d[3];")), [OFFSET_UNDECLARED_ID]);
}

#[test]
fn the_return_operand_is_judged_like_the_assignment_rhs() {
    let src = "<?php\n/** @param array{a?: string} $d */\n\
               function f(array $d): string { return $d['a']; }\n";
    assert_eq!(ids(src), [OFFSET_MAYBE_MISSING_ID]);
}

// It stays quiet where a proof exists (the discharge ladder).

#[test]
fn a_required_key_is_clean() {
    assert!(ids(&fixture("array{a: string}", "$x = $d['a'];")).is_empty());
}

#[test]
fn an_isset_if_guard_discharges_the_read() {
    // L3: the guard promotes the key to Required on the true branch — clean by proof.
    let src = fixture("array{a?: string}", "if (isset($d['a'])) { $x = $d['a']; }");
    assert!(ids(&src).is_empty(), "isset-guarded read must be clean: {:?}", strict(&src));
}

#[test]
fn an_array_key_exists_guard_discharges_the_read() {
    let src = fixture("array{a?: string}", "if (array_key_exists('a', $d)) { $x = $d['a']; }");
    assert!(ids(&src).is_empty(), "array_key_exists-guarded read must be clean: {:?}", strict(&src));
}

#[test]
fn a_not_empty_guard_discharges_the_read() {
    // `empty(e)` lowers to `!isset(e) || !e`, so its false branch is `isset(e) && e`
    // and the presence promotion is the `isset` half's (full polarity table in
    // `tests/shape_guards.rs`).
    let src = fixture("array{a?: string}", "if (!empty($d['a'])) { $x = $d['a']; }");
    assert!(ids(&src).is_empty(), "!empty-guarded read must be clean: {:?}", strict(&src));
}

#[test]
fn an_empty_guarded_read_still_fires_on_the_true_branch() {
    // `empty($d['a'])` true leaves "absent" and "present-falsy" both open — discharges
    // nothing, the honest reading the corpus needs.
    let src = fixture("array{a?: string}", "if (empty($d['a'])) { $x = $d['a']; }");
    assert_eq!(ids(&src), [OFFSET_MAYBE_MISSING_ID]);
}

#[test]
fn a_tagged_assertion_helper_discharges_the_read() {
    // The entire 2026-07-29-sweep `offset.maybe-missing` residue was this shape
    // (own suite: `tests/assert_helper_discharge.rs`; this is the ladder-level pin).
    let src = format!(
        "<?php\nfinal class H {{\n\
         /** @phpstan-assert true $c */\n\
         public static function t(bool $c): void {{ if (!$c) {{ throw new \\RuntimeException('x'); }} }}\n\
         }}\n\
         /** @param {AB} $d */\n\
         function f(array $d): void {{ H::t(isset($d['a'])); $x = $d['a']; }}\n"
    );
    assert!(ids(&src).is_empty(), "helper-guarded read must be clean: {:?}", strict(&src));
}

#[test]
fn a_read_outside_the_guarded_branch_still_fires() {
    // The guard's proof is branch-scoped; silence here would mean the narrowing leaked.
    let src = fixture("array{a?: string}", "if (isset($d['a'])) { $x = $d['a']; } $y = $d['a'];");
    assert_eq!(ids(&src), [OFFSET_MAYBE_MISSING_ID]);
}

// The `??` split (issue #51 §2).

#[test]
fn a_non_final_coalesce_arm_never_fires() {
    // PHP protects exactly the arms it may fall through; `$d['a']` is one of them.
    let src = fixture(AB, "$x = $d['a'] ?? 'fallback';");
    assert!(ids(&src).is_empty(), "a protected arm must be silent: {:?}", strict(&src));
}

#[test]
fn the_final_coalesce_arm_is_a_plain_read_and_fires() {
    // issue #51's opening example without the assert — nothing proves `$d['b']`.
    let src = fixture(AB, "$x = $d['a'] ?? $d['b'];");
    assert_eq!(ids(&src), [OFFSET_MAYBE_MISSING_ID]);
    let (_, msg) = strict(&src).into_iter().next().expect("one finding");
    assert!(msg.starts_with("offset 'b' may be missing on the final `??` arm"), "got: {msg}");
}

#[test]
fn exactly_one_finding_on_a_three_arm_chain() {
    // Two protected arms, one judged: the count is the discipline.
    let src = fixture(
        "array{a?: string, b?: string, c?: string}",
        "$x = $d['a'] ?? $d['b'] ?? $d['c'];",
    );
    assert_eq!(ids(&src), [OFFSET_MAYBE_MISSING_ID]);
    assert!(strict(&src)[0].1.contains("offset 'c'"), "the RIGHT-MOST arm is the judged one");
}

#[test]
fn the_disjunctive_assert_pattern_proves_the_final_arm_clean() {
    // L4 + L5, issue #51's headline: the assert records the cover, `??` supplies
    // `¬isset($d['a'])`, together proving `$d['b']` present — clean by PROOF, which
    // the tiers Steins is imported from cannot do.
    let src = fixture(AB, "assert(isset($d['a']) || isset($d['b'])); $x = $d['a'] ?? $d['b'];");
    assert!(ids(&src).is_empty(), "the discharged pattern must be clean: {:?}", strict(&src));
}

#[test]
fn an_opaque_premise_leaves_the_final_arm_undischarged() {
    // By design (issue #51's closing note): no cover recorded, one honest warning.
    let src = fixture(AB, "$x = $d['a'] ?? $d['b'];");
    assert_eq!(ids(&src), [OFFSET_MAYBE_MISSING_ID]);
}

#[test]
fn a_call_arm_invalidates_the_ladder_and_the_final_arm_fires() {
    // A-G11 conservatism: a call may write through a reference, so the accumulated
    // `¬isset` goes stale and the cover no longer discharges.
    let src = fixture(
        AB,
        "assert(isset($d['a']) || isset($d['b'])); $x = $d['a'] ?? g() ?? $d['b'];",
    );
    assert_eq!(ids(&src), [OFFSET_MAYBE_MISSING_ID]);
}

#[test]
fn a_settled_earlier_arm_makes_the_final_arm_dead_and_silent() {
    // `$d['a']` is Required and non-null, so `??` never evaluates its right side —
    // judging the dead arm would be a wolf cry.
    let src = fixture("array{a: string, b?: string}", "$x = $d['a'] ?? $d['b'];");
    assert!(ids(&src).is_empty(), "an unreachable arm must be silent: {:?}", strict(&src));
}

#[test]
fn a_declared_absence_on_the_final_arm_fires_undeclared() {
    let src = fixture("array{a?: string}", "$x = $d['a'] ?? $d['zzz'];");
    assert_eq!(ids(&src), [OFFSET_UNDECLARED_ID]);
}

// v1 scope and the silence contexts.

#[test]
fn a_general_map_read_does_not_fire() {
    // A-G10's v1 emission scope: `ShapeRead::Tail` is silent — a general map/list
    // leg is a separate future id.
    assert!(ids(&fixture("array<string, int>", "$x = $d['k'];")).is_empty());
    assert!(ids(&fixture("array", "$x = $d['k'];")).is_empty());
    assert!(ids(&fixture("list<string>", "$x = $d[0];")).is_empty());
}

#[test]
fn the_silence_contexts_stay_silent() {
    // None lowers into a whitelisted value slot, so none is judged (same A7
    // whitelist the proof leg rides).
    for body in [
        "if (isset($d['a'])) { return; }",
        "$b = array_key_exists('a', $d);",
        "unset($d['a']);",
        "$d['a'] = 'w';",
        "$arr = [$d['a']];",
    ] {
        let src = fixture(AB, body);
        assert!(ids(&src).is_empty(), "`{body}` must be silent: {:?}", strict(&src));
    }
}

#[test]
fn a_nullable_base_declines_the_judgment() {
    // The base may be null, so no field is guaranteed and no claim is made.
    let src = "<?php\n/** @param array{a?: string}|null $d */\n\
               function f(?array $d): void { $x = $d['a']; }\n";
    assert!(ids(src).is_empty(), "a nullable base must decline: {:?}", strict(src));
}

#[test]
fn a_dynamic_key_declines_the_judgment() {
    // Only constant / env-resolved keys are in v1 scope.
    let src = "<?php\n/** @param array{a?: string} $d */\n\
               function f(array $d, string $k): void { $x = $d[$k]; }\n";
    assert!(ids(src).is_empty(), "an unproven key must decline: {:?}", strict(src));
}

// The four issue-motivating patterns, end to end.

/// issue #51's worked example: `works` is the discharged disjunctive assert;
/// `fail1`/`fail2` are guarded by a single `isset`; `fail3` is the undischarged chain.
#[test]
fn the_four_issue_patterns_land_as_specified() {
    let works = fixture(AB, "assert(isset($d['a']) || isset($d['b'])); $x = $d['a'] ?? $d['b'];");
    let fail1 = fixture(AB, "assert(isset($d['a'])); $x = $d['a'];");
    let fail2 = fixture(AB, "if (isset($d['b'])) { $x = $d['b']; }");
    let fail3 = fixture(AB, "$x = $d['a'] ?? $d['b'];");

    assert!(ids(&works).is_empty(), "works: {:?}", strict(&works));
    assert!(ids(&fail1).is_empty(), "fail1: {:?}", strict(&fail1));
    assert!(ids(&fail2).is_empty(), "fail2: {:?}", strict(&fail2));
    assert_eq!(ids(&fail3), [OFFSET_MAYBE_MISSING_ID], "fail3 fires exactly once, on the final arm");
}

// One finding per site.

#[test]
fn the_two_legs_never_double_report_one_site() {
    // Proof leg takes `Verified` operands, strict leg an `Asserted` shape — disjoint
    // by construction.
    let concrete = "<?php\nfunction g(): void { $a = ['a' => 1]; $x = $a['nope']; }\n";
    let ds: Vec<&str> = diagnostics(concrete)
        .iter()
        .filter(|d| layer(d.id) != Some(steins_infer::Layer::Debug))
        .map(|d| d.id)
        .collect();
    assert_eq!(ds, ["offset.missing"], "one site, one finding");
}
