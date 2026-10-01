//! Acceptance tests for the throw system (ADR-0040 damming, ADR-0007 checked
//! accounting): `throw.undeclared` `@throws`-envelope escapes and
//! `throw.liskov-widened` overrides.
//!
//! The consumer-inverted safety asymmetry is the load-bearing joint: only a
//! **proven** (`Yes`) escape of a **checked** exception, provably is-a **none**
//! of the declared classes or interfaces, ever fires. Maybe-absorption (a catch of an
//! unknown external class), unchecked families (`Error`/`LogicException`), and
//! unproven coverage all stay silent.

use steins_infer::{
    Diagnostic, Facet, Origin, THROW_LISKOV_ID, THROW_UNDECLARED_ID, check, declared_facet,
};
use steins_syntax::SourceTree;

fn undeclared(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php").into_iter().filter(|d| d.id == THROW_UNDECLARED_ID).collect()
}

fn liskov(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php").into_iter().filter(|d| d.id == THROW_LISKOV_ID).collect()
}

fn n_undeclared(src: &str) -> usize {
    undeclared(src).len()
}

// Envelope: proven escape fires, with provenance

#[test]
fn uncaught_checked_escape_fires_with_message() {
    let src = "<?php\n/** @throws \\JsonException */\nfunction f(): void { throw new \\RuntimeException(); }\n";
    let ds = undeclared(src);
    assert_eq!(ds.len(), 1, "got: {ds:#?}");
    let d = &ds[0];
    assert_eq!(
        d.message,
        "RuntimeException can escape f() but is not declared (@throws JsonException) — proven escape"
    );
    assert_eq!(d.line, 3, "reported at the throw origin");
}

#[test]
fn declared_exact_covers() {
    let src = "<?php\n/** @throws \\RuntimeException */\nfunction f(): void { throw new \\RuntimeException(); }\n";
    assert_eq!(n_undeclared(src), 0);
}

/// `OutOfBoundsException <: RuntimeException` through the SPL builtin table.
#[test]
fn declared_parent_covers_subclass_via_builtin_hierarchy() {
    let src = "<?php\n/** @throws \\RuntimeException */\nfunction f(): void { throw new \\OutOfBoundsException(); }\n";
    assert_eq!(n_undeclared(src), 0, "subclass of a declared class is covered");
}

// Checked accounting (ADR-0007)

#[test]
fn error_family_never_counts() {
    let src = "<?php\n/** @throws \\JsonException */\nfunction f(): void { throw new \\TypeError(); }\n";
    assert_eq!(n_undeclared(src), 0, "TypeError is unchecked (Error family)");
}

#[test]
fn logic_exception_family_never_counts() {
    let src = "<?php\n/** @throws \\JsonException */\nfunction f(): void { throw new \\InvalidArgumentException(); }\n";
    assert_eq!(n_undeclared(src), 0, "InvalidArgumentException is unchecked (Logic family)");
}

#[test]
fn runtime_exception_is_checked() {
    let src = "<?php\n/** @throws \\JsonException */\nfunction f(): void { throw new \\RuntimeException(); }\n";
    assert_eq!(n_undeclared(src), 1, "RuntimeException is checked");
}

// Damming: absorption through catch clauses

#[test]
fn caught_exact_absorbs() {
    let src = "<?php\n/** @throws \\JsonException */\nfunction f(): void { try { throw new \\RuntimeException(); } catch (\\RuntimeException $e) {} }\n";
    assert_eq!(n_undeclared(src), 0, "caught exactly → absorbed");
}

/// `MyErr extends \RuntimeException`; `catch (\RuntimeException)` absorbs it
/// through the project chain into the builtin table.
#[test]
fn caught_via_project_subclass_chain() {
    let src = "<?php\nclass MyErr extends \\RuntimeException {}\n/** @throws \\JsonException */\nfunction f(): void { try { throw new MyErr(); } catch (\\RuntimeException $e) {} }\n";
    assert_eq!(n_undeclared(src), 0, "project subclass caught by builtin supertype");
}

#[test]
fn caught_via_builtin_exception_hierarchy() {
    let src = "<?php\n/** @throws \\RuntimeException */\nfunction f(): void { try { throw new \\JsonException(); } catch (\\Exception $e) {} }\n";
    assert_eq!(n_undeclared(src), 0, "JsonException caught via \\Exception");
}

