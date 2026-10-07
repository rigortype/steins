//! The engine `Throwable` accessors are pure, and a call through a project
//! subclass reaches their row (issue #847). `getMessage`, `getCode`, `getFile`,
//! `getLine`, `getPrevious`, `getTrace` and `getTraceAsString` are `final` on
//! `Exception` and `Error`, so `$this->getTrace()` in a project exception, or
//! `$e->getMessage()` on a parameter declared as one, runs the engine's body
//! whatever subclass the object is. `__toString` is not final and stays
//! unknown.

use steins_infer::{Diagnostic, EFFECT_ID, EffectSummary, check, effect_summary};
use steins_syntax::SourceTree;

fn exceeded(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php").into_iter().filter(|d| d.id == EFFECT_ID).collect()
}

fn summary(src: &str, symbol: &str) -> EffectSummary {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let classes = tree.classes().to_vec();
    effect_summary(&tree, &functions, &classes)
        .into_iter()
        .find(|s| s.symbol == symbol)
        .unwrap_or_else(|| panic!("no summary for {symbol}"))
}

fn proven_pure(src: &str, symbol: &str) {
    let s = summary(src, symbol);
    assert!(s.labels.is_empty() && s.exhaustive, "{symbol}: {s:?}");
}

fn unknown(src: &str, symbol: &str) {
    let s = summary(src, symbol);
    assert!(s.labels.is_empty() && !s.exhaustive, "{symbol}: {s:?}");
}

/// A test framework's base exception: it forwards to the engine's constructor
/// and keeps a copy of the trace.
const SNAPSHOT: &str = "<?php
namespace App;

class Snapshot extends \\RuntimeException
{
    protected array $frames;

    public function __construct(string $message = '')
    {
        parent::__construct($message);
        $this->frames = $this->getTrace();
    }
}

class Skipped extends Snapshot {}

#[\\Steins\\Pure]
function skip(string $why): never
{
    throw new Skipped($why);
}
";

#[test]
fn a_constructor_that_reads_its_trace_is_pure_and_exhaustive() {
    proven_pure(SNAPSHOT, "Snapshot::__construct");
    // `new` follows the inherited constructor, so the caller stays exhaustive.
    proven_pure(SNAPSHOT, "skip");
    assert!(exceeded(SNAPSHOT).is_empty());
}

#[test]
fn every_accessor_is_pure_through_this() {
    for accessor in
        ["getMessage", "getCode", "getFile", "getLine", "getPrevious", "getTrace", "getTraceAsString"]
    {
        let src = format!(
            "<?php\nclass Oops extends \\Exception {{\n    \
             public function read(): mixed {{ return $this->{accessor}(); }}\n}}\n\
             class Fault extends \\Error {{\n    \
             public function read(): mixed {{ return self::{accessor}(); }}\n}}\n"
        );
        proven_pure(&src, "Oops::read");
        proven_pure(&src, "Fault::read");
    }
}

// The returns below are `mixed`: `getMessage()` and `getCode()` hand back a property a subclass may fill
// with an object, which a `string` return type would convert through `__toString` (ADR-0099 §4.3's
// Coerce row, issue #868), so a `: string` there is a gap of its own (`coercion_boundaries`).
#[test]
fn a_declared_receiver_reaches_the_row_through_its_chain() {
    let src = "<?php\nnamespace App;\nclass Oops extends \\LogicException {}\n\
               function project(Oops $e): mixed { return $e->getMessage(); }\n\
               function engine(?\\RuntimeException $e): int { return $e->getLine(); }\n\
               function contract(\\Throwable $e): ?\\Throwable { return $e->getPrevious(); }\n\
               final class Holder {\n    \
               public function __construct(private \\Exception $cause) {}\n    \
               public function trace(): string { return $this->cause->getTraceAsString(); }\n}\n";
    proven_pure(src, "project");
    proven_pure(src, "engine");
    proven_pure(src, "contract");
    proven_pure(src, "Holder::trace");
}

#[test]
fn exact_receivers_reach_the_row_too() {
    let src = "<?php\nclass Oops extends \\Exception {\n    \
               public function up(): mixed { return parent::getMessage(); }\n}\n\
               function fresh(): int { return (new Oops('x'))->getCode(); }\n";
    proven_pure(src, "Oops::up");
    proven_pure(src, "fresh");
}

#[test]
fn to_string_is_not_final_and_stays_unknown() {
    let src = "<?php\nclass Oops extends \\Exception {\n    \
               public function show(): string { return $this->__toString(); }\n}\n\
               function show(\\Exception $e): string { return $e->__toString(); }\n";
    unknown(src, "Oops::show");
    unknown(src, "show");
}

#[test]
fn a_class_the_analysis_cannot_see_stays_unknown() {
    // The chain leaves the project at a namespaced class nothing declares.
    let src = "<?php\nnamespace App;\nclass Local extends \\Vendor\\Failure {\n    \
               public function m(): string { return $this->getMessage(); }\n}\n\
               function f(\\Vendor\\Failure $e): string { return $e->getMessage(); }\n";
    unknown(src, "Local::m");
    unknown(src, "f");
}

#[test]
fn a_catch_binding_names_no_class_for_the_effects_pass() {
    // The catch writes `$e`, so the call's receiver is not a declared one, and
    // the effects pass has no flow environment to read the caught class from.
    let src = "<?php\nfunction f(): string {\n    \
               try { return 'ok'; } catch (\\RuntimeException $e) { return $e->getMessage(); }\n}\n";
    unknown(src, "f");
}

#[test]
fn a_bound_receiver_reaches_only_a_final_row() {
    // `PDO::query` is not final, so `$this->query()` may run a subclass's
    // override; `parent::query()` names the engine's exactly.
    let src = "<?php\nclass Db extends \\PDO {\n    \
               public function viaThis(): mixed { return $this->query('SELECT 1'); }\n    \
               #[\\Steins\\Pure]\n    \
               public function viaParent(): mixed { return parent::query('SELECT 1'); }\n}\n";
    unknown(src, "Db::viaThis");
    let s = summary(src, "Db::viaParent");
    assert_eq!(s.labels, ["io.db"]);
    assert!(s.exhaustive, "{s:?}");
    let f = exceeded(src);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert!(f[0].message.starts_with("parent::query() has effect io.db"), "{}", f[0].message);
}
