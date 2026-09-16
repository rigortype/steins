//! ADR-0049 A16-A19 / issue #619 — a declaration-only target for an inexact receiver.
//!
//! Dispatch admissibility (audit G1) refuses every receiver whose runtime class is
//! unproven, and that refusal is right for every consumer that acts on the resolved
//! method. It is wrong for the one consumer that only asks what the call returns:
//! PHP enforces return covariance at class-declaration time, so the declaring
//! method's envelope is a sound upper bound under every descendant.
//!
//! This file pins the second resolver A16 rules in — what it answers, what it
//! still refuses, which stratum the answer rides at, and, above all, that the
//! body-walking consumers (`descend`, `promote`, `asserts`) never see it.

use steins_db::{Project, SourceFile, SteinsDatabase};
use steins_infer::promote::{MethodSweep, sweep_methods};
use steins_infer::{
    DEBUG_PHPDOC_TYPE_ID, DEBUG_TYPE_ID, Diagnostic, OVERRIDE_RETURN_VARIANCE_ID, check,
};
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

fn phpdoc_types(src: &str) -> Vec<String> {
    findings(src)
        .into_iter()
        .filter(|d| d.id == DEBUG_PHPDOC_TYPE_ID)
        .map(|d| d.message.replace("dumped phpdoc type: ", ""))
        .collect()
}

// ---------------------------------------------------------------------------
// A16: the declaration answers where dispatch refuses.
// ---------------------------------------------------------------------------

/// The headline: an open class, an overridable method, a declared parameter
/// receiver. `resolve_guarded` refuses (nothing is `final`); the declaration path
/// reads `: string` off the declaration the receiver's declared chain names.
#[test]
fn a_non_final_receiver_answers_from_its_declaration() {
    let src = r#"<?php
class Foo {
    public function getName(): string { return $this->n; }
}
function f(Foo $foo): void {
    \PHPStan\dumpType($foo->getName());
}
"#;
    assert_eq!(one_type(src), "string");
}

/// The same call under the dispatch resolver's own consumers stays refused: the
/// body is never walked, so a value the body proves (`'concrete'`) does not reach
/// the caller. Only the declared envelope does.
#[test]
fn the_declaration_path_yields_the_envelope_never_the_body() {
    let src = r#"<?php
class Foo {
    public function tag(): string { return 'concrete'; }
}
function f(Foo $foo): void {
    \PHPStan\dumpType($foo->tag());
}
"#;
    assert_eq!(one_type(src), "string");
}

/// A `final` receiver keeps every sharper answer it had: the declaration path is a
/// fallback reached only after `resolve_call_target` declines, so an exact `new`
/// receiver still descends into the body and still reads its proven value.
#[test]
fn a_proven_receiver_keeps_the_sharper_body_answer() {
    let src = r#"<?php
final class Foo {
    public function tag(): string { return 'concrete'; }
}
function f(): void {
    $foo = new Foo();
    \PHPStan\dumpType($foo->tag());
}
"#;
    assert_eq!(one_type(src), "'concrete'");
}

/// `$this->` in an open class — the commonest receiver in the corpus, and the one
/// `resolve_guarded` refuses on the declaring class's own non-finality.
#[test]
fn this_in_an_open_class_answers_from_its_declaration() {
    let src = r#"<?php
class Foo {
    public function width(): int { return 1; }
    public function show(): void {
        \PHPStan\dumpType($this->width());
    }
}
"#;
    assert_eq!(one_type(src), "int");
}

// ---------------------------------------------------------------------------
// A17: the receiver carrier is the declared-receiver lane, not a heap object.
// ---------------------------------------------------------------------------

/// A `?C` parameter seeds no heap object at all (`seed_declared_param_object`
/// declines a nullable hint), so refusal (1) killed it before finality was ever
/// consulted. Its contract lane past the null guard is a one-arm list, and that is
/// the carrier A17 rules in.
#[test]
fn a_nullable_receiver_resolves_after_its_null_guard() {
    let src = r#"<?php
interface Reservation {
    public function isFoo(): bool;
}
function f(?Reservation $r): void {
    if ($r !== null) {
        \PHPStan\dumpType($r->isFoo());
    }
}
"#;
    assert_eq!(one_type(src), "bool");
}

