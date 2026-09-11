//! Issue #673 — the builtin `Class::method` declared-return table.
//!
//! ADR-0069's floor has been function-keyed since #73, so `$f->fgets()` typed
//! `unknown` while `str_repeat($s, $n)` typed `string`. The 6,658 `Class::method`
//! keys the miner skipped were deferred because so many of them return objects and
//! the object bucket had no denotation; ADR-0093 §3.1 settled that an object arm may
//! enter the contract lane when it is sourced from a declaration, and a mined
//! declared return is one.
//!
//! This file pins what the new rung answers, which gate each shape hits, and — above
//! all — the two things that must stay true whatever the table says: every arm is
//! `Asserted` and premises no proof-layer finding, and a project class extending a
//! builtin keeps its own declaration on every name it declares.

use steins_infer::{DEBUG_TYPE_ID, Diagnostic, check};
use steins_syntax::SourceTree;

fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php")
}

fn types(src: &str) -> Vec<String> {
    findings(src)
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message.replace("dumped type: ", ""))
        .collect()
}

fn one_type(src: &str) -> String {
    let ts = types(src);
    assert_eq!(ts.len(), 1, "expected exactly one debug.type dump, got {ts:?}");
    ts.into_iter().next().expect("one dump")
}

// ---------------------------------------------------------------------------
// The receiver carriers (ADR-0049 A17): where the declared class comes from.
// ---------------------------------------------------------------------------

/// The headline, and issue #673's first witness: a `SplFileObject $f` **parameter**.
/// No heap object exists — `seed_declared_param_object` declines an unknown class —
/// so the declared class comes from the contract-arm lane, which is exactly the
/// carrier A17 rules the declaration path reads.
#[test]
fn a_declared_builtin_parameter_answers_from_the_table() {
    let src = r#"<?php
function f(\SplFileObject $file): void {
    \PHPStan\dumpType($file->fgets());
}
"#;
    assert_eq!(one_type(src), "string (asserted)");
}

/// An **allocation** receiver. `new DOMDocument()` proves the exact class, and the
/// class is still a builtin, so the answer is the table's — and it is an object arm,
/// live only because ADR-0093 §3.1 sources it from a declaration.
#[test]
fn an_allocation_of_a_builtin_answers_from_the_table() {
    let src = r#"<?php
function f(): void {
    $dom = new \DOMDocument();
    \PHPStan\dumpType($dom->getElementById('x'));
}
"#;
    assert_eq!(one_type(src), "DOMElement|null (asserted)");
}

/// The `new` written **at the call site**, with no variable in between: the
/// `Receiver::New` arm of the same read.
#[test]
fn a_new_receiver_at_the_call_site_answers_from_the_table() {
    let src = r#"<?php
function f(): void {
    \PHPStan\dumpType((new \DOMDocument())->saveXML());
}
"#;
    assert_eq!(one_type(src), "string|false (asserted)");
}

/// A `?PDO` parameter past its null guard. The lane starts as two arms and the
/// guard subtracts the `null` one, leaving a single class arm — A17's third carrier,
/// and the shape #619 named as the one `seed_declared_param_object` could not reach.
#[test]
fn a_nullable_builtin_receiver_answers_past_its_null_guard() {
    let src = r#"<?php
function f(?\PDO $pdo): void {
    if ($pdo === null) {
        return;
    }
    \PHPStan\dumpType($pdo->lastInsertId());
}
"#;
    assert_eq!(one_type(src), "string|false (asserted)");
}

/// Before the guard the lane holds two arms, and two class arms are not one
/// receiver: the rung declines rather than answering the union of two envelopes.
#[test]
fn a_nullable_builtin_receiver_answers_nothing_before_the_guard() {
    let src = r#"<?php
function f(?\PDO $pdo): void {
    \PHPStan\dumpType($pdo->lastInsertId());
}
"#;
    assert_eq!(one_type(src), "unknown");
}

// ---------------------------------------------------------------------------
// Inheritance (ADR-0049 A16): a parent's row answers for a child receiver.
// ---------------------------------------------------------------------------

/// `SplFileObject` has no `getPath` row; `SplFileInfo` does, and `SplFileObject`
/// descends from it in the generated hierarchy (ADR-0043). PHP enforces return
/// covariance at class-declaration time, so the declaring class's envelope is an
/// upper bound under every descendant — which is what licenses reading a parent's
/// row through a child receiver at all.
#[test]
fn a_parents_row_answers_for_a_child_receiver() {
    let src = r#"<?php
function f(\SplFileObject $file): void {
    \PHPStan\dumpType($file->getPath());
}
"#;
    assert_eq!(one_type(src), "string (asserted)");
}

