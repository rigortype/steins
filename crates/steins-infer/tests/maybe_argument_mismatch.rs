//! The argument side's possibly grade (ADR-0081's 2026-08-16 amendment, issue
//! #391): some arm of an argument's abstract fact is rejected by the native
//! parameter and some is accepted.
//!
//! One judgment, two ids, routed by the premise's minimum stratum (ADR-0052 §5):
//! `type.maybe-argument-mismatch` on an all-`Verified` premise,
//! `phpdoc.maybe-argument-mismatch` where any arm is `Asserted`. The stratum
//! routing is pinned from both sides, since a leg that only silenced the contract
//! id would look green while the promotion leaked past it.
//!
//! The **all**-arms-rejected verdict is not emitted at all — issue #291 measured
//! it empty on every public source — and that silence is pinned too, so a later
//! reader does not mistake it for a gap.
//!
//! Issue #418 extends the premise reader past `ArgValue::Var` to the other
//! carriers the propagated-argument checks already resolve a fact for: a
//! depth-1 property fetch (`$o->p`), a nested call/method call's return
//! (`g($x)`, `$b->m()`), and a shape-lane offset read (`$a['k']`). Each carrier
//! gets its own reporting/silent pair below, plus a stratum pin where that
//! carrier's own routing differs from the `Var` case.

use steins_infer::{
    Diagnostic, ID, PHPDOC_MAYBE_ARGUMENT_MISMATCH_ID, TYPE_MAYBE_ARGUMENT_MISMATCH_ID, check,
};
use steins_syntax::SourceTree;

fn run(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "t.php")
}

/// Every possibly-grade argument finding, whichever id the stratum routed it to.
fn family(src: &str) -> Vec<Diagnostic> {
    run(src)
        .into_iter()
        .filter(|d| {
            d.id == TYPE_MAYBE_ARGUMENT_MISMATCH_ID || d.id == PHPDOC_MAYBE_ARGUMENT_MISMATCH_ID
        })
        .collect()
}

fn proof(src: &str) -> Vec<Diagnostic> {
    run(src).into_iter().filter(|d| d.id == TYPE_MAYBE_ARGUMENT_MISMATCH_ID).collect()
}

fn contract(src: &str) -> Vec<Diagnostic> {
    run(src).into_iter().filter(|d| d.id == PHPDOC_MAYBE_ARGUMENT_MISMATCH_ID).collect()
}

const PRELUDE: &str = "function needString(string $s): void {}\nfunction needInt(int $n): void {}\n";

/// A `<?php` + `declare(strict_types=1)` source with the prelude in front.
fn strict(body: &str) -> String {
    format!("<?php\ndeclare(strict_types=1);\n{PRELUDE}{body}")
}

/// The same without the strict declaration — the coercive table.
fn coercive(body: &str) -> String {
    format!("<?php\n{PRELUDE}{body}")
}


// The headline: a native `T|false`/`T|null` into a native `T`


