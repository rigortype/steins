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
//!   coercion path into anything;
//! * the **heap state** (ADR-0097 §2.3–§2.4): a producer's handle is `Open` on
//!   the heap, aliases share it, a closing call's return proves it `Closed`, a
//!   keeper leaves it alone, and everything else is an escape to `Unknown`.

use std::collections::HashMap;

use steins_infer::{DEBUG_TYPE_ID, Diagnostic, EngineFolder, FoldEngine, check_with};
use steins_sidecar::{
    BuiltinParam, ClassReflection, ConstantDefined, EnvInfo, FoldArg, FoldResult, PregCompile,
    Reflection,
};
use steins_syntax::SourceTree;

// Mock engine: only what `reflect` says about a return type matters (§8.2's tripwire),
// plus the parameter list a keeper (ADR-0097 §2.4) is recognized by.

/// An engine on PHP 8.5 (catalog pin, so §8.2's minor gate passes) that knows every
/// name asked and declares the return types it was given; a name absent from
/// `declares` reflects as **typeless** — the shape a genuine resource producer has.
struct Engine {
    declares: HashMap<String, String>,
    /// Names the engine does NOT have (an unloaded extension).
    absent: Vec<String>,
    /// The PHP version `env()` reports; `8.5.x` is the pin, else it closes the §8.2 gate.
    version: String,
    /// The reflected parameter lists — what makes a builtin a **keeper** (ADR-0097
    /// §2.4): a by-value position cannot close the handle it merely reads. A name
    /// absent here reflects no signature, and such a builtin is an escape.
    params: HashMap<String, Vec<BuiltinParam>>,
}

