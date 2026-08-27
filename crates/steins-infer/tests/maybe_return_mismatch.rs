//! The return side's possibly grade (ADR-0081 §8's 2026-08-27 amendment, issue
//! #537): some arm of the returned variable's abstract fact is rejected by the
//! enclosing function's native return type and some is accepted.
//!
//! One judgment, two ids, routed by the premise's minimum stratum (ADR-0052 §5):
//! `type.maybe-return-mismatch` on an all-`Verified` premise,
//! `phpdoc.maybe-return-mismatch` where any arm is `Asserted`. Both directions are
//! pinned, since a leg that only silenced the contract id would look green while
//! the promotion leaked past it — the same discipline `maybe_argument_mismatch.rs`
//! keeps for the argument pair.
//!
//! The **all**-arms-rejected verdict is not emitted here either, and neither is a
//! position with an arm the judgment cannot read: "some rejected, some accepted" is
//! a claim about the whole arm list, so a list with a hole in it supports neither
//! half. Both silences are pinned below.

use steins_infer::{
    Diagnostic, PHPDOC_MAYBE_RETURN_MISMATCH_ID, RETURN_ID, TYPE_MAYBE_RETURN_MISMATCH_ID, check,
};
use steins_syntax::SourceTree;

fn run(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "t.php")
}

/// Every possibly-grade return finding, whichever id the stratum routed it to.
fn family(src: &str) -> Vec<Diagnostic> {
    run(src)
        .into_iter()
        .filter(|d| {
            d.id == TYPE_MAYBE_RETURN_MISMATCH_ID || d.id == PHPDOC_MAYBE_RETURN_MISMATCH_ID
        })
        .collect()
}

fn proof(src: &str) -> Vec<Diagnostic> {
    run(src).into_iter().filter(|d| d.id == TYPE_MAYBE_RETURN_MISMATCH_ID).collect()
}

fn contract(src: &str) -> Vec<Diagnostic> {
    run(src).into_iter().filter(|d| d.id == PHPDOC_MAYBE_RETURN_MISMATCH_ID).collect()
}

const FINALS: &str = "final class A {}\nfinal class B {}\n";

/// A `<?php` + `declare(strict_types=1)` source.
fn strict(body: &str) -> String {
    format!("<?php\ndeclare(strict_types=1);\n{body}")
}

/// The same without the strict declaration — the coercive table.
fn coercive(body: &str) -> String {
    format!("<?php\n{body}")
}


// The issue's own repro: an object union returned into one of its own arms


