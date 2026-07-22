//! Acceptance tests for whole-project (cross-file) resolution (ADR-0001/0009).
//!
//! Each test builds a real multi-file salsa project and runs [`check_project`]
//! (or [`annotate_project`]). The zero-FP guards (ambiguity, builtin shadowing,
//! duplicate FQN) resolve to *silence*, never to a false positive.

use steins_db::{Project, SourceFile, SteinsDatabase};
use steins_infer::{Diagnostic, EFFECT_ID, FactKind, ID, NoFold, annotate_project, check_project};

/// Build a project from `(path, source)` pairs and return every finding.
fn findings(files: &[(&str, &str)]) -> Vec<Diagnostic> {
    let db = SteinsDatabase::default();
    let inputs: Vec<SourceFile> = files
        .iter()
        .map(|(p, t)| SourceFile::new(&db, (*p).to_owned(), (*t).to_owned()))
        .collect();
    let project = Project::new(&db, inputs);
    check_project(&db, project, &mut NoFold)
}

fn n(files: &[(&str, &str)]) -> usize {
    findings(files).len()
}

fn only(files: &[(&str, &str)]) -> Diagnostic {
    let f = findings(files);
    assert_eq!(f.len(), 1, "expected exactly one finding, got: {f:#?}");
    f.into_iter().next().unwrap()
}

// ---- 1. Cross-file function call -----------------------------------------

#[test]
fn cross_file_function_call_flagged() {
    // render() is defined in lib.php; the bad literal call is in main.php. The
    // finding exists only because both files are one project.
    let d = only(&[
        ("main.php", "<?php\nrender(\"abc\");\n"),
        ("lib.php", "<?php\nfunction render(int $w): int { return $w; }\n"),
    ]);
    assert_eq!(d.id, ID);
    assert_eq!(d.path, "main.php");
    assert_eq!(d.line, 2);
    assert!(d.message.contains("to render() cannot become int $w"), "{}", d.message);
}

// ---- 2. Namespaced resolution: NS\f, and the global fallback --------------

#[test]
fn namespaced_function_resolves_within_namespace() {
    // App\greet is defined and called unqualified inside namespace App.
    let d = only(&[
        ("lib.php", "<?php\nnamespace App;\nfunction greet(int $n): int { return $n; }\n"),
        ("main.php", "<?php\nnamespace App;\ngreet(\"abc\");\n"),
    ]);
    assert!(d.message.contains("to greet() cannot become int"), "{}", d.message);
}

#[test]
fn unqualified_call_falls_back_to_global_user_function() {
    // No App\gwidth exists, so PHP falls back to the global gwidth (not a
    // builtin) — resolved and flagged.
    let d = only(&[
        ("lib.php", "<?php\nfunction gwidth(int $w): int { return $w; }\n"),
        ("main.php", "<?php\nnamespace App;\ngwidth(\"abc\");\n"),
    ]);
    assert!(d.message.contains("to gwidth() cannot become int"), "{}", d.message);
}

// ---- 3. Builtin-shadowing ambiguity → silent -----------------------------

#[test]
fn builtin_shadowing_is_silent() {
    // A userland global `trim` shadows the catalogued builtin `trim`: PHP would
    // fatal on the redefinition, and we cannot know which runs, so the call is
    // ambiguous → silent (even though a user trim(int) would flag "abc").
    assert_eq!(
        n(&[
            ("poly.php", "<?php\nfunction trim(int $n): int { return $n; }\n"),
            ("main.php", "<?php\ntrim(\"abc\");\n"),
        ]),
        0,
        "builtin-shadowing user function → ambiguous → silent"
    );
}

// ---- 4. Duplicate FQN → silent -------------------------------------------

#[test]
fn duplicate_fqn_is_silent() {
    // Two files define the same global `dup` (polyfill pattern) → ambiguous FQN
    // → never resolved.
    assert_eq!(
        n(&[
            ("a.php", "<?php\nfunction dup(int $w): int { return $w; }\n"),
            ("b.php", "<?php\nfunction dup(int $w): int { return $w; }\n"),
            ("main.php", "<?php\ndup(\"abc\");\n"),
        ]),
        0,
        "duplicate FQN → silent"
    );
}

// ---- 5. Cross-file class extends chain ctor check ------------------------

#[test]
fn cross_file_extends_chain_constructor_flagged() {
    // Sub (sub.php) extends Base (base.php); `new Sub("abc")` runs the inherited
    // Base::__construct(int), resolved across two files.
    let d = only(&[
        ("base.php", "<?php\nclass Base { public function __construct(int $w) {} }\n"),
        ("sub.php", "<?php\nclass Sub extends Base {}\n"),
        ("main.php", "<?php\nnew Sub(\"abc\");\n"),
    ]);
    assert!(d.message.contains("to Base::__construct()"), "{}", d.message);
}

// ---- 6. Cross-file method, exact receiver --------------------------------

