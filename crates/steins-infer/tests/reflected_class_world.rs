//! Issue #269: the reflected class world — classes from loaded PHP extensions
//! resolve against the project's own PHP.
//!
//! A class an installed extension provides (`Redis`, `Random\Randomizer`,
//! `Dom\Element`) has no source declaration and no builtin-catalog row, so both
//! of Steins' static class worlds are silent about it. ADR-0049 §1: the running
//! engine is the only honest source for it — no curated stub list, no bundled
//! class inventory.
//!
//! # This slice resolves; it does not convict (owner ruling, 2026-08-09)
//!
//! A reflected declaration is an **envelope-grade** fact that restores coverage,
//! but premises no absence-family finding: `call.undefined-method`,
//! `property.undefined`, `class-const.undefined`, `class.undefined` and the
//! arity family all require a source-declared, uniquely-resolved chain, and a
//! reflected class never enters the project index they enumerate over. The
//! silence tests below are the structural proof: member access on an extension
//! class is exactly as silent with a live engine as without one.
//!
//! Three halves, in the arrangement `constant_undefined.rs` set:
//!
//! * **mocked** — a counting [`FoldEngine`] for the memoization and env-identity
//!   discipline, pinned deterministically;
//! * **live** — a real `php` answering a real `ReflectionClass` (skipped with a
//!   marker when `php` is absent, or when the fixture's extension is not loaded);
//! * **sound subset** — `--no-php` and the `Folder` default, which must be exactly
//!   today's behavior.

use steins_infer::{
    Diagnostic, EngineFolder, Folder, FoldEngine, NoFold, SidecarFolder, check_with,
};
use steins_sidecar::{
    ClassReflection, ConstantDefined, EnvInfo, FoldArg, FoldResult, PregCompile,
    ReflectedClassKind, Reflection,
};
use steins_syntax::SourceTree;

/// The always-available fixture class: ext-random is built into every PHP since
/// 8.2, and `Random\Randomizer` carries no row in `steins-catalog`'s hierarchy
/// table (verified: no namespaced `random\*` key at all).
const EXTENSION_CLASS: &str = "Random\\Randomizer";

/// The skip-if-absent optional-extension fixture. Nothing in CI loads ext-redis.
const OPTIONAL_CLASS: &str = "Redis";

fn run(src: &str, folder: &mut dyn Folder) -> Vec<Diagnostic> {
    check_with(&SourceTree::parse(src), &[], "test.php", folder)
}

/// A real folder over a real `php`, or a skip marker. The probe asks about a name
/// no test below uses, so the per-run memo cannot answer a later question from it.
fn live_or_skip(test: &str) -> Option<SidecarFolder> {
    let mut folder = SidecarFolder::enabled();
    if folder.reflected_class("Steins\\Probe269").is_none() {
        eprintln!("SKIP {test}: no PHP engine answered `reflect_class` — is `php` on PATH?");
        return None;
    }
    Some(folder)
}

// Live: the project's own PHP answers.

/// The acceptance criterion, end to end: a class no source or catalog row
/// answers is offered to the engine, whose answer carries the whole class
/// world — methods with signatures, constants, properties, hierarchy edges —
/// plus the origin it came from.
#[test]
fn an_extension_class_resolves_off_the_projects_own_php() {
    let Some(mut folder) = live_or_skip("an_extension_class_resolves_off_the_projects_own_php")
    else {
        return;
    };
    let answer = folder.reflected_class(EXTENSION_CLASS).expect("a live engine answers");
    let Some(d) = answer.declaration else {
        eprintln!("SKIP an_extension_class_resolves_off_the_projects_own_php: ext-random absent");
        return;
    };

    // The catalog genuinely has no row for this name — the premise of the slice.
    assert!(
        steins_catalog::builtin_class_supers("random\\randomizer").is_none(),
        "the fixture class must be one the builtin catalog has no row for"
    );

    assert_eq!(d.name, EXTENSION_CLASS);
    assert_eq!(d.kind, ReflectedClassKind::Class);
    // Origin: what `doctor` prints beside the name.
    assert!(d.internal, "{d:?}");
    assert_eq!(d.extension.as_deref(), Some("random"), "{d:?}");
    let get_int = d.methods.iter().find(|m| m.name == "getInt").expect("getInt: {d:?}");
    assert_eq!(get_int.params_required, 2, "{get_int:?}");
    assert_eq!(get_int.return_type.as_deref(), Some("int"), "{get_int:?}");
    assert!(d.properties.iter().any(|p| p.name == "engine"), "{d:?}");
}