/// `@param object $foo` narrowed by `assert($foo instanceof Foo)`: the narrowing
/// binds a `Member` fact and subtracts from the contract lane — it never mints a
/// heap object. The `Member` fact's single class is the carrier, at `Asserted`.
#[test]
fn an_instanceof_narrowed_object_param_resolves() {
    let src = r#"<?php
class Foo {
    public function getName(): string { return 'x'; }
}
/** @param object $foo */
function f($foo): void {
    assert($foo instanceof Foo);
    \PHPStan\dumpType($foo->getName());
}
"#;
    assert_eq!(one_type(src), "string (asserted)");
}

// ---------------------------------------------------------------------------
// A18: abstract lifts, trait-provided stays refused.
// ---------------------------------------------------------------------------

/// An abstract declaration is the richest floor source in the corpus: it has a
/// return type and a docblock and no body a walker could mistake for the one that
/// runs. `resolve_in_chain` refused it before finality was ever the question.
#[test]
fn an_abstract_declaration_lifts() {
    let src = r#"<?php
abstract class Base {
    abstract public function retain(?bool $input): ?string;
}
function f(Base $b): void {
    \PHPStan\dumpType($b->retain(null));
}
"#;
    assert_eq!(one_type(src), "string|null");
}

/// A method the class obtains from a **trait** stays refused: trait bodies and
/// return types are not lowered, so "not found on the class" must not be read as
/// "absent" while that is so.
#[test]
fn a_trait_provided_method_stays_refused() {
    let src = r#"<?php
trait Naming {
    public function getName(): string { return 'x'; }
}
class Foo {
    use Naming;
}
function f(Foo $foo): void {
    \PHPStan\dumpType($foo->getName());
}
"#;
    assert_eq!(one_type(src), "unknown");
}

/// A trait-using class that declares the name **itself** answers: no trait
/// resolution was needed to find it (A18's "on the class itself" leg).
#[test]
fn a_trait_using_class_answers_for_a_name_it_declares_itself() {
    let src = r#"<?php
trait Helping {
    public function help(): int { return 1; }
}
class Foo {
    use Helping;
    public function getName(): string { return 'x'; }
}
function f(Foo $foo): void {
    \PHPStan\dumpType($foo->getName());
}
"#;
    assert_eq!(one_type(src), "string");
}

// ---------------------------------------------------------------------------
// A19: `@return static` / LSB deferred, with the reason.
// ---------------------------------------------------------------------------

/// A `: static` return binds to the *calling* receiver's type, which is a
/// template-binding question over the receiver's arms and not a dispatch one.
/// `override_return_widens` already goes silent on `ret_bound_keyword`; the
/// declaration path does the same rather than answering `self` and losing the
/// identity.
#[test]
fn a_static_return_bound_is_deferred() {
    let src = r#"<?php
class Foo {
    public function me(): static { return $this; }
}
function f(Foo $foo): void {
    \PHPStan\dumpType($foo->me());
}
"#;
    assert_eq!(one_type(src), "unknown");
}

/// `: self` and `: parent` ride the same field and take the same deferral — one
/// gate, not three.
#[test]
fn a_self_return_bound_is_deferred_too() {
    let src = r#"<?php
class Foo {
    public function me(): self { return $this; }
}
function f(Foo $foo): void {
    \PHPStan\dumpType($foo->me());
}
"#;
    assert_eq!(one_type(src), "unknown");
}

// ---------------------------------------------------------------------------
// The unrepresentable-hint gate (#603), from the other side.
// ---------------------------------------------------------------------------

