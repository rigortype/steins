//! Issue #74 — the **member-wise union fold**: an allowlisted builtin whose
//! argument is a bounded union of constants is folded once per member
//! combination, and the answers are composed into one value fact.
//!
//! ADR-0069's amendment names the gap this closes: PHPStan's extension stack can
//! fold a union of constants member-wise and compose; Steins' fold lane could
//! not (`$x = $c ? 'a' : 'b'; strtoupper($x)` widened here where PHPStan answers
//! `'A'|'B'`).
//!
//! Four disciplines pinned here, not just the spellings:
//! * **No partial unions.** Busting the per-argument cap (4) or combination cap
//!   (16) DECLINES; it never truncates.
//! * **Shared gates.** Every combination re-enters `try_fold` (allowlist,
//!   shadowing, budget, memo, issue-#64 width gate); a refused member declines
//!   the WHOLE fold.
//! * **Stratum (ADR-0048 N2).** Member answers are engine-`Verified`, but the
//!   composed fact takes the input union's own trust.
//! * **Zero emission.** A union fold is a *type*; the one finding pinned below
//!   is the ordinary proof-layer consequence of members that all agreed.

use std::cell::RefCell;
use std::rc::Rc;

use steins_infer::{
    DEBUG_TYPE_ID, Diagnostic, EngineFolder, FoldEngine, Folder, ID, SidecarFolder, check_with,
};
use steins_sidecar::{EnvInfo, FoldArg, FoldResult, FoldValue, PregCompile, Reflection};
use steins_syntax::{ArgValue, SourceTree};

// Foldable-union source shapes

/// `$x = $c ? <a> : <b>`: a two-member `OneOf` at `Verified` (ADR-0031).
fn ternary(then_val: &str, else_val: &str, expr: &str) -> String {
    format!(
        "<?php\nfunction f(bool $c): void {{ $x = $c ? {then_val} : {else_val}; \\PHPStan\\dumpType({expr}); }}\n"
    )
}

/// A branch merge: `$v` starts at `members[0]`, each `if` re-assigns it, and the
/// join is an `n`-member `Verified` `OneOf`.
fn merged(var: &str, members: &[&str]) -> String {
    let mut out = format!("${var} = {}; ", members[0]);
    for (i, m) in members.iter().enumerate().skip(1) {
        out.push_str(&format!("if (${var}_n === {i}) {{ ${var} = {m}; }} "));
    }
    out
}

/// A whole function body built from branch-merged unions, dumping `expr`.
fn merge_src(lanes: &[(&str, &[&str])], expr: &str) -> String {
    let params: Vec<String> = lanes.iter().map(|(v, _)| format!("int ${v}_n")).collect();
    let body: String = lanes.iter().map(|(v, ms)| merged(v, ms)).collect();
    format!(
        "<?php\nfunction f({}): void {{ {body}\\PHPStan\\dumpType({expr}); }}\n",
        params.join(", ")
    )
}

/// `$s = $v['k']`: the slot's declared fact, source of an `Asserted` union
/// (ADR-0052 §9 / ADR-0062 §4).
fn shape_read(slot: &str, expr: &str) -> String {
    format!(
        "<?php\n/** @param array{{k: {slot}}} $v */\nfunction f(array $v): void {{ $s = $v['k']; \\PHPStan\\dumpType({expr}); }}\n"
    )
}

// Folders

/// One call the gate forwarded to the folder.
type Ask = (String, Vec<ArgValue>);

/// A deterministic stand-in for the engine, recording every `(name, args)` the
/// gate handed it, so a decline is distinguishable from a full dispatch by count.
#[derive(Clone, Default)]
struct Mock(Rc<RefCell<Vec<Ask>>>);

impl Mock {
    fn asks(&self) -> Vec<Ask> {
        self.0.borrow().clone()
    }

    fn count(&self) -> usize {
        self.0.borrow().len()
    }
}

impl Folder for Mock {
    fn fold(&mut self, name: &str, args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
        self.0.borrow_mut().push((name.to_owned(), args.to_vec()));
        match (name, args) {
            ("strtoupper", [ArgValue::Str(s)]) => Some(ArgValue::Str(s.as_str()?.to_uppercase().into())),
            ("str_repeat", [ArgValue::Str(s), ArgValue::Int(n)]) => {
                Some(ArgValue::Str(s.as_str()?.repeat(usize::try_from(*n).ok()?).into()))
            }
            ("str_replace", [ArgValue::Str(a), ArgValue::Str(b), ArgValue::Str(c)]) => {
                Some(ArgValue::Str(c.as_str()?.replace(a.as_str()?, b.as_str()?).into()))
            }
            _ => None,
        }
    }
}

