//! The non-object receiver family — `call.on-non-object` / `property.on-non-object`
//! (ADR-0078, issue #190).
//!
//! Both ids consume the fact `call.on-null` already consumes: the four-layer value
//! domain's proven receiver. `Val` has no object variant and `Base` no object base,
//! so a fact whose denotation is a single named type is already a proof that the
//! value is **not** an object; a receiver that might be one simply has no fact, and
//! that absence is where every `Maybe`, object-arm union and unknown-class receiver
//! lands. The silence fixtures below pin that.
//!
//! Every runtime claim is `php -r`-witnessed on PHP 8.5.9, one scalar kind per test
//! below: a call fatals with `Call to a member function m() on <type>`; a property
//! read warns `Attempt to read property "p" on <type>` and yields `NULL` — the same
//! shape for `int`/`string`/`float`/`true`/`false`/`array`/`null` in place of `int`.

use steins_infer::{
    CALL_ON_NON_OBJECT_ID, CALL_ON_NULL_ID, Diagnostic, Folder, PROPERTY_ON_NON_OBJECT_ID, check,
    check_full,
};
use steins_syntax::{ArgValue, SourceTree};

/// A folder that never folds and answers no boot surface. Neither id in this family
/// needs a sidecar — the evidence is the value domain's own — so this stands in for
/// the runtime wherever a `warning-handler` posture must be chosen.
struct Plain;

impl Folder for Plain {
    fn fold(&mut self, _name: &str, _args: &[ArgValue]) -> Option<ArgValue> {
        None
    }
}

fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php")
}

fn on_non_object(src: &str) -> Vec<Diagnostic> {
    findings(src).into_iter().filter(|d| d.id == CALL_ON_NON_OBJECT_ID).collect()
}

fn on_null(src: &str) -> Vec<Diagnostic> {
    findings(src).into_iter().filter(|d| d.id == CALL_ON_NULL_ID).collect()
}

/// The property id under an explicit `warning-handler` posture (`true` = the default
/// `"abort"`, `false` = a declared `"null"`).
fn prop_on_non_object_posture(src: &str, warning_handler_abort: bool) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_full(&tree, "test.php", &mut Plain, warning_handler_abort)
        .into_iter()
        .filter(|d| d.id == PROPERTY_ON_NON_OBJECT_ID)
        .collect()
}

fn prop_on_non_object(src: &str) -> Vec<Diagnostic> {
    prop_on_non_object_posture(src, true)
}

/// A one-function fixture whose local `$x` is assigned `value`, then used as `usage`.
fn scope(value: &str, usage: &str) -> String {
    format!("<?php\nfunction f(): void {{\n    $x = {value};\n    {usage}\n}}\n")
}

// --- call.on-non-object: one fixture per proven scalar kind -----------------

#[test]
fn call_on_proven_int_fires() {
    let d = on_non_object(&scope("1", "$x->m();"));
    assert_eq!(d.len(), 1, "a proven int receiver is a fatal method call: {d:#?}");
    assert!(d[0].message.contains("int"), "the message names the receiver's type: {d:#?}");
}

#[test]
fn call_on_proven_string_fires() {
    assert_eq!(on_non_object(&scope("'s'", "$x->m();")).len(), 1);
}

#[test]
fn call_on_proven_float_fires() {
    assert_eq!(on_non_object(&scope("1.5", "$x->m();")).len(), 1);
}

#[test]
fn call_on_proven_bool_fires() {
    // `true` and `false` are the same id here; the finding names the *type*, `bool`.
    assert_eq!(on_non_object(&scope("true", "$x->m();")).len(), 1);
    assert_eq!(on_non_object(&scope("false", "$x->m();")).len(), 1);
}

#[test]
fn call_on_proven_array_fires() {
    assert_eq!(on_non_object(&scope("[]", "$x->m();")).len(), 1);
    assert_eq!(on_non_object(&scope("[1, 2]", "$x->m();")).len(), 1);
}

#[test]
fn call_on_native_seeded_scalar_param_fires() {
    // The receiver need not be a literal: a native `string` parameter is a
    // runtime-enforced entry fact (`Fact::General`), proving non-objecthood too.
    let src = "<?php\nfunction f(string $s): void { $s->m(); }\n";
    assert_eq!(on_non_object(src).len(), 1, "a native scalar seed is a Verified fact");
}

