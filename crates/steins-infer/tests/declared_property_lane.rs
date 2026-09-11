//! ADR-0049 A20 / issue #620 — the declared-property lane.
//!
//! ADR-0036 keeps declaration-derived property *values* out of the heap on
//! purpose. It says nothing against the declared **type**, which PHP enforces on
//! every write to a natively typed slot, so a property read with no in-trace fact
//! can answer the declaration rather than `unknown`.
//!
//! This file pins what the lane answers, what it refuses, which stratum the answer
//! rides at, and — the half that matters most — that the in-trace write still wins
//! and that no body-walking or proof-layer consumer moved.

use steins_db::{Project, SourceFile, SteinsDatabase};
use steins_infer::promote::{MethodSweep, sweep_methods};
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
// What the lane answers.
// ---------------------------------------------------------------------------

/// The headline (the issue's first acceptance row): a native property type on a
/// declared parameter receiver, with nothing in the trace about the slot.
#[test]
fn a_native_property_on_a_declared_receiver_answers_its_hint() {
    let src = r#"<?php
class C { public int $x = 1; }
function f(C $c): void {
    \PHPStan\dumpType($c->x);
}
"#;
    assert_eq!(one_type(src), "int");
}

/// The same read on `$this`, whose heap shell seeds no property values at all
/// (ADR-0036) — the lane's other principal site.
#[test]
fn a_native_property_on_this_answers_its_hint() {
    let src = r#"<?php
class C {
    public string $name = 'a';
    public function show(): void { \PHPStan\dumpType($this->name); }
}
"#;
    assert_eq!(one_type(src), "string");
}

/// The issue's second acceptance row: an untyped property whose only description
/// is a `@var`, read through `$this`. The arm is `Asserted`, so the dump carries
/// the marker — a docblock claim never launders into a proven value.
#[test]
fn a_var_docblock_on_this_renders_asserted() {
    let src = r#"<?php
class C {
    /** @var int<0, max> */
    public $max = 0;
    public function show(): void { \PHPStan\dumpType($this->max); }
}
"#;
    assert_eq!(one_type(src), "int<0, max> (asserted)");
}

/// A nullable hint is a two-arm lane and spells as one.
#[test]
fn a_nullable_hint_keeps_its_null_arm() {
    let src = r#"<?php
class C { public ?string $s = null; }
function f(C $c): void {
    \PHPStan\dumpType($c->s);
}
"#;
    assert_eq!(one_type(src), "string|null");
}

/// A class-typed property answers the class, source-cased — membership, never
/// exactness (the runtime value may be any subclass).
#[test]
fn a_class_typed_property_answers_its_class() {
    let src = r#"<?php
class Dep {}
class C { public Dep $dep; }
function f(C $c): void {
    \PHPStan\dumpType($c->dep);
}
"#;
    assert_eq!(one_type(src), "Dep");
}

/// A promoted constructor parameter is a property declaration too, and carries its
/// hint through lowering.
#[test]
fn a_promoted_constructor_property_answers_its_hint() {
    let src = r#"<?php
class C {
    public function __construct(public int $n) {}
}
function f(C $c): void {
    \PHPStan\dumpType($c->n);
}
"#;
    assert_eq!(one_type(src), "int");
}

// ---------------------------------------------------------------------------
// The carrier and the hierarchy rules (A17, mirroring #673's shape).
// ---------------------------------------------------------------------------

/// Inherited: the receiver's own class declares nothing, so the ancestor's
/// declaration is the answer.
#[test]
fn an_inherited_property_answers_from_the_ancestor() {
    let src = r#"<?php
class Base { public int $count = 0; }
class Child extends Base {}
function f(Child $c): void {
    \PHPStan\dumpType($c->count);
}
"#;
    assert_eq!(one_type(src), "int");
}

