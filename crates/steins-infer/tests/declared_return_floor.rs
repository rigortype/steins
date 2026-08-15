//! ADR-0069 / issues #73, #79, ADR-0071 — the Asserted declared-return floor.
//!
//! Where the engine is silent about a builtin's return type, the catalog's mined
//! declaration answers instead (`Asserted`, per NAME not per run). Issue #79
//! widened it to unions/scalar refinements; ADR-0071 again to array/class
//! vocabularies — grade, firewall and silence condition unchanged throughout.
//!
//! The dump surface's `(asserted)` marker (ADR-0053 §2 / ADR-0052 §5) says which
//! rung answered — a reflected envelope carries none, the floor always carries
//! one. The seeded fact is `Asserted`, read by the proof layer's all-Verified
//! premise rule; the absence family neither consumes it nor is suppressed by it.

use std::collections::HashMap;
use std::path::PathBuf;

use steins_db::{
    GoverningRoot, PhpTarget, PhpTargetSource, Project, ProjectLayout, SourceFile, SteinsDatabase,
};
use steins_domain::{Base, Fact};
use steins_infer::{
    CALL_UNDEFINED_FUNCTION_ID, DEBUG_PHPDOC_TYPE_ID, DEBUG_TYPE_ID, Diagnostic, Folder, Layer,
    NoFold, RETURN_MISMATCH_ID, check, check_project_with_runtime, check_with, layer,
};
use steins_syntax::{ArgValue, SourceTree};

/// Reflects a return type for given names, silent for every other — the shape
/// of a real PHP whose extensions don't cover the whole call graph.
#[derive(Default)]
struct Engine {
    facts: HashMap<String, Fact>,
    /// Names the boot surface reports as resident functions. Only consulted when
    /// [`Self::absence`] is set.
    resident: Vec<String>,
    absence: bool,
}

impl Engine {
    fn reflecting(name: &str, fact: Fact) -> Self {
        let mut e = Engine::default();
        e.facts.insert(name.to_ascii_lowercase(), fact);
        e
    }
    fn and_reflecting(mut self, name: &str, fact: Fact) -> Self {
        self.facts.insert(name.to_ascii_lowercase(), fact);
        self
    }
    /// A live, monkey-patch-free engine whose boot surface knows exactly `resident`.
    fn with_boot_surface(mut self, resident: &[&str]) -> Self {
        self.absence = true;
        self.resident = resident.iter().map(|n| n.to_ascii_lowercase()).collect();
        self
    }
}

impl Folder for Engine {
    fn fold(&mut self, _name: &str, _args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
        None
    }
    fn builtin_return_fact(&mut self, name: &str) -> Option<Fact> {
        self.facts.get(&name.to_ascii_lowercase()).cloned()
    }
    fn absence_family_available(&mut self) -> bool {
        self.absence
    }
    fn boot_surface_function(&mut self, fqn: &str) -> Option<bool> {
        self.absence.then(|| self.resident.iter().any(|n| n.eq_ignore_ascii_case(fqn)))
    }
    fn boot_surface_label(&mut self) -> Option<String> {
        self.absence.then(|| "PHP 8.5.8 (32 extensions)".to_owned())
    }
}

fn general(base: Base) -> Fact {
    Fact::General { base, nullable: false }
}

/// The `debug.type` message bodies under `NoFold` (`--no-php`), in source order.
fn no_php_dumps(src: &str) -> Vec<String> {
    let tree = SourceTree::parse(src);
    check(&tree, &[], "t.php")
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message)
        .collect()
}

/// The same, with an engine standing behind the folder seam.
fn dumps_with(src: &str, folder: &mut dyn Folder) -> Vec<String> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "t.php", folder)
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message)
        .collect()
}

fn run(src: &str, folder: &mut dyn Folder) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "t.php", folder)
}

fn require_target(raw: &str, floor: (u16, u16), ceiling: Option<(u16, u16)>) -> PhpTarget {
    PhpTarget { floor, ceiling, source: PhpTargetSource::Require, raw: raw.to_owned() }
}

