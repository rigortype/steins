//! The declared effect lane (ADR-0067, issue #70): a call through a receiver
//! whose declared type is a project **interface** carries that interface's effect
//! envelope into the caller's summary — as a *declared* bound, never a proven
//! effect. This is what keeps effect information alive across dependency
//! injection, where the concrete call graph is deliberately unknowable.
//!
//! The two lanes are kept apart on purpose: `effect.envelope-exceeded` and
//! `effect.liskov-widened` read the proven lane only, so no declaration can ever
//! manufacture a finding.

use steins_infer::{
    Diagnostic, EFFECT_ID, EFFECT_LISKOV_ID, EffectSummary, FactKind, NoFold, annotate_facts, check,
    effect_summary,
};
use steins_syntax::SourceTree;

fn summary(src: &str, symbol: &str) -> EffectSummary {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let classes = tree.classes().to_vec();
    effect_summary(&tree, &functions, &classes)
        .into_iter()
        .find(|s| s.symbol == symbol)
        .unwrap_or_else(|| panic!("no summary for {symbol}"))
}

/// Every `annotate` effect-margin body in a source (ADR-0020 rendering).
fn effect_margins(src: &str) -> Vec<String> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let classes = tree.classes().to_vec();
    annotate_facts(&tree, &functions, &classes, "test.php", &mut NoFold)
        .into_iter()
        .filter(|f| matches!(f.kind, FactKind::Effects { .. }))
        .map(|f| f.body())
        .collect()
}

fn effect_findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php")
        .into_iter()
        .filter(|d| d.id == EFFECT_ID || d.id == EFFECT_LISKOV_ID)
        .collect()
}

/// The controller→repository→`io.db` interface, reused by most cases below.
const REPO: &str = concat!(
    "interface Repo {\n",
    "    #[\\Steins\\Effect('io.db')]\n",
    "    public function find(int $id): string;\n",
    "}\n",
);

// ---- THE HEADLINE: an injected interface keeps its effect --------------------

#[test]
fn interface_typed_parameter_receiver_contributes_declared_label() {
    // No concrete class is resolvable — and the effect survives anyway.
    let src = format!(
        "<?php\n{REPO}function f(Repo $r): string {{ return $r->find(1); }}\n"
    );
    let s = summary(&src, "f");
    assert!(s.labels.is_empty(), "nothing is PROVEN here, got: {:?}", s.labels);
    assert_eq!(s.declared, vec!["io.db"], "the envelope's label enters the declared lane");
    assert!(s.exhaustive, "the declaration answers this call site: taint discharged");
}

#[test]
fn constructor_promoted_property_receiver_contributes_declared_label() {
    // Form (b): the DI shape as actually written — `$this->repo->find()`.
    let src = format!(
        "<?php\n{REPO}final class Controller {{\n\
             public function __construct(private readonly Repo $repo) {{}}\n\
             public function show(): string {{ return $this->repo->find(1); }}\n\
         }}\n"
    );
    let s = summary(&src, "Controller::show");
    assert!(s.labels.is_empty(), "nothing proven, got: {:?}", s.labels);
    assert_eq!(s.declared, vec!["io.db"]);
    assert!(s.exhaustive);
}

#[test]
fn plain_typed_property_receiver_contributes_declared_label() {
    let src = format!(
        "<?php\n{REPO}final class Controller {{\n\
             private Repo $repo;\n\
             public function show(): string {{ return $this->repo->find(1); }}\n\
         }}\n"
    );
    let s = summary(&src, "Controller::show");
    assert_eq!(s.declared, vec!["io.db"]);
    assert!(s.exhaustive);
}

// ---- The gates: anything less than certain falls back to today's taint -------

#[test]
fn reassigned_parameter_receiver_falls_back_to_taint() {
    // The name no longer provably holds what the signature typed.
    let src = format!(
        "<?php\n{REPO}function f(Repo $r): string {{ $r = new Other(); return $r->find(1); }}\n"
    );
    let s = summary(&src, "f");
    assert!(s.declared.is_empty(), "a written receiver imports nothing, got: {:?}", s.declared);
    assert!(!s.exhaustive, "and it taints exactly as it did before ADR-0067");
}

#[test]
fn parameter_handed_to_a_call_falls_back_to_taint() {
    // A by-ref parameter on the other side could rebind it; the gate cannot tell.
    let src = format!(
        "<?php\n{REPO}function f(Repo $r): string {{ rebind($r); return $r->find(1); }}\n"
    );
    let s = summary(&src, "f");
    assert!(s.declared.is_empty());
    assert!(!s.exhaustive);
}

#[test]
fn written_property_receiver_falls_back_to_taint() {
    let src = format!(
        "<?php\n{REPO}final class Controller {{\n\
             private Repo $repo;\n\
             public function show(): string {{ $this->repo = new Other(); return $this->repo->find(1); }}\n\
         }}\n"
    );
    let s = summary(&src, "Controller::show");
    assert!(s.declared.is_empty());
    assert!(!s.exhaustive);
}