impl Engine {
    fn typeless() -> Self {
        Engine {
            declares: HashMap::new(),
            absent: Vec::new(),
            version: "8.5.9".to_owned(),
            params: HashMap::new(),
        }
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
    /// `name` takes these parameters, each untyped and by value — `ftell(resource
    /// $stream)`'s reflected shape at 8.5.10 (a resource position declares no type).
    fn taking(mut self, name: &str, names: &[&str]) -> Self {
        let params = names
            .iter()
            .map(|n| BuiltinParam {
                name: (*n).to_owned(),
                ty: None,
                by_ref: false,
                variadic: false,
                optional: false,
            })
            .collect();
        self.params.insert(name.to_ascii_lowercase(), params);
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
            params: self.params.get(&key).cloned(),
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

/// The `\PHPStan\dumpType()` renderings, in source order — what the lane says.
fn dumped(src: &str, engine: Engine) -> Vec<String> {
    findings(src, engine, &[DEBUG_TYPE_ID])
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

// ADR-0097 §2.3–§2.4 — the state lives on the heap: a producer's handle is
// `Open`, a closing call's return proves it `Closed`, a keeper leaves it alone,
// and everything else lets it escape to `Unknown`.

/// A strict-types file declaring `f` with `@param {spelling}`, then `body`.
fn with_state_param(spelling: &str, body: &str) -> String {
    format!(
        "<?php\ndeclare(strict_types=1);\n\
         /** @param {spelling} $v */\n\
         function f($v): void {{}}\n\
         {body}"
    )
}

/// A guarded fresh stream handle in `$h`.
const OPEN_H: &str = "$h = fopen('php://memory', 'r');\nif ($h === false) { return; }\n";

/// An engine that reflects `ftell(resource $stream)` — the keeper the tests below
/// hand the handle to. `Engine::typeless()` reflects no signature for it, and a
/// builtin with no signature is an escape, not a keeper.
fn with_ftell() -> Engine {
    Engine::typeless().taking("ftell", &["stream"])
}

#[test]
fn a_closed_handle_is_not_an_open_resource() {
    // The conformance case: `fclose()` returned, so the handle is closed, and
    // `open-resource` is exactly the set a closed handle is not in.
    let src = with_state_param("open-resource", &format!("{OPEN_H}fclose($h);\nf($h);\n"));
    let out = phpdoc_mismatches(&src, Engine::typeless());
    assert_eq!(out.len(), 1, "a proven-closed handle must convict; got {out:?}");
    assert!(out[0].contains("closed resource"), "the message must say why: {}", out[0]);
}

#[test]
fn the_assert_spelling_of_the_guard_convicts_the_same_way() {
    // The conformance file's own shape: `\assert()` discharges `false`, and an
    // assignment that keeps its result is still a call that returned.
    let src = with_state_param(
        "open-resource",
        "$h = \\fopen('php://memory', 'r');\n\\assert($h !== false);\n$ok = \\fclose($h);\nf($h);\n",
    );
    assert_eq!(phpdoc_mismatches(&src, Engine::typeless()).len(), 1);
}

#[test]
fn a_fresh_handle_is_proven_open() {
    // `Open` at allocation is a proof (ADR-0097 §2.3): the producer returned,
    // `false` was subtracted, nothing has touched the handle — so
    // `closed-resource` refuses it, and the other two spellings take it.
    let src = with_state_param("closed-resource", &format!("{OPEN_H}f($h);\n"));
    let out = phpdoc_mismatches(&src, Engine::typeless());
    assert_eq!(out.len(), 1, "a fresh handle is an open resource; got {out:?}");
    assert!(out[0].contains("open resource"), "the message must say why: {}", out[0]);
    for spelling in ["resource", "open-resource"] {
        let src = with_state_param(spelling, &format!("{OPEN_H}f($h);\n"));
        assert!(any_mismatch(&src, Engine::typeless()).is_empty(), "`@param {spelling}`");
    }
}

#[test]
fn a_closed_handle_is_still_a_resource_and_a_closed_resource() {
    // `gettype()` says `resource (closed)`: the type survives the close.
    for spelling in ["resource", "closed-resource", "mixed", "non-empty-mixed"] {
        let src = with_state_param(spelling, &format!("{OPEN_H}fclose($h);\nf($h);\n"));
        assert!(
            any_mismatch(&src, Engine::typeless()).is_empty(),
            "`@param {spelling}` accepts a closed handle",
        );
    }
}

#[test]
fn a_closed_handle_still_convicts_every_scalar_native_parameter() {
    // Probed at 8.5.10: `string`/`int`/`bool` parameters `TypeError` on a closed
    // handle exactly as on an open one — the proof layer's lane still reads it,
    // state-blind.
    for param in ["string", "int", "bool"] {
        let src = format!(
            "<?php\ndeclare(strict_types=1);\nfunction f({param} $v): void {{}}\n\
             {OPEN_H}fclose($h);\nf($h);\n"
        );
        let out = mismatches(&src, Engine::typeless());
        assert_eq!(out.len(), 1, "`{param}` must still reject a closed handle; got {out:?}");
    }
}

#[test]
fn closing_an_alias_closes_the_handle_every_name_holds() {
    // `$b = $h` binds a second name to the same heap entry (ADR-0097 §2.3), so
    // `fclose($b)` is `fclose($h)` — PHP's own handle semantics, and the case
    // the per-variable lane of ADR-0056 §8.8 could not reach.
    let src = with_state_param(
        "open-resource",
        &format!("{OPEN_H}$b = $h;\nfclose($b);\nf($h);\n"),
    );
    let out = phpdoc_mismatches(&src, Engine::typeless());
    assert_eq!(out.len(), 1, "the alias closed the one handle; got {out:?}");
    // …and the state is monotone: `closed-resource` takes it through either name.
    let src = with_state_param(
        "closed-resource",
        &format!("{OPEN_H}$b = $h;\nfclose($b);\nf($h);\nf($b);\n"),
    );
    assert!(any_mismatch(&src, Engine::typeless()).is_empty());
}

#[test]
fn a_callee_receiving_the_handle_is_an_escape() {
    // A project function may close its argument or keep it; the state is
    // `Unknown` after the call, and `Unknown` convicts nothing in either
    // direction (ADR-0097 §2.4).
    for spelling in ["open-resource", "closed-resource"] {
        let src = format!(
            "<?php\ndeclare(strict_types=1);\n\
             /** @param {spelling} $v */\nfunction f($v): void {{}}\n\
             function userfn(mixed $r): void {{}}\n\
             {OPEN_H}userfn($h);\nf($h);\n"
        );
        assert!(any_mismatch(&src, Engine::typeless()).is_empty(), "`@param {spelling}`");
    }
    // A callee that closes its argument is the same escape, however it closes.
    let src = format!(
        "<?php\ndeclare(strict_types=1);\n\
         /** @param open-resource $v */\nfunction f($v): void {{}}\n\
         function shut(mixed $r): void {{ if (is_resource($r)) {{ fclose($r); }} }}\n\
         {OPEN_H}shut($h);\nf($h);\n"
    );
    assert!(any_mismatch(&src, Engine::typeless()).is_empty());
}

#[test]
fn the_lane_survives_a_keeper_and_the_state_is_unchanged() {
    // `ftell($h)` reads the handle by value: a builtin cannot close a handle it
    // merely reads (ADR-0097 §2.4), so the lane survives — on `master` the call
    // erased it to `unknown` — and the heap state stays `Open`.
    let src = with_state_param("open-resource", &format!("{OPEN_H}ftell($h);\nf($h);\n"));
    assert!(any_mismatch(&src, with_ftell()).is_empty());
    let src = with_state_param("closed-resource", &format!("{OPEN_H}ftell($h);\nf($h);\n"));
    assert_eq!(phpdoc_mismatches(&src, with_ftell()).len(), 1, "still open after a keeper");
    // The dump surface reads the lane: `resource`, not `unknown`.
    let src = format!("<?php\n{OPEN_H}ftell($h);\n\\PHPStan\\dumpType($h);\n");
    assert_eq!(dumped(&src, with_ftell()), vec!["dumped type: resource"]);
    // A builtin the engine reflects no signature for is not a keeper: the lane
    // does not survive on a guess, and a dropped lane convicts nothing.
    let src = format!("<?php\n{OPEN_H}ftell($h);\n\\PHPStan\\dumpType($h);\n");
    assert_eq!(dumped(&src, Engine::typeless()), vec!["dumped type: unknown"]);
    let src = with_state_param("closed-resource", &format!("{OPEN_H}ftell($h);\nf($h);\n"));
    assert!(any_mismatch(&src, Engine::typeless()).is_empty());
}

#[test]
fn is_resource_narrows_the_state_and_keeps_the_lane() {
    // The true branch: the handle answered `true`, so it is open (ADR-0097
    // §2.4) — `closed-resource` refuses it — and the lane is still `resource`
    // on both branches (on `master` the guard erased it in both).
    let src = with_state_param(
        "closed-resource",
        &format!("{OPEN_H}if (is_resource($h)) {{ f($h); }}\n"),
    );
    assert_eq!(phpdoc_mismatches(&src, Engine::typeless()).len(), 1);
    let src = format!(
        "<?php\n{OPEN_H}if (is_resource($h)) {{ \\PHPStan\\dumpType($h); }} \
         else {{ \\PHPStan\\dumpType($h); }}\n"
    );
    assert_eq!(dumped(&src, Engine::typeless()), vec!["dumped type: resource"; 2]);
    // The false branch on a one-arm lane: `is_resource(closed) === false`, so
    // the handle is closed there and `open-resource` refuses it.
    let src = with_state_param(
        "open-resource",
        &format!("{OPEN_H}if (!is_resource($h)) {{ f($h); }}\n"),
    );
    assert_eq!(phpdoc_mismatches(&src, Engine::typeless()).len(), 1);
    // The guard-then-throw idiom keeps the lane and the open state after it.
    let src = with_state_param(
        "closed-resource",
        &format!("{OPEN_H}if (!is_resource($h)) {{ throw new \\RuntimeException('x'); }}\nf($h);\n"),
    );
    assert_eq!(phpdoc_mismatches(&src, Engine::typeless()).len(), 1);
}

#[test]
fn is_resource_false_keeps_the_false_arm_beside_the_closed_handle() {
    // On `resource|false` the false branch is `false` OR a closed handle — PHPStan
    // drops the resource arm there, which is unsound for a closed one, and is not
    // copied. The `false` arm then leaves by the ordinary guard, and what is left
    // is a proven-closed handle.
    let unguarded = "$h = fopen('php://memory', 'r');\n";
    let src = format!("<?php\n{unguarded}if (!is_resource($h)) {{ \\PHPStan\\dumpType($h); }}\n");
    assert_eq!(dumped(&src, Engine::typeless()), vec!["dumped type: false|resource"]);
    let src = with_state_param(
        "open-resource",
        &format!("{unguarded}if (!is_resource($h)) {{ if ($h !== false) {{ f($h); }} }}\n"),
    );
    let out = phpdoc_mismatches(&src, Engine::typeless());
    assert_eq!(out.len(), 1, "closed after `!is_resource`, then not false; got {out:?}");
    // The true branch subtracts `false` by itself: what answered `true` is a resource.
    let src = format!("<?php\n{unguarded}if (is_resource($h)) {{ \\PHPStan\\dumpType($h); }}\n");
    assert_eq!(dumped(&src, Engine::typeless()), vec!["dumped type: resource"]);
}

#[test]
fn a_handle_stored_into_an_array_escapes() {
    // `$arr = [$h]; fclose($arr[0])` closes the handle through a route the walk
    // does not follow: the store is the escape (ADR-0097 §2.4), the state is
    // `Unknown` from then on, and nothing convicts in either direction.
    for spelling in ["open-resource", "closed-resource"] {
        let src = with_state_param(
            spelling,
            &format!("{OPEN_H}$arr = [$h];\nfclose($arr[0]);\nf($h);\n"),
        );
        assert!(any_mismatch(&src, Engine::typeless()).is_empty(), "`@param {spelling}`");
    }
}

#[test]
fn a_branch_that_may_not_have_closed_the_handle_forgets_the_state() {
    // The join rule: `Open ⊔ Closed = Unknown`, silent against both spellings.
    for spelling in ["open-resource", "closed-resource"] {
        let one_side = with_state_param(
            spelling,
            &format!("{OPEN_H}if (random_int(0, 1) === 1) {{ fclose($h); }}\nf($h);\n"),
        );
        assert!(any_mismatch(&one_side, Engine::typeless()).is_empty(), "`@param {spelling}`");
    }
    // Closed on every path stays closed (`Closed ⊔ Closed`).
    let both_sides = with_state_param(
        "open-resource",
        &format!("{OPEN_H}if (random_int(0, 1) === 1) {{ fclose($h); }} else {{ fclose($h); }}\nf($h);\n"),
    );
    assert_eq!(phpdoc_mismatches(&both_sides, Engine::typeless()).len(), 1);
}

#[test]
fn a_loop_that_may_not_run_forgets_the_state() {
    for spelling in ["open-resource", "closed-resource"] {
        let src = with_state_param(
            spelling,
            &format!("{OPEN_H}foreach ([1, 2] as $i) {{ if ($i === 2) {{ fclose($h); }} }}\nf($h);\n"),
        );
        assert!(any_mismatch(&src, Engine::typeless()).is_empty(), "`@param {spelling}`");
        let src = with_state_param(
            spelling,
            &format!("{OPEN_H}while (random_int(0, 1) === 1) {{ fclose($h); break; }}\nf($h);\n"),
        );
        assert!(any_mismatch(&src, Engine::typeless()).is_empty(), "`@param {spelling}`");
        // A loop closing an ALIAS: `$h` is never written by the loop, but the
        // handle it holds may have been closed through `$b` (ADR-0097 §2.4).
        let src = with_state_param(
            spelling,
            &format!("{OPEN_H}$b = $h;\nwhile (random_int(0, 1) === 1) {{ fclose($b); break; }}\nf($h);\n"),
        );
        assert!(any_mismatch(&src, Engine::typeless()).is_empty(), "`@param {spelling}`");
    }
}

#[test]
fn reassignment_and_by_ref_passing_kill_the_proof() {
    let reopened = with_state_param(
        "open-resource",
        &format!("{OPEN_H}fclose($h);\n{OPEN_H}f($h);\n"),
    );
    assert!(any_mismatch(&reopened, Engine::typeless()).is_empty());
    let by_ref = format!(
        "<?php\ndeclare(strict_types=1);\n\
         /** @param open-resource $v */\nfunction f($v): void {{}}\n\
         function g(mixed &$r): void {{}}\n\
         {OPEN_H}fclose($h);\ng($h);\nf($h);\n"
    );
    assert!(any_mismatch(&by_ref, Engine::typeless()).is_empty());
    // `$h = fclose($h)`: the rebind to the call's result is the last word for
    // `$h` — and the close still reaches the entry an alias holds.
    let rebound = with_state_param(
        "open-resource|bool",
        &format!("{OPEN_H}$h = fclose($h);\nf($h);\n"),
    );
    assert!(any_mismatch(&rebound, Engine::typeless()).is_empty());
    let alias_of_rebound = with_state_param(
        "open-resource",
        &format!("{OPEN_H}$b = $h;\n$h = fclose($h);\nf($b);\n"),
    );
    assert_eq!(phpdoc_mismatches(&alias_of_rebound, Engine::typeless()).len(), 1);
}

#[test]
fn fclose_does_not_close_a_directory_handle() {
    // Probed at 8.5.10: `fclose()`/`gzclose()`/`bzclose()` over an `opendir()`
    // handle warn, return `false` and leave it OPEN — the kind on the heap entry
    // is what the closing table reads. `closedir()` and `pclose()` close it.
    let dir = "$h = opendir('.');\nif ($h === false) { return; }\n";
    for closer in ["fclose", "gzclose", "bzclose"] {
        let src = with_state_param("open-resource", &format!("{dir}{closer}($h);\nf($h);\n"));
        assert!(
            any_mismatch(&src, Engine::typeless()).is_empty(),
            "`{closer}` leaves a directory handle open",
        );
        let src = with_state_param("closed-resource", &format!("{dir}{closer}($h);\nf($h);\n"));
        assert_eq!(
            phpdoc_mismatches(&src, Engine::typeless()).len(),
            1,
            "`{closer}` is a keeper over a directory handle: still open",
        );
    }
    for closer in ["closedir", "pclose"] {
        let src = with_state_param("open-resource", &format!("{dir}{closer}($h);\nf($h);\n"));
        assert_eq!(phpdoc_mismatches(&src, Engine::typeless()).len(), 1, "`{closer}`");
    }
}

#[test]
fn the_other_closing_calls_close_what_they_return_from() {
    let cases = [
        ("$h = popen('true', 'r');\nif ($h === false) { return; }\n", "pclose"),
        ("$h = proc_open('true', [], $pipes);\nif ($h === false) { return; }\n", "proc_close"),
        ("$h = pfsockopen('127.0.0.1', 1);\nif ($h === false) { return; }\n", "fclose"),
        (OPEN_H, "gzclose"),
        (OPEN_H, "bzclose"),
    ];
    for (open, closer) in cases {
        let src = with_state_param("open-resource", &format!("{open}{closer}($h);\nf($h);\n"));
        assert_eq!(
            phpdoc_mismatches(&src, Engine::typeless()).len(),
            1,
            "`{closer}` closes the handle it returns from",
        );
    }
}

#[test]
fn nothing_closes_a_lane_that_is_not_a_proven_resource() {
    // `resource|false` is left alone: the lane is not the one-arm resource lane
    // the argument families read, so no state is ever read off it.
    let unguarded = with_state_param(
        "open-resource|false",
        "$h = fopen('php://memory', 'r');\nfclose($h);\nf($h);\n",
    );
    assert!(any_mismatch(&unguarded, Engine::typeless()).is_empty());
}

#[test]
fn a_docblock_state_is_asserted_and_never_reaches_the_heap() {
    // A `@param closed-resource` is a claim about the parameter, not a handle
    // anything allocated: `Asserted`, heap-less, silent against `open-resource`.
    let declared = with_state_param(
        "open-resource",
        "/** @param closed-resource $c */\nfunction g($c): void { f($c); }\n",
    );
    assert!(any_mismatch(&declared, Engine::typeless()).is_empty());
    // A `@return open-resource` seeds the lane at `Asserted` with no heap entry
    // behind it, so `closed-resource` cannot refuse what it returns either.
    let returned = with_state_param(
        "closed-resource",
        "/** @return open-resource */\nfunction g() { return fopen('php://memory', 'r'); }\n\
         $h = g();\nf($h);\n",
    );
    assert!(any_mismatch(&returned, Engine::typeless()).is_empty());
}

#[test]
fn a_project_fclose_is_not_the_closing_builtin() {
    let src = "<?php\nnamespace App;\n\
               /** @param open-resource $v */\nfunction f($v): void {}\n\
               function fclose(mixed $h): void {}\n\
               $h = \\fopen('php://memory', 'r');\nif ($h === false) { return; }\n\
               fclose($h);\nf($h);\n";
    assert!(any_mismatch(src, Engine::typeless()).is_empty());
}

