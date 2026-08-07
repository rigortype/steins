//! Issue #74 — the **member-wise union fold**: an allowlisted builtin whose
//! argument is a bounded union of constants is folded once per member
//! combination, and the answers are composed into one value fact.
//!
//! ADR-0069's amendment tabulates Steins' return ladder against PHPStan's
//! extension stack and names the one condition the fold lane could not meet: an
//! extension takes a constant *or a union of constants*, calls the real function
//! per member, and composes. `$x = $c ? 'a' : 'b'; strtoupper($x)` widened here
//! where PHPStan answers `'A'|'B'`. This file is that gap, closed and pinned.
//!
//! Four disciplines are pinned, not just the spellings:
//!
//! * **No partial unions.** Busting the per-argument member cap (4) or the
//!   combination cap (16) DECLINES; it never truncates. A union missing a member
//!   is a wrong value domain, not a wider one.
//! * **The gates are the fold lane's own gates.** Every combination goes back
//!   through `try_fold`, so the allowlist, the shadowing check, the per-argument
//!   budget, the memo and the issue-#64 integer-width gate apply once, in one
//!   place. A member the width gate refuses declines the WHOLE fold.
//! * **Stratum (ADR-0048 N2).** Each member answer is engine-`Verified`, but the
//!   input union carries its own trust: an `Asserted` union in, an `Asserted`
//!   fact out.
//! * **Zero emission from the mechanism.** A union fold is a *type*. The one
//!   finding pinned below is the ordinary proof-layer consequence of a product
//!   whose members all agreed, which is a genuine proven value.

use std::cell::RefCell;
use std::rc::Rc;

use steins_infer::{
    DEBUG_TYPE_ID, Diagnostic, EngineFolder, FoldEngine, Folder, ID, SidecarFolder, check_with,
};
use steins_sidecar::{EnvInfo, FoldArg, FoldResult, FoldValue, PregCompile, Reflection};
use steins_syntax::{ArgValue, SourceTree};

// Foldable-union source shapes

/// `$x = $c ? <a> : <b>` under an undecided guard — a two-member `OneOf` at the
/// `Verified` stratum (ADR-0031's conditional value).
fn ternary(then_val: &str, else_val: &str, expr: &str) -> String {
    format!(
        "<?php\nfunction f(bool $c): void {{ $x = $c ? {then_val} : {else_val}; \\PHPStan\\dumpType({expr}); }}\n"
    )
}

/// A branch merge: `$v` starts at `members[0]` and each `if` re-assigns it, so the
/// join at the end is an `n`-member `OneOf`, `Verified`. This is how a union wider
/// than two arms is reachable from a plain function body.
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

/// A declared array shape read: `$s = $v['k']` takes the slot's declared fact,
/// which is where an `Asserted` union comes from (ADR-0052 §9 / ADR-0062 §4).
fn shape_read(slot: &str, expr: &str) -> String {
    format!(
        "<?php\n/** @param array{{k: {slot}}} $v */\nfunction f(array $v): void {{ $s = $v['k']; \\PHPStan\\dumpType({expr}); }}\n"
    )
}

// Folders

/// One call the gate forwarded to the folder.
type Ask = (String, Vec<ArgValue>);

/// A deterministic stand-in for the engine over the handful of names these
/// fixtures use, recording every `(name, args)` the gate handed it. The counts are
/// the instrument: a decline that never dispatched and a fold that dispatched every
/// combination are different events, and only the record tells them apart.
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
    fn fold(&mut self, name: &str, args: &[ArgValue]) -> Option<ArgValue> {
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

/// A [`FoldEngine`] that answers over the arguments it is actually given — which
/// is what a union needs, since its members must get DIFFERENT answers — with the
/// integer width the test chooses. Drives the SHARED policy in `EngineFolder`, so
/// the width gate under test is the real one.
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

    fn fold(&mut self, name: &str, args: &[FoldArg]) -> FoldResult {
        self.dispatched.push((name.to_owned(), args.to_vec()));
        match (name, args) {
            ("strval", [FoldArg::Int(i)]) => FoldResult::Value(FoldValue::Str(i.to_string())),
            _ => FoldResult::widen("unmodeled by the fake"),
        }
    }

    /// The fake has no PCRE (ADR-0078): declining is what a transport that cannot
    /// answer must do, and it keeps this file's subject the fold-width lane.
    fn preg_compile(&mut self, _pattern: &str) -> Option<PregCompile> {
        None
    }
    /// No constant oracle either (issue #198): this file's subject is integer width.
    fn constant_defined(&mut self, _name: &str) -> Option<steins_sidecar::ConstantDefined> {
        None
    }
}