/// A bare `: array` on an open method is still silent here, and #603 CHANGED WHICH
/// GATE holds it. The lowering gate no longer does: `: array` now lowers to the
/// enforced top `array` (ADR-0057 note), and a proven receiver binds it. What keeps
/// the declaration path silent is A16's own withholding — `CallTarget::enforced_top`
/// answers `None` for a target only `resolve_declaration_target` produced — so the
/// bullet's list of unrepresentable hints is now a list of *withheld* ones, and the
/// pin it asked for reads the same way from the outside.
///
/// Nothing in A16's covariance argument forbids the top: a child overriding
/// `: array` can only narrow it. Extending the path to seed it is #603's stated
/// out-of-scope and wants its own slice.
#[test]
fn a_bare_array_return_is_withheld_from_the_declaration_path() {
    let src = r#"<?php
class Foo {
    public function all(): array { return []; }
}
function f(Foo $foo): void {
    \PHPStan\dumpType($foo->all());
}
"#;
    assert_eq!(one_type(src), "unknown");
}

/// The same open method, the same `: array`, with a `@return` the contract lane can
/// state: the dispatch gate was never the blocker. The `@return` refines against an
/// EMPTY native list here — a withheld top is not an envelope the docblock must fit
/// inside — so it arrives `Asserted` exactly as before.
#[test]
fn an_array_return_with_a_statable_docblock_answers() {
    let src = r#"<?php
class Foo {
    /** @return list<string> */
    public function all(): array { return []; }
}
function f(Foo $foo): void {
    \PHPStan\dumpType($foo->all());
}
"#;
    assert_eq!(one_type(src), "list<string> (asserted)");
}

/// `void` and `mixed` lower to the same `None` an absent hint gives, and neither is
/// an enforced top: `void` names no value and `mixed` cuts nothing.
#[test]
fn an_unrepresentable_hint_says_nothing() {
    let src = r#"<?php
class Foo {
    public function anything(): mixed { return 1; }
}
function f(Foo $foo): void {
    \PHPStan\dumpType($foo->anything());
}
"#;
    assert_eq!(one_type(src), "unknown");
}

/// `: object` and `: iterable` are withheld from the declaration path for the same
/// reason `: array` is — the twins the A16 bullet named alongside it.
#[test]
fn the_other_enforced_tops_are_withheld_too() {
    let src = r#"<?php
class Foo {
    public function rows(): iterable { return []; }
}
function f(Foo $foo): void {
    \PHPStan\dumpType($foo->rows());
}
"#;
    assert_eq!(one_type(src), "unknown");
}

// ---------------------------------------------------------------------------
// Strata: `@return` merges at `Asserted` and renders `(asserted)`.
// ---------------------------------------------------------------------------

/// An `@return` refinement within the native envelope is the docblock's claim, so
/// it renders with the `(asserted)` marker and never premises the proof layer.
#[test]
fn a_docblock_refinement_renders_asserted() {
    let src = r#"<?php
class Foo {
    /** @return non-empty-string */
    public function getName(): string { return 'x'; }
}
function f(Foo $foo): void {
    \PHPStan\dumpType($foo->getName());
}
"#;
    let ds = findings(src);
    let d = ds.iter().find(|d| d.id == DEBUG_TYPE_ID).expect("a dump");
    assert!(d.message.ends_with("(asserted)"), "expected the asserted marker, got {}", d.message);
}

/// The **native** envelope on an inexact receiver rides at its native stratum: PHP
/// enforces it at the return boundary under every descendant, so the marker is
/// absent.
#[test]
fn a_native_envelope_carries_no_asserted_marker() {
    let src = r#"<?php
class Foo {
    public function getName(): string { return $this->n; }
}
function f(Foo $foo): void {
    \PHPStan\dumpType($foo->getName());
}
"#;
    let ds = findings(src);
    let d = ds.iter().find(|d| d.id == DEBUG_TYPE_ID).expect("a dump");
    assert!(!d.message.contains("(asserted)"), "expected no marker, got {}", d.message);
}