/// A [`FoldEngine`] at the integer width the test chooses, driving the SHARED
/// policy in `EngineFolder` so the width gate under test is the real one.
struct Fake {
    int_size: Option<u32>,
    dispatched: Vec<(String, Vec<FoldArg>)>,
}

impl Fake {
    fn at_width(int_size: u32) -> Fake {
        Fake { int_size: Some(int_size), dispatched: Vec::new() }
    }
}

impl FoldEngine for Fake {
    fn env(&mut self) -> Option<EnvInfo> {
        Some(EnvInfo {
            php_version: "8.5.8".to_owned(),
            extensions: vec!["Core".to_owned()],
            sapi: "cli".to_owned(),
            int_size: self.int_size,
        })
    }

    fn reflect(&mut self, _target: &str) -> Option<Reflection> {
        None
    }

    fn fold(&mut self, name: &str, args: &[FoldArg], _strict: bool) -> FoldResult {
        self.dispatched.push((name.to_owned(), args.to_vec()));
        match (name, args) {
            ("strval", [FoldArg::Int(i)]) => FoldResult::Value(FoldValue::Str(i.to_string())),
            _ => FoldResult::widen("unmodeled by the fake"),
        }
    }

    /// No PCRE (ADR-0078): keeps this file's subject the fold-width lane.
    fn preg_compile(&mut self, _pattern: &str) -> Option<PregCompile> {
        None
    }
    /// No constant oracle either (issue #198): this file's subject is integer width.
    fn constant_defined(&mut self, _name: &str) -> Option<steins_sidecar::ConstantDefined> {
        None
    }
    /// No class world either (issue #269): same reason.
    fn reflect_class(&mut self, _target: &str) -> Option<steins_sidecar::ClassReflection> {
        None
    }
}

/// A live sidecar folder, or `None` when `php` is unreachable (caller skips loudly).
fn live(test: &str) -> Option<SidecarFolder> {
    let mut folder = SidecarFolder::enabled();
    if folder.fold("strtoupper", &[ArgValue::Str("probe".into())], true).is_none() {
        eprintln!("SKIP {test}: no folding engine — is `php` on PATH?");
        return None;
    }
    Some(folder)
}

// Harness

fn diagnostics(src: &str, folder: &mut dyn Folder) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check_with(&tree, &functions, "test.php", folder)
}

/// The `dumpType` bodies for `src`; asserts the fold emitted no finding of its own.
fn dumps(src: &str, folder: &mut dyn Folder) -> Vec<String> {
    let ds = diagnostics(src, folder);
    let other: Vec<&Diagnostic> = ds.iter().filter(|d| !d.id.starts_with("debug.")).collect();
    assert!(other.is_empty(), "a union fold emitted a finding: {other:?}");
    ds.iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message.replace("dumped type: ", ""))
        .collect()
}

/// The single dump body of a one-dump fixture.
fn dump(src: &str, folder: &mut dyn Folder) -> String {
    let ds = dumps(src, folder);
    assert_eq!(ds.len(), 1, "expected exactly one dump, got {ds:?}");
    ds[0].clone()
}

// (1) The flagship, through the real engine

/// ADR-0069's named gap, closed: a union of constants reaches the real engine
/// member by member and comes back composed, across assignment, all-agree
/// (Singleton), and a plain literal composed alongside it — `strtoupper`'s
/// answers are the engine's own.
#[test]
fn the_flagship_union_folds_through_the_real_engine() {
    let Some(mut folder) = live("the_flagship_union_folds_through_the_real_engine") else {
        return;
    };
    assert_eq!(dump(&ternary("'a'", "'b'", "strtoupper($x)"), &mut folder), "'A'|'B'");
    // Assignment form: the value survives a hop.
    let assigned = "<?php\nfunction f(bool $c): void { $x = $c ? 'a' : 'b'; $y = strtoupper($x); \\PHPStan\\dumpType($y); }\n";
    assert_eq!(dump(assigned, &mut folder), "'A'|'B'");
    // All members fold to the SAME value: composes to a Singleton, not a union of one.
    assert_eq!(dump(&ternary("'a'", "'A'", "strtoupper($x)"), &mut folder), "'A'");
    // Literal and union lanes compose in the product.
    assert_eq!(dump(&ternary("'a'", "'b'", "str_repeat($x, 2)"), &mut folder), "'aa'|'bb'");
}