/// The dumped type of `call` under a project whose layout DECLARES `target` — the
/// seam `floor_target_admits` reads (issue #28's layout→Cx path).
fn dump_call_under_target(call: &str, target: Option<PhpTarget>) -> String {
    let root = GoverningRoot::new(
        PathBuf::from("/proj/composer.json"),
        PathBuf::from("/proj"),
        vec![PathBuf::from("/proj/vendor")],
        vec![],
    )
    .with_php_target(target);
    let layout = ProjectLayout::new(PathBuf::from("/proj"), vec![root]);
    let db = SteinsDatabase::default();
    let src =
        format!("<?php\nfunction f(string $s, int $n): void {{ \\PHPStan\\dumpType({call}); }}\n");
    let file = SourceFile::new(&db, "/proj/t.php".to_owned(), src);
    let project = Project::new(&db, vec![file], layout, steins_db::PluginFacts::none());
    check_project_with_runtime(&db, project, &mut NoFold, true)
        .into_iter()
        .find(|d| d.id == DEBUG_TYPE_ID)
        .expect("one dump")
        .message
}

/// The same on `strstr`, whose type never moved — the version gate's complement.
fn dump_under_target(target: Option<PhpTarget>) -> String {
    dump_call_under_target("strstr($s, $s)", target)
}

#[test]
fn str_repeat_of_variables_dumps_string_under_no_php() {
    // ADR-0069 §1's worked example: before the floor, both dumped `unknown` (fold
    // cannot reach variable operands, and every other rung is engine-gated).
    let src = "<?php\nfunction f(string $s, int $n): void {\n\
               $r = str_repeat($s, $n);\n\
               \\PHPStan\\dumpType($r);\n\
               \\PHPStan\\dumpType(str_repeat($s, $n));\n}\n";
    assert_eq!(
        no_php_dumps(src),
        vec!["dumped type: string (asserted)".to_owned(), "dumped type: string (asserted)".to_owned()],
        "both the assignment rung and the dump rung must reach the floor",
    );
}

/// The `debug.type` message body of one call expression under the sound subset.
fn probe(call: &str) -> String {
    let src =
        format!("<?php\nfunction f(string $s, int $n, $h): void {{ \\PHPStan\\dumpType({call}); }}\n");
    no_php_dumps(&src).first().cloned().unwrap_or_default()
}

#[test]
fn the_floor_covers_the_scalar_bases_and_nothing_else() {
    // One row per envelope shape — a lowering regression in arm seeding shows here.
    assert_eq!(probe("str_pad($s, $n)"), "dumped type: string (asserted)");
    assert_eq!(probe("curl_errno($h)"), "dumped type: int (asserted)");
    assert_eq!(probe("acos($n)"), "dumped type: float (asserted)");
    assert_eq!(probe("array_key_exists($s, [])"), "dumped type: bool (asserted)");
    // A `?T` row keeps its nullability — the floor states a type, not a base.
    assert_eq!(probe("curl_multi_getcontent($h)"), "dumped type: string|null (asserted)");
    assert_eq!(probe("sodium_add($s, $s)"), "dumped type: unknown");
}

#[test]
fn a_t_false_row_renders_as_the_contract_lane_spells_it() {
    // Issue #79: `strstr` is `string|false` in functionMap; the row now seeds
    // the ARM lane so the dump surface spells the union like any contract arm.
    assert_eq!(probe("strstr($s, $s)"), "dumped type: string|false (asserted)");
    assert_eq!(probe("strrchr($s, $s)"), "dumped type: string|false (asserted)");
    assert_eq!(probe("file_get_contents($s)"), "dumped type: string|false (asserted)");
    // Three or more arms compose the same way — no special case for the pair.
    assert_eq!(probe("array_search($s, [])"), "dumped type: int|string|false (asserted)");
    // The other #79 bucket: a scalar refinement (envelope_fact would flatten to base).
    assert_eq!(probe("mb_strtoupper($s)"), "dumped type: uppercase-string (asserted)");
    assert_eq!(probe("preg_match($s, $s)"), "dumped type: 0|1|false (asserted)");
}

#[test]
fn a_t_false_row_survives_the_assignment_rung_and_the_declared_surface() {
    // The assignment seam seeds both carriers; a multi-arm row lives in the arm
    // lane alone (no single value-domain fact), and all four forms must agree.
    let dumped = |src: &str| {
        let tree = SourceTree::parse(src);
        let msgs: Vec<String> = check(&tree, &[], "t.php")
            .into_iter()
            .filter(|d| d.id == DEBUG_TYPE_ID || d.id == DEBUG_PHPDOC_TYPE_ID)
            .map(|d| d.message)
            .collect();
        msgs.first().cloned().unwrap_or_default()
    };
    let fun = |body: &str| format!("<?php\nfunction f(string $s): void {{ {body} }}\n");
    assert_eq!(
        dumped(&fun("\\PHPStan\\dumpType(strstr($s, $s));")),
        "dumped type: string|false (asserted)"
    );
    assert_eq!(
        dumped(&fun("\\PHPStan\\dumpPhpDocType(strstr($s, $s));")),
        "dumped phpdoc type: string|false (asserted)"
    );
    assert_eq!(
        dumped(&fun("$r = strstr($s, $s); \\PHPStan\\dumpType($r);")),
        "dumped type: string|false (asserted)"
    );
    assert_eq!(
        dumped(&fun("$r = strstr($s, $s); \\PHPStan\\dumpPhpDocType($r);")),
        "dumped phpdoc type: string|false (asserted)"
    );
}

