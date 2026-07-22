//! Acceptance tests for the throw system (ADR-0040 damming, ADR-0007 checked
//! accounting): `throw.undeclared` `@throws`-envelope escapes and
//! `throw.liskov-widened` overrides.
//!
//! The consumer-inverted safety asymmetry is the load-bearing joint: only a
//! **proven** (`Yes`) escape of a **checked** exception, provably a subclass of
//! **none** of the declared classes, ever fires. Maybe-absorption (a catch of an
//! unknown external class), unchecked families (`Error`/`LogicException`), and
//! unproven coverage all stay silent.

use steins_infer::{Diagnostic, THROW_LISKOV_ID, THROW_UNDECLARED_ID, check};
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

// ---- Envelope: proven escape fires, with provenance ----------------------

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

#[test]
fn declared_parent_covers_subclass_via_builtin_hierarchy() {
    // OutOfBoundsException <: RuntimeException through the SPL builtin table.
    let src = "<?php\n/** @throws \\RuntimeException */\nfunction f(): void { throw new \\OutOfBoundsException(); }\n";
    assert_eq!(n_undeclared(src), 0, "subclass of a declared class is covered");
}

// ---- Checked accounting (ADR-0007) ---------------------------------------

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

// ---- Damming: absorption through catch clauses ---------------------------

#[test]
fn caught_exact_absorbs() {
    let src = "<?php\n/** @throws \\JsonException */\nfunction f(): void { try { throw new \\RuntimeException(); } catch (\\RuntimeException $e) {} }\n";
    assert_eq!(n_undeclared(src), 0, "caught exactly → absorbed");
}

#[test]
fn caught_via_project_subclass_chain() {
    // Project class MyErr extends \RuntimeException; catch (\RuntimeException)
    // absorbs a thrown MyErr through the project chain into the builtin table.
    let src = "<?php\nclass MyErr extends \\RuntimeException {}\n/** @throws \\JsonException */\nfunction f(): void { try { throw new MyErr(); } catch (\\RuntimeException $e) {} }\n";
    assert_eq!(n_undeclared(src), 0, "project subclass caught by builtin supertype");
}

#[test]
fn caught_via_builtin_exception_hierarchy() {
    // JsonException is a \Exception; catch (\Exception) absorbs it.
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
    // Rethrow re-emits exactly RuntimeException; declaring it covers the rethrow.
    let covered = "<?php\n/** @throws \\RuntimeException */\nfunction f(): void { try { throw new \\RuntimeException(); } catch (\\RuntimeException $e) { throw $e; } }\n";
    assert_eq!(n_undeclared(covered), 0, "rethrow of a declared class is covered");
    // Same rethrow, undeclared → fires (proving the rethrow re-emits it).
    let bare = "<?php\n/** @throws \\JsonException */\nfunction f(): void { try { throw new \\RuntimeException(); } catch (\\RuntimeException $e) { throw $e; } }\n";
    assert_eq!(n_undeclared(bare), 1, "rethrow re-emits the caught class");
}

#[test]
fn wrap_and_throw_emits_new_class() {
    // Original absorbed; the wrapper (RangeException) is what propagates.
    let src = "<?php\n/** @throws \\RangeException */\nfunction f(): void { try { throw new \\RuntimeException(); } catch (\\RuntimeException $e) { throw new \\RangeException(); } }\n";
    assert_eq!(n_undeclared(src), 0, "wrapper is declared; original absorbed");
}

#[test]
fn finally_throw_counts_and_absorbs_nothing() {
    let src = "<?php\n/** @throws \\JsonException */\nfunction f(): void { try {} catch (\\RuntimeException $e) {} finally { throw new \\RuntimeException(); } }\n";
    // The finally throw is not absorbed by the sibling catch → escapes.
    assert_eq!(n_undeclared(src), 1, "finally throw counts, sibling catch absorbs nothing");
}

#[test]
fn nested_trys_compose() {
    let src = "<?php\n/** @throws \\JsonException */\nfunction f(): void { try { try { throw new \\RuntimeException(); } catch (\\TypeError $e) {} } catch (\\RuntimeException $e) {} }\n";
    // Inner catch (TypeError) misses; outer catch (RuntimeException) absorbs.
    assert_eq!(n_undeclared(src), 0, "outer try absorbs what the inner misses");
}

// ---- Adversarial: Maybe-absorption must stay silent (zero-FP) ------------

#[test]
fn maybe_absorption_by_unknown_external_stays_silent() {
    // MyExc extends an external \Vendor\Base (not in the project), so MyExc's
    // ancestry leaves known territory. A `catch (\Vendor\Other)` MIGHT be a
    // supertype (we cannot see Other's relation to Base) → Maybe absorption →
    // escape Maybe → silent. An analyzer that resolved the unknown catch to a
    // hard `No` (and MyExc to checked) would false-positively report the escape;
    // the ADR-0040 consumer-inverted Maybe is exactly what prevents that.
    let src = "<?php\nclass MyExc extends \\Vendor\\Base {}\n/** @throws \\JsonException */\nfunction f(): void { try { throw new MyExc(); } catch (\\Vendor\\Other $e) {} }\n";
    assert_eq!(n_undeclared(src), 0, "Maybe-absorption reported would be a false positive");
}

#[test]
fn unknown_external_catch_of_known_throw_is_a_real_escape() {
    // Contrast: a KNOWN builtin throw (RuntimeException) has a fully-enumerated
    // ancestry, so `catch (\App\Weird)` provably cannot absorb it — reporting the
    // escape is correct, not a false positive.
    let src = "<?php\n/** @throws \\JsonException */\nfunction f(): void { try { throw new \\RuntimeException(); } catch (\\App\\Weird $e) {} }\n";
    assert_eq!(n_undeclared(src), 1, "an unrelated catch of a known throw is a real escape");
}

#[test]
fn throw_of_unknown_variable_is_silent() {
    // `throw $e` where $e is not a catch parameter → unknown class → taint only.
    let src = "<?php\n/** @throws \\JsonException */\nfunction f(\\Throwable $e): void { throw $e; }\n";
    assert_eq!(n_undeclared(src), 0, "opaque throw taints, never reports");
}

// ---- Propagation through the call graph ----------------------------------

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

// ---- Liskov widening (ADR-0033/0040 rule 4) ------------------------------

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

// ---- Unannotated functions are never envelope-checked --------------------

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
    // The FP shape found in review: declared JsonException, caught
    // RuntimeException swapped for a JsonException before the throw.
    let fp = r#"<?php
/** @throws \JsonException */
function fp(): void {
    try { throw new \RuntimeException("x"); }
    catch (\RuntimeException $e) { $e = new \JsonException("s"); throw $e; }
}
"#;
    assert_eq!(n_undeclared(fp), 0, "reassigned rethrow must not report the caught class");
    // Control: without the reassignment the genuine rethrow of an undeclared
    // checked class still fires.
    let control = r#"<?php
/** @throws \JsonException */
function ctl(): void {
    try { throw new \RuntimeException("x"); }
    catch (\RuntimeException $e) { throw $e; }
}
"#;
    assert_eq!(n_undeclared(control), 1, "genuine rethrow of undeclared class must fire");
    // Passing $e to any call (possible by-ref rebinding) also voids precision.
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