/// The rung ORDER: the union fold outranks issue #77's string-predicate
/// transfer, and past the fold's cap the ladder DEGRADES to that transfer
/// rather than falling to the floor.
#[test]
fn the_union_fold_outranks_the_predicate_transfer() {
    let Some(mut folder) = live("the_union_fold_outranks_the_predicate_transfer") else { return };
    assert_eq!(dump(&ternary("'a'", "'b'", "strtoupper($x)"), &mut folder), "'A'|'B'");
    let five = merge_src(&[("x", &["'a'", "'b'", "'c'", "'d'", "'e'"])], "strtoupper($x)");
    // Over the cap: #77's transfer still reads member-wise (casing since #240).
    assert_eq!(dump(&five, &mut folder), "non-falsy-uppercase-string");
}

/// A member that THROWS (`intdiv($n, 0)` raises `DivisionByZeroError`) declines
/// the whole fold rather than quietly dropping it, which would claim a value
/// domain the program does not have.
#[test]
fn a_throwing_member_declines_the_whole_fold() {
    let Some(mut folder) = live("a_throwing_member_declines_the_whole_fold") else { return };
    // Control: two ordinary divisors compose, so the decline below is the throw.
    let ok = "<?php\nfunction f(bool $c): void { $d = $c ? 2 : 5; \\PHPStan\\dumpType(intdiv(10, $d)); }\n";
    assert_eq!(dump(ok, &mut folder), "2|5");
    let throws = "<?php\nfunction f(bool $c): void { $d = $c ? 2 : 0; \\PHPStan\\dumpType(intdiv(10, $d)); }\n";
    let widened = dump(throws, &mut folder);
    assert_ne!(widened, "5", "the surviving member must not stand alone");
    assert_ne!(widened, "2|5", "…and the throwing member is not silently an answer");
    assert_eq!(widened, "int", "the fold declines and the reflected envelope stands");
}

// (2) The caps decline; they never truncate

/// The per-argument member cap is 4: at five members the fold declines and
/// dispatches NOTHING, since the cap is charged before any combination is built.
#[test]
fn the_member_cap_declines_rather_than_truncating() {
    let mock = Mock::default();
    let four = merge_src(&[("x", &["'a'", "'b'", "'c'", "'d'"])], "strtoupper($x)");
    assert_eq!(dump(&four, &mut mock.clone()), "'A'|'B'|'C'|'D'");

    let mock = Mock::default();
    let five = merge_src(&[("x", &["'a'", "'b'", "'c'", "'d'", "'e'"])], "strtoupper($x)");
    // `mock.count() == 0` is the property owned here; the rendered type is the
    // rung below (ADR-0069's Asserted floor, issue #79).
    assert_eq!(dump(&five, &mut mock.clone()), "uppercase-string (asserted)", "five members: no fold");
    assert_eq!(mock.count(), 0, "the cap is charged before any combination: {:?}", mock.asks());
}

/// The combination cap is 16 and caps the PRODUCT, not any single argument:
/// 3×3×3=27 (over, though each lane is inside the member cap); 2×2×4=16 folds.
#[test]
fn the_combination_cap_is_charged_on_the_product() {
    let mock = Mock::default();
    let twenty_seven = merge_src(
        &[("a", &["'a'", "'b'", "'c'"]), ("b", &["'x'", "'y'", "'z'"]), ("c", &["'aa'", "'ab'", "'ac'"])],
        "str_replace($a, $b, $c)",
    );
    // `mock.count() == 0`: the rung below renders instead (`str_replace` is
    // `string|array` in functionMap, ADR-0071).
    assert_eq!(dump(&twenty_seven, &mut mock.clone()), "string|array (asserted)", "27 > 16 declines");
    assert_eq!(mock.count(), 0, "…and dispatches nothing: {:?}", mock.asks());

    let mock = Mock::default();
    let sixteen = merge_src(
        &[("a", &["'a'", "'b'"]), ("b", &["'x'", "'y'"]), ("c", &["'aa'", "'ab'", "'ac'", "'ad'"])],
        "str_replace($a, $b, $c)",
    );
    // Sixteen answers overflow the `OneOf` CAP, so `Fact::from_vals` hands back
    // its **computed** widening (ADR-0035) — the fold still succeeding.
    assert_eq!(dump(&sixteen, &mut mock.clone()), "non-falsy-lowercase-string");
    assert_eq!(mock.count(), 16, "every combination was folded: {:?}", mock.asks());
}

