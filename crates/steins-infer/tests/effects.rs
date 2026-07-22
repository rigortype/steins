//! Acceptance tests for the effect-envelope check (`effect.envelope-exceeded`,
//! ADR-0005): a function declared `#[\Steins\Pure]` whose inferred effects
//! exceed the empty envelope. Proven violations only — unknown effects stay
//! silent (the deferred "cannot-verify" maybe-diagnostic).

use steins_infer::{Diagnostic, EFFECT_ID, UNKNOWN_LABEL_ID, check};
use steins_syntax::SourceTree;

/// Parse + check inline PHP, returning only the effect-envelope findings.
fn effects(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php").into_iter().filter(|d| d.id == EFFECT_ID).collect()
}

/// Parse + check inline PHP, returning only the unknown-label findings.
fn unknown_labels(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php").into_iter().filter(|d| d.id == UNKNOWN_LABEL_ID).collect()
}

fn one(src: &str) -> Diagnostic {
    let f = effects(src);
    assert_eq!(f.len(), 1, "expected exactly one effect finding, got: {f:#?}");
    f.into_iter().next().unwrap()
}

// ---- Direct builtin effect at a Pure call site ---------------------------

#[test]
fn pure_calling_rand_is_flagged_with_exact_message() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction withRng(): int { return rand(); }\n";
    let d = one(src);
    assert_eq!(d.id, EFFECT_ID);
    assert_eq!(
        d.message,
        "rand() has effect nondet.random, but withRng() is declared #[\\Steins\\Pure]"
    );
    // Points at the `rand` call (line 3).
    assert_eq!(d.line, 3);
}

#[test]
fn pure_builtin_and_arithmetic_are_silent() {
    // strtolower is catalogued-pure; arithmetic has no effect.
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(string $s): string { $x = 1 + 2; return strtolower($s); }\n";
    assert_eq!(effects(src).len(), 0, "pure builtin + arithmetic → silent");
}

#[test]
fn pure_calling_uncatalogued_builtin_is_silent() {
    // Unknown builtin widens to unknown-effect → deferred maybe → silent.
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(): void { some_unknown_fn(); }\n";
    assert_eq!(effects(src).len(), 0, "uncatalogued builtin → silent (deferred)");
}

// ---- echo (CST-scan case: nested in control flow) ------------------------

#[test]
fn echo_inside_if_inside_pure_is_flagged() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(bool $c): void { if ($c) { echo \"hi\"; } }\n";
    let d = one(src);
    assert_eq!(d.message, "echo has effect output, but f() is declared #[\\Steins\\Pure]");
}

// ---- exit (ADR-0019 rule 4) ----------------------------------------------

#[test]
fn exit_inside_pure_is_flagged() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(): void { exit(1); }\n";
    let d = one(src);
    assert_eq!(d.message, "exit has effect exit, but f() is declared #[\\Steins\\Pure]");
}

// ---- throw is permitted by Pure (ADR-0006) -------------------------------