#[test]
fn a_verified_union_premise_fires_the_proof_id() {
    let src = strict("function f(string|false $x): void { needString($x); }\n");
    let d = proof(&src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(
        d[0].message,
        "argument $x to needString() may not become string $s — $x is string|false, \
         and its false arm raises a TypeError (strict mode)"
    );
    assert!(contract(&src).is_empty(), "a Verified premise never reaches the contract id");
}

#[test]
fn the_null_side_flag_is_an_arm_like_any_other() {
    let src = strict("function f(?string $x): void { needString($x); }\n");
    let d = proof(&src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("its null arm raises a TypeError"), "{}", d[0].message);
}

#[test]
fn a_method_argument_is_judged_too() {
    // The method-call twin, at that seam's own reach: a proven-exact receiver.
    // Widening the reach is `check_method_args`' question, not this id's.
    let src = strict(
        "class C { public function m(string $s): void {} }
function f(string|false $x): void { $c = new C(); $c->m($x); }\n",
    );
    let d = proof(&src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("C::m()"), "{}", d[0].message);
}

#[test]
fn a_constructor_argument_is_judged_too() {
    let src = strict(
        "class C { public function __construct(string $s) {} }
function f(string|false $x): void { new C($x); }\n",
    );
    let d = proof(&src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("C::__construct()"), "{}", d[0].message);
}


// Stratum routing (ADR-0052 §5): an Asserted arm never premises a `type.*` id


#[test]
fn an_asserted_premise_routes_to_the_contract_id_and_never_the_proof_one() {
    // The builtin-return shape the corpus is full of: `realpath()`'s declared
    // return reaches the arm lane, refined to `non-empty-string|false`, and the
    // refinement is `Asserted` (ADR-0069's floor grade), so the whole premise is.
    let src = strict("function f(string $p): void { $x = \\realpath($p); needString($x); }\n");
    let d = contract(&src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("its false arm"), "{}", d[0].message);
    assert!(
        proof(&src).is_empty(),
        "an Asserted arm must not premise `type.maybe-argument-mismatch` (ADR-0052 §5)"
    );
}

#[test]
fn a_docblock_only_premise_is_asserted_too() {
    let src = strict(
        "/** @param string|false $x */\nfunction f($x): void { needString($x); }\n",
    );
    assert_eq!(contract(&src).len(), 1);
    assert!(proof(&src).is_empty());
}

#[test]
fn the_message_names_the_arm_the_declaration_spells_not_the_base_it_lowers_to() {
    // `string|false` lowers to a `string|bool` union in the value lattice, and
    // reporting `bool` back at a reader whose declaration says `false` would be a
    // worse answer than the one the dump surface gives them.
    let src = strict("/** @param string|false $x */\nfunction f($x): void { needString($x); }\n");
    let d = contract(&src);
    assert!(d[0].message.contains("$x is string|false"), "{}", d[0].message);
    assert!(d[0].message.contains("its false arm"), "{}", d[0].message);
}


// The two verdicts that are not this id's


#[test]
fn the_all_arms_rejected_verdict_is_not_emitted() {
    // The definite No of issue #291: every arm rejected. Measured empty on every
    // public source and deliberately not built — silence here is the decision.
    let src = strict("function f(int|float $v): void { needString($v); }\n");
    assert!(family(&src).is_empty(), "{:?}", family(&src));
    assert!(run(&src).iter().all(|d| d.id != ID), "and no definite id either");
}

#[test]
fn a_fully_accepted_fact_is_silent() {
    let src = strict("function f(string $s): void { needString($s); }\n");
    assert!(family(&src).is_empty(), "{:?}", family(&src));
}

#[test]
fn a_finite_fact_stays_with_the_concrete_lane() {
    // `Singleton`/`OneOf` are `is_type_error`'s, and it judges them exactly. The
    // possibly grade never doubles a definite answer.
    let src = strict("function f(bool $b): void { $x = $b ? 'a' : 'b'; needString($x); }\n");
    assert!(family(&src).is_empty(), "{:?}", family(&src));
}

#[test]
fn a_guard_that_removes_the_rejected_arm_silences_it() {
    let src = strict(
        "function f(string|false $x): void { if ($x !== false) { needString($x); } }\n",
    );
    assert!(family(&src).is_empty(), "{:?}", family(&src));
}

#[test]
fn an_assert_silences_it_too() {
    // Issue #391's first repair, observed through this id: before it, the assert
    // left the `false` arm standing and this fired on guarded code.
    let src = strict(
        "function f(string|false $x): void { \\assert($x !== false && $x !== ''); needString($x); }\n",
    );
    assert!(family(&src).is_empty(), "{:?}", family(&src));
}

#[test]
fn an_implicitly_nullable_parameter_accepts_the_null_arm() {
    // Issue #391's second repair, observed through this id.
    let src = strict("function g(string $s = null): void {}\nfunction f(?string $x): void { g($x); }\n");
    assert!(family(&src).is_empty(), "{:?}", family(&src));
}


// The coercion mode is the call site's, and the two tables differ


#[test]
fn the_coercive_table_keeps_the_string_arm_alive() {
    // Coercive mode accepts a numeric string as an `int`, so the `string` arm is
    // NOT rejected — only the `null` one is, which is exactly the partial verdict.
    // The same source in strict mode rejects both arms and says nothing at all.
    let loose = coercive("function f(?string $x): void { needInt($x); }\n");
    let d = proof(&loose);
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.ends_with("(coercive mode)"), "{}", d[0].message);
    assert!(d[0].message.contains("its null arm"), "{}", d[0].message);

    let tight = strict("function f(?string $x): void { needInt($x); }\n");
    assert!(family(&tight).is_empty(), "strict mode rejects both arms — the definite verdict");
}


// Issue #418: the non-`Var` carriers — a property fetch