/// The nearest row wins where both a class and its parent carry one: `SplFileObject`
/// declares `key` itself, so the walk stops there rather than reaching `SplFileInfo`.
#[test]
fn the_nearest_row_wins_over_an_ancestors() {
    let src = r#"<?php
function f(\SplFileObject $file): void {
    \PHPStan\dumpType($file->key());
}
"#;
    assert_eq!(one_type(src), "int (asserted)");
}

/// A name no row on the chain carries answers nothing. Silence is the floor's
/// normal state, not an error.
#[test]
fn a_name_no_row_carries_answers_nothing() {
    let src = r#"<?php
function f(\SplFileObject $file): void {
    \PHPStan\dumpType($file->noSuchMethodAnywhere());
}
"#;
    assert_eq!(one_type(src), "unknown");
}

// ---------------------------------------------------------------------------
// No countersign, no row — the two SILENT countersigns (review finding 1).
//
// The function half admits a row the engine declares nothing for, and one it
// declares `mixed` for, because ADR-0056's reflected envelope sits above it at
// analysis time and corrects it per name. Nothing sits above this table, and the
// walk above hands a row to every DESCENDANT on a covariance argument that only a
// native, non-`mixed` envelope can make. So the miner refuses both, and these are
// the answers PHP itself gave when the review checked them on 8.5.10.
// ---------------------------------------------------------------------------

/// `Exception::getCode` is `final` and carries no declared return type, so
/// functionMap's `int` on the exception classes bounded nothing — and the walk
/// handed that `int` to `PDOException`, whose code is the SQLSTATE **string**
/// `"HY000"`. The row is gone, so the answer is silence.
#[test]
fn a_row_the_engine_declares_no_return_type_for_answers_nothing() {
    let src = r#"<?php
function f(\PDOException $e): void {
    \PHPStan\dumpType($e->getCode());
}
"#;
    assert_eq!(one_type(src), "unknown");
}

/// The same refusal reached through a project child, which is where it bites
/// hardest: `$code` is a plain untyped property and any subclass may set it to a
/// string, exactly as PDO's own does.
#[test]
fn a_project_child_of_an_untyped_builtin_method_answers_nothing() {
    let src = r#"<?php
class ParseFailure extends \RuntimeException {
    protected $code = 'E_PARSE';
}
function f(ParseFailure $e): void {
    \PHPStan\dumpType($e->getCode());
}
"#;
    assert_eq!(one_type(src), "unknown");
}

/// `Iterator::key(): mixed` is a real declared type that bounds nothing, and
/// `mixed` subsumes every row, so the arm-wise countersign passed vacuously.
/// functionMap says `string`; PHP hands back `int(0)`.
#[test]
fn a_row_the_engine_declares_mixed_for_answers_nothing() {
    let src = r#"<?php
function f(\DirectoryIterator $d): void {
    \PHPStan\dumpType($d->key());
}
"#;
    assert_eq!(one_type(src), "unknown");
}

/// The same vacuous countersign, on the row whose wrongness is a data-type
/// question rather than a hierarchy one: `PDOStatement::fetchColumn` is declared
/// `mixed`, functionMap states `int|string|false|null`, and PHP 8.1+ returns a
/// `float` for a REAL column.
#[test]
fn a_mixed_engine_envelope_refuses_even_a_plausible_row() {
    let src = r#"<?php
function f(\PDOStatement $s): void {
    \PHPStan\dumpType($s->fetchColumn());
}
"#;
    assert_eq!(one_type(src), "unknown");
}

/// An untyped row that is wrong about the *shape* rather than the width:
/// functionMap says `mysqli::init()` returns a `mysqli`, and it returns `NULL`.
#[test]
fn an_untyped_engine_method_refuses_an_object_row_too() {
    let src = r#"<?php
function f(\mysqli $m): void {
    \PHPStan\dumpType($m->init());
}
"#;
    assert_eq!(one_type(src), "unknown");
}

// ---------------------------------------------------------------------------
// Shadows: a key the miner refused is not the same fact as silence
// (review finding 2).
//
// Only 957 of the 6,606 reduced keys became rows, so "no row on the child" cannot
// be read as "the map said nothing about the child". Where functionMap STATES a
// key and the miner dropped or refused it, that is positive evidence the child's
// declaration differs from the ancestor's, and the walk stops there.
// ---------------------------------------------------------------------------