/// The two-argument product at the boundary: 4×4 folds, 4×5 does not.
#[test]
fn a_two_argument_product_folds_at_sixteen_and_declines_at_twenty() {
    let mock = Mock::default();
    let four_by_four = merge_src(
        &[("s", &["'a'", "'b'", "'c'", "'d'"]), ("k", &["1", "2", "3", "4"])],
        "str_repeat($s, $k)",
    );
    assert_eq!(dump(&four_by_four, &mut mock.clone()), "non-falsy-lowercase-string");
    assert_eq!(mock.count(), 16, "4×4 = 16 folds: {:?}", mock.asks());

    let mock = Mock::default();
    let four_by_five = merge_src(
        &[("s", &["'a'", "'b'", "'c'", "'d'"]), ("k", &["1", "2", "3", "4", "5"])],
        "str_repeat($s, $k)",
    );
    assert_ne!(dump(&four_by_five, &mut mock.clone()), "non-falsy-string", "4×5 declines");
    assert_eq!(mock.count(), 0, "…and dispatches nothing: {:?}", mock.asks());
}

/// Determinism (ADR-0028 / ADR-0048): the product is enumerated arguments in
/// source order, members canonically ordered, last argument varying fastest.
#[test]
fn the_product_is_enumerated_in_a_canonical_order() {
    let mock = Mock::default();
    let src = merge_src(
        &[("s", &["'b'", "'a'"]), ("k", &["2", "1"])],
        "str_repeat($s, $k)",
    );
    assert_eq!(dump(&src, &mut mock.clone()), "'a'|'aa'|'b'|'bb'");
    let order: Vec<Vec<ArgValue>> = mock.asks().into_iter().map(|(_, args)| args).collect();
    assert_eq!(
        order,
        vec![
            vec![ArgValue::Str("a".into()), ArgValue::Int(1)],
            vec![ArgValue::Str("a".into()), ArgValue::Int(2)],
            vec![ArgValue::Str("b".into()), ArgValue::Int(1)],
            vec![ArgValue::Str("b".into()), ArgValue::Int(2)],
        ],
        "sorted members, last argument fastest — never the source's written order",
    );
}

// (3) The width gate, on both engines

/// Issue #64's width gate, reached member by member: on 64-bit both members of
/// `$c ? 1 : 3000000000` fold; on 32-bit the range guard refuses the oversized
/// one and the WHOLE fold declines (dispatch record: in-range member still asked).
#[test]
fn a_member_the_width_gate_refuses_declines_the_whole_fold() {
    const SRC: &str = "<?php\nfunction f(bool $c): void { $x = $c ? 1 : 3000000000; \\PHPStan\\dumpType(strval($x)); }\n";

    let mut wide = EngineFolder::with_engine(Fake::at_width(8));
    assert_eq!(dump(SRC, &mut wide), "'1'|'3000000000'", "64-bit: both members fold");
    assert_eq!(wide.engine_mut().dispatched.len(), 2, "both members reached the engine");

    let mut narrow = EngineFolder::with_engine(Fake::at_width(4));
    assert_ne!(dump(SRC, &mut narrow), "'1'|'3000000000'", "32-bit: the union declines");
    assert_eq!(
        narrow.engine_mut().dispatched,
        vec![("strval".to_owned(), vec![FoldArg::Int(1)])],
        "the in-range member folded; the oversized one never reached the engine",
    );
}

// (4) Stratum discipline (ADR-0048 N2)

/// Member answers are engine-`Verified`, but the composed fact takes its
/// stratum from the INPUT union (`min` clause); `(asserted)` marks that trust.
#[test]
fn the_composed_fact_takes_the_input_unions_stratum() {
    // Asserted in, asserted out: the slot fact is a docblock claim.
    assert_eq!(
        dump(&shape_read("'a'|'b'", "strtoupper($s)"), &mut Mock::default()),
        "'A'|'B' (asserted)"
    );
    // Verified in, verified out: the ternary's arms are written literals.
    assert_eq!(dump(&ternary("'a'", "'b'", "strtoupper($x)"), &mut Mock::default()), "'A'|'B'");
    // The caps are stratum-blind too: an over-wide asserted union declines the
    // fold, leaving ADR-0069's Asserted floor (issue #79) to answer instead.
    assert_eq!(
        dump(&shape_read("'a'|'b'|'c'|'d'|'e'", "strtoupper($s)"), &mut Mock::default()),
        "uppercase-string (asserted)"
    );
}

