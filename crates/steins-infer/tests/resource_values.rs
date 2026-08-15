//! ADR-0056 §8 — the engine-inexpressible type: `resource`.
//!
//! `resource` is the one PHP type PHP cannot write down: `fopen` declares no
//! return type because the language has no such declaration to make, so the
//! reflected envelope anchoring every other builtin return fact (ADR-0056 §1) is
//! structurally unavailable — §8 replaces its authority with a three-condition
//! gate whose middle condition is a tripwire.
//!
//! What is worth pinning is not "does `fopen` produce a resource" but each place
//! the design could quietly become unsound:
//!
//! * the **tripwire** (§8.2): an engine that declares a return type has migrated
//!   the function to an object and disowned the row, and the row must vanish;
//! * the **`false` arm** (§8.4): `resource|false` is not a proven resource, and
//!   the only thing that turns it into one is the ordinary `=== false` guard;
//! * the **lane-reading predicate** (§8.6): one arm, `Resource`, `Verified` —
//!   each clause blocking a different way of being wrong;
//! * the **mode independence** (§8.5): unlike every other argument mismatch, this
//!   one does not depend on `declare(strict_types=1)`, because a resource has no
//!   coercion path into anything.

use std::collections::HashMap;

use steins_infer::{Diagnostic, EngineFolder, FoldEngine, check_with};
use steins_sidecar::{ClassReflection, ConstantDefined, EnvInfo, FoldArg, FoldResult, PregCompile, Reflection};
use steins_syntax::SourceTree;

// Mock engine: only what `reflect` says about a return type matters (§8.2's tripwire).

/// An engine on PHP 8.5 (catalog pin, so §8.2's minor gate passes) that knows every
/// name asked and declares the return types it was given; a name absent from
/// `declares` reflects as **typeless** — the shape a genuine resource producer has.
struct Engine {
    declares: HashMap<String, String>,
    /// Names the engine does NOT have (an unloaded extension).
    absent: Vec<String>,
    /// The PHP version `env()` reports; `8.5.x` is the pin, else it closes the §8.2 gate.
    version: String,
}

impl Engine {
    fn typeless() -> Self {
        Engine { declares: HashMap::new(), absent: Vec::new(), version: "8.5.9".to_owned() }
    }
    /// The migrated shape: declaring a class here is the engine disowning the `resource` row.
    fn declaring(mut self, name: &str, ty: &str) -> Self {
        self.declares.insert(name.to_ascii_lowercase(), ty.to_owned());
        self
    }
    fn without(mut self, name: &str) -> Self {
        self.absent.push(name.to_ascii_lowercase());
        self
    }
    fn on_php(mut self, version: &str) -> Self {
        self.version = version.to_owned();
        self
    }
}

impl FoldEngine for Engine {
    fn env(&mut self) -> Option<EnvInfo> {
        Some(EnvInfo {
            php_version: self.version.clone(),
            extensions: vec!["Core".to_owned(), "standard".to_owned()],
            sapi: "cli".to_owned(),
            int_size: Some(8),
        })
    }
    fn reflect(&mut self, target: &str) -> Option<Reflection> {
        let key = target.to_ascii_lowercase();
        Some(Reflection {
            target: target.to_owned(),
            function_exists: !self.absent.contains(&key),
            class_like_exists: false,
            return_type: self.declares.get(&key).cloned(),
            return_type_tentative: false,
            params_total: None,
            params_required: None,
        })
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
        0
    }
}

/// `type.argument-mismatch` vs `engine` — PROOF-layer id, from a **native** param type.
fn mismatches(src: &str, engine: Engine) -> Vec<String> {
    findings(src, engine, &[steins_infer::ID])
}

/// The `phpdoc.param-mismatch` messages — the CONTRACT-layer id a `@param`
/// violation yields, distinct from [`mismatches`]'s PROOF-layer id (ADR-0022: a
/// docblock is a claim, a native hint a runtime guarantee) — reading the wrong
/// id here would show a false silence.
fn phpdoc_mismatches(src: &str, engine: Engine) -> Vec<String> {
    findings(src, engine, &[steins_infer::PARAM_MISMATCH_ID])
}

/// Both ids — every SILENCE assertion wants: "nothing reported" means neither layer fired.
fn any_mismatch(src: &str, engine: Engine) -> Vec<String> {
    findings(src, engine, &[steins_infer::ID, steins_infer::PARAM_MISMATCH_ID])
}