/// `SplFileInfo::getMTime` is admitted; `DirectoryIterator::getMTime` is the same
/// map's row for the child, refused because it states `int` where the engine
/// declares `int|false`. The walk used to climb straight past that refusal. What
/// makes it wrong is not that the ancestor's envelope is unsound here — it happens
/// to hold — but that the *source* said something about the child and the table
/// could not carry it: inheriting is then a guess dressed as a lookup, and it is
/// the guess that answered `int` for `PDOException::getCode`. The cost of the
/// retreat is visible and small (25 keys), the cost of the guess was a wrong
/// answer nothing downstream could correct.
#[test]
fn a_refused_child_key_shadows_its_ancestors_row() {
    assert!(steins_catalog::declared_method_return("SplFileInfo", "getMTime").is_some());
    assert!(steins_catalog::declared_method_return_blocked("DirectoryIterator", "getMTime"));
    let src = r#"<?php
function f(\DirectoryIterator $d): void {
    \PHPStan\dumpType($d->getMTime());
}
"#;
    assert_eq!(one_type(src), "unknown");
}

/// The ancestor itself is untouched: the shadow blocks the walk, it does not
/// retract the row. This is the pair that makes the previous test about the gate
/// rather than about a missing row.
#[test]
fn the_shadowed_ancestor_still_answers_for_its_own_receiver() {
    let src = r#"<?php
function f(\SplFileInfo $i): void {
    \PHPStan\dumpType($i->getMTime());
}
"#;
    assert_eq!(one_type(src), "int|false (asserted)");
}

/// A class **between** the receiver and the row-bearing ancestor is enough:
/// `RecursiveDirectoryIterator` → `FilesystemIterator` → `DirectoryIterator` (the
/// shadow) → `SplFileInfo` (the row). Neither of the first two is blocked itself,
/// and the walk must still stop.
#[test]
fn a_shadow_between_the_receiver_and_the_row_stops_the_walk() {
    assert!(!steins_catalog::declared_method_return_blocked("RecursiveDirectoryIterator", "getMTime"));
    let src = r#"<?php
function f(\RecursiveDirectoryIterator $r): void {
    \PHPStan\dumpType($r->getMTime());
}
"#;
    assert_eq!(one_type(src), "unknown");
}

/// A project class inherits the shadow along with everything else: the chain
/// leaves the project at `DirectoryIterator`, which is where the walk stops.
#[test]
fn a_project_child_inherits_the_shadow() {
    let src = r#"<?php
class Walker extends \DirectoryIterator {}
function f(Walker $w): void {
    \PHPStan\dumpType($w->getMTime());
}
"#;
    assert_eq!(one_type(src), "unknown");
}

/// The shadow is keyed on the **pair**, not on the class: `DirectoryIterator` is
/// blocked for `getMTime` and for nothing else, so its other inherited rows still
/// answer. A class-wide block would be a much bigger retreat than the evidence
/// supports.
#[test]
fn a_shadow_blocks_one_method_and_not_its_class() {
    assert!(!steins_catalog::declared_method_return_blocked("DirectoryIterator", "getPath"));
    let src = r#"<?php
function f(\DirectoryIterator $d): void {
    \PHPStan\dumpType($d->getPath());
}
"#;
    assert_eq!(one_type(src), "string (asserted)");
}

/// The two tables must never both answer for one key — a key that is admitted and
/// blocked at once would make the walk's verdict depend on the order the two
/// lookups happen to be written in. The generator refuses such a pair; this is the
/// shipped-side assertion that it did.
#[test]
fn the_row_table_and_the_shadow_table_are_disjoint() {
    for (class, method) in [
        ("DirectoryIterator", "getMTime"),
        ("ArrayObject", "getIterator"),
        ("ParentIterator", "valid"),
        ("SplQueue", "setIteratorMode"),
    ] {
        assert!(
            steins_catalog::declared_method_return_blocked(class, method),
            "{class}::{method} is a shadow key"
        );
        assert_eq!(
            steins_catalog::declared_method_return(class, method),
            None,
            "{class}::{method} is blocked, so it must carry no row"
        );
    }
}

// ---------------------------------------------------------------------------
// The static twin.
// ---------------------------------------------------------------------------

