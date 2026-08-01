//! ADR-0069 / issues #73, #79, ADR-0071 — the Asserted declared-return floor.
//!
//! The bottom rung of the return ladder: where the engine says nothing about a
//! builtin's return type, the catalog's mined declaration speaks instead, at the
//! `Asserted` stratum. Issue #79 widened what "the declaration" may be — a
//! `T|false` failure union or a scalar refinement as well as a bare envelope — by
//! seeding through the declared-return ARM lane instead of `envelope_fact`, and
//! ADR-0071 widened it again to the array vocabulary once the generation-time
//! countersign could decide an array pair at all, then to the CLASS vocabulary,
//! which needed no widening of the relation whatsoever — `subsumes_class` was
//! already reflexive, and reflexivity is the whole question a functionMap row
//! poses. The grade, the firewall and the per-name silence condition are unchanged
//! through all three, and the #73 pins below are the evidence for that.
//!
//! Everything worth pinning about it is a *boundary*:
//!
//! * it answers where the engine is silent — **per name**, not per run;
//! * the engine's answer wins wherever the engine has one;
//! * the fact it seeds is `Asserted`, which is the bit the proof layer's
//!   all-Verified premise rule reads;
//! * the absence family neither consumes it nor is suppressed by it.
//!
//! The observable for "which rung answered" is the dump surface's `(asserted)`
//! marker (ADR-0053 §2 / ADR-0052 §5): the reflected envelope is a native
//! declaration and carries none, the floor is a catalog claim and always carries
//! one. That single bit *is* the firewall, so the tests read it directly rather
//! than through prose.

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

// ---------------------------------------------------------------------------
// Mocks. There is no PHP in a unit test, so a live engine is a `Folder` that
// answers `builtin_return_fact` (and, for the absence legs, the boot surface).
// ---------------------------------------------------------------------------