/// Project-declaration-wins: a child that redeclares the property is met first by
/// the chain walk, so its docblock — not the parent's — describes the slot. PHP
/// requires the native type to be kept identically, which is why only the
/// docblocks can differ here.
#[test]
fn a_child_redeclaration_wins_over_the_parent() {
    let src = r#"<?php
class Base {
    /** @var string */
    public $tag = '';
}
class Child extends Base {
    /** @var 'a'|'b' */
    public $tag = 'a';
}
function f(Child $c): void {
    \PHPStan\dumpType($c->tag);
}
"#;
    assert_eq!(one_type(src), "'a'|'b' (asserted)");
}

/// A builtin ancestor answers nothing (issue #715): the chain leaves the project
/// and this lane has no table to consult there.
#[test]
fn a_builtin_ancestor_answers_nothing() {
    let src = r#"<?php
class MyEx extends RuntimeException {}
function f(MyEx $e): void {
    \PHPStan\dumpType($e->message);
}
"#;
    assert_eq!(one_type(src), "unknown");
}

/// The receiver carrier demotes the whole answer (A13/A17): a native `int`
/// property read through a docblock-only receiver is `Asserted`, because the
/// premise that `$o` is a `C` at all is.
#[test]
fn an_asserted_receiver_demotes_a_native_property() {
    let src = r#"<?php
class C { public int $x = 1; }
/** @param C $c */
function f($c): void {
    \PHPStan\dumpType($c->x);
}
"#;
    assert_eq!(one_type(src), "int (asserted)");
}

/// A receiver whose declared lane is a union of two classes declines: the answer
/// would be the union of two declarations and A17 does not build that floor.
#[test]
fn a_two_class_receiver_lane_declines() {
    let src = r#"<?php
class A { public int $x = 1; }
class B { public int $x = 2; }
/** @param A|B $o */
function f($o): void {
    \PHPStan\dumpType($o->x);
}
"#;
    assert_eq!(one_type(src), "unknown");
}

// ---------------------------------------------------------------------------
// What wins, and what the lane refuses.
// ---------------------------------------------------------------------------

/// The heap wins: a write the trace saw states what the slot holds *now*, which
/// is strictly sharper than what it may ever hold.
#[test]
fn an_in_trace_write_beats_the_declaration() {
    let src = r#"<?php
class C { public int $x = 1; }
function f(C $c): void {
    $c->x = 7;
    \PHPStan\dumpType($c->x);
}
"#;
    assert_eq!(one_type(src), "7");
}

/// The ~55 written-then-read rows: the write happened, but its rvalue is
/// unspellable, so the heap recorded nothing to beat the floor. The declaration is
/// the answer there too — and it is a sound one, because PHP type-checked that
/// very write against the hint.
#[test]
fn a_write_with_an_unspellable_rvalue_falls_back_to_the_declaration() {
    let src = r#"<?php
class C { public int $x = 0; }
function f(C $c, $u): void {
    $c->x = $u;
    \PHPStan\dumpType($c->x);
}
"#;
    assert_eq!(one_type(src), "int");
}

/// The same floor after a **sweep**: passing the object to a call drops its
/// non-readonly property facts (ADR-0036), and what the declaration says survives
/// that, because a callee's write is type-checked too.
#[test]
fn a_swept_property_falls_back_to_the_declaration() {
    let src = r#"<?php
class C { public int $x = 0; }
function g(C $o): void {}
function f(C $c): void {
    $c->x = 7;
    g($c);
    \PHPStan\dumpType($c->x);
}
"#;
    assert_eq!(one_type(src), "int");
}

/// An enum-typed property carries the same enforced case set a `: Suit` return
/// does, and the whole set collapses back to the enum's own name for the reader.
#[test]
fn an_enum_typed_property_answers_its_enum() {
    let src = r#"<?php
enum Suit { case Hearts; case Spades; }
class C { public Suit $s; }
function f(C $c): void {
    \PHPStan\dumpType($c->s);
}
"#;
    assert_eq!(one_type(src), "Suit");
}