#[test]
fn cross_file_exact_receiver_method_flagged() {
    let d = only(&[
        ("foo.php", "<?php\nclass Foo { public function m(int $w): void {} }\n"),
        ("main.php", "<?php\n$x = new Foo();\n$x->m(\"abc\");\n"),
    ]);
    assert!(d.message.contains("to Foo::m()"), "{}", d.message);
    assert_eq!(d.path, "main.php");
}

// ---- 7. `use`-import class resolution ------------------------------------

#[test]
fn use_import_class_resolution_flagged() {
    // `use App\Models\User;` binds User; `new User("abc")` resolves to the
    // imported FQN and checks its cross-file constructor.
    let d = only(&[
        (
            "user.php",
            "<?php\nnamespace App\\Models;\nclass User { public function __construct(int $id) {} }\n",
        ),
        ("main.php", "<?php\nnamespace App;\nuse App\\Models\\User;\nnew User(\"abc\");\n"),
    ]);
    assert!(d.message.contains("to User::__construct()"), "{}", d.message);
}

#[test]
fn unqualified_class_without_import_does_not_leak_across_namespaces() {
    // ZERO-FP: `new User("abc")` in namespace App with NO import resolves to
    // App\User, which does not exist — so it is silent, never mis-resolved to
    // App\Models\User.
    assert_eq!(
        n(&[
            (
                "user.php",
                "<?php\nnamespace App\\Models;\nclass User { public function __construct(int $id) {} }\n",
            ),
            ("main.php", "<?php\nnamespace App;\nnew User(\"abc\");\n"),
        ]),
        0,
        "unqualified class resolves to current ns only (no import) → App\\User absent → silent"
    );
}

// ---- 8. Cross-file effects with via-provenance naming the other file -----

#[test]
fn cross_file_effect_via_provenance_names_the_other_file() {
    // pure_fn (main.php) calls sideeffect (lib.php) which writes the filesystem.
    // The transitive envelope violation names the file the effect arises in.
    let f: Vec<_> = findings(&[
        ("lib.php", "<?php\nfunction sideeffect(): void { file_put_contents(\"/x\", \"y\"); }\n"),
        ("main.php", "<?php\n#[\\Steins\\Pure] function pure_fn(): void { sideeffect(); }\n"),
    ])
    .into_iter()
    .filter(|d| d.id == EFFECT_ID)
    .collect();
    assert_eq!(f.len(), 1, "one transitive effect finding, got: {f:#?}");
    assert_eq!(
        f[0].message,
        "sideeffect() has effect io.fs.write (via file_put_contents at lib.php line 2), but pure_fn() is declared #[\\Steins\\Pure]"
    );
}

// ---- 9. Cross-file const-fn ----------------------------------------------

#[test]
fn cross_file_const_fn_propagates() {
    // answer() (lib.php) is a constant function returning "abc"; width(answer())
    // in main.php resolves it cross-file, then flags "abc" into int $w.
    let d = only(&[
        ("lib.php", "<?php\nfunction answer(): string { return \"abc\"; }\n"),
        ("main.php", "<?php\nfunction width(int $w): int { return $w; }\nwidth(answer());\n"),
    ]);
    assert!(d.message.contains("to width()"), "{}", d.message);
    assert!(d.message.contains("from answer(), defined at line"), "provenance: {}", d.message);
}

// ---- 10. Binding descent provenance names the caller's file --------------

#[test]
fn cross_file_binding_descent_names_caller_file() {
    // main.php calls outer("abc") (in a.php); outer forwards to inner (in b.php)
    // where "abc" is a proven int mismatch. Provenance names the caller file.
    let d = only(&[
        ("a.php", "<?php\nfunction outer(string $s): void { inner($s); }\n"),
        ("b.php", "<?php\nfunction inner(int $w): void {}\n"),
        ("main.php", "<?php\nouter(\"abc\");\n"),
    ]);
    assert!(d.message.contains("to inner()"), "{}", d.message);
    assert!(d.message.contains("from $s"), "immediate var: {}", d.message);
    assert!(
        d.message.contains("bound at outer(\"abc\") call at main.php line 2"),
        "cross-file provenance names caller file: {}",
        d.message
    );
}

// ---- 11. annotate --project surfaces a cross-file fact --------------------

#[test]
fn annotate_project_shows_cross_file_finding() {
    let db = SteinsDatabase::default();
    let lib = SourceFile::new(
        &db,
        "lib.php".to_owned(),
        "<?php\nfunction render(int $w): int { return $w; }\n".to_owned(),
    );
    let main = SourceFile::new(&db, "main.php".to_owned(), "<?php\nrender(\"abc\");\n".to_owned());
    let project = Project::new(&db, vec![lib, main]);

    let facts = annotate_project(&db, project, main, &mut NoFold);
    assert!(
        facts.iter().any(|f| matches!(&f.kind, FactKind::Finding { id } if *id == ID) && f.line == 2),
        "cross-file finding fact on the render(\"abc\") line, got: {facts:#?}"
    );
}