#[test]
fn an_engine_answer_wins_over_a_rich_row_too() {
    // Engine-wins, re-pinned on a row issue #73 could not carry: engine
    // reflects bare `string` (not catalog's `string|false`) and wins, marker-free.
    let src = "<?php\nfunction f(string $s): void { \\PHPStan\\dumpType(strstr($s, $s)); }\n";
    let mut engine = Engine::reflecting("strstr", general(Base::String));
    assert_eq!(dumps_with(src, &mut engine), vec!["dumped type: string".to_owned()]);
    let assigned =
        "<?php\nfunction f(string $s): void { $r = strstr($s, $s); \\PHPStan\\dumpType($r); }\n";
    let mut engine = Engine::reflecting("strstr", general(Base::String));
    assert_eq!(dumps_with(assigned, &mut engine), vec!["dumped type: string".to_owned()]);
}

#[test]
fn the_version_gate_still_guards_the_widened_rung() {
    // The gate's complement: a row whose type never moved across the supported
    // line answers for every target, including one that straddles a boundary.
    for (raw, floor, ceiling) in [
        ("^8.1", (8, 1), Some((8, u16::MAX))),
        (">=8.3", (8, 3), None),
        (">=8.1 <8.3", (8, 1), Some((8, 2))),
    ] {
        assert_eq!(
            dump_under_target(Some(require_target(raw, floor, ceiling))),
            "dumped type: string|false (asserted)",
            "the floor must answer under a declared target of {raw}",
        );
    }
    assert_eq!(dump_under_target(None), "dumped type: string|false (asserted)");
}

#[test]
fn a_project_function_shadows_the_floor() {
    // Same refusal the reflected-envelope rung makes: a project function of the
    // builtin's simple name shadows the call, so the project's own definition wins.
    let src = "<?php\nfunction str_repeat($a, $b) { return []; }\n\
               function f(string $s, int $n): void { \\PHPStan\\dumpType(str_repeat($s, $n)); }\n";
    assert_eq!(no_php_dumps(src), vec!["dumped type: unknown".to_owned()]);
}

#[test]
fn an_engine_answer_wins_over_the_floor() {
    // Engine reflects `int` for `str_repeat` (not catalog's `string`) — no
    // `(asserted)` marker since it's a native declaration off the engine's arginfo.
    let src = "<?php\nfunction f(string $s, int $n): void { \\PHPStan\\dumpType(str_repeat($s, $n)); }\n";
    let mut engine = Engine::reflecting("str_repeat", general(Base::Int));
    assert_eq!(dumps_with(src, &mut engine), vec!["dumped type: int".to_owned()]);
    let assigned = "<?php\nfunction f(string $s, int $n): void {\n\
                    $r = str_repeat($s, $n); \\PHPStan\\dumpType($r); }\n";
    let mut engine = Engine::reflecting("str_repeat", general(Base::Int));
    assert_eq!(dumps_with(assigned, &mut engine), vec!["dumped type: int".to_owned()]);
}

#[test]
fn the_floor_fills_the_engines_silence_per_name_not_per_run() {
    // ADR-0069 §2 as amended: the rung's condition is per NAME — an engine that
    // knows `str_repeat` but not `gmp_intval` leaves only that name to the floor.
    let src = "<?php\nfunction f(string $s, int $n, $g): void {\n\
               \\PHPStan\\dumpType(str_repeat($s, $n));\n\
               \\PHPStan\\dumpType(gmp_intval($g));\n}\n";
    let mut engine = Engine::reflecting("str_repeat", general(Base::String));
    assert_eq!(
        dumps_with(src, &mut engine),
        vec!["dumped type: string".to_owned(), "dumped type: int (asserted)".to_owned()],
    );
}

#[test]
fn the_floor_seeds_asserted_and_never_verified() {
    // The stratum bit (`Asserted`) — the premise the all-Verified proof rule consults.
    let src = "<?php\nfunction f(string $s, int $n): void { $r = str_repeat($s, $n); \\PHPStan\\dumpType($r); }\n";
    assert_eq!(no_php_dumps(src), vec!["dumped type: string (asserted)".to_owned()]);
    // Same fact from the engine is Verified instead — no marker (ADR-0069 §2).
    let mut engine = Engine::reflecting("str_repeat", general(Base::String));
    assert_eq!(dumps_with(src, &mut engine), vec!["dumped type: string".to_owned()]);
}

