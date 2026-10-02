//! Issue #928 / #930: a guard discharges what it guards **wherever it is written**.
//!
//! Every guard family the engine knows (`defined`, `class_exists`, `extension_loaded`,
//! `function_exists`, `method_exists`, `is_callable([$o, 'm'])`, `property_exists`) is
//! written in every position a guard can take — an `if`, an early return, a ternary in
//! `return`/`echo`/an argument/an assignment/a statement, an `&&` conjunct, an `||`
//! disjunct of a negation, a `switch (true)` case — around a reference to a symbol or
//! member that does not exist on the analysing PHP. The guard is false in every one, so
//! the reference is never evaluated and PHP runs each function cleanly (witnessed
//! against `php` 8.5): the analysis must report nothing in any of them.
//!
//! A new guard function added to the vocabulary adds one row to [`FAMILIES`] and gets
//! every position for free, which is the regression this file exists for. The controls
//! below the table pin the other half: a reference outside any guard, a guard on a
//! symbol that exists, and the opposite arm of a decided guard all keep reporting.

use steins_infer::{
    CALL_UNDEFINED_FUNCTION_ID, CALL_UNDEFINED_METHOD_ID, CLASS_UNDEFINED_ID, CONSTANT_UNDEFINED_ID,
    Diagnostic, EngineFolder, FoldEngine, Folder, NoFold, PHPDOC_UNDEFINED_METHOD_ID,
    PROPERTY_MAYBE_UNDEFINED_ID, PROPERTY_UNDEFINED_ID, check_with,
};
use steins_sidecar::{
    ClassReflection, ConstantDefined, EnvInfo, FoldArg, FoldResult, PregCompile, Reflection,
};
use steins_syntax::{ArgValue, SourceTree};

/// A boot-surface mock: the family is available, nothing is resident (every symbol the
/// fixtures name is absent), and `extensions` is what the engine reports loaded, or
/// `None` for an unanswerable `env()`.
struct Boot {
    extensions: Option<Vec<&'static str>>,
}

impl Folder for Boot {
    fn fold(&mut self, _: &str, _: &[ArgValue], _strict: bool) -> Option<ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        true
    }
    fn boot_surface_class_like(&mut self, _: &str) -> Option<bool> {
        Some(false)
    }
    fn boot_surface_function(&mut self, name: &str) -> Option<bool> {
        Some(name == "dl")
    }
    fn boot_surface_constant(&mut self, name: &str) -> Option<bool> {
        Some(name == "PHP_VERSION_ID")
    }
    fn boot_surface_extension(&mut self, name: &str) -> Option<bool> {
        let loaded = self.extensions.as_ref()?;
        Some(loaded.iter().any(|e| e.eq_ignore_ascii_case(name)))
    }
    fn boot_surface_label(&mut self) -> Option<String> {
        Some("PHP 8.5.8 (32 extensions)".to_owned())
    }
}

const IDS: &[&str] = &[
    CALL_UNDEFINED_FUNCTION_ID,
    CALL_UNDEFINED_METHOD_ID,
    CLASS_UNDEFINED_ID,
    CONSTANT_UNDEFINED_ID,
    PHPDOC_UNDEFINED_METHOD_ID,
    PROPERTY_MAYBE_UNDEFINED_ID,
    PROPERTY_UNDEFINED_ID,
];

fn run_with(src: &str, folder: &mut Boot) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "test.php", folder)
        .into_iter()
        .filter(|d| IDS.contains(&d.id))
        .collect()
}

fn run(src: &str) -> Vec<Diagnostic> {
    run_with(src, &mut Boot { extensions: Some(vec!["json", "standard"]) })
}

fn render(d: &[Diagnostic]) -> String {
    d.iter().map(|d| format!("{}:{} {}", d.line, d.column, d.id)).collect::<Vec<_>>().join("\n")
}

/// One guard family: the guard call, and the references it guards (each is evaluated
/// only on the guard's true path).
struct Family {
    name: &'static str,
    /// The guard call, a boolean expression.
    guard: &'static str,
    /// The guarded reference, an expression.
    reference: &'static str,
    /// Extra parameters the reference needs (`N $n`), without a trailing comma.
    params: &'static str,
}