fn findings(src: &str, engine: Engine, ids: &[&str]) -> Vec<String> {
    let tree = SourceTree::parse(src);
    let mut folder = EngineFolder::with_engine(engine);
    check_with(&tree, &[], "t.php", &mut folder)
        .into_iter()
        .filter(|d: &Diagnostic| ids.contains(&d.id))
        .map(|d| d.message)
        .collect()
}

/// Opens a stream, discharges `false`, passes the handle to `f`; `strict` adds strict_types.
fn narrowed_then_passed(param: &str, strict: bool) -> String {
    format!(
        "<?php\n{}function f({param} $v): void {{}}\n\
         $h = fopen('php://memory', 'r');\n\
         if ($h === false) {{ throw new \\RuntimeException('x'); }}\n\
         f($h);\n",
        if strict { "declare(strict_types=1);\n" } else { "" },
    )
}

// The acceptance criterion (the conformance case).

#[test]
fn a_narrowed_stream_handle_is_rejected_by_every_scalar_parameter() {
    for param in ["string", "int", "float", "bool"] {
        let out = mismatches(&narrowed_then_passed(param, true), Engine::typeless());
        assert_eq!(out.len(), 1, "`{param}` must reject a resource; got {out:?}");
        assert!(
            out[0].contains("holds a resource") && out[0].contains(param),
            "the message must name both the value and the parameter type: {}",
            out[0],
        );
    }
}

#[test]
fn the_verdict_does_not_depend_on_the_coercion_mode() {
    // One mode-independent argument-mismatch finding — no `__toResource` exists, so
    // nothing coerces a resource into `string`; `__toString` DOES coerce an object in
    // coercive mode, hence the different messages.
    let strict = mismatches(&narrowed_then_passed("string", true), Engine::typeless());
    let coercive = mismatches(&narrowed_then_passed("string", false), Engine::typeless());
    assert_eq!(strict.len(), 1);
    assert_eq!(coercive, strict, "dropping strict_types must change nothing");
    assert!(
        strict[0].contains("in either mode"),
        "the message must not name a mode the reader could try to change: {}",
        strict[0],
    );
}

// §8.2 — the tripwire.

#[test]
fn an_engine_that_declares_a_class_disowns_the_row() {
    // A PHP 8 migration, simulated: this engine's `fopen` returns an object. No
    // such PHP exists today — tested anyway, since the tripwire's job is to be
    // right on the day it does.
    let engine = Engine::typeless().declaring("fopen", "SplFileObject|false");
    assert!(
        any_mismatch(&narrowed_then_passed("string", true), engine).is_empty(),
        "a declared return type is the engine speaking; curation must yield (§1 precedence)",
    );
}

#[test]
fn a_name_the_engine_does_not_have_seeds_nothing() {
    // An unloaded extension. The pinned stub still says `resource`; this engine
    // has no such function, so there is no call to say anything about.
    let engine = Engine::typeless().without("fopen");
    assert!(any_mismatch(&narrowed_then_passed("string", true), engine).is_empty());
}

#[test]
fn a_php_off_the_catalog_pin_seeds_nothing() {
    // §8.2 condition 3 (from §2): a curated row is verified at `PINNED_PHP` only.
    let engine = Engine::typeless().on_php("8.4.12");
    assert!(any_mismatch(&narrowed_then_passed("string", true), engine).is_empty());
}

#[test]
fn a_project_function_shadowing_the_builtin_seeds_nothing() {
    // `fopen` declared in the project is not the builtin, whatever the catalog
    // says about the name — the same shadow check the declared floor applies.
    let src = "<?php\ndeclare(strict_types=1);\n\
               function fopen(string $p, string $m): string { return $p; }\n\
               function f(string $v): void {}\n\
               $h = fopen('php://memory', 'r');\n\
               f($h);\n";
    assert!(any_mismatch(src, Engine::typeless()).is_empty());
}

// §8.4 / §8.6 — the `false` arm, and the lane-reading predicate.

#[test]
fn an_undischarged_false_arm_is_not_a_proven_resource() {
    // Straight out of `fopen()` the variable is `resource|false`, and `false` is
    // a valid `bool` arg; two arms is not one, so the lane stays shut (§8.6 clause 1).
    let src = "<?php\ndeclare(strict_types=1);\n\
               function f(bool $v): void {}\n\
               $h = fopen('php://memory', 'r');\n\
               f($h);\n";
    assert!(
        any_mismatch(src, Engine::typeless()).is_empty(),
        "an unchecked fopen() result may genuinely be `false`",
    );
}