/// An engine that reflects a return type for the names it was given and is silent
/// for every other name — the shape of a real PHP whose extension set does not
/// cover the whole call graph.
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
    fn fold(&mut self, _name: &str, _args: &[ArgValue]) -> Option<ArgValue> {
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

/// The `debug.type` message bodies a source produces under the sound subset
/// (`NoFold` — the `--no-php` posture), in source order.
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

/// The same on `strstr`, whose declared return type never moved across the
/// supported line — the version gate's complement.
fn dump_under_target(target: Option<PhpTarget>) -> String {
    dump_call_under_target("strstr($s, $s)", target)
}

// ---------------------------------------------------------------------------
// The acceptance criterion: `--no-php` gains declared types.
// ---------------------------------------------------------------------------

#[test]
fn str_repeat_of_variables_dumps_string_under_no_php() {
    // ADR-0069 §1's own worked example. Before the floor both of these dumped
    // `unknown`: the fold cannot reach a call with variable operands, and every
    // remaining rung is engine-gated. The `(asserted)` marker is not decoration —
    // it is the grade, and it is what tells a reader the catalog answered.
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
    // One row per envelope shape the table can carry, so a lowering regression in
    // the arm seeding shows up here rather than as a silent loss of rows.
    assert_eq!(probe("str_pad($s, $n)"), "dumped type: string (asserted)");
    assert_eq!(probe("curl_errno($h)"), "dumped type: int (asserted)");
    assert_eq!(probe("acos($n)"), "dumped type: float (asserted)");
    assert_eq!(probe("array_key_exists($s, [])"), "dumped type: bool (asserted)");
    // A `?T` row keeps its nullability — the floor states a type, not a base.
    assert_eq!(probe("curl_multi_getcontent($h)"), "dumped type: string|null (asserted)");
    // A name with no admitted row stays honestly unknown; the floor invents nothing.
    assert_eq!(probe("sodium_add($s, $s)"), "dumped type: unknown");
}

// ---------------------------------------------------------------------------
// Issue #79: the rows richer than an envelope.
// ---------------------------------------------------------------------------

#[test]
fn a_t_false_row_renders_as_the_contract_lane_spells_it() {
    // The slice's acceptance criterion. `strstr` is `string|false` in functionMap —
    // a row #73 counted and dropped, because `envelope_fact` had nowhere to put the
    // `false` arm. It now seeds the declared-return ARM lane, so the dump surface
    // spells the union the way `spell_arms` spells every other contract arm list,
    // and the `(asserted)` marker still says which rung answered.
    assert_eq!(probe("strstr($s, $s)"), "dumped type: string|false (asserted)");
    assert_eq!(probe("strrchr($s, $s)"), "dumped type: string|false (asserted)");
    assert_eq!(probe("file_get_contents($s)"), "dumped type: string|false (asserted)");
    // Three or more arms compose the same way — no special case for the pair.
    assert_eq!(probe("array_search($s, [])"), "dumped type: int|string|false (asserted)");
    // The other #79 bucket: a scalar refinement functionMap states and reflection
    // cannot. These are the rows `envelope_fact` would have flattened to their base.
    assert_eq!(probe("mb_strtoupper($s)"), "dumped type: uppercase-string (asserted)");
    assert_eq!(probe("preg_match($s, $s)"), "dumped type: 0|1|false (asserted)");
}

#[test]
fn a_t_false_row_survives_the_assignment_rung_and_the_declared_surface() {
    // The assignment seam seeds BOTH carriers, as the `@param` entry seeding does.
    // A multi-arm row has no single value-domain fact — the domain carries no scalar
    // union layer — so it lives in the contract-arm lane alone, and both dump
    // surfaces read it from there. The four forms must agree on the type: the two
    // argument-position forms (`dumpType(f(…))`, `dumpPhpDocType(f(…))`) and the two
    // assigned forms. Each is its own snippet because an intervening unresolved call
    // sweeps the scope's carriers — long-standing behavior, not this slice's.
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
    // Engine-wins, re-pinned on a row the #73 slice could not carry. The engine
    // reflects a bare `string` for `strstr` — deliberately NOT the catalog's
    // `string|false` — and its answer stands, marker-free.
    let src = "<?php\nfunction f(string $s): void { \\PHPStan\\dumpType(strstr($s, $s)); }\n";
    let mut engine = Engine::reflecting("strstr", general(Base::String));
    assert_eq!(dumps_with(src, &mut engine), vec!["dumped type: string".to_owned()]);
    // And through the assignment rung, which is a separate call site.
    let assigned =
        "<?php\nfunction f(string $s): void { $r = strstr($s, $s); \\PHPStan\\dumpType($r); }\n";
    let mut engine = Engine::reflecting("strstr", general(Base::String));
    assert_eq!(dumps_with(assigned, &mut engine), vec!["dumped type: string".to_owned()]);
}

#[test]
fn the_version_gate_still_guards_the_widened_rung() {
    // The complement of the gate, and the half that was observable before ADR-0071:
    // a rich row whose declared return type never moved across the supported line
    // answers for every target, including one that straddles a minor boundary. A
    // gate that over-fired would silence the whole table on any ranged target.
    // (The declining half is `the_version_gate_declines_below_a_change_boundary`.)
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
    // And with no declared target at all, which the gate admits by design.
    assert_eq!(dump_under_target(None), "dumped type: string|false (asserted)");
}

#[test]
fn a_project_function_shadows_the_floor() {
    // Same refusal the reflected-envelope rung makes: a project function of the
    // builtin's simple name shadows (or makes ambiguous) the call, and the
    // project's own definition is the better answer than a catalog row.
    let src = "<?php\nfunction str_repeat($a, $b) { return []; }\n\
               function f(string $s, int $n): void { \\PHPStan\\dumpType(str_repeat($s, $n)); }\n";
    assert_eq!(no_php_dumps(src), vec!["dumped type: unknown".to_owned()]);
}

// ---------------------------------------------------------------------------
// Which rung answered: the engine wins wherever it speaks.
// ---------------------------------------------------------------------------

#[test]
fn an_engine_answer_wins_over_the_floor() {
    // The engine reflects `int` for `str_repeat` — deliberately NOT the catalog's
    // `string`, so the rendered type alone says which rung answered. The engine's
    // answer stands, and it carries no `(asserted)` marker: it is a native
    // declaration read off the running engine's own arginfo.
    let src = "<?php\nfunction f(string $s, int $n): void { \\PHPStan\\dumpType(str_repeat($s, $n)); }\n";
    let mut engine = Engine::reflecting("str_repeat", general(Base::Int));
    assert_eq!(dumps_with(src, &mut engine), vec!["dumped type: int".to_owned()]);
    // And through the assignment rung, which is a separate call site.
    let assigned = "<?php\nfunction f(string $s, int $n): void {\n\
                    $r = str_repeat($s, $n); \\PHPStan\\dumpType($r); }\n";
    let mut engine = Engine::reflecting("str_repeat", general(Base::Int));
    assert_eq!(dumps_with(assigned, &mut engine), vec!["dumped type: int".to_owned()]);
}

#[test]
fn the_floor_fills_the_engines_silence_per_name_not_per_run() {
    // ADR-0069 §2 as amended: the rung's condition is per NAME. A live engine that
    // reflects `str_repeat` but knows nothing of `gmp_intval` (no gmp extension on
    // the analyzing PHP) leaves exactly that one name to the floor — and the marker
    // separates the two answers in the same run.
    let src = "<?php\nfunction f(string $s, int $n, $g): void {\n\
               \\PHPStan\\dumpType(str_repeat($s, $n));\n\
               \\PHPStan\\dumpType(gmp_intval($g));\n}\n";
    let mut engine = Engine::reflecting("str_repeat", general(Base::String));
    assert_eq!(
        dumps_with(src, &mut engine),
        vec!["dumped type: string".to_owned(), "dumped type: int (asserted)".to_owned()],
    );
}

// ---------------------------------------------------------------------------
// The proof-layer firewall.
// ---------------------------------------------------------------------------

#[test]
fn the_floor_seeds_asserted_and_never_verified() {
    // The stratum bit itself, read straight off the binding through the dump
    // surface (which renders `known.stratum == Asserted`). This is the premise the
    // proof layer's all-Verified rule consults, so pinning it pins the firewall at
    // its mechanism rather than at one downstream symptom.
    let src = "<?php\nfunction f(string $s, int $n): void { $r = str_repeat($s, $n); \\PHPStan\\dumpType($r); }\n";
    assert_eq!(no_php_dumps(src), vec!["dumped type: string (asserted)".to_owned()]);
    // The identical fact from the engine is Verified — no marker. Same fact, same
    // rendering, different grade: one lowering, two provenances (ADR-0069 §2).
    let mut engine = Engine::reflecting("str_repeat", general(Base::String));
    assert_eq!(dumps_with(src, &mut engine), vec!["dumped type: string".to_owned()]);
}

#[test]
fn a_floor_fact_premises_a_contract_finding_but_never_a_proof_one() {
    // The floor's `string` is a real premise: it decides `phpdoc.return-mismatch`,
    // a CONTRACT-layer finding, which is exactly the "contracts-tier reasoning"
    // ADR-0069 §2 admits. What must never happen is the same premise reaching the
    // proof layer, and the assertion is made over the whole diagnostic set rather
    // than one id — a future proof-layer consumer of abstract facts would break
    // this test, which is the point of writing it this way.
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

    // The same snippet with the SAME envelope from a live engine — Verified, the
    // grade the floor is denied. The contract finding still fires (the fact is the
    // same set of values either way), and the proof layer is silent for a reason
    // that has nothing to do with the floor: no proof-layer id consumes an abstract
    // fact today. The firewall is therefore belt AND braces, and this pin records
    // which brace is which.
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
    // The #79 extension of the pin above, and the regression this slice must not
    // introduce. A `string|false` row is a strictly stronger premise than an
    // envelope — it says a call *can* return `false`, which is exactly the shape a
    // proof-layer consumer would want to reason from — so the firewall is asserted
    // against the whole diagnostic set on sources that exercise the arm lane in
    // every direction it can be exercised: bound, guarded, subtracted, returned.
    let sources = [
        // Bound, then dumped.
        "<?php\nfunction f(string $s): void { $r = strstr($s, $s); \\PHPStan\\dumpType($r); }\n",
        // Guarded: the `!== false` subtraction is the arm lane doing real work.
        "<?php\nfunction f(string $s): void {\n\
         $r = strstr($s, $s);\n\
         if ($r !== false) { \\PHPStan\\dumpType($r); }\n}\n",
        // Used where a `false` would be a type error if anything trusted the row.
        "<?php\nfunction f(string $s): int { $r = strstr($s, $s); return strlen($r); }\n",
        // Returned against a declared contract the `false` arm violates.
        "<?php\n/** @return string */\nfunction f(string $s) { $r = strstr($s, $s); return $r; }\n",
        // The refinement bucket, same question.
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
    // The floor row is a declared contract and nothing more, so it must narrow the
    // way a written one does — the same lane, the same operators, no bespoke
    // behavior for the catalog's rows.
    //
    // A runtime type predicate narrows it (`is_string` is checked on the branch, so
    // the branch fact is Verified and carries no marker — the floor is not the
    // premise there, the guard is). A `!== false` comparison now subtracts the
    // `false` arm too: the ADR-0052 §2 `Value` subtrahend is wired, so the arm lane
    // narrows on identity guards as well as on `instanceof` and `!== null`. The
    // grade survives it — the surviving arm is still the catalog's claim, so the
    // `(asserted)` marker stays — and the hand-written `@param string|false` pair
    // below is the whole point: the floor row narrows the way a written row does,
    // with no bespoke behavior for the catalog. `false_arm_strip.rs` owns the
    // mechanism; these two assertions are the floor/hand-written parity pins.
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

// ---------------------------------------------------------------------------
// The absence family: not a consumer, and not suppressed.
// ---------------------------------------------------------------------------

#[test]
fn the_absence_family_stays_silent_under_no_php() {
    // Existence is a boot-surface fact. Without an engine there is no boot surface,
    // so the family is silent — unchanged by the floor, which answers only about
    // return types and never about whether a name exists.
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
    // ADR-0069 §2 as amended. A live engine without gmp: the boot surface proves
    // `gmp_intval` absent (a real finding — the call fails on the analyzing PHP),
    // and the floor still states the shape it declares where it DOES exist. Both
    // are true at once; neither suppresses the other, and the floor must never be
    // read as an existence answer in either direction.
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
    // The other direction of the same rule. `gmp_intval` carries a floor row and no
    // effect row; `typo_nope` carries neither. Against a boot surface that reports
    // both absent, the family judges them identically — coverage by this table buys
    // a name exactly nothing on the existence question.
    let covered = "<?php\nfunction f($g): void { gmp_intval($g); }\n";
    let uncovered = "<?php\nfunction f($g): void { typo_nope($g); }\n";
    let mut engine = Engine::default().with_boot_surface(&[]);
    let count = |src: &str, e: &mut Engine| {
        run(src, e).iter().filter(|d| d.id == CALL_UNDEFINED_FUNCTION_ID).count()
    };
    assert_eq!(count(covered, &mut engine), 1, "a floor row must not vouch for existence");
    assert_eq!(count(uncovered, &mut engine), 1);
}

// ---------------------------------------------------------------------------
// Precedence against the rungs above.
// ---------------------------------------------------------------------------

#[test]
fn a_more_precise_rung_still_wins() {
    // The floor is the FLOOR: an engine that folds the call to a literal, or that
    // reflects an envelope, is consulted first and its answer stands. Here the
    // engine reflects `bool` for `array_key_exists` (whose floor row also says
    // `bool`), and the absent marker proves the engine rung answered.
    let src = "<?php\nfunction f(string $s): void { \\PHPStan\\dumpType(array_key_exists($s, [])); }\n";
    let mut engine =
        Engine::reflecting("array_key_exists", general(Base::Bool)).and_reflecting("noop", general(Base::Int));
    assert_eq!(dumps_with(src, &mut engine), vec!["dumped type: bool".to_owned()]);
}

// ---------------------------------------------------------------------------
// ADR-0071: the array-vocabulary rows.
//
// The 388-row bucket #73 and #79 both counted and dropped. Nothing about the
// consuming side is new — a single array arm has always reached the value lane
// through `seed_shape_fact`, and a multi-arm row has always lived in the arm lane
// — so these fixtures pin that a builtin row travels those seams exactly as a
// project function's `@return array{…}` does, and that the seeded lane is the one
// the arms denote rather than whichever happens to render the same text.
// ---------------------------------------------------------------------------

/// The dumped type of `$r` (or of an expression over it) after `bind`, under the
/// sound subset. Each probe is its own snippet: an intervening unresolved call
/// sweeps the scope's carriers, so two dumps in one body would not both read the
/// binding — long-standing behavior, not this slice's.
fn after(bind: &str, expr: &str) -> String {
    let src = format!(
        "<?php\nfunction f(string $s, int $n): void {{ {bind} \\PHPStan\\dumpType({expr}); }}\n"
    );
    no_php_dumps(&src).first().cloned().unwrap_or_default()
}

#[test]
fn an_array_row_spells_through_the_one_speller_on_both_surfaces() {
    // A shape, a list and a map — one row per array spelling the table now carries,
    // so a lowering or spelling regression shows up here rather than as a silent
    // loss. `spell_arms` is the one speller (ADR-0062 §6, D4): the same renderer
    // that spells a hand-written `@return array{…}`, reached from a catalog string.
    assert_eq!(
        probe("imagecolorsforindex($n, $n)"),
        "dumped type: array{alpha: int<0, 127>, blue: int<0, 255>, green: int<0, 255>, red: int<0, 255>} (asserted)"
    );
    assert_eq!(probe("str_split($s)"), "dumped type: list<string> (asserted)");
    assert_eq!(probe("array_count_values([])"), "dumped type: array<positive-int> (asserted)");
    // The declared-side surface agrees, through the assignment rung — the parity
    // claim ADR-0069 makes for every row shape.
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
    // The value lane, not just the arm lane — and the difference is only visible
    // through a consumer that reads a `Fact::Shape`. An offset read is that
    // consumer: the field's own declared type comes back, `Asserted` because the
    // shape it was read from is.
    assert_eq!(after("$r = imagecolorsforindex($n, $n);", "$r['alpha']"), "dumped type: int<0, 127> (asserted)");
    assert_eq!(after("$r = str_split($s);", "$r[0]"), "dumped type: string (asserted)");
    // The seal is the row's own: `array{…}` with no tail declares those keys and no
    // others, so an undeclared key reads honestly unknown rather than `mixed`.
    assert_eq!(after("$r = imagecolorsforindex($n, $n);", "$r['nope']"), "dumped type: unknown");
}

#[test]
fn a_nullable_array_row_stays_in_the_arm_lane_alone() {
    // A DESIGNED refusal, not a gap. `floor_value_fact` states one nullability rule
    // — `fact_with_null` — and that rule refuses a shape: the value domain carries
    // `nullable` on a shape as a side flag, but the floor declines to be the thing
    // that sets it, because a row saying "an array or null" denotes two states and
    // ADR-0069's value-lane rule seeds only where the arms denote one.
    //
    // Nothing is lost to the surfaces: the arms still carry both the shape and the
    // null, and both dump surfaces read them.
    assert_eq!(
        probe("error_get_last()"),
        "dumped type: null|array{file: string, line: int, message: string, type: int} (asserted)"
    );
    // What the decline costs is exactly the shape-lane consumer — compare with the
    // non-nullable row above, where the same read answers.
    assert_eq!(after("$r = error_get_last();", "$r['file']"), "dumped type: unknown");
}

#[test]
fn an_array_scalar_union_row_lives_in_the_arm_lane() {
    // `pathinfo` is `string|array` in functionMap: the row was uncarriable through
    // #79 only because ONE of its two arms was an array. It is a genuinely multi-arm
    // row, so it behaves exactly as `string|false` does — the arm lane holds it, the
    // one speller spells it, and no value lane is seeded because the arms denote no
    // single fact.
    assert_eq!(probe("pathinfo($s)"), "dumped type: string|array (asserted)");
    assert_eq!(after("$r = pathinfo($s);", "$r"), "dumped type: string|array (asserted)");
    assert_eq!(after("$r = pathinfo($s);", "$r['dirname']"), "dumped type: unknown");
}

#[test]
fn an_engine_answer_wins_over_an_array_row_too() {
    // Engine-wins, re-pinned one vocabulary over. The engine reflects a bare
    // `string` for `str_split` — deliberately not the catalog's `list<string>` —
    // and its answer stands, marker-free.
    let src = "<?php\nfunction f(string $s): void { \\PHPStan\\dumpType(str_split($s)); }\n";
    let mut engine = Engine::reflecting("str_split", general(Base::String));
    assert_eq!(dumps_with(src, &mut engine), vec!["dumped type: string".to_owned()]);
}

#[test]
fn an_array_floor_row_never_premises_a_proof_finding() {
    // The firewall on the richest premise the table now carries. A shape row states
    // key presence and per-field types — strictly more than any envelope — so it is
    // the row most worth proving the all-Verified premise rule against, over every
    // direction the shape lane can be exercised: bound, read, iterated, returned.
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
    // The end-to-end fixture ADR-0069's amendment predicted and steins-catalog's
    // tripwire demanded. Every version-sensitive name at this pin returns an array,
    // so the two mined tables were disjoint through #73 and #79 and this gate had
    // nothing to decide; ADR-0071 admits all four, and the gate is live.
    //
    // The rule is "wholly at or above the boundary", which is STRICTER than "does
    // not straddle": a target entirely below the boundary does not straddle it
    // either, and the row states the type as of the pin, so it would be as wrong
    // there as in the straddling case.
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
        // The spellings of the two shape rows are long enough to be their own
        // maintenance burden; what this leg owns is that the gate ADMITS, and the
        // marker is what proves the floor rather than another rung answered.
        // `str_split`'s exact spelling is pinned by the undeclared-target leg below.
        assert!(
            admitted.ends_with(" (asserted)"),
            "{name} must answer at or above its boundary, got {admitted}",
        );
    }
    // A ranged target that STRADDLES the boundary declines for the same reason the
    // wholly-below one does — part of the range is a PHP the row is wrong for.
    assert_eq!(
        dump_call_under_target(
            "str_split($s)",
            Some(require_target(">=8.1 <8.3", (8, 1), Some((8, 2))))
        ),
        "dumped type: unknown",
    );
    // And an UNDECLARED target admits, by design: the row is Asserted anyway, and
    // its consumers tolerate that grade (ADR-0069 §3).
    assert_eq!(
        dump_call_under_target("str_split($s)", None),
        "dumped type: list<string> (asserted)",
    );
}

// ---------------------------------------------------------------------------
// The object slice: the class rows.
//
// The other half of the 620-row bucket, and the one that needed no new relation
// at all — `subsumes_class` is reflexive, so a row naming the class the engine
// names countersigns on that alone (ADR-0071 §2.3). What is new HERE is that a
// `ContractTy::Class` arm can now reach the consuming floor at the TOP level,
// where before it could only appear inside an array row's element type. These
// fixtures pin the two things that follow: the arm lane carries it, and the value
// lane stays empty.
// ---------------------------------------------------------------------------

#[test]
fn a_class_row_reaches_the_declared_surface_and_the_arm_lane() {
    // A mined single-class row, end to end from the catalog string. Both the call
    // form and the bound form reach it, and both carry the `(asserted)` marker —
    // the floor answered, and it says so.
    assert_eq!(probe("gmp_init($s)"), "dumped type: GMP (asserted)");
    assert_eq!(probe("date_diff($h, $h)"), "dumped type: DateInterval (asserted)");
    assert_eq!(after("$r = gmp_init($s);", "$r"), "dumped type: GMP (asserted)");
    // The declared surface agrees with the value surface, the parity every rung
    // above this one also keeps.
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
    // Formerly the third-amendment NAMED RESIDUAL (a class row rendered
    // lowercased, `gmp` where PHPStan says `GMP`): `ContractTy::Class` case-folds
    // on the way in — that is what makes the countersign's `class_eq` comparison
    // work — and the project index knows nothing about a builtin. The builtin
    // display-name table beside the hierarchy catalog now holds the casing
    // php-src declares, mined from the same pin, and `Cx::class_display_fqn`
    // consults it exactly where the project index misses. Display fidelity only:
    // every judgment downstream still compares through the case-insensitive
    // `class_eq`, so nothing decides differently because of it.
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
    // `?Collator` is carriable per ARM — a `Null` beside a `Class` — so it needed no
    // case of its own in either the mining filter or the lowering.
    assert_eq!(probe("collator_create($s)"), "dumped type: null|Collator (asserted)");
    // And the arm lane does real work on it: `!== null` subtracts the null arm.
    // This is the leg that a `!== false` guard does NOT have (see
    // `a_rich_floor_row_behaves_exactly_like_a_declared_one_under_guards`) — null
    // subtraction is wired, scalar-literal subtraction is not — so a class row is
    // strictly better served by the existing operators than a `T|false` one.
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
    // The value domain has no object inhabitant (ADR-0035/0038), so a class row is
    // ARM-LANE ONLY — not "a fact this declines to seed" but "no fact exists to
    // seed". Both lowerings say so independently: `contractty_to_fact` has no arm
    // for `Class`, and `to_shape_fact` has none either, so `floor_value_fact`
    // returns `None` by two routes.
    //
    // The consequence is the one worth pinning: with nothing in the value lane there
    // is no premise anywhere, so no class row can reach the proof layer no matter
    // how it is exercised. Asserted grade would have kept it out anyway; this is the
    // stronger statement, that there is nothing there to keep out.
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
    // `render_contract_arms`' class path spells a PURE class/`null` arm list and
    // refuses anything else, so a `Class|false` row and a bare `object` row have no
    // faithful spelling and the surface falls to honest unknown rather than guessing.
    //
    // Inert on the RENDERER, not dropped from the lane: these rows countersigned and
    // they seed their arms exactly as the rows above do. The same posture the array
    // slice recorded for the two rows `spell_arms` refuses to spell back.
    assert_eq!(probe("simplexml_load_string($s)"), "dumped type: unknown");
    assert_eq!(probe("stream_bucket_new($h, $s)"), "dumped type: unknown");
    // `curl_init` is the same shape wearing PHPStan's own spelling of a plain union:
    // the catalog stores `__benevolent<CurlHandle|false>`, which the phpdoc parser
    // expands to `CurlHandle|false` before it is ever lowered.
    assert_eq!(probe("curl_init()"), "dumped type: unknown");
}

#[test]
fn a_class_row_raises_nothing_anywhere_it_is_consumed() {
    // The consumer audit for this slice, asserted rather than argued. Only two
    // places read the FQN out of a class arm: the dump renderer (cosmetic) and
    // `phpdoc.undefined-method`. Everything else — instanceof subtraction through
    // `subtrahend_covers`, the runtime-predicate and shape subtractors, the argument
    // check's `to_fact` — is FQN-agnostic or floors to `Maybe`/`None`.
    //
    // The FQNs a floor row carries are builtins the PROJECT index does not know, and
    // that is the FP-safe side of every one of those paths: `is_final` is
    // project-only so the positive instanceof branch keeps the arm, `is_a` answers
    // `No`/`Maybe` so the negative branch keeps it, and the method chain hits
    // `find_class` → silent before it can emit. So the whole-diagnostic-set
    // assertion is the honest pin, not just the proof-layer one.
    let sources = [
        // The method-existence family's shape: a class arm with a method call on it.
        "<?php\nfunction f(string $s): void { $r = gmp_init($s); $r->nope(); }\n",
        // instanceof, both polarities, against a class the project does not declare.
        "<?php\nfunction f(string $s): void {\n\
         $r = gmp_init($s);\n\
         if ($r instanceof \\Countable) { $r->count(); }\n}\n",
        "<?php\nfunction f(string $s): void {\n\
         $r = gmp_init($s);\n\
         if (!($r instanceof \\Countable)) { $r->nope(); }\n}\n",
        // Returned against a declared contract the class arm cannot satisfy. The
        // contracts tier reads the VALUE lane, which a class row leaves empty, so
        // there is nothing to mismatch against.
        "<?php\n/** @return string */\nfunction f(string $s) { $r = gmp_init($s); return $r; }\n",
        // Passed where a scalar is declared, the argument-side twin.
        "<?php\nfunction g(string $x): void {}\n\
         function f(string $s): void { $r = gmp_init($s); g($r); }\n",
        // The nullable pair used without a guard.
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
    // The absence family's posture is UNCHANGED by this slice, and the class rows
    // are the population most likely to tempt a reader into thinking otherwise — a
    // row naming `GMP` still says nothing about whether the ext is loaded, because
    // existence is a boot-surface fact and this table answers only about return
    // types. Same assertion as `a_floor_row_is_not_an_existence_vouch`, on a row
    // whose declaration is a class rather than a scalar.
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