#[test]
fn a_floor_fact_premises_a_contract_finding_but_never_a_proof_one() {
    // The floor's `string` decides `phpdoc.return-mismatch` (CONTRACT-layer,
    // ADR-0069 §2) but must never reach the proof layer — asserted set-wide.
    let src = "<?php\n/** @return int */\nfunction f(string $s, int $n) { $r = str_repeat($s, $n); return $r; }\n";
    let tree = SourceTree::parse(src);
    let ds = check(&tree, &[], "t.php");
    assert!(
        ds.iter().any(|d| d.id == RETURN_MISMATCH_ID),
        "the floor fact must actually be consumed, or this test proves nothing: {ds:?}"
    );
    let proof: Vec<&Diagnostic> =
        ds.iter().filter(|d| layer(d.id) == Some(Layer::Proof)).collect();
    assert!(proof.is_empty(), "a floor fact reached the proof layer: {proof:?}");

    // The same envelope from a live engine is Verified — contract finding still
    // fires, proof layer silent for an unrelated reason. Belt and braces.
    let mut engine = Engine::reflecting("str_repeat", general(Base::String));
    let verified = run(src, &mut engine);
    assert!(
        verified.iter().any(|d| d.id == RETURN_MISMATCH_ID),
        "the engine's envelope must premise the same contract finding: {verified:?}"
    );
    assert!(
        verified.iter().all(|d| layer(d.id) != Some(Layer::Proof)),
        "no proof-layer id consumes an abstract return fact yet: {verified:?}"
    );
}

#[test]
fn a_rich_floor_row_never_premises_a_proof_finding() {
    // #79 extension: `string|false` is a stronger premise than an envelope (can
    // return `false`) — asserted across bound/guarded/subtracted/returned uses.
    let sources = [
        "<?php\nfunction f(string $s): void { $r = strstr($s, $s); \\PHPStan\\dumpType($r); }\n",
        // Guarded: the `!== false` subtraction is the arm lane doing real work.
        "<?php\nfunction f(string $s): void {\n\
         $r = strstr($s, $s);\n\
         if ($r !== false) { \\PHPStan\\dumpType($r); }\n}\n",
        // Used where a `false` would be a type error if anything trusted the row.
        "<?php\nfunction f(string $s): int { $r = strstr($s, $s); return strlen($r); }\n",
        // Returned against a declared contract the `false` arm violates.
        "<?php\n/** @return string */\nfunction f(string $s) { $r = strstr($s, $s); return $r; }\n",
        "<?php\nfunction f(string $s): void { $r = mb_strtoupper($s); \\PHPStan\\dumpType($r); }\n",
    ];
    for src in sources {
        let tree = SourceTree::parse(src);
        let ds = check(&tree, &[], "t.php");
        let proof: Vec<&Diagnostic> =
            ds.iter().filter(|d| layer(d.id) == Some(Layer::Proof)).collect();
        assert!(proof.is_empty(), "a rich floor row reached the proof layer in {src:?}: {proof:?}");
    }
}

#[test]
fn a_rich_floor_row_behaves_exactly_like_a_declared_one_under_guards() {
    // The floor row narrows like a written contract — same lane, same operators.
    // `!== false` now subtracts `false` too (ADR-0052 §2 `Value` subtrahend); the
    // surviving arm keeps `(asserted)`. `false_arm_strip.rs` owns the mechanism.
    let guarded = |decl: &str, param: &str, bind: &str, guard: &str| {
        let src = format!(
            "<?php\n{decl}function f({param}): void {{ {bind} if ({guard}) {{ \\PHPStan\\dumpType($r); }} }}\n"
        );
        no_php_dumps(&src).first().cloned().unwrap_or_default()
    };
    let floor = ("", "string $s", "$r = strstr($s, $s);");
    let written = ("/** @param string|false $r */\n", "$r", "");
    assert_eq!(guarded(floor.0, floor.1, floor.2, "is_string($r)"), "dumped type: string");
    assert_eq!(guarded(written.0, written.1, written.2, "is_string($r)"), "dumped type: string");
    assert_eq!(guarded(floor.0, floor.1, floor.2, "$r !== false"), "dumped type: string (asserted)");
    assert_eq!(
        guarded(written.0, written.1, written.2, "$r !== false"),
        "dumped type: string (asserted)"
    );
}