#[test]
fn call_on_proven_prop_receiver_fires() {
    // The depth-1 `$v->prop->m()` receiver lane `call.on-null` already reads
    // (ADR-0052 §7): the heap property fact proves the same about what it holds.
    let src = "<?php
class C { public int $n = 1; }
function f(): void { $o = new C(); $o->n->m(); }
";
    assert_eq!(on_non_object(src).len(), 1, "a proven int property receiver fires");
}

// --- the null case stays call.on-null's ------------------------------------

#[test]
fn null_receiver_stays_call_on_null_with_its_own_message() {
    // ADR-0022 id stability: `call.on-null` keeps the null case, its id and its
    // sentence. The sibling must not also fire — the two are disjoint at every site.
    let src = scope("null", "$x->m();");
    let null_findings = on_null(&src);
    assert_eq!(null_findings.len(), 1, "the null receiver is still call.on-null");
    assert_eq!(
        null_findings[0].message,
        "method call $x->m() — $x is proven null on this path — proven Error (Call to a member function on null)",
        "the null case's message is unchanged"
    );
    assert!(on_non_object(&src).is_empty(), "the sibling must not double-report the null case");
}

// --- the nullsafe pair ------------------------------------------------------

#[test]
fn nullsafe_on_null_stays_silent() {
    // Nullsafe is the legal form for a null receiver; neither id may fire.
    let src = scope("null", "$x?->m();");
    assert!(on_null(&src).is_empty(), "?-> on null is legal PHP");
    assert!(on_non_object(&src).is_empty(), "?-> on null is legal PHP");
}

#[test]
fn nullsafe_on_proven_non_object_still_fires() {
    // `?->` short-circuits on null ALONE, so a proven non-null non-object still fatals.
    assert_eq!(
        on_non_object(&scope("1", "$x?->m();")).len(),
        1,
        "?-> does not excuse a proven int receiver"
    );
}

// --- silence: only a DEFINITE non-object fires ------------------------------

#[test]
fn maybe_object_receiver_is_silent() {
    // A parameter that may be an object carries no value-domain fact (the domain
    // can't spell an object), so the receiver is unknown and both ids are silent.
    let src = "<?php
class C { public function m(): void {} }
function f(?C $c): void { $c->m(); }
";
    assert!(on_non_object(src).is_empty(), "a maybe-object receiver is silence");
    assert!(on_null(src).is_empty(), "…and it is not the null case either");
}

#[test]
fn union_with_an_object_arm_is_silent() {
    // `int|C` is not representable as a fact (the union has an object arm), so the
    // receiver has none and the call is silence — even though the `int` arm alone
    // would fatal.
    let src = "<?php
class C { public function m(): void {} }
function f(int|C $v): void { $v->m(); }
";
    assert!(on_non_object(src).is_empty(), "a union with an object arm is silence");
}

#[test]
fn unknown_class_receiver_is_silent() {
    // A receiver of a class Steins cannot resolve is an object as far as the value
    // domain is concerned: no fact, no finding.
    let src = "<?php
function f(\\Some\\Absent\\Thing $t): void { $t->m(); }
";
    assert!(on_non_object(src).is_empty(), "an unknown-class receiver is silence");
}

#[test]
fn nullable_scalar_receiver_is_silent() {
    // `?int` is a non-object under BOTH arms, so PHP fatals either way — but the
    // fact is `General { base: Int, nullable: true }`, with no single type to name
    // (nor a settled owner between the two ids): silence, not a wrong sentence.
    let src = "<?php\nfunction f(?int $n): void { $n->m(); }\n";
    assert!(on_non_object(src).is_empty(), "a nullable scalar receiver is this slice's boundary");
    assert!(on_null(src).is_empty(), "…and it is not proven null either");
}

#[test]
fn object_receiver_is_silent() {
    // The control: a proven object receiver has an ObjRef binding and no fact.
    let src = "<?php
class C { public function m(): void {} }
function f(): void { $o = new C(); $o->m(); }
";
    assert!(on_non_object(src).is_empty(), "a proven object receiver must never fire");
}

#[test]
fn asserted_scalar_receiver_cannot_premise_the_proof() {
    // ADR-0052 §5: a docblock CLAIM narrows at the `Asserted` stratum and buys
    // silence, but can never forge a proof-layer fatal.
    let src = "<?php
/** @phpstan-assert int $x */
function claimInt($x): void {}
function f(mixed $x): void { claimInt($x); $x->m(); }
";
    assert!(on_non_object(src).is_empty(), "an Asserted int receiver must not premise the proof");
}

