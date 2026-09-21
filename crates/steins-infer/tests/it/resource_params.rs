//! ADR-0097 §2.5 — the consumer table: a builtin position the pinned stub
//! declares `@param resource`, judged for resource-ness.
//!
//! `resource` is the one parameter type PHP cannot spell, so the engine reports
//! no type at every such position and the ordinary builtin judgment (ADR-0056
//! §9) declines there. The consumer table is the stub reading at the pin, admitted
//! by the same three-condition gate the producer table passes — and, as in
//! `resource_values.rs`, what is worth pinning is each place the gate could
//! quietly stop gating:
//!
//! * the **tripwire**: an engine that declares a type at the position has
//!   migrated it, and the row must vanish;
//! * the **mode independence**: `fwrite('x', …)` and `fwrite(null, …)` are
//!   `TypeError`s with and without `strict_types=1` (probed at 8.5.10 — the
//!   transcripts are in `resource_params.toml`), so the §9.3 null carve-out must
//!   NOT reach a resource position;
//! * the **premise**: only a proven VALUE convicts — a declared `string $s`
//!   forwarded is a type, not a value, and stays silent;
//! * the **shape declines**: by-reference and variadic positions, a union in the
//!   stub, a name the engine lacks, a project shadow, a PHP off the pin.
//!
//! The closed-state cell (`fclose($h); fread($h, 1)`) reads the heap state §2.3
//! put there: `Closed` convicts, `Open` is what the position asks for, and
//! `Unknown` — an escape, a branch that may not have closed it — convicts
//! nothing.

use std::collections::HashMap;

use steins_infer::{Diagnostic, EngineFolder, FoldEngine, Folder, ID, NoFold, SidecarFolder, check_with};
use steins_sidecar::{
    BuiltinParam, ClassReflection, ConstantDefined, EnvInfo, FoldArg, FoldResult, PregCompile,
    Reflection,
};
use steins_syntax::SourceTree;

// ---------------------------------------------------------------------------
// Mock engine: `reflect` answers a parameter list per name, with the resource
// positions UNTYPED — the shape a genuine `@param resource` position has.
// ---------------------------------------------------------------------------

fn p(name: &str, ty: Option<&str>) -> BuiltinParam {
    BuiltinParam {
        name: name.to_owned(),
        ty: ty.map(ToOwned::to_owned),
        by_ref: false,
        variadic: false,
        optional: false,
    }
}

fn optional(name: &str, ty: Option<&str>) -> BuiltinParam {
    BuiltinParam { optional: true, ..p(name, ty) }
}

/// An engine on PHP 8.5 (the catalog pin) whose signatures were read off
/// `ReflectionFunction::getParameters()` at 8.5.10.
struct Engine {
    params: HashMap<String, Vec<BuiltinParam>>,
    /// Names the engine does NOT have (an unloaded extension).
    absent: Vec<String>,
    version: String,
}

impl Engine {
    fn pinned() -> Self {
        let mut params = HashMap::new();
        params.insert(
            "fwrite".to_owned(),
            vec![p("stream", None), p("data", Some("string")), optional("length", Some("?int"))],
        );
        params.insert("fclose".to_owned(), vec![p("stream", None)]);
        params.insert("fread".to_owned(), vec![p("stream", None), p("length", Some("int"))]);
        params.insert("feof".to_owned(), vec![p("stream", None)]);
        params.insert("get_resource_id".to_owned(), vec![p("resource", None)]);
        params.insert(
            "fopen".to_owned(),
            vec![
                p("filename", Some("string")),
                p("mode", Some("string")),
                optional("use_include_path", Some("bool")),
                optional("context", None),
            ],
        );
        params.insert(
            "hash_update_stream".to_owned(),
            vec![p("context", Some("HashContext")), p("stream", None), optional("length", Some("int"))],
        );
        // `imagepng(GdImage $image, $file = null, …)`: `$file` is untyped and the
        // stub says `resource|string|null` — a union, not a row.
        params.insert(
            "imagepng".to_owned(),
            vec![
                p("image", Some("GdImage")),
                optional("file", None),
                optional("quality", Some("int")),
                optional("filters", Some("int")),
            ],
        );
        // `proc_open(…, &$pipes, …)`: by-reference, untyped, and not a row.
        params.insert(
            "proc_open".to_owned(),
            vec![
                p("command", Some("array|string")),
                p("descriptor_spec", Some("array")),
                BuiltinParam { by_ref: true, ..p("pipes", None) },
                optional("cwd", Some("?string")),
            ],
        );
        Engine { params, absent: Vec::new(), version: "8.5.10".to_owned() }
    }