const FAMILIES: &[Family] = &[
    Family { name: "defined", guard: "defined('UNDEF_C')", reference: "UNDEF_C", params: "" },
    Family {
        name: "defined-class-constant",
        guard: "defined('Nope\\\\C')",
        reference: "\\Nope\\C",
        params: "",
    },
    Family {
        name: "class_exists-new",
        guard: "class_exists('NopeC')",
        reference: "new NopeC()",
        params: "",
    },
    Family {
        name: "class_exists-static",
        guard: "class_exists('NopeC')",
        reference: "NopeC::X",
        params: "",
    },
    Family {
        name: "extension_loaded",
        guard: "extension_loaded('redis')",
        reference: "new Redis()",
        params: "",
    },
    Family {
        name: "function_exists",
        guard: "function_exists('nope_fn')",
        reference: "nope_fn()",
        params: "",
    },
    Family {
        name: "method_exists",
        guard: "method_exists($n, 'go')",
        reference: "$n->go()",
        params: "N $n",
    },
    Family {
        name: "is_callable",
        guard: "is_callable([$n, 'go'])",
        reference: "$n->go()",
        params: "N $n",
    },
    Family {
        name: "property_exists",
        guard: "property_exists($n, 'p')",
        reference: "$n->p",
        params: "N $n",
    },
];

/// A position: how a function body spells "the reference runs only if the guard holds".
/// `{G}` is the guard call and `{R}` the reference.
const POSITIONS: &[(&str, &str)] = &[
    ("if", "if ({G}) { {R}; }"),
    ("if-and-conjunct", "if ({G} && {R}) {}"),
    ("if-negated-early-return", "if (!{G}) { return; } {R};"),
    ("if-else-negated", "if (!{G}) { echo 1; } else { {R}; }"),
    ("if-and-later-conjunct", "if ({G} && true && {R}) {}"),
    ("if-not-or-disjunct", "if (!{G} || {R}) {}"),
    ("return-ternary", "return {G} ? {R} : null;"),
    ("return-negated-ternary", "return !{G} ? null : {R};"),
    ("echo-ternary", "echo {G} ? {R} : '';"),
    ("argument-ternary", "take({G} ? {R} : null);"),
    ("assign-ternary", "$v = {G} ? {R} : null;"),
    ("statement-ternary", "{G} ? {R} : null;"),
    ("short-circuit-statement", "{G} && {R};"),
    ("negated-or-statement", "!{G} || {R};"),
    ("assign-and", "$v = {G} && {R};"),
    ("assign-and-conjunct-first", "$v = ({G} && true) && {R};"),
    ("return-and", "return {G} && {R};"),
    ("echo-and", "echo {G} && {R};"),
    ("nested-ternary", "return {G} ? ({G} ? {R} : 1) : 2;"),
    ("concat-ternary", "echo 'x' . ({G} ? {R} : '');"),
    ("switch-true", "switch (true) { case {G}: {R}; break; default: }"),
    ("switch-true-last-case", "switch (true) { case {G}: {R}; }"),
    ("switch-true-return", "switch (true) { case {G}: return {R}; default: return null; }"),
    ("switch-true-later-case", "switch (true) { case false: break; case {G}: {R}; break; }"),
];

fn build(family: &Family, body: &str, index: usize) -> String {
    let body = body.replace("{G}", family.guard).replace("{R}", family.reference);
    format!("function g{index}({}): mixed {{ {body} return null; }}\n", family.params)
}

const PRELUDE: &str = "<?php\nclass N { public function real(): int { return 1; } }\n\
function take(mixed $x): void {}\n";