/// An `Asserted` **receiver carrier** demotes the whole answer, however native the
/// return hint is (A13's minimum-stratum rule applied to the receiver lane): the
/// `instanceof` fact carries no stratum of its own and an `assert()` narrowing is
/// `Asserted`, so the envelope read through it is too.
#[test]
fn an_asserted_receiver_carrier_demotes_a_native_envelope() {
    let src = r#"<?php
class Foo {
    public function getName(): string { return $this->n; }
}
/** @param object $foo */
function f($foo): void {
    assert($foo instanceof Foo);
    \PHPStan\dumpType($foo->getName());
}
"#;
    let ds = findings(src);
    let d = ds.iter().find(|d| d.id == DEBUG_TYPE_ID).expect("a dump");
    assert!(d.message.ends_with("(asserted)"), "expected the asserted marker, got {}", d.message);
}

/// The declared-side dump reads the same floor (parity with `dumpType`).
#[test]
fn the_declared_side_dump_reads_the_same_floor() {
    let src = r#"<?php
class Foo {
    public function getName(): string { return $this->n; }
}
function f(Foo $foo): void {
    \PHPStan\dumpPhpDocType($foo->getName());
}
"#;
    assert_eq!(phpdoc_types(src), vec!["string".to_owned()]);
}

// ---------------------------------------------------------------------------
// Every override/variance finding is unchanged.
// ---------------------------------------------------------------------------

/// The engine that convicts a widening override is the same one the soundness
/// argument rests on; it keeps convicting, and the declaration path adds no
/// finding of its own.
#[test]
fn the_override_variance_finding_is_unchanged() {
    let src = r#"<?php
class Base {
    public function id(): int { return 1; }
}
class Child extends Base {
    public function id(): int|string { return 1; }
}
"#;
    let ds = findings(src);
    assert_eq!(ds.iter().filter(|d| d.id == OVERRIDE_RETURN_VARIANCE_ID).count(), 1);
}

// ---------------------------------------------------------------------------
// A16's hazard: the body-walking consumers keep the resolver they had.
// ---------------------------------------------------------------------------

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

/// **`descend`**: the callee's body is never walked for an inexact receiver, so a
/// value the body proves does not reach the caller — only the declared envelope
/// does. An override may run instead, and walking the resolved body would state a
/// summary about code that does not run.
#[test]
fn descend_still_sees_no_target_for_an_inexact_receiver() {
    let src = r#"<?php
class Foo {
    public function tag(): string { return 'concrete'; }
}
function f(Foo $foo): void {
    \PHPStan\dumpType($foo->tag());
}
"#;
    // The body's `'concrete'` would be the answer if the descent had resolved.
    assert_eq!(one_type(src), "string");
}

/// **`promote`**: the reverse call-site sweep records an observed argument only
/// against a target it resolved. An inexact receiver resolves to none, so the
/// sweep stays empty — the declaration path is invisible here.
#[test]
fn promote_still_sees_no_target_for_an_inexact_receiver() {
    // `$this->` in an open class: `resolve_guarded` refuses on the declaring
    // class's own non-finality, and that is exactly the receiver the declaration
    // path now answers for. The sweep must still see nothing.
    let open = "<?php\nclass Foo {\n  public function take($p) { return $p; }\n  public function go() { return $this->take(1); }\n}\n";
    assert!(
        method_sweep(&[("open.php", open)]).targets.is_empty(),
        "an overridable method on an open class resolves to no sweep target"
    );
    // The `final` twin does resolve — so the emptiness above is the guard talking,
    // not the fixture failing to reach the sweep.
    let sealed = "<?php\nfinal class Foo {\n  public function take($p) { return $p; }\n  public function go() { return $this->take(1); }\n}\n";
    assert_eq!(method_sweep(&[("sealed.php", sealed)]).targets.len(), 1);
}