// ---- The escape hatches a purely local write-scan would miss ---------------

#[test]
fn by_ref_closure_capture_of_the_receiver_falls_back_to_taint() {
    // The closure aliases `$r` and rebinds it, from a scope the write-scan
    // otherwise stops at. A by-ref capture is a write, whatever the body does.
    let src = format!(
        "<?php\n{REPO}function f(Repo $r): string {{\n\
             $g = function () use (&$r) {{ $r = new Wild(); }};\n\
             $g();\n\
             return $r->find(1);\n\
         }}\n"
    );
    let f = effect_findings(&src);
    assert!(f.is_empty(), "no finding either way, got: {f:#?}");
    let s = summary(&src, "f");
    assert!(s.declared.is_empty(), "the binding may be another object's, got: {:?}", s.declared);
    assert!(!s.exhaustive, "so the honest answer is the taint");
}

#[test]
fn by_value_closure_capture_of_the_receiver_keeps_the_bound() {
    // The positive control. `use ($r)` is a copy: writing it inside the closure
    // cannot touch the caller's binding, so the declaration still holds.
    let src = format!(
        "<?php\n{REPO}function f(Repo $r): string {{\n\
             $g = function () use ($r) {{ $r = new Wild(); }};\n\
             $g();\n\
             return $r->find(1);\n\
         }}\n"
    );
    let s = summary(&src, "f");
    assert_eq!(s.declared, vec!["io.db"], "a copy rebinds nothing");
    // `$g()` resolves to the body-local closure (ADR-0033), whose body is
    // effect-free — so nothing in this frame taints, and the bound is the whole
    // summary. Overcorrecting on by-value captures would have cost exactly this.
    assert!(s.exhaustive);
}

#[test]
fn a_closure_writing_the_property_falls_back_to_taint() {
    // A non-static closure declared in a method binds the *same* `$this`, so
    // this is a write to this frame's property from inside a nested scope.
    let src = format!(
        "<?php\n{REPO}final class Controller {{\n\
             private Repo $repo;\n\
             public function show(): string {{\n\
                 $g = function () {{ $this->repo = new Wild(); }};\n\
                 $g();\n\
                 return $this->repo->find(1);\n\
             }}\n\
         }}\n"
    );
    let s = summary(&src, "Controller::show");
    assert!(s.declared.is_empty(), "the property may be another object's, got: {:?}", s.declared);
    assert!(!s.exhaustive);
}

#[test]
fn a_global_statement_rebinding_the_parameter_falls_back_to_taint() {
    // `global $r;` over a parameter name is legal PHP and rebinds the name to
    // the interpreter's global — the signature's type no longer describes it.
    let src = format!(
        "<?php\n{REPO}function f(Repo $r): string {{ global $r; return $r->find(1); }}\n"
    );
    let s = summary(&src, "f");
    assert!(s.declared.is_empty(), "rebound to the global, got: {:?}", s.declared);
    assert!(!s.exhaustive);
}

#[test]
fn interface_method_without_an_envelope_still_taints() {
    // Absence of a contract is not a contract.
    let src = concat!(
        "<?php\n",
        "interface Repo { public function find(int $id): string; }\n",
        "function f(Repo $r): string { return $r->find(1); }\n",
    );
    let s = summary(src, "f");
    assert!(s.declared.is_empty(), "no envelope, nothing to import");
    assert!(!s.exhaustive, "and the unknown stays honest");
}

#[test]
fn class_typed_receiver_is_out_of_scope_and_taints() {
    // A non-final class is an abstraction carrier too, but out of scope for this
    // tracer, so it falls back to the plain taint rather than guessing.
    let src = concat!(
        "<?php\n",
        "class Repo {\n",
        "    #[\\Steins\\Effect('io.db')]\n",
        "    public function find(int $id): string { return ''; }\n",
        "}\n",
        "function f(Repo $r): string { return $r->find(1); }\n",
    );
    let s = summary(src, "f");
    assert!(s.declared.is_empty());
    assert!(!s.exhaustive);
}

#[test]
fn union_typed_receiver_taints() {
    let src = format!(
        "<?php\n{REPO}interface Other {{ public function find(int $id): string; }}\n\
         function f(Repo|Other $r): string {{ return $r->find(1); }}\n"
    );
    let s = summary(&src, "f");
    assert!(s.declared.is_empty(), "one receiver, two contracts — no single bound");
    assert!(!s.exhaustive);
}

// ---- An inherited envelope: nearest carrier wins -----------------------------