/// A class-level `@template` name shadows a same-named class inside the property's
/// `@var` (issue #5) — a property docblock is a member docblock too, and the write
/// side neutralizes them for the same reason. `T` names nothing spellable, so the
/// lane answers nothing rather than a class called `T`.
#[test]
fn a_class_level_template_name_in_a_var_is_neutralized() {
    let src = r#"<?php
/** @template T */
class Box {
    /** @var T */
    public $item;
}
function f(Box $b): void {
    \PHPStan\dumpType($b->item);
}
"#;
    assert_eq!(one_type(src), "unknown");
}

/// An unrepresentable hint lowers to nothing, exactly as an unrepresentable
/// declared return does — never a `mixed` pretense.
#[test]
fn an_unrepresentable_hint_answers_nothing() {
    let src = r#"<?php
class C { public array $rows = []; }
function f(C $c): void {
    \PHPStan\dumpType($c->rows);
}
"#;
    assert_eq!(one_type(src), "unknown");
}

/// A property the chain never declares is the absence family's question, not this
/// lane's.
#[test]
fn an_undeclared_property_answers_nothing() {
    let src = r#"<?php
class C { public int $x = 1; }
function f(C $c): void {
    \PHPStan\dumpType($c->nope);
}
"#;
    assert_eq!(one_type(src), "unknown");
}

/// A `static` property is out of scope (ADR-0052 N5, owner-deferred): the chain
/// walk skips the declaration, so a same-named instance read answers nothing.
#[test]
fn a_static_property_is_not_this_lanes_business() {
    let src = r#"<?php
class C { public static int $x = 1; }
function f(C $c): void {
    \PHPStan\dumpType($c->x);
}
"#;
    assert_eq!(one_type(src), "unknown");
}

/// A property a class obtains from a **trait** answers nothing, for ADR-0049 A18's
/// reason one rung over: trait members are not lowered into the using class, so
/// "not declared on the chain" cannot be read as "absent" while a trait could be
/// declaring it — and there is no declaration here to read either way.
#[test]
fn a_trait_property_answers_nothing() {
    let src = r#"<?php
trait T { public int $x = 1; }
class C { use T; }
function f(C $c): void {
    \PHPStan\dumpType($c->x);
}
"#;
    assert_eq!(one_type(src), "unknown");
}

/// A class-body hooked property (PHP 8.4) answers nothing, and does so one rung
/// earlier than the refusal: the declaration is dropped entirely at lowering, so
/// the chain walk never finds a name to read.
#[test]
fn a_class_body_hooked_property_answers_nothing() {
    let src = r#"<?php
class C {
    public int $x { get => 1; }
    public int $y = 2;
}
function f(C $c): void {
    \PHPStan\dumpType($c->x);
}
"#;
    assert_eq!(one_type(src), "unknown");
}

/// A **promoted** hooked property is the spelling that survives lowering, carrying
/// its `hooked` flag — the refusal's first half. Its value is whatever the hook
/// computes, and this crate's rule for the hooked surface is that it binds no fact
/// ever (FP class 16); the read side keeps the rule the write side keeps.
#[test]
fn a_promoted_hooked_property_answers_nothing() {
    let src = r#"<?php
class C {
    public function __construct(public int $n { get => 1; }) {}
}
function f(C $c): void {
    \PHPStan\dumpType($c->n);
}
"#;
    assert_eq!(one_type(src), "unknown");

    // The same declaration without the hook does answer — so the silence above is
    // the refusal talking, not the promoted spelling failing to reach the lane.
    let unhooked = r#"<?php
class C {
    public function __construct(public int $n) {}
}
function f(C $c): void {
    \PHPStan\dumpType($c->n);
}
"#;
    assert_eq!(one_type(unhooked), "int");
}