#[test]
fn catch_all_throwable_absorbs() {
    let src = "<?php\n/** @throws \\JsonException */\nfunction f(): void { try { throw new \\RuntimeException(); } catch (\\Throwable $e) {} }\n";
    assert_eq!(n_undeclared(src), 0, "\\Throwable absorbs hierarchically");
}

#[test]
fn multi_catch_absorbs() {
    let src = "<?php\n/** @throws \\JsonException */\nfunction f(): void { try { throw new \\RuntimeException(); } catch (\\LogicException | \\RuntimeException $e) {} }\n";
    assert_eq!(n_undeclared(src), 0, "multi-catch member absorbs");
}

#[test]
fn unrelated_catch_does_not_absorb() {
    let src = "<?php\n/** @throws \\JsonException */\nfunction f(): void { try { throw new \\RuntimeException(); } catch (\\TypeError $e) {} }\n";
    assert_eq!(n_undeclared(src), 1, "TypeError catch cannot absorb RuntimeException");
}

#[test]
fn catch_body_throw_escapes() {
    let src = "<?php\n/** @throws \\JsonException */\nfunction f(): void { try { throw new \\RuntimeException(); } catch (\\RuntimeException $e) { throw new \\RangeException(); } }\n";
    // Try-throw absorbed; the catch-body throw is outside its own clause → escapes.
    let ds = undeclared(src);
    assert_eq!(ds.len(), 1, "got: {ds:#?}");
    assert!(ds[0].message.starts_with("RangeException can escape"));
}

#[test]
fn rethrow_precise_reemits_caught_set() {
    let covered = "<?php\n/** @throws \\RuntimeException */\nfunction f(): void { try { throw new \\RuntimeException(); } catch (\\RuntimeException $e) { throw $e; } }\n";
    assert_eq!(n_undeclared(covered), 0, "rethrow of a declared class is covered");
    let bare = "<?php\n/** @throws \\JsonException */\nfunction f(): void { try { throw new \\RuntimeException(); } catch (\\RuntimeException $e) { throw $e; } }\n";
    assert_eq!(n_undeclared(bare), 1, "rethrow re-emits the caught class");
}

#[test]
fn wrap_and_throw_emits_new_class() {
    let src = "<?php\n/** @throws \\RangeException */\nfunction f(): void { try { throw new \\RuntimeException(); } catch (\\RuntimeException $e) { throw new \\RangeException(); } }\n";
    assert_eq!(n_undeclared(src), 0, "wrapper is declared; original absorbed");
}

#[test]
fn finally_throw_counts_and_absorbs_nothing() {
    let src = "<?php\n/** @throws \\JsonException */\nfunction f(): void { try {} catch (\\RuntimeException $e) {} finally { throw new \\RuntimeException(); } }\n";
    assert_eq!(n_undeclared(src), 1, "finally throw counts, sibling catch absorbs nothing");
}

#[test]
fn nested_trys_compose() {
    let src = "<?php\n/** @throws \\JsonException */\nfunction f(): void { try { try { throw new \\RuntimeException(); } catch (\\TypeError $e) {} } catch (\\RuntimeException $e) {} }\n";
    assert_eq!(n_undeclared(src), 0, "outer try absorbs what the inner misses");
}

// Adversarial: Maybe-absorption must stay silent (zero-FP)

/// `MyExc extends` an external, unresolvable `\Vendor\Base`, so `catch
/// (\Vendor\Other)` MIGHT be a supertype → Maybe absorption → silent. Resolving
/// the unknown catch to a hard `No` would false-positively report the escape;
/// ADR-0040's consumer-inverted Maybe is what prevents that.
#[test]
fn maybe_absorption_by_unknown_external_stays_silent() {
    let src = "<?php\nclass MyExc extends \\Vendor\\Base {}\n/** @throws \\JsonException */\nfunction f(): void { try { throw new MyExc(); } catch (\\Vendor\\Other $e) {} }\n";
    assert_eq!(n_undeclared(src), 0, "Maybe-absorption reported would be a false positive");
}

