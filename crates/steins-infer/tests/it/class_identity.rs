//! Issues #926 and #917: class identity is resolved, not spelled (ADR-0043 amendment).
//!
//! Two names for one class are one class to every is-a consumer. A project's literal
//! `class_alias` (chained to a fixpoint) and a stub's class-level `@alias`
//! (`Dom\DOMException` is `DOMException`) both reach the oracle through
//! `Cx::class_identity`, and a target that names nothing the analysis can see declines
//! rather than proves while a dynamic `class_alias` dams the universe.
//!
//! Every positive case ships its negative control: the fix widens what two spellings
//! mean, never what an unrelated class accepts.

use steins_infer::{Diagnostic, THROW_UNDECLARED_ID, check};
use steins_syntax::SourceTree;

fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    // `untyped.*` reports on the fixtures' deliberately bare declarations, not the
    // behaviour under test.
    check(&tree, &functions, "test.php")
        .into_iter()
        .filter(|d| !d.id.starts_with("untyped."))
        .collect()
}

fn ids(src: &str) -> Vec<String> {
    findings(src).into_iter().map(|d| d.id.to_owned()).collect()
}

fn silent(src: &str) {
    let ds = findings(src);
    assert!(ds.is_empty(), "expected silence, got: {ds:#?}");
}

// #926: a project alias names the class it points at.

#[test]
fn an_alias_and_its_target_accept_each_other() {
    silent(
        "<?php declare(strict_types=1);
class Real {}
class_alias(Real::class, 'Alias');
function acceptAlias(Alias $_): void {}
function makeReal(): Real { return new Alias(); }
acceptAlias(new Real());
makeReal();
",
    );
}

/// The seven shapes of mago's `issue_2323.php`: `::class` and string operands, a chain,
/// `__NAMESPACE__` concatenation, an interface, an enum and a parent-class alias.
#[test]
fn the_seven_alias_shapes_are_silent() {
    silent(
        "<?php declare(strict_types=1);
namespace App;
interface I {}
enum E { case A; }
class P {}
class Real extends P implements I {}
class_alias(Real::class, 'App\\\\A1');
class_alias('App\\\\A1', 'App\\\\A2');
class_alias(__NAMESPACE__ . '\\\\Real', 'App\\\\A3');
class_alias(I::class, 'App\\\\IA');
class_alias(E::class, 'App\\\\EA');
class_alias(P::class, 'App\\\\PA');
function f1(A1 $x): void {} function f2(A2 $x): void {} function f3(A3 $x): void {}
function fi(IA $x): void {} function fe(EA $x): void {} function fp(PA $x): void {}
function fr(Real $x): void {}
f1(new Real()); f2(new Real()); f3(new Real()); fi(new Real()); fe(E::A); fp(new Real());
fr(new A1()); fr(new A2()); fr(new A3());
",
    );
}

/// A chain resolves whichever order the calls are written in, and a longer chain
/// resolves too: the fold is a fixpoint, not one pass.
#[test]
fn a_chain_resolves_in_either_order_and_to_any_depth() {
    for calls in [
        "class_alias('A1', 'A2'); class_alias('A2', 'A3'); class_alias('Real', 'A1');",
        "class_alias('Real', 'A1'); class_alias('A1', 'A2'); class_alias('A2', 'A3');",
    ] {
        silent(&format!(
            "<?php declare(strict_types=1);
class Real {{}}
{calls}
function f(A3 $x): void {{}}
function g(Real $x): void {{}}
f(new Real());
g(new A3());
"
        ));
    }
}

/// A cycle of aliases with no declaration under it names nothing and the fold ends: the
/// analysis terminates, and the declared class beside it is unaffected.
#[test]
fn an_alias_cycle_terminates() {
    let src = "<?php declare(strict_types=1);
class Real {}
class_alias('B1', 'B2'); class_alias('B2', 'B1');
function f(Real $x): void {}
f(new Real());
";
    silent(src);
}