#[test]
fn the_false_arm_is_discharged_by_the_ordinary_guard() {
    // Both spellings reach the same one-arm lane; neither is resource-specific —
    // the arm lane's `Refine::Exclude` subtraction does all the work.
    let early_return = "<?php\ndeclare(strict_types=1);\n\
                        function f(bool $v): void {}\n\
                        $h = fopen('php://memory', 'r');\n\
                        if ($h === false) { return; }\n\
                        f($h);\n";
    let negated = "<?php\ndeclare(strict_types=1);\n\
                   function f(bool $v): void {}\n\
                   $h = fopen('php://memory', 'r');\n\
                   if ($h !== false) { f($h); }\n";
    for src in [early_return, negated] {
        let out = mismatches(src, Engine::typeless());
        assert_eq!(out.len(), 1, "the `false` arm must be gone here; got {out:?}");
    }
}

#[test]
fn a_producer_with_no_false_arm_needs_no_guard_at_all() {
    // `stream_context_create` is one of three bare-`resource` stub rows — one arm
    // from the start, pinning that `false` is read from the row, not assumed.
    let src = "<?php\ndeclare(strict_types=1);\n\
               function f(string $v): void {}\n\
               $c = stream_context_create([]);\n\
               f($c);\n";
    assert_eq!(mismatches(src, Engine::typeless()).len(), 1);
}

#[test]
fn a_rebound_variable_loses_the_resource_lane() {
    // Lane dies with its value (ADR-0052 §9); post-rebind `string` is a valid `string` arg.
    let src = "<?php\ndeclare(strict_types=1);\n\
               function f(string $v): void {}\n\
               $h = fopen('php://memory', 'r');\n\
               if ($h === false) { return; }\n\
               $h = 'not a handle';\n\
               f($h);\n";
    assert!(any_mismatch(src, Engine::typeless()).is_empty());
}

// §8.5 — where the definite `No` is claimed, and where it is refused.

#[test]
fn mixed_and_its_cuts_accept_a_resource() {
    // No resource is null, every resource truthy even CLOSED (`fclose($h); (bool)
    // $h === true` at 8.5.9, the guess-wrong case); `mixed` is native, the cuts phpdoc.
    let cases = [
        "function f(mixed $v): void {}",
        "/** @param non-null-mixed $v */\nfunction f($v): void {}",
        "/** @param non-empty-mixed $v */\nfunction f($v): void {}",
    ];
    for decl in cases {
        let src = format!(
            "<?php\ndeclare(strict_types=1);\n{decl}\n\
             $h = fopen('php://memory', 'r');\n\
             if ($h === false) {{ return; }}\n\
             f($h);\n",
        );
        assert!(
            any_mismatch(&src, Engine::typeless()).is_empty(),
            "a resource inhabits `mixed` and both of its cuts: {decl}",
        );
    }
}

#[test]
fn a_resource_parameter_accepts_the_resource_it_asks_for() {
    // Other direction of the leaf: `@param resource`, once an opaque `Maybe`, must
    // still say YES as a relation — refuse-only would be worse than none.
    let src = "<?php\ndeclare(strict_types=1);\n\
               /** @param resource $v */\n\
               function f($v): void {}\n\
               $h = fopen('php://memory', 'r');\n\
               if ($h === false) { return; }\n\
               f($h);\n";
    assert!(any_mismatch(src, Engine::typeless()).is_empty());
}

#[test]
fn a_scalar_handed_to_a_resource_parameter_is_now_a_finding() {
    // Relation's other half, why `resource` left `KNOWN_UNENFORCED`: this was
    // silent but a real TypeError at every PHP boundary; all three spellings agree.
    for spelling in ["resource", "open-resource", "closed-resource"] {
        let src = format!(
            "<?php\ndeclare(strict_types=1);\n\
             /** @param {spelling} $v */\n\
             function f($v): void {{}}\n\
             f('not a handle');\n",
        );
        assert_eq!(
            phpdoc_mismatches(&src, Engine::typeless()).len(),
            1,
            "`@param {spelling}` must refuse a string",
        );
    }
}

#[test]
fn an_object_handed_to_a_resource_parameter_stays_silent() {
    // §8.5's named FP channel — the one verdict the amendment declines to reach:
    // PHP 8 left a decade of `@param resource $ch` attached to params that now
    // receive a `CurlHandle`; convicting that inherited rot would call the programmer a liar.
    let src = "<?php\ndeclare(strict_types=1);\n\
               /** @param resource $v */\n\
               function f($v): void {}\n\
               f(new \\stdClass());\n";
    assert!(any_mismatch(src, Engine::typeless()).is_empty());
}