/// Contrast: a KNOWN builtin throw has a fully-enumerated ancestry, so `catch
/// (\App\Weird)` provably cannot absorb it — reporting the escape is correct.
#[test]
fn unknown_external_catch_of_known_throw_is_a_real_escape() {
    let src = "<?php\n/** @throws \\JsonException */\nfunction f(): void { try { throw new \\RuntimeException(); } catch (\\App\\Weird $e) {} }\n";
    assert_eq!(n_undeclared(src), 1, "an unrelated catch of a known throw is a real escape");
}

// Interfaces on the supertype graph (issue #852): `@throws` and `catch` may name
// an interface, and the thrown class is-a every interface it or an ancestor
// implements, through each interface's own `extends`.

const DOMAIN: &str = "<?php
interface Marker {}
interface DomainError extends \\Countable, Marker {}
class Base extends \\RuntimeException implements DomainError {}
final class Oops extends Base {}
";

#[test]
fn interface_in_throws_covers_its_implementor() {
    let src = "<?php
interface DomainError {}
final class Oops extends \\RuntimeException implements DomainError {}
/** @throws DomainError */
function f(): void { throw new Oops(); }
";
    assert_eq!(n_undeclared(src), 0, "Oops is a DomainError");
}

#[test]
fn interface_in_catch_absorbs_its_implementor() {
    let src = "<?php
interface DomainError {}
interface Elsewhere {}
final class Oops extends \\RuntimeException implements DomainError {}
/** @throws \\JsonException */
function g(): void { try { throw new Oops(); } catch (DomainError $e) {} }
/** @throws \\JsonException */
function h(): void { try { throw new Oops(); } catch (Elsewhere $e) {} }
";
    let ds = undeclared(src);
    assert_eq!(ds.len(), 1, "only the unrelated catch lets Oops out: {ds:#?}");
    assert!(ds[0].message.contains("escape h()"), "got: {}", ds[0].message);
}