/// A static call on a builtin, resolved by name with no receiver at all.
#[test]
fn a_static_builtin_call_answers_from_the_table() {
    let src = r#"<?php
function f(): void {
    \PHPStan\dumpType(\PDO::getAvailableDrivers());
}
"#;
    assert_eq!(one_type(src), "array (asserted)");
}

/// The static **form** on an instance row is a PHP 8 `Error` outside a class, so
/// the call never returns and there is no envelope to hand the statement after it —
/// the same parity `resolve_static_named` and the declaration path already keep.
#[test]
fn the_static_form_on_an_instance_row_answers_nothing_outside_a_class() {
    let src = r#"<?php
function f(): void {
    \PHPStan\dumpType(\SplFileObject::fgets());
}
"#;
    assert_eq!(one_type(src), "unknown");
}

/// The instance form on a **static** row still answers: PHP lets an instance
/// receiver call a static method, so only the static direction constrains.
#[test]
fn the_instance_form_on_a_static_row_still_answers() {
    let src = r#"<?php
function f(\PDO $pdo): void {
    \PHPStan\dumpType($pdo->getAvailableDrivers());
}
"#;
    assert_eq!(one_type(src), "array (asserted)");
}

// ---------------------------------------------------------------------------
// Composition with the project chain (issue #619's path, and the two must agree).
// ---------------------------------------------------------------------------

/// A project class extending a builtin, on a name the **child declares**: the
/// child's own declaration wins, through `resolve_declaration_target`. The table is
/// never consulted — `resolve_builtin_callee` refuses outright once a project class
/// on the chain declares the name — so the two can never disagree here.
#[test]
fn a_child_declaration_wins_over_the_inherited_table_row() {
    let src = r#"<?php
class Reader extends \SplFileObject {
    public function fgets(): string { return 'x'; }
}
function f(Reader $r): void {
    \PHPStan\dumpType($r->fgets());
}
"#;
    // The child's native `: string` is runtime-enforced, so it rides Verified and
    // renders without the `(asserted)` marker — which is how a reader can tell
    // which of the two answered.
    assert_eq!(one_type(src), "string");
}

/// The same class, on a name it **inherits**: the project chain runs out at
/// `SplFileObject`, which has no `ClassDecl`, and the table answers on the
/// inherited name.
#[test]
fn an_inherited_name_answers_from_the_table_through_a_project_child() {
    let src = r#"<?php
class Reader extends \SplFileObject {
    public function fgets(): string { return 'x'; }
}
function f(Reader $r): void {
    \PHPStan\dumpType($r->getPath());
}
"#;
    assert_eq!(one_type(src), "string (asserted)");
}

/// The refusal that makes the previous test's non-disagreement *structural* rather
/// than incidental. A child declaring `fgets(): static` is a name the declaration
/// path cannot answer — ADR-0049 A19 defers `static` with its reason — so if the
/// table were free to step in it would hand back `SplFileObject`'s `string` and
/// contradict a declaration sitting right there in the project. `builtin_root`
/// refuses on the declaration, not on whether the declaration path used it.
#[test]
fn a_child_declaration_the_declaration_path_defers_still_blocks_the_table() {
    let src = r#"<?php
class Reader extends \SplFileObject {
    public function fgets(): static { return $this; }
}
function f(Reader $r): void {
    \PHPStan\dumpType($r->fgets());
}
"#;
    assert_eq!(one_type(src), "unknown");
}

/// `$this` inside a class extending a builtin reaches the same walk from the other
/// side: the enclosing class is the declared receiver, and the chain leaves the
/// project one hop up.
#[test]
fn this_inside_a_builtin_subclass_answers_from_the_table() {
    let src = r#"<?php
class Reader extends \SplFileObject {
    public function head(): void {
        \PHPStan\dumpType($this->fgets());
    }
}
"#;
    assert_eq!(one_type(src), "string (asserted)");
}

/// A project chain that ends **inside the project** answers nothing: no builtin is
/// involved, so the absent name is the absence family's question and not this
/// table's.
#[test]
fn a_chain_that_never_leaves_the_project_answers_nothing() {
    let src = r#"<?php
class Base {}
class Reader extends Base {}
function f(Reader $r): void {
    \PHPStan\dumpType($r->fgets());
}
"#;
    assert_eq!(one_type(src), "unknown");
}