/// Constants and hierarchy edges, on a class-like resident on every supported minor.
#[test]
fn a_resident_class_carries_constants_and_hierarchy_edges() {
    let Some(mut folder) = live_or_skip("a_resident_class_carries_constants_and_hierarchy_edges")
    else {
        return;
    };
    let d = folder
        .reflected_class("ArrayObject")
        .expect("a live engine answers")
        .declaration
        .expect("ArrayObject is resident on every supported PHP");
    assert!(d.constants.iter().any(|c| c.name == "ARRAY_AS_PROPS"), "{d:?}");
    assert!(d.interfaces.iter().any(|i| i == "Countable"), "{d:?}");
}

/// The skip-if-absent optional-extension case: where ext-redis is loaded the
/// class resolves through the identical path.
#[test]
fn an_optional_extension_class_resolves_where_the_extension_is_loaded() {
    let Some(mut folder) =
        live_or_skip("an_optional_extension_class_resolves_where_the_extension_is_loaded")
    else {
        return;
    };
    let answer = folder.reflected_class(OPTIONAL_CLASS).expect("a live engine answers");
    match answer.declaration {
        Some(d) => {
            assert_eq!(d.extension.as_deref(), Some("redis"), "{d:?}");
            assert!(!d.methods.is_empty(), "a loaded extension class has methods: {d:?}");
        }
        None => eprintln!(
            "SKIP (partial) an_optional_extension_class_resolves…: ext-redis is not loaded — \
             the not-found leg is asserted instead"
        ),
    }
}

/// A class the engine does not have is a **structured not-found**, distinct
/// from a decline.
#[test]
fn a_class_no_extension_provides_is_a_structured_not_found() {
    let Some(mut folder) = live_or_skip("a_class_no_extension_provides_is_a_structured_not_found")
    else {
        return;
    };
    let answer = folder.reflected_class("Steins\\NoSuchClass269").expect("an answer");
    assert!(!answer.exists(), "{answer:?}");
}

/// Case-insensitivity: PHP class names are, so the query is too.
#[test]
fn the_class_query_is_case_insensitive() {
    let Some(mut folder) = live_or_skip("the_class_query_is_case_insensitive") else { return };
    let lower = folder.reflected_class("arrayobject").expect("an answer");
    let mixed = folder.reflected_class("ArrayObject").expect("an answer");
    assert_eq!(lower, mixed, "one class, one answer");
}

// The ruling: resolution buys silence and coverage, never a finding.

/// A method call, a property access and a class-constant fetch on a class the
/// engine HAS: **byte-identical diagnostics** with a live engine and without one.
///
/// Zero-movement proof: the checks that could convict (`call.undefined-method`,
/// `property.undefined`, `class-const.undefined`, the arity family) never see
/// this class, since a reflected class does not enter the project index they
/// enumerate over — so the run with an engine and without produce the same
/// findings, down to the message.
#[test]
fn member_access_on_a_resolved_extension_class_is_as_silent_as_it_was() {
    let src = "<?php
function f(\\Random\\Randomizer $r): void {
    $r->getInt(1, 6);
    $r->nosuchmethod();
    echo $r->engine;
    echo $r->nosuchproperty;
    echo \\Random\\Randomizer::NO_SUCH_CONST;
}
";
    let Some(mut live) =
        live_or_skip("member_access_on_a_resolved_extension_class_is_as_silent_as_it_was")
    else {
        return;
    };
    if !live.reflected_class(EXTENSION_CLASS).is_some_and(|r| r.exists()) {
        eprintln!("SKIP member_access_on_a_resolved_extension_class…: ext-random absent");
        return;
    }
    let with_engine = run(src, &mut live);
    let without_engine = run(src, &mut SidecarFolder::new(true));
    assert_eq!(
        rendered(&with_engine),
        rendered(&without_engine),
        "a reflected class world may not move a single finding (issue #269 ruling)"
    );
    assert!(
        member_family(&with_engine).is_empty(),
        "a resolved class convicts no member: {with_engine:#?}"
    );
}

/// The same, for a class the engine does **not** have (ext-redis unloaded in
/// CI): the pre-existing `class.undefined` leg is untouched.
#[test]
fn an_unresolved_extension_class_gains_no_member_finding() {
    let src = "<?php
function f(\\Redis $c): void {
    $c->get('k');
    $c->nosuchmethod();
    echo $c->nosuchproperty;
    echo \\Redis::NO_SUCH_CONST;
}
";
    let Some(mut live) = live_or_skip("an_unresolved_extension_class_gains_no_member_finding")
    else {
        return;
    };
    let found = run(src, &mut live);
    assert!(member_family(&found).is_empty(), "no member conviction: {found:#?}");
}

