//! Stage D acceptance tests (ADR-0033 point 5): Liskov substitutability as a
//! standing rule for envelope carriers — `effect.liskov-widened` for effect
//! envelopes, and `throw.liskov-widened` extended to interface implementations.
//! Implementations may be purer / throw narrower, never the reverse; only the
//! PROVEN part of a tainted effect set judges.

use steins_infer::{check, Diagnostic, EFFECT_LISKOV_ID, THROW_LISKOV_ID};
use steins_syntax::SourceTree;

fn findings(src: &str, id: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php").into_iter().filter(|d| d.id == id).collect()
}

fn effect_liskov(src: &str) -> Vec<Diagnostic> {
    findings(src, EFFECT_LISKOV_ID)
}
fn throw_liskov(src: &str) -> Vec<Diagnostic> {
    findings(src, THROW_LISKOV_ID)
}

// ---- Effect Liskov against an interface envelope ---------------------------

#[test]
fn impure_impl_of_pure_interface_fires() {
    // Interface declares the method #[\Steins\Pure]; the implementation echoes.
    let src = "<?php\ninterface Clock {\n    #[\\Steins\\Pure]\n    public function now(): int;\n}\nclass EchoClock implements Clock {\n    public function now(): int { echo \"tick\"; return 1; }\n}\n";
    let ds = effect_liskov(src);
    assert_eq!(ds.len(), 1, "got: {ds:#?}");
    assert!(ds[0].message.contains("output"), "{}", ds[0].message);
    assert!(ds[0].message.contains("Clock::now()"), "names the abstraction: {}", ds[0].message);
    assert!(ds[0].message.contains("#[\\Steins\\Pure]"), "{}", ds[0].message);
    // Reported at the implementation site.
    assert_eq!(ds[0].line, 7);
}

#[test]
fn purer_impl_of_pure_interface_is_silent() {
    // A pure implementation of a Pure interface method is always legal.
    let src = "<?php\ninterface Clock {\n    #[\\Steins\\Pure]\n    public function now(): int;\n}\nclass FrozenClock implements Clock {\n    public function now(): int { return 42; }\n}\n";
    assert_eq!(effect_liskov(src).len(), 0, "purer impl is legal");
}

#[test]
fn impl_within_effect_envelope_is_silent() {
    // Interface allows `io`; the impl's file read (io.fs.read) is subsumed.
    let src = "<?php\ninterface Store {\n    #[\\Steins\\Effect('io')]\n    public function load(string $p): string;\n}\nclass FileStore implements Store {\n    public function load(string $p): string { return file_get_contents($p); }\n}\n";
    assert_eq!(effect_liskov(src).len(), 0, "io.fs.read is within the io envelope");
}

#[test]
fn impl_exceeding_effect_envelope_fires() {
    // Interface allows only `nondet`; the impl writes a file (io.fs.write) — exceeds.
    let src = "<?php\ninterface Gen {\n    #[\\Steins\\Effect('nondet')]\n    public function make(): void;\n}\nclass FileGen implements Gen {\n    public function make(): void { file_put_contents(\"/x\", \"y\"); }\n}\n";
    let ds = effect_liskov(src);
    assert_eq!(ds.len(), 1, "got: {ds:#?}");
    assert!(ds[0].message.contains("io.fs.write"), "{}", ds[0].message);
}

// ---- Effect Liskov against a parent-class envelope -------------------------

#[test]
fn impure_override_of_pure_parent_fires() {
    let src = "<?php\nclass Base {\n    #[\\Steins\\Pure]\n    public function go(): int { return 0; }\n}\nclass Sub extends Base {\n    public function go(): int { echo \"x\"; return 1; }\n}\n";
    let ds = effect_liskov(src);
    assert_eq!(ds.len(), 1, "got: {ds:#?}");
    assert!(ds[0].message.contains("Base::go()"), "{}", ds[0].message);
}

// ---- Proven-only: tainted impl judges only its proven subset ---------------

#[test]
fn tainted_impl_reports_only_proven_part() {
    // The impl both echoes (proven output) AND calls an unknown function (taint).
    // The proven `output` still fires; the unknown remainder stays silent.
    let src = "<?php\ninterface I {\n    #[\\Steins\\Pure]\n    public function m(): void;\n}\nclass C implements I {\n    public function m(): void { echo \"x\"; unknown_ext_fn(); }\n}\n";
    let ds = effect_liskov(src);
    assert_eq!(ds.len(), 1, "proven output fires despite the taint: {ds:#?}");
    assert!(ds[0].message.contains("output"), "{}", ds[0].message);
}

// ---- Throw Liskov extended to interface implementations --------------------

#[test]
fn interface_throws_widened_implementation_fires() {
    // Interface method declares @throws \RuntimeException; the impl declares a
    // wider (unrelated checked) @throws \JsonException.
    let src = "<?php\ninterface Repo {\n    /** @throws \\RuntimeException */\n    public function find(): void;\n}\nclass DbRepo implements Repo {\n    /** @throws \\JsonException */\n    public function find(): void { throw new \\JsonException(); }\n}\n";
    let ds = throw_liskov(src);
    assert_eq!(ds.len(), 1, "got: {ds:#?}");
    assert!(ds[0].message.contains("JsonException"), "{}", ds[0].message);
    assert!(ds[0].message.contains("Repo::find()"), "names the interface: {}", ds[0].message);
}

#[test]
fn interface_throws_narrower_implementation_is_silent() {
    // The impl declares a SUBCLASS of the interface's declared throw — narrower, OK.
    let src = "<?php\ninterface Repo {\n    /** @throws \\RuntimeException */\n    public function find(): void;\n}\nclass DbRepo implements Repo {\n    /** @throws \\OutOfBoundsException */\n    public function find(): void { throw new \\OutOfBoundsException(); }\n}\n";
    assert_eq!(throw_liskov(src).len(), 0, "OutOfBoundsException <: RuntimeException — narrower");
}