#[test]
fn an_alias_of_another_class_is_still_that_class() {
    // Control: `Real` is not `Other`, whatever it is also called.
    let src = "<?php declare(strict_types=1);
class Real {}
class Other {}
class_alias(Real::class, 'Alias');
function f(Real $r): void {}
function g(Other $o): void {}
f(new Other());
g(new Alias());
g(new Real());
";
    assert_eq!(
        ids(src),
        vec!["type.argument-mismatch"; 3],
        "each of the three mismatches reports: {:#?}",
        findings(src)
    );
}

#[test]
fn an_alias_of_the_real_class_does_not_accept_a_sibling() {
    let src = "<?php declare(strict_types=1);
class Real {}
class Other {}
class_alias(Real::class, 'Alias');
function f(Alias $a): void {}
f(new Other());
";
    assert_eq!(ids(src), vec!["type.argument-mismatch"], "{:#?}", findings(src));
}

/// A dynamic `class_alias` dams the universe. A mismatch between two classes the
/// analysis can see still proves, since neither name can be the dynamic one's target:
/// an alias cannot rename an existing class. A parameter typed with a name nothing
/// declares might be what the dynamic call mints, so it declines.
#[test]
fn a_dynamic_alias_declines_only_the_unknown_name() {
    let dammed = "<?php declare(strict_types=1);
class Real {}
class Other {}
class_alias($target, $name);
function known(Real $r): void {}
function unknown(Minted $m): void {}
known(new Other());
unknown(new Real());
";
    let ds = findings(dammed);
    let mismatches: Vec<_> = ds.iter().filter(|d| d.id == "type.argument-mismatch").collect();
    assert_eq!(mismatches.len(), 1, "only the known-class mismatch reports: {ds:#?}");
    assert!(mismatches[0].message.contains("new Other()"), "{ds:#?}");

    // Without the dynamic call the same unknown name is a proven mismatch.
    let clear = "<?php declare(strict_types=1);
class Real {}
function unknown(Minted $m): void {}
unknown(new Real());
";
    assert!(ids(clear).iter().any(|id| id == "type.argument-mismatch"), "{:#?}", findings(clear));
}

// #917: a stub's alias is the class.

#[test]
fn a_stub_alias_accepts_the_declared_class() {
    for strict in ["declare(strict_types=1);", ""] {
        silent(&format!(
            "<?php {strict}
function takes(\\Dom\\DOMException $e): string {{ return $e->getMessage(); }}
function takes2(\\DOMException $e): string {{ return $e->getMessage(); }}
takes(new \\DOMException('x'));
takes2(new \\Dom\\DOMException('y'));
"
        ));
    }
}

#[test]
fn a_stub_alias_catches_the_declared_class_and_the_converse() {
    silent(
        "<?php
/** @throws \\RuntimeException */
function thrower(): void {
    try {
        throw new \\DOMException('y');
    } catch (\\Dom\\DOMException $e) {
        echo 'caught';
    }
}
/** @throws \\RuntimeException */
function thrower2(): void {
    try {
        throw new \\Dom\\DOMException('y');
    } catch (\\DOMException $e) {
        echo 'caught2';
    }
}
",
    );
}

#[test]
fn a_stub_alias_covers_its_class_in_a_throws_tag() {
    silent(
        "<?php
/** @throws \\Dom\\DOMException */
function thrower(): void { throw new \\DOMException('y'); }
/** @throws \\DOMException */
function thrower2(): void { throw new \\Dom\\DOMException('y'); }
",
    );
}

#[test]
fn a_stub_alias_names_no_other_class() {
    // Control: the pair is one class, and nothing else joins it.
    let src = "<?php declare(strict_types=1);
function takes(\\Dom\\DOMException $e): void {}
takes(new \\RuntimeException('x'));
";
    assert_eq!(ids(src), vec!["type.argument-mismatch"], "{:#?}", findings(src));

    // A `RuntimeException` is not caught by `catch (\\Dom\\DOMException)`: it escapes.
    let src = "<?php
/** @throws \\LogicException */
function thrower(): void {
    try {
        throw new \\RuntimeException('y');
    } catch (\\Dom\\DOMException $e) {
        echo 'caught';
    }
}
";
    let ds = findings(src);
    assert!(ds.iter().any(|d| d.id == THROW_UNDECLARED_ID), "{ds:#?}");
}