#[test]
fn envelope_inherited_from_an_extended_interface_binds() {
    let src = format!(
        "<?php\n{REPO}interface CachedRepo extends Repo {{}}\n\
         function f(CachedRepo $r): string {{ return $r->find(1); }}\n"
    );
    let s = summary(&src, "f");
    assert_eq!(s.declared, vec!["io.db"]);
    assert!(s.exhaustive);
}

// ---- The lanes never mix: no declared label may make a finding ---------------

#[test]
fn declared_label_never_triggers_envelope_exceeded() {
    // A `Pure` function whose body calls an `io.db`-declared interface method.
    // The proven lane is empty, so the envelope check has nothing to say.
    let src = format!(
        "<?php\n{REPO}#[\\Steins\\Pure]\nfunction f(Repo $r): string {{ return $r->find(1); }}\n"
    );
    let f = effect_findings(&src);
    assert!(f.is_empty(), "a declared bound is not a proven effect, got: {f:#?}");
    let s = summary(&src, "f");
    assert_eq!(s.declared, vec!["io.db"], "and the bound is still reported in the summary");
}

#[test]
fn declared_label_never_triggers_liskov_widening() {
    let src = format!(
        "<?php\n{REPO}interface Handler {{\n\
             #[\\Steins\\Pure]\n\
             public function run(): string;\n\
         }}\n\
         final class Controller implements Handler {{\n\
             public function __construct(private readonly Repo $repo) {{}}\n\
             public function run(): string {{ return $this->repo->find(1); }}\n\
         }}\n"
    );
    let f = effect_findings(&src);
    assert!(f.is_empty(), "the Liskov check reads the proven lane only, got: {f:#?}");
    let s = summary(&src, "Controller::run");
    assert_eq!(s.declared, vec!["io.db"]);
}

// ---- Propagation: a declared label travels call edges ------------------------

#[test]
fn declared_label_propagates_one_hop_to_a_caller() {
    let src = format!(
        "<?php\n{REPO}function f(Repo $r): string {{ return $r->find(1); }}\n\
         function g(Repo $r): string {{ return f($r); }}\n"
    );
    let s = summary(&src, "g");
    assert!(s.labels.is_empty());
    assert_eq!(s.declared, vec!["io.db"], "the callee's declared lane joins the caller's");
    assert!(s.exhaustive, "an exhaustive callee taints nobody");
}

// ---- Rendering normalization and the per-site taint discharge ----------------

#[test]
fn declared_label_covered_by_a_proven_one_renders_once() {
    // The body both PROVES io.fs.write (file_put_contents) and imports it as a
    // bound. The proven lane already says everything the declared one would.
    let src = concat!(
        "<?php\n",
        "interface Writer {\n",
        "    #[\\Steins\\Effect('io.fs.write')]\n",
        "    public function put(string $s): void;\n",
        "}\n",
        "function f(Writer $w): void { file_put_contents('/x', 'y'); $w->put('z'); }\n",
    );
    let s = summary(src, "f");
    assert_eq!(s.labels, vec!["io.fs.write"]);
    assert!(s.declared.is_empty(), "subsumed by the proven label, got: {:?}", s.declared);
    assert!(s.exhaustive);
}

#[test]
fn declared_label_not_covered_by_the_proven_lane_survives() {
    let src = format!(
        "<?php\n{REPO}function f(Repo $r): string {{ echo 'x'; return $r->find(1); }}\n"
    );
    let s = summary(&src, "f");
    assert_eq!(s.labels, vec!["io.output.buffer"]);
    assert_eq!(s.declared, vec!["io.db"]);
    assert!(s.exhaustive);
}

#[test]
fn the_margin_renders_a_declared_label_with_a_less_than_or_equal_prefix() {
    // An only-declared summary: `effects: {≤io.db}`. Beside a proven label the
    // two share the braces, proven first: `effects: {io.output.buffer, ≤io.db}`.
    let only = format!(
        "<?php\n{REPO}function f(Repo $r): string {{ return $r->find(1); }}\n"
    );
    assert!(
        effect_margins(&only).contains(&"effects: {≤io.db}".to_owned()),
        "got: {:?}",
        effect_margins(&only)
    );
    let mixed = format!(
        "<?php\n{REPO}function f(Repo $r): string {{ echo 'x'; return $r->find(1); }}\n"
    );
    assert!(
        effect_margins(&mixed).contains(&"effects: {io.output.buffer, ≤io.db}".to_owned()),
        "got: {:?}",
        effect_margins(&mixed)
    );
}

#[test]
fn the_taint_discharge_is_per_call_site() {
    // One covered call, one genuinely dynamic one: the bound is imported AND the
    // summary stays non-exhaustive. A declaration answers its own call site only.
    let src = format!(
        "<?php\n{REPO}function f(Repo $r, callable $cb): string {{ $cb(); return $r->find(1); }}\n"
    );
    let s = summary(&src, "f");
    assert_eq!(s.declared, vec!["io.db"]);
    assert!(!s.exhaustive, "the dynamic call still taints");
}