#[test]
fn interface_through_an_ancestor_and_interface_extends() {
    // `Oops` implements nothing itself; `Base` implements `DomainError`, which
    // extends `Marker` as its second parent interface.
    let src = format!("{DOMAIN}
/** @throws DomainError */
function a(): void {{ throw new Oops(); }}
/** @throws Marker */
function b(): void {{ throw new Oops(); }}
/** @throws \\JsonException */
function c(): void {{ try {{ throw new Oops(); }} catch (Marker $e) {{}} }}
");
    assert_eq!(n_undeclared(&src), 0);
}

#[test]
fn propagated_throw_dammed_by_an_interface_catch() {
    let src = format!("{DOMAIN}
function inner(): void {{ throw new Oops(); }}
function outer(): void {{ try {{ inner(); }} catch (DomainError $e) {{}} }}
/** @throws \\JsonException */
function top(): void {{ outer(); }}
");
    assert_eq!(n_undeclared(&src), 0, "the interface catch in outer() dams it");
}

#[test]
fn engine_interfaces_in_throws_and_catch() {
    // `Throwable extends Stringable`, so a `catch (\Stringable)` absorbs any throw.
    let src = format!("{DOMAIN}
/** @throws \\Throwable */
function a(): void {{ throw new Oops(); }}
/** @throws \\JsonException */
function b(): void {{ try {{ throw new \\RuntimeException(); }} catch (\\Stringable $e) {{}} }}
");
    assert_eq!(n_undeclared(&src), 0);
    // Implementing a catalogued engine interface keeps the hierarchy closed.
    let closed = "<?php
final class Oops extends \\RuntimeException implements \\Stringable, \\JsonSerializable {
    public function jsonSerialize(): mixed { return null; }
}
/** @throws \\JsonException */
function f(): void { throw new Oops(); }
";
    assert_eq!(n_undeclared(closed), 1, "Oops is no JsonException");
}

/// The catalog's full hierarchy replaced the frozen exception table, so an
/// engine exception the table lacked is now enumerated, and checked.
#[test]
fn engine_exception_outside_the_old_table_is_enumerated() {
    let src = "<?php
/** @throws \\RuntimeException */
function covered(): void { throw new \\PDOException(); }
/** @throws \\JsonException */
function uncovered(): void { throw new \\PDOException(); }
/** @throws \\JsonException */
function unchecked(): void { throw new \\ArgumentCountError(); }
";
    let ds = undeclared(src);
    assert_eq!(ds.len(), 1, "got: {ds:#?}");
    let msg = &ds[0].message;
    assert!(msg.starts_with("PDOException can escape uncovered()"), "got: {msg}");
}

/// An interface no analyzed file declares may extend anything, so it never
/// yields a `No` against an interface target: a `@throws` or `catch` naming
/// another unknown interface stays silent.
#[test]
fn unknown_external_interface_stays_silent() {
    let src = "<?php
final class Oops extends \\RuntimeException implements \\Vendor\\Marker {}
/** @throws \\Vendor\\DomainError */
function f(): void { throw new Oops(); }
/** @throws \\JsonException */
function g(): void { try { throw new Oops(); } catch (\\Vendor\\DomainError $e) {} }
";
    assert_eq!(n_undeclared(src), 0, "a Maybe must not report");
}

/// Contrast: an unknown interface can only add interfaces, never a class, so a
/// class target is still decided by the class chain.
#[test]
fn unknown_external_interface_cannot_hide_a_class() {
    let src = "<?php
final class Oops extends \\RuntimeException implements \\Vendor\\Marker {}
class Other extends \\Exception {}
/** @throws Other */
function f(): void { throw new Oops(); }
";
    assert_eq!(n_undeclared(src), 1, "Oops is no Other, whatever Marker extends");
}

#[test]
fn liskov_widening_sees_an_interface_abstraction_throws() {
    let src = "<?php
interface DomainError {}
final class Oops extends \\RuntimeException implements DomainError {}
class Base { /** @throws DomainError */ public function m(): void {} }
class Sub extends Base { /** @throws Oops */ public function m(): void {} }
";
    assert_eq!(liskov(src).len(), 0, "Oops is a DomainError: narrower, not wider");
}

#[test]
fn throw_of_unknown_variable_is_silent() {
    let src = "<?php\n/** @throws \\JsonException */\nfunction f(\\Throwable $e): void { throw $e; }\n";
    assert_eq!(n_undeclared(src), 0, "opaque throw taints, never reports");
}

// Propagation through the call graph

#[test]
fn propagated_callee_throw_escapes_caller() {
    let src = "<?php\nfunction g(): void { throw new \\RuntimeException(); }\n/** @throws \\JsonException */\nfunction f(): void { g(); }\n";
    let ds = undeclared(src);
    assert_eq!(ds.len(), 1, "got: {ds:#?}");
    assert!(ds[0].message.starts_with("RuntimeException can escape f()"));
}

#[test]
fn propagated_callee_throw_dammed_at_caller() {
    let src = "<?php\nfunction g(): void { throw new \\RuntimeException(); }\n/** @throws \\JsonException */\nfunction f(): void { try { g(); } catch (\\RuntimeException $e) {} }\n";
    assert_eq!(n_undeclared(src), 0, "caller's try/catch dams the propagated throw");
}

// Liskov widening (ADR-0033/0040 rule 4)

#[test]
fn liskov_widened_override_fires() {
    let src = "<?php\nclass Base { /** @throws \\RuntimeException */ public function m(): void {} }\nclass Sub extends Base { /** @throws \\JsonException */ public function m(): void {} }\n";
    let ds = liskov(src);
    assert_eq!(ds.len(), 1, "got: {ds:#?}");
    assert!(ds[0].message.contains("JsonException"));
}

#[test]
fn liskov_narrower_override_silent() {
    let src = "<?php\nclass Base { /** @throws \\Exception */ public function m(): void {} }\nclass Sub extends Base { /** @throws \\RuntimeException */ public function m(): void {} }\n";
    assert_eq!(liskov(src).len(), 0, "narrower (subclass) throw is allowed");
}

#[test]
fn liskov_one_side_undeclared_silent() {
    let src = "<?php\nclass Base { public function m(): void {} }\nclass Sub extends Base { /** @throws \\RuntimeException */ public function m(): void {} }\n";
    assert_eq!(liskov(src).len(), 0, "no check unless both sides declare @throws");
}

// Unannotated functions are never envelope-checked

#[test]
fn unannotated_function_is_never_checked() {
    let src = "<?php\nfunction f(): void { throw new \\RuntimeException(); }\n";
    assert_eq!(n_undeclared(src), 0, "opt-in: no @throws → no envelope");
}

/// Review counterexample: a catch body that REASSIGNS its parameter must not
/// claim rethrow precision — `throw $e` after `$e = new Other()` throws the
/// new class, and reporting the *caught* class as escaping is a false
/// positive when the new class is the declared one.
#[test]
fn rethrow_after_reassignment_is_not_a_rethrow() {
    let fp = r#"<?php
/** @throws \JsonException */
function fp(): void {
    try { throw new \RuntimeException("x"); }
    catch (\RuntimeException $e) { $e = new \JsonException("s"); throw $e; }
}
"#;
    assert_eq!(n_undeclared(fp), 0, "reassigned rethrow must not report the caught class");
    let control = r#"<?php
/** @throws \JsonException */
function ctl(): void {
    try { throw new \RuntimeException("x"); }
    catch (\RuntimeException $e) { throw $e; }
}
"#;
    assert_eq!(n_undeclared(control), 1, "genuine rethrow of undeclared class must fire");
    let passed = r#"<?php
function mutate(\Throwable &$t): void {}
/** @throws \JsonException */
function pass(): void {
    try { throw new \RuntimeException("x"); }
    catch (\RuntimeException $e) { mutate($e); throw $e; }
}
"#;
    assert_eq!(n_undeclared(passed), 0, "param handed to a call voids rethrow precision");
}

// The `origin` facet (ADR-0050 §4)
//
// `throw.undeclared` findings carry a registry-declared `origin` facet: DIRECT
// when the escaping throw originates in the annotated declaration's OWN body,
// PROPAGATED when it arrives up a call edge — the split the `throws-direct`
// profile selects on (158 direct vs 43,805 propagated on the legacy monorepo).

#[test]
fn own_body_throw_is_direct() {
    let src = "<?php\n/** @throws \\JsonException */\nfunction f(): void { throw new \\RuntimeException(); }\n";
    let ds = undeclared(src);
    assert_eq!(ds.len(), 1, "got: {ds:#?}");
    assert_eq!(ds[0].facet, Some(Facet::Origin(Origin::Direct)));
}

/// The escape's origin is g()'s body, reached up a call edge → propagated,
/// even though g lives in the same file as f.
#[test]
fn escape_through_a_callee_is_propagated() {
    let src = "<?php\n\
        function g(): void { throw new \\RuntimeException(); }\n\
        /** @throws \\JsonException */\n\
        function f(): void { g(); }\n";
    let ds = undeclared(src);
    assert_eq!(ds.len(), 1, "got: {ds:#?}");
    assert_eq!(ds[0].facet, Some(Facet::Origin(Origin::Propagated)));
}

/// f() throws directly AND calls g() which throws: both escape f()'s envelope,
/// the own-body one direct and the callee one propagated — the facet is
/// per-finding, not per-declaration.
#[test]
fn direct_and_propagated_coexist_on_one_declaration() {
    let src = "<?php\n\
        function g(): void { throw new \\RangeException(); }\n\
        /** @throws \\JsonException */\n\
        function f(): void { g(); throw new \\RuntimeException(); }\n";
    let ds = undeclared(src);
    assert_eq!(ds.len(), 2, "got: {ds:#?}");
    let facets: std::collections::HashSet<_> = ds.iter().map(|d| d.facet).collect();
    assert!(facets.contains(&Some(Facet::Origin(Origin::Direct))));
    assert!(facets.contains(&Some(Facet::Origin(Origin::Propagated))));
}

#[test]
fn only_throw_undeclared_declares_the_origin_facet() {
    assert_eq!(declared_facet(THROW_UNDECLARED_ID), Some("origin"));
    assert_eq!(declared_facet(THROW_LISKOV_ID), None);
    assert_eq!(declared_facet("type.argument-mismatch"), None);
    assert_eq!(declared_facet("nope"), None);
}

/// A liskov-widened finding is not a throw.undeclared finding, so it declares
/// no facet (ADR-0050 §4).
#[test]
fn liskov_findings_carry_no_facet() {
    let src = r#"<?php
class Base { /** @throws \JsonException */ public function m(): void {} }
class Child extends Base { /** @throws \RuntimeException */ public function m(): void {} }
"#;
    let ds = liskov(src);
    assert!(!ds.is_empty(), "expected a liskov finding");
    for d in &ds {
        assert_eq!(d.facet, None);
    }
}