#[test]
fn the_absence_family_stays_silent_under_no_php() {
    // Existence is a boot-surface fact; with no engine there's none, so the
    // family is silent — the floor answers return types only.
    let src = "<?php\nfunction f($g): void { gmp_intval($g); typo_that_does_not_exist(); }\n";
    let tree = SourceTree::parse(src);
    let ds = check(&tree, &[], "t.php");
    assert!(
        ds.iter().all(|d| d.id != CALL_UNDEFINED_FUNCTION_ID),
        "the absence family must stay silent under the sound subset: {ds:?}"
    );
}

#[test]
fn an_absence_finding_and_a_floor_fact_are_complementary() {
    // ADR-0069 §2 as amended: with no gmp, the boot surface proves `gmp_intval`
    // absent while the floor still states its declared shape — both hold at once.
    let src = "<?php\nfunction f($g): void { $r = gmp_intval($g); \\PHPStan\\dumpType($r); }\n";
    let mut engine = Engine::default().with_boot_surface(&[]);
    let ds = run(src, &mut engine);
    assert!(
        ds.iter().any(|d| d.id == CALL_UNDEFINED_FUNCTION_ID),
        "the absence proof must still fire: {ds:?}"
    );
    let dumped: Vec<&str> =
        ds.iter().filter(|d| d.id == DEBUG_TYPE_ID).map(|d| d.message.as_str()).collect();
    assert_eq!(dumped, vec!["dumped type: int (asserted)"], "the floor still states the declared shape");
}

#[test]
fn a_floor_row_is_not_an_existence_vouch() {
    // `gmp_intval` has a floor row, `typo_nope` has neither; against a boot
    // surface reporting both absent, the family judges them identically.
    let covered = "<?php\nfunction f($g): void { gmp_intval($g); }\n";
    let uncovered = "<?php\nfunction f($g): void { typo_nope($g); }\n";
    let mut engine = Engine::default().with_boot_surface(&[]);
    let count = |src: &str, e: &mut Engine| {
        run(src, e).iter().filter(|d| d.id == CALL_UNDEFINED_FUNCTION_ID).count()
    };
    assert_eq!(count(covered, &mut engine), 1, "a floor row must not vouch for existence");
    assert_eq!(count(uncovered, &mut engine), 1);
}

#[test]
fn a_more_precise_rung_still_wins() {
    // The floor is the FLOOR: an engine consulted first wins. The absent marker
    // (despite the floor row also saying `bool`) proves the engine rung answered.
    let src = "<?php\nfunction f(string $s): void { \\PHPStan\\dumpType(array_key_exists($s, [])); }\n";
    let mut engine =
        Engine::reflecting("array_key_exists", general(Base::Bool)).and_reflecting("noop", general(Base::Int));
    assert_eq!(dumps_with(src, &mut engine), vec!["dumped type: bool".to_owned()]);
}

// ADR-0071: array-vocabulary rows (388-row bucket #73/#79 dropped) — a single
// arm reaches the value lane via `seed_shape_fact`, a multi-arm row lives in the arm lane.

/// The dumped type of `$r` (or an expression over it) after `bind`. Each probe
/// is its own snippet — an intervening unresolved call sweeps scope carriers.
fn after(bind: &str, expr: &str) -> String {
    let src = format!(
        "<?php\nfunction f(string $s, int $n): void {{ {bind} \\PHPStan\\dumpType({expr}); }}\n"
    );
    no_php_dumps(&src).first().cloned().unwrap_or_default()
}

#[test]
fn an_array_row_spells_through_the_one_speller_on_both_surfaces() {
    // One row per array spelling the table carries. `spell_arms` is the one
    // speller (ADR-0062 §6, D4), same renderer as a hand-written `@return array{…}`.
    assert_eq!(
        probe("imagecolorsforindex($n, $n)"),
        "dumped type: array{alpha: int<0, 127>, blue: int<0, 255>, green: int<0, 255>, red: int<0, 255>} (asserted)"
    );
    assert_eq!(probe("str_split($s)"), "dumped type: list<string> (asserted)");
    assert_eq!(probe("array_count_values([])"), "dumped type: array<int<1, max>> (asserted)");
    // The declared surface agrees, through the assignment rung (ADR-0069 parity).
    let phpdoc = "<?php\nfunction f(string $s): void { $r = str_split($s); \\PHPStan\\dumpPhpDocType($r); }\n";
    let tree = SourceTree::parse(phpdoc);
    let msgs: Vec<String> = check(&tree, &[], "t.php")
        .into_iter()
        .filter(|d| d.id == DEBUG_PHPDOC_TYPE_ID)
        .map(|d| d.message)
        .collect();
    assert_eq!(msgs, vec!["dumped phpdoc type: list<string> (asserted)".to_owned()]);
}