/// The absence-family ids a reflected declaration must never license.
fn member_family(ds: &[Diagnostic]) -> Vec<&Diagnostic> {
    ds.iter()
        .filter(|d| {
            d.id.starts_with("call.")
                || d.id.starts_with("property.")
                || d.id.starts_with("class-const.")
                || d.id.starts_with("member.")
                || d.id.starts_with("override.")
        })
        .collect()
}

/// `id@line: message` for each diagnostic, sorted — the comparable rendering the
/// equality assert above needs.
fn rendered(ds: &[Diagnostic]) -> Vec<String> {
    let mut out: Vec<String> =
        ds.iter().map(|d| format!("{}@{}: {}", d.id, d.line, d.message)).collect();
    out.sort();
    out
}

// The sound subset: byte-identical to today.

/// `--no-php` never asks, and therefore never answers.
#[test]
fn the_no_php_folder_declines() {
    let mut folder = SidecarFolder::new(true);
    assert_eq!(folder.reflected_class(EXTENSION_CLASS), None);
    assert_eq!(folder.reflected_class("ArrayObject"), None);
}

/// The `Folder` default is the sound subset: a folder that implements nothing
/// declines, keeping every non-sidecar caller exactly as it was.
#[test]
fn the_folder_default_declines() {
    assert_eq!(NoFold.reflected_class(EXTENSION_CLASS), None);
}

// Mocked: the memo and its env-identity key.

/// A transport that counts class queries and lets the test change the engine
/// underneath the run.
struct Counting {
    /// `reflect_class` calls served.
    class_calls: u32,
    /// The extension list `env()` reports; changing it is a changed runtime.
    extensions: Vec<String>,
    /// The transport generation: bumping it models a child dying and being
    /// replaced (ADR-0024).
    restarts: u32,
}

impl Counting {
    fn new() -> Self {
        Counting { class_calls: 0, extensions: vec!["Core".to_owned()], restarts: 0 }
    }
}

impl FoldEngine for Counting {
    fn env(&mut self) -> Option<EnvInfo> {
        Some(EnvInfo {
            php_version: "8.5.9".to_owned(),
            extensions: self.extensions.clone(),
            sapi: "cli".to_owned(),
            int_size: Some(8),
        })
    }
    fn reflect(&mut self, _target: &str) -> Option<Reflection> {
        None
    }
    fn reflect_class(&mut self, target: &str) -> Option<ClassReflection> {
        self.class_calls += 1;
        Some(ClassReflection { target: target.to_owned(), declaration: None })
    }
    fn fold(&mut self, _name: &str, _args: &[FoldArg]) -> FoldResult {
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

/// One class, one request — however many times and however it is spelled.
#[test]
fn a_repeated_class_query_costs_one_request() {
    let mut folder = EngineFolder::with_engine(Counting::new());
    for spelling in ["Redis", "redis", "REDIS", "Redis"] {
        let _ = folder.reflected_class(spelling);
    }
    assert_eq!(folder.engine_mut().class_calls, 1, "the memo answered the other three");
}

/// The memo is keyed by the `env()` identity: a runtime that gains an extension is
/// a different class world, and every answer taken from the old one is discarded
/// rather than mixed with the new one's.
///
/// The runtime can only change where the transport was replaced (ADR-0024's
/// respawn) — a live child's extension set does not move — so the identity is
/// re-taken on a bumped generation, and it is the *comparison* that clears the memo.
#[test]
fn a_changed_runtime_invalidates_the_class_memo() {
    let mut folder = EngineFolder::with_engine(Counting::new());
    let _ = folder.reflected_class("Redis");
    assert_eq!(folder.engine_mut().class_calls, 1);
    let _ = folder.reflected_class("Redis");
    assert_eq!(folder.engine_mut().class_calls, 1, "same runtime, same answer");

    // A replacement child that is the SAME runtime keeps the memo: re-asking every
    // class after every respawn is exactly the traffic the memo exists to avoid.
    folder.engine_mut().restarts = 1;
    let _ = folder.reflected_class("Redis");
    assert_eq!(folder.engine_mut().class_calls, 1, "an identical runtime keeps its answers");

    // A replacement child with a different extension set is a different class
    // world: what `Redis` even is may have changed, so the old answer is dropped.
    folder.engine_mut().restarts = 2;
    folder.engine_mut().extensions.push("redis".to_owned());
    let _ = folder.reflected_class("Redis");
    assert_eq!(folder.engine_mut().class_calls, 2, "a changed runtime is re-asked");
}