#[test]
fn throw_inside_pure_is_silent() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(): void { throw new \\RuntimeException(\"x\"); }\n";
    assert_eq!(effects(src).len(), 0, "Pure permits throw");
}

// ---- Transitive: pure → helper → file_put_contents, with via-provenance --

#[test]
fn transitive_effect_reports_via_origin() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(): void { helper(); }\nfunction helper(): void { file_put_contents(\"/tmp/x\", \"y\"); }\n";
    let d = one(src);
    assert_eq!(
        d.message,
        "helper() has effect io.fs.write (via file_put_contents at line 4), but f() is declared #[\\Steins\\Pure]"
    );
    // Reported at the outer `helper()` call site (line 3).
    assert_eq!(d.line, 3);
}

#[test]
fn transitive_through_two_hops() {
    // f → g → h → file_put_contents; the via still names the ultimate origin.
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(): void { g(); }\nfunction g(): void { h(); }\nfunction h(): void { file_put_contents(\"/tmp/x\", \"y\"); }\n";
    let d = one(src);
    assert!(
        d.message.contains("g() has effect io.fs.write (via file_put_contents at line 5)"),
        "got: {}",
        d.message
    );
}

#[test]
fn transitive_pure_helper_is_silent() {
    // helper only calls a pure builtin → no effect propagates.
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(string $s): void { helper($s); }\nfunction helper(string $s): string { return strtolower($s); }\n";
    assert_eq!(effects(src).len(), 0, "pure helper → silent");
}

// ---- Recursion must not hang ---------------------------------------------

#[test]
fn mutual_recursion_terminates() {
    // a ⇄ b with no real effects: converges, reports nothing, does not loop.
    let src = "<?php\n#[\\Steins\\Pure]\nfunction a(): void { b(); }\nfunction b(): void { a(); }\n";
    assert_eq!(effects(src).len(), 0);
    // And with a real effect reachable through the cycle it is still found.
    let effectful = "<?php\n#[\\Steins\\Pure]\nfunction a(): void { b(); }\nfunction b(): void { a(); rand(); }\n";
    let f = effects(effectful);
    assert!(
        f.iter().any(|d| d.message.contains("b() has effect nondet.random")),
        "effect through the cycle is found: {f:#?}"
    );
}

// ---- Attribute recognition guards (end-to-end through the check) ---------

#[test]
fn bare_pure_without_use_is_not_checked() {
    // JetBrains collision guard: #[Pure] without `use Steins\Pure` → not an
    // envelope → the rand() call is not a violation.
    let src = "<?php\n#[Pure]\nfunction f(): int { return rand(); }\n";
    assert_eq!(effects(src).len(), 0, "#[Pure] without use is not the Steins envelope");
}

#[test]
fn bare_pure_with_use_is_checked() {
    let src = "<?php\nuse Steins\\Pure;\n#[Pure]\nfunction f(): int { return rand(); }\n";
    let d = one(src);
    assert!(d.message.contains("rand() has effect nondet.random"), "got: {}", d.message);
}

#[test]
fn jetbrains_qualified_pure_is_not_checked() {
    let src = "<?php\n#[JetBrains\\PhpStorm\\Pure]\nfunction f(): int { return rand(); }\n";
    assert_eq!(effects(src).len(), 0, "#[JetBrains\\PhpStorm\\Pure] is not the Steins envelope");
}

// ---- Coexistence with type.argument-mismatch -----------------------------

#[test]
fn effect_and_type_findings_coexist() {
    // A Pure function that both calls rand() (effect) and is called with a bad
    // literal (type mismatch) yields both diagnostics.
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(int $w): int { return rand() + $w; }\nf(\"abc\");\n";
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let all = check(&tree, &functions, "test.php");
    assert!(
        all.iter().any(|d| d.id == "type.argument-mismatch"),
        "type finding present: {all:#?}"
    );
    assert!(all.iter().any(|d| d.id == EFFECT_ID), "effect finding present: {all:#?}");
}

// ---- Non-Pure functions are never effect-checked -------------------------

#[test]
fn unannotated_function_with_effects_is_silent() {
    let src = "<?php\nfunction f(): int { echo \"hi\"; return rand(); }\n";
    assert_eq!(effects(src).len(), 0, "no envelope → no effect check");
}

// ==========================================================================
// ADR-0018: hierarchical `#[\Steins\Effect(...)]` envelopes — subsumption.
// ==========================================================================

#[test]
fn effect_io_subsumes_io_fs_write() {
    // #[Effect('io')] admits io.fs.write (coarse declaration, fine catalog).
    let src = "<?php\n#[\\Steins\\Effect('io')]\nfunction f(): void { file_put_contents(\"/tmp/x\", \"y\"); }\n";
    assert_eq!(effects(src).len(), 0, "io subsumes io.fs.write → silent");
}

#[test]
fn effect_io_does_not_admit_nondet_random() {
    // rand() is nondet.random, outside the io subtree → a violation.
    let src = "<?php\n#[\\Steins\\Effect('io')]\nfunction f(): int { return rand(); }\n";
    let d = one(src);
    assert_eq!(
        d.message,
        "rand() has effect nondet.random, but f() is declared #[\\Steins\\Effect('io')] — nondet.random exceeds the envelope"
    );
    assert_eq!(d.line, 3);
}

#[test]
fn narrow_read_envelope_flags_a_write() {
    // #[Effect('io.fs.read')] does not admit io.fs.write.
    let src = "<?php\n#[\\Steins\\Effect('io.fs.read')]\nfunction f(): void { file_put_contents(\"/tmp/x\", \"y\"); }\n";
    let d = one(src);
    assert_eq!(
        d.message,
        "file_put_contents() has effect io.fs.write, but f() is declared #[\\Steins\\Effect('io.fs.read')] — io.fs.write exceeds the envelope"
    );
}

#[test]
fn narrow_read_envelope_admits_a_read() {
    let src = "<?php\n#[\\Steins\\Effect('io.fs.read')]\nfunction f(): void { file_get_contents(\"/tmp/x\"); }\n";
    assert_eq!(effects(src).len(), 0, "io.fs.read admits io.fs.read → silent");
}

#[test]
fn nondet_envelope_covers_random_and_time() {
    // Both nondet.random (rand) and nondet.time (time) are under nondet.
    let src = "<?php\n#[\\Steins\\Effect('nondet')]\nfunction f(): int { return rand() + time(); }\n";
    assert_eq!(effects(src).len(), 0, "nondet subsumes both nondet.random and nondet.time");
}

#[test]
fn multi_label_envelope_admits_each_subtree() {
    // #[Effect('io', 'nondet.time')] admits io.fs.write and nondet.time, but not
    // nondet.random.
    let ok = "<?php\n#[\\Steins\\Effect('io', 'nondet.time')]\nfunction f(): void { file_put_contents(\"/x\", \"y\"); time(); }\n";
    assert_eq!(effects(ok).len(), 0, "both effects subsumed → silent");

    let bad = "<?php\n#[\\Steins\\Effect('io', 'nondet.time')]\nfunction f(): int { return rand(); }\n";
    let d = one(bad);
    assert_eq!(
        d.message,
        "rand() has effect nondet.random, but f() is declared #[\\Steins\\Effect('io', 'nondet.time')] — nondet.random exceeds the envelope"
    );
}

#[test]
fn effect_exit_admits_exit_but_pure_forbids_it() {
    // ADR-0019: #[Effect('exit')] permits exit; Pure still forbids it.
    let permitted = "<?php\n#[\\Steins\\Effect('exit')]\nfunction f(): void { exit(1); }\n";
    assert_eq!(effects(permitted).len(), 0, "Effect('exit') admits exit → silent");

    let forbidden = "<?php\n#[\\Steins\\Pure]\nfunction f(): void { exit(1); }\n";
    let d = one(forbidden);
    assert_eq!(d.message, "exit has effect exit, but f() is declared #[\\Steins\\Pure]");
}

#[test]
fn effect_output_admits_echo() {
    let src = "<?php\n#[\\Steins\\Effect('output')]\nfunction f(): void { echo \"hi\"; }\n";
    assert_eq!(effects(src).len(), 0, "Effect('output') admits echo → silent");
}

// ---- Non-literal args → unrecognized: no envelope AND no unknown-label -----

#[test]
fn non_literal_effect_args_impose_no_checking() {
    // A class-constant argument → the attribute is unrecognized, so the function
    // is NOT effect-checked and produces NO unknown-label diagnostic either.
    let src = "<?php\n#[\\Steins\\Effect(Effects::IO)]\nfunction f(): int { return rand(); }\n";
    assert_eq!(effects(src).len(), 0, "unrecognized attribute → no envelope check");
    assert_eq!(unknown_labels(src).len(), 0, "unrecognized attribute → no unknown-label");
}

// ---- Transitive through a same-file helper --------------------------------

#[test]
fn transitive_effect_exceeds_declared_envelope() {
    // #[Effect('nondet')] loadCfg → helper → file_put_contents (io.fs.write).
    let src = "<?php\n#[\\Steins\\Effect('nondet')]\nfunction loadCfg(): void { helper(); }\nfunction helper(): void { file_put_contents(\"/tmp/x\", \"y\"); }\n";
    let d = one(src);
    assert_eq!(
        d.message,
        "helper() has effect io.fs.write (via file_put_contents at line 4), but loadCfg() is declared #[\\Steins\\Effect('nondet')] — io.fs.write exceeds the envelope"
    );
    assert_eq!(d.line, 3, "reported at the outer helper() call site");
}

#[test]
fn transitive_effect_within_envelope_is_silent() {
    // #[Effect('io')] f → helper → file_put_contents: io.fs.write is subsumed.
    let src = "<?php\n#[\\Steins\\Effect('io')]\nfunction f(): void { helper(); }\nfunction helper(): void { file_put_contents(\"/tmp/x\", \"y\"); }\n";
    assert_eq!(effects(src).len(), 0, "transitive io.fs.write under io → silent");
}

// ==========================================================================
// ADR-0018: unknown-label registry diagnostic.
// ==========================================================================

#[test]
fn typo_label_reports_unknown_with_suggestion() {
    let src = "<?php\n#[\\Steins\\Effect('io.netw')]\nfunction f(): void {}\n";
    let u = unknown_labels(src);
    assert_eq!(u.len(), 1, "one unknown-label finding: {u:#?}");
    assert_eq!(u[0].id, UNKNOWN_LABEL_ID);
    assert_eq!(
        u[0].message,
        "unknown effect label 'io.netw' in #[\\Steins\\Effect] on f() — did you mean 'io.net'?"
    );
    // Points at the attribute (line 2).
    assert_eq!(u[0].line, 2);
}

#[test]
fn private_label_is_unknown_for_now() {
    // email.send is a semantic/plugin label the registry does not yet know.
    let src = "<?php\n#[\\Steins\\Effect('email.send')]\nfunction f(): void {}\n";
    let u = unknown_labels(src);
    assert_eq!(u.len(), 1, "email.send is unknown until plugins can register it");
    assert!(u[0].message.contains("unknown effect label 'email.send'"), "got: {}", u[0].message);
}

#[test]
fn registry_roots_produce_no_unknown_label() {
    for label in [
        "output", "io", "io.fs", "io.fs.read", "io.fs.write", "io.net", "io.net.http", "io.db",
        "io.process", "global.read", "global.write", "nondet", "nondet.random", "nondet.time",
        "exit", "mutate",
    ] {
        let src = format!("<?php\n#[\\Steins\\Effect('{label}')]\nfunction f(): void {{}}\n");
        assert_eq!(unknown_labels(&src).len(), 0, "{label} is a known registry root");
    }
}

#[test]
fn pure_never_produces_unknown_label() {
    // Pure has an empty label set → no label can be unknown.
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(): void {}\n";
    assert_eq!(unknown_labels(src).len(), 0);
}

// ==========================================================================
// Methods: an Effect envelope exceeded via a private `$this->` helper.
// ==========================================================================

#[test]
fn method_effect_envelope_exceeded_via_this_helper() {
    let src = "<?php\nfinal class Svc {\n  #[\\Steins\\Effect('io')]\n  public function run(): void { $this->helper(); }\n  private function helper(): void { rand(); }\n}\n";
    let d = one(src);
    assert_eq!(
        d.message,
        "Svc::helper() has effect nondet.random (via rand at line 5), but Svc::run() is declared #[\\Steins\\Effect('io')] — nondet.random exceeds the envelope"
    );
    // Reported at the `$this->helper()` call site (line 4).
    assert_eq!(d.line, 4);
}

#[test]
fn method_effect_envelope_admits_subsumed_helper_effect() {
    // Same shape but the helper's effect (io.fs.write) is under the io envelope.
    let src = "<?php\nfinal class Svc {\n  #[\\Steins\\Effect('io')]\n  public function run(): void { $this->helper(); }\n  private function helper(): void { file_put_contents(\"/x\", \"y\"); }\n}\n";
    assert_eq!(effects(src).len(), 0, "io.fs.write under io → silent");
}