/// **`asserts`**: an `@phpstan-assert` tag on an overridable method is not
/// applied — an override may assert differently. The declaration path resolves
/// the same method for its return envelope and changes nothing here.
#[test]
fn asserts_still_sees_no_target_for_an_inexact_receiver() {
    let src = r#"<?php
class Guard {
    /** @phpstan-assert string $v */
    public function must($v): void {}
}
function f(Guard $g, $v): void {
    $g->must($v);
    \PHPStan\dumpType($v);
}
"#;
    assert_eq!(one_type(src), "unknown", "the assert tag must not have narrowed $v");
    // The `final` twin proves the tag is applied when dispatch does resolve.
    let sealed = r#"<?php
final class Guard {
    /** @phpstan-assert string $v */
    public function must($v): void {}
}
function f(Guard $g, $v): void {
    $g->must($v);
    \PHPStan\dumpType($v);
}
"#;
    assert_eq!(one_type(sealed), "string (asserted)");
}

/// The proof layer stays shut to a docblock-derived floor: an `@return` arm read
/// through the declaration path premises `phpdoc.maybe-argument-mismatch`, never
/// its `type.*` sibling (ADR-0052 §5).
#[test]
fn a_docblock_return_never_premises_the_proof_layer() {
    let src = r#"<?php
class Foo {
    /** @return non-empty-string */
    public function getName(): string { return 'x'; }
}
function want(int $n): void {}
function f(Foo $foo): void {
    want($foo->getName());
}
"#;
    let ids: Vec<String> = findings(src).into_iter().map(|d| d.id.to_owned()).collect();
    assert!(!ids.iter().any(|i| i.starts_with("type.")), "no proof-layer finding, got {ids:?}");
}

// The two refusals the adversarial review added.

/// The private-shadow rule: from inside `P`, which declares a private `m`, `$c->m()`
/// on a `C extends P` calls `P::m` — a private method is not virtual — whatever `C`
/// declares under the name. The declared chain would find `C::m(): ?string` and
/// manufacture a possibly-grade mismatch on an `int` that is really there.
#[test]
fn a_private_method_in_the_enclosing_class_shadows_the_declared_chain() {
    let src = r#"<?php
class P {
    private function m(): int { return 1; }
    public function viaParam(C $c): void { takesInt($c->m()); \PHPStan\dumpType($c->m()); }
}
class C extends P {
    public function m(): ?string { return null; }
}
function takesInt(int $i): void {}
"#;
    let ids: Vec<String> = findings(src).into_iter().map(|d| d.id.to_owned()).collect();
    assert!(!ids.iter().any(|i| i.contains("maybe-argument-mismatch")), "P::m() returns int: {ids:?}");
    assert_eq!(one_type(src), "int");
}

/// `P::i()` on an instance method with no enclosing class is a PHP 8 `Error`, so
/// nothing after it runs — parity with the dispatch resolver's own refusal.
#[test]
fn a_named_static_call_of_an_instance_method_at_top_level_answers_nothing() {
    let src = r#"<?php
class P {
    public function i(): ?string { return null; }
}
function takesString(string $s): void {}
takesString(P::i());
\PHPStan\dumpType(P::i());
"#;
    let ids: Vec<String> = findings(src).into_iter().map(|d| d.id.to_owned()).collect();
    assert!(!ids.iter().any(|i| i.contains("maybe-argument-mismatch")), "a dead call premises nothing: {ids:?}");
    assert_eq!(one_type(src), "unknown");
}

/// The positive half of the stratum split: a docblock-only `@return ?string` on an
/// open receiver reaches the CONTRACT-layer possibly grade and never the proof one.
#[test]
fn a_docblock_return_reaches_the_phpdoc_possibly_grade_and_not_the_proof_one() {
    let src = r#"<?php
class Foo {
    /** @return ?string */
    public function getName() { return null; }
}
function want(string $s): void {}
function f(Foo $foo): void {
    want($foo->getName());
}
"#;
    let ids: Vec<String> = findings(src).into_iter().map(|d| d.id.to_owned()).collect();
    assert!(ids.iter().any(|i| i == "phpdoc.maybe-argument-mismatch"), "the contract-layer id fires: {ids:?}");
    assert!(!ids.iter().any(|i| i.starts_with("type.")), "no proof-layer id: {ids:?}");
}