#[test]
fn a_single_array_arm_row_seeds_the_shape_lane_end_to_end() {
    // The value lane, not just the arm lane — visible only via a `Fact::Shape`
    // consumer (an offset read); comes back `Asserted` since the shape is.
    assert_eq!(after("$r = imagecolorsforindex($n, $n);", "$r['alpha']"), "dumped type: int<0, 127> (asserted)");
    assert_eq!(after("$r = str_split($s);", "$r[0]"), "dumped type: string (asserted)");
    // `array{…}` with no tail declares only those keys; undeclared reads unknown.
    assert_eq!(after("$r = imagecolorsforindex($n, $n);", "$r['nope']"), "dumped type: unknown");
}

#[test]
fn a_nullable_array_row_stays_in_the_arm_lane_alone() {
    // DESIGNED refusal: `floor_value_fact`'s nullability rule refuses a shape
    // ("array or null" is two states) — but arms still carry both to the surfaces.
    assert_eq!(
        probe("error_get_last()"),
        "dumped type: null|array{file: string, line: int, message: string, type: int} (asserted)"
    );
    // The decline costs exactly the shape-lane consumer (compare the non-nullable row above).
    assert_eq!(after("$r = error_get_last();", "$r['file']"), "dumped type: unknown");
}

#[test]
fn an_array_scalar_union_row_lives_in_the_arm_lane() {
    // `pathinfo` (`string|array`) was uncarriable through #79 (one arm is an
    // array); a genuine multi-arm row behaves like `string|false` — arm lane only.
    assert_eq!(probe("pathinfo($s)"), "dumped type: string|array (asserted)");
    assert_eq!(after("$r = pathinfo($s);", "$r"), "dumped type: string|array (asserted)");
    assert_eq!(after("$r = pathinfo($s);", "$r['dirname']"), "dumped type: unknown");
}

#[test]
fn an_engine_answer_wins_over_an_array_row_too() {
    // Engine reflects a bare `string` (not catalog's `list<string>`) and wins, marker-free.
    let src = "<?php\nfunction f(string $s): void { \\PHPStan\\dumpType(str_split($s)); }\n";
    let mut engine = Engine::reflecting("str_split", general(Base::String));
    assert_eq!(dumps_with(src, &mut engine), vec!["dumped type: string".to_owned()]);
}

#[test]
fn an_array_floor_row_never_premises_a_proof_finding() {
    // Richest premise the table carries: a shape row states key presence and
    // per-field types — exercised bound, read, iterated, returned.
    let sources = [
        "<?php\nfunction f(int $n): void { $r = imagecolorsforindex($n, $n); \\PHPStan\\dumpType($r); }\n",
        "<?php\nfunction f(int $n): void { $r = imagecolorsforindex($n, $n); echo $r['alpha']; }\n",
        "<?php\nfunction f(int $n): void { $r = imagecolorsforindex($n, $n); echo $r['nope']; }\n",
        "<?php\nfunction f(string $s): void { $r = str_split($s); foreach ($r as $p) { echo $p; } }\n",
        "<?php\nfunction f(string $s): void { $r = str_split($s); echo $r[99]; }\n",
        "<?php\n/** @return string */\nfunction f(string $s) { $r = str_split($s); return $r; }\n",
        "<?php\nfunction f(): void { $r = error_get_last(); echo $r['file']; }\n",
        "<?php\nfunction f(string $s): void { $r = pathinfo($s); echo $r; }\n",
    ];
    for src in sources {
        let tree = SourceTree::parse(src);
        let ds = check(&tree, &[], "t.php");
        let proof: Vec<&Diagnostic> =
            ds.iter().filter(|d| layer(d.id) == Some(Layer::Proof)).collect();
        assert!(proof.is_empty(), "an array floor row reached the proof layer in {src:?}: {proof:?}");
    }
}