#[test]
fn every_guard_discharges_in_every_position() {
    let mut failures = Vec::new();
    for family in FAMILIES {
        for (i, (position, template)) in POSITIONS.iter().enumerate() {
            let src = format!("{PRELUDE}{}", build(family, template, i));
            let found = run(&src);
            if !found.is_empty() {
                failures.push(format!(
                    "{} in `{position}` still reports:\n{}\n{src}",
                    family.name,
                    render(&found),
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n---\n"));
}

/// The same table, all positions of one family in a single file: the verdict of one
/// function must not leak into the next, and the regions of each stay its own.
#[test]
fn every_family_discharges_with_all_positions_in_one_file() {
    for family in FAMILIES {
        let mut src = String::from(PRELUDE);
        for (i, (_, template)) in POSITIONS.iter().enumerate() {
            src.push_str(&build(family, template, i));
        }
        let found = run(&src);
        assert!(found.is_empty(), "{}:\n{}", family.name, render(&found));
    }
}

// ---------------------------------------------------------------------------
// Controls: what must keep reporting.
// ---------------------------------------------------------------------------

/// `reference` outside any guard reports, for every family whose reference is a proof
/// on its own (the member and function families need a receiver or a sidecar answer).
#[test]
fn an_unguarded_reference_still_reports() {
    for (name, reference, id) in [
        ("class", "new NopeC()", CLASS_UNDEFINED_ID),
        ("constant", "UNDEF_C", CONSTANT_UNDEFINED_ID),
        ("extension class", "new Redis()", CLASS_UNDEFINED_ID),
    ] {
        let src = format!("<?php\nfunction g() {{ {reference}; }}\n");
        let found = run(&src);
        assert_eq!(found.len(), 1, "{name}: {}", render(&found));
        assert_eq!(found[0].id, id, "{name}");
    }
    let src = format!("{PRELUDE}function g(N $n): mixed {{ return $n->go(); }}\n");
    let found = run(&src);
    assert_eq!(found.len(), 1, "{}", render(&found));
    assert_eq!(found[0].id, CALL_UNDEFINED_METHOD_ID);
}

/// The guard's *other* side is live: a decided guard kills one arm, never both.
#[test]
fn the_unguarded_arm_of_a_decided_guard_still_reports() {
    for (name, body) in [
        ("else arm", "return class_exists('NopeC') ? 1 : new NopeC();"),
        ("else arm of negation", "return !class_exists('NopeC') ? new NopeC() : 1;"),
        ("or disjunct", "class_exists('NopeC') || new NopeC();"),
        ("negated and conjunct", "!class_exists('NopeC') && new NopeC();"),
        ("echo else arm", "echo defined('UNDEF_C') ? 1 : UNDEF_C;"),
        ("assign else arm", "$v = extension_loaded('redis') ? 1 : new Redis();"),
        (
            "switch default",
            "switch (true) { case class_exists('NopeC'): break; default: new NopeC(); }",
        ),
        ("switch negated case", "switch (true) { case !class_exists('NopeC'): new NopeC(); }"),
        ("earlier statement", "$a = new NopeC(); if (class_exists('NopeC')) {}"),
        ("after the guard", "if (class_exists('NopeC')) {} new NopeC();"),
        ("negated guard body", "if (!extension_loaded('redis')) { new Redis(); }"),
    ] {
        let src = format!("<?php\nfunction g() {{ {body} }}\n");
        let found = run(&src);
        assert_eq!(found.len(), 1, "{name}: {}\n{src}", render(&found));
    }
}

/// A guard on a symbol that **exists** decides the other way: its body is live, so a
/// real mistake inside it still reports.
#[test]
fn a_guard_on_an_existing_symbol_keeps_its_body_live() {
    for (name, guard) in [
        ("class_exists", "class_exists('N')"),
        ("method_exists", "method_exists('N', 'real')"),
        ("function_exists", "function_exists('present_fn')"),
    ] {
        let src = format!(
            "<?php\nclass N {{ public function real(): int {{ return 1; }} }}\n\
             function present_fn(): int {{ return 1; }}\n\
             function g() {{ if ({guard}) {{ (new N)->nope(); }} }}\n"
        );
        let found = run(&src);
        assert_eq!(found.len(), 1, "{name}: {}", render(&found));
        assert_eq!(found[0].id, CALL_UNDEFINED_METHOD_ID, "{name}");
    }
}

/// A member guard on a **different** member decides nothing about this one: `N` has
/// `real` (so that guard is live, not dead) and has neither `other` nor `go`. The
/// receiver is a native parameter type, so the claim is the proof layer's.
#[test]
fn a_member_guard_on_another_member_does_not_vouch() {
    for (name, body, id) in [
        ("method", "if (method_exists($n, 'real')) { $n->go(); }", CALL_UNDEFINED_METHOD_ID),
        ("callable", "if (is_callable([$n, 'real'])) { $n->go(); }", CALL_UNDEFINED_METHOD_ID),
        ("property", "if (property_exists($n, 'q')) { return $n->p; }", PROPERTY_UNDEFINED_ID),
        ("after the guard", "if (method_exists($n, 'go')) {} $n->go();", CALL_UNDEFINED_METHOD_ID),
    ] {
        let src = format!("{PRELUDE}function g(N $n): mixed {{ {body} return null; }}\n");
        let found = run(&src);
        assert_eq!(found.len(), 1, "{name}: {}", render(&found));
        assert_eq!(found[0].id, id, "{name}");
    }
}

// ---------------------------------------------------------------------------
// extension_loaded.
// ---------------------------------------------------------------------------

/// The branch the fold keeps and the one it kills, probed with a method absence, which
/// needs no existence dam: it fires wherever the walk reaches.
fn undef_in(branch: &str, extensions: Option<Vec<&'static str>>) -> usize {
    let src = format!(
        "<?php\nclass C {{}}\nfunction g() {{ if (extension_loaded('json')) {{ {branch} }} \
         else {{ {branch} }} }}\n"
    );
    run_with(&src, &mut Boot { extensions }).len()
}

#[test]
fn extension_loaded_folds_from_the_sidecar_list() {
    // Loaded: the else branch is dead, the then branch live — one finding.
    assert_eq!(undef_in("(new C)->undef();", Some(vec!["Json"])), 1);
    // Not loaded: the then branch is dead, the else branch live — one finding.
    assert_eq!(undef_in("(new C)->undef();", Some(vec!["standard"])), 1);
    // Unanswerable `env()`: undecided, both arms live.
    assert_eq!(undef_in("(new C)->undef();", None), 2);
}

#[test]
fn extension_loaded_matches_names_case_insensitively() {
    let src = "<?php\nclass C {}\nfunction g() { if (extension_loaded('JSON')) {} \
               else { (new C)->undef(); } }\n";
    assert_eq!(run(src).len(), 0, "loaded under another case: the else arm is dead");
    let src = "<?php\nclass C {}\nfunction g() { if (!extension_loaded('JSON')) \
               { (new C)->undef(); } }\n";
    assert_eq!(run(src).len(), 0, "loaded under another case: the negated body is dead");
}

#[test]
fn a_dl_call_anywhere_leaves_extension_loaded_undecided() {
    let src = "<?php\nclass C {}\nfunction boot() { dl('redis.so'); }\n\
               function g() { if (extension_loaded('redis')) { (new C)->undef(); } }\n";
    assert_eq!(run(src).len(), 1, "a run-time extension load: neither polarity is decidable");
    // A qualified call is another function and loads nothing.
    let src = "<?php\nnamespace Other { function dl(string $x): void {} }\n\
               namespace App { class C {} function boot() { \\Other\\dl('x'); }\n\
               function g() { if (extension_loaded('redis')) { (new C)->undef(); } } }\n";
    assert_eq!(run(src).len(), 0, "Other\\dl() is not the builtin: the guard is decided");
}

#[test]
fn extension_loaded_with_a_computed_name_is_undecided() {
    let src = "<?php\nclass C {}\nfunction g(string $e) { if (extension_loaded($e)) \
               { (new C)->undef(); } }\n";
    assert_eq!(run(src).len(), 1);
}

// ---------------------------------------------------------------------------
// Member vouches through declared receivers (#930).
// ---------------------------------------------------------------------------

/// `method_exists($v, 'm')` over a declared union folds to false only when **every** arm
/// provably lacks `m`, so the guarded body is dead exactly where the declared-receiver
/// lane would have reported inside it. An arm that has `m` leaves the guard undecided and
/// the body live.
#[test]
fn a_method_guard_over_a_declared_union_folds_only_when_every_arm_lacks_the_method() {
    let src = "<?php\nclass A {}\nclass B {}\n\
               function g(A|B $v): void { if (method_exists($v, 'go')) { $v->go(); } }\n\
               function h(A|B $v): void { if (is_callable([$v, 'go'])) { $v->go(); } }\n";
    let found = run(src);
    assert!(found.is_empty(), "{}", render(&found));
    let src = "<?php\nclass A {}\nclass B {}\n\
               function g(A|B $v): void { $v->go(); }\n";
    assert_eq!(run(src).len(), 1, "unguarded, the union receiver still reports");
}

/// `property_exists` cannot fold (it is true for a dynamic property too): it vouches
/// `$v->p` for this binding, and for a union that is the binding, not either class.
#[test]
fn a_property_guard_vouches_the_binding() {
    let src = "<?php\nclass A {}\nclass B {}\n\
               function h(A|B $v): void { if (property_exists($v, 'p')) { echo $v->p; } }\n";
    let found = run(src);
    assert!(found.is_empty(), "{}", render(&found));
}

/// The leaks a class-keyed vouch had, each now reporting: a guard on one binding says
/// nothing of another receiver of the class, and nothing of the same name after a rebind.
#[test]
fn a_member_guard_vouches_neither_another_receiver_nor_a_rebound_one() {
    let n = "class N { public function real(): int { return 1; } }\n";
    for (name, src, id) in [
        (
            "union: B has no go, guard only decides A",
            "final class A { public function go(): int { return 1; } }\nfinal class B {}\n\
             function u(A|B $x, B $y): mixed {\n\
             if (method_exists($x, 'go')) { return $y->go(); } return null; }\n",
            CALL_UNDEFINED_METHOD_ID,
        ),
        (
            "method guard, then a rebind to a class without it",
            "final class A { public function go(): int { return 1; } }\nfinal class B {}\n\
             function w(A|B $x): mixed {\n\
             if (method_exists($x, 'go')) { $x = new B(); return $x->go(); } return null; }\n",
            CALL_UNDEFINED_METHOD_ID,
        ),
        (
            "property guard on one binding, read through another",
            &format!(
                "{n}function f(N $a, N $b): mixed {{\n\
                 if (property_exists($a, 'p')) {{ return $b->p; }} return null; }}\n"
            ),
            PROPERTY_UNDEFINED_ID,
        ),
        (
            "property guard, then a rebind",
            &format!(
                "{n}function p3(N $n): mixed {{\n\
                 if (property_exists($n, 'p')) {{ $n = new N(); return $n->p; }} return null; }}\n"
            ),
            PROPERTY_UNDEFINED_ID,
        ),
    ] {
        let src = format!("<?php\n{src}");
        let found = run(&src);
        assert_eq!(found.len(), 1, "{name}: {}\n{src}", render(&found));
        assert_eq!(found[0].id, id, "{name}");
    }
    // A guard on `$n` that is provably false kills the body, whichever receiver it reads:
    // dead, not vouched — `N` has no `go`, so the guard cannot hold.
    let src = format!(
        "<?php\n{n}function m5(N $n, N $o): void {{\n\
         if (method_exists($n, 'go')) {{ $o->go(); }} }}\n"
    );
    let found = run(&src);
    assert!(found.is_empty(), "{}", render(&found));
}

#[test]
fn a_member_vouch_survives_the_early_return_only_on_its_own_path() {
    let src = "<?php\nclass N {}\nfunction g(N $n, bool $f): void {\n\
               if ($f) { if (!method_exists($n, 'go')) { return; } }\n\
               $n->go();\n}\n";
    assert_eq!(run(src).len(), 1, "the other path reaches the call unguarded");
}

// ---------------------------------------------------------------------------
// Conditional declarations, `$autoload`, `dl()`, shared switch defaults.
// ---------------------------------------------------------------------------

/// `class_exists` of a declaration the program may not have run is undecided: a class
/// declared under a condition (a polyfill, an environment switch) and a project class
/// asked about with `$autoload` other than `true` (declared is not loaded).
#[test]
fn class_exists_of_a_declaration_that_may_not_have_run_is_undecided() {
    for (name, src) in [
        (
            "autoload false, ternary",
            "class ProjLater {}\nfunction r1(): mixed {\n\
             return class_exists('ProjLater', false) ? 1 : new NopeP1(); }\n",
        ),
        (
            "autoload false, if",
            "class ProjLater {}\nfunction r2(): mixed {\n\
             if (class_exists('ProjLater', false)) { return 1; } return new NopeP2(); }\n",
        ),
        (
            "conditional polyfill, ternary",
            "if (PHP_VERSION_ID < 50000) { class PolyOld {} }\n\
             function r3(): mixed { return class_exists('PolyOld') ? 1 : new NopeP3(); }\n",
        ),
        (
            "conditional polyfill, if",
            "if (PHP_VERSION_ID < 50000) { class PolyOld {} }\n\
             function r4(): mixed {\n\
             if (class_exists('PolyOld')) { return 1; } return new NopeP4(); }\n",
        ),
        (
            "environment switch, or",
            "if (getenv('X') === 'yes') { class PolyEnv {} }\n\
             function r7(): void { class_exists('PolyEnv') || new NopeP7(); }\n",
        ),
    ] {
        let found = run(&format!("<?php\n{src}"));
        assert_eq!(found.len(), 1, "{name}: {}", render(&found));
        assert_eq!(found[0].id, CLASS_UNDEFINED_ID, "{name}");
    }
}

/// The same project class under `$autoload` absent or literally `true` is present, so
/// the `else` that says it is missing is dead.
#[test]
fn class_exists_of_an_unconditional_declaration_with_autoload_is_decided() {
    for call in ["class_exists('Declared')", "class_exists('Declared', true)"] {
        let src = format!(
            "<?php\nclass Declared {{}}\n\
             function r(): mixed {{ return {call} ? 1 : new Nope(); }}\n"
        );
        let found = run(&src);
        assert!(found.is_empty(), "{call}: {}", render(&found));
    }
}

/// The trade-off #978 tracks, pinned as it stands: `function_exists` of a conditional
/// polyfill still answers `Yes` with the dam clear, which kills the `else`.
#[test]
fn function_exists_of_a_conditional_function_still_answers_yes() {
    let polyfill = "if (PHP_VERSION_ID < 50000) { function poly_old(): int { return 1; } }\n";
    let guarded = format!(
        "<?php\n{polyfill}function r(): mixed {{\n\
         return function_exists('poly_old') ? 1 : nope_p6(); }}\n"
    );
    assert!(run(&guarded).is_empty(), "{}", render(&run(&guarded)));
    let bare = "<?php\nfunction r(): mixed { return nope_p6(); }\n";
    assert_eq!(run(bare).len(), 1, "the control: the call reports unguarded");
}

/// `dl()` leaves `extension_loaded()` undecided and closes no other valve: an unrelated
/// undefined function, class and constant still report.
#[test]
fn dl_closes_no_name_or_constant_valve() {
    let src = "<?php\nfunction load(): void { @dl('x.so'); }\n\
               function q(): void { nope_fn_dl3(); new NopeClsDl3(); echo NOPE_CONST_DL3; }\n";
    let found = run(src);
    let ids: Vec<&str> = found.iter().map(|d| d.id).collect();
    assert_eq!(
        ids,
        [CALL_UNDEFINED_FUNCTION_ID, CLASS_UNDEFINED_ID, CONSTANT_UNDEFINED_ID],
        "{}",
        render(&found)
    );
}

/// A default that shares its body with case labels is the `else` only as the last body.
/// Before the last, a later case could match the shared labels first, so the construct
/// stays unstructured and the shared body is reported as live.
#[test]
fn a_default_sharing_a_body_before_a_later_case_stays_live() {
    for (name, src) in [
        (
            "switch (true), case then default",
            "function s1(): void { switch (true) {\n\
             case strlen('a') === 1: default: new NopeSw(); break;\n\
             case !defined('NOPE_SW_CONST'): echo 1; break; } }\n",
        ),
        (
            "switch (true), default then case",
            "function s2(): void { switch (true) {\n\
             default: case strlen('a') === 1: new NopeSw2(); break;\n\
             case !defined('NOPE_SW2_CONST'): echo 1; break; } }\n",
        ),
        (
            "by value, case then default",
            "function s3(): void { $x = null; switch ($x) {\n\
             case null: default: new NopeSw3(); break;\n\
             case false: echo 1; break; } }\n",
        ),
        (
            "by value, default then case",
            "function s4(): void { $x = null; switch ($x) {\n\
             default: case null: new NopeSw4(); break;\n\
             case false: echo 1; break; } }\n",
        ),
    ] {
        let found = run(&format!("<?php\n{src}"));
        assert_eq!(found.len(), 1, "{name}: {}", render(&found));
        assert_eq!(found[0].id, CLASS_UNDEFINED_ID, "{name}");
    }
}

// ---------------------------------------------------------------------------
// The folder's answer: the sidecar `env()` extension list.
// ---------------------------------------------------------------------------

/// A transport whose `env()` lists `extensions` (or declines), counting the asks.
struct ExtensionEngine {
    extensions: Option<Vec<String>>,
    env_calls: u32,
    restarts: u32,
}

impl FoldEngine for ExtensionEngine {
    fn env(&mut self) -> Option<EnvInfo> {
        self.env_calls += 1;
        Some(EnvInfo {
            php_version: "8.5.9".to_owned(),
            extensions: self.extensions.clone()?,
            sapi: "cli".to_owned(),
            int_size: Some(8),
        })
    }
    fn reflect(&mut self, _target: &str) -> Option<Reflection> {
        None
    }
    fn reflect_class(&mut self, _target: &str) -> Option<ClassReflection> {
        None
    }
    fn fold(&mut self, _name: &str, _args: &[FoldArg], _strict: bool) -> FoldResult {
        FoldResult::widen("stub")
    }
    fn preg_compile(&mut self, _pattern: &str) -> Option<PregCompile> {
        None
    }
    fn constant_defined(&mut self, _name: &str) -> Option<ConstantDefined> {
        None
    }
    fn restarts(&self) -> u32 {
        self.restarts
    }
}

#[test]
fn the_engine_folder_answers_extension_loaded_from_env_once() {
    let engine = ExtensionEngine {
        extensions: Some(vec!["Core".to_owned(), "Zend OPcache".to_owned()]),
        env_calls: 0,
        restarts: 0,
    };
    let mut folder = EngineFolder::with_engine(engine);
    assert_eq!(folder.boot_surface_extension("core"), Some(true));
    assert_eq!(folder.boot_surface_extension("zend opcache"), Some(true));
    assert_eq!(folder.boot_surface_extension("redis"), Some(false));
    assert_eq!(folder.engine_mut().env_calls, 1, "the list is one whole-run answer");
}

#[test]
fn an_unanswerable_env_leaves_extension_loaded_undecided_and_is_asked_again_after_a_respawn() {
    let engine = ExtensionEngine { extensions: None, env_calls: 0, restarts: 0 };
    let mut folder = EngineFolder::with_engine(engine);
    assert_eq!(folder.boot_surface_extension("json"), None);
    assert_eq!(folder.boot_surface_extension("json"), None);
    assert_eq!(folder.engine_mut().env_calls, 1, "a decline is memoized within a child");
    // The replacement child answers: the standing decline must not outlive the child.
    folder.engine_mut().restarts = 1;
    folder.engine_mut().extensions = Some(vec!["json".to_owned()]);
    assert_eq!(folder.boot_surface_extension("json"), Some(true));
}

#[test]
fn the_folder_default_declines_extension_loaded() {
    assert_eq!(NoFold.boot_surface_extension("json"), None);
}