#[test]
fn a_property_fetch_is_judged_too() {
    // The corpus shape stays a builtin `T|false` — reached through an
    // intermediate variable, since a bare `$c->p = file_get_contents(...)` has
    // no fact to write today (`arg_abstract_fact` reads the value lane alone;
    // see the write-side note beside `apply_prop_assign`'s arm-lane fallback).
    let src = strict(
        "class C { public $p; }
function f(): void {
    $c = new C();
    $x = \\file_get_contents('/tmp/x');
    $c->p = $x;
    needString($c->p);
}\n",
    );
    // No declared-arm lane to spell from here — `PropFact` carries one `Fact`
    // and one stratum, never a list (see the write-side note above), so the
    // message falls back to the BASE spelling `to_fact` widened `false` to
    // (`bool`), the same fallback a value-lane `Var` premise takes.
    let d = contract(&src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("$c->p is string|bool"), "{}", d[0].message);
    assert!(d[0].message.contains("its bool arm"), "{}", d[0].message);
    assert!(
        proof(&src).is_empty(),
        "the builtin floor arm is Asserted (ADR-0069) — never `type.*`: {:?}",
        proof(&src)
    );
}

#[test]
fn a_property_fetch_of_a_plain_string_is_silent() {
    let src = strict(
        "class C { public $p; }
function f(string $s): void {
    $c = new C();
    $c->p = $s;
    needString($c->p);
}\n",
    );
    assert!(family(&src).is_empty(), "{:?}", family(&src));
}


// Issue #418: the non-`Var` carriers — a nested call / method call


#[test]
fn a_nested_call_result_is_judged_too() {
    // The callee's own body join is finite (`Fact::OneOf`, the concrete lane's
    // own — `is_type_error` already judges it exactly), so this reaches the
    // declared native return arms instead. `Verified`, since the return type is
    // a native hint, not a docblock claim.
    let src = strict(
        "function g(bool $ok): string|false { if ($ok) { return \"value\"; } return false; }
function f(bool $flag): void { needString(g($flag)); }\n",
    );
    let d = proof(&src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("g() is string|false"), "{}", d[0].message);
    assert!(d[0].message.contains("its false arm"), "{}", d[0].message);
}

#[test]
fn a_nested_call_result_of_a_plain_string_is_silent() {
    let src =
        strict("function g(): string { return 'x'; }\nfunction f(): void { needString(g()); }\n");
    assert!(family(&src).is_empty(), "{:?}", family(&src));
}

#[test]
fn a_nested_call_with_a_docblock_only_return_is_asserted() {
    // No native return hint at all — the only arms are the phpdoc's, `Asserted`
    // (ADR-0037 trust order), the same routing the builtin floor takes.
    let src = strict(
        "/**\n * @return string|false\n */\nfunction g(bool $ok) { if ($ok) { return \"value\"; } return false; }
function f(bool $flag): void { needString(g($flag)); }\n",
    );
    let d = contract(&src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(
        proof(&src).is_empty(),
        "an Asserted arm must not premise `type.maybe-argument-mismatch`: {:?}",
        proof(&src)
    );
}

#[test]
fn a_nested_method_call_result_is_judged_too() {
    let src = strict(
        "class C {
    public function m(bool $ok): string|false { if ($ok) { return \"value\"; } return false; }
}
function f(bool $flag): void { $c = new C(); needString($c->m($flag)); }\n",
    );
    let d = proof(&src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("$c->m() is string|false"), "{}", d[0].message);
}

#[test]
fn a_nested_method_call_of_a_plain_string_is_silent() {
    let src = strict(
        "class C { public function m(): string { return 'x'; } }
function f(): void { $c = new C(); needString($c->m()); }\n",
    );
    assert!(family(&src).is_empty(), "{:?}", family(&src));
}


// Issue #418: the non-`Var` carriers — a shape-lane offset read


#[test]
fn an_offset_read_is_judged_too() {
    // The shape lane's field fact for the key (`shape_read_at`, ADR-0062 §4 S3's
    // own resolver — a read and this judgment can never disagree about which
    // field they mean). A shape fact is seeded `Asserted` unconditionally
    // (A-G9's corollary), so this carrier premises only the contract id.
    let src = strict(
        "/**\n * @param array{k: string|false} $a\n */\nfunction f($a): void { needString($a['k']); }\n",
    );
    // The shape field's slot is a bare `Fact` too (no arm list riding along), so
    // this takes the same base-spelling fallback the prop carrier does.
    let d = contract(&src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("is string|bool"), "{}", d[0].message);
    assert!(d[0].message.contains("its bool arm"), "{}", d[0].message);
    assert!(
        proof(&src).is_empty(),
        "a shape fact is always Asserted — never `type.*`: {:?}",
        proof(&src)
    );
}

#[test]
fn an_offset_read_of_a_plain_string_field_is_silent() {
    let src = strict(
        "/**\n * @param array{k: string} $a\n */\nfunction f($a): void { needString($a['k']); }\n",
    );
    assert!(family(&src).is_empty(), "{:?}", family(&src));
}