// --- property.on-non-object -------------------------------------------------

#[test]
fn property_on_proven_int_fires() {
    let d = prop_on_non_object(&scope("1", "$y = $x->p;"));
    assert_eq!(d.len(), 1, "a proven int receiver warns on a property read: {d:#?}");
    assert!(d[0].message.contains("int"), "the message names the receiver's type: {d:#?}");
}

#[test]
fn property_on_proven_string_fires() {
    assert_eq!(prop_on_non_object(&scope("'s'", "$y = $x->p;")).len(), 1);
}

#[test]
fn property_on_proven_float_fires() {
    assert_eq!(prop_on_non_object(&scope("1.5", "$y = $x->p;")).len(), 1);
}

#[test]
fn property_on_proven_bool_fires() {
    assert_eq!(prop_on_non_object(&scope("true", "$y = $x->p;")).len(), 1);
    assert_eq!(prop_on_non_object(&scope("false", "$y = $x->p;")).len(), 1);
}

#[test]
fn property_on_proven_array_fires() {
    assert_eq!(prop_on_non_object(&scope("[]", "$y = $x->p;")).len(), 1);
}

#[test]
fn property_on_proven_null_fires() {
    // Unlike the call side, the property side OWNS the null receiver: PHP raises the
    // very same warning (`Attempt to read property "p" on null`) and there is no
    // `property.on-null` in the ADR-0078 table to defer to.
    assert_eq!(
        prop_on_non_object(&scope("null", "$y = $x->p;")).len(),
        1,
        "a proven null receiver is a non-object property fetch"
    );
}

#[test]
fn property_at_the_return_position_fires() {
    // The second whitelisted read position (the offset family's A7 pair).
    let src = "<?php\nfunction f(): mixed { $x = 1; return $x->p; }\n";
    assert_eq!(prop_on_non_object(src).len(), 1, "a return operand is a whitelisted read");
}

#[test]
fn property_on_maybe_object_is_silent() {
    let src = "<?php
class C { public int $p = 1; }
function f(?C $c): void { $y = $c->p; }
";
    assert!(prop_on_non_object(src).is_empty(), "a maybe-object receiver is silence");
}

#[test]
fn property_on_object_is_silent() {
    let src = "<?php
class C { public int $p = 1; }
function f(): void { $o = new C(); $y = $o->p; }
";
    assert!(prop_on_non_object(src).is_empty(), "a proven object receiver must never fire");
}

#[test]
fn property_nullsafe_fetch_is_the_recorded_silence() {
    // `$x?->p` DOES warn at runtime, but lowers to an opaque value rather than a
    // property fetch, so it never reaches this check — a recorded reach boundary,
    // not correct behaviour.
    assert!(
        prop_on_non_object(&scope("1", "$y = $x?->p;")).is_empty(),
        "the nullsafe property form is outside the lowered read lane"
    );
}

// --- the warning-handler gate, both postures (ADR-0049 §7) ------------------

#[test]
fn property_warning_handler_abort_emits() {
    let src = scope("1", "$y = $x->p;");
    assert_eq!(
        prop_on_non_object_posture(&src, true).len(),
        1,
        "the default \"abort\" posture emits the warning-grade finding"
    );
}

#[test]
fn property_warning_handler_null_silences() {
    // Under `warning-handler = "null"` the application tolerates the warning, so the
    // warning-grade finding leaves the proof surface — exactly as `offset.missing`
    // does.
    let src = scope("1", "$y = $x->p;");
    assert!(
        prop_on_non_object_posture(&src, false).is_empty(),
        "\"null\" posture silences the warning-grade property finding"
    );
}

#[test]
fn call_id_is_not_warning_gated() {
    // The call side is a FATAL, so no posture demotes it — the gate boundary and the
    // id boundary coincide (ADR-0078 §1 point 4).
    let tree = SourceTree::parse(&scope("1", "$x->m();"));
    let d: Vec<Diagnostic> = check_full(&tree, "test.php", &mut Plain, false)
        .into_iter()
        .filter(|d| d.id == CALL_ON_NON_OBJECT_ID)
        .collect();
    assert_eq!(d.len(), 1, "a fatal is never demoted by the warning-handler posture");
}