/// ADR-0052 §5: a `Verified` product whose members all agreed premises
/// `type.argument-mismatch`; the same shape over `Asserted` stays silent.
#[test]
fn a_collapsed_verified_product_premises_the_proof_layer_and_an_asserted_one_does_not() {
    const VERIFIED: &str = "<?php\nfunction takesInt(int $n): void {}\nfunction f(bool $c): void { $x = $c ? 'a' : 'A'; $y = strtoupper($x); takesInt($y); }\n";
    let fired = diagnostics(VERIFIED, &mut Mock::default());
    let mismatches: Vec<&Diagnostic> = fired.iter().filter(|d| d.id == ID).collect();
    assert_eq!(mismatches.len(), 1, "a proven value premises the proof layer: {fired:?}");
    assert_eq!(
        mismatches[0].message,
        "argument \"A\" (from $y, folded from strtoupper() over 2 argument combinations) to takesInt() cannot become int $n — proven TypeError (coercive mode)",
        "the provenance names the member-wise fold, not a single-tuple one",
    );

    const ASSERTED: &str = "<?php\nfunction takesInt(int $n): void {}\n/** @param array{k: 'a'|'A'} $v */\nfunction f(array $v): void { $s = $v['k']; $y = strtoupper($s); takesInt($y); }\n";
    let quiet = diagnostics(ASSERTED, &mut Mock::default());
    assert!(
        quiet.iter().all(|d| d.id != ID),
        "an Asserted union must not premise a proof-layer finding: {quiet:?}",
    );
}

// (5) Standing declines

/// The allowlist, shadowing rule, and argument gate decline these cases.
#[test]
fn the_standing_declines_are_unmoved() {
    let mock = Mock::default();
    // A project function shadowing the simple name is never folded, union or not.
    let shadowed = "<?php\nfunction strtoupper(string $s): string { return $s; }\nfunction f(bool $c): void { $x = $c ? 'a' : 'b'; \\PHPStan\\dumpType(strtoupper($x)); }\n";
    assert_eq!(dump(shadowed, &mut mock.clone()), "unknown");
    assert_eq!(mock.count(), 0, "a shadowed name asks nothing: {:?}", mock.asks());

    let mock = Mock::default();
    // A name outside the allowlist falls to ADR-0069's declared-return floor.
    assert_eq!(dump(&ternary("'a'", "'b'", "nl2br($x)"), &mut mock.clone()), "string (asserted)");
    assert_eq!(mock.count(), 0);

    let mock = Mock::default();
    // Neither a proven literal nor a finite union: an abstract `string` offers
    // no members to enumerate, so the fold asks nothing and the ADR-0069 floor
    // answers below it (issue #79).
    let abstract_arg = "<?php\nfunction f(string $s): void { \\PHPStan\\dumpType(strtoupper($s)); }\n";
    assert_eq!(dump(abstract_arg, &mut mock.clone()), "uppercase-string (asserted)");
    assert_eq!(mock.count(), 0);
}

/// A poisoned scope buys the union fold nothing: it needs env facts a poisoned
/// scope may not be trusted with.
#[test]
fn a_poisoned_scope_folds_no_union() {
    let mock = Mock::default();
    let src = "<?php\nfunction f(bool $c, array $a): void { extract($a); $x = $c ? 'a' : 'b'; \\PHPStan\\dumpType(strtoupper($x)); }\n";
    let _ = diagnostics(src, &mut mock.clone());
    assert_eq!(mock.count(), 0, "a poisoned scope dispatches no member: {:?}", mock.asks());
}

// (6) Zero emission, swept

/// Every fixture shape in this file, swept to confirm none emits a non-debug
/// finding — so a future row cannot quietly skip the check.
#[test]
fn no_findings_from_union_folds() {
    let mut mock = Mock::default();
    let cases: Vec<String> = vec![
        ternary("'a'", "'b'", "strtoupper($x)"),
        ternary("'a'", "'A'", "strtoupper($x)"),
        ternary("'a'", "'b'", "str_repeat($x, 2)"),
        merge_src(&[("x", &["'a'", "'b'", "'c'", "'d'"])], "strtoupper($x)"),
        merge_src(&[("x", &["'a'", "'b'", "'c'", "'d'", "'e'"])], "strtoupper($x)"),
        shape_read("'a'|'b'", "strtoupper($s)"),
        shape_read("'a'|'b'|'c'|'d'|'e'", "strtoupper($s)"),
    ];
    for src in &cases {
        let ds = diagnostics(src, &mut mock);
        let other: Vec<&Diagnostic> = ds.iter().filter(|d| !d.id.starts_with("debug.")).collect();
        assert!(other.is_empty(), "emitted {other:?} for {src}");
    }
}