/// A live sidecar folder, or `None` when `php` cannot be reached — in which case
/// the caller skips loudly rather than asserting something vacuous.
fn live(test: &str) -> Option<SidecarFolder> {
    let mut folder = SidecarFolder::enabled();
    if folder.fold("strtoupper", &[ArgValue::Str("probe".into())]).is_none() {
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

/// The `dumpType` bodies for `src`, in source order — asserting on the way that a
/// union fold emitted no finding of its own (a transfer is a type, never a report).
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

/// ADR-0069's named gap, closed: the union of constants reaches the engine member
/// by member and comes back as a union. `strtoupper` is on the allowlist and its
/// answers are the engine's own — nothing here models PHP's casing.
#[test]
fn the_flagship_union_folds_through_the_real_engine() {
    let Some(mut folder) = live("the_flagship_union_folds_through_the_real_engine") else {
        return;
    };
    assert_eq!(dump(&ternary("'a'", "'b'", "strtoupper($x)"), &mut folder), "'A'|'B'");
    // The assignment form binds the same fact, so the value survives a hop.
    let assigned = "<?php\nfunction f(bool $c): void { $x = $c ? 'a' : 'b'; $y = strtoupper($x); \\PHPStan\\dumpType($y); }\n";
    assert_eq!(dump(assigned, &mut folder), "'A'|'B'");
    // A union whose members all fold to the SAME value composes to a Singleton —
    // a genuinely proven value, not a union of one.
    assert_eq!(dump(&ternary("'a'", "'A'", "strtoupper($x)"), &mut folder), "'A'");
    // The literal lane and the union lane compose in the product: a written
    // constant is the one-member case of the same ladder.
    assert_eq!(dump(&ternary("'a'", "'b'", "str_repeat($x, 2)"), &mut folder), "'aa'|'bb'");
}

/// The rung ORDER, against the neighbour that would otherwise answer. Issue #77's
/// string-predicate transfer knows `strtoupper` forces uppercase for any subject
/// at all; the union fold knows *which* uppercase strings. The value wins, and
/// where the union is too wide to enumerate the predicate still answers — so the
/// ladder degrades rather than falling to the floor.
#[test]
fn the_union_fold_outranks_the_predicate_transfer() {
    let Some(mut folder) = live("the_union_fold_outranks_the_predicate_transfer") else { return };
    assert_eq!(dump(&ternary("'a'", "'b'", "strtoupper($x)"), &mut folder), "'A'|'B'");
    let five = merge_src(&[("x", &["'a'", "'b'", "'c'", "'d'", "'e'"])], "strtoupper($x)");
    // Over the cap the ladder DEGRADES to the rung below rather than dropping to
    // the reflected envelope: issue #77 reads the same union member-wise for its
    // predicates, so `string` is still not the answer.
    assert_eq!(dump(&five, &mut folder), "non-falsy-string");
}

/// A member that THROWS declines the whole fold. `intdiv($n, 0)` raises
/// `DivisionByZeroError`, which the wire reports as a throw and the fold lane has
/// always widened on; a union that quietly dropped it would claim a value domain
/// the program does not have.
#[test]
fn a_throwing_member_declines_the_whole_fold() {
    let Some(mut folder) = live("a_throwing_member_declines_the_whole_fold") else { return };
    // The control: two ordinary divisors compose, so the decline below is the
    // throw and not the shape of the fixture.
    let ok = "<?php\nfunction f(bool $c): void { $d = $c ? 2 : 5; \\PHPStan\\dumpType(intdiv(10, $d)); }\n";
    assert_eq!(dump(ok, &mut folder), "2|5");
    let throws = "<?php\nfunction f(bool $c): void { $d = $c ? 2 : 0; \\PHPStan\\dumpType(intdiv(10, $d)); }\n";
    let widened = dump(throws, &mut folder);
    assert_ne!(widened, "5", "the surviving member must not stand alone");
    assert_ne!(widened, "2|5", "…and the throwing member is not silently an answer");
    assert_eq!(widened, "int", "the fold declines and the reflected envelope stands");
}

// (2) The caps decline; they never truncate

/// The per-argument member cap is 4. At five members the fold declines outright —
/// and, crucially, dispatches NOTHING: the cap is charged before any combination
/// is built, so an over-wide union costs no engine traffic either.
#[test]
fn the_member_cap_declines_rather_than_truncating() {
    let mock = Mock::default();
    let four = merge_src(&[("x", &["'a'", "'b'", "'c'", "'d'"])], "strtoupper($x)");
    assert_eq!(dump(&four, &mut mock.clone()), "'A'|'B'|'C'|'D'");

    let mock = Mock::default();
    let five = merge_src(&[("x", &["'a'", "'b'", "'c'", "'d'", "'e'"])], "strtoupper($x)");
    // The FOLD declines — that is what `mock.count() == 0` measures, and it is the
    // property this test owns. The rendered type is then whatever the rungs below
    // supply, and since issue #79 that is ADR-0069's Asserted floor stating
    // functionMap's `uppercase-string`. The `(asserted)` marker is how a reader
    // tells a declared claim from a folded one.
    assert_eq!(dump(&five, &mut mock.clone()), "uppercase-string (asserted)", "five members: no fold");
    assert_eq!(mock.count(), 0, "the cap is charged before any combination: {:?}", mock.asks());
}

/// The combination cap is 16, and it is a cap on the PRODUCT: three arguments of
/// three members each is 27, with every individual lane comfortably inside the
/// member cap. Two-by-two-by-four is 16 and folds.
#[test]
fn the_combination_cap_is_charged_on_the_product() {
    let mock = Mock::default();
    let twenty_seven = merge_src(
        &[("a", &["'a'", "'b'", "'c'"]), ("b", &["'x'", "'y'", "'z'"]), ("c", &["'aa'", "'ab'", "'ac'"])],
        "str_replace($a, $b, $c)",
    );
    // The FOLD declines — `mock.count() == 0` is the property this test owns. What
    // renders is the rung below: `str_replace` is `string|array` in functionMap, a
    // row ADR-0071 admitted because one of its two arms is an array, and the
    // `(asserted)` marker separates that declaration from a folded answer.
    assert_eq!(dump(&twenty_seven, &mut mock.clone()), "string|array (asserted)", "27 > 16 declines");
    assert_eq!(mock.count(), 0, "…and dispatches nothing: {:?}", mock.asks());

    let mock = Mock::default();
    let sixteen = merge_src(
        &[("a", &["'a'", "'b'"]), ("b", &["'x'", "'y'"]), ("c", &["'aa'", "'ab'", "'ac'", "'ad'"])],
        "str_replace($a, $b, $c)",
    );
    // Sixteen distinct answers overflow the value domain's own `OneOf` CAP, so
    // `Fact::from_vals` hands back its **computed** widening — the summary derived
    // by evaluating predicates on every member (ADR-0035). That is the fold
    // succeeding, and it is a strictly better answer than the declared envelope.
    assert_eq!(dump(&sixteen, &mut mock.clone()), "non-falsy-string");
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
    assert_eq!(dump(&four_by_four, &mut mock.clone()), "non-falsy-string");
    assert_eq!(mock.count(), 16, "4×4 = 16 folds: {:?}", mock.asks());

    let mock = Mock::default();
    let four_by_five = merge_src(
        &[("s", &["'a'", "'b'", "'c'", "'d'"]), ("k", &["1", "2", "3", "4", "5"])],
        "str_repeat($s, $k)",
    );
    assert_ne!(dump(&four_by_five, &mut mock.clone()), "non-falsy-string", "4×5 declines");
    assert_eq!(mock.count(), 0, "…and dispatches nothing: {:?}", mock.asks());
}

/// Determinism (ADR-0028 / ADR-0048): the product is enumerated in one fixed
/// order — arguments in source order, members in the fact's own canonical order,
/// last argument varying fastest — so the walk stays a pure function of (CST,
/// entry state, fold memo) with no map iteration anywhere in it.
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

/// Issue #64's integer-width gate, reached member by member. `strval` is on the
/// verified width-safe subset, so on a 64-bit engine both members fold and compose.
/// On a 32-bit one the argument range guard refuses the oversized member — and the
/// WHOLE fold declines with it, rather than answering the union of what survived.
///
/// The dispatch record is the proof that this is the gate and not a missing answer:
/// the in-range member IS dispatched on both engines.
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

/// Each member answer is the engine's, and so `Verified` — but the composed fact
/// consumed the INPUT union, and takes its stratum by the ordinary `min` clause.
/// A declared array shape provides a reachable `Asserted` union, and the
/// `(asserted)` marker is how the dump surface says which trust the fact carries.
#[test]
fn the_composed_fact_takes_the_input_unions_stratum() {
    // Asserted in, asserted out: the slot fact is a docblock claim.
    assert_eq!(
        dump(&shape_read("'a'|'b'", "strtoupper($s)"), &mut Mock::default()),
        "'A'|'B' (asserted)"
    );
    // Verified in, verified out: the ternary's arms are written literals.
    assert_eq!(dump(&ternary("'a'", "'b'", "strtoupper($x)"), &mut Mock::default()), "'A'|'B'");
    // …and the caps are stratum-blind: an over-wide asserted union declines the fold
    // too, leaving ADR-0069's Asserted floor to state functionMap's declaration
    // (issue #79 — the row is a refinement #73 counted and dropped). What proves the
    // fold declined is the absence of any `'A'|…` composition, not the word unknown.
    assert_eq!(
        dump(&shape_read("'a'|'b'|'c'|'d'|'e'", "strtoupper($s)"), &mut Mock::default()),
        "uppercase-string (asserted)"
    );
}

/// The proof-layer consequence, and the ADR-0052 §5 gate on it. A `Verified`
/// product whose members all agreed is a proven value and premises
/// `type.argument-mismatch`; the same shape over an `Asserted` union stays silent.
/// The provenance says what actually happened.
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
    // An argument that is neither a proven literal nor a finite union: an
    // abstract `string` envelope offers no members to enumerate, so the fold asks
    // nothing (`count() == 0`) and the ADR-0069 floor answers below it with
    // functionMap's `uppercase-string` (issue #79's widened lowering).
    let abstract_arg = "<?php\nfunction f(string $s): void { \\PHPStan\\dumpType(strtoupper($s)); }\n";
    assert_eq!(dump(abstract_arg, &mut mock.clone()), "uppercase-string (asserted)");
    assert_eq!(mock.count(), 0);
}

/// The union fold is a fold, so a poisoned scope buys it nothing: the env facts it
/// would read are exactly the ones a poisoned scope may not be trusted with.
#[test]
fn a_poisoned_scope_folds_no_union() {
    let mock = Mock::default();
    let src = "<?php\nfunction f(bool $c, array $a): void { extract($a); $x = $c ? 'a' : 'b'; \\PHPStan\\dumpType(strtoupper($x)); }\n";
    let _ = diagnostics(src, &mut mock.clone());
    assert_eq!(mock.count(), 0, "a poisoned scope dispatches no member: {:?}", mock.asks());
}

// (6) Zero emission, swept

/// Every fixture shape in this file, asserted to emit no non-debug finding. The
/// `dumps` helper already asserts it per call; this sweeps the matrix so a future
/// row cannot quietly skip the check.
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