/// A class that `use`s a trait and does not declare the name refuses, for
/// ADR-0049 A18's reason: trait method bodies and return types are not lowered, so
/// "not found on the class" cannot be read as "inherited from the parent" while a
/// trait could be declaring it.
#[test]
fn a_trait_using_child_refuses_the_inherited_row() {
    let src = r#"<?php
trait T {}
class Reader extends \SplFileObject {
    use T;
}
function f(Reader $r): void {
    \PHPStan\dumpType($r->getPath());
}
"#;
    assert_eq!(one_type(src), "unknown");
}

// ---------------------------------------------------------------------------
// The grade, pinned (ADR-0069 §2).
// ---------------------------------------------------------------------------

/// Every arm renders with the `(asserted)` marker, whatever the row's shape —
/// scalar, failure union, object. There is no Verified twin waiting to be built: a
/// native stub's `@return` would be one, and functionMap is a third party's claim
/// about the engine rather than a stub.
#[test]
fn every_row_renders_asserted() {
    for (src, expected) in [
        ("\\PHPStan\\dumpType($f->fgets());", "string (asserted)"),
        ("\\PHPStan\\dumpType($f->getRealPath());", "string|false (asserted)"),
        ("\\PHPStan\\dumpType($f->getPathInfo());", "SplFileInfo|null (asserted)"),
    ] {
        let body = format!("<?php\nfunction f(\\SplFileObject $f): void {{\n    {src}\n}}\n");
        assert_eq!(one_type(&body), expected, "for {src}");
    }
}

/// The negative pin, and the one that matters most: a table row may not premise a
/// proof-layer finding. Each of these shapes would be a `type.*` conviction if the
/// arms rode `Verified` — a `string|false` handed to `strlen`, a `string` returned
/// from a `: int` function, a `DOMElement|null` returned from a `: DOMElement` one
/// — and each is exactly the laundering ADR-0069 §2's firewall exists to prevent.
/// The whole file must produce nothing at all but the dumps.
#[test]
fn an_asserted_row_premises_no_definite_finding() {
    let src = r#"<?php
declare(strict_types=1);
function a(\SplFileObject $file): void { strlen($file->getRealPath()); }
function b(\SplFileObject $file): int { return $file->fgets(); }
function c(\DOMDocument $dom): \DOMElement { return $dom->getElementById('x'); }
function d(\SplFileObject $file): void { echo $file->getSize() . 'x'; }
"#;
    let ids: Vec<String> = findings(src).into_iter().map(|d| d.id.to_owned()).collect();
    assert!(ids.is_empty(), "a table row must premise no finding at all, got {ids:?}");
}

// ---------------------------------------------------------------------------
// Version discipline (ADR-0069 §3, A11-shaped).
// ---------------------------------------------------------------------------

/// The two method tables are **disjoint at this pin**, so the version gate has no
/// end-to-end fixture — and this assertion is the tripwire that will demand one.
///
/// All three version-sensitive keys return `static`, which is a keyword the arm lane
/// cannot carry (ADR-0049 A19), so none of them has an admitted row. That is exactly
/// the state ADR-0069 §5 recorded for the *function* tables at #79 — "all four
/// version-sensitive names return arrays, so the two mined tables are disjoint" —
/// and the assertion written there is what fired when ADR-0071 widened
/// carriability. This one is written to fire the same way: when a later pin admits
/// a version-sensitive method row, this test fails and the fixture it is standing in
/// for has to be written.
#[test]
fn the_method_table_and_its_version_oracle_are_disjoint_at_this_pin() {
    for (class, method) in [
        ("DateInterval", "createFromDateString"),
        ("DateTime", "modify"),
        ("DateTimeImmutable", "modify"),
    ] {
        assert_eq!(
            steins_catalog::declared_method_return_changed_at(class, method),
            Some((8, 3)),
            "{class}::{method} is the oracle's row"
        );
        assert_eq!(
            steins_catalog::declared_method_return(class, method),
            None,
            "{class}::{method} returns `static`, so the arm lane cannot carry it — \
             if this now has a row, the version gate has its first end-to-end fixture \
             and this test owes it one"
        );
    }
    // A name the oracle does not list stands at every target, which is the gate's
    // normal case and what every other test in this file exercises.
    assert_eq!(steins_catalog::declared_method_return_changed_at("SplFileObject", "key"), None);
    let src = r#"<?php
function f(\SplFileObject $file): void {
    \PHPStan\dumpType($file->key());
}
"#;
    assert_eq!(one_type(src), "int (asserted)");
}