#[test]
fn the_still_ab_repro_reports() {
    // Issue #537's headline, and php-typing-conformance's `stillAB` /
    // `stillUnion` scored line. Silent at every profile before this id existed:
    // the value domain is object-free, so `A|B` lives only in the declared-arm
    // lane and no scalar judgment could ever see it.
    let src = strict(&format!("{FINALS}function stillAB(A|B $x): B\n{{\n    return $x;\n}}\n"));
    let d = proof(&src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(
        d[0].message,
        "return value $x may not become B (return type of stillAB()) — $x is A|B, \
         and its A arm raises a TypeError (strict mode)"
    );
    assert!(contract(&src).is_empty(), "a native declaration is Verified on every arm");
}

#[test]
fn a_guard_that_removes_the_rejected_arm_silences_it() {
    // What #538/#539 will do for the guarded conformance siblings, shown here with
    // the narrowing that already exists.
    let src = strict(&format!(
        "{FINALS}function f(A|B $x): B {{ if ($x instanceof B) {{ return $x; }} throw new \\LogicException('no'); }}\n"
    ));
    assert!(family(&src).is_empty(), "{:?}", family(&src));
}

#[test]
fn a_method_return_is_judged_too() {
    let src =
        strict(&format!("{FINALS}class C {{ public function m(A|B $x): B {{ return $x; }} }}\n"));
    let d = proof(&src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("return type of C::m()"), "{}", d[0].message);
}

#[test]
fn a_closure_return_is_judged_too() {
    let src = strict(&format!("{FINALS}$f = function (A|B $x): B {{ return $x; }};\n"));
    assert_eq!(proof(&src).len(), 1, "{:?}", proof(&src));
}

#[test]
fn a_nullable_object_return_still_reports_the_rejected_class_arm() {
    // `?B` accepts the `null` PHP would refuse for a bare `B`, and nothing else —
    // the `A` arm is untouched by the question mark.
    let src = strict(&format!("{FINALS}function f(A|B $x): ?B {{ return $x; }}\n"));
    let d = proof(&src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("may not become ?B"), "{}", d[0].message);
}

#[test]
fn an_interface_return_reports_a_final_arm_that_cannot_implement_it() {
    let src = strict(
        "interface I {}\nfinal class A {}\nfinal class B implements I {}\nfunction f(A|B $x): I { return $x; }\n",
    );
    assert_eq!(proof(&src).len(), 1, "{:?}", proof(&src));
}

#[test]
fn a_class_arm_that_can_have_a_subclass_stays_silent() {
    // The soundness gate, and the reason a class arm is not simply handed to the
    // exact-class oracle: `class A` is extensible, and a subclass of it may
    // implement `I`, so no value of the `A` arm is provably refused. Convicting
    // this would be a false positive no floor makes safe (ADR-0002).
    let src = strict(
        "interface I {}\nclass A {}\nfinal class B implements I {}\nfunction f(A|B $x): I { return $x; }\n",
    );
    assert!(family(&src).is_empty(), "{:?}", family(&src));
}

#[test]
fn an_enum_arm_is_judged_by_its_cases() {
    // An enum-typed declaration seeds one arm per case (issue #429), and an enum
    // case is an object of an implicitly-final class — so every case arm is
    // decidable and the message names them the way the dump surface does.
    let src = strict(
        "enum E { case X; case Y; }\nfinal class B {}\nfunction f(E|B $x): B { return $x; }\n",
    );
    let d = proof(&src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("E::X and E::Y arms raise"), "{}", d[0].message);
}


// The scalar half: the argument pair's own shapes, at the return seam


#[test]
fn a_verified_scalar_union_premise_fires_the_proof_id() {
    let src = strict("function f(string|false $x): string { return $x; }\n");
    let d = proof(&src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(
        d[0].message,
        "return value $x may not become string (return type of f()) — $x is string|false, \
         and its false arm raises a TypeError (strict mode)"
    );
}

#[test]
fn the_null_side_flag_is_an_arm_like_any_other() {
    let src = strict("function f(?string $x): string { return $x; }\n");
    let d = proof(&src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("its null arm raises a TypeError"), "{}", d[0].message);
}


// Stratum routing (ADR-0052 §5): an Asserted arm never premises a `type.*` id


#[test]
fn a_docblock_only_premise_routes_to_the_contract_id_and_never_the_proof_one() {
    let src = strict("/** @param string|false $x */\nfunction f($x): string { return $x; }\n");
    let d = contract(&src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("its false arm"), "{}", d[0].message);
    assert!(
        proof(&src).is_empty(),
        "an Asserted arm must not premise `type.maybe-return-mismatch` (ADR-0052 §5)"
    );
}

#[test]
fn a_builtin_declared_return_floor_is_asserted_too() {
    // ADR-0069's declared floor: `realpath()`'s `non-empty-string|false` reaches
    // the arm lane at `Asserted`, so the whole premise is.
    let src = strict("function f(string $p): string { $v = \\realpath($p); return $v; }\n");
    let d = contract(&src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("$v is non-empty-string|false"), "{}", d[0].message);
    assert!(proof(&src).is_empty(), "{:?}", proof(&src));
}


// The verdicts that are not this id's


#[test]
fn the_all_arms_rejected_verdict_is_not_emitted() {
    let src = strict(&format!("{FINALS}final class C {{}}\nfunction f(A|B $x): C {{ return $x; }}\n"));
    assert!(family(&src).is_empty(), "{:?}", family(&src));
}

#[test]
fn a_fully_accepted_union_is_silent() {
    let src = strict(&format!("{FINALS}function f(A|B $x): A|B {{ return $x; }}\n"));
    assert!(family(&src).is_empty(), "{:?}", family(&src));
}

#[test]
fn an_arm_the_judgment_cannot_read_silences_the_whole_position() {
    // `float` has no faithful value-domain reading (the domain's `float` accepts
    // ints), so `steins_contract::to_fact` refuses the spelling. Reading that
    // refusal as acceptance would report "some arm is rejected" about an arm list
    // this judgment has not finished reading — the argument side stays silent on
    // the same shape, and so does this.
    let src = strict("function f(int|float $x): string { return $x; }\n");
    assert!(family(&src).is_empty(), "{:?}", family(&src));
}

#[test]
fn the_definite_native_proof_shadows_its_weaker_sibling() {
    // Placement pin: the possibly grade runs only where `type.return-mismatch` did
    // not fire, so one `return` never carries both claims.
    let src = strict("function f(): string { $v = 1; return $v; }\n");
    assert_eq!(run(&src).iter().filter(|d| d.id == RETURN_ID).count(), 1);
    assert!(family(&src).is_empty(), "{:?}", family(&src));
}

#[test]
fn a_generator_is_not_judged() {
    // A generator's declared return names the `Generator` the CALL yields, not the
    // values of in-body `return` — the guard `Cx::scope_return` has carried since
    // issue #128, inherited here for free.
    let src = strict(&format!("{FINALS}function f(A|B $x): B {{ yield 1; return $x; }}\n"));
    assert!(family(&src).is_empty(), "{:?}", family(&src));
}

#[test]
fn void_and_never_returns_are_silent() {
    // Neither spelling lowers to a `NativeType` at all (`lower_hint` refuses
    // both), so there is no envelope to judge against — stated as a pin rather
    // than left to be rediscovered.
    let void = strict("function f(string $x): void { return; }\n");
    assert!(family(&void).is_empty(), "{:?}", family(&void));
    let never = strict("function f(string|false $x): never { return $x; }\n");
    assert!(family(&never).is_empty(), "{:?}", family(&never));
}

#[test]
fn a_non_var_carrier_is_not_read_yet() {
    // The slice's stated bound: `return $x;` is the carrier. A nested call's
    // return (`return g();`) is issue #418's seam on the argument side and needs
    // that seam's guard-decline surface (issue #421) before it can ship here.
    let src = strict(
        "function g(bool $ok): string|false { if ($ok) { return \"v\"; } return false; }
function f(bool $flag): string { return g($flag); }\n",
    );
    assert!(family(&src).is_empty(), "{:?}", family(&src));
}


// The coercion mode is the returning file's, and PHP applies the parameter table


#[test]
fn the_coercive_table_keeps_the_string_arm_alive() {
    // Measured, not assumed: all 144 return-position cells of
    // `harness/coercion-grid` answer exactly as their parameter twins at 8.5.9.
    // Coercive mode accepts a numeric string as an `int`, so only the `null` arm
    // is rejected — the partial verdict. Strict mode rejects both and says nothing.
    let loose = coercive("function f(?string $x): int { return $x; }\n");
    let d = proof(&loose);
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.ends_with("(coercive mode)"), "{}", d[0].message);
    assert!(d[0].message.contains("its null arm"), "{}", d[0].message);

    let tight = strict("function f(?string $x): int { return $x; }\n");
    assert!(family(&tight).is_empty(), "strict mode rejects both arms — the definite verdict");
}


// Property hooks (issue #544/#550): a `get` body's native return type is the
// property's own, so it rides this check like any other body — deliberately, and
// pinned so a later reader knows it was decided rather than overlooked.


#[test]
fn a_get_hook_body_is_judged_against_the_propertys_own_type() {
    let src = strict(
        "class H { public string $p { get { $v = \\realpath('/x'); return $v; } } }\n",
    );
    let d = contract(&src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].message.contains("return type of H::$p::get()"), "{}", d[0].message);
}