#[test]
fn the_version_gate_declines_below_a_change_boundary() {
    // Predicted by ADR-0069's amendment, demanded by steins-catalog's tripwire.
    // Rule: "wholly at or above the boundary" — STRICTER than "does not
    // straddle", since the row states the type as of the pin.
    for (name, call, below, at) in [
        ("str_split", "str_split($s)", (8, 1), (8, 2)),
        ("gc_status", "gc_status()", (8, 2), (8, 3)),
        ("session_get_cookie_params", "session_get_cookie_params()", (8, 4), (8, 5)),
    ] {
        let raw_below = format!(">={}.{}", below.0, below.1);
        assert_eq!(
            dump_call_under_target(call, Some(require_target(&raw_below, below, None))),
            "dumped type: unknown",
            "{name} must decline below its change boundary",
        );
        let raw_at = format!(">={}.{}", at.0, at.1);
        let admitted = dump_call_under_target(call, Some(require_target(&raw_at, at, None)));
        // Spellings are long; this leg only owns that the gate ADMITS (exact spelling below).
        assert!(
            admitted.ends_with(" (asserted)"),
            "{name} must answer at or above its boundary, got {admitted}",
        );
    }
    // A straddling range declines for the same reason: part of it is wrong for.
    assert_eq!(
        dump_call_under_target(
            "str_split($s)",
            Some(require_target(">=8.1 <8.3", (8, 1), Some((8, 2))))
        ),
        "dumped type: unknown",
    );
    // An UNDECLARED target admits, by design: the row is Asserted anyway, and
    // its consumers tolerate that grade (ADR-0069 §3).
    assert_eq!(
        dump_call_under_target("str_split($s)", None),
        "dumped type: list<string> (asserted)",
    );
}

// The class rows — other half of the 620-row bucket. `subsumes_class` is
// reflexive (ADR-0071 §2.3); `ContractTy::Class` now reaches the floor at the TOP level.

#[test]
fn a_class_row_reaches_the_declared_surface_and_the_arm_lane() {
    // A mined single-class row, end to end from the catalog string. Both call
    // and bound forms carry the `(asserted)` marker.
    assert_eq!(probe("gmp_init($s)"), "dumped type: GMP (asserted)");
    assert_eq!(probe("date_diff($h, $h)"), "dumped type: DateInterval (asserted)");
    assert_eq!(after("$r = gmp_init($s);", "$r"), "dumped type: GMP (asserted)");
    let src = "<?php\nfunction f(string $s): void { \\PHPStan\\dumpPhpDocType(gmp_init($s)); }\n";
    let tree = SourceTree::parse(src);
    let phpdoc: Vec<String> = check(&tree, &[], "t.php")
        .into_iter()
        .filter(|d| d.id == DEBUG_PHPDOC_TYPE_ID)
        .map(|d| d.message)
        .collect();
    assert_eq!(phpdoc, vec!["dumped phpdoc type: GMP (asserted)".to_owned()]);
}

#[test]
fn a_class_row_renders_the_casing_php_src_declares() {
    // Formerly a named residual (rendered lowercased `gmp` for `GMP`):
    // `ContractTy::Class` case-folds for `class_eq`; a builtin display-name
    // table now holds php-src's casing. Display fidelity only.
    assert_eq!(probe("hash_init($s)"), "dumped type: HashContext (asserted)");
    assert_eq!(probe("xml_parser_create()"), "dumped type: XMLParser (asserted)");
    // A PROJECT class of the same name still wins — the catalog speaks only for
    // a name no project file declares (`class_absent`, issue #67 precedence).
    let src = "<?php\nclass Gmp {}\nfunction f(Gmp $g): void { \\PHPStan\\dumpType($g); }\n";
    let tree = SourceTree::parse(src);
    let dumps: Vec<String> = check(&tree, &[], "t.php")
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message)
        .collect();
    assert_eq!(dumps, vec!["dumped type: Gmp".to_owned()]);
}

#[test]
fn a_nullable_class_row_keeps_its_null_arm_and_subtracts_it_under_a_guard() {
    // `?Collator` is carriable per ARM (`Null`+`Class`) — no special-case mining/lowering.
    assert_eq!(probe("collator_create($s)"), "dumped type: null|Collator (asserted)");
    // `!== null` subtracts the null arm — wired, unlike scalar-literal
    // subtraction (see `a_rich_floor_row_behaves_exactly_like_a_declared_one_under_guards`).
    let src = "<?php\nfunction f(string $s): void {\n\
               $r = collator_create($s);\n\
               if ($r !== null) { \\PHPStan\\dumpType($r); }\n}\n";
    assert_eq!(
        no_php_dumps(src).first().cloned().unwrap_or_default(),
        "dumped type: Collator (asserted)"
    );
}