    /// The migrated shape: the engine declaring a type at the position is the
    /// engine disowning the row.
    fn typing(mut self, name: &str, index: usize, ty: &str) -> Self {
        self.params.get_mut(name).expect("a known name")[index].ty = Some(ty.to_owned());
        self
    }

    fn by_ref_at(mut self, name: &str, index: usize) -> Self {
        self.params.get_mut(name).expect("a known name")[index].by_ref = true;
        self
    }

    fn variadic_at(mut self, name: &str, index: usize) -> Self {
        self.params.get_mut(name).expect("a known name")[index].variadic = true;
        self
    }

    fn without(mut self, name: &str) -> Self {
        self.absent.push(name.to_owned());
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
        let known = self.params.contains_key(&key) && !self.absent.contains(&key);
        Some(Reflection {
            target: target.to_owned(),
            function_exists: known,
            class_like_exists: false,
            // No producer here declares a return type — `fopen` stays a resource row.
            return_type: None,
            return_type_tentative: false,
            params_total: None,
            params_required: None,
            params: known.then(|| self.params[&key].clone()),
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

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

fn findings_with(src: &str, folder: &mut dyn Folder) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "t.php", folder)
}

/// The `type.argument-mismatch` messages a source produces against `engine`.
fn mismatches_on(src: &str, engine: Engine) -> Vec<String> {
    let mut folder = EngineFolder::with_engine(engine);
    findings_with(src, &mut folder).into_iter().filter(|d| d.id == ID).map(|d| d.message).collect()
}

fn mismatches(src: &str) -> Vec<String> {
    mismatches_on(src, Engine::pinned())
}

fn coercive(body: &str) -> String {
    format!("<?php\n{body}")
}

fn strict(body: &str) -> String {
    format!("<?php\ndeclare(strict_types=1);\n{body}")
}

/// One finding, identical in both modes, whose message is `expected`.
fn one_in_both_modes(body: &str, expected: &str) {
    for src in [coercive(body), strict(body)] {
        let out = mismatches(&src);
        assert_eq!(out, vec![expected.to_owned()], "for:\n{src}");
    }
}

fn silent_in_both_modes(body: &str) {
    for src in [coercive(body), strict(body)] {
        let out = mismatches(&src);
        assert!(out.is_empty(), "expected silence, got {out:?} for:\n{src}");
    }
}

// ---------------------------------------------------------------------------
// The finding: a proven non-resource at a resource position, in either mode
// ---------------------------------------------------------------------------

#[test]
fn a_string_literal_to_fwrite_is_a_proven_type_error_in_both_modes() {
    one_in_both_modes(
        "fwrite('x', 'y');\n",
        "argument \"x\" to fwrite() cannot become resource $stream — proven TypeError (must be of type resource, in either mode)",
    );
}

#[test]
fn every_other_runtime_kind_convicts_at_fwrite_fclose_and_fread() {
    // int, float, bool, null, array, object — the seven kinds minus resource
    // (ADR-0097 §2.1), each probed a `TypeError` at 8.5.10 with and without
    // `strict_types=1`.
    one_in_both_modes(
        "fclose(1);\n",
        "argument 1 to fclose() cannot become resource $stream — proven TypeError (must be of type resource, in either mode)",
    );
    one_in_both_modes(
        "fwrite(1.5, 'y');\n",
        "argument 1.5 to fwrite() cannot become resource $stream — proven TypeError (must be of type resource, in either mode)",
    );
    one_in_both_modes(
        "fwrite(true, 'y');\n",
        "argument true to fwrite() cannot become resource $stream — proven TypeError (must be of type resource, in either mode)",
    );
    one_in_both_modes(
        "fread([], 1);\n",
        "argument [] to fread() cannot become resource $stream — proven TypeError (must be of type resource, in either mode)",
    );
    one_in_both_modes(
        "fwrite(new \\stdClass(), 'y');\n",
        "argument new stdClass() to fwrite() cannot become resource $stream — proven TypeError (must be of type resource, in either mode)",
    );
}

#[test]
fn null_convicts_in_coercive_mode_too_because_the_carve_out_is_for_scalar_positions() {
    // ADR-0056 §9.3's internal-null carve-out is a DEPRECATION for a non-nullable
    // scalar position; `fclose(null)` is `TypeError: fclose(): Argument #1
    // ($stream) must be of type resource, null given` with no `strict_types` at
    // all (probed at 8.5.10), so the carve-out must not reach this arm.
    one_in_both_modes(
        "fclose(null);\n",
        "argument null to fclose() cannot become resource $stream — proven TypeError (must be of type resource, in either mode)",
    );
}

#[test]
fn a_propagated_variable_convicts_with_its_provenance() {
    // Not `one_in_both_modes`: the `declare` line shifts the assignment's line.
    let body = "$s = 'x';\nfwrite($s, 'y');\n";
    let tail = "to fwrite() cannot become resource $stream — proven TypeError (must be of type resource, in either mode)";
    assert_eq!(
        mismatches(&coercive(body)),
        vec![format!("argument \"x\" (from $s, assigned at line 2) {tail}")]
    );
    assert_eq!(
        mismatches(&strict(body)),
        vec![format!("argument \"x\" (from $s, assigned at line 3) {tail}")]
    );
}

#[test]
fn a_variable_the_heap_binds_to_an_object_convicts() {
    // ADR-0036's heap: `$o` is exactly a `stdClass`, and no class has resource
    // instances. `SplFileObject` fails the same way at the engine (ADR-0097 §1.1).
    one_in_both_modes(
        "$o = new \\stdClass();\nfwrite($o, 'y');\n",
        "argument $o (holds a stdClass) to fwrite() cannot become resource $stream — proven TypeError (must be of type resource, in either mode)",
    );
}

#[test]
fn a_position_past_the_first_is_judged_and_named() {
    // `hash_update_stream(HashContext $context, $stream, …)`: the row is at
    // position 1, and the message names `$stream`, not `$context`.
    let out = mismatches(&strict("hash_update_stream($ctx, 'x');\n"));
    assert_eq!(
        out,
        vec!["argument \"x\" to hash_update_stream() cannot become resource $stream — proven TypeError (must be of type resource, in either mode)".to_owned()]
    );
}

#[test]
fn the_same_call_judges_its_typed_positions_beside_the_resource_one() {
    // `fwrite($stream, string $data)`: the resource arm and the reflected arm
    // report on the same call, one finding each, neither shadowing the other.
    let out = mismatches(&strict("fwrite('x', 1);\n"));
    assert_eq!(out.len(), 2, "{out:?}");
    assert!(out[0].contains("cannot become resource $stream"), "{}", out[0]);
    assert!(out[1].contains("cannot become string $data"), "{}", out[1]);
}

// ---------------------------------------------------------------------------
// The acceptance: a resource is what the position asks for
// ---------------------------------------------------------------------------

#[test]
fn a_narrowed_fopen_handle_is_silent() {
    silent_in_both_modes(
        "$h = fopen('php://memory', 'r');\n\
         if ($h === false) { throw new \\RuntimeException('x'); }\n\
         fwrite($h, 'y');\nfread($h, 1);\nfclose($h);\n",
    );
}

#[test]
fn an_any_state_position_accepts_the_handle_and_refuses_a_scalar() {
    // `get_resource_id`'s row is `accepts_closed = true` (probed); the
    // resource-ness judgment is the same as everywhere else.
    silent_in_both_modes(
        "$h = fopen('php://memory', 'r');\n\
         if ($h === false) { throw new \\RuntimeException('x'); }\n\
         get_resource_id($h);\n",
    );
    one_in_both_modes(
        "get_resource_id('x');\n",
        "argument \"x\" to get_resource_id() cannot become resource $resource — proven TypeError (must be of type resource, in either mode)",
    );
}

// ---------------------------------------------------------------------------
// The state cell (§2.5's second): a CLOSED handle where the row wants an open one
// ---------------------------------------------------------------------------

#[test]
fn a_closed_handle_where_the_position_wants_an_open_one_is_a_type_error_in_both_modes() {
    // Probed at 8.5.10: `fclose($h); fread($h, 1)` is `TypeError: fread():
    // Argument #1 ($stream) must be an open stream resource`, with and without
    // `declare(strict_types=1)`.
    one_in_both_modes(
        "$h = fopen('php://memory', 'r');\n\
         if ($h === false) { throw new \\RuntimeException('x'); }\n\
         fclose($h);\nfread($h, 1);\n",
        "argument $h to fread() cannot become resource $stream — the handle is closed; proven TypeError (must be an open stream resource, in either mode)",
    );
}

#[test]
fn the_state_is_the_handles_so_an_alias_closes_what_the_original_holds() {
    // §2.3: the state lives on the heap entry, not on the name. Closing through
    // either name convicts a later use of the other.
    one_in_both_modes(
        "$h = fopen('php://memory', 'r');\n\
         if ($h === false) { throw new \\RuntimeException('x'); }\n\
         $b = $h;\nfclose($b);\nfread($h, 1);\n",
        "argument $h to fread() cannot become resource $stream — the handle is closed; proven TypeError (must be an open stream resource, in either mode)",
    );
}

#[test]
fn closing_a_handle_twice_is_the_same_finding_because_fclose_wants_an_open_one() {
    // `fclose`'s own row is `accepts_closed = false` — probed: a second
    // `fclose($h)` raises the same `TypeError`.
    one_in_both_modes(
        "$h = fopen('php://memory', 'r');\n\
         if ($h === false) { throw new \\RuntimeException('x'); }\n\
         fclose($h);\nfclose($h);\n",
        "argument $h to fclose() cannot become resource $stream — the handle is closed; proven TypeError (must be an open stream resource, in either mode)",
    );
}

#[test]
fn an_accepts_closed_position_takes_the_closed_handle() {
    // `get_resource_id` and `get_resource_type` are the two probed rows that
    // accept one — `get_resource_type` answers `'Unknown'` rather than raising.
    silent_in_both_modes(
        "$h = fopen('php://memory', 'r');\n\
         if ($h === false) { throw new \\RuntimeException('x'); }\n\
         fclose($h);\nget_resource_id($h);\nget_resource_type($h);\n",
    );
}

#[test]
fn a_branch_that_may_not_have_closed_the_handle_convicts_nothing() {
    // §2.4: `Open ⊔ Closed` is `Unknown`, and `Unknown` convicts nothing. The
    // handle here is closed on one path only, which is not a proof.
    silent_in_both_modes(
        "$h = fopen('php://memory', 'r');\n\
         if ($h === false) { throw new \\RuntimeException('x'); }\n\
         if (random_int(0, 1) === 1) { fclose($h); }\nfread($h, 1);\n",
    );
}

#[test]
fn a_handle_that_escaped_into_a_project_call_convicts_nothing() {
    // The callee may close it or not; an escape drops the state to `Unknown`,
    // and the closed cell asks for a proof it no longer has.
    silent_in_both_modes(
        "function sink($r): void {}\n\
         $h = fopen('php://memory', 'r');\n\
         if ($h === false) { throw new \\RuntimeException('x'); }\n\
         sink($h);\nfread($h, 1);\n",
    );
}

#[test]
fn an_open_handle_stays_silent_at_the_same_positions() {
    // The other half of the cell: `Open` is exactly what the position asks for.
    silent_in_both_modes(
        "$h = fopen('php://memory', 'r');\n\
         if ($h === false) { throw new \\RuntimeException('x'); }\n\
         fread($h, 1);\nfclose($h);\n",
    );
}

// ---------------------------------------------------------------------------
// The premise: a proven VALUE, never an abstract fact
// ---------------------------------------------------------------------------

#[test]
fn a_declared_string_parameter_forwarded_is_silent() {
    // A `string $s` is a type the callee's signature guarantees, not a value the
    // walk proved; the resource cell of the possibly pair does not exist, and this
    // slice adds none. Silence, in both modes, in a project function and a method.
    silent_in_both_modes("function f(string $s): void { fwrite($s, 'y'); }\n");
    silent_in_both_modes("function g(mixed $m): void { fclose($m); }\n");
    silent_in_both_modes(
        "/** @return resource */\nfunction h() { return fopen('php://memory', 'r'); }\n\
         $x = h();\nfwrite($x, 'y');\n",
    );
}

#[test]
fn an_unproven_argument_is_silent() {
    silent_in_both_modes("fwrite($unknown, 'y');\n");
    silent_in_both_modes("fwrite(strlen('a') > 0 ? 'x' : $y, 'y');\n");
}

// ---------------------------------------------------------------------------
// The element carrier (ADR-0098): a place, not only a variable, holds a handle
// ---------------------------------------------------------------------------

#[test]
fn an_element_of_an_array_literal_is_the_handle_the_variable_holds() {
    // Probed at 8.5.10: `$arr = [$h]; fclose($arr[0]);` leaves `is_resource($h)`
    // false and `gettype($h)` reading `resource (closed)` — one handle, two
    // names. So a use through EITHER name after a close through EITHER name is
    // the same `TypeError`.
    one_in_both_modes(
        "$h = fopen('php://memory', 'r');\n\
         if ($h === false) { throw new \\RuntimeException('x'); }\n\
         $arr = [$h];\nfclose($arr[0]);\nfread($h, 1);\n",
        "argument $h to fread() cannot become resource $stream — the handle is closed; proven TypeError (must be an open stream resource, in either mode)",
    );
    one_in_both_modes(
        "$h = fopen('php://memory', 'r');\n\
         if ($h === false) { throw new \\RuntimeException('x'); }\n\
         $arr = [$h];\nfclose($h);\nfread($arr[0], 1);\n",
        "argument $arr[0] to fread() cannot become resource $stream — the handle is closed; proven TypeError (must be an open stream resource, in either mode)",
    );
}

#[test]
fn a_string_key_names_a_place_too() {
    one_in_both_modes(
        "$h = fopen('php://memory', 'r');\n\
         if ($h === false) { throw new \\RuntimeException('x'); }\n\
         $arr = ['in' => $h];\nfclose($arr['in']);\nfread($arr['in'], 1);\n",
        // The message renders the ARGUMENT, which spells its key the way every
        // other value message does; the place key underneath it is canonical
        // (`arr['in']`), and the two never have to agree.
        "argument $arr[\"in\"] to fread() cannot become resource $stream — the handle is closed; proven TypeError (must be an open stream resource, in either mode)",
    );
}

#[test]
fn an_open_element_is_what_the_position_asks_for() {
    silent_in_both_modes(
        "$h = fopen('php://memory', 'r');\n\
         if ($h === false) { throw new \\RuntimeException('x'); }\n\
         $arr = [$h];\nfwrite($arr[0], 'y');\nfread($arr[0], 1);\nfclose($arr[0]);\n",
    );
}

// ---------------------------------------------------------------------------
// Which keys name a place (ADR-0098 §2.2 as amended): the stratum floor
// ---------------------------------------------------------------------------

#[test]
fn a_key_the_walk_proved_names_the_place_it_proves() {
    // The rule is not "a literal key" but "a key proven at `Verified`" — the
    // same floor `offset_operand_fact` puts on an offset key one seam over, and
    // the floor `check_resource_position` already puts on the argument's value.
    // `$i = 0` is a proof, so `$arr[$i]` IS `arr[0]`, and reading it convicts.
    one_in_both_modes(
        "$h = fopen('php://memory', 'r');\n\
         if ($h === false) { throw new \\RuntimeException('x'); }\n\
         $arr = [$h];\nfclose($arr[0]);\n$i = 0;\nfread($arr[$i], 1);\n",
        "argument $arr[$i] to fread() cannot become resource $stream — the handle is closed; proven TypeError (must be an open stream resource, in either mode)",
    );
    // …and it picks the RIGHT entry: `$i = 1` names the open sibling.
    silent_in_both_modes(
        "$h = fopen('php://memory', 'r');\n\
         $g = fopen('php://memory', 'r');\n\
         if ($h === false || $g === false) { throw new \\RuntimeException('x'); }\n\
         $arr = [$h, $g];\nfclose($arr[0]);\n$i = 1;\nfread($arr[$i], 1);\n",
    );
}

#[test]
fn an_asserted_key_names_no_place() {
    // The blocker this gate closes. `@phpstan-assert 0 $i` on an EMPTY body is a
    // docblock claim, `Asserted`, never a proof — and a key decides WHICH
    // allocation the closed-state judgment reads, so an unverified claim would
    // pick the accusation's subject. Probed at 8.5.10: this file prints `ok` and
    // exits 0, because `$i` is `1` and `$arr[1]` is the open handle.
    silent_in_both_modes(
        "/** @phpstan-assert 0 $i */\nfunction assert_zero(int $i): void {}\n\
         function rnd(): int { return random_int(1, 1); }\n\
         $h = fopen('php://memory', 'r');\n\
         $g = fopen('php://memory', 'r');\n\
         if ($h === false || $g === false) { throw new \\RuntimeException('x'); }\n\
         $arr = [$h, $g];\nfclose($arr[0]);\n\
         $i = rnd();\nassert_zero($i);\nfread($arr[$i], 1);\n",
    );
}

#[test]
fn an_unprovable_key_names_no_place() {
    // Nothing proves `$i` at all: no place, no judgment, in either direction.
    silent_in_both_modes(
        "$h = fopen('php://memory', 'r');\n\
         if ($h === false) { throw new \\RuntimeException('x'); }\n\
         $arr = [$h];\nfclose($arr[0]);\n$i = random_int(0, 1);\nfread($arr[$i], 1);\n",
    );
}

#[test]
fn a_dynamic_key_closes_no_place() {
    // The other half, and the asymmetry it pins: the EFFECT seam has no env to
    // ask, so `fclose($arr[$i])` moves no state even where `$i` is proven. A
    // missed close is silence; the read seam can name more places than the close
    // seam can move, and a place is `Closed` only where a close put it there.
    silent_in_both_modes(
        "$h = fopen('php://memory', 'r');\n\
         if ($h === false) { throw new \\RuntimeException('x'); }\n\
         $i = 0;\n$arr = [$h];\nfclose($arr[$i]);\nfread($arr[0], 1);\n",
    );
    silent_in_both_modes(
        "$h = fopen('php://memory', 'r');\n\
         if ($h === false) { throw new \\RuntimeException('x'); }\n\
         $i = 0;\n$arr = [$h];\nfclose($arr[$i]);\nfread($arr[$i], 1);\n",
    );
}

#[test]
fn a_branch_that_may_not_have_closed_the_element_convicts_nothing() {
    silent_in_both_modes(
        "$h = fopen('php://memory', 'r');\n\
         if ($h === false) { throw new \\RuntimeException('x'); }\n\
         $arr = [$h];\n\
         if (random_int(0, 1) === 1) { fclose($arr[0]); }\nfread($arr[0], 1);\n",
    );
}

#[test]
fn a_rebound_base_takes_its_places_with_it() {
    // The sweep (§2.3): the place must not outlive the base. `$arr` is a fresh
    // array by the time the read happens, and nothing is proven about `$arr[0]`.
    silent_in_both_modes(
        "$h = fopen('php://memory', 'r');\n\
         if ($h === false) { throw new \\RuntimeException('x'); }\n\
         $arr = [$h];\nfclose($arr[0]);\n$arr = [];\nfread($arr[0], 1);\n",
    );
}

// ---------------------------------------------------------------------------
// The gate: each condition, switched off one at a time
// ---------------------------------------------------------------------------

#[test]
fn the_tripwire_an_engine_that_declares_a_type_at_the_position_disowns_the_row() {
    // The migration, simulated: this engine's `fwrite` declares `Stream $stream`.
    // No such PHP exists today; the tripwire's job is to be right the day it does.
    let engine = Engine::pinned().typing("fwrite", 0, "Stream");
    for src in [coercive("fwrite('x', 'y');\n"), strict("fwrite('x', 'y');\n")] {
        assert!(
            mismatches_on(&src, engine.clone_for_test()).is_empty(),
            "a declared type is the engine speaking; the stub row must yield"
        );
    }
}

#[test]
fn a_name_the_engine_does_not_have_judges_nothing() {
    let engine = Engine::pinned().without("fwrite");
    assert!(mismatches_on(&strict("fwrite('x', 'y');\n"), engine).is_empty());
}

#[test]
fn a_php_off_the_catalog_pin_judges_nothing() {
    let engine = Engine::pinned().on_php("8.4.12");
    assert!(mismatches_on(&strict("fwrite('x', 'y');\n"), engine).is_empty());
}

#[test]
fn the_sound_subset_judges_nothing() {
    // `--no-php`: no engine, no tripwire, no row.
    let ds = findings_with(&strict("fwrite('x', 'y');\n"), &mut NoFold);
    assert!(ds.iter().all(|d| d.id != ID), "{ds:?}");
}

#[test]
fn a_project_function_shadowing_the_name_is_not_the_builtin() {
    silent_in_both_modes("function fwrite($s, string $d): int { return 1; }\nfwrite('x', 'y');\n");
}

#[test]
fn a_union_typed_stub_position_is_not_a_row() {
    // `imagepng`'s `$file` is untyped at the engine and `resource|string|null` in
    // the stub — declined at mining time, so a string there is exactly what the
    // union admits.
    silent_in_both_modes("imagepng($img, 'out.png');\n");
    // `fopen`'s `$context` is `resource|null`: the same.
    silent_in_both_modes("fopen('php://memory', 'r', false, 'ctx');\n");
}

#[test]
fn by_reference_and_variadic_positions_are_untouched() {
    // `proc_open`'s `$pipes` is by-reference and untyped: not a row, and the arm
    // never reaches it. And a row whose ENGINE position is by-reference or
    // variadic (no such builtin exists; the gate is pinned anyway) is disowned.
    silent_in_both_modes("proc_open('true', [], 'notavar');\n");
    let by_ref = Engine::pinned().by_ref_at("fwrite", 0);
    assert!(mismatches_on(&strict("fwrite('x', 'y');\n"), by_ref).is_empty());
    let variadic = Engine::pinned().variadic_at("fwrite", 0);
    assert!(mismatches_on(&strict("fwrite('x', 'y');\n"), variadic).is_empty());
}

#[test]
fn named_arguments_and_unpacking_decline_the_whole_call() {
    // ADR-0056 §9.4 (v1): the position→parameter map this judgment is indexed by
    // does not survive either.
    silent_in_both_modes("fwrite(stream: 'x', data: 'y');\n");
    silent_in_both_modes("$a = ['x', 'y'];\nfwrite(...$a);\n");
}

impl Engine {
    /// A fresh engine with the same shape, for fixtures that run two sources.
    fn clone_for_test(&self) -> Self {
        Engine { params: self.params.clone(), absent: self.absent.clone(), version: self.version.clone() }
    }
}

// ---------------------------------------------------------------------------
// The live lane: the same verdict off a real `php`, no mock anywhere
// ---------------------------------------------------------------------------

#[test]
fn a_live_php_at_the_pin_answers_the_same_verdict_the_mock_does() {
    let mut folder = SidecarFolder::enabled();
    let Some(params) = folder.builtin_param_types("fwrite") else {
        eprintln!("SKIP: no reflection engine — is `php` on PATH?");
        return;
    };
    if folder.php_minor() != Some(steins_catalog::PINNED_PHP) {
        eprintln!("SKIP: the live php is off the catalog pin, so the row is not admitted by design");
        return;
    }
    // The signature the file is written against, straight off the engine: an
    // untyped first position — the shape the tripwire reads as "not migrated".
    assert_eq!(params[0].name, "stream");
    assert_eq!(params[0].ty, None, "fwrite's $stream must be untyped on a PHP that still has resources");
    let row = folder.builtin_resource_param("fwrite", 0).expect("the live gate admits fwrite #0");
    assert_eq!(row.name, "stream");
    assert!(folder.builtin_resource_param("fwrite", 1).is_none(), "`string $data` is not a resource position");

    let reported = |src: &str, folder: &mut SidecarFolder| {
        findings_with(src, folder).into_iter().any(|d| d.id == ID)
    };
    assert!(reported(&coercive("fwrite('x', 'y');\n"), &mut folder));
    assert!(reported(&strict("fwrite('x', 'y');\n"), &mut folder));
    assert!(reported(&coercive("fclose(null);\n"), &mut folder), "no null carve-out at a resource position");
    assert!(!reported(
        &strict("$h = fopen('php://memory', 'r');\nif ($h === false) { return; }\nfwrite($h, 'y');\n"),
        &mut folder
    ));
    assert!(!reported(&strict("function f(string $s): void { fwrite($s, 'y'); }\n"), &mut folder));
}