/// The refusal's second half: a child that **hooks** a property its parent
/// declares plainly. The chain walk finds the parent's declaration, which knows
/// nothing about the hook, so the hooked-name query is asked separately — exactly
/// as the write side asks it.
#[test]
fn a_child_hooking_an_inherited_property_answers_nothing() {
    let src = r#"<?php
class Base { public int $x = 1; }
class Child extends Base {
    public int $x { get => 2; }
}
function f(Child $c): void {
    \PHPStan\dumpType($c->x);
}
"#;
    assert_eq!(one_type(src), "unknown");
}

// ---------------------------------------------------------------------------
// What must not have moved.
// ---------------------------------------------------------------------------

/// `descend` is unchanged: a declared property type is a membership claim about a
/// slot, not a proven value, so it does not flow into a callee's **untyped**
/// parameter — which is the spelling that would show it if it did. The callee's
/// dump still sees nothing.
#[test]
fn descend_does_not_receive_a_declared_property_as_a_value() {
    let src = r#"<?php
class C { public int $x = 1; }
function inner($n): void { \PHPStan\dumpType($n); }
function f(C $c): void {
    inner($c->x);
}
"#;
    assert_eq!(one_type(src), "unknown");
}

/// `promote` is unchanged: the reverse call-site sweep records an *observed
/// argument value* against a resolved target, and a declared property type is a
/// membership claim about a slot, never a value. The sweep sees the call and
/// records nothing about the argument.
#[test]
fn promote_records_no_argument_value_for_a_declared_property_read() {
    let src = "<?php\nfinal class C {\n  public int $x = 1;\n  public function take($p) { return $p; }\n  public function go() { return $this->take($this->x); }\n}\n";
    let sweep = method_sweep(&[("c.php", src)]);
    assert_eq!(sweep.targets.len(), 1, "the `final` receiver resolves, so the sweep sees the call");
    let observed = format!("{:?}", sweep.targets);
    assert!(
        !observed.contains("Int"),
        "a declared property type must not enter the sweep as an observed argument value: {observed}"
    );
}

fn method_sweep(files: &[(&str, &str)]) -> MethodSweep {
    let db = SteinsDatabase::default();
    let inputs: Vec<SourceFile> =
        files.iter().map(|(p, t)| SourceFile::new(&db, (*p).to_owned(), (*t).to_owned())).collect();
    let project = Project::new(
        &db,
        inputs,
        steins_db::ProjectLayout::fallback(),
        steins_db::PluginFacts::none(),
    );
    sweep_methods(&db, project)
}

/// An assert consumer is unchanged: `assert()` narrows the *variable* it names and
/// the declared-property floor neither feeds it nor is refined by it. The read
/// still answers the declaration, not the assert's claim.
#[test]
fn an_assert_does_not_refine_the_declared_property_floor() {
    let src = r#"<?php
class C { public int $x = 1; }
function f(C $c): void {
    assert($c->x > 5);
    \PHPStan\dumpType($c->x);
}
"#;
    assert_eq!(one_type(src), "int");
}

/// No proof-layer finding premises on a `@var`-derived arm. The property's only
/// description is a docblock claiming a type the argument would violate; the
/// argument check must stay silent (or speak at the contract layer), never emit a
/// `type.*` id.
#[test]
fn no_proof_layer_finding_premises_on_a_var_arm() {
    let src = r#"<?php
class C {
    /** @var string */
    public $s = 'a';
}
function want_int(int $n): void {}
function f(C $c): void {
    want_int($c->s);
}
"#;
    // The lane really did answer here — otherwise the silence below would be the
    // fixture failing to reach it rather than the stratum rule talking.
    let dumped = types(&src.replace("want_int($c->s);", "\\PHPStan\\dumpType($c->s);"));
    assert_eq!(dumped, vec!["string (asserted)".to_owned()]);

    let proof: Vec<String> = findings(src)
        .into_iter()
        .filter(|d| d.id.starts_with("type."))
        .map(|d| format!("{}: {}", d.id, d.message))
        .collect();
    assert!(proof.is_empty(), "a @var-derived arm premised a proof-layer finding: {proof:?}");
}