#[test]
fn a_class_row_seeds_no_value_fact_at_all() {
    // No object inhabitant in the value domain (ADR-0035/0038): class rows are
    // ARM-LANE ONLY, so none can reach the proof layer however exercised.
    let sources = [
        "<?php\nfunction f(string $s): void { $r = gmp_init($s); \\PHPStan\\dumpType($r); }\n",
        "<?php\nfunction f(string $s): void {\n\
         $r = collator_create($s);\n\
         if ($r !== null) { \\PHPStan\\dumpType($r); }\n}\n",
        // Used where the class would be a type error if anything trusted the row.
        "<?php\nfunction f(string $s): int { $r = gmp_init($s); return strlen($r); }\n",
        // Returned against a declared contract the class arm violates.
        "<?php\n/** @return string */\nfunction f(string $s) { $r = gmp_init($s); return $r; }\n",
        // The nullable pair, unguarded — the shape most likely to premise something.
        "<?php\nfunction f(string $s): void { $r = collator_create($s); \\PHPStan\\dumpType($r); }\n",
    ];
    for src in sources {
        let tree = SourceTree::parse(src);
        let ds = check(&tree, &[], "t.php");
        let proof: Vec<&Diagnostic> =
            ds.iter().filter(|d| layer(d.id) == Some(Layer::Proof)).collect();
        assert!(proof.is_empty(), "a class floor row reached the proof layer in {src:?}: {proof:?}");
    }
}

#[test]
fn a_class_row_mixed_with_a_non_class_arm_is_inert_on_the_dump_surface() {
    // `render_contract_arms` spells a PURE class/`null` arm list only, falling
    // to unknown rather than guessing — inert on the RENDERER, not dropped from the lane.
    assert_eq!(probe("simplexml_load_string($s)"), "dumped type: unknown");
    assert_eq!(probe("stream_bucket_new($h, $s)"), "dumped type: unknown");
    // `curl_init` wears PHPStan's `__benevolent<CurlHandle|false>`, expanded to
    // `CurlHandle|false` by the phpdoc parser before lowering.
    assert_eq!(probe("curl_init()"), "dumped type: unknown");
}

#[test]
fn a_class_row_raises_nothing_anywhere_it_is_consumed() {
    // Consumer audit, asserted not argued: only the dump renderer (cosmetic) and
    // `phpdoc.undefined-method` read a class arm's FQN — everything else is
    // FQN-agnostic or floors to `Maybe`/`None`, the FP-safe side of unknown builtins.
    let sources = [
        "<?php\nfunction f(string $s): void { $r = gmp_init($s); $r->nope(); }\n",
        "<?php\nfunction f(string $s): void {\n\
         $r = gmp_init($s);\n\
         if ($r instanceof \\Countable) { $r->count(); }\n}\n",
        "<?php\nfunction f(string $s): void {\n\
         $r = gmp_init($s);\n\
         if (!($r instanceof \\Countable)) { $r->nope(); }\n}\n",
        // Returned against a contract the class arm can't satisfy — VALUE lane is empty.
        "<?php\n/** @return string */\nfunction f(string $s) { $r = gmp_init($s); return $r; }\n",
        "<?php\nfunction g(string $x): void {}\n\
         function f(string $s): void { $r = gmp_init($s); g($r); }\n",
        "<?php\nfunction f(string $s): void { $r = collator_create($s); $r->nope(); }\n",
    ];
    for src in sources {
        let tree = SourceTree::parse(src);
        let ds = check(&tree, &[], "t.php");
        assert!(ds.is_empty(), "a class floor row raised a finding in {src:?}: {ds:?}");
    }
}

#[test]
fn a_class_floor_row_is_not_an_existence_vouch_either() {
    // Class rows don't change absence posture — naming `GMP` says nothing about
    // whether the ext is loaded. Same assertion, on a class row not a scalar.
    let covered = "<?php\nfunction f($g): void { gmp_init($g); }\n";
    let uncovered = "<?php\nfunction f($g): void { typo_nope($g); }\n";
    let mut engine = Engine::default().with_boot_surface(&[]);
    let count = |src: &str, e: &mut Engine| {
        run(src, e).iter().filter(|d| d.id == CALL_UNDEFINED_FUNCTION_ID).count()
    };
    assert_eq!(count(covered, &mut engine), 1, "a class floor row must not vouch for existence");
    assert_eq!(count(uncovered, &mut engine), 1);
}

#[test]
fn an_engine_answer_wins_over_a_class_row_too() {
    // The floor is the floor, for this vocabulary as for every other: where the
    // engine reflects, the engine's answer stands and the marker is absent.
    let src = "<?php\nfunction f(string $s): void { \\PHPStan\\dumpType(gmp_init($s)); }\n";
    let mut engine = Engine::reflecting("gmp_init", general(Base::Int));
    assert_eq!(dumps_with(src, &mut engine), vec!["dumped type: int".to_owned()]);
}
